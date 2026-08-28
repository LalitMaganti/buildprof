//! Seccomp filter used to turn selected filesystem calls into ptrace events.

use std::io;

const SET_MODE_FILTER: libc::c_uint = 1;
const RET_TRACE: u32 = 0x7ff0_0000;
const RET_ALLOW: u32 = 0x7fff_0000;
const BPF_LD_W_ABS: u16 = 0x20;
const BPF_JMP_JEQ_K: u16 = 0x15;
const BPF_RET_K: u16 = 0x06;

pub(super) unsafe fn install_filter() -> io::Result<()> {
    let mut instructions = vec![SockFilter::statement(BPF_LD_W_ABS, 0)];
    for syscall in traced_syscalls() {
        instructions.push(SockFilter::jump(BPF_JMP_JEQ_K, syscall as u32, 0, 1));
        instructions.push(SockFilter::statement(BPF_RET_K, RET_TRACE));
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
    // The two-argument syscall is architecture-specific.
    #[cfg(target_arch = "x86_64")]
    syscalls.push(libc::SYS_rename);
    syscalls
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
