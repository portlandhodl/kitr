// SPDX-License-Identifier: GPL-3.0-or-later
//! Keyboard and mouse input ported from `kitty/keys.c`,
//! `kitty/key_encoding.c` and `kitty/mouse.c`.
//!
//! The port is self-contained: it does not link GLFW and does not depend on
//! the screen crate. A frontend supplies the few screen fields needed by the
//! input layer through [`keys::InputContext`] (or uses the bundled
//! [`keys::ScreenInputContext`]).
//!
//! ```
//! use kitty_input::{encode_glfw_key_event, EncodedKey, KeyEvent, KittyKeyboardFlags};
//! use kitty_input::key_constants::*;
//!
//! let ev = KeyEvent::press(GLFW_FKEY_UP);
//! assert_eq!(
//!     encode_glfw_key_event(&ev, false, KittyKeyboardFlags::NONE),
//!     EncodedKey::Bytes(b"\x1b[A".to_vec())
//! );
//! ```

pub mod key;
pub mod key_constants;
pub mod key_encoding;
pub mod key_names;
pub mod keys;
pub mod mouse;

pub use key::{KeyAction, KeyEvent, Modifiers};
pub use key_constants::*;
pub use key_encoding::{
    encode_glfw_key_event, encode_key, encode_key_for_tty, EncodedKey, KeyEncodingStack,
    KittyKeyboardFlags,
};
pub use key_names::{
    convert_kp_key_to_normal_key, functional_key_number, is_modifier_key, key_from_name,
    key_to_name, legacy_encoding_for_name, legacy_name_for_encoding, FUNCTIONAL_KEY_NAMES,
    LEGACY_KEY_ENCODING,
};
pub use keys::{
    application_keypad_bytes, parse_shortcut, shortcut_candidates, InputContext, InputModes,
    KeyDispatcher, KeyMap, KeyOutput, MappingResult, ScreenInputContext, Shortcut, ShortcutHandler,
};
pub use mouse::{
    button_map, cell_for_pos, dispatch_mouse_event, encode_button_number, encode_mouse_button,
    encode_mouse_event, encode_mouse_event_scaled, encode_mouse_scroll, should_send_mouse_event,
    MouseAction, MouseButton, MouseGeometry, MousePosition, MouseShape, MouseTrackingMode,
    MouseTrackingProtocol,
};

/// Re-export of the core crate, matching the other ported crates.
pub use kitty_core as core;
