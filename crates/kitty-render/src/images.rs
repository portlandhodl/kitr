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
//! Parent/child placements, unicode placeholder expansion, animations and
//! indexed-colour images without RGBA data are not turned into quads; they are
//! skipped (see the crate-level documentation).

use std::fmt;
use std::path::Path;
use std::sync::Arc;

use kitty_graphics::{decode_png, Image, MAX_IMAGE_DIMENSION};
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
    pub data: ImageData,
    pub filter: ImageFilter,
    pub wrap: ImageWrap,
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
            fingerprint,
        }
    }

    #[inline]
    pub fn is_valid(&self) -> bool {
        self.width != 0
            && self.height != 0
            && self.data.len() >= self.width as usize * self.height as usize * 4
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

/// Expand a protocol image's placement into an instance and upload.
fn protocol_placement(
    placement: &kitty_screen::ImagePlacement,
    image: &Image,
    geometry: &GridGeometry,
    viewport_width: f32,
    viewport_height: f32,
    filter: ImageFilter,
) -> Option<(ImageInstance, ImageUpload)> {
    let rgba = image.rgba.as_ref()?;
    if image.width == 0 || image.height == 0 {
        return None;
    }
    let p = &placement.placement;
    let cell_w = geometry.cell_width;
    let cell_h = geometry.cell_height;
    let iw = image.width as f32;
    let ih = image.height as f32;

    let explicit_width = p.width > 0;
    let explicit_height = p.height > 0;
    let mut width = if explicit_width {
        p.width as f32
    } else if p.num_cols > 0 {
        p.num_cols as f32 * cell_w
    } else {
        iw
    };
    let mut height = if explicit_height {
        p.height as f32
    } else if p.num_rows > 0 {
        p.num_rows as f32 * cell_h
    } else {
        ih
    };

    // If only one dimension was constrained, preserve the aspect ratio.
    if !explicit_width && p.num_cols == 0 && (explicit_height || p.num_rows > 0) {
        width = height * iw / ih;
    } else if !explicit_height && p.num_rows == 0 && (explicit_width || p.num_cols > 0) {
        height = width * ih / iw;
    }
    if width <= 0.0 || height <= 0.0 {
        return None;
    }

    let rect = Rect::new(
        geometry.origin_x
            + placement.anchor_x as f32 * cell_w
            + p.x_offset as f32
            + p.cell_x_offset as f32,
        geometry.origin_y
            + placement.anchor_y as f32 * cell_h
            + p.y_offset as f32
            + p.cell_y_offset as f32,
        width,
        height,
    );
    let uv = UvRect {
        u0: 0.0,
        v0: 0.0,
        u1: 1.0,
        v1: 1.0,
    };
    let (rect, uv) = clip_rect_uv(rect, uv, viewport_width, viewport_height)?;
    let key = ImageKey::Protocol(placement.image_id);
    let upload = ImageUpload::new(
        key,
        image.width,
        image.height,
        Arc::new(rgba.clone()),
        filter,
        ImageWrap::Clamp,
    );
    let instance = ImageInstance::new(key, rect)
        .with_uv(uv)
        .with_z_index(p.z_index);
    Some((instance, upload))
}

/// Collect visible protocol placements from `screen`.
///
/// Placements are sorted by `(z-index, image id, placement id)`, matching
/// kitty's draw order. Duplicate textures for the same image are uploaded only
/// once per frame.
pub fn collect_protocol_images(
    screen: &Screen,
    geometry: &GridGeometry,
    viewport_width: f32,
    viewport_height: f32,
    filter: ImageFilter,
    instances: &mut Vec<ImageInstance>,
    uploads: &mut Vec<ImageUpload>,
) {
    let mut seen_uploads: Vec<ImageKey> = Vec::new();
    let mut collected: Vec<(i32, u32, u32, ImageInstance, Option<ImageUpload>)> = Vec::new();

    for placement in screen.placements() {
        let Some(image) = screen.image(placement.image_id) else {
            continue;
        };
        let Some((instance, upload)) = protocol_placement(
            placement,
            image,
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
            instance,
            upload,
        ));
    }

    collected.sort_by_key(|(z, image_id, placement_id, _, _)| (*z, *image_id, *placement_id));
    for (_, _, _, instance, upload) in collected {
        instances.push(instance);
        if let Some(upload) = upload {
            uploads.push(upload);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
