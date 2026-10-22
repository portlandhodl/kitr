// SPDX-License-Identifier: GPL-3.0-or-later
//! Headless GPU tests for the optional EGL/OpenGL backend.
//!
//! Every test takes a process-wide mutex because a Mesa EGL display is
//! process-global. If a surfaceless context cannot be created, or the reported
//! GL version is too old, the test prints a skip message and returns instead
//! of failing.

#![cfg(feature = "gl")]

use std::sync::{Arc, Mutex, MutexGuard};

use kitty_render::gl_backend::{FilterMode, GammaMode, GlBackend, GlBackendConfig, GlBackendError};
use kitty_render::{
    AtlasConfig, BackgroundImageConfig, BackgroundInstance, BackgroundLayout, BasicColorResolver,
    CellMetrics, ContextConfig, CursorInstance, Damage, Frame, FullRedrawReason, GlyphAtlas,
    GlyphBitmap, GlyphInstance, GlyphKey, GridGeometry, ImageAsset, ImageFilter, ImageInstance,
    ImageKey, ImageUpload, ImageWrap, InstanceFlags, InstanceGenerator, LineRange, Rect,
    RenderConfig, RendererCore, Rgba8, TrailInstance,
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

fn image_2x2(key: ImageKey, wrap: ImageWrap) -> ImageUpload {
    ImageUpload::new(
        key,
        2,
        2,
        Arc::new(vec![
            255, 0, 0, 255, // top-left red
            0, 255, 0, 255, // top-right green
            0, 0, 255, 255, // bottom-left blue
            255, 255, 255, 255, // bottom-right white
        ]),
        ImageFilter::Nearest,
        wrap,
    )
}

fn image_frame_with(image: ImageInstance, upload: ImageUpload) -> Frame {
    let mut frame = empty_frame(2, 2, 16.0, 16.0, Rgba8::BLACK);
    frame.images.push(image);
    frame.image_uploads.push(upload);
    frame
}

#[test]
fn image_upload_placement_pixels() {
    let _guard = lock();
    let mut backend = try_backend!(32, 32);
    let key = ImageKey::Protocol(1);
    let mut frame = image_frame_with(
        ImageInstance::new(key, Rect::new(0.0, 0.0, 32.0, 32.0)),
        image_2x2(key, ImageWrap::Clamp),
    );
    frame.damage.full_redraw = true;

    let sync = backend.sync_images(&frame).expect("image upload");
    assert_eq!(sync.created, 1);
    assert_eq!(backend.image_texture_count(), 1);
    let stats = backend
        .render(&frame, &RenderConfig::default())
        .expect("render");
    assert_eq!(stats.images, 1);
    assert_eq!(stats.images_drawn, 1);
    assert_eq!(stats.images_skipped, 0);
    let pixels = backend.read_pixels().expect("readback");
    assert_pixel(pixel(&pixels, 32, 8, 8), [255, 0, 0, 255], 1, "top-left");
    assert_pixel(pixel(&pixels, 32, 24, 8), [0, 255, 0, 255], 1, "top-right");
    assert_pixel(
        pixel(&pixels, 32, 8, 24),
        [0, 0, 255, 255],
        1,
        "bottom-left",
    );
    assert_pixel(
        pixel(&pixels, 32, 24, 24),
        [255, 255, 255, 255],
        1,
        "bottom-right",
    );
}

#[test]
fn image_z_order_under_and_over_text() {
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
    let key = GlyphKey::new(0, 1, 0);
    let inserted = atlas
        .insert(key, GlyphBitmap::gray(2, 2, vec![255; 4]).expect("bitmap"))
        .expect("insert");

    let image_key = ImageKey::Protocol(2);
    let mut under = image_frame_with(
        ImageInstance::new(image_key, Rect::new(0.0, 0.0, 32.0, 32.0)).with_z_index(-1),
        image_2x2(image_key, ImageWrap::Clamp),
    );
    under.glyphs.push(GlyphInstance {
        rect: Rect::new(0.0, 0.0, 32.0, 32.0),
        cell_x: 0,
        cell_y: 0,
        fg: Rgba8::rgb(0, 0, 255),
        sprite: Some(inserted.slot),
        text: key.text,
        flags: InstanceFlags::default(),
    });
    backend.sync_atlas(&atlas, &under).expect("atlas");
    backend.sync_images(&under).expect("images");
    backend
        .render(&under, &RenderConfig::default())
        .expect("render under");
    let pixels = backend.read_pixels().expect("readback under");
    assert_pixel(
        pixel(&pixels, 32, 16, 16),
        [0, 0, 255, 255],
        1,
        "text over a negative-z image",
    );

    let mut over = image_frame_with(
        ImageInstance::new(image_key, Rect::new(0.0, 0.0, 32.0, 32.0)).with_z_index(1),
        image_2x2(image_key, ImageWrap::Clamp),
    );
    over.glyphs.push(GlyphInstance {
        rect: Rect::new(0.0, 0.0, 32.0, 32.0),
        cell_x: 0,
        cell_y: 0,
        fg: Rgba8::rgb(0, 0, 255),
        sprite: Some(inserted.slot),
        text: key.text,
        flags: InstanceFlags::default(),
    });
    backend.sync_images(&over).expect("images");
    backend
        .render(&over, &RenderConfig::default())
        .expect("render over");
    let pixels = backend.read_pixels().expect("readback over");
    assert_pixel(
        pixel(&pixels, 32, 8, 8),
        [255, 0, 0, 255],
        1,
        "positive-z image over text",
    );
}

#[test]
fn image_delete_removes_texture_and_pixels() {
    let _guard = lock();
    let mut backend = try_backend!(32, 32);
    let key = ImageKey::Protocol(3);
    let frame = image_frame_with(
        ImageInstance::new(key, Rect::new(0.0, 0.0, 32.0, 32.0)),
        image_2x2(key, ImageWrap::Clamp),
    );
    backend.sync_images(&frame).expect("upload");
    backend
        .render(&frame, &RenderConfig::default())
        .expect("draw");
    assert_eq!(backend.image_texture_count(), 1);

    let mut cleared = empty_frame(2, 2, 16.0, 16.0, Rgba8::BLACK);
    cleared.damage.full_redraw = true;
    let sync = backend.sync_images(&cleared).expect("sync after delete");
    assert_eq!(sync.removed, 1);
    assert_eq!(backend.image_texture_count(), 0);
    assert!(!backend.has_image_texture(key));
    let stats = backend
        .render(&cleared, &RenderConfig::default())
        .expect("render after delete");
    assert_eq!(stats.images_drawn, 0);
    let pixels = backend.read_pixels().expect("readback");
    assert_pixel(pixel(&pixels, 32, 16, 16), [0, 0, 0, 255], 1, "cleared");
}

#[test]
fn image_source_rect_and_scaling() {
    let _guard = lock();
    let mut backend = try_backend!(32, 32);
    let key = ImageKey::Protocol(4);
    let mut image = ImageInstance::new(key, Rect::new(0.0, 0.0, 16.0, 16.0));
    image.uv = kitty_render::UvRect {
        u0: 0.5,
        v0: 0.0,
        u1: 1.0,
        v1: 0.5,
    };
    let frame = image_frame_with(image, image_2x2(key, ImageWrap::Clamp));
    backend.sync_images(&frame).expect("upload");
    backend
        .render(&frame, &RenderConfig::default())
        .expect("draw");
    let pixels = backend.read_pixels().expect("readback");
    // Only the top-right texel is sampled, stretched over the 16x16 quad.
    assert_pixel(
        pixel(&pixels, 32, 8, 8),
        [0, 255, 0, 255],
        1,
        "source texel",
    );
    assert_pixel(
        pixel(&pixels, 32, 24, 24),
        [0, 0, 0, 255],
        1,
        "outside quad",
    );
}

fn background_frame(
    asset: &ImageAsset,
    layout: BackgroundLayout,
    viewport_width: u32,
    viewport_height: u32,
) -> Frame {
    let config = BackgroundImageConfig {
        asset: asset.clone(),
        layout,
        opacity: 1.0,
        tint: 0.0,
        linear: false,
    };
    let (instance, upload) = kitty_render::background_image(
        &config,
        viewport_width as f32,
        viewport_height as f32,
        Rgba8::BLACK,
    )
    .expect("background geometry");
    let columns = (viewport_width / 16).max(1);
    let rows = (viewport_height / 16).max(1);
    let mut frame = empty_frame(columns, rows, 16.0, 16.0, Rgba8::BLACK);
    frame.background_image = Some(instance);
    frame.image_uploads.push(upload);
    frame
}

fn asset_2x2() -> ImageAsset {
    ImageAsset::from_rgba(
        2,
        2,
        vec![
            255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
        ],
    )
    .expect("asset")
}

#[test]
fn background_layout_modes_pixels() {
    let _guard = lock();
    let mut backend = try_backend!(64, 32);
    let asset = asset_2x2();

    let frame = background_frame(&asset, BackgroundLayout::Tiling, 64, 32);
    backend.sync_images(&frame).expect("upload tiling");
    let stats = backend
        .render(&frame, &RenderConfig::default())
        .expect("render tiling");
    assert!(stats.background_image_drawn);
    let pixels = backend.read_pixels().expect("readback tiling");
    assert_pixel(pixel(&pixels, 64, 0, 0), [255, 0, 0, 255], 1, "tiling 0,0");
    assert_pixel(pixel(&pixels, 64, 3, 0), [0, 255, 0, 255], 1, "tiling 1,0");
    assert_pixel(
        pixel(&pixels, 64, 4, 0),
        [255, 0, 0, 255],
        1,
        "tiling repeat x",
    );
    assert_pixel(pixel(&pixels, 64, 0, 3), [0, 0, 255, 255], 1, "tiling 0,1");

    let frame = background_frame(&asset, BackgroundLayout::Mirrored, 64, 32);
    backend.sync_images(&frame).expect("upload mirrored");
    backend
        .render(&frame, &RenderConfig::default())
        .expect("render mirrored");
    let pixels = backend.read_pixels().expect("readback mirrored");
    assert_pixel(
        pixel(&pixels, 64, 3, 0),
        [255, 0, 0, 255],
        1,
        "mirrored flips x",
    );

    let frame = background_frame(&asset, BackgroundLayout::Clamped, 64, 32);
    backend.sync_images(&frame).expect("upload clamped");
    backend
        .render(&frame, &RenderConfig::default())
        .expect("render clamped");
    let pixels = backend.read_pixels().expect("readback clamped");
    assert_pixel(
        pixel(&pixels, 64, 63, 0),
        [0, 255, 0, 255],
        1,
        "clamped right edge",
    );
    assert_pixel(
        pixel(&pixels, 64, 0, 31),
        [0, 0, 255, 255],
        1,
        "clamped bottom edge",
    );

    let frame = background_frame(&asset, BackgroundLayout::Scaled, 64, 32);
    backend.sync_images(&frame).expect("upload scaled");
    backend
        .render(&frame, &RenderConfig::default())
        .expect("render scaled");
    let pixels = backend.read_pixels().expect("readback scaled");
    assert_pixel(pixel(&pixels, 64, 8, 8), [255, 0, 0, 255], 1, "scaled tl");
    assert_pixel(pixel(&pixels, 64, 56, 8), [0, 255, 0, 255], 1, "scaled tr");
    assert_pixel(pixel(&pixels, 64, 8, 24), [0, 0, 255, 255], 1, "scaled bl");
    assert_pixel(
        pixel(&pixels, 64, 56, 24),
        [255, 255, 255, 255],
        1,
        "scaled br",
    );

    let frame = background_frame(&asset, BackgroundLayout::CenterClamped, 64, 32);
    backend.sync_images(&frame).expect("upload centered");
    backend
        .render(&frame, &RenderConfig::default())
        .expect("render centered");
    let pixels = backend.read_pixels().expect("readback centered");
    assert_pixel(
        pixel(&pixels, 64, 30, 14),
        [0, 0, 0, 255],
        1,
        "center margin",
    );
    assert_pixel(pixel(&pixels, 64, 31, 15), [255, 0, 0, 255], 1, "center tl");
    assert_pixel(
        pixel(&pixels, 64, 32, 16),
        [255, 255, 255, 255],
        1,
        "center br",
    );

    let frame = background_frame(&asset, BackgroundLayout::CenterScaled, 64, 32);
    backend.sync_images(&frame).expect("upload cscaled");
    backend
        .render(&frame, &RenderConfig::default())
        .expect("render cscaled");
    let pixels = backend.read_pixels().expect("readback cscaled");
    assert_pixel(pixel(&pixels, 64, 8, 8), [255, 0, 0, 255], 1, "cscaled tl");
    assert_pixel(
        pixel(&pixels, 64, 56, 24),
        [255, 255, 255, 255],
        1,
        "cscaled br",
    );
}

#[test]
fn background_image_survives_resize() {
    let _guard = lock();
    let mut backend = try_backend!(32, 32);
    let asset = asset_2x2();
    let frame = background_frame(&asset, BackgroundLayout::Tiling, 32, 32);
    backend.sync_images(&frame).expect("upload");
    backend
        .render(&frame, &RenderConfig::default())
        .expect("draw");
    assert_eq!(backend.image_texture_count(), 1);

    backend.resize(64, 32).expect("resize");
    let resized = background_frame(&asset, BackgroundLayout::Tiling, 64, 32);
    let sync = backend.sync_images(&resized).expect("sync after resize");
    assert_eq!(sync.unchanged, 1);
    assert_eq!(sync.created, 0);
    assert_eq!(backend.image_texture_count(), 1);
    backend
        .render(&resized, &RenderConfig::default())
        .expect("draw after resize");
    let pixels = backend.read_pixels().expect("readback");
    assert_eq!(pixels.len(), 64 * 32 * 4);
    assert_pixel(
        pixel(&pixels, 64, 4, 0),
        [255, 0, 0, 255],
        1,
        "tiling after resize",
    );
    assert_pixel(
        pixel(&pixels, 64, 3, 0),
        [0, 255, 0, 255],
        1,
        "tiling after resize x",
    );
}

#[test]
fn cursor_trail_fades_in_gl() {
    let _guard = lock();
    let mut backend = try_backend!(32, 32);
    let mut frame = empty_frame(2, 2, 16.0, 16.0, Rgba8::BLACK);
    frame.trail.push(TrailInstance {
        rect: Rect::new(0.0, 0.0, 32.0, 32.0),
        color: Rgba8::rgb(255, 0, 0),
        opacity: 0.5,
    });
    let stats = backend
        .render(&frame, &RenderConfig::default())
        .expect("render trail");
    assert_eq!(stats.trail_instances, 1);
    let pixels = backend.read_pixels().expect("readback");
    assert_pixel(pixel(&pixels, 32, 16, 16), [128, 0, 0, 255], 1, "50% trail");

    let mut faded = empty_frame(2, 2, 16.0, 16.0, Rgba8::BLACK);
    faded.trail.push(TrailInstance {
        rect: Rect::new(0.0, 0.0, 32.0, 32.0),
        color: Rgba8::rgb(255, 0, 0),
        opacity: 0.25,
    });
    backend
        .render(&faded, &RenderConfig::default())
        .expect("render faded trail");
    let pixels = backend.read_pixels().expect("readback faded");
    assert_pixel(pixel(&pixels, 32, 16, 16), [64, 0, 0, 255], 1, "25% trail");
}
#[test]
fn background_opacity_and_tint() {
    let _guard = lock();
    let mut backend = try_backend!(32, 32);
    let asset = ImageAsset::from_rgba(1, 1, vec![255, 255, 255, 255]).unwrap();

    let half = BackgroundImageConfig {
        asset: asset.clone(),
        layout: BackgroundLayout::Scaled,
        opacity: 0.5,
        tint: 0.0,
        linear: false,
    };
    let (instance, upload) =
        kitty_render::background_image(&half, 32.0, 32.0, Rgba8::BLACK).unwrap();
    let mut frame = empty_frame(2, 2, 16.0, 16.0, Rgba8::BLACK);
    frame.background_image = Some(instance);
    frame.image_uploads.push(upload);
    backend.sync_images(&frame).expect("upload half");
    backend
        .render(&frame, &RenderConfig::default())
        .expect("draw half");
    let pixels = backend.read_pixels().expect("readback half");
    assert_pixel(
        pixel(&pixels, 32, 16, 16),
        [128, 128, 128, 255],
        1,
        "50% white",
    );

    let tinted = BackgroundImageConfig { tint: 1.0, ..half };
    let (instance, upload) =
        kitty_render::background_image(&tinted, 32.0, 32.0, Rgba8::rgb(0, 0, 255)).unwrap();
    frame.background_image = Some(instance);
    frame.image_uploads.clear();
    frame.image_uploads.push(upload);
    backend.sync_images(&frame).expect("upload tinted");
    backend
        .render(&frame, &RenderConfig::default())
        .expect("draw tinted");
    let pixels = backend.read_pixels().expect("readback tinted");
    // Fully tinted with blue at 50% opacity over black.
    assert_pixel(
        pixel(&pixels, 32, 16, 16),
        [0, 0, 128, 255],
        1,
        "tinted blue",
    );
}

#[test]
fn placeholder_quads_render_pixels() {
    let _guard = lock();
    let mut screen = Screen::builder(3, 3).scrollback(8).build().expect("screen");
    let mut image = kitty_graphics::parse_raw(b"a=t,f=32,s=2,v=2,i=7").unwrap();
    image.payload = vec![
        255, 0, 0, 255, // top-left red
        0, 255, 0, 255, // top-right green
        0, 0, 255, 255, // bottom-left blue
        255, 255, 255, 255, // bottom-right white
    ];
    screen.handle_graphics_command(image).unwrap();
    let mut virtual_placement = kitty_graphics::parse_raw(b"a=p,U=1,i=7,c=2,r=2").unwrap();
    virtual_placement.payload.clear();
    screen.handle_graphics_command(virtual_placement).unwrap();
    screen.cursor.sgr.fg = (7 << 8) | 1;
    screen.cursor.sgr.decoration_fg = 0;
    for row in 0..2u32 {
        let mut text = Vec::new();
        for column in 0..2u32 {
            text.push(kitty_core::cell::IMAGE_PLACEHOLDER_CHAR);
            text.push(if row == 0 { 0x305 } else { 0x30d + row - 1 });
            text.push(if column == 0 {
                0x305
            } else {
                0x30d + column - 1
            });
        }
        screen.cursor_position(row + 1, 1);
        screen.draw_text(&text);
    }

    let config = RenderConfig {
        metrics: CellMetrics {
            cell_width: 16.0,
            cell_height: 16.0,
            ..CellMetrics::default()
        },
        viewport_width: 64.0,
        viewport_height: 32.0,
        shaped_runs: false,
        ..RenderConfig::default()
    };
    let generator = InstanceGenerator::new(config.clone());
    let mut atlas = GlyphAtlas::new(AtlasConfig::default());
    let mut rasterizer = |_key: &GlyphKey| GlyphBitmap::gray(8, 12, vec![0; 96]).ok();
    let resolver = BasicColorResolver::default();
    let mut frame = Frame::default();
    generator.generate(&screen, &mut atlas, &mut rasterizer, &resolver, &mut frame);
    frame.damage.full_redraw = true;
    assert_eq!(frame.images.len(), 2);
    assert!(
        frame.glyphs.is_empty(),
        "placeholder cells must not draw glyphs"
    );

    let mut backend = try_backend!(64, 32);
    backend.sync_images(&frame).expect("placeholder upload");
    backend
        .render(&frame, &config)
        .expect("render placeholders");
    let pixels = backend.read_pixels().expect("readback");
    assert_pixel(pixel(&pixels, 64, 8, 8), [255, 0, 0, 255], 1, "top-left");
    assert_pixel(pixel(&pixels, 64, 24, 8), [0, 255, 0, 255], 1, "top-right");
    assert_pixel(
        pixel(&pixels, 64, 8, 24),
        [0, 0, 255, 255],
        1,
        "bottom-left",
    );
    assert_pixel(
        pixel(&pixels, 64, 24, 24),
        [255, 255, 255, 255],
        1,
        "bottom-right",
    );
}

#[test]
fn animated_image_swap_renders_new_frame_pixels() {
    let _guard = lock();
    let mut screen = Screen::builder(3, 2).scrollback(8).build().expect("screen");
    let mut image = kitty_graphics::parse_raw(b"a=t,f=32,s=1,v=1,i=11").unwrap();
    image.payload = vec![255, 0, 0, 255];
    screen.handle_graphics_command(image).unwrap();
    let mut placement = kitty_graphics::parse_raw(b"a=p,i=11,p=1,c=1,r=1,z=0").unwrap();
    placement.payload.clear();
    screen.handle_graphics_command(placement).unwrap();

    let mut manager = kitty_graphics::AnimationManager::new();
    manager.insert_root(screen.image(11).unwrap()).unwrap();
    let mut frame_command = kitty_graphics::parse_raw(b"a=f,i=11,r=2,s=1,v=1").unwrap();
    frame_command.payload = vec![0, 255, 0, 255];
    manager
        .feed(&frame_command, &kitty_graphics::ImageLimits::default(), 0)
        .unwrap();
    manager.set_gap(11, 1, 10);
    manager.set_gap(11, 2, 10);
    manager.set_state(11, kitty_graphics::AnimationState::Running, 0);

    let config = RenderConfig {
        metrics: CellMetrics {
            cell_width: 16.0,
            cell_height: 16.0,
            ..CellMetrics::default()
        },
        viewport_width: 32.0,
        viewport_height: 32.0,
        ..RenderConfig::default()
    };
    let generator = InstanceGenerator::new(config.clone()).with_animations(manager);
    let mut atlas = GlyphAtlas::new(AtlasConfig::default());
    let mut rasterizer = |_key: &GlyphKey| GlyphBitmap::gray(8, 12, vec![0; 96]).ok();
    let resolver = BasicColorResolver::default();

    let mut frame = Frame::default();
    generator.generate(&screen, &mut atlas, &mut rasterizer, &resolver, &mut frame);
    frame.damage.full_redraw = true;
    let mut backend = try_backend!(32, 32);
    backend.sync_images(&frame).expect("root upload");
    backend.render(&frame, &config).expect("render root");
    let pixels = backend.read_pixels().expect("readback root");
    assert_pixel(pixel(&pixels, 32, 8, 8), [255, 0, 0, 255], 1, "root red");

    let tick = generator.tick_animations(10);
    assert_eq!(tick.redraw, vec![11]);
    let mut frame = Frame::default();
    generator.generate(&screen, &mut atlas, &mut rasterizer, &resolver, &mut frame);
    frame.damage.full_redraw = true;
    backend.sync_images(&frame).expect("frame upload");
    backend.render(&frame, &config).expect("render frame");
    let pixels = backend.read_pixels().expect("readback frame");
    assert_pixel(pixel(&pixels, 32, 8, 8), [0, 255, 0, 255], 1, "frame green");
}
