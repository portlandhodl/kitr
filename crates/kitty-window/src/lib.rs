// SPDX-License-Identifier: GPL-3.0-or-later
//! Linux X11/Wayland/EGL window and event backend for kitr.
//!
//! Kitty normally uses GLFW for its Linux window and input needs. This crate is
//! an **equivalent backend**, not GLFW: the default build talks to Xlib directly
//! through raw `extern "C"` declarations and links only against the system
//! `X11` and `EGL` libraries. With the `wayland` feature it also builds a
//! Wayland client using `wayland-client` and `wayland-protocols`.
//!
//! Both backends expose the same neutral [`WindowEvent`], [`KeyInput`],
//! [`Modifiers`], [`MouseButton`] and clipboard types:
//!
//! * top-level windows with UTF-8 titles, size hints, minimum/maximum size,
//!   mapping and close requests;
//! * keyboard events with XKB-aware keysyms, UTF-8 text, neutral modifiers and
//!   repeat handling (`repeat_info` on Wayland, detectable auto-repeat on X11);
//! * mouse motion, buttons, wheel axes, enter/leave, focus, resize, move,
//!   expose, close and clipboard selection events;
//! * X11 input methods (XIM) with pre-edit/commit callbacks and dead-key
//!   composition, plus `zwp_text_input_v3` on Wayland, exposed as
//!   [`WindowEvent::Preedit`], [`WindowEvent::Commit`] and
//!   [`WindowEvent::DeleteSurrounding`];
//! * XDND drag and drop on X11 (URI lists and UTF-8 text, with `INCR` for large
//!   payloads) and `wl_data_device` drag targets on Wayland, exposed as
//!   [`WindowEvent::DragEnter`], [`WindowEvent::DragMotion`],
//!   [`WindowEvent::DragDrop`] and [`WindowEvent::DragLeave`];
//! * local drag sources on both backends: XDND enter/position/drop/leave on
//!   X11 and `wl_data_source.start_drag` on Wayland, offering
//!   `text/uri-list` for files and UTF-8 text for selections, with
//!   [`WindowEvent::DragStarted`], [`WindowEvent::DragFinished`] and
//!   [`WindowEvent::DragCancelled`] notifications;
//! * XInput2 touch begin/update/end on X11 and `wl_touch` on Wayland, exposed
//!   as [`WindowEvent::Touch`] with id, position, pressure and phase;
//! * `zwp_tablet_v2` tool events on Wayland, exposed as
//!   [`WindowEvent::TabletTool`] with pressure, tilt, rotation and axes;
//! * `wlr-layer-shell` panel/QAT surfaces on Wayland via
//!   [`WindowConfig::layer_shell`];
//! * a `poll()`-based event pump with EINTR handling, configure/expose
//!   coalescing and a pipe-based wakeup handle that another thread can use;
//! * clipboard get/set with an async request state machine and blocking
//!   convenience methods. Wayland uses `wl_data_device` for `CLIPBOARD` and
//!   `zwp_primary_selection_device_manager_v1` for `PRIMARY`, offering
//!   `text/plain;charset=utf-8`, `text/plain`, `UTF8_STRING`, `STRING` and
//!   synthesised `TARGETS`; X11 uses `CLIPBOARD`/`PRIMARY` selections and X
//!   atoms, with chunked `INCR` transfers for payloads above 256 KiB;
//! * fullscreen, maximize, hide/show, title, size and pointer-shape state;
//! * HiDPI scale factors: `Xft.dpi` on X11, `wl_output.scale`,
//!   `wl_surface.preferred_buffer_scale` and `fractional-scale-v1` on Wayland;
//! * cursor theme snapshots with hot reload and a per-window cursor scale.
//!   X11 loads size-specific images through `libXcursor`; Wayland loads the
//!   same themes through `libwayland-cursor` and paints a cursor surface with
//!   `wl_pointer.set_cursor`, falling back to compositor selection if the
//!   helper library is unavailable;
//! * independent EGL window surfaces with an RGB8 config, GL 3.3 core (falling
//!   back to GLES 3), clear/swap/readback and RAII teardown. The X11 path uses
//!   `EGL_PLATFORM_X11_KHR`; the Wayland path uses
//!   `EGL_PLATFORM_WAYLAND_KHR` through [`EglWaylandSurface`].
//!
//! [`WindowBackend::open`] picks Wayland when `$WAYLAND_DISPLAY` is set and the
//! `wayland` feature is enabled, otherwise X11. [`X11Window`] remains available
//! directly for callers that need an explicit backend.
//!
//! # Intentional gaps
//!
//! macOS/Cocoa is not implemented. Wayland `zwp_tablet_v2` tool proximity,
//! pressure (shaped by [`PressureCurve`]), tilt, rotation, slider and wheel are
//! forwarded as [`WindowEvent::TabletTool`]; X11 touch maps the
//! `Abs MT Pressure`/`Abs Pressure` valuator through its advertised range and
//! the same curve. `wl_touch` still uses unit pressure because the core
//! Wayland protocol carries no pressure axis. `wlr-layer-shell` panel/QAT
//! surfaces are created when [`WindowConfig::layer_shell`] is set.
//! X11 cursor themes load size-specific images through `libXcursor` when
//! present and fall back to the cursor font otherwise; Wayland uses
//! `libwayland-cursor` when present. Wayland window position is
//! compositor-owned, so `position` is only bookkeeping. Fractional scaling
//! falls back to the integer output scale when `viewporter` is unavailable. The
//! X11 input-method tests exercise callback state directly because `Xvfb`
//! usually has no XIM server.
//!
//! # Example
//!
//! ```no_run
//! use kitty_window::{WindowBackend, WindowConfig, WindowEvent};
//!
//! let mut window = WindowBackend::open(&WindowConfig::new("kitty", 800, 600)).unwrap();
//! loop {
//!     for event in window.poll_events(16) {
//!         if let WindowEvent::CloseRequested = event {
//!             return;
//!         }
//!     }
//!     if window.should_close() {
//!         return;
//!     }
//! }
//! ```
//!
//! The X11 integration tests spawn `Xvfb` themselves when `$DISPLAY` is unset.
//! The Wayland integration test connects to `$WAYLAND_DISPLAY` when one is
//! available and skips cleanly otherwise; end-to-end Wayland testing requires a
//! compositor such as weston. Unit tests for configure coalescing, key repeat
//! classification, pointer mapping and clipboard target preference do not need
//! a compositor.

mod backend;
pub mod clipboard;
pub mod cursor;
pub mod dnd;
pub mod egl;
pub mod types;
mod wakeup;
pub mod x11;

#[cfg(feature = "wayland")]
pub mod wayland;

pub use backend::WindowBackend;
pub use clipboard::{
    ClipboardError, ClipboardReply, ClipboardState, ClipboardTarget, ClipboardToken,
};
pub use cursor::{CursorTheme, CursorThemeEvent};
pub use dnd::{DragSourceData, DragSourceState, DropTarget};
#[cfg(feature = "wayland")]
pub use egl::EglWaylandSurface;
pub use egl::{EglError, EglWindowSurface, GlApi, GlFns};
pub use types::{
    keysym, KeyAction, KeyInput, LayerShellConfig, LayerShellEdge, LayerShellFocus,
    LayerShellLayer, ModifierMasks, Modifiers, MouseButton, PointerShape, PressureCurve, Selection,
    TabletPhase, TouchPhase, WindowConfig, WindowError, WindowEvent,
};
pub use wakeup::WakeupHandle;
pub use x11::{probe_display, x_error_count, X11Window};

#[cfg(feature = "wayland")]
pub use wayland::{probe_wayland, WaylandWindow};

/// An EGL surface for either backend, selected to match [`WindowBackend`].
pub enum EglSurface {
    X11(egl::EglWindowSurface),
    #[cfg(feature = "wayland")]
    Wayland(egl::EglWaylandSurface),
}

impl EglSurface {
    pub fn new(window: &WindowBackend) -> Result<Self, egl::EglError> {
        match window {
            WindowBackend::X11(window) => egl::EglWindowSurface::new(window).map(Self::X11),
            #[cfg(feature = "wayland")]
            WindowBackend::Wayland(window) => {
                egl::EglWaylandSurface::new(window).map(Self::Wayland)
            }
        }
    }

    pub fn make_current(&self) -> Result<(), egl::EglError> {
        match self {
            Self::X11(surface) => surface.make_current(),
            #[cfg(feature = "wayland")]
            Self::Wayland(surface) => surface.make_current(),
        }
    }

    pub fn size(&self) -> (u32, u32) {
        match self {
            Self::X11(surface) => surface.size(),
            #[cfg(feature = "wayland")]
            Self::Wayland(surface) => surface.size(),
        }
    }

    pub fn refresh_size(&mut self) -> (u32, u32) {
        match self {
            Self::X11(surface) => surface.refresh_size(),
            #[cfg(feature = "wayland")]
            Self::Wayland(surface) => surface.refresh_size(),
        }
    }

    pub fn swap_buffers(&self) -> Result<(), egl::EglError> {
        match self {
            Self::X11(surface) => surface.swap_buffers(),
            #[cfg(feature = "wayland")]
            Self::Wayland(surface) => surface.swap_buffers(),
        }
    }

    pub fn read_pixels(&mut self) -> Result<Vec<u8>, egl::EglError> {
        match self {
            Self::X11(surface) => surface.read_pixels(),
            #[cfg(feature = "wayland")]
            Self::Wayland(surface) => surface.read_pixels(),
        }
    }
}
