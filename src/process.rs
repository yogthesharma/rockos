//! Processes: **PID**, **fork**, **brk** / **mmap** / **munmap**, **exec** (embedded ELF), syscall **saved** state.

#![allow(dead_code)]

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;
use x86_64::structures::paging::{
    FrameAllocator, Mapper, Page, PageSize, PageTableFlags, PhysFrame, Size4KiB, Translate,
};
use x86_64::VirtAddr;

use crate::task::{AlignedKstack, SyscallSaved, CHILD_STACK_LO, USER_STACK_CHILD_TOP};

pub const USER_BRK_BASE: u64 = 0x404000;
pub const USER_BRK_LIMIT: u64 = 0x410000;

pub const USER_MMAP_BASE: u64 = 0x410000;
pub const USER_MMAP_LIMIT: u64 = 0x480000;

pub const PROT_READ: u64 = 1;
pub const PROT_WRITE: u64 = 2;
pub const PROT_EXEC: u64 = 4;

static VM_AREA_NEXT_ID: AtomicU64 = AtomicU64::new(1);
static NEXT_MMAP: AtomicU64 = AtomicU64::new(USER_MMAP_BASE);

#[derive(Clone)]
pub struct VmArea {
    pub id: u64,
    pub virt_start: u64,
    pub pages: usize,
    pub writable: bool,
    pub executable: bool,
}

pub struct Process {
    pub pid: u64,
    pub parent: Option<u64>,
    pub brk_end: u64,
    pub vm_areas: Vec<VmArea>,
    pub exit_code: Option<i32>,
    pub kstack: AlignedKstack,
    pub save: SyscallSaved,
    pub pending_rax: u64,
}

impl Process {
    pub fn new(pid: u64, parent: Option<u64>, bootstrap: SyscallSaved) -> Self {
        Self {
            pid,
            parent,
            brk_end: USER_BRK_BASE,
            vm_areas: Vec::new(),
            exit_code: None,
            kstack: AlignedKstack([0u8; 4096]),
            save: bootstrap,
            pending_rax: 0,
        }
    }

    pub fn kstack_top(&self) -> u64 {
        self.kstack.top_ptr()
    }
}

static TABLE: Mutex<Option<ProcessTable>> = Mutex::new(None);
static CURRENT_SLOT: Mutex<usize> = Mutex::new(0);

struct ProcessTable {
    /// Box avoids a multi‑megabyte `Vec` of `Option<Process>` (each `Process` embeds a 4 KiB kstack).
    procs: Vec<Option<Box<Process>>>,
}

impl ProcessTable {
    fn alloc_slot(&mut self) -> Option<usize> {
        for (i, s) in self.procs.iter_mut().enumerate() {
            if s.is_none() {
                return Some(i);
            }
        }
        if self.procs.len() < 32 {
            self.procs.push(None);
            Some(self.procs.len() - 1)
        } else {
            None
        }
    }
}

pub fn init() {
    let entry = crate::user::USER_ENTRY_RIP.load(Ordering::Relaxed);
    let stack_top = crate::user::USER_STACK_TOP;
    let bootstrap = SyscallSaved::bootstrap_user(entry, stack_top, 0x202);
    let mut v = Vec::new();
    v.resize_with(32, || None);
    let init_p = Process::new(1, None, bootstrap);
    let top = init_p.kstack_top();
    v[0] = Some(Box::new(init_p));
    *TABLE.lock() = Some(ProcessTable { procs: v });
    *CURRENT_SLOT.lock() = 0;
    crate::task::set_current_kstack_top(top);
}

pub fn current_slot() -> usize {
    *CURRENT_SLOT.lock()
}

pub fn set_current_slot(s: usize) {
    *CURRENT_SLOT.lock() = s;
    if let Some(t) = TABLE.lock().as_ref() {
        if let Some(p) = t.procs.get(s).and_then(|x| x.as_deref()) {
            crate::task::set_current_kstack_top(p.kstack_top());
        }
    }
}

pub fn current_pid() -> u64 {
    with_slot(|t, i| t.procs[i].as_ref().map(|p| p.pid))
        .flatten()
        .unwrap_or(1)
}

pub fn current_ppid() -> Option<u64> {
    with_slot(|t, i| t.procs[i].as_ref().and_then(|p| p.parent))
        .flatten()
}

fn with_slot<R>(f: impl FnOnce(&ProcessTable, usize) -> R) -> Option<R> {
    let i = *CURRENT_SLOT.lock();
    let g = TABLE.lock();
    let t = g.as_ref()?;
    Some(f(t, i))
}

fn with_table_mut<R>(f: impl FnOnce(&mut ProcessTable) -> R) -> Option<R> {
    let mut g = TABLE.lock();
    let t = g.as_mut()?;
    Some(f(t))
}

pub fn kstack_top_for_slot(slot: usize) -> u64 {
    TABLE
        .lock()
        .as_ref()
        .and_then(|t| t.procs.get(slot))
        .and_then(|p| p.as_deref())
        .map(|p| p.kstack_top())
        .unwrap_or_else(|| crate::task::current_kstack_top())
}

pub fn syscall_saved_for_slot(slot: usize) -> SyscallSaved {
    TABLE
        .lock()
        .as_ref()
        .and_then(|t| t.procs.get(slot))
        .and_then(|p| p.as_deref())
        .map(|p| p.save.clone())
        .expect("saved")
}

pub fn set_syscall_saved(slot: usize, save: SyscallSaved, pending_rax: u64) {
    let _ = with_table_mut(|t| {
        if let Some(p) = t.procs.get_mut(slot).and_then(|x| x.as_mut()) {
            p.save = save;
            p.pending_rax = pending_rax;
        }
    });
}

pub fn pending_rax_for_slot(slot: usize) -> u64 {
    TABLE
        .lock()
        .as_ref()
        .and_then(|t| t.procs.get(slot))
        .and_then(|p| p.as_deref())
        .map(|p| p.pending_rax)
        .unwrap_or(0)
}

pub fn slot_is_alive_runnable(slot: usize) -> bool {
    TABLE
        .lock()
        .as_ref()
        .and_then(|t| t.procs.get(slot))
        .and_then(|p| p.as_deref())
        .map(|p| p.exit_code.is_none())
        .unwrap_or(false)
}

pub fn mark_zombie_current() {
    // `exit_code` is set in `mark_exited` before this runs.
}

pub fn mark_exited(status: i32) {
    let i = current_slot();
    let _ = with_table_mut(|t| {
        if let Some(p) = t.procs.get_mut(i).and_then(|x| x.as_mut()) {
            p.exit_code = Some(status);
        }
    });
}

// --- memory regions ---

pub fn covers_brk_heap(addr: u64, end: u64) -> bool {
    let Some(brk) = with_slot(|t, i| t.procs[i].as_ref().map(|p| p.brk_end)).flatten() else {
        return false;
    };
    addr >= USER_BRK_BASE && end <= brk && end <= USER_BRK_LIMIT
}

pub fn covers_mmap_read(addr: u64, end: u64) -> bool {
    with_slot(|t, i| {
        let p = t.procs[i].as_ref()?;
        for a in &p.vm_areas {
            let aend = a.virt_start + (a.pages as u64) * Size4KiB::SIZE;
            if addr >= a.virt_start && end <= aend {
                return Some(true);
            }
        }
        Some(false)
    })
    .flatten()
    .unwrap_or(false)
}

pub fn covers_mmap_write(addr: u64, end: u64) -> bool {
    with_slot(|t, i| {
        let p = t.procs[i].as_ref()?;
        for a in &p.vm_areas {
            if !a.writable {
                continue;
            }
            let aend = a.virt_start + (a.pages as u64) * Size4KiB::SIZE;
            if addr >= a.virt_start && end <= aend {
                return Some(true);
            }
        }
        Some(false)
    })
    .flatten()
    .unwrap_or(false)
}

pub fn sys_brk(addr: u64) -> u64 {
    with_table_mut(|t| {
        let idx = *CURRENT_SLOT.lock();
        let Some(proc) = t.procs[idx].as_mut() else {
            return (-12i64) as u64;
        };
        if addr == 0 {
            return proc.brk_end;
        }
        if addr < USER_BRK_BASE || addr > USER_BRK_LIMIT {
            return (-12i64) as u64;
        }
        let old = proc.brk_end;
        if addr < old {
            if map_brk_shrink(addr, old).is_err() {
                return (-12i64) as u64;
            }
            proc.brk_end = addr;
            return addr;
        }
        if addr == old {
            return addr;
        }
        let fl = PageTableFlags::PRESENT
            | PageTableFlags::WRITABLE
            | PageTableFlags::USER_ACCESSIBLE
            | PageTableFlags::NO_EXECUTE;
        if map_brk_grow(old, addr, fl).is_err() {
            return (-12i64) as u64;
        }
        proc.brk_end = addr;
        addr
    })
    .unwrap_or((-12i64) as u64)
}

fn map_brk_shrink(new_end: u64, old_end: u64) -> Result<(), ()> {
    if new_end >= old_end || new_end < USER_BRK_BASE {
        return Ok(());
    }
    let first_page_lo = align_up(new_end, Size4KiB::SIZE);
    if first_page_lo >= old_end {
        return Ok(());
    }
    let first = Page::<Size4KiB>::containing_address(VirtAddr::new(first_page_lo));
    let last = Page::<Size4KiB>::containing_address(VirtAddr::new(old_end.saturating_sub(1)));
    crate::paging::with_mapper(|mapper| {
        let mut guard = crate::memory::lock_allocator();
        let fa = guard.as_mut().ok_or(())?;
        for page in Page::range_inclusive(first, last) {
            if mapper.translate_addr(page.start_address()).is_none() {
                continue;
            }
            let _ = crate::paging::unmap_4k_and_free_bitmap(mapper, fa, page.start_address());
        }
        Ok::<(), ()>(())
    })
}

fn align_up(x: u64, a: u64) -> u64 {
    (x + a - 1) & !(a - 1)
}

fn map_brk_grow(old_end: u64, new_end: u64, flags: PageTableFlags) -> Result<(), ()> {
    let first = Page::<Size4KiB>::containing_address(VirtAddr::new(old_end));
    let last = Page::<Size4KiB>::containing_address(VirtAddr::new(new_end - 1));
    crate::paging::with_mapper(|mapper| {
        let mut guard = crate::memory::lock_allocator();
        let fa = guard.as_mut().ok_or(())?;
        for page in Page::range_inclusive(first, last) {
            if mapper.translate_addr(page.start_address()).is_some() {
                continue;
            }
            let frame: PhysFrame<Size4KiB> = fa.allocate_frame().ok_or(())?;
            unsafe {
                mapper
                    .map_to(page, frame, flags, fa)
                    .map_err(|_| ())?
                    .flush();
            }
        }
        Ok::<(), ()>(())
    })
}

pub fn sys_mmap(mut addr: u64, len: u64, prot: u64, _flags: u64) -> u64 {
    if len == 0 || len > USER_MMAP_LIMIT - USER_MMAP_BASE {
        return (-22i64) as u64;
    }
    let n_pages = ((len + Size4KiB::SIZE - 1) / Size4KiB::SIZE) as usize;
    let mut fl = PageTableFlags::PRESENT | PageTableFlags::USER_ACCESSIBLE;
    if prot & PROT_WRITE != 0 {
        fl |= PageTableFlags::WRITABLE;
    }
    if prot & PROT_EXEC != 0 {
        fl.remove(PageTableFlags::NO_EXECUTE);
    } else {
        fl |= PageTableFlags::NO_EXECUTE;
    }
    if addr == 0 {
        let base = NEXT_MMAP.fetch_add((n_pages as u64) * Size4KiB::SIZE, Ordering::SeqCst);
        if base.saturating_add((n_pages as u64) * Size4KiB::SIZE) > USER_MMAP_LIMIT {
            return (-12i64) as u64;
        }
        addr = base;
    } else {
        if addr % Size4KiB::SIZE != 0 {
            return (-22i64) as u64;
        }
        if addr < USER_MMAP_BASE || addr.saturating_add(len) > USER_MMAP_LIMIT {
            return (-22i64) as u64;
        }
    }
    if map_user_anon_pages(VirtAddr::new(addr), n_pages, fl).is_err() {
        return (-12i64) as u64;
    }
    let id = VM_AREA_NEXT_ID.fetch_add(1, Ordering::SeqCst);
    let idx = current_slot();
    let _ = with_table_mut(|t| {
        if let Some(p) = t.procs[idx].as_mut() {
            p.vm_areas.push(VmArea {
                id,
                virt_start: addr,
                pages: n_pages,
                writable: prot & PROT_WRITE != 0,
                executable: prot & PROT_EXEC != 0,
            });
        }
    });
    addr
}

pub fn sys_munmap(addr: u64, len: u64) -> u64 {
    if addr % Size4KiB::SIZE != 0 || len == 0 {
        return (-22i64) as u64;
    }
    let n_pages = (len / Size4KiB::SIZE) as usize;
    let end = addr.saturating_add(len);
    let removed = with_table_mut(|t| {
        let idx = *CURRENT_SLOT.lock();
        let proc = t.procs[idx].as_mut()?;
        let mut hit = false;
        proc.vm_areas.retain(|a| {
            let aend = a.virt_start + (a.pages as u64) * Size4KiB::SIZE;
            if a.virt_start == addr && aend == end {
                hit = true;
                return false;
            }
            true
        });
        Some(hit)
    })
    .flatten()
    .unwrap_or(false);
    if !removed {
        return (-22i64) as u64;
    }
    if unmap_user_range(addr, n_pages).is_err() {
        return (-12i64) as u64;
    }
    0
}

fn unmap_user_range(start: u64, page_count: usize) -> Result<(), ()> {
    crate::paging::with_mapper(|mapper| {
        let mut guard = crate::memory::lock_allocator();
        let fa = guard.as_mut().ok_or(())?;
        for i in 0..page_count {
            let va = VirtAddr::new(start + (i as u64) * Size4KiB::SIZE);
            let _ = crate::paging::unmap_4k_and_free_bitmap(mapper, fa, va);
        }
        Ok::<(), ()>(())
    })
}

fn map_user_anon_pages(start: VirtAddr, page_count: usize, flags: PageTableFlags) -> Result<(), ()> {
    crate::paging::with_mapper(|mapper| {
        let mut guard = crate::memory::lock_allocator();
        let fa = guard.as_mut().ok_or(())?;
        for i in 0..page_count {
            let v = start + u64::try_from(i).unwrap() * Size4KiB::SIZE;
            let page = Page::containing_address(v);
            let frame: PhysFrame<Size4KiB> = fa.allocate_frame().ok_or(())?;
            unsafe {
                mapper
                    .map_to(page, frame, flags, fa)
                    .map_err(|_| ())?
                    .flush();
            }
        }
        Ok::<(), ()>(())
    })
}

/// Linux-ish `fork`: shared address space; **child** stack is **0x402000..0x403000**, **RSP** mirrored.
pub fn sys_fork() -> u64 {
    let parent_slot = current_slot();
    let parent_pid = current_pid();
    let Some(child_pid) = alloc_pid() else {
        return (-12i64) as u64;
    };

    let rsp: u64;
    unsafe {
        core::arch::asm!("mov {0}, rsp", out(reg) rsp);
    }
    let saved = unsafe { crate::task::save_from_dispatch_rsp(rsp as *const u64) };

    if map_child_stack_and_copy(&saved).is_err() {
        return (-12i64) as u64;
    }

    let mut child_save = saved.clone();
    let delta = crate::user::USER_STACK_TOP.saturating_sub(saved.user_rsp);
    child_save.user_rsp = USER_STACK_CHILD_TOP.saturating_sub(delta);

    let (vm, brk) = {
        let g = TABLE.lock();
        let t = g.as_ref().unwrap();
        let p = t.procs[parent_slot].as_deref().unwrap();
        (p.vm_areas.clone(), p.brk_end)
    };

    let child = Process {
        pid: child_pid,
        parent: Some(parent_pid),
        brk_end: brk,
        vm_areas: vm,
        exit_code: None,
        kstack: AlignedKstack([0u8; 4096]),
        save: child_save.clone(),
        pending_rax: 0,
    };
    let child_slot = {
        let mut g = TABLE.lock();
        let t = g.as_mut().unwrap();
        let slot = t.alloc_slot().unwrap();
        t.procs[slot] = Some(Box::new(child));
        slot
    };

    set_syscall_saved(child_slot, child_save, 0);

    crate::scheduler::enqueue_new_task(child_slot);
    crate::scheduler::note_yield();

    child_pid
}

fn map_child_stack_and_copy(saved: &SyscallSaved) -> Result<(), ()> {
    let fl = PageTableFlags::PRESENT
        | PageTableFlags::WRITABLE
        | PageTableFlags::USER_ACCESSIBLE
        | PageTableFlags::NO_EXECUTE;
    crate::paging::with_mapper(|mapper| {
        let mut guard = crate::memory::lock_allocator();
        let fa = guard.as_mut().ok_or(())?;
        let page = Page::containing_address(VirtAddr::new(CHILD_STACK_LO));
        if mapper.translate_addr(page.start_address()).is_none() {
            let frame: PhysFrame<Size4KiB> = fa.allocate_frame().ok_or(())?;
            unsafe {
                mapper
                    .map_to(page, frame, fl, fa)
                    .map_err(|_| ())?
                    .flush();
            }
        }
        Ok::<(), ()>(())
    })?;
    let src_lo = saved.user_rsp;
    let src_hi = crate::user::USER_STACK_TOP;
    if src_lo >= src_hi {
        return Err(());
    }
    let len = (src_hi - src_lo) as usize;
    let parent_stack_lo = crate::user::USER_STACK_TOP - Size4KiB::SIZE;
    let dst_lo = CHILD_STACK_LO + (src_lo.saturating_sub(parent_stack_lo));
    copy_user_pages(src_lo, dst_lo, len)?;
    Ok(())
}

fn copy_user_pages(src: u64, dst: u64, len: usize) -> Result<(), ()> {
    for i in 0..len {
        let s = VirtAddr::new(src + i as u64);
        let d = VirtAddr::new(dst + i as u64);
        let ps = crate::paging::with_mapper(|m| m.translate_addr(s)).ok_or(())?;
        let pd = crate::paging::with_mapper(|m| m.translate_addr(d)).ok_or(())?;
        let b = unsafe {
            *VirtAddr::new(crate::memory::phys_to_virt(ps)).as_ptr::<u8>()
        };
        unsafe {
            *VirtAddr::new(crate::memory::phys_to_virt(pd)).as_mut_ptr::<u8>() = b;
        }
    }
    Ok(())
}

pub fn alloc_pid() -> Option<u64> {
    with_table_mut(|t| {
        for cand in 2..1024u64 {
            if t.procs.iter().filter_map(|x| x.as_deref()).all(|p| p.pid != cand) {
                return Some(cand);
            }
        }
        None
    })
    .flatten()
}

pub fn wait_reap(target: i64) -> Option<(u64, i32)> {
    let me = current_pid();
    with_table_mut(|t| {
        for slot in 0..t.procs.len() {
            let Some(p) = t.procs[slot].as_mut() else {
                continue;
            };
            if p.parent != Some(me) {
                continue;
            }
            let code = p.exit_code?;
            if target >= 0 && p.pid != target as u64 {
                continue;
            }
            let pid = p.pid;
            let st = code;
            t.procs[slot] = None;
            return Some((pid, st));
        }
        None
    })
    .flatten()
}

pub fn sys_execve(path_u: u64, argv_u: u64, envp_u: u64) -> u64 {
    const INIT_PATH: &[u8] = b"/init\0";
    let mut path = [0u8; 64];
    if crate::user::user_region_is_readable_range(path_u, INIT_PATH.len()) {
        unsafe {
            core::ptr::copy_nonoverlapping(path_u as *const u8, path.as_mut_ptr(), INIT_PATH.len());
        }
    } else {
        return (-14i64) as u64;
    }
    if path[..INIT_PATH.len()] != *INIT_PATH {
        return (-2i64) as u64;
    }
    let argv = match parse_user_ptrvec(argv_u, 32, 256) {
        Ok(a) => a,
        Err(e) => return e,
    };
    let envp = match parse_user_ptrvec(envp_u, 16, 256) {
        Ok(a) => a,
        Err(e) => return e,
    };
    let bin = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/init.elf"));
    match crate::elf::load_elf_exec(
        bin,
        0,
        crate::elf::ExecArgs {
            argv: &argv,
            envp: &envp,
        },
    ) {
        Ok((entry, rsp)) => {
            let slot = current_slot();
            let save = SyscallSaved::bootstrap_user(entry, rsp, 0x202);
            set_syscall_saved(slot, save, 0);
            crate::user::set_user_entry(entry);
            0
        }
        Err(_) => (-8i64) as u64,
    }
}

fn parse_user_ptrvec(base: u64, max_ptrs: usize, max_str: usize) -> Result<Vec<String>, u64> {
    if base == 0 {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for i in 0..max_ptrs {
        let p = base + (i as u64) * 8;
        if !crate::user::user_region_is_readable_range(p, 8) {
            return Err((-14i64) as u64);
        }
        let ptr = unsafe { *(p as *const u64) };
        if ptr == 0 {
            break;
        }
        let s = read_user_cstring(ptr, max_str)?;
        out.push(s);
    }
    Ok(out)
}

fn read_user_cstring(ptr: u64, cap: usize) -> Result<String, u64> {
    let mut v = Vec::new();
    for _ in 0..cap {
        if !crate::user::user_region_is_readable_range(ptr + v.len() as u64, 1) {
            return Err((-14i64) as u64);
        }
        let c = unsafe { *((ptr + v.len() as u64) as *const u8) };
        if c == 0 {
            break;
        }
        v.push(c);
    }
    String::from_utf8(v).map_err(|_| (-22i64) as u64)
}
