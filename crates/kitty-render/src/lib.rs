// SPDX-License-Identifier: GPL-3.0-or-later
//! CPU-side renderer core ported from kitty's rendering code.
//!
//! The default build is deliberately backend-free. It owns the pieces that can
//! be built and tested without an OpenGL/EGL context:
//!
//! * [`damage`] — dirty-line coalescing, scroll/cursor/full-redraw detection;
//! * [`colors`] — SGR colour decoding, palette resolution, premultiplication
//!   and the sRGB/gamma LUT from `shaders/linear2srgb.slang`;
//! * [`atlas`] — deterministic Gray8/RGBA8 glyph page packing with a
//!   [`GlyphRasterizer`] bridge to `kitty-fonts`;
//! * [`instances`] — background/glyph/decoration/cursor instance generation
//!   from [`kitty_screen::Screen`] cells;
//! * [`decorations`] — underline/strikethrough masks and geometry, cursor
//!   geometry, window/tab-bar borders, padding rects and rounded-corner
//!   tessellation, ported from `kitty/decorations.c`, `kitty/borders.py` and
//!   `kitty/tab_bar.py`;
//! * [`overlays`] — selection ranges, hyperlink hover state, tab-bar
//!   descriptions and overlay counters;
//! * [`shaped`] — shaped-run requests, cluster sprites and stable run hashes;
//! * [`font_rasterizer`] — optional `kitty-fonts` FreeType/HarfBuzz adapter;
//! * [`RendererCore`] — the persistent object that owns an atlas and damage
//!   tracker and produces [`Frame`]s.
//!
//! # API overview
//!
//! ```no_run
//! use kitty_render::{BasicColorResolver, RendererCore};
//! use kitty_screen::Screen;
//!
//! let screen = Screen::builder(80, 24).scrollback(1000).build().unwrap();
//! let mut core = RendererCore::default();
//! let mut rasterizer = |key: &kitty_render::GlyphKey| {
//!     // A real adapter asks kitty-fonts for the glyph bitmap here.
//!     kitty_render::GlyphBitmap::gray(8, 12, vec![0; 96]).ok()
//! };
//! let resolver = BasicColorResolver::default();
//! let frame = core.prepare_frame(&screen, &mut rasterizer, &resolver);
//! for glyph in &frame.glyphs {
//!     if let Some(sprite) = glyph.sprite {
//!         // sprite.page, sprite.uv, ...
//!     }
//! }
//! ```
//!
//! # Supported
//!
//! * Dirty line ranges, full redraws on the first frame/resize/alt-screen/
//!   scroll, cursor-only damage and explicit invalidation.
//! * SGR colours (default, indexed, RGB and context-dependent special
//!   colours), reverse video, dim/blink/style flags.
//! * Wide cells and multicell continuations, skipped blank cells.
//! * Block, beam, underline and hollow cursor geometry.
//! * Straight, double, curly, dotted and dashed underlines plus
//!   strikethrough, with a separate decoration colour.
//! * Multiple cell/word/line/block selection ranges with selection foreground
//!   and background tinting, including scrollback range rows and the current
//!   `Screen::scrolled_by` view offset.
//! * OSC 8 hyperlink spans with a dotted underline, an optional whole-link
//!   hover highlight and URL colour from [`ColorResolver`].
//! * A top tab bar with per-tab inactive/active backgrounds, borders and title
//!   glyphs rendered through the normal atlas path. [`TabBarSpec`] carries
//!   titles, active index, colours and height; overflow is clipped, not
//!   scrolled.
//! * Shaped-run sprites: contiguous same-style cells are shaped through
//!   [`GlyphRasterizer::shape_run`]; ligature and combining clusters become one
//!   atlas sprite keyed by a stable run hash. The generator falls back to
//!   per-cell glyphs when shaping is unavailable or fails.
//! * Window borders with compensatory padding and rounded-corner geometry,
//!   tab-bar blank/margin rectangles, padding rectangles.
//! * Gray8 and RGBA8 atlas pages with deterministic shelf packing, UVs,
//!   page-creation/eviction events and duplicate detection.
//! * Allocation reuse across frames via [`RendererCore::recycle_frame`] or
//!   [`RendererCore::prepare_frame_into`].
//! * Graphics-protocol placements stored by `kitty-screen` as textured quads,
//!   with source rectangles, cell/pixel offsets, automatic aspect-ratio sizing,
//!   z-index/layer ordering and clipping to the viewport.
//! * Animation frames from [`kitty_graphics::AnimationManager`], timed frame
//!   swaps, composition canvases and Unicode `U+10EEEE` placeholder runs with
//!   row/column diacritics and image-id foreground colours.
//! * PNG background images with all six layout modes, tint and opacity, and
//!   window logos with nine anchors, alpha and percentage scaling.
//! * A simple fading cursor trail driven by recent cursor positions (not
//!   kitty's full corner animation).
//!
//! # OpenGL backend (`gl` feature)
//!
//! The optional, default-off `gl` feature adds a headless EGL/OpenGL
//! implementation of the backend contract:
//!
//! * [`egl`] — raw `libEGL`/GL FFI, a surfaceless/pbuffer
//!   [`GlContext`](egl::GlContext), an `eglGetProcAddress` function table,
//!   version/extension queries, resize, swap and readback;
//! * [`shaders`] — kitty's real Slang programs compiled to GLSL by
//!   `scripts/generate_shaders.py`, with program variants, preprocessor
//!   defines, uniform/attribute reflection and a user-shader compiler;
//! * [`gl_shaders`] — compatibility helpers returning the generated programs
//!   used for glyphs, solids and images;
//! * [`gl_backend`] — instanced drawing, per-page atlas textures, an image
//!   texture cache keyed by [`ImageKey`], scissor-based damage, premultiplied
//!   blending and `read_pixels()`. `GlBackend::new` creates its own headless
//!   pbuffer context; `GlBackend::with_loader` reuses a caller-provided current
//!   EGL window context.
//!
//! The GLSL is checked in under `src/shaders/generated` so a normal build does
//! not need `slangc`; `SHADERS.md` records how each program in kitty's
//! catalogue is driven.
//!
//! ```text
//! cargo test -p kitty-render                    # CPU only, no EGL linked
//! cargo test -p kitty-render --features gl      # adds the headless GPU tests
//! cargo run  -p kitty-render --features gl --example bench_gl
//! ```
//!
//! Headless GPU tests create a surfaceless EGL context and skip at runtime
//! with a message when EGL or the required GL version is unavailable.
//!
//! # Renderer features not supported yet
//!
//! * Relative placement chains are resolved with cycle/depth checks and
//!   descendant visibility rules. Indexed protocol images are uploaded as
//!   integer index textures and looked up in the live palette in the shader,
//!   matching kitty. Animation pixels can be spilled to a private disk cache
//!   that is removed when the renderer is dropped. Animation/composition state lives in `kitty-graphics`; the renderer
//!   consumes its reconstructed current frame and exposes
//!   [`RendererCore::tick_animations`].
//! * The full cursor-trail corner animation and custom trail shaders. The
//!   implemented trail is a simple bounded history of recent cursor cells that
//!   fades linearly and is replaced rather than smoothly eased.
//! * Image transforms (rotation, mirroring, scaling filters beyond
//!   nearest/linear) and per-window background-image lists.
//! * Image and trail changes are not part of `kitty-screen`'s dirty-line
//!   tracking, so [`RendererCore`] forces a full redraw while those layers are
//!   present. Background images survive a GPU resize, but the frame must be
//!   regenerated with the new [`RenderConfig::viewport_width`] /
//!   [`RenderConfig::viewport_height`] so the quad covers the new drawable.
//! * Selection marks (mark/scrollbar ranges), URL hover *menus* and custom
//!   selection shaders. Selections only tint cell backgrounds/foregrounds;
//!   they do not alter text extraction.
//! * Tab-bar scrolling, vertical tab bars, tab-bar margins/edge colours beyond
//!   the simple [`TabBarSpec`] geometry, and tab icons/close buttons.
//! * Custom user shaders and animated multi-group pipelines are compiled,
//!   linked and run as full-screen post-processing passes. The pipeline model
//!   and animation state machine live in [`custom_shader`]; the GL backend
//!   provides `GlBackend::install_custom_pipeline` and
//!   `GlBackend::render_custom_pipeline`. Pointer remapping and the full
//!   `KittyCustomShaderData` payload are not populated yet.
//! * Shaped runs are limited to at most [`SHAPED_RUN_MAX_CELLS`] contiguous
//!   single-width cells with the same style, foreground, background and
//!   hyperlink, and to 24 codepoints per key. Selected/hovered runs, wide
//!   cells and runs crossing row boundaries fall back to per-cell glyphs.
//!   Shaping forces LTR and does not implement bidirectional reordering or
//!   complex-script cell segmentation.
//! * Colour glyph formats (COLR/CPAL layers, CBDT/CBLC and SVG-in-OpenType) in
//!   the shaped-run compositor. [`FontRasterizer`] shapes and composites Gray8
//!   clusters only and returns `None` for colour glyphs so the caller falls
//!   back.
//! * Multicell text-sizing (`TextSizing`/OSC 66) and ligature expansion beyond
//!   the cells already present in the screen's text cache.
//! * Blink animation (the flag is carried but not animated) and font-fallback
//!   selection plus rendered-glyph disk caching.
//!
//! The CPU-only build uses only `std` and contains no `unsafe`; the `gl`
//! feature is the only place with FFI.

#![cfg_attr(not(feature = "gl"), forbid(unsafe_code))]
#![deny(unsafe_op_in_unsafe_fn)]

pub mod atlas;
pub mod cell;
pub mod colors;
pub mod core;
pub mod custom_shader;
pub mod damage;
pub mod decorations;
pub mod font_rasterizer;
pub mod geom;
pub mod images;
pub mod instances;
pub mod overlays;
pub mod shaders;
pub mod shaped;
pub mod trail;

#[cfg(feature = "gl")]
pub mod egl;
#[cfg(feature = "gl")]
pub mod gl_backend;
#[cfg(feature = "gl")]
pub mod gl_shaders;

pub use atlas::{
    AtlasConfig, AtlasError, AtlasEvent, AtlasFormat, AtlasInsert, AtlasPage, AtlasStats,
    GlyphAtlas, GlyphBitmap, GlyphKey, GlyphRasterizer, GlyphSlot, GlyphText, UvRect,
    MAX_GLYPH_CODEPOINTS,
};
pub use cell::{build_cell_sprites, CellGrid, CellRecord, CellSpriteData, SpriteKey};
pub use colors::{
    apply_text_contrast, build_gamma_lut, contrast_ratio, decode_color, dimmed_color,
    encode_indexed, encode_rgb, encode_special, gamma_lut, gamma_lut_lookup, indexed_image_palette,
    kitty_default_palette, linear_to_srgb, luminance, rgb_u32_to_linear, srgb_to_linear,
    BasicColorResolver, CellColor, ColorResolver, Rgba8, KITTY_DEFAULT_ANSI_COLORS,
};
pub use core::RendererCore;
pub use custom_shader::{
    CustomShaderAnimationState, CustomShaderError, CustomShaderGroup, CustomShaderPipeline,
    NamedTexture, ShaderEventMask, ShaderGroupAnimState, SHADER_ANIM_EVENT_BELL_IN_WINDOW,
    SHADER_ANIM_EVENT_CURSOR_TRAIL_MOVE, SHADER_ANIM_EVENT_CURSOR_TRAIL_STOP,
    SHADER_ANIM_EVENT_OS_WINDOW_FOCUS_IN, SHADER_ANIM_EVENT_OS_WINDOW_FOCUS_OUT,
    SHADER_ANIM_EVENT_POINTER_LEFT_BUTTON_PRESS, SHADER_ANIM_EVENT_TAB_CHANGE,
    SHADER_ANIM_EVENT_USER_ACTIVITY, SHADER_ANIM_EVENT_USER_IDLE,
    SHADER_ANIM_EVENT_WINDOW_FOCUS_IN, SHADER_ANIM_EVENT_WINDOW_FOCUS_OUT,
    SHADER_ANIM_FIRST_USER_EVENT,
};
pub use damage::{coalesce_lines, CursorState, Damage, DamageTracker, FullRedrawReason, LineRange};
pub use decorations::{
    add_borders, background_rect, beam_cursor_rect, curly_underline_mask, dashed_underline_mask,
    dotted_underline_mask, double_underline_geometry, double_underline_mask, hollow_cursor_rects,
    horizontal_edge, missing_glyph_mask, padding_rects, padding_rects_between, pane_border_rects,
    rounded_rect_points, straight_underline_geometry, straight_underline_mask,
    strikethrough_geometry, strikethrough_mask, strikethrough_rects, tab_bar_blank_rects,
    tessellate_rounded_rect_border, tessellate_rounded_rect_fill, underline_cursor_rect,
    underline_mask, underline_rects, vertical_edge, BorderColor, BorderRect, DecorationGeometry,
    DecorationMask, Edge, PaneBorderLine, PaneBorderPane, TabBarBlankRectConfig, Triangle,
    UnderlineStyle, Vertex, WindowGeometry,
};
pub use font_rasterizer::FontRasterizer;
pub use geom::{CellMetrics, EdgeWidths, GridGeometry, Rect, Region};
pub use images::{
    background_image, background_quad, clip_rect_uv, collect_protocol_images,
    collect_protocol_images_with_animations, collect_protocol_images_with_palette,
    color_to_image_id, diacritic_to_num, expand_unicode_placeholders, fingerprint, window_logo,
    window_logo_quad, BackgroundImageConfig, BackgroundLayout, ImageAsset, ImageData, ImageFilter,
    ImageInstance, ImageKey, ImageLayer, ImageLoadError, ImageUpload, ImageWrap, LogoPosition,
    PlaceholderRun, WindowLogoConfig,
};
pub use instances::{
    BackgroundInstance, CursorGeometry, CursorInstance, DecorationInstance, DecorationKind, Frame,
    GlyphInstance, InstanceFlags, InstanceGenerator, RenderConfig, SolidRect, GLYPH_FLAG_BOLD,
    GLYPH_FLAG_COLOR, GLYPH_FLAG_ITALIC,
};
pub use overlays::{
    interval_contains, selection_intervals, HyperlinkHover, OverlayStats, SelectionCell,
    SelectionRange, TabBarColors, TabBarFrame, TabBarSpec, TabBarTab,
};
pub use shaders::{
    cell_variant, default_variant, named_variant, variants, CustomShader, GeneratedMetadata,
    GeneratedProgram, ProgramKind, ProgramSources, ShaderDialect, ShaderError, SlangCompiler,
};
pub use shaped::{
    shaped_run_hash, ShapedRunCluster, ShapedRunOutput, ShapedRunRequest, SHAPED_RUN_MAX_CELLS,
};
pub use trail::{CursorTrail, CursorTrailConfig, TrailInstance};

#[cfg(feature = "gl")]
pub use egl::{ContextConfig, EglError, GlApi, GlContext, GlError, GlFns, GlVersion};
#[cfg(feature = "gl")]
pub use gl_backend::{
    validate_all_shader_variants, validate_builtin_programs, AtlasSyncStats, FilterMode, GammaMode,
    GlBackend, GlBackendConfig, GlBackendError, ImageSyncStats, RenderStats, Screenshot,
};
