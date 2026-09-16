// Copyright 2026 The Buildprof Authors.
// SPDX-License-Identifier: Apache-2.0

//! The kernel trace facility, which reports every process and file event on the
//! machine as compact binary records.
//!
//! Configuring it needs root, but the kernel grants further access to the
//! process that owns the session, so the recorder sets the session up and then
//! drops privileges for the rest of the recording.
//!
//! The interface is private to Apple: `sysctl(KERN_KDEBUG, …)`, as used by
//! `fs_usage(1)` and `ktrace(1)`. It can change between releases.

use std::io;
use std::time::{Duration, SystemTime};

const CTL_KERN: i32 = 1;
const KERN_KDEBUG: i32 = 24;
const KERN_PROCARGS2: i32 = 49;

const KERN_KDENABLE: i32 = 3;
const KERN_KDSETBUF: i32 = 4;
const KERN_KDSETUP: i32 = 6;
const KERN_KDREMOVE: i32 = 7;
const KERN_KDREADTR: i32 = 10;
const KERN_KDTHRMAP: i32 = 12;
const KERN_KDSET_TYPEFILTER: i32 = 22;
const KERN_KDBUFWAIT: i32 = 23;
const KDEBUG_ENABLE_TRACE: i32 = 1;
const TYPEFILTER_BYTES: usize = (256 * 256) / 8;

/// Events the kernel writes per read, sized so a read covers a few
/// milliseconds of even a very parallel build.
const READ_EVENTS: usize = 128 * 1024;
const EVENT_BYTES: usize = 64;

/// Subclasses the recorder subscribes to. Class 7 (`DBG_TRACE`, process
/// lifecycle) is always enabled by the kernel.
const SUBSCRIPTIONS: &[(u16, u16)] = &[
    (0x04, 0x01), // BSD process: proc_exit, with its wait status
    (0x04, 0x0c), // BSD syscalls: the open, rename, chdir and fd family
    (0x03, 0x01), // File system: VFS_LOOKUP path records
];

/// One kernel trace record.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Event {
    /// `mach_absolute_time()` when the event happened.
    pub timestamp: u64,
    pub args: [u64; 4],
    pub thread: u64,
    pub debugid: u32,
}

impl Event {
    /// The event kind, without the start/end bits.
    pub fn code(&self) -> u32 {
        self.debugid & !3
    }

    pub fn is_start(&self) -> bool {
        self.debugid & 1 != 0
    }

    pub fn is_end(&self) -> bool {
        self.debugid & 2 != 0
    }
}

fn sysctl(mib: &mut [i32], buffer: *mut libc::c_void, length: &mut usize) -> io::Result<()> {
    // SAFETY: `mib` and `length` are valid for the call, and `buffer` either is
    // null or points at `*length` writable bytes.
    let result = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as u32,
            buffer,
            length,
            std::ptr::null_mut(),
            0,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn control(
    op: i32,
    value: Option<i32>,
    buffer: *mut libc::c_void,
    length: &mut usize,
) -> io::Result<()> {
    match value {
        Some(value) => sysctl(&mut [CTL_KERN, KERN_KDEBUG, op, value], buffer, length),
        None => sysctl(&mut [CTL_KERN, KERN_KDEBUG, op], buffer, length),
    }
}

fn command(op: i32, value: Option<i32>) -> io::Result<()> {
    control(op, value, std::ptr::null_mut(), &mut 0)
}

/// An enabled trace session. Dropping it stops tracing and frees the buffers.
pub struct Session {
    buffer: Vec<u8>,
}

impl Session {
    /// Configures and enables tracing. Must run as root; afterwards the
    /// recorder may drop privileges and keep reading.
    pub fn start(buffer_events: usize) -> io::Result<Self> {
        // Take over any session left behind by an earlier run.
        let _ = command(KERN_KDENABLE, Some(0));
        let _ = command(KERN_KDREMOVE, None);
        command(KERN_KDSETBUF, Some(buffer_events as i32)).map_err(describe)?;
        command(KERN_KDSETUP, None).map_err(describe)?;
        // From here on the session exists and must be torn down on any failure.
        let session = Self {
            buffer: vec![0; READ_EVENTS * EVENT_BYTES],
        };
        let mut filter = vec![0_u8; TYPEFILTER_BYTES];
        for (class, subclass) in SUBSCRIPTIONS {
            let bit = usize::from((class << 8) | subclass);
            filter[bit / 8] |= 1 << (bit % 8);
        }
        let mut length = filter.len();
        control(
            KERN_KDSET_TYPEFILTER,
            None,
            filter.as_mut_ptr().cast(),
            &mut length,
        )
        .map_err(describe)?;
        command(KERN_KDENABLE, Some(KDEBUG_ENABLE_TRACE)).map_err(describe)?;
        Ok(session)
    }

    /// Blocks until the kernel has a batch worth reading, or `timeout_ms`.
    pub fn wait(&self, timeout_ms: u64) -> io::Result<bool> {
        let mut length = timeout_ms as usize;
        control(KERN_KDBUFWAIT, None, std::ptr::null_mut(), &mut length)?;
        Ok(length != 0)
    }

    /// Threads that already existed when tracing started, as (thread, pid).
    pub fn thread_map(&self) -> io::Result<Vec<(u64, i32)>> {
        let mut map = vec![0_u8; 64 * 1024 * 32];
        let mut length = map.len();
        control(KERN_KDTHRMAP, None, map.as_mut_ptr().cast(), &mut length)?;
        Ok(map[..length.min(map.len())]
            .as_chunks::<32>()
            .0
            .iter()
            .map(|entry| {
                (
                    u64::from_le_bytes(entry[0..8].try_into().expect("8 bytes")),
                    i32::from_le_bytes(entry[8..12].try_into().expect("4 bytes")),
                )
            })
            .collect())
    }

    /// Reads whatever the kernel has buffered, oldest first.
    pub fn read(&mut self) -> io::Result<impl Iterator<Item = Event> + '_> {
        let mut length = self.buffer.len();
        control(
            KERN_KDREADTR,
            None,
            self.buffer.as_mut_ptr().cast(),
            &mut length,
        )?;
        // The kernel returns a count of events, not a byte count.
        let bytes = length.saturating_mul(EVENT_BYTES).min(self.buffer.len());
        Ok(self.buffer[..bytes]
            .as_chunks::<EVENT_BYTES>()
            .0
            .iter()
            .map(decode))
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = command(KERN_KDENABLE, Some(0));
        let _ = command(KERN_KDREMOVE, None);
    }
}

fn decode(record: &[u8; EVENT_BYTES]) -> Event {
    let word = |at: usize| u64::from_le_bytes(record[at..at + 8].try_into().expect("8 bytes"));
    Event {
        timestamp: word(0),
        args: [word(8), word(16), word(24), word(32)],
        thread: word(40),
        debugid: u32::from_le_bytes(record[48..52].try_into().expect("4 bytes")),
    }
}

/// Explains the failures people can do something about.
fn describe(error: io::Error) -> io::Error {
    match error.raw_os_error() {
        Some(libc::EBUSY) => io::Error::new(
            error.kind(),
            "another program is using the kernel trace facility (Instruments, ktrace, fs_usage \
             or a system diagnostic); stop it and try again",
        ),
        Some(libc::EPERM) => io::Error::new(error.kind(), "the kernel trace facility needs root"),
        _ => error,
    }
}

/// The command line of a running process, as `(executable, argv)`.
///
/// Fails with `EINVAL` while a process is still setting up its new image after
/// `exec`, and once it has exited, so callers retry briefly.
pub fn command_line(pid: i32, buffer: &mut Vec<u8>) -> io::Result<(String, Vec<String>)> {
    let mut length = buffer.len();
    sysctl(
        &mut [CTL_KERN, KERN_PROCARGS2, pid],
        buffer.as_mut_ptr().cast(),
        &mut length,
    )?;
    let data = &buffer[..length.min(buffer.len())];
    if data.len() < 4 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "short procargs"));
    }
    // [argc][executable path][padding][argv][envp]
    let count = i32::from_ne_bytes(data[0..4].try_into().expect("4 bytes")).max(0) as usize;
    let rest = &data[4..];
    let executable_end = rest
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(rest.len());
    let executable = String::from_utf8_lossy(&rest[..executable_end]).into_owned();
    let rest = &rest[executable_end..];
    let argv_start = rest
        .iter()
        .position(|byte| *byte != 0)
        .unwrap_or(rest.len());
    let argv = rest[argv_start..]
        .split(|byte| *byte == 0)
        .take(count)
        .map(|arg| String::from_utf8_lossy(arg).into_owned())
        .collect();
    Ok((executable, argv))
}

/// When a process started, which identifies it apart from its pid: pids are
/// reused, and a build can burn through thousands of them.
pub fn start_time(pid: i32) -> io::Result<SystemTime> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = size_of::<libc::proc_bsdinfo>() as i32;
    // SAFETY: `info` is a valid, correctly sized destination for the call.
    let written = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            std::ptr::from_mut(&mut info).cast(),
            size,
        )
    };
    if written != size {
        return Err(io::Error::last_os_error());
    }
    Ok(SystemTime::UNIX_EPOCH
        + Duration::new(info.pbi_start_tvsec, info.pbi_start_tvusec as u32 * 1_000))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_a_record() {
        let mut record = [0_u8; EVENT_BYTES];
        record[0..8].copy_from_slice(&1_234_u64.to_le_bytes());
        record[8..16].copy_from_slice(&7_u64.to_le_bytes());
        record[40..48].copy_from_slice(&99_u64.to_le_bytes());
        record[48..52].copy_from_slice(&0x0401_0006_u32.to_le_bytes());
        let event = decode(&record);
        assert_eq!(event.timestamp, 1_234);
        assert_eq!(event.args[0], 7);
        assert_eq!(event.thread, 99);
        assert_eq!(event.code(), 0x0401_0004);
        assert!(event.is_end() && !event.is_start());
    }

    #[test]
    fn reads_own_command_line() {
        let mut buffer = vec![0_u8; 1 << 16];
        let (executable, argv) =
            command_line(std::process::id() as i32, &mut buffer).expect("own argv");
        assert!(
            executable.contains("kdebug") || executable.contains("buildprof"),
            "{executable}"
        );
        assert!(!argv.is_empty());
    }

    #[test]
    fn own_start_time_is_in_the_past() {
        let started = start_time(std::process::id() as i32).expect("own start time");
        assert!(started <= SystemTime::now());
        assert!(start_time(-1).is_err());
    }

    #[test]
    fn missing_process_is_an_error() {
        let mut buffer = vec![0_u8; 1 << 16];
        // Pid 0 is the kernel, which has no argv to read.
        assert!(command_line(0, &mut buffer).is_err());
    }
}
