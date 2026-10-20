// Copyright 2026 The Buildprof Authors.
// SPDX-License-Identifier: Apache-2.0

use self::{ptrace::*, seccomp::*, tracee::*};
use crate::blind_spots::BlindSpots;
use crate::compiler::Capture;
use crate::model::{FileOpen, Process, Rename, Segment, exit_code};
use crate::perfetto::Writer;
use std::collections::{HashMap, HashSet};
use std::ffi::{CString, OsString, c_void};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::ptr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

mod ptrace;
mod seccomp;
mod tracee;

/// Exit status of the forked child when it cannot make itself traceable.
const TRACE_SETUP_FAILURE_STATUS: i32 = 126;
/// Exit status of the forked child when the command itself cannot start.
const EXEC_FAILURE_STATUS: i32 = 127;

/// How a recording treats processes and users beyond the defaults.
#[derive(Default)]
pub struct Options {
    /// Stop recording processes still running this long after the command
    /// exits. They keep running, traced but unrecorded, after buildprof's
    /// front process exits; see [`Front`]. `None` waits for every process.
    pub detach_after_exit: Option<Duration>,
    /// Run the command as this user. Buildprof must be root: a root tracer
    /// lets setuid programs such as sudo work, which they cannot otherwise.
    pub run_as: Option<RunAs>,
}

/// The identity the command runs as, resolved before fork.
pub struct RunAs {
    uid: libc::uid_t,
    gid: libc::gid_t,
    groups: Vec<libc::gid_t>,
}

impl RunAs {
    /// Resolves `name` and points HOME, USER and LOGNAME at it, which the
    /// command inherits.
    pub fn resolve(name: &str) -> io::Result<Self> {
        if unsafe { libc::geteuid() } != 0 {
            return Err(io::Error::other(
                "--run-as needs buildprof to run as root, for example under sudo",
            ));
        }
        let c_name =
            CString::new(name).map_err(|_| io::Error::other("user name contains a NUL byte"))?;
        let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
        let mut buffer = vec![0 as libc::c_char; 16 * 1024];
        let mut found: *mut libc::passwd = ptr::null_mut();
        let result = unsafe {
            libc::getpwnam_r(
                c_name.as_ptr(),
                &mut entry,
                buffer.as_mut_ptr(),
                buffer.len(),
                &mut found,
            )
        };
        if result != 0 || found.is_null() {
            return Err(io::Error::other(format!("no such user: {name}")));
        }
        let mut groups = vec![0 as libc::gid_t; 256];
        let mut count = groups.len() as libc::c_int;
        if unsafe {
            libc::getgrouplist(
                c_name.as_ptr(),
                entry.pw_gid,
                groups.as_mut_ptr(),
                &mut count,
            )
        } < 0
        {
            groups.resize(count.max(0) as usize, 0);
            unsafe {
                libc::getgrouplist(
                    c_name.as_ptr(),
                    entry.pw_gid,
                    groups.as_mut_ptr(),
                    &mut count,
                )
            };
        }
        groups.truncate(count.max(0) as usize);
        let home = unsafe { std::ffi::CStr::from_ptr(entry.pw_dir) }
            .to_string_lossy()
            .into_owned();
        // SAFETY: buildprof is single-threaded until it forks the command.
        unsafe {
            std::env::set_var("HOME", home);
            std::env::set_var("USER", name);
            std::env::set_var("LOGNAME", name);
        }
        Ok(Self {
            uid: entry.pw_uid,
            gid: entry.pw_gid,
            groups,
        })
    }

    pub fn owner(&self) -> (libc::uid_t, libc::gid_t) {
        (self.uid, self.gid)
    }
}

pub fn record(
    command: &[OsString],
    writer: &mut Writer,
    compilers: &mut Capture,
    blind_spots: &mut BlindSpots,
    file_events: bool,
    options: &Options,
) -> io::Result<(u8, Leftovers)> {
    let argv = make_argv(command)?;
    let initial_command = command_line(command);
    let initial_name = executable_name(command.first().expect("validated command"));
    let initial_cwd = std::env::current_dir()?.to_string_lossy().into_owned();
    let origin = SystemTime::now();
    let clock = Instant::now();
    compilers.set_origin(origin);

    let child = unsafe { libc::fork() };
    if child < 0 {
        return Err(io::Error::last_os_error());
    }
    if child == 0 {
        child_exec(&argv, compilers, file_events, options.run_as.as_ref());
    }

    let mut status = 0;
    if unsafe { libc::waitpid(child, &mut status, 0) } < 0 {
        return Err(io::Error::last_os_error());
    }
    if !wifstopped(status) {
        if libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == TRACE_SETUP_FAILURE_STATUS {
            return Err(io::Error::other(
                "tracing could not start here (see above); in Docker add --cap-add SYS_PTRACE, \
                 keep kernel.yama.ptrace_scope below 3, and note that gVisor-style sandboxes \
                 cannot trace at all",
            ));
        }
        return Err(io::Error::other("traced child did not stop before exec"));
    }

    let mut trace_options = O_TRACESYSGOOD
        | O_TRACEFORK
        | O_TRACEVFORK
        | O_TRACECLONE
        | O_TRACEEXEC
        | O_TRACEEXIT
        | O_EXITKILL;
    // The filter also reports LeakSanitizer's PR_SET_PTRACER call.
    if file_events {
        trace_options |= O_TRACESECCOMP;
    }
    call(SETOPTIONS, child, 0, trace_options)?;

    let root = Process {
        pid: child,
        parent_pid: 0,
        build_parent_pid: 0,
        execed: false,
    };
    writer.process_started(child)?;
    let tracer = Tracer {
        writer,
        compilers,
        blind_spots,
        clock,
        root_pid: child,
        root_exit_code: None,
        root_exited_at: None,
        detach_after_exit: options.detach_after_exit,
        trace_options,
        detached: Vec::new(),
        detached_exits: Arc::new(Mutex::new(HashMap::new())),
        leak_tracers: HashMap::new(),
        detaching: false,
        queued_leak_checks: Vec::new(),
        active: HashSet::from([child]),
        // The caller consumes the root's initial stop.
        started: HashSet::from([child]),
        tasks: HashMap::from([(child, child)]),
        newborns: HashMap::new(),
        pending_opens: HashMap::new(),
        pending_renames: HashMap::new(),
        exit_codes: HashMap::new(),
        processes: HashMap::from([(
            child,
            ProcessState {
                process: root,
                segment: Some(Segment {
                    start_ns: 0,
                    end_ns: 0,
                    name: initial_name,
                    command: initial_command,
                    cwd: initial_cwd,
                    exit_code: None,
                }),
            },
        )]),
    };

    tracer.resume(child, 0)?;
    tracer.run()
}

struct ProcessState {
    process: Process,
    segment: Option<Segment>,
}

/// Processes still running when the recording ended. They stay traced:
/// buildprof's seccomp filter makes file calls fail in a process nobody
/// traces, so they cannot simply be detached.
pub struct Leftovers {
    tasks: HashSet<i32>,
}

impl Leftovers {
    /// Resumes every stop, without recording, until all leftovers exit.
    pub fn keep_running(self) {
        let mut started = self.tasks;
        loop {
            let mut status = 0;
            let tid = unsafe { libc::waitpid(-1, &mut status, WAIT_WALL) };
            if tid < 0 {
                if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return;
            }
            if !wifstopped(status) {
                continue;
            }
            let signal = wstopsig(status);
            let event = (status as u32) >> 16;
            // New tasks stop once with SIGSTOP when they are first traced.
            let initial = started.insert(tid) && event == 0 && signal == libc::SIGSTOP;
            let deliver = if event != 0 || initial || signal == (libc::SIGTRAP | 0x80) {
                0
            } else {
                signal
            };
            let _ = call(CONT, tid, 0, deliver as usize);
        }
    }
}

/// The process the caller waits on when leftovers may outlive the recording.
///
/// Buildprof forks before recording: the parent waits for the exit status and
/// exits with it, while the child records and then keeps any leftover
/// processes running. Callers such as CI steps therefore finish when the
/// recording does, as they would without buildprof.
pub struct Front {
    status: i32,
}

impl Front {
    /// Forks; only the child returns.
    pub fn split() -> io::Result<Self> {
        let mut fds = [0; 2];
        if unsafe { libc::pipe(fds.as_mut_ptr()) } < 0 {
            return Err(io::Error::last_os_error());
        }
        let child = unsafe { libc::fork() };
        if child < 0 {
            return Err(io::Error::last_os_error());
        }
        if child > 0 {
            unsafe { libc::close(fds[1]) };
            let mut code = [0_u8; 1];
            let read = unsafe { libc::read(fds[0], code.as_mut_ptr().cast(), 1) };
            if read == 1 {
                std::process::exit(i32::from(code[0]));
            }
            // The recorder died before reporting: exit as it did.
            let mut status = 0;
            unsafe { libc::waitpid(child, &mut status, 0) };
            std::process::exit(if libc::WIFEXITED(status) {
                libc::WEXITSTATUS(status)
            } else {
                1
            });
        }
        unsafe { libc::close(fds[0]) };
        Ok(Self { status: fds[1] })
    }

    /// Lets the front process exit with `code`, then keeps `leftovers`
    /// running, detached from the caller's terminal and pipes.
    pub fn finish(self, code: u8, leftovers: Leftovers) -> ! {
        unsafe {
            libc::write(self.status, [code].as_ptr().cast(), 1);
            libc::close(self.status);
            let null = libc::open(c"/dev/null".as_ptr(), libc::O_RDWR);
            for fd in 0..3 {
                libc::dup2(null, fd);
            }
            libc::setsid();
        }
        leftovers.keep_running();
        std::process::exit(i32::from(code));
    }
}

struct PendingOpen {
    timestamp_ns: u64,
    raw_path: String,
    flags: u64,
}

struct PendingRename {
    timestamp_ns: u64,
    from: String,
    to: String,
}

struct Tracer<'writer> {
    writer: &'writer mut Writer,
    compilers: &'writer mut Capture,
    blind_spots: &'writer mut BlindSpots,
    clock: Instant,
    root_pid: i32,
    root_exit_code: Option<u8>,
    root_exited_at: Option<Instant>,
    detach_after_exit: Option<Duration>,
    trace_options: usize,
    /// Processes let go for a LeakSanitizer check, until its tracer exits.
    detached: Vec<ProcessState>,
    /// When each detached process exited, filled in by watcher threads.
    detached_exits: Arc<Mutex<HashMap<i32, u64>>>,
    /// LeakSanitizer tracer threads, by the process they check.
    leak_tracers: HashMap<i32, i32>,
    detaching: bool,
    queued_leak_checks: Vec<(i32, i32)>,
    active: HashSet<i32>,
    // Initial stops and parent clone events may arrive in either order.
    started: HashSet<i32>,
    tasks: HashMap<i32, i32>,
    newborns: HashMap<i32, i32>,
    pending_opens: HashMap<i32, PendingOpen>,
    pending_renames: HashMap<i32, PendingRename>,
    exit_codes: HashMap<i32, u32>,
    processes: HashMap<i32, ProcessState>,
}

impl Tracer<'_> {
    fn run(mut self) -> io::Result<(u8, Leftovers)> {
        let mut leftovers = HashSet::new();
        while !self.active.is_empty() {
            let grace = self.root_exited_at.zip(self.detach_after_exit);
            let flags = if grace.is_some() {
                WAIT_WALL | libc::WNOHANG
            } else {
                WAIT_WALL
            };
            let mut status = 0;
            let tid = unsafe { libc::waitpid(-1, &mut status, flags) };
            if tid == 0 {
                let (exited_at, wait) = grace.expect("WNOHANG only during the grace period");
                if exited_at.elapsed() >= wait {
                    leftovers = std::mem::take(&mut self.active);
                    for state in self.processes.values() {
                        if let Some(segment) = &state.segment {
                            self.blind_spots.outlived(&segment.name);
                        }
                    }
                    break;
                } else {
                    std::thread::sleep(Duration::from_millis(10));
                }
                continue;
            }
            if tid < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                if error.raw_os_error() == Some(libc::ECHILD) {
                    break;
                }
                return Err(error);
            }

            self.handle_status(tid, status)?;
        }
        self.finish_detached()?;
        let leftovers = Leftovers { tasks: leftovers };

        let end_ns = self.now_ns();
        for state in self.processes.values_mut() {
            if let Some(mut segment) = state.segment.take() {
                segment.end_ns = end_ns;
                self.writer.segment(state.process, &segment)?;
            }
        }
        Ok((self.root_exit_code.unwrap_or(1), leftovers))
    }

    fn handle_status(&mut self, tid: i32, status: i32) -> io::Result<()> {
        if wifexited(status) || wifsignaled(status) {
            return self.handle_task_exit(tid, status);
        }
        if !wifstopped(status) {
            return Ok(());
        }

        // Register the task without assuming which ptrace stop arrived.
        if let Some(parent_tgid) = self.newborns.remove(&tid) {
            self.initialize_newborn(tid, parent_tgid)?;
        }

        let signal = wstopsig(status);
        let event = (status as u32) >> 16;

        // Consume exactly one initial SIGSTOP per task.
        let first_stop = self.started.insert(tid);
        if first_stop && event == 0 && signal == libc::SIGSTOP {
            self.active.insert(tid);
            return self.resume(tid, 0);
        }

        if event != 0 {
            self.handle_event(tid, event)
        } else if signal == (libc::SIGTRAP | 0x80) {
            self.handle_syscall_stop(tid)
        } else {
            self.resume_with_mode(tid, signal)
        }
    }

    fn tgid_of(&self, tid: i32) -> i32 {
        self.tasks.get(&tid).copied().unwrap_or(tid)
    }

    /// Stops tracing every thread of `tgids` and leaves them running.
    ///
    /// `stopped` is a thread of one of them already in a ptrace stop; it is
    /// detached last, so the process stays put until all its threads are free.
    /// Lets LeakSanitizer's tracer attach to the process `tid` belongs to.
    ///
    /// The tracer thread stays traced so its file calls still pass our
    /// seccomp filter; the process is traced again when the tracer exits.
    fn release_for_leak_check(&mut self, tid: i32, tracer: i32) -> io::Result<()> {
        // A release in progress owns waitpid; this one waits its turn, stopped.
        if self.detaching {
            self.queued_leak_checks.push((tid, tracer));
            return Ok(());
        }
        let tgid = self.tgid_of(tid);
        if !self.active.contains(&tracer) {
            // Created with CLONE_UNTRACED; trace it so its opens still work.
            if call(SEIZE, tracer, 0, self.trace_options).is_ok() {
                self.active.insert(tracer);
                self.started.insert(tracer);
                self.tasks.insert(tracer, tracer);
            }
        }
        self.leak_tracers.insert(tracer, tgid);
        self.detaching = true;
        let result = self.detach_now(tgid, tid, tracer);
        self.detaching = false;
        result?;
        for (tid, tracer) in std::mem::take(&mut self.queued_leak_checks) {
            if self.processes.contains_key(&self.tgid_of(tid)) {
                self.release_for_leak_check(tid, tracer)?;
            }
        }
        Ok(())
    }

    /// Traces a process again after its leak check, best effort: threads
    /// that ran untraced in between could not open files.
    fn retrace(&mut self, tgid: i32) {
        let Some(index) = self
            .detached
            .iter()
            .position(|state| state.process.pid == tgid)
        else {
            return;
        };
        let Ok(threads) = std::fs::read_dir(format!("/proc/{tgid}/task")) else {
            return;
        };
        let mut traced = false;
        for thread in threads.flatten() {
            let Some(tid) = thread
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<i32>().ok())
            else {
                continue;
            };
            if call(SEIZE, tid, 0, self.trace_options).is_ok() {
                self.active.insert(tid);
                self.started.insert(tid);
                self.tasks.insert(tid, tgid);
                traced = true;
            }
        }
        if traced {
            let state = self.detached.remove(index);
            self.processes.insert(tgid, state);
        }
    }

    fn detach_now(&mut self, tgid: i32, stopped: i32, tracer: i32) -> io::Result<()> {
        let tgids = HashSet::from([tgid]);
        let tgids = &tgids;
        let mut pending: HashSet<i32> = self
            .active
            .iter()
            .copied()
            .filter(|tid| tgids.contains(&self.tgid_of(*tid)) && *tid != stopped && *tid != tracer)
            .collect();
        for &tid in &pending {
            unsafe { libc::syscall(libc::SYS_tgkill, self.tgid_of(tid), tid, libc::SIGSTOP) };
        }
        // Threads detached at another stop still have our SIGSTOP pending;
        // SIGCONT discards it.
        let mut need_continue = HashSet::new();
        while !pending.is_empty() {
            let mut status = 0;
            let tid = unsafe { libc::waitpid(-1, &mut status, WAIT_WALL) };
            if tid < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                if error.raw_os_error() == Some(libc::ECHILD) {
                    break;
                }
                return Err(error);
            }
            if !pending.contains(&tid) {
                // A thread first seen now that belongs to the process is
                // detached too; anything else, the tracer included, is traced on.
                let newborn_of = self.newborns.get(&tid).copied().or_else(|| read_tgid(tid));
                if tid != tracer
                    && wifstopped(status)
                    && newborn_of.is_some_and(|tgid| tgids.contains(&tgid))
                {
                    pending.insert(tid);
                } else {
                    self.handle_status(tid, status)?;
                    continue;
                }
            }
            if wifexited(status) || wifsignaled(status) {
                pending.remove(&tid);
                self.handle_task_exit(tid, status)?;
                continue;
            }
            if !wifstopped(status) {
                continue;
            }
            let signal = wstopsig(status);
            let event = (status as u32) >> 16;
            if matches!(event, EVENT_FORK | EVENT_VFORK | EVENT_CLONE)
                && let Ok(child) = event_message(tid)
                && child as i32 != tracer
            {
                // The new task is traced already and reports its own stop.
                let child = child as i32;
                pending.insert(child);
                self.newborns.insert(child, self.tgid_of(tid));
            }
            let deliver =
                if event == 0 && signal != libc::SIGSTOP && signal != (libc::SIGTRAP | 0x80) {
                    signal
                } else {
                    0
                };
            if !(event == 0 && signal == libc::SIGSTOP) {
                need_continue.insert(self.tgid_of(tid));
            }
            ignore_dead(call(DETACH, tid, 0, deliver as usize))?;
            pending.remove(&tid);
            self.forget_task(tid);
        }
        for tgid in &need_continue {
            unsafe { libc::kill(*tgid, libc::SIGCONT) };
        }
        ignore_dead(call(DETACH, stopped, 0, 0))?;
        self.forget_task(stopped);
        if let Some(state) = self.processes.remove(&tgid) {
            self.watch_exit(tgid);
            if let Some(segment) = &state.segment {
                self.blind_spots.leak_check(&segment.name);
            }
            self.detached.push(state);
        }
        Ok(())
    }

    fn forget_task(&mut self, tid: i32) {
        self.active.remove(&tid);
        self.started.remove(&tid);
        self.pending_opens.remove(&tid);
        self.pending_renames.remove(&tid);
        self.newborns.remove(&tid);
        self.tasks.remove(&tid);
    }

    /// Notes when a detached process exits, without being its tracer.
    fn watch_exit(&self, tgid: i32) {
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, tgid, 0) } as i32;
        if fd < 0 {
            return;
        }
        let exits = Arc::clone(&self.detached_exits);
        let clock = self.clock;
        std::thread::spawn(move || {
            let mut poll = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            while unsafe { libc::poll(&mut poll, 1, -1) } < 0
                && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted
            {}
            let at = clock.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64;
            exits.lock().expect("exit watcher lock").insert(tgid, at);
            unsafe { libc::close(fd) };
        });
    }

    /// Writes the processes that were detached, ending each when it exited
    /// if that has been seen, and collects the root's status if it was one.
    fn finish_detached(&mut self) -> io::Result<()> {
        if self.root_exit_code.is_none()
            && self
                .detached
                .iter()
                .any(|state| state.process.pid == self.root_pid)
        {
            let mut status = 0;
            while unsafe { libc::waitpid(self.root_pid, &mut status, 0) } < 0 {
                if io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
                    break;
                }
            }
            self.root_exit_code = Some(exit_code(status).min(u8::MAX as u32) as u8);
            // Give the root's watcher a moment to note the exit.
            std::thread::sleep(Duration::from_millis(20));
        }
        let end_ns = self.now_ns();
        let exits = self
            .detached_exits
            .lock()
            .expect("exit watcher lock")
            .clone();
        for mut state in std::mem::take(&mut self.detached) {
            let Some(mut segment) = state.segment.take() else {
                continue;
            };
            match exits.get(&state.process.pid) {
                Some(at) => segment.end_at(*at),
                None => segment.end_at(end_ns),
            }
            self.writer.segment(state.process, &segment)?;
        }
        Ok(())
    }

    fn handle_event(&mut self, tid: i32, event: u32) -> io::Result<()> {
        match event {
            EVENT_FORK | EVENT_VFORK | EVENT_CLONE => {
                let Some(message) = allow_dead(event_message(tid))? else {
                    return Ok(());
                };
                let child_tid = message as i32;
                let parent_tgid = self.tasks.get(&tid).copied().unwrap_or(tid);
                self.active.insert(child_tid);
                // Register immediately because clone and child stops race.
                // Re-registration is harmless.
                self.initialize_newborn(child_tid, parent_tgid)?;
                self.newborns.insert(child_tid, parent_tgid);
            }
            EVENT_EXEC => self.handle_exec(tid)?,
            EVENT_SECCOMP => return self.handle_seccomp(tid),
            EVENT_EXIT => {}
            _ => {}
        }
        self.resume_with_mode(tid, 0)
    }

    fn handle_seccomp(&mut self, tid: i32) -> io::Result<()> {
        let Some(info) = allow_dead(syscall_info(tid))? else {
            return Ok(());
        };
        if info.op != SYSCALL_INFO_SECCOMP {
            return Err(io::Error::other(
                "kernel returned invalid seccomp syscall information",
            ));
        }
        let seccomp = unsafe { info.data.seccomp };
        let nr = seccomp.nr as libc::c_long;

        // LeakSanitizer is about to attach its own tracer thread, which fails
        // while we trace the process: let it go before the check starts.
        if nr == libc::SYS_prctl && seccomp.args[0] == PR_SET_PTRACER {
            return self.release_for_leak_check(tid, seccomp.args[1] as i32);
        }

        // Resolve rename paths on entry while their directory fds are valid.
        if is_rename(nr) {
            let read_at = |dirfd: i32, address: u64| {
                let raw = read_c_string(tid, address as usize, MAX_PATH_BYTES)
                    .unwrap_or_else(|_| String::new());
                if raw.is_empty() {
                    None
                } else {
                    Some(resolve_at_path(tid, dirfd, &raw))
                }
            };
            // rename(old, new); renameat[2](olddirfd, old, newdirfd, new)
            let (from, to) = if is_bare_rename(nr) {
                (
                    read_at(libc::AT_FDCWD, seccomp.args[0]),
                    read_at(libc::AT_FDCWD, seccomp.args[1]),
                )
            } else {
                (
                    read_at(seccomp.args[0] as i32, seccomp.args[1]),
                    read_at(seccomp.args[2] as i32, seccomp.args[3]),
                )
            };
            if let (Some(from), Some(to)) = (from, to) {
                self.pending_renames.insert(
                    tid,
                    PendingRename {
                        timestamp_ns: self.now_ns(),
                        from,
                        to,
                    },
                );
            }
            return call(SYSCALL, tid, 0, 0).map(|_| ());
        }

        let (path_address, flags) = if is_bare_open(nr) {
            (seccomp.args[0], seccomp.args[1])
        } else if nr == libc::SYS_openat {
            (seccomp.args[1], seccomp.args[2])
        } else {
            (
                seccomp.args[1],
                read_u64(tid, seccomp.args[2] as usize).unwrap_or(0),
            )
        };
        let raw_path = read_c_string(tid, path_address as usize, MAX_PATH_BYTES)
            .unwrap_or_else(|_| "<unreadable path>".into());
        self.pending_opens.insert(
            tid,
            PendingOpen {
                timestamp_ns: self.now_ns(),
                raw_path,
                flags,
            },
        );
        call(SYSCALL, tid, 0, 0).map(|_| ())
    }

    fn handle_syscall_stop(&mut self, tid: i32) -> io::Result<()> {
        if let Some(pending) = self.pending_renames.remove(&tid) {
            let Some(info) = allow_dead(syscall_info(tid))? else {
                return Ok(());
            };
            if info.op != SYSCALL_INFO_EXIT {
                self.pending_renames.insert(tid, pending);
                return call(SYSCALL, tid, 0, 0).map(|_| ());
            }
            // Only a rename that succeeded moved anything.
            if unsafe { info.data.exit }.rval == 0 {
                let tgid = self.tasks.get(&tid).copied().unwrap_or(tid);
                self.writer.rename(
                    tgid,
                    &Rename {
                        timestamp_ns: pending.timestamp_ns,
                        from: pending.from,
                        to: pending.to,
                    },
                )?;
            }
            return self.resume(tid, 0);
        }
        let Some(pending) = self.pending_opens.remove(&tid) else {
            return self.resume(tid, 0);
        };
        let info = syscall_info(tid)?;
        if info.op != SYSCALL_INFO_EXIT {
            self.pending_opens.insert(tid, pending);
            return call(SYSCALL, tid, 0, 0).map(|_| ());
        }
        let exit = unsafe { info.data.exit };
        if exit.rval >= 0 {
            let fd = exit.rval as i32;
            let path = resolve_open_path(tid, fd, &pending.raw_path);
            let tgid = self.tasks.get(&tid).copied().unwrap_or(tid);
            self.writer.file_open(
                tgid,
                &FileOpen {
                    timestamp_ns: pending.timestamp_ns,
                    path,
                    flags: pending.flags,
                    fd,
                },
            )?;
        }
        self.resume(tid, 0)
    }

    fn handle_exec(&mut self, tid: i32) -> io::Result<()> {
        let tgid = self.tasks.get(&tid).copied().unwrap_or(tid);
        let timestamp_ns = self.now_ns();
        let argv = read_command_line(tid).unwrap_or_else(|| vec![format!("pid:{tid}")]);
        self.blind_spots.observe(&argv);
        let command = argv.join(" ");
        let name = argv
            .first()
            .and_then(|arg| Path::new(arg).file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| format!("pid:{tid}"));
        let cwd = read_cwd(tid).unwrap_or_default();

        // Exec and the parent's fork event may be observed out of order.
        if !self.processes.contains_key(&tgid) {
            self.adopt_unknown_process(tid, tgid, timestamp_ns)?;
        }
        let Some(state) = self.processes.get_mut(&tgid) else {
            return Ok(());
        };
        if state.process.execed {
            if let Some(previous) = state.segment.as_mut() {
                previous.end_at(timestamp_ns);
                self.writer.segment(state.process, previous)?;
            }
            state.segment = Some(Segment {
                start_ns: timestamp_ns,
                end_ns: 0,
                name,
                command,
                cwd,
                exit_code: None,
            });
        } else {
            state.process.execed = true;
            if let Some(initial) = state.segment.as_mut() {
                initial.name = name;
                initial.command = command;
                initial.cwd = cwd;
            }
        }
        Ok(())
    }

    /// Register a process first observed at exec, using `/proc` for its parent.
    fn adopt_unknown_process(&mut self, tid: i32, tgid: i32, timestamp_ns: u64) -> io::Result<()> {
        let parent_pid = read_ppid(tid).unwrap_or(0);
        let build_parent_pid = self.build_parent_of(parent_pid);
        self.tasks.entry(tid).or_insert(tgid);
        self.active.insert(tid);
        self.writer.process_started(tgid)?;
        self.processes.insert(
            tgid,
            ProcessState {
                process: Process {
                    pid: tgid,
                    parent_pid,
                    build_parent_pid,
                    execed: false,
                },
                segment: Some(Segment {
                    start_ns: timestamp_ns,
                    end_ns: 0,
                    name: String::new(),
                    command: String::new(),
                    cwd: String::new(),
                    exit_code: None,
                }),
            },
        );
        Ok(())
    }

    fn initialize_newborn(&mut self, tid: i32, parent_tgid: i32) -> io::Result<()> {
        let tgid = read_tgid(tid).unwrap_or(tid);
        self.tasks.insert(tid, tgid);
        if tgid == parent_tgid || self.processes.contains_key(&tgid) {
            return Ok(());
        }

        let timestamp_ns = self.now_ns();
        let build_parent_pid = self.build_parent_of(parent_tgid);
        let (name, command, cwd) = self
            .processes
            .get(&parent_tgid)
            .and_then(|parent| parent.segment.as_ref())
            .map(|segment| {
                (
                    format!("fork:{}", segment.name),
                    segment.command.clone(),
                    segment.cwd.clone(),
                )
            })
            .unwrap_or_else(|| ("fork".into(), String::new(), String::new()));
        self.writer.process_started(tgid)?;
        self.processes.insert(
            tgid,
            ProcessState {
                process: Process {
                    pid: tgid,
                    parent_pid: parent_tgid,
                    build_parent_pid,
                    execed: false,
                },
                segment: Some(Segment {
                    start_ns: timestamp_ns,
                    end_ns: 0,
                    name,
                    command,
                    cwd,
                    exit_code: None,
                }),
            },
        );
        Ok(())
    }

    fn handle_task_exit(&mut self, tid: i32, status: i32) -> io::Result<()> {
        self.active.remove(&tid);
        self.started.remove(&tid);
        self.pending_opens.remove(&tid);
        self.pending_renames.remove(&tid);
        self.newborns.remove(&tid);
        let tgid = self.tasks.remove(&tid).unwrap_or(tid);
        if let Some(checked) = self.leak_tracers.remove(&tid) {
            self.retrace(checked);
        }
        let exit_code = exit_code(status);
        if tid == self.root_pid {
            self.root_exit_code = Some(exit_code.min(u8::MAX as u32) as u8);
            self.root_exited_at = Some(Instant::now());
        }
        self.exit_codes.insert(tgid, exit_code);

        if self.tasks.values().any(|candidate| *candidate == tgid) {
            return Ok(());
        }
        let timestamp_ns = self.now_ns();
        if let Some(mut state) = self.processes.remove(&tgid)
            && let Some(mut segment) = state.segment.take()
        {
            segment.end_at(timestamp_ns);
            segment.exit_code = self.exit_codes.remove(&tgid);
            self.writer.segment(state.process, &segment)?;
            self.compilers
                .process_exited(tgid, segment.start_ns, self.writer);
        }
        Ok(())
    }

    fn resume_with_mode(&self, tid: i32, signal: i32) -> io::Result<()> {
        if self.pending_opens.contains_key(&tid) || self.pending_renames.contains_key(&tid) {
            ignore_dead(call(SYSCALL, tid, 0, signal as usize))
        } else {
            self.resume(tid, signal)
        }
    }

    fn resume(&self, tid: i32, signal: i32) -> io::Result<()> {
        ignore_dead(call(CONT, tid, 0, signal as usize))
    }

    fn now_ns(&self) -> u64 {
        self.clock.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64
    }

    fn build_parent_of(&self, parent_pid: i32) -> i32 {
        Process::build_parent_of(
            self.processes
                .get(&parent_pid)
                .map(|parent| &parent.process),
            parent_pid,
        )
    }
}

fn make_argv(command: &[OsString]) -> io::Result<Vec<CString>> {
    if command.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty command"));
    }
    command
        .iter()
        .map(|arg| {
            CString::new(arg.as_bytes()).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "command argument contains a NUL byte",
                )
            })
        })
        .collect()
}

fn child_exec(
    argv: &[CString],
    compilers: &Capture,
    file_events: bool,
    run_as: Option<&RunAs>,
) -> ! {
    unsafe {
        // Rust ignores SIGPIPE and exec preserves ignored signals.
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
        compilers.configure_child();
        if libc::ptrace(
            TRACEME,
            0,
            ptr::null_mut::<c_void>(),
            ptr::null_mut::<c_void>(),
        ) == -1
        {
            child_fail(
                b"ptrace is not permitted in this environment",
                TRACE_SETUP_FAILURE_STATUS,
            );
        }
        libc::raise(libc::SIGSTOP);
        if file_events {
            // With CAP_SYS_ADMIN the filter needs no no_new_privs, which would
            // stop setuid programs such as sudo from gaining privileges. Root
            // in a container usually lacks it and falls back.
            let filtered = libc::geteuid() == 0 && install_filter().is_ok();
            if !filtered {
                if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) == -1 {
                    child_fail(
                        b"could not set no_new_privs for the seccomp filter",
                        TRACE_SETUP_FAILURE_STATUS,
                    );
                }
                if install_filter().is_err() {
                    child_fail(
                        b"could not install the seccomp filter; the kernel or sandbox lacks seccomp-bpf",
                        TRACE_SETUP_FAILURE_STATUS,
                    );
                }
            }
        }
        if let Some(run_as) = run_as
            && (libc::setgroups(run_as.groups.len(), run_as.groups.as_ptr()) == -1
                || libc::setgid(run_as.gid) == -1
                || libc::setuid(run_as.uid) == -1)
        {
            child_fail(
                b"could not switch to the --run-as user",
                TRACE_SETUP_FAILURE_STATUS,
            );
        }
        let mut pointers: Vec<*const libc::c_char> = argv.iter().map(|arg| arg.as_ptr()).collect();
        pointers.push(ptr::null());
        libc::execvp(argv[0].as_ptr(), pointers.as_ptr());
        child_fail(b"could not execute the command", EXEC_FAILURE_STATUS);
    }
}

/// Reports why the forked child is giving up, then exits with `status`.
///
/// Runs between `fork` and `exec`, so it uses only async-signal-safe calls:
/// fixed byte strings, a hand-formatted errno, and `write` to stderr.
unsafe fn child_fail(message: &[u8], status: i32) -> ! {
    let errno = unsafe { *libc::__errno_location() };
    let reason: &[u8] = match errno {
        libc::EPERM => b"operation not permitted",
        libc::EACCES => b"permission denied",
        libc::ENOENT => b"no such file or directory",
        libc::ENOEXEC => b"not an executable",
        libc::ENOSYS => b"not supported by this kernel",
        libc::EINVAL => b"invalid argument",
        _ => b"",
    };
    let mut digits = [0_u8; 11];
    let mut index = digits.len();
    let mut remaining = errno.unsigned_abs();
    loop {
        index -= 1;
        digits[index] = b'0' + (remaining % 10) as u8;
        remaining /= 10;
        if remaining == 0 {
            break;
        }
    }
    for part in [
        b"buildprof: ".as_slice(),
        message,
        b" (errno ",
        &digits[index..],
        if reason.is_empty() { b"" } else { b": " },
        reason,
        b")\n",
    ] {
        unsafe { libc::write(libc::STDERR_FILENO, part.as_ptr().cast(), part.len()) };
    }
    unsafe { libc::_exit(status) }
}
