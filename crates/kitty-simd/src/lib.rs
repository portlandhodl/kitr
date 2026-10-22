// SPDX-License-Identifier: GPL-3.0-or-later
//! SIMD and string primitives ported 1:1 from kitty's `simd-string.[ch]`,
//! `simd-string-impl.h` and `simd-string-{128,256,512}.c`.
//!
//! The public free functions use runtime CPU feature detection. For
//! differential testing, the [`scalar`] module and the per-width modules under
//! [`x86`] expose the individual implementations, and [`with_forced_level`]
//! temporarily forces the dispatch level of the current thread.
//!
//! # Panics
//!
//! Slice lengths are validated where the C API relies on the caller. See each
//! function's documentation. Out of range memory is never accessed.

use std::cell::Cell;
use std::sync::OnceLock;

pub mod scalar;
pub mod utf8;

#[cfg(target_arch = "x86_64")]
pub mod x86;

pub use utf8::UTF8Decoder;
pub use utf8::{decode_to_esc_scalar, UTF8_ACCEPT, UTF8_REJECT};

/// Level used when the caller has forced one for the current thread.
const LEVEL_UNSET: u32 = u32::MAX;

thread_local! {
    static FORCED_LEVEL: Cell<u32> = const { Cell::new(LEVEL_UNSET) };
}

fn detected_level() -> u32 {
    static DETECTED: OnceLock<u32> = OnceLock::new();
    *DETECTED.get_or_init(|| {
        #[cfg(target_arch = "x86_64")]
        {
            if std::is_x86_feature_detected!("avx512f")
                && std::is_x86_feature_detected!("avx512bw")
                && std::is_x86_feature_detected!("avx512vl")
                && std::is_x86_feature_detected!("avx512vbmi2")
            {
                return 512;
            }
            if std::is_x86_feature_detected!("avx2") {
                return 256;
            }
            // The 128-bit port uses SSE4.1 and SSSE3 instructions; like the C
            // code we gate it on SSE4.2, which implies both.
            if std::is_x86_feature_detected!("sse4.2") {
                return 128;
            }
            0
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            0
        }
    })
}

fn env_level() -> Option<u32> {
    static ENV: OnceLock<Option<u32>> = OnceLock::new();
    *ENV.get_or_init(|| match std::env::var("KITTY_SIMD").ok()?.as_str() {
        "128" => Some(128),
        "256" => Some(256),
        "512" => Some(512),
        // Matching init_simd(): any other value disables all SIMD levels.
        _ => Some(0),
    })
}

fn clamp_level(level: u32) -> u32 {
    let detected = detected_level();
    match level {
        0 => 0,
        128 | 256 | 512 => level.min(detected),
        _ => detected,
    }
}

/// The SIMD level currently used by the public entry points: 512, 256, 128 or
/// 0 (scalar). Honors `KITTY_SIMD` and [`with_forced_level`].
pub fn simd_level() -> u32 {
    let forced = FORCED_LEVEL.with(Cell::get);
    if forced != LEVEL_UNSET {
        return clamp_level(forced);
    }
    if let Some(level) = env_level() {
        return clamp_level(level);
    }
    detected_level()
}

/// Highest SIMD level implemented and usable on this CPU.
pub fn max_simd_level() -> u32 {
    detected_level()
}

/// Whether a specific implementation level can be executed on this CPU.
pub fn level_supported(level: u32) -> bool {
    matches!(level, 0 | 128 | 256 | 512) && level <= detected_level()
}

/// Run `f` with this thread's dispatch level forced to `level`.
///
/// Levels that the CPU does not support are clamped down to a supported level,
/// so no illegal instruction can be executed. The previous value is restored
/// even if `f` panics.
///
/// # Panics
///
/// Panics if `level` is not one of 0, 128, 256 or 512.
pub fn with_forced_level<R>(level: u32, f: impl FnOnce() -> R) -> R {
    assert!(
        matches!(level, 0 | 128 | 256 | 512),
        "level must be 0, 128, 256 or 512, got {level}"
    );
    struct Guard(u32);
    impl Drop for Guard {
        fn drop(&mut self) {
            FORCED_LEVEL.with(|cell| cell.set(self.0));
        }
    }
    let previous = FORCED_LEVEL.with(|cell| cell.replace(level));
    let _guard = Guard(previous);
    f()
}

/// Return the index of the first byte equal to `a` or `b`.
pub fn find_either_of_two_bytes(haystack: &[u8], a: u8, b: u8) -> Option<usize> {
    #[cfg(target_arch = "x86_64")]
    match simd_level() {
        512 => return unsafe { x86::avx512::find_either_of_two_bytes(haystack, a, b) },
        256 => return unsafe { x86::avx2::find_either_of_two_bytes(haystack, a, b) },
        128 => return unsafe { x86::sse2::find_either_of_two_bytes(haystack, a, b) },
        _ => {}
    }
    scalar::find_either_of_two_bytes(haystack, a, b)
}

/// XOR `data` with the 64 byte key, repeating the key.
pub fn xor_data64(key: &[u8; 64], data: &mut [u8]) {
    #[cfg(target_arch = "x86_64")]
    match simd_level() {
        512 => return unsafe { x86::avx512::xor_data64(key, data) },
        256 => return unsafe { x86::avx2::xor_data64(key, data) },
        128 => return unsafe { x86::sse2::xor_data64(key, data) },
        _ => {}
    }
    scalar::xor_data64(key, data)
}

/// Length of the prefix of `chars` containing only printable ASCII
/// codepoints (32 <= ch <= 126).
pub fn printable_ascii_run_length(chars: &[u32]) -> usize {
    #[cfg(target_arch = "x86_64")]
    match simd_level() {
        512 => return unsafe { x86::avx512::printable_ascii_run_length(chars) },
        256 => return unsafe { x86::avx2::printable_ascii_run_length(chars) },
        128 => return unsafe { x86::sse2::printable_ascii_run_length(chars) },
        _ => {}
    }
    scalar::printable_ascii_run_length(chars)
}

/// Composite `src` over `dst` in place using straight alpha. Both slices
/// contain RGBA pixels.
///
/// # Panics
///
/// Panics if the slices are not the same length or that length is not a
/// multiple of four.
pub fn blend_over_straight(dst: &mut [u8], src: &[u8]) {
    #[cfg(target_arch = "x86_64")]
    match simd_level() {
        512 => return unsafe { x86::avx512::blend_over_straight(dst, src) },
        256 => return unsafe { x86::avx2::blend_over_straight(dst, src) },
        128 => return unsafe { x86::sse2::blend_over_straight(dst, src) },
        _ => {}
    }
    scalar::blend_over_straight(dst, src)
}

/// Composite RGBA `src` over an opaque destination with `dst_bpp` bytes per
/// pixel (3 = RGB, 4 = RGBA).
///
/// # Panics
///
/// Panics if `dst_bpp` is not 3 or 4, if `src.len()` is not a multiple of four,
/// or if the destination length does not match.
pub fn blend_over_opaque(dst: &mut [u8], dst_bpp: u32, src: &[u8]) {
    #[cfg(target_arch = "x86_64")]
    match simd_level() {
        512 => return unsafe { x86::avx512::blend_over_opaque(dst, dst_bpp, src) },
        256 => return unsafe { x86::avx2::blend_over_opaque(dst, dst_bpp, src) },
        128 => return unsafe { x86::sse2::blend_over_opaque(dst, dst_bpp, src) },
        _ => {}
    }
    scalar::blend_over_opaque(dst, dst_bpp, src)
}

/// Composite an 8-bit alpha mask onto 32-bit pixels.
///
/// # Panics
///
/// Panics if `dst.len() != mask.len()`.
pub fn composite_alpha_mask(dst: &mut [u32], mask: &[u8], color_rgb: u32) {
    #[cfg(target_arch = "x86_64")]
    match simd_level() {
        512 => return unsafe { x86::avx512::composite_alpha_mask(dst, mask, color_rgb) },
        256 => return unsafe { x86::avx2::composite_alpha_mask(dst, mask, color_rgb) },
        128 => return unsafe { x86::sse2::composite_alpha_mask(dst, mask, color_rgb) },
        _ => {}
    }
    scalar::composite_alpha_mask(dst, mask, color_rgb)
}

/// Decode `src` until an ESC sentinel, carrying partial UTF-8 state across
/// calls. Returns `true` if the sentinel was found.
///
/// This mirrors the C entry point, including its early return after a chunk
/// with invalid UTF-8: see [`UTF8Decoder::decode_to_esc`] for the caller loop
/// and the `num_consumed` contract.
pub fn utf8_decode_to_esc(d: &mut UTF8Decoder, src: &[u8]) -> bool {
    #[cfg(target_arch = "x86_64")]
    match simd_level() {
        512 => return unsafe { x86::avx512::utf8_decode_to_esc(d, src) },
        256 => return unsafe { x86::avx2::utf8_decode_to_esc(d, src) },
        128 => return unsafe { x86::sse2::utf8_decode_to_esc(d, src) },
        _ => {}
    }
    decode_to_esc_scalar(d, src)
}

pub(crate) mod scalar_checks {
    pub(crate) fn assert_blend_straight_lengths(dst: &[u8], src: &[u8]) {
        assert!(
            src.len().is_multiple_of(4),
            "src length must be a multiple of 4, got {}",
            src.len()
        );
        assert_eq!(
            dst.len(),
            src.len(),
            "dst and src must contain the same number of RGBA bytes"
        );
    }

    pub(crate) fn assert_blend_opaque_lengths(dst: &[u8], dst_bpp: u32, src: &[u8]) {
        assert!(
            dst_bpp == 3 || dst_bpp == 4,
            "dst_bpp must be 3 or 4, got {dst_bpp}"
        );
        assert!(
            src.len().is_multiple_of(4),
            "src length must be a multiple of 4, got {}",
            src.len()
        );
        assert_eq!(
            dst.len(),
            (src.len() / 4) * dst_bpp as usize,
            "dst must contain num_pixels * dst_bpp bytes"
        );
    }
}

#[cfg(test)]
mod tests;
