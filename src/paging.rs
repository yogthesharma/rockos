//! Active **P4** from [`Cr3`](x86_64::registers::control::Cr3), wrapped as [`OffsetPageTable`].
//!
//! ## Virtual layout (this kernel)
//! With [`bootloader::BootInfo::physical_memory_offset`](bootloader::BootInfo), the bootloader
//! maps **all physical RAM** at `virt = phys + offset`. Page-table pages are edited through that
//! window; normal kernel data can use the same `phys_to_virt` addresses.
//!
//! The **low canonical half** (including identity `0..` where the bootloader typically maps the
//! kernel) may additionally be identity-mapped depending on bootloader setup; our VGA access uses
//! the offset-mapped address so it stays consistent when the heap and other regions live high.
//!
//! ## MMIO vs port I/O
//! - **VGA text buffer** at physical `0xB8000` is memory-mapped. [`init`](init) idempotently
//!   ensures a 4 KiB mapping at `phys_to_virt(0xB8000)`.
//! - **COM1** at I/O port `0x3F8` is **not** MMIO; [`crate::serial`](crate::serial) uses `in`/`out`.

use spin::Mutex;
use x86_64::registers::control::Cr3;
use x86_64::structures::paging::page_table::PageTable;
use x86_64::structures::paging::{
    FrameAllocator, FrameDeallocator, Mapper, OffsetPageTable, Page, PageTableFlags, PhysFrame,
    Size4KiB,
};
use x86_64::structures::paging::mapper::{MapToError, MapperFlush, UnmapError};
use x86_64::{PhysAddr, VirtAddr};

/// Serialize all page-table edits: one `OffsetPageTable` at a time.
static PAGING_LOCK: Mutex<()> = Mutex::new(());

/// Run `f` with a fresh [`OffsetPageTable`] built from the current [`Cr3::read`] P4.
pub fn with_mapper<R>(f: impl FnOnce(&mut OffsetPageTable<'_>) -> R) -> R {
    let _guard = PAGING_LOCK.lock();
    let offset = VirtAddr::new(crate::memory::physical_memory_offset());
    let (l4_frame, _) = Cr3::read();
    let l4_virt = VirtAddr::new(crate::memory::phys_to_virt(l4_frame.start_address()));
    let l4_table = unsafe { &mut *l4_virt.as_mut_ptr::<PageTable>() };
    let mut mapper = unsafe { OffsetPageTable::new(l4_table, offset) };
    f(&mut mapper)
}

/// Map virtual `virt` to physical `phys` as one 4 KiB page. Uses `alloc` for new page-table levels.
///
/// # Safety
/// Caller must ensure the mapping cannot create unsound aliases or violate device requirements.
pub unsafe fn map_to_phys_4k<A: FrameAllocator<Size4KiB> + ?Sized>(
    mapper: &mut OffsetPageTable<'_>,
    alloc: &mut A,
    virt: VirtAddr,
    phys: PhysAddr,
    flags: PageTableFlags,
) -> Result<MapperFlush<Size4KiB>, MapToError<Size4KiB>> {
    let page = Page::<Size4KiB>::containing_address(virt);
    let frame = PhysFrame::<Size4KiB>::containing_address(phys);
    unsafe { mapper.map_to(page, frame, flags, alloc) }
}

/// Remove a 4 KiB mapping. Does **not** return frames to the bitmap; see [`unmap_4k_and_free_bitmap`].
#[allow(dead_code)]
pub fn unmap_4k(
    mapper: &mut OffsetPageTable<'_>,
    virt: VirtAddr,
) -> Result<(PhysFrame<Size4KiB>, MapperFlush<Size4KiB>), UnmapError> {
    let page = Page::<Size4KiB>::containing_address(virt);
    mapper.unmap(page)
}

/// Unmap and return the backing **data** frame to the bitmap allocator.
///
/// Only safe if that frame was obtained from [`crate::memory::allocate_frame`] for this page
/// (not MMIO or kernel image pages).
#[allow(dead_code)]
pub fn unmap_4k_and_free_bitmap(
    mapper: &mut OffsetPageTable<'_>,
    bitmap: &mut crate::memory::BitmapFrameAllocator,
    virt: VirtAddr,
) -> Result<(), UnmapError> {
    let (frame, flush) = unmap_4k(mapper, virt)?;
    flush.flush();
    unsafe {
        bitmap.deallocate_frame(frame);
    }
    Ok(())
}

fn ensure_vga_text_buffer_mapped(
    mapper: &mut OffsetPageTable<'_>,
    fa: &mut crate::memory::BitmapFrameAllocator,
) {
    let phys = PhysAddr::new(0xb8000);
    let virt = VirtAddr::new(crate::memory::phys_to_virt(phys));
    let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE;
    match unsafe { map_to_phys_4k(mapper, fa, virt, phys, flags) } {
        Ok(fl) => fl.flush(),
        Err(MapToError::PageAlreadyMapped(_)) => {}
        Err(e) => panic!("VGA 0xB8000 map failed: {:?}", e),
    }
}

/// After [`crate::memory::init`](crate::memory::init), attach to CR3 and ensure MMIO coverage for VGA.
pub fn init() {
    with_mapper(|mapper| {
        let mut guard = crate::memory::lock_allocator();
        let fa = guard
            .as_mut()
            .expect("memory::init must run before paging::init");
        ensure_vga_text_buffer_mapped(mapper, fa);
    });
}
