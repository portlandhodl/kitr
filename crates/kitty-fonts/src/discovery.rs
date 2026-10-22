// SPDX-License-Identifier: GPL-3.0-or-later
//! Font discovery through Fontconfig.
//!
//! Ports the matching portions of `kitty/fontconfig.c`: family + style
//! matching, PostScript/full-name matching and codepoint-coverage fallback
//! matching. Fonts are always identified by a file path plus face index, just
//! like kitty descriptors.

use std::ffi::CString;
use std::sync::OnceLock;

use crate::error::{Error, Result};
use crate::face::{Face, FaceDescriptor};
use crate::family::FontStyle;
use crate::ffi::fontconfig as fc;

#[derive(Clone, Debug, Default)]
pub struct FontQuery {
    pub family: Option<String>,
    pub style: FontStyle,
    pub monospace: bool,
    pub postscript_name: Option<String>,
    pub full_name: Option<String>,
    pub prefer_color: bool,
    pub allow_bitmapped: bool,
    pub size_pts: Option<f64>,
    pub dpi: Option<f64>,
    pub charset: Option<Vec<u32>>,
}

impl FontQuery {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn family(mut self, family: impl Into<String>) -> Self {
        self.family = Some(family.into());
        self
    }

    pub fn style(mut self, style: FontStyle) -> Self {
        self.style = style;
        self
    }

    pub fn monospace(mut self, yes: bool) -> Self {
        self.monospace = yes;
        self
    }

    pub fn postscript_name(mut self, name: impl Into<String>) -> Self {
        self.postscript_name = Some(name.into());
        self
    }

    pub fn full_name(mut self, name: impl Into<String>) -> Self {
        self.full_name = Some(name.into());
        self
    }

    pub fn prefer_color(mut self, yes: bool) -> Self {
        self.prefer_color = yes;
        self
    }

    pub fn allow_bitmapped(mut self, yes: bool) -> Self {
        self.allow_bitmapped = yes;
        self
    }

    pub fn size_pts(mut self, pts: f64) -> Self {
        self.size_pts = Some(pts);
        self
    }

    pub fn dpi(mut self, dpi: f64) -> Self {
        self.dpi = Some(dpi);
        self
    }

    pub fn charset(mut self, codepoints: impl IntoIterator<Item = u32>) -> Self {
        self.charset = Some(codepoints.into_iter().collect());
        self
    }
}

/// A Fontconfig match, before it is opened by FreeType.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MatchedFace {
    pub path: String,
    pub index: i32,
    pub hinting: bool,
    pub hint_style: i32,
    pub family: Option<String>,
    pub style: Option<String>,
    pub postscript_name: Option<String>,
    pub full_name: Option<String>,
    pub color: bool,
    pub monospace: bool,
    pub spacing: Option<i32>,
}

impl MatchedFace {
    pub fn descriptor(&self) -> FaceDescriptor {
        FaceDescriptor {
            path: self.path.clone().into(),
            index: self.index,
            hinting: self.hinting,
            hint_style: self.hint_style,
        }
    }

    pub fn load(&self, size_pts: f64, dpi_x: f64, dpi_y: f64) -> Result<Face> {
        self.descriptor().load(size_pts, dpi_x, dpi_y)
    }
}

/// Fontconfig handle. The library is initialized once per process.
#[derive(Clone, Copy, Debug, Default)]
pub struct Fontconfig;

impl Fontconfig {
    pub fn new() -> Result<Self> {
        static INIT: OnceLock<i32> = OnceLock::new();
        let ok = *INIT.get_or_init(|| unsafe { fc::FcInit() });
        if ok == 0 {
            Err(Error::Fontconfig("FcInit failed"))
        } else {
            Ok(Self)
        }
    }

    /// Match a family (or an explicit PostScript/full name) and style.
    pub fn match_font(&self, query: &FontQuery) -> Result<Option<MatchedFace>> {
        if query.family.is_none()
            && query.postscript_name.is_none()
            && query.full_name.is_none()
            && query.charset.is_none()
            && !query.monospace
        {
            return Err(Error::Fontconfig("empty font query"));
        }
        let pattern = Pattern::new()?;
        let mut charset = None;

        if let Some(family) = &query.family {
            pattern.add_string(fc::FC_FAMILY, family)?;
        }
        if query.monospace {
            pattern.add_string(fc::FC_FAMILY, "monospace")?;
            pattern.add_integer(fc::FC_SPACING, fc::FC_MONO)?;
        }
        if let Some(name) = &query.postscript_name {
            pattern.add_string(fc::FC_POSTSCRIPT_NAME, name)?;
        }
        if let Some(name) = &query.full_name {
            pattern.add_string(fc::FC_FULLNAME, name)?;
        }
        if query.style.bold() {
            pattern.add_integer(fc::FC_WEIGHT, fc::FC_WEIGHT_BOLD)?;
        }
        if query.style.italic() {
            pattern.add_integer(fc::FC_SLANT, fc::FC_SLANT_ITALIC)?;
        }
        if query.prefer_color {
            pattern.add_bool(fc::FC_COLOR, true)?;
        }
        if !query.allow_bitmapped {
            pattern.add_bool(fc::FC_OUTLINE, true)?;
            pattern.add_bool(fc::FC_SCALABLE, true)?;
        }
        if let Some(size) = query.size_pts {
            pattern.add_double(fc::FC_SIZE, size)?;
        }
        if let Some(dpi) = query.dpi {
            pattern.add_double(fc::FC_DPI, dpi)?;
        }
        if let Some(chars) = &query.charset {
            charset = Some(CharSet::new(chars)?);
            if let Some(cs) = &charset {
                pattern.add_charset(fc::FC_CHARSET, cs)?;
            }
        }

        let match_ = pattern.font_match()?;
        // The pattern may reference the charset, so destroy it first.
        drop(pattern);
        drop(charset);
        match match_ {
            None => Ok(None),
            Some(m) => Ok(Some(extract_match(&m))),
        }
    }

    pub fn match_family(
        &self,
        family: impl AsRef<str>,
        style: FontStyle,
    ) -> Result<Option<MatchedFace>> {
        self.match_font(&FontQuery::new().family(family.as_ref()).style(style))
    }

    pub fn match_postscript_name(&self, name: impl AsRef<str>) -> Result<Option<MatchedFace>> {
        self.match_font(&FontQuery::new().postscript_name(name.as_ref()))
    }

    /// Match a fallback font that covers `codepoint`, like kitty's
    /// `fallback_font()`.
    pub fn match_for_codepoint(
        &self,
        codepoint: u32,
        style: FontStyle,
        prefer_color: bool,
    ) -> Result<Option<MatchedFace>> {
        let mut query = FontQuery::new()
            .style(style)
            .prefer_color(prefer_color)
            .charset([codepoint]);
        if prefer_color {
            query = query.family("emoji");
        } else {
            query = query.monospace(true);
        }
        self.match_font(&query)
    }

    /// Match a font covering all supplied codepoints (cell fallback).
    pub fn match_for_codepoints(
        &self,
        codepoints: impl IntoIterator<Item = u32>,
        style: FontStyle,
        prefer_color: bool,
    ) -> Result<Option<MatchedFace>> {
        let mut query = FontQuery::new()
            .style(style)
            .prefer_color(prefer_color)
            .charset(codepoints);
        if prefer_color {
            query = query.family("emoji");
        } else {
            query = query.monospace(true);
        }
        self.match_font(&query)
    }

    pub fn default_monospace(&self, style: FontStyle) -> Result<Option<MatchedFace>> {
        self.match_font(
            &FontQuery::new()
                .family("monospace")
                .monospace(true)
                .style(style),
        )
    }
}

struct Pattern {
    ptr: *mut fc::FcPattern,
}

impl Pattern {
    fn new() -> Result<Self> {
        let ptr = unsafe { fc::FcPatternCreate() };
        if ptr.is_null() {
            return Err(Error::Fontconfig("FcPatternCreate failed"));
        }
        Ok(Self { ptr })
    }

    fn add_string(&self, object: &'static [u8], value: &str) -> Result<()> {
        let c = CString::new(value).map_err(|_| Error::InvalidString)?;
        let ok = unsafe {
            fc::FcPatternAddString(self.ptr, fc::object_cstr(object), c.as_ptr().cast::<u8>())
        };
        if ok == 0 {
            return Err(Error::Fontconfig("FcPatternAddString failed"));
        }
        Ok(())
    }

    fn add_integer(&self, object: &'static [u8], value: i32) -> Result<()> {
        let ok = unsafe { fc::FcPatternAddInteger(self.ptr, fc::object_cstr(object), value) };
        if ok == 0 {
            return Err(Error::Fontconfig("FcPatternAddInteger failed"));
        }
        Ok(())
    }

    fn add_bool(&self, object: &'static [u8], value: bool) -> Result<()> {
        let value = if value { fc::FC_TRUE } else { fc::FC_FALSE };
        let ok = unsafe { fc::FcPatternAddBool(self.ptr, fc::object_cstr(object), value) };
        if ok == 0 {
            return Err(Error::Fontconfig("FcPatternAddBool failed"));
        }
        Ok(())
    }

    fn add_double(&self, object: &'static [u8], value: f64) -> Result<()> {
        let ok = unsafe { fc::FcPatternAddDouble(self.ptr, fc::object_cstr(object), value) };
        if ok == 0 {
            return Err(Error::Fontconfig("FcPatternAddDouble failed"));
        }
        Ok(())
    }

    fn add_charset(&self, object: &'static [u8], charset: &CharSet) -> Result<()> {
        let ok = unsafe { fc::FcPatternAddCharSet(self.ptr, fc::object_cstr(object), charset.ptr) };
        if ok == 0 {
            return Err(Error::Fontconfig("FcPatternAddCharSet failed"));
        }
        Ok(())
    }

    fn font_match(&self) -> Result<Option<MatchedPattern>> {
        unsafe {
            fc::FcConfigSubstitute(std::ptr::null_mut(), self.ptr, fc::FC_MATCH_PATTERN);
            fc::FcDefaultSubstitute(self.ptr);
        }
        let mut result = fc::FcResult::NoMatch;
        let match_ = unsafe { fc::FcFontMatch(std::ptr::null_mut(), self.ptr, &mut result) };
        if match_.is_null() {
            return Ok(None);
        }
        Ok(Some(MatchedPattern { ptr: match_ }))
    }
}

impl Drop for Pattern {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe { fc::FcPatternDestroy(self.ptr) };
        }
    }
}

struct MatchedPattern {
    ptr: *mut fc::FcPattern,
}

impl Drop for MatchedPattern {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe { fc::FcPatternDestroy(self.ptr) };
        }
    }
}

struct CharSet {
    ptr: *mut fc::FcCharSet,
}

impl CharSet {
    fn new(codepoints: &[u32]) -> Result<Self> {
        let ptr = unsafe { fc::FcCharSetCreate() };
        if ptr.is_null() {
            return Err(Error::Fontconfig("FcCharSetCreate failed"));
        }
        for &cp in codepoints {
            unsafe { fc::FcCharSetAddChar(ptr, cp) };
        }
        Ok(Self { ptr })
    }
}

impl Drop for CharSet {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe { fc::FcCharSetDestroy(self.ptr) };
        }
    }
}

fn extract_match(pattern: &MatchedPattern) -> MatchedFace {
    let mut ans = MatchedFace {
        hinting: true,
        hint_style: 3,
        ..MatchedFace::default()
    };
    ans.path = get_string(pattern, fc::FC_FILE).unwrap_or_default();
    ans.family = get_string(pattern, fc::FC_FAMILY);
    ans.style = get_string(pattern, fc::FC_STYLE);
    ans.postscript_name = get_string(pattern, fc::FC_POSTSCRIPT_NAME);
    ans.full_name = get_string(pattern, fc::FC_FULLNAME);
    ans.index = get_integer(pattern, fc::FC_INDEX).unwrap_or(0);
    ans.hint_style = get_integer(pattern, fc::FC_HINT_STYLE).unwrap_or(3);
    ans.hinting = get_bool(pattern, fc::FC_HINTING).unwrap_or(true);
    ans.color = get_bool(pattern, fc::FC_COLOR).unwrap_or(false);
    ans.spacing = get_integer(pattern, fc::FC_SPACING);
    ans.monospace = ans.spacing == Some(fc::FC_MONO);
    ans
}

fn get_string(pattern: &MatchedPattern, object: &'static [u8]) -> Option<String> {
    let mut out: *mut fc::FcChar8 = std::ptr::null_mut();
    let result =
        unsafe { fc::FcPatternGetString(pattern.ptr, fc::object_cstr(object), 0, &mut out) };
    if result != fc::FcResult::Match || out.is_null() {
        return None;
    }
    let s = unsafe { std::ffi::CStr::from_ptr(out.cast()) };
    Some(s.to_string_lossy().into_owned())
}

fn get_integer(pattern: &MatchedPattern, object: &'static [u8]) -> Option<i32> {
    let mut out = 0;
    let result =
        unsafe { fc::FcPatternGetInteger(pattern.ptr, fc::object_cstr(object), 0, &mut out) };
    if result == fc::FcResult::Match {
        Some(out)
    } else {
        None
    }
}

fn get_bool(pattern: &MatchedPattern, object: &'static [u8]) -> Option<bool> {
    let mut out = 0;
    let result = unsafe { fc::FcPatternGetBool(pattern.ptr, fc::object_cstr(object), 0, &mut out) };
    if result == fc::FcResult::Match {
        Some(out != 0)
    } else {
        None
    }
}
