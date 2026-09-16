// Copyright 2026 The Buildprof Authors.
// SPDX-License-Identifier: Apache-2.0

//! Turns the collector's events into the same process segments the Linux
//! tracer records.

use super::event::{Event, Message};
use crate::blind_spots::BlindSpots;
use crate::model::{Process, Segment};
use crate::perfetto::Writer;
use std::collections::{HashMap, HashSet};
use std::io;
use std::path::Path;

const MINIMUM_SEGMENT_DURATION_NS: u64 = 1;
const SIGNAL_EXIT_STATUS_OFFSET: u32 = 128;
/// The raw exit status of a child that `posix_spawn` created but could not
/// exec. `posix_spawnp` tries each `PATH` directory in turn, so one spawn can
/// leave a row of these; no program ever ran in them.
const FAILED_SPAWN_STAT: i32 = 1;

/// Where a tracker's records go; the trace writer, or a list in tests.
pub trait Sink {
    fn process_started(&mut self, pid: i32) -> io::Result<()>;
    fn segment(&mut self, process: Process, segment: &Segment) -> io::Result<()>;
}

/// The trace being recorded.
pub struct TraceSink<'a> {
    pub writer: &'a mut Writer,
}

impl Sink for TraceSink<'_> {
    fn process_started(&mut self, pid: i32) -> io::Result<()> {
        self.writer.process_started(pid)
    }
    fn segment(&mut self, process: Process, segment: &Segment) -> io::Result<()> {
        self.writer.segment(process, segment)
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

    /// Trace time units per nanosecond.
    pub fn ticks_per_ns(&self) -> f64 {
        f64::from(self.denom) / f64::from(self.numer)
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
            Event::Exec { executable } => self.exec(pid, timestamp_ns, executable)?,
            Event::Exit { stat } => self.exit(pid, timestamp_ns, stat)?,
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

    fn exec(&mut self, pid: i32, timestamp_ns: u64, executable: String) -> io::Result<()> {
        self.blind_spots.observe(std::slice::from_ref(&executable));
        let name = Path::new(&executable)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| format!("pid:{pid}"));
        let command = executable.clone();
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
                    cwd: String::new(),
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
        self.sink.segment(state.process, &state.segment)
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

    #[derive(Default)]
    struct Records(Vec<String>);

    impl Sink for Records {
        fn process_started(&mut self, pid: i32) -> io::Result<()> {
            self.0.push(format!("start {pid}"));
            Ok(())
        }
        fn segment(&mut self, process: Process, segment: &Segment) -> io::Result<()> {
            self.0.push(format!(
                "segment {} parent={} build_parent={} execed={} {}..{} {:?} exit={:?}",
                process.pid,
                process.parent_pid,
                process.build_parent_pid,
                process.execed,
                segment.start_ns,
                segment.end_ns,
                segment.command,
                segment.exit_code
            ));
            Ok(())
        }
    }

    fn clock() -> Clock {
        Clock {
            origin: 1000,
            numer: 1,
            denom: 1,
        }
    }

    /// A message from `pid`, at `mach_time`.
    fn at(mach_time: u64, pid: i32, ppid: i32, event: Event) -> Message {
        Message {
            mach_time,
            pid,
            ppid,
            event,
        }
    }

    fn exec(program: &str) -> Event {
        Event::Exec {
            executable: format!("/usr/bin/{program}"),
        }
    }

    fn tracker<'a>(
        records: &'a mut Records,
        blind_spots: &'a mut BlindSpots,
        name: &str,
    ) -> Tracker<'a, Records> {
        Tracker::new(
            records,
            blind_spots,
            clock(),
            10,
            name.into(),
            name.into(),
            "/src".into(),
        )
    }

    #[test]
    fn records_a_build_tree() {
        let mut records = Records::default();
        let mut blind_spots = BlindSpots::default();
        let mut tracker = tracker(&mut records, &mut blind_spots, "make");
        for message in [
            at(1001, 10, 1, exec("make")),
            at(1010, 10, 1, Event::Fork { child: 11 }),
            // A child started with posix_spawn is claimed through its parent.
            at(1015, 12, 10, exec("cc")),
            at(1020, 11, 10, exec("sh")),
            at(1040, 12, 10, Event::Exit { stat: 0 }),
            at(1041, 99, 1, exec("unrelated")),
            at(1050, 11, 10, Event::Exit { stat: 2 << 8 }),
            at(1060, 10, 1, Event::Exit { stat: 0 }),
        ] {
            tracker.handle(message).unwrap();
        }
        assert_eq!(tracker.root_exit_code(), Some(0));
        tracker.finish().unwrap();
        assert_eq!(
            records.0,
            [
                "start 12",
                r#"segment 12 parent=10 build_parent=10 execed=true 15..40 "/usr/bin/cc" exit=Some(0)"#,
                "start 11",
                r#"segment 11 parent=10 build_parent=10 execed=true 10..50 "/usr/bin/sh" exit=Some(2)"#,
                "start 10",
                r#"segment 10 parent=0 build_parent=0 execed=true 0..60 "/usr/bin/make" exit=Some(0)"#,
            ]
        );
    }

    #[test]
    fn a_second_exec_starts_a_new_segment() {
        let mut records = Records::default();
        let mut blind_spots = BlindSpots::default();
        let mut tracker = tracker(&mut records, &mut blind_spots, "sh");
        for message in [
            at(1001, 10, 1, exec("sh")),
            at(1005, 10, 1, exec("cc")),
            at(1009, 10, 1, Event::Exit { stat: 9 }),
        ] {
            tracker.handle(message).unwrap();
        }
        assert_eq!(tracker.root_exit_code(), Some(9 + 128));
        assert_eq!(
            records.0[1..],
            [
                r#"segment 10 parent=0 build_parent=0 execed=true 0..5 "/usr/bin/sh" exit=None"#,
                r#"segment 10 parent=0 build_parent=0 execed=true 5..9 "/usr/bin/cc" exit=Some(137)"#,
            ]
        );
    }

    #[test]
    fn a_failed_spawn_leaves_nothing_behind() {
        let mut records = Records::default();
        let mut blind_spots = BlindSpots::default();
        let mut tracker = tracker(&mut records, &mut blind_spots, "make");
        for message in [
            at(1001, 10, 1, exec("make")),
            // posix_spawnp trying a PATH directory without the program.
            at(1002, 10, 1, Event::Fork { child: 13 }),
            at(1003, 13, 10, Event::Exit { stat: 1 }),
            at(1004, 10, 1, Event::Exit { stat: 0 }),
        ] {
            tracker.handle(message).unwrap();
        }
        tracker.finish().unwrap();
        assert!(
            !records.0.iter().any(|record| record.contains(" 13 ")),
            "{:?}",
            records.0
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
        assert!((clock.ticks_per_ns() - 3.0 / 125.0).abs() < f64::EPSILON);
    }
}
