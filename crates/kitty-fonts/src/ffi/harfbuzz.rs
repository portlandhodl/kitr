// SPDX-License-Identifier: GPL-3.0-or-later
//! Minimal, hand-written HarfBuzz bindings.

#![allow(non_camel_case_types, non_snake_case, dead_code)]

use core::ffi::{c_char, c_int, c_uint, c_void};

use super::freetype::FT_Face;

pub enum hb_font_t {}
pub enum hb_buffer_t {}
pub enum hb_face_t {}
pub enum hb_blob_t {}
pub enum hb_unicode_funcs_t {}

pub type hb_bool_t = c_int;
pub type hb_codepoint_t = u32;
pub type hb_mask_t = u32;
pub type hb_tag_t = u32;
pub type hb_position_t = i32;
pub type hb_var_int_t = u32;

pub const HB_DIRECTION_LTR: c_int = 4;
pub const HB_DIRECTION_RTL: c_int = 5;
pub const HB_DIRECTION_TTB: c_int = 6;
pub const HB_DIRECTION_BTT: c_int = 7;

pub const HB_BUFFER_CLUSTER_LEVEL_MONOTONE_GRAPHEMES: c_int = 0;
pub const HB_BUFFER_CLUSTER_LEVEL_MONOTONE_CHARACTERS: c_int = 1;
pub const HB_BUFFER_CLUSTER_LEVEL_CHARACTERS: c_int = 2;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct hb_glyph_info_t {
    pub codepoint: hb_codepoint_t,
    pub mask: hb_mask_t,
    pub cluster: u32,
    pub var1: hb_var_int_t,
    pub var2: hb_var_int_t,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct hb_glyph_position_t {
    pub x_advance: hb_position_t,
    pub y_advance: hb_position_t,
    pub x_offset: hb_position_t,
    pub y_offset: hb_position_t,
    pub var: hb_var_int_t,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct hb_feature_t {
    pub tag: hb_tag_t,
    pub value: u32,
    pub start: c_uint,
    pub end: c_uint,
}

pub const HB_FEATURE_GLOBAL_START: c_uint = 0;
pub const HB_FEATURE_GLOBAL_END: c_uint = u32::MAX;

#[link(name = "harfbuzz")]
extern "C" {
    pub fn hb_buffer_create() -> *mut hb_buffer_t;
    pub fn hb_buffer_destroy(buffer: *mut hb_buffer_t);
    pub fn hb_buffer_clear_contents(buffer: *mut hb_buffer_t);
    pub fn hb_buffer_add_utf32(
        buffer: *mut hb_buffer_t,
        text: *const u32,
        text_length: c_int,
        item_offset: c_uint,
        item_length: c_int,
    );
    pub fn hb_buffer_guess_segment_properties(buffer: *mut hb_buffer_t);
    pub fn hb_buffer_set_direction(buffer: *mut hb_buffer_t, direction: c_int);
    pub fn hb_buffer_set_cluster_level(buffer: *mut hb_buffer_t, level: c_int);
    pub fn hb_buffer_get_glyph_infos(
        buffer: *mut hb_buffer_t,
        length: *mut c_uint,
    ) -> *const hb_glyph_info_t;
    pub fn hb_buffer_get_glyph_positions(
        buffer: *mut hb_buffer_t,
        length: *mut c_uint,
    ) -> *const hb_glyph_position_t;
    pub fn hb_shape(
        font: *mut hb_font_t,
        buffer: *mut hb_buffer_t,
        features: *const hb_feature_t,
        num_features: c_uint,
    );
    pub fn hb_feature_from_string(
        string: *const c_char,
        len: c_int,
        feature: *mut hb_feature_t,
    ) -> hb_bool_t;
    pub fn hb_feature_to_string(
        feature: *mut hb_feature_t,
        buf: *mut c_char,
        size: c_uint,
    ) -> c_uint;
    pub fn hb_font_destroy(font: *mut hb_font_t);
    pub fn hb_ft_font_create(face: FT_Face, destroy: *mut c_void) -> *mut hb_font_t;
    pub fn hb_ft_font_changed(font: *mut hb_font_t);
    pub fn hb_ft_font_set_load_flags(font: *mut hb_font_t, load_flags: c_int);
    pub fn hb_font_set_scale(font: *mut hb_font_t, x_scale: c_int, y_scale: c_int);
    pub fn hb_font_get_glyph_h_advance(
        font: *mut hb_font_t,
        glyph: hb_codepoint_t,
    ) -> hb_position_t;
}
