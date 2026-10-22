// SPDX-License-Identifier: GPL-3.0-or-later
//! Palette and default colours built from `kitty-config`.

use kitty_config::{Color, Options};
use kitty_render::{BasicColorResolver, ColorResolver, Rgba8};

fn rgba(color: Color) -> Rgba8 {
    Rgba8::rgb(color.red, color.green, color.blue)
}

/// Colours from the configuration file and CLI defaults.
pub struct ConfigColorResolver {
    palette: [Rgba8; 256],
    fg: Rgba8,
    bg: Rgba8,
    cursor_bg: Rgba8,
    cursor_fg: Rgba8,
    selection_bg: Rgba8,
    selection_fg: Rgba8,
}

impl ConfigColorResolver {
    pub fn new(options: &Options) -> Self {
        let fg = rgba(options.foreground);
        let bg = rgba(options.background);
        let base = BasicColorResolver::new(fg, bg);
        let mut palette = *base.palette();
        for (index, color) in &options.colors {
            palette[*index as usize] = rgba(*color);
        }
        let cursor_bg = options.cursor.map(rgba).unwrap_or(fg);
        let cursor_fg = options.cursor_text_color.map(rgba).unwrap_or(bg);
        let selection_bg = options.selection_background.map(rgba).unwrap_or(fg);
        let selection_fg = options.selection_foreground.map(rgba).unwrap_or(bg);
        Self {
            palette,
            fg,
            bg,
            cursor_bg,
            cursor_fg,
            selection_bg,
            selection_fg,
        }
    }

    /// Set a palette entry (used by the remote `set-colors` command).
    pub fn set_palette_color(&mut self, index: u8, color: Rgba8) {
        self.palette[index as usize] = color;
    }

    /// Set one of the special colours by kitty.conf name.
    pub fn set_named_color(&mut self, name: &str, color: Rgba8) -> bool {
        match name {
            "foreground" => self.fg = color,
            "background" => self.bg = color,
            "cursor" => self.cursor_bg = color,
            "cursor_text_color" | "cursor_text" => self.cursor_fg = color,
            "selection_background" => self.selection_bg = color,
            "selection_foreground" => self.selection_fg = color,
            _ => return false,
        }
        true
    }

    /// Return the current colours as kitty.conf lines for `get-colors`.
    pub fn as_kitty_conf(&self) -> String {
        fn hex(c: Rgba8) -> String {
            format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b)
        }
        let mut out = String::new();
        out.push_str(&format!("foreground {}\n", hex(self.fg)));
        out.push_str(&format!("background {}\n", hex(self.bg)));
        out.push_str(&format!("cursor {}\n", hex(self.cursor_bg)));
        out.push_str(&format!("cursor_text_color {}\n", hex(self.cursor_fg)));
        out.push_str(&format!(
            "selection_background {}\n",
            hex(self.selection_bg)
        ));
        out.push_str(&format!(
            "selection_foreground {}\n",
            hex(self.selection_fg)
        ));
        for (index, color) in self.palette.iter().enumerate() {
            out.push_str(&format!("color{index} {}\n", hex(*color)));
        }
        out
    }
}

impl ColorResolver for ConfigColorResolver {
    fn palette_color(&self, index: u8) -> Rgba8 {
        self.palette[index as usize]
    }

    fn default_fg(&self) -> Rgba8 {
        self.fg
    }

    fn default_bg(&self) -> Rgba8 {
        self.bg
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
