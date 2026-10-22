// SPDX-License-Identifier: GPL-3.0-or-later
// Ported from kitty/wcswidth.h and kitty/wcswidth.c,
// Copyright (C) 2020 Kovid Goyal <kovid at kovidgoyal.net>

//! ANSI-aware `wcswidth`, ported 1:1 from `kitty/wcswidth.c`.
//!
//! The parser skips CSI sequences (`ESC [ ... final`) and ST-terminated
//! strings (`ESC P`, `ESC ]`, `ESC X`, `ESC ^`, `ESC _` ... `BEL` or
//! `ESC \`), and applies the FE0F/FE0E presentation-selector and SpacingMark
//! cell-widening adjustments from the C implementation.

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

    /// `wcswidth_step()` from wcswidth.c. Returns the width delta for `ch`.
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
            WCSParserState::NORMAL => {
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
            }
            WCSParserState::IN_ESC => match ch {
                ESC_CSI => self.parser_state = WCSParserState::IN_CSI,
                ESC_DCS | ESC_OSC | ESC_SOS | ESC_PM | ESC_APC => {
                    self.parser_state = WCSParserState::IN_ST_TERMINATED;
                }
                // Two/three character escapes that are entirely consumed here.
                ESC_D | ESC_E | ESC_H | ESC_M | ESC_N | ESC_O | ESC_Z | ESC_6 | ESC_7 | ESC_8
                | ESC_9 | ESC_EQUALS | ESC_GREATER | ESC_F | ESC_C_LOWER | ESC_L_LOWER
                | ESC_M_LOWER | ESC_N_LOWER | ESC_O_LOWER | ESC_PIPE | ESC_RBRACE | ESC_TILDE => {}
                _ => {
                    // C: zero_at_ptr(state); return wcswidth_step(state, ch);
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
    let mut state = WCSState::new();
    let mut ans = 0u32;
    for &ch in chars {
        ans = ans.wrapping_add(state.wcswidth_step(ch) as u32);
    }
    ans
}

/// `wcswidth_string()`: width of a NUL-terminated sequence.
///
/// The C function takes a `char_type *` and stops at the first NUL. Rust
/// callers pass the slice containing the terminator (as is common when
/// mirroring the C API); bytes after the NUL are ignored.
#[inline]
pub fn wcswidth_string(chars: &[CharType]) -> u32 {
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
