// SPDX-License-Identifier: GPL-3.0-or-later
//! Quick-access terminal, ported from `kittens/quick_access_terminal`.
//!
//! Loads `quick-access-terminal.conf` and builds the `kitten panel` command
//! line used to toggle/show the singleton QAT window.

use std::fs;
use std::path::{Path, PathBuf};

use crate::panel::{Edge, FocusPolicy, Layer};

pub const QUICK_ACCESS_USAGE: &str = "\
A quick access terminal window that you can bring up instantly.
Usage: kitten quick-access-terminal [options] [cmdline-to-run ...]

Options:
  -c, --config FILE      Configuration file (repeatable)
  -o, --override KEY=VAL Configuration override (repeatable)
      --detach --detached-log PATH
      --instance-group NAME
      --if-running ignore|new-tab|new-window
      --hold --listen-on ADDR
      --debug-rendering --debug-input
  -h, --help";

#[derive(Clone, Debug, PartialEq)]
pub struct QuickAccessConfig {
    pub app_id: String,
    pub background_opacity: f64,
    pub columns: String,
    pub edge: Edge,
    pub focus_policy: FocusPolicy,
    pub grab_keyboard: bool,
    pub hide_on_focus_loss: bool,
    pub kitty_conf: Vec<String>,
    pub kitty_override: Vec<String>,
    pub layer: Layer,
    pub lines: String,
    pub margin_bottom: i64,
    pub margin_left: i64,
    pub margin_right: i64,
    pub margin_top: i64,
    pub output_name: String,
    pub start_as_hidden: bool,
}

impl Default for QuickAccessConfig {
    fn default() -> Self {
        Self {
            app_id: "kitty-quick-access".to_owned(),
            background_opacity: 0.85,
            columns: "80".to_owned(),
            edge: Edge::Top,
            focus_policy: FocusPolicy::Exclusive,
            grab_keyboard: false,
            hide_on_focus_loss: false,
            kitty_conf: Vec::new(),
            kitty_override: Vec::new(),
            layer: Layer::Overlay,
            lines: "25".to_owned(),
            margin_bottom: 0,
            margin_left: 0,
            margin_right: 0,
            margin_top: 0,
            output_name: String::new(),
            start_as_hidden: false,
        }
    }
}

fn bool_value(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "yes" | "y" | "true" | "1" | "on"
    )
}

impl QuickAccessConfig {
    pub fn apply(&mut self, key: &str, value: &str) -> Result<(), String> {
        let value = value.trim();
        match key {
            "app_id" => self.app_id = value.to_owned(),
            "background_opacity" => {
                self.background_opacity = value
                    .parse()
                    .map_err(|_| format!("invalid background_opacity: {value}"))?
            }
            "columns" => self.columns = value.to_owned(),
            "edge" => {
                self.edge = Edge::parse(value).ok_or_else(|| format!("invalid edge: {value}"))?
            }
            "focus_policy" => {
                self.focus_policy = FocusPolicy::parse(value)
                    .ok_or_else(|| format!("invalid focus_policy: {value}"))?
            }
            "grab_keyboard" => self.grab_keyboard = bool_value(value),
            "hide_on_focus_loss" => self.hide_on_focus_loss = bool_value(value),
            "kitty_conf" => self.kitty_conf.push(value.to_owned()),
            "kitty_override" => self.kitty_override.push(value.to_owned()),
            "layer" => {
                self.layer = Layer::parse(value).ok_or_else(|| format!("invalid layer: {value}"))?
            }
            "lines" => self.lines = value.to_owned(),
            "margin_bottom" => {
                self.margin_bottom = value
                    .parse()
                    .map_err(|_| format!("invalid margin_bottom: {value}"))?
            }
            "margin_left" => {
                self.margin_left = value
                    .parse()
                    .map_err(|_| format!("invalid margin_left: {value}"))?
            }
            "margin_right" => {
                self.margin_right = value
                    .parse()
                    .map_err(|_| format!("invalid margin_right: {value}"))?
            }
            "margin_top" => {
                self.margin_top = value
                    .parse()
                    .map_err(|_| format!("invalid margin_top: {value}"))?
            }
            "output_name" => self.output_name = value.to_owned(),
            "start_as_hidden" => self.start_as_hidden = bool_value(value),
            other => return Err(format!("Unknown configuration key: {other:?}")),
        }
        Ok(())
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let mut out = Self::default();
        out.merge(text)?;
        Ok(out)
    }

    /// Apply a config file on top of the current values.
    pub fn merge(&mut self, text: &str) -> Result<(), String> {
        for raw in text.lines() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (key, value) = line
                .split_once(char::is_whitespace)
                .map_or((line, ""), |(a, b)| (a, b.trim()));
            self.apply(key, value)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct QuickAccessOptions {
    pub config: Vec<PathBuf>,
    pub override_opts: Vec<String>,
    pub detach: bool,
    pub detached_log: Option<PathBuf>,
    pub instance_group: String,
    pub if_running: String,
    pub hold: bool,
    pub listen_on: String,
    pub debug_rendering: bool,
    pub debug_input: bool,
    pub items: Vec<String>,
    pub help: bool,
}

impl Default for QuickAccessOptions {
    fn default() -> Self {
        Self {
            config: Vec::new(),
            override_opts: Vec::new(),
            detach: false,
            detached_log: None,
            instance_group: "quick-access".to_owned(),
            if_running: "ignore".to_owned(),
            hold: false,
            listen_on: String::new(),
            debug_rendering: false,
            debug_input: false,
            items: Vec::new(),
            help: false,
        }
    }
}

pub fn parse_quick_access_args(args: &[String]) -> Result<QuickAccessOptions, String> {
    let mut opts = QuickAccessOptions::default();
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
            "-c" | "--config" => opts.config.push(PathBuf::from(take())),
            "-o" | "--override" => opts.override_opts.push(take()),
            "--detach" => opts.detach = true,
            "--detached-log" => opts.detached_log = Some(PathBuf::from(take())),
            "--instance-group" => opts.instance_group = take(),
            "--if-running" => opts.if_running = take(),
            "--hold" => opts.hold = true,
            "--listen-on" => opts.listen_on = take(),
            "--debug-rendering" => opts.debug_rendering = true,
            "--debug-input" => opts.debug_input = true,
            other if !other.starts_with('-') => opts.items.push(other.to_owned()),
            other => return Err(format!("unknown option: {other}")),
        }
        index += 1;
    }
    Ok(opts)
}

/// Load the config, merging `--config` files on top of the defaults and then
/// applying `--override` values.
pub fn load_config(
    opts: &QuickAccessOptions,
    default_paths: &[PathBuf],
) -> Result<QuickAccessConfig, String> {
    let mut config = QuickAccessConfig::default();
    let paths = if opts.config.is_empty() {
        default_paths.to_vec()
    } else {
        opts.config.clone()
    };
    for path in paths {
        if path.as_os_str() == "NONE" {
            continue;
        }
        let text = if path.as_os_str() == "-" || path == Path::new("/dev/stdin") {
            let mut text = String::new();
            use std::io::Read;
            std::io::stdin()
                .read_to_string(&mut text)
                .map_err(|err| err.to_string())?;
            text
        } else {
            match fs::read_to_string(&path) {
                Ok(text) => text,
                Err(_) => continue,
            }
        };
        config.merge(&text)?;
    }
    for override_opt in &opts.override_opts {
        let (key, value) = override_opt
            .split_once('=')
            .ok_or_else(|| format!("invalid override: {override_opt}"))?;
        config.apply(key, value)?;
    }
    Ok(config)
}

pub fn config_paths() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(dir) = std::env::var_os("KITTY_CONFIG_DIRECTORY") {
        out.push(PathBuf::from(dir).join("quick-access-terminal.conf"));
        return out;
    }
    if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME") {
        out.push(PathBuf::from(dir).join("kitty/quick-access-terminal.conf"));
    } else if let Some(home) = std::env::var_os("HOME") {
        out.push(PathBuf::from(home).join(".config/kitty/quick-access-terminal.conf"));
    }
    let dirs = std::env::var("XDG_CONFIG_DIRS").unwrap_or_else(|_| "/etc/xdg".to_owned());
    for dir in dirs.split(':').filter(|dir| !dir.is_empty()) {
        out.push(PathBuf::from(dir).join("kitty/quick-access-terminal.conf"));
    }
    out
}

/// Build the `kitty +kitten panel ...` argv for the quick-access terminal.
pub fn build_quick_access_command(
    kitty_exe: &Path,
    config: &QuickAccessConfig,
    opts: &QuickAccessOptions,
    config_dir: &Path,
) -> Vec<String> {
    let mut argv = vec![
        kitty_exe.to_string_lossy().into_owned(),
        "+kitten".to_owned(),
        "panel".to_owned(),
        "--toggle-visibility".to_owned(),
        "--exclusive-zone=0".to_owned(),
        "--override-exclusive-zone".to_owned(),
        "--single-instance".to_owned(),
        "--move-to-active-monitor".to_owned(),
        format!("--lines={}", config.lines),
        format!("--columns={}", config.columns),
        format!("--edge={}", config.edge.as_str()),
        format!("--layer={}", config.layer.as_str()),
    ];
    for (flag, value) in [
        ("--margin-top", config.margin_top),
        ("--margin-bottom", config.margin_bottom),
        ("--margin-left", config.margin_left),
        ("--margin-right", config.margin_right),
    ] {
        if value != 0 {
            argv.push(format!("{flag}={value}"));
        }
    }
    for config_file in &config.kitty_conf {
        let path = if Path::new(config_file).is_absolute() {
            PathBuf::from(config_file)
        } else {
            config_dir.join(config_file)
        };
        argv.push(format!("--config={}", path.display()));
    }
    for override_opt in &config.kitty_override {
        argv.push(format!("--override={override_opt}"));
    }
    argv.push(format!(
        "--override=background_opacity={:.6}",
        config.background_opacity
    ));
    if cfg!(not(target_os = "macos")) {
        argv.push(format!("--app-id={}", config.app_id));
    }
    if !config.output_name.is_empty() {
        argv.push(format!("--output-name={}", config.output_name));
    }
    argv.push(format!("--focus-policy={}", config.focus_policy.as_str()));
    if config.start_as_hidden {
        argv.push("--start-as-hidden".to_owned());
    }
    if config.grab_keyboard {
        argv.push("--grab-keyboard".to_owned());
    }
    if config.hide_on_focus_loss {
        argv.push("--hide-on-focus-loss".to_owned());
    }
    if opts.debug_rendering {
        argv.push("--debug-rendering".to_owned());
    }
    if opts.debug_input {
        argv.push("--debug-input".to_owned());
    }
    if opts.detach {
        argv.push("--detach".to_owned());
    }
    if let Some(log) = &opts.detached_log {
        let log = if log.is_absolute() {
            log.clone()
        } else {
            std::env::current_dir()
                .map(|cwd| cwd.join(log))
                .unwrap_or_else(|_| log.clone())
        };
        argv.push(format!("--detached-log={}", log.display()));
    }
    if !opts.instance_group.is_empty() {
        argv.push(format!("--instance-group={}", opts.instance_group));
    }
    if !opts.if_running.is_empty() && opts.if_running != "ignore" {
        argv.push(format!("--if-running={}", opts.if_running));
    }
    if opts.hold {
        argv.push("--hold".to_owned());
    }
    if !opts.listen_on.is_empty() {
        argv.push(format!("--listen-on={}", opts.listen_on));
    }
    argv.extend(opts.items.iter().cloned());
    argv
}

/// A quick-access launch ready for a GUI host.
#[derive(Clone, Debug, PartialEq)]
pub struct QuickAccessLaunch {
    pub argv: Vec<String>,
    pub layer_shell: crate::panel::LayerShellConfig,
    pub instance_group: String,
}

/// Build a quick-access launch, including the panel layer-shell configuration.
pub fn build_quick_access_launch(
    kitty_exe: &Path,
    config: &QuickAccessConfig,
    opts: &QuickAccessOptions,
    config_dir: &Path,
) -> QuickAccessLaunch {
    let panel_opts = crate::panel::PanelOptions {
        edge: config.edge,
        layer: config.layer,
        lines: config.lines.clone(),
        columns: config.columns.clone(),
        margin_top: config.margin_top as i32,
        margin_left: config.margin_left as i32,
        margin_bottom: config.margin_bottom as i32,
        margin_right: config.margin_right as i32,
        focus_policy: config.focus_policy,
        exclusive_zone: 0,
        override_exclusive_zone: true,
        hide_on_focus_loss: config.hide_on_focus_loss,
        output_name: config.output_name.clone(),
        toggle_visibility: true,
        move_to_active_monitor: true,
        if_running: opts.if_running.clone(),
        hold: opts.hold,
        start_as_hidden: config.start_as_hidden,
        grab_keyboard: config.grab_keyboard,
        class: config.app_id.clone(),
        name: String::new(),
        single_instance: true,
        instance_group: opts.instance_group.clone(),
        listen_on: opts.listen_on.clone(),
        detach: opts.detach,
        detached_log: opts.detached_log.clone(),
        items: opts.items.clone(),
        ..crate::panel::PanelOptions::default()
    };
    QuickAccessLaunch {
        argv: build_quick_access_command(kitty_exe, config, opts, config_dir),
        layer_shell: crate::panel::layer_shell_config(&panel_opts),
        instance_group: opts.instance_group.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_and_parsing() {
        let config = QuickAccessConfig::parse(
            "lines 30\ncolumns 100px\nedge bottom\nlayer top\nbackground_opacity 0.5\nmargin_top 4\nstart_as_hidden yes\n",
        )
        .unwrap();
        assert_eq!(config.lines, "30");
        assert_eq!(config.columns, "100px");
        assert_eq!(config.edge, Edge::Bottom);
        assert_eq!(config.layer, Layer::Top);
        assert_eq!(config.background_opacity, 0.5);
        assert_eq!(config.margin_top, 4);
        assert!(config.start_as_hidden);
        assert!(QuickAccessConfig::parse("bogus 1").is_err());
    }

    #[test]
    fn overrides_are_applied() {
        let opts = QuickAccessOptions {
            override_opts: vec!["lines=12".to_owned(), "edge=left".to_owned()],
            ..QuickAccessOptions::default()
        };
        let config = load_config(&opts, &[]).unwrap();
        assert_eq!(config.lines, "12");
        assert_eq!(config.edge, Edge::Left);
    }

    #[test]
    fn later_config_files_merge_onto_defaults() {
        let mut config = QuickAccessConfig::default();
        config.merge("lines 10\nedge bottom\n").unwrap();
        config.merge("lines 20\n").unwrap();
        assert_eq!(config.lines, "20");
        assert_eq!(config.edge, Edge::Bottom);
        assert_eq!(config.columns, "80");
    }

    #[test]
    fn builds_panel_command() {
        let config = QuickAccessConfig::default();
        let opts = QuickAccessOptions {
            hold: true,
            items: vec!["htop".to_owned()],
            ..QuickAccessOptions::default()
        };
        let argv = build_quick_access_command(
            Path::new("/usr/bin/kitty"),
            &config,
            &opts,
            Path::new("/home/u/.config/kitty"),
        );
        assert_eq!(argv[0], "/usr/bin/kitty");
        assert!(argv.contains(&"--toggle-visibility".to_owned()));
        assert!(argv.contains(&"--single-instance".to_owned()));
        assert!(argv.contains(&"--lines=25".to_owned()));
        assert!(argv.contains(&"--hold".to_owned()));
        assert_eq!(argv.last().map(String::as_str), Some("htop"));
        let launch = build_quick_access_launch(
            Path::new("/usr/bin/kitty"),
            &config,
            &opts,
            Path::new("/home/u/.config/kitty"),
        );
        assert_eq!(launch.instance_group, "quick-access");
        assert_eq!(launch.layer_shell.edge, Edge::Top);
        assert_eq!(launch.layer_shell.exclusive_zone, 0);
    }

    #[test]
    fn cli_parsing() {
        let args = [
            "--detach",
            "--instance-group=myqat",
            "--if-running=new-tab",
            "sh",
        ]
        .iter()
        .map(|x| (*x).to_owned())
        .collect::<Vec<_>>();
        let opts = parse_quick_access_args(&args).unwrap();
        assert!(opts.detach);
        assert_eq!(opts.instance_group, "myqat");
        assert_eq!(opts.if_running, "new-tab");
        assert_eq!(opts.items, vec!["sh"]);
    }
}
