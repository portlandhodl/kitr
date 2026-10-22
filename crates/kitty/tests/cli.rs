// SPDX-License-Identifier: GPL-3.0-or-later
#![cfg(target_os = "linux")]

mod common;

use std::io::Write;
use std::os::fd::AsRawFd;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use common::{run_kitr, stdout_text, wait_child_timeout};
use kitty::Input;
use kitty_os::child::ExitStatus;
use kitty_os::pty;

fn base_args<'a>(size: &'a str, script: &'a str) -> Vec<&'a str> {
    vec![
        "--headless",
        "--size",
        size,
        "--dump-screen",
        "--timeout",
        "15",
        "--",
        "/bin/sh",
        "-c",
        script,
    ]
}

#[test]
fn printf_is_dumped_and_exit_code_is_reported() {
    let output = run_kitr(
        &base_args("20x5", "printf hello"),
        b"",
        Duration::from_secs(10),
    );
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout_text(&output), "hello");
    assert!(output.stderr.is_empty());
}

#[test]
fn nonzero_child_exit_status_is_propagated() {
    let output = run_kitr(&base_args("20x5", "exit 3"), b"", Duration::from_secs(10));
    assert_eq!(output.status.code(), Some(3));
}

#[test]
fn sgr_colors_are_stripped_from_text_dump() {
    let output = run_kitr(
        &base_args("30x5", "printf '\\033[31mred\\033[0m plain'"),
        b"",
        Duration::from_secs(10),
    );
    assert_eq!(output.status.code(), Some(0));
    let text = stdout_text(&output);
    assert_eq!(text, "red plain");
    assert!(!text.contains('\u{1b}'));
}

#[test]
fn separator_and_dash_c_shortcut_both_work() {
    let output = run_kitr(
        &base_args("20x5", "printf passed"),
        b"",
        Duration::from_secs(10),
    );
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout_text(&output), "passed");

    let output = run_kitr(
        &[
            "--size",
            "20x5",
            "--dump-screen",
            "--timeout",
            "10",
            "-c",
            "printf shortcut",
        ],
        b"",
        Duration::from_secs(10),
    );
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(stdout_text(&output), "shortcut");
}

#[test]
fn stdin_pipe_is_forwarded_to_the_child() {
    let output = run_kitr(
        &[
            "--size",
            "20x6",
            "--dump-screen",
            "--timeout",
            "10",
            "--",
            "/bin/cat",
        ],
        b"hello\nworld\n",
        Duration::from_secs(10),
    );
    assert_eq!(output.status.code(), Some(0));
    let text = stdout_text(&output);
    assert!(text.contains("hello"), "dump was {text:?}");
    assert!(text.contains("world"), "dump was {text:?}");
}

#[test]
fn large_output_is_drained_without_hanging() {
    let started = Instant::now();
    let output = run_kitr(
        &[
            "--size",
            "20x10",
            "--dump-screen",
            "--timeout",
            "20",
            "--",
            "/bin/sh",
            "-c",
            "yes x | head -n 50000",
        ],
        b"",
        Duration::from_secs(30),
    );
    assert_eq!(output.status.code(), Some(0));
    assert!(started.elapsed() < Duration::from_secs(25));
    let text = stdout_text(&output);
    assert!(!text.is_empty());
    assert!(text.lines().all(|line| line == "x"), "dump was {text:?}");
}

#[test]
fn timeout_kills_a_running_child() {
    let started = Instant::now();
    let output = run_kitr(
        &[
            "--headless",
            "--size",
            "20x5",
            "--timeout",
            "0.25",
            "--",
            "/bin/sh",
            "-c",
            "sleep 30",
        ],
        b"",
        Duration::from_secs(10),
    );
    assert_eq!(output.status.code(), Some(124));
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn help_and_version_do_not_spawn_a_child() {
    let output = run_kitr(&["--help"], b"", Duration::from_secs(5));
    assert_eq!(output.status.code(), Some(0));
    assert!(stdout_text(&output).contains("Usage: kitr"));

    let output = run_kitr(&["--version"], b"", Duration::from_secs(5));
    assert_eq!(output.status.code(), Some(0));
    assert!(stdout_text(&output).contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn interactive_cat_roundtrip_over_a_tty() {
    let pair = pty::open_pty().expect("pty");
    pty::set_window_size(pair.master_fd(), pty::WindowSize::new(5, 20)).unwrap();
    let check_fd = pair.slave().try_clone().unwrap();
    let original = pty::get_termios(check_fd.as_raw_fd()).unwrap();

    let (master, slave) = pair.into_fds();
    let mut master = std::fs::File::from(master);
    let mut child = Command::new(env!("CARGO_BIN_EXE_kitr"))
        .args([
            "--size",
            "20x5",
            "--dump-screen",
            "--timeout",
            "10",
            "--",
            "/bin/cat",
        ])
        .stdin(Stdio::from(std::fs::File::from(slave)))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn kitr on a tty");
    std::thread::sleep(Duration::from_millis(200));
    master.write_all(b"hello\n").unwrap();
    master.flush().unwrap();
    std::thread::sleep(Duration::from_millis(150));
    master.write_all(&[4u8]).unwrap();
    master.flush().unwrap();

    let status = wait_child_timeout(&mut child, Duration::from_secs(10));
    assert_eq!(status.code(), Some(0));
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

#[test]
fn screen_helpers_cover_cursor_scrolling_and_resize() {
    let mut options = kitty::Options {
        size: Some((12, 5)),
        scrollback: 20,
        ..kitty::Options::default()
    };
    options.command = ["/bin/sh", "-c", "printf 'abc\\033[2;5HX'"]
        .iter()
        .map(Into::into)
        .collect();
    let outcome = kitty::run_options(&options, Input::None).unwrap();
    assert_eq!(outcome.exit, ExitStatus::Exited(0));
    assert_eq!(outcome.screen.cursor_position_1_based(), (2, 6));
    assert_eq!(outcome.screen.line_text(0), "abc");
    assert_eq!(outcome.screen.line_text(1).trim_end(), "    X");

    options.command = ["/bin/sh", "-c", "seq 1 30"]
        .iter()
        .map(Into::into)
        .collect();
    let outcome = kitty::run_options(&options, Input::None).unwrap();
    assert!(outcome.screen.historybuf().count > 0);
    let last = (0..outcome.screen.lines)
        .map(|y| outcome.screen.line_text(y))
        .rev()
        .find(|line| !line.is_empty())
        .unwrap();
    assert_eq!(last, "30");

    let resized = kitty::resize_keep_text(&outcome.screen, 24, 8, 20).expect("resize helper");
    assert_eq!(resized.columns, 24);
    assert_eq!(resized.lines, 8);
}
