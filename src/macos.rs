// Copyright 2026 The Buildprof Authors.
// SPDX-License-Identifier: Apache-2.0

//! Recording on macOS, through Endpoint Security.
//!
//! macOS has no unprivileged way to follow a process tree, so recording
//! splits in two. A small helper runs as root (see `helper`) and streams the
//! build's events; this process stays unprivileged, starts the build exactly
//! as the user would, and turns the events into a trace. The build never runs
//! as root and keeps the user's environment.

use self::eslogger::Message;
use self::helper::HELPER_ARG;
use self::tracker::TraceSink;
use self::tracker::{Clock, Tracker};
use crate::blind_spots::BlindSpots;
use crate::compiler::Capture;
use crate::perfetto::Writer;
use serde_json::Value;
use std::ffi::OsString;
use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

mod eslogger;
pub mod helper;
mod tracker;

/// A root-owned setuid copy of buildprof, for people who would rather
/// install one than type a password for every recording.
const INSTALLED_HELPER: &str = "/usr/local/libexec/buildprof-es-helper";
const HELPER_ENV: &str = "BUILDPROF_ES_HELPER";
/// Copies every line from the helper to this file, to debug schema changes.
const EVENT_LOG_ENV: &str = "BUILDPROF_ES_EVENT_LOG";
/// How often to look for the end of a build whose last exits were not seen.
const LIVENESS_INTERVAL: Duration = Duration::from_millis(250);
const SIGNAL_EXIT_STATUS_OFFSET: i32 = 128;

pub fn record(
    command: &[OsString],
    writer: &mut Writer,
    compilers: &mut Capture,
    blind_spots: &mut BlindSpots,
    file_events: bool,
) -> io::Result<u8> {
    let program = command.first().expect("validated command");
    let mut helper = Helper::start(file_events)?;
    helper.wait_ready()?;

    // Compiler profiles carry wall-clock times; both origins are the same instant.
    compilers.set_origin(SystemTime::now());
    let clock = Clock::now();
    let mut build = Command::new(program)
        .args(&command[1..])
        .envs(compilers.child_environment())
        .spawn()
        .map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("could not run {}: {error}", program.to_string_lossy()),
            )
        })?;
    let root_pid = build.id() as i32;
    let mut sink = TraceSink { writer, compilers };
    let mut tracker = Tracker::new(
        &mut sink,
        blind_spots,
        clock,
        root_pid,
        executable_name(program),
        command
            .iter()
            .map(|arg| arg.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" "),
        std::env::current_dir()?.to_string_lossy().into_owned(),
    );

    let mut root_status = None;
    let mut last_check = Instant::now();
    loop {
        if let Some(Line::Event(message)) = helper.next(LIVENESS_INTERVAL)? {
            tracker.handle(message)?;
        }
        if last_check.elapsed() < LIVENESS_INTERVAL {
            continue;
        }
        last_check = Instant::now();
        if root_status.is_none() {
            root_status = build.try_wait()?;
        }
        // Like the Linux tracer, wait for everything the build started, not
        // only the command itself. Events trail the processes, so whether
        // each one still exists decides; the sync below collects the rest.
        if root_status.is_some() && tracker.live_pids().all(|pid| !process_exists(pid)) {
            break;
        }
    }

    // Take every event raised before the build ended.
    let dropped = helper.sync(|message| tracker.handle(message))?;
    if dropped > 0 {
        note!(
            "Endpoint Security dropped {dropped} events while the build ran; the trace may be \
             missing processes or files"
        );
    }
    let exit_code = tracker.root_exit_code();
    tracker.finish()?;
    helper.stop();

    let status = root_status.expect("the loop ends after the build");
    Ok(exit_code.unwrap_or_else(|| exit_status_code(status)))
}

fn exit_status_code(status: std::process::ExitStatus) -> u8 {
    match (status.code(), status.signal()) {
        (Some(code), _) => code.clamp(0, 255) as u8,
        (None, Some(signal)) => (SIGNAL_EXIT_STATUS_OFFSET + signal).min(255) as u8,
        _ => 1,
    }
}

fn executable_name(command: &OsString) -> String {
    Path::new(command)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| command.to_string_lossy().into_owned())
}

fn process_exists(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 || *libc::__error() != libc::ESRCH }
}

enum Line {
    Event(Message),
    Synced { dropped: u64 },
}

enum Output {
    Line(String),
    Closed,
}

/// The privileged helper, as seen from the recorder.
struct Helper {
    process: Child,
    commands: ChildStdin,
    lines: mpsc::Receiver<Output>,
    event_log: Option<io::BufWriter<std::fs::File>>,
}

impl Helper {
    fn start(file_events: bool) -> io::Result<Self> {
        let mut command = helper_command()?;
        if file_events {
            command.arg("--file-events");
        }
        let mut process = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|error| io::Error::other(format!("could not start the helper: {error}")))?;
        let commands = process.stdin.take().expect("piped");
        let stdout = process.stdout.take().expect("piped");
        let (sender, lines) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if sender.send(Output::Line(line)).is_err() {
                    return;
                }
            }
            let _ = sender.send(Output::Closed);
        });
        let event_log = std::env::var_os(EVENT_LOG_ENV)
            .map(|path| std::fs::File::create(path).map(io::BufWriter::new))
            .transpose()?;
        Ok(Self {
            process,
            commands,
            lines,
            event_log,
        })
    }

    /// Blocks until events flow. With sudo this includes the password prompt.
    fn wait_ready(&mut self) -> io::Result<()> {
        loop {
            match self.lines.recv() {
                Ok(Output::Line(line)) => match control(&line) {
                    Some(Control::Ready) => return Ok(()),
                    Some(Control::Error(message)) => return Err(helper_error(&message)),
                    _ => {}
                },
                Ok(Output::Closed) | Err(_) => {
                    let _ = self.process.wait();
                    return Err(io::Error::other(
                        "the Endpoint Security helper did not start; recording on macOS needs \
                         root, so run it where sudo can ask for a password",
                    ));
                }
            }
        }
    }

    /// The next line within `timeout`, if any.
    fn next(&mut self, timeout: Duration) -> io::Result<Option<Line>> {
        let line = match self.lines.recv_timeout(timeout) {
            Ok(Output::Line(line)) => line,
            Err(mpsc::RecvTimeoutError::Timeout) => return Ok(None),
            Ok(Output::Closed) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(io::Error::other(
                    "the Endpoint Security helper stopped during the recording",
                ));
            }
        };
        if let Some(log) = &mut self.event_log {
            writeln!(log, "{line}")?;
        }
        Ok(match control(&line) {
            Some(Control::Synced(dropped)) => Some(Line::Synced { dropped }),
            Some(Control::Error(message)) => return Err(helper_error(&message)),
            Some(Control::Ready) => None,
            None => eslogger::parse(&line).map(Line::Event),
        })
    }

    /// Hands every event before now to `handle`; returns the dropped count.
    fn sync(&mut self, mut handle: impl FnMut(Message) -> io::Result<()>) -> io::Result<u64> {
        writeln!(self.commands, "sync")?;
        self.commands.flush()?;
        loop {
            match self.next(Duration::from_secs(10))? {
                Some(Line::Event(message)) => handle(message)?,
                Some(Line::Synced { dropped }) => return Ok(dropped),
                None => {}
            }
        }
    }

    fn stop(self) {
        let Self {
            mut process,
            commands,
            ..
        } = self;
        drop(commands);
        let _ = process.wait();
    }
}

/// How to start the helper: an installed setuid copy if there is one,
/// directly if already root, and through sudo otherwise.
fn helper_command() -> io::Result<Command> {
    let installed = std::env::var_os(HELPER_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(INSTALLED_HELPER));
    if is_setuid_root(&installed) {
        let mut command = Command::new(installed);
        command.arg(HELPER_ARG);
        return Ok(command);
    }
    let exe = std::env::current_exe()?;
    let recorder_pid = std::process::id().to_string();
    let mut command = if unsafe { libc::geteuid() } == 0 {
        Command::new(exe)
    } else {
        note!(
            "Recording on macOS uses Endpoint Security, which needs root; sudo may ask for your password"
        );
        let mut sudo = Command::new("/usr/bin/sudo");
        sudo.arg("--").arg(exe);
        sudo
    };
    command.args([HELPER_ARG, "--root-pid", &recorder_pid]);
    Ok(command)
}

fn is_setuid_root(path: &Path) -> bool {
    std::fs::metadata(path)
        .is_ok_and(|metadata| metadata.uid() == 0 && metadata.mode() & libc::S_ISUID as u32 != 0)
}

enum Control {
    Ready,
    Synced(u64),
    Error(String),
}

fn control(line: &str) -> Option<Control> {
    if !line.starts_with(r#"{"buildprof""#) {
        return None;
    }
    let value: Value = serde_json::from_str(line).ok()?;
    match value.get("buildprof")?.as_str()? {
        "ready" => Some(Control::Ready),
        "synced" => Some(Control::Synced(
            value.get("dropped").and_then(Value::as_u64).unwrap_or(0),
        )),
        "error" => Some(Control::Error(
            value
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown error")
                .to_owned(),
        )),
        _ => None,
    }
}

/// Explains the failures people can fix themselves.
fn helper_error(message: &str) -> io::Error {
    let advice = if message.contains("ERR_NOT_PERMITTED") {
        "; give your terminal app Full Disk Access in System Settings > Privacy & Security > \
         Full Disk Access (or allow it for remote users under Sharing > Remote Login over SSH), \
         then restart the terminal"
    } else if message.contains("ERR_NOT_PRIVILEGED") {
        "; the helper must run as root"
    } else if message.contains("ERR_TOO_MANY_CLIENTS") {
        "; too many Endpoint Security clients are running, stop another eslogger"
    } else {
        ""
    };
    io::Error::other(format!("Endpoint Security: {message}{advice}"))
}
