// Copyright 2026 The Buildprof Authors.
// SPDX-License-Identifier: Apache-2.0

//! The privileged half of macOS recording.
//!
//! Endpoint Security clients must run as root, so the recorder starts this
//! helper through `sudo` (or as an installed setuid copy of the binary). It
//! runs `eslogger`, which reports every process on the machine, and passes
//! on only events from processes descended from the recorder. Everything
//! else — parsing the build's events, writing the trace, running the build
//! itself — stays unprivileged.
//!
//! Protocol, one line at a time:
//! - stdout carries `eslogger` JSON lines verbatim, plus control lines
//!   `{"buildprof":"ready"}`, `{"buildprof":"synced","dropped":N}` and
//!   `{"buildprof":"error","message":…}`.
//! - stdin takes `sync`, answered once every event raised before it has been
//!   passed on. End of input stops the helper.
//!
//! Readiness and sync both use a probe: the helper forks a child that exits
//! at once, and waits to see that fork in the stream. `eslogger` subscribes
//! to all event types together and delivers them in order, so seeing the
//! probe proves the subscription is live, or that everything before it has
//! arrived.

use super::eslogger::{Envelope, FILE_EVENTS, PROCESS_EVENTS};
use std::collections::HashSet;
use std::ffi::OsString;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::process::CommandExt;
use std::process::{Command, ExitCode, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// Versioned so a stale installed setuid helper refuses a newer recorder.
pub const HELPER_ARG: &str = "__endpoint-security-helper-v1";
const ESLOGGER: &str = "/usr/bin/eslogger";
const PROBE_INTERVAL: Duration = Duration::from_millis(50);
const READY_TIMEOUT: Duration = Duration::from_secs(15);

/// Runs the helper when this process was started as one.
///
/// Called before anything else in `main`: a setuid copy must not parse
/// arguments, read the environment, or do anything but this.
pub fn run_if_requested() -> Option<ExitCode> {
    let mut args = std::env::args_os().skip(1);
    let setuid = unsafe { libc::geteuid() != libc::getuid() };
    if args.next().as_deref() != Some(HELPER_ARG.as_ref()) {
        if setuid {
            eprintln!("buildprof: a setuid copy of buildprof only runs as the recording helper");
            return Some(ExitCode::FAILURE);
        }
        return None;
    }
    let args: Vec<OsString> = args.collect();
    let mut output = io::stdout().lock();
    match run(&args, setuid, &mut output) {
        Ok(()) => Some(ExitCode::SUCCESS),
        Err(message) => {
            let _ = control(
                &mut output,
                serde_json::json!({"buildprof": "error", "message": message}),
            );
            Some(ExitCode::FAILURE)
        }
    }
}

fn run(args: &[OsString], setuid: bool, output: &mut impl Write) -> Result<(), String> {
    let mut root_pid = None;
    let mut file_events = false;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--file-events") => file_events = true,
            Some("--root-pid") => {
                root_pid = args
                    .next()
                    .and_then(|pid| pid.to_str()?.parse::<i32>().ok())
                    .filter(|pid| *pid > 1);
                if root_pid.is_none() {
                    return Err("--root-pid needs a process id".into());
                }
            }
            _ => return Err(format!("unknown helper argument {}", arg.to_string_lossy())),
        }
    }
    if unsafe { libc::geteuid() } != 0 {
        return Err("the helper is not running as root".into());
    }
    // Only someone who is already root (through sudo) may choose whose
    // events to see. Through setuid, it is always the caller's own tree.
    let parent = unsafe { libc::getppid() };
    let root_pid = match root_pid {
        Some(pid) if !setuid => pid,
        Some(_) => return Err("--root-pid is not accepted through setuid".into()),
        None => parent,
    };
    if root_pid <= 1 {
        return Err("the helper has no parent to record".into());
    }
    if setuid && unsafe { libc::setuid(0) } != 0 {
        return Err(format!(
            "could not become root: {}",
            io::Error::last_os_error()
        ));
    }
    // The recorder's Ctrl-C reaches this process group too; stay up so the
    // recorder decides when to stop.
    unsafe { libc::signal(libc::SIGINT, libc::SIG_IGN) };

    let mut events = PROCESS_EVENTS.to_vec();
    if file_events {
        events.extend_from_slice(FILE_EVENTS);
    }
    let mut eslogger = Command::new(ESLOGGER)
        .args(&events)
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // eslogger hides events from its own process group, which would
        // otherwise include the helper's probes.
        .process_group(0)
        .spawn()
        .map_err(|error| format!("could not start {ESLOGGER}: {error}"))?;

    let (sender, receiver) = mpsc::channel();
    let stdout = eslogger.stdout.take().expect("piped");
    let lines = sender.clone();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if lines.send(Input::Event(line)).is_err() {
                return;
            }
        }
        let _ = lines.send(Input::EventsClosed);
    });
    let mut stderr = eslogger.stderr.take().expect("piped");
    let diagnostics = thread::spawn(move || {
        let mut text = String::new();
        let _ = stderr.read_to_string(&mut text);
        text
    });
    thread::spawn(move || {
        for line in io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if sender.send(Input::Command(line)).is_err() {
                return;
            }
        }
        let _ = sender.send(Input::CommandsClosed);
    });

    let result = pump(
        &receiver,
        Filter::new(root_pid, unsafe { libc::getpid() }),
        output,
    );
    let _ = eslogger.kill();
    let _ = eslogger.wait();
    match result {
        Err(Stop::EventsClosed) => {
            let diagnostics = diagnostics.join().unwrap_or_default();
            Err(format!("eslogger exited: {}", diagnostics.trim()))
        }
        Err(Stop::Failed(message)) => Err(message),
        Ok(()) => Ok(()),
    }
}

enum Input {
    Event(String),
    EventsClosed,
    Command(String),
    CommandsClosed,
}

enum Stop {
    EventsClosed,
    Failed(String),
}

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    Starting,
    Running,
    Syncing,
}

fn pump(
    receiver: &mpsc::Receiver<Input>,
    mut filter: Filter,
    output: &mut impl Write,
) -> Result<(), Stop> {
    let failed = |error: io::Error| Stop::Failed(format!("could not pass events on: {error}"));
    let started = Instant::now();
    let mut phase = Phase::Starting;
    let mut probes = HashSet::new();
    let mut last_probe: Option<Instant> = None;
    loop {
        if phase != Phase::Running
            && last_probe.is_none_or(|probe| probe.elapsed() >= PROBE_INTERVAL)
        {
            if phase == Phase::Starting && started.elapsed() > READY_TIMEOUT {
                return Err(Stop::Failed(
                    "eslogger started but delivered no events".into(),
                ));
            }
            probes.insert(probe().map_err(failed)?);
            last_probe = Some(Instant::now());
        }
        let input = match receiver.try_recv() {
            Ok(input) => input,
            Err(_) => {
                output.flush().map_err(failed)?;
                match receiver.recv_timeout(PROBE_INTERVAL) {
                    Ok(input) => input,
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
                }
            }
        };
        match input {
            Input::Event(line) => {
                let Ok(envelope) = serde_json::from_str::<Envelope>(&line) else {
                    continue;
                };
                match filter.observe(&envelope) {
                    Verdict::Skip => {}
                    Verdict::Forward => {
                        // Nothing is passed on until the recorder can tell
                        // the build's events from the startup's.
                        if phase != Phase::Starting {
                            writeln!(output, "{line}").map_err(failed)?;
                        }
                    }
                    Verdict::Probe(child) => {
                        if !probes.remove(&child) || phase == Phase::Running {
                            continue;
                        }
                        let reply = if phase == Phase::Starting {
                            filter.dropped = 0;
                            serde_json::json!({"buildprof": "ready"})
                        } else {
                            serde_json::json!({"buildprof": "synced", "dropped": filter.dropped})
                        };
                        control(output, reply).map_err(failed)?;
                        phase = Phase::Running;
                        last_probe = None;
                        // A retried startup probe must not answer a later sync.
                        probes.clear();
                    }
                }
            }
            Input::EventsClosed => return Err(Stop::EventsClosed),
            Input::Command(command) if command.trim() == "sync" => {
                phase = Phase::Syncing;
                last_probe = None;
            }
            Input::Command(_) => {}
            Input::CommandsClosed => return Ok(()),
        }
    }
}

fn control(output: &mut impl Write, value: serde_json::Value) -> io::Result<()> {
    writeln!(output, "{value}")?;
    output.flush()
}

/// Forks a child that exits immediately, returning its pid.
fn probe() -> io::Result<i32> {
    let child = unsafe { libc::fork() };
    if child < 0 {
        return Err(io::Error::last_os_error());
    }
    if child == 0 {
        unsafe { libc::_exit(0) };
    }
    let mut status = 0;
    unsafe { libc::waitpid(child, &mut status, 0) };
    Ok(child)
}

#[derive(Debug, PartialEq)]
enum Verdict {
    Skip,
    Forward,
    /// The helper itself forked this child.
    Probe(i32),
}

/// Follows the recorder's descendants through the system-wide stream.
struct Filter {
    helper_pid: i32,
    tracked: HashSet<i32>,
    next_seq: Option<u64>,
    /// Events Endpoint Security discarded because the client fell behind.
    dropped: u64,
}

impl Filter {
    fn new(root_pid: i32, helper_pid: i32) -> Self {
        Self {
            helper_pid,
            tracked: HashSet::from([root_pid]),
            next_seq: None,
            dropped: 0,
        }
    }

    fn observe(&mut self, envelope: &Envelope) -> Verdict {
        if let Some(seq) = envelope.global_seq_num {
            if let Some(expected) = self.next_seq
                && seq > expected
            {
                self.dropped += seq - expected;
            }
            self.next_seq = Some(seq + 1);
        }

        let pid = envelope.process.audit_token.pid;
        if pid == self.helper_pid {
            return match &envelope.event.fork {
                Some(fork) => Verdict::Probe(fork.child.audit_token.pid),
                None => Verdict::Skip,
            };
        }
        // A process whose fork was not seen — started with posix_spawn, or
        // before its exit was reported — is claimed through its parent.
        if !self.tracked.contains(&pid) {
            if !self.tracked.contains(&envelope.process.ppid) {
                return Verdict::Skip;
            }
            self.tracked.insert(pid);
        }
        if let Some(fork) = &envelope.event.fork {
            self.tracked.insert(fork.child.audit_token.pid);
        }
        if envelope.event.exit.is_some() {
            // Its pid may be reused by an unrelated process.
            self.tracked.remove(&pid);
        }
        Verdict::Forward
    }
}

#[cfg(test)]
mod tests {
    use super::super::eslogger::tests::{exec, exit, fork, line};
    use super::*;

    fn observe(filter: &mut Filter, line: &str) -> Verdict {
        filter.observe(&serde_json::from_str(line).unwrap())
    }

    #[test]
    fn follows_only_the_recorders_descendants() {
        let mut filter = Filter::new(100, 50);
        assert_eq!(
            observe(&mut filter, &line(100, 1, 1, 1, &fork(101))),
            Verdict::Forward
        );
        assert_eq!(
            observe(&mut filter, &line(101, 100, 2, 2, &exec(&["make"], "/"))),
            Verdict::Forward
        );
        assert_eq!(
            observe(&mut filter, &line(101, 100, 3, 3, &fork(102))),
            Verdict::Forward
        );
        assert_eq!(
            observe(&mut filter, &line(102, 101, 4, 4, &exit(0))),
            Verdict::Forward
        );
        assert_eq!(
            observe(&mut filter, &line(999, 1, 5, 5, &fork(1000))),
            Verdict::Skip,
            "unrelated processes are not passed on"
        );
        assert_eq!(
            observe(&mut filter, &line(102, 1, 6, 6, &exec(&["reused"], "/"))),
            Verdict::Skip,
            "an exited pid is forgotten"
        );
    }

    #[test]
    fn claims_spawned_children_through_their_parent() {
        let mut filter = Filter::new(100, 50);
        assert_eq!(
            observe(&mut filter, &line(103, 100, 1, 1, &exec(&["cc"], "/"))),
            Verdict::Forward
        );
        assert_eq!(
            observe(&mut filter, &line(103, 100, 2, 2, &exit(0))),
            Verdict::Forward
        );
    }

    #[test]
    fn recognizes_its_own_probes_and_counts_drops() {
        let mut filter = Filter::new(100, 50);
        assert_eq!(
            observe(&mut filter, &line(50, 40, 1, 7, &fork(51))),
            Verdict::Probe(51)
        );
        assert_eq!(
            observe(&mut filter, &line(51, 50, 1, 8, &exit(0))),
            Verdict::Skip
        );
        assert_eq!(filter.dropped, 0);
        observe(&mut filter, &line(999, 1, 1, 12, &exit(0)));
        assert_eq!(filter.dropped, 3);
    }
}
