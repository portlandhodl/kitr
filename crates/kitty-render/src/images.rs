// SPDX-License-Identifier: GPL-3.0-or-later
//! CPU-side image placement, background-image layout and window-logo geometry.
//!
//! This module is the bridge between `kitty-screen`'s stored graphics protocol
//! state and the renderer. It deliberately does not touch OpenGL: it produces
//! backend-independent [`ImageInstance`]s and [`ImageUpload`]s that the `gl`
//! backend turns into textured quads.
//!
//! # Coverage
//!
//! * protocol images stored by `kitty-screen` (`Screen::images()` /
//!   `Screen::placements()`) with destination cell/pixel offsets, explicit
//!   width/height, automatic aspect-ratio sizing, source rectangles, z-index
//!   layer ordering and clipping to the viewport;
//! * background images loaded from PNG through `kitty-graphics` with all six
//!   kitty layout modes;
//! * window logos with the nine kitty anchor positions, opacity and optional
//!   percentage scaling.
//!
//! Relative placements are resolved through `kitty_graphics::PlacementGraph`
//! (cycles, depth limits, virtual-placeholder anchors, inherited z-index/source
//! rectangles). Indexed-colour images keep their palette indices in the upload;
//! the GL shader performs a live palette lookup so colour changes do not
//! re-upload textures. Unicode placeholder runs and animation frames are
//! expanded here; the current animation frame takes precedence over the root
//! image stored on the screen.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;
use std::sync::Arc;

#[cfg(test)]
use kitty_core::cell::IMAGE_PLACEHOLDER_CHAR;
use kitty_graphics::{
    decode_png, AnimationManager, Image, ImageFormat, IndexedPalette, PlacementGraph, PlacementKey,
    ResolvedPlacement, MAX_IMAGE_DIMENSION,
};
use kitty_screen::Screen;

use crate::atlas::UvRect;
use crate::colors::Rgba8;
use crate::geom::{GridGeometry, Rect};
use crate::instances::InstanceFlags;

/// Shared RGBA8 pixel storage.
pub type ImageData = Arc<Vec<u8>>;

/// Identity of a GPU image texture.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ImageKey {
    /// A kitty graphics-protocol image, keyed by its client image id.
    Protocol(u32),
    /// The configured background image (only one is supported).
    Background(u32),
    /// The configured window logo (only one is supported).
    Logo,
}

/// Texture filtering for image quads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ImageFilter {
    #[default]
    Nearest,
    Linear,
}

/// Texture wrapping for image quads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ImageWrap {
    #[default]
    Clamp,
    Repeat,
    MirroredRepeat,
}

/// The draw position of an image relative to text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ImageLayer {
    /// Drawn before the cell backgrounds (kitty's "below" z-index range).
    #[default]
    BelowBackground,
    /// Drawn after cell backgrounds but before glyphs and decorations.
    BelowText,
    /// Drawn after glyphs and decorations, before the cursor.
    AboveText,
}

impl ImageLayer {
    /// Map kitty's signed z-index to a coarse layer.
    ///
    /// Kitty keeps very negative z-indices below even the background cells,
    /// negative indices between the background and the text, and non-negative
    /// indices above the text. The exact integer is still carried on the
    /// instance so a backend that supports more layers can sort within these
    /// groups.
    pub fn for_z_index(z: i32) -> Self {
        if z < i32::MIN / 2 {
            Self::BelowBackground
        } else if z < 0 {
            Self::BelowText
        } else {
            Self::AboveText
        }
    }
}

/// A decoded image ready for upload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageAsset {
    pub width: u32,
    pub height: u32,
    pub rgba: ImageData,
}

impl ImageAsset {
    /// Build an asset from tightly packed, top-down RGBA8 pixels.
    pub fn from_rgba(width: u32, height: u32, rgba: Vec<u8>) -> Result<Self, ImageLoadError> {
        if width == 0 || height == 0 {
            return Err(ImageLoadError::Decode(
                "image dimensions must be positive".into(),
            ));
        }
        let expected = width as usize * height as usize * 4;
        if rgba.len() != expected {
            return Err(ImageLoadError::Decode(format!(
                "RGBA buffer has {} bytes, expected {expected}",
                rgba.len()
            )));
        }
        Ok(Self {
            width,
            height,
            rgba: Arc::new(rgba),
        })
    }

    /// Decode PNG bytes through `kitty-graphics` (libpng).
    pub fn from_png_bytes(data: &[u8]) -> Result<Self, ImageLoadError> {
        let decoded = decode_png(data, MAX_IMAGE_DIMENSION)
            .map_err(|error| ImageLoadError::Decode(error.to_string()))?;
        Self::from_rgba(decoded.width, decoded.height, decoded.rgba)
    }

    /// Load and decode a PNG from `path`.
    pub fn from_png_path(path: impl AsRef<Path>) -> Result<Self, ImageLoadError> {
        let path = path.as_ref();
        let data = std::fs::read(path)
            .map_err(|error| ImageLoadError::Io(format!("{}: {error}", path.display())))?;
        Self::from_png_bytes(&data)
    }

    #[inline]
    pub fn row_stride(&self) -> usize {
        self.width as usize * 4
    }
}

/// Errors while loading a background image or logo.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImageLoadError {
    /// Reading the file failed.
    Io(String),
    /// PNG decoding or validation failed.
    Decode(String),
}

impl fmt::Display for ImageLoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(message) => write!(f, "failed to read image: {message}"),
            Self::Decode(message) => write!(f, "failed to decode image: {message}"),
        }
    }
}

impl std::error::Error for ImageLoadError {}

/// A texture upload for the backend.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageUpload {
    pub key: ImageKey,
    pub width: u32,
    pub height: u32,
    /// RGBA8 for ordinary images, or tightly packed palette indices for
    /// indexed protocol images (`f=8`/`f=16`).
    pub data: ImageData,
    pub filter: ImageFilter,
    pub wrap: ImageWrap,
    /// Set for `f=8`/`f=16`; the backend samples it as an integer texture and
    /// performs the palette lookup in the shader.
    pub indexed_format: Option<ImageFormat>,
    /// Palette snapshot used when the caller does not supply a live one on the
    /// frame. Renderers normally put the current palette on [`Frame`].
    pub palette: Option<IndexedPalette>,
    /// Cheap content fingerprint; the backend re-uploads only when it changes.
    pub fingerprint: u64,
}

impl ImageUpload {
    pub fn new(
        key: ImageKey,
        width: u32,
        height: u32,
        data: ImageData,
        filter: ImageFilter,
        wrap: ImageWrap,
    ) -> Self {
        let fingerprint = fingerprint(&data);
        Self {
            key,
            width,
            height,
            data,
            filter,
            wrap,
            indexed_format: None,
            palette: None,
            fingerprint,
        }
    }

    /// Build an upload from raw indexed samples. The index data is kept as-is;
    /// the GPU shader performs the palette lookup so terminal colour changes
    /// take effect without re-uploading the image.
    #[allow(clippy::too_many_arguments)]
    pub fn from_indexed(
        key: ImageKey,
        width: u32,
        height: u32,
        raw: &[u8],
        format: ImageFormat,
        palette: &IndexedPalette,
        filter: ImageFilter,
        wrap: ImageWrap,
    ) -> Option<Self> {
        if width == 0 || height == 0 || !format.is_indexed() {
            return None;
        }
        let expected = width as usize * height as usize * format.bytes_per_pixel()?;
        if raw.len() < expected {
            return None;
        }
        let data: ImageData = Arc::new(raw[..expected].to_vec());
        Some(Self {
            key,
            width,
            height,
            fingerprint: fingerprint(&data),
            data,
            filter,
            wrap,
            indexed_format: Some(format),
            palette: Some(palette.clone()),
        })
    }

    #[inline]
    pub fn bytes_per_pixel(&self) -> usize {
        self.indexed_format
            .and_then(ImageFormat::bytes_per_pixel)
            .unwrap_or(4)
    }

    #[inline]
    pub fn is_indexed(&self) -> bool {
        self.indexed_format.is_some()
    }

    #[inline]
    pub fn is_valid(&self) -> bool {
        self.width != 0
            && self.height != 0
            && self.data.len()
                >= self.width as usize * self.height as usize * self.bytes_per_pixel()
    }
}

/// FNV-1a over the pixel bytes. Enough to avoid needless GPU uploads without
/// pulling in a hashing crate.
pub fn fingerprint(data: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for &byte in data {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// A textured quad for one image placement.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ImageInstance {
    pub key: ImageKey,
    pub rect: Rect,
    pub uv: UvRect,
    pub z_index: i32,
    pub layer: ImageLayer,
    /// Tint colour (used by background images).
    pub color: Rgba8,
    /// How much of `color` to mix into the texture, in `[0, 1]`.
    pub tint: f32,
    /// Extra alpha multiplier in `[0, 1]`.
    pub opacity: f32,
    pub flags: InstanceFlags,
}

impl ImageInstance {
    /// Full-texture placement at `rect` with no tint and opaque alpha.
    pub fn new(key: ImageKey, rect: Rect) -> Self {
        Self {
            key,
            rect,
            uv: UvRect {
                u0: 0.0,
                v0: 0.0,
                u1: 1.0,
                v1: 1.0,
            },
            z_index: 0,
            layer: ImageLayer::AboveText,
            color: Rgba8::WHITE,
            tint: 0.0,
            opacity: 1.0,
            flags: InstanceFlags::default(),
        }
    }

    /// Apply a source rectangle in normalised texture coordinates.
    pub fn with_uv(mut self, uv: UvRect) -> Self {
        self.uv = uv;
        self
    }

    /// Apply a kitty z-index (which also selects the draw layer).
    pub fn with_z_index(mut self, z_index: i32) -> Self {
        self.z_index = z_index;
        self.layer = ImageLayer::for_z_index(z_index);
        self
    }

    /// Apply tint colour/amount and extra alpha.
    pub fn with_tint(mut self, color: Rgba8, amount: f32, opacity: f32) -> Self {
        self.color = color;
        self.tint = amount.clamp(0.0, 1.0);
        self.opacity = opacity.clamp(0.0, 1.0);
        self
    }
}

/// Kitty's `BackgroundImageLayout`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BackgroundLayout {
    #[default]
    Tiling,
    Scaled,
    Mirrored,
    Clamped,
    CenterClamped,
    CenterScaled,
}

impl BackgroundLayout {
    /// Parse a config value (`tiled`, `mirror-tiled`, `scaled`, `clamped`,
    /// `centered`, `cscaled`).
    pub fn from_name(name: &str) -> Self {
        match name {
            "mirror-tiled" | "mirrored" => Self::Mirrored,
            "scaled" => Self::Scaled,
            "clamped" => Self::Clamped,
            "centered" => Self::CenterClamped,
            "cscaled" => Self::CenterScaled,
            _ => Self::Tiling,
        }
    }

    /// GL wrap mode implied by the layout.
    pub fn wrap(self) -> ImageWrap {
        match self {
            Self::Mirrored => ImageWrap::MirroredRepeat,
            Self::Tiling => ImageWrap::Repeat,
            Self::Clamped | Self::Scaled | Self::CenterClamped | Self::CenterScaled => {
                ImageWrap::Clamp
            }
        }
    }
}

/// A loaded background image plus its layout options.
#[derive(Clone, Debug, PartialEq)]
pub struct BackgroundImageConfig {
    pub asset: ImageAsset,
    pub layout: BackgroundLayout,
    /// Extra alpha multiplier.
    pub opacity: f32,
    /// How much to tint with the resolved default background colour.
    pub tint: f32,
    /// Linear filtering when scaled.
    pub linear: bool,
}

impl BackgroundImageConfig {
    /// Load a background image from a PNG path.
    pub fn from_png_path(
        path: impl AsRef<Path>,
        layout: BackgroundLayout,
    ) -> Result<Self, ImageLoadError> {
        Ok(Self {
            asset: ImageAsset::from_png_path(path)?,
            layout,
            opacity: 1.0,
            tint: 0.0,
            linear: false,
        })
    }

    pub fn with_opacity(mut self, opacity: f32) -> Self {
        self.opacity = opacity.clamp(0.0, 1.0);
        self
    }

    pub fn with_tint(mut self, tint: f32) -> Self {
        self.tint = tint.clamp(0.0, 1.0);
        self
    }

    pub fn with_linear(mut self, linear: bool) -> Self {
        self.linear = linear;
        self
    }
}

/// One of kitty's nine window-logo anchor positions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LogoPosition {
    TopLeft,
    Top,
    TopRight,
    Left,
    Center,
    Right,
    BottomLeft,
    Bottom,
    #[default]
    BottomRight,
}

impl LogoPosition {
    pub fn from_name(name: &str) -> Self {
        match name {
            "top-left" => Self::TopLeft,
            "top" => Self::Top,
            "top-right" => Self::TopRight,
            "left" => Self::Left,
            "center" => Self::Center,
            "right" => Self::Right,
            "bottom-left" => Self::BottomLeft,
            "bottom" => Self::Bottom,
            _ => Self::BottomRight,
        }
    }

    #[inline]
    fn anchor(self) -> (f32, f32) {
        match self {
            Self::TopLeft => (0.0, 0.0),
            Self::Top => (0.5, 0.0),
            Self::TopRight => (1.0, 0.0),
            Self::Left => (0.0, 0.5),
            Self::Center => (0.5, 0.5),
            Self::Right => (1.0, 0.5),
            Self::BottomLeft => (0.0, 1.0),
            Self::Bottom => (0.5, 1.0),
            Self::BottomRight => (1.0, 1.0),
        }
    }
}

/// A loaded window logo plus its placement options.
#[derive(Clone, Debug, PartialEq)]
pub struct WindowLogoConfig {
    pub asset: ImageAsset,
    pub position: LogoPosition,
    pub alpha: f32,
    /// Percentage of the shortest viewport dimension. `0` keeps the natural
    /// size. When both are non-zero they are percentages of width and height.
    pub scale_x: f32,
    pub scale_y: f32,
    pub linear: bool,
}

impl WindowLogoConfig {
    pub fn from_png_path(path: impl AsRef<Path>) -> Result<Self, ImageLoadError> {
        Ok(Self {
            asset: ImageAsset::from_png_path(path)?,
            position: LogoPosition::default(),
            alpha: 0.5,
            scale_x: 0.0,
            scale_y: 0.0,
            linear: true,
        })
    }

    pub fn with_position(mut self, position: LogoPosition) -> Self {
        self.position = position;
        self
    }

    pub fn with_alpha(mut self, alpha: f32) -> Self {
        self.alpha = alpha.clamp(0.0, 1.0);
        self
    }

    pub fn with_scale(mut self, scale_x: f32, scale_y: f32) -> Self {
        self.scale_x = scale_x.max(0.0);
        self.scale_y = scale_y.max(0.0);
        self
    }

    pub fn with_linear(mut self, linear: bool) -> Self {
        self.linear = linear;
        self
    }
}

/// Clip `rect` to a viewport and adjust `uv` so the visible part still shows
/// the correct texels. Returns `None` when nothing remains.
pub fn clip_rect_uv(
    rect: Rect,
    uv: UvRect,
    viewport_width: f32,
    viewport_height: f32,
) -> Option<(Rect, UvRect)> {
    if rect.is_empty() || viewport_width <= 0.0 || viewport_height <= 0.0 {
        return None;
    }
    let left = rect.x.max(0.0);
    let top = rect.y.max(0.0);
    let right = rect.right().min(viewport_width);
    let bottom = rect.bottom().min(viewport_height);
    if right <= left || bottom <= top {
        return None;
    }
    let du = (uv.u1 - uv.u0) / rect.width;
    let dv = (uv.v1 - uv.v0) / rect.height;
    let u0 = uv.u0 + (left - rect.x) * du;
    let u1 = uv.u0 + (right - rect.x) * du;
    let v0 = uv.v0 + (top - rect.y) * dv;
    let v1 = uv.v0 + (bottom - rect.y) * dv;
    Some((
        Rect::new(left, top, right - left, bottom - top),
        UvRect { u0, v0, u1, v1 },
    ))
}

fn upload_for(
    key: ImageKey,
    asset: &ImageAsset,
    filter: ImageFilter,
    wrap: ImageWrap,
) -> ImageUpload {
    ImageUpload::new(
        key,
        asset.width,
        asset.height,
        asset.rgba.clone(),
        filter,
        wrap,
    )
}

/// Compute the background quad for `layout` over a viewport.
///
/// The returned UV rectangle can exceed `[0, 1]`; [`BackgroundLayout::wrap`]
/// tells the backend which GL wrap mode to use.
pub fn background_quad(
    asset: &ImageAsset,
    layout: BackgroundLayout,
    viewport_width: f32,
    viewport_height: f32,
) -> Option<(Rect, UvRect)> {
    let vw = viewport_width.max(0.0);
    let vh = viewport_height.max(0.0);
    let iw = asset.width as f32;
    let ih = asset.height as f32;
    if vw <= 0.0 || vh <= 0.0 || iw <= 0.0 || ih <= 0.0 {
        return None;
    }
    let full = Rect::new(0.0, 0.0, vw, vh);
    let unit = UvRect {
        u0: 0.0,
        v0: 0.0,
        u1: 1.0,
        v1: 1.0,
    };
    let (rect, uv) = match layout {
        BackgroundLayout::Tiling | BackgroundLayout::Mirrored => (
            full,
            UvRect {
                u1: vw / iw,
                v1: vh / ih,
                ..unit
            },
        ),
        BackgroundLayout::Scaled => (full, unit),
        BackgroundLayout::Clamped => (
            full,
            UvRect {
                u1: vw / iw,
                v1: vh / ih,
                ..unit
            },
        ),
        BackgroundLayout::CenterClamped => {
            (Rect::new((vw - iw) * 0.5, (vh - ih) * 0.5, iw, ih), unit)
        }
        BackgroundLayout::CenterScaled => {
            let (scaled_w, scaled_h) = if iw / ih > vw / vh {
                let h = vh;
                (h * iw / ih, h)
            } else {
                let w = vw;
                (w, w * ih / iw)
            };
            (
                Rect::new(
                    (vw - scaled_w) * 0.5,
                    (vh - scaled_h) * 0.5,
                    scaled_w,
                    scaled_h,
                ),
                unit,
            )
        }
    };
    clip_rect_uv(rect, uv, vw, vh)
}

/// Build the background image instance and upload for `config`.
pub fn background_image(
    config: &BackgroundImageConfig,
    viewport_width: f32,
    viewport_height: f32,
    tint_color: Rgba8,
) -> Option<(ImageInstance, ImageUpload)> {
    let (rect, uv) = background_quad(
        &config.asset,
        config.layout,
        viewport_width,
        viewport_height,
    )?;
    let filter = if config.linear {
        ImageFilter::Linear
    } else {
        ImageFilter::Nearest
    };
    let upload = upload_for(
        ImageKey::Background(0),
        &config.asset,
        filter,
        config.layout.wrap(),
    );
    let instance = ImageInstance::new(ImageKey::Background(0), rect)
        .with_uv(uv)
        .with_z_index(i32::MIN)
        .with_tint(tint_color, config.tint, config.opacity);
    Some((instance, upload))
}

/// Compute the window-logo quad for the given viewport.
pub fn window_logo_quad(
    config: &WindowLogoConfig,
    viewport_width: f32,
    viewport_height: f32,
) -> Option<(Rect, UvRect)> {
    let vw = viewport_width.max(0.0);
    let vh = viewport_height.max(0.0);
    let iw = config.asset.width as f32;
    let ih = config.asset.height as f32;
    if vw <= 0.0 || vh <= 0.0 || iw <= 0.0 || ih <= 0.0 {
        return None;
    }
    let shortest = vw.min(vh);
    let (mut width, mut height) = (iw, ih);
    if config.scale_x > 0.0 && config.scale_y > 0.0 {
        width = vw * config.scale_x / 100.0;
        height = vh * config.scale_y / 100.0;
    } else if config.scale_x > 0.0 {
        width = shortest * config.scale_x / 100.0;
        height = width * ih / iw;
    } else if config.scale_y > 0.0 {
        height = shortest * config.scale_y / 100.0;
        width = height * iw / ih;
    }
    let (ax, ay) = config.position.anchor();
    let rect = Rect::new((vw - width) * ax, (vh - height) * ay, width, height);
    clip_rect_uv(
        rect,
        UvRect {
            u0: 0.0,
            v0: 0.0,
            u1: 1.0,
            v1: 1.0,
        },
        vw,
        vh,
    )
}

/// Build the logo instance and upload for `config`.
pub fn window_logo(
    config: &WindowLogoConfig,
    viewport_width: f32,
    viewport_height: f32,
) -> Option<(ImageInstance, ImageUpload)> {
    let (rect, uv) = window_logo_quad(config, viewport_width, viewport_height)?;
    let upload = upload_for(
        ImageKey::Logo,
        &config.asset,
        if config.linear {
            ImageFilter::Linear
        } else {
            ImageFilter::Nearest
        },
        ImageWrap::Clamp,
    );
    let instance = ImageInstance::new(ImageKey::Logo, rect)
        .with_uv(uv)
        .with_z_index(0)
        .with_tint(Rgba8::WHITE, 0.0, config.alpha);
    Some((instance, upload))
}

pub use kitty_screen::{color_to_image_id, diacritic_to_num};

/// One horizontal run of `U+10EEEE` placeholder cells.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlaceholderRun {
    pub image_id: u32,
    pub placement_id: u32,
    pub image_row: u32,
    pub image_column: u32,
    pub screen_x: u32,
    pub screen_y: u32,
    /// Number of placeholder cells (image-grid columns).
    pub columns: u32,
    /// Screen columns covered, including wide/multicell cells.
    pub screen_columns: u32,
}

/// Expand all Unicode placeholder cells into image-space runs.
pub fn expand_unicode_placeholders(screen: &Screen) -> Vec<PlaceholderRun> {
    let mut runs = Vec::new();
    let mut run: Option<PlaceholderRun> = None;
    let mut prev_row = 1u32;
    let mut prev_column = 1u32;
    let mut prev_high = 1u32;
    for cell in screen.materialize_virtual_placements() {
        if let Some(current) = run.as_mut() {
            let continues = cell.screen_y == current.screen_y
                && cell.screen_x == current.screen_x + current.screen_columns
                && current.image_id == cell.image_id
                && current.placement_id == cell.placement_id
                && (cell.image_row == 0 || cell.image_row == prev_row)
                && (cell.image_column == 0 || cell.image_column == prev_column + 1)
                && (cell.image_id_high == 0 || cell.image_id_high == prev_high);
            if continues {
                current.columns += 1;
                current.screen_columns += cell.screen_columns;
                prev_row = prev_row.max(1);
                prev_column += 1;
                prev_high = prev_high.max(1);
                continue;
            }
            if let Some(finished) = run.take() {
                runs.push(finished);
            }
        }
        let row = cell.image_row.max(1);
        let column = cell.image_column.max(1);
        let high = cell.image_id_high.max(1);
        run = Some(PlaceholderRun {
            image_id: cell.image_id,
            placement_id: cell.placement_id,
            image_row: row - 1,
            image_column: column.saturating_sub(1),
            screen_x: cell.screen_x,
            screen_y: cell.screen_y,
            columns: 1,
            screen_columns: cell.screen_columns.max(1),
        });
        prev_row = row;
        prev_column = column;
        prev_high = high;
    }
    if let Some(run) = run {
        runs.push(run);
    }
    runs.retain(|run| run.columns > 0);
    runs
}

fn image_pixels(
    image: &Image,
    image_id: u32,
    animations: Option<&AnimationManager>,
    palette: &IndexedPalette,
) -> Option<(ImageData, u32, u32, Option<ImageFormat>)> {
    if let Some(animated) = animations.and_then(|manager| manager.image(image_id)) {
        return Some((
            animated.current_data(),
            animated.width,
            animated.height,
            None,
        ));
    }
    if image.width == 0 || image.height == 0 {
        return None;
    }
    if let Some(rgba) = image.rgba.as_ref() {
        return Some((Arc::new(rgba.clone()), image.width, image.height, None));
    }
    if image.format.is_indexed() {
        // Keep palette indices raw; the GPU shader performs the live lookup.
        return Some((
            Arc::new(image.raw.clone()),
            image.width,
            image.height,
            Some(image.format),
        ));
    }
    let _ = palette;
    None
}

fn protocol_upload(
    key: ImageKey,
    width: u32,
    height: u32,
    data: ImageData,
    indexed_format: Option<ImageFormat>,
    palette: &IndexedPalette,
    filter: ImageFilter,
) -> Option<ImageUpload> {
    match indexed_format {
        Some(format) => ImageUpload::from_indexed(
            key,
            width,
            height,
            &data,
            format,
            palette,
            filter,
            ImageWrap::Clamp,
        ),
        None => Some(ImageUpload::new(
            key,
            width,
            height,
            data,
            filter,
            ImageWrap::Clamp,
        )),
    }
}

/// Build the relative-placement graph for a screen, including anchors for
/// virtual placements derived from their placeholder cells.
fn placement_graph(screen: &Screen, _runs: &[PlaceholderRun]) -> PlacementGraph {
    screen.placement_graph()
}

#[allow(clippy::too_many_arguments)]
fn protocol_placement(
    placement: &kitty_screen::ImagePlacement,
    resolved: &ResolvedPlacement,
    data: ImageData,
    image_width: u32,
    image_height: u32,
    indexed_format: Option<ImageFormat>,
    palette: &IndexedPalette,
    geometry: &GridGeometry,
    viewport_width: f32,
    viewport_height: f32,
    filter: ImageFilter,
) -> Option<(ImageInstance, ImageUpload)> {
    let p = &placement.placement;
    let cell_w = geometry.cell_width;
    let cell_h = geometry.cell_height;
    let source = resolved.source.clamped(image_width, image_height);
    let source_w = if source.width == 0 {
        image_width as f32
    } else {
        source.width as f32
    };
    let source_h = if source.height == 0 {
        image_height as f32
    } else {
        source.height as f32
    };

    let mut width = if p.num_cols > 0 {
        p.num_cols as f32 * cell_w
    } else {
        source_w
    };
    let mut height = if p.num_rows > 0 {
        p.num_rows as f32 * cell_h
    } else {
        source_h
    };
    if p.num_cols == 0 && p.num_rows > 0 && source_h > 0.0 {
        width = height * source_w / source_h;
    } else if p.num_rows == 0 && p.num_cols > 0 && source_w > 0.0 {
        height = width * source_h / source_w;
    }
    if width <= 0.0 || height <= 0.0 {
        return None;
    }

    let rect = Rect::new(
        geometry.origin_x + resolved.anchor_x as f32 * cell_w + p.cell_x_offset as f32,
        geometry.origin_y + resolved.anchor_y as f32 * cell_h + p.cell_y_offset as f32,
        width,
        height,
    );
    let iw = image_width as f32;
    let ih = image_height as f32;
    let uv = UvRect {
        u0: (source.x as f32 / iw).clamp(0.0, 1.0),
        v0: (source.y as f32 / ih).clamp(0.0, 1.0),
        u1: ((source.x as f32 + source_w) / iw).clamp(0.0, 1.0),
        v1: ((source.y as f32 + source_h) / ih).clamp(0.0, 1.0),
    };
    let (rect, uv) = clip_rect_uv(rect, uv, viewport_width, viewport_height)?;
    let key = ImageKey::Protocol(placement.image_id);
    let upload = protocol_upload(
        key,
        image_width,
        image_height,
        data,
        indexed_format,
        palette,
        filter,
    )?;
    let instance = ImageInstance::new(key, rect)
        .with_uv(uv)
        .with_z_index(resolved.z_index);
    Some((instance, upload))
}

fn placeholder_quad(
    run: &PlaceholderRun,
    virtual_placement: &kitty_screen::ImagePlacement,
    image_width: u32,
    image_height: u32,
    geometry: &GridGeometry,
) -> Option<(Rect, UvRect)> {
    let cell_w = geometry.cell_width;
    let cell_h = geometry.cell_height;
    let iw = image_width as f32;
    let ih = image_height as f32;
    if iw <= 0.0 || ih <= 0.0 || cell_w <= 0.0 || cell_h <= 0.0 {
        return None;
    }
    let mut box_cols = virtual_placement.placement.num_cols;
    let mut box_rows = virtual_placement.placement.num_rows;
    if box_cols == 0 {
        box_cols = ((image_width as f32 + cell_w - 1.0) / cell_w).max(1.0) as u32;
    }
    if box_rows == 0 {
        box_rows = ((image_height as f32 + cell_h - 1.0) / cell_h).max(1.0) as u32;
    }
    let (x_scale, x_offset, y_scale, y_offset) =
        if iw * box_rows as f32 * cell_h > ih * box_cols as f32 * cell_w {
            let scale = (box_cols as f32 * cell_w) / iw.max(1.0);
            (
                scale,
                0.0,
                scale,
                (box_rows as f32 * cell_h - ih * scale) / 2.0,
            )
        } else {
            let scale = (box_rows as f32 * cell_h) / ih.max(1.0);
            (
                scale,
                (box_cols as f32 * cell_w - iw * scale) / 2.0,
                scale,
                0.0,
            )
        };
    let mut start_col = run.screen_x;
    let mut start_row = run.screen_y;
    let mut columns = run.screen_columns.max(1) as f32;
    let mut rows = 1.0f32;
    let mut cell_x_offset = 0.0f32;
    let mut cell_y_offset = 0.0f32;
    let mut src_x = (run.image_column as f32 * cell_w - x_offset) / x_scale;
    let mut src_y = (run.image_row as f32 * cell_h - y_offset) / y_scale;
    let mut src_w = columns * cell_w / x_scale;
    let mut src_h = rows * cell_h / y_scale;
    if src_x < 0.0 {
        src_w += src_x;
        cell_x_offset = -src_x * x_scale;
        src_x = 0.0;
        let col_offset = (cell_x_offset / cell_w) as u32;
        start_col = start_col.saturating_add(col_offset);
        if columns <= col_offset as f32 {
            return None;
        }
        columns -= col_offset as f32;
        cell_x_offset %= cell_w;
    }
    if src_y < 0.0 {
        src_h += src_y;
        cell_y_offset = -src_y * y_scale;
        src_y = 0.0;
        let row_offset = (cell_y_offset / cell_h) as u32;
        start_row = start_row.saturating_add(row_offset);
        if rows <= row_offset as f32 {
            return None;
        }
        rows -= row_offset as f32;
        cell_y_offset %= cell_h;
    }
    if src_x + src_w > iw {
        let redundant = (src_x + src_w - iw) * x_scale / cell_w;
        let redundant = redundant as u32;
        if columns <= redundant as f32 {
            return None;
        }
        src_w -= redundant as f32 * cell_w / x_scale;
        columns -= redundant as f32;
    }
    if src_y + src_h > ih {
        let redundant = (src_y + src_h - ih) * y_scale / cell_h;
        let redundant = redundant as u32;
        if rows <= redundant as f32 {
            return None;
        }
        src_h -= redundant as f32 * cell_h / y_scale;
        rows -= redundant as f32;
    }
    if src_w <= 0.0 || src_h <= 0.0 || columns <= 0.0 || rows <= 0.0 {
        return None;
    }
    let rect = Rect::new(
        geometry.origin_x + start_col as f32 * cell_w + cell_x_offset,
        geometry.origin_y + start_row as f32 * cell_h + cell_y_offset,
        columns * cell_w,
        rows * cell_h,
    );
    let uv = UvRect {
        u0: (src_x / iw).clamp(0.0, 1.0),
        v0: (src_y / ih).clamp(0.0, 1.0),
        u1: ((src_x + src_w) / iw).clamp(0.0, 1.0),
        v1: ((src_y + src_h) / ih).clamp(0.0, 1.0),
    };
    Some((rect, uv))
}

/// Collect visible protocol placements and Unicode placeholder runs.
///
/// Placements are sorted by `(z-index, image id, placement id)`, matching
/// kitty's draw order. Duplicate textures for the same image are uploaded only
/// once per frame. When `animations` contains an image, its current frame is
/// used instead of the root image stored on the screen.
#[allow(clippy::too_many_arguments)]
pub fn collect_protocol_images_with_animations(
    screen: &Screen,
    animations: Option<&AnimationManager>,
    geometry: &GridGeometry,
    viewport_width: f32,
    viewport_height: f32,
    filter: ImageFilter,
    instances: &mut Vec<ImageInstance>,
    uploads: &mut Vec<ImageUpload>,
) {
    let palette = IndexedPalette::default();
    collect_protocol_images_with_palette(
        screen,
        animations,
        &palette,
        geometry,
        viewport_width,
        viewport_height,
        filter,
        instances,
        uploads,
    );
}

/// Like [`collect_protocol_images_with_animations`], but attaches the live
/// `palette` to indexed protocol uploads and resolves relative placements.
#[allow(clippy::too_many_arguments)]
pub fn collect_protocol_images_with_palette(
    screen: &Screen,
    animations: Option<&AnimationManager>,
    palette: &IndexedPalette,
    geometry: &GridGeometry,
    viewport_width: f32,
    viewport_height: f32,
    filter: ImageFilter,
    instances: &mut Vec<ImageInstance>,
    uploads: &mut Vec<ImageUpload>,
) {
    let runs = expand_unicode_placeholders(screen);
    let graph = placement_graph(screen, &runs);
    let resolved: BTreeMap<PlacementKey, ResolvedPlacement> =
        graph.resolve_all().into_iter().collect();
    let mut seen_uploads: Vec<ImageKey> = Vec::new();
    let mut collected: Vec<(i32, u32, u32, u32, ImageInstance, Option<ImageUpload>)> = Vec::new();
    let mut order = 0u32;

    for placement in screen.placements() {
        if placement.placement.unicode_placement {
            continue;
        }
        let key = PlacementKey::new(placement.image_id, placement.placement_id);
        let Some(resolved) = resolved.get(&key) else {
            // Broken, cyclic, over-deep or self-referential relative chain.
            continue;
        };
        let Some(image) = screen.image(placement.image_id) else {
            continue;
        };
        let Some((data, width, height, indexed_format)) =
            image_pixels(image, placement.image_id, animations, palette)
        else {
            continue;
        };
        let Some((instance, upload)) = protocol_placement(
            placement,
            resolved,
            data,
            width,
            height,
            indexed_format,
            palette,
            geometry,
            viewport_width,
            viewport_height,
            filter,
        ) else {
            continue;
        };
        let upload = if seen_uploads.contains(&instance.key) {
            None
        } else {
            seen_uploads.push(instance.key);
            Some(upload)
        };
        collected.push((
            instance.z_index,
            placement.image_id,
            placement.placement_id,
            order,
            instance,
            upload,
        ));
        order = order.wrapping_add(1);
    }

    for run in runs {
        let Some(image) = screen.image(run.image_id) else {
            continue;
        };
        let Some(virtual_placement) = screen.placements().iter().find(|placement| {
            placement.image_id == run.image_id
                && placement.placement.unicode_placement
                && (run.placement_id == 0 || placement.placement_id == run.placement_id)
        }) else {
            continue;
        };
        let Some((data, width, height, indexed_format)) =
            image_pixels(image, run.image_id, animations, palette)
        else {
            continue;
        };
        let Some((rect, uv)) = placeholder_quad(&run, virtual_placement, width, height, geometry)
        else {
            continue;
        };
        let key = ImageKey::Protocol(run.image_id);
        let upload = if seen_uploads.contains(&key) {
            None
        } else {
            seen_uploads.push(key);
            protocol_upload(key, width, height, data, indexed_format, palette, filter)
        };
        let instance = ImageInstance::new(key, rect).with_uv(uv).with_z_index(-1);
        collected.push((
            -1,
            run.image_id,
            virtual_placement.placement_id,
            order,
            instance,
            upload,
        ));
        order = order.wrapping_add(1);
    }

    collected.sort_by_key(|(z, image_id, placement_id, order, _, _)| {
        (*z, *image_id, *placement_id, *order)
    });
    for (_, _, _, _, instance, upload) in collected {
        instances.push(instance);
        if let Some(upload) = upload {
            uploads.push(upload);
        }
    }
}

/// Collect visible protocol placements from `screen` without animation state.
#[allow(clippy::too_many_arguments)]
pub fn collect_protocol_images(
    screen: &Screen,
    geometry: &GridGeometry,
    viewport_width: f32,
    viewport_height: f32,
    filter: ImageFilter,
    instances: &mut Vec<ImageInstance>,
    uploads: &mut Vec<ImageUpload>,
) {
    collect_protocol_images_with_animations(
        screen,
        None,
        geometry,
        viewport_width,
        viewport_height,
        filter,
        instances,
        uploads,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atlas::GlyphAtlas;
    use crate::geom::CellMetrics;
    use crate::instances::RenderConfig;
    use kitty_graphics::png::encode_png_rgba;

    fn asset_2x2() -> ImageAsset {
        ImageAsset::from_rgba(
            2,
            2,
            vec![
                255, 0, 0, 255, // top-left red
                0, 255, 0, 255, // top-right green
                0, 0, 255, 255, // bottom-left blue
                255, 255, 255, 255, // bottom-right white
            ],
        )
        .unwrap()
    }

    #[test]
    fn png_asset_round_trip() {
        let png = encode_png_rgba(2, 2, &asset_2x2().rgba).unwrap();
        let asset = ImageAsset::from_png_bytes(&png).unwrap();
        assert_eq!((asset.width, asset.height), (2, 2));
        assert_eq!(asset.rgba.as_ref(), asset_2x2().rgba.as_ref());
    }

    #[test]
    fn rejects_bad_rgba_size_and_empty_png() {
        assert!(ImageAsset::from_rgba(2, 2, vec![0; 3]).is_err());
        assert!(ImageAsset::from_rgba(0, 2, vec![]).is_err());
        assert!(ImageAsset::from_png_bytes(b"not a png").is_err());
    }

    #[test]
    fn z_index_maps_to_layers() {
        assert_eq!(
            ImageLayer::for_z_index(i32::MIN),
            ImageLayer::BelowBackground
        );
        assert_eq!(ImageLayer::for_z_index(-1), ImageLayer::BelowText);
        assert_eq!(ImageLayer::for_z_index(0), ImageLayer::AboveText);
        assert_eq!(ImageLayer::for_z_index(9), ImageLayer::AboveText);
    }

    #[test]
    fn clip_adjusts_uv() {
        let rect = Rect::new(-10.0, -10.0, 20.0, 20.0);
        let uv = UvRect {
            u0: 0.0,
            v0: 0.0,
            u1: 1.0,
            v1: 1.0,
        };
        let (rect, uv) = clip_rect_uv(rect, uv, 100.0, 100.0).unwrap();
        assert_eq!(rect, Rect::new(0.0, 0.0, 10.0, 10.0));
        assert!((uv.u0 - 0.5).abs() < 1e-6);
        assert!((uv.u1 - 1.0).abs() < 1e-6);
        assert!((uv.v0 - 0.5).abs() < 1e-6);
        assert!((uv.v1 - 1.0).abs() < 1e-6);
        assert!(clip_rect_uv(Rect::new(200.0, 0.0, 1.0, 1.0), uv, 100.0, 100.0).is_none());
    }

    #[test]
    fn background_layouts_have_expected_quads() {
        let a = asset_2x2();

        let (rect, uv) = background_quad(&a, BackgroundLayout::Tiling, 32.0, 16.0).unwrap();
        assert_eq!(rect, Rect::new(0.0, 0.0, 32.0, 16.0));
        assert_eq!((uv.u1, uv.v1), (16.0, 8.0));
        assert_eq!(BackgroundLayout::Tiling.wrap(), ImageWrap::Repeat);

        let (_, uv) = background_quad(&a, BackgroundLayout::Scaled, 32.0, 16.0).unwrap();
        assert_eq!((uv.u1, uv.v1), (1.0, 1.0));
        assert_eq!(BackgroundLayout::Scaled.wrap(), ImageWrap::Clamp);

        let (_, uv) = background_quad(&a, BackgroundLayout::Mirrored, 32.0, 16.0).unwrap();
        assert_eq!((uv.u1, uv.v1), (16.0, 8.0));
        assert_eq!(BackgroundLayout::Mirrored.wrap(), ImageWrap::MirroredRepeat);

        let (_, uv) = background_quad(&a, BackgroundLayout::Clamped, 32.0, 16.0).unwrap();
        assert_eq!((uv.u1, uv.v1), (16.0, 8.0));
        assert_eq!(BackgroundLayout::Clamped.wrap(), ImageWrap::Clamp);

        let (rect, _) = background_quad(&a, BackgroundLayout::CenterClamped, 32.0, 16.0).unwrap();
        assert_eq!(rect, Rect::new(15.0, 7.0, 2.0, 2.0));

        let (rect, _) = background_quad(&a, BackgroundLayout::CenterScaled, 32.0, 32.0).unwrap();
        assert_eq!(rect, Rect::new(0.0, 0.0, 32.0, 32.0));

        // Center-scaled uses aspect-preserving *cover*, so a square image in a
        // wide viewport is scaled to the viewport width and clipped vertically.
        let (rect, _) = background_quad(&a, BackgroundLayout::CenterScaled, 64.0, 16.0).unwrap();
        assert_eq!(rect, Rect::new(0.0, 0.0, 64.0, 16.0));
    }

    #[test]
    fn logo_positions_and_scaling() {
        let a = asset_2x2();
        let base = WindowLogoConfig {
            asset: a,
            position: LogoPosition::Center,
            alpha: 0.5,
            scale_x: 0.0,
            scale_y: 0.0,
            linear: true,
        };
        let (rect, _) = window_logo_quad(&base, 32.0, 16.0).unwrap();
        assert_eq!(rect, Rect::new(15.0, 7.0, 2.0, 2.0));

        let scaled = WindowLogoConfig {
            position: LogoPosition::TopLeft,
            scale_x: 50.0,
            ..base
        };
        let (rect, _) = window_logo_quad(&scaled, 32.0, 16.0).unwrap();
        assert_eq!(rect, Rect::new(0.0, 0.0, 8.0, 8.0));
    }
    #[test]
    fn loads_background_and_logo_from_png_path() {
        use kitty_graphics::png::encode_png_rgba;

        let png = encode_png_rgba(2, 2, &asset_2x2().rgba).unwrap();
        let path = std::env::temp_dir().join(format!(
            "kitty-render-bg-{}-{}.png",
            std::process::id(),
            fingerprint(&png)
        ));
        std::fs::write(&path, &png).unwrap();
        let background =
            BackgroundImageConfig::from_png_path(&path, BackgroundLayout::Scaled).unwrap();
        assert_eq!((background.asset.width, background.asset.height), (2, 2));
        let logo = WindowLogoConfig::from_png_path(&path)
            .unwrap()
            .with_position(LogoPosition::TopLeft)
            .with_alpha(0.25)
            .with_scale(50.0, 0.0);
        assert_eq!(logo.position, LogoPosition::TopLeft);
        let (rect, _) = window_logo_quad(&logo, 32.0, 16.0).unwrap();
        assert_eq!(rect, Rect::new(0.0, 0.0, 8.0, 8.0));
        let _ = std::fs::remove_file(path);
    }
    fn placeholder_screen(id: u32, rows: u32, columns: u32) -> Screen {
        let mut screen = Screen::builder(columns + 1, rows + 1)
            .scrollback(8)
            .build()
            .unwrap();
        let mut image =
            kitty_graphics::parse_raw(format!("a=t,f=32,s=1,v=1,i={id}").as_bytes()).unwrap();
        image.payload = vec![9, 8, 7, 255];
        screen.handle_graphics_command(image).unwrap();
        let mut virtual_placement =
            kitty_graphics::parse_raw(format!("a=p,U=1,i={id},c={columns},r={rows}").as_bytes())
                .unwrap();
        virtual_placement.payload.clear();
        screen.handle_graphics_command(virtual_placement).unwrap();
        screen.cursor.sgr.fg = (id << 8) | 1;
        screen.cursor.sgr.decoration_fg = 0;
        for row in 0..rows {
            let mut text = Vec::new();
            for column in 0..columns {
                text.push(IMAGE_PLACEHOLDER_CHAR);
                text.push(if row == 0 { 0x305 } else { 0x30d + row - 1 });
                text.push(if column == 0 {
                    0x305
                } else {
                    0x30d + column - 1
                });
            }
            screen.cursor_position(row + 1, 1);
            screen.draw_text(&text);
        }
        screen
    }

    #[test]
    fn placeholder_diacritic_table_is_one_based() {
        assert_eq!(diacritic_to_num(0x305), 1);
        assert_eq!(diacritic_to_num(0x30d), 2);
        assert_eq!(diacritic_to_num(0x30e), 3);
        assert_eq!(diacritic_to_num(0x1d245), 298);
        assert_eq!(diacritic_to_num(0x41), 0);
        assert_eq!(color_to_image_id((42 << 8) | 1), 42);
        assert_eq!(
            color_to_image_id((0x12 << 24) | (0x34 << 16) | (0x56 << 8) | 2),
            0x123456
        );
    }

    #[test]
    fn placeholder_runs_decode_rows_and_columns() {
        let screen = placeholder_screen(42, 2, 2);
        let runs = expand_unicode_placeholders(&screen);
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].image_id, 42);
        assert_eq!((runs[0].image_row, runs[0].image_column), (0, 0));
        assert_eq!((runs[0].screen_x, runs[0].screen_y), (0, 0));
        assert_eq!((runs[0].columns, runs[0].screen_columns), (2, 2));
        assert_eq!((runs[1].image_row, runs[1].image_column), (1, 0));
        assert_eq!((runs[1].screen_x, runs[1].screen_y), (0, 1));
    }

    #[test]
    fn placeholder_runs_inherit_columns_to_the_right() {
        let mut screen = Screen::builder(4, 2).scrollback(8).build().unwrap();
        screen.cursor.sgr.fg = (7 << 8) | 1;
        screen.draw_text(&[IMAGE_PLACEHOLDER_CHAR, 0x305]);
        screen.draw_text(&[IMAGE_PLACEHOLDER_CHAR, 0x305]);
        let runs = expand_unicode_placeholders(&screen);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].image_id, 7);
        assert_eq!((runs[0].image_row, runs[0].image_column), (0, 0));
        assert_eq!(runs[0].columns, 2);
    }

    #[test]
    fn placeholder_high_id_byte_is_decoded() {
        let mut screen = Screen::builder(4, 1).scrollback(8).build().unwrap();
        screen.cursor.sgr.fg = (42 << 8) | 1;
        screen.draw_text(&[IMAGE_PLACEHOLDER_CHAR, 0x305, 0x305, 0x30e]);
        let runs = expand_unicode_placeholders(&screen);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].image_id, 42 + (2 << 24));
        assert_eq!(runs[0].placement_id, 0);
    }

    #[test]
    fn placeholder_quad_uses_screen_columns_for_wide_cells() {
        let mut screen = Screen::builder(4, 2).scrollback(8).build().unwrap();
        let mut image = kitty_graphics::parse_raw(b"a=t,f=32,s=1,v=1,i=9").unwrap();
        image.payload = vec![1, 2, 3, 255];
        screen.handle_graphics_command(image).unwrap();
        let mut virtual_placement = kitty_graphics::parse_raw(b"a=p,U=1,i=9,c=2,r=1").unwrap();
        virtual_placement.payload.clear();
        screen.handle_graphics_command(virtual_placement).unwrap();
        let run = PlaceholderRun {
            image_id: 9,
            placement_id: 0,
            image_row: 0,
            image_column: 0,
            screen_x: 0,
            screen_y: 0,
            columns: 1,
            screen_columns: 2,
        };
        let geometry = GridGeometry::new(4, 2, CellMetrics::default());
        let placement = screen
            .placements()
            .iter()
            .find(|p| p.placement.unicode_placement)
            .unwrap();
        let (rect, _) = placeholder_quad(&run, placement, 1, 1, &geometry).unwrap();
        assert_eq!(rect.width, geometry.cell_width * 2.0);
    }

    #[test]
    fn missing_placeholder_image_suppresses_the_glyph() {
        use crate::atlas::{AtlasConfig, GlyphBitmap, GlyphKey};
        use crate::colors::BasicColorResolver;
        use crate::instances::InstanceGenerator;

        let mut screen = Screen::builder(4, 2).scrollback(8).build().unwrap();
        screen.cursor.sgr.fg = (99 << 8) | 1;
        screen.draw_text(&[IMAGE_PLACEHOLDER_CHAR, 0x305, 0x305]);
        let generator = InstanceGenerator::new(RenderConfig::default());
        let mut atlas = GlyphAtlas::new(AtlasConfig::default());
        let mut rasterizer = |_key: &GlyphKey| GlyphBitmap::gray(8, 12, vec![0xff; 96]).ok();
        let resolver = BasicColorResolver::default();
        let mut frame = crate::instances::Frame::default();
        generator.generate(&screen, &mut atlas, &mut rasterizer, &resolver, &mut frame);
        assert!(frame.glyphs.is_empty());
        assert!(frame.images.is_empty());
    }

    fn protocol_screen(id: u32, pixels: Vec<u8>) -> Screen {
        let mut screen = Screen::builder(4, 3).scrollback(8).build().unwrap();
        let mut image =
            kitty_graphics::parse_raw(format!("a=t,f=32,s=1,v=1,i={id}").as_bytes()).unwrap();
        image.payload = pixels;
        screen.handle_graphics_command(image).unwrap();
        let mut placement =
            kitty_graphics::parse_raw(format!("a=p,i={id},p=1,c=1,r=1,z=0").as_bytes()).unwrap();
        placement.payload.clear();
        screen.handle_graphics_command(placement).unwrap();
        screen
    }

    fn animated_frame(id: u32, color: [u8; 4]) -> kitty_graphics::GraphicsCommand {
        let mut command =
            kitty_graphics::parse_raw(format!("a=f,i={id},r=2,s=1,v=1").as_bytes()).unwrap();
        command.payload = color.to_vec();
        command
    }

    #[test]
    fn animation_current_frame_is_used_for_uploads() {
        use kitty_graphics::{AnimationManager, AnimationState, ImageLimits};

        let screen = protocol_screen(5, vec![255, 0, 0, 255]);
        let mut manager = AnimationManager::new();
        manager.insert_root(screen.image(5).unwrap()).unwrap();
        manager
            .feed(
                &animated_frame(5, [0, 255, 0, 255]),
                &ImageLimits::default(),
                0,
            )
            .unwrap();
        manager.set_gap(5, 1, 10);
        manager.set_gap(5, 2, 10);
        let geometry = GridGeometry::new(4, 3, CellMetrics::default());
        let mut instances = Vec::new();
        let mut uploads = Vec::new();
        collect_protocol_images_with_animations(
            &screen,
            Some(&manager),
            &geometry,
            40.0,
            60.0,
            ImageFilter::Nearest,
            &mut instances,
            &mut uploads,
        );
        assert_eq!(instances.len(), 1);
        assert_eq!(uploads[0].data.as_slice(), &[255, 0, 0, 255]);

        manager.set_state(5, AnimationState::Running, 0);
        assert_eq!(manager.tick(10).redraw, vec![5]);
        instances.clear();
        uploads.clear();
        collect_protocol_images_with_animations(
            &screen,
            Some(&manager),
            &geometry,
            40.0,
            60.0,
            ImageFilter::Nearest,
            &mut instances,
            &mut uploads,
        );
        assert_eq!(uploads[0].data.as_slice(), &[0, 255, 0, 255]);
    }

    #[test]
    fn deleted_animation_frame_falls_back_to_root() {
        use kitty_graphics::{AnimationManager, ImageLimits};

        let screen = protocol_screen(6, vec![1, 2, 3, 255]);
        let mut manager = AnimationManager::new();
        manager.insert_root(screen.image(6).unwrap()).unwrap();
        manager
            .feed(
                &animated_frame(6, [4, 5, 6, 255]),
                &ImageLimits::default(),
                0,
            )
            .unwrap();
        assert!(manager.select_frame(6, 2, 0));
        assert!(manager.delete_frame(6, 2));
        let geometry = GridGeometry::new(4, 3, CellMetrics::default());
        let mut instances = Vec::new();
        let mut uploads = Vec::new();
        collect_protocol_images_with_animations(
            &screen,
            Some(&manager),
            &geometry,
            40.0,
            60.0,
            ImageFilter::Nearest,
            &mut instances,
            &mut uploads,
        );
        assert_eq!(uploads[0].data.as_slice(), &[1, 2, 3, 255]);
    }

    #[test]
    fn animation_preserves_z_order() {
        use kitty_graphics::{AnimationManager, ImageLimits};

        let mut screen = protocol_screen(7, vec![1, 1, 1, 255]);
        let mut second = kitty_graphics::parse_raw(b"a=t,f=32,s=1,v=1,i=8").unwrap();
        second.payload = vec![2, 2, 2, 255];
        screen.handle_graphics_command(second).unwrap();
        let mut under = kitty_graphics::parse_raw(b"a=p,i=7,p=1,z=-1").unwrap();
        under.payload.clear();
        screen.handle_graphics_command(under).unwrap();
        let mut over = kitty_graphics::parse_raw(b"a=p,i=8,p=1,z=3").unwrap();
        over.payload.clear();
        screen.handle_graphics_command(over).unwrap();

        let mut manager = AnimationManager::new();
        manager.insert_root(screen.image(7).unwrap()).unwrap();
        manager
            .feed(
                &animated_frame(7, [9, 9, 9, 255]),
                &ImageLimits::default(),
                0,
            )
            .unwrap();
        let geometry = GridGeometry::new(4, 3, CellMetrics::default());
        let mut instances = Vec::new();
        let mut uploads = Vec::new();
        collect_protocol_images_with_animations(
            &screen,
            Some(&manager),
            &geometry,
            40.0,
            60.0,
            ImageFilter::Nearest,
            &mut instances,
            &mut uploads,
        );
        let z: Vec<i32> = instances.iter().map(|i| i.z_index).collect();
        assert_eq!(z, vec![-1, 3]);
    }
}
