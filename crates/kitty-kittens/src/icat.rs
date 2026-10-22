// SPDX-License-Identifier: GPL-3.0-or-later
//! Rust-native `kitten icat`.
//!
//! The encoder reads PNG data through [`kitty_graphics::decode_png`], optionally
//! rescales it with a small nearest-neighbour scaler, and emits kitty graphics
//! protocol escape sequences (`ESC _ G ... ESC \`). The default transmission is
//! the original PNG (`a=T,f=100`); `--format rgb`/`rgba` emit raw `f=24`/`f=32`
//! pixels instead.
//!
//! Payloads are base64 encoded without padding (the form kitty's own Go kitten
//! uses) and split into independently decodable chunks. Each chunk is a complete
//! graphics command with `m=1` except for the final one. A 1x1 PNG is one frame;
//! a large image or a small `chunk_size` produces several frames that
//! [`kitty_graphics::ImageAssembler`] can reassemble.
//!
//! No terminal is contacted. Placement geometry is computed from an explicit
//! `--use-window-size`, or from a deterministic fallback (`80x24` cells,
//! `800x480` pixels), so the same options produce identical bytes in tests.

use std::io::{Read, Write};
use std::path::PathBuf;

use kitty_graphics::base64::encode as encode_base64;
use kitty_graphics::png::encode_png_rgba;
use kitty_graphics::{decode_png, PngError, MAX_IMAGE_DIMENSION};

/// Default maximum payload bytes per graphics command. Kitty's Go kitten uses
/// 128 KiB of base64 characters; this port keeps the same order of magnitude
/// while making each chunk independently decodable.
pub const DEFAULT_CHUNK_SIZE: usize = 128 * 1024;

/// Fallback window geometry used when `--use-window-size` is absent.
pub const FALLBACK_WINDOW: WindowSize = WindowSize {
    cols: 80,
    rows: 24,
    width_px: 800,
    height_px: 480,
};

/// A rectangle for `--place`, expressed in cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Place {
    pub width: u32,
    pub height: u32,
    pub left: u32,
    pub top: u32,
}

impl Place {
    /// Parse `WIDTHxHEIGHT@LEFTxTOP`.
    pub fn parse(value: &str) -> Result<Self, IcatError> {
        let (area, position) = value
            .split_once('@')
            .ok_or_else(|| IcatError::InvalidOption {
                option: "--place",
                value: value.to_owned(),
            })?;
        let (width, height) = area
            .split_once('x')
            .ok_or_else(|| IcatError::InvalidOption {
                option: "--place",
                value: value.to_owned(),
            })?;
        let (left, top) = position
            .split_once('x')
            .ok_or_else(|| IcatError::InvalidOption {
                option: "--place",
                value: value.to_owned(),
            })?;
        let parse = |part: &str| {
            part.parse::<u32>().map_err(|_| IcatError::InvalidOption {
                option: "--place",
                value: value.to_owned(),
            })
        };
        let place = Self {
            width: parse(width)?,
            height: parse(height)?,
            left: parse(left)?,
            top: parse(top)?,
        };
        if place.width == 0 || place.height == 0 {
            return Err(IcatError::InvalidOption {
                option: "--place",
                value: value.to_owned(),
            });
        }
        Ok(place)
    }
}

/// Screen geometry used for placement and fit calculations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowSize {
    pub cols: u32,
    pub rows: u32,
    pub width_px: u32,
    pub height_px: u32,
}

impl WindowSize {
    /// Parse `COLS,ROWS,WIDTH_PX,HEIGHT_PX`.
    pub fn parse(value: &str) -> Result<Self, IcatError> {
        let parts: Vec<&str> = value.split(',').collect();
        let error = || IcatError::InvalidOption {
            option: "--use-window-size",
            value: value.to_owned(),
        };
        if parts.len() != 4 {
            return Err(error());
        }
        let mut numbers = [0u32; 4];
        for (slot, part) in numbers.iter_mut().zip(parts) {
            *slot = part.parse().map_err(|_| error())?;
            if *slot == 0 {
                return Err(error());
            }
        }
        if numbers[2] < numbers[0] || numbers[3] < numbers[1] {
            return Err(error());
        }
        Ok(Self {
            cols: numbers[0],
            rows: numbers[1],
            width_px: numbers[2],
            height_px: numbers[3],
        })
    }

    fn cell_width(self) -> u32 {
        (self.width_px / self.cols).max(1)
    }

    fn cell_height(self) -> u32 {
        (self.height_px / self.rows).max(1)
    }
}

/// How an image is scaled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FitMode {
    None,
    Width,
    Height,
    Both,
}

/// `--scale`/`--fit` selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scale {
    Fit(FitMode),
    Exact { width: u32, height: u32 },
}

impl Scale {
    fn parse(value: &str) -> Result<Self, IcatError> {
        let lower = value.trim().to_ascii_lowercase();
        Ok(match lower.as_str() {
            "none" | "no" | "off" | "neither" => Self::Fit(FitMode::None),
            "width" => Self::Fit(FitMode::Width),
            "height" => Self::Fit(FitMode::Height),
            "both" | "fit" | "auto" => Self::Fit(FitMode::Both),
            "up" | "scale-up" => Self::Fit(FitMode::Both),
            _ => {
                let (width, height) =
                    lower
                        .split_once('x')
                        .ok_or_else(|| IcatError::InvalidOption {
                            option: "--scale",
                            value: value.to_owned(),
                        })?;
                let parse = |part: &str| {
                    part.parse::<u32>()
                        .ok()
                        .filter(|number| *number > 0)
                        .ok_or_else(|| IcatError::InvalidOption {
                            option: "--scale",
                            value: value.to_owned(),
                        })
                };
                Self::Exact {
                    width: parse(width)?,
                    height: parse(height)?,
                }
            }
        })
    }
}

/// Horizontal alignment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

impl Align {
    fn parse(value: &str) -> Result<Self, IcatError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "left" => Ok(Self::Left),
            "center" | "centre" => Ok(Self::Center),
            "right" => Ok(Self::Right),
            _ => Err(IcatError::InvalidOption {
                option: "--align",
                value: value.to_owned(),
            }),
        }
    }

    fn offset_cells(self, available: u32, used: u32) -> u32 {
        let free = available.saturating_sub(used);
        match self {
            Self::Left => 0,
            Self::Center => free / 2,
            Self::Right => free,
        }
    }
}

/// Pixel format used on the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputFormat {
    Png,
    Rgb,
    Rgba,
}

impl OutputFormat {
    fn parse(value: &str) -> Result<Self, IcatError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "png" => Ok(Self::Png),
            "rgb" => Ok(Self::Rgb),
            "rgba" => Ok(Self::Rgba),
            _ => Err(IcatError::InvalidOption {
                option: "--format",
                value: value.to_owned(),
            }),
        }
    }

    fn protocol_value(self) -> u32 {
        match self {
            Self::Png => 100,
            Self::Rgb => 24,
            Self::Rgba => 32,
        }
    }
}

/// Whether `--stdin` is enabled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StdinMode {
    Auto,
    Yes,
    No,
}

impl StdinMode {
    fn parse(value: &str) -> Result<Self, IcatError> {
        match value.trim().to_ascii_lowercase().as_str() {
            "detect" | "auto" => Ok(Self::Auto),
            "yes" | "true" | "1" => Ok(Self::Yes),
            "no" | "false" | "0" => Ok(Self::No),
            _ => Err(IcatError::InvalidOption {
                option: "--stdin",
                value: value.to_owned(),
            }),
        }
    }
}

/// Options for one `icat` invocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IcatOptions {
    pub place: Option<Place>,
    pub scale: Option<Scale>,
    pub scale_up: bool,
    pub align: Align,
    pub z_index: i32,
    pub clear: bool,
    pub clear_all: bool,
    pub stdin: StdinMode,
    pub window_size: Option<WindowSize>,
    pub format: OutputFormat,
    pub image_id: u32,
    pub chunk_size: usize,
    pub quiet: bool,
    pub trailing_newline: bool,
    /// Window id inherited from `KITTY_WINDOW_ID`/`KITTY_KITTEN_WINDOW_ID`.
    pub window_id: Option<u64>,
}

impl Default for IcatOptions {
    fn default() -> Self {
        Self {
            place: None,
            scale: None,
            scale_up: false,
            align: Align::Center,
            z_index: 0,
            clear: false,
            clear_all: false,
            stdin: StdinMode::Auto,
            window_size: None,
            format: OutputFormat::Png,
            image_id: 0,
            chunk_size: DEFAULT_CHUNK_SIZE,
            quiet: true,
            trailing_newline: true,
            window_id: None,
        }
    }
}

/// A parsed `kitten icat` invocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IcatArgs {
    pub options: IcatOptions,
    pub files: Vec<String>,
    pub help: bool,
}

/// Usage/help text for `kitten icat`.
pub const ICAT_USAGE: &str = "\
Usage: kitten icat [OPTIONS] [FILE...]

Display PNG images in the terminal using the kitty graphics protocol.
With no FILE, or with FILE '-', image data is read from standard input.

Options:
  --place WxH@LxT       Display in a cell rectangle (single image only)
  --scale MODE          none, width, height, both, up, or WxH (default: none)
  --fit MODE            Alias for --scale
  --scale-up            Allow images to be enlarged when fitting
  --align left|center|right
  --z-index N, -z N     Image z-index
  --clear               Remove all visible images first
  --clear-all           Remove images and scrollback placements first
  --stdin detect|yes|no
  --use-window-size C,R,PX,PY
  --format png|rgb|rgba (default: png)
  --image-id N
  --chunk-size BYTES    Maximum payload bytes per graphics command
  --window-id N         Override the inherited window id
  --no-trailing-newline, -n
  -h, --help";

/// Parse `kitten icat` arguments (excluding `icat` itself).
pub fn parse_icat_args<I, S>(args: I) -> Result<IcatArgs, IcatError>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let args: Vec<String> = args.into_iter().map(Into::into).collect();
    let mut options = IcatOptions::default();
    let mut files = Vec::new();
    let mut help = false;
    let mut index = 0usize;
    while index < args.len() {
        let arg = &args[index];
        if arg == "--" {
            files.extend(args[index + 1..].iter().cloned());
            break;
        }
        if arg == "-" {
            files.push(arg.clone());
            index += 1;
            continue;
        }
        if !arg.starts_with('-') || arg == "--1" {
            files.push(arg.clone());
            index += 1;
            continue;
        }
        let (name, inline) = match arg.split_once('=') {
            Some((name, value)) => (name, Some(value.to_owned())),
            None => (arg.as_str(), None),
        };
        let mut take = |option: &'static str| -> Result<String, IcatError> {
            if let Some(value) = &inline {
                return Ok(value.clone());
            }
            index += 1;
            args.get(index)
                .cloned()
                .ok_or(IcatError::MissingValue(option))
        };
        match name {
            "-h" | "--help" => help = true,
            "--place" => options.place = Some(Place::parse(&take("--place")?)?),
            "--scale" | "--fit" => {
                let value = take("--scale")?;
                let scale = Scale::parse(&value)?;
                options.scale_up |=
                    matches!(value.to_ascii_lowercase().as_str(), "up" | "scale-up");
                options.scale = Some(scale);
            }
            "--scale-up" => options.scale_up = true,
            "--align" => options.align = Align::parse(&take("--align")?)?,
            "--z-index" | "-z" => {
                let value = take("--z-index")?;
                options.z_index = value.parse().map_err(|_| IcatError::InvalidOption {
                    option: "--z-index",
                    value,
                })?;
            }
            "--clear" => options.clear = true,
            "--clear-all" => options.clear_all = true,
            "--stdin" => {
                options.stdin = match inline {
                    Some(value) => StdinMode::parse(&value)?,
                    None => StdinMode::Yes,
                };
            }
            "--use-window-size" => {
                options.window_size = Some(WindowSize::parse(&take("--use-window-size")?)?)
            }
            "--format" => options.format = OutputFormat::parse(&take("--format")?)?,
            "--image-id" => {
                let value = take("--image-id")?;
                options.image_id = value.parse().map_err(|_| IcatError::InvalidOption {
                    option: "--image-id",
                    value,
                })?;
            }
            "--chunk-size" => {
                let value = take("--chunk-size")?;
                options.chunk_size = value.parse::<usize>().ok().filter(|size| *size > 0).ok_or(
                    IcatError::InvalidOption {
                        option: "--chunk-size",
                        value,
                    },
                )?;
            }
            "--window-id" => {
                let value = take("--window-id")?;
                options.window_id = Some(crate::env::parse_window_id(&value).ok_or(
                    IcatError::InvalidOption {
                        option: "--window-id",
                        value,
                    },
                )?);
            }
            "--no-trailing-newline" | "-n" => options.trailing_newline = false,
            "--silent" | "--quiet" => options.quiet = true,
            other => return Err(IcatError::UnknownOption(other.to_owned())),
        }
        index += 1;
    }
    Ok(IcatArgs {
        options,
        files,
        help,
    })
}

/// Errors from parsing or encoding an image.
#[derive(Debug)]
#[non_exhaustive]
pub enum IcatError {
    Io(std::io::Error),
    Png(PngError),
    MissingValue(&'static str),
    UnknownOption(String),
    InvalidOption { option: &'static str, value: String },
    NoInput,
    PlaceRequiresSingleImage(usize),
    Message(String),
}

impl std::fmt::Display for IcatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "I/O error: {error}"),
            Self::Png(error) => write!(f, "PNG error: {error}"),
            Self::MissingValue(option) => write!(f, "{option} requires a value"),
            Self::UnknownOption(option) => write!(f, "unknown icat option: {option}"),
            Self::InvalidOption { option, value } => {
                write!(f, "invalid value for {option}: {value:?}")
            }
            Self::NoInput => f.write_str("no image input: specify a file or enable --stdin"),
            Self::PlaceRequiresSingleImage(count) => write!(
                f,
                "--place can only be used with a single image, not {count}"
            ),
            Self::Message(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for IcatError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Png(error) => Some(error),
            _ => None,
        }
    }
}

impl From<std::io::Error> for IcatError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<PngError> for IcatError {
    fn from(value: PngError) -> Self {
        Self::Png(value)
    }
}

struct PreparedImage {
    payload: Vec<u8>,
    format: OutputFormat,
    width: u32,
    height: u32,
}

fn resize_rgba(source: &[u8], width: u32, height: u32, new_width: u32, new_height: u32) -> Vec<u8> {
    let mut output = vec![0u8; new_width as usize * new_height as usize * 4];
    for y in 0..new_height {
        let sy = (u64::from(y) * u64::from(height) / u64::from(new_height)) as u32;
        for x in 0..new_width {
            let sx = (u64::from(x) * u64::from(width) / u64::from(new_width)) as u32;
            let source_index = ((sy * width + sx) * 4) as usize;
            let target_index = ((y * new_width + x) * 4) as usize;
            output[target_index..target_index + 4]
                .copy_from_slice(&source[source_index..source_index + 4]);
        }
    }
    output
}

fn fit_dimensions(
    width: u32,
    height: u32,
    available_width: Option<u32>,
    available_height: Option<u32>,
    mode: FitMode,
    scale_up: bool,
) -> (u32, u32) {
    if mode == FitMode::None {
        return (width, height);
    }
    let mut ratio = f64::INFINITY;
    if matches!(mode, FitMode::Width | FitMode::Both) {
        if let Some(limit) = available_width {
            ratio = ratio.min(f64::from(limit) / f64::from(width));
        }
    }
    if matches!(mode, FitMode::Height | FitMode::Both) {
        if let Some(limit) = available_height {
            ratio = ratio.min(f64::from(limit) / f64::from(height));
        }
    }
    if !ratio.is_finite() {
        ratio = 1.0;
    }
    if !scale_up {
        ratio = ratio.min(1.0);
    }
    ratio = ratio.max(0.0);
    let scaled_width = (f64::from(width) * ratio).round().max(1.0) as u32;
    let scaled_height = (f64::from(height) * ratio).round().max(1.0) as u32;
    (
        scaled_width.min(MAX_IMAGE_DIMENSION),
        scaled_height.min(MAX_IMAGE_DIMENSION),
    )
}

fn available_pixels(options: &IcatOptions) -> (Option<u32>, Option<u32>, WindowSize) {
    let window = options.window_size.unwrap_or(FALLBACK_WINDOW);
    if let Some(place) = options.place {
        let width = place.width.saturating_mul(window.cell_width()).max(1);
        let height = place.height.saturating_mul(window.cell_height()).max(1);
        return (Some(width), Some(height), window);
    }
    (Some(window.width_px), Some(window.height_px), window)
}

fn target_dimensions(options: &IcatOptions, width: u32, height: u32) -> (u32, u32) {
    match options.scale {
        Some(Scale::Exact {
            width: exact_width,
            height: exact_height,
        }) => (exact_width, exact_height),
        Some(Scale::Fit(mode)) => {
            let (available_width, available_height, _) = available_pixels(options);
            fit_dimensions(
                width,
                height,
                available_width,
                available_height,
                mode,
                options.scale_up,
            )
        }
        None if options.place.is_some() => {
            let (available_width, available_height, _) = available_pixels(options);
            fit_dimensions(
                width,
                height,
                available_width,
                available_height,
                FitMode::Both,
                options.scale_up,
            )
        }
        None => (width, height),
    }
}

fn png_dimensions(data: &[u8]) -> Option<(u32, u32)> {
    const SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
    if data.len() < 33 || &data[..8] != SIGNATURE || &data[12..16] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes(data[16..20].try_into().ok()?);
    let height = u32::from_be_bytes(data[20..24].try_into().ok()?);
    (width > 0 && height > 0 && width <= MAX_IMAGE_DIMENSION && height <= MAX_IMAGE_DIMENSION)
        .then_some((width, height))
}

fn prepare_image(data: &[u8], options: &IcatOptions) -> Result<PreparedImage, IcatError> {
    // The common path is a pass-through transmission: the terminal decodes the
    // PNG, so there is no need to inflate the pixels here. Read the IHDR for
    // the protocol dimensions and leave the bytes untouched.
    if options.format == OutputFormat::Png && options.scale.is_none() && options.place.is_none() {
        if let Some((width, height)) = png_dimensions(data) {
            return Ok(PreparedImage {
                payload: data.to_vec(),
                format: OutputFormat::Png,
                width,
                height,
            });
        }
    }
    let decoded = decode_png(data, MAX_IMAGE_DIMENSION)?;
    let (target_width, target_height) = target_dimensions(options, decoded.width, decoded.height);
    let needs_resize = target_width != decoded.width || target_height != decoded.height;
    let rgba = if needs_resize {
        resize_rgba(
            &decoded.rgba,
            decoded.width,
            decoded.height,
            target_width,
            target_height,
        )
    } else {
        decoded.rgba.clone()
    };
    let (payload, format) = match options.format {
        OutputFormat::Png if !needs_resize => (data.to_vec(), OutputFormat::Png),
        OutputFormat::Png => (
            encode_png_rgba(target_width, target_height, &rgba)?,
            OutputFormat::Png,
        ),
        OutputFormat::Rgba => (rgba, OutputFormat::Rgba),
        OutputFormat::Rgb => {
            let mut rgb = Vec::with_capacity(rgba.len() / 4 * 3);
            for pixel in rgba.as_chunks::<4>().0 {
                rgb.extend_from_slice(&pixel[..3]);
            }
            (rgb, OutputFormat::Rgb)
        }
    };
    Ok(PreparedImage {
        payload,
        format,
        width: target_width,
        height: target_height,
    })
}

struct PlacementFields {
    columns: u32,
    rows: u32,
    /// Destination pixel offset inside the starting cell (`X=`).
    cell_x_offset: u32,
    /// Number of whole cells to move right before placing (no `--place`).
    move_right_cells: u32,
    /// Absolute cursor target for `--place` (row, column), 1-based.
    cursor: Option<(u32, u32)>,
    static_cursor: bool,
}

fn placement_fields(options: &IcatOptions, width: u32, height: u32) -> PlacementFields {
    let (_, _, window) = available_pixels(options);
    let cell_width = window.cell_width();
    let cell_height = window.cell_height();
    let columns = width.div_ceil(cell_width).max(1);
    let rows = height.div_ceil(cell_height).max(1);
    let remainder = width % cell_width;
    let cell_x_offset = if remainder == 0 {
        0
    } else {
        match options.align {
            Align::Left => 0,
            Align::Center => (cell_width - remainder) / 2,
            Align::Right => cell_width - remainder,
        }
    };
    if let Some(place) = options.place {
        let align_cells = options.align.offset_cells(place.width, columns);
        return PlacementFields {
            columns,
            rows,
            cell_x_offset,
            move_right_cells: 0,
            cursor: Some((
                place.top.saturating_add(1),
                place.left.saturating_add(1).saturating_add(align_cells),
            )),
            static_cursor: true,
        };
    }
    PlacementFields {
        columns,
        rows,
        cell_x_offset,
        move_right_cells: options.align.offset_cells(window.cols, columns),
        cursor: None,
        static_cursor: false,
    }
}

fn push_control_field(control: &mut String, key: &str, value: impl std::fmt::Display) {
    if !control.is_empty() {
        control.push(',');
    }
    control.push_str(key);
    control.push('=');
    control.push_str(&value.to_string());
}

fn encode_chunk_frame(control: &str, chunk: &[u8], more: bool, include_more: bool) -> Vec<u8> {
    let mut fields = control.to_owned();
    if include_more {
        if !fields.is_empty() {
            fields.push(',');
        }
        fields.push_str(if more { "m=1" } else { "m=0" });
    }
    let encoded = encode_base64(chunk, false);
    let mut frame = Vec::with_capacity(fields.len() + encoded.len() + 8);
    frame.extend_from_slice(b"\x1b_G");
    frame.extend_from_slice(fields.as_bytes());
    if !encoded.is_empty() {
        frame.push(b';');
        frame.extend_from_slice(&encoded);
    }
    frame.extend_from_slice(b"\x1b\\");
    frame
}

/// Encode only the graphics frames for one PNG (cursor move included). The
/// clear sequences are not included; use [`encode_png`] for the complete
/// `icat` output.
pub fn encode_png_frames(data: &[u8], options: &IcatOptions) -> Result<Vec<Vec<u8>>, IcatError> {
    let prepared = prepare_image(data, options)?;
    let placement = placement_fields(options, prepared.width, prepared.height);
    let mut control = String::new();
    push_control_field(&mut control, "a", "T");
    push_control_field(&mut control, "f", prepared.format.protocol_value());
    push_control_field(&mut control, "s", prepared.width);
    push_control_field(&mut control, "v", prepared.height);
    if options.image_id != 0 {
        push_control_field(&mut control, "i", options.image_id);
    }
    if options.quiet {
        push_control_field(&mut control, "q", 2);
    }
    if options.z_index != 0 {
        push_control_field(&mut control, "z", options.z_index);
    }
    if placement.static_cursor {
        push_control_field(&mut control, "C", 1);
        push_control_field(&mut control, "c", placement.columns);
        push_control_field(&mut control, "r", placement.rows);
    }
    if placement.cell_x_offset != 0 {
        push_control_field(&mut control, "X", placement.cell_x_offset);
    }
    let followup = {
        let mut followup = String::new();
        push_control_field(&mut followup, "a", "T");
        if options.quiet {
            push_control_field(&mut followup, "q", 2);
        }
        followup
    };

    let chunk_size = options.chunk_size.max(1);
    let chunks: Vec<&[u8]> = prepared.payload.chunks(chunk_size).collect();
    let chunked = chunks.len() > 1;
    let mut frames = Vec::with_capacity(chunks.len() + 1);
    if let Some((row, column)) = placement.cursor {
        frames.push(format!("\x1b[{row};{column}H").into_bytes());
    } else {
        // Kitty's icat resets the column and moves right by whole cells for
        // horizontal alignment. The sub-cell remainder is carried by `X=`.
        let mut prefix = b"\r".to_vec();
        if placement.move_right_cells != 0 {
            prefix.extend_from_slice(format!("\x1b[{}C", placement.move_right_cells).as_bytes());
        }
        frames.push(prefix);
    }
    for (index, chunk) in chunks.iter().enumerate() {
        let more = index + 1 < chunks.len();
        let fields = if index == 0 {
            control.as_str()
        } else {
            followup.as_str()
        };
        frames.push(encode_chunk_frame(fields, chunk, more, chunked));
    }
    Ok(frames)
}

/// Complete clear sequences requested by the options.
pub fn clear_frames(options: &IcatOptions) -> Vec<Vec<u8>> {
    let mut frames = Vec::new();
    if options.clear || options.clear_all {
        frames.push(b"\x1b_Ga=d,d=A,q=2\x1b\\".to_vec());
    }
    if options.clear_all {
        frames.push(b"\x1b_Ga=d,d=R,q=2\x1b\\".to_vec());
    }
    frames
}

/// Encode one PNG as complete `icat` output (clear sequences, placement and
/// chunks).
pub fn encode_png(data: &[u8], options: &IcatOptions) -> Result<Vec<u8>, IcatError> {
    let mut output = Vec::new();
    for frame in clear_frames(options) {
        output.extend_from_slice(&frame);
    }
    for frame in encode_png_frames(data, options)? {
        output.extend_from_slice(&frame);
    }
    Ok(output)
}

/// Read and display images. This is the terminal-free core of the binary; the
/// caller supplies stdin/stdout, which makes the function testable.
pub fn run_icat<R: Read, W: Write>(
    args: &IcatArgs,
    stdin: &mut R,
    stdout: &mut W,
) -> Result<(), IcatError> {
    let mut inputs: Vec<Option<PathBuf>> = Vec::new();
    for file in &args.files {
        if file == "-" {
            if args.options.stdin == StdinMode::No {
                return Err(IcatError::Message(
                    "- cannot be used with --stdin=no".to_owned(),
                ));
            }
            inputs.push(None);
        } else {
            inputs.push(Some(PathBuf::from(file)));
        }
    }
    if inputs.is_empty() && args.options.stdin != StdinMode::No {
        inputs.push(None);
    }
    if inputs.is_empty() {
        return Err(IcatError::NoInput);
    }
    if args.options.place.is_some() && inputs.len() > 1 {
        return Err(IcatError::PlaceRequiresSingleImage(inputs.len()));
    }
    for frame in clear_frames(&args.options) {
        stdout.write_all(&frame)?;
    }
    for input in inputs {
        let data = match input {
            Some(path) => std::fs::read(&path).map_err(|error| {
                IcatError::Message(format!("failed to read {}: {error}", path.display()))
            })?,
            None => {
                let mut data = Vec::new();
                stdin.read_to_end(&mut data)?;
                data
            }
        };
        for frame in encode_png_frames(&data, &args.options)? {
            stdout.write_all(&frame)?;
        }
        if args.options.trailing_newline && args.options.place.is_none() {
            stdout.write_all(b"\n")?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_place_and_window_geometry() {
        assert_eq!(
            Place::parse("40x20@3x4").unwrap(),
            Place {
                width: 40,
                height: 20,
                left: 3,
                top: 4
            }
        );
        assert!(Place::parse("40x20").is_err());
        assert!(Place::parse("0x20@1x2").is_err());
        assert_eq!(
            WindowSize::parse("80,24,800,480").unwrap(),
            WindowSize {
                cols: 80,
                rows: 24,
                width_px: 800,
                height_px: 480
            }
        );
        assert!(WindowSize::parse("80,24,79,480").is_err());
    }

    #[test]
    fn parses_scale_align_and_format_options() {
        let args = parse_icat_args([
            "--scale=both",
            "--scale-up",
            "--align",
            "right",
            "--format",
            "rgb",
            "--z-index=-3",
            "a.png",
        ])
        .unwrap();
        assert_eq!(args.options.scale, Some(Scale::Fit(FitMode::Both)));
        assert!(args.options.scale_up);
        assert_eq!(args.options.align, Align::Right);
        assert_eq!(args.options.format, OutputFormat::Rgb);
        assert_eq!(args.options.z_index, -3);
        assert_eq!(args.files, vec!["a.png".to_owned()]);
        assert_eq!(
            Scale::parse("100x50").unwrap(),
            Scale::Exact {
                width: 100,
                height: 50
            }
        );
    }

    #[test]
    fn rejects_unknown_and_incomplete_options() {
        assert!(matches!(
            parse_icat_args(["--bogus"]),
            Err(IcatError::UnknownOption(_))
        ));
        assert!(matches!(
            parse_icat_args(["--place"]),
            Err(IcatError::MissingValue("--place"))
        ));
        assert!(matches!(
            parse_icat_args(["--align=middle"]),
            Err(IcatError::InvalidOption {
                option: "--align",
                ..
            })
        ));
        assert!(matches!(
            parse_icat_args(["--chunk-size=0"]),
            Err(IcatError::InvalidOption {
                option: "--chunk-size",
                ..
            })
        ));
    }
}
