// SPDX-License-Identifier: GPL-3.0-or-later
//! Raw EGL/OpenGL FFI for the headless backend.
//!
//! There is no `build.rs`, no bindgen and no external crate here: the handful
//! of EGL entry points are linked directly against `libEGL`, and every core GL
//! entry point is resolved through `eglGetProcAddress` into [`GlFns`]. This
//! keeps the CPU-only build completely free of EGL/GL link dependencies (the
//! whole module is behind the `gl` feature).
//!
//! # Headless context
//!
//! [`GlContext`] prefers `EGL_PLATFORM_SURFACELESS_MESA` via
//! `eglGetPlatformDisplayEXT`, falls back to `eglGetDisplay`, and uses an
//! RGB8/DEPTH24 pbuffer surface. It supports both desktop OpenGL 3.3 core and
//! OpenGL ES 3.0. When no headless display is available, construction fails
//! with a descriptive [`EglError`]; the tests turn that into a skip rather
//! than a failure.
//!
//! # Lifetime and threading
//!
//! An EGL display is process-global in Mesa, so only one [`GlContext`] should
//! be alive per process at a time. Creation and teardown are not thread safe
//! with respect to other contexts; the tests serialise access with a mutex.
//! A context is made current on the thread that created it.

use std::ffi::{c_char, c_void, CStr};
use std::fmt;
use std::ptr;

// {{{ EGL types and constants

/// Opaque `EGLDisplay`.
pub type EGLDisplay = *mut c_void;
/// Opaque `EGLConfig`.
pub type EGLConfig = *mut c_void;
/// Opaque `EGLSurface`.
pub type EGLSurface = *mut c_void;
/// Opaque `EGLContext`.
pub type EGLContext = *mut c_void;

type EGLBoolean = u32;
type EGLenum = u32;
type EGLint = i32;

const EGL_FALSE: EGLBoolean = 0;
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

const EGL_PLATFORM_SURFACELESS_MESA: EGLenum = 0x31DD;

const EGL_NONE: EGLint = 0x3038;
const EGL_DEFAULT_DISPLAY: *mut c_void = ptr::null_mut();

const EGL_SURFACE_TYPE: EGLint = 0x3033;
const EGL_PBUFFER_BIT: EGLint = 0x0001;
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
const EGL_EXTENSIONS: EGLint = 0x3055;
const EGL_VENDOR: EGLint = 0x3053;
const EGL_VERSION: EGLint = 0x3054;
const EGL_CLIENT_APIS: EGLint = 0x308D;

const EGL_OPENGL_API: EGLenum = 0x30A2;
const EGL_OPENGL_ES_API: EGLenum = 0x30A0;

const EGL_CONTEXT_MAJOR_VERSION: EGLint = 0x3098;
const EGL_CONTEXT_MINOR_VERSION: EGLint = 0x30FB;
const EGL_CONTEXT_OPENGL_PROFILE_MASK: EGLint = 0x30FD;
const EGL_CONTEXT_OPENGL_CORE_PROFILE_BIT: EGLint = 0x0001;

#[link(name = "EGL")]
unsafe extern "C" {
    fn eglGetProcAddress(name: *const c_char) -> *mut c_void;
    fn eglGetDisplay(display_id: *mut c_void) -> EGLDisplay;
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
    fn eglGetConfigAttrib(
        display: EGLDisplay,
        config: EGLConfig,
        attribute: EGLint,
        value: *mut EGLint,
    ) -> EGLBoolean;
    fn eglCreatePbufferSurface(
        display: EGLDisplay,
        config: EGLConfig,
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
    fn eglReleaseThread() -> EGLBoolean;
}

type EglGetPlatformDisplayFn = unsafe extern "C" fn(
    platform: EGLenum,
    native_display: *mut c_void,
    attrib_list: *const EGLint,
) -> EGLDisplay;

// }}}
// {{{ EGL errors

/// The value of `eglGetError()` and the operation that produced it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EglError {
    pub code: EGLint,
    pub operation: &'static str,
}

impl EglError {
    fn last(operation: &'static str) -> Self {
        // SAFETY: eglGetError has no preconditions and returns a thread-local
        // error code.
        Self {
            code: unsafe { eglGetError() },
            operation,
        }
    }

    /// Human readable name for an EGL error code.
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

    #[inline]
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
// {{{ GL constants

pub const GL_NO_ERROR: u32 = 0;
pub const GL_INVALID_ENUM: u32 = 0x0500;
pub const GL_INVALID_VALUE: u32 = 0x0501;
pub const GL_INVALID_OPERATION: u32 = 0x0502;
pub const GL_INVALID_FRAMEBUFFER_OPERATION: u32 = 0x0506;
pub const GL_OUT_OF_MEMORY: u32 = 0x0505;

pub const GL_VERSION: u32 = 0x1F02;
pub const GL_VENDOR: u32 = 0x1F00;
pub const GL_RENDERER: u32 = 0x1F01;
pub const GL_SHADING_LANGUAGE_VERSION: u32 = 0x8B8C;
pub const GL_EXTENSIONS: u32 = 0x1F03;
pub const GL_NUM_EXTENSIONS: u32 = 0x821D;

pub const GL_COLOR_BUFFER_BIT: u32 = 0x4000;
pub const GL_DEPTH_BUFFER_BIT: u32 = 0x0100;
pub const GL_BLEND: u32 = 0x0BE2;
pub const GL_ONE: u32 = 1;
pub const GL_ONE_MINUS_SRC_ALPHA: u32 = 0x0303;
pub const GL_SCISSOR_TEST: u32 = 0x0C11;
pub const GL_DEPTH_TEST: u32 = 0x0B71;
pub const GL_CULL_FACE: u32 = 0x0B44;
pub const GL_FRAMEBUFFER_SRGB: u32 = 0x8DB9;

pub const GL_TEXTURE_2D: u32 = 0x0DE1;
pub const GL_TEXTURE0: u32 = 0x84C0;
pub const GL_TEXTURE_MIN_FILTER: u32 = 0x2801;
pub const GL_TEXTURE_MAG_FILTER: u32 = 0x2800;
pub const GL_TEXTURE_WRAP_S: u32 = 0x2802;
pub const GL_TEXTURE_WRAP_T: u32 = 0x2803;
pub const GL_CLAMP_TO_EDGE: u32 = 0x812F;
pub const GL_LINEAR: u32 = 0x2601;
pub const GL_NEAREST: u32 = 0x2600;

pub const GL_RED: u32 = 0x1903;
pub const GL_RGBA: u32 = 0x1908;
pub const GL_R8: u32 = 0x8229;
pub const GL_R8UI: u32 = 0x8232;
pub const GL_RG8UI: u32 = 0x8238;
pub const GL_RED_INTEGER: u32 = 0x8D94;
pub const GL_RG_INTEGER: u32 = 0x8D98;
pub const GL_RGBA8: u32 = 0x8058;
pub const GL_UNSIGNED_BYTE: u32 = 0x1401;
pub const GL_UNPACK_ALIGNMENT: u32 = 0x0CF5;
pub const GL_PACK_ALIGNMENT: u32 = 0x0D05;

pub const GL_ARRAY_BUFFER: u32 = 0x8892;
pub const GL_UNIFORM_BUFFER: u32 = 0x8A11;
pub const GL_STATIC_DRAW: u32 = 0x88E4;
pub const GL_DYNAMIC_DRAW: u32 = 0x88E8;
pub const GL_TRIANGLE_STRIP: u32 = 0x0005;
pub const GL_TRIANGLE_FAN: u32 = 0x0006;
pub const GL_FLOAT: u32 = 0x1406;
pub const GL_FALSE: u8 = 0;
pub const GL_TRUE: u8 = 1;

pub const GL_VERTEX_SHADER: u32 = 0x8B31;
pub const GL_FRAGMENT_SHADER: u32 = 0x8B30;
pub const GL_COMPILE_STATUS: u32 = 0x8B81;
pub const GL_LINK_STATUS: u32 = 0x8B82;
pub const GL_INFO_LOG_LENGTH: u32 = 0x8B84;
pub const GL_MAX_TEXTURE_SIZE: u32 = 0x0D33;

// }}}
// {{{ GL errors

/// A non-zero `glGetError()` value and the call that produced it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GlError {
    pub code: u32,
    pub operation: &'static str,
}

impl GlError {
    pub fn name(code: u32) -> &'static str {
        match code {
            GL_NO_ERROR => "GL_NO_ERROR",
            GL_INVALID_ENUM => "GL_INVALID_ENUM",
            GL_INVALID_VALUE => "GL_INVALID_VALUE",
            GL_INVALID_OPERATION => "GL_INVALID_OPERATION",
            GL_INVALID_FRAMEBUFFER_OPERATION => "GL_INVALID_FRAMEBUFFER_OPERATION",
            GL_OUT_OF_MEMORY => "GL_OUT_OF_MEMORY",
            _ => "GL_UNKNOWN_ERROR",
        }
    }
}

impl fmt::Display for GlError {
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

impl std::error::Error for GlError {}

// }}}
// {{{ GL entry point table

macro_rules! gl_fns {
    ($( $field:ident : unsafe extern "C" fn($($arg:ty),* $(,)?) $(-> $ret:ty)? = $name:literal ),* $(,)?) => {
        /// Core GL entry points resolved with `eglGetProcAddress`.
        ///
        /// The struct is `Copy`; clone it into objects that need to delete
        /// their GL resources on drop.
        #[derive(Clone, Copy)]
        #[allow(non_snake_case)]
        pub struct GlFns {
            $(pub $field: unsafe extern "C" fn($($arg),*) $(-> $ret)?,)*
        }

        impl GlFns {
            /// Resolve every required entry point.
            ///
            /// A current EGL context must exist. Missing entry points are
            /// reported by name instead of being silently ignored.
            ///
            /// # Safety
            ///
            /// The returned pointers are only valid while the EGL context that
            /// was current during loading (or a share-group sibling) is
            /// current on the calling thread.
            pub unsafe fn load() -> Result<Self, LoadError> {
                Ok(Self {
                    $(
                        $field: {
                            let name = concat!($name, "\0");
                            // SAFETY: the C string is NUL terminated by the
                            // concat above and eglGetProcAddress accepts any
                            // valid extension/function name.
                            let proc = unsafe {
                                eglGetProcAddress(name.as_ptr() as *const c_char)
                            };
                            if proc.is_null() {
                                return Err(LoadError::Missing($name));
                            }
                            // SAFETY: EGL guarantees that a non-null result is
                            // a function pointer with the documented signature.
                            unsafe {
                                std::mem::transmute_copy::<*mut c_void, unsafe extern "C" fn($($arg),*) $(-> $ret)?>(&proc)
                            }
                        },
                    )*
                })
            }
        }
    };
}

gl_fns! {
    glGetString: unsafe extern "C" fn(u32) -> *const u8 = "glGetString",
    glGetStringi: unsafe extern "C" fn(u32, u32) -> *const u8 = "glGetStringi",
    glGetError: unsafe extern "C" fn() -> u32 = "glGetError",
    glGetIntegerv: unsafe extern "C" fn(u32, *mut i32) = "glGetIntegerv",
    glViewport: unsafe extern "C" fn(i32, i32, i32, i32) = "glViewport",
    glScissor: unsafe extern "C" fn(i32, i32, i32, i32) = "glScissor",
    glEnable: unsafe extern "C" fn(u32) = "glEnable",
    glDisable: unsafe extern "C" fn(u32) = "glDisable",
    glBlendFunc: unsafe extern "C" fn(u32, u32) = "glBlendFunc",
    glClearColor: unsafe extern "C" fn(f32, f32, f32, f32) = "glClearColor",
    glClear: unsafe extern "C" fn(u32) = "glClear",
    glFinish: unsafe extern "C" fn() = "glFinish",
    glFlush: unsafe extern "C" fn() = "glFlush",
    glPixelStorei: unsafe extern "C" fn(u32, i32) = "glPixelStorei",
    glReadPixels: unsafe extern "C" fn(i32, i32, i32, i32, u32, u32, *mut c_void) = "glReadPixels",
    glGenTextures: unsafe extern "C" fn(i32, *mut u32) = "glGenTextures",
    glBindTexture: unsafe extern "C" fn(u32, u32) = "glBindTexture",
    glTexImage2D: unsafe extern "C" fn(u32, i32, i32, i32, i32, i32, u32, u32, *const c_void) = "glTexImage2D",
    glTexSubImage2D: unsafe extern "C" fn(u32, i32, i32, i32, i32, i32, u32, u32, *const c_void) = "glTexSubImage2D",
    glTexParameteri: unsafe extern "C" fn(u32, u32, i32) = "glTexParameteri",
    glDeleteTextures: unsafe extern "C" fn(i32, *const u32) = "glDeleteTextures",
    glActiveTexture: unsafe extern "C" fn(u32) = "glActiveTexture",
    glGenVertexArrays: unsafe extern "C" fn(i32, *mut u32) = "glGenVertexArrays",
    glBindVertexArray: unsafe extern "C" fn(u32) = "glBindVertexArray",
    glDeleteVertexArrays: unsafe extern "C" fn(i32, *const u32) = "glDeleteVertexArrays",
    glGenBuffers: unsafe extern "C" fn(i32, *mut u32) = "glGenBuffers",
    glBindBuffer: unsafe extern "C" fn(u32, u32) = "glBindBuffer",
    glBufferData: unsafe extern "C" fn(u32, isize, *const c_void, u32) = "glBufferData",
    glBufferSubData: unsafe extern "C" fn(u32, isize, isize, *const c_void) = "glBufferSubData",
    glDeleteBuffers: unsafe extern "C" fn(i32, *const u32) = "glDeleteBuffers",
    glEnableVertexAttribArray: unsafe extern "C" fn(u32) = "glEnableVertexAttribArray",
    glVertexAttribPointer: unsafe extern "C" fn(u32, i32, u32, u8, i32, *const c_void) = "glVertexAttribPointer",
    glVertexAttribDivisor: unsafe extern "C" fn(u32, u32) = "glVertexAttribDivisor",
    glDrawArrays: unsafe extern "C" fn(u32, i32, i32) = "glDrawArrays",
    glDrawArraysInstanced: unsafe extern "C" fn(u32, i32, i32, i32) = "glDrawArraysInstanced",
    glCreateShader: unsafe extern "C" fn(u32) -> u32 = "glCreateShader",
    glShaderSource: unsafe extern "C" fn(u32, i32, *const *const c_char, *const i32) = "glShaderSource",
    glCompileShader: unsafe extern "C" fn(u32) = "glCompileShader",
    glGetShaderiv: unsafe extern "C" fn(u32, u32, *mut i32) = "glGetShaderiv",
    glGetShaderInfoLog: unsafe extern "C" fn(u32, i32, *mut i32, *mut c_char) = "glGetShaderInfoLog",
    glDeleteShader: unsafe extern "C" fn(u32) = "glDeleteShader",
    glCreateProgram: unsafe extern "C" fn() -> u32 = "glCreateProgram",
    glAttachShader: unsafe extern "C" fn(u32, u32) = "glAttachShader",
    glLinkProgram: unsafe extern "C" fn(u32) = "glLinkProgram",
    glGetProgramiv: unsafe extern "C" fn(u32, u32, *mut i32) = "glGetProgramiv",
    glGetProgramInfoLog: unsafe extern "C" fn(u32, i32, *mut i32, *mut c_char) = "glGetProgramInfoLog",
    glUseProgram: unsafe extern "C" fn(u32) = "glUseProgram",
    glDeleteProgram: unsafe extern "C" fn(u32) = "glDeleteProgram",
    glGetUniformLocation: unsafe extern "C" fn(u32, *const c_char) -> i32 = "glGetUniformLocation",
    glUniform1i: unsafe extern "C" fn(i32, i32) = "glUniform1i",
    glUniform1ui: unsafe extern "C" fn(i32, u32) = "glUniform1ui",
    glUniform1f: unsafe extern "C" fn(i32, f32) = "glUniform1f",
    glUniform1fv: unsafe extern "C" fn(i32, i32, *const f32) = "glUniform1fv",
    glUniform2f: unsafe extern "C" fn(i32, f32, f32) = "glUniform2f",
    glUniform3f: unsafe extern "C" fn(i32, f32, f32, f32) = "glUniform3f",
    glUniform4f: unsafe extern "C" fn(i32, f32, f32, f32, f32) = "glUniform4f",
    glUniform4fv: unsafe extern "C" fn(i32, i32, *const f32) = "glUniform4fv",
    glBindBufferBase: unsafe extern "C" fn(u32, u32, u32) = "glBindBufferBase",
    glGetUniformBlockIndex: unsafe extern "C" fn(u32, *const c_char) -> u32 = "glGetUniformBlockIndex",
    glUniformBlockBinding: unsafe extern "C" fn(u32, u32, u32) = "glUniformBlockBinding",
}

/// An entry point missing from `eglGetProcAddress`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadError {
    Missing(&'static str),
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing(name) => write!(f, "GL entry point {name} is unavailable"),
        }
    }
}

impl std::error::Error for LoadError {}

/// A parsed `GL_VERSION` value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GlVersion {
    pub major: u32,
    pub minor: u32,
}

impl fmt::Display for GlVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

impl GlFns {
    /// Drain all pending GL errors, returning the first one.
    pub fn check(&self, operation: &'static str) -> Result<(), GlError> {
        // SAFETY: glGetError has no preconditions and is valid whenever a GL
        // context is current.
        let mut first = None;
        loop {
            let code = unsafe { (self.glGetError)() };
            if code == GL_NO_ERROR {
                break;
            }
            if first.is_none() {
                first = Some(GlError { code, operation });
            }
        }
        match first {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    fn string(&self, name: u32) -> String {
        // SAFETY: glGetString returns either a NUL terminated UTF-8 string or
        // null. It is only called with a current context.
        unsafe {
            let ptr = (self.glGetString)(name);
            if ptr.is_null() {
                String::new()
            } else {
                CStr::from_ptr(ptr as *const c_char)
                    .to_string_lossy()
                    .into_owned()
            }
        }
    }

    /// `GL_VERSION`.
    pub fn version_string(&self) -> String {
        self.string(GL_VERSION)
    }

    /// `GL_SHADING_LANGUAGE_VERSION`.
    pub fn shading_language_version(&self) -> String {
        self.string(GL_SHADING_LANGUAGE_VERSION)
    }

    /// `GL_VENDOR`.
    pub fn vendor(&self) -> String {
        self.string(GL_VENDOR)
    }

    /// `GL_RENDERER`.
    pub fn renderer(&self) -> String {
        self.string(GL_RENDERER)
    }

    /// Parse the leading `major.minor` of `GL_VERSION`.
    pub fn version(&self) -> Option<GlVersion> {
        parse_version(&self.version_string())
    }

    /// Whether `GL_VERSION` reports an OpenGL ES context.
    pub fn is_gles(&self) -> bool {
        self.version_string().starts_with("OpenGL ES")
    }

    /// All advertised extensions.
    ///
    /// Core profiles expose no `GL_EXTENSIONS` string, so this uses
    /// `glGetStringi` on OpenGL 3.0+ and GLES 3.0+, falling back to the
    /// classic space-separated string otherwise.
    pub fn extensions(&self) -> Vec<String> {
        let version = self.version();
        let indexed = version.is_some_and(|v| v.major >= 3);
        if indexed {
            // SAFETY: valid with a current context; the count is bounded by
            // the driver's extension table.
            unsafe {
                let mut count = 0i32;
                (self.glGetIntegerv)(GL_NUM_EXTENSIONS, &mut count);
                let mut ans = Vec::with_capacity(count.max(0) as usize);
                for i in 0..count.max(0) as u32 {
                    let ptr = (self.glGetStringi)(GL_EXTENSIONS, i);
                    if !ptr.is_null() {
                        ans.push(
                            CStr::from_ptr(ptr as *const c_char)
                                .to_string_lossy()
                                .into_owned(),
                        );
                    }
                }
                // Ignore a GL_INVALID_ENUM from drivers that claim 3.0 but do
                // not implement indexed queries; the fallback below catches it.
                let _ = self.check("glGetStringi");
                if !ans.is_empty() {
                    return ans;
                }
            }
        }
        self.string(GL_EXTENSIONS)
            .split_whitespace()
            .map(str::to_owned)
            .collect()
    }

    /// Whether `name` is advertised by [`extensions`](Self::extensions).
    pub fn has_extension(&self, name: &str) -> bool {
        self.extensions().iter().any(|e| e == name)
    }

    /// `GL_MAX_TEXTURE_SIZE`.
    pub fn max_texture_size(&self) -> u32 {
        // SAFETY: valid with a current context.
        unsafe {
            let mut value = 0i32;
            (self.glGetIntegerv)(GL_MAX_TEXTURE_SIZE, &mut value);
            value.max(0) as u32
        }
    }
}

fn parse_version(value: &str) -> Option<GlVersion> {
    let mut numbers = value
        .split(|c: char| !c.is_ascii_digit())
        .filter(|part| !part.is_empty());
    let major = numbers.next()?.parse().ok()?;
    let minor = numbers.next().unwrap_or("0").parse().unwrap_or(0);
    Some(GlVersion { major, minor })
}

// }}}
// {{{ Context configuration

/// Which client API to request from EGL.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlApi {
    /// Desktop OpenGL (core profile by default).
    OpenGl,
    /// OpenGL ES 3.0+.
    OpenGlEs,
}

impl GlApi {
    #[inline]
    const fn renderable_bit(self) -> EGLint {
        match self {
            Self::OpenGl => EGL_OPENGL_BIT,
            Self::OpenGlEs => EGL_OPENGL_ES3_BIT_KHR,
        }
    }

    #[inline]
    const fn bind_api(self) -> EGLenum {
        match self {
            Self::OpenGl => EGL_OPENGL_API,
            Self::OpenGlEs => EGL_OPENGL_ES_API,
        }
    }

    /// Human readable name.
    pub const fn label(self) -> &'static str {
        match self {
            Self::OpenGl => "OpenGL",
            Self::OpenGlEs => "OpenGL ES",
        }
    }
}

/// Configuration for a headless pbuffer context.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ContextConfig {
    pub width: u32,
    pub height: u32,
    pub api: GlApi,
    pub major: u32,
    pub minor: u32,
    pub core_profile: bool,
    pub depth_bits: u32,
    /// Include an alpha channel in the EGL config (needed for RGBA readback
    /// of the default framebuffer).
    pub alpha: bool,
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            width: 640,
            height: 480,
            api: GlApi::OpenGl,
            major: 3,
            minor: 3,
            core_profile: true,
            depth_bits: 24,
            alpha: true,
        }
    }
}

impl ContextConfig {
    /// A headless surfaceless configuration of the given size.
    pub const fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            api: GlApi::OpenGl,
            major: 3,
            minor: 3,
            core_profile: true,
            depth_bits: 24,
            alpha: true,
        }
    }

    /// Ask for GLES 3.0 instead of desktop OpenGL.
    pub const fn gles(mut self) -> Self {
        self.api = GlApi::OpenGlEs;
        self.major = 3;
        self.minor = 0;
        self.core_profile = false;
        self
    }
}

/// Actual attributes of the chosen EGL config.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ConfigAttribs {
    pub red: u32,
    pub green: u32,
    pub blue: u32,
    pub alpha: u32,
    pub depth: u32,
    pub stencil: u32,
}

// }}}
// {{{ GlContext

/// A surfaceless EGL display, pbuffer surface and GL context.
///
/// Dropping the value makes the context non-current, destroys the surface and
/// context, and terminates the EGL display.
pub struct GlContext {
    display: EGLDisplay,
    surface: EGLSurface,
    context: EGLContext,
    egl_config: EGLConfig,
    config: ContextConfig,
    config_attribs: ConfigAttribs,
    width: u32,
    height: u32,
    egl_version: (i32, i32),
    egl_vendor: String,
    egl_version_string: String,
    egl_client_apis: String,
    display_initialized: bool,
}

impl GlContext {
    /// Create a surfaceless (or default-display fallback) pbuffer context.
    pub fn new(config: ContextConfig) -> Result<Self, EglError> {
        let width = config.width.max(1);
        let height = config.height.max(1);

        // SAFETY: all EGL calls below operate on handles that are checked for
        // null / EGL_TRUE. The unsafe block covers the complete construction so
        // every partial failure can be unwound before returning.
        unsafe {
            let get_platform = load_egl_proc::<EglGetPlatformDisplayFn>("eglGetPlatformDisplayEXT");
            let display = match get_platform {
                Some(proc) => proc(
                    EGL_PLATFORM_SURFACELESS_MESA,
                    EGL_DEFAULT_DISPLAY,
                    ptr::null(),
                ),
                None => eglGetDisplay(EGL_DEFAULT_DISPLAY),
            };
            if display.is_null() {
                return Err(EglError::last("eglGetPlatformDisplayEXT/eglGetDisplay"));
            }

            let mut egl_major = 0;
            let mut egl_minor = 0;
            if eglInitialize(display, &mut egl_major, &mut egl_minor) == EGL_FALSE {
                let error = EglError::last("eglInitialize");
                let _ = eglTerminate(display);
                return Err(error);
            }

            let attributes = [
                EGL_SURFACE_TYPE,
                EGL_PBUFFER_BIT,
                EGL_RENDERABLE_TYPE,
                config.api.renderable_bit(),
                EGL_RED_SIZE,
                8,
                EGL_GREEN_SIZE,
                8,
                EGL_BLUE_SIZE,
                8,
                EGL_ALPHA_SIZE,
                if config.alpha { 8 } else { 0 },
                EGL_DEPTH_SIZE,
                config.depth_bits as EGLint,
                EGL_STENCIL_SIZE,
                0,
                EGL_NONE,
            ];
            let mut egl_config = ptr::null_mut();
            let mut config_count = 0;
            if eglChooseConfig(
                display,
                attributes.as_ptr(),
                &mut egl_config,
                1,
                &mut config_count,
            ) == EGL_FALSE
                || config_count == 0
            {
                let mut error = EglError::last("eglChooseConfig");
                if error.code == EGL_SUCCESS {
                    error.code = EGL_BAD_CONFIG;
                }
                let _ = eglTerminate(display);
                return Err(error);
            }

            let mut attribs = ConfigAttribs::default();
            for (attribute, slot) in [
                (EGL_RED_SIZE, &mut attribs.red),
                (EGL_GREEN_SIZE, &mut attribs.green),
                (EGL_BLUE_SIZE, &mut attribs.blue),
                (EGL_ALPHA_SIZE, &mut attribs.alpha),
                (EGL_DEPTH_SIZE, &mut attribs.depth),
                (EGL_STENCIL_SIZE, &mut attribs.stencil),
            ] {
                let mut value = 0;
                if eglGetConfigAttrib(display, egl_config, attribute, &mut value) != EGL_FALSE {
                    *slot = value.max(0) as u32;
                }
            }

            let pbuffer_attributes = [
                EGL_WIDTH,
                width as EGLint,
                EGL_HEIGHT,
                height as EGLint,
                EGL_NONE,
            ];
            let surface = eglCreatePbufferSurface(display, egl_config, pbuffer_attributes.as_ptr());
            if surface.is_null() {
                let error = EglError::last("eglCreatePbufferSurface");
                let _ = eglTerminate(display);
                return Err(error);
            }

            if eglBindAPI(config.api.bind_api()) == EGL_FALSE {
                let error = EglError::last("eglBindAPI");
                let _ = eglDestroySurface(display, surface);
                let _ = eglTerminate(display);
                return Err(error);
            }

            let mut context_attributes = Vec::with_capacity(7);
            context_attributes.push(EGL_CONTEXT_MAJOR_VERSION);
            context_attributes.push(config.major as EGLint);
            context_attributes.push(EGL_CONTEXT_MINOR_VERSION);
            context_attributes.push(config.minor as EGLint);
            if config.api == GlApi::OpenGl && config.core_profile && config.major >= 3 {
                context_attributes.push(EGL_CONTEXT_OPENGL_PROFILE_MASK);
                context_attributes.push(EGL_CONTEXT_OPENGL_CORE_PROFILE_BIT);
            }
            context_attributes.push(EGL_NONE);

            let context = eglCreateContext(
                display,
                egl_config,
                ptr::null_mut(),
                context_attributes.as_ptr(),
            );
            if context.is_null() {
                let error = EglError::last("eglCreateContext");
                let _ = eglDestroySurface(display, surface);
                let _ = eglTerminate(display);
                return Err(error);
            }

            if eglMakeCurrent(display, surface, surface, context) == EGL_FALSE {
                let error = EglError::last("eglMakeCurrent");
                let _ = eglDestroyContext(display, context);
                let _ = eglDestroySurface(display, surface);
                let _ = eglTerminate(display);
                return Err(error);
            }

            let mut actual_width = width as EGLint;
            let mut actual_height = height as EGLint;
            let _ = eglQuerySurface(display, surface, EGL_WIDTH, &mut actual_width);
            let _ = eglQuerySurface(display, surface, EGL_HEIGHT, &mut actual_height);

            Ok(Self {
                display,
                surface,
                context,
                egl_config,
                config,
                config_attribs: attribs,
                width: actual_width.max(1) as u32,
                height: actual_height.max(1) as u32,
                egl_version: (egl_major, egl_minor),
                egl_vendor: egl_string(display, EGL_VENDOR),
                egl_version_string: egl_string(display, EGL_VERSION),
                egl_client_apis: egl_string(display, EGL_CLIENT_APIS),
                display_initialized: true,
            })
        }
    }

    /// Configuration used for this context.
    #[inline]
    pub const fn config(&self) -> ContextConfig {
        self.config
    }

    /// Attributes of the EGL config actually selected.
    #[inline]
    pub const fn config_attribs(&self) -> ConfigAttribs {
        self.config_attribs
    }

    /// Current pbuffer size.
    #[inline]
    pub const fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// EGL version reported by `eglInitialize`.
    #[inline]
    pub const fn egl_version(&self) -> (i32, i32) {
        self.egl_version
    }

    /// `EGL_VENDOR`.
    #[inline]
    pub fn egl_vendor(&self) -> &str {
        &self.egl_vendor
    }

    /// `EGL_VERSION`.
    #[inline]
    pub fn egl_version_string(&self) -> &str {
        &self.egl_version_string
    }

    /// `EGL_CLIENT_APIS`.
    #[inline]
    pub fn egl_client_apis(&self) -> &str {
        &self.egl_client_apis
    }

    /// Client EGL extensions (`eglQueryString(EGL_NO_DISPLAY, ...)`).
    pub fn egl_client_extensions(&self) -> Vec<String> {
        // SAFETY: eglQueryString with EGL_NO_DISPLAY is valid after a
        // successful eglInitialize.
        let value = unsafe { egl_string(EGL_DEFAULT_DISPLAY, EGL_EXTENSIONS) };
        value.split_whitespace().map(str::to_owned).collect()
    }

    /// Display EGL extensions.
    pub fn egl_display_extensions(&self) -> Vec<String> {
        // SAFETY: the display is initialized for the lifetime of self.
        let value = unsafe { egl_string(self.display, EGL_EXTENSIONS) };
        value.split_whitespace().map(str::to_owned).collect()
    }

    /// Make this context current on the calling thread.
    pub fn make_current(&self) -> Result<(), EglError> {
        // SAFETY: the handles are valid for the lifetime of self.
        EglError::check(
            unsafe { eglMakeCurrent(self.display, self.surface, self.surface, self.context) },
            "eglMakeCurrent",
        )
    }

    /// Present the pbuffer surface.
    ///
    /// Pbuffers are not displayed; this mainly exists for API completeness and
    /// for callers that later switch to a window surface.
    pub fn swap_buffers(&self) -> Result<(), EglError> {
        // SAFETY: the handles are valid for the lifetime of self.
        EglError::check(
            unsafe { eglSwapBuffers(self.display, self.surface) },
            "eglSwapBuffers",
        )
    }

    /// Resize the pbuffer, recreating the surface while keeping the context.
    pub fn resize(&mut self, width: u32, height: u32) -> Result<(), EglError> {
        let width = width.max(1);
        let height = height.max(1);
        // SAFETY: all handles are valid; the old surface is detached before it
        // is destroyed and the new one is checked before it is stored.
        unsafe {
            let _ = eglMakeCurrent(
                self.display,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
            );
            if !self.surface.is_null() {
                EglError::check(
                    eglDestroySurface(self.display, self.surface),
                    "eglDestroySurface",
                )?;
                self.surface = ptr::null_mut();
            }
            let attributes = [
                EGL_WIDTH,
                width as EGLint,
                EGL_HEIGHT,
                height as EGLint,
                EGL_NONE,
            ];
            let surface =
                eglCreatePbufferSurface(self.display, self.egl_config, attributes.as_ptr());
            if surface.is_null() {
                return Err(EglError::last("eglCreatePbufferSurface"));
            }
            self.surface = surface;
            EglError::check(
                eglMakeCurrent(self.display, surface, surface, self.context),
                "eglMakeCurrent",
            )?;
            self.width = width;
            self.height = height;
            Ok(())
        }
    }

    /// Read the default framebuffer as tightly packed RGBA8.
    ///
    /// Rows are returned in OpenGL's bottom-up order. The backend flips them
    /// into the top-left convention used by [`crate::geom::Rect`].
    pub fn read_pixels(&self, gl: &GlFns) -> Result<Vec<u8>, GlError> {
        let (width, height) = self.size();
        let mut pixels = vec![0u8; width as usize * height as usize * 4];
        // SAFETY: pixels is large enough for width*height RGBA8 pixels and the
        // context is current (the caller owns the backend lifecycle).
        unsafe {
            (gl.glPixelStorei)(GL_PACK_ALIGNMENT, 1);
            (gl.glReadPixels)(
                0,
                0,
                width as i32,
                height as i32,
                GL_RGBA,
                GL_UNSIGNED_BYTE,
                pixels.as_mut_ptr() as *mut c_void,
            );
            (gl.glFinish)();
        }
        gl.check("glReadPixels")?;
        Ok(pixels)
    }
}

impl Drop for GlContext {
    fn drop(&mut self) {
        // SAFETY: the handles below were created by this context and are only
        // destroyed once. Making no context current first is required before
        // destroying the current surface/context.
        unsafe {
            let _ = eglMakeCurrent(
                self.display,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
            );
            if !self.surface.is_null() {
                let _ = eglDestroySurface(self.display, self.surface);
                self.surface = ptr::null_mut();
            }
            if !self.context.is_null() {
                let _ = eglDestroyContext(self.display, self.context);
                self.context = ptr::null_mut();
            }
            if self.display_initialized {
                let _ = eglTerminate(self.display);
                self.display_initialized = false;
            }
            let _ = eglReleaseThread();
        }
    }
}

impl fmt::Debug for GlContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GlContext")
            .field("api", &self.config.api)
            .field("requested_version", &(self.config.major, self.config.minor))
            .field("config_attribs", &self.config_attribs)
            .field("size", &self.size())
            .field("egl_version", &self.egl_version)
            .finish_non_exhaustive()
    }
}

unsafe fn egl_string(display: EGLDisplay, name: EGLint) -> String {
    // SAFETY: the caller guarantees that display is initialized or
    // EGL_NO_DISPLAY, both accepted by eglQueryString.
    let ptr = unsafe { eglQueryString(display, name) };
    if ptr.is_null() {
        String::new()
    } else {
        // SAFETY: eglQueryString returns a NUL terminated string.
        unsafe { CStr::from_ptr(ptr).to_string_lossy().into_owned() }
    }
}

fn load_egl_proc<T: Copy>(name: &str) -> Option<T> {
    let name = std::ffi::CString::new(name).ok()?;
    // SAFETY: the name is a NUL terminated C string.
    let proc = unsafe { eglGetProcAddress(name.as_ptr()) };
    if proc.is_null() {
        None
    } else {
        // SAFETY: EGL guarantees that a non-null result is a function pointer
        // of the requested type.
        Some(unsafe { std::mem::transmute_copy::<*mut c_void, T>(&proc) })
    }
}

// }}}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_parser() {
        assert_eq!(
            parse_version("4.6 (Core Profile) Mesa 26.1.1"),
            Some(GlVersion { major: 4, minor: 6 })
        );
        assert_eq!(
            parse_version("OpenGL ES 3.2 Mesa 22.0"),
            Some(GlVersion { major: 3, minor: 2 })
        );
        assert_eq!(parse_version("garbage"), None);
    }

    #[test]
    fn error_names_are_not_empty() {
        assert_eq!(GlError::name(GL_INVALID_ENUM), "GL_INVALID_ENUM");
        assert_eq!(EglError::name(EGL_BAD_SURFACE), "EGL_BAD_SURFACE");
    }
}
