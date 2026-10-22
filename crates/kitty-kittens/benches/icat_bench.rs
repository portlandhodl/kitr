// SPDX-License-Identifier: GPL-3.0-or-later
//! `icat` encoding throughput benchmark.
//!
//! Run with `cargo bench -p kitty-kittens --bench icat`. No external
//! benchmarking crate is used.

use std::hint::black_box;
use std::time::Instant;

use kitty_graphics::png::encode_png_rgba;
use kitty_kittens::icat::{encode_png, IcatOptions, OutputFormat};

fn fixture(width: u32, height: u32) -> Vec<u8> {
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            rgba.push((x & 0xff) as u8);
            rgba.push((y & 0xff) as u8);
            rgba.push(((x ^ y) & 0xff) as u8);
            rgba.push(255);
        }
    }
    encode_png_rgba(width, height, &rgba).unwrap()
}

fn report(label: &str, bytes: usize, rounds: usize, elapsed_secs: f64) {
    let total_mb = (bytes as f64 * rounds as f64) / (1024.0 * 1024.0);
    println!(
        "{label}: {rounds} iterations, {total_mb:.1} MiB in {elapsed_secs:.4}s = {:.1} MiB/s",
        total_mb / elapsed_secs
    );
}

fn main() {
    let png = fixture(512, 512);
    let png_options = IcatOptions::default();
    let rounds = 200;
    let start = Instant::now();
    for _ in 0..rounds {
        black_box(encode_png(black_box(&png), black_box(&png_options)).unwrap());
    }
    report(
        "PNG passthrough",
        png.len(),
        rounds,
        start.elapsed().as_secs_f64(),
    );

    let raw_options = IcatOptions {
        format: OutputFormat::Rgba,
        chunk_size: 64 * 1024,
        ..IcatOptions::default()
    };
    let rounds = 200;
    let start = Instant::now();
    for _ in 0..rounds {
        black_box(encode_png(black_box(&png), black_box(&raw_options)).unwrap());
    }
    report(
        "decode + RGBA + chunking",
        png.len(),
        rounds,
        start.elapsed().as_secs_f64(),
    );

    let scaled_options = IcatOptions {
        scale: Some(kitty_kittens::icat::Scale::Exact {
            width: 256,
            height: 256,
        }),
        ..IcatOptions::default()
    };
    let rounds = 100;
    let start = Instant::now();
    for _ in 0..rounds {
        black_box(encode_png(black_box(&png), black_box(&scaled_options)).unwrap());
    }
    report(
        "decode + nearest-neighbour resize + PNG",
        png.len(),
        rounds,
        start.elapsed().as_secs_f64(),
    );
}
