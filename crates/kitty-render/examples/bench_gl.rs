// SPDX-License-Identifier: GPL-3.0-or-later
//! Offscreen OpenGL benchmark for the `gl` feature.
//!
//! Usage:
//!   cargo run --release -p kitty-render --features gl --example bench_gl -- \
//!       [--iters N] [--cols C] [--rows R]
//!
//! The default workload renders a 200x50 screen containing 5000 distinct
//! glyphs into a surfaceless EGL pbuffer. It reports draw throughput, draw +
//! readback throughput and raw `glReadPixels` throughput. On a machine without
//! a usable headless EGL context it prints a skip message and exits cleanly.

use std::hint::black_box;
use std::time::Instant;

use kitty_render::gl_backend::{FilterMode, GlBackend, GlBackendConfig};
use kitty_render::{BasicColorResolver, GlyphBitmap, GlyphKey, RenderConfig, RendererCore};
use kitty_screen::Screen;

fn main() {
    let mut iters = 100usize;
    let mut columns = 200u32;
    let mut rows = 50u32;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--iters" => {
                iters = args
                    .next()
                    .expect("--iters needs a number")
                    .parse()
                    .expect("invalid iteration count")
            }
            "--cols" => {
                columns = args
                    .next()
                    .expect("--cols needs a number")
                    .parse()
                    .expect("invalid column count")
            }
            "--rows" => {
                rows = args
                    .next()
                    .expect("--rows needs a number")
                    .parse()
                    .expect("invalid row count")
            }
            "-h" | "--help" => {
                println!("usage: bench_gl [--iters N] [--cols C] [--rows R]");
                return;
            }
            other => panic!("unknown argument: {other}"),
        }
    }

    let per_row = columns / 2;
    let mut screen = Screen::builder(columns, rows)
        .scrollback(1000)
        .build()
        .expect("failed to create screen");
    let mut drawn = 0usize;
    for row in 0..rows {
        screen.cursor_position(row + 1, 1);
        let text: Vec<u32> = (0..per_row)
            .map(|col| 0x4e00 + (row * per_row + col) % 20_000)
            .collect();
        drawn += text.len();
        screen.draw_text(&text);
    }

    let render_config = RenderConfig::default();
    let resolver = BasicColorResolver::default();
    let mut rasterizer = |key: &GlyphKey| {
        black_box(key);
        GlyphBitmap::gray(9, 17, vec![0x80; 9 * 17]).ok()
    };

    let mut core = RendererCore::new(render_config.clone(), Default::default());
    let width = (columns as f32 * render_config.metrics.cell_width) as u32;
    let height = (rows as f32 * render_config.metrics.cell_height) as u32;
    let mut backend = match GlBackend::new(GlBackendConfig {
        atlas_filter: FilterMode::Linear,
        ..GlBackendConfig::new(width.max(1), height.max(1))
    }) {
        Ok(backend) => backend,
        Err(error) => {
            eprintln!("skipping GPU bench: headless EGL unavailable: {error}");
            return;
        }
    };

    println!(
        "context: {} {} ({}) via EGL {}; GLSL {}",
        backend.gl_version_string(),
        backend.renderer().trim(),
        backend.vendor().trim(),
        {
            let (major, minor) = backend.context().egl_version();
            format!("{major}.{minor}")
        },
        backend.shading_language_version()
    );
    println!(
        "screen: {}x{} cells, {} glyph cells, drawable {}x{}",
        columns,
        rows,
        drawn,
        width.max(1),
        height.max(1)
    );

    // Cold frame: rasterizes every glyph and uploads all atlas pages.
    let frame = core.prepare_frame(&screen, &mut rasterizer, &resolver);
    let cold_stats = backend
        .render_with_atlas(&frame, &render_config, &core.atlas)
        .expect("cold render failed");
    println!(
        "atlas: {} glyphs, {} pages, {} textures; cold frame: {} instances, {} draw calls, {} new glyph slots",
        core.atlas.len(),
        core.atlas.pages().len(),
        backend.atlas_texture_count(),
        cold_stats.instances,
        cold_stats.draw_calls,
        frame.atlas_updates.len(),
    );
    core.recycle_frame(frame);
    screen.reset_dirty();

    // Draw only.
    let start = Instant::now();
    let mut instances = 0usize;
    for _ in 0..iters {
        core.invalidate();
        let frame = core.prepare_frame(&screen, &mut rasterizer, &resolver);
        let stats = backend
            .render_with_atlas(&frame, &render_config, &core.atlas)
            .expect("render failed");
        instances = instances.wrapping_add(black_box(stats.instances));
        core.recycle_frame(frame);
    }
    let elapsed = start.elapsed();
    let per_frame = elapsed.as_secs_f64() / iters as f64;
    println!(
        "draw:            {} frames in {:?}  {:.3} ms/frame  {:.1} frames/s  ({} instances/frame)",
        iters,
        elapsed,
        per_frame * 1e3,
        1.0 / per_frame,
        instances / iters,
    );

    // Draw + readback.
    let start = Instant::now();
    let mut checksum = 0u64;
    for _ in 0..iters {
        core.invalidate();
        let frame = core.prepare_frame(&screen, &mut rasterizer, &resolver);
        backend
            .render_with_atlas(&frame, &render_config, &core.atlas)
            .expect("render failed");
        let pixels = backend.read_pixels().expect("readback failed");
        checksum = checksum.wrapping_add(black_box(pixels[0]) as u64);
        core.recycle_frame(frame);
    }
    let elapsed = start.elapsed();
    let per_frame = elapsed.as_secs_f64() / iters as f64;
    let frame_bytes = backend.size().0 as u64 * backend.size().1 as u64 * 4;
    println!(
        "draw+readback:   {} frames in {:?}  {:.3} ms/frame  {:.1} frames/s  ({} MiB/frame, checksum {})",
        iters,
        elapsed,
        per_frame * 1e3,
        1.0 / per_frame,
        frame_bytes / (1024 * 1024),
        checksum,
    );

    // Readback only.
    let start = Instant::now();
    for _ in 0..iters {
        let pixels = backend.read_pixels().expect("readback failed");
        black_box(pixels[0]);
    }
    let elapsed = start.elapsed();
    let total_bytes = frame_bytes * iters as u64;
    let seconds = elapsed.as_secs_f64();
    println!(
        "readback only:   {} frames in {:?}  {:.3} ms/frame  {:.1} GiB/s",
        iters,
        elapsed,
        seconds / iters as f64 * 1e3,
        total_bytes as f64 / seconds / (1024.0 * 1024.0 * 1024.0),
    );
}
