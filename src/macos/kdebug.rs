// Copyright 2026 The Buildprof Authors.
// SPDX-License-Identifier: Apache-2.0

//! The kernel trace facility. It reports every process and file event on the
//! machine as compact binary records.
//!
//! Configuring it needs root. After that, the kernel lets the process that owns
//! the session keep reading. The recorder therefore sets up the session and
//! then drops privileges.
//!
//! The interface is `sysctl(KERN_KDEBUG, …)`. It is private to Apple and can
//! change between releases. `fs_usage(1)` and `ktrace(1)` use it too.

use std::io;

const CTL_KERN: i32 = 1;
const KERN_KDEBUG: i32 = 24;
const KERN_ARGMAX: i32 = 8;
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

/// The most events one read returns. This covers a few milliseconds of even a
/// very parallel build.
const READ_EVENTS: usize = 128 * 1024;
const EVENT_BYTES: usize = 64;

/// Subclasses the recorder subscribes to. The kernel always enables class 7,
/// `DBG_TRACE`, which reports process lifecycle.
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
    /// Configures and enables tracing. This must run as root. Afterwards the
    /// recorder can drop privileges and keep reading.
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
            "another program, like Instruments, ktrace or fs_usage, is using the kernel trace \
             facility; stop it and try again",
        ),
        Some(libc::EPERM) => io::Error::new(error.kind(), "the kernel trace facility needs root"),
        _ => error,
    }
}

/// How large a buffer `command_line` needs. The kernel cuts a command line
/// that does not fit short, without saying so.
pub fn command_line_space() -> usize {
    let mut argmax: i32 = 0;
    let mut length = size_of::<i32>();
    let read = sysctl(
        &mut [CTL_KERN, KERN_ARGMAX],
        std::ptr::from_mut(&mut argmax).cast(),
        &mut length,
    );
    // The kernel refuses a buffer much larger than this.
    match read {
        Ok(()) if argmax > 0 => argmax as usize,
        _ => 1 << 18,
    }
}

/// The command line of a running process, as `(executable, argv)`.
///
/// This fails while a process is still setting up a new program after `exec`,
/// and after it has exited. Callers retry for a short while.
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

/// When a process started, in trace time. This tells processes apart when a
/// pid is reused, which happens often in a build of thousands of processes.
/// The kernel takes it from the same clock as trace events, so the two compare
/// exactly.
pub fn start_time(pid: i32) -> io::Result<u64> {
    let mut info: libc::rusage_info_v4 = unsafe { std::mem::zeroed() };
    // SAFETY: `info` is a valid destination for this flavor.
    let result = unsafe {
        libc::proc_pid_rusage(
            pid,
            libc::RUSAGE_INFO_V4,
            std::ptr::from_mut(&mut info).cast(),
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(info.ri_proc_start_abstime)
}

/// The file id of the program a process is running now. The kernel's execution
/// event identifies the new program by the same id.
pub fn image(pid: i32) -> io::Result<u64> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::MetadataExt;

    let mut path = [0_u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: `path` is a valid destination of the length given.
    let length = unsafe { libc::proc_pidpath(pid, path.as_mut_ptr().cast(), path.len() as u32) };
    if length <= 0 {
        return Err(io::Error::last_os_error());
    }
    let path = std::ffi::OsStr::from_bytes(&path[..length as usize]);
    Ok(std::fs::metadata(path)?.ino())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_image_is_this_test_binary() {
        use std::os::unix::fs::MetadataExt;

        let expected = std::fs::metadata(std::env::current_exe().unwrap())
            .unwrap()
            .ino();
        assert_eq!(image(std::process::id() as i32).unwrap(), expected);
    }

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

    #[test]
    fn reads_a_long_command_line_whole() {
        let long = "x".repeat(600 * 1024);
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", "sleep 5; :", "sh", &long, "last"])
            .spawn()
            .expect("spawn");
        let mut buffer = vec![0_u8; command_line_space()];
        // The shell may not have finished starting yet.
        let mut read = command_line(child.id() as i32, &mut buffer);
        for _ in 0..100 {
            if read.as_ref().is_ok_and(|(_, argv)| argv.len() == 6) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
            read = command_line(child.id() as i32, &mut buffer);
        }
        child.kill().expect("kill");
        child.wait().expect("wait");
        let (_, argv) = read.expect("argv");
        assert_eq!(argv.len(), 6);
        assert_eq!(argv[4].len(), long.len());
        assert_eq!(argv[5], "last");
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
    #[allow(deprecated)]
    fn a_child_starts_between_the_trace_times_around_its_spawn() {
        // SAFETY: reads the trace clock.
        let before = unsafe { libc::mach_absolute_time() };
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("1")
            .spawn()
            .expect("spawn");
        let after = unsafe { libc::mach_absolute_time() };
        let started = start_time(child.id() as i32).expect("start time");
        child.kill().expect("kill");
        child.wait().expect("wait");
        assert!(before <= started && started <= after);
        assert!(start_time(-1).is_err());
    }

    #[test]
    fn missing_process_is_an_error() {
        let mut buffer = vec![0_u8; 1 << 16];
        // Pid 0 is the kernel. It has no argv to read.
        assert!(command_line(0, &mut buffer).is_err());
    }
}
