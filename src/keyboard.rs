//! PS/2 keyboard via legacy controller ports (`0x60` / `0x64`) and optional PIC (`0x21`) unmask.
//!
//! Assumes **scan code set 1** (typical on PC / QEMU with default controller state).
//!
//! With [`crate::interrupts::init`], scancodes arrive on **IRQ1** into a small queue; use
//! [`pop_scancode`] or [`read_scancode`] from the main loop (after [`x86_64::instructions::hlt`]).

use spin::Mutex;
use x86_64::instructions::interrupts::without_interrupts;
use x86_64::instructions::port::Port;

/// Master PIC data port: each IRQ mask bit — `1` = IRQ **blocked**, `0` = **allowed**.
const PIC1_DATA: u16 = 0x21;
const PS2_DATA: u16 = 0x60;
const PS2_STATUS: u16 = 0x64;

const STATUS_OUT_FULL: u8 = 1;
const STATUS_AUX_DATA: u8 = 0x20;

const SCANCODE_BUF_SIZE: usize = 256;

struct ScancodeQueue {
    buf: [u8; SCANCODE_BUF_SIZE],
    head: usize,
    tail: usize,
}

impl ScancodeQueue {
    const fn new() -> Self {
        Self {
            buf: [0; SCANCODE_BUF_SIZE],
            head: 0,
            tail: 0,
        }
    }

    fn try_push(&mut self, b: u8) -> bool {
        let next = (self.tail + 1) % SCANCODE_BUF_SIZE;
        if next == self.head {
            return false;
        }
        self.buf[self.tail] = b;
        self.tail = next;
        true
    }

    fn try_pop(&mut self) -> Option<u8> {
        if self.head == self.tail {
            return None;
        }
        let b = self.buf[self.head];
        self.head = (self.head + 1) % SCANCODE_BUF_SIZE;
        Some(b)
    }
}

static SCANCODE_QUEUE: Mutex<ScancodeQueue> = Mutex::new(ScancodeQueue::new());

/// Clears **IRQ1** only (keyboard). **IRQ0** stays masked unless you handle it separately.
///
/// Prefer [`crate::interrupts::init`] if you use the **PIT** — it unmasks **IRQ0+IRQ1** together.
///
/// # Safety
/// Call only after **IRQ1** is registered in the IDT and the handler sends **EOI**.
pub unsafe fn unmask_keyboard_irq() {
    let mut pic_data = Port::<u8>::new(PIC1_DATA);
    let mask = pic_data.read();
    pic_data.write(mask & 0xFD);
}

/// Backwards-compatible alias for [`unmask_keyboard_irq`].
#[allow(dead_code)]
#[inline]
pub unsafe fn init_keyboard() {
    unmask_keyboard_irq();
}

/// IRQ1 handler path: dequeue one byte from the PS/2 controller (keyboard only).
pub fn handle_keyboard_irq() {
    unsafe {
        let mut status_port = Port::<u8>::new(PS2_STATUS);
        let mut data_port = Port::<u8>::new(PS2_DATA);
        let status = status_port.read();
        if status & STATUS_OUT_FULL == 0 {
            return;
        }
        let b = data_port.read();
        if status & STATUS_AUX_DATA != 0 {
            return;
        }
        let mut q = SCANCODE_QUEUE.lock();
        let _ = q.try_push(b);
    }
}

/// Pop the next scancode enqueued by the keyboard ISR. Uses `cli` so it does not race the IRQ.
pub fn pop_scancode() -> Option<u8> {
    without_interrupts(|| {
        let mut q = SCANCODE_QUEUE.lock();
        q.try_pop()
    })
}

/// IRQ queue first, then legacy **polling** of `0x60` (works even if IRQ is masked).
pub fn read_scancode() -> Option<u8> {
    if let Some(b) = pop_scancode() {
        return Some(b);
    }
    read_scancode_poll_port()
}

fn read_scancode_poll_port() -> Option<u8> {
    unsafe {
        let mut status_port = Port::<u8>::new(PS2_STATUS);
        let mut data_port = Port::<u8>::new(PS2_DATA);

        let status = status_port.read();
        if status & STATUS_OUT_FULL == 0 {
            return None;
        }
        if status & STATUS_AUX_DATA != 0 {
            let _ = data_port.read();
            return None;
        }

        Some(data_port.read())
    }
}

/// PS/2 scan code **set 1**, **make** code → **unshifted** US 104-key layout character.
///
/// Returns [`None`] for:
/// - Break codes (`>= 0x80`), extended prefix `0xE0`, or the second byte after `0xE0`
/// - Modifiers (Shift/Ctrl/Alt), Caps/Num/Scroll lock, F1–F12, Win/Menu (if present)
/// - Keys with no single Unicode scalar (arrows/Home/etc. usually need the `0xE0` sequence)
///
/// Numpad keys are mapped to their **digit/symbol** labels (`'0'`…`'9'`, `'+'`, `'-'`, `'*'`, `'.'`).
pub fn scancode_to_char(scancode: u8) -> Option<char> {
    if scancode >= 0x80 || scancode == 0xE0 {
        return None;
    }

    match scancode {
        // Escape & whitespace / editing
        0x01 => Some('\x1b'),
        0x0E => Some('\x08'), // Backspace
        0x0F => Some('\t'),
        0x1C => Some('\n'), // Enter (main keyboard)
        0x39 => Some(' '),

        // Top row: `1234567890-=`
        0x02 => Some('1'),
        0x03 => Some('2'),
        0x04 => Some('3'),
        0x05 => Some('4'),
        0x06 => Some('5'),
        0x07 => Some('6'),
        0x08 => Some('7'),
        0x09 => Some('8'),
        0x0A => Some('9'),
        0x0B => Some('0'),
        0x0C => Some('-'),
        0x0D => Some('='),

        // QWERTY row: `qwertyuiop[]`
        0x10 => Some('q'),
        0x11 => Some('w'),
        0x12 => Some('e'),
        0x13 => Some('r'),
        0x14 => Some('t'),
        0x15 => Some('y'),
        0x16 => Some('u'),
        0x17 => Some('i'),
        0x18 => Some('o'),
        0x19 => Some('p'),
        0x1A => Some('['),
        0x1B => Some(']'),

        // ASDF row: `asdfghjkl;'`
        0x1E => Some('a'),
        0x1F => Some('s'),
        0x20 => Some('d'),
        0x21 => Some('f'),
        0x22 => Some('g'),
        0x23 => Some('h'),
        0x24 => Some('j'),
        0x25 => Some('k'),
        0x26 => Some('l'),
        0x27 => Some(';'),
        0x28 => Some('\''),

        // Backtick (US), backslash (US ANSI between Enter and Backspace)
        0x29 => Some('`'),
        0x2B => Some('\\'),

        // ZXCV row: `zxcvbnm,./`
        0x2C => Some('z'),
        0x2D => Some('x'),
        0x2E => Some('c'),
        0x2F => Some('v'),
        0x30 => Some('b'),
        0x31 => Some('n'),
        0x32 => Some('m'),
        0x33 => Some(','),
        0x34 => Some('.'),
        0x35 => Some('/'),

        // Numpad (set 1; same scancodes as “duplicate” nav keys — we expose the printed label)
        0x37 => Some('*'),
        0x4A => Some('-'),
        0x4E => Some('+'),
        0x47 => Some('7'),
        0x48 => Some('8'),
        0x49 => Some('9'),
        0x4B => Some('4'),
        0x4C => Some('5'),
        0x4D => Some('6'),
        0x4F => Some('1'),
        0x50 => Some('2'),
        0x51 => Some('3'),
        0x52 => Some('0'),
        0x53 => Some('.'),

        // Modifiers & toggles (no character)
        0x1D | 0x2A | 0x36 | 0x38 | 0x3A | 0x45 | 0x46 => None,

        // F1 = 0x3B … F10 = 0x44, F11 = 0x57, F12 = 0x58
        0x3B..=0x44 | 0x57 | 0x58 => None,

        // Win / menu / power (common on newer AT keyboards), unused / ISO extras
        0x5B | 0x5C | 0x5D | 0x5E | 0x5F => None,

        _ => None,
    }
}
