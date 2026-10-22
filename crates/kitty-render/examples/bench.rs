// SPDX-License-Identifier: GPL-3.0-or-later
//! Micro-benchmark for frame preparation.
//!
//! Usage:
//!   cargo run --release -p kitty-render --example bench -- [--iters N] [--cols C] [--rows R]
//!
//! The default workload is a 200x50 screen containing 5000 distinct glyphs.

use std::hint::black_box;
use std::time::Instant;

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
                println!("usage: bench [--iters N] [--cols C] [--rows R]");
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

    let resolver = BasicColorResolver::default();
    let mut rasterizer = |key: &GlyphKey| {
        black_box(key);
        GlyphBitmap::gray(9, 17, vec![0x80; 9 * 17]).ok()
    };

    let mut core = RendererCore::new(RenderConfig::default(), Default::default());
    // Warm the atlas so the timed loop measures steady-state frame
    // preparation. The damage tracker is invalidated every iteration so the
    // full redraw bookkeeping is included.
    let warm = core.prepare_frame(&screen, &mut rasterizer, &resolver);
    println!(
        "screen: {}x{} cells, {} glyph cells, {} atlas glyphs, {} pages",
        columns,
        rows,
        drawn,
        core.atlas.len(),
        core.atlas.pages().len()
    );
    core.recycle_frame(warm);
    screen.reset_dirty();

    let start = Instant::now();
    let mut instance_count = 0usize;
    for _ in 0..iters {
        core.invalidate();
        let frame = core.prepare_frame(&screen, &mut rasterizer, &resolver);
        instance_count = instance_count.wrapping_add(black_box(frame.instance_count()));
        core.recycle_frame(frame);
    }
    let elapsed = start.elapsed();
    let per_frame = elapsed.as_secs_f64() / iters as f64;
    println!(
        "frame preparation: {} frames in {:?}  {:.3} ms/frame  {:.1} frames/s  ({} instances, {} atlas glyphs, {} pages)",
        iters,
        elapsed,
        per_frame * 1e3,
        1.0 / per_frame,
        instance_count / iters.max(1),
        core.atlas.len(),
        core.atlas.pages().len(),
    );
}
