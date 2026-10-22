// SPDX-License-Identifier: GPL-3.0-or-later
//! Minimal, hand-written Fontconfig bindings.

#![allow(non_camel_case_types, non_snake_case, dead_code)]

use core::ffi::{c_char, c_int};

pub type FcBool = c_int;
pub type FcChar8 = u8;
pub type FcChar32 = u32;

pub enum FcPattern {}
pub enum FcCharSet {}
pub enum FcConfig {}
pub enum FcObjectSet {}

#[repr(C)]
pub struct FcFontSet {
    pub nfont: c_int,
    pub sfont: c_int,
    pub fonts: *mut *mut FcPattern,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FcResult {
    Match = 0,
    NoMatch = 1,
    TypeMismatch = 2,
    NoId = 3,
    OutOfMemory = 4,
}

pub const FC_FALSE: FcBool = 0;
pub const FC_TRUE: FcBool = 1;

pub const FC_WEIGHT_REGULAR: c_int = 80;
pub const FC_WEIGHT_SEMIBOLD: c_int = 180;
pub const FC_WEIGHT_BOLD: c_int = 200;
pub const FC_SLANT_ROMAN: c_int = 0;
pub const FC_SLANT_ITALIC: c_int = 100;
pub const FC_SLANT_OBLIQUE: c_int = 110;
pub const FC_MONO: c_int = 100;
pub const FC_DUAL: c_int = 90;

pub const FC_MATCH_PATTERN: c_int = 0;

pub const FC_FAMILY: &[u8] = b"family\0";
pub const FC_STYLE: &[u8] = b"style\0";
pub const FC_SLANT: &[u8] = b"slant\0";
pub const FC_WEIGHT: &[u8] = b"weight\0";
pub const FC_SPACING: &[u8] = b"spacing\0";
pub const FC_HINTING: &[u8] = b"hinting\0";
pub const FC_HINT_STYLE: &[u8] = b"hintstyle\0";
pub const FC_FILE: &[u8] = b"file\0";
pub const FC_INDEX: &[u8] = b"index\0";
pub const FC_CHARSET: &[u8] = b"charset\0";
pub const FC_FULLNAME: &[u8] = b"fullname\0";
pub const FC_POSTSCRIPT_NAME: &[u8] = b"postscriptname\0";
pub const FC_OUTLINE: &[u8] = b"outline\0";
pub const FC_SCALABLE: &[u8] = b"scalable\0";
pub const FC_COLOR: &[u8] = b"color\0";
pub const FC_SIZE: &[u8] = b"size\0";
pub const FC_DPI: &[u8] = b"dpi\0";

#[link(name = "fontconfig")]
extern "C" {
    pub fn FcInit() -> FcBool;
    pub fn FcPatternCreate() -> *mut FcPattern;
    pub fn FcPatternDestroy(pattern: *mut FcPattern);
    pub fn FcPatternAddString(
        pattern: *mut FcPattern,
        object: *const c_char,
        s: *const FcChar8,
    ) -> FcBool;
    pub fn FcPatternAddInteger(pattern: *mut FcPattern, object: *const c_char, i: c_int) -> FcBool;
    pub fn FcPatternAddBool(pattern: *mut FcPattern, object: *const c_char, b: FcBool) -> FcBool;
    pub fn FcPatternAddDouble(pattern: *mut FcPattern, object: *const c_char, d: f64) -> FcBool;
    pub fn FcPatternAddCharSet(
        pattern: *mut FcPattern,
        object: *const c_char,
        c: *const FcCharSet,
    ) -> FcBool;
    pub fn FcPatternGetString(
        pattern: *const FcPattern,
        object: *const c_char,
        n: c_int,
        s: *mut *mut FcChar8,
    ) -> FcResult;
    pub fn FcPatternGetInteger(
        pattern: *const FcPattern,
        object: *const c_char,
        n: c_int,
        i: *mut c_int,
    ) -> FcResult;
    pub fn FcPatternGetBool(
        pattern: *const FcPattern,
        object: *const c_char,
        n: c_int,
        b: *mut FcBool,
    ) -> FcResult;
    pub fn FcConfigSubstitute(
        config: *mut FcConfig,
        pattern: *mut FcPattern,
        kind: c_int,
    ) -> FcBool;
    pub fn FcDefaultSubstitute(pattern: *mut FcPattern);
    pub fn FcFontMatch(
        config: *mut FcConfig,
        pattern: *mut FcPattern,
        result: *mut FcResult,
    ) -> *mut FcPattern;
    pub fn FcFontSort(
        config: *mut FcConfig,
        pattern: *mut FcPattern,
        trim: FcBool,
        csp: *mut *mut FcCharSet,
        result: *mut FcResult,
    ) -> *mut FcFontSet;
    pub fn FcFontSetDestroy(fs: *mut FcFontSet);
    pub fn FcCharSetCreate() -> *mut FcCharSet;
    pub fn FcCharSetDestroy(cs: *mut FcCharSet);
    pub fn FcCharSetAddChar(cs: *mut FcCharSet, ucs4: FcChar32) -> FcBool;
    pub fn FcCharSetHasChar(cs: *const FcCharSet, ucs4: FcChar32) -> FcBool;
}

pub fn object_cstr(object: &'static [u8]) -> *const c_char {
    object.as_ptr().cast()
}

pub fn result_matches(result: FcResult) -> bool {
    result == FcResult::Match
}
