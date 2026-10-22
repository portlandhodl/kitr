// SPDX-License-Identifier: GPL-3.0-or-later
//! Dependency free microbenchmarks for the Phase 0 core structures.
//!
//! Run with: cargo run --release -p kitty-core --example bench

use std::cell::RefCell;
use std::hint::black_box;
use std::rc::Rc;
use std::time::Instant;

use kitty_core::{CpuCell, HistoryBuf, Line, LineBuf, TextCache};

fn timed<F: FnOnce() -> u64>(name: &str, bytes: u64, f: F) {
    let start = Instant::now();
    let ops = f();
    let elapsed = start.elapsed();
    let ops_s = ops as f64 / elapsed.as_secs_f64();
    let mib_s = bytes as f64 / elapsed.as_secs_f64() / (1024.0 * 1024.0);
    println!("{name:<38} {ops:>12} ops  {elapsed:>10.2?}  {ops_s:>14.0} ops/s  {mib_s:>8.1} MiB/s");
}

fn bench_linebuf_scroll() {
    let columns = 200u32;
    let lines = 100u32;
    let tc = Rc::new(RefCell::new(TextCache::new()));
    let mut lb = LineBuf::new(lines, columns, tc).unwrap();
    for y in 0..lines as usize {
        for (x, cell) in lb.cpu_cells_for_line_mut(y).iter_mut().enumerate() {
            cell.set_char((b'a' + (x % 26) as u8) as u32);
        }
    }
    let iterations = 10_000_000u64;
    let bytes = iterations * (lines as u64) * 16;
    timed("linebuf index/reverse_index", bytes, || {
        for i in 0..iterations {
            if i & 1 == 0 {
                lb.index(0, lines as usize - 1);
            } else {
                lb.reverse_index(0, lines as usize - 1);
            }
        }
        black_box(lb.cpu_cell_at(0, 0));
        iterations
    });
}

fn bench_linebuf_clear() {
    let columns = 200u32;
    let lines = 100u32;
    let tc = Rc::new(RefCell::new(TextCache::new()));
    let mut lb = LineBuf::new(lines, columns, tc).unwrap();
    let iterations = 2_000_000u64;
    let bytes = iterations * (lines as u64) * columns as u64 * 32;
    timed("linebuf clear line", bytes, || {
        for i in 0..iterations {
            lb.clear_line((i % lines as u64) as usize, i & 1 == 0);
        }
        black_box(lb.cpu_cell_at(0, 0));
        iterations
    });
}

fn bench_history_push() {
    let columns = 200u32;
    let lines = 2_000u32;
    let tc = Rc::new(RefCell::new(TextCache::new()));
    let mut hb = HistoryBuf::new(columns, lines, tc.clone()).unwrap();
    let mut line = Line::new(columns as usize);
    for cell in line.cpu_cells.iter_mut() {
        cell.set_char('x' as u32);
    }
    let iterations = 2_000_000u64;
    let bytes = iterations * columns as u64 * 32;
    timed("history add_line (200 cols)", bytes, || {
        for i in 0..iterations {
            line.cpu_cells[(i as usize) % columns as usize]
                .set_char((b'a' + (i % 26) as u8) as u32);
            hb.add_line(&line);
        }
        black_box(&hb);
        black_box(hb.count);
        iterations
    });
}

fn bench_text_cache() {
    let mut tc = TextCache::new();
    let mut alphabet = vec![0u32; 256];
    for (i, v) in alphabet.iter_mut().enumerate() {
        *v = 0x1f300 + i as u32;
    }
    let iterations = 5_000_000u64;
    timed("text-cache insert (3 cp, unique)", iterations * 12, || {
        let mut x = 12345u64;
        for _ in 0..iterations {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
            let text = [
                alphabet[(x & 0xff) as usize],
                alphabet[((x >> 8) & 0xff) as usize],
                alphabet[((x >> 16) & 0xff) as usize],
            ];
            black_box(tc.get_or_insert(&text));
        }
        iterations
    });
    let mut hits = TextCache::new();
    for a in 0..4u32 {
        for b in 0..4u32 {
            for c in 0..4u32 {
                hits.get_or_insert(&[a, b, c]);
            }
        }
    }
    timed("text-cache hit (3 cp, 64 keys)", iterations * 12, || {
        let mut x = 999u64;
        for _ in 0..iterations {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
            let text = [(x & 3) as u32, ((x >> 2) & 3) as u32, ((x >> 4) & 3) as u32];
            black_box(hits.get_or_insert(&text));
        }
        iterations
    });
}

fn bench_cell_layout() {
    let cells: Vec<CpuCell> = (0..10_000_000u32)
        .map(|i| {
            let mut c = CpuCell::new();
            c.set_char(i % 0x10ffff);
            c.set_width(1);
            c
        })
        .collect();
    let bytes = cells.len() as u64 * std::mem::size_of::<CpuCell>() as u64;
    timed("cell construction (12 bytes)", bytes, || {
        let sum: u32 = cells.iter().map(|c| c.ch()).sum();
        black_box(sum);
        cells.len() as u64
    });
}

fn main() {
    println!(
        "cell={} gpu={} line_attrs={}",
        std::mem::size_of::<CpuCell>(),
        std::mem::size_of::<kitty_core::GpuCell>(),
        std::mem::size_of::<kitty_core::LineAttrs>()
    );
    bench_cell_layout();
    bench_linebuf_scroll();
    bench_linebuf_clear();
    bench_text_cache();
    bench_history_push();
}
