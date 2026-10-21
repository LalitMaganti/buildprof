// Copyright 2026 The Buildprof Authors.
// SPDX-License-Identifier: Apache-2.0

//! Recording on macOS, through the kernel trace facility.
//!
//! macOS has no unprivileged way to follow a process tree. Configuring kernel
//! tracing needs root. After that, the kernel keeps letting the process that
//! owns the session read from it. Recording therefore needs root only while it
//! starts.
//!
//! 0. Started as yourself, save the environment and run again through sudo.
//! 1. As root, set up and enable the trace session.
//! 2. Permanently drop privileges to the person who ran the recording.
//! 3. Put back the saved environment.
//! 4. Start the build, read events and write the trace as that person.
//!
//! The build never runs as root, and it sees the environment it was started
//! with. The trace file belongs to the person who ran the recording. Nothing
//! privileged is left running.

use self::event::Message;
use self::tracker::{Clock, TraceSink, Tracker};
use crate::blind_spots::{self, BlindSpots};
use crate::compiler::Capture;
use crate::perfetto::Writer;
use crate::report;
use std::ffi::{CString, OsStr, OsString};
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

mod event;
mod kdebug;
mod recon;
mod tracker;

/// Size of the kernel buffer, in events. A parallel build produces a few
/// million events a second. The buffer holds about a hundred times what the
/// recorder collects in one read, so a stall in reading does not lose events.
const BUFFER_EVENTS: usize = 512 * 1024;
/// The longest wait for the kernel to fill a batch. A short wait means a
/// process's command line is still readable when its exec is seen. A longer one
/// costs less CPU.
const READ_INTERVAL: Duration = Duration::from_millis(1);
/// How often to look for the end of a build whose last exits were not seen.
const LIVENESS_INTERVAL: Duration = Duration::from_millis(250);
/// How long to keep reading after the build, to collect trailing events.
const DRAIN: Duration = Duration::from_millis(50);

/// A trace session, owned and enabled, with privileges already dropped.
pub struct Prepared {
    session: kdebug::Session,
    clock: Clock,
    threads: Vec<(u64, i32)>,
}

/// Passes the saved environment's path to the copy of Buildprof that sudo runs.
/// sudo drops environment variables and open descriptors, so the path goes in a
/// hidden argument.
const SAVED_ENVIRONMENT_ARGUMENT: &str = "--buildprof-saved-environment=";

pub enum Start {
    Recording(Prepared),
    /// The recording ran through sudo, and finished with this status.
    Finished(ExitCode),
}

/// Starts collecting and drops privileges. It runs before the recorder creates
/// any files, so the files belong to the person who started the recording.
pub fn prepare(saved_environment: Option<&Path>) -> io::Result<Start> {
    // SAFETY: reading the process's own credentials.
    if unsafe { libc::geteuid() } != 0 {
        return escalate().map(Start::Finished);
    }
    let (uid, gid) = target_user()?;
    let session = kdebug::Session::start(BUFFER_EVENTS)?;
    let threads = session.thread_map()?;
    let clock = Clock::now();
    let started_by_sudo = std::env::var_os("SUDO_UID").is_some();
    drop_privileges(uid, gid)?;
    match saved_environment {
        Some(path) => restore_saved_environment(path, uid)?,
        None => {
            restore_user_environment(uid);
            if started_by_sudo {
                hint!(
                    "sudo reset the environment, so the build may miss variables you set, like CC \
                     or RUSTFLAGS; run {} without sudo to keep them",
                    report::emph("buildprof")
                );
            }
        }
    }
    Ok(Start::Recording(Prepared {
        session,
        clock,
        threads,
    }))
}

/// Runs this recording again as root, through sudo, with the environment it
/// was started with saved for it.
fn escalate() -> io::Result<ExitCode> {
    authorize()?;
    let saved = save_environment()?;
    let mut argument = OsString::from(SAVED_ENVIRONMENT_ARGUMENT);
    argument.push(&saved);
    // authorize() already got access, so this sudo never prompts.
    let child = Command::new("sudo")
        .args(["-n", "--"])
        .arg(std::env::current_exe()?)
        .arg(argument)
        .args(std::env::args_os().skip(1))
        .spawn();
    let status = child.and_then(|mut child| {
        // Ctrl-C reaches this process as well as the build. The build decides
        // what it means, so this process ignores it once sudo has started.
        // SAFETY: changing this process's own signal dispositions.
        unsafe {
            libc::signal(libc::SIGINT, libc::SIG_IGN);
            libc::signal(libc::SIGQUIT, libc::SIG_IGN);
        }
        child.wait()
    });
    // The recording deletes the file once it has read it. This removes it when
    // the recording stopped before that.
    let _ = fs::remove_file(&saved);
    let status = status
        .map_err(|error| io::Error::new(error.kind(), format!("could not run sudo: {error}")))?;
    Ok(ExitCode::from(exit_status_code(status)))
}

/// Makes sure sudo will start the recording without a password prompt. It asks
/// for a password only when stdin is a terminal. Without a terminal, sudo must
/// already allow access, or the recording fails at once. A refusal is reported
/// as an error, not as the build's exit status.
fn authorize() -> io::Result<()> {
    let granted = |status: io::Result<std::process::ExitStatus>| {
        status
            .map(|status| status.success())
            .map_err(|error| io::Error::new(error.kind(), format!("could not run sudo: {error}")))
    };
    let already = Command::new("sudo")
        .args(["-n", "-v"])
        .stderr(Stdio::null())
        .status();
    if granted(already)? {
        return Ok(());
    }
    if !io::stdin().is_terminal() {
        return Err(io::Error::other(
            "kernel tracing needs administrator access, and there is no terminal to ask for a \
             password; allow sudo without a password, or run in a terminal",
        ));
    }
    hint!(
        "kernel tracing needs administrator access; asking {} for your password",
        report::emph("sudo")
    );
    if !granted(Command::new("sudo").arg("-v").status())? {
        return Err(io::Error::other("sudo did not grant administrator access"));
    }
    Ok(())
}

/// Writes the environment to a file that only its owner can read, in their own
/// temporary directory.
fn save_environment() -> io::Result<PathBuf> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "buildprof-environment-{}-{nonce}",
        std::process::id()
    ));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    file.write_all(&encode_environment(std::env::vars_os()))?;
    Ok(path)
}

/// Replaces the environment with the one saved before sudo ran. It runs after
/// privileges are dropped, so root never reads the file.
fn restore_saved_environment(path: &Path, uid: u32) -> io::Result<()> {
    check_saved_environment(path, uid)?;
    let contents = fs::read(path)?;
    fs::remove_file(path)?;
    let names: Vec<OsString> = std::env::vars_os().map(|(name, _)| name).collect();
    // SAFETY: single-threaded, before the build starts.
    unsafe {
        for name in names {
            std::env::remove_var(name);
        }
        for (name, value) in decode_environment(&contents) {
            std::env::set_var(name, value);
        }
    }
    Ok(())
}

/// The saved environment must be a plain file that its owner wrote and that
/// nobody else could have read or changed.
fn check_saved_environment(path: &Path, uid: u32) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() || metadata.uid() != uid || metadata.mode() & 0o077 != 0 {
        return Err(io::Error::other(format!(
            "{} is not a private file of yours, so the environment was not restored",
            path.display()
        )));
    }
    Ok(())
}

/// Each variable as `name=value` followed by a NUL. Neither a name nor a value
/// can contain a NUL.
fn encode_environment(variables: impl IntoIterator<Item = (OsString, OsString)>) -> Vec<u8> {
    let mut contents = Vec::new();
    for (name, value) in variables {
        contents.extend_from_slice(name.as_bytes());
        contents.push(b'=');
        contents.extend_from_slice(value.as_bytes());
        contents.push(0);
    }
    contents
}

fn decode_environment(contents: &[u8]) -> Vec<(OsString, OsString)> {
    contents
        .split(|byte| *byte == 0)
        .filter_map(|entry| {
            let equals = entry.iter().position(|byte| *byte == b'=')?;
            Some((
                OsStr::from_bytes(&entry[..equals]).to_owned(),
                OsStr::from_bytes(&entry[equals + 1..]).to_owned(),
            ))
        })
        .collect()
}

pub fn record(
    prepared: Prepared,
    command: &[OsString],
    writer: &mut Writer,
    compilers: &mut Capture,
    blind_spots: &mut BlindSpots,
    file_events: bool,
) -> io::Result<u8> {
    let Prepared {
        mut session,
        clock,
        threads,
    } = prepared;
    let program = command.first().expect("validated command");

    // Compiler profiles carry wall-clock times. Both clocks start at the same
    // instant.
    compilers.set_origin(SystemTime::now());
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
    let mut collector = recon::Collector::new(
        root_pid,
        std::env::current_dir()?.to_string_lossy().into_owned(),
        clock,
        file_events,
    );
    collector.set_root_command(
        command
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect(),
    );
    collector.seed_threads(threads);

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

    keep_up(&clock);
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
            // The recording ends when the build's command and everything it
            // started have exited. Events arrive after the processes act, so
            // this asks the system whether each process still exists. Once the
            // command has been reaped, its pid can go to an unrelated process,
            // so only its descendants are checked.
            if root_status.is_some()
                && collector
                    .live_pids()
                    .filter(|pid| *pid != root_pid)
                    .all(|pid| !process_exists(pid))
            {
                ended = Some(Instant::now());
            }
        }
        // Pace the reads. A read costs about the same however much it returns,
        // so reading nonstop wastes a core. The kernel ends the wait early when
        // a batch is ready.
        let _ = session.wait(READ_INTERVAL.as_millis() as u64);
    }
    collector.flush(&mut |message| tracker.handle(message))?;
    if collector.unnamed > 0 {
        match collector.unnamed {
            1 => note!(
                "1 process ended before its command line could be read; it is named after its \
                 program alone"
            ),
            n => note!(
                "{n} processes ended before their command line could be read; they are named \
                 after their program alone"
            ),
        }
        blind_spots::see("unnamed-processes");
    }
    if collector.dropped > 0 {
        let times = match collector.dropped {
            1 => "once".to_owned(),
            n => format!("{n} times"),
        };
        note!(
            "the kernel dropped events {times} while the build ran; the trace may be missing \
             processes or files"
        );
        blind_spots::see("dropped-events");
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

/// The user to run the build as. This is whoever started the recording, never
/// root.
fn target_user() -> io::Result<(u32, u32)> {
    // SAFETY: reading the process's own credentials.
    let uid = unsafe { libc::getuid() };
    // A setuid copy of Buildprof runs the build as the real user. This is not
    // documented, and it needs nothing beyond dropping privileges.
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
            "refusing to run the build as root; start the recording as yourself, not from a \
             root shell",
        )),
    }
}

/// Gives up root for good, so the build and the trace belong to their owner.
fn drop_privileges(uid: u32, gid: u32) -> io::Result<()> {
    let name = user_name(uid);
    // SAFETY: plain credential calls, made while the recorder has one thread.
    unsafe {
        // The supplementary groups are still root's, including wheel and admin.
        // Replace them with the person's own groups. If those cannot be looked
        // up, keep only the group they record as.
        let own_groups = name
            .as_ref()
            .is_some_and(|name| libc::initgroups(name.as_ptr(), gid as i32) == 0);
        if !own_groups && libc::setgroups(1, &(gid as libc::gid_t)) != 0 {
            return Err(io::Error::last_os_error());
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
    // The per-user temporary directory depends on the current credentials. This
    // is only right after privileges are dropped.
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

    crate::model::exit_code(status.into_raw()).min(u32::from(u8::MAX)) as u8
}

fn executable_name(command: &OsString) -> String {
    Path::new(command)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| command.to_string_lossy().into_owned())
}

/// Asks the scheduler to wake the recorder on time. Ordinary threads have their
/// timers batched with others to save power. During a busy build, that
/// stretches one wait in a hundred to 10ms or more. A compiler's version check
/// can exit in that time, before its command line is read. The build has
/// already started, so it does not inherit this. If the kernel refuses, or
/// later demotes a thread that overruns its share, reading continues at normal
/// priority.
fn keep_up(clock: &Clock) {
    let ticks = |interval: Duration| (interval.as_nanos() as f64 * clock.ticks_per_ns()) as u32;
    let mut policy = libc::thread_time_constraint_policy {
        period: ticks(READ_INTERVAL),
        computation: ticks(READ_INTERVAL / 2),
        constraint: ticks(READ_INTERVAL),
        preemptible: 1,
    };
    // SAFETY: `policy` is a filled-in struct of the flavor named.
    unsafe {
        libc::thread_policy_set(
            libc::pthread_mach_thread_np(libc::pthread_self()),
            libc::THREAD_TIME_CONSTRAINT_POLICY as libc::thread_policy_flavor_t,
            (&raw mut policy).cast(),
            libc::THREAD_TIME_CONSTRAINT_POLICY_COUNT,
        );
    }
}

fn process_exists(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 || *libc::__error() != libc::ESRCH }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_saved_environment_reads_back_exactly() {
        let variables = vec![
            (OsString::from("PATH"), OsString::from("/usr/bin:/bin")),
            (
                OsString::from("RUSTFLAGS"),
                OsString::from("-C opt-level=3 -Z x=y"),
            ),
            (OsString::from("EMPTY"), OsString::new()),
            (
                OsString::from("UNICODE"),
                OsString::from("caf\u{e9} \u{1f980}"),
            ),
        ];
        assert_eq!(
            decode_environment(&encode_environment(variables.clone())),
            variables
        );
    }

    #[test]
    fn only_a_private_file_of_ours_is_restored() {
        let directory = std::env::temp_dir().join(format!(
            "buildprof-environment-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&directory).unwrap();
        let uid = unsafe { libc::getuid() };
        let private = directory.join("private");
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&private)
            .unwrap();
        assert!(check_saved_environment(&private, uid).is_ok());

        let readable = directory.join("readable");
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o644)
            .open(&readable)
            .unwrap();
        assert!(check_saved_environment(&readable, uid).is_err());

        let link = directory.join("link");
        std::os::unix::fs::symlink(&private, &link).unwrap();
        assert!(check_saved_environment(&link, uid).is_err());

        // The check sees this as someone else's file.
        assert!(check_saved_environment(&private, uid + 1).is_err());
        fs::remove_dir_all(directory).unwrap();
    }
}
