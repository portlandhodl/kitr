// SPDX-License-Identifier: GPL-3.0-or-later
// Ported from kitty/line.h, Copyright (C) 2024 Kovid Goyal <kovid at kovidgoyal.net>

//! Cell representations.
//!
//! These mirror the C unions in `kitty/line.h` bit for bit. The C structs use
//! bit-fields whose layout is implementation defined for big endian targets;
//! kitty itself only supports little endian here, so we do the same.

use core::mem::size_of;

pub type CharType = u32;
pub type ColorType = u32;
pub type HyperlinkIdType = u16;
pub type SpriteIndex = u32;

pub const MAX_CHAR_TYPE_VALUE: CharType = u32::MAX;
pub const BLANK_CHAR: CharType = 0;
pub const WIDTH_MASK: u8 = 3;
pub const DECORATION_MASK: u32 = 7;
pub const MARK_MASK: u32 = 3;
pub const SGR_MASK: u32 = !(MARK_MASK << 9);
pub const MAX_NUM_CODEPOINTS_PER_CELL: u32 = 24;
/// Text presentation selector.
pub const VS15: CharType = 0xfe0e;
/// Emoji presentation selector.
pub const VS16: CharType = 0xfe0f;
pub const IMAGE_PLACEHOLDER_CHAR: CharType = 0x10EEEE;

pub const SCALE_BITS: u8 = 3;
pub const WIDTH_BITS: u8 = 3;
pub const SUBSCALE_BITS: u8 = 4;
pub const VALIGN_BITS: u8 = 2;
pub const HALIGN_BITS: u8 = 2;

pub const PROMPT_KIND_MASK: u8 = 3;

/// `CellAttrs` from line.h. The bit-field order on little endian is:
/// decoration:3, bold:1, italic:1, reverse:1, strike:1, dim:1, blink:1,
/// mark:2, padding:21.
#[derive(Copy, Clone, Default, PartialEq, Eq, Hash)]
#[repr(transparent)]
pub struct CellAttrs(pub u32);

impl CellAttrs {
    #[inline(always)]
    pub const fn from_val(val: u32) -> Self {
        Self(val)
    }

    #[inline(always)]
    pub const fn val(self) -> u32 {
        self.0
    }

    #[inline(always)]
    pub const fn decoration(self) -> u32 {
        self.0 & DECORATION_MASK
    }

    #[inline(always)]
    pub fn set_decoration(&mut self, v: u32) {
        self.0 = (self.0 & !DECORATION_MASK) | (v & DECORATION_MASK);
    }

    #[inline(always)]
    pub const fn bold(self) -> bool {
        (self.0 >> 3) & 1 != 0
    }

    #[inline(always)]
    pub fn set_bold(&mut self, v: bool) {
        self.0 = (self.0 & !(1 << 3)) | ((v as u32) << 3);
    }

    #[inline(always)]
    pub const fn italic(self) -> bool {
        (self.0 >> 4) & 1 != 0
    }

    #[inline(always)]
    pub fn set_italic(&mut self, v: bool) {
        self.0 = (self.0 & !(1 << 4)) | ((v as u32) << 4);
    }

    #[inline(always)]
    pub const fn reverse(self) -> bool {
        (self.0 >> 5) & 1 != 0
    }

    #[inline(always)]
    pub fn set_reverse(&mut self, v: bool) {
        self.0 = (self.0 & !(1 << 5)) | ((v as u32) << 5);
    }

    #[inline(always)]
    pub const fn strike(self) -> bool {
        (self.0 >> 6) & 1 != 0
    }

    #[inline(always)]
    pub fn set_strike(&mut self, v: bool) {
        self.0 = (self.0 & !(1 << 6)) | ((v as u32) << 6);
    }

    #[inline(always)]
    pub const fn dim(self) -> bool {
        (self.0 >> 7) & 1 != 0
    }

    #[inline(always)]
    pub fn set_dim(&mut self, v: bool) {
        self.0 = (self.0 & !(1 << 7)) | ((v as u32) << 7);
    }

    #[inline(always)]
    pub const fn blink(self) -> bool {
        (self.0 >> 8) & 1 != 0
    }

    #[inline(always)]
    pub fn set_blink(&mut self, v: bool) {
        self.0 = (self.0 & !(1 << 8)) | ((v as u32) << 8);
    }

    #[inline(always)]
    pub const fn mark(self) -> u32 {
        (self.0 >> 9) & MARK_MASK
    }

    #[inline(always)]
    pub fn set_mark(&mut self, v: u32) {
        self.0 = (self.0 & !(MARK_MASK << 9)) | ((v & MARK_MASK) << 9);
    }
}

impl core::fmt::Debug for CellAttrs {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("CellAttrs")
            .field("decoration", &self.decoration())
            .field("bold", &self.bold())
            .field("italic", &self.italic())
            .field("reverse", &self.reverse())
            .field("strike", &self.strike())
            .field("dim", &self.dim())
            .field("blink", &self.blink())
            .field("mark", &self.mark())
            .finish()
    }
}

/// `GPUCell` from line.h, exactly 20 bytes.
#[derive(Copy, Clone, Default, PartialEq, Eq, Hash, Debug)]
#[repr(C)]
pub struct GpuCell {
    pub fg: ColorType,
    pub bg: ColorType,
    pub decoration_fg: ColorType,
    pub sprite_idx: SpriteIndex,
    pub attrs: CellAttrs,
}

/// `CPUCell` from line.h, exactly 12 bytes on little endian.
///
/// The C version is a union of bit-fields. We store the three 32 bit words and
/// expose accessors with the same bit offsets. `word1`/`word2` together are the
/// second 64 bit bit-field block.
#[derive(Copy, Clone, Default, PartialEq, Eq, Hash)]
#[repr(C)]
pub struct CpuCell {
    /// ch_or_idx:31, ch_is_idx:1
    word0: u32,
    /// hyperlink_id:16, scale:3, subscale_n:4, subscale_d:4,
    /// valign:2, halign:2, next_char_was_wrapped:1
    word1: u32,
    /// is_multicell:1, natural_width:1, x:6, y:3, width:3, temp_flag:1,
    /// padding:17
    word2: u32,
}

const CH_MASK: u32 = 0x7fff_ffff;
const CH_IS_IDX_SHIFT: u32 = 31;

impl CpuCell {
    #[inline(always)]
    pub const fn new() -> Self {
        Self {
            word0: 0,
            word1: 0,
            word2: 0,
        }
    }

    /// Raw value of the first union view (char plus index flag).
    #[inline(always)]
    pub const fn ch_and_idx(self) -> u32 {
        self.word0
    }

    #[inline(always)]
    pub fn set_ch_and_idx(&mut self, val: u32) {
        self.word0 = val;
    }

    #[inline(always)]
    pub const fn ch(self) -> CharType {
        self.word0 & CH_MASK
    }

    #[inline(always)]
    pub const fn ch_is_idx(self) -> bool {
        (self.word0 >> CH_IS_IDX_SHIFT) & 1 != 0
    }

    #[inline(always)]
    pub fn set_ch_is_idx(&mut self, val: bool) {
        self.word0 = (self.word0 & CH_MASK) | ((val as u32) << CH_IS_IDX_SHIFT);
    }

    #[inline(always)]
    pub fn set_char(&mut self, ch: CharType) {
        self.word0 = (self.word0 & !CH_MASK) | (ch & CH_MASK);
    }

    #[inline(always)]
    pub const fn is_char(self, ch: CharType) -> bool {
        self.word0 == ch
    }

    #[inline(always)]
    pub const fn has_text(self) -> bool {
        self.word0 != 0
    }

    #[inline(always)]
    pub const fn hyperlink_id(self) -> HyperlinkIdType {
        self.word1 as u16
    }

    #[inline(always)]
    pub fn set_hyperlink_id(&mut self, v: HyperlinkIdType) {
        self.word1 = (self.word1 & !0xffff) | v as u32;
    }

    #[inline(always)]
    pub const fn scale_field(self) -> u32 {
        (self.word1 >> 16) & 0x7
    }

    #[inline(always)]
    pub fn set_scale_field(&mut self, v: u32) {
        self.word1 = (self.word1 & !(0x7 << 16)) | ((v & 0x7) << 16);
    }

    #[inline(always)]
    pub const fn subscale_n(self) -> u32 {
        (self.word1 >> 19) & 0xf
    }

    #[inline(always)]
    pub fn set_subscale_n(&mut self, v: u32) {
        self.word1 = (self.word1 & !(0xf << 19)) | ((v & 0xf) << 19);
    }

    #[inline(always)]
    pub const fn subscale_d(self) -> u32 {
        (self.word1 >> 23) & 0xf
    }

    #[inline(always)]
    pub fn set_subscale_d(&mut self, v: u32) {
        self.word1 = (self.word1 & !(0xf << 23)) | ((v & 0xf) << 23);
    }

    #[inline(always)]
    pub const fn valign(self) -> u32 {
        (self.word1 >> 27) & 0x3
    }

    #[inline(always)]
    pub fn set_valign(&mut self, v: u32) {
        self.word1 = (self.word1 & !(0x3 << 27)) | ((v & 0x3) << 27);
    }

    #[inline(always)]
    pub const fn halign(self) -> u32 {
        (self.word1 >> 29) & 0x3
    }

    #[inline(always)]
    pub fn set_halign(&mut self, v: u32) {
        self.word1 = (self.word1 & !(0x3 << 29)) | ((v & 0x3) << 29);
    }

    #[inline(always)]
    pub const fn next_char_was_wrapped(self) -> bool {
        (self.word1 >> 31) & 1 != 0
    }

    #[inline(always)]
    pub fn set_next_char_was_wrapped(&mut self, v: bool) {
        self.word1 = (self.word1 & !(1 << 31)) | ((v as u32) << 31);
    }

    #[inline(always)]
    pub const fn is_multicell(self) -> bool {
        self.word2 & 1 != 0
    }

    #[inline(always)]
    pub fn set_is_multicell(&mut self, v: bool) {
        self.word2 = (self.word2 & !1) | v as u32;
    }

    #[inline(always)]
    pub const fn natural_width(self) -> bool {
        (self.word2 >> 1) & 1 != 0
    }

    #[inline(always)]
    pub fn set_natural_width(&mut self, v: bool) {
        self.word2 = (self.word2 & !(1 << 1)) | ((v as u32) << 1);
    }

    #[inline(always)]
    pub const fn x(self) -> u32 {
        (self.word2 >> 2) & 0x3f
    }

    #[inline(always)]
    pub fn set_x(&mut self, v: u32) {
        self.word2 = (self.word2 & !(0x3f << 2)) | ((v & 0x3f) << 2);
    }

    #[inline(always)]
    pub const fn y(self) -> u32 {
        (self.word2 >> 8) & 0x7
    }

    #[inline(always)]
    pub fn set_y(&mut self, v: u32) {
        self.word2 = (self.word2 & !(0x7 << 8)) | ((v & 0x7) << 8);
    }

    #[inline(always)]
    pub const fn width(self) -> u32 {
        (self.word2 >> 11) & 0x7
    }

    #[inline(always)]
    pub fn set_width(&mut self, v: u32) {
        self.word2 = (self.word2 & !(0x7 << 11)) | ((v & 0x7) << 11);
    }

    #[inline(always)]
    pub const fn temp_flag(self) -> bool {
        (self.word2 >> 14) & 1 != 0
    }

    #[inline(always)]
    pub fn set_temp_flag(&mut self, v: bool) {
        self.word2 = (self.word2 & !(1 << 14)) | ((v as u32) << 14);
    }

    /// `cell_scale()`: multicell cells store a scale, others are 1.
    #[inline(always)]
    pub const fn scale(self) -> u32 {
        if self.is_multicell() {
            self.scale_field()
        } else {
            1
        }
    }

    /// `mcd_x_limit()`
    #[inline(always)]
    pub const fn mcd_x_limit(self) -> u32 {
        self.scale() * self.width()
    }
}

impl core::fmt::Debug for CpuCell {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("CpuCell")
            .field("ch", &format_args!("0x{:x}", self.ch()))
            .field("ch_is_idx", &self.ch_is_idx())
            .field("hyperlink_id", &self.hyperlink_id())
            .field("width", &self.width())
            .field("is_multicell", &self.is_multicell())
            .finish()
    }
}

/// `LineAttrs` from line.h: dirty:1, image_placeholders:1, prompt_kind:2.
#[derive(Copy, Clone, Default, PartialEq, Eq, Hash, Debug)]
#[repr(transparent)]
pub struct LineAttrs(pub u8);

impl LineAttrs {
    #[inline(always)]
    pub const fn from_val(val: u8) -> Self {
        Self(val)
    }

    #[inline(always)]
    pub const fn val(self) -> u8 {
        self.0
    }

    #[inline(always)]
    pub const fn has_dirty_text(self) -> bool {
        self.0 & 1 != 0
    }

    #[inline(always)]
    pub fn set_has_dirty_text(&mut self, v: bool) {
        self.0 = (self.0 & !1) | v as u8;
    }

    #[inline(always)]
    pub const fn has_image_placeholders(self) -> bool {
        (self.0 >> 1) & 1 != 0
    }

    #[inline(always)]
    pub fn set_has_image_placeholders(&mut self, v: bool) {
        self.0 = (self.0 & !(1 << 1)) | ((v as u8) << 1);
    }

    #[inline(always)]
    pub const fn prompt_kind(self) -> u8 {
        (self.0 >> 2) & PROMPT_KIND_MASK
    }

    #[inline(always)]
    pub fn set_prompt_kind(&mut self, v: u8) {
        self.0 = (self.0 & !(PROMPT_KIND_MASK << 2)) | ((v & PROMPT_KIND_MASK) << 2);
    }
}

const _: () = {
    assert!(size_of::<CellAttrs>() == 4);
    assert!(size_of::<GpuCell>() == 20);
    assert!(size_of::<CpuCell>() == 12);
    assert!(size_of::<LineAttrs>() == 1);
    assert!(core::mem::align_of::<CpuCell>() == 4);
    assert!(core::mem::align_of::<GpuCell>() == 4);
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_cell_char_roundtrip() {
        let mut c = CpuCell::new();
        assert!(!c.has_text());
        c.set_char('é' as u32);
        assert_eq!(c.ch(), 'é' as u32);
        assert!(!c.ch_is_idx());
        assert!(c.has_text());
        assert!(c.is_char('é' as u32));
    }

    #[test]
    fn cpu_cell_fields_do_not_overlap() {
        let mut c = CpuCell::new();
        c.set_char(0x1f600);
        c.set_ch_is_idx(true);
        c.set_hyperlink_id(0xbeef);
        c.set_scale_field(5);
        c.set_subscale_n(9);
        c.set_subscale_d(3);
        c.set_valign(2);
        c.set_halign(1);
        c.set_next_char_was_wrapped(true);
        c.set_is_multicell(true);
        c.set_natural_width(true);
        c.set_x(0x3f);
        c.set_y(7);
        c.set_width(6);
        c.set_temp_flag(true);

        assert_eq!(c.ch(), 0x1f600);
        assert!(c.ch_is_idx());
        assert_eq!(c.hyperlink_id(), 0xbeef);
        assert_eq!(c.scale_field(), 5);
        assert_eq!(c.subscale_n(), 9);
        assert_eq!(c.subscale_d(), 3);
        assert_eq!(c.valign(), 2);
        assert_eq!(c.halign(), 1);
        assert!(c.next_char_was_wrapped());
        assert!(c.is_multicell());
        assert!(c.natural_width());
        assert_eq!(c.x(), 0x3f);
        assert_eq!(c.y(), 7);
        assert_eq!(c.width(), 6);
        assert!(c.temp_flag());
        assert_eq!(c.scale(), 5);
        assert_eq!(c.mcd_x_limit(), 30);
    }

    #[test]
    fn attrs_are_bit_compatible() {
        let mut a = CellAttrs::default();
        a.set_decoration(5);
        a.set_bold(true);
        a.set_italic(true);
        a.set_reverse(true);
        a.set_strike(true);
        a.set_dim(true);
        a.set_blink(true);
        a.set_mark(2);
        assert_eq!(a.val(), 0b101_1111_1101);
        assert_eq!(a.decoration(), 5);
        assert!(a.bold() && a.italic() && a.reverse() && a.strike() && a.dim() && a.blink());
        assert_eq!(a.mark(), 2);
        assert_eq!(CellAttrs::default().val() & SGR_MASK, 0);
    }

    #[test]
    fn line_attrs_bit_compatible() {
        let mut a = LineAttrs::default();
        a.set_has_dirty_text(true);
        a.set_has_image_placeholders(true);
        a.set_prompt_kind(3);
        assert_eq!(a.val(), 0b1111);
        a.set_prompt_kind(1);
        assert_eq!(a.prompt_kind(), 1);
        assert!(a.has_dirty_text());
        assert!(a.has_image_placeholders());
    }
}
