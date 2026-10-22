// SPDX-License-Identifier: GPL-3.0-or-later
//! Raw Xlib backend: window creation, event pump, keyboard/mouse translation,
//! EWMH window state, cursor shapes and clipboard selections.
//!
//! This is an equivalent backend to the GLFW/X11 path kitty normally uses. It
//! deliberately uses raw FFI with no bindgen and no external crates.

use std::collections::{HashSet, VecDeque};
use std::ffi::{c_char, c_int, c_long, c_short, c_uchar, c_uint, c_ulong, c_void, CStr, CString};
use std::ptr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Once};
use std::time::{Duration, Instant};

use crate::clipboard::{
    ClipboardError, ClipboardReply, ClipboardState, ClipboardTarget, ClipboardToken,
};
use crate::types::{
    keysym, KeyAction, KeyInput, ModifierMasks, Modifiers, MouseButton, PointerShape, Selection,
    WindowConfig, WindowError, WindowEvent,
};
use crate::wakeup::{WakeupHandle, WakeupPipe};

// {{{ Raw Xlib declarations

#[allow(
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals,
    dead_code
)]
mod ffi {
    use super::*;

    pub type Display = c_void;
    pub type Window = c_ulong;
    pub type Atom = c_ulong;
    pub type Time = c_ulong;
    pub type KeySym = c_ulong;
    pub type KeyCode = c_uchar;
    pub type Bool = c_int;
    pub type Status = c_int;
    pub type Visual = c_void;

    pub const NONE: c_ulong = 0;
    pub const CURRENT_TIME: Time = 0;

    pub const KEY_PRESS: c_int = 2;
    pub const KEY_RELEASE: c_int = 3;
    pub const BUTTON_PRESS: c_int = 4;
    pub const BUTTON_RELEASE: c_int = 5;
    pub const MOTION_NOTIFY: c_int = 6;
    pub const ENTER_NOTIFY: c_int = 7;
    pub const LEAVE_NOTIFY: c_int = 8;
    pub const FOCUS_IN: c_int = 9;
    pub const FOCUS_OUT: c_int = 10;
    pub const EXPOSE: c_int = 12;
    pub const DESTROY_NOTIFY: c_int = 17;
    pub const UNMAP_NOTIFY: c_int = 18;
    pub const MAP_NOTIFY: c_int = 19;
    pub const CONFIGURE_NOTIFY: c_int = 22;
    pub const PROPERTY_NOTIFY: c_int = 28;
    pub const SELECTION_CLEAR: c_int = 29;
    pub const SELECTION_REQUEST: c_int = 30;
    pub const SELECTION_NOTIFY: c_int = 31;
    pub const CLIENT_MESSAGE: c_int = 33;

    pub const KEY_PRESS_MASK: c_long = 1 << 0;
    pub const KEY_RELEASE_MASK: c_long = 1 << 1;
    pub const BUTTON_PRESS_MASK: c_long = 1 << 2;
    pub const BUTTON_RELEASE_MASK: c_long = 1 << 3;
    pub const ENTER_WINDOW_MASK: c_long = 1 << 4;
    pub const LEAVE_WINDOW_MASK: c_long = 1 << 5;
    pub const POINTER_MOTION_MASK: c_long = 1 << 6;
    pub const EXPOSURE_MASK: c_long = 1 << 15;
    pub const STRUCTURE_NOTIFY_MASK: c_long = 1 << 17;
    pub const SUBSTRUCTURE_NOTIFY_MASK: c_long = 1 << 19;
    pub const SUBSTRUCTURE_REDIRECT_MASK: c_long = 1 << 20;
    pub const FOCUS_CHANGE_MASK: c_long = 1 << 21;
    pub const PROPERTY_CHANGE_MASK: c_long = 1 << 22;

    pub const IS_VIEWABLE: c_int = 2;
    pub const REVERT_TO_PARENT: c_int = 2;

    pub const PROP_MODE_REPLACE: c_int = 0;

    pub const XA_ATOM: Atom = 4;
    pub const XA_STRING: Atom = 31;
    pub const ANY_PROPERTY_TYPE: Atom = 0;

    pub const P_MIN_SIZE: c_long = 1 << 4;
    pub const P_MAX_SIZE: c_long = 1 << 5;
    pub const P_SIZE: c_long = 1 << 3;

    pub const NET_WM_STATE_REMOVE: c_long = 0;
    pub const NET_WM_STATE_ADD: c_long = 1;

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct XKeyEvent {
        pub type_: c_int,
        pub serial: c_ulong,
        pub send_event: Bool,
        pub display: *mut Display,
        pub window: Window,
        pub root: Window,
        pub subwindow: Window,
        pub time: Time,
        pub x: c_int,
        pub y: c_int,
        pub x_root: c_int,
        pub y_root: c_int,
        pub state: c_uint,
        pub keycode: c_uint,
        pub same_screen: Bool,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct XButtonEvent {
        pub type_: c_int,
        pub serial: c_ulong,
        pub send_event: Bool,
        pub display: *mut Display,
        pub window: Window,
        pub root: Window,
        pub subwindow: Window,
        pub time: Time,
        pub x: c_int,
        pub y: c_int,
        pub x_root: c_int,
        pub y_root: c_int,
        pub state: c_uint,
        pub button: c_uint,
        pub same_screen: Bool,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct XMotionEvent {
        pub type_: c_int,
        pub serial: c_ulong,
        pub send_event: Bool,
        pub display: *mut Display,
        pub window: Window,
        pub root: Window,
        pub subwindow: Window,
        pub time: Time,
        pub x: c_int,
        pub y: c_int,
        pub x_root: c_int,
        pub y_root: c_int,
        pub state: c_uint,
        pub is_hint: c_char,
        pub same_screen: Bool,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct XConfigureEvent {
        pub type_: c_int,
        pub serial: c_ulong,
        pub send_event: Bool,
        pub display: *mut Display,
        pub event: Window,
        pub window: Window,
        pub x: c_int,
        pub y: c_int,
        pub width: c_int,
        pub height: c_int,
        pub border_width: c_int,
        pub above: Window,
        pub override_redirect: Bool,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct XExposeEvent {
        pub type_: c_int,
        pub serial: c_ulong,
        pub send_event: Bool,
        pub display: *mut Display,
        pub window: Window,
        pub x: c_int,
        pub y: c_int,
        pub width: c_int,
        pub height: c_int,
        pub count: c_int,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct XCrossingEvent {
        pub type_: c_int,
        pub serial: c_ulong,
        pub send_event: Bool,
        pub display: *mut Display,
        pub window: Window,
        pub root: Window,
        pub subwindow: Window,
        pub time: Time,
        pub x: c_int,
        pub y: c_int,
        pub x_root: c_int,
        pub y_root: c_int,
        pub mode: c_int,
        pub detail: c_int,
        pub same_screen: Bool,
        pub focus: Bool,
        pub state: c_uint,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct XFocusChangeEvent {
        pub type_: c_int,
        pub serial: c_ulong,
        pub send_event: Bool,
        pub display: *mut Display,
        pub window: Window,
        pub mode: c_int,
        pub detail: c_int,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct XAnyEvent {
        pub type_: c_int,
        pub serial: c_ulong,
        pub send_event: Bool,
        pub display: *mut Display,
        pub window: Window,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct XClientMessageEvent {
        pub type_: c_int,
        pub serial: c_ulong,
        pub send_event: Bool,
        pub display: *mut Display,
        pub window: Window,
        pub message_type: Atom,
        pub format: c_int,
        pub data: [c_long; 5],
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct XSelectionClearEvent {
        pub type_: c_int,
        pub serial: c_ulong,
        pub send_event: Bool,
        pub display: *mut Display,
        pub window: Window,
        pub selection: Atom,
        pub time: Time,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct XSelectionRequestEvent {
        pub type_: c_int,
        pub serial: c_ulong,
        pub send_event: Bool,
        pub display: *mut Display,
        pub owner: Window,
        pub requestor: Window,
        pub selection: Atom,
        pub target: Atom,
        pub property: Atom,
        pub time: Time,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct XSelectionEvent {
        pub type_: c_int,
        pub serial: c_ulong,
        pub send_event: Bool,
        pub display: *mut Display,
        pub requestor: Window,
        pub selection: Atom,
        pub target: Atom,
        pub property: Atom,
        pub time: Time,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct XErrorEvent {
        pub type_: c_int,
        pub display: *mut Display,
        pub resourceid: c_ulong,
        pub serial: c_ulong,
        pub error_code: c_uchar,
        pub request_code: c_uchar,
        pub minor_code: c_uchar,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct XWindowAttributes {
        pub x: c_int,
        pub y: c_int,
        pub width: c_int,
        pub height: c_int,
        pub border_width: c_int,
        pub depth: c_int,
        pub visual: *mut Visual,
        pub root: Window,
        pub class: c_int,
        pub bit_gravity: c_int,
        pub win_gravity: c_int,
        pub backing_store: c_int,
        pub backing_planes: c_ulong,
        pub backing_pixel: c_ulong,
        pub save_under: Bool,
        pub colormap: c_ulong,
        pub map_installed: Bool,
        pub map_state: c_int,
        pub all_event_masks: c_long,
        pub your_event_mask: c_long,
        pub do_not_propagate_mask: c_long,
        pub override_redirect: Bool,
        pub screen: *mut c_void,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct XSizeHints {
        pub flags: c_long,
        pub x: c_int,
        pub y: c_int,
        pub width: c_int,
        pub height: c_int,
        pub min_width: c_int,
        pub min_height: c_int,
        pub max_width: c_int,
        pub max_height: c_int,
        pub width_inc: c_int,
        pub height_inc: c_int,
        pub min_aspect_x: c_int,
        pub min_aspect_y: c_int,
        pub max_aspect_x: c_int,
        pub max_aspect_y: c_int,
        pub base_width: c_int,
        pub base_height: c_int,
        pub win_gravity: c_int,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct XModifierKeymap {
        pub max_keypermod: c_int,
        pub modifiermap: *mut KeyCode,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub union XEvent {
        pub type_: c_int,
        pub xkey: XKeyEvent,
        pub xbutton: XButtonEvent,
        pub xmotion: XMotionEvent,
        pub xcrossing: XCrossingEvent,
        pub xconfigure: XConfigureEvent,
        pub xexpose: XExposeEvent,
        pub xfocus: XFocusChangeEvent,
        pub xany: XAnyEvent,
        pub xclient: XClientMessageEvent,
        pub xselectionclear: XSelectionClearEvent,
        pub xselectionrequest: XSelectionRequestEvent,
        pub xselection: XSelectionEvent,
        pub pad: [c_long; 24],
    }

    pub type XErrorHandler = Option<unsafe extern "C" fn(*mut Display, *mut XErrorEvent) -> c_int>;

    #[link(name = "X11")]
    unsafe extern "C" {
        pub fn XOpenDisplay(name: *const c_char) -> *mut Display;
        pub fn XCloseDisplay(display: *mut Display) -> c_int;
        pub fn XDefaultScreen(display: *mut Display) -> c_int;
        pub fn XRootWindow(display: *mut Display, screen: c_int) -> Window;
        pub fn XDefaultDepth(display: *mut Display, screen: c_int) -> c_int;
        pub fn XDisplayWidth(display: *mut Display, screen: c_int) -> c_int;
        pub fn XDisplayHeight(display: *mut Display, screen: c_int) -> c_int;
        pub fn XConnectionNumber(display: *mut Display) -> c_int;
        pub fn XCreateSimpleWindow(
            display: *mut Display,
            parent: Window,
            x: c_int,
            y: c_int,
            width: c_uint,
            height: c_uint,
            border_width: c_uint,
            border: c_ulong,
            background: c_ulong,
        ) -> Window;
        pub fn XDestroyWindow(display: *mut Display, window: Window) -> c_int;
        pub fn XMapWindow(display: *mut Display, window: Window) -> c_int;
        pub fn XUnmapWindow(display: *mut Display, window: Window) -> c_int;
        pub fn XRaiseWindow(display: *mut Display, window: Window) -> c_int;
        pub fn XMoveWindow(display: *mut Display, window: Window, x: c_int, y: c_int) -> c_int;
        pub fn XResizeWindow(
            display: *mut Display,
            window: Window,
            width: c_uint,
            height: c_uint,
        ) -> c_int;
        pub fn XMoveResizeWindow(
            display: *mut Display,
            window: Window,
            x: c_int,
            y: c_int,
            width: c_uint,
            height: c_uint,
        ) -> c_int;
        pub fn XStoreName(display: *mut Display, window: Window, name: *const c_char) -> c_int;
        pub fn XFetchName(display: *mut Display, window: Window, name: *mut *mut c_char) -> Status;
        pub fn XInternAtom(
            display: *mut Display,
            name: *const c_char,
            only_if_exists: Bool,
        ) -> Atom;
        pub fn XGetAtomName(display: *mut Display, atom: Atom) -> *mut c_char;
        pub fn XSetWMProtocols(
            display: *mut Display,
            window: Window,
            protocols: *mut Atom,
            count: c_int,
        ) -> Status;
        pub fn XSetWMNormalHints(display: *mut Display, window: Window, hints: *mut XSizeHints);
        pub fn XSelectInput(display: *mut Display, window: Window, mask: c_long) -> c_int;
        pub fn XSync(display: *mut Display, discard: Bool) -> c_int;
        pub fn XFlush(display: *mut Display) -> c_int;
        pub fn XPending(display: *mut Display) -> c_int;
        pub fn XNextEvent(display: *mut Display, event: *mut XEvent) -> c_int;
        pub fn XSendEvent(
            display: *mut Display,
            window: Window,
            propagate: Bool,
            event_mask: c_long,
            event: *mut XEvent,
        ) -> Status;
        pub fn XChangeProperty(
            display: *mut Display,
            window: Window,
            property: Atom,
            type_: Atom,
            format: c_int,
            mode: c_int,
            data: *const c_uchar,
            nelements: c_int,
        ) -> c_int;
        pub fn XDeleteProperty(display: *mut Display, window: Window, property: Atom) -> c_int;
        pub fn XGetWindowProperty(
            display: *mut Display,
            window: Window,
            property: Atom,
            long_offset: c_long,
            long_length: c_long,
            delete: Bool,
            req_type: Atom,
            actual_type: *mut Atom,
            actual_format: *mut c_int,
            nitems: *mut c_ulong,
            bytes_after: *mut c_ulong,
            prop: *mut *mut c_uchar,
        ) -> c_int;
        pub fn XGetWindowAttributes(
            display: *mut Display,
            window: Window,
            attributes: *mut XWindowAttributes,
        ) -> Status;
        pub fn XSetInputFocus(
            display: *mut Display,
            window: Window,
            revert_to: c_int,
            time: Time,
        ) -> c_int;
        pub fn XSetSelectionOwner(
            display: *mut Display,
            selection: Atom,
            owner: Window,
            time: Time,
        ) -> c_int;
        pub fn XGetSelectionOwner(display: *mut Display, selection: Atom) -> Window;
        pub fn XConvertSelection(
            display: *mut Display,
            selection: Atom,
            target: Atom,
            property: Atom,
            requestor: Window,
            time: Time,
        ) -> c_int;
        pub fn XCreateFontCursor(display: *mut Display, shape: c_uint) -> c_ulong;
        pub fn XDefineCursor(display: *mut Display, window: Window, cursor: c_ulong) -> c_int;
        pub fn XFreeCursor(display: *mut Display, cursor: c_ulong) -> c_int;
        pub fn XBell(display: *mut Display, percent: c_int) -> c_int;
        pub fn XLookupString(
            event: *mut XKeyEvent,
            buffer: *mut c_char,
            nbytes: c_int,
            keysym: *mut KeySym,
            compose_status: *mut c_void,
        ) -> c_int;
        pub fn XLookupKeysym(event: *mut XKeyEvent, index: c_int) -> KeySym;
        pub fn XkbKeycodeToKeysym(
            display: *mut Display,
            keycode: KeyCode,
            group: c_int,
            level: c_int,
        ) -> KeySym;
        pub fn XkbSetDetectableAutoRepeat(
            display: *mut Display,
            detect: Bool,
            supported: *mut Bool,
        ) -> Bool;
        pub fn XKeysymToKeycode(display: *mut Display, keysym: KeySym) -> KeyCode;
        pub fn XGetModifierMapping(display: *mut Display) -> *mut XModifierKeymap;
        pub fn XFreeModifiermap(map: *mut XModifierKeymap) -> c_int;
        pub fn XResourceManagerString(display: *mut Display) -> *mut c_char;
        pub fn XFree(data: *mut c_void) -> c_int;
        pub fn XSetErrorHandler(handler: XErrorHandler) -> XErrorHandler;
    }

    pub const POLLIN: c_short = 0x0001;
    pub const EINTR: c_int = 4;
    pub const F_GETFL: c_int = 3;
    pub const F_SETFL: c_int = 4;
    pub const O_NONBLOCK: c_int = 0x800;

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct PollFd {
        pub fd: c_int,
        pub events: c_short,
        pub revents: c_short,
    }

    #[link(name = "c")]
    unsafe extern "C" {
        pub fn poll(fds: *mut PollFd, nfds: c_ulong, timeout: c_int) -> c_int;
        pub fn pipe(fds: *mut c_int) -> c_int;
        pub fn read(fd: c_int, buf: *mut c_void, count: usize) -> isize;
        pub fn write(fd: c_int, buf: *const c_void, count: usize) -> isize;
        pub fn close(fd: c_int) -> c_int;
        pub fn fcntl(fd: c_int, cmd: c_int, ...) -> c_int;
        pub fn __errno_location() -> *mut c_int;
    }
}

use ffi::*;

const _: () = assert!(std::mem::size_of::<XEvent>() == 24 * std::mem::size_of::<c_long>());

// }}}
// {{{ Error handling and small helpers

static X_ERROR_COUNT: AtomicUsize = AtomicUsize::new(0);
static INSTALL_ERROR_HANDLER: Once = Once::new();

unsafe extern "C" fn ignore_x_error(_display: *mut Display, _event: *mut XErrorEvent) -> c_int {
    X_ERROR_COUNT.fetch_add(1, Ordering::Relaxed);
    0
}

fn install_error_handler() {
    INSTALL_ERROR_HANDLER.call_once(|| unsafe {
        XSetErrorHandler(Some(ignore_x_error));
    });
}

/// Number of X11 protocol errors observed so far. Mostly useful in tests.
pub fn x_error_count() -> usize {
    X_ERROR_COUNT.load(Ordering::Relaxed)
}

fn errno() -> c_int {
    unsafe { *__errno_location() }
}

fn xerr(message: impl Into<String>) -> WindowError {
    WindowError::new(message)
}

/// Check whether an X display can be opened, without creating a window.
pub fn probe_display(name: &str) -> bool {
    let Ok(name) = CString::new(name) else {
        return false;
    };
    install_error_handler();
    let display = unsafe { XOpenDisplay(name.as_ptr()) };
    if display.is_null() {
        return false;
    }
    unsafe {
        XCloseDisplay(display);
    }
    true
}

unsafe fn atom_name(display: *mut Display, atom: Atom) -> Option<String> {
    if atom == NONE {
        return None;
    }
    let name = unsafe { XGetAtomName(display, atom) };
    if name.is_null() {
        return None;
    }
    let text = unsafe { CStr::from_ptr(name) }
        .to_string_lossy()
        .into_owned();
    unsafe {
        XFree(name.cast());
    }
    Some(text)
}

unsafe fn get_property(
    display: *mut Display,
    window: Window,
    property: Atom,
) -> Option<(Atom, c_int, Vec<u8>)> {
    let mut actual_type: Atom = NONE;
    let mut actual_format: c_int = 0;
    let mut nitems: c_ulong = 0;
    let mut bytes_after: c_ulong = 0;
    let mut prop: *mut c_uchar = ptr::null_mut();
    let status = unsafe {
        XGetWindowProperty(
            display,
            window,
            property,
            0,
            0x1fff_ffff,
            1,
            ANY_PROPERTY_TYPE,
            &mut actual_type,
            &mut actual_format,
            &mut nitems,
            &mut bytes_after,
            &mut prop,
        )
    };
    if status != 0 || prop.is_null() || actual_type == NONE {
        if !prop.is_null() {
            unsafe {
                XFree(prop.cast());
            }
        }
        return None;
    }
    let item_size = match actual_format {
        8 => 1,
        16 => 2,
        32 => std::mem::size_of::<c_long>(),
        _ => 0,
    };
    let len = (nitems as usize).saturating_mul(item_size);
    let data = unsafe { std::slice::from_raw_parts(prop, len) }.to_vec();
    unsafe {
        XFree(prop.cast());
    }
    Some((actual_type, actual_format, data))
}

// }}}
// {{{ Window state

#[derive(Clone, Debug)]
struct Atoms {
    wm_delete_window: Atom,
    net_wm_name: Atom,
    utf8_string: Atom,
    clipboard: Atom,
    primary: Atom,
    targets: Atom,
    net_wm_state: Atom,
    net_wm_state_fullscreen: Atom,
    net_wm_state_maximized_vert: Atom,
    net_wm_state_maximized_horz: Atom,
    net_supporting_wm_check: Atom,
}

impl Atoms {
    unsafe fn intern(display: *mut Display) -> Self {
        let intern = |name: &str| {
            let cname = CString::new(name).expect("atom names contain no NUL");
            unsafe { XInternAtom(display, cname.as_ptr(), 0) }
        };
        Self {
            wm_delete_window: intern("WM_DELETE_WINDOW"),
            net_wm_name: intern("_NET_WM_NAME"),
            utf8_string: intern("UTF8_STRING"),
            clipboard: intern("CLIPBOARD"),
            primary: intern("PRIMARY"),
            targets: intern("TARGETS"),
            net_wm_state: intern("_NET_WM_STATE"),
            net_wm_state_fullscreen: intern("_NET_WM_STATE_FULLSCREEN"),
            net_wm_state_maximized_vert: intern("_NET_WM_STATE_MAXIMIZED_VERT"),
            net_wm_state_maximized_horz: intern("_NET_WM_STATE_MAXIMIZED_HORZ"),
            net_supporting_wm_check: intern("_NET_SUPPORTING_WM_CHECK"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ModCategory {
    Shift,
    Control,
    Alt,
    Super,
    AltGr,
}

fn category_index(category: ModCategory) -> usize {
    match category {
        ModCategory::Shift => 0,
        ModCategory::Control => 1,
        ModCategory::Alt => 2,
        ModCategory::Super => 3,
        ModCategory::AltGr => 4,
    }
}

fn modifier_category(sym: u32) -> Option<ModCategory> {
    match sym {
        keysym::SHIFT_L | keysym::SHIFT_R => Some(ModCategory::Shift),
        keysym::CONTROL_L | keysym::CONTROL_R => Some(ModCategory::Control),
        keysym::ALT_L | keysym::ALT_R => Some(ModCategory::Alt),
        keysym::SUPER_L | keysym::SUPER_R | keysym::HYPER_L | keysym::HYPER_R => {
            Some(ModCategory::Super)
        }
        keysym::ISO_LEVEL3_SHIFT => Some(ModCategory::AltGr),
        _ => None,
    }
}

/// A raw X11 top-level window and its event loop.
///
/// The window is mapped by [`X11Window::open`] unless `WindowConfig::visible`
/// is false. All calls must happen on the thread that opened the window; use a
/// [`WakeupHandle`] to wake a blocked loop from another thread.
pub struct X11Window {
    display: *mut Display,
    screen: c_int,
    root: Window,
    window: Window,
    atoms: Atoms,
    wakeup: Arc<WakeupPipe>,
    inbox: VecDeque<WindowEvent>,
    clipboard: ClipboardState,
    clipboard_property_seq: u64,
    pressed_keys: HashSet<u8>,
    pressed_modifier_keys: [HashSet<u8>; 5],
    mod_masks: ModifierMasks,
    detectable_autorepeat: bool,
    width: u32,
    height: u32,
    x: i32,
    y: i32,
    title: String,
    scale_factor: f64,
    min_width: u32,
    min_height: u32,
    max_width: Option<u32>,
    max_height: Option<u32>,
    cursor: c_ulong,
    cursor_shape: PointerShape,
    should_close: bool,
    mapped: bool,
    fullscreen: bool,
    maximized: bool,
    normal_geometry: Option<(i32, i32, u32, u32)>,
}

impl X11Window {
    /// Open a window on the display named by `$DISPLAY`.
    pub fn new(config: &WindowConfig) -> Result<Self, WindowError> {
        Self::open(config, None)
    }

    /// Open a window on an explicit display name, or `$DISPLAY` when `None`.
    pub fn open(config: &WindowConfig, display_name: Option<&str>) -> Result<Self, WindowError> {
        install_error_handler();
        let display_c = match display_name {
            Some(name) => {
                Some(CString::new(name).map_err(|_| xerr("display name contains a NUL byte"))?)
            }
            None => None,
        };
        let display =
            unsafe { XOpenDisplay(display_c.as_ref().map_or(ptr::null(), |name| name.as_ptr())) };
        if display.is_null() {
            let name = display_name.unwrap_or("<from DISPLAY>");
            return Err(xerr(format!("failed to open X11 display {name}")));
        }
        let result = Self::create(display, config);
        if result.is_err() {
            unsafe {
                XCloseDisplay(display);
            }
        }
        result
    }

    fn create(display: *mut Display, config: &WindowConfig) -> Result<Self, WindowError> {
        let title = CString::new(config.title.as_str())
            .map_err(|_| xerr("window title contains a NUL byte"))?;
        let wakeup = Arc::new(WakeupPipe::new()?);
        let screen = unsafe { XDefaultScreen(display) };
        let root = unsafe { XRootWindow(display, screen) };
        let atoms = unsafe { Atoms::intern(display) };

        let mod_masks = unsafe { query_modifier_masks(display) };
        let mut supported: Bool = 0;
        unsafe {
            XkbSetDetectableAutoRepeat(display, 1, &mut supported);
        }

        let (x, y) = config.position.unwrap_or((0, 0));
        let max_width = if config.resizable {
            config.max_width
        } else {
            Some(config.width.max(1))
        };
        let max_height = if config.resizable {
            config.max_height
        } else {
            Some(config.height.max(1))
        };
        let window = unsafe {
            XCreateSimpleWindow(
                display,
                root,
                x,
                y,
                config.width.max(1),
                config.height.max(1),
                0,
                0,
                0,
            )
        };
        if window == NONE {
            return Err(xerr("XCreateSimpleWindow failed"));
        }

        let mut this = Self {
            display,
            screen,
            root,
            window,
            atoms,
            wakeup,
            inbox: VecDeque::new(),
            clipboard: ClipboardState::default(),
            clipboard_property_seq: 0,
            pressed_keys: HashSet::new(),
            pressed_modifier_keys: std::array::from_fn(|_| HashSet::new()),
            mod_masks,
            detectable_autorepeat: supported != 0,
            width: config.width.max(1),
            height: config.height.max(1),
            x,
            y,
            title: config.title.clone(),
            scale_factor: unsafe { query_scale_factor(display) },
            min_width: config.min_width.max(1),
            min_height: config.min_height.max(1),
            max_width,
            max_height,
            cursor: 0,
            cursor_shape: PointerShape::Default,
            should_close: false,
            mapped: false,
            fullscreen: false,
            maximized: false,
            normal_geometry: None,
        };

        unsafe {
            XStoreName(display, window, title.as_ptr());
            let name_bytes = config.title.as_bytes();
            XChangeProperty(
                display,
                window,
                this.atoms.net_wm_name,
                this.atoms.utf8_string,
                8,
                PROP_MODE_REPLACE,
                name_bytes.as_ptr(),
                name_bytes.len().min(c_int::MAX as usize) as c_int,
            );

            let mut protocols = [this.atoms.wm_delete_window];
            XSetWMProtocols(display, window, protocols.as_mut_ptr(), 1);
            this.apply_size_hints();

            let mask = KEY_PRESS_MASK
                | KEY_RELEASE_MASK
                | BUTTON_PRESS_MASK
                | BUTTON_RELEASE_MASK
                | POINTER_MOTION_MASK
                | ENTER_WINDOW_MASK
                | LEAVE_WINDOW_MASK
                | EXPOSURE_MASK
                | STRUCTURE_NOTIFY_MASK
                | FOCUS_CHANGE_MASK
                | PROPERTY_CHANGE_MASK;
            XSelectInput(display, window, mask);

            if config.visible {
                XMapWindow(display, window);
                this.mapped = true;
            }
            XSync(display, 0);
        }

        // Record the geometry the server actually assigned. This prevents the
        // initial ConfigureNotify from being reported as a resize.
        unsafe {
            let mut attributes: XWindowAttributes = std::mem::zeroed();
            if XGetWindowAttributes(display, window, &mut attributes) != 0 {
                if attributes.width > 0 {
                    this.width = attributes.width as u32;
                }
                if attributes.height > 0 {
                    this.height = attributes.height as u32;
                }
                this.x = attributes.x;
                this.y = attributes.y;
            }
        }
        Ok(this)
    }

    fn apply_size_hints(&self) {
        let (width, height) = self.size();
        let mut hints: XSizeHints = unsafe { std::mem::zeroed() };
        hints.flags = P_SIZE | P_MIN_SIZE;
        hints.width = width as c_int;
        hints.height = height as c_int;
        hints.min_width = self.min_width as c_int;
        hints.min_height = self.min_height as c_int;
        if let Some(max_width) = self.max_width {
            hints.flags |= P_MAX_SIZE;
            hints.max_width = max_width as c_int;
        }
        if let Some(max_height) = self.max_height {
            hints.flags |= P_MAX_SIZE;
            hints.max_height = max_height as c_int;
        }
        unsafe {
            XSetWMNormalHints(self.display, self.window, &mut hints);
        }
    }

    /// Native display pointer, suitable for `eglGetPlatformDisplay`.
    pub fn display_ptr(&self) -> *mut c_void {
        self.display.cast()
    }

    /// Native X11 window id.
    pub fn window_id(&self) -> u64 {
        self.window
    }

    pub fn root_window_id(&self) -> u64 {
        self.root
    }

    pub fn scale_factor(&self) -> f64 {
        self.scale_factor
    }

    pub fn detectable_autorepeat(&self) -> bool {
        self.detectable_autorepeat
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    /// Query the WM_NAME property, useful for integration tests.
    pub fn server_title(&mut self) -> Option<String> {
        let mut name: *mut c_char = ptr::null_mut();
        let status = unsafe { XFetchName(self.display, self.window, &mut name) };
        if status == 0 || name.is_null() {
            return None;
        }
        let text = unsafe { CStr::from_ptr(name) }
            .to_string_lossy()
            .into_owned();
        unsafe {
            XFree(name.cast());
        }
        Some(text)
    }

    pub fn set_title(&mut self, title: &str) -> Result<(), WindowError> {
        let ctitle = CString::new(title).map_err(|_| xerr("title contains a NUL byte"))?;
        unsafe {
            XStoreName(self.display, self.window, ctitle.as_ptr());
            XChangeProperty(
                self.display,
                self.window,
                self.atoms.net_wm_name,
                self.atoms.utf8_string,
                8,
                PROP_MODE_REPLACE,
                title.as_bytes().as_ptr(),
                title.len().min(c_int::MAX as usize) as c_int,
            );
            XFlush(self.display);
        }
        self.title = title.to_owned();
        Ok(())
    }

    pub fn size(&self) -> (u32, u32) {
        let mut attributes: XWindowAttributes = unsafe { std::mem::zeroed() };
        let ok = unsafe { XGetWindowAttributes(self.display, self.window, &mut attributes) } != 0;
        if ok && attributes.width > 0 && attributes.height > 0 {
            (attributes.width as u32, attributes.height as u32)
        } else {
            (self.width, self.height)
        }
    }

    pub fn position(&self) -> (i32, i32) {
        let mut attributes: XWindowAttributes = unsafe { std::mem::zeroed() };
        let ok = unsafe { XGetWindowAttributes(self.display, self.window, &mut attributes) } != 0;
        if ok {
            (attributes.x, attributes.y)
        } else {
            (self.x, self.y)
        }
    }

    pub fn screen_size(&self) -> (u32, u32) {
        unsafe {
            (
                XDisplayWidth(self.display, self.screen).max(1) as u32,
                XDisplayHeight(self.display, self.screen).max(1) as u32,
            )
        }
    }

    pub fn set_size(&mut self, width: u32, height: u32) {
        let width = width.max(1);
        let height = height.max(1);
        unsafe {
            XResizeWindow(self.display, self.window, width, height);
            XSync(self.display, 0);
        }
        // Leave the cached geometry alone until the resulting ConfigureNotify
        // is processed, so the resize is reported to the caller as an event.
    }

    pub fn set_position(&mut self, x: i32, y: i32) {
        unsafe {
            XMoveWindow(self.display, self.window, x, y);
            XSync(self.display, 0);
        }
        // Keep the cached position until ConfigureNotify arrives.
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
        self.apply_size_hints();
        unsafe {
            XFlush(self.display);
        }
    }

    pub fn min_size(&self) -> (u32, u32) {
        (self.min_width, self.min_height)
    }

    pub fn show(&mut self) {
        if !self.mapped {
            unsafe {
                XMapWindow(self.display, self.window);
                XSync(self.display, 0);
            }
            self.mapped = true;
        }
    }

    pub fn hide(&mut self) {
        if self.mapped {
            unsafe {
                XUnmapWindow(self.display, self.window);
                XSync(self.display, 0);
            }
            self.mapped = false;
        }
    }

    pub fn is_visible(&self) -> bool {
        let mut attributes: XWindowAttributes = unsafe { std::mem::zeroed() };
        let ok = unsafe { XGetWindowAttributes(self.display, self.window, &mut attributes) } != 0;
        ok && attributes.map_state == IS_VIEWABLE
    }

    pub fn focus(&mut self) {
        unsafe {
            XSetInputFocus(self.display, self.window, REVERT_TO_PARENT, CURRENT_TIME);
            XFlush(self.display);
        }
    }

    pub fn raise(&mut self) {
        unsafe {
            XRaiseWindow(self.display, self.window);
            XFlush(self.display);
        }
    }

    pub fn set_pointer_shape(&mut self, shape: PointerShape) {
        if shape == self.cursor_shape && self.cursor != 0 {
            return;
        }
        let xc = match shape {
            PointerShape::Default => 68,                          // XC_left_ptr
            PointerShape::Text => 152,                            // XC_xterm
            PointerShape::Pointer => 60,                          // XC_hand2
            PointerShape::Crosshair => 34,                        // XC_crosshair
            PointerShape::Move => 52,                             // XC_fleur
            PointerShape::Wait | PointerShape::Progress => 150,   // XC_watch
            PointerShape::ResizeN => 138,                         // XC_top_side
            PointerShape::ResizeS => 16,                          // XC_bottom_side
            PointerShape::ResizeE => 96,                          // XC_right_side
            PointerShape::ResizeW => 70,                          // XC_left_side
            PointerShape::ResizeNE => 136,                        // XC_top_right_corner
            PointerShape::ResizeNW => 134,                        // XC_top_left_corner
            PointerShape::ResizeSE => 14,                         // XC_bottom_right_corner
            PointerShape::ResizeSW => 12,                         // XC_bottom_left_corner
            PointerShape::NotAllowed => 0,                        // XC_X_cursor
            PointerShape::Help => 92,                             // XC_question_arrow
            PointerShape::Cell | PointerShape::ContextMenu => 40, // XC_dotbox
            PointerShape::VerticalText => 116,                    // XC_sb_v_double_arrow
            PointerShape::ZoomIn | PointerShape::ZoomOut => 34,
        };
        let cursor = unsafe { XCreateFontCursor(self.display, xc) };
        if cursor == 0 {
            return;
        }
        unsafe {
            XDefineCursor(self.display, self.window, cursor);
            if self.cursor != 0 {
                XFreeCursor(self.display, self.cursor);
            }
            XFlush(self.display);
        }
        self.cursor = cursor;
        self.cursor_shape = shape;
    }

    pub fn pointer_shape(&self) -> PointerShape {
        self.cursor_shape
    }

    pub fn bell(&mut self) {
        unsafe {
            XBell(self.display, 0);
            XFlush(self.display);
        }
    }

    // {{{ EWMH window state

    unsafe fn has_window_manager(&self) -> bool {
        get_property(self.display, self.root, self.atoms.net_supporting_wm_check).is_some()
    }

    fn send_wm_state(&mut self, action: c_long, first: Atom, second: Atom) {
        let mut event: XEvent = unsafe { std::mem::zeroed() };
        event.xclient = XClientMessageEvent {
            type_: CLIENT_MESSAGE,
            serial: 0,
            send_event: 1,
            display: self.display,
            window: self.window,
            message_type: self.atoms.net_wm_state,
            format: 32,
            data: [action, first as c_long, second as c_long, 1, 0],
        };
        unsafe {
            XSendEvent(
                self.display,
                self.root,
                0,
                SUBSTRUCTURE_REDIRECT_MASK | SUBSTRUCTURE_NOTIFY_MASK,
                &mut event,
            );
            XFlush(self.display);
        }
    }

    pub fn set_fullscreen(&mut self, fullscreen: bool) {
        if fullscreen == self.fullscreen {
            return;
        }
        let has_wm = unsafe { self.has_window_manager() };
        if fullscreen {
            let (width, height) = self.size();
            let (x, y) = self.position();
            self.normal_geometry = Some((x, y, width, height));
            self.send_wm_state(NET_WM_STATE_ADD, self.atoms.net_wm_state_fullscreen, NONE);
            if !has_wm {
                let (width, height) = self.screen_size();
                unsafe {
                    XMoveResizeWindow(self.display, self.window, 0, 0, width, height);
                    XRaiseWindow(self.display, self.window);
                }
            }
        } else {
            self.send_wm_state(
                NET_WM_STATE_REMOVE,
                self.atoms.net_wm_state_fullscreen,
                NONE,
            );
            if !has_wm {
                if let Some((x, y, width, height)) = self.normal_geometry.take() {
                    unsafe {
                        XMoveResizeWindow(self.display, self.window, x, y, width, height);
                    }
                }
            }
        }
        unsafe {
            XSync(self.display, 0);
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
        let has_wm = unsafe { self.has_window_manager() };
        if maximized {
            let (width, height) = self.size();
            let (x, y) = self.position();
            self.normal_geometry = Some((x, y, width, height));
            self.send_wm_state(
                NET_WM_STATE_ADD,
                self.atoms.net_wm_state_maximized_vert,
                self.atoms.net_wm_state_maximized_horz,
            );
            if !has_wm {
                let (width, height) = self.screen_size();
                unsafe {
                    XMoveResizeWindow(self.display, self.window, 0, 0, width, height);
                }
            }
        } else {
            self.send_wm_state(
                NET_WM_STATE_REMOVE,
                self.atoms.net_wm_state_maximized_vert,
                self.atoms.net_wm_state_maximized_horz,
            );
            if !has_wm {
                if let Some((x, y, width, height)) = self.normal_geometry.take() {
                    unsafe {
                        XMoveResizeWindow(self.display, self.window, x, y, width, height);
                    }
                }
            }
        }
        unsafe {
            XSync(self.display, 0);
        }
        self.maximized = maximized;
    }

    pub fn is_maximized(&self) -> bool {
        self.maximized
    }

    // }}}
    // {{{ Event pump

    pub fn wakeup_handle(&self) -> WakeupHandle {
        WakeupHandle::from_pipe(Arc::clone(&self.wakeup))
    }

    pub fn wakeup(&self) {
        self.wakeup.wake();
    }

    pub fn should_close(&self) -> bool {
        self.should_close
    }

    /// Ask the window to close at the next event-loop iteration.
    pub fn request_close(&mut self) {
        self.should_close = true;
    }

    /// Poll for events. `timeout_ms < 0` waits forever, `0` does not block.
    pub fn poll_events(&mut self, timeout_ms: i32) -> Vec<WindowEvent> {
        if self.inbox.is_empty() {
            self.pump(timeout_ms);
        }
        self.inbox.drain(..).collect()
    }

    /// Wait for at least one event (or wakeup).
    pub fn wait_events(&mut self) -> Vec<WindowEvent> {
        self.poll_events(-1)
    }

    fn pump(&mut self, timeout_ms: i32) {
        let start = Instant::now();
        let x_fd = unsafe { XConnectionNumber(self.display) };
        let mut woken = false;

        loop {
            if unsafe { XPending(self.display) } > 0 {
                break;
            }
            let remaining = if timeout_ms < 0 {
                -1
            } else {
                let elapsed = start.elapsed();
                let total = Duration::from_millis(timeout_ms.max(0) as u64);
                if elapsed >= total {
                    break;
                }
                let left = total - elapsed;
                left.as_millis().max(1).min(c_int::MAX as u128) as c_int
            };
            let mut fds = [
                PollFd {
                    fd: x_fd,
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
            if ready < 0 {
                if errno() == EINTR {
                    continue;
                }
                break;
            }
            if ready == 0 {
                break;
            }
            if fds[1].revents & POLLIN != 0 {
                woken |= self.wakeup.drain();
                if fds[0].revents == 0 {
                    break;
                }
            }
            if fds[0].revents != 0 {
                if unsafe { XPending(self.display) } > 0 {
                    break;
                }
                if timeout_ms == 0 {
                    break;
                }
                // The connection is readable but Xlib has not decoded an event
                // yet. Loop and ask again; the remaining timeout is recomputed.
                continue;
            }
            if timeout_ms == 0 {
                break;
            }
        }

        if woken {
            self.inbox.push_back(WindowEvent::Wakeup);
        }

        let mut events = Vec::new();
        while unsafe { XPending(self.display) } > 0 {
            let mut event: XEvent = unsafe { std::mem::zeroed() };
            unsafe {
                XNextEvent(self.display, &mut event);
            }
            self.process_event(&event, &mut events);
        }
        coalesce_events(&mut events);
        self.inbox.extend(events);
    }

    fn process_event(&mut self, event: &XEvent, out: &mut Vec<WindowEvent>) {
        let event_type = unsafe { event.type_ };
        match event_type {
            KEY_PRESS | KEY_RELEASE => {
                let key = unsafe { event.xkey };
                let input = self.translate_key(&key);
                out.push(WindowEvent::Key(input));
            }
            BUTTON_PRESS | BUTTON_RELEASE => {
                let button = unsafe { event.xbutton };
                self.translate_button(&button, out);
            }
            MOTION_NOTIFY => {
                let motion = unsafe { event.xmotion };
                out.push(WindowEvent::MouseMove {
                    x: motion.x as f64,
                    y: motion.y as f64,
                    modifiers: self.modifiers(motion.state),
                });
            }
            ENTER_NOTIFY | LEAVE_NOTIFY => {
                let crossing = unsafe { event.xcrossing };
                let modifiers = self.modifiers(crossing.state);
                let event = if event_type == ENTER_NOTIFY {
                    WindowEvent::Enter {
                        x: crossing.x as f64,
                        y: crossing.y as f64,
                        modifiers,
                    }
                } else {
                    WindowEvent::Leave {
                        x: crossing.x as f64,
                        y: crossing.y as f64,
                        modifiers,
                    }
                };
                out.push(event);
            }
            FOCUS_IN | FOCUS_OUT => {
                let focus = unsafe { event.xfocus };
                let focused = event_type == FOCUS_IN;
                if !focused {
                    self.pressed_keys.clear();
                    for set in &mut self.pressed_modifier_keys {
                        set.clear();
                    }
                }
                let _ = focus;
                out.push(WindowEvent::Focus { focused });
            }
            CONFIGURE_NOTIFY => {
                let configure = unsafe { event.xconfigure };
                let width = configure.width.max(0) as u32;
                let height = configure.height.max(0) as u32;
                if width > 0 && height > 0 && (width != self.width || height != self.height) {
                    self.width = width;
                    self.height = height;
                    out.push(WindowEvent::Resize { width, height });
                }
                if configure.x != self.x || configure.y != self.y {
                    self.x = configure.x;
                    self.y = configure.y;
                    out.push(WindowEvent::Move {
                        x: configure.x,
                        y: configure.y,
                    });
                }
            }
            EXPOSE => {
                let expose = unsafe { event.xexpose };
                if expose.width > 0 && expose.height > 0 {
                    out.push(WindowEvent::Expose {
                        x: expose.x,
                        y: expose.y,
                        width: expose.width as u32,
                        height: expose.height as u32,
                    });
                }
            }
            CLIENT_MESSAGE => {
                let client = unsafe { event.xclient };
                if client.format == 32 && client.data[0] as Atom == self.atoms.wm_delete_window {
                    self.should_close = true;
                    out.push(WindowEvent::CloseRequested);
                }
            }
            MAP_NOTIFY => {
                self.mapped = true;
            }
            UNMAP_NOTIFY => {
                self.mapped = false;
            }
            DESTROY_NOTIFY => {
                self.should_close = true;
                out.push(WindowEvent::CloseRequested);
            }
            SELECTION_CLEAR => {
                let clear = unsafe { event.xselectionclear };
                if let Some(selection) = self.selection_for_atom(clear.selection) {
                    self.clipboard.clear(selection);
                    out.push(WindowEvent::SelectionClear { selection });
                }
            }
            SELECTION_REQUEST => {
                let request = unsafe { event.xselectionrequest };
                if let Some(event) = self.handle_selection_request(&request) {
                    out.push(event);
                }
            }
            SELECTION_NOTIFY => {
                let notify = unsafe { event.xselection };
                if let Some(event) = self.handle_selection_notify(&notify) {
                    out.push(event);
                }
            }
            _ => {}
        }
    }

    fn modifiers(&self, state: u32) -> Modifiers {
        let mut modifiers = self.mod_masks.decode(state);
        if !self.pressed_modifier_keys[category_index(ModCategory::Shift)].is_empty() {
            modifiers.shift = true;
        }
        if !self.pressed_modifier_keys[category_index(ModCategory::Control)].is_empty() {
            modifiers.control = true;
        }
        if !self.pressed_modifier_keys[category_index(ModCategory::Alt)].is_empty() {
            modifiers.alt = true;
        }
        if !self.pressed_modifier_keys[category_index(ModCategory::Super)].is_empty() {
            modifiers.super_key = true;
        }
        if !self.pressed_modifier_keys[category_index(ModCategory::AltGr)].is_empty() {
            modifiers.alt_gr = true;
        }
        modifiers
    }

    fn translate_key(&mut self, event: &XKeyEvent) -> KeyInput {
        let keycode = event.keycode as u8;
        let action = if event.type_ == KEY_RELEASE {
            self.pressed_keys.remove(&keycode);
            KeyAction::Release
        } else if self.pressed_keys.insert(keycode) {
            KeyAction::Press
        } else {
            KeyAction::Repeat
        };

        let mut xkey = *event;
        let mut buffer = [0 as c_char; 64];
        let mut symbol: KeySym = 0;
        let count = unsafe {
            XLookupString(
                &mut xkey,
                buffer.as_mut_ptr(),
                buffer.len() as c_int,
                &mut symbol,
                ptr::null_mut(),
            )
        };
        let mut base_symbol = unsafe { XLookupKeysym(&mut xkey, 0) };
        let mut modifiers = self.modifiers(event.state);
        if symbol == 0 {
            let level = if modifiers.shift { 1 } else { 0 };
            symbol = unsafe { XkbKeycodeToKeysym(self.display, keycode, 0, level) };
        }
        if base_symbol == 0 {
            base_symbol = symbol;
        }

        let category =
            modifier_category(base_symbol as u32).or_else(|| modifier_category(symbol as u32));
        if let Some(category) = category {
            let set = &mut self.pressed_modifier_keys[category_index(category)];
            if action.is_release() {
                set.remove(&keycode);
            } else {
                set.insert(keycode);
            }
            let active = !set.is_empty();
            match category {
                ModCategory::Shift => modifiers.shift = active,
                ModCategory::Control => modifiers.control = active,
                ModCategory::Alt => modifiers.alt = active,
                ModCategory::Super => modifiers.super_key = active,
                ModCategory::AltGr => modifiers.alt_gr = active,
            }
        }

        let text = if action.is_release() {
            None
        } else {
            let bytes: Vec<u8> = buffer[..count.max(0) as usize]
                .iter()
                .map(|byte| *byte as u8)
                .collect();
            text_for_key(symbol as u32, &bytes)
        };

        KeyInput {
            keysym: symbol as u32,
            base_keysym: base_symbol as u32,
            keycode: event.keycode,
            text,
            modifiers,
            action,
        }
    }

    fn translate_button(&mut self, event: &XButtonEvent, out: &mut Vec<WindowEvent>) {
        let button = event.button.min(u8::MAX as c_uint) as u8;
        let pressed = event.type_ == BUTTON_PRESS;
        let modifiers = self.modifiers(event.state);
        let x = event.x as f64;
        let y = event.y as f64;
        if (4..=8).contains(&button) {
            // Wheel events are delivered as button presses by X11; releases are
            // ignored to avoid duplicate scroll events.
            if !pressed {
                return;
            }
            let (delta_x, delta_y) = match button {
                4 => (0.0, 1.0),
                5 => (0.0, -1.0),
                6 => (-1.0, 0.0),
                7 => (1.0, 0.0),
                _ => (0.0, 1.0),
            };
            out.push(WindowEvent::Scroll {
                x,
                y,
                delta_x,
                delta_y,
                button,
                modifiers,
            });
            return;
        }
        out.push(WindowEvent::MouseButton {
            button: MouseButton::from_x11_button(button),
            pressed,
            x,
            y,
            modifiers,
        });
    }

    // }}}
    // {{{ Clipboard

    fn selection_atom(&self, selection: Selection) -> Atom {
        match selection {
            Selection::Clipboard => self.atoms.clipboard,
            Selection::Primary => self.atoms.primary,
        }
    }

    fn selection_for_atom(&self, atom: Atom) -> Option<Selection> {
        if atom == self.atoms.clipboard {
            Some(Selection::Clipboard)
        } else if atom == self.atoms.primary {
            Some(Selection::Primary)
        } else {
            None
        }
    }

    /// Set a text offer on the requested selection.
    pub fn set_clipboard_text(
        &mut self,
        selection: Selection,
        text: &str,
    ) -> Result<(), WindowError> {
        self.clipboard.set_text(selection, text.as_bytes());
        let atom = self.selection_atom(selection);
        unsafe {
            XSetSelectionOwner(self.display, atom, self.window, CURRENT_TIME);
            XSync(self.display, 0);
            if XGetSelectionOwner(self.display, atom) != self.window {
                return Err(xerr("failed to become clipboard selection owner"));
            }
        }
        Ok(())
    }

    /// Set arbitrary MIME targets (all data is offered verbatim).
    pub fn set_clipboard_targets(
        &mut self,
        selection: Selection,
        targets: Vec<(String, Vec<u8>)>,
    ) -> Result<(), WindowError> {
        let targets = targets
            .into_iter()
            .map(|(name, data)| ClipboardTarget { name, data })
            .collect();
        self.clipboard.set_targets(selection, targets);
        let atom = self.selection_atom(selection);
        unsafe {
            XSetSelectionOwner(self.display, atom, self.window, CURRENT_TIME);
            XSync(self.display, 0);
            if XGetSelectionOwner(self.display, atom) != self.window {
                return Err(xerr("failed to become clipboard selection owner"));
            }
        }
        Ok(())
    }

    pub fn set_primary_text(&mut self, text: &str) -> Result<(), WindowError> {
        self.set_clipboard_text(Selection::Primary, text)
    }

    pub fn has_clipboard_offer(&self, selection: Selection) -> bool {
        self.clipboard.has_offer(selection)
    }

    /// Begin an async UTF-8 text read.
    pub fn request_clipboard_text(
        &mut self,
        selection: Selection,
    ) -> Result<ClipboardToken, WindowError> {
        self.begin_clipboard_request(selection, "UTF8_STRING")
    }

    /// Begin an async TARGETS read.
    pub fn request_clipboard_targets(
        &mut self,
        selection: Selection,
    ) -> Result<ClipboardToken, WindowError> {
        self.begin_clipboard_request(selection, "TARGETS")
    }

    fn begin_clipboard_request(
        &mut self,
        selection: Selection,
        target: &str,
    ) -> Result<ClipboardToken, WindowError> {
        if unsafe { XGetSelectionOwner(self.display, self.selection_atom(selection)) } == NONE {
            return Err(xerr("clipboard selection has no owner"));
        }
        let target_name =
            CString::new(target).map_err(|_| xerr("clipboard target contains a NUL byte"))?;
        let target_atom = unsafe { XInternAtom(self.display, target_name.as_ptr(), 0) };
        // The property name embeds a monotonically increasing token, so replies
        // can be correlated without a global map or shared connection.
        self.clipboard_property_seq = self.clipboard_property_seq.wrapping_add(1);
        let property_name = CString::new(format!(
            "KITTY_WINDOW_SELECTION_{}",
            self.clipboard_property_seq
        ))
        .expect("generated property name is valid");
        let property = unsafe { XInternAtom(self.display, property_name.as_ptr(), 0) };
        let token = self.clipboard.begin(selection, target, property as u64);
        unsafe {
            XConvertSelection(
                self.display,
                self.selection_atom(selection),
                target_atom,
                property,
                self.window,
                CURRENT_TIME,
            );
            XFlush(self.display);
        }
        Ok(token)
    }

    /// Collect a completed async clipboard result, if ready.
    pub fn take_clipboard_result(
        &mut self,
        token: ClipboardToken,
    ) -> Option<Result<ClipboardReply, ClipboardError>> {
        self.clipboard.take_result(token)
    }

    pub fn pending_clipboard_requests(&self) -> usize {
        self.clipboard.pending_count()
    }

    /// Read UTF-8 text, pumping this window's event loop until the reply
    /// arrives. If another process (or window) owns the selection, that owner
    /// must get a chance to service the request; in an in-process test pump the
    /// owner window too.
    pub fn clipboard_text_blocking(
        &mut self,
        selection: Selection,
        timeout: Duration,
    ) -> Result<String, ClipboardError> {
        let token = self
            .request_clipboard_text(selection)
            .map_err(|error| ClipboardError::BadReply(error.to_string()))?;
        let reply = self.wait_for_clipboard(token, timeout)?;
        match reply {
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
        let reply = self.wait_for_clipboard(token, timeout)?;
        match reply {
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
                return Err(ClipboardError::Timeout);
            }
            let remaining = deadline - now;
            let slice = remaining.as_millis().clamp(1, 50) as i32;
            self.pump(slice);
        }
    }

    fn handle_selection_request(
        &mut self,
        request: &XSelectionRequestEvent,
    ) -> Option<WindowEvent> {
        let selection = self.selection_for_atom(request.selection)?;
        let target = unsafe { atom_name(self.display, request.target) }.unwrap_or_default();
        let property = if request.property == NONE {
            request.target
        } else {
            request.property
        };
        let mut success = false;

        if request.target == self.atoms.targets {
            let names = self.clipboard.target_names(selection);
            let atoms: Vec<Atom> = names
                .iter()
                .map(|name| {
                    let cname = CString::new(name.as_str()).expect("target name has no NUL");
                    unsafe { XInternAtom(self.display, cname.as_ptr(), 0) }
                })
                .collect();
            unsafe {
                XChangeProperty(
                    self.display,
                    request.requestor,
                    property,
                    XA_ATOM,
                    32,
                    PROP_MODE_REPLACE,
                    atoms.as_ptr().cast(),
                    atoms.len() as c_int,
                );
            }
            success = true;
        } else if let Some(data) = self.clipboard.target_data(selection, &target) {
            let type_atom = if target == "UTF8_STRING" {
                self.atoms.utf8_string
            } else if target == "STRING" {
                XA_STRING
            } else {
                request.target
            };
            unsafe {
                XChangeProperty(
                    self.display,
                    request.requestor,
                    property,
                    type_atom,
                    8,
                    PROP_MODE_REPLACE,
                    data.as_ptr(),
                    data.len().min(c_int::MAX as usize) as c_int,
                );
            }
            success = true;
        }

        let mut notify: XEvent = unsafe { std::mem::zeroed() };
        notify.xselection = XSelectionEvent {
            type_: SELECTION_NOTIFY,
            serial: 0,
            send_event: 1,
            display: self.display,
            requestor: request.requestor,
            selection: request.selection,
            target: request.target,
            property: if success { property } else { NONE },
            time: request.time,
        };
        unsafe {
            XSendEvent(self.display, request.requestor, 0, 0, &mut notify);
            XFlush(self.display);
        }
        Some(WindowEvent::SelectionRequest { selection, target })
    }

    fn handle_selection_notify(&mut self, notify: &XSelectionEvent) -> Option<WindowEvent> {
        let selection = self.selection_for_atom(notify.selection)?;
        let target = unsafe { atom_name(self.display, notify.target) }.unwrap_or_default();
        let succeeded = notify.property != NONE;
        let reply = if succeeded {
            read_selection_property(self.display, self.window, notify.property)
        } else {
            Err(ClipboardError::ConversionFailed)
        };
        if notify.property != NONE {
            self.clipboard.complete(notify.property, reply);
        } else {
            let _ = self.clipboard.complete_matching(
                selection,
                &target,
                Err(ClipboardError::ConversionFailed),
            );
        }
        Some(WindowEvent::SelectionNotify {
            selection,
            target,
            succeeded,
        })
    }

    // }}}
    // {{{ Synthetic events for tests

    /// Inject a synthetic event through XSendEvent (or the corresponding Xlib
    /// request for geometry changes). This is intended for integration tests.
    pub fn inject_event(&mut self, event: &WindowEvent) -> Result<(), WindowError> {
        match event {
            WindowEvent::Key(key) => self.inject_key(key),
            WindowEvent::MouseMove { x, y, modifiers } => {
                let mut xevent: XEvent = unsafe { std::mem::zeroed() };
                xevent.xmotion = XMotionEvent {
                    type_: MOTION_NOTIFY,
                    serial: 0,
                    send_event: 1,
                    display: self.display,
                    window: self.window,
                    root: self.root,
                    subwindow: NONE,
                    time: CURRENT_TIME,
                    x: *x as c_int,
                    y: *y as c_int,
                    x_root: *x as c_int,
                    y_root: *y as c_int,
                    state: modifiers.raw_state,
                    is_hint: 0,
                    same_screen: 1,
                };
                self.send_event(xevent, POINTER_MOTION_MASK);
                Ok(())
            }
            WindowEvent::MouseButton {
                button,
                pressed,
                x,
                y,
                modifiers,
            } => {
                let button = button.to_x11_button();
                self.inject_pointer(button, *pressed, *x, *y, modifiers.raw_state);
                Ok(())
            }
            WindowEvent::Scroll {
                x,
                y,
                button,
                modifiers,
                ..
            } => {
                self.inject_pointer(*button, true, *x, *y, modifiers.raw_state);
                Ok(())
            }
            WindowEvent::Resize { width, height } => {
                self.set_size(*width, *height);
                Ok(())
            }
            WindowEvent::Move { x, y } => {
                self.set_position(*x, *y);
                Ok(())
            }
            WindowEvent::Expose {
                x,
                y,
                width,
                height,
            } => {
                let mut xevent: XEvent = unsafe { std::mem::zeroed() };
                xevent.xexpose = XExposeEvent {
                    type_: EXPOSE,
                    serial: 0,
                    send_event: 1,
                    display: self.display,
                    window: self.window,
                    x: *x,
                    y: *y,
                    width: *width as c_int,
                    height: *height as c_int,
                    count: 0,
                };
                self.send_event(xevent, EXPOSURE_MASK);
                Ok(())
            }
            WindowEvent::CloseRequested => {
                let mut xevent: XEvent = unsafe { std::mem::zeroed() };
                xevent.xclient = XClientMessageEvent {
                    type_: CLIENT_MESSAGE,
                    serial: 0,
                    send_event: 1,
                    display: self.display,
                    window: self.window,
                    message_type: self.atoms.wm_delete_window,
                    format: 32,
                    data: [self.atoms.wm_delete_window as c_long, 0, 0, 0, 0],
                };
                self.send_event(xevent, 0);
                Ok(())
            }
            WindowEvent::Focus { focused } => {
                if *focused {
                    self.focus();
                } else {
                    unsafe {
                        XSetInputFocus(self.display, NONE, REVERT_TO_PARENT, CURRENT_TIME);
                        XFlush(self.display);
                    }
                }
                Ok(())
            }
            WindowEvent::Wakeup => {
                self.wakeup();
                Ok(())
            }
            _ => Err(xerr("event type cannot be injected")),
        }
    }

    fn inject_key(&mut self, key: &KeyInput) -> Result<(), WindowError> {
        let keycode = unsafe { XKeysymToKeycode(self.display, key.keysym as KeySym) };
        if keycode == 0 {
            return Err(xerr(format!("no keycode for keysym 0x{:x}", key.keysym)));
        }
        let type_ = if key.action.is_release() {
            KEY_RELEASE
        } else {
            KEY_PRESS
        };
        let mut xevent: XEvent = unsafe { std::mem::zeroed() };
        xevent.xkey = XKeyEvent {
            type_,
            serial: 0,
            send_event: 1,
            display: self.display,
            window: self.window,
            root: self.root,
            subwindow: NONE,
            time: CURRENT_TIME,
            x: 1,
            y: 1,
            x_root: 1,
            y_root: 1,
            state: key.modifiers.raw_state,
            keycode: keycode as c_uint,
            same_screen: 1,
        };
        let mask = if type_ == KEY_PRESS {
            KEY_PRESS_MASK
        } else {
            KEY_RELEASE_MASK
        };
        self.send_event(xevent, mask);
        Ok(())
    }

    fn inject_pointer(&mut self, button: u8, pressed: bool, x: f64, y: f64, state: u32) {
        let mut xevent: XEvent = unsafe { std::mem::zeroed() };
        xevent.xbutton = XButtonEvent {
            type_: if pressed {
                BUTTON_PRESS
            } else {
                BUTTON_RELEASE
            },
            serial: 0,
            send_event: 1,
            display: self.display,
            window: self.window,
            root: self.root,
            subwindow: NONE,
            time: CURRENT_TIME,
            x: x as c_int,
            y: y as c_int,
            x_root: x as c_int,
            y_root: y as c_int,
            state,
            button: button as c_uint,
            same_screen: 1,
        };
        let mask = if pressed {
            BUTTON_PRESS_MASK
        } else {
            BUTTON_RELEASE_MASK
        };
        self.send_event(xevent, mask);
    }

    fn send_event(&self, mut event: XEvent, mask: c_long) {
        unsafe {
            XSendEvent(self.display, self.window, 0, mask, &mut event);
            XSync(self.display, 0);
        }
    }

    // }}}
}

impl Drop for X11Window {
    fn drop(&mut self) {
        unsafe {
            if self.cursor != 0 {
                XFreeCursor(self.display, self.cursor);
                self.cursor = 0;
            }
            XDestroyWindow(self.display, self.window);
            XSync(self.display, 0);
            XCloseDisplay(self.display);
        }
    }
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
            WindowEvent::Move { x, y } => match coalesced.last_mut() {
                Some(WindowEvent::Move {
                    x: previous_x,
                    y: previous_y,
                }) => {
                    *previous_x = x;
                    *previous_y = y;
                }
                _ => coalesced.push(WindowEvent::Move { x, y }),
            },
            WindowEvent::Expose {
                x,
                y,
                width,
                height,
            } => match coalesced.last_mut() {
                Some(WindowEvent::Expose {
                    x: previous_x,
                    y: previous_y,
                    width: previous_width,
                    height: previous_height,
                }) => {
                    let left = (*previous_x).min(x);
                    let top = (*previous_y).min(y);
                    let right = (*previous_x + *previous_width as i32).max(x + width as i32);
                    let bottom = (*previous_y + *previous_height as i32).max(y + height as i32);
                    *previous_x = left;
                    *previous_y = top;
                    *previous_width = (right - left).max(0) as u32;
                    *previous_height = (bottom - top).max(0) as u32;
                }
                _ => coalesced.push(WindowEvent::Expose {
                    x,
                    y,
                    width,
                    height,
                }),
            },
            other => coalesced.push(other),
        }
    }
    *events = coalesced;
}

unsafe fn query_modifier_masks(display: *mut Display) -> ModifierMasks {
    let mut masks = ModifierMasks::default();
    let map = unsafe { XGetModifierMapping(display) };
    if map.is_null() {
        return masks;
    }
    let max_keypermod = unsafe { (*map).max_keypermod.max(0) as usize };
    let entries = unsafe { (*map).modifiermap };
    if !entries.is_null() && max_keypermod > 0 {
        let alt_l = unsafe { XKeysymToKeycode(display, keysym::ALT_L as KeySym) };
        let alt_r = unsafe { XKeysymToKeycode(display, keysym::ALT_R as KeySym) };
        let super_l = unsafe { XKeysymToKeycode(display, keysym::SUPER_L as KeySym) };
        let super_r = unsafe { XKeysymToKeycode(display, keysym::SUPER_R as KeySym) };
        let num_lock = unsafe { XKeysymToKeycode(display, 0xff7f as KeySym) };
        let alt_gr = unsafe { XKeysymToKeycode(display, keysym::ISO_LEVEL3_SHIFT as KeySym) };
        for modifier in 0..8usize {
            let mask = 1u32 << modifier;
            for key in 0..max_keypermod {
                let keycode = unsafe { *entries.add(modifier * max_keypermod + key) };
                if keycode == 0 {
                    continue;
                }
                if keycode == alt_l || keycode == alt_r {
                    masks.alt = mask;
                }
                if keycode == super_l || keycode == super_r {
                    masks.super_key = mask;
                }
                if keycode == num_lock {
                    masks.num_lock = mask;
                }
                if keycode == alt_gr {
                    masks.alt_gr = mask;
                }
            }
        }
    }
    unsafe {
        XFreeModifiermap(map);
    }
    masks
}

unsafe fn query_scale_factor(display: *mut Display) -> f64 {
    let resources = unsafe { XResourceManagerString(display) };
    if resources.is_null() {
        return 1.0;
    }
    let text = unsafe { CStr::from_ptr(resources) }.to_string_lossy();
    for line in text.lines() {
        let mut parts = line.splitn(2, ':');
        let key = parts.next().unwrap_or("").trim();
        if key == "Xft.dpi" {
            if let Some(value) = parts.next() {
                if let Ok(dpi) = value.trim().parse::<f64>() {
                    if dpi > 0.0 {
                        return (dpi / 96.0).clamp(0.5, 8.0);
                    }
                }
            }
        }
    }
    1.0
}

fn text_for_key(symbol: u32, bytes: &[u8]) -> Option<String> {
    if let Some(ch) = keysym_char(symbol) {
        if !ch.is_control() {
            return Some(ch.to_string());
        }
    }
    if bytes.iter().any(|byte| *byte >= 0x20 && *byte != 0x7f) {
        if let Ok(text) = std::str::from_utf8(bytes) {
            if !text.chars().any(char::is_control) {
                return Some(text.to_owned());
            }
        }
    }
    None
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

fn read_selection_property(
    display: *mut Display,
    window: Window,
    property: Atom,
) -> Result<ClipboardReply, ClipboardError> {
    let Some((actual_type, format, data)) = (unsafe { get_property(display, window, property) })
    else {
        return Err(ClipboardError::BadReply(
            "selection property is missing".to_owned(),
        ));
    };
    if actual_type == XA_ATOM && format == 32 {
        let item_size = std::mem::size_of::<c_long>();
        let mut targets = Vec::with_capacity(data.len() / item_size);
        for chunk in data.chunks_exact(item_size) {
            let mut raw = [0u8; std::mem::size_of::<c_long>()];
            raw.copy_from_slice(chunk);
            let atom = c_long::from_ne_bytes(raw) as Atom;
            if let Some(name) = unsafe { atom_name(display, atom) } {
                targets.push(name);
            }
        }
        return Ok(ClipboardReply::Targets(targets));
    }
    match String::from_utf8(data.clone()) {
        Ok(text) => Ok(ClipboardReply::Text(text)),
        Err(_) => Ok(ClipboardReply::Data {
            target: unsafe { atom_name(display, actual_type) }.unwrap_or_default(),
            data,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xevent_union_has_xlib_size() {
        assert_eq!(
            std::mem::size_of::<XEvent>(),
            24 * std::mem::size_of::<c_long>()
        );
        assert_eq!(std::mem::size_of::<XKeyEvent>(), 96);
        assert_eq!(std::mem::size_of::<XButtonEvent>(), 96);
        assert_eq!(std::mem::size_of::<XConfigureEvent>(), 88);
        assert_eq!(std::mem::size_of::<XClientMessageEvent>(), 96);
        assert_eq!(std::mem::size_of::<XSelectionRequestEvent>(), 80);
    }

    #[test]
    fn keysym_to_text_handles_ascii_latin1_and_unicode() {
        assert_eq!(keysym_char(0x61), Some('a'));
        assert_eq!(keysym_char(0x20), Some(' '));
        assert_eq!(keysym_char(0xe9), Some('é'));
        assert_eq!(keysym_char(0x0100_20ac), Some('€'));
        assert_eq!(keysym_char(keysym::RETURN), None);
        assert_eq!(text_for_key(0x61, b"a"), Some("a".to_owned()));
        assert_eq!(text_for_key(keysym::RETURN, b"\r"), None);
    }

    #[test]
    fn modifier_categories_cover_left_and_right_keys() {
        assert_eq!(modifier_category(keysym::SHIFT_L), Some(ModCategory::Shift));
        assert_eq!(
            modifier_category(keysym::CONTROL_R),
            Some(ModCategory::Control)
        );
        assert_eq!(modifier_category(keysym::ALT_R), Some(ModCategory::Alt));
        assert_eq!(modifier_category(keysym::SUPER_L), Some(ModCategory::Super));
        assert_eq!(modifier_category(0x61), None);
    }

    #[test]
    fn coalescing_merges_adjacent_geometry_events() {
        let mut events = vec![
            WindowEvent::Resize {
                width: 10,
                height: 20,
            },
            WindowEvent::Resize {
                width: 30,
                height: 40,
            },
            WindowEvent::Expose {
                x: 0,
                y: 0,
                width: 10,
                height: 10,
            },
            WindowEvent::Expose {
                x: 5,
                y: 5,
                width: 10,
                height: 10,
            },
        ];
        coalesce_events(&mut events);
        assert_eq!(
            events,
            vec![
                WindowEvent::Resize {
                    width: 30,
                    height: 40
                },
                WindowEvent::Expose {
                    x: 0,
                    y: 0,
                    width: 15,
                    height: 15
                }
            ]
        );
    }
}
