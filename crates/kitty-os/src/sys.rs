// SPDX-License-Identifier: GPL-3.0-or-later
//! Minimal libc bindings.
//!
//! Ported from the subset of `<unistd.h>`, `<fcntl.h>`, `<termios.h>`,
//! `<sys/ioctl.h>`, `<sys/wait.h>`, `<poll.h>`, `<sys/signalfd.h>`,
//! `<sys/eventfd.h>`, `<time.h>` and `safe-wrappers.h` used by kitty's OS
//! layer. No external crate is used: the declarations and the handful of
//! structs that cross the ABI boundary are defined here.
//!
//! Only Linux is implemented. The layout of [`Termios`], [`SigSet`] and
//! [`SignalfdSiginfo`] follows glibc on the architectures kitty supports
//! (x86_64, aarch64 and the other common Linux ABIs). Other platforms get the
//! stubs in the sibling modules and report `Unsupported`.

#![cfg(target_os = "linux")]
#![allow(dead_code)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use core::ffi::c_void;

pub type CChar = core::ffi::c_char;
pub type CInt = core::ffi::c_int;
pub type CUInt = core::ffi::c_uint;
pub type CLong = core::ffi::c_long;
pub type CULong = core::ffi::c_ulong;
pub type RawFd = core::ffi::c_int;
pub type Pid = core::ffi::c_int;
pub type Size = usize;
pub type Ssize = isize;

pub const O_RDWR: CInt = 0o2;
pub const O_NOCTTY: CInt = 0o400;
pub const O_CLOEXEC: CInt = 0o2000000;
pub const O_NONBLOCK: CInt = 0o4000;

pub const F_GETFD: CInt = 1;
pub const F_SETFD: CInt = 2;
pub const F_GETFL: CInt = 3;
pub const F_SETFL: CInt = 4;
pub const FD_CLOEXEC: CInt = 1;

pub const TIOCSCTTY: CULong = 0x540e;
pub const TIOCGWINSZ: CULong = 0x5413;
pub const TIOCSWINSZ: CULong = 0x5414;

pub const WNOHANG: CInt = 1;

pub const POLLIN: i16 = 0x001;
pub const POLLOUT: i16 = 0x004;
pub const POLLERR: i16 = 0x008;
pub const POLLHUP: i16 = 0x010;
pub const POLLNVAL: i16 = 0x020;

pub const CLOCK_MONOTONIC: CInt = 1;
pub const CLOCK_MONOTONIC_RAW: CInt = 4;

pub const EFD_CLOEXEC: CInt = 0o2000000;
pub const EFD_NONBLOCK: CInt = 0o4000;
pub const SFD_CLOEXEC: CInt = 0o2000000;
pub const SFD_NONBLOCK: CInt = 0o4000;

pub const SIG_BLOCK: CInt = 0;
pub const SIG_UNBLOCK: CInt = 1;
pub const SIG_SETMASK: CInt = 2;

pub const TCSANOW: CInt = 0;

pub const X_OK: CInt = 1;

pub const SIGHUP: CInt = 1;
pub const SIGINT: CInt = 2;
pub const SIGKILL: CInt = 9;
pub const SIGPIPE: CInt = 13;
pub const SIGTERM: CInt = 15;
pub const SIGCHLD: CInt = 17;
pub const SIGXFSZ: CInt = 25;
pub const SIG_DFL: usize = 0;

pub const EPERM: CInt = 1;
pub const ENOENT: CInt = 2;
pub const ESRCH: CInt = 3;
pub const EINTR: CInt = 4;
pub const EIO: CInt = 5;
pub const ENXIO: CInt = 6;
pub const EBADF: CInt = 9;
pub const ECHILD: CInt = 10;
pub const EAGAIN: CInt = 11;
pub const EWOULDBLOCK: CInt = EAGAIN;
pub const EINVAL: CInt = 22;
pub const ENOTTY: CInt = 25;
pub const ENOSYS: CInt = 38;
pub const ETIMEDOUT: CInt = 110;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Winsize {
    pub ws_row: u16,
    pub ws_col: u16,
    pub ws_xpixel: u16,
    pub ws_ypixel: u16,
}

/// glibc `struct termios` for Linux (60 bytes on common ABIs).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Termios {
    pub c_iflag: u32,
    pub c_oflag: u32,
    pub c_cflag: u32,
    pub c_lflag: u32,
    pub c_line: u8,
    pub c_cc: [u8; 32],
    pub c_ispeed: u32,
    pub c_ospeed: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Timespec {
    pub tv_sec: i64,
    pub tv_nsec: i64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct PollFd {
    pub fd: RawFd,
    pub events: i16,
    pub revents: i16,
}

/// glibc `sigset_t` on Linux: 1024 bits.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct SigSet {
    pub bits: [u64; 16],
}

impl SigSet {
    pub fn empty() -> Self {
        Self { bits: [0; 16] }
    }

    pub fn add(&mut self, sig: CInt) {
        if (1..=1024).contains(&sig) {
            let idx = (sig - 1) as usize;
            self.bits[idx / 64] |= 1u64 << (idx % 64);
        }
    }

    pub fn contains(&self, sig: CInt) -> bool {
        if (1..=1024).contains(&sig) {
            let idx = (sig - 1) as usize;
            self.bits[idx / 64] & (1u64 << (idx % 64)) != 0
        } else {
            false
        }
    }
}

/// Linux `struct signalfd_siginfo` (128 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct SignalfdSiginfo {
    pub ssi_signo: u32,
    pub ssi_errno: i32,
    pub ssi_code: i32,
    pub ssi_pid: u32,
    pub ssi_uid: u32,
    pub ssi_fd: i32,
    pub ssi_tid: u32,
    pub ssi_band: u32,
    pub ssi_overrun: u32,
    pub ssi_trapno: u32,
    pub ssi_status: i32,
    pub ssi_int: i32,
    pub ssi_ptr: u64,
    pub ssi_utime: u64,
    pub ssi_stime: u64,
    pub ssi_addr: u64,
    pub pad: [u8; 48],
}

extern "C" {
    pub fn open(path: *const CChar, flags: CInt, mode: CUInt) -> CInt;
    pub fn close(fd: CInt) -> CInt;
    pub fn read(fd: CInt, buf: *mut c_void, count: Size) -> Ssize;
    pub fn write(fd: CInt, buf: *const c_void, count: Size) -> Ssize;
    pub fn pipe2(fds: *mut CInt, flags: CInt) -> CInt;
    pub fn eventfd(initval: CUInt, flags: CInt) -> CInt;
    pub fn signalfd(fd: CInt, mask: *const SigSet, flags: CInt) -> CInt;
    pub fn fork() -> Pid;
    pub fn execvp(file: *const CChar, argv: *const *const CChar) -> CInt;
    pub fn dup(fd: CInt) -> CInt;
    pub fn dup2(oldfd: CInt, newfd: CInt) -> CInt;
    pub fn fcntl(fd: CInt, cmd: CInt, ...) -> CInt;
    pub fn ioctl(fd: CInt, request: CULong, ...) -> CInt;
    pub fn setsid() -> Pid;
    pub fn chdir(path: *const CChar) -> CInt;
    pub fn access(path: *const CChar, mode: CInt) -> CInt;
    pub fn getpgid(pid: Pid) -> Pid;
    pub fn getpid() -> Pid;
    pub fn kill(pid: Pid, sig: CInt) -> CInt;
    pub fn killpg(pgrp: Pid, sig: CInt) -> CInt;
    pub fn waitpid(pid: Pid, status: *mut CInt, options: CInt) -> Pid;
    pub fn posix_openpt(flags: CInt) -> CInt;
    pub fn grantpt(fd: CInt) -> CInt;
    pub fn unlockpt(fd: CInt) -> CInt;
    pub fn ptsname_r(fd: CInt, buf: *mut CChar, buflen: Size) -> CInt;
    pub fn tcgetattr(fd: CInt, termios: *mut Termios) -> CInt;
    pub fn tcsetattr(fd: CInt, actions: CInt, termios: *const Termios) -> CInt;
    pub fn cfmakeraw(termios: *mut Termios);
    pub fn isatty(fd: CInt) -> CInt;
    pub fn clock_gettime(clockid: CInt, tp: *mut Timespec) -> CInt;
    pub fn nanosleep(req: *const Timespec, rem: *mut Timespec) -> CInt;
    pub fn poll(fds: *mut PollFd, nfds: CULong, timeout: CInt) -> CInt;
    pub fn sigemptyset(set: *mut SigSet) -> CInt;
    pub fn sigaddset(set: *mut SigSet, sig: CInt) -> CInt;
    pub fn sigprocmask(how: CInt, set: *const SigSet, oldset: *mut SigSet) -> CInt;
    pub fn signal(signum: CInt, handler: usize) -> usize;
    pub fn _exit(status: CInt) -> !;
}

// glibc/musl expose the environment as a global named `environ`. It is not a
// function, so it must be declared separately.
extern "C" {
    #[link_name = "environ"]
    pub static mut ENVIRON: *mut *mut CChar;
}

pub fn errno() -> CInt {
    // SAFETY: __errno_location returns a valid thread-local pointer.
    unsafe { *__errno_location() }
}

pub fn set_errno(value: CInt) {
    // SAFETY: as above.
    unsafe {
        *__errno_location() = value;
    }
}

extern "C" {
    fn __errno_location() -> *mut CInt;
}

pub fn last_os_error() -> std::io::Error {
    std::io::Error::from_raw_os_error(errno())
}

pub fn cvt(ret: CInt) -> std::io::Result<CInt> {
    if ret == -1 {
        Err(last_os_error())
    } else {
        Ok(ret)
    }
}

pub fn cvt_ssize(ret: Ssize) -> std::io::Result<Ssize> {
    if ret == -1 {
        Err(last_os_error())
    } else {
        Ok(ret)
    }
}

pub fn cvt_pid(ret: Pid) -> std::io::Result<Pid> {
    if ret == -1 {
        Err(last_os_error())
    } else {
        Ok(ret)
    }
}

pub fn wifexited(status: CInt) -> bool {
    (status & 0x7f) == 0
}

pub fn wexitstatus(status: CInt) -> CInt {
    (status >> 8) & 0xff
}

pub fn wifsignaled(status: CInt) -> bool {
    ((status & 0x7f) + 1) as i8 >> 1 > 0
}

pub fn wtermsig(status: CInt) -> CInt {
    status & 0x7f
}

pub fn wifstopped(status: CInt) -> bool {
    (status & 0xff) == 0x7f
}

pub fn wstopsig(status: CInt) -> CInt {
    (status >> 8) & 0xff
}

pub fn wifcontinued(status: CInt) -> bool {
    status == 0xffff
}

pub fn ioctl_winsize(fd: RawFd, request: CULong, ws: *mut Winsize) -> CInt {
    // SAFETY: the caller supplies a valid pointer for the ioctl request.
    unsafe { ioctl(fd, request, ws) }
}

pub trait AsRawFdExt {
    fn raw_fd(&self) -> RawFd;
}

impl AsRawFdExt for std::os::fd::OwnedFd {
    fn raw_fd(&self) -> RawFd {
        use std::os::fd::AsRawFd;
        self.as_raw_fd()
    }
}
