// SPDX-License-Identifier: GPL-3.0-or-later
//! Adaptation of the packed CPU glyph atlas to kitty's fixed-cell sprite array.
//!
//! The normal backend draws glyphs with the graphics program and arbitrary
//! atlas UVs. The cell programs (`cell`, `cell_bg`, `cell_fg`) instead expect
//! kitty's fixed-cell `GL_TEXTURE_2D_ARRAY`: each sprite occupies a
//! `cell_width` by `cell_height + 1` tile, and the shader addresses a sprite by
//! its integer index. This module builds that representation from the packed
//! [`GlyphAtlas`] without changing the CPU atlas or its tests.
//!
//! The tile has one extra blank scanline at the bottom because the cell shader
//! subtracts one texel row when converting a sprite row to texture coordinates.
//! That is the same layout as `realloc_sprite_texture()` in `kitty/shaders.c`.
//! Sprite index zero is reserved for an empty tile, exactly like kitty's
//! `clear_sprite_position()`.

use std::collections::HashMap;

use crate::atlas::{AtlasFormat, GlyphAtlas, GlyphSlot};
use crate::colors::{encode_rgb, Rgba8};
use crate::geom::Rect;
use crate::instances::Frame;

/// Identity of a sprite atlas slot, independent of the UV floats.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SpriteKey {
    pub page: u32,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub format: AtlasFormat,
}

impl From<GlyphSlot> for SpriteKey {
    fn from(slot: GlyphSlot) -> Self {
        Self {
            page: slot.page,
            x: slot.x,
            y: slot.y,
            width: slot.width,
            height: slot.height,
            format: slot.format,
        }
    }
}

/// One instance of the cell programs. The field order and sizes mirror
/// `GPUCell`/the `colors`, `sprite_idx` and `is_selected` attributes.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CellRecord {
    /// `[foreground, background, decoration foreground]`, kitty colour type
    /// encoded (see [`encode_rgb`]).
    pub colors: [u32; 3],
    /// Low 31 bits: sprite index. High bit: colour sprite.
    pub sprite_idx: u32,
    /// Kitty's text attribute bitfield (`sprite_idx[1]`).
    pub attrs: u32,
    pub is_selected: u8,
    pub _padding: [u8; 3],
}

/// CPU-side fixed-cell sprite array and cell stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CellSpriteData {
    pub columns: u32,
    pub lines: u32,
    pub cell_width: u32,
    pub cell_height: u32,
    /// Height of one sprite tile, including the trailing blank scanline.
    pub tile_height: u32,
    pub sprites_xnum: u32,
    pub sprites_ynum: u32,
    pub layers: u32,
    /// Tightly packed RGBA8 sprite layers.
    pub pixels: Vec<u8>,
    /// One R32UI entry per sprite index, giving the decoration sprite index.
    pub decorations: Vec<u32>,
    /// Index for every distinct atlas slot, starting at 1 (zero is empty).
    pub sprite_indices: HashMap<SpriteKey, u32>,
    pub cells: Vec<CellRecord>,
}

impl CellSpriteData {
    #[inline]
    pub const fn sprite_count(&self) -> u32 {
        self.sprites_xnum * self.sprites_ynum * self.layers
    }

    /// Row-major cell record for `(x, y)`.
    pub fn cell(&self, x: u32, y: u32) -> Option<&CellRecord> {
        if x >= self.columns || y >= self.lines {
            return None;
        }
        self.cells.get((y * self.columns + x) as usize)
    }
}

/// Geometry needed to build a cell stream. Kept separate from [`Frame`] so the
/// adapter can be tested against synthetic data.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CellGrid {
    pub columns: u32,
    pub lines: u32,
    pub cell_width: f32,
    pub cell_height: f32,
    pub origin_x: f32,
    pub origin_y: f32,
    pub default_fg: Rgba8,
    pub default_bg: Rgba8,
}

impl CellGrid {
    pub fn from_frame(frame: &Frame) -> Option<Self> {
        let g = frame.geometry;
        if g.columns == 0
            || g.rows == 0
            || g.cell_width <= 0.0
            || g.cell_height <= 0.0
            || g.origin_x.abs() > 0.01
            || g.origin_y.abs() > 0.01
        {
            return None;
        }
        Some(Self {
            columns: g.columns,
            lines: g.rows,
            cell_width: g.cell_width,
            cell_height: g.cell_height,
            origin_x: g.origin_x,
            origin_y: g.origin_y,
            default_fg: frame.default_fg,
            default_bg: frame.default_bg,
        })
    }
}

/// Build a fixed-cell sprite array from `atlas` and the visible `frame`.
///
/// Returns `None` when the frame is not a plain, origin-aligned cell grid or
/// contains glyphs wider than one cell (shaped runs and wide characters still
/// use the graphics-program path, which supports arbitrary sprite rectangles).
pub fn build_cell_sprites(atlas: &GlyphAtlas, frame: &Frame) -> Option<CellSpriteData> {
    let grid = CellGrid::from_frame(frame)?;
    let cell_width = grid.cell_width.round().max(1.0) as u32;
    let cell_height = grid.cell_height.round().max(1.0) as u32;
    let tile_height = cell_height + 1;

    // The cell shader only knows about cell_width/cell_height tiles. A glyph
    // rectangle wider than one cell would be squeezed into a single tile, so
    // leave those frames to the general graphics path.
    if frame
        .glyphs
        .iter()
        .any(|glyph| glyph.rect.width > grid.cell_width * 1.5)
    {
        return None;
    }

    // Fixed tile grid, capped so a tiny font does not allocate an enormous
    // array texture. 64 slots per axis is enough for ~4k glyphs per layer.
    let sprites_xnum = (1024 / cell_width).clamp(1, 64);
    let sprites_ynum = (1024 / tile_height).clamp(1, 64);

    let mut sprite_indices: HashMap<SpriteKey, u32> = HashMap::new();
    let mut sprites: Vec<(SpriteKey, u32)> = Vec::new();
    let mut next_index = 1u32;
    for glyph in &frame.glyphs {
        let Some(slot) = glyph.sprite else {
            continue;
        };
        let key = SpriteKey::from(slot);
        if sprite_indices.contains_key(&key) {
            continue;
        }
        sprite_indices.insert(key, next_index);
        sprites.push((key, next_index));
        next_index += 1;
    }

    let sprites_per_layer = (sprites_xnum * sprites_ynum) as usize;
    let layers = sprites.len().div_ceil(sprites_per_layer).max(1) as u32;
    let width = sprites_xnum * cell_width;
    let height = sprites_ynum * tile_height;
    let mut pixels = vec![0u8; width as usize * height as usize * layers as usize * 4];
    let decorations = vec![0u32; layers as usize * sprites_per_layer];

    for (key, index) in &sprites {
        let Some(page) = atlas.page(key.page as usize) else {
            continue;
        };
        let page_width = page.width() as usize;
        if key.x + key.width > page.width() || key.y + key.height > page.height() {
            continue;
        }
        let bpp = key.format.bytes_per_pixel();
        let layer = (*index as usize) / sprites_per_layer;
        let within = (*index as usize) % sprites_per_layer;
        let slot_x = within % sprites_xnum as usize;
        let slot_y = within / sprites_xnum as usize;
        let dst_x = slot_x * cell_width as usize;
        let dst_y = slot_y * tile_height as usize;
        let layer_stride = width as usize * height as usize * 4;
        let layer_base = layer * layer_stride;
        for dy in 0..cell_height {
            let sy = (dy as u64 * key.height as u64 / cell_height as u64)
                .min(key.height.saturating_sub(1) as u64) as usize;
            for dx in 0..cell_width {
                let sx = (dx as u64 * key.width as u64 / cell_width as u64)
                    .min(key.width.saturating_sub(1) as u64) as usize;
                let src = ((key.y as usize + sy) * page_width + key.x as usize + sx) * bpp;
                let dst =
                    layer_base + ((dst_y + dy as usize) * width as usize + dst_x + dx as usize) * 4;
                if bpp == 1 {
                    let alpha = page.data()[src];
                    pixels[dst] = 255;
                    pixels[dst + 1] = 255;
                    pixels[dst + 2] = 255;
                    pixels[dst + 3] = alpha;
                } else {
                    pixels[dst..dst + 4].copy_from_slice(&page.data()[src..src + 4]);
                }
            }
        }
    }

    // Backgrounds that are actual grid cells are folded into the per-cell
    // colour stream. Chrome rectangles (tab bar, pane borders) are left to the
    // ordinary rectangle path because they are not cell-aligned.
    let mut backgrounds: HashMap<(u32, u32), Rgba8> = HashMap::new();
    for background in &frame.backgrounds {
        if !cell_aligned(background.rect, background.cell_x, background.cell_y, grid) {
            continue;
        }
        let (x, y) = (background.cell_x, background.cell_y);
        let span = (background.rect.width / grid.cell_width).round().max(1.0) as u32;
        for cx in x..x.saturating_add(span).min(grid.columns) {
            if y < grid.lines {
                backgrounds.insert((cx, y), background.color);
            }
        }
    }

    let mut glyphs: HashMap<(u32, u32), (&crate::instances::GlyphInstance, u32)> = HashMap::new();
    for glyph in &frame.glyphs {
        let Some(slot) = glyph.sprite else {
            continue;
        };
        let Some(index) = sprite_indices.get(&SpriteKey::from(slot)).copied() else {
            continue;
        };
        if glyph.cell_x < grid.columns && glyph.cell_y < grid.lines {
            let index = index | u32::from(slot.format == AtlasFormat::Rgba8) << 31;
            glyphs.insert((glyph.cell_x, glyph.cell_y), (glyph, index));
        }
    }

    let mut cells = Vec::with_capacity(grid.columns as usize * grid.lines as usize);
    for y in 0..grid.lines {
        for x in 0..grid.columns {
            let background = backgrounds.get(&(x, y)).copied().unwrap_or(grid.default_bg);
            let (fg, sprite_idx) = match glyphs.get(&(x, y)) {
                Some((glyph, index)) => (glyph.fg, *index),
                None => (grid.default_fg, 0),
            };
            cells.push(CellRecord {
                colors: [
                    encode_rgb(fg.r, fg.g, fg.b),
                    encode_rgb(background.r, background.g, background.b),
                    encode_rgb(fg.r, fg.g, fg.b),
                ],
                sprite_idx,
                attrs: 0,
                is_selected: 0,
                _padding: [0; 3],
            });
        }
    }

    Some(CellSpriteData {
        columns: grid.columns,
        lines: grid.lines,
        cell_width,
        cell_height,
        tile_height,
        sprites_xnum,
        sprites_ynum,
        layers,
        pixels,
        decorations,
        sprite_indices,
        cells,
    })
}

fn cell_aligned(rect: Rect, cell_x: u32, cell_y: u32, grid: CellGrid) -> bool {
    let expected = grid_cell_rect(grid, cell_x, cell_y);
    (rect.x - expected.x).abs() < 0.51
        && (rect.y - expected.y).abs() < 0.51
        && (rect.height - expected.height).abs() < 0.51
        && rect.width <= expected.width * 64.0 + 0.51
}

fn grid_cell_rect(grid: CellGrid, x: u32, y: u32) -> Rect {
    Rect::new(
        grid.origin_x + x as f32 * grid.cell_width,
        grid.origin_y + y as f32 * grid.cell_height,
        grid.cell_width,
        grid.cell_height,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atlas::{AtlasConfig, GlyphBitmap, GlyphKey};
    use crate::damage::Damage;
    use crate::geom::{CellMetrics, GridGeometry};
    use crate::instances::{BackgroundInstance, GlyphInstance, InstanceFlags};

    fn test_atlas() -> (GlyphAtlas, GlyphSlot) {
        let mut atlas = GlyphAtlas::new(AtlasConfig::with_page_size(16, 16));
        let slot = atlas
            .insert(
                GlyphKey::new(0, 1, 0),
                GlyphBitmap::gray(2, 2, vec![255, 255, 255, 255]).unwrap(),
            )
            .unwrap()
            .slot;
        (atlas, slot)
    }

    fn frame_with(slot: GlyphSlot) -> Frame {
        let mut frame = Frame {
            damage: Damage::default(),
            geometry: GridGeometry::new(
                2,
                2,
                CellMetrics {
                    cell_width: 8.0,
                    cell_height: 10.0,
                    ..CellMetrics::default()
                },
            ),
            default_fg: Rgba8::WHITE,
            default_bg: Rgba8::BLACK,
            ..Frame::default()
        };
        frame.backgrounds.push(BackgroundInstance {
            rect: Rect::new(0.0, 0.0, 8.0, 10.0),
            cell_x: 0,
            cell_y: 0,
            color: Rgba8::rgb(10, 20, 30),
            is_default_bg: false,
            flags: InstanceFlags::default(),
        });
        frame.glyphs.push(GlyphInstance {
            rect: Rect::new(8.0, 10.0, 8.0, 10.0),
            cell_x: 1,
            cell_y: 1,
            fg: Rgba8::rgb(200, 100, 50),
            sprite: Some(slot),
            text: GlyphKey::new(0, 1, 0).text,
            flags: InstanceFlags::default(),
        });
        frame
    }

    #[test]
    fn sprite_tiles_and_cells_match_the_cell_shader_layout() {
        let (atlas, slot) = test_atlas();
        let frame = frame_with(slot);
        let data = build_cell_sprites(&atlas, &frame).expect("cell data");
        assert_eq!((data.columns, data.lines), (2, 2));
        assert_eq!(
            (data.cell_width, data.cell_height, data.tile_height),
            (8, 10, 11)
        );
        assert_eq!(data.sprite_indices.len(), 1);
        assert_eq!(data.cell(0, 0).unwrap().colors[1], encode_rgb(10, 20, 30));
        let glyph_cell = data.cell(1, 1).unwrap();
        assert_eq!(glyph_cell.sprite_idx & 0x7fff_ffff, 1);
        assert_eq!(glyph_cell.colors[0], encode_rgb(200, 100, 50));
        assert_eq!(data.cells[0].sprite_idx, 0);
        // Tile 1 is at x=cell_width in the first layer and is opaque white.
        let stride = (data.sprites_xnum * data.cell_width * 4) as usize;
        let offset = stride + data.cell_width as usize * 4;
        assert_eq!(&data.pixels[offset..offset + 4], &[255, 255, 255, 255]);
    }

    #[test]
    fn wide_glyph_falls_back_to_the_graphics_path() {
        let (atlas, slot) = test_atlas();
        let mut frame = frame_with(slot);
        frame.glyphs[0].rect.width = 16.0;
        assert!(build_cell_sprites(&atlas, &frame).is_none());
    }

    #[test]
    fn origin_offset_is_not_supported_by_the_cell_grid() {
        let (atlas, slot) = test_atlas();
        let mut frame = frame_with(slot);
        frame.geometry.origin_x = 4.0;
        assert!(build_cell_sprites(&atlas, &frame).is_none());
    }
}
