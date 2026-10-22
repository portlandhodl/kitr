// SPDX-License-Identifier: GPL-3.0-or-later
//! 256-bit (AVX2) implementations, mirroring `simd-string-256.c`.

use core::arch::x86_64::*;

use crate::utf8::{decode_all_scalar, decode_to_accept_scalar, UTF8Decoder, UTF8_ACCEPT};

const KEY_SIZE: usize = 64;
const LANES_BYTES: usize = 32;

#[inline(always)]
unsafe fn set1_epi8(x: u8) -> __m256i {
    _mm256_set1_epi8(x as i8)
}

#[inline(always)]
unsafe fn cmplt_epi8(a: __m256i, b: __m256i) -> __m256i {
    _mm256_cmpgt_epi8(b, a)
}

// shift_right moves bytes towards higher indices (slli semantics).
#[inline(always)]
unsafe fn shift_right_by_one_byte(a: __m256i) -> __m256i {
    _mm256_alignr_epi8::<15>(a, _mm256_permute2x128_si256::<0x08>(a, a))
}

#[inline(always)]
unsafe fn shift_right_by_two_bytes(a: __m256i) -> __m256i {
    _mm256_alignr_epi8::<14>(a, _mm256_permute2x128_si256::<0x08>(a, a))
}

#[inline(always)]
unsafe fn shift_left_in_lane_by_one_byte(a: __m256i) -> __m256i {
    _mm256_srli_si256::<1>(a)
}

#[inline(always)]
unsafe fn shift_left_in_lane_by_two_bytes(a: __m256i) -> __m256i {
    _mm256_srli_si256::<2>(a)
}

#[inline(always)]
unsafe fn shift_left_in_lane_by_four_bytes(a: __m256i) -> __m256i {
    _mm256_srli_si256::<4>(a)
}

#[inline(always)]
unsafe fn shift_left_in_lane_by_eight_bytes(a: __m256i) -> __m256i {
    _mm256_srli_si256::<8>(a)
}

#[inline(always)]
unsafe fn shift_right_in_lane_by_one_byte(a: __m256i) -> __m256i {
    _mm256_slli_si256::<1>(a)
}

#[inline(always)]
unsafe fn shift_right_in_lane_by_two_bytes(a: __m256i) -> __m256i {
    _mm256_slli_si256::<2>(a)
}

#[inline(always)]
unsafe fn shift_right_in_lane_by_four_bytes(a: __m256i) -> __m256i {
    _mm256_slli_si256::<4>(a)
}

#[inline(always)]
unsafe fn shift_right_in_lane_by_eight_bytes(a: __m256i) -> __m256i {
    _mm256_slli_si256::<8>(a)
}

#[inline(always)]
unsafe fn zero_last_n_bytes(vec: __m256i, n: u32) -> __m256i {
    debug_assert!(n <= 32);
    if n == 0 {
        return vec;
    }
    let mut bytes = [0xffu8; 32];
    for b in bytes.iter_mut().skip(32 - n as usize) {
        *b = 0;
    }
    let mask = _mm256_loadu_si256(bytes.as_ptr() as *const __m256i);
    _mm256_and_si256(mask, vec)
}

#[inline(always)]
unsafe fn is_zero(a: __m256i) -> bool {
    _mm256_testz_si256(a, a) != 0
}

#[inline(always)]
unsafe fn bytes_to_first_match(vec: __m256i) -> i32 {
    if is_zero(vec) {
        -1
    } else {
        (_mm256_movemask_epi8(vec) as u32).trailing_zeros() as i32
    }
}

#[inline(always)]
fn low_bits(n: u32) -> u32 {
    if n >= 32 {
        u32::MAX
    } else {
        (1u32 << n) - 1
    }
}

#[target_feature(enable = "avx2")]
/// # Safety
///
/// The CPU must support every instruction set enabled on this function (see
/// the module-level documentation). The safe public wrappers in the crate root
/// guarantee this with runtime feature detection.
pub unsafe fn find_either_of_two_bytes(haystack: &[u8], a: u8, b: u8) -> Option<usize> {
    if haystack.is_empty() {
        return None;
    }
    let a_vec = set1_epi8(a);
    let b_vec = set1_epi8(b);
    let mut i = 0;
    while i + LANES_BYTES <= haystack.len() {
        let chunk = _mm256_loadu_si256(haystack.as_ptr().add(i) as *const __m256i);
        let test = _mm256_or_si256(
            _mm256_cmpeq_epi8(chunk, a_vec),
            _mm256_cmpeq_epi8(chunk, b_vec),
        );
        let mask = _mm256_movemask_epi8(test) as u32;
        if mask != 0 {
            return Some(i + mask.trailing_zeros() as usize);
        }
        i += LANES_BYTES;
    }
    let tail = haystack.len() - i;
    if tail != 0 {
        let mut tmp = [0u8; LANES_BYTES];
        tmp[..tail].copy_from_slice(&haystack[i..]);
        let chunk = _mm256_loadu_si256(tmp.as_ptr() as *const __m256i);
        let test = _mm256_or_si256(
            _mm256_cmpeq_epi8(chunk, a_vec),
            _mm256_cmpeq_epi8(chunk, b_vec),
        );
        let mask = (_mm256_movemask_epi8(test) as u32) & low_bits(tail as u32);
        if mask != 0 {
            return Some(i + mask.trailing_zeros() as usize);
        }
    }
    None
}

#[target_feature(enable = "avx2")]
/// # Safety
///
/// The CPU must support every instruction set enabled on this function (see
/// the module-level documentation). The safe public wrappers in the crate root
/// guarantee this with runtime feature detection.
pub unsafe fn xor_data64(key: &[u8; KEY_SIZE], data: &mut [u8]) {
    let unaligned_bytes = KEY_SIZE - (data.as_ptr() as usize & (KEY_SIZE - 1));
    if data.len() <= unaligned_bytes {
        for i in 0..data.len() {
            data[i] ^= key[i];
        }
        return;
    }
    for i in 0..unaligned_bytes {
        data[i] ^= key[i];
    }
    let mut rotated = [0u8; KEY_SIZE];
    rotated[..KEY_SIZE - unaligned_bytes].copy_from_slice(&key[unaligned_bytes..]);
    rotated[KEY_SIZE - unaligned_bytes..].copy_from_slice(&key[..unaligned_bytes]);
    let k0 = _mm256_loadu_si256(rotated.as_ptr() as *const __m256i);
    let k1 = _mm256_loadu_si256(rotated.as_ptr().add(32) as *const __m256i);
    let keys = [k0, k1];
    let mut i = unaligned_bytes;
    let mut j = 0usize;
    while i + 64 <= data.len() {
        for (q, k) in keys.iter().enumerate() {
            let p = data.as_mut_ptr().add(i + 32 * q) as *mut __m256i;
            _mm256_storeu_si256(p, _mm256_xor_si256(_mm256_loadu_si256(p), *k));
        }
        i += 64;
        j += 64;
    }
    while i + 32 <= data.len() {
        let k = keys[(j / 32) & 1];
        let p = data.as_mut_ptr().add(i) as *mut __m256i;
        _mm256_storeu_si256(p, _mm256_xor_si256(_mm256_loadu_si256(p), k));
        i += 32;
        j += 32;
    }
    while i < data.len() {
        data[i] ^= rotated[j & 63];
        i += 1;
        j += 1;
    }
}

#[target_feature(enable = "avx2")]
/// # Safety
///
/// The CPU must support every instruction set enabled on this function (see
/// the module-level documentation). The safe public wrappers in the crate root
/// guarantee this with runtime feature detection.
pub unsafe fn printable_ascii_run_length(chars: &[u32]) -> usize {
    let sign = _mm256_set1_epi32(i32::MIN);
    let lower = _mm256_set1_epi32((32u32 ^ 0x8000_0000) as i32);
    let upper = _mm256_set1_epi32((126u32 ^ 0x8000_0000) as i32);
    let lanes = 8;
    let mut i = 0;
    while i + lanes <= chars.len() {
        let chunk = _mm256_loadu_si256(chars.as_ptr().add(i) as *const __m256i);
        let chunk = _mm256_xor_si256(chunk, sign);
        let non_printable = _mm256_or_si256(
            _mm256_cmpgt_epi32(lower, chunk),
            _mm256_cmpgt_epi32(chunk, upper),
        );
        let n = bytes_to_first_match(non_printable);
        if n > -1 {
            return i + n as usize / 4;
        }
        i += lanes;
    }
    while i < chars.len() && chars[i].wrapping_sub(32) < 95 {
        i += 1;
    }
    i
}

// Pixel compositing {{{

#[inline(always)]
unsafe fn div255_epu16(x: __m256i) -> __m256i {
    let y = _mm256_add_epi16(x, _mm256_set1_epi16(128));
    _mm256_srli_epi16::<8>(_mm256_add_epi16(y, _mm256_srli_epi16::<8>(y)))
}

#[inline(always)]
unsafe fn alpha_broadcast_pattern() -> __m256i {
    _mm256_set_epi8(
        15, 15, 15, 15, 11, 11, 11, 11, 7, 7, 7, 7, 3, 3, 3, 3, 15, 15, 15, 15, 11, 11, 11, 11, 7,
        7, 7, 7, 3, 3, 3, 3,
    )
}

#[inline(always)]
unsafe fn blend_opaque_pixels(under: __m256i, over: __m256i) -> __m256i {
    let zero = _mm256_setzero_si256();
    let alpha = _mm256_shuffle_epi8(over, alpha_broadcast_pattern());
    let inv_alpha = _mm256_xor_si256(alpha, _mm256_set1_epi64x(-1));
    let lo = div255_epu16(_mm256_add_epi16(
        _mm256_mullo_epi16(
            _mm256_unpacklo_epi8(over, zero),
            _mm256_unpacklo_epi8(alpha, zero),
        ),
        _mm256_mullo_epi16(
            _mm256_unpacklo_epi8(under, zero),
            _mm256_unpacklo_epi8(inv_alpha, zero),
        ),
    ));
    let hi = div255_epu16(_mm256_add_epi16(
        _mm256_mullo_epi16(
            _mm256_unpackhi_epi8(over, zero),
            _mm256_unpackhi_epi8(alpha, zero),
        ),
        _mm256_mullo_epi16(
            _mm256_unpackhi_epi8(under, zero),
            _mm256_unpackhi_epi8(inv_alpha, zero),
        ),
    ));
    _mm256_or_si256(
        _mm256_packus_epi16(lo, hi),
        _mm256_set1_epi32(0xff00_0000u32 as i32),
    )
}

#[inline(always)]
unsafe fn blend_straight_dwords(d: __m256i, s: __m256i) -> __m256i {
    let c255 = _mm256_set1_epi32(255);
    let alpha = _mm256_shuffle_epi32::<0xff>(s);
    let dst_alpha = _mm256_shuffle_epi32::<0xff>(d);
    let inv_alpha = _mm256_sub_epi32(c255, alpha);
    let denom = _mm256_add_epi32(
        _mm256_mullo_epi32(alpha, c255),
        _mm256_mullo_epi32(dst_alpha, inv_alpha),
    );
    let num = _mm256_add_epi32(
        _mm256_mullo_epi32(_mm256_mullo_epi32(s, alpha), c255),
        _mm256_mullo_epi32(_mm256_mullo_epi32(d, dst_alpha), inv_alpha),
    );
    let mut r = _mm256_cvtps_epi32(_mm256_div_ps(
        _mm256_cvtepi32_ps(num),
        _mm256_cvtepi32_ps(denom),
    ));
    let y = _mm256_add_epi32(denom, _mm256_set1_epi32(128));
    let out_alpha = _mm256_srli_epi32::<8>(_mm256_add_epi32(y, _mm256_srli_epi32::<8>(y)));
    r = _mm256_blendv_epi8(r, out_alpha, _mm256_set_epi32(-1, 0, 0, 0, -1, 0, 0, 0));
    _mm256_blendv_epi8(r, d, _mm256_cmpeq_epi32(denom, _mm256_setzero_si256()))
}

#[inline(always)]
unsafe fn widen_quarter(a: __m256i, q: usize) -> __m256i {
    let x = _mm256_extracti128_si256::<0>(a);
    let x = if q < 2 {
        x
    } else {
        _mm256_extracti128_si256::<1>(a)
    };
    let x = if q & 1 == 0 {
        x
    } else {
        _mm_srli_si128::<8>(x)
    };
    _mm256_cvtepu8_epi32(x)
}

#[target_feature(enable = "avx2")]
/// # Safety
///
/// The CPU must support every instruction set enabled on this function (see
/// the module-level documentation). The safe public wrappers in the crate root
/// guarantee this with runtime feature detection.
pub unsafe fn blend_over_straight(dst: &mut [u8], src: &[u8]) {
    crate::scalar_checks::assert_blend_straight_lengths(dst, src);
    let num_pixels = src.len() / 4;
    let fixup = _mm256_setr_epi32(0, 4, 1, 5, 2, 6, 3, 7);
    let mut i = 0;
    while i + 8 <= num_pixels {
        let s = _mm256_loadu_si256(src.as_ptr().add(4 * i) as *const __m256i);
        let d = _mm256_loadu_si256(dst.as_ptr().add(4 * i) as *const __m256i);
        let r0 = blend_straight_dwords(widen_quarter(d, 0), widen_quarter(s, 0));
        let r1 = blend_straight_dwords(widen_quarter(d, 1), widen_quarter(s, 1));
        let r2 = blend_straight_dwords(widen_quarter(d, 2), widen_quarter(s, 2));
        let r3 = blend_straight_dwords(widen_quarter(d, 3), widen_quarter(s, 3));
        let packed = _mm256_packus_epi16(_mm256_packus_epi32(r0, r1), _mm256_packus_epi32(r2, r3));
        let packed = _mm256_permutevar8x32_epi32(packed, fixup);
        _mm256_storeu_si256(dst.as_mut_ptr().add(4 * i) as *mut __m256i, packed);
        i += 8;
    }
    for j in i..num_pixels {
        crate::scalar::blend_pixel_over_straight(
            &mut dst[4 * j..4 * j + 4],
            &src[4 * j..4 * j + 4],
        );
    }
}

#[target_feature(enable = "avx2")]
/// # Safety
///
/// The CPU must support every instruction set enabled on this function (see
/// the module-level documentation). The safe public wrappers in the crate root
/// guarantee this with runtime feature detection.
pub unsafe fn blend_over_opaque(dst: &mut [u8], dst_bpp: u32, src: &[u8]) {
    crate::scalar_checks::assert_blend_opaque_lengths(dst, dst_bpp, src);
    let num_pixels = src.len() / 4;
    let mut i = 0;
    if dst_bpp == 4 {
        while i + 8 <= num_pixels {
            let s = _mm256_loadu_si256(src.as_ptr().add(4 * i) as *const __m256i);
            let d = _mm256_loadu_si256(dst.as_ptr().add(4 * i) as *const __m256i);
            let blended = blend_opaque_pixels(d, s);
            _mm256_storeu_si256(dst.as_mut_ptr().add(4 * i) as *mut __m256i, blended);
            i += 8;
        }
    } else {
        // The byte expand/compact shuffles cannot cross 128-bit lanes, so the
        // 3-byte destination kernel is 128-bit wide, as in the C code.
        return crate::x86::sse2::blend_over_opaque(dst, 3, src);
    }
    let bpp = dst_bpp as usize;
    for j in i..num_pixels {
        crate::scalar::blend_pixel_over_opaque(
            &mut dst[bpp * j..bpp * j + bpp],
            &src[4 * j..4 * j + 4],
            bpp,
        );
    }
}

#[inline(always)]
unsafe fn load_bytes_as_dwords(p: *const u8) -> __m256i {
    _mm256_cvtepu8_epi32(_mm_loadl_epi64(p as *const __m128i))
}

#[target_feature(enable = "avx2")]
/// # Safety
///
/// The CPU must support every instruction set enabled on this function (see
/// the module-level documentation). The safe public wrappers in the crate root
/// guarantee this with runtime feature detection.
pub unsafe fn composite_alpha_mask(dst: &mut [u32], mask: &[u8], color_rgb: u32) {
    assert_eq!(
        dst.len(),
        mask.len(),
        "dst must contain one 32-bit pixel per mask byte"
    );
    let col = color_rgb.wrapping_shl(8) & 0xffff_ff00;
    let col_vec = _mm256_set1_epi32(col as i32);
    let low_byte = _mm256_set1_epi32(0xff);
    let mut i = 0;
    while i + 8 <= dst.len() {
        let m = load_bytes_as_dwords(mask.as_ptr().add(i));
        let d = _mm256_loadu_si256(dst.as_ptr().add(i) as *const __m256i);
        let r = _mm256_or_si256(col_vec, _mm256_max_epu32(m, _mm256_and_si256(d, low_byte)));
        _mm256_storeu_si256(dst.as_mut_ptr().add(i) as *mut __m256i, r);
        i += 8;
    }
    for j in i..dst.len() {
        dst[j] = col | (dst[j] & 0xff).max(mask[j] as u32);
    }
}

// }}}

// UTF-8 {{{

#[inline(always)]
unsafe fn output_plain_ascii(d: &mut UTF8Decoder, vec: __m256i, src_sz: usize) {
    let pos = d.output_len();
    let limit = pos + src_sz;
    let out = d.output_ptr();
    let mut p = pos;
    let mut x = _mm256_extracti128_si256::<0>(vec);
    let mut unpacked = _mm256_cvtepu8_epi32(x);
    _mm256_storeu_si256(out.add(p) as *mut __m256i, unpacked);
    p += 8;
    if p < limit {
        x = _mm_srli_si128::<8>(x);
        unpacked = _mm256_cvtepu8_epi32(x);
        _mm256_storeu_si256(out.add(p) as *mut __m256i, unpacked);
        p += 8;
        if p < limit {
            x = _mm256_extracti128_si256::<1>(vec);
            unpacked = _mm256_cvtepu8_epi32(x);
            _mm256_storeu_si256(out.add(p) as *mut __m256i, unpacked);
            p += 8;
            if p < limit {
                x = _mm_srli_si128::<8>(x);
                unpacked = _mm256_cvtepu8_epi32(x);
                _mm256_storeu_si256(out.add(p) as *mut __m256i, unpacked);
            }
        }
    }
    d.set_output_len(pos + src_sz);
}

#[allow(unused_assignments)]
#[inline(always)]
unsafe fn output_unicode(
    d: &mut UTF8Decoder,
    output1: __m256i,
    output2: __m256i,
    output3: __m256i,
    num_codepoints_lane0: usize,
    num_codepoints_lane1: usize,
) {
    let base = d.output_len();
    let out = d.output_ptr();
    let mut p = base;
    macro_rules! chunk {
        ($x1:expr, $x2:expr, $x3:expr) => {{
            let unpacked1 = _mm256_cvtepu8_epi32($x1);
            let unpacked2 = shift_right_in_lane_by_one_byte(_mm256_cvtepu8_epi32($x2));
            let unpacked3 = shift_right_in_lane_by_two_bytes(_mm256_cvtepu8_epi32($x3));
            let unpacked = _mm256_or_si256(_mm256_or_si256(unpacked1, unpacked2), unpacked3);
            _mm256_storeu_si256(out.add(p) as *mut __m256i, unpacked);
            p += 8;
        }};
    }
    let mut x1 = _mm256_extracti128_si256::<0>(output1);
    let mut x2 = _mm256_extracti128_si256::<0>(output2);
    let mut x3 = _mm256_extracti128_si256::<0>(output3);
    chunk!(x1, x2, x3);
    if num_codepoints_lane0 > 8 {
        x1 = _mm_srli_si128::<8>(x1);
        x2 = _mm_srli_si128::<8>(x2);
        x3 = _mm_srli_si128::<8>(x3);
        chunk!(x1, x2, x3);
    }
    p = base + num_codepoints_lane0;
    x1 = _mm256_extracti128_si256::<1>(output1);
    x2 = _mm256_extracti128_si256::<1>(output2);
    x3 = _mm256_extracti128_si256::<1>(output3);
    chunk!(x1, x2, x3);
    if num_codepoints_lane1 > 8 {
        x1 = _mm_srli_si128::<8>(x1);
        x2 = _mm_srli_si128::<8>(x2);
        x3 = _mm_srli_si128::<8>(x3);
        chunk!(x1, x2, x3);
    }
    d.set_output_len(base + num_codepoints_lane0 + num_codepoints_lane1);
}

#[target_feature(enable = "avx2")]
/// # Safety
///
/// The CPU must support every instruction set enabled on this function (see
/// the module-level documentation). The safe public wrappers in the crate root
/// guarantee this with runtime feature detection.
pub unsafe fn utf8_decode_to_esc(d: &mut UTF8Decoder, src: &[u8]) -> bool {
    d.begin_call();
    let mut src_data = src;
    let mut src_len = src.len();
    if d.cur != UTF8_ACCEPT {
        d.ensure_capacity(src_len);
        let n = decode_to_accept_scalar(d, src_data);
        d.num_consumed = n;
        src_data = &src_data[n..];
        src_len -= n;
    }
    d.ensure_capacity(src_len + 64);
    let esc_vec = set1_epi8(0x1b);
    let zero = _mm256_setzero_si256();
    let one = set1_epi8(1);
    let two = set1_epi8(2);
    let three = set1_epi8(3);
    let four = set1_epi8(4);
    let numbered = _mm256_set_epi8(
        31, 30, 29, 28, 27, 26, 25, 24, 23, 22, 21, 20, 19, 18, 17, 16, 15, 14, 13, 12, 11, 10, 9,
        8, 7, 6, 5, 4, 3, 2, 1, 0,
    );
    let numbered_in_lane = _mm256_set_epi8(
        15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0, 15, 14, 13, 12, 11, 10, 9, 8, 7, 6,
        5, 4, 3, 2, 1, 0,
    );
    let mut p = 0usize;
    let limit = src_len;
    let mut sentinel_found = false;
    let mut chunk_src_sz: usize;
    let mut prev_chunk_was_all_ascii = true;

    while p < limit && !sentinel_found {
        if prev_chunk_was_all_ascii {
            while limit - p >= 2 * LANES_BYTES {
                let v1 = _mm256_loadu_si256(src_data.as_ptr().add(p) as *const __m256i);
                let v2 =
                    _mm256_loadu_si256(src_data.as_ptr().add(p + LANES_BYTES) as *const __m256i);
                let esc_or_non_ascii = _mm256_or_si256(
                    _mm256_or_si256(
                        _mm256_cmpeq_epi8(v1, esc_vec),
                        _mm256_cmpeq_epi8(v2, esc_vec),
                    ),
                    _mm256_or_si256(v1, v2),
                );
                if _mm256_movemask_epi8(esc_or_non_ascii) != 0 {
                    break;
                }
                output_plain_ascii(d, v1, LANES_BYTES);
                output_plain_ascii(d, v2, LANES_BYTES);
                d.num_consumed += 2 * LANES_BYTES;
                p += 2 * LANES_BYTES;
            }
            if p >= limit {
                break;
            }
        }
        chunk_src_sz = (limit - p).min(LANES_BYTES);
        let mut vec = if chunk_src_sz == LANES_BYTES {
            _mm256_loadu_si256(src_data.as_ptr().add(p) as *const __m256i)
        } else {
            let mut tmp = [0u8; LANES_BYTES];
            tmp[..chunk_src_sz].copy_from_slice(&src_data[p..p + chunk_src_sz]);
            _mm256_loadu_si256(tmp.as_ptr() as *const __m256i)
        };
        let start_of_current_chunk = p;
        p += chunk_src_sz;

        let mut ascii_mask = _mm256_movemask_epi8(vec) as u32;
        let mut esc_mask = _mm256_movemask_epi8(_mm256_cmpeq_epi8(vec, esc_vec)) as u32;
        if chunk_src_sz < LANES_BYTES {
            let chunk_mask = low_bits(chunk_src_sz as u32);
            ascii_mask &= chunk_mask;
            esc_mask &= chunk_mask;
        }
        if esc_mask != 0 {
            sentinel_found = true;
            chunk_src_sz = esc_mask.trailing_zeros() as usize;
            ascii_mask &= low_bits(chunk_src_sz as u32);
            d.num_consumed += chunk_src_sz + 1;
            if chunk_src_sz == 0 {
                continue;
            }
        } else {
            d.num_consumed += chunk_src_sz;
        }

        let mut num_of_trailing_bytes = 0usize;
        let mut check_for_trailing_bytes = !sentinel_found;
        let mut stop = false;

        loop {
            if ascii_mask == 0 {
                output_plain_ascii(d, vec, chunk_src_sz);
                prev_chunk_was_all_ascii = true;
                if num_of_trailing_bytes != 0 {
                    if p >= limit {
                        decode_all_scalar(d, &src_data[p - num_of_trailing_bytes..p]);
                        d.num_consumed += num_of_trailing_bytes;
                        stop = true;
                    } else {
                        p -= num_of_trailing_bytes;
                    }
                }
                break;
            }
            prev_chunk_was_all_ascii = false;
            if chunk_src_sz < LANES_BYTES {
                vec = zero_last_n_bytes(vec, (LANES_BYTES - chunk_src_sz) as u32);
            }
            let state80 = set1_epi8(0x80);
            let vec_signed = _mm256_add_epi8(vec, state80);
            let start_two = cmplt_epi8(set1_epi8(0xc0 - 1 - 0x80), vec_signed);
            let mut state = set1_epi8(0x80);
            state = _mm256_blendv_epi8(state, set1_epi8(0xc2), start_two);
            let start_three = cmplt_epi8(set1_epi8(0xe0 - 1 - 0x80), vec_signed);
            state = _mm256_blendv_epi8(state, set1_epi8(0xe3), start_three);
            let start_four = cmplt_epi8(set1_epi8(0xf0 - 1 - 0x80), vec_signed);
            state = _mm256_blendv_epi8(state, set1_epi8(0xf4), start_four);
            let mask = _mm256_and_si256(state, set1_epi8(0xf8));
            let count = _mm256_and_si256(state, set1_epi8(0x7));
            let count_subs1 = _mm256_subs_epu8(count, one);
            let mut counts = _mm256_add_epi8(count, shift_right_by_one_byte(count_subs1));
            counts = _mm256_add_epi8(
                counts,
                shift_right_by_two_bytes(_mm256_subs_epu8(counts, two)),
            );
            if check_for_trailing_bytes
                && !is_zero(cmplt_epi8(
                    one,
                    _mm256_and_si256(
                        counts,
                        _mm256_cmpeq_epi8(numbered, _mm256_set1_epi8((chunk_src_sz - 1) as i8)),
                    ),
                ))
            {
                check_for_trailing_bytes = false;
                let last = start_of_current_chunk + chunk_src_sz - 1;
                if src_data[last] >= 0xc0 {
                    num_of_trailing_bytes = 1;
                } else if chunk_src_sz > 1 && src_data[last - 1] >= 0xe0 {
                    num_of_trailing_bytes = 2;
                } else if chunk_src_sz > 2 && src_data[last - 2] >= 0xf0 {
                    num_of_trailing_bytes = 3;
                }
                if num_of_trailing_bytes != 0 {
                    chunk_src_sz -= num_of_trailing_bytes;
                    d.num_consumed -= num_of_trailing_bytes;
                    if chunk_src_sz == 0 {
                        decode_all_scalar(
                            d,
                            &src_data[start_of_current_chunk
                                ..start_of_current_chunk + num_of_trailing_bytes],
                        );
                        d.num_consumed += num_of_trailing_bytes;
                        stop = true;
                        break;
                    }
                    ascii_mask &= low_bits(chunk_src_sz as u32);
                    vec = zero_last_n_bytes(vec, (LANES_BYTES - chunk_src_sz) as u32);
                }
                continue;
            }

            let ascii_sequence_count_mismatches =
                ascii_mask ^ (_mm256_movemask_epi8(_mm256_cmpgt_epi8(counts, zero)) as u32);
            let mut chunk_is_invalid = set1_epi8(if ascii_sequence_count_mismatches != 0 {
                0xff
            } else {
                0x00
            });
            chunk_is_invalid = _mm256_or_si256(
                chunk_is_invalid,
                _mm256_and_si256(start_two, cmplt_epi8(vec, set1_epi8(0xc2))),
            );
            chunk_is_invalid = _mm256_or_si256(
                chunk_is_invalid,
                _mm256_and_si256(start_four, _mm256_cmpgt_epi8(vec, set1_epi8(0xf4))),
            );
            chunk_is_invalid = _mm256_or_si256(
                chunk_is_invalid,
                _mm256_andnot_si256(
                    cmplt_epi8(vec, set1_epi8(0xc0)),
                    _mm256_cmpgt_epi8(counts, count),
                ),
            );
            let prev_bytes = shift_right_by_one_byte(vec);
            let e0_first_follower = _mm256_cmpeq_epi8(prev_bytes, set1_epi8(0xe0));
            chunk_is_invalid = _mm256_or_si256(
                chunk_is_invalid,
                _mm256_and_si256(
                    e0_first_follower,
                    cmplt_epi8(_mm256_and_si256(e0_first_follower, vec), set1_epi8(0xa0)),
                ),
            );
            let ed_first_follower = _mm256_cmpeq_epi8(prev_bytes, set1_epi8(0xed));
            chunk_is_invalid = _mm256_or_si256(
                chunk_is_invalid,
                _mm256_and_si256(
                    ed_first_follower,
                    _mm256_cmpgt_epi8(_mm256_and_si256(ed_first_follower, vec), set1_epi8(0x9f)),
                ),
            );
            let f0_first_follower = _mm256_cmpeq_epi8(prev_bytes, set1_epi8(0xf0));
            chunk_is_invalid = _mm256_or_si256(
                chunk_is_invalid,
                _mm256_and_si256(
                    f0_first_follower,
                    cmplt_epi8(_mm256_and_si256(f0_first_follower, vec), set1_epi8(0x90)),
                ),
            );
            let f4_first_follower = _mm256_cmpeq_epi8(prev_bytes, set1_epi8(0xf4));
            chunk_is_invalid = _mm256_or_si256(
                chunk_is_invalid,
                _mm256_and_si256(
                    f4_first_follower,
                    _mm256_cmpgt_epi8(_mm256_and_si256(f4_first_follower, vec), set1_epi8(0x8f)),
                ),
            );
            if !is_zero(chunk_is_invalid) {
                decode_all_scalar(
                    d,
                    &src_data[start_of_current_chunk
                        ..start_of_current_chunk + chunk_src_sz + num_of_trailing_bytes],
                );
                d.num_consumed += num_of_trailing_bytes;
                stop = true;
                break;
            }

            vec = _mm256_andnot_si256(mask, vec);
            let count2_locations = _mm256_cmpeq_epi8(counts, two);
            let count3_locations = _mm256_cmpeq_epi8(counts, three);
            let vec_non_ascii = _mm256_andnot_si256(_mm256_cmpeq_epi8(counts, zero), vec);
            let mut output1 = _mm256_blendv_epi8(
                vec,
                _mm256_or_si256(
                    vec,
                    _mm256_and_si256(
                        _mm256_slli_epi16::<6>(shift_right_by_one_byte(vec_non_ascii)),
                        set1_epi8(0xc0),
                    ),
                ),
                _mm256_cmpeq_epi8(counts, one),
            );
            let mut output2 = _mm256_and_si256(vec, count2_locations);
            output2 = _mm256_srli_epi32::<2>(output2);
            output2 = _mm256_or_si256(
                output2,
                _mm256_and_si256(
                    set1_epi8(0xf0),
                    _mm256_slli_epi16::<4>(shift_right_by_one_byte(_mm256_and_si256(
                        count3_locations,
                        vec_non_ascii,
                    ))),
                ),
            );
            output2 = _mm256_and_si256(output2, count2_locations);
            output2 = shift_right_by_one_byte(output2);
            let mut output3 = _mm256_and_si256(three, _mm256_srli_epi32::<4>(vec));
            let count4_locations = _mm256_cmpeq_epi8(counts, four);
            output3 = _mm256_or_si256(
                output3,
                _mm256_and_si256(
                    set1_epi8(0xfc),
                    _mm256_slli_epi16::<2>(shift_right_by_one_byte(_mm256_and_si256(
                        count4_locations,
                        vec_non_ascii,
                    ))),
                ),
            );
            output3 = _mm256_and_si256(output3, count3_locations);
            output3 = shift_right_by_two_bytes(output3);

            let discarded_locations = _mm256_cmpgt_epi8(counts, one);
            let mut shifts = _mm256_and_si256(one, discarded_locations);
            shifts = _mm256_add_epi8(shifts, shift_right_in_lane_by_one_byte(shifts));
            shifts = _mm256_add_epi8(shifts, shift_right_in_lane_by_two_bytes(shifts));
            shifts = _mm256_add_epi8(shifts, shift_right_in_lane_by_four_bytes(shifts));
            shifts = _mm256_add_epi8(shifts, shift_right_in_lane_by_eight_bytes(shifts));
            shifts = _mm256_andnot_si256(discarded_locations, shifts);
            #[inline(always)]
            unsafe fn move1(a: __m256i) -> __m256i {
                _mm256_blendv_epi8(
                    a,
                    shift_left_in_lane_by_one_byte(a),
                    shift_left_in_lane_by_one_byte(_mm256_slli_epi16::<7>(a)),
                )
            }
            #[inline(always)]
            unsafe fn move2(a: __m256i) -> __m256i {
                _mm256_blendv_epi8(
                    a,
                    shift_left_in_lane_by_two_bytes(a),
                    shift_left_in_lane_by_two_bytes(_mm256_slli_epi16::<6>(a)),
                )
            }
            #[inline(always)]
            unsafe fn move4(a: __m256i) -> __m256i {
                _mm256_blendv_epi8(
                    a,
                    shift_left_in_lane_by_four_bytes(a),
                    shift_left_in_lane_by_four_bytes(_mm256_slli_epi16::<5>(a)),
                )
            }
            #[inline(always)]
            unsafe fn move8(a: __m256i) -> __m256i {
                _mm256_blendv_epi8(
                    a,
                    shift_left_in_lane_by_eight_bytes(a),
                    shift_left_in_lane_by_eight_bytes(_mm256_slli_epi16::<4>(a)),
                )
            }
            shifts = move1(shifts);
            shifts = move2(shifts);
            shifts = move4(shifts);
            shifts = move8(shifts);
            shifts = _mm256_add_epi8(shifts, numbered_in_lane);

            output1 = _mm256_shuffle_epi8(output1, shifts);
            output2 = _mm256_shuffle_epi8(output2, shifts);
            output3 = _mm256_shuffle_epi8(output3, shifts);

            let mut kept_mask = !(_mm256_movemask_epi8(discarded_locations) as u32);
            if chunk_src_sz < 32 {
                kept_mask &= low_bits(chunk_src_sz as u32);
            }
            let num_codepoints_lane0 = (kept_mask & 0xffff).count_ones() as usize;
            let num_codepoints_lane1 = (kept_mask >> 16).count_ones() as usize;
            output_unicode(
                d,
                output1,
                output2,
                output3,
                num_codepoints_lane0,
                num_codepoints_lane1,
            );

            if num_of_trailing_bytes != 0 {
                if p >= limit {
                    decode_all_scalar(d, &src_data[p - num_of_trailing_bytes..p]);
                    d.num_consumed += num_of_trailing_bytes;
                    stop = true;
                } else {
                    p -= num_of_trailing_bytes;
                }
            }
            break;
        }
        if stop {
            break;
        }
    }
    if sentinel_found && d.cur != UTF8_ACCEPT {
        d.ensure_capacity(1);
        d.push_codepoint(0xfffd);
        d.reset();
    }
    sentinel_found
}

// }}}
