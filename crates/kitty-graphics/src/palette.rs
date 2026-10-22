// SPDX-License-Identifier: GPL-3.0-or-later
//! Palette handling for indexed-colour graphics (`f=8` and `f=16`).
//!
//! Kitty stores indexed images as integer textures and performs the palette
//! lookup in the shader, so the image automatically follows terminal colour
//! changes. The CPU side still needs an explicit palette to expand such images
//! for backends (and for testing), which is what this module provides.
//!
//! Indices `0..=247` use the SGR 256-colour table. The special entries match
//! `docs/graphics-protocol.rst`:
//!
//! | Index | Colour |
//! |-------|--------|
//! | 248 | default background |
//! | 249 | default foreground |
//! | 250 | selection background |
//! | 251 | selection foreground |
//! | 252 | cursor colour |
//! | 253 | cursor text colour |
//! | 254 | URL colour |
//! | 255 | reserved, currently the default foreground |

use crate::image::{GraphicsError, Image, ImageFormat};

/// An RGBA8 palette with kitty's special-colour mapping.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IndexedPalette {
    entries: [[u8; 4]; 256],
}

impl IndexedPalette {
    /// Build a palette from 256 explicit RGBA entries. Entries 248..255 are
    /// used verbatim.
    pub const fn from_rgba(entries: [[u8; 4]; 256]) -> Self {
        Self { entries }
    }

    /// Build a palette from the 256 SGR colours plus terminal context colours.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        colors: [[u8; 4]; 256],
        default_bg: [u8; 4],
        default_fg: [u8; 4],
        selection_bg: [u8; 4],
        selection_fg: [u8; 4],
        cursor_bg: [u8; 4],
        cursor_fg: [u8; 4],
        url: [u8; 4],
    ) -> Self {
        let mut entries = colors;
        entries[248] = opaque(default_bg);
        entries[249] = opaque(default_fg);
        entries[250] = opaque(selection_bg);
        entries[251] = opaque(selection_fg);
        entries[252] = opaque(cursor_bg);
        entries[253] = opaque(cursor_fg);
        entries[254] = opaque(url);
        entries[255] = opaque(default_fg);
        Self { entries }
    }

    /// The xterm/SGR palette with the given default colours. Special colours
    /// fall back to the defaults in the same way as `ColorResolver`.
    pub fn xterm(default_fg: [u8; 4], default_bg: [u8; 4]) -> Self {
        let mut entries = [[0u8, 0, 0, 255]; 256];
        let base: [[u8; 3]; 16] = [
            [0x00, 0x00, 0x00],
            [0xcd, 0x00, 0x00],
            [0x00, 0xcd, 0x00],
            [0xcd, 0xcd, 0x00],
            [0x00, 0x00, 0xee],
            [0xcd, 0x00, 0xcd],
            [0x00, 0xcd, 0xcd],
            [0xe5, 0xe5, 0xe5],
            [0x7f, 0x7f, 0x7f],
            [0xff, 0x00, 0x00],
            [0x00, 0xff, 0x00],
            [0xff, 0xff, 0x00],
            [0x5c, 0x5c, 0xff],
            [0xff, 0x00, 0xff],
            [0x00, 0xff, 0xff],
            [0xff, 0xff, 0xff],
        ];
        for (index, color) in base.iter().enumerate() {
            entries[index] = [color[0], color[1], color[2], 255];
        }
        let levels = [0u8, 95, 135, 175, 215, 255];
        for r in 0..6 {
            for g in 0..6 {
                for b in 0..6 {
                    entries[16 + r * 36 + g * 6 + b] = [levels[r], levels[g], levels[b], 255];
                }
            }
        }
        for i in 0..24 {
            let value = (8 + i * 10) as u8;
            entries[232 + i] = [value, value, value, 255];
        }
        entries[248] = opaque(default_bg);
        entries[249] = opaque(default_fg);
        entries[250] = opaque(default_bg);
        entries[251] = opaque(default_fg);
        entries[252] = opaque(default_fg);
        entries[253] = opaque(default_bg);
        entries[254] = [0x5c, 0x5c, 0xff, 255];
        entries[255] = opaque(default_fg);
        Self { entries }
    }

    /// The RGBA colour for one palette index.
    #[inline]
    pub fn color(&self, index: u8) -> [u8; 4] {
        self.entries[index as usize]
    }

    /// All entries.
    pub fn entries(&self) -> &[[u8; 4]; 256] {
        &self.entries
    }

    /// Expand raw indexed samples (`f=8` or `f=16`) to tightly packed RGBA8.
    ///
    /// For `f=16`, the second byte of each pixel is its opacity and replaces
    /// the palette entry's alpha channel.
    pub fn expand(&self, raw: &[u8], format: ImageFormat, width: u32, height: u32) -> Vec<u8> {
        let pixels = width as usize * height as usize;
        let mut out = Vec::with_capacity(pixels * 4);
        match format {
            ImageFormat::Indexed => {
                for &index in raw.iter().take(pixels) {
                    out.extend_from_slice(&self.entries[index as usize]);
                }
            }
            ImageFormat::IndexedAlpha => {
                for index in 0..pixels {
                    let mut color = self.entries[raw[index * 2] as usize];
                    color[3] = raw[index * 2 + 1];
                    out.extend_from_slice(&color);
                }
            }
            _ => return out,
        }
        out
    }

    /// Expand an indexed [`Image`] using this palette.
    pub fn expand_image(&self, image: &Image) -> Result<Vec<u8>, GraphicsError> {
        if !image.format.is_indexed() {
            return Err(GraphicsError::UnknownFormat(image.format.as_u32()));
        }
        Ok(self.expand(&image.raw, image.format, image.width, image.height))
    }
}

impl Default for IndexedPalette {
    fn default() -> Self {
        Self::xterm([0xdd, 0xdd, 0xdd, 255], [0x11, 0x11, 0x11, 255])
    }
}

#[inline]
const fn opaque(color: [u8; 4]) -> [u8; 4] {
    [color[0], color[1], color[2], 255]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{decode_image, parse_raw, ImageLimits};

    #[test]
    fn expands_opaque_and_alpha_indexed_pixels() {
        let mut palette = IndexedPalette::default();
        let mut colors = *palette.entries();
        colors[1] = [1, 2, 3, 255];
        colors[2] = [4, 5, 6, 255];
        palette = IndexedPalette::from_rgba(colors);

        let opaque = palette.expand(&[1, 2], ImageFormat::Indexed, 2, 1);
        assert_eq!(opaque, vec![1, 2, 3, 255, 4, 5, 6, 255]);
        let alpha = palette.expand(&[1, 0, 2, 128], ImageFormat::IndexedAlpha, 2, 1);
        assert_eq!(alpha, vec![1, 2, 3, 0, 4, 5, 6, 128]);
    }

    #[test]
    fn special_indices_use_context_colours() {
        let palette = IndexedPalette::new(
            [[0; 4]; 256],
            [10, 11, 12, 1],
            [20, 21, 22, 1],
            [30, 31, 32, 1],
            [40, 41, 42, 1],
            [50, 51, 52, 1],
            [60, 61, 62, 1],
            [70, 71, 72, 1],
        );
        assert_eq!(palette.color(248), [10, 11, 12, 255]);
        assert_eq!(palette.color(249), [20, 21, 22, 255]);
        assert_eq!(palette.color(250), [30, 31, 32, 255]);
        assert_eq!(palette.color(251), [40, 41, 42, 255]);
        assert_eq!(palette.color(252), [50, 51, 52, 255]);
        assert_eq!(palette.color(253), [60, 61, 62, 255]);
        assert_eq!(palette.color(254), [70, 71, 72, 255]);
        assert_eq!(palette.color(255), [20, 21, 22, 255]);
    }

    #[test]
    fn expands_a_decoded_indexed_image() {
        let mut command = parse_raw(b"a=t,f=16,s=2,v=1,i=1").unwrap();
        command.payload = vec![1, 128, 2, 255];
        let image = decode_image(&command, &command.payload, &ImageLimits::default()).unwrap();
        let palette = IndexedPalette::default();
        let rgba = palette.expand_image(&image).unwrap();
        assert_eq!(rgba.len(), 8);
        assert_eq!(
            &rgba[0..4],
            &palette.color(1)[..3]
                .iter()
                .copied()
                .chain([128])
                .collect::<Vec<_>>()[..]
        );
        assert_eq!(rgba[7], 255);
    }
}
