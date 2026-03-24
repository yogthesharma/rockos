# RockOS

RockOS is a small **x86_64** kernel in **Rust** (`#![no_std]`). It boots with **[bootloader](https://github.com/rust-osdev/bootloader) 0.9**, walks the firmware memory map, and runs on the bare metal with a bitmap physical allocator, a kernel heap, IRQ-driven timers and keyboard, and a **Linux-style `syscall` / `sysret`** path into **ring 3**.

## What’s in the box

- **Boot & memory** — `map_physical_memory`; identity-style low kernel mapping plus **phys + offset** for the whole RAM window. Bitmap frame allocator (trackable region is sized in [`src/memory.rs`](src/memory.rs)); kernel heap via [`linked_list_allocator`](https://crates.io/crates/linked_list_allocator).
- **CPU setup** — GDT/TSS (including stack for double fault), IDT, PIC remap, PIT on IRQ0 at ~[`pit::TIMER_HZ`](src/pit.rs) (default **100 Hz**).
- **Console** — VGA text buffer and **COM1** (`0x3F8`). With QEMU **`-serial stdio`**, kernel `println!` and user `write(1, …)` show up in your terminal.
- **Input** — PS/2 scan codes → ASCII; a **cooked TTY** (line editing + Enter) backs `read(0, …)` (see [`src/tty.rs`](src/tty.rs)).
- **Userspace** — Page-table mappings for a low user layout (`0x400000` text, one stack page). The kernel prefers [`assets/init.elf`](assets/init.elf) (bundled with `include_bytes!`); if ELF load fails, it falls back to a tiny in-tree machine-code blob (see [`src/user.rs`](src/user.rs)).
- **Scheduler** — Round-robin runnable queue, timer-driven **time slice** hints, per-task kernel stacks and syscall-save frames ([`src/scheduler.rs`](src/scheduler.rs), [`src/task.rs`](src/task.rs)).
- **Syscalls** — Numbers follow Linux x86-64 where it matters: `read`, `write`, `brk`, `mmap`, `munmap`, `fork`/`execve` hooks, `sched_yield`, `nanosleep`, `wait4`, `exit`, `getpid`, `getppid`. A thin [`src/fs.rs`](src/fs.rs) layer wires stdin to the TTY and stdout/stderr to the serial port; `open` / `close` are stubbed with `-ENOSYS` until full VFS exists.

## Requirements

- **Toolchain**: **nightly** Rust (this crate uses unstable features and `build-std`; see [`.cargo/config.toml`](.cargo/config.toml)).
- **Components** (for the same nightly you build with):

  ```bash
  rustup toolchain install nightly
  rustup component add rust-src llvm-tools-preview --toolchain nightly
  ```

- **`bootimage`**: `cargo install bootimage` (installs the `bootimage` subcommand used below).
- **QEMU**: `qemu-system-x86_64` on your `PATH`.

The default build target is **`x86_64-unknown-none`**. `rustflags` use **`-C relocation-model=static`** so the bootloader’s static layout matches expectations.

## Build & run

Point this repo at nightly (once per clone):

```bash
cd rockos
rustup override set nightly
```

**Run in QEMU** (builds the kernel, wraps it in a boot image, and invokes the `bootimage` runner from [`Cargo.toml`](Cargo.toml)):

```bash
cargo run
```

**Produce the disk image only**:

```bash
cargo bootimage
```

Artifact: **`target/x86_64-unknown-none/debug/bootimage-rockos.bin`**.

### cargo-make (optional)

With [cargo-make](https://github.com/sagiegurari/cargo-make):

```bash
cargo install cargo-make
cargo make run
```

That uses [`Makefile.toml`](Makefile.toml): `cargo bootimage`, then `qemu-system-x86_64` with the same flags as `[package.metadata.bootimage]`. If you change QEMU options, keep **`Cargo.toml`** and **`Makefile.toml`** in sync.

### QEMU flags (why they matter)

| Flag | Purpose |
|------|---------|
| `-serial stdio` | Attach COM1 to the terminal (kernel log + userspace stdout). |
| `-vga std` | Standard VGA for the text buffer. |
| `-no-reboot` | On triple fault, exit instead of rebooting in a tight loop. |
| `-D target/qemu-debug.log -d …` | Send QEMU’s CPU / interrupt debug output to a file so it does not interleave with serial. |

## Source map

| Module | Responsibility |
|--------|----------------|
| [`main.rs`](src/main.rs) | `kernel_main`: init order, keyboard demo window, syscall + scheduler bring-up, ELF or blob, `iretq` to ring 3 |
| [`memory.rs`](src/memory.rs) | `BootInfo` → bitmap allocator, `phys_to_virt` |
| [`paging.rs`](src/paging.rs) | Current CR3 → `OffsetPageTable`, guarded map/unmap |
| [`heap.rs`](src/heap.rs) | Kernel heap region |
| [`elf.rs`](src/elf.rs) | ELF64 `PT_LOAD` + `execve` reload helper |
| [`interrupts.rs`](src/interrupts.rs) | IDT, PIC, IRQ handlers, global tick counter |
| [`gdt.rs`](src/gdt.rs) | Segments, TSS, user/kernel selectors for syscall path |
| [`pit.rs`](src/pit.rs) | PIT programming |
| [`keyboard.rs`](src/keyboard.rs), [`tty.rs`](src/tty.rs) | Input and line discipline |
| [`serial.rs`](src/serial.rs), [`vga.rs`](src/vga.rs) | Output |
| [`syscall.rs`](src/syscall.rs) | `syscall` entry asm, dispatch, tail to scheduler |
| [`fs.rs`](src/fs.rs) | Minimal fd ↔ TTY / serial |
| [`process.rs`](src/process.rs) | Process table, `brk` / `mmap`, fork/exec |
| [`scheduler.rs`](src/scheduler.rs) | Run queue, preemption, post-syscall switch |
| [`task.rs`](src/task.rs) | Per-task kstack tops and saved syscall frames |
| [`user.rs`](src/user.rs) | User mappings, `iretq`, memory checks for syscalls |

## Syscall numbers (Linux ABI subset)

| `#` | Name |
|-----|------|
| 0 | `read` |
| 1 | `write` |
| 9 | `mmap` |
| 11 | `munmap` |
| 12 | `brk` |
| 24 | `sched_yield` |
| 35 | `nanosleep` |
| 39 | `getpid` |
| 57 | `fork` |
| 59 | `execve` |
| 60 | `exit` |
| 61 | `wait4` |
| 110 | `getppid` |

Implementations evolve; see [`syscall.rs`](src/syscall.rs) for the authoritative dispatch table.

## Hacking

- **Userland**: Replace [`assets/init.elf`](assets/init.elf) with your own **64-bit** ELF (static or PIE with load bias `0` as in the loader). Ensure segments fit the low user layout or extend [`user.rs`](src/user.rs) / [`process.rs`](src/process.rs).
- **Dual-serial noise**: If debugging faults, tail `target/qemu-debug.log` while watching serial on stdio.
- **Tests**: The binary is `test = false` in `Cargo.toml`; validation today is **boot under QEMU** and manual behavior checks.

RockOS is an educational playground, not a production OS—use it to experiment with bare-metal Rust, paging, and ring-3 bring-up.
