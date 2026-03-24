//! Minimal **x86-64 ELF** loader (`ET_EXEC` / `ET_DYN` + bias) using the shared kernel page table.

use x86_64::structures::paging::{
    FrameAllocator, Mapper, Page, PageSize, PageTableFlags, PhysFrame, Size4KiB, Translate,
};
use x86_64::VirtAddr;

const ELFMAG: &[u8; 4] = b"\x7FELF";

/// Loads **PT_LOAD** segments; returns **`e_entry + bias`**. Updates [`crate::user::USER_IMAGE_END`].
pub fn load_elf(image: &[u8], bias: u64) -> Result<u64, &'static str> {
    if image.len() < 64 {
        return Err("truncated");
    }
    if &image[0..4] != ELFMAG {
        return Err("bad magic");
    }
    if image[4] != 2 {
        return Err("not 64-bit");
    }
    if image[5] != 1 {
        return Err("not little-endian");
    }
    let e_type = u16::from_le_bytes(image[16..18].try_into().unwrap());
    if e_type != 2 && e_type != 3 {
        return Err("not ET_EXEC/ET_DYN");
    }
    let e_machine = u16::from_le_bytes(image[18..20].try_into().unwrap());
    if e_machine != 0x3E {
        return Err("not x86-64");
    }
    let e_entry = u64::from_le_bytes(image[24..32].try_into().unwrap());
    let e_phoff = u64::from_le_bytes(image[32..40].try_into().unwrap());
    let e_phnum = u16::from_le_bytes(image[56..58].try_into().unwrap()) as usize;
    let e_phentsize = u16::from_le_bytes(image[54..56].try_into().unwrap()) as usize;
    if e_phentsize != 56 {
        return Err("unexpected phdr size");
    }

    let mut image_end = 0u64;
    let mut rw_min: Option<u64> = None;

    crate::paging::with_mapper(|mapper| {
        let mut guard = crate::memory::lock_allocator();
        let fa = guard.as_mut().ok_or("allocator")?;

        for i in 0..e_phnum {
            let off = e_phoff as usize + i * e_phentsize;
            if off + 56 > image.len() {
                return Err("phdr oob");
            }
            let hdr = &image[off..off + 56];
            let p_type = u32::from_le_bytes(hdr[0..4].try_into().unwrap());
            if p_type != 1 {
                continue;
            }
            let p_flags = u32::from_le_bytes(hdr[4..8].try_into().unwrap());
            let p_offset = u64::from_le_bytes(hdr[8..16].try_into().unwrap());
            let p_vaddr = u64::from_le_bytes(hdr[16..24].try_into().unwrap());
            let p_filesz = u64::from_le_bytes(hdr[32..40].try_into().unwrap());
            let p_memsz = u64::from_le_bytes(hdr[40..48].try_into().unwrap());
            let p_align = u64::from_le_bytes(hdr[48..56].try_into().unwrap());
            if p_align != 0 && p_align != 4096 {
                return Err("unsupported align");
            }
            if p_memsz == 0 {
                continue;
            }

            let seg = p_vaddr.wrapping_add(bias);
            let vstart = VirtAddr::new(seg);
            let vend = VirtAddr::new(seg + p_memsz);

            let mut flags = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;
            if p_flags & 2 != 0 {
                flags |= PageTableFlags::WRITABLE;
                rw_min = Some(rw_min.map_or(seg, |m| m.min(seg)));
            }
            if p_flags & 1 != 0 {
                // executable
            } else {
                flags |= PageTableFlags::NO_EXECUTE;
            }

            let first = Page::<Size4KiB>::containing_address(vstart);
            let last = Page::<Size4KiB>::containing_address(vend - 1u64);
            for page in Page::range_inclusive(first, last) {
                let frame: PhysFrame<Size4KiB> = fa.allocate_frame().ok_or("frame OOM")?;
                unsafe {
                    let kv = VirtAddr::new(crate::memory::phys_to_virt(frame.start_address()));
                    core::ptr::write_bytes(kv.as_mut_ptr::<u8>(), 0, Size4KiB::SIZE as usize);
                }
                unsafe {
                    mapper
                        .map_to(page, frame, flags, fa)
                        .map_err(|_| "map_to")?
                        .flush();
                }
            }

            let off0 = p_offset as usize;
            for i in 0..p_filesz {
                let file_off = off0 + i as usize;
                if file_off >= image.len() {
                    return Err("segment file oob");
                }
                let va = seg + i;
                let phys = mapper
                    .translate_addr(VirtAddr::new(va))
                    .ok_or("translate")?;
                let kw = VirtAddr::new(crate::memory::phys_to_virt(phys));
                unsafe {
                    *kw.as_mut_ptr::<u8>() = image[file_off];
                }
            }

            image_end = image_end.max(seg + p_memsz);
        }
        Ok::<(), &'static str>(())
    })?;

    crate::user::set_user_image_end(image_end.max(crate::user::USER_TEXT_BASE + Size4KiB::SIZE));
    crate::user::set_user_rw_image_start(rw_min.unwrap_or(
        crate::user::USER_TEXT_BASE + Size4KiB::SIZE,
    ));
    Ok(e_entry.wrapping_add(bias))
}
