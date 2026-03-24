//! Cooked TTY: PS/2 scan codes → ASCII (**shift**-aware), **canonical line** input for `read(0, …)`.
//!
//! A **line** is returned after **Enter** (`\n` appended). Backspace edits the current line.

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use spin::Mutex;

const LINE_BUF: usize = 1024;
const MAX_READY_LINES: usize = 128;

static TTY: Mutex<TtyState> = Mutex::new(TtyState::new());

struct TtyState {
    shift_left: bool,
    shift_right: bool,
    e0_prefix: bool,
    line_edit: Vec<u8>,
    ready_lines: VecDeque<Vec<u8>>,
}

impl TtyState {
    const fn new() -> Self {
        Self {
            shift_left: false,
            shift_right: false,
            e0_prefix: false,
            line_edit: Vec::new(),
            ready_lines: VecDeque::new(),
        }
    }

    fn shift(&self) -> bool {
        self.shift_left || self.shift_right
    }

    fn feed_scancode(&mut self, code: u8) {
        if self.e0_prefix {
            self.e0_prefix = code != 0xE0;
            if code == 0xE0 {
                return;
            }
            self.e0_prefix = false;
            return;
        }
        if code == 0xE0 {
            self.e0_prefix = true;
            return;
        }
        if code >= 0x80 {
            let mk = code - 0x80;
            match mk {
                0x2a => self.shift_left = false,
                0x36 => self.shift_right = false,
                _ => {}
            }
            return;
        }
        match code {
            0x2a => {
                self.shift_left = true;
                return;
            }
            0x36 => {
                self.shift_right = true;
                return;
            }
            _ => {}
        }
        let ch = if self.shift() {
            shifted_char(code)
        } else {
            crate::keyboard::scancode_to_char(code)
        };
        let Some(c) = ch else {
            return;
        };
        match c {
            '\n' | '\r' => {
                let mut line = core::mem::take(&mut self.line_edit);
                line.push(b'\n');
                if self.ready_lines.len() < MAX_READY_LINES {
                    self.ready_lines.push_back(line);
                }
            }
            '\x08' | '\x7f' => {
                let _ = self.line_edit.pop();
            }
            _ => {
                let mut buf = [0u8; 4];
                let n = c.encode_utf8(&mut buf).len();
                for i in 0..n {
                    if self.line_edit.len() < LINE_BUF {
                        self.line_edit.push(buf[i]);
                    }
                }
            }
        }
    }
}

fn shifted_char(code: u8) -> Option<char> {
    match code {
        0x02 => Some('!'),
        0x03 => Some('@'),
        0x04 => Some('#'),
        0x05 => Some('$'),
        0x06 => Some('%'),
        0x07 => Some('^'),
        0x08 => Some('&'),
        0x09 => Some('*'),
        0x0a => Some('('),
        0x0b => Some(')'),
        0x0c => Some('_'),
        0x0d => Some('+'),
        0x29 => Some('~'),
        0x2b => Some('|'),
        0x33 => Some('<'),
        0x34 => Some('>'),
        0x35 => Some('?'),
        _ => crate::keyboard::scancode_to_char(code).map(|c| c.to_ascii_uppercase()),
    }
}

/// Called for each keyboard IRQ byte (after [`crate::keyboard::handle_keyboard_irq`] enqueues it).
#[inline]
pub fn feed_from_irq(scancode: u8) {
    TTY.lock().feed_scancode(scancode);
}

/// Line discipline: **blocking** (until a line exists) vs **non-blocking** (`EAGAIN` = `-11`).
pub fn read(dst: &mut [u8], nonblock: bool) -> Result<usize, i32> {
    let mut g = TTY.lock();
    let Some(line) = g.ready_lines.pop_front() else {
        return if nonblock { Err(-11) } else { Ok(0) };
    };
    let n = line.len().min(dst.len());
    dst[..n].copy_from_slice(&line[..n]);
    if n < line.len() {
        g.ready_lines.push_front(line[n..].to_vec());
    }
    Ok(n)
}
