//! Kernel heap: high virtual range backed by frames from [`crate::memory`], [`linked_list_allocator`].
//!
//! Virtual base [`HEAP_START`] must not collide with the bootloader kernel mapping or the
//! physical-memory offset window layout.

use linked_list_allocator::LockedHeap;
use x86_64::structures::paging::{FrameAllocator, Mapper, Page, PageSize, PageTableFlags, Size4KiB};
use x86_64::VirtAddr;

/// Distinct high window; adjust if you map more device memory here.
pub const HEAP_START: u64 = 0x4444_4444_0000;
pub const HEAP_PAGES: usize = 64;

#[global_allocator]
static GLOBAL_ALLOCATOR: LockedHeap = LockedHeap::empty();

/// Map [`HEAP_PAGES`] × 4 KiB at [`HEAP_START`] and initialize the linked-list heap.
pub fn init() {
    let heap_size = HEAP_PAGES * Size4KiB::SIZE as usize;
    crate::paging::with_mapper(|mapper| {
        let mut guard = crate::memory::lock_allocator();
        let fa = guard
            .as_mut()
            .expect("memory::init must run before heap::init");
        for i in 0..HEAP_PAGES {
            let virt = VirtAddr::new(HEAP_START + (i as u64) * Size4KiB::SIZE);
            let page = Page::containing_address(virt);
            let frame = fa.allocate_frame().expect("heap backing frame OOM");
            let flush = unsafe {
                mapper
                    .map_to(
                        page,
                        frame,
                        PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE,
                        fa,
                    )
                    .expect("heap map_to")
            };
            flush.flush();
        }
    });
    unsafe {
        GLOBAL_ALLOCATOR
            .lock()
            .init(HEAP_START as *mut u8, heap_size);
    }
}
