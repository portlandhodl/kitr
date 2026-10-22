// SPDX-License-Identifier: GPL-3.0-or-later
//! Per-width x86_64 implementations. The public functions in the crate root
//! dispatch to these based on runtime CPU feature detection.

pub mod avx2;
pub mod avx512;
pub mod sse2;
