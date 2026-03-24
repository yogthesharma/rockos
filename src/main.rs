// No standard library: we're on bare metal (no OS syscalls, no heap unless we add one).
#![no_std]
// We don't use `fn main`; the bootloader jumps to a symbol we register with `entry_point!`.
#![no_main]
#![feature(abi_x86_interrupt)]
#![feature(alloc_error_handler)]

extern crate alloc;

use alloc::boxed::Box;
use bootloader::{entry_point, BootInfo};
use core::alloc::Layout;
use core::panic::PanicInfo;
use x86_64::instructions::hlt;

mod gdt;
mod heap;
mod interrupts;
mod keyboard;
mod memory;
mod paging;
mod pit;
mod serial;
mod syscall;
mod user;
mod vga;

// Registers `kernel_main` as the first Rust code the bootloader runs after setting up paging.
// The macro expands to an `_start` symbol with the right calling convention and passes `BootInfo`.
entry_point!(kernel_main);

/// Kernel entry: runs forever (`-> !`) because there is nothing to return to.
fn kernel_main(boot_info: &'static BootInfo) -> ! {
    serial::init();
    memory::init(boot_info);
    paging::init();
    heap::init();
    vga::init();

    println!("Hello welcome to rockos kernel!");
    println!(
        "Memory: phys_offset={:#x} usable_frames~{} free={} (bitmap {} frames / {} MiB)",
        memory::physical_memory_offset(),
        memory::total_usable_frames(),
        memory::free_frames(),
        memory::FRAME_BITMAP_BITS,
        memory::FRAME_BITMAP_BITS * 4096 / (1024 * 1024),
    );

    let free_before = memory::free_frames();
    let f1 = memory::allocate_frame();
    let f2 = memory::allocate_frame();
    if let (Some(a), Some(b)) = (f1, f2) {
        println!(
            "Frame alloc test: {:#x}, {:#x} (free {free_before} -> {})",
            a.start_address().as_u64(),
            b.start_address().as_u64(),
            memory::free_frames(),
        );
        unsafe {
            memory::deallocate_frame(a);
            memory::deallocate_frame(b);
        }
        println!("Frame alloc restored, free={}", memory::free_frames());
    }

    let heap_demo = Box::new(0x_DEC0DEC0DE_u64);
    println!(
        "Heap test: Box at {:#x} = {:#x} (heap {:#x} + {} pages)",
        core::ptr::from_ref(heap_demo.as_ref()) as usize,
        *heap_demo,
        heap::HEAP_START,
        heap::HEAP_PAGES,
    );
    drop(heap_demo);

    // GDT + TSS (IST for #DF) must be loaded before the IDT references those stacks.
    gdt::init();
    interrupts::init();
    // One timer IRQ so `ticks()` is non-zero before we print (optional sanity check).
    interrupts::wait_ticks(1);

    println!(
        "Timer IRQ0 ~{} Hz (ticks={}), keyboard IRQ1 …",
        pit::TIMER_HZ,
        interrupts::ticks()
    );

    println!(
        "~{} tick keyboard window, then Unix-ish ring 3 (`write`=1) demo.",
        pit::TIMER_HZ as u64 * 5
    );
    let keyboard_until = interrupts::ticks() + pit::TIMER_HZ as u64 * 5;
    while interrupts::ticks() < keyboard_until {
        while let Some(scancode) = keyboard::read_scancode() {
            match keyboard::scancode_to_char(scancode) {
                Some(c) => print!("{}", c),
                None => println!("raw 0x{:02x}", scancode),
            }
        }
        hlt();
    }

    println!("Syscall ABI: write=1, exit=60 (Linux numbers). Entering ring 3…");
    syscall::init();
    user::map_and_load();
    println!(
        "User @ text {:#x} stack {:#x}; expect serial line from user `write`:",
        user::USER_TEXT_BASE,
        user::USER_STACK_TOP
    );
    unsafe {
        user::enter_via_iret();
    }
}

/// If anything panics, we must define this handler (required in `#![no_std]` binaries).
/// For now we just hang; later you could log to serial or VGA.
#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {}
}

#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    loop {
        hlt();
    }
}
