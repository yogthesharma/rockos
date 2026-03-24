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
    pub rw_min: u64,
}

const AT_NULL: u64 = 0;
const AT_PHDR: u64 = 3;
const AT_PHENT: u64 = 4;
const AT_PHNUM: u64 = 5;
const AT_PAGESZ: u64 = 6;
const AT_ENTRY: u64 = 9;

pub fn load_elf(image: &[u8], bias: u64) -> Result<u64, &'static str> {
    let (e, meta) = load_elf_mapped(image, bias)?;
    apply_user_meta(&meta);
    Ok(e)
}

fn apply_user_meta(meta: &ElfLoadedMeta) {
    crate::user::set_user_image_end(
        meta
            .image_end
            .max(crate::user::USER_TEXT_BASE + Size4KiB::SIZE),
    );
    crate::user::set_user_rw_image_start(meta.rw_min);
}

pub fn load_elf_exec(
    image: &[u8],
    bias: u64,
    args: ExecArgs<'_>,
) -> Result<(u64, u64), &'static str> {
    unmap_old_image_best_effort();
    let (entry, meta) = load_elf_mapped(image, bias)?;
    apply_user_meta(&meta);
    let rsp = build_exec_stack(&meta, &args)?;
    Ok((entry, rsp))
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
    if &image[0..4] != ELFMAG || image[4] != 2 || image[5] != 1 {
        return Err("bad elf");
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
                rw_min = Some(rw_min.map_or(seg, |m| m.min(seg)));
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
                let kw = VirtAddr::new(crate::memory::phys_to_virt(phys));
                unsafe {
                    *kw.as_mut_ptr::<u8>() = image[file_off];
                }
            }

            image_end = image_end.max(seg + p_memsz);
        }
        Ok::<(), &'static str>(())
    })?;

    let phdr = phdr_va.ok_or("no phdr in LOAD")?;
    let rw = rw_min.unwrap_or(crate::user::USER_TEXT_BASE + Size4KiB::SIZE);

    Ok((
        e_entry.wrapping_add(bias),
        ElfLoadedMeta {
            entry: e_entry.wrapping_add(bias),
            phdr,
            phnum: e_phnum as u64,
            phent: e_phentsize as u64,
            image_end,
            rw_min: rw,
        },
    ))
}

fn write_user_byte(va: u64, b: u8) -> Result<(), &'static str> {
    let phys = crate::paging::with_mapper(|m| m.translate_addr(VirtAddr::new(va)))
        .ok_or("no map")?;
    let kw = VirtAddr::new(crate::memory::phys_to_virt(phys));
    unsafe {
        *kw.as_mut_ptr::<u8>() = b;
    }
    Ok(())
}

fn align_up(x: u64, a: u64) -> u64 {
    (x + a - 1) & !(a - 1)
}

/// SysV amd64 **initial stack**: **`argc`**, `argv[]`, `0`, `envp[]`, `0`, `auxv`, then strings (ascending VA).
fn build_exec_stack(meta: &ElfLoadedMeta, args: &ExecArgs<'_>) -> Result<u64, &'static str> {
    let stack_lo = crate::user::USER_STACK_TOP - Size4KiB::SIZE;
    let aux_pairs = 6usize;
    let mut v: Vec<u8> = Vec::new();
    v.extend_from_slice(&(args.argv.len() as u64).to_le_bytes());
    let argv_tab = v.len();
    for _ in 0..args.argv.len() {
        v.extend_from_slice(&0u64.to_le_bytes());
    }
    v.extend_from_slice(&0u64.to_le_bytes());
    let env_tab = v.len();
    for _ in 0..args.envp.len() {
        v.extend_from_slice(&0u64.to_le_bytes());
    }
    v.extend_from_slice(&0u64.to_le_bytes());
    for _ in 0..aux_pairs {
        v.extend_from_slice(&0u64.to_le_bytes());
        v.extend_from_slice(&0u64.to_le_bytes());
    }
    while v.len() % 16 != 0 {
        v.push(0);
    }
    let str0 = align_up(v.len() as u64, 16) as usize;
    while v.len() < str0 {
        v.push(0);
    }
    let mut str_off: Vec<u64> = Vec::new();
    for e in args.envp {
        str_off.push(stack_lo + v.len() as u64);
        v.extend_from_slice(e.as_bytes());
        v.push(0);
        while v.len() % 8 != 0 {
            v.push(0);
        }
    }
    for a in args.argv {
        str_off.push(stack_lo + v.len() as u64);
        v.extend_from_slice(a.as_bytes());
        v.push(0);
        while v.len() % 8 != 0 {
            v.push(0);
        }
    }
    if stack_lo + v.len() as u64 > crate::user::USER_STACK_TOP {
        return Err("stack ovf");
    }
    let env_n = args.envp.len();
    for i in 0..env_n {
        let o = env_tab + i * 8;
        v[o..o + 8].copy_from_slice(&str_off[i].to_le_bytes());
    }
    for i in 0..args.argv.len() {
        let o = argv_tab + i * 8;
        v[o..o + 8].copy_from_slice(&str_off[env_n + i].to_le_bytes());
    }
    let aux: &[(u64, u64)] = &[
        (AT_PHDR, meta.phdr),
        (AT_PHENT, meta.phent),
        (AT_PHNUM, meta.phnum),
        (AT_PAGESZ, Size4KiB::SIZE),
        (AT_ENTRY, meta.entry),
        (AT_NULL, 0),
    ];
    let mut ao = env_tab + (env_n + 1) * 8;
    for &(tag, val) in aux {
        let slot = v.get_mut(ao..ao + 16).ok_or("aux ovf")?;
        slot[..8].copy_from_slice(&tag.to_le_bytes());
        slot[8..].copy_from_slice(&val.to_le_bytes());
        ao += 16;
    }
    for (i, &b) in v.iter().enumerate() {
        write_user_byte(stack_lo + i as u64, b)?;
    }
    Ok(stack_lo)
}
