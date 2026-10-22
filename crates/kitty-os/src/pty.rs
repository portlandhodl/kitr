// SPDX-License-Identifier: GPL-3.0-or-later
// Ported from kitty/child.py (openpty) and kitty/child-monitor.c (pty_resize),
// Copyright (C) 2018-2024 Kovid Goyal <kovid at kovidgoyal.net>

//! PTY creation, window size and termios helpers.
//!
//! `open_pty()` mirrors `kitty.child.openpty()`: the master is close-on-exec
//! and the slave is inheritable. Both descriptors are blocking on return.
//! The termios helpers are the raw-mode operations used by the monitor and by
//! the tests; `enter_raw_mode` returns the previous state so it can be
//! restored exactly.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

#[cfg(target_os = "linux")]
pub use crate::sys::Termios;

#[cfg(not(target_os = "linux"))]
#[derive(Clone, Copy, Debug)]
pub struct Termios {
    _opaque: [u8; 64],
}

#[cfg(not(target_os = "linux"))]
impl Default for Termios {
    fn default() -> Self {
        Self { _opaque: [0; 64] }
    }
}

// Linux termios flag bits (asm-generic).
pub const ISIG: u32 = 0o000001;
pub const ICANON: u32 = 0o000002;
pub const ECHO: u32 = 0o000010;
pub const ECHONL: u32 = 0o000100;
pub const IEXTEN: u32 = 0o100000;
pub const ICRNL: u32 = 0o000400;
pub const IXON: u32 = 0o002000;
pub const OPOST: u32 = 0o000001;
pub const CS8: u32 = 0o000060;

/// `struct winsize` without the libc type leaking into the public API.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WindowSize {
    pub rows: u16,
    pub columns: u16,
    pub xpixel: u16,
    pub ypixel: u16,
}

impl WindowSize {
    pub fn new(rows: u16, columns: u16) -> Self {
        Self {
            rows,
            columns,
            xpixel: 0,
            ypixel: 0,
        }
    }
}

/// A freshly created PTY pair.
#[derive(Debug)]
pub struct PtyPair {
    master: OwnedFd,
    slave: OwnedFd,
}

impl PtyPair {
    pub fn master_fd(&self) -> RawFd {
        self.master.as_raw_fd()
    }

    pub fn slave_fd(&self) -> RawFd {
        self.slave.as_raw_fd()
    }

    pub fn master(&self) -> &OwnedFd {
        &self.master
    }

    pub fn slave(&self) -> &OwnedFd {
        &self.slave
    }

    /// Give up ownership of both descriptors.
    pub fn into_fds(self) -> (OwnedFd, OwnedFd) {
        (self.master, self.slave)
    }
}

/// `openpty()` using `posix_openpt`/`grantpt`/`unlockpt`/`ptsname_r`.
#[cfg(target_os = "linux")]
pub fn open_pty() -> io::Result<PtyPair> {
    use crate::sys;
    use std::ffi::CStr;

    // SAFETY: plain libc calls with valid arguments.
    let master_raw = unsafe { sys::posix_openpt(sys::O_RDWR | sys::O_NOCTTY | sys::O_CLOEXEC) };
    if master_raw < 0 {
        return Err(sys::last_os_error());
    }
    // SAFETY: master_raw is a fresh owned descriptor.
    let master = unsafe { OwnedFd::from_raw_fd(master_raw) };

    // SAFETY: master is a valid pty master.
    sys::cvt(unsafe { sys::grantpt(master.as_raw_fd()) })?;
    // SAFETY: as above.
    sys::cvt(unsafe { sys::unlockpt(master.as_raw_fd()) })?;

    let mut name = [0i8; 256];
    // SAFETY: the buffer is valid for its whole length.
    let ret = unsafe { sys::ptsname_r(master.as_raw_fd(), name.as_mut_ptr(), name.len()) };
    if ret != 0 {
        return Err(io::Error::from_raw_os_error(ret));
    }
    // SAFETY: ptsname_r NUL-terminates the name on success.
    let name = unsafe { CStr::from_ptr(name.as_ptr()) };

    // SAFETY: name is a valid NUL-terminated path.
    let slave_raw = unsafe { sys::open(name.as_ptr(), sys::O_RDWR | sys::O_NOCTTY, 0o600) };
    if slave_raw < 0 {
        return Err(sys::last_os_error());
    }
    // SAFETY: slave_raw is a fresh owned descriptor.
    let slave = unsafe { OwnedFd::from_raw_fd(slave_raw) };

    // Python's openpty() makes the slave inheritable and the master not.
    set_cloexec(slave.as_raw_fd(), false)?;
    set_cloexec(master.as_raw_fd(), true)?;
    Ok(PtyPair { master, slave })
}

#[cfg(not(target_os = "linux"))]
pub fn open_pty() -> io::Result<PtyPair> {
    Err(crate::unsupported("open_pty"))
}

#[cfg(target_os = "linux")]
pub fn set_window_size(fd: RawFd, size: WindowSize) -> io::Result<()> {
    use crate::sys;
    let mut ws = sys::Winsize {
        ws_row: size.rows,
        ws_col: size.columns,
        ws_xpixel: size.xpixel,
        ws_ypixel: size.ypixel,
    };
    loop {
        // SAFETY: ws is valid for TIOCSWINSZ.
        let ret = unsafe { sys::ioctl(fd, sys::TIOCSWINSZ, &mut ws as *mut sys::Winsize) };
        if ret == 0 {
            return Ok(());
        }
        let err = sys::errno();
        if err == sys::EINTR {
            continue;
        }
        return Err(io::Error::from_raw_os_error(err));
    }
}

#[cfg(not(target_os = "linux"))]
pub fn set_window_size(_fd: RawFd, _size: WindowSize) -> io::Result<()> {
    Err(crate::unsupported("set_window_size"))
}

/// `pty_resize()` from child-monitor.c: `EBADF` and `ENOTTY` are ignored so a
/// child whose pty has already gone away does not produce an error.
#[cfg(target_os = "linux")]
pub fn resize_pty(fd: RawFd, size: WindowSize) -> io::Result<()> {
    use crate::sys;
    match set_window_size(fd, size) {
        Ok(()) => Ok(()),
        Err(err)
            if err.raw_os_error() == Some(sys::EBADF)
                || err.raw_os_error() == Some(sys::ENOTTY) =>
        {
            Ok(())
        }
        Err(err) => Err(err),
    }
}

#[cfg(not(target_os = "linux"))]
pub fn resize_pty(_fd: RawFd, _size: WindowSize) -> io::Result<()> {
    Err(crate::unsupported("resize_pty"))
}

#[cfg(target_os = "linux")]
pub fn window_size(fd: RawFd) -> io::Result<WindowSize> {
    use crate::sys;
    let mut ws = sys::Winsize::default();
    loop {
        // SAFETY: ws is valid for TIOCGWINSZ.
        let ret = unsafe { sys::ioctl(fd, sys::TIOCGWINSZ, &mut ws as *mut sys::Winsize) };
        if ret == 0 {
            return Ok(WindowSize {
                rows: ws.ws_row,
                columns: ws.ws_col,
                xpixel: ws.ws_xpixel,
                ypixel: ws.ws_ypixel,
            });
        }
        let err = sys::errno();
        if err == sys::EINTR {
            continue;
        }
        return Err(io::Error::from_raw_os_error(err));
    }
}

#[cfg(not(target_os = "linux"))]
pub fn window_size(_fd: RawFd) -> io::Result<WindowSize> {
    Err(crate::unsupported("window_size"))
}

#[cfg(target_os = "linux")]
pub fn get_termios(fd: RawFd) -> io::Result<Termios> {
    use crate::sys;
    let mut t = Termios::default();
    loop {
        // SAFETY: t is valid writable storage.
        let ret = unsafe { sys::tcgetattr(fd, &mut t) };
        if ret == 0 {
            return Ok(t);
        }
        let err = sys::errno();
        if err == sys::EINTR {
            continue;
        }
        return Err(io::Error::from_raw_os_error(err));
    }
}

#[cfg(not(target_os = "linux"))]
pub fn get_termios(_fd: RawFd) -> io::Result<Termios> {
    Err(crate::unsupported("get_termios"))
}

#[cfg(target_os = "linux")]
pub fn set_termios(fd: RawFd, termios: &Termios) -> io::Result<()> {
    use crate::sys;
    loop {
        // SAFETY: termios is a valid pointer for TCSANOW.
        let ret = unsafe { sys::tcsetattr(fd, sys::TCSANOW, termios) };
        if ret == 0 {
            return Ok(());
        }
        let err = sys::errno();
        if err == sys::EINTR {
            continue;
        }
        return Err(io::Error::from_raw_os_error(err));
    }
}

#[cfg(not(target_os = "linux"))]
pub fn set_termios(_fd: RawFd, _termios: &Termios) -> io::Result<()> {
    Err(crate::unsupported("set_termios"))
}

/// Put `fd` in raw mode, returning the previous termios state.
#[cfg(target_os = "linux")]
pub fn enter_raw_mode(fd: RawFd) -> io::Result<Termios> {
    use crate::sys;
    let old = get_termios(fd)?;
    let mut raw = old;
    // SAFETY: raw is a valid termios value.
    unsafe { sys::cfmakeraw(&mut raw) };
    set_termios(fd, &raw)?;
    Ok(old)
}

#[cfg(not(target_os = "linux"))]
pub fn enter_raw_mode(_fd: RawFd) -> io::Result<Termios> {
    Err(crate::unsupported("enter_raw_mode"))
}

#[cfg(target_os = "linux")]
pub fn restore_termios(fd: RawFd, termios: &Termios) -> io::Result<()> {
    set_termios(fd, termios)
}

#[cfg(not(target_os = "linux"))]
pub fn restore_termios(_fd: RawFd, _termios: &Termios) -> io::Result<()> {
    Err(crate::unsupported("restore_termios"))
}

#[cfg(target_os = "linux")]
pub fn set_nonblocking(fd: RawFd, nonblocking: bool) -> io::Result<()> {
    use crate::sys;
    // SAFETY: fcntl get/set with integer arguments.
    let flags = unsafe { sys::fcntl(fd, sys::F_GETFL) };
    if flags < 0 {
        return Err(sys::last_os_error());
    }
    let new_flags = if nonblocking {
        flags | sys::O_NONBLOCK
    } else {
        flags & !sys::O_NONBLOCK
    };
    if new_flags != flags {
        // SAFETY: as above.
        sys::cvt(unsafe { sys::fcntl(fd, sys::F_SETFL, new_flags) })?;
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub fn set_nonblocking(_fd: RawFd, _nonblocking: bool) -> io::Result<()> {
    Err(crate::unsupported("set_nonblocking"))
}

#[cfg(target_os = "linux")]
pub fn set_cloexec(fd: RawFd, cloexec: bool) -> io::Result<()> {
    use crate::sys;
    // SAFETY: fcntl get/set with integer arguments.
    let flags = unsafe { sys::fcntl(fd, sys::F_GETFD) };
    if flags < 0 {
        return Err(sys::last_os_error());
    }
    let new_flags = if cloexec {
        flags | sys::FD_CLOEXEC
    } else {
        flags & !sys::FD_CLOEXEC
    };
    if new_flags != flags {
        // SAFETY: as above.
        sys::cvt(unsafe { sys::fcntl(fd, sys::F_SETFD, new_flags) })?;
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub fn set_cloexec(_fd: RawFd, _cloexec: bool) -> io::Result<()> {
    Err(crate::unsupported("set_cloexec"))
}

#[cfg(target_os = "linux")]
pub fn is_cloexec(fd: RawFd) -> io::Result<bool> {
    use crate::sys;
    // SAFETY: fcntl get with integer argument.
    let flags = unsafe { sys::fcntl(fd, sys::F_GETFD) };
    if flags < 0 {
        Err(sys::last_os_error())
    } else {
        Ok(flags & sys::FD_CLOEXEC != 0)
    }
}

#[cfg(not(target_os = "linux"))]
pub fn is_cloexec(_fd: RawFd) -> io::Result<bool> {
    Err(crate::unsupported("is_cloexec"))
}

#[cfg(target_os = "linux")]
pub fn is_tty(fd: RawFd) -> bool {
    use crate::sys;
    // SAFETY: isatty only inspects the descriptor.
    unsafe { sys::isatty(fd) == 1 }
}

#[cfg(not(target_os = "linux"))]
pub fn is_tty(_fd: RawFd) -> bool {
    false
}
