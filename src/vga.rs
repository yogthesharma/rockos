//! VGA text mode (80×25): each screen cell is two bytes — ASCII character + color attribute.
//! The hardware reads this memory and draws characters; we write directly to RAM at `0xB8000`.

use core::cell::UnsafeCell;
use core::fmt;

/// Wraps the raw screen memory layout: 80 columns × 25 rows, each cell is `[char, attribute]`.
/// `#[repr(transparent)]` means `VgaBuffer` has the same memory layout as the inner array
/// (no extra padding or discriminator), which matches what lives at `0xB8000`.
#[repr(transparent)]
pub struct VgaBuffer([[u8; 2]; 80 * 25]);

/// Cursor position + color, plus a mutable handle to the hardware buffer.
pub struct VgaWriter {
    col: usize,
    row: usize,
    /// High nibble = background, low nibble = foreground (e.g. `0x0F` = white on black).
    color: u8,
    buffer: &'static mut VgaBuffer,
}

impl VgaWriter {
    pub fn new(buffer: &'static mut VgaBuffer) -> Self {
        Self {
            col: 0,
            row: 0,
            color: 0x0f,
            buffer,
        }
    }

    /// Writes one byte at the current cursor; wraps at 80 columns or on `\n`.
    /// After row 24, the cursor wraps to row 0 (overwrites the top of the screen — no scroll yet).
    pub fn write_byte(&mut self, byte: u8) {
        match byte {
            b'\n' => self.new_line(),
            byte => {
                if self.col >= 80 {
                    self.new_line();
                }

                let row = self.row;
                let col = self.col;
                // Flat buffer: one `[char, attr]` per cell, row-major (same as video RAM layout).
                let i = row * 80 + col;

                self.buffer.0[i] = [byte, self.color];
                self.col += 1;
            }
        }
    }

    fn new_line(&mut self) {
        self.col = 0;
        self.row += 1;
        if self.row >= 25 {
            self.row = 0;
        }
    }
}

/// Lets you use `write!(&mut writer, "text {}", x)` — Rust's formatting trait.
///
/// Note: `write_str` feeds **UTF-8 bytes** to `write_byte`. ASCII is fine; other Unicode will show
/// as multiple VGA cells (often wrong glyphs) unless you add proper decoding later.
impl fmt::Write for VgaWriter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for byte in s.bytes() {
            self.write_byte(byte);
        }
        Ok(())
    }
}

/// Returns a reference to the VGA memory-mapped region.
///
/// # Safety
/// Caller must ensure `0xB8000` is valid for this size (true on PC VGA text mode). A bad pointer
/// would corrupt memory or fault.
pub fn get_vga_buffer() -> &'static mut VgaBuffer {
    let virt = crate::memory::phys_to_virt(x86_64::PhysAddr::new(0xb8000));
    unsafe { &mut *(virt as *mut VgaBuffer) }
}

// ---------------------------------------------------------------------------
// Global writer for `print!` / `println!`
// ---------------------------------------------------------------------------
// `static mut` + `&mut` triggers Rust 2024 `static_mut_refs` warnings and is easy to misuse.
// `UnsafeCell<Option<...>>` is the usual pattern for interior mutability in a `static`.
//
// **Assumption:** only one CPU runs this code (no concurrent `print!` from interrupts yet).
// If you later print from ISRs, protect this with a spinlock or disable interrupts around writes.
//
// Do not call [`get_vga_buffer`] and mutate the buffer elsewhere while using this global writer:
// that would alias `&mut` and is undefined behavior.

struct WriterSlot(UnsafeCell<Option<VgaWriter>>);

// SAFETY: We only mutate through `UnsafeCell` APIs below; the kernel is treated as single-threaded.
unsafe impl Sync for WriterSlot {}

static WRITER: WriterSlot = WriterSlot(UnsafeCell::new(None));

/// Call once from `kernel_main` before using `print!` / `println!`.
/// Installs the VGA buffer into the global writer slot.
pub fn init() {
    {
        let b = get_vga_buffer();
        // High-contrast fill so the QEMU window is obviously alive (not confused with an empty black window).
        for cell in b.0.iter_mut() {
            *cell = [b' ', 0x1f];
        }
    }
    unsafe {
        *WRITER.0.get() = Some(VgaWriter::new(get_vga_buffer()));
    }
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    use core::fmt::Write;
    unsafe {
        let slot = &mut *WRITER.0.get();
        if let Some(writer) = slot.as_mut() {
            let _ = writer.write_fmt(args);
        }
    }
    // Mirror to COM1 (`format_args!` is [`Copy`]).
    crate::serial::write_fmt(args);
}

/// Like `_print`, then appends a newline (matches `println!` behavior).
#[doc(hidden)]
pub fn _println(args: fmt::Arguments) {
    use core::fmt::Write;
    unsafe {
        let slot = &mut *WRITER.0.get();
        if let Some(writer) = slot.as_mut() {
            let _ = writer.write_fmt(args);
            let _ = writer.write_str("\n");
        }
    }
    crate::serial::write_line_fmt(args);
}

// `#[macro_export]` places these macros at the **crate root** (`crate::print!`), not `vga::print!`.
// In the transcriber, `$($arg)*` repeats every token tree captured by `$($arg:tt)*` — *not* `$(arg)*`.

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => {
        $crate::vga::_print(format_args!($($arg)*))
    };
}

#[macro_export]
macro_rules! println {
    () => {
        $crate::vga::_println(format_args!(""))
    };
    ($($arg:tt)*) => {
        $crate::vga::_println(format_args!($($arg)*))
    };
}
