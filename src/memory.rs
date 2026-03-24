//! Physical memory from [`bootloader::BootInfo`] and a bitmap [`FrameAllocator`](FrameAllocator).
//!
//! [`PHYSICAL_MEMORY_OFFSET`] is filled when the `bootloader` dependency is built with
//! `map_physical_memory` (enabled in `Cargo.toml`). Use [`phys_to_virt`] when mapping pages.

use bootloader::bootinfo::{MemoryMap, MemoryRegionType};
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;
use x86_64::structures::paging::{
    FrameAllocator, FrameDeallocator, PageSize, PhysFrame, Size4KiB,
};
use x86_64::PhysAddr;

/// Kernel virtual address = physical + this offset (when the bootloader maps all of RAM).
pub static PHYSICAL_MEMORY_OFFSET: AtomicU64 = AtomicU64::new(0);

/// Bitmap size: **65536** frame indices → **256 MiB** of trackable 4 KiB frames.
pub const BITMAP_BYTES: usize = 8192;
pub const FRAME_BITMAP_BITS: usize = BITMAP_BYTES * 8;

static ALLOCATOR: Mutex<Option<BitmapFrameAllocator>> = Mutex::new(None);
static TOTAL_USABLE: AtomicU64 = AtomicU64::new(0);
static FREE_COUNT: AtomicU64 = AtomicU64::new(0);

pub struct BitmapFrameAllocator {
    bitmap: [u8; BITMAP_BYTES],
}

/// `bit = 1` → used, `bit = 0` → free.
#[inline]
fn mark_free(bitmap: &mut [u8; BITMAP_BYTES], idx: usize) {
    if idx >= FRAME_BITMAP_BITS {
        return;
    }
    bitmap[idx / 8] &= !(1 << (idx % 8));
}

#[inline]
fn mark_used(bitmap: &mut [u8; BITMAP_BYTES], idx: usize) {
    if idx >= FRAME_BITMAP_BITS {
        return;
    }
    bitmap[idx / 8] |= 1 << (idx % 8);
}

fn count_free(bitmap: &[u8; BITMAP_BYTES]) -> u64 {
    let mut n = 0u64;
    for idx in 0..FRAME_BITMAP_BITS {
        if bitmap[idx / 8] & (1 << (idx % 8)) == 0 {
            n += 1;
        }
    }
    n
}

impl BitmapFrameAllocator {
    fn new(memory_map: &MemoryMap) -> Self {
        let mut bitmap = [0xFFu8; BITMAP_BYTES];
        let mut total = 0u64;

        for region in memory_map.iter() {
            if region.region_type != MemoryRegionType::Usable {
                continue;
            }
            for f in region.range.start_frame_number..region.range.end_frame_number {
                let idx = f as usize;
                if idx >= FRAME_BITMAP_BITS {
                    continue;
                }
                mark_free(&mut bitmap, idx);
                total += 1;
            }
        }

        // Never hand out physical page 0 (null pointer traps).
        mark_used(&mut bitmap, 0);

        // QEMU / some firmwares mark **legacy video RAM** (0xA0000–0xBFFF) as "usable".
        // Physical **0xB8000** is the VGA text buffer; allocating it corrupts the display (flash, then black).
        const ISA_VIDEO_LO: u64 = 0xA0000;
        const ISA_VIDEO_HI: u64 = 0xC0000;
        let vga_first = (ISA_VIDEO_LO / Size4KiB::SIZE) as usize;
        let vga_end = (ISA_VIDEO_HI / Size4KiB::SIZE) as usize;
        for idx in vga_first..vga_end {
            mark_used(&mut bitmap, idx);
        }

        let free = count_free(&bitmap);
        TOTAL_USABLE.store(total, Ordering::Relaxed);
        FREE_COUNT.store(free, Ordering::Relaxed);

        Self { bitmap }
    }

    fn allocate_frame_inner(&mut self) -> Option<PhysFrame<Size4KiB>> {
        for (i, byte) in self.bitmap.iter_mut().enumerate() {
            if *byte == 0xFF {
                continue;
            }
            let bit = (!*byte).trailing_zeros();
            if bit >= 8 {
                continue;
            }
            *byte |= 1 << bit;
            let frame_idx = (i * 8) + bit as usize;
            if frame_idx >= FRAME_BITMAP_BITS {
                *byte &= !(1 << bit);
                continue;
            }
            FREE_COUNT.fetch_sub(1, Ordering::Relaxed);
            let addr = PhysAddr::new((frame_idx as u64) * Size4KiB::SIZE);
            return Some(PhysFrame::containing_address(addr));
        }
        None
    }

    fn deallocate_frame_inner(&mut self, frame: PhysFrame<Size4KiB>) {
        let idx = (frame.start_address().as_u64() / Size4KiB::SIZE) as usize;
        if idx >= FRAME_BITMAP_BITS || idx == 0 {
            return;
        }
        let byte = idx / 8;
        let bit = idx % 8;
        let mask = 1 << bit;
        if self.bitmap[byte] & mask == 0 {
            return;
        }
        self.bitmap[byte] &= !mask;
        FREE_COUNT.fetch_add(1, Ordering::Relaxed);
    }
}

unsafe impl FrameAllocator<Size4KiB> for BitmapFrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame<Size4KiB>> {
        self.allocate_frame_inner()
    }
}

impl FrameDeallocator<Size4KiB> for BitmapFrameAllocator {
    unsafe fn deallocate_frame(&mut self, frame: PhysFrame<Size4KiB>) {
        self.deallocate_frame_inner(frame);
    }
}

pub fn init(boot_info: &'static bootloader::BootInfo) {
    PHYSICAL_MEMORY_OFFSET.store(boot_info.physical_memory_offset, Ordering::Relaxed);
    *ALLOCATOR.lock() = Some(BitmapFrameAllocator::new(&boot_info.memory_map));
}

#[inline]
pub fn physical_memory_offset() -> u64 {
    PHYSICAL_MEMORY_OFFSET.load(Ordering::Relaxed)
}

/// For use when programming page tables / MMIO with an offset-mapped physical window.
#[allow(dead_code)]
#[inline]
pub fn phys_to_virt(phys: PhysAddr) -> u64 {
    phys.as_u64() + PHYSICAL_MEMORY_OFFSET.load(Ordering::Relaxed)
}

pub fn allocate_frame() -> Option<PhysFrame<Size4KiB>> {
    ALLOCATOR.lock().as_mut()?.allocate_frame_inner()
}

/// # Safety
/// `frame` must come from [`allocate_frame`] and must not be freed twice.
pub unsafe fn deallocate_frame(frame: PhysFrame<Size4KiB>) {
    if let Some(a) = ALLOCATOR.lock().as_mut() {
        a.deallocate_frame_inner(frame);
    }
}

pub fn free_frames() -> u64 {
    FREE_COUNT.load(Ordering::Relaxed)
}

pub fn total_usable_frames() -> u64 {
    TOTAL_USABLE.load(Ordering::Relaxed)
}

#[allow(dead_code)]
pub fn lock_allocator() -> spin::MutexGuard<'static, Option<BitmapFrameAllocator>> {
    ALLOCATOR.lock()
}
