// SPDX-License-Identifier: GPL-3.0-or-later
//! Workloads mirrored by kitty/bench_c.py so the two implementations can be
//! compared directly. Run with:
//!   cargo run --release -p kitty-screen --example compare

use std::hint::black_box;
use std::time::{Duration, Instant};

use kitty_screen::{Screen, ScreenHandler};
use kitty_simd as simd;
use kitty_simd::UTF8Decoder;
use kitty_unicode::wcswidth_std;
use kitty_vt::Parser;

fn bench<F: FnMut()>(iters: u32, mut f: F) -> Duration {
    for _ in 0..(iters / 10).max(1) {
        f();
    }
    let start = Instant::now();
    for _ in 0..iters {
        f();
    }
    start.elapsed()
}

fn print(name: &str, work: f64, elapsed: Duration) {
    let rate = work / elapsed.as_secs_f64();
    println!("{name}\t{rate:.2}");
}

fn main() {
    const COLS: u32 = 120;
    const ROWS: u32 = 40;

    // Screen draw, same text fill as bench_c.py.
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
    let iters = 5_000u32;
    let elapsed = bench(iters, || {
        screen.draw_text(&ascii);
    });
    print(
        "screen_draw_ascii_cells_s",
        ascii.len() as f64 * iters as f64,
        elapsed,
    );

    let mut screen = Screen::builder(COLS, ROWS)
        .scrollback(1000)
        .build()
        .unwrap();
    let wide: Vec<u32> = std::iter::repeat_n('界' as u32, (COLS * ROWS / 2) as usize).collect();
    let wide_iters = 2_000u32;
    let elapsed = bench(wide_iters, || {
        screen.draw_text(&wide);
    });
    print(
        "screen_draw_wide_cells_s",
        (wide.len() * 2) as f64 * wide_iters as f64,
        elapsed,
    );

    // Parser + screen dispatch on the same mixed stream used by kitty/bench_c.py.
    let stream = make_stream(16 * 1024 * 1024);
    let mut handler = ScreenHandler::new(
        Screen::builder(COLS, ROWS)
            .scrollback(1000)
            .build()
            .unwrap(),
    );
    let mut parser = Parser::new();
    let iterations = 4u32;
    let start = Instant::now();
    for _ in 0..iterations {
        handler.feed(&mut parser, &stream);
    }
    let elapsed = start.elapsed();
    print(
        "parser_screen_MiB_s",
        stream.len() as f64 * iterations as f64 / (1024.0 * 1024.0),
        elapsed,
    );
    println!("parser_screen_errors\t{}", handler.errors.len());
    black_box(&handler.screen);

    // ANSI aware wcswidth.
    let ascii_str: Vec<u32> = "The quick brown fox 0123456789 "
        .chars()
        .cycle()
        .take(1_000_000)
        .map(|c| c as u32)
        .collect();
    let unicode_str: Vec<u32> = "a界😀e\u{301}ก\u{4e33}"
        .chars()
        .cycle()
        .take(1_000_000)
        .map(|c| c as u32)
        .collect();
    let ansi_str: Vec<u32> = "a\x1b[31mred\x1b[0m界\n"
        .chars()
        .cycle()
        .take(1_000_000)
        .map(|c| c as u32)
        .collect();
    for (name, s) in [
        ("wcswidth_ascii_Mchars_s", &ascii_str),
        ("wcswidth_unicode_Mchars_s", &unicode_str),
        ("wcswidth_ansi_Mchars_s", &ansi_str),
    ] {
        let iters = 10u32;
        let start = Instant::now();
        let mut total = 0u64;
        for _ in 0..iters {
            total = total.wrapping_add(wcswidth_std(s) as u64);
        }
        print(name, s.len() as f64 * iters as f64 / 1e6, start.elapsed());
        black_box(total);
    }

    // SIMD primitives. C counterparts copy the buffer per call; sizes are large
    // enough that the copy is a constant factor, noted in the report.
    let n = 16 * 1024 * 1024usize;
    let mut haystack = vec![b'a'; n];
    haystack[n - 1] = b'<';
    let mut data = vec![0x5au8; n];
    let key = [0x42u8; 64];
    let chars: Vec<u32> = (0..n).map(|i| b' ' as u32 + (i % 95) as u32).collect();
    let ascii_bytes: Vec<u8> = (0..4 * 1024 * 1024)
        .map(|i| b' ' + (i % 95) as u8)
        .collect();
    let unicode_text = "a界😀e\u{301}ก\u{4e33}";
    let mut utf8_bytes = Vec::with_capacity(4 * 1024 * 1024);
    while utf8_bytes.len() < 4 * 1024 * 1024 {
        utf8_bytes.extend_from_slice(unicode_text.as_bytes());
    }

    let levels = {
        let mut v = vec![0u32];
        for l in [128u32, 256, 512] {
            if simd::level_supported(l) {
                v.push(l);
            }
        }
        v
    };
    for level in levels {
        let lname = if level == 0 { "scalar" } else { "simd" };
        let elapsed = simd::with_forced_level(level, || {
            let iters = 8u32;
            let start = Instant::now();
            for _ in 0..iters {
                black_box(simd::find_either_of_two_bytes(&haystack, b'<', b'>'));
            }
            start.elapsed()
        });
        println!(
            "simd_find_{lname}_{level}_GiB_s\t{:.2}",
            n as f64 * 8.0 / elapsed.as_secs_f64() / (1024.0 * 1024.0 * 1024.0)
        );

        let elapsed = simd::with_forced_level(level, || {
            let iters = 8u32;
            let start = Instant::now();
            for _ in 0..iters {
                black_box(simd::printable_ascii_run_length(&chars));
            }
            start.elapsed()
        });
        println!(
            "simd_printable_{lname}_{level}_GiB_s\t{:.2}",
            n as f64 * 4.0 * 8.0 / elapsed.as_secs_f64() / (1024.0 * 1024.0 * 1024.0)
        );

        let elapsed = simd::with_forced_level(level, || {
            let iters = 8u32;
            let start = Instant::now();
            for _ in 0..iters {
                simd::xor_data64(&key, &mut data);
            }
            start.elapsed()
        });
        println!(
            "simd_xor_{lname}_{level}_GiB_s\t{:.2}",
            n as f64 * 8.0 / elapsed.as_secs_f64() / (1024.0 * 1024.0 * 1024.0)
        );

        let elapsed = simd::with_forced_level(level, || {
            let iters = 4u32;
            let mut d = UTF8Decoder::new();
            let start = Instant::now();
            for _ in 0..iters {
                d.reset();
                d.decode_to_esc(&ascii_bytes);
                black_box(d.decoded().len());
            }
            start.elapsed()
        });
        println!(
            "simd_utf8_ascii_{lname}_{level}_MiB_s\t{:.2}",
            ascii_bytes.len() as f64 * 4.0 / elapsed.as_secs_f64() / (1024.0 * 1024.0)
        );

        let elapsed = simd::with_forced_level(level, || {
            let iters = 2u32;
            let mut d = UTF8Decoder::new();
            let start = Instant::now();
            for _ in 0..iters {
                d.reset();
                d.decode_to_esc(&utf8_bytes);
                black_box(d.decoded().len());
            }
            start.elapsed()
        });
        println!(
            "simd_utf8_unicode_{lname}_{level}_MiB_s\t{:.2}",
            utf8_bytes.len() as f64 * 2.0 / elapsed.as_secs_f64() / (1024.0 * 1024.0)
        );
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
