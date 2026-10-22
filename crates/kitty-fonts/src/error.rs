// SPDX-License-Identifier: GPL-3.0-or-later
//! Error type shared by the font stack.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// A FreeType call failed.
    FreeType { operation: &'static str, code: i32 },
    /// Fontconfig could not produce a match.
    Fontconfig(&'static str),
    /// A path, family or PostScript name could not be converted to a C string.
    InvalidString,
    /// The font specification string is malformed.
    InvalidFontSpec(String),
    /// A HarfBuzz font feature string is malformed.
    InvalidFeature(String),
    /// The face does not contain the requested glyph.
    MissingGlyph(u32),
    /// The face index is outside the range of faces in the file.
    InvalidFaceIndex(i32),
    /// No face in the stack covers the requested text.
    NoCoverage { codepoint: u32 },
    /// The requested size is not usable for this face.
    InvalidSize,
    /// FreeType produced a pixel mode that cannot be represented by
    /// [`crate::GlyphBitmap`].
    UnsupportedPixelMode(u8),
    /// FreeType produced a glyph format that is not a bitmap after rendering.
    UnsupportedGlyphFormat(u32),
    /// The glyph could not be rasterized safely.
    Raster(&'static str),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FreeType { operation, code } => write!(
                f,
                "{operation} failed: {} (FreeType error 0x{code:02x})",
                crate::ffi::freetype::error_name(*code)
            ),
            Self::Fontconfig(msg) => write!(f, "fontconfig: {msg}"),
            Self::InvalidString => write!(f, "string contains an interior NUL byte"),
            Self::InvalidFontSpec(msg) => write!(f, "invalid font specification: {msg}"),
            Self::InvalidFeature(msg) => write!(f, "invalid font feature: {msg}"),
            Self::MissingGlyph(cp) => write!(f, "no glyph for U+{cp:04X}"),
            Self::InvalidFaceIndex(idx) => write!(f, "invalid face index: {idx}"),
            Self::NoCoverage { codepoint } => {
                write!(f, "no face in the stack covers U+{codepoint:04X}")
            }
            Self::InvalidSize => write!(f, "invalid font size"),
            Self::UnsupportedPixelMode(mode) => {
                write!(f, "unsupported FreeType pixel mode: {mode}")
            }
            Self::UnsupportedGlyphFormat(format) => {
                write!(f, "unsupported FreeType glyph format: 0x{format:08x}")
            }
            Self::Raster(msg) => write!(f, "rasterization failed: {msg}"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn ft_check(operation: &'static str, code: i32) -> Result<()> {
    if code == 0 {
        Ok(())
    } else {
        Err(Error::FreeType { operation, code })
    }
}
