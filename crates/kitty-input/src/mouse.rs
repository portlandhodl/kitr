// SPDX-License-Identifier: GPL-3.0-or-later
//! Mouse tracking and mouse protocol encoding, ported from `kitty/mouse.c`
//! and the mode definitions in `kitty/data-types.h` / `kitty/screen.c`.
//!
//! The encoding functions are byte-for-byte ports of
//! `encode_mouse_event_impl()`; the selection/scrollbar/URL parts of mouse.c
//! depend on the screen and window layers and are intentionally out of scope.
//! [`cell_for_pos`] ports the coordinate mapping and clamping, and
//! [`encode_mouse_event_scaled`] exposes the pixel-coordinate scaling hook used
//! by SGR-pixel mode.

use crate::key::Modifiers;
use crate::key_constants::*;
use crate::keys::InputContext;

/// `MouseTrackingMode` from `data-types.h`, with the same numeric values.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum MouseTrackingMode {
    #[default]
    Off = 0,
    /// 1000: press/release only.
    Click = 1,
    /// 1002: press/release plus motion while a button is held.
    Motion = 2,
    /// 1003: all motion.
    Any = 3,
}

/// `MouseTrackingProtocol` from `data-types.h`, with the same numeric values.
///
/// The ordering matters: `RELEASE` rewrites the button field only for
/// protocols numerically below SGR.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum MouseTrackingProtocol {
    #[default]
    Normal = 0,
    Utf8 = 1,
    Sgr = 2,
    Urxvt = 3,
    SgrPixel = 4,
}

/// `MouseActions` from `mouse.c`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum MouseAction {
    Press,
    Release,
    Drag,
    Move,
    Leave,
}

/// Mouse buttons. The variants use GLFW's button ordering.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Button4,
    Button5,
    Button6,
    Button7,
    Button8,
    WheelUp,
    WheelDown,
    WheelLeft,
    WheelRight,
}

impl MouseButton {
    pub const fn from_glfw(button: i32) -> Option<Self> {
        Some(match button {
            GLFW_MOUSE_BUTTON_LEFT => Self::Left,
            GLFW_MOUSE_BUTTON_RIGHT => Self::Right,
            GLFW_MOUSE_BUTTON_MIDDLE => Self::Middle,
            GLFW_MOUSE_BUTTON_4 => Self::Button4,
            GLFW_MOUSE_BUTTON_5 => Self::Button5,
            GLFW_MOUSE_BUTTON_6 => Self::Button6,
            GLFW_MOUSE_BUTTON_7 => Self::Button7,
            GLFW_MOUSE_BUTTON_8 => Self::Button8,
            _ => return None,
        })
    }

    pub const fn glfw_code(self) -> i32 {
        match self {
            Self::Left => GLFW_MOUSE_BUTTON_LEFT,
            Self::Right => GLFW_MOUSE_BUTTON_RIGHT,
            Self::Middle => GLFW_MOUSE_BUTTON_MIDDLE,
            Self::Button4 => GLFW_MOUSE_BUTTON_4,
            Self::Button5 => GLFW_MOUSE_BUTTON_5,
            Self::Button6 => GLFW_MOUSE_BUTTON_6,
            Self::Button7 => GLFW_MOUSE_BUTTON_7,
            Self::Button8 => GLFW_MOUSE_BUTTON_8,
            Self::WheelUp => MOUSE_WHEEL_UP,
            Self::WheelDown => MOUSE_WHEEL_DOWN,
            Self::WheelLeft => MOUSE_WHEEL_LEFT,
            Self::WheelRight => MOUSE_WHEEL_RIGHT,
        }
    }

    /// The `button` value passed to `encode_mouse_event_impl()` in mouse.c.
    pub const fn encoding_code(self) -> u32 {
        match self {
            Self::Left => 1,
            Self::Right => 3,
            Self::Middle => 2,
            Self::WheelUp => 4,
            Self::WheelDown => 5,
            Self::WheelLeft => 6,
            Self::WheelRight => 7,
            Self::Button4 => 8,
            Self::Button5 => 9,
            Self::Button6 => 10,
            Self::Button7 => 11,
            Self::Button8 => 12,
        }
    }
}

/// `button_map()` from mouse.c: GLFW button to the config/normalised number.
pub const fn button_map(button: i32) -> u32 {
    match button {
        GLFW_MOUSE_BUTTON_LEFT => 1,
        GLFW_MOUSE_BUTTON_RIGHT => 3,
        GLFW_MOUSE_BUTTON_MIDDLE => 2,
        GLFW_MOUSE_BUTTON_4..=GLFW_MOUSE_BUTTON_8 => (button + 5) as u32,
        _ => u32::MAX,
    }
}

/// `encode_button()` from mouse.c.
pub const fn encode_button_number(button: u32) -> u32 {
    if button >= 8 && button <= 11 {
        (button - 8) | 128
    } else if button >= 4 && button <= 7 {
        (button - 4) | 64
    } else if button >= 1 && button <= 3 {
        button - 1
    } else {
        u32::MAX
    }
}

/// Mouse coordinates, matching `MousePosition` in mouse.c.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MousePosition {
    pub cell_x: u32,
    pub cell_y: u32,
    pub global_x: f64,
    pub global_y: f64,
    pub in_left_half_of_cell: bool,
}

impl MousePosition {
    pub const fn new(cell_x: u32, cell_y: u32) -> Self {
        Self {
            cell_x,
            cell_y,
            global_x: 0.0,
            global_y: 0.0,
            in_left_half_of_cell: true,
        }
    }

    pub const fn with_pixels(mut self, x: f64, y: f64) -> Self {
        self.global_x = x;
        self.global_y = y;
        self
    }

    pub const fn with_left_half(mut self, value: bool) -> Self {
        self.in_left_half_of_cell = value;
        self
    }
}

const SHIFT_INDICATOR: u32 = 1 << 2;
const ALT_INDICATOR: u32 = 1 << 3;
const CONTROL_INDICATOR: u32 = 1 << 4;
const MOTION_INDICATOR: u32 = 1 << 5;
const LEAVE_INDICATOR: u32 = 1 << 8;

/// Exact port of `encode_mouse_event_impl()`.
///
/// `button` is the normalised value from [`button_map`] / [`MouseButton::encoding_code`].
/// Use `u32::MAX` for movement with no button held. `pixel_scale` is applied
/// only in SGR-pixel mode (kitty uses 1.0).
pub fn encode_mouse_event_scaled(
    mpos: &MousePosition,
    protocol: MouseTrackingProtocol,
    button: u32,
    action: MouseAction,
    mods: Modifiers,
    pixel_scale: f64,
) -> Option<Vec<u8>> {
    let mut cb = encode_button_number(button);
    match action {
        MouseAction::Move => {
            if cb == u32::MAX {
                cb = 3;
            }
            cb += 32;
        }
        MouseAction::Leave => {
            if protocol != MouseTrackingProtocol::SgrPixel {
                return None;
            }
            cb = LEAVE_INDICATOR | MOTION_INDICATOR;
        }
        MouseAction::Press | MouseAction::Release | MouseAction::Drag => {
            if cb == u32::MAX {
                return None;
            }
        }
    }
    if matches!(action, MouseAction::Drag | MouseAction::Move) {
        cb |= MOTION_INDICATOR;
    } else if action == MouseAction::Release && protocol < MouseTrackingProtocol::Sgr {
        cb = 3;
    }
    if mods.shift() {
        cb |= SHIFT_INDICATOR;
    }
    if mods.alt() {
        cb |= ALT_INDICATOR;
    }
    if mods.ctrl() {
        cb |= CONTROL_INDICATOR;
    }
    let mut x = mpos.cell_x as i64 + 1;
    let mut y = mpos.cell_y as i64 + 1;
    match protocol {
        MouseTrackingProtocol::SgrPixel => {
            x = (mpos.global_x * pixel_scale).round() as i64;
            y = (mpos.global_y * pixel_scale).round() as i64;
            let trailer = if action == MouseAction::Release {
                b'm'
            } else {
                b'M'
            };
            Some(format!("<{cb};{x};{y}{}", trailer as char).into_bytes())
        }
        MouseTrackingProtocol::Sgr => {
            let trailer = if action == MouseAction::Release {
                b'm'
            } else {
                b'M'
            };
            Some(format!("<{cb};{x};{y}{}", trailer as char).into_bytes())
        }
        MouseTrackingProtocol::Urxvt => Some(format!("{};{x};{y}M", cb + 32).into_bytes()),
        MouseTrackingProtocol::Utf8 => {
            let mut out = vec![b'M'];
            out.push((cb + 32) as u8);
            encode_utf8(x as u32 + 32, &mut out);
            encode_utf8(y as u32 + 32, &mut out);
            Some(out)
        }
        MouseTrackingProtocol::Normal => {
            if x > 223 || y > 223 {
                None
            } else {
                Some(vec![b'M', (cb + 32) as u8, (x + 32) as u8, (y + 32) as u8])
            }
        }
    }
}

/// [`encode_mouse_event_scaled`] with kitty's default pixel scale of 1.0.
pub fn encode_mouse_event(
    mpos: &MousePosition,
    protocol: MouseTrackingProtocol,
    button: u32,
    action: MouseAction,
    mods: Modifiers,
) -> Option<Vec<u8>> {
    encode_mouse_event_scaled(mpos, protocol, button, action, mods, 1.0)
}

/// Typed convenience wrapper around [`encode_mouse_event`].
pub fn encode_mouse_button(
    mpos: &MousePosition,
    protocol: MouseTrackingProtocol,
    button: Option<MouseButton>,
    action: MouseAction,
    mods: Modifiers,
) -> Option<Vec<u8>> {
    let code = match button {
        Some(button) => button.encoding_code(),
        None => u32::MAX,
    };
    encode_mouse_event(mpos, protocol, code, action, mods)
}

/// Encode one wheel step. In mouse.c this is `encode_mouse_scroll()`:
/// wheel buttons 4..7 are encoded as a press.
pub fn encode_mouse_scroll(
    mpos: &MousePosition,
    protocol: MouseTrackingProtocol,
    button: MouseButton,
    mods: Modifiers,
) -> Option<Vec<u8>> {
    encode_mouse_event(
        mpos,
        protocol,
        button.encoding_code(),
        MouseAction::Press,
        mods,
    )
}

/// `abs(send_mouse_event)` condition in mouse.c.
pub fn should_send_mouse_event(mode: MouseTrackingMode, action: MouseAction) -> bool {
    mode == MouseTrackingMode::Any
        || (mode == MouseTrackingMode::Motion && action != MouseAction::Move)
        || (mode == MouseTrackingMode::Click
            && matches!(action, MouseAction::Press | MouseAction::Release))
}

/// Dispatch a mouse event using the modes and pixel scale from an input context.
///
/// This is the mode-aware equivalent of `send_mouse_event()` in mouse.c.
pub fn dispatch_mouse_event(
    ctx: &dyn InputContext,
    mpos: &MousePosition,
    button: Option<MouseButton>,
    action: MouseAction,
    mods: Modifiers,
) -> Option<Vec<u8>> {
    let modes = ctx.modes();
    if !should_send_mouse_event(modes.mouse_tracking_mode, action) {
        return None;
    }
    let code = match button {
        Some(button) => button.encoding_code(),
        None => u32::MAX,
    };
    encode_mouse_event_scaled(
        mpos,
        modes.mouse_tracking_protocol,
        code,
        action,
        mods,
        ctx.pixel_scale(),
    )
}

fn encode_utf8(codepoint: u32, out: &mut Vec<u8>) {
    let ch = char::from_u32(codepoint).unwrap_or('\u{fffd}');
    let mut buf = [0u8; 4];
    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
}

/// Geometry needed to turn window coordinates into a cell position.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MouseGeometry {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
    pub cell_width: f64,
    pub cell_height: f64,
    pub columns: u32,
    pub lines: u32,
    pub pixel_scroll_offset_y: f64,
}

/// Exact port of `cell_for_pos()` from mouse.c, including clamping to the
/// window and the half-cell flag. `pixel_scale` is applied to the reported
/// global coordinates (the C code stores them unscaled; pass 1.0 to match it).
pub fn cell_for_pos(
    mouse_x: f64,
    mouse_y: f64,
    geometry: &MouseGeometry,
    clamp_to_window: bool,
    pixel_scale: f64,
) -> Option<MousePosition> {
    if geometry.cell_width <= 0.0 || geometry.cell_height <= 0.0 {
        return None;
    }
    let mut mouse_x = mouse_x;
    let mut mouse_y = mouse_y;
    let global_x = (mouse_x - geometry.left) * pixel_scale;
    let global_y = (mouse_y - geometry.top) * pixel_scale;
    if clamp_to_window {
        mouse_x = mouse_x.max(geometry.left).min(geometry.right);
        mouse_y = mouse_y.max(geometry.top).min(geometry.bottom);
    }
    if mouse_x < geometry.left
        || mouse_y < geometry.top
        || mouse_x > geometry.right
        || mouse_y > geometry.bottom
    {
        return None;
    }
    let mut qx = 0u32;
    let mut qy = 0u32;
    let mut in_left_half = true;
    if mouse_x >= geometry.right {
        qx = geometry.columns.saturating_sub(1);
        in_left_half = false;
    } else if mouse_x >= geometry.left {
        let xval = (mouse_x - geometry.left) / geometry.cell_width;
        let fxval = xval.floor();
        qx = fxval as u32;
        in_left_half = xval - fxval <= 0.5;
    }
    if mouse_y >= geometry.bottom {
        qy = geometry.lines.saturating_sub(1);
    } else if mouse_y >= geometry.top {
        qy = ((mouse_y - geometry.top - geometry.pixel_scroll_offset_y) / geometry.cell_height)
            as u32;
    }
    if qx < geometry.columns && qy < geometry.lines {
        Some(MousePosition {
            cell_x: qx,
            cell_y: qy,
            global_x,
            global_y,
            in_left_half_of_cell: in_left_half,
        })
    } else {
        None
    }
}

/// `MouseShape` from `data-types.h`.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum MouseShape {
    #[default]
    Invalid,
    Default,
    Text,
    Pointer,
    Help,
    Wait,
    Progress,
    Crosshair,
    Cell,
    VerticalText,
    Move,
    EResize,
    NeResize,
    NwResize,
    NResize,
    SeResize,
    SwResize,
    SResize,
    WResize,
    EwResize,
    NsResize,
    NeswResize,
    NwseResize,
    ZoomIn,
    ZoomOut,
    Alias,
    Copy,
    NotAllowed,
    NoDrop,
    Grab,
    Grabbing,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn button_numbering() {
        assert_eq!(button_map(GLFW_MOUSE_BUTTON_LEFT), 1);
        assert_eq!(button_map(GLFW_MOUSE_BUTTON_RIGHT), 3);
        assert_eq!(button_map(GLFW_MOUSE_BUTTON_MIDDLE), 2);
        assert_eq!(button_map(GLFW_MOUSE_BUTTON_4), 8);
        assert_eq!(encode_button_number(1), 0);
        assert_eq!(encode_button_number(4), 64);
        assert_eq!(encode_button_number(8), 128);
        assert_eq!(encode_button_number(12), u32::MAX);
    }

    #[test]
    fn tracking_mode_gate() {
        assert!(should_send_mouse_event(
            MouseTrackingMode::Any,
            MouseAction::Move
        ));
        assert!(!should_send_mouse_event(
            MouseTrackingMode::Motion,
            MouseAction::Move
        ));
        assert!(should_send_mouse_event(
            MouseTrackingMode::Motion,
            MouseAction::Drag
        ));
        assert!(should_send_mouse_event(
            MouseTrackingMode::Click,
            MouseAction::Press
        ));
        assert!(!should_send_mouse_event(
            MouseTrackingMode::Click,
            MouseAction::Move
        ));
        assert!(!should_send_mouse_event(
            MouseTrackingMode::Off,
            MouseAction::Press
        ));
    }
}
