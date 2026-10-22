// SPDX-License-Identifier: GPL-3.0-or-later
// Ported from kitty/modes.h, Copyright (C) 2016-2024 Kovid Goyal <kovid at kovidgoyal.net>

//! Screen mode constants and the `Modes` bit set.
//!
//! The numeric constants are the exact `DEC`/`xterm` mode numbers used by
//! `modes.h`. Private modes are shifted left by five before being passed to
//! [`Screen::set_mode`](crate::Screen::set_mode), exactly as `vt-parser.c`
//! does; the helper [`encoded`] performs that shift.

/// `Modes` is a compact set of the boolean screen modes. Mouse tracking and
/// protocol modes are stored separately by the screen because they are enums,
/// not booleans; the constants are still provided here for dispatch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Modes(pub u32);

impl Modes {
    pub const LNM: Self = Self(1 << 0);
    pub const IRM: Self = Self(1 << 1);
    pub const DECTCEM: Self = Self(1 << 2);
    pub const DECSCNM: Self = Self(1 << 3);
    pub const DECOM: Self = Self(1 << 4);
    pub const DECAWM: Self = Self(1 << 5);
    pub const DECCOLM: Self = Self(1 << 6);
    pub const DECARM: Self = Self(1 << 7);
    pub const DECCKM: Self = Self(1 << 8);
    pub const BRACKETED_PASTE: Self = Self(1 << 9);
    pub const FOCUS_TRACKING: Self = Self(1 << 10);

    #[inline]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    #[inline]
    pub fn insert(&mut self, other: Self) {
        self.0 |= other.0;
    }

    #[inline]
    pub fn remove(&mut self, other: Self) {
        self.0 &= !other.0;
    }

    #[inline]
    pub fn set(&mut self, other: Self, value: bool) {
        if value {
            self.insert(other);
        } else {
            self.remove(other);
        }
    }

    #[inline]
    pub fn get(self, other: Self) -> bool {
        self.contains(other)
    }
}

/// `LNM` (line feed/new line mode).
pub const LNM: u32 = 20;
/// `IRM` (insert/replace mode).
pub const IRM: u32 = 4;

pub const DECCKM: u32 = 1 << 5;
pub const DECCOLM: u32 = 3 << 5;
pub const DECSCLM: u32 = 4 << 5;
pub const DECSCNM: u32 = 5 << 5;
pub const DECARM: u32 = 8 << 5;
pub const DECOM: u32 = 6 << 5;
pub const DECAWM: u32 = 7 << 5;
pub const CONTROL_CURSOR_BLINK: u32 = 12 << 5;
pub const DECTCEM: u32 = 25 << 5;
pub const DECNRCM: u32 = 42 << 5;

pub const MOUSE_BUTTON_TRACKING: u32 = 1000 << 5;
pub const MOUSE_MOTION_TRACKING: u32 = 1002 << 5;
pub const MOUSE_MOVE_TRACKING: u32 = 1003 << 5;
pub const FOCUS_TRACKING: u32 = 1004 << 5;
pub const MOUSE_UTF8_MODE: u32 = 1005 << 5;
pub const MOUSE_SGR_MODE: u32 = 1006 << 5;
pub const MOUSE_URXVT_MODE: u32 = 1015 << 5;
pub const MOUSE_SGR_PIXEL_MODE: u32 = 1016 << 5;

pub const SAVE_CURSOR: u32 = 1048 << 5;
pub const TOGGLE_ALT_SCREEN_1: u32 = 47 << 5;
pub const TOGGLE_ALT_SCREEN_2: u32 = 1047 << 5;
pub const ALTERNATE_SCREEN: u32 = 1049 << 5;
pub const BRACKETED_PASTE: u32 = 2004 << 5;
pub const PENDING_UPDATE: u32 = 2026 << 5;
pub const COLOR_PREFERENCE_NOTIFICATION: u32 = 2031 << 5;
pub const VISIBILITY_REPORTS: u32 = 2033 << 5;
pub const INBAND_RESIZE_NOTIFICATION: u32 = 2048 << 5;
pub const PASTE_EVENTS: u32 = 5522 << 5;
pub const HANDLE_TERMIOS_SIGNALS: u32 = 19997 << 5;

/// Encode a mode number the way `handle_mode()` does for `CSI ? Pm h/l`.
#[inline]
pub const fn encoded(private: bool, mode: u32) -> u32 {
    if private {
        mode << 5
    } else {
        mode
    }
}
