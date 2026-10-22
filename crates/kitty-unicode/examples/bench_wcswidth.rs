// SPDX-License-Identifier: GPL-3.0-or-later
//! wcswidth workloads matching benchmarks/bench_kitty.py exactly.
//!
//! Run with:
//!   taskset -c 3 cargo run --release -p kitty-unicode --example bench_wcswidth

use std::hint::black_box;
use std::time::Instant;

use kitty_core::CharType;
use kitty_unicode::wcswidth_std;

fn timed(name: &str, work: f64, iters: u32, mut f: impl FnMut() -> u32) {
    let _ = black_box(f());
    let start = Instant::now();
    for _ in 0..iters {
        let _ = black_box(f());
    }
    let rate = work * iters as f64 / start.elapsed().as_secs_f64();
    println!("{name}\t{rate:.2}");
}

fn main() {
    let ascii_str = "The quick brown fox 0123456789 "
        .chars()
        .cycle()
        .take(1_000_000)
        .map(|c| c as CharType)
        .collect::<Vec<_>>();
    let unicode_str = "a界😀e\u{301}ก\u{0e33}"
        .chars()
        .cycle()
        .take(1_000_000)
        .map(|c| c as CharType)
        .collect::<Vec<_>>();
    // Python: ('a\x1b[31mred\x1b[0m界\n' * (1_000_000 // 16 + 1))[:1_000_000]
    // The pattern is 15 codepoints, so this is 937_515 chars, not 1_000_000.
    let ansi_str: Vec<CharType> = "a\x1b[31mred\x1b[0m界\n"
        .repeat(1_000_000 / 16 + 1)
        .chars()
        .take(1_000_000)
        .map(|c| c as CharType)
        .collect();

    for (name, value) in [
        ("wcswidth_ascii_Mchars_s", &ascii_str),
        ("wcswidth_unicode_Mchars_s", &unicode_str),
        ("wcswidth_ansi_Mchars_s", &ansi_str),
    ] {
        timed(name, value.len() as f64 / 1e6, 10, || wcswidth_std(value));
    }
}
