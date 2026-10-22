// SPDX-License-Identifier: GPL-3.0-or-later
// Ported from kitty/monotonic.c and kitty/monotonic.h,
// Copyright (C) 2019 Kovid Goyal <kovid at kovidgoyal.net>

//! Monotonic clock and the `monotonic_t` helper functions.
//!
//! A `monotonic_t` is a signed 64-bit nanosecond count, just like the C type.
//! `monotonic()` returns nanoseconds since the process called
//! [`init_monotonic`] (or since the first call if it was not called), matching
//! the C `monotonic_() - monotonic_start_time` split.

use core::sync::atomic::{AtomicI64, Ordering};

/// Nanosecond count, the Rust equivalent of `typedef int64_t monotonic_t`.
pub type Monotonic = i64;

pub const MONOTONIC_T_MAX: Monotonic = Monotonic::MAX;
pub const MONOTONIC_T_MIN: Monotonic = Monotonic::MIN;
pub const MONOTONIC_T_1E9: Monotonic = 1_000_000_000;
pub const MONOTONIC_T_1E6: Monotonic = 1_000_000;
pub const MONOTONIC_T_1E3: Monotonic = 1_000;

static START: AtomicI64 = AtomicI64::new(0);
static INITIALIZED: AtomicI64 = AtomicI64::new(0);

#[inline]
pub fn s_double_to_monotonic_t(time: f64) -> Monotonic {
    (time * 1e9) as Monotonic
}

#[inline]
pub fn ms_double_to_monotonic_t(time: f64) -> Monotonic {
    (time * 1e6) as Monotonic
}

#[inline]
pub fn s_to_monotonic_t(time: Monotonic) -> Monotonic {
    time.wrapping_mul(MONOTONIC_T_1E9)
}

#[inline]
pub fn s_to_monotonic_t_checked(time: Monotonic) -> Option<Monotonic> {
    time.checked_mul(MONOTONIC_T_1E9)
}

#[inline]
pub fn ms_to_monotonic_t(time: Monotonic) -> Monotonic {
    time.wrapping_mul(MONOTONIC_T_1E6)
}

#[inline]
pub fn ms_to_monotonic_t_checked(time: Monotonic) -> Option<Monotonic> {
    time.checked_mul(MONOTONIC_T_1E6)
}

#[inline]
pub fn monotonic_t_to_ms(time: Monotonic) -> i32 {
    (time / MONOTONIC_T_1E6) as i32
}

#[inline]
pub fn monotonic_t_to_ms_ceil(time: Monotonic) -> i32 {
    ((time + MONOTONIC_T_1E6 - 1) / MONOTONIC_T_1E6) as i32
}

#[inline]
pub fn monotonic_t_to_us(time: Monotonic) -> i32 {
    (time / MONOTONIC_T_1E3) as i32
}

#[inline]
pub fn monotonic_t_to_s_double(time: Monotonic) -> f64 {
    time as f64 / 1e9
}

/// `monotonic_()`: raw clock value, independent of the start time.
#[cfg(target_os = "linux")]
pub fn monotonic_ns() -> Monotonic {
    use crate::sys;
    let mut ts = sys::Timespec::default();
    // SAFETY: ts points to valid writable storage and the clock id is valid.
    let ret = unsafe { sys::clock_gettime(sys::CLOCK_MONOTONIC_RAW, &mut ts) };
    if ret != 0 {
        // CLOCK_MONOTONIC_RAW has been available since Linux 2.6.28. Fall back
        // rather than panicking if a seccomp filter or emulation denies it.
        // SAFETY: as above.
        let ret = unsafe { sys::clock_gettime(sys::CLOCK_MONOTONIC, &mut ts) };
        debug_assert_eq!(ret, 0, "clock_gettime(CLOCK_MONOTONIC) failed");
    }
    (ts.tv_sec as Monotonic)
        .wrapping_mul(MONOTONIC_T_1E9)
        .wrapping_add(ts.tv_nsec as Monotonic)
}

/// Raw monotonic nanoseconds on platforms without the Linux clock IDs.
#[cfg(not(target_os = "linux"))]
pub fn monotonic_ns() -> Monotonic {
    use std::sync::OnceLock;
    use std::time::Instant;
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    let epoch = EPOCH.get_or_init(Instant::now);
    epoch.elapsed().as_nanos().min(i64::MAX as u128) as Monotonic
}

/// `init_monotonic()`.
pub fn init_monotonic() {
    let now = monotonic_ns();
    START.store(now, Ordering::SeqCst);
    INITIALIZED.store(1, Ordering::SeqCst);
}

/// `monotonic()`: nanoseconds since the start time.
///
/// If [`init_monotonic`] has not been called, the start time is captured on
/// the first call. This is a convenience for Rust callers; the C code always
/// calls `init_monotonic()` during startup.
pub fn monotonic() -> Monotonic {
    if INITIALIZED.load(Ordering::Acquire) == 0 {
        init_monotonic();
    }
    monotonic_ns().wrapping_sub(START.load(Ordering::Acquire))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_conversions_match_the_c_macros() {
        assert_eq!(s_to_monotonic_t(2), 2 * MONOTONIC_T_1E9);
        assert_eq!(ms_to_monotonic_t(3), 3 * MONOTONIC_T_1E6);
        assert_eq!(s_double_to_monotonic_t(0.5), 500_000_000);
        assert_eq!(ms_double_to_monotonic_t(0.5), 500_000);
        assert_eq!(monotonic_t_to_ms(1_999_999), 1);
        assert_eq!(monotonic_t_to_ms_ceil(1_000_001), 2);
        assert_eq!(monotonic_t_to_us(1_999), 1);
        assert!((monotonic_t_to_s_double(1_500_000_000) - 1.5).abs() < f64::EPSILON);
    }

    #[test]
    fn conversion_overflow_is_checked_when_requested() {
        assert_eq!(s_to_monotonic_t_checked(1), Some(MONOTONIC_T_1E9));
        assert_eq!(ms_to_monotonic_t_checked(1), Some(MONOTONIC_T_1E6));
        assert_eq!(s_to_monotonic_t_checked(Monotonic::MAX), None);
        assert_eq!(ms_to_monotonic_t_checked(Monotonic::MAX), None);
    }

    #[test]
    fn clock_is_monotonic_and_advances() {
        let a = monotonic_ns();
        let b = monotonic_ns();
        assert!(b >= a, "{b} < {a}");
        let before = monotonic();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let after = monotonic();
        assert!(after > before, "{after} <= {before}");
        assert!(after - before < 5 * MONOTONIC_T_1E9);
    }
}
