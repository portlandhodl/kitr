// SPDX-License-Identifier: GPL-3.0-or-later
//! Measure end-to-end PTY throughput through the parser and screen.
//!
//! Usage: cargo run --release -p kitty --example bench_pty [LINES]

use std::ffi::OsString;
use std::time::{Duration, Instant};

use kitty::{Input, Options};

fn main() {
    let lines = std::env::args()
        .nth(1)
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(500_000);
    let script = format!("yes 0123456789abcdef | head -n {lines}");
    let options = Options {
        size: Some((120, 40)),
        scrollback: 2_000,
        timeout: Some(Duration::from_secs(60)),
        command: vec![
            OsString::from("/bin/sh"),
            OsString::from("-c"),
            OsString::from(script),
        ],
        ..Options::default()
    };

    let started = Instant::now();
    let outcome = kitty::run_options(&options, Input::None).expect("run benchmark");
    let elapsed = started.elapsed();
    let mib = outcome.output_bytes as f64 / (1024.0 * 1024.0);
    let rate = mib / elapsed.as_secs_f64();
    println!(
        "{} bytes ({:.2} MiB) in {:.3} s: {:.2} MiB/s (exit {:?}, screen {}x{})",
        outcome.output_bytes,
        mib,
        elapsed.as_secs_f64(),
        rate,
        outcome.exit,
        outcome.screen.columns,
        outcome.screen.lines
    );
}
