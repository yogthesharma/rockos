//! Linux-compatible `syscall` / `sysret` ABI with **per-task kernel stacks** and scheduling tail.

use core::arch::global_asm;
use x86_64::registers::model_specific::{Efer, EferFlags, LStar, SFMask, Star};
use x86_64::registers::rflags::RFlags;
use x86_64::VirtAddr;

#[no_mangle]
pub static mut SYSCALL_USER_RSP_STASH: u64 = 0;

#[repr(C, align(4096))]
struct SyscallKernelStack([u8; 4096]);

#[no_mangle]
static mut SYSCALL_KERNEL_STACK: SyscallKernelStack = SyscallKernelStack([0u8; 4096]);

global_asm!(
    ".section .text",
    ".global syscall_entry",
    ".global syscall_user_return",
    "syscall_entry:",
    "mov [rip + SYSCALL_USER_RSP_STASH], rsp",
    "mov r14, [rip + CURRENT_KSTACK_TOP]",
    "lea rsp, [r14]",
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
    "mov rdi, rax",
    "mov rsi, rsp",
    "call syscall_tail",
    "syscall_user_return:",
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
    pub fn syscall_user_return();
}

const ENOSYS: u64 = (-38i64) as u64;

pub const SYS_READ: u64 = 0;
pub const SYS_WRITE: u64 = 1;
pub const SYS_OPEN: u64 = 2;
pub const SYS_CLOSE: u64 = 3;
pub const SYS_SCHED_YIELD: u64 = 24;
pub const SYS_NANOSLEEP: u64 = 35;
pub const SYS_BRK: u64 = 12;
pub const SYS_MMAP: u64 = 9;
pub const SYS_MUNMAP: u64 = 11;
pub const SYS_FORK: u64 = 57;
pub const SYS_EXECVE: u64 = 59;
pub const SYS_EXIT: u64 = 60;
pub const SYS_WAIT4: u64 = 61;
pub const SYS_GETPID: u64 = 39;
pub const SYS_GETPPID: u64 = 110;

const MAP_PRIVATE: u64 = 0x02;
const MAP_ANONYMOUS: u64 = 0x20;

#[no_mangle]
extern "C" fn syscall_dispatch(n: u64, arg0: u64, arg1: u64, arg2: u64) -> u64 {
    match n {
        SYS_READ => sys_read(arg0, arg1, arg2),
        SYS_WRITE => sys_write(arg0, arg1, arg2),
        SYS_OPEN => crate::fs::sys_open(arg0, arg1),
        SYS_CLOSE => crate::fs::sys_close(arg0),
        SYS_BRK => crate::process::sys_brk(arg0),
        SYS_MMAP => crate::process::sys_mmap(arg0, arg1, arg2, MAP_PRIVATE | MAP_ANONYMOUS),
        SYS_MUNMAP => crate::process::sys_munmap(arg0, arg1),
        SYS_FORK => crate::process::sys_fork(),
        SYS_EXECVE => crate::process::sys_execve(arg0, arg1, arg2),
        SYS_SCHED_YIELD => {
            crate::scheduler::note_yield();
            0
        }
        SYS_NANOSLEEP => sys_nanosleep(arg0),
        SYS_WAIT4 => sys_wait4(arg0 as i64, arg1),
        SYS_GETPID => crate::process::current_pid(),
        SYS_GETPPID => crate::process::current_ppid().unwrap_or(0),
        SYS_EXIT => exit_and_schedule(arg0 as i32),
        _ => ENOSYS,
    }
}

fn sys_read(fd: u64, buf_addr: u64, len: u64) -> u64 {
    crate::fs::sys_read(fd, buf_addr, len)
}

fn exit_and_schedule(code: i32) -> ! {
    crate::process::mark_exited(code);
    crate::println!("[syscall] exit({})", code);
    crate::scheduler::schedule_after_exit();
}

fn sys_wait4(pid: i64, wstatus: u64) -> u64 {
    if let Some((child, st)) = crate::process::wait_reap(pid) {
        if wstatus != 0 {
            if !crate::user::user_region_is_writable_range(wstatus, 4) {
                return (-14i64) as u64;
            }
            unsafe {
                (wstatus as *mut i32).write(st);
            }
        }
        return child;
    }
    (-11i64) as u64
}

fn sys_nanosleep(req: u64) -> u64 {
    if req == 0 {
        return (-14i64) as u64;
    }
    if !crate::user::user_region_is_readable_range(req, 16) {
        return (-14i64) as u64;
    }
    unsafe {
        let sec = i64::from_le_bytes(core::slice::from_raw_parts(req as *const u8, 8).try_into().unwrap());
        let nsec = i64::from_le_bytes(
            core::slice::from_raw_parts((req + 8) as *const u8, 8)
                .try_into()
                .unwrap(),
        );
        let hz = crate::pit::TIMER_HZ as i64;
        let mut ticks = sec.saturating_mul(hz);
        ticks = ticks.saturating_add((nsec * hz) / 1_000_000_000);
        let ticks = ticks.max(1) as u64;
        crate::scheduler::sleep_until_ticks(crate::interrupts::ticks().saturating_add(ticks));
    }
    0
}

fn sys_write(_fd: u64, buf_addr: u64, len: u64) -> u64 {
    if len > 8192 {
        return (-28i64) as u64;
    }
    let len = len as usize;
    if !crate::user::user_region_is_readable_range(buf_addr, len) {
        return (-14i64) as u64;
    }
    let slice = unsafe { core::slice::from_raw_parts(buf_addr as *const u8, len) };
    crate::fs::write_fd_any(_fd, slice)
}

#[no_mangle]
extern "C" fn syscall_tail(ret_val: u64, dispatch_rsp: *const u64) -> ! {
    crate::scheduler::complete_syscall_and_schedule(ret_val, dispatch_rsp);
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
