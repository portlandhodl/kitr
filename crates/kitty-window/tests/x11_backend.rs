// SPDX-License-Identifier: GPL-3.0-or-later
//! Integration tests for the raw X11 + EGL backend.
//!
//! When `$DISPLAY` is unset every test spawns its own `Xvfb` instance, waits
//! for it to accept connections, and kills it on drop. If `Xvfb` is not
//! installed the tests print a skip message and return successfully.

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, MutexGuard, Once};
use std::thread;
use std::time::{Duration, Instant};

use kitty_window::{
    keysym, probe_display, ClipboardReply, DragSourceData, EglWindowSurface, KeyAction, KeyInput,
    Modifiers, MouseButton, PointerShape, Selection, TouchPhase, WindowConfig, WindowEvent,
    X11Window,
};

static TEST_LOCK: Mutex<()> = Mutex::new(());
static REPORT_SKIP_ONCE: Once = Once::new();

fn test_lock() -> MutexGuard<'static, ()> {
    TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[derive(Debug)]
enum Backing {
    Spawned(Child),
    External,
}

/// A display that is either inherited from `$DISPLAY` or backed by a private
/// Xvfb process that is killed when this value is dropped.
struct TestDisplay {
    name: String,
    backing: Backing,
}

impl TestDisplay {
    fn new() -> Option<Self> {
        if let Ok(name) = std::env::var("DISPLAY") {
            if !name.is_empty() {
                return Some(Self {
                    name,
                    backing: Backing::External,
                });
            }
        }

        let pid = std::process::id() as u64;
        for attempt in 0..120u64 {
            let number = 100 + (pid.wrapping_mul(37).wrapping_add(attempt)) % 800;
            let lock = format!("/tmp/.X{number}-lock");
            let socket = format!("/tmp/.X11-unix/X{number}");
            if Path::new(&lock).exists() || Path::new(&socket).exists() {
                continue;
            }
            let name = format!(":{number}");
            let child = Command::new("Xvfb")
                .arg(&name)
                .args(["-screen", "0", "800x600x24", "-nolisten", "tcp", "-ac"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
            let Ok(mut child) = child else {
                REPORT_SKIP_ONCE.call_once(|| {
                    eprintln!("skipping X11 tests: Xvfb is not available");
                });
                return None;
            };
            let deadline = Instant::now() + Duration::from_secs(8);
            let mut ready = false;
            while Instant::now() < deadline {
                if let Ok(Some(_)) = child.try_wait() {
                    break;
                }
                if probe_display(&name) {
                    ready = true;
                    break;
                }
                thread::sleep(Duration::from_millis(20));
            }
            if ready {
                // Give Xvfb a moment to finish initialising its screen.
                thread::sleep(Duration::from_millis(20));
                return Some(Self {
                    name,
                    backing: Backing::Spawned(child),
                });
            }
            let _ = child.kill();
            let _ = child.wait();
        }
        REPORT_SKIP_ONCE.call_once(|| {
            eprintln!("skipping X11 tests: no free display for Xvfb");
        });
        None
    }

    fn is_spawned(&self) -> bool {
        matches!(self.backing, Backing::Spawned(_))
    }

    fn open(&self, config: &WindowConfig) -> X11Window {
        X11Window::open(config, Some(&self.name))
            .unwrap_or_else(|error| panic!("failed to open window on {}: {error}", self.name))
    }
}

impl Drop for TestDisplay {
    fn drop(&mut self) {
        if let Backing::Spawned(child) = &mut self.backing {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn wait_for<F>(window: &mut X11Window, timeout: Duration, mut predicate: F) -> Option<WindowEvent>
where
    F: FnMut(&WindowEvent) -> bool,
{
    let deadline = Instant::now() + timeout;
    loop {
        let now = Instant::now();
        if now >= deadline {
            return None;
        }
        let slice = (deadline - now).as_millis().clamp(1, 100) as i32;
        for event in window.poll_events(slice) {
            if predicate(&event) {
                return Some(event);
            }
        }
    }
}

fn drain(window: &mut X11Window) {
    let deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < deadline {
        if window.poll_events(10).is_empty() {
            break;
        }
    }
}

fn key(symbol: u32, action: KeyAction, modifiers: Modifiers, text: Option<&str>) -> KeyInput {
    let mut input = KeyInput::new(symbol, action).with_modifiers(modifiers);
    if let Some(text) = text {
        input.text = Some(text.to_owned());
    }
    input
}

#[test]
fn window_lifecycle_size_title_and_close() {
    let _guard = test_lock();
    let Some(display) = TestDisplay::new() else {
        return;
    };

    let config = WindowConfig::new("kitty-window test", 320, 200).min_size(100, 80);
    let mut window = display.open(&config);
    assert_eq!(window.size(), (320, 200), "initial client size");
    assert_eq!(window.title(), "kitty-window test");
    assert_eq!(window.server_title().as_deref(), Some("kitty-window test"));
    assert!(window.is_visible());
    assert!(window.scale_factor() >= 0.5);

    window.set_title("renamed").unwrap();
    assert_eq!(window.title(), "renamed");
    assert_eq!(window.server_title().as_deref(), Some("renamed"));

    window
        .inject_event(&WindowEvent::Resize {
            width: 400,
            height: 300,
        })
        .unwrap();
    let resized = wait_for(&mut window, Duration::from_secs(2), |event| {
        matches!(
            event,
            WindowEvent::Resize {
                width: 400,
                height: 300
            }
        )
    });
    assert!(resized.is_some(), "resize event was not delivered");
    assert_eq!(window.size(), (400, 300));

    window
        .inject_event(&WindowEvent::Expose {
            x: 1,
            y: 2,
            width: 30,
            height: 40,
        })
        .unwrap();
    let exposed = wait_for(&mut window, Duration::from_secs(2), |event| {
        matches!(
            event,
            WindowEvent::Expose {
                x: 1,
                y: 2,
                width: 30,
                height: 40
            }
        )
    });
    assert!(exposed.is_some(), "expose event was not delivered");

    window.hide();
    assert!(!window.is_visible());
    window.show();
    assert!(window.is_visible());

    window.set_pointer_shape(PointerShape::Text);
    assert_eq!(window.pointer_shape(), PointerShape::Text);
    window.set_pointer_shape(PointerShape::ResizeSE);
    assert_eq!(window.pointer_shape(), PointerShape::ResizeSE);
    window.bell();

    window.inject_event(&WindowEvent::CloseRequested).unwrap();
    let closed = wait_for(&mut window, Duration::from_secs(2), |event| {
        matches!(event, WindowEvent::CloseRequested)
    });
    assert!(closed.is_some(), "close request was not delivered");
    assert!(window.should_close());
}

#[test]
fn keyboard_modifiers_and_mouse_translation() {
    let _guard = test_lock();
    let Some(display) = TestDisplay::new() else {
        return;
    };
    let mut window = display.open(&WindowConfig::new("input", 200, 150));
    drain(&mut window);

    // Plain printable key.
    window
        .inject_event(&WindowEvent::Key(key(
            0x61,
            KeyAction::Press,
            Modifiers::default(),
            Some("a"),
        )))
        .unwrap();
    let pressed = wait_for(&mut window, Duration::from_secs(2), |event| {
        matches!(event, WindowEvent::Key(input) if input.keysym == 0x61 && input.action == KeyAction::Press)
    })
    .expect("key press");
    if let WindowEvent::Key(input) = pressed {
        assert_eq!(input.text.as_deref(), Some("a"));
        assert!(!input.modifiers.shift);
    }

    // A second press without a release is a repeat (Xkb detectable autorepeat).
    window
        .inject_event(&WindowEvent::Key(key(
            0x61,
            KeyAction::Press,
            Modifiers::default(),
            Some("a"),
        )))
        .unwrap();
    let repeated = wait_for(
        &mut window,
        Duration::from_secs(2),
        |event| matches!(event, WindowEvent::Key(input) if input.keysym == 0x61 && input.action == KeyAction::Repeat),
    );
    if window.detectable_autorepeat() {
        assert!(repeated.is_some(), "repeat key event was not detected");
    }

    window
        .inject_event(&WindowEvent::Key(key(
            0x61,
            KeyAction::Release,
            Modifiers::default(),
            None,
        )))
        .unwrap();
    let released = wait_for(
        &mut window,
        Duration::from_secs(2),
        |event| matches!(event, WindowEvent::Key(input) if input.keysym == 0x61 && input.action == KeyAction::Release),
    );
    assert!(released.is_some(), "key release was not delivered");

    // Shift is tracked from modifier key events, not only from event.state.
    window
        .inject_event(&WindowEvent::Key(key(
            keysym::SHIFT_L,
            KeyAction::Press,
            Modifiers::default(),
            None,
        )))
        .unwrap();
    wait_for(&mut window, Duration::from_secs(2), |event| {
        matches!(event, WindowEvent::Key(input) if input.keysym == keysym::SHIFT_L && input.modifiers.shift)
    })
    .expect("shift press");

    let mut shift = Modifiers {
        shift: true,
        raw_state: 1,
        ..Modifiers::default()
    };
    window
        .inject_event(&WindowEvent::Key(key(
            0x41,
            KeyAction::Press,
            shift,
            Some("A"),
        )))
        .unwrap();
    let shifted = wait_for(
        &mut window,
        Duration::from_secs(2),
        |event| matches!(event, WindowEvent::Key(input) if input.keysym == 0x41 && input.modifiers.shift),
    );
    assert!(shifted.is_some(), "shifted key event");
    if let Some(WindowEvent::Key(input)) = shifted {
        assert_eq!(input.text.as_deref(), Some("A"));
    }

    shift.raw_state = 1;
    window
        .inject_event(&WindowEvent::Key(key(
            keysym::SHIFT_L,
            KeyAction::Release,
            shift,
            None,
        )))
        .unwrap();
    let unshifted = wait_for(
        &mut window,
        Duration::from_secs(2),
        |event| matches!(event, WindowEvent::Key(input) if input.keysym == keysym::SHIFT_L && !input.modifiers.shift),
    );
    assert!(unshifted.is_some(), "shift release clears modifier");

    // Mouse motion, button and scroll (X11 buttons 4/6).
    window
        .inject_event(&WindowEvent::MouseMove {
            x: 11.0,
            y: 22.0,
            modifiers: Modifiers::default(),
        })
        .unwrap();
    assert!(wait_for(&mut window, Duration::from_secs(2), |event| {
        matches!(event, WindowEvent::MouseMove { x, y, .. } if *x == 11.0 && *y == 22.0)
    })
    .is_some());

    window
        .inject_event(&WindowEvent::MouseButton {
            button: MouseButton::Left,
            pressed: true,
            x: 15.0,
            y: 25.0,
            modifiers: Modifiers::default(),
        })
        .unwrap();
    assert!(wait_for(&mut window, Duration::from_secs(2), |event| {
        matches!(
            event,
            WindowEvent::MouseButton {
                button: MouseButton::Left,
                pressed: true,
                x,
                y,
                ..
            } if *x == 15.0 && *y == 25.0
        )
    })
    .is_some());

    window
        .inject_event(&WindowEvent::Scroll {
            x: 5.0,
            y: 6.0,
            delta_x: 0.0,
            delta_y: 1.0,
            button: 4,
            modifiers: Modifiers::default(),
        })
        .unwrap();
    assert!(wait_for(&mut window, Duration::from_secs(2), |event| {
        matches!(
            event,
            WindowEvent::Scroll {
                button: 4,
                delta_y,
                ..
            } if *delta_y == 1.0
        )
    })
    .is_some());

    window
        .inject_event(&WindowEvent::Scroll {
            x: 7.0,
            y: 8.0,
            delta_x: 1.0,
            delta_y: 0.0,
            button: 7,
            modifiers: Modifiers::default(),
        })
        .unwrap();
    assert!(wait_for(&mut window, Duration::from_secs(2), |event| {
        matches!(
            event,
            WindowEvent::Scroll {
                button: 7,
                delta_x,
                ..
            } if *delta_x == 1.0
        )
    })
    .is_some());
}

#[test]
fn egl_clear_swap_and_readback() {
    let _guard = test_lock();
    let Some(display) = TestDisplay::new() else {
        return;
    };
    let mut window = display.open(&WindowConfig::new("egl", 64, 48));
    let mut surface = match EglWindowSurface::new(&window) {
        Ok(surface) => surface,
        Err(error) => {
            eprintln!("skipping EGL test: {error}");
            return;
        }
    };
    assert_eq!(surface.size(), (64, 48));
    assert!(surface.egl_version().0 >= 1);
    assert!(!surface.egl_vendor().is_empty());
    assert!(surface.gl_version_string().is_some());
    surface.set_swap_interval(0).expect("swap interval");

    surface.clear([1.0, 0.0, 0.0, 1.0]).expect("clear red");
    let red = surface.read_pixel(16, 16).expect("read red pixel");
    assert!(red[0] >= 250 && red[1] <= 5 && red[2] <= 5, "got {red:?}");
    surface.swap_buffers().expect("swap");

    window.set_size(80, 60);
    surface.resize(80, 60).expect("resize surface");
    assert_eq!(surface.size(), (80, 60));
    surface.clear([0.0, 0.0, 1.0, 1.0]).expect("clear blue");
    let blue = surface.read_pixel(40, 30).expect("read blue pixel");
    assert!(
        blue[2] >= 250 && blue[0] <= 5 && blue[1] <= 5,
        "got {blue:?}"
    );
    assert_eq!(surface.read_pixels().expect("readback").len(), 80 * 60 * 4);

    surface.swap_buffers().expect("swap after resize");
    drop(surface);
    drop(window);
}

fn roundtrip_text(owner: &mut X11Window, reader: &mut X11Window, selection: Selection) -> String {
    let token = reader
        .request_clipboard_text(selection)
        .expect("start clipboard read");
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        owner.poll_events(20);
        reader.poll_events(20);
        if let Some(reply) = reader.take_clipboard_result(token) {
            match reply.expect("clipboard conversion") {
                ClipboardReply::Text(text) => return text,
                other => panic!("unexpected clipboard reply: {other:?}"),
            }
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for clipboard roundtrip"
        );
    }
}

#[test]
fn clipboard_roundtrip_between_two_windows() {
    let _guard = test_lock();
    let Some(display) = TestDisplay::new() else {
        return;
    };
    let mut owner = display.open(&WindowConfig::new("clipboard-owner", 100, 100));
    let mut reader = display.open(&WindowConfig::new("clipboard-reader", 100, 100));
    drain(&mut owner);
    drain(&mut reader);

    owner
        .set_clipboard_text(Selection::Clipboard, "hello from the owner")
        .expect("set clipboard text");
    // The owner must pump its event loop to answer the reader's request.
    let mut saw_selection_request = false;
    let token = reader
        .request_clipboard_text(Selection::Clipboard)
        .expect("start clipboard read");
    let deadline = Instant::now() + Duration::from_secs(3);
    let text = loop {
        for event in owner.poll_events(20) {
            if matches!(
                event,
                WindowEvent::SelectionRequest {
                    selection: Selection::Clipboard,
                    ..
                }
            ) {
                saw_selection_request = true;
            }
        }
        reader.poll_events(20);
        if let Some(reply) = reader.take_clipboard_result(token) {
            match reply.expect("clipboard conversion") {
                ClipboardReply::Text(text) => break text,
                other => panic!("unexpected clipboard reply: {other:?}"),
            }
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for clipboard roundtrip"
        );
    };
    assert_eq!(text, "hello from the owner");
    assert!(
        saw_selection_request,
        "owner did not observe SelectionRequest"
    );

    // TARGETS should advertise UTF8_STRING.
    let token = reader
        .request_clipboard_targets(Selection::Clipboard)
        .expect("start targets read");
    let deadline = Instant::now() + Duration::from_secs(3);
    let targets = loop {
        owner.poll_events(20);
        reader.poll_events(20);
        if let Some(reply) = reader.take_clipboard_result(token) {
            match reply.expect("targets conversion") {
                ClipboardReply::Targets(targets) => break targets,
                other => panic!("unexpected targets reply: {other:?}"),
            }
        }
        assert!(Instant::now() < deadline, "timed out waiting for TARGETS");
    };
    assert!(targets.iter().any(|target| target == "UTF8_STRING"));
    assert!(targets.iter().any(|target| target == "TARGETS"));

    // PRIMARY uses the same state machine.
    owner
        .set_primary_text("primary selection text")
        .expect("set primary selection");
    let text = roundtrip_text(&mut owner, &mut reader, Selection::Primary);
    assert_eq!(text, "primary selection text");
}

#[test]
fn clipboard_incr_roundtrip_for_large_payload() {
    let _guard = test_lock();
    let Some(display) = TestDisplay::new() else {
        return;
    };
    let mut owner = display.open(&WindowConfig::new("incr-owner", 100, 100));
    let mut reader = display.open(&WindowConfig::new("incr-reader", 100, 100));
    drain(&mut owner);
    drain(&mut reader);

    let payload = "0123456789abcdef".repeat(20_000);
    assert!(payload.len() > 256 * 1024);
    owner
        .set_clipboard_text(Selection::Clipboard, &payload)
        .expect("offer large clipboard payload");
    let text = roundtrip_text(&mut owner, &mut reader, Selection::Clipboard);
    assert_eq!(text.len(), payload.len());
    assert_eq!(text, payload);
}

#[test]
fn clipboard_incr_roundtrip_for_large_primary_payload() {
    let _guard = test_lock();
    let Some(display) = TestDisplay::new() else {
        return;
    };
    let mut owner = display.open(&WindowConfig::new("primary-incr-owner", 100, 100));
    let mut reader = display.open(&WindowConfig::new("primary-incr-reader", 100, 100));
    drain(&mut owner);
    drain(&mut reader);

    let payload = "primary-0123456789".repeat(20_000);
    assert!(payload.len() > 256 * 1024);
    owner
        .set_primary_text(&payload)
        .expect("offer large primary payload");
    let text = roundtrip_text(&mut owner, &mut reader, Selection::Primary);
    assert_eq!(text.len(), payload.len());
    assert_eq!(text, payload);
}

#[test]
fn cursor_scale_theme_and_hot_reload_state() {
    let _guard = test_lock();
    let Some(display) = TestDisplay::new() else {
        return;
    };
    let mut window = display.open(&WindowConfig::new("cursor-theme", 120, 80));
    drain(&mut window);
    let base = window.cursor_size();
    window.set_cursor_scale(2.0);
    assert_eq!(window.cursor_scale(), 2.0);
    assert!(window.cursor_size() >= base);
    window.set_cursor_size(Some(48));
    assert_eq!(window.cursor_size(), 48);
    window.set_cursor_theme(Some("Adwaita"));
    assert_eq!(window.cursor_theme_name(), "Adwaita");
    window.set_cursor_theme(None);
    assert!(window.reload_cursor_theme());
    // Calling refresh repeatedly must be cheap and stable.
    assert!(!window.refresh_cursor_theme() || window.cursor_size() > 0);
}

#[test]
fn xinput2_touch_mapping_is_safe_and_injectable() {
    let _guard = test_lock();
    let Some(display) = TestDisplay::new() else {
        return;
    };
    let mut window = display.open(&WindowConfig::new("touch", 160, 120));
    drain(&mut window);
    // Xvfb usually has no touch device. The XI2 map logic is covered by unit
    // tests; this asserts the public path is safe with or without a device.
    let _ = window.has_touch_input();
    window
        .inject_event(&WindowEvent::Touch {
            id: 9,
            x: 10.0,
            y: 20.0,
            pressure: 0.5,
            phase: TouchPhase::Began,
        })
        .unwrap();
    let touch = wait_for(&mut window, Duration::from_secs(2), |event| {
        matches!(
            event,
            WindowEvent::Touch {
                id: 9,
                phase: TouchPhase::Began,
                ..
            }
        )
    });
    assert!(touch.is_some(), "injected touch event was not delivered");
}

#[test]
fn xdnd_drag_source_and_target_roundtrip() {
    let _guard = test_lock();
    let Some(display) = TestDisplay::new() else {
        return;
    };
    let mut source = display.open(&WindowConfig::new("xdnd-source", 100, 100));
    let mut target = display.open(&WindowConfig::new("xdnd-target", 100, 100));
    drain(&mut source);
    drain(&mut target);

    source
        .start_drag_to_at(
            DragSourceData::from_text("dragged text"),
            target.window_id(),
            20,
            20,
        )
        .expect("start drag source");
    assert!(source.is_drag_source_active());
    assert!(source
        .drag_source_targets()
        .iter()
        .any(|m| m == "UTF8_STRING"));

    let deadline = Instant::now() + Duration::from_secs(3);
    let mut entered = false;
    while Instant::now() < deadline && !entered {
        source.poll_events(10);
        for event in target.poll_events(10) {
            if matches!(event, WindowEvent::DragEnter { .. }) {
                entered = true;
            }
        }
    }
    assert!(entered, "XDND target did not see DragEnter");
    target.drag_accept(None).expect("accept drag");
    source.drag_drop().expect("drop drag source");

    let deadline = Instant::now() + Duration::from_secs(3);
    let mut dropped = None;
    let mut finished = false;
    while Instant::now() < deadline && (dropped.is_none() || !finished) {
        for event in source.poll_events(10) {
            if matches!(event, WindowEvent::DragFinished { .. }) {
                finished = true;
            }
        }
        for event in target.poll_events(10) {
            if let WindowEvent::DragDrop { text, .. } = event {
                dropped = text;
            }
        }
    }
    assert_eq!(dropped.as_deref(), Some("dragged text"));
    assert!(finished, "drag source did not receive XdndFinished");
    assert!(!source.is_drag_source_active());
}

#[test]
fn ime_toggle_is_safe_without_an_input_method() {
    let _guard = test_lock();
    let Some(display) = TestDisplay::new() else {
        return;
    };
    let mut window = display.open(&WindowConfig::new("ime", 120, 80));
    drain(&mut window);
    assert!(window.ime_enabled());
    window.set_ime_enabled(false);
    assert!(!window.ime_enabled());
    window.set_ime_enabled(true);
    assert!(window.ime_enabled());
    // Xvfb rarely has an input method configured. The unit tests exercise the
    // callback state machine; this test only guarantees the public toggle is
    // safe when XOpenIM returns nothing.
    let _ = window.has_input_method();
}

#[test]
fn wakeup_latency_and_timeout() {
    let _guard = test_lock();
    let Some(display) = TestDisplay::new() else {
        return;
    };
    let mut window = display.open(&WindowConfig::new("wakeup", 100, 100));
    drain(&mut window);

    let handle = window.wakeup_handle();
    let start = Instant::now();
    let thread = thread::spawn(move || {
        thread::sleep(Duration::from_millis(40));
        handle.wakeup();
    });
    let mut saw_wakeup = false;
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        let events = window.poll_events(250);
        if events
            .iter()
            .any(|event| matches!(event, WindowEvent::Wakeup))
        {
            saw_wakeup = true;
            break;
        }
    }
    thread.join().unwrap();
    assert!(saw_wakeup, "wakeup did not interrupt the event pump");
    assert!(start.elapsed() < Duration::from_millis(1500));

    // A poll without events must respect its timeout instead of blocking.
    drain(&mut window);
    let start = Instant::now();
    let events = window.poll_events(60);
    let elapsed = start.elapsed();
    assert!(events.is_empty(), "unexpected events: {events:?}");
    assert!(
        elapsed >= Duration::from_millis(40),
        "returned too early: {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_millis(800),
        "returned too late: {elapsed:?}"
    );

    // wait_events returns queued events without blocking.
    window
        .inject_event(&WindowEvent::Expose {
            x: 0,
            y: 0,
            width: 5,
            height: 5,
        })
        .unwrap();
    let start = Instant::now();
    let events = window.wait_events();
    assert!(start.elapsed() < Duration::from_millis(500));
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0], WindowEvent::Expose { .. }));
}

#[test]
fn fullscreen_maximize_geometry_and_scale() {
    let _guard = test_lock();
    let Some(display) = TestDisplay::new() else {
        return;
    };
    let mut window = display.open(&WindowConfig::new("states", 320, 200));
    drain(&mut window);

    // Without a window manager the backend falls back to direct geometry
    // changes, which is what Xvfb gives us.
    window.set_fullscreen(true);
    assert!(window.is_fullscreen());
    wait_for(&mut window, Duration::from_secs(2), |event| {
        matches!(
            event,
            WindowEvent::Resize {
                width: 800,
                height: 600
            }
        )
    });
    if display.is_spawned() {
        assert_eq!(window.size(), (800, 600));
    }
    window.set_fullscreen(false);
    assert!(!window.is_fullscreen());
    wait_for(&mut window, Duration::from_secs(2), |event| {
        matches!(
            event,
            WindowEvent::Resize {
                width: 320,
                height: 200
            }
        )
    });
    if display.is_spawned() {
        assert_eq!(window.size(), (320, 200));
    }

    window.set_maximized(true);
    assert!(window.is_maximized());
    if display.is_spawned() {
        assert_eq!(window.size(), (800, 600));
    }
    window.set_maximized(false);
    assert!(!window.is_maximized());
    if display.is_spawned() {
        assert_eq!(window.size(), (320, 200));
    }

    window.set_position(10, 20);
    if display.is_spawned() {
        assert_eq!(window.position(), (10, 20));
    }
    window.set_size_limits(50, 40, Some(1000), Some(800));
    assert_eq!(window.min_size(), (50, 40));
    assert!(window.scale_factor() >= 0.5);

    window.set_fullscreen(true);
    window.hide();
    assert!(!window.is_visible());
    window.show();
    assert!(window.is_visible());
    window.set_fullscreen(false);
}
