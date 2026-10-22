// SPDX-License-Identifier: GPL-3.0-or-later
//! Encoding throughput benchmark.
//!
//! Run with: cargo run --release -p kitty-input --example bench

use std::hint::black_box;
use std::time::Instant;

use kitty_input::key_constants::*;
use kitty_input::*;

fn bench(name: &str, iterations: u64, mut f: impl FnMut() -> usize) -> f64 {
    let start = Instant::now();
    let mut bytes = 0u64;
    for _ in 0..iterations {
        bytes += f() as u64;
    }
    let elapsed = start.elapsed();
    let per_second = iterations as f64 / elapsed.as_secs_f64();
    let mib_s = (bytes as f64 / elapsed.as_secs_f64()) / (1024.0 * 1024.0);
    println!(
        "{name:<34} {:>12.0} events/s  {:>8.1} MiB/s  {:>10.2?}",
        per_second, mib_s, elapsed
    );
    black_box(bytes);
    mib_s
}

fn main() {
    let iterations = 5_000_000;

    let plain = KeyEvent::press(b'a' as u32);
    let legacy = KittyKeyboardFlags::NONE;
    bench("legacy printable", iterations, || {
        encode_glfw_key_event(black_box(&plain), false, legacy)
            .into_bytes()
            .map_or(0, |b| b.len())
    });

    let ctrl = KeyEvent::press(b'a' as u32).with_modifiers(Modifiers::CTRL | Modifiers::SHIFT);
    bench("legacy modified", iterations, || {
        encode_glfw_key_event(black_box(&ctrl), false, legacy)
            .into_bytes()
            .map_or(0, |b| b.len())
    });

    let up = KeyEvent::press(GLFW_FKEY_UP);
    bench("legacy arrow", iterations, || {
        encode_glfw_key_event(black_box(&up), false, legacy)
            .into_bytes()
            .map_or(0, |b| b.len())
    });
    bench("application cursor arrow", iterations, || {
        encode_glfw_key_event(black_box(&up), true, legacy)
            .into_bytes()
            .map_or(0, |b| b.len())
    });

    let kitty = KeyEvent::press(b'a' as u32)
        .with_modifiers(Modifiers::CTRL | Modifiers::SHIFT)
        .with_shifted_key(b'A' as u32)
        .with_alternate_key(b'a' as u32)
        .with_text("A");
    let flags = KittyKeyboardFlags::ALL;
    bench("kitty protocol all flags", iterations, || {
        encode_glfw_key_event(black_box(&kitty), false, flags)
            .into_bytes()
            .map_or(0, |b| b.len())
    });

    let function = KeyEvent::press(GLFW_FKEY_F12).with_modifiers(Modifiers::SHIFT);
    bench("kitty protocol F12", iterations, || {
        encode_glfw_key_event(black_box(&function), false, flags)
            .into_bytes()
            .map_or(0, |b| b.len())
    });

    let pos = MousePosition::new(80, 24);
    bench("SGR mouse press", iterations, || {
        encode_mouse_event(
            black_box(&pos),
            MouseTrackingProtocol::Sgr,
            1,
            MouseAction::Press,
            Modifiers::NONE,
        )
        .map_or(0, |b| b.len())
    });
    bench("UTF-8 mouse move", iterations, || {
        encode_mouse_event(
            black_box(&pos),
            MouseTrackingProtocol::Utf8,
            0,
            MouseAction::Move,
            Modifiers::NONE,
        )
        .map_or(0, |b| b.len())
    });
}
