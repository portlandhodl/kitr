// SPDX-License-Identifier: GPL-3.0-or-later
//! Minimal dependency-free terminal UI primitives used by the native kittens.
//!
//! The real backend opens `/dev/tty`, switches it to raw mode, and uses ANSI
//! cursor addressing. Tests use [`FakeTty`], an in-memory backend with the
//! same [`Tty`] contract, so every overlay's key handling and drawing can be
//! exercised without a terminal.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::time::Duration;

/// Terminal dimensions in cells.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Size {
    pub cols: u16,
    pub rows: u16,
}

impl Size {
    pub const fn new(cols: u16, rows: u16) -> Self {
        Self { cols, rows }
    }
}

/// A byte-oriented terminal with a known size. Implementations may block in
/// [`Tty::read`]; callers use short timeouts for polling keys.
pub trait Tty {
    fn size(&mut self) -> io::Result<Size>;
    fn write_all(&mut self, data: &[u8]) -> io::Result<()>;
    fn flush(&mut self) -> io::Result<()>;
    fn read(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<usize>;
}

/// A key decoded from raw terminal input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Enter,
    Esc,
    Backspace,
    Delete,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Tab,
    BackTab,
    Ctrl(char),
    Unknown(Vec<u8>),
}

impl Key {
    pub fn is_quit(&self) -> bool {
        matches!(self, Key::Esc | Key::Ctrl('c') | Key::Ctrl('d'))
    }
}

/// Incremental decoder for terminal key input, including CSI cursor keys and
/// kitty's CSI-u encoding (`CSI codepoint;mods[:event]u`).
#[derive(Default)]
pub struct KeyParser {
    pending: Vec<u8>,
}

impl KeyParser {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn feed(&mut self, data: &[u8]) {
        self.pending.extend_from_slice(data);
    }

    /// Decode the next complete key. `None` means more bytes are needed.
    pub fn next_key(&mut self) -> Option<Key> {
        if self.pending.is_empty() {
            return None;
        }
        let first = self.pending[0];
        match first {
            0x1b => {
                if self.pending.len() == 1 {
                    return Some(Key::Esc);
                }
                match self.pending[1] {
                    b'[' => {
                        let end = self.pending[2..]
                            .iter()
                            .position(|b| (0x40..=0x7e).contains(b))
                            .map(|p| p + 2)?;
                        let seq = self.pending[..=end].to_vec();
                        let key = parse_csi(&seq[2..seq.len() - 1], seq[seq.len() - 1]);
                        self.pending.drain(..=end);
                        Some(key)
                    }
                    b'O' => {
                        if self.pending.len() < 3 {
                            return None;
                        }
                        let key = match self.pending[2] {
                            b'A' => Key::Up,
                            b'B' => Key::Down,
                            b'C' => Key::Right,
                            b'D' => Key::Left,
                            b'H' => Key::Home,
                            b'F' => Key::End,
                            other => Key::Unknown(vec![0x1b, b'O', other]),
                        };
                        self.pending.drain(..3);
                        Some(key)
                    }
                    b'\r' | b'\n' => {
                        self.pending.drain(..2);
                        Some(Key::Enter)
                    }
                    _ => {
                        // Alt+key is reported as an unknown key, matching the
                        // legacy kitten behavior of displaying it verbatim.
                        let ch = decode_utf8(&self.pending[1..])?;
                        let len = 1 + ch.len_utf8();
                        let raw = self.pending[..len].to_vec();
                        self.pending.drain(..len);
                        Some(Key::Unknown(raw))
                    }
                }
            }
            0x7f | 0x08 => {
                self.pending.remove(0);
                Some(Key::Backspace)
            }
            b'\r' | b'\n' => {
                self.pending.remove(0);
                Some(Key::Enter)
            }
            b'\t' => {
                self.pending.remove(0);
                Some(Key::Tab)
            }
            0x00..=0x1f => {
                self.pending.remove(0);
                let ch = if first == 0 {
                    '@'
                } else {
                    (b'a' + first - 1) as char
                };
                Some(Key::Ctrl(ch))
            }
            _ => {
                let text = decode_utf8(&self.pending)?;
                let len = text.len_utf8();
                self.pending.drain(..len);
                Some(Key::Char(text))
            }
        }
    }
}

fn decode_utf8(bytes: &[u8]) -> Option<char> {
    for take in 1..=bytes.len().min(4) {
        if let Ok(text) = std::str::from_utf8(&bytes[..take]) {
            if let Some(ch) = text.chars().next() {
                return Some(ch);
            }
        }
    }
    None
}

fn parse_csi(body: &[u8], trailer: u8) -> Key {
    let text = String::from_utf8_lossy(body);
    let mut fields = text.split(';');
    let first = fields.next().unwrap_or("");
    let second = fields.next().unwrap_or("");
    let mods = second
        .split(':')
        .next()
        .and_then(|x| x.parse::<u32>().ok())
        .unwrap_or(1);
    match trailer {
        b'A' => Key::Up,
        b'B' => Key::Down,
        b'C' => Key::Right,
        b'D' => Key::Left,
        b'H' => Key::Home,
        b'F' => Key::End,
        b'Z' => Key::BackTab,
        b'~' => match first {
            "1" | "7" => Key::Home,
            "2" => Key::Unknown(vec![0x1b, b'[', b'2', b'~']),
            "3" => Key::Delete,
            "4" | "8" => Key::End,
            "5" => Key::PageUp,
            "6" => Key::PageDown,
            _ => Key::Unknown(body.to_vec()),
        },
        b'u' => {
            let code = first.parse::<u32>().unwrap_or(0);
            if mods >= 5 && code <= 0x10ffff {
                if let Some(ch) = char::from_u32(code) {
                    return Key::Ctrl(ch.to_ascii_lowercase());
                }
            }
            char::from_u32(code)
                .map(Key::Char)
                .unwrap_or_else(|| Key::Unknown(body.to_vec()))
        }
        _ => Key::Unknown(body.to_vec()),
    }
}

/// Real terminal backend: an open `/dev/tty`, raw mode, restored on drop.
pub struct StdTty {
    file: File,
    saved: Option<kitty_os::pty::Termios>,
}

impl StdTty {
    pub fn open() -> io::Result<Self> {
        let file = OpenOptions::new().read(true).write(true).open("/dev/tty")?;
        let fd = std::os::fd::AsRawFd::as_raw_fd(&file);
        let saved = kitty_os::pty::enter_raw_mode(fd)?;
        Ok(Self {
            file,
            saved: Some(saved),
        })
    }

    pub fn is_terminal() -> bool {
        std::io::IsTerminal::is_terminal(&io::stdin())
    }
}

impl Drop for StdTty {
    fn drop(&mut self) {
        if let Some(saved) = self.saved.take() {
            let fd = std::os::fd::AsRawFd::as_raw_fd(&self.file);
            let _ = kitty_os::pty::restore_termios(fd, &saved);
        }
    }
}

impl Tty for StdTty {
    fn size(&mut self) -> io::Result<Size> {
        let fd = std::os::fd::AsRawFd::as_raw_fd(&self.file);
        let ws = kitty_os::pty::window_size(fd)?;
        Ok(Size::new(ws.columns, ws.rows))
    }

    fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        self.file.write_all(data)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }

    fn read(&mut self, buf: &mut [u8], _timeout: Duration) -> io::Result<usize> {
        loop {
            match self.file.read(buf) {
                Ok(n) => return Ok(n),
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) => return Err(err),
            }
        }
    }
}

/// Deterministic in-memory terminal used by the UI tests.
#[derive(Default)]
pub struct FakeTty {
    pub output: Vec<u8>,
    input: VecDeque<u8>,
    pub size: Size,
}

impl FakeTty {
    pub fn new(cols: u16, rows: u16, input: impl Into<Vec<u8>>) -> Self {
        Self {
            output: Vec::new(),
            input: input.into().into(),
            size: Size::new(cols, rows),
        }
    }

    pub fn push_input(&mut self, input: impl Into<Vec<u8>>) {
        self.input.extend(input.into());
    }

    pub fn output_text(&self) -> String {
        String::from_utf8_lossy(&self.output).into_owned()
    }
}

impl Tty for FakeTty {
    fn size(&mut self) -> io::Result<Size> {
        Ok(self.size)
    }

    fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        self.output.extend_from_slice(data);
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn read(&mut self, buf: &mut [u8], _timeout: Duration) -> io::Result<usize> {
        if self.input.is_empty() {
            return Ok(0);
        }
        let n = buf.len().min(self.input.len());
        for slot in buf.iter_mut().take(n) {
            *slot = self.input.pop_front().expect("checked length");
        }
        Ok(n)
    }
}

/// Read one key, polling the backend for up to `timeout`.
pub fn read_key(
    tty: &mut dyn Tty,
    parser: &mut KeyParser,
    timeout: Duration,
) -> io::Result<Option<Key>> {
    if let Some(key) = parser.next_key() {
        return Ok(Some(key));
    }
    let mut buf = [0u8; 256];
    match tty.read(&mut buf, timeout) {
        Ok(0) => Ok(None),
        Ok(n) => {
            parser.feed(&buf[..n]);
            if let Some(key) = parser.next_key() {
                return Ok(Some(key));
            }
            let mut more = [0u8; 256];
            match tty.read(&mut more, timeout) {
                Ok(0) => Ok(None),
                Ok(m) => {
                    parser.feed(&more[..m]);
                    Ok(parser.next_key())
                }
                Err(err) if err.kind() == io::ErrorKind::TimedOut => Ok(None),
                Err(err) => Err(err),
            }
        }
        Err(err) if err.kind() == io::ErrorKind::TimedOut => Ok(None),
        Err(err) => Err(err),
    }
}

/// Enter the alternate screen, hide the cursor, and clear.
pub fn enter_screen(tty: &mut dyn Tty) -> io::Result<()> {
    tty.write_all(b"\x1b[?1049h\x1b[?25l\x1b[2J\x1b[H")?;
    tty.flush()
}

/// Restore cursor and the normal screen.
pub fn leave_screen(tty: &mut dyn Tty) -> io::Result<()> {
    tty.write_all(b"\x1b[?25h\x1b[?1049l")?;
    tty.flush()
}

/// Move the cursor to a 1-based cell.
pub fn move_to(tty: &mut dyn Tty, x: u16, y: u16) -> io::Result<()> {
    let data = format!("\x1b[{};{}H", y.max(1), x.max(1));
    tty.write_all(data.as_bytes())
}

pub struct TtyWriter<'a, T: Tty + ?Sized>(pub &'a mut T);

impl<T: Tty + ?Sized> Write for TtyWriter<'_, T> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write_all(buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

pub fn clear_line(tty: &mut dyn Tty) -> io::Result<()> {
    tty.write_all(b"\r\x1b[K")
}

pub fn clear_screen(tty: &mut dyn Tty) -> io::Result<()> {
    tty.write_all(b"\x1b[2J\x1b[H")
}

/// Fit text into a cell width, appending an ellipsis when truncated.
pub fn fit(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let count = text.chars().count();
    if count <= width {
        return text.to_owned();
    }
    if width == 1 {
        return "…".to_owned();
    }
    let mut out: String = text.chars().take(width - 1).collect();
    out.push('…');
    out
}

/// Simple, deterministic subsequence fuzzy matcher used by list overlays.
pub fn fuzzy_score(query: &str, candidate: &str) -> Option<i64> {
    let q: Vec<char> = query.to_lowercase().chars().collect();
    if q.is_empty() {
        return Some(0);
    }
    let c: Vec<char> = candidate.to_lowercase().chars().collect();
    let mut qi = 0usize;
    let mut score = 0i64;
    let mut last = None;
    for (i, ch) in c.iter().enumerate() {
        if qi < q.len() && *ch == q[qi] {
            if last == Some(i.wrapping_sub(1)) {
                score += 4;
            } else {
                score += 1;
            }
            if i == 0 || matches!(c[i - 1], ' ' | '_' | '-' | '/') {
                score += 2;
            }
            last = Some(i);
            qi += 1;
        }
    }
    if qi == q.len() {
        Some(score - c.len() as i64 / 8)
    } else {
        None
    }
}

/// A reusable filtered list state for the simple full-screen pickers.
#[derive(Clone, Debug, Default)]
pub struct ListState {
    pub items: Vec<String>,
    pub query: String,
    pub filtered: Vec<usize>,
    pub selected: usize,
    pub offset: usize,
}

impl ListState {
    pub fn new(items: Vec<String>) -> Self {
        let mut state = Self {
            items,
            ..Self::default()
        };
        state.refilter();
        state
    }

    pub fn refilter(&mut self) {
        let mut scored: Vec<(i64, usize)> = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(i, item)| fuzzy_score(&self.query, item).map(|score| (score, i)))
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        self.filtered = scored.into_iter().map(|(_, i)| i).collect();
        if self.selected >= self.filtered.len() {
            self.selected = 0;
        }
    }

    pub fn current(&self) -> Option<&str> {
        self.filtered
            .get(self.selected)
            .and_then(|i| self.items.get(*i))
            .map(String::as_str)
    }

    pub fn move_by(&mut self, delta: isize) {
        if self.filtered.is_empty() {
            return;
        }
        let len = self.filtered.len() as isize;
        self.selected = ((self.selected as isize + delta).rem_euclid(len)) as usize;
    }

    pub fn handle_edit_key(&mut self, key: &Key) -> bool {
        match key {
            Key::Char(ch) => {
                self.query.push(*ch);
                self.refilter();
                true
            }
            Key::Backspace => {
                self.query.pop();
                self.refilter();
                true
            }
            Key::Up => {
                self.move_by(-1);
                true
            }
            Key::Down => {
                self.move_by(1);
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_keys_and_csi_u() {
        let mut p = KeyParser::new();
        p.feed(b"a\x1b[A\r\x03\x1b[97;5u");
        assert_eq!(p.next_key(), Some(Key::Char('a')));
        assert_eq!(p.next_key(), Some(Key::Up));
        assert_eq!(p.next_key(), Some(Key::Enter));
        assert_eq!(p.next_key(), Some(Key::Ctrl('c')));
        assert_eq!(p.next_key(), Some(Key::Ctrl('a')));
    }

    #[test]
    fn fuzzy_and_list_filtering() {
        assert!(fuzzy_score("cf", "choose_files").is_some());
        assert!(fuzzy_score("zz", "choose_files").is_none());
        let mut state = ListState::new(vec!["alpha".into(), "beta".into()]);
        state.query = "bet".into();
        state.refilter();
        assert_eq!(state.current(), Some("beta"));
    }

    #[test]
    fn fake_tty_roundtrip() {
        let mut tty = FakeTty::new(80, 24, b"q");
        enter_screen(&mut tty).unwrap();
        assert!(tty.output_text().contains("1049h"));
        let mut parser = KeyParser::new();
        assert_eq!(
            read_key(&mut tty, &mut parser, Duration::ZERO).unwrap(),
            Some(Key::Char('q'))
        );
    }
}
