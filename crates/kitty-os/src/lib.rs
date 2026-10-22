// SPDX-License-Identifier: GPL-3.0-or-later
//! OS integration ported from `kitty/child.c`, `kitty/child-monitor.c`,
//! `kitty/loop-utils.c` and `kitty/monotonic.c`.
//!
//! This crate is the Linux-first port of kitty's process/PTY/event-loop layer.
//! It deliberately contains no Python objects and no renderer: the monitor in
//! [`child_monitor`] is a single-threaded, functional subset of
//! `child-monitor.c`. See that module for the precise list of deferred
//! features.
//!
//! Everything is built on `std` plus raw `extern "C"` declarations in
//! [`sys`]; no third-party crate is used.

#![cfg_attr(
    not(target_os = "linux"),
    allow(dead_code, unused_imports, unused_variables)
)]

pub mod child;
pub mod child_monitor;
pub mod loop_utils;
pub mod monotonic;
pub mod pty;

#[cfg(target_os = "linux")]
mod sys;

pub use child::{
    decode_wait_status, reap_pid, spawn, Child, ChildProcess, ExecFailure, ExecStage, ExecStatus,
    ExitStatus, SpawnConfig,
};
pub use child_monitor::{
    ChildMonitor, MonitorError, MonitorOutcome, DEFAULT_READ_BUFFER_LIMIT,
    DEFAULT_WRITE_BUFFER_LIMIT,
};
pub use loop_utils::{
    poll, set_cloexec, set_nonblocking, IoOutcome, LoopData, PollFd, SelfPipe, SigInfo, SignalPipe,
    WakeupFd,
};
pub use monotonic::{
    init_monotonic, monotonic, monotonic_ns, ms_double_to_monotonic_t, ms_to_monotonic_t,
    ms_to_monotonic_t_checked, s_double_to_monotonic_t, s_to_monotonic_t, s_to_monotonic_t_checked,
    Monotonic, MONOTONIC_T_1E3, MONOTONIC_T_1E6, MONOTONIC_T_1E9, MONOTONIC_T_MAX, MONOTONIC_T_MIN,
};
pub use pty::{open_pty, PtyPair, WindowSize};

/// Error returned by every OS primitive on a platform other than Linux.
pub fn unsupported(what: &str) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        format!("{what} is only implemented on Linux in kitty-os"),
    )
}
