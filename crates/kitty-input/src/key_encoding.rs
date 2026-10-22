// SPDX-License-Identifier: GPL-3.0-or-later
//! Kitty keyboard protocol and legacy key encoding.
//!
//! This is a direct port of the encoding half of `kitty/key_encoding.c`:
//! [`encode_glfw_key_event`] corresponds to `encode_glfw_key_event()`,
//! [`encode_key`] to `encode_key()`, and the helper tables come from
//! `gen/key_constants.py`. The kitty protocol flags are the progressive
//! enhancement flags documented at <https://sw.kovidgoyal.net/kitty/keyboard-protocol/>.

use crate::key::{KeyAction, KeyEvent, Modifiers};
use crate::key_constants::*;
use crate::key_names::{convert_kp_key_to_normal_key, is_modifier_key};

/// Progressive keyboard enhancement flags (`CSI > flags u`).
#[derive(Copy, Clone, Default, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[repr(transparent)]
pub struct KittyKeyboardFlags(pub u32);

impl KittyKeyboardFlags {
    pub const NONE: Self = Self(0);
    /// `0b1`: disambiguate escape codes.
    pub const DISAMBIGUATE_ESCAPE_CODES: Self = Self(1);
    /// `0b10`: report event types (repeat and release).
    pub const REPORT_EVENT_TYPES: Self = Self(2);
    /// `0b100`: report alternate keys (shifted and base-layout).
    pub const REPORT_ALTERNATE_KEYS: Self = Self(4);
    /// `0b1000`: report all keys as escape codes.
    pub const REPORT_ALL_KEYS_AS_ESCAPE_CODES: Self = Self(8);
    /// `0b10000`: report associated text.
    pub const REPORT_ASSOCIATED_TEXT: Self = Self(16);
    pub const ALL: Self = Self(31);

    #[inline(always)]
    pub const fn from_bits_truncate(bits: u32) -> Self {
        Self(bits & 0x7f)
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
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    #[inline(always)]
    pub const fn difference(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    #[inline(always)]
    pub const fn disambiguate(self) -> bool {
        self.intersects(Self::DISAMBIGUATE_ESCAPE_CODES)
    }
    #[inline(always)]
    pub const fn report_all_event_types(self) -> bool {
        self.intersects(Self::REPORT_EVENT_TYPES)
    }
    #[inline(always)]
    pub const fn report_alternate_key(self) -> bool {
        self.intersects(Self::REPORT_ALTERNATE_KEYS)
    }
    /// The C flag named `report_text` (`REPORT_ALL_KEYS_AS_ESCAPE_CODES`).
    #[inline(always)]
    pub const fn report_all_keys_as_escape_codes(self) -> bool {
        self.intersects(Self::REPORT_ALL_KEYS_AS_ESCAPE_CODES)
    }
    /// The C flag named `embed_text` (`REPORT_ASSOCIATED_TEXT`).
    #[inline(always)]
    pub const fn report_associated_text(self) -> bool {
        self.intersects(Self::REPORT_ASSOCIATED_TEXT)
    }
}

impl core::ops::BitOr for KittyKeyboardFlags {
    type Output = Self;
    #[inline(always)]
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl core::ops::BitOrAssign for KittyKeyboardFlags {
    #[inline(always)]
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl core::ops::BitAnd for KittyKeyboardFlags {
    type Output = Self;
    #[inline(always)]
    fn bitand(self, rhs: Self) -> Self {
        Self(self.0 & rhs.0)
    }
}

/// Result of encoding a key event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EncodedKey {
    /// Nothing should be written (the event is not supported by the current mode).
    None,
    /// Write the event's text to the child unchanged (`SEND_TEXT_TO_CHILD`).
    Text,
    /// Write these bytes to the child.
    Bytes(Vec<u8>),
}

impl EncodedKey {
    pub fn into_bytes(self) -> Option<Vec<u8>> {
        match self {
            Self::Bytes(bytes) => Some(bytes),
            Self::None | Self::Text => None,
        }
    }

    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Bytes(bytes) => Some(bytes),
            Self::None | Self::Text => None,
        }
    }
}

struct EncodingData<'a> {
    key: u32,
    shifted_key: u32,
    alternate_key: u32,
    add_alternates: bool,
    has_mods: bool,
    add_actions: bool,
    add_text: bool,
    encoded_mods: [u8; 4],
    text: Option<&'a str>,
    action: KeyAction,
}

fn init_encoding_data<'a>(ev: &'a KeyEvent, flags: KittyKeyboardFlags) -> EncodingData<'a> {
    let add_actions = flags.report_all_event_types() && ev.action != KeyAction::Press;
    let has_mods = ev.modifiers.protocol_value() != 0;
    let add_alternates = flags.report_alternate_key()
        && ((ev.shifted_key > 0 && ev.modifiers.shift()) || ev.alternate_key > 0);
    let (shifted_key, alternate_key) = if add_alternates {
        (
            if ev.modifiers.shift() {
                ev.shifted_key
            } else {
                0
            },
            ev.alternate_key,
        )
    } else {
        (0, 0)
    };
    let add_text =
        flags.report_associated_text() && ev.text.as_deref().is_some_and(|t| !t.is_empty());
    let mut encoded_mods = [0u8; 4];
    let mut value = ev.modifiers.protocol_value() + 1;
    let mut pos = 4;
    loop {
        pos -= 1;
        encoded_mods[pos] = b'0' + (value % 10) as u8;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    let mut compact = [0u8; 4];
    compact[..4 - pos].copy_from_slice(&encoded_mods[pos..]);
    EncodingData {
        key: ev.key,
        shifted_key,
        alternate_key,
        add_alternates,
        has_mods,
        add_actions,
        add_text,
        encoded_mods: compact,
        text: ev.text.as_deref(),
        action: ev.action,
    }
}

fn serialize(data: &EncodingData<'_>, trailer: u8) -> Vec<u8> {
    let second_field_not_empty = data.has_mods || data.add_actions;
    let third_field_not_empty = data.add_text;
    let mut out = Vec::with_capacity(KEY_BUFFER_SIZE);
    out.extend_from_slice(b"\x1b[");
    if data.key != 1 || data.add_alternates || second_field_not_empty || third_field_not_empty {
        push_u32(&mut out, data.key);
    }
    if data.add_alternates {
        out.push(b':');
        if data.shifted_key > 0 {
            push_u32(&mut out, data.shifted_key);
        }
        if data.alternate_key > 0 {
            out.push(b':');
            push_u32(&mut out, data.alternate_key);
        }
    }
    if second_field_not_empty || third_field_not_empty {
        out.push(b';');
        if second_field_not_empty {
            let end = data
                .encoded_mods
                .iter()
                .position(|&b| b == 0)
                .unwrap_or(data.encoded_mods.len());
            out.extend_from_slice(&data.encoded_mods[..end]);
        }
        if data.add_actions {
            out.push(b':');
            out.push(b'0' + data.action.protocol_value());
        }
    }
    if third_field_not_empty {
        let mut first = true;
        for codepoint in data.text.unwrap_or("").chars() {
            out.push(if first { b';' } else { b':' });
            push_u32(&mut out, codepoint as u32);
            first = false;
        }
    }
    if out.len() > KEY_BUFFER_SIZE - 2 {
        out.truncate(KEY_BUFFER_SIZE - 2);
    }
    out.push(trailer);
    out
}

fn push_u32(out: &mut Vec<u8>, mut value: u32) {
    let mut buf = [0u8; 10];
    let mut pos = buf.len();
    loop {
        pos -= 1;
        buf[pos] = b'0' + (value % 10) as u8;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    out.extend_from_slice(&buf[pos..]);
}

#[inline]
fn encode_utf8(codepoint: u32, out: &mut Vec<u8>) -> usize {
    let ch = char::from_u32(codepoint).unwrap_or('\u{fffd}');
    let mut buf = [0u8; 4];
    let encoded = ch.encode_utf8(&mut buf);
    out.extend_from_slice(encoded.as_bytes());
    encoded.len()
}

fn legacy_functional_key_encoding_with_modifiers(
    key_number: u32,
    ev: &KeyEvent,
    output: &mut Vec<u8>,
) -> bool {
    let mut prefix: &[u8] = if ev.modifiers.alt() { b"\x1b" } else { b"" };
    let main_bytes: &[u8] = match key_number {
        GLFW_FKEY_ENTER => b"\x0d",
        GLFW_FKEY_ESCAPE => b"\x1b",
        GLFW_FKEY_BACKSPACE => {
            if ev.modifiers.ctrl() {
                b"\x08"
            } else {
                b"\x7f"
            }
        }
        GLFW_FKEY_TAB => {
            if ev.modifiers.shift() {
                prefix = if ev.modifiers.alt() {
                    b"\x1b\x1b"
                } else {
                    b"\x1b"
                };
                b"[Z"
            } else {
                b"\t"
            }
        }
        _ => return false,
    };
    output.extend_from_slice(prefix);
    output.extend_from_slice(main_bytes);
    true
}

fn encode_function_key(ev: &KeyEvent, flags: KittyKeyboardFlags, cursor_key_mode: bool) -> Vec<u8> {
    let mut csi_trailer = b'u';
    let mut key_number = ev.key;
    let legacy_mode = !flags.report_all_event_types()
        && !flags.disambiguate()
        && !flags.report_all_keys_as_escape_codes();

    if cursor_key_mode && ev.modifiers.is_empty() && legacy_mode {
        let simple: Option<&[u8]> = match key_number {
            GLFW_FKEY_UP => Some(b"\x1bOA"),
            GLFW_FKEY_DOWN => Some(b"\x1bOB"),
            GLFW_FKEY_RIGHT => Some(b"\x1bOC"),
            GLFW_FKEY_LEFT => Some(b"\x1bOD"),
            GLFW_FKEY_KP_BEGIN => Some(b"\x1bOE"),
            GLFW_FKEY_END => Some(b"\x1bOF"),
            GLFW_FKEY_HOME => Some(b"\x1bOH"),
            _ => None,
        };
        if let Some(bytes) = simple {
            return bytes.to_vec();
        }
    }
    if ev.modifiers.is_empty() {
        if !flags.disambiguate()
            && !flags.report_all_keys_as_escape_codes()
            && key_number == GLFW_FKEY_ESCAPE
        {
            return b"\x1b".to_vec();
        }
        if legacy_mode {
            match key_number {
                GLFW_FKEY_F1 => return b"\x1bOP".to_vec(),
                GLFW_FKEY_F2 => return b"\x1bOQ".to_vec(),
                GLFW_FKEY_F3 => return b"\x1bOR".to_vec(),
                GLFW_FKEY_F4 => return b"\x1bOS".to_vec(),
                _ => {}
            }
        }
        if !flags.report_all_keys_as_escape_codes() {
            match key_number {
                GLFW_FKEY_ENTER => {
                    if ev.action == KeyAction::Release {
                        return Vec::new();
                    }
                    return b"\r".to_vec();
                }
                GLFW_FKEY_BACKSPACE => {
                    if ev.action == KeyAction::Release {
                        return Vec::new();
                    }
                    return b"\x7f".to_vec();
                }
                GLFW_FKEY_TAB => {
                    if ev.action == KeyAction::Release {
                        return Vec::new();
                    }
                    return b"\t".to_vec();
                }
                _ => {}
            }
        }
    } else if legacy_mode {
        let mut output = Vec::new();
        if legacy_functional_key_encoding_with_modifiers(key_number, ev, &mut output) {
            return output;
        }
    }
    if !ev.modifiers.intersects(!Modifiers::LOCKS) && !flags.report_all_keys_as_escape_codes() {
        match key_number {
            GLFW_FKEY_ENTER => {
                if ev.action == KeyAction::Release {
                    return Vec::new();
                }
                return b"\r".to_vec();
            }
            GLFW_FKEY_BACKSPACE => {
                if ev.action == KeyAction::Release {
                    return Vec::new();
                }
                return b"\x7f".to_vec();
            }
            GLFW_FKEY_TAB => {
                if ev.action == KeyAction::Release {
                    return Vec::new();
                }
                return b"\t".to_vec();
            }
            _ => {}
        }
    }
    match key_number {
        GLFW_FKEY_ESCAPE => {
            key_number = 27;
            csi_trailer = b'u';
        }
        GLFW_FKEY_ENTER => {
            key_number = 13;
            csi_trailer = b'u';
        }
        GLFW_FKEY_TAB => {
            key_number = 9;
            csi_trailer = b'u';
        }
        GLFW_FKEY_BACKSPACE => {
            key_number = 127;
            csi_trailer = b'u';
        }
        GLFW_FKEY_INSERT => {
            key_number = 2;
            csi_trailer = b'~';
        }
        GLFW_FKEY_DELETE => {
            key_number = 3;
            csi_trailer = b'~';
        }
        GLFW_FKEY_LEFT => {
            key_number = 1;
            csi_trailer = b'D';
        }
        GLFW_FKEY_RIGHT => {
            key_number = 1;
            csi_trailer = b'C';
        }
        GLFW_FKEY_UP => {
            key_number = 1;
            csi_trailer = b'A';
        }
        GLFW_FKEY_DOWN => {
            key_number = 1;
            csi_trailer = b'B';
        }
        GLFW_FKEY_PAGE_UP => {
            key_number = 5;
            csi_trailer = b'~';
        }
        GLFW_FKEY_PAGE_DOWN => {
            key_number = 6;
            csi_trailer = b'~';
        }
        GLFW_FKEY_HOME => {
            key_number = 1;
            csi_trailer = b'H';
        }
        GLFW_FKEY_END => {
            key_number = 1;
            csi_trailer = b'F';
        }
        GLFW_FKEY_F1 => {
            key_number = 1;
            csi_trailer = b'P';
        }
        GLFW_FKEY_F2 => {
            key_number = 1;
            csi_trailer = b'Q';
        }
        GLFW_FKEY_F3 => {
            key_number = 13;
            csi_trailer = b'~';
        }
        GLFW_FKEY_F4 => {
            key_number = 1;
            csi_trailer = b'S';
        }
        GLFW_FKEY_F5 => {
            key_number = 15;
            csi_trailer = b'~';
        }
        GLFW_FKEY_F6 => {
            key_number = 17;
            csi_trailer = b'~';
        }
        GLFW_FKEY_F7 => {
            key_number = 18;
            csi_trailer = b'~';
        }
        GLFW_FKEY_F8 => {
            key_number = 19;
            csi_trailer = b'~';
        }
        GLFW_FKEY_F9 => {
            key_number = 20;
            csi_trailer = b'~';
        }
        GLFW_FKEY_F10 => {
            key_number = 21;
            csi_trailer = b'~';
        }
        GLFW_FKEY_F11 => {
            key_number = 23;
            csi_trailer = b'~';
        }
        GLFW_FKEY_F12 => {
            key_number = 24;
            csi_trailer = b'~';
        }
        GLFW_FKEY_KP_BEGIN => {
            key_number = 1;
            csi_trailer = b'E';
        }
        GLFW_FKEY_MENU if legacy_mode => {
            key_number = 29;
            csi_trailer = b'~';
        }
        _ => {}
    }
    let mut ed = init_encoding_data(ev, flags);
    ed.key = key_number;
    ed.add_alternates = false;
    serialize(&ed, csi_trailer)
}

fn ctrled_key(key: u8) -> u8 {
    match key {
        b' ' | b'@' | b'2' => 0,
        b'/' | b'7' | b'_' => 31,
        b'0' => 48,
        b'1' => 49,
        b'3' | b'[' => 27,
        b'4' | b'\\' => 28,
        b'5' | b']' => 29,
        b'6' | b'^' | b'~' => 30,
        b'8' | b'?' => 127,
        b'9' => 57,
        b'a'..=b'z' => key - b'a' + 1,
        _ => key,
    }
}

fn encode_printable_ascii_key_legacy(ev: &KeyEvent, output: &mut Vec<u8>) -> bool {
    let mut mods = ev.modifiers;
    if mods.is_empty() {
        output.push(ev.key as u8);
        return true;
    }

    let mut key = ev.key as u8;
    if mods.shift() {
        let shifted = ev.shifted_key;
        if shifted > 0
            && shifted != ev.key
            && (!mods.ctrl() || ev.key < b'a' as u32 || ev.key > b'z' as u32)
        {
            key = shifted as u8;
            mods -= Modifiers::SHIFT;
        }
    }

    if ev.modifiers == Modifiers::SHIFT {
        output.push(key);
        return true;
    }
    if mods == Modifiers::ALT {
        output.push(0x1b);
        output.push(key);
        return true;
    }
    if mods == Modifiers::CTRL {
        output.push(ctrled_key(key));
        return true;
    }
    if mods == Modifiers::CTRL | Modifiers::ALT {
        output.push(0x1b);
        output.push(ctrled_key(key));
        return true;
    }
    if key == b' ' {
        if mods == Modifiers::CTRL | Modifiers::SHIFT {
            output.push(ctrled_key(key));
            return true;
        }
        if mods == Modifiers::ALT | Modifiers::SHIFT {
            output.push(0x1b);
            output.push(key);
            return true;
        }
    }
    false
}

fn is_legacy_ascii_key(key: u32) -> bool {
    let Some(c) = char::from_u32(key) else {
        return false;
    };
    matches!(
        c,
        'a'..='z'
            | '0'..='9'
            | '!' | '@' | '#' | '$' | '%' | '^' | '&' | '*' | '(' | ')' | '`' | '~'
            | '-' | '_' | '=' | '+' | '[' | '{' | ']' | '}' | '\\' | '|' | ';' | ':'
            | '\'' | '"' | ',' | '<' | '.' | '>' | '/' | '?' | ' '
    )
}

fn encode_key_inner(ev: &KeyEvent, flags: KittyKeyboardFlags, cursor_key_mode: bool) -> EncodedKey {
    if !flags.report_all_event_types() && ev.action == KeyAction::Release {
        return EncodedKey::None;
    }
    if (GLFW_FKEY_FIRST..=GLFW_FKEY_LAST).contains(&ev.key) {
        let bytes = encode_function_key(ev, flags, cursor_key_mode);
        if bytes.is_empty() {
            return EncodedKey::None;
        }
        return EncodedKey::Bytes(bytes);
    }
    let ed = init_encoding_data(ev, flags);
    let simple_encoding_ok = !ed.add_actions && !ed.add_alternates && !ed.add_text;

    if simple_encoding_ok {
        if !ed.has_mods {
            if flags.report_all_keys_as_escape_codes() {
                return EncodedKey::Bytes(serialize(&ed, b'u'));
            }
            let mut out = Vec::with_capacity(4);
            encode_utf8(ev.key, &mut out);
            return EncodedKey::Bytes(out);
        }
        if !flags.disambiguate() && !flags.report_all_keys_as_escape_codes() {
            if is_legacy_ascii_key(ev.key)
                || (ev.shifted_key > 0 && is_legacy_ascii_key(ev.shifted_key))
            {
                let mut out = Vec::new();
                if encode_printable_ascii_key_legacy(ev, &mut out) {
                    return EncodedKey::Bytes(out);
                }
            }
            let mods = ev.modifiers;
            if (mods == Modifiers::CTRL
                || mods == Modifiers::ALT
                || mods == Modifiers::CTRL | Modifiers::ALT)
                && ev.alternate_key > 0
                && !is_legacy_ascii_key(ev.key)
                && is_legacy_ascii_key(ev.alternate_key)
            {
                let alternate = KeyEvent {
                    key: ev.alternate_key,
                    alternate_key: 0,
                    shifted_key: 0,
                    ..ev.clone()
                };
                let mut out = Vec::new();
                if encode_printable_ascii_key_legacy(&alternate, &mut out) {
                    return EncodedKey::Bytes(out);
                }
            }
        }
    }

    EncodedKey::Bytes(serialize(&init_encoding_data(ev, flags), b'u'))
}

/// Port of `encode_key()` from `key_encoding.c`.
///
/// `ev.modifiers` must already have lock bits handled the way kitty does (they
/// are stripped only when no protocol flags are active). Most callers should
/// use [`encode_glfw_key_event`], which performs that preprocessing and the
/// keypad translation.
pub fn encode_key(ev: &KeyEvent, flags: KittyKeyboardFlags) -> EncodedKey {
    encode_key_inner(ev, flags, false)
}

/// Port of `encode_glfw_key_event()`.
///
/// `cursor_key_mode` is DECCKM. When the event should be sent to the child as
/// plain text, [`EncodedKey::Text`] is returned and the caller should write
/// `event.text` unchanged.
pub fn encode_glfw_key_event(
    event: &KeyEvent,
    cursor_key_mode: bool,
    flags: KittyKeyboardFlags,
) -> EncodedKey {
    if !flags.report_all_keys_as_escape_codes() && is_modifier_key(event.key) {
        return EncodedKey::None;
    }
    let has_text = event.has_usable_text();
    if event.key == 0 && !has_text {
        return EncodedKey::None;
    }
    let send_text_standalone = !flags.report_all_keys_as_escape_codes();
    let mut ev = event.clone();
    if !flags.disambiguate()
        && !flags.report_all_keys_as_escape_codes()
        && (GLFW_FKEY_KP_0..=GLFW_FKEY_KP_BEGIN).contains(&ev.key)
    {
        ev.key = convert_kp_key_to_normal_key(ev.key);
    }
    if send_text_standalone
        && has_text
        && (ev.action == KeyAction::Press || ev.action == KeyAction::Repeat)
    {
        return EncodedKey::Text;
    }
    if flags.is_empty() {
        ev.modifiers = ev.modifiers.without_locks();
    }
    encode_key_inner(&ev, flags, cursor_key_mode)
}

/// Convenience wrapper matching the Python `encode_key_for_tty()` test helper.
///
/// For [`EncodedKey::Text`] this returns the event's text bytes, exactly as the
/// Python binding does.
pub fn encode_key_for_tty(
    event: &KeyEvent,
    cursor_key_mode: bool,
    flags: KittyKeyboardFlags,
) -> Vec<u8> {
    match encode_glfw_key_event(event, cursor_key_mode, flags) {
        EncodedKey::None => Vec::new(),
        EncodedKey::Text => event.text.as_deref().unwrap_or("").as_bytes().to_vec(),
        EncodedKey::Bytes(bytes) => bytes,
    }
}

/// `screen_set_key_encoding_flags` (CSI `=`, `>`, `<` `u`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum KeyEncodingUpdate {
    Set,
    Push,
    Pop,
}

/// The stack of progressive enhancement flags maintained by a screen.
///
/// This is a 1:1 port of `screen_set_key_encoding_flags()`,
/// `screen_push_key_encoding_flags()` and `screen_pop_key_encoding_flags()`,
/// including the 8-deep stack and the high bit marking active entries.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KeyEncodingStack {
    flags: [u8; 8],
}

impl KeyEncodingStack {
    pub fn new() -> Self {
        Self::default()
    }

    fn current_index(&self) -> usize {
        let mut idx = 0;
        for i in (0..self.flags.len()).rev() {
            if self.flags[i] & 0x80 != 0 {
                idx = i;
                break;
            }
        }
        idx
    }

    /// `screen_current_key_encoding_flags()`.
    pub fn current(&self) -> KittyKeyboardFlags {
        for i in (0..self.flags.len()).rev() {
            if self.flags[i] & 0x80 != 0 {
                return KittyKeyboardFlags::from_bits_truncate((self.flags[i] & 0x7f) as u32);
            }
        }
        KittyKeyboardFlags::NONE
    }

    pub fn set(&mut self, val: u32, how: u32) {
        let idx = self.current_index();
        let q = (val & 0x7f) as u8;
        if how == 1 {
            self.flags[idx] = q;
        } else if how == 2 {
            self.flags[idx] |= q;
        } else if how == 3 {
            self.flags[idx] &= !q;
        }
        self.flags[idx] |= 0x80;
    }

    pub fn push(&mut self, val: u32) {
        let q = (val & 0x7f) as u8;
        let sz = self.flags.len();
        let mut current_idx = self.current_index();
        if current_idx == sz - 1 {
            self.flags.copy_within(1.., 0);
        } else {
            self.flags[current_idx] |= 0x80;
            current_idx += 1;
        }
        self.flags[current_idx] = 0x80 | q;
    }

    pub fn pop(&mut self, mut num: u32) {
        for i in (0..self.flags.len()).rev() {
            if num == 0 {
                break;
            }
            if self.flags[i] & 0x80 != 0 {
                num -= 1;
                self.flags[i] = 0;
            }
        }
    }

    /// Apply a parsed update (`CSI = u`, `CSI > u`, `CSI < u`).
    pub fn apply(&mut self, update: KeyEncodingUpdate, value: u32) {
        match update {
            KeyEncodingUpdate::Set => self.set(value, 1),
            KeyEncodingUpdate::Push => self.push(value),
            KeyEncodingUpdate::Pop => self.pop(value),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flag_stack_push_set_pop() {
        let mut stack = KeyEncodingStack::new();
        assert_eq!(stack.current(), KittyKeyboardFlags::NONE);
        stack.push(3);
        assert_eq!(stack.current(), KittyKeyboardFlags::from_bits_truncate(3));
        stack.set(1, 2);
        assert_eq!(stack.current(), KittyKeyboardFlags::from_bits_truncate(3));
        stack.set(4, 3);
        assert_eq!(stack.current(), KittyKeyboardFlags::from_bits_truncate(3));
        stack.push(8);
        assert_eq!(stack.current(), KittyKeyboardFlags::from_bits_truncate(8));
        stack.pop(1);
        assert_eq!(stack.current(), KittyKeyboardFlags::from_bits_truncate(3));
        stack.pop(99);
        assert_eq!(stack.current(), KittyKeyboardFlags::NONE);
    }
}
