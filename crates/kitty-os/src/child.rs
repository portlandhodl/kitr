// SPDX-License-Identifier: GPL-3.0-or-later
// Ported from kitty/child.c, Copyright (C) 2018 Kovid Goyal <kovid at kovidgoyal.net>

//! Fork/exec of a child attached to a PTY.
//!
//! The sequence in the child is the same as `spawn()` in `kitty/child.c`:
//! reset the handled signals, clear the signal mask, `chdir` with the same
//! fallback to `/`, `setsid()`, open the pty slave and make it the controlling
//! terminal with `TIOCSCTTY`, wire stdin/stdout/stderr, close every unrelated
//! descriptor, optionally wait for the ready signal, then `execvp()` with a
//! replaced `environ`.
//!
//! Deviations from the C code, all deliberate:
//!
//! * exec failures are reported structurally through an `O_CLOEXEC` pipe
//!   (an 8-byte `{stage, errno}` record) instead of relying on the
//!   `kitten __hold_till_enter__` fallback, which this crate does not have;
//! * the forwarded stdio descriptors are duplicated in the parent, not the
//!   child, so that the `KITTY_STDIO_FORWARDED` environment value can be
//!   formatted without allocating after `fork()`. The descriptors inherited
//!   by the child are identical;
//! * the error pipe is preserved from the "close unrelated fds" loop so a
//!   failed `exec` can still be reported.

use std::ffi::{CString, OsStr, OsString};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::time::Duration;

use crate::monotonic::{monotonic, Monotonic, MONOTONIC_T_1E6};
use crate::pty::{self, WindowSize};

pub type Pid = i32;

/// Decoded `wait(2)` status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitStatus {
    Exited(i32),
    Signaled(i32),
    Stopped(i32),
    Continued,
    Unknown(i32),
}

impl ExitStatus {
    pub fn success(self) -> bool {
        self == Self::Exited(0)
    }

    pub fn code(self) -> Option<i32> {
        match self {
            Self::Exited(code) => Some(code),
            _ => None,
        }
    }

    pub fn signal(self) -> Option<i32> {
        match self {
            Self::Signaled(sig) => Some(sig),
            _ => None,
        }
    }
}

/// `WIFEXITED`/`WEXITSTATUS`/`WIFSIGNALED`/`WTERMSIG`/`WIFSTOPPED`/`WIFCONTINUED`.
#[cfg(target_os = "linux")]
pub fn decode_wait_status(status: i32) -> ExitStatus {
    use crate::sys;
    if sys::wifexited(status) {
        ExitStatus::Exited(sys::wexitstatus(status))
    } else if sys::wifsignaled(status) {
        ExitStatus::Signaled(sys::wtermsig(status))
    } else if sys::wifstopped(status) {
        ExitStatus::Stopped(sys::wstopsig(status))
    } else if sys::wifcontinued(status) {
        ExitStatus::Continued
    } else {
        ExitStatus::Unknown(status)
    }
}

#[cfg(not(target_os = "linux"))]
pub fn decode_wait_status(status: i32) -> ExitStatus {
    ExitStatus::Unknown(status)
}

/// Where the child failed before `exec`.
#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecStage {
    None = 0,
    Sigprocmask = 1,
    Setsid = 2,
    OpenTerminal = 3,
    SetControllingTerminal = 4,
    DupStdout = 5,
    DupStderr = 6,
    DupStdin = 7,
    Exec = 8,
}

impl ExecStage {
    pub fn from_raw(raw: u32) -> Self {
        match raw {
            1 => Self::Sigprocmask,
            2 => Self::Setsid,
            3 => Self::OpenTerminal,
            4 => Self::SetControllingTerminal,
            5 => Self::DupStdout,
            6 => Self::DupStderr,
            7 => Self::DupStdin,
            8 => Self::Exec,
            _ => Self::None,
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::None => "unknown stage",
            Self::Sigprocmask => "sigprocmask() in child process failed",
            Self::Setsid => "setsid() in child process failed",
            Self::OpenTerminal => "Failed to open controlling terminal",
            Self::SetControllingTerminal => "Failed to set controlling terminal with TIOCSCTTY",
            Self::DupStdout => "dup2() failed for fd number 1",
            Self::DupStderr => "dup2() failed for fd number 2",
            Self::DupStdin => "dup2() failed for fd number 0",
            Self::Exec => "Failed to launch child",
        }
    }
}

/// Contents of the exec-error pipe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExecFailure {
    pub stage: ExecStage,
    pub errno: i32,
}

impl ExecFailure {
    pub fn error(self) -> io::Error {
        io::Error::from_raw_os_error(self.errno)
    }
}

impl core::fmt::Display for ExecFailure {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "{}: {}",
            self.stage.description(),
            io::Error::from_raw_os_error(self.errno)
        )
    }
}

impl std::error::Error for ExecFailure {}

/// State of the asynchronous exec handshake.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExecStatus {
    /// The child has not exec'ed yet (it may be waiting for the ready signal).
    Pending,
    /// `execvp` returned successfully; the CLOEXEC error pipe reached EOF.
    Succeeded,
    /// `execvp` failed.
    Failed(ExecFailure),
}

/// Builder for [`spawn`].
#[derive(Clone, Debug)]
pub struct SpawnConfig {
    pub exe: OsString,
    pub argv: Vec<OsString>,
    pub env: Vec<OsString>,
    pub cwd: Option<PathBuf>,
    pub stdin_pipe: bool,
    pub ready_pipe: bool,
    pub forward_stdio: bool,
    pub pass_fds: Vec<RawFd>,
    pub handled_signals: Vec<i32>,
    pub initial_window_size: Option<WindowSize>,
}

impl SpawnConfig {
    pub fn new(exe: impl Into<OsString>) -> Self {
        let exe = exe.into();
        Self {
            argv: vec![exe.clone()],
            exe,
            env: Vec::new(),
            cwd: None,
            stdin_pipe: false,
            ready_pipe: false,
            forward_stdio: false,
            pass_fds: Vec::new(),
            handled_signals: Vec::new(),
            initial_window_size: None,
        }
    }

    pub fn arg(mut self, arg: impl Into<OsString>) -> Self {
        self.argv.push(arg.into());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        self.argv.extend(args.into_iter().map(Into::into));
        self
    }

    /// Set `argv[0]` without changing the executable.
    pub fn argv0(mut self, arg0: impl Into<OsString>) -> Self {
        if self.argv.is_empty() {
            self.argv.push(arg0.into());
        } else {
            self.argv[0] = arg0.into();
        }
        self
    }

    pub fn env(mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> Self {
        let mut entry = key.as_ref().to_os_string();
        entry.push("=");
        entry.push(value);
        self.env.push(entry);
        self
    }

    pub fn envs<I, K, V>(mut self, vars: I) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<OsStr>,
        V: AsRef<OsStr>,
    {
        for (k, v) in vars {
            self = self.env(k, v);
        }
        self
    }

    pub fn current_dir(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    pub fn stdin_pipe(mut self, enabled: bool) -> Self {
        self.stdin_pipe = enabled;
        self
    }

    pub fn ready_pipe(mut self, enabled: bool) -> Self {
        self.ready_pipe = enabled;
        self
    }

    pub fn forward_stdio(mut self, enabled: bool) -> Self {
        self.forward_stdio = enabled;
        self
    }

    pub fn pass_fd(mut self, fd: RawFd) -> Self {
        self.pass_fds.push(fd);
        self
    }

    pub fn handled_signal(mut self, sig: i32) -> Self {
        self.handled_signals.push(sig);
        self
    }

    pub fn initial_window_size(mut self, size: WindowSize) -> Self {
        self.initial_window_size = Some(size);
        self
    }
}

/// A forked child that owns its pty master.
#[derive(Debug)]
pub struct ChildProcess {
    pid: Pid,
    master: OwnedFd,
    stdin_write: Option<OwnedFd>,
    ready_write: Option<OwnedFd>,
    exec_reader: Option<OwnedFd>,
    exec_buffer: Vec<u8>,
    exec_state: Option<ExecStatus>,
    exit_status: Option<ExitStatus>,
}

fn cstring(value: &OsStr, what: &str) -> io::Result<CString> {
    CString::new(value.as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{what} contains an interior NUL byte"),
        )
    })
}

fn make_pipe() -> io::Result<(OwnedFd, OwnedFd)> {
    #[cfg(target_os = "linux")]
    {
        use crate::sys;
        let mut fds = [0i32; 2];
        // SAFETY: fds is valid for two descriptors.
        let ret = unsafe { sys::pipe2(fds.as_mut_ptr(), sys::O_CLOEXEC) };
        if ret != 0 {
            return Err(sys::last_os_error());
        }
        // SAFETY: both descriptors are fresh and owned.
        Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err(crate::unsupported("make_pipe"))
    }
}

/// Alias matching the C/Python naming; `Child::spawn(&config)` is available.
pub type Child = ChildProcess;

impl ChildProcess {
    /// Spawn a child on a fresh PTY. Equivalent to the free [`spawn`] function.
    pub fn spawn(config: &SpawnConfig) -> io::Result<Self> {
        spawn(config)
    }

    pub fn pid(&self) -> Pid {
        self.pid
    }

    pub fn master_fd(&self) -> RawFd {
        self.master.as_raw_fd()
    }

    pub fn master(&self) -> &OwnedFd {
        &self.master
    }

    /// Take the stdin pipe writer, if `stdin_pipe` was requested.
    pub fn take_stdin_write(&mut self) -> Option<OwnedFd> {
        self.stdin_write.take()
    }

    pub fn stdin_write_fd(&self) -> Option<RawFd> {
        self.stdin_write.as_ref().map(AsRawFd::as_raw_fd)
    }

    /// Write once to the stdin pipe. Returns the number of bytes written.
    #[cfg(target_os = "linux")]
    pub fn write_stdin(&mut self, data: &[u8]) -> io::Result<usize> {
        use crate::sys;
        use std::ffi::c_void;
        let Some(fd) = self.stdin_write.as_ref() else {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "no stdin pipe was requested",
            ));
        };
        loop {
            // SAFETY: data is a valid buffer for its length.
            let ret =
                unsafe { sys::write(fd.as_raw_fd(), data.as_ptr() as *const c_void, data.len()) };
            if ret >= 0 {
                return Ok(ret as usize);
            }
            let err = sys::errno();
            if err == sys::EINTR {
                continue;
            }
            return Err(io::Error::from_raw_os_error(err));
        }
    }

    #[cfg(not(target_os = "linux"))]
    pub fn write_stdin(&mut self, _data: &[u8]) -> io::Result<usize> {
        Err(crate::unsupported("write_stdin"))
    }

    /// Release the child from the `ready_pipe` wait. The write end is closed
    /// afterwards, matching `Child.mark_terminal_ready()`.
    #[cfg(target_os = "linux")]
    pub fn mark_terminal_ready(&mut self) -> io::Result<()> {
        use crate::sys;
        use std::ffi::c_void;
        let Some(fd) = self.ready_write.take() else {
            return Ok(());
        };
        let byte = 1u8;
        loop {
            // SAFETY: byte is a valid one-byte buffer.
            let ret = unsafe { sys::write(fd.as_raw_fd(), &byte as *const u8 as *const c_void, 1) };
            if ret >= 0 {
                // Dropping fd closes the pipe.
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
    pub fn mark_terminal_ready(&mut self) -> io::Result<()> {
        Err(crate::unsupported("mark_terminal_ready"))
    }

    /// Non-blocking check of the exec-error pipe.
    #[cfg(target_os = "linux")]
    pub fn exec_status_nonblocking(&mut self) -> io::Result<ExecStatus> {
        use crate::sys;
        use std::ffi::c_void;
        if let Some(state) = self.exec_state {
            return Ok(state);
        }
        let Some(reader) = self.exec_reader.as_ref() else {
            self.exec_state = Some(ExecStatus::Succeeded);
            return Ok(ExecStatus::Succeeded);
        };
        let mut buf = [0u8; 8];
        loop {
            let space = 8 - self.exec_buffer.len();
            if space > 0 {
                // SAFETY: the destination is within buf.
                let ret = unsafe {
                    sys::read(reader.as_raw_fd(), buf.as_mut_ptr() as *mut c_void, space)
                };
                if ret > 0 {
                    self.exec_buffer.extend_from_slice(&buf[..ret as usize]);
                } else if ret == 0 {
                    self.exec_reader = None;
                    let state = if self.exec_buffer.len() >= 8 {
                        parse_exec_failure(&self.exec_buffer)?
                    } else {
                        ExecStatus::Succeeded
                    };
                    self.exec_state = Some(state);
                    return Ok(state);
                } else {
                    let err = sys::errno();
                    if err == sys::EINTR {
                        continue;
                    }
                    if err == sys::EAGAIN || err == sys::EWOULDBLOCK {
                        return Ok(ExecStatus::Pending);
                    }
                    return Err(io::Error::from_raw_os_error(err));
                }
            }
            if self.exec_buffer.len() >= 8 {
                let state = parse_exec_failure(&self.exec_buffer)?;
                self.exec_state = Some(state);
                self.exec_reader = None;
                return Ok(state);
            }
        }
    }

    #[cfg(not(target_os = "linux"))]
    pub fn exec_status_nonblocking(&mut self) -> io::Result<ExecStatus> {
        Err(crate::unsupported("exec_status_nonblocking"))
    }

    /// Wait (bounded when `timeout` is `Some`) for `exec` to succeed or fail.
    #[cfg(target_os = "linux")]
    pub fn wait_for_exec(&mut self, timeout: Option<Duration>) -> io::Result<ExecStatus> {
        let deadline = timeout.map(|t| monotonic().saturating_add(duration_ns(t)));
        loop {
            let state = self.exec_status_nonblocking()?;
            if state != ExecStatus::Pending {
                return Ok(state);
            }
            let wait_ms = match deadline {
                Some(deadline) => {
                    let remaining = deadline - monotonic();
                    if remaining <= 0 {
                        return Ok(ExecStatus::Pending);
                    }
                    ceil_ms(remaining).min(i32::MAX as i64) as i32
                }
                None => -1,
            };
            if let Some(reader) = self.exec_reader.as_ref() {
                wait_fd(reader.as_raw_fd(), wait_ms)?;
            } else {
                return Ok(ExecStatus::Pending);
            }
        }
    }

    #[cfg(not(target_os = "linux"))]
    pub fn wait_for_exec(&mut self, _timeout: Option<Duration>) -> io::Result<ExecStatus> {
        Err(crate::unsupported("wait_for_exec"))
    }

    /// Try to reap the child without blocking.
    #[cfg(target_os = "linux")]
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        use crate::sys;
        if let Some(status) = self.exit_status {
            return Ok(Some(status));
        }
        let mut status = 0i32;
        loop {
            // SAFETY: status points to valid storage.
            let ret = unsafe { sys::waitpid(self.pid, &mut status, sys::WNOHANG) };
            if ret == self.pid {
                let decoded = decode_wait_status(status);
                self.exit_status = Some(decoded);
                return Ok(Some(decoded));
            }
            if ret == 0 {
                return Ok(None);
            }
            let err = sys::errno();
            if err == sys::EINTR {
                continue;
            }
            if err == sys::ECHILD {
                // Someone else reaped the child; report an unknown exit rather
                // than pretending it is still running forever.
                let status = ExitStatus::Unknown(-1);
                self.exit_status = Some(status);
                return Ok(Some(status));
            }
            return Err(io::Error::from_raw_os_error(err));
        }
    }

    #[cfg(not(target_os = "linux"))]
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        Err(crate::unsupported("try_wait"))
    }

    /// Wait for the child to exit. With `None` this blocks indefinitely
    /// (`waitpid` without `WNOHANG`); with `Some(d)` it returns
    /// `io::ErrorKind::TimedOut` after `d`.
    #[cfg(target_os = "linux")]
    pub fn wait(&mut self, timeout: Option<Duration>) -> io::Result<ExitStatus> {
        use crate::sys;
        if let Some(status) = self.exit_status {
            return Ok(status);
        }
        let deadline = timeout.map(|t| monotonic().saturating_add(duration_ns(t)));
        let mut status = 0i32;
        loop {
            let options = if timeout.is_some() { sys::WNOHANG } else { 0 };
            // SAFETY: status points to valid storage.
            let ret = unsafe { sys::waitpid(self.pid, &mut status, options) };
            if ret == self.pid {
                let decoded = decode_wait_status(status);
                self.exit_status = Some(decoded);
                return Ok(decoded);
            }
            if ret == -1 {
                let err = sys::errno();
                if err == sys::EINTR {
                    continue;
                }
                if err == sys::ECHILD {
                    return Err(io::Error::from_raw_os_error(err));
                }
                return Err(io::Error::from_raw_os_error(err));
            }
            // ret == 0: still running.
            match deadline {
                Some(deadline) => {
                    let remaining = deadline - monotonic();
                    if remaining <= 0 {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "timed out waiting for child",
                        ));
                    }
                    sleep_ns(remaining.min(MONOTONIC_T_1E6) as u64);
                }
                None => {
                    // This branch is unreachable for blocking waitpid, but a
                    // spurious 0 must not spin.
                    sleep_ns(1_000_000);
                }
            }
        }
    }

    #[cfg(not(target_os = "linux"))]
    pub fn wait(&mut self, _timeout: Option<Duration>) -> io::Result<ExitStatus> {
        Err(crate::unsupported("wait"))
    }

    /// `kill(pid, sig)`.
    #[cfg(target_os = "linux")]
    pub fn signal(&self, sig: i32) -> io::Result<()> {
        use crate::sys;
        // SAFETY: kill only takes integer arguments.
        sys::cvt(unsafe { sys::kill(self.pid, sig) })?;
        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    pub fn signal(&self, _sig: i32) -> io::Result<()> {
        Err(crate::unsupported("signal"))
    }

    pub fn kill(&self, sig: i32) -> io::Result<()> {
        self.signal(sig)
    }

    /// `killpg(getpgid(pid), sig)`, like `hangup()`/`sig_queue()` in C.
    #[cfg(target_os = "linux")]
    pub fn signal_group(&self, sig: i32) -> io::Result<()> {
        use crate::sys;
        // SAFETY: getpgid/killpg only take integer arguments.
        let pgid = unsafe { sys::getpgid(self.pid) };
        if pgid < 0 {
            let err = sys::errno();
            if err == sys::ESRCH {
                return Ok(());
            }
            return Err(io::Error::from_raw_os_error(err));
        }
        let ret = unsafe { sys::killpg(pgid, sig) };
        if ret != 0 {
            let err = sys::errno();
            if err == sys::ESRCH {
                return Ok(());
            }
            return Err(io::Error::from_raw_os_error(err));
        }
        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    pub fn signal_group(&self, _sig: i32) -> io::Result<()> {
        Err(crate::unsupported("signal_group"))
    }

    /// The `hangup()` helper: SIGHUP the child's process group, ignoring ESRCH.
    pub fn hangup(&self) -> io::Result<()> {
        #[cfg(target_os = "linux")]
        {
            use crate::sys;
            self.signal_group(sys::SIGHUP)
        }
        #[cfg(not(target_os = "linux"))]
        {
            Err(crate::unsupported("hangup"))
        }
    }

    pub fn resize(&self, size: WindowSize) -> io::Result<()> {
        pty::resize_pty(self.master_fd(), size)
    }

    pub fn window_size(&self) -> io::Result<WindowSize> {
        pty::window_size(self.master_fd())
    }

    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        pty::set_nonblocking(self.master_fd(), nonblocking)
    }
}

fn parse_exec_failure(buf: &[u8]) -> io::Result<ExecStatus> {
    debug_assert!(buf.len() >= 8);
    let stage = u32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]]);
    let errno = i32::from_ne_bytes([buf[4], buf[5], buf[6], buf[7]]);
    Ok(ExecStatus::Failed(ExecFailure {
        stage: ExecStage::from_raw(stage),
        errno,
    }))
}

fn duration_ns(duration: Duration) -> Monotonic {
    duration.as_nanos().min(Monotonic::MAX as u128) as Monotonic
}

fn ceil_ms(ns: Monotonic) -> i64 {
    (ns + MONOTONIC_T_1E6 - 1) / MONOTONIC_T_1E6
}

fn sleep_ns(ns: u64) {
    #[cfg(target_os = "linux")]
    {
        use crate::sys;
        let req = sys::Timespec {
            tv_sec: (ns / 1_000_000_000) as i64,
            tv_nsec: (ns % 1_000_000_000) as i64,
        };
        // SAFETY: req is valid; no remainder is needed.
        unsafe {
            sys::nanosleep(&req, core::ptr::null_mut());
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        std::thread::sleep(Duration::from_nanos(ns));
    }
}

#[cfg(target_os = "linux")]
fn wait_fd(fd: RawFd, timeout_ms: i32) -> io::Result<()> {
    use crate::sys;
    let mut pfd = sys::PollFd {
        fd,
        events: sys::POLLIN,
        revents: 0,
    };
    loop {
        // SAFETY: pfd is valid for one pollfd.
        let ret = unsafe { sys::poll(&mut pfd, 1, timeout_ms) };
        if ret > 0 {
            return Ok(());
        }
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

/// Spawn `config.exe` on a fresh PTY.
#[cfg(target_os = "linux")]
pub fn spawn(config: &SpawnConfig) -> io::Result<ChildProcess> {
    use crate::sys;
    use core::ffi::c_char;

    if config.argv.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "argv must contain at least argv[0]",
        ));
    }

    let exe = cstring(&config.exe, "executable")?;
    let argv: Vec<CString> = config
        .argv
        .iter()
        .map(|a| cstring(a, "argv entry"))
        .collect::<io::Result<_>>()?;
    let mut env_os = config.env.clone();
    let cwd = match &config.cwd {
        Some(path) => Some(cstring(path.as_os_str(), "cwd")?),
        None => None,
    };

    let pair = pty::open_pty()?;
    if let Some(size) = config.initial_window_size {
        pty::set_window_size(pair.slave_fd(), size)?;
    }
    let (master, slave) = pair.into_fds();

    let (stdin_read, stdin_write) = if config.stdin_pipe {
        let (r, w) = make_pipe()?;
        (Some(r), Some(w))
    } else {
        (None, None)
    };
    let (ready_read, ready_write) = if config.ready_pipe {
        let (r, w) = make_pipe()?;
        (Some(r), Some(w))
    } else {
        (None, None)
    };
    let (error_read, error_write) = make_pipe()?;
    pty::set_nonblocking(error_read.as_raw_fd(), true)?;

    // Forwarded stdio descriptors. See the module-level deviation note.
    let mut forward_fds: Vec<OwnedFd> = Vec::new();
    if config.forward_stdio {
        // SAFETY: dup only takes an integer fd.
        let out = sys::cvt(unsafe { sys::dup(1) })?;
        // SAFETY: out is a fresh descriptor.
        forward_fds.push(unsafe { OwnedFd::from_raw_fd(out) });
        // SAFETY: dup only takes an integer fd.
        let err = sys::cvt(unsafe { sys::dup(2) })?;
        // SAFETY: err is a fresh descriptor.
        forward_fds.push(unsafe { OwnedFd::from_raw_fd(err) });
        env_os.push(OsString::from(format!("KITTY_STDIO_FORWARDED={out}")));
    }

    let env: Vec<CString> = env_os
        .iter()
        .map(|e| cstring(e, "environment entry"))
        .collect::<io::Result<_>>()?;

    let mut preserved = [0u64; 4];
    for fd in config
        .pass_fds
        .iter()
        .copied()
        .chain(forward_fds.iter().map(|f| f.as_raw_fd()))
        .chain(stdin_read.iter().map(AsRawFd::as_raw_fd))
        .chain(ready_read.iter().map(AsRawFd::as_raw_fd))
        .chain(core::iter::once(error_write.as_raw_fd()))
    {
        preserve_fd(&mut preserved, fd);
    }

    let mut argv_ptrs: Vec<*const c_char> = argv.iter().map(|a| a.as_ptr()).collect();
    argv_ptrs.push(core::ptr::null());
    let mut env_ptrs: Vec<*const c_char> = env.iter().map(|e| e.as_ptr()).collect();
    env_ptrs.push(core::ptr::null());

    // ptsname() must be resolved before fork: allocating in the child is not
    // async-signal-safe. This is the same path child.c opens in the child.
    let mut name_buf = [0i8; 256];
    // SAFETY: name_buf is valid for the whole length.
    let ret = unsafe { sys::ptsname_r(master.as_raw_fd(), name_buf.as_mut_ptr(), name_buf.len()) };
    if ret != 0 {
        return Err(io::Error::from_raw_os_error(ret));
    }

    let slave_raw = slave.as_raw_fd();
    let master_raw = master.as_raw_fd();
    let stdin_read_raw = stdin_read.as_ref().map_or(-1, AsRawFd::as_raw_fd);
    let stdin_write_raw = stdin_write.as_ref().map_or(-1, AsRawFd::as_raw_fd);
    let ready_read_raw = ready_read.as_ref().map_or(-1, AsRawFd::as_raw_fd);
    let ready_write_raw = ready_write.as_ref().map_or(-1, AsRawFd::as_raw_fd);
    let error_write_raw = error_write.as_raw_fd();
    let handled_signals = config.handled_signals.clone();

    // SAFETY: fork() has no preconditions. The child branch below only calls
    // async-signal-safe libc functions and never allocates or unwinds.
    let pid = unsafe { sys::fork() };
    if pid < 0 {
        return Err(sys::last_os_error());
    }
    if pid == 0 {
        // SAFETY: all pointers point to buffers that live in the (copied)
        // address space until exec/_exit.
        unsafe {
            child_exec(
                name_buf.as_ptr(),
                master_raw,
                slave_raw,
                stdin_read_raw,
                stdin_write_raw,
                ready_read_raw,
                ready_write_raw,
                error_write_raw,
                &handled_signals,
                &preserved,
                cwd.as_ref().map_or(core::ptr::null(), |c| c.as_ptr()),
                exe.as_ptr(),
                argv_ptrs.as_ptr(),
                env_ptrs.as_ptr(),
            )
        }
    }

    // Parent. Close everything that belongs only to the child.
    drop(slave);
    drop(stdin_read);
    drop(ready_read);
    drop(error_write);
    drop(forward_fds);
    // error_read is kept in the ChildProcess.
    let _ = (master_raw, stdin_write_raw, ready_write_raw);

    Ok(ChildProcess {
        pid,
        master,
        stdin_write,
        ready_write,
        exec_reader: Some(error_read),
        exec_buffer: Vec::new(),
        exec_state: None,
        exit_status: None,
    })
}

#[cfg(not(target_os = "linux"))]
pub fn spawn(_config: &SpawnConfig) -> io::Result<ChildProcess> {
    Err(crate::unsupported("spawn"))
}

/// Reap an arbitrary child pid that is no longer owned by a [`ChildProcess`]
/// (for example one whose `ChildMonitor` was dropped). Bounded when `timeout`
/// is `Some`.
#[cfg(target_os = "linux")]
pub fn reap_pid(pid: Pid, timeout: Option<Duration>) -> io::Result<ExitStatus> {
    use crate::sys;
    let deadline = timeout.map(|t| monotonic().saturating_add(duration_ns(t)));
    let mut status = 0i32;
    loop {
        let options = if timeout.is_some() { sys::WNOHANG } else { 0 };
        // SAFETY: status points to valid storage.
        let ret = unsafe { sys::waitpid(pid, &mut status, options) };
        if ret == pid {
            return Ok(decode_wait_status(status));
        }
        if ret == -1 {
            let err = sys::errno();
            if err == sys::EINTR {
                continue;
            }
            return Err(io::Error::from_raw_os_error(err));
        }
        match deadline {
            Some(deadline) => {
                let remaining = deadline - monotonic();
                if remaining <= 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "timed out waiting for child",
                    ));
                }
                sleep_ns(remaining.min(MONOTONIC_T_1E6) as u64);
            }
            None => sleep_ns(1_000_000),
        }
    }
}

#[cfg(not(target_os = "linux"))]
pub fn reap_pid(_pid: Pid, _timeout: Option<Duration>) -> io::Result<ExitStatus> {
    Err(crate::unsupported("reap_pid"))
}

fn preserve_fd(bits: &mut [u64; 4], fd: RawFd) {
    if (0..256).contains(&fd) {
        let fd = fd as usize;
        bits[fd / 64] |= 1u64 << (fd % 64);
    }
}

fn is_preserved(bits: &[u64; 4], fd: RawFd) -> bool {
    if (0..256).contains(&fd) {
        let fd = fd as usize;
        bits[fd / 64] & (1u64 << (fd % 64)) != 0
    } else {
        false
    }
}

#[cfg(target_os = "linux")]
unsafe fn raw_write_all(fd: RawFd, mut buf: *const u8, mut len: usize) {
    use crate::sys;
    use core::ffi::c_void;
    while len > 0 {
        // SAFETY: the caller guarantees buf/len are valid.
        let ret = sys::write(fd, buf as *const c_void, len);
        if ret > 0 {
            let written = ret as usize;
            buf = buf.add(written);
            len -= written;
            continue;
        }
        if ret == 0 {
            return;
        }
        if sys::errno() == sys::EINTR {
            continue;
        }
        return;
    }
}

/// Child side. Never returns.
///
/// # Safety
///
/// Must only be called in the forked child, with all pointers valid in the
/// copied address space. Only async-signal-safe operations are performed.
#[cfg(target_os = "linux")]
#[allow(clippy::too_many_arguments)]
unsafe fn child_exec(
    tty_name: *const i8,
    master: RawFd,
    slave: RawFd,
    stdin_read: RawFd,
    stdin_write: RawFd,
    ready_read: RawFd,
    ready_write: RawFd,
    error_write: RawFd,
    handled_signals: &[i32],
    preserved: &[u64; 4],
    cwd: *const i8,
    exe: *const i8,
    argv: *const *const i8,
    envp: *const *const i8,
) -> ! {
    use crate::sys;
    use core::ffi::c_void;

    let fail = |stage: ExecStage, err: i32| -> ! {
        let mut payload = [0u8; 8];
        payload[..4].copy_from_slice(&(stage as u32).to_ne_bytes());
        payload[4..].copy_from_slice(&err.to_ne_bytes());
        // SAFETY: raw_write_all only uses raw libc calls.
        unsafe { raw_write_all(error_write, payload.as_ptr(), payload.len()) };
        let msg = b"Failed to launch child\n";
        // SAFETY: as above.
        unsafe { raw_write_all(2, msg.as_ptr(), msg.len()) };
        // SAFETY: _exit is async-signal-safe.
        unsafe { sys::_exit(127) }
    };

    // 1. Reset signals. child.c resets the handled signals and the two
    // signals CPython normally changes before clearing the mask.
    // SAFETY: signal() is async-signal-safe.
    unsafe {
        for &sig in handled_signals {
            sys::signal(sig, sys::SIG_DFL);
        }
        sys::signal(sys::SIGPIPE, sys::SIG_DFL);
        sys::signal(sys::SIGXFSZ, sys::SIG_DFL);
    }
    let empty = sys::SigSet::empty();
    // SAFETY: empty is a valid signal set.
    if unsafe { sys::sigprocmask(sys::SIG_SETMASK, &empty, core::ptr::null_mut()) } != 0 {
        fail(ExecStage::Sigprocmask, sys::errno());
    }

    // 2. cwd, with the same fallback as child.c.
    if !cwd.is_null() && unsafe { sys::chdir(cwd) } != 0 {
        let dot = *b".\0";
        // SAFETY: dot is a valid C string.
        if unsafe { sys::access(dot.as_ptr() as *const i8, sys::X_OK) } != 0 {
            let root = *b"/\0";
            // SAFETY: root is a valid C string.
            let _ = unsafe { sys::chdir(root.as_ptr() as *const i8) };
        }
    }

    // 3. New session.
    // SAFETY: setsid has no preconditions.
    if unsafe { sys::setsid() } < 0 {
        fail(ExecStage::Setsid, sys::errno());
    }

    // 4. Controlling terminal.
    // SAFETY: tty_name is a valid NUL-terminated path.
    let tfd = unsafe { sys::open(tty_name, sys::O_RDWR | sys::O_CLOEXEC, 0) };
    if tfd < 0 {
        fail(ExecStage::OpenTerminal, sys::errno());
    }
    // SAFETY: tfd is a valid descriptor.
    if unsafe { sys::ioctl(tfd, sys::TIOCSCTTY, 0) } < 0 {
        fail(ExecStage::SetControllingTerminal, sys::errno());
    }
    // SAFETY: closing our own descriptor.
    unsafe { sys::close(tfd) };

    // 5. stdio.
    if unsafe { sys::dup2(slave, 1) } < 0 {
        fail(ExecStage::DupStdout, sys::errno());
    }
    if unsafe { sys::dup2(slave, 2) } < 0 {
        fail(ExecStage::DupStderr, sys::errno());
    }
    if stdin_read >= 0 {
        if unsafe { sys::dup2(stdin_read, 0) } < 0 {
            fail(ExecStage::DupStdin, sys::errno());
        }
        // SAFETY: closing descriptors we own.
        unsafe {
            sys::close(stdin_read);
            if stdin_write >= 0 {
                sys::close(stdin_write);
            }
        }
    } else if unsafe { sys::dup2(slave, 0) } < 0 {
        fail(ExecStage::DupStdin, sys::errno());
    }

    // 6. Close the parent's copies of the pipes and the pty master.
    // SAFETY: closing descriptors we own.
    unsafe {
        sys::close(slave);
        sys::close(master);
        if ready_write >= 0 {
            sys::close(ready_write);
        }
        if stdin_write >= 0 && stdin_read < 0 {
            sys::close(stdin_write);
        }
    }

    // 7. Wait for the ready signal from the parent.
    if ready_read >= 0 {
        let mut byte = 0u8;
        loop {
            // SAFETY: byte is a valid one-byte buffer.
            let ret = unsafe { sys::read(ready_read, &mut byte as *mut u8 as *mut c_void, 1) };
            if ret == 0 {
                break;
            }
            if ret < 0 {
                let err = sys::errno();
                if err == sys::EINTR || err == sys::EAGAIN {
                    continue;
                }
                break;
            }
            break;
        }
        // SAFETY: closing our own descriptor.
        unsafe { sys::close(ready_read) };
    }

    // 8. Close every unrelated descriptor, exactly 3..256.
    for fd in 3..256 {
        if !is_preserved(preserved, fd) {
            // SAFETY: close on an arbitrary descriptor is harmless here; errors
            // for non-open fds are ignored just like safe_close().
            unsafe { sys::close(fd) };
        }
    }

    // 9. Replace the environment and exec.
    // SAFETY: envp is a valid NULL-terminated array.
    unsafe {
        sys::ENVIRON = envp as *mut *mut i8;
        sys::execvp(exe, argv);
    }
    fail(ExecStage::Exec, sys::errno());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wait_status_decoding_covers_all_cases() {
        let exited = 7 << 8;
        assert_eq!(decode_wait_status(exited), ExitStatus::Exited(7));
        assert_eq!(decode_wait_status(9), ExitStatus::Signaled(9));
        assert_eq!(decode_wait_status(0x7f | (2 << 8)), ExitStatus::Stopped(2));
        assert_eq!(decode_wait_status(0xffff), ExitStatus::Continued);
        assert!(ExitStatus::Exited(0).success());
        assert!(!ExitStatus::Exited(1).success());
        assert_eq!(ExitStatus::Signaled(15).signal(), Some(15));
        assert_eq!(ExitStatus::Exited(3).code(), Some(3));
    }

    #[test]
    fn exec_stage_round_trips() {
        for stage in [
            ExecStage::None,
            ExecStage::Sigprocmask,
            ExecStage::Setsid,
            ExecStage::OpenTerminal,
            ExecStage::SetControllingTerminal,
            ExecStage::DupStdout,
            ExecStage::DupStderr,
            ExecStage::DupStdin,
            ExecStage::Exec,
        ] {
            assert_eq!(ExecStage::from_raw(stage as u32), stage);
        }
        assert_eq!(
            parse_exec_failure(&[8, 0, 0, 0, 2, 0, 0, 0]).unwrap(),
            ExecStatus::Failed(ExecFailure {
                stage: ExecStage::Exec,
                errno: 2
            })
        );
    }
}
