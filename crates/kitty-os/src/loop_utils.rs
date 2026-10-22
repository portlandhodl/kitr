// SPDX-License-Identifier: GPL-3.0-or-later
// Ported from kitty/loop-utils.c, kitty/loop-utils.h,
// kitty/safe-wrappers.h and kitty/threading.h,
// Copyright (C) 2019-2024 Kovid Goyal <kovid at kovidgoyal.net>

//! Event-loop primitives: wakeup fd, self-pipes, signalfd, poll and the
//! EINTR/EAGAIN-aware read/write helpers from `safe-wrappers.h`.
//!
//! On Linux the wakeup fd is an `eventfd` and [`LoopData`] uses `signalfd`,
//! exactly like `init_loop_data()` in C. [`SignalPipe`] is the self-pipe
//! alternative used by the child monitor for SIGCHLD; it installs a
//! process-wide handler whose only action is `write()` on a nonblocking pipe.
//!
//! Every other platform gets the same public API with `Unsupported` errors.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

pub use crate::pty::{is_cloexec, set_cloexec, set_nonblocking};

pub const POLLIN: i16 = 0x001;
pub const POLLOUT: i16 = 0x004;
pub const POLLERR: i16 = 0x008;
pub const POLLHUP: i16 = 0x010;
pub const POLLNVAL: i16 = 0x020;

/// Portable `struct pollfd`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct PollFd {
    pub fd: RawFd,
    pub events: i16,
    pub revents: i16,
}

impl PollFd {
    pub fn new(fd: RawFd, events: i16) -> Self {
        Self {
            fd,
            events,
            revents: 0,
        }
    }

    pub fn has_event(&self, event: i16) -> bool {
        self.revents & event != 0
    }
}

/// Result of a single nonblocking read/write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IoOutcome {
    /// Bytes transferred. `0` from a read means EOF.
    Completed(usize),
    WouldBlock,
    Interrupted,
}

impl IoOutcome {
    pub fn is_eof(self) -> bool {
        self == Self::Completed(0)
    }
}

/// `poll()` with EINTR handling. The timeout is recalculated after a signal so
/// the total wait does not exceed the requested timeout.
#[cfg(target_os = "linux")]
pub fn poll(fds: &mut [PollFd], timeout_ms: i32) -> io::Result<usize> {
    use crate::monotonic::{monotonic, MONOTONIC_T_1E6};
    use crate::sys;

    let deadline = if timeout_ms >= 0 {
        Some(monotonic().saturating_add((timeout_ms as i64) * MONOTONIC_T_1E6))
    } else {
        None
    };
    let mut raw: Vec<sys::PollFd> = fds
        .iter()
        .map(|p| sys::PollFd {
            fd: p.fd,
            events: p.events,
            revents: 0,
        })
        .collect();
    loop {
        let this_timeout = match deadline {
            Some(deadline) => {
                let remaining = deadline - monotonic();
                if remaining <= 0 {
                    0
                } else {
                    ((remaining + MONOTONIC_T_1E6 - 1) / MONOTONIC_T_1E6).min(i32::MAX as i64)
                        as i32
                }
            }
            None => -1,
        };
        // SAFETY: raw is a valid array of pollfd.
        let ret = unsafe { sys::poll(raw.as_mut_ptr(), raw.len() as sys::CULong, this_timeout) };
        if ret >= 0 {
            for (dst, src) in fds.iter_mut().zip(raw.iter()) {
                dst.revents = src.revents;
            }
            return Ok(ret as usize);
        }
        let err = sys::errno();
        if err == sys::EINTR {
            if matches!(deadline, Some(d) if monotonic() >= d) {
                for p in fds.iter_mut() {
                    p.revents = 0;
                }
                return Ok(0);
            }
            continue;
        }
        return Err(io::Error::from_raw_os_error(err));
    }
}

#[cfg(not(target_os = "linux"))]
pub fn poll(_fds: &mut [PollFd], _timeout_ms: i32) -> io::Result<usize> {
    Err(crate::unsupported("poll"))
}

/// One nonblocking `read`.
#[cfg(target_os = "linux")]
pub fn read_once(fd: RawFd, buf: &mut [u8]) -> io::Result<IoOutcome> {
    use crate::sys;
    use core::ffi::c_void;
    // SAFETY: buf is valid for its length.
    let ret = unsafe { sys::read(fd, buf.as_mut_ptr() as *mut c_void, buf.len()) };
    if ret >= 0 {
        return Ok(IoOutcome::Completed(ret as usize));
    }
    let err = sys::errno();
    if err == sys::EINTR {
        Ok(IoOutcome::Interrupted)
    } else if err == sys::EAGAIN || err == sys::EWOULDBLOCK {
        Ok(IoOutcome::WouldBlock)
    } else {
        Err(io::Error::from_raw_os_error(err))
    }
}

#[cfg(not(target_os = "linux"))]
pub fn read_once(_fd: RawFd, _buf: &mut [u8]) -> io::Result<IoOutcome> {
    Err(crate::unsupported("read_once"))
}

/// One nonblocking `write`.
#[cfg(target_os = "linux")]
pub fn write_once(fd: RawFd, buf: &[u8]) -> io::Result<IoOutcome> {
    use crate::sys;
    use core::ffi::c_void;
    // SAFETY: buf is valid for its length.
    let ret = unsafe { sys::write(fd, buf.as_ptr() as *const c_void, buf.len()) };
    if ret >= 0 {
        return Ok(IoOutcome::Completed(ret as usize));
    }
    let err = sys::errno();
    if err == sys::EINTR {
        Ok(IoOutcome::Interrupted)
    } else if err == sys::EAGAIN || err == sys::EWOULDBLOCK {
        Ok(IoOutcome::WouldBlock)
    } else {
        Err(io::Error::from_raw_os_error(err))
    }
}

#[cfg(not(target_os = "linux"))]
pub fn write_once(_fd: RawFd, _buf: &[u8]) -> io::Result<IoOutcome> {
    Err(crate::unsupported("write_once"))
}

/// Read once, retrying only EINTR. EOF is `Ok(0)`; EAGAIN is returned as an
/// error so callers can distinguish it.
#[cfg(target_os = "linux")]
pub fn read_retry(fd: RawFd, buf: &mut [u8]) -> io::Result<usize> {
    loop {
        match read_once(fd, buf)? {
            IoOutcome::Completed(n) => return Ok(n),
            IoOutcome::Interrupted => continue,
            IoOutcome::WouldBlock => {
                return Err(io::Error::from(io::ErrorKind::WouldBlock));
            }
        }
    }
}

#[cfg(not(target_os = "linux"))]
pub fn read_retry(_fd: RawFd, _buf: &mut [u8]) -> io::Result<usize> {
    Err(crate::unsupported("read_retry"))
}

/// Write as much as possible, retrying EINTR, stopping on EAGAIN. Returns the
/// number of bytes written (which may be less than `buf.len()`).
#[cfg(target_os = "linux")]
pub fn write_all(fd: RawFd, buf: &[u8]) -> io::Result<usize> {
    let mut written = 0;
    while written < buf.len() {
        match write_once(fd, &buf[written..])? {
            IoOutcome::Completed(0) => {
                return Err(io::Error::from_raw_os_error(crate::sys::EIO));
            }
            IoOutcome::Completed(n) => written += n,
            IoOutcome::Interrupted => continue,
            IoOutcome::WouldBlock => break,
        }
    }
    Ok(written)
}

#[cfg(not(target_os = "linux"))]
pub fn write_all(_fd: RawFd, _buf: &[u8]) -> io::Result<usize> {
    Err(crate::unsupported("write_all"))
}

/// `drain_fd()` from loop-utils.h.
#[cfg(target_os = "linux")]
pub fn drain_fd(fd: RawFd) {
    let mut buf = [0u8; 1024];
    loop {
        match read_once(fd, &mut buf) {
            Ok(IoOutcome::Completed(n)) if n > 0 => continue,
            Ok(IoOutcome::Interrupted) => continue,
            _ => break,
        }
    }
}

#[cfg(not(target_os = "linux"))]
pub fn drain_fd(_fd: RawFd) {}

#[cfg(target_os = "linux")]
fn wait_readable(fd: RawFd, timeout_ms: i32) -> io::Result<bool> {
    let mut pfd = [PollFd::new(fd, POLLIN)];
    Ok(poll(&mut pfd, timeout_ms)? > 0 && pfd[0].has_event(POLLIN))
}

#[cfg(not(target_os = "linux"))]
fn wait_readable(_fd: RawFd, _timeout_ms: i32) -> io::Result<bool> {
    Err(crate::unsupported("wait_readable"))
}

fn make_pipe(flags: i32) -> io::Result<(OwnedFd, OwnedFd)> {
    #[cfg(target_os = "linux")]
    {
        use crate::sys;
        let mut fds = [0i32; 2];
        // SAFETY: fds is valid for two descriptors.
        let ret = unsafe { sys::pipe2(fds.as_mut_ptr(), flags) };
        if ret != 0 {
            return Err(sys::last_os_error());
        }
        // SAFETY: both are fresh owned descriptors.
        Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = flags;
        Err(crate::unsupported("make_pipe"))
    }
}

/// `self_pipe()`: close-on-exec and, optionally, nonblocking.
#[derive(Debug)]
pub struct SelfPipe {
    read: OwnedFd,
    write: OwnedFd,
}

impl SelfPipe {
    pub fn new(nonblocking: bool) -> io::Result<Self> {
        #[cfg(target_os = "linux")]
        {
            use crate::sys;
            let flags = sys::O_CLOEXEC | if nonblocking { sys::O_NONBLOCK } else { 0 };
            let (read, write) = make_pipe(flags)?;
            Ok(Self { read, write })
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = nonblocking;
            Err(crate::unsupported("SelfPipe::new"))
        }
    }

    pub fn read_fd(&self) -> RawFd {
        self.read.as_raw_fd()
    }

    pub fn write_fd(&self) -> RawFd {
        self.write.as_raw_fd()
    }

    pub fn write_byte(&self, byte: u8) -> io::Result<()> {
        match write_once(self.write.as_raw_fd(), &[byte])? {
            IoOutcome::Completed(1) => Ok(()),
            IoOutcome::Completed(_) => Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "self-pipe write returned zero",
            )),
            IoOutcome::WouldBlock => Ok(()),
            IoOutcome::Interrupted => self.write_byte(byte),
        }
    }

    pub fn wait_readable(&self, timeout_ms: i32) -> io::Result<bool> {
        wait_readable(self.read.as_raw_fd(), timeout_ms)
    }

    pub fn drain(&self) {
        drain_fd(self.read.as_raw_fd());
    }
}

/// `eventfd`-backed wakeup descriptor (`wakeup_read_fd` in LoopData).
#[derive(Debug)]
pub struct WakeupFd {
    fd: OwnedFd,
}

impl WakeupFd {
    pub fn new() -> io::Result<Self> {
        #[cfg(target_os = "linux")]
        {
            use crate::sys;
            // SAFETY: eventfd has no preconditions.
            let fd = unsafe { sys::eventfd(0, sys::EFD_CLOEXEC | sys::EFD_NONBLOCK) };
            if fd < 0 {
                return Err(sys::last_os_error());
            }
            // SAFETY: fd is fresh and owned.
            Ok(Self {
                fd: unsafe { OwnedFd::from_raw_fd(fd) },
            })
        }
        #[cfg(not(target_os = "linux"))]
        {
            Err(crate::unsupported("WakeupFd::new"))
        }
    }

    pub fn read_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }

    /// `wakeup_loop()`: increment the counter. A full counter means a wakeup
    /// is already pending, which is success.
    #[cfg(target_os = "linux")]
    pub fn wake(&self) -> io::Result<()> {
        use crate::sys;
        use core::ffi::c_void;
        let value = 1u64;
        loop {
            // SAFETY: value is a valid 8-byte buffer.
            let ret = unsafe {
                sys::write(
                    self.fd.as_raw_fd(),
                    &value as *const u64 as *const c_void,
                    core::mem::size_of::<u64>(),
                )
            };
            if ret >= 0 {
                return Ok(());
            }
            let err = sys::errno();
            if err == sys::EINTR {
                continue;
            }
            if err == sys::EAGAIN || err == sys::EWOULDBLOCK {
                return Ok(());
            }
            return Err(io::Error::from_raw_os_error(err));
        }
    }

    #[cfg(not(target_os = "linux"))]
    pub fn wake(&self) -> io::Result<()> {
        Err(crate::unsupported("WakeupFd::wake"))
    }

    #[cfg(target_os = "linux")]
    pub fn drain(&self) -> io::Result<()> {
        use crate::sys;
        use core::ffi::c_void;
        let mut value = 0u64;
        loop {
            // SAFETY: value is valid writable storage.
            let ret = unsafe {
                sys::read(
                    self.fd.as_raw_fd(),
                    &mut value as *mut u64 as *mut c_void,
                    core::mem::size_of::<u64>(),
                )
            };
            if ret >= 0 {
                if ret < core::mem::size_of::<u64>() as isize {
                    return Ok(());
                }
                // Multiple wakes may have accumulated.
                continue;
            }
            let err = sys::errno();
            if err == sys::EINTR {
                continue;
            }
            if err == sys::EAGAIN || err == sys::EWOULDBLOCK {
                return Ok(());
            }
            return Err(io::Error::from_raw_os_error(err));
        }
    }

    #[cfg(not(target_os = "linux"))]
    pub fn drain(&self) -> io::Result<()> {
        Err(crate::unsupported("WakeupFd::drain"))
    }
}

/// Decoded `signalfd_siginfo` (the fields Python's `SigInfo` exposes).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SigInfo {
    pub signo: i32,
    pub code: i32,
    pub pid: i32,
    pub uid: u32,
    pub status: i32,
    pub int: i32,
    pub addr: u64,
}

/// `LoopData`: wakeup eventfd + optional signalfd.
#[derive(Debug)]
pub struct LoopData {
    wakeup: WakeupFd,
    signal_read: Option<OwnedFd>,
    handled_signals: Vec<i32>,
    #[cfg(target_os = "linux")]
    previous_mask: Option<crate::sys::SigSet>,
}

impl LoopData {
    /// `init_loop_data()` for the supplied signal list (SIGCHLD, etc.).
    #[cfg(target_os = "linux")]
    pub fn new(signals: &[i32]) -> io::Result<Self> {
        use crate::sys;
        let wakeup = WakeupFd::new()?;
        let mut set = sys::SigSet::empty();
        for &sig in signals {
            set.add(sig);
        }
        let mut previous_mask = None;
        let signal_read = if signals.is_empty() {
            None
        } else {
            let mut old = sys::SigSet::empty();
            // SAFETY: pointers are valid signal sets.
            let ret = unsafe { sys::sigprocmask(sys::SIG_BLOCK, &set, &mut old) };
            if ret != 0 {
                return Err(sys::last_os_error());
            }
            // SAFETY: signalfd takes a valid mask and flags.
            let fd = unsafe { sys::signalfd(-1, &set, sys::SFD_NONBLOCK | sys::SFD_CLOEXEC) };
            if fd < 0 {
                let err = sys::last_os_error();
                // SAFETY: restoring the mask we just changed.
                unsafe {
                    sys::sigprocmask(sys::SIG_SETMASK, &old, core::ptr::null_mut());
                }
                return Err(err);
            }
            previous_mask = Some(old);
            // SAFETY: fd is fresh and owned.
            Some(unsafe { OwnedFd::from_raw_fd(fd) })
        };
        Ok(Self {
            wakeup,
            signal_read,
            handled_signals: signals.to_vec(),
            previous_mask,
        })
    }

    #[cfg(not(target_os = "linux"))]
    pub fn new(_signals: &[i32]) -> io::Result<Self> {
        Err(crate::unsupported("LoopData::new"))
    }

    pub fn wakeup_read_fd(&self) -> RawFd {
        self.wakeup.read_fd()
    }

    pub fn signal_read_fd(&self) -> RawFd {
        self.signal_read.as_ref().map_or(-1, AsRawFd::as_raw_fd)
    }

    pub fn handled_signals(&self) -> &[i32] {
        &self.handled_signals
    }

    pub fn wakeup(&self) -> io::Result<()> {
        self.wakeup.wake()
    }

    pub fn drain_wakeup(&self) -> io::Result<()> {
        self.wakeup.drain()
    }

    /// `read_signals()`: drain all queued `signalfd_siginfo` records.
    #[cfg(target_os = "linux")]
    pub fn read_signals(&self) -> io::Result<Vec<SigInfo>> {
        use crate::sys;
        use core::ffi::c_void;
        let Some(fd) = self.signal_read.as_ref() else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        loop {
            let mut records = [sys::SignalfdSiginfo {
                ssi_signo: 0,
                ssi_errno: 0,
                ssi_code: 0,
                ssi_pid: 0,
                ssi_uid: 0,
                ssi_fd: 0,
                ssi_tid: 0,
                ssi_band: 0,
                ssi_overrun: 0,
                ssi_trapno: 0,
                ssi_status: 0,
                ssi_int: 0,
                ssi_ptr: 0,
                ssi_utime: 0,
                ssi_stime: 0,
                ssi_addr: 0,
                pad: [0; 48],
            }; 32];
            // SAFETY: records is valid for its whole size.
            let ret = unsafe {
                sys::read(
                    fd.as_raw_fd(),
                    records.as_mut_ptr() as *mut c_void,
                    core::mem::size_of_val(&records),
                )
            };
            if ret < 0 {
                let err = sys::errno();
                if err == sys::EINTR {
                    continue;
                }
                if err == sys::EAGAIN || err == sys::EWOULDBLOCK {
                    return Ok(out);
                }
                return Err(io::Error::from_raw_os_error(err));
            }
            if ret == 0 {
                return Ok(out);
            }
            let count = ret as usize / core::mem::size_of::<sys::SignalfdSiginfo>();
            if count == 0 || count * core::mem::size_of::<sys::SignalfdSiginfo>() != ret as usize {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "incomplete signal read from signalfd",
                ));
            }
            for r in &records[..count] {
                out.push(SigInfo {
                    signo: r.ssi_signo as i32,
                    code: r.ssi_code,
                    pid: r.ssi_pid as i32,
                    uid: r.ssi_uid,
                    status: r.ssi_status,
                    int: r.ssi_int,
                    addr: r.ssi_addr,
                });
            }
        }
    }

    #[cfg(not(target_os = "linux"))]
    pub fn read_signals(&self) -> io::Result<Vec<SigInfo>> {
        Err(crate::unsupported("LoopData::read_signals"))
    }
}

#[cfg(target_os = "linux")]
impl Drop for LoopData {
    fn drop(&mut self) {
        use crate::sys;
        if let Some(old) = self.previous_mask.take() {
            // The signalfd is closed by the OwnedFd drop after this. Restore
            // the thread's previous mask so a blocking SIGCHLD does not leak.
            // SAFETY: old is a valid signal set.
            unsafe {
                sys::sigprocmask(sys::SIG_SETMASK, &old, core::ptr::null_mut());
            }
        }
    }
}

#[cfg(target_os = "linux")]
static SIGNAL_WRITE_FD: core::sync::atomic::AtomicI32 = core::sync::atomic::AtomicI32::new(-1);

#[cfg(target_os = "linux")]
extern "C" fn signal_pipe_handler(_sig: i32) {
    use core::sync::atomic::Ordering;
    let fd = SIGNAL_WRITE_FD.load(Ordering::Relaxed);
    if fd >= 0 {
        let byte = 1u8;
        // SAFETY: write() is async-signal-safe; the fd is owned by the live
        // SignalPipe (or may be racing with its close, in which case write
        // simply fails and is ignored, as C does).
        unsafe {
            crate::sys::write(fd, &byte as *const u8 as *const core::ffi::c_void, 1);
        }
    }
}

/// A self-pipe driven by a process-wide signal handler. Only one may be
/// installed at a time; the child monitor owns the SIGCHLD one.
#[cfg(target_os = "linux")]
#[derive(Debug)]
pub struct SignalPipe {
    pipe: SelfPipe,
    sig: i32,
    previous: usize,
}

#[cfg(target_os = "linux")]
impl SignalPipe {
    pub fn install(sig: i32) -> io::Result<Self> {
        use crate::sys;
        use core::sync::atomic::Ordering;
        if SIGNAL_WRITE_FD
            .compare_exchange(-1, -2, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "a SignalPipe is already installed",
            ));
        }
        let pipe = match SelfPipe::new(true) {
            Ok(p) => p,
            Err(e) => {
                SIGNAL_WRITE_FD.store(-1, Ordering::SeqCst);
                return Err(e);
            }
        };
        SIGNAL_WRITE_FD.store(pipe.write_fd(), Ordering::SeqCst);
        // SAFETY: signal() takes a signal number and a handler address.
        let previous = unsafe { sys::signal(sig, signal_pipe_handler as *const () as usize) };
        if previous == usize::MAX {
            // SIG_ERR. Undo the global registration before reporting.
            SIGNAL_WRITE_FD.store(-1, Ordering::SeqCst);
            return Err(sys::last_os_error());
        }
        Ok(Self {
            pipe,
            sig,
            previous,
        })
    }

    pub fn read_fd(&self) -> RawFd {
        self.pipe.read_fd()
    }

    pub fn wait_readable(&self, timeout_ms: i32) -> io::Result<bool> {
        self.pipe.wait_readable(timeout_ms)
    }

    pub fn drain(&self) {
        self.pipe.drain();
    }
}

#[cfg(target_os = "linux")]
impl Drop for SignalPipe {
    fn drop(&mut self) {
        use crate::sys;
        use core::sync::atomic::Ordering;
        SIGNAL_WRITE_FD.store(-1, Ordering::SeqCst);
        // SAFETY: restoring the handler saved at install time.
        unsafe {
            sys::signal(self.sig, self.previous);
        }
    }
}

#[cfg(not(target_os = "linux"))]
#[derive(Debug)]
pub struct SignalPipe {
    _private: (),
}

#[cfg(not(target_os = "linux"))]
impl SignalPipe {
    pub fn install(_sig: i32) -> io::Result<Self> {
        Err(crate::unsupported("SignalPipe::install"))
    }

    pub fn read_fd(&self) -> RawFd {
        -1
    }

    pub fn wait_readable(&self, _timeout_ms: i32) -> io::Result<bool> {
        Err(crate::unsupported("SignalPipe::wait_readable"))
    }

    pub fn drain(&self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn self_pipe_wakes_poll_and_drains() {
        let pipe = SelfPipe::new(true).expect("self pipe");
        let mut fds = [PollFd::new(pipe.read_fd(), POLLIN)];
        assert_eq!(poll(&mut fds, 0).unwrap(), 0);
        pipe.write_byte(b'w').unwrap();
        assert_eq!(poll(&mut fds, 1000).unwrap(), 1);
        assert!(fds[0].has_event(POLLIN));
        pipe.drain();
        assert_eq!(poll(&mut fds, 0).unwrap(), 0);
    }

    #[test]
    fn eventfd_wakeup_coalesces() {
        let wake = WakeupFd::new().expect("eventfd");
        let mut fds = [PollFd::new(wake.read_fd(), POLLIN)];
        wake.wake().unwrap();
        wake.wake().unwrap();
        assert_eq!(poll(&mut fds, 1000).unwrap(), 1);
        wake.drain().unwrap();
        assert_eq!(poll(&mut fds, 0).unwrap(), 0);
    }

    #[test]
    fn nonblocking_pipe_reports_would_block() {
        let pipe = SelfPipe::new(true).expect("self pipe");
        let mut buf = [0u8; 8];
        assert_eq!(
            read_once(pipe.read_fd(), &mut buf).unwrap(),
            IoOutcome::WouldBlock
        );
        let mut written = 0usize;
        loop {
            match write_once(pipe.write_fd(), &[0u8; 4096]).unwrap() {
                IoOutcome::Completed(n) => written += n,
                IoOutcome::WouldBlock => break,
                IoOutcome::Interrupted => continue,
            }
        }
        assert!(written > 0);
        assert_eq!(
            write_once(pipe.write_fd(), b"x").unwrap(),
            IoOutcome::WouldBlock
        );
        assert!(read_retry(pipe.read_fd(), &mut buf).unwrap() > 0);
    }

    #[test]
    fn read_retry_reports_eof() {
        let (r, w) = make_pipe(0).expect("pipe");
        drop(w);
        let mut buf = [0u8; 4];
        let n = read_retry(r.as_raw_fd(), &mut buf).unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn cloexec_and_nonblocking_helpers_round_trip() {
        let pipe = SelfPipe::new(false).expect("self pipe");
        assert!(is_cloexec(pipe.read_fd()).unwrap());
        set_cloexec(pipe.read_fd(), false).unwrap();
        assert!(!is_cloexec(pipe.read_fd()).unwrap());
        set_nonblocking(pipe.read_fd(), true).unwrap();
        let mut buf = [0u8; 1];
        assert_eq!(
            read_once(pipe.read_fd(), &mut buf).unwrap(),
            IoOutcome::WouldBlock
        );
    }

    #[test]
    fn loop_data_blocks_and_reads_signals() {
        let loop_data = LoopData::new(&[crate::sys::SIGCHLD]).expect("loop data");
        assert!(loop_data.signal_read_fd() >= 0);
        loop_data.wakeup().unwrap();
        let mut fds = [PollFd::new(loop_data.wakeup_read_fd(), POLLIN)];
        assert_eq!(poll(&mut fds, 1000).unwrap(), 1);
        loop_data.drain_wakeup().unwrap();
        assert!(loop_data.handled_signals().contains(&crate::sys::SIGCHLD));
    }
}
