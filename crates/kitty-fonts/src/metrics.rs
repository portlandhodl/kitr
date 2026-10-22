// SPDX-License-Identifier: GPL-3.0-or-later
//! Font and glyph metrics.

/// Raw metrics read from the font tables, in font units.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FaceMetrics {
    pub units_per_em: u32,
    pub ascender: i32,
    pub descender: i32,
    pub height: i32,
    pub max_advance_width: i32,
    pub max_advance_height: i32,
    pub underline_position: i32,
    pub underline_thickness: i32,
    pub strikethrough_position: i32,
    pub strikethrough_thickness: i32,
}

/// Pixel metrics for one terminal cell, computed exactly like kitty's
/// `cell_metrics()` in `kitty/freetype.c`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FontCellMetrics {
    pub cell_width: u32,
    pub cell_height: u32,
    pub baseline: u32,
    pub underline_position: u32,
    pub underline_thickness: u32,
    pub strikethrough_position: u32,
    pub strikethrough_thickness: u32,
}

/// Metrics for a single glyph at the face's current size.
///
/// Values with a `_26_6` suffix are FreeType 26.6 fixed-point pixels. The
/// convenience pixel values use kitty's rounding: advances are rounded up,
/// while `width_px` mirrors `get_glyph_width()` (bitmap width when present,
/// otherwise truncating `width / 64`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GlyphMetrics {
    pub glyph_id: u32,
    pub advance_x_26_6: i32,
    pub advance_y_26_6: i32,
    pub bearing_x_26_6: i32,
    pub bearing_y_26_6: i32,
    pub width_26_6: i32,
    pub height_26_6: i32,
    pub bitmap_width: u32,
    pub bitmap_rows: u32,
    pub bitmap_left: i32,
    pub bitmap_top: i32,
}

impl GlyphMetrics {
    pub fn advance_x_px(&self) -> u32 {
        ceil_div_64(self.advance_x_26_6)
    }

    pub fn advance_y_px(&self) -> u32 {
        ceil_div_64(self.advance_y_26_6)
    }

    /// Width in pixels as computed by kitty's `get_glyph_width()`.
    pub fn width_px(&self) -> u32 {
        if self.bitmap_width != 0 {
            self.bitmap_width
        } else {
            (self.width_26_6 / 64) as u32
        }
    }
}

fn ceil_div_64(value: i32) -> u32 {
    if value <= 0 {
        0
    } else {
        ((value as i64 + 63) / 64) as u32
    }
}
