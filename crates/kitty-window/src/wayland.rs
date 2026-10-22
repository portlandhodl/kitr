// SPDX-License-Identifier: GPL-3.0-or-later
//! Raw Wayland client backend.
//!
//! This module implements the subset of the Wayland protocol that kitty needs
//! for a top-level window: `wl_compositor`, `xdg_wm_base`, `wl_seat` with
//! keyboard and pointer, `wl_shm`, `wl_output`, `wl_data_device`,
//! `fractional-scale-v1` and `viewporter`. It deliberately mirrors the neutral
//! API of [`X11Window`](crate::X11Window) so that callers can use
//! [`WindowBackend`](crate::WindowBackend) and ignore the display server.
//!
//! Presentation is provided in two ways: [`WaylandWindow::present_clear`] uses
//! shared memory for a basic solid-colour frame, while
//! [`EglWaylandSurface`](crate::EglWaylandSurface) creates a real EGL surface
//! through `EGL_PLATFORM_WAYLAND_KHR`. The GUI can try EGL first and fall back
//! to the shm path when EGL is unavailable.
//!
//! # Intentional gaps
//!
//! IME/preedit, drag-and-drop, touch, tablet input, cursor themes and the
//! primary selection are not implemented. Window position is controlled by the
//! compositor, so `position`/`set_position` report or accept a best-effort
//! value and cannot be used for absolute placement. Fractional scaling is
//! honoured when both `fractional-scale-v1` and `viewporter` are advertised;
//! otherwise the integer output scale is rounded.

use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::{c_char, c_int, c_uint, c_void, CStr, CString};
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::ptr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use wayland_client::protocol::{
    wl_buffer, wl_callback, wl_compositor, wl_data_device, wl_data_device_manager, wl_data_offer,
    wl_data_source, wl_keyboard, wl_output, wl_pointer, wl_registry, wl_seat, wl_shm, wl_shm_pool,
    wl_surface,
};
use wayland_client::{
    delegate_noop, event_created_child, Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum,
};
use wayland_protocols::wp::fractional_scale::v1::client::{
    wp_fractional_scale_manager_v1, wp_fractional_scale_v1,
};
use wayland_protocols::wp::viewporter::client::{wp_viewport, wp_viewporter};
use wayland_protocols::xdg::shell::client::{xdg_surface, xdg_toplevel, xdg_wm_base};

use crate::clipboard::{
    ClipboardError, ClipboardReply, ClipboardState, ClipboardTarget, ClipboardToken,
};
use crate::types::{
    keysym, KeyAction, KeyInput, Modifiers, MouseButton, PointerShape, Selection, WindowConfig,
    WindowError, WindowEvent,
};
use crate::wakeup::{WakeupHandle, WakeupPipe};

fn werr(message: impl Into<String>) -> WindowError {
    WindowError::new(message)
}

/// Check whether a Wayland compositor can be reached without creating a window.
pub fn probe_wayland() -> bool {
    Connection::connect_to_env().is_ok()
}

// {{{ xkbcommon FFI

#[repr(C)]
struct XkbContext {
    _private: [u8; 0],
}

#[repr(C)]
struct XkbKeymap {
    _private: [u8; 0],
}

#[repr(C)]
struct XkbState {
    _private: [u8; 0],
}

const XKB_KEYMAP_FORMAT_TEXT_V1: u32 = 1;
const XKB_STATE_MODS_EFFECTIVE: u32 = 1 << 3;

#[link(name = "xkbcommon")]
unsafe extern "C" {
    fn xkb_context_new(flags: u32) -> *mut XkbContext;
    fn xkb_context_unref(context: *mut XkbContext);
    fn xkb_keymap_new_from_string(
        context: *mut XkbContext,
        string: *const c_char,
        format: u32,
        flags: u32,
    ) -> *mut XkbKeymap;
    fn xkb_keymap_unref(keymap: *mut XkbKeymap);
    fn xkb_state_new(keymap: *mut XkbKeymap) -> *mut XkbState;
    fn xkb_state_unref(state: *mut XkbState);
    fn xkb_state_update_mask(
        state: *mut XkbState,
        depressed_mods: u32,
        latched_mods: u32,
        locked_mods: u32,
        depressed_layout: u32,
        latched_layout: u32,
        locked_layout: u32,
    ) -> u32;
    fn xkb_state_key_get_one_sym(state: *mut XkbState, keycode: u32) -> u32;
    fn xkb_state_key_get_utf8(
        state: *mut XkbState,
        keycode: u32,
        buffer: *mut c_char,
        size: usize,
    ) -> c_int;
    fn xkb_state_mod_name_is_active(state: *mut XkbState, name: *const c_char, kind: u32) -> c_int;
    fn xkb_keymap_key_get_syms_by_level(
        keymap: *mut XkbKeymap,
        keycode: u32,
        layout: u32,
        level: u32,
        syms_out: *mut *const u32,
    ) -> c_int;
}

struct Xkb {
    context: *mut XkbContext,
    keymap: *mut XkbKeymap,
    state: *mut XkbState,
}

impl Xkb {
    fn new() -> Self {
        Self {
            context: unsafe { xkb_context_new(0) },
            keymap: ptr::null_mut(),
            state: ptr::null_mut(),
        }
    }

    fn update_keymap(
        &mut self,
        fd: OwnedFd,
        size: u32,
        format: WEnum<wl_keyboard::KeymapFormat>,
    ) -> Result<(), WindowError> {
        if !matches!(format, WEnum::Value(wl_keyboard::KeymapFormat::XkbV1)) {
            return Err(werr("compositor sent an unsupported keymap format"));
        }
        if self.context.is_null() {
            return Err(werr("failed to create an xkbcommon context"));
        }
        let mut file = File::from(fd);
        let mut bytes = Vec::with_capacity(size as usize);
        file.read_to_end(&mut bytes)
            .map_err(|error| werr(format!("failed to read the Wayland keymap: {error}")))?;
        bytes.truncate(size as usize);
        if let Some(nul) = bytes.iter().position(|byte| *byte == 0) {
            bytes.truncate(nul);
        }
        let string = CString::new(bytes)
            .map_err(|_| werr("the Wayland keymap contains an embedded NUL byte"))?;
        let keymap = unsafe {
            xkb_keymap_new_from_string(self.context, string.as_ptr(), XKB_KEYMAP_FORMAT_TEXT_V1, 0)
        };
        if keymap.is_null() {
            return Err(werr("xkbcommon rejected the compositor keymap"));
        }
        let state = unsafe { xkb_state_new(keymap) };
        if state.is_null() {
            unsafe {
                xkb_keymap_unref(keymap);
            }
            return Err(werr("failed to create an xkbcommon state"));
        }
        unsafe {
            if !self.state.is_null() {
                xkb_state_unref(self.state);
            }
            if !self.keymap.is_null() {
                xkb_keymap_unref(self.keymap);
            }
        }
        self.keymap = keymap;
        self.state = state;
        Ok(())
    }

    fn update_modifiers(&self, depressed: u32, latched: u32, locked: u32, group: u32) {
        if self.state.is_null() {
            return;
        }
        unsafe {
            xkb_state_update_mask(self.state, depressed, latched, locked, 0, 0, group);
        }
    }

    fn modifiers(&self, raw_state: u32) -> Modifiers {
        if self.state.is_null() {
            return Modifiers {
                raw_state,
                ..Modifiers::default()
            };
        }
        let active = |name: &CStr| unsafe {
            xkb_state_mod_name_is_active(self.state, name.as_ptr(), XKB_STATE_MODS_EFFECTIVE) > 0
        };
        Modifiers {
            shift: active(c"Shift"),
            control: active(c"Control"),
            alt: active(c"Alt"),
            super_key: active(c"Super") || active(c"Mod4"),
            hyper: active(c"Hyper"),
            meta: active(c"Meta"),
            caps_lock: active(c"Lock"),
            num_lock: active(c"Mod2"),
            alt_gr: active(c"ISO_Level3_Shift"),
            raw_state,
        }
    }

    fn key(&self, key: u32, action: KeyAction) -> (u32, u32, Option<String>) {
        let keycode = key + 8;
        if self.state.is_null() {
            let (symbol, text) = fallback_key(key, action);
            return (symbol, symbol, text);
        }
        let symbol = unsafe { xkb_state_key_get_one_sym(self.state, keycode) };
        let mut base = symbol;
        if !self.keymap.is_null() {
            let mut syms: *const u32 = ptr::null();
            let count =
                unsafe { xkb_keymap_key_get_syms_by_level(self.keymap, keycode, 0, 0, &mut syms) };
            if count > 0 && !syms.is_null() {
                base = unsafe { *syms };
            }
        }
        let text = if action.is_release() {
            None
        } else {
            let mut buffer = [0 as c_char; 64];
            let count = unsafe {
                xkb_state_key_get_utf8(self.state, keycode, buffer.as_mut_ptr(), buffer.len())
            };
            if count > 0 {
                let bytes: Vec<u8> = buffer[..count as usize]
                    .iter()
                    .map(|byte| *byte as u8)
                    .collect();
                let text = String::from_utf8_lossy(&bytes).into_owned();
                text.chars()
                    .any(|character| !character.is_control())
                    .then_some(text)
            } else {
                None
            }
        };
        (symbol, base, text)
    }
}

impl Drop for Xkb {
    fn drop(&mut self) {
        unsafe {
            if !self.state.is_null() {
                xkb_state_unref(self.state);
            }
            if !self.keymap.is_null() {
                xkb_keymap_unref(self.keymap);
            }
            if !self.context.is_null() {
                xkb_context_unref(self.context);
            }
        }
    }
}

fn fallback_key(key: u32, action: KeyAction) -> (u32, Option<String>) {
    let (symbol, text) = match key {
        1 => (keysym::ESCAPE, None),
        2..=10 => (
            0x31 + (key - 2),
            Some(((b'1' + (key as u8 - 2)) as char).to_string()),
        ),
        11 => (0x30, Some("0".to_owned())),
        12 => (0x2d, Some("-".to_owned())),
        13 => (0x3d, Some("=".to_owned())),
        14 => (keysym::BACKSPACE, None),
        15 => (keysym::TAB, None),
        16 => (0x71, Some("q".to_owned())),
        17 => (0x77, Some("w".to_owned())),
        18 => (0x65, Some("e".to_owned())),
        19 => (0x72, Some("r".to_owned())),
        20 => (0x74, Some("t".to_owned())),
        21 => (0x79, Some("y".to_owned())),
        22 => (0x75, Some("u".to_owned())),
        23 => (0x69, Some("i".to_owned())),
        24 => (0x6f, Some("o".to_owned())),
        25 => (0x70, Some("p".to_owned())),
        28 => (keysym::RETURN, None),
        29 => (keysym::CONTROL_L, None),
        30 => (0x61, Some("a".to_owned())),
        31 => (0x73, Some("s".to_owned())),
        32 => (0x64, Some("d".to_owned())),
        33 => (0x66, Some("f".to_owned())),
        34 => (0x67, Some("g".to_owned())),
        35 => (0x68, Some("h".to_owned())),
        36 => (0x6a, Some("j".to_owned())),
        37 => (0x6b, Some("k".to_owned())),
        38 => (0x6c, Some("l".to_owned())),
        42 => (keysym::SHIFT_L, None),
        44 => (0x7a, Some("z".to_owned())),
        45 => (0x78, Some("x".to_owned())),
        46 => (0x63, Some("c".to_owned())),
        47 => (0x76, Some("v".to_owned())),
        48 => (0x62, Some("b".to_owned())),
        49 => (0x6e, Some("n".to_owned())),
        50 => (0x6d, Some("m".to_owned())),
        54 => (keysym::SHIFT_R, None),
        56 => (keysym::ALT_L, None),
        57 => (keysym::SPACE, Some(" ".to_owned())),
        58 => (0xffe5, None),
        96 => (keysym::RETURN, None),
        97 => (keysym::CONTROL_R, None),
        100 => (keysym::ALT_R, None),
        103 => (keysym::UP, None),
        105 => (keysym::LEFT, None),
        106 => (keysym::RIGHT, None),
        108 => (keysym::DOWN, None),
        110 => (keysym::HOME, None),
        111 => (keysym::DELETE, None),
        116 => (0xff13, None),
        119 => (keysym::PAGE_UP, None),
        121 => (keysym::PAGE_DOWN, None),
        125 => (keysym::SUPER_L, None),
        126 => (keysym::SUPER_R, None),
        _ => (0, None),
    };
    let text = if action.is_release() { None } else { text };
    (symbol, text)
}

// }}}
// {{{ Shared-memory buffer

#[repr(C)]
struct PollFd {
    fd: c_int,
    events: i16,
    revents: i16,
}

const POLLIN: i16 = 0x0001;
const PROT_READ: c_int = 1;
const PROT_WRITE: c_int = 2;
const MAP_SHARED: c_int = 1;
const MFD_CLOEXEC: c_uint = 1;

unsafe extern "C" {
    fn poll(fds: *mut PollFd, nfds: u64, timeout: c_int) -> c_int;
    fn memfd_create(name: *const c_char, flags: c_uint) -> c_int;
    fn ftruncate(fd: c_int, length: i64) -> c_int;
    fn mmap(
        address: *mut c_void,
        length: usize,
        prot: c_int,
        flags: c_int,
        fd: c_int,
        offset: i64,
    ) -> *mut c_void;
    fn munmap(address: *mut c_void, length: usize) -> c_int;
}

struct ShmBuffer {
    _file: File,
    map: *mut u8,
    len: usize,
    width: u32,
    height: u32,
    stride: i32,
    buffer: wl_buffer::WlBuffer,
}

impl ShmBuffer {
    fn new(
        shm: &wl_shm::WlShm,
        qh: &QueueHandle<WaylandWindow>,
        width: u32,
        height: u32,
    ) -> Result<Self, WindowError> {
        let width = width.max(1);
        let height = height.max(1);
        let stride = width
            .checked_mul(4)
            .ok_or_else(|| werr("Wayland buffer width overflow"))? as i32;
        let len = (stride as usize)
            .checked_mul(height as usize)
            .ok_or_else(|| werr("Wayland buffer size overflow"))?;
        let name = c"kitty-window-shm";
        let fd = unsafe { memfd_create(name.as_ptr(), MFD_CLOEXEC) };
        if fd < 0 {
            return Err(werr("memfd_create failed for the Wayland shm buffer"));
        }
        let file = unsafe { File::from_raw_fd(fd) };
        if unsafe { ftruncate(fd, len as i64) } != 0 {
            return Err(werr("ftruncate failed for the Wayland shm buffer"));
        }
        let map = unsafe {
            mmap(
                ptr::null_mut(),
                len,
                PROT_READ | PROT_WRITE,
                MAP_SHARED,
                fd,
                0,
            )
        };
        if map == usize::MAX as *mut c_void {
            return Err(werr("mmap failed for the Wayland shm buffer"));
        }
        let pool = shm.create_pool(file.as_fd(), len as i32, qh, ());
        let buffer = pool.create_buffer(
            0,
            width as i32,
            height as i32,
            stride,
            wl_shm::Format::Xrgb8888,
            qh,
            (),
        );
        pool.destroy();
        Ok(Self {
            _file: file,
            map: map.cast(),
            len,
            width,
            height,
            stride,
            buffer,
        })
    }

    fn fill(&mut self, color: [f32; 4]) {
        let red = (color[0].clamp(0.0, 1.0) * 255.0).round() as u8;
        let green = (color[1].clamp(0.0, 1.0) * 255.0).round() as u8;
        let blue = (color[2].clamp(0.0, 1.0) * 255.0).round() as u8;
        let alpha = (color[3].clamp(0.0, 1.0) * 255.0).round() as u8;
        let pixel = [blue, green, red, alpha];
        let bytes = unsafe { std::slice::from_raw_parts_mut(self.map, self.len) };
        let row = self.stride as usize;
        for line in bytes.chunks_exact_mut(row) {
            let (pixels, _) = line.as_chunks_mut::<4>();
            for component in pixels {
                *component = pixel;
            }
        }
    }
}

impl Drop for ShmBuffer {
    fn drop(&mut self) {
        if !self.map.is_null() {
            unsafe {
                munmap(self.map.cast(), self.len);
            }
            self.map = ptr::null_mut();
        }
    }
}

// }}}
// {{{ Clipboard pending reads

struct ClipboardRead {
    token: ClipboardToken,
    selection: Selection,
    target: String,
    file: UnixStream,
    data: Vec<u8>,
}

// }}}
// {{{ Protocol state helpers (unit tested without a compositor)

#[derive(Clone, Debug, Default)]
struct ConfigureState {
    pending: Option<(u32, u32)>,
    configured: bool,
}

impl ConfigureState {
    fn set_pending(&mut self, width: u32, height: u32) {
        self.pending = Some((width, height));
    }

    fn acknowledge(&mut self, current: (u32, u32)) -> Option<(u32, u32)> {
        self.configured = true;
        let pending = self.pending.take();
        match pending {
            Some((width, height)) => {
                let next = (
                    if width == 0 { current.0 } else { width },
                    if height == 0 { current.1 } else { height },
                );
                (next != current).then_some(next)
            }
            None => None,
        }
    }
}

fn classify_key_action(pressed: &mut HashSet<u32>, key: u32, down: bool) -> Option<KeyAction> {
    if down {
        Some(if pressed.insert(key) {
            KeyAction::Press
        } else {
            KeyAction::Repeat
        })
    } else if pressed.remove(&key) {
        Some(KeyAction::Release)
    } else {
        None
    }
}

fn mouse_button_from_evdev(button: u32) -> MouseButton {
    match button {
        0x110 => MouseButton::Left,
        0x111 => MouseButton::Right,
        0x112 => MouseButton::Middle,
        0x113 => MouseButton::Back,
        0x114 => MouseButton::Forward,
        other => MouseButton::Other(other.min(u8::MAX as u32) as u8),
    }
}

fn axis_to_scroll(axis: WEnum<wl_pointer::Axis>, value: f64) -> (f64, f64, u8) {
    let value = value / 10.0;
    match axis {
        WEnum::Value(wl_pointer::Axis::VerticalScroll) => {
            (0.0, -value, if value < 0.0 { 4 } else { 5 })
        }
        WEnum::Value(wl_pointer::Axis::HorizontalScroll) => {
            (value, 0.0, if value < 0.0 { 6 } else { 7 })
        }
        _ => (0.0, 0.0, 0),
    }
}

fn preferred_clipboard_mime(mimes: &[String]) -> Option<&str> {
    const PREFERENCE: [&str; 4] = [
        "text/plain;charset=utf-8",
        "text/plain",
        "UTF8_STRING",
        "STRING",
    ];
    PREFERENCE
        .iter()
        .find(|preferred| mimes.iter().any(|mime| mime == *preferred))
        .copied()
}

fn coalesce_events(events: &mut Vec<WindowEvent>) {
    let mut coalesced: Vec<WindowEvent> = Vec::with_capacity(events.len());
    for event in events.drain(..) {
        match event {
            WindowEvent::Resize { width, height } => match coalesced.last_mut() {
                Some(WindowEvent::Resize {
                    width: previous_width,
                    height: previous_height,
                }) => {
                    *previous_width = width;
                    *previous_height = height;
                }
                _ => coalesced.push(WindowEvent::Resize { width, height }),
            },
            WindowEvent::MouseMove { x, y, modifiers } => match coalesced.last_mut() {
                Some(WindowEvent::MouseMove {
                    x: previous_x,
                    y: previous_y,
                    modifiers: previous_modifiers,
                }) => {
                    *previous_x = x;
                    *previous_y = y;
                    *previous_modifiers = modifiers;
                }
                _ => coalesced.push(WindowEvent::MouseMove { x, y, modifiers }),
            },
            other => coalesced.push(other),
        }
    }
    *events = coalesced;
}

// }}}
// {{{ WaylandWindow

/// A Wayland top-level window and event loop.
pub struct WaylandWindow {
    connection: Connection,
    queue: Option<EventQueue<WaylandWindow>>,
    qh: QueueHandle<WaylandWindow>,
    _registry: wl_registry::WlRegistry,
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    wm_base: Option<xdg_wm_base::XdgWmBase>,
    seat: Option<wl_seat::WlSeat>,
    outputs: Vec<wl_output::WlOutput>,
    output_scales: HashMap<u32, i32>,
    integer_scale: i32,
    fractional_manager: Option<wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1>,
    fractional_scale: Option<wp_fractional_scale_v1::WpFractionalScaleV1>,
    fractional_scale_factor: Option<f64>,
    viewporter: Option<wp_viewporter::WpViewporter>,
    viewport: Option<wp_viewport::WpViewport>,
    surface: Option<wl_surface::WlSurface>,
    xdg_surface: Option<xdg_surface::XdgSurface>,
    toplevel: Option<xdg_toplevel::XdgToplevel>,
    configure: ConfigureState,
    shm_buffer: Option<ShmBuffer>,
    retired_buffers: Vec<ShmBuffer>,
    frame_callback: Option<wl_callback::WlCallback>,
    frame_pending: bool,
    frame_count: u64,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<wl_pointer::WlPointer>,
    keyboard_focused: bool,
    pointer_x: f64,
    pointer_y: f64,
    pressed_keys: HashSet<u32>,
    xkb: Xkb,
    depressed: u32,
    latched: u32,
    locked: u32,
    group: u32,
    repeat_rate: i32,
    repeat_delay: i32,
    repeat_key: Option<KeyInput>,
    repeat_due: Option<Instant>,
    serial: u32,
    data_device_manager: Option<wl_data_device_manager::WlDataDeviceManager>,
    data_device: Option<wl_data_device::WlDataDevice>,
    data_offer: Option<wl_data_offer::WlDataOffer>,
    pending_offer: Option<wl_data_offer::WlDataOffer>,
    data_offer_mimes: Vec<String>,
    data_source: Option<wl_data_source::WlDataSource>,
    source_selection: Option<Selection>,
    clipboard: ClipboardState,
    clipboard_seq: u64,
    pending_reads: Vec<ClipboardRead>,
    inbox: VecDeque<WindowEvent>,
    wakeup: Arc<WakeupPipe>,
    width: u32,
    height: u32,
    x: i32,
    y: i32,
    title: String,
    min_width: u32,
    min_height: u32,
    max_width: Option<u32>,
    max_height: Option<u32>,
    resizable: bool,
    visible_requested: bool,
    should_close: bool,
    fullscreen: bool,
    maximized: bool,
    pointer_shape: PointerShape,
}

impl WaylandWindow {
    /// Connect to `$WAYLAND_DISPLAY` and create a top-level window.
    pub fn new(config: &WindowConfig) -> Result<Self, WindowError> {
        let connection = Connection::connect_to_env().map_err(|error| {
            werr(format!(
                "failed to connect to Wayland (WAYLAND_DISPLAY): {error}"
            ))
        })?;
        let mut queue = connection.new_event_queue::<WaylandWindow>();
        let qh = queue.handle();
        let registry = connection.display().get_registry(&qh, ());
        let wakeup = Arc::new(WakeupPipe::new()?);

        let mut window = Self {
            connection,
            queue: None,
            qh,
            _registry: registry,
            compositor: None,
            shm: None,
            wm_base: None,
            seat: None,
            outputs: Vec::new(),
            output_scales: HashMap::new(),
            integer_scale: 1,
            fractional_manager: None,
            fractional_scale: None,
            fractional_scale_factor: None,
            viewporter: None,
            viewport: None,
            surface: None,
            xdg_surface: None,
            toplevel: None,
            configure: ConfigureState::default(),
            shm_buffer: None,
            retired_buffers: Vec::new(),
            frame_callback: None,
            frame_pending: false,
            frame_count: 0,
            keyboard: None,
            pointer: None,
            keyboard_focused: false,
            pointer_x: 0.0,
            pointer_y: 0.0,
            pressed_keys: HashSet::new(),
            xkb: Xkb::new(),
            depressed: 0,
            latched: 0,
            locked: 0,
            group: 0,
            repeat_rate: 0,
            repeat_delay: 0,
            repeat_key: None,
            repeat_due: None,
            serial: 0,
            data_device_manager: None,
            data_device: None,
            data_offer: None,
            pending_offer: None,
            data_offer_mimes: Vec::new(),
            data_source: None,
            source_selection: None,
            clipboard: ClipboardState::default(),
            clipboard_seq: 0,
            pending_reads: Vec::new(),
            inbox: VecDeque::new(),
            wakeup,
            width: config.width.max(1),
            height: config.height.max(1),
            x: config.position.map_or(0, |position| position.0),
            y: config.position.map_or(0, |position| position.1),
            title: config.title.clone(),
            min_width: config.min_width.max(1),
            min_height: config.min_height.max(1),
            max_width: config.max_width,
            max_height: config.max_height,
            resizable: config.resizable,
            visible_requested: config.visible,
            should_close: false,
            fullscreen: false,
            maximized: false,
            pointer_shape: PointerShape::Default,
        };

        queue
            .roundtrip(&mut window)
            .map_err(|error| werr(format!("Wayland registry roundtrip failed: {error}")))?;
        window.queue = Some(queue);
        window.setup_shell()?;
        Ok(window)
    }

    fn setup_shell(&mut self) -> Result<(), WindowError> {
        let compositor = self
            .compositor
            .clone()
            .ok_or_else(|| werr("compositor does not advertise wl_compositor"))?;
        let wm_base = self
            .wm_base
            .clone()
            .ok_or_else(|| werr("compositor does not advertise xdg_wm_base"))?;

        let surface = compositor.create_surface(&self.qh, ());
        let xdg_surface = wm_base.get_xdg_surface(&surface, &self.qh, ());
        let toplevel = xdg_surface.get_toplevel(&self.qh, ());

        toplevel.set_title(self.title.clone());
        toplevel.set_app_id("kitty".to_owned());
        toplevel.set_min_size(
            self.min_width.min(i32::MAX as u32) as i32,
            self.min_height.min(i32::MAX as u32) as i32,
        );
        if let Some(max_width) = self.max_width {
            toplevel.set_max_size(
                max_width.min(i32::MAX as u32) as i32,
                self.max_height.unwrap_or(u32::MAX).min(i32::MAX as u32) as i32,
            );
        } else if !self.resizable {
            toplevel.set_max_size(self.width as i32, self.height as i32);
        }

        if let Some(manager) = self.fractional_manager.clone() {
            if let Some(viewporter) = self.viewporter.clone() {
                self.viewport = Some(viewporter.get_viewport(&surface, &self.qh, ()));
                self.fractional_scale = Some(manager.get_fractional_scale(&surface, &self.qh, ()));
            }
        }

        if let (Some(manager), Some(seat)) = (self.data_device_manager.clone(), self.seat.clone()) {
            self.data_device = Some(manager.get_data_device(&seat, &self.qh, ()));
        }

        self.surface = Some(surface.clone());
        self.xdg_surface = Some(xdg_surface);
        self.toplevel = Some(toplevel);
        surface.commit();
        Ok(())
    }

    fn apply_integer_scale(&mut self) {
        if let Some(surface) = &self.surface {
            surface.set_buffer_scale(self.integer_scale.max(1));
        }
    }

    fn refresh_integer_scale(&mut self) {
        self.integer_scale = self
            .output_scales
            .values()
            .copied()
            .max()
            .unwrap_or(1)
            .max(1);
    }

    fn effective_scale(&self) -> f64 {
        if let Some(scale) = self.fractional_scale_factor {
            scale
        } else {
            self.integer_scale.max(1) as f64
        }
    }

    fn request_frame_callback(&mut self) {
        if self.frame_pending {
            return;
        }
        if let Some(surface) = &self.surface {
            self.frame_callback = Some(surface.frame(&self.qh, ()));
            self.frame_pending = true;
        }
    }

    fn ensure_shm_buffer(&mut self, width: u32, height: u32) -> Result<(), WindowError> {
        if self
            .shm_buffer
            .as_ref()
            .is_some_and(|buffer| buffer.width == width && buffer.height == height)
        {
            return Ok(());
        }
        let shm = self
            .shm
            .clone()
            .ok_or_else(|| werr("compositor does not advertise wl_shm"))?;
        if let Some(old) = self.shm_buffer.take() {
            self.retired_buffers.push(old);
        }
        self.shm_buffer = Some(ShmBuffer::new(&shm, &self.qh, width, height)?);
        Ok(())
    }

    /// Present a solid-colour frame through `wl_shm`. This is the fallback when
    /// EGL is unavailable, and is also useful in headless tests.
    pub fn present_clear(&mut self, color: [f32; 4]) -> Result<(), WindowError> {
        let surface = self
            .surface
            .clone()
            .ok_or_else(|| werr("Wayland surface has not been created"))?;
        if !self.configure.configured {
            return Err(werr("the compositor has not configured the surface yet"));
        }
        let scale = self.effective_scale().max(0.1);
        let buffer_width = ((self.width as f64 * scale).round() as u32).max(1);
        let buffer_height = ((self.height as f64 * scale).round() as u32).max(1);
        self.ensure_shm_buffer(buffer_width, buffer_height)?;
        if let Some(buffer) = self.shm_buffer.as_mut() {
            buffer.fill(color);
            let wl_buffer = buffer.buffer.clone();
            surface.attach(Some(&wl_buffer), 0, 0);
            if self.fractional_scale.is_some() {
                surface.set_buffer_scale(1);
                if let Some(viewport) = &self.viewport {
                    viewport.set_destination(self.width as i32, self.height as i32);
                }
            } else {
                surface.set_buffer_scale(self.integer_scale.max(1));
            }
            if surface.version() >= 4 {
                surface.damage_buffer(0, 0, buffer_width as i32, buffer_height as i32);
            } else {
                surface.damage(0, 0, self.width as i32, self.height as i32);
            }
            self.request_frame_callback();
            surface.commit();
        }
        self.visible_requested = true;
        Ok(())
    }

    pub fn wl_surface_ptr(&self) -> *mut c_void {
        self.surface
            .as_ref()
            .map(|surface| surface.id().as_ptr().cast())
            .unwrap_or(ptr::null_mut())
    }

    /// Native `wl_display` pointer, suitable for `eglGetPlatformDisplay`.
    pub fn display_ptr(&self) -> *mut c_void {
        self.connection.backend().display_ptr().cast()
    }

    pub fn scale_factor(&self) -> f64 {
        self.effective_scale()
    }

    pub fn integer_scale(&self) -> i32 {
        self.integer_scale
    }

    pub fn fractional_scale(&self) -> Option<f64> {
        self.fractional_scale_factor
    }

    pub fn detectable_autorepeat(&self) -> bool {
        self.repeat_rate > 0
    }

    pub fn repeat_info(&self) -> (i32, i32) {
        (self.repeat_rate, self.repeat_delay)
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn set_title(&mut self, title: &str) -> Result<(), WindowError> {
        if let Some(toplevel) = &self.toplevel {
            toplevel.set_title(title.to_owned());
        }
        self.title = title.to_owned();
        Ok(())
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn position(&self) -> (i32, i32) {
        (self.x, self.y)
    }

    pub fn screen_size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn set_size(&mut self, width: u32, height: u32) {
        let width = width.max(1);
        let height = height.max(1);
        if (width, height) == (self.width, self.height) {
            return;
        }
        self.width = width;
        self.height = height;
        self.inbox.push_back(WindowEvent::Resize { width, height });
        if self.visible_requested {
            let _ = self.present_clear([0.0, 0.0, 0.0, 1.0]);
        }
    }

    pub fn set_position(&mut self, x: i32, y: i32) {
        // Wayland compositors own global placement. Keep the requested value so
        // callers that only use it for bookkeeping still have something sane.
        self.x = x;
        self.y = y;
    }

    pub fn set_size_limits(
        &mut self,
        min_width: u32,
        min_height: u32,
        max_width: Option<u32>,
        max_height: Option<u32>,
    ) {
        self.min_width = min_width.max(1);
        self.min_height = min_height.max(1);
        self.max_width = max_width;
        self.max_height = max_height;
        if let Some(toplevel) = &self.toplevel {
            toplevel.set_min_size(
                self.min_width.min(i32::MAX as u32) as i32,
                self.min_height.min(i32::MAX as u32) as i32,
            );
            if let Some(max_width) = max_width {
                toplevel.set_max_size(
                    max_width.min(i32::MAX as u32) as i32,
                    max_height.unwrap_or(u32::MAX).min(i32::MAX as u32) as i32,
                );
            }
        }
    }

    pub fn min_size(&self) -> (u32, u32) {
        (self.min_width, self.min_height)
    }

    pub fn show(&mut self) {
        self.visible_requested = true;
        let _ = self.present_clear([0.0, 0.0, 0.0, 1.0]);
    }

    pub fn hide(&mut self) {
        self.visible_requested = false;
        if let Some(surface) = &self.surface {
            surface.attach(None, 0, 0);
            surface.commit();
        }
    }

    pub fn is_visible(&self) -> bool {
        self.visible_requested && self.configure.configured
    }

    pub fn focus(&mut self) {
        // Focus is owned by the compositor on Wayland.
    }

    pub fn raise(&mut self) {
        // Raising is owned by the compositor on Wayland.
    }

    pub fn set_pointer_shape(&mut self, shape: PointerShape) {
        // Cursor themes require wl_cursor and are outside this backend's scope.
        self.pointer_shape = shape;
    }

    pub fn pointer_shape(&self) -> PointerShape {
        self.pointer_shape
    }

    pub fn bell(&mut self) {}

    pub fn set_fullscreen(&mut self, fullscreen: bool) {
        if fullscreen == self.fullscreen {
            return;
        }
        if let Some(toplevel) = &self.toplevel {
            if fullscreen {
                toplevel.set_fullscreen(None);
            } else {
                toplevel.unset_fullscreen();
            }
        }
        self.fullscreen = fullscreen;
    }

    pub fn is_fullscreen(&self) -> bool {
        self.fullscreen
    }

    pub fn set_maximized(&mut self, maximized: bool) {
        if maximized == self.maximized {
            return;
        }
        if let Some(toplevel) = &self.toplevel {
            if maximized {
                toplevel.set_maximized();
            } else {
                toplevel.unset_maximized();
            }
        }
        self.maximized = maximized;
    }

    pub fn is_maximized(&self) -> bool {
        self.maximized
    }

    pub fn wakeup_handle(&self) -> WakeupHandle {
        WakeupHandle::from_pipe(Arc::clone(&self.wakeup))
    }

    pub fn wakeup(&self) {
        self.wakeup.wake();
    }

    pub fn should_close(&self) -> bool {
        self.should_close
    }

    pub fn request_close(&mut self) {
        self.should_close = true;
    }

    pub fn frame_pending(&self) -> bool {
        self.frame_pending
    }

    pub fn frame_count(&self) -> u64 {
        self.frame_count
    }

    pub fn poll_events(&mut self, timeout_ms: i32) -> Vec<WindowEvent> {
        if self.inbox.is_empty() {
            self.pump(timeout_ms);
        }
        let mut events: Vec<WindowEvent> = self.inbox.drain(..).collect();
        coalesce_events(&mut events);
        events
    }

    pub fn wait_events(&mut self) -> Vec<WindowEvent> {
        self.poll_events(-1)
    }

    fn pump(&mut self, timeout_ms: i32) {
        self.drain_clipboard_reads();
        let start = Instant::now();
        let mut woken = false;
        let mut queue = match self.queue.take() {
            Some(queue) => queue,
            None => return,
        };

        let _ = queue.dispatch_pending(self);
        let _ = self.connection.flush();

        let mut remaining = if timeout_ms < 0 {
            -1
        } else {
            let elapsed = start.elapsed();
            let total = Duration::from_millis(timeout_ms.max(0) as u64);
            if elapsed >= total {
                0
            } else {
                (total - elapsed).as_millis().max(1).min(c_int::MAX as u128) as c_int
            }
        };

        if !self.pending_reads.is_empty() {
            remaining = if remaining < 0 { 50 } else { remaining.min(50) };
        }
        if let Some(due) = self.repeat_due {
            let now = Instant::now();
            let until = due.saturating_duration_since(now);
            let ms = until.as_millis().max(1).min(c_int::MAX as u128) as c_int;
            remaining = if remaining < 0 { ms } else { remaining.min(ms) };
        }

        if self.inbox.is_empty() && remaining != 0 {
            if let Some(guard) = queue.prepare_read() {
                let mut fds = [
                    PollFd {
                        fd: guard.connection_fd().as_raw_fd(),
                        events: POLLIN,
                        revents: 0,
                    },
                    PollFd {
                        fd: self.wakeup.read_fd(),
                        events: POLLIN,
                        revents: 0,
                    },
                ];
                let ready = unsafe { poll(fds.as_mut_ptr(), 2, remaining) };
                if ready > 0 {
                    if fds[1].revents & POLLIN != 0 {
                        woken |= self.wakeup.drain();
                    }
                    if fds[0].revents & POLLIN != 0 {
                        let _ = guard.read();
                    }
                }
            }
        }

        let _ = queue.dispatch_pending(self);
        let _ = self.connection.flush();
        self.queue = Some(queue);

        if woken {
            self.inbox.push_back(WindowEvent::Wakeup);
        }
        self.drain_clipboard_reads();
        self.repeat_tick();
    }

    fn repeat_tick(&mut self) {
        let Some(due) = self.repeat_due else {
            return;
        };
        let now = Instant::now();
        if now < due {
            return;
        }
        if let Some(mut key) = self.repeat_key.clone() {
            key.action = KeyAction::Repeat;
            self.inbox.push_back(WindowEvent::Key(key));
        }
        let interval = if self.repeat_rate > 0 {
            Duration::from_secs_f64(1.0 / self.repeat_rate as f64)
        } else {
            Duration::from_millis(30)
        };
        self.repeat_due = Some(now + interval);
    }

    fn modifiers(&self) -> Modifiers {
        self.xkb
            .modifiers(self.depressed | self.latched | self.locked)
    }

    fn handle_key_event(&mut self, key: u32, down: bool, serial: u32) {
        self.serial = serial;
        let action = match classify_key_action(&mut self.pressed_keys, key, down) {
            Some(action) => action,
            None => return,
        };
        let (symbol, base, text) = self.xkb.key(key, action);
        let modifiers = self.modifiers();
        let input = KeyInput {
            keysym: symbol,
            base_keysym: base,
            keycode: key + 8,
            text,
            modifiers,
            action,
        };
        if !is_modifier_key(symbol) {
            if action.is_release() {
                self.repeat_key = None;
                self.repeat_due = None;
            } else if self.repeat_rate > 0 {
                self.repeat_key = Some(input.clone());
                self.repeat_due =
                    Some(Instant::now() + Duration::from_millis(self.repeat_delay.max(0) as u64));
            }
        }
        self.inbox.push_back(WindowEvent::Key(input));
    }

    // {{{ Clipboard

    fn clipboard_selection_supported(selection: Selection) -> bool {
        matches!(selection, Selection::Clipboard)
    }

    pub fn set_clipboard_text(
        &mut self,
        selection: Selection,
        text: &str,
    ) -> Result<(), WindowError> {
        if !Self::clipboard_selection_supported(selection) {
            return Err(werr(
                "the PRIMARY selection is not implemented by the Wayland backend",
            ));
        }
        self.clipboard.set_text(selection, text.as_bytes());
        self.publish_clipboard(selection)
    }

    pub fn set_clipboard_targets(
        &mut self,
        selection: Selection,
        targets: Vec<(String, Vec<u8>)>,
    ) -> Result<(), WindowError> {
        if !Self::clipboard_selection_supported(selection) {
            return Err(werr(
                "the PRIMARY selection is not implemented by the Wayland backend",
            ));
        }
        let targets = targets
            .into_iter()
            .map(|(name, data)| ClipboardTarget { name, data })
            .collect();
        self.clipboard.set_targets(selection, targets);
        self.publish_clipboard(selection)
    }

    pub fn set_primary_text(&mut self, _text: &str) -> Result<(), WindowError> {
        Err(werr(
            "the PRIMARY selection is not implemented by the Wayland backend",
        ))
    }

    fn publish_clipboard(&mut self, selection: Selection) -> Result<(), WindowError> {
        let manager = self
            .data_device_manager
            .clone()
            .ok_or_else(|| werr("compositor does not advertise wl_data_device_manager"))?;
        let device = self
            .data_device
            .clone()
            .ok_or_else(|| werr("Wayland data device has not been created"))?;
        if self.serial == 0 {
            return Err(werr(
                "cannot own the clipboard before the compositor has sent an input serial",
            ));
        }
        let source = manager.create_data_source(&self.qh, ());
        for target in self.clipboard.targets(selection) {
            source.offer(target.name.clone());
        }
        if let Some(old) = self.data_source.take() {
            old.destroy();
        }
        device.set_selection(Some(&source), self.serial);
        self.data_source = Some(source);
        self.source_selection = Some(selection);
        Ok(())
    }

    pub fn has_clipboard_offer(&self, selection: Selection) -> bool {
        Self::clipboard_selection_supported(selection) && self.data_offer.is_some()
    }

    pub fn request_clipboard_text(
        &mut self,
        selection: Selection,
    ) -> Result<ClipboardToken, WindowError> {
        if !Self::clipboard_selection_supported(selection) {
            return Err(werr(
                "the PRIMARY selection is not implemented by the Wayland backend",
            ));
        }
        let offer = self
            .data_offer
            .clone()
            .ok_or_else(|| werr("clipboard selection has no owner"))?;
        self.clipboard_seq = self.clipboard_seq.wrapping_add(1).max(1);
        let sequence = self.clipboard_seq;
        let token = self.clipboard.begin(selection, "text/plain", sequence);
        let mime = match preferred_clipboard_mime(&self.data_offer_mimes) {
            Some(mime) => mime.to_owned(),
            None => {
                self.clipboard.complete(
                    sequence,
                    Err(ClipboardError::UnsupportedTarget("text/plain".to_owned())),
                );
                return Ok(token);
            }
        };
        let (read, write) = UnixStream::pair()
            .map_err(|error| werr(format!("failed to create clipboard pipe: {error}")))?;
        read.set_nonblocking(true)
            .map_err(|error| werr(format!("failed to configure clipboard pipe: {error}")))?;
        offer.receive(mime.clone(), write.as_fd());
        drop(write);
        self.pending_reads.push(ClipboardRead {
            token,
            selection,
            target: mime,
            file: read,
            data: Vec::new(),
        });
        Ok(token)
    }

    pub fn request_clipboard_targets(
        &mut self,
        selection: Selection,
    ) -> Result<ClipboardToken, WindowError> {
        if !Self::clipboard_selection_supported(selection) {
            return Err(werr(
                "the PRIMARY selection is not implemented by the Wayland backend",
            ));
        }
        if self.data_offer.is_none() {
            return Err(werr("clipboard selection has no owner"));
        }
        self.clipboard_seq = self.clipboard_seq.wrapping_add(1).max(1);
        let sequence = self.clipboard_seq;
        let token = self.clipboard.begin(selection, "TARGETS", sequence);
        let mut targets = vec!["TARGETS".to_owned()];
        targets.extend(self.data_offer_mimes.iter().cloned());
        self.clipboard
            .complete(sequence, Ok(ClipboardReply::Targets(targets.clone())));
        self.inbox.push_back(WindowEvent::SelectionNotify {
            selection,
            target: "TARGETS".to_owned(),
            succeeded: true,
        });
        Ok(token)
    }

    pub fn take_clipboard_result(
        &mut self,
        token: ClipboardToken,
    ) -> Option<Result<ClipboardReply, ClipboardError>> {
        self.clipboard.take_result(token)
    }

    pub fn pending_clipboard_requests(&self) -> usize {
        self.clipboard.pending_count()
    }

    pub fn clipboard_text_blocking(
        &mut self,
        selection: Selection,
        timeout: Duration,
    ) -> Result<String, ClipboardError> {
        let token = self
            .request_clipboard_text(selection)
            .map_err(|error| ClipboardError::BadReply(error.to_string()))?;
        match self.wait_for_clipboard(token, timeout)? {
            ClipboardReply::Text(text) => Ok(text),
            ClipboardReply::Data { data, .. } => Ok(String::from_utf8_lossy(&data).into_owned()),
            ClipboardReply::Targets(_) => {
                Err(ClipboardError::UnsupportedTarget("TARGETS".to_owned()))
            }
        }
    }

    pub fn clipboard_targets_blocking(
        &mut self,
        selection: Selection,
        timeout: Duration,
    ) -> Result<Vec<String>, ClipboardError> {
        let token = self
            .request_clipboard_targets(selection)
            .map_err(|error| ClipboardError::BadReply(error.to_string()))?;
        match self.wait_for_clipboard(token, timeout)? {
            ClipboardReply::Targets(targets) => Ok(targets),
            _ => Err(ClipboardError::BadReply(
                "expected a TARGETS reply".to_owned(),
            )),
        }
    }

    fn wait_for_clipboard(
        &mut self,
        token: ClipboardToken,
        timeout: Duration,
    ) -> Result<ClipboardReply, ClipboardError> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(reply) = self.clipboard.take_result(token) {
                return reply;
            }
            if self.should_close {
                return Err(ClipboardError::WindowClosed);
            }
            let now = Instant::now();
            if now >= deadline {
                self.clipboard.forget(token);
                self.pending_reads.retain(|read| read.token != token);
                return Err(ClipboardError::Timeout);
            }
            let slice = (deadline - now).as_millis().clamp(1, 50) as i32;
            self.pump(slice);
        }
    }

    fn drain_clipboard_reads(&mut self) {
        let mut index = 0;
        while index < self.pending_reads.len() {
            let mut finished = false;
            let mut failure = None;
            {
                let read = &mut self.pending_reads[index];
                let mut buffer = [0u8; 8192];
                loop {
                    match read.file.read(&mut buffer) {
                        Ok(0) => {
                            finished = true;
                            break;
                        }
                        Ok(count) => read.data.extend_from_slice(&buffer[..count]),
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                        Err(error) => {
                            failure = Some(ClipboardError::BadReply(error.to_string()));
                            finished = true;
                            break;
                        }
                    }
                }
            }
            if finished {
                let read = self.pending_reads.remove(index);
                let succeeded = failure.is_none();
                let reply = match failure {
                    Some(error) => Err(error),
                    None => Ok(ClipboardReply::Text(
                        String::from_utf8_lossy(&read.data).into_owned(),
                    )),
                };
                let sequence = read.token.raw();
                self.clipboard.complete(sequence, reply);
                self.inbox.push_back(WindowEvent::SelectionNotify {
                    selection: read.selection,
                    target: read.target,
                    succeeded,
                });
            } else {
                index += 1;
            }
        }
    }

    // }}}
    // {{{ Synthetic events for tests

    /// Inject a neutral event into the local inbox. This never sends input to
    /// the compositor; it exists so tests and callers can exercise the same
    /// event consumption path as X11.
    pub fn inject_event(&mut self, event: &WindowEvent) -> Result<(), WindowError> {
        match event {
            WindowEvent::Resize { width, height } => {
                self.set_size(*width, *height);
                Ok(())
            }
            WindowEvent::Move { x, y } => {
                self.set_position(*x, *y);
                self.inbox.push_back(event.clone());
                Ok(())
            }
            WindowEvent::CloseRequested => {
                self.should_close = true;
                self.inbox.push_back(event.clone());
                Ok(())
            }
            WindowEvent::Focus { focused } => {
                self.keyboard_focused = *focused;
                if !focused {
                    self.pressed_keys.clear();
                    self.repeat_key = None;
                    self.repeat_due = None;
                }
                self.inbox.push_back(event.clone());
                Ok(())
            }
            WindowEvent::Wakeup => {
                self.wakeup();
                Ok(())
            }
            WindowEvent::Key(_)
            | WindowEvent::MouseMove { .. }
            | WindowEvent::MouseButton { .. }
            | WindowEvent::Scroll { .. }
            | WindowEvent::Enter { .. }
            | WindowEvent::Leave { .. }
            | WindowEvent::Expose { .. }
            | WindowEvent::SelectionClear { .. }
            | WindowEvent::SelectionRequest { .. }
            | WindowEvent::SelectionNotify { .. } => {
                self.inbox.push_back(event.clone());
                Ok(())
            }
        }
    }

    // }}}
}

fn is_modifier_key(symbol: u32) -> bool {
    matches!(
        symbol,
        keysym::SHIFT_L
            | keysym::SHIFT_R
            | keysym::CONTROL_L
            | keysym::CONTROL_R
            | keysym::ALT_L
            | keysym::ALT_R
            | keysym::SUPER_L
            | keysym::SUPER_R
            | keysym::HYPER_L
            | keysym::HYPER_R
            | keysym::META_L
            | keysym::META_R
            | keysym::ISO_LEVEL3_SHIFT
            | 0xffe5
    )
}

// }}}
// {{{ Wayland dispatch implementations

impl Dispatch<wl_registry::WlRegistry, ()> for WaylandWindow {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            match interface.as_str() {
                "wl_compositor" if state.compositor.is_none() => {
                    state.compositor = Some(registry.bind(name, version.clamp(1, 6), qh, ()));
                }
                "wl_shm" if state.shm.is_none() => {
                    state.shm = Some(registry.bind(name, version.clamp(1, 1), qh, ()));
                }
                "xdg_wm_base" if state.wm_base.is_none() => {
                    state.wm_base = Some(registry.bind(name, version.clamp(1, 6), qh, ()));
                }
                "wl_seat" if state.seat.is_none() => {
                    state.seat = Some(registry.bind(name, version.clamp(1, 7), qh, ()));
                }
                "wl_output" => {
                    let output = registry.bind::<wl_output::WlOutput, _, _>(
                        name,
                        version.clamp(1, 4),
                        qh,
                        (),
                    );
                    state.outputs.push(output);
                }
                "wl_data_device_manager" if state.data_device_manager.is_none() => {
                    state.data_device_manager =
                        Some(registry.bind(name, version.clamp(1, 3), qh, ()));
                }
                "wp_fractional_scale_manager_v1" if state.fractional_manager.is_none() => {
                    state.fractional_manager =
                        Some(registry.bind(name, version.clamp(1, 1), qh, ()));
                }
                "wp_viewporter" if state.viewporter.is_none() => {
                    state.viewporter = Some(registry.bind(name, version.clamp(1, 1), qh, ()));
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<wl_surface::WlSurface, ()> for WaylandWindow {
    fn event(
        state: &mut Self,
        _: &wl_surface::WlSurface,
        event: wl_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_surface::Event::PreferredBufferScale { factor } => {
                if state.fractional_scale_factor.is_none() && factor > 0 {
                    state.integer_scale = factor;
                    state.apply_integer_scale();
                }
            }
            wl_surface::Event::Enter { .. } | wl_surface::Event::Leave { .. } => {}
            _ => {}
        }
    }
}

impl Dispatch<wl_output::WlOutput, ()> for WaylandWindow {
    fn event(
        state: &mut Self,
        output: &wl_output::WlOutput,
        event: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_output::Event::Scale { factor } = event {
            let key = output.id().protocol_id();
            state.output_scales.insert(key, factor.max(1));
            state.refresh_integer_scale();
            state.apply_integer_scale();
        }
    }
}

impl Dispatch<xdg_wm_base::XdgWmBase, ()> for WaylandWindow {
    fn event(
        _: &mut Self,
        wm_base: &xdg_wm_base::XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm_base.pong(serial);
        }
    }
}

impl Dispatch<xdg_surface::XdgSurface, ()> for WaylandWindow {
    fn event(
        state: &mut Self,
        xdg_surface: &xdg_surface::XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            xdg_surface.ack_configure(serial);
            if let Some((width, height)) = state.configure.acknowledge((state.width, state.height))
            {
                state.width = width;
                state.height = height;
                state.inbox.push_back(WindowEvent::Resize { width, height });
            }
            if state.visible_requested {
                let _ = state.present_clear([0.0, 0.0, 0.0, 1.0]);
            }
        }
    }
}

impl Dispatch<xdg_toplevel::XdgToplevel, ()> for WaylandWindow {
    fn event(
        state: &mut Self,
        _: &xdg_toplevel::XdgToplevel,
        event: xdg_toplevel::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            xdg_toplevel::Event::Configure {
                width,
                height,
                states,
            } => {
                state
                    .configure
                    .set_pending(width.max(0) as u32, height.max(0) as u32);
                for item in states {
                    match item {
                        1 => state.maximized = true,
                        2 => state.fullscreen = true,
                        _ => {}
                    }
                }
            }
            xdg_toplevel::Event::Close => {
                state.should_close = true;
                state.inbox.push_back(WindowEvent::CloseRequested);
            }
            xdg_toplevel::Event::ConfigureBounds { .. }
            | xdg_toplevel::Event::WmCapabilities { .. } => {}
            _ => {}
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for WaylandWindow {
    fn event(
        state: &mut Self,
        seat: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(capabilities),
        } = event
        {
            if capabilities.contains(wl_seat::Capability::Keyboard) && state.keyboard.is_none() {
                state.keyboard = Some(seat.get_keyboard(qh, ()));
            }
            if capabilities.contains(wl_seat::Capability::Pointer) && state.pointer.is_none() {
                state.pointer = Some(seat.get_pointer(qh, ()));
            }
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for WaylandWindow {
    fn event(
        state: &mut Self,
        _: &wl_keyboard::WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_keyboard::Event::Keymap { format, fd, size } => {
                let _ = state.xkb.update_keymap(fd, size, format);
            }
            wl_keyboard::Event::Enter { serial, .. } => {
                state.serial = serial;
                state.keyboard_focused = true;
                state.inbox.push_back(WindowEvent::Focus { focused: true });
            }
            wl_keyboard::Event::Leave { .. } => {
                state.keyboard_focused = false;
                state.pressed_keys.clear();
                state.repeat_key = None;
                state.repeat_due = None;
                state.inbox.push_back(WindowEvent::Focus { focused: false });
            }
            wl_keyboard::Event::Key {
                serial,
                key,
                state: WEnum::Value(key_state),
                ..
            } => {
                state.handle_key_event(
                    key,
                    matches!(key_state, wl_keyboard::KeyState::Pressed),
                    serial,
                );
            }
            wl_keyboard::Event::Key { .. } => {}
            wl_keyboard::Event::Modifiers {
                mods_depressed,
                mods_latched,
                mods_locked,
                group,
                ..
            } => {
                state.depressed = mods_depressed;
                state.latched = mods_latched;
                state.locked = mods_locked;
                state.group = group;
                state
                    .xkb
                    .update_modifiers(mods_depressed, mods_latched, mods_locked, group);
            }
            wl_keyboard::Event::RepeatInfo { rate, delay } => {
                state.repeat_rate = rate;
                state.repeat_delay = delay;
                if rate <= 0 {
                    state.repeat_key = None;
                    state.repeat_due = None;
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for WaylandWindow {
    fn event(
        state: &mut Self,
        _: &wl_pointer::WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_pointer::Event::Enter {
                serial,
                surface_x,
                surface_y,
                ..
            } => {
                state.serial = serial;
                state.pointer_x = surface_x;
                state.pointer_y = surface_y;
                state.inbox.push_back(WindowEvent::Enter {
                    x: surface_x,
                    y: surface_y,
                    modifiers: state.modifiers(),
                });
            }
            wl_pointer::Event::Leave { .. } => {
                state.inbox.push_back(WindowEvent::Leave {
                    x: state.pointer_x,
                    y: state.pointer_y,
                    modifiers: state.modifiers(),
                });
            }
            wl_pointer::Event::Motion {
                surface_x,
                surface_y,
                ..
            } => {
                state.pointer_x = surface_x;
                state.pointer_y = surface_y;
                state.inbox.push_back(WindowEvent::MouseMove {
                    x: surface_x,
                    y: surface_y,
                    modifiers: state.modifiers(),
                });
            }
            wl_pointer::Event::Button {
                serial,
                button,
                state: button_state,
                ..
            } => {
                state.serial = serial;
                if let WEnum::Value(button_state) = button_state {
                    state.inbox.push_back(WindowEvent::MouseButton {
                        button: mouse_button_from_evdev(button),
                        pressed: matches!(button_state, wl_pointer::ButtonState::Pressed),
                        x: state.pointer_x,
                        y: state.pointer_y,
                        modifiers: state.modifiers(),
                    });
                }
            }
            wl_pointer::Event::Axis { axis, value, .. } => {
                let (delta_x, delta_y, button) = axis_to_scroll(axis, value);
                if button != 0 {
                    state.inbox.push_back(WindowEvent::Scroll {
                        x: state.pointer_x,
                        y: state.pointer_y,
                        delta_x,
                        delta_y,
                        button,
                        modifiers: state.modifiers(),
                    });
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_data_device::WlDataDevice, ()> for WaylandWindow {
    event_created_child!(WaylandWindow, wl_data_device::WlDataDevice, [
        0 => (wl_data_offer::WlDataOffer, ()),
    ]);

    fn event(
        state: &mut Self,
        _: &wl_data_device::WlDataDevice,
        event: wl_data_device::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_data_device::Event::DataOffer { id } => {
                state.pending_offer = Some(id);
                state.data_offer_mimes.clear();
            }
            wl_data_device::Event::Selection { id } => {
                if let Some(old) = state.data_offer.take() {
                    old.destroy();
                }
                state.data_offer = id.or_else(|| state.pending_offer.take());
                if state.data_offer.is_none() {
                    state.data_offer_mimes.clear();
                }
            }
            // Drag-and-drop enter/motion/leave/drop are not implemented; they
            // must not disturb the clipboard selection.
            _ => {}
        }
    }
}

impl Dispatch<wl_data_offer::WlDataOffer, ()> for WaylandWindow {
    fn event(
        state: &mut Self,
        _: &wl_data_offer::WlDataOffer,
        event: wl_data_offer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_data_offer::Event::Offer { mime_type } = event {
            if !state.data_offer_mimes.contains(&mime_type) {
                state.data_offer_mimes.push(mime_type);
            }
        }
    }
}

impl Dispatch<wl_data_source::WlDataSource, ()> for WaylandWindow {
    fn event(
        state: &mut Self,
        _: &wl_data_source::WlDataSource,
        event: wl_data_source::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_data_source::Event::Send { mime_type, fd } => {
                let selection = state.source_selection.unwrap_or(Selection::Clipboard);
                let mut file = File::from(fd);
                if mime_type == "TARGETS" {
                    let mut data = state.clipboard.target_names(selection).join("\n");
                    data.push('\n');
                    let _ = file.write_all(data.as_bytes());
                } else if let Some(data) = state.clipboard.target_data(selection, &mime_type) {
                    let _ = file.write_all(data);
                }
                state.inbox.push_back(WindowEvent::SelectionRequest {
                    selection,
                    target: mime_type,
                });
            }
            wl_data_source::Event::Cancelled => {
                let selection = state.source_selection.unwrap_or(Selection::Clipboard);
                state.clipboard.clear(selection);
                state.data_source = None;
                state.source_selection = None;
                state
                    .inbox
                    .push_back(WindowEvent::SelectionClear { selection });
            }
            wl_data_source::Event::Target { .. }
            | wl_data_source::Event::DndDropPerformed
            | wl_data_source::Event::DndFinished
            | wl_data_source::Event::Action { .. } => {}
            _ => {}
        }
    }
}

impl Dispatch<wl_callback::WlCallback, ()> for WaylandWindow {
    fn event(
        state: &mut Self,
        _: &wl_callback::WlCallback,
        event: wl_callback::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_callback::Event::Done { .. } = event {
            state.frame_pending = false;
            state.frame_callback = None;
            state.frame_count = state.frame_count.saturating_add(1);
        }
    }
}

impl Dispatch<wp_fractional_scale_v1::WpFractionalScaleV1, ()> for WaylandWindow {
    fn event(
        state: &mut Self,
        _: &wp_fractional_scale_v1::WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event {
            if scale > 0 {
                state.fractional_scale_factor = Some(scale as f64 / 120.0);
            }
        }
    }
}

delegate_noop!(WaylandWindow: ignore wl_compositor::WlCompositor);
delegate_noop!(WaylandWindow: ignore wl_shm::WlShm);
delegate_noop!(WaylandWindow: ignore wl_shm_pool::WlShmPool);
delegate_noop!(WaylandWindow: ignore wl_buffer::WlBuffer);
delegate_noop!(WaylandWindow: ignore wl_data_device_manager::WlDataDeviceManager);
delegate_noop!(WaylandWindow: ignore wp_viewporter::WpViewporter);
delegate_noop!(WaylandWindow: ignore wp_viewport::WpViewport);
delegate_noop!(WaylandWindow: ignore wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1);

// }}}
// {{{ Tests that do not need a compositor

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configure_events_are_coalesced_and_zero_keeps_the_current_size() {
        let mut state = ConfigureState::default();
        state.set_pending(640, 480);
        state.set_pending(800, 600);
        assert_eq!(state.acknowledge((320, 200)), Some((800, 600)));
        assert!(state.configured);

        state.set_pending(0, 0);
        assert_eq!(state.acknowledge((800, 600)), None);

        state.set_pending(0, 700);
        assert_eq!(state.acknowledge((800, 600)), Some((800, 700)));
        assert_eq!(state.acknowledge((800, 700)), None);
    }

    #[test]
    fn virtual_keyboard_classifies_press_repeat_and_release() {
        let mut pressed = HashSet::new();
        assert_eq!(
            classify_key_action(&mut pressed, 30, true),
            Some(KeyAction::Press)
        );
        assert_eq!(
            classify_key_action(&mut pressed, 30, true),
            Some(KeyAction::Repeat)
        );
        assert_eq!(
            classify_key_action(&mut pressed, 30, false),
            Some(KeyAction::Release)
        );
        assert_eq!(classify_key_action(&mut pressed, 30, false), None);
    }

    #[test]
    fn pointer_buttons_and_axes_match_the_neutral_api() {
        assert_eq!(mouse_button_from_evdev(0x110), MouseButton::Left);
        assert_eq!(mouse_button_from_evdev(0x111), MouseButton::Right);
        assert_eq!(mouse_button_from_evdev(0x112), MouseButton::Middle);
        assert_eq!(mouse_button_from_evdev(0x113), MouseButton::Back);
        assert_eq!(mouse_button_from_evdev(0x114), MouseButton::Forward);
        assert_eq!(mouse_button_from_evdev(7), MouseButton::Other(7));
        assert_eq!(
            axis_to_scroll(WEnum::Value(wl_pointer::Axis::VerticalScroll), 10.0),
            (0.0, -1.0, 5)
        );
        assert_eq!(
            axis_to_scroll(WEnum::Value(wl_pointer::Axis::VerticalScroll), -10.0),
            (0.0, 1.0, 4)
        );
        assert_eq!(
            axis_to_scroll(WEnum::Value(wl_pointer::Axis::HorizontalScroll), 10.0),
            (1.0, 0.0, 7)
        );
    }

    #[test]
    fn clipboard_prefers_utf8_plain_text() {
        let mimes = vec![
            "image/png".to_owned(),
            "STRING".to_owned(),
            "text/plain;charset=utf-8".to_owned(),
        ];
        assert_eq!(
            preferred_clipboard_mime(&mimes),
            Some("text/plain;charset=utf-8")
        );
        assert_eq!(preferred_clipboard_mime(&[]), None);
    }

    #[test]
    fn event_coalescing_keeps_the_last_resize_and_motion() {
        let mut events = vec![
            WindowEvent::Resize {
                width: 100,
                height: 50,
            },
            WindowEvent::Resize {
                width: 200,
                height: 80,
            },
            WindowEvent::MouseMove {
                x: 1.0,
                y: 2.0,
                modifiers: Modifiers::default(),
            },
            WindowEvent::MouseMove {
                x: 3.0,
                y: 4.0,
                modifiers: Modifiers::default(),
            },
            WindowEvent::Focus { focused: true },
        ];
        coalesce_events(&mut events);
        assert_eq!(events.len(), 3);
        assert!(matches!(
            events[0],
            WindowEvent::Resize {
                width: 200,
                height: 80
            }
        ));
        assert!(matches!(
            events[1],
            WindowEvent::MouseMove { x, y, .. } if x == 3.0 && y == 4.0
        ));
    }

    #[test]
    fn fallback_keymap_covers_printable_keys() {
        let (symbol, text) = fallback_key(30, KeyAction::Press);
        assert_eq!(symbol, 0x61);
        assert_eq!(text.as_deref(), Some("a"));
        assert_eq!(fallback_key(30, KeyAction::Release).1, None);
        assert_eq!(fallback_key(1, KeyAction::Press).0, keysym::ESCAPE);
    }
}

// }}}
