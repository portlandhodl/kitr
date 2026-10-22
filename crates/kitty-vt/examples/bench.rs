// SPDX-License-Identifier: GPL-3.0-or-later
//! Throughput benchmark for the VT parser.
//!
//! Run with: cargo run --release -p kitty-vt --example bench

use std::hint::black_box;
use std::time::Instant;

use kitty_vt::{Handler, Parser};

#[derive(Default)]
struct CountHandler {
    draws: u64,
    csies: u64,
    oscs: u64,
    chars: u64,
}

impl Handler for CountHandler {
    fn draw(&mut self, text: &[kitty_vt::CharType]) {
        self.draws += 1;
        self.chars += text.len() as u64;
    }
    fn csi_dispatch(&mut self, _params: &kitty_vt::CsiParams, _intermediates: &[u8], _action: u8) {
        self.csies += 1;
    }
    fn osc_dispatch(&mut self, _data: &[u8]) {
        self.oscs += 1;
    }
}

fn make_input(target: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(target + 256);
    let line =
        b"\x1b[38;2;255;128;0mThe quick brown fox jumps over the lazy dog 0123456789\x1b[0m\r\n";
    let title = b"\x1b]2;kitty rust port\x1b\\";
    let mut i = 0u32;
    while out.len() < target {
        out.extend_from_slice(line);
        if i & 0x3f == 0 {
            out.extend_from_slice(title);
        }
        out.extend_from_slice(b"\x1b[1;1H\x1b[2J\x1b[?25h");
        i += 1;
    }
    out
}

fn run(name: &str, input: &[u8]) -> f64 {
    let iterations = 16u32;
    let mut parser = Parser::new();
    let mut handler = CountHandler::default();
    let start = Instant::now();
    for _ in 0..iterations {
        parser.parse(input, &mut handler);
    }
    let elapsed = start.elapsed();
    let bytes = input.len() as u64 * iterations as u64;
    let mib_s = bytes as f64 / elapsed.as_secs_f64() / (1024.0 * 1024.0);
    println!(
        "{name:<34} {:>8.1} MiB/s  {:>10.2?}  draws={} chars={} csi={} osc={}",
        mib_s,
        elapsed,
        black_box(handler.draws),
        black_box(handler.chars),
        black_box(handler.csies),
        black_box(handler.oscs)
    );
    mib_s
}

fn main() {
    let input = make_input(64 * 1024 * 1024);
    println!(
        "input: {} MiB, one parse call per iteration",
        input.len() / (1024 * 1024)
    );
    run("mixed xterm stream", &input);

    let ascii: Vec<u8> = (0..16 * 1024 * 1024u32)
        .map(|i| b' ' + (i % 95) as u8)
        .collect();
    run("printable ascii", &ascii);

    let mut parser = Parser::new();
    let mut handler = CountHandler::default();
    let small = 4096usize;
    let start = Instant::now();
    for chunk in input.chunks(small) {
        parser.parse(chunk, &mut handler);
    }
    let elapsed = start.elapsed();
    println!(
        "{:<34} {:>8.1} MiB/s  {:>10.2?}  ({} byte writes)",
        "mixed stream, 4 KiB incremental writes",
        input.len() as f64 / elapsed.as_secs_f64() / (1024.0 * 1024.0),
        elapsed,
        input.len() / small
    );
}
