// SPDX-License-Identifier: GPL-3.0-or-later
//! End-to-end Wayland tests.
//!
//! These tests connect to the compositor named by `$WAYLAND_DISPLAY` and skip
//! cleanly when it is unset or unreachable. They never spawn or block on a
//! missing compositor. End-to-end Wayland coverage requires a real compositor
//! such as weston; the protocol state, key-repeat, pointer and configure
//! coalescing tests live in `src/wayland.rs` and need no compositor.

#![cfg(feature = "wayland")]

use std::process::{Command, Stdio};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use kitty_window::{
    probe_wayland, EglWaylandSurface, Modifiers, PointerShape, Selection, WaylandWindow,
    WindowBackend, WindowConfig, WindowEvent,
};

static TEST_LOCK: Mutex<()> = Mutex::new(());

fn test_lock() -> MutexGuard<'static, ()> {
    TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn compositor_available() -> bool {
    let display = match std::env::var_os("WAYLAND_DISPLAY") {
        Some(display) if !display.is_empty() => display,
        _ => {
            eprintln!("skipping Wayland tests: WAYLAND_DISPLAY is not set");
            return false;
        }
    };
    if !probe_wayland() {
        eprintln!("skipping Wayland tests: cannot connect to {:?}", display);
        return false;
    }
    true
}

fn wait_for<F>(window: &mut WaylandWindow, timeout: Duration, mut predicate: F) -> bool
where
    F: FnMut(&WindowEvent) -> bool,
{
    let deadline = Instant::now() + timeout;
    loop {
        let now = Instant::now();
        if now >= deadline {
            return false;
        }
        let slice = (deadline - now).as_millis().clamp(1, 100) as i32;
        if window.poll_events(slice).iter().any(&mut predicate) {
            return true;
        }
    }
}

#[test]
fn wayland_window_lifecycle_and_shm_presentation() {
    let _guard = test_lock();
    if !compositor_available() {
        return;
    }
    let config = WindowConfig::new("kitty-window wayland test", 320, 200).min_size(100, 80);
    let mut window = match WaylandWindow::new(&config) {
        Ok(window) => window,
        Err(error) => {
            eprintln!("skipping Wayland lifecycle test: {error}");
            return;
        }
    };

    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        window.poll_events(50);
        if window.is_visible() {
            break;
        }
    }
    assert!(
        window.is_visible(),
        "the compositor never configured the surface"
    );
    assert!(window.scale_factor() > 0.0);
    assert!(window.integer_scale() >= 1);
    assert_eq!(window.title(), "kitty-window wayland test");

    window.set_title("renamed on wayland").expect("set title");
    assert_eq!(window.title(), "renamed on wayland");
    window
        .present_clear([0.2, 0.4, 0.8, 1.0])
        .expect("shm present");

    window.set_size(400, 300);
    assert_eq!(window.size(), (400, 300));
    window
        .present_clear([0.0, 0.5, 0.0, 1.0])
        .expect("resized shm present");

    let (width, height) = window.size();
    window.set_size_limits(50, 40, Some(width.max(500)), Some(height.max(400)));
    assert_eq!(window.min_size(), (50, 40));

    window.set_pointer_shape(PointerShape::Text);
    assert_eq!(window.pointer_shape(), PointerShape::Text);
    window.set_fullscreen(true);
    assert!(window.is_fullscreen());
    window.set_fullscreen(false);
    assert!(!window.is_fullscreen());
    window.set_maximized(true);
    assert!(window.is_maximized());
    window.set_maximized(false);
    assert!(!window.is_maximized());

    window
        .inject_event(&WindowEvent::MouseMove {
            x: 12.0,
            y: 34.0,
            modifiers: Modifiers::default(),
        })
        .expect("inject motion");
    assert!(wait_for(&mut window, Duration::from_secs(1), |event| {
        matches!(event, WindowEvent::MouseMove { x, y, .. } if *x == 12.0 && *y == 34.0)
    }));

    window
        .inject_event(&WindowEvent::CloseRequested)
        .expect("inject close");
    assert!(wait_for(&mut window, Duration::from_secs(1), |event| {
        matches!(event, WindowEvent::CloseRequested)
    }));
    assert!(window.should_close());
}

#[test]
fn wayland_clipboard_requires_a_selection_owner_before_reading() {
    let _guard = test_lock();
    if !compositor_available() {
        return;
    }
    let mut window = match WaylandWindow::new(&WindowConfig::new("clipboard", 120, 80)) {
        Ok(window) => window,
        Err(error) => {
            eprintln!("skipping Wayland clipboard test: {error}");
            return;
        }
    };
    // Give the compositor a chance to deliver an existing selection.
    for _ in 0..5 {
        window.poll_events(20);
    }
    if !window.has_clipboard_offer(Selection::Clipboard) {
        eprintln!("skipping Wayland clipboard read: no current selection owner");
        return;
    }
    match window.clipboard_text_blocking(Selection::Clipboard, Duration::from_secs(2)) {
        Ok(text) => {
            let _ = text;
        }
        Err(error) => eprintln!("Wayland clipboard read did not complete: {error}"),
    }
}

#[test]
fn unified_constructor_selects_wayland_when_display_is_set() {
    let _guard = test_lock();
    if !compositor_available() {
        return;
    }
    let mut window = match WindowBackend::open(&WindowConfig::new("backend", 160, 100)) {
        Ok(window) => window,
        Err(error) => {
            eprintln!("skipping unified backend test: {error}");
            return;
        }
    };
    assert!(window.is_wayland());
    window.poll_events(50);
    window.request_close();
    assert!(window.should_close());
}

#[test]
fn wayland_egl_surface_clear_swap_and_readback() {
    let _guard = test_lock();
    if !compositor_available() {
        return;
    }
    let mut window = match WaylandWindow::new(&WindowConfig::new("egl-wayland", 64, 48)) {
        Ok(window) => window,
        Err(error) => {
            eprintln!("skipping Wayland EGL test: {error}");
            return;
        }
    };
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        window.poll_events(50);
        if window.is_visible() {
            break;
        }
    }
    let mut surface = match EglWaylandSurface::new(&window) {
        Ok(surface) => surface,
        Err(error) => {
            eprintln!("skipping Wayland EGL test (no EGL surface): {error}");
            return;
        }
    };
    assert!(surface.egl_version().0 >= 1);
    assert!(surface.gl_version_string().is_some());
    surface.set_swap_interval(0).expect("swap interval");
    surface.clear([1.0, 0.0, 0.0, 1.0]).expect("clear red");
    let red = surface.read_pixel(16, 16).expect("read red pixel");
    assert!(red[0] >= 250 && red[1] <= 5 && red[2] <= 5, "got {red:?}");
    surface.swap_buffers().expect("swap");
    surface.resize(80, 60).expect("resize EGL surface");
    assert_eq!(surface.size(), (80, 60));
    surface
        .swap_after_clear([0.0, 0.0, 1.0, 1.0])
        .expect("blue frame");
    drop(surface);
    drop(window);
}

#[test]
fn wayland_clipboard_reads_from_an_external_owner() {
    let _guard = test_lock();
    if !compositor_available() {
        return;
    }
    let available = Command::new("wl-copy")
        .arg("--clear")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false);
    if !available {
        eprintln!("skipping external clipboard test: wl-copy is not usable");
        return;
    }
    let mut window = match WaylandWindow::new(&WindowConfig::new("clipboard-read", 120, 80)) {
        Ok(window) => window,
        Err(error) => {
            eprintln!("skipping external clipboard test: {error}");
            return;
        }
    };
    let status = Command::new("wl-copy")
        .arg("wayland clipboard test")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let owned = status.map(|status| status.success()).unwrap_or(false);
    if !owned {
        eprintln!("skipping external clipboard test: wl-copy could not own the selection");
        return;
    }

    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline && !window.has_clipboard_offer(Selection::Clipboard) {
        window.poll_events(50);
    }
    if !window.has_clipboard_offer(Selection::Clipboard) {
        eprintln!("skipping external clipboard test: no selection event arrived");
        let _ = Command::new("wl-copy").arg("--clear").status();
        return;
    }

    let targets = window
        .clipboard_targets_blocking(Selection::Clipboard, Duration::from_secs(2))
        .expect("TARGETS reply");
    assert!(targets.iter().any(|target| target == "TARGETS"));
    assert!(targets
        .iter()
        .any(|target| target == "text/plain;charset=utf-8" || target == "text/plain"));

    let text = window
        .clipboard_text_blocking(Selection::Clipboard, Duration::from_secs(2))
        .expect("clipboard text");
    assert_eq!(text, "wayland clipboard test");
    let _ = Command::new("wl-copy").arg("--clear").status();
}
