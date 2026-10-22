// SPDX-License-Identifier: GPL-3.0-or-later
//! Event-loop wakeup latency and EGL swap/readback benchmark.
//!
//! Usage:
//!   cargo run --release -p kitty-window --example bench -- [--iters N] [--width W] [--height H]
//!
//! When `$DISPLAY` is unset the benchmark spawns a private
//! `Xvfb :N -screen 0 800x600x24` instance and kills it on exit. Render
//! throughput is skipped cleanly if EGL cannot create a window surface.

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use kitty_window::{probe_display, EglWindowSurface, WindowConfig, WindowEvent, X11Window};

struct Xvfb {
    name: String,
    child: Option<Child>,
}

impl Xvfb {
    fn start() -> Option<Self> {
        if let Ok(name) = std::env::var("DISPLAY") {
            if !name.is_empty() {
                return Some(Self { name, child: None });
            }
        }
        let pid = std::process::id() as u64;
        for attempt in 0..120u64 {
            let number = 200 + (pid.wrapping_mul(53).wrapping_add(attempt)) % 700;
            let lock = format!("/tmp/.X{number}-lock");
            let socket = format!("/tmp/.X11-unix/X{number}");
            if Path::new(&lock).exists() || Path::new(&socket).exists() {
                continue;
            }
            let name = format!(":{number}");
            let Ok(mut child) = Command::new("Xvfb")
                .arg(&name)
                .args(["-screen", "0", "800x600x24", "-nolisten", "tcp", "-ac"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            else {
                eprintln!("Xvfb is not available");
                return None;
            };
            let deadline = Instant::now() + Duration::from_secs(8);
            while Instant::now() < deadline {
                if let Ok(Some(_)) = child.try_wait() {
                    break;
                }
                if probe_display(&name) {
                    thread::sleep(Duration::from_millis(20));
                    return Some(Self {
                        name,
                        child: Some(child),
                    });
                }
                thread::sleep(Duration::from_millis(20));
            }
            let _ = child.kill();
            let _ = child.wait();
        }
        eprintln!("no free display for Xvfb");
        None
    }
}

impl Drop for Xvfb {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn percentile(sorted: &[Duration], fraction: f64) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    let index = ((sorted.len() - 1) as f64 * fraction).round() as usize;
    sorted[index.min(sorted.len() - 1)]
}

fn main() {
    let mut iters = 200usize;
    let mut width = 640u32;
    let mut height = 480u32;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--iters" => {
                iters = args
                    .next()
                    .expect("--iters needs a number")
                    .parse()
                    .expect("invalid iteration count")
            }
            "--width" => {
                width = args
                    .next()
                    .expect("--width needs a number")
                    .parse()
                    .expect("invalid width")
            }
            "--height" => {
                height = args
                    .next()
                    .expect("--height needs a number")
                    .parse()
                    .expect("invalid height")
            }
            "-h" | "--help" => {
                println!(
                    "usage: bench [--iters N] [--width W] [--height H]\n\
                     Runs under $DISPLAY, or starts a private Xvfb when it is unset."
                );
                return;
            }
            other => panic!("unknown argument: {other}"),
        }
    }

    let Some(xvfb) = Xvfb::start() else {
        return;
    };
    println!("display: {}", xvfb.name);
    let mut window = match X11Window::open(
        &WindowConfig::new("kitty-window bench", width, height),
        Some(&xvfb.name),
    ) {
        Ok(window) => window,
        Err(error) => {
            eprintln!("failed to open benchmark window: {error}");
            return;
        }
    };

    // Drain map/expose/configure events before measuring wakeups.
    while !window.poll_events(20).is_empty() {}

    let (request_tx, request_rx) = mpsc::channel::<()>();
    let handle = window.wakeup_handle();
    let waker = thread::spawn(move || {
        while request_rx.recv().is_ok() {
            thread::sleep(Duration::from_micros(50));
            handle.wakeup();
        }
    });

    let mut latencies = Vec::with_capacity(iters);
    let mut missed = 0usize;
    for _ in 0..iters {
        while !window.poll_events(0).is_empty() {}
        let start = Instant::now();
        request_tx.send(()).expect("waker thread");
        let deadline = start + Duration::from_secs(2);
        let mut latency = None;
        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let slice = remaining.as_millis().clamp(1, 1000) as i32;
            for event in window.poll_events(slice) {
                if matches!(event, WindowEvent::Wakeup) {
                    latency = Some(start.elapsed());
                    break;
                }
            }
            if latency.is_some() {
                break;
            }
        }
        if let Some(latency) = latency {
            latencies.push(latency);
        } else {
            missed += 1;
        }
    }
    drop(request_tx);
    let _ = waker.join();

    latencies.sort_unstable();
    let total: Duration = latencies.iter().sum();
    let mean = if latencies.is_empty() {
        Duration::ZERO
    } else {
        total / latencies.len() as u32
    };
    println!(
        "wakeup latency over {} iterations: mean {:?}, p50 {:?}, p95 {:?}, p99 {:?}, max {:?}, missed {}",
        iters,
        mean,
        percentile(&latencies, 0.50),
        percentile(&latencies, 0.95),
        percentile(&latencies, 0.99),
        percentile(&latencies, 1.0),
        missed,
    );

    let mut surface = match EglWindowSurface::new(&window) {
        Ok(surface) => surface,
        Err(error) => {
            eprintln!("skipping swap/readback benchmark: EGL unavailable: {error}");
            return;
        }
    };
    let _ = surface.set_swap_interval(0);
    println!(
        "EGL {} ({}), GL: {} / {}",
        surface.egl_version_string(),
        surface.egl_vendor().trim(),
        surface.gl_version_string().unwrap_or("<unknown>"),
        surface.gl_renderer_string().unwrap_or("<unknown>").trim(),
    );
    println!("surface: {}x{}", surface.size().0, surface.size().1);

    const WARMUP: usize = 5;
    for _ in 0..WARMUP {
        let _ = surface.clear([0.05, 0.1, 0.2, 1.0]);
        let _ = surface.swap_buffers();
    }

    let start = Instant::now();
    for i in 0..iters {
        let shade = (i % 255) as f32 / 255.0;
        surface
            .clear([shade, 0.2, 1.0 - shade, 1.0])
            .expect("clear");
        surface.swap_buffers().expect("swap");
    }
    let swap_elapsed = start.elapsed();

    let start = Instant::now();
    let mut bytes = 0usize;
    for i in 0..iters {
        let shade = (i % 255) as f32 / 255.0;
        surface
            .clear([shade, 0.2, 1.0 - shade, 1.0])
            .expect("clear");
        bytes += surface.read_pixels().expect("readback").len();
    }
    let readback_elapsed = start.elapsed();

    let swap_rate = iters as f64 / swap_elapsed.as_secs_f64();
    let readback_rate = iters as f64 / readback_elapsed.as_secs_f64();
    let mib = bytes as f64 / (1024.0 * 1024.0);
    println!(
        "swap: {} frames in {:?} ({:.1} fps, {:.3} ms/frame)",
        iters,
        swap_elapsed,
        swap_rate,
        1000.0 * swap_elapsed.as_secs_f64() / iters as f64,
    );
    println!(
        "readback: {} frames / {:.1} MiB in {:?} ({:.1} fps, {:.1} MiB/s)",
        iters,
        mib,
        readback_elapsed,
        readback_rate,
        mib / readback_elapsed.as_secs_f64(),
    );
}
