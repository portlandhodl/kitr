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
