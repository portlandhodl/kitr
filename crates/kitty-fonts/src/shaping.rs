// SPDX-License-Identifier: GPL-3.0-or-later
//! HarfBuzz shaping and a small shaped-run cache.
//!
//! Ports the shaping core of `kitty/fonts.c` (`load_hb_buffer()` and
//! `shape()`), plus a simplified version of `kitty/shaped-run-cache.c`.

use std::collections::HashMap;
use std::ffi::CString;
use std::fmt;

use crate::error::{Error, Result};
use crate::face::Face;
use crate::ffi::harfbuzz::{self, hb_font_t};

/// A HarfBuzz feature (`kern`, `ss01`, `-calt`, ...).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Feature {
    pub tag: u32,
    pub value: u32,
    pub start: u32,
    pub end: u32,
}

impl Feature {
    pub fn parse(spec: &str) -> Result<Self> {
        let c = CString::new(spec).map_err(|_| Error::InvalidString)?;
        let mut feature = harfbuzz::hb_feature_t::default();
        let ok = unsafe { harfbuzz::hb_feature_from_string(c.as_ptr(), -1, &mut feature) };
        if ok == 0 {
            return Err(Error::InvalidFeature(spec.to_string()));
        }
        Ok(Self {
            tag: feature.tag,
            value: feature.value,
            start: feature.start,
            end: feature.end,
        })
    }

    fn as_hb(self) -> harfbuzz::hb_feature_t {
        harfbuzz::hb_feature_t {
            tag: self.tag,
            value: self.value,
            start: self.start,
            end: self.end,
        }
    }
}

impl fmt::Display for Feature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut raw = self.as_hb();
        let mut buf = [0 as core::ffi::c_char; 128];
        let n =
            unsafe { harfbuzz::hb_feature_to_string(&mut raw, buf.as_mut_ptr(), buf.len() as u32) };
        if n == 0 {
            let tag = self.tag.to_be_bytes();
            write!(
                f,
                "{}{}{}{}={}",
                tag[0] as char, tag[1] as char, tag[2] as char, tag[3] as char, self.value
            )
        } else {
            let cstr = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) };
            f.write_str(&cstr.to_string_lossy())
        }
    }
}

/// HarfBuzz cluster level. kitty uses `MONOTONE_CHARACTERS`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ClusterLevel {
    MonotoneGraphemes,
    #[default]
    MonotoneCharacters,
    Characters,
}

impl ClusterLevel {
    fn as_hb(self) -> i32 {
        match self {
            Self::MonotoneGraphemes => harfbuzz::HB_BUFFER_CLUSTER_LEVEL_MONOTONE_GRAPHEMES,
            Self::MonotoneCharacters => harfbuzz::HB_BUFFER_CLUSTER_LEVEL_MONOTONE_CHARACTERS,
            Self::Characters => harfbuzz::HB_BUFFER_CLUSTER_LEVEL_CHARACTERS,
        }
    }
}

/// Options for one shaping call.
#[derive(Clone, Debug, Default)]
pub struct ShapeOptions {
    pub features: Vec<Feature>,
    pub force_ltr: bool,
    /// Mirrors kitty's `disable_ligature` flag: appends `-calt`.
    pub disable_ligature: bool,
    pub cluster_level: ClusterLevel,
}

/// One output glyph.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ShapedGlyph {
    pub glyph_id: u32,
    pub cluster: u32,
    pub x_advance: i32,
    pub y_advance: i32,
    pub x_offset: i32,
    pub y_offset: i32,
}

impl ShapedGlyph {
    pub fn x_advance_px(self) -> u32 {
        ceil_div_64(self.x_advance)
    }
}

/// Output of shaping one UTF-32 run.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShapedRun {
    pub glyphs: Vec<ShapedGlyph>,
    pub num_codepoints: usize,
    pub cluster_level: ClusterLevel,
}

impl ShapedRun {
    pub fn total_advance_26_6(&self) -> i32 {
        self.glyphs.iter().map(|g| g.x_advance).sum()
    }

    pub fn total_advance_px(&self) -> u32 {
        ceil_div_64(self.total_advance_26_6())
    }

    pub fn clusters(&self) -> Vec<u32> {
        self.glyphs.iter().map(|g| g.cluster).collect()
    }
}

struct HbBuffer {
    ptr: *mut harfbuzz::hb_buffer_t,
}

impl HbBuffer {
    fn new() -> Result<Self> {
        let ptr = unsafe { harfbuzz::hb_buffer_create() };
        if ptr.is_null() {
            return Err(Error::FreeType {
                operation: "hb_buffer_create",
                code: 0,
            });
        }
        Ok(Self { ptr })
    }
}

impl Drop for HbBuffer {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe { harfbuzz::hb_buffer_destroy(self.ptr) };
        }
    }
}

/// Shape a UTF-32 run with a face's HarfBuzz font.
pub fn shape(face: &Face, text: &[u32], options: &ShapeOptions) -> Result<ShapedRun> {
    // SAFETY: `face` owns a live `hb_font_t` for as long as the borrow exists.
    unsafe { shape_raw(face.hb_font_ptr(), text, options) }
}

/// Shape a UTF-32 run with a raw HarfBuzz font.
///
/// # Safety
///
/// `font` must be a valid `hb_font_t` that outlives the call.
pub(crate) unsafe fn shape_raw(
    font: *mut hb_font_t,
    text: &[u32],
    options: &ShapeOptions,
) -> Result<ShapedRun> {
    if font.is_null() {
        return Err(Error::FreeType {
            operation: "hb_shape",
            code: 0,
        });
    }
    let buffer = HbBuffer::new()?;
    let mut features: Vec<harfbuzz::hb_feature_t> =
        options.features.iter().map(|f| f.as_hb()).collect();
    if options.disable_ligature {
        features.push(Feature::parse("-calt")?.as_hb());
    }
    unsafe {
        harfbuzz::hb_buffer_clear_contents(buffer.ptr);
        harfbuzz::hb_buffer_add_utf32(
            buffer.ptr,
            text.as_ptr(),
            text.len() as i32,
            0,
            text.len() as i32,
        );
        harfbuzz::hb_buffer_guess_segment_properties(buffer.ptr);
        harfbuzz::hb_buffer_set_cluster_level(buffer.ptr, options.cluster_level.as_hb());
        if options.force_ltr {
            harfbuzz::hb_buffer_set_direction(buffer.ptr, harfbuzz::HB_DIRECTION_LTR);
        }
        harfbuzz::hb_shape(font, buffer.ptr, features.as_ptr(), features.len() as u32);
    }

    let mut info_len = 0u32;
    let mut pos_len = 0u32;
    let infos = unsafe { harfbuzz::hb_buffer_get_glyph_infos(buffer.ptr, &mut info_len) };
    let positions = unsafe { harfbuzz::hb_buffer_get_glyph_positions(buffer.ptr, &mut pos_len) };
    let count = info_len.min(pos_len) as usize;
    let mut glyphs = Vec::with_capacity(count);
    for i in 0..count {
        let info = unsafe { *infos.add(i) };
        let pos = unsafe { *positions.add(i) };
        glyphs.push(ShapedGlyph {
            glyph_id: info.codepoint,
            cluster: info.cluster,
            x_advance: pos.x_advance,
            y_advance: pos.y_advance,
            x_offset: pos.x_offset,
            y_offset: pos.y_offset,
        });
    }
    Ok(ShapedRun {
        glyphs,
        num_codepoints: text.len(),
        cluster_level: options.cluster_level,
    })
}

/// Limits mirroring `kitty/shaped-run-cache.c`.
pub const SHAPED_RUN_MAX_CELLS: usize = 32;
pub const SHAPED_RUN_MAX_ENTRIES: usize = 2048;
pub const SHAPED_RUN_MAX_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct CacheKey {
    font_address: usize,
    path: String,
    index: i32,
    size_bits: u64,
    dpi_x_bits: u64,
    dpi_y_bits: u64,
    text: Vec<u32>,
    features: Vec<Feature>,
    disable_ligature: bool,
    force_ltr: bool,
    cluster_level: ClusterLevel,
}

impl CacheKey {
    fn new(face: &Face, text: &[u32], options: &ShapeOptions) -> Self {
        let (dpi_x, dpi_y) = face.dpi();
        Self {
            font_address: face as *const Face as usize,
            path: face.path().to_string_lossy().into_owned(),
            index: face.index(),
            size_bits: face.point_size().to_bits(),
            dpi_x_bits: dpi_x.to_bits(),
            dpi_y_bits: dpi_y.to_bits(),
            text: text.to_vec(),
            features: options.features.clone(),
            disable_ligature: options.disable_ligature,
            force_ltr: options.force_ltr,
            cluster_level: options.cluster_level,
        }
    }

    fn estimate_bytes(&self, run: &ShapedRun) -> usize {
        std::mem::size_of::<Self>()
            + self.path.len()
            + self.text.len() * std::mem::size_of::<u32>()
            + self.features.len() * std::mem::size_of::<Feature>()
            + run.glyphs.len() * std::mem::size_of::<ShapedGlyph>()
    }
}

/// A bounded shaped-run cache, modelled on `kitty/shaped-run-cache.c`.
///
/// Difference from kitty: the cache is not per-face and the key is a
/// `HashMap` key rather than a packed cell buffer. Runs longer than
/// [`SHAPED_RUN_MAX_CELLS`] codepoints are shaped but not stored; when the
/// entry or byte limits are reached the whole cache is dropped, just like
/// kitty's `shaped_run_drop()`.
#[derive(Default, Debug)]
pub struct ShapedRunCache {
    entries: HashMap<CacheKey, (ShapedRun, usize)>,
    bytes: usize,
    hits: u64,
    misses: u64,
}

impl ShapedRunCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn shape(
        &mut self,
        face: &Face,
        text: &[u32],
        options: &ShapeOptions,
    ) -> Result<ShapedRun> {
        let key = CacheKey::new(face, text, options);
        if let Some((run, _)) = self.entries.get(&key) {
            self.hits += 1;
            return Ok(run.clone());
        }
        self.misses += 1;
        let run = face.shape(text, options)?;
        if text.len() <= SHAPED_RUN_MAX_CELLS {
            let bytes = key.estimate_bytes(&run);
            if self.entries.len() + 1 > SHAPED_RUN_MAX_ENTRIES
                || self.bytes + bytes > SHAPED_RUN_MAX_BYTES
            {
                self.clear();
            }
            self.entries.insert(key, (run.clone(), bytes));
            self.bytes += bytes;
        }
        Ok(run)
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.bytes = 0;
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn hits(&self) -> u64 {
        self.hits
    }

    pub fn misses(&self) -> u64 {
        self.misses
    }
}

fn ceil_div_64(value: i32) -> u32 {
    if value <= 0 {
        0
    } else {
        ((value as i64 + 63) / 64) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_features() {
        let f = Feature::parse("-dlig").unwrap();
        assert_eq!(f.tag, u32::from_be_bytes(*b"dlig"));
        assert_eq!(f.value, 0);
        let f = Feature::parse("ss01=2").unwrap();
        assert_eq!(f.value, 2);
        assert!(Feature::parse("not-a-feature")
            .unwrap_err()
            .to_string()
            .contains("invalid"));
    }

    #[test]
    fn feature_display_round_trips() {
        assert_eq!(Feature::parse("-dlig").unwrap().to_string(), "-dlig");
        assert_eq!(Feature::parse("ss01=2").unwrap().to_string(), "ss01=2");
    }
}
