// SPDX-License-Identifier: GPL-3.0-or-later
//! Unit tests, including ports of the vectors from `kitty_tests/parser.py`.

use crate::scalar;
use crate::utf8::UTF8Decoder;
use crate::*;

fn levels() -> Vec<u32> {
    [0u32, 128, 256, 512]
        .into_iter()
        .filter(|&l| level_supported(l))
        .collect()
}

fn with_level<R>(level: u32, f: impl FnOnce() -> R) -> R {
    with_forced_level(level, f)
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn byte(&mut self) -> u8 {
        (self.next() >> 24) as u8
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() as usize) % n
    }
}

// {{{ find_either_of_two_bytes

#[test]
fn find_either_align_offsets_and_lengths() {
    let needles = [
        (b'<', b'>'),
        (b' ', b'b'),
        (b'<', b'a'),
        (b'<', b'b'),
        (b'c', b'>'),
        (0, 0),
        (0xff, 0x80),
    ];
    for &(a, b) in &needles {
        for off in 0..64usize {
            let mut hay = vec![b' '; off];
            hay.extend_from_slice(b"abc");
            let expected = scalar::find_either_of_two_bytes(&hay, a, b);
            for level in levels() {
                let actual = with_level(level, || find_either_of_two_bytes(&hay, a, b));
                assert_eq!(actual, expected, "offset={off} level={level}");
            }
        }
        for sz in [0usize, 16, 32, 64, 79, 128, 133] {
            for tail in [&b""[..], b"a", b"dsdfsfa", b"xa", b"bbb", b"bba", b"baa"] {
                let mut hay = vec![b' '; sz];
                hay.extend_from_slice(tail);
                let expected = scalar::find_either_of_two_bytes(&hay, a, b);
                for level in levels() {
                    let actual = with_level(level, || find_either_of_two_bytes(&hay, a, b));
                    assert_eq!(actual, expected, "sz={sz} tail={tail:?} level={level}");
                }
            }
        }
    }
}

#[test]
fn find_either_matches_every_position() {
    let mut rng = Rng(0x5eed);
    for len in 0..200usize {
        let hay: Vec<u8> = (0..len).map(|_| rng.byte()).collect();
        for &(a, b) in &[(0u8, 0u8), (0, 255), (hay.first().copied().unwrap_or(1), 0)] {
            let expected = hay.iter().position(|&x| x == a || x == b);
            for level in levels() {
                assert_eq!(
                    with_level(level, || find_either_of_two_bytes(&hay, a, b)),
                    expected,
                    "len={len} a={a} b={b} level={level}"
                );
            }
        }
    }
}

#[test]
fn find_either_does_not_match_padding() {
    // 0 must not be found in the zero-filled tail used by the SIMD kernels.
    for len in [1usize, 15, 16, 17, 31, 32, 33, 63, 64, 65] {
        let hay = vec![b'x'; len];
        for level in levels() {
            assert_eq!(
                with_level(level, || find_either_of_two_bytes(&hay, 0, 0)),
                None,
                "len={len} level={level}"
            );
        }
    }
}

// }}}

// {{{ xor_data64

fn scalar_xor(key: &[u8; 64], data: &[u8]) -> Vec<u8> {
    let mut out = data.to_vec();
    scalar::xor_data64(key, &mut out);
    out
}

#[test]
fn xor_data64_sizes_and_alignment() {
    let key: [u8; 64] = core::array::from_fn(|i| (i as u8).wrapping_mul(7).wrapping_add(101));
    let base: Vec<u8> = (0..256u32).map(|i| i as u8).collect();
    for &len in &[0usize, 1, 15, 16, 17, 63, 64, 65, 79, 127, 128, 129, 192] {
        for off in [0usize, 1, 7, 15, 16, 31, 63] {
            let mut storage = vec![0xAAu8; off + len + 64];
            storage[off..off + len].copy_from_slice(&base[..len]);
            let expected = scalar_xor(&key, &storage[off..off + len]);
            for level in levels() {
                let mut actual = storage.clone();
                with_level(level, || xor_data64(&key, &mut actual[off..off + len]));
                assert_eq!(
                    &actual[off..off + len],
                    &expected[..],
                    "len={len} off={off} level={level}"
                );
                assert!(actual[..off].iter().all(|&x| x == 0xAA));
                assert!(actual[off + len..].iter().all(|&x| x == 0xAA));
            }
        }
    }
}

#[test]
fn xor_data64_key_repetition() {
    let key: [u8; 64] = core::array::from_fn(|i| i as u8);
    let mut data = vec![0u8; 1000];
    for level in levels() {
        let mut d = data.clone();
        with_level(level, || xor_data64(&key, &mut d));
        for (i, b) in d.iter().enumerate() {
            assert_eq!(*b, key[i & 63], "i={i} level={level}");
        }
    }
    data.fill(0);
}

// }}}

// {{{ printable_ascii_run_length

#[test]
fn printable_ascii_vectors() {
    let bad_chars = [
        '\x1b',
        '\n',
        '\x00',
        '\x7f',
        '\x1f',
        'ü',
        '中',
        '😀',
        '\u{10ffff}',
        '\u{7f}',
    ];
    for prefix_len in [0usize, 1, 3, 4, 7, 8, 15, 16, 17, 31, 32, 33, 63, 64, 100] {
        let prefix: Vec<u32> = vec![b'a' as u32; prefix_len];
        for bad in bad_chars {
            for text in [
                prefix.clone(),
                [prefix.clone(), vec![bad as u32]].concat(),
                [prefix.clone(), vec![bad as u32], vec![b'x' as u32; 3]].concat(),
                [prefix.clone(), vec![bad as u32; 3], prefix.clone()].concat(),
            ] {
                let expected = scalar::printable_ascii_run_length(&text);
                for level in levels() {
                    assert_eq!(
                        with_level(level, || printable_ascii_run_length(&text)),
                        expected,
                        "prefix={prefix_len} bad={bad:?} level={level}"
                    );
                }
            }
        }
    }
}

#[test]
fn printable_ascii_high_u32() {
    for ch in [0x7fff_ffffu32, 0x8000_0000, 0xffff_ffff, 127, 126, 32, 31] {
        let text = vec![b'a' as u32; 37];
        let mut with_bad = text.clone();
        with_bad.push(ch);
        with_bad.extend_from_slice(&text);
        for level in levels() {
            assert_eq!(
                with_level(level, || printable_ascii_run_length(&with_bad)),
                scalar::printable_ascii_run_length(&with_bad),
                "ch={ch:#x} level={level}"
            );
        }
    }
}

// }}}

// {{{ pixel compositing

fn blend_straight_scalar(dst: &[u8], src: &[u8]) -> Vec<u8> {
    let mut d = dst.to_vec();
    scalar::blend_over_straight(&mut d, src);
    d
}

fn blend_opaque_scalar(dst: &[u8], bpp: u32, src: &[u8]) -> Vec<u8> {
    let mut d = dst.to_vec();
    scalar::blend_over_opaque(&mut d, bpp, src);
    d
}

fn mask_scalar(dst: &[u32], mask: &[u8], color: u32) -> Vec<u32> {
    let mut d = dst.to_vec();
    scalar::composite_alpha_mask(&mut d, mask, color);
    d
}

fn make_images(num_pixels: usize, seed: u64) -> (Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>) {
    let mut rng = Rng(seed);
    let mut src: Vec<u8> = (0..4 * num_pixels).map(|_| rng.byte()).collect();
    for i in 0..num_pixels {
        match i % 5 {
            0 => src[4 * i + 3] = 0,
            1 => src[4 * i + 3] = 255,
            _ => {}
        }
    }
    let dst4: Vec<u8> = (0..4 * num_pixels).map(|_| rng.byte()).collect();
    let dst3: Vec<u8> = (0..3 * num_pixels).map(|_| rng.byte()).collect();
    let mask: Vec<u8> = (0..num_pixels).map(|_| rng.byte()).collect();
    (src, dst4, dst3, mask)
}

#[test]
fn blend_straight_matches_scalar() {
    for &num_pixels in &[
        0usize, 1, 2, 3, 4, 5, 7, 8, 9, 15, 16, 17, 31, 32, 33, 63, 65, 255, 257,
    ] {
        let (src, dst4, _, _) = make_images(num_pixels, 0x1b7f5 + num_pixels as u64);
        let expected = blend_straight_scalar(&dst4, &src);
        for level in levels() {
            for off in [0usize, 1, 2, 3, 7, 13] {
                let mut storage = vec![0xEEu8; off + dst4.len() + 64];
                storage[off..off + dst4.len()].copy_from_slice(&dst4);
                with_level(level, || {
                    blend_over_straight(&mut storage[off..off + dst4.len()], &src)
                });
                assert_eq!(
                    &storage[off..off + dst4.len()],
                    &expected[..],
                    "num_pixels={num_pixels} off={off} level={level}"
                );
            }
        }
    }
}

#[test]
fn blend_straight_formula_edges() {
    // Both fully transparent: dst unchanged.
    let mut d = [10u8, 20, 30, 0];
    scalar::blend_over_straight(&mut d, &[1, 2, 3, 0]);
    assert_eq!(d, [10, 20, 30, 0]);
    // Fully opaque src replaces rgb and alpha.
    let mut d = [10u8, 20, 30, 7];
    scalar::blend_over_straight(&mut d, &[200, 100, 50, 255]);
    assert_eq!(d, [200, 100, 50, 255]);
    // Half-way value rounds to even, like lrintf under FE_TONEAREST.
    // alpha=1, dst_alpha=0 => denom=255, num=src_c*255 => exact result src_c.
    let mut d = [0u8, 0, 0, 0];
    scalar::blend_over_straight(&mut d, &[255, 1, 2, 1]);
    assert_eq!(d, [255, 1, 2, 1]);
    // Check the exact integer numerator/denominator for a mixed case.
    let mut d = [13u8, 200, 7, 100];
    let s = [250u8, 4, 255, 128];
    let mut expected = d;
    scalar::blend_over_straight(&mut expected, &s);
    scalar::blend_over_straight(&mut d, &s);
    assert_eq!(d, expected);
}

#[test]
fn blend_opaque_4bpp_matches_scalar() {
    for &num_pixels in &[0usize, 1, 2, 3, 4, 5, 8, 9, 15, 16, 17, 31, 32, 33, 65, 257] {
        let (src, dst4, _, _) = make_images(num_pixels, 0x99 + num_pixels as u64);
        let expected = blend_opaque_scalar(&dst4, 4, &src);
        for level in levels() {
            for off in [0usize, 1, 2, 3, 7, 13] {
                let mut storage = vec![0xDDu8; off + dst4.len() + 64];
                storage[off..off + dst4.len()].copy_from_slice(&dst4);
                with_level(level, || {
                    blend_over_opaque(&mut storage[off..off + dst4.len()], 4, &src)
                });
                assert_eq!(
                    &storage[off..off + dst4.len()],
                    &expected[..],
                    "num_pixels={num_pixels} off={off} level={level}"
                );
            }
        }
    }
}

#[test]
fn blend_opaque_3bpp_matches_scalar() {
    for &num_pixels in &[
        0usize, 1, 2, 3, 4, 5, 6, 7, 8, 9, 15, 16, 17, 63, 64, 65, 255, 257,
    ] {
        let (src, _, dst3, _) = make_images(num_pixels, 0x33 + num_pixels as u64);
        let expected = blend_opaque_scalar(&dst3, 3, &src);
        for level in levels() {
            for off in [0usize, 1, 2, 3, 7, 13] {
                let mut storage = vec![0xCCu8; off + dst3.len() + 64];
                storage[off..off + dst3.len()].copy_from_slice(&dst3);
                with_level(level, || {
                    blend_over_opaque(&mut storage[off..off + dst3.len()], 3, &src)
                });
                assert_eq!(
                    &storage[off..off + dst3.len()],
                    &expected[..],
                    "num_pixels={num_pixels} off={off} level={level}"
                );
            }
        }
    }
}

#[test]
fn composite_alpha_mask_matches_scalar() {
    for &num_pixels in &[
        0usize, 1, 2, 3, 4, 5, 7, 8, 9, 15, 16, 17, 31, 32, 33, 63, 64, 65, 257,
    ] {
        let (_, _, _, mask) = make_images(num_pixels, 0x44 + num_pixels as u64);
        let mut rng = Rng(0x77 + num_pixels as u64);
        let dst: Vec<u32> = (0..num_pixels).map(|_| rng.next() as u32).collect();
        for color in [0u32, 0x123456, 0xffffff, 0xabcdef] {
            let expected = mask_scalar(&dst, &mask, color);
            for level in levels() {
                for off in [0usize, 1, 4, 12, 36] {
                    let mut storage = vec![0x1234_5678u32; off + num_pixels + 16];
                    storage[off..off + num_pixels].copy_from_slice(&dst);
                    with_level(level, || {
                        composite_alpha_mask(&mut storage[off..off + num_pixels], &mask, color)
                    });
                    assert_eq!(
                        &storage[off..off + num_pixels],
                        &expected[..],
                        "px={num_pixels} off={off} color={color:#x} level={level}"
                    );
                }
            }
        }
    }
}

#[test]
fn composite_alpha_mask_rounding() {
    let dst = [0x11223344u32, 0x00000000, 0xffff_ffff];
    let mask = [0u8, 255, 3];
    let out = mask_scalar(&dst, &mask, 0x010203);
    assert_eq!(out[0], 0x01020344);
    assert_eq!(out[1], 0x010203ff);
    assert_eq!(out[2], 0x010203ff);
}

#[test]
fn pixel_formulas_independent_reference() {
    fn div255(x: u32) -> u32 {
        let y = x + 128;
        (y + (y >> 8)) >> 8
    }
    let mut rng = Rng(0xf00d);
    for &n in &[0usize, 1, 2, 3, 4, 5, 7, 8, 9, 15, 16, 17, 33, 64, 65] {
        let src: Vec<u8> = (0..4 * n).map(|_| rng.byte()).collect();
        let dst4: Vec<u8> = (0..4 * n).map(|_| rng.byte()).collect();
        let dst3: Vec<u8> = (0..3 * n).map(|_| rng.byte()).collect();

        let mut expected = dst4.clone();
        for i in 0..n {
            let a = src[4 * i + 3] as u32;
            let da = dst4[4 * i + 3] as u32;
            let inv = 255 - a;
            let den = a * 255 + da * inv;
            if den != 0 {
                for c in 0..3 {
                    let num =
                        (src[4 * i + c] as u32) * a * 255 + (dst4[4 * i + c] as u32) * da * inv;
                    expected[4 * i + c] = ((num as f32) / (den as f32)).round_ties_even() as u8;
                }
                expected[4 * i + 3] = div255(den) as u8;
            }
        }
        for level in levels() {
            let mut got = dst4.clone();
            with_level(level, || blend_over_straight(&mut got, &src));
            assert_eq!(got, expected, "straight n={n} level={level}");
        }

        let mut expected = dst4.clone();
        for i in 0..n {
            let a = src[4 * i + 3] as u32;
            let inv = 255 - a;
            for c in 0..3 {
                expected[4 * i + c] =
                    div255((src[4 * i + c] as u32) * a + (dst4[4 * i + c] as u32) * inv) as u8;
            }
            expected[4 * i + 3] = 255;
        }
        for level in levels() {
            let mut got = dst4.clone();
            with_level(level, || blend_over_opaque(&mut got, 4, &src));
            assert_eq!(got, expected, "opaque4 n={n} level={level}");
        }

        let mut expected = dst3.clone();
        for i in 0..n {
            let a = src[4 * i + 3] as u32;
            let inv = 255 - a;
            for c in 0..3 {
                expected[3 * i + c] =
                    div255((src[4 * i + c] as u32) * a + (dst3[3 * i + c] as u32) * inv) as u8;
            }
        }
        for level in levels() {
            let mut got = dst3.clone();
            with_level(level, || blend_over_opaque(&mut got, 3, &src));
            assert_eq!(got, expected, "opaque3 n={n} level={level}");
        }

        let mask: Vec<u8> = (0..n).map(|_| rng.byte()).collect();
        let dstp: Vec<u32> = (0..n).map(|_| rng.next() as u32).collect();
        let color = 0xabcdefu32;
        let col = (color << 8) & 0xffff_ff00;
        let expected: Vec<u32> = dstp
            .iter()
            .zip(&mask)
            .map(|(&d, &m)| col | (d & 0xff).max(m as u32))
            .collect();
        for level in levels() {
            let mut got = dstp.clone();
            with_level(level, || composite_alpha_mask(&mut got, &mask, color));
            assert_eq!(got, expected, "mask n={n} level={level}");
        }
    }
}

// }}}

// {{{ UTF-8

fn decode_parts(level: u32, parts: &[&[u8]]) -> (bool, Vec<u32>, usize) {
    with_level(level, || {
        let mut d = UTF8Decoder::new();
        let mut out = Vec::new();
        let mut consumed_sum = 0usize;
        let mut esc = false;
        for part in parts {
            // Mirrors the loop in the C test wrapper: the vector decoder may
            // stop early after decoding an invalid chunk, the caller resumes
            // at num_consumed until the part is exhausted or ESC is found.
            let mut off = 0usize;
            while off < part.len() {
                let found = d.decode_to_esc(&part[off..]);
                consumed_sum += d.num_consumed;
                esc |= found;
                out.extend_from_slice(d.decoded());
                off += d.num_consumed;
                if found || d.num_consumed == 0 {
                    break;
                }
            }
        }
        (esc, out, consumed_sum)
    })
}

fn decode_all(level: u32, input: &[u8]) -> Vec<u32> {
    decode_parts(level, &[input]).1
}

fn utf8_expected(s: &str) -> Vec<u32> {
    s.chars().map(|c| c as u32).collect()
}

fn check_utf8(input: &[u8], expected: &str) {
    let exp = utf8_expected(expected);
    for level in levels() {
        assert_eq!(
            decode_all(level, input),
            exp,
            "input={input:?} level={level}"
        );
    }
}

#[test]
fn utf8_reference_vectors() {
    // Ported from test_utf8_simd_decode in kitty_tests/parser.py.
    check_utf8(b"filler\xe3\x83\x8b\xe3\x83\x81", "fillerニチ");
    check_utf8(b"filler\xc2\x84\xc2\x85", "filler\u{84}\u{85}");
    check_utf8(b"filler\xf4\x8f\xbf\xbf", "filler\u{10ffff}");
    check_utf8(b"filler\xef\xbf\xbf", "filler\u{ffff}");
    check_utf8(b"filler\x00", "filler\0");
    check_utf8(b"filler\xf4\x8f\xbf\xbf", "filler\u{10ffff}");
    let vectors: &[(&[u8], &str)] = &[
        (b"abcd\xf51234", "abcd\u{fffd}1234"),
        (b"abcd\xff1234", "abcd\u{fffd}1234"),
        (b"\"\xbf\"", "\"\u{fffd}\""),
        (b"\"\x80\"", "\"\u{fffd}\""),
        (b"\"\x80\xbf\"", "\"\u{fffd}\u{fffd}\""),
        (b"\"\x80\xbf\x80\"", "\"\u{fffd}\u{fffd}\u{fffd}\""),
        (b"\"\xc0 \"", "\"\u{fffd} \""),
        (b"\"\xfe\"", "\"\u{fffd}\""),
        (b"\"\xff\"", "\"\u{fffd}\""),
        (b"\"\xff\xfe\"", "\"\u{fffd}\u{fffd}\""),
        (
            b"\"\xfe\xfe\xff\xff\"",
            "\"\u{fffd}\u{fffd}\u{fffd}\u{fffd}\"",
        ),
        (b"\"\xef\xbf\"", "\"\u{fffd}\""),
        (b"\"\xe0\xa0\"", "\"\u{fffd}\""),
        (b"\"\xf0\x9f\x98\"", "\"\u{fffd}\""),
        (b"\"\xef\x93\x94\x95\"", "\"\u{f4d4}\u{fffd}\""),
        (b"\"\xc2\"", "\"\u{fffd}\""),
        (b"\"\xe1\x28\xa1\"", "\"\u{fffd}(\u{fffd}\""),
        (b"\"\xc0\x80\"", "\"\u{fffd}\u{fffd}\""),
        (b"\"\xe0\x80\x80\"", "\"\u{fffd}\u{fffd}\u{fffd}\""),
        (
            b"\"\xf0\x80\x80\x80\"",
            "\"\u{fffd}\u{fffd}\u{fffd}\u{fffd}\"",
        ),
        (b"\"\xed\xa0\x80\"", "\"\u{fffd}\u{fffd}\u{fffd}\""),
        (b"\"\xed\xb0\x80\"", "\"\u{fffd}\u{fffd}\u{fffd}\""),
        (
            b"\"\xff\x80\x80\x80\"",
            "\"\u{fffd}\u{fffd}\u{fffd}\u{fffd}\"",
        ),
        (b"\"\xc1\x7f\"", "\"\u{fffd}\x7f\""),
        (b"\"\xc1\x80\"", "\"\u{fffd}\u{fffd}\""),
        (b"\"\xc2\x7f\"", "\"\u{fffd}\x7f\""),
        (b"\"\xc2\x80\"", "\"\u{80}\""),
        (b"\"\xc2\xbf\"", "\"\u{bf}\""),
        (b"\"\xdf\x80\"", "\"\u{7c0}\""),
        (b"\"\xdf\xbf\"", "\"\u{7ff}\""),
        (b"\"\xe0\x9f\x80\"", "\"\u{fffd}\u{fffd}\u{fffd}\""),
        (b"\"\xe0\xa0\x7f\"", "\"\u{fffd}\x7f\""),
        (b"\"\xe0\xa0\x80\"", "\"\u{800}\""),
        (b"\"\xe0\xa0\xbf\"", "\"\u{83f}\""),
        (b"\"\xe1\x7f\x80\"", "\"\u{fffd}\x7f\u{fffd}\""),
        (b"\"\xe1\x80\x7f\"", "\"\u{fffd}\x7f\""),
        (b"\"\xe1\x80\x80\"", "\"\u{1000}\""),
        (b"\"\xed\x9f\xbf\"", "\"\u{d7ff}\""),
        (b"\"\xee\x80\x80\"", "\"\u{e000}\""),
        (b"\"\xef\xbf\xbf\"", "\"\u{ffff}\""),
        (
            b"\"\xf0\x8f\x80\x80\"",
            "\"\u{fffd}\u{fffd}\u{fffd}\u{fffd}\"",
        ),
        (b"\"\xf0\x90\x80\x80\"", "\"\u{10000}\""),
        (b"\"\xf0\x90\xc2\x80\"", "\"\u{fffd}\u{80}\""),
        (b"\"\xf1\x80\x80\x80\"", "\"\u{40000}\""),
        (b"\"\xf3\xbf\xbf\xbf\"", "\"\u{fffff}\""),
        (b"\"\xf4\x8f\xbf\xbf\"", "\"\u{10ffff}\""),
        (
            b"\"\xf4\x90\x80\x80\"",
            "\"\u{fffd}\u{fffd}\u{fffd}\u{fffd}\"",
        ),
        (
            b"\"\xf5\x80\x80\x80\"",
            "\"\u{fffd}\u{fffd}\u{fffd}\u{fffd}\"",
        ),
    ];
    for &(input, expected) in vectors {
        check_utf8(input, expected);
    }
}

#[test]
fn utf8_incremental_every_split() {
    let inputs: &[&[u8]] = &[
        b"2:\xce\xb13:",
        b"2:\xce\xb13:\xe2\x89\xa44:\xf0\x9f\x98\xb8|",
        "abc≤\u{1f638}".as_bytes(),
        b"abc\x1bd1234",
        b"\x80",
        b"\xc0\x80",
        b"\xed\xa0\x80",
        b"a\xf0\x9f\x98",
        b"\xf0\x9f\x98\xb8\xf0\x9f\x98",
        b"\xef\x93\x94\x95",
        b"x\xe1\x28\xa1y",
    ];
    for &input in inputs {
        let no_esc = !input.contains(&0x1b);
        let expected_full = decode_all(0, input);
        for level in levels() {
            for cut in 0..=input.len() {
                let (a, b) = input.split_at(cut);
                let (_, scalar_split, _) = decode_parts(0, &[a, b]);
                let (_, out, _) = decode_parts(level, &[a, b]);
                assert_eq!(out, scalar_split, "cut={cut} input={input:?} level={level}");
                if no_esc {
                    assert_eq!(
                        out, expected_full,
                        "cut={cut} input={input:?} level={level}"
                    );
                }
            }
        }
    }
}

#[test]
fn utf8_partial_sequences_across_calls() {
    let prefixes: &[&[u8]] = &[b"abcd", "\u{1f638}".as_bytes()];
    let suffixes: &[&[u8]] = &[b"1234", "\u{1f638}".as_bytes()];
    let split_seqs: &[&[u8]] = &[
        b"\xf0\x9f",
        b"\xf0\x9f\x9b",
        b"\xf0",
        b"\xc3",
        b"\xe2",
        b"\xe2\x89",
    ];
    for prefix in prefixes {
        for suffix in suffixes {
            for seq in split_seqs {
                let mut first = prefix.to_vec();
                first.extend_from_slice(seq);
                let mut second = Vec::from(&b"\x98\xb8"[..]);
                second.extend_from_slice(suffix);
                let mut all = first.clone();
                all.extend_from_slice(&second);
                let expected = decode_all(0, &all);
                for level in levels() {
                    let (_, out, _) = decode_parts(level, &[&first, &second]);
                    assert_eq!(out, expected, "prefix={prefix:?} seq={seq:?} level={level}");
                }
            }
        }
    }
}

#[test]
fn utf8_esc_sentinel_positions() {
    for &esc_pos in &[
        0usize, 1, 14, 15, 16, 17, 30, 31, 32, 33, 47, 48, 63, 64, 65,
    ] {
        let mut buf = vec![b'x'; 80];
        buf[esc_pos] = 0x1b;
        buf.extend_from_slice(b"after");
        for level in levels() {
            let (esc, out, consumed) = decode_parts(level, &[&buf]);
            assert!(esc, "esc_pos={esc_pos} level={level}");
            assert_eq!(out, vec![b'x' as u32; esc_pos], "esc_pos={esc_pos}");
            assert_eq!(consumed, esc_pos + 1);
        }
        // Incomplete sequence cut off by the sentinel: scalar emits U+FFFD.
        let mut buf = vec![b'y'; esc_pos];
        buf.extend_from_slice(b"\xe2\x89\x1b");
        buf.extend_from_slice(b"z");
        let expected = decode_all(0, &buf);
        for level in levels() {
            let (esc, out, _) = decode_parts(level, &[&buf]);
            assert!(esc);
            assert_eq!(out, expected, "esc_pos={esc_pos} level={level}");
        }
    }
}

#[test]
fn utf8_lane_and_chunk_boundaries() {
    let multibyte: &[&[u8]] = &[
        b"\xc2\xb5",
        b"\xce\xb1",
        b"\xe2\x89\xa4",
        b"\xf0\x9f\x98\xb8",
    ];
    for &mb in multibyte {
        for filler in 0..70usize {
            let mut a = vec![b'a'; filler];
            a.extend_from_slice(mb);
            a.extend_from_slice(&[b'b'; 40]);
            let mut b = vec![b'a'; filler];
            for _ in 0..3 {
                b.extend_from_slice(mb);
            }
            b.extend_from_slice(&[b'b'; 40]);
            for input in [&a[..], &b[..]] {
                let expected = decode_all(0, input);
                for level in levels() {
                    assert_eq!(
                        decode_all(level, input),
                        expected,
                        "filler={filler} mb={mb:?} level={level}"
                    );
                }
            }
        }
    }
}

#[test]
fn utf8_invalid_at_chunk_boundaries() {
    let bad_seqs: &[&[u8]] = &[
        b"\x80",
        b"\xbf\xbf",
        b"\xc0\x80",
        b"\xc1\xbf",
        b"\xe0\x80\x80",
        b"\xe0\x9f\x80",
        b"\xed\xa0\x80",
        b"\xf0\x8f\x80\x80",
        b"\xf4\x90\x80\x80",
        b"\xf5\x81\x82\x83",
        b"\xfe",
        b"\xff",
        b"\xc2",
        b"\xe2\x89",
        b"\xf0\x9f\x98",
    ];
    for &bad in bad_seqs {
        for &pos in &[0usize, 13, 14, 15, 16, 17, 29, 30, 31, 32, 33, 62, 63, 64] {
            let mut a = vec![b'k'; pos];
            a.extend_from_slice(bad);
            a.extend_from_slice(&[b'm'; 9]);
            let mut b = vec![b'k'; pos];
            b.extend_from_slice(bad);
            for input in [&a[..], &b[..]] {
                let expected = decode_all(0, input);
                for level in levels() {
                    assert_eq!(
                        decode_all(level, input),
                        expected,
                        "bad={bad:?} pos={pos} level={level}"
                    );
                }
            }
        }
    }
}

#[test]
fn utf8_dense_and_exact_chunk_sizes() {
    let cases: Vec<Vec<u8>> = vec![
        vec![0x80; 32],
        vec![0xbf; 64],
        "\u{3b1}".repeat(32).into_bytes(),
        "a\u{3b1}".repeat(24).into_bytes(),
        "\u{2264}".repeat(22).into_bytes(),
        "\u{1f638}".repeat(17).into_bytes(),
        format!("a{}", "\u{1f638}".repeat(13)).into_bytes(),
        "\u{3b1}\u{2264}\u{1f638}".repeat(11).into_bytes(),
        vec![b'a'; 31],
        vec![b'a'; 32],
        vec![b'a'; 33],
        vec![b'a'; 64],
        vec![b'a'; 65],
        vec![b'a'; 128],
        vec![b'a'; 129],
    ];
    for input in &cases {
        let expected = decode_all(0, input);
        for level in levels() {
            assert_eq!(decode_all(level, input), expected, "level={level}");
        }
    }
    for n in [16usize, 32, 64, 128] {
        for pattern in [
            &b"\xc0\xefh\x7f"[..],
            b"\xc2\xefh\x7f",
            b"\xc0\xefhq",
            b"\xe0\xa0\xc2\xe2\x89",
        ] {
            let mut a = vec![b'k'; n - 4];
            a.extend_from_slice(pattern);
            let mut b = a.clone();
            b.extend_from_slice(b"more data");
            for input in [&a[..], &b[..]] {
                let expected = decode_all(0, input);
                for level in levels() {
                    assert_eq!(decode_all(level, input), expected, "level={level}");
                }
            }
        }
    }
}

#[test]
fn utf8_randomized_fuzz() {
    let interesting = [
        0x1bu8, 0x00, 0x7f, 0x80, 0xbf, 0xc0, 0xc1, 0xc2, 0xdf, 0xe0, 0xed, 0xee, 0xef, 0xf0, 0xf4,
        0xf5, 0xff,
    ];
    let valid_chars = [
        "a",
        "\u{b5}",
        "\u{3b1}",
        "\u{2264}",
        "\u{1f638}",
        "\u{7ff}",
        "\u{800}",
        "\u{d7ff}",
        "\u{e000}",
        "\u{ffff}",
        "\u{10000}",
        "\u{10ffff}",
    ];
    let mut rng = Rng(0x1bad_f00d);
    for _ in 0..1000 {
        let mut data = Vec::new();
        for _ in 0..(1 + rng.below(80)) {
            let r = rng.next() % 100;
            if r < 50 {
                data.extend_from_slice(valid_chars[rng.below(valid_chars.len())].as_bytes());
            } else if r < 80 {
                data.push(interesting[rng.below(interesting.len())]);
            } else {
                data.push(rng.byte());
            }
        }
        let expected = decode_all(0, &data);
        for level in levels() {
            assert_eq!(
                decode_all(level, &data),
                expected,
                "data={data:?} level={level}"
            );
            if data.len() > 2 {
                let cut = 1 + rng.below(data.len() - 1);
                let (_, out, _) = decode_parts(level, &[&data[..cut], &data[cut..]]);
                let (_, expected_split, _) = decode_parts(0, &[&data[..cut], &data[cut..]]);
                assert_eq!(out, expected_split, "split cut={cut} level={level}");
            }
        }
    }
}

#[test]
fn utf8_reset_and_state_carry() {
    let mut d = UTF8Decoder::new();
    d.decode_to_esc(b"\xf0\x9f");
    assert_ne!(d.cur, UTF8_ACCEPT);
    assert!(d.decoded().is_empty());
    d.decode_to_esc(b"\x98\xb8!");
    assert_eq!(d.decoded(), utf8_expected("\u{1f638}!"));
    d.reset();
    assert_eq!(d.cur, UTF8_ACCEPT);
    assert_eq!(d.prev, UTF8_ACCEPT);
    assert_eq!(d.codep, 0);
    d.decode_to_esc(b"\xf0\x9f\x98\xb8");
    assert_eq!(d.decoded(), utf8_expected("\u{1f638}"));
}

#[test]
fn utf8_num_consumed_semantics() {
    let mut d = UTF8Decoder::new();
    assert!(d.decode_to_esc(b"ab\x1bcd"));
    assert_eq!(d.num_consumed, 3);
    assert_eq!(d.decoded(), utf8_expected("ab"));
    // No sentinel: all bytes consumed.
    assert!(!d.decode_to_esc(b"xyz"));
    assert_eq!(d.num_consumed, 3);
    // ESC as first byte consumes just the ESC and emits nothing.
    assert!(d.decode_to_esc(b"\x1b[31m"));
    assert_eq!(d.num_consumed, 1);
    assert!(d.decoded().is_empty());
}

// }}}

// {{{ dispatch

#[test]
fn dispatch_and_forced_levels() {
    assert!([0, 128, 256, 512].contains(&simd_level()));
    assert!([0, 128, 256, 512].contains(&max_simd_level()));
    assert!(level_supported(0));
    for level in [128u32, 256, 512] {
        assert_eq!(level_supported(level), level <= max_simd_level());
        let observed = with_forced_level(level, simd_level);
        if level_supported(level) {
            assert_eq!(observed, level);
        } else {
            assert_eq!(observed, max_simd_level());
        }
    }
    let data = b"abc<def";
    let expected = scalar::find_either_of_two_bytes(data, b'<', b'>');
    for level in levels() {
        let actual = with_forced_level(level, || find_either_of_two_bytes(data, b'<', b'>'));
        assert_eq!(actual, expected);
    }
}

#[test]
#[should_panic(expected = "level must be 0, 128, 256 or 512")]
fn forced_level_rejects_invalid() {
    let _ = with_forced_level(1, || 0);
}

#[test]
#[should_panic(expected = "dst_bpp must be 3 or 4")]
fn blend_opaque_rejects_bad_bpp() {
    let mut dst = [0u8; 4];
    blend_over_opaque(&mut dst, 2, &[0u8; 4]);
}

#[test]
#[should_panic(expected = "multiple of 4")]
fn blend_straight_rejects_bad_src() {
    let mut dst = [0u8; 5];
    blend_over_straight(&mut dst, &[0u8; 5]);
}

#[test]
#[should_panic(expected = "one 32-bit pixel per mask byte")]
fn composite_rejects_bad_length() {
    let mut dst = [0u32; 2];
    composite_alpha_mask(&mut dst, &[0u8; 3], 0);
}

// }}}
