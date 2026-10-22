// SPDX-License-Identifier: GPL-3.0-or-later
//! Window-system EGL surface for the X11 backend.
//!
//! This module is intentionally independent of `kitty-render`. It links raw
//! `libEGL`, obtains an X11 platform display with
//! `eglGetPlatformDisplay(EGL_PLATFORM_X11_KHR, ...)`, creates an RGB8 window
//! surface, and loads only the small set of core GL entry points needed to
//! clear, swap and read back pixels. A consumer that already has its own GL
//! loader can ignore [`GlFns`] and use [`EglWindowSurface::make_current`].
//!
//! A Mesa EGL display is process-global. Only one [`EglWindowSurface`] should
//! normally be alive per native X11 display, and it must be dropped before the
//! [`X11Window`](crate::X11Window) that owns the drawable.

use std::ffi::{c_char, c_int, c_ulong, c_void, CStr};
use std::fmt;
use std::ptr;

use crate::x11::X11Window;

// {{{ EGL types, constants and FFI

pub type EGLDisplay = *mut c_void;
pub type EGLConfig = *mut c_void;
pub type EGLSurface = *mut c_void;
pub type EGLContext = *mut c_void;

type EGLBoolean = u32;
type EGLenum = u32;
type EGLint = i32;

const EGL_TRUE: EGLBoolean = 1;

const EGL_SUCCESS: EGLint = 0x3000;
const EGL_NOT_INITIALIZED: EGLint = 0x3001;
const EGL_BAD_ACCESS: EGLint = 0x3002;
const EGL_BAD_ALLOC: EGLint = 0x3003;
const EGL_BAD_ATTRIBUTE: EGLint = 0x3004;
const EGL_BAD_CONFIG: EGLint = 0x3005;
const EGL_BAD_CONTEXT: EGLint = 0x3006;
const EGL_BAD_CURRENT_SURFACE: EGLint = 0x3007;
const EGL_BAD_DISPLAY: EGLint = 0x3008;
const EGL_BAD_MATCH: EGLint = 0x3009;
const EGL_BAD_NATIVE_PIXMAP: EGLint = 0x300A;
const EGL_BAD_NATIVE_WINDOW: EGLint = 0x300B;
const EGL_BAD_PARAMETER: EGLint = 0x300C;
const EGL_BAD_SURFACE: EGLint = 0x300D;
const EGL_CONTEXT_LOST: EGLint = 0x300E;

const EGL_PLATFORM_X11_KHR: EGLenum = 0x31D5;
#[cfg(feature = "wayland")]
const EGL_PLATFORM_WAYLAND_KHR: EGLenum = 0x31D8;

const EGL_NONE: EGLint = 0x3038;
const EGL_NO_SURFACE_VALUE: *mut c_void = ptr::null_mut();
const EGL_NO_CONTEXT_VALUE: *mut c_void = ptr::null_mut();

const EGL_SURFACE_TYPE: EGLint = 0x3033;
const EGL_WINDOW_BIT: EGLint = 0x0004;
const EGL_RENDERABLE_TYPE: EGLint = 0x3040;
const EGL_OPENGL_BIT: EGLint = 0x0008;
const EGL_OPENGL_ES3_BIT_KHR: EGLint = 0x0040;
const EGL_RED_SIZE: EGLint = 0x3024;
const EGL_GREEN_SIZE: EGLint = 0x3023;
const EGL_BLUE_SIZE: EGLint = 0x3022;
const EGL_ALPHA_SIZE: EGLint = 0x3021;
const EGL_DEPTH_SIZE: EGLint = 0x3025;
const EGL_STENCIL_SIZE: EGLint = 0x3026;
const EGL_WIDTH: EGLint = 0x3057;
const EGL_HEIGHT: EGLint = 0x3056;
const EGL_VERSION: EGLint = 0x3054;
const EGL_VENDOR: EGLint = 0x3053;
const EGL_CLIENT_APIS: EGLint = 0x308D;

const EGL_OPENGL_API: EGLenum = 0x30A2;
const EGL_OPENGL_ES_API: EGLenum = 0x30A0;

const EGL_CONTEXT_MAJOR_VERSION: EGLint = 0x3098;
const EGL_CONTEXT_MINOR_VERSION: EGLint = 0x30FB;
const EGL_CONTEXT_OPENGL_PROFILE_MASK: EGLint = 0x30FD;
const EGL_CONTEXT_OPENGL_CORE_PROFILE_BIT: EGLint = 0x0001;

const GL_COLOR_BUFFER_BIT: u32 = 0x0000_4000;
const GL_RGBA: u32 = 0x1908;
const GL_UNSIGNED_BYTE: u32 = 0x1401;
const GL_PACK_ALIGNMENT: u32 = 0x0D05;

#[link(name = "EGL")]
unsafe extern "C" {
    fn eglGetPlatformDisplay(
        platform: EGLenum,
        native_display: *mut c_void,
        attrib_list: *const EGLint,
    ) -> EGLDisplay;
    fn eglGetDisplay(display_id: *mut c_void) -> EGLDisplay;
    fn eglGetProcAddress(name: *const c_char) -> *mut c_void;
    fn eglInitialize(display: EGLDisplay, major: *mut EGLint, minor: *mut EGLint) -> EGLBoolean;
    fn eglTerminate(display: EGLDisplay) -> EGLBoolean;
    fn eglGetError() -> EGLint;
    fn eglQueryString(display: EGLDisplay, name: EGLint) -> *const c_char;
    fn eglChooseConfig(
        display: EGLDisplay,
        attribs: *const EGLint,
        configs: *mut EGLConfig,
        config_size: EGLint,
        num_config: *mut EGLint,
    ) -> EGLBoolean;
    fn eglCreateWindowSurface(
        display: EGLDisplay,
        config: EGLConfig,
        native_window: *mut c_void,
        attribs: *const EGLint,
    ) -> EGLSurface;
    fn eglDestroySurface(display: EGLDisplay, surface: EGLSurface) -> EGLBoolean;
    fn eglQuerySurface(
        display: EGLDisplay,
        surface: EGLSurface,
        attribute: EGLint,
        value: *mut EGLint,
    ) -> EGLBoolean;
    fn eglBindAPI(api: EGLenum) -> EGLBoolean;
    fn eglCreateContext(
        display: EGLDisplay,
        config: EGLConfig,
        share_context: EGLContext,
        attribs: *const EGLint,
    ) -> EGLContext;
    fn eglDestroyContext(display: EGLDisplay, context: EGLContext) -> EGLBoolean;
    fn eglMakeCurrent(
        display: EGLDisplay,
        draw: EGLSurface,
        read: EGLSurface,
        context: EGLContext,
    ) -> EGLBoolean;
    fn eglSwapBuffers(display: EGLDisplay, surface: EGLSurface) -> EGLBoolean;
    fn eglSwapInterval(display: EGLDisplay, interval: EGLint) -> EGLBoolean;
}

#[link(name = "X11")]
unsafe extern "C" {
    fn XResizeWindow(display: *mut c_void, window: c_ulong, width: u32, height: u32) -> c_int;
    fn XSync(display: *mut c_void, discard: c_int) -> c_int;
}

#[cfg(feature = "wayland")]
#[link(name = "wayland-egl")]
unsafe extern "C" {
    fn wl_egl_window_create(surface: *mut c_void, width: c_int, height: c_int) -> *mut c_void;
    fn wl_egl_window_destroy(window: *mut c_void);
    fn wl_egl_window_resize(window: *mut c_void, width: c_int, height: c_int, dx: c_int, dy: c_int);
}

// }}}
// {{{ EGL errors

/// EGL/GL error carrying the failed operation and the raw EGL error code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EglError {
    pub code: EGLint,
    pub operation: &'static str,
}

impl EglError {
    fn last(operation: &'static str) -> Self {
        Self {
            code: unsafe { eglGetError() },
            operation,
        }
    }

    pub fn name(code: EGLint) -> &'static str {
        match code {
            EGL_SUCCESS => "EGL_SUCCESS",
            EGL_NOT_INITIALIZED => "EGL_NOT_INITIALIZED",
            EGL_BAD_ACCESS => "EGL_BAD_ACCESS",
            EGL_BAD_ALLOC => "EGL_BAD_ALLOC",
            EGL_BAD_ATTRIBUTE => "EGL_BAD_ATTRIBUTE",
            EGL_BAD_CONFIG => "EGL_BAD_CONFIG",
            EGL_BAD_CONTEXT => "EGL_BAD_CONTEXT",
            EGL_BAD_CURRENT_SURFACE => "EGL_BAD_CURRENT_SURFACE",
            EGL_BAD_DISPLAY => "EGL_BAD_DISPLAY",
            EGL_BAD_MATCH => "EGL_BAD_MATCH",
            EGL_BAD_NATIVE_PIXMAP => "EGL_BAD_NATIVE_PIXMAP",
            EGL_BAD_NATIVE_WINDOW => "EGL_BAD_NATIVE_WINDOW",
            EGL_BAD_PARAMETER => "EGL_BAD_PARAMETER",
            EGL_BAD_SURFACE => "EGL_BAD_SURFACE",
            EGL_CONTEXT_LOST => "EGL_CONTEXT_LOST",
            _ => "EGL_UNKNOWN_ERROR",
        }
    }

    fn check(ok: EGLBoolean, operation: &'static str) -> Result<(), Self> {
        if ok == EGL_TRUE {
            Ok(())
        } else {
            Err(Self::last(operation))
        }
    }
}

impl fmt::Display for EglError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} failed with {} (0x{:04x})",
            self.operation,
            Self::name(self.code),
            self.code
        )
    }
}

impl std::error::Error for EglError {}

// }}}
// {{{ Minimal GL loader

/// Which client API the context uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlApi {
    OpenGl,
    OpenGlEs3,
}

type GlClearColorFn = unsafe extern "C" fn(f32, f32, f32, f32);
type GlClearFn = unsafe extern "C" fn(u32);
type GlViewportFn = unsafe extern "C" fn(c_int, c_int, c_int, c_int);
type GlReadPixelsFn = unsafe extern "C" fn(c_int, c_int, c_int, c_int, u32, u32, *mut c_void);
type GlGetStringFn = unsafe extern "C" fn(u32) -> *const u8;
type GlGetErrorFn = unsafe extern "C" fn() -> u32;
type GlFinishFn = unsafe extern "C" fn();
type GlPixelStoreFn = unsafe extern "C" fn(u32, c_int);

/// Core GL functions needed for clear/readback.
#[derive(Clone, Copy)]
pub struct GlFns {
    clear_color: GlClearColorFn,
    clear: GlClearFn,
    viewport: GlViewportFn,
    read_pixels: GlReadPixelsFn,
    get_string: GlGetStringFn,
    get_error: GlGetErrorFn,
    finish: GlFinishFn,
    pixel_store_i: GlPixelStoreFn,
}

impl GlFns {
    unsafe fn load() -> Result<Self, EglError> {
        Ok(Self {
            clear_color: unsafe { load_proc(c"glClearColor")? },
            clear: unsafe { load_proc(c"glClear")? },
            viewport: unsafe { load_proc(c"glViewport")? },
            read_pixels: unsafe { load_proc(c"glReadPixels")? },
            get_string: unsafe { load_proc(c"glGetString")? },
            get_error: unsafe { load_proc(c"glGetError")? },
            finish: unsafe { load_proc(c"glFinish")? },
            pixel_store_i: unsafe { load_proc(c"glPixelStorei")? },
        })
    }

    pub fn clear_color(&self, red: f32, green: f32, blue: f32, alpha: f32) {
        unsafe {
            (self.clear_color)(red, green, blue, alpha);
        }
    }

    pub fn clear(&self) {
        unsafe {
            (self.clear)(GL_COLOR_BUFFER_BIT);
        }
    }

    pub fn viewport(&self, x: i32, y: i32, width: i32, height: i32) {
        unsafe {
            (self.viewport)(x, y, width, height);
        }
    }

    pub fn finish(&self) {
        unsafe {
            (self.finish)();
        }
    }

    pub fn get_error(&self) -> u32 {
        unsafe { (self.get_error)() }
    }

    pub fn version_string(&self) -> Option<String> {
        const GL_VERSION: u32 = 0x1F02;
        self.string(GL_VERSION)
    }

    pub fn renderer_string(&self) -> Option<String> {
        const GL_RENDERER: u32 = 0x1F01;
        self.string(GL_RENDERER)
    }

    fn string(&self, name: u32) -> Option<String> {
        let ptr = unsafe { (self.get_string)(name) };
        if ptr.is_null() {
            None
        } else {
            Some(
                unsafe { CStr::from_ptr(ptr.cast()) }
                    .to_string_lossy()
                    .into_owned(),
            )
        }
    }

    unsafe fn read_rgba(&self, width: u32, height: u32, out: *mut c_void) {
        unsafe {
            (self.pixel_store_i)(GL_PACK_ALIGNMENT, 1);
            (self.read_pixels)(
                0,
                0,
                width as c_int,
                height as c_int,
                GL_RGBA,
                GL_UNSIGNED_BYTE,
                out,
            );
        }
    }
}

unsafe fn load_proc<T: Copy>(name: &CStr) -> Result<T, EglError> {
    let ptr = unsafe { eglGetProcAddress(name.as_ptr()) };
    if ptr.is_null() {
        return Err(EglError {
            code: EGL_BAD_PARAMETER,
            operation: "eglGetProcAddress",
        });
    }
    Ok(unsafe { std::mem::transmute_copy(&ptr) })
}

// }}}
// {{{ EglWindowSurface

/// An EGL window surface bound to an [`X11Window`].
pub struct EglWindowSurface {
    display: EGLDisplay,
    surface: EGLSurface,
    context: EGLContext,
    gl: GlFns,
    api: GlApi,
    native_display: *mut c_void,
    native_window: u64,
    width: u32,
    height: u32,
    egl_version: (i32, i32),
    gl_version: Option<String>,
    gl_renderer: Option<String>,
    terminated: bool,
}

impl EglWindowSurface {
    /// Create an RGB8 window surface and a GL 3.3 core context, falling back to
    /// OpenGL ES 3 when desktop GL is unavailable.
    pub fn new(window: &X11Window) -> Result<Self, EglError> {
        let native_display = window.display_ptr();
        let native_window = window.window_id();
        let (width, height) = window.size();
        let display = unsafe {
            let display = eglGetPlatformDisplay(EGL_PLATFORM_X11_KHR, native_display, ptr::null());
            if !display.is_null() {
                display
            } else {
                let ext: Option<
                    unsafe extern "C" fn(EGLenum, *mut c_void, *const EGLint) -> EGLDisplay,
                > = load_proc(c"eglGetPlatformDisplayEXT").ok();
                if let Some(ext) = ext {
                    let display = ext(EGL_PLATFORM_X11_KHR, native_display, ptr::null());
                    if !display.is_null() {
                        display
                    } else {
                        eglGetDisplay(native_display)
                    }
                } else {
                    eglGetDisplay(native_display)
                }
            }
        };
        if display.is_null() {
            return Err(EglError::last("eglGetPlatformDisplay"));
        }

        let mut major = 0;
        let mut minor = 0;
        unsafe {
            EglError::check(
                eglInitialize(display, &mut major, &mut minor),
                "eglInitialize",
            )?;
        }

        let (surface, context, api) =
            match unsafe { try_desktop(display, native_window as *mut c_void) } {
                Ok((surface, context)) => (surface, context, GlApi::OpenGl),
                Err(_) => {
                    unsafe {
                        let _ = eglBindAPI(EGL_OPENGL_ES_API);
                    }
                    match unsafe { try_gles(display, native_window as *mut c_void) } {
                        Ok((surface, context)) => (surface, context, GlApi::OpenGlEs3),
                        Err(error) => {
                            unsafe {
                                let _ = eglTerminate(display);
                            }
                            return Err(error);
                        }
                    }
                }
            };

        if unsafe { eglMakeCurrent(display, surface, surface, context) } != EGL_TRUE {
            let error = EglError::last("eglMakeCurrent");
            unsafe {
                let _ = eglDestroyContext(display, context);
                let _ = eglDestroySurface(display, surface);
                let _ = eglTerminate(display);
            }
            return Err(error);
        }

        let gl = match unsafe { GlFns::load() } {
            Ok(gl) => gl,
            Err(error) => {
                unsafe {
                    let _ = eglMakeCurrent(
                        display,
                        EGL_NO_SURFACE_VALUE,
                        EGL_NO_SURFACE_VALUE,
                        EGL_NO_CONTEXT_VALUE,
                    );
                    let _ = eglDestroyContext(display, context);
                    let _ = eglDestroySurface(display, surface);
                    let _ = eglTerminate(display);
                }
                return Err(error);
            }
        };
        let (width, height) = unsafe { query_surface_size(display, surface, width, height) };
        Ok(Self {
            display,
            surface,
            context,
            gl,
            api,
            native_display,
            native_window,
            width,
            height,
            egl_version: (major, minor),
            gl_version: gl.version_string(),
            gl_renderer: gl.renderer_string(),
            terminated: false,
        })
    }

    pub fn display_ptr(&self) -> *mut c_void {
        self.display
    }

    pub fn native_window(&self) -> u64 {
        self.native_window
    }

    pub fn native_display(&self) -> *mut c_void {
        self.native_display
    }

    pub fn api(&self) -> GlApi {
        self.api
    }

    pub fn gl_fns(&self) -> GlFns {
        self.gl
    }

    pub fn egl_version(&self) -> (i32, i32) {
        self.egl_version
    }

    pub fn gl_version_string(&self) -> Option<&str> {
        self.gl_version.as_deref()
    }

    pub fn gl_renderer_string(&self) -> Option<&str> {
        self.gl_renderer.as_deref()
    }

    pub fn egl_vendor(&self) -> String {
        query_egl_string(self.display, EGL_VENDOR).unwrap_or_default()
    }

    pub fn egl_version_string(&self) -> String {
        query_egl_string(self.display, EGL_VERSION).unwrap_or_default()
    }

    pub fn egl_client_apis(&self) -> String {
        query_egl_string(self.display, EGL_CLIENT_APIS).unwrap_or_default()
    }

    pub fn make_current(&self) -> Result<(), EglError> {
        unsafe {
            EglError::check(
                eglMakeCurrent(self.display, self.surface, self.surface, self.context),
                "eglMakeCurrent",
            )
        }
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Query the current size from the native surface. This is useful after a
    /// window manager changed the window geometry.
    pub fn refresh_size(&mut self) -> (u32, u32) {
        let (width, height) =
            unsafe { query_surface_size(self.display, self.surface, self.width, self.height) };
        self.width = width;
        self.height = height;
        (width, height)
    }

    /// Resize the native X11 window and refresh the EGL surface size.
    pub fn resize(&mut self, width: u32, height: u32) -> Result<(), EglError> {
        let width = width.max(1);
        let height = height.max(1);
        unsafe {
            XResizeWindow(
                self.native_display,
                self.native_window as c_ulong,
                width,
                height,
            );
            XSync(self.native_display, 0);
        }
        self.refresh_size();
        Ok(())
    }

    pub fn swap_buffers(&self) -> Result<(), EglError> {
        unsafe { EglError::check(eglSwapBuffers(self.display, self.surface), "eglSwapBuffers") }
    }

    pub fn set_swap_interval(&self, interval: i32) -> Result<(), EglError> {
        unsafe { EglError::check(eglSwapInterval(self.display, interval), "eglSwapInterval") }
    }

    /// Set the viewport to the whole surface and clear it.
    pub fn clear(&mut self, color: [f32; 4]) -> Result<(), EglError> {
        self.make_current()?;
        self.refresh_size();
        self.gl
            .viewport(0, 0, self.width as i32, self.height as i32);
        self.gl.clear_color(color[0], color[1], color[2], color[3]);
        self.gl.clear();
        let error = self.gl.get_error();
        if error == 0 {
            Ok(())
        } else {
            Err(EglError {
                code: error as EGLint,
                operation: "glClear",
            })
        }
    }

    /// Read the current drawable as tightly packed RGBA8, bottom-up as GL
    /// returns it.
    pub fn read_pixels(&mut self) -> Result<Vec<u8>, EglError> {
        self.make_current()?;
        self.refresh_size();
        self.gl
            .viewport(0, 0, self.width as i32, self.height as i32);
        self.gl.finish();
        let mut pixels = vec![0u8; self.width as usize * self.height as usize * 4];
        unsafe {
            self.gl
                .read_rgba(self.width, self.height, pixels.as_mut_ptr().cast());
        }
        let error = self.gl.get_error();
        if error == 0 {
            Ok(pixels)
        } else {
            Err(EglError {
                code: error as EGLint,
                operation: "glReadPixels",
            })
        }
    }

    /// Convenience for tests: read one pixel at (x, y) in the GL bottom-left
    /// coordinate system.
    pub fn read_pixel(&mut self, x: u32, y: u32) -> Result<[u8; 4], EglError> {
        let pixels = self.read_pixels()?;
        if x >= self.width || y >= self.height {
            return Err(EglError {
                code: EGL_BAD_PARAMETER,
                operation: "read_pixel",
            });
        }
        let index = ((y * self.width + x) * 4) as usize;
        Ok([
            pixels[index],
            pixels[index + 1],
            pixels[index + 2],
            pixels[index + 3],
        ])
    }
}

// {{{ Wayland EGL surface

/// An EGL window surface bound to a [`WaylandWindow`](crate::WaylandWindow).
///
/// This uses `EGL_PLATFORM_WAYLAND_KHR`, a `wl_egl_window` and the same
/// clear/swap/readback helpers as the X11 surface. The surface must be dropped
/// before the window that created it.
#[cfg(feature = "wayland")]
pub struct EglWaylandSurface {
    display: EGLDisplay,
    egl_window: *mut c_void,
    surface: EGLSurface,
    context: EGLContext,
    gl: GlFns,
    api: GlApi,
    native_display: *mut c_void,
    wayland_surface: *mut c_void,
    width: u32,
    height: u32,
    egl_version: (i32, i32),
    gl_version: Option<String>,
    gl_renderer: Option<String>,
    terminated: bool,
}

#[cfg(feature = "wayland")]
impl EglWaylandSurface {
    /// Create an RGB8 EGL surface for a Wayland window, using GL 3.3 core and
    /// falling back to OpenGL ES 3.
    pub fn new(window: &crate::WaylandWindow) -> Result<Self, EglError> {
        let native_display = window.display_ptr();
        let wayland_surface = window.wl_surface_ptr();
        if native_display.is_null() || wayland_surface.is_null() {
            return Err(EglError {
                code: EGL_BAD_NATIVE_WINDOW,
                operation: "WaylandWindow",
            });
        }
        let (width, height) = window.size();
        let egl_window = unsafe {
            wl_egl_window_create(
                wayland_surface,
                width.max(1) as c_int,
                height.max(1) as c_int,
            )
        };
        if egl_window.is_null() {
            return Err(EglError {
                code: EGL_BAD_NATIVE_WINDOW,
                operation: "wl_egl_window_create",
            });
        }
        let display = unsafe {
            let display =
                eglGetPlatformDisplay(EGL_PLATFORM_WAYLAND_KHR, native_display, ptr::null());
            if !display.is_null() {
                display
            } else {
                let ext: Option<
                    unsafe extern "C" fn(EGLenum, *mut c_void, *const EGLint) -> EGLDisplay,
                > = load_proc(c"eglGetPlatformDisplayEXT").ok();
                if let Some(ext) = ext {
                    let display = ext(EGL_PLATFORM_WAYLAND_KHR, native_display, ptr::null());
                    if !display.is_null() {
                        display
                    } else {
                        eglGetDisplay(native_display)
                    }
                } else {
                    eglGetDisplay(native_display)
                }
            }
        };
        if display.is_null() {
            unsafe {
                wl_egl_window_destroy(egl_window);
            }
            return Err(EglError::last("eglGetPlatformDisplay"));
        }

        let mut major = 0;
        let mut minor = 0;
        unsafe {
            EglError::check(
                eglInitialize(display, &mut major, &mut minor),
                "eglInitialize",
            )?;
        }

        let (surface, context, api) = match unsafe { try_desktop(display, egl_window) } {
            Ok((surface, context)) => (surface, context, GlApi::OpenGl),
            Err(_) => {
                unsafe {
                    let _ = eglBindAPI(EGL_OPENGL_ES_API);
                }
                match unsafe { try_gles(display, egl_window) } {
                    Ok((surface, context)) => (surface, context, GlApi::OpenGlEs3),
                    Err(error) => {
                        unsafe {
                            let _ = eglTerminate(display);
                            wl_egl_window_destroy(egl_window);
                        }
                        return Err(error);
                    }
                }
            }
        };

        if unsafe { eglMakeCurrent(display, surface, surface, context) } != EGL_TRUE {
            let error = EglError::last("eglMakeCurrent");
            unsafe {
                let _ = eglDestroyContext(display, context);
                let _ = eglDestroySurface(display, surface);
                let _ = eglTerminate(display);
                wl_egl_window_destroy(egl_window);
            }
            return Err(error);
        }

        let gl = match unsafe { GlFns::load() } {
            Ok(gl) => gl,
            Err(error) => {
                unsafe {
                    let _ = eglMakeCurrent(
                        display,
                        EGL_NO_SURFACE_VALUE,
                        EGL_NO_SURFACE_VALUE,
                        EGL_NO_CONTEXT_VALUE,
                    );
                    let _ = eglDestroyContext(display, context);
                    let _ = eglDestroySurface(display, surface);
                    let _ = eglTerminate(display);
                    wl_egl_window_destroy(egl_window);
                }
                return Err(error);
            }
        };
        let (width, height) = unsafe { query_surface_size(display, surface, width, height) };
        Ok(Self {
            display,
            egl_window,
            surface,
            context,
            gl,
            api,
            native_display,
            wayland_surface,
            width,
            height,
            egl_version: (major, minor),
            gl_version: gl.version_string(),
            gl_renderer: gl.renderer_string(),
            terminated: false,
        })
    }

    pub fn display_ptr(&self) -> *mut c_void {
        self.display
    }

    pub fn native_display(&self) -> *mut c_void {
        self.native_display
    }

    pub fn wl_surface_ptr(&self) -> *mut c_void {
        self.wayland_surface
    }

    pub fn api(&self) -> GlApi {
        self.api
    }

    pub fn gl_fns(&self) -> GlFns {
        self.gl
    }

    pub fn egl_version(&self) -> (i32, i32) {
        self.egl_version
    }

    pub fn gl_version_string(&self) -> Option<&str> {
        self.gl_version.as_deref()
    }

    pub fn gl_renderer_string(&self) -> Option<&str> {
        self.gl_renderer.as_deref()
    }

    pub fn egl_vendor(&self) -> String {
        query_egl_string(self.display, EGL_VENDOR).unwrap_or_default()
    }

    pub fn egl_version_string(&self) -> String {
        query_egl_string(self.display, EGL_VERSION).unwrap_or_default()
    }

    pub fn egl_client_apis(&self) -> String {
        query_egl_string(self.display, EGL_CLIENT_APIS).unwrap_or_default()
    }

    pub fn make_current(&self) -> Result<(), EglError> {
        unsafe {
            EglError::check(
                eglMakeCurrent(self.display, self.surface, self.surface, self.context),
                "eglMakeCurrent",
            )
        }
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn refresh_size(&mut self) -> (u32, u32) {
        // `wl_egl_window_resize` is authoritative; eglQuerySurface on Wayland
        // can keep reporting the previous size until the next buffer swap.
        (self.width, self.height)
    }

    /// Resize the `wl_egl_window` and update the cached surface size.
    pub fn resize(&mut self, width: u32, height: u32) -> Result<(), EglError> {
        let width = width.max(1);
        let height = height.max(1);
        unsafe {
            wl_egl_window_resize(self.egl_window, width as c_int, height as c_int, 0, 0);
        }
        self.width = width;
        self.height = height;
        Ok(())
    }

    pub fn swap_buffers(&self) -> Result<(), EglError> {
        unsafe { EglError::check(eglSwapBuffers(self.display, self.surface), "eglSwapBuffers") }
    }

    pub fn set_swap_interval(&self, interval: i32) -> Result<(), EglError> {
        unsafe { EglError::check(eglSwapInterval(self.display, interval), "eglSwapInterval") }
    }

    pub fn clear(&mut self, color: [f32; 4]) -> Result<(), EglError> {
        self.make_current()?;
        self.refresh_size();
        self.gl
            .viewport(0, 0, self.width as i32, self.height as i32);
        self.gl.clear_color(color[0], color[1], color[2], color[3]);
        self.gl.clear();
        let error = self.gl.get_error();
        if error == 0 {
            Ok(())
        } else {
            Err(EglError {
                code: error as EGLint,
                operation: "glClear",
            })
        }
    }

    /// Swap buffers after a clear. This is the normal per-frame entry point for
    /// a Wayland window using EGL.
    pub fn swap_after_clear(&mut self, color: [f32; 4]) -> Result<(), EglError> {
        self.clear(color)?;
        self.swap_buffers()
    }

    pub fn read_pixels(&mut self) -> Result<Vec<u8>, EglError> {
        self.make_current()?;
        self.refresh_size();
        self.gl
            .viewport(0, 0, self.width as i32, self.height as i32);
        self.gl.finish();
        let mut pixels = vec![0u8; self.width as usize * self.height as usize * 4];
        unsafe {
            self.gl
                .read_rgba(self.width, self.height, pixels.as_mut_ptr().cast());
        }
        let error = self.gl.get_error();
        if error == 0 {
            Ok(pixels)
        } else {
            Err(EglError {
                code: error as EGLint,
                operation: "glReadPixels",
            })
        }
    }

    pub fn read_pixel(&mut self, x: u32, y: u32) -> Result<[u8; 4], EglError> {
        let pixels = self.read_pixels()?;
        if x >= self.width || y >= self.height {
            return Err(EglError {
                code: EGL_BAD_PARAMETER,
                operation: "read_pixel",
            });
        }
        let index = ((y * self.width + x) * 4) as usize;
        Ok([
            pixels[index],
            pixels[index + 1],
            pixels[index + 2],
            pixels[index + 3],
        ])
    }
}

#[cfg(feature = "wayland")]
impl Drop for EglWaylandSurface {
    fn drop(&mut self) {
        if self.terminated {
            return;
        }
        unsafe {
            let _ = eglMakeCurrent(
                self.display,
                EGL_NO_SURFACE_VALUE,
                EGL_NO_SURFACE_VALUE,
                EGL_NO_CONTEXT_VALUE,
            );
            if !self.surface.is_null() {
                let _ = eglDestroySurface(self.display, self.surface);
                self.surface = ptr::null_mut();
            }
            if !self.context.is_null() {
                let _ = eglDestroyContext(self.display, self.context);
                self.context = ptr::null_mut();
            }
            let _ = eglTerminate(self.display);
            if !self.egl_window.is_null() {
                wl_egl_window_destroy(self.egl_window);
                self.egl_window = ptr::null_mut();
            }
        }
        self.terminated = true;
    }
}

// }}}
impl Drop for EglWindowSurface {
    fn drop(&mut self) {
        if self.terminated {
            return;
        }
        unsafe {
            let _ = eglMakeCurrent(
                self.display,
                EGL_NO_SURFACE_VALUE,
                EGL_NO_SURFACE_VALUE,
                EGL_NO_CONTEXT_VALUE,
            );
            if !self.surface.is_null() {
                let _ = eglDestroySurface(self.display, self.surface);
                self.surface = ptr::null_mut();
            }
            if !self.context.is_null() {
                let _ = eglDestroyContext(self.display, self.context);
                self.context = ptr::null_mut();
            }
            let _ = eglTerminate(self.display);
        }
        self.terminated = true;
    }
}

unsafe fn try_desktop(
    display: EGLDisplay,
    native_window: *mut c_void,
) -> Result<(EGLSurface, EGLContext), EglError> {
    unsafe {
        EglError::check(eglBindAPI(EGL_OPENGL_API), "eglBindAPI")?;
    }
    let config = unsafe { choose_config(display, EGL_OPENGL_BIT)? };
    let context_attribs = [
        EGL_CONTEXT_MAJOR_VERSION,
        3,
        EGL_CONTEXT_MINOR_VERSION,
        3,
        EGL_CONTEXT_OPENGL_PROFILE_MASK,
        EGL_CONTEXT_OPENGL_CORE_PROFILE_BIT,
        EGL_NONE,
    ];
    let context =
        unsafe { eglCreateContext(display, config, ptr::null_mut(), context_attribs.as_ptr()) };
    if context.is_null() {
        return Err(EglError::last("eglCreateContext(GL 3.3 core)"));
    }
    match unsafe { create_window_surface(display, config, native_window) } {
        Ok(surface) => Ok((surface, context)),
        Err(error) => {
            unsafe {
                let _ = eglDestroyContext(display, context);
            }
            Err(error)
        }
    }
}

unsafe fn try_gles(
    display: EGLDisplay,
    native_window: *mut c_void,
) -> Result<(EGLSurface, EGLContext), EglError> {
    let config = unsafe { choose_config(display, EGL_OPENGL_ES3_BIT_KHR)? };
    let context_attribs = [
        EGL_CONTEXT_MAJOR_VERSION,
        3,
        EGL_CONTEXT_MINOR_VERSION,
        0,
        EGL_NONE,
    ];
    let context =
        unsafe { eglCreateContext(display, config, ptr::null_mut(), context_attribs.as_ptr()) };
    if context.is_null() {
        return Err(EglError::last("eglCreateContext(GL ES 3)"));
    }
    match unsafe { create_window_surface(display, config, native_window) } {
        Ok(surface) => Ok((surface, context)),
        Err(error) => {
            unsafe {
                let _ = eglDestroyContext(display, context);
            }
            Err(error)
        }
    }
}

unsafe fn choose_config(display: EGLDisplay, renderable: EGLint) -> Result<EGLConfig, EglError> {
    let attribs = [
        EGL_SURFACE_TYPE,
        EGL_WINDOW_BIT,
        EGL_RENDERABLE_TYPE,
        renderable,
        EGL_RED_SIZE,
        8,
        EGL_GREEN_SIZE,
        8,
        EGL_BLUE_SIZE,
        8,
        EGL_ALPHA_SIZE,
        8,
        EGL_DEPTH_SIZE,
        24,
        EGL_STENCIL_SIZE,
        8,
        EGL_NONE,
    ];
    let mut config: EGLConfig = ptr::null_mut();
    let mut count: EGLint = 0;
    unsafe {
        EglError::check(
            eglChooseConfig(display, attribs.as_ptr(), &mut config, 1, &mut count),
            "eglChooseConfig",
        )?;
    }
    if count == 0 || config.is_null() {
        // Try again without depth/stencil, still RGB8.
        let fallback = [
            EGL_SURFACE_TYPE,
            EGL_WINDOW_BIT,
            EGL_RENDERABLE_TYPE,
            renderable,
            EGL_RED_SIZE,
            8,
            EGL_GREEN_SIZE,
            8,
            EGL_BLUE_SIZE,
            8,
            EGL_ALPHA_SIZE,
            8,
            EGL_NONE,
        ];
        let mut config: EGLConfig = ptr::null_mut();
        let mut count: EGLint = 0;
        unsafe {
            EglError::check(
                eglChooseConfig(display, fallback.as_ptr(), &mut config, 1, &mut count),
                "eglChooseConfig",
            )?;
        }
        if count > 0 && !config.is_null() {
            return Ok(config);
        }
        return Err(EglError {
            code: EGL_BAD_CONFIG,
            operation: "eglChooseConfig(RGB8)",
        });
    }
    Ok(config)
}

unsafe fn create_window_surface(
    display: EGLDisplay,
    config: EGLConfig,
    native_window: *mut c_void,
) -> Result<EGLSurface, EglError> {
    let surface = unsafe { eglCreateWindowSurface(display, config, native_window, ptr::null()) };
    if surface.is_null() {
        Err(EglError::last("eglCreateWindowSurface"))
    } else {
        Ok(surface)
    }
}

unsafe fn query_surface_size(
    display: EGLDisplay,
    surface: EGLSurface,
    fallback_width: u32,
    fallback_height: u32,
) -> (u32, u32) {
    let mut width: EGLint = 0;
    let mut height: EGLint = 0;
    let ok_width = unsafe { eglQuerySurface(display, surface, EGL_WIDTH, &mut width) } == EGL_TRUE;
    let ok_height =
        unsafe { eglQuerySurface(display, surface, EGL_HEIGHT, &mut height) } == EGL_TRUE;
    (
        if ok_width && width > 0 {
            width as u32
        } else {
            fallback_width.max(1)
        },
        if ok_height && height > 0 {
            height as u32
        } else {
            fallback_height.max(1)
        },
    )
}

fn query_egl_string(display: EGLDisplay, name: EGLint) -> Option<String> {
    let ptr = unsafe { eglQueryString(display, name) };
    if ptr.is_null() {
        None
    } else {
        Some(
            unsafe { CStr::from_ptr(ptr) }
                .to_string_lossy()
                .into_owned(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn egl_error_names_and_formatting() {
        assert_eq!(EglError::name(EGL_BAD_CONFIG), "EGL_BAD_CONFIG");
        assert_eq!(EglError::name(EGL_SUCCESS), "EGL_SUCCESS");
        let error = EglError {
            code: EGL_BAD_SURFACE,
            operation: "test",
        };
        assert!(error.to_string().contains("EGL_BAD_SURFACE"));
    }

    #[test]
    fn cstring_proc_names_are_valid() {
        assert_eq!(c"glClear".to_str().unwrap(), "glClear");
        assert_eq!(
            c"eglGetPlatformDisplayEXT".to_str().unwrap(),
            "eglGetPlatformDisplayEXT"
        );
    }
}
