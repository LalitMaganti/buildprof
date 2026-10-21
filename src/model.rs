// Copyright 2026 The Buildprof Authors.
// SPDX-License-Identifier: Apache-2.0

/// The shortest a segment can be, so one that starts and ends in the same
/// nanosecond is still drawn.
const MINIMUM_SEGMENT_DURATION_NS: u64 = 1;
/// A shell reports a process killed by a signal as 128 plus the signal.
const SIGNAL_EXIT_STATUS_OFFSET: u32 = 128;

#[derive(Clone, Copy, Debug)]
pub struct Process {
    pub pid: i32,
    pub parent_pid: i32,
    /// Nearest ancestor known to have exec'd when this process was observed.
    pub build_parent_pid: i32,
    /// Whether this pid ever ran a program of its own.
    pub execed: bool,
}

#[derive(Debug)]
pub struct Segment {
    pub start_ns: u64,
    pub end_ns: u64,
    pub name: String,
    pub command: String,
    pub cwd: String,
    pub exit_code: Option<u32>,
}

impl Process {
    /// The process a child of `parent_pid` is attributed to: the parent if it
    /// ran a program of its own, and otherwise the one the parent belongs to.
    pub fn build_parent_of(parent: Option<&Process>, parent_pid: i32) -> i32 {
        match parent {
            Some(parent) if !parent.execed => parent.build_parent_pid,
            _ => parent_pid,
        }
    }
}

impl Segment {
    /// Ends the segment at `end_ns`, but never before it has lasted the
    /// shortest time a segment can.
    pub fn end_at(&mut self, end_ns: u64) {
        self.end_ns = end_ns.max(self.start_ns.saturating_add(MINIMUM_SEGMENT_DURATION_NS));
    }
}

/// The exit code a shell would report for a wait status: the code the process
/// exited with, or 128 plus the signal that ended it.
pub fn exit_code(wait_status: i32) -> u32 {
    let signal = wait_status & 0x7f;
    if signal == 0 {
        ((wait_status >> 8) & 0xff) as u32
    } else {
        SIGNAL_EXIT_STATUS_OFFSET + signal as u32
    }
}

/// A path moved from a temporary or cache location to its final destination.
#[derive(Debug)]
pub struct Rename {
    pub timestamp_ns: u64,
    pub from: String,
    pub to: String,
}

#[derive(Debug)]
pub struct FileOpen {
    pub timestamp_ns: u64,
    pub path: String,
    pub flags: u64,
    pub fd: i32,
    /// The recorder could not work out where `path` is. It holds the path as
    /// the program's lookup ended, not an absolute one.
    pub unresolved: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_exit_codes_as_a_shell_would() {
        assert_eq!(exit_code(0), 0);
        assert_eq!(exit_code(2 << 8), 2);
        // Killed by SIGKILL.
        assert_eq!(exit_code(9), 137);
    }

    #[test]
    fn attributes_a_child_to_the_nearest_ancestor_that_ran_a_program() {
        let process = |pid, build_parent_pid, execed| Process {
            pid,
            parent_pid: 0,
            build_parent_pid,
            execed,
        };
        assert_eq!(
            Process::build_parent_of(Some(&process(10, 1, true)), 10),
            10
        );
        assert_eq!(
            Process::build_parent_of(Some(&process(11, 10, false)), 11),
            10
        );
        assert_eq!(Process::build_parent_of(None, 12), 12);
    }

    #[test]
    fn a_segment_lasts_at_least_a_nanosecond() {
        let mut segment = Segment {
            start_ns: 5,
            end_ns: 0,
            name: String::new(),
            command: String::new(),
            cwd: String::new(),
            exit_code: None,
        };
        segment.end_at(5);
        assert_eq!(segment.end_ns, 6);
        segment.end_at(9);
        assert_eq!(segment.end_ns, 9);
    }
}
