// SPDX-License-Identifier: GPL-3.0-or-later
//! COLR/CPAL and bitmap colour-glyph compositing.
//!
//! FreeType exposes COLR v0 layers and the COLR v1 paint graph, plus CBDT/CBLC
//! and SVG colour bitmaps through `FT_LOAD_COLOR`. This module turns all of
//! those into the same tightly packed RGBA bitmap used by the renderer. It is
//! deliberately self contained: callers only need [`ColorGlyphOptions`].

use std::collections::HashMap;

use crate::error::{Error, Result};
use crate::face::Face;
use crate::ffi::freetype::{self, FT_Color, FT_Face, FT_OpaquePaint};
use crate::raster::{GlyphBitmap, PixelFormat};

/// Classification of a colour glyph, mirroring kitty's `GlyphColorType`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ColorGlyphType {
    /// A CBDT/CBLC or SVG bitmap rendered by FreeType itself.
    Bitmap,
    /// COLR v0 layers with CPAL colours.
    ColrV0,
    /// COLR v1 paint graph.
    ColrV1,
}

/// Foreground and palette selection for colour glyphs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ColorGlyphOptions {
    /// RGB used for CPAL palette index 0xFFFF.
    pub foreground: [u8; 3],
    /// Select the palette marked for dark backgrounds.
    pub dark_background: bool,
    /// Override the automatic palette choice.
    pub palette_index: Option<u16>,
}

impl Default for ColorGlyphOptions {
    fn default() -> Self {
        Self {
            foreground: [255, 255, 255],
            dark_background: true,
            palette_index: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Affine {
    xx: f64,
    xy: f64,
    dx: f64,
    yx: f64,
    yy: f64,
    dy: f64,
}

impl Affine {
    const IDENTITY: Self = Self {
        xx: 1.0,
        xy: 0.0,
        dx: 0.0,
        yx: 0.0,
        yy: 1.0,
        dy: 0.0,
    };

    fn then(self, other: Self) -> Self {
        Self {
            xx: self.xx * other.xx + self.xy * other.yx,
            xy: self.xx * other.xy + self.xy * other.yy,
            dx: self.xx * other.dx + self.xy * other.dy + self.dx,
            yx: self.yx * other.xx + self.yy * other.yx,
            yy: self.yx * other.xy + self.yy * other.yy,
            dy: self.yx * other.dx + self.yy * other.dy + self.dy,
        }
    }

    fn from_ft(value: freetype::FT_Affine23) -> Self {
        Self {
            xx: value.xx as f64 / 65536.0,
            xy: value.xy as f64 / 65536.0,
            dx: value.dx as f64 / 65536.0,
            yx: value.yx as f64 / 65536.0,
            yy: value.yy as f64 / 65536.0,
            dy: value.dy as f64 / 65536.0,
        }
    }

    fn to_ft(self, units_x: f64, units_y: f64) -> (freetype::FT_Matrix, freetype::FT_Vector) {
        let matrix = freetype::FT_Matrix {
            xx: (self.xx * 65536.0).round() as freetype::FT_Fixed,
            xy: (self.xy * 65536.0).round() as freetype::FT_Fixed,
            yx: (self.yx * 65536.0).round() as freetype::FT_Fixed,
            yy: (self.yy * 65536.0).round() as freetype::FT_Fixed,
        };
        let delta = freetype::FT_Vector {
            x: (self.dx * units_x * 64.0).round() as freetype::FT_Pos,
            y: (self.dy * units_y * 64.0).round() as freetype::FT_Pos,
        };
        (matrix, delta)
    }
}

/// RGBA straight-alpha pixel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Rgba([u8; 4]);

impl Rgba {
    const TRANSPARENT: Self = Self([0, 0, 0, 0]);

    fn new(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self([r, g, b, a])
    }

    fn from_ft(color: FT_Color) -> Self {
        Self([color.red, color.green, color.blue, color.alpha])
    }

    fn over(self, under: Self) -> Self {
        let sa = self.0[3] as u32;
        let ua = under.0[3] as u32;
        if sa == 0 {
            return under;
        }
        if sa == 255 {
            return self;
        }
        let out_a = sa + ua * (255 - sa) / 255;
        if out_a == 0 {
            return Self::TRANSPARENT;
        }
        let channel = |s: u8, u: u8| -> u8 {
            let num = s as u32 * sa * 255 + u as u32 * ua * (255 - sa);
            (num / (out_a * 255)) as u8
        };
        Self([
            channel(self.0[0], under.0[0]),
            channel(self.0[1], under.0[1]),
            channel(self.0[2], under.0[2]),
            out_a as u8,
        ])
    }

    fn from_indexed(entries: &[FT_Color], foreground: [u8; 3], index: usize, alpha: f64) -> Self {
        let base = if index == freetype::FT_PALETTE_FOREGROUND_INDEX as usize {
            Self::new(foreground[0], foreground[1], foreground[2], 255)
        } else {
            entries
                .get(index)
                .copied()
                .map(Self::from_ft)
                .unwrap_or(Self::TRANSPARENT)
        };
        let a = (base.0[3] as f64 * alpha.clamp(0.0, 1.0)).round() as u8;
        Self::new(base.0[0], base.0[1], base.0[2], a)
    }
}

#[derive(Clone)]
struct Layer {
    left: i32,
    top: i32,
    width: u32,
    height: u32,
    data: Vec<u8>,
}

impl Layer {
    fn union_bounds(layers: &[Layer]) -> Option<(i32, i32, i32, i32)> {
        let mut iter = layers.iter().filter(|l| l.width > 0 && l.height > 0);
        let first = iter.next()?;
        let mut left = first.left;
        let mut right = first.left + first.width as i32;
        let mut top = first.top;
        let mut bottom = first.top - first.height as i32;
        for layer in iter {
            left = left.min(layer.left);
            right = right.max(layer.left + layer.width as i32);
            top = top.max(layer.top);
            bottom = bottom.min(layer.top - layer.height as i32);
        }
        Some((left, top, right, bottom))
    }

    fn blit_into(&self, canvas: &mut [u8], width: u32, height: u32, left: i32, top: i32) {
        let dest_x = self.left - left;
        let dest_y = top - self.top;
        for row in 0..self.height as i32 {
            let y = dest_y + row;
            if y < 0 || y >= height as i32 {
                continue;
            }
            for col in 0..self.width as i32 {
                let x = dest_x + col;
                if x < 0 || x >= width as i32 {
                    continue;
                }
                let src = ((row as u32 * self.width + col as u32) * 4) as usize;
                let dst = ((y as u32 * width + x as u32) * 4) as usize;
                let s = Rgba([
                    self.data[src],
                    self.data[src + 1],
                    self.data[src + 2],
                    self.data[src + 3],
                ]);
                let d = Rgba([
                    canvas[dst],
                    canvas[dst + 1],
                    canvas[dst + 2],
                    canvas[dst + 3],
                ]);
                let out = s.over(d);
                canvas[dst..dst + 4].copy_from_slice(&out.0);
            }
        }
    }
}

fn layer_from_alpha_mask(bitmap: &GlyphBitmap, color: Rgba) -> Layer {
    let mut data = vec![0u8; bitmap.width as usize * bitmap.height as usize * 4];
    for row in 0..bitmap.height as usize {
        for col in 0..bitmap.width as usize {
            let src = row * bitmap.pitch + col;
            let a = bitmap.data.get(src).copied().unwrap_or(0);
            let out = Rgba::new(
                color.0[0],
                color.0[1],
                color.0[2],
                (color.0[3] as u32 * a as u32 / 255) as u8,
            );
            let dst = (row * bitmap.width as usize + col) * 4;
            data[dst..dst + 4].copy_from_slice(&out.0);
        }
    }
    Layer {
        left: bitmap.left,
        top: bitmap.top,
        width: bitmap.width,
        height: bitmap.height,
        data,
    }
}

fn layer_from_rgba(bitmap: &GlyphBitmap) -> Layer {
    let mut data = vec![0u8; bitmap.width as usize * bitmap.height as usize * 4];
    for row in 0..bitmap.height as usize {
        let start = row * bitmap.pitch;
        let src = &bitmap.data[start..start + bitmap.width as usize * 4];
        match bitmap.format {
            PixelFormat::Bgra8 => {
                for col in 0..bitmap.width as usize {
                    let i = col * 4;
                    let o = (row * bitmap.width as usize + col) * 4;
                    data[o] = src[i + 2];
                    data[o + 1] = src[i + 1];
                    data[o + 2] = src[i];
                    data[o + 3] = src[i + 3];
                }
            }
            _ => data[start / 4 * 4..start + bitmap.width as usize * 4].copy_from_slice(src),
        }
    }
    Layer {
        left: bitmap.left,
        top: bitmap.top,
        width: bitmap.width,
        height: bitmap.height,
        data,
    }
}

fn bitmap_from_layers(layers: &[Layer]) -> Result<GlyphBitmap> {
    let Some((left, top, right, bottom)) = Layer::union_bounds(layers) else {
        return Ok(empty_bitmap(
            layers.first().map(|l| l.left).unwrap_or(0),
            layers.first().map(|l| l.top).unwrap_or(0),
        ));
    };
    let width = (right - left).max(0) as u32;
    let height = (top - bottom).max(0) as u32;
    let mut canvas = vec![0u8; width as usize * height as usize * 4];
    for layer in layers {
        layer.blit_into(&mut canvas, width, height, left, top);
    }
    Ok(GlyphBitmap {
        width,
        height,
        left,
        top,
        advance_x: layers.first().map(|_| 0).unwrap_or(0),
        advance_y: 0,
        advance_x_26_6: 0,
        advance_y_26_6: 0,
        format: PixelFormat::Rgba8,
        pitch: width as usize * 4,
        data: canvas,
    })
}

fn empty_bitmap(left: i32, top: i32) -> GlyphBitmap {
    GlyphBitmap {
        width: 0,
        height: 0,
        left,
        top,
        advance_x: 0,
        advance_y: 0,
        advance_x_26_6: 0,
        advance_y_26_6: 0,
        format: PixelFormat::Rgba8,
        pitch: 0,
        data: Vec::new(),
    }
}

/// Return the colour technology used by `glyph_id`, or `None`.
pub fn glyph_color_type(face: &Face, glyph_id: u32) -> Option<ColorGlyphType> {
    if !face.has_color() || glyph_id >= face.num_glyphs() {
        return None;
    }
    let raw = face.ffi_face();
    let mut paint = freetype::FT_OpaquePaint::default();
    let is_v1 = unsafe {
        freetype::FT_Get_Color_Glyph_Paint(
            raw,
            glyph_id,
            freetype::FT_COLOR_NO_ROOT_TRANSFORM,
            &mut paint,
        ) != 0
    };
    if is_v1 {
        return Some(ColorGlyphType::ColrV1);
    }
    let mut iterator = freetype::FT_LayerIterator::default();
    let mut layer_glyph = 0;
    let mut color_index = 0;
    let is_v0 = unsafe {
        freetype::FT_Get_Color_Glyph_Layer(
            raw,
            glyph_id,
            &mut layer_glyph,
            &mut color_index,
            &mut iterator,
        ) != 0
    };
    if is_v0 {
        return Some(ColorGlyphType::ColrV0);
    }
    Some(ColorGlyphType::Bitmap)
}

fn choose_palette(face: FT_Face, options: &ColorGlyphOptions) -> u16 {
    if let Some(index) = options.palette_index {
        return index;
    }
    let mut data = freetype::FT_Palette_Data::default();
    if unsafe { freetype::FT_Palette_Data_Get(face, &mut data) } != 0 || data.num_palettes == 0 {
        return 0;
    }
    if data.palette_flags.is_null() {
        return 0;
    }
    let wanted = if options.dark_background {
        freetype::FT_PALETTE_FOR_DARK_BACKGROUND
    } else {
        freetype::FT_PALETTE_FOR_LIGHT_BACKGROUND
    };
    for index in 0..data.num_palettes as usize {
        let flags = unsafe { *data.palette_flags.add(index) };
        if flags & wanted != 0 {
            return index as u16;
        }
    }
    0
}

fn palette_entries(face: FT_Face, options: &ColorGlyphOptions) -> Vec<FT_Color> {
    let index = choose_palette(face, options);
    let mut entries: *mut FT_Color = std::ptr::null_mut();
    if unsafe { freetype::FT_Palette_Select(face, index, &mut entries) } != 0 || entries.is_null() {
        return Vec::new();
    }
    let mut data = freetype::FT_Palette_Data::default();
    if unsafe { freetype::FT_Palette_Data_Get(face, &mut data) } != 0 {
        return Vec::new();
    }
    unsafe { std::slice::from_raw_parts(entries, data.num_palette_entries as usize).to_vec() }
}

struct ColorContext {
    face: FT_Face,
    load_flags: i32,
    units_x: f64,
    units_y: f64,
    palette: Vec<FT_Color>,
    options: ColorGlyphOptions,
}

impl ColorContext {
    fn color_for(&self, index: usize, alpha: f64) -> Rgba {
        Rgba::from_indexed(&self.palette, self.options.foreground, index, alpha)
    }

    fn gradient_stops(&self, line: &freetype::FT_ColorLine) -> Vec<(f64, Rgba)> {
        let mut iterator = line.color_stop_iterator;
        let mut ans = Vec::new();
        loop {
            let mut stop = freetype::FT_ColorStop::default();
            if unsafe { freetype::FT_Get_Colorline_Stops(self.face, &mut stop, &mut iterator) } == 0
            {
                break;
            }
            ans.push((
                stop.stop_offset as f64 / 65536.0,
                self.color_for(
                    stop.color.palette_index as usize,
                    stop.color.alpha as f64 / 16384.0,
                ),
            ));
        }
        ans
    }

    fn stops_color(stops: &[(f64, Rgba)], t: f64, extend: i32) -> Rgba {
        if stops.is_empty() {
            return Rgba::TRANSPARENT;
        }
        let mut t = t;
        if extend == 1 {
            t = t.rem_euclid(1.0);
        } else if extend == 2 {
            t = t.rem_euclid(2.0);
            if t > 1.0 {
                t = 2.0 - t;
            }
        }
        let t = t.clamp(0.0, 1.0);
        let mut prev = stops[0];
        for &next in &stops[1..] {
            if t <= next.0 {
                let span = (next.0 - prev.0).max(1e-9);
                let f = ((t - prev.0) / span).clamp(0.0, 1.0);
                let mix =
                    |a: u8, b: u8| -> u8 { (a as f64 + (b as f64 - a as f64) * f).round() as u8 };
                return Rgba::new(
                    mix(prev.1 .0[0], next.1 .0[0]),
                    mix(prev.1 .0[1], next.1 .0[1]),
                    mix(prev.1 .0[2], next.1 .0[2]),
                    mix(prev.1 .0[3], next.1 .0[3]),
                );
            }
            prev = next;
        }
        prev.1
    }
}

/// Rasterize a colour glyph. Returns `Ok(None)` when the glyph is not a colour
/// glyph or when the face has no colour technology.
pub fn rasterize_color_glyph(
    face: &Face,
    glyph_id: u32,
    options: &ColorGlyphOptions,
) -> Result<Option<GlyphBitmap>> {
    let Some(kind) = glyph_color_type(face, glyph_id) else {
        return Ok(None);
    };
    let raw = face.ffi_face();
    let (units_x, units_y) = face.pixels_per_font_unit();
    let ctx = ColorContext {
        face: raw,
        load_flags: face.load_flags_value(),
        units_x,
        units_y,
        palette: palette_entries(raw, options),
        options: *options,
    };
    let mut bitmap = match kind {
        ColorGlyphType::ColrV0 => render_colrv0(&ctx, glyph_id)?,
        ColorGlyphType::ColrV1 => render_colrv1(&ctx, glyph_id)?,
        ColorGlyphType::Bitmap => {
            let bitmap = match face.rasterize_glyph_with_flags(
                glyph_id,
                freetype::FT_LOAD_DEFAULT | freetype::FT_LOAD_RENDER | freetype::FT_LOAD_COLOR,
            ) {
                Ok(bitmap) => bitmap,
                Err(_) => {
                    return Ok(decode_cbdt_glyph(face, glyph_id));
                }
            };
            match bitmap.format {
                PixelFormat::Bgra8 | PixelFormat::Rgba8 => {
                    bitmap_from_layers(&[layer_from_rgba(&bitmap)])?
                }
                _ => return Ok(decode_cbdt_glyph(face, glyph_id)),
            }
        }
    };
    // Preserve the advance from the base glyph; colour painting only supplies
    // the bitmap. A missing advance would make the caller fall back to a cell
    // width guess.
    if let Ok(metrics) = face.glyph_metrics(glyph_id) {
        bitmap.advance_x_26_6 = metrics.advance_x_26_6;
        bitmap.advance_y_26_6 = metrics.advance_y_26_6;
        bitmap.advance_x = crate::shaping::ceil_div_64_public(metrics.advance_x_26_6);
        bitmap.advance_y = crate::shaping::ceil_div_64_public(metrics.advance_y_26_6);
    }
    fn _unused(_: &ColorContext) {}
    Ok(Some(bitmap))
}

fn render_colrv0(ctx: &ColorContext, glyph_id: u32) -> Result<GlyphBitmap> {
    let mut iterator = freetype::FT_LayerIterator::default();
    let mut layers = Vec::new();
    loop {
        let mut layer_glyph = 0;
        let mut color_index = 0;
        if unsafe {
            freetype::FT_Get_Color_Glyph_Layer(
                ctx.face,
                glyph_id,
                &mut layer_glyph,
                &mut color_index,
                &mut iterator,
            )
        } == 0
        {
            break;
        }
        let Ok(mask) = load_and_render_mask(ctx, layer_glyph, Affine::IDENTITY) else {
            continue;
        };
        let color = ctx.color_for(color_index as usize, 1.0);
        layers.push(layer_from_alpha_mask(&mask, color));
    }
    bitmap_from_layers(&layers)
}

fn load_and_render_mask(
    ctx: &ColorContext,
    glyph_id: u32,
    transform: Affine,
) -> Result<GlyphBitmap> {
    let (matrix, delta) = transform.to_ft(ctx.units_x, ctx.units_y);
    unsafe {
        freetype::FT_Set_Transform(ctx.face, &matrix, &delta);
    }
    let result = load_mask_untransformed(ctx, glyph_id);
    unsafe {
        freetype::FT_Set_Transform(
            ctx.face,
            &freetype::FT_Matrix {
                xx: 0x10000,
                xy: 0,
                yx: 0,
                yy: 0x10000,
            },
            &freetype::FT_Vector { x: 0, y: 0 },
        );
    }
    result
}

fn load_mask_untransformed(ctx: &ColorContext, glyph_id: u32) -> Result<GlyphBitmap> {
    let flags = ctx.load_flags | freetype::FT_LOAD_RENDER;
    let code = unsafe { freetype::FT_Load_Glyph(ctx.face, glyph_id, flags) };
    if code != 0 {
        return Err(Error::FreeType {
            operation: "FT_Load_Glyph",
            code,
        });
    }
    let slot = unsafe { (*ctx.face).glyph };
    if slot.is_null() {
        return Err(Error::Raster("FreeType returned a null glyph slot"));
    }
    if unsafe { (*slot).format } == freetype::FT_GLYPH_FORMAT_OUTLINE {
        let code = unsafe { freetype::FT_Render_Glyph(slot, freetype::FT_RENDER_MODE_NORMAL) };
        if code != 0 {
            return Err(Error::FreeType {
                operation: "FT_Render_Glyph",
                code,
            });
        }
    }
    let slot = unsafe { &*slot };
    if slot.format != freetype::FT_GLYPH_FORMAT_BITMAP {
        return Err(Error::UnsupportedGlyphFormat(slot.format));
    }
    let bitmap = &slot.bitmap;
    if bitmap.pixel_mode != freetype::FT_PIXEL_MODE_GRAY {
        // Mono and packed gray modes are converted by the normal rasterizer;
        // colour layers are almost always 8-bit gray.
        return Err(Error::UnsupportedPixelMode(bitmap.pixel_mode));
    }
    let width = bitmap.width;
    let height = bitmap.rows;
    let mut data = Vec::new();
    if width > 0 && height > 0 {
        if bitmap.buffer.is_null() {
            return Err(Error::Raster("FreeType returned a null bitmap buffer"));
        }
        data.resize(width as usize * height as usize, 0);
        for row in 0..height as usize {
            unsafe {
                std::ptr::copy_nonoverlapping(
                    bitmap.buffer.offset(row as isize * bitmap.pitch as isize),
                    data[row * width as usize..].as_mut_ptr(),
                    width as usize,
                );
            }
        }
    }
    Ok(GlyphBitmap {
        width,
        height,
        left: slot.bitmap_left,
        top: slot.bitmap_top,
        advance_x: 0,
        advance_y: 0,
        advance_x_26_6: 0,
        advance_y_26_6: 0,
        format: PixelFormat::Gray8,
        pitch: width as usize,
        data,
    })
}

fn render_colrv1(ctx: &ColorContext, glyph_id: u32) -> Result<GlyphBitmap> {
    let mut paint = freetype::FT_OpaquePaint::default();
    if unsafe {
        freetype::FT_Get_Color_Glyph_Paint(
            ctx.face,
            glyph_id,
            freetype::FT_COLOR_NO_ROOT_TRANSFORM,
            &mut paint,
        )
    } == 0
    {
        return Ok(empty_bitmap(0, 0));
    }
    let mut layers = Vec::new();
    paint_node(ctx, paint, Affine::IDENTITY, &mut layers, 0)?;
    if layers.is_empty() {
        // Fall back to FreeType's own colour bitmap (CBDT/SVG).
        let bitmap = load_color_bitmap(ctx, glyph_id)?;
        return Ok(bitmap);
    }
    bitmap_from_layers(&layers)
}

fn load_color_bitmap(ctx: &ColorContext, glyph_id: u32) -> Result<GlyphBitmap> {
    let flags = ctx.load_flags | freetype::FT_LOAD_RENDER | freetype::FT_LOAD_COLOR;
    let code = unsafe { freetype::FT_Load_Glyph(ctx.face, glyph_id, flags) };
    if code != 0 {
        return Err(Error::FreeType {
            operation: "FT_Load_Glyph",
            code,
        });
    }
    let slot = unsafe { (*ctx.face).glyph };
    if slot.is_null() {
        return Ok(empty_bitmap(0, 0));
    }
    if unsafe { (*slot).format } == freetype::FT_GLYPH_FORMAT_OUTLINE {
        unsafe { freetype::FT_Render_Glyph(slot, freetype::FT_RENDER_MODE_NORMAL) };
    }
    let slot = unsafe { &*slot };
    let bitmap = &slot.bitmap;
    if bitmap.pixel_mode != freetype::FT_PIXEL_MODE_BGRA || bitmap.width == 0 || bitmap.rows == 0 {
        return Ok(empty_bitmap(slot.bitmap_left, slot.bitmap_top));
    }
    let width = bitmap.width;
    let height = bitmap.rows;
    let mut data = vec![0u8; width as usize * height as usize * 4];
    for row in 0..height as usize {
        unsafe {
            std::ptr::copy_nonoverlapping(
                bitmap.buffer.offset(row as isize * bitmap.pitch as isize),
                data[row * width as usize * 4..].as_mut_ptr(),
                width as usize * 4,
            );
        }
    }
    Ok(GlyphBitmap {
        width,
        height,
        left: slot.bitmap_left,
        top: slot.bitmap_top,
        advance_x: 0,
        advance_y: 0,
        advance_x_26_6: slot.advance.x as i32,
        advance_y_26_6: slot.advance.y as i32,
        format: PixelFormat::Bgra8,
        pitch: width as usize * 4,
        data,
    })
}

fn paint_node(
    ctx: &ColorContext,
    opaque: FT_OpaquePaint,
    transform: Affine,
    out: &mut Vec<Layer>,
    depth: u32,
) -> Result<()> {
    if depth > 32 {
        return Err(Error::Raster("COLR v1 paint graph is too deep"));
    }
    let mut paint = freetype::FT_COLR_Paint {
        format: 0,
        u: freetype::FT_COLR_PaintUnion {
            colr_layers: freetype::FT_PaintColrLayers::default(),
        },
    };
    if unsafe { freetype::FT_Get_Paint(ctx.face, opaque, &mut paint) } == 0 {
        return Ok(());
    }
    match paint.format {
        freetype::FT_COLR_PAINTFORMAT_COLR_LAYERS => {
            let mut iterator = unsafe { paint.u.colr_layers.layer_iterator };
            loop {
                let mut child = freetype::FT_OpaquePaint::default();
                if unsafe { freetype::FT_Get_Paint_Layers(ctx.face, &mut iterator, &mut child) }
                    == 0
                {
                    break;
                }
                paint_node(ctx, child, transform, out, depth + 1)?;
            }
        }
        freetype::FT_COLR_PAINTFORMAT_GLYPH => {
            let glyph = unsafe { paint.u.glyph };
            let mask = load_and_render_mask(ctx, glyph.glyphID, transform)?;
            if let Some(layer) = paint_mask(ctx, glyph.paint, transform, &mask, depth + 1)? {
                out.push(layer);
            }
        }
        freetype::FT_COLR_PAINTFORMAT_COLR_GLYPH => {
            let glyph = unsafe { paint.u.colr_glyph };
            let mut child = freetype::FT_OpaquePaint::default();
            if unsafe {
                freetype::FT_Get_Color_Glyph_Paint(
                    ctx.face,
                    glyph.glyphID,
                    freetype::FT_COLOR_NO_ROOT_TRANSFORM,
                    &mut child,
                )
            } != 0
            {
                paint_node(ctx, child, transform, out, depth + 1)?;
            }
        }
        freetype::FT_COLR_PAINTFORMAT_TRANSFORM => {
            let value = unsafe { paint.u.transform };
            paint_node(
                ctx,
                value.paint,
                transform.then(Affine::from_ft(value.affine)),
                out,
                depth + 1,
            )?;
        }
        freetype::FT_COLR_PAINTFORMAT_TRANSLATE => {
            let value = unsafe { paint.u.translate };
            paint_node(
                ctx,
                value.paint,
                transform.then(Affine {
                    dx: value.dx as f64 / 65536.0,
                    dy: value.dy as f64 / 65536.0,
                    ..Affine::IDENTITY
                }),
                out,
                depth + 1,
            )?;
        }
        freetype::FT_COLR_PAINTFORMAT_SCALE => {
            let value = unsafe { paint.u.scale };
            let sx = value.scale_x as f64 / 65536.0;
            let sy = value.scale_y as f64 / 65536.0;
            let cx = value.center_x as f64 / 65536.0;
            let cy = value.center_y as f64 / 65536.0;
            let affine = Affine {
                xx: sx,
                xy: 0.0,
                dx: cx - sx * cx,
                yx: 0.0,
                yy: sy,
                dy: cy - sy * cy,
            };
            paint_node(ctx, value.paint, transform.then(affine), out, depth + 1)?;
        }
        freetype::FT_COLR_PAINTFORMAT_ROTATE => {
            let value = unsafe { paint.u.rotate };
            let angle = value.angle as f64 / 65536.0;
            let (s, c) = angle.sin_cos();
            let cx = value.center_x as f64 / 65536.0;
            let cy = value.center_y as f64 / 65536.0;
            let affine = Affine {
                xx: c,
                xy: -s,
                dx: cx - c * cx + s * cy,
                yx: s,
                yy: c,
                dy: cy - s * cx - c * cy,
            };
            paint_node(ctx, value.paint, transform.then(affine), out, depth + 1)?;
        }
        freetype::FT_COLR_PAINTFORMAT_SKEW => {
            let value = unsafe { paint.u.skew };
            let xs = (value.x_skew_angle as f64 / 65536.0).tan();
            let ys = (value.y_skew_angle as f64 / 65536.0).tan();
            let cx = value.center_x as f64 / 65536.0;
            let cy = value.center_y as f64 / 65536.0;
            let affine = Affine {
                xx: 1.0,
                xy: xs,
                dx: -xs * cy,
                yx: ys,
                yy: 1.0,
                dy: -ys * cx,
            };
            paint_node(ctx, value.paint, transform.then(affine), out, depth + 1)?;
        }
        freetype::FT_COLR_PAINTFORMAT_COMPOSITE => {
            let value = unsafe { paint.u.composite };
            let mut source = Vec::new();
            let mut backdrop = Vec::new();
            paint_node(ctx, value.source_paint, transform, &mut source, depth + 1)?;
            paint_node(
                ctx,
                value.backdrop_paint,
                transform,
                &mut backdrop,
                depth + 1,
            )?;
            out.push(composite_layers(&source, &backdrop, value.composite_mode)?);
        }
        // A bare solid/gradient has no glyph mask; paint the whole glyph box by
        // using the base glyph's outline as the mask.
        freetype::FT_COLR_PAINTFORMAT_SOLID
        | freetype::FT_COLR_PAINTFORMAT_LINEAR_GRADIENT
        | freetype::FT_COLR_PAINTFORMAT_RADIAL_GRADIENT
        | freetype::FT_COLR_PAINTFORMAT_SWEEP_GRADIENT => {
            let mask = load_and_render_mask(ctx, opaque_glyph_id(ctx, opaque), transform)
                .unwrap_or_else(|_| empty_mask());
            let color = paint_color(ctx, opaque, transform, depth + 1)?;
            if mask.width > 0 && mask.height > 0 {
                out.push(layer_from_alpha_mask(&mask, color));
            }
        }
        _ => {}
    }
    Ok(())
}

fn opaque_glyph_id(_ctx: &ColorContext, _opaque: FT_OpaquePaint) -> u32 {
    0
}

fn empty_mask() -> GlyphBitmap {
    GlyphBitmap {
        width: 0,
        height: 0,
        left: 0,
        top: 0,
        advance_x: 0,
        advance_y: 0,
        advance_x_26_6: 0,
        advance_y_26_6: 0,
        format: PixelFormat::Gray8,
        pitch: 0,
        data: Vec::new(),
    }
}

fn composite_layers(source: &[Layer], backdrop: &[Layer], mode: i32) -> Result<Layer> {
    let mut all = Vec::with_capacity(source.len() + backdrop.len());
    all.extend_from_slice(source);
    all.extend_from_slice(backdrop);
    let Some((left, top, right, bottom)) = Layer::union_bounds(&all) else {
        return Ok(Layer {
            left: 0,
            top: 0,
            width: 0,
            height: 0,
            data: Vec::new(),
        });
    };
    let width = (right - left) as u32;
    let height = (top - bottom) as u32;
    let mut src = vec![0u8; width as usize * height as usize * 4];
    let mut dst = vec![0u8; width as usize * height as usize * 4];
    for layer in source {
        layer.blit_into(&mut src, width, height, left, top);
    }
    for layer in backdrop {
        layer.blit_into(&mut dst, width, height, left, top);
    }
    let mut out = vec![0u8; src.len()];
    for i in (0..src.len()).step_by(4) {
        let s = Rgba([src[i], src[i + 1], src[i + 2], src[i + 3]]);
        let d = Rgba([dst[i], dst[i + 1], dst[i + 2], dst[i + 3]]);
        let result = match mode {
            freetype::FT_COLR_COMPOSITE_CLEAR => Rgba::TRANSPARENT,
            freetype::FT_COLR_COMPOSITE_SRC => s,
            freetype::FT_COLR_COMPOSITE_DEST => d,
            freetype::FT_COLR_COMPOSITE_PLUS => {
                let add = |a: u8, b: u8| a.saturating_add(b);
                Rgba::new(
                    add(s.0[0], d.0[0]),
                    add(s.0[1], d.0[1]),
                    add(s.0[2], d.0[2]),
                    add(s.0[3], d.0[3]),
                )
            }
            freetype::FT_COLR_COMPOSITE_MULTIPLY => Rgba::new(
                (s.0[0] as u32 * d.0[0] as u32 / 255) as u8,
                (s.0[1] as u32 * d.0[1] as u32 / 255) as u8,
                (s.0[2] as u32 * d.0[2] as u32 / 255) as u8,
                (s.0[3] as u32 + d.0[3] as u32).min(255) as u8,
            ),
            _ => s.over(d),
        };
        out[i..i + 4].copy_from_slice(&result.0);
    }
    Ok(Layer {
        left,
        top,
        width,
        height,
        data: out,
    })
}

fn paint_mask(
    ctx: &ColorContext,
    opaque: FT_OpaquePaint,
    transform: Affine,
    mask: &GlyphBitmap,
    depth: u32,
) -> Result<Option<Layer>> {
    if depth > 32 {
        return Ok(None);
    }
    let mut paint = freetype::FT_COLR_Paint {
        format: 0,
        u: freetype::FT_COLR_PaintUnion {
            colr_layers: freetype::FT_PaintColrLayers::default(),
        },
    };
    if unsafe { freetype::FT_Get_Paint(ctx.face, opaque, &mut paint) } == 0 {
        return Ok(None);
    }
    match paint.format {
        freetype::FT_COLR_PAINTFORMAT_SOLID => {
            let value = unsafe { paint.u.solid };
            let color = ctx.color_for(
                value.color.palette_index as usize,
                value.color.alpha as f64 / 16384.0,
            );
            Ok(Some(layer_from_alpha_mask(mask, color)))
        }
        freetype::FT_COLR_PAINTFORMAT_TRANSFORM => {
            let value = unsafe { paint.u.transform };
            paint_mask(
                ctx,
                value.paint,
                transform.then(Affine::from_ft(value.affine)),
                mask,
                depth + 1,
            )
        }
        freetype::FT_COLR_PAINTFORMAT_TRANSLATE
        | freetype::FT_COLR_PAINTFORMAT_SCALE
        | freetype::FT_COLR_PAINTFORMAT_ROTATE
        | freetype::FT_COLR_PAINTFORMAT_SKEW => paint_mask(ctx, opaque, transform, mask, depth + 1),
        freetype::FT_COLR_PAINTFORMAT_LINEAR_GRADIENT
        | freetype::FT_COLR_PAINTFORMAT_RADIAL_GRADIENT
        | freetype::FT_COLR_PAINTFORMAT_SWEEP_GRADIENT => {
            let mut data = vec![0u8; mask.width as usize * mask.height as usize * 4];
            for row in 0..mask.height as i32 {
                let py = mask.top as f64 - row as f64;
                for col in 0..mask.width as i32 {
                    let px = mask.left as f64 + col as f64;
                    let color = gradient_at(ctx, &paint, px, py);
                    let alpha = mask
                        .data
                        .get(row as usize * mask.pitch + col as usize)
                        .copied()
                        .unwrap_or(0);
                    let out = Rgba::new(
                        color.0[0],
                        color.0[1],
                        color.0[2],
                        (color.0[3] as u32 * alpha as u32 / 255) as u8,
                    );
                    let dst = (row as usize * mask.width as usize + col as usize) * 4;
                    data[dst..dst + 4].copy_from_slice(&out.0);
                }
            }
            Ok(Some(Layer {
                left: mask.left,
                top: mask.top,
                width: mask.width,
                height: mask.height,
                data,
            }))
        }
        _ => paint_color(ctx, opaque, transform, depth + 1)
            .map(|color| Some(layer_from_alpha_mask(mask, color))),
    }
}

fn gradient_at(ctx: &ColorContext, paint: &freetype::FT_COLR_Paint, px: f64, py: f64) -> Rgba {
    let fx = px / ctx.units_x.max(1e-9);
    let fy = py / ctx.units_y.max(1e-9);
    match paint.format {
        freetype::FT_COLR_PAINTFORMAT_LINEAR_GRADIENT => {
            let value = unsafe { paint.u.linear_gradient };
            let stops = ctx.gradient_stops(&value.colorline);
            let x0 = value.p0.x as f64 / 64.0;
            let y0 = value.p0.y as f64 / 64.0;
            let x1 = value.p1.x as f64 / 64.0;
            let y1 = value.p1.y as f64 / 64.0;
            let dx = x1 - x0;
            let dy = y1 - y0;
            let len2 = dx * dx + dy * dy;
            let t = if len2 > 1e-12 {
                ((fx - x0) * dx + (fy - y0) * dy) / len2
            } else {
                0.0
            };
            ColorContext::stops_color(&stops, t, value.colorline.extend)
        }
        freetype::FT_COLR_PAINTFORMAT_RADIAL_GRADIENT => {
            let value = unsafe { paint.u.radial_gradient };
            let stops = ctx.gradient_stops(&value.colorline);
            let cx = value.c0.x as f64 / 64.0;
            let cy = value.c0.y as f64 / 64.0;
            let r0 = value.r0 as f64 / 64.0;
            let r1 = value.r1 as f64 / 64.0;
            let dist = ((fx - cx).powi(2) + (fy - cy).powi(2)).sqrt();
            let t = if (r1 - r0).abs() > 1e-12 {
                (dist - r0) / (r1 - r0)
            } else {
                0.0
            };
            ColorContext::stops_color(&stops, t, value.colorline.extend)
        }
        freetype::FT_COLR_PAINTFORMAT_SWEEP_GRADIENT => {
            let value = unsafe { paint.u.sweep_gradient };
            let stops = ctx.gradient_stops(&value.colorline);
            let cx = value.center.x as f64 / 64.0;
            let cy = value.center.y as f64 / 64.0;
            let start = value.start_angle as f64 / 65536.0;
            let end = value.end_angle as f64 / 65536.0;
            let angle = (fy - cy).atan2(fx - cx);
            let span = end - start;
            let t = if span.abs() > 1e-12 {
                (angle - start) / span
            } else {
                0.0
            };
            ColorContext::stops_color(&stops, t, value.colorline.extend)
        }
        _ => Rgba::TRANSPARENT,
    }
}

fn paint_color(
    ctx: &ColorContext,
    opaque: FT_OpaquePaint,
    transform: Affine,
    depth: u32,
) -> Result<Rgba> {
    if depth > 32 {
        return Ok(Rgba::TRANSPARENT);
    }
    let mut paint = freetype::FT_COLR_Paint {
        format: 0,
        u: freetype::FT_COLR_PaintUnion {
            colr_layers: freetype::FT_PaintColrLayers::default(),
        },
    };
    if unsafe { freetype::FT_Get_Paint(ctx.face, opaque, &mut paint) } == 0 {
        return Ok(Rgba::TRANSPARENT);
    }
    match paint.format {
        freetype::FT_COLR_PAINTFORMAT_SOLID => {
            let value = unsafe { paint.u.solid };
            Ok(ctx.color_for(
                value.color.palette_index as usize,
                value.color.alpha as f64 / 16384.0,
            ))
        }
        freetype::FT_COLR_PAINTFORMAT_TRANSFORM => {
            let value = unsafe { paint.u.transform };
            paint_color(
                ctx,
                value.paint,
                transform.then(Affine::from_ft(value.affine)),
                depth + 1,
            )
        }
        freetype::FT_COLR_PAINTFORMAT_TRANSLATE
        | freetype::FT_COLR_PAINTFORMAT_SCALE
        | freetype::FT_COLR_PAINTFORMAT_ROTATE
        | freetype::FT_COLR_PAINTFORMAT_SKEW => paint_color(ctx, opaque, transform, depth + 1),
        _ => Ok(Rgba::TRANSPARENT),
    }
}

#[allow(dead_code)]
fn _keep_colors(_cache: HashMap<u32, Rgba>) {}

// {{{ CBDT/CBLC bitmap strikes

fn be_u16(data: &[u8], offset: usize) -> Option<u16> {
    data.get(offset..offset + 2)
        .map(|b| u16::from_be_bytes([b[0], b[1]]))
}

fn be_u32(data: &[u8], offset: usize) -> Option<u32> {
    data.get(offset..offset + 4)
        .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

#[derive(Clone, Copy, Debug)]
struct Strike {
    start_glyph: u16,
    end_glyph: u16,
    ppem_y: u8,
    array_offset: u32,
    number_subtables: u32,
}

#[derive(Clone, Copy, Debug)]
struct Subtable {
    first_glyph: u16,
    last_glyph: u16,
    subtable_offset: u32,
}

fn parse_strikes(cblc: &[u8]) -> Vec<Strike> {
    let Some(count) = be_u32(cblc, 4) else {
        return Vec::new();
    };
    let mut ans = Vec::new();
    for i in 0..count as usize {
        let base = 8 + i * 48;
        let Some(array_offset) = be_u32(cblc, base) else {
            break;
        };
        let Some(number_subtables) = be_u32(cblc, base + 8) else {
            break;
        };
        let Some(start_glyph) = be_u16(cblc, base + 40) else {
            break;
        };
        let Some(end_glyph) = be_u16(cblc, base + 42) else {
            break;
        };
        let Some(ppem_y) = cblc.get(base + 45).copied() else {
            break;
        };
        ans.push(Strike {
            start_glyph,
            end_glyph,
            ppem_y,
            array_offset,
            number_subtables,
        });
    }
    ans
}

fn parse_subtables(cblc: &[u8], strike: &Strike) -> Vec<Subtable> {
    let mut ans = Vec::new();
    for i in 0..strike.number_subtables as usize {
        let base = strike.array_offset as usize + i * 8;
        let Some(first) = be_u16(cblc, base) else {
            break;
        };
        let Some(last) = be_u16(cblc, base + 2) else {
            break;
        };
        let Some(offset) = be_u32(cblc, base + 4) else {
            break;
        };
        ans.push(Subtable {
            first_glyph: first,
            last_glyph: last,
            subtable_offset: strike.array_offset + offset,
        });
    }
    ans
}

type GlyphImageSlice<'a> = (&'a [u8], u16, Option<(i8, i8, u8)>);

fn glyph_image_slice<'a>(
    cblc: &[u8],
    cbdt: &'a [u8],
    subtable: &Subtable,
    glyph_id: u16,
) -> Option<GlyphImageSlice<'a>> {
    let base = subtable.subtable_offset as usize;
    let index_format = be_u16(cblc, base)?;
    let image_format = be_u16(cblc, base + 2)?;
    let image_data_offset = be_u32(cblc, base + 4)?;
    let index = glyph_id.checked_sub(subtable.first_glyph)? as usize;
    let count = (subtable.last_glyph - subtable.first_glyph + 1) as usize;
    let (offset, next_offset, metrics) = match index_format {
        1 => {
            let start = be_u32(cblc, base + 8 + index * 4)?;
            let end = be_u32(cblc, base + 8 + (index + 1) * 4)?;
            (start, end, None)
        }
        2 => {
            let image_size = be_u32(cblc, base + 8)?;
            let m = cblc.get(base + 12..base + 20)?;
            let metrics = (m[2] as i8, m[3] as i8, m[4]);
            (
                image_size * index as u32,
                image_size * (index + 1) as u32,
                Some(metrics),
            )
        }
        3 => {
            let start = be_u16(cblc, base + 8 + index * 2)? as u32;
            let end = be_u16(cblc, base + 8 + (index + 1) * 2)? as u32;
            (start, end, None)
        }
        4 => {
            let num = be_u32(cblc, base + 8)? as usize;
            let mut start = None;
            let mut end = None;
            for i in 0..num {
                let rec = base + 12 + i * 4;
                let gid = be_u16(cblc, rec)?;
                if gid == glyph_id {
                    let off = be_u16(cblc, rec + 2)? as u32;
                    start = Some(off);
                    let next_rec = base + 12 + (i + 1) * 4;
                    end = Some(be_u16(cblc, next_rec + 2).unwrap_or(0) as u32);
                    break;
                }
            }
            (start?, end?, None)
        }
        5 => {
            let image_size = be_u32(cblc, base + 8)?;
            let m = cblc.get(base + 12..base + 20)?;
            let metrics = (m[2] as i8, m[3] as i8, m[4]);
            let num = be_u32(cblc, base + 20)? as usize;
            let mut found = None;
            for i in 0..num {
                if be_u16(cblc, base + 24 + i * 2)? == glyph_id {
                    found = Some(i as u32);
                    break;
                }
            }
            let index = found?;
            (image_size * index, image_size * (index + 1), Some(metrics))
        }
        _ => return None,
    };
    let _ = count;
    let start = (image_data_offset as usize)
        .checked_add(offset as usize)?
        .checked_add(0)?;
    let end = (image_data_offset as usize).checked_add(next_offset as usize)?;
    let data = cbdt.get(start..end)?;
    Some((data, image_format, metrics))
}

fn decode_cbdt_glyph(face: &Face, glyph_id: u32) -> Option<GlyphBitmap> {
    let glyph_id = u16::try_from(glyph_id).ok()?;
    let cblc = face.sfnt_table(b"CBLC")?;
    let cbdt = face.sfnt_table(b"CBDT")?;
    let strikes = parse_strikes(&cblc);
    let target = face.cell_metrics().cell_height.max(8) as i32;
    let strike = strikes
        .iter()
        .filter(|s| glyph_id >= s.start_glyph && glyph_id <= s.end_glyph)
        .min_by_key(|s| (s.ppem_y as i32 - target).abs())?;
    let subtables = parse_subtables(&cblc, strike);
    let subtable = subtables
        .iter()
        .find(|s| glyph_id >= s.first_glyph && glyph_id <= s.last_glyph)?;
    let (data, image_format, metrics) = glyph_image_slice(&cblc, &cbdt, subtable, glyph_id)?;
    let png_start = find_png(data)?;
    let png = &data[png_start..];
    let decoded = kitty_graphics::decode_png(png, 4096).ok()?;
    let (left, top, advance) = match image_format {
        17 => {
            let m = data.get(png_start.wrapping_sub(5)..png_start)?;
            (m[2] as i8 as i32, m[3] as i8 as i32, m[4] as u32)
        }
        18 => {
            let m = data.get(png_start.wrapping_sub(8)..png_start)?;
            (m[2] as i8 as i32, m[3] as i8 as i32, m[4] as u32)
        }
        19 => metrics
            .map(|(x, y, adv)| (x as i32, y as i32, adv as u32))
            .unwrap_or((0, decoded.height as i32, decoded.width)),
        _ => metrics
            .map(|(x, y, adv)| (x as i32, y as i32, adv as u32))
            .unwrap_or((0, decoded.height as i32, decoded.width)),
    };
    Some(GlyphBitmap {
        width: decoded.width,
        height: decoded.height,
        left,
        top,
        advance_x: advance as i32,
        advance_y: 0,
        advance_x_26_6: advance as i32 * 64,
        advance_y_26_6: 0,
        format: PixelFormat::Rgba8,
        pitch: decoded.width as usize * 4,
        data: decoded.rgba,
    })
}

fn find_png(data: &[u8]) -> Option<usize> {
    const SIG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    data.windows(SIG.len()).position(|w| w == SIG)
}

// }}}
