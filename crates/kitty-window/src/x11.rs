// SPDX-License-Identifier: GPL-3.0-or-later
//! Raw Xlib backend: window creation, event pump, keyboard/mouse translation,
//! EWMH window state, cursor shapes and clipboard selections.
//!
//! This is an equivalent backend to the GLFW/X11 path kitty normally uses. It
//! deliberately uses raw FFI with no bindgen and no external crates.

use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::{
    c_char, c_int, c_long, c_short, c_uchar, c_uint, c_ulong, c_ushort, c_void, CStr, CString,
};
use std::ptr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Once};
use std::time::{Duration, Instant};

use crate::clipboard::{
    ClipboardError, ClipboardReply, ClipboardState, ClipboardTarget, ClipboardToken, IncrReadState,
    IncrWriteState, INCR_THRESHOLD,
};
use crate::cursor::CursorTheme;
use crate::dnd::{parse_uri_list, DragSession, DragSourceData, DragSourceState};
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
    #[allow(clippy::upper_case_acronyms)]
    pub type XIM = c_void;
    #[allow(clippy::upper_case_acronyms)]
    pub type XIC = c_void;
    pub type XIMStyle = c_ulong;

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
    pub const GENERIC_EVENT: c_int = 35;

    pub const PROPERTY_NEW_VALUE: c_int = 0;
    pub const PROPERTY_DELETE: c_int = 1;

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
    pub struct XPropertyEvent {
        pub type_: c_int,
        pub serial: c_ulong,
        pub send_event: Bool,
        pub display: *mut Display,
        pub window: Window,
        pub atom: Atom,
        pub time: Time,
        pub state: c_int,
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
    pub struct XComposeStatus {
        pub compose_ptr: *mut c_void,
        pub chars_matched: c_ulong,
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
        pub xproperty: XPropertyEvent,
        pub xgeneric: XGenericEventCookie,
        pub pad: [c_long; 24],
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct XGenericEventCookie {
        pub type_: c_int,
        pub serial: c_ulong,
        pub send_event: Bool,
        pub display: *mut Display,
        pub extension: c_int,
        pub evtype: c_int,
        pub cookie: c_uint,
        pub data: *mut c_void,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct XIAnyClassInfo {
        pub type_: c_int,
        pub sourceid: c_int,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct XITouchClassInfo {
        pub type_: c_int,
        pub sourceid: c_int,
        pub mode: c_int,
        pub num_touches: c_int,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct XIValuatorClassInfo {
        pub type_: c_int,
        pub sourceid: c_int,
        pub number: c_int,
        pub label: Atom,
        pub min: f64,
        pub max: f64,
        pub value: f64,
        pub resolution: c_int,
        pub mode: c_int,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct XIDeviceInfo {
        pub deviceid: c_int,
        pub name: *mut c_char,
        pub use_: c_int,
        pub attachment: c_int,
        pub enabled: Bool,
        pub num_classes: c_int,
        pub classes: *mut *mut XIAnyClassInfo,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct XIEventMask {
        pub deviceid: c_int,
        pub mask_len: c_int,
        pub mask: *mut c_uchar,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct XIButtonState {
        pub mask_len: c_int,
        pub mask: *mut c_uchar,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct XIValuatorState {
        pub mask_len: c_int,
        pub mask: *mut c_uchar,
        pub values: *mut f64,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct XIModifierState {
        pub base: c_int,
        pub latched: c_int,
        pub locked: c_int,
        pub effective: c_int,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct XIGroupState {
        pub base: c_int,
        pub latched: c_int,
        pub locked: c_int,
        pub effective: c_int,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct XIDeviceEvent {
        pub type_: c_int,
        pub serial: c_ulong,
        pub send_event: Bool,
        pub display: *mut Display,
        pub extension: c_int,
        pub evtype: c_int,
        pub time: Time,
        pub deviceid: c_int,
        pub sourceid: c_int,
        pub detail: c_int,
        pub root: Window,
        pub event: Window,
        pub child: Window,
        pub root_x: f64,
        pub root_y: f64,
        pub event_x: f64,
        pub event_y: f64,
        pub flags: c_int,
        pub buttons: XIButtonState,
        pub valuators: XIValuatorState,
        pub mods: XIModifierState,
        pub group: XIGroupState,
    }

    pub const XI_TOUCH_BEGIN: c_int = 18;
    pub const XI_TOUCH_UPDATE: c_int = 19;
    pub const XI_TOUCH_END: c_int = 20;
    pub const XI_ALL_DEVICES: c_int = 0;
    pub const XI_ALL_MASTER_DEVICES: c_int = 1;
    pub const XI_TOUCH_CLASS: c_int = 8;
    pub const XI_VALUATOR_CLASS: c_int = 2;
    pub const XI_TOUCH_BEGIN_MASK: u32 = 1 << XI_TOUCH_BEGIN;
    pub const XI_TOUCH_UPDATE_MASK: u32 = 1 << XI_TOUCH_UPDATE;
    pub const XI_TOUCH_END_MASK: u32 = 1 << XI_TOUCH_END;

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct XIMText {
        pub length: c_ushort,
        pub feedback: *mut c_ulong,
        pub encoding_is_wchar: Bool,
        pub string: XIMTextString,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub union XIMTextString {
        pub multi_byte: *mut c_char,
        pub wide_char: *mut u32,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct XIMPreeditDrawCallbackStruct {
        pub caret: c_int,
        pub chg_first: c_int,
        pub chg_length: c_int,
        pub text: *mut XIMText,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct XIMPreeditCaretCallbackStruct {
        pub position: c_int,
        pub direction: c_int,
        pub style: c_int,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct XIMStyles {
        pub count_styles: c_ushort,
        pub supported_styles: *mut XIMStyle,
    }

    pub type XIMProc = Option<unsafe extern "C" fn(*mut XIC, *mut c_void, *mut c_void)>;

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
        pub fn XQueryPointer(
            display: *mut Display,
            window: Window,
            root_return: *mut Window,
            child_return: *mut Window,
            root_x: *mut c_int,
            root_y: *mut c_int,
            win_x: *mut c_int,
            win_y: *mut c_int,
            mask_return: *mut c_uint,
        ) -> Bool;
        pub fn XQueryExtension(
            display: *mut Display,
            name: *const c_char,
            major_opcode: *mut c_int,
            first_event: *mut c_int,
            first_error: *mut c_int,
        ) -> Bool;
        pub fn XGetEventData(display: *mut Display, cookie: *mut XGenericEventCookie) -> Bool;
        pub fn XFreeEventData(display: *mut Display, cookie: *mut XGenericEventCookie);
        pub fn XTranslateCoordinates(
            display: *mut Display,
            src: Window,
            dest: Window,
            src_x: c_int,
            src_y: c_int,
            dest_x: *mut c_int,
            dest_y: *mut c_int,
            child: *mut Window,
        ) -> Bool;
        pub fn XFilterEvent(event: *mut XEvent, window: Window) -> Bool;
        pub fn XOpenIM(
            display: *mut Display,
            db: *mut c_void,
            res_name: *mut c_char,
            res_class: *mut c_char,
        ) -> *mut XIM;
        pub fn XCloseIM(im: *mut XIM) -> Status;
        pub fn XCreateIC(im: *mut XIM, ...) -> *mut XIC;
        pub fn XDestroyIC(ic: *mut XIC);
        pub fn XSetICFocus(ic: *mut XIC);
        pub fn XUnsetICFocus(ic: *mut XIC);
        pub fn XGetIMValues(im: *mut XIM, ...) -> *mut c_char;
        pub fn XGetICValues(ic: *mut XIC, ...) -> *mut c_char;
        pub fn XSetICValues(ic: *mut XIC, ...) -> *mut c_char;
    }

    #[link(name = "Xi")]
    unsafe extern "C" {
        pub fn XISelectEvents(
            display: *mut Display,
            window: Window,
            masks: *mut XIEventMask,
            num_masks: c_int,
        ) -> Status;
        pub fn XIQueryVersion(
            display: *mut Display,
            major: *mut c_int,
            minor: *mut c_int,
        ) -> Status;
        pub fn XIQueryDevice(
            display: *mut Display,
            deviceid: c_int,
            ndevices_return: *mut c_int,
        ) -> *mut XIDeviceInfo;
        pub fn XIFreeDeviceInfo(info: *mut XIDeviceInfo);
    }

    #[link(name = "dl")]
    unsafe extern "C" {
        pub fn dlopen(filename: *const c_char, flags: c_int) -> *mut c_void;
        pub fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
        pub fn dlclose(handle: *mut c_void) -> c_int;
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
    unsafe { get_property_with(display, window, property, 1) }
}

unsafe fn get_property_with(
    display: *mut Display,
    window: Window,
    property: Atom,
    delete: Bool,
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
            delete,
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
// {{{ Input methods, cursor themes and transfer state

const XIM_PREEDIT_CALLBACKS: XIMStyle = 1 << 1;
const XIM_PREEDIT_NOTHING: XIMStyle = 1 << 3;
const XIM_STATUS_NOTHING: XIMStyle = 1 << 7;
const XIM_STATUS_NONE: XIMStyle = 1 << 8;

const XN_QUERY_INPUT_STYLE: &[u8] = b"queryInputStyle\0";
const XN_INPUT_STYLE: &[u8] = b"inputStyle\0";
const XN_CLIENT_WINDOW: &[u8] = b"clientWindow\0";
const XN_FOCUS_WINDOW: &[u8] = b"focusWindow\0";
const XN_FILTER_EVENTS: &[u8] = b"filterEvents\0";
const XN_COMMIT: &[u8] = b"commit\0";
const XN_PREEDIT_START: &[u8] = b"preeditStart\0";
const XN_PREEDIT_DONE: &[u8] = b"preeditDone\0";
const XN_PREEDIT_DRAW: &[u8] = b"preeditDraw\0";
const XN_PREEDIT_CARET: &[u8] = b"preeditCaret\0";

fn xn(name: &'static [u8]) -> *const c_char {
    name.as_ptr().cast()
}

fn clamp_byte_boundary(text: &str, index: i32) -> usize {
    let mut index = index.max(0) as usize;
    if index > text.len() {
        index = text.len();
    }
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn char_index(text: &str, byte_index: usize) -> usize {
    text[..clamp_byte_boundary(text, byte_index as i32)]
        .chars()
        .count()
}

/// Pure pre-edit state machine used by the XIM callbacks.
///
/// Keeping the state independent of Xlib makes callback parsing testable on
/// machines where no input method is installed.
#[derive(Clone, Debug, Default)]
pub(crate) struct ImeState {
    text: String,
    cursor: Option<(usize, usize)>,
    events: VecDeque<WindowEvent>,
}

impl ImeState {
    fn emit_preedit(&mut self) {
        self.events.push_back(WindowEvent::Preedit {
            text: self.text.clone(),
            cursor: self.cursor,
        });
    }

    pub(crate) fn draw(
        &mut self,
        chg_first: i32,
        chg_length: i32,
        replacement: Option<&str>,
        caret: i32,
    ) {
        let start = clamp_byte_boundary(&self.text, chg_first);
        let end =
            clamp_byte_boundary(&self.text, chg_first.saturating_add(chg_length.max(0))).max(start);
        let replacement = replacement.unwrap_or("");
        self.text.replace_range(start..end, replacement);
        let caret = clamp_byte_boundary(&self.text, caret);
        let index = self.text[..caret].chars().count();
        self.cursor = Some((index, index));
        self.emit_preedit();
    }

    pub(crate) fn set_caret(&mut self, position: i32) {
        let index = char_index(&self.text, clamp_byte_boundary(&self.text, position));
        self.cursor = Some((index, index));
        self.emit_preedit();
    }

    pub(crate) fn commit(&mut self, text: &str) {
        if !text.is_empty() {
            if !self.text.is_empty() {
                self.text.clear();
                self.cursor = None;
                self.emit_preedit();
            }
            self.events.push_back(WindowEvent::Commit {
                text: text.to_owned(),
            });
        }
    }

    pub(crate) fn reset(&mut self) {
        let had_preedit = !self.text.is_empty() || self.cursor.is_some();
        self.text.clear();
        self.cursor = None;
        if had_preedit {
            self.emit_preedit();
        }
    }

    pub(crate) fn drain(&mut self) -> Vec<WindowEvent> {
        self.events.drain(..).collect()
    }

    #[cfg(test)]
    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    #[cfg(test)]
    pub(crate) fn cursor(&self) -> Option<(usize, usize)> {
        self.cursor
    }
}

unsafe fn decode_xim_text(text: *const XIMText) -> Option<String> {
    if text.is_null() {
        return None;
    }
    let text = unsafe { &*text };
    if text.length == 0 {
        return Some(String::new());
    }
    if text.encoding_is_wchar != 0 {
        let pointer = unsafe { text.string.wide_char };
        if pointer.is_null() {
            return None;
        }
        let units = unsafe { std::slice::from_raw_parts(pointer, text.length as usize) };
        Some(
            units
                .iter()
                .filter_map(|unit| char::from_u32(*unit))
                .collect(),
        )
    } else {
        let pointer = unsafe { text.string.multi_byte };
        if pointer.is_null() {
            return None;
        }
        let bytes =
            unsafe { std::slice::from_raw_parts(pointer.cast::<u8>(), text.length as usize) };
        Some(String::from_utf8_lossy(bytes).into_owned())
    }
}

struct ImeContext {
    im: *mut XIM,
    ic: *mut XIC,
    state: Arc<std::sync::Mutex<ImeState>>,
    callbacks: bool,
}

impl ImeContext {
    unsafe fn destroy(&mut self) {
        unsafe {
            if !self.ic.is_null() {
                XDestroyIC(self.ic);
                self.ic = ptr::null_mut();
            }
            if !self.im.is_null() {
                XCloseIM(self.im);
                self.im = ptr::null_mut();
            }
        }
    }
}

unsafe extern "C" fn ime_preedit_draw(
    _ic: *mut XIC,
    client_data: *mut c_void,
    call_data: *mut c_void,
) {
    if client_data.is_null() || call_data.is_null() {
        return;
    }
    let state = unsafe { &*(client_data as *const std::sync::Mutex<ImeState>) };
    let draw = unsafe { &*(call_data as *const XIMPreeditDrawCallbackStruct) };
    let replacement = unsafe { decode_xim_text(draw.text) };
    if let Ok(mut state) = state.lock() {
        state.draw(
            draw.chg_first,
            draw.chg_length,
            replacement.as_deref(),
            draw.caret,
        );
    }
}

unsafe extern "C" fn ime_preedit_caret(
    _ic: *mut XIC,
    client_data: *mut c_void,
    call_data: *mut c_void,
) {
    if client_data.is_null() || call_data.is_null() {
        return;
    }
    let state = unsafe { &*(client_data as *const std::sync::Mutex<ImeState>) };
    let caret = unsafe { &*(call_data as *const XIMPreeditCaretCallbackStruct) };
    if let Ok(mut state) = state.lock() {
        state.set_caret(caret.position);
    }
}

unsafe extern "C" fn ime_preedit_done(
    _ic: *mut XIC,
    client_data: *mut c_void,
    _call_data: *mut c_void,
) {
    if client_data.is_null() {
        return;
    }
    let state = unsafe { &*(client_data as *const std::sync::Mutex<ImeState>) };
    if let Ok(mut state) = state.lock() {
        state.reset();
    }
}

unsafe extern "C" fn ime_preedit_start(
    _ic: *mut XIC,
    _client_data: *mut c_void,
    _call_data: *mut c_void,
) {
}

unsafe extern "C" fn ime_commit(_ic: *mut XIC, client_data: *mut c_void, call_data: *mut c_void) {
    if client_data.is_null() {
        return;
    }
    let state = unsafe { &*(client_data as *const std::sync::Mutex<ImeState>) };
    let text = unsafe { decode_xim_text(call_data as *const XIMText) }.unwrap_or_default();
    if let Ok(mut state) = state.lock() {
        state.commit(&text);
    }
}

fn choose_input_style(styles: &[XIMStyle]) -> Option<(XIMStyle, bool)> {
    let callback_style = styles.iter().copied().find(|style| {
        style & XIM_PREEDIT_CALLBACKS != 0 && style & (XIM_STATUS_NOTHING | XIM_STATUS_NONE) != 0
    });
    if let Some(style) = callback_style {
        return Some((style, true));
    }
    if let Some(style) = styles
        .iter()
        .copied()
        .find(|style| style & XIM_PREEDIT_CALLBACKS != 0)
    {
        return Some((style, true));
    }
    styles
        .iter()
        .copied()
        .find(|style| style & XIM_PREEDIT_NOTHING != 0)
        .map(|style| (style, false))
        .or_else(|| styles.first().copied().map(|style| (style, false)))
}

unsafe fn open_input_context(
    display: *mut Display,
    window: Window,
) -> Option<(ImeContext, c_long)> {
    let im = unsafe { XOpenIM(display, ptr::null_mut(), ptr::null_mut(), ptr::null_mut()) };
    if im.is_null() {
        return None;
    }
    let mut styles: *mut XIMStyles = ptr::null_mut();
    unsafe {
        XGetIMValues(
            im,
            xn(XN_QUERY_INPUT_STYLE),
            &mut styles,
            ptr::null::<c_char>(),
        );
    }
    let supported: Vec<XIMStyle> = if styles.is_null() {
        Vec::new()
    } else {
        let count = unsafe { (*styles).count_styles as usize };
        let pointer = unsafe { (*styles).supported_styles };
        if pointer.is_null() {
            Vec::new()
        } else {
            unsafe { std::slice::from_raw_parts(pointer, count) }.to_vec()
        }
    };
    if !styles.is_null() {
        unsafe {
            XFree(styles.cast());
        }
    }
    let (style, callbacks) =
        choose_input_style(&supported).unwrap_or((XIM_PREEDIT_NOTHING | XIM_STATUS_NOTHING, false));
    let state = Arc::new(std::sync::Mutex::new(ImeState::default()));
    let client_data = Arc::as_ptr(&state) as *mut c_void;
    let ic = unsafe {
        XCreateIC(
            im,
            xn(XN_INPUT_STYLE),
            style,
            xn(XN_CLIENT_WINDOW),
            window,
            xn(XN_FOCUS_WINDOW),
            window,
            ptr::null::<c_char>(),
        )
    };
    if ic.is_null() {
        unsafe {
            XCloseIM(im);
        }
        return None;
    }
    if callbacks {
        unsafe {
            XSetICValues(
                ic,
                xn(XN_COMMIT),
                Some(ime_commit),
                client_data,
                xn(XN_PREEDIT_START),
                Some(ime_preedit_start),
                client_data,
                xn(XN_PREEDIT_DONE),
                Some(ime_preedit_done),
                client_data,
                xn(XN_PREEDIT_DRAW),
                Some(ime_preedit_draw),
                client_data,
                xn(XN_PREEDIT_CARET),
                Some(ime_preedit_caret),
                client_data,
                ptr::null::<c_char>(),
            );
        }
    }
    let mut filter_events: c_long = 0;
    unsafe {
        XGetICValues(
            ic,
            xn(XN_FILTER_EVENTS),
            &mut filter_events,
            ptr::null::<c_char>(),
        );
    }
    Some((
        ImeContext {
            im,
            ic,
            state,
            callbacks,
        },
        filter_events,
    ))
}

type XcursorLoadCursor = unsafe extern "C" fn(*mut Display, *const c_char) -> c_ulong;
type XcursorLoadImage =
    unsafe extern "C" fn(*const c_char, *const c_char, c_int) -> *mut XcursorImage;
type XcursorImageLoadCursor = unsafe extern "C" fn(*mut Display, *const XcursorImage) -> c_ulong;
type XcursorImageDestroy = unsafe extern "C" fn(*mut XcursorImage);
type XcursorGetTheme = unsafe extern "C" fn(*mut Display) -> *const c_char;
type XcursorGetDefaultSize = unsafe extern "C" fn(*mut Display) -> c_int;

#[repr(C)]
struct XcursorImage {
    version: c_uint,
    size: c_uint,
    width: c_uint,
    height: c_uint,
    xhot: c_uint,
    yhot: c_uint,
    delay: c_uint,
    pixels: *mut c_uint,
}

struct XcursorLoader {
    handle: *mut c_void,
    load_cursor: XcursorLoadCursor,
    load_image: Option<XcursorLoadImage>,
    image_load_cursor: Option<XcursorImageLoadCursor>,
    image_destroy: Option<XcursorImageDestroy>,
    get_theme: Option<XcursorGetTheme>,
    get_default_size: Option<XcursorGetDefaultSize>,
}

impl XcursorLoader {
    fn new() -> Option<Self> {
        const NAMES: [&[u8]; 3] = [
            b"libXcursor.so.1\0",
            b"libXcursor.so\0",
            b"libXcursor.so.1.0.2\0",
        ];
        for name in NAMES {
            let handle = unsafe { dlopen(name.as_ptr().cast(), 1) };
            if handle.is_null() {
                continue;
            }
            let symbol = unsafe { dlsym(handle, c"XcursorLibraryLoadCursor".as_ptr()) };
            if symbol.is_null() {
                unsafe {
                    dlclose(handle);
                }
                continue;
            }
            let load_cursor: XcursorLoadCursor = unsafe { std::mem::transmute(symbol) };
            let optional = |name: &CStr| unsafe { dlsym(handle, name.as_ptr()) };
            let load_image_symbol = optional(c"XcursorLibraryLoadImage");
            let image_load_symbol = optional(c"XcursorImageLoadCursor");
            let image_destroy_symbol = optional(c"XcursorImageDestroy");
            let get_theme_symbol = optional(c"XcursorGetTheme");
            let get_size_symbol = optional(c"XcursorGetDefaultSize");
            return Some(Self {
                handle,
                load_cursor,
                load_image: (!load_image_symbol.is_null())
                    .then(|| unsafe { std::mem::transmute(load_image_symbol) }),
                image_load_cursor: (!image_load_symbol.is_null())
                    .then(|| unsafe { std::mem::transmute(image_load_symbol) }),
                image_destroy: (!image_destroy_symbol.is_null())
                    .then(|| unsafe { std::mem::transmute(image_destroy_symbol) }),
                get_theme: (!get_theme_symbol.is_null())
                    .then(|| unsafe { std::mem::transmute(get_theme_symbol) }),
                get_default_size: (!get_size_symbol.is_null())
                    .then(|| unsafe { std::mem::transmute(get_size_symbol) }),
            });
        }
        None
    }

    fn display_theme(&self, display: *mut Display) -> Option<String> {
        let get_theme = self.get_theme?;
        let pointer = unsafe { get_theme(display) };
        if pointer.is_null() {
            return None;
        }
        Some(
            unsafe { CStr::from_ptr(pointer) }
                .to_string_lossy()
                .into_owned(),
        )
    }

    fn display_size(&self, display: *mut Display) -> Option<u32> {
        let get_size = self.get_default_size?;
        let size = unsafe { get_size(display) };
        (size > 0).then_some(size as u32)
    }

    fn load_sized(
        &self,
        display: *mut Display,
        name: &str,
        theme: Option<&str>,
        size: u32,
    ) -> c_ulong {
        let (Some(load_image), Some(image_load_cursor), Some(image_destroy)) =
            (self.load_image, self.image_load_cursor, self.image_destroy)
        else {
            return 0;
        };
        let Ok(name) = CString::new(name) else {
            return 0;
        };
        let theme = theme.and_then(|theme| CString::new(theme).ok());
        let image = unsafe {
            load_image(
                name.as_ptr(),
                theme.as_ref().map_or(ptr::null(), |theme| theme.as_ptr()),
                size.min(i32::MAX as u32) as c_int,
            )
        };
        if image.is_null() {
            return 0;
        }
        let cursor = unsafe { image_load_cursor(display, image) };
        unsafe {
            image_destroy(image);
        }
        cursor
    }

    fn load(&self, display: *mut Display, name: &str) -> c_ulong {
        let Ok(name) = CString::new(name) else {
            return 0;
        };
        unsafe { (self.load_cursor)(display, name.as_ptr()) }
    }
}

impl Drop for XcursorLoader {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe {
                dlclose(self.handle);
            }
            self.handle = ptr::null_mut();
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct XiPressureAxis {
    number: c_int,
    min: f64,
    max: f64,
}

/// Pressure axis lookup for an XInput2 touch device.
fn xi_pressure_axis(display: *mut Display, deviceid: c_int) -> Option<XiPressureAxis> {
    let mut count = 0;
    let devices = unsafe { XIQueryDevice(display, deviceid, &mut count) };
    if devices.is_null() {
        return None;
    }
    let mut axis = None;
    for index in 0..count.max(0) as usize {
        let info = unsafe { &*devices.add(index) };
        if info.deviceid != deviceid {
            continue;
        }
        for class_index in 0..info.num_classes.max(0) as usize {
            let class = unsafe { *info.classes.add(class_index) };
            if class.is_null() {
                continue;
            }
            let class = unsafe { &*class };
            if class.type_ != XI_VALUATOR_CLASS {
                continue;
            }
            let valuator =
                unsafe { &*(class as *const XIAnyClassInfo as *const XIValuatorClassInfo) };
            let name = unsafe { atom_name(display, valuator.label) }.unwrap_or_default();
            if name.eq_ignore_ascii_case("Abs Pressure") || name.eq_ignore_ascii_case("Pressure") {
                axis = Some(XiPressureAxis {
                    number: valuator.number,
                    min: valuator.min,
                    max: valuator.max,
                });
                break;
            }
        }
        if axis.is_some() {
            break;
        }
    }
    unsafe {
        XIFreeDeviceInfo(devices);
    }
    axis
}

/// Whether any XInput2 device advertises a touch class.
fn xi_has_touch_device(display: *mut Display) -> bool {
    let mut count = 0;
    let devices = unsafe { XIQueryDevice(display, XI_ALL_DEVICES, &mut count) };
    if devices.is_null() {
        return false;
    }
    let mut found = false;
    for index in 0..count.max(0) as usize {
        let info = unsafe { &*devices.add(index) };
        for class_index in 0..info.num_classes.max(0) as usize {
            let class = unsafe { *info.classes.add(class_index) };
            if !class.is_null() && unsafe { (*class).type_ } == XI_TOUCH_CLASS {
                found = true;
                break;
            }
        }
        if found {
            break;
        }
    }
    unsafe {
        XIFreeDeviceInfo(devices);
    }
    found
}

fn xi_touch_devices(display: *mut Display) -> Vec<c_int> {
    let mut count = 0;
    let devices = unsafe { XIQueryDevice(display, XI_ALL_DEVICES, &mut count) };
    if devices.is_null() {
        return Vec::new();
    }
    let mut found = Vec::new();
    for index in 0..count.max(0) as usize {
        let info = unsafe { &*devices.add(index) };
        let has_touch = (0..info.num_classes.max(0) as usize).any(|class_index| {
            let class = unsafe { *info.classes.add(class_index) };
            !class.is_null() && unsafe { (*class).type_ } == XI_TOUCH_CLASS
        });
        if has_touch && !found.contains(&info.deviceid) {
            found.push(info.deviceid);
        }
    }
    unsafe {
        XIFreeDeviceInfo(devices);
    }
    found
}

fn xi_select_touch(display: *mut Display, window: Window) {
    let mut mask = [0u8; 4];
    let bits = XI_TOUCH_BEGIN_MASK | XI_TOUCH_UPDATE_MASK | XI_TOUCH_END_MASK;
    mask[0] = bits as u8;
    mask[1] = (bits >> 8) as u8;
    mask[2] = (bits >> 16) as u8;
    mask[3] = (bits >> 24) as u8;
    for deviceid in [XI_ALL_MASTER_DEVICES, XI_ALL_DEVICES] {
        let mut event_mask = XIEventMask {
            deviceid,
            mask_len: mask.len() as c_int,
            mask: mask.as_mut_ptr(),
        };
        unsafe {
            XISelectEvents(display, window, &mut event_mask, 1);
        }
    }
}

fn xi_value_for_axis(event: &XIDeviceEvent, axis: c_int) -> Option<f64> {
    if axis < 0 || event.valuators.mask.is_null() || event.valuators.values.is_null() {
        return None;
    }
    let bit = axis as usize;
    if bit >= event.valuators.mask_len.max(0) as usize * 8 {
        return None;
    }
    let mask = unsafe {
        std::slice::from_raw_parts(event.valuators.mask, event.valuators.mask_len as usize)
    };
    let set = mask[bit / 8] & (1 << (bit % 8)) != 0;
    if !set {
        return None;
    }
    let value_index = (0..bit)
        .filter(|index| mask[index / 8] & (1 << (index % 8)) != 0)
        .count();
    Some(unsafe { *event.valuators.values.add(value_index) })
}

/// Translate an XInput2 touch event into the neutral touch representation.
fn xi_touch_event(
    event: &XIDeviceEvent,
    pressure_axis: Option<XiPressureAxis>,
) -> Option<WindowEvent> {
    let phase = match event.evtype {
        XI_TOUCH_BEGIN => crate::types::TouchPhase::Began,
        XI_TOUCH_UPDATE => crate::types::TouchPhase::Updated,
        XI_TOUCH_END => crate::types::TouchPhase::Ended,
        _ => return None,
    };
    let pressure = pressure_axis
        .and_then(|axis| xi_value_for_axis(event, axis.number).map(|value| (value, axis)))
        .map(|(value, axis)| {
            if axis.max > axis.min {
                (value - axis.min) / (axis.max - axis.min)
            } else if value > 1.0 {
                // Some devices omit the range and report raw 16-bit pressure.
                value / 65535.0
            } else {
                value
            }
            .clamp(0.0, 1.0)
        })
        .unwrap_or(if matches!(phase, crate::types::TouchPhase::Ended) {
            0.0
        } else {
            1.0
        });
    Some(WindowEvent::Touch {
        id: event.detail.max(0) as u64,
        x: event.event_x,
        y: event.event_y,
        pressure,
        phase,
    })
}

fn xcursor_name(shape: PointerShape) -> Option<&'static str> {
    Some(match shape {
        PointerShape::Default => "left_ptr",
        PointerShape::Text => "xterm",
        PointerShape::Pointer => "hand2",
        PointerShape::Crosshair => "crosshair",
        PointerShape::Move => "fleur",
        PointerShape::Wait => "watch",
        PointerShape::Progress => "left_ptr_watch",
        PointerShape::ResizeN => "top_side",
        PointerShape::ResizeS => "bottom_side",
        PointerShape::ResizeE => "right_side",
        PointerShape::ResizeW => "left_side",
        PointerShape::ResizeNE => "top_right_corner",
        PointerShape::ResizeNW => "top_left_corner",
        PointerShape::ResizeSE => "bottom_right_corner",
        PointerShape::ResizeSW => "bottom_left_corner",
        PointerShape::NotAllowed => "not-allowed",
        PointerShape::Help => "question_arrow",
        PointerShape::Cell => "dotbox",
        PointerShape::VerticalText => "sb_v_double_arrow",
        PointerShape::ZoomIn => "zoom-in",
        PointerShape::ZoomOut => "zoom-out",
        PointerShape::ContextMenu => "context-menu",
    })
}

#[derive(Clone, Debug)]
enum IncrPurpose {
    Clipboard {
        token: ClipboardToken,
        selection: Selection,
        target: String,
    },
    Drag {
        drop: PendingDragDrop,
    },
}

#[derive(Clone, Debug)]
struct IncrRead {
    property: Atom,
    state: IncrReadState,
    purpose: IncrPurpose,
    deadline: Instant,
}

#[derive(Clone, Debug)]
struct IncrWrite {
    requestor: Window,
    property: Atom,
    type_atom: Atom,
    state: IncrWriteState,
    deadline: Instant,
}

#[derive(Clone, Debug, Default)]
struct PendingDragDrop {
    x: f64,
    y: f64,
    target: String,
}

#[derive(Clone, Debug)]
struct DragState {
    session: DragSession,
    source: Window,
    version: i32,
    property: Atom,
    pending_drop: Option<PendingDragDrop>,
    deadline: Option<Instant>,
}

impl Default for DragState {
    fn default() -> Self {
        Self {
            session: DragSession::default(),
            source: NONE,
            version: 0,
            property: NONE,
            pending_drop: None,
            deadline: None,
        }
    }
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
    incr: Atom,
    xdnd_aware: Atom,
    xdnd_enter: Atom,
    xdnd_position: Atom,
    xdnd_status: Atom,
    xdnd_drop: Atom,
    xdnd_leave: Atom,
    xdnd_finished: Atom,
    xdnd_selection: Atom,
    xdnd_type_list: Atom,
    xdnd_action_copy: Atom,
    xdnd_action_none: Atom,
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
            incr: intern("INCR"),
            xdnd_aware: intern("XdndAware"),
            xdnd_enter: intern("XdndEnter"),
            xdnd_position: intern("XdndPosition"),
            xdnd_status: intern("XdndStatus"),
            xdnd_drop: intern("XdndDrop"),
            xdnd_leave: intern("XdndLeave"),
            xdnd_finished: intern("XdndFinished"),
            xdnd_selection: intern("XdndSelection"),
            xdnd_type_list: intern("XdndTypeList"),
            xdnd_action_copy: intern("XdndActionCopy"),
            xdnd_action_none: intern("XdndActionNone"),
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
    incr_reads: Vec<IncrRead>,
    incr_writes: Vec<IncrWrite>,
    drag: DragState,
    drag_source: DragSourceState,
    drag_source_data: DragSourceData,
    drag_source_target: Window,
    xi_available: bool,
    xi_opcode: c_int,
    xi_pressure_axes: HashMap<c_int, XiPressureAxis>,
    ime: Option<ImeContext>,
    ime_enabled: bool,
    focused: bool,
    cursor_loader: Option<XcursorLoader>,
    pressed_keys: HashSet<u8>,
    pressed_modifier_keys: [HashSet<u8>; 5],
    mod_masks: ModifierMasks,
    compose_status: XComposeStatus,
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
    cursor_theme: CursorTheme,
    cursor_scale: f64,
    cursor_theme_override: Option<String>,
    cursor_size_override: Option<u32>,
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
            incr_reads: Vec::new(),
            incr_writes: Vec::new(),
            drag: DragState::default(),
            drag_source: DragSourceState::default(),
            drag_source_data: DragSourceData::default(),
            drag_source_target: NONE,
            xi_available: false,
            xi_opcode: 0,
            xi_pressure_axes: HashMap::new(),
            ime: None,
            ime_enabled: true,
            focused: false,
            cursor_loader: XcursorLoader::new(),
            pressed_keys: HashSet::new(),
            pressed_modifier_keys: std::array::from_fn(|_| HashSet::new()),
            mod_masks,
            compose_status: unsafe { std::mem::zeroed() },
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
            cursor_theme: crate::cursor::snapshot(
                None,
                None,
                1.0,
                xcursor_name(PointerShape::Default).unwrap_or("left_ptr"),
            ),
            cursor_scale: 1.0,
            cursor_theme_override: None,
            cursor_size_override: None,
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

            let xdnd_version: c_long = 5;
            XChangeProperty(
                display,
                window,
                this.atoms.xdnd_aware,
                XA_ATOM,
                32,
                PROP_MODE_REPLACE,
                (&xdnd_version as *const c_long).cast(),
                1,
            );

            let mut filter_events = 0;
            if let Some((context, extra)) = open_input_context(display, window) {
                this.ime = Some(context);
                filter_events = extra;
            }

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
                | PROPERTY_CHANGE_MASK
                | filter_events;
            XSelectInput(display, window, mask);

            {
                let mut opcode = 0;
                let mut first_event = 0;
                let mut first_error = 0;
                if XQueryExtension(
                    display,
                    c"XInputExtension".as_ptr(),
                    &mut opcode,
                    &mut first_event,
                    &mut first_error,
                ) != 0
                {
                    let mut major = 2;
                    let mut minor = 2;
                    if XIQueryVersion(display, &mut major, &mut minor) == 0
                        && minor >= 2
                        && xi_has_touch_device(display)
                    {
                        xi_select_touch(display, window);
                        for device in xi_touch_devices(display) {
                            if let Some(axis) = xi_pressure_axis(display, device) {
                                this.xi_pressure_axes.insert(device, axis);
                            }
                        }
                        this.xi_available = true;
                        this.xi_opcode = opcode;
                    }
                }
            }

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

    /// Enable or disable input-method processing.
    ///
    /// Disabling releases the input context focus and clears any pre-edit text;
    /// key events are then delivered directly by the backend.
    pub fn set_ime_enabled(&mut self, enabled: bool) {
        if enabled == self.ime_enabled {
            return;
        }
        self.ime_enabled = enabled;
        if let Some(ime) = &self.ime {
            if !ime.ic.is_null() {
                unsafe {
                    if enabled && self.focused {
                        XSetICFocus(ime.ic);
                    } else {
                        XUnsetICFocus(ime.ic);
                    }
                }
            }
            if !enabled {
                if let Ok(mut state) = ime.state.lock() {
                    state.reset();
                }
            }
        }
        let mut events = Vec::new();
        self.drain_ime_events(&mut events);
        self.inbox.extend(events);
    }

    pub fn ime_enabled(&self) -> bool {
        self.ime_enabled
    }

    pub fn has_input_method(&self) -> bool {
        self.ime.as_ref().is_some_and(|ime| ime.callbacks)
    }

    /// Whether XInput2 touch events are available for this display.
    pub fn has_touch_input(&self) -> bool {
        self.xi_available
    }

    pub fn set_pointer_shape(&mut self, shape: PointerShape) {
        if shape == self.cursor_shape && self.cursor != 0 {
            return;
        }
        self.cursor_shape = shape;
        self.install_cursor(shape);
    }

    fn cursor_snapshot(&self, shape: PointerShape) -> CursorTheme {
        let name = xcursor_name(shape).unwrap_or("left_ptr");
        let loader_theme = self
            .cursor_loader
            .as_ref()
            .and_then(|loader| loader.display_theme(self.display));
        let loader_size = self
            .cursor_loader
            .as_ref()
            .and_then(|loader| loader.display_size(self.display));
        crate::cursor::snapshot(
            self.cursor_theme_override
                .as_deref()
                .or(loader_theme.as_deref()),
            self.cursor_size_override.or(loader_size),
            self.cursor_scale,
            name,
        )
    }

    fn install_cursor(&mut self, shape: PointerShape) {
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
        let name = xcursor_name(shape).unwrap_or("left_ptr");
        let theme = self.cursor_snapshot(shape);
        let scaled = crate::cursor::scaled_size(theme.size, theme.scale());
        let mut cursor = self
            .cursor_loader
            .as_ref()
            .map(|loader| loader.load_sized(self.display, name, Some(&theme.name), scaled))
            .unwrap_or(0);
        if cursor == 0 {
            cursor = self
                .cursor_loader
                .as_ref()
                .map(|loader| loader.load(self.display, name))
                .unwrap_or(0);
        }
        if cursor == 0 {
            cursor = unsafe { XCreateFontCursor(self.display, xc) };
        }
        if cursor == 0 {
            return;
        }
        self.cursor_theme = theme;
        unsafe {
            XDefineCursor(self.display, self.window, cursor);
            if self.cursor != 0 {
                XFreeCursor(self.display, self.cursor);
            }
            XFlush(self.display);
        }
        self.cursor = cursor;
    }

    /// Reload the current cursor from the theme, discarding any cached image.
    ///
    /// Returns `true` when a cursor could be installed. This is also used by
    /// [`X11Window::refresh_cursor_theme`] after a theme file changes.
    pub fn reload_cursor_theme(&mut self) -> bool {
        self.cursor_theme = self.cursor_snapshot(self.cursor_shape);
        self.install_cursor(self.cursor_shape);
        self.cursor != 0
    }

    /// Detect and apply a cursor-theme change since the previous check.
    ///
    /// Theme name, nominal size, per-window scale and the theme directory/file
    /// modification times all participate in the signature, so editing a theme
    /// in place reloads the cursor on the next event-pump iteration.
    pub fn refresh_cursor_theme(&mut self) -> bool {
        let theme = self.cursor_snapshot(self.cursor_shape);
        if theme.signature() == self.cursor_theme.signature() {
            return false;
        }
        self.cursor_theme = theme;
        self.install_cursor(self.cursor_shape);
        true
    }

    /// Set a per-window cursor scale. The scaled cursor is reloaded at once.
    pub fn set_cursor_scale(&mut self, scale: f64) {
        let scale = scale.clamp(0.5, 8.0);
        if (scale - self.cursor_scale).abs() < f64::EPSILON {
            return;
        }
        self.cursor_scale = scale;
        self.reload_cursor_theme();
    }

    pub fn cursor_scale(&self) -> f64 {
        self.cursor_scale
    }

    /// Override the theme name for this window. `None` restores the
    /// environment / display default.
    pub fn set_cursor_theme<'a>(&mut self, theme: impl Into<Option<&'a str>>) {
        self.cursor_theme_override = theme.into().map(str::to_owned);
        self.reload_cursor_theme();
    }

    /// Alias for callers that pass a theme name directly.
    pub fn set_cursor_theme_name(&mut self, theme: &str) {
        self.set_cursor_theme(theme);
    }

    pub fn cursor_theme_name(&self) -> &str {
        &self.cursor_theme.name
    }

    /// Override the nominal (unscaled) cursor size for this window.
    pub fn set_cursor_size(&mut self, size: impl Into<Option<u32>>) {
        self.cursor_size_override = size.into().map(|size| size.max(1));
        self.reload_cursor_theme();
    }

    pub fn cursor_size(&self) -> u32 {
        self.cursor_theme.size
    }

    pub fn pointer_shape(&self) -> PointerShape {
        self.cursor_shape
    }

    /// Start an XDND drag source with text and/or file paths.
    ///
    /// The destination is resolved from the current pointer position. Use
    /// [`X11Window::start_drag_to`] when the destination window is known.
    pub fn start_drag(&mut self, data: impl Into<DragSourceData>) -> Result<(), WindowError> {
        let data = data.into();
        let (target, root_x, root_y) = self
            .pointer_xdnd_target()
            .ok_or_else(|| xerr("no XDND-aware window under the pointer"))?;
        self.start_drag_to_at(&data, target, root_x, root_y)
    }

    /// Start a text drag source.
    pub fn start_text_drag(&mut self, text: &str) -> Result<(), WindowError> {
        self.start_drag(DragSourceData::from_text(text))
    }

    /// Start a file drag source.
    pub fn start_file_drag(&mut self, paths: &[String]) -> Result<(), WindowError> {
        self.start_drag(DragSourceData::from_paths(paths.iter().cloned()))
    }

    /// Start an XDND drag source at an explicit destination window and root
    /// coordinates. This is primarily useful for tests and for callers that
    /// track the drag target themselves.
    pub fn start_drag_to_at(
        &mut self,
        data: impl Into<DragSourceData>,
        target: Window,
        root_x: c_int,
        root_y: c_int,
    ) -> Result<(), WindowError> {
        let data = data.into();
        if data.is_empty() {
            return Err(xerr("drag source has no text or file payload"));
        }
        if self.drag_source.is_active() {
            return Err(xerr("a drag source is already active"));
        }
        if target == NONE || target == self.window {
            return Err(xerr("invalid XDND destination window"));
        }
        let targets = data.targets();
        if targets.is_empty() {
            return Err(xerr("drag source has no offered targets"));
        }
        let mut atoms = Vec::with_capacity(targets.len());
        for target in &targets {
            let target = CString::new(target.as_str())
                .map_err(|_| xerr("drag target contains a NUL byte"))?;
            atoms.push(unsafe { XInternAtom(self.display, target.as_ptr(), 0) });
        }
        unsafe {
            XSetSelectionOwner(
                self.display,
                self.atoms.xdnd_selection,
                self.window,
                CURRENT_TIME,
            );
            XChangeProperty(
                self.display,
                self.window,
                self.atoms.xdnd_type_list,
                XA_ATOM,
                32,
                PROP_MODE_REPLACE,
                atoms.as_ptr().cast(),
                atoms.len().min(c_int::MAX as usize) as c_int,
            );
        }
        self.drag_source_data = data;
        self.drag_source_target = target;
        self.drag_source.begin(targets.clone(), CURRENT_TIME as u32);
        self.send_drag_enter(target, &atoms);
        self.send_drag_position(target, root_x, root_y);
        self.inbox.push_back(WindowEvent::DragStarted { targets });
        Ok(())
    }

    /// Convenience wrapper around [`X11Window::start_drag_to_at`] using the
    /// pointer position as the initial destination coordinates.
    pub fn start_drag_to(
        &mut self,
        data: impl Into<DragSourceData>,
        target: Window,
    ) -> Result<(), WindowError> {
        let (root_x, root_y) = self.pointer_root_position().unwrap_or((0, 0));
        self.start_drag_to_at(data, target, root_x, root_y)
    }

    fn pointer_root_position(&self) -> Option<(c_int, c_int)> {
        let mut root_return = NONE;
        let mut child = NONE;
        let mut root_x = 0;
        let mut root_y = 0;
        let mut win_x = 0;
        let mut win_y = 0;
        let mut mask = 0;
        let ok = unsafe {
            XQueryPointer(
                self.display,
                self.root,
                &mut root_return,
                &mut child,
                &mut root_x,
                &mut root_y,
                &mut win_x,
                &mut win_y,
                &mut mask,
            )
        };
        (ok != 0).then_some((root_x, root_y))
    }

    fn pointer_xdnd_target(&self) -> Option<(Window, c_int, c_int)> {
        let (root_x, root_y) = self.pointer_root_position()?;
        let mut root_return = NONE;
        let mut child = NONE;
        let mut win_x = 0;
        let mut win_y = 0;
        let mut mask = 0;
        let ok = unsafe {
            XQueryPointer(
                self.display,
                self.root,
                &mut root_return,
                &mut child,
                &mut win_x,
                &mut win_y,
                &mut win_x,
                &mut win_y,
                &mut mask,
            )
        };
        if ok == 0 {
            return None;
        }
        let mut candidate = child;
        while candidate != NONE {
            if unsafe { get_property(self.display, candidate, self.atoms.xdnd_aware) }.is_some() {
                return Some((candidate, root_x, root_y));
            }
            let mut next = NONE;
            let mut next_root_x = 0;
            let mut next_root_y = 0;
            let mut next_win_x = 0;
            let mut next_win_y = 0;
            let ok = unsafe {
                XQueryPointer(
                    self.display,
                    candidate,
                    &mut root_return,
                    &mut next,
                    &mut next_root_x,
                    &mut next_root_y,
                    &mut next_win_x,
                    &mut next_win_y,
                    &mut mask,
                )
            };
            if ok == 0 || next == candidate {
                break;
            }
            candidate = next;
        }
        None
    }

    fn send_drag_enter(&self, target: Window, atoms: &[Atom]) {
        let mut event: XEvent = unsafe { std::mem::zeroed() };
        event.xclient = XClientMessageEvent {
            type_: CLIENT_MESSAGE,
            serial: 0,
            send_event: 1,
            display: self.display,
            window: target,
            message_type: self.atoms.xdnd_enter,
            format: 32,
            data: [
                self.window as c_long,
                (5i64 << 24) | i64::from(atoms.len() > 3),
                atoms.first().copied().unwrap_or(NONE) as c_long,
                atoms.get(1).copied().unwrap_or(NONE) as c_long,
                atoms.get(2).copied().unwrap_or(NONE) as c_long,
            ],
        };
        unsafe {
            XSendEvent(self.display, target, 0, 0, &mut event);
            XFlush(self.display);
        }
    }

    fn send_drag_position(&self, target: Window, root_x: c_int, root_y: c_int) {
        let packed = ((root_y & 0xffff) << 16) | (root_x & 0xffff);
        let mut event: XEvent = unsafe { std::mem::zeroed() };
        event.xclient = XClientMessageEvent {
            type_: CLIENT_MESSAGE,
            serial: 0,
            send_event: 1,
            display: self.display,
            window: target,
            message_type: self.atoms.xdnd_position,
            format: 32,
            data: [
                self.window as c_long,
                0,
                packed as c_long,
                CURRENT_TIME as c_long,
                self.atoms.xdnd_action_copy as c_long,
            ],
        };
        unsafe {
            XSendEvent(self.display, target, 0, 0, &mut event);
            XFlush(self.display);
        }
    }

    /// Drop the active drag source. The destination will request the selection
    /// data and later send `XdndFinished`.
    pub fn drag_drop(&mut self) -> Result<(), WindowError> {
        if !self.drag_source.is_active() {
            return Err(xerr("no active drag source"));
        }
        if !self.drag_source.mark_dropped() {
            return Err(xerr("the drag source has already been dropped"));
        }
        let target = self.drag_source_target;
        let mut event: XEvent = unsafe { std::mem::zeroed() };
        event.xclient = XClientMessageEvent {
            type_: CLIENT_MESSAGE,
            serial: 0,
            send_event: 1,
            display: self.display,
            window: target,
            message_type: self.atoms.xdnd_drop,
            format: 32,
            data: [self.window as c_long, 0, CURRENT_TIME as c_long, 0, 0],
        };
        unsafe {
            XSendEvent(self.display, target, 0, 0, &mut event);
            XFlush(self.display);
        }
        Ok(())
    }

    /// Finish the active drag source locally. This is useful when the
    /// destination does not implement `XdndFinished` (protocol version < 5).
    pub fn finish_drag(&mut self, accepted: bool) -> bool {
        let was_active = self.drag_source.finish(accepted);
        if was_active {
            self.drag_source_target = NONE;
            self.inbox.push_back(WindowEvent::DragFinished { accepted });
        }
        was_active
    }

    /// Cancel the active drag source and notify the destination.
    pub fn cancel_drag(&mut self) -> bool {
        if !self.drag_source.is_active() {
            return false;
        }
        let target = self.drag_source_target;
        let mut event: XEvent = unsafe { std::mem::zeroed() };
        event.xclient = XClientMessageEvent {
            type_: CLIENT_MESSAGE,
            serial: 0,
            send_event: 1,
            display: self.display,
            window: target,
            message_type: self.atoms.xdnd_leave,
            format: 32,
            data: [self.window as c_long, 0, 0, 0, 0],
        };
        unsafe {
            XSendEvent(self.display, target, 0, 0, &mut event);
            XFlush(self.display);
        }
        let cancelled = self.drag_source.cancel();
        if cancelled {
            self.drag_source_target = NONE;
            self.inbox.push_back(WindowEvent::DragCancelled);
        }
        cancelled
    }

    pub fn is_drag_source_active(&self) -> bool {
        self.drag_source.is_active()
    }

    pub fn drag_source_targets(&self) -> &[String] {
        self.drag_source.targets()
    }

    /// Accept the current drag, optionally forcing a MIME target.
    ///
    /// Returns the selected target when one was available. Calling this after
    /// `DragEnter` sends an XDND status message to the source immediately.
    pub fn drag_accept(&mut self, target: Option<&str>) -> Result<Option<String>, WindowError> {
        if !self.drag.session.is_active() {
            return Ok(None);
        }
        let chosen = self.drag.session.accept(target);
        self.send_drag_status(chosen.is_some());
        Ok(chosen)
    }

    /// Reject the current drag. The source is told that no action is possible.
    pub fn drag_reject(&mut self) {
        if !self.drag.session.is_active() {
            return;
        }
        self.drag.session.reject();
        self.send_drag_status(false);
    }

    pub fn is_drag_active(&self) -> bool {
        self.drag.session.is_active()
    }

    pub fn drag_target(&self) -> Option<&str> {
        self.drag.session.chosen_target()
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
        self.refresh_cursor_theme();
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
            if self.ime_enabled
                && self.ime.as_ref().is_some_and(|ime| !ime.ic.is_null())
                && unsafe { XFilterEvent(&mut event, self.window) } != 0
            {
                self.drain_ime_events(&mut events);
                continue;
            }
            self.process_event(&event, &mut events);
            self.drain_ime_events(&mut events);
        }
        self.expire_drag(&mut events);
        let now = Instant::now();
        self.incr_writes.retain(|write| write.deadline > now);
        coalesce_events(&mut events);
        self.inbox.extend(events);
    }

    fn drain_ime_events(&mut self, out: &mut Vec<WindowEvent>) {
        let Some(ime) = &self.ime else {
            return;
        };
        let Ok(mut state) = ime.state.lock() else {
            return;
        };
        out.extend(state.drain());
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
                self.focused = focused;
                if let Some(ime) = &self.ime {
                    if !ime.ic.is_null() {
                        unsafe {
                            if focused && self.ime_enabled {
                                XSetICFocus(ime.ic);
                            } else {
                                XUnsetICFocus(ime.ic);
                            }
                        }
                    }
                    if !focused {
                        if let Ok(mut state) = ime.state.lock() {
                            state.reset();
                        }
                    }
                }
                if !focused {
                    self.pressed_keys.clear();
                    self.compose_status = unsafe { std::mem::zeroed() };
                    for set in &mut self.pressed_modifier_keys {
                        set.clear();
                    }
                }
                let _ = focus;
                self.drain_ime_events(out);
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
                if client.format != 32 {
                    return;
                }
                if client.data[0] as Atom == self.atoms.wm_delete_window {
                    self.should_close = true;
                    out.push(WindowEvent::CloseRequested);
                } else {
                    self.handle_drag_message(&client, out);
                }
            }
            PROPERTY_NOTIFY => {
                let property = unsafe { event.xproperty };
                self.handle_property_notify(&property, out);
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
                if clear.selection == self.atoms.xdnd_selection {
                    if self.drag_source.cancel() {
                        self.drag_source_target = NONE;
                        out.push(WindowEvent::DragCancelled);
                    }
                } else if let Some(selection) = self.selection_for_atom(clear.selection) {
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
                if notify.selection == self.atoms.xdnd_selection {
                    self.handle_drag_selection_notify(&notify, out);
                } else if let Some(event) = self.handle_selection_notify(&notify) {
                    out.push(event);
                }
            }
            GENERIC_EVENT => {
                let mut cookie = unsafe { event.xgeneric };
                if unsafe { XGetEventData(self.display, &mut cookie) } != 0 {
                    if cookie.extension == self.xi_opcode && !cookie.data.is_null() {
                        let xi_event = unsafe { &*(cookie.data as *const XIDeviceEvent) };
                        if let Some(touch) = xi_touch_event(
                            xi_event,
                            self.xi_pressure_axes.get(&xi_event.deviceid).copied(),
                        ) {
                            out.push(touch);
                        }
                    }
                    unsafe {
                        XFreeEventData(self.display, &mut cookie);
                    }
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
                (&mut self.compose_status as *mut XComposeStatus).cast(),
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
                self.incr_reads.retain(|read| {
                    !matches!(
                        read.purpose,
                        IncrPurpose::Clipboard { token: pending, .. } if pending == token
                    )
                });
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
        if request.selection == self.atoms.xdnd_selection {
            return self.handle_drag_source_selection_request(request);
        }
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
            if data.len() > INCR_THRESHOLD {
                let expected = data.len() as c_long;
                unsafe {
                    // The owner must watch the requestor window for the
                    // PropertyDelete notifications that pace INCR chunks.
                    XSelectInput(self.display, request.requestor, PROPERTY_CHANGE_MASK);
                    XChangeProperty(
                        self.display,
                        request.requestor,
                        property,
                        self.atoms.incr,
                        32,
                        PROP_MODE_REPLACE,
                        (&expected as *const c_long).cast(),
                        1,
                    );
                }
                self.incr_writes.push(IncrWrite {
                    requestor: request.requestor,
                    property,
                    type_atom,
                    state: IncrWriteState::new(data.to_vec()),
                    deadline: Instant::now() + Duration::from_secs(30),
                });
            } else {
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

    fn handle_drag_source_selection_request(
        &mut self,
        request: &XSelectionRequestEvent,
    ) -> Option<WindowEvent> {
        if !self.drag_source.is_active() && !self.drag_source.has_dropped() {
            return None;
        }
        let target = unsafe { atom_name(self.display, request.target) }.unwrap_or_default();
        let property = if request.property == NONE {
            request.target
        } else {
            request.property
        };
        let mut success = false;
        if request.target == self.atoms.targets {
            let mut names = vec!["TARGETS".to_owned()];
            names.extend(self.drag_source_data.targets());
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
        } else if let Some(data) = self.drag_source_data.target_data(&target) {
            let type_atom = if target == "UTF8_STRING" {
                self.atoms.utf8_string
            } else if target == "STRING" {
                XA_STRING
            } else {
                request.target
            };
            if data.len() > INCR_THRESHOLD {
                let expected = data.len() as c_long;
                unsafe {
                    XSelectInput(self.display, request.requestor, PROPERTY_CHANGE_MASK);
                    XChangeProperty(
                        self.display,
                        request.requestor,
                        property,
                        self.atoms.incr,
                        32,
                        PROP_MODE_REPLACE,
                        (&expected as *const c_long).cast(),
                        1,
                    );
                }
                self.incr_writes.push(IncrWrite {
                    requestor: request.requestor,
                    property,
                    type_atom,
                    state: IncrWriteState::new(data),
                    deadline: Instant::now() + Duration::from_secs(30),
                });
            } else {
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
            }
            self.drag_source.set_chosen_target(Some(&target));
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
        None
    }

    fn handle_selection_notify(&mut self, notify: &XSelectionEvent) -> Option<WindowEvent> {
        let selection = self.selection_for_atom(notify.selection)?;
        let target = unsafe { atom_name(self.display, notify.target) }.unwrap_or_default();
        let succeeded = notify.property != NONE;
        if notify.property != NONE {
            if let Some((actual_type, format, data)) =
                unsafe { get_property(self.display, self.window, notify.property) }
            {
                if actual_type == self.atoms.incr {
                    if let Some(token) = self.clipboard.token_for_property(notify.property) {
                        let expected = expected_size(&data, format);
                        self.incr_reads.push(IncrRead {
                            property: notify.property,
                            state: IncrReadState::new(expected),
                            purpose: IncrPurpose::Clipboard {
                                token,
                                selection,
                                target: target.clone(),
                            },
                            deadline: Instant::now() + Duration::from_secs(30),
                        });
                        return Some(WindowEvent::SelectionNotify {
                            selection,
                            target,
                            succeeded: true,
                        });
                    }
                }
                let reply = selection_reply(self.display, actual_type, format, data);
                self.clipboard.complete(notify.property, reply);
            } else {
                self.clipboard.complete(
                    notify.property,
                    Err(ClipboardError::BadReply(
                        "selection property is missing".to_owned(),
                    )),
                );
            }
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

    fn next_property_atom(&mut self, prefix: &str) -> Atom {
        self.clipboard_property_seq = self.clipboard_property_seq.wrapping_add(1);
        let name = CString::new(format!("{prefix}_{}", self.clipboard_property_seq))
            .expect("generated property name is valid");
        unsafe { XInternAtom(self.display, name.as_ptr(), 0) }
    }

    fn send_drag_status(&self, accepted: bool) {
        if self.drag.source == NONE {
            return;
        }
        let action = if accepted {
            self.atoms.xdnd_action_copy
        } else {
            self.atoms.xdnd_action_none
        };
        let mut event: XEvent = unsafe { std::mem::zeroed() };
        event.xclient = XClientMessageEvent {
            type_: CLIENT_MESSAGE,
            serial: 0,
            send_event: 1,
            display: self.display,
            window: self.drag.source,
            message_type: self.atoms.xdnd_status,
            format: 32,
            data: [
                self.window as c_long,
                i64::from(accepted) as c_long,
                0,
                0,
                action as c_long,
            ],
        };
        unsafe {
            XSendEvent(self.display, self.drag.source, 0, 0, &mut event);
            XFlush(self.display);
        }
    }

    fn send_drag_finished(&self, accepted: bool) {
        if self.drag.source == NONE || self.drag.version < 5 {
            return;
        }
        let action = if accepted {
            self.atoms.xdnd_action_copy
        } else {
            self.atoms.xdnd_action_none
        };
        let mut event: XEvent = unsafe { std::mem::zeroed() };
        event.xclient = XClientMessageEvent {
            type_: CLIENT_MESSAGE,
            serial: 0,
            send_event: 1,
            display: self.display,
            window: self.drag.source,
            message_type: self.atoms.xdnd_finished,
            format: 32,
            data: [
                self.window as c_long,
                i64::from(accepted) as c_long,
                action as c_long,
                0,
                0,
            ],
        };
        unsafe {
            XSendEvent(self.display, self.drag.source, 0, 0, &mut event);
            XFlush(self.display);
        }
    }

    fn handle_drag_message(&mut self, client: &XClientMessageEvent, out: &mut Vec<WindowEvent>) {
        let message = client.message_type;
        if message == self.atoms.xdnd_enter {
            self.handle_drag_enter(client, out);
        } else if message == self.atoms.xdnd_position {
            self.handle_drag_position(client, out);
        } else if message == self.atoms.xdnd_drop {
            self.handle_drag_drop(client, out);
        } else if message == self.atoms.xdnd_leave {
            if self.drag_source.is_active() {
                self.handle_drag_source_message(client, out);
            } else {
                if self.drag.session.leave() {
                    out.push(WindowEvent::DragLeave);
                }
                self.drag.source = NONE;
                self.drag.pending_drop = None;
                self.drag.deadline = None;
            }
        } else if message == self.atoms.xdnd_status || message == self.atoms.xdnd_finished {
            self.handle_drag_source_message(client, out);
        }
    }

    fn handle_drag_source_message(
        &mut self,
        client: &XClientMessageEvent,
        out: &mut Vec<WindowEvent>,
    ) {
        if client.data[0] as Window != self.drag_source_target || !self.drag_source.is_active() {
            return;
        }
        let message = client.message_type;
        if message == self.atoms.xdnd_status {
            let accepted = client.data[1] & 1 != 0;
            self.drag_source.set_accepted(accepted);
        } else if message == self.atoms.xdnd_finished {
            let accepted = client.data[1] & 1 != 0;
            if self.drag_source.finish(accepted) {
                self.drag_source_target = NONE;
                out.push(WindowEvent::DragFinished { accepted });
            }
        } else if message == self.atoms.xdnd_leave && self.drag_source.cancel() {
            self.drag_source_target = NONE;
            out.push(WindowEvent::DragCancelled);
        }
    }

    fn handle_drag_enter(&mut self, client: &XClientMessageEvent, out: &mut Vec<WindowEvent>) {
        let source = client.data[0] as Window;
        if source == NONE {
            return;
        }
        let version = ((client.data[1] >> 24) & 0xff) as i32;
        let flags = client.data[1] & 0xff;
        let mut atoms = Vec::new();
        if flags & 1 != 0 {
            if let Some((_, format, data)) =
                unsafe { get_property_with(self.display, source, self.atoms.xdnd_type_list, 0) }
            {
                if format == 32 {
                    let size = std::mem::size_of::<c_long>();
                    for chunk in data.chunks_exact(size) {
                        let mut raw = [0u8; std::mem::size_of::<c_long>()];
                        raw.copy_from_slice(chunk);
                        let atom = c_long::from_ne_bytes(raw) as Atom;
                        if atom != NONE {
                            atoms.push(atom);
                        }
                    }
                }
            }
        } else {
            for index in 2..5 {
                let atom = client.data[index] as Atom;
                if atom != NONE {
                    atoms.push(atom);
                }
            }
        }
        let targets: Vec<String> = atoms
            .iter()
            .filter_map(|atom| unsafe { atom_name(self.display, *atom) })
            .collect();
        let mut session = DragSession::default();
        session.begin(targets.clone());
        self.drag = DragState {
            session,
            source,
            version,
            property: NONE,
            pending_drop: None,
            deadline: Some(Instant::now() + Duration::from_secs(10)),
        };
        out.push(WindowEvent::DragEnter {
            x: 0.0,
            y: 0.0,
            targets,
        });
    }

    fn handle_drag_position(&mut self, client: &XClientMessageEvent, out: &mut Vec<WindowEvent>) {
        if client.data[0] as Window != self.drag.source || !self.drag.session.is_active() {
            return;
        }
        let packed = client.data[2] as c_ulong;
        let root_x = (packed & 0xffff) as u16 as i16 as c_int;
        let root_y = ((packed >> 16) & 0xffff) as u16 as i16 as c_int;
        let mut x: c_int = 0;
        let mut y: c_int = 0;
        let mut child: Window = NONE;
        unsafe {
            XTranslateCoordinates(
                self.display,
                self.root,
                self.window,
                root_x,
                root_y,
                &mut x,
                &mut y,
                &mut child,
            );
        }
        self.drag.session.set_position(x as f64, y as f64);
        self.drag.deadline = Some(Instant::now() + Duration::from_secs(10));
        let targets = self.drag.session.targets().to_vec();
        out.push(WindowEvent::DragMotion {
            x: x as f64,
            y: y as f64,
            targets,
        });
        self.send_drag_status(self.drag.session.is_accepted());
    }

    fn handle_drag_drop(&mut self, client: &XClientMessageEvent, out: &mut Vec<WindowEvent>) {
        if client.data[0] as Window != self.drag.source || !self.drag.session.is_active() {
            return;
        }
        let Some(target) = self.drag.session.chosen_target().map(str::to_owned) else {
            self.send_drag_finished(false);
            self.drag.session.leave();
            self.drag.source = NONE;
            self.drag.deadline = None;
            out.push(WindowEvent::DragLeave);
            return;
        };
        let (x, y) = self.drag.session.position();
        let timestamp = client.data[2] as Time;
        let property = self.next_property_atom("KITTY_WINDOW_DND");
        let target_c = match CString::new(target.as_str()) {
            Ok(target) => target,
            Err(_) => {
                self.send_drag_finished(false);
                self.drag.session.leave();
                self.drag.source = NONE;
                out.push(WindowEvent::DragLeave);
                return;
            }
        };
        let target_atom = unsafe { XInternAtom(self.display, target_c.as_ptr(), 0) };
        self.drag.property = property;
        self.drag.pending_drop = Some(PendingDragDrop { x, y, target });
        self.drag.deadline = Some(Instant::now() + Duration::from_secs(10));
        unsafe {
            XConvertSelection(
                self.display,
                self.atoms.xdnd_selection,
                target_atom,
                property,
                self.window,
                timestamp,
            );
            XFlush(self.display);
        }
    }

    fn handle_drag_selection_notify(
        &mut self,
        notify: &XSelectionEvent,
        out: &mut Vec<WindowEvent>,
    ) {
        let Some(drop) = self.drag.pending_drop.clone() else {
            return;
        };
        if notify.property == NONE {
            self.send_drag_finished(false);
            self.drag.session.leave();
            self.drag.pending_drop = None;
            self.drag.source = NONE;
            out.push(WindowEvent::DragLeave);
            return;
        }
        let Some((actual_type, format, data)) =
            (unsafe { get_property(self.display, self.window, notify.property) })
        else {
            self.send_drag_finished(false);
            self.drag.session.leave();
            self.drag.pending_drop = None;
            self.drag.source = NONE;
            out.push(WindowEvent::DragLeave);
            return;
        };
        if actual_type == self.atoms.incr {
            let expected = expected_size(&data, format);
            self.incr_reads.push(IncrRead {
                property: notify.property,
                state: IncrReadState::new(expected),
                purpose: IncrPurpose::Drag { drop },
                deadline: Instant::now() + Duration::from_secs(10),
            });
            return;
        }
        self.deliver_drag_data(drop, data, out);
    }

    fn deliver_drag_data(
        &mut self,
        drop: PendingDragDrop,
        data: Vec<u8>,
        out: &mut Vec<WindowEvent>,
    ) {
        let is_uri_list = drop.target.eq_ignore_ascii_case("text/uri-list");
        let (paths, text) = if is_uri_list {
            let payload = String::from_utf8_lossy(&data);
            (parse_uri_list(&payload), None)
        } else {
            (
                Vec::new(),
                Some(String::from_utf8_lossy(&data).into_owned()),
            )
        };
        out.push(WindowEvent::DragDrop {
            x: drop.x,
            y: drop.y,
            paths,
            text,
            target: Some(drop.target),
        });
        self.send_drag_finished(true);
        self.drag.session.leave();
        self.drag.pending_drop = None;
        self.drag.property = NONE;
        self.drag.source = NONE;
        self.drag.deadline = None;
    }

    fn expire_drag(&mut self, out: &mut Vec<WindowEvent>) {
        let now = Instant::now();
        let mut expired = Vec::new();
        let mut expired_clipboard = Vec::new();
        let mut index = 0;
        while index < self.incr_reads.len() {
            if self.incr_reads[index].deadline <= now {
                let read = self.incr_reads.remove(index);
                match read.purpose {
                    IncrPurpose::Drag { drop } => expired.push(drop),
                    IncrPurpose::Clipboard {
                        token: _,
                        selection,
                        target,
                    } => {
                        self.clipboard
                            .complete(read.property, Err(ClipboardError::Timeout));
                        expired_clipboard.push((selection, target));
                    }
                }
            } else {
                index += 1;
            }
        }
        for (selection, target) in expired_clipboard {
            out.push(WindowEvent::SelectionNotify {
                selection,
                target,
                succeeded: false,
            });
        }
        if !expired.is_empty() {
            self.send_drag_finished(false);
            self.drag.session.leave();
            self.drag.pending_drop = None;
            self.drag.source = NONE;
            self.drag.deadline = None;
            out.push(WindowEvent::DragLeave);
        }
        if self.drag.session.is_active()
            && self.drag.pending_drop.is_none()
            && self.drag.deadline.is_some_and(|deadline| deadline <= now)
        {
            self.drag.session.leave();
            self.drag.source = NONE;
            self.drag.deadline = None;
            out.push(WindowEvent::DragLeave);
        }
    }

    fn handle_property_notify(&mut self, event: &XPropertyEvent, out: &mut Vec<WindowEvent>) {
        match event.state {
            // Delete notifications arrive on the requestor window, which is not
            // our own when we are the selection owner.
            PROPERTY_DELETE => self.advance_incr_write(event.atom),
            PROPERTY_NEW_VALUE if event.window == self.window => {
                self.read_incr_chunk(event.atom, out)
            }
            _ => {}
        }
    }

    fn advance_incr_write(&mut self, property: Atom) {
        let Some(index) = self
            .incr_writes
            .iter()
            .position(|write| write.property == property)
        else {
            return;
        };
        self.incr_writes[index].deadline = Instant::now() + Duration::from_secs(30);
        let chunk = self.incr_writes[index]
            .state
            .next_chunk()
            .map(<[u8]>::to_vec);
        let finished = self.incr_writes[index].state.is_finished();
        if let Some(chunk) = chunk {
            let write = &self.incr_writes[index];
            unsafe {
                XChangeProperty(
                    self.display,
                    write.requestor,
                    write.property,
                    write.type_atom,
                    8,
                    PROP_MODE_REPLACE,
                    chunk.as_ptr(),
                    chunk.len() as c_int,
                );
                XFlush(self.display);
            }
        }
        if finished {
            self.incr_writes.remove(index);
        }
    }

    fn read_incr_chunk(&mut self, property: Atom, out: &mut Vec<WindowEvent>) {
        let Some(index) = self
            .incr_reads
            .iter()
            .position(|read| read.property == property)
        else {
            return;
        };
        let Some((_, _, data)) = (unsafe { get_property(self.display, self.window, property) })
        else {
            return;
        };
        let finished = self.incr_reads[index].state.push(&data);
        if !finished {
            return;
        }
        let read = self.incr_reads.remove(index);
        let property = read.property;
        match read.purpose {
            IncrPurpose::Clipboard {
                token: _,
                selection,
                target,
            } => {
                let data = read.state.into_data();
                let reply = match String::from_utf8(data.clone()) {
                    Ok(text) => ClipboardReply::Text(text),
                    Err(_) => ClipboardReply::Data {
                        target: target.clone(),
                        data,
                    },
                };
                self.clipboard.complete(property, Ok(reply));
                out.push(WindowEvent::SelectionNotify {
                    selection,
                    target,
                    succeeded: true,
                });
            }
            IncrPurpose::Drag { drop } => {
                self.deliver_drag_data(drop, read.state.into_data(), out);
            }
        }
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
            WindowEvent::Touch { .. }
            | WindowEvent::DragStarted { .. }
            | WindowEvent::DragFinished { .. }
            | WindowEvent::DragCancelled => {
                self.inbox.push_back(event.clone());
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
            if let Some(mut ime) = self.ime.take() {
                ime.destroy();
            }
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

fn expected_size(data: &[u8], format: c_int) -> usize {
    if format != 32 || data.len() < std::mem::size_of::<c_long>() {
        return 0;
    }
    let mut raw = [0u8; std::mem::size_of::<c_long>()];
    raw.copy_from_slice(&data[..std::mem::size_of::<c_long>()]);
    c_long::from_ne_bytes(raw).max(0) as usize
}

fn selection_reply(
    display: *mut Display,
    actual_type: Atom,
    format: c_int,
    data: Vec<u8>,
) -> Result<ClipboardReply, ClipboardError> {
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
    #[test]
    fn ime_draw_replaces_deletes_and_tracks_caret() {
        let mut state = ImeState::default();
        state.draw(0, 0, Some("abc"), 3);
        let events = state.drain();
        assert_eq!(
            events,
            vec![WindowEvent::Preedit {
                text: "abc".to_owned(),
                cursor: Some((3, 3))
            }]
        );
        assert_eq!(state.text(), "abc");

        state.draw(1, 1, Some("X"), 2);
        assert_eq!(state.text(), "aXc");
        assert_eq!(state.cursor(), Some((2, 2)));

        state.draw(1, 1, None, 1);
        assert_eq!(state.text(), "ac");
        assert_eq!(state.cursor(), Some((1, 1)));
        assert_eq!(state.drain().len(), 2);
    }

    #[test]
    fn ime_caret_uses_character_offsets_for_utf8() {
        let mut state = ImeState::default();
        state.draw(0, 0, Some("café"), 5);
        state.drain();
        state.set_caret(4);
        assert_eq!(state.cursor(), Some((3, 3)));
        state.set_caret(100);
        assert_eq!(state.cursor(), Some((4, 4)));
        assert_eq!(state.drain().len(), 2);
    }

    #[test]
    fn ime_commit_clears_preedit_and_emits_commit() {
        let mut state = ImeState::default();
        state.draw(0, 0, Some("に"), 3);
        state.drain();
        state.commit("日本");
        let events = state.drain();
        assert_eq!(events.len(), 2);
        assert_eq!(
            events[0],
            WindowEvent::Preedit {
                text: String::new(),
                cursor: None
            }
        );
        assert_eq!(
            events[1],
            WindowEvent::Commit {
                text: "日本".to_owned()
            }
        );
        assert_eq!(state.text(), "");
        assert_eq!(state.cursor(), None);
    }

    #[test]
    fn ime_reset_only_emits_when_preedit_was_visible() {
        let mut state = ImeState::default();
        state.reset();
        assert!(state.drain().is_empty());
        state.draw(0, 0, Some("x"), 1);
        state.drain();
        state.reset();
        assert_eq!(
            state.drain(),
            vec![WindowEvent::Preedit {
                text: String::new(),
                cursor: None
            }]
        );
    }

    #[test]
    fn xim_text_decoding_handles_multibyte_and_wide_strings() {
        let bytes = b"hello";
        let text = XIMText {
            length: bytes.len() as c_ushort,
            feedback: ptr::null_mut(),
            encoding_is_wchar: 0,
            string: XIMTextString {
                multi_byte: bytes.as_ptr() as *mut c_char,
            },
        };
        assert_eq!(unsafe { decode_xim_text(&text) }, Some("hello".to_owned()));

        let units = [0x61_u32, 0x1f600];
        let text = XIMText {
            length: units.len() as c_ushort,
            feedback: ptr::null_mut(),
            encoding_is_wchar: 1,
            string: XIMTextString {
                wide_char: units.as_ptr() as *mut u32,
            },
        };
        assert_eq!(unsafe { decode_xim_text(&text) }, Some("a😀".to_owned()));
        assert_eq!(unsafe { decode_xim_text(ptr::null()) }, None);
    }

    #[test]
    fn input_style_prefers_preedit_callbacks() {
        let styles = [
            XIM_PREEDIT_NOTHING | XIM_STATUS_NOTHING,
            XIM_PREEDIT_CALLBACKS | XIM_STATUS_NOTHING,
        ];
        assert_eq!(
            choose_input_style(&styles),
            Some((XIM_PREEDIT_CALLBACKS | XIM_STATUS_NOTHING, true))
        );
        assert_eq!(
            choose_input_style(&[XIM_PREEDIT_NOTHING | XIM_STATUS_NOTHING]),
            Some((XIM_PREEDIT_NOTHING | XIM_STATUS_NOTHING, false))
        );
        assert_eq!(choose_input_style(&[]), None);
    }

    #[test]
    fn xcursor_names_cover_pointer_shapes() {
        assert_eq!(xcursor_name(PointerShape::Default), Some("left_ptr"));
        assert_eq!(xcursor_name(PointerShape::Text), Some("xterm"));
        assert_eq!(
            xcursor_name(PointerShape::ResizeSE),
            Some("bottom_right_corner")
        );
        assert_eq!(xcursor_name(PointerShape::ZoomIn), Some("zoom-in"));
        assert_eq!(
            xcursor_name(PointerShape::ContextMenu),
            Some("context-menu")
        );
    }

    #[test]
    fn xi_touch_events_map_phase_position_and_pressure() {
        let mut mask = [1u8 << 2];
        let mut values = [0.75f64];
        let mut event: XIDeviceEvent = unsafe { std::mem::zeroed() };
        event.type_ = GENERIC_EVENT;
        event.evtype = XI_TOUCH_BEGIN;
        event.detail = 4;
        event.event_x = 12.5;
        event.event_y = 34.5;
        event.valuators = XIValuatorState {
            mask_len: 1,
            mask: mask.as_mut_ptr(),
            values: values.as_mut_ptr(),
        };
        assert_eq!(xi_value_for_axis(&event, 2), Some(0.75));
        assert_eq!(xi_value_for_axis(&event, 3), None);
        assert_eq!(
            xi_touch_event(
                &event,
                Some(XiPressureAxis {
                    number: 2,
                    min: 0.0,
                    max: 1.0
                })
            ),
            Some(WindowEvent::Touch {
                id: 4,
                x: 12.5,
                y: 34.5,
                pressure: 0.75,
                phase: crate::types::TouchPhase::Began,
            })
        );

        event.evtype = XI_TOUCH_UPDATE;
        event.detail = 4;
        assert!(matches!(
            xi_touch_event(
                &event,
                Some(XiPressureAxis {
                    number: 2,
                    min: 0.0,
                    max: 1.0
                })
            ),
            Some(WindowEvent::Touch {
                phase: crate::types::TouchPhase::Updated,
                ..
            })
        ));

        event.evtype = XI_TOUCH_END;
        match xi_touch_event(&event, None) {
            Some(WindowEvent::Touch {
                pressure, phase, ..
            }) => {
                assert_eq!(pressure, 0.0);
                assert_eq!(phase, crate::types::TouchPhase::Ended);
            }
            other => panic!("unexpected touch mapping: {other:?}"),
        }
    }

    #[test]
    fn xi_touch_pressure_accepts_16_bit_ranges() {
        let mut mask = [1u8 << 2];
        let mut values = [32768.0f64];
        let mut event: XIDeviceEvent = unsafe { std::mem::zeroed() };
        event.evtype = XI_TOUCH_BEGIN;
        event.detail = 1;
        event.valuators = XIValuatorState {
            mask_len: 1,
            mask: mask.as_mut_ptr(),
            values: values.as_mut_ptr(),
        };
        let Some(WindowEvent::Touch { pressure, .. }) = xi_touch_event(
            &event,
            Some(XiPressureAxis {
                number: 2,
                min: 0.0,
                max: 65535.0,
            }),
        ) else {
            panic!("expected a touch event");
        };
        assert!((pressure - 0.5).abs() < 0.001);
    }

    #[test]
    fn incr_expected_size_reads_a_32_bit_property() {
        let value: c_long = 300 * 1024;
        let bytes = value.to_ne_bytes();
        assert_eq!(expected_size(&bytes, 32), 300 * 1024);
        assert_eq!(expected_size(&bytes, 8), 0);
        assert_eq!(expected_size(&[], 32), 0);
    }
}
