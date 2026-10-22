// SPDX-License-Identifier: GPL-3.0-or-later
// Ported from kitty/char-props.h, kitty/char-props.c and
// kitty/char-props-data.h, Copyright (C) 2025 Kovid Goyal <kovid at kovidgoyal.net>

//! Unicode character properties and grapheme segmentation.
//!
//! [`CharProps`] is a `#[repr(transparent)]` wrapper around the same packed
//! 32-bit little-endian bitfield union used by C. The generated tables in
//! [`crate::char_props_data`] store these values directly, so a lookup is just
//! three indexed loads. The union's alternate 7-bit
//! `grapheme_segmentation_property` view occupies the top seven bits
//! (`grapheme_break | indic_conjunct_break << 4 | extended_pictographic << 6`),
//! exactly as in C.
//!
//! [`GraphemeSegmentationResult`] likewise wraps the C `uint16_t` union; the
//! 9-bit `state` is bits 7..=15 and `add_to_current_cell` is bit 6.

use kitty_core::CharType;

use crate::char_props_data::{
    CHAR_PROPS_MASK, CHAR_PROPS_SHIFT, CHAR_PROPS_T1, CHAR_PROPS_T2, CHAR_PROPS_T3,
    GRAPHEME_SEGMENTATION_RESULT_MASK, GRAPHEME_SEGMENTATION_RESULT_SHIFT,
    GRAPHEME_SEGMENTATION_RESULT_T1, GRAPHEME_SEGMENTATION_RESULT_T2,
};

pub use crate::char_props_data::{MAX_UNICODE, UNICODE_VERSION};

/// The C generator's `width_shift`: `shifted_width = width + WIDTH_SHIFT`.
pub const WIDTH_SHIFT: i32 = 4;

/// Unicode general category (`UnicodeCategory` in C).
#[allow(non_camel_case_types)]
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum UnicodeCategory {
    UC_Cn = 0,
    UC_Cc = 1,
    UC_Zs = 2,
    UC_Po = 3,
    UC_Sc = 4,
    UC_Ps = 5,
    UC_Pe = 6,
    UC_Sm = 7,
    UC_Pd = 8,
    UC_Nd = 9,
    UC_Lu = 10,
    UC_Sk = 11,
    UC_Pc = 12,
    UC_Ll = 13,
    UC_So = 14,
    UC_Lo = 15,
    UC_Pi = 16,
    UC_Cf = 17,
    UC_No = 18,
    UC_Pf = 19,
    UC_Lt = 20,
    UC_Lm = 21,
    UC_Mn = 22,
    UC_Me = 23,
    UC_Mc = 24,
    UC_Nl = 25,
    UC_Zl = 26,
    UC_Zp = 27,
    UC_Cs = 28,
    UC_Co = 29,
}

impl UnicodeCategory {
    /// Decode the 5-bit category field. Values outside the enum cannot be
    /// produced by the generated tables; they fall back to `UC_Cn` like
    /// `char_category()` does in C.
    #[inline]
    pub const fn from_raw(value: u8) -> Self {
        match value {
            0 => Self::UC_Cn,
            1 => Self::UC_Cc,
            2 => Self::UC_Zs,
            3 => Self::UC_Po,
            4 => Self::UC_Sc,
            5 => Self::UC_Ps,
            6 => Self::UC_Pe,
            7 => Self::UC_Sm,
            8 => Self::UC_Pd,
            9 => Self::UC_Nd,
            10 => Self::UC_Lu,
            11 => Self::UC_Sk,
            12 => Self::UC_Pc,
            13 => Self::UC_Ll,
            14 => Self::UC_So,
            15 => Self::UC_Lo,
            16 => Self::UC_Pi,
            17 => Self::UC_Cf,
            18 => Self::UC_No,
            19 => Self::UC_Pf,
            20 => Self::UC_Lt,
            21 => Self::UC_Lm,
            22 => Self::UC_Mn,
            23 => Self::UC_Me,
            24 => Self::UC_Mc,
            25 => Self::UC_Nl,
            26 => Self::UC_Zl,
            27 => Self::UC_Zp,
            28 => Self::UC_Cs,
            29 => Self::UC_Co,
            _ => Self::UC_Cn,
        }
    }

    /// The two-letter category name used by C's `char_category()`.
    #[inline]
    pub const fn name(self) -> &'static str {
        match self {
            Self::UC_Cn => "Cn",
            Self::UC_Cc => "Cc",
            Self::UC_Zs => "Zs",
            Self::UC_Po => "Po",
            Self::UC_Sc => "Sc",
            Self::UC_Ps => "Ps",
            Self::UC_Pe => "Pe",
            Self::UC_Sm => "Sm",
            Self::UC_Pd => "Pd",
            Self::UC_Nd => "Nd",
            Self::UC_Lu => "Lu",
            Self::UC_Sk => "Sk",
            Self::UC_Pc => "Pc",
            Self::UC_Ll => "Ll",
            Self::UC_So => "So",
            Self::UC_Lo => "Lo",
            Self::UC_Pi => "Pi",
            Self::UC_Cf => "Cf",
            Self::UC_No => "No",
            Self::UC_Pf => "Pf",
            Self::UC_Lt => "Lt",
            Self::UC_Lm => "Lm",
            Self::UC_Mn => "Mn",
            Self::UC_Me => "Me",
            Self::UC_Mc => "Mc",
            Self::UC_Nl => "Nl",
            Self::UC_Zl => "Zl",
            Self::UC_Zp => "Zp",
            Self::UC_Cs => "Cs",
            Self::UC_Co => "Co",
        }
    }
}

/// Grapheme cluster break property (`GraphemeBreakProperty` in C).
#[allow(non_camel_case_types)]
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum GraphemeBreakProperty {
    GBP_AtStart = 0,
    GBP_None = 1,
    GBP_Prepend = 2,
    GBP_CR = 3,
    GBP_LF = 4,
    GBP_Control = 5,
    GBP_Extend = 6,
    GBP_Regional_Indicator = 7,
    GBP_SpacingMark = 8,
    GBP_L = 9,
    GBP_V = 10,
    GBP_T = 11,
    GBP_LV = 12,
    GBP_LVT = 13,
    GBP_ZWJ = 14,
    GBP_Private_Expecting_RI = 15,
}

impl GraphemeBreakProperty {
    #[inline]
    pub const fn from_raw(value: u8) -> Self {
        match value {
            0 => Self::GBP_AtStart,
            1 => Self::GBP_None,
            2 => Self::GBP_Prepend,
            3 => Self::GBP_CR,
            4 => Self::GBP_LF,
            5 => Self::GBP_Control,
            6 => Self::GBP_Extend,
            7 => Self::GBP_Regional_Indicator,
            8 => Self::GBP_SpacingMark,
            9 => Self::GBP_L,
            10 => Self::GBP_V,
            11 => Self::GBP_T,
            12 => Self::GBP_LV,
            13 => Self::GBP_LVT,
            14 => Self::GBP_ZWJ,
            _ => Self::GBP_Private_Expecting_RI,
        }
    }
}

/// Indic conjunct break property (`IndicConjunctBreak` in C).
#[allow(non_camel_case_types)]
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum IndicConjunctBreak {
    ICB_None = 0,
    ICB_Linker = 1,
    ICB_Consonant = 2,
    ICB_Extend = 3,
}

impl IndicConjunctBreak {
    #[inline]
    pub const fn from_raw(value: u8) -> Self {
        match value {
            0 => Self::ICB_None,
            1 => Self::ICB_Linker,
            2 => Self::ICB_Consonant,
            _ => Self::ICB_Extend,
        }
    }
}

/// Packed character properties, bit-compatible with C's `CharProps` union.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct CharProps(u32);

impl CharProps {
    #[inline]
    pub const fn from_bits(bits: u32) -> Self {
        Self(bits)
    }

    /// The raw `uint32_t val` of the C union.
    #[inline]
    pub const fn bits(self) -> u32 {
        self.0
    }

    #[inline]
    pub const fn shifted_width(self) -> u8 {
        ((self.0 >> 9) & 0x7) as u8
    }

    #[inline]
    pub const fn is_emoji(self) -> bool {
        (self.0 >> 12) & 0x1 != 0
    }

    #[inline]
    pub const fn category(self) -> UnicodeCategory {
        UnicodeCategory::from_raw(((self.0 >> 13) & 0x1f) as u8)
    }

    #[inline]
    pub const fn is_emoji_presentation_base(self) -> bool {
        (self.0 >> 18) & 0x1 != 0
    }

    #[inline]
    pub const fn is_invalid(self) -> bool {
        (self.0 >> 19) & 0x1 != 0
    }

    #[inline]
    pub const fn is_non_rendered(self) -> bool {
        (self.0 >> 20) & 0x1 != 0
    }

    #[inline]
    pub const fn is_symbol(self) -> bool {
        (self.0 >> 21) & 0x1 != 0
    }

    #[inline]
    pub const fn is_combining_char(self) -> bool {
        (self.0 >> 22) & 0x1 != 0
    }

    #[inline]
    pub const fn is_word_char(self) -> bool {
        (self.0 >> 23) & 0x1 != 0
    }

    #[inline]
    pub const fn is_punctuation(self) -> bool {
        (self.0 >> 24) & 0x1 != 0
    }

    #[inline]
    pub const fn grapheme_break(self) -> GraphemeBreakProperty {
        GraphemeBreakProperty::from_raw(((self.0 >> 25) & 0xf) as u8)
    }

    #[inline]
    pub const fn indic_conjunct_break(self) -> IndicConjunctBreak {
        IndicConjunctBreak::from_raw(((self.0 >> 29) & 0x3) as u8)
    }

    #[inline]
    pub const fn is_extended_pictographic(self) -> bool {
        (self.0 >> 31) & 0x1 != 0
    }

    /// The alternate union view used by grapheme segmentation (bits 25..=31).
    #[inline]
    pub const fn grapheme_segmentation_property(self) -> u8 {
        ((self.0 >> 25) & 0x7f) as u8
    }
}

impl core::fmt::Debug for CharProps {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("CharProps")
            .field("shifted_width", &self.shifted_width())
            .field("is_emoji", &self.is_emoji())
            .field("category", &self.category())
            .field(
                "is_emoji_presentation_base",
                &self.is_emoji_presentation_base(),
            )
            .field("is_invalid", &self.is_invalid())
            .field("is_non_rendered", &self.is_non_rendered())
            .field("is_symbol", &self.is_symbol())
            .field("is_combining_char", &self.is_combining_char())
            .field("is_word_char", &self.is_word_char())
            .field("is_punctuation", &self.is_punctuation())
            .field("grapheme_break", &self.grapheme_break())
            .field("indic_conjunct_break", &self.indic_conjunct_break())
            .field("is_extended_pictographic", &self.is_extended_pictographic())
            .finish()
    }
}

/// `char_props_for()` from char-props.c.
///
/// Values above [`MAX_UNICODE`] are mapped to index 0, exactly like the
/// branchless `ensure_char_in_range()` in C.
#[inline]
pub fn char_props_for(ch: CharType) -> CharProps {
    // Branchless `ensure_char_in_range()`: the shift yields all ones for an
    // in-range value and zero for anything above MAX_UNICODE.
    let diff = (ch as i64) - (MAX_UNICODE as i64 + 1);
    let mask = (diff >> 63) as u32;
    let ch = ch & mask;
    let t1 = CHAR_PROPS_T1[(ch >> CHAR_PROPS_SHIFT) as usize] as usize;
    let index = (t1 << CHAR_PROPS_SHIFT) + (ch & CHAR_PROPS_MASK) as usize;
    CHAR_PROPS_T3[CHAR_PROPS_T2[index] as usize]
}

/// `wcwidth_std()` from char-props.h.
#[inline]
pub fn wcwidth_std(ch: CharProps) -> i32 {
    ch.shifted_width() as i32 - WIDTH_SHIFT
}

/// `is_private_use()` from char-props.h.
#[inline]
pub fn is_private_use(ch: CharProps) -> bool {
    ch.category() == UnicodeCategory::UC_Co
}

/// `char_category()` from char-props.h.
#[inline]
pub fn char_category(ch: CharProps) -> &'static str {
    ch.category().name()
}

/// Packed grapheme segmentation state/result, bit-compatible with C's
/// `GraphemeSegmentationResult` union.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
#[repr(transparent)]
pub struct GraphemeSegmentationResult(u16);

impl GraphemeSegmentationResult {
    #[inline]
    pub const fn new() -> Self {
        Self(0)
    }

    #[inline]
    pub const fn from_bits(bits: u16) -> Self {
        Self(bits)
    }

    /// The raw `uint16_t val` of the C union.
    #[inline]
    pub const fn bits(self) -> u16 {
        self.0
    }

    #[inline]
    pub const fn add_to_current_cell(self) -> bool {
        (self.0 >> 6) & 0x1 != 0
    }

    #[inline]
    pub const fn grapheme_break(self) -> GraphemeBreakProperty {
        GraphemeBreakProperty::from_raw(((self.0 >> 7) & 0xf) as u8)
    }

    #[inline]
    pub const fn incb_consonant_extended(self) -> bool {
        (self.0 >> 11) & 0x1 != 0
    }

    #[inline]
    pub const fn incb_consonant_extended_linker(self) -> bool {
        (self.0 >> 12) & 0x1 != 0
    }

    #[inline]
    pub const fn incb_consonant_extended_linker_extended(self) -> bool {
        (self.0 >> 13) & 0x1 != 0
    }

    #[inline]
    pub const fn emoji_modifier_sequence(self) -> bool {
        (self.0 >> 14) & 0x1 != 0
    }

    #[inline]
    pub const fn emoji_modifier_sequence_before_last_char(self) -> bool {
        (self.0 >> 15) & 0x1 != 0
    }

    /// The alternate union view used as the table state (bits 7..=15).
    #[inline]
    pub const fn state(self) -> u16 {
        (self.0 >> 7) & 0x1ff
    }
}

impl core::fmt::Debug for GraphemeSegmentationResult {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("GraphemeSegmentationResult")
            .field("add_to_current_cell", &self.add_to_current_cell())
            .field("grapheme_break", &self.grapheme_break())
            .field("incb_consonant_extended", &self.incb_consonant_extended())
            .field(
                "incb_consonant_extended_linker",
                &self.incb_consonant_extended_linker(),
            )
            .field(
                "incb_consonant_extended_linker_extended",
                &self.incb_consonant_extended_linker_extended(),
            )
            .field("emoji_modifier_sequence", &self.emoji_modifier_sequence())
            .field(
                "emoji_modifier_sequence_before_last_char",
                &self.emoji_modifier_sequence_before_last_char(),
            )
            .finish()
    }
}

/// `grapheme_segmentation_reset()` from char-props.c.
#[inline]
pub fn grapheme_segmentation_reset(s: &mut GraphemeSegmentationResult) {
    *s = GraphemeSegmentationResult::new();
}

/// `grapheme_segmentation_step()` from char-props.c.
#[inline]
pub fn grapheme_segmentation_step(
    r: GraphemeSegmentationResult,
    ch: CharProps,
) -> GraphemeSegmentationResult {
    let key = ((r.state() as usize) << 7) | ch.grapheme_segmentation_property() as usize;
    let t1 = (GRAPHEME_SEGMENTATION_RESULT_T1[key >> GRAPHEME_SEGMENTATION_RESULT_SHIFT] as usize)
        << GRAPHEME_SEGMENTATION_RESULT_SHIFT;
    let index = t1 + (key & GRAPHEME_SEGMENTATION_RESULT_MASK as usize);
    GRAPHEME_SEGMENTATION_RESULT_T2[index]
}

// Keep the compiler honest about the generated table geometry (the generated
// module also asserts its own lengths).
const _: () = {
    assert!(CHAR_PROPS_T1.len() == 4352);
    assert!(CHAR_PROPS_T2.len() == 46592);
    assert!(CHAR_PROPS_T3.len() == 106);
    assert!(GRAPHEME_SEGMENTATION_RESULT_T1.len() == 4096);
    assert!(GRAPHEME_SEGMENTATION_RESULT_T2.len() == 2880);
};

const _: () = {
    assert!(core::mem::size_of::<CharProps>() == 4);
    assert!(core::mem::size_of::<GraphemeSegmentationResult>() == 2);
    assert!(MAX_UNICODE == 0x10ffff);
};

#[cfg(test)]
mod tests {
    use super::*;

    fn graphemes(text: &str) -> Vec<String> {
        let mut seg = GraphemeSegmentationResult::new();
        let mut ans = Vec::new();
        let mut current = String::new();
        for c in text.chars() {
            let step = grapheme_segmentation_step(seg, char_props_for(c as CharType));
            if !step.add_to_current_cell() && !current.is_empty() {
                ans.push(core::mem::take(&mut current));
            }
            current.push(c);
            seg = step;
        }
        if !current.is_empty() {
            ans.push(current);
        }
        ans
    }

    #[test]
    fn ascii_properties() {
        let a = char_props_for('a' as CharType);
        assert_eq!(a.category(), UnicodeCategory::UC_Ll);
        assert_eq!(wcwidth_std(a), 1);
        assert_eq!(char_category(a), "Ll");
        assert!(!a.is_invalid());
        assert!(!a.is_non_rendered());
        assert!(!a.is_combining_char());
        assert!(a.is_word_char());
        assert!(!a.is_punctuation());
        assert!(!a.is_emoji());
        assert!(!is_private_use(a));

        let space = char_props_for(' ' as CharType);
        assert_eq!(space.category(), UnicodeCategory::UC_Zs);
        assert_eq!(wcwidth_std(space), 1);

        let comma = char_props_for(',' as CharType);
        assert_eq!(comma.category(), UnicodeCategory::UC_Po);
        assert!(comma.is_punctuation());
        assert!(!comma.is_word_char());
    }

    #[test]
    fn control_and_invalid_properties() {
        let nul = char_props_for(0);
        assert_eq!(nul.category(), UnicodeCategory::UC_Cc);
        assert!(nul.is_invalid());
        assert!(nul.is_non_rendered());
        assert_eq!(wcwidth_std(nul), 0);
        assert_eq!(nul.grapheme_break(), GraphemeBreakProperty::GBP_Control);

        let esc = char_props_for(0x1b);
        assert_eq!(wcwidth_std(esc), -1);
        assert!(esc.is_invalid());

        // Surrogates are Cs and are non-rendered.
        let surrogate = char_props_for(0xd800);
        assert_eq!(surrogate.category(), UnicodeCategory::UC_Cs);
        assert!(surrogate.is_non_rendered());
    }

    #[test]
    fn wide_and_combining_properties() {
        let cjk = char_props_for('コ' as CharType);
        assert_eq!(wcwidth_std(cjk), 2);

        let emoji = char_props_for(0x1f337);
        assert_eq!(wcwidth_std(emoji), 2);
        assert!(emoji.is_emoji());
        assert!(emoji.is_extended_pictographic());
        assert_eq!(
            emoji.grapheme_segmentation_property() >> 6,
            1,
            "extended pictographic bit must be the top bit"
        );

        let combining = char_props_for(0x0301);
        assert_eq!(wcwidth_std(combining), 0);
        assert!(combining.is_combining_char());
        assert_eq!(
            combining.grapheme_break(),
            GraphemeBreakProperty::GBP_Extend
        );

        let zwj = char_props_for(0x200d);
        assert_eq!(zwj.grapheme_break(), GraphemeBreakProperty::GBP_ZWJ);
        assert_eq!(wcwidth_std(zwj), 0);
    }

    #[test]
    fn emoji_presentation_and_regional_indicators() {
        let btn = char_props_for(0x25b6);
        assert!(btn.is_emoji_presentation_base());

        let vs16 = char_props_for(0xfe0f);
        assert!(vs16.is_non_rendered());
        assert_eq!(wcwidth_std(vs16), 0);

        let ri = char_props_for(0x1f1ee);
        assert_eq!(
            ri.grapheme_break(),
            GraphemeBreakProperty::GBP_Regional_Indicator
        );
        assert_eq!(wcwidth_std(ri), 2);
    }

    #[test]
    fn category_matches_known_values() {
        assert_eq!(
            char_props_for('A' as CharType).category(),
            UnicodeCategory::UC_Lu
        );
        assert_eq!(
            char_props_for('0' as CharType).category(),
            UnicodeCategory::UC_Nd
        );
        assert_eq!(
            char_props_for('∑' as CharType).category(),
            UnicodeCategory::UC_Sm
        );
        assert_eq!(
            char_props_for('\u{e000}' as CharType).category(),
            UnicodeCategory::UC_Co
        );
        assert!(is_private_use(char_props_for(0xe000)));
        assert_eq!(
            char_props_for('\u{2028}' as CharType).category(),
            UnicodeCategory::UC_Zl
        );
    }

    #[test]
    fn all_codepoints_do_not_panic() {
        for cp in 0..=0x10ffffu32 {
            let p = char_props_for(cp);
            assert!(p.shifted_width() <= 7);
            assert!((p.category() as u8) < 30);
            assert!((p.grapheme_break() as u8) < 16);
            assert!((p.indic_conjunct_break() as u8) < 4);
            let _ = wcwidth_std(p);
            let _ = char_category(p);
        }
    }

    #[test]
    fn out_of_range_maps_to_index_zero() {
        let zero = char_props_for(0);
        for cp in [0x110000, 0x110001, 0x1fffff, u32::MAX] {
            assert_eq!(char_props_for(cp), zero, "cp {cp:#x}");
        }
    }

    #[test]
    fn table_lengths() {
        assert_eq!(CHAR_PROPS_T1.len(), 4352);
        assert_eq!(CHAR_PROPS_T2.len(), 46592);
        assert_eq!(CHAR_PROPS_T3.len(), 106);
        assert_eq!(GRAPHEME_SEGMENTATION_RESULT_T1.len(), 4096);
        assert_eq!(GRAPHEME_SEGMENTATION_RESULT_T2.len(), 2880);
        assert_eq!(
            CHAR_PROPS_T1.len(),
            1 + (MAX_UNICODE >> CHAR_PROPS_SHIFT) as usize
        );
    }

    #[test]
    fn matches_the_c_reference_implementation() {
        // FNV-1a over the raw CharProps.val for every code point, and over the
        // GraphemeSegmentationResult.val after stepping the sample sequence
        // below. These hashes were produced by compiling the checked-in C
        // char-props.c/char-props-data.h with gcc and running the same loop.
        const C_CHAR_PROPS_HASH: u64 = 17_608_787_913_117_681_027;
        const C_GRAPHEME_HASH: u64 = 3_278_123_042_399_646_769;

        let mut h: u64 = 1_469_598_103_934_665_603;
        for cp in 0..=MAX_UNICODE {
            h ^= char_props_for(cp).bits() as u64;
            h = h.wrapping_mul(1_099_511_628_211);
        }
        assert_eq!(h, C_CHAR_PROPS_HASH);

        let mut h: u64 = 1_469_598_103_934_665_603;
        let mut r = GraphemeSegmentationResult::new();
        let samples = [
            0x61u32, 0x301, 0x200d, 0x1f469, 0x1f1e6, 0x0d, 0x0a, 0x1100, 0x1161, 0x11a8, 0x915,
            0x94d, 0x0e33, 0xfe0f, 0xfe0e,
        ];
        for cp in samples {
            r = grapheme_segmentation_step(r, char_props_for(cp));
            h ^= r.bits() as u64;
            h = h.wrapping_mul(1_099_511_628_211);
        }
        assert_eq!(h, C_GRAPHEME_HASH);
    }

    #[test]
    fn grapheme_combining_mark() {
        assert_eq!(graphemes("e\u{0301}"), vec!["e\u{0301}"]);
        assert_eq!(
            graphemes("a\u{0301}\u{0302}b"),
            vec!["a\u{0301}\u{0302}", "b"]
        );
    }

    #[test]
    fn grapheme_crlf() {
        assert_eq!(graphemes("\r\n"), vec!["\r\n"]);
        assert_eq!(graphemes("a\r\nb"), vec!["a", "\r\n", "b"]);
        assert_eq!(graphemes("\r\r\n"), vec!["\r", "\r\n"]);
    }

    #[test]
    fn grapheme_regional_indicators() {
        // RI pairs are one cluster; a third RI starts a new cluster.
        assert_eq!(graphemes("\u{1f1e6}\u{1f1e8}"), vec!["\u{1f1e6}\u{1f1e8}"]);
        assert_eq!(
            graphemes("\u{1f1e6}\u{1f1e8}\u{1f1e6}"),
            vec!["\u{1f1e6}\u{1f1e8}", "\u{1f1e6}"]
        );
        assert_eq!(
            graphemes("\u{1f1e6}\u{1f1e8}\u{1f1e6}\u{1f1e8}"),
            vec!["\u{1f1e6}\u{1f1e8}", "\u{1f1e6}\u{1f1e8}"]
        );
    }

    #[test]
    fn grapheme_hangul() {
        assert_eq!(
            graphemes("\u{1100}\u{1161}\u{11a8}"),
            vec!["\u{1100}\u{1161}\u{11a8}"]
        );
        assert_eq!(graphemes("\u{ac00}\u{11a8}"), vec!["\u{ac00}\u{11a8}"]);
        assert_eq!(graphemes("\u{ac00}\u{ac01}"), vec!["\u{ac00}", "\u{ac01}"]);
    }

    #[test]
    fn grapheme_zwj_emoji() {
        let family = "\u{1f469}\u{200d}\u{1f469}\u{200d}\u{1f466}";
        assert_eq!(graphemes(family), vec![family]);
        // A ZWJ joins the previous character (GB9), but a following non-emoji
        // does not join the ZWJ cluster (GB11 requires Extended_Pictographic).
        assert_eq!(graphemes("a\u{200d}b"), vec!["a\u{200d}", "b"]);
    }

    #[test]
    fn grapheme_indic_conjunct() {
        // KA + VIRAMA (linker) + KA: GB9c keeps the conjunct together.
        let conjunct = "\u{0915}\u{094d}\u{0915}";
        assert_eq!(graphemes(conjunct), vec![conjunct]);
        // The same with a ZWJ inserted (extend/linker chain).
        let with_zwj = "\u{0915}\u{094d}\u{200d}\u{0915}";
        assert_eq!(graphemes(with_zwj), vec![with_zwj]);
        // Without the linker there is a break.
        assert_eq!(graphemes("\u{0915}\u{0915}"), vec!["\u{0915}", "\u{0915}"]);
    }

    #[test]
    fn grapheme_prepend_and_spacing_mark() {
        // Arabic number sign is Prepend: it sticks to the following character.
        assert_eq!(graphemes("\u{0600}1"), vec!["\u{0600}1"]);
        // Thai SARA AM is a SpacingMark and attaches to the base.
        assert_eq!(graphemes("\u{0e01}\u{0e33}"), vec!["\u{0e01}\u{0e33}"]);
    }

    #[test]
    fn grapheme_reset_and_state_bits() {
        let mut s = GraphemeSegmentationResult::new();
        assert_eq!(s.bits(), 0);
        assert_eq!(s.state(), 0);
        assert_eq!(s.grapheme_break(), GraphemeBreakProperty::GBP_AtStart);
        assert!(!s.add_to_current_cell());

        s = grapheme_segmentation_step(s, char_props_for('a' as CharType));
        assert!(s.add_to_current_cell());
        let old = s;
        grapheme_segmentation_reset(&mut s);
        assert_eq!(s.bits(), 0);
        assert_ne!(old.bits(), 0);
    }
}
