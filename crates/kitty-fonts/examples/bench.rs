// SPDX-License-Identifier: GPL-3.0-or-later
//! Micro-benchmark for glyph lookup, HarfBuzz shaping and rasterization.
//!
//! Usage:
//!   cargo run --release -p kitty-fonts --example bench -- [--mode all|glyph|shape|raster] [--font PATH] [--iters N]
//!
//! By default it uses the bundled Fira Code face and 200,000 iterations.

use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::time::Instant;

use kitty_fonts::{Face, RasterCache, ShapeOptions, ShapedRunCache};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    All,
    Glyph,
    Shape,
    Raster,
}

fn default_font() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../kitty_tests/FiraCode-Medium.otf")
}

fn main() {
    let mut font = default_font();
    let mut iters = 200_000usize;
    let mut mode = Mode::All;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--font" => font = PathBuf::from(args.next().expect("--font needs a path")),
            "--iters" => {
                iters = args
                    .next()
                    .expect("--iters needs a number")
                    .parse()
                    .expect("invalid iteration count")
            }
            "--mode" => {
                mode = match args.next().expect("--mode needs a value").as_str() {
                    "all" => Mode::All,
                    "glyph" => Mode::Glyph,
                    "shape" => Mode::Shape,
                    "raster" => Mode::Raster,
                    other => panic!("unknown mode: {other}"),
                }
            }
            "-h" | "--help" => {
                println!("usage: bench [--mode all|glyph|shape|raster] [--font PATH] [--iters N]");
                return;
            }
            other => panic!("unknown argument: {other}"),
        }
    }

    let face = Face::from_path(&font, 0).expect("failed to load font");
    let metrics = face.cell_metrics();
    println!(
        "font: {} ({})  size: {}pt @ {}dpi  cell: {}x{}  baseline: {}",
        face.family_name(),
        font.display(),
        face.point_size(),
        face.dpi().0,
        metrics.cell_width,
        metrics.cell_height,
        metrics.baseline,
    );

    if matches!(mode, Mode::All | Mode::Glyph) {
        bench_glyph_lookup(&face, iters);
    }
    if matches!(mode, Mode::All | Mode::Shape) {
        bench_shaping(&face, iters);
    }
    if matches!(mode, Mode::All | Mode::Raster) {
        bench_raster(&face, iters);
    }
}

fn bench_glyph_lookup(face: &Face, iters: usize) {
    let lookup_text: Vec<u32> = "The quick brown fox 0123456789 -> ==> != 你好 😀"
        .chars()
        .map(|c| c as u32)
        .collect();

    let start = Instant::now();
    let mut found = 0u32;
    for _ in 0..iters {
        for &cp in &lookup_text {
            found = found.wrapping_add(black_box(face.glyph_id(cp)));
        }
    }
    let elapsed = start.elapsed();
    let lookups = iters * lookup_text.len();
    println!(
        "glyph lookup: {} lookups in {:?}  {:.1} ns/lookup  {:.2} Mlookups/s  (checksum {found})",
        lookups,
        elapsed,
        elapsed.as_nanos() as f64 / lookups as f64,
        lookups as f64 / elapsed.as_secs_f64() / 1e6,
    );
}

fn bench_shaping(face: &Face, iters: usize) {
    let run: Vec<u32> = "a->b ==> if (x != y) { return 0; } 你好 😀"
        .chars()
        .map(|c| c as u32)
        .collect();
    let options = ShapeOptions::default();
    let shape_iters = (iters / 20).max(1_000);
    let start = Instant::now();
    let mut glyphs = 0usize;
    for _ in 0..shape_iters {
        let shaped = face
            .shape(black_box(&run), &options)
            .expect("shaping failed");
        glyphs += black_box(shaped.glyphs.len());
    }
    let elapsed = start.elapsed();
    println!(
        "uncached shaping: {} runs ({} codepoints) in {:?}  {:.2} us/run  {:.2} Mcodepoints/s  ({} glyphs)",
        shape_iters,
        run.len(),
        elapsed,
        elapsed.as_nanos() as f64 / shape_iters as f64 / 1e3,
        (shape_iters * run.len()) as f64 / elapsed.as_secs_f64() / 1e6,
        glyphs,
    );

    // Shaped-run cache entries are limited to 32 codepoints, like kitty.
    let cached_run: Vec<u32> = "a->b ==> if (x != y) 你好 😀"
        .chars()
        .map(|c| c as u32)
        .collect();
    let mut cache = ShapedRunCache::new();
    let _ = cache
        .shape(face, &cached_run, &options)
        .expect("shaping failed");
    let start = Instant::now();
    let mut cached_glyphs = 0usize;
    for _ in 0..iters {
        let shaped = cache
            .shape(face, black_box(&cached_run), &options)
            .expect("shaping failed");
        cached_glyphs += black_box(shaped.glyphs.len());
    }
    let elapsed = start.elapsed();
    println!(
        "cached shaping: {} runs in {:?}  {:.1} ns/run  {:.2} Mruns/s  ({} glyphs, {} hits, {} misses)",
        iters,
        elapsed,
        elapsed.as_nanos() as f64 / iters as f64,
        iters as f64 / elapsed.as_secs_f64() / 1e6,
        cached_glyphs,
        cache.hits(),
        cache.misses(),
    );
}

fn bench_raster(face: &Face, iters: usize) {
    let glyphs: Vec<u32> = "AgijmMW_|"
        .chars()
        .map(|c| face.glyph_id(c as u32))
        .collect();
    let raster_iters = (iters / 20).max(1_000);
    let start = Instant::now();
    let mut pixels = 0u64;
    let mut bytes = 0u64;
    for _ in 0..raster_iters {
        for &gid in &glyphs {
            let bitmap = face
                .rasterize_glyph(black_box(gid))
                .expect("rasterization failed");
            pixels += black_box(bitmap.width as u64 * bitmap.height as u64);
            bytes += black_box(bitmap.data.len() as u64);
        }
    }
    let elapsed = start.elapsed();
    let count = raster_iters * glyphs.len();
    println!(
        "uncached raster: {} glyphs in {:?}  {:.2} us/glyph  {:.2} Kglyphs/s  ({:.1} Mpixels/s, {} MiB)",
        count,
        elapsed,
        elapsed.as_nanos() as f64 / count as f64 / 1e3,
        count as f64 / elapsed.as_secs_f64() / 1e3,
        pixels as f64 / elapsed.as_secs_f64() / 1e6,
        bytes / (1024 * 1024),
    );

    let mut cache = RasterCache::new();
    for &gid in &glyphs {
        let _ = cache.rasterize(face, gid).expect("rasterization failed");
    }
    let start = Instant::now();
    let mut cached_pixels = 0u64;
    for _ in 0..iters {
        for &gid in &glyphs {
            let bitmap = cache
                .rasterize(face, black_box(gid))
                .expect("rasterization failed");
            cached_pixels += black_box(bitmap.width as u64 * bitmap.height as u64);
        }
    }
    let elapsed = start.elapsed();
    let count = iters * glyphs.len();
    println!(
        "cached raster: {} glyphs in {:?}  {:.1} ns/glyph  {:.2} Mghlyphs/s  ({:.1} Mpixels/s, {} hits, {} misses)",
        count,
        elapsed,
        elapsed.as_nanos() as f64 / count as f64,
        count as f64 / elapsed.as_secs_f64() / 1e6,
        cached_pixels as f64 / elapsed.as_secs_f64() / 1e6,
        cache.hits(),
        cache.misses(),
    );
}
