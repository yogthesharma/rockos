//! Minimal ring-3 `init`: `exit(0)` via Linux syscall 60 (matches rockos).

#![no_std]
#![no_main]

use core::arch::asm;
use core::panic::PanicInfo;

#[no_mangle]
pub extern "C" fn _start() -> ! {
    unsafe {
        asm!(
            "mov eax, 60",
            "xor edi, edi",
            "syscall",
            options(nostack, noreturn),
        );
    }
}

#[panic_handler]
fn panic(_: &PanicInfo) -> ! {
    loop {}
}
