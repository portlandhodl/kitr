// SPDX-License-Identifier: GPL-3.0-or-later
//! 128-bit (SSE) implementations, mirroring `simd-string-128.c`.
//!
//! The functions require SSE4.1, SSSE3 and SSE2 and are gated by the public
//! dispatcher on SSE4.2, exactly like the C code.

use core::arch::x86_64::*;

use crate::utf8::{decode_all_scalar, decode_to_accept_scalar, UTF8Decoder, UTF8_ACCEPT};

const KEY_SIZE: usize = 64;
const LANES_BYTES: usize = 16;

#[inline(always)]
unsafe fn set1_epi8(x: u8) -> __m128i {
    _mm_set1_epi8(x as i8)
}

#[inline(always)]
unsafe fn shift_left_by_bytes(a: __m128i, n: u32) -> __m128i {
    match n {
        1 => _mm_srli_si128::<1>(a),
        2 => _mm_srli_si128::<2>(a),
        3 => _mm_srli_si128::<3>(a),
        4 => _mm_srli_si128::<4>(a),
        5 => _mm_srli_si128::<5>(a),
        6 => _mm_srli_si128::<6>(a),
        7 => _mm_srli_si128::<7>(a),
        8 => _mm_srli_si128::<8>(a),
        9 => _mm_srli_si128::<9>(a),
        10 => _mm_srli_si128::<10>(a),
        11 => _mm_srli_si128::<11>(a),
        12 => _mm_srli_si128::<12>(a),
        13 => _mm_srli_si128::<13>(a),
        14 => _mm_srli_si128::<14>(a),
        15 => _mm_srli_si128::<15>(a),
        _ => a,
    }
}

#[inline(always)]
unsafe fn shift_right_by_one_byte(a: __m128i) -> __m128i {
    _mm_slli_si128::<1>(a)
}

#[inline(always)]
unsafe fn shift_right_by_two_bytes(a: __m128i) -> __m128i {
    _mm_slli_si128::<2>(a)
}

#[inline(always)]
unsafe fn shift_right_by_four_bytes(a: __m128i) -> __m128i {
    _mm_slli_si128::<4>(a)
}

#[inline(always)]
unsafe fn shift_right_by_eight_bytes(a: __m128i) -> __m128i {
    _mm_slli_si128::<8>(a)
}

#[inline(always)]
unsafe fn zero_last_n_bytes(vec: __m128i, n: u32) -> __m128i {
    let mask = shift_left_by_bytes(_mm_set1_epi64x(-1), n);
    _mm_and_si128(mask, vec)
}

#[inline(always)]
unsafe fn is_zero(a: __m128i) -> bool {
    _mm_testz_si128(a, a) != 0
}

#[inline(always)]
unsafe fn bytes_to_first_match(vec: __m128i) -> i32 {
    if is_zero(vec) {
        -1
    } else {
        (_mm_movemask_epi8(vec) as u32).trailing_zeros() as i32
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

#[target_feature(enable = "sse2,ssse3,sse4.1,sse4.2")]
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
        let chunk = _mm_loadu_si128(haystack.as_ptr().add(i) as *const __m128i);
        let test = _mm_or_si128(_mm_cmpeq_epi8(chunk, a_vec), _mm_cmpeq_epi8(chunk, b_vec));
        let mask = _mm_movemask_epi8(test) as u32;
        if mask != 0 {
            return Some(i + mask.trailing_zeros() as usize);
        }
        i += LANES_BYTES;
    }
    let tail = haystack.len() - i;
    if tail != 0 {
        let mut tmp = [0u8; LANES_BYTES];
        tmp[..tail].copy_from_slice(&haystack[i..]);
        let chunk = _mm_loadu_si128(tmp.as_ptr() as *const __m128i);
        let test = _mm_or_si128(_mm_cmpeq_epi8(chunk, a_vec), _mm_cmpeq_epi8(chunk, b_vec));
        let mask = (_mm_movemask_epi8(test) as u32) & low_bits(tail as u32);
        if mask != 0 {
            return Some(i + mask.trailing_zeros() as usize);
        }
    }
    None
}

#[target_feature(enable = "sse2,ssse3,sse4.1,sse4.2")]
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
    let k0 = _mm_loadu_si128(rotated.as_ptr() as *const __m128i);
    let k1 = _mm_loadu_si128(rotated.as_ptr().add(16) as *const __m128i);
    let k2 = _mm_loadu_si128(rotated.as_ptr().add(32) as *const __m128i);
    let k3 = _mm_loadu_si128(rotated.as_ptr().add(48) as *const __m128i);
    let keys = [k0, k1, k2, k3];
    let mut i = unaligned_bytes;
    let mut j = 0usize;
    while i + 64 <= data.len() {
        for (q, k) in keys.iter().enumerate() {
            let p = data.as_mut_ptr().add(i + 16 * q) as *mut __m128i;
            _mm_storeu_si128(p, _mm_xor_si128(_mm_loadu_si128(p), *k));
        }
        i += 64;
        j += 64;
    }
    while i + 16 <= data.len() {
        let k = keys[(j / 16) & 3];
        let p = data.as_mut_ptr().add(i) as *mut __m128i;
        _mm_storeu_si128(p, _mm_xor_si128(_mm_loadu_si128(p), k));
        i += 16;
        j += 16;
    }
    while i < data.len() {
        data[i] ^= rotated[j & 63];
        i += 1;
        j += 1;
    }
}

#[target_feature(enable = "sse2,ssse3,sse4.1,sse4.2")]
/// # Safety
///
/// The CPU must support every instruction set enabled on this function (see
/// the module-level documentation). The safe public wrappers in the crate root
/// guarantee this with runtime feature detection.
pub unsafe fn printable_ascii_run_length(chars: &[u32]) -> usize {
    let sign = _mm_set1_epi32(i32::MIN);
    let lower = _mm_set1_epi32((32u32 ^ 0x8000_0000) as i32);
    let upper = _mm_set1_epi32((126u32 ^ 0x8000_0000) as i32);
    let lanes = 4;
    let mut i = 0;
    while i + lanes <= chars.len() {
        let chunk = _mm_loadu_si128(chars.as_ptr().add(i) as *const __m128i);
        let chunk = _mm_xor_si128(chunk, sign);
        let non_printable =
            _mm_or_si128(_mm_cmpgt_epi32(lower, chunk), _mm_cmpgt_epi32(chunk, upper));
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
unsafe fn div255_epu16(x: __m128i) -> __m128i {
    let y = _mm_add_epi16(x, _mm_set1_epi16(128));
    _mm_srli_epi16::<8>(_mm_add_epi16(y, _mm_srli_epi16::<8>(y)))
}

#[inline(always)]
unsafe fn alpha_broadcast_pattern() -> __m128i {
    _mm_set_epi8(15, 15, 15, 15, 11, 11, 11, 11, 7, 7, 7, 7, 3, 3, 3, 3)
}

#[target_feature(enable = "sse2,ssse3,sse4.1,sse4.2")]
pub(crate) unsafe fn blend_opaque_pixels(under: __m128i, over: __m128i) -> __m128i {
    let zero = _mm_setzero_si128();
    let alpha = _mm_shuffle_epi8(over, alpha_broadcast_pattern());
    let inv_alpha = _mm_xor_si128(alpha, _mm_set1_epi64x(-1));
    let lo = div255_epu16(_mm_add_epi16(
        _mm_mullo_epi16(
            _mm_unpacklo_epi8(over, zero),
            _mm_unpacklo_epi8(alpha, zero),
        ),
        _mm_mullo_epi16(
            _mm_unpacklo_epi8(under, zero),
            _mm_unpacklo_epi8(inv_alpha, zero),
        ),
    ));
    let hi = div255_epu16(_mm_add_epi16(
        _mm_mullo_epi16(
            _mm_unpackhi_epi8(over, zero),
            _mm_unpackhi_epi8(alpha, zero),
        ),
        _mm_mullo_epi16(
            _mm_unpackhi_epi8(under, zero),
            _mm_unpackhi_epi8(inv_alpha, zero),
        ),
    ));
    _mm_or_si128(
        _mm_packus_epi16(lo, hi),
        _mm_set1_epi32(0xff00_0000u32 as i32),
    )
}

#[inline(always)]
unsafe fn blend_straight_dwords(d: __m128i, s: __m128i) -> __m128i {
    let c255 = _mm_set1_epi32(255);
    let alpha = _mm_shuffle_epi32::<0xff>(s);
    let dst_alpha = _mm_shuffle_epi32::<0xff>(d);
    let inv_alpha = _mm_sub_epi32(c255, alpha);
    let denom = _mm_add_epi32(
        _mm_mullo_epi32(alpha, c255),
        _mm_mullo_epi32(dst_alpha, inv_alpha),
    );
    let num = _mm_add_epi32(
        _mm_mullo_epi32(_mm_mullo_epi32(s, alpha), c255),
        _mm_mullo_epi32(_mm_mullo_epi32(d, dst_alpha), inv_alpha),
    );
    let mut r = _mm_cvtps_epi32(_mm_div_ps(_mm_cvtepi32_ps(num), _mm_cvtepi32_ps(denom)));
    let y = _mm_add_epi32(denom, _mm_set1_epi32(128));
    let out_alpha = _mm_srli_epi32::<8>(_mm_add_epi32(y, _mm_srli_epi32::<8>(y)));
    r = _mm_blendv_epi8(r, out_alpha, _mm_set_epi32(-1, 0, 0, 0));
    _mm_blendv_epi8(r, d, _mm_cmpeq_epi32(denom, _mm_setzero_si128()))
}

#[target_feature(enable = "sse2,ssse3,sse4.1,sse4.2")]
/// # Safety
///
/// The CPU must support every instruction set enabled on this function (see
/// the module-level documentation). The safe public wrappers in the crate root
/// guarantee this with runtime feature detection.
pub unsafe fn blend_over_straight(dst: &mut [u8], src: &[u8]) {
    crate::scalar_checks::assert_blend_straight_lengths(dst, src);
    let num_pixels = src.len() / 4;
    let mut i = 0;
    while i + 4 <= num_pixels {
        let s = _mm_loadu_si128(src.as_ptr().add(4 * i) as *const __m128i);
        let d = _mm_loadu_si128(dst.as_ptr().add(4 * i) as *const __m128i);
        let r0 = blend_straight_dwords(_mm_cvtepu8_epi32(d), _mm_cvtepu8_epi32(s));
        let r1 = blend_straight_dwords(
            _mm_cvtepu8_epi32(_mm_srli_si128::<4>(d)),
            _mm_cvtepu8_epi32(_mm_srli_si128::<4>(s)),
        );
        let r2 = blend_straight_dwords(
            _mm_cvtepu8_epi32(_mm_srli_si128::<8>(d)),
            _mm_cvtepu8_epi32(_mm_srli_si128::<8>(s)),
        );
        let r3 = blend_straight_dwords(
            _mm_cvtepu8_epi32(_mm_srli_si128::<12>(d)),
            _mm_cvtepu8_epi32(_mm_srli_si128::<12>(s)),
        );
        let packed = _mm_packus_epi16(_mm_packus_epi32(r0, r1), _mm_packus_epi32(r2, r3));
        _mm_storeu_si128(dst.as_mut_ptr().add(4 * i) as *mut __m128i, packed);
        i += 4;
    }
    for j in i..num_pixels {
        crate::scalar::blend_pixel_over_straight(
            &mut dst[4 * j..4 * j + 4],
            &src[4 * j..4 * j + 4],
        );
    }
}

#[target_feature(enable = "sse2,ssse3,sse4.1,sse4.2")]
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
        while i + 4 <= num_pixels {
            let s = _mm_loadu_si128(src.as_ptr().add(4 * i) as *const __m128i);
            let d = _mm_loadu_si128(dst.as_ptr().add(4 * i) as *const __m128i);
            let blended = blend_opaque_pixels(d, s);
            _mm_storeu_si128(dst.as_mut_ptr().add(4 * i) as *mut __m128i, blended);
            i += 4;
        }
    } else {
        // Four 3-byte pixels at a time. The 16-byte load from dst reads four
        // bytes beyond those pixels, which are inside dst while at least six
        // pixels remain. Only the 12 blended bytes are stored.
        let expand = _mm_set_epi8(-1, 11, 10, 9, -1, 8, 7, 6, -1, 5, 4, 3, -1, 2, 1, 0);
        let compact = _mm_set_epi8(-1, -1, -1, -1, 14, 13, 12, 10, 9, 8, 6, 5, 4, 2, 1, 0);
        while i + 6 <= num_pixels {
            let s = _mm_loadu_si128(src.as_ptr().add(4 * i) as *const __m128i);
            let d = _mm_loadu_si128(dst.as_ptr().add(3 * i) as *const __m128i);
            let blended =
                _mm_shuffle_epi8(blend_opaque_pixels(_mm_shuffle_epi8(d, expand), s), compact);
            let p = dst.as_mut_ptr().add(3 * i);
            _mm_storel_epi64(p as *mut __m128i, blended);
            let last = _mm_extract_epi32::<2>(blended) as u32;
            core::ptr::copy_nonoverlapping(
                &last as *const u32 as *const u8,
                p.add(8),
                core::mem::size_of::<u32>(),
            );
            i += 4;
        }
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
unsafe fn load_bytes_as_dwords(p: *const u8) -> __m128i {
    let v = core::ptr::read_unaligned(p as *const i32);
    _mm_cvtepu8_epi32(_mm_cvtsi32_si128(v))
}

#[target_feature(enable = "sse2,ssse3,sse4.1,sse4.2")]
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
    let col_vec = _mm_set1_epi32(col as i32);
    let low_byte = _mm_set1_epi32(0xff);
    let mut i = 0;
    while i + 4 <= dst.len() {
        let m = load_bytes_as_dwords(mask.as_ptr().add(i));
        let d = _mm_loadu_si128(dst.as_ptr().add(i) as *const __m128i);
        let r = _mm_or_si128(col_vec, _mm_max_epu32(m, _mm_and_si128(d, low_byte)));
        _mm_storeu_si128(dst.as_mut_ptr().add(i) as *mut __m128i, r);
        i += 4;
    }
    for j in i..dst.len() {
        dst[j] = col | (dst[j] & 0xff).max(mask[j] as u32);
    }
}

// }}}

// UTF-8 {{{

#[inline(always)]
unsafe fn output_plain_ascii(d: &mut UTF8Decoder, mut vec: __m128i, src_sz: usize) {
    let pos = d.output_len();
    let limit = pos + src_sz;
    let out = d.output_ptr();
    let mut p = pos;
    while p < limit {
        let unpacked = _mm_cvtepu8_epi32(vec);
        _mm_storeu_si128(out.add(p) as *mut __m128i, unpacked);
        vec = _mm_srli_si128::<4>(vec);
        p += 4;
    }
    d.set_output_len(pos + src_sz);
}

#[inline(always)]
unsafe fn output_unicode(
    d: &mut UTF8Decoder,
    mut output1: __m128i,
    mut output2: __m128i,
    mut output3: __m128i,
    num_codepoints: usize,
) {
    let pos = d.output_len();
    let limit = pos + num_codepoints;
    let out = d.output_ptr();
    let mut p = pos;
    while p < limit {
        let unpacked1 = _mm_cvtepu8_epi32(output1);
        let unpacked2 = _mm_slli_si128::<1>(_mm_cvtepu8_epi32(output2));
        let unpacked3 = _mm_slli_si128::<2>(_mm_cvtepu8_epi32(output3));
        let unpacked = _mm_or_si128(_mm_or_si128(unpacked1, unpacked2), unpacked3);
        _mm_storeu_si128(out.add(p) as *mut __m128i, unpacked);
        output1 = _mm_srli_si128::<4>(output1);
        output2 = _mm_srli_si128::<4>(output2);
        output3 = _mm_srli_si128::<4>(output3);
        p += 4;
    }
    d.set_output_len(pos + num_codepoints);
}

#[target_feature(enable = "sse2,ssse3,sse4.1,sse4.2")]
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
    let zero = _mm_setzero_si128();
    let one = set1_epi8(1);
    let two = set1_epi8(2);
    let three = set1_epi8(3);
    let four = set1_epi8(4);
    let numbered = _mm_set_epi8(15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0);
    let mut p = 0usize;
    let limit = src_len;
    let mut sentinel_found = false;
    let mut chunk_src_sz: usize;
    let mut prev_chunk_was_all_ascii = true;

    while p < limit && !sentinel_found {
        if prev_chunk_was_all_ascii {
            while limit - p >= 2 * LANES_BYTES {
                let v1 = _mm_loadu_si128(src_data.as_ptr().add(p) as *const __m128i);
                let v2 = _mm_loadu_si128(src_data.as_ptr().add(p + LANES_BYTES) as *const __m128i);
                let esc_or_non_ascii = _mm_or_si128(
                    _mm_or_si128(_mm_cmpeq_epi8(v1, esc_vec), _mm_cmpeq_epi8(v2, esc_vec)),
                    _mm_or_si128(v1, v2),
                );
                if _mm_movemask_epi8(esc_or_non_ascii) != 0 {
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
            _mm_loadu_si128(src_data.as_ptr().add(p) as *const __m128i)
        } else {
            let mut tmp = [0u8; LANES_BYTES];
            tmp[..chunk_src_sz].copy_from_slice(&src_data[p..p + chunk_src_sz]);
            _mm_loadu_si128(tmp.as_ptr() as *const __m128i)
        };
        let start_of_current_chunk = p;
        p += chunk_src_sz;

        let mut ascii_mask = _mm_movemask_epi8(vec) as u32;
        let mut esc_mask = _mm_movemask_epi8(_mm_cmpeq_epi8(vec, esc_vec)) as u32;
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
            let vec_signed = _mm_add_epi8(vec, state80);
            let start_two = _mm_cmplt_epi8(set1_epi8(0xc0 - 1 - 0x80), vec_signed);
            let mut state = set1_epi8(0x80);
            state = _mm_blendv_epi8(state, set1_epi8(0xc2), start_two);
            let start_three = _mm_cmplt_epi8(set1_epi8(0xe0 - 1 - 0x80), vec_signed);
            state = _mm_blendv_epi8(state, set1_epi8(0xe3), start_three);
            let start_four = _mm_cmplt_epi8(set1_epi8(0xf0 - 1 - 0x80), vec_signed);
            state = _mm_blendv_epi8(state, set1_epi8(0xf4), start_four);
            let mask = _mm_and_si128(state, set1_epi8(0xf8));
            let count = _mm_and_si128(state, set1_epi8(0x7));
            let count_subs1 = _mm_subs_epu8(count, one);
            let mut counts = _mm_add_epi8(count, shift_right_by_one_byte(count_subs1));
            counts = _mm_add_epi8(counts, shift_right_by_two_bytes(_mm_subs_epu8(counts, two)));
            if check_for_trailing_bytes
                && !is_zero(_mm_cmplt_epi8(
                    one,
                    _mm_and_si128(
                        counts,
                        _mm_cmpeq_epi8(numbered, _mm_set1_epi8((chunk_src_sz - 1) as i8)),
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
                ascii_mask ^ (_mm_movemask_epi8(_mm_cmpgt_epi8(counts, zero)) as u32);
            let mut chunk_is_invalid = set1_epi8(if ascii_sequence_count_mismatches != 0 {
                0xff
            } else {
                0x00
            });
            chunk_is_invalid = _mm_or_si128(
                chunk_is_invalid,
                _mm_and_si128(start_two, _mm_cmplt_epi8(vec, set1_epi8(0xc2))),
            );
            chunk_is_invalid = _mm_or_si128(
                chunk_is_invalid,
                _mm_and_si128(start_four, _mm_cmpgt_epi8(vec, set1_epi8(0xf4))),
            );
            chunk_is_invalid = _mm_or_si128(
                chunk_is_invalid,
                _mm_andnot_si128(
                    _mm_cmplt_epi8(vec, set1_epi8(0xc0)),
                    _mm_cmpgt_epi8(counts, count),
                ),
            );
            let prev_bytes = shift_right_by_one_byte(vec);
            let e0_first_follower = _mm_cmpeq_epi8(prev_bytes, set1_epi8(0xe0));
            chunk_is_invalid = _mm_or_si128(
                chunk_is_invalid,
                _mm_and_si128(
                    e0_first_follower,
                    _mm_cmplt_epi8(_mm_and_si128(e0_first_follower, vec), set1_epi8(0xa0)),
                ),
            );
            let ed_first_follower = _mm_cmpeq_epi8(prev_bytes, set1_epi8(0xed));
            chunk_is_invalid = _mm_or_si128(
                chunk_is_invalid,
                _mm_and_si128(
                    ed_first_follower,
                    _mm_cmpgt_epi8(_mm_and_si128(ed_first_follower, vec), set1_epi8(0x9f)),
                ),
            );
            let f0_first_follower = _mm_cmpeq_epi8(prev_bytes, set1_epi8(0xf0));
            chunk_is_invalid = _mm_or_si128(
                chunk_is_invalid,
                _mm_and_si128(
                    f0_first_follower,
                    _mm_cmplt_epi8(_mm_and_si128(f0_first_follower, vec), set1_epi8(0x90)),
                ),
            );
            let f4_first_follower = _mm_cmpeq_epi8(prev_bytes, set1_epi8(0xf4));
            chunk_is_invalid = _mm_or_si128(
                chunk_is_invalid,
                _mm_and_si128(
                    f4_first_follower,
                    _mm_cmpgt_epi8(_mm_and_si128(f4_first_follower, vec), set1_epi8(0x8f)),
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

            vec = _mm_andnot_si128(mask, vec);
            let count2_locations = _mm_cmpeq_epi8(counts, two);
            let count3_locations = _mm_cmpeq_epi8(counts, three);
            let vec_non_ascii = _mm_andnot_si128(_mm_cmpeq_epi8(counts, zero), vec);
            let mut output1 = _mm_blendv_epi8(
                vec,
                _mm_or_si128(
                    vec,
                    _mm_and_si128(
                        _mm_slli_epi16::<6>(shift_right_by_one_byte(vec_non_ascii)),
                        set1_epi8(0xc0),
                    ),
                ),
                _mm_cmpeq_epi8(counts, one),
            );
            let mut output2 = _mm_and_si128(vec, count2_locations);
            output2 = _mm_srli_epi32::<2>(output2);
            output2 = _mm_or_si128(
                output2,
                _mm_and_si128(
                    set1_epi8(0xf0),
                    _mm_slli_epi16::<4>(shift_right_by_one_byte(_mm_and_si128(
                        count3_locations,
                        vec_non_ascii,
                    ))),
                ),
            );
            output2 = _mm_and_si128(output2, count2_locations);
            output2 = shift_right_by_one_byte(output2);
            let mut output3 = _mm_and_si128(three, _mm_srli_epi32::<4>(vec));
            let count4_locations = _mm_cmpeq_epi8(counts, four);
            output3 = _mm_or_si128(
                output3,
                _mm_and_si128(
                    set1_epi8(0xfc),
                    _mm_slli_epi16::<2>(shift_right_by_one_byte(_mm_and_si128(
                        count4_locations,
                        vec_non_ascii,
                    ))),
                ),
            );
            output3 = _mm_and_si128(output3, count3_locations);
            output3 = shift_right_by_two_bytes(output3);

            let discarded_locations = _mm_cmpgt_epi8(counts, one);
            let mut shifts = _mm_and_si128(one, discarded_locations);
            shifts = _mm_add_epi8(shifts, shift_right_by_one_byte(shifts));
            shifts = _mm_add_epi8(shifts, shift_right_by_two_bytes(shifts));
            shifts = _mm_add_epi8(shifts, shift_right_by_four_bytes(shifts));
            shifts = _mm_add_epi8(shifts, shift_right_by_eight_bytes(shifts));
            shifts = _mm_andnot_si128(discarded_locations, shifts);
            #[inline(always)]
            unsafe fn move1(a: __m128i) -> __m128i {
                _mm_blendv_epi8(
                    a,
                    _mm_srli_si128::<1>(a),
                    _mm_srli_si128::<1>(_mm_slli_epi16::<7>(a)),
                )
            }
            #[inline(always)]
            unsafe fn move2(a: __m128i) -> __m128i {
                _mm_blendv_epi8(
                    a,
                    _mm_srli_si128::<2>(a),
                    _mm_srli_si128::<2>(_mm_slli_epi16::<6>(a)),
                )
            }
            #[inline(always)]
            unsafe fn move4(a: __m128i) -> __m128i {
                _mm_blendv_epi8(
                    a,
                    _mm_srli_si128::<4>(a),
                    _mm_srli_si128::<4>(_mm_slli_epi16::<5>(a)),
                )
            }
            #[inline(always)]
            unsafe fn move8(a: __m128i) -> __m128i {
                _mm_blendv_epi8(
                    a,
                    _mm_srli_si128::<8>(a),
                    _mm_srli_si128::<8>(_mm_slli_epi16::<4>(a)),
                )
            }
            shifts = move1(shifts);
            shifts = move2(shifts);
            shifts = move4(shifts);
            shifts = move8(shifts);
            let numbered_in_lane = numbered;
            shifts = _mm_add_epi8(shifts, numbered_in_lane);

            output1 = _mm_shuffle_epi8(output1, shifts);
            output2 = _mm_shuffle_epi8(output2, shifts);
            output3 = _mm_shuffle_epi8(output3, shifts);

            let mut kept_mask = !(_mm_movemask_epi8(discarded_locations) as u32);
            if chunk_src_sz < 32 {
                kept_mask &= low_bits(chunk_src_sz as u32);
            }
            let num_codepoints = kept_mask.count_ones() as usize;
            output_unicode(d, output1, output2, output3, num_codepoints);

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
