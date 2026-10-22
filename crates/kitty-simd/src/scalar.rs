// SPDX-License-Identifier: GPL-3.0-or-later
//! Scalar reference implementations.
//!
//! These are line by line ports of the `*_scalar` functions in kitty's
//! `simd-string.c`. Every accelerated path must produce bit identical results
//! to the functions in this module.

/// Return the index of the first byte equal to `a` or `b`.
pub fn find_either_of_two_bytes(haystack: &[u8], a: u8, b: u8) -> Option<usize> {
    haystack.iter().position(|&ch| ch == a || ch == b)
}

/// XOR `data` with the 64 byte key, repeating the key.
pub fn xor_data64(key: &[u8; 64], data: &mut [u8]) {
    for (i, byte) in data.iter_mut().enumerate() {
        *byte ^= key[i & 63];
    }
}

/// Length of the prefix of `chars` containing only printable ASCII
/// codepoints (32 <= ch <= 126).
pub fn printable_ascii_run_length(chars: &[u32]) -> usize {
    let mut n = 0;
    while n < chars.len() && chars[n].wrapping_sub(32) < 95 {
        n += 1;
    }
    n
}

/// Rounding division by 255, exact for `x <= 65407`.
#[inline]
pub fn div255_round(x: u32) -> u32 {
    let y = x + 128;
    (y + (y >> 8)) >> 8
}

#[inline]
pub fn blend_pixel_over_opaque(dst: &mut [u8], src: &[u8], dst_bpp: usize) {
    let alpha = src[3] as u32;
    let inv_alpha = 255 - alpha;
    for c in 0..3 {
        dst[c] = div255_round(src[c] as u32 * alpha + dst[c] as u32 * inv_alpha) as u8;
    }
    if dst_bpp == 4 {
        dst[3] = 255;
    }
}

/// Straight alpha over blend of a single RGBA pixel.
///
/// The C code computes `(float)num / (float)denom` and rounds with `lrintf`,
/// which under the default `FE_TONEAREST` rounding mode rounds halfway cases to
/// even. Rust's [`f32::round_ties_even`] has the same semantics, so this is
/// bit identical to the C scalar implementation and to the SIMD paths (which
/// use `cvtps_epi32`).
#[inline]
pub fn blend_pixel_over_straight(dst: &mut [u8], src: &[u8]) {
    let alpha = src[3] as u32;
    let dst_alpha = dst[3] as u32;
    let inv_alpha = 255 - alpha;
    let denom = alpha * 255 + dst_alpha * inv_alpha;
    if denom == 0 {
        return;
    }
    for c in 0..3 {
        let num = src[c] as u32 * alpha * 255 + dst[c] as u32 * dst_alpha * inv_alpha;
        dst[c] = ((num as f32) / (denom as f32)).round_ties_even() as u8;
    }
    dst[3] = div255_round(denom) as u8;
}

fn assert_blend_straight_lengths(dst: &[u8], src: &[u8]) {
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

/// Composite `src` over `dst` in place using straight (non-premultiplied)
/// alpha. Both slices contain RGBA pixels.
///
/// # Panics
///
/// Panics if the slice lengths are not equal or are not a multiple of four.
pub fn blend_over_straight(dst: &mut [u8], src: &[u8]) {
    assert_blend_straight_lengths(dst, src);
    for i in 0..src.len() / 4 {
        blend_pixel_over_straight(&mut dst[4 * i..4 * i + 4], &src[4 * i..4 * i + 4]);
    }
}

fn assert_blend_opaque_lengths(dst: &[u8], dst_bpp: u32, src: &[u8]) {
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

/// Composite RGBA `src` over an opaque destination of `dst_bpp` bytes per
/// pixel (3 = RGB, 4 = RGBA with alpha set to 255).
///
/// # Panics
///
/// Panics if `dst_bpp` is not 3 or 4, if `src.len()` is not a multiple of four,
/// or if `dst.len() != (src.len() / 4) * dst_bpp`.
pub fn blend_over_opaque(dst: &mut [u8], dst_bpp: u32, src: &[u8]) {
    assert_blend_opaque_lengths(dst, dst_bpp, src);
    let bpp = dst_bpp as usize;
    for i in 0..src.len() / 4 {
        blend_pixel_over_opaque(
            &mut dst[bpp * i..bpp * i + bpp],
            &src[4 * i..4 * i + 4],
            bpp,
        );
    }
}

/// Composite an 8-bit alpha mask onto 32-bit pixels.
///
/// `dst[i] = ((color_rgb << 8) & 0xffffff00) | max(mask[i], dst[i] & 0xff)`.
///
/// # Panics
///
/// Panics if `dst.len() != mask.len()`.
pub fn composite_alpha_mask(dst: &mut [u32], mask: &[u8], color_rgb: u32) {
    assert_eq!(
        dst.len(),
        mask.len(),
        "dst must contain one 32-bit pixel per mask byte"
    );
    let col = color_rgb.wrapping_shl(8) & 0xffff_ff00;
    for (d, &m) in dst.iter_mut().zip(mask) {
        *d = col | (*d & 0xff).max(m as u32);
    }
}
