// SPDX-License-Identifier: GPL-3.0-or-later
//! Key names and aliases ported from `kitty/key_names.py`,
//! `kitty/key_encoding.py` and the checked-in `key_encoding.json`.
//!
//! `key_encoding.json` is a historical table from kitty 0.9 used by the
//! original compact key-encoding protocol. It is preserved here verbatim so
//! callers that need to decode old configuration or replay old recordings do
//! not have to carry a copy of the JSON file. The modern protocol uses the
//! CSI-u functional key numbers in [`crate::key_constants`].

use crate::key_constants::*;

/// `(functional key number, canonical upper-case name)` in GLFW order.
pub const FUNCTIONAL_KEY_NAMES: &[(u32, &str)] = &[
    (GLFW_FKEY_ESCAPE, "ESCAPE"),
    (GLFW_FKEY_ENTER, "ENTER"),
    (GLFW_FKEY_TAB, "TAB"),
    (GLFW_FKEY_BACKSPACE, "BACKSPACE"),
    (GLFW_FKEY_INSERT, "INSERT"),
    (GLFW_FKEY_DELETE, "DELETE"),
    (GLFW_FKEY_LEFT, "LEFT"),
    (GLFW_FKEY_RIGHT, "RIGHT"),
    (GLFW_FKEY_UP, "UP"),
    (GLFW_FKEY_DOWN, "DOWN"),
    (GLFW_FKEY_PAGE_UP, "PAGE_UP"),
    (GLFW_FKEY_PAGE_DOWN, "PAGE_DOWN"),
    (GLFW_FKEY_HOME, "HOME"),
    (GLFW_FKEY_END, "END"),
    (GLFW_FKEY_CAPS_LOCK, "CAPS_LOCK"),
    (GLFW_FKEY_SCROLL_LOCK, "SCROLL_LOCK"),
    (GLFW_FKEY_NUM_LOCK, "NUM_LOCK"),
    (GLFW_FKEY_PRINT_SCREEN, "PRINT_SCREEN"),
    (GLFW_FKEY_PAUSE, "PAUSE"),
    (GLFW_FKEY_MENU, "MENU"),
    (GLFW_FKEY_F1, "F1"),
    (GLFW_FKEY_F2, "F2"),
    (GLFW_FKEY_F3, "F3"),
    (GLFW_FKEY_F4, "F4"),
    (GLFW_FKEY_F5, "F5"),
    (GLFW_FKEY_F6, "F6"),
    (GLFW_FKEY_F7, "F7"),
    (GLFW_FKEY_F8, "F8"),
    (GLFW_FKEY_F9, "F9"),
    (GLFW_FKEY_F10, "F10"),
    (GLFW_FKEY_F11, "F11"),
    (GLFW_FKEY_F12, "F12"),
    (GLFW_FKEY_F13, "F13"),
    (GLFW_FKEY_F14, "F14"),
    (GLFW_FKEY_F15, "F15"),
    (GLFW_FKEY_F16, "F16"),
    (GLFW_FKEY_F17, "F17"),
    (GLFW_FKEY_F18, "F18"),
    (GLFW_FKEY_F19, "F19"),
    (GLFW_FKEY_F20, "F20"),
    (GLFW_FKEY_F21, "F21"),
    (GLFW_FKEY_F22, "F22"),
    (GLFW_FKEY_F23, "F23"),
    (GLFW_FKEY_F24, "F24"),
    (GLFW_FKEY_F25, "F25"),
    (GLFW_FKEY_F26, "F26"),
    (GLFW_FKEY_F27, "F27"),
    (GLFW_FKEY_F28, "F28"),
    (GLFW_FKEY_F29, "F29"),
    (GLFW_FKEY_F30, "F30"),
    (GLFW_FKEY_F31, "F31"),
    (GLFW_FKEY_F32, "F32"),
    (GLFW_FKEY_F33, "F33"),
    (GLFW_FKEY_F34, "F34"),
    (GLFW_FKEY_F35, "F35"),
    (GLFW_FKEY_KP_0, "KP_0"),
    (GLFW_FKEY_KP_1, "KP_1"),
    (GLFW_FKEY_KP_2, "KP_2"),
    (GLFW_FKEY_KP_3, "KP_3"),
    (GLFW_FKEY_KP_4, "KP_4"),
    (GLFW_FKEY_KP_5, "KP_5"),
    (GLFW_FKEY_KP_6, "KP_6"),
    (GLFW_FKEY_KP_7, "KP_7"),
    (GLFW_FKEY_KP_8, "KP_8"),
    (GLFW_FKEY_KP_9, "KP_9"),
    (GLFW_FKEY_KP_DECIMAL, "KP_DECIMAL"),
    (GLFW_FKEY_KP_DIVIDE, "KP_DIVIDE"),
    (GLFW_FKEY_KP_MULTIPLY, "KP_MULTIPLY"),
    (GLFW_FKEY_KP_SUBTRACT, "KP_SUBTRACT"),
    (GLFW_FKEY_KP_ADD, "KP_ADD"),
    (GLFW_FKEY_KP_ENTER, "KP_ENTER"),
    (GLFW_FKEY_KP_EQUAL, "KP_EQUAL"),
    (GLFW_FKEY_KP_SEPARATOR, "KP_SEPARATOR"),
    (GLFW_FKEY_KP_LEFT, "KP_LEFT"),
    (GLFW_FKEY_KP_RIGHT, "KP_RIGHT"),
    (GLFW_FKEY_KP_UP, "KP_UP"),
    (GLFW_FKEY_KP_DOWN, "KP_DOWN"),
    (GLFW_FKEY_KP_PAGE_UP, "KP_PAGE_UP"),
    (GLFW_FKEY_KP_PAGE_DOWN, "KP_PAGE_DOWN"),
    (GLFW_FKEY_KP_HOME, "KP_HOME"),
    (GLFW_FKEY_KP_END, "KP_END"),
    (GLFW_FKEY_KP_INSERT, "KP_INSERT"),
    (GLFW_FKEY_KP_DELETE, "KP_DELETE"),
    (GLFW_FKEY_KP_BEGIN, "KP_BEGIN"),
    (GLFW_FKEY_MEDIA_PLAY, "MEDIA_PLAY"),
    (GLFW_FKEY_MEDIA_PAUSE, "MEDIA_PAUSE"),
    (GLFW_FKEY_MEDIA_PLAY_PAUSE, "MEDIA_PLAY_PAUSE"),
    (GLFW_FKEY_MEDIA_REVERSE, "MEDIA_REVERSE"),
    (GLFW_FKEY_MEDIA_STOP, "MEDIA_STOP"),
    (GLFW_FKEY_MEDIA_FAST_FORWARD, "MEDIA_FAST_FORWARD"),
    (GLFW_FKEY_MEDIA_REWIND, "MEDIA_REWIND"),
    (GLFW_FKEY_MEDIA_TRACK_NEXT, "MEDIA_TRACK_NEXT"),
    (GLFW_FKEY_MEDIA_TRACK_PREVIOUS, "MEDIA_TRACK_PREVIOUS"),
    (GLFW_FKEY_MEDIA_RECORD, "MEDIA_RECORD"),
    (GLFW_FKEY_LOWER_VOLUME, "LOWER_VOLUME"),
    (GLFW_FKEY_RAISE_VOLUME, "RAISE_VOLUME"),
    (GLFW_FKEY_MUTE_VOLUME, "MUTE_VOLUME"),
    (GLFW_FKEY_LEFT_SHIFT, "LEFT_SHIFT"),
    (GLFW_FKEY_LEFT_CONTROL, "LEFT_CONTROL"),
    (GLFW_FKEY_LEFT_ALT, "LEFT_ALT"),
    (GLFW_FKEY_LEFT_SUPER, "LEFT_SUPER"),
    (GLFW_FKEY_LEFT_HYPER, "LEFT_HYPER"),
    (GLFW_FKEY_LEFT_META, "LEFT_META"),
    (GLFW_FKEY_RIGHT_SHIFT, "RIGHT_SHIFT"),
    (GLFW_FKEY_RIGHT_CONTROL, "RIGHT_CONTROL"),
    (GLFW_FKEY_RIGHT_ALT, "RIGHT_ALT"),
    (GLFW_FKEY_RIGHT_SUPER, "RIGHT_SUPER"),
    (GLFW_FKEY_RIGHT_HYPER, "RIGHT_HYPER"),
    (GLFW_FKEY_RIGHT_META, "RIGHT_META"),
    (GLFW_FKEY_ISO_LEVEL3_SHIFT, "ISO_LEVEL3_SHIFT"),
    (GLFW_FKEY_ISO_LEVEL5_SHIFT, "ISO_LEVEL5_SHIFT"),
];

/// The historical `key_encoding.json` table, byte for byte.
pub const LEGACY_KEY_ENCODING: &[(&str, &str)] = &[
    ("0", "G"),
    ("1", "H"),
    ("2", "I"),
    ("3", "J"),
    ("4", "K"),
    ("5", "L"),
    ("6", "M"),
    ("7", "N"),
    ("8", "O"),
    ("9", "P"),
    ("A", "S"),
    ("APOSTROPHE", "B"),
    ("B", "T"),
    ("BACKSLASH", "t"),
    ("BACKSPACE", "1"),
    ("C", "U"),
    ("CAPS LOCK", ":"),
    ("COMMA", "C"),
    ("D", "V"),
    ("DELETE", "3"),
    ("DOWN", "6"),
    ("E", "W"),
    ("END", "-"),
    ("ENTER", "z"),
    ("EQUAL", "R"),
    ("ESCAPE", "y"),
    ("F", "X"),
    ("F1", "/"),
    ("F10", "]"),
    ("F11", "{"),
    ("F12", "}"),
    ("F13", "@"),
    ("F14", "%"),
    ("F15", "$"),
    ("F16", "#"),
    ("F17", "BA"),
    ("F18", "BB"),
    ("F19", "BC"),
    ("F2", "*"),
    ("F20", "BD"),
    ("F21", "BE"),
    ("F22", "BF"),
    ("F23", "BG"),
    ("F24", "BH"),
    ("F25", "BI"),
    ("F3", "?"),
    ("F4", "&"),
    ("F5", "<"),
    ("F6", ">"),
    ("F7", "("),
    ("F8", ")"),
    ("F9", "["),
    ("G", "Y"),
    ("GRAVE ACCENT", "v"),
    ("H", "Z"),
    ("HOME", "."),
    ("I", "a"),
    ("INSERT", "2"),
    ("J", "b"),
    ("K", "c"),
    ("KP 0", "BJ"),
    ("KP 1", "BK"),
    ("KP 2", "BL"),
    ("KP 3", "BM"),
    ("KP 4", "BN"),
    ("KP 5", "BO"),
    ("KP 6", "BP"),
    ("KP 7", "BQ"),
    ("KP 8", "BR"),
    ("KP 9", "BS"),
    ("KP ADD", "BX"),
    ("KP DECIMAL", "BT"),
    ("KP DIVIDE", "BU"),
    ("KP ENTER", "BY"),
    ("KP EQUAL", "BZ"),
    ("KP MULTIPLY", "BV"),
    ("KP SUBTRACT", "BW"),
    ("L", "d"),
    ("LEFT", "5"),
    ("LEFT ALT", "Bc"),
    ("LEFT BRACKET", "s"),
    ("LEFT CONTROL", "Bb"),
    ("LEFT SHIFT", "Ba"),
    ("LEFT SUPER", "Bd"),
    ("M", "e"),
    ("MINUS", "D"),
    ("N", "f"),
    ("NUM LOCK", "="),
    ("O", "g"),
    ("P", "h"),
    ("PAGE DOWN", "9"),
    ("PAGE UP", "8"),
    ("PAUSE", "!"),
    ("PERIOD", "E"),
    ("PRINT SCREEN", "^"),
    ("Q", "i"),
    ("R", "j"),
    ("RIGHT", "4"),
    ("RIGHT ALT", "Bg"),
    ("RIGHT BRACKET", "u"),
    ("RIGHT CONTROL", "Bf"),
    ("RIGHT SHIFT", "Be"),
    ("RIGHT SUPER", "Bh"),
    ("S", "k"),
    ("SCROLL LOCK", "+"),
    ("SEMICOLON", "Q"),
    ("SLASH", "F"),
    ("SPACE", "A"),
    ("T", "l"),
    ("TAB", "0"),
    ("U", "m"),
    ("UP", "7"),
    ("V", "n"),
    ("W", "o"),
    ("WORLD 1", "w"),
    ("WORLD 2", "x"),
    ("X", "p"),
    ("Y", "q"),
    ("Z", "r"),
];

pub fn key_to_name(key: u32) -> Option<&'static str> {
    FUNCTIONAL_KEY_NAMES
        .iter()
        .find_map(|&(code, name)| (code == key).then_some(name))
}

fn normalize(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c == '_' || c == '-' {
                ' '
            } else {
                c.to_ascii_uppercase()
            }
        })
        .collect()
}

/// Parse a functional key name, accepting both `PAGE_UP` and `PAGE UP`.
pub fn functional_key_number(name: &str) -> Option<u32> {
    let normalized = normalize(name);
    FUNCTIONAL_KEY_NAMES
        .iter()
        .find_map(|&(code, canonical)| (normalize(canonical) == normalized).then_some(code))
}

/// Resolve a key name from a config shortcut. Returns `None` for unknown names.
///
/// Single character names are returned as their scalar value, preserving case,
/// which is what `kitty.key_encoding.parse_shortcut` does for character keys.
pub fn key_from_name(name: &str) -> Option<u32> {
    let upper = name.to_ascii_uppercase();
    let functional_alias = functional_alias(&upper);
    if let Some(alias) = functional_alias {
        return functional_key_number(alias);
    }
    if let Some(code) = functional_key_number(name) {
        return Some(code);
    }
    if let Some(alias) = character_alias(&upper) {
        return alias.chars().next().map(|c| c as u32);
    }
    let mut chars = name.chars();
    let first = chars.next()?;
    if chars.next().is_none() {
        Some(first as u32)
    } else {
        None
    }
}

pub fn functional_alias(name: &str) -> Option<&'static str> {
    Some(match name {
        "ESC" => "ESCAPE",
        "PGUP" | "PAGEUP" => "PAGE_UP",
        "PGDN" | "PAGEDOWN" => "PAGE_DOWN",
        "RETURN" => "ENTER",
        "ARROWUP" => "UP",
        "ARROWDOWN" => "DOWN",
        "ARROWRIGHT" => "RIGHT",
        "ARROWLEFT" => "LEFT",
        "DEL" => "DELETE",
        "KP_PLUS" | "KP PLUS" => "KP_ADD",
        "KP_MINUS" | "KP MINUS" => "KP_SUBTRACT",
        "KP_PAGEUP" | "KP PAGEUP" => "KP_PAGE_UP",
        "KP_PAGEDOWN" | "KP PAGEDOWN" => "KP_PAGE_DOWN",
        _ => return None,
    })
}

pub fn character_alias(name: &str) -> Option<&'static str> {
    Some(match name {
        "SPC" | "SPACE" => " ",
        "STAR" | "MULTIPLY" => "*",
        "PLUS" => "+",
        "MINUS" | "HYPHEN" => "-",
        "BAR" | "PIPE" => "|",
        "EQUAL" => "=",
        "UNDERSCORE" => "_",
        "COMMA" => ",",
        "PERIOD" | "DOT" => ".",
        "SLASH" => "/",
        "BACKSLASH" => "\\",
        "TILDE" => "~",
        "GRAVE" | "GRAVE_ACCENT" | "GRAVE ACCENT" => "`",
        "APOSTROPHE" => "'",
        "SEMICOLON" => ";",
        "COLON" => ":",
        "LEFT_BRACKET" | "LEFT BRACKET" => "[",
        "RIGHT_BRACKET" | "RIGHT BRACKET" => "]",
        _ => return None,
    })
}

/// Look up the historical compact encoding value from `key_encoding.json`.
pub fn legacy_encoding_for_name(name: &str) -> Option<&'static str> {
    LEGACY_KEY_ENCODING
        .iter()
        .find_map(|&(k, v)| k.eq_ignore_ascii_case(name).then_some(v))
}

/// Inverse of [`legacy_encoding_for_name`].
pub fn legacy_name_for_encoding(encoding: &str) -> Option<&'static str> {
    LEGACY_KEY_ENCODING
        .iter()
        .find_map(|&(k, v)| (v == encoding).then_some(k))
}

/// True for modifier keys. Mirrors `is_modifier_key()` in `keys.c`.
pub fn is_modifier_key(key: u32) -> bool {
    matches!(
        key,
        GLFW_FKEY_LEFT_SHIFT
            ..=GLFW_FKEY_ISO_LEVEL5_SHIFT
                | GLFW_FKEY_CAPS_LOCK
                | GLFW_FKEY_SCROLL_LOCK
                | GLFW_FKEY_NUM_LOCK
    )
}

/// Keypad keys that can be translated to their non-keypad equivalents.
pub fn convert_kp_key_to_normal_key(mut key: u32) -> u32 {
    key = match key {
        GLFW_FKEY_KP_ENTER => GLFW_FKEY_ENTER,
        GLFW_FKEY_KP_HOME => GLFW_FKEY_HOME,
        GLFW_FKEY_KP_END => GLFW_FKEY_END,
        GLFW_FKEY_KP_INSERT => GLFW_FKEY_INSERT,
        GLFW_FKEY_KP_DELETE => GLFW_FKEY_DELETE,
        GLFW_FKEY_KP_PAGE_UP => GLFW_FKEY_PAGE_UP,
        GLFW_FKEY_KP_PAGE_DOWN => GLFW_FKEY_PAGE_DOWN,
        GLFW_FKEY_KP_UP => GLFW_FKEY_UP,
        GLFW_FKEY_KP_DOWN => GLFW_FKEY_DOWN,
        GLFW_FKEY_KP_LEFT => GLFW_FKEY_LEFT,
        GLFW_FKEY_KP_RIGHT => GLFW_FKEY_RIGHT,
        GLFW_FKEY_KP_0..=GLFW_FKEY_KP_9 => b'0' as u32 + (key - GLFW_FKEY_KP_0),
        GLFW_FKEY_KP_DECIMAL => b'.' as u32,
        GLFW_FKEY_KP_DIVIDE => b'/' as u32,
        GLFW_FKEY_KP_MULTIPLY => b'*' as u32,
        GLFW_FKEY_KP_SUBTRACT => b'-' as u32,
        GLFW_FKEY_KP_ADD => b'+' as u32,
        GLFW_FKEY_KP_EQUAL => b'=' as u32,
        _ => return key,
    };
    key
}

/// CSI number for a functional key used in the kitty keyboard protocol.
pub fn csi_number_for_key(key: u32) -> u32 {
    match key {
        GLFW_FKEY_ESCAPE => 27,
        GLFW_FKEY_ENTER => 13,
        GLFW_FKEY_TAB => 9,
        GLFW_FKEY_BACKSPACE => 127,
        GLFW_FKEY_INSERT => 2,
        GLFW_FKEY_DELETE => 3,
        GLFW_FKEY_PAGE_UP => 5,
        GLFW_FKEY_PAGE_DOWN => 6,
        GLFW_FKEY_HOME | GLFW_FKEY_END | GLFW_FKEY_UP | GLFW_FKEY_DOWN | GLFW_FKEY_LEFT
        | GLFW_FKEY_RIGHT | GLFW_FKEY_F1 | GLFW_FKEY_F2 | GLFW_FKEY_F4 | GLFW_FKEY_KP_BEGIN => 1,
        GLFW_FKEY_F3 => 13,
        GLFW_FKEY_F5 => 15,
        GLFW_FKEY_F6 => 17,
        GLFW_FKEY_F7 => 18,
        GLFW_FKEY_F8 => 19,
        GLFW_FKEY_F9 => 20,
        GLFW_FKEY_F10 => 21,
        GLFW_FKEY_F11 => 23,
        GLFW_FKEY_F12 => 24,
        GLFW_FKEY_MENU => 29,
        _ => key,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for &(code, name) in FUNCTIONAL_KEY_NAMES {
            assert_eq!(key_to_name(code), Some(name));
            assert_eq!(functional_key_number(name), Some(code));
        }
        assert_eq!(functional_key_number("page up"), Some(GLFW_FKEY_PAGE_UP));
        assert_eq!(functional_key_number("page-up"), Some(GLFW_FKEY_PAGE_UP));
    }

    #[test]
    fn aliases() {
        assert_eq!(key_from_name("ESC"), Some(GLFW_FKEY_ESCAPE));
        assert_eq!(key_from_name("return"), Some(GLFW_FKEY_ENTER));
        assert_eq!(key_from_name("SPACE"), Some(b' ' as u32));
        assert_eq!(key_from_name("A"), Some(b'A' as u32));
        assert_eq!(key_from_name("a"), Some(b'a' as u32));
        assert_eq!(key_from_name("F35"), Some(GLFW_FKEY_F35));
        assert_eq!(key_from_name("not a key"), None);
    }

    #[test]
    fn historical_json_table_is_complete() {
        assert_eq!(LEGACY_KEY_ENCODING.len(), 119);
        assert_eq!(legacy_encoding_for_name("F17"), Some("BA"));
        assert_eq!(legacy_encoding_for_name("KP ENTER"), Some("BY"));
        assert_eq!(legacy_name_for_encoding("BY"), Some("KP ENTER"));
    }

    #[test]
    fn kp_conversion() {
        assert_eq!(convert_kp_key_to_normal_key(GLFW_FKEY_KP_5), b'5' as u32);
        assert_eq!(
            convert_kp_key_to_normal_key(GLFW_FKEY_KP_ENTER),
            GLFW_FKEY_ENTER
        );
        assert_eq!(
            convert_kp_key_to_normal_key(GLFW_FKEY_KP_BEGIN),
            GLFW_FKEY_KP_BEGIN
        );
        assert!(is_modifier_key(GLFW_FKEY_LEFT_SHIFT));
        assert!(!is_modifier_key(GLFW_FKEY_LEFT));
    }
}
