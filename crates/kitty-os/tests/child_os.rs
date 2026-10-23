// SPDX-License-Identifier: GPL-3.0-or-later
// Integration tests for the kitty child/PTY/event-loop port.
//
// These tests really fork. Every wait is bounded and every spawned child is
// either reaped or killed before the test returns.

#![cfg(target_os = "linux")]

use std::io;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kitty_os::child::{decode_wait_status, ExecStage, ExecStatus, ExitStatus};
use kitty_os::loop_utils::{poll, read_once, read_retry, write_once, IoOutcome, PollFd, SelfPipe};
use kitty_os::pty::{
    enter_raw_mode, get_termios, is_cloexec, is_tty, open_pty, resize_pty, restore_termios,
    set_cloexec, set_window_size, window_size, WindowSize, ECHO, ICANON,
};
use kitty_os::{spawn, ChildMonitor, MonitorOutcome, SpawnConfig};

const SH: &str = "/bin/sh";
const EIO: i32 = 5;

// The SIGCHLD self-pipe is process-wide, so monitor tests are serialized.
static MONITOR_LOCK: Mutex<()> = Mutex::new(());

fn monitor_guard() -> std::sync::MutexGuard<'static, ()> {
    MONITOR_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn deadline_ms(start: Instant, timeout: Duration) -> i32 {
    let remaining = timeout.saturating_sub(start.elapsed());
    let ns = remaining.as_nanos().min(i32::MAX as u128 * 1_000_000) as i64;
    ((ns + 999_999) / 1_000_000) as i32
}

fn read_until_eof(fd: i32, timeout: Duration) -> Vec<u8> {
    let start = Instant::now();
    let mut out = Vec::new();
    loop {
        let ms = deadline_ms(start, timeout);
        if ms == 0 {
            return out;
        }
        let mut fds = [PollFd::new(fd, kitty_os::loop_utils::POLLIN)];
        let ready = poll(&mut fds, ms).expect("poll master");
        if ready == 0 {
            return out;
        }
        loop {
            let mut buf = [0u8; 64 * 1024];
            match read_once(fd, &mut buf) {
                Ok(IoOutcome::Completed(0)) => return out,
                Ok(IoOutcome::Completed(n)) => out.extend_from_slice(&buf[..n]),
                Ok(IoOutcome::WouldBlock) => break,
                Ok(IoOutcome::Interrupted) => continue,
                Err(err) if err.raw_os_error() == Some(EIO) => return out,
                Err(err) => panic!("read master: {err}"),
            }
        }
    }
}

fn read_available(fd: i32, timeout: Duration) -> Vec<u8> {
    let start = Instant::now();
    let mut out = Vec::new();
    loop {
        let ms = deadline_ms(start, timeout);
        if ms == 0 {
            return out;
        }
        let mut fds = [PollFd::new(fd, kitty_os::loop_utils::POLLIN)];
        if poll(&mut fds, ms).expect("poll") == 0 {
            continue;
        }
        loop {
            let mut buf = [0u8; 64 * 1024];
            match read_once(fd, &mut buf) {
                Ok(IoOutcome::Completed(0)) => return out,
                Ok(IoOutcome::Completed(n)) => out.extend_from_slice(&buf[..n]),
                Ok(IoOutcome::WouldBlock) => break,
                Ok(IoOutcome::Interrupted) => continue,
                Err(err) if err.raw_os_error() == Some(EIO) => return out,
                Err(err) => panic!("read: {err}"),
            }
        }
        if !out.is_empty() {
            return out;
        }
    }
}

fn read_until_contains(fd: i32, needle: &[u8], timeout: Duration) -> Vec<u8> {
    let start = Instant::now();
    let mut out = Vec::new();
    loop {
        if out.windows(needle.len().max(1)).any(|w| w == needle) {
            return out;
        }
        let ms = deadline_ms(start, timeout);
        if ms == 0 {
            return out;
        }
        let mut fds = [PollFd::new(fd, kitty_os::loop_utils::POLLIN)];
        if poll(&mut fds, ms).expect("poll") == 0 {
            continue;
        }
        loop {
            let mut buf = [0u8; 4096];
            match read_once(fd, &mut buf) {
                Ok(IoOutcome::Completed(0)) => return out,
                Ok(IoOutcome::Completed(n)) => out.extend_from_slice(&buf[..n]),
                Ok(IoOutcome::WouldBlock) => break,
                Ok(IoOutcome::Interrupted) => continue,
                Err(err) if err.raw_os_error() == Some(EIO) => return out,
                Err(err) => panic!("read: {err}"),
            }
        }
    }
}

fn sh(script: &str) -> SpawnConfig {
    SpawnConfig::new(SH)
        .arg("-c")
        .arg(script)
        .env("PATH", "/usr/bin:/bin")
        .env("LC_ALL", "C")
}

// ---------------------------------------------------------------------------
// pty.rs
// ---------------------------------------------------------------------------

#[test]
fn pty_pair_is_a_terminal_and_cloexec_flags_match_openpty() {
    let pair = open_pty().expect("open pty");
    assert!(is_tty(pair.master_fd()));
    assert!(is_tty(pair.slave_fd()));
    assert!(is_cloexec(pair.master_fd()).unwrap());
    assert!(!is_cloexec(pair.slave_fd()).unwrap());
}

#[test]
fn pty_window_size_round_trips() {
    let pair = open_pty().unwrap();
    let requested = WindowSize {
        rows: 37,
        columns: 111,
        xpixel: 1110,
        ypixel: 740,
    };
    set_window_size(pair.master_fd(), requested).unwrap();
    assert_eq!(window_size(pair.master_fd()).unwrap(), requested);
    assert_eq!(window_size(pair.slave_fd()).unwrap(), requested);
}

#[test]
fn pty_pairs_are_independent() {
    let a = open_pty().unwrap();
    let b = open_pty().unwrap();
    assert_ne!(a.master_fd(), b.master_fd());
    set_window_size(a.master_fd(), WindowSize::new(10, 20)).unwrap();
    set_window_size(b.master_fd(), WindowSize::new(30, 40)).unwrap();
    assert_eq!(window_size(a.slave_fd()).unwrap(), WindowSize::new(10, 20));
    assert_eq!(window_size(b.slave_fd()).unwrap(), WindowSize::new(30, 40));
}

#[test]
fn termios_raw_mode_round_trips() {
    let pair = open_pty().unwrap();
    let original = get_termios(pair.master_fd()).unwrap();
    let saved = enter_raw_mode(pair.master_fd()).unwrap();
    let raw = get_termios(pair.master_fd()).unwrap();
    assert_eq!(raw.c_lflag & ECHO, 0);
    assert_eq!(raw.c_lflag & ICANON, 0);
    restore_termios(pair.master_fd(), &saved).unwrap();
    let restored = get_termios(pair.master_fd()).unwrap();
    assert_eq!(restored.c_lflag & ECHO, original.c_lflag & ECHO);
    assert_eq!(restored.c_lflag & ICANON, original.c_lflag & ICANON);
}

#[test]
fn resize_pty_ignores_bad_terminal_like_the_c_code() {
    let pair = open_pty().unwrap();
    resize_pty(pair.master_fd(), WindowSize::new(5, 6)).unwrap();
    assert_eq!(
        window_size(pair.master_fd()).unwrap(),
        WindowSize::new(5, 6)
    );
}

#[test]
fn pty_nonblocking_and_cloexec_helpers() {
    let pipe = SelfPipe::new(false).unwrap();
    assert!(is_cloexec(pipe.read_fd()).unwrap());
    set_cloexec(pipe.read_fd(), false).unwrap();
    assert!(!is_cloexec(pipe.read_fd()).unwrap());
    kitty_os::set_nonblocking(pipe.read_fd(), true).unwrap();
    let mut buf = [0u8; 1];
    assert_eq!(
        read_once(pipe.read_fd(), &mut buf).unwrap(),
        IoOutcome::WouldBlock
    );
}

// ---------------------------------------------------------------------------
// child.rs
// ---------------------------------------------------------------------------

#[test]
fn spawn_printf_reads_output_until_eof_and_exits_zero() {
    let mut child = spawn(&sh("printf hello")).expect("spawn");
    let output = read_until_eof(child.master_fd(), Duration::from_secs(5));
    assert_eq!(output, b"hello");
    assert_eq!(
        child.wait(Some(Duration::from_secs(5))).unwrap(),
        ExitStatus::Exited(0)
    );
}

#[test]
fn exec_failure_is_reported_through_the_error_pipe() {
    let config = SpawnConfig::new("/definitely/does/not/exist/kitty-test");
    let mut child = spawn(&config).expect("fork succeeds");
    let status = child
        .wait_for_exec(Some(Duration::from_secs(5)))
        .expect("wait for exec");
    match status {
        ExecStatus::Failed(failure) => {
            assert_eq!(failure.stage, ExecStage::Exec);
            assert_eq!(failure.errno, 2);
            assert!(failure.to_string().contains("Failed to launch child"));
        }
        other => panic!("expected exec failure, got {other:?}"),
    }
    assert_eq!(
        child.wait(Some(Duration::from_secs(5))).unwrap(),
        ExitStatus::Exited(127)
    );
}

#[test]
fn exec_success_is_detected_by_eof_on_the_error_pipe() {
    let mut child = spawn(&sh("exit 0")).expect("spawn");
    assert_eq!(
        child.wait_for_exec(Some(Duration::from_secs(5))).unwrap(),
        ExecStatus::Succeeded
    );
    child.wait(Some(Duration::from_secs(5))).unwrap();
}

#[test]
fn argv0_and_environment_are_passed_verbatim() {
    let config = SpawnConfig {
        exe: SH.into(),
        argv: vec![
            "custom-argv0".into(),
            "-c".into(),
            "printf '%s|%s' \"$0\" \"$FOO\"".into(),
        ],
        ..SpawnConfig::new(SH).env("FOO", "bar")
    };
    let mut child = spawn(&config).unwrap();
    let output = read_until_eof(child.master_fd(), Duration::from_secs(5));
    assert_eq!(output, b"custom-argv0|bar");
    child.wait(Some(Duration::from_secs(5))).unwrap();
}

#[test]
fn cwd_is_applied() {
    let mut child = spawn(&sh("pwd").current_dir("/tmp")).unwrap();
    let output = read_until_eof(child.master_fd(), Duration::from_secs(5));
    assert_eq!(output, b"/tmp\r\n");
    child.wait(Some(Duration::from_secs(5))).unwrap();
}

#[test]
fn stdin_pipe_is_connected_to_fd0() {
    let mut child = spawn(&sh("read x; printf '<%s>' \"$x\"").stdin_pipe(true)).unwrap();
    assert!(child.stdin_write_fd().is_some());
    let written = child.write_stdin(b"abc\n").unwrap();
    assert_eq!(written, 4);
    let output = read_until_eof(child.master_fd(), Duration::from_secs(5));
    assert_eq!(output, b"<abc>");
    child.wait(Some(Duration::from_secs(5))).unwrap();
}

#[test]
fn non_zero_exit_code_is_decoded() {
    let mut child = spawn(&sh("exit 7")).unwrap();
    assert_eq!(
        child.wait(Some(Duration::from_secs(5))).unwrap(),
        ExitStatus::Exited(7)
    );
}

#[test]
fn death_by_signal_is_decoded() {
    let mut child = spawn(&sh("kill -TERM $$")).unwrap();
    assert_eq!(
        child.wait(Some(Duration::from_secs(5))).unwrap(),
        ExitStatus::Signaled(15)
    );
}

#[test]
fn child_can_be_killed_with_signal_and_signal_group() {
    let mut child = spawn(&sh("sleep 30")).unwrap();
    std::thread::sleep(Duration::from_millis(50));
    child.signal(15).unwrap();
    assert_eq!(
        child.wait(Some(Duration::from_secs(5))).unwrap(),
        ExitStatus::Signaled(15)
    );

    let mut child = spawn(&sh("sleep 30")).unwrap();
    std::thread::sleep(Duration::from_millis(50));
    child.signal_group(9).unwrap();
    assert_eq!(
        child.wait(Some(Duration::from_secs(5))).unwrap(),
        ExitStatus::Signaled(9)
    );
}

#[test]
fn wait_with_timeout_does_not_hang() {
    let mut child = spawn(&sh("sleep 30")).unwrap();
    let err = child.wait(Some(Duration::from_millis(75))).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::TimedOut);
    child.signal(9).unwrap();
    assert_eq!(
        child.wait(Some(Duration::from_secs(5))).unwrap(),
        ExitStatus::Signaled(9)
    );
}

#[test]
fn ready_pipe_blocks_exec_until_parent_releases_it() {
    let mut child = spawn(&sh("printf ready").ready_pipe(true)).unwrap();
    std::thread::sleep(Duration::from_millis(100));
    let before = read_available(child.master_fd(), Duration::from_millis(100));
    assert!(before.is_empty(), "child ran before ready: {before:?}");
    assert_eq!(
        child
            .wait_for_exec(Some(Duration::from_millis(50)))
            .unwrap(),
        ExecStatus::Pending
    );
    child.mark_terminal_ready().unwrap();
    let output = read_until_eof(child.master_fd(), Duration::from_secs(5));
    assert_eq!(output, b"ready");
    child.wait(Some(Duration::from_secs(5))).unwrap();
}

#[test]
fn passes_selected_fd_to_child_and_closes_unrelated_fds() {
    let pipe = SelfPipe::new(false).unwrap();
    set_cloexec(pipe.write_fd(), false).unwrap();
    let script = format!("printf hi > /proc/self/fd/{}", pipe.write_fd());
    let mut child = spawn(&sh(&script).pass_fd(pipe.write_fd())).unwrap();
    let mut buf = [0u8; 2];
    let start = Instant::now();
    loop {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "timed out waiting for passed fd"
        );
        let mut fds = [PollFd::new(pipe.read_fd(), kitty_os::loop_utils::POLLIN)];
        if poll(&mut fds, 50).unwrap() > 0 {
            read_retry(pipe.read_fd(), &mut buf).unwrap();
            break;
        }
    }
    assert_eq!(&buf, b"hi");
    child.wait(Some(Duration::from_secs(5))).unwrap();

    // A descriptor that was not passed must be closed by the 3..256 sweep.
    let unrelated = SelfPipe::new(false).unwrap();
    // Clear CLOEXEC so only the child's explicit close can hide it.
    set_cloexec(unrelated.write_fd(), false).unwrap();
    let script = format!(
        "if [ -e /proc/self/fd/{} ]; then printf leaked; else printf closed; fi",
        unrelated.write_fd()
    );
    let mut child = spawn(&sh(&script)).unwrap();
    let output = read_until_eof(child.master_fd(), Duration::from_secs(5));
    assert_eq!(output, b"closed");
    child.wait(Some(Duration::from_secs(5))).unwrap();
}

#[test]
fn initial_window_size_is_visible_through_tiocgwinsz_in_the_child() {
    let mut child = spawn(&sh("stty size").initial_window_size(WindowSize::new(31, 101))).unwrap();
    let output = read_until_eof(child.master_fd(), Duration::from_secs(5));
    assert_eq!(String::from_utf8_lossy(&output).trim(), "31 101");
    child.wait(Some(Duration::from_secs(5))).unwrap();
}

#[test]
fn resize_after_spawn_is_seen_by_the_child() {
    let mut child = spawn(&sh("sleep 0.15; stty size")).unwrap();
    std::thread::sleep(Duration::from_millis(50));
    child.resize(WindowSize::new(42, 132)).unwrap();
    let output = read_until_eof(child.master_fd(), Duration::from_secs(5));
    assert_eq!(String::from_utf8_lossy(&output).trim(), "42 132");
    child.wait(Some(Duration::from_secs(5))).unwrap();
}

#[test]
fn large_output_exceeding_the_pipe_buffer_is_complete() {
    let config = sh("dd if=/dev/zero bs=4096 count=256 2>/dev/null");
    let mut child = spawn(&config).unwrap();
    let output = read_until_eof(child.master_fd(), Duration::from_secs(20));
    assert!(
        output.len() >= 1024 * 1024,
        "only got {} bytes",
        output.len()
    );
    assert!(output.iter().all(|&b| b == 0));
    child.wait(Some(Duration::from_secs(5))).unwrap();
}

#[test]
fn exec_and_wait_timings_are_sane() {
    let start = Instant::now();
    let mut child = spawn(&sh("printf x")).unwrap();
    let out = read_until_eof(child.master_fd(), Duration::from_secs(5));
    assert_eq!(out, b"x");
    child.wait(Some(Duration::from_secs(5))).unwrap();
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "spawn/read/wait took {:?}",
        start.elapsed()
    );
}

#[test]
fn wait_status_decoder_accepts_raw_kernel_encoding() {
    assert_eq!(decode_wait_status(7 << 8), ExitStatus::Exited(7));
    assert_eq!(decode_wait_status(15), ExitStatus::Signaled(15));
}

// ---------------------------------------------------------------------------
// loop_utils.rs (integration-level)
// ---------------------------------------------------------------------------

#[test]
fn wake_pipe_wakes_a_blocking_poll() {
    let pipe = SelfPipe::new(true).unwrap();
    let writer = std::thread::spawn({
        let write_fd = pipe.write_fd();
        move || {
            std::thread::sleep(Duration::from_millis(20));
            write_once(write_fd, b"w").unwrap();
        }
    });
    let mut fds = [PollFd::new(pipe.read_fd(), kitty_os::loop_utils::POLLIN)];
    let start = Instant::now();
    assert_eq!(poll(&mut fds, 2000).unwrap(), 1);
    assert!(start.elapsed() < Duration::from_secs(2));
    writer.join().unwrap();
    pipe.drain();
}

#[test]
fn nonblocking_partial_write_semantics() {
    let pipe = SelfPipe::new(true).unwrap();
    let mut total = 0;
    let mut blocked = false;
    for _ in 0..1000 {
        match write_once(pipe.write_fd(), &[7u8; 4096]).unwrap() {
            IoOutcome::Completed(n) => total += n,
            IoOutcome::WouldBlock => {
                blocked = true;
                break;
            }
            IoOutcome::Interrupted => continue,
        }
    }
    assert!(total > 0);
    assert!(blocked, "nonblocking pipe never reported EAGAIN");
    assert_eq!(
        write_once(pipe.write_fd(), b"x").unwrap(),
        IoOutcome::WouldBlock
    );
    let mut buf = [0u8; 4096];
    assert!(read_retry(pipe.read_fd(), &mut buf).unwrap() > 0);
}

// ---------------------------------------------------------------------------
// child_monitor.rs
// ---------------------------------------------------------------------------

#[test]
fn monitor_reads_output_and_reports_exit() {
    let _guard = monitor_guard();
    let child = spawn(&sh("printf monitor")).unwrap();
    let seen_exit = Arc::new(AtomicBool::new(false));
    let seen_output = Arc::new(Mutex::new(Vec::new()));
    let flag = Arc::clone(&seen_exit);
    let out = Arc::clone(&seen_output);
    let mut monitor = ChildMonitor::new_with_callbacks(
        child,
        move |data| out.lock().unwrap().extend_from_slice(data),
        move |status| {
            assert_eq!(status, ExitStatus::Exited(0));
            flag.store(true, Ordering::SeqCst);
        },
    )
    .unwrap();
    let status = monitor.run_until_exit(Duration::from_secs(5)).unwrap();
    assert_eq!(status, ExitStatus::Exited(0));
    assert!(seen_exit.load(Ordering::SeqCst));
    assert_eq!(&*seen_output.lock().unwrap(), b"monitor");
}

#[test]
fn monitor_sigchld_and_eof_callbacks_fire_once() {
    let _guard = monitor_guard();
    let child = spawn(&sh("printf eof")).unwrap();
    let eofs = Arc::new(AtomicUsize::new(0));
    let exits = Arc::new(AtomicUsize::new(0));
    let eof_flag = Arc::clone(&eofs);
    let exit_flag = Arc::clone(&exits);
    let mut monitor = ChildMonitor::new(child).unwrap();
    monitor.set_eof_callback(move || {
        eof_flag.fetch_add(1, Ordering::SeqCst);
    });
    monitor.set_exit_callback(move |_| {
        exit_flag.fetch_add(1, Ordering::SeqCst);
    });
    monitor.run_until_eof(Duration::from_secs(5)).unwrap();
    monitor.run_until_exit(Duration::from_secs(5)).unwrap();
    // Extra polls must not duplicate callbacks.
    monitor.poll_once(Some(Duration::from_millis(10))).unwrap();
    monitor.poll_once(Some(Duration::from_millis(10))).unwrap();
    assert_eq!(eofs.load(Ordering::SeqCst), 1);
    assert_eq!(exits.load(Ordering::SeqCst), 1);
    assert_eq!(monitor.take_output(), b"eof");
}

#[test]
fn monitor_wakeup_makes_poll_return_immediately() {
    let _guard = monitor_guard();
    let child = spawn(&sh("sleep 30")).unwrap();
    let mut monitor = ChildMonitor::new(child).unwrap();
    monitor.wakeup().unwrap();
    let start = Instant::now();
    let outcome = monitor.poll_once(Some(Duration::from_secs(3))).unwrap();
    assert!(outcome.woken);
    assert!(start.elapsed() < Duration::from_secs(1));
    monitor.child_mut().signal(9).unwrap();
    monitor.run_until_exit(Duration::from_secs(5)).unwrap();
}

#[test]
fn monitor_queues_writes_and_child_receives_them() {
    let _guard = monitor_guard();
    let child = spawn(&sh("stty raw -echo; cat")).unwrap();
    let mut monitor = ChildMonitor::new(child).unwrap();
    monitor.queue_write(b"hello").unwrap();
    let wrote = monitor.flush().unwrap();
    assert_eq!(wrote, 5);
    assert_eq!(monitor.queued_write_len(), 0);
    let echoed = read_until_contains(monitor.master_fd(), b"hello", Duration::from_secs(5));
    assert!(echoed.windows(5).any(|w| w == b"hello"));
    monitor.child_mut().signal(9).unwrap();
    monitor.run_until_exit(Duration::from_secs(5)).unwrap();
}

#[test]
fn monitor_write_queue_reports_backpressure() {
    let _guard = monitor_guard();
    let child = spawn(&sh("sleep 30")).unwrap();
    let mut monitor = ChildMonitor::new(child).unwrap();
    monitor.set_write_buffer_limit(16);
    monitor.queue_write(&[0u8; 8]).unwrap();
    assert!(monitor.queue_write(&[0u8; 9]).is_err());
    assert_eq!(monitor.queued_write_len(), 8);
    monitor.child_mut().signal(9).unwrap();
    monitor.run_until_exit(Duration::from_secs(5)).unwrap();
}

#[test]
fn monitor_partial_flush_preserves_the_remainder() {
    let _guard = monitor_guard();
    let child = spawn(&sh("stty raw -echo; cat >/dev/null")).unwrap();
    let mut monitor = ChildMonitor::new(child).unwrap();
    // Let the shell finish stty before filling the pty input buffer.
    std::thread::sleep(Duration::from_millis(200));
    let payload = vec![b'x'; 256 * 1024];
    monitor.queue_write(&payload).unwrap();
    let first = monitor.flush().unwrap();
    assert!(first > 0);
    assert!(
        monitor.queued_write_len() > 0,
        "pty accepted everything at once"
    );
    let mut guard = 0;
    while monitor.queued_write_len() > 0 {
        guard += 1;
        assert!(guard < 500, "flush made no progress");
        monitor.poll_once(Some(Duration::from_millis(50))).unwrap();
        monitor.flush().unwrap();
    }
    monitor.child_mut().signal(9).unwrap();
    monitor.run_until_exit(Duration::from_secs(5)).unwrap();
}

#[test]
fn monitor_resize_updates_tiocgwinsz() {
    let _guard = monitor_guard();
    let child = spawn(&sh("sleep 0.15; stty size")).unwrap();
    let mut monitor = ChildMonitor::new(child).unwrap();
    std::thread::sleep(Duration::from_millis(50));
    monitor.resize(WindowSize::new(45, 150)).unwrap();
    assert_eq!(
        monitor.child().window_size().unwrap(),
        WindowSize::new(45, 150)
    );
    monitor.run_until_exit(Duration::from_secs(5)).unwrap();
    let output = monitor.take_output();
    assert_eq!(String::from_utf8_lossy(&output).trim(), "45 150");
}

#[test]
fn monitor_reports_signaled_exit_for_killed_child() {
    let _guard = monitor_guard();
    let child = spawn(&sh("sleep 30")).unwrap();
    let mut monitor = ChildMonitor::new(child).unwrap();
    monitor.child_mut().signal(15).unwrap();
    let status = monitor.run_until_exit(Duration::from_secs(5)).unwrap();
    assert_eq!(status, ExitStatus::Signaled(15));
}

#[test]
fn monitor_handles_large_output_with_backpressure() {
    let _guard = monitor_guard();
    let child = spawn(&sh("dd if=/dev/zero bs=4096 count=256 2>/dev/null")).unwrap();
    let bytes = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&bytes);
    let mut monitor = ChildMonitor::new(child).unwrap();
    monitor.set_read_buffer_limit(128 * 1024);
    monitor.set_output_callback(move |data| {
        counter.fetch_add(data.len(), Ordering::SeqCst);
    });
    monitor.run_until_exit(Duration::from_secs(20)).unwrap();
    assert!(
        bytes.load(Ordering::SeqCst) >= 1024 * 1024,
        "callback saw only {} bytes",
        bytes.load(Ordering::SeqCst)
    );
}

#[test]
fn monitor_stop_is_observable() {
    let _guard = monitor_guard();
    let child = spawn(&sh("sleep 30")).unwrap();
    let mut monitor = ChildMonitor::new(child).unwrap();
    monitor.stop().unwrap();
    assert!(monitor.is_stopped());
    let outcome: MonitorOutcome = monitor.poll_once(Some(Duration::from_millis(10))).unwrap();
    assert!(!outcome.woken);
    monitor.child_mut().signal(9).unwrap();
    // The stopped monitor will not reap, so wait on the child directly.
    assert_eq!(
        monitor
            .child_mut()
            .wait(Some(Duration::from_secs(5)))
            .unwrap(),
        ExitStatus::Signaled(9)
    );
}

#[test]
fn monitor_eof_is_distinct_from_exit() {
    let _guard = monitor_guard();
    let child = spawn(&sh("printf done; exec 0<&- 1>&- 2>&-; sleep 30")).unwrap();
    let mut monitor = ChildMonitor::new(child).unwrap();
    monitor.run_until_eof(Duration::from_secs(5)).unwrap();
    assert!(monitor.is_eof());
    assert!(monitor.exit_status().is_none());
    assert_eq!(monitor.take_output(), b"done");
    monitor.child_mut().signal(9).unwrap();
    monitor.run_until_exit(Duration::from_secs(5)).unwrap();
}

#[test]
fn monitor_does_not_leave_children_behind() {
    let _guard = monitor_guard();
    let child = spawn(&sh("sleep 30")).unwrap();
    let monitor = ChildMonitor::new(child).unwrap();
    // Drop without waiting: the monitor's Drop must SIGHUP the process group.
    let pid = monitor.pid();
    drop(monitor);
    let start = Instant::now();
    loop {
        // waitpid(-1) is not exposed; probe /proc to confirm the child died.
        let path = format!("/proc/{pid}/stat");
        match std::fs::read_to_string(path) {
            Ok(stat) => {
                // Zombie children are reaped by init once we exit; accept a
                // zombie as "not running". State is field 3 after the comm.
                if stat.contains(") Z ") {
                    break;
                }
            }
            Err(_) => break,
        }
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "child {pid} did not die after monitor drop"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let status = kitty_os::reap_pid(pid, Some(Duration::from_secs(5))).unwrap();
    assert!(status.signal().is_some(), "unexpected status {status:?}");
}

#[test]
fn try_wait_reaps_an_exited_child() {
    let mut child = spawn(&sh("exit 0")).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "child was never reaped"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

#[test]
fn try_wait_reports_exit_after_an_external_reap() {
    let mut child = spawn(&sh("exit 0")).unwrap();
    let pid = child.pid();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        let mut status = 0i32;
        let ret = unsafe { kitty_os::sys::waitpid(pid, &mut status, kitty_os::sys::WNOHANG) };
        if ret == pid {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "child was never reaped"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(
        child.try_wait().unwrap(),
        Some(kitty_os::child::ExitStatus::Unknown(-1))
    );
}
