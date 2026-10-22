// SPDX-License-Identifier: GPL-3.0-or-later
//! Image loading pipeline for the kitty graphics protocol.
//!
//! This is the transport-independent portion of `kitty/graphics.c`: it takes a
//! parsed [`GraphicsCommand`], decodes the payload (direct data or a regular
//! file), optionally inflates zlib data, turns raw RGB/RGBA/indexed data or PNG
//! data into an [`Image`], validates dimensions and data sizes, and records the
//! placement attributes from the command.
//!
//! Nothing here talks to a Screen, GPU or terminal. A caller that wants to
//! display images implements [`GraphicsSink`] (or stores the returned images
//! itself). This is deliberate: Screen integration is a later phase.
//!
//! Deviations from kitty are documented on [`ImageAssembler`], [`read_image_file`]
//! and in the crate-level documentation.
//!
//! Copyright (C) 2017-2026 Kovid Goyal <kovid at kovidgoyal.net>

use std::ffi::c_ulong;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crate::command::{GraphicsCommand, ParseError, TransmissionType};
use crate::png::{decode_png, PngError};
use crate::tempfile::{read_temp_image_file, TempFilePolicy};

/// Kitty's `MAX_IMAGE_DIMENSION`.
pub const MAX_IMAGE_DIMENSION: u32 = 10000;
/// Kitty's `MAX_DATA_SZ` (`4 * 100000000`).
pub const MAX_DATA_SIZE: usize = 400_000_000;

/// The `f=` pixel format.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImageFormat {
    /// `f=8`: one byte per pixel, indexed colour (opaque).
    Indexed,
    /// `f=16`: two bytes per pixel, indexed colour with alpha.
    IndexedAlpha,
    /// `f=24`: raw RGB, three bytes per pixel.
    Rgb,
    /// `f=32`: raw RGBA, four bytes per pixel.
    Rgba,
    /// `f=100`: PNG data.
    Png,
    /// Any other value. The parser accepts it; semantic validation rejects it.
    Other(u32),
}

impl ImageFormat {
    pub fn from_u32(value: u32) -> Self {
        match value {
            8 => Self::Indexed,
            16 => Self::IndexedAlpha,
            24 => Self::Rgb,
            32 => Self::Rgba,
            100 => Self::Png,
            other => Self::Other(other),
        }
    }

    pub const fn as_u32(self) -> u32 {
        match self {
            Self::Indexed => 8,
            Self::IndexedAlpha => 16,
            Self::Rgb => 24,
            Self::Rgba => 32,
            Self::Png => 100,
            Self::Other(other) => other,
        }
    }

    /// Bytes per stored pixel for the raw formats, `None` for PNG.
    pub const fn bytes_per_pixel(self) -> Option<usize> {
        match self {
            Self::Indexed => Some(1),
            Self::IndexedAlpha => Some(2),
            Self::Rgb => Some(3),
            Self::Rgba => Some(4),
            Self::Png | Self::Other(_) => None,
        }
    }

    pub const fn is_png(self) -> bool {
        matches!(self, Self::Png)
    }

    pub const fn is_indexed(self) -> bool {
        matches!(self, Self::Indexed | Self::IndexedAlpha)
    }

    pub const fn is_opaque(self) -> bool {
        matches!(self, Self::Indexed | Self::Rgb)
    }
}

/// Placement/reference attributes attached to a loaded image.
///
/// These correspond to the placement fields in `GraphicsCommand` and kitty's
/// `ImageRef`/`ImageRenderData`. They are intentionally transport-neutral.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Placement {
    pub image_id: u32,
    pub image_number: u32,
    pub placement_id: u32,
    /// Destination x pixel offset (`x=`).
    pub x_offset: u32,
    /// Destination y pixel offset (`y=`).
    pub y_offset: u32,
    /// Destination width in pixels (`w=`), zero means the natural width.
    pub width: u32,
    /// Destination height in pixels (`h=`), zero means the natural height.
    pub height: u32,
    /// Number of cells to occupy (`c=`).
    pub num_cols: u32,
    /// Number of lines to occupy (`r=`).
    pub num_rows: u32,
    /// Pixel offset inside the starting cell (`X=`).
    pub cell_x_offset: u32,
    /// Pixel offset inside the starting cell (`Y=`).
    pub cell_y_offset: u32,
    pub z_index: i32,
    /// `C=`: move the cursor after placing (non-zero), or `compose_mode` for `a=c`.
    pub cursor_movement: u32,
    pub unicode_placement: bool,
    pub parent_id: u32,
    pub parent_placement_id: u32,
    pub offset_from_parent_x: i32,
    pub offset_from_parent_y: i32,
    pub usage_hints: u32,
}

impl Placement {
    pub fn from_command(command: &GraphicsCommand) -> Self {
        Self {
            image_id: command.id,
            image_number: command.image_number,
            placement_id: command.placement_id,
            x_offset: command.x_offset,
            y_offset: command.y_offset,
            width: command.width,
            height: command.height,
            num_cols: command.num_cells,
            num_rows: command.num_lines,
            cell_x_offset: command.cell_x_offset,
            cell_y_offset: command.cell_y_offset,
            z_index: command.z_index,
            cursor_movement: command.cursor_movement,
            unicode_placement: command.unicode_placement,
            parent_id: command.parent_id,
            parent_placement_id: command.parent_placement_id,
            offset_from_parent_x: command.offset_from_parent_x,
            offset_from_parent_y: command.offset_from_parent_y,
            usage_hints: command.usage_hints,
        }
    }

    pub fn is_transient(&self) -> bool {
        self.usage_hints & crate::command::GRAPHICS_USAGE_HINT_TRANSIENT != 0
    }
}

/// A decoded image plus its placement metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Image {
    /// Client image id (`i=`). Zero means the loader should assign one.
    pub id: u32,
    /// Client image number (`I=`), used for animation/image lookup.
    pub number: u32,
    pub width: u32,
    pub height: u32,
    /// Format the image was transmitted in.
    pub format: ImageFormat,
    /// Decoded pixels in the transmitted representation: RGB for `f=24`, RGBA
    /// for `f=32`, indexed samples for `f=8/16`, and RGBA for PNG.
    pub raw: Vec<u8>,
    /// Always RGBA8 for RGB/RGBA/PNG; `None` for indexed images. Expand those
    /// with [`IndexedPalette::expand_image`](crate::palette::IndexedPalette::expand_image).
    pub rgba: Option<Vec<u8>>,
    pub is_opaque: bool,
    pub is_4byte_aligned: bool,
    pub placement: Placement,
    pub transmission: TransmissionType,
    pub transient: bool,
}

impl Image {
    /// Bytes suitable for uploading as RGBA/RGB data, if available.
    pub fn display_data(&self) -> &[u8] {
        self.rgba.as_deref().unwrap_or(&self.raw)
    }
}

/// Limits applied while loading an image.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ImageLimits {
    pub max_dimension: u32,
    pub max_data_size: usize,
}

impl Default for ImageLimits {
    fn default() -> Self {
        Self {
            max_dimension: MAX_IMAGE_DIMENSION,
            max_data_size: MAX_DATA_SIZE,
        }
    }
}

/// Errors from the image pipeline. Codes follow kitty's graphics responses.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GraphicsError {
    /// The command cannot be handled by the loader (`a=p`, `a=d`, ...).
    NotAnImageCommand,
    /// `a=q` requires an image id.
    QueryWithoutImageId,
    ImageTooLarge {
        width: u32,
        height: u32,
        max_dimension: u32,
    },
    ZeroDimensions,
    UnknownFormat(u32),
    DimensionMismatch {
        width: u32,
        height: u32,
        expected: usize,
        actual: usize,
    },
    InsufficientData {
        expected: usize,
        actual: usize,
    },
    DataTooLarge {
        limit: usize,
    },
    Png(PngError),
    Compression(String),
    UnsupportedTransmission(TransmissionType),
    BadFile(String),
    Io(String),
    NoPendingTransfer,
    Parse(ParseError),
    /// Animation/composition protocol error with a kitty response code.
    Animation {
        code: &'static str,
        message: String,
    },
}

impl GraphicsError {
    /// Kitty-compatible response code.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::NotAnImageCommand | Self::QueryWithoutImageId => "EINVAL",
            Self::ImageTooLarge { .. } | Self::ZeroDimensions | Self::UnknownFormat(_) => "EINVAL",
            Self::DimensionMismatch { .. } => "EINVAL",
            Self::InsufficientData { .. } => "ENODATA",
            Self::DataTooLarge { .. } => "EFBIG",
            Self::Png(e) => e.code(),
            Self::Compression(_) => "EINVAL",
            Self::UnsupportedTransmission(_) => "EINVAL",
            Self::BadFile(_) | Self::Io(_) => "EBADF",
            Self::NoPendingTransfer => "EILSEQ",
            Self::Parse(_) => "EINVAL",
            Self::Animation { code, .. } => code,
        }
    }
}

impl core::fmt::Display for GraphicsError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NotAnImageCommand => f.write_str("not an image loading command"),
            Self::QueryWithoutImageId => {
                f.write_str("Query graphics command without image id")
            }
            Self::ImageTooLarge {
                width,
                height,
                max_dimension,
            } => write!(
                f,
                "Image too large, width or height greater than {max_dimension} (got {width}x{height})"
            ),
            Self::ZeroDimensions => f.write_str("Zero width/height not allowed"),
            Self::UnknownFormat(format) => write!(f, "Unknown image format: {format}"),
            Self::DimensionMismatch {
                width,
                height,
                expected,
                actual,
            } => write!(
                f,
                "Image dimensions: {width}x{height} do not match data size: {actual}, expected size: {expected}"
            ),
            Self::InsufficientData { expected, actual } => {
                write!(f, "Insufficient image data: {actual} < {expected}")
            }
            Self::DataTooLarge { limit } => write!(f, "Too much data (limit: {limit} bytes)"),
            Self::Png(e) => write!(f, "{e}"),
            Self::Compression(msg) => write!(f, "Failed to inflate image data: {msg}"),
            Self::UnsupportedTransmission(t) => write!(
                f,
                "Unsupported image transmission type: {}",
                t.as_byte() as char
            ),
            Self::BadFile(msg) => write!(f, "Failed to read image file: {msg}"),
            Self::Io(msg) => write!(f, "Failed to read image file: {msg}"),
            Self::NoPendingTransfer => {
                f.write_str("More payload loading refers to non-existent image")
            }
            Self::Parse(e) => write!(f, "{e}"),
            Self::Animation { message, .. } => f.write_str(message),
        }
    }
}

impl std::error::Error for GraphicsError {}

impl From<PngError> for GraphicsError {
    fn from(value: PngError) -> Self {
        Self::Png(value)
    }
}

/// Callbacks for the eventual Screen/terminal integration.
///
/// All methods have no-op defaults, so a caller can implement only the parts it
/// needs. The loader calls these methods through [`ImagePipeline`].
pub trait GraphicsSink {
    /// A complete image was transmitted (`a=t` or a completed chunked load).
    fn on_image(
        &mut self,
        _command: &GraphicsCommand,
        _image: &Image,
    ) -> Result<(), GraphicsError> {
        Ok(())
    }

    /// An image should be displayed (`a=T` or `a=p`).
    fn on_display(
        &mut self,
        _command: &GraphicsCommand,
        _placement: &Placement,
    ) -> Result<(), GraphicsError> {
        Ok(())
    }

    /// A delete command was received (`a=d`).
    fn on_delete(&mut self, _command: &GraphicsCommand) -> Result<(), GraphicsError> {
        Ok(())
    }

    /// A query was received (`a=q`). The image is `None` if loading failed.
    fn on_query(
        &mut self,
        _command: &GraphicsCommand,
        _image: Option<&Image>,
    ) -> Result<(), GraphicsError> {
        Ok(())
    }

    /// A command the image loader does not implement (`a=a`, `a=f`, `a=c`, ...).
    fn on_unsupported(&mut self, _command: &GraphicsCommand) -> Result<(), GraphicsError> {
        Ok(())
    }
}

/// A completed image load.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoadedImage {
    pub image: Image,
    pub query: bool,
    pub display: bool,
}

/// Result of feeding a command to [`ImageAssembler`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LoadOutcome {
    /// More chunks are required.
    ChunkAccepted,
    /// A complete image was produced.
    Loaded(LoadedImage),
    /// Not an image-loading command; the caller should hand it to its sink.
    Deferred(GraphicsCommand),
}

#[derive(Clone, Debug)]
struct PendingTransfer {
    start: GraphicsCommand,
    payload: Vec<u8>,
}

/// Accumulates chunked transmissions (`m=1`) and decodes complete images.
///
/// Only direct (and regular-file/temp-file) transmission plus `a=t`, `a=T` and
/// `a=q` are handled. The pixel formats understood are `f=8`, `f=16`, `f=24`,
/// `f=32` and `f=100`, with optional zlib compression (`o=z`).
///
/// Unix shared-memory transmission (`t=s`) is parsed but not loaded; kitty's
/// boss/path policy and shared memory lifecycle belong to terminal integration.
#[derive(Debug)]
pub struct ImageAssembler {
    limits: ImageLimits,
    temp_policy: TempFilePolicy,
    pending: Option<PendingTransfer>,
}

impl Default for ImageAssembler {
    fn default() -> Self {
        Self::new(ImageLimits::default())
    }
}

impl ImageAssembler {
    pub fn new(limits: ImageLimits) -> Self {
        Self {
            limits,
            temp_policy: TempFilePolicy::default(),
            pending: None,
        }
    }

    pub fn limits(&self) -> ImageLimits {
        self.limits
    }

    pub fn set_limits(&mut self, limits: ImageLimits) {
        self.limits = limits;
    }

    /// Policy used for temporary-file (`t=t`) transmissions.
    pub fn temp_policy(&self) -> &TempFilePolicy {
        &self.temp_policy
    }

    pub fn set_temp_policy(&mut self, policy: TempFilePolicy) {
        self.temp_policy = policy;
    }

    pub fn has_pending_transfer(&self) -> bool {
        self.pending.is_some()
    }

    /// Forget a partially transferred image (for example after `a=d`).
    pub fn clear_pending(&mut self) {
        self.pending = None;
    }

    /// Feed one parsed command.
    pub fn feed(&mut self, command: &GraphicsCommand) -> Result<LoadOutcome, GraphicsError> {
        if let Some(mut pending) = self.pending.take() {
            if command.action == Some(crate::command::Action::Delete) {
                drop(pending);
                return Ok(LoadOutcome::Deferred(command.clone()));
            }
            pending.payload.extend_from_slice(&command.payload);
            if pending.payload.len() > self.limits.max_data_size {
                return Err(GraphicsError::DataTooLarge {
                    limit: self.limits.max_data_size,
                });
            }
            if command.more != 0 {
                self.pending = Some(pending);
                return Ok(LoadOutcome::ChunkAccepted);
            }
            return self.finish_pending(pending);
        }

        let action = command.action;
        let is_load = action.is_none()
            || matches!(
                action,
                Some(crate::command::Action::Transmit)
                    | Some(crate::command::Action::TransmitAndDisplay)
                    | Some(crate::command::Action::Query)
            );
        if !is_load {
            return Ok(LoadOutcome::Deferred(command.clone()));
        }

        if command.more != 0 {
            if command.payload.len() > self.limits.max_data_size {
                return Err(GraphicsError::DataTooLarge {
                    limit: self.limits.max_data_size,
                });
            }
            self.pending = Some(PendingTransfer {
                start: command.clone(),
                payload: command.payload.clone(),
            });
            return Ok(LoadOutcome::ChunkAccepted);
        }

        let image = decode_image_with_temp_policy(
            command,
            &command.payload,
            &self.limits,
            &self.temp_policy,
        )?;
        let query = action == Some(crate::command::Action::Query);
        if query && command.id == 0 {
            return Err(GraphicsError::QueryWithoutImageId);
        }
        Ok(LoadOutcome::Loaded(LoadedImage {
            image,
            query,
            display: action == Some(crate::command::Action::TransmitAndDisplay),
        }))
    }

    fn finish_pending(&self, pending: PendingTransfer) -> Result<LoadOutcome, GraphicsError> {
        let mut start = pending.start;
        let image = decode_image_with_temp_policy(
            &start,
            &pending.payload,
            &self.limits,
            &self.temp_policy,
        )?;
        let query = start.action == Some(crate::command::Action::Query);
        if query && start.id == 0 {
            return Err(GraphicsError::QueryWithoutImageId);
        }
        let display = start.action == Some(crate::command::Action::TransmitAndDisplay);
        // The response should use the id from the start command.
        start.payload = pending.payload;
        Ok(LoadOutcome::Loaded(LoadedImage {
            image,
            query,
            display,
        }))
    }
}

/// Load and validate a complete image from one command.
///
/// `payload` should be the base64-decoded command payload for direct
/// transmission, or the filename for file transmission.
pub fn decode_image(
    command: &GraphicsCommand,
    payload: &[u8],
    limits: &ImageLimits,
) -> Result<Image, GraphicsError> {
    decode_image_with_temp_policy(command, payload, limits, &TempFilePolicy::default())
}

/// Like [`decode_image`] but with an explicit temporary-file policy for
/// `t=t` transmissions.
pub fn decode_image_with_temp_policy(
    command: &GraphicsCommand,
    payload: &[u8],
    limits: &ImageLimits,
    temp_policy: &TempFilePolicy,
) -> Result<Image, GraphicsError> {
    let format_value = if command.format == 0 {
        32
    } else {
        command.format
    };
    let format = ImageFormat::from_u32(format_value);
    let transmission = command
        .transmission_type
        .unwrap_or(TransmissionType::Direct);

    let expected_pixels = expected_raw_size(format, command.data_width, command.data_height)?;
    let mut data = match transmission {
        TransmissionType::Direct => {
            if payload.len() > limits.max_data_size {
                return Err(GraphicsError::DataTooLarge {
                    limit: limits.max_data_size,
                });
            }
            payload.to_vec()
        }
        TransmissionType::File => {
            let max_to_read = if command.compressed.is_some() || format.is_png() {
                limits.max_data_size
            } else {
                expected_pixels.unwrap_or(limits.max_data_size)
            };
            read_image_file(
                path_from_payload(payload)?,
                command.data_offset,
                command.data_sz,
                max_to_read,
            )?
        }
        TransmissionType::TempFile => {
            let max_to_read = if command.compressed.is_some() || format.is_png() {
                limits.max_data_size
            } else {
                expected_pixels.unwrap_or(limits.max_data_size)
            };
            read_temp_image_file(
                path_from_payload(payload)?,
                command.data_offset,
                command.data_sz,
                max_to_read,
                temp_policy,
            )?
        }
        TransmissionType::SharedMemory => {
            let max_to_read = if command.compressed.is_some() || format.is_png() {
                limits.max_data_size
            } else {
                expected_pixels.unwrap_or(limits.max_data_size)
            };
            crate::shm::read_shared_memory(
                payload,
                command.data_offset,
                command.data_sz,
                max_to_read,
            )?
        }
    };

    if command.compressed.is_some() {
        data = inflate_zlib(&data, expected_pixels, limits.max_data_size)?;
    }
    if data.len() > limits.max_data_size {
        return Err(GraphicsError::DataTooLarge {
            limit: limits.max_data_size,
        });
    }

    let (width, height, raw, rgba, is_opaque, is_4byte_aligned) = match format {
        ImageFormat::Png => {
            if command.data_sz as usize > limits.max_data_size {
                return Err(GraphicsError::DataTooLarge {
                    limit: limits.max_data_size,
                });
            }
            let decoded = decode_png(&data, limits.max_dimension)?;
            if limits.max_dimension != 0
                && (decoded.width > limits.max_dimension || decoded.height > limits.max_dimension)
            {
                return Err(GraphicsError::ImageTooLarge {
                    width: decoded.width,
                    height: decoded.height,
                    max_dimension: limits.max_dimension,
                });
            }
            let rgba = decoded.rgba;
            (
                decoded.width,
                decoded.height,
                rgba.clone(),
                Some(rgba),
                false,
                true,
            )
        }
        ImageFormat::Rgb | ImageFormat::Rgba | ImageFormat::Indexed | ImageFormat::IndexedAlpha => {
            let (width, height) = (command.data_width, command.data_height);
            if width == 0 || height == 0 || expected_pixels == Some(0) {
                return Err(GraphicsError::ZeroDimensions);
            }
            if limits.max_dimension != 0
                && (width > limits.max_dimension || height > limits.max_dimension)
            {
                return Err(GraphicsError::ImageTooLarge {
                    width,
                    height,
                    max_dimension: limits.max_dimension,
                });
            }
            let expected = expected_pixels.expect("raw format has a pixel size");
            if command.data_sz != 0 && command.data_sz as usize != expected {
                return Err(GraphicsError::DimensionMismatch {
                    width,
                    height,
                    expected,
                    actual: command.data_sz as usize,
                });
            }
            if data.len() < expected {
                if matches!(
                    transmission,
                    TransmissionType::File
                        | TransmissionType::TempFile
                        | TransmissionType::SharedMemory
                ) {
                    return Err(GraphicsError::BadFile("Failed to read image file".into()));
                }
                return Err(GraphicsError::InsufficientData {
                    expected,
                    actual: data.len(),
                });
            }
            data.truncate(expected);
            let rgba = match format {
                ImageFormat::Rgb => Some(expand_rgb_to_rgba(&data)),
                ImageFormat::Rgba => Some(data.clone()),
                _ => None,
            };
            let aligned = match format {
                ImageFormat::Rgb => width % 4 == 0,
                ImageFormat::Rgba => true,
                ImageFormat::Indexed => width % 4 == 0,
                ImageFormat::IndexedAlpha => (width as usize * 2).is_multiple_of(4),
                _ => unreachable!(),
            };
            (width, height, data, rgba, format.is_opaque(), aligned)
        }
        ImageFormat::Other(other) => return Err(GraphicsError::UnknownFormat(other)),
    };

    Ok(Image {
        id: command.id,
        number: command.image_number,
        width,
        height,
        format,
        raw,
        rgba,
        is_opaque,
        is_4byte_aligned,
        placement: Placement::from_command(command),
        transmission,
        transient: command.is_transient(),
    })
}

fn expected_raw_size(
    format: ImageFormat,
    width: u32,
    height: u32,
) -> Result<Option<usize>, GraphicsError> {
    let Some(bpp) = format.bytes_per_pixel() else {
        return Ok(None);
    };
    if width == 0 || height == 0 {
        return Ok(Some(0));
    }
    usize::try_from(width)
        .ok()
        .and_then(|w| usize::try_from(height).ok().and_then(|h| w.checked_mul(h)))
        .and_then(|n| n.checked_mul(bpp))
        .map(Some)
        .ok_or(GraphicsError::DataTooLarge {
            limit: MAX_DATA_SIZE,
        })
}

fn expand_rgb_to_rgba(rgb: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgb.len() / 3 * 4);
    for px in rgb.chunks(3) {
        out.extend_from_slice(px);
        out.push(255);
    }
    out
}

fn path_from_payload(payload: &[u8]) -> Result<&Path, GraphicsError> {
    if payload.is_empty() {
        return Err(GraphicsError::BadFile("empty file name".into()));
    }
    if payload.contains(&0) {
        return Err(GraphicsError::BadFile("file name contains NUL".into()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Ok(Path::new(std::ffi::OsStr::from_bytes(payload)))
    }
    #[cfg(not(unix))]
    {
        let s = std::str::from_utf8(payload)
            .map_err(|_| GraphicsError::BadFile("file name is not UTF-8".into()))?;
        Ok(Path::new(s))
    }
}

/// Read a regular file window for the graphics protocol.
///
/// Only regular files are accepted, matching the protocol requirement (reading
/// a FIFO/device could block or have side effects). A window of `size` bytes is
/// read starting at `offset`; `size == 0` means "read to EOF". The amount read
/// is capped at `max_to_read`. Short reads are reported as `EBADF`.
///
/// Unlike kitty, this helper does **not** ask a boss/window-manager about path
/// access. Temporary-file (`t=t`) transmissions go through
/// [`read_temp_image_file`](crate::tempfile::read_temp_image_file), which adds
/// the temp-directory/symlink policy and immediate cleanup.
pub fn read_image_file(
    path: &Path,
    offset: u32,
    size: u32,
    max_to_read: usize,
) -> Result<Vec<u8>, GraphicsError> {
    let mut file =
        File::open(path).map_err(|e| GraphicsError::BadFile(format!("{}: {e}", path.display())))?;
    let metadata = file
        .metadata()
        .map_err(|e| GraphicsError::BadFile(format!("{}: {e}", path.display())))?;
    if !metadata.is_file() {
        return Err(GraphicsError::BadFile(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    let file_len = metadata.len();
    let offset_u64 = u64::from(offset);
    if offset_u64 > file_len {
        return Err(GraphicsError::BadFile(format!(
            "offset {offset} is past the end of {}",
            path.display()
        )));
    }
    let available = file_len - offset_u64;
    let requested = if size == 0 {
        available
    } else {
        u64::from(size)
    };
    let requested = requested.min(max_to_read as u64) as usize;
    file.seek(SeekFrom::Start(offset_u64))
        .map_err(|e| GraphicsError::BadFile(format!("{}: {e}", path.display())))?;
    let mut out = Vec::with_capacity(requested);
    file.take(requested as u64)
        .read_to_end(&mut out)
        .map_err(|e| GraphicsError::BadFile(format!("{}: {e}", path.display())))?;
    if out.len() != requested {
        return Err(GraphicsError::BadFile(format!(
            "{} is truncated: read {} bytes, wanted {requested}",
            path.display(),
            out.len()
        )));
    }
    Ok(out)
}

#[link(name = "z")]
extern "C" {
    fn uncompress(
        dest: *mut u8,
        dest_len: *mut c_ulong,
        source: *const u8,
        source_len: c_ulong,
    ) -> i32;
}

const Z_OK: i32 = 0;
const Z_BUF_ERROR: i32 = -5;

fn inflate_zlib(
    data: &[u8],
    expected: Option<usize>,
    max_output: usize,
) -> Result<Vec<u8>, GraphicsError> {
    if data.len() > c_ulong::MAX as usize {
        return Err(GraphicsError::Compression("input too large".into()));
    }
    let mut capacity = match expected {
        Some(0) => return Err(GraphicsError::ZeroDimensions),
        Some(n) => n,
        None => {
            let estimate = data.len().saturating_mul(4).max(1024);
            estimate.min(max_output.max(1))
        }
    };
    if capacity > max_output {
        return Err(GraphicsError::DataTooLarge { limit: max_output });
    }
    loop {
        let mut out = vec![0u8; capacity];
        let mut out_len = capacity as c_ulong;
        let ret = unsafe {
            uncompress(
                out.as_mut_ptr(),
                &mut out_len,
                data.as_ptr(),
                data.len() as c_ulong,
            )
        };
        if ret == Z_OK {
            let produced = out_len as usize;
            out.truncate(produced);
            if let Some(expected) = expected {
                if produced != expected {
                    return Err(GraphicsError::InsufficientData {
                        expected,
                        actual: produced,
                    });
                }
            }
            return Ok(out);
        }
        if ret == Z_BUF_ERROR && expected.is_none() {
            if capacity >= max_output {
                return Err(GraphicsError::DataTooLarge { limit: max_output });
            }
            capacity = capacity.saturating_mul(2).min(max_output);
            continue;
        }
        return Err(GraphicsError::Compression(format!("zlib error {ret}")));
    }
}

/// Convenience wrapper that parses and feeds one escape sequence.
pub fn handle_escape<S: GraphicsSink>(
    input: &[u8],
    assembler: &mut ImageAssembler,
    sink: &mut S,
) -> Result<LoadOutcome, GraphicsError> {
    let command = crate::command::parse_escape(input).map_err(GraphicsError::Parse)?;
    let outcome = assembler.feed(&command)?;
    match &outcome {
        LoadOutcome::Loaded(loaded) if loaded.query => {
            sink.on_query(&command, Some(&loaded.image))?;
        }
        LoadOutcome::Loaded(loaded) => {
            sink.on_image(&command, &loaded.image)?;
            if loaded.display {
                sink.on_display(&command, &loaded.image.placement)?;
            }
        }
        LoadOutcome::Deferred(_) => match command.action {
            Some(crate::command::Action::Delete) => sink.on_delete(&command)?,
            _ => sink.on_unsupported(&command)?,
        },
        LoadOutcome::ChunkAccepted => {}
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{parse_raw, Action};
    use std::sync::atomic::{AtomicU32, Ordering};

    fn cmd(raw: &[u8]) -> GraphicsCommand {
        parse_raw(raw).unwrap()
    }

    fn rgba_cmd(w: u32, h: u32, id: u32) -> GraphicsCommand {
        cmd(format!("a=t,f=32,i={id},s={w},v={h}").as_bytes())
    }

    #[test]
    fn loads_raw_rgba_and_expands_nothing() {
        let mut c = rgba_cmd(2, 2, 7);
        c.payload = (0..16).collect();
        let img = decode_image(&c, &c.payload, &ImageLimits::default()).unwrap();
        assert_eq!((img.width, img.height), (2, 2));
        assert_eq!(img.raw, (0..16).collect::<Vec<u8>>());
        assert_eq!(img.rgba.as_deref(), Some(img.raw.as_slice()));
        assert!(!img.is_opaque);
        assert!(img.is_4byte_aligned);
        assert_eq!(img.id, 7);
    }

    #[test]
    fn loads_raw_rgb_and_fills_alpha() {
        let mut c = cmd(b"a=t,f=24,s=2,v=1");
        c.payload = vec![1, 2, 3, 4, 5, 6];
        let img = decode_image(&c, &c.payload, &ImageLimits::default()).unwrap();
        assert_eq!(img.raw, vec![1, 2, 3, 4, 5, 6]);
        assert_eq!(
            img.rgba.as_deref(),
            Some([1, 2, 3, 255, 4, 5, 6, 255].as_slice())
        );
        assert!(img.is_opaque);
        assert!(!img.is_4byte_aligned);
        assert_eq!(img.display_data().len(), 8);
    }

    #[test]
    fn loads_png_and_uses_png_dimensions() {
        let pixels: Vec<u8> = (0..2 * 3 * 4).map(|i| (i * 3) as u8).collect();
        let png = crate::png::encode_png_rgba(2, 3, &pixels).unwrap();
        let mut c = cmd(b"a=T,f=100,i=9");
        // A wrong s=/v= is ignored for PNG, as in kitty.
        c.data_width = 99;
        c.data_height = 99;
        c.payload = png;
        let img = decode_image(&c, &c.payload, &ImageLimits::default()).unwrap();
        assert_eq!((img.width, img.height), (2, 3));
        assert_eq!(img.raw, pixels);
        assert_eq!(img.rgba.as_deref(), Some(pixels.as_slice()));
        assert_eq!(img.format, ImageFormat::Png);
    }

    #[test]
    fn corrupt_png_is_ebadpng() {
        let mut c = cmd(b"a=t,f=100,i=1");
        c.payload = b"not a png".to_vec();
        let err = decode_image(&c, &c.payload, &ImageLimits::default()).unwrap_err();
        assert_eq!(err.code(), "EBADPNG");
        assert!(err.to_string().contains("EBADPNG"));
    }

    #[test]
    fn validates_sizes_dimensions_and_formats() {
        let mut c = cmd(b"a=t,f=24,s=2,v=2");
        c.payload = vec![0; 8];
        let err = decode_image(&c, &c.payload, &ImageLimits::default()).unwrap_err();
        assert_eq!(
            err,
            GraphicsError::InsufficientData {
                expected: 12,
                actual: 8
            }
        );
        assert_eq!(err.code(), "ENODATA");

        let mut c = cmd(b"a=t,f=24,s=2,v=2,S=13");
        c.payload = vec![0; 13];
        assert_eq!(
            decode_image(&c, &c.payload, &ImageLimits::default()).unwrap_err(),
            GraphicsError::DimensionMismatch {
                width: 2,
                height: 2,
                expected: 12,
                actual: 13
            }
        );

        let mut c = cmd(b"a=t,f=24,s=0,v=1");
        c.payload.clear();
        assert_eq!(
            decode_image(&c, &c.payload, &ImageLimits::default()).unwrap_err(),
            GraphicsError::ZeroDimensions
        );

        let mut c = cmd(b"a=t,f=99,s=1,v=1");
        c.payload = vec![0; 4];
        assert_eq!(
            decode_image(&c, &c.payload, &ImageLimits::default()).unwrap_err(),
            GraphicsError::UnknownFormat(99)
        );
    }

    #[test]
    fn enforces_max_dimension_and_data_limits() {
        let limits = ImageLimits {
            max_dimension: 2,
            max_data_size: 1024,
        };
        let mut c = cmd(b"a=t,f=32,s=3,v=1");
        c.payload = vec![0; 12];
        let err = decode_image(&c, &c.payload, &limits).unwrap_err();
        assert!(matches!(err, GraphicsError::ImageTooLarge { .. }));

        let limits = ImageLimits {
            max_dimension: 100,
            max_data_size: 4,
        };
        let mut c = cmd(b"a=t,f=32,s=1,v=1");
        c.payload = vec![0; 8];
        assert_eq!(
            decode_image(&c, &c.payload, &limits).unwrap_err(),
            GraphicsError::DataTooLarge { limit: 4 }
        );
    }

    #[test]
    fn indexed_formats_are_kept_raw() {
        let mut c = cmd(b"a=t,f=8,s=4,v=1");
        c.payload = vec![0, 1, 2, 3];
        let img = decode_image(&c, &c.payload, &ImageLimits::default()).unwrap();
        assert_eq!(img.format, ImageFormat::Indexed);
        assert_eq!(img.raw, vec![0, 1, 2, 3]);
        assert_eq!(img.rgba, None);
        assert!(img.is_opaque);
        assert!(img.is_4byte_aligned);

        let mut c = cmd(b"a=t,f=16,s=1,v=1");
        c.payload = vec![0, 1];
        let img = decode_image(&c, &c.payload, &ImageLimits::default()).unwrap();
        assert_eq!(img.format, ImageFormat::IndexedAlpha);
        assert!(!img.is_opaque);
    }

    #[test]
    fn chunked_direct_load_accumulates() {
        let mut a = ImageAssembler::default();
        let mut first = cmd(b"a=t,f=32,i=1,s=2,v=2,m=1");
        first.payload = b"abcd".to_vec();
        assert_eq!(a.feed(&first).unwrap(), LoadOutcome::ChunkAccepted);
        assert!(a.has_pending_transfer());
        let mut second = cmd(b"m=1");
        second.payload = b"efgh".to_vec();
        assert_eq!(a.feed(&second).unwrap(), LoadOutcome::ChunkAccepted);
        let mut last = cmd(b"m=0");
        last.payload = b"ijklmnop".to_vec();
        match a.feed(&last).unwrap() {
            LoadOutcome::Loaded(loaded) => {
                assert_eq!(loaded.image.raw, b"abcdefghijklmnop");
                assert_eq!(loaded.image.id, 1);
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(!a.has_pending_transfer());
    }

    #[test]
    fn delete_clears_partial_transfer() {
        let mut a = ImageAssembler::default();
        let mut first = cmd(b"a=t,f=32,i=1,s=2,v=2,m=1");
        first.payload = b"abcd".to_vec();
        a.feed(&first).unwrap();
        let del = cmd(b"a=d");
        assert!(matches!(a.feed(&del).unwrap(), LoadOutcome::Deferred(_)));
        assert!(!a.has_pending_transfer());
        let mut again = cmd(b"a=t,f=32,i=1,s=1,v=1,m=1");
        again.payload = b"ab".to_vec();
        a.feed(&again).unwrap();
        let mut last = cmd(b"m=0");
        last.payload = b"cd".to_vec();
        match a.feed(&last).unwrap() {
            LoadOutcome::Loaded(loaded) => assert_eq!(loaded.image.raw, b"abcd"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn query_marks_result_and_requires_id() {
        let mut a = ImageAssembler::default();
        let mut c = cmd(b"a=q,i=4,f=32,s=1,v=1");
        c.payload = vec![1, 2, 3, 4];
        match a.feed(&c).unwrap() {
            LoadOutcome::Loaded(l) => {
                assert!(l.query);
                assert!(!l.display);
                assert_eq!(l.image.id, 4);
            }
            other => panic!("unexpected {other:?}"),
        }
        let mut c = cmd(b"a=q,f=32,s=1,v=1");
        c.payload = vec![1, 2, 3, 4];
        assert_eq!(a.feed(&c).unwrap_err(), GraphicsError::QueryWithoutImageId);
    }

    #[test]
    fn transmit_and_display_sets_display_flag() {
        let mut a = ImageAssembler::default();
        let mut c = cmd(b"a=T,f=32,i=1,s=1,v=1");
        c.payload = vec![0; 4];
        match a.feed(&c).unwrap() {
            LoadOutcome::Loaded(l) => assert!(l.display),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn zlib_compressed_raw_data_is_inflated() {
        let data = vec![7u8; 256];
        let compressed = zlib_compress(&data);
        let mut c = cmd(b"a=t,f=32,o=z,s=8,v=8");
        c.payload = compressed;
        let img = decode_image(&c, &c.payload, &ImageLimits::default()).unwrap();
        assert_eq!(img.raw, data);
        assert_eq!(img.rgba.as_deref(), Some(data.as_slice()));
    }

    #[test]
    fn malformed_zlib_is_rejected() {
        let mut c = cmd(b"a=t,f=32,o=z,s=1,v=1");
        c.payload = b"not zlib data".to_vec();
        let err = decode_image(&c, &c.payload, &ImageLimits::default()).unwrap_err();
        assert_eq!(err.code(), "EINVAL");
        assert!(matches!(err, GraphicsError::Compression(_)));
    }

    #[test]
    fn loads_from_regular_file_window() {
        let path = temp_path("file-window");
        let data: Vec<u8> = (0..64).collect();
        std::fs::write(&path, &data).unwrap();
        let mut c = cmd(b"a=t,f=32,t=f,s=2,v=2,S=16,O=3");
        c.payload = path.to_string_lossy().as_bytes().to_vec();
        let img = decode_image(&c, &c.payload, &ImageLimits::default()).unwrap();
        assert_eq!(img.raw, &data[3..19]);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn file_errors_are_generic_and_do_not_leak_size() {
        let missing = temp_path("missing");
        let err = read_image_file(&missing, 0, 0, 1024).unwrap_err();
        assert_eq!(err.code(), "EBADF");

        let path = temp_path("short");
        std::fs::write(&path, [1u8, 2, 3]).unwrap();
        let err = read_image_file(&path, 0, 10, 1024).unwrap_err();
        assert_eq!(err.code(), "EBADF");
        let err = read_image_file(&path, 5, 0, 1024).unwrap_err();
        assert_eq!(err.code(), "EBADF");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn rejects_directories_and_missing_shared_memory() {
        let dir = std::env::temp_dir();
        let err = read_image_file(&dir, 0, 0, 1024).unwrap_err();
        assert_eq!(err.code(), "EBADF");

        let mut c = cmd(b"a=t,f=32,t=s,s=1,v=1");
        c.payload =
            format!("/kitty-graphics-missing-{}-{}", std::process::id(), line!()).into_bytes();
        let err = decode_image(&c, &c.payload, &ImageLimits::default()).unwrap_err();
        assert_eq!(err.code(), "EBADF");
    }

    #[test]
    fn placement_fields_come_from_command() {
        let mut c = cmd(b"a=T,f=32,i=7,I=8,p=9,s=1,v=1,x=1,y=2,w=3,h=4,c=5,r=6,X=7,Y=8,z=-9,C=1,U=1,P=10,Q=11,H=-12,V=-13,N=1");
        c.payload = vec![0; 4];
        let img = decode_image(&c, &c.payload, &ImageLimits::default()).unwrap();
        let p = &img.placement;
        assert_eq!((p.image_id, p.image_number, p.placement_id), (7, 8, 9));
        assert_eq!((p.x_offset, p.y_offset, p.width, p.height), (1, 2, 3, 4));
        assert_eq!((p.num_cols, p.num_rows), (5, 6));
        assert_eq!((p.cell_x_offset, p.cell_y_offset), (7, 8));
        assert_eq!(p.z_index, -9);
        assert!(p.unicode_placement);
        assert!(p.is_transient());
        assert!(img.transient);
    }

    #[test]
    fn non_loading_commands_are_deferred() {
        let mut a = ImageAssembler::default();
        for raw in [
            b"a=p,i=1".as_slice(),
            b"a=d".as_slice(),
            b"a=f,i=1".as_slice(),
        ] {
            let c = cmd(raw);
            assert!(matches!(a.feed(&c).unwrap(), LoadOutcome::Deferred(_)));
        }
        assert_eq!(
            cmd(b"a=p,i=1").action,
            Some(Action::Put),
            "parser must expose the put action"
        );
    }

    fn temp_path(tag: &str) -> std::path::PathBuf {
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("kitty-graphics-{tag}-{}-{n}", std::process::id()))
    }

    /// Tiny zlib compressor used only by tests (stored deflate blocks).
    fn zlib_compress(data: &[u8]) -> Vec<u8> {
        // zlib header: deflate, no preset dictionary, default compression.
        let mut out = vec![0x78, 0x01];
        let mut remaining = data;
        loop {
            let last = remaining.len() <= 65535;
            let len = remaining.len().min(65535);
            out.push(u8::from(last));
            let n = len as u16;
            out.extend_from_slice(&n.to_le_bytes());
            out.extend_from_slice(&(!n).to_le_bytes());
            out.extend_from_slice(&remaining[..len]);
            remaining = &remaining[len..];
            if last {
                break;
            }
        }
        let (mut a, mut b) = (1u32, 0u32);
        for &byte in data {
            a = (a + u32::from(byte)) % 65521;
            b = (b + a) % 65521;
        }
        out.extend_from_slice(&((b << 16) | a).to_be_bytes());
        out
    }
}
