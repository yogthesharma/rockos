//! Per-thread syscall frame and **per-task kernel stack** top for [`crate::syscall`] tail / scheduling.

use core::sync::atomic::{AtomicU64, Ordering};
use x86_64::structures::paging::{PageSize, Size4KiB};

#[repr(C, align(4096))]
pub struct AlignedKstack(pub [u8; 4096]);

impl AlignedKstack {
    #[inline]
    pub fn top_ptr(&self) -> u64 {
        self.0.as_ptr() as u64 + self.0.len() as u64
    }
}

/// Callee-saved + user return state stashed across [`super::syscall::syscall_tail`].
#[derive(Clone, Debug)]
#[repr(C)]
pub struct SyscallSaved {
    pub user_rsp: u64,
    pub user_rip: u64,
    pub user_rflags: u64,
    pub rbx: u64,
    pub rbp: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,
}

impl SyscallSaved {
    pub const fn bootstrap_user(entry: u64, stack_top: u64, rflags: u64) -> Self {
        Self {
            user_rsp: stack_top,
            user_rip: entry,
            user_rflags: rflags,
            rbx: 0,
            rbp: 0,
            r12: 0,
            r13: 0,
            r14: 0,
            r15: 0,
        }
    }
}

/// `syscall_dispatch`'s RSP: `[0]=ret`, `[1]=pad`, `[2]=r15` … `[10]=user_rsp` (as `*const u64`).
pub unsafe fn save_from_dispatch_rsp(dispatch_rsp: *const u64) -> SyscallSaved {
    SyscallSaved {
        user_rsp: *dispatch_rsp.add(10),
        user_rip: *dispatch_rsp.add(9),
        user_rflags: *dispatch_rsp.add(8),
        rbx: *dispatch_rsp.add(7),
        rbp: *dispatch_rsp.add(6),
        r12: *dispatch_rsp.add(5),
        r13: *dispatch_rsp.add(4),
        r14: *dispatch_rsp.add(3),
        r15: *dispatch_rsp.add(2),
    }
}

/// Write the x86 syscall stack layout so **`ret`** runs [`SYSCALL_USER_RETURN`].
pub unsafe fn restore_to_kstack(top_exclusive: u64, s: &SyscallSaved, ret_to_kernel: u64) {
    let base = top_exclusive - 88;
    let p = base as *mut u64;
    p.write(ret_to_kernel);
    p.add(1).write(0); // align pad
    p.add(2).write(s.r15);
    p.add(3).write(s.r14);
    p.add(4).write(s.r13);
    p.add(5).write(s.r12);
    p.add(6).write(s.rbp);
    p.add(7).write(s.rbx);
    p.add(8).write(s.user_rflags);
    p.add(9).write(s.user_rip);
    p.add(10).write(s.user_rsp);
}

#[no_mangle]
pub static CURRENT_KSTACK_TOP: AtomicU64 = AtomicU64::new(0);

#[inline]
pub fn set_current_kstack_top(top_exclusive: u64) {
    CURRENT_KSTACK_TOP.store(top_exclusive, Ordering::SeqCst);
}

#[inline]
pub fn current_kstack_top() -> u64 {
    CURRENT_KSTACK_TOP.load(Ordering::SeqCst)
}

/// User-mode **child stack** page (after parent stack); see [`crate::process::USER_BRK_BASE`].
pub const USER_STACK_CHILD_TOP: u64 = 0x403000;
pub const CHILD_STACK_LO: u64 = USER_STACK_CHILD_TOP - Size4KiB::SIZE;
