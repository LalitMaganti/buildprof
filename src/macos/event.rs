// Copyright 2026 The Buildprof Authors.
// SPDX-License-Identifier: Apache-2.0

//! What the collector observed. The tracker turns these into the trace's
//! process segments and file events.

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
    Exec {
        argv: Vec<String>,
        cwd: String,
        executable: String,
    },
    /// A `wait(2)` status.
    Exit {
        stat: i32,
    },
    /// `flags` are `open(2)` flags as Linux numbers them, so the trace format
    /// and the UI read the same on both platforms. `fd` is -1 when the
    /// collector does not report one.
    Open {
        path: String,
        flags: u64,
        fd: i32,
    },
}

/// Darwin `open(2)` flags as the Linux numbers the trace format stores.
pub fn linux_open_flags(flags: u64) -> u64 {
    const DARWIN_APPEND: u64 = 0x8;
    const DARWIN_CREAT: u64 = 0x200;
    const DARWIN_TRUNC: u64 = 0x400;
    const DARWIN_EXCL: u64 = 0x800;
    const DARWIN_DIRECTORY: u64 = 0x0010_0000;
    const LINUX_CREAT: u64 = 0o100;
    const LINUX_EXCL: u64 = 0o200;
    const LINUX_TRUNC: u64 = 0o1000;
    const LINUX_APPEND: u64 = 0o2000;
    const LINUX_DIRECTORY: u64 = 0o200_000;

    // The access mode is the low two bits on both.
    let mut linux = flags & 0o3;
    for (darwin, linux_bit) in [
        (DARWIN_APPEND, LINUX_APPEND),
        (DARWIN_CREAT, LINUX_CREAT),
        (DARWIN_TRUNC, LINUX_TRUNC),
        (DARWIN_EXCL, LINUX_EXCL),
        (DARWIN_DIRECTORY, LINUX_DIRECTORY),
    ] {
        if flags & darwin != 0 {
            linux |= linux_bit;
        }
    }
    linux
}

#[cfg(test)]
mod tests {
    use super::linux_open_flags;

    #[test]
    fn translates_open_flags() {
        assert_eq!(linux_open_flags(0x0), 0);
        // O_WRONLY | O_CREAT | O_TRUNC
        assert_eq!(linux_open_flags(0x1 | 0x200 | 0x400), 0o1 | 0o100 | 0o1000);
        // O_RDWR | O_APPEND | O_EXCL
        assert_eq!(linux_open_flags(0x2 | 0x8 | 0x800), 0o2 | 0o2000 | 0o200);
        // O_RDONLY | O_DIRECTORY
        assert_eq!(linux_open_flags(0x0010_0000), 0o200_000);
    }
}
