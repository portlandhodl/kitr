// SPDX-License-Identifier: GPL-3.0-or-later
//! One PTY + screen + parser + input-tracker tuple (a GUI pane).

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use kitty_os::child::{ExitStatus, SpawnConfig};
use kitty_os::loop_utils::{self, IoOutcome};
use kitty_os::pty::WindowSize;
use kitty_os::ChildProcess;
use kitty_screen::{CursorShape, Parser, Screen, ScreenHandler};

use super::input::ModeTracker;
use crate::inherited_env;

const MAX_PENDING_INPUT: usize = 4 * 1024 * 1024;

/// Everything needed to start a pane's child.
#[derive(Clone, Debug, Default)]
pub(crate) struct SpawnSpec {
    pub argv: Vec<OsString>,
    pub cwd: Option<PathBuf>,
    pub env: Vec<(OsString, OsString)>,
    pub title: Option<String>,
}

pub(crate) struct Pane {
    pub parser: Parser,
    pub handler: ScreenHandler,
    pub tracker: ModeTracker,
    pub child: ChildProcess,
    pub write_queue: VecDeque<u8>,
    pub read_buffer: Vec<u8>,
    pub output_bytes: u64,
    pub pty_eof: bool,
    pub exit_status: Option<ExitStatus>,
    pub exited_at: Option<Instant>,
    pub last_title: Option<String>,
    pub columns: u32,
    pub rows: u32,
}

impl Pane {
    pub(crate) fn spawn(
        spec: &SpawnSpec,
        columns: u32,
        rows: u32,
        scrollback: u32,
        cursor_shape: CursorShape,
    ) -> io::Result<Self> {
        let command = if spec.argv.is_empty() {
            vec![crate::default_shell()]
        } else {
            spec.argv.clone()
        };
        let mut config = SpawnConfig::new(command[0].clone());
        config.argv = command;
        let mut env = inherited_env();
        for (key, value) in &spec.env {
            let mut prefix = key.clone();
            prefix.push("=");
            env.retain(|entry| !entry.as_bytes().starts_with(prefix.as_bytes()));
            let mut entry = key.clone();
            entry.push("=");
            entry.push(value);
            env.push(entry);
        }
        config.env = env;
        config.cwd = spec.cwd.clone();
        config.initial_window_size = Some(WindowSize::new(rows as u16, columns as u16));
        config.stdin_pipe = false;
        let mut child = kitty_os::spawn(&config)?;
        let _ = child.wait_for_exec(Some(Duration::from_secs(2)));
        loop_utils::set_nonblocking(child.master_fd(), true)?;

        let mut screen = Screen::builder(columns.max(1), rows.max(1))
            .scrollback(scrollback.max(1))
            .build()
            .map_err(|error| io::Error::other(error.to_string()))?;
        screen.cursor.shape = cursor_shape;
        Ok(Self {
            parser: Parser::new(),
            handler: ScreenHandler::new(screen),
            tracker: ModeTracker::new(),
            child,
            write_queue: VecDeque::new(),
            read_buffer: vec![0u8; 64 * 1024],
            output_bytes: 0,
            pty_eof: false,
            exit_status: None,
            exited_at: None,
            last_title: spec.title.clone(),
            columns: columns.max(1),
            rows: rows.max(1),
        })
    }

    pub(crate) fn screen(&self) -> &Screen {
        &self.handler.screen
    }

    pub(crate) fn write(&mut self, bytes: &[u8]) {
        if self.write_queue.len() + bytes.len() <= MAX_PENDING_INPUT {
            self.write_queue.extend(bytes);
        }
    }

    /// Read up to 16 chunks. Returns the number of bytes parsed this call.
    pub(crate) fn read(&mut self) -> io::Result<usize> {
        if self.pty_eof {
            return Ok(0);
        }
        let mut total = 0usize;
        for _ in 0..16 {
            match loop_utils::read_once(self.child.master_fd(), &mut self.read_buffer)? {
                IoOutcome::Completed(0) => {
                    self.pty_eof = true;
                    break;
                }
                IoOutcome::Completed(n) => {
                    let chunk = self.read_buffer[..n].to_vec();
                    self.tracker.feed(&chunk);
                    self.handler.feed(&mut self.parser, &chunk);
                    self.output_bytes += n as u64;
                    total += n;
                }
                IoOutcome::WouldBlock => break,
                IoOutcome::Interrupted => continue,
            }
        }
        Ok(total)
    }

    pub(crate) fn flush(&mut self) -> io::Result<()> {
        while !self.write_queue.is_empty() {
            let (front, _) = self.write_queue.as_slices();
            match loop_utils::write_once(self.child.master_fd(), front)? {
                IoOutcome::Completed(0) => {
                    self.write_queue.clear();
                    break;
                }
                IoOutcome::Completed(n) => {
                    self.write_queue.drain(..n);
                }
                IoOutcome::WouldBlock => break,
                IoOutcome::Interrupted => continue,
            }
        }
        Ok(())
    }

    pub(crate) fn check(&mut self) -> io::Result<Option<ExitStatus>> {
        if self.exit_status.is_none() {
            if let Some(status) = self.child.try_wait()? {
                self.exit_status = Some(status);
                self.exited_at = Some(Instant::now());
                return Ok(Some(status));
            }
        }
        Ok(None)
    }

    pub(crate) fn resize(&mut self, columns: u32, rows: u32) -> bool {
        let columns = columns.max(1);
        let rows = rows.max(1);
        if columns == self.columns && rows == self.rows {
            return false;
        }
        self.columns = columns;
        self.rows = rows;
        self.handler.screen.resize(rows, columns);
        let _ = self.child.resize(WindowSize::new(
            rows.min(u16::MAX as u32) as u16,
            columns.min(u16::MAX as u32) as u16,
        ));
        true
    }

    pub(crate) fn input_context(&self) -> kitty_input::keys::ScreenInputContext {
        super::input::input_context(&self.handler.screen, &self.tracker)
    }

    pub(crate) fn scrollback_up(&mut self) {
        if self.handler.screen.is_using_alternate_linebuf() {
            return;
        }
        let count = self.handler.screen.lines.saturating_sub(1).max(1);
        self.handler
            .screen
            .reverse_scroll_and_fill_from_scrollback(count);
    }

    pub(crate) fn scrollback_down(&mut self) {
        if self.handler.screen.is_using_alternate_linebuf() {
            return;
        }
        let count = self.handler.screen.lines.saturating_sub(1).max(1);
        self.handler.screen.scroll(count);
    }

    /// Best-effort termination used when a pane/tab is closed or the GUI exits.
    pub(crate) fn terminate(&mut self) {
        if self.exit_status.is_none() {
            let _ = self.child.signal_group(15);
            let start = Instant::now();
            while self.exit_status.is_none() && start.elapsed() < Duration::from_millis(200) {
                let _ = self.read();
                let _ = self.flush();
                let _ = self.check();
                if self.exit_status.is_none() {
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
            if self.exit_status.is_none() {
                let _ = self.child.signal_group(9);
                let start = Instant::now();
                while self.exit_status.is_none() && start.elapsed() < Duration::from_secs(1) {
                    let _ = self.check();
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        }
        for _ in 0..10 {
            let _ = self.read();
            if self.pty_eof {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        if self.exit_status.is_none() {
            self.exit_status = self.child.try_wait().ok().flatten();
        }
    }
}
