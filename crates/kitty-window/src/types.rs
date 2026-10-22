// SPDX-License-Identifier: GPL-3.0-or-later
//! Platform-neutral window, input and clipboard types.
//!
//! Nothing in this module mentions X11, EGL or GLFW values. The X11 backend
//! translates its native events into these types so that the rest of kitr (and
//! tests) can consume a stable API.

use std::fmt;

/// Neutral modifier state for an input event.
///
/// The X11 backend fills this from the XKB modifier mapping rather than from
/// hard-coded Mod1/Mod4 assumptions. `raw_state` retains the native X11 state
/// word for callers that need to implement platform-specific remapping.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub shift: bool,
    pub control: bool,
    pub alt: bool,
    pub super_key: bool,
    pub hyper: bool,
    pub meta: bool,
    pub caps_lock: bool,
    pub num_lock: bool,
    pub alt_gr: bool,
    /// The native X11 state word, or zero on other backends.
    pub raw_state: u32,
}

impl Modifiers {
    /// Bits used by the neutral API. These deliberately are *not* the GLFW
    /// constants; callers that need GLFW-compatible bits can translate them.
    pub const SHIFT: u32 = 1 << 0;
    pub const CONTROL: u32 = 1 << 1;
    pub const ALT: u32 = 1 << 2;
    pub const SUPER: u32 = 1 << 3;
    pub const HYPER: u32 = 1 << 4;
    pub const META: u32 = 1 << 5;
    pub const CAPS_LOCK: u32 = 1 << 6;
    pub const NUM_LOCK: u32 = 1 << 7;
    pub const ALT_GR: u32 = 1 << 8;

    /// Neutral bit mask for this state.
    pub fn bits(self) -> u32 {
        let mut bits = 0;
        if self.shift {
            bits |= Self::SHIFT;
        }
        if self.control {
            bits |= Self::CONTROL;
        }
        if self.alt {
            bits |= Self::ALT;
        }
        if self.super_key {
            bits |= Self::SUPER;
        }
        if self.hyper {
            bits |= Self::HYPER;
        }
        if self.meta {
            bits |= Self::META;
        }
        if self.caps_lock {
            bits |= Self::CAPS_LOCK;
        }
        if self.num_lock {
            bits |= Self::NUM_LOCK;
        }
        if self.alt_gr {
            bits |= Self::ALT_GR;
        }
        bits
    }

    /// GLFW-compatible bits, useful while kitty's input layer is still being
    /// ported. GLFW uses Shift=1, Alt=2, Control=4, Super=8, Hyper=16, Meta=32.
    pub fn glfw_bits(self) -> u32 {
        let mut bits = 0;
        if self.shift {
            bits |= 0x0001;
        }
        if self.alt {
            bits |= 0x0002;
        }
        if self.control {
            bits |= 0x0004;
        }
        if self.super_key {
            bits |= 0x0008;
        }
        if self.hyper {
            bits |= 0x0010;
        }
        if self.meta {
            bits |= 0x0020;
        }
        bits
    }
}

/// XKB modifier masks discovered at runtime.
#[derive(Clone, Copy, Debug)]
pub struct ModifierMasks {
    pub alt: u32,
    pub super_key: u32,
    pub num_lock: u32,
    pub alt_gr: u32,
}

impl Default for ModifierMasks {
    fn default() -> Self {
        // Conventional X11 defaults: Mod1=Alt, Mod2=NumLock, Mod4=Super,
        // Mod5=ISO_Level3 (AltGr). The X11 backend replaces these with masks
        // obtained from XGetModifierMapping.
        Self {
            alt: 1 << 3,
            super_key: 1 << 6,
            num_lock: 1 << 4,
            alt_gr: 1 << 7,
        }
    }
}

impl ModifierMasks {
    /// Translate an X11 state word into neutral modifiers.
    pub fn decode(self, state: u32) -> Modifiers {
        const SHIFT_MASK: u32 = 1 << 0;
        const LOCK_MASK: u32 = 1 << 1;
        const CONTROL_MASK: u32 = 1 << 2;
        Modifiers {
            shift: state & SHIFT_MASK != 0,
            control: state & CONTROL_MASK != 0,
            alt: state & self.alt != 0,
            super_key: state & self.super_key != 0,
            hyper: false,
            meta: false,
            caps_lock: state & LOCK_MASK != 0,
            num_lock: state & self.num_lock != 0,
            alt_gr: state & self.alt_gr != 0,
            raw_state: state,
        }
    }
}

/// Keyboard event action. Repeats are synthesised from detectable auto-repeat.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyAction {
    Press,
    Release,
    Repeat,
}

impl KeyAction {
    pub fn is_press(self) -> bool {
        !matches!(self, Self::Release)
    }

    pub fn is_release(self) -> bool {
        matches!(self, Self::Release)
    }
}

/// A platform-neutral key event.
///
/// `keysym` is the effective keysym (Shift applied), `base_keysym` is the
/// unshifted keysym. `text` contains the UTF-8 text produced by the key, when
/// the key produces printable text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyInput {
    pub keysym: u32,
    pub base_keysym: u32,
    pub keycode: u32,
    pub text: Option<String>,
    pub modifiers: Modifiers,
    pub action: KeyAction,
}

impl KeyInput {
    pub fn new(keysym: u32, action: KeyAction) -> Self {
        Self {
            keysym,
            base_keysym: keysym,
            keycode: 0,
            text: None,
            modifiers: Modifiers::default(),
            action,
        }
    }

    pub fn with_text(mut self, text: &str) -> Self {
        self.text = Some(text.to_owned());
        self
    }

    pub fn with_modifiers(mut self, modifiers: Modifiers) -> Self {
        self.modifiers = modifiers;
        self
    }

    pub fn is_press(&self) -> bool {
        self.action.is_press()
    }

    pub fn is_release(&self) -> bool {
        self.action.is_release()
    }
}

/// Mouse buttons reported by the backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    Back,
    Forward,
    /// Any other native button number.
    Other(u8),
}

impl MouseButton {
    /// Map an X11 button number to a neutral button.
    pub fn from_x11_button(button: u8) -> Self {
        match button {
            1 => Self::Left,
            2 => Self::Middle,
            3 => Self::Right,
            8 => Self::Back,
            9 => Self::Forward,
            other => Self::Other(other),
        }
    }

    /// Native X11 button number for this button.
    pub fn to_x11_button(self) -> u8 {
        match self {
            Self::Left => 1,
            Self::Middle => 2,
            Self::Right => 3,
            Self::Back => 8,
            Self::Forward => 9,
            Self::Other(button) => button,
        }
    }
}

/// An X11 clipboard selection (or the corresponding concept elsewhere).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Selection {
    Clipboard,
    Primary,
}

impl Selection {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Clipboard => "CLIPBOARD",
            Self::Primary => "PRIMARY",
        }
    }
}

/// A neutral event produced by the window backend.
#[derive(Clone, Debug, PartialEq)]
pub enum WindowEvent {
    Key(KeyInput),
    MouseMove {
        x: f64,
        y: f64,
        modifiers: Modifiers,
    },
    MouseButton {
        button: MouseButton,
        pressed: bool,
        x: f64,
        y: f64,
        modifiers: Modifiers,
    },
    Scroll {
        x: f64,
        y: f64,
        delta_x: f64,
        delta_y: f64,
        button: u8,
        modifiers: Modifiers,
    },
    Enter {
        x: f64,
        y: f64,
        modifiers: Modifiers,
    },
    Leave {
        x: f64,
        y: f64,
        modifiers: Modifiers,
    },
    Focus {
        focused: bool,
    },
    Resize {
        width: u32,
        height: u32,
    },
    Move {
        x: i32,
        y: i32,
    },
    Expose {
        x: i32,
        y: i32,
        width: u32,
        height: u32,
    },
    /// The window manager or `inject_event` asked the window to close.
    CloseRequested,
    /// The selection owner went away (we lost ownership).
    SelectionClear {
        selection: Selection,
    },
    /// Another client requested data from a selection we own. The backend has
    /// already answered it; this notification is for observability.
    SelectionRequest {
        selection: Selection,
        target: String,
    },
    /// A clipboard conversion we requested completed. The data is also
    /// available from the window's clipboard token API.
    SelectionNotify {
        selection: Selection,
        target: String,
        succeeded: bool,
    },
    /// Updated input-method pre-edit text. `cursor` is a character range into
    /// `text`; the start and end are equal for a simple caret.
    Preedit {
        text: String,
        cursor: Option<(usize, usize)>,
    },
    /// Text committed by an input method.
    Commit {
        text: String,
    },
    /// The input method asked the application to delete text around the
    /// cursor. Lengths are in UTF-8 bytes, before and after the cursor.
    DeleteSurrounding {
        before: u32,
        after: u32,
    },
    /// A drag entered the window. `targets` lists the offered MIME types.
    DragEnter {
        x: f64,
        y: f64,
        targets: Vec<String>,
    },
    /// A drag moved inside the window.
    DragMotion {
        x: f64,
        y: f64,
        targets: Vec<String>,
    },
    /// A drag was dropped on the window.
    DragDrop {
        x: f64,
        y: f64,
        paths: Vec<String>,
        text: Option<String>,
        target: Option<String>,
    },
    /// A drag left the window without being dropped.
    DragLeave,
    /// The event pump was woken through its wakeup pipe.
    Wakeup,
}

/// Common pointer shapes. Mapped to the X11 cursor font by the X11 backend.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PointerShape {
    #[default]
    Default,
    Text,
    Pointer,
    Crosshair,
    Move,
    Wait,
    Progress,
    ResizeN,
    ResizeS,
    ResizeE,
    ResizeW,
    ResizeNE,
    ResizeNW,
    ResizeSE,
    ResizeSW,
    NotAllowed,
    Help,
    Cell,
    VerticalText,
    ZoomIn,
    ZoomOut,
    ContextMenu,
}

/// Configuration for opening a top-level window.
#[derive(Clone, Debug)]
pub struct WindowConfig {
    pub title: String,
    pub width: u32,
    pub height: u32,
    pub min_width: u32,
    pub min_height: u32,
    pub max_width: Option<u32>,
    pub max_height: Option<u32>,
    pub position: Option<(i32, i32)>,
    pub visible: bool,
    pub resizable: bool,
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            title: "kitty".to_owned(),
            width: 800,
            height: 600,
            min_width: 1,
            min_height: 1,
            max_width: None,
            max_height: None,
            position: None,
            visible: true,
            resizable: true,
        }
    }
}

impl WindowConfig {
    pub fn new(title: impl Into<String>, width: u32, height: u32) -> Self {
        Self {
            title: title.into(),
            width: width.max(1),
            height: height.max(1),
            ..Self::default()
        }
    }

    pub fn min_size(mut self, width: u32, height: u32) -> Self {
        self.min_width = width.max(1);
        self.min_height = height.max(1);
        self
    }

    pub fn position(mut self, x: i32, y: i32) -> Self {
        self.position = Some((x, y));
        self
    }

    pub fn visible(mut self, visible: bool) -> Self {
        self.visible = visible;
        self
    }
}

/// Error raised by the X11 backend.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowError {
    message: String,
}

impl WindowError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for WindowError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for WindowError {}

/// X keysyms used by tests and callers that need to inject/inspect keys.
pub mod keysym {
    pub const BACKSPACE: u32 = 0xff08;
    pub const TAB: u32 = 0xff09;
    pub const RETURN: u32 = 0xff0d;
    pub const ESCAPE: u32 = 0xff1b;
    pub const DELETE: u32 = 0xffff;
    pub const HOME: u32 = 0xff50;
    pub const LEFT: u32 = 0xff51;
    pub const UP: u32 = 0xff52;
    pub const RIGHT: u32 = 0xff53;
    pub const DOWN: u32 = 0xff54;
    pub const PAGE_UP: u32 = 0xff55;
    pub const PAGE_DOWN: u32 = 0xff56;
    pub const END: u32 = 0xff57;
    pub const INSERT: u32 = 0xff63;
    pub const MENU: u32 = 0xff67;
    pub const F1: u32 = 0xffbe;
    pub const SHIFT_L: u32 = 0xffe1;
    pub const SHIFT_R: u32 = 0xffe2;
    pub const CONTROL_L: u32 = 0xffe3;
    pub const CONTROL_R: u32 = 0xffe4;
    pub const ALT_L: u32 = 0xffe9;
    pub const ALT_R: u32 = 0xffea;
    pub const SUPER_L: u32 = 0xffeb;
    pub const SUPER_R: u32 = 0xffec;
    pub const HYPER_L: u32 = 0xffed;
    pub const HYPER_R: u32 = 0xffee;
    pub const META_L: u32 = 0xffe7;
    pub const META_R: u32 = 0xffe8;
    pub const ISO_LEVEL3_SHIFT: u32 = 0xfe03;
    pub const SPACE: u32 = 0x0020;
}
