// SPDX-License-Identifier: GPL-3.0-or-later
//! Colour encoding, gamma conversion and colour resolution.
//!
//! Cell colours use the same 32-bit encoding as kitty's `GPUCell`:
//!
//! * `0` means "not set" (use the default colour);
//! * `(index << 8) | 1` is an entry in the 256 colour palette;
//! * `(r << 24) | (g << 16) | (b << 8) | 2` is a 24-bit RGB colour.
//!
//! The low byte is a discriminator, mirroring `resolve_color()` in
//! `kitty/shaders/background.slang`. A fourth kind (`(kind << 8) | 3`) is
//! accepted for special colours that depend on the cell/context; the
//! [`ColorResolver`] decides what those mean.
//!
//! The sRGB transfer functions and the gamma LUT implement
//! `kitty/shaders/linear2srgb.slang`, and the LUT values match the generated
//! `kitty/srgb_gamma.h` table.

use std::sync::OnceLock;

/// A non-premultiplied 8-bit sRGB colour.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Rgba8 {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba8 {
    pub const TRANSPARENT: Self = Self::rgba(0, 0, 0, 0);
    pub const BLACK: Self = Self::rgb(0, 0, 0);
    pub const WHITE: Self = Self::rgb(255, 255, 255);

    #[inline]
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    #[inline]
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    /// Build from a packed `0xrrggbb` value.
    #[inline]
    pub const fn from_rgb_u32(rgb: u32) -> Self {
        Self::rgb(
            ((rgb >> 16) & 0xff) as u8,
            ((rgb >> 8) & 0xff) as u8,
            (rgb & 0xff) as u8,
        )
    }

    /// Pack as `0xrrggbb`.
    #[inline]
    pub const fn to_rgb_u32(self) -> u32 {
        ((self.r as u32) << 16) | ((self.g as u32) << 8) | self.b as u32
    }

    /// Pack as `0xaarrggbb`.
    #[inline]
    pub const fn to_argb_u32(self) -> u32 {
        ((self.a as u32) << 24) | self.to_rgb_u32()
    }

    /// Premultiply the colour channels by alpha using round-to-nearest, the
    /// same convention as kitty's `vec4_premul()`.
    #[inline]
    pub const fn premultiplied(self) -> Self {
        Self {
            r: premultiply_channel(self.r, self.a),
            g: premultiply_channel(self.g, self.a),
            b: premultiply_channel(self.b, self.a),
            a: self.a,
        }
    }

    /// Non-premultiplied components as `[r, g, b, a]` in the range 0..=1.
    #[inline]
    pub fn to_f32(self) -> [f32; 4] {
        [
            self.r as f32 / 255.0,
            self.g as f32 / 255.0,
            self.b as f32 / 255.0,
            self.a as f32 / 255.0,
        ]
    }

    /// Premultiplied components as `[r, g, b, a]` in the range 0..=1.
    #[inline]
    pub fn to_premultiplied_f32(self) -> [f32; 4] {
        let p = self.premultiplied();
        p.to_f32()
    }

    /// Convert to linear space and premultiply. This is what the shaders want
    /// for framebuffer blending in linear RGB.
    #[inline]
    pub fn to_linear_premultiplied_f32(self) -> [f32; 4] {
        let a = self.a as f32 / 255.0;
        [
            srgb_to_linear(self.r as f32 / 255.0) * a,
            srgb_to_linear(self.g as f32 / 255.0) * a,
            srgb_to_linear(self.b as f32 / 255.0) * a,
            a,
        ]
    }

    /// Mix `self` over `under` in linear RGB, returning an 8-bit sRGB colour.
    pub fn blend_over_linear(self, under: Self, opacity: f32) -> Self {
        let opacity = opacity.clamp(0.0, 1.0);
        if opacity >= 1.0 {
            return self;
        }
        let blend = |a: u8, b: u8| {
            let linear = (1.0 - opacity) * srgb_to_linear(b as f32 / 255.0)
                + opacity * srgb_to_linear(a as f32 / 255.0);
            linear_to_rgba8_channel(linear)
        };
        Self::rgb(
            blend(self.r, under.r),
            blend(self.g, under.g),
            blend(self.b, under.b),
        )
    }
}

#[inline]
const fn premultiply_channel(c: u8, a: u8) -> u8 {
    ((c as u32 * a as u32 + 127) / 255) as u8
}

#[inline]
fn linear_to_rgba8_channel(linear: f32) -> u8 {
    (linear_to_srgb(linear.clamp(0.0, 1.0)) * 255.0).round() as u8
}

/// sRGB to linear conversion, scalar form of `srgb2linear()` in
/// `linear2srgb.slang`.
#[inline]
pub fn srgb_to_linear(x: f32) -> f32 {
    if x <= 0.04045 {
        x / 12.92
    } else {
        ((x + 0.055) / 1.055).powf(2.4)
    }
}

/// Linear to sRGB conversion, scalar form of `linear2srgb()`.
#[inline]
pub fn linear_to_srgb(x: f32) -> f32 {
    if x <= 0.003_130_8 {
        12.92 * x
    } else {
        1.055 * x.powf(1.0 / 2.4) - 0.055
    }
}

/// Convert a packed 24-bit sRGB colour to linear components.
#[inline]
pub fn rgb_u32_to_linear(rgb: u32) -> [f32; 3] {
    let c = Rgba8::from_rgb_u32(rgb);
    [
        srgb_to_linear(c.r as f32 / 255.0),
        srgb_to_linear(c.g as f32 / 255.0),
        srgb_to_linear(c.b as f32 / 255.0),
    ]
}

/// The 256 entry sRGB-to-linear lookup table used by the shaders.
#[inline]
pub fn gamma_lut() -> &'static [f32; 256] {
    static LUT: OnceLock<[f32; 256]> = OnceLock::new();
    LUT.get_or_init(build_gamma_lut)
}

/// Build a fresh copy of the gamma LUT. Most callers should use
/// [`gamma_lut()`], which caches the table.
pub fn build_gamma_lut() -> [f32; 256] {
    let mut lut = [0.0f32; 256];
    for (i, value) in lut.iter_mut().enumerate() {
        *value = srgb_to_linear(i as f32 / 255.0);
    }
    lut
}

/// Look up an 8-bit sRGB component in the gamma LUT.
#[inline]
pub fn gamma_lut_lookup(component: u8) -> f32 {
    gamma_lut()[component as usize]
}

// {{{ Cell colour decoding

/// The kind of a packed cell colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CellColor {
    /// No colour was set, use the default.
    NotSet,
    /// An index into the 256 colour palette.
    Indexed(u8),
    /// A 24-bit RGB colour.
    Rgb(u8, u8, u8),
    /// A special colour whose meaning is supplied by the context.
    Special(u8),
}

/// Encode a palette colour exactly like kitty's SGR parser.
#[inline]
pub const fn encode_indexed(index: u8) -> u32 {
    ((index as u32) << 8) | 1
}

/// Encode a 24-bit RGB colour exactly like kitty's SGR parser.
#[inline]
pub const fn encode_rgb(r: u8, g: u8, b: u8) -> u32 {
    ((r as u32) << 24) | ((g as u32) << 16) | ((b as u32) << 8) | 2
}

/// Encode a context-dependent special colour.
#[inline]
pub const fn encode_special(kind: u8) -> u32 {
    ((kind as u32) << 8) | 3
}

/// Decode the SGR colour encoding used by `GPUCell`.
#[inline]
pub const fn decode_color(color: u32) -> CellColor {
    match color & 0xff {
        1 => CellColor::Indexed(((color >> 8) & 0xff) as u8),
        2 => CellColor::Rgb(
            ((color >> 24) & 0xff) as u8,
            ((color >> 16) & 0xff) as u8,
            ((color >> 8) & 0xff) as u8,
        ),
        3 => CellColor::Special(((color >> 8) & 0xff) as u8),
        _ => CellColor::NotSet,
    }
}

// }}}

// {{{ Colour resolution

/// Supplies the palette and default colours used to turn packed cell colours
/// into concrete RGBA values.
///
/// Only [`palette_color`](Self::palette_color), [`default_fg`](Self::default_fg)
/// and [`default_bg`](Self::default_bg) are required. The remaining methods
/// have kitty-compatible defaults and can be overridden by the host.
pub trait ColorResolver {
    /// Resolve an entry of the 256 colour palette.
    fn palette_color(&self, index: u8) -> Rgba8;

    /// The default foreground colour.
    fn default_fg(&self) -> Rgba8;

    /// The default background colour.
    fn default_bg(&self) -> Rgba8;

    /// Resolve a special colour. `kind` is the low-byte payload of
    /// [`encode_special`].
    fn special_color(&self, kind: u8) -> Rgba8 {
        match kind {
            1 => self.default_fg(),
            _ => self.default_bg(),
        }
    }

    /// The colour used to fill a block cursor.
    fn cursor_bg(&self) -> Rgba8 {
        self.default_fg()
    }

    /// The colour of text drawn under a block cursor.
    fn cursor_fg(&self) -> Rgba8 {
        self.default_bg()
    }

    /// The background of a selection.
    fn selection_bg(&self) -> Rgba8 {
        self.default_fg()
    }

    /// The foreground of selected text.
    fn selection_fg(&self) -> Rgba8 {
        self.default_bg()
    }

    /// The colour of hyperlinked text.
    fn url_color(&self) -> Rgba8 {
        self.palette_color(4)
    }

    /// Resolve a packed cell colour, falling back to `default` when it is not
    /// set.
    fn resolve(&self, color: u32, default: Rgba8) -> Rgba8 {
        match decode_color(color) {
            CellColor::NotSet => default,
            CellColor::Indexed(index) => self.palette_color(index),
            CellColor::Rgb(r, g, b) => Rgba8::rgb(r, g, b),
            CellColor::Special(kind) => self.special_color(kind),
        }
    }

    /// Resolve a packed colour to a premultiplied RGBA8 value.
    fn resolve_premultiplied(&self, color: u32, default: Rgba8) -> Rgba8 {
        self.resolve(color, default).premultiplied()
    }
}

/// The 16 default ANSI colours from `kitty/options/definition.py`
/// (`color0`..`color15`). These intentionally differ from the historic
/// xterm values in `kitty/colors.c`'s `FG_BG_256` fallback table: kitty's
/// defaults are `#cc0403` for red and `#f2201f` for bright red.
pub const KITTY_DEFAULT_ANSI_COLORS: [[u8; 3]; 16] = [
    [0x00, 0x00, 0x00], // color0 black
    [0xcc, 0x04, 0x03], // color1 red
    [0x19, 0xcb, 0x00], // color2 green
    [0xce, 0xcb, 0x00], // color3 yellow
    [0x0d, 0x73, 0xcc], // color4 blue
    [0xcb, 0x1e, 0xd1], // color5 magenta
    [0x0d, 0xcd, 0xcd], // color6 cyan
    [0xdd, 0xdd, 0xdd], // color7 white
    [0x76, 0x76, 0x76], // color8 bright black
    [0xf2, 0x20, 0x1f], // color9 bright red
    [0x23, 0xfd, 0x00], // color10 bright green
    [0xff, 0xfd, 0x00], // color11 bright yellow
    [0x1a, 0x8f, 0xff], // color12 bright blue
    [0xfd, 0x28, 0xff], // color13 bright magenta
    [0x14, 0xff, 0xff], // color14 bright cyan
    [0xff, 0xff, 0xff], // color15 bright white
];

/// The default 256-colour palette used when no `colorN` options are set.
///
/// Entries 0..=15 are [`KITTY_DEFAULT_ANSI_COLORS`]; 16..=231 are the fixed
/// 6x6x6 cube and 232..=255 are the grayscale ramp, matching
/// `init_FG_BG_table()` in `kitty/colors.c`.
pub fn kitty_default_palette() -> [Rgba8; 256] {
    let mut palette = [Rgba8::BLACK; 256];
    for (i, c) in KITTY_DEFAULT_ANSI_COLORS.iter().enumerate() {
        palette[i] = Rgba8::rgb(c[0], c[1], c[2]);
    }
    let levels = [0u8, 95, 135, 175, 215, 255];
    for r in 0..6 {
        for g in 0..6 {
            for b in 0..6 {
                palette[16 + r * 36 + g * 6 + b] = Rgba8::rgb(levels[r], levels[g], levels[b]);
            }
        }
    }
    for i in 0..24 {
        let v = (8 + i * 10) as u8;
        palette[232 + i] = Rgba8::rgb(v, v, v);
    }
    palette
}

/// A simple palette-based resolver, useful for tests, examples and as a
/// sensible default before the full `kitty/colors.c` profile is wired up.
#[derive(Clone, Debug)]
pub struct BasicColorResolver {
    palette: [Rgba8; 256],
    pub fg: Rgba8,
    pub bg: Rgba8,
    pub cursor_bg: Rgba8,
    pub cursor_fg: Rgba8,
    pub selection_bg: Rgba8,
    pub selection_fg: Rgba8,
    pub special: [Rgba8; 8],
}

impl Default for BasicColorResolver {
    fn default() -> Self {
        Self::new(Rgba8::rgb(0xdd, 0xdd, 0xdd), Rgba8::rgb(0x00, 0x00, 0x00))
    }
}

impl BasicColorResolver {
    pub fn new(fg: Rgba8, bg: Rgba8) -> Self {
        Self {
            palette: kitty_default_palette(),
            fg,
            bg,
            cursor_bg: Rgba8::rgb(0xcc, 0xcc, 0xcc),
            cursor_fg: Rgba8::rgb(0x11, 0x11, 0x11),
            selection_bg: Rgba8::rgb(0xff, 0xfa, 0xcd),
            selection_fg: Rgba8::rgb(0x00, 0x00, 0x00),
            special: [bg; 8],
        }
    }

    /// Replace the whole palette.
    pub fn with_palette(mut self, palette: [Rgba8; 256]) -> Self {
        self.palette = palette;
        self
    }

    /// Change one palette entry.
    pub fn set_palette_color(&mut self, index: u8, color: Rgba8) {
        self.palette[index as usize] = color;
    }

    pub fn palette(&self) -> &[Rgba8; 256] {
        &self.palette
    }
}

impl ColorResolver for BasicColorResolver {
    fn palette_color(&self, index: u8) -> Rgba8 {
        self.palette[index as usize]
    }

    fn default_fg(&self) -> Rgba8 {
        self.fg
    }

    fn default_bg(&self) -> Rgba8 {
        self.bg
    }

    fn special_color(&self, kind: u8) -> Rgba8 {
        self.special[kind as usize % self.special.len()]
    }

    fn cursor_bg(&self) -> Rgba8 {
        self.cursor_bg
    }

    fn cursor_fg(&self) -> Rgba8 {
        self.cursor_fg
    }

    fn selection_bg(&self) -> Rgba8 {
        self.selection_bg
    }

    fn selection_fg(&self) -> Rgba8 {
        self.selection_fg
    }
}

/// Blend the foreground towards the background in linear space, matching
/// `dimmed_color_component()` in `kitty/shaders.c`.
#[inline]
pub fn dimmed_color(fg: Rgba8, bg: Rgba8, opacity: f32) -> Rgba8 {
    if opacity >= 1.0 {
        return fg;
    }
    let opacity = opacity.max(0.0);
    let blend = |f: u8, b: u8| {
        let linear = (1.0 - opacity) * srgb_to_linear(b as f32 / 255.0)
            + opacity * srgb_to_linear(f as f32 / 255.0);
        linear_to_rgba8_channel(linear)
    };
    Rgba8::rgb(blend(fg.r, bg.r), blend(fg.g, bg.g), blend(fg.b, bg.b))
}

/// Relative luminance, the `dot(color, Y)` used by the shaders.
#[inline]
pub fn luminance(color: Rgba8) -> f32 {
    0.2126 * gamma_lut_lookup(color.r)
        + 0.7152 * gamma_lut_lookup(color.g)
        + 0.0722 * gamma_lut_lookup(color.b)
}

/// WCAG contrast ratio, clamped to `[1, 21]` like `contrast_ratio()`.
#[inline]
pub fn contrast_ratio(a: Rgba8, b: Rgba8) -> f32 {
    let la = luminance(a);
    let lb = luminance(b);
    ((la.max(lb) + 0.05) / (la.min(lb) + 0.05)).clamp(1.0, 21.0)
}

// }}}

/// Build an [`kitty_graphics::IndexedPalette`] from a colour resolver.
///
/// This is what the renderer uses to expand `f=8`/`f=16` graphics-protocol
/// images before uploading them. Entries `248..=255` follow the protocol's
/// special-colour table and use the resolver's context colours.
pub fn indexed_image_palette(resolver: &dyn ColorResolver) -> kitty_graphics::IndexedPalette {
    let mut colors = [[0u8, 0, 0, 255]; 256];
    for (index, entry) in colors.iter_mut().enumerate() {
        let color = resolver.palette_color(index as u8);
        *entry = [color.r, color.g, color.b, color.a];
    }
    let rgba = |color: Rgba8| [color.r, color.g, color.b, color.a];
    kitty_graphics::IndexedPalette::new(
        colors,
        rgba(resolver.default_bg()),
        rgba(resolver.default_fg()),
        rgba(resolver.selection_bg()),
        rgba(resolver.selection_fg()),
        rgba(resolver.cursor_bg()),
        rgba(resolver.cursor_fg()),
        rgba(resolver.url_color()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srgb_roundtrip_known_values() {
        assert_eq!(srgb_to_linear(0.0), 0.0);
        assert_eq!(srgb_to_linear(1.0), 1.0);
        assert!((srgb_to_linear(0.04045) - 0.003_130_8).abs() < 1e-6);
        assert!((linear_to_srgb(0.003_130_8) - 0.04045).abs() < 1e-5);
        for i in 0..=100 {
            let x = i as f32 / 100.0;
            let y = linear_to_srgb(srgb_to_linear(x));
            assert!((x - y).abs() < 2e-5, "{x} != {y}");
        }
    }

    #[test]
    fn default_palette_matches_kitty_ansi_table() {
        // Values from kitty/options/types.py's default `color_table` and
        // kitty/options/definition.py. This is deliberately not the xterm
        // table in colors.c (which is only the generated-color fallback).
        let c_table: [[u8; 3]; 16] = [
            [0x00, 0x00, 0x00],
            [0xcc, 0x04, 0x03],
            [0x19, 0xcb, 0x00],
            [0xce, 0xcb, 0x00],
            [0x0d, 0x73, 0xcc],
            [0xcb, 0x1e, 0xd1],
            [0x0d, 0xcd, 0xcd],
            [0xdd, 0xdd, 0xdd],
            [0x76, 0x76, 0x76],
            [0xf2, 0x20, 0x1f],
            [0x23, 0xfd, 0x00],
            [0xff, 0xfd, 0x00],
            [0x1a, 0x8f, 0xff],
            [0xfd, 0x28, 0xff],
            [0x14, 0xff, 0xff],
            [0xff, 0xff, 0xff],
        ];
        assert_eq!(KITTY_DEFAULT_ANSI_COLORS, c_table);
        let palette = kitty_default_palette();
        for (i, expected) in c_table.iter().enumerate() {
            assert_eq!(
                palette[i],
                Rgba8::rgb(expected[0], expected[1], expected[2]),
                "palette entry {i}"
            );
        }
        // 6x6x6 cube and grayscale ramp from init_FG_BG_table().
        assert_eq!(palette[16], Rgba8::rgb(0, 0, 0));
        assert_eq!(palette[21], Rgba8::rgb(0, 0, 255));
        assert_eq!(palette[196], Rgba8::rgb(255, 0, 0));
        assert_eq!(palette[231], Rgba8::rgb(255, 255, 255));
        assert_eq!(palette[232], Rgba8::rgb(8, 8, 8));
        assert_eq!(palette[255], Rgba8::rgb(238, 238, 238));
    }

    #[test]
    fn default_cursor_and_selection_colors_match_kitty() {
        let r = BasicColorResolver::default();
        assert_eq!(r.default_fg(), Rgba8::rgb(0xdd, 0xdd, 0xdd));
        assert_eq!(r.default_bg(), Rgba8::rgb(0x00, 0x00, 0x00));
        assert_eq!(r.cursor_bg(), Rgba8::rgb(0xcc, 0xcc, 0xcc));
        assert_eq!(r.cursor_fg(), Rgba8::rgb(0x11, 0x11, 0x11));
        assert_eq!(r.selection_bg(), Rgba8::rgb(0xff, 0xfa, 0xcd));
        assert_eq!(r.selection_fg(), Rgba8::rgb(0x00, 0x00, 0x00));
        assert_eq!(r.palette_color(1), Rgba8::rgb(0xcc, 0x04, 0x03));
        assert_eq!(r.palette_color(9), Rgba8::rgb(0xf2, 0x20, 0x1f));
    }

    #[test]
    fn gamma_lut_matches_definition() {
        let lut = gamma_lut();
        assert_eq!(lut[0], 0.0);
        assert_eq!(lut[255], 1.0);
        assert!((lut[1] - 0.000_303_53).abs() < 1e-7);
        assert!((lut[128] - 0.215_860_5).abs() < 1e-5);
        assert!(gamma_lut().as_ptr() == lut.as_ptr(), "LUT is cached");
    }

    #[test]
    fn premultiplication() {
        let c = Rgba8::rgba(255, 128, 0, 128);
        let p = c.premultiplied();
        assert_eq!(p, Rgba8::rgba(128, 64, 0, 128));
        assert_eq!(
            Rgba8::rgba(10, 20, 30, 255).premultiplied(),
            Rgba8::rgba(10, 20, 30, 255)
        );
        assert_eq!(
            Rgba8::rgba(10, 20, 30, 0).premultiplied(),
            Rgba8::rgba(0, 0, 0, 0)
        );
        let linear = c.to_linear_premultiplied_f32();
        assert!((linear[3] - 128.0 / 255.0).abs() < 1e-6);
        assert!((linear[0] - srgb_to_linear(1.0) * 128.0 / 255.0).abs() < 1e-6);
    }

    #[test]
    fn encode_decode() {
        assert_eq!(decode_color(0), CellColor::NotSet);
        assert_eq!(decode_color(encode_indexed(200)), CellColor::Indexed(200));
        assert_eq!(decode_color(encode_rgb(1, 2, 3)), CellColor::Rgb(1, 2, 3));
        assert_eq!(decode_color(encode_special(5)), CellColor::Special(5));
        assert_eq!(encode_rgb(0xaa, 0xbb, 0xcc), 0xaabb_cc02);
    }

    #[test]
    fn resolver_and_special_callback() {
        let mut r = BasicColorResolver::new(Rgba8::rgb(1, 2, 3), Rgba8::rgb(4, 5, 6));
        r.set_palette_color(200, Rgba8::rgb(7, 8, 9));
        assert_eq!(r.resolve(0, r.default_bg()), Rgba8::rgb(4, 5, 6));
        assert_eq!(
            r.resolve(encode_indexed(200), r.default_bg()),
            Rgba8::rgb(7, 8, 9)
        );
        assert_eq!(
            r.resolve(encode_rgb(11, 12, 13), r.default_bg()),
            Rgba8::rgb(11, 12, 13)
        );
        assert_eq!(r.resolve(encode_special(0), r.default_bg()), r.default_bg());
        assert_eq!(
            r.resolve_premultiplied(encode_rgb(255, 128, 0), Rgba8::BLACK),
            Rgba8::rgba(255, 128, 0, 255)
        );
    }

    #[test]
    fn dim_and_contrast() {
        let fg = Rgba8::rgb(255, 255, 255);
        let bg = Rgba8::rgb(0, 0, 0);
        assert_eq!(dimmed_color(fg, bg, 0.5), Rgba8::rgb(188, 188, 188));
        assert_eq!(dimmed_color(fg, bg, 1.0), fg);
        assert!((contrast_ratio(fg, bg) - 21.0).abs() < 1e-4);
        assert!((contrast_ratio(bg, bg) - 1.0).abs() < 1e-6);
    }
}
