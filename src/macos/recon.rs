// Copyright 2026 The Buildprof Authors.
// SPDX-License-Identifier: Apache-2.0

//! Turning kernel trace events into what the build did.
//!
//! The kernel reports paths as the program passed them: relative to a working
//! directory, relative to a directory descriptor, or restarting at whatever a
//! symbolic link pointed to. So this follows the build's processes and keeps,
//! for each one, its working directory and the paths behind its open
//! descriptors. Command lines are not in the trace at all and are read from the
//! process itself as soon as its exec appears.

use super::event::{Event, Message, linux_open_flags};
use super::kdebug;
use super::tracker::Clock;
use std::collections::HashMap;
use std::io;
use std::time::Duration;

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
const RENAME: u32 = 0x040c_0200;
const RENAMEAT: u32 = 0x040c_0744;
const RENAMEATX: u32 = 0x040c_07a0;
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
/// also reports an exit when a process replaces its image, and the new image
/// then goes on doing things; anything further from the pid cancels the exit.
const EXIT_HOLD_NS: u64 = 10_000_000;
/// Attempts to read a command line before giving up on it. A read lands
/// before the new image is ready surprisingly often.
const ARGV_ATTEMPTS: u32 = 5;
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
    lookups: Vec<(u64, String)>,
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
    lookups: HashMap<u64, (u64, Vec<u64>)>,
    calls: HashMap<u64, Call>,
    /// Paths of files that some process looked up by an absolute path. A
    /// lookup that restarted at a symbolic link resolves through this.
    vnode_paths: HashMap<u64, String>,
    names: HashMap<i32, String>,
    held_exits: Vec<HeldExit>,
    argv_retries: Vec<(i32, u64, u32)>,
    argv_buffer: Vec<u8>,
    file_events: bool,
    /// Whether a name exists in a directory, asked only when a path cannot be
    /// resolved otherwise and remembered per directory and name.
    exists: HashMap<String, bool>,
    /// Events the kernel dropped because the buffer filled.
    pub dropped: u64,
    /// Processes whose command line was gone before it could be read, and which
    /// are named after their program alone.
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
            vnode_paths: HashMap::new(),
            names: HashMap::new(),
            held_exits: Vec::new(),
            argv_retries: Vec::new(),
            argv_buffer: vec![0; 1 << 18],
            file_events,
            exists: HashMap::new(),
            dropped: 0,
            unnamed: 0,
        }
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
                        // Someone else now has this pid; stop following it.
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
        // Anything further from a pid means its reported exit was an image
        // being replaced, not the process ending.
        self.cancel_exit(pid);
        self.believe_exits(event.timestamp, out)?;
        if !self.processes.contains_key(&pid) {
            return Ok(());
        }

        match code {
            STRING_EXEC => {
                self.names.insert(pid, decode_string(&args));
                return Ok(());
            }
            DATA_EXEC => return self.exec(pid, event.timestamp, out),
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
                let entry = self.lookups.entry(thread).or_default();
                if event.is_start() {
                    entry.0 = args[0];
                    entry.1.clear();
                    entry.1.extend_from_slice(&args[1..]);
                } else {
                    entry.1.extend_from_slice(&args);
                }
                if event.is_end() {
                    let (vnode, words) = std::mem::take(entry);
                    let path = decode_string(&words);
                    if path.starts_with('/') {
                        self.vnode_paths.insert(vnode, normalize(&path));
                    }
                    if let Some(call) = self.calls.get_mut(&thread) {
                        call.lookups.push((vnode, path));
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
        let held = std::mem::take(&mut self.held_exits);
        for exit in held {
            self.emit_exit(exit, out)?;
        }
        self.retry_command_lines(out, true)
    }

    /// Pids the build started that have not been seen to exit.
    pub fn live_pids(&self) -> impl Iterator<Item = i32> + '_ {
        self.processes.keys().copied()
    }

    fn ticks(&self, nanoseconds: u64) -> u64 {
        (nanoseconds as f64 * self.clock.ticks_per_ns()) as u64
    }

    /// Reads a command line, refusing one from a process that started after the
    /// exec: that pid now belongs to someone else.
    fn command_line(&mut self, pid: i32, mach_time: u64) -> io::Result<(String, Vec<String>)> {
        let started = kdebug::start_time(pid)?;
        if started > self.clock.wall(mach_time) + REUSE_TOLERANCE {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "the pid was reused before its command line could be read",
            ));
        }
        kdebug::command_line(pid, &mut self.argv_buffer)
    }

    fn cancel_exit(&mut self, pid: i32) {
        self.held_exits.retain(|exit| exit.pid != pid);
    }

    fn believe_exits(
        &mut self,
        now: u64,
        out: &mut impl FnMut(Message) -> io::Result<()>,
    ) -> io::Result<()> {
        while let Some(index) = self
            .held_exits
            .iter()
            .position(|exit| exit.believe_after <= now)
        {
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
        out: &mut impl FnMut(Message) -> io::Result<()>,
    ) -> io::Result<()> {
        if let Some(state) = self.processes.get_mut(&pid) {
            state.fds.retain(|_, (_, close_on_exec)| !*close_on_exec);
        }
        match self.command_line(pid, mach_time) {
            Ok((executable, argv)) => self.emit_exec(pid, mach_time, executable, argv, out),
            // Either the image is still being set up, or the process is already
            // gone. Come back to it: the kernel names the program in the event
            // after this one, so even giving up reads better later.
            Err(_) => {
                self.argv_retries.push((pid, mach_time, 1));
                Ok(())
            }
        }
    }

    fn retry_command_lines(
        &mut self,
        out: &mut impl FnMut(Message) -> io::Result<()>,
        last_chance: bool,
    ) -> io::Result<()> {
        for (pid, mach_time, attempt) in std::mem::take(&mut self.argv_retries) {
            match self.command_line(pid, mach_time) {
                Ok((executable, argv)) => self.emit_exec(pid, mach_time, executable, argv, out)?,
                Err(_) if attempt < ARGV_ATTEMPTS && !last_chance => {
                    self.argv_retries.push((pid, mach_time, attempt + 1));
                }
                // Gone before its command line could be read.
                Err(_) => self.emit_named_exec(pid, mach_time, out)?,
            }
        }
        Ok(())
    }

    /// Reports an execution the kernel named but whose arguments are lost.
    fn emit_named_exec(
        &mut self,
        pid: i32,
        mach_time: u64,
        out: &mut impl FnMut(Message) -> io::Result<()>,
    ) -> io::Result<()> {
        self.unnamed += 1;
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
                Some((vnode, raw)) => self.resolve(pid, *vnode, raw, directory),
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
            RENAME | RENAMEAT | RENAMEATX => {
                if !self.file_events || call.lookups.len() < 2 {
                    return Ok(());
                }
                // A rename looks up its source and then its destination.
                let at = code != RENAME;
                let last = call.lookups.len() - 1;
                let (from_vnode, from_raw) = call.lookups[last - 1].clone();
                let (to_vnode, to_raw) = call.lookups[last].clone();
                let from_dir = at.then(|| call.args[0]).filter(|fd| *fd as i32 != AT_FDCWD);
                let to_dir = at.then(|| call.args[2]).filter(|fd| *fd as i32 != AT_FDCWD);
                let from = self.resolve(pid, from_vnode, &from_raw, from_dir);
                let to = self.resolve(pid, to_vnode, &to_raw, to_dir);
                if let (Some(from), Some(to)) = (from, to) {
                    out(Message {
                        mach_time,
                        pid,
                        ppid: 0,
                        event: Event::Rename { from, to },
                    })?;
                }
            }
            CHDIR => {
                if let Some((vnode, raw)) = call.lookups.first().cloned() {
                    let resolved = self.resolve(pid, vnode, &raw, None);
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

    /// Whether `directory` holds `name`, remembered for later lookups.
    fn holds(&mut self, directory: &str, name: &str) -> bool {
        let candidate = format!("{directory}/{name}");
        if let Some(known) = self.exists.get(&candidate) {
            return *known;
        }
        let known = std::fs::symlink_metadata(&candidate).is_ok();
        self.exists.insert(candidate, known);
        known
    }

    fn fd_path(&self, pid: i32, fd: u64) -> Option<String> {
        self.processes
            .get(&pid)?
            .fds
            .get(&(fd as i32))
            .and_then(|(path, _)| path.clone())
    }

    /// Resolves a path as the kernel reported it.
    fn resolve(
        &mut self,
        pid: i32,
        vnode: u64,
        raw: &str,
        directory: Option<u64>,
    ) -> Option<String> {
        if raw.starts_with('/') {
            return Some(normalize(raw));
        }
        // A lookup's vnode is the object found, or its directory when the name
        // does not exist yet, so only a known path ending in the same name is
        // the same file. This recovers lookups that restarted at a symlink.
        let last = |path: &str| path.rsplit('/').next().unwrap_or_default().to_owned();
        if let Some(known) = self.vnode_paths.get(&vnode)
            && last(known) == last(raw)
        {
            return Some(known.clone());
        }
        let base = match directory {
            Some(fd) => self.fd_path(pid, fd)?,
            None => self.processes.get(&pid)?.cwd.clone()?,
        };
        // A lookup that restarted at a relative symbolic link near the root,
        // such as `/var` pointing at `private/var`, continues from the root
        // rather than from the process's directory.
        let first = raw.split('/').next().unwrap_or(raw);
        if !first.is_empty()
            && raw.contains('/')
            && !self.holds(&base, first)
            && self.holds("", first)
        {
            return Some(normalize(&format!("/{raw}")));
        }
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
            RENAME
                | RENAMEAT
                | RENAMEATX
                | CHDIR
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

/// Resolves `.` and `..` textually, and the data volume's firmlinked spelling,
/// which names the same files.
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
    fn recovers_a_lookup_that_restarted_at_a_symlink() {
        let mut stream = Stream::new();
        // Another process resolves the real path, so the vnode is known.
        stream
            .push(10, OPEN | 1, [0, 0, 0, 0])
            .lookup(10, 0x5555, "/toolchain/sdk-26/settings.plist")
            .push(10, OPEN | 2, [0, 3, 0, 0])
            // A later open of the same file through a relative symlink reports
            // only what the link pointed at.
            .push(10, OPEN | 1, [0, 0, 0, 0])
            .lookup(10, 0x5555, "sdk-26/settings.plist")
            .push(10, OPEN | 2, [0, 4, 0, 0]);
        let messages = stream.collect(&mut collector());
        let paths: Vec<_> = messages
            .iter()
            .filter_map(|message| match &message.event {
                Event::Open { path, .. } => Some(path.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            paths,
            [
                "/toolchain/sdk-26/settings.plist",
                "/toolchain/sdk-26/settings.plist"
            ]
        );
    }

    #[test]
    fn resolves_a_lookup_that_restarted_at_a_link_near_the_root() {
        // /var points at private/var, so a temporary file's lookup restarts
        // there and continues from the root, not the working directory.
        let mut stream = Stream::new();
        stream
            .push(10, OPEN | 1, [0, 0x601, 0, 0])
            .lookup(10, 0xaaaa, "private/var/folders/xx/ar.tmp")
            .push(10, OPEN | 2, [0, 3, 0, 0]);
        let messages = stream.collect(&mut collector());
        assert!(
            matches!(
                &messages[0].event,
                Event::Open { path, .. } if path == "/private/var/folders/xx/ar.tmp"
            ),
            "{messages:?}"
        );
    }

    #[test]
    fn reports_a_rename_with_both_paths_resolved() {
        let mut stream = Stream::new();
        stream
            .push(10, RENAME | 1, [0, 0, 0, 0])
            .lookup(10, 0x1234, "a-1234.o.tmp")
            .lookup(10, 0x5678, "a.o")
            .push(10, RENAME | 2, [0, 0, 0, 0]);
        let messages = stream.collect(&mut collector());
        assert_eq!(
            messages,
            [Message {
                mach_time: 1_040,
                pid: 100,
                ppid: 0,
                event: Event::Rename {
                    from: "/src/a-1234.o.tmp".into(),
                    to: "/src/a.o".into(),
                },
            }]
        );
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
