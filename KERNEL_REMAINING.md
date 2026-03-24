# RockOS — completeness snapshot and remaining work

This document answers whether the kernel is “complete.” **It is not**: it is a functional `no_std` x86‑64Bring-up kernel with Linux-ish syscalls, a userspace `init` crate, and QEMU serial smoke checks. What follows is a detailed backlog grouped by area.

---

## 1. Address spaces and memory management

| Item | Status | Detail |
|------|--------|--------|
| **Single global page table** | **Major gap** | All user mappings (`mmap`, `brk`, ELF, fork) edit the **same** active `CR3` tree (`paging::with_mapper` always uses current `Cr3::read()`). There is no per-process `CR3`, no switch on reschedule, and no ASID/PCID management. |
| **Fork and address spaces** | **Partial** | `fork` duplicates `VmArea` metadata and copies the **child user stack** into a second page, but **code, heap, and other mappings remain physically shared** with identical virtual layout. True isolation or **copy-on-write** is not implemented. |
| **Exec and old mappings** | **Partial** | `unmap_old_image_best_effort` clears old image pages for a new ELF; **per-process bookkeeping** (`vm_areas`, `brk`) may not always match every edge case after `execve` (e.g. interactions with pre-existing `mmap` regions overlapping image range). |
| **ELF loader limits** | **Partial** | Static `ET_EXEC`/`ET_DYN`‑ish loading with `PT_LOAD`; **BSS** beyond file-backed pages is zero-filled via mapped pages. Restrictions: **`p_align`** must be 0 or 4096; no dynamic linker; no `PT_INTERP`. |
| **`AT_PHDR` / low image VA** | **Quirk** | The userspace link script can map a **header page** below `0x400000` (e.g. first `PT_LOAD` at `0x3ff000`). The loader computes `AT_PHDR` correctly, but `user_region_is_readable_range` still treats the **image** as starting at **`USER_TEXT_BASE` (`0x400000`)**, so accesses to the auxiliary page (e.g. reading program headers from that VA) are **not** validated as “in image” unless extended. |
| **`munmap` / `mmap`** | **Partial** | **`munmap`** requires an **exact** `VmArea` match (same `virt_start` and span). No partial unmap, no merging/splitting of areas, no `MAP_FIXED` semantics beyond best-effort. **`MAP_SHARED`**, file-backed `mmap`, and prot changes (`mprotect`) are absent. |
| **Kernel robustness on OOM** | **Weak** | `alloc_error_handler` spins on `hlt`; many paths return `ENOMEM`-style errors but there is no global policy (kill process, printk, etc.). |

---

## 2. Scheduling and concurrency

| Item | Status | Detail |
|------|--------|--------|
| **Time-slice flag** | **Implemented (cooperative tail)** | Timer IRQ calls `scheduler::on_timer_tick`, which sets **`need_resched`**. Actual thread switch happens when the syscall path runs **`complete_syscall_and_schedule`** — i.e. **after syscalls** (or exit), not arbitrarily mid-userland execution. |
| **True preemptive multitasking** | **Gap** | No **forced** save/restore of user `RIP/RSP` on timer interrupt for a running task; no “schedule from IRQ” return path to another user context while one is still runnable. |
| **Idle CPU / when queue empty** | **Unreviewed in doc** | If the runnable queue becomes empty (e.g. all blocked or exited), behavior depends on `pick_next_and_restore` / stubs — worth auditing for **infinite hang** vs **halt**. |
| **Blocking syscalls vs global queue** | **Gap** | **`wait4`** returns **`EAGAIN` (-11)** when no zombie child exists; userspace must **`sched_yield`** in a loop. There is no **wait queue**, no “block this task until child exits,” and no priority inheritance. |
| **SMP** | **Absent** | Single CPU assumption; no IPIs, no per-CPU run queues, no big kernel lock strategy beyond existing `Mutex`/`spin` usage. |

---

## 3. Processes, `exec`, and program loading

| Item | Status | Detail |
|------|--------|--------|
| **`execve`** | **Very narrow** | Only path accepted is the literal **`/init`** kernel check against embedded **`/init\0`**. **No** VFS path walk, **no** `#!/` interpreter, **no** argument size limits beyond pointer/string scans — and the binary is **`include_bytes!(.../assets/init.elf)`**, not loaded from a disk-backed inode. |
| **Argv / envp / auxv** | **Partial** | Initial stack build provides **`argc`, `argv`, `envp`,** and aux entries: `AT_PHDR`, `AT_PHENT`, `AT_PHNUM`, `AT_PAGESZ`, `AT_ENTRY`, `AT_NULL`. Missing common entries glibc/musl expect: **`AT_BASE`**, **`AT_RANDOM`**, **`AT_HWCAP`**, **`AT_UID/GID`**, **`AT_SECURE`**, **`AT_CLKTCK`**, etc. |
| **Stack ABI** | **Partial** | 16-byte alignment is attempted in the stack builder; **full** conformance with every SysV amd64 edge case (extra AT entries, `LD_*`, `auxv` ordering vs glibc) is not guaranteed. |
| **`clone` / threads** | **Absent** | Only **`fork`**-like duplication; **no** `clone(2)` flags, **no** kernel threads sharing `mm` but separate stacks, **no** `pthread` support. |

---

## 4. Filesystem and I/O

| Item | Status | Detail |
|------|--------|--------|
| **`open` / `close`** | **Stub-level** | Special cases: **`/dev/stdin`**, **`/dev/tty`**, **`/dev/null`**, and a catch-all that creates an **empty RAM `Vec<u8>`** keyed by fd — **not** a real ramfs with names, directories, or `read`/`write` coherence beyond the minimal `read` path for fd ≥ 4. |
| **`write`** | **Partial** | Writes to stdout/stderr go to serial; other fds need audit for full behavior vs POSIX `write` partial returns and `EPIPE`. |
| **`read`** | **Partial** | stdin can **block** with `hlt` or use **O_NONBLOCK** (`fs::set_stdin_nonblock`); RAM file fd returns **`EAGAIN`** if empty — not the same as disk or pipe semantics. **No** `EINTR` / **`ERESTARTSYS`** model; **no** restartable syscalls. |
| **Pipes, `dup`, `dup2`, `fcntl`** | **Absent** | No `pipe(2)`, no `fcntl` beyond flags implied by open paths, no file description sharing. |
| **Real storage** | **Absent** | No block device, no partition table, no ext2/fat cache. |

---

## 5. Signals, security, and fault handling

| Item | Status | Detail |
|------|--------|--------|
| **Signals** | **Absent** | No `kill`, `sigaction`, signal frames, or `EINTR` from signals. |
| **Page fault from user** | **Debug-only** | **`#PF`** logs CR2/-error/RIP then **halt loop** — no **COW**, no extend stack on demand, no `SIGSEGV` delivery. |
| **Other faults** | **Similar** | **`#GP`**, **`#DE`**, double fault: **log and stop** — no recovery. |
| **Hard flags** | **Not audited** | No discussion in README of **SMEP/SMAP**, **UMIP**, **NX** policy for user stacks, or syscall filtering — worth a future hardening pass. |

---

## 6. IPC and networking

| Item | Status | Detail |
|------|--------|--------|
| **IPC** | **Absent** | No pipes, shared memory syscalls, sockets, futexes, or message queues. |
| **Networking** | **Absent** | No NIC driver, no TCP/IP stack. |

---

## 7. Time, `sleep`, and clocks

| Item | Status | Detail |
|------|--------|--------|
| **`nanosleep`** | **Stub** | Implemented as **busy wait** on PIT ticks inside the kernel (`sleep_until_ticks`) — **not** a wakable sleep with accounting against stolen time; **no** `clock_gettime` / monotonic clocks exposed. |

---

## 8. Testing, tooling, CI

| Item | Status | Detail |
|------|--------|--------|
| **`scripts/qemu_serial_test.sh`** | **Smoke** | Greps serial for **`Loaded init ELF`** and **`[syscall] exit(0)`** — good regression guard, **not** coverage of fork, mmap, negative errno paths, or faults. |
| **Unit / integration tests** | **Absent** | No `#[test]` harness in-tree for kernel logic; no scripted **syscall errno** matrix or **induced #PF** expectations. |
| **Build footguns** | **Minor** | **`cargo bootimage`** may report **`CARGO_MANIFEST_DIR` not set** in some environments; kernel binary vs disk image staleness is easy to confuse — consider documenting “always `cargo bootimage` before QEMU” in one place. |

---

## 9. “Done enough” for a minimal demo (short list)

These exist in some form today: **GDT/IDT/PIC/PIT**, **ring‑3 `iret`**, **SYSCALL/SYSRET**, **per-task kernel stack + saved user rip/rsp/rflags**, **round-robin runnable queue**, **timer-driven `need_resched`** (honored on syscall completion), **`fork` / `exit` / `getpid` / `getppid`**, **`brk` shrink/grow** with unmap/free, **`mmap`/`munmap`** (exact area), **static ELF + embedded `init` built from `userspace/init`**, **basic `open`/`read`/`write`/`close`** paths, **`/init`-only `execve` stack**.

---

## Suggested priority order (if continuing)

1. **Per-process `CR3`** (or **COW fork** on a shared tree) — unlocks real isolation and is prerequisite for “real” Unix semantics.  
2. **Blocking `wait4`** with a **wait queue** (and later **blocking read** integrated with scheduler).  
3. **True preempt** from timer (save user state in IRQ path or IPI to reschedule).  
4. **Ramfs + fd table** coherent with `read`/`write`/`seek`, then **pipe**.  
5. **Richer `execve`** (path lookup, load from FS, more **auxv**).  
6. **Tests**: syscall golden errno tests + optional fault injection in QEMU serial scripts.

---

*Generated to reflect the codebase as of the session that added `userspace/init`, `build.rs`, and `KERNEL_REMAINING.md`. Adjust this file as features land.*
