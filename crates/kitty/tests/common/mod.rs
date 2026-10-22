// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(dead_code)]

use std::io::Write;
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

extern "C" {
    fn kill(pid: i32, sig: i32) -> i32;
}

pub fn force_kill(pid: u32) {
    unsafe {
        kill(pid as i32, 9);
    }
}

pub fn run_kitr(args: &[&str], input: &[u8], timeout: Duration) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_kitr"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn kitr");
    let pid = child.id();
    let mut stdin = child.stdin.take().expect("stdin");
    let input = input.to_vec();
    let writer = thread::spawn(move || {
        let _ = stdin.write_all(&input);
    });
    let (tx, rx) = mpsc::channel();
    let waiter = thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    match rx.recv_timeout(timeout) {
        Ok(result) => {
            let _ = writer.join();
            let _ = waiter.join();
            result.expect("wait for kitr")
        }
        Err(_) => {
            force_kill(pid);
            match rx.recv_timeout(Duration::from_secs(5)) {
                Ok(Ok(output)) => panic!(
                    "kitr timed out; stdout={:?} stderr={:?}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                ),
                _ => panic!("kitr timed out and could not be reaped"),
            }
        }
    }
}

pub fn wait_child_timeout(child: &mut Child, timeout: Duration) -> std::process::ExitStatus {
    let pid = child.id();
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            return status;
        }
        if start.elapsed() >= timeout {
            force_kill(pid);
            let _ = child.wait();
            panic!("child did not exit within {timeout:?}");
        }
        thread::sleep(Duration::from_millis(10));
    }
}

pub fn stdout_text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}
