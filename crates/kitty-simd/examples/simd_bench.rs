// SPDX-License-Identifier: GPL-3.0-or-later
//! Throughput benchmark for the main primitives, one row per usable SIMD level.
//!
//! Run with: cargo run --release -p kitty-simd --example simd_bench

use kitty_simd::*;
use std::hint::black_box;
use std::time::Instant;

fn bench<F: FnMut()>(bytes_per_iter: usize, mut f: F) -> f64 {
    let target = 96 * 1024 * 1024usize;
    let iters = (target / bytes_per_iter.max(1)).max(8);
    // Warm up.
    for _ in 0..(iters / 8).max(1) {
        f();
    }
    let start = Instant::now();
    for _ in 0..iters {
        f();
    }
    let elapsed = start.elapsed();
    (bytes_per_iter as f64 * iters as f64) / elapsed.as_secs_f64() / 1e9
}

fn run_c_utf8_only(unit: &str, noascii: bool) {
    const UN: usize = 4 * 1024 * 1024;
    let ascii_bytes: Vec<u8> = (0..UN).map(|i| 32 + (i % 95) as u8).collect();
    let mut utf8_bytes = Vec::with_capacity(UN);
    while utf8_bytes.len() < UN {
        let n = unit.len().min(UN - utf8_bytes.len());
        utf8_bytes.extend_from_slice(&unit.as_bytes()[..n]);
    }
    for level in [0u32, 128, 256] {
        if !level_supported(level) {
            continue;
        }
        with_forced_level(level, || {
            let tag = if level == 0 { "scalar" } else { "simd" };
            let mut d = UTF8Decoder::new();
            let iters = 20u64;
            if !noascii {
                let start = Instant::now();
                for _ in 0..iters {
                    d.reset();
                    d.decode_to_esc(black_box(&ascii_bytes));
                    black_box(d.decoded().len());
                }
                let dt = start.elapsed().as_secs_f64();
                println!(
                    "simd_utf8_ascii_{tag}_{level}_MiB_s\t{:.2}",
                    UN as f64 * 20.0 / dt / (1024.0 * 1024.0)
                );
            }
            let start = Instant::now();
            for _ in 0..iters {
                d.reset();
                d.decode_to_esc(black_box(&utf8_bytes));
                black_box(d.decoded().len());
            }
            let dt = start.elapsed().as_secs_f64();
            println!(
                "simd_utf8_unicode_{tag}_{level}_MiB_s\t{:.2}",
                UN as f64 * 20.0 / dt / (1024.0 * 1024.0)
            );
        });
    }
}

fn run_c_harness() {
    // Exactly the buffers, sizes and iteration counts of
    // rust/benchmarks/bench_simd.c, with the same output names.
    const N: usize = 16 * 1024 * 1024;
    const UN: usize = 4 * 1024 * 1024;
    let mut haystack = vec![b'a'; N];
    haystack[N - 1] = b'<';
    let mut xdata = vec![0x5au8; N];
    let key = [0x42u8; 64];
    let chars: Vec<u32> = (0..N).map(|i| 32 + (i % 95) as u32).collect();
    let ascii_bytes: Vec<u8> = (0..UN).map(|i| 32 + (i % 95) as u8).collect();
    let unit = "a界😀e\u{301}ก\u{0e33}";
    let mut utf8_bytes = Vec::with_capacity(UN);
    while utf8_bytes.len() < UN {
        let n = unit.len().min(UN - utf8_bytes.len());
        utf8_bytes.extend_from_slice(&unit.as_bytes()[..n]);
    }

    let levels = [0u32, 128, 256];
    for level in levels {
        if !level_supported(level) {
            continue;
        }
        with_forced_level(level, || {
            let tag = if level == 0 { "scalar" } else { "simd" };
            let mut sink = 0u64;

            let iters = 8u64;
            let start = Instant::now();
            for _ in 0..iters {
                sink = sink.wrapping_add(
                    find_either_of_two_bytes(black_box(&haystack), b'<', b'>').unwrap_or(0) as u64,
                );
            }
            let dt = start.elapsed().as_secs_f64();
            println!(
                "simd_find_{tag}_{level}_GiB_s\t{:.2}",
                N as f64 * 8.0 / dt / (1024.0 * 1024.0 * 1024.0)
            );

            let start = Instant::now();
            for _ in 0..iters {
                sink = sink.wrapping_add(printable_ascii_run_length(black_box(&chars)) as u64);
            }
            let dt = start.elapsed().as_secs_f64();
            println!(
                "simd_printable_{tag}_{level}_GiB_s\t{:.2}",
                N as f64 * 4.0 * 8.0 / dt / (1024.0 * 1024.0 * 1024.0)
            );

            let start = Instant::now();
            for _ in 0..iters {
                xor_data64(&key, black_box(&mut xdata));
            }
            let dt = start.elapsed().as_secs_f64();
            println!(
                "simd_xor_{tag}_{level}_GiB_s\t{:.2}",
                N as f64 * 8.0 / dt / (1024.0 * 1024.0 * 1024.0)
            );

            let start = Instant::now();
            let mut d = UTF8Decoder::new();
            let ascii_iters = 2u64;
            for _ in 0..ascii_iters {
                d.reset();
                d.decode_to_esc(black_box(&ascii_bytes));
                sink = sink.wrapping_add(black_box(d.decoded().len()) as u64);
            }
            let dt = start.elapsed().as_secs_f64();
            println!(
                "simd_utf8_ascii_{tag}_{level}_MiB_s\t{:.2}",
                UN as f64 * 2.0 / dt / (1024.0 * 1024.0)
            );

            let start = Instant::now();
            for _ in 0..2u64 {
                d.reset();
                d.decode_to_esc(black_box(&utf8_bytes));
                sink = sink.wrapping_add(black_box(d.decoded().len()) as u64);
            }
            let dt = start.elapsed().as_secs_f64();
            println!(
                "simd_utf8_unicode_{tag}_{level}_MiB_s\t{:.2}",
                UN as f64 * 2.0 / dt / (1024.0 * 1024.0)
            );
            black_box(sink);
        });
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--c-harness") {
        run_c_harness();
        return;
    }
    if args.iter().any(|a| a == "--c-utf8") {
        let unit = args
            .iter()
            .position(|a| a == "--unit")
            .and_then(|i| args.get(i + 1))
            .map(String::as_str)
            .unwrap_or("a界😀e\u{301}ก\u{0e33}");
        let noascii = args.iter().any(|a| a == "--noascii");
        run_c_utf8_only(unit, noascii);
        return;
    }
    let levels: Vec<u32> = [0, 128, 256, 512]
        .into_iter()
        .filter(|&l| level_supported(l))
        .collect();
    println!(
        "kitty-simd benchmark: detected level {}, testing {:?}",
        max_simd_level(),
        levels
    );
    println!("{:>5} {:>28} {:>10}", "level", "primitive", "GB/s");

    let mut find_data = vec![b'x'; 1 << 20];
    let last = find_data.len() - 3;
    find_data[last] = b'\x1b';
    let key: [u8; 64] = core::array::from_fn(|i| i as u8);
    let mut xor_data = vec![0x5au8; 1 << 20];
    let mut printable = vec![b'a' as u32; 1 << 18];
    *printable.last_mut().unwrap() = 0x4e2d;
    let num_pixels = 1 << 17;
    let src: Vec<u8> = (0..4 * num_pixels).map(|i| (i * 31) as u8).collect();
    let mut dst4: Vec<u8> = (0..4 * num_pixels).map(|i| (i * 17) as u8).collect();
    let mut dst4_opaque = dst4.clone();
    let mut dst3: Vec<u8> = (0..3 * num_pixels).map(|i| (i * 13) as u8).collect();
    let mut mask_pixels: Vec<u32> = (0..num_pixels).map(|i| (i * 7) as u32).collect();
    let mask: Vec<u8> = (0..num_pixels).map(|i| (i * 11) as u8).collect();
    let ascii = vec![b'a'; 1 << 20];
    let unicode: Vec<u8> = "α≤\u{1f638}a".repeat(1 << 15).into_bytes();

    for &level in &levels {
        with_forced_level(level, || {
            let gb = bench(find_data.len(), || {
                black_box(find_either_of_two_bytes(black_box(&find_data), 0x1b, b'z'));
            });
            println!("{level:>5} {:>28} {gb:>10.2}", "find_either_of_two_bytes");

            let gb = bench(xor_data.len(), || {
                xor_data64(&key, black_box(&mut xor_data));
            });
            println!("{level:>5} {:>28} {gb:>10.2}", "xor_data64");
            black_box(&xor_data);

            let gb = bench(printable.len() * 4, || {
                black_box(printable_ascii_run_length(black_box(&printable)));
            });
            println!("{level:>5} {:>28} {gb:>10.2}", "printable_ascii_run_length");

            let gb = bench(src.len(), || {
                blend_over_straight(black_box(&mut dst4), black_box(&src));
            });
            println!("{level:>5} {:>28} {gb:>10.2}", "blend_over_straight");
            black_box(&dst4);

            let gb = bench(src.len(), || {
                blend_over_opaque(black_box(&mut dst4_opaque), 4, black_box(&src));
            });
            println!("{level:>5} {:>28} {gb:>10.2}", "blend_over_opaque(4bpp)");
            black_box(&dst4_opaque);

            let gb = bench(num_pixels * 3, || {
                blend_over_opaque(black_box(&mut dst3), 3, black_box(&src));
            });
            println!("{level:>5} {:>28} {gb:>10.2}", "blend_over_opaque(3bpp)");
            black_box(&dst3);

            let gb = bench(num_pixels * 4, || {
                composite_alpha_mask(black_box(&mut mask_pixels), black_box(&mask), 0x123456);
            });
            println!("{level:>5} {:>28} {gb:>10.2}", "composite_alpha_mask");
            black_box(&mask_pixels);

            let gb = bench(ascii.len(), || {
                let mut d = UTF8Decoder::new();
                black_box(d.decode_to_esc(black_box(&ascii)));
            });
            println!("{level:>5} {:>28} {gb:>10.2}", "utf8_decode_to_esc(ascii)");

            let gb = bench(unicode.len(), || {
                let mut d = UTF8Decoder::new();
                black_box(d.decode_to_esc(black_box(&unicode)));
            });
            println!(
                "{level:>5} {:>28} {gb:>10.2}",
                "utf8_decode_to_esc(unicode)"
            );
        });
    }
}
