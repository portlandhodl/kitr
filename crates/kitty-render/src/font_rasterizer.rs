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

use kitty_fonts::{Face, PixelFormat, ShapeOptions};

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
        let shaped = self.face.shape(run.text, &ShapeOptions::default()).ok()?;
        if shaped.glyphs.is_empty() {
            return None;
        }
        let special = shaped.direction.is_rtl()
            || run.text.len() != run.cell_count as usize
            || shaped.glyphs.len() != run.cell_count as usize;
        if !special {
            return None;
        }
        let cell_width = run.cell_width.round().max(1.0) as i32;
        let cell_height = run.cell_height.round().max(1.0) as i32;
        let baseline = run.baseline.round() as i32;
        let canvas_width = cell_width.checked_mul(run.cell_count as i32)?;
        if canvas_width <= 0 || cell_height <= 0 {
            return None;
        }
        let mut any_color = false;
        let mut glyphs = Vec::new();
        let mut pen_x = 0.0f32;
        let mut pen_y = 0.0f32;
        for glyph in &shaped.glyphs {
            let bitmap = self.face.rasterize_glyph(glyph.glyph_id).ok()?;
            if bitmap.format != PixelFormat::Gray8 {
                any_color = true;
            }
            glyphs.push((pen_x, pen_y, glyph.x_offset, glyph.y_offset, bitmap));
            pen_x += (glyph.x_advance as f32 / 64.0).round();
            pen_y += (glyph.y_advance as f32 / 64.0).round();
        }
        let mut gray = vec![0u8; canvas_width as usize * cell_height as usize];
        let mut rgba = if any_color {
            vec![0u8; canvas_width as usize * cell_height as usize * 4]
        } else {
            Vec::new()
        };
        for (index, (gx, gy, x_off, y_off, bitmap)) in glyphs.into_iter().enumerate() {
            let x = gx + x_off as f32 / 64.0;
            let y = gy + y_off as f32 / 64.0;
            let mut dest_x = (x + bitmap.left as f32) as i32;
            if dest_x > 0 && index < 4 && dest_x as i64 + bitmap.width as i64 > canvas_width as i64
            {
                let extra = dest_x as i64 + bitmap.width as i64 - canvas_width as i64;
                dest_x = if extra > dest_x as i64 {
                    0
                } else {
                    dest_x - extra as i32
                };
            }
            let yoff = (y + bitmap.top as f32) as i32;
            let dest_y = if yoff > 0 && yoff > baseline {
                0
            } else {
                baseline - yoff
            };
            if bitmap.format == PixelFormat::Gray8 {
                if any_color {
                    let mut layer = vec![0u8; bitmap.width as usize * bitmap.height as usize * 4];
                    for row in 0..bitmap.height as usize {
                        for col in 0..bitmap.width as usize {
                            let a = bitmap.data[row * bitmap.pitch + col];
                            let d = (row * bitmap.width as usize + col) * 4;
                            layer[d..d + 4].copy_from_slice(&[255, 255, 255, a]);
                        }
                    }
                    blit_rgba(
                        &mut rgba,
                        canvas_width as u32,
                        cell_height as u32,
                        bitmap.width,
                        bitmap.height,
                        &layer,
                        dest_x,
                        dest_y,
                    );
                } else {
                    blit_gray(
                        &mut gray,
                        canvas_width as u32,
                        cell_height as u32,
                        &bitmap,
                        dest_x,
                        dest_y,
                    );
                }
            } else {
                let bgra = bitmap.format == PixelFormat::Bgra8;
                let width = bitmap.width;
                let height = bitmap.height;
                let mut layer = vec![0u8; width as usize * height as usize * 4];
                for row in 0..height as usize {
                    for col in 0..width as usize {
                        let src = row * bitmap.pitch + col * 4;
                        let d = (row * width as usize + col) * 4;
                        if bgra {
                            layer[d] = bitmap.data[src + 2];
                            layer[d + 1] = bitmap.data[src + 1];
                            layer[d + 2] = bitmap.data[src];
                        } else {
                            layer[d..d + 3].copy_from_slice(&bitmap.data[src..src + 3]);
                        }
                        layer[d + 3] = bitmap.data[src + 3];
                    }
                }
                blit_rgba(
                    &mut rgba,
                    canvas_width as u32,
                    cell_height as u32,
                    width,
                    height,
                    &layer,
                    dest_x,
                    dest_y,
                );
            }
        }
        let bitmap = if any_color {
            GlyphBitmap::rgba(canvas_width as u32, cell_height as u32, rgba).ok()?
        } else {
            GlyphBitmap::gray(canvas_width as u32, cell_height as u32, gray).ok()?
        };
        Some(ShapedRunOutput {
            clusters: vec![ShapedRunCluster {
                cell_start: 0,
                cell_span: run.cell_count,
                bitmap,
            }],
        })
    }
}

fn blit_gray(
    canvas: &mut [u8],
    width: u32,
    height: u32,
    bitmap: &kitty_fonts::GlyphBitmap,
    x0: i32,
    y0: i32,
) {
    for row in 0..bitmap.height as i32 {
        let y = y0 + row;
        if y < 0 || y >= height as i32 {
            continue;
        }
        for col in 0..bitmap.width as i32 {
            let x = x0 + col;
            if x < 0 || x >= width as i32 {
                continue;
            }
            let src = bitmap.data[row as usize * bitmap.pitch + col as usize];
            let dst = &mut canvas[y as usize * width as usize + x as usize];
            *dst = (*dst).max(src);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn blit_rgba(
    canvas: &mut [u8],
    width: u32,
    height: u32,
    bitmap_width: u32,
    bitmap_height: u32,
    data: &[u8],
    x0: i32,
    y0: i32,
) {
    for row in 0..bitmap_height as i32 {
        let y = y0 + row;
        if y < 0 || y >= height as i32 {
            continue;
        }
        for col in 0..bitmap_width as i32 {
            let x = x0 + col;
            if x < 0 || x >= width as i32 {
                continue;
            }
            let src = (row as u32 * bitmap_width + col as u32) as usize * 4;
            let s = &data[src..src + 4];
            if s[3] == 0 {
                continue;
            }
            let dst = (y as usize * width as usize + x as usize) * 4;
            if s[3] == 255 {
                canvas[dst..dst + 4].copy_from_slice(s);
                continue;
            }
            let sa = s[3] as u32;
            let da = canvas[dst + 3] as u32;
            let out_a = sa + da * (255 - sa) / 255;
            let mix = |sc: u8, dc: u8| -> u8 {
                if out_a == 0 {
                    0
                } else {
                    ((sc as u32 * sa * 255 + dc as u32 * da * (255 - sa)) / (out_a * 255)) as u8
                }
            };
            canvas[dst] = mix(s[0], canvas[dst]);
            canvas[dst + 1] = mix(s[1], canvas[dst + 1]);
            canvas[dst + 2] = mix(s[2], canvas[dst + 2]);
            canvas[dst + 3] = out_a as u8;
        }
    }
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
