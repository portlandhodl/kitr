// SPDX-License-Identifier: GPL-3.0-or-later
//! GUI event-loop throughput under X11/EGL.
//!
//! Usage:
//!   xvfb-run -a cargo run -p kitty --release --example bench_gui -- [seconds]
//!
//! When `$DISPLAY` is unset this example starts its own Xvfb and kills it on
//! exit. Output throughput includes PTY read + VT parse + raster + GL draw +
//! swap, i.e. the end-to-end GUI loop.

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use kitty::gui::GuiSession;
use kitty::{GuiMode, Input, Options};
use kitty_window::probe_display;

struct Display {
    name: String,
    child: Option<Child>,
}

impl Display {
    fn new() -> Option<Self> {
        if let Ok(name) = std::env::var("DISPLAY") {
            if !name.is_empty() {
                return Some(Self { name, child: None });
            }
        }
        let pid = std::process::id() as u64;
        for attempt in 0..120u64 {
            let number = 300 + (pid.wrapping_mul(43).wrapping_add(attempt)) % 600;
            let name = format!(":{number}");
            if Path::new(&format!("/tmp/.X{number}-lock")).exists()
                || Path::new(&format!("/tmp/.X11-unix/X{number}")).exists()
            {
                continue;
            }
            let child = Command::new("Xvfb")
                .arg(&name)
                .args(["-screen", "0", "800x600x24", "-nolisten", "tcp", "-ac"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .ok()?;
            let deadline = Instant::now() + Duration::from_secs(8);
            while Instant::now() < deadline {
                if probe_display(&name) {
                    return Some(Self {
                        name,
                        child: Some(child),
                    });
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            let mut child = child;
            let _ = child.kill();
            let _ = child.wait();
        }
        None
    }
}

impl Drop for Display {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn options(command: &[&str]) -> Options {
    let mut options = Options {
        size: Some((80, 24)),
        gui: GuiMode::Gui,
        ..Options::default()
    };
    options.command = command.iter().map(Into::into).collect();
    options
}

fn main() {
    let seconds = std::env::args()
        .nth(1)
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| *value > 0.0 && value.is_finite())
        .unwrap_or(2.0);
    let Some(display) = Display::new() else {
        eprintln!("bench_gui: Xvfb is not available and $DISPLAY is unset");
        return;
    };

    let opts = options(&["/bin/sh", "-c", "yes x"]);
    let mut session = match GuiSession::open_on_display(&opts, Input::None, Some(&display.name)) {
        Ok(session) => session,
        Err(error) => {
            eprintln!("bench_gui: cannot open GUI session: {error}");
            return;
        }
    };

    let start = Instant::now();
    let start_bytes = session.output_bytes();
    let start_frames = session.frame_count();
    // Prime the first frame before measuring.
    let _ = session.step(Duration::from_millis(1));
    let start_bytes = session.output_bytes().max(start_bytes);
    let start_frames = session.frame_count().max(start_frames);
    let measure_start = Instant::now();
    while measure_start.elapsed() < Duration::from_secs_f64(seconds) {
        if !session.step(Duration::from_millis(1)).unwrap_or(false) {
            break;
        }
    }
    let elapsed = measure_start.elapsed().as_secs_f64().max(1e-9);
    let frames = session.frame_count().saturating_sub(start_frames);
    let bytes = session.output_bytes().saturating_sub(start_bytes);
    println!(
        "output: {:.1} MiB/s, {:.0} frames/s, {:.1} MiB/frame ({} bytes, {} frames over {:.2}s)",
        bytes as f64 / (1024.0 * 1024.0) / elapsed,
        frames as f64 / elapsed,
        bytes as f64 / frames.max(1) as f64 / (1024.0 * 1024.0),
        bytes,
        frames,
        elapsed
    );
    session.shutdown();

    let idle_opts = options(&["/bin/sh", "-c", "sleep 5"]);
    let mut idle = match GuiSession::open_on_display(&idle_opts, Input::None, Some(&display.name)) {
        Ok(session) => session,
        Err(error) => {
            eprintln!("bench_gui: cannot open idle session: {error}");
            return;
        }
    };
    let idle_start = Instant::now();
    let mut wakeups = 0u64;
    while idle_start.elapsed() < Duration::from_secs(1) {
        if !idle.step(Duration::from_millis(50)).unwrap_or(false) {
            break;
        }
        wakeups += 1;
    }
    let idle_elapsed = idle_start.elapsed().as_secs_f64().max(1e-9);
    println!(
        "idle: {:.1} loop wakeups/s ({} in {:.2}s)",
        wakeups as f64 / idle_elapsed,
        wakeups,
        idle_elapsed
    );
    idle.shutdown();
    let _ = start;
}
