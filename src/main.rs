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

use std::process::ExitCode;

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
            open,
        } => record(output, command, compiler_traces, open),
        args::Args::Open { trace, url } => open_in_ui(&trace, &url),
    }
}

#[cfg(target_os = "linux")]
fn record(
    output: std::path::PathBuf,
    command: Vec<std::ffi::OsString>,
    compiler_traces: bool,
    open: bool,
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
    if open {
        let _ = open_in_ui(&output, args::DEFAULT_UI_URL);
    } else {
        eprintln!(
            "buildprof: open https://buildprof.lalitm.com and choose {}",
            output.display()
        );
    }
    ExitCode::from(exit_code)
}

fn open_in_ui(output: &std::path::Path, ui_url: &str) -> ExitCode {
    use std::process::Command;

    let Ok(output) = output.canonicalize() else {
        eprintln!(
            "buildprof: could not resolve {} for --open",
            output.display()
        );
        return ExitCode::FAILURE;
    };
    let listener = match std::net::TcpListener::bind(("127.0.0.1", 9001)) {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("buildprof: could not start the trace handoff server: {error}");
            return ExitCode::FAILURE;
        }
    };
    let trace_url = "http://127.0.0.1:9001/trace";
    let url = format!("{}/#!/?url={trace_url}", ui_url.trim_end_matches('/'),);
    eprintln!("buildprof: serving trace at {trace_url}");
    eprintln!("buildprof: opening {url}");
    #[cfg(target_os = "macos")]
    let browser = Command::new("open").arg(&url).spawn();
    #[cfg(target_os = "linux")]
    let browser = Command::new("xdg-open").arg(&url).spawn();
    #[cfg(target_os = "windows")]
    let browser = Command::new("cmd").args(["/C", "start", "", &url]).spawn();
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    let browser: std::io::Result<std::process::Child> = Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "browser launching is unsupported on this platform",
    ));

    if browser.is_err() {
        eprintln!("buildprof: could not launch a browser; open that URL manually");
        return ExitCode::FAILURE;
    }

    eprintln!("buildprof: waiting for the browser to download the trace");
    match serve_trace_once(&listener, &output) {
        Ok(()) => {
            eprintln!("buildprof: trace handed off to the browser");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("buildprof: trace handoff failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn serve_trace_once(
    listener: &std::net::TcpListener,
    trace: &std::path::Path,
) -> std::io::Result<()> {
    use std::io::{Read, Write};
    use std::net::Shutdown;

    loop {
        let (mut stream, _) = listener.accept()?;
        let mut request = [0_u8; 8192];
        let mut length = 0;
        while length < request.len() && !request[..length].windows(4).any(|w| w == b"\r\n\r\n") {
            let read = stream.read(&mut request[length..])?;
            if read == 0 {
                break;
            }
            length += read;
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
    _open: bool,
) -> ExitCode {
    eprintln!("buildprof: recording is currently supported only on Linux");
    ExitCode::FAILURE
}

#[cfg(test)]
mod tests {
    use super::serve_trace_once;
    use std::io::{Read, Write};

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
        let server = std::thread::spawn(move || serve_trace_once(&listener, &server_trace));

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
