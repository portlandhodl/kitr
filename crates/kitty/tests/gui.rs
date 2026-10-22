// SPDX-License-Identifier: GPL-3.0-or-later
#![cfg(all(target_os = "linux", feature = "gui"))]

//! End-to-end X11/EGL frontend tests.
//!
//! The tests start their own `Xvfb` (unless `$DISPLAY` is set), and skip
//! cleanly when Xvfb, EGL or the system fonts are unavailable. Every loop is
//! bounded so a broken event loop cannot hang the test suite.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, MutexGuard, Once};
use std::time::{Duration, Instant};

use kitty::gui::GuiSession;
use kitty::Input;
use kitty_os::pty;
use kitty_window::{keysym, probe_display, KeyAction, KeyInput, WindowEvent};

static TEST_LOCK: Mutex<()> = Mutex::new(());
static REPORT_SKIP_ONCE: Once = Once::new();

fn test_lock() -> MutexGuard<'static, ()> {
    TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

struct TestDisplay {
    name: String,
    child: Option<Child>,
}

impl TestDisplay {
    fn new() -> Option<Self> {
        if let Ok(name) = std::env::var("DISPLAY") {
            if !name.is_empty() {
                return Some(Self { name, child: None });
            }
        }
        let pid = std::process::id() as u64;
        for attempt in 0..120u64 {
            let number = 200 + (pid.wrapping_mul(41).wrapping_add(attempt)) % 700;
            let lock = format!("/tmp/.X{number}-lock");
            let socket = format!("/tmp/.X11-unix/X{number}");
            if Path::new(&lock).exists() || Path::new(&socket).exists() {
                continue;
            }
            let name = format!(":{number}");
            let child = Command::new("Xvfb")
                .arg(&name)
                .args(["-screen", "0", "640x480x24", "-nolisten", "tcp", "-ac"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
            let Ok(mut child) = child else {
                skip("Xvfb is not available");
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
                std::thread::sleep(Duration::from_millis(20));
            }
            if ready {
                std::thread::sleep(Duration::from_millis(20));
                return Some(Self {
                    name,
                    child: Some(child),
                });
            }
            let _ = child.kill();
            let _ = child.wait();
        }
        skip("no free display for Xvfb");
        None
    }
}

impl Drop for TestDisplay {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn skip(reason: &str) {
    REPORT_SKIP_ONCE.call_once(|| eprintln!("skipping GUI tests: {reason}"));
}

fn options(command: &[&str], size: (u16, u16)) -> kitty::Options {
    let mut options = kitty::Options {
        size: Some(size),
        gui: kitty::GuiMode::Gui,
        ..kitty::Options::default()
    };
    options.command = command.iter().map(Into::into).collect();
    options
}

fn dump(session: &GuiSession) -> String {
    kitty::dump_screen(session.screen())
}

fn wait_until(
    session: &mut GuiSession,
    timeout: Duration,
    predicate: impl Fn(&GuiSession) -> bool,
) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let _ = session.step(Duration::from_millis(20));
        if predicate(session) {
            return true;
        }
    }
    false
}

fn inject_text(session: &mut GuiSession, text: &str) {
    for ch in text.chars() {
        let symbol = ch as u32;
        let press = KeyInput::new(symbol, KeyAction::Press).with_text(&ch.to_string());
        session
            .inject_event(&WindowEvent::Key(press))
            .expect("inject key press");
        let release = KeyInput::new(symbol, KeyAction::Release);
        session
            .inject_event(&WindowEvent::Key(release))
            .expect("inject key release");
    }
    let press = KeyInput::new(keysym::RETURN, KeyAction::Press);
    session
        .inject_event(&WindowEvent::Key(press))
        .expect("inject enter");
    let release = KeyInput::new(keysym::RETURN, KeyAction::Release);
    session
        .inject_event(&WindowEvent::Key(release))
        .expect("inject enter release");
}

fn ppm_stats(path: &Path) -> Option<(u32, u32, usize)> {
    let data = std::fs::read(path).ok()?;
    let mut offset = 0usize;
    let mut lines = Vec::new();
    for _ in 0..3 {
        let end = offset + data[offset..].iter().position(|b| *b == b'\n')?;
        lines.push(&data[offset..end]);
        offset = end + 1;
    }
    if lines[0] != b"P6" || lines[2] != b"255" {
        return None;
    }
    let text = std::str::from_utf8(lines[1]).ok()?;
    let (width, height) = text.split_once(' ')?;
    let width: u32 = width.parse().ok()?;
    let height: u32 = height.parse().ok()?;
    let body = &data[offset..];
    if body.len() < width as usize * height as usize * 3 {
        return None;
    }
    let mut non_black = 0usize;
    for pixel in body.as_chunks::<3>().0 {
        if pixel != &[0, 0, 0] {
            non_black += 1;
        }
    }
    Some((width, height, non_black))
}

#[test]
fn screenshot_screen_text_and_pixels() {
    let _guard = test_lock();
    let Some(display) = TestDisplay::new() else {
        return;
    };
    let opts = options(&["/bin/sh", "-c", "printf 'gui-ok'; sleep 1"], (40, 8));
    let mut session = match GuiSession::open_on_display(&opts, Input::None, Some(&display.name)) {
        Ok(session) => session,
        Err(error) => {
            skip(&format!("EGL/window unavailable: {error}"));
            return;
        }
    };

    assert!(
        wait_until(&mut session, Duration::from_secs(5), |session| {
            dump(session).contains("gui-ok")
        }),
        "screen did not receive child output: {:?}",
        dump(&session)
    );

    session.render_now().expect("render");
    let (width, height, pixels) = session.capture_rgba().expect("framebuffer");
    assert!(width > 0 && height > 0);
    assert_eq!(pixels.len(), width as usize * height as usize * 4);
    let non_black = pixels
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[..3] != [0, 0, 0])
        .count();
    assert!(
        non_black > 100,
        "framebuffer is blank ({non_black} lit pixels)"
    );

    let path = std::env::temp_dir().join(format!("kitr-gui-{}.ppm", std::process::id()));
    session.write_screenshot(&path).expect("write ppm");
    let (pw, ph, lit) = ppm_stats(&path).expect("valid PPM");
    assert!(pw > 0 && ph > 0 && lit > 100);
    let _ = std::fs::remove_file(&path);

    session.shutdown();
    assert!(session.screen().columns >= 40);
}

#[test]
fn resize_and_typing_echo() {
    let _guard = test_lock();
    let Some(display) = TestDisplay::new() else {
        return;
    };
    let opts = options(&["/bin/cat"], (30, 8));
    let mut session = match GuiSession::open_on_display(&opts, Input::None, Some(&display.name)) {
        Ok(session) => session,
        Err(error) => {
            skip(&format!("EGL/window unavailable: {error}"));
            return;
        }
    };

    // Let cat start before injecting keys.
    let _ = session.step(Duration::from_millis(100));
    inject_text(&mut session, "hello");
    assert!(
        wait_until(&mut session, Duration::from_secs(5), |session| {
            dump(session).contains("hello")
        }),
        "echo was not visible: {:?}",
        dump(&session)
    );

    let (width, height) = session.window_size();
    let initial_columns = session.screen().columns;
    let initial_rows = session.screen().lines;
    session.resize_window((width / 2).max(80), (height / 2).max(60));
    let resized = wait_until(&mut session, Duration::from_secs(5), |session| {
        session.screen().columns != initial_columns || session.screen().lines != initial_rows
    });
    assert!(
        resized,
        "screen did not resize (was {initial_columns}x{initial_rows}, now {}x{})",
        session.screen().columns,
        session.screen().lines
    );
    session.render_now().expect("render after resize");
    let (width, height, pixels) = session.capture_rgba().expect("framebuffer after resize");
    assert!(width > 0 && height > 0);
    assert!(pixels.iter().any(|byte| *byte != 0));
    session.shutdown();
}

#[test]
fn binary_gui_screenshot_and_termios_restore() {
    let _guard = test_lock();
    let Some(display) = TestDisplay::new() else {
        return;
    };

    // Probe the GUI stack first so an EGL-less host skips instead of failing.
    let probe_opts = options(&["/bin/true"], (10, 3));
    match GuiSession::open_on_display(&probe_opts, Input::None, Some(&display.name)) {
        Ok(mut probe) => probe.shutdown(),
        Err(error) => {
            skip(&format!("EGL/window unavailable: {error}"));
            return;
        }
    }

    let pair = pty::open_pty().expect("pty");
    pty::set_window_size(pair.master_fd(), pty::WindowSize::new(5, 20)).unwrap();
    let check_fd = pair.slave().try_clone().unwrap();
    let original = pty::get_termios(check_fd.as_raw_fd()).unwrap();
    let (master, slave) = pair.into_fds();
    let _master = master;

    let path: PathBuf =
        std::env::temp_dir().join(format!("kitr-gui-cli-{}.ppm", std::process::id()));
    let mut child = Command::new(env!("CARGO_BIN_EXE_kitr"))
        .args([
            "--gui",
            "--timeout",
            "8",
            "--size",
            "20x5",
            "--screenshot",
            path.to_str().unwrap(),
            "--",
            "/bin/sh",
            "-c",
            "printf 'cli-ok'; sleep 0.3",
        ])
        .env("DISPLAY", &display.name)
        .stdin(Stdio::from(std::fs::File::from(slave)))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn kitr");

    let status = wait_child_timeout(&mut child, Duration::from_secs(20));
    assert_eq!(status.code(), Some(0), "GUI binary did not exit cleanly");
    let (width, height, lit) = ppm_stats(&path).expect("valid screenshot");
    assert!(width > 0 && height > 0 && lit > 20);
    let _ = std::fs::remove_file(&path);

    let restored = pty::get_termios(check_fd.as_raw_fd()).unwrap();
    assert_eq!(
        restored.c_lflag & pty::ECHO,
        original.c_lflag & pty::ECHO,
        "ECHO was not restored"
    );
    assert_eq!(
        restored.c_lflag & pty::ICANON,
        original.c_lflag & pty::ICANON,
        "ICANON was not restored"
    );
}

fn wait_child_timeout(child: &mut Child, timeout: Duration) -> std::process::ExitStatus {
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            return status;
        }
        if start.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            panic!("child did not exit within {timeout:?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

use std::os::fd::AsRawFd;
