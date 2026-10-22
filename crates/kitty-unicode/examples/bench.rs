// SPDX-License-Identifier: GPL-3.0-or-later
//! Simple throughput benchmark for char_props_for() and wcswidth_std().
//!
//! Run with: cargo run --release -p kitty-unicode --example bench

use std::hint::black_box;
use std::time::{Duration, Instant};

use kitty_core::CharType;
use kitty_unicode::{char_props_for, wcswidth_std};

fn report(label: &str, units: usize, elapsed: Duration, unit_name: &str) {
    let seconds = elapsed.as_secs_f64();
    let per_unit = seconds * 1e9 / units as f64;
    let rate = units as f64 / seconds / 1e6;
    println!(
        "{label:<28} {units:>10} {unit_name}  {per_unit:>8.3} ns/{unit_name}  {rate:>9.2} M{unit_name}/s"
    );
}

fn bench_char_props(codepoints: &[CharType], rounds: usize) {
    let mut checksum = 0u64;
    for &cp in codepoints {
        checksum ^= black_box(char_props_for(black_box(cp)).bits()) as u64;
    }
    black_box(checksum);

    let start = Instant::now();
    let mut checksum = 0u64;
    for _ in 0..rounds {
        for &cp in codepoints {
            checksum ^= black_box(char_props_for(black_box(cp)).bits()) as u64;
        }
    }
    let elapsed = start.elapsed();
    black_box(checksum);
    report("char_props_for", codepoints.len() * rounds, elapsed, "char");
}

fn bench_wcswidth(label: &str, text: &str, rounds: usize) {
    let data: Vec<CharType> = text.chars().map(|c| c as CharType).collect();
    let mut checksum = 0u64;
    for _ in 0..2 {
        checksum = checksum.wrapping_add(wcswidth_std(black_box(&data)) as u64);
    }
    black_box(checksum);

    let start = Instant::now();
    let mut checksum = 0u64;
    for _ in 0..rounds {
        checksum = checksum.wrapping_add(black_box(wcswidth_std(black_box(&data))) as u64);
    }
    let elapsed = start.elapsed();
    black_box(checksum);
    let bytes = text.len() * rounds;
    report(label, data.len() * rounds, elapsed, "char");
    let seconds = elapsed.as_secs_f64();
    println!(
        "{:<28} {:>10} bytes {:>8.3} ns/byte {:>9.2} MiB/s",
        "",
        bytes,
        seconds * 1e9 / bytes as f64,
        bytes as f64 / seconds / (1024.0 * 1024.0)
    );
}

fn main() {
    let codepoints: Vec<CharType> = (0..=0x10ffff).collect();
    bench_char_props(&codepoints, 3);

    let ascii = "The quick brown fox jumps over the lazy dog. 0123456789\n".repeat(64);
    let unicode = "a1\0コニチ ✔ \u{2716}\u{2716}\u{fe0f}\u{1f337} \u{25b6}\u{fe0f} \
        \u{1f610}\u{fe0e} \u{1f1ee}\u{1f1f3} จำ กิ ກຳ a\u{ad}b\n"
        .repeat(64);
    let ansi =
        "normal \u{1b}[31mred\u{1b}[0m \u{1b}]8;id=x;https://example.com\u{1b}\\ link\n".repeat(64);

    bench_wcswidth("wcswidth_std ASCII", &ascii, 2000);
    bench_wcswidth("wcswidth_std Unicode", &unicode, 2000);
    bench_wcswidth("wcswidth_std ANSI", &ansi, 2000);
}
