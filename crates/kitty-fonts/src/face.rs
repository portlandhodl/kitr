// SPDX-License-Identifier: GPL-3.0-or-later
//! Safe wrapper around `FT_Face` plus its `hb_font_t`.
//!
//! The metrics code is a direct port of `cell_metrics()`, `calc_cell_width()`,
//! `calc_cell_height()` and `set_font_size()` from `kitty/freetype.c`.

use std::ffi::CString;
use std::path::{Path, PathBuf};

use crate::color::{self, ColorGlyphOptions, ColorGlyphType};
use crate::error::{ft_check, Error, Result};
use crate::ffi::freetype::{self, FT_Face};
use crate::ffi::harfbuzz;
use crate::library::FreeTypeLibrary;
use crate::metrics::{FaceMetrics, FontCellMetrics, GlyphMetrics};
use crate::raster::{GlyphBitmap, PixelFormat};
use crate::shaping::{shape, ShapeOptions, ShapedRun};

pub const DEFAULT_SIZE_PTS: f64 = 12.0;
pub const DEFAULT_DPI: f64 = 96.0;

/// Description of a face on disk, mirroring kitty's `Face` descriptor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FaceDescriptor {
    pub path: PathBuf,
    pub index: i32,
    pub hinting: bool,
    pub hint_style: i32,
}

impl FaceDescriptor {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            index: 0,
            hinting: true,
            hint_style: 3,
        }
    }

    pub fn load(&self, size_pts: f64, dpi_x: f64, dpi_y: f64) -> Result<Face> {
        Face::from_descriptor(self, size_pts, dpi_x, dpi_y)
    }
}

/// An opened font face.
///
/// A `Face` owns one `FT_Face` and one `hb_font_t`. It is deliberately neither
/// `Send` nor `Sync` because FreeType glyph loading mutates the face's glyph
/// slot; use one face per thread.
pub struct Face {
    face: FT_Face,
    hb_font: *mut harfbuzz::hb_font_t,
    descriptor: FaceDescriptor,
    size_pts: f64,
    dpi_x: f64,
    dpi_y: f64,
    metrics: FaceMetrics,
    is_scalable: bool,
    has_color: bool,
    is_variable: bool,
    space_glyph: u32,
    load_flags: i32,
}

impl Face {
    /// Load a face at kitty's default 12pt / 96dpi size.
    pub fn from_path(path: impl AsRef<Path>, index: i32) -> Result<Self> {
        Self::from_path_with_size(path, index, DEFAULT_SIZE_PTS, DEFAULT_DPI, DEFAULT_DPI)
    }

    /// Load a face at a specific size.
    pub fn from_path_with_size(
        path: impl AsRef<Path>,
        index: i32,
        size_pts: f64,
        dpi_x: f64,
        dpi_y: f64,
    ) -> Result<Self> {
        let descriptor = FaceDescriptor::new(path.as_ref()).with_index(index);
        Self::from_descriptor(&descriptor, size_pts, dpi_x, dpi_y)
    }

    pub fn from_descriptor(
        descriptor: &FaceDescriptor,
        size_pts: f64,
        dpi_x: f64,
        dpi_y: f64,
    ) -> Result<Self> {
        if size_pts <= 0.0 || dpi_x <= 0.0 || dpi_y <= 0.0 {
            return Err(Error::InvalidSize);
        }
        let library = FreeTypeLibrary::global()?;
        let path = descriptor
            .path
            .to_str()
            .ok_or(Error::InvalidString)
            .and_then(|s| CString::new(s).map_err(|_| Error::InvalidString))?;
        let mut face = std::ptr::null_mut();
        let code = unsafe {
            freetype::FT_New_Face(
                library.as_ptr(),
                path.as_ptr(),
                descriptor.index as freetype::FT_Long,
                &mut face,
            )
        };
        ft_check("FT_New_Face", code)?;
        // From here on an error must not leak the FT_Face.
        let mut ans = Self {
            face,
            hb_font: std::ptr::null_mut(),
            descriptor: descriptor.clone(),
            size_pts,
            dpi_x,
            dpi_y,
            metrics: FaceMetrics::default(),
            is_scalable: false,
            has_color: false,
            is_variable: false,
            space_glyph: 0,
            load_flags: 0,
        };
        match ans.init() {
            Ok(()) => Ok(ans),
            Err(err) => {
                ans.shutdown();
                Err(err)
            }
        }
    }

    fn init(&mut self) -> Result<()> {
        let raw = unsafe { &*self.face };
        self.is_scalable = raw.face_flags & freetype::FT_FACE_FLAG_SCALABLE != 0;
        self.has_color = raw.face_flags & freetype::FT_FACE_FLAG_COLOR != 0;
        self.is_variable = raw.face_flags & freetype::FT_FACE_FLAG_MULTIPLE_MASTERS != 0;
        self.load_flags = load_flags_for(self.descriptor.hinting, self.descriptor.hint_style);
        self.copy_face_metrics();
        self.set_size(self.size_pts, self.dpi_x, self.dpi_y)?;

        let hb_font = unsafe { harfbuzz::hb_ft_font_create(self.face, std::ptr::null_mut()) };
        if hb_font.is_null() {
            return Err(Error::FreeType {
                operation: "hb_ft_font_create",
                code: 0,
            });
        }
        self.hb_font = hb_font;
        unsafe {
            harfbuzz::hb_ft_font_set_load_flags(self.hb_font, self.load_flags);
        }

        let os2 = unsafe {
            freetype::FT_Get_Sfnt_Table(self.face, freetype::FT_SFNT_OS2) as *const freetype::TT_OS2
        };
        if !os2.is_null() {
            let os2 = unsafe { &*os2 };
            self.metrics.strikethrough_position = os2.yStrikeoutPosition as i32;
            self.metrics.strikethrough_thickness = os2.yStrikeoutSize as i32;
        }
        self.space_glyph = self.glyph_id(' ' as u32);
        Ok(())
    }

    fn shutdown(&mut self) {
        if !self.hb_font.is_null() {
            unsafe {
                harfbuzz::hb_font_destroy(self.hb_font);
            }
            self.hb_font = std::ptr::null_mut();
        }
        if !self.face.is_null() {
            unsafe {
                freetype::FT_Done_Face(self.face);
            }
            self.face = std::ptr::null_mut();
        }
    }

    fn copy_face_metrics(&mut self) {
        let raw = unsafe { &*self.face };
        self.metrics.units_per_em = raw.units_per_EM as u32;
        self.metrics.ascender = raw.ascender as i32;
        self.metrics.descender = raw.descender as i32;
        self.metrics.height = raw.height as i32;
        self.metrics.max_advance_width = raw.max_advance_width as i32;
        self.metrics.max_advance_height = raw.max_advance_height as i32;
        self.metrics.underline_position = raw.underline_position as i32;
        self.metrics.underline_thickness = raw.underline_thickness as i32;
    }

    /// Change the point size and resolution. The HarfBuzz font is notified so
    /// shaping advances remain in sync with FreeType.
    pub fn set_size(&mut self, size_pts: f64, dpi_x: f64, dpi_y: f64) -> Result<()> {
        if size_pts <= 0.0 || dpi_x <= 0.0 || dpi_y <= 0.0 {
            return Err(Error::InvalidSize);
        }
        let char_height = (size_pts * 64.0).ceil() as freetype::FT_F26Dot6;
        let code = unsafe {
            freetype::FT_Set_Char_Size(
                self.face,
                0,
                char_height,
                dpi_x as freetype::FT_UInt,
                dpi_y as freetype::FT_UInt,
            )
        };
        if code != 0 {
            let raw = unsafe { &*self.face };
            if !self.is_scalable && raw.num_fixed_sizes > 0 {
                // Bitmap-only faces are not a supported terminal font, but
                // selecting the first available strike keeps them usable.
                if self.try_select_size(0).is_ok() {
                    self.size_pts = size_pts;
                    self.dpi_x = dpi_x;
                    self.dpi_y = dpi_y;
                    if !self.hb_font.is_null() {
                        unsafe { harfbuzz::hb_ft_font_changed(self.hb_font) };
                    }
                    self.copy_face_metrics();
                    return Ok(());
                }
            }
            return Err(Error::FreeType {
                operation: "FT_Set_Char_Size",
                code,
            });
        }
        self.size_pts = size_pts;
        self.dpi_x = dpi_x;
        self.dpi_y = dpi_y;
        if !self.hb_font.is_null() {
            unsafe { harfbuzz::hb_ft_font_changed(self.hb_font) };
        }
        self.copy_face_metrics();
        Ok(())
    }

    fn try_select_size(&self, index: i32) -> Result<()> {
        let code = unsafe { freetype::FT_Select_Size(self.face, index) };
        ft_check("FT_Select_Size", code)
    }

    pub fn descriptor(&self) -> &FaceDescriptor {
        &self.descriptor
    }

    pub fn path(&self) -> &Path {
        &self.descriptor.path
    }

    pub fn index(&self) -> i32 {
        self.descriptor.index
    }

    pub fn point_size(&self) -> f64 {
        self.size_pts
    }

    pub fn dpi(&self) -> (f64, f64) {
        (self.dpi_x, self.dpi_y)
    }

    pub fn raw_metrics(&self) -> FaceMetrics {
        self.metrics
    }

    pub fn is_scalable(&self) -> bool {
        self.is_scalable
    }

    pub fn has_color(&self) -> bool {
        self.has_color
    }

    /// Colour technology used by `glyph_id`, if any.
    pub fn color_glyph_type(&self, glyph_id: u32) -> Option<ColorGlyphType> {
        color::glyph_color_type(self, glyph_id)
    }

    /// Rasterize a COLR/CBDT/SVG glyph into RGBA. Returns `None` when the
    /// glyph is not a colour glyph. This is the kitty-compatible entry point
    /// used by the renderer.
    pub fn rasterize_color_glyph(
        &self,
        glyph_id: u32,
        options: &ColorGlyphOptions,
    ) -> Result<Option<GlyphBitmap>> {
        color::rasterize_color_glyph(self, glyph_id, options)
    }

    pub(crate) fn ffi_face(&self) -> FT_Face {
        self.face
    }

    /// Read a raw SFNT table through FreeType. `tag` is a four byte tag such
    /// as `b"CBLC"`.
    pub fn sfnt_table(&self, tag: &[u8; 4]) -> Option<Vec<u8>> {
        let tag = u32::from_be_bytes(*tag) as freetype::FT_ULong;
        let mut length: freetype::FT_ULong = 0;
        let code = unsafe {
            freetype::FT_Load_Sfnt_Table(self.face, tag, 0, std::ptr::null_mut(), &mut length)
        };
        if code != 0 || length == 0 {
            return None;
        }
        let mut data = vec![0u8; length as usize];
        let code = unsafe {
            freetype::FT_Load_Sfnt_Table(self.face, tag, 0, data.as_mut_ptr(), &mut length)
        };
        if code != 0 {
            return None;
        }
        data.truncate(length as usize);
        Some(data)
    }

    pub(crate) fn load_flags_value(&self) -> i32 {
        self.load_flags
    }

    /// Pixels per font unit on each axis, derived from the active FT_Size.
    pub(crate) fn pixels_per_font_unit(&self) -> (f64, f64) {
        let size = unsafe { (*self.face).size };
        if size.is_null() {
            return (1.0, 1.0);
        }
        let metrics = unsafe { &(*size).metrics };
        (
            metrics.x_scale as f64 / 65536.0 / 64.0,
            metrics.y_scale as f64 / 65536.0 / 64.0,
        )
    }

    pub fn is_variable(&self) -> bool {
        self.is_variable
    }

    pub fn num_glyphs(&self) -> u32 {
        unsafe { &*self.face }.num_glyphs as u32
    }

    pub fn family_name(&self) -> String {
        unsafe { c_string((*self.face).family_name) }
    }

    pub fn style_name(&self) -> String {
        unsafe { c_string((*self.face).style_name) }
    }

    pub fn postscript_name(&self) -> String {
        let p = unsafe { freetype::FT_Get_Postscript_Name(self.face) };
        unsafe { c_string(p.cast_mut()) }
    }

    pub fn is_bold(&self) -> bool {
        unsafe { &*self.face }.style_flags & freetype::FT_STYLE_FLAG_BOLD != 0
    }

    pub fn is_italic(&self) -> bool {
        unsafe { &*self.face }.style_flags & freetype::FT_STYLE_FLAG_ITALIC != 0
    }

    /// The FreeType glyph index for a Unicode codepoint, or 0 if absent.
    pub fn glyph_id(&self, codepoint: u32) -> u32 {
        unsafe { freetype::FT_Get_Char_Index(self.face, codepoint as freetype::FT_ULong) }
    }

    pub fn has_codepoint(&self, codepoint: u32) -> bool {
        self.glyph_id(codepoint) != 0
    }

    pub fn space_glyph_id(&self) -> u32 {
        self.space_glyph
    }

    fn load_glyph(&self, glyph_id: u32, base_flags: i32) -> Result<()> {
        let flags = self.load_flags | base_flags;
        let code = unsafe { freetype::FT_Load_Glyph(self.face, glyph_id, flags) };
        ft_check("FT_Load_Glyph", code)
    }

    /// Metrics for a glyph at the current size.
    pub fn glyph_metrics(&self, glyph_id: u32) -> Result<GlyphMetrics> {
        self.load_glyph(glyph_id, freetype::FT_LOAD_DEFAULT)?;
        let slot = unsafe { (*self.face).glyph };
        if slot.is_null() {
            return Err(Error::MissingGlyph(glyph_id));
        }
        let slot = unsafe { &*slot };
        Ok(GlyphMetrics {
            glyph_id,
            advance_x_26_6: slot.metrics.horiAdvance as i32,
            advance_y_26_6: slot.metrics.vertAdvance as i32,
            bearing_x_26_6: slot.metrics.horiBearingX as i32,
            bearing_y_26_6: slot.metrics.horiBearingY as i32,
            width_26_6: slot.metrics.width as i32,
            height_26_6: slot.metrics.height as i32,
            bitmap_width: slot.bitmap.width,
            bitmap_rows: slot.bitmap.rows,
            bitmap_left: slot.bitmap_left,
            bitmap_top: slot.bitmap_top,
        })
    }

    /// Rasterize a glyph with kitty's default load flags
    /// (`FT_LOAD_DEFAULT | FT_LOAD_RENDER`). Color faces automatically also
    /// get `FT_LOAD_COLOR`.
    pub fn rasterize_glyph(&self, glyph_id: u32) -> Result<GlyphBitmap> {
        if self.color_glyph_type(glyph_id).is_some() {
            if let Some(bitmap) =
                self.rasterize_color_glyph(glyph_id, &ColorGlyphOptions::default())?
            {
                return Ok(bitmap);
            }
        }
        self.rasterize_glyph_with_flags(
            glyph_id,
            freetype::FT_LOAD_DEFAULT | freetype::FT_LOAD_RENDER,
        )
    }

    /// Rasterize a glyph, ORing `flags` into the face's hinting/load flags.
    ///
    /// Callers that want pixels must include `FT_LOAD_RENDER` (see
    /// [`crate::ffi::freetype::FT_LOAD_RENDER`]); [`Face::rasterize_glyph`]
    /// does so by default.
    pub fn rasterize_glyph_with_flags(
        &self,
        glyph_id: u32,
        flags: freetype::FT_Int,
    ) -> Result<GlyphBitmap> {
        if glyph_id as i64 >= self.num_glyphs() as i64 {
            return Err(Error::MissingGlyph(glyph_id));
        }
        let mut load_flags = self.load_flags | flags;
        if self.has_color {
            load_flags |= freetype::FT_LOAD_COLOR;
        }
        let code = unsafe { freetype::FT_Load_Glyph(self.face, glyph_id, load_flags) };
        ft_check("FT_Load_Glyph", code)?;
        let slot = unsafe { (*self.face).glyph };
        if slot.is_null() {
            return Err(Error::Raster("FreeType returned a null glyph slot"));
        }
        if unsafe { (*slot).format } == freetype::FT_GLYPH_FORMAT_OUTLINE
            && load_flags & freetype::FT_LOAD_RENDER != 0
        {
            let code = unsafe { freetype::FT_Render_Glyph(slot, freetype::FT_RENDER_MODE_NORMAL) };
            ft_check("FT_Render_Glyph", code)?;
        }
        let slot = unsafe { &*slot };
        if slot.format != freetype::FT_GLYPH_FORMAT_BITMAP {
            return Err(Error::UnsupportedGlyphFormat(slot.format));
        }
        let bitmap = &slot.bitmap;
        let pixel_mode = bitmap.pixel_mode;
        let format = match pixel_mode {
            freetype::FT_PIXEL_MODE_NONE | freetype::FT_PIXEL_MODE_GRAY => PixelFormat::Gray8,
            freetype::FT_PIXEL_MODE_MONO
            | freetype::FT_PIXEL_MODE_GRAY2
            | freetype::FT_PIXEL_MODE_GRAY4 => PixelFormat::Gray8,
            freetype::FT_PIXEL_MODE_BGRA => PixelFormat::Bgra8,
            other => return Err(Error::UnsupportedPixelMode(other)),
        };
        let raw_advance_x = if slot.advance.x != 0 {
            slot.advance.x as i32
        } else {
            slot.metrics.horiAdvance as i32
        };
        let raw_advance_y = if slot.advance.y != 0 {
            slot.advance.y as i32
        } else {
            slot.metrics.vertAdvance as i32
        };
        let width = bitmap.width;
        let height = bitmap.rows;
        let bpp = format.bytes_per_pixel();
        let pitch = width as usize * bpp;
        let mut data = Vec::new();
        if width > 0 && height > 0 {
            if bitmap.buffer.is_null() {
                return Err(Error::Raster("FreeType returned a null bitmap buffer"));
            }
            if bitmap.pitch == 0 {
                return Err(Error::Raster("FreeType returned a zero bitmap pitch"));
            }
            data.resize(pitch * height as usize, 0);
            let src_pitch = bitmap.pitch as isize;
            for y in 0..height as usize {
                let src = unsafe { bitmap.buffer.offset(y as isize * src_pitch) };
                let dst = &mut data[y * pitch..(y + 1) * pitch];
                unsafe {
                    expand_bitmap_row(src, dst, width as usize, pixel_mode);
                }
            }
        }
        Ok(GlyphBitmap {
            width,
            height,
            left: slot.bitmap_left,
            top: slot.bitmap_top,
            advance_x: convert_26_6(raw_advance_x),
            advance_y: convert_26_6(raw_advance_y),
            advance_x_26_6: raw_advance_x,
            advance_y_26_6: raw_advance_y,
            format,
            pitch,
            data,
        })
    }

    /// Rasterize a codepoint on this face. Missing codepoints return
    /// [`Error::MissingGlyph`].
    pub fn rasterize_codepoint(&self, codepoint: u32) -> Result<GlyphBitmap> {
        if codepoint <= 0x10FFFF {
            let glyph_id = self.glyph_id(codepoint);
            if glyph_id != 0 && self.color_glyph_type(glyph_id).is_some() {
                if let Some(bitmap) =
                    self.rasterize_color_glyph(glyph_id, &ColorGlyphOptions::default())?
                {
                    return Ok(bitmap);
                }
            }
        }
        self.rasterize_codepoint_with_flags(
            codepoint,
            freetype::FT_LOAD_DEFAULT | freetype::FT_LOAD_RENDER,
        )
    }

    pub fn rasterize_codepoint_with_flags(
        &self,
        codepoint: u32,
        flags: freetype::FT_Int,
    ) -> Result<GlyphBitmap> {
        if codepoint > 0x10FFFF {
            return Err(Error::MissingGlyph(codepoint));
        }
        let glyph_id = self.glyph_id(codepoint);
        if glyph_id == 0 {
            return Err(Error::MissingGlyph(codepoint));
        }
        self.rasterize_glyph_with_flags(glyph_id, flags)
    }

    /// Advance of U+0020 at the current size, in 26.6 pixels.
    pub fn space_advance_26_6(&self) -> i32 {
        match self.glyph_metrics(self.space_glyph) {
            Ok(m) => m.advance_x_26_6,
            Err(_) => 0,
        }
    }

    /// Advance of U+0020 rounded up to whole pixels.
    pub fn space_advance_px(&self) -> u32 {
        ceil_div_64(self.space_advance_26_6())
    }

    fn font_units_to_pixels_y(&self, value: i32) -> u32 {
        self.font_units_to_pixels(value, true)
    }

    fn font_units_to_pixels(&self, value: i32, y_axis: bool) -> u32 {
        let size = unsafe { (*self.face).size };
        if size.is_null() {
            return 0;
        }
        let metrics = unsafe { &(*size).metrics };
        let scale = if y_axis {
            metrics.y_scale
        } else {
            metrics.x_scale
        };
        let scaled = unsafe { freetype::FT_MulFix(value as freetype::FT_Long, scale) };
        ((scaled as f64) / 64.0).ceil() as u32
    }

    fn calc_cell_width(&self) -> u32 {
        let mut ans = 0;
        for ch in 32u32..128 {
            let glyph = self.glyph_id(ch);
            if self.load_glyph(glyph, freetype::FT_LOAD_DEFAULT).is_ok() {
                let slot = unsafe { (*self.face).glyph };
                if !slot.is_null() {
                    let advance = unsafe { (*slot).metrics.horiAdvance };
                    ans = ans.max(ceil_div_64(advance as i32));
                }
            }
        }
        if ans == 0 {
            let size = unsafe { (*self.face).size };
            if !size.is_null() {
                let max_advance = unsafe { (*size).metrics.max_advance };
                ans = ans.max(ceil_div_64(max_advance as i32).max(1));
            } else {
                ans = 1;
            }
        }
        ans
    }

    fn height_for_char(&self, ch: u32) -> u32 {
        let glyph = self.glyph_id(ch);
        if self.load_glyph(glyph, freetype::FT_LOAD_DEFAULT).is_err() {
            return 0;
        }
        let slot = unsafe { (*self.face).glyph };
        if slot.is_null() {
            return 0;
        }
        let slot = unsafe { &*slot };
        let baseline = self.font_units_to_pixels_y(self.metrics.ascender);
        let bitmap_top = slot.bitmap_top;
        let rows = slot.bitmap.rows;
        if bitmap_top <= 0 || (bitmap_top > 0 && (bitmap_top as u32) < baseline) {
            baseline.saturating_sub(bitmap_top.max(0) as u32) + rows
        } else {
            0
        }
    }

    fn calc_cell_height(&self) -> u32 {
        let ans = self.font_units_to_pixels_y(self.metrics.height);
        let underscore = self.height_for_char('_' as u32);
        ans.max(underscore)
    }

    /// Compute kitty-compatible cell metrics for the current size.
    pub fn cell_metrics(&self) -> FontCellMetrics {
        let cell_width = self.calc_cell_width();
        let cell_height = self.calc_cell_height();
        let baseline = self.font_units_to_pixels_y(self.metrics.ascender);
        let max_y = cell_height.saturating_sub(1);
        let underline_position = max_y.min(self.font_units_to_pixels_y(
            (self.metrics.ascender - self.metrics.underline_position).max(0),
        ));
        let underline_thickness =
            1.max(self.font_units_to_pixels_y(self.metrics.underline_thickness));
        let strikethrough_position = if self.metrics.strikethrough_position != 0 {
            max_y.min(self.font_units_to_pixels_y(
                (self.metrics.ascender - self.metrics.strikethrough_position).max(0),
            ))
        } else {
            (baseline as f64 * 0.65).floor() as u32
        };
        let strikethrough_thickness = if self.metrics.strikethrough_thickness > 0 {
            1.max(self.font_units_to_pixels_y(self.metrics.strikethrough_thickness))
        } else {
            underline_thickness
        };
        FontCellMetrics {
            cell_width,
            cell_height,
            baseline,
            underline_position,
            underline_thickness,
            strikethrough_position,
            strikethrough_thickness,
        }
    }

    /// Shape a UTF-32 run with HarfBuzz.
    pub fn shape(&self, text: &[u32], options: &ShapeOptions) -> Result<ShapedRun> {
        shape(self, text, options)
    }

    pub(crate) fn hb_font_ptr(&self) -> *mut harfbuzz::hb_font_t {
        self.hb_font
    }
}

impl Drop for Face {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl std::fmt::Debug for Face {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Face")
            .field("descriptor", &self.descriptor)
            .field("family", &self.family_name())
            .field("style", &self.style_name())
            .field("postscript_name", &self.postscript_name())
            .field("size_pts", &self.size_pts)
            .field("is_scalable", &self.is_scalable)
            .field("has_color", &self.has_color)
            .field("is_variable", &self.is_variable)
            .finish()
    }
}

impl FaceDescriptor {
    pub fn with_index(mut self, index: i32) -> Self {
        self.index = index;
        self
    }

    pub fn with_hinting(mut self, hinting: bool, hint_style: i32) -> Self {
        self.hinting = hinting;
        self.hint_style = hint_style;
        self
    }
}

fn load_flags_for(hinting: bool, hint_style: i32) -> i32 {
    if !hinting {
        return freetype::FT_LOAD_NO_HINTING;
    }
    if hint_style >= 3 {
        freetype::FT_LOAD_TARGET_NORMAL
    } else if hint_style > 0 {
        freetype::FT_LOAD_TARGET_LIGHT
    } else {
        0
    }
}

fn ceil_div_64(value: i32) -> u32 {
    if value <= 0 {
        0
    } else {
        ((value as i64 + 63) / 64) as u32
    }
}

fn convert_26_6(value: i32) -> i32 {
    if value >= 0 {
        (value + 63) / 64
    } else {
        -((-value + 63) / 64)
    }
}

/// Expand one FreeType bitmap row into a tightly packed output row.
///
/// # Safety
///
/// `src` must point to at least the number of bytes FreeType reports for the
/// row, and `dst` must be exactly `width * bytes_per_pixel` bytes.
unsafe fn expand_bitmap_row(src: *const u8, dst: &mut [u8], width: usize, pixel_mode: u8) {
    match pixel_mode {
        freetype::FT_PIXEL_MODE_GRAY => {
            unsafe { std::ptr::copy_nonoverlapping(src, dst.as_mut_ptr(), width) };
        }
        freetype::FT_PIXEL_MODE_MONO => {
            for (x, out) in dst.iter_mut().enumerate().take(width) {
                let byte = unsafe { *src.add(x / 8) };
                let bit = (byte >> (7 - (x % 8))) & 1;
                *out = if bit == 0 { 0 } else { 255 };
            }
        }
        freetype::FT_PIXEL_MODE_GRAY2 => {
            for (x, out) in dst.iter_mut().enumerate().take(width) {
                let byte = unsafe { *src.add(x / 4) };
                let shift = 6 - 2 * (x % 4);
                *out = ((byte >> shift) & 0x3) * 85;
            }
        }
        freetype::FT_PIXEL_MODE_GRAY4 => {
            for (x, out) in dst.iter_mut().enumerate().take(width) {
                let byte = unsafe { *src.add(x / 2) };
                let value = if x % 2 == 0 { byte >> 4 } else { byte & 0xf };
                *out = value * 17;
            }
        }
        freetype::FT_PIXEL_MODE_BGRA => {
            unsafe { std::ptr::copy_nonoverlapping(src, dst.as_mut_ptr(), width * 4) };
        }
        _ => {}
    }
}

unsafe fn c_string(ptr: *mut std::ffi::c_char) -> String {
    if ptr.is_null() {
        return String::new();
    }
    unsafe { std::ffi::CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::expand_bitmap_row;
    use crate::ffi::freetype;

    #[test]
    fn expands_supported_bitmap_rows() {
        let mut mono = [0u8; 4];
        unsafe {
            expand_bitmap_row(
                [0b1010_0000].as_ptr(),
                &mut mono,
                4,
                freetype::FT_PIXEL_MODE_MONO,
            );
        }
        assert_eq!(mono, [255, 0, 255, 0]);

        let mut gray2 = [0u8; 4];
        unsafe {
            expand_bitmap_row(
                [0b0001_1011].as_ptr(),
                &mut gray2,
                4,
                freetype::FT_PIXEL_MODE_GRAY2,
            );
        }
        assert_eq!(gray2, [0, 85, 170, 255]);

        let mut gray4 = [0u8; 2];
        unsafe {
            expand_bitmap_row(
                [0x0f].as_ptr(),
                &mut gray4,
                2,
                freetype::FT_PIXEL_MODE_GRAY4,
            );
        }
        assert_eq!(gray4, [0, 255]);

        let mut bgra = [0u8; 8];
        unsafe {
            expand_bitmap_row(
                [1, 2, 3, 4, 5, 6, 7, 8].as_ptr(),
                &mut bgra,
                2,
                freetype::FT_PIXEL_MODE_BGRA,
            );
        }
        assert_eq!(bgra, [1, 2, 3, 4, 5, 6, 7, 8]);
    }
}
