// Copyright 2026 The Buildprof Authors.
// SPDX-License-Identifier: Apache-2.0

//! Recording on macOS, through the kernel trace facility.
//!
//! macOS has no unprivileged way to follow a process tree. Configuring kernel
//! tracing needs root, but the kernel keeps granting access to the process that
//! owns the session, so recording is privileged only while it starts:
//!
//! 1. set up and enable the trace session (root, a handful of `sysctl` calls),
//! 2. drop privileges permanently to the person who ran the recording,
//! 3. start the build, read events, and write the trace as that person.
//!
//! So the build never runs as root, the trace file belongs to its owner, and
//! nothing privileged is left running.

use self::event::Message;
use self::tracker::{Clock, TraceSink, Tracker};
use crate::blind_spots::BlindSpots;
use crate::perfetto::Writer;
use std::ffi::{CString, OsString};
use std::io;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

mod event;
mod kdebug;
mod recon;
mod tracker;

/// Kernel buffer, in events. A parallel build produces a few million events a
/// second, and the recorder reads every millisecond or so; this is room for
/// roughly a hundred times that, so a stall does not lose events.
const BUFFER_EVENTS: usize = 512 * 1024;
/// How long to wait for the kernel to fill a batch. Short enough that a
/// process's command line is still readable when its exec is seen, long enough
/// that reading does not cost a core of its own.
const READ_INTERVAL: Duration = Duration::from_millis(2);
/// How often to look for the end of a build whose last exits were not seen.
const LIVENESS_INTERVAL: Duration = Duration::from_millis(250);
/// How long to keep reading after the build, to collect trailing events.
const DRAIN: Duration = Duration::from_millis(50);
const SIGNAL_EXIT_STATUS_OFFSET: i32 = 128;

/// A trace session, owned and enabled, with privileges already dropped.
pub struct Prepared {
    session: kdebug::Session,
    clock: Clock,
    threads: Vec<(u64, i32)>,
}

/// Starts collecting and drops privileges. Everything after this runs as the
/// person who started the recording, so it happens before the recorder creates
/// files.
pub fn prepare() -> io::Result<Prepared> {
    let (uid, gid) = target_user()?;
    let session = kdebug::Session::start(BUFFER_EVENTS)?;
    let threads = session.thread_map()?;
    let clock = Clock::now();
    drop_privileges(uid, gid)?;
    restore_user_environment(uid);
    Ok(Prepared {
        session,
        clock,
        threads,
    })
}

pub fn record(
    prepared: Prepared,
    command: &[OsString],
    writer: &mut Writer,
    blind_spots: &mut BlindSpots,
) -> io::Result<u8> {
    let Prepared {
        mut session,
        clock,
        threads,
    } = prepared;
    let program = command.first().expect("validated command");

    let mut build = Command::new(program)
        .args(&command[1..])
        .spawn()
        .map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("could not run {}: {error}", program.to_string_lossy()),
            )
        })?;
    let root_pid = build.id() as i32;
    let mut collector = recon::Collector::new(root_pid, clock);
    collector.seed_threads(threads);

    let mut sink = TraceSink { writer };
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
    let mut ended: Option<Instant> = None;
    loop {
        collect(&mut session, &mut collector, &mut |message| {
            tracker.handle(message)
        })?;
        collector.tick(&mut |message| tracker.handle(message))?;
        if let Some(ended) = ended
            && ended.elapsed() > DRAIN
        {
            break;
        }
        if ended.is_none() && last_check.elapsed() >= LIVENESS_INTERVAL {
            last_check = Instant::now();
            if root_status.is_none() {
                root_status = build.try_wait()?;
            }
            // Like the Linux tracer, wait for everything the build started, not
            // only the command itself. Events trail the processes, so whether
            // each one still exists decides.
            if root_status.is_some() && collector.live_pids().all(|pid| !process_exists(pid)) {
                ended = Some(Instant::now());
            }
        }
        // Pace the reads. Each one costs about the same whatever it returns, so
        // reading as fast as possible burns a core for nothing; the kernel wakes
        // us early when a batch is waiting.
        let _ = session.wait(READ_INTERVAL.as_millis() as u64);
    }
    collector.flush(&mut |message| tracker.handle(message))?;
    if collector.unnamed > 0 {
        note!(
            "{} processes ended before their command line could be read; they are named after \
             their program alone",
            collector.unnamed
        );
    }
    if collector.dropped > 0 {
        note!(
            "The kernel dropped events {} times while the build ran; the trace may be missing \
             processes or files",
            collector.dropped
        );
    }
    let exit_code = tracker.root_exit_code();
    tracker.finish()?;

    let status = match root_status {
        Some(status) => status,
        None => build.wait()?,
    };
    Ok(exit_code.unwrap_or_else(|| exit_status_code(status)))
}

/// Reads and interprets whatever the kernel has buffered.
fn collect(
    session: &mut kdebug::Session,
    collector: &mut recon::Collector,
    out: &mut impl FnMut(Message) -> io::Result<()>,
) -> io::Result<()> {
    // Reading borrows the session's buffer, so the batch is taken first.
    let events: Vec<kdebug::Event> = session.read()?.collect();
    for event in &events {
        collector.observe(event, out)?;
    }
    Ok(())
}

/// Who to become: whoever invoked the recording, never root.
fn target_user() -> io::Result<(u32, u32)> {
    // SAFETY: reading the process's own credentials.
    let (uid, euid) = unsafe { (libc::getuid(), libc::geteuid()) };
    if euid != 0 {
        return Err(io::Error::other(
            "recording on macOS needs root to start kernel tracing; run it with sudo",
        ));
    }
    // Started from a setuid copy: become the real user. Not advertised, but it
    // needs no special handling beyond dropping to whoever ran it.
    if uid != 0 {
        return Ok((uid, unsafe { libc::getgid() }));
    }
    let number = |name: &str| {
        std::env::var(name)
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
    };
    match (number("SUDO_UID"), number("SUDO_GID")) {
        (Some(uid), Some(gid)) if uid != 0 => Ok((uid, gid)),
        _ => Err(io::Error::other(
            "refusing to run the build as root; start the recording as yourself with sudo, \
             not from a root shell",
        )),
    }
}

/// Gives up root for good, so the build and the trace belong to their owner.
fn drop_privileges(uid: u32, gid: u32) -> io::Result<()> {
    let name = user_name(uid);
    // SAFETY: plain credential calls; the recorder is single-threaded here.
    unsafe {
        if let Some(name) = &name {
            libc::initgroups(name.as_ptr(), gid as i32);
        }
        if libc::setgid(gid) != 0 {
            return Err(io::Error::last_os_error());
        }
        if libc::setuid(uid) != 0 {
            return Err(io::Error::last_os_error());
        }
        if libc::setuid(0) == 0 {
            return Err(io::Error::other("privileges could not be dropped"));
        }
    }
    Ok(())
}

/// Puts back what sudo replaced, so the build sees its own environment.
fn restore_user_environment(uid: u32) {
    let entry = unsafe { libc::getpwuid(uid) };
    if entry.is_null() {
        return;
    }
    let text = |value: *const libc::c_char| {
        (!value.is_null()).then(|| {
            unsafe { std::ffi::CStr::from_ptr(value) }
                .to_string_lossy()
                .into_owned()
        })
    };
    let (name, home, shell) = unsafe {
        (
            text((*entry).pw_name),
            text((*entry).pw_dir),
            text((*entry).pw_shell),
        )
    };
    for (key, value) in [
        ("USER", name.clone()),
        ("LOGNAME", name),
        ("HOME", std::env::var("SUDO_HOME").ok().or(home)),
        ("SHELL", shell),
    ] {
        if let Some(value) = value {
            // SAFETY: single-threaded, before the build starts.
            unsafe { std::env::set_var(key, value) };
        }
    }
    // The per-user temporary directory is chosen by the current credentials, so
    // this is only right once privileges are dropped.
    let mut buffer = [0_i8; libc::PATH_MAX as usize];
    let length = unsafe {
        libc::confstr(
            libc::_CS_DARWIN_USER_TEMP_DIR,
            buffer.as_mut_ptr(),
            buffer.len(),
        )
    };
    if length > 1 {
        let bytes: Vec<u8> = buffer[..length - 1]
            .iter()
            .map(|byte| *byte as u8)
            .collect();
        if let Ok(path) = String::from_utf8(bytes) {
            unsafe { std::env::set_var("TMPDIR", path) };
        }
    }
}

fn user_name(uid: u32) -> Option<CString> {
    let entry = unsafe { libc::getpwuid(uid) };
    if entry.is_null() {
        return None;
    }
    let name = unsafe { (*entry).pw_name };
    (!name.is_null()).then(|| unsafe { std::ffi::CStr::from_ptr(name) }.to_owned())
}

fn exit_status_code(status: std::process::ExitStatus) -> u8 {
    use std::os::unix::process::ExitStatusExt;

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
