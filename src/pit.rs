//! Intel 8253/8254 **Programmable Interval Timer** — channel 0 drives **IRQ0** (PIC).
//!
//! Default setup: **mode 2** (rate generator), divisor chosen for [`TIMER_HZ`].

use x86_64::instructions::port::Port;

const CHANNEL0_DATA: u16 = 0x40;
const COMMAND: u16 = 0x43;

/// PIT input crystal frequency (Hz) used in IBM PC compatibles.
pub const PIT_BASE_FREQUENCY_HZ: u32 = 1_193_182;

/// Target timer interrupt rate (Hz). Divisor rounded; actual rate ≈ [`PIT_BASE_FREQUENCY_HZ`] / divisor.
pub const TIMER_HZ: u32 = 100;

/// Divisor for channel 0: `1193182 / TIMER_HZ` (integer division).
pub const DEFAULT_TIMER_DIVISOR: u16 = (PIT_BASE_FREQUENCY_HZ / TIMER_HZ) as u16;

/// Select channel 0, lobyte then hibyte, mode 2 (rate generator), binary count.
const CMD_CHANNEL0_RATE_GEN: u8 = 0x34;

/// Program **channel 0** with the given **16-bit divisor** (1 = fastest, 0 = 65536).
///
/// Call after [`crate::interrupts::init_pic`] remaps the PIC but **before** unmasking **IRQ0**,
/// so the first tick does not go to a missing handler.
pub fn init(divisor: u16) {
    unsafe {
        let mut cmd = Port::<u8>::new(COMMAND);
        let mut data = Port::<u8>::new(CHANNEL0_DATA);
        cmd.write(CMD_CHANNEL0_RATE_GEN);
        data.write((divisor & 0xFF) as u8);
        data.write((divisor >> 8) as u8);
    }
}
