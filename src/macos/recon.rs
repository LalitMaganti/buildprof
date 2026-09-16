// Copyright 2026 The Buildprof Authors.
// SPDX-License-Identifier: Apache-2.0

//! Turns kernel trace events into what the build did.
//! The kernel reports every process on the machine. This follows only the ones
//! the build started. A process belongs to the build if the process that
//! created it does.
//!
//! The kernel reports each path as the program passed it. A path can be
//! relative to a working directory or to a directory descriptor. It can also
//! restart at the target of a symbolic link. This therefore keeps each
//! process's working directory and the paths behind its open descriptors.
//!
//! Command lines are not in the trace. They are read from the process itself as
//! soon as its execution appears.

use super::event::{Event, Message, linux_open_flags};
use super::kdebug;
use super::tracker::Clock;
use std::collections::HashMap;
use std::io;
use std::time::{Duration, SystemTime};

// Process lifecycle, always traced.
const NEWTHREAD: u32 = 0x0700_0004;
const DATA_EXEC: u32 = 0x0700_0008;
const STRING_EXEC: u32 = 0x0701_0008;
const LOST_EVENTS: u32 = 0x0702_0008;
const PROC_EXIT: u32 = 0x0401_0004;
const VFS_LOOKUP: u32 = 0x0301_0090;

// The syscalls whose paths, descriptors and results are followed.
const OPEN: u32 = 0x040c_0014;
const OPEN_NOCANCEL: u32 = 0x040c_0638;
const OPEN_EXTENDED: u32 = 0x040c_0454;
const OPEN_DPROTECTED: u32 = 0x040c_0360;
const GUARDED_OPEN: u32 = 0x040c_06e4;
const GUARDED_OPEN_DPROTECTED: u32 = 0x040c_0790;
const OPENAT: u32 = 0x040c_073c;
const OPENAT_NOCANCEL: u32 = 0x040c_0740;
const OPENAT_DPROTECTED: u32 = 0x040c_0368;
const CHDIR: u32 = 0x040c_0030;
const FCHDIR: u32 = 0x040c_0034;
const CLOSE: u32 = 0x040c_0018;
const CLOSE_NOCANCEL: u32 = 0x040c_063c;
const GUARDED_CLOSE: u32 = 0x040c_06e8;
const DUP: u32 = 0x040c_00a4;
const DUP2: u32 = 0x040c_0168;
const FCNTL: u32 = 0x040c_0170;
const FCNTL_NOCANCEL: u32 = 0x040c_0658;

const AT_FDCWD: i32 = -2;
const O_CLOEXEC: u64 = 0x0100_0000;
const O_DIRECTORY: u64 = 0x0010_0000;
const F_DUPFD: u64 = 0;
const F_SETFD: u64 = 2;
const F_DUPFD_CLOEXEC: u64 = 67;

/// How long an exit is held before it is believed, in trace time. The kernel
/// also reports an exit when a process starts a new program. Anything further
/// from the pid cancels the exit.
const EXIT_HOLD_NS: u64 = 10_000_000;
/// How long, in trace time after a program starts, to keep trying to read its
/// command line. A read can land before the new program is ready.
const ARGV_RETRY_NS: u64 = 50_000_000;
/// How much later than its exec a process may claim to have started before it
/// is taken to be a different process that reused the pid.
const REUSE_TOLERANCE: Duration = Duration::from_millis(2);

#[derive(Clone, Default)]
struct Process {
    cwd: Option<String>,
    /// Descriptor to (path, close-on-exec).
    fds: HashMap<i32, (Option<String>, bool)>,
}

struct Call {
    code: u32,
    args: [u64; 4],
    lookups: Vec<String>,
}

/// What a process reports about itself. This is when it started, its command
/// line, and the file id of the program it is running now.
pub struct Inspection {
    pub started: SystemTime,
    pub executable: String,
    pub argv: Vec<String>,
    pub image: u64,
}

fn inspect(pid: i32, buffer: &mut Vec<u8>) -> io::Result<Inspection> {
    let started = kdebug::start_time(pid)?;
    let (executable, argv) = kdebug::command_line(pid, buffer)?;
    // Read after the arguments, so a program started in between is noticed.
    let image = kdebug::image(pid)?;
    Ok(Inspection {
        started,
        executable,
        argv,
        image,
    })
}

/// Reads a process's command line and program.
type Inspect = Box<dyn FnMut(i32, &mut Vec<u8>) -> io::Result<Inspection>>;

impl Unread {
    /// What the process was running instead, if it had moved on.
    fn moved_on(&self) -> Option<(String, Vec<String>)> {
        match self {
            Self::MovedOn(executable, argv) => Some((executable.clone(), argv.clone())),
            _ => None,
        }
    }
}

/// Why a command line could not be used.
enum Unread {
    /// Not readable yet. The new program may still be starting.
    NotYet,
    /// The process has exited, or its pid belongs to another process now.
    Gone,
    /// The process has started another program since, which has this
    /// executable and these arguments.
    MovedOn(String, Vec<String>),
}

/// A program whose command line is still to be read. Once reading is given up,
/// it waits only for the kernel to name the program.
struct PendingArgv {
    pid: i32,
    mach_time: u64,
    /// The file id the kernel reported for the program, or zero.
    image: u64,
    give_up: bool,
    /// What the process was running instead, when it had started another
    /// program by the time it was read.
    moved_on: Option<(String, Vec<String>)>,
    /// Whether the kernel's name for this program has arrived.
    named: bool,
}

struct HeldExit {
    pid: i32,
    mach_time: u64,
    stat: i32,
    believe_after: u64,
}

pub struct Collector {
    root: i32,
    clock: Clock,
    processes: HashMap<i32, Process>,
    threads: HashMap<u64, i32>,
    lookups: HashMap<u64, Vec<u64>>,
    calls: HashMap<u64, Call>,
    names: HashMap<i32, String>,
    held_exits: Vec<HeldExit>,
    argv_retries: Vec<PendingArgv>,
    /// The command Buildprof started the build with, until the build's first
    /// program is seen.
    root_command: Option<Vec<String>>,
    /// Reads a process's command line and program. Tests replace it.
    inspect: Inspect,
    /// The time of the newest event seen. Retries measure their window against
    /// it.
    latest: u64,
    argv_buffer: Vec<u8>,
    file_events: bool,
    /// Events the kernel dropped because the buffer filled.
    pub dropped: u64,
    /// Processes whose command line was gone before it could be read. They are
    /// named after their program alone.
    pub unnamed: u64,
}

impl Collector {
    pub fn new(root: i32, root_cwd: String, clock: Clock, file_events: bool) -> Self {
        Self {
            root,
            clock,
            processes: HashMap::from([(
                root,
                Process {
                    cwd: Some(normalize(&root_cwd)),
                    fds: HashMap::new(),
                },
            )]),
            threads: HashMap::new(),
            lookups: HashMap::new(),
            calls: HashMap::new(),
            names: HashMap::new(),
            held_exits: Vec::new(),
            argv_retries: Vec::new(),
            root_command: None,
            inspect: Box::new(inspect),
            latest: 0,
            argv_buffer: vec![0; 1 << 18],
            file_events,
            dropped: 0,
            unnamed: 0,
        }
    }

    /// Gives the command Buildprof started the build with. The build's first
    /// program then needs no read, which fails for a command that exits at once.
    pub fn set_root_command(&mut self, argv: Vec<String>) {
        self.root_command = Some(argv);
    }

    /// Threads that existed before tracing started.
    pub fn seed_threads(&mut self, map: impl IntoIterator<Item = (u64, i32)>) {
        self.threads.extend(map);
    }

    pub fn observe(
        &mut self,
        event: &kdebug::Event,
        out: &mut impl FnMut(Message) -> io::Result<()>,
    ) -> io::Result<()> {
        let code = event.code();
        let args = event.args;
        let thread = event.thread;
        self.latest = self.latest.max(event.timestamp);

        match code {
            NEWTHREAD => {
                let (child_thread, pid) = (args[0], args[1] as i32);
                let parent = self.threads.get(&thread).copied();
                self.threads.insert(child_thread, pid);
                if let Some(parent) = parent
                    && parent != pid
                {
                    match self.processes.get(&parent).cloned() {
                        // A new process inherits its parent's directory and descriptors.
                        Some(state) => {
                            self.cancel_exit(pid);
                            self.processes.insert(pid, state);
                            out(Message {
                                mach_time: event.timestamp,
                                pid: parent,
                                ppid: 0,
                                event: Event::Fork { child: pid },
                            })?;
                        }
                        // Another process now has this pid, so stop following
                        // it.
                        None if pid != self.root => {
                            self.processes.remove(&pid);
                        }
                        None => {}
                    }
                }
                return Ok(());
            }
            LOST_EVENTS => {
                self.dropped += 1;
                return Ok(());
            }
            _ => {}
        }

        let Some(&pid) = self.threads.get(&thread) else {
            return Ok(());
        };
        // Anything further from a pid means its exit was a new program
        // starting. The process has not ended.
        self.cancel_exit(pid);
        self.believe_exits(event.timestamp, out)?;
        if !self.processes.contains_key(&pid) {
            return Ok(());
        }

        match code {
            STRING_EXEC => {
                self.names.insert(pid, decode_string(&args));
                for pending in self
                    .argv_retries
                    .iter_mut()
                    .filter(|pending| pending.pid == pid)
                {
                    pending.named = true;
                }
                return Ok(());
            }
            // The kernel reports the new program's file id as the third argument.
            DATA_EXEC => return self.exec(pid, event.timestamp, args[2], out),
            PROC_EXIT if event.is_end() => {
                self.held_exits.push(HeldExit {
                    pid,
                    mach_time: event.timestamp,
                    stat: args[1] as i32,
                    believe_after: event.timestamp + self.ticks(EXIT_HOLD_NS),
                });
                return Ok(());
            }
            VFS_LOOKUP => {
                let words = self.lookups.entry(thread).or_default();
                if event.is_start() {
                    words.clear();
                    words.extend_from_slice(&args[1..]);
                } else {
                    words.extend_from_slice(&args);
                }
                if event.is_end() {
                    let path = decode_string(&std::mem::take(words));
                    if let Some(call) = self.calls.get_mut(&thread) {
                        call.lookups.push(path);
                    }
                }
                return Ok(());
            }
            _ => {}
        }

        if !is_followed(code) {
            return Ok(());
        }
        if event.is_start() {
            self.calls.insert(
                thread,
                Call {
                    code,
                    args,
                    lookups: Vec::new(),
                },
            );
            return Ok(());
        }
        if !event.is_end() {
            return Ok(());
        }
        let Some(call) = self.calls.remove(&thread) else {
            return Ok(());
        };
        // The first argument of a syscall's end is its errno.
        if call.code != code {
            return Ok(());
        }
        if args[0] != 0 {
            return Ok(());
        }
        let returned = args[1] as i64;
        self.finish_call(pid, event.timestamp, &call, returned, out)
    }

    /// Follows up the command lines that were not ready yet.
    pub fn tick(&mut self, out: &mut impl FnMut(Message) -> io::Result<()>) -> io::Result<()> {
        self.retry_command_lines(out, false)
    }

    /// Emits everything still held back, at the end of a recording.
    pub fn flush(&mut self, out: &mut impl FnMut(Message) -> io::Result<()>) -> io::Result<()> {
        // A process's execution has to reach the trace before its exit ends it.
        // The last reads therefore go first.
        self.retry_command_lines(out, true)?;
        for exit in std::mem::take(&mut self.held_exits) {
            self.emit_exit(exit, out)?;
        }
        Ok(())
    }

    /// Pids the build started that have not been seen to exit.
    pub fn live_pids(&self) -> impl Iterator<Item = i32> + '_ {
        self.processes.keys().copied()
    }

    fn ticks(&self, nanoseconds: u64) -> u64 {
        (nanoseconds as f64 * self.clock.ticks_per_ns()) as u64
    }

    /// Reads the command line of the program a process started at `mach_time`,
    /// whose file id the kernel reported as `image`. A pid that started later
    /// belongs to another process now. A process running a different program
    /// has started another since. Neither command line is this program's.
    fn command_line(
        &mut self,
        pid: i32,
        mach_time: u64,
        image: u64,
    ) -> Result<(String, Vec<String>), Unread> {
        let inspection = match (self.inspect)(pid, &mut self.argv_buffer) {
            Ok(inspection) => inspection,
            Err(error) if error.raw_os_error() == Some(libc::ESRCH) => return Err(Unread::Gone),
            Err(_) => return Err(Unread::NotYet),
        };
        if inspection.started > self.clock.wall(mach_time) + REUSE_TOLERANCE {
            return Err(Unread::Gone);
        }
        // The kernel reports zero for a file id that does not fit.
        if image != 0 && inspection.image != image {
            return Err(Unread::MovedOn(inspection.executable, inspection.argv));
        }
        Ok((inspection.executable, inspection.argv))
    }

    fn cancel_exit(&mut self, pid: i32) {
        self.held_exits.retain(|exit| exit.pid != pid);
    }

    fn believe_exits(
        &mut self,
        now: u64,
        out: &mut impl FnMut(Message) -> io::Result<()>,
    ) -> io::Result<()> {
        // An exit waits for its process's command line. The execution then
        // reaches the trace first, even if only named after its program.
        while let Some(index) = self.held_exits.iter().position(|exit| {
            exit.believe_after <= now
                && !self
                    .argv_retries
                    .iter()
                    .any(|pending| pending.pid == exit.pid)
        }) {
            let exit = self.held_exits.remove(index);
            self.emit_exit(exit, out)?;
        }
        Ok(())
    }

    fn emit_exit(
        &mut self,
        exit: HeldExit,
        out: &mut impl FnMut(Message) -> io::Result<()>,
    ) -> io::Result<()> {
        if exit.pid != self.root {
            self.processes.remove(&exit.pid);
        }
        self.names.remove(&exit.pid);
        out(Message {
            mach_time: exit.mach_time,
            pid: exit.pid,
            ppid: 0,
            event: Event::Exit { stat: exit.stat },
        })
    }

    fn exec(
        &mut self,
        pid: i32,
        mach_time: u64,
        image: u64,
        out: &mut impl FnMut(Message) -> io::Result<()>,
    ) -> io::Result<()> {
        if let Some(state) = self.processes.get_mut(&pid) {
            state.fds.retain(|_, (_, close_on_exec)| !*close_on_exec);
        }
        // An earlier program whose command line is still unread can no longer
        // be read, because the process has started another one since. It is
        // named after its own program before the kernel names this one.
        if let Some(index) = self
            .argv_retries
            .iter()
            .position(|pending| pending.pid == pid)
        {
            let earlier = self.argv_retries.remove(index);
            self.settle(earlier, out)?;
        }
        if pid == self.root
            && let Some(argv) = self.root_command.take()
        {
            let executable = argv.first().cloned().unwrap_or_default();
            return self.emit_exec(pid, mach_time, executable, argv, out);
        }
        match self.command_line(pid, mach_time, image) {
            Ok((executable, argv)) => self.emit_exec(pid, mach_time, executable, argv, out),
            // Come back to it even to give up. The kernel names the program in
            // the event after this one.
            Err(unread) => {
                self.argv_retries.push(PendingArgv {
                    pid,
                    mach_time,
                    image,
                    give_up: !matches!(unread, Unread::NotYet),
                    moved_on: unread.moved_on(),
                    named: false,
                });
                Ok(())
            }
        }
    }

    fn retry_command_lines(
        &mut self,
        out: &mut impl FnMut(Message) -> io::Result<()>,
        last_chance: bool,
    ) -> io::Result<()> {
        for pending in std::mem::take(&mut self.argv_retries) {
            let read = if pending.give_up {
                Err(Unread::Gone)
            } else {
                self.command_line(pending.pid, pending.mach_time, pending.image)
            };
            let deadline = pending.mach_time + self.ticks(ARGV_RETRY_NS);
            let waiting = !last_chance && self.latest < deadline;
            match read {
                Ok((executable, argv)) => {
                    self.emit_exec(pending.pid, pending.mach_time, executable, argv, out)?
                }
                // Read again while the window lasts. A read that was given up
                // on still waits for the kernel to name the program.
                Err(unread) if waiting && (matches!(unread, Unread::NotYet) || !pending.named) => {
                    self.argv_retries.push(PendingArgv {
                        give_up: !matches!(unread, Unread::NotYet),
                        moved_on: pending.moved_on.or(unread.moved_on()),
                        ..pending
                    });
                }
                Err(unread) => {
                    let moved_on = pending.moved_on.or(unread.moved_on());
                    self.settle(
                        PendingArgv {
                            moved_on,
                            ..pending
                        },
                        out,
                    )?
                }
            }
        }
        Ok(())
    }

    /// Reports a program whose own command line could not be read. A launcher
    /// may start another program in the same process and pass its own arguments
    /// on unchanged. /bin/sh does this when it starts bash. Such a launcher
    /// keeps those arguments. A launcher whose successor has other arguments is
    /// named after its program.
    fn settle(
        &mut self,
        pending: PendingArgv,
        out: &mut impl FnMut(Message) -> io::Result<()>,
    ) -> io::Result<()> {
        match pending.moved_on {
            Some((executable, argv)) if self.passed_on(pending.pid, &argv) => {
                self.emit_exec(pending.pid, pending.mach_time, executable, argv, out)
            }
            Some(_) => self.emit_named_exec(pending.pid, pending.mach_time, false, out),
            None => self.emit_named_exec(pending.pid, pending.mach_time, true, out),
        }
    }

    /// Whether `argv` starts with the name the kernel gave the program that
    /// `pid` started. A launcher's own arguments look like this once it has
    /// passed them on.
    fn passed_on(&self, pid: i32, argv: &[String]) -> bool {
        let Some(name) = self.names.get(&pid).filter(|name| !name.is_empty()) else {
            return false;
        };
        let first = argv.first().map_or("", |arg| {
            arg.rsplit('/')
                .next()
                .unwrap_or(arg)
                .trim_start_matches('-')
        });
        // The kernel cuts long names short.
        first == name || (name.len() >= 15 && first.starts_with(name.as_str()))
    }

    /// Reports an execution under the name the kernel gave it, without its
    /// arguments. They count as `lost` unless the process started another
    /// program that has them.
    fn emit_named_exec(
        &mut self,
        pid: i32,
        mach_time: u64,
        lost: bool,
        out: &mut impl FnMut(Message) -> io::Result<()>,
    ) -> io::Result<()> {
        if lost {
            self.unnamed += 1;
        }
        let executable = self.names.get(&pid).cloned().unwrap_or_default();
        let argv = vec![executable.clone()];
        self.emit_exec(pid, mach_time, executable, argv, out)
    }

    fn emit_exec(
        &mut self,
        pid: i32,
        mach_time: u64,
        executable: String,
        argv: Vec<String>,
        out: &mut impl FnMut(Message) -> io::Result<()>,
    ) -> io::Result<()> {
        let cwd = self
            .processes
            .get(&pid)
            .and_then(|state| state.cwd.clone())
            .unwrap_or_default();
        out(Message {
            mach_time,
            pid,
            ppid: 0,
            event: Event::Exec {
                argv,
                cwd,
                executable,
            },
        })
    }

    fn finish_call(
        &mut self,
        pid: i32,
        mach_time: u64,
        call: &Call,
        returned: i64,
        out: &mut impl FnMut(Message) -> io::Result<()>,
    ) -> io::Result<()> {
        let code = call.code;
        if is_open(code) {
            let at = matches!(code, OPENAT | OPENAT_NOCANCEL | OPENAT_DPROTECTED);
            let flags = match code {
                GUARDED_OPEN | GUARDED_OPEN_DPROTECTED => call.args[3],
                _ if at => call.args[2],
                _ => call.args[1],
            };
            let directory = at.then(|| call.args[0]).filter(|fd| *fd as i32 != AT_FDCWD);
            let path = match call.lookups.first() {
                Some(raw) => self.resolve(pid, raw, directory),
                // Opening "/" is the one open the kernel logs no lookup for.
                None if flags & O_DIRECTORY != 0 => Some("/".to_owned()),
                None => return Ok(()),
            };
            if let Some(state) = self.processes.get_mut(&pid) {
                state
                    .fds
                    .insert(returned as i32, (path.clone(), flags & O_CLOEXEC != 0));
            }
            if let Some(path) = path
                && self.file_events
            {
                out(Message {
                    mach_time,
                    pid,
                    ppid: 0,
                    event: Event::Open {
                        path,
                        flags: linux_open_flags(flags),
                        fd: returned as i32,
                    },
                })?;
            }
            return Ok(());
        }
        match code {
            CHDIR => {
                if let Some(raw) = call.lookups.first().cloned() {
                    let resolved = self.resolve(pid, &raw, None);
                    if let Some(state) = self.processes.get_mut(&pid) {
                        state.cwd = resolved;
                    }
                }
            }
            FCHDIR => {
                let path = self.fd_path(pid, call.args[0]);
                if let Some(state) = self.processes.get_mut(&pid) {
                    state.cwd = path;
                }
            }
            CLOSE | CLOSE_NOCANCEL | GUARDED_CLOSE => {
                if let Some(state) = self.processes.get_mut(&pid) {
                    state.fds.remove(&(call.args[0] as i32));
                }
            }
            DUP | DUP2 | FCNTL | FCNTL_NOCANCEL => {
                let (from, to, close_on_exec) = match code {
                    DUP => (call.args[0], returned, false),
                    DUP2 => (call.args[0], call.args[1] as i64, false),
                    _ => match call.args[1] {
                        F_DUPFD => (call.args[0], returned, false),
                        F_DUPFD_CLOEXEC => (call.args[0], returned, true),
                        F_SETFD => {
                            if let Some(state) = self.processes.get_mut(&pid)
                                && let Some(entry) = state.fds.get_mut(&(call.args[0] as i32))
                            {
                                entry.1 = call.args[2] & 1 != 0;
                            }
                            return Ok(());
                        }
                        _ => return Ok(()),
                    },
                };
                let path = self.fd_path(pid, from);
                if let Some(state) = self.processes.get_mut(&pid) {
                    state.fds.insert(to as i32, (path, close_on_exec));
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn fd_path(&self, pid: i32, fd: u64) -> Option<String> {
        self.processes
            .get(&pid)?
            .fds
            .get(&(fd as i32))
            .and_then(|(path, _)| path.clone())
    }

    /// Resolves a path as the kernel reported it.
    fn resolve(&self, pid: i32, raw: &str, directory: Option<u64>) -> Option<String> {
        if raw.starts_with('/') {
            return Some(normalize(raw));
        }
        let base = match directory {
            Some(fd) => self.fd_path(pid, fd)?,
            None => self.processes.get(&pid)?.cwd.clone()?,
        };
        Some(normalize(&format!("{base}/{raw}")))
    }
}

fn is_open(code: u32) -> bool {
    matches!(
        code,
        OPEN | OPEN_NOCANCEL
            | OPEN_EXTENDED
            | OPEN_DPROTECTED
            | GUARDED_OPEN
            | GUARDED_OPEN_DPROTECTED
            | OPENAT
            | OPENAT_NOCANCEL
            | OPENAT_DPROTECTED
    )
}

fn is_followed(code: u32) -> bool {
    is_open(code)
        || matches!(
            code,
            CHDIR
                | FCHDIR
                | CLOSE
                | CLOSE_NOCANCEL
                | GUARDED_CLOSE
                | DUP
                | DUP2
                | FCNTL
                | FCNTL_NOCANCEL
        )
}

/// Paths arrive as little-endian words packed with the bytes of the name.
fn decode_string(words: &[u64]) -> String {
    let mut bytes = Vec::with_capacity(words.len() * 8);
    for word in words {
        bytes.extend_from_slice(&word.to_le_bytes());
    }
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

/// Resolves `.` and `..` textually. It also maps the data volume's firmlinked
/// spelling to the path it names.
fn normalize(path: &str) -> String {
    let path = path
        .strip_prefix("/System/Volumes/Data")
        .filter(|rest| rest.starts_with('/'))
        .unwrap_or(path);
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    let mut resolved = String::with_capacity(path.len() + 1);
    for part in parts {
        resolved.push('/');
        resolved.push_str(part);
    }
    if resolved.is_empty() {
        resolved.push('/');
    }
    resolved
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds the trace events a syscall produces, for tests.
    struct Stream {
        events: Vec<kdebug::Event>,
        time: u64,
    }

    impl Stream {
        fn new() -> Self {
            Self {
                events: Vec::new(),
                time: 1_000,
            }
        }

        fn push(&mut self, thread: u64, debugid: u32, args: [u64; 4]) -> &mut Self {
            self.time += 10;
            self.events.push(kdebug::Event {
                timestamp: self.time,
                args,
                thread,
                debugid,
            });
            self
        }

        fn lookup(&mut self, thread: u64, vnode: u64, path: &str) -> &mut Self {
            let mut words = vec![vnode];
            let mut bytes = path.as_bytes().to_vec();
            bytes.push(0);
            while !bytes.len().is_multiple_of(8) {
                bytes.push(0);
            }
            words.extend(
                bytes
                    .chunks(8)
                    .map(|chunk| u64::from_le_bytes(chunk.try_into().expect("8 bytes"))),
            );
            // A lookup is one start record and then continuations.
            let first: [u64; 4] = [
                words[0],
                words[1],
                *words.get(2).unwrap_or(&0),
                *words.get(3).unwrap_or(&0),
            ];
            if words.len() <= 4 {
                self.push(thread, VFS_LOOKUP | 3, first)
            } else {
                self.push(thread, VFS_LOOKUP | 1, first);
                let rest = &words[4..];
                for (index, chunk) in rest.chunks(4).enumerate() {
                    let mut args = [0; 4];
                    args[..chunk.len()].copy_from_slice(chunk);
                    let last = (index + 1) * 4 >= rest.len();
                    self.push(thread, VFS_LOOKUP | if last { 2 } else { 0 }, args);
                }
                self
            }
        }

        fn collect(&self, collector: &mut Collector) -> Vec<Message> {
            let mut out = Vec::new();
            for event in &self.events {
                collector
                    .observe(event, &mut |message| {
                        out.push(message);
                        Ok(())
                    })
                    .expect("observe");
            }
            collector
                .flush(&mut |message| {
                    out.push(message);
                    Ok(())
                })
                .expect("flush");
            out
        }
    }

    /// An event the collector ignores, for moving time on.
    const OTHER_EVENT: u32 = 0x0409_0000;

    fn collector() -> Collector {
        let clock = Clock {
            origin: 0,
            numer: 1,
            denom: 1,
            origin_wall: std::time::SystemTime::now(),
        };
        let mut collector = Collector::new(100, "/src".into(), clock, true);
        collector.seed_threads([(10, 100)]);
        collector
    }

    #[test]
    fn resolves_a_relative_open_against_the_working_directory() {
        let mut stream = Stream::new();
        stream
            .push(10, OPEN | 1, [0, 0x601, 0o644, 0])
            .lookup(10, 0x1111, "out/a.o")
            .push(10, OPEN | 2, [0, 5, 0, 0]);
        let messages = stream.collect(&mut collector());
        assert_eq!(
            messages,
            [Message {
                mach_time: 1_030,
                pid: 100,
                ppid: 0,
                event: Event::Open {
                    path: "/src/out/a.o".into(),
                    // O_WRONLY | O_CREAT | O_TRUNC
                    flags: 0o1101,
                    fd: 5,
                },
            }]
        );
    }

    #[test]
    fn the_build_command_is_known_without_reading_it() {
        // `true` exits before anything can read it.
        let mut stream = Stream::new();
        stream
            .push(10, DATA_EXEC, [100, 0, 0, 0])
            .push(10, STRING_EXEC, program_name("true"))
            .push(10, DATA_EXEC, [100, 0, 0, 0])
            .push(10, STRING_EXEC, program_name("sh"));
        let mut collector = collector();
        collector.set_root_command(vec!["true".into(), "--flag".into()]);
        let messages = stream.collect(&mut collector);
        // Only the first program is the command Buildprof started.
        assert_eq!(
            command_lines(&messages, 100),
            [vec!["true".to_owned(), "--flag".into()], vec!["sh".into()]]
        );
        assert_eq!(collector.unnamed, 1);
    }

    #[test]
    fn follows_chdir_and_directory_descriptors() {
        let mut stream = Stream::new();
        // chdir("sub"), then openat(fd of "/src/dir", "b.o").
        stream
            .push(10, CHDIR | 1, [0, 0, 0, 0])
            .lookup(10, 0x2222, "sub")
            .push(10, CHDIR | 2, [0, 0, 0, 0])
            .push(10, OPEN | 1, [0, O_DIRECTORY, 0, 0])
            .lookup(10, 0x3333, "/src/dir")
            .push(10, OPEN | 2, [0, 7, 0, 0])
            .push(10, OPENAT | 1, [7, 0, 0, 0])
            .lookup(10, 0x4444, "b.o")
            .push(10, OPENAT | 2, [0, 8, 0, 0])
            // A plain open resolves against the new working directory.
            .push(10, OPEN | 1, [0, 0x601, 0, 0])
            .lookup(10, 0x9999, "c.o")
            .push(10, OPEN | 2, [0, 9, 0, 0]);
        let messages = stream.collect(&mut collector());
        let paths: Vec<_> = messages
            .iter()
            .filter_map(|message| match &message.event {
                Event::Open { path, .. } => Some(path.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(paths, ["/src/dir", "/src/dir/b.o", "/src/sub/c.o"]);
    }

    #[test]
    fn a_new_process_inherits_and_is_reported() {
        let mut stream = Stream::new();
        stream
            .push(10, NEWTHREAD, [11, 200, 0, 0])
            .push(11, OPEN | 1, [0, 0, 0, 0])
            .lookup(11, 0x6666, "child.txt")
            .push(11, OPEN | 2, [0, 3, 0, 0]);
        let messages = stream.collect(&mut collector());
        assert!(matches!(messages[0].event, Event::Fork { child: 200 }));
        assert_eq!(messages[0].pid, 100);
        assert!(matches!(
            &messages[1].event,
            Event::Open { path, .. } if path == "/src/child.txt"
        ));
        assert_eq!(messages[1].pid, 200);
    }

    #[test]
    fn an_exit_waits_for_its_process_command_line() {
        // A child whose command line cannot be read exits, and its exit is due
        // long before the recording ends.
        let child = -2_i32;
        let mut name = [0_u64; 4];
        name[0] = u64::from_le_bytes(*b"sh\0\0\0\0\0\0");
        let mut stream = Stream::new();
        stream
            .push(10, NEWTHREAD, [11, child as u64, 0, 0])
            .push(11, DATA_EXEC, [child as u64, 0, 0, 0])
            .push(11, STRING_EXEC, name)
            .push(11, PROC_EXIT | 2, [child as u64, 0, 0, 0]);
        stream.time += 2 * EXIT_HOLD_NS;
        stream.push(10, OTHER_EVENT, [0; 4]);
        let messages = stream.collect(&mut collector());
        let child_events: Vec<_> = messages
            .iter()
            .filter(|message| message.pid == child)
            .map(|message| &message.event)
            .collect();
        // Its execution still reaches the trace before its exit, named after
        // its program.
        assert_eq!(
            child_events,
            [
                &Event::Exec {
                    argv: vec!["sh".into()],
                    cwd: "/src".into(),
                    executable: "sh".into(),
                },
                &Event::Exit { stat: 0 },
            ]
        );
    }

    /// The words the kernel's name event carries for `name`.
    fn program_name(name: &str) -> [u64; 4] {
        let mut bytes = name.as_bytes().to_vec();
        bytes.resize(32, 0);
        let mut words = [0; 4];
        for (word, chunk) in words.iter_mut().zip(bytes.chunks(8)) {
            *word = u64::from_le_bytes(chunk.try_into().expect("8 bytes"));
        }
        words
    }

    fn command_lines(messages: &[Message], pid: i32) -> Vec<Vec<String>> {
        messages
            .iter()
            .filter(|message| message.pid == pid)
            .filter_map(|message| match &message.event {
                Event::Exec { argv, .. } => Some(argv.clone()),
                _ => None,
            })
            .collect()
    }

    fn running(argv: &[&str], image: u64) -> io::Result<Inspection> {
        Ok(Inspection {
            started: SystemTime::UNIX_EPOCH,
            executable: argv[0].into(),
            argv: argv.iter().map(|arg| (*arg).to_owned()).collect(),
            image,
        })
    }

    /// Feeds `stream` to `collector` and reads pending command lines after each
    /// event. The recorder does the same after each batch.
    fn collect_ticking(stream: &Stream, collector: &mut Collector) -> Vec<Message> {
        let mut out = Vec::new();
        for event in &stream.events {
            let mut push = |message| {
                out.push(message);
                Ok(())
            };
            collector.observe(event, &mut push).expect("observe");
            collector.tick(&mut push).expect("tick");
        }
        out
    }

    #[test]
    fn a_process_that_starts_another_program_keeps_its_own_name() {
        // /usr/bin/cc is a shim that runs the real clang in the same process,
        // so by the time cc's command line is read, clang is running.
        const CC: u64 = 0x1111;
        const CLANG: u64 = 0x2222;
        let mut collector = collector();
        collector.inspect = Box::new(|_, _| running(&["clang", "-c", "a.c"], CLANG));
        let mut stream = Stream::new();
        stream
            .push(10, DATA_EXEC, [100, 0, CC, 0])
            .push(10, STRING_EXEC, program_name("cc"))
            .push(10, DATA_EXEC, [100, 0, CLANG, 0])
            .push(10, STRING_EXEC, program_name("clang"));
        let messages = stream.collect(&mut collector);
        assert_eq!(
            command_lines(&messages, 100),
            [
                vec!["cc".to_owned()],
                vec!["clang".into(), "-c".into(), "a.c".into()]
            ]
        );
        assert_eq!(collector.unnamed, 0);
    }

    #[test]
    fn a_launcher_that_passes_its_arguments_on_keeps_them() {
        // /bin/sh starts bash in the same process straight away, with its own
        // arguments, so by the time sh is read, bash has them.
        const SH: u64 = 0x3333;
        const BASH: u64 = 0x4444;
        let mut collector = collector();
        collector.inspect = Box::new(|_, _| running(&["/bin/sh", "-c", "true"], BASH));
        let mut stream = Stream::new();
        stream
            .push(10, DATA_EXEC, [100, 0, SH, 0])
            .push(10, STRING_EXEC, program_name("sh"))
            .push(10, DATA_EXEC, [100, 0, BASH, 0])
            .push(10, STRING_EXEC, program_name("bash"));
        let messages = stream.collect(&mut collector);
        let sh = vec!["/bin/sh".to_owned(), "-c".into(), "true".into()];
        assert_eq!(command_lines(&messages, 100), [sh.clone(), sh]);
        assert_eq!(collector.unnamed, 0);
    }

    #[test]
    fn a_command_line_is_retried_for_a_while_not_a_number_of_times() {
        // Unreadable seven times, all within the window.
        let mut reads = 0;
        let mut collector = collector();
        collector.inspect = Box::new(move |_, _| {
            reads += 1;
            if reads <= 7 {
                Err(io::Error::from_raw_os_error(libc::EINVAL))
            } else {
                running(&["cc", "-c", "a.c"], 0)
            }
        });
        let mut stream = Stream::new();
        stream
            .push(10, DATA_EXEC, [100, 0, 0, 0])
            .push(10, STRING_EXEC, program_name("cc"));
        for _ in 0..8 {
            stream.push(10, OTHER_EVENT, [0; 4]);
        }
        let messages = collect_ticking(&stream, &mut collector);
        assert_eq!(
            command_lines(&messages, 100),
            [vec!["cc".to_owned(), "-c".into(), "a.c".into()]]
        );
        assert_eq!(collector.unnamed, 0);
    }

    #[test]
    fn a_command_line_is_given_up_once_its_window_has_passed() {
        let mut collector = collector();
        collector.inspect = Box::new(|_, _| Err(io::Error::from_raw_os_error(libc::EINVAL)));
        let mut stream = Stream::new();
        stream
            .push(10, DATA_EXEC, [100, 0, 0, 0])
            .push(10, STRING_EXEC, program_name("cc"))
            .push(10, OTHER_EVENT, [0; 4]);
        let early = collect_ticking(&stream, &mut collector);
        assert!(command_lines(&early, 100).is_empty(), "{early:?}");
        let mut later = Stream::new();
        later.time = stream.time + ARGV_RETRY_NS;
        later.push(10, OTHER_EVENT, [0; 4]);
        let messages = collect_ticking(&later, &mut collector);
        assert_eq!(command_lines(&messages, 100), [vec!["cc".to_owned()]]);
        assert_eq!(collector.unnamed, 1);
    }

    #[test]
    fn a_process_already_gone_is_not_read_again() {
        let reads = std::rc::Rc::new(std::cell::Cell::new(0));
        let counted = reads.clone();
        let mut collector = collector();
        collector.inspect = Box::new(move |_, _| {
            counted.set(counted.get() + 1);
            Err(io::Error::from_raw_os_error(libc::ESRCH))
        });
        let mut stream = Stream::new();
        stream
            .push(10, DATA_EXEC, [100, 0, 0, 0])
            .push(10, STRING_EXEC, program_name("cc"));
        for _ in 0..5 {
            stream.push(10, OTHER_EVENT, [0; 4]);
        }
        let messages = collect_ticking(&stream, &mut collector);
        assert_eq!(command_lines(&messages, 100), [vec!["cc".to_owned()]]);
        assert_eq!(reads.get(), 1);
    }

    #[test]
    fn an_exit_is_only_believed_when_nothing_follows_it() {
        // The kernel reports an exit when an image is replaced, too.
        let mut stream = Stream::new();
        stream
            .push(10, PROC_EXIT | 2, [100, 0, 0, 0])
            .push(10, OPEN | 1, [0, 0, 0, 0])
            .lookup(10, 0x7777, "still-here.txt")
            .push(10, OPEN | 2, [0, 3, 0, 0]);
        let messages = stream.collect(&mut collector());
        assert!(
            !messages
                .iter()
                .any(|message| matches!(message.event, Event::Exit { .. })),
            "{messages:?}"
        );

        // With nothing after it, the exit stands, with its wait status.
        let mut stream = Stream::new();
        stream.push(10, PROC_EXIT | 2, [100, 256, 0, 0]);
        let messages = stream.collect(&mut collector());
        assert_eq!(
            messages,
            [Message {
                mach_time: 1_010,
                pid: 100,
                ppid: 0,
                event: Event::Exit { stat: 256 },
            }]
        );
    }

    #[test]
    fn unrelated_processes_are_ignored() {
        let mut stream = Stream::new();
        stream
            .push(99, OPEN | 1, [0, 0x601, 0, 0])
            .lookup(99, 0x8888, "/elsewhere/x")
            .push(99, OPEN | 2, [0, 3, 0, 0]);
        let mut collector = collector();
        collector.seed_threads([(99, 999)]);
        assert_eq!(stream.collect(&mut collector), []);
    }

    #[test]
    fn normalizes_paths() {
        assert_eq!(normalize("/a/b/../c/./d"), "/a/c/d");
        assert_eq!(normalize("/a/../.."), "/");
        assert_eq!(normalize("/System/Volumes/Data/Users/x"), "/Users/x");
    }
}
