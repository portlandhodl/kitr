// SPDX-License-Identifier: GPL-3.0-or-later
// Ported from kitty/cursor.c and kitty/data-types.h
// Copyright (C) 2016-2024 Kovid Goyal <kovid at kovidgoyal.net>

//! Cursor state and SGR parsing.
//!
//! This mirrors `Cursor` from `data-types.h` and the display-attribute
//! handling in `cursor.c`. Colours use the same encoding as the C code:
//! `(index << 8) | 1` for palette colours and
//! `(r << 24) | (g << 16) | (b << 8) | 2` for 24-bit colours.

use kitty_core::{CellAttrs, GpuCell};

/// `CursorShape` from `data-types.h`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum CursorShape {
    #[default]
    NoCursor = 0,
    Block = 1,
    Beam = 2,
    Underline = 3,
    Hollow = 4,
}

impl CursorShape {
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Block,
            2 => Self::Beam,
            3 => Self::Underline,
            4 => Self::Hollow,
            _ => Self::NoCursor,
        }
    }
}

pub const DECORATION_FG_CODE: u32 = 58;

/// The `sgr` member of `Cursor`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SgrAttrs {
    pub bold: bool,
    pub italic: bool,
    pub reverse: bool,
    pub strikethrough: bool,
    pub dim: bool,
    pub blink: bool,
    pub decoration: u8,
    pub fg: u32,
    pub bg: u32,
    pub decoration_fg: u32,
}

impl SgrAttrs {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Convert the boolean/text attributes to the packed `CellAttrs` layout.
    pub fn as_cell_attrs(self) -> CellAttrs {
        let mut a = CellAttrs::default();
        a.set_decoration(self.decoration as u32);
        a.set_bold(self.bold);
        a.set_italic(self.italic);
        a.set_reverse(self.reverse);
        a.set_strike(self.strikethrough);
        a.set_dim(self.dim);
        a.set_blink(self.blink);
        a
    }

    /// Build the GPU cell used as the template for newly drawn cells.
    pub fn as_gpu_cell(self) -> GpuCell {
        GpuCell {
            fg: self.fg,
            bg: self.bg,
            decoration_fg: self.decoration_fg,
            sprite_idx: 0,
            attrs: self.as_cell_attrs(),
        }
    }
}

/// A cursor, matching the C `Cursor` structure.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cursor {
    pub x: u32,
    pub y: u32,
    pub non_blinking: bool,
    pub shape: CursorShape,
    pub sgr: SgrAttrs,
}

impl Cursor {
    pub fn new() -> Self {
        Self::default()
    }

    /// `cursor_reset()`: clear display attributes and position/shape.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// `cursor_reset_display_attrs()`.
    pub fn reset_display_attrs(&mut self) {
        self.sgr.reset();
    }

    /// The Python `cursor.blink` property is `!non_blinking`.
    pub fn blinking(&self) -> bool {
        !self.non_blinking
    }

    /// `cursor_to_attrs()`.
    pub fn to_attrs(&self) -> CellAttrs {
        self.sgr.as_cell_attrs()
    }

    /// `attrs_to_cursor()` (display attributes only).
    pub fn set_attrs(&mut self, attrs: CellAttrs) {
        self.sgr.decoration = attrs.decoration() as u8;
        self.sgr.bold = attrs.bold();
        self.sgr.italic = attrs.italic();
        self.sgr.reverse = attrs.reverse();
        self.sgr.strikethrough = attrs.strike();
        self.sgr.dim = attrs.dim();
        self.sgr.blink = attrs.blink();
    }

    /// `cursor_from_sgr()`. When `is_group` is true only the first SGR
    /// parameter (plus any colour sub-parameters) is consumed.
    pub fn from_sgr(&mut self, params: &[i32], is_group: bool) {
        let fallback = [0i32];
        let params = if params.is_empty() { &fallback } else { params };
        let mut i = 0usize;
        while i < params.len() {
            let attr = params[i] as u32;
            i += 1;
            match attr {
                0 => self.reset_display_attrs(),
                1 => self.sgr.bold = true,
                2 => self.sgr.dim = true,
                3 => self.sgr.italic = true,
                4 => {
                    if is_group && i < params.len() {
                        self.sgr.decoration = (params[i].clamp(0, 5)) as u8;
                        i += 1;
                    } else {
                        self.sgr.decoration = 1;
                    }
                }
                5 => self.sgr.blink = true,
                7 => self.sgr.reverse = true,
                9 => self.sgr.strikethrough = true,
                21 => self.sgr.decoration = 2,
                221 => self.sgr.bold = false,
                222 => self.sgr.dim = false,
                22 => {
                    self.sgr.bold = false;
                    self.sgr.dim = false;
                }
                23 => self.sgr.italic = false,
                24 => self.sgr.decoration = 0,
                25 => self.sgr.blink = false,
                27 => self.sgr.reverse = false,
                29 => self.sgr.strikethrough = false,
                30..=37 => self.sgr.fg = ((attr - 30) << 8) | 1,
                38 => parse_color(params, &mut i, &mut self.sgr.fg),
                39 => self.sgr.fg = 0,
                40..=47 => self.sgr.bg = ((attr - 40) << 8) | 1,
                48 => parse_color(params, &mut i, &mut self.sgr.bg),
                49 => self.sgr.bg = 0,
                90..=97 => self.sgr.fg = ((attr - 90 + 8) << 8) | 1,
                100..=107 => self.sgr.bg = ((attr - 100 + 8) << 8) | 1,
                DECORATION_FG_CODE => parse_color(params, &mut i, &mut self.sgr.decoration_fg),
                x if x == DECORATION_FG_CODE + 1 => self.sgr.decoration_fg = 0,
                _ => {}
            }
            if is_group {
                break;
            }
        }
    }
}

/// `parse_color()` from `cursor.c`.
fn parse_color(params: &[i32], i: &mut usize, result: &mut u32) {
    if *i >= params.len() {
        return;
    }
    let attr = params[*i] as u32;
    *i += 1;
    match attr {
        5 => {
            if *i < params.len() {
                let idx = (params[*i] as u32) & 0xff;
                *i += 1;
                *result = (idx << 8) | 1;
            }
        }
        2 if *i + 2 < params.len() => {
            // Ignore the optional colour-space id in a four parameter RGB
            // sequence, matching kitty issue #227.
            if *i + 3 < params.len() {
                *i += 1;
            }
            let r = (params[*i] as u32) & 0xff;
            let g = (params[*i + 1] as u32) & 0xff;
            let b = (params[*i + 2] as u32) & 0xff;
            *i += 3;
            *result = (r << 24) | (g << 16) | (b << 8) | 2;
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_attributes() {
        let mut c = Cursor::new();
        c.from_sgr(&[1, 2, 3, 4, 5, 7, 9], false);
        assert!(c.sgr.bold && c.sgr.dim && c.sgr.italic && c.sgr.blink);
        assert!(c.sgr.reverse && c.sgr.strikethrough);
        assert_eq!(c.sgr.decoration, 1);
        c.from_sgr(&[22, 23, 24, 25, 27, 29], false);
        assert!(!c.sgr.bold && !c.sgr.dim && !c.sgr.italic);
        assert!(!c.sgr.blink && !c.sgr.reverse && !c.sgr.strikethrough);
    }

    #[test]
    fn colors_and_groups() {
        let mut c = Cursor::new();
        c.from_sgr(&[31, 42], false);
        assert_eq!(c.sgr.fg, (1 << 8) | 1);
        assert_eq!(c.sgr.bg, (2 << 8) | 1);
        c.from_sgr(&[97, 107], false);
        assert_eq!(c.sgr.fg, (15 << 8) | 1);
        assert_eq!(c.sgr.bg, (15 << 8) | 1);
        c.from_sgr(&[38, 5, 200], false);
        assert_eq!(c.sgr.fg, (200 << 8) | 1);
        c.from_sgr(&[48, 2, 1, 2, 3], false);
        assert_eq!(c.sgr.bg, (1 << 24) | (2 << 16) | (3 << 8) | 2);
        c.from_sgr(&[4, 5], true);
        assert_eq!(c.sgr.decoration, 5);
    }
}
