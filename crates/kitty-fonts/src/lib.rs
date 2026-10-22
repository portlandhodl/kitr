// SPDX-License-Identifier: GPL-3.0-or-later
//! Font discovery, metrics and shaping ported from `kitty/fonts.c`,
//! `kitty/freetype.c`, `kitty/fontconfig.c`, `kitty/font-names.c`,
//! `kitty/shaped-run-cache.c` and `kitty/disk-cache.c`.
//!
//! # Supported subset
//!
//! * Raw FFI to the system FreeType 2, HarfBuzz and Fontconfig libraries.
//! * Safe RAII wrappers for `FT_Library`, `FT_Face`, `hb_font_t`, `hb_buffer_t`
//!   and Fontconfig patterns.
//! * Fontconfig family/style matching, PostScript and full-name matching, and
//!   lazy codepoint-coverage fallback.
//! * kitty's font-specification parser (bare family, `key=value`, quoted
//!   values, features and design axes).
//! * kitty-compatible cell metrics: cell width/height, baseline, underline and
//!   strikethrough geometry, plus per-glyph FreeType metrics.
//! * HarfBuzz shaping of UTF-32 runs with features and basic ligatures.
//! * Glyph rasterization into owned [`raster::GlyphBitmap`] values (Gray8,
//!   Rgba8 and Bgra8) and a bounded [`raster::RasterCache`].
//!
//! # Rasterization
//!
//! Outline glyphs are rendered with `FT_RENDER_MODE_NORMAL`; embedded
//! grayscale/bitmap glyphs are copied as Gray8, and FreeType color bitmaps
//! (`FT_PIXEL_MODE_BGRA`) are exposed as Bgra8. `FT_PIXEL_MODE_MONO`,
//! `GRAY2` and `GRAY4` are expanded to Gray8. LCD and LCD_V pixel modes are
//! not converted and produce [`error::Error::UnsupportedPixelMode`].
//!
//! Color faces are always loaded with `FT_LOAD_COLOR` when available. COLRv0/
//! COLRv1 layer painting and SVG-in-OpenType are not implemented here: such
//! glyphs either render through FreeType's own bitmap support (BGRA) or produce
//! a valid empty bitmap/error if the installed FreeType cannot paint them.
//!
//! # Not supported
//!
//! * Glyph atlases, sprites, cell rendering and decoration drawing.
//! * COLR/CPAL layer compositing and SVG-in-OpenType painting (see above).
//! * kitty's `disk-cache.c` (font metadata caches), `font-names.c` OpenType
//! * kitty's `disk-cache.c` (font metadata caches), `font-names.c` OpenType
//!   name/STAT/fvar table readers, variable-font axis specialization, synthetic
//!   italic matrices, symbol maps, built-in Nerd Font, box/blank fonts and
//!   multicell/grapheme cell grouping.
//! * The full `shaped-run-cache.c` key format. [`shaping::ShapedRunCache`]
//!   provides equivalent limits and caching semantics but keys on the text and
//!   face identity instead of packed cell data.
//!
//! # FFI libraries
//!
//! The crate links `libfreetype`, `libharfbuzz` and `libfontconfig` directly
//! via `#[link]`; there is no build script and no `pkg-config` dependency. The
//! system headers are not required at build time because all declarations are
//! hand-written.
//!
//! # Example
//!
//! ```no_run
//! use kitty_fonts::{Face, ShapeOptions};
//!
//! let face = Face::from_path("FiraCode-Medium.otf", 0).unwrap();
//! let metrics = face.cell_metrics();
//! assert!(metrics.cell_width > 0 && metrics.cell_height > 0);
//! let run = face.shape(&['-' as u32, '>' as u32], &ShapeOptions::default()).unwrap();
//! assert_eq!(run.glyphs.len(), 2); // Fira Code's arrow ligature spans two cells
//! let bitmap = face.rasterize_codepoint('A' as u32).unwrap();
//! assert!(!bitmap.data.is_empty());
//! ```

pub mod discovery;
pub mod error;
pub mod face;
pub mod fallback;
pub mod family;
pub mod ffi;
pub mod library;
pub mod metrics;
pub mod raster;
pub mod shaping;

pub use discovery::{FontQuery, Fontconfig, MatchedFace};
pub use error::{Error, Result};

/// Alias used by rasterization callers: the crate's error type.
pub type FontError = Error;
pub use face::{Face, FaceDescriptor, DEFAULT_DPI, DEFAULT_SIZE_PTS};
pub use fallback::{FallbackRun, FontStack, ShapedText};
pub use family::{FontSpec, FontStyle};
pub use library::FreeTypeLibrary;
pub use metrics::{FaceMetrics, FontCellMetrics, GlyphMetrics};
pub use raster::{
    GlyphBitmap, PixelFormat, RasterCache, RASTER_CACHE_MAX_BYTES, RASTER_CACHE_MAX_ENTRIES,
};
pub use shaping::{
    shape, ClusterLevel, Feature, ShapeOptions, ShapedGlyph, ShapedRun, ShapedRunCache,
};
