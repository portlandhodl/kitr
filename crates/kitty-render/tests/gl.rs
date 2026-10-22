// SPDX-License-Identifier: GPL-3.0-or-later
//! Headless GPU tests for the optional EGL/OpenGL backend.
//!
//! Every test takes a process-wide mutex because a Mesa EGL display is
//! process-global. If a surfaceless context cannot be created, or the reported
//! GL version is too old, the test prints a skip message and returns instead
//! of failing.

#![cfg(feature = "gl")]

use std::sync::{Mutex, MutexGuard};

use kitty_render::gl_backend::{FilterMode, GammaMode, GlBackend, GlBackendConfig, GlBackendError};
use kitty_render::{
    AtlasConfig, BackgroundInstance, BasicColorResolver, CellMetrics, ContextConfig,
    CursorInstance, Damage, Frame, FullRedrawReason, GlyphAtlas, GlyphBitmap, GlyphInstance,
    GlyphKey, GridGeometry, InstanceFlags, LineRange, Rect, RenderConfig, RendererCore, Rgba8,
};
use kitty_screen::{CursorShape, Screen};

static GPU_LOCK: Mutex<()> = Mutex::new(());

fn lock() -> MutexGuard<'static, ()> {
    GPU_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn backend(width: u32, height: u32) -> Option<GlBackend> {
    let config = GlBackendConfig {
        context: ContextConfig::new(width, height),
        gamma: GammaMode::None,
        atlas_filter: FilterMode::Nearest,
        cursor_radius: 0.0,
    };
    match GlBackend::new(config) {
        Ok(backend) => Some(backend),
        Err(error) => {
            eprintln!("skipping GPU test: headless EGL unavailable: {error}");
            None
        }
    }
}

macro_rules! try_backend {
    ($width:expr, $height:expr) => {
        match backend($width, $height) {
            Some(backend) => backend,
            None => return,
        }
    };
}

fn pixel(pixels: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
    let index = ((y * width + x) * 4) as usize;
    [
        pixels[index],
        pixels[index + 1],
        pixels[index + 2],
        pixels[index + 3],
    ]
}

fn assert_pixel(actual: [u8; 4], expected: [u8; 4], tolerance: i32, what: &str) {
    for channel in 0..4 {
        let difference = actual[channel] as i32 - expected[channel] as i32;
        assert!(
            difference.abs() <= tolerance,
            "{what}: channel {channel} was {} but expected {} (actual {actual:?}, expected {expected:?})",
            actual[channel],
            expected[channel],
        );
    }
}

fn empty_frame(columns: u32, rows: u32, cell_width: f32, cell_height: f32, bg: Rgba8) -> Frame {
    Frame {
        damage: Damage {
            full_redraw: true,
            full_redraw_reason: Some(FullRedrawReason::Explicit),
            ..Damage::default()
        },
        geometry: GridGeometry::new(
            columns,
            rows,
            CellMetrics {
                cell_width,
                cell_height,
                ..CellMetrics::default()
            },
        ),
        default_bg: bg,
        default_fg: Rgba8::WHITE,
        ..Frame::default()
    }
}

#[test]
fn context_creation_teardown_and_queries() {
    let _guard = lock();
    let context = match kitty_render::GlContext::new(ContextConfig::new(16, 16)) {
        Ok(context) => context,
        Err(error) => {
            eprintln!("skipping GPU test: headless EGL unavailable: {error}");
            return;
        }
    };
    assert_eq!(context.size(), (16, 16));
    assert!(context.egl_version().0 >= 1);
    assert!(!context.egl_vendor().is_empty());
    assert!(context.egl_client_apis().contains("OpenGL"));

    // SAFETY: the newly created context is current.
    let gl = unsafe { kitty_render::GlFns::load().expect("load GL entry points") };
    let version = gl.version().expect("parse GL_VERSION");
    assert!(
        version.major > 3 || (version.major == 3 && version.minor >= 3),
        "unexpected GL version {version}"
    );
    assert!(!gl.renderer().is_empty());
    assert!(!gl.shading_language_version().is_empty());
    assert!(!gl.extensions().is_empty());
    assert_eq!(gl.check("queries"), Ok(()));
    let _ = gl;
    drop(context);

    // A second context proves the first one released the EGL display.
    let mut context =
        kitty_render::GlContext::new(ContextConfig::new(8, 8)).expect("recreate context");
    context.resize(24, 12).expect("resize");
    assert_eq!(context.size(), (24, 12));
}

#[test]
fn gles_context_and_shaders() {
    let _guard = lock();
    let config = GlBackendConfig {
        context: ContextConfig::new(32, 32).gles(),
        ..GlBackendConfig::default()
    };
    let mut backend = match GlBackend::new(config) {
        Ok(backend) => backend,
        Err(GlBackendError::Egl(error)) => {
            eprintln!("skipping GLES test: headless EGL/GLES unavailable: {error}");
            return;
        }
        Err(GlBackendError::UnsupportedVersion { .. }) => {
            eprintln!("skipping GLES test: driver lacks OpenGL ES 3.0");
            return;
        }
        Err(error) => panic!("GLES backend failed after context creation: {error}"),
    };
    assert!(backend.gl_version_string().starts_with("OpenGL ES"));
    backend.clear(Rgba8::rgb(1, 2, 3)).expect("clear");
    let pixels = backend.read_pixels().expect("readback");
    assert_eq!(pixel(&pixels, 32, 16, 16), [1, 2, 3, 255]);
}

#[test]
fn shaders_compile_and_link() {
    let _guard = lock();
    let backend = try_backend!(32, 32);
    assert!(backend.gl_version().is_some());
    assert!(
        backend.shading_language_version().starts_with("3.30")
            || backend.shading_language_version().starts_with("4.")
            || backend.shading_language_version().starts_with("3.2")
    );
    // Construction is only successful when both embedded programs compiled
    // and linked; dropping exercises program/resource teardown.
    drop(backend);
}

#[test]
fn clear_color_readback() {
    let _guard = lock();
    let mut backend = try_backend!(32, 32);
    backend.clear(Rgba8::rgb(12, 34, 56)).expect("clear");
    let pixels = backend.read_pixels().expect("readback");
    assert_eq!(pixels.len(), 32 * 32 * 4);
    assert_eq!(pixel(&pixels, 32, 16, 16), [12, 34, 56, 255]);
    assert_eq!(pixel(&pixels, 32, 0, 0), [12, 34, 56, 255]);
    assert_eq!(pixel(&pixels, 32, 31, 31), [12, 34, 56, 255]);
}

#[test]
fn draw_background_quad() {
    let _guard = lock();
    let mut backend = try_backend!(64, 32);
    let mut frame = empty_frame(2, 1, 32.0, 32.0, Rgba8::BLACK);
    frame.backgrounds.push(BackgroundInstance {
        rect: Rect::new(0.0, 0.0, 64.0, 32.0),
        cell_x: 0,
        cell_y: 0,
        color: Rgba8::rgb(200, 40, 10),
        is_default_bg: false,
        flags: InstanceFlags::default(),
    });

    let stats = backend
        .render(&frame, &RenderConfig::default())
        .expect("render");
    assert_eq!(stats.instances, 1);
    assert_eq!(stats.scissor_rects, 1);
    let pixels = backend.read_pixels().expect("readback");
    assert_pixel(
        pixel(&pixels, 64, 32, 16),
        [200, 40, 10, 255],
        1,
        "background centre",
    );
    assert_pixel(
        pixel(&pixels, 64, 1, 1),
        [200, 40, 10, 255],
        1,
        "background corner",
    );
}

#[test]
fn atlas_glyph_alpha_blending() {
    let _guard = lock();
    let mut backend = try_backend!(32, 32);
    // Avoid eviction so the slot/page mapping stays stable.
    let atlas_config = AtlasConfig {
        page_width: 16,
        page_height: 16,
        padding: 1,
        max_gray_pages: 1,
        max_rgba_pages: 1,
        allow_eviction: false,
    };
    let mut atlas = GlyphAtlas::new(atlas_config);
    let key = GlyphKey::new(0, 1, 0);
    let inserted = atlas
        .insert(key, GlyphBitmap::gray(2, 2, vec![255; 4]).expect("bitmap"))
        .expect("insert");
    assert_eq!(inserted.slot.page, 0);

    let mut frame = empty_frame(2, 2, 16.0, 16.0, Rgba8::BLACK);
    frame.glyphs.push(GlyphInstance {
        rect: Rect::new(0.0, 0.0, 16.0, 16.0),
        cell_x: 0,
        cell_y: 0,
        fg: Rgba8::rgba(255, 0, 0, 128),
        sprite: Some(inserted.slot),
        text: key.text,
        flags: InstanceFlags::default(),
    });

    let sync = backend.sync_atlas(&atlas, &frame).expect("atlas upload");
    assert_eq!(sync.pages_created, 1);
    assert_eq!(sync.pages_uploaded, 1);
    assert_eq!(backend.atlas_texture_count(), 1);
    let stats = backend
        .render(&frame, &RenderConfig::default())
        .expect("render");
    assert_eq!(stats.instances, 1);
    assert_eq!(stats.glyphs_skipped, 0);

    let pixels = backend.read_pixels().expect("readback");
    // 50% red over opaque black: premultiplied (0.5, 0, 0, 0.5) blended with
    // GL_ONE / GL_ONE_MINUS_SRC_ALPHA gives (128, 0, 0) and an opaque alpha.
    assert_pixel(
        pixel(&pixels, 32, 8, 8),
        [128, 0, 0, 255],
        1,
        "glyph centre",
    );
    assert_pixel(
        pixel(&pixels, 32, 1, 1),
        [128, 0, 0, 255],
        1,
        "glyph corner",
    );
    assert_eq!(pixel(&pixels, 32, 24, 24), [0, 0, 0, 255]);
}

#[test]
fn rgba_atlas_glyph() {
    let _guard = lock();
    let mut backend = try_backend!(32, 32);
    let mut atlas = GlyphAtlas::new(AtlasConfig {
        page_width: 16,
        page_height: 16,
        padding: 1,
        max_gray_pages: 1,
        max_rgba_pages: 1,
        allow_eviction: false,
    });
    let key = GlyphKey::new(0, 2, 0);
    let inserted = atlas
        .insert(
            key,
            GlyphBitmap::rgba(2, 2, [0, 255, 0, 255].repeat(4)).expect("bitmap"),
        )
        .expect("insert");
    assert_eq!(inserted.slot.format, kitty_render::AtlasFormat::Rgba8);

    let mut frame = empty_frame(2, 2, 16.0, 16.0, Rgba8::BLACK);
    frame.glyphs.push(GlyphInstance {
        rect: Rect::new(0.0, 0.0, 16.0, 16.0),
        cell_x: 0,
        cell_y: 0,
        fg: Rgba8::WHITE,
        sprite: Some(inserted.slot),
        text: key.text,
        flags: InstanceFlags::default(),
    });
    backend.sync_atlas(&atlas, &frame).expect("atlas upload");
    backend
        .render(&frame, &RenderConfig::default())
        .expect("render");
    let pixels = backend.read_pixels().expect("readback");
    assert_pixel(pixel(&pixels, 32, 8, 8), [0, 255, 0, 255], 1, "rgba glyph");
}

#[test]
fn rounded_cursor_sdf() {
    let _guard = lock();
    let mut backend = match GlBackend::new(GlBackendConfig {
        cursor_radius: 16.0,
        ..GlBackendConfig::new(32, 32)
    }) {
        Ok(backend) => backend,
        Err(error) => {
            eprintln!("skipping GPU test: headless EGL unavailable: {error}");
            return;
        }
    };
    let mut frame = empty_frame(2, 2, 16.0, 16.0, Rgba8::BLACK);
    frame.cursors.push(CursorInstance {
        cell_x: 0,
        cell_y: 0,
        cell_rect: Rect::new(0.0, 0.0, 32.0, 32.0),
        shape: CursorShape::Block,
        color: Rgba8::rgb(255, 0, 0),
        text_color: Rgba8::WHITE,
        beam_thickness: 2.0,
        underline_thickness: 2.0,
        hollow_thickness: 1.0,
        flags: InstanceFlags::default(),
    });
    backend
        .render(&frame, &RenderConfig::default())
        .expect("render");
    let pixels = backend.read_pixels().expect("readback");
    assert_pixel(
        pixel(&pixels, 32, 16, 16),
        [255, 0, 0, 255],
        1,
        "rounded cursor centre",
    );
    assert_pixel(
        pixel(&pixels, 32, 0, 0),
        [0, 0, 0, 255],
        2,
        "rounded cursor corner (SDF discards it)",
    );
}

#[test]
fn damage_scissor_clips_rows() {
    let _guard = lock();
    let mut backend = try_backend!(64, 64);
    backend.clear(Rgba8::rgb(0, 0, 255)).expect("clear");

    let mut frame = empty_frame(2, 2, 32.0, 32.0, Rgba8::rgb(0, 0, 255));
    frame.damage = Damage {
        full_redraw: false,
        lines: vec![LineRange::new(0, 1)],
        ..Damage::default()
    };
    // Row 0 is inside the damaged (scissored) range and must be redrawn...
    frame.backgrounds.push(BackgroundInstance {
        rect: Rect::new(0.0, 0.0, 64.0, 32.0),
        cell_x: 0,
        cell_y: 0,
        color: Rgba8::rgb(255, 0, 0),
        is_default_bg: false,
        flags: InstanceFlags::default(),
    });
    // ...while row 1 is outside it and must keep the previous blue clear.
    frame.backgrounds.push(BackgroundInstance {
        rect: Rect::new(0.0, 32.0, 64.0, 32.0),
        cell_x: 0,
        cell_y: 1,
        color: Rgba8::rgb(0, 255, 0),
        is_default_bg: false,
        flags: InstanceFlags::default(),
    });

    let stats = backend
        .render(&frame, &RenderConfig::default())
        .expect("render");
    assert_eq!(stats.scissor_rects, 1);
    let pixels = backend.read_pixels().expect("readback");
    assert_pixel(
        pixel(&pixels, 64, 32, 16),
        [255, 0, 0, 255],
        1,
        "damaged row",
    );
    assert_pixel(
        pixel(&pixels, 64, 32, 48),
        [0, 0, 255, 255],
        1,
        "undamaged row",
    );
}

#[test]
fn resize_and_readback() {
    let _guard = lock();
    let mut backend = try_backend!(32, 32);
    backend.clear(Rgba8::rgb(255, 0, 0)).expect("clear");
    let pixels = backend.read_pixels().expect("readback");
    assert_eq!(pixel(&pixels, 32, 16, 16), [255, 0, 0, 255]);

    backend.resize(48, 24).expect("resize");
    assert_eq!(backend.size(), (48, 24));
    backend
        .clear(Rgba8::rgb(0, 128, 0))
        .expect("clear after resize");
    let pixels = backend.read_pixels().expect("readback after resize");
    assert_eq!(pixels.len(), 48 * 24 * 4);
    assert_eq!(pixel(&pixels, 48, 24, 12), [0, 128, 0, 255]);
    assert_eq!(pixel(&pixels, 48, 0, 23), [0, 128, 0, 255]);
}

#[test]
fn renderer_core_frame_round_trip() {
    let _guard = lock();
    let mut screen = Screen::builder(4, 2)
        .scrollback(16)
        .build()
        .expect("screen");
    screen.draw_text(&[b'a' as u32, b'b' as u32]);
    let resolver = BasicColorResolver::default();
    let mut rasterizer = |_key: &GlyphKey| GlyphBitmap::gray(8, 12, vec![0x80; 96]).ok();
    let config = RenderConfig::default();
    let mut core = RendererCore::new(config.clone(), AtlasConfig::default());
    let frame = core.prepare_frame(&screen, &mut rasterizer, &resolver);

    let mut backend = try_backend!(40, 40);
    let stats = backend
        .render_with_atlas(&frame, &config, &core.atlas)
        .expect("render frame");
    assert!(stats.instances >= 2, "background + glyph instances");
    assert!(stats.glyphs_skipped == 0);
    assert_eq!(backend.atlas_texture_count(), core.atlas.pages().len());

    let pixels = backend.read_pixels().expect("readback");
    let background = [
        frame.default_bg.r,
        frame.default_bg.g,
        frame.default_bg.b,
        255,
    ];
    let changed = (0..40 * 40).any(|index| {
        let offset = index * 4;
        pixels[offset..offset + 4] != background
    });
    assert!(changed, "rendered frame differs from the clear colour");
}
