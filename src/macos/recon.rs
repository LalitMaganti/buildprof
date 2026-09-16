// Copyright 2026 The Buildprof Authors.
// SPDX-License-Identifier: Apache-2.0

//! Turning kernel trace events into what the build did.
//!
//! The kernel reports every process on the machine, so this follows the ones
//! the build started: a process is part of the build if the process that
//! created it was. Command lines are not in the trace at all, and are read from
//! the process itself as soon as its execution appears.

use super::event::{Event, Message};
use super::kdebug;
use super::tracker::Clock;
use std::collections::{HashMap, HashSet};
use std::io;
use std::time::Duration;

const NEWTHREAD: u32 = 0x0700_0004;
const DATA_EXEC: u32 = 0x0700_0008;
const STRING_EXEC: u32 = 0x0701_0008;
const LOST_EVENTS: u32 = 0x0702_0008;
const PROC_EXIT: u32 = 0x0401_0004;

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

struct HeldExit {
    pid: i32,
    mach_time: u64,
    stat: i32,
    believe_after: u64,
}

pub struct Collector {
    root: i32,
    clock: Clock,
    processes: HashSet<i32>,
    threads: HashMap<u64, i32>,
    /// The program each process is running, for the ones whose command line
    /// could not be read.
    names: HashMap<i32, String>,
    held_exits: Vec<HeldExit>,
    argv_retries: Vec<(i32, u64, u32)>,
    argv_buffer: Vec<u8>,
    /// Events the kernel dropped because the buffer filled.
    pub dropped: u64,
    /// Processes whose command line was gone before it could be read, and which
    /// are named after their program alone.
    pub unnamed: u64,
}

impl Collector {
    pub fn new(root: i32, clock: Clock) -> Self {
        Self {
            root,
            clock,
            processes: HashSet::from([root]),
            threads: HashMap::new(),
            names: HashMap::new(),
            held_exits: Vec::new(),
            argv_retries: Vec::new(),
            argv_buffer: vec![0; 1 << 18],
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
                    if self.processes.contains(&parent) {
                        self.cancel_exit(pid);
                        self.processes.insert(pid);
                        out(Message {
                            mach_time: event.timestamp,
                            pid: parent,
                            ppid: 0,
                            event: Event::Fork { child: pid },
                        })?;
                    } else if pid != self.root {
                        // Someone else now has this pid; stop following it.
                        self.processes.remove(&pid);
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
        if !self.processes.contains(&pid) {
            return Ok(());
        }

        match code {
            DATA_EXEC => return self.exec(pid, event.timestamp, out),
            // The kernel names the new program right after the execution.
            STRING_EXEC => {
                self.names.insert(pid, decode_string(&args));
            }
            PROC_EXIT if event.is_end() => {
                self.held_exits.push(HeldExit {
                    pid,
                    mach_time: event.timestamp,
                    stat: args[1] as i32,
                    believe_after: event.timestamp + self.ticks(EXIT_HOLD_NS),
                });
            }
            _ => {}
        }
        Ok(())
    }

    /// Follows up the command lines that were not ready yet.
    pub fn tick(&mut self, out: &mut impl FnMut(Message) -> io::Result<()>) -> io::Result<()> {
        self.retry_command_lines(out, false)
    }

    /// Emits everything still held back, at the end of a recording.
    pub fn flush(&mut self, out: &mut impl FnMut(Message) -> io::Result<()>) -> io::Result<()> {
        for exit in std::mem::take(&mut self.held_exits) {
            self.emit_exit(exit, out)?;
        }
        self.retry_command_lines(out, true)
    }

    /// Pids the build started that have not been seen to exit.
    pub fn live_pids(&self) -> impl Iterator<Item = i32> + '_ {
        self.processes.iter().copied()
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

    fn exec(
        &mut self,
        pid: i32,
        mach_time: u64,
        out: &mut impl FnMut(Message) -> io::Result<()>,
    ) -> io::Result<()> {
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
        out(Message {
            mach_time,
            pid,
            ppid: 0,
            event: Event::Exec { argv, executable },
        })
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
}

/// Names arrive as little-endian words packed with the bytes of the name.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds the trace events a build produces, for tests.
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

        /// An execution: the kernel reports it, then names the program.
        fn exec(&mut self, thread: u64, pid: i32, name: &str) -> &mut Self {
            let mut bytes = name.as_bytes().to_vec();
            bytes.resize(32, 0);
            let mut words = [0; 4];
            for (word, chunk) in words.iter_mut().zip(bytes.chunks(8)) {
                *word = u64::from_le_bytes(chunk.try_into().expect("8 bytes"));
            }
            self.push(thread, DATA_EXEC, [pid as u64, 0, 0, 0]);
            self.push(thread, STRING_EXEC, words)
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
        let mut collector = Collector::new(100, clock);
        collector.seed_threads([(10, 100)]);
        collector
    }

    #[test]
    fn an_execution_whose_command_line_is_gone_is_named_after_its_program() {
        let mut stream = Stream::new();
        stream.exec(10, 100, "make");
        let mut collector = collector();
        let messages = stream.collect(&mut collector);
        assert_eq!(
            messages,
            [Message {
                mach_time: 1_010,
                pid: 100,
                ppid: 0,
                event: Event::Exec {
                    argv: vec!["make".into()],
                    executable: "make".into(),
                },
            }]
        );
        assert_eq!(collector.unnamed, 1);
    }

    #[test]
    fn a_new_process_is_followed_and_its_parent_reported() {
        let mut stream = Stream::new();
        stream
            .push(10, NEWTHREAD, [11, 200, 0, 0])
            .exec(11, 200, "cc");
        let messages = stream.collect(&mut collector());
        assert!(matches!(messages[0].event, Event::Fork { child: 200 }));
        assert_eq!(messages[0].pid, 100);
        assert!(matches!(
            &messages[1].event,
            Event::Exec { executable, .. } if executable == "cc"
        ));
        assert_eq!(messages[1].pid, 200);
    }

    #[test]
    fn unrelated_processes_are_ignored() {
        let mut stream = Stream::new();
        stream.exec(99, 999, "someone-else");
        let mut collector = collector();
        collector.seed_threads([(99, 999)]);
        assert_eq!(stream.collect(&mut collector), []);
    }

    #[test]
    fn an_exit_is_only_believed_when_nothing_follows_it() {
        // The kernel reports an exit when an image is replaced, too.
        let mut stream = Stream::new();
        stream
            .push(10, PROC_EXIT | 2, [100, 0, 0, 0])
            .exec(10, 100, "still-here");
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
}
