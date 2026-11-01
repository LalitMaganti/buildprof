// Copyright 2026 The Buildprof Authors.
// SPDX-License-Identifier: Apache-2.0

//! Turns kernel trace events into what the build did.
//!
//! The kernel reports every process on the machine. This follows only the ones
//! the build started. A process belongs to the build if the process that
//! created it does.

use super::event::{Event, Message};
use super::kdebug;
use std::collections::{HashMap, HashSet};
use std::io;

const NEWTHREAD: u32 = 0x0700_0004;
const DATA_EXEC: u32 = 0x0700_0008;
const STRING_EXEC: u32 = 0x0701_0008;
const LOST_EVENTS: u32 = 0x0702_0008;
const PROC_EXIT: u32 = 0x0401_0004;

pub struct Collector {
    root: i32,
    processes: HashSet<i32>,
    threads: HashMap<u64, i32>,
    /// Executions seen but not yet reported. The kernel names the new program
    /// in a second event.
    starting: HashMap<i32, u64>,
    /// The first thread of each new task that a program is starting in, and the
    /// thread that asked. execvp tries each PATH directory in turn, and the end
    /// of a failed try can be reported after the next has begun.
    new_tasks: HashMap<u64, u64>,
    /// Threads whose program another program replaced. The kernel reports the
    /// replaced program's end as an exit, on that thread.
    replaced: HashSet<u64>,
    /// Events the kernel dropped because the buffer filled.
    pub dropped: u64,
}

impl Collector {
    pub fn new(root: i32) -> Self {
        Self {
            root,
            processes: HashSet::from([root]),
            threads: HashMap::new(),
            starting: HashMap::new(),
            new_tasks: HashMap::new(),
            replaced: HashSet::new(),
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
                // Starting a program creates a new task, whose first thread
                // the kernel flags.
                if args[2] == 1 && parent == Some(pid) {
                    self.new_tasks.insert(child_thread, thread);
                }
                if let Some(parent) = parent
                    && parent != pid
                {
                    if self.processes.contains(&parent) {
                        self.processes.insert(pid);
                        out(Message {
                            mach_time: event.timestamp,
                            pid: parent,
                            ppid: 0,
                            event: Event::Fork { child: pid },
                        })?;
                    } else if pid != self.root && parent != 0 {
                        // Another process now has this pid, so stop following
                        // it. The kernel itself (pid 0) also starts worker
                        // threads in existing processes. Those are not new
                        // processes.
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
        if !self.processes.contains(&pid) {
            return Ok(());
        }

        match code {
            DATA_EXEC => {
                // The new program runs on the new task's thread. A start that
                // failed has no such exec, and the old program carries on.
                if let Some(asker) = self.new_tasks.remove(&thread) {
                    self.replaced.insert(asker);
                }
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
            // The old program's end, not the process's.
            PROC_EXIT if event.is_end() && self.replaced.remove(&thread) => {}
            // A start that failed, as when execvp tries each PATH directory.
            // The kernel discards the new task, and reports that as an exit on
            // its thread. The process carries on.
            PROC_EXIT if event.is_end() && self.new_tasks.remove(&thread).is_some() => {}
            PROC_EXIT if event.is_end() => {
                if pid != self.root {
                    self.processes.remove(&pid);
                }
                self.starting.remove(&pid);
                out(Message {
                    mach_time: event.timestamp,
                    pid,
                    ppid: 0,
                    event: Event::Exit {
                        stat: args[1] as i32,
                    },
                })?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Pids the build started that have not been seen to exit.
    pub fn live_pids(&self) -> impl Iterator<Item = i32> + '_ {
        self.processes.iter().copied()
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
            out
        }
    }

    fn collector() -> Collector {
        let mut collector = Collector::new(100);
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
    fn a_worker_thread_the_kernel_starts_keeps_its_process() {
        // GCD asks the kernel for worker threads. A kernel thread (pid 0)
        // then creates the thread inside the process.
        let mut collector = collector();
        collector.seed_threads([(1, 0)]);
        let mut stream = Stream::new();
        stream
            .push(10, NEWTHREAD, [11, 200, 0, 0])
            .push(1, NEWTHREAD, [12, 200, 0, 0])
            .push(12, NEWTHREAD, [13, 300, 0, 0]);
        let messages = stream.collect(&mut collector);
        assert!(messages.iter().any(
            |message| message.pid == 200 && matches!(message.event, Event::Fork { child: 300 })
        ));
    }

    #[test]
    fn a_pid_another_process_reuses_is_no_longer_followed() {
        let mut collector = collector();
        collector.seed_threads([(99, 999)]);
        let mut stream = Stream::new();
        stream
            .push(10, NEWTHREAD, [11, 200, 0, 0])
            .push(99, NEWTHREAD, [12, 200, 0, 0])
            .push(12, NEWTHREAD, [13, 300, 0, 0]);
        let messages = stream.collect(&mut collector);
        assert!(
            !messages
                .iter()
                .any(|message| matches!(message.event, Event::Fork { child: 300 }))
        );
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
    fn the_end_of_a_replaced_program_is_not_an_exit() {
        // As traced on macOS 26: starting a program creates thread 11 for the
        // new task, and the old program ends on thread 10.
        let mut stream = Stream::new();
        stream
            .push(10, NEWTHREAD, [11, 100, 1, 0])
            .exec(11, 100, "cc")
            .push(10, PROC_EXIT | 1, [100, 0, 0, 0])
            .push(10, PROC_EXIT | 2, [100, 0, 0, 0])
            .push(11, PROC_EXIT | 1, [100, 256, 0, 0])
            .push(11, PROC_EXIT | 2, [100, 256, 0, 0]);
        let messages = stream.collect(&mut collector());
        let exits: Vec<_> = messages
            .iter()
            .filter(|message| matches!(message.event, Event::Exit { .. }))
            .collect();
        assert_eq!(
            exits,
            [&Message {
                mach_time: 1_070,
                pid: 100,
                ppid: 0,
                event: Event::Exit { stat: 256 },
            }]
        );
    }

    #[test]
    fn a_failed_start_on_the_way_to_a_program_is_not_an_exit() {
        // As traced on macOS 26 when make runs a command through execvp: the
        // first PATH directory fails, so the kernel discards new task 11 and
        // reports an exit on its thread. That report can come after the next
        // attempt has begun. The third attempt succeeds on thread 13.
        let mut stream = Stream::new();
        stream
            .push(10, NEWTHREAD, [11, 100, 1, 0])
            .push(10, NEWTHREAD, [12, 100, 1, 0])
            .push(11, PROC_EXIT | 2, [100, 0, 0, 0])
            .push(12, PROC_EXIT | 2, [100, 0, 0, 0])
            .push(10, NEWTHREAD, [13, 100, 1, 0])
            .exec(13, 100, "pwd")
            .push(10, PROC_EXIT | 2, [100, 0, 0, 0])
            .push(13, PROC_EXIT | 2, [100, 256, 0, 0]);
        let messages = stream.collect(&mut collector());
        let events: Vec<_> = messages.iter().map(|message| &message.event).collect();
        assert!(matches!(events[0], Event::Exec { .. }), "{events:?}");
        assert_eq!(events[1..], [&Event::Exit { stat: 256 }]);
    }

    #[test]
    fn a_program_that_failed_to_start_leaves_its_process_running() {
        // The new task was created, but the program never ran in it.
        let mut stream = Stream::new();
        stream
            .push(10, NEWTHREAD, [11, 100, 1, 0])
            .push(10, PROC_EXIT | 2, [100, 256, 0, 0]);
        let messages = stream.collect(&mut collector());
        assert!(
            messages
                .iter()
                .any(|message| message.event == Event::Exit { stat: 256 }),
            "{messages:?}"
        );
    }
}
