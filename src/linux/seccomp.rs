// Copyright 2026 The Buildprof Authors.
// SPDX-License-Identifier: Apache-2.0

//! Seccomp filter used to turn selected filesystem calls, and the calls
//! LeakSanitizer makes around its leak check, into ptrace events.

use std::io;

const SET_MODE_FILTER: libc::c_uint = 1;
const RET_TRACE: u32 = 0x7ff0_0000;
const RET_ALLOW: u32 = 0x7fff_0000;
const BPF_LD_W_ABS: u16 = 0x20;
const BPF_JMP_JEQ_K: u16 = 0x15;
const BPF_RET_K: u16 = 0x06;
/// Offset of the low word of the first argument in `struct seccomp_data`.
const FIRST_ARGUMENT: u32 = 16;
/// `prctl(PR_SET_PTRACER, …)`: LeakSanitizer lets its own tracer thread attach.
pub(super) const PR_SET_PTRACER: libc::c_ulong = 0x5961_6d61;
/// `ptrace(PTRACE_DETACH, …)`: LeakSanitizer lets a thread go after its check.
pub(super) const PTRACE_DETACH: u32 = 17;

pub(super) unsafe fn install_filter() -> io::Result<()> {
    let mut instructions = vec![SockFilter::statement(BPF_LD_W_ABS, 0)];
    for syscall in traced_syscalls() {
        instructions.push(SockFilter::jump(BPF_JMP_JEQ_K, syscall as u32, 0, 1));
        instructions.push(SockFilter::statement(BPF_RET_K, RET_TRACE));
    }
    // Only PR_SET_PTRACER and PTRACE_DETACH stop; prctl calls are frequent.
    // Each jump skips to the final allow when its comparison fails.
    let checks = [
        (libc::SYS_prctl as u32, PR_SET_PTRACER as u32),
        (libc::SYS_ptrace as u32, PTRACE_DETACH),
    ];
    for (syscall, first_argument) in checks {
        instructions.push(SockFilter::jump(BPF_JMP_JEQ_K, syscall, 0, 3));
        instructions.push(SockFilter::statement(BPF_LD_W_ABS, FIRST_ARGUMENT));
        instructions.push(SockFilter::jump(BPF_JMP_JEQ_K, first_argument, 0, 1));
        instructions.push(SockFilter::statement(BPF_RET_K, RET_TRACE));
        // Reload the syscall number for the next check.
        instructions.push(SockFilter::statement(BPF_LD_W_ABS, 0));
    }
    instructions.push(SockFilter::statement(BPF_RET_K, RET_ALLOW));
    let program = SockFprog {
        len: instructions.len() as u16,
        filter: instructions.as_ptr(),
    };

    // SAFETY: the kernel only reads `program` and its instruction slice for
    // the duration of this call; both remain alive until it returns.
    let result = unsafe {
        libc::syscall(
            libc::SYS_seccomp,
            SET_MODE_FILTER,
            0,
            &program as *const SockFprog,
        )
    };
    if result == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn traced_syscalls() -> Vec<libc::c_long> {
    #[allow(unused_mut)]
    let mut syscalls = vec![
        libc::SYS_openat,
        libc::SYS_openat2,
        // Track artifacts moved into their final paths.
        libc::SYS_renameat,
        libc::SYS_renameat2,
    ];
    // These older syscalls are absent on architectures such as aarch64.
    // musl still uses open on x86_64, including inside Alpine containers.
    #[cfg(target_arch = "x86_64")]
    syscalls.extend([libc::SYS_open, libc::SYS_rename]);
    syscalls
}

pub(super) fn is_bare_open(nr: libc::c_long) -> bool {
    #[cfg(target_arch = "x86_64")]
    return nr == libc::SYS_open;
    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = nr;
        false
    }
}

pub(super) fn is_rename(nr: libc::c_long) -> bool {
    #[cfg(target_arch = "x86_64")]
    if nr == libc::SYS_rename {
        return true;
    }
    nr == libc::SYS_renameat || nr == libc::SYS_renameat2
}

pub(super) fn is_bare_rename(nr: libc::c_long) -> bool {
    #[cfg(target_arch = "x86_64")]
    return nr == libc::SYS_rename;
    #[cfg(not(target_arch = "x86_64"))]
    {
        let _ = nr;
        false
    }
}

#[repr(C)]
struct SockFilter {
    code: u16,
    jt: u8,
    jf: u8,
    k: u32,
}

impl SockFilter {
    fn statement(code: u16, k: u32) -> Self {
        Self {
            code,
            jt: 0,
            jf: 0,
            k,
        }
    }

    fn jump(code: u16, k: u32, jt: u8, jf: u8) -> Self {
        Self { code, jt, jf, k }
    }
}

#[repr(C)]
struct SockFprog {
    len: u16,
    filter: *const SockFilter,
}
