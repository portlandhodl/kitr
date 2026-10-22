// SPDX-License-Identifier: GPL-3.0-or-later
//! Display-server selection and a unified window handle.

use std::time::Duration;

use crate::clipboard::{ClipboardError, ClipboardReply, ClipboardToken};
use crate::types::{
    KeyInput, MouseButton, PointerShape, Selection, WindowConfig, WindowError, WindowEvent,
};
use crate::wakeup::WakeupHandle;
use crate::x11::X11Window;

#[cfg(feature = "wayland")]
use crate::wayland::WaylandWindow;

/// A window backed by either X11 or Wayland.
///
/// [`WindowBackend::open`] selects Wayland when `$WAYLAND_DISPLAY` is set and
/// the `wayland` feature is enabled. Otherwise it opens an X11 window. The
/// wrapper exposes the same API as [`X11Window`], plus a few backend-neutral
/// extras.
pub enum WindowBackend {
    X11(Box<X11Window>),
    #[cfg(feature = "wayland")]
    Wayland(Box<WaylandWindow>),
}

impl WindowBackend {
    /// Open a window on the display server selected from the environment.
    pub fn open(config: &WindowConfig) -> Result<Self, WindowError> {
        #[cfg(feature = "wayland")]
        {
            if std::env::var_os("WAYLAND_DISPLAY").is_some_and(|value| !value.is_empty()) {
                return WaylandWindow::new(config).map(|window| Self::Wayland(Box::new(window)));
            }
        }
        X11Window::new(config).map(|window| Self::X11(Box::new(window)))
    }

    pub fn is_wayland(&self) -> bool {
        #[cfg(feature = "wayland")]
        {
            matches!(self, Self::Wayland(_))
        }
        #[cfg(not(feature = "wayland"))]
        {
            false
        }
    }

    pub fn scale_factor(&self) -> f64 {
        match self {
            Self::X11(window) => window.scale_factor(),
            #[cfg(feature = "wayland")]
            Self::Wayland(window) => window.scale_factor(),
        }
    }

    pub fn detectable_autorepeat(&self) -> bool {
        match self {
            Self::X11(window) => window.detectable_autorepeat(),
            #[cfg(feature = "wayland")]
            Self::Wayland(window) => window.detectable_autorepeat(),
        }
    }

    pub fn title(&self) -> &str {
        match self {
            Self::X11(window) => window.title(),
            #[cfg(feature = "wayland")]
            Self::Wayland(window) => window.title(),
        }
    }

    pub fn set_title(&mut self, title: &str) -> Result<(), WindowError> {
        match self {
            Self::X11(window) => window.set_title(title),
            #[cfg(feature = "wayland")]
            Self::Wayland(window) => window.set_title(title),
        }
    }

    pub fn size(&self) -> (u32, u32) {
        match self {
            Self::X11(window) => window.size(),
            #[cfg(feature = "wayland")]
            Self::Wayland(window) => window.size(),
        }
    }

    pub fn position(&self) -> (i32, i32) {
        match self {
            Self::X11(window) => window.position(),
            #[cfg(feature = "wayland")]
            Self::Wayland(window) => window.position(),
        }
    }

    pub fn screen_size(&self) -> (u32, u32) {
        match self {
            Self::X11(window) => window.screen_size(),
            #[cfg(feature = "wayland")]
            Self::Wayland(window) => window.screen_size(),
        }
    }

    pub fn set_size(&mut self, width: u32, height: u32) {
        match self {
            Self::X11(window) => window.set_size(width, height),
            #[cfg(feature = "wayland")]
            Self::Wayland(window) => window.set_size(width, height),
        }
    }

    pub fn set_position(&mut self, x: i32, y: i32) {
        match self {
            Self::X11(window) => window.set_position(x, y),
            #[cfg(feature = "wayland")]
            Self::Wayland(window) => window.set_position(x, y),
        }
    }

    pub fn set_size_limits(
        &mut self,
        min_width: u32,
        min_height: u32,
        max_width: Option<u32>,
        max_height: Option<u32>,
    ) {
        match self {
            Self::X11(window) => {
                window.set_size_limits(min_width, min_height, max_width, max_height)
            }
            #[cfg(feature = "wayland")]
            Self::Wayland(window) => {
                window.set_size_limits(min_width, min_height, max_width, max_height)
            }
        }
    }

    pub fn min_size(&self) -> (u32, u32) {
        match self {
            Self::X11(window) => window.min_size(),
            #[cfg(feature = "wayland")]
            Self::Wayland(window) => window.min_size(),
        }
    }

    pub fn show(&mut self) {
        match self {
            Self::X11(window) => window.show(),
            #[cfg(feature = "wayland")]
            Self::Wayland(window) => window.show(),
        }
    }

    pub fn hide(&mut self) {
        match self {
            Self::X11(window) => window.hide(),
            #[cfg(feature = "wayland")]
            Self::Wayland(window) => window.hide(),
        }
    }

    pub fn is_visible(&self) -> bool {
        match self {
            Self::X11(window) => window.is_visible(),
            #[cfg(feature = "wayland")]
            Self::Wayland(window) => window.is_visible(),
        }
    }

    pub fn focus(&mut self) {
        match self {
            Self::X11(window) => window.focus(),
            #[cfg(feature = "wayland")]
            Self::Wayland(window) => window.focus(),
        }
    }

    pub fn raise(&mut self) {
        match self {
            Self::X11(window) => window.raise(),
            #[cfg(feature = "wayland")]
            Self::Wayland(window) => window.raise(),
        }
    }

    pub fn set_pointer_shape(&mut self, shape: PointerShape) {
        match self {
            Self::X11(window) => window.set_pointer_shape(shape),
            #[cfg(feature = "wayland")]
            Self::Wayland(window) => window.set_pointer_shape(shape),
        }
    }

    pub fn pointer_shape(&self) -> PointerShape {
        match self {
            Self::X11(window) => window.pointer_shape(),
            #[cfg(feature = "wayland")]
            Self::Wayland(window) => window.pointer_shape(),
        }
    }

    pub fn bell(&mut self) {
        match self {
            Self::X11(window) => window.bell(),
            #[cfg(feature = "wayland")]
            Self::Wayland(window) => window.bell(),
        }
    }

    pub fn set_ime_enabled(&mut self, enabled: bool) {
        match self {
            Self::X11(window) => window.set_ime_enabled(enabled),
            #[cfg(feature = "wayland")]
            Self::Wayland(window) => window.set_ime_enabled(enabled),
        }
    }

    pub fn ime_enabled(&self) -> bool {
        match self {
            Self::X11(window) => window.ime_enabled(),
            #[cfg(feature = "wayland")]
            Self::Wayland(window) => window.ime_enabled(),
        }
    }

    pub fn drag_accept(&mut self, target: Option<&str>) -> Result<Option<String>, WindowError> {
        match self {
            Self::X11(window) => window.drag_accept(target),
            #[cfg(feature = "wayland")]
            Self::Wayland(window) => window.drag_accept(target),
        }
    }

    pub fn drag_reject(&mut self) {
        match self {
            Self::X11(window) => window.drag_reject(),
            #[cfg(feature = "wayland")]
            Self::Wayland(window) => window.drag_reject(),
        }
    }

    pub fn is_drag_active(&self) -> bool {
        match self {
            Self::X11(window) => window.is_drag_active(),
            #[cfg(feature = "wayland")]
            Self::Wayland(window) => window.is_drag_active(),
        }
    }

    pub fn drag_target(&self) -> Option<&str> {
        match self {
            Self::X11(window) => window.drag_target(),
            #[cfg(feature = "wayland")]
            Self::Wayland(window) => window.drag_target(),
        }
    }

    pub fn set_fullscreen(&mut self, fullscreen: bool) {
        match self {
            Self::X11(window) => window.set_fullscreen(fullscreen),
            #[cfg(feature = "wayland")]
            Self::Wayland(window) => window.set_fullscreen(fullscreen),
        }
    }

    pub fn is_fullscreen(&self) -> bool {
        match self {
            Self::X11(window) => window.is_fullscreen(),
            #[cfg(feature = "wayland")]
            Self::Wayland(window) => window.is_fullscreen(),
        }
    }

    pub fn set_maximized(&mut self, maximized: bool) {
        match self {
            Self::X11(window) => window.set_maximized(maximized),
            #[cfg(feature = "wayland")]
            Self::Wayland(window) => window.set_maximized(maximized),
        }
    }

    pub fn is_maximized(&self) -> bool {
        match self {
            Self::X11(window) => window.is_maximized(),
            #[cfg(feature = "wayland")]
            Self::Wayland(window) => window.is_maximized(),
        }
    }

    pub fn wakeup_handle(&self) -> WakeupHandle {
        match self {
            Self::X11(window) => window.wakeup_handle(),
            #[cfg(feature = "wayland")]
            Self::Wayland(window) => window.wakeup_handle(),
        }
    }

    pub fn wakeup(&self) {
        match self {
            Self::X11(window) => window.wakeup(),
            #[cfg(feature = "wayland")]
            Self::Wayland(window) => window.wakeup(),
        }
    }

    pub fn should_close(&self) -> bool {
        match self {
            Self::X11(window) => window.should_close(),
            #[cfg(feature = "wayland")]
            Self::Wayland(window) => window.should_close(),
        }
    }

    pub fn request_close(&mut self) {
        match self {
            Self::X11(window) => window.request_close(),
            #[cfg(feature = "wayland")]
            Self::Wayland(window) => window.request_close(),
        }
    }

    pub fn poll_events(&mut self, timeout_ms: i32) -> Vec<WindowEvent> {
        match self {
            Self::X11(window) => window.poll_events(timeout_ms),
            #[cfg(feature = "wayland")]
            Self::Wayland(window) => window.poll_events(timeout_ms),
        }
    }

    pub fn wait_events(&mut self) -> Vec<WindowEvent> {
        match self {
            Self::X11(window) => window.wait_events(),
            #[cfg(feature = "wayland")]
            Self::Wayland(window) => window.wait_events(),
        }
    }

    pub fn inject_event(&mut self, event: &WindowEvent) -> Result<(), WindowError> {
        match self {
            Self::X11(window) => window.inject_event(event),
            #[cfg(feature = "wayland")]
            Self::Wayland(window) => window.inject_event(event),
        }
    }

    pub fn set_clipboard_text(
        &mut self,
        selection: Selection,
        text: &str,
    ) -> Result<(), WindowError> {
        match self {
            Self::X11(window) => window.set_clipboard_text(selection, text),
            #[cfg(feature = "wayland")]
            Self::Wayland(window) => window.set_clipboard_text(selection, text),
        }
    }

    pub fn set_clipboard_targets(
        &mut self,
        selection: Selection,
        targets: Vec<(String, Vec<u8>)>,
    ) -> Result<(), WindowError> {
        match self {
            Self::X11(window) => window.set_clipboard_targets(selection, targets),
            #[cfg(feature = "wayland")]
            Self::Wayland(window) => window.set_clipboard_targets(selection, targets),
        }
    }

    pub fn set_primary_text(&mut self, text: &str) -> Result<(), WindowError> {
        match self {
            Self::X11(window) => window.set_primary_text(text),
            #[cfg(feature = "wayland")]
            Self::Wayland(window) => window.set_primary_text(text),
        }
    }

    pub fn has_clipboard_offer(&self, selection: Selection) -> bool {
        match self {
            Self::X11(window) => window.has_clipboard_offer(selection),
            #[cfg(feature = "wayland")]
            Self::Wayland(window) => window.has_clipboard_offer(selection),
        }
    }

    pub fn request_clipboard_text(
        &mut self,
        selection: Selection,
    ) -> Result<ClipboardToken, WindowError> {
        match self {
            Self::X11(window) => window.request_clipboard_text(selection),
            #[cfg(feature = "wayland")]
            Self::Wayland(window) => window.request_clipboard_text(selection),
        }
    }

    pub fn request_clipboard_targets(
        &mut self,
        selection: Selection,
    ) -> Result<ClipboardToken, WindowError> {
        match self {
            Self::X11(window) => window.request_clipboard_targets(selection),
            #[cfg(feature = "wayland")]
            Self::Wayland(window) => window.request_clipboard_targets(selection),
        }
    }

    pub fn take_clipboard_result(
        &mut self,
        token: ClipboardToken,
    ) -> Option<Result<ClipboardReply, ClipboardError>> {
        match self {
            Self::X11(window) => window.take_clipboard_result(token),
            #[cfg(feature = "wayland")]
            Self::Wayland(window) => window.take_clipboard_result(token),
        }
    }

    pub fn pending_clipboard_requests(&self) -> usize {
        match self {
            Self::X11(window) => window.pending_clipboard_requests(),
            #[cfg(feature = "wayland")]
            Self::Wayland(window) => window.pending_clipboard_requests(),
        }
    }

    pub fn clipboard_text_blocking(
        &mut self,
        selection: Selection,
        timeout: Duration,
    ) -> Result<String, ClipboardError> {
        match self {
            Self::X11(window) => window.clipboard_text_blocking(selection, timeout),
            #[cfg(feature = "wayland")]
            Self::Wayland(window) => window.clipboard_text_blocking(selection, timeout),
        }
    }

    pub fn clipboard_targets_blocking(
        &mut self,
        selection: Selection,
        timeout: Duration,
    ) -> Result<Vec<String>, ClipboardError> {
        match self {
            Self::X11(window) => window.clipboard_targets_blocking(selection, timeout),
            #[cfg(feature = "wayland")]
            Self::Wayland(window) => window.clipboard_targets_blocking(selection, timeout),
        }
    }

    /// Injectable key helper for tests that want backend-neutral input state.
    pub fn inject_key(&mut self, key: &KeyInput) -> Result<(), WindowError> {
        self.inject_event(&WindowEvent::Key(key.clone()))
    }

    /// Injectable mouse button helper for tests.
    pub fn inject_mouse_button(
        &mut self,
        button: MouseButton,
        pressed: bool,
        x: f64,
        y: f64,
    ) -> Result<(), WindowError> {
        self.inject_event(&WindowEvent::MouseButton {
            button,
            pressed,
            x,
            y,
            modifiers: Default::default(),
        })
    }
}
