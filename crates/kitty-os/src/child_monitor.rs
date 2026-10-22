// SPDX-License-Identifier: GPL-3.0-or-later
// Ported from kitty/child-monitor.c, Copyright (C) 2017-2024
// Kovid Goyal <kovid at kovidgoyal.net>

//! A functional, single-threaded subset of `child-monitor.c`.
//!
//! This monitor owns one [`ChildProcess`] (pid + pty master), its wakeup
//! eventfd and a SIGCHLD self-pipe. `poll_once()` drives one iteration of the
//! I/O loop: it drains the wakeup fd, reaps SIGCHLD, reads pty output with
//! bounded buffering, and flushes queued input to the child.
//!
//! # Semantics ported from child-monitor.c
//!
//! * the poll set is `[wakeup, signal, pty]`, with `POLLIN` on the pty only
//!   while read-buffer space is available (backpressure);
//! * `POLLOUT` is armed only when there is queued input;
//! * a read of 0 or `EIO` means the child is gone; a short read stops reading
//!   for this iteration, like `read_bytes()`. On Linux C does one read per
//!   poll; this port keeps reading full 64 KiB chunks until `EAGAIN` or the
//!   buffer limit. That changes batching only, never the bytes delivered;
//! * queued writes are partially written; `EINTR` is retried, `EAGAIN` stops
//!   the flush, and any other error discards the remaining queue (as C does)
//!   and is also returned to the caller;
//! * SIGCHLD wakes the loop, `waitpid(pid, WNOHANG)` decodes the status and the
//!   exit callback runs once;
//! * on drop the child's process group is sent SIGHUP (`hangup()`).
//!
//! # Features deliberately deferred (not faked)
//!
//! * **Multi-child batching.** C supports up to `MAX_CHILDREN = 512` children
//!   behind a children mutex, add/remove queues and a scratch array. This port
//!   handles exactly one child per monitor. Use one monitor per child.
//! * **Main-loop/parser input modes.** `ParserInputWake`, `input_delay`,
//!   `small_pending`/`large_ready`, held chunks and `vt_parser_accept_input`
//!   are renderer concerns and are not implemented. Output is delivered as
//!   soon as it is read.
//! * **Graphics / render integration.** No `Screen`, graphics protocol,
//!   dirty-line updates, `wakeup_main_loop`, `maximum_wait`, timers, or OS
//!   window state.
//! * **Talk thread/remote control.** `talk_fd`, `listen_fd`, peers, UNIX socket
//!   credential checks, message framing and response callbacks are absent.
//! * **Thread pool and dedicated I/O thread.** The C monitor runs `io_loop`
//!   on a thread and a separate talk thread. This monitor is single-threaded;
//!   `wakeup()` exists so another thread can wake a blocking `poll_once()`.
//! * **`close_on_child_death`, `monitor_pid`, `sigqueue`, `thread_write`,
//!   `set_iutf8`, benchmark wakeup fd.** Not present.
//! * **macOS/BSD pty gathering.** The `PTY_GATHER_MAX` macOS optimization is
//!   intentionally not used; Linux reads already return large chunks.
//! * **`add_child` / `remove_child` queues and child IDs.** A monitor is
//!   constructed around an already-spawned child and stops when it exits.
//!
//! None of these are simulated; APIs that would need them simply are not
//! exposed.

use std::collections::VecDeque;
use std::io;
use std::time::Duration;

use crate::child::{ChildProcess, ExitStatus};
use crate::loop_utils::{
    self, poll, PollFd, SignalPipe, WakeupFd, POLLERR, POLLHUP, POLLIN, POLLNVAL, POLLOUT,
};
use crate::pty::{self, WindowSize};

/// Matches `write_buf_limit` in child-monitor.c.
pub const DEFAULT_WRITE_BUFFER_LIMIT: usize = 100 * 1024 * 1024;
/// Upper bound on un-consumed pty output retained by the monitor.
pub const DEFAULT_READ_BUFFER_LIMIT: usize = 1024 * 1024;
/// Size of a single pty read (C uses the parser's write buffer instead).
pub const DEFAULT_READ_CHUNK: usize = 64 * 1024;

/// Errors from the monitor.
#[derive(Debug)]
pub enum MonitorError {
    Io(io::Error),
    /// Queueing input would exceed the write buffer limit.
    TooMuchData,
    /// The child has already been reaped and has no monitor state left.
    ChildGone,
    Unsupported,
}

impl core::fmt::Display for MonitorError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Io(err) => write!(f, "{err}"),
            Self::TooMuchData => f.write_str("Too much data being written to child; dropping it"),
            Self::ChildGone => f.write_str("The child has already exited"),
            Self::Unsupported => f.write_str("ChildMonitor is only implemented on Linux"),
        }
    }
}

impl std::error::Error for MonitorError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<io::Error> for MonitorError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

/// What one `poll_once()` accomplished.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MonitorOutcome {
    pub woken: bool,
    pub signalled: bool,
    pub output_bytes: usize,
    pub wrote_bytes: usize,
    pub eof: bool,
    pub exited: Option<ExitStatus>,
    pub timed_out: bool,
}

type OutputCallback = Box<dyn FnMut(&[u8])>;
type ExitCallback = Box<dyn FnMut(ExitStatus)>;
type EofCallback = Box<dyn FnMut()>;

/// Single-child event-loop monitor.
#[cfg(target_os = "linux")]
pub struct ChildMonitor {
    child: ChildProcess,
    wake: WakeupFd,
    sigchld: SignalPipe,
    write_queue: VecDeque<u8>,
    pending_output: VecDeque<u8>,
    scratch: Vec<u8>,
    write_limit: usize,
    read_limit: usize,
    read_chunk: usize,
    on_output: Option<OutputCallback>,
    on_exit: Option<ExitCallback>,
    on_eof: Option<EofCallback>,
    eof: bool,
    eof_notified: bool,
    exit_status: Option<ExitStatus>,
    stopped: bool,
    hangup_on_drop: bool,
}

#[cfg(target_os = "linux")]
impl ChildMonitor {
    pub fn new(child: ChildProcess) -> io::Result<Self> {
        let wake = WakeupFd::new()?;
        let sigchld = SignalPipe::install(crate::sys::SIGCHLD)?;
        pty::set_nonblocking(child.master_fd(), true)?;
        Ok(Self {
            child,
            wake,
            sigchld,
            write_queue: VecDeque::new(),
            pending_output: VecDeque::new(),
            scratch: vec![0u8; DEFAULT_READ_CHUNK],
            write_limit: DEFAULT_WRITE_BUFFER_LIMIT,
            read_limit: DEFAULT_READ_BUFFER_LIMIT,
            read_chunk: DEFAULT_READ_CHUNK,
            on_output: None,
            on_exit: None,
            on_eof: None,
            eof: false,
            eof_notified: false,
            exit_status: None,
            stopped: false,
            hangup_on_drop: true,
        })
    }

    pub fn new_with_callbacks<O, E>(
        child: ChildProcess,
        on_output: O,
        on_exit: E,
    ) -> io::Result<Self>
    where
        O: FnMut(&[u8]) + 'static,
        E: FnMut(ExitStatus) + 'static,
    {
        let mut ans = Self::new(child)?;
        ans.on_output = Some(Box::new(on_output));
        ans.on_exit = Some(Box::new(on_exit));
        Ok(ans)
    }

    pub fn set_output_callback<F>(&mut self, callback: F)
    where
        F: FnMut(&[u8]) + 'static,
    {
        self.on_output = Some(Box::new(callback));
    }

    pub fn set_exit_callback<F>(&mut self, callback: F)
    where
        F: FnMut(ExitStatus) + 'static,
    {
        self.on_exit = Some(Box::new(callback));
    }

    pub fn set_eof_callback<F>(&mut self, callback: F)
    where
        F: FnMut() + 'static,
    {
        self.on_eof = Some(Box::new(callback));
    }

    pub fn set_write_buffer_limit(&mut self, limit: usize) {
        self.write_limit = limit;
    }

    pub fn set_read_buffer_limit(&mut self, limit: usize) {
        self.read_limit = limit.max(1);
    }

    pub fn set_read_chunk(&mut self, chunk: usize) {
        self.read_chunk = chunk.clamp(1, DEFAULT_READ_CHUNK);
        self.scratch.resize(self.read_chunk, 0);
    }

    pub fn set_hangup_on_drop(&mut self, enabled: bool) {
        self.hangup_on_drop = enabled;
    }

    pub fn pid(&self) -> i32 {
        self.child.pid()
    }

    pub fn master_fd(&self) -> i32 {
        self.child.master_fd()
    }

    pub fn child(&self) -> &ChildProcess {
        &self.child
    }

    pub fn child_mut(&mut self) -> &mut ChildProcess {
        &mut self.child
    }

    pub fn exit_status(&self) -> Option<ExitStatus> {
        self.exit_status
    }

    pub fn is_eof(&self) -> bool {
        self.eof
    }

    pub fn is_stopped(&self) -> bool {
        self.stopped
    }

    pub fn queued_write_len(&self) -> usize {
        self.write_queue.len()
    }

    pub fn pending_output_len(&self) -> usize {
        self.pending_output.len()
    }

    pub fn wakeup(&self) -> io::Result<()> {
        self.wake.wake()
    }

    /// `schedule_write_to_child*`: queue bytes for the pty, bounded by
    /// [`DEFAULT_WRITE_BUFFER_LIMIT`]. Wakes the loop.
    pub fn queue_write(&mut self, data: &[u8]) -> Result<(), MonitorError> {
        if self.write_queue.len().saturating_add(data.len()) > self.write_limit {
            return Err(MonitorError::TooMuchData);
        }
        self.write_queue.extend(data);
        self.wake.wake()?;
        Ok(())
    }

    /// `write_to_child()`: flush as much of the queue as the kernel accepts.
    pub fn flush(&mut self) -> io::Result<usize> {
        let mut total = 0;
        while !self.write_queue.is_empty() {
            let (front, _) = self.write_queue.as_slices();
            match loop_utils::write_once(self.child.master_fd(), front) {
                Ok(loop_utils::IoOutcome::Completed(0)) => break,
                Ok(loop_utils::IoOutcome::Completed(n)) => {
                    self.write_queue.drain(..n);
                    total += n;
                }
                Ok(loop_utils::IoOutcome::Interrupted) => continue,
                Ok(loop_utils::IoOutcome::WouldBlock) => break,
                Err(err) => {
                    // write_to_child() in C logs and discards the buffer.
                    self.write_queue.clear();
                    return Err(err);
                }
            }
        }
        Ok(total)
    }

    pub fn resize(&self, size: WindowSize) -> io::Result<()> {
        self.child.resize(size)
    }

    pub fn mark_terminal_ready(&mut self) -> io::Result<()> {
        self.child.mark_terminal_ready()
    }

    pub fn hangup(&self) -> io::Result<()> {
        self.child.hangup()
    }

    /// Drain output retained because no output callback is installed.
    pub fn take_output(&mut self) -> Vec<u8> {
        self.pending_output.drain(..).collect()
    }

    /// Request the loop to stop after the current iteration.
    pub fn stop(&mut self) -> io::Result<()> {
        self.stopped = true;
        self.wake.wake()
    }

    fn deliver_output(&mut self) -> usize {
        let Some(callback) = self.on_output.as_mut() else {
            return 0;
        };
        if self.pending_output.is_empty() {
            return 0;
        }
        let data: Vec<u8> = self.pending_output.drain(..).collect();
        callback(&data);
        data.len()
    }

    fn notify_eof(&mut self) {
        if self.eof && !self.eof_notified {
            self.eof_notified = true;
            if let Some(callback) = self.on_eof.as_mut() {
                callback();
            }
        }
    }

    fn note_exit(&mut self) -> Option<ExitStatus> {
        if let Some(status) = self.exit_status {
            return Some(status);
        }
        match self.child.try_wait() {
            Ok(Some(status)) => {
                self.exit_status = Some(status);
                if let Some(callback) = self.on_exit.as_mut() {
                    callback(status);
                }
                Some(status)
            }
            Ok(None) => None,
            Err(err) => {
                // ECHILD means someone else reaped it; treat as gone.
                if err.raw_os_error() == Some(crate::sys::ECHILD) {
                    return Some(ExitStatus::Unknown(-1));
                }
                None
            }
        }
    }

    /// `read_bytes()`: read until EAGAIN, EOF, EIO or the read buffer limit.
    fn read_available(&mut self, scratch: &mut [u8]) -> io::Result<usize> {
        let mut total = 0;
        loop {
            let space = self.read_limit.saturating_sub(self.pending_output.len());
            if space == 0 {
                break;
            }
            let chunk = space.min(scratch.len());
            match loop_utils::read_once(self.child.master_fd(), &mut scratch[..chunk]) {
                Ok(loop_utils::IoOutcome::Completed(0)) => {
                    self.eof = true;
                    break;
                }
                Ok(loop_utils::IoOutcome::Completed(n)) => {
                    self.pending_output.extend(&scratch[..n]);
                    total += n;
                    if n < chunk {
                        // A short read means the child has no more data queued
                        // for us. Linux pty reads are large, so no gathering.
                        break;
                    }
                }
                Ok(loop_utils::IoOutcome::Interrupted) => continue,
                Ok(loop_utils::IoOutcome::WouldBlock) => break,
                Err(err) if err.raw_os_error() == Some(crate::sys::EIO) => {
                    // C's read_bytes() treats EIO as "child is dead".
                    self.eof = true;
                    break;
                }
                Err(err) => return Err(err),
            }
        }
        Ok(total)
    }

    /// One iteration of `io_loop()` (single child). Blocks up to `timeout`.
    pub fn poll_once(&mut self, timeout: Option<Duration>) -> Result<MonitorOutcome, MonitorError> {
        if self.stopped {
            return Ok(MonitorOutcome::default());
        }
        let mut outcome = MonitorOutcome::default();

        // Reap an already-dead child without waiting for SIGCHLD. This also
        // makes the monitor robust if the process-wide SIGCHLD handler was
        // replaced by other code.
        if let Some(status) = self.note_exit() {
            outcome.exited = Some(status);
        }

        let timeout_ms = match timeout {
            Some(duration) => {
                let ns = duration.as_nanos().min(i32::MAX as u128 * 1_000_000) as i64;
                ((ns + 999_999) / 1_000_000) as i32
            }
            None => -1,
        };

        let pty_events = (if !self.eof && self.pending_output.len() < self.read_limit {
            POLLIN
        } else {
            0
        }) | (if self.write_queue.is_empty() {
            0
        } else {
            POLLOUT
        });

        let mut fds = [
            PollFd::new(self.wake.read_fd(), POLLIN),
            PollFd::new(self.sigchld.read_fd(), POLLIN),
            PollFd::new(self.child.master_fd(), pty_events),
        ];
        let ret = poll(&mut fds, timeout_ms)?;
        outcome.timed_out = ret == 0;
        if ret > 0 {
            if fds[0].has_event(POLLIN) {
                self.wake.drain()?;
                outcome.woken = true;
            }
            if fds[1].has_event(POLLIN) {
                self.sigchld.drain();
                outcome.signalled = true;
            }
            if fds[2].has_event(POLLOUT) {
                outcome.wrote_bytes += self.flush()?;
            }
            if fds[2].has_event(POLLIN | POLLHUP | POLLERR) {
                let mut scratch = std::mem::take(&mut self.scratch);
                let read = self.read_available(&mut scratch);
                self.scratch = scratch;
                outcome.output_bytes += read?;
            }
            if fds[2].has_event(POLLNVAL) {
                self.eof = true;
            }
        }

        // Deliver after all reads so a burst produces one callback.
        outcome.output_bytes = outcome.output_bytes.max(self.deliver_output());
        self.notify_eof();

        if let Some(status) = self.note_exit() {
            outcome.exited = Some(status);
        }
        // Re-read queued output after the exit reaping: a child may have
        // written its last bytes between the poll and waitpid.
        if outcome.exited.is_some() && !self.eof && !outcome.timed_out {
            let mut scratch = std::mem::take(&mut self.scratch);
            let read = self.read_available(&mut scratch);
            self.scratch = scratch;
            outcome.output_bytes += read?;
            outcome.output_bytes = outcome.output_bytes.max(self.deliver_output());
            self.notify_eof();
        }
        Ok(outcome)
    }

    /// Poll with a short internal timeout until the child exits or `timeout`
    /// elapses. Returns `TimedOut` if the child is still running.
    pub fn run_until_exit(&mut self, timeout: Duration) -> Result<ExitStatus, MonitorError> {
        let start = std::time::Instant::now();
        loop {
            if let Some(status) = self.exit_status {
                return Ok(status);
            }
            let remaining = timeout.saturating_sub(start.elapsed());
            if remaining.is_zero() {
                return Err(MonitorError::Io(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "timed out waiting for child to exit",
                )));
            }
            let slice = remaining.min(Duration::from_millis(100));
            self.poll_once(Some(slice))?;
        }
    }

    /// Poll until EOF is observed, then return all output collected by the
    /// callback or [`Self::take_output`].
    pub fn run_until_eof(&mut self, timeout: Duration) -> Result<(), MonitorError> {
        let start = std::time::Instant::now();
        loop {
            if self.eof {
                return Ok(());
            }
            let remaining = timeout.saturating_sub(start.elapsed());
            if remaining.is_zero() {
                return Err(MonitorError::Io(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "timed out waiting for pty EOF",
                )));
            }
            self.poll_once(Some(remaining.min(Duration::from_millis(100))))?;
        }
    }
}

#[cfg(target_os = "linux")]
impl Drop for ChildMonitor {
    fn drop(&mut self) {
        if self.hangup_on_drop && self.exit_status.is_none() {
            let _ = self.child.hangup();
        }
    }
}

#[cfg(not(target_os = "linux"))]
#[derive(Debug)]
pub struct ChildMonitor {
    _private: (),
}

#[cfg(not(target_os = "linux"))]
impl ChildMonitor {
    pub fn new(_child: ChildProcess) -> io::Result<Self> {
        Err(crate::unsupported("ChildMonitor::new"))
    }

    pub fn poll_once(
        &mut self,
        _timeout: Option<Duration>,
    ) -> Result<MonitorOutcome, MonitorError> {
        Err(MonitorError::Unsupported)
    }

    pub fn run_until_exit(&mut self, _timeout: Duration) -> Result<ExitStatus, MonitorError> {
        Err(MonitorError::Unsupported)
    }

    pub fn run_until_eof(&mut self, _timeout: Duration) -> Result<(), MonitorError> {
        Err(MonitorError::Unsupported)
    }

    pub fn stop(&mut self) -> io::Result<()> {
        Err(crate::unsupported("ChildMonitor::stop"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monitor_outcome_default_is_quiet() {
        let outcome = MonitorOutcome::default();
        assert!(!outcome.woken && !outcome.eof && outcome.exited.is_none());
        assert_eq!(outcome.output_bytes, 0);
    }
}
