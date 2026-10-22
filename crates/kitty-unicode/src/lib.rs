// SPDX-License-Identifier: GPL-3.0-or-later
//! Unicode property tables, grapheme segmentation and wcswidth, ported from
//! `kitty/char-props.c`, `char-props-data.h` and `kitty/wcswidth.c`.

pub mod char_props;
mod char_props_data;
pub mod wcswidth;

pub use char_props::{
    char_category, char_props_for, grapheme_segmentation_reset, grapheme_segmentation_step,
    is_private_use, wcwidth_std, CharProps, GraphemeBreakProperty, GraphemeSegmentationResult,
    IndicConjunctBreak, UnicodeCategory, MAX_UNICODE, UNICODE_VERSION, WIDTH_SHIFT,
};
pub use wcswidth::{wcswidth_std, wcswidth_step, wcswidth_string, WCSParserState, WCSState};
