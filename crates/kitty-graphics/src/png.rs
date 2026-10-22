// SPDX-License-Identifier: GPL-3.0-or-later
//! PNG decoding and encoding through libpng's simplified API.
//!
//! Ported from `kitty/png-reader.c` and `kitty/png-reader.h`.
//!
//! The C implementation uses libpng's low level API with `setjmp()`/`longjmp()`
//! error handling. Rust cannot safely return through a C `setjmp()` frame, so
//! this port uses libpng's simplified API (`png_image_*`) instead. It performs
//! the same normalisation that kitty requests explicitly:
//!
//! * palette images are expanded to RGB,
//! * grayscale (including 1/2/4 bit) is expanded and converted to RGB,
//! * 16 bit samples are reduced to 8 bit,
//! * `tRNS` is converted to alpha,
//! * missing alpha channels are filled with opaque alpha,
//! * every image is returned as tightly packed, top-down RGBA8.
//!
//! ICC colour management is **deferred**: kitty uses lcms2 for embedded
//! profiles. The simplified API returns sRGB-assumed 8 bit data and does not
//! apply an ICC transform. Gamma handling is likewise left to libpng. This is
//! documented as a deviation in the crate README section of `lib.rs`.
//!
//! Copyright (C) 2018 Kovid Goyal <kovid at kovidgoyal.net>

use std::ffi::{c_char, c_int, c_void};

/// libpng's `PNG_IMAGE_VERSION`.
const PNG_IMAGE_VERSION: u32 = 1;
/// libpng's `PNG_FORMAT_RGB`.
const PNG_FORMAT_RGB: u32 = 0x02;
/// libpng's `PNG_FORMAT_RGBA`.
const PNG_FORMAT_RGBA: u32 = 0x03;
/// libpng's `PNG_FORMAT_GRAY`.
const PNG_FORMAT_GRAY: u32 = 0x00;
/// libpng's `PNG_FORMAT_GA`.
const PNG_FORMAT_GA: u32 = 0x01;
/// libpng's `PNG_FORMAT_LINEAR_RGBA`.
const PNG_FORMAT_LINEAR_RGBA: u32 = 0x07;

#[repr(C)]
struct PngImage {
    opaque: *mut c_void,
    version: u32,
    width: u32,
    height: u32,
    format: u32,
    flags: u32,
    colormap_entries: u32,
    warning_or_error: u32,
    message: [c_char; 64],
}

impl PngImage {
    fn new() -> Self {
        Self {
            opaque: std::ptr::null_mut(),
            version: PNG_IMAGE_VERSION,
            width: 0,
            height: 0,
            format: 0,
            flags: 0,
            colormap_entries: 0,
            warning_or_error: 0,
            message: [0; 64],
        }
    }

    fn error_message(&self) -> String {
        let bytes: Vec<u8> = self
            .message
            .iter()
            .take_while(|&&c| c != 0)
            .map(|&c| c as u8)
            .collect();
        if bytes.is_empty() {
            "unknown libpng error".to_string()
        } else {
            String::from_utf8_lossy(&bytes).into_owned()
        }
    }
}

#[link(name = "png16")]
extern "C" {
    fn png_image_begin_read_from_memory(
        image: *mut PngImage,
        memory: *const c_void,
        size: usize,
    ) -> c_int;
    fn png_image_finish_read(
        image: *mut PngImage,
        background: *const c_void,
        buffer: *mut c_void,
        row_stride: i32,
        colormap: *mut c_void,
    ) -> c_int;
    fn png_image_free(image: *mut PngImage);
    fn png_image_write_to_memory(
        image: *mut PngImage,
        memory: *mut c_void,
        memory_bytes: *mut usize,
        convert_to_8bit: c_int,
        buffer: *const c_void,
        row_stride: i32,
        colormap: *const c_void,
    ) -> c_int;
}

/// Error returned by PNG decoding or encoding.
///
/// Kitty reports malformed PNG data as `EBADPNG` and allocation/limit failures
/// as `ENOMEM`. Those codes are preserved through [`PngError::code`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PngError {
    code: &'static str,
    message: String,
}

impl PngError {
    fn bad(message: impl Into<String>) -> Self {
        Self {
            code: "EBADPNG",
            message: message.into(),
        }
    }

    fn too_large(message: impl Into<String>) -> Self {
        Self {
            code: "ENOMEM",
            message: message.into(),
        }
    }

    /// Kitty-compatible error code (`EBADPNG` or `ENOMEM`).
    pub const fn code(&self) -> &'static str {
        self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl core::fmt::Display for PngError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)
    }
}

impl std::error::Error for PngError {}

/// A decoded, tightly packed, top-down RGBA8 image.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodedPng {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// `width * height * 4` bytes of RGBA data.
    pub rgba: Vec<u8>,
}

impl DecodedPng {
    /// Row stride in bytes (always `width * 4`).
    pub fn row_stride(&self) -> usize {
        self.width as usize * 4
    }
}

/// Decode a PNG byte stream to RGBA8.
///
/// `max_image_dimension` mirrors kitty's `MAX_IMAGE_DIMENSION` check. A value
/// of zero disables the limit (not recommended for untrusted input).
pub fn decode_png(data: &[u8], max_image_dimension: u32) -> Result<DecodedPng, PngError> {
    if data.is_empty() {
        return Err(PngError::bad("empty PNG data"));
    }
    let mut image = PngImage::new();
    let ok = unsafe {
        png_image_begin_read_from_memory(&mut image, data.as_ptr().cast::<c_void>(), data.len())
    };
    if ok == 0 {
        let msg = image.error_message();
        unsafe { png_image_free(&mut image) };
        return Err(PngError::bad(msg));
    }

    let width = image.width;
    let height = image.height;
    if width == 0 || height == 0 {
        unsafe { png_image_free(&mut image) };
        return Err(PngError::bad("PNG image has zero width or height"));
    }
    if max_image_dimension != 0 && (width > max_image_dimension || height > max_image_dimension) {
        let msg = format!(
            "PNG image is too large: {width}x{height}, maximum dimension is {max_image_dimension}"
        );
        unsafe { png_image_free(&mut image) };
        return Err(PngError::too_large(msg));
    }

    let stride = (width as usize)
        .checked_mul(4)
        .ok_or_else(|| PngError::too_large("PNG row size overflow"))?;
    let total = stride
        .checked_mul(height as usize)
        .ok_or_else(|| PngError::too_large("PNG image size overflow"))?;
    let stride_i32 = i32::try_from(stride)
        .map_err(|_| PngError::too_large("PNG row stride does not fit in libpng's i32"))?;
    let mut rgba = vec![0u8; total];
    image.format = PNG_FORMAT_RGBA;
    let ok = unsafe {
        png_image_finish_read(
            &mut image,
            std::ptr::null(),
            rgba.as_mut_ptr().cast::<c_void>(),
            stride_i32,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        let msg = image.error_message();
        unsafe { png_image_free(&mut image) };
        return Err(PngError::bad(msg));
    }
    unsafe { png_image_free(&mut image) };
    Ok(DecodedPng {
        width,
        height,
        rgba,
    })
}

fn encode_with_format(
    width: u32,
    height: u32,
    format: u32,
    components: usize,
    convert_to_8bit: bool,
    data: &[u8],
) -> Result<Vec<u8>, PngError> {
    if width == 0 || height == 0 {
        return Err(PngError::bad("PNG dimensions must be positive"));
    }
    let stride = (width as usize)
        .checked_mul(components)
        .ok_or_else(|| PngError::too_large("PNG row size overflow"))?;
    let expected = stride
        .checked_mul(height as usize)
        .ok_or_else(|| PngError::too_large("PNG image size overflow"))?;
    if data.len() != expected {
        return Err(PngError::bad(format!(
            "pixel buffer has size {}, expected {expected}",
            data.len()
        )));
    }
    let stride_i32 = i32::try_from(stride)
        .map_err(|_| PngError::too_large("PNG row stride does not fit in libpng's i32"))?;
    let mut image = PngImage::new();
    image.width = width;
    image.height = height;
    image.format = format;
    let mut size = 0usize;
    let ok = unsafe {
        png_image_write_to_memory(
            &mut image,
            std::ptr::null_mut(),
            &mut size,
            i32::from(convert_to_8bit),
            data.as_ptr().cast::<c_void>(),
            stride_i32,
            std::ptr::null(),
        )
    };
    if ok == 0 || size == 0 {
        let msg = image.error_message();
        unsafe { png_image_free(&mut image) };
        return Err(PngError::bad(msg));
    }
    let mut out = vec![0u8; size];
    let mut written = size;
    let ok = unsafe {
        png_image_write_to_memory(
            &mut image,
            out.as_mut_ptr().cast::<c_void>(),
            &mut written,
            i32::from(convert_to_8bit),
            data.as_ptr().cast::<c_void>(),
            stride_i32,
            std::ptr::null(),
        )
    };
    unsafe { png_image_free(&mut image) };
    if ok == 0 {
        return Err(PngError::bad("libpng failed to encode PNG"));
    }
    out.truncate(written);
    Ok(out)
}

/// Encode tightly packed RGBA8 pixels as a PNG (used by tests and by callers
/// that need to generate fixtures without an external image library).
pub fn encode_png_rgba(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, PngError> {
    encode_with_format(width, height, PNG_FORMAT_RGBA, 4, false, rgba)
}

/// Encode tightly packed RGB8 pixels as a PNG.
pub fn encode_png_rgb(width: u32, height: u32, rgb: &[u8]) -> Result<Vec<u8>, PngError> {
    encode_with_format(width, height, PNG_FORMAT_RGB, 3, false, rgb)
}

/// Encode one-byte grayscale pixels as a PNG.
pub fn encode_png_gray(width: u32, height: u32, gray: &[u8]) -> Result<Vec<u8>, PngError> {
    encode_with_format(width, height, PNG_FORMAT_GRAY, 1, false, gray)
}

/// Encode grayscale + alpha pixels as a PNG.
pub fn encode_png_gray_alpha(width: u32, height: u32, ga: &[u8]) -> Result<Vec<u8>, PngError> {
    encode_with_format(width, height, PNG_FORMAT_GA, 2, false, ga)
}

/// Encode a 16 bit per channel linear RGBA PNG. The input is native-endian
/// `u16` components. Used to exercise 16 bit stripping in tests.
pub fn encode_png_rgba16(width: u32, height: u32, rgba: &[u16]) -> Result<Vec<u8>, PngError> {
    let mut bytes = Vec::with_capacity(rgba.len() * 2);
    for v in rgba {
        bytes.extend_from_slice(&v.to_ne_bytes());
    }
    encode_with_format(width, height, PNG_FORMAT_LINEAR_RGBA, 8, false, &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_rgba() {
        let pixels: Vec<u8> = (0..4 * 3 * 4).map(|i| (i * 7) as u8).collect();
        let png = encode_png_rgba(4, 3, &pixels).unwrap();
        let decoded = decode_png(&png, 10000).unwrap();
        assert_eq!((decoded.width, decoded.height), (4, 3));
        assert_eq!(decoded.rgba, pixels);
        assert_eq!(decoded.row_stride(), 16);
    }

    #[test]
    fn roundtrip_rgb_fills_opaque_alpha() {
        let pixels: Vec<u8> = (0..3 * 2 * 3).map(|i| (i * 11) as u8).collect();
        let png = encode_png_rgb(3, 2, &pixels).unwrap();
        let decoded = decode_png(&png, 10000).unwrap();
        let mut expected = Vec::new();
        for rgb in pixels.chunks(3) {
            expected.extend_from_slice(rgb);
            expected.push(255);
        }
        assert_eq!(decoded.rgba, expected);
    }

    #[test]
    fn decodes_palette_png() {
        // 2x2 palette PNG with a tRNS chunk, generated once with libpng's low
        // level API and checked in as a tiny fixture.
        let png = PALETTE_PNG;
        let decoded = decode_png(png, 10000).unwrap();
        assert_eq!((decoded.width, decoded.height), (2, 2));
        assert_eq!(decoded.rgba.len(), 16);
        assert_eq!(&decoded.rgba[..4], &[255, 0, 0, 255]);
        assert_eq!(&decoded.rgba[4..8], &[0, 255, 0, 128]);
        assert_eq!(&decoded.rgba[8..12], &[0, 0, 255, 255]);
        assert_eq!(&decoded.rgba[12..16], &[255, 255, 255, 0]);
    }

    #[test]
    fn decodes_gray_and_gray_alpha() {
        let png = encode_png_gray(3, 1, &[0, 128, 255]).unwrap();
        let decoded = decode_png(&png, 10000).unwrap();
        assert_eq!(
            decoded.rgba,
            vec![0, 0, 0, 255, 128, 128, 128, 255, 255, 255, 255, 255]
        );

        let png = encode_png_gray_alpha(2, 1, &[10, 20, 30, 40]).unwrap();
        let decoded = decode_png(&png, 10000).unwrap();
        assert_eq!(decoded.rgba, vec![10, 10, 10, 20, 30, 30, 30, 40]);
    }

    #[test]
    fn decodes_sixteen_bit_png_to_eight_bit() {
        let pixels: Vec<u16> = vec![65535, 0, 0, 65535, 0, 65535, 0, 65535, 0, 0, 65535, 65535];
        let png = encode_png_rgba16(3, 1, &pixels).unwrap();
        let decoded = decode_png(&png, 10000).unwrap();
        assert_eq!((decoded.width, decoded.height), (3, 1));
        // libpng may colour manage the linear 16 bit input, so only check that
        // the alpha channel survives and the output is eight bit RGBA.
        assert_eq!(decoded.rgba.len(), 12);
        assert_eq!(decoded.rgba[3], 255);
        assert_eq!(decoded.rgba[7], 255);
        assert_eq!(decoded.rgba[11], 255);
    }

    #[test]
    fn rejects_malformed_and_truncated_png() {
        let err = decode_png(b"dsfsdfsfsfd", 10000).unwrap_err();
        assert_eq!(err.code(), "EBADPNG");
        assert!(err.to_string().starts_with("[EBADPNG]"));

        let png = encode_png_rgba(3, 3, &[0u8; 36]).unwrap();
        let truncated = &png[..png.len() - 16];
        let err = decode_png(truncated, 10000).unwrap_err();
        assert_eq!(err.code(), "EBADPNG");
    }

    #[test]
    fn enforces_dimension_limit_before_allocating() {
        let png = encode_png_rgba(100, 50, &vec![0u8; 100 * 50 * 4]).unwrap();
        let err = decode_png(&png, 99).unwrap_err();
        assert_eq!(err.code(), "ENOMEM");
        assert!(err.message().contains("too large"));
    }

    // 2x2, 2-bit palette PNG with a tRNS chunk. Kept inline so the test needs
    // no fixture files at runtime.
    const PALETTE_PNG: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x02, 0x02, 0x03, 0x00, 0x00, 0x00, 0x0f,
        0xd8, 0xe5, 0xb7, 0x00, 0x00, 0x00, 0x0c, 0x50, 0x4c, 0x54, 0x45, 0xff, 0x00, 0x00, 0x00,
        0xff, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0xfb, 0x00, 0x60, 0xf6, 0x00, 0x00, 0x00,
        0x04, 0x74, 0x52, 0x4e, 0x53, 0xff, 0x80, 0xff, 0x00, 0x1f, 0x87, 0x86, 0x99, 0x00, 0x00,
        0x00, 0x0c, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0x10, 0x60, 0xd8, 0x00, 0x00, 0x00,
        0xe4, 0x00, 0xc1, 0x19, 0x55, 0x3b, 0xfb, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44,
        0xae, 0x42, 0x60, 0x82,
    ];
}
