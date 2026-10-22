// SPDX-License-Identifier: GPL-3.0-or-later
//! AVX-512 (F, BW, VL, VBMI2) implementations, mirroring
//! `simd-string-512.c`.
//!
//! All AVX-512 intrinsics used here are stable on Rust 1.98, including
//! `_mm512_maskz_compress_epi8` (VBMI2), so no fallback to AVX2 was needed.

use core::arch::x86_64::*;

use crate::utf8::{decode_all_scalar, decode_to_accept_scalar, UTF8Decoder, UTF8_ACCEPT};

const KEY_SIZE: usize = 64;

#[inline(always)]
unsafe fn set1_epi8(x: u8) -> __m512i {
    _mm512_set1_epi8(x as i8)
}

// Shift towards higher indices, keeping the lane-crossing behaviour of the C
// helper at the 512-bit level.
#[inline(always)]
unsafe fn shift_right_by_one_byte(a: __m512i) -> __m512i {
    _mm512_alignr_epi8::<15>(a, _mm512_alignr_epi64::<6>(a, _mm512_setzero_si512()))
}

#[inline(always)]
unsafe fn shift_right_by_two_bytes(a: __m512i) -> __m512i {
    _mm512_alignr_epi8::<14>(a, _mm512_alignr_epi64::<6>(a, _mm512_setzero_si512()))
}

#[inline(always)]
fn low_bits64(n: u32) -> u64 {
    if n >= 64 {
        u64::MAX
    } else {
        (1u64 << n) - 1
    }
}

#[target_feature(enable = "avx512f,avx512bw,avx512vl,avx512vbmi2")]
/// # Safety
///
/// The CPU must support every instruction set enabled on this function (see
/// the module-level documentation). The safe public wrappers in the crate root
/// guarantee this with runtime feature detection.
pub unsafe fn find_either_of_two_bytes(haystack: &[u8], a: u8, b: u8) -> Option<usize> {
    let a_vec = set1_epi8(a);
    let b_vec = set1_epi8(b);
    let mut i = 0;
    while i < haystack.len() {
        let chunk_bits = low_bits64((haystack.len() - i).min(64) as u32);
        let chunk = _mm512_maskz_loadu_epi8(chunk_bits, haystack.as_ptr().add(i) as *const i8);
        let matches = chunk_bits
            & (_mm512_cmpeq_epi8_mask(chunk, a_vec) | _mm512_cmpeq_epi8_mask(chunk, b_vec));
        if matches != 0 {
            return Some(i + matches.trailing_zeros() as usize);
        }
        i += 64;
    }
    None
}

#[target_feature(enable = "avx512f,avx512bw,avx512vl,avx512vbmi2")]
/// # Safety
///
/// The CPU must support every instruction set enabled on this function (see
/// the module-level documentation). The safe public wrappers in the crate root
/// guarantee this with runtime feature detection.
pub unsafe fn xor_data64(key: &[u8; KEY_SIZE], data: &mut [u8]) {
    let key_vec = _mm512_loadu_si512(key.as_ptr() as *const __m512i);
    let mut i = 0;
    while i + 64 <= data.len() {
        let p = data.as_mut_ptr().add(i) as *mut __m512i;
        _mm512_storeu_si512(p, _mm512_xor_si512(_mm512_loadu_si512(p), key_vec));
        i += 64;
    }
    if i < data.len() {
        let tail_bits = low_bits64((data.len() - i) as u32);
        let p = data.as_mut_ptr().add(i);
        _mm512_mask_storeu_epi8(
            p as *mut i8,
            tail_bits,
            _mm512_xor_si512(_mm512_maskz_loadu_epi8(tail_bits, p as *const i8), key_vec),
        );
    }
}

#[target_feature(enable = "avx512f,avx512bw,avx512vl,avx512vbmi2")]
/// # Safety
///
/// The CPU must support every instruction set enabled on this function (see
/// the module-level documentation). The safe public wrappers in the crate root
/// guarantee this with runtime feature detection.
pub unsafe fn printable_ascii_run_length(chars: &[u32]) -> usize {
    let lower = _mm512_set1_epi32(32);
    let upper = _mm512_set1_epi32(126);
    let mut i = 0;
    while i < chars.len() {
        let tail = (chars.len() - i).min(16) as u32;
        let chunk_bits = (low_bits64(tail) & 0xffff) as u16;
        let chunk = _mm512_maskz_loadu_epi32(chunk_bits, chars.as_ptr().add(i) as *const i32);
        let non_printable = chunk_bits
            & (_mm512_cmplt_epu32_mask(chunk, lower) | _mm512_cmpgt_epu32_mask(chunk, upper));
        if non_printable != 0 {
            return i + non_printable.trailing_zeros() as usize;
        }
        i += 16;
    }
    chars.len()
}

// Pixel compositing {{{

#[inline(always)]
unsafe fn div255_epu16(x: __m512i) -> __m512i {
    let y = _mm512_add_epi16(x, _mm512_set1_epi16(128));
    _mm512_srli_epi16::<8>(_mm512_add_epi16(y, _mm512_srli_epi16::<8>(y)))
}

#[inline(always)]
unsafe fn alpha_broadcast_pattern() -> __m512i {
    _mm512_broadcast_i32x4(_mm_set_epi8(
        15, 15, 15, 15, 11, 11, 11, 11, 7, 7, 7, 7, 3, 3, 3, 3,
    ))
}

#[inline(always)]
unsafe fn blend_opaque_pixels(under: __m512i, over: __m512i) -> __m512i {
    let zero = _mm512_setzero_si512();
    let alpha = _mm512_shuffle_epi8(over, alpha_broadcast_pattern());
    let inv_alpha = _mm512_xor_si512(alpha, _mm512_set1_epi64(-1));
    let lo = div255_epu16(_mm512_add_epi16(
        _mm512_mullo_epi16(
            _mm512_unpacklo_epi8(over, zero),
            _mm512_unpacklo_epi8(alpha, zero),
        ),
        _mm512_mullo_epi16(
            _mm512_unpacklo_epi8(under, zero),
            _mm512_unpacklo_epi8(inv_alpha, zero),
        ),
    ));
    let hi = div255_epu16(_mm512_add_epi16(
        _mm512_mullo_epi16(
            _mm512_unpackhi_epi8(over, zero),
            _mm512_unpackhi_epi8(alpha, zero),
        ),
        _mm512_mullo_epi16(
            _mm512_unpackhi_epi8(under, zero),
            _mm512_unpackhi_epi8(inv_alpha, zero),
        ),
    ));
    _mm512_or_si512(
        _mm512_packus_epi16(lo, hi),
        _mm512_set1_epi32(0xff00_0000u32 as i32),
    )
}

#[target_feature(enable = "avx512f,avx512bw,avx512vl,avx512vbmi2")]
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
    let col = _mm512_set1_epi32((color_rgb.wrapping_shl(8) & 0xffff_ff00) as i32);
    let low_byte = _mm512_set1_epi32(0xff);
    let mut i = 0;
    while i < dst.len() {
        let px = (dst.len() - i).min(16) as u32;
        let k = (low_bits64(px) & 0xffff) as u16;
        let m = _mm512_cvtepu8_epi32(_mm_maskz_loadu_epi8(k, mask.as_ptr().add(i) as *const i8));
        let d = _mm512_maskz_loadu_epi32(k, dst.as_ptr().add(i) as *const i32);
        _mm512_mask_storeu_epi32(
            dst.as_mut_ptr().add(i) as *mut i32,
            k,
            _mm512_or_si512(col, _mm512_max_epu32(m, _mm512_and_si512(d, low_byte))),
        );
        i += 16;
    }
}

#[target_feature(enable = "avx512f,avx512bw,avx512vl,avx512vbmi2")]
/// # Safety
///
/// The CPU must support every instruction set enabled on this function (see
/// the module-level documentation). The safe public wrappers in the crate root
/// guarantee this with runtime feature detection.
pub unsafe fn blend_over_opaque(dst: &mut [u8], dst_bpp: u32, src: &[u8]) {
    crate::scalar_checks::assert_blend_opaque_lengths(dst, dst_bpp, src);
    if dst_bpp != 4 {
        // The byte expand/compress operations needed here are slower than the
        // 128-bit shuffle kernel on the CPUs tested, matching the C code.
        return crate::x86::sse2::blend_over_opaque(dst, 3, src);
    }
    let num_pixels = src.len() / 4;
    let mut i = 0;
    while i < num_pixels {
        let px = (num_pixels - i).min(16) as u32;
        let k = low_bits64(4 * px);
        let s = _mm512_maskz_loadu_epi8(k, src.as_ptr().add(4 * i) as *const i8);
        let d = _mm512_maskz_loadu_epi8(k, dst.as_ptr().add(4 * i) as *const i8);
        _mm512_mask_storeu_epi8(
            dst.as_mut_ptr().add(4 * i) as *mut i8,
            k,
            blend_opaque_pixels(d, s),
        );
        i += 16;
    }
}

#[target_feature(enable = "avx512f,avx512bw,avx512vl,avx512vbmi2")]
/// # Safety
///
/// The CPU must support every instruction set enabled on this function (see
/// the module-level documentation). The safe public wrappers in the crate root
/// guarantee this with runtime feature detection.
pub unsafe fn blend_over_straight(dst: &mut [u8], src: &[u8]) {
    crate::scalar_checks::assert_blend_straight_lengths(dst, src);
    let num_pixels = src.len() / 4;
    let c255 = _mm512_set1_epi32(255);
    let c128 = _mm512_set1_epi32(128);
    let zero = _mm512_setzero_si512();
    let mut i = 0;
    while i < num_pixels {
        let px = (num_pixels - i).min(4) as u32;
        let k = (low_bits64(4 * px) & 0xffff) as u16;
        let s = _mm512_cvtepu8_epi32(_mm_maskz_loadu_epi8(
            k,
            src.as_ptr().add(4 * i) as *const i8,
        ));
        let d = _mm512_cvtepu8_epi32(_mm_maskz_loadu_epi8(
            k,
            dst.as_ptr().add(4 * i) as *const i8,
        ));
        let alpha = _mm512_shuffle_epi32::<0xff>(s);
        let dst_alpha = _mm512_shuffle_epi32::<0xff>(d);
        let inv_alpha = _mm512_sub_epi32(c255, alpha);
        let denom = _mm512_add_epi32(
            _mm512_mullo_epi32(alpha, c255),
            _mm512_mullo_epi32(dst_alpha, inv_alpha),
        );
        let num = _mm512_add_epi32(
            _mm512_mullo_epi32(_mm512_mullo_epi32(s, alpha), c255),
            _mm512_mullo_epi32(_mm512_mullo_epi32(d, dst_alpha), inv_alpha),
        );
        let mut r = _mm512_cvtps_epi32(_mm512_div_ps(
            _mm512_cvtepi32_ps(num),
            _mm512_cvtepi32_ps(denom),
        ));
        let y = _mm512_add_epi32(denom, c128);
        let out_alpha = _mm512_srli_epi32::<8>(_mm512_add_epi32(y, _mm512_srli_epi32::<8>(y)));
        r = _mm512_mask_mov_epi32(r, 0x8888, out_alpha);
        r = _mm512_mask_mov_epi32(r, _mm512_cmpeq_epi32_mask(denom, zero), d);
        _mm_mask_storeu_epi8(
            dst.as_mut_ptr().add(4 * i) as *mut i8,
            k,
            _mm512_cvtepi32_epi8(r),
        );
        i += 4;
    }
}

// }}}

// UTF-8 {{{

#[inline(always)]
unsafe fn output_plain_ascii_from_memory(d: &mut UTF8Decoder, src: *const u8, src_sz: usize) {
    let base = d.output_len();
    let out = d.output_ptr();
    let mut i = 0;
    while i < src_sz {
        let v = _mm512_cvtepu8_epi32(_mm_loadu_si128(src.add(i) as *const __m128i));
        _mm512_storeu_si512(out.add(base + i) as *mut __m512i, v);
        i += 16;
    }
    d.set_output_len(base + src_sz);
}

#[inline(always)]
unsafe fn output_plain_ascii(d: &mut UTF8Decoder, vec: __m512i, src_sz: usize) {
    let base = d.output_len();
    let out = d.output_ptr();
    _mm512_storeu_si512(
        out.add(base) as *mut __m512i,
        _mm512_cvtepu8_epi32(_mm512_castsi512_si128(vec)),
    );
    if src_sz > 16 {
        _mm512_storeu_si512(
            out.add(base + 16) as *mut __m512i,
            _mm512_cvtepu8_epi32(_mm512_extracti32x4_epi32::<1>(vec)),
        );
        if src_sz > 32 {
            _mm512_storeu_si512(
                out.add(base + 32) as *mut __m512i,
                _mm512_cvtepu8_epi32(_mm512_extracti32x4_epi32::<2>(vec)),
            );
            if src_sz > 48 {
                _mm512_storeu_si512(
                    out.add(base + 48) as *mut __m512i,
                    _mm512_cvtepu8_epi32(_mm512_extracti32x4_epi32::<3>(vec)),
                );
            }
        }
    }
    d.set_output_len(base + src_sz);
}

#[inline(always)]
unsafe fn output_unicode(
    d: &mut UTF8Decoder,
    mut output1: __m512i,
    mut output2: __m512i,
    mut output3: __m512i,
    num_codepoints: usize,
) {
    let base = d.output_len();
    let out = d.output_ptr();
    let zero = _mm512_setzero_si512();
    let mut i = 0;
    while i < num_codepoints {
        let unpacked1 = _mm512_cvtepu8_epi32(_mm512_castsi512_si128(output1));
        let unpacked2 =
            _mm512_slli_epi32::<8>(_mm512_cvtepu8_epi32(_mm512_castsi512_si128(output2)));
        let unpacked3 =
            _mm512_slli_epi32::<16>(_mm512_cvtepu8_epi32(_mm512_castsi512_si128(output3)));
        _mm512_storeu_si512(
            out.add(base + i) as *mut __m512i,
            _mm512_or_si512(_mm512_or_si512(unpacked1, unpacked2), unpacked3),
        );
        output1 = _mm512_alignr_epi64::<2>(zero, output1);
        output2 = _mm512_alignr_epi64::<2>(zero, output2);
        output3 = _mm512_alignr_epi64::<2>(zero, output3);
        i += 16;
    }
    d.set_output_len(base + num_codepoints);
}

#[target_feature(enable = "avx512f,avx512bw,avx512vl,avx512vbmi2")]
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
    let zero = _mm512_setzero_si512();
    let one = set1_epi8(1);
    let two = set1_epi8(2);
    let three = set1_epi8(3);
    let mut p = 0usize;
    let limit = src_len;
    let mut sentinel_found = false;
    let mut chunk_src_sz: usize;
    let mut prev_chunk_was_all_ascii = true;

    while p < limit && !sentinel_found {
        if prev_chunk_was_all_ascii {
            while limit - p >= 128 {
                let v1 = _mm512_loadu_si512(src_data.as_ptr().add(p) as *const __m512i);
                let v2 = _mm512_loadu_si512(src_data.as_ptr().add(p + 64) as *const __m512i);
                let bad = _mm512_cmpeq_epi8_mask(v1, esc_vec)
                    | _mm512_cmpeq_epi8_mask(v2, esc_vec)
                    | _mm512_movepi8_mask(_mm512_or_si512(v1, v2));
                if bad != 0 {
                    break;
                }
                output_plain_ascii_from_memory(d, src_data.as_ptr().add(p), 128);
                d.num_consumed += 128;
                p += 128;
            }
            if p >= limit {
                break;
            }
        }
        chunk_src_sz = (limit - p).min(64);
        let chunk_bits = low_bits64(chunk_src_sz as u32);
        let mut vec = _mm512_maskz_loadu_epi8(chunk_bits, src_data.as_ptr().add(p) as *const i8);
        let start_of_current_chunk = p;
        p += chunk_src_sz;

        let mut ascii_mask = _mm512_movepi8_mask(vec);
        let esc_mask = _mm512_cmpeq_epi8_mask(vec, esc_vec);
        if esc_mask != 0 {
            sentinel_found = true;
            chunk_src_sz = esc_mask.trailing_zeros() as usize;
            let kept_bits = low_bits64(chunk_src_sz as u32);
            ascii_mask &= kept_bits;
            vec = _mm512_maskz_mov_epi8(kept_bits, vec);
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

            let starts_two = _mm512_cmpge_epu8_mask(vec, set1_epi8(0xc0));
            let starts_three = _mm512_cmpge_epu8_mask(vec, set1_epi8(0xe0));
            let starts_four = _mm512_cmpge_epu8_mask(vec, set1_epi8(0xf0));
            let mut count = _mm512_maskz_mov_epi8(starts_two, two);
            count = _mm512_mask_add_epi8(count, starts_three, count, one);
            count = _mm512_mask_add_epi8(count, starts_four, count, one);
            let mut strip = set1_epi8(0x80);
            strip = _mm512_mask_mov_epi8(strip, starts_two, set1_epi8(0xc0));
            strip = _mm512_mask_mov_epi8(strip, starts_three, set1_epi8(0xe0));
            strip = _mm512_mask_mov_epi8(strip, starts_four, set1_epi8(0xf0));

            let count_subs1 = _mm512_subs_epu8(count, one);
            let mut counts = _mm512_add_epi8(count, shift_right_by_one_byte(count_subs1));
            counts = _mm512_add_epi8(
                counts,
                shift_right_by_two_bytes(_mm512_subs_epu8(counts, two)),
            );

            let discarded_locations = _mm512_cmpgt_epu8_mask(counts, one);
            if check_for_trailing_bytes && ((discarded_locations >> (chunk_src_sz - 1)) & 1) != 0 {
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
                    let kept_bits = low_bits64(chunk_src_sz as u32);
                    ascii_mask &= kept_bits;
                    vec = _mm512_maskz_mov_epi8(kept_bits, vec);
                }
                continue;
            }

            let mut chunk_is_invalid = ascii_mask ^ _mm512_cmpgt_epu8_mask(counts, zero);
            chunk_is_invalid |= _mm512_mask_cmplt_epu8_mask(starts_two, vec, set1_epi8(0xc2));
            chunk_is_invalid |= _mm512_cmpgt_epu8_mask(vec, set1_epi8(0xf4));
            chunk_is_invalid |= _mm512_mask_cmpgt_epu8_mask(starts_two, counts, count);
            let prev_bytes = shift_right_by_one_byte(vec);
            chunk_is_invalid |= _mm512_mask_cmplt_epu8_mask(
                _mm512_cmpeq_epi8_mask(prev_bytes, set1_epi8(0xe0)),
                vec,
                set1_epi8(0xa0),
            );
            chunk_is_invalid |= _mm512_mask_cmpgt_epu8_mask(
                _mm512_cmpeq_epi8_mask(prev_bytes, set1_epi8(0xed)),
                vec,
                set1_epi8(0x9f),
            );
            chunk_is_invalid |= _mm512_mask_cmplt_epu8_mask(
                _mm512_cmpeq_epi8_mask(prev_bytes, set1_epi8(0xf0)),
                vec,
                set1_epi8(0x90),
            );
            chunk_is_invalid |= _mm512_mask_cmpgt_epu8_mask(
                _mm512_cmpeq_epi8_mask(prev_bytes, set1_epi8(0xf4)),
                vec,
                set1_epi8(0x8f),
            );
            if chunk_is_invalid != 0 {
                decode_all_scalar(
                    d,
                    &src_data[start_of_current_chunk
                        ..start_of_current_chunk + chunk_src_sz + num_of_trailing_bytes],
                );
                d.num_consumed += num_of_trailing_bytes;
                stop = true;
                break;
            }

            vec = _mm512_andnot_si512(strip, vec);
            let count1_locations = _mm512_cmpeq_epu8_mask(counts, one);
            let count2_locations = _mm512_cmpeq_epu8_mask(counts, two);
            let count3_locations = _mm512_cmpeq_epu8_mask(counts, three);
            let count4_locations = _mm512_cmpeq_epu8_mask(counts, set1_epi8(4));
            let vec_non_ascii = _mm512_maskz_mov_epi8(_mm512_cmpgt_epu8_mask(counts, zero), vec);
            let prev_non_ascii = shift_right_by_one_byte(vec_non_ascii);

            let low_two_bits_of_starter =
                _mm512_and_si512(_mm512_slli_epi16::<6>(prev_non_ascii), set1_epi8(0xc0));
            let output1 = _mm512_mask_mov_epi8(
                vec,
                count1_locations,
                _mm512_or_si512(vec, low_two_bits_of_starter),
            );

            let mut output2 = _mm512_srli_epi32::<2>(_mm512_maskz_mov_epi8(count2_locations, vec));
            output2 = _mm512_or_si512(
                output2,
                _mm512_and_si512(
                    set1_epi8(0xf0),
                    _mm512_slli_epi16::<4>(_mm512_maskz_mov_epi8(
                        count3_locations << 1,
                        prev_non_ascii,
                    )),
                ),
            );
            let output2 = shift_right_by_one_byte(_mm512_maskz_mov_epi8(count2_locations, output2));

            let mut output3 = _mm512_and_si512(three, _mm512_srli_epi32::<4>(vec));
            output3 = _mm512_or_si512(
                output3,
                _mm512_and_si512(
                    set1_epi8(0xfc),
                    _mm512_slli_epi16::<2>(_mm512_maskz_mov_epi8(
                        count4_locations << 1,
                        prev_non_ascii,
                    )),
                ),
            );
            let output3 =
                shift_right_by_two_bytes(_mm512_maskz_mov_epi8(count3_locations, output3));

            let kept_locations = !discarded_locations
                & (if chunk_src_sz == 64 {
                    u64::MAX
                } else {
                    low_bits64(chunk_src_sz as u32)
                });
            let output1 = _mm512_maskz_compress_epi8(kept_locations, output1);
            let output2 = _mm512_maskz_compress_epi8(kept_locations, output2);
            let output3 = _mm512_maskz_compress_epi8(kept_locations, output3);
            let num_codepoints = kept_locations.count_ones() as usize;
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
