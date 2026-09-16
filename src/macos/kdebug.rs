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

const CTL_KERN: i32 = 1;
const KERN_KDEBUG: i32 = 24;

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
        assert!(event.is_end());
    }
}
