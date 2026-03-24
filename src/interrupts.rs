//! 8259 PIC remap, IDT, and hardware interrupt handlers.

use core::sync::atomic::{AtomicU64, Ordering};

use spin::Lazy;
use x86_64::instructions::port::Port;
use x86_64::registers::control::Cr2;
use x86_64::structures::idt::{
    InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode,
};

/// Monotonic counter incremented on each **IRQ0** (PIT) interrupt.
///
/// Rate is set by [`crate::pit::TIMER_HZ`] (default ~100 Hz).
pub static TICKS: AtomicU64 = AtomicU64::new(0);

/// Snapshot of [`TICKS`] (relaxed ordering; fine for wall-clock-ish delays).
#[inline]
pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

/// Block until at least `n` more timer ticks have passed (uses [`hlt`] so the CPU idles).
pub fn wait_ticks(n: u64) {
    let start = ticks();
    while ticks().saturating_sub(start) < n {
        x86_64::instructions::hlt();
    }
}

const PIC1_CMD: u16 = 0x20;
const PIC1_DATA: u16 = 0x21;
const PIC2_CMD: u16 = 0xA0;
const PIC2_DATA: u16 = 0xA1;

/// IRQ0 → vector `0x20` after remap (master PIC offset).
pub const PIC_1_OFFSET: u8 = 0x20;

#[repr(u8)]
#[derive(Clone, Copy)]
pub enum InterruptIndex {
    Timer = PIC_1_OFFSET,
    Keyboard = PIC_1_OFFSET + 1,
}

impl InterruptIndex {
    pub const fn as_usize(self) -> usize {
        self as usize
    }
}

fn io_wait() {
    unsafe {
        Port::<u8>::new(0x80).write(0);
    }
}

/// Remap master/slave PICs to vectors `0x20`–`0x2F` and mask all IRQ lines.
pub unsafe fn init_pic() {
    let mut cmd1 = Port::<u8>::new(PIC1_CMD);
    let mut data1 = Port::<u8>::new(PIC1_DATA);
    let mut cmd2 = Port::<u8>::new(PIC2_CMD);
    let mut data2 = Port::<u8>::new(PIC2_DATA);

    // ICW1: init + ICW4 needed
    cmd1.write(0x11);
    io_wait();
    cmd2.write(0x11);
    io_wait();

    // ICW2: vector offsets
    data1.write(PIC_1_OFFSET);
    io_wait();
    data2.write(PIC_1_OFFSET + 8);
    io_wait();

    // ICW3: master has slave on IRQ2; slave has cascade identity
    data1.write(0b0000_0100);
    io_wait();
    data2.write(2);
    io_wait();

    // ICW4: 8086 mode
    data1.write(0x01);
    io_wait();
    data2.write(0x01);
    io_wait();

    // Mask everything until we enable specific IRQs
    data1.write(0xFF);
    io_wait();
    data2.write(0xFF);
    io_wait();
}

pub static IDT: Lazy<InterruptDescriptorTable> = Lazy::new(|| {
    let mut idt = InterruptDescriptorTable::new();

    idt.breakpoint.set_handler_fn(breakpoint_handler);
    unsafe {
        idt.double_fault
            .set_handler_fn(double_fault_handler)
            .set_stack_index(crate::gdt::DOUBLE_FAULT_IST_INDEX);
    }
    idt.page_fault.set_handler_fn(page_fault_handler);
    idt.general_protection_fault.set_handler_fn(gpf_handler);
    idt.divide_error.set_handler_fn(divide_error_handler);

    idt[InterruptIndex::Timer.as_usize()].set_handler_fn(timer_interrupt_handler);
    idt[InterruptIndex::Keyboard.as_usize()].set_handler_fn(keyboard_interrupt_handler);

    idt
});

extern "x86-interrupt" fn breakpoint_handler(_frame: InterruptStackFrame) {}

extern "x86-interrupt" fn divide_error_handler(_frame: InterruptStackFrame) {
    loop {
        x86_64::instructions::hlt();
    }
}

extern "x86-interrupt" fn double_fault_handler(
    frame: InterruptStackFrame,
    error_code: u64,
) -> ! {
    x86_64::instructions::interrupts::disable();
    crate::serial::write_bytes(b"\r\n!! double fault err=");
    crate::serial::debug_hex_u64(error_code);
    crate::serial::write_bytes(b" rip=");
    crate::serial::debug_hex_u64(frame.instruction_pointer.as_u64());
    crate::serial::write_bytes(b"\r\n");
    loop {
        x86_64::instructions::hlt();
    }
}

extern "x86-interrupt" fn page_fault_handler(frame: InterruptStackFrame, error: PageFaultErrorCode) {
    x86_64::instructions::interrupts::disable();
    crate::serial::write_bytes(b"\r\n!! #PF cr2=");
    crate::serial::debug_hex_u64(Cr2::read().as_u64());
    crate::serial::write_bytes(b" err=");
    crate::serial::debug_hex_u64(error.bits() as u64);
    crate::serial::write_bytes(b" rip=");
    crate::serial::debug_hex_u64(frame.instruction_pointer.as_u64());
    crate::serial::write_bytes(b"\r\n");
    loop {
        x86_64::instructions::hlt();
    }
}

extern "x86-interrupt" fn gpf_handler(frame: InterruptStackFrame, error: u64) {
    x86_64::instructions::interrupts::disable();
    crate::serial::write_bytes(b"\r\n!! #GP err=");
    crate::serial::debug_hex_u64(error);
    crate::serial::write_bytes(b" rip=");
    crate::serial::debug_hex_u64(frame.instruction_pointer.as_u64());
    crate::serial::write_bytes(b"\r\n");
    loop {
        x86_64::instructions::hlt();
    }
}

extern "x86-interrupt" fn timer_interrupt_handler(_frame: InterruptStackFrame) {
    TICKS.fetch_add(1, Ordering::Relaxed);
    unsafe {
        Port::<u8>::new(PIC1_CMD).write(0x20);
    }
}

extern "x86-interrupt" fn keyboard_interrupt_handler(_frame: InterruptStackFrame) {
    crate::keyboard::handle_keyboard_irq();
    unsafe {
        Port::<u8>::new(PIC1_CMD).write(0x20);
    }
}

/// Loads IDT, programs **PIT channel 0**, unmasks **IRQ0** (timer) + **IRQ1** (keyboard), then `sti`.
pub fn init() {
    unsafe {
        init_pic();
    }
    crate::pit::init(crate::pit::DEFAULT_TIMER_DIVISOR);
    IDT.load();
    unsafe {
        let mut pic_data = Port::<u8>::new(PIC1_DATA);
        // `0xFC`: clear bits 0 and 1 → unmask **IRQ0** (PIT) and **IRQ1** (keyboard)
        let mask = pic_data.read() & 0xFC;
        pic_data.write(mask);
    }
    x86_64::instructions::interrupts::enable();
}
