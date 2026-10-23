// SPDX-License-Identifier: GPL-3.0-or-later
#![cfg(all(target_os = "linux", feature = "gui"))]

//! End-to-end X11/EGL frontend tests.
//!
//! The tests start their own `Xvfb` (unless `$DISPLAY` is set), and skip
//! cleanly when Xvfb, EGL or the system fonts are unavailable. Every loop is
//! bounded so a broken event loop cannot hang the test suite.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::{Mutex, MutexGuard, Once};
use std::time::{Duration, Instant};

use kitty::gui::GuiSession;
use kitty::Input;
use kitty_os::pty;
use kitty_window::{
    keysym, probe_display, KeyAction, KeyInput, Modifiers, MouseButton, WindowEvent,
};

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
        .env("KITTY_CONFIG_DIRECTORY", "/nonexistent")
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

fn inject_key(session: &mut GuiSession, symbol: u32, control: bool, shift: bool, alt: bool) {
    let mut modifiers = Modifiers {
        control,
        shift,
        alt,
        ..Modifiers::default()
    };
    // X11 key events carry modifiers in `state`; inject_event forwards that
    // field verbatim rather than the boolean flags.
    modifiers.raw_state = (u32::from(shift)) | (u32::from(control) << 2) | (u32::from(alt) << 3);
    let press = KeyInput::new(symbol, KeyAction::Press).with_modifiers(modifiers);
    session
        .inject_event(&WindowEvent::Key(press))
        .expect("inject key press");
    let release = KeyInput::new(symbol, KeyAction::Release).with_modifiers(modifiers);
    session
        .inject_event(&WindowEvent::Key(release))
        .expect("inject key release");
}

fn run_remote_while_stepping(session: &mut GuiSession, args: &[&str], timeout: Duration) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_kitr"))
        .env("KITTY_CONFIG_DIRECTORY", "/nonexistent")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn kitr @");
    let deadline = Instant::now() + timeout;
    loop {
        if child.try_wait().expect("try_wait client").is_some() {
            break;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("remote client did not finish within {timeout:?}");
        }
        let _ = session.step(Duration::from_millis(10));
        std::thread::sleep(Duration::from_millis(2));
    }
    child.wait_with_output().expect("client output")
}

#[test]
fn tabs_create_switch_and_close() {
    let _guard = test_lock();
    let Some(display) = TestDisplay::new() else {
        return;
    };
    let opts = options(&["/bin/sh", "-c", "printf 'tab-one'; sleep 20"], (40, 8));
    let mut session = match GuiSession::open_on_display(&opts, Input::None, Some(&display.name)) {
        Ok(session) => session,
        Err(error) => {
            skip(&format!("EGL/window unavailable: {error}"));
            return;
        }
    };
    assert!(wait_until(
        &mut session,
        Duration::from_secs(5),
        |session| { dump(session).contains("tab-one") }
    ));
    assert_eq!(session.tab_count(), 1);

    inject_key(&mut session, b't' as u32, true, true, false);
    assert!(
        wait_until(&mut session, Duration::from_secs(5), |session| {
            session.tab_count() == 2
        }),
        "Ctrl+Shift+T did not create a tab"
    );

    inject_key(&mut session, keysym::LEFT, true, true, false);
    assert!(
        wait_until(&mut session, Duration::from_secs(3), |session| {
            session.active_tab_index() == 0 && dump(session).contains("tab-one")
        }),
        "Ctrl+Shift+Left did not activate the first tab"
    );

    inject_key(&mut session, keysym::RIGHT, true, true, false);
    assert!(wait_until(
        &mut session,
        Duration::from_secs(3),
        |session| { session.active_tab_index() == 1 }
    ));

    inject_key(&mut session, b'w' as u32, true, true, false);
    assert!(
        wait_until(&mut session, Duration::from_secs(5), |session| {
            session.tab_count() == 1 && session.active_tab_index() == 0
        }),
        "Ctrl+Shift+W did not close the active tab"
    );
    session.shutdown();
}

#[test]
fn tab_bar_hidden_for_single_tab_and_shown_for_two() {
    let _guard = test_lock();
    let Some(display) = TestDisplay::new() else {
        return;
    };
    let config = std::env::temp_dir().join(format!(
        "kitr-tabbar-{}-{}.conf",
        std::process::id(),
        Instant::now().elapsed().as_nanos()
    ));
    std::fs::write(&config, "tab_bar_style powerline\ntab_bar_min_tabs 2\n").unwrap();
    let opts = kitty::Options {
        size: Some((40, 8)),
        gui: kitty::GuiMode::Gui,
        config: Some(config.clone().into_os_string()),
        ..kitty::Options::default()
    };
    let mut session = match GuiSession::open_on_display(&opts, Input::None, Some(&display.name)) {
        Ok(session) => session,
        Err(error) => {
            let _ = std::fs::remove_file(&config);
            skip(&format!("EGL/window unavailable: {error}"));
            return;
        }
    };
    assert_eq!(session.tab_count(), 1);
    assert!(
        !session.tab_bar_visible(),
        "single tab must autohide the bar"
    );
    assert_eq!(session.tab_bar_height(), 0.0);
    let hidden_size = session.window_size();
    let hidden_rows = session.screen().lines;

    session
        .new_tab_for_test(&["/bin/sh", "-c", "sleep 5"])
        .unwrap();
    assert!(
        wait_until(&mut session, Duration::from_secs(3), |session| {
            session.tab_count() == 2 && session.tab_bar_visible()
        }),
        "tab bar did not appear for two tabs"
    );
    assert!(session.tab_bar_height() > 0.0);
    session.render_now().expect("render with tab bar");
    let shown_size = session.window_size();
    assert_eq!(
        shown_size, hidden_size,
        "kitty keeps the OS window size and gives the bar a central row"
    );
    assert_eq!(
        session.screen().lines + 1,
        hidden_rows,
        "the central grid should lose one row to the tab bar"
    );
    session.shutdown();
    let _ = std::fs::remove_file(&config);
}

#[test]
fn session_restore_opens_tabs_and_layouts() {
    let _guard = test_lock();
    let Some(display) = TestDisplay::new() else {
        return;
    };
    let path = std::env::temp_dir().join(format!(
        "kitr-session-{}-{}.session",
        std::process::id(),
        Instant::now().elapsed().as_nanos()
    ));
    std::fs::write(
        &path,
        "new_tab alpha\n\
layout splits\n\
enabled_layouts splits\n\
launch --title=alpha /bin/sh -c \"printf alpha; sleep 20\"\n\
new_tab beta\n\
launch --title=beta /bin/sh -c \"printf beta; sleep 20\"\n\
focus\n",
    )
    .expect("write session");
    let opts = kitty::Options {
        size: Some((50, 10)),
        gui: kitty::GuiMode::Gui,
        session: Some(path.clone()),
        ..kitty::Options::default()
    };
    let mut session = match GuiSession::open_on_display(&opts, Input::None, Some(&display.name)) {
        Ok(session) => session,
        Err(error) => {
            let _ = std::fs::remove_file(&path);
            skip(&format!("EGL/window unavailable: {error}"));
            return;
        }
    };
    assert_eq!(session.tab_count(), 2, "session tabs were not restored");
    assert!(wait_until(
        &mut session,
        Duration::from_secs(5),
        |session| { dump(session).contains("beta") }
    ));
    let titles = session.tab_titles();
    assert!(
        titles.iter().any(|title| title.contains("alpha")),
        "{titles:?}"
    );
    assert!(
        titles.iter().any(|title| title.contains("beta")),
        "{titles:?}"
    );

    inject_key(&mut session, keysym::LEFT, true, true, false);
    assert!(wait_until(
        &mut session,
        Duration::from_secs(5),
        |session| { dump(session).contains("alpha") }
    ));

    let bad = std::env::temp_dir().join(format!("kitr-bad-{}.session", std::process::id()));
    std::fs::write(&bad, "not_a_session_command foo\n").unwrap();
    let bad_opts = kitty::Options {
        session: Some(bad.clone()),
        ..opts.clone()
    };
    let error = match GuiSession::open_on_display(&bad_opts, Input::None, Some(&display.name)) {
        Ok(mut session) => {
            session.shutdown();
            panic!("invalid session unexpectedly opened");
        }
        Err(error) => error.to_string(),
    };
    assert!(error.contains("session"), "unclear error: {error}");
    let _ = std::fs::remove_file(&bad);
    let _ = std::fs::remove_file(&path);
    session.shutdown();
}

#[test]
fn split_and_pane_focus() {
    let _guard = test_lock();
    let Some(display) = TestDisplay::new() else {
        return;
    };
    let opts = options(&["/bin/cat"], (60, 12));
    let mut session = match GuiSession::open_on_display(&opts, Input::None, Some(&display.name)) {
        Ok(session) => session,
        Err(error) => {
            skip(&format!("EGL/window unavailable: {error}"));
            return;
        }
    };
    let _ = session.step(Duration::from_millis(100));
    let first = session.active_window_id().expect("active window");
    inject_key(&mut session, keysym::RETURN, true, true, false);
    assert!(
        wait_until(&mut session, Duration::from_secs(5), |session| {
            session.pane_count() == 2
        }),
        "Ctrl+Shift+Enter did not split the tab"
    );
    let second = session.active_window_id().expect("active window");
    assert_ne!(first, second);
    let first_rect = session.pane_rect(first).expect("first pane rect");
    let second_rect = session.pane_rect(second).expect("second pane rect");
    assert!(first_rect.2 > 0 && first_rect.3 > 0);
    assert!(second_rect.2 > 0 && second_rect.3 > 0);
    assert!(
        first_rect.2 < 60,
        "existing pane was not relaid out: {first_rect:?}"
    );
    assert_ne!((first_rect.0, first_rect.1), (second_rect.0, second_rect.1));

    inject_key(&mut session, keysym::LEFT, true, false, true);
    assert!(
        wait_until(&mut session, Duration::from_secs(3), |session| {
            session.active_window_id() == Some(first)
        }),
        "Ctrl+Alt+Left did not focus the left pane"
    );
    session.render_now().expect("render split");
    let (_, _, pixels) = session.capture_rgba().expect("framebuffer");
    assert!(pixels.iter().any(|byte| *byte != 0));
    session.shutdown();
}

#[test]
fn remote_control_roundtrip_and_screenshot() {
    let _guard = test_lock();
    let Some(display) = TestDisplay::new() else {
        return;
    };
    let socket = std::env::temp_dir().join(format!(
        "kitr-rc-{}-{}.sock",
        std::process::id(),
        Instant::now().elapsed().as_nanos()
    ));
    let screenshot = std::env::temp_dir().join(format!(
        "kitr-rc-{}-{}.ppm",
        std::process::id(),
        Instant::now().elapsed().as_nanos()
    ));
    let listen = OsString::from(format!("unix:{}", socket.display()));
    let mut opts = options(&["/bin/sh", "-c", "printf 'remote-ok\n'; cat"], (40, 10));
    opts.listen_on = Some(listen);
    let mut session = match GuiSession::open_on_display(&opts, Input::None, Some(&display.name)) {
        Ok(session) => session,
        Err(error) => {
            skip(&format!("EGL/window unavailable: {error}"));
            return;
        }
    };
    assert!(wait_until(
        &mut session,
        Duration::from_secs(5),
        |session| { dump(session).contains("remote-ok") }
    ));

    let target = format!("--to=unix:{}", socket.display());
    let output = run_remote_while_stepping(
        &mut session,
        &["@", &target, "get-text"],
        Duration::from_secs(10),
    );
    assert!(output.status.success(), "{:?}", output);
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("remote-ok"),
        "get-text stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let output = run_remote_while_stepping(
        &mut session,
        &["@", &target, "send-text", "roundtrip\n"],
        Duration::from_secs(10),
    );
    assert!(output.status.success(), "{:?}", output);
    assert!(wait_until(
        &mut session,
        Duration::from_secs(5),
        |session| { dump(session).contains("roundtrip") }
    ));

    let shot_arg = format!("--output={}", screenshot.display());
    let output = run_remote_while_stepping(
        &mut session,
        &["@", &target, "screenshot", "--format=ppm", &shot_arg],
        Duration::from_secs(10),
    );
    assert!(output.status.success(), "{:?}", output);
    let (width, height, lit) = ppm_stats(&screenshot).expect("remote PPM screenshot");
    assert!(width > 0 && height > 0 && lit > 20);

    inject_key(&mut session, keysym::RETURN, true, true, false);
    assert!(
        wait_until(&mut session, Duration::from_secs(5), |session| {
            session.pane_count() == 2
        }),
        "split before remote resize failed"
    );
    let output = run_remote_while_stepping(
        &mut session,
        &[
            "@",
            &target,
            "resize-window",
            "--axis=horizontal",
            "--increment=2",
        ],
        Duration::from_secs(10),
    );
    assert!(output.status.success(), "{:?}", output);

    let _ = std::fs::remove_file(&screenshot);
    let _ = std::fs::remove_file(&socket);
    session.shutdown();
}

#[test]
fn remote_control_abstract_socket() {
    let _guard = test_lock();
    let Some(display) = TestDisplay::new() else {
        return;
    };
    let name = format!(
        "kitr-test-{}-{}",
        std::process::id(),
        Instant::now().elapsed().as_nanos()
    );
    let mut opts = options(
        &["/bin/sh", "-c", "printf 'abstract-ok'; sleep 20"],
        (30, 8),
    );
    opts.listen_on = Some(OsString::from(format!("unix:@{name}")));
    let mut session = match GuiSession::open_on_display(&opts, Input::None, Some(&display.name)) {
        Ok(session) => session,
        Err(error) => {
            skip(&format!("EGL/window unavailable: {error}"));
            return;
        }
    };
    assert!(wait_until(
        &mut session,
        Duration::from_secs(5),
        |session| { dump(session).contains("abstract-ok") }
    ));
    let target = format!("--to=unix:@{name}");
    let output = run_remote_while_stepping(
        &mut session,
        &["@", &target, "get-text"],
        Duration::from_secs(10),
    );
    assert!(output.status.success(), "{:?}", output);
    assert!(String::from_utf8_lossy(&output.stdout).contains("abstract-ok"));
    session.shutdown();
}

use std::os::fd::AsRawFd;

fn mouse_modifiers(shift: bool) -> Modifiers {
    let mut modifiers = Modifiers {
        shift,
        ..Modifiers::default()
    };
    modifiers.raw_state = u32::from(shift);
    modifiers
}

fn inject_mouse(
    session: &mut GuiSession,
    button: MouseButton,
    pressed: bool,
    x: f64,
    y: f64,
    shift: bool,
) {
    session
        .inject_event(&WindowEvent::MouseButton {
            button,
            pressed,
            x,
            y,
            modifiers: mouse_modifiers(shift),
        })
        .expect("inject mouse button");
    let _ = session.step(Duration::from_millis(5));
}

fn inject_move(session: &mut GuiSession, x: f64, y: f64, shift: bool) {
    session
        .inject_event(&WindowEvent::MouseMove {
            x,
            y,
            modifiers: mouse_modifiers(shift),
        })
        .expect("inject mouse move");
    let _ = session.step(Duration::from_millis(5));
}

/// Pixel centre of a cell relative to a pane's origin.
fn pane_cell_center(session: &GuiSession, origin: (u32, u32), column: u32, row: u32) -> (f64, f64) {
    let (cell_width, cell_height) = session.cell_size();
    (
        (origin.0 + column) as f64 * cell_width as f64 + cell_width as f64 / 2.0,
        (origin.1 + row) as f64 * cell_height as f64 + cell_height as f64 / 2.0,
    )
}

/// Pixel centre of a tab-bar cell.
fn tab_cell_center(session: &GuiSession, cell: u32) -> (f64, f64) {
    let (cell_width, cell_height) = session.cell_size();
    let bar_height = session.tab_bar_height() as f64;
    let y = if session.tab_bar_at_top() {
        bar_height / 2.0
    } else {
        session.window_size().1 as f64 - bar_height / 2.0
    };
    (
        cell as f64 * cell_width as f64 + cell_width as f64 / 2.0,
        y.max(cell_height as f64 / 2.0),
    )
}

#[test]
fn default_window_and_tab_titles_come_from_the_command() {
    let _guard = test_lock();
    let Some(display) = TestDisplay::new() else {
        return;
    };
    let opts = options(&["/bin/sh", "-c", "sleep 20"], (40, 8));
    let mut session = match GuiSession::open_on_display(&opts, Input::None, Some(&display.name)) {
        Ok(session) => session,
        Err(error) => {
            skip(&format!("EGL/window unavailable: {error}"));
            return;
        }
    };

    assert_eq!(session.tab_titles(), vec!["sh".to_string()]);
    assert_eq!(session.window_title(), "sh");
    session.shutdown();
}

#[test]
fn osc_title_updates_tab_and_window_title_and_empty_resets_it() {
    let _guard = test_lock();
    let Some(display) = TestDisplay::new() else {
        return;
    };
    let opts = options(
        &[
            "/bin/sh",
            "-c",
            "printf '\\033]2;build@example\\033\\\\'; sleep 0.4; printf '\\033]2;\\033\\\\'; sleep 20",
        ],
        (40, 8),
    );
    let mut session = match GuiSession::open_on_display(&opts, Input::None, Some(&display.name)) {
        Ok(session) => session,
        Err(error) => {
            skip(&format!("EGL/window unavailable: {error}"));
            return;
        }
    };

    assert!(
        wait_until(&mut session, Duration::from_secs(5), |session| {
            session
                .tab_titles()
                .iter()
                .any(|title| title == "build@example")
        }),
        "OSC title was not applied: {:?}",
        session.tab_titles()
    );
    assert_eq!(session.window_title(), "build@example");

    assert!(
        wait_until(&mut session, Duration::from_secs(5), |session| {
            session.tab_titles().iter().any(|title| title == "sh")
        }),
        "empty OSC title did not reset to the command title: {:?}",
        session.tab_titles()
    );
    assert_eq!(session.window_title(), "sh");
    session.shutdown();
}

#[test]
fn clicking_tabs_switches_the_active_tab() {
    let _guard = test_lock();
    let Some(display) = TestDisplay::new() else {
        return;
    };
    let opts = options(&["/bin/sh", "-c", "sleep 20"], (60, 10));
    let mut session = match GuiSession::open_on_display(&opts, Input::None, Some(&display.name)) {
        Ok(session) => session,
        Err(error) => {
            skip(&format!("EGL/window unavailable: {error}"));
            return;
        }
    };
    session
        .new_tab_for_test(&["/bin/sh", "-c", "sleep 20"])
        .expect("second tab");
    assert!(
        wait_until(&mut session, Duration::from_secs(3), |session| {
            session.tab_count() == 2 && session.tab_bar_visible()
        }),
        "tab bar did not appear"
    );
    session.render_now().expect("render tab bar");
    let ranges = session.tab_ranges().to_vec();
    assert_eq!(ranges.len(), 2, "expected two tab hit boxes: {ranges:?}");

    let (second_start, second_end) = (ranges[1].1, ranges[1].2);
    let (x, y) = tab_cell_center(&session, (second_start + second_end) / 2);
    inject_mouse(&mut session, MouseButton::Left, true, x, y, false);
    inject_mouse(&mut session, MouseButton::Left, false, x, y, false);
    assert_eq!(session.active_tab_index(), 1);

    let ranges = session.tab_ranges().to_vec();
    let (first_start, first_end) = (ranges[0].1, ranges[0].2);
    let first_cell = (first_start + first_end) / 2;
    let (x, y) = tab_cell_center(&session, first_cell);
    inject_mouse(&mut session, MouseButton::Left, true, x, y, false);
    inject_mouse(&mut session, MouseButton::Left, false, x, y, false);
    assert_eq!(
        session.active_tab_index(),
        0,
        "ranges={ranges:?} first_cell={first_cell} cell_size={:?}",
        session.cell_size()
    );
    session.shutdown();
}

#[test]
fn middle_click_on_a_tab_closes_it() {
    let _guard = test_lock();
    let Some(display) = TestDisplay::new() else {
        return;
    };
    let opts = options(&["/bin/sh", "-c", "sleep 20"], (60, 10));
    let mut session = match GuiSession::open_on_display(&opts, Input::None, Some(&display.name)) {
        Ok(session) => session,
        Err(error) => {
            skip(&format!("EGL/window unavailable: {error}"));
            return;
        }
    };
    session
        .new_tab_for_test(&["/bin/sh", "-c", "sleep 20"])
        .expect("second tab");
    assert!(wait_until(
        &mut session,
        Duration::from_secs(3),
        |session| { session.tab_count() == 2 && session.tab_bar_visible() }
    ));
    session.render_now().expect("render tab bar");
    let ranges = session.tab_ranges().to_vec();
    let (x, y) = tab_cell_center(&session, (ranges[0].1 + ranges[0].2) / 2);
    inject_mouse(&mut session, MouseButton::Middle, true, x, y, false);
    inject_mouse(&mut session, MouseButton::Middle, false, x, y, false);
    assert_eq!(session.tab_count(), 1);
    session.shutdown();
}

#[test]
fn dragging_selects_text_and_clicking_focuses_a_pane() {
    let _guard = test_lock();
    let Some(display) = TestDisplay::new() else {
        return;
    };
    let opts = options(
        &["/bin/sh", "-c", "printf 'hello-world\\n'; sleep 20"],
        (40, 8),
    );
    let mut session = match GuiSession::open_on_display(&opts, Input::None, Some(&display.name)) {
        Ok(session) => session,
        Err(error) => {
            skip(&format!("EGL/window unavailable: {error}"));
            return;
        }
    };
    assert!(wait_until(
        &mut session,
        Duration::from_secs(5),
        |session| { dump(session).contains("hello-world") }
    ));
    let wid = session.active_window_id().expect("active window");
    let rect = session.pane_rect(wid).expect("pane rect");
    let origin = (rect.0, rect.1);

    let (x0, y0) = pane_cell_center(&session, origin, 0, 0);
    let (x1, y1) = pane_cell_center(&session, origin, 4, 0);
    inject_mouse(&mut session, MouseButton::Left, true, x0, y0, false);
    inject_move(&mut session, x1, y1, false);
    inject_mouse(&mut session, MouseButton::Left, false, x1, y1, false);

    assert!(session.screen().has_selection(), "drag did not select text");
    let selected = session.screen().selected_text();
    assert!(
        selected.contains("hello"),
        "unexpected selection: {selected:?}"
    );

    // Clicking the other pane must focus it and keep the selection local.
    let first = wid;
    inject_key(&mut session, keysym::RETURN, true, true, false);
    assert!(wait_until(
        &mut session,
        Duration::from_secs(5),
        |session| { session.pane_count() == 2 }
    ));
    let second = session.active_window_id().expect("second window");
    assert_ne!(first, second);
    let first_rect = session.pane_rect(first).expect("first rect");
    let (fx, fy) = pane_cell_center(&session, (first_rect.0, first_rect.1), 1, 1);
    inject_mouse(&mut session, MouseButton::Left, true, fx, fy, false);
    inject_mouse(&mut session, MouseButton::Left, false, fx, fy, false);
    assert_eq!(session.active_window_id(), Some(first));
    session.shutdown();
}

#[test]
fn shift_drag_selects_even_when_the_child_tracks_the_mouse() {
    let _guard = test_lock();
    let Some(display) = TestDisplay::new() else {
        return;
    };
    let opts = options(
        &[
            "/bin/sh",
            "-c",
            "printf 'pick-me\\n'; printf '\\033[?1000h\\033[?1006h'; sleep 20",
        ],
        (40, 8),
    );
    let mut session = match GuiSession::open_on_display(&opts, Input::None, Some(&display.name)) {
        Ok(session) => session,
        Err(error) => {
            skip(&format!("EGL/window unavailable: {error}"));
            return;
        }
    };
    assert!(wait_until(
        &mut session,
        Duration::from_secs(5),
        |session| { dump(session).contains("pick-me") }
    ));
    let wid = session.active_window_id().expect("active window");
    let rect = session.pane_rect(wid).expect("pane rect");
    let origin = (rect.0, rect.1);
    let (x0, y0) = pane_cell_center(&session, origin, 0, 0);
    let (x1, y1) = pane_cell_center(&session, origin, 3, 0);

    inject_mouse(&mut session, MouseButton::Left, true, x0, y0, true);
    inject_move(&mut session, x1, y1, true);
    inject_mouse(&mut session, MouseButton::Left, false, x1, y1, true);

    assert!(
        session.screen().has_selection(),
        "shift-drag did not select text while mouse tracking was enabled"
    );
    assert!(session.screen().selected_text().contains("pick"));
    session.shutdown();
}

#[test]
fn osc_titles_are_sanitized_to_one_line() {
    let _guard = test_lock();
    let Some(display) = TestDisplay::new() else {
        return;
    };
    let opts = options(
        &[
            "/bin/sh",
            "-c",
            "printf '\\033]2;build\\nhost\\007'; sleep 20",
        ],
        (40, 8),
    );
    let mut session = match GuiSession::open_on_display(&opts, Input::None, Some(&display.name)) {
        Ok(session) => session,
        Err(error) => {
            skip(&format!("EGL/window unavailable: {error}"));
            return;
        }
    };
    assert!(
        wait_until(&mut session, Duration::from_secs(5), |session| {
            session
                .tab_titles()
                .iter()
                .any(|title| title == "build host")
        }),
        "title was not sanitized: {:?}",
        session.tab_titles()
    );
    assert_eq!(session.window_title(), "build host");
    session.shutdown();
}

#[test]
fn switching_tabs_updates_the_window_title() {
    let _guard = test_lock();
    let Some(display) = TestDisplay::new() else {
        return;
    };
    let opts = options(
        &["/bin/sh", "-c", "printf '\\033]2;first-tab\\007'; sleep 20"],
        (40, 8),
    );
    let mut session = match GuiSession::open_on_display(&opts, Input::None, Some(&display.name)) {
        Ok(session) => session,
        Err(error) => {
            skip(&format!("EGL/window unavailable: {error}"));
            return;
        }
    };
    assert!(wait_until(
        &mut session,
        Duration::from_secs(5),
        |session| { session.window_title() == "first-tab" }
    ));
    session
        .new_tab_for_test(&[
            "/bin/sh",
            "-c",
            "printf '\\033]2;second-tab\\007'; sleep 20",
        ])
        .expect("second tab");
    assert!(wait_until(
        &mut session,
        Duration::from_secs(5),
        |session| { session.window_title() == "second-tab" }
    ));

    assert!(session.select_tab(0));
    assert!(wait_until(
        &mut session,
        Duration::from_secs(3),
        |session| { session.window_title() == "first-tab" }
    ));
    session.shutdown();
}

#[test]
fn double_click_selects_a_word_and_triple_click_selects_a_line() {
    let _guard = test_lock();
    let Some(display) = TestDisplay::new() else {
        return;
    };
    let opts = options(
        &["/bin/sh", "-c", "printf 'alpha beta gamma\\n'; sleep 20"],
        (40, 8),
    );
    let mut session = match GuiSession::open_on_display(&opts, Input::None, Some(&display.name)) {
        Ok(session) => session,
        Err(error) => {
            skip(&format!("EGL/window unavailable: {error}"));
            return;
        }
    };
    assert!(wait_until(
        &mut session,
        Duration::from_secs(5),
        |session| { dump(session).contains("alpha beta gamma") }
    ));
    let wid = session.active_window_id().expect("active window");
    let rect = session.pane_rect(wid).expect("pane rect");
    let origin = (rect.0, rect.1);

    let (bx, by) = pane_cell_center(&session, origin, 7, 0);
    inject_mouse(&mut session, MouseButton::Left, true, bx, by, false);
    inject_mouse(&mut session, MouseButton::Left, false, bx, by, false);
    inject_mouse(&mut session, MouseButton::Left, true, bx, by, false);
    inject_mouse(&mut session, MouseButton::Left, false, bx, by, false);
    assert_eq!(session.screen().selected_text(), "beta");

    let (gx, gy) = pane_cell_center(&session, origin, 12, 0);
    inject_mouse(&mut session, MouseButton::Left, true, gx, gy, false);
    inject_mouse(&mut session, MouseButton::Left, false, gx, gy, false);
    inject_mouse(&mut session, MouseButton::Left, true, gx, gy, false);
    inject_mouse(&mut session, MouseButton::Left, false, gx, gy, false);
    inject_mouse(&mut session, MouseButton::Left, true, gx, gy, false);
    inject_mouse(&mut session, MouseButton::Left, false, gx, gy, false);
    let selected = session.screen().selected_text();
    assert_eq!(selected, "alpha beta gamma");
    session.shutdown();
}

#[test]
fn selection_is_rendered_and_can_be_copied() {
    let _guard = test_lock();
    let Some(display) = TestDisplay::new() else {
        return;
    };
    let opts = options(
        &["/bin/sh", "-c", "printf 'copy-me-now\\n'; sleep 20"],
        (40, 8),
    );
    let mut session = match GuiSession::open_on_display(&opts, Input::None, Some(&display.name)) {
        Ok(session) => session,
        Err(error) => {
            skip(&format!("EGL/window unavailable: {error}"));
            return;
        }
    };
    assert!(wait_until(
        &mut session,
        Duration::from_secs(5),
        |session| { dump(session).contains("copy-me-now") }
    ));
    session.render_now().expect("initial render");
    let (_, _, before) = session.capture_rgba().expect("before");
    let wid = session.active_window_id().expect("active window");
    let rect = session.pane_rect(wid).expect("pane rect");
    let origin = (rect.0, rect.1);
    let (x0, y0) = pane_cell_center(&session, origin, 0, 0);
    let (x1, y1) = pane_cell_center(&session, origin, 6, 0);
    inject_mouse(&mut session, MouseButton::Left, true, x0, y0, false);
    inject_move(&mut session, x1, y1, false);
    inject_mouse(&mut session, MouseButton::Left, false, x1, y1, false);
    assert!(session.screen().has_selection());
    session.render_now().expect("selection render");
    let (_, _, after) = session.capture_rgba().expect("after");

    let changed = before
        .as_chunks::<4>()
        .0
        .iter()
        .zip(after.as_chunks::<4>().0)
        .filter(|(a, b)| a != b)
        .count();
    assert!(changed > 10, "selection is not visible ({changed} pixels)");

    inject_key(&mut session, b'c' as u32, true, true, false);
    assert!(
        session.screen().has_selection(),
        "copy cleared the selection"
    );
    let mut copied = String::new();
    for _ in 0..20 {
        let _ = session.step(Duration::from_millis(10));
        copied = session.clipboard_text().unwrap_or_default();
        if copied.contains("copy-me") {
            break;
        }
    }
    assert!(copied.contains("copy-me"), "clipboard={copied:?}");
    session.shutdown();
}

#[test]
fn clicking_inside_a_tab_selects_it_at_every_cell() {
    let _guard = test_lock();
    let Some(display) = TestDisplay::new() else {
        return;
    };
    let opts = options(&["/bin/sh", "-c", "sleep 20"], (60, 10));
    let mut session = match GuiSession::open_on_display(&opts, Input::None, Some(&display.name)) {
        Ok(session) => session,
        Err(error) => {
            skip(&format!("EGL/window unavailable: {error}"));
            return;
        }
    };
    session
        .new_tab_for_test(&["/bin/sh", "-c", "sleep 20"])
        .expect("second tab");
    assert!(wait_until(
        &mut session,
        Duration::from_secs(3),
        |session| { session.tab_count() == 2 && session.tab_bar_visible() }
    ));
    session.render_now().expect("render tab bar");
    let ranges = session.tab_ranges().to_vec();
    assert_eq!(ranges.len(), 2);

    for (expected, (_, start, end)) in ranges.iter().enumerate() {
        for cell in [*start, end - 1] {
            session.select_tab(1 - expected);
            let (x, y) = tab_cell_center(&session, cell);
            inject_mouse(&mut session, MouseButton::Left, true, x, y, false);
            inject_mouse(&mut session, MouseButton::Left, false, x, y, false);
            assert_eq!(
                session.active_tab_index(),
                expected,
                "cell {cell} in {start}..{end}"
            );
        }
    }
    session.shutdown();
}

fn open_with_config(display: &str, config_text: &str, size: (u16, u16)) -> Option<GuiSession> {
    let config = std::env::temp_dir().join(format!(
        "kitr-tabs-{}-{}.conf",
        std::process::id(),
        Instant::now().elapsed().as_nanos()
    ));
    std::fs::write(&config, config_text).ok()?;
    let opts = kitty::Options {
        size: Some(size),
        gui: kitty::GuiMode::Gui,
        config: Some(config.clone().into_os_string()),
        command: vec!["/bin/sh".into(), "-c".into(), "sleep 20".into()],
        ..kitty::Options::default()
    };
    let session = GuiSession::open_on_display(&opts, Input::None, Some(display)).ok();
    let _ = std::fs::remove_file(&config);
    session
}

#[test]
fn clicking_tabs_works_with_a_top_tab_bar() {
    let _guard = test_lock();
    let Some(display) = TestDisplay::new() else {
        return;
    };
    let Some(mut session) = open_with_config(
        &display.name,
        "tab_bar_edge top\ntab_bar_min_tabs 1\ntab_bar_style powerline\n",
        (60, 10),
    ) else {
        skip("EGL/window unavailable");
        return;
    };
    session
        .new_tab_for_test(&["/bin/sh", "-c", "sleep 20"])
        .expect("second tab");
    assert!(wait_until(
        &mut session,
        Duration::from_secs(3),
        |session| {
            session.tab_count() == 2 && session.tab_bar_visible() && session.tab_bar_at_top()
        }
    ));
    session.render_now().expect("render tab bar");
    let ranges = session.tab_ranges().to_vec();
    let (x, y) = tab_cell_center(&session, (ranges[1].1 + ranges[1].2) / 2);
    inject_mouse(&mut session, MouseButton::Left, true, x, y, false);
    inject_mouse(&mut session, MouseButton::Left, false, x, y, false);
    assert_eq!(session.active_tab_index(), 1);
    session.shutdown();
}

#[test]
fn clicking_empty_tab_bar_space_keeps_the_active_tab() {
    let _guard = test_lock();
    let Some(display) = TestDisplay::new() else {
        return;
    };
    let opts = options(&["/bin/sh", "-c", "sleep 20"], (60, 10));
    let mut session = match GuiSession::open_on_display(&opts, Input::None, Some(&display.name)) {
        Ok(session) => session,
        Err(error) => {
            skip(&format!("EGL/window unavailable: {error}"));
            return;
        }
    };
    session
        .new_tab_for_test(&["/bin/sh", "-c", "sleep 20"])
        .expect("second tab");
    assert!(wait_until(
        &mut session,
        Duration::from_secs(3),
        |session| { session.tab_count() == 2 && session.tab_bar_visible() }
    ));
    session.render_now().expect("render tab bar");
    let ranges = session.tab_ranges().to_vec();
    assert_eq!(session.active_tab_index(), 1);
    let (x, y) = tab_cell_center(&session, ranges[1].2 + 10);
    inject_mouse(&mut session, MouseButton::Left, true, x, y, false);
    inject_mouse(&mut session, MouseButton::Left, false, x, y, false);
    assert_eq!(session.active_tab_index(), 1);
    session.shutdown();
}

#[test]
fn hidden_tab_bar_lets_the_pane_receive_the_click() {
    let _guard = test_lock();
    let Some(display) = TestDisplay::new() else {
        return;
    };
    let opts = options(
        &["/bin/sh", "-c", "printf 'top-line\\n'; sleep 20"],
        (40, 8),
    );
    let mut session = match GuiSession::open_on_display(&opts, Input::None, Some(&display.name)) {
        Ok(session) => session,
        Err(error) => {
            skip(&format!("EGL/window unavailable: {error}"));
            return;
        }
    };
    assert!(wait_until(
        &mut session,
        Duration::from_secs(5),
        |session| { dump(session).contains("top-line") }
    ));
    assert!(!session.tab_bar_visible());
    let wid = session.active_window_id().expect("active window");
    let rect = session.pane_rect(wid).expect("pane rect");
    let (x0, y0) = pane_cell_center(&session, (rect.0, rect.1), 0, 0);
    let (x1, y1) = pane_cell_center(&session, (rect.0, rect.1), 3, 0);
    inject_mouse(&mut session, MouseButton::Left, true, x0, y0, false);
    inject_move(&mut session, x1, y1, false);
    inject_mouse(&mut session, MouseButton::Left, false, x1, y1, false);
    assert!(session.screen().has_selection());
    assert_eq!(session.screen().selected_text(), "top-");
    session.shutdown();
}

#[test]
fn exiting_a_tab_keeps_the_other_tabs_alive() {
    let _guard = test_lock();
    let Some(display) = TestDisplay::new() else {
        return;
    };
    let opts = options(&["/bin/sh", "-c", "printf 'keeper\\n'; sleep 20"], (50, 10));
    let mut session = match GuiSession::open_on_display(&opts, Input::None, Some(&display.name)) {
        Ok(session) => session,
        Err(error) => {
            skip(&format!("EGL/window unavailable: {error}"));
            return;
        }
    };
    assert!(wait_until(
        &mut session,
        Duration::from_secs(5),
        |session| { dump(session).contains("keeper") }
    ));

    // A second tab whose child exits almost immediately.
    session
        .new_tab_for_test(&["/bin/sh", "-c", "sleep 0.05"])
        .expect("second tab");
    assert_eq!(session.tab_count(), 2);
    assert_eq!(session.active_tab_index(), 1);
    assert!(
        wait_until(&mut session, Duration::from_secs(5), |session| {
            session.tab_count() == 1
        }),
        "exiting tab was not closed"
    );
    assert_eq!(session.active_tab_index(), 0);
    assert!(
        wait_until(&mut session, Duration::from_secs(3), |session| {
            dump(session).contains("keeper")
        }),
        "remaining tab was disturbed"
    );
    session.shutdown();
}

#[test]
fn exiting_a_background_tab_keeps_the_active_tab() {
    let _guard = test_lock();
    let Some(display) = TestDisplay::new() else {
        return;
    };
    let opts = options(&["/bin/sh", "-c", "printf 'active\\n'; sleep 20"], (50, 10));
    let mut session = match GuiSession::open_on_display(&opts, Input::None, Some(&display.name)) {
        Ok(session) => session,
        Err(error) => {
            skip(&format!("EGL/window unavailable: {error}"));
            return;
        }
    };
    assert!(wait_until(
        &mut session,
        Duration::from_secs(5),
        |session| { dump(session).contains("active") }
    ));
    session
        .new_tab_for_test(&["/bin/sh", "-c", "sleep 0.05"])
        .expect("background tab");
    session
        .new_tab_for_test(&["/bin/sh", "-c", "printf 'front\\n'; sleep 20"])
        .expect("front tab");
    assert!(wait_until(
        &mut session,
        Duration::from_secs(5),
        |session| { dump(session).contains("front") }
    ));
    let active = session.active_tab_id();
    assert!(
        wait_until(&mut session, Duration::from_secs(5), |session| {
            session.tab_count() == 2
        }),
        "background tab was not closed"
    );
    assert_eq!(session.active_tab_id(), active);
    assert!(dump(&session).contains("front"));
    session.shutdown();
}

#[test]
fn exiting_the_last_tab_ends_the_session() {
    let _guard = test_lock();
    let Some(display) = TestDisplay::new() else {
        return;
    };
    let opts = options(&["/bin/sh", "-c", "sleep 0.05"], (40, 8));
    let mut session = match GuiSession::open_on_display(&opts, Input::None, Some(&display.name)) {
        Ok(session) => session,
        Err(error) => {
            skip(&format!("EGL/window unavailable: {error}"));
            return;
        }
    };
    let start = Instant::now();
    loop {
        if !session.step(Duration::from_millis(20)).expect("step") {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "session did not end after its last tab exited"
        );
    }
    assert_eq!(session.tab_count(), 1);
    session.shutdown();
}
