// SPDX-License-Identifier: GPL-3.0-or-later
// Ported from kitty/charsets.h and kitty/charsets.c
// Copyright (C) 2016-2024 Kovid Goyal <kovid at kovidgoyal.net>

//! G0/G1 charset designation and translation.
//!
//! The translation tables themselves live in `kitty-vt` (they are shared with
//! the parser crate); this module owns the small amount of screen state that
//! decides which table, if any, is active.

use kitty_vt::translation_table;

/// `CharsetState` from `screen.h`.
#[derive(Clone, Copy, Debug, Default)]
pub struct CharsetState {
    zero: Option<&'static [u32; 256]>,
    one: Option<&'static [u32; 256]>,
    pub current_num: u8,
}

impl CharsetState {
    #[inline]
    pub fn current(&self) -> Option<&'static [u32; 256]> {
        match self.current_num {
            0 => self.zero,
            1 => self.one,
            _ => None,
        }
    }

    /// `screen_change_charset()`.
    pub fn select(&mut self, which: u8) {
        if which <= 1 {
            self.current_num = which;
        }
    }

    /// `screen_designate_charset()`.
    pub fn designate(&mut self, which: u8, as_byte: u8) {
        match which {
            0 => self.zero = translation_table(as_byte),
            1 => self.one = translation_table(as_byte),
            _ => {}
        }
    }

    /// Reset to the default (no translation, G0 selected).
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// `map_char()`: apply the active translation table to byte-sized chars.
    #[inline]
    pub fn map_char(&self, ch: u32) -> u32 {
        match self.current() {
            Some(table) if ch < 256 => table[ch as usize],
            _ => ch,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn designates_and_selects() {
        let mut c = CharsetState::default();
        assert_eq!(c.map_char('q' as u32), 'q' as u32);
        c.designate(0, b'0');
        assert_eq!(c.map_char('q' as u32), 0x2500);
        c.designate(1, b'0');
        c.select(1);
        assert_eq!(c.map_char('q' as u32), 0x2500);
        c.select(0);
        assert_eq!(c.map_char('q' as u32), 0x2500);
        c.designate(0, b'B');
        assert_eq!(c.map_char('q' as u32), 'q' as u32);
        c.reset();
        assert_eq!(c.map_char('q' as u32), 'q' as u32);
    }

    #[test]
    fn uk_charset() {
        let mut c = CharsetState::default();
        c.designate(0, b'A');
        assert_eq!(c.map_char('#' as u32), 0xa3);
    }
}
