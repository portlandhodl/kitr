// SPDX-License-Identifier: GPL-3.0-or-later
// Dynamic colour overrides and the OSC 4/10/11/12/104/110/111/112 handling
// from kitty/screen.c and kitty/window.py.
// Copyright (C) 2016-2024 Kovid Goyal <kovid at kovidgoyal.net>

//! Runtime colour overrides.
//!
//! Kitty keeps a `ColorProfile` with configured and overridden colours. This
//! module implements the escape-code driven subset: default foreground,
//! background and cursor colour, the selection colours (OSC 17/19), the 256
//! colour palette (OSC 4/104), plus a small colour stack for CSI `#P`.

/// An 8-bit RGB colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Parse `#rgb`, `#rrggbb`, `rgb:r/g/b`, `rgb:rr/gg/bb` or
    /// `rgb:rrrr/gggg/bbbb`.
    pub fn parse(spec: &str) -> Option<Self> {
        let spec = spec.trim();
        if spec.is_empty() || spec == "?" {
            return None;
        }
        if let Some(hex) = spec.strip_prefix('#') {
            return match hex.len() {
                3 => {
                    let r = u8::from_str_radix(&hex[0..1], 16).ok()?;
                    let g = u8::from_str_radix(&hex[1..2], 16).ok()?;
                    let b = u8::from_str_radix(&hex[2..3], 16).ok()?;
                    Some(Self::new(r * 17, g * 17, b * 17))
                }
                6 => {
                    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
                    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
                    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
                    Some(Self::new(r, g, b))
                }
                _ => None,
            };
        }
        if let Some(rest) = spec.strip_prefix("rgb:") {
            let mut parts = rest.split('/');
            let r = parts.next()?;
            let g = parts.next()?;
            let b = parts.next()?;
            if parts.next().is_some() {
                return None;
            }
            let scale = |s: &str| -> Option<u8> {
                let v = u32::from_str_radix(s, 16).ok()?;
                let max = if s.len() == 1 {
                    0xf
                } else if s.len() == 2 {
                    0xff
                } else if s.len() == 3 {
                    0xfff
                } else if s.len() == 4 {
                    0xffff
                } else {
                    return None;
                };
                Some(((v * 255 + max / 2) / max) as u8)
            };
            return Some(Self::new(scale(r)?, scale(g)?, scale(b)?));
        }
        None
    }

    /// Serialize as the 16-bit-per-channel form kitty reports.
    pub fn as_osc_rgb(self) -> String {
        let r = self.r as u16 * 257;
        let g = self.g as u16 * 257;
        let b = self.b as u16 * 257;
        format!("rgb:{r:04x}/{g:04x}/{b:04x}")
    }
}

/// Overridden colours. `None` means "use the configured/default colour".
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ColorOverrides {
    pub foreground: Option<Rgb>,
    pub background: Option<Rgb>,
    pub cursor: Option<Rgb>,
    pub cursor_text: Option<Rgb>,
    pub selection_background: Option<Rgb>,
    pub selection_foreground: Option<Rgb>,
    pub palette: Vec<Option<Rgb>>,
}

impl ColorOverrides {
    pub fn new() -> Self {
        Self {
            palette: vec![None; 256],
            ..Default::default()
        }
    }

    pub fn reset_dynamic(&mut self, code: u32) {
        match code {
            10 | 110 => self.foreground = None,
            11 | 111 => self.background = None,
            12 | 112 => self.cursor = None,
            17 | 117 => self.selection_background = None,
            19 | 119 => self.selection_foreground = None,
            _ => {}
        }
    }

    pub fn reset_palette(&mut self) {
        self.palette.iter_mut().for_each(|x| *x = None);
    }

    pub fn palette_color(&self, index: u8) -> Option<Rgb> {
        self.palette.get(index as usize).copied().flatten()
    }
}

/// Outcome of handling an OSC colour sequence.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ColorChange {
    /// True if any override was changed/reset.
    pub dirtied: bool,
    /// True if the default background changed.
    pub default_bg_changed: bool,
}

/// A small colour stack used by CSI `#P` / `#Q`.
#[derive(Clone, Debug, Default)]
pub struct ColorStack {
    stack: Vec<ColorOverrides>,
}

impl ColorStack {
    pub fn push(&mut self, colors: ColorOverrides) {
        self.stack.push(colors);
    }

    pub fn pop(&mut self) -> Option<ColorOverrides> {
        self.stack.pop()
    }

    pub fn len(&self) -> usize {
        self.stack.len()
    }

    pub fn is_empty(&self) -> bool {
        self.stack.is_empty()
    }

    pub fn clear(&mut self) {
        self.stack.clear();
    }
}

impl crate::screen::Screen {
    /// Read the effective override for a dynamic colour code.
    pub fn dynamic_color(&self, code: u32) -> Option<Rgb> {
        match code {
            10 => self.colors.foreground,
            11 => self.colors.background,
            12 => self.colors.cursor,
            13 => self.colors.cursor_text,
            17 => self.colors.selection_background,
            19 => self.colors.selection_foreground,
            _ => None,
        }
    }

    /// Read the effective override for a palette index.
    pub fn palette_color(&self, index: u8) -> Option<Rgb> {
        self.colors.palette_color(index)
    }

    /// Append bytes to the response queue (normally sent to the child).
    pub fn write_output(&mut self, bytes: &[u8]) {
        self.output.extend_from_slice(bytes);
    }

    /// Drain the response queue.
    pub fn take_output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.output)
    }

    /// Peek at the response queue.
    pub fn pending_output(&self) -> &[u8] {
        &self.output
    }

    /// Emit `OSC <code>;rgb:rrrr/gggg/bbbb ST`.
    pub fn report_color(&mut self, code: &str, color: Rgb) {
        let seq = format!("\x1b]{};{}\x1b\\", code, color.as_osc_rgb());
        self.write_output(seq.as_bytes());
    }

    fn set_dynamic_color_value(&mut self, code: u32, value: &str) -> ColorChange {
        let mut change = ColorChange::default();
        if code >= 100 || value.is_empty() {
            self.colors.reset_dynamic(code);
            change.dirtied = true;
            change.default_bg_changed = matches!(code, 11 | 111);
            return change;
        }
        let Some(color) = Rgb::parse(value) else {
            return change;
        };
        let slot = match code {
            10 => &mut self.colors.foreground,
            11 => &mut self.colors.background,
            12 => &mut self.colors.cursor,
            13 => &mut self.colors.cursor_text,
            17 => &mut self.colors.selection_background,
            19 => &mut self.colors.selection_foreground,
            _ => return change,
        };
        *slot = Some(color);
        change.dirtied = true;
        change.default_bg_changed = code == 11;
        change
    }

    /// Handle OSC 10/11/12/13/17/19 (and their +100 reset forms).
    pub fn handle_dynamic_color_osc(&mut self, code: u32, payload: &str) -> ColorChange {
        let mut change = ColorChange::default();
        for (c, value) in (code..).zip(payload.split(';')) {
            if !matches!(
                c,
                10 | 11 | 12 | 13 | 17 | 19 | 110 | 111 | 112 | 113 | 117 | 119
            ) {
                break;
            }
            if c < 100 && value == "?" {
                let col = self.dynamic_color(c).unwrap_or_default();
                self.report_color(&c.to_string(), col);
            } else {
                let ch = self.set_dynamic_color_value(c, value);
                change.dirtied |= ch.dirtied;
                change.default_bg_changed |= ch.default_bg_changed;
            }
        }
        change
    }

    /// Handle OSC 4 (set/query palette entries) and OSC 104 (reset).
    pub fn handle_palette_osc(&mut self, code: u32, payload: &str) -> ColorChange {
        let mut change = ColorChange::default();
        if code == 104 {
            if payload.trim().is_empty() {
                self.colors.reset_palette();
                change.dirtied = true;
            } else {
                for part in payload.split(';') {
                    if let Ok(idx) = part.trim().parse::<u8>() {
                        if let Some(slot) = self.colors.palette.get_mut(idx as usize) {
                            *slot = None;
                            change.dirtied = true;
                        }
                    }
                }
            }
            return change;
        }
        let parts: Vec<&str> = payload.split(';').collect();
        let mut i = 0usize;
        while i + 1 < parts.len() {
            let Ok(idx) = parts[i].trim().parse::<u8>() else {
                i += 2;
                continue;
            };
            let spec = parts[i + 1];
            if spec == "?" {
                let col = self.palette_color(idx).unwrap_or_default();
                self.report_color(&format!("4;{idx}"), col);
            } else if spec.is_empty() {
                if let Some(slot) = self.colors.palette.get_mut(idx as usize) {
                    *slot = None;
                    change.dirtied = true;
                }
            } else if let Some(col) = Rgb::parse(spec) {
                if let Some(slot) = self.colors.palette.get_mut(idx as usize) {
                    *slot = Some(col);
                    change.dirtied = true;
                }
            }
            i += 2;
        }
        change
    }

    /// Handle any colour-related OSC code. Returns false if unknown.
    pub fn handle_color_osc(&mut self, code: u32, payload: &str) -> bool {
        match code {
            4 | 104 => {
                self.handle_palette_osc(code, payload);
                true
            }
            10 | 11 | 12 | 13 | 17 | 19 | 110 | 111 | 112 | 113 | 117 | 119 => {
                self.handle_dynamic_color_osc(code, payload);
                true
            }
            _ => false,
        }
    }

    /// Push the current overrides onto the colour stack (`CSI #P`).
    pub fn push_colors(&mut self) {
        self.color_stack.push(self.colors.clone());
    }

    /// Pop the colour stack (`CSI #Q`). Returns true if the background changed.
    pub fn pop_colors(&mut self) -> bool {
        let old_bg = self.colors.background;
        if let Some(colors) = self.color_stack.pop() {
            self.colors = colors;
            self.is_dirty = true;
            old_bg != self.colors.background
        } else {
            false
        }
    }

    pub fn color_stack_len(&self) -> usize {
        self.color_stack.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hex_and_rgb_forms() {
        assert_eq!(Rgb::parse("#f00"), Some(Rgb::new(255, 0, 0)));
        assert_eq!(Rgb::parse("#00ff00"), Some(Rgb::new(0, 255, 0)));
        assert_eq!(Rgb::parse("rgb:0/80/ffff"), Some(Rgb::new(0, 0x80, 255)));
        assert_eq!(
            Rgb::parse("rgb:ffff/0000/8080"),
            Some(Rgb::new(255, 0, 0x80))
        );
        assert_eq!(Rgb::parse("?"), None);
    }

    #[test]
    fn osc_rgb_is_16_bit() {
        assert_eq!(Rgb::new(1, 2, 3).as_osc_rgb(), "rgb:0101/0202/0303");
    }
}

#[cfg(test)]
mod screen_tests {
    use super::*;
    use crate::handler::ScreenHandler;
    use crate::screen::Screen;
    use kitty_vt::Parser;

    fn handler() -> ScreenHandler {
        ScreenHandler::new(Screen::builder(4, 2).scrollback(4).build().unwrap())
    }

    fn feed(h: &mut ScreenHandler, bytes: &[u8]) {
        h.feed(&mut Parser::new(), bytes);
    }

    #[test]
    fn dynamic_colors_override_query_and_reset() {
        let mut h = handler();
        feed(&mut h, b"\x1b]11;#ff0000\x1b\\");
        assert_eq!(h.screen.dynamic_color(11), Some(Rgb::new(255, 0, 0)));
        h.screen.take_output();
        feed(&mut h, b"\x1b]11;?\x1b\\");
        let out = String::from_utf8(h.screen.take_output()).unwrap();
        assert_eq!(out, "\x1b]11;rgb:ffff/0000/0000\x1b\\");
        feed(&mut h, b"\x1b]111\x1b\\");
        assert_eq!(h.screen.dynamic_color(11), None);
        feed(&mut h, b"\x1b]10;#00ff00\x1b\\");
        assert_eq!(h.screen.dynamic_color(10), Some(Rgb::new(0, 255, 0)));
        feed(&mut h, b"\x1b]110\x1b\\");
        assert_eq!(h.screen.dynamic_color(10), None);
    }

    #[test]
    fn palette_osc4_query_and_osc104_reset() {
        let mut h = handler();
        feed(&mut h, b"\x1b]4;2;#102030\x1b\\");
        assert_eq!(h.screen.palette_color(2), Some(Rgb::new(0x10, 0x20, 0x30)));
        h.screen.take_output();
        feed(&mut h, b"\x1b]4;2;?\x1b\\");
        assert_eq!(
            String::from_utf8(h.screen.take_output()).unwrap(),
            "\x1b]4;2;rgb:1010/2020/3030\x1b\\"
        );
        feed(&mut h, b"\x1b]104;2\x1b\\");
        assert_eq!(h.screen.palette_color(2), None);
        feed(&mut h, b"\x1b]4;1;#ffffff\x1b\\\x1b]104\x1b\\");
        assert_eq!(h.screen.palette_color(1), None);
    }

    #[test]
    fn color_stack_csi_push_pop() {
        let mut h = handler();
        feed(&mut h, b"\x1b[#P\x1b]11;#123456\x1b\\");
        assert_eq!(h.screen.dynamic_color(11), Some(Rgb::new(0x12, 0x34, 0x56)));
        assert_eq!(h.screen.color_stack_len(), 1);
        feed(&mut h, b"\x1b[#Q");
        assert_eq!(h.screen.dynamic_color(11), None);
        assert_eq!(h.screen.color_stack_len(), 0);
    }
}
