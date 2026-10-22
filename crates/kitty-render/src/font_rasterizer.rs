// SPDX-License-Identifier: GPL-3.0-or-later
//! Adapter from `kitty-fonts` shaping/rasterization to the renderer's glyph
//! atlas interfaces.
//!
//! Per-cell glyphs use FreeType through [`Face`]. [`FontRasterizer::shape_run`]
//! uses HarfBuzz and composites all glyphs belonging to a ligature or combining
//! cluster into one cell-aligned Gray8 bitmap. Colour glyphs (COLR/CBDT/SVG)
//! are intentionally not composited: they return `None`, which makes the
//! instance generator fall back to per-cell glyphs.

use std::path::Path;

use kitty_fonts::{Face, PixelFormat, ShapeOptions, ShapedGlyph};

use crate::atlas::{GlyphBitmap, GlyphKey, GlyphRasterizer};
use crate::shaped::{ShapedRunCluster, ShapedRunOutput, ShapedRunRequest};

/// A [`GlyphRasterizer`] backed by a single `kitty-fonts` face.
pub struct FontRasterizer {
    face: Face,
}

impl FontRasterizer {
    pub fn new(face: Face) -> Self {
        Self { face }
    }

    pub fn from_path(path: impl AsRef<Path>, index: i32) -> Option<Self> {
        Face::from_path(path, index).ok().map(Self::new)
    }

    pub fn face(&self) -> &Face {
        &self.face
    }

    fn rasterize_glyph_id(&self, glyph_id: u32) -> Option<GlyphBitmap> {
        let bitmap = self.face.rasterize_glyph(glyph_id).ok()?;
        convert_bitmap(bitmap)
    }
}

impl GlyphRasterizer for FontRasterizer {
    fn rasterize(&mut self, glyph: &GlyphKey) -> Option<GlyphBitmap> {
        let glyph_id = match glyph.text.as_slice().first().copied() {
            Some(cp) => self.face.glyph_id(cp),
            None => glyph.glyph_id,
        };
        if glyph_id == 0 {
            return None;
        }
        self.rasterize_glyph_id(glyph_id)
    }

    fn shape_run(&mut self, run: &ShapedRunRequest<'_>) -> Option<ShapedRunOutput> {
        if run.text.is_empty()
            || run.cell_for_codepoint.len() != run.text.len()
            || run.cell_count == 0
        {
            return None;
        }
        let options = ShapeOptions {
            force_ltr: true,
            ..ShapeOptions::default()
        };
        let shaped = self.face.shape(run.text, &options).ok()?;
        if shaped.glyphs.is_empty() {
            return None;
        }

        let mut cell_codepoints = vec![0usize; run.cell_count as usize];
        for &cell in run.cell_for_codepoint {
            let slot = cell_codepoints.get_mut(cell as usize)?;
            *slot += 1;
        }

        let mut groups: Vec<GlyphGroup> = Vec::new();
        let mut last_cluster = 0usize;
        for (index, glyph) in shaped.glyphs.iter().enumerate() {
            let start = glyph.cluster as usize;
            if start >= run.text.len() || start < last_cluster {
                return None;
            }
            last_cluster = start;
            let end = shaped
                .glyphs
                .get(index + 1)
                .map(|next| next.cluster as usize)
                .unwrap_or(run.text.len())
                .max(start + 1)
                .min(run.text.len());
            let first_cell = *run.cell_for_codepoint.get(start)?;
            let last_cell = *run.cell_for_codepoint.get(end - 1)?;
            if last_cell < first_cell {
                return None;
            }
            let span = last_cell - first_cell + 1;
            match groups.last_mut() {
                Some(group) if group.cell_start == first_cell => {
                    group.cell_span = group.cell_span.max(span);
                    group.glyphs.push(index);
                }
                _ => groups.push(GlyphGroup {
                    cell_start: first_cell,
                    cell_span: span,
                    glyphs: vec![index],
                }),
            }
        }

        let mut pen_positions = Vec::with_capacity(shaped.glyphs.len());
        let mut pen_x = 0i32;
        let mut pen_y = 0i32;
        for glyph in &shaped.glyphs {
            pen_positions.push((pen_x, pen_y));
            pen_x = pen_x.saturating_add(glyph.x_advance);
            pen_y = pen_y.saturating_add(glyph.y_advance);
        }

        let cell_width = run.cell_width.round().max(1.0) as i32;
        let cell_height = run.cell_height.round().max(1.0) as i32;
        let baseline = run.baseline.round() as i32;

        let mut output = ShapedRunOutput::default();
        for group in groups {
            let special = group.cell_span > 1
                || group.glyphs.len() > 1
                || (group.cell_start as usize
                    ..group.cell_start as usize + group.cell_span as usize)
                    .any(|cell| cell_codepoints.get(cell).copied().unwrap_or(0) > 1);
            if !special {
                continue;
            }
            let canvas_width = cell_width.checked_mul(group.cell_span as i32)?;
            if canvas_width <= 0 || cell_height <= 0 {
                continue;
            }
            let mut canvas = vec![0u8; canvas_width as usize * cell_height as usize];
            let origin_x = group.cell_start as i32 * cell_width;
            for &glyph_index in &group.glyphs {
                let glyph = shaped.glyphs[glyph_index];
                let (pen_x, pen_y) = pen_positions[glyph_index];
                if !composite_glyph(
                    &self.face,
                    glyph,
                    pen_x,
                    pen_y,
                    origin_x,
                    baseline,
                    canvas_width,
                    cell_height,
                    &mut canvas,
                ) {
                    return None;
                }
            }
            output.clusters.push(ShapedRunCluster {
                cell_start: group.cell_start,
                cell_span: group.cell_span,
                bitmap: GlyphBitmap::gray(canvas_width as u32, cell_height as u32, canvas).ok()?,
            });
        }
        if output.clusters.is_empty() {
            None
        } else {
            Some(output)
        }
    }
}

struct GlyphGroup {
    cell_start: u32,
    cell_span: u32,
    glyphs: Vec<usize>,
}

#[allow(clippy::too_many_arguments)]
fn composite_glyph(
    face: &Face,
    glyph: ShapedGlyph,
    pen_x: i32,
    pen_y: i32,
    origin_x: i32,
    baseline: i32,
    canvas_width: i32,
    canvas_height: i32,
    canvas: &mut [u8],
) -> bool {
    let Ok(bitmap) = face.rasterize_glyph(glyph.glyph_id) else {
        return false;
    };
    if bitmap.format != PixelFormat::Gray8 || bitmap.width == 0 || bitmap.height == 0 {
        return false;
    }
    let x_px = ((pen_x.saturating_add(glyph.x_offset)) as f32 / 64.0).round() as i32;
    let y_px = ((pen_y.saturating_add(glyph.y_offset)) as f32 / 64.0).round() as i32;
    let dest_x = x_px + bitmap.left - origin_x;
    let dest_y = baseline - bitmap.top - y_px;
    let bpp = bitmap.bytes_per_pixel();
    for row in 0..bitmap.height as i32 {
        let y = dest_y + row;
        if y < 0 || y >= canvas_height {
            continue;
        }
        let src_start = row as usize * bitmap.pitch;
        if src_start + bitmap.width as usize * bpp > bitmap.data.len() {
            return false;
        }
        for col in 0..bitmap.width as i32 {
            let x = dest_x + col;
            if x < 0 || x >= canvas_width {
                continue;
            }
            let src = bitmap.data[src_start + col as usize * bpp];
            let dst = &mut canvas[y as usize * canvas_width as usize + x as usize];
            *dst = (*dst).max(src);
        }
    }
    true
}

fn convert_bitmap(bitmap: kitty_fonts::GlyphBitmap) -> Option<GlyphBitmap> {
    if bitmap.width == 0 || bitmap.height == 0 {
        return Some(GlyphBitmap {
            width: 0,
            height: 0,
            format: crate::atlas::AtlasFormat::Gray8,
            data: Vec::new(),
        });
    }
    let bpp = bitmap.bytes_per_pixel();
    let row_bytes = bitmap.width as usize * bpp;
    let mut data = Vec::with_capacity(row_bytes * bitmap.height as usize);
    for row in 0..bitmap.height as usize {
        let start = row * bitmap.pitch;
        let end = start.checked_add(row_bytes)?;
        if end > bitmap.data.len() {
            return None;
        }
        let src = &bitmap.data[start..end];
        match bitmap.format {
            PixelFormat::Gray8 => data.extend_from_slice(src),
            PixelFormat::Rgba8 => data.extend_from_slice(src),
            PixelFormat::Bgra8 => {
                let mut index = 0usize;
                while index + 4 <= src.len() {
                    data.extend_from_slice(&[
                        src[index + 2],
                        src[index + 1],
                        src[index],
                        src[index + 3],
                    ]);
                    index += 4;
                }
            }
        }
    }
    let format = match bitmap.format {
        PixelFormat::Gray8 => crate::atlas::AtlasFormat::Gray8,
        PixelFormat::Rgba8 | PixelFormat::Bgra8 => crate::atlas::AtlasFormat::Rgba8,
    };
    GlyphBitmap::new(bitmap.width, bitmap.height, format, data).ok()
}
