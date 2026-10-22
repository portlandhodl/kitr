// SPDX-License-Identifier: GPL-3.0-or-later
//! Keyboard and mouse constants used by kitty.
//!
//! Checked-in copy of the constants that `gen/key_constants.py` writes into
//! `glfw/glfw3.h`, `kitty/glfw-wrapper.h` and `kitty/data-types.h`. Keeping
//! these here means the crate has no build script and never links against
//! GLFW. The source of truth remains `gen/key_constants.py`; regenerate that
//! file first, then update this module if kitty adds more function keys.

/// First functional key (`GLFW_FKEY_FIRST`).
pub const GLFW_FKEY_FIRST: u32 = 0xe000;
pub const GLFW_FKEY_ESCAPE: u32 = 0xe000;
pub const GLFW_FKEY_ENTER: u32 = 0xe001;
pub const GLFW_FKEY_TAB: u32 = 0xe002;
pub const GLFW_FKEY_BACKSPACE: u32 = 0xe003;
pub const GLFW_FKEY_INSERT: u32 = 0xe004;
pub const GLFW_FKEY_DELETE: u32 = 0xe005;
pub const GLFW_FKEY_LEFT: u32 = 0xe006;
pub const GLFW_FKEY_RIGHT: u32 = 0xe007;
pub const GLFW_FKEY_UP: u32 = 0xe008;
pub const GLFW_FKEY_DOWN: u32 = 0xe009;
pub const GLFW_FKEY_PAGE_UP: u32 = 0xe00a;
pub const GLFW_FKEY_PAGE_DOWN: u32 = 0xe00b;
pub const GLFW_FKEY_HOME: u32 = 0xe00c;
pub const GLFW_FKEY_END: u32 = 0xe00d;
pub const GLFW_FKEY_CAPS_LOCK: u32 = 0xe00e;
pub const GLFW_FKEY_SCROLL_LOCK: u32 = 0xe00f;
pub const GLFW_FKEY_NUM_LOCK: u32 = 0xe010;
pub const GLFW_FKEY_PRINT_SCREEN: u32 = 0xe011;
pub const GLFW_FKEY_PAUSE: u32 = 0xe012;
pub const GLFW_FKEY_MENU: u32 = 0xe013;
pub const GLFW_FKEY_F1: u32 = 0xe014;
pub const GLFW_FKEY_F2: u32 = 0xe015;
pub const GLFW_FKEY_F3: u32 = 0xe016;
pub const GLFW_FKEY_F4: u32 = 0xe017;
pub const GLFW_FKEY_F5: u32 = 0xe018;
pub const GLFW_FKEY_F6: u32 = 0xe019;
pub const GLFW_FKEY_F7: u32 = 0xe01a;
pub const GLFW_FKEY_F8: u32 = 0xe01b;
pub const GLFW_FKEY_F9: u32 = 0xe01c;
pub const GLFW_FKEY_F10: u32 = 0xe01d;
pub const GLFW_FKEY_F11: u32 = 0xe01e;
pub const GLFW_FKEY_F12: u32 = 0xe01f;
pub const GLFW_FKEY_F13: u32 = 0xe020;
pub const GLFW_FKEY_F14: u32 = 0xe021;
pub const GLFW_FKEY_F15: u32 = 0xe022;
pub const GLFW_FKEY_F16: u32 = 0xe023;
pub const GLFW_FKEY_F17: u32 = 0xe024;
pub const GLFW_FKEY_F18: u32 = 0xe025;
pub const GLFW_FKEY_F19: u32 = 0xe026;
pub const GLFW_FKEY_F20: u32 = 0xe027;
pub const GLFW_FKEY_F21: u32 = 0xe028;
pub const GLFW_FKEY_F22: u32 = 0xe029;
pub const GLFW_FKEY_F23: u32 = 0xe02a;
pub const GLFW_FKEY_F24: u32 = 0xe02b;
pub const GLFW_FKEY_F25: u32 = 0xe02c;
pub const GLFW_FKEY_F26: u32 = 0xe02d;
pub const GLFW_FKEY_F27: u32 = 0xe02e;
pub const GLFW_FKEY_F28: u32 = 0xe02f;
pub const GLFW_FKEY_F29: u32 = 0xe030;
pub const GLFW_FKEY_F30: u32 = 0xe031;
pub const GLFW_FKEY_F31: u32 = 0xe032;
pub const GLFW_FKEY_F32: u32 = 0xe033;
pub const GLFW_FKEY_F33: u32 = 0xe034;
pub const GLFW_FKEY_F34: u32 = 0xe035;
pub const GLFW_FKEY_F35: u32 = 0xe036;
pub const GLFW_FKEY_KP_0: u32 = 0xe037;
pub const GLFW_FKEY_KP_1: u32 = 0xe038;
pub const GLFW_FKEY_KP_2: u32 = 0xe039;
pub const GLFW_FKEY_KP_3: u32 = 0xe03a;
pub const GLFW_FKEY_KP_4: u32 = 0xe03b;
pub const GLFW_FKEY_KP_5: u32 = 0xe03c;
pub const GLFW_FKEY_KP_6: u32 = 0xe03d;
pub const GLFW_FKEY_KP_7: u32 = 0xe03e;
pub const GLFW_FKEY_KP_8: u32 = 0xe03f;
pub const GLFW_FKEY_KP_9: u32 = 0xe040;
pub const GLFW_FKEY_KP_DECIMAL: u32 = 0xe041;
pub const GLFW_FKEY_KP_DIVIDE: u32 = 0xe042;
pub const GLFW_FKEY_KP_MULTIPLY: u32 = 0xe043;
pub const GLFW_FKEY_KP_SUBTRACT: u32 = 0xe044;
pub const GLFW_FKEY_KP_ADD: u32 = 0xe045;
pub const GLFW_FKEY_KP_ENTER: u32 = 0xe046;
pub const GLFW_FKEY_KP_EQUAL: u32 = 0xe047;
pub const GLFW_FKEY_KP_SEPARATOR: u32 = 0xe048;
pub const GLFW_FKEY_KP_LEFT: u32 = 0xe049;
pub const GLFW_FKEY_KP_RIGHT: u32 = 0xe04a;
pub const GLFW_FKEY_KP_UP: u32 = 0xe04b;
pub const GLFW_FKEY_KP_DOWN: u32 = 0xe04c;
pub const GLFW_FKEY_KP_PAGE_UP: u32 = 0xe04d;
pub const GLFW_FKEY_KP_PAGE_DOWN: u32 = 0xe04e;
pub const GLFW_FKEY_KP_HOME: u32 = 0xe04f;
pub const GLFW_FKEY_KP_END: u32 = 0xe050;
pub const GLFW_FKEY_KP_INSERT: u32 = 0xe051;
pub const GLFW_FKEY_KP_DELETE: u32 = 0xe052;
pub const GLFW_FKEY_KP_BEGIN: u32 = 0xe053;
pub const GLFW_FKEY_MEDIA_PLAY: u32 = 0xe054;
pub const GLFW_FKEY_MEDIA_PAUSE: u32 = 0xe055;
pub const GLFW_FKEY_MEDIA_PLAY_PAUSE: u32 = 0xe056;
pub const GLFW_FKEY_MEDIA_REVERSE: u32 = 0xe057;
pub const GLFW_FKEY_MEDIA_STOP: u32 = 0xe058;
pub const GLFW_FKEY_MEDIA_FAST_FORWARD: u32 = 0xe059;
pub const GLFW_FKEY_MEDIA_REWIND: u32 = 0xe05a;
pub const GLFW_FKEY_MEDIA_TRACK_NEXT: u32 = 0xe05b;
pub const GLFW_FKEY_MEDIA_TRACK_PREVIOUS: u32 = 0xe05c;
pub const GLFW_FKEY_MEDIA_RECORD: u32 = 0xe05d;
pub const GLFW_FKEY_LOWER_VOLUME: u32 = 0xe05e;
pub const GLFW_FKEY_RAISE_VOLUME: u32 = 0xe05f;
pub const GLFW_FKEY_MUTE_VOLUME: u32 = 0xe060;
pub const GLFW_FKEY_LEFT_SHIFT: u32 = 0xe061;
pub const GLFW_FKEY_LEFT_CONTROL: u32 = 0xe062;
pub const GLFW_FKEY_LEFT_ALT: u32 = 0xe063;
pub const GLFW_FKEY_LEFT_SUPER: u32 = 0xe064;
pub const GLFW_FKEY_LEFT_HYPER: u32 = 0xe065;
pub const GLFW_FKEY_LEFT_META: u32 = 0xe066;
pub const GLFW_FKEY_RIGHT_SHIFT: u32 = 0xe067;
pub const GLFW_FKEY_RIGHT_CONTROL: u32 = 0xe068;
pub const GLFW_FKEY_RIGHT_ALT: u32 = 0xe069;
pub const GLFW_FKEY_RIGHT_SUPER: u32 = 0xe06a;
pub const GLFW_FKEY_RIGHT_HYPER: u32 = 0xe06b;
pub const GLFW_FKEY_RIGHT_META: u32 = 0xe06c;
pub const GLFW_FKEY_ISO_LEVEL3_SHIFT: u32 = 0xe06d;
pub const GLFW_FKEY_ISO_LEVEL5_SHIFT: u32 = 0xe06e;
/// Last functional key (`GLFW_FKEY_LAST`).
pub const GLFW_FKEY_LAST: u32 = 0xe06e;

pub const GLFW_MOD_SHIFT: u32 = 0x0001;
pub const GLFW_MOD_ALT: u32 = 0x0002;
pub const GLFW_MOD_CONTROL: u32 = 0x0004;
pub const GLFW_MOD_SUPER: u32 = 0x0008;
pub const GLFW_MOD_HYPER: u32 = 0x0010;
pub const GLFW_MOD_META: u32 = 0x0020;
pub const GLFW_MOD_CAPS_LOCK: u32 = 0x0040;
pub const GLFW_MOD_NUM_LOCK: u32 = 0x0080;
pub const GLFW_MOD_LAST: u32 = GLFW_MOD_NUM_LOCK;
pub const GLFW_LOCK_MASK: u32 = GLFW_MOD_NUM_LOCK | GLFW_MOD_CAPS_LOCK;
/// `GLFW_MOD_KITTY` from `kitty/data-types.h`.
pub const GLFW_MOD_KITTY: u32 = GLFW_MOD_LAST * 2;

pub const GLFW_MOUSE_BUTTON_LEFT: i32 = 0;
pub const GLFW_MOUSE_BUTTON_RIGHT: i32 = 1;
pub const GLFW_MOUSE_BUTTON_MIDDLE: i32 = 2;
pub const GLFW_MOUSE_BUTTON_4: i32 = 3;
pub const GLFW_MOUSE_BUTTON_5: i32 = 4;
pub const GLFW_MOUSE_BUTTON_6: i32 = 5;
pub const GLFW_MOUSE_BUTTON_7: i32 = 6;
pub const GLFW_MOUSE_BUTTON_8: i32 = 7;
pub const GLFW_MOUSE_BUTTON_LAST: i32 = 7;

pub const GLFW_RELEASE: i32 = 0;
pub const GLFW_PRESS: i32 = 1;
pub const GLFW_REPEAT: i32 = 2;

/// Size of the encoded key buffer in `kitty/keys.h`.
pub const KEY_BUFFER_SIZE: usize = 128;

/// Sentinel returned by `encode_glfw_key_event` when the text should be sent
/// to the child unchanged (`SEND_TEXT_TO_CHILD`).
pub const SEND_TEXT_TO_CHILD: isize = isize::MIN;

/// Mouse pseudo-button numbers used by config mouse mappings.
pub const MOUSE_WHEEL_UP: i32 = GLFW_MOUSE_BUTTON_LAST + 1;
pub const MOUSE_WHEEL_DOWN: i32 = GLFW_MOUSE_BUTTON_LAST + 2;
pub const MOUSE_WHEEL_LEFT: i32 = GLFW_MOUSE_BUTTON_LAST + 3;
pub const MOUSE_WHEEL_RIGHT: i32 = GLFW_MOUSE_BUTTON_LAST + 4;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_match_kitty_layout() {
        assert_eq!(GLFW_FKEY_LAST - GLFW_FKEY_FIRST, 110);
        assert_eq!(GLFW_FKEY_KP_0, 0xe037);
        assert_eq!(GLFW_FKEY_KP_BEGIN, 0xe053);
        assert_eq!(GLFW_MOD_KITTY, 0x100);
        assert_eq!(GLFW_LOCK_MASK, 0xc0);
    }
}
