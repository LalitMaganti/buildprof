//! Read process metadata and memory while a tracee is stopped.

use super::ptrace;
use std::ffi::{OsString, c_void};
use std::fs;
use std::io;
use std::mem::size_of;
use std::path::{Path, PathBuf};
use std::ptr;

pub(super) const MAX_PATH_BYTES: usize = 4096;

pub(super) fn read_c_string(tid: i32, address: usize, limit: usize) -> io::Result<String> {
    let mut bytes = vec![0u8; limit];
    let count = read_memory(tid, address, &mut bytes)?;
    bytes.truncate(count);
    if let Some(nul) = bytes.iter().position(|byte| *byte == 0) {
        bytes.truncate(nul);
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

pub(super) fn read_u64(tid: i32, address: usize) -> io::Result<u64> {
    let mut bytes = [0u8; 8];
    let count = read_memory(tid, address, &mut bytes)?;
    if count < bytes.len() {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "short tracee read",
        ));
    }
    Ok(u64::from_ne_bytes(bytes))
}

fn read_memory(tid: i32, address: usize, destination: &mut [u8]) -> io::Result<usize> {
    let local = libc::iovec {
        iov_base: destination.as_mut_ptr().cast(),
        iov_len: destination.len(),
    };
    let remote = libc::iovec {
        iov_base: address as *mut c_void,
        iov_len: destination.len(),
    };
    // SAFETY: both iovecs describe valid buffers for this call. The remote
    // address may be invalid, which the kernel reports as an ordinary error.
    let count = unsafe { libc::process_vm_readv(tid, &local, 1, &remote, 1, 0) };
    if count >= 0 {
        return Ok(count as usize);
    }

    // Older kernels and security policies may reject process_vm_readv. Fall
    // back to word-sized ptrace reads so tracing still works there.
    let word_size = size_of::<libc::c_long>();
    let mut offset = 0;
    while offset < destination.len() {
        unsafe { *libc::__errno_location() = 0 };
        let word = unsafe {
            libc::ptrace(
                ptrace::PEEKDATA,
                tid,
                address.saturating_add(offset) as *mut c_void,
                ptr::null_mut::<c_void>(),
            )
        };
        let error = io::Error::last_os_error();
        if word == -1 && error.raw_os_error() != Some(0) {
            if offset == 0 {
                return Err(error);
            }
            break;
        }
        let bytes = word.to_ne_bytes();
        let amount = word_size.min(destination.len() - offset);
        destination[offset..offset + amount].copy_from_slice(&bytes[..amount]);
        offset += amount;
        if bytes[..amount].contains(&0) {
            break;
        }
    }
    Ok(offset)
}

/// Resolve a path relative to `AT_FDCWD` or an open directory fd.
pub(super) fn resolve_at_path(tid: i32, dirfd: i32, raw_path: &str) -> String {
    if Path::new(raw_path).is_absolute() {
        return raw_path.into();
    }
    let base = if dirfd == libc::AT_FDCWD {
        read_cwd(tid)
    } else {
        fs::read_link(format!("/proc/{tid}/fd/{dirfd}"))
            .ok()
            .map(|path| path.to_string_lossy().into_owned())
    };
    base.map(|dir| {
        PathBuf::from(dir)
            .join(raw_path)
            .to_string_lossy()
            .into_owned()
    })
    .unwrap_or_else(|| raw_path.into())
}

pub(super) fn resolve_open_path(tid: i32, fd: i32, raw_path: &str) -> String {
    fs::read_link(format!("/proc/{tid}/fd/{fd}"))
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| {
            if Path::new(raw_path).is_absolute() {
                raw_path.into()
            } else {
                read_cwd(tid)
                    .map(|cwd| {
                        PathBuf::from(cwd)
                            .join(raw_path)
                            .to_string_lossy()
                            .into_owned()
                    })
                    .unwrap_or_else(|| raw_path.into())
            }
        })
}

pub(super) fn read_command_line(pid: i32) -> Option<Vec<String>> {
    let bytes = fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let args = bytes
        .split(|byte| *byte == 0)
        .filter(|arg| !arg.is_empty())
        .map(|arg| String::from_utf8_lossy(arg).into_owned())
        .collect::<Vec<_>>();
    (!args.is_empty()).then_some(args)
}

pub(super) fn read_cwd(pid: i32) -> Option<String> {
    fs::read_link(format!("/proc/{pid}/cwd"))
        .ok()
        .map(|path| path.to_string_lossy().into_owned())
}

pub(super) fn read_ppid(tid: i32) -> Option<i32> {
    status_field(tid, "PPid:")
}

pub(super) fn read_tgid(tid: i32) -> Option<i32> {
    status_field(tid, "Tgid:")
}

fn status_field(tid: i32, field: &str) -> Option<i32> {
    let status = fs::read_to_string(format!("/proc/{tid}/status")).ok()?;
    status.lines().find_map(|line| {
        line.strip_prefix(field)
            .and_then(|value| value.trim().parse().ok())
    })
}

pub(super) fn command_line(command: &[OsString]) -> String {
    command
        .iter()
        .map(|arg| arg.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ")
}

pub(super) fn executable_name(command: &OsString) -> String {
    Path::new(command)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| command.to_string_lossy().into_owned())
}
