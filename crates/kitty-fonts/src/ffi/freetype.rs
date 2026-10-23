// SPDX-License-Identifier: GPL-3.0-or-later
//! Minimal, hand-written FreeType 2 bindings.
//!
//! Only the small part of the FreeType API needed by the font stack is
//! declared here. The public structs are `#[repr(C)]` prefixes of the real
//! FreeType structs: fields after the ones declared here are never read, so
//! declaring a prefix is sound as long as the declared fields are in the same
//! order as in the system headers.

#![allow(non_camel_case_types, non_snake_case, dead_code)]

use core::ffi::{c_char, c_int, c_long, c_short, c_uint, c_ulong, c_ushort, c_void};

pub type FT_Error = c_int;
pub type FT_Fixed = c_long;
pub type FT_Pos = c_long;
pub type FT_Long = c_long;
pub type FT_ULong = c_ulong;
pub type FT_Int = c_int;
pub type FT_UInt = c_uint;
pub type FT_UShort = c_ushort;
pub type FT_Short = c_short;
pub type FT_F26Dot6 = c_long;
pub type FT_Bool = c_int;

pub enum FT_LibraryRec_ {}
pub type FT_Library = *mut FT_LibraryRec_;

pub type FT_Face = *mut FT_FaceRec;
pub type FT_Size = *mut FT_SizeRec;
pub type FT_GlyphSlot = *mut FT_GlyphSlotRec;

pub enum FT_CharMapRec {}
pub type FT_CharMap = *mut FT_CharMapRec;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct FT_Generic {
    pub data: *mut c_void,
    pub finalizer: Option<unsafe extern "C" fn(*mut c_void)>,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct FT_Vector {
    pub x: FT_Pos,
    pub y: FT_Pos,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct FT_BBox {
    pub xMin: FT_Pos,
    pub yMin: FT_Pos,
    pub xMax: FT_Pos,
    pub yMax: FT_Pos,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct FT_Glyph_Metrics {
    pub width: FT_Pos,
    pub height: FT_Pos,
    pub horiBearingX: FT_Pos,
    pub horiBearingY: FT_Pos,
    pub horiAdvance: FT_Pos,
    pub vertBearingX: FT_Pos,
    pub vertBearingY: FT_Pos,
    pub vertAdvance: FT_Pos,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct FT_Bitmap {
    pub rows: c_uint,
    pub width: c_uint,
    pub pitch: c_int,
    pub buffer: *mut u8,
    pub num_grays: c_ushort,
    pub pixel_mode: u8,
    pub palette_mode: u8,
    pub palette: *mut c_void,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct FT_Size_Metrics {
    pub x_ppem: FT_UShort,
    pub y_ppem: FT_UShort,
    pub x_scale: FT_Fixed,
    pub y_scale: FT_Fixed,
    pub ascender: FT_Pos,
    pub descender: FT_Pos,
    pub height: FT_Pos,
    pub max_advance: FT_Pos,
}

#[repr(C)]
pub struct FT_SizeRec {
    pub face: FT_Face,
    pub generic: FT_Generic,
    pub metrics: FT_Size_Metrics,
    pub internal: *mut c_void,
}

#[repr(C)]
pub struct FT_GlyphSlotRec {
    pub library: FT_Library,
    pub face: FT_Face,
    pub next: FT_GlyphSlot,
    pub glyph_index: FT_UInt,
    pub generic: FT_Generic,
    pub metrics: FT_Glyph_Metrics,
    pub linearHoriAdvance: FT_Fixed,
    pub linearVertAdvance: FT_Fixed,
    pub advance: FT_Vector,
    pub format: c_uint,
    pub bitmap: FT_Bitmap,
    pub bitmap_left: FT_Int,
    pub bitmap_top: FT_Int,
    // The remainder of FT_GlyphSlotRec is intentionally omitted.
}

#[repr(C)]
pub struct FT_FaceRec {
    pub num_faces: FT_Long,
    pub face_index: FT_Long,
    pub face_flags: FT_Long,
    pub style_flags: FT_Long,
    pub num_glyphs: FT_Long,
    pub family_name: *mut c_char,
    pub style_name: *mut c_char,
    pub num_fixed_sizes: FT_Int,
    pub available_sizes: *mut c_void,
    pub num_charmaps: FT_Int,
    pub charmaps: *mut FT_CharMap,
    pub generic: FT_Generic,
    pub bbox: FT_BBox,
    pub units_per_EM: FT_UShort,
    pub ascender: FT_Short,
    pub descender: FT_Short,
    pub height: FT_Short,
    pub max_advance_width: FT_Short,
    pub max_advance_height: FT_Short,
    pub underline_position: FT_Short,
    pub underline_thickness: FT_Short,
    pub glyph: FT_GlyphSlot,
    pub size: FT_Size,
    pub charmap: FT_CharMap,
}

#[repr(C)]
pub struct TT_OS2 {
    pub version: FT_UShort,
    pub xAvgCharWidth: FT_Short,
    pub usWeightClass: FT_UShort,
    pub usWidthClass: FT_UShort,
    pub fsType: FT_UShort,
    pub ySubscriptXSize: FT_Short,
    pub ySubscriptYSize: FT_Short,
    pub ySubscriptXOffset: FT_Short,
    pub ySubscriptYOffset: FT_Short,
    pub ySuperscriptXSize: FT_Short,
    pub ySuperscriptYSize: FT_Short,
    pub ySuperscriptXOffset: FT_Short,
    pub ySuperscriptYOffset: FT_Short,
    pub yStrikeoutSize: FT_Short,
    pub yStrikeoutPosition: FT_Short,
}

pub const FT_FACE_FLAG_SCALABLE: FT_Long = 1 << 0;
pub const FT_FACE_FLAG_FIXED_SIZES: FT_Long = 1 << 1;
pub const FT_FACE_FLAG_FIXED_WIDTH: FT_Long = 1 << 2;
pub const FT_FACE_FLAG_SFNT: FT_Long = 1 << 3;
pub const FT_FACE_FLAG_MULTIPLE_MASTERS: FT_Long = 1 << 8;
pub const FT_FACE_FLAG_COLOR: FT_Long = 1 << 14;

pub const FT_STYLE_FLAG_ITALIC: FT_Long = 1 << 0;
pub const FT_STYLE_FLAG_BOLD: FT_Long = 1 << 1;

pub const FT_LOAD_DEFAULT: c_int = 0;
pub const FT_LOAD_NO_HINTING: c_int = 1 << 1;
pub const FT_LOAD_RENDER: c_int = 1 << 2;
pub const FT_LOAD_COLOR: c_int = 1 << 20;

pub const FT_PIXEL_MODE_NONE: u8 = 0;
pub const FT_PIXEL_MODE_MONO: u8 = 1;
pub const FT_PIXEL_MODE_GRAY: u8 = 2;
pub const FT_PIXEL_MODE_GRAY2: u8 = 3;
pub const FT_PIXEL_MODE_GRAY4: u8 = 4;
pub const FT_PIXEL_MODE_LCD: u8 = 5;
pub const FT_PIXEL_MODE_LCD_V: u8 = 6;
pub const FT_PIXEL_MODE_BGRA: u8 = 7;

pub const FT_GLYPH_FORMAT_BITMAP: c_uint = 0x6269_7473; // 'bits'
pub const FT_GLYPH_FORMAT_OUTLINE: c_uint = 0x6f75_746c; // 'outl'
pub const FT_RENDER_MODE_NORMAL: c_int = 0;
pub const FT_LOAD_TARGET_LIGHT: c_int = 1 << 16;
pub const FT_LOAD_TARGET_NORMAL: c_int = 0;

pub const FT_SFNT_OS2: c_int = 2;

#[link(name = "freetype")]
extern "C" {
    pub fn FT_Init_FreeType(alibrary: *mut FT_Library) -> FT_Error;
    pub fn FT_Done_FreeType(library: FT_Library) -> FT_Error;
    pub fn FT_New_Face(
        library: FT_Library,
        filepathname: *const c_char,
        face_index: FT_Long,
        aface: *mut FT_Face,
    ) -> FT_Error;
    pub fn FT_Done_Face(face: FT_Face) -> FT_Error;
    pub fn FT_Set_Char_Size(
        face: FT_Face,
        char_width: FT_F26Dot6,
        char_height: FT_F26Dot6,
        horz_resolution: FT_UInt,
        vert_resolution: FT_UInt,
    ) -> FT_Error;
    pub fn FT_Select_Size(face: FT_Face, strike_index: FT_Int) -> FT_Error;
    pub fn FT_Get_Char_Index(face: FT_Face, charcode: FT_ULong) -> FT_UInt;
    pub fn FT_Load_Glyph(face: FT_Face, glyph_index: FT_UInt, load_flags: FT_Int) -> FT_Error;
    pub fn FT_Render_Glyph(slot: FT_GlyphSlot, render_mode: c_int) -> FT_Error;
    pub fn FT_Get_Postscript_Name(face: FT_Face) -> *const c_char;
    pub fn FT_Get_Sfnt_Table(face: FT_Face, tag: c_int) -> *mut c_void;
    pub fn FT_MulFix(a: FT_Long, b: FT_Long) -> FT_Long;
    pub fn FT_Get_Advance(
        face: FT_Face,
        gindex: FT_UInt,
        flags: FT_Int,
        padvance: *mut FT_Fixed,
    ) -> FT_Error;
}

pub fn error_name(code: FT_Error) -> &'static str {
    match code {
        0x00 => "no error",
        0x01 => "cannot open resource",
        0x02 => "unknown file format",
        0x03 => "broken file",
        0x04 => "invalid FreeType version",
        0x05 => "module version is too low",
        0x06 => "invalid argument",
        0x07 => "unimplemented feature",
        0x08 => "broken table",
        0x09 => "broken offset within table",
        0x0A => "array allocation size too large",
        0x0B => "missing module",
        0x0C => "missing property",
        0x10 => "invalid glyph index",
        0x11 => "invalid character code",
        0x12 => "unsupported glyph image format",
        0x13 => "cannot render this glyph format",
        0x14 => "invalid outline",
        0x15 => "invalid composite glyph",
        0x16 => "too many hints",
        0x17 => "invalid pixel size",
        0x20 => "invalid object handle",
        0x21 => "invalid library handle",
        0x22 => "invalid module handle",
        0x23 => "invalid face handle",
        0x24 => "invalid size handle",
        0x25 => "invalid glyph slot handle",
        0x26 => "invalid charmap handle",
        0x27 => "invalid cache manager handle",
        0x28 => "invalid stream handle",
        _ => "unknown FreeType error",
    }
}

// {{{ Color fonts (COLR/CPAL), glyph transforms and raw SFNT tables

pub type FT_F2Dot14 = c_short;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct FT_Matrix {
    pub xx: FT_Fixed,
    pub xy: FT_Fixed,
    pub yx: FT_Fixed,
    pub yy: FT_Fixed,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct FT_Color {
    pub blue: u8,
    pub green: u8,
    pub red: u8,
    pub alpha: u8,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct FT_Palette_Data {
    pub num_palettes: FT_UShort,
    pub palette_name_ids: *const FT_UShort,
    pub palette_flags: *const FT_UShort,
    pub num_palette_entries: FT_UShort,
    pub palette_entry_name_ids: *const FT_UShort,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct FT_LayerIterator {
    pub num_layers: FT_UInt,
    pub layer: FT_UInt,
    pub p: *mut u8,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct FT_OpaquePaint {
    pub p: *mut u8,
    pub insert_root_transform: FT_Bool,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct FT_ColorIndex {
    pub palette_index: FT_UShort,
    pub alpha: FT_F2Dot14,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct FT_ColorStop {
    pub stop_offset: FT_Fixed,
    pub color: FT_ColorIndex,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct FT_ColorStopIterator {
    pub num_color_stops: FT_UInt,
    pub current_color_stop: FT_UInt,
    pub p: *mut u8,
    pub read_variable: FT_Bool,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct FT_ColorLine {
    pub extend: c_int,
    pub color_stop_iterator: FT_ColorStopIterator,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct FT_Affine23 {
    pub xx: FT_Fixed,
    pub xy: FT_Fixed,
    pub dx: FT_Fixed,
    pub yx: FT_Fixed,
    pub yy: FT_Fixed,
    pub dy: FT_Fixed,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct FT_PaintColrLayers {
    pub layer_iterator: FT_LayerIterator,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct FT_PaintSolid {
    pub color: FT_ColorIndex,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct FT_PaintLinearGradient {
    pub colorline: FT_ColorLine,
    pub p0: FT_Vector,
    pub p1: FT_Vector,
    pub p2: FT_Vector,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct FT_PaintRadialGradient {
    pub colorline: FT_ColorLine,
    pub c0: FT_Vector,
    pub r0: FT_Pos,
    pub c1: FT_Vector,
    pub r1: FT_Pos,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct FT_PaintSweepGradient {
    pub colorline: FT_ColorLine,
    pub center: FT_Vector,
    pub start_angle: FT_Fixed,
    pub end_angle: FT_Fixed,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct FT_PaintGlyph {
    pub paint: FT_OpaquePaint,
    pub glyphID: FT_UInt,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct FT_PaintColrGlyph {
    pub glyphID: FT_UInt,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct FT_PaintTransform {
    pub paint: FT_OpaquePaint,
    pub affine: FT_Affine23,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct FT_PaintTranslate {
    pub paint: FT_OpaquePaint,
    pub dx: FT_Fixed,
    pub dy: FT_Fixed,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct FT_PaintScale {
    pub paint: FT_OpaquePaint,
    pub scale_x: FT_Fixed,
    pub scale_y: FT_Fixed,
    pub center_x: FT_Fixed,
    pub center_y: FT_Fixed,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct FT_PaintRotate {
    pub paint: FT_OpaquePaint,
    pub angle: FT_Fixed,
    pub center_x: FT_Fixed,
    pub center_y: FT_Fixed,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct FT_PaintSkew {
    pub paint: FT_OpaquePaint,
    pub x_skew_angle: FT_Fixed,
    pub y_skew_angle: FT_Fixed,
    pub center_x: FT_Fixed,
    pub center_y: FT_Fixed,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub union FT_COLR_PaintUnion {
    pub colr_layers: FT_PaintColrLayers,
    pub glyph: FT_PaintGlyph,
    pub solid: FT_PaintSolid,
    pub linear_gradient: FT_PaintLinearGradient,
    pub radial_gradient: FT_PaintRadialGradient,
    pub sweep_gradient: FT_PaintSweepGradient,
    pub transform: FT_PaintTransform,
    pub translate: FT_PaintTranslate,
    pub scale: FT_PaintScale,
    pub rotate: FT_PaintRotate,
    pub skew: FT_PaintSkew,
    pub composite: FT_PaintComposite,
    pub colr_glyph: FT_PaintColrGlyph,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct FT_PaintComposite {
    pub source_paint: FT_OpaquePaint,
    pub composite_mode: c_int,
    pub backdrop_paint: FT_OpaquePaint,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct FT_COLR_Paint {
    pub format: c_int,
    pub u: FT_COLR_PaintUnion,
}

pub const FT_COLR_PAINTFORMAT_COLR_LAYERS: c_int = 1;
pub const FT_COLR_PAINTFORMAT_SOLID: c_int = 2;
pub const FT_COLR_PAINTFORMAT_LINEAR_GRADIENT: c_int = 4;
pub const FT_COLR_PAINTFORMAT_RADIAL_GRADIENT: c_int = 6;
pub const FT_COLR_PAINTFORMAT_SWEEP_GRADIENT: c_int = 8;
pub const FT_COLR_PAINTFORMAT_GLYPH: c_int = 10;
pub const FT_COLR_PAINTFORMAT_COLR_GLYPH: c_int = 11;
pub const FT_COLR_PAINTFORMAT_TRANSFORM: c_int = 12;
pub const FT_COLR_PAINTFORMAT_TRANSLATE: c_int = 14;
pub const FT_COLR_PAINTFORMAT_SCALE: c_int = 16;
pub const FT_COLR_PAINTFORMAT_ROTATE: c_int = 24;
pub const FT_COLR_PAINTFORMAT_SKEW: c_int = 28;
pub const FT_COLR_PAINTFORMAT_COMPOSITE: c_int = 32;
pub const FT_COLR_PAINTFORMAT_UNSUPPORTED: c_int = 255;

pub const FT_COLOR_INCLUDE_ROOT_TRANSFORM: c_int = 0;
pub const FT_COLOR_NO_ROOT_TRANSFORM: c_int = 1;

pub const FT_COLR_COMPOSITE_CLEAR: c_int = 0;
pub const FT_COLR_COMPOSITE_SRC: c_int = 1;
pub const FT_COLR_COMPOSITE_DEST: c_int = 2;
pub const FT_COLR_COMPOSITE_SRC_OVER: c_int = 3;
pub const FT_COLR_COMPOSITE_DEST_OVER: c_int = 4;
pub const FT_COLR_COMPOSITE_SRC_IN: c_int = 5;
pub const FT_COLR_COMPOSITE_DEST_IN: c_int = 6;
pub const FT_COLR_COMPOSITE_SRC_OUT: c_int = 7;
pub const FT_COLR_COMPOSITE_DEST_OUT: c_int = 8;
pub const FT_COLR_COMPOSITE_SRC_ATOP: c_int = 9;
pub const FT_COLR_COMPOSITE_DEST_ATOP: c_int = 10;
pub const FT_COLR_COMPOSITE_XOR: c_int = 11;
pub const FT_COLR_COMPOSITE_PLUS: c_int = 12;
pub const FT_COLR_COMPOSITE_SCREEN: c_int = 13;
pub const FT_COLR_COMPOSITE_OVERLAY: c_int = 14;
pub const FT_COLR_COMPOSITE_DARKEN: c_int = 15;
pub const FT_COLR_COMPOSITE_LIGHTEN: c_int = 16;
pub const FT_COLR_COMPOSITE_COLOR_DODGE: c_int = 17;
pub const FT_COLR_COMPOSITE_COLOR_BURN: c_int = 18;
pub const FT_COLR_COMPOSITE_HARD_LIGHT: c_int = 19;
pub const FT_COLR_COMPOSITE_SOFT_LIGHT: c_int = 20;
pub const FT_COLR_COMPOSITE_DIFFERENCE: c_int = 21;
pub const FT_COLR_COMPOSITE_EXCLUSION: c_int = 22;
pub const FT_COLR_COMPOSITE_MULTIPLY: c_int = 23;

pub const FT_PALETTE_FOR_LIGHT_BACKGROUND: FT_UShort = 1 << 0;
pub const FT_PALETTE_FOR_DARK_BACKGROUND: FT_UShort = 1 << 1;
pub const FT_PALETTE_FOREGROUND_INDEX: FT_UShort = 0xFFFF;

#[link(name = "freetype")]
extern "C" {
    pub fn FT_Set_Transform(face: FT_Face, matrix: *const FT_Matrix, delta: *const FT_Vector);
    pub fn FT_Get_Transform(face: FT_Face, matrix: *mut FT_Matrix, delta: *mut FT_Vector);
    pub fn FT_Load_Sfnt_Table(
        face: FT_Face,
        tag: FT_ULong,
        offset: FT_Long,
        buffer: *mut u8,
        length: *mut FT_ULong,
    ) -> FT_Error;
    pub fn FT_Palette_Data_Get(face: FT_Face, apalette: *mut FT_Palette_Data) -> FT_Error;
    pub fn FT_Palette_Select(
        face: FT_Face,
        palette_index: FT_UShort,
        apalette_entries: *mut *mut FT_Color,
    ) -> FT_Error;
    pub fn FT_Palette_Set_Foreground_Color(face: FT_Face, foreground: FT_Color) -> FT_Error;
    pub fn FT_Get_Color_Glyph_Layer(
        face: FT_Face,
        base_glyph: FT_UInt,
        aglyph_index: *mut FT_UInt,
        acolor_index: *mut FT_UInt,
        iterator: *mut FT_LayerIterator,
    ) -> FT_Bool;
    pub fn FT_Get_Color_Glyph_Paint(
        face: FT_Face,
        base_glyph: FT_UInt,
        root_transform: c_int,
        paint: *mut FT_OpaquePaint,
    ) -> FT_Bool;
    pub fn FT_Get_Paint(
        face: FT_Face,
        paint: FT_OpaquePaint,
        apaint: *mut FT_COLR_Paint,
    ) -> FT_Bool;
    pub fn FT_Get_Paint_Layers(
        face: FT_Face,
        iterator: *mut FT_LayerIterator,
        paint: *mut FT_OpaquePaint,
    ) -> FT_Bool;
    pub fn FT_Get_Colorline_Stops(
        face: FT_Face,
        color_stop: *mut FT_ColorStop,
        iterator: *mut FT_ColorStopIterator,
    ) -> FT_Bool;
}

// }}}
