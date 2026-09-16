// Copyright 2026 The Buildprof Authors.
// SPDX-License-Identifier: Apache-2.0

//! What the collector observed. The tracker turns these into the trace's
//! process segments.

/// One thing a followed process did.
#[derive(Debug, PartialEq)]
pub struct Message {
    /// `mach_absolute_time()` when it happened.
    pub mach_time: u64,
    pub pid: i32,
    /// The parent, when the collector knows it; 0 otherwise.
    pub ppid: i32,
    pub event: Event,
}

#[derive(Debug, PartialEq)]
pub enum Event {
    Fork {
        child: i32,
    },
    /// What a process is now running.
    Exec {
        argv: Vec<String>,
        executable: String,
    },
    /// A `wait(2)` status.
    Exit {
        stat: i32,
    },
}
