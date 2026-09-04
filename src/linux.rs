use self::{ptrace::*, seccomp::*, tracee::*};
use crate::compiler::Capture;
use crate::model::{FileOpen, Process, Rename, Segment};
use crate::perfetto::Writer;
use std::collections::{HashMap, HashSet};
use std::ffi::{CString, OsString, c_void};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::ptr;
use std::time::{Instant, SystemTime};

mod ptrace;
mod seccomp;
mod tracee;

const MINIMUM_SEGMENT_DURATION_NS: u64 = 1;
const SIGNAL_EXIT_STATUS_OFFSET: i32 = 128;
/// Exit status of the forked child when it cannot make itself traceable.
const TRACE_SETUP_FAILURE_STATUS: i32 = 126;
/// Exit status of the forked child when the command itself cannot start.
const EXEC_FAILURE_STATUS: i32 = 127;

pub fn record(
    command: &[OsString],
    writer: &mut Writer,
    compilers: &mut Capture,
) -> io::Result<u8> {
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
        child_exec(&argv, compilers);
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

    let options = O_TRACESYSGOOD
        | O_TRACEFORK
        | O_TRACEVFORK
        | O_TRACECLONE
        | O_TRACEEXEC
        | O_TRACEEXIT
        | O_TRACESECCOMP
        | O_EXITKILL;
    call(SETOPTIONS, child, 0, options)?;

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
        clock,
        root_pid: child,
        root_exit_code: None,
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
    clock: Instant,
    root_pid: i32,
    root_exit_code: Option<u8>,
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
    fn run(mut self) -> io::Result<u8> {
        while !self.active.is_empty() {
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

            if wifexited(status) || wifsignaled(status) {
                self.handle_task_exit(tid, status)?;
                continue;
            }
            if !wifstopped(status) {
                continue;
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
                self.resume(tid, 0)?;
                continue;
            }

            if event != 0 {
                self.handle_event(tid, event)?;
            } else if signal == (libc::SIGTRAP | 0x80) {
                self.handle_syscall_stop(tid)?;
            } else {
                self.resume_with_mode(tid, signal)?;
            }
        }

        let end_ns = self.now_ns();
        for state in self.processes.values_mut() {
            if let Some(mut segment) = state.segment.take() {
                segment.end_ns = end_ns;
                self.writer.segment(state.process, &segment)?;
            }
        }
        Ok(self.root_exit_code.unwrap_or(1))
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

        let path_address = seccomp.args[1] as usize;
        let raw_path = read_c_string(tid, path_address, MAX_PATH_BYTES)
            .unwrap_or_else(|_| "<unreadable path>".into());
        let flags = if nr == libc::SYS_openat {
            seccomp.args[2]
        } else {
            read_u64(tid, seccomp.args[2] as usize).unwrap_or(0)
        };
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
                previous.end_ns = timestamp_ns.max(
                    previous
                        .start_ns
                        .saturating_add(MINIMUM_SEGMENT_DURATION_NS),
                );
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
        let exit_code = if wifexited(status) {
            wexitstatus(status) as u32
        } else {
            (SIGNAL_EXIT_STATUS_OFFSET + wtermsig(status)) as u32
        };
        if tid == self.root_pid {
            self.root_exit_code = Some(exit_code.min(u8::MAX as u32) as u8);
        }
        self.exit_codes.insert(tgid, exit_code);

        if self.tasks.values().any(|candidate| *candidate == tgid) {
            return Ok(());
        }
        let timestamp_ns = self.now_ns();
        if let Some(mut state) = self.processes.remove(&tgid)
            && let Some(mut segment) = state.segment.take()
        {
            segment.end_ns =
                timestamp_ns.max(segment.start_ns.saturating_add(MINIMUM_SEGMENT_DURATION_NS));
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
        self.processes
            .get(&parent_pid)
            .map(|parent| {
                if parent.process.execed {
                    parent_pid
                } else {
                    parent.process.build_parent_pid
                }
            })
            .unwrap_or(parent_pid)
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

fn child_exec(argv: &[CString], compilers: &Capture) -> ! {
    unsafe {
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
