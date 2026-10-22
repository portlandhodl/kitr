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
