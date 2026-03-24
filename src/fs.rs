//! Minimal **fd** table: **stdin/stdout/stderr**, tiny **RAM** files, **O_NONBLOCK** for fd 0.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use spin::Mutex;

static STDIN_NONBLOCK: AtomicBool = AtomicBool::new(false);
static NEXT_FD: AtomicU64 = AtomicU64::new(4);
static FILES: Mutex<BTreeMap<u64, Vec<u8>>> = Mutex::new(BTreeMap::new());

pub fn set_stdin_nonblock(nb: bool) {
    STDIN_NONBLOCK.store(nb, Ordering::Relaxed);
}

pub fn sys_read(fd: u64, buf: u64, len: u64) -> u64 {
    const EAGAIN: u64 = (-11i64) as u64;
    const EBADF: u64 = (-9i64) as u64;
    const EFAULT: u64 = (-14i64) as u64;
    const EINVAL: u64 = (-22i64) as u64;

    if len > 8192 {
        return EINVAL;
    }
    let len = len as usize;
    if !crate::user::user_region_is_writable_range(buf, len) {
        return EFAULT;
    }
    let dst = unsafe { core::slice::from_raw_parts_mut(buf as *mut u8, len) };

    if fd == 0 {
        let nb = STDIN_NONBLOCK.load(Ordering::Relaxed);
        if !nb {
            loop {
                match crate::tty::read(dst, false) {
                    Ok(0) => x86_64::instructions::interrupts::enable_and_hlt(),
                    Ok(n) => return n as u64,
                    Err(_) => return (-4i64) as u64,
                }
            }
        }
        match crate::tty::read(dst, true) {
            Ok(n) => n as u64,
            Err(e) => e as u64,
        }
    } else if fd == 1 || fd == 2 {
        EBADF
    } else {
        let g = FILES.lock();
        let Some(data) = g.get(&fd) else {
            return EBADF;
        };
        if data.is_empty() {
            return EAGAIN;
        }
        let n = data.len().min(len);
        dst[..n].copy_from_slice(&data[..n]);
        n as u64
    }
}

pub fn sys_open(path_ptr: u64, flags: u64) -> u64 {
    const ENOENT: u64 = (-2i64) as u64;
    const EMFILE: u64 = (-24i64) as u64;

    let mut path = [0u8; 128];
    if !crate::user::user_region_is_readable_range(path_ptr, 128) {
        return (-14i64) as u64;
    }
    unsafe {
        core::ptr::copy_nonoverlapping(path_ptr as *const u8, path.as_mut_ptr(), 128);
    }
    let nul = path.iter().position(|&b| b == 0).unwrap_or(128);
    let path = match core::str::from_utf8(&path[..nul]) {
        Ok(s) => s,
        Err(_) => return (-22i64) as u64,
    };

    if path == "/dev/stdin" || path == "/dev/tty" {
        if flags & 0x800 != 0 {
            set_stdin_nonblock(true);
        }
        return 0;
    }
    if path == "/dev/null" {
        let fd = NEXT_FD.fetch_add(1, Ordering::SeqCst);
        FILES.lock().insert(fd, Vec::new());
        return fd;
    }

    if nul == 0 || path.is_empty() {
        return ENOENT;
    }
    if FILES.lock().len() > 64 {
        return EMFILE;
    }
    let fd = NEXT_FD.fetch_add(1, Ordering::SeqCst);
    FILES.lock().insert(fd, Vec::new());
    fd
}

pub fn sys_close(fd: u64) -> u64 {
    if fd <= 2u64 {
        return (-9i64) as u64;
    }
    if FILES.lock().remove(&fd).is_none() {
        return (-9i64) as u64;
    }
    0
}

pub fn write_fd_any(fd: u64, slice: &[u8]) -> u64 {
    match fd {
        1 | 2 => {
            crate::serial::write_bytes(slice);
            slice.len() as u64
        }
        n if n > 2 => {
            let mut g = FILES.lock();
            if let Some(v) = g.get_mut(&n) {
                v.extend_from_slice(slice);
                slice.len() as u64
            } else {
                (-9i64) as u64
            }
        }
        _ => (-9i64) as u64,
    }
}
