// SPDX-License-Identifier: GPL-3.0-or-later
//! Headless OpenGL backend for the CPU instance stream.
//!
//! The backend compiles kitty's real Slang programs (via the generated GLSL in
//! [`crate::shaders`]), owns one shared VAO/VBO pair and a GPU texture per
//! atlas page. [`GlBackend::new`] creates a headless EGL pbuffer
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
//! Image data lives in [`Frame::image_uploads`]; call
//! [`sync_images`](GlBackend::sync_images) (also done by
//! [`render_with_atlas`](GlBackend::render_with_atlas)) to upload and evict
//! them. Textures are keyed by [`ImageKey`]; content fingerprints avoid
//! needless uploads, and textures no longer referenced by the frame are
//! deleted. This is what makes graphics-protocol deletes visible on the GPU.
//!
//! # Damage
//!
//! A full redraw clears the whole viewport. A partial frame sets the scissor
//! box to each damaged row range, clears that box to the default background
//! and replays all draw calls clipped to it. This is simple and correct, at
//! the cost of re-issuing every instance for each damaged range. Cursor-only
//! damage arrives as the two cursor row ranges from [`crate::DamageTracker`].
//!
//! # Shader programs
//!
//! Every program from `kitty/shaders.c` is present in the generated catalogue,
//! including all 27 cell variants, the graphics/indexed-graphics
//! specialisations and the borders/padding/bgimage/blit/screenshot programs.
//! [`GlBackend::compiled_programs`] lists the programs linked eagerly;
//! [`validate_builtin_programs`] and [`validate_all_shader_variants`] compile
//! the complete set for tests.
//!
//! The frame instance stream is drawn with the real `tint`, `rounded_rect`,
//! `graphics` (default and alpha-mask), `trail` and indexed-graphics programs.
//! Background images use the `bgimage` program when their layout and opacity
//! can be represented by it (transparent or arbitrary-source-rectangle
//! backgrounds fall back to the general image path). See `SHADERS.md` for the
//! exact mapping.
//!
//! * Gray8 atlas pages (`GL_R8`) and RGBA8 pages (`GL_RGBA8`);
//! * RGBA8 image textures keyed by [`ImageKey`], with nearest/linear filtering
//!   and clamp/repeat/mirrored-repeat wrapping for background layouts;
//! * a fixed-cell `GL_TEXTURE_2D_ARRAY` sprite atlas adapted from the packed
//!   CPU pages, driving the `cell`/`cell_bg`/`cell_fg` UBO programs when
//!   [`GlBackend::set_cell_programs`] is enabled;
//! * nearest or linear atlas filtering;
//! * premultiplied alpha blending;
//! * scissor-based damage, viewport, clear, offscreen `glReadPixels`;
//! * offscreen `blit` and colour-space-correct `screenshot` scaling;
//! * animated multi-group custom-shader post-processing;
//! * optional CPU-side gamma conversion (`GammaMode`).
//!
//! # Limits
//!
//! The cell path requires an origin-aligned full grid without multi-cell
//! glyphs; other frames fall back to the instance path. Image transforms
//! beyond source-rectangle scaling, per-window background-image lists, full
//! cursor-trail corner animation and the complete `KittyCustomShaderData`
//! payload (pointer mapping in particular) remain out of scope.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::ffi::{c_char, c_void, CString};
use std::fmt;
use std::mem::size_of;
use std::ptr;

use crate::atlas::{AtlasEvent, AtlasFormat, GlyphAtlas};
use crate::cell::{build_cell_sprites, CellSpriteData};
use crate::colors::{linear_to_srgb, srgb_to_linear, Rgba8};
use crate::custom_shader::{
    CustomShaderAnimationState, CustomShaderPipeline, NamedTexture, ShaderEventMask,
};
use crate::egl::{
    ConfigAttribs, ContextConfig, EglError, GlApi, GlContext, GlError, GlFns, GlVersion, LoadError,
    GL_ARRAY_BUFFER, GL_BLEND, GL_CLAMP_TO_EDGE, GL_COLOR_ATTACHMENT0, GL_COLOR_BUFFER_BIT,
    GL_COMPILE_STATUS, GL_CULL_FACE, GL_DEPTH_TEST, GL_DYNAMIC_DRAW, GL_FALSE, GL_FLOAT,
    GL_FRAGMENT_SHADER, GL_FRAMEBUFFER, GL_FRAMEBUFFER_COMPLETE, GL_INFO_LOG_LENGTH, GL_LINEAR,
    GL_LINK_STATUS, GL_NEAREST, GL_ONE, GL_ONE_MINUS_SRC_ALPHA, GL_PACK_ALIGNMENT, GL_R32UI, GL_R8,
    GL_R8UI, GL_RED, GL_RED_INTEGER, GL_RG8UI, GL_RGBA, GL_RGBA16F, GL_RGBA8, GL_RG_INTEGER,
    GL_SCISSOR_TEST, GL_SRGB8_ALPHA8, GL_STATIC_DRAW, GL_TEXTURE0, GL_TEXTURE_2D,
    GL_TEXTURE_2D_ARRAY, GL_TEXTURE_MAG_FILTER, GL_TEXTURE_MIN_FILTER, GL_TEXTURE_WRAP_S,
    GL_TEXTURE_WRAP_T, GL_TRIANGLE_FAN, GL_UNIFORM_BUFFER, GL_UNPACK_ALIGNMENT, GL_UNSIGNED_BYTE,
    GL_UNSIGNED_INT, GL_VERTEX_SHADER,
};
use crate::geom::{Rect, Region};
use crate::images::{ImageFilter, ImageKey, ImageWrap};
use crate::instances::{CursorGeometry, Frame, RenderConfig};
use kitty_graphics::ImageFormat;

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

#[inline]
const fn image_filter_gl(filter: ImageFilter) -> u32 {
    match filter {
        ImageFilter::Nearest => GL_NEAREST,
        ImageFilter::Linear => GL_LINEAR,
    }
}

#[inline]
const fn image_wrap_gl(wrap: ImageWrap) -> u32 {
    match wrap {
        ImageWrap::Clamp => GL_CLAMP_TO_EDGE,
        ImageWrap::Repeat => GL_REPEAT,
        ImageWrap::MirroredRepeat => GL_MIRRORED_REPEAT,
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
    #[allow(dead_code)]
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
    /// kitty cell-shader text contrast multiplier (`1 + percent / 100`).
    pub text_contrast: f32,
    /// kitty cell-shader text gamma adjustment (reciprocal of the option).
    pub text_gamma_adjustment: f32,
}

impl Default for GlBackendConfig {
    fn default() -> Self {
        Self {
            context: ContextConfig::default(),
            gamma: GammaMode::None,
            atlas_filter: FilterMode::Linear,
            cursor_radius: 0.0,
            text_contrast: 1.0,
            text_gamma_adjustment: 1.0,
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
    /// Protocol image placements considered this frame.
    pub images: usize,
    /// Image quads actually drawn (texture available).
    pub images_drawn: usize,
    /// Image quads skipped because their texture was missing.
    pub images_skipped: usize,
    /// Whether the background image was drawn.
    pub background_image_drawn: bool,
    /// Background images drawn through the `bgimage` program.
    pub bgimage_draws: usize,
    /// Whether the window logo was drawn.
    pub logo_drawn: bool,
    /// Cursor-trail quads drawn.
    pub trail_instances: usize,
    /// Cells submitted to the fixed-cell sprite-array programs.
    pub cell_instances: usize,
    /// Whether cell/background/foreground programs were used.
    pub cell_programs: bool,
}

/// Statistics from one [`GlBackend::sync_atlas`] call.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AtlasSyncStats {
    pub pages_created: usize,
    pub pages_uploaded: usize,
    pub pages_removed: usize,
}

/// Statistics from one [`GlBackend::sync_images`] call.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ImageSyncStats {
    /// New GPU textures created.
    pub created: usize,
    /// Existing textures refreshed with new pixels.
    pub updated: usize,
    /// Textures deleted because they are no longer referenced.
    pub removed: usize,
    /// Uploads that were already current and skipped.
    pub unchanged: usize,
}

/// Pixels returned by [`GlBackend::screenshot_region`], top-down RGBA8.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Screenshot {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
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
    CustomShader(String),
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
            Self::CustomShader(log) => write!(f, "custom shader compilation failed: {log}"),
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

// Not exported by `egl.rs` because the atlas only uses clamp-to-edge.
const GL_REPEAT: u32 = 0x2901;
const GL_MIRRORED_REPEAT: u32 = 0x8370;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BatchKind {
    Solid,
    Glyph {
        page: u32,
        is_color: bool,
    },
    Image(ImageKey),
    BackgroundImage(ImageKey),
    Trail,
    /// Draw the cell grid through `cell` (both background and foreground).
    CellBoth,
    /// Draw non-default backgrounds and text through `cell`.
    CellBothNonDefault,
    /// Draw only default backgrounds through `cell_bg`.
    CellBackgroundDefault,
    /// Draw only non-default backgrounds through `cell_bg`.
    CellBackgroundNonDefault,
    /// Draw only text through `cell_fg`.
    CellForeground,
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
struct CachedImage {
    texture: u32,
    width: u32,
    height: u32,
    filter: ImageFilter,
    wrap: ImageWrap,
    indexed_format: Option<ImageFormat>,
    fingerprint: u64,
}

struct CellSprites {
    texture: u32,
    decorations: u32,
    data: CellSpriteData,
}

#[derive(Clone, Copy, Debug)]
struct OffscreenTarget {
    framebuffer: u32,
    texture: u32,
    width: u32,
    height: u32,
}

struct InstalledCustomPipeline {
    pipeline: CustomShaderPipeline,
    program: u32,
    uniforms: HashMap<String, i32>,
    data_ubo: u32,
    shader_data_ubo: u32,
    source_texture: u32,
    named: [Option<OffscreenTarget>; 3],
    ping: [Option<OffscreenTarget>; 2],
    width: u32,
    height: u32,
}

impl InstalledCustomPipeline {
    fn uniform(&self, name: &str) -> i32 {
        if let Some(location) = self.uniforms.get(name).copied() {
            if location >= 0 {
                return location;
            }
        }
        self.uniforms
            .get(&format!("{name}_0"))
            .copied()
            .unwrap_or(-1)
    }
}

#[derive(Clone, Copy, Debug)]
struct ScissorRect {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

struct LinkedProgram {
    id: u32,
    source: &'static crate::shaders::GeneratedProgram,
    uniforms: HashMap<String, i32>,
}

impl LinkedProgram {
    fn new(
        gl: &GlFns,
        kind: crate::shaders::ProgramKind,
        api: GlApi,
    ) -> Result<Self, GlBackendError> {
        let source =
            crate::shaders::default_variant(kind).ok_or_else(|| GlBackendError::Shader {
                stage: "pipeline",
                log: format!("no generated variant for {kind:?}"),
            })?;
        let sources = crate::shaders::ProgramSources::for_api(
            source,
            crate::shaders::ShaderDialect::for_api(api),
        );
        let vertex = compile_shader(gl, GL_VERTEX_SHADER, "vertex", &sources.vertex)?;
        let fragment = match compile_shader(gl, GL_FRAGMENT_SHADER, "fragment", &sources.fragment) {
            Ok(id) => id,
            Err(error) => {
                unsafe { (gl.glDeleteShader)(vertex) };
                return Err(error);
            }
        };
        let id = link_program(gl, vertex, fragment, kind.name())?;
        // SAFETY: the program is linked and a context is current.
        unsafe {
            let index = (gl.glGetUniformBlockIndex)(id, c"block_ColorTable_0".as_ptr());
            if index != u32::MAX {
                (gl.glUniformBlockBinding)(id, index, 2);
            }
        }
        let mut uniforms = HashMap::new();
        for (slang, glsl) in source.metadata.loose_uniforms {
            let name = glsl.split('[').next().unwrap_or(glsl);
            let location = uniform_location(gl, id, name);
            uniforms.insert((*slang).to_owned(), location);
        }
        for (slang, glsl) in source.metadata.uniform_struct_names {
            let binding = match *slang {
                "CellRenderData" => 0,
                "ColorTable" => 1,
                "Colors" => 2,
                "KittyCustomShaderData" => 3,
                _ => continue,
            };
            if let Ok(name) = CString::new(*glsl) {
                // SAFETY: the program is linked and a context is current.
                unsafe {
                    let index = (gl.glGetUniformBlockIndex)(id, name.as_ptr());
                    if index != u32::MAX {
                        (gl.glUniformBlockBinding)(id, index, binding);
                    }
                }
            }
        }
        Ok(Self {
            id,
            source,
            uniforms,
        })
    }

    fn uniform(&self, slang: &str) -> i32 {
        self.uniforms.get(slang).copied().unwrap_or(-1)
    }
}

/// Programs the backend compiles up front. The complete catalogue (including
/// every cell render mode/specialisation and indexed-image variants) is
/// available through [`ProgramKind::ALL`] and
/// [`validate_builtin_programs`].
const COMPILED_PROGRAMS: &[crate::shaders::ProgramKind] = &[
    crate::shaders::ProgramKind::Cell,
    crate::shaders::ProgramKind::Borders,
    crate::shaders::ProgramKind::Graphics,
    crate::shaders::ProgramKind::GraphicsPremult,
    crate::shaders::ProgramKind::GraphicsAlphaMask,
    crate::shaders::ProgramKind::GraphicsIndexed,
    crate::shaders::ProgramKind::GraphicsIndexedUnscaled,
    crate::shaders::ProgramKind::BgImage,
    crate::shaders::ProgramKind::Tint,
    crate::shaders::ProgramKind::Trail,
    crate::shaders::ProgramKind::Blit,
    crate::shaders::ProgramKind::Screenshot,
    crate::shaders::ProgramKind::RoundedRect,
    crate::shaders::ProgramKind::Padding,
];

struct Programs {
    gl: GlFns,
    programs: HashMap<crate::shaders::ProgramKind, LinkedProgram>,
}

impl Programs {
    fn new(gl: &GlFns, api: GlApi) -> Result<Self, GlBackendError> {
        let mut programs = HashMap::new();
        for &kind in COMPILED_PROGRAMS {
            match LinkedProgram::new(gl, kind, api) {
                Ok(program) => {
                    programs.insert(kind, program);
                }
                Err(error) => {
                    for program in programs.values() {
                        unsafe { (gl.glDeleteProgram)(program.id) };
                    }
                    return Err(error);
                }
            }
        }
        Ok(Self { gl: *gl, programs })
    }

    fn get(&self, kind: crate::shaders::ProgramKind) -> &LinkedProgram {
        self.programs
            .get(&kind)
            .unwrap_or_else(|| panic!("program {kind:?} was not compiled"))
    }

    fn upload_gamma_lut(&self) {
        let lut = crate::colors::gamma_lut();
        for program in self.programs.values() {
            let location = program.uniform("gamma_lut");
            if location >= 0 {
                // SAFETY: the program is linked and a context is current; the
                // LUT is a static array of 256 floats.
                unsafe {
                    (self.gl.glUseProgram)(program.id);
                    (self.gl.glUniform1fv)(location, lut.len() as i32, lut.as_ptr());
                }
            }
        }
        // SAFETY: restore the conventional no-program state.
        unsafe { (self.gl.glUseProgram)(0) };
    }

    fn destroy(&mut self) {
        // SAFETY: the ids are owned by this struct and the context is still
        // current during backend teardown.
        unsafe {
            for program in self.programs.values() {
                (self.gl.glDeleteProgram)(program.id);
            }
        }
        self.programs.clear();
    }
}

impl Drop for Programs {
    fn drop(&mut self) {
        self.destroy();
    }
}

/// Compile and link every logical program from `kitty/shaders.c`.
///
/// This is intended for validation/tests; the renderer itself only compiles
/// the programs it draws with. It creates a temporary headless context.
pub fn validate_builtin_programs(api: GlApi) -> Result<usize, GlBackendError> {
    validate_programs(api, false)
}

/// Compile and link every generated specialisation, including all 27 cell
/// variants and the graphics/indexed-graphics variants.
pub fn validate_all_shader_variants(api: GlApi) -> Result<usize, GlBackendError> {
    validate_programs(api, true)
}

fn validate_programs(api: GlApi, all_variants: bool) -> Result<usize, GlBackendError> {
    let config = if api == GlApi::OpenGlEs {
        ContextConfig::new(16, 16).gles()
    } else {
        ContextConfig::new(16, 16)
    };
    let context = GlContext::new(config)?;
    context.make_current()?;
    // SAFETY: the context is current for the lifetime of `gl`.
    let gl = unsafe { GlFns::load()? };
    let mut count = 0;
    if all_variants {
        for source in crate::shaders::BUILTIN_PROGRAMS {
            let sources = crate::shaders::ProgramSources::for_api(
                source,
                crate::shaders::ShaderDialect::for_api(api),
            );
            let vertex = compile_shader(&gl, GL_VERTEX_SHADER, "vertex", &sources.vertex)?;
            let fragment =
                match compile_shader(&gl, GL_FRAGMENT_SHADER, "fragment", &sources.fragment) {
                    Ok(id) => id,
                    Err(error) => {
                        unsafe { (gl.glDeleteShader)(vertex) };
                        return Err(error);
                    }
                };
            let program = link_program(&gl, vertex, fragment, source.program)?;
            unsafe { (gl.glDeleteProgram)(program) };
            count += 1;
        }
    } else {
        for &kind in crate::shaders::ProgramKind::ALL {
            if kind == crate::shaders::ProgramKind::CustomEnd {
                continue;
            }
            let program = LinkedProgram::new(&gl, kind, api)?;
            unsafe { (gl.glDeleteProgram)(program.id) };
            count += 1;
        }
    }
    drop(context);
    Ok(count)
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

unsafe fn ensure_offscreen(
    gl: &GlFns,
    target: &mut Option<OffscreenTarget>,
    width: u32,
    height: u32,
) -> Result<OffscreenTarget, GlError> {
    if let Some(existing) = *target {
        if existing.width == width && existing.height == height {
            return Ok(existing);
        }
        // SAFETY: the caller guarantees a current context and owns the ids.
        unsafe {
            (gl.glDeleteFramebuffers)(1, &existing.framebuffer);
            (gl.glDeleteTextures)(1, &existing.texture);
        }
    }
    // SAFETY: the caller guarantees a current context.
    let created = unsafe { create_offscreen_target(gl, width, height, GL_RGBA8 as i32)? };
    *target = Some(created);
    Ok(created)
}

/// std140 size of kitty's `CellRenderData` block (the C mirror has the same
/// size; the final scalar is padded to the block's 16-byte alignment).
const CELL_RENDER_DATA_SIZE: usize = 320;

unsafe fn create_cell_geometry(gl: &GlFns) -> Result<(u32, u32, u32, u32), GlError> {
    // SAFETY: the caller guarantees a current context; ids are owned by the
    // caller on success.
    unsafe {
        let mut vao = 0u32;
        (gl.glGenVertexArrays)(1, &mut vao);
        if vao == 0 {
            return Err(GlError {
                code: 1,
                operation: "glGenVertexArrays",
            });
        }
        let mut buffers = [0u32; 3];
        (gl.glGenBuffers)(3, buffers.as_mut_ptr());
        if buffers.contains(&0) {
            (gl.glDeleteVertexArrays)(1, &vao);
            (gl.glDeleteBuffers)(3, buffers.as_ptr());
            return Err(GlError {
                code: 1,
                operation: "glGenBuffers",
            });
        }
        (gl.glBindVertexArray)(vao);
        // colors: location 0, three uint32 per cell
        (gl.glBindBuffer)(GL_ARRAY_BUFFER, buffers[0]);
        (gl.glEnableVertexAttribArray)(0);
        (gl.glVertexAttribIPointer)(0, 3, GL_UNSIGNED_INT, 12, ptr::null());
        (gl.glVertexAttribDivisor)(0, 1);
        // sprite_idx: location 1, two uint32 per cell
        (gl.glBindBuffer)(GL_ARRAY_BUFFER, buffers[1]);
        (gl.glEnableVertexAttribArray)(1);
        (gl.glVertexAttribIPointer)(1, 2, GL_UNSIGNED_INT, 8, ptr::null());
        (gl.glVertexAttribDivisor)(1, 1);
        // is_selected: location 2, one ubyte per cell
        (gl.glBindBuffer)(GL_ARRAY_BUFFER, buffers[2]);
        (gl.glEnableVertexAttribArray)(2);
        (gl.glVertexAttribIPointer)(2, 1, GL_UNSIGNED_BYTE, 1, ptr::null());
        (gl.glVertexAttribDivisor)(2, 1);
        (gl.glBindVertexArray)(0);
        (gl.glBindBuffer)(GL_ARRAY_BUFFER, 0);
        Ok((vao, buffers[0], buffers[1], buffers[2]))
    }
}

unsafe fn create_offscreen_target(
    gl: &GlFns,
    width: u32,
    height: u32,
    internal_format: i32,
) -> Result<OffscreenTarget, GlError> {
    // SAFETY: the caller guarantees a current context.
    unsafe {
        let mut texture = 0u32;
        (gl.glGenTextures)(1, &mut texture);
        (gl.glBindTexture)(GL_TEXTURE_2D, texture);
        (gl.glTexParameteri)(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST as i32);
        (gl.glTexParameteri)(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST as i32);
        (gl.glTexParameteri)(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE as i32);
        (gl.glTexParameteri)(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE as i32);
        (gl.glTexImage2D)(
            GL_TEXTURE_2D,
            0,
            internal_format,
            width as i32,
            height as i32,
            0,
            GL_RGBA,
            GL_UNSIGNED_BYTE,
            ptr::null(),
        );
        let mut framebuffer = 0u32;
        (gl.glGenFramebuffers)(1, &mut framebuffer);
        (gl.glBindFramebuffer)(GL_FRAMEBUFFER, framebuffer);
        (gl.glFramebufferTexture2D)(
            GL_FRAMEBUFFER,
            GL_COLOR_ATTACHMENT0,
            GL_TEXTURE_2D,
            texture,
            0,
        );
        let status = (gl.glCheckFramebufferStatus)(GL_FRAMEBUFFER);
        (gl.glBindFramebuffer)(GL_FRAMEBUFFER, 0);
        (gl.glBindTexture)(GL_TEXTURE_2D, 0);
        if status != GL_FRAMEBUFFER_COMPLETE {
            (gl.glDeleteFramebuffers)(1, &framebuffer);
            (gl.glDeleteTextures)(1, &texture);
            return Err(GlError {
                code: 0x0506,
                operation: "glCheckFramebufferStatus",
            });
        }
        Ok(OffscreenTarget {
            framebuffer,
            texture,
            width,
            height,
        })
    }
}

fn put_f32(buf: &mut [u8], offset: usize, value: f32) {
    buf[offset..offset + 4].copy_from_slice(&value.to_ne_bytes());
}

fn put_u32(buf: &mut [u8], offset: usize, value: u32) {
    buf[offset..offset + 4].copy_from_slice(&value.to_ne_bytes());
}

fn put_linear_vec4(buf: &mut [u8], offset: usize, color: Rgba8) {
    // Same conversion as linear_color_vec4() in kitty/shaders.c: linear RGB,
    // with the fourth component as unused padding.
    for (index, channel) in [color.r, color.g, color.b].iter().enumerate() {
        put_f32(
            buf,
            offset + index * 4,
            srgb_to_linear(*channel as f32 / 255.0),
        );
    }
    put_f32(buf, offset + 12, 0.0);
}

/// Pack the std140 `CellRenderData` block for one cell draw.
fn pack_cell_render_data(data: &CellSpriteData, frame: &Frame) -> [u8; CELL_RENDER_DATA_SIZE] {
    let mut buf = [0u8; CELL_RENDER_DATA_SIZE];
    put_linear_vec4(&mut buf, 0, frame.default_fg);
    put_linear_vec4(&mut buf, 16, frame.default_bg);
    put_linear_vec4(&mut buf, 32, frame.default_fg);
    put_linear_vec4(&mut buf, 48, frame.default_bg);
    put_linear_vec4(&mut buf, 64, frame.default_fg);
    put_linear_vec4(&mut buf, 80, frame.default_bg);
    put_linear_vec4(&mut buf, 96, frame.default_fg);
    put_linear_vec4(&mut buf, 112, frame.default_fg);
    put_linear_vec4(&mut buf, 128, frame.default_bg);
    // use_cell_* selection flags stay zero: the CPU stage already resolved
    // selection colours into the cell colour stream.
    put_u32(
        &mut buf,
        156,
        crate::colors::encode_rgb(frame.default_fg.r, frame.default_fg.g, frame.default_fg.b),
    );
    put_u32(&mut buf, 180, data.columns);
    put_u32(&mut buf, 184, data.lines);
    put_u32(&mut buf, 188, data.sprites_xnum);
    put_u32(&mut buf, 192, data.sprites_ynum);
    put_u32(&mut buf, 196, 0); // cursor_shape
    put_u32(&mut buf, 200, data.cell_width);
    put_u32(&mut buf, 204, data.cell_height);
    put_u32(&mut buf, 208, data.columns + 1); // cursor_x1
    put_u32(&mut buf, 212, data.columns); // cursor_x2
    put_u32(&mut buf, 216, data.lines + 1); // cursor_y1
    put_u32(&mut buf, 220, data.lines); // cursor_y2
    put_f32(&mut buf, 224, 1.0); // cursor_opacity
    put_f32(&mut buf, 228, 1.0); // inactive_text_alpha
    put_f32(&mut buf, 232, 0.0); // fg_override_threshold
    put_f32(&mut buf, 236, 0.0); // row_offset
    put_f32(&mut buf, 240, 1.0); // dim_opacity
    put_f32(&mut buf, 244, 1.0); // blink_opacity
    let default_bg =
        crate::colors::encode_rgb(frame.default_bg.r, frame.default_bg.g, frame.default_bg.b);
    for slot in 0..8 {
        put_u32(&mut buf, 248 + slot * 4, default_bg);
        put_f32(&mut buf, 280 + slot * 4, 1.0);
    }
    buf
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
    image_textures: HashMap<ImageKey, CachedImage>,
    width: u32,
    height: u32,
    last_stats: RenderStats,
    custom_program: u32,
    color_table_ubo: u32,
    cell_data_ubo: u32,
    cell_vao: u32,
    cell_colors_vbo: u32,
    cell_sprite_vbo: u32,
    cell_selection_vbo: u32,
    cell_instance_capacity: usize,
    cell_sprites: Option<CellSprites>,
    cell_offscreen: Option<OffscreenTarget>,
    cell_programs: bool,
    custom_pipeline: Option<InstalledCustomPipeline>,
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
        let mut color_table_ubo = 0u32;
        let mut cell_data_ubo = 0u32;
        // SAFETY: a context is current; the UBOs are owned by the backend.
        unsafe {
            (gl.glGenBuffers)(1, &mut color_table_ubo);
            (gl.glBindBuffer)(GL_UNIFORM_BUFFER, color_table_ubo);
            // std140 gives each uint array element a 16-byte stride.
            (gl.glBufferData)(
                GL_UNIFORM_BUFFER,
                (272 * 16) as isize,
                ptr::null(),
                GL_DYNAMIC_DRAW,
            );
            (gl.glBindBufferBase)(GL_UNIFORM_BUFFER, 1, color_table_ubo);
            (gl.glGenBuffers)(1, &mut cell_data_ubo);
            (gl.glBindBuffer)(GL_UNIFORM_BUFFER, cell_data_ubo);
            (gl.glBufferData)(
                GL_UNIFORM_BUFFER,
                CELL_RENDER_DATA_SIZE as isize,
                ptr::null(),
                GL_DYNAMIC_DRAW,
            );
            (gl.glBindBufferBase)(GL_UNIFORM_BUFFER, 0, cell_data_ubo);
            (gl.glBindBuffer)(GL_UNIFORM_BUFFER, 0);
        }
        let (cell_vao, cell_colors_vbo, cell_sprite_vbo, cell_selection_vbo) =
            unsafe { create_cell_geometry(&gl)? };
        programs.upload_gamma_lut();
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
            image_textures: HashMap::new(),
            width,
            height,
            last_stats: RenderStats::default(),
            custom_program: 0,
            color_table_ubo,
            cell_data_ubo,
            cell_vao,
            cell_colors_vbo,
            cell_sprite_vbo,
            cell_selection_vbo,
            cell_instance_capacity: 0,
            cell_sprites: None,
            cell_offscreen: None,
            cell_programs: false,
            custom_pipeline: None,
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

    /// Number of live GPU image textures (protocol, background and logo).
    #[inline]
    pub fn image_texture_count(&self) -> usize {
        self.image_textures.len()
    }
    /// Select whether ordinary frame rendering uses the `cell`/`cell_bg`/
    /// `cell_fg` sprite-array programs instead of the per-instance graphics
    /// path for backgrounds and glyphs.
    ///
    /// The sprite array is rebuilt from [`GlyphAtlas`] during
    /// [`sync_atlas`](Self::sync_atlas). Frames that are not a plain
    /// origin-aligned cell grid, or that contain multi-cell glyphs, silently
    /// fall back to the graphics path.
    pub fn set_cell_programs(&mut self, enabled: bool) {
        self.cell_programs = enabled;
        if !enabled {
            self.cell_sprites = None;
        }
    }

    /// Whether [`render`](Self::render) uses the cell program family.
    #[inline]
    pub const fn cell_programs_enabled(&self) -> bool {
        self.cell_programs
    }

    /// Number of sprites in the most recently uploaded fixed-cell array.
    #[inline]
    pub fn cell_sprite_count(&self) -> usize {
        self.cell_sprites
            .as_ref()
            .map(|sprites| sprites.data.sprite_indices.len())
            .unwrap_or(0)
    }

    /// Draw a frame using the cell sprite-array programs, uploading `atlas`
    /// first. This is the explicit form of
    /// [`set_cell_programs`](Self::set_cell_programs); the setting is left
    /// enabled afterwards.
    pub fn render_cells_with_atlas(
        &mut self,
        frame: &Frame,
        config: &RenderConfig,
        atlas: &GlyphAtlas,
    ) -> Result<RenderStats, GlBackendError> {
        self.set_cell_programs(true);
        self.render_with_atlas(frame, config, atlas)
    }

    /// Programs compiled by this backend.
    pub fn compiled_programs(&self) -> Vec<crate::shaders::ProgramKind> {
        let mut ans: Vec<_> = self.programs.programs.keys().copied().collect();
        ans.sort_unstable();
        ans
    }

    /// Uniform location using the Slang name from the generated metadata.
    pub fn program_uniform(&self, kind: crate::shaders::ProgramKind, name: &str) -> Option<i32> {
        self.programs
            .programs
            .get(&kind)
            .map(|program| program.uniform(name))
            .filter(|location| *location >= 0)
    }

    /// The generated Slang/GLSL program used for `kind`.
    pub fn program_source(
        &self,
        kind: crate::shaders::ProgramKind,
    ) -> Option<&'static crate::shaders::GeneratedProgram> {
        self.programs
            .programs
            .get(&kind)
            .map(|program| program.source)
    }

    /// Compile and install a user Slang shader using the same pipeline as
    /// kitty's custom-shader path.
    pub fn install_custom_shader(
        &mut self,
        name: &str,
        source: &str,
    ) -> Result<(), GlBackendError> {
        let compiler = crate::shaders::SlangCompiler::from_env()
            .map_err(|error| GlBackendError::CustomShader(error.to_string()))?;
        self.install_custom_shader_with(&compiler, name, source, &[])
    }

    /// Like [`install_custom_shader`](Self::install_custom_shader) but with an
    /// explicit compiler and Slang defines.
    pub fn install_custom_shader_with(
        &mut self,
        compiler: &crate::shaders::SlangCompiler,
        name: &str,
        source: &str,
        defines: &[(&str, &str)],
    ) -> Result<(), GlBackendError> {
        self.ensure_current()?;
        let custom = compiler
            .compile_custom_shader(name, source, defines)
            .map_err(|error| GlBackendError::CustomShader(error.to_string()))?;
        let sources = custom.sources(crate::shaders::ShaderDialect::for_api(
            self.config.context.api,
        ));
        let vertex = compile_shader(&self.gl, GL_VERTEX_SHADER, "custom vertex", &sources.vertex)?;
        let fragment = match compile_shader(
            &self.gl,
            GL_FRAGMENT_SHADER,
            "custom fragment",
            &sources.fragment,
        ) {
            Ok(id) => id,
            Err(error) => {
                unsafe { (self.gl.glDeleteShader)(vertex) };
                return Err(error);
            }
        };
        let program = link_program(&self.gl, vertex, fragment, "custom")?;
        if self.custom_program != 0 {
            unsafe { (self.gl.glDeleteProgram)(self.custom_program) };
        }
        self.custom_program = program;
        Ok(())
    }

    /// Compile and install an animated multi-group custom shader pipeline.
    ///
    /// `sources` has one entry per pipeline group; each entry is the list of
    /// Slang sources for that group's shaders. The sources use kitty's custom
    /// shader signature `fragment_main(float4 color, KittyTextures t,
    /// KittyCustomShaderData d)` and are combined into a single
    /// `CUSTOM_END_PROGRAM`-style dispatcher.
    pub fn install_custom_pipeline(
        &mut self,
        compiler: &crate::shaders::SlangCompiler,
        pipeline: &CustomShaderPipeline,
        sources: &[Vec<String>],
    ) -> Result<(), GlBackendError> {
        self.ensure_current()?;
        let custom = compiler
            .compile_custom_pipeline(&pipeline.name, pipeline, sources)
            .map_err(|error| GlBackendError::CustomShader(error.to_string()))?;
        let compiled_sources = custom.sources(crate::shaders::ShaderDialect::for_api(
            self.config.context.api,
        ));
        let vertex = compile_shader(
            &self.gl,
            GL_VERTEX_SHADER,
            "pipeline vertex",
            &compiled_sources.vertex,
        )?;
        let fragment = match compile_shader(
            &self.gl,
            GL_FRAGMENT_SHADER,
            "pipeline fragment",
            &compiled_sources.fragment,
        ) {
            Ok(id) => id,
            Err(error) => {
                unsafe { (self.gl.glDeleteShader)(vertex) };
                return Err(error);
            }
        };
        let program = link_program(&self.gl, vertex, fragment, "custom pipeline")?;
        let uniform_names = [
            "src_rect",
            "dest_rect",
            "backbuffer",
            "a",
            "b",
            "persist",
            "group",
            "viewport",
            "animation_progress",
            "convert_to_srgb",
            "active_groups",
            "source_is_srgb",
            "viewport_size_pixels",
        ];
        let mut uniforms = HashMap::new();
        for name in uniform_names {
            uniforms.insert(name.to_owned(), uniform_location(&self.gl, program, name));
            let suffixed = format!("{name}_0");
            uniforms.insert(
                suffixed.clone(),
                uniform_location(&self.gl, program, &suffixed),
            );
        }
        let mut data_ubo = 0u32;
        let mut shader_data_ubo = 0u32;
        // SAFETY: a context is current; the UBO and bindings are owned below.
        unsafe {
            (self.gl.glGenBuffers)(1, &mut data_ubo);
            (self.gl.glBindBuffer)(GL_UNIFORM_BUFFER, data_ubo);
            (self.gl.glBufferData)(GL_UNIFORM_BUFFER, 4096, ptr::null(), GL_DYNAMIC_DRAW);
            let mut zeroes = [0u8; 4096];
            (self.gl.glBufferSubData)(
                GL_UNIFORM_BUFFER,
                0,
                zeroes.len() as isize,
                zeroes.as_mut_ptr() as *const c_void,
            );
            (self.gl.glGenBuffers)(1, &mut shader_data_ubo);
            (self.gl.glBindBuffer)(GL_UNIFORM_BUFFER, shader_data_ubo);
            (self.gl.glBufferData)(GL_UNIFORM_BUFFER, 4096, ptr::null(), GL_DYNAMIC_DRAW);
            let zeroes = [0u8; 4096];
            (self.gl.glBufferSubData)(
                GL_UNIFORM_BUFFER,
                0,
                zeroes.len() as isize,
                zeroes.as_ptr() as *const c_void,
            );
            (self.gl.glBindBuffer)(GL_UNIFORM_BUFFER, 0);
            for (names, binding) in [
                (["block_GlobalParams_0", "GlobalParams"].as_slice(), 3u32),
                (
                    ["block_KittyCustomShaderData_0", "KittyCustomShaderData"].as_slice(),
                    4u32,
                ),
            ] {
                for block_name in names {
                    if let Ok(name) = CString::new(*block_name) {
                        let index = (self.gl.glGetUniformBlockIndex)(program, name.as_ptr());
                        if index != u32::MAX {
                            (self.gl.glUniformBlockBinding)(program, index, binding);
                            break;
                        }
                    }
                }
            }
        }
        self.destroy_custom_pipeline();
        // SAFETY: a context is current; the capture texture is owned by the
        // installed pipeline struct.
        let mut source_texture = 0u32;
        unsafe { (self.gl.glGenTextures)(1, &mut source_texture) };
        self.custom_pipeline = Some(InstalledCustomPipeline {
            pipeline: pipeline.clone(),
            program,
            uniforms,
            data_ubo,
            shader_data_ubo,
            source_texture,
            named: [None, None, None],
            ping: [None, None],
            width: 0,
            height: 0,
        });
        Ok(())
    }

    /// Compile and install a pipeline using the compiler located from the
    /// environment.
    pub fn install_custom_pipeline_from_env(
        &mut self,
        pipeline: &CustomShaderPipeline,
        sources: &[Vec<String>],
    ) -> Result<(), GlBackendError> {
        let compiler = crate::shaders::SlangCompiler::from_env()
            .map_err(|error| GlBackendError::CustomShader(error.to_string()))?;
        self.install_custom_pipeline(&compiler, pipeline, sources)
    }

    /// Whether a custom-shader pipeline is installed.
    pub fn has_custom_pipeline(&self) -> bool {
        self.custom_pipeline.is_some()
    }

    /// Installed pipeline definition, if any.
    pub fn custom_pipeline(&self) -> Option<&CustomShaderPipeline> {
        self.custom_pipeline
            .as_ref()
            .map(|installed| &installed.pipeline)
    }

    /// Remove the installed pipeline and free its GPU resources.
    pub fn remove_custom_pipeline(&mut self) {
        self.destroy_custom_pipeline();
    }

    fn destroy_custom_pipeline(&mut self) {
        let Some(mut installed) = self.custom_pipeline.take() else {
            return;
        };
        // SAFETY: all ids are owned by the installed pipeline and a context is
        // current during install/remove/teardown.
        unsafe {
            (self.gl.glDeleteProgram)(installed.program);
            (self.gl.glDeleteBuffers)(1, &installed.data_ubo);
            (self.gl.glDeleteBuffers)(1, &installed.shader_data_ubo);
            if installed.source_texture != 0 {
                (self.gl.glDeleteTextures)(1, &installed.source_texture);
            }
            for target in installed
                .named
                .iter_mut()
                .chain(installed.ping.iter_mut())
                .flatten()
            {
                (self.gl.glDeleteFramebuffers)(1, &target.framebuffer);
                (self.gl.glDeleteTextures)(1, &target.texture);
            }
        }
    }

    /// Run the installed custom-shader pipeline over the current framebuffer.
    ///
    /// `events` is this tick's [`ShaderEventMask`]; `now_ms` is a monotonic
    /// millisecond clock. The first call initialises the animation state if
    /// `state` does not match the installed pipeline. Returns the number of
    /// group passes actually drawn (zero when every group is inactive).
    pub fn render_custom_pipeline(
        &mut self,
        state: &mut CustomShaderAnimationState,
        events: ShaderEventMask,
        now_ms: u64,
        cursor_stop_ms: i64,
    ) -> Result<usize, GlBackendError> {
        self.ensure_current()?;
        let Some(installed) = self.custom_pipeline.as_mut() else {
            return Err(GlBackendError::CustomShader(
                "no custom shader pipeline installed".into(),
            ));
        };
        let pipeline = installed.pipeline.clone();
        if state.groups.len() != pipeline.groups.len() {
            *state = CustomShaderAnimationState::new(&pipeline);
        }
        state.update(&pipeline, events, now_ms, cursor_stop_ms);
        if !state.has_active_shaders {
            return Ok(0);
        }
        let progresses = state.progresses(&pipeline, now_ms, cursor_stop_ms);
        let width = self.width;
        let height = self.height;
        let gl = self.gl;
        // SAFETY: a context is current; every id used below is owned by this
        // backend or the installed pipeline.
        unsafe {
            if installed.width != width || installed.height != height {
                for target in installed
                    .named
                    .iter_mut()
                    .chain(installed.ping.iter_mut())
                    .flatten()
                {
                    (gl.glDeleteFramebuffers)(1, &target.framebuffer);
                    (gl.glDeleteTextures)(1, &target.texture);
                }
                installed.named = [None, None, None];
                installed.ping = [None, None];
                (gl.glBindTexture)(GL_TEXTURE_2D, installed.source_texture);
                (gl.glTexParameteri)(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR as i32);
                (gl.glTexParameteri)(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR as i32);
                (gl.glTexParameteri)(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE as i32);
                (gl.glTexParameteri)(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE as i32);
                (gl.glTexImage2D)(
                    GL_TEXTURE_2D,
                    0,
                    GL_RGBA8 as i32,
                    width as i32,
                    height as i32,
                    0,
                    GL_RGBA,
                    GL_UNSIGNED_BYTE,
                    ptr::null(),
                );
                installed.width = width;
                installed.height = height;
            } else {
                (gl.glBindTexture)(GL_TEXTURE_2D, installed.source_texture);
            }
            (gl.glCopyTexSubImage2D)(GL_TEXTURE_2D, 0, 0, 0, 0, 0, width as i32, height as i32);
            let active_bits = pipeline
                .groups
                .iter()
                .enumerate()
                .fold(0u32, |mask, (index, _)| {
                    mask | if state.group_active(&pipeline, index) {
                        1u32 << index.min(31)
                    } else {
                        0
                    }
                });
            let last_active = pipeline
                .groups
                .iter()
                .enumerate()
                .filter(|(index, _)| state.group_active(&pipeline, *index))
                .map(|(index, _)| index as i32)
                .next_back()
                .unwrap_or(-1);
            if last_active < 0 {
                return Ok(0);
            }
            (gl.glBindBuffer)(GL_UNIFORM_BUFFER, installed.shader_data_ubo);
            (gl.glBindBufferBase)(GL_UNIFORM_BUFFER, 4, installed.shader_data_ubo);
            (gl.glBindBuffer)(GL_UNIFORM_BUFFER, 0);
            let mut current_texture = installed.source_texture;
            let mut current_is_srgb = true;
            let mut ping_index = 0usize;
            let mut passes = 0usize;
            (gl.glDisable)(GL_BLEND);
            (gl.glDisable)(GL_SCISSOR_TEST);
            (gl.glBindVertexArray)(self.vao);
            for (group_index, group) in pipeline.groups.iter().enumerate() {
                if !state.group_active(&pipeline, group_index) {
                    continue;
                }
                let is_last = group_index as i32 == last_active;
                let (output_framebuffer, output_texture) = if is_last {
                    (0, None)
                } else if group.output_texture == NamedTexture::Default {
                    let target =
                        ensure_offscreen(&gl, &mut installed.ping[ping_index], width, height)?;
                    ping_index ^= 1;
                    (target.framebuffer, Some(target.texture))
                } else {
                    let slot = match group.output_texture {
                        NamedTexture::A => 0,
                        NamedTexture::B => 1,
                        NamedTexture::Persist => 2,
                        NamedTexture::Default => unreachable!(),
                    };
                    let target = ensure_offscreen(&gl, &mut installed.named[slot], width, height)?;
                    (target.framebuffer, Some(target.texture))
                };
                let updates_backbuffer = !is_last && group.output_texture == NamedTexture::Default;
                (gl.glBindFramebuffer)(GL_FRAMEBUFFER, output_framebuffer);
                if is_last {
                    (gl.glViewport)(0, 0, width as i32, height as i32);
                } else {
                    let x0 = (group.viewport_pos.0 * width as f32).floor().max(0.0) as i32;
                    let y0 = (group.viewport_pos.1 * height as f32).floor().max(0.0) as i32;
                    let x1 = ((group.viewport_pos.0 + group.viewport_size.0) * width as f32)
                        .ceil()
                        .min(width as f32) as i32;
                    let y1 = ((group.viewport_pos.1 + group.viewport_size.1) * height as f32)
                        .ceil()
                        .min(height as f32) as i32;
                    (gl.glViewport)(x0, y0, (x1 - x0).max(0), (y1 - y0).max(0));
                }
                let mut params = [0u8; 64];
                put_u32(&mut params, 0, group_index as u32);
                put_f32(&mut params, 16, group.viewport_pos.0);
                put_f32(&mut params, 20, group.viewport_pos.1);
                put_f32(&mut params, 24, group.viewport_size.0);
                put_f32(&mut params, 28, group.viewport_size.1);
                put_f32(&mut params, 32, progresses[group_index]);
                put_u32(&mut params, 36, u32::from(is_last));
                put_u32(&mut params, 40, active_bits);
                put_u32(&mut params, 44, u32::from(current_is_srgb));
                put_f32(&mut params, 48, width as f32);
                put_f32(&mut params, 52, height as f32);
                (gl.glBindBuffer)(GL_UNIFORM_BUFFER, installed.data_ubo);
                (gl.glBufferSubData)(
                    GL_UNIFORM_BUFFER,
                    0,
                    params.len() as isize,
                    params.as_ptr() as *const c_void,
                );
                (gl.glBindBufferBase)(GL_UNIFORM_BUFFER, 3, installed.data_ubo);
                (gl.glBindBuffer)(GL_UNIFORM_BUFFER, 0);
                (gl.glUseProgram)(installed.program);
                (gl.glUniform4f)(installed.uniform("src_rect"), 0.0, 0.0, 1.0, 1.0);
                (gl.glUniform4f)(installed.uniform("dest_rect"), -1.0, -1.0, 1.0, 1.0);
                (gl.glUniform1i)(installed.uniform("group"), group_index as i32);
                (gl.glUniform4f)(
                    installed.uniform("viewport"),
                    group.viewport_pos.0,
                    group.viewport_pos.1,
                    group.viewport_size.0,
                    group.viewport_size.1,
                );
                (gl.glUniform1f)(
                    installed.uniform("animation_progress"),
                    progresses[group_index],
                );
                (gl.glUniform1i)(installed.uniform("convert_to_srgb"), i32::from(is_last));
                (gl.glUniform1ui)(installed.uniform("active_groups"), active_bits);
                (gl.glUniform1ui)(
                    installed.uniform("source_is_srgb"),
                    u32::from(current_is_srgb),
                );
                (gl.glUniform2f)(
                    installed.uniform("viewport_size_pixels"),
                    width as f32,
                    height as f32,
                );
                for (name, unit) in [("backbuffer", 0u32), ("a", 1), ("b", 2), ("persist", 3)] {
                    (gl.glUniform1i)(installed.uniform(name), unit as i32);
                }
                (gl.glActiveTexture)(GL_TEXTURE0);
                (gl.glBindTexture)(GL_TEXTURE_2D, current_texture);
                for (slot, unit, name) in [(0usize, 1u32, "a"), (1, 2, "b"), (2, 3, "persist")] {
                    let declared = pipeline.textures.iter().any(|texture| {
                        matches!(
                            (*texture, name),
                            (NamedTexture::A, "a")
                                | (NamedTexture::B, "b")
                                | (NamedTexture::Persist, "persist")
                        )
                    });
                    let is_output = !is_last
                        && matches!(
                            (group.output_texture, name),
                            (NamedTexture::A, "a")
                                | (NamedTexture::B, "b")
                                | (NamedTexture::Persist, "persist")
                        );
                    let texture = if !declared || is_output {
                        current_texture
                    } else {
                        let target =
                            ensure_offscreen(&gl, &mut installed.named[slot], width, height)?;
                        target.texture
                    };
                    (gl.glActiveTexture)(GL_TEXTURE0 + unit);
                    (gl.glBindTexture)(GL_TEXTURE_2D, texture);
                }
                (gl.glActiveTexture)(GL_TEXTURE0);
                (gl.glDrawArrays)(GL_TRIANGLE_FAN, 0, 4);
                passes += 1;
                if updates_backbuffer {
                    if let Some(texture) = output_texture {
                        current_texture = texture;
                        current_is_srgb = false;
                    }
                }
            }
            (gl.glBindFramebuffer)(GL_FRAMEBUFFER, 0);
            (gl.glViewport)(0, 0, width as i32, height as i32);
            (gl.glBindVertexArray)(0);
            (gl.glUseProgram)(0);
            for unit in 0..4 {
                (gl.glActiveTexture)(GL_TEXTURE0 + unit);
                (gl.glBindTexture)(GL_TEXTURE_2D, 0);
            }
            (gl.glActiveTexture)(GL_TEXTURE0);
            (gl.glEnable)(GL_BLEND);
            (gl.glBlendFunc)(GL_ONE, GL_ONE_MINUS_SRC_ALPHA);
            (gl.glEnable)(GL_SCISSOR_TEST);
            self.gl.check("custom pipeline draw")?;
            Ok(passes)
        }
    }

    /// Whether a user shader has been installed successfully.
    pub fn has_custom_shader(&self) -> bool {
        self.custom_program != 0
    }

    /// Whether a texture for `key` is currently resident.
    #[inline]
    pub fn has_image_texture(&self, key: ImageKey) -> bool {
        self.image_textures.contains_key(&key)
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
            if self.cell_programs {
                self.sync_cell_sprites(atlas, frame)?;
            }
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
        if self.cell_programs {
            self.sync_cell_sprites(atlas, frame)?;
        }
        Ok(stats)
    }

    fn sync_cell_sprites(
        &mut self,
        atlas: &GlyphAtlas,
        frame: &Frame,
    ) -> Result<(), GlBackendError> {
        let Some(data) = build_cell_sprites(atlas, frame) else {
            self.cell_sprites = None;
            return Ok(());
        };
        let width = data.sprites_xnum * data.cell_width;
        let height = data.sprites_ynum * data.tile_height;
        let layers = data.layers;
        let decorate_width = (data.decorations.len() as u32).max(1);
        let recreate = self.cell_sprites.as_ref().is_none_or(|sprites| {
            sprites.data.sprites_xnum != data.sprites_xnum
                || sprites.data.sprites_ynum != data.sprites_ynum
                || sprites.data.cell_width != data.cell_width
                || sprites.data.tile_height != data.tile_height
                || sprites.data.layers != data.layers
                || sprites.data.decorations.len() != data.decorations.len()
        });
        // SAFETY: a context is current; all data pointers outlive the calls.
        unsafe {
            (self.gl.glActiveTexture)(GL_TEXTURE0);
            (self.gl.glPixelStorei)(GL_UNPACK_ALIGNMENT, 1);
            if recreate {
                if let Some(old) = self.cell_sprites.take() {
                    let textures = [old.texture, old.decorations];
                    (self.gl.glDeleteTextures)(2, textures.as_ptr());
                }
                let mut textures = [0u32; 2];
                (self.gl.glGenTextures)(2, textures.as_mut_ptr());
                if textures.contains(&0) {
                    return Err(GlError {
                        code: 1,
                        operation: "glGenTextures",
                    }
                    .into());
                }
                (self.gl.glBindTexture)(GL_TEXTURE_2D_ARRAY, textures[0]);
                (self.gl.glTexParameteri)(
                    GL_TEXTURE_2D_ARRAY,
                    GL_TEXTURE_MIN_FILTER,
                    GL_NEAREST as i32,
                );
                (self.gl.glTexParameteri)(
                    GL_TEXTURE_2D_ARRAY,
                    GL_TEXTURE_MAG_FILTER,
                    GL_NEAREST as i32,
                );
                (self.gl.glTexParameteri)(
                    GL_TEXTURE_2D_ARRAY,
                    GL_TEXTURE_WRAP_S,
                    GL_CLAMP_TO_EDGE as i32,
                );
                (self.gl.glTexParameteri)(
                    GL_TEXTURE_2D_ARRAY,
                    GL_TEXTURE_WRAP_T,
                    GL_CLAMP_TO_EDGE as i32,
                );
                (self.gl.glTexImage3D)(
                    GL_TEXTURE_2D_ARRAY,
                    0,
                    GL_SRGB8_ALPHA8 as i32,
                    width as i32,
                    height as i32,
                    layers as i32,
                    0,
                    GL_RGBA,
                    GL_UNSIGNED_BYTE,
                    data.pixels.as_ptr() as *const c_void,
                );
                (self.gl.glBindTexture)(GL_TEXTURE_2D, textures[1]);
                (self.gl.glTexParameteri)(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_NEAREST as i32);
                (self.gl.glTexParameteri)(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_NEAREST as i32);
                (self.gl.glTexImage2D)(
                    GL_TEXTURE_2D,
                    0,
                    GL_R32UI as i32,
                    decorate_width as i32,
                    1,
                    0,
                    GL_RED_INTEGER,
                    GL_UNSIGNED_INT,
                    data.decorations.as_ptr() as *const c_void,
                );
                self.cell_sprites = Some(CellSprites {
                    texture: textures[0],
                    decorations: textures[1],
                    data,
                });
            } else if let Some(sprites) = self.cell_sprites.as_mut() {
                (self.gl.glBindTexture)(GL_TEXTURE_2D_ARRAY, sprites.texture);
                (self.gl.glTexSubImage3D)(
                    GL_TEXTURE_2D_ARRAY,
                    0,
                    0,
                    0,
                    0,
                    width as i32,
                    height as i32,
                    layers as i32,
                    GL_RGBA,
                    GL_UNSIGNED_BYTE,
                    data.pixels.as_ptr() as *const c_void,
                );
                (self.gl.glBindTexture)(GL_TEXTURE_2D, sprites.decorations);
                (self.gl.glTexSubImage2D)(
                    GL_TEXTURE_2D,
                    0,
                    0,
                    0,
                    decorate_width as i32,
                    1,
                    GL_RED_INTEGER,
                    GL_UNSIGNED_INT,
                    data.decorations.as_ptr() as *const c_void,
                );
                sprites.data = data;
            }
            (self.gl.glBindTexture)(GL_TEXTURE_2D_ARRAY, 0);
            (self.gl.glBindTexture)(GL_TEXTURE_2D, 0);
            (self.gl.glPixelStorei)(GL_UNPACK_ALIGNMENT, 4);
        }
        self.gl.check("cell sprite upload")?;
        Ok(())
    }

    /// Upload image textures referenced by `frame` and delete unreferenced
    /// ones.
    ///
    /// Textures are keyed by [`ImageKey`]. Uploads whose content fingerprint,
    /// dimensions, filter and wrap mode are unchanged are skipped. A texture
    /// that is no longer referenced by the frame (after a graphics-protocol
    /// delete, or after clearing the background/logo) is removed, which is how
    /// `a=d` becomes visible to the GPU.
    pub fn sync_images(&mut self, frame: &Frame) -> Result<ImageSyncStats, GlBackendError> {
        self.ensure_current()?;
        let mut stats = ImageSyncStats::default();

        let mut live: HashSet<ImageKey> = frame.image_uploads.iter().map(|u| u.key).collect();
        for image in &frame.images {
            live.insert(image.key);
        }
        if let Some(background) = &frame.background_image {
            live.insert(background.key);
        }
        if let Some(logo) = &frame.logo {
            live.insert(logo.key);
        }
        let stale: Vec<ImageKey> = self
            .image_textures
            .keys()
            .copied()
            .filter(|key| !live.contains(key))
            .collect();
        for key in stale {
            if let Some(cached) = self.image_textures.remove(&key) {
                // SAFETY: the texture is owned by this backend.
                unsafe { (self.gl.glDeleteTextures)(1, &cached.texture) };
                stats.removed += 1;
            }
        }

        if frame.image_uploads.is_empty() {
            return Ok(stats);
        }

        // SAFETY: a context is current; upload data outlives each call.
        unsafe {
            (self.gl.glActiveTexture)(GL_TEXTURE0);
        }
        for upload in &frame.image_uploads {
            if !upload.is_valid() {
                continue;
            }
            if upload.indexed_format.is_some() {
                if let Some(palette) = upload.palette.as_ref() {
                    self.upload_indexed_palette(palette);
                }
            }
            // Indexed uploads stay integer textures. The indexed-graphics
            // shader performs the palette lookup against the live ColorTable
            // UBO, so terminal colour changes never re-upload image pixels.
            let indexed_format = upload.indexed_format;
            let indexed = indexed_format.is_some();
            let (internal_format, pixel_format) = match indexed_format {
                Some(ImageFormat::IndexedAlpha) => (GL_RG8UI, GL_RG_INTEGER),
                Some(_) => (GL_R8UI, GL_RED_INTEGER),
                None => (GL_RGBA8, GL_RGBA),
            };
            let bytes_per_pixel = upload.bytes_per_pixel();
            let alignment = if indexed || bytes_per_pixel != 4 {
                1
            } else {
                4
            };
            // SAFETY: a context is current; alignment matches the tightly
            // packed upload buffer.
            unsafe {
                (self.gl.glPixelStorei)(GL_UNPACK_ALIGNMENT, alignment);
            }
            let pixel_data: &[u8] = &upload.data;
            let current = self.image_textures.get(&upload.key).copied();
            let recreate = current.is_none_or(|cached| {
                cached.width != upload.width
                    || cached.height != upload.height
                    || cached.filter != upload.filter
                    || cached.wrap != upload.wrap
                    || cached.indexed_format != indexed_format
            });
            if recreate {
                if let Some(old) = self.image_textures.remove(&upload.key) {
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
                // SAFETY: the newly created texture is bound before any use.
                unsafe {
                    (self.gl.glBindTexture)(GL_TEXTURE_2D, texture);
                    // Integer textures cannot be filtered; the indexed shader
                    // performs its own bilinear lookup after the palette fetch.
                    let filter = if indexed {
                        GL_NEAREST
                    } else {
                        image_filter_gl(upload.filter)
                    } as i32;
                    (self.gl.glTexParameteri)(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, filter);
                    (self.gl.glTexParameteri)(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, filter);
                    (self.gl.glTexParameteri)(
                        GL_TEXTURE_2D,
                        GL_TEXTURE_WRAP_S,
                        image_wrap_gl(upload.wrap) as i32,
                    );
                    (self.gl.glTexParameteri)(
                        GL_TEXTURE_2D,
                        GL_TEXTURE_WRAP_T,
                        image_wrap_gl(upload.wrap) as i32,
                    );
                    (self.gl.glTexImage2D)(
                        GL_TEXTURE_2D,
                        0,
                        internal_format as i32,
                        upload.width as i32,
                        upload.height as i32,
                        0,
                        pixel_format,
                        GL_UNSIGNED_BYTE,
                        pixel_data.as_ptr() as *const c_void,
                    );
                }
                self.image_textures.insert(
                    upload.key,
                    CachedImage {
                        texture,
                        width: upload.width,
                        height: upload.height,
                        filter: upload.filter,
                        wrap: upload.wrap,
                        indexed_format,
                        fingerprint: upload.fingerprint,
                    },
                );
                stats.created += 1;
            } else {
                let Some(cached) = self.image_textures.get_mut(&upload.key) else {
                    continue;
                };
                if cached.fingerprint == upload.fingerprint {
                    stats.unchanged += 1;
                    continue;
                }
                // SAFETY: the texture and upload data are valid for this call.
                unsafe {
                    (self.gl.glBindTexture)(GL_TEXTURE_2D, cached.texture);
                    (self.gl.glTexSubImage2D)(
                        GL_TEXTURE_2D,
                        0,
                        0,
                        0,
                        upload.width as i32,
                        upload.height as i32,
                        pixel_format,
                        GL_UNSIGNED_BYTE,
                        pixel_data.as_ptr() as *const c_void,
                    );
                }
                cached.fingerprint = upload.fingerprint;
                stats.updated += 1;
            }
        }
        // SAFETY: restoring conventional state.
        unsafe {
            (self.gl.glBindTexture)(GL_TEXTURE_2D, 0);
            (self.gl.glPixelStorei)(GL_UNPACK_ALIGNMENT, 4);
        }
        self.gl.check("image upload")?;
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
        self.sync_images(frame)?;
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
        self.config.text_contrast = config.text_contrast.max(0.0);
        self.config.text_gamma_adjustment = config.text_gamma_adjustment.max(0.01);
        if let Some(palette) = frame.indexed_palette.as_ref() {
            self.upload_indexed_palette(palette);
        }
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
        self.upload_cell_instances(frame)?;
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

    /// Draw an already-resident 2D `texture` over the whole drawable with the
    /// real `blit` program.
    ///
    /// `src_rect` and `dest_rect` use the same convention as `blit.slang`:
    /// source coordinates are normalised texture coordinates and destination
    /// coordinates are NDC. The source is treated as premultiplied linear RGB
    /// and converted to sRGB, exactly like kitty's offscreen-to-window blit.
    pub fn blit_texture(
        &mut self,
        texture: u32,
        src_rect: [f32; 4],
        dest_rect: [f32; 4],
    ) -> Result<(), GlBackendError> {
        self.ensure_current()?;
        // SAFETY: a context is current and the program/texture ids are live.
        unsafe {
            (self.gl.glBindFramebuffer)(GL_FRAMEBUFFER, 0);
            (self.gl.glViewport)(0, 0, self.width as i32, self.height as i32);
            let program = self.programs.get(crate::shaders::ProgramKind::Blit);
            (self.gl.glUseProgram)(program.id);
            (self.gl.glUniform4f)(
                program.uniform("src_rect"),
                src_rect[0],
                src_rect[1],
                src_rect[2],
                src_rect[3],
            );
            (self.gl.glUniform4f)(
                program.uniform("dest_rect"),
                dest_rect[0],
                dest_rect[1],
                dest_rect[2],
                dest_rect[3],
            );
            (self.gl.glUniform1i)(program.uniform("image"), 0);
            (self.gl.glActiveTexture)(GL_TEXTURE0);
            (self.gl.glBindTexture)(GL_TEXTURE_2D, texture);
            (self.gl.glBindVertexArray)(self.vao);
            (self.gl.glDrawArrays)(GL_TRIANGLE_FAN, 0, 4);
            (self.gl.glBindVertexArray)(0);
            (self.gl.glBindTexture)(GL_TEXTURE_2D, 0);
            (self.gl.glUseProgram)(0);
        }
        self.gl.check("blit")?;
        Ok(())
    }

    /// Capture a rectangular region of the current framebuffer.
    ///
    /// With `no_scaling` the region is read back 1:1. Otherwise the real
    /// `screenshot` program performs the colour-space-correct downsample into
    /// a `thumb_width` x `thumb_height` target (pass zero to keep the source
    /// dimensions). Pixels are returned top-down RGBA8, matching
    /// [`read_pixels`](Self::read_pixels).
    pub fn screenshot_region(
        &mut self,
        region: Region,
        thumb_width: u32,
        thumb_height: u32,
        no_scaling: bool,
    ) -> Result<Screenshot, GlBackendError> {
        self.ensure_current()?;
        let vw = self.width;
        let vh = self.height;
        let left = region.left.floor().max(0.0).min(vw as f32) as u32;
        let top = region.top.floor().max(0.0).min(vh as f32) as u32;
        let right = region.right.ceil().max(0.0).min(vw as f32) as u32;
        let bottom = region.bottom.ceil().max(0.0).min(vh as f32) as u32;
        if right <= left || bottom <= top {
            return Ok(Screenshot {
                width: 0,
                height: 0,
                pixels: Vec::new(),
            });
        }
        let src_width = right - left;
        let src_height = bottom - top;
        if no_scaling {
            let mut pixels = vec![0u8; src_width as usize * src_height as usize * 4];
            // SAFETY: the buffer is exactly the requested size and a context
            // is current. GL rows are bottom-up, flipped below.
            unsafe {
                (self.gl.glBindFramebuffer)(GL_FRAMEBUFFER, 0);
                (self.gl.glPixelStorei)(GL_PACK_ALIGNMENT, 1);
                (self.gl.glReadPixels)(
                    left as i32,
                    (vh - bottom) as i32,
                    src_width as i32,
                    src_height as i32,
                    GL_RGBA,
                    GL_UNSIGNED_BYTE,
                    pixels.as_mut_ptr().cast(),
                );
                (self.gl.glFinish)();
            }
            self.gl.check("screenshot readback")?;
            flip_rows(&mut pixels, src_width, src_height);
            return Ok(Screenshot {
                width: src_width,
                height: src_height,
                pixels,
            });
        }

        let width = if thumb_width == 0 {
            src_width
        } else {
            thumb_width.min(src_width)
        };
        let height = if thumb_height == 0 {
            src_height
        } else {
            thumb_height.min(src_height)
        };
        if width == 0 || height == 0 {
            return Ok(Screenshot {
                width: 0,
                height: 0,
                pixels: Vec::new(),
            });
        }
        let mut src_texture = 0u32;
        // SAFETY: a context is current; all resources are owned here.
        unsafe {
            (self.gl.glGenTextures)(1, &mut src_texture);
            (self.gl.glActiveTexture)(GL_TEXTURE0);
            (self.gl.glBindTexture)(GL_TEXTURE_2D, src_texture);
            (self.gl.glTexParameteri)(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR as i32);
            (self.gl.glTexParameteri)(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR as i32);
            (self.gl.glTexParameteri)(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE as i32);
            (self.gl.glTexParameteri)(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE as i32);
            (self.gl.glBindFramebuffer)(GL_FRAMEBUFFER, 0);
            (self.gl.glCopyTexImage2D)(GL_TEXTURE_2D, 0, GL_RGBA, 0, 0, vw as i32, vh as i32, 0);
        }
        let target = unsafe { create_offscreen_target(&self.gl, width, height, GL_RGBA8 as i32)? };
        let mut pixels = vec![0u8; width as usize * height as usize * 4];
        // SAFETY: a context is current and the temporary framebuffer/texture
        // are live for the duration of these calls.
        unsafe {
            (self.gl.glBindFramebuffer)(GL_FRAMEBUFFER, target.framebuffer);
            (self.gl.glViewport)(0, 0, width as i32, height as i32);
            (self.gl.glDisable)(GL_SCISSOR_TEST);
            let program = self.programs.get(crate::shaders::ProgramKind::Screenshot);
            (self.gl.glUseProgram)(program.id);
            let left_norm = left as f32 / vw as f32;
            let right_norm = right as f32 / vw as f32;
            let top_norm = (vh - bottom) as f32 / vh as f32;
            let bottom_norm = (vh - top) as f32 / vh as f32;
            (self.gl.glUniform4f)(
                program.uniform("src_rect"),
                left_norm,
                top_norm,
                right_norm,
                bottom_norm,
            );
            (self.gl.glUniform4f)(program.uniform("dest_rect"), -1.0, -1.0, 1.0, 1.0);
            (self.gl.glUniform2f)(
                program.uniform("src_size"),
                src_width as f32,
                src_height as f32,
            );
            (self.gl.glUniform1i)(program.uniform("image"), 0);
            (self.gl.glBindTexture)(GL_TEXTURE_2D, src_texture);
            (self.gl.glBindVertexArray)(self.vao);
            (self.gl.glDrawArrays)(GL_TRIANGLE_FAN, 0, 4);
            (self.gl.glBindVertexArray)(0);
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
            (self.gl.glBindFramebuffer)(GL_FRAMEBUFFER, 0);
            (self.gl.glBindTexture)(GL_TEXTURE_2D, 0);
            (self.gl.glUseProgram)(0);
            (self.gl.glDeleteFramebuffers)(1, &target.framebuffer);
            (self.gl.glDeleteTextures)(1, &target.texture);
            (self.gl.glDeleteTextures)(1, &src_texture);
            (self.gl.glPixelStorei)(GL_PACK_ALIGNMENT, 4);
        }
        self.gl.check("screenshot program")?;
        flip_rows(&mut pixels, width, height);
        Ok(Screenshot {
            width,
            height,
            pixels,
        })
    }

    /// Alias for [`screenshot_region`](Self::screenshot_region).
    pub fn take_screenshot(
        &mut self,
        region: Region,
        thumb_width: u32,
        thumb_height: u32,
        no_scaling: bool,
    ) -> Result<Screenshot, GlBackendError> {
        self.screenshot_region(region, thumb_width, thumb_height, no_scaling)
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

    fn push_images(&mut self, images: &[crate::images::ImageInstance], stats: &mut RenderStats) {
        let mut index = 0usize;
        while index < images.len() {
            let key = images[index].key;
            if !self.image_textures.contains_key(&key) {
                while index < images.len() && images[index].key == key {
                    stats.images_skipped += 1;
                    index += 1;
                }
                continue;
            }
            let start = self.instances.len();
            while index < images.len() && images[index].key == key {
                let image = &images[index];
                let color = self.storage_color(image.color);
                self.push_quad(
                    image.rect,
                    [image.uv.u0, image.uv.v0, image.uv.u1, image.uv.v1],
                    [color[0], color[1], color[2], 1.0],
                    [image.tint, image.opacity, 0.0, 0.0],
                );
                stats.images_drawn += 1;
                index += 1;
            }
            if self.instances.len() > start {
                self.draw_calls.push(DrawCall {
                    kind: BatchKind::Image(key),
                    first: start as i32,
                    count: (self.instances.len() - start) as i32,
                });
            }
        }
    }

    fn push_image_layer(
        &mut self,
        images: &[crate::images::ImageInstance],
        layer: crate::images::ImageLayer,
        stats: &mut RenderStats,
    ) {
        let mut index = 0usize;
        while index < images.len() {
            if images[index].layer != layer {
                index += 1;
                continue;
            }
            let key = images[index].key;
            let missing = !self.image_textures.contains_key(&key);
            let start = self.instances.len();
            while index < images.len() && images[index].layer == layer && images[index].key == key {
                if missing {
                    stats.images_skipped += 1;
                } else {
                    let image = &images[index];
                    let color = self.storage_color(image.color);
                    self.push_quad(
                        image.rect,
                        [image.uv.u0, image.uv.v0, image.uv.u1, image.uv.v1],
                        [color[0], color[1], color[2], 1.0],
                        [image.tint, image.opacity, 0.0, 0.0],
                    );
                    stats.images_drawn += 1;
                }
                index += 1;
            }
            if !missing && self.instances.len() > start {
                self.draw_calls.push(DrawCall {
                    kind: BatchKind::Image(key),
                    first: start as i32,
                    count: (self.instances.len() - start) as i32,
                });
            }
        }
    }

    fn build_instances(&mut self, frame: &Frame, stats: &mut RenderStats) {
        stats.images = frame.images.len();

        // The background image is the bottom-most layer. When one is present,
        // default-background cell quads are skipped so the image remains
        // visible behind text; explicitly coloured cells still draw on top.
        if let Some(background) = &frame.background_image {
            if self.image_textures.contains_key(&background.key) {
                stats.background_image_drawn = true;
                let tiled =
                    (background.uv.u1 - 1.0).abs() > 1e-6 || (background.uv.v1 - 1.0).abs() > 1e-6;
                // bgimage has no separate opacity input. Transparent
                // backgrounds keep the explicit UV quad path, which supports
                // arbitrary alpha and tint combinations.
                let uv_origin_clear =
                    background.uv.u0.abs() < 1e-6 && background.uv.v0.abs() < 1e-6;
                if background.opacity >= 0.999 && uv_origin_clear {
                    let cached = self.image_textures.get(&background.key).copied();
                    let (width, height) = cached
                        .map(|image| (image.width as f32, image.height as f32))
                        .unwrap_or((1.0, 1.0));
                    let start = self.instances.len();
                    self.push_quad(
                        background.rect,
                        [0.0; 4],
                        [0.0; 4],
                        [f32::from(tiled), width, height, background.tint],
                    );
                    self.draw_calls.push(DrawCall {
                        kind: BatchKind::BackgroundImage(background.key),
                        first: start as i32,
                        count: 1,
                    });
                    stats.images_drawn += 1;
                } else {
                    self.push_images(std::slice::from_ref(background), stats);
                }
            }
        }

        self.push_image_layer(
            &frame.images,
            crate::images::ImageLayer::BelowBackground,
            stats,
        );

        let cell_mode = self.cell_programs
            && self
                .cell_sprites
                .as_ref()
                .is_some_and(|sprites| !sprites.data.cells.is_empty());
        if cell_mode {
            // The cell shader paints a complete fixed grid. When something has
            // to appear between backgrounds and foreground (the logo or a
            // below-text image) the background and foreground passes are
            // split, exactly like kitty's CELL_BG/CELL_FG draws.
            let has_between = frame.logo.is_some()
                || frame
                    .images
                    .iter()
                    .any(|image| image.layer == crate::images::ImageLayer::BelowText);
            let has_background_image = self.cell_background_image_ready(frame);
            if has_between && !has_background_image {
                self.push_cell_call(BatchKind::CellBackgroundDefault);
            }
            if let Some(logo) = &frame.logo {
                if self.image_textures.contains_key(&logo.key) {
                    stats.logo_drawn = true;
                    self.push_images(std::slice::from_ref(logo), stats);
                }
            }
            self.push_image_layer(&frame.images, crate::images::ImageLayer::BelowText, stats);
            if has_between {
                self.push_cell_call(BatchKind::CellBackgroundNonDefault);
                self.push_cell_call(BatchKind::CellForeground);
            } else if has_background_image {
                self.push_cell_call(BatchKind::CellBothNonDefault);
            } else {
                self.push_cell_call(BatchKind::CellBoth);
            }
            if let Some(sprites) = &self.cell_sprites {
                stats.cell_instances = sprites.data.cells.len();
                stats.cell_programs = true;
            }
        } else {
            // Cell backgrounds.
            let start = self.instances.len();
            for background in &frame.backgrounds {
                if frame.background_image.is_some() && background.is_default_bg {
                    continue;
                }
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

            // Window logo sits between the background and the text.
            if let Some(logo) = &frame.logo {
                if self.image_textures.contains_key(&logo.key) {
                    stats.logo_drawn = true;
                    self.push_images(std::slice::from_ref(logo), stats);
                }
            }

            self.push_image_layer(&frame.images, crate::images::ImageLayer::BelowText, stats);

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

        self.push_image_layer(&frame.images, crate::images::ImageLayer::AboveText, stats);

        // Cursor trail, then the cursor itself so the cursor always wins.
        let start = self.instances.len();
        for trail in &frame.trail {
            let mut color = self.storage_color(trail.color);
            color[3] *= trail.opacity.clamp(0.0, 1.0);
            self.push_quad(trail.rect, [0.0; 4], color, [0.0; 4]);
            stats.trail_instances += 1;
        }
        if self.instances.len() > start {
            self.draw_calls.push(DrawCall {
                kind: BatchKind::Trail,
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

    fn cell_background_image_ready(&self, frame: &Frame) -> bool {
        frame
            .background_image
            .as_ref()
            .is_some_and(|image| self.image_textures.contains_key(&image.key))
    }

    fn push_cell_call(&mut self, kind: BatchKind) {
        self.draw_calls.push(DrawCall {
            kind,
            first: 0,
            count: 0,
        });
    }

    fn upload_cell_instances(&mut self, frame: &Frame) -> Result<(), GlBackendError> {
        if !self.cell_programs {
            return Ok(());
        }
        let Some(sprites) = self.cell_sprites.as_ref() else {
            return Ok(());
        };
        let count = sprites.data.cells.len();
        if count == 0 {
            return Ok(());
        }
        let mut colors = Vec::with_capacity(count * 4);
        let mut sprite_idx = Vec::with_capacity(count * 2);
        let mut selected = Vec::with_capacity(count);
        for cell in &sprites.data.cells {
            for value in cell.colors {
                colors.extend_from_slice(&value.to_ne_bytes());
            }
            sprite_idx.extend_from_slice(&cell.sprite_idx.to_ne_bytes());
            sprite_idx.extend_from_slice(&cell.attrs.to_ne_bytes());
            selected.push(cell.is_selected);
        }
        let data = pack_cell_render_data(&sprites.data, frame);
        let needed = colors.len().max(sprite_idx.len()).max(selected.len());
        // SAFETY: a context is current and all slices outlive the upload calls.
        unsafe {
            let buffers = [
                self.cell_colors_vbo,
                self.cell_sprite_vbo,
                self.cell_selection_vbo,
            ];
            let payloads: [&[u8]; 3] = [&colors, &sprite_idx, &selected];
            if needed > self.cell_instance_capacity {
                for (buffer, payload) in buffers.iter().zip(payloads.iter()) {
                    (self.gl.glBindBuffer)(GL_ARRAY_BUFFER, *buffer);
                    (self.gl.glBufferData)(
                        GL_ARRAY_BUFFER,
                        (needed * 2).max(4096) as isize,
                        ptr::null(),
                        GL_DYNAMIC_DRAW,
                    );
                    let _ = payload;
                }
                self.cell_instance_capacity = (needed * 2).max(4096);
            }
            for (buffer, payload) in buffers.iter().zip(payloads.iter()) {
                (self.gl.glBindBuffer)(GL_ARRAY_BUFFER, *buffer);
                (self.gl.glBufferSubData)(
                    GL_ARRAY_BUFFER,
                    0,
                    payload.len() as isize,
                    payload.as_ptr() as *const c_void,
                );
            }
            (self.gl.glBindBuffer)(GL_ARRAY_BUFFER, 0);
            (self.gl.glBindBuffer)(GL_UNIFORM_BUFFER, self.cell_data_ubo);
            (self.gl.glBufferSubData)(
                GL_UNIFORM_BUFFER,
                0,
                data.len() as isize,
                data.as_ptr() as *const c_void,
            );
            (self.gl.glBindBufferBase)(GL_UNIFORM_BUFFER, 0, self.cell_data_ubo);
        }
        self.gl.check("cell instance upload")?;
        Ok(())
    }

    fn ensure_cell_offscreen(&mut self) -> Result<(), GlBackendError> {
        let (width, height) = (self.width, self.height);
        let recreate = self
            .cell_offscreen
            .is_none_or(|target| target.width != width || target.height != height);
        if !recreate {
            return Ok(());
        }
        self.ensure_current()?;
        if let Some(old) = self.cell_offscreen.take() {
            // SAFETY: the objects are owned by this backend.
            unsafe {
                (self.gl.glDeleteFramebuffers)(1, &old.framebuffer);
                (self.gl.glDeleteTextures)(1, &old.texture);
            }
        }
        // SAFETY: a context is current.
        let target =
            unsafe { create_offscreen_target(&self.gl, width, height, GL_RGBA16F as i32)? };
        self.cell_offscreen = Some(target);
        Ok(())
    }

    fn draw_cell_layer(
        &mut self,
        kind: BatchKind,
        scissor: ScissorRect,
    ) -> Result<(), GlBackendError> {
        use crate::shaders::ProgramKind;
        let Some((sprites_texture, decorations_texture, cell_count)) =
            self.cell_sprites.as_ref().map(|sprites| {
                (
                    sprites.texture,
                    sprites.decorations,
                    sprites.data.cells.len(),
                )
            })
        else {
            return Ok(());
        };
        self.ensure_cell_offscreen()?;
        let Some(target) = self.cell_offscreen else {
            return Ok(());
        };
        let (program_kind, bitfield) = match kind {
            BatchKind::CellBoth => (ProgramKind::Cell, 3u32),
            BatchKind::CellBothNonDefault => (ProgramKind::Cell, 2),
            BatchKind::CellBackgroundDefault => (ProgramKind::CellBg, 1),
            BatchKind::CellBackgroundNonDefault => (ProgramKind::CellBg, 2),
            BatchKind::CellForeground => (ProgramKind::CellFg, 0),
            _ => return Ok(()),
        };
        let y = self.height as i32 - (scissor.y + scissor.height);
        // SAFETY: a context is current and all ids are live.
        unsafe {
            (self.gl.glBindFramebuffer)(GL_FRAMEBUFFER, target.framebuffer);
            (self.gl.glViewport)(0, 0, target.width as i32, target.height as i32);
            (self.gl.glEnable)(GL_SCISSOR_TEST);
            (self.gl.glScissor)(scissor.x, y, scissor.width, scissor.height);
            (self.gl.glClearColor)(0.0, 0.0, 0.0, 0.0);
            (self.gl.glClear)(GL_COLOR_BUFFER_BIT);

            let program = self.programs.get(program_kind);
            (self.gl.glUseProgram)(program.id);
            (self.gl.glUniform1ui)(program.uniform("draw_bg_bitfield"), bitfield);
            (self.gl.glUniform1i)(program.uniform("sprites"), 0);
            (self.gl.glUniform1i)(program.uniform("sprite_decorations_map"), 1);
            if program.uniform("text_contrast") >= 0 {
                (self.gl.glUniform1f)(
                    program.uniform("text_contrast"),
                    self.config.text_contrast.max(0.0),
                );
            }
            if program.uniform("text_gamma_adjustment") >= 0 {
                (self.gl.glUniform1f)(
                    program.uniform("text_gamma_adjustment"),
                    self.config.text_gamma_adjustment.max(0.01),
                );
            }
            (self.gl.glActiveTexture)(GL_TEXTURE0);
            (self.gl.glBindTexture)(GL_TEXTURE_2D_ARRAY, sprites_texture);
            (self.gl.glActiveTexture)(GL_TEXTURE0 + 1);
            (self.gl.glBindTexture)(GL_TEXTURE_2D, decorations_texture);
            (self.gl.glActiveTexture)(GL_TEXTURE0);
            (self.gl.glBindVertexArray)(self.cell_vao);
            (self.gl.glDrawArraysInstanced)(GL_TRIANGLE_FAN, 0, 4, cell_count as i32);
            (self.gl.glBindVertexArray)(0);
            self.gl.check("cell instanced draw")?;

            // Blit the linear cell layer to the sRGB output with kitty's blit
            // program (it performs the same linear2srgb conversion as the
            // final custom-shader pass).
            (self.gl.glBindFramebuffer)(GL_FRAMEBUFFER, 0);
            (self.gl.glViewport)(0, 0, self.width as i32, self.height as i32);
            (self.gl.glScissor)(scissor.x, y, scissor.width, scissor.height);
            let blit = self.programs.get(ProgramKind::Blit);
            (self.gl.glUseProgram)(blit.id);
            (self.gl.glUniform4f)(blit.uniform("src_rect"), 0.0, 0.0, 1.0, 1.0);
            (self.gl.glUniform4f)(blit.uniform("dest_rect"), -1.0, -1.0, 1.0, 1.0);
            (self.gl.glUniform1i)(blit.uniform("image"), 0);
            (self.gl.glBindTexture)(GL_TEXTURE_2D, target.texture);
            (self.gl.glBindVertexArray)(self.vao);
            (self.gl.glDrawArrays)(GL_TRIANGLE_FAN, 0, 4);
            (self.gl.glBindVertexArray)(0);
            (self.gl.glBindTexture)(GL_TEXTURE_2D, 0);
            (self.gl.glActiveTexture)(GL_TEXTURE0 + 1);
            (self.gl.glBindTexture)(GL_TEXTURE_2D, 0);
            (self.gl.glActiveTexture)(GL_TEXTURE0);
        }
        Ok(())
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

    fn upload_indexed_palette(&self, palette: &kitty_graphics::IndexedPalette) {
        if self.color_table_ubo == 0 {
            return;
        }
        // The shader block is std140, so every uint has a 16-byte stride.
        // Kitty's copy_color_table_to_buffer() writes the same padded layout.
        let mut table = [0u8; 272 * 16];
        let pack = |rgba: [u8; 4]| {
            ((u32::from(rgba[0]) << 16) | (u32::from(rgba[1]) << 8) | u32::from(rgba[2]))
                .to_ne_bytes()
        };
        let mut set = |slot: usize, value: [u8; 4]| {
            let offset = slot * 16;
            table[offset..offset + 4].copy_from_slice(&value);
        };
        for index in 0..248u16 {
            set(index as usize, pack(palette.color(index as u8)));
        }
        for index in 248..256u16 {
            set(
                264 + (index as usize - 248),
                pack(palette.color(index as u8)),
            );
        }
        // SAFETY: a context is current and the UBO was allocated above.
        unsafe {
            (self.gl.glBindBuffer)(GL_UNIFORM_BUFFER, self.color_table_ubo);
            (self.gl.glBufferSubData)(
                GL_UNIFORM_BUFFER,
                0,
                table.len() as isize,
                table.as_ptr() as *const c_void,
            );
            (self.gl.glBindBufferBase)(GL_UNIFORM_BUFFER, 1, self.color_table_ubo);
        }
    }

    fn ndc_rect(&self, rect: Rect) -> [f32; 4] {
        let vw = self.width as f32;
        let vh = self.height as f32;
        let left = rect.x / vw * 2.0 - 1.0;
        let right = rect.right() / vw * 2.0 - 1.0;
        let top = 1.0 - rect.y / vh * 2.0;
        let bottom = 1.0 - rect.bottom() / vh * 2.0;
        [left, top, right, bottom]
    }

    fn draw(
        &mut self,
        scissors: &[ScissorRect],
        frame: &Frame,
        stats: &mut RenderStats,
    ) -> Result<(), GlBackendError> {
        use crate::shaders::ProgramKind;

        let background = self.storage_color(frame.default_bg);

        // SAFETY: a context is current, the VAO holds the unit quad and every
        // program/texture id used below is live.
        unsafe {
            (self.gl.glBindVertexArray)(self.vao);
            (self.gl.glEnable)(GL_BLEND);
            (self.gl.glBlendFunc)(GL_ONE, GL_ONE_MINUS_SRC_ALPHA);
            (self.gl.glDisable)(GL_DEPTH_TEST);
            (self.gl.glDisable)(GL_CULL_FACE);
            (self.gl.glEnable)(GL_SCISSOR_TEST);
            (self.gl.glActiveTexture)(GL_TEXTURE0);

            for scissor in scissors {
                let y = self.height as i32 - (scissor.y + scissor.height);
                (self.gl.glScissor)(scissor.x, y, scissor.width, scissor.height);
                if !frame.skip_clear {
                    (self.gl.glClearColor)(
                        background[0],
                        background[1],
                        background[2],
                        background[3],
                    );
                    (self.gl.glClear)(GL_COLOR_BUFFER_BIT);
                }

                let calls = self.draw_calls.clone();
                for call in &calls {
                    if matches!(
                        call.kind,
                        BatchKind::CellBoth
                            | BatchKind::CellBothNonDefault
                            | BatchKind::CellBackgroundDefault
                            | BatchKind::CellBackgroundNonDefault
                            | BatchKind::CellForeground
                    ) {
                        self.draw_cell_layer(call.kind, *scissor)?;
                        stats.draw_calls += 1;
                        continue;
                    }
                    let first = call.first as usize;
                    let end = first + call.count.max(0) as usize;
                    for instance in &self.instances[first..end] {
                        let rect = Rect::new(
                            instance.rect[0],
                            instance.rect[1],
                            instance.rect[2],
                            instance.rect[3],
                        );
                        let color = instance.color;
                        match call.kind {
                            BatchKind::Solid => {
                                let radius = instance.params[0];
                                if radius > 0.0 {
                                    let program = self.programs.get(ProgramKind::RoundedRect);
                                    (self.gl.glUseProgram)(program.id);
                                    let glrect = [
                                        rect.x,
                                        self.height as f32 - rect.bottom(),
                                        rect.width,
                                        rect.height,
                                    ];
                                    (self.gl.glUniform4f)(
                                        program.uniform("rect"),
                                        glrect[0],
                                        glrect[1],
                                        glrect[2],
                                        glrect[3],
                                    );
                                    let thickness = rect.width.max(rect.height).max(1.0);
                                    (self.gl.glUniform2f)(
                                        program.uniform("params"),
                                        thickness,
                                        radius.min(rect.width.min(rect.height) * 0.5),
                                    );
                                    // rounded_rect expects a non-premultiplied
                                    // over colour; alpha is its coverage.
                                    (self.gl.glUniform4f)(
                                        program.uniform("color"),
                                        color[0],
                                        color[1],
                                        color[2],
                                        color[3],
                                    );
                                    (self.gl.glUniform4f)(
                                        program.uniform("background_color"),
                                        0.0,
                                        0.0,
                                        0.0,
                                        0.0,
                                    );
                                    (self.gl.glUniform4f)(
                                        program.uniform("outer_color"),
                                        0.0,
                                        0.0,
                                        0.0,
                                        0.0,
                                    );
                                } else {
                                    let program = self.programs.get(ProgramKind::Tint);
                                    (self.gl.glUseProgram)(program.id);
                                    let ndc = self.ndc_rect(rect);
                                    (self.gl.glUniform4f)(
                                        program.uniform("edges"),
                                        ndc[0],
                                        ndc[1],
                                        ndc[2],
                                        ndc[3],
                                    );
                                    (self.gl.glUniform4f)(
                                        program.uniform("tint_color"),
                                        color[0] * color[3],
                                        color[1] * color[3],
                                        color[2] * color[3],
                                        color[3],
                                    );
                                }
                                (self.gl.glDrawArrays)(GL_TRIANGLE_FAN, 0, 4);
                            }
                            BatchKind::Glyph { page, is_color } => {
                                let kind = if is_color {
                                    ProgramKind::Graphics
                                } else {
                                    ProgramKind::GraphicsAlphaMask
                                };
                                let program = self.programs.get(kind);
                                (self.gl.glUseProgram)(program.id);
                                let ndc = self.ndc_rect(rect);
                                let src = [
                                    instance.uv[0],
                                    instance.uv[1],
                                    instance.uv[2],
                                    instance.uv[3],
                                ];
                                (self.gl.glUniform4f)(
                                    program.uniform("src_rect"),
                                    src[0],
                                    src[1],
                                    src[2],
                                    src[3],
                                );
                                (self.gl.glUniform4f)(
                                    program.uniform("dest_rect"),
                                    ndc[0],
                                    ndc[1],
                                    ndc[2],
                                    ndc[3],
                                );
                                (self.gl.glUniform1i)(program.uniform("image"), 0);
                                (self.gl.glUniform1f)(program.uniform("extra_alpha"), 1.0);
                                if !is_color {
                                    // Fold the foreground alpha into the mask
                                    // colour; the generated alpha-mask pass has
                                    // no separate alpha uniform.
                                    (self.gl.glUniform3f)(
                                        program.uniform("amask_fg"),
                                        color[0] * color[3],
                                        color[1] * color[3],
                                        color[2] * color[3],
                                    );
                                    (self.gl.glUniform4f)(
                                        program.uniform("amask_bg_premult"),
                                        0.0,
                                        0.0,
                                        0.0,
                                        0.0,
                                    );
                                }
                                let texture = self
                                    .atlas_pages
                                    .get(&page)
                                    .map(|texture| texture.texture)
                                    .unwrap_or(0);
                                (self.gl.glBindTexture)(GL_TEXTURE_2D, texture);
                                (self.gl.glDrawArrays)(GL_TRIANGLE_FAN, 0, 4);
                            }
                            BatchKind::Image(key) => {
                                let tint = instance.params[0].clamp(0.0, 1.0);
                                let opacity = instance.params[1].clamp(0.0, 1.0);
                                let cached = self.image_textures.get(&key).copied();
                                let texture = cached.map(|cached| cached.texture).unwrap_or(0);
                                let ndc = self.ndc_rect(rect);
                                let src = [
                                    instance.uv[0],
                                    instance.uv[1],
                                    instance.uv[2],
                                    instance.uv[3],
                                ];
                                if let Some(format) =
                                    cached.and_then(|cached| cached.indexed_format)
                                {
                                    // The real indexed graphics program performs
                                    // the palette lookup (including bilinear
                                    // filtering) after reading the integer texture.
                                    let unscaled = cached.is_some_and(|cached| {
                                        (cached.width as f32 - rect.width).abs() < 0.5
                                            && (cached.height as f32 - rect.height).abs() < 0.5
                                    });
                                    let kind = if unscaled {
                                        ProgramKind::GraphicsIndexedUnscaled
                                    } else {
                                        ProgramKind::GraphicsIndexed
                                    };
                                    let program = self.programs.get(kind);
                                    (self.gl.glUseProgram)(program.id);
                                    (self.gl.glUniform4f)(
                                        program.uniform("src_rect"),
                                        src[0],
                                        src[1],
                                        src[2],
                                        src[3],
                                    );
                                    (self.gl.glUniform4f)(
                                        program.uniform("dest_rect"),
                                        ndc[0],
                                        ndc[1],
                                        ndc[2],
                                        ndc[3],
                                    );
                                    (self.gl.glUniform1i)(program.uniform("image"), 0);
                                    (self.gl.glUniform1ui)(
                                        program.uniform("has_alpha"),
                                        u32::from(
                                            format == kitty_graphics::ImageFormat::IndexedAlpha,
                                        ),
                                    );
                                    (self.gl.glUniform1f)(
                                        program.uniform("extra_alpha"),
                                        opacity * (1.0 - tint),
                                    );
                                    (self.gl.glBindTexture)(GL_TEXTURE_2D, texture);
                                    (self.gl.glDrawArrays)(GL_TRIANGLE_FAN, 0, 4);
                                } else {
                                    let graphics = self.programs.get(ProgramKind::Graphics);
                                    (self.gl.glUseProgram)(graphics.id);
                                    (self.gl.glUniform4f)(
                                        graphics.uniform("src_rect"),
                                        src[0],
                                        src[1],
                                        src[2],
                                        src[3],
                                    );
                                    (self.gl.glUniform4f)(
                                        graphics.uniform("dest_rect"),
                                        ndc[0],
                                        ndc[1],
                                        ndc[2],
                                        ndc[3],
                                    );
                                    (self.gl.glUniform1i)(graphics.uniform("image"), 0);
                                    (self.gl.glUniform1f)(
                                        graphics.uniform("extra_alpha"),
                                        opacity * (1.0 - tint),
                                    );
                                    (self.gl.glBindTexture)(GL_TEXTURE_2D, texture);
                                    (self.gl.glDrawArrays)(GL_TRIANGLE_FAN, 0, 4);
                                }
                                if tint > 0.0 {
                                    let program = self.programs.get(ProgramKind::Tint);
                                    (self.gl.glUseProgram)(program.id);
                                    (self.gl.glUniform4f)(
                                        program.uniform("edges"),
                                        ndc[0],
                                        ndc[1],
                                        ndc[2],
                                        ndc[3],
                                    );
                                    let alpha = opacity * tint;
                                    (self.gl.glUniform4f)(
                                        program.uniform("tint_color"),
                                        color[0] * alpha,
                                        color[1] * alpha,
                                        color[2] * alpha,
                                        alpha,
                                    );
                                    (self.gl.glDrawArrays)(GL_TRIANGLE_FAN, 0, 4);
                                }
                            }
                            BatchKind::BackgroundImage(key) => {
                                let cached = self.image_textures.get(&key).copied();
                                let texture = cached.map(|cached| cached.texture).unwrap_or(0);
                                let tiled = instance.params[0];
                                let image_width = instance.params[1];
                                let image_height = instance.params[2];
                                let tint = instance.params[3].clamp(0.0, 1.0);
                                let ndc = self.ndc_rect(rect);
                                let program = self.programs.get(ProgramKind::BgImage);
                                (self.gl.glUseProgram)(program.id);
                                (self.gl.glUniform4f)(
                                    program.uniform("sizes"),
                                    self.width as f32,
                                    self.height as f32,
                                    image_width,
                                    image_height,
                                );
                                (self.gl.glUniform1f)(program.uniform("tiled"), tiled);
                                (self.gl.glUniform4f)(
                                    program.uniform("positions"),
                                    ndc[0],
                                    ndc[1],
                                    ndc[2],
                                    ndc[3],
                                );
                                (self.gl.glUniform1i)(program.uniform("image"), 0);
                                let background = self.storage_color(frame.default_bg);
                                (self.gl.glUniform4f)(
                                    program.uniform("background"),
                                    background[0],
                                    background[1],
                                    background[2],
                                    1.0,
                                );
                                (self.gl.glBindTexture)(GL_TEXTURE_2D, texture);
                                (self.gl.glDrawArrays)(GL_TRIANGLE_FAN, 0, 4);
                                stats.bgimage_draws += 1;
                                if tint > 0.0 {
                                    let tint_program = self.programs.get(ProgramKind::Tint);
                                    (self.gl.glUseProgram)(tint_program.id);
                                    (self.gl.glUniform4f)(
                                        tint_program.uniform("edges"),
                                        ndc[0],
                                        ndc[1],
                                        ndc[2],
                                        ndc[3],
                                    );
                                    let color = self.storage_color(
                                        frame
                                            .background_image
                                            .as_ref()
                                            .map_or(Rgba8::BLACK, |image| image.color),
                                    );
                                    (self.gl.glUniform4f)(
                                        tint_program.uniform("tint_color"),
                                        color[0] * tint,
                                        color[1] * tint,
                                        color[2] * tint,
                                        tint,
                                    );
                                    (self.gl.glDrawArrays)(GL_TRIANGLE_FAN, 0, 4);
                                }
                            }
                            BatchKind::Trail => {
                                let program = self.programs.get(ProgramKind::Trail);
                                (self.gl.glUseProgram)(program.id);
                                let ndc = self.ndc_rect(rect);
                                (self.gl.glUniform4fv)(
                                    program.uniform("x_coords"),
                                    1,
                                    [ndc[0], ndc[0], ndc[2], ndc[2]].as_ptr(),
                                );
                                (self.gl.glUniform4fv)(
                                    program.uniform("y_coords"),
                                    1,
                                    [ndc[3], ndc[1], ndc[1], ndc[3]].as_ptr(),
                                );
                                (self.gl.glUniform2f)(program.uniform("cursor_edge_x"), 2.0, 2.0);
                                (self.gl.glUniform2f)(program.uniform("cursor_edge_y"), 2.0, 2.0);
                                (self.gl.glUniform3f)(
                                    program.uniform("trail_color"),
                                    color[0],
                                    color[1],
                                    color[2],
                                );
                                let alpha = color[3].clamp(0.0, 1.0);
                                (self.gl.glUniform1f)(program.uniform("trail_opacity"), alpha);
                                (self.gl.glDrawArrays)(GL_TRIANGLE_FAN, 0, 4);
                            }
                            BatchKind::CellBoth
                            | BatchKind::CellBothNonDefault
                            | BatchKind::CellBackgroundDefault
                            | BatchKind::CellBackgroundNonDefault
                            | BatchKind::CellForeground => {}
                        }
                        stats.draw_calls += 1;
                    }
                }
            }

            (self.gl.glDisable)(GL_SCISSOR_TEST);
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
        // Partial frames belong to one viewport (a GUI pane or tab strip).
        // Restrict the clear/draw rectangle to that viewport instead of the
        // whole window, otherwise drawing a second pane would erase the first.
        let origin_x = frame.geometry.origin_x.floor().max(0.0) as i32;
        let viewport_width = if config.viewport_width > 0.0 {
            config.viewport_width
        } else {
            frame.geometry.width_px()
        };
        let x0 = origin_x.min(width);
        let x1 = (origin_x + viewport_width.ceil() as i32).max(x0).min(width);
        if x1 <= x0 {
            return Vec::new();
        }
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
                    x: x0,
                    y: y0,
                    width: x1 - x0,
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

fn flip_rows(pixels: &mut [u8], width: u32, height: u32) {
    let stride = width as usize * 4;
    if stride == 0 {
        return;
    }
    let mut scratch = vec![0u8; stride];
    for row in 0..height as usize / 2 {
        let top = row * stride;
        let bottom = (height as usize - 1 - row) * stride;
        scratch.copy_from_slice(&pixels[top..top + stride]);
        pixels.copy_within(bottom..bottom + stride, top);
        pixels[bottom..bottom + stride].copy_from_slice(&scratch);
    }
}

impl Drop for GlBackend {
    fn drop(&mut self) {
        if let Some(context) = &self.context {
            if context.make_current().is_err() {
                // No current context: make sure Programs::drop does not call GL.
                self.programs.programs.clear();
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
            for (_, cached) in self.image_textures.drain() {
                (self.gl.glDeleteTextures)(1, &cached.texture);
            }
            if self.custom_program != 0 {
                (self.gl.glDeleteProgram)(self.custom_program);
                self.custom_program = 0;
            }
            self.destroy_custom_pipeline();
            if self.color_table_ubo != 0 {
                (self.gl.glDeleteBuffers)(1, &self.color_table_ubo);
                self.color_table_ubo = 0;
            }
            if self.cell_data_ubo != 0 {
                (self.gl.glDeleteBuffers)(1, &self.cell_data_ubo);
                self.cell_data_ubo = 0;
            }
            for buffer in [
                self.cell_colors_vbo,
                self.cell_sprite_vbo,
                self.cell_selection_vbo,
            ] {
                if buffer != 0 {
                    (self.gl.glDeleteBuffers)(1, &buffer);
                }
            }
            self.cell_colors_vbo = 0;
            self.cell_sprite_vbo = 0;
            self.cell_selection_vbo = 0;
            if self.cell_vao != 0 {
                (self.gl.glDeleteVertexArrays)(1, &self.cell_vao);
                self.cell_vao = 0;
            }
            if let Some(sprites) = self.cell_sprites.take() {
                let textures = [sprites.texture, sprites.decorations];
                (self.gl.glDeleteTextures)(2, textures.as_ptr());
            }
            if let Some(target) = self.cell_offscreen.take() {
                (self.gl.glDeleteFramebuffers)(1, &target.framebuffer);
                (self.gl.glDeleteTextures)(1, &target.texture);
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
            .field("image_textures", &self.image_texture_count())
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
