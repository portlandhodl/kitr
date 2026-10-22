// SPDX-License-Identifier: GPL-3.0-or-later
//! Headless OpenGL backend for the CPU instance stream.
//!
//! [`GlBackend`] owns two shader programs, one shared VAO/VBO pair and a GPU
//! texture per atlas page. [`GlBackend::new`] creates a headless EGL pbuffer
//! context; [`GlBackend::with_loader`] instead draws into a caller-owned
//! current EGL context (for example a window surface supplied by a windowing
//! crate). It consumes the [`Frame`] produced by [`crate::RendererCore`]:
//!
//! ```no_run
//! # #[cfg(feature = "gl")] {
//! use kitty_render::gl_backend::{GlBackend, GlBackendConfig};
//! use kitty_render::{BasicColorResolver, RenderConfig, RendererCore};
//! use kitty_screen::Screen;
//!
//! let mut screen = Screen::builder(80, 24).build().unwrap();
//! screen.draw_text(&[b'x' as u32]);
//! let resolver = BasicColorResolver::default();
//! let mut rasterizer = |_key: &kitty_render::GlyphKey| {
//!     kitty_render::GlyphBitmap::gray(6, 12, vec![0x80; 72]).ok()
//! };
//! let mut core = RendererCore::default();
//! let frame = core.prepare_frame(&screen, &mut rasterizer, &resolver);
//!
//! let mut backend = GlBackend::new(GlBackendConfig::default()).unwrap();
//! backend.sync_atlas(&core.atlas, &frame).unwrap();
//! backend.render(&frame, &RenderConfig::default()).unwrap();
//! let pixels = backend.read_pixels().unwrap();
//! # }
//! ```
//!
//! # Atlas uploads
//!
//! [`render`](GlBackend::render) only knows about [`Frame`] instances. Atlas
//! pixel data lives in [`GlyphAtlas`], so call [`sync_atlas`](GlBackend::sync_atlas)
//! (or the convenience [`render_with_atlas`](GlBackend::render_with_atlas))
//! before rendering. Only pages touched by `Frame::atlas_updates` or
//! `Frame::atlas_events` are uploaded, plus pages that are referenced by a
//! glyph but have no GPU texture yet. Textures are reused across frames.
//!
//! # Damage
//!
//! A full redraw clears the whole viewport. A partial frame sets the scissor
//! box to each damaged row range, clears that box to the default background
//! and replays all draw calls clipped to it. This is simple and correct, at
//! the cost of re-issuing every instance for each damaged range. Cursor-only
//! damage arrives as the two cursor row ranges from [`crate::DamageTracker`].
//!
//! # Supported subset
//!
//! * Instanced background, glyph, decoration and cursor quads;
//! * Gray8 atlas pages (`GL_R8`) and RGBA8 pages (`GL_RGBA8`);
//! * nearest or linear atlas filtering;
//! * premultiplied alpha blending;
//! * scissor-based damage, viewport, clear, offscreen `glReadPixels`;
//! * optional gamma conversion in the fragment shader (`GammaMode`).
//!
//! # Not supported yet
//!
//! Graphics-protocol images, background images, animations, cursor trails,
//! window logos, selection/hyperlink overlays, custom user shaders, shaped-run
//! sprite caches and multi-format colour glyphs (COLR/CBDT/SVG). Blink and
//! strikethrough style flags are carried by the CPU instances but blinking is
//! not animated here; dim is implemented as a 50% alpha reduction.

use std::collections::{BTreeSet, HashMap};
use std::ffi::{c_char, c_void, CString};
use std::fmt;
use std::mem::size_of;
use std::ptr;

use crate::atlas::{AtlasEvent, AtlasFormat, GlyphAtlas};
use crate::colors::{linear_to_srgb, srgb_to_linear, Rgba8};
use crate::egl::{
    ConfigAttribs, ContextConfig, EglError, GlApi, GlContext, GlError, GlFns, GlVersion, LoadError,
    GL_ARRAY_BUFFER, GL_BLEND, GL_CLAMP_TO_EDGE, GL_COLOR_BUFFER_BIT, GL_COMPILE_STATUS,
    GL_CULL_FACE, GL_DEPTH_TEST, GL_DYNAMIC_DRAW, GL_FALSE, GL_FLOAT, GL_FRAGMENT_SHADER,
    GL_INFO_LOG_LENGTH, GL_LINEAR, GL_LINK_STATUS, GL_NEAREST, GL_ONE, GL_ONE_MINUS_SRC_ALPHA,
    GL_PACK_ALIGNMENT, GL_R8, GL_RED, GL_RGBA, GL_RGBA8, GL_SCISSOR_TEST, GL_STATIC_DRAW,
    GL_TEXTURE0, GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_TEXTURE_MIN_FILTER, GL_TEXTURE_WRAP_S,
    GL_TEXTURE_WRAP_T, GL_TRIANGLE_STRIP, GL_UNPACK_ALIGNMENT, GL_UNSIGNED_BYTE, GL_VERTEX_SHADER,
};
use crate::geom::Rect;
use crate::gl_shaders;
use crate::instances::{CursorGeometry, Frame, RenderConfig};

/// Texture filtering for atlas pages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilterMode {
    Nearest,
    Linear,
}

impl FilterMode {
    #[inline]
    const fn gl(self) -> u32 {
        match self {
            Self::Nearest => GL_NEAREST,
            Self::Linear => GL_LINEAR,
        }
    }
}

/// Optional per-fragment transfer conversion.
///
/// This only changes the shader's per-fragment transfer function. Blending is
/// still premultiplied in the target's stored space; neither mode creates an
/// sRGB framebuffer or a linear resolve. Use `Linear` with a linear target and
/// `Srgb` with an sRGB-encoded target.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GammaMode {
    /// Write the supplied sRGB values unchanged (default).
    #[default]
    None,
    /// Convert sRGB input to linear light; use with a linear framebuffer.
    Linear,
    /// Encode linear input as sRGB; use with an sRGB framebuffer.
    Srgb,
}

impl GammaMode {
    #[inline]
    const fn uniform(self) -> i32 {
        match self {
            Self::None => 0,
            Self::Linear => 1,
            Self::Srgb => 2,
        }
    }
}

/// Backend and context configuration.
#[derive(Clone, Debug, PartialEq)]
pub struct GlBackendConfig {
    pub context: ContextConfig,
    pub gamma: GammaMode,
    pub atlas_filter: FilterMode,
    /// Corner radius for block/beam/underline cursor rectangles. `0.0` draws
    /// plain rectangles.
    pub cursor_radius: f32,
}

impl Default for GlBackendConfig {
    fn default() -> Self {
        Self {
            context: ContextConfig::default(),
            gamma: GammaMode::None,
            atlas_filter: FilterMode::Linear,
            cursor_radius: 0.0,
        }
    }
}

impl GlBackendConfig {
    /// Configuration for a headless context of exactly `width` x `height`.
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            context: ContextConfig::new(width, height),
            ..Self::default()
        }
    }
}

/// Statistics from one [`GlBackend::render`] call.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RenderStats {
    pub instances: usize,
    pub draw_calls: usize,
    pub scissor_rects: usize,
    pub glyphs_skipped: usize,
    pub cursor_instances: usize,
}

/// Statistics from one [`GlBackend::sync_atlas`] call.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AtlasSyncStats {
    pub pages_created: usize,
    pub pages_uploaded: usize,
    pub pages_removed: usize,
}

/// Errors produced by the OpenGL backend.
#[derive(Debug)]
pub enum GlBackendError {
    Egl(EglError),
    Load(LoadError),
    Gl(GlError),
    Shader {
        stage: &'static str,
        log: String,
    },
    Link {
        log: String,
    },
    UnsupportedVersion {
        required: &'static str,
        actual: String,
    },
}

impl fmt::Display for GlBackendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Egl(error) => write!(f, "EGL error: {error}"),
            Self::Load(error) => write!(f, "GL loader error: {error}"),
            Self::Gl(error) => write!(f, "GL error: {error}"),
            Self::Shader { stage, log } => {
                write!(f, "{stage} shader compilation failed: {log}")
            }
            Self::Link { log } => write!(f, "shader program link failed: {log}"),
            Self::UnsupportedVersion { required, actual } => write!(
                f,
                "unsupported GL version {actual}; this backend needs {required}"
            ),
        }
    }
}

impl std::error::Error for GlBackendError {}

impl From<EglError> for GlBackendError {
    fn from(value: EglError) -> Self {
        Self::Egl(value)
    }
}

impl From<LoadError> for GlBackendError {
    fn from(value: LoadError) -> Self {
        Self::Load(value)
    }
}

impl From<GlError> for GlBackendError {
    fn from(value: GlError) -> Self {
        Self::Gl(value)
    }
}

// {{{ GPU resources

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
struct QuadInstance {
    rect: [f32; 4],
    uv: [f32; 4],
    color: [f32; 4],
    params: [f32; 4],
}

const QUAD_INSTANCE_STRIDE: i32 = size_of::<QuadInstance>() as i32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BatchKind {
    Solid,
    Glyph { page: u32, is_color: bool },
}

#[derive(Clone, Copy, Debug)]
struct DrawCall {
    kind: BatchKind,
    first: i32,
    count: i32,
}

#[derive(Clone, Copy, Debug)]
struct AtlasTexture {
    texture: u32,
    format: AtlasFormat,
    width: u32,
    height: u32,
}

#[derive(Clone, Copy, Debug)]
struct ScissorRect {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

struct Programs {
    gl: GlFns,
    glyph: u32,
    solid: u32,
    glyph_viewport: i32,
    glyph_atlas: i32,
    glyph_is_color: i32,
    glyph_gamma: i32,
    solid_viewport: i32,
    solid_gamma: i32,
}

impl Programs {
    fn new(gl: &GlFns, api: GlApi) -> Result<Self, GlBackendError> {
        let glyph_sources = gl_shaders::glyph_sources(api);
        let glyph_vs = compile_shader(gl, GL_VERTEX_SHADER, "glyph vertex", &glyph_sources.vertex)?;
        let glyph_fs = match compile_shader(
            gl,
            GL_FRAGMENT_SHADER,
            "glyph fragment",
            &glyph_sources.fragment,
        ) {
            Ok(id) => id,
            Err(error) => {
                unsafe { (gl.glDeleteShader)(glyph_vs) };
                return Err(error);
            }
        };
        let glyph = link_program(gl, glyph_vs, glyph_fs, "glyph")?;

        let solid_sources = gl_shaders::solid_sources(api);
        let solid_vs =
            match compile_shader(gl, GL_VERTEX_SHADER, "solid vertex", &solid_sources.vertex) {
                Ok(id) => id,
                Err(error) => {
                    unsafe {
                        (gl.glDeleteProgram)(glyph);
                    }
                    return Err(error);
                }
            };
        let solid_fs = match compile_shader(
            gl,
            GL_FRAGMENT_SHADER,
            "solid fragment",
            &solid_sources.fragment,
        ) {
            Ok(id) => id,
            Err(error) => {
                unsafe {
                    (gl.glDeleteShader)(solid_vs);
                    (gl.glDeleteProgram)(glyph);
                }
                return Err(error);
            }
        };
        let solid = match link_program(gl, solid_vs, solid_fs, "solid") {
            Ok(id) => id,
            Err(error) => {
                unsafe { (gl.glDeleteProgram)(glyph) };
                return Err(error);
            }
        };

        Ok(Self {
            gl: *gl,
            glyph,
            solid,
            glyph_viewport: uniform_location(gl, glyph, "u_viewport"),
            glyph_atlas: uniform_location(gl, glyph, "u_atlas"),
            glyph_is_color: uniform_location(gl, glyph, "u_is_color"),
            glyph_gamma: uniform_location(gl, glyph, "u_gamma"),
            solid_viewport: uniform_location(gl, solid, "u_viewport"),
            solid_gamma: uniform_location(gl, solid, "u_gamma"),
        })
    }

    fn destroy(&mut self) {
        // SAFETY: the ids are owned by this struct and the context is still
        // current during backend teardown.
        unsafe {
            if self.glyph != 0 {
                (self.gl.glDeleteProgram)(self.glyph);
                self.glyph = 0;
            }
            if self.solid != 0 {
                (self.gl.glDeleteProgram)(self.solid);
                self.solid = 0;
            }
        }
    }
}

impl Drop for Programs {
    fn drop(&mut self) {
        self.destroy();
    }
}

fn compile_shader(
    gl: &GlFns,
    stage: u32,
    stage_name: &'static str,
    source: &str,
) -> Result<u32, GlBackendError> {
    // SAFETY: source is valid for the duration of glShaderSource; the shader
    // id is owned by the caller after a successful return.
    unsafe {
        let shader = (gl.glCreateShader)(stage);
        if shader == 0 {
            return Err(GlError {
                code: 1,
                operation: "glCreateShader",
            }
            .into());
        }
        let source_ptr = source.as_ptr() as *const c_char;
        let length = source.len() as i32;
        (gl.glShaderSource)(shader, 1, &source_ptr, &length);
        (gl.glCompileShader)(shader);

        let mut status = 0;
        (gl.glGetShaderiv)(shader, GL_COMPILE_STATUS, &mut status);
        if status == 0 {
            let log = shader_info_log(gl, shader);
            (gl.glDeleteShader)(shader);
            return Err(GlBackendError::Shader {
                stage: stage_name,
                log,
            });
        }
        Ok(shader)
    }
}

fn link_program(
    gl: &GlFns,
    vertex: u32,
    fragment: u32,
    name: &'static str,
) -> Result<u32, GlBackendError> {
    // SAFETY: both shader ids are valid and owned by the caller; the program
    // id is owned by the caller after a successful return.
    unsafe {
        let program = (gl.glCreateProgram)();
        if program == 0 {
            (gl.glDeleteShader)(vertex);
            (gl.glDeleteShader)(fragment);
            return Err(GlError {
                code: 1,
                operation: "glCreateProgram",
            }
            .into());
        }
        (gl.glAttachShader)(program, vertex);
        (gl.glAttachShader)(program, fragment);
        (gl.glLinkProgram)(program);
        (gl.glDeleteShader)(vertex);
        (gl.glDeleteShader)(fragment);

        let mut status = 0;
        (gl.glGetProgramiv)(program, GL_LINK_STATUS, &mut status);
        if status == 0 {
            let log = program_info_log(gl, program);
            (gl.glDeleteProgram)(program);
            return Err(GlBackendError::Link {
                log: format!("{name}: {log}"),
            });
        }
        Ok(program)
    }
}

fn shader_info_log(gl: &GlFns, shader: u32) -> String {
    // SAFETY: the shader id is valid and the context current.
    unsafe {
        let mut length = 0;
        (gl.glGetShaderiv)(shader, GL_INFO_LOG_LENGTH, &mut length);
        if length <= 1 {
            return String::new();
        }
        let mut buffer = vec![0u8; length as usize];
        let mut written = 0;
        (gl.glGetShaderInfoLog)(
            shader,
            length,
            &mut written,
            buffer.as_mut_ptr() as *mut c_char,
        );
        c_buffer_to_string(buffer, written)
    }
}

fn program_info_log(gl: &GlFns, program: u32) -> String {
    // SAFETY: the program id is valid and the context current.
    unsafe {
        let mut length = 0;
        (gl.glGetProgramiv)(program, GL_INFO_LOG_LENGTH, &mut length);
        if length <= 1 {
            return String::new();
        }
        let mut buffer = vec![0u8; length as usize];
        let mut written = 0;
        (gl.glGetProgramInfoLog)(
            program,
            length,
            &mut written,
            buffer.as_mut_ptr() as *mut c_char,
        );
        c_buffer_to_string(buffer, written)
    }
}

fn c_buffer_to_string(mut buffer: Vec<u8>, written: i32) -> String {
    let end = (written.max(0) as usize).min(buffer.len());
    buffer.truncate(end);
    String::from_utf8_lossy(&buffer)
        .trim_end_matches('\0')
        .to_owned()
}

fn uniform_location(gl: &GlFns, program: u32, name: &str) -> i32 {
    let Ok(name) = CString::new(name) else {
        return -1;
    };
    // SAFETY: program is valid and the name is a NUL terminated C string.
    unsafe { (gl.glGetUniformLocation)(program, name.as_ptr()) }
}

unsafe fn create_geometry(gl: &GlFns) -> Result<(u32, u32, u32), GlError> {
    let quad: [f32; 8] = [0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 1.0];
    let mut vao = 0u32;
    // SAFETY: the caller guarantees a current context; generated ids are
    // returned to the caller which owns them.
    unsafe {
        (gl.glGenVertexArrays)(1, &mut vao);
        if vao == 0 {
            return Err(GlError {
                code: 1,
                operation: "glGenVertexArrays",
            });
        }
        let mut quad_vbo = 0u32;
        (gl.glGenBuffers)(1, &mut quad_vbo);
        if quad_vbo == 0 {
            (gl.glDeleteVertexArrays)(1, &vao);
            return Err(GlError {
                code: 1,
                operation: "glGenBuffers",
            });
        }
        let mut instance_vbo = 0u32;
        (gl.glGenBuffers)(1, &mut instance_vbo);
        if instance_vbo == 0 {
            (gl.glDeleteBuffers)(1, &quad_vbo);
            (gl.glDeleteVertexArrays)(1, &vao);
            return Err(GlError {
                code: 1,
                operation: "glGenBuffers",
            });
        }

        (gl.glBindVertexArray)(vao);
        (gl.glBindBuffer)(GL_ARRAY_BUFFER, quad_vbo);
        (gl.glBufferData)(
            GL_ARRAY_BUFFER,
            size_of::<[f32; 8]>() as isize,
            quad.as_ptr() as *const c_void,
            GL_STATIC_DRAW,
        );
        (gl.glEnableVertexAttribArray)(0);
        (gl.glVertexAttribPointer)(0, 2, GL_FLOAT, GL_FALSE, 0, ptr::null());

        (gl.glBindBuffer)(GL_ARRAY_BUFFER, instance_vbo);
        for (location, components, offset) in [(1, 4, 0), (2, 4, 16), (3, 4, 32), (4, 4, 48)] {
            (gl.glEnableVertexAttribArray)(location);
            (gl.glVertexAttribPointer)(
                location,
                components,
                GL_FLOAT,
                GL_FALSE,
                QUAD_INSTANCE_STRIDE,
                offset as usize as *const c_void,
            );
            (gl.glVertexAttribDivisor)(location, 1);
        }
        (gl.glBindVertexArray)(0);
        (gl.glBindBuffer)(GL_ARRAY_BUFFER, 0);

        gl.check("create_geometry")?;
        Ok((vao, quad_vbo, instance_vbo))
    }
}

// }}}
// {{{ Backend

/// A headless or caller-owned OpenGL renderer.
pub struct GlBackend {
    config: GlBackendConfig,
    gl: GlFns,
    programs: Programs,
    vao: u32,
    quad_vbo: u32,
    instance_vbo: u32,
    instance_capacity: usize,
    instances: Vec<QuadInstance>,
    draw_calls: Vec<DrawCall>,
    glyph_order: Vec<(u32, usize)>,
    atlas_pages: HashMap<u32, AtlasTexture>,
    width: u32,
    height: u32,
    last_stats: RenderStats,
    // Keep the context last: Rust drops fields in declaration order, so all
    // GL resources are released while an owned context is still alive.
    // `None` means the caller owns and manages the current EGL context.
    context: Option<GlContext>,
}

impl GlBackend {
    /// Create a headless pbuffer context and all GPU resources.
    pub fn new(config: GlBackendConfig) -> Result<Self, GlBackendError> {
        let context = GlContext::new(config.context)?;
        context.make_current()?;
        // SAFETY: a context is current.
        let gl = unsafe { GlFns::load()? };
        let (width, height) = context.size();
        Self::from_parts(gl, width, height, config, Some(context))
    }

    /// Build a backend that draws into the caller's current EGL surface.
    ///
    /// The caller must have made its EGL context current on this thread and
    /// must keep it current whenever [`render`](Self::render),
    /// [`sync_atlas`](Self::sync_atlas), [`clear`](Self::clear) or
    /// [`read_pixels`](Self::read_pixels) is called, and while this value is
    /// dropped.
    pub fn with_loader(gl: GlFns, width: u32, height: u32) -> Result<Self, GlBackendError> {
        Self::with_loader_config(gl, width, height, GlBackendConfig::default())
    }

    /// [`with_loader`](Self::with_loader) with an explicit configuration.
    pub fn with_loader_config(
        gl: GlFns,
        width: u32,
        height: u32,
        config: GlBackendConfig,
    ) -> Result<Self, GlBackendError> {
        Self::from_parts(gl, width.max(1), height.max(1), config, None)
    }

    fn from_parts(
        gl: GlFns,
        width: u32,
        height: u32,
        config: GlBackendConfig,
        context: Option<GlContext>,
    ) -> Result<Self, GlBackendError> {
        gl.check("load entry points")?;

        let version = gl
            .version()
            .ok_or_else(|| GlBackendError::UnsupportedVersion {
                required: match config.context.api {
                    GlApi::OpenGl => "OpenGL 3.3 core",
                    GlApi::OpenGlEs => "OpenGL ES 3.0",
                },
                actual: gl.version_string(),
            })?;
        let supported = match config.context.api {
            GlApi::OpenGl => version >= (GlVersion { major: 3, minor: 3 }),
            GlApi::OpenGlEs => version >= (GlVersion { major: 3, minor: 0 }),
        };
        if !supported {
            return Err(GlBackendError::UnsupportedVersion {
                required: match config.context.api {
                    GlApi::OpenGl => "OpenGL 3.3 core",
                    GlApi::OpenGlEs => "OpenGL ES 3.0",
                },
                actual: gl.version_string(),
            });
        }

        let programs = Programs::new(&gl, config.context.api)?;
        // SAFETY: the caller guarantees a current context.
        let (vao, quad_vbo, instance_vbo) = unsafe { create_geometry(&gl)? };
        let width = width.max(1);
        let height = height.max(1);
        // SAFETY: setting state for the current context.
        unsafe {
            (gl.glViewport)(0, 0, width as i32, height as i32);
            (gl.glDisable)(GL_DEPTH_TEST);
            (gl.glDisable)(GL_CULL_FACE);
            (gl.glDisable)(GL_SCISSOR_TEST);
            (gl.glEnable)(GL_BLEND);
            (gl.glBlendFunc)(GL_ONE, GL_ONE_MINUS_SRC_ALPHA);
            (gl.glPixelStorei)(GL_UNPACK_ALIGNMENT, 4);
        }
        gl.check("initialise state")?;

        Ok(Self {
            config,
            gl,
            programs,
            vao,
            quad_vbo,
            instance_vbo,
            instance_capacity: 0,
            instances: Vec::new(),
            draw_calls: Vec::new(),
            glyph_order: Vec::new(),
            atlas_pages: HashMap::new(),
            width,
            height,
            last_stats: RenderStats::default(),
            context,
        })
    }

    /// The underlying EGL context, if this backend owns one.
    #[inline]
    pub fn context(&self) -> Option<&GlContext> {
        self.context.as_ref()
    }

    fn ensure_current(&self) -> Result<(), GlBackendError> {
        if let Some(context) = &self.context {
            context.make_current()?;
        }
        Ok(())
    }

    /// Current drawable size in pixels.
    #[inline]
    pub const fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Parsed `GL_VERSION`.
    pub fn gl_version(&self) -> Option<GlVersion> {
        self.gl.version()
    }

    /// `GL_VERSION`.
    pub fn gl_version_string(&self) -> String {
        self.gl.version_string()
    }

    /// `GL_RENDERER`.
    pub fn renderer(&self) -> String {
        self.gl.renderer()
    }

    /// `GL_VENDOR`.
    pub fn vendor(&self) -> String {
        self.gl.vendor()
    }

    /// `GL_SHADING_LANGUAGE_VERSION`.
    pub fn shading_language_version(&self) -> String {
        self.gl.shading_language_version()
    }

    /// All advertised GL extensions.
    pub fn extensions(&self) -> Vec<String> {
        self.gl.extensions()
    }

    /// Attributes of the selected EGL config.
    pub fn config_attribs(&self) -> ConfigAttribs {
        self.context
            .as_ref()
            .map(GlContext::config_attribs)
            .unwrap_or_default()
    }

    /// Statistics from the most recent render.
    #[inline]
    pub const fn last_stats(&self) -> RenderStats {
        self.last_stats
    }

    /// Number of live GPU atlas textures.
    #[inline]
    pub fn atlas_texture_count(&self) -> usize {
        self.atlas_pages.len()
    }

    /// Resize the owned pbuffer (if any) and keep the GPU resources.
    ///
    /// A caller-owned window surface is resized by the caller; this only
    /// updates the viewport and scissor geometry it uses.
    pub fn resize(&mut self, width: u32, height: u32) -> Result<(), GlBackendError> {
        if let Some(context) = &mut self.context {
            context.resize(width, height)?;
            context.make_current()?;
            let (width, height) = context.size();
            self.set_size(width, height)?;
        } else {
            self.set_size(width, height)?;
        }
        Ok(())
    }

    /// Set the drawable size without touching an owned surface.
    ///
    /// Use this after resizing a caller-provided EGL window surface.
    pub fn set_size(&mut self, width: u32, height: u32) -> Result<(), GlBackendError> {
        self.width = width.max(1);
        self.height = height.max(1);
        self.ensure_current()?;
        // SAFETY: a context is current.
        unsafe {
            (self.gl.glViewport)(0, 0, self.width as i32, self.height as i32);
        }
        self.gl.check("resize")?;
        Ok(())
    }

    /// Clear the whole drawable to `color`.
    pub fn clear(&mut self, color: Rgba8) -> Result<(), GlBackendError> {
        self.ensure_current()?;
        let color = self.storage_color(color);
        // SAFETY: a context is current and the viewport matches the pbuffer.
        unsafe {
            (self.gl.glViewport)(0, 0, self.width as i32, self.height as i32);
            (self.gl.glDisable)(GL_SCISSOR_TEST);
            (self.gl.glClearColor)(color[0], color[1], color[2], color[3]);
            (self.gl.glClear)(GL_COLOR_BUFFER_BIT);
        }
        self.gl.check("glClear")?;
        Ok(())
    }

    /// Upload atlas pages that changed since the last sync.
    ///
    /// New pages are created with `glTexImage2D`; existing pages are refreshed
    /// with `glTexSubImage2D`. Pages that disappeared from the atlas (for
    /// example after [`GlyphAtlas::clear`]) have their textures deleted.
    pub fn sync_atlas(
        &mut self,
        atlas: &GlyphAtlas,
        frame: &Frame,
    ) -> Result<AtlasSyncStats, GlBackendError> {
        self.ensure_current()?;
        let mut stats = AtlasSyncStats::default();

        let mut changed: BTreeSet<u32> = BTreeSet::new();
        for slot in &frame.atlas_updates {
            changed.insert(slot.page);
        }
        for event in &frame.atlas_events {
            match event {
                AtlasEvent::NewPage { page, .. } | AtlasEvent::EvictedPage { page, .. } => {
                    changed.insert(*page);
                }
                AtlasEvent::Full { .. } => {}
            }
        }
        for glyph in &frame.glyphs {
            if let Some(sprite) = glyph.sprite {
                if !self.atlas_pages.contains_key(&sprite.page) {
                    changed.insert(sprite.page);
                }
            }
        }

        let page_count = atlas.pages().len() as u32;
        let stale: Vec<u32> = self
            .atlas_pages
            .keys()
            .copied()
            .filter(|page| *page >= page_count)
            .collect();
        for page in stale {
            if let Some(texture) = self.atlas_pages.remove(&page) {
                // SAFETY: the texture is owned by this backend.
                unsafe { (self.gl.glDeleteTextures)(1, &texture.texture) };
                stats.pages_removed += 1;
            }
        }

        if changed.is_empty() {
            return Ok(stats);
        }

        // Gray8 pages are tightly packed; RGBA8 is naturally 4-byte aligned.
        // SAFETY: a context is current and all data pointers below point at
        // the atlas page for the duration of the upload call.
        unsafe {
            (self.gl.glPixelStorei)(GL_UNPACK_ALIGNMENT, 1);
            (self.gl.glActiveTexture)(GL_TEXTURE0);
        }
        for page_index in changed {
            let Some(page) = atlas.page(page_index as usize) else {
                continue;
            };
            let width = page.width();
            let height = page.height();
            let format = page.format();
            if width == 0 || height == 0 {
                continue;
            }
            let recreate = self.atlas_pages.get(&page_index).is_none_or(|texture| {
                texture.format != format || texture.width != width || texture.height != height
            });

            if recreate {
                if let Some(old) = self.atlas_pages.remove(&page_index) {
                    // SAFETY: the texture is owned by this backend.
                    unsafe { (self.gl.glDeleteTextures)(1, &old.texture) };
                }
                let mut texture = 0u32;
                // SAFETY: a context is current.
                unsafe {
                    (self.gl.glGenTextures)(1, &mut texture);
                }
                if texture == 0 {
                    return Err(GlError {
                        code: 1,
                        operation: "glGenTextures",
                    }
                    .into());
                }
                // SAFETY: the texture is bound and configured before being
                // stored in the map.
                unsafe {
                    (self.gl.glBindTexture)(GL_TEXTURE_2D, texture);
                    let filter = self.config.atlas_filter.gl() as i32;
                    (self.gl.glTexParameteri)(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, filter);
                    (self.gl.glTexParameteri)(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, filter);
                    (self.gl.glTexParameteri)(
                        GL_TEXTURE_2D,
                        GL_TEXTURE_WRAP_S,
                        GL_CLAMP_TO_EDGE as i32,
                    );
                    (self.gl.glTexParameteri)(
                        GL_TEXTURE_2D,
                        GL_TEXTURE_WRAP_T,
                        GL_CLAMP_TO_EDGE as i32,
                    );
                    let (internal, format_enum) = match format {
                        AtlasFormat::Gray8 => (GL_R8 as i32, GL_RED),
                        AtlasFormat::Rgba8 => (GL_RGBA8 as i32, GL_RGBA),
                    };
                    (self.gl.glTexImage2D)(
                        GL_TEXTURE_2D,
                        0,
                        internal,
                        width as i32,
                        height as i32,
                        0,
                        format_enum,
                        GL_UNSIGNED_BYTE,
                        page.data().as_ptr() as *const c_void,
                    );
                }
                self.atlas_pages.insert(
                    page_index,
                    AtlasTexture {
                        texture,
                        format,
                        width,
                        height,
                    },
                );
                stats.pages_created += 1;
                stats.pages_uploaded += 1;
            } else {
                let texture = self
                    .atlas_pages
                    .get(&page_index)
                    .map(|texture| texture.texture)
                    .unwrap_or(0);
                // SAFETY: the texture and page data are valid for this call.
                unsafe {
                    (self.gl.glBindTexture)(GL_TEXTURE_2D, texture);
                    let format_enum = match format {
                        AtlasFormat::Gray8 => GL_RED,
                        AtlasFormat::Rgba8 => GL_RGBA,
                    };
                    (self.gl.glTexSubImage2D)(
                        GL_TEXTURE_2D,
                        0,
                        0,
                        0,
                        width as i32,
                        height as i32,
                        format_enum,
                        GL_UNSIGNED_BYTE,
                        page.data().as_ptr() as *const c_void,
                    );
                }
                stats.pages_uploaded += 1;
            }
        }
        // SAFETY: restoring the conventional unpack alignment.
        unsafe {
            (self.gl.glBindTexture)(GL_TEXTURE_2D, 0);
            (self.gl.glPixelStorei)(GL_UNPACK_ALIGNMENT, 4);
        }
        self.gl.check("atlas upload")?;
        Ok(stats)
    }

    /// Upload `atlas` changes and draw `frame` (convenience wrapper).
    pub fn render_with_atlas(
        &mut self,
        frame: &Frame,
        config: &RenderConfig,
        atlas: &GlyphAtlas,
    ) -> Result<RenderStats, GlBackendError> {
        self.sync_atlas(atlas, frame)?;
        self.render(frame, config)
    }

    /// Draw one frame.
    ///
    /// [`sync_atlas`](Self::sync_atlas) must have been called for any glyph
    /// whose page is not already on the GPU; glyphs without a texture are
    /// counted in [`RenderStats::glyphs_skipped`].
    pub fn render(
        &mut self,
        frame: &Frame,
        config: &RenderConfig,
    ) -> Result<RenderStats, GlBackendError> {
        self.ensure_current()?;
        let scissors = self.scissor_rects(frame, config);

        self.instances.clear();
        self.draw_calls.clear();
        self.glyph_order.clear();
        let mut stats = RenderStats {
            scissor_rects: scissors.len(),
            ..RenderStats::default()
        };

        if scissors.is_empty() {
            self.last_stats = stats;
            return Ok(stats);
        }

        self.build_instances(frame, &mut stats);
        self.upload_instances()?;
        self.draw(&scissors, frame, &mut stats)?;
        stats.instances = self.instances.len();
        self.last_stats = stats;
        Ok(stats)
    }

    /// Read the framebuffer as tightly packed RGBA8, top-left origin.
    pub fn read_pixels(&mut self) -> Result<Vec<u8>, GlBackendError> {
        self.ensure_current()?;
        let width = self.width;
        let height = self.height;
        let mut pixels = vec![0u8; width as usize * height as usize * 4];
        // SAFETY: the buffer is exactly width*height RGBA8 and a context is
        // current. GL returns rows bottom-up, which is flipped below.
        unsafe {
            (self.gl.glPixelStorei)(GL_PACK_ALIGNMENT, 1);
            (self.gl.glReadPixels)(
                0,
                0,
                width as i32,
                height as i32,
                GL_RGBA,
                GL_UNSIGNED_BYTE,
                pixels.as_mut_ptr().cast(),
            );
            (self.gl.glFinish)();
        }
        self.gl.check("glReadPixels")?;
        let stride = self.width as usize * 4;
        if stride > 0 {
            let mut scratch = vec![0u8; stride];
            for row in 0..self.height as usize / 2 {
                let top = row * stride;
                let bottom = (self.height as usize - 1 - row) * stride;
                scratch.copy_from_slice(&pixels[top..top + stride]);
                pixels.copy_within(bottom..bottom + stride, top);
                pixels[bottom..bottom + stride].copy_from_slice(&scratch);
            }
        }
        Ok(pixels)
    }

    fn storage_color(&self, color: Rgba8) -> [f32; 4] {
        let mut ans = [
            color.r as f32 / 255.0,
            color.g as f32 / 255.0,
            color.b as f32 / 255.0,
            color.a as f32 / 255.0,
        ];
        match self.config.gamma {
            GammaMode::None => {}
            GammaMode::Linear => {
                for channel in &mut ans[..3] {
                    *channel = srgb_to_linear(*channel);
                }
            }
            GammaMode::Srgb => {
                for channel in &mut ans[..3] {
                    *channel = linear_to_srgb(*channel);
                }
            }
        }
        ans
    }

    fn instance_color(&self, color: Rgba8, dim: bool) -> [f32; 4] {
        let mut ans = self.storage_color(color);
        if dim {
            ans[3] *= 0.5;
        }
        ans
    }

    fn push_quad(&mut self, rect: Rect, uv: [f32; 4], color: [f32; 4], params: [f32; 4]) {
        self.instances.push(QuadInstance {
            rect: [rect.x, rect.y, rect.width, rect.height],
            uv,
            color,
            params,
        });
    }

    fn push_solid(&mut self, rect: Rect, color: [f32; 4], radius: f32) {
        if rect.is_empty() {
            return;
        }
        self.push_quad(rect, [0.0; 4], color, [radius, 0.0, 0.0, 0.0]);
    }

    fn build_instances(&mut self, frame: &Frame, stats: &mut RenderStats) {
        // Backgrounds.
        let start = self.instances.len();
        for background in &frame.backgrounds {
            let color = self.instance_color(background.color, background.flags.dim);
            self.push_solid(background.rect, color, 0.0);
        }
        if self.instances.len() > start {
            self.draw_calls.push(DrawCall {
                kind: BatchKind::Solid,
                first: start as i32,
                count: (self.instances.len() - start) as i32,
            });
        }

        // Glyphs, grouped by atlas page so each page is bound once.
        for (index, glyph) in frame.glyphs.iter().enumerate() {
            if let Some(sprite) = glyph.sprite {
                if self.atlas_pages.contains_key(&sprite.page) {
                    self.glyph_order.push((sprite.page, index));
                } else {
                    stats.glyphs_skipped += 1;
                }
            } else {
                stats.glyphs_skipped += 1;
            }
        }
        self.glyph_order.sort_unstable_by_key(|(page, _)| *page);
        let mut cursor = 0usize;
        while cursor < self.glyph_order.len() {
            let page = self.glyph_order[cursor].0;
            let is_color = self
                .atlas_pages
                .get(&page)
                .map(|texture| texture.format == AtlasFormat::Rgba8)
                .unwrap_or(false);
            let start = self.instances.len();
            while cursor < self.glyph_order.len() && self.glyph_order[cursor].0 == page {
                let index = self.glyph_order[cursor].1;
                let glyph = &frame.glyphs[index];
                if let Some(sprite) = glyph.sprite {
                    let color = self.instance_color(glyph.fg, glyph.flags.dim);
                    self.push_quad(
                        glyph.rect,
                        [sprite.uv.u0, sprite.uv.v0, sprite.uv.u1, sprite.uv.v1],
                        color,
                        [0.0; 4],
                    );
                }
                cursor += 1;
            }
            if self.instances.len() > start {
                self.draw_calls.push(DrawCall {
                    kind: BatchKind::Glyph { page, is_color },
                    first: start as i32,
                    count: (self.instances.len() - start) as i32,
                });
            }
        }

        // Decorations.
        let start = self.instances.len();
        for decoration in &frame.decorations {
            let color = self.instance_color(decoration.color, decoration.flags.dim);
            self.push_solid(decoration.rect, color, 0.0);
        }
        if self.instances.len() > start {
            self.draw_calls.push(DrawCall {
                kind: BatchKind::Solid,
                first: start as i32,
                count: (self.instances.len() - start) as i32,
            });
        }

        // Cursor layer.
        let start = self.instances.len();
        for cursor in &frame.cursors {
            let color = self.instance_color(cursor.color, cursor.flags.dim);
            let radius = self.config.cursor_radius.max(0.0);
            match cursor.geometry() {
                CursorGeometry::Block(rect)
                | CursorGeometry::Beam(rect)
                | CursorGeometry::Underline(rect) => {
                    self.push_solid(rect, color, radius);
                    stats.cursor_instances += 1;
                }
                CursorGeometry::Hollow(rects) => {
                    for rect in rects {
                        self.push_solid(rect, color, 0.0);
                        stats.cursor_instances += 1;
                    }
                }
            }
        }
        if self.instances.len() > start {
            self.draw_calls.push(DrawCall {
                kind: BatchKind::Solid,
                first: start as i32,
                count: (self.instances.len() - start) as i32,
            });
        }
    }

    fn upload_instances(&mut self) -> Result<(), GlBackendError> {
        let bytes = self.instances.len() * size_of::<QuadInstance>();
        if bytes == 0 {
            return Ok(());
        }
        // SAFETY: the instance buffer is owned by this backend and grows with
        // glBufferData before glBufferSubData writes into it.
        unsafe {
            (self.gl.glBindBuffer)(GL_ARRAY_BUFFER, self.instance_vbo);
            if bytes > self.instance_capacity {
                let capacity = (bytes * 2).max(4096);
                (self.gl.glBufferData)(
                    GL_ARRAY_BUFFER,
                    capacity as isize,
                    ptr::null(),
                    GL_DYNAMIC_DRAW,
                );
                self.instance_capacity = capacity;
            }
            (self.gl.glBufferSubData)(
                GL_ARRAY_BUFFER,
                0,
                bytes as isize,
                self.instances.as_ptr() as *const c_void,
            );
            (self.gl.glBindBuffer)(GL_ARRAY_BUFFER, 0);
        }
        self.gl.check("instance upload")?;
        Ok(())
    }

    fn draw(
        &mut self,
        scissors: &[ScissorRect],
        frame: &Frame,
        stats: &mut RenderStats,
    ) -> Result<(), GlBackendError> {
        let background = self.storage_color(frame.default_bg);
        let gamma = self.config.gamma.uniform();
        let viewport = [self.width as f32, self.height as f32];

        // SAFETY: a context is current, the VAO/VBO hold the uploaded stream
        // and every program/texture id used below is live.
        unsafe {
            (self.gl.glBindVertexArray)(self.vao);
            (self.gl.glEnable)(GL_BLEND);
            (self.gl.glBlendFunc)(GL_ONE, GL_ONE_MINUS_SRC_ALPHA);
            (self.gl.glDisable)(GL_DEPTH_TEST);
            (self.gl.glDisable)(GL_CULL_FACE);
            (self.gl.glEnable)(GL_SCISSOR_TEST);
            (self.gl.glActiveTexture)(GL_TEXTURE0);
            (self.gl.glBindBuffer)(GL_ARRAY_BUFFER, self.instance_vbo);

            for scissor in scissors {
                let y = self.height as i32 - (scissor.y + scissor.height);
                (self.gl.glScissor)(scissor.x, y, scissor.width, scissor.height);
                (self.gl.glClearColor)(background[0], background[1], background[2], background[3]);
                (self.gl.glClear)(GL_COLOR_BUFFER_BIT);

                for call in &self.draw_calls {
                    // glDrawArraysInstanced has no base-instance parameter in
                    // GL 3.3 / GLES 3.0, so point the instanced attributes at
                    // this batch inside the shared stream.
                    let base = call.first as usize * size_of::<QuadInstance>();
                    for (location, components, offset) in
                        [(1, 4, 0), (2, 4, 16), (3, 4, 32), (4, 4, 48)]
                    {
                        (self.gl.glVertexAttribPointer)(
                            location,
                            components,
                            GL_FLOAT,
                            GL_FALSE,
                            QUAD_INSTANCE_STRIDE,
                            (base + offset) as *const c_void,
                        );
                    }
                    match call.kind {
                        BatchKind::Solid => {
                            (self.gl.glUseProgram)(self.programs.solid);
                            (self.gl.glUniform2f)(
                                self.programs.solid_viewport,
                                viewport[0],
                                viewport[1],
                            );
                            (self.gl.glUniform1i)(self.programs.solid_gamma, gamma);
                        }
                        BatchKind::Glyph { page, is_color } => {
                            (self.gl.glUseProgram)(self.programs.glyph);
                            (self.gl.glUniform2f)(
                                self.programs.glyph_viewport,
                                viewport[0],
                                viewport[1],
                            );
                            (self.gl.glUniform1i)(self.programs.glyph_atlas, 0);
                            (self.gl.glUniform1i)(
                                self.programs.glyph_is_color,
                                i32::from(is_color),
                            );
                            (self.gl.glUniform1i)(self.programs.glyph_gamma, gamma);
                            let texture = self
                                .atlas_pages
                                .get(&page)
                                .map(|texture| texture.texture)
                                .unwrap_or(0);
                            (self.gl.glBindTexture)(GL_TEXTURE_2D, texture);
                        }
                    }
                    (self.gl.glDrawArraysInstanced)(GL_TRIANGLE_STRIP, 0, 4, call.count);
                    stats.draw_calls += 1;
                }
            }

            (self.gl.glDisable)(GL_SCISSOR_TEST);
            (self.gl.glBindBuffer)(GL_ARRAY_BUFFER, 0);
            (self.gl.glBindTexture)(GL_TEXTURE_2D, 0);
            (self.gl.glBindVertexArray)(0);
            (self.gl.glUseProgram)(0);
        }
        self.gl.check("draw")?;
        Ok(())
    }

    fn scissor_rects(&self, frame: &Frame, config: &RenderConfig) -> Vec<ScissorRect> {
        let width = self.width as i32;
        let height = self.height as i32;
        if width <= 0 || height <= 0 {
            return Vec::new();
        }
        if frame.damage.full_redraw {
            return vec![ScissorRect {
                x: 0,
                y: 0,
                width,
                height,
            }];
        }

        let cell_height = if frame.geometry.cell_height > 0.0 {
            frame.geometry.cell_height
        } else {
            config.metrics.cell_height
        };
        let origin_y = frame.geometry.origin_y;
        let mut ans = Vec::new();
        let mut push_rows = |start: u32, end: u32| {
            if end <= start || cell_height <= 0.0 {
                return;
            }
            let top = origin_y + start as f32 * cell_height;
            let bottom = origin_y + end as f32 * cell_height;
            let y0 = top.floor().max(0.0) as i32;
            let y1 = bottom.ceil().min(height as f32) as i32;
            if y1 > y0 {
                ans.push(ScissorRect {
                    x: 0,
                    y: y0,
                    width,
                    height: y1 - y0,
                });
            }
        };

        for range in &frame.damage.lines {
            push_rows(range.start as u32, range.end as u32);
        }
        if frame.damage.lines.is_empty() && frame.damage.cursor_moved {
            if let Some(cursor) = frame.damage.new_cursor {
                push_rows(cursor.y, cursor.y.saturating_add(1));
            }
        }
        ans
    }
}

impl Drop for GlBackend {
    fn drop(&mut self) {
        if let Some(context) = &self.context {
            if context.make_current().is_err() {
                // No current context: make sure Programs::drop does not call GL.
                self.programs.glyph = 0;
                self.programs.solid = 0;
                return;
            }
        }
        // With a caller-owned context we assume the caller kept it current.
        // SAFETY: all ids are owned by this backend and a context is current
        // (owned here, or supplied and kept current by the caller).
        unsafe {
            if self.instance_vbo != 0 {
                (self.gl.glDeleteBuffers)(1, &self.instance_vbo);
                self.instance_vbo = 0;
            }
            if self.quad_vbo != 0 {
                (self.gl.glDeleteBuffers)(1, &self.quad_vbo);
                self.quad_vbo = 0;
            }
            if self.vao != 0 {
                (self.gl.glDeleteVertexArrays)(1, &self.vao);
                self.vao = 0;
            }
            for (_, texture) in self.atlas_pages.drain() {
                (self.gl.glDeleteTextures)(1, &texture.texture);
            }
        }
        let _ = self.gl.check("teardown");
        self.programs.destroy();
        // `Programs::drop` is now a no-op; `context` is dropped last.
    }
}

impl fmt::Debug for GlBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GlBackend")
            .field("size", &self.size())
            .field("gl_version", &self.gl_version_string())
            .field("renderer", &self.renderer())
            .field("atlas_textures", &self.atlas_texture_count())
            .field("stats", &self.last_stats)
            .finish_non_exhaustive()
    }
}

// }}}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_layout_is_stable() {
        assert_eq!(size_of::<QuadInstance>(), 64);
        assert_eq!(QUAD_INSTANCE_STRIDE, 64);
    }

    #[test]
    fn gamma_uniform_values() {
        assert_eq!(GammaMode::None.uniform(), 0);
        assert_eq!(GammaMode::Linear.uniform(), 1);
        assert_eq!(GammaMode::Srgb.uniform(), 2);
    }
}
