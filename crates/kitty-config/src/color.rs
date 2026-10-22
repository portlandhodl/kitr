// SPDX-License-Identifier: GPL-3.0-or-later
//! Color parsing compatible with `kitty/colors.c`'s `Color.parse_color` for
//! named colors, `#rgb`/`#rrggbb` (and the wider 9/12 digit forms), and
//! `rgb:r/g/b` / `rgbi:r/g/b`. The named-color table is generated from
//! `gen/color_names.py` (Copyright (C) 2025 Kovid Goyal).

use std::fmt;

use crate::color_names;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Color {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
    /// kitty's Color stores an alpha byte; parsed config colors always use 0.
    pub alpha: u8,
}

impl Color {
    pub const fn new(red: u8, green: u8, blue: u8) -> Self {
        Self {
            red,
            green,
            blue,
            alpha: 0,
        }
    }

    pub const fn rgba(red: u8, green: u8, blue: u8, alpha: u8) -> Self {
        Self {
            red,
            green,
            blue,
            alpha,
        }
    }

    pub const fn from_rgb_int(value: u32) -> Self {
        Self::new(
            ((value >> 16) & 0xff) as u8,
            ((value >> 8) & 0xff) as u8,
            (value & 0xff) as u8,
        )
    }

    pub const fn as_rgb_int(self) -> u32 {
        ((self.red as u32) << 16) | ((self.green as u32) << 8) | self.blue as u32
    }

    /// Equivalent to `Color.parse_color()`: returns `None` for invalid input.
    pub fn parse(spec: &str) -> Option<Self> {
        let lowered = spec.to_lowercase();
        let mut spec = lowered.trim();
        if spec.len() < 2 {
            return None;
        }

        // Remove trailing comments exactly like the C implementation.
        if spec.starts_with('#') {
            if let Some(idx) = spec.find(' ') {
                spec = &spec[..idx];
            }
        } else if let Some(idx) = spec.find('#') {
            spec = spec[..idx].trim();
        }

        if let Some(value) = color_names::lookup(spec) {
            return Some(Self::from_rgb_int(value));
        }
        if spec.len() < 4 {
            return None;
        }

        let bytes = spec.as_bytes();
        match bytes[0] {
            b'#' => parse_sharp(&spec[1..]),
            b'r' => {
                if bytes.get(1) != Some(&b'g') || bytes.get(2) != Some(&b'b') || spec.len() < 6 {
                    return None;
                }
                match bytes.get(3) {
                    Some(b':') => parse_rgb(&spec[4..]),
                    Some(b'i') if bytes.get(4) == Some(&b':') => parse_rgbi(&spec[5..]),
                    _ => None,
                }
            }
            // oklch()/lab() are intentionally not implemented in this
            // working subset; see the crate documentation.
            _ => None,
        }
    }

    /// Parse or report the same message `kitty.conf.utils.to_color()` uses.
    pub fn parse_required(spec: &str) -> Result<Self, String> {
        Self::parse(spec).ok_or_else(|| format!("Invalid color name: {spec:?}"))
    }

    pub fn as_sharp(self) -> String {
        if self.alpha == 0 {
            format!("#{:02x}{:02x}{:02x}", self.red, self.green, self.blue)
        } else {
            format!(
                "#{:02x}{:02x}{:02x}{:02x}",
                self.alpha, self.red, self.green, self.blue
            )
        }
    }

    pub fn as_sgr(self) -> String {
        if self.alpha == 0 {
            format!(":2:{}:{}:{}", self.red, self.green, self.blue)
        } else {
            format!(
                ":2:{}:{}:{}:{}",
                self.red, self.green, self.blue, self.alpha
            )
        }
    }
}

impl fmt::Display for Color {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.as_sharp())
    }
}

fn parse_sharp(spec: &str) -> Option<Color> {
    let bytes = spec.as_bytes();
    let (r, g, b) = match bytes.len() {
        3 => (
            parse_color_bytes(&bytes[0..1])?,
            parse_color_bytes(&bytes[1..2])?,
            parse_color_bytes(&bytes[2..3])?,
        ),
        6 | 9 | 12 => {
            // The C implementation always reads a two-byte component from
            // offsets 0, len/3 and 2*len/3.
            let n = bytes.len() / 3;
            (
                parse_color_bytes(&bytes[..2])?,
                parse_color_bytes(&bytes[n..])?,
                parse_color_bytes(&bytes[2 * n..])?,
            )
        }
        _ => return None,
    };
    Some(Color::new(r, g, b))
}

fn parse_color_bytes(bytes: &[u8]) -> Option<u8> {
    if bytes.is_empty() {
        return None;
    }
    let hi = hexchar_to_int(bytes[0])?;
    let lo = if bytes.len() == 1 {
        hexchar_to_int(bytes[0])
    } else {
        hexchar_to_int(bytes[1])
    }?;
    Some((hi << 4) | lo)
}

fn parse_rgb(spec: &str) -> Option<Color> {
    if spec.len() >= 32 {
        return None;
    }
    let mut tokens = spec.split('/').filter(|x| !x.is_empty());
    let r = parse_single_color(tokens.next()?)?;
    let g = parse_single_color(tokens.next()?)?;
    let b = parse_single_color(tokens.next()?)?;
    Some(Color::new(r, g, b))
}

fn parse_rgbi(spec: &str) -> Option<Color> {
    if spec.len() >= 256 {
        return None;
    }
    let mut tokens = spec.split('/').filter(|x| !x.is_empty());
    let r = parse_single_intensity(tokens.next()?)?;
    let g = parse_single_intensity(tokens.next()?)?;
    let b = parse_single_intensity(tokens.next()?)?;
    Some(Color::new(r, g, b))
}

fn parse_single_color(token: &str) -> Option<u8> {
    parse_color_bytes(token.as_bytes())
}

fn parse_single_intensity(token: &str) -> Option<u8> {
    let value: f64 = token.parse().ok()?;
    if value.is_nan() {
        return None;
    }
    Some(as8bit(value.clamp(0.0, 1.0)))
}

fn as8bit(value: f64) -> u8 {
    (value * 255.0).round() as u8
}

fn hexchar_to_int(ch: u8) -> Option<u8> {
    match ch {
        b'0'..=b'9' => Some(ch - b'0'),
        b'a'..=b'f' => Some(ch - b'a' + 10),
        b'A'..=b'F' => Some(ch - b'A' + 10),
        _ => None,
    }
}
