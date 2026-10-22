// SPDX-License-Identifier: GPL-3.0-or-later
// Ported from kitty/wcswidth.h and kitty/wcswidth.c,
// Copyright (C) 2020 Kovid Goyal <kovid at kovidgoyal.net>

//! ANSI-aware `wcswidth`, ported 1:1 from `kitty/wcswidth.c`.
//!
//! The parser skips CSI sequences (`ESC [ ... final`) and ST-terminated
//! strings (`ESC P`, `ESC ]`, `ESC X`, `ESC ^`, `ESC _` ... `BEL` or
//! `ESC \`), and applies the FE0F/FE0E presentation-selector and SpacingMark
//! cell-widening adjustments from the C implementation.
//!
//! # Fast paths
//!
//! [`wcswidth_std`] and [`wcswidth_string`] collapse runs of at least two
//! printable ASCII characters into one accumulated width. Every such character
//! has width 1, grapheme break property `None`, and no combining/SpacingMark
//! behaviour; after two steps the grapheme segmentation state is a fixed
//! `GBP_None` state. The only way the first character can attach to the
//! previous cell is a prior `AtStart`/`Prepend` state with `can_combine` set,
//! which is handled explicitly. CSI and ST bodies are scanned without a call to
//! [`WCSState::wcswidth_step`] for each byte. Both shortcuts preserve the exact
//! observable state (`prev_ch`, `prev_width`, `can_combine`, `seg`) at every
//! boundary; see the randomized/exhaustive differential tests below.

use kitty_core::CharType;

use crate::char_props::{
    char_props_for, grapheme_segmentation_step, wcwidth_std, CharProps, GraphemeBreakProperty,
    GraphemeSegmentationResult,
};

const BEL: CharType = 0x07;
const ESC: CharType = 0x1b;
const ESC_CSI: CharType = b'[' as CharType;
const ESC_DCS: CharType = b'P' as CharType;
const ESC_OSC: CharType = b']' as CharType;
const ESC_SOS: CharType = b'X' as CharType;
const ESC_PM: CharType = b'^' as CharType;
const ESC_APC: CharType = b'_' as CharType;
const BACKSLASH: CharType = b'\\' as CharType;
const VS15: CharType = 0xfe0e;
const VS16: CharType = 0xfe0f;

// Two/three character escapes that are entirely consumed by the ESC state.
const ESC_D: CharType = b'D' as CharType;
const ESC_E: CharType = b'E' as CharType;
const ESC_H: CharType = b'H' as CharType;
const ESC_M: CharType = b'M' as CharType;
const ESC_N: CharType = b'N' as CharType;
const ESC_O: CharType = b'O' as CharType;
const ESC_Z: CharType = b'Z' as CharType;
const ESC_6: CharType = b'6' as CharType;
const ESC_7: CharType = b'7' as CharType;
const ESC_8: CharType = b'8' as CharType;
const ESC_9: CharType = b'9' as CharType;
const ESC_EQUALS: CharType = b'=' as CharType;
const ESC_GREATER: CharType = b'>' as CharType;
const ESC_F: CharType = b'F' as CharType;
const ESC_C_LOWER: CharType = b'c' as CharType;
const ESC_L_LOWER: CharType = b'l' as CharType;
const ESC_M_LOWER: CharType = b'm' as CharType;
const ESC_N_LOWER: CharType = b'n' as CharType;
const ESC_O_LOWER: CharType = b'o' as CharType;
const ESC_PIPE: CharType = b'|' as CharType;
const ESC_RBRACE: CharType = b'}' as CharType;
const ESC_TILDE: CharType = b'~' as CharType;

/// `WCSParserState` from wcswidth.h.
#[allow(non_camel_case_types)]
#[allow(clippy::upper_case_acronyms)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum WCSParserState {
    #[default]
    NORMAL,
    IN_ESC,
    IN_CSI,
    IN_ST_TERMINATED,
}

/// `WCSState` from wcswidth.h.
#[allow(clippy::upper_case_acronyms)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WCSState {
    pub prev_ch: CharType,
    pub prev_width: i32,
    pub parser_state: WCSParserState,
    pub can_combine: bool,
    pub seg: GraphemeSegmentationResult,
}

impl WCSState {
    /// `initialize_wcs_state()`: zero everything, equivalent to
    /// `zero_at_ptr(state)` in C.
    #[inline]
    pub const fn new() -> Self {
        Self {
            prev_ch: 0,
            prev_width: 0,
            parser_state: WCSParserState::NORMAL,
            can_combine: false,
            seg: GraphemeSegmentationResult::new(),
        }
    }

    /// The `NORMAL` arm of `wcswidth_step()`, split out so the hot loop can
    /// call it without matching on the parser state again.
    #[inline(always)]
    fn normal_step(&mut self, ch: CharType) -> i32 {
        let mut ans = 0;
        let cp: CharProps = char_props_for(ch);
        self.seg = grapheme_segmentation_step(self.seg, cp);
        if self.seg.add_to_current_cell() && self.can_combine {
            match ch {
                VS16 => {
                    if char_props_for(self.prev_ch).is_emoji_presentation_base()
                        && self.prev_width == 1
                    {
                        ans = 1;
                        self.prev_width = 2;
                    } else {
                        self.prev_width = 0;
                    }
                }
                VS15 => {
                    if char_props_for(self.prev_ch).is_emoji_presentation_base()
                        && self.prev_width == 2
                    {
                        ans = -1;
                        self.prev_width = 1;
                    } else {
                        self.prev_width = 0;
                    }
                }
                _ => {
                    // Thai/Lao SARA AM: a SpacingMark with non-zero width
                    // widens a narrow base cell.
                    if cp.grapheme_break() == GraphemeBreakProperty::GBP_SpacingMark
                        && wcwidth_std(cp) > 0
                        && self.prev_width == 1
                    {
                        ans = 1;
                        self.prev_width = 2;
                    }
                }
            }
        } else {
            let width = wcwidth_std(cp);
            match width {
                -1 | 0 => {
                    self.prev_width = 0;
                    if ch == ESC {
                        self.parser_state = WCSParserState::IN_ESC;
                    }
                }
                2 => self.prev_width = 2,
                _ => self.prev_width = 1,
            }
            ans = self.prev_width;
            self.can_combine = true;
        }
        ans
    }

    /// `wcswidth_step()` from wcswidth.c. Returns the width delta for `ch`.
    #[inline(always)]
    pub fn wcswidth_step(&mut self, ch: CharType) -> i32 {
        let mut ans = 0;
        match self.parser_state {
            WCSParserState::IN_CSI => {
                self.prev_width = 0;
                if (0x40..=0x7e).contains(&ch) {
                    self.parser_state = WCSParserState::NORMAL;
                    self.can_combine = false;
                }
            }
            WCSParserState::IN_ST_TERMINATED => {
                self.prev_width = 0;
                if ch == BEL || (ch == BACKSLASH && self.prev_ch == ESC) {
                    self.parser_state = WCSParserState::NORMAL;
                    self.can_combine = false;
                }
            }
            WCSParserState::NORMAL => ans = self.normal_step(ch),
            WCSParserState::IN_ESC => match ch {
                ESC_CSI => self.parser_state = WCSParserState::IN_CSI,
                ESC_DCS | ESC_OSC | ESC_SOS | ESC_PM | ESC_APC => {
                    self.parser_state = WCSParserState::IN_ST_TERMINATED;
                }
                ESC_D | ESC_E | ESC_H | ESC_M | ESC_N | ESC_O | ESC_Z | ESC_6 | ESC_7 | ESC_8
                | ESC_9 | ESC_EQUALS | ESC_GREATER | ESC_F | ESC_C_LOWER | ESC_L_LOWER
                | ESC_M_LOWER | ESC_N_LOWER | ESC_O_LOWER | ESC_PIPE | ESC_RBRACE | ESC_TILDE => {}
                _ => {
                    *self = Self::new();
                    return self.wcswidth_step(ch);
                }
            },
        }
        self.prev_ch = ch;
        ans
    }
}

/// Free-function form of [`WCSState::wcswidth_step()`], mirroring the C API.
#[inline]
pub fn wcswidth_step(state: &mut WCSState, ch: CharType) -> i32 {
    state.wcswidth_step(ch)
}

/// `wcswidth_std()`: width of a full slice of code points.
///
/// The sum is folded with wrapping arithmetic to match C's unsigned `size_t`
/// accumulator (a negative step, such as the FE0E adjustment, subtracts one).
#[inline]
pub fn wcswidth_std(chars: &[CharType]) -> u32 {
    wcswidth_impl::<false>(chars)
}

/// `wcswidth_string()`: width of a NUL-terminated sequence.
///
/// The C function takes a `char_type *` and stops at the first NUL. Rust
/// callers pass the slice containing the terminator (as is common when
/// mirroring the C API); bytes after the NUL are ignored.
#[inline]
pub fn wcswidth_string(chars: &[CharType]) -> u32 {
    wcswidth_impl::<true>(chars)
}

#[inline]
const fn is_printable_ascii(ch: CharType) -> bool {
    ch.wrapping_sub(32) < 95
}

/// State after stepping over an ASCII printable character (`GBP_None`, no
/// ICB/emoji flags, `add_to_current_cell == false`).
const ASCII_SEG: GraphemeSegmentationResult =
    GraphemeSegmentationResult::from_bits((GraphemeBreakProperty::GBP_None as u16) << 7);

/// Length of the printable-ASCII prefix of `chars`.
///
/// Scans eight code points per branch so long runs auto-vectorize, while short
/// runs (the common case in ANSI-heavy input) pay only the scalar tail.
#[inline]
fn printable_ascii_run_length(chars: &[CharType]) -> usize {
    const LANES: usize = 8;
    let mut n = 0;
    // No early exit inside a block so LLVM can vectorize the eight range
    // checks. Anything shorter falls through to the scalar tail.
    while n + LANES <= chars.len() {
        let mut all = true;
        for k in 0..LANES {
            all &= is_printable_ascii(chars[n + k]);
        }
        if !all {
            break;
        }
        n += LANES;
    }
    while n < chars.len() && is_printable_ascii(chars[n]) {
        n += 1;
    }
    n
}

#[inline]
fn wcswidth_impl<const NUL_TERMINATED: bool>(chars: &[CharType]) -> u32 {
    let mut state = WCSState::new();
    let mut ans = 0u32;
    let mut i = 0;
    'chars: while i < chars.len() {
        let ch = chars[i];
        if NUL_TERMINATED && ch == 0 {
            break;
        }

        if state.parser_state == WCSParserState::NORMAL {
            // Every printable ASCII character has width 1, is not a combining
            // mark/SpacingMark, and has grapheme break property None. After the
            // first step over such a character the segmentation state has
            // grapheme_break == None, and after the second it is exactly
            // ASCII_SEG (the first step can still carry the previous state's
            // emoji_modifier_sequence flag). For a run of at least two chars
            // every subsequent char therefore takes the width branch, giving
            // an exact shortcut:
            //   - the first char attaches only when the previous state says
            //     "at start" or "prepend" and can_combine is set, in which
            //     case it contributes 0 and leaves prev_width alone;
            //   - every remaining char contributes 1 and sets prev_width = 1.
            // A one-char run is deliberately left to wcswidth_step().
            if is_printable_ascii(ch) && i + 1 < chars.len() && is_printable_ascii(chars[i + 1]) {
                let run = printable_ascii_run_length(&chars[i..]);
                if run >= 2 {
                    let gb = state.seg.grapheme_break();
                    let attaches = state.can_combine
                        && matches!(
                            gb,
                            GraphemeBreakProperty::GBP_AtStart | GraphemeBreakProperty::GBP_Prepend
                        );
                    let contribution = if attaches { run - 1 } else { run };
                    ans = ans.wrapping_add(contribution as u32);
                    state.prev_ch = chars[i + run - 1];
                    state.prev_width = 1;
                    state.can_combine = true;
                    state.seg = ASCII_SEG;
                    i += run;
                    continue;
                }
            }
            ans = ans.wrapping_add(state.normal_step(ch) as u32);
            state.prev_ch = ch;
            i += 1;
            continue;
        } else if state.parser_state == WCSParserState::IN_CSI {
            // Skip the CSI body in a tight loop. C sets prev_width = 0 and
            // prev_ch for every char; only the values after the last consumed
            // char are observable.
            let mut j = i;
            while j < chars.len() {
                let c = chars[j];
                if NUL_TERMINATED && c == 0 {
                    break 'chars;
                }
                if (0x40..=0x7e).contains(&c) {
                    state.prev_width = 0;
                    state.prev_ch = c;
                    state.parser_state = WCSParserState::NORMAL;
                    state.can_combine = false;
                    j += 1;
                    break;
                }
                j += 1;
            }
            if j == chars.len() {
                state.prev_width = 0;
                state.prev_ch = chars[j - 1];
            }
            i = j;
            continue;
        } else if state.parser_state == WCSParserState::IN_ST_TERMINATED {
            // Same idea for ST-terminated strings: BEL, or backslash preceded
            // by ESC, ends the string.
            let mut j = i;
            let mut prev = state.prev_ch;
            while j < chars.len() {
                let c = chars[j];
                if NUL_TERMINATED && c == 0 {
                    break 'chars;
                }
                if c == BEL || (c == BACKSLASH && prev == ESC) {
                    state.prev_width = 0;
                    state.prev_ch = c;
                    state.parser_state = WCSParserState::NORMAL;
                    state.can_combine = false;
                    j += 1;
                    break;
                }
                prev = c;
                j += 1;
            }
            if j == chars.len() {
                state.prev_width = 0;
                state.prev_ch = chars[j - 1];
            }
            i = j;
            continue;
        }

        ans = ans.wrapping_add(state.wcswidth_step(ch) as u32);
        i += 1;
    }
    ans
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars(text: &str) -> Vec<CharType> {
        text.chars().map(|c| c as CharType).collect()
    }

    fn wcs(text: &str) -> u32 {
        wcswidth_std(&chars(text))
    }

    fn width(c: char) -> i32 {
        wcwidth_std(char_props_for(c as CharType))
    }

    // The vectors below are copied from kitty_tests/datatypes.py:test_utils.
    #[test]
    fn data_type_vectors_1() {
        assert_eq!(wcs("\u{9c}"), 0);
        assert_eq!(wcs("a\u{1b}[2mb"), 2);
        assert_eq!(wcs("\u{1b}a\u{1b}[2mb"), 2);
        assert_eq!(wcs("a\u{1b}]8;id=moo;https://foo\u{1b}\\a"), 2);
        assert_eq!(wcs("a\u{1b}x"), 2);
    }

    #[test]
    fn data_type_vectors_single_widths() {
        let got: Vec<i32> = "a1\0コニチ ✔".chars().map(width).collect();
        assert_eq!(got, vec![1, 1, 0, 2, 2, 2, 1, 1]);
    }

    #[test]
    fn data_type_vectors_emoji_selectors() {
        assert_eq!(wcs("\u{2716}\u{2716}\u{fe0f}\u{1f337}"), 5);
        assert_eq!(wcs("\u{25b6}\u{fe0f}"), 2);
        assert_eq!(wcs("\u{1f610}\u{fe0e}"), 1);
    }

    #[test]
    fn data_type_vectors_regional_indicators() {
        assert_eq!(wcs("\u{1f1e6}a"), 3);
        assert_eq!(wcs("\u{1f1e6}a\u{1f1e8}a"), 6);
        assert_eq!(wcs("\u{1f1e6}\u{1f1e8}a"), 3);
        assert_eq!(wcs("\u{1f1e6}\u{1f1e8}\u{1f1e6}"), 4);
        let got: Vec<i32> = "\u{1f1ee}\u{1f1f3}".chars().map(width).collect();
        assert_eq!(got, vec![2, 2]);
        assert_eq!(wcs("\u{1f1ee}\u{1f1f3}"), 2);
    }

    #[test]
    fn data_type_vectors_thai_lao_spacing_mark() {
        assert_eq!(wcs("จำ"), 2);
        assert_eq!(wcs("ำ"), 1);
        assert_eq!(wcs("กิ"), 1);
        assert_eq!(wcs("จำำ"), 2);
        assert_eq!(wcs("ກຳ"), 2);
    }

    #[test]
    fn data_type_vectors_soft_hyphen_zwj() {
        assert_eq!(wcs("a\u{ad}b"), 2);
    }

    #[test]
    fn csi_sequences_are_skipped() {
        assert_eq!(wcs("\u{1b}[31mred\u{1b}[0m"), 3);
        assert_eq!(wcs("\u{1b}[?25l"), 0);
        // A CSI final byte is 0x40..=0x7e; '[' itself starts the CSI state.
        let mut s = WCSState::new();
        assert_eq!(s.wcswidth_step(ESC), 0);
        assert_eq!(s.parser_state, WCSParserState::IN_ESC);
        assert_eq!(s.wcswidth_step(b'[' as CharType), 0);
        assert_eq!(s.parser_state, WCSParserState::IN_CSI);
        assert_eq!(s.wcswidth_step(b'1' as CharType), 0);
        assert_eq!(s.parser_state, WCSParserState::IN_CSI);
        assert_eq!(s.wcswidth_step(b'm' as CharType), 0);
        assert_eq!(s.parser_state, WCSParserState::NORMAL);
        assert!(!s.can_combine);
    }

    #[test]
    fn st_terminated_sequences_are_skipped() {
        assert_eq!(wcs("\u{1b}]0;title\u{7}x"), 1);
        assert_eq!(wcs("\u{1b}]0;title\u{1b}\\x"), 1);
        assert_eq!(wcs("\u{1b}Pdata\u{1b}\\x"), 1);
        assert_eq!(wcs("\u{1b}_apc\u{1b}\\x"), 1);
        assert_eq!(wcs("\u{1b}^pm\u{1b}\\x"), 1);
        assert_eq!(wcs("\u{1b}Xsos\u{1b}\\x"), 1);
        // LF inside ST does not terminate it.
        assert_eq!(wcs("\u{1b}]0;a\nb\u{7}x"), 1);
    }

    #[test]
    fn st_termination_requires_esc_before_backslash() {
        let mut s = WCSState::new();
        s.wcswidth_step(ESC);
        s.wcswidth_step(ESC_OSC);
        assert_eq!(s.parser_state, WCSParserState::IN_ST_TERMINATED);
        s.wcswidth_step(BACKSLASH);
        assert_eq!(s.parser_state, WCSParserState::IN_ST_TERMINATED);
        s.wcswidth_step(ESC);
        s.wcswidth_step(BACKSLASH);
        assert_eq!(s.parser_state, WCSParserState::NORMAL);
        assert!(!s.can_combine);
    }

    #[test]
    fn unknown_escape_is_reparsed_after_reset() {
        assert_eq!(wcs("a\u{1b}x"), 2);
        let mut s = WCSState::new();
        s.wcswidth_step(b'a' as CharType);
        s.wcswidth_step(ESC);
        assert_eq!(s.parser_state, WCSParserState::IN_ESC);
        // 'x' is not a known escape, so the whole state is zeroed and 'x' is
        // processed as a normal character.
        assert_eq!(s.wcswidth_step(b'x' as CharType), 1);
        assert_eq!(s.parser_state, WCSParserState::NORMAL);
        assert_eq!(s.prev_ch, b'x' as CharType);
        assert_eq!(s.prev_width, 1);
        assert!(s.can_combine);
    }

    #[test]
    fn two_char_escapes_consume_their_byte() {
        for &esc in b"DEHMNOZ6789=>Fclmno|}~" {
            assert_eq!(
                wcs(&format!("\u{1b}{}x", esc as char)),
                1,
                "ESC {}",
                esc as char
            );
        }
    }

    #[test]
    fn fe0f_and_fe0e_adjust_previous_width() {
        // Base with width 1 + VS16 -> width 2.
        assert_eq!(wcs("\u{25b6}\u{fe0f}"), 2);
        // Base with width 2 + VS16 keeps width 2 (no extra column).
        assert_eq!(wcs("\u{1f337}\u{fe0f}"), 2);
        // Base with width 2 + VS15 -> width 1.
        assert_eq!(wcs("\u{1f610}\u{fe0e}"), 1);
        // VS16 after a non-emoji-presentation base drops prev_width.
        assert_eq!(wcs("a\u{fe0f}b"), 2);
    }

    #[test]
    fn spacing_mark_only_widens_narrow_cells() {
        assert_eq!(wcs("จำ"), 2); // base width 1, SARA AM widens to 2
        assert_eq!(wcs("ำ"), 1); // standalone SARA AM has width 1
        assert_eq!(wcs("コำ"), 2); // prev cell already wide: no widening
    }

    #[test]
    fn nul_terminated_string_stops_at_nul() {
        let data = chars("ab\0cd");
        assert_eq!(wcswidth_string(&data), 2);
        assert_eq!(wcswidth_std(&data), 4);
        assert_eq!(wcswidth_string(&[0]), 0);
        assert_eq!(wcswidth_string(&[]), 0);
    }

    #[test]
    fn control_characters_have_zero_width() {
        assert_eq!(wcs("a\tb"), 2);
        assert_eq!(wcs("a\nb"), 2);
        assert_eq!(wcs("\u{7f}"), 0);
    }

    fn slow(chars: &[CharType]) -> u32 {
        let mut state = WCSState::new();
        let mut ans = 0u32;
        for &ch in chars {
            ans = ans.wrapping_add(state.wcswidth_step(ch) as u32);
        }
        ans
    }

    fn slow_string(chars: &[CharType]) -> u32 {
        let mut state = WCSState::new();
        let mut ans = 0u32;
        for &ch in chars {
            if ch == 0 {
                break;
            }
            ans = ans.wrapping_add(state.wcswidth_step(ch) as u32);
        }
        ans
    }

    fn check_fast_paths(chars: &[CharType]) {
        assert_eq!(
            wcswidth_std(chars),
            slow(chars),
            "wcswidth_std mismatch for {chars:?}"
        );
        assert_eq!(
            wcswidth_string(chars),
            slow_string(chars),
            "wcswidth_string mismatch for {chars:?}"
        );
        // Prefixes exercise state at every run/escape boundary, not just the
        // final accumulated width.
        for end in 1..=chars.len() {
            let prefix = &chars[..end];
            assert_eq!(
                wcswidth_std(prefix),
                slow(prefix),
                "prefix mismatch at {end} for {chars:?}"
            );
            assert_eq!(
                wcswidth_string(prefix),
                slow_string(prefix),
                "NUL-string prefix mismatch at {end} for {chars:?}"
            );
        }
    }

    #[test]
    fn fast_paths_match_reference_on_targeted_sequences() {
        let cases = [
            "The quick brown fox 0123456789 ",
            "a\x1b[31mred\x1b[0m界\n",
            "\x1b]8;id=moo;https://foo\x1b\\a",
            "\x1bPdata\x1b\\x",
            "\x1b_apc\x07x",
            "\u{25b6}\u{fe0f}\u{1f610}\u{fe0e}",
            "จำำກຳกิ",
            "\u{1f1e6}\u{1f1e8}\u{1f1e6}",
            "e\u{301}\u{302}a",
            "\u{0600}abcdef",
            "abc\u{0600}def",
            "a\0bc",
            "\x1b[38;2;255;128;0mabcdefghijklmnopqrstuvwxyz\x1b[0m",
        ];
        for case in cases {
            let chars: Vec<CharType> = case.chars().map(|c| c as CharType).collect();
            check_fast_paths(&chars);
        }
    }

    #[test]
    fn fast_paths_match_reference_exhaustively_for_short_inputs() {
        let alphabet: Vec<CharType> = [
            'a' as CharType,
            'b' as CharType,
            ' ' as CharType,
            ESC,
            b'[' as CharType,
            b'm' as CharType,
            b'\\' as CharType,
            BEL,
            0,
            0x0301,
            0x0600,
            0x0e33,
            0x200d,
            0x1f1e6,
            0xfe0f,
            0xfe0e,
        ]
        .to_vec();
        let mut buf = Vec::with_capacity(5);
        fn visit(
            alphabet: &[CharType],
            depth: usize,
            buf: &mut Vec<CharType>,
            check: &mut impl FnMut(&[CharType]),
        ) {
            check(buf);
            if depth == 0 {
                return;
            }
            for &ch in alphabet {
                buf.push(ch);
                visit(alphabet, depth - 1, buf, check);
                buf.pop();
            }
        }
        visit(&alphabet, 4, &mut buf, &mut check_fast_paths);
    }

    #[test]
    fn fast_paths_match_reference_on_random_inputs() {
        const ALPHABET: &[CharType] = &[
            'a' as CharType,
            'Z' as CharType,
            ' ' as CharType,
            '~' as CharType,
            '0' as CharType,
            ESC,
            b'[' as CharType,
            b';' as CharType,
            b'm' as CharType,
            b'P' as CharType,
            b']' as CharType,
            b'\\' as CharType,
            BEL,
            0,
            0x07,
            0x0a,
            0x0d,
            0x7f,
            0x0301,
            0x0600,
            0x0e33,
            0x0eb3,
            0x200d,
            0x1f1e6,
            0x1f1e8,
            0x1f469,
            0x1f337,
            0x25b6,
            0x754c,
            0xfe0e,
            0xfe0f,
        ];
        let mut rng = 0x4d59_5df4_d0f3_3173u64;
        let mut next = || {
            rng = rng
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (rng >> 33) as usize
        };
        for _ in 0..400 {
            let len = next() % 160;
            let mut chars = Vec::with_capacity(len);
            for _ in 0..len {
                let r = next();
                let ch = if r % 5 == 0 {
                    (next() % 0x110000) as CharType
                } else {
                    ALPHABET[next() % ALPHABET.len()]
                };
                chars.push(ch);
            }
            check_fast_paths(&chars);
        }
    }

    #[test]
    fn matches_the_c_wcswidth_reference_hashes() {
        // Produced by compiling kitty/char-props.c + kitty/wcswidth.c with
        // gcc -O3 and running the same 400 deterministic random strings and
        // every prefix through both implementations. The state hash covers
        // prev_ch, prev_width, parser_state, can_combine and seg.val.
        const C_STD: u64 = 11_277_171_046_263_327_895;
        const C_STR: u64 = 9_202_096_353_269_906_736;
        const C_STATE: u64 = 6_467_578_230_997_928_839;
        const C_PREFIX: u64 = 8_411_208_983_303_807_444;
        const C_PSTATE: u64 = 7_805_757_256_308_952_708;
        const OFF: u64 = 1_469_598_103_934_665_603;
        const PRIME: u64 = 1_099_511_628_211;

        #[inline]
        fn mix(h: &mut u64, v: u64) {
            *h ^= v;
            *h = h.wrapping_mul(PRIME);
        }
        fn mix_state(h: &mut u64, st: &WCSState) {
            mix(h, st.prev_ch as u64);
            mix(h, st.prev_width as i64 as u64);
            mix(h, st.parser_state as u64);
            mix(h, st.can_combine as u64);
            mix(h, st.seg.bits() as u64);
        }

        const ALPHABET: &[CharType] = &[
            'a' as CharType,
            'Z' as CharType,
            ' ' as CharType,
            '~' as CharType,
            '0' as CharType,
            ESC,
            b'[' as CharType,
            b';' as CharType,
            b'm' as CharType,
            b'P' as CharType,
            b']' as CharType,
            b'\\' as CharType,
            0x07,
            0,
            0x07,
            0x0a,
            0x0d,
            0x7f,
            0x0301,
            0x0600,
            0x0e33,
            0x0eb3,
            0x200d,
            0x1f1e6,
            0x1f1e8,
            0x1f469,
            0x1f337,
            0x25b6,
            0x754c,
            0xfe0e,
            0xfe0f,
        ];
        let mut rng: u64 = 0x4d59_5df4_d0f3_3173;
        let mut next = || {
            rng = rng
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (rng >> 33) as usize
        };
        let mut h_std = OFF;
        let mut h_str = OFF;
        let mut h_state = OFF;
        let mut h_prefix = OFF;
        let mut h_pstate = OFF;
        for _ in 0..400 {
            let len = next() % 160;
            let mut chars = Vec::with_capacity(len + 1);
            for _ in 0..len {
                let r = next();
                let ch = if r % 5 == 0 {
                    (next() % 0x110000) as CharType
                } else {
                    ALPHABET[next() % ALPHABET.len()]
                };
                chars.push(ch);
            }

            mix(&mut h_std, wcswidth_std(&chars) as u64);

            let mut zbuf = chars.clone();
            zbuf.push(0);
            mix(&mut h_str, wcswidth_string(&zbuf) as u64);

            let mut st = WCSState::new();
            for &ch in &chars {
                st.wcswidth_step(ch);
            }
            mix_state(&mut h_state, &st);

            for end in 1..=len {
                mix(&mut h_prefix, wcswidth_std(&chars[..end]) as u64);
                let mut prefix = chars[..end].to_vec();
                prefix.push(0);
                mix(&mut h_prefix, wcswidth_string(&prefix) as u64);
                let mut ps = WCSState::new();
                for &ch in &chars[..end] {
                    ps.wcswidth_step(ch);
                }
                mix_state(&mut h_pstate, &ps);
            }
        }
        assert_eq!(h_std, C_STD);
        assert_eq!(h_str, C_STR);
        assert_eq!(h_state, C_STATE);
        assert_eq!(h_prefix, C_PREFIX);
        assert_eq!(h_pstate, C_PSTATE);
    }

    #[test]
    fn ascii_seg_constant_matches_step_result() {
        let mut seg = GraphemeSegmentationResult::new();
        for ch in [0x0041u32, 0x007e, 0x0020] {
            seg = grapheme_segmentation_step(seg, char_props_for(ch));
        }
        assert_eq!(seg, ASCII_SEG);
        assert_eq!(ASCII_SEG.bits(), 0x80);
    }

    #[test]
    fn state_is_zeroed_by_new_and_default() {
        let mut s = WCSState::new();
        s.wcswidth_step(ESC);
        s.wcswidth_step(ESC_OSC);
        assert_ne!(s, WCSState::new());
        s = WCSState::default();
        assert_eq!(s, WCSState::new());
        assert_eq!(s.parser_state, WCSParserState::NORMAL);
        assert_eq!(s.prev_ch, 0);
        assert_eq!(s.prev_width, 0);
        assert!(!s.can_combine);
        assert_eq!(s.seg.bits(), 0);
    }
}
