//! Round-robin runnable **queue**, blocking **`wait4`**, **sleep** deadlines, syscall **tail** scheduling.
//! Time-slice **hint** via [`on_timer_tick`] (`need_resched` — checked on syscall exit).

use alloc::collections::{BTreeMap, VecDeque};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use spin::Mutex;

static SCHED: Mutex<Option<SchedState>> = Mutex::new(None);

struct SchedState {
    runnable: VecDeque<usize>,
    /// Slot indices blocked in `wait4`.
    wait_blocked: Vec<usize>,
    time_slice_ticks: u64,
    last_started_tick: u64,
}

impl SchedState {
    const SLICE: u64 = 15;
}

static NEED_RESCHED: AtomicBool = AtomicBool::new(false);
static SLEEP_UNTIL: Mutex<BTreeMap<u64, Vec<usize>>> = Mutex::new(BTreeMap::new());

pub fn init(_pid: u64) {
    *SCHED.lock() = Some(SchedState {
        runnable: VecDeque::from([0usize]),
        wait_blocked: Vec::new(),
        time_slice_ticks: SchedState::SLICE,
        last_started_tick: crate::interrupts::ticks(),
    });
    NEED_RESCHED.store(false, Ordering::Relaxed);
}

fn with_sched<R>(f: impl FnOnce(&mut SchedState) -> R) -> Option<R> {
    let mut g = SCHED.lock();
    let s = g.as_mut()?;
    Some(f(s))
}

pub fn on_timer_tick(now: u64) {
    let re = with_sched(|s| {
        if now.saturating_sub(s.last_started_tick) >= s.time_slice_ticks {
            NEED_RESCHED.store(true, Ordering::Relaxed);
        }
    });
    let _ = re;

    let mut wake: Vec<usize> = Vec::new();
    let mut sleep = SLEEP_UNTIL.lock();
    while let Some((&t, _)) = sleep.first_key_value() {
        if t > now {
            break;
        }
        if let Some((_, slots)) = sleep.pop_first() {
            wake.extend(slots);
        }
    }
    drop(sleep);
    for sl in wake {
        enqueue_slot(sl);
    }
}

pub fn note_yield() {
    NEED_RESCHED.store(true, Ordering::Relaxed);
}

pub fn sleep_until_ticks(target: u64) {
    let slot = crate::process::current_slot();
    {
        let mut m = SLEEP_UNTIL.lock();
        m.entry(target).or_default().push(slot);
    }
    with_sched(|s| {
        s.runnable.retain(|&x| x != slot);
    });
    NEED_RESCHED.store(true, Ordering::Relaxed);
    pick_next_and_restore_from_idle(0);
}

fn enqueue_slot(slot: usize) {
    let _ = with_sched(|s| {
        if !s.runnable.contains(&slot) && !s.wait_blocked.contains(&slot) {
            s.runnable.push_back(slot);
        }
    });
}

pub fn block_current_on_wait() {
    let slot = crate::process::current_slot();
    with_sched(|s| {
        s.runnable.retain(|&x| x != slot);
        if !s.wait_blocked.contains(&slot) {
            s.wait_blocked.push(slot);
        }
    });
    NEED_RESCHED.store(true, Ordering::Relaxed);
    pick_next_and_restore_from_idle(0);
}

pub fn try_unblock_waiting_parent(_parent: u64) {
    // Wakeup is coarse: retry all wait-blocked; `wait_reap` in syscall loop filters.
    let waiters: Vec<usize> = with_sched(|s| core::mem::take(&mut s.wait_blocked)).unwrap_or_default();
    for sl in waiters {
        enqueue_slot(sl);
    }
}

/// After syscall: save `saved`, apply `ret_val`, maybe rotate queue, restore next task.
#[no_mangle]
pub fn complete_syscall_and_schedule(ret_val: u64, dispatch_rsp: *const u64) -> ! {
    let slot = crate::process::current_slot();
    let saved =
        unsafe { crate::task::save_from_dispatch_rsp(dispatch_rsp) };
    crate::process::set_syscall_saved(slot, saved, ret_val);

    if NEED_RESCHED.swap(false, Ordering::Relaxed) {
        rotate_runnable();
    }

    pick_next_and_restore(ret_val);
}

fn rotate_runnable() {
    let _ = with_sched(|s| {
        if let Some(front) = s.runnable.pop_front() {
            s.runnable.push_back(front);
        }
        s.last_started_tick = crate::interrupts::ticks();
    });
}

fn pick_next_and_restore(_from_ret: u64) -> ! {
    let (next_slot, pending_rax) = loop {
        let pair = with_sched(|s| {
            while let Some(&cand) = s.runnable.front() {
                if crate::process::slot_runnable(cand) {
                    let rax = crate::process::take_pending_rax(cand);
                    return Some((cand, rax));
                }
                s.runnable.pop_front();
            }
            None
        })
        .flatten();
        if let Some(p) = pair {
            break p;
        }
        x86_64::instructions::interrupts::enable_and_hlt();
    };

    crate::process::set_current_slot(next_slot);
    let top = crate::process::kstack_top_for_slot(next_slot);
    let saved = crate::process::syscall_saved_for_slot(next_slot);
    let ret_to = crate::syscall::syscall_user_return as *const () as u64;
    unsafe {
        crate::task::restore_to_kstack(top, &saved, ret_to);
    }
    let base = top - 88;
    let rsp = base;
    unsafe {
        core::arch::asm!(
            "mov rsp, {rsp}",
            "mov rax, {rax}",
            "jmp syscall_user_return",
            rsp = in(reg) rsp,
            rax = in(reg) pending_rax,
            options(noreturn),
        );
    }
}

/// Idle kernel when no runnable: **used from sleep/wait** with `ret_val` ignored — re-save caller.
fn pick_next_and_restore_from_idle(_dummy: u64) -> ! {
    pick_next_and_restore(0);
}

/// Child exited: unblock parents that might be waiting.
pub fn schedule_after_exit() -> ! {
    let parent = crate::process::current_ppid();
    crate::process::mark_zombie_current();
    let my_slot = crate::process::current_slot();
    with_sched(|s| {
        s.runnable.retain(|&x| x != my_slot);
    });
    try_unblock_waiting_parent(parent.unwrap_or(0));
    NEED_RESCHED.store(true, Ordering::Relaxed);
    pick_next_and_restore(0);
}

pub fn enqueue_new_task(slot: usize) {
    enqueue_slot(slot);
    NEED_RESCHED.store(true, Ordering::Relaxed);
}
