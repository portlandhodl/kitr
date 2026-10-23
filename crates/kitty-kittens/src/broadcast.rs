// SPDX-License-Identifier: GPL-3.0-or-later
//! Broadcast kitten, ported from `kittens/broadcast`.
//!
//! Typed text is sent to the terminal with `send-text` remote-control
//! commands. The UI is a minimal line editor; the protocol payloads and
//! session start/end escapes are independently testable.

use std::io::{self, Write};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use kitty_remote::base64::encode as b64encode;
use kitty_remote::json::Value;

use crate::protocol::encode_remote_command_value;
use crate::tui::{self, Key, KeyParser, Tty};

pub const BROADCAST_USAGE: &str = "\
Usage: kitten broadcast [options] [initial text to send ...]

Broadcast typed text to kitty windows.

Options:
      --hide-input-toggle KEY  Toggle hiding input (default Ctrl+Alt+Esc)
      --end-session KEY        End the session (default Ctrl+Esc)
  -m, --match EXPR             Window to match
  -t, --match-tab EXPR         Tab to match
  -h, --help";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BroadcastPayload {
    pub exclude_active: bool,
    pub data: String,
    pub match_spec: String,
    pub match_tab: String,
    pub session_id: String,
    pub all: bool,
}

impl Default for BroadcastPayload {
    fn default() -> Self {
        Self {
            exclude_active: true,
            data: String::new(),
            match_spec: String::new(),
            match_tab: String::new(),
            session_id: new_session_id(),
            all: false,
        }
    }
}

impl BroadcastPayload {
    fn to_value(&self, no_response: bool) -> Value {
        let mut payload = vec![
            ("exclude_active", Value::Bool(self.exclude_active)),
            ("data", Value::String(self.data.clone())),
            ("session_id", Value::String(self.session_id.clone())),
        ];
        if !self.match_spec.is_empty() {
            payload.push(("match", Value::String(self.match_spec.clone())));
        }
        if !self.match_tab.is_empty() {
            payload.push(("match_tab", Value::String(self.match_tab.clone())));
        }
        if self.all {
            payload.push(("all", Value::Bool(true)));
        }
        let mut request = vec![
            ("cmd", Value::String("send-text".to_owned())),
            (
                "version",
                Value::String(kitty_remote::KITTY_COMPAT_VERSION.to_owned()),
            ),
            ("payload", Value::object_from(payload)),
        ];
        if no_response {
            request.push(("no_response", Value::Bool(true)));
        }
        Value::object_from(request)
    }

    pub fn escape_code(&self) -> Result<Vec<u8>, String> {
        encode_remote_command_value(&self.to_value(true)).map_err(|err| err.to_string())
    }
}

pub fn new_session_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{:x}{:x}", std::process::id(), nanos)
}

pub fn b64_text(text: &str) -> String {
    format!(
        "base64:{}",
        String::from_utf8_lossy(&b64encode(text.as_bytes(), true))
    )
}

pub fn send_text_escape_code(payload: &BroadcastPayload) -> Result<Vec<u8>, String> {
    payload.escape_code()
}

/// Beginning or end of a broadcast session (`data=session:start|end`).
pub fn session_escape_code(payload: &BroadcastPayload, start: bool) -> Result<Vec<u8>, String> {
    let mut copy = payload.clone();
    copy.data = if start {
        "session:start".to_owned()
    } else {
        "session:end".to_owned()
    };
    copy.escape_code()
}

#[derive(Clone, Debug)]
pub struct BroadcastOptions {
    pub hide_input_toggle: String,
    pub end_session: String,
    pub match_spec: String,
    pub match_tab: String,
    pub initial: Vec<String>,
    pub help: bool,
}

impl Default for BroadcastOptions {
    fn default() -> Self {
        Self {
            hide_input_toggle: "Ctrl+Alt+Esc".to_owned(),
            end_session: "Ctrl+Esc".to_owned(),
            match_spec: String::new(),
            match_tab: String::new(),
            initial: Vec::new(),
            help: false,
        }
    }
}

pub fn parse_broadcast_args(args: &[String]) -> Result<BroadcastOptions, String> {
    let mut opts = BroadcastOptions::default();
    let mut index = 0usize;
    while index < args.len() {
        let arg = &args[index];
        if arg == "--" {
            opts.initial.extend_from_slice(&args[index + 1..]);
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
            "--hide-input-toggle" => opts.hide_input_toggle = take(),
            "--end-session" => opts.end_session = take(),
            "-m" | "--match" => opts.match_spec = take(),
            "-t" | "--match-tab" => opts.match_tab = take(),
            other if !other.starts_with('-') => opts.initial.push(other.to_owned()),
            other => return Err(format!("unknown option: {other}")),
        }
        index += 1;
    }
    Ok(opts)
}

/// Minimal line editor used by the broadcast UI.
#[derive(Clone, Debug, Default)]
pub struct LineEditor {
    pub text: Vec<char>,
    pub cursor: usize,
}

impl LineEditor {
    pub fn as_string(&self) -> String {
        self.text.iter().collect()
    }

    pub fn insert(&mut self, text: &str) {
        let chars: Vec<char> = text.chars().collect();
        self.text
            .splice(self.cursor..self.cursor, chars.iter().copied());
        self.cursor += chars.len();
    }

    pub fn backspace(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        self.cursor -= 1;
        self.text.remove(self.cursor);
        true
    }

    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn right(&mut self) {
        if self.cursor < self.text.len() {
            self.cursor += 1;
        }
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.text.len();
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }

    pub fn handle_key(&mut self, key: &Key) -> bool {
        match key {
            Key::Char(ch) => {
                self.insert(&ch.to_string());
                true
            }
            Key::Backspace => self.backspace(),
            Key::Left => {
                self.left();
                true
            }
            Key::Right => {
                self.right();
                true
            }
            Key::Home => {
                self.home();
                true
            }
            Key::End => {
                self.end();
                true
            }
            Key::Ctrl('a') => {
                self.home();
                true
            }
            Key::Ctrl('e') => {
                self.end();
                true
            }
            _ => false,
        }
    }
}

fn key_name(key: &Key) -> String {
    match key {
        Key::Char(ch) => ch.to_string(),
        Key::Enter => "enter".to_owned(),
        Key::Esc => "escape".to_owned(),
        Key::Backspace => "backspace".to_owned(),
        Key::Delete => "delete".to_owned(),
        Key::Up => "up".to_owned(),
        Key::Down => "down".to_owned(),
        Key::Left => "left".to_owned(),
        Key::Right => "right".to_owned(),
        Key::Home => "home".to_owned(),
        Key::End => "end".to_owned(),
        Key::PageUp => "page_up".to_owned(),
        Key::PageDown => "page_down".to_owned(),
        Key::Tab => "tab".to_owned(),
        Key::BackTab => "shift+tab".to_owned(),
        Key::Ctrl(ch) => format!("ctrl+{ch}"),
        Key::Unknown(bytes) => format!("raw:{:?}", bytes),
    }
}

fn matches_binding(key: &Key, binding: &str) -> bool {
    if binding.eq_ignore_ascii_case("ctrl+alt+esc") {
        return matches!(key, Key::Unknown(_)) && key_name(key).contains("0x1b");
    }
    if binding.eq_ignore_ascii_case("ctrl+esc") {
        return matches!(key, Key::Unknown(_));
    }
    key_name(key).eq_ignore_ascii_case(binding)
}

/// Run the broadcast UI, writing escape codes to `tty`.
pub fn run_broadcast_ui(
    tty: &mut dyn Tty,
    opts: &BroadcastOptions,
    mut payload: BroadcastPayload,
) -> io::Result<()> {
    let mut editor = LineEditor::default();
    let mut hide_input = false;
    let mut parser = KeyParser::new();
    tty.write_all(&session_escape_code(&payload, true).unwrap_or_default())?;
    tty.write_all(
        format!(
            "Type the text to broadcast below, press {} to quit:\r\n",
            opts.end_session
        )
        .as_bytes(),
    )?;
    for text in &opts.initial {
        payload.data = b64_text(text);
        tty.write_all(&payload.escape_code().unwrap_or_default())?;
    }
    loop {
        let key = match tui::read_key(tty, &mut parser, Duration::from_secs(1))? {
            Some(key) => key,
            None => continue,
        };
        if matches_binding(&key, &opts.hide_input_toggle) {
            hide_input = !hide_input;
            if hide_input {
                editor.clear();
                tty.write_all(b"\r\nInput hidden\r\n")?;
            }
            continue;
        }
        if matches_binding(&key, &opts.end_session) {
            break;
        }
        match key {
            Key::Ctrl('c') => {
                payload.data = b64_text("\x03");
                tty.write_all(&payload.escape_code().unwrap_or_default())?;
                editor.clear();
            }
            Key::Ctrl('d') => {
                payload.data = b64_text("\x04");
                tty.write_all(&payload.escape_code().unwrap_or_default())?;
            }
            Key::Enter => {
                payload.data = b64_text("\r");
                tty.write_all(&payload.escape_code().unwrap_or_default())?;
                tty.write_all(b"\r\n")?;
                editor.clear();
            }
            key if editor.handle_key(&key) => {
                if let Key::Char(ch) = key {
                    if !hide_input {
                        payload.data = b64_text(&ch.to_string());
                        tty.write_all(&payload.escape_code().unwrap_or_default())?;
                    }
                }
            }
            key => {
                let encoded = format!(
                    "kitty-key:{}",
                    String::from_utf8_lossy(&b64encode(key_name(&key).as_bytes(), true))
                );
                payload.data = encoded;
                tty.write_all(&payload.escape_code().unwrap_or_default())?;
            }
        }
        if !hide_input {
            let text = format!("\r{}\x1b[K", editor.as_string());
            tty.write_all(text.as_bytes())?;
        }
        tty.flush()?;
    }
    tty.write_all(&session_escape_code(&payload, false).unwrap_or_default())?;
    tty.flush()?;
    Ok(())
}

pub fn run_broadcast_cli(
    args: &[String],
    tty: &mut dyn Tty,
    out: &mut dyn Write,
) -> Result<(), String> {
    let opts = parse_broadcast_args(args)?;
    if opts.help {
        writeln!(out, "{BROADCAST_USAGE}").map_err(|err| err.to_string())?;
        return Ok(());
    }
    let payload = BroadcastPayload {
        match_spec: opts.match_spec.clone(),
        match_tab: opts.match_tab.clone(),
        all: opts.match_spec.is_empty() && opts.match_tab.is_empty(),
        ..BroadcastPayload::default()
    };
    run_broadcast_ui(tty, &opts, payload).map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::FakeTty;

    #[test]
    fn payload_shape_and_session_escapes() {
        let payload = BroadcastPayload {
            data: b64_text("hi"),
            ..BroadcastPayload::default()
        };
        let code = send_text_escape_code(&payload).unwrap();
        let text = String::from_utf8_lossy(&code);
        assert!(text.starts_with("\x1bP@kitty-cmd"));
        assert!(text.contains("\"cmd\":\"send-text\""));
        assert!(text.contains("\"no_response\":true"));
        assert!(text.contains("\"data\":\"base64:aGk=\""));
        let start =
            String::from_utf8_lossy(&session_escape_code(&payload, true).unwrap()).into_owned();
        assert!(start.contains("session:start"));
        let end =
            String::from_utf8_lossy(&session_escape_code(&payload, false).unwrap()).into_owned();
        assert!(end.contains("session:end"));
    }

    #[test]
    fn line_editor() {
        let mut editor = LineEditor::default();
        editor.insert("abc");
        editor.left();
        editor.insert("X");
        assert_eq!(editor.as_string(), "abXc");
        assert!(editor.backspace());
        assert_eq!(editor.as_string(), "abc");
        editor.home();
        assert_eq!(editor.cursor, 0);
        editor.end();
        assert_eq!(editor.cursor, 3);
    }

    #[test]
    fn parses_options_and_initial_text() {
        let args = ["-m", "id:1", "-t", "index:0", "hello", "world"]
            .iter()
            .map(|x| (*x).to_owned())
            .collect::<Vec<_>>();
        let opts = parse_broadcast_args(&args).unwrap();
        assert_eq!(opts.match_spec, "id:1");
        assert_eq!(opts.match_tab, "index:0");
        assert_eq!(opts.initial, vec!["hello", "world"]);
    }

    #[test]
    fn ui_broadcasts_text_and_ends() {
        let opts = BroadcastOptions {
            end_session: "ctrl+x".to_owned(),
            ..BroadcastOptions::default()
        };
        let mut tty = FakeTty::new(80, 24, b"hi\r\x18");
        let payload = BroadcastPayload {
            all: true,
            ..BroadcastPayload::default()
        };
        run_broadcast_ui(&mut tty, &opts, payload).unwrap();
        let text = tty.output_text();
        assert!(text.contains("session:start"));
        assert!(text.contains("base64:aA=="));
        assert!(text.contains("base64:aQ=="));
        assert!(text.contains("base64:DQ=="));
        assert!(text.contains("session:end"));
    }
}
