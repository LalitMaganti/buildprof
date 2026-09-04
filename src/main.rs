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
use std::net::{TcpListener, TcpStream};
use std::process::ExitCode;
use std::time::{Duration, Instant};

/// Perfetto's UI allowlists this port for its own Trace Processor RPC, so it
/// is the one place a browser may fetch a local trace from.
const HANDOFF_PORT: u16 = 9001;
const TRACE_URL: &str = "http://127.0.0.1:9001/trace";
const ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(50);
const REQUEST_READ_TIMEOUT: Duration = Duration::from_secs(5);

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
            handoff,
            wait,
        } => record(output, command, compiler_traces, handoff, wait),
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

    let mut compilers = compiler::Capture::new(compiler_traces);
    let result = linux::record(&command, &mut writer, &mut compilers);
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
    match serve_trace_once(&listener, &trace, wait.map(|wait| Instant::now() + wait)) {
        Ok(()) => {
            eprintln!("buildprof: trace handed off to the browser");
            ExitCode::SUCCESS
        }
        Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {
            eprintln!(
                "buildprof: no browser fetched the trace in time; run `buildprof open {}` to try again",
                trace.display()
            );
            ExitCode::FAILURE
        }
        Err(error) => {
            eprintln!("buildprof: trace handoff failed: {error}");
            ExitCode::FAILURE
        }
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
fn accept(listener: &TcpListener, deadline: Option<Instant>) -> std::io::Result<TcpStream> {
    let Some(deadline) = deadline else {
        return listener.accept().map(|(stream, _)| stream);
    };
    listener.set_nonblocking(true)?;
    let stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "no browser connected before the deadline",
                    ));
                }
                std::thread::sleep(ACCEPT_POLL_INTERVAL);
            }
            Err(error) => return Err(error),
        }
    };
    stream.set_nonblocking(false)?;
    Ok(stream)
}

fn serve_trace_once(
    listener: &TcpListener,
    trace: &std::path::Path,
    deadline: Option<Instant>,
) -> std::io::Result<()> {
    use std::io::{Read, Write};
    use std::net::Shutdown;

    loop {
        let mut stream = accept(listener, deadline)?;
        stream.set_read_timeout(Some(REQUEST_READ_TIMEOUT))?;
        let mut request = [0_u8; 8192];
        let mut length = 0;
        let mut complete = false;
        while length < request.len() && !complete {
            let read = match stream.read(&mut request[length..]) {
                Ok(read) => read,
                // A stalled or broken client should not end the handoff.
                Err(_) => break,
            };
            if read == 0 {
                break;
            }
            length += read;
            complete = request[..length].windows(4).any(|w| w == b"\r\n\r\n");
        }
        let request = String::from_utf8_lossy(&request[..length]);
        let request_line = request.lines().next().unwrap_or_default();
        let mut request_parts = request_line.split_whitespace();
        let method = request_parts.next().unwrap_or_default();
        let path = request_parts.next().unwrap_or_default();

        if method == "OPTIONS" {
            stream.write_all(
                b"HTTP/1.1 204 No Content\r\nAccess-Control-Allow-Origin: *\r\nAccess-Control-Allow-Methods: GET, OPTIONS\r\nConnection: close\r\n\r\n",
            )?;
            continue;
        }
        if method != "GET" || path != "/trace" {
            stream.write_all(
                b"HTTP/1.1 404 Not Found\r\nAccess-Control-Allow-Origin: *\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )?;
            continue;
        }

        let mut file = std::fs::File::open(trace)?;
        let length = file.metadata()?.len();
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {length}\r\nAccess-Control-Allow-Origin: *\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n"
        )?;
        std::io::copy(&mut file, &mut stream)?;
        stream.flush()?;
        stream.shutdown(Shutdown::Write)?;
        return Ok(());
    }
}

#[cfg(not(target_os = "linux"))]
fn record(
    _output: std::path::PathBuf,
    _command: Vec<std::ffi::OsString>,
    _compiler_traces: bool,
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
    use super::serve_trace_once;
    use std::io::{Read, Write};
    use std::time::{Duration, Instant};

    #[test]
    fn trace_server_gives_up_at_the_deadline() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let deadline = Instant::now() + Duration::from_millis(120);
        let error = serve_trace_once(&listener, std::path::Path::new("unused"), Some(deadline))
            .unwrap_err();
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
        let server = std::thread::spawn(move || serve_trace_once(&listener, &server_trace, None));

        let mut client = std::net::TcpStream::connect(address).unwrap();
        client
            .write_all(b"GET /trace HTTP/1.0\r\nHost: localhost\r\n\r\n")
            .unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).unwrap();
        server.join().unwrap().unwrap();
        std::fs::remove_file(trace).unwrap();

        let body_start = response
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .unwrap()
            + 4;
        assert_eq!(&response[body_start..], contents);
    }
}
