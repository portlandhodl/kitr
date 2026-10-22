// SPDX-License-Identifier: GPL-3.0-or-later
//! Micro-benchmark for frame preparation, overlays and shaped-run sprites.
//!
//! Usage:
//!   cargo run --release -p kitty-render --example bench -- [--iters N] [--cols C] [--rows R] [--no-overlays] [--no-shaped]
//!
//! The default workload is a 200x50 screen containing 5000 distinct glyphs.
//! It measures a baseline frame, the same frame with selection/hyperlink/tab-bar
//! overlays, and an ASCII workload with ligature-shaped sprites. The shaped
//! phase uses a fake rasterizer so no font file is required.

use std::hint::black_box;
use std::time::Instant;

use kitty_graphics::{decode_image, parse_raw, AnimationManager, AnimationState, ImageLimits};
use kitty_render::{
    collect_protocol_images_with_animations, expand_unicode_placeholders, BasicColorResolver,
    CellMetrics, GlyphBitmap, GlyphKey, GlyphRasterizer, GridGeometry, OverlayStats, RenderConfig,
    RendererCore, SelectionRange, ShapedRunCluster, ShapedRunOutput, ShapedRunRequest, TabBarSpec,
};
use kitty_screen::Screen;

#[derive(Default)]
struct BenchRasterizer;

impl BenchRasterizer {
    fn cell_bitmap() -> GlyphBitmap {
        GlyphBitmap::gray(9, 17, vec![0x80; 9 * 17]).expect("valid gray bitmap")
    }
}

impl GlyphRasterizer for BenchRasterizer {
    fn rasterize(&mut self, key: &GlyphKey) -> Option<GlyphBitmap> {
        black_box(key);
        Some(Self::cell_bitmap())
    }

    fn shape_run(&mut self, run: &ShapedRunRequest<'_>) -> Option<ShapedRunOutput> {
        let cw = run.cell_width.round().max(1.0) as u32;
        let ch = run.cell_height.round().max(1.0) as u32;
        let mut clusters = Vec::new();
        let mut i = 0usize;
        while i + 1 < run.text.len() {
            let pair = (run.text[i], run.text[i + 1]);
            let ligature = matches!(
                pair,
                (0x2d, 0x3e) | (0x3d, 0x3e) | (0x21, 0x3d) | (0x3c, 0x3d) | (0x3d, 0x3d)
            );
            let cell = run.cell_for_codepoint.get(i).copied();
            let next_cell = run.cell_for_codepoint.get(i + 1).copied();
            if ligature && matches!((cell, next_cell), (Some(a), Some(b)) if b == a + 1) {
                let cell = cell.unwrap();
                let bitmap = GlyphBitmap::gray(cw * 2, ch, vec![0xff; (cw * 2 * ch) as usize])
                    .expect("valid ligature bitmap");
                clusters.push(ShapedRunCluster {
                    cell_start: cell,
                    cell_span: 2,
                    bitmap,
                });
                i += 2;
            } else {
                i += 1;
            }
        }
        (!clusters.is_empty()).then_some(ShapedRunOutput { clusters })
    }
}

struct BenchResult {
    per_frame: f64,
    atlas_glyphs: usize,
    atlas_pages: usize,
    overlay: OverlayStats,
    atlas_insertions: u64,
}

fn bench_frames(
    label: &str,
    screen: &Screen,
    config: RenderConfig,
    rasterizer: &mut dyn GlyphRasterizer,
    resolver: &dyn kitty_render::ColorResolver,
    iters: usize,
) -> BenchResult {
    let mut core = RendererCore::new(config, Default::default());
    let warm = core.prepare_frame(screen, rasterizer, resolver);
    println!(
        "{label}: {} glyph instances, {} linked cells, {} tab glyphs, {} atlas glyphs, {} pages",
        warm.glyphs.len(),
        warm.overlay_stats.hyperlink_cells,
        warm.overlay_stats.tab_glyphs,
        core.atlas.len(),
        core.atlas.pages().len()
    );
    let overlay = warm.overlay_stats;
    core.recycle_frame(warm);

    let start = Instant::now();
    let mut instance_count = 0usize;
    let mut last_overlay = OverlayStats::default();
    for _ in 0..iters {
        core.invalidate();
        let frame = core.prepare_frame(screen, rasterizer, resolver);
        instance_count = instance_count.wrapping_add(black_box(frame.instance_count()));
        last_overlay = frame.overlay_stats;
        core.recycle_frame(frame);
    }
    let elapsed = start.elapsed();
    let per_frame = elapsed.as_secs_f64() / iters.max(1) as f64;
    let stats = core.atlas.stats();
    println!(
        "{label}: {iters} frames in {:?}  {:.3} ms/frame  {:.1} frames/s  ({} instances, {} shaped sprites, {} shaped cache hits/{} misses)",
        elapsed,
        per_frame * 1e3,
        1.0 / per_frame,
        instance_count / iters.max(1),
        last_overlay.shaped_sprites,
        last_overlay.shaped_cache_hits,
        last_overlay.shaped_cache_misses,
    );
    BenchResult {
        per_frame,
        atlas_glyphs: core.atlas.len(),
        atlas_pages: core.atlas.pages().len(),
        overlay,
        atlas_insertions: stats.insertions,
    }
}

fn bench_animation_frame_swap(iters: usize) {
    let mut root_command = parse_raw(b"a=t,f=32,s=32,v=32,i=1").unwrap();
    root_command.payload = vec![0u8; 32 * 32 * 4];
    let root = decode_image(
        &root_command,
        &root_command.payload,
        &ImageLimits::default(),
    )
    .unwrap();
    let mut manager = AnimationManager::new();
    manager.insert_root(&root).unwrap();
    for number in 2..=16u32 {
        let mut command = parse_raw(format!("a=f,i=1,r={number},s=32,v=32").as_bytes()).unwrap();
        command.payload = vec![number as u8; 32 * 32 * 4];
        manager.feed(&command, &ImageLimits::default(), 0).unwrap();
        manager.set_gap(1, number, 5);
    }
    manager.set_gap(1, 1, 5);
    manager.set_state(1, AnimationState::Running, 0);

    let start = Instant::now();
    let mut swaps = 0usize;
    for i in 0..iters {
        let tick = manager.tick(i as u64 * 5);
        swaps += tick.redraw.len();
        black_box(manager.image(1).unwrap().current_data());
    }
    let elapsed = start.elapsed();
    let per_swap = elapsed.as_secs_f64() / iters.max(1) as f64;
    println!(
        "animation: {iters} ticks in {:?}  {:.3} us/tick  {:.0} ticks/s  ({} frame swaps)",
        elapsed,
        per_swap * 1e6,
        1.0 / per_swap,
        swaps
    );
}

fn diacritic_for(number: u32) -> u32 {
    if number == 0 {
        0x305
    } else {
        0x30d + number - 1
    }
}

fn bench_placeholder_expansion(iters: usize, columns: u32, rows: u32) {
    let mut screen = Screen::builder(columns, rows)
        .scrollback(16)
        .build()
        .expect("placeholder screen");
    let mut image = parse_raw(b"a=t,f=32,s=1,v=1,i=42").unwrap();
    image.payload = vec![1, 2, 3, 255];
    screen.handle_graphics_command(image).unwrap();
    let mut placement = parse_raw(format!("a=p,U=1,i=42,c={columns},r={rows}").as_bytes()).unwrap();
    placement.payload.clear();
    screen.handle_graphics_command(placement).unwrap();
    screen.cursor.sgr.fg = (42 << 8) | 1;
    for row in 0..rows {
        let mut text = Vec::with_capacity(columns as usize + 1);
        text.push(kitty_core::cell::IMAGE_PLACEHOLDER_CHAR);
        text.push(diacritic_for(row));
        text.resize(
            columns as usize + 1,
            kitty_core::cell::IMAGE_PLACEHOLDER_CHAR,
        );
        screen.cursor_position(row + 1, 1);
        screen.draw_text(&text);
    }

    let geometry = GridGeometry::new(
        columns,
        rows,
        CellMetrics {
            cell_width: 9.0,
            cell_height: 17.0,
            ..CellMetrics::default()
        },
    );
    let cells = u64::from(columns) * u64::from(rows);
    let start = Instant::now();
    let mut runs = 0usize;
    let mut instances = Vec::new();
    let mut uploads = Vec::new();
    for _ in 0..iters {
        runs += expand_unicode_placeholders(&screen).len();
        instances.clear();
        uploads.clear();
        collect_protocol_images_with_animations(
            &screen,
            None,
            &geometry,
            columns as f32 * 9.0,
            rows as f32 * 17.0,
            kitty_render::ImageFilter::Nearest,
            &mut instances,
            &mut uploads,
        );
        black_box(instances.len());
    }
    let elapsed = start.elapsed();
    let per_iter = elapsed.as_secs_f64() / iters.max(1) as f64;
    println!(
        "placeholders: {}x{} cells, {iters} expansions in {:?}  {:.3} ms/pass  {:.1} Mcell/s  ({} runs/pass, {} quads/pass)",
        columns,
        rows,
        elapsed,
        per_iter * 1e3,
        cells as f64 * iters as f64 / elapsed.as_secs_f64().max(f64::EPSILON) / 1e6,
        runs / iters.max(1),
        instances.len()
    );
}

fn main() {
    let mut iters = 100usize;
    let mut columns = 200u32;
    let mut rows = 50u32;
    let mut overlays = true;
    let mut shaped = true;
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
            "--no-overlays" => overlays = false,
            "--no-shaped" => shaped = false,
            "-h" | "--help" => {
                println!(
                    "usage: bench [--iters N] [--cols C] [--rows R] [--no-overlays] [--no-shaped]"
                );
                return;
            }
            other => panic!("unknown argument: {other}"),
        }
    }

    // 100 distinct CJK codepoints per row, each two cells wide, fills the
    // default 200 column screen and gives 5000 glyphs for 50 rows.
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
    println!("screen: {}x{} cells, {} glyph cells", columns, rows, drawn);

    let mut resolver = BasicColorResolver::default();
    resolver.selection_bg = kitty_render::Rgba8::rgb(38, 79, 120);
    let resolver = resolver;
    let mut rasterizer = BenchRasterizer;
    let base_config = RenderConfig {
        shaped_runs: false,
        ..RenderConfig::default()
    };
    let base = bench_frames(
        "baseline",
        &screen,
        base_config.clone(),
        &mut rasterizer,
        &resolver,
        iters,
    );

    if overlays {
        let mut overlay_screen = screen;
        overlay_screen.cursor_position(1, 1);
        overlay_screen.set_active_hyperlink(Some("bench"), Some("https://example.com"));
        let link: Vec<u32> = "https://example.com/kitty"
            .chars()
            .map(|c| c as u32)
            .collect();
        overlay_screen.draw_text(&link);
        overlay_screen.set_active_hyperlink(None, None);
        let viewport_width = columns as f32 * base_config.metrics.cell_width;
        let viewport_height = rows as f32 * base_config.metrics.cell_height;
        let overlay_config = RenderConfig {
            shaped_runs: false,
            viewport_width,
            viewport_height,
            selections: vec![
                SelectionRange::cell(0, 0, columns / 2, (rows / 2) as i64),
                SelectionRange::block(columns / 4, 1, columns / 2, (rows / 3) as i64),
            ],
            hyperlink_hover: Some(kitty_render::HyperlinkHover { x: 2, y: 0 }),
            tab_bar: Some(TabBarSpec {
                titles: (0..6).map(|i| format!("tab {i}")).collect(),
                active: 2,
                height: 30.0,
                ..TabBarSpec::default()
            }),
            ..RenderConfig::default()
        };
        let over = bench_frames(
            "overlays",
            &overlay_screen,
            overlay_config,
            &mut rasterizer,
            &resolver,
            iters,
        );
        println!(
            "overlay delta: {:+.3} ms/frame, {} selected cells, {} hyperlink decorations, {} tab glyphs (atlas {} glyphs/{} pages)",
            (over.per_frame - base.per_frame) * 1e3,
            over.overlay.selection_cells,
            over.overlay.hyperlink_decorations,
            over.overlay.tab_glyphs,
            over.atlas_glyphs,
            over.atlas_pages,
        );
    }

    if shaped {
        let mut shaped_screen = Screen::builder(columns, rows)
            .scrollback(1000)
            .build()
            .expect("failed to create shaped screen");
        let pattern = [
            b'-' as u32,
            b'>' as u32,
            b'=' as u32,
            b'>' as u32,
            b'!' as u32,
            b'=' as u32,
        ];
        for row in 0..rows {
            shaped_screen.cursor_position(row + 1, 1);
            let text: Vec<u32> = (0..columns)
                .map(|col| pattern[col as usize % pattern.len()])
                .collect();
            shaped_screen.draw_text(&text);
        }
        let shaped_config = RenderConfig {
            shaped_runs: true,
            ..RenderConfig::default()
        };
        let result = bench_frames(
            "shaped",
            &shaped_screen,
            shaped_config,
            &mut rasterizer,
            &resolver,
            iters,
        );
        let hits = result.overlay.shaped_cache_hits as f64;
        let misses = result.overlay.shaped_cache_misses as f64;
        let total = (hits + misses).max(1.0);
        println!(
            "shaped runs: {} attempts, {} sprites, cache hit rate {:.1}% ({} hits / {} misses), {} atlas insertions, {:.1} ms/frame",
            result.overlay.shaped_run_attempts,
            result.overlay.shaped_sprites,
            hits / total * 100.0,
            result.overlay.shaped_cache_hits,
            result.overlay.shaped_cache_misses,
            result.atlas_insertions,
            result.per_frame * 1e3,
        );
    }
    bench_animation_frame_swap(iters);
    bench_placeholder_expansion(iters, columns, rows);
}
