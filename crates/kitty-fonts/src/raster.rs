// SPDX-License-Identifier: GPL-3.0-or-later
//! Glyph rasterization and a bounded raster cache.
//!
//! This is the part of kitty's `freetype.c` that turns a glyph into a
//! bitmap for the renderer's glyph atlas. Rendering to an actual GPU atlas is
//! deliberately outside this crate.

use std::collections::HashMap;
use std::fmt;

use crate::error::{Error, Result};
use crate::face::Face;

/// Pixel layout of a [`GlyphBitmap`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PixelFormat {
    /// One byte per pixel, 0 = transparent, 255 = opaque.
    Gray8,
    /// Four bytes per pixel in R, G, B, A order.
    Rgba8,
    /// Four bytes per pixel in B, G, R, A order (FreeType color bitmaps).
    Bgra8,
}

impl PixelFormat {
    pub const fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Gray8 => 1,
            Self::Rgba8 | Self::Bgra8 => 4,
        }
    }
}

impl fmt::Display for PixelFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Gray8 => "Gray8",
            Self::Rgba8 => "Rgba8",
            Self::Bgra8 => "Bgra8",
        })
    }
}

/// An owned, tightly packed glyph bitmap.
///
/// `rows` are stored top-to-bottom, each `pitch` bytes long. `pitch` is
/// always at least `width * format.bytes_per_pixel()`; it is currently exactly
/// that (no alignment padding), but callers should use it as the stride.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GlyphBitmap {
    pub width: u32,
    pub height: u32,
    /// Horizontal bearing of the bitmap relative to the pen origin.
    pub left: i32,
    /// Vertical bearing of the bitmap relative to the pen origin.
    pub top: i32,
    /// Horizontal advance in whole pixels (FreeType 26.6 value rounded up).
    pub advance_x: i32,
    /// Vertical advance in whole pixels (FreeType 26.6 value rounded up).
    pub advance_y: i32,
    /// Raw FreeType horizontal advance in 26.6 pixels.
    pub advance_x_26_6: i32,
    /// Raw FreeType vertical advance in 26.6 pixels.
    pub advance_y_26_6: i32,
    pub format: PixelFormat,
    /// Row stride of `data`, in bytes.
    pub pitch: usize,
    /// Owned pixels, `pitch * height` bytes.
    pub data: Vec<u8>,
}

impl GlyphBitmap {
    pub fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// Alias for [`GlyphBitmap::pitch`].
    pub fn stride(&self) -> usize {
        self.pitch
    }

    pub fn bytes_per_pixel(&self) -> usize {
        self.format.bytes_per_pixel()
    }

    pub fn row(&self, y: u32) -> Option<&[u8]> {
        if y >= self.height {
            return None;
        }
        let start = y as usize * self.pitch;
        Some(&self.data[start..start + self.pitch])
    }
}

/// A bounded LRU-less raster cache, keyed by face identity, glyph id, size,
/// resolution and load flags.
///
/// Like the shaped-run cache, the whole cache is dropped when either the
/// entry or byte limit is reached. Zero-size bitmaps (spaces) are cached too.
pub const RASTER_CACHE_MAX_ENTRIES: usize = 1024;
pub const RASTER_CACHE_MAX_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct CacheKey {
    font_address: usize,
    path: String,
    index: i32,
    size_bits: u64,
    dpi_x_bits: u64,
    dpi_y_bits: u64,
    color: bool,
    glyph_id: u32,
    flags: i32,
}

impl CacheKey {
    fn new(face: &Face, glyph_id: u32, flags: i32) -> Self {
        let (dpi_x, dpi_y) = face.dpi();
        Self {
            font_address: face as *const Face as usize,
            path: face.path().to_string_lossy().into_owned(),
            index: face.index(),
            size_bits: face.point_size().to_bits(),
            dpi_x_bits: dpi_x.to_bits(),
            dpi_y_bits: dpi_y.to_bits(),
            color: face.has_color(),
            glyph_id,
            flags,
        }
    }
}

#[derive(Default, Debug)]
pub struct RasterCache {
    entries: HashMap<CacheKey, (GlyphBitmap, usize)>,
    bytes: usize,
    hits: u64,
    misses: u64,
}

impl RasterCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Rasterize a glyph with the default flags
    /// (`FT_LOAD_DEFAULT | FT_LOAD_RENDER`, plus color loading for color faces).
    pub fn rasterize(&mut self, face: &Face, glyph_id: u32) -> Result<GlyphBitmap> {
        self.rasterize_with_flags(
            face,
            glyph_id,
            crate::ffi::freetype::FT_LOAD_DEFAULT | crate::ffi::freetype::FT_LOAD_RENDER,
        )
    }

    pub fn rasterize_with_flags(
        &mut self,
        face: &Face,
        glyph_id: u32,
        flags: i32,
    ) -> Result<GlyphBitmap> {
        let key = CacheKey::new(face, glyph_id, flags);
        if let Some((bitmap, _)) = self.entries.get(&key) {
            self.hits += 1;
            return Ok(bitmap.clone());
        }
        self.misses += 1;
        let bitmap = face.rasterize_glyph_with_flags(glyph_id, flags)?;
        let bytes = std::mem::size_of::<CacheKey>()
            + key.path.len()
            + std::mem::size_of::<GlyphBitmap>()
            + bitmap.data.len();
        if self.entries.len() + 1 > RASTER_CACHE_MAX_ENTRIES
            || self.bytes + bytes > RASTER_CACHE_MAX_BYTES
        {
            self.clear();
        }
        self.entries.insert(key, (bitmap.clone(), bytes));
        self.bytes += bytes;
        Ok(bitmap)
    }

    /// Rasterize a codepoint using this face directly. Missing codepoints are
    /// reported as [`Error::MissingGlyph`].
    pub fn rasterize_codepoint(&mut self, face: &Face, codepoint: u32) -> Result<GlyphBitmap> {
        let glyph_id = face.glyph_id(codepoint);
        if glyph_id == 0 {
            return Err(Error::MissingGlyph(codepoint));
        }
        self.rasterize(face, glyph_id)
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
