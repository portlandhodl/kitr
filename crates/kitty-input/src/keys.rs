// SPDX-License-Identifier: GPL-3.0-or-later
//! Mode-dependent key dispatch and shortcut matching, ported from the input
//! path of `kitty/keys.c` and the matching rules in `kitty/key_encoding.py`.
//!
//! The screen/options/data structures that the C code reaches through
//! `Screen`, `Window` and `global_state` are replaced by the deliberately
//! narrow [`InputContext`] trait. A backend only has to provide the DEC modes,
//! mouse modes and the current progressive keyboard flags; everything else
//! (encoding, shortcut callbacks, text fallback) stays in this crate.

use crate::key::{KeyAction, KeyEvent, Modifiers};
use crate::key_constants::*;
use crate::key_encoding::{
    encode_glfw_key_event, EncodedKey, KeyEncodingStack, KittyKeyboardFlags,
};
use crate::key_names::key_from_name;
use crate::mouse::{MouseTrackingMode, MouseTrackingProtocol};

/// The screen modes that affect input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InputModes {
    /// DECCKM: application cursor key mode.
    pub decckm: bool,
    /// DECKPAM: application keypad mode.
    pub deckpam: bool,
    /// DECARM: keyboard auto-repeat.
    pub decarm: bool,
    pub mouse_tracking_mode: MouseTrackingMode,
    pub mouse_tracking_protocol: MouseTrackingProtocol,
}

impl Default for InputModes {
    fn default() -> Self {
        Self {
            decckm: false,
            deckpam: false,
            // `empty_modes` in screen.c has DECARM on.
            decarm: true,
            mouse_tracking_mode: MouseTrackingMode::Off,
            mouse_tracking_protocol: MouseTrackingProtocol::Normal,
        }
    }
}

impl InputModes {
    /// Apply a DEC private mode number as it appears on the wire.
    ///
    /// Mouse modes 1000/1002/1003 and protocol modes 1005/1006/1015/1016 are
    /// handled exactly like `set_mode_from_const()` in `screen.c`. DECCKM is
    /// mode 1 and DECKPAM comes from the separate ESC = / ESC > sequences.
    pub fn set_private_mode(&mut self, mode: u32, value: bool) {
        match mode {
            1 => self.decckm = value,
            1000 => {
                self.mouse_tracking_mode = if value {
                    MouseTrackingMode::Click
                } else {
                    MouseTrackingMode::Off
                }
            }
            1002 => {
                self.mouse_tracking_mode = if value {
                    MouseTrackingMode::Motion
                } else {
                    MouseTrackingMode::Off
                }
            }
            1003 => {
                self.mouse_tracking_mode = if value {
                    MouseTrackingMode::Any
                } else {
                    MouseTrackingMode::Off
                }
            }
            1005 => {
                self.mouse_tracking_protocol = if value {
                    MouseTrackingProtocol::Utf8
                } else {
                    MouseTrackingProtocol::Normal
                }
            }
            1006 => {
                self.mouse_tracking_protocol = if value {
                    MouseTrackingProtocol::Sgr
                } else {
                    MouseTrackingProtocol::Normal
                }
            }
            1015 => {
                self.mouse_tracking_protocol = if value {
                    MouseTrackingProtocol::Urxvt
                } else {
                    MouseTrackingProtocol::Normal
                }
            }
            1016 => {
                self.mouse_tracking_protocol = if value {
                    MouseTrackingProtocol::SgrPixel
                } else {
                    MouseTrackingProtocol::Normal
                }
            }
            _ => {}
        }
    }
}

/// Narrow view of the screen needed by the input layer.
pub trait InputContext {
    fn modes(&self) -> &InputModes;
    fn key_encoding_flags(&self) -> KittyKeyboardFlags;

    /// Scale factor applied to pixel coordinates before reporting them to the
    /// child in SGR-pixel mode. The default is 1.0.
    fn pixel_scale(&self) -> f64 {
        1.0
    }
}

/// A self-contained screen input context, avoiding a dependency on the screen
/// port. It owns the key-encoding flag stack from `screen.c`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ScreenInputContext {
    pub modes: InputModes,
    pub key_encoding_stack: KeyEncodingStack,
}

impl ScreenInputContext {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn key_encoding_flags(&self) -> KittyKeyboardFlags {
        self.key_encoding_stack.current()
    }

    pub fn set_private_mode(&mut self, mode: u32, value: bool) {
        self.modes.set_private_mode(mode, value);
    }

    /// `ESC =` (DECKPAM).
    pub fn alternate_keypad_mode(&mut self) {
        self.modes.deckpam = true;
    }

    /// `ESC >` (DECKPNM).
    pub fn normal_keypad_mode(&mut self) {
        self.modes.deckpam = false;
    }

    /// xterm `CSI > 4 ; n m` / `CSI ? 4 ; n m` modifyOtherKeys.
    ///
    /// kitty deliberately ignores modifyOtherKeys because the kitty keyboard
    /// protocol supersedes it (see `screen_modify_other_keys()` in screen.c).
    /// We record whether the application requested it so a frontend can emit
    /// the same diagnostic, but the key encoding is unchanged.
    pub fn modify_other_keys(&mut self, value: u32, mode: u32) -> bool {
        value == 4 && mode != 0 && mode != u32::MAX && self.key_encoding_flags().is_empty()
    }
}

impl InputContext for ScreenInputContext {
    fn modes(&self) -> &InputModes {
        &self.modes
    }

    fn key_encoding_flags(&self) -> KittyKeyboardFlags {
        self.key_encoding_stack.current()
    }
}

/// What the caller should do with a key event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyOutput {
    /// No bytes.
    None,
    /// The event was handled by a kitty shortcut; nothing goes to the child.
    Shortcut,
    /// Write these bytes to the child.
    Bytes(Vec<u8>),
    /// Write this text to the child unchanged (`SEND_TEXT_TO_CHILD`).
    Text(String),
}

impl KeyOutput {
    pub fn bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Bytes(bytes) => Some(bytes),
            _ => None,
        }
    }

    pub fn into_bytes(self) -> Option<Vec<u8>> {
        match self {
            Self::Bytes(bytes) => Some(bytes),
            _ => None,
        }
    }
}

/// Callback interface used by [`KeyDispatcher`].
pub trait ShortcutHandler {
    /// Return true when the event was consumed by a kitty shortcut.
    fn dispatch_possible_special_key(&mut self, event: &KeyEvent) -> bool;

    /// Called when the key press that was consumed as a shortcut is released.
    fn on_shortcut_key_release(&mut self, _event: &KeyEvent) {}
}

impl<F> ShortcutHandler for F
where
    F: FnMut(&KeyEvent) -> bool,
{
    fn dispatch_possible_special_key(&mut self, event: &KeyEvent) -> bool {
        self(event)
    }
}

/// Port of the hot path of `on_key_input()`/`send_key_to_child()`.
#[derive(Debug)]
pub struct KeyDispatcher<H> {
    handler: H,
    last_special_key_pressed: Option<u32>,
    buffered_keys_enabled: bool,
    buffered: Vec<KeyEvent>,
}

impl<H: ShortcutHandler> KeyDispatcher<H> {
    pub fn new(handler: H) -> Self {
        Self {
            handler,
            last_special_key_pressed: None,
            buffered_keys_enabled: false,
            buffered: Vec::new(),
        }
    }

    pub fn handler(&self) -> &H {
        &self.handler
    }

    pub fn handler_mut(&mut self) -> &mut H {
        &mut self.handler
    }

    /// When enabled, events are queued instead of encoded, matching the
    /// window's buffered-key state while the child is not ready.
    pub fn set_buffered_keys_enabled(&mut self, enabled: bool) {
        self.buffered_keys_enabled = enabled;
    }

    pub fn buffered_key_count(&self) -> usize {
        self.buffered.len()
    }

    pub fn take_buffered_keys(&mut self) -> Vec<KeyEvent> {
        core::mem::take(&mut self.buffered)
    }

    pub fn dispatch(&mut self, event: &KeyEvent, ctx: &dyn InputContext) -> KeyOutput {
        if event.action == KeyAction::Repeat && !ctx.modes().decarm {
            return KeyOutput::None;
        }

        if event.action == KeyAction::Press || event.action == KeyAction::Repeat {
            self.last_special_key_pressed = None;
            if self.handler.dispatch_possible_special_key(event) {
                self.last_special_key_pressed = Some(event.key);
                return KeyOutput::Shortcut;
            }
        } else if self.last_special_key_pressed == Some(event.key) {
            self.last_special_key_pressed = None;
            self.handler.on_shortcut_key_release(event);
            return KeyOutput::Shortcut;
        }

        if self.buffered_keys_enabled {
            self.buffered.push(event.clone());
            return KeyOutput::None;
        }

        self.encode_for_child(event, ctx)
    }

    /// Encode without invoking shortcut callbacks. Useful for injecting keys.
    pub fn encode_for_child(&self, event: &KeyEvent, ctx: &dyn InputContext) -> KeyOutput {
        let modes = ctx.modes();
        let flags = ctx.key_encoding_flags();

        if modes.deckpam
            && !flags.disambiguate()
            && !flags.report_all_keys_as_escape_codes()
            && (event.modifiers & !Modifiers::LOCKS).is_empty()
        {
            if let Some(bytes) = application_keypad_bytes(event.key) {
                return KeyOutput::Bytes(bytes);
            }
        }

        match encode_glfw_key_event(event, modes.decckm, flags) {
            EncodedKey::None => KeyOutput::None,
            EncodedKey::Text => KeyOutput::Text(event.text.clone().unwrap_or_default()),
            EncodedKey::Bytes(bytes) => KeyOutput::Bytes(bytes),
        }
    }

    /// Encode a scroll wheel event as arrow presses/releases (`fake_scroll()`).
    pub fn fake_scroll(&self, amount: u32, upwards: bool, ctx: &dyn InputContext) -> Vec<Vec<u8>> {
        let key = if upwards {
            GLFW_FKEY_UP
        } else {
            GLFW_FKEY_DOWN
        };
        let mut ans = Vec::new();
        for _ in 0..amount {
            for action in [KeyAction::Press, KeyAction::Release] {
                let ev = KeyEvent {
                    key,
                    action,
                    ..KeyEvent::default()
                };
                if let KeyOutput::Bytes(bytes) = self.encode_for_child(&ev, ctx) {
                    ans.push(bytes);
                }
            }
        }
        ans
    }
}

/// xterm application keypad sequences (DECKPAM).
///
/// In C kitty leaves DECKPAM to the GUI (`screen_alternate_keypad_mode()` is a
/// no-op). This helper supplies the conventional xterm sequences so a pure
/// Rust frontend can honour the mode; [`InputModes::deckpam`] is otherwise
/// inert, matching kitty.
pub fn application_keypad_bytes(key: u32) -> Option<Vec<u8>> {
    let trailer = match key {
        GLFW_FKEY_KP_0 => b'p',
        GLFW_FKEY_KP_1 => b'q',
        GLFW_FKEY_KP_2 => b'r',
        GLFW_FKEY_KP_3 => b's',
        GLFW_FKEY_KP_4 => b't',
        GLFW_FKEY_KP_5 => b'u',
        GLFW_FKEY_KP_6 => b'v',
        GLFW_FKEY_KP_7 => b'w',
        GLFW_FKEY_KP_8 => b'x',
        GLFW_FKEY_KP_9 => b'y',
        GLFW_FKEY_KP_DECIMAL => b'n',
        GLFW_FKEY_KP_DIVIDE => b'o',
        GLFW_FKEY_KP_MULTIPLY => b'j',
        GLFW_FKEY_KP_SUBTRACT => b'm',
        GLFW_FKEY_KP_ADD => b'k',
        GLFW_FKEY_KP_ENTER => b'M',
        GLFW_FKEY_KP_EQUAL => b'X',
        GLFW_FKEY_KP_SEPARATOR => b'l',
        _ => return None,
    };
    Some(vec![0x1b, b'O', trailer])
}

/// A parsed key combination from a config file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Shortcut {
    pub mods: Modifiers,
    pub key: u32,
}

impl Shortcut {
    pub const fn new(mods: Modifiers, key: u32) -> Self {
        Self { mods, key }
    }

    pub fn from_event(event: &KeyEvent) -> Self {
        Self {
            mods: event.modifiers.without_locks(),
            key: event.key,
        }
    }

    /// Direct, shifted and alternate-key matches as in `KeyEvent.matches()`.
    pub fn matches_event(&self, event: &KeyEvent) -> bool {
        shortcut_candidates(event).iter().any(|c| c == self)
    }
}

/// Candidate shortcuts for an event, in kitty's match order: direct key,
/// shifted key (when Shift is held), then alternate/base-layout key for a
/// non-ASCII, non-private-use character.
pub fn shortcut_candidates(event: &KeyEvent) -> Vec<Shortcut> {
    let mods = event.modifiers.without_locks();
    let mut ans = vec![Shortcut::new(mods, event.key)];
    if event.shifted_key > 0 && event.modifiers.shift() {
        ans.push(Shortcut::new(mods - Modifiers::SHIFT, event.shifted_key));
    }
    if event.alternate_key > 0 && event.key > 127 && event.key < 0xE000 && event.key != 0 {
        ans.push(Shortcut::new(mods, event.alternate_key));
    }
    ans
}

fn modifier_from_name(name: &str) -> Option<Modifiers> {
    Some(match name.to_ascii_uppercase().as_str() {
        "SHIFT" | "⇧" => Modifiers::SHIFT,
        "ALT" | "OPTION" | "OPT" | "⌥" => Modifiers::ALT,
        "SUPER" | "COMMAND" | "CMD" | "⌘" => Modifiers::SUPER,
        "CONTROL" | "CTRL" | "⌃" => Modifiers::CTRL,
        "HYPER" => Modifiers::HYPER,
        "META" => Modifiers::META,
        "NUM_LOCK" | "NUM LOCK" => Modifiers::NUM_LOCK,
        "CAPS_LOCK" | "CAPS LOCK" => Modifiers::CAPS_LOCK,
        "KITTY_MOD" | "KITTY MOD" => Modifiers::KITTY,
        _ => return None,
    })
}

/// Parse a shortcut specification such as `ctrl+shift+p` or `kitty_mod+enter`.
pub fn parse_shortcut(spec: &str) -> Option<Shortcut> {
    let spec = spec.trim();
    if spec.is_empty() {
        return None;
    }
    let owned;
    let spec = if let Some(stripped) = spec.strip_suffix('+') {
        owned = format!("{stripped}plus");
        owned.as_str()
    } else {
        spec
    };
    let parts: Vec<&str> = spec.split('+').collect();
    let (mods, key_name) = parts.split_at(parts.len() - 1);
    let mut mods_value = Modifiers::NONE;
    for name in mods {
        mods_value |= modifier_from_name(name)?;
    }
    let key = key_from_name(key_name[0])?;
    Some(Shortcut::new(mods_value, key))
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Mapping {
    sequence: Vec<Shortcut>,
    action: String,
}

/// Result of feeding one event to a [`KeyMap`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MappingResult {
    /// Not consumed; these previously buffered events must be sent to the child.
    NotConsumed(Vec<KeyEvent>),
    /// Consumed as part of a longer sequence or as a mapping without an action.
    Consumed,
    /// A mapping matched; run its action.
    Action(String),
}

/// A small mapping engine with kitty's single/multi-key override rules.
///
/// It implements the observable behaviour of `kitty/keys.py` used by the
/// config tests: single-key mappings replace longer mappings with the same
/// prefix, longer mappings replace a single-key mapping with the same prefix,
/// and an unmatched prefix is flushed to the child.
#[derive(Clone, Debug)]
pub struct KeyMap {
    mappings: Vec<Mapping>,
    pending: Vec<KeyEvent>,
    pending_candidates: Vec<Shortcut>,
    kitty_mod: Modifiers,
}

impl Default for KeyMap {
    fn default() -> Self {
        Self::new(Modifiers::CTRL | Modifiers::SHIFT)
    }
}

impl KeyMap {
    pub fn new(kitty_mod: Modifiers) -> Self {
        Self {
            mappings: Vec::new(),
            pending: Vec::new(),
            pending_candidates: Vec::new(),
            kitty_mod,
        }
    }

    pub fn kitty_mod(&self) -> Modifiers {
        self.kitty_mod
    }

    pub fn set_kitty_mod(&mut self, kitty_mod: Modifiers) {
        self.kitty_mod = kitty_mod;
    }

    /// Parse `spec action...` (the portion of a `map` line after `map`).
    pub fn add_line(&mut self, line: &str) -> Result<(), String> {
        let line = line.trim();
        let (spec, action) = match line.find(char::is_whitespace) {
            Some(idx) => (&line[..idx], line[idx..].trim()),
            None => (line, ""),
        };
        self.add_mapping(spec, action)
    }

    /// Add a mapping. An empty action unmaps the sequence.
    pub fn add_mapping(&mut self, spec: &str, action: &str) -> Result<(), String> {
        let sequence: Vec<Shortcut> = spec
            .split('>')
            .map(|part| {
                parse_shortcut(part)
                    .map(|s| Shortcut::new(s.mods.resolve_kitty_mod(self.kitty_mod), s.key))
                    .ok_or_else(|| format!("invalid shortcut: {part}"))
            })
            .collect::<Result<_, _>>()?;
        if sequence.is_empty() {
            return Err("empty shortcut".into());
        }
        let first = sequence[0];
        if action.is_empty() && sequence.len() == 1 {
            self.mappings.retain(|m| m.sequence != sequence);
            return Ok(());
        }
        if sequence.len() == 1 {
            self.mappings.retain(|m| m.sequence.first() != Some(&first));
        } else {
            self.mappings
                .retain(|m| !(m.sequence.len() == 1 && m.sequence[0] == first));
            self.mappings.retain(|m| m.sequence != sequence);
        }
        self.mappings.push(Mapping {
            sequence,
            action: action.to_string(),
        });
        Ok(())
    }

    pub fn clear(&mut self) {
        self.mappings.clear();
        self.pending.clear();
        self.pending_candidates.clear();
    }

    pub fn is_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    pub fn take_pending(&mut self) -> Vec<KeyEvent> {
        core::mem::take(&mut self.pending)
    }

    /// Feed an event to the mapping engine.
    pub fn dispatch(&mut self, event: &KeyEvent) -> MappingResult {
        if !matches!(event.action, KeyAction::Press | KeyAction::Repeat) {
            return MappingResult::NotConsumed(Vec::new());
        }
        let candidates = shortcut_candidates(event);
        for candidate in &candidates {
            let mut sequence: Vec<Shortcut> = self.pending_sequence();
            sequence.push(*candidate);
            let exact = self
                .mappings
                .iter()
                .find(|m| m.sequence == sequence)
                .map(|m| m.action.clone());
            let is_prefix = self
                .mappings
                .iter()
                .any(|m| m.sequence.len() > sequence.len() && m.sequence.starts_with(&sequence));
            if let Some(action) = exact {
                if action.is_empty() {
                    let flush = core::mem::take(&mut self.pending);
                    self.pending_candidates.clear();
                    return MappingResult::NotConsumed(flush);
                }
                self.pending.clear();
                self.pending_candidates.clear();
                return MappingResult::Action(action);
            }
            if is_prefix {
                self.pending.push(event.clone());
                self.pending_candidates.push(*candidate);
                return MappingResult::Consumed;
            }
        }
        if self.pending.is_empty() {
            MappingResult::NotConsumed(Vec::new())
        } else {
            let flush = core::mem::take(&mut self.pending);
            self.pending_candidates.clear();
            MappingResult::NotConsumed(flush)
        }
    }

    fn pending_sequence(&self) -> Vec<Shortcut> {
        self.pending_candidates.clone()
    }
}

impl KeyMap {
    pub fn mapping_count(&self) -> usize {
        self.mappings.len()
    }

    pub fn pending_candidates(&self) -> Vec<Shortcut> {
        self.pending_candidates.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcut_parsing() {
        let s = parse_shortcut("ctrl+shift+p").unwrap();
        assert_eq!(s.key, b'p' as u32);
        assert_eq!(s.mods, Modifiers::CTRL | Modifiers::SHIFT);
        let s = parse_shortcut("kitty_mod+enter").unwrap();
        assert_eq!(s.key, GLFW_FKEY_ENTER);
        assert!(s.mods.contains(Modifiers::KITTY));
        assert_eq!(parse_shortcut("ctrl++").unwrap().key, b'+' as u32);
    }

    #[test]
    fn alternative_key_candidates() {
        let ev = KeyEvent {
            key: 0x441,
            alternate_key: b'c' as u32,
            modifiers: Modifiers::CTRL,
            ..KeyEvent::default()
        };
        assert!(Shortcut::new(Modifiers::CTRL, b'c' as u32).matches_event(&ev));
        assert!(!Shortcut::new(Modifiers::CTRL, b'k' as u32).matches_event(&ev));
    }
}
