//! `syscall` / `sysret` ABI (Linux-compatible numbers for [`crate::user`] demos).
//!
//! # Implemented
//! - [`SYS_READ`] (**0**): non-blocking read of **raw PS/2 scancodes** from `fd == 0` into user memory.
//! - [`SYS_WRITE`] (**1**): `fd` ignored; writes to serial.
//! - [`SYS_EXIT`] (**60**): log and return `0` to ring 3.
//!
//! # Stubs (`-ENOSYS`)
//! - [`SYS_MMAP`] (**9**), [`SYS_BRK`] (**12**): not yet (need virtual-memory policy + per-process state).
//!
//! # Roadmap (Unix-ish)
//! - **`mmap` / `brk`**: anonymous mappings, optional kernel `VmArea` list, syscall return virt ranges.
//! - **Per-process address space**: separate P4 or deep `fork`; switch `CR3` on reschedule.
//! - **ELF loader / `exec`**: parse program headers, map RX/RW, user stack, `auxv`, jump to entry.
//! - **Scheduler + `sleep`**: wait queues, `nanosleep`-style syscall, tie PIT or TSC to ticks.
//! - **`wait` / `exit`**: parent collects status; zombie/reap; align with `SIGCHLD`-like semantics later.

use core::arch::global_asm;
use x86_64::registers::model_specific::{Efer, EferFlags, LStar, SFMask, Star};
use x86_64::registers::rflags::RFlags;
use x86_64::VirtAddr;

#[no_mangle]
static mut SYSCALL_USER_RSP_STASH: u64 = 0;

#[repr(C, align(4096))]
struct SyscallKernelStack([u8; 4096]);

#[no_mangle]
static mut SYSCALL_KERNEL_STACK: SyscallKernelStack = SyscallKernelStack([0u8; 4096]);

global_asm!(
    ".section .text",
    ".global syscall_entry",
    ".type syscall_entry, @function",
    "syscall_entry:",
    "mov [rip + SYSCALL_USER_RSP_STASH], rsp",
    "lea rsp, [rip + SYSCALL_KERNEL_STACK + 4096]",
    "push [rip + SYSCALL_USER_RSP_STASH]",
    "push rcx",
    "push r11",
    "push rbx",
    "push rbp",
    "push r12",
    "push r13",
    "push r14",
    "push r15",
    "sub rsp, 8",
    "mov r9, rax",
    "mov r8, rdi",
    "mov r10, rsi",
    "mov r11, rdx",
    "mov rdi, r9",
    "mov rsi, r8",
    "mov rdx, r10",
    "mov rcx, r11",
    "call syscall_dispatch",
    "add rsp, 8",
    "pop r15",
    "pop r14",
    "pop r13",
    "pop r12",
    "pop rbp",
    "pop rbx",
    "pop r11",
    "pop rcx",
    "pop rsp",
    "sysretq",
);

unsafe extern "C" {
    fn syscall_entry();
}

const ENOSYS: u64 = (-38i64) as u64;

/// Linux x86-64: `read(2)`
pub const SYS_READ: u64 = 0;
/// Linux: `write(2)`
pub const SYS_WRITE: u64 = 1;
/// Linux: `mmap(2)` — stub
pub const SYS_MMAP: u64 = 9;
/// Linux: `brk(2)` — stub
pub const SYS_BRK: u64 = 12;
/// Linux: `exit(2)` / `exit_group`-style status in `rdi`.
pub const SYS_EXIT: u64 = 60;

#[no_mangle]
extern "C" fn syscall_dispatch(n: u64, arg0: u64, arg1: u64, arg2: u64) -> u64 {
    match n {
        SYS_READ => sys_read(arg0, arg1, arg2),
        SYS_WRITE => sys_write(arg0, arg1, arg2),
        SYS_MMAP | SYS_BRK => ENOSYS,
        SYS_EXIT => {
            crate::println!("[syscall] exit({})", arg0 as i32);
            0
        }
        _ => ENOSYS,
    }
}

/// `read(0, buf, len)` — drains the **IRQ keyboard queue** (raw scancodes). Non-blocking: returns
/// `0`..=`len`, never waits for a key. Other `fd` → `-EBADF` (-9).
fn sys_read(fd: u64, buf_addr: u64, len: u64) -> u64 {
    if fd != 0 {
        return (-9i64) as u64;
    }
    if len > 4096 {
        return (-22i64) as u64;
    }
    let len = len as usize;
    if !crate::user::user_region_is_writable_range(buf_addr, len) {
        return (-14i64) as u64;
    }
    let dst = unsafe { core::slice::from_raw_parts_mut(buf_addr as *mut u8, len) };
    let mut n = 0usize;
    while n < len {
        // `read_scancode`: IRQ queue first, then **port poll** — works even when user runs with `IF=0`.
        let Some(b) = crate::keyboard::read_scancode() else {
            break;
        };
        dst[n] = b;
        n += 1;
    }
    n as u64
}

fn sys_write(_fd: u64, buf_addr: u64, len: u64) -> u64 {
    if len > 4096 {
        return (-28i64) as u64;
    }
    let len = len as usize;
    if !crate::user::user_region_is_readable_range(buf_addr, len) {
        return (-14i64) as u64;
    }
    let slice = unsafe { core::slice::from_raw_parts(buf_addr as *const u8, len) };
    crate::serial::write_bytes(slice);
    len as u64
}

pub fn init() {
    unsafe {
        Efer::update(|e| {
            *e |= EferFlags::SYSTEM_CALL_EXTENSIONS;
        });
    }
    let s = &crate::gdt::GDT.1;
    Star::write(
        s.user_code_segment,
        s.user_data_segment,
        s.kernel_code_segment,
        s.kernel_data_segment,
    )
    .expect("IA32_STAR layout for SYSCALL/SYSRET");
    LStar::write(VirtAddr::new(syscall_entry as *const () as u64));
    SFMask::write(RFlags::INTERRUPT_FLAG | RFlags::DIRECTION_FLAG);
}
