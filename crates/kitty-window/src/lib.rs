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
//! * a `poll()`-based event pump with EINTR handling, configure/expose
//!   coalescing and a pipe-based wakeup handle that another thread can use;
//! * clipboard get/set with an async request state machine and blocking
//!   convenience methods. Wayland uses `wl_data_device` with
//!   `text/plain;charset=utf-8`, `text/plain`, `UTF8_STRING`, `STRING` and
//!   synthesised `TARGETS`; X11 uses `CLIPBOARD`/`PRIMARY` selections and X
//!   atoms;
//! * fullscreen, maximize, hide/show, title, size and pointer-shape state;
//! * HiDPI scale factors: `Xft.dpi` on X11, `wl_output.scale`,
//!   `wl_surface.preferred_buffer_scale` and `fractional-scale-v1` on Wayland;
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
//! macOS/Cocoa, IME/preedit, drag-and-drop, touch gestures, tablet input and
//! cursor themes are not implemented. Wayland currently implements the
//! `CLIPBOARD` selection but not the `PRIMARY` selection; X11 still supports
//! both. Wayland window position is compositor-owned, so `position` is only
//! bookkeeping. X11 clipboard transfers do not implement `INCR` yet, so very
//! large payloads are not supported. Fractional scaling falls back to the
//! integer output scale when `viewporter` is unavailable.
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
#[cfg(feature = "wayland")]
pub use egl::EglWaylandSurface;
pub use egl::{EglError, EglWindowSurface, GlApi, GlFns};
pub use types::{
    keysym, KeyAction, KeyInput, ModifierMasks, Modifiers, MouseButton, PointerShape, Selection,
    WindowConfig, WindowError, WindowEvent,
};
pub use wakeup::WakeupHandle;
pub use x11::{probe_display, x_error_count, X11Window};

#[cfg(feature = "wayland")]
pub use wayland::{probe_wayland, WaylandWindow};
