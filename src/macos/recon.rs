// Copyright 2026 The Buildprof Authors.
// SPDX-License-Identifier: Apache-2.0

//! Turning kernel trace events into what the build did.
//!
//! The kernel reports every process on the machine, so this follows the ones
//! the build started: a process is part of the build if the process that
//! created it was.

use super::event::{Event, Message};
use super::kdebug;
use super::tracker::Clock;
use std::collections::{HashMap, HashSet};
use std::io;

const NEWTHREAD: u32 = 0x0700_0004;
const DATA_EXEC: u32 = 0x0700_0008;
const STRING_EXEC: u32 = 0x0701_0008;
const LOST_EVENTS: u32 = 0x0702_0008;
const PROC_EXIT: u32 = 0x0401_0004;

/// How long an exit is held before it is believed, in trace time. The kernel
/// also reports an exit when a process replaces its image, and the new image
/// then goes on doing things; anything further from the pid cancels the exit.
const EXIT_HOLD_NS: u64 = 10_000_000;

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
    /// Executions seen but not yet reported: the kernel names the new program
    /// in a second event.
    starting: HashMap<i32, u64>,
    held_exits: Vec<HeldExit>,
    /// Events the kernel dropped because the buffer filled.
    pub dropped: u64,
}

impl Collector {
    pub fn new(root: i32, clock: Clock) -> Self {
        Self {
            root,
            clock,
            processes: HashSet::from([root]),
            threads: HashMap::new(),
            starting: HashMap::new(),
            held_exits: Vec::new(),
            dropped: 0,
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
            DATA_EXEC => {
                self.starting.insert(pid, event.timestamp);
            }
            // The program's name follows the execution that started it.
            STRING_EXEC => {
                if let Some(mach_time) = self.starting.remove(&pid) {
                    out(Message {
                        mach_time,
                        pid,
                        ppid: 0,
                        event: Event::Exec {
                            executable: decode_string(&args),
                        },
                    })?;
                }
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

    /// Emits everything still held back, at the end of a recording.
    pub fn flush(&mut self, out: &mut impl FnMut(Message) -> io::Result<()>) -> io::Result<()> {
        for exit in std::mem::take(&mut self.held_exits) {
            self.emit_exit(exit, out)?;
        }
        Ok(())
    }

    /// Pids the build started that have not been seen to exit.
    pub fn live_pids(&self) -> impl Iterator<Item = i32> + '_ {
        self.processes.iter().copied()
    }

    fn ticks(&self, nanoseconds: u64) -> u64 {
        (nanoseconds as f64 * self.clock.ticks_per_ns()) as u64
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
        self.starting.remove(&exit.pid);
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
        };
        let mut collector = Collector::new(100, clock);
        collector.seed_threads([(10, 100)]);
        collector
    }

    #[test]
    fn reports_a_process_and_what_it_runs() {
        let mut stream = Stream::new();
        stream.exec(10, 100, "make");
        let messages = stream.collect(&mut collector());
        assert_eq!(
            messages,
            [Message {
                mach_time: 1_010,
                pid: 100,
                ppid: 0,
                event: Event::Exec {
                    executable: "make".into(),
                },
            }]
        );
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
            Event::Exec { executable } if executable == "cc"
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
