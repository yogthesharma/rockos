//! Minimal **ring 3** task: mapped text + stack, entered with `iretq`, **syscalls** for I/O.
//!
//! Virtual layout (classic “Unix-ish” low userspace):
//! - **Text** `0x400000` — one executable user page.
//! - **Stack** `0x401000..0x402000` — grows down; `RSP` starts at [`USER_STACK_TOP`].

use core::ptr::copy_nonoverlapping;
use x86_64::structures::paging::{FrameAllocator, Mapper, Page, PageSize, PageTableFlags, Size4KiB};
use x86_64::{PhysAddr, VirtAddr};

/// User text virtual base (4 KiB).
pub const USER_TEXT_BASE: u64 = 0x400000;
/// One page stack, top just past high end.
pub const USER_STACK_TOP: u64 = 0x402000;

/// Hand-built ring-3 demo: `write(1, msg, 12)` then `hlt` loop (`syscall` / Linux `write`=1).
const USER_BLOB: &[u8] = &[
    0x31, 0xc0, 0xff, 0xc0, // xor eax,eax; inc eax  -> SYS_WRITE = 1
    0x48, 0x8d, 0x35, 0x0f, 0x00, 0x00,
    0x00, // lea rsi,[rip+0x0f]  -> string below
    0x48, 0x31, 0xd2, // xor rdx,rdx
    0x48, 0x83, 0xc2, 0x0c, // add rdx,12
    0x48, 0x31, 0xff, // xor rdi,rdi (fd)
    0x0f, 0x05, // syscall
    0xf4,       // hlt
    0xeb, 0xfe, // jmp .-0  (spin on hlt)
    // "userland!\n"
    b'u', b's', b'e', b'r', b'l', b'a', b'n', b'd', b'!', b'\n', 0, 0,
];

/// Returns true if `[addr, addr+len)` lies entirely inside the mapped user text or stack page.
pub fn user_region_is_readable_range(addr: u64, len: usize) -> bool {
    let Some(end) = addr.checked_add(len as u64) else {
        return false;
    };
    let text_lo = USER_TEXT_BASE;
    let text_hi = USER_TEXT_BASE + Size4KiB::SIZE;
    let stack_lo = USER_STACK_TOP - Size4KiB::SIZE;
    let stack_hi = USER_STACK_TOP;
    let in_text = addr >= text_lo && end <= text_hi;
    let in_stack = addr >= stack_lo && end <= stack_hi;
    in_text || in_stack
}

/// Writable user pages: only the **stack** (code is RX).
pub fn user_region_is_writable_range(addr: u64, len: usize) -> bool {
    let Some(end) = addr.checked_add(len as u64) else {
        return false;
    };
    let stack_lo = USER_STACK_TOP - Size4KiB::SIZE;
    let stack_hi = USER_STACK_TOP;
    addr >= stack_lo && end <= stack_hi
}

fn copy_blob_to_frame(frame: PhysAddr) {
    let kv = VirtAddr::new(crate::memory::phys_to_virt(frame));
    assert!(
        USER_BLOB.len() <= Size4KiB::SIZE as usize,
        "user blob fits one page"
    );
    unsafe {
        copy_nonoverlapping(
            USER_BLOB.as_ptr(),
            kv.as_mut_ptr(),
            USER_BLOB.len(),
        );
    }
}

/// Allocate and map user text + stack; copy [`USER_BLOB`] into the text frame.
pub fn map_and_load() {
    crate::paging::with_mapper(|mapper| {
        let mut guard = crate::memory::lock_allocator();
        let fa = guard.as_mut().expect("bitmap allocator");
        let code_frame = fa.allocate_frame().expect("user code frame");
        let stack_frame = fa.allocate_frame().expect("user stack frame");
        copy_blob_to_frame(code_frame.start_address());

        let code_page = Page::containing_address(VirtAddr::new(USER_TEXT_BASE));
        unsafe {
            mapper
                .map_to(
                    code_page,
                    code_frame,
                    PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE,
                    fa,
                )
                .expect("map user text")
                .flush();
        }
        let stack_page = Page::containing_address(VirtAddr::new(USER_STACK_TOP - 1));
        unsafe {
            mapper
                .map_to(
                    stack_page,
                    stack_frame,
                    PageTableFlags::PRESENT
                        | PageTableFlags::WRITABLE
                        | PageTableFlags::USER_ACCESSIBLE
                        | PageTableFlags::NO_EXECUTE,
                    fa,
                )
                .expect("map user stack")
                .flush();
        }
    });
}

/// `RFLAGS` for ring 3: **IF clear** — no timer/keyboard IRQs while in user (avoids IRQ-from-ring-3
/// resets while `TSS.RSP0` / handlers are still being debugged). Bit 1 must stay set (Intel).
const USER_RFLAGS_IRQS_OFF: u64 = 0x2;
/// Enter ring 3 with **interrupts disabled** in user (`IF=0`). Default while stabilizing the VM.
#[inline]
pub unsafe fn enter_via_iret() -> ! {
    enter_via_iret_with_rflags(USER_RFLAGS_IRQS_OFF)
}

/// Enter ring 3 with **interrupts enabled** in user (`IF=1`) — use after QEMU `-d` shows no spurious resets.
#[inline]
#[allow(dead_code)]
pub unsafe fn enter_via_iret_irqs_on() -> ! {
    enter_via_iret_with_rflags(0x202)
}

pub unsafe fn enter_via_iret_with_rflags(user_rflags: u64) -> ! {
    let gdt = &crate::gdt::GDT.1;
    let cs = gdt.user_code_segment;
    let ss = gdt.user_data_segment;
    core::arch::asm!(
        "push {ss}",
        "push {user_rsp}",
        "push {user_rflags}",
        "push {cs}",
        "push {user_rip}",
        "iretq",
        ss = in(reg) ss.0 as u64,
        user_rsp = in(reg) USER_STACK_TOP,
        user_rflags = in(reg) user_rflags,
        cs = in(reg) cs.0 as u64,
        user_rip = in(reg) USER_TEXT_BASE,
        options(noreturn),
    );
}
