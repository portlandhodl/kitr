// SPDX-License-Identifier: GPL-3.0-or-later
//! Tab-switch and remote-control round-trip benchmark.
//!
//! Usage:
//!   xvfb-run -a cargo run -p kitty --release --example bench_tabs_remote

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use kitty::gui::GuiSession;
use kitty::{GuiMode, Input, Options};
use kitty_remote::json::Value;
use kitty_remote::{Client, KITTY_COMPAT_VERSION};
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
            let number = 400 + (pid.wrapping_mul(47).wrapping_add(attempt)) % 500;
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

fn options(command: &[&str], listen: Option<String>) -> Options {
    let mut options = Options {
        size: Some((80, 24)),
        gui: GuiMode::Gui,
        listen_on: listen.map(Into::into),
        ..Options::default()
    };
    options.command = command.iter().map(Into::into).collect();
    options
}

fn main() {
    let Some(display) = Display::new() else {
        eprintln!("bench_tabs_remote: Xvfb is not available and $DISPLAY is unset");
        return;
    };

    let opts = options(&["/bin/sh", "-c", "sleep 20"], None);
    let mut session = match GuiSession::open_on_display(&opts, Input::None, Some(&display.name)) {
        Ok(session) => session,
        Err(error) => {
            eprintln!("bench_tabs_remote: cannot open GUI session: {error}");
            return;
        }
    };
    session
        .new_tab_for_test(&["/bin/sh", "-c", "sleep 20"])
        .expect("second tab");
    let switches = 200usize;
    let start = Instant::now();
    for i in 0..switches {
        session.select_tab(1 - (i & 1));
        session.render_now().expect("render");
    }
    let elapsed = start.elapsed().as_secs_f64().max(1e-9);
    println!(
        "tab switch+render: {:.2} ms/op ({:.0} ops/s, {} switches)",
        elapsed * 1000.0 / switches as f64,
        switches as f64 / elapsed,
        switches
    );
    session.shutdown();

    let socket = std::env::temp_dir().join(format!(
        "kitr-bench-{}-{}.sock",
        std::process::id(),
        Instant::now().elapsed().as_nanos()
    ));
    let listen = format!("unix:{}", socket.display());
    let opts = options(
        &["/bin/sh", "-c", "printf bench; sleep 20"],
        Some(listen.clone()),
    );
    let mut session = match GuiSession::open_on_display(&opts, Input::None, Some(&display.name)) {
        Ok(session) => session,
        Err(error) => {
            eprintln!("bench_tabs_remote: cannot open remote session: {error}");
            return;
        }
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && !kitty::dump_screen(session.screen()).contains("bench") {
        let _ = session.step(Duration::from_millis(5));
    }
    let rounds = 200usize;
    let path = socket.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    let mut result = None;
    std::thread::scope(|scope| {
        scope.spawn(move || {
            let elapsed = (|| -> Result<f64, String> {
                let mut client = Client::connect_plain(&path).map_err(|e| e.to_string())?;
                let request = Value::object_from([
                    ("cmd", Value::String("get-text".into())),
                    ("version", Value::String(KITTY_COMPAT_VERSION.into())),
                    (
                        "payload",
                        Value::object_from([("extent", Value::String("screen".into()))]),
                    ),
                ]);
                let start = Instant::now();
                for _ in 0..rounds {
                    let response = client.command(request.clone()).map_err(|e| e.to_string())?;
                    if response.get("ok").and_then(Value::as_bool) != Some(true) {
                        return Err("remote command failed".into());
                    }
                }
                Ok(start.elapsed().as_secs_f64())
            })();
            let _ = tx.send(elapsed);
        });
        loop {
            match rx.try_recv() {
                Ok(value) => {
                    result = Some(value);
                    break;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    let _ = session.step(Duration::from_millis(2));
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
            }
        }
    });
    match result {
        Some(Ok(elapsed)) => {
            let elapsed = elapsed.max(1e-9);
            println!(
                "remote get-text round trip: {:.3} ms/op ({:.0} ops/s, {} requests)",
                elapsed * 1000.0 / rounds as f64,
                rounds as f64 / elapsed,
                rounds
            );
        }
        Some(Err(error)) => eprintln!("bench_tabs_remote: remote benchmark failed: {error}"),
        None => eprintln!("bench_tabs_remote: remote benchmark thread disconnected"),
    }
    let _ = std::fs::remove_file(&socket);
    session.shutdown();
}
