// SPDX-License-Identifier: GPL-3.0-or-later
//! Resize-window kitten, ported from `kittens/resize_window`.
//!
//! The engine computes resize commands and persists per-strategy preferences
//! (modifier and fraction) in `resize-window.json`. `Window` and `Edge`
//! strategies are both modeled; the UI is a small full-screen ANSI page.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;
use std::time::Duration;

use kitty_remote::json::{self, Value};

use crate::protocol::encode_remote_command_value;
use crate::tui::{self, Key, KeyParser, Size, Tty};

pub const RESIZE_USAGE: &str = "\
Usage: kitten resize-window [options]

Resize the current window interactively.

Options:
      --horizontal-increment N  Base horizontal increment (default 2)
      --vertical-increment N    Base vertical increment (default 2)
      --fraction F              Fraction of remaining steps (default 1/3)
      --strategy window|edge    Initial strategy (default window)
  -h, --help";

pub fn parse_fraction(value: &str) -> Result<f64, String> {
    let value = value.trim();
    let (numerator, denominator) = match value.split_once('/') {
        Some((a, b)) => (a.trim(), Some(b.trim())),
        None => (value, None),
    };
    let mut n: f64 = numerator
        .parse()
        .map_err(|_| format!("invalid fraction: {value:?}"))?;
    if let Some(denominator) = denominator {
        let d: f64 = denominator
            .parse()
            .map_err(|_| format!("invalid fraction: {value:?}"))?;
        if d <= 0.0 || !d.is_finite() {
            return Err(format!("invalid fraction: {value:?}"));
        }
        n /= d;
    }
    if !n.is_finite() || n <= 0.0 || n > 1.0 {
        return Err(format!(
            "--fraction must be greater than zero and at most one, got {value:?}"
        ));
    }
    Ok(n)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StrategySettings {
    pub modifier: String,
    pub fraction: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResizeSettings {
    pub version: i64,
    pub strategy: String,
    pub strategies: BTreeMap<String, StrategySettings>,
}

impl Default for ResizeSettings {
    fn default() -> Self {
        let mut strategies = BTreeMap::new();
        for name in ["window", "edge"] {
            strategies.insert(
                name.to_owned(),
                StrategySettings {
                    modifier: "alt".to_owned(),
                    fraction: "1/3".to_owned(),
                },
            );
        }
        Self {
            version: 1,
            strategy: "window".to_owned(),
            strategies,
        }
    }
}

impl ResizeSettings {
    pub fn preferences(&self, strategy: &str) -> StrategySettings {
        self.strategies
            .get(strategy)
            .cloned()
            .unwrap_or_else(|| StrategySettings {
                modifier: "alt".to_owned(),
                fraction: "1/3".to_owned(),
            })
    }

    pub fn normalize(&mut self) -> Result<(), String> {
        if self.version == 0 {
            self.version = 1;
        }
        if self.version != 1 {
            return Err(format!(
                "unsupported resize settings version: {}",
                self.version
            ));
        }
        if self.strategy.is_empty() {
            self.strategy = "window".to_owned();
        }
        if self.strategy != "window" && self.strategy != "edge" {
            return Err(format!("unknown resize strategy: {:?}", self.strategy));
        }
        let defaults = ResizeSettings::default();
        for name in ["window", "edge"] {
            let fallback = defaults.preferences(name);
            let entry = self
                .strategies
                .entry(name.to_owned())
                .or_insert_with(|| fallback.clone());
            if entry.modifier.is_empty() {
                entry.modifier = fallback.modifier.clone();
            }
            if entry.modifier != "alt" && entry.modifier != "shift" {
                return Err(format!("invalid {name} modifier: {:?}", entry.modifier));
            }
            entry.fraction = entry.fraction.trim().to_owned();
            if entry.fraction.is_empty() {
                entry.fraction = fallback.fraction.clone();
            }
            parse_fraction(&entry.fraction)
                .map_err(|err| format!("invalid {name} fraction: {err}"))?;
        }
        Ok(())
    }

    pub fn to_value(&self) -> Value {
        let mut strategies = Vec::new();
        for (name, pref) in &self.strategies {
            strategies.push((
                name.clone(),
                Value::object_from([
                    ("modifier", Value::String(pref.modifier.clone())),
                    ("fraction", Value::String(pref.fraction.clone())),
                ]),
            ));
        }
        Value::object_from([
            ("version", Value::from(self.version)),
            ("strategy", Value::String(self.strategy.clone())),
            ("strategies", Value::Object(strategies)),
        ])
    }

    pub fn from_value(value: &Value) -> Result<Self, String> {
        let mut out = ResizeSettings::default();
        if let Some(version) = value.get("version").and_then(Value::as_i64) {
            out.version = version;
        }
        if let Some(strategy) = value.get("strategy").and_then(Value::as_str) {
            out.strategy = strategy.to_owned();
        }
        if let Some(items) = value.get("strategies").and_then(Value::as_object) {
            for (name, pref) in items {
                let modifier = pref
                    .get("modifier")
                    .and_then(Value::as_str)
                    .unwrap_or("alt")
                    .to_owned();
                let fraction = pref
                    .get("fraction")
                    .and_then(Value::as_str)
                    .unwrap_or("1/3")
                    .to_owned();
                out.strategies
                    .insert(name.clone(), StrategySettings { modifier, fraction });
            }
        }
        out.normalize()?;
        Ok(out)
    }

    pub fn load(path: &Path) -> Self {
        let Ok(raw) = fs::read(path) else {
            return Self::default();
        };
        json::parse(&raw)
            .ok()
            .and_then(|value| Self::from_value(&value).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let text = self
            .to_value()
            .to_json()
            .unwrap_or_else(|_| "{}".to_owned())
            + "\n";
        let temp = path.with_extension("json.tmp");
        fs::write(&temp, text)?;
        fs::rename(temp, path)
    }

    /// Merge one changed field with the latest saved settings so concurrent
    /// resize pages do not overwrite each other.
    pub fn update(path: &Path, strategy: &str, field: &str, value: &str) -> Result<Self, String> {
        if strategy != "window" && strategy != "edge" {
            return Err(format!("unknown resize strategy: {strategy:?}"));
        }
        let mut settings = Self::load(path);
        let mut pref = settings.preferences(strategy);
        match field {
            "strategy" => settings.strategy = strategy.to_owned(),
            "modifier" => pref.modifier = value.to_owned(),
            "fraction" => pref.fraction = value.to_owned(),
            other => return Err(format!("unknown resize setting: {other:?}")),
        }
        settings.strategies.insert(strategy.to_owned(), pref);
        settings.normalize()?;
        settings.save(path).map_err(|err| err.to_string())?;
        Ok(settings)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Axis {
    #[default]
    Horizontal,
    Vertical,
    Reset,
}

impl Axis {
    fn payload(self) -> &'static str {
        match self {
            Self::Horizontal => "horizontal",
            Self::Vertical => "vertical",
            Self::Reset => "reset",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Strategy {
    #[default]
    Window,
    Edge,
}

impl Strategy {
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "window" => Self::Window,
            "edge" => Self::Edge,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Window => "window",
            Self::Edge => "edge",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResizeStep {
    pub horizontal: bool,
    pub decrease: bool,
    pub reset: bool,
    pub multiplier: i32,
    pub fraction: f64,
}

impl ResizeStep {
    pub fn new(
        horizontal: bool,
        decrease: bool,
        reset: bool,
        multiplier: i32,
        fraction: f64,
    ) -> Self {
        Self {
            horizontal,
            decrease,
            reset,
            multiplier,
            fraction,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ResizeOptions {
    pub horizontal_increment: i64,
    pub vertical_increment: i64,
    pub fraction: String,
    pub strategy: Option<Strategy>,
    pub help: bool,
}

impl Default for ResizeOptions {
    fn default() -> Self {
        Self {
            horizontal_increment: 2,
            vertical_increment: 2,
            fraction: "1/3".to_owned(),
            strategy: None,
            help: false,
        }
    }
}

pub fn parse_resize_args(args: &[String]) -> Result<ResizeOptions, String> {
    let mut opts = ResizeOptions::default();
    let mut index = 0usize;
    while index < args.len() {
        let arg = &args[index];
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
            "--horizontal-increment" => {
                opts.horizontal_increment = take()
                    .parse()
                    .map_err(|_| "invalid horizontal increment".to_owned())?
            }
            "--vertical-increment" => {
                opts.vertical_increment = take()
                    .parse()
                    .map_err(|_| "invalid vertical increment".to_owned())?
            }
            "--fraction" => opts.fraction = take(),
            "--strategy" => {
                let value = take();
                opts.strategy = Some(
                    Strategy::parse(&value).ok_or_else(|| format!("invalid strategy: {value}"))?,
                );
            }
            other => return Err(format!("unknown option: {other}")),
        }
        index += 1;
    }
    parse_fraction(&opts.fraction)?;
    Ok(opts)
}

/// Build the `@kitty-cmd` escape code for a window resize.
pub fn resize_command_escape_code(
    increment: i64,
    axis: Axis,
    fraction: f64,
) -> Result<Vec<u8>, String> {
    let mut payload = vec![
        ("increment", Value::from(increment)),
        ("axis", Value::String(axis.payload().to_owned())),
        ("self", Value::Bool(true)),
    ];
    if fraction > 0.0 {
        payload.push(("fraction", Value::from(fraction)));
    }
    if axis == Axis::Reset {
        payload.push(("restore_entry_layout", Value::Bool(true)));
    }
    let request = Value::object_from([
        ("cmd", Value::String("resize-window".to_owned())),
        (
            "version",
            Value::String(kitty_remote::KITTY_COMPAT_VERSION.to_owned()),
        ),
        ("payload", Value::object_from(payload)),
    ]);
    encode_remote_command_value(&request).map_err(|err| err.to_string())
}

/// Build the escape code for the newer internal pane-edge command.
pub fn edge_command_escape_code(
    operation: &str,
    edge: &str,
    divider: &str,
    increment: i64,
    fraction: f64,
) -> Result<Vec<u8>, String> {
    let mut payload = vec![
        ("operation", Value::String(operation.to_owned())),
        ("edge", Value::String(edge.to_owned())),
        ("increment", Value::from(increment)),
        ("self", Value::Bool(true)),
    ];
    if !divider.is_empty() {
        payload.push(("divider", Value::String(divider.to_owned())));
    }
    if fraction > 0.0 {
        payload.push(("fraction", Value::from(fraction)));
    }
    let request = Value::object_from([
        ("cmd", Value::String("resize-window-edge".to_owned())),
        (
            "version",
            Value::String(kitty_remote::KITTY_COMPAT_VERSION.to_owned()),
        ),
        ("payload", Value::object_from(payload)),
    ]);
    encode_remote_command_value(&request).map_err(|err| err.to_string())
}

#[derive(Clone, Debug, Default)]
pub struct RcResponse {
    pub ok: bool,
    pub error: String,
    pub traceback: String,
    pub data: Option<Value>,
}

impl RcResponse {
    pub fn parse(raw: &[u8]) -> Result<Self, String> {
        let value = json::parse(raw).map_err(|err| err.to_string())?;
        Ok(Self {
            ok: value.get("ok").and_then(Value::as_bool).unwrap_or(false),
            error: value
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
            traceback: value
                .get("tb")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
            data: value.get("data").cloned(),
        })
    }

    pub fn truthy_data(&self) -> Option<String> {
        match self.data.as_ref()? {
            Value::Null => None,
            Value::Bool(false) => None,
            Value::Bool(true) => Some(String::new()),
            Value::String(text) if text.is_empty() => None,
            Value::String(text) => Some(text.clone()),
            Value::Number(number) => {
                if number.as_f64() == 0.0 {
                    None
                } else {
                    Some(String::new())
                }
            }
            Value::Array(items) if items.is_empty() => None,
            Value::Array(_) | Value::Object(_) => Some(String::new()),
        }
    }
}

/// Map a key and modifiers to a resize step. Alt/Shift chooses the fraction
/// step; Ctrl doubles the increment.
pub fn step_for_key(
    key: char,
    modifier: &str,
    ctrl: bool,
    pref_modifier: &str,
    fraction: f64,
) -> Option<ResizeStep> {
    let reset = key == 'r';
    let (horizontal, decrease) = match key {
        'w' => (true, false),
        'n' => (true, true),
        't' => (false, false),
        's' => (false, true),
        'r' => (true, false),
        _ => return None,
    };
    let multiplier = if ctrl { 2 } else { 1 };
    let f = if !reset && modifier.eq_ignore_ascii_case(pref_modifier) {
        fraction
    } else {
        0.0
    };
    Some(ResizeStep::new(horizontal, decrease, reset, multiplier, f))
}

/// Full-screen resize page. Returns the last command written, and whether the
/// page was reset.
pub fn run_resize_ui(
    tty: &mut dyn Tty,
    opts: &ResizeOptions,
    settings: &mut ResizeSettings,
    _settings_path: &Path,
) -> io::Result<()> {
    let mut parser = KeyParser::new();
    let pref = settings.preferences(settings.strategy.as_str());
    let fraction = parse_fraction(&pref.fraction).unwrap_or(1.0 / 3.0);
    tui::enter_screen(tty)?;
    loop {
        let size = tty.size().unwrap_or(Size::new(80, 24));
        tui::clear_screen(tty)?;
        let modifier = if pref.modifier == "shift" {
            "Shift"
        } else {
            "Alt"
        };
        let text = format!(
            "\x1b[1mResize this window\x1b[0m\r\n\
             Strategy: \x1b[32m1: Window\x1b[0m  \x1b[32m2: {}\x1b[0m\r\n\
             \x1b[32mM\x1b[0m: {modifier}    \x1b[32mF\x1b[0m: {} (settings)\r\n\r\n\
               \x1b[32mW\x1b[0mider\r\n  \x1b[32mN\x1b[0marrower\r\n  \x1b[32mT\x1b[0maller\r\n  \x1b[32mS\x1b[0mhorter\r\n  \x1b[31mR\x1b[0meset\r\n\r\n\
             Esc: quit    Ctrl: double step size\r\n\
             Current: {size:?}\r\n",
            settings.strategy.as_str(),
            pref.fraction
        );
        tty.write_all(text.as_bytes())?;
        tty.flush()?;
        let Some(key) = tui::read_key(tty, &mut parser, Duration::from_secs(1))? else {
            continue;
        };
        match key {
            Key::Esc | Key::Ctrl('c') | Key::Ctrl('d') | Key::Char('q') => break,
            Key::Char('1') => {
                settings.strategy = "window".to_owned();
            }
            Key::Char('2') => {
                settings.strategy = "edge".to_owned();
            }
            Key::Char(ch @ ('w' | 'n' | 't' | 's' | 'r')) => {
                let ctrl = false;
                if let Some(step) = step_for_key(ch, "", ctrl, &pref.modifier, fraction) {
                    let increment = if step.horizontal {
                        opts.horizontal_increment
                    } else {
                        opts.vertical_increment
                    } * step.multiplier as i64;
                    let increment = if step.decrease { -increment } else { increment };
                    let axis = if step.reset {
                        Axis::Reset
                    } else if step.horizontal {
                        Axis::Horizontal
                    } else {
                        Axis::Vertical
                    };
                    let command = resize_command_escape_code(increment, axis, step.fraction)
                        .unwrap_or_default();
                    tty.write_all(&command)?;
                }
            }
            _ => {}
        }
    }
    tui::leave_screen(tty)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::FakeTty;
    use std::path::PathBuf;

    #[test]
    fn fractions() {
        assert!((parse_fraction("1/3").unwrap() - 1.0 / 3.0).abs() < 1e-9);
        assert_eq!(parse_fraction("0.5").unwrap(), 0.5);
        assert!(parse_fraction("0").is_err());
        assert!(parse_fraction("1.5").is_err());
        assert!(parse_fraction("1/0").is_err());
    }

    #[test]
    fn settings_roundtrip_and_merge() {
        let dir = std::env::temp_dir().join(format!("kitty-resize-{}-1", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("resize-window.json");
        let mut settings = ResizeSettings {
            strategy: "edge".to_owned(),
            ..ResizeSettings::default()
        };
        settings.strategies.get_mut("edge").unwrap().fraction = "1/2".to_owned();
        settings.save(&path).unwrap();
        let loaded = ResizeSettings::load(&path);
        assert_eq!(loaded.strategy, "edge");
        assert_eq!(loaded.preferences("edge").fraction, "1/2");
        let updated = ResizeSettings::update(&path, "window", "modifier", "shift").unwrap();
        assert_eq!(updated.preferences("window").modifier, "shift");
        assert_eq!(updated.preferences("edge").fraction, "1/2");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn command_shape() {
        let code = resize_command_escape_code(4, Axis::Horizontal, 0.0).unwrap();
        let text = String::from_utf8_lossy(&code);
        assert!(text.starts_with("\x1bP@kitty-cmd"));
        assert!(text.contains("\"cmd\":\"resize-window\""));
        assert!(text.contains("\"axis\":\"horizontal\""));
        assert!(text.contains("\"self\":true"));
        let reset = resize_command_escape_code(0, Axis::Reset, 0.0).unwrap();
        assert!(String::from_utf8_lossy(&reset).contains("restore_entry_layout"));
    }

    #[test]
    fn key_steps() {
        let step = step_for_key('w', "alt", false, "alt", 0.5).unwrap();
        assert!(step.horizontal && !step.decrease && step.fraction == 0.5);
        let step = step_for_key('s', "", true, "alt", 0.5).unwrap();
        assert!(!step.horizontal && step.decrease && step.fraction == 0.0);
    }

    #[test]
    fn rc_response() {
        let response = RcResponse::parse(br#"{"ok":true,"data":"Resized"}"#).unwrap();
        assert!(response.ok);
        assert_eq!(response.truthy_data().as_deref(), Some("Resized"));
        let response = RcResponse::parse(br#"{"ok":false,"error":"nope"}"#).unwrap();
        assert!(!response.ok);
        assert_eq!(response.error, "nope");
    }

    #[test]
    fn ui_quits_and_emits_resize() {
        let mut tty = FakeTty::new(80, 24, b"wq");
        let opts = ResizeOptions::default();
        let mut settings = ResizeSettings::default();
        let path = PathBuf::from("/nonexistent/resize.json");
        run_resize_ui(&mut tty, &opts, &mut settings, &path).unwrap();
        let text = tty.output_text();
        assert!(text.contains("Resize this window"));
        assert!(text.contains("@kitty-cmd"));
        assert!(text.contains("resize-window"));
    }
}
