// SPDX-License-Identifier: GPL-3.0-or-later
//! Key events and modifier bit flags, mirroring `PyKeyEvent` in `keys.c` and
//! the modifier masks used throughout kitty.

use core::ops::{BitAnd, BitAndAssign, BitOr, BitOrAssign, Not, Sub, SubAssign};

use crate::key_constants::*;
use crate::key_names::is_modifier_key;

/// GLFW-compatible modifier bits.
///
/// The first eight bits are identical to `GLFW_MOD_*` in `kitty/data-types.h`.
/// [`Modifiers::KITTY`] is `GLFW_MOD_KITTY`, used only while parsing config
/// shortcuts to mark the `kitty_mod` placeholder.
#[derive(Copy, Clone, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(transparent)]
pub struct Modifiers(pub u32);

impl Modifiers {
    pub const NONE: Self = Self(0);
    pub const SHIFT: Self = Self(GLFW_MOD_SHIFT);
    pub const ALT: Self = Self(GLFW_MOD_ALT);
    pub const CTRL: Self = Self(GLFW_MOD_CONTROL);
    pub const CONTROL: Self = Self(GLFW_MOD_CONTROL);
    pub const SUPER: Self = Self(GLFW_MOD_SUPER);
    pub const HYPER: Self = Self(GLFW_MOD_HYPER);
    pub const META: Self = Self(GLFW_MOD_META);
    pub const CAPS_LOCK: Self = Self(GLFW_MOD_CAPS_LOCK);
    pub const NUM_LOCK: Self = Self(GLFW_MOD_NUM_LOCK);
    pub const LOCKS: Self = Self(GLFW_LOCK_MASK);
    pub const KITTY: Self = Self(GLFW_MOD_KITTY);

    #[inline(always)]
    pub const fn from_bits_truncate(bits: u32) -> Self {
        Self(bits)
    }

    #[inline(always)]
    pub const fn bits(self) -> u32 {
        self.0
    }

    #[inline(always)]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    #[inline(always)]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    #[inline(always)]
    pub const fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }

    #[inline(always)]
    pub const fn without(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    /// Drop Caps Lock and Num Lock, as `mods_without_locks` does in kitty.
    #[inline(always)]
    pub const fn without_locks(self) -> Self {
        Self(self.0 & !GLFW_LOCK_MASK)
    }

    /// The value encoded as the second CSI field (`mods.value + 1`).
    #[inline(always)]
    pub const fn protocol_value(self) -> u32 {
        self.0
    }

    pub fn shift(self) -> bool {
        self.contains(Self::SHIFT)
    }
    pub fn alt(self) -> bool {
        self.contains(Self::ALT)
    }
    pub fn ctrl(self) -> bool {
        self.contains(Self::CTRL)
    }
    pub fn super_key(self) -> bool {
        self.contains(Self::SUPER)
    }
    pub fn hyper(self) -> bool {
        self.contains(Self::HYPER)
    }
    pub fn meta(self) -> bool {
        self.contains(Self::META)
    }
    pub fn caps_lock(self) -> bool {
        self.contains(Self::CAPS_LOCK)
    }
    pub fn num_lock(self) -> bool {
        self.contains(Self::NUM_LOCK)
    }

    /// Resolve the `kitty_mod` placeholder and then remove it from the bits.
    #[inline(always)]
    pub const fn resolve_kitty_mod(self, kitty_mod: Self) -> Self {
        if self.contains(Self::KITTY) {
            Self((self.0 & !Self::KITTY.0) | kitty_mod.0)
        } else {
            self
        }
    }

    /// Format modifiers in kitty's debug order.
    pub fn debug_string(self) -> String {
        let mut parts = Vec::new();
        for (bit, name) in [
            (Self::CTRL, "ctrl"),
            (Self::ALT, "alt"),
            (Self::SHIFT, "shift"),
            (Self::SUPER, "super"),
            (Self::HYPER, "hyper"),
            (Self::META, "meta"),
            (Self::CAPS_LOCK, "capslock"),
            (Self::NUM_LOCK, "numlock"),
        ] {
            if self.contains(bit) {
                parts.push(name);
            }
        }
        if parts.is_empty() {
            "none".into()
        } else {
            parts.join("+")
        }
    }
}

impl BitOr for Modifiers {
    type Output = Self;
    #[inline(always)]
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}
impl BitOrAssign for Modifiers {
    #[inline(always)]
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}
impl BitAnd for Modifiers {
    type Output = Self;
    #[inline(always)]
    fn bitand(self, rhs: Self) -> Self {
        Self(self.0 & rhs.0)
    }
}
impl BitAndAssign for Modifiers {
    #[inline(always)]
    fn bitand_assign(&mut self, rhs: Self) {
        self.0 &= rhs.0;
    }
}
impl Sub for Modifiers {
    type Output = Self;
    #[inline(always)]
    fn sub(self, rhs: Self) -> Self {
        Self(self.0 & !rhs.0)
    }
}
impl SubAssign for Modifiers {
    #[inline(always)]
    fn sub_assign(&mut self, rhs: Self) {
        self.0 &= !rhs.0;
    }
}
impl Not for Modifiers {
    type Output = Self;
    #[inline(always)]
    fn not(self) -> Self {
        Self(!self.0)
    }
}

impl core::fmt::Debug for Modifiers {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_tuple("Modifiers")
            .field(&self.debug_string())
            .finish()
    }
}

/// Key event type. The discriminants are the kitty `KeyAction` values used by
/// `key_encoding.c` (not the GLFW action values).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(u8)]
pub enum KeyAction {
    #[default]
    Press = 0,
    Repeat = 1,
    Release = 2,
}

impl KeyAction {
    /// Value printed in the CSI-u event-type sub-parameter (`action + 1`).
    #[inline(always)]
    pub const fn protocol_value(self) -> u8 {
        self as u8 + 1
    }

    /// GLFW action value (`GLFW_RELEASE`, `GLFW_PRESS`, `GLFW_REPEAT`).
    #[inline(always)]
    pub const fn glfw_value(self) -> i32 {
        match self {
            Self::Press => GLFW_PRESS,
            Self::Repeat => GLFW_REPEAT,
            Self::Release => GLFW_RELEASE,
        }
    }

    pub const fn from_glfw(value: i32) -> Option<Self> {
        match value {
            GLFW_PRESS => Some(Self::Press),
            GLFW_REPEAT => Some(Self::Repeat),
            GLFW_RELEASE => Some(Self::Release),
            _ => None,
        }
    }
}

/// A keyboard event, matching the fields kitty consumes from `GLFWkeyevent`
/// plus the Python-visible `key`/`shifted_key`/`alternate_key` fields.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KeyEvent {
    pub key: u32,
    pub shifted_key: u32,
    pub alternate_key: u32,
    /// Native platform key code, used only by the shortcut dispatcher to skip
    /// keys with no useful action.
    pub native_key: u32,
    pub modifiers: Modifiers,
    pub action: KeyAction,
    pub text: Option<String>,
}

impl KeyEvent {
    pub fn new(key: u32) -> Self {
        Self {
            key,
            ..Self::default()
        }
    }

    pub const fn press(key: u32) -> Self {
        Self {
            key,
            shifted_key: 0,
            alternate_key: 0,
            native_key: 0,
            modifiers: Modifiers::NONE,
            action: KeyAction::Press,
            text: None,
        }
    }

    pub fn with_modifiers(mut self, modifiers: Modifiers) -> Self {
        self.modifiers = modifiers;
        self
    }

    pub fn with_action(mut self, action: KeyAction) -> Self {
        self.action = action;
        self
    }

    pub fn with_shifted_key(mut self, key: u32) -> Self {
        self.shifted_key = key;
        self
    }

    pub fn with_alternate_key(mut self, key: u32) -> Self {
        self.alternate_key = key;
        self
    }

    pub fn with_native_key(mut self, key: u32) -> Self {
        self.native_key = key;
        self
    }

    pub fn with_text(mut self, text: impl Into<String>) -> Self {
        self.text = Some(text.into());
        self
    }

    pub fn text_str(&self) -> &str {
        self.text.as_deref().unwrap_or("")
    }

    pub fn is_modifier(&self) -> bool {
        is_modifier_key(self.key)
    }

    pub fn is_release(&self) -> bool {
        self.action == KeyAction::Release
    }

    /// True when `text` begins with an ASCII control character (or is empty),
    /// exactly as `startswith_ascii_control_char()` in `key_encoding.c`.
    pub fn starts_with_ascii_control_char(&self) -> bool {
        match self.text.as_deref() {
            None | Some("") => true,
            Some(text) => {
                let first = text.chars().next().unwrap_or('\0');
                (first as u32) < 32 || first == '\u{7f}'
            }
        }
    }

    /// `has_text` from `encode_glfw_key_event()`.
    pub fn has_usable_text(&self) -> bool {
        self.text.as_deref().is_some_and(|t| !t.is_empty())
            && !self.starts_with_ascii_control_char()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifier_ops() {
        let m = Modifiers::SHIFT | Modifiers::CTRL | Modifiers::CAPS_LOCK;
        assert!(m.shift() && m.ctrl() && m.caps_lock());
        assert_eq!(m.without_locks(), Modifiers::SHIFT | Modifiers::CTRL);
        assert_eq!(m - Modifiers::SHIFT, Modifiers::CTRL | Modifiers::CAPS_LOCK);
        assert_eq!(
            Modifiers::KITTY.resolve_kitty_mod(Modifiers::CTRL),
            Modifiers::CTRL
        );
    }

    #[test]
    fn actions() {
        assert_eq!(KeyAction::Press.protocol_value(), 1);
        assert_eq!(KeyAction::Repeat.protocol_value(), 2);
        assert_eq!(KeyAction::Release.protocol_value(), 3);
        assert_eq!(KeyAction::Release.glfw_value(), GLFW_RELEASE);
        assert_eq!(KeyAction::from_glfw(GLFW_REPEAT), Some(KeyAction::Repeat));
        assert_eq!(KeyAction::from_glfw(99), None);
    }

    #[test]
    fn text_control_detection() {
        assert!(KeyEvent::press(97).starts_with_ascii_control_char());
        assert!(KeyEvent::press(97)
            .with_text("\x1b[A")
            .starts_with_ascii_control_char());
        assert!(!KeyEvent::press(97)
            .with_text("a")
            .starts_with_ascii_control_char());
        assert!(KeyEvent::press(97).with_text("a").has_usable_text());
    }
}
