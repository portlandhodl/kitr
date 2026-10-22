// SPDX-License-Identifier: GPL-3.0-or-later
//! Font loading and the bridge from the renderer's glyph keys to FreeType.

use kitty_config::Options;
use kitty_fonts::{
    FontCellMetrics, FontSpec as KittyFontSpec, FontStack, FontStyle, PixelFormat, RasterCache,
    DEFAULT_DPI,
};
use kitty_render::{
    GlyphBitmap as RenderGlyphBitmap, GlyphKey, GlyphRasterizer, GLYPH_FLAG_BOLD, GLYPH_FLAG_ITALIC,
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
    let baseline = metrics.baseline.min(canvas_height);
    let dest_x = bitmap.left.max(0) as i64;
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
}
