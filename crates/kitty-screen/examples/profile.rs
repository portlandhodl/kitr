// SPDX-License-Identifier: GPL-3.0-or-later
//! Reduced-iteration workload driver for profiling (callgrind et al).
//!
//! Select the workload with `KITTY_SCREEN_PROFILE=ascii|wide|parser` (default
//! `ascii`) and run under a profiler. Iteration counts are intentionally small
//! so instruction-level profilers finish quickly; this file is not a benchmark
//! and makes no timing claims.

use std::hint::black_box;

use kitty_screen::{Screen, ScreenHandler};
use kitty_vt::Parser;

fn main() {
    const COLS: u32 = 120;
    const ROWS: u32 = 40;
    let which = std::env::var("KITTY_SCREEN_PROFILE").unwrap_or_else(|_| "ascii".to_string());
    match which.as_str() {
        "wide" => {
            let mut screen = Screen::builder(COLS, ROWS)
                .scrollback(1000)
                .build()
                .unwrap();
            let wide: Vec<u32> =
                std::iter::repeat_n('界' as u32, (COLS * ROWS / 2) as usize).collect();
            for _ in 0..64 {
                screen.draw_text(&wide);
            }
            black_box(&screen);
        }
        "parser" => {
            let stream = make_stream(1024 * 1024);
            let mut handler = ScreenHandler::new(
                Screen::builder(COLS, ROWS)
                    .scrollback(1000)
                    .build()
                    .unwrap(),
            );
            let mut parser = Parser::new();
            for _ in 0..2 {
                handler.feed(&mut parser, &stream);
            }
            black_box(&handler.screen);
        }
        _ => {
            let pattern = "The quick brown fox jumps over the lazy dog 0123456789 ";
            let mut text = String::with_capacity((COLS * ROWS) as usize);
            while text.chars().count() < (COLS * ROWS) as usize {
                text.push_str(pattern);
            }
            let ascii: Vec<u32> = text
                .chars()
                .take((COLS * ROWS) as usize)
                .map(|c| c as u32)
                .collect();
            let mut screen = Screen::builder(COLS, ROWS)
                .scrollback(1000)
                .build()
                .unwrap();
            for _ in 0..128 {
                screen.draw_text(&ascii);
            }
            black_box(&screen);
        }
    }
}

fn make_stream(target: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(target + 256);
    let line =
        b"\x1b[38;2;255;128;0mThe quick brown fox jumps over the lazy dog 0123456789\x1b[0m\r\n";
    let mut lines = 0u32;
    while out.len() < target {
        out.extend_from_slice(line);
        if lines.is_multiple_of(64) {
            out.extend_from_slice(b"\x1b[1;1H\x1b[2J");
        }
        lines += 1;
    }
    out
}
