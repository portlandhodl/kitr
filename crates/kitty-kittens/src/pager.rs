// SPDX-License-Identifier: GPL-3.0-or-later
//! Pager kitten, ported from `kittens/pager`.
//!
//! Provides a line-oriented input reader (including `--follow` polling and
//! carriage-return counting for scrollback roles), a scroll/search model and a
//! compact ANSI pager. Syntax highlighting and image previews are not ported.

use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::tui::{self, Key, KeyParser, Tty};

pub const PAGER_USAGE: &str = "\
Usage: kitten pager [options] [filename]

Display text in a pager with searching and scrolling.

Options:
      --role pager|scrollback
      --follow
  -h, --help";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PagerRole {
    #[default]
    Pager,
    Scrollback,
}

#[derive(Clone, Debug, Default)]
pub struct PagerOptions {
    pub role: PagerRole,
    pub follow: bool,
    pub help: bool,
}

pub fn parse_pager_args(args: &[String]) -> Result<(PagerOptions, Vec<String>), String> {
    let mut opts = PagerOptions::default();
    let mut files = Vec::new();
    let mut index = 0usize;
    while index < args.len() {
        let arg = &args[index];
        if arg == "--" {
            files.extend_from_slice(&args[index + 1..]);
            break;
        }
        let (name, inline) = arg
            .split_once('=')
            .map_or((arg.as_str(), None), |(a, b)| (a, Some(b.to_owned())));
        let mut take = || -> String {
            if let Some(value) = inline.clone() {
                return value;
            }
            index += 1;
            args.get(index).cloned().unwrap_or_default()
        };
        match name {
            "-h" | "--help" => opts.help = true,
            "--follow" => opts.follow = true,
            "--role" => {
                opts.role = match take().as_str() {
                    "scrollback" => PagerRole::Scrollback,
                    "pager" => PagerRole::Pager,
                    other => return Err(format!("invalid role: {other}")),
                }
            }
            other if !other.starts_with('-') => files.push(other.to_owned()),
            other => return Err(format!("unknown option: {other}")),
        }
        index += 1;
    }
    if files.len() > 1 {
        return Err("Only a single file can be viewed at a time".to_owned());
    }
    Ok((opts, files))
}

/// A growing buffer of input lines.
#[derive(Clone, Debug, Default)]
pub struct PagerBuffer {
    pub lines: Vec<String>,
    partial: String,
    pub carriage_returns: usize,
}

impl PagerBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed bytes, splitting on newlines. With `count_carriage_returns`, `\r`
    /// increments a counter and does not end a line, matching the scrollback
    /// role.
    pub fn feed(&mut self, data: &[u8], count_carriage_returns: bool) {
        let text = String::from_utf8_lossy(data);
        for ch in text.chars() {
            if ch == '\n' {
                self.lines.push(std::mem::take(&mut self.partial));
                self.carriage_returns = 0;
            } else if ch == '\r' && count_carriage_returns {
                self.carriage_returns += 1;
            } else {
                self.partial.push(ch);
            }
        }
    }

    pub fn finish(&mut self) {
        if !self.partial.is_empty() {
            self.lines.push(std::mem::take(&mut self.partial));
        }
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }
}

pub fn read_pager_input(path: &Path, scrollback: bool) -> io::Result<PagerBuffer> {
    let mut data = Vec::new();
    File::open(path)?.read_to_end(&mut data)?;
    let mut buffer = PagerBuffer::new();
    buffer.feed(&data, scrollback);
    buffer.finish();
    Ok(buffer)
}

fn wait_for_growth(path: &Path, offset: u64) -> io::Result<u64> {
    loop {
        std::thread::sleep(Duration::from_millis(250));
        if fs::metadata(path)?.len() > offset {
            return Ok(offset);
        }
    }
}

pub fn read_follow(
    path: &Path,
    scrollback: bool,
    max_iterations: usize,
) -> io::Result<PagerBuffer> {
    let mut buffer = PagerBuffer::new();
    let mut file = File::open(path)?;
    let mut offset = 0u64;
    for _ in 0..max_iterations {
        let mut data = Vec::new();
        file.seek(SeekFrom::Start(offset))?;
        let read = file.read_to_end(&mut data)?;
        if read > 0 {
            offset += read as u64;
            buffer.feed(&data, scrollback);
        }
        let _ = wait_for_growth(path, offset)?;
    }
    buffer.finish();
    Ok(buffer)
}

#[derive(Clone, Debug, Default)]
pub struct SearchResult {
    pub query: String,
    pub matches: Vec<usize>,
    pub current: usize,
}

#[derive(Clone, Debug, Default)]
pub struct PagerState {
    pub offset: usize,
    pub search: SearchResult,
    pub last_modified: Option<SystemTime>,
}

impl PagerState {
    pub fn search(&mut self, lines: &[String], query: &str) {
        self.search.query = query.to_owned();
        self.search.matches.clear();
        let lower = query.to_lowercase();
        if lower.is_empty() {
            return;
        }
        for (index, line) in lines.iter().enumerate() {
            if line.to_lowercase().contains(&lower) {
                self.search.matches.push(index);
            }
        }
        self.search.current = 0;
        if let Some(index) = self.search.matches.first() {
            self.offset = *index;
        }
    }

    pub fn next_match(&mut self) {
        if self.search.matches.is_empty() {
            return;
        }
        self.search.current = (self.search.current + 1) % self.search.matches.len();
        self.offset = self.search.matches[self.search.current];
    }

    pub fn prev_match(&mut self) {
        if self.search.matches.is_empty() {
            return;
        }
        self.search.current =
            (self.search.current + self.search.matches.len() - 1) % self.search.matches.len();
        self.offset = self.search.matches[self.search.current];
    }

    pub fn scroll(&mut self, delta: isize, line_count: usize) {
        let max = line_count.saturating_sub(1);
        self.offset = if delta < 0 {
            self.offset.saturating_sub((-delta) as usize)
        } else {
            (self.offset + delta as usize).min(max)
        };
    }

    pub fn scroll_to_end(&mut self, line_count: usize) {
        self.offset = line_count.saturating_sub(1);
    }
}

pub fn visible_lines<'a>(lines: &'a [String], state: &PagerState, height: usize) -> &'a [String] {
    let end = (state.offset + height).min(lines.len());
    &lines[state.offset.min(lines.len())..end]
}

/// Run the pager UI. `follow` is only used when the caller already reads new
/// lines into the buffer; this loop redraws the growing buffer.
pub fn run_pager_ui(
    tty: &mut dyn Tty,
    mut buffer: PagerBuffer,
    opts: &PagerOptions,
    source: Option<PathBuf>,
) -> io::Result<()> {
    let mut parser = KeyParser::new();
    let mut state = PagerState::default();
    let mut searching = false;
    let mut query = String::new();
    let mut offset_seen = 0usize;
    tui::enter_screen(tty)?;
    loop {
        if opts.follow {
            if let Some(path) = &source {
                if let Ok(mut file) = File::open(path) {
                    let mut data = Vec::new();
                    if file.seek(SeekFrom::Start(offset_seen as u64)).is_ok()
                        && file.read_to_end(&mut data).is_ok()
                        && !data.is_empty()
                    {
                        offset_seen += data.len();
                        buffer.feed(&data, opts.role == PagerRole::Scrollback);
                        if state.offset + 3 >= buffer.lines.len() {
                            state.scroll_to_end(buffer.lines.len());
                        }
                    }
                }
            }
        }
        let size = tty.size().unwrap_or(tui::Size::new(80, 24));
        let height = size.rows.saturating_sub(1) as usize;
        tui::clear_screen(tty)?;
        let mut text = String::new();
        for line in visible_lines(&buffer.lines, &state, height) {
            text.push_str(&truncate(line, size.cols as usize));
            text.push_str("\r\n");
        }
        if searching {
            text.push_str(&format!("/{query}"));
        } else {
            let search = if state.search.query.is_empty() {
                String::new()
            } else {
                format!(
                    "  /{} [{}/{}]",
                    state.search.query,
                    state.search.current + 1,
                    state.search.matches.len()
                )
            };
            text.push_str(&format!(
                "{} lines  j/k scroll  / search  n/N match  q quit{search}",
                buffer.lines.len()
            ));
        }
        tty.write_all(text.as_bytes())?;
        tty.flush()?;
        let Some(key) = tui::read_key(tty, &mut parser, Duration::from_millis(250))? else {
            if opts.follow {
                continue;
            }
            continue;
        };
        if searching {
            match key {
                Key::Enter => {
                    searching = false;
                    state.search(&buffer.lines, &query);
                }
                Key::Esc => {
                    searching = false;
                    query.clear();
                }
                Key::Backspace => {
                    query.pop();
                }
                Key::Char(ch) => query.push(ch),
                _ => {}
            }
            continue;
        }
        match key {
            Key::Esc | Key::Ctrl('c') | Key::Char('q') => break,
            Key::Char('j') | Key::Down => state.scroll(1, buffer.lines.len()),
            Key::Char('k') | Key::Up => state.scroll(-1, buffer.lines.len()),
            Key::PageDown | Key::Char(' ') => state.scroll(height as isize, buffer.lines.len()),
            Key::PageUp => state.scroll(-(height as isize), buffer.lines.len()),
            Key::Home => state.offset = 0,
            Key::End => state.scroll_to_end(buffer.lines.len()),
            Key::Char('/') => {
                searching = true;
                query.clear();
            }
            Key::Char('n') => state.next_match(),
            Key::Char('N') => state.prev_match(),
            _ => {}
        }
    }
    tui::leave_screen(tty)?;
    Ok(())
}

fn truncate(text: &str, width: usize) -> String {
    let text = text.replace('\t', "    ");
    let count = text.chars().count();
    if count <= width {
        text
    } else {
        format!(
            "{}…",
            text.chars()
                .take(width.saturating_sub(1))
                .collect::<String>()
        )
    }
}

pub fn run_pager_cli(
    args: &[String],
    tty: &mut dyn Tty,
    out: &mut dyn Write,
) -> Result<(), String> {
    let (opts, files) = parse_pager_args(args)?;
    if opts.help {
        writeln!(out, "{PAGER_USAGE}").map_err(|err| err.to_string())?;
        return Ok(());
    }
    let (buffer, source) = match files.first() {
        Some(path) => {
            let path = PathBuf::from(path);
            (
                read_pager_input(&path, opts.role == PagerRole::Scrollback)
                    .map_err(|err| err.to_string())?,
                Some(path),
            )
        }
        None => {
            if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
                return Err("STDIN is a terminal and no filename specified".to_owned());
            }
            let mut buffer = PagerBuffer::new();
            let mut data = Vec::new();
            io::stdin()
                .read_to_end(&mut data)
                .map_err(|err| err.to_string())?;
            buffer.feed(&data, opts.role == PagerRole::Scrollback);
            buffer.finish();
            (buffer, None)
        }
    };
    run_pager_ui(tty, buffer, &opts, source).map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::FakeTty;

    #[test]
    fn buffer_splits_lines_and_carriage_returns() {
        let mut buffer = PagerBuffer::new();
        buffer.feed(b"one\ntwo\rthree\nlast", true);
        buffer.finish();
        assert_eq!(buffer.lines, vec!["one", "twothree", "last"]);
        assert_eq!(buffer.carriage_returns, 0);
    }

    #[test]
    fn search_and_scrolling() {
        let lines: Vec<String> = (0..20).map(|i| format!("line {i}")).collect();
        let mut state = PagerState::default();
        state.search(&lines, "line 1");
        assert_eq!(state.search.matches.len(), 11);
        assert_eq!(state.offset, 1);
        state.next_match();
        assert_eq!(state.offset, 10);
        state.prev_match();
        assert_eq!(state.offset, 1);
        state.scroll_to_end(lines.len());
        assert_eq!(state.offset, 19);
        state.scroll(-5, lines.len());
        assert_eq!(state.offset, 14);
        assert_eq!(visible_lines(&lines, &state, 2), &lines[14..16]);
    }

    #[test]
    fn parse_options() {
        let args = ["--follow", "--role=scrollback", "file.txt"]
            .iter()
            .map(|x| (*x).to_owned())
            .collect::<Vec<_>>();
        let (opts, files) = parse_pager_args(&args).unwrap();
        assert!(opts.follow);
        assert_eq!(opts.role, PagerRole::Scrollback);
        assert_eq!(files, vec!["file.txt"]);
    }

    #[test]
    fn reads_a_file() {
        let dir = std::env::temp_dir().join(format!("kitty-pager-{}-1", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.txt");
        fs::write(&path, b"a\nb\n").unwrap();
        let buffer = read_pager_input(&path, false).unwrap();
        assert_eq!(buffer.lines, vec!["a", "b"]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn ui_searches_and_quits() {
        let mut buffer = PagerBuffer::new();
        buffer.feed(b"alpha\nbeta\ngamma\n", false);
        buffer.finish();
        let opts = PagerOptions::default();
        let mut tty = FakeTty::new(80, 10, b"/gam\rq");
        run_pager_ui(&mut tty, buffer, &opts, None).unwrap();
        let text = tty.output_text();
        assert!(text.contains("alpha"));
        assert!(text.contains("/gam"));
        assert!(text.contains("gamma"));
    }
}
