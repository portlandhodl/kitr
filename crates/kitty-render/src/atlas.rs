// SPDX-License-Identifier: GPL-3.0-or-later
//! CPU glyph atlas packing.
//!
//! Glyphs are packed into fixed-size pages using a deterministic shelf
//! allocator: the first shelf that fits is used, otherwise a new shelf is
//! opened at the bottom of the page, and finally a new page is created. Gray8
//! (alpha masks) and RGBA8 (colour glyphs) pages are kept separately, mirroring
//! the two texture formats used by kitty's sprite atlas.
//!
//! The atlas never touches the GPU. [`GlyphRasterizer`] is the bridge to
//! `kitty-fonts`: the renderer asks it for a bitmap when a key is not cached,
//! then copies that bitmap into a page and hands the resulting [`GlyphSlot`]
//! to the instance generator. Page creation and eviction are reported through
//! [`AtlasEvent`], so a backend can upload or discard textures incrementally.

use std::collections::HashMap;

/// Maximum number of codepoints stored in a single glyph key. This matches
/// `MAX_NUM_CODEPOINTS_PER_CELL` from `kitty/line.h`.
pub const MAX_GLYPH_CODEPOINTS: usize = 24;

/// Pixel format of an atlas page.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AtlasFormat {
    /// One byte per pixel, used for monochrome glyph masks and decorations.
    Gray8,
    /// Four bytes per pixel, used for colour glyphs.
    Rgba8,
}

impl AtlasFormat {
    #[inline]
    pub const fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Gray8 => 1,
            Self::Rgba8 => 4,
        }
    }
}

/// Atlas configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AtlasConfig {
    pub page_width: u32,
    pub page_height: u32,
    /// Gap in pixels inserted between glyphs on a shelf and between shelves.
    pub padding: u32,
    pub max_gray_pages: usize,
    pub max_rgba_pages: usize,
    /// When a page limit is reached, evict the least recently used page of
    /// that format instead of failing.
    pub allow_eviction: bool,
}

impl Default for AtlasConfig {
    fn default() -> Self {
        Self {
            page_width: 1024,
            page_height: 1024,
            padding: 1,
            max_gray_pages: 64,
            max_rgba_pages: 64,
            allow_eviction: true,
        }
    }
}

impl AtlasConfig {
    /// A configuration with the given page size.
    pub const fn with_page_size(width: u32, height: u32) -> Self {
        Self {
            page_width: width,
            page_height: height,
            padding: 1,
            max_gray_pages: 64,
            max_rgba_pages: 64,
            allow_eviction: true,
        }
    }

    #[inline]
    pub const fn max_pages(self, format: AtlasFormat) -> usize {
        match format {
            AtlasFormat::Gray8 => self.max_gray_pages,
            AtlasFormat::Rgba8 => self.max_rgba_pages,
        }
    }
}

/// The text associated with a glyph key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GlyphText {
    len: u8,
    chars: [u32; MAX_GLYPH_CODEPOINTS],
}

impl Default for GlyphText {
    fn default() -> Self {
        Self {
            len: 0,
            chars: [0; MAX_GLYPH_CODEPOINTS],
        }
    }
}

impl GlyphText {
    pub fn new(chars: &[u32]) -> Self {
        let mut ans = Self::default();
        let n = chars.len().min(MAX_GLYPH_CODEPOINTS);
        ans.chars[..n].copy_from_slice(&chars[..n]);
        ans.len = n as u8;
        ans
    }

    #[inline]
    pub fn as_slice(&self) -> &[u32] {
        &self.chars[..self.len as usize]
    }

    #[inline]
    pub const fn len(&self) -> usize {
        self.len as usize
    }

    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// Cache key for a rasterized glyph.
///
/// The key is intentionally independent of `kitty-fonts` types: a font stack
/// adapter can populate `font_id`, `glyph_id` and `flags` after shaping while
/// retaining the cell text for fallback and colour-glyph lookup.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GlyphKey {
    pub font_id: u16,
    pub glyph_id: u32,
    pub flags: u32,
    pub text: GlyphText,
}

impl GlyphKey {
    pub const fn new(font_id: u16, glyph_id: u32, flags: u32) -> Self {
        Self {
            font_id,
            glyph_id,
            flags,
            text: GlyphText {
                len: 0,
                chars: [0; MAX_GLYPH_CODEPOINTS],
            },
        }
    }

    /// Build a key from cell text. `glyph_id` defaults to the first codepoint;
    /// a shaping adapter should replace it with the resolved glyph index.
    pub fn from_codepoints(font_id: u16, chars: &[u32], flags: u32) -> Self {
        Self {
            font_id,
            glyph_id: chars.first().copied().unwrap_or(0),
            flags,
            text: GlyphText::new(chars),
        }
    }

    pub fn with_glyph_id(mut self, glyph_id: u32) -> Self {
        self.glyph_id = glyph_id;
        self
    }
}

/// A rasterized bitmap ready to be copied into an atlas page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GlyphBitmap {
    pub width: u32,
    pub height: u32,
    pub format: AtlasFormat,
    /// Tightly packed rows, `width * bytes_per_pixel` bytes per row.
    pub data: Vec<u8>,
}

impl GlyphBitmap {
    pub fn new(
        width: u32,
        height: u32,
        format: AtlasFormat,
        data: Vec<u8>,
    ) -> Result<Self, AtlasError> {
        let bpp = format.bytes_per_pixel();
        let expected = (width as usize)
            .checked_mul(height as usize)
            .and_then(|v| v.checked_mul(bpp))
            .ok_or(AtlasError::InvalidBitmap {
                reason: "bitmap dimensions overflow",
            })?;
        if data.len() != expected {
            return Err(AtlasError::InvalidBitmap {
                reason: "bitmap data length does not match dimensions and format",
            });
        }
        Ok(Self {
            width,
            height,
            format,
            data,
        })
    }

    pub fn gray(width: u32, height: u32, data: Vec<u8>) -> Result<Self, AtlasError> {
        Self::new(width, height, AtlasFormat::Gray8, data)
    }

    pub fn rgba(width: u32, height: u32, data: Vec<u8>) -> Result<Self, AtlasError> {
        Self::new(width, height, AtlasFormat::Rgba8, data)
    }

    #[inline]
    pub fn stride(&self) -> usize {
        self.width as usize * self.format.bytes_per_pixel()
    }
}

/// Supplies rasterized bitmaps to the atlas/renderer.
///
/// The trait is object safe so [`crate::RendererCore`] can hold a
/// `&mut dyn GlyphRasterizer`. A `kitty-fonts` adapter is expected to implement
/// this later; for tests and demos the blanket implementation for closures is
/// convenient.
pub trait GlyphRasterizer {
    fn rasterize(&mut self, glyph: &GlyphKey) -> Option<GlyphBitmap>;
}

impl<F> GlyphRasterizer for F
where
    F: FnMut(&GlyphKey) -> Option<GlyphBitmap>,
{
    fn rasterize(&mut self, glyph: &GlyphKey) -> Option<GlyphBitmap> {
        self(glyph)
    }
}

/// Normalized texture coordinates of a glyph.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct UvRect {
    pub u0: f32,
    pub v0: f32,
    pub u1: f32,
    pub v1: f32,
}

impl UvRect {
    pub fn from_pixels(
        x: u32,
        y: u32,
        width: u32,
        height: u32,
        page_width: u32,
        page_height: u32,
    ) -> Self {
        if page_width == 0 || page_height == 0 {
            return Self::default();
        }
        Self {
            u0: x as f32 / page_width as f32,
            v0: y as f32 / page_height as f32,
            u1: (x + width) as f32 / page_width as f32,
            v1: (y + height) as f32 / page_height as f32,
        }
    }
}

/// The location of a glyph in the atlas.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GlyphSlot {
    pub page: u32,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub format: AtlasFormat,
    pub uv: UvRect,
}

impl GlyphSlot {
    #[inline]
    pub fn pixel_rect(&self) -> crate::geom::Rect {
        crate::geom::Rect::new(
            self.x as f32,
            self.y as f32,
            self.width as f32,
            self.height as f32,
        )
    }
}

/// Events a backend needs to keep its GPU pages in sync.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AtlasEvent {
    /// A new empty page was created.
    NewPage { page: u32, format: AtlasFormat },
    /// An old page was reused, dropping all of its glyphs.
    EvictedPage {
        page: u32,
        format: AtlasFormat,
        glyphs: usize,
    },
    /// A page could not be allocated because the configured limit was reached.
    Full { format: AtlasFormat, pages: usize },
}

/// Result of inserting a glyph.
#[derive(Clone, Debug, PartialEq)]
pub struct AtlasInsert {
    pub slot: GlyphSlot,
    pub event: Option<AtlasEvent>,
    pub already_present: bool,
}

/// Atlas errors.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AtlasError {
    InvalidBitmap {
        reason: &'static str,
    },
    GlyphTooLarge {
        width: u32,
        height: u32,
        page_width: u32,
        page_height: u32,
    },
    Full {
        format: AtlasFormat,
        pages: usize,
    },
}

impl core::fmt::Display for AtlasError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::InvalidBitmap { reason } => write!(f, "invalid glyph bitmap: {reason}"),
            Self::GlyphTooLarge {
                width,
                height,
                page_width,
                page_height,
            } => write!(
                f,
                "glyph {width}x{height} does not fit in a {page_width}x{page_height} atlas page"
            ),
            Self::Full { format, pages } => {
                write!(f, "{format:?} atlas is full ({pages} pages)")
            }
        }
    }
}

impl std::error::Error for AtlasError {}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Shelf {
    y: u32,
    height: u32,
    x: u32,
}

/// One atlas page. Pixels are stored tightly packed in [`AtlasFormat`].
#[derive(Clone, Debug)]
pub struct AtlasPage {
    format: AtlasFormat,
    width: u32,
    height: u32,
    data: Vec<u8>,
    shelves: Vec<Shelf>,
    next_y: u32,
    keys: Vec<GlyphKey>,
    last_used: u64,
}

impl AtlasPage {
    fn new(format: AtlasFormat, width: u32, height: u32) -> Self {
        let len = width as usize * height as usize * format.bytes_per_pixel();
        Self {
            format,
            width,
            height,
            data: vec![0; len],
            shelves: Vec::new(),
            next_y: 0,
            keys: Vec::new(),
            last_used: 0,
        }
    }

    #[inline]
    pub const fn format(&self) -> AtlasFormat {
        self.format
    }

    #[inline]
    pub const fn width(&self) -> u32 {
        self.width
    }

    #[inline]
    pub const fn height(&self) -> u32 {
        self.height
    }

    #[inline]
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    #[inline]
    pub fn glyph_count(&self) -> usize {
        self.keys.len()
    }

    fn reset(&mut self) {
        self.data.fill(0);
        self.shelves.clear();
        self.next_y = 0;
        self.keys.clear();
    }

    fn allocate(&mut self, width: u32, height: u32, padding: u32) -> Option<(u32, u32)> {
        for shelf in &mut self.shelves {
            if height <= shelf.height && shelf.x + width + padding <= self.width {
                let x = shelf.x;
                shelf.x += width + padding;
                return Some((x, shelf.y));
            }
        }
        if self.next_y + height + padding <= self.height {
            let y = self.next_y;
            self.shelves.push(Shelf {
                y,
                height,
                x: width + padding,
            });
            self.next_y = y + height + padding;
            Some((0, y))
        } else {
            None
        }
    }

    fn copy_bitmap(&mut self, bitmap: &GlyphBitmap, x: u32, y: u32) {
        let bpp = self.format.bytes_per_pixel();
        let src_stride = bitmap.stride();
        let dst_stride = self.width as usize * bpp;
        for row in 0..bitmap.height as usize {
            let src = &bitmap.data[row * src_stride..(row + 1) * src_stride];
            let dst_start = (y as usize + row) * dst_stride + x as usize * bpp;
            self.data[dst_start..dst_start + src_stride].copy_from_slice(src);
        }
    }
}

/// Statistics accumulated by a [`GlyphAtlas`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AtlasStats {
    pub insertions: u64,
    pub cache_hits: u64,
    pub pages_created: u64,
    pub pages_evicted: u64,
    pub full_page_errors: u64,
}

/// A deterministic shelf-packing glyph atlas.
#[derive(Clone, Debug)]
pub struct GlyphAtlas {
    config: AtlasConfig,
    pages: Vec<AtlasPage>,
    entries: HashMap<GlyphKey, GlyphSlot>,
    clock: u64,
    stats: AtlasStats,
}

impl Default for GlyphAtlas {
    fn default() -> Self {
        Self::new(AtlasConfig::default())
    }
}

impl GlyphAtlas {
    pub fn new(config: AtlasConfig) -> Self {
        assert!(config.page_width > 0 && config.page_height > 0);
        Self {
            config,
            pages: Vec::new(),
            entries: HashMap::new(),
            clock: 0,
            stats: AtlasStats::default(),
        }
    }

    #[inline]
    pub const fn config(&self) -> AtlasConfig {
        self.config
    }

    #[inline]
    pub fn stats(&self) -> AtlasStats {
        self.stats
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[inline]
    pub fn pages(&self) -> &[AtlasPage] {
        &self.pages
    }

    #[inline]
    pub fn page(&self, index: usize) -> Option<&AtlasPage> {
        self.pages.get(index)
    }

    #[inline]
    pub fn page_count(&self, format: AtlasFormat) -> usize {
        self.pages.iter().filter(|p| p.format == format).count()
    }

    #[inline]
    pub fn lookup(&self, key: &GlyphKey) -> Option<&GlyphSlot> {
        self.entries.get(key)
    }

    pub fn entries(&self) -> impl Iterator<Item = (&GlyphKey, &GlyphSlot)> {
        self.entries.iter()
    }

    /// Insert a bitmap. The bitmap is copied into the page.
    pub fn insert(
        &mut self,
        key: GlyphKey,
        bitmap: GlyphBitmap,
    ) -> Result<AtlasInsert, AtlasError> {
        if let Some(slot) = self.entries.get(&key).copied() {
            self.stats.cache_hits += 1;
            self.pages[slot.page as usize].last_used = self.clock;
            return Ok(AtlasInsert {
                slot,
                event: None,
                already_present: true,
            });
        }
        if bitmap.width == 0 || bitmap.height == 0 {
            return Err(AtlasError::InvalidBitmap {
                reason: "zero sized bitmap",
            });
        }
        if bitmap.width > self.config.page_width || bitmap.height > self.config.page_height {
            return Err(AtlasError::GlyphTooLarge {
                width: bitmap.width,
                height: bitmap.height,
                page_width: self.config.page_width,
                page_height: self.config.page_height,
            });
        }
        let expected = bitmap
            .width
            .checked_mul(bitmap.height)
            .and_then(|v| (v as usize).checked_mul(bitmap.format.bytes_per_pixel()))
            .ok_or(AtlasError::InvalidBitmap {
                reason: "bitmap dimensions overflow",
            })?;
        if bitmap.data.len() != expected {
            return Err(AtlasError::InvalidBitmap {
                reason: "bitmap data length does not match dimensions and format",
            });
        }

        self.clock += 1;
        let format = bitmap.format;
        let (page_index, x, y) = match self.find_fit(format, bitmap.width, bitmap.height) {
            Some(v) => (v.0, v.1, v.2),
            None => {
                let pages = self.page_count(format);
                let event;
                let page_index;
                if pages < self.config.max_pages(format) {
                    page_index = self.create_page(format);
                    event = Some(AtlasEvent::NewPage {
                        page: page_index as u32,
                        format,
                    });
                } else if self.config.allow_eviction {
                    let (idx, glyphs) = self
                        .evict_oldest_page(format)
                        .ok_or(AtlasError::Full { format, pages })?;
                    page_index = idx;
                    event = Some(AtlasEvent::EvictedPage {
                        page: idx as u32,
                        format,
                        glyphs,
                    });
                } else {
                    self.stats.full_page_errors += 1;
                    return Err(AtlasError::Full { format, pages });
                }
                let (x, y) = self.pages[page_index]
                    .allocate(bitmap.width, bitmap.height, self.config.padding)
                    .ok_or(AtlasError::GlyphTooLarge {
                        width: bitmap.width,
                        height: bitmap.height,
                        page_width: self.config.page_width,
                        page_height: self.config.page_height,
                    })?;
                let slot = self.make_slot(page_index, x, y, &bitmap);
                let page = &mut self.pages[page_index];
                page.copy_bitmap(&bitmap, x, y);
                page.keys.push(key);
                page.last_used = self.clock;
                self.entries.insert(key, slot);
                self.stats.insertions += 1;
                return Ok(AtlasInsert {
                    slot,
                    event,
                    already_present: false,
                });
            }
        };

        let slot = self.make_slot(page_index, x, y, &bitmap);
        let page = &mut self.pages[page_index];
        page.copy_bitmap(&bitmap, x, y);
        page.keys.push(key);
        page.last_used = self.clock;
        self.entries.insert(key, slot);
        self.stats.insertions += 1;
        Ok(AtlasInsert {
            slot,
            event: None,
            already_present: false,
        })
    }

    /// Convenience wrapper for a tightly packed bitmap held as a slice.
    pub fn insert_bitmap(
        &mut self,
        key: GlyphKey,
        width: u32,
        height: u32,
        format: AtlasFormat,
        data: &[u8],
    ) -> Result<AtlasInsert, AtlasError> {
        self.insert(key, GlyphBitmap::new(width, height, format, data.to_vec())?)
    }

    fn make_slot(&self, page_index: usize, x: u32, y: u32, bitmap: &GlyphBitmap) -> GlyphSlot {
        let page = &self.pages[page_index];
        GlyphSlot {
            page: page_index as u32,
            x,
            y,
            width: bitmap.width,
            height: bitmap.height,
            format: bitmap.format,
            uv: UvRect::from_pixels(x, y, bitmap.width, bitmap.height, page.width, page.height),
        }
    }

    fn find_fit(
        &mut self,
        format: AtlasFormat,
        width: u32,
        height: u32,
    ) -> Option<(usize, u32, u32)> {
        for (i, page) in self.pages.iter_mut().enumerate() {
            if page.format == format {
                if let Some((x, y)) = page.allocate(width, height, self.config.padding) {
                    return Some((i, x, y));
                }
            }
        }
        None
    }

    fn create_page(&mut self, format: AtlasFormat) -> usize {
        let index = self.pages.len();
        self.pages.push(AtlasPage::new(
            format,
            self.config.page_width,
            self.config.page_height,
        ));
        self.stats.pages_created += 1;
        index
    }

    fn evict_oldest_page(&mut self, format: AtlasFormat) -> Option<(usize, usize)> {
        let index = self
            .pages
            .iter()
            .enumerate()
            .filter(|(_, p)| p.format == format)
            .min_by_key(|(_, p)| p.last_used)
            .map(|(i, _)| i)?;
        let count = self.evict_page(index);
        Some((index, count))
    }

    /// Clear a page and remove all of its glyphs from the cache. Returns the
    /// number of glyphs evicted.
    pub fn evict_page(&mut self, index: usize) -> usize {
        let Some(page) = self.pages.get_mut(index) else {
            return 0;
        };
        let keys = core::mem::take(&mut page.keys);
        page.reset();
        for key in &keys {
            self.entries.remove(key);
        }
        if !keys.is_empty() {
            self.stats.pages_evicted += 1;
        }
        keys.len()
    }

    /// Remove one glyph. Its shelf space is not reclaimed, matching the
    /// simplicity of kitty's sprite cache.
    pub fn remove(&mut self, key: &GlyphKey) -> Option<GlyphSlot> {
        let slot = self.entries.remove(key)?;
        let page = &mut self.pages[slot.page as usize];
        page.keys.retain(|k| k != key);
        Some(slot)
    }

    /// Drop all pages and cached entries.
    pub fn clear(&mut self) {
        self.pages.clear();
        self.entries.clear();
        self.clock = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(width: u32, height: u32) -> AtlasConfig {
        AtlasConfig {
            page_width: width,
            page_height: height,
            padding: 1,
            max_gray_pages: 64,
            max_rgba_pages: 64,
            allow_eviction: true,
        }
    }

    fn gray_bitmap(width: u32, height: u32, value: u8) -> GlyphBitmap {
        GlyphBitmap::gray(width, height, vec![value; (width * height) as usize]).unwrap()
    }

    #[test]
    fn deterministic_placement_and_no_overlaps() {
        let mut atlas = GlyphAtlas::new(config(256, 256));
        let mut atlas2 = GlyphAtlas::new(config(256, 256));
        let mut state = 0x1234_5678u64;
        let mut slots = Vec::new();
        for i in 0..3000u32 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let w = 1 + (state % 24) as u32;
            let h = 1 + ((state >> 8) % 24) as u32;
            let key = GlyphKey::new(0, i, 0);
            let a = atlas
                .insert(key, gray_bitmap(w, h, (i & 0xff) as u8))
                .unwrap();
            let b = atlas2
                .insert(key, gray_bitmap(w, h, (i & 0xff) as u8))
                .unwrap();
            assert_eq!(a.slot, b.slot, "placement must be deterministic");
            slots.push(a.slot);
        }
        assert_eq!(atlas.len(), 3000);
        for i in 0..slots.len() {
            let a = slots[i];
            assert!(a.x + a.width <= 256 && a.y + a.height <= 256);
            for b in &slots[i + 1..] {
                if a.page == b.page {
                    let ra = a.pixel_rect();
                    let rb = b.pixel_rect();
                    assert!(!ra.intersects(rb), "overlap between {ra:?} and {rb:?}");
                }
            }
        }
    }

    #[test]
    fn uv_rects_are_in_bounds() {
        let mut atlas = GlyphAtlas::new(config(64, 64));
        for i in 0..20u32 {
            let slot = atlas
                .insert(GlyphKey::new(0, i, 0), gray_bitmap(8, 9, 1))
                .unwrap()
                .slot;
            assert!(slot.uv.u0 >= 0.0 && slot.uv.v0 >= 0.0);
            assert!(slot.uv.u1 <= 1.0 && slot.uv.v1 <= 1.0);
            assert!(slot.uv.u0 < slot.uv.u1 && slot.uv.v0 < slot.uv.v1);
            let page = atlas.page(slot.page as usize).unwrap();
            assert_eq!(slot.uv.u0, slot.x as f32 / page.width() as f32);
        }
    }

    #[test]
    fn separate_formats_and_pixel_copy() {
        let mut atlas = GlyphAtlas::new(config(32, 32));
        let rgba = GlyphBitmap::rgba(2, 1, vec![1, 2, 3, 4, 5, 6, 7, 8]).unwrap();
        let g = atlas
            .insert(GlyphKey::new(0, 0, 0), gray_bitmap(2, 2, 0xaa))
            .unwrap();
        let c = atlas.insert(GlyphKey::new(0, 1, 0), rgba).unwrap();
        assert_ne!(g.slot.page, c.slot.page);
        let gp = atlas.page(g.slot.page as usize).unwrap();
        assert_eq!(gp.data()[(g.slot.y * 32 + g.slot.x) as usize], 0xaa);
        let cp = atlas.page(c.slot.page as usize).unwrap();
        let start = ((c.slot.y * 32 + c.slot.x) * 4) as usize;
        assert_eq!(&cp.data()[start..start + 8], &[1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(atlas.page_count(AtlasFormat::Gray8), 1);
        assert_eq!(atlas.page_count(AtlasFormat::Rgba8), 1);
    }

    #[test]
    fn page_overflow_is_reported() {
        let mut cfg = config(16, 16);
        cfg.padding = 0;
        cfg.max_gray_pages = 1;
        cfg.allow_eviction = false;
        let mut atlas = GlyphAtlas::new(cfg);
        atlas
            .insert(GlyphKey::new(0, 0, 0), gray_bitmap(16, 16, 1))
            .unwrap();
        let err = atlas
            .insert(GlyphKey::new(0, 1, 0), gray_bitmap(16, 16, 1))
            .expect_err("page should be full");
        assert_eq!(
            err,
            AtlasError::Full {
                format: AtlasFormat::Gray8,
                pages: 1
            }
        );
        assert_eq!(atlas.stats().full_page_errors, 1);
    }

    #[test]
    fn eviction_reports_and_reuses_page() {
        let mut cfg = config(16, 16);
        cfg.padding = 0;
        cfg.max_gray_pages = 1;
        cfg.allow_eviction = true;
        let mut atlas = GlyphAtlas::new(cfg);
        let a = GlyphKey::new(0, 0, 0);
        let b = GlyphKey::new(0, 1, 0);
        atlas.insert(a, gray_bitmap(16, 16, 1)).unwrap();
        let inserted = atlas.insert(b, gray_bitmap(16, 16, 2)).unwrap();
        assert_eq!(
            inserted.event,
            Some(AtlasEvent::EvictedPage {
                page: 0,
                format: AtlasFormat::Gray8,
                glyphs: 1
            })
        );
        assert!(atlas.lookup(&a).is_none());
        assert!(atlas.lookup(&b).is_some());
        assert_eq!(atlas.len(), 1);
    }

    #[test]
    fn glyph_too_large_and_bad_bitmap() {
        let mut atlas = GlyphAtlas::new(config(16, 16));
        let err = atlas
            .insert(GlyphKey::new(0, 0, 0), gray_bitmap(17, 1, 1))
            .unwrap_err();
        assert!(matches!(err, AtlasError::GlyphTooLarge { .. }));
        let bad = GlyphBitmap {
            width: 2,
            height: 2,
            format: AtlasFormat::Gray8,
            data: vec![0; 3],
        };
        let err = atlas.insert(GlyphKey::new(0, 1, 0), bad).unwrap_err();
        assert!(matches!(err, AtlasError::InvalidBitmap { .. }));
    }

    #[test]
    fn duplicate_insert_is_a_cache_hit() {
        let mut atlas = GlyphAtlas::new(config(32, 32));
        let key = GlyphKey::from_codepoints(0, &[b'a' as u32], 0);
        let first = atlas.insert(key, gray_bitmap(4, 4, 1)).unwrap();
        let second = atlas.insert(key, gray_bitmap(4, 4, 1)).unwrap();
        assert!(!first.already_present);
        assert!(second.already_present);
        assert_eq!(first.slot, second.slot);
        assert_eq!(atlas.stats().cache_hits, 1);
        assert_eq!(atlas.len(), 1);
    }
}
