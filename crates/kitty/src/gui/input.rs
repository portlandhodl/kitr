// SPDX-License-Identifier: GPL-3.0-or-later
//! Keyboard/mouse translation and screen-mode tracking.
//!
//! `kitty-screen` stores the boolean DEC modes but not the mouse-protocol
//! modes or the kitty keyboard flag stack. A second small parser observes the
//! same PTY byte stream and keeps that extra state here without touching the
//! screen crate.

use kitty_input::key_constants::*;
use kitty_input::keys::{InputContext, InputModes, ScreenInputContext};
use kitty_input::mouse::{
    cell_for_pos, dispatch_mouse_event, MouseAction, MouseGeometry, MousePosition,
    MouseTrackingMode, MouseTrackingProtocol,
};
use kitty_input::{KeyAction, KeyDispatcher, KeyEncodingStack, KeyEvent, KeyOutput, Modifiers};
use kitty_screen::{Modes, Screen};
use kitty_vt::{CsiParams, Handler, Parser};
use kitty_window::{keysym, KeyAction as XKeyAction, KeyInput};

const XK_INSERT: u32 = 0xff63;
const XK_KP_0: u32 = 0xffb0;
const XK_KP_9: u32 = 0xffb9;
const XK_KP_DECIMAL: u32 = 0xffae;
const XK_KP_DIVIDE: u32 = 0xffaf;
const XK_KP_MULTIPLY: u32 = 0xffaa;
const XK_KP_SUBTRACT: u32 = 0xffad;
const XK_KP_ADD: u32 = 0xffab;
const XK_KP_ENTER: u32 = 0xff8d;
const XK_KP_EQUAL: u32 = 0xffbd;
const XK_KP_SEPARATOR: u32 = 0xffac;
const XK_F1: u32 = 0xffbe;
const XK_F35: u32 = 0xffe0;

/// Extra state parsed from the child's escape sequences.
pub struct ModeTracker {
    parser: Parser,
    pub mouse_mode: MouseTrackingMode,
    pub mouse_protocol: MouseTrackingProtocol,
    pub deckpam: bool,
    pub key_flags: KeyEncodingStack,
    pub title: Option<String>,
}

impl Default for ModeTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl ModeTracker {
    pub fn new() -> Self {
        Self {
            parser: Parser::new(),
            mouse_mode: MouseTrackingMode::Off,
            mouse_protocol: MouseTrackingProtocol::Normal,
            deckpam: false,
            key_flags: KeyEncodingStack::new(),
            title: None,
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        let mut parser = std::mem::replace(&mut self.parser, Parser::new());
        parser.parse(bytes, self);
        self.parser = parser;
    }

    fn apply_private_mode(&mut self, mode: i32, set: bool) {
        match mode {
            1000 => {
                self.mouse_mode = if set {
                    MouseTrackingMode::Click
                } else {
                    MouseTrackingMode::Off
                }
            }
            1002 => {
                self.mouse_mode = if set {
                    MouseTrackingMode::Motion
                } else {
                    MouseTrackingMode::Off
                }
            }
            1003 => {
                self.mouse_mode = if set {
                    MouseTrackingMode::Any
                } else {
                    MouseTrackingMode::Off
                }
            }
            1005 => {
                self.mouse_protocol = if set {
                    MouseTrackingProtocol::Utf8
                } else {
                    MouseTrackingProtocol::Normal
                }
            }
            1006 => {
                self.mouse_protocol = if set {
                    MouseTrackingProtocol::Sgr
                } else {
                    MouseTrackingProtocol::Normal
                }
            }
            1015 => {
                self.mouse_protocol = if set {
                    MouseTrackingProtocol::Urxvt
                } else {
                    MouseTrackingProtocol::Normal
                }
            }
            1016 => {
                self.mouse_protocol = if set {
                    MouseTrackingProtocol::SgrPixel
                } else {
                    MouseTrackingProtocol::Normal
                }
            }
            _ => {}
        }
    }
}

impl Handler for ModeTracker {
    fn csi_dispatch(&mut self, params: &CsiParams, _intermediates: &[u8], action: u8) {
        let primary = params.primary();
        if (action == b'h' || action == b'l') && primary == b'?' {
            for mode in params.raw_params() {
                if *mode >= 0 {
                    self.apply_private_mode(*mode, action == b'h');
                }
            }
        } else if action == b'u' {
            let value = params.param_or(0, 0).max(0) as u32;
            match primary {
                b'>' => self.key_flags.push(value),
                b'<' => self.key_flags.pop(value),
                b'=' => self.key_flags.set(value, 1),
                _ => {}
            }
        }
    }

    fn esc_dispatch(&mut self, _intermediates: &[u8], action: u8) {
        match action {
            b'=' => self.deckpam = true,
            b'>' => self.deckpam = false,
            _ => {}
        }
    }

    fn osc_dispatch(&mut self, data: &[u8]) {
        if let Some(rest) = data.strip_prefix(b"2;") {
            self.title = Some(String::from_utf8_lossy(rest).into_owned());
        }
    }
}

/// Build the input context for the current screen and tracked modes.
pub fn input_context(screen: &Screen, tracker: &ModeTracker) -> ScreenInputContext {
    let mut ctx = ScreenInputContext::new();
    ctx.modes = InputModes {
        decckm: screen.modes.contains(Modes::DECCKM),
        deckpam: tracker.deckpam,
        decarm: screen.modes.contains(Modes::DECARM),
        mouse_tracking_mode: tracker.mouse_mode,
        mouse_tracking_protocol: tracker.mouse_protocol,
    };
    ctx.key_encoding_stack = tracker.key_flags.clone();
    ctx
}

fn keysym_char(symbol: u32) -> Option<char> {
    let code = if symbol & 0xff00_0000 == 0x0100_0000 {
        symbol & 0x00ff_ffff
    } else if (0x20..=0x7e).contains(&symbol) || (0xa0..=0xff).contains(&symbol) {
        symbol
    } else {
        return None;
    };
    char::from_u32(code)
}

fn functional_key(symbol: u32) -> Option<u32> {
    Some(match symbol {
        keysym::BACKSPACE => GLFW_FKEY_BACKSPACE,
        keysym::TAB => GLFW_FKEY_TAB,
        keysym::RETURN => GLFW_FKEY_ENTER,
        keysym::ESCAPE => GLFW_FKEY_ESCAPE,
        keysym::DELETE => GLFW_FKEY_DELETE,
        keysym::HOME => GLFW_FKEY_HOME,
        keysym::LEFT => GLFW_FKEY_LEFT,
        keysym::UP => GLFW_FKEY_UP,
        keysym::RIGHT => GLFW_FKEY_RIGHT,
        keysym::DOWN => GLFW_FKEY_DOWN,
        keysym::PAGE_UP => GLFW_FKEY_PAGE_UP,
        keysym::PAGE_DOWN => GLFW_FKEY_PAGE_DOWN,
        keysym::END => GLFW_FKEY_END,
        XK_INSERT => GLFW_FKEY_INSERT,
        keysym::MENU => GLFW_FKEY_MENU,
        keysym::SHIFT_L => GLFW_FKEY_LEFT_SHIFT,
        keysym::SHIFT_R => GLFW_FKEY_RIGHT_SHIFT,
        keysym::CONTROL_L => GLFW_FKEY_LEFT_CONTROL,
        keysym::CONTROL_R => GLFW_FKEY_RIGHT_CONTROL,
        keysym::ALT_L => GLFW_FKEY_LEFT_ALT,
        keysym::ALT_R => GLFW_FKEY_RIGHT_ALT,
        keysym::SUPER_L => GLFW_FKEY_LEFT_SUPER,
        keysym::SUPER_R => GLFW_FKEY_RIGHT_SUPER,
        keysym::HYPER_L => GLFW_FKEY_LEFT_HYPER,
        keysym::HYPER_R => GLFW_FKEY_RIGHT_HYPER,
        keysym::META_L => GLFW_FKEY_LEFT_META,
        keysym::META_R => GLFW_FKEY_RIGHT_META,
        keysym::ISO_LEVEL3_SHIFT => GLFW_FKEY_ISO_LEVEL3_SHIFT,
        XK_KP_ENTER => GLFW_FKEY_KP_ENTER,
        XK_KP_DECIMAL => GLFW_FKEY_KP_DECIMAL,
        XK_KP_DIVIDE => GLFW_FKEY_KP_DIVIDE,
        XK_KP_MULTIPLY => GLFW_FKEY_KP_MULTIPLY,
        XK_KP_SUBTRACT => GLFW_FKEY_KP_SUBTRACT,
        XK_KP_ADD => GLFW_FKEY_KP_ADD,
        XK_KP_EQUAL => GLFW_FKEY_KP_EQUAL,
        XK_KP_SEPARATOR => GLFW_FKEY_KP_SEPARATOR,
        symbol if (XK_F1..=XK_F35).contains(&symbol) => GLFW_FKEY_F1 + (symbol - XK_F1),
        symbol if (XK_KP_0..=XK_KP_9).contains(&symbol) => GLFW_FKEY_KP_0 + (symbol - XK_KP_0),
        _ => return None,
    })
}

pub fn key_event(input: &KeyInput) -> KeyEvent {
    let functional = functional_key(input.keysym).or_else(|| functional_key(input.base_keysym));
    let base_char = keysym_char(input.base_keysym).or_else(|| keysym_char(input.keysym));
    let effective_char = keysym_char(input.keysym);
    let key = functional.unwrap_or_else(|| {
        base_char
            .map(|ch| {
                if ch.is_ascii_uppercase() {
                    ch.to_ascii_lowercase() as u32
                } else {
                    ch as u32
                }
            })
            .unwrap_or(0)
    });
    let shifted_key = if input.modifiers.shift {
        effective_char
            .filter(|ch| Some(*ch) != base_char)
            .map(|ch| ch as u32)
            .unwrap_or(0)
    } else {
        0
    };
    let mut modifiers = input.modifiers.glfw_bits();
    if input.modifiers.caps_lock {
        modifiers |= GLFW_MOD_CAPS_LOCK;
    }
    if input.modifiers.num_lock {
        modifiers |= GLFW_MOD_NUM_LOCK;
    }
    let text = if input.modifiers.control
        || input.modifiers.alt
        || input.modifiers.super_key
        || input.modifiers.meta
        || input.modifiers.hyper
    {
        None
    } else {
        input.text.clone()
    };
    KeyEvent {
        key,
        shifted_key,
        alternate_key: 0,
        native_key: input.keycode,
        modifiers: Modifiers(modifiers),
        action: match input.action {
            XKeyAction::Press => KeyAction::Press,
            XKeyAction::Release => KeyAction::Release,
            XKeyAction::Repeat => KeyAction::Repeat,
        },
        text,
    }
}

pub fn encode_key_for_screen(event: &KeyEvent, ctx: &ScreenInputContext) -> KeyOutput {
    let dispatcher = KeyDispatcher::new(|_: &KeyEvent| false);
    dispatcher.encode_for_child(event, ctx)
}

/// Result of a mouse position lookup.
#[derive(Clone, Copy, Debug, Default)]
pub struct MouseGeometryState {
    pub left: f64,
    pub top: f64,
    pub cell_width: f64,
    pub cell_height: f64,
    pub columns: u32,
    pub lines: u32,
}

impl MouseGeometryState {
    pub fn position(&self, x: f64, y: f64) -> Option<MousePosition> {
        cell_for_pos(
            x,
            y,
            &MouseGeometry {
                left: self.left,
                top: self.top,
                right: self.left + self.cell_width * self.columns as f64,
                bottom: self.top + self.cell_height * self.lines as f64,
                cell_width: self.cell_width,
                cell_height: self.cell_height,
                columns: self.columns,
                lines: self.lines,
                pixel_scroll_offset_y: 0.0,
            },
            true,
            1.0,
        )
    }
}

pub fn mouse_modifiers(modifiers: kitty_window::Modifiers) -> Modifiers {
    let mut bits = modifiers.glfw_bits();
    if modifiers.caps_lock {
        bits |= GLFW_MOD_CAPS_LOCK;
    }
    if modifiers.num_lock {
        bits |= GLFW_MOD_NUM_LOCK;
    }
    Modifiers(bits)
}

pub fn mouse_button(button: kitty_window::MouseButton) -> Option<kitty_input::mouse::MouseButton> {
    use kitty_input::mouse::MouseButton as K;
    use kitty_window::MouseButton as X;
    Some(match button {
        X::Left => K::Left,
        X::Middle => K::Middle,
        X::Right => K::Right,
        X::Back => K::Button4,
        X::Forward => K::Button5,
        X::Other(_) => return None,
    })
}

pub fn send_mouse(
    ctx: &ScreenInputContext,
    position: &MousePosition,
    button: Option<kitty_input::mouse::MouseButton>,
    action: MouseAction,
    modifiers: Modifiers,
) -> Option<Vec<u8>> {
    dispatch_mouse_event(ctx, position, button, action, modifiers)
}

/// Whether a press/release should be sent to the child rather than used for a
/// local selection or scrollback movement.
pub fn mouse_goes_to_child(ctx: &dyn InputContext, modifiers: Modifiers) -> bool {
    ctx.modes().mouse_tracking_mode != MouseTrackingMode::Off && !modifiers.shift()
}
