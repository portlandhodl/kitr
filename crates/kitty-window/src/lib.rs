// SPDX-License-Identifier: GPL-3.0-or-later
//! Linux X11/EGL window and event backend for kitr.
//!
//! Kitty normally uses GLFW for its Linux window and input needs. This crate is
//! an **equivalent backend**, not GLFW: it talks to Xlib directly through raw
//! `extern "C"` declarations and links only against the system `X11` and `EGL`
//! libraries. No external Rust crates are used. It covers kitty's Linux
//! window/event requirements:
//!
//! * top-level X11 windows with `WM_DELETE_WINDOW`, UTF-8 titles, size hints and
//!   minimum size, mapping and `XSync`;
//! * `KeyPress`/`KeyRelease` with XKB-aware keysyms, UTF-8 text, neutral
//!   modifiers and detectable-auto-repeat based repeat detection;
//! * mouse motion, buttons, wheel buttons 4–8, enter/leave, focus, resize,
//!   move, expose, close and clipboard selection events;
//! * a `poll()`-based event pump with EINTR handling, configure/expose
//!   coalescing and a pipe-based wakeup handle that another thread can use;
//! * `CLIPBOARD`/`PRIMARY` get/set using `UTF8_STRING` and `TARGETS`, with an
//!   async request state machine and blocking convenience methods;
//! * fullscreen, maximize, hide/show, title, position, size, Xft.dpi scale
//!   factor, cursor shapes and the X bell;
//! * an independent EGL window surface with an RGB8 config, GL 3.3 core
//!   (falling back to GLES 3), clear/swap/readback and RAII teardown.
//!
//! # Intentional gaps
//!
//! Wayland, macOS/Cocoa, IME/preedit, drag-and-drop, touch gestures and
//! multi-monitor/HiDPI per-window scaling are not implemented yet. X11 screen
//! coordinates are used directly; `scale_factor()` is derived from `Xft.dpi`
//! and is informational. Clipboard transfers do not implement `INCR` yet, so
//! very large payloads are not supported.
//!
//! # Example
//!
//! ```no_run
//! use kitty_window::{WindowConfig, WindowEvent, X11Window};
//!
//! let mut window = X11Window::new(&WindowConfig::new("kitty", 800, 600)).unwrap();
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
//! The integration tests spawn `Xvfb` themselves when `$DISPLAY` is unset. See
//! `tests/x11_backend.rs` and `examples/bench.rs`.

pub mod clipboard;
pub mod egl;
pub mod types;
pub mod x11;

pub use clipboard::{
    ClipboardError, ClipboardReply, ClipboardState, ClipboardTarget, ClipboardToken,
};
pub use egl::{EglError, EglWindowSurface, GlApi, GlFns};
pub use types::{
    keysym, KeyAction, KeyInput, ModifierMasks, Modifiers, MouseButton, PointerShape, Selection,
    WindowConfig, WindowError, WindowEvent,
};
pub use x11::{probe_display, x_error_count, WakeupHandle, X11Window};
