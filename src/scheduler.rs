//! Round-robin **runnable queue** and syscall **tail** (`syscall_tail`).  
//! **`wait4`** returns **`EAGAIN`** when no zombie child; userspace should **`sched_yield`** and retry.
//! Timer tick sets **`need_resched`** for **time-sliced** rotation on syscall boundaries.

use alloc::collections::VecDeque;
use spin::Mutex;
use core::sync::atomic::{AtomicBool, Ordering};

static SCHED: Mutex<Option<SchedState>> = Mutex::new(None);

struct SchedState {
    runnable: VecDeque<usize>,
    time_slice_ticks: u64,
    last_started_tick: u64,
}

const SLICE: u64 = 12;

static NEED_RESCHED: AtomicBool = AtomicBool::new(false);

pub fn init(_pid: u64) {
    *SCHED.lock() = Some(SchedState {
        runnable: VecDeque::from([0usize]),
        time_slice_ticks: SLICE,
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
    let _ = with_sched(|s| {
        if now.saturating_sub(s.last_started_tick) >= s.time_slice_ticks {
            NEED_RESCHED.store(true, Ordering::Relaxed);
        }
    });
}

pub fn note_yield() {
    NEED_RESCHED.store(true, Ordering::Relaxed);
}

pub fn enqueue_new_task(slot: usize) {
    let _ = with_sched(|s| {
        if !s.runnable.contains(&slot) {
            s.runnable.push_back(slot);
        }
    });
    NEED_RESCHED.store(true, Ordering::Relaxed);
}

fn rotate_runnable() {
    let _ = with_sched(|s| {
        if let Some(front) = s.runnable.pop_front() {
            s.runnable.push_back(front);
        }
        s.last_started_tick = crate::interrupts::ticks();
    });
}

/// Syscall **exit** path: zombie current, drop from queue, run someone else.
#[no_mangle]
pub fn schedule_after_exit() -> ! {
    let my_slot = crate::process::current_slot();
    crate::process::mark_zombie_current();
    with_sched(|s| {
        s.runnable.retain(|&x| x != my_slot);
    });
    NEED_RESCHED.store(true, Ordering::Relaxed);
    pick_next_and_restore();
}

/// Wait for IRQs (and other tasks) while [`crate::process::wait_reap`] has nothing to return.
pub fn block_current_on_wait() {
    x86_64::instructions::interrupts::enable_and_hlt();
}

/// Busy-sleep using the PIT tick counter until `deadline` (inclusive).
pub fn sleep_until_ticks(deadline: u64) {
    while crate::interrupts::ticks() < deadline {
        x86_64::instructions::interrupts::enable_and_hlt();
    }
}

#[no_mangle]
pub fn complete_syscall_and_schedule(ret_val: u64, dispatch_rsp: *const u64) -> ! {
    let slot = crate::process::current_slot();
    let saved =
        unsafe { crate::task::save_from_dispatch_rsp(dispatch_rsp) };
    crate::process::set_syscall_saved(slot, saved, ret_val);

    if NEED_RESCHED.swap(false, Ordering::Relaxed) {
        rotate_runnable();
    }

    pick_next_and_restore();
}

fn pick_next_and_restore() -> ! {
    let next_slot = loop {
        let cand = with_sched(|s| {
            while let Some(&c) = s.runnable.front() {
                if crate::process::slot_is_alive_runnable(c) {
                    return Some(c);
                }
                s.runnable.pop_front();
            }
            None
        })
        .flatten();
        if let Some(c) = cand {
            break c;
        }
        x86_64::instructions::interrupts::enable_and_hlt();
    };

    crate::process::set_current_slot(next_slot);
    let rax = crate::process::pending_rax_for_slot(next_slot);
    let top = crate::process::kstack_top_for_slot(next_slot);
    let saved = crate::process::syscall_saved_for_slot(next_slot);
    let ret_to = crate::syscall::syscall_user_return as *const () as u64;
    unsafe {
        crate::task::restore_to_kstack(top, &saved, ret_to);
    }
    let rsp = top - 88;
    unsafe {
        core::arch::asm!(
            "mov rsp, {rsp}",
            "mov rax, {rax}",
            "jmp syscall_user_return",
            rsp = in(reg) rsp,
            rax = in(reg) rax,
            options(noreturn),
        );
    }
}
