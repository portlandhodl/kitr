// SPDX-License-Identifier: GPL-3.0-or-later
//! Font loading and the bridge from the renderer's glyph keys to FreeType.

use kitty_config::Options;
use kitty_fonts::{
    FontCellMetrics, FontSpec as KittyFontSpec, FontStack, FontStyle, PixelFormat, RasterCache,
    DEFAULT_DPI,
};
use kitty_render::{
    GlyphBitmap as RenderGlyphBitmap, GlyphKey, GlyphRasterizer, ShapedRunCluster, ShapedRunOutput,
    ShapedRunRequest, GLYPH_FLAG_BOLD, GLYPH_FLAG_ITALIC,
};
use kitty_unicode::wcswidth_std;

fn spec_string(spec: &kitty_config::FontSpec) -> String {
    if !spec.created_from_string.is_empty() {
        return spec.created_from_string.clone();
    }
    spec.system
        .clone()
        .or_else(|| spec.family.clone())
        .unwrap_or_else(|| "monospace".to_string())
}

fn convert_spec(spec: &kitty_config::FontSpec) -> Option<KittyFontSpec> {
    KittyFontSpec::from_setting(&spec_string(spec)).ok()
}

fn load_stack(
    spec: &kitty_config::FontSpec,
    fallback: &kitty_config::FontSpec,
    style: FontStyle,
    size_pts: f64,
    dpi: f64,
) -> Result<FontStack, String> {
    let source = if spec.system.as_deref() == Some("auto") {
        fallback
    } else {
        spec
    };
    let parsed = convert_spec(source).ok_or_else(|| {
        format!(
            "could not parse font specification {:?}",
            spec_string(source)
        )
    })?;
    FontStack::from_spec(&parsed, style, size_pts, dpi).map_err(|error| error.to_string())
}

/// One `FontStack` per terminal style.
pub struct FontSet {
    regular: FontStack,
    bold: Option<FontStack>,
    italic: Option<FontStack>,
    bold_italic: Option<FontStack>,
    pub metrics: FontCellMetrics,
}

impl FontSet {
    pub fn load(options: &Options, scale: f64) -> Result<Self, String> {
        let size = (options.font_size * scale).clamp(1.0, 512.0);
        let dpi = DEFAULT_DPI;
        let regular = load_stack(
            &options.font_family,
            &options.font_family,
            FontStyle::Regular,
            size,
            dpi,
        )
        .or_else(|_| {
            let spec = KittyFontSpec {
                system: Some("monospace".into()),
                ..KittyFontSpec::default()
            };
            FontStack::from_spec(&spec, FontStyle::Regular, size, dpi)
                .map_err(|error| error.to_string())
        })?;
        let mut fonts = Self {
            metrics: regular.primary_face().cell_metrics(),
            regular,
            bold: None,
            italic: None,
            bold_italic: None,
        };
        if fonts.metrics.cell_width == 0 {
            fonts.metrics.cell_width = 8;
        }
        if fonts.metrics.cell_height == 0 {
            fonts.metrics.cell_height = 16;
        }
        fonts.bold = load_stack(
            &options.bold_font,
            &options.font_family,
            FontStyle::Bold,
            size,
            dpi,
        )
        .ok();
        fonts.italic = load_stack(
            &options.italic_font,
            &options.font_family,
            FontStyle::Italic,
            size,
            dpi,
        )
        .ok();
        fonts.bold_italic = load_stack(
            &options.bold_italic_font,
            &options.font_family,
            FontStyle::BoldItalic,
            size,
            dpi,
        )
        .ok();
        Ok(fonts)
    }

    pub fn stack(&mut self, bold: bool, italic: bool) -> &mut FontStack {
        if bold && italic {
            if let Some(stack) = self.bold_italic.as_mut() {
                return stack;
            }
        }
        if bold {
            if let Some(stack) = self.bold.as_mut() {
                return stack;
            }
        }
        if italic {
            if let Some(stack) = self.italic.as_mut() {
                return stack;
            }
        }
        &mut self.regular
    }

    pub fn cell_metrics(&self) -> FontCellMetrics {
        self.metrics
    }
}

fn blit_gray(
    canvas: &mut [u8],
    canvas_width: u32,
    canvas_height: u32,
    bitmap: &kitty_fonts::GlyphBitmap,
    dest_x: i64,
    dest_y: i64,
) {
    for row in 0..bitmap.height as i64 {
        let y = dest_y + row;
        if y < 0 || y >= canvas_height as i64 {
            continue;
        }
        let src_start = row as usize * bitmap.pitch;
        let src = &bitmap.data[src_start..src_start + bitmap.width as usize];
        for col in 0..bitmap.width as i64 {
            let x = dest_x + col;
            if x < 0 || x >= canvas_width as i64 {
                continue;
            }
            canvas[y as usize * canvas_width as usize + x as usize] = src[col as usize];
        }
    }
}

fn blit_color(
    canvas: &mut [u8],
    canvas_width: u32,
    canvas_height: u32,
    bitmap: &kitty_fonts::GlyphBitmap,
    dest_x: i64,
    dest_y: i64,
    bgra: bool,
) {
    for row in 0..bitmap.height as i64 {
        let y = dest_y + row;
        if y < 0 || y >= canvas_height as i64 {
            continue;
        }
        let src_start = row as usize * bitmap.pitch;
        let src = &bitmap.data[src_start..src_start + bitmap.width as usize * 4];
        for col in 0..bitmap.width as i64 {
            let x = dest_x + col;
            if x < 0 || x >= canvas_width as i64 {
                continue;
            }
            let s = col as usize * 4;
            let d = (y as usize * canvas_width as usize + x as usize) * 4;
            if bgra {
                canvas[d] = src[s + 2];
                canvas[d + 1] = src[s + 1];
                canvas[d + 2] = src[s];
            } else {
                canvas[d..d + 3].copy_from_slice(&src[s..s + 3]);
            }
            canvas[d + 3] = src[s + 3];
        }
    }
}

fn scale_glyph(
    bitmap: kitty_fonts::GlyphBitmap,
    max_width: u32,
    max_height: u32,
) -> kitty_fonts::GlyphBitmap {
    if bitmap.width <= max_width && bitmap.height <= max_height {
        return bitmap;
    }
    if bitmap.width == 0 || bitmap.height == 0 {
        return bitmap;
    }
    let scale = (max_width as f64 / bitmap.width as f64)
        .min(max_height as f64 / bitmap.height as f64)
        .min(1.0);
    let width = ((bitmap.width as f64 * scale).round() as u32).max(1);
    let height = ((bitmap.height as f64 * scale).round() as u32).max(1);
    let bpp = bitmap.bytes_per_pixel();
    let mut data = vec![0u8; width as usize * height as usize * bpp];
    for y in 0..height as usize {
        let sy = ((y as f64 + 0.5) * bitmap.height as f64 / height as f64)
            .floor()
            .min(bitmap.height as f64 - 1.0) as usize;
        for x in 0..width as usize {
            let sx = ((x as f64 + 0.5) * bitmap.width as f64 / width as f64)
                .floor()
                .min(bitmap.width as f64 - 1.0) as usize;
            let src = sy * bitmap.pitch + sx * bpp;
            let dst = (y * width as usize + x) * bpp;
            data[dst..dst + bpp].copy_from_slice(&bitmap.data[src..src + bpp]);
        }
    }
    let scaled_left = (bitmap.left as f64 * scale).round() as i32;
    let scaled_top = (bitmap.top as f64 * scale).round() as i32;
    kitty_fonts::GlyphBitmap {
        width,
        height,
        left: scaled_left,
        top: scaled_top,
        advance_x: (bitmap.advance_x as f64 * scale).round() as i32,
        advance_y: (bitmap.advance_y as f64 * scale).round() as i32,
        advance_x_26_6: bitmap.advance_x_26_6,
        advance_y_26_6: bitmap.advance_y_26_6,
        format: bitmap.format,
        pitch: width as usize * bpp,
        data,
    }
}

fn compose_bitmap(
    bitmap: kitty_fonts::GlyphBitmap,
    width_cells: u32,
    metrics: FontCellMetrics,
) -> Option<RenderGlyphBitmap> {
    if bitmap.is_empty() {
        return None;
    }
    let cell_width = metrics.cell_width.max(1);
    let cell_height = metrics.cell_height.max(1);
    let canvas_width = cell_width.saturating_mul(width_cells.clamp(1, 4));
    let canvas_height = cell_height;
    let bitmap = if matches!(bitmap.format, PixelFormat::Rgba8 | PixelFormat::Bgra8) {
        scale_glyph(bitmap, canvas_width, canvas_height)
    } else {
        bitmap
    };
    let baseline = metrics.baseline.min(canvas_height);
    let mut dest_x = bitmap.left as i64;
    if dest_x > 0 && dest_x + bitmap.width as i64 > canvas_width as i64 {
        let extra = dest_x + bitmap.width as i64 - canvas_width as i64;
        dest_x = if extra > dest_x { 0 } else { dest_x - extra };
    }
    dest_x = dest_x.max(0);
    let dest_y = baseline as i64 - bitmap.top as i64;

    match bitmap.format {
        PixelFormat::Gray8 => {
            let mut canvas = vec![0u8; canvas_width as usize * canvas_height as usize];
            blit_gray(
                &mut canvas,
                canvas_width,
                canvas_height,
                &bitmap,
                dest_x,
                dest_y,
            );
            RenderGlyphBitmap::gray(canvas_width, canvas_height, canvas).ok()
        }
        PixelFormat::Rgba8 | PixelFormat::Bgra8 => {
            let mut canvas = vec![0u8; canvas_width as usize * canvas_height as usize * 4];
            blit_color(
                &mut canvas,
                canvas_width,
                canvas_height,
                &bitmap,
                dest_x,
                dest_y,
                bitmap.format == PixelFormat::Bgra8,
            );
            RenderGlyphBitmap::rgba(canvas_width, canvas_height, canvas).ok()
        }
    }
}

fn blend_gray(
    canvas: &mut [u8],
    width: u32,
    height: u32,
    bitmap: &kitty_fonts::GlyphBitmap,
    x0: i64,
    y0: i64,
) {
    for row in 0..bitmap.height as i64 {
        let y = y0 + row;
        if y < 0 || y >= height as i64 {
            continue;
        }
        for col in 0..bitmap.width as i64 {
            let x = x0 + col;
            if x < 0 || x >= width as i64 {
                continue;
            }
            let src = &bitmap.data[row as usize * bitmap.pitch + col as usize];
            let dst = &mut canvas[y as usize * width as usize + x as usize];
            *dst = (*dst).max(*src);
        }
    }
}

fn blend_rgba(
    canvas: &mut [u8],
    width: u32,
    height: u32,
    bitmap: &kitty_fonts::GlyphBitmap,
    x0: i64,
    y0: i64,
    bgra: bool,
) {
    let bpp = 4usize;
    for row in 0..bitmap.height as i64 {
        let y = y0 + row;
        if y < 0 || y >= height as i64 {
            continue;
        }
        for col in 0..bitmap.width as i64 {
            let x = x0 + col;
            if x < 0 || x >= width as i64 {
                continue;
            }
            let src = row as usize * bitmap.pitch + col as usize * bpp;
            let s = &bitmap.data[src..src + bpp];
            let (sr, sg, sb, sa) = if bgra {
                (s[2] as u32, s[1] as u32, s[0] as u32, s[3] as u32)
            } else {
                (s[0] as u32, s[1] as u32, s[2] as u32, s[3] as u32)
            };
            if sa == 0 {
                continue;
            }
            let dst = (y as usize * width as usize + x as usize) * bpp;
            if sa == 255 {
                canvas[dst] = sr as u8;
                canvas[dst + 1] = sg as u8;
                canvas[dst + 2] = sb as u8;
                canvas[dst + 3] = 255;
                continue;
            }
            let da = canvas[dst + 3] as u32;
            let out_a = sa + da * (255 - sa) / 255;
            let mix = |sc: u32, dc: u32| -> u8 {
                if out_a == 0 {
                    0
                } else {
                    ((sc * sa * 255 + dc * da * (255 - sa)) / (out_a * 255)) as u8
                }
            };
            canvas[dst] = mix(sr, canvas[dst] as u32);
            canvas[dst + 1] = mix(sg, canvas[dst + 1] as u32);
            canvas[dst + 2] = mix(sb, canvas[dst + 2] as u32);
            canvas[dst + 3] = out_a as u8;
        }
    }
}

fn compose_shaped_run(
    stack: &mut FontStack,
    run: &ShapedRunRequest<'_>,
    metrics: FontCellMetrics,
) -> Option<ShapedRunOutput> {
    if run.text.is_empty() || run.cell_for_codepoint.len() != run.text.len() || run.cell_count == 0
    {
        return None;
    }
    let options = kitty_fonts::ShapeOptions::default();
    let shaped = stack.shape(run.text, &options).ok()?;
    if shaped.runs.is_empty() {
        return None;
    }
    let cell_width = metrics.cell_width.max(1) as i32;
    let cell_height = metrics.cell_height.max(1) as i32;
    let baseline = metrics.baseline.min(metrics.cell_height) as i32;
    let canvas_width = cell_width.checked_mul(run.cell_count as i32)?;
    if canvas_width <= 0 || cell_height <= 0 {
        return None;
    }
    let mut any_color = false;
    let mut glyphs = Vec::new();
    let mut pen_x = 0.0f32;
    let mut pen_y = 0.0f32;
    for segment in &shaped.runs {
        let face = stack.faces().get(segment.face_index)?;
        for glyph in &segment.run.glyphs {
            let bitmap = face.rasterize_glyph(glyph.glyph_id).ok()?;
            if bitmap.format != PixelFormat::Gray8 {
                any_color = true;
            }
            glyphs.push((pen_x, pen_y, glyph.x_offset, glyph.y_offset, bitmap));
            pen_x += (glyph.x_advance as f32 / 64.0).round();
            pen_y += (glyph.y_advance as f32 / 64.0).round();
        }
    }
    if glyphs.is_empty() {
        return None;
    }
    let mut gray = vec![0u8; canvas_width as usize * cell_height as usize];
    let mut rgba = if any_color {
        vec![0u8; canvas_width as usize * cell_height as usize * 4]
    } else {
        Vec::new()
    };
    for (index, (glyph_x, glyph_y, x_offset, y_offset, bitmap)) in glyphs.into_iter().enumerate() {
        let x = glyph_x + x_offset as f32 / 64.0;
        let y = glyph_y + y_offset as f32 / 64.0;
        let mut dest_x = (x + bitmap.left as f32) as i32;
        let xoff = dest_x;
        if xoff > 0 && index < 4 && xoff as i64 + bitmap.width as i64 > canvas_width as i64 {
            let extra = xoff as i64 + bitmap.width as i64 - canvas_width as i64;
            dest_x = if extra > xoff as i64 {
                0
            } else {
                xoff - extra as i32
            };
        }
        let yoff = (y + bitmap.top as f32) as i32;
        let dest_y = if yoff > 0 && yoff > baseline {
            0
        } else {
            baseline - yoff
        };
        match bitmap.format {
            PixelFormat::Gray8 => {
                if any_color {
                    let mut layer = vec![0u8; bitmap.width as usize * bitmap.height as usize * 4];
                    for row in 0..bitmap.height as usize {
                        for col in 0..bitmap.width as usize {
                            let a = bitmap.data[row * bitmap.pitch + col];
                            let d = (row * bitmap.width as usize + col) * 4;
                            layer[d..d + 4].copy_from_slice(&[255, 255, 255, a]);
                        }
                    }
                    let colored = kitty_fonts::GlyphBitmap {
                        pitch: bitmap.width as usize * 4,
                        format: PixelFormat::Rgba8,
                        data: layer,
                        ..bitmap
                    };
                    blend_rgba(
                        &mut rgba,
                        canvas_width as u32,
                        cell_height as u32,
                        &colored,
                        dest_x as i64,
                        dest_y as i64,
                        false,
                    );
                } else {
                    blend_gray(
                        &mut gray,
                        canvas_width as u32,
                        cell_height as u32,
                        &bitmap,
                        dest_x as i64,
                        dest_y as i64,
                    );
                }
            }
            PixelFormat::Rgba8 | PixelFormat::Bgra8 => {
                let bgra = bitmap.format == PixelFormat::Bgra8;
                let scaled = scale_glyph(bitmap, canvas_width as u32, cell_height as u32);
                blend_rgba(
                    &mut rgba,
                    canvas_width as u32,
                    cell_height as u32,
                    &scaled,
                    dest_x as i64,
                    dest_y as i64,
                    bgra,
                );
            }
        }
    }
    let bitmap = if any_color {
        RenderGlyphBitmap::rgba(canvas_width as u32, cell_height as u32, rgba).ok()?
    } else {
        RenderGlyphBitmap::gray(canvas_width as u32, cell_height as u32, gray).ok()?
    };
    Some(ShapedRunOutput {
        clusters: vec![ShapedRunCluster {
            cell_start: 0,
            cell_span: run.cell_count,
            bitmap,
        }],
    })
}

/// Adapter used by [`kitty_render::RendererCore`].
pub struct FontRasterizer<'a> {
    pub fonts: &'a mut FontSet,
    pub cache: &'a mut RasterCache,
    pub metrics: FontCellMetrics,
}

impl GlyphRasterizer for FontRasterizer<'_> {
    fn rasterize(&mut self, key: &GlyphKey) -> Option<RenderGlyphBitmap> {
        let chars = key.text.as_slice();
        if chars.is_empty() {
            return None;
        }
        let codepoint = chars
            .iter()
            .copied()
            .find(|cp| *cp != 0 && *cp != b' ' as u32)?;
        let bold = key.flags & GLYPH_FLAG_BOLD != 0;
        let italic = key.flags & GLYPH_FLAG_ITALIC != 0;
        let stack = self.fonts.stack(bold, italic);
        let (_, bitmap) = stack
            .rasterize_codepoint_cached(self.cache, codepoint)
            .ok()??;
        let width_cells = wcswidth_std(chars).max(1);
        compose_bitmap(bitmap, width_cells, self.metrics)
    }

    fn shape_run(&mut self, run: &ShapedRunRequest<'_>) -> Option<ShapedRunOutput> {
        let bold = run.flags & GLYPH_FLAG_BOLD != 0;
        let italic = run.flags & GLYPH_FLAG_ITALIC != 0;
        let metrics = self.metrics;
        let stack = self.fonts.stack(bold, italic);
        compose_shaped_run(stack, run, metrics)
    }
}
