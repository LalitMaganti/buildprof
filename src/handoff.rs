// Copyright 2026 The Buildprof Authors.
// SPDX-License-Identifier: Apache-2.0

//! Getting a trace into the browser: served once over loopback for the
//! hosted UI to fetch, launched here or spelled out for an SSH reader.

use crate::args::{self, Handoff, Source, Wait};
use crate::{report, util};
use std::net::TcpListener;
use std::process::ExitCode;
use std::time::Instant;
use tiny_http::{Header, Method, Response, Server};

/// Perfetto's UI allowlists this port for its own Trace Processor RPC, so it
/// is the one place a browser may fetch a local trace from.
const HANDOFF_PORT: u16 = 9001;
const TRACE_URL: &str = "http://127.0.0.1:9001/trace";

/// `recorded` says the trace was just written by this run, which changes
/// how the report opens.
pub fn open_in_ui(
    source: &Source,
    ui_url: &str,
    handoff: Handoff,
    wait: Wait,
    recorded: bool,
) -> ExitCode {
    let ui_url = ui_url.trim_end_matches('/');
    let trace = match source {
        Source::Example(example) => {
            let url = format!("{ui_url}/#!/?url={}/{}", args::EXAMPLES_URL, example.file);
            match handoff {
                Handoff::Browser => {
                    head!("Opening the {} example in your browser", example.name);
                    launch(&url);
                }
                Handoff::Ssh => {
                    head!("Open the {} example from your own machine:", example.name);
                    report::gap();
                    detail!("{url}");
                    report::gap();
                }
            }
            return ExitCode::SUCCESS;
        }
        Source::Trace(trace) => trace,
    };

    let Ok(served) = trace.canonicalize() else {
        error!("could not resolve {}", report::emph(trace.display()));
        return ExitCode::FAILURE;
    };
    let listener = match TcpListener::bind(("127.0.0.1", HANDOFF_PORT)) {
        Ok(listener) => listener,
        Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
            error!(
                "port {HANDOFF_PORT} is already in use, probably by another buildprof still \
                 waiting for a browser or by a Perfetto `trace_processor --httpd`"
            );
            hint!(
                "stop it and retry, or open {} and choose {}",
                report::emph(ui_url),
                report::emph(trace.display())
            );
            return ExitCode::FAILURE;
        }
        Err(error) => {
            error!("could not start the trace handoff server: {error}");
            return ExitCode::FAILURE;
        }
    };

    let url = format!("{ui_url}/#!/?url={TRACE_URL}");
    let path = report::emph(trace.display());
    match handoff {
        Handoff::Browser => {
            if recorded {
                head!("Recorded {path}, opening it in your browser");
            } else {
                head!("Opening {path} in your browser");
            }
            launch(&url);
        }
        Handoff::Ssh => {
            if recorded {
                head!("Recorded {path}");
            } else {
                head!("Serving {path}");
            }
            report::gap();
            line!("This is an SSH session. From your own machine, forward the trace port:");
            report::gap();
            detail!(
                "ssh -L {HANDOFF_PORT}:127.0.0.1:{HANDOFF_PORT} {}",
                ssh_target()
            );
            report::gap();
            line!("Then open the UI there:");
            report::gap();
            detail!("{url}");
            report::gap();
        }
    }
    line!(
        "If the browser asks to access other apps and services on this device, allow it; that \
         is the page fetching the trace."
    );
    report::gap();
    match wait {
        Some(wait) => line!(
            "Waiting up to {} (Ctrl-C to stop)",
            util::humanize_duration(wait)
        ),
        None => line!("Waiting for the browser (Ctrl-C to stop)"),
    }
    match serve_trace_once(listener, &served, wait.map(|wait| Instant::now() + wait)) {
        Ok(()) => {
            line!("Handed off to the browser");
            ExitCode::SUCCESS
        }
        Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {
            report::gap();
            error!(
                "no browser fetched the trace; run {} to try again",
                report::emph(format_args!("buildprof open {}", trace.display()))
            );
            hint!(
                "if you blocked the browser's prompt for {}, allow it again in the site \
                 settings: \"Apps on device\" in Chrome, \"Access this device\" in Firefox",
                util::site_origin(ui_url)
            );
            ExitCode::FAILURE
        }
        Err(error) => {
            error!("trace handoff failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn launch(url: &str) {
    if launch_browser(url).is_err() {
        note!("Could not launch a browser; open {}", report::emph(url));
    }
}

fn launch_browser(url: &str) -> std::io::Result<()> {
    use std::process::Command;

    #[cfg(target_os = "macos")]
    let browser = Command::new("open").arg(url).spawn();
    #[cfg(target_os = "linux")]
    let browser = Command::new("xdg-open").arg(url).spawn();
    #[cfg(target_os = "windows")]
    let browser = Command::new("cmd").args(["/C", "start", "", url]).spawn();
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    let browser: std::io::Result<std::process::Child> = Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "browser launching is unsupported on this platform",
    ));
    browser.map(drop)
}

/// Best guess at the `user@host` an SSH client would use to reach this machine.
fn ssh_target() -> String {
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_else(|_| "<user>".to_owned());
    let host = hostname().unwrap_or_else(|| "<host>".to_owned());
    format!("{user}@{host}")
}

#[cfg(unix)]
fn hostname() -> Option<String> {
    let mut buffer = [0_u8; 256];
    let result = unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) };
    if result != 0 {
        return None;
    }
    let length = buffer.iter().position(|byte| *byte == 0)?;
    String::from_utf8(buffer[..length].to_vec()).ok()
}

#[cfg(not(unix))]
fn hostname() -> Option<String> {
    std::env::var("COMPUTERNAME").ok()
}

fn header(name: &str, value: &str) -> Header {
    Header::from_bytes(name, value).expect("constant header is well formed")
}

/// Answer requests until the trace has been sent once. The page at the UI
/// origin fetches it cross-origin, so every reply allows any origin and the
/// preflight is answered too.
fn serve_trace_once(
    listener: TcpListener,
    trace: &std::path::Path,
    deadline: Option<Instant>,
) -> std::io::Result<()> {
    let server = Server::from_listener(listener, None).map_err(std::io::Error::other)?;
    loop {
        let request = match deadline {
            None => server.recv()?,
            Some(deadline) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                match server.recv_timeout(remaining)? {
                    Some(request) => request,
                    None => {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            "no browser connected before the deadline",
                        ));
                    }
                }
            }
        };
        let allow_origin = header("Access-Control-Allow-Origin", "*");
        if *request.method() == Method::Options {
            request.respond(
                Response::empty(204)
                    .with_header(allow_origin)
                    .with_header(header("Access-Control-Allow-Methods", "GET, OPTIONS")),
            )?;
            continue;
        }
        if *request.method() != Method::Get || request.url() != "/trace" {
            request.respond(Response::empty(404).with_header(allow_origin))?;
            continue;
        }
        let response = Response::from_file(std::fs::File::open(trace)?)
            .with_header(allow_origin)
            .with_header(header("Content-Type", "application/octet-stream"))
            .with_header(header("Cache-Control", "no-store"));
        request.respond(response)?;
        return Ok(());
    }
}

#[cfg(test)]
mod tests {
    use super::serve_trace_once;
    use std::io::{Read, Write};
    use std::time::{Duration, Instant};

    #[test]
    fn trace_server_gives_up_at_the_deadline() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let deadline = Instant::now() + Duration::from_millis(120);
        let error =
            serve_trace_once(listener, std::path::Path::new("unused"), Some(deadline)).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert!(Instant::now() >= deadline);
    }

    #[test]
    fn trace_server_writes_the_complete_response_before_returning() {
        let trace = std::env::temp_dir().join(format!(
            "buildprof-open-test-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("unnamed")
        ));
        let contents = vec![0x5a; 256 * 1024];
        std::fs::write(&trace, &contents).unwrap();

        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server_trace = trace.clone();
        let server = std::thread::spawn(move || serve_trace_once(listener, &server_trace, None));

        let mut client = std::net::TcpStream::connect(address).unwrap();
        client
            .write_all(b"GET /trace HTTP/1.0\r\nHost: localhost\r\n\r\n")
            .unwrap();
        // The process exits as soon as the handoff returns, so everything
        // must already be on the wire by then.
        server.join().unwrap().unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).unwrap();
        std::fs::remove_file(trace).unwrap();

        let body_start = response
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .unwrap()
            + 4;
        assert_eq!(&response[body_start..], contents);
        let head = String::from_utf8_lossy(&response[..body_start]);
        assert!(head.starts_with("HTTP/1.0 200 OK\r\n") || head.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(head.contains("Access-Control-Allow-Origin: *\r\n"));
        assert!(head.contains(&format!("Content-Length: {}\r\n", contents.len())));
    }

    #[test]
    fn trace_server_answers_preflight_and_other_paths_without_finishing() {
        let trace = std::env::temp_dir().join(format!(
            "buildprof-open-preflight-test-{}",
            std::process::id()
        ));
        std::fs::write(&trace, b"trace").unwrap();
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let server_trace = trace.clone();
        let server = std::thread::spawn(move || serve_trace_once(listener, &server_trace, None));

        let exchange = |request: &[u8]| {
            let mut client = std::net::TcpStream::connect(address).unwrap();
            client.write_all(request).unwrap();
            let mut response = Vec::new();
            client.read_to_end(&mut response).unwrap();
            String::from_utf8_lossy(&response).into_owned()
        };
        let preflight =
            exchange(b"OPTIONS /trace HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
        assert!(preflight.starts_with("HTTP/1.1 204 "), "{preflight}");
        assert!(preflight.contains("Access-Control-Allow-Origin: *\r\n"));
        assert!(preflight.contains("Access-Control-Allow-Methods: GET, OPTIONS\r\n"));
        let missing =
            exchange(b"GET /other HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
        assert!(missing.starts_with("HTTP/1.1 404 "), "{missing}");
        assert!(
            !server.is_finished(),
            "the server gave up before serving the trace"
        );

        let served =
            exchange(b"GET /trace HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
        assert!(served.ends_with("\r\n\r\ntrace"), "{served}");
        server.join().unwrap().unwrap();
        std::fs::remove_file(trace).unwrap();
    }
}
