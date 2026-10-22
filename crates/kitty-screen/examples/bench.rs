// SPDX-License-Identifier: GPL-3.0-or-later
// Benchmarks for the screen core, derived from kitty's draw/scroll hot paths.

//! A dependency-free micro-benchmark.
//!
//! Run with `cargo run --release --example bench -p kitty-screen`.

use std::time::{Duration, Instant};

use kitty_screen::Screen;

fn timed<F: FnMut()>(iters: u32, mut f: F) -> Duration {
    // Warm up.
    for _ in 0..(iters / 10).max(1) {
        f();
    }
    let start = Instant::now();
    for _ in 0..iters {
        f();
    }
    start.elapsed()
}

fn main() {
    const COLS: u32 = 120;
    const ROWS: u32 = 40;
    let text: Vec<u32> = "The quick brown fox jumps over the lazy dog 0123456789 "
        .chars()
        .map(|c| c as u32)
        .collect();

    let mut screen = Screen::builder(COLS, ROWS)
        .scrollback(1000)
        .build()
        .unwrap();

    let iters = 20_000u32;
    let draw_time = timed(iters, || {
        screen.draw_text(&text);
        if screen.cursor.y + 2 >= ROWS {
            screen.carriage_return();
            screen.cursor.y = 0;
        }
    });
    let bytes = text.len() as f64 * iters as f64;
    let secs = draw_time.as_secs_f64();
    println!(
        "draw_text ASCII: {:.2} MiB/s ({:.0} cells/s, {} iters of {} cells)",
        bytes / secs / (1024.0 * 1024.0),
        bytes / secs,
        iters,
        text.len()
    );

    let scroll_iters = 100_000u32;
    let scroll_time = timed(scroll_iters, || {
        screen.scroll(1);
    });
    println!(
        "scroll: {:.2} M lines/s ({scroll_iters} iterations)",
        scroll_iters as f64 / scroll_time.as_secs_f64() / 1e6
    );

    let mut wide = Screen::builder(COLS, ROWS)
        .scrollback(1000)
        .build()
        .unwrap();
    let cjk: Vec<u32> = std::iter::repeat_n('界' as u32, 200).collect();
    let wide_iters = 5_000u32;
    let wide_time = timed(wide_iters, || {
        wide.draw_text(&cjk);
        wide.cursor.y = 0;
        wide.cursor.x = 0;
    });
    println!(
        "draw_text wide CJK: {:.2} M cells/s",
        (cjk.len() as f64 * wide_iters as f64) / wide_time.as_secs_f64() / 1e6
    );
}
