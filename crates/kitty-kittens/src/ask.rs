// SPDX-License-Identifier: GPL-3.0-or-later
//! Ask kitten, ported from `kittens/ask`.
//!
//! Provides the choice/yes-no engine, the input types (`line`, `password`,
//! `file`) and a minimal full-screen UI. The result is the JSON object emitted
//! by kitty's kitten output serializer.

use std::io::{self, Write};
use std::time::Duration;

use kitty_remote::json::Value;

use crate::tui::{self, Key, KeyParser, Tty};

pub const ASK_USAGE: &str = "\
Usage: kitten ask [options]

Ask the user for input.

Options:
  -t, --type line|yesno|choices|password|file
  -m, --message MESSAGE
  -n, --name NAME
      --title TITLE
  -c, --choice LETTER[;COLOR]:TEXT
  -d, --default VALUE
  -p, --prompt PROMPT
      --unhide-key KEY
      --hidden-text-placeholder TEXT
  -h, --help";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AskType {
    #[default]
    Line,
    YesNo,
    Choices,
    Password,
    File,
}

impl AskType {
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "line" => Self::Line,
            "yesno" => Self::YesNo,
            "choices" => Self::Choices,
            "password" => Self::Password,
            "file" => Self::File,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Choice {
    pub letter: String,
    pub color: String,
    pub text: String,
    pub index: usize,
}

impl Choice {
    pub fn parse(spec: &str) -> Result<Self, String> {
        let (letter_spec, text) = spec
            .split_once(':')
            .ok_or_else(|| format!("Invalid choice {spec:?}: missing ':'"))?;
        let (letter, color) = letter_spec
            .split_once(';')
            .map_or((letter_spec, ""), |(letter, color)| (letter, color));
        let letter = letter.trim().to_lowercase();
        if letter.chars().count() != 1 {
            return Err(format!(
                "The choice letter {letter:?} must be one character"
            ));
        }
        let lower_text = text.to_lowercase();
        let index = lower_text
            .find(&letter)
            .ok_or_else(|| format!("The choice letter {letter:?} is not present in {text:?}"))?;
        let index = lower_text[..index].chars().count();
        Ok(Self {
            letter,
            color: color.to_owned(),
            text: text.to_owned(),
            index,
        })
    }

    pub fn prefix(&self) -> String {
        self.text.chars().take(self.index).collect()
    }

    pub fn display_letter(&self) -> String {
        self.text
            .chars()
            .nth(self.index)
            .map(String::from)
            .unwrap_or_default()
    }

    pub fn suffix(&self) -> String {
        self.text.chars().skip(self.index + 1).collect()
    }
}

#[derive(Clone, Debug)]
pub struct AskOptions {
    pub kind: AskType,
    pub message: String,
    pub name: String,
    pub title: String,
    pub choices: Vec<String>,
    pub default: String,
    pub prompt: String,
    pub unhide_key: String,
    pub hidden_text_placeholder: String,
    pub items: Vec<String>,
    pub help: bool,
}

impl Default for AskOptions {
    fn default() -> Self {
        Self {
            kind: AskType::Line,
            message: String::new(),
            name: String::new(),
            title: String::new(),
            choices: Vec::new(),
            default: String::new(),
            prompt: "> ".to_owned(),
            unhide_key: "u".to_owned(),
            hidden_text_placeholder: String::new(),
            items: Vec::new(),
            help: false,
        }
    }
}

pub fn parse_ask_args(args: &[String]) -> Result<AskOptions, String> {
    let mut opts = AskOptions::default();
    let mut index = 0usize;
    while index < args.len() {
        let arg = &args[index];
        if arg == "--" {
            opts.items.extend_from_slice(&args[index + 1..]);
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
            "-t" | "--type" => {
                let value = take();
                opts.kind =
                    AskType::parse(&value).ok_or_else(|| format!("Unknown type: {value}"))?;
            }
            "-m" | "--message" => opts.message = take(),
            "-n" | "--name" => opts.name = take(),
            "--title" | "--window-title" => opts.title = take(),
            "-c" | "--choice" => opts.choices.push(take()),
            "-d" | "--default" => opts.default = take(),
            "-p" | "--prompt" => opts.prompt = take(),
            "--unhide-key" => opts.unhide_key = take(),
            "--hidden-text-placeholder" => opts.hidden_text_placeholder = take(),
            other if !other.starts_with('-') => opts.items.push(other.to_owned()),
            other => return Err(format!("unknown option: {other}")),
        }
        index += 1;
    }
    if opts.prompt.len() > 2 {
        let first = opts.prompt.chars().next().unwrap_or(' ');
        let last = opts.prompt.chars().last().unwrap_or(' ');
        if first == last && (first == '"' || first == '\'') {
            opts.prompt = opts.prompt[1..opts.prompt.len() - 1].to_owned();
        }
    }
    Ok(opts)
}

/// Validated choices plus the value accepted by Enter and the allowed letters.
#[derive(Clone, Debug, Default)]
pub struct ChoiceSet {
    pub choices: Vec<Choice>,
    pub response_on_accept: String,
    pub allowed: Vec<String>,
}

pub fn build_choices(opts: &AskOptions) -> Result<ChoiceSet, String> {
    let mut out = ChoiceSet::default();
    match opts.kind {
        AskType::YesNo => {
            out.allowed = vec!["y".to_owned(), "n".to_owned()];
            out.response_on_accept = if out.allowed.contains(&opts.default) {
                opts.default.clone()
            } else {
                "y".to_owned()
            };
        }
        AskType::Choices => {
            for spec in &opts.choices {
                let choice = Choice::parse(spec)?;
                out.allowed.push(choice.letter.clone());
                out.choices.push(choice);
            }
            let first = out
                .choices
                .first()
                .map(|choice| choice.letter.clone())
                .unwrap_or_default();
            out.response_on_accept = if out.allowed.contains(&opts.default) {
                opts.default.clone()
            } else {
                first
            };
        }
        _ => {}
    }
    Ok(out)
}

/// The response object emitted to the terminal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AskResponse {
    pub items: Vec<String>,
    pub response: Option<String>,
}

impl AskResponse {
    pub fn to_json(&self) -> Result<String, String> {
        let items = Value::Array(
            self.items
                .iter()
                .map(|item| Value::String(item.clone()))
                .collect(),
        );
        let response = match &self.response {
            Some(value) => Value::String(value.clone()),
            None => Value::Null,
        };
        Value::object_from([("items", items), ("response", response)])
            .to_json()
            .map_err(|err| err.to_string())
    }
}

/// Engine for a yes/no or choices prompt: returns `Some(response)` when the
/// key accepts, `Some("")`/`None` semantics are handled by the caller.
pub fn choice_text_response(set: &ChoiceSet, text: &str, unhide: Option<&str>) -> ChoiceResponse {
    let lower = text.to_lowercase();
    if set.allowed.contains(&lower) {
        return ChoiceResponse::Accept(lower);
    }
    if let Some(key) = unhide {
        if lower == key {
            return ChoiceResponse::Unhide;
        }
    }
    ChoiceResponse::Reject
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChoiceResponse {
    Accept(String),
    Unhide,
    Reject,
}

/// Full-screen choice UI. Returns the selected letter or `None` on cancel.
pub fn run_choice_ui(
    tty: &mut dyn Tty,
    opts: &AskOptions,
    set: &ChoiceSet,
    hidden_text: &mut Option<String>,
) -> io::Result<Option<String>> {
    let mut parser = KeyParser::new();
    let mut response = None;
    tui::enter_screen(tty)?;
    loop {
        tui::clear_screen(tty)?;
        if !opts.title.is_empty() {
            tty.write_all(format!("\x1b]2;{}\x1b\\", opts.title).as_bytes())?;
        }
        let mut text = String::new();
        if !opts.message.is_empty() {
            text.push_str(&opts.message);
            text.push_str("\r\n\r\n");
        }
        match opts.kind {
            AskType::YesNo => {
                text.push_str("  \x1b[32mY\x1b[0mes   \x1b[31mN\x1b[0mo\r\n");
            }
            AskType::Choices => {
                for choice in &set.choices {
                    let color = if choice.color.is_empty() {
                        "green"
                    } else {
                        &choice.color
                    };
                    text.push_str(&format!(
                        "  {}\x1b[{}m{}\x1b[0m{}\r\n",
                        choice.prefix(),
                        color,
                        choice.display_letter(),
                        choice.suffix()
                    ));
                }
            }
            _ => {}
        }
        text.push_str("\r\nEsc: cancel");
        tty.write_all(text.as_bytes())?;
        tty.flush()?;
        let Some(key) = tui::read_key(tty, &mut parser, Duration::from_secs(1))? else {
            continue;
        };
        match key {
            Key::Esc | Key::Ctrl('c') => break,
            Key::Enter => {
                response = Some(set.response_on_accept.clone());
                break;
            }
            Key::Char(ch) => {
                let mut q = ch.to_string();
                if let Some(hidden) = hidden_text.as_mut() {
                    q = q.to_lowercase();
                    if let Some(key) = hidden.find('|').map(|_| String::new()) {
                        let _ = key;
                    }
                }
                match choice_text_response(set, &q, Some(&opts.unhide_key)) {
                    ChoiceResponse::Accept(value) => {
                        response = Some(value);
                        break;
                    }
                    ChoiceResponse::Unhide => {
                        if let Some(hidden) = hidden_text.take() {
                            if let Some(pos) = opts.message.find(&opts.hidden_text_placeholder) {
                                let suffix =
                                    &opts.message[pos + opts.hidden_text_placeholder.len()..];
                                let _ = (pos, suffix, hidden);
                            }
                        }
                    }
                    ChoiceResponse::Reject => {
                        if opts.kind == AskType::YesNo {
                            break;
                        }
                    }
                }
            }
            _ => {}
        }
    }
    tui::leave_screen(tty)?;
    Ok(response)
}

/// Read a line/password/file value. Hidden input never echoes characters.
pub fn run_line_ui(
    tty: &mut dyn Tty,
    opts: &AskOptions,
    password: bool,
) -> io::Result<Option<String>> {
    let mut parser = KeyParser::new();
    let mut text = String::new();
    let mut cursor = 0usize;
    if !opts.message.is_empty() {
        tty.write_all(opts.message.as_bytes())?;
        tty.write_all(b"\r\n")?;
    }
    if !opts.title.is_empty() {
        tty.write_all(format!("\x1b]2;{}\x1b\\", opts.title).as_bytes())?;
    }
    if !opts.default.is_empty() && opts.kind != AskType::Password {
        text = opts.default.clone();
        cursor = text.chars().count();
    }
    loop {
        if password {
            let mask = "•".repeat(text.chars().count());
            tty.write_all(format!("\r{}{}\x1b[K", opts.prompt, mask).as_bytes())?;
        } else {
            tty.write_all(format!("\r{}{}\x1b[K", opts.prompt, text).as_bytes())?;
        }
        tty.flush()?;
        let Some(key) = tui::read_key(tty, &mut parser, Duration::from_secs(1))? else {
            continue;
        };
        match key {
            Key::Enter => {
                tty.write_all(b"\r\n")?;
                return Ok(Some(text));
            }
            Key::Esc | Key::Ctrl('c') => {
                tty.write_all(b"\r\n")?;
                return Ok(None);
            }
            Key::Backspace => {
                if cursor > 0 {
                    cursor -= 1;
                    let byte = text
                        .char_indices()
                        .nth(cursor)
                        .map(|(i, _)| i)
                        .unwrap_or(text.len());
                    text.remove(byte);
                }
            }
            Key::Left => cursor = cursor.saturating_sub(1),
            Key::Right => {
                if cursor < text.chars().count() {
                    cursor += 1;
                }
            }
            Key::Home => cursor = 0,
            Key::End => cursor = text.chars().count(),
            Key::Char(ch) => {
                let byte = text
                    .char_indices()
                    .nth(cursor)
                    .map(|(i, _)| i)
                    .unwrap_or(text.len());
                text.insert(byte, ch);
                cursor += 1;
            }
            Key::Ctrl('u') => {
                text.clear();
                cursor = 0;
            }
            _ => {}
        }
    }
}

/// Read hidden text from STDIN for `--hidden-text-placeholder`.
pub fn read_hidden_text() -> io::Result<String> {
    let mut text = String::new();
    io::stdin().read_line(&mut text)?;
    Ok(text.trim_end().to_owned())
}

pub fn run_ask(opts: &AskOptions, tty: &mut dyn Tty) -> Result<AskResponse, String> {
    if opts.help {
        return Ok(AskResponse {
            items: opts.items.clone(),
            response: None,
        });
    }
    let mut hidden = None;
    let response = match opts.kind {
        AskType::YesNo | AskType::Choices => {
            let set = build_choices(opts)?;
            run_choice_ui(tty, opts, &set, &mut hidden).map_err(|err| err.to_string())?
        }
        AskType::Password => run_line_ui(tty, opts, true).map_err(|err| err.to_string())?,
        AskType::Line | AskType::File => {
            run_line_ui(tty, opts, false).map_err(|err| err.to_string())?
        }
    };
    Ok(AskResponse {
        items: opts.items.clone(),
        response,
    })
}

pub fn run_ask_cli(args: &[String], tty: &mut dyn Tty, out: &mut dyn Write) -> Result<(), String> {
    let opts = parse_ask_args(args)?;
    if opts.help {
        writeln!(out, "{ASK_USAGE}").map_err(|err| err.to_string())?;
        return Ok(());
    }
    let response = run_ask(&opts, tty)?;
    writeln!(out, "{}", response.to_json()?).map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::FakeTty;

    #[test]
    fn choice_specs_and_defaults() {
        let choice = Choice::parse("n;red:No").unwrap();
        assert_eq!(choice.prefix(), "");
        assert_eq!(choice.display_letter(), "N");
        assert_eq!(choice.suffix(), "o");
        assert_eq!(choice.color, "red");
        assert!(Choice::parse("z:Yes").is_err());
        let opts = AskOptions {
            kind: AskType::YesNo,
            ..AskOptions::default()
        };
        let set = build_choices(&opts).unwrap();
        assert_eq!(set.response_on_accept, "y");
        let opts = AskOptions {
            kind: AskType::Choices,
            choices: vec!["y:Yes".to_owned(), "n:No".to_owned()],
            default: "n".to_owned(),
            ..AskOptions::default()
        };
        assert_eq!(build_choices(&opts).unwrap().response_on_accept, "n");
    }

    #[test]
    fn option_parsing_strips_prompt_quotes() {
        let args = ["-t=yesno", "--message", "Question?", "--prompt", "\"$ \""]
            .iter()
            .map(|x| (*x).to_owned())
            .collect::<Vec<_>>();
        let opts = parse_ask_args(&args).unwrap();
        assert_eq!(opts.kind, AskType::YesNo);
        assert_eq!(opts.message, "Question?");
        assert_eq!(opts.prompt, "$ ");
    }

    #[test]
    fn response_json_shape() {
        let response = AskResponse {
            items: vec!["flag".to_owned()],
            response: Some("yes".to_owned()),
        };
        assert_eq!(
            response.to_json().unwrap(),
            r#"{"items":["flag"],"response":"yes"}"#
        );
    }

    #[test]
    fn yesno_ui_and_choices_ui() {
        let opts = AskOptions {
            kind: AskType::YesNo,
            message: "Continue?".to_owned(),
            items: vec!["x".to_owned()],
            ..AskOptions::default()
        };
        let mut tty = FakeTty::new(80, 24, b"n");
        let response = run_ask(&opts, &mut tty).unwrap();
        assert_eq!(response.response.as_deref(), Some("n"));
        assert!(tty.output_text().contains("Continue?"));

        let opts = AskOptions {
            kind: AskType::Choices,
            choices: vec!["a:Apple".to_owned(), "b:Banana".to_owned()],
            ..AskOptions::default()
        };
        let mut tty = FakeTty::new(80, 24, b"b");
        let response = run_ask(&opts, &mut tty).unwrap();
        assert_eq!(response.response.as_deref(), Some("b"));
    }

    #[test]
    fn line_and_password_ui() {
        let opts = AskOptions {
            kind: AskType::Line,
            prompt: "> ".to_owned(),
            items: vec![],
            ..AskOptions::default()
        };
        let mut tty = FakeTty::new(80, 24, b"hello\r");
        let response = run_ask(&opts, &mut tty).unwrap();
        assert_eq!(response.response.as_deref(), Some("hello"));
        let opts = AskOptions {
            kind: AskType::Password,
            ..AskOptions::default()
        };
        let mut tty = FakeTty::new(80, 24, b"secret\r");
        let response = run_ask(&opts, &mut tty).unwrap();
        assert_eq!(response.response.as_deref(), Some("secret"));
        assert!(!tty.output_text().contains("secret"));
    }
}
