//! **ELF** loader + **exec** stack (**argv**, **envp**, **auxv**).

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::Ordering;
use x86_64::structures::paging::{
    FrameAllocator, Mapper, Page, PageSize, PageTableFlags, PhysFrame, Size4KiB, Translate,
};
use x86_64::VirtAddr;

const ELFMAG: &[u8; 4] = b"\x7FELF";

pub struct ExecArgs<'a> {
    pub argv: &'a [String],
    pub envp: &'a [String],
}

pub struct ElfLoadedMeta {
    pub entry: u64,
    pub phdr: u64,
    pub phnum: u64,
    pub phent: u64,
    pub image_end: u64,
}

const AT_NULL: u64 = 0;
const AT_PHDR: u64 = 3;
const AT_PHENT: u64 = 4;
const AT_PHNUM: u64 = 5;
const AT_PAGESZ: u64 = 6;
const AT_ENTRY: u64 = 9;

/// Same as [`load_elf`] but also returns **PHDR** metadata for **auxv**.
pub fn load_elf(image: &[u8], bias: u64) -> Result<u64, &'static str> {
    let (e, meta) = load_elf_mapped(image, bias)?;
    finish_user_meta(&meta);
    Ok(e)
}

fn finish_user_meta(meta: &ElfLoadedMeta) {
    crate::user::set_user_image_end(
        meta
            .image_end
            .max(crate::user::USER_TEXT_BASE + Size4KiB::SIZE),
    );
}

pub fn load_elf_exec(
    image: &[u8],
    bias: u64,
    args: ExecArgs<'_>,
) -> Result<(u64, u64), &'static str> {
    unmap_old_image_best_effort();
    let (entry, meta) = load_elf_mapped(image, bias)?;
    finish_user_meta(&meta);
    let rw = meta.image_end.max(crate::user::USER_TEXT_BASE + Size4KiB::SIZE);
    let rw_lo = rw_min_for_load(image, bias).unwrap_or(crate::user::USER_TEXT_BASE + Size4KiB::SIZE);
    crate::user::set_user_rw_image_start(rw_lo);
    let rsp = build_exec_stack(&meta, &args)?;
    Ok((entry, rsp))
}

fn rw_min_for_load(image: &[u8], bias: u64) -> Option<u64> {
    let e_phoff = u64::from_le_bytes(image[32..40].try_into().ok()?);
    let e_phnum = u16::from_le_bytes(image[56..58].try_into().ok()?) as usize;
    let e_phentsize = u16::from_le_bytes(image[54..56].try_into().ok()?) as usize;
    let mut rw_min: Option<u64> = None;
    for i in 0..e_phnum {
        let off = e_phoff as usize + i * e_phentsize;
        let hdr = image.get(off..off + 56)?;
        if u32::from_le_bytes(hdr[0..4].try_into().ok()?) != 1 {
            continue;
        }
        let p_flags = u32::from_le_bytes(hdr[4..8].try_into().ok()?);
        let p_vaddr = u64::from_le_bytes(hdr[16..24].try_into().ok()?);
        if p_flags & 2 != 0 {
            let seg = p_vaddr.wrapping_add(bias);
            rw_min = Some(rw_min.map_or(seg, |m: u64| m.min(seg)));
        }
    }
    rw_min
}

fn unmap_old_image_best_effort() {
    let hi = crate::user::USER_IMAGE_END.load(Ordering::Relaxed);
    let lo = crate::user::USER_TEXT_BASE;
    let _ = crate::paging::with_mapper(|mapper| {
        let mut guard = crate::memory::lock_allocator();
        let fa = guard.as_mut()?;
        let mut va = lo;
        while va < hi {
            let v = VirtAddr::new(va);
            if mapper.translate_addr(v).is_some() {
                let _ = crate::paging::unmap_4k_and_free_bitmap(mapper, fa, v);
            }
            va += Size4KiB::SIZE;
        }
        Some(())
    });
}

fn load_elf_mapped(image: &[u8], bias: u64) -> Result<(u64, ElfLoadedMeta), &'static str> {
    if image.len() < 64 {
        return Err("truncated");
    }
    if &image[0..4] != ELFMAG {
        return Err("bad magic");
    }
    if image[4] != 2 || image[5] != 1 {
        return Err("not le64");
    }
    let e_type = u16::from_le_bytes(image[16..18].try_into().unwrap());
    if e_type != 2 && e_type != 3 {
        return Err("not ET_EXEC/ET_DYN");
    }
    if u16::from_le_bytes(image[18..20].try_into().unwrap()) != 0x3E {
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
    let mut phdr_va: Option<u64> = None;

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
            if e_phoff >= p_offset && e_phoff < p_offset.saturating_add(p_filesz) {
                phdr_va = Some(seg + (e_phoff - p_offset));
            }

            let vstart = VirtAddr::new(seg);
            let vend = VirtAddr::new(seg + p_memsz);

            let mut flags = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;
            if p_flags & 2 != 0 {
                flags |= PageTableFlags::WRITABLE;
                rw_min = Some(rw_min.map_or(seg, |m: u64| m.min(seg)));
            }
            if p_flags & 1 != 0 {
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
            for j in 0..p_filesz {
                let file_off = off0 + j as usize;
                if file_off >= image.len() {
                    return Err("segment file oob");
                }
                let va = seg + j;
                let phys = mapper
                    .translate_addr(VirtAddr::new(va))
                    .ok_or("translate")?;
                let kw = VirtAddr::new(crate::memory::hex_to_virt(phys));
                unsafe {
                    *kw.as_mut_ptr::<u8>() = image[file_off];
                }
            }

            image_end = image_end.max(seg + p_memsz);
        }
        Ok::<(), &'static str>(())
    })?;

    let phdr = phdr_va.ok_or("no phdr in LOAD")?;
    crate::user::set_user_rw_image_start(rw_min.unwrap_or(
        crate::user::USER_TEXT_BASE + Size4KiB::SIZE,
    ));

    Ok((
        e_entry.wrapping_add(bias),
        ElfLoadedMeta {
            entry: e_entry.wrapping_add(bias),
            phdr,
            phnum: e_phnum as u64,
            phent: e_phentsize as u64,
            image_end,
        },
    ))
}

// typo fix - phys_to_virt not hex_to_virt
fn write_user_byte(va: u64, b: u8) -> Result<(), &'static str> {
    let phys = crate::paging::with_mapper(|m| m.translate_addr(VirtAddr::new(va)))
        .ok_or("no map")?;
    let kw = VirtAddr::new(crate::memory::phys_to_virt(phys));
    unsafe {
        *kw.as_mut_ptr::<u8>() = b;
    }
    Ok(())
}

fn write_user_slice(va: u64, data: &[u8]) -> Result<(), &'static str> {
    for (i, &b) in data.iter().enumerate() {
        write_user_byte(va + i as u64, b)?;
    }
    Ok(())
}

fn write_u64(va: u64, v: u64) -> Result<(), &'static str> {
    for i in 0..8 {
        write_user_byte(va + i, ((v >> (8 * i)) & 0xFF) as u8)?;
    }
    Ok(())
}

fn align_up(mut x: usize, a: usize) -> usize {
    if x == 0 {
        return 0;
    }
    (x + a - 1) & !(a - 1)
}

fn build_exec_stack(meta: &ElfLoadedMeta, args: &ExecArgs<'_>) -> Result<u64, &'static str> {
    let stack_lo = crate::user::USER_STACK_TOP - Size4KiB::SIZE;
    let mut layout: Vec<u8> = Vec::new();
    let mut strings: Vec<(usize, u64)> = Vec::new();

    for e in args.envp {
        let off = layout.len();
        layout.extend_from_slice(e.as_bytes());
        layout.push(0);
        while layout.len() % 8 != 0 {
            layout.push(0);
        }
        strings.push((off, 0));
    }
    for a in args.argv {
        let off = layout.len();
        layout.extend_from_slice(a.as_bytes());
        layout.push(0);
        while layout.len() % 8 != 0 {
            layout.push(0);
        }
        strings.push((off, 0));
    }

    let string_base = align_up(layout.len(), 16);
    let aux_bytes = 8 * 2 * 7;
    let argv_n = args.argv.len();
    let env_n = args.envp.len();
    let ptr_area = 8 + (argv_n + 1) * 8 + (env_n + 1) * 8 + aux_bytes + 16;
    let total = string_base + ptr_area;
    if total > Size4KiB::SIZE as usize {
        return Err("stack ovf");
    }

    layout.resize(string_base, 0);
    let blob_start = stack_lo + (Size4KiB::SIZE - total) as u64;
    if blob_start < stack_lo {
        return Err("stack ovf");
    }

    let mut write_off = 0usize;
    for (i, e) in args.envp.iter().enumerate() {
        strings[i].1 = blob_start + write_off as u64;
        write_off = align_up(write_off + e.len() + 1, 8);
    }
    let env_base_i = args.envp.len();
    for (j, a) in args.argv.iter().enumerate() {
        strings[env_base_i + j].1 = blob_start + write_off as u64;
        write_off = align_up(write_off + a.len() + 1, 8);
    }

    write_off = 0;
    for (e, _) in args.envp.iter().zip(0..) {
        write_user_slice(blob_start + write_off as u64, e.as_bytes())?;
        write_off += e.len();
        write_user_byte(blob_start + write_off as u64, 0)?;
        write_off += 1;
        write_off = align_up(write_off, 8);
    }
    for a in args.argv {
        write_user_slice(blob_start + write_off as u64, a.as_bytes())?;
        write_off += a.len();
        write_user_byte(blob_start + write_off as u64, 0)?;
        write_off += 1;
        write_off = align_up(write_off, 8);
    }

    let mut sp = crate::user::USER_STACK_TOP as i64;
    sp -= 8;
    write_u64(sp as u64, args.argv.len() as u64)?;
    for i in 0..args.argv.len() {
        sp -= 8;
        write_u64(sp as u64, strings[env_base_i + i].1)?;
    }
    sp -= 8;
    write_u64(sp as u64, 0)?;
    for i in 0..args.envp.len() {
        sp -= 8;
        write_u64(sp as u64, strings[i].1)?;
    }
    sp -= 8;
    write_u64(sp as u64, 0)?;

    let aux: &[(u64, u64)] = &[
        (0, AT_NULL),
        (meta.phent, AT_PHENT),
        (meta.phnum, AT_PHNUM),
        (Size4KiB::SIZE, AT_PAGESZ),
        (meta.phdr, AT_PHDR),
        (meta.entry, AT_ENTRY),
    ];
    for &(val, tag) in aux.iter().rev() {
        sp -= 8;
        write_u64(sp as u64, val)?;
        sp -= 8;
        write_u64(sp as u64, tag)?;
    }

    while sp % 16 != 0 {
        sp -= 8;
        write_u64(sp as u64, 0)?;
    }

    Ok(sp as u64)
}
