// SPDX-License-Identifier: GPL-3.0-or-later
//! Session file parsing and serialisation, ported from ``kitty/session.py``.
//!
//! Only the CPU-side definition is modelled: each window points at an opaque
//! launch specification, cwd, env vars and user vars. A [`RestorePlan`] is
//! produced for a future PTY/window layer to execute.

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::path::{Component, Path, PathBuf};

use crate::layout::parse_layout_names;
use crate::model::{LaunchSpec, OsWindowId, WindowId};
use crate::tab::{Tab, TabManager};
use crate::value::Value;

pub const UNSERIALIZE_LAUNCH_FLAG: &str = "kitty-unserialize-data=";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowSize {
    pub size: u32,
    pub unit: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowSizes {
    pub width: WindowSize,
    pub height: WindowSize,
}

#[derive(Clone, Debug)]
pub struct SessionOptions {
    pub enabled_layouts: Vec<String>,
    /// Used when a session has a tab with no explicit launch command.
    pub shell: String,
}

impl Default for SessionOptions {
    fn default() -> Self {
        Self {
            enabled_layouts: vec!["tall".into()],
            shell: std::env::var("SHELL").unwrap_or_else(|_| "sh".into()),
        }
    }
}

#[derive(Clone, Debug)]
pub struct WindowSpec {
    pub launch: LaunchSpec,
    pub serialized_id: WindowId,
    pub resize_spec: Option<(String, i64)>,
    pub focus_matching_window: Option<String>,
    pub run_command_at_shell_startup: Vec<String>,
    pub is_background_process: bool,
}

impl WindowSpec {
    fn new(launch: LaunchSpec) -> Self {
        let is_background = launch.background;
        Self {
            launch,
            serialized_id: 0,
            resize_spec: None,
            focus_matching_window: None,
            run_command_at_shell_startup: Vec::new(),
            is_background_process: is_background,
        }
    }
}

#[derive(Clone, Debug)]
pub struct SessionTab {
    pub name: String,
    pub windows: Vec<WindowSpec>,
    pub enabled_layouts: Vec<String>,
    pub layout: String,
    pub layout_state: Option<Value>,
    pub cwd: Option<PathBuf>,
    pub next_title: Option<String>,
    pub active_window_idx: usize,
}

impl SessionTab {
    fn new(opts: &SessionOptions, name: &str) -> Self {
        let enabled = if opts.enabled_layouts.is_empty() {
            vec!["tall".to_string()]
        } else {
            opts.enabled_layouts.clone()
        };
        let layout = enabled[0].clone();
        Self {
            name: name.trim().to_string(),
            windows: Vec::new(),
            enabled_layouts: enabled,
            layout,
            layout_state: None,
            cwd: None,
            next_title: None,
            active_window_idx: 0,
        }
    }

    pub fn has_non_background_processes(&self) -> bool {
        self.windows.iter().any(|w| !w.is_background_process)
    }
}

#[derive(Clone, Debug)]
pub struct Session {
    pub tabs: Vec<SessionTab>,
    pub active_tab_idx: usize,
    pub focus_tab_spec: Option<String>,
    pub default_title: Option<String>,
    pub os_window_size: Option<WindowSizes>,
    pub os_window_position: Option<(i32, i32)>,
    pub os_window_class: Option<String>,
    pub os_window_name: Option<String>,
    pub os_window_state: Option<String>,
    pub os_window_title: Option<String>,
    pub focus_os_window: bool,
    pub session_name: String,
    pub num_of_windows_in_definition: usize,
    opts: SessionOptions,
}

impl Session {
    pub fn new(opts: &SessionOptions) -> Self {
        Self {
            tabs: Vec::new(),
            active_tab_idx: 0,
            focus_tab_spec: None,
            default_title: None,
            os_window_size: None,
            os_window_position: None,
            os_window_class: None,
            os_window_name: None,
            os_window_state: None,
            os_window_title: None,
            focus_os_window: false,
            session_name: String::new(),
            num_of_windows_in_definition: 0,
            opts: opts.clone(),
        }
    }

    pub fn add_tab(&mut self, name: &str) {
        if self
            .tabs
            .last()
            .map(|t| t.windows.is_empty())
            .unwrap_or(false)
        {
            self.tabs.pop();
        }
        self.tabs.push(SessionTab::new(&self.opts, name));
    }

    pub fn set_next_title(&mut self, title: &str) {
        if let Some(t) = self.tabs.last_mut() {
            t.next_title = Some(title.trim().to_string());
        }
    }

    pub fn set_layout(&mut self, val: &str) -> Result<(), String> {
        let base = val.split(':').next().unwrap_or("");
        if !crate::layout::is_layout_name(base) {
            return Err(format!("{val} is not a valid layout"));
        }
        if let Some(t) = self.tabs.last_mut() {
            t.layout = val.to_string();
        }
        Ok(())
    }

    pub fn set_enabled_layouts(&mut self, raw: &str) -> Result<(), String> {
        let enabled = parse_layout_names(raw)?;
        if let Some(t) = self.tabs.last_mut() {
            t.enabled_layouts = enabled;
            if !t.enabled_layouts.contains(&t.layout) {
                t.layout = t.enabled_layouts[0].clone();
            }
        }
        Ok(())
    }

    pub fn set_cwd(&mut self, val: &str, session_base_dir: &Path) {
        let Some(t) = self.tabs.last_mut() else {
            return;
        };
        if !val.is_empty() {
            let val = expand_tilde(val);
            let path = Path::new(&val);
            t.cwd = Some(if path.is_absolute() {
                normalize_path(path)
            } else {
                normalize_path(&session_base_dir.join(path))
            });
        } else if !session_base_dir.as_os_str().is_empty() {
            t.cwd = Some(session_base_dir.to_path_buf());
        }
    }

    /// Parse a launch command line. ``expand`` is used for ``$VAR`` expansion
    /// outside of ``launch``/``set_layout_state``.
    pub fn add_window(
        &mut self,
        cmd: &str,
        environ: &BTreeMap<String, String>,
    ) -> Result<(), String> {
        let mut args = shlex_split(cmd)?;
        let mut serialize_data: Option<Value> = None;
        if let Some(first) = args.first() {
            if let Some(rest) = first.strip_prefix(UNSERIALIZE_LAUNCH_FLAG) {
                let data = Value::from_json(rest).map_err(|e| e.to_string())?;
                if !matches!(data, Value::Map(_)) {
                    return Err("Invalid serialized launch data".into());
                }
                serialize_data = Some(data);
                args.remove(0);
            }
        }
        let launch = parse_launch_args(&args, &|x| expand_vars(x, environ))?;
        let Some(tab) = self.tabs.last_mut() else {
            return Err("launch before new_tab".into());
        };
        let mut launch = launch;
        if let Some(next_title) = &tab.next_title {
            if launch.title.is_none() {
                launch.title = Some(next_title.clone());
            }
        }
        if launch.cwd.is_none() {
            if let Some(cwd) = &tab.cwd {
                launch.cwd = Some(cwd.to_string_lossy().into_owned());
            }
        }
        let mut spec = WindowSpec::new(launch);
        if let Some(data) = serialize_data {
            spec.serialized_id = data.get_i64("id").unwrap_or(0).max(0) as WindowId;
            if let Some(cmd) = data.get("cmd_at_shell_startup").and_then(Value::as_list) {
                for x in cmd {
                    if let Some(s) = x.as_str() {
                        spec.run_command_at_shell_startup.push(s.to_string());
                    }
                }
            }
        }
        tab.windows.push(spec);
        tab.next_title = None;
        Ok(())
    }

    pub fn resize_window(&mut self, args: &[String]) -> Result<(), String> {
        let quality = args
            .first()
            .map(|x| x.to_lowercase())
            .filter(|x| ["reset", "taller", "shorter", "wider", "narrower"].contains(&x.as_str()))
            .unwrap_or_else(|| "wider".into());
        let increment = args.get(1).and_then(|x| x.parse::<i64>().ok()).unwrap_or(1);
        let Some(tab) = self.tabs.last_mut() else {
            return Err("resize_window before new_tab".into());
        };
        let spec = (quality, increment);
        if let Some(w) = tab.windows.last_mut() {
            w.resize_spec = Some(spec);
        }
        Ok(())
    }

    pub fn focus_matching_window(&mut self, spec: &str) -> Result<(), String> {
        let Some(tab) = self.tabs.last_mut() else {
            return Err("focus_matching_window before new_tab".into());
        };
        if let Some(w) = tab.windows.last_mut() {
            w.focus_matching_window = Some(spec.to_string());
        }
        Ok(())
    }

    pub fn focus(&mut self) {
        self.active_tab_idx = self.tabs.len().saturating_sub(1);
        if let Some(t) = self.tabs.get_mut(self.active_tab_idx) {
            t.active_window_idx = t.windows.len().saturating_sub(1);
        }
    }

    pub fn focus_tab(&mut self, spec: &str) {
        self.focus_tab_spec = Some(spec.to_string());
    }

    fn finalize(&mut self, session_name: &str) {
        self.session_name = session_name.to_string();
        for t in &mut self.tabs {
            if t.windows.is_empty() {
                let launch = LaunchSpec::with_args([self.opts.shell.clone()]);
                t.windows.push(WindowSpec::new(launch));
            }
        }
        self.num_of_windows_in_definition = self.tabs.iter().map(|t| t.windows.len()).sum();
    }

    /// The restore plan for this session.
    pub fn restore_plan(&self) -> RestorePlan {
        RestorePlan {
            os_windows: vec![OsWindowRestore {
                class: self.os_window_class.clone(),
                name: self.os_window_name.clone(),
                title: self.os_window_title.clone(),
                state: self.os_window_state.clone(),
                size: self.os_window_size.clone(),
                position: self.os_window_position,
                tabs: self
                    .tabs
                    .iter()
                    .map(|t| TabRestore {
                        name: t.name.clone(),
                        layout: t.layout.clone(),
                        enabled_layouts: t.enabled_layouts.clone(),
                        cwd: t.cwd.clone(),
                        layout_state: t.layout_state.clone(),
                        active_window_idx: t.active_window_idx,
                        windows: t
                            .windows
                            .iter()
                            .map(|w| WindowRestore {
                                launch: w.launch.clone(),
                                cwd: w
                                    .launch
                                    .cwd
                                    .as_ref()
                                    .map(PathBuf::from)
                                    .or_else(|| t.cwd.clone()),
                                title: w.launch.title.clone(),
                                serialized_id: w.serialized_id,
                                resize_spec: w.resize_spec.clone(),
                                focus_matching_window: w.focus_matching_window.clone(),
                                shell_startup_command: w.run_command_at_shell_startup.clone(),
                            })
                            .collect(),
                    })
                    .collect(),
                active_tab_idx: self.active_tab_idx,
                focus_tab_spec: self.focus_tab_spec.clone(),
            }],
            focus_os_window: self.focus_os_window,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct RestorePlan {
    pub os_windows: Vec<OsWindowRestore>,
    pub focus_os_window: bool,
}

#[derive(Clone, Debug)]
pub struct OsWindowRestore {
    pub class: Option<String>,
    pub name: Option<String>,
    pub title: Option<String>,
    pub state: Option<String>,
    pub size: Option<WindowSizes>,
    pub position: Option<(i32, i32)>,
    pub tabs: Vec<TabRestore>,
    pub active_tab_idx: usize,
    pub focus_tab_spec: Option<String>,
}

#[derive(Clone, Debug)]
pub struct TabRestore {
    pub name: String,
    pub layout: String,
    pub enabled_layouts: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub layout_state: Option<Value>,
    pub active_window_idx: usize,
    pub windows: Vec<WindowRestore>,
}

#[derive(Clone, Debug)]
pub struct WindowRestore {
    pub launch: LaunchSpec,
    pub cwd: Option<PathBuf>,
    pub title: Option<String>,
    pub serialized_id: WindowId,
    pub resize_spec: Option<(String, i64)>,
    pub focus_matching_window: Option<String>,
    pub shell_startup_command: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionError {
    pub line: usize,
    pub message: String,
}

impl SessionError {
    fn new(line: usize, message: impl Into<String>) -> Self {
        Self {
            line,
            message: message.into(),
        }
    }
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.line > 0 {
            write!(f, "line {}: {}", self.line, self.message)
        } else {
            write!(f, "{}", self.message)
        }
    }
}

impl std::error::Error for SessionError {}

pub fn has_session_extension(path: &str) -> bool {
    let name = Path::new(path)
        .file_name()
        .and_then(|x| x.to_str())
        .unwrap_or("");
    let ext = name.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    matches!(ext, "session" | "kitty-session" | "kitty_session")
}

pub fn session_arg_to_name(session_arg: &str) -> String {
    let arg = if matches!(session_arg, "-" | "/dev/stdin" | "none") {
        ""
    } else {
        session_arg
    };
    let mut name = Path::new(arg)
        .file_name()
        .and_then(|x| x.to_str())
        .unwrap_or("")
        .to_string();
    if has_session_extension(&name) {
        name = name
            .rsplit_once('.')
            .map(|(a, _)| a)
            .unwrap_or("")
            .to_string();
    }
    name
}

/// Parse a session file. A session file can contain multiple OS windows; each
/// yields one [`Session`].
pub fn parse_sessions(
    raw: &str,
    opts: &SessionOptions,
    environ: Option<&BTreeMap<String, String>>,
    session_arg: &str,
    session_path: &str,
) -> Result<Vec<Session>, SessionError> {
    let session_name = session_arg_to_name(session_arg);
    let session_base_dir = if session_path.is_empty() {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    } else {
        Path::new(session_path)
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."))
    };
    let env = match environ {
        Some(e) => e.clone(),
        None => std::env::vars().collect(),
    };
    let expand = |s: &str| expand_vars(s, &env);

    let mut ans = Session::new(opts);
    ans.add_tab("");
    let mut sessions = Vec::new();
    for (lineno, raw_line) in raw.split('\n').enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (cmd, rest) = match line.split_once(char::is_whitespace) {
            Some((cmd, rest)) => (cmd.trim(), rest.trim()),
            None => (line, ""),
        };
        let expanded = if cmd == "launch" || cmd == "set_layout_state" {
            rest.to_string()
        } else {
            expand(rest)
        };
        match cmd {
            "new_tab" => ans.add_tab(&expanded),
            "new_os_window" => {
                ans.finalize(&session_name);
                sessions.push(ans);
                ans = Session::new(opts);
                ans.add_tab(&expanded);
            }
            "layout" => ans
                .set_layout(&expanded)
                .map_err(|e| SessionError::new(lineno + 1, e))?,
            "launch" => ans
                .add_window(&expanded, &env)
                .map_err(|e| SessionError::new(lineno + 1, e))?,
            "focus" => ans.focus(),
            "focus_tab" => ans.focus_tab(&expanded),
            "focus_os_window" => ans.focus_os_window = true,
            "enabled_layouts" => ans
                .set_enabled_layouts(&expanded)
                .map_err(|e| SessionError::new(lineno + 1, e))?,
            "cd" => ans.set_cwd(&expanded, &session_base_dir),
            "title" => ans.set_next_title(&expanded),
            "os_window_size" => {
                let parts: Vec<&str> = expanded.split_whitespace().collect();
                if parts.len() != 2 {
                    return Err(SessionError::new(
                        lineno + 1,
                        "os_window_size requires width and height",
                    ));
                }
                let w =
                    parse_window_size(parts[0]).map_err(|e| SessionError::new(lineno + 1, e))?;
                let h =
                    parse_window_size(parts[1]).map_err(|e| SessionError::new(lineno + 1, e))?;
                ans.os_window_size = Some(WindowSizes {
                    width: w,
                    height: h,
                });
            }
            "os_window_position" => {
                let lower = expanded.to_lowercase();
                let Some((x, y)) = lower.split_once('x') else {
                    return Err(SessionError::new(
                        lineno + 1,
                        format!(
                            "Invalid os_window_position: {expanded:?}, must be of the form XxY, for example: 100x50"
                        ),
                    ));
                };
                let x = x
                    .parse::<i32>()
                    .map_err(|_| SessionError::new(lineno + 1, "Invalid os_window_position x"))?;
                let y = y
                    .parse::<i32>()
                    .map_err(|_| SessionError::new(lineno + 1, "Invalid os_window_position y"))?;
                ans.os_window_position = Some((x, y));
            }
            "os_window_class" => ans.os_window_class = Some(expanded),
            "os_window_name" => ans.os_window_name = Some(expanded),
            "os_window_title" => ans.os_window_title = Some(expanded),
            "os_window_state" => ans.os_window_state = Some(expanded),
            "resize_window" => {
                let args: Vec<String> = expanded
                    .split_whitespace()
                    .map(ToString::to_string)
                    .collect();
                ans.resize_window(&args)
                    .map_err(|e| SessionError::new(lineno + 1, e))?;
            }
            "focus_matching_window" => ans
                .focus_matching_window(&expanded)
                .map_err(|e| SessionError::new(lineno + 1, e))?,
            "set_layout_state" => {
                let value = Value::from_json(&expanded)
                    .map_err(|e| SessionError::new(lineno + 1, e.to_string()))?;
                if let Some(t) = ans.tabs.last_mut() {
                    t.layout_state = Some(value);
                }
            }
            other => {
                return Err(SessionError::new(
                    lineno + 1,
                    format!("Unknown command in session file: {other}"),
                ))
            }
        }
    }
    ans.finalize(&session_name);
    sessions.push(ans);
    Ok(sessions)
}

pub fn parse_session(
    raw: &str,
    opts: &SessionOptions,
    environ: Option<&BTreeMap<String, String>>,
    session_arg: &str,
    session_path: &str,
) -> Result<Session, SessionError> {
    let mut sessions = parse_sessions(raw, opts, environ, session_arg, session_path)?;
    if sessions.len() != 1 {
        return Err(SessionError::new(
            0,
            format!(
                "Session file defines {} OS windows, expected one",
                sessions.len()
            ),
        ));
    }
    Ok(sessions.remove(0))
}

fn parse_window_size(raw: &str) -> Result<WindowSize, String> {
    let val = raw.to_lowercase();
    let (num, unit) = if val.ends_with('c') {
        (&val[..val.len() - 1], "cells")
    } else {
        (val.as_str(), "px")
    };
    let size = num
        .parse::<u32>()
        .map_err(|_| format!("Invalid window size: {raw}"))?;
    if size == 0 {
        return Err(format!("Invalid window size: {raw}"));
    }
    Ok(WindowSize {
        size,
        unit: unit.into(),
    })
}

/// Parse launch arguments without invoking the whole kitty CLI machinery.
fn parse_launch_args(
    args: &[String],
    expand: &dyn Fn(&str) -> String,
) -> Result<LaunchSpec, String> {
    let mut ans = LaunchSpec::default();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--" {
            ans.args.extend(args[i + 1..].iter().cloned());
            break;
        }
        if !arg.starts_with("--") {
            ans.args.push(arg.clone());
            i += 1;
            continue;
        }
        let (name, inline) = match arg.split_once('=') {
            Some((n, v)) => (n, Some(v.to_string())),
            None => (arg.as_str(), None),
        };
        let take_value = |i: &mut usize| -> Result<String, String> {
            let raw = if let Some(v) = &inline {
                v.clone()
            } else {
                *i += 1;
                args.get(*i)
                    .cloned()
                    .ok_or_else(|| format!("Missing value for launch option {name}"))?
            };
            Ok(expand(&raw))
        };
        match name {
            "--cwd" | "--directory" => ans.cwd = Some(take_value(&mut i)?),
            "--title" => ans.title = Some(take_value(&mut i)?),
            "--env" => {
                let v = take_value(&mut i)?;
                let (k, val) = v
                    .split_once('=')
                    .ok_or_else(|| format!("Invalid --env value: {v}"))?;
                ans.env.insert(k.to_string(), val.to_string());
            }
            "--var" => {
                let v = take_value(&mut i)?;
                let (k, val) = v
                    .split_once('=')
                    .ok_or_else(|| format!("Invalid --var value: {v}"))?;
                ans.vars.insert(k.to_string(), val.to_string());
            }
            "--hold" => ans.hold = true,
            "--allow-remote-control" => ans.allow_remote_control = true,
            "--type" => {
                let v = take_value(&mut i)?;
                ans.background = v == "background";
                ans.extra_options.push(format!("--type={v}"));
            }
            "--watcher"
            | "--color"
            | "--spacing"
            | "--logo"
            | "--logo-position"
            | "--logo-alpha"
            | "--os-panel"
            | "--hold-after-ssh"
            | "--copy-env"
            | "--stdin-source"
            | "--stdin-add-format"
            | "--stdin-add-line-wrap-markers"
            | "--remote-control-password"
            | "--os-window-class"
            | "--os-window-name"
            | "--os-window-title"
            | "--os-window-state"
            | "--os-window-size"
            | "--os-window-position"
            | "--single-instance"
            | "--dont-take-focus" => {
                if inline.is_some()
                    || matches!(
                        name,
                        "--hold-after-ssh" | "--copy-env" | "--dont-take-focus"
                    )
                {
                    ans.extra_options.push(arg.clone());
                } else {
                    let v = take_value(&mut i)?;
                    ans.extra_options.push(format!("{name}={v}"));
                }
            }
            _ => {
                // Preserve unknown options rather than rejecting them, so newer
                // session files remain loadable. kitty itself would reject them.
                if let Some(v) = inline {
                    ans.extra_options.push(format!("{name}={}", expand(&v)));
                } else {
                    ans.extra_options.push(arg.clone());
                }
            }
        }
        i += 1;
    }
    Ok(ans)
}

/// Expand ``$VAR`` and ``${VAR}``, leaving unknown variables untouched, as
/// kitty's ``expandvars(fallback_to_os_env=False)`` does.
pub fn expand_vars(raw: &str, env: &BTreeMap<String, String>) -> String {
    let mut ans = String::new();
    let chars: Vec<char> = raw.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '$' {
            ans.push(chars[i]);
            i += 1;
            continue;
        }
        if i + 1 < chars.len() && chars[i + 1] == '$' {
            ans.push('$');
            i += 2;
            continue;
        }
        if i + 1 < chars.len() && chars[i + 1] == '{' {
            if let Some(end) = chars[i + 2..].iter().position(|c| *c == '}') {
                let key: String = chars[i + 2..i + 2 + end].iter().collect();
                if let Some(val) = env.get(&key) {
                    ans.push_str(val);
                } else {
                    ans.push_str(&format!("${{{key}}}"));
                }
                i += end + 3;
                continue;
            }
        }
        let mut j = i + 1;
        while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '_') {
            j += 1;
        }
        if j > i + 1 {
            let key: String = chars[i + 1..j].iter().collect();
            if let Some(val) = env.get(&key) {
                ans.push_str(val);
            } else {
                ans.push_str(&format!("${key}"));
            }
            i = j;
        } else {
            ans.push('$');
            i += 1;
        }
    }
    ans
}

fn expand_tilde(raw: &str) -> String {
    if raw == "~" {
        return std::env::var("HOME").unwrap_or_else(|_| raw.into());
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return format!("{home}/{rest}");
        }
    }
    raw.to_string()
}

pub fn normalize_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// A replacement for ``os.path.relpath`` for the paths used by sessions.
pub fn relative_path(path: &Path, base: &Path) -> PathBuf {
    let path = normalize_path(path);
    let base = normalize_path(base);
    if path == base {
        return PathBuf::from(".");
    }
    let pc: Vec<_> = path.components().collect();
    let bc: Vec<_> = base.components().collect();
    let common = pc.iter().zip(bc.iter()).take_while(|(a, b)| a == b).count();
    let mut out = PathBuf::new();
    for _ in common..bc.len() {
        out.push("..");
    }
    for c in &pc[common..] {
        out.push(c.as_os_str());
    }
    if out.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        out
    }
}

/// POSIX-ish tokenisation supporting single quotes, double quotes, backslash
/// escapes and bash ``$'...'`` ANSI-C quoting.
pub fn shlex_split(raw: &str) -> Result<Vec<String>, String> {
    let chars: Vec<char> = raw.chars().collect();
    let mut ans = Vec::new();
    let mut cur = String::new();
    let mut have_word = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            ' ' | '\t' | '\n' | '\r' => {
                if have_word {
                    ans.push(std::mem::take(&mut cur));
                    have_word = false;
                }
                i += 1;
            }
            '\'' => {
                have_word = true;
                i += 1;
                loop {
                    if i >= chars.len() {
                        return Err("Unterminated single-quoted string".into());
                    }
                    if chars[i] == '\'' {
                        i += 1;
                        break;
                    }
                    cur.push(chars[i]);
                    i += 1;
                }
            }
            '"' => {
                have_word = true;
                i += 1;
                loop {
                    if i >= chars.len() {
                        return Err("Unterminated double-quoted string".into());
                    }
                    if chars[i] == '"' {
                        i += 1;
                        break;
                    }
                    if chars[i] == '\\' && i + 1 < chars.len() {
                        let next = chars[i + 1];
                        if matches!(next, '"' | '\\' | '$' | '`') {
                            cur.push(next);
                            i += 2;
                            continue;
                        }
                    }
                    cur.push(chars[i]);
                    i += 1;
                }
            }
            '\\' => {
                have_word = true;
                i += 1;
                if i < chars.len() {
                    cur.push(chars[i]);
                    i += 1;
                }
            }
            '$' if i + 1 < chars.len() && chars[i + 1] == '\'' => {
                have_word = true;
                i += 2;
                loop {
                    if i >= chars.len() {
                        return Err("Unterminated ANSI-C quoted string".into());
                    }
                    if chars[i] == '\'' {
                        i += 1;
                        break;
                    }
                    if chars[i] == '\\' && i + 1 < chars.len() {
                        i += 1;
                        let esc = chars[i];
                        match esc {
                            'n' => cur.push('\n'),
                            't' => cur.push('\t'),
                            'r' => cur.push('\r'),
                            '\\' => cur.push('\\'),
                            '\'' => cur.push('\''),
                            '"' => cur.push('"'),
                            'x' => {
                                let mut val = 0u32;
                                let mut count = 0;
                                while count < 2 && i + 1 < chars.len() {
                                    if let Some(d) = chars[i + 1].to_digit(16) {
                                        val = val * 16 + d;
                                        i += 1;
                                        count += 1;
                                    } else {
                                        break;
                                    }
                                }
                                if let Some(ch) = char::from_u32(val) {
                                    cur.push(ch);
                                }
                            }
                            'u' => {
                                let mut val = 0u32;
                                for _ in 0..4 {
                                    if i + 1 < chars.len() {
                                        if let Some(d) = chars[i + 1].to_digit(16) {
                                            val = val * 16 + d;
                                            i += 1;
                                        }
                                    }
                                }
                                if let Some(ch) = char::from_u32(val) {
                                    cur.push(ch);
                                }
                            }
                            other => cur.push(other),
                        }
                        i += 1;
                    } else {
                        cur.push(chars[i]);
                        i += 1;
                    }
                }
            }
            _ => {
                have_word = true;
                cur.push(c);
                i += 1;
            }
        }
    }
    if have_word {
        ans.push(cur);
    }
    Ok(ans)
}

/// Quote one argument for a session file, using POSIX shell rules.
pub fn shlex_join(args: &[String]) -> String {
    args.iter()
        .map(|a| shell_quote(a))
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn shell_quote(raw: &str) -> String {
    if !raw.is_empty()
        && raw
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_-./:=+,@%^".contains(c))
    {
        return raw.to_string();
    }
    let mut out = String::from("'");
    for c in raw.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

/// Serialise a tab manager to kitty session lines.
pub fn serialize_session(tm: &TabManager, session_path: &str, relocatable: bool) -> Vec<String> {
    let session_base_dir = if session_path.is_empty() {
        PathBuf::new()
    } else {
        Path::new(session_path)
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default()
    };
    let mut ans = Vec::new();
    let meta = &tm.os_window;
    if let Some(class) = &meta.class {
        ans.push(format!("os_window_class {class}"));
    }
    if let Some(name) = &meta.name {
        ans.push(format!("os_window_name {name}"));
    }
    if let Some(title) = &meta.title {
        ans.push(format!("os_window_title {title}"));
    }
    if let Some(size) = &meta.size {
        let text = |value: u32, unit: &str| {
            if unit == "cells" {
                format!("{value}c")
            } else {
                value.to_string()
            }
        };
        ans.push(format!(
            "os_window_size {} {}",
            text(size.width, &size.width_unit),
            text(size.height, &size.height_unit)
        ));
    }
    if let Some((x, y)) = meta.position {
        ans.push(format!("os_window_position {x}x{y}"));
    }
    if let Some(state) = &meta.state {
        ans.push(format!("os_window_state {state}"));
    }
    for tab in tm.tabs() {
        let lines = serialize_tab(tab, &session_base_dir, relocatable);
        if lines.is_empty() {
            continue;
        }
        ans.extend(lines);
    }
    if !ans.is_empty() {
        ans.push(String::new());
        ans.push(format!("focus_tab {}", tm.active_tab_idx()));
    }
    ans
}

fn serialize_tab(tab: &Tab, session_base_dir: &Path, relocatable: bool) -> Vec<String> {
    let windows = tab.windows();
    let mut launch_cmds = Vec::new();

    let make_relative = |cwd: &str| -> String {
        if relocatable && !session_base_dir.as_os_str().is_empty() {
            let p = Path::new(cwd);
            if p.is_absolute() {
                return relative_path(p, session_base_dir)
                    .to_string_lossy()
                    .into_owned();
            }
        }
        cwd.to_string()
    };

    let mut cwds: HashMap<WindowId, String> = HashMap::new();
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for g in windows.groups() {
        for wid in &g.windows {
            if let Some(w) = windows.window(*wid) {
                let cwd = w
                    .cwd
                    .as_ref()
                    .map(|x| x.to_string_lossy().into_owned())
                    .or_else(|| tab.cwd.as_ref().map(|x| x.to_string_lossy().into_owned()))
                    .unwrap_or_default();
                let cwd = make_relative(&cwd);
                *counts.entry(cwd.clone()).or_default() += 1;
                cwds.insert(*wid, cwd);
            }
        }
    }
    let most_common_cwd = counts
        .iter()
        .max_by_key(|(_, n)| **n)
        .map(|(c, _)| c.clone())
        .unwrap_or_default();

    let active_group = windows.active_group().map(|g| g.id);
    for g in windows.groups() {
        let mut group_lines = Vec::new();
        for wid in &g.windows {
            let Some(w) = windows.window(*wid) else {
                continue;
            };
            let cwd = cwds.get(wid).cloned().unwrap_or_default();
            if let Some(line) = window_launch_line(w, cwd, &most_common_cwd) {
                group_lines.push(line);
            }
        }
        if !group_lines.is_empty() {
            launch_cmds.extend(group_lines);
            if Some(g.id) == active_group {
                launch_cmds.push("focus".into());
            }
        }
    }
    if launch_cmds.is_empty() {
        return Vec::new();
    }
    let mut enabled = tab.layouts().enabled_layouts().to_vec();
    let layout = tab.layouts().current_name().to_string();
    if !enabled.contains(&layout) {
        enabled.push(layout.clone());
    }
    let state = tab.layouts().serialize_layout_state_for_session(windows);
    vec![
        String::new(),
        format!("new_tab {}", tab.name).trim_end().to_string(),
        format!("layout {layout}"),
        format!("enabled_layouts {}", enabled.join(",")),
        format!("set_layout_state {}", state.to_json()),
        format!("cd {most_common_cwd}"),
        String::new(),
    ]
    .into_iter()
    .chain(launch_cmds)
    .collect()
}

fn window_launch_line(
    w: &crate::model::Window,
    cwd: String,
    most_common_cwd: &str,
) -> Option<String> {
    let mut args: Vec<String> = vec!["launch".into()];
    let mut data = Value::map();
    data.insert("id".into(), Value::Int(w.serialized_id as i64));
    if !w.launch.shell_startup_command.is_empty() {
        data.insert(
            "cmd_at_shell_startup".into(),
            Value::List(
                w.launch
                    .shell_startup_command
                    .iter()
                    .map(|x| Value::Str(x.clone()))
                    .collect(),
            ),
        );
    }
    args.push(format!(
        "{UNSERIALIZE_LAUNCH_FLAG}{}",
        Value::Map(data).to_json()
    ));
    if !cwd.is_empty() && cwd != most_common_cwd {
        args.push(format!("--cwd={cwd}"));
    }
    for (k, v) in &w.launch.env {
        args.push(format!("--env={k}={v}"));
    }
    let mut vars = w.launch.vars.clone();
    for (k, v) in &w.user_vars {
        vars.insert(k.clone(), v.clone());
    }
    for (k, v) in &vars {
        args.push(format!("--var={k}={v}"));
    }
    if w.launch.allow_remote_control {
        args.push("--allow-remote-control".into());
    }
    if w.launch.hold {
        args.push("--hold".into());
    }
    if w.launch.background {
        args.push("--type=background".into());
    }
    for opt in &w.launch.extra_options {
        args.push(opt.clone());
    }
    if let Some(title) = &w.override_title {
        args.push(format!("--title={title}"));
    }
    for arg in &w.launch.args {
        args.push(arg.clone());
    }
    Some(shlex_join(&args))
}

/// Restore plans for a batch of sessions (one per OS window).
pub fn restore_plans(sessions: &[Session]) -> RestorePlan {
    RestorePlan {
        os_windows: sessions
            .iter()
            .flat_map(|s| s.restore_plan().os_windows)
            .collect(),
        focus_os_window: sessions.iter().any(|s| s.focus_os_window),
    }
}

/// A stable OS-window id for a session restore plan. The actual binding is
/// done by the (not yet implemented) GUI layer.
pub fn session_os_window_id(index: usize) -> OsWindowId {
    index as OsWindowId + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shlex_roundtrip() {
        let args = vec![
            "launch".to_string(),
            "--env=A=b c".to_string(),
            "quote'inside".to_string(),
            String::new(),
        ];
        let joined = shlex_join(&args);
        assert_eq!(shlex_split(&joined).unwrap(), args);
    }

    #[test]
    fn all_layouts_contains_names() {
        for name in crate::layout::ALL_LAYOUT_NAMES {
            assert!(crate::layout::is_layout_name(name));
        }
    }
}
