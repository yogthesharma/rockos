//! Minimal file-descriptor layer: **stdin** via [`crate::tty`], **stdout/stderr** on **COM1**.

use crate::user;

const STDIN_FILENO: u64 = 0;
const STDOUT_FILENO: u64 = 1;
const STDERR_FILENO: u64 = 2;

pub fn sys_read(fd: u64, buf_addr: u64, len: u64) -> u64 {
    if fd != STDIN_FILENO {
        return 0;
    }
    let len = len as usize;
    if len == 0 {
        return 0;
    }
    if !user::user_region_is_writable_range(buf_addr, len) {
        return (-14i64) as u64;
    }
    let dst = unsafe { core::slice::from_raw_parts_mut(buf_addr as *mut u8, len) };
    crate::tty::read_line(dst) as u64
}

pub fn write_fd_any(fd: u64, slice: &[u8]) -> u64 {
    match fd {
        STDOUT_FILENO | STDERR_FILENO => {
            crate::serial::write_bytes(slice);
            slice.len() as u64
        }
        _ => (-9i64) as u64,
    }
}

pub fn sys_open(_path: u64, _flags: u64) -> u64 {
    (-38i64) as u64
}

pub fn sys_close(_fd: u64) -> u64 {
    let _ = _fd;
    0
}
