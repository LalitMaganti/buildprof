mod args;
#[cfg(target_os = "linux")]
mod compiler;
#[cfg(target_os = "linux")]
mod linux;
// The trace model and writer are portable; only recording is Linux-specific.
// Building them everywhere keeps the writer's unit tests running on every
// platform the viewer ships on.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod model;
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod perfetto;

use args::{Handoff, Source, Wait};
use std::net::TcpListener;
use std::process::ExitCode;
use std::time::Instant;
use tiny_http::{Header, Method, Response, Server};

/// Perfetto's UI allowlists this port for its own Trace Processor RPC, so it
/// is the one place a browser may fetch a local trace from.
const HANDOFF_PORT: u16 = 9001;
const TRACE_URL: &str = "http://127.0.0.1:9001/trace";

fn main() -> ExitCode {
    #[cfg(target_os = "linux")]
    if let Some(code) = compiler::run_wrapper() {
        return code;
    }

    let args = args::parse();
    match args {
        args::Args::Record {
            output,
            command,
            compiler_traces,
            file_events,
            handoff,
            wait,
        } => record(output, command, compiler_traces, file_events, handoff, wait),
        args::Args::Open {
            source,
            url,
            handoff,
            wait,
        } => open_in_ui(&source, &url, handoff, wait),
        args::Args::Examples => list_examples(),
    }
}

fn list_examples() -> ExitCode {
    let width = args::EXAMPLES
        .iter()
        .map(|example| example.name.len())
        .max()
        .unwrap_or_default();
    for example in args::EXAMPLES {
        println!("{:width$}  {}", example.name, example.description);
        println!("{:width$}  buildprof open --example {}", "", example.name);
    }
    ExitCode::SUCCESS
}

#[cfg(target_os = "linux")]
fn record(
    output: std::path::PathBuf,
    command: Vec<std::ffi::OsString>,
    compiler_traces: bool,
    file_events: bool,
    handoff: Option<Handoff>,
    wait: Wait,
) -> ExitCode {
    let mut writer = match perfetto::Writer::create(&output) {
        Ok(writer) => writer,
        Err(error) => {
            eprintln!("buildprof: could not write {}: {error}", output.display());
            return ExitCode::FAILURE;
        }
    };

    if let Err(error) = writer.collection_options(file_events, compiler_traces) {
        eprintln!("buildprof: could not write recording options: {error}");
        return ExitCode::FAILURE;
    }
    let mut compilers = compiler::Capture::new(compiler_traces);
    let result = linux::record(&command, &mut writer, &mut compilers, file_events);
    let write_result = writer.finish();
    let exit_code = match result {
        Ok(exit_code) => exit_code,
        Err(error) => {
            eprintln!("buildprof: recording failed: {error}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(error) = write_result {
        eprintln!("buildprof: could not write {}: {error}", output.display());
        return ExitCode::FAILURE;
    }
    eprintln!("buildprof: wrote {}", output.display());
    match handoff {
        Some(handoff) => {
            let _ = open_in_ui(&Source::Trace(output), args::DEFAULT_UI_URL, handoff, wait);
        }
        None => eprintln!(
            "buildprof: open {} and choose {}",
            args::DEFAULT_UI_URL,
            output.display()
        ),
    }
    ExitCode::from(exit_code)
}

fn open_in_ui(source: &Source, ui_url: &str, handoff: Handoff, wait: Wait) -> ExitCode {
    let ui_url = ui_url.trim_end_matches('/');
    let trace = match source {
        Source::Example(example) => {
            let url = format!("{ui_url}/#!/?url={}/{}", args::EXAMPLES_URL, example.file);
            eprintln!("buildprof: opening the {} example", example.name);
            present(&url, handoff);
            return ExitCode::SUCCESS;
        }
        Source::Trace(trace) => trace,
    };

    let Ok(trace) = trace.canonicalize() else {
        eprintln!("buildprof: could not resolve {}", trace.display());
        return ExitCode::FAILURE;
    };
    let listener = match TcpListener::bind(("127.0.0.1", HANDOFF_PORT)) {
        Ok(listener) => listener,
        Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
            eprintln!(
                "buildprof: port {HANDOFF_PORT} is already in use, probably by another buildprof \
                 still waiting for a browser or by a Perfetto `trace_processor --httpd`"
            );
            eprintln!(
                "buildprof: stop it and retry, or open {ui_url} and choose {}",
                trace.display()
            );
            return ExitCode::FAILURE;
        }
        Err(error) => {
            eprintln!("buildprof: could not start the trace handoff server: {error}");
            return ExitCode::FAILURE;
        }
    };

    let url = format!("{ui_url}/#!/?url={TRACE_URL}");
    eprintln!("buildprof: serving {} at {TRACE_URL}", trace.display());
    present(&url, handoff);
    match wait {
        Some(wait) => eprintln!(
            "buildprof: waiting up to {}s for the browser to download the trace (Ctrl-C to stop)",
            wait.as_secs()
        ),
        None => {
            eprintln!("buildprof: waiting for the browser to download the trace (Ctrl-C to stop)")
        }
    }
    let site = site_origin(ui_url);
    eprintln!(
        "buildprof: if Chrome asks to let {site} access other apps and services on this \
         device, allow it; that is the page fetching the trace from this machine"
    );
    match serve_trace_once(listener, &trace, wait.map(|wait| Instant::now() + wait)) {
        Ok(()) => {
            eprintln!("buildprof: trace handed off to the browser");
            ExitCode::SUCCESS
        }
        Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {
            eprintln!(
                "buildprof: no browser fetched the trace in time; run `buildprof open {}` to try again",
                trace.display()
            );
            eprintln!(
                "buildprof: if you blocked that for {site}, allow it again in the site settings \
                 first: \"Apps on device\" in Chrome, \"Access this device\" in Firefox"
            );
            ExitCode::FAILURE
        }
        Err(error) => {
            eprintln!("buildprof: trace handoff failed: {error}");
            ExitCode::FAILURE
        }
    }
}

/// The scheme and host of `url`, which is how the browser names the site in
/// its permission prompts.
fn site_origin(url: &str) -> &str {
    let host_start = url.find("://").map_or(0, |index| index + 3);
    match url[host_start..].find('/') {
        Some(path_start) => &url[..host_start + path_start],
        None => url,
    }
}

/// Gets `url` in front of the user: launched here, or spelled out when the
/// browser has to run on another machine.
fn present(url: &str, handoff: Handoff) {
    match handoff {
        Handoff::Browser => {
            eprintln!("buildprof: opening {url}");
            if launch_browser(url).is_err() {
                eprintln!("buildprof: could not launch a browser; open that URL yourself");
            }
        }
        Handoff::Ssh => {
            eprintln!(
                "buildprof: this is an SSH session, so the browser has to run on your own machine"
            );
            if url.contains(TRACE_URL) {
                eprintln!("buildprof: from there, forward the trace port:");
                eprintln!(
                    "buildprof:     ssh -L {HANDOFF_PORT}:127.0.0.1:{HANDOFF_PORT} {}",
                    ssh_target()
                );
                eprintln!("buildprof: then open:");
            } else {
                eprintln!("buildprof: open:");
            }
            eprintln!("buildprof:     {url}");
        }
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

/// Blocks until a client connects, or until `deadline` passes.
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

#[cfg(not(target_os = "linux"))]
fn record(
    _output: std::path::PathBuf,
    _command: Vec<std::ffi::OsString>,
    _compiler_traces: bool,
    _file_events: bool,
    _handoff: Option<Handoff>,
    _wait: Wait,
) -> ExitCode {
    eprintln!("buildprof: recording needs Linux; this build can only view traces");
    eprintln!(
        "buildprof: record on a Linux machine, copy the trace here, and run `buildprof open <TRACE>`"
    );
    ExitCode::FAILURE
}

#[cfg(test)]
mod tests {
    use super::{serve_trace_once, site_origin};
    use std::io::{Read, Write};
    use std::time::{Duration, Instant};

    #[test]
    fn site_origin_drops_the_path() {
        assert_eq!(
            site_origin("https://buildprof.lalitm.com/v0.2.3"),
            "https://buildprof.lalitm.com"
        );
        assert_eq!(
            site_origin("http://localhost:10000"),
            "http://localhost:10000"
        );
    }

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
