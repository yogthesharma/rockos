//! Per-task virtual memory metadata (**`brk`**, **`mmap`**) in the shared page table.
#![allow(dead_code)]
// `spawn_child_process`, `alloc_pid`, etc. are used by future fork/exec paths.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use spin::Mutex;
use x86_64::structures::paging::{
    FrameAllocator, Mapper, Page, PageSize, PageTableFlags, PhysFrame, Size4KiB, Translate,
};
use x86_64::VirtAddr;

/// Program break region: grows **up** from here.
pub const USER_BRK_BASE: u64 = 0x403000;
pub const USER_BRK_LIMIT: u64 = 0x410000;

pub const USER_MMAP_BASE: u64 = 0x410000;
pub const USER_MMAP_LIMIT: u64 = 0x480000;

pub const PROT_READ: u64 = 1;
pub const PROT_WRITE: u64 = 2;
pub const PROT_EXEC: u64 = 4;

#[derive(Clone)]
pub struct VmArea {
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
}

impl Process {
    pub fn new(pid: u64, parent: Option<u64>) -> Self {
        Self {
            pid,
            parent,
            brk_end: USER_BRK_BASE,
            vm_areas: Vec::new(),
            exit_code: None,
        }
    }
}

static NEXT_MMAP: AtomicU64 = AtomicU64::new(USER_MMAP_BASE);
static TABLE: Mutex<Option<ProcessTable>> = Mutex::new(None);

struct ProcessTable {
    procs: Vec<Option<Process>>,
    current: usize,
}

impl ProcessTable {
    fn alloc_slot(&mut self) -> Option<usize> {
        for (i, s) in self.procs.iter_mut().enumerate() {
            if s.is_none() {
                return Some(i);
            }
        }
        None
    }
}

pub fn init() {
    let mut v = Vec::new();
    v.resize_with(8, || None);
    v[0] = Some(Process::new(1, None));
    *TABLE.lock() = Some(ProcessTable { procs: v, current: 0 });
}

fn with_table<R>(f: impl FnOnce(&mut ProcessTable) -> R) -> Option<R> {
    let mut g = TABLE.lock();
    let t = g.as_mut()?;
    Some(f(t))
}

pub fn current_pid() -> u64 {
    with_table(|t| {
        t.procs[t.current]
            .as_ref()
            .map(|p| p.pid)
            .unwrap_or(1)
    })
    .unwrap_or(1)
}

pub fn parent_of(pid: u64) -> Option<u64> {
    with_table(|t| {
        for p in t.procs.iter().flatten() {
            if p.pid == pid {
                return p.parent;
            }
        }
        None
    })
    .flatten()
}

pub fn covers_brk_heap(addr: u64, end: u64) -> bool {
    let Some(brk) = with_table(|t| t.procs[t.current].as_ref().map(|p| p.brk_end)).flatten() else {
        return false;
    };
    addr >= USER_BRK_BASE && end <= brk && end <= USER_BRK_LIMIT
}

pub fn covers_mmap_read(addr: u64, end: u64) -> bool {
    with_table(|t| {
        let p = t.procs[t.current].as_ref()?;
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
    with_table(|t| {
        let p = t.procs[t.current].as_ref()?;
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
    with_table(|t| {
        let idx = t.current;
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
        if addr <= old {
            proc.brk_end = addr;
            return addr;
        }
        let fl = PageTableFlags::PRESENT
            | PageTableFlags::WRITABLE
            | PageTableFlags::USER_ACCESSIBLE
            | PageTableFlags::NO_EXECUTE;
        match map_brk_grow(old, addr, fl) {
            Ok(()) => {
                proc.brk_end = addr;
                addr
            }
            Err(()) => (-12i64) as u64,
        }
    })
    .unwrap_or((-12i64) as u64)
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
    match map_user_anon_pages(VirtAddr::new(addr), n_pages, fl) {
        Ok(()) => {
            let _ = with_table(|t| {
                if let Some(p) = t.procs[t.current].as_mut() {
                    p.vm_areas.push(VmArea {
                        virt_start: addr,
                        pages: n_pages,
                        writable: prot & PROT_WRITE != 0,
                        executable: prot & PROT_EXEC != 0,
                    });
                }
            });
            addr
        }
        Err(()) => (-12i64) as u64,
    }
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

pub fn mark_exit(status: i32) {
    let _ = with_table(|t| {
        if let Some(p) = t.procs[t.current].as_mut() {
            p.exit_code = Some(status);
        }
    });
}

/// `target < 0` → any child of the running process; else wait for that PID.
pub fn wait_reap(target: i64) -> Option<(u64, i32)> {
    with_table(|t| {
        let me = t.procs.get(t.current)?.as_ref()?.pid;
        for slot in t.procs.iter_mut() {
            let Some(p) = slot.as_mut() else {
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
            *slot = None;
            return Some((pid, st));
        }
        None
    })
    .flatten()
}

pub fn spawn_child_process(child_pid: u64, parent: u64) -> Result<(), ()> {
    let Some(r) = with_table(|t| {
        let slot = t.alloc_slot().ok_or(())?;
        t.procs[slot] = Some(Process::new(child_pid, Some(parent)));
        Ok(())
    }) else {
        return Err(());
    };
    r
}

pub fn alloc_pid() -> Option<u64> {
    with_table(|t| {
        for cand in 2..=64u64 {
            if t.procs.iter().flatten().all(|p| p.pid != cand) {
                return Some(cand);
            }
        }
        None
    })
    .flatten()
}
