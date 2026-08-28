//! Typed wrappers around the small part of the Linux ptrace ABI we use.

use std::ffi::c_void;
use std::io;
use std::mem::{self, size_of};
use std::ptr;

pub(super) const TRACEME: libc::c_uint = 0;
pub(super) const PEEKDATA: libc::c_uint = 2;
pub(super) const CONT: libc::c_uint = 7;
pub(super) const SYSCALL: libc::c_uint = 24;
pub(super) const SETOPTIONS: libc::c_uint = 0x4200;
const GETEVENTMSG: libc::c_uint = 0x4201;
const GET_SYSCALL_INFO: libc::c_uint = 0x420e;

pub(super) const O_TRACESYSGOOD: usize = 0x1;
pub(super) const O_TRACEFORK: usize = 0x2;
pub(super) const O_TRACEVFORK: usize = 0x4;
pub(super) const O_TRACECLONE: usize = 0x8;
pub(super) const O_TRACEEXEC: usize = 0x10;
pub(super) const O_TRACEEXIT: usize = 0x40;
pub(super) const O_TRACESECCOMP: usize = 0x80;
pub(super) const O_EXITKILL: usize = 0x0010_0000;

pub(super) const EVENT_FORK: u32 = 1;
pub(super) const EVENT_VFORK: u32 = 2;
pub(super) const EVENT_CLONE: u32 = 3;
pub(super) const EVENT_EXEC: u32 = 4;
pub(super) const EVENT_EXIT: u32 = 6;
pub(super) const EVENT_SECCOMP: u32 = 7;

pub(super) const WAIT_WALL: libc::c_int = 0x4000_0000;
pub(super) const SYSCALL_INFO_EXIT: u8 = 2;
pub(super) const SYSCALL_INFO_SECCOMP: u8 = 3;

/// Convert ESRCH into `None` when a tracee exits between ptrace operations.
pub(super) fn allow_dead<T>(result: io::Result<T>) -> io::Result<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.raw_os_error() == Some(libc::ESRCH) => Ok(None),
        Err(error) => Err(error),
    }
}

pub(super) fn ignore_dead(result: io::Result<libc::c_long>) -> io::Result<()> {
    match result {
        Ok(_) => Ok(()),
        Err(error) if error.raw_os_error() == Some(libc::ESRCH) => Ok(()),
        Err(error) => Err(error),
    }
}

pub(super) fn call(
    request: libc::c_uint,
    pid: i32,
    address: usize,
    data: usize,
) -> io::Result<libc::c_long> {
    // SAFETY: ptrace interprets address and data according to request. Callers
    // pass integer values for the requests exposed by this module.
    let result = unsafe { libc::ptrace(request, pid, address as *mut c_void, data as *mut c_void) };
    if result == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(result)
    }
}

pub(super) fn event_message(tid: i32) -> io::Result<libc::c_ulong> {
    let mut message: libc::c_ulong = 0;
    // SAFETY: GETEVENTMSG writes one c_ulong to the supplied pointer while the
    // tracee is stopped.
    let result = unsafe {
        libc::ptrace(
            GETEVENTMSG,
            tid,
            ptr::null_mut::<c_void>(),
            &mut message as *mut libc::c_ulong as *mut c_void,
        )
    };
    if result == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(message)
    }
}

pub(super) fn syscall_info(tid: i32) -> io::Result<SyscallInfo> {
    // A zeroed value is a valid output buffer for this C ABI structure.
    let mut info: SyscallInfo = unsafe { mem::zeroed() };
    // SAFETY: GET_SYSCALL_INFO writes at most the supplied structure size,
    // and ptrace stops serialize access to the tracee state.
    let result = unsafe {
        libc::ptrace(
            GET_SYSCALL_INFO,
            tid,
            size_of::<SyscallInfo>() as *mut c_void,
            &mut info as *mut SyscallInfo as *mut c_void,
        )
    };
    if result == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(info)
    }
}

#[repr(C)]
pub(super) struct SyscallInfo {
    pub(super) op: u8,
    pad: [u8; 3],
    arch: u32,
    instruction_pointer: u64,
    stack_pointer: u64,
    pub(super) data: SyscallInfoData,
}

#[repr(C)]
pub(super) union SyscallInfoData {
    entry: SyscallEntry,
    pub(super) exit: SyscallExit,
    pub(super) seccomp: SyscallSeccomp,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct SyscallEntry {
    nr: u64,
    args: [u64; 6],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct SyscallExit {
    pub(super) rval: i64,
    is_error: u8,
    pad: [u8; 7],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct SyscallSeccomp {
    pub(super) nr: u64,
    pub(super) args: [u64; 6],
    ret_data: u32,
    pad: u32,
}

pub(super) fn wifexited(status: i32) -> bool {
    status & 0x7f == 0
}

pub(super) fn wexitstatus(status: i32) -> i32 {
    (status >> 8) & 0xff
}

pub(super) fn wifsignaled(status: i32) -> bool {
    // The signed-byte narrowing is load-bearing: for a ptrace stop the low
    // seven bits are 0x7f, and 0x80 only reads as negative once truncated.
    ((((status & 0x7f) + 1) as i8) >> 1) > 0
}

pub(super) fn wtermsig(status: i32) -> i32 {
    status & 0x7f
}

pub(super) fn wifstopped(status: i32) -> bool {
    status & 0xff == 0x7f
}

pub(super) fn wstopsig(status: i32) -> i32 {
    (status >> 8) & 0xff
}
