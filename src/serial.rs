//! **COM1** (UART 16550 at `0x3F8`) — log to **QEMU** with `-serial stdio`.

use core::fmt;
use spin::Mutex;
use x86_64::instructions::port::Port;

/// Base I/O port for the first serial port (`COM1`) on PC/AT compatibles.
pub const COM1_BASE: u16 = 0x3F8;

pub struct SerialPort {
    data: Port<u8>,
}

impl SerialPort {
    pub const fn new(base: u16) -> Self {
        Self {
            data: Port::new(base),
        }
    }

    /// Program **8N1**, divisor for ~**38400 baud** (QEMU ignores exact rate for `stdio` but real hardware needs this).
    pub unsafe fn init(&mut self) {
        let mut int_en = Port::<u8>::new(COM1_BASE + 1);
        let mut fifo = Port::<u8>::new(COM1_BASE + 2);
        let mut line_ctrl = Port::<u8>::new(COM1_BASE + 3);
        let mut modem = Port::<u8>::new(COM1_BASE + 4);

        int_en.write(0x00);
        line_ctrl.write(0x80);
        self.data.write(0x03);
        int_en.write(0x00);
        line_ctrl.write(0x03);
        fifo.write(0xC7);
        modem.write(0x0B);
    }

    fn line_sts_ready(&self) -> bool {
        unsafe {
            let mut sts = Port::<u8>::new(COM1_BASE + 5);
            sts.read() & 0x20 != 0
        }
    }

    pub fn send_raw(&mut self, byte: u8) {
        while !self.line_sts_ready() {}
        unsafe {
            self.data.write(byte);
        }
    }
}

impl fmt::Write for SerialPort {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for b in s.bytes() {
            self.send_raw(b);
        }
        Ok(())
    }
}

pub static SERIAL1: Mutex<SerialPort> = Mutex::new(SerialPort::new(COM1_BASE));

/// Initialize COM1 before any [`write_fmt`].
pub fn init() {
    unsafe {
        SERIAL1.lock().init();
    }
}

/// Send a single raw byte (e.g. control codes); [`write_fmt`] is used by `print!` / `println!`.
#[allow(dead_code)]
#[inline]
pub fn write_byte(b: u8) {
    SERIAL1.lock().send_raw(b);
}

/// Raw bytes to COM1 (used by `write` syscall path).
pub fn write_bytes(bytes: &[u8]) {
    let mut port = SERIAL1.lock();
    for &b in bytes {
        port.send_raw(b);
    }
}

/// Lock-free friendly logging for fault handlers (no `fmt`, no `println!`).
pub fn debug_hex_u64(n: u64) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    write_byte(b'0');
    write_byte(b'x');
    if n == 0 {
        write_byte(b'0');
        return;
    }
    let mut buf = [0u8; 16];
    let mut v = n;
    let mut i = 0usize;
    while v != 0 && i < buf.len() {
        buf[i] = HEX[(v & 0xf) as usize];
        v >>= 4;
        i += 1;
    }
    while i > 0 {
        i -= 1;
        write_byte(buf[i]);
    }
}

pub fn write_fmt(args: fmt::Arguments) {
    use fmt::Write;
    let _ = SERIAL1.lock().write_fmt(args);
}

pub fn write_line_fmt(args: fmt::Arguments) {
    use fmt::Write;
    let mut port = SERIAL1.lock();
    let _ = port.write_fmt(args);
    let _ = port.write_str("\r\n");
}
