// Copyright 2026 The Buildprof Authors.
// SPDX-License-Identifier: Apache-2.0

//! Turns Endpoint Security events for the build's process tree into the same
//! process segments, file opens and renames the Linux tracer records.

use super::eslogger::{Event, Message, linux_open_flags};
use crate::blind_spots::BlindSpots;
use crate::compiler::Capture;
use crate::model::{FileOpen, Process, Rename, Segment};
use crate::perfetto::Writer;
use std::collections::{HashMap, HashSet};
use std::io;
use std::path::Path;

const MINIMUM_SEGMENT_DURATION_NS: u64 = 1;
const SIGNAL_EXIT_STATUS_OFFSET: u32 = 128;
/// Endpoint Security reports no descriptor; the trace keeps the field.
const UNKNOWN_FD: i32 = -1;
/// The raw exit status of a child that `posix_spawn` created but could not
/// exec. `posix_spawnp` tries each `PATH` directory in turn, so one spawn can
/// leave a row of these; no program ever ran in them.
const FAILED_SPAWN_STAT: i32 = 1;

/// Where a tracker's records go; the trace writer, or a list in tests.
pub trait Sink {
    fn process_started(&mut self, pid: i32) -> io::Result<()>;
    fn segment(&mut self, process: Process, segment: &Segment) -> io::Result<()>;
    fn file_open(&mut self, pid: i32, open: &FileOpen) -> io::Result<()>;
    fn rename(&mut self, pid: i32, rename: &Rename) -> io::Result<()>;
    /// A process ended; import any compiler profile it left behind.
    fn process_exited(&mut self, pid: i32, segment_start_ns: u64);
}

/// The trace being recorded, with the compiler profiles that feed into it.
pub struct TraceSink<'a> {
    pub writer: &'a mut Writer,
    pub compilers: &'a mut Capture,
}

impl Sink for TraceSink<'_> {
    fn process_started(&mut self, pid: i32) -> io::Result<()> {
        self.writer.process_started(pid)
    }
    fn segment(&mut self, process: Process, segment: &Segment) -> io::Result<()> {
        self.writer.segment(process, segment)
    }
    fn file_open(&mut self, pid: i32, open: &FileOpen) -> io::Result<()> {
        self.writer.file_open(pid, open)
    }
    fn rename(&mut self, pid: i32, rename: &Rename) -> io::Result<()> {
        self.writer.rename(pid, rename)
    }
    fn process_exited(&mut self, pid: i32, segment_start_ns: u64) {
        self.compilers
            .process_exited(pid, segment_start_ns, self.writer);
    }
}

/// Converts `mach_absolute_time` ticks to nanoseconds since recording began.
#[derive(Clone, Copy)]
pub struct Clock {
    pub origin: u64,
    pub numer: u32,
    pub denom: u32,
}

// libc points at the mach2 crate for these; not worth a dependency for two calls.
#[allow(deprecated)]
impl Clock {
    pub fn now() -> Self {
        let mut timebase = libc::mach_timebase_info { numer: 0, denom: 0 };
        unsafe { libc::mach_timebase_info(&mut timebase) };
        Self {
            origin: mach_now(),
            numer: timebase.numer.max(1),
            denom: timebase.denom.max(1),
        }
    }

    pub fn ns(&self, mach_time: u64) -> u64 {
        let ticks = u128::from(mach_time.saturating_sub(self.origin));
        (ticks * u128::from(self.numer) / u128::from(self.denom)).min(u128::from(u64::MAX)) as u64
    }

    pub fn elapsed_ns(&self) -> u64 {
        self.ns(mach_now())
    }
}

#[allow(deprecated)]
fn mach_now() -> u64 {
    unsafe { libc::mach_absolute_time() }
}

struct ProcessState {
    process: Process,
    segment: Segment,
}

pub struct Tracker<'a, S: Sink> {
    sink: &'a mut S,
    blind_spots: &'a mut BlindSpots,
    clock: Clock,
    root_pid: i32,
    root_exit_code: Option<u8>,
    processes: HashMap<i32, ProcessState>,
    /// Pids whose tracks are in the trace. A process is only announced once
    /// something is written for it, so a failed spawn leaves nothing behind.
    announced: HashSet<i32>,
}

impl<'a, S: Sink> Tracker<'a, S> {
    /// Starts following `root_pid`, already started running `command`.
    pub fn new(
        sink: &'a mut S,
        blind_spots: &'a mut BlindSpots,
        clock: Clock,
        root_pid: i32,
        name: String,
        command: String,
        cwd: String,
    ) -> Self {
        let root = ProcessState {
            process: Process {
                pid: root_pid,
                parent_pid: 0,
                build_parent_pid: 0,
                execed: false,
            },
            segment: Segment {
                start_ns: 0,
                end_ns: 0,
                name,
                command,
                cwd,
                exit_code: None,
            },
        };
        Self {
            sink,
            blind_spots,
            clock,
            root_pid,
            root_exit_code: None,
            processes: HashMap::from([(root_pid, root)]),
            announced: HashSet::new(),
        }
    }

    /// Pids started under the build that have not been seen to exit.
    pub fn live_pids(&self) -> impl Iterator<Item = i32> + '_ {
        self.processes.keys().copied()
    }

    pub fn root_exit_code(&self) -> Option<u8> {
        self.root_exit_code
    }

    pub fn handle(&mut self, message: Message) -> io::Result<()> {
        let timestamp_ns = self.clock.ns(message.mach_time);
        let pid = message.pid;
        if !self.processes.contains_key(&pid) {
            // Children started with posix_spawn may reach us as an exec
            // with no fork before it.
            if !matches!(message.event, Event::Exec { .. })
                || !self.processes.contains_key(&message.ppid)
            {
                return Ok(());
            }
            self.start_process(pid, message.ppid, timestamp_ns);
        }
        match message.event {
            Event::Fork { child } => {
                if !self.processes.contains_key(&child) {
                    self.start_process(child, pid, timestamp_ns);
                }
            }
            Event::Exec {
                argv,
                cwd,
                executable,
            } => self.exec(pid, timestamp_ns, argv, cwd, executable)?,
            Event::Exit { stat } => self.exit(pid, timestamp_ns, stat)?,
            Event::Open { path, fflag } => {
                announce(self.sink, &mut self.announced, pid)?;
                self.sink.file_open(
                    pid,
                    &FileOpen {
                        timestamp_ns,
                        path,
                        flags: linux_open_flags(fflag),
                        fd: UNKNOWN_FD,
                    },
                )?
            }
            Event::Rename { from, to } => {
                announce(self.sink, &mut self.announced, pid)?;
                self.sink.rename(
                    pid,
                    &Rename {
                        timestamp_ns,
                        from,
                        to,
                    },
                )?
            }
        }
        Ok(())
    }

    /// Ends every process still open, as of now.
    pub fn finish(mut self) -> io::Result<()> {
        let end_ns = self.clock.elapsed_ns();
        for (pid, mut state) in self.processes {
            state.segment.end_ns = end_ns.max(state.segment.start_ns);
            announce(self.sink, &mut self.announced, pid)?;
            self.sink.segment(state.process, &state.segment)?;
        }
        Ok(())
    }

    fn start_process(&mut self, pid: i32, parent_pid: i32, timestamp_ns: u64) {
        let parent = self.processes.get(&parent_pid);
        let build_parent_pid = parent
            .map(|parent| {
                if parent.process.execed {
                    parent_pid
                } else {
                    parent.process.build_parent_pid
                }
            })
            .unwrap_or(parent_pid);
        let (name, command, cwd) = parent
            .map(|parent| {
                (
                    format!("fork:{}", parent.segment.name),
                    parent.segment.command.clone(),
                    parent.segment.cwd.clone(),
                )
            })
            .unwrap_or_else(|| ("fork".into(), String::new(), String::new()));
        self.processes.insert(
            pid,
            ProcessState {
                process: Process {
                    pid,
                    parent_pid,
                    build_parent_pid,
                    execed: false,
                },
                segment: Segment {
                    start_ns: timestamp_ns,
                    end_ns: 0,
                    name,
                    command,
                    cwd,
                    exit_code: None,
                },
            },
        );
    }

    fn exec(
        &mut self,
        pid: i32,
        timestamp_ns: u64,
        mut argv: Vec<String>,
        cwd: String,
        executable: String,
    ) -> io::Result<()> {
        if argv.is_empty() {
            argv.push(executable.clone());
        }
        self.blind_spots.observe(&argv);
        let name = Path::new(
            argv.first()
                .filter(|arg| !arg.is_empty())
                .unwrap_or(&executable),
        )
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| format!("pid:{pid}"));
        let command = argv.join(" ");
        let Some(state) = self.processes.get_mut(&pid) else {
            return Ok(());
        };
        if state.process.execed {
            let previous = std::mem::replace(
                &mut state.segment,
                Segment {
                    start_ns: timestamp_ns,
                    end_ns: 0,
                    name,
                    command,
                    cwd,
                    exit_code: None,
                },
            );
            let previous = Segment {
                end_ns: timestamp_ns.max(
                    previous
                        .start_ns
                        .saturating_add(MINIMUM_SEGMENT_DURATION_NS),
                ),
                ..previous
            };
            announce(self.sink, &mut self.announced, pid)?;
            self.sink.segment(state.process, &previous)?;
        } else {
            state.process.execed = true;
            state.segment.name = name;
            state.segment.command = command;
            state.segment.cwd = cwd;
        }
        Ok(())
    }

    fn exit(&mut self, pid: i32, timestamp_ns: u64, stat: i32) -> io::Result<()> {
        let Some(mut state) = self.processes.remove(&pid) else {
            return Ok(());
        };
        if stat == FAILED_SPAWN_STAT && !state.process.execed && !self.announced.contains(&pid) {
            return Ok(());
        }
        let exit_code = if libc::WIFEXITED(stat) {
            libc::WEXITSTATUS(stat) as u32
        } else {
            SIGNAL_EXIT_STATUS_OFFSET + libc::WTERMSIG(stat) as u32
        };
        if pid == self.root_pid {
            self.root_exit_code = Some(exit_code.min(u32::from(u8::MAX)) as u8);
        }
        state.segment.end_ns = timestamp_ns.max(
            state
                .segment
                .start_ns
                .saturating_add(MINIMUM_SEGMENT_DURATION_NS),
        );
        state.segment.exit_code = Some(exit_code);
        announce(self.sink, &mut self.announced, pid)?;
        self.sink.segment(state.process, &state.segment)?;
        self.sink.process_exited(pid, state.segment.start_ns);
        Ok(())
    }
}

fn announce<S: Sink>(sink: &mut S, announced: &mut HashSet<i32>, pid: i32) -> io::Result<()> {
    if announced.insert(pid) {
        sink.process_started(pid)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::macos::eslogger::parse;
    use crate::macos::eslogger::tests::{exec, exit, fork, line};

    #[derive(Default)]
    struct Records(Vec<String>);

    impl Sink for Records {
        fn process_started(&mut self, pid: i32) -> io::Result<()> {
            self.0.push(format!("start {pid}"));
            Ok(())
        }
        fn segment(&mut self, process: Process, segment: &Segment) -> io::Result<()> {
            self.0.push(format!(
                "segment {} parent={} build_parent={} execed={} {}..{} {:?} cwd={} exit={:?}",
                process.pid,
                process.parent_pid,
                process.build_parent_pid,
                process.execed,
                segment.start_ns,
                segment.end_ns,
                segment.command,
                segment.cwd,
                segment.exit_code
            ));
            Ok(())
        }
        fn file_open(&mut self, pid: i32, open: &FileOpen) -> io::Result<()> {
            self.0.push(format!(
                "open {pid} {} {} flags={:o}",
                open.timestamp_ns, open.path, open.flags
            ));
            Ok(())
        }
        fn rename(&mut self, pid: i32, rename: &Rename) -> io::Result<()> {
            self.0
                .push(format!("rename {pid} {} -> {}", rename.from, rename.to));
            Ok(())
        }
        fn process_exited(&mut self, pid: i32, segment_start_ns: u64) {
            self.0.push(format!("exited {pid} {segment_start_ns}"));
        }
    }

    const CLOCK: Clock = Clock {
        origin: 1000,
        numer: 1,
        denom: 1,
    };

    #[test]
    fn records_a_build_tree() {
        let mut records = Records::default();
        let mut blind_spots = BlindSpots::default();
        let mut tracker = Tracker::new(
            &mut records,
            &mut blind_spots,
            CLOCK,
            10,
            "make".into(),
            "make -j2".into(),
            "/src".into(),
        );
        let open = r#"{"open":{"fflag":1538,"file":{"path":"/src/a.o"}}}"#;
        for event in [
            line(10, 1, 1001, 1, &exec(&["make", "-j2"], "/src")),
            line(10, 1, 1010, 2, &fork(11)),
            // posix_spawnp trying a PATH directory without the program.
            line(10, 1, 1011, 2, &fork(13)),
            line(13, 10, 1012, 2, &exit(1)),
            // posix_spawn: no fork event before the exec.
            line(12, 10, 1015, 3, &exec(&["cc", "-c", "a.c"], "/src")),
            line(11, 10, 1020, 4, &exec(&["sh", "-c", "true"], "/src")),
            line(12, 10, 1030, 5, open),
            line(12, 10, 1040, 6, &exit(0)),
            line(99, 1, 1041, 7, &exec(&["unrelated"], "/")),
            line(11, 10, 1050, 8, &exit(2 << 8)),
            line(10, 1, 1060, 9, &exit(0)),
        ] {
            tracker.handle(parse(&event).unwrap()).unwrap();
        }
        assert_eq!(tracker.live_pids().count(), 0);
        assert_eq!(tracker.root_exit_code(), Some(0));
        tracker.finish().unwrap();
        assert_eq!(
            records.0,
            [
                "start 12",
                "open 12 30 /src/a.o flags=1101",
                r#"segment 12 parent=10 build_parent=10 execed=true 15..40 "cc -c a.c" cwd=/src exit=Some(0)"#,
                "exited 12 15",
                "start 11",
                r#"segment 11 parent=10 build_parent=10 execed=true 10..50 "sh -c true" cwd=/src exit=Some(2)"#,
                "exited 11 10",
                "start 10",
                r#"segment 10 parent=0 build_parent=0 execed=true 0..60 "make -j2" cwd=/src exit=Some(0)"#,
                "exited 10 0",
            ]
        );
    }

    #[test]
    fn a_second_exec_starts_a_new_segment() {
        let mut records = Records::default();
        let mut blind_spots = BlindSpots::default();
        let mut tracker = Tracker::new(
            &mut records,
            &mut blind_spots,
            CLOCK,
            10,
            "sh".into(),
            "sh".into(),
            "/".into(),
        );
        for event in [
            line(10, 1, 1001, 1, &exec(&["sh", "-c", "exec cc"], "/")),
            line(10, 1, 1005, 2, &exec(&["cc"], "/")),
            line(10, 1, 1009, 3, &exit(9)),
        ] {
            tracker.handle(parse(&event).unwrap()).unwrap();
        }
        assert_eq!(tracker.root_exit_code(), Some(9 + 128));
        assert_eq!(
            records.0[1..],
            [
                r#"segment 10 parent=0 build_parent=0 execed=true 0..5 "sh -c exec cc" cwd=/ exit=None"#,
                r#"segment 10 parent=0 build_parent=0 execed=true 5..9 "cc" cwd=/ exit=Some(137)"#,
                // Profiles are matched to the program that last ran in the pid.
                "exited 10 5",
            ]
        );
    }

    #[test]
    fn converts_mach_ticks() {
        let clock = Clock {
            origin: 100,
            numer: 125,
            denom: 3,
        };
        assert_eq!(clock.ns(103), 125);
        assert_eq!(clock.ns(50), 0, "events before the origin clamp to it");
    }
}
