// SPDX-License-Identifier: GPL-3.0-or-later
//! Scalar reference implementations.
//!
//! These are line by line ports of the `*_scalar` functions in kitty's
//! `simd-string.c`. Every accelerated path must produce bit identical results
//! to the functions in this module.

/// Return the index of the first byte equal to `a` or `b`.
pub fn find_either_of_two_bytes(haystack: &[u8], a: u8, b: u8) -> Option<usize> {
    const ONES: u64 = 0x0101_0101_0101_0101;
    const HIGHS: u64 = 0x8080_8080_8080_8080;
    const REPEAT: u64 = 0x0101_0101_0101_0101;
    #[inline(always)]
    fn zero_byte_mask(x: u64) -> u64 {
        // Classic SWAR has-zero-byte test: high bits set exactly at zero bytes.
        x.wrapping_sub(ONES) & !x & HIGHS
    }
    let len = haystack.len();
    let mut i = 0usize;
    if len >= 8 {
        let a_vec = (a as u64).wrapping_mul(REPEAT);
        let b_vec = (b as u64).wrapping_mul(REPEAT);
        let ptr = haystack.as_ptr();
        while i + 8 <= len {
            // SAFETY: i + 8 <= len, unaligned load is fine for u8 data.
            let word = u64::from_le_bytes(unsafe {
                core::ptr::read_unaligned(ptr.add(i) as *const [u8; 8])
            });
            let matches = zero_byte_mask(word ^ a_vec) | zero_byte_mask(word ^ b_vec);
            if matches != 0 {
                return Some(i + matches.trailing_zeros() as usize / 8);
            }
            i += 8;
        }
    }
    while i < len {
        let ch = haystack[i];
        if ch == a || ch == b {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// XOR `data` with the 64 byte key, repeating the key.
pub fn xor_data64(key: &[u8; 64], data: &mut [u8]) {
    // Semantically identical to the C scalar loop data[i] ^= key[i & 63],
    // split so LLVM can vectorize the full 64-byte groups.
    let (chunks, remainder) = data.as_chunks_mut::<64>();
    for chunk in chunks {
        for (d, k) in chunk.iter_mut().zip(key.iter()) {
            *d ^= *k;
        }
    }
    for (d, k) in remainder.iter_mut().zip(key.iter()) {
        *d ^= *k;
    }
}

/// Length of the prefix of `chars` containing only printable ASCII
/// codepoints (32 <= ch <= 126).
pub fn printable_ascii_run_length(chars: &[u32]) -> usize {
    // Mirrors the tight C scalar loop; indexing through a raw pointer avoids
    // re-checking the slice bound on every iteration.
    let len = chars.len();
    let mut n = 0usize;
    // Check eight codepoints at a time with a fixed-length inner loop so LLVM
    // can vectorize it, then fall back to the tight one-at-a-time loop.
    while n + 8 <= len {
        let mut bad = 0u32;
        for k in 0..8 {
            bad |= (chars[n + k].wrapping_sub(32) >= 95) as u32;
        }
        if bad != 0 {
            break;
        }
        n += 8;
    }
    while n < len && chars[n].wrapping_sub(32) < 95 {
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
