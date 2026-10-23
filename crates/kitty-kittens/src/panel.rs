// SPDX-License-Identifier: GPL-3.0-or-later
//! Panel kitten, ported from `kittens/panel`.
//!
//! This module builds the layer-shell configuration and kitty command line.
//! The actual panel surface is created by the GUI; this crate remains
//! window-system independent.

use std::path::{Path, PathBuf};

pub const PANEL_USAGE: &str = "\
Usage: kitten panel [options] [cmdline-to-run ...]

Use a command line program to draw a GPU accelerated panel on your desktop.

Options:
      --edge top|bottom|left|right|center|none|center-sized|background
      --layer background|bottom|top|overlay
      --lines N[c|px] --columns N[c|px]
      --margin-top/left/bottom/right N
      --focus-policy not-allowed|exclusive|on-demand
      --exclusive-zone N --override-exclusive-zone
      --hide-on-focus-loss
      --output-name NAME
      --toggle-visibility --move-to-active-monitor
      --if-running ignore|new-tab|new-window
      --hold --start-as-hidden --grab-keyboard
  -h, --help";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Edge {
    #[default]
    Top,
    Bottom,
    Left,
    Right,
    Center,
    None,
    CenterSized,
    Background,
}

impl Edge {
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "top" => Self::Top,
            "bottom" => Self::Bottom,
            "left" => Self::Left,
            "right" => Self::Right,
            "center" => Self::Center,
            "none" => Self::None,
            "center-sized" => Self::CenterSized,
            "background" => Self::Background,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Top => "top",
            Self::Bottom => "bottom",
            Self::Left => "left",
            Self::Right => "right",
            Self::Center => "center",
            Self::None => "none",
            Self::CenterSized => "center-sized",
            Self::Background => "background",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Layer {
    Background,
    Bottom,
    #[default]
    Top,
    Overlay,
}

impl Layer {
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "background" => Self::Background,
            "bottom" => Self::Bottom,
            "top" => Self::Top,
            "overlay" => Self::Overlay,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Background => "background",
            Self::Bottom => "bottom",
            Self::Top => "top",
            Self::Overlay => "overlay",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FocusPolicy {
    #[default]
    NotAllowed,
    Exclusive,
    OnDemand,
}

impl FocusPolicy {
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "not-allowed" => Self::NotAllowed,
            "exclusive" => Self::Exclusive,
            "on-demand" => Self::OnDemand,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotAllowed => "not-allowed",
            Self::Exclusive => "exclusive",
            Self::OnDemand => "on-demand",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LayerShellConfig {
    pub edge: Edge,
    pub layer: Layer,
    pub x_size_in_cells: i32,
    pub x_size_in_pixels: i32,
    pub y_size_in_cells: i32,
    pub y_size_in_pixels: i32,
    pub margin_top: i32,
    pub margin_left: i32,
    pub margin_bottom: i32,
    pub margin_right: i32,
    pub focus_policy: FocusPolicy,
    pub exclusive_zone: i32,
    pub override_exclusive_zone: bool,
    pub hide_on_focus_loss: bool,
    pub output_name: String,
}

/// Parse `N`, `Nc` (cells) or `Npx` (pixels), returning `(cells, pixels)`.
pub fn dual_distance(spec: &str, min_cells_if_no_pixels: i32) -> (i32, i32) {
    let spec = spec.trim();
    if let Ok(value) = spec.parse::<i32>() {
        return (value, 0);
    }
    if let Some(value) = spec.strip_suffix("px") {
        if let Ok(value) = value.parse::<i32>() {
            return (min_cells_if_no_pixels, value);
        }
    }
    if let Some(value) = spec.strip_suffix('c') {
        if let Ok(value) = value.parse::<i32>() {
            return (value, 0);
        }
    }
    (min_cells_if_no_pixels, 0)
}

#[derive(Clone, Debug)]
pub struct PanelOptions {
    pub config: Vec<PathBuf>,
    pub override_opts: Vec<String>,
    pub edge: Edge,
    pub layer: Layer,
    pub lines: String,
    pub columns: String,
    pub margin_top: i32,
    pub margin_left: i32,
    pub margin_bottom: i32,
    pub margin_right: i32,
    pub focus_policy: FocusPolicy,
    pub exclusive_zone: i32,
    pub override_exclusive_zone: bool,
    pub hide_on_focus_loss: bool,
    pub output_name: String,
    pub toggle_visibility: bool,
    pub move_to_active_monitor: bool,
    pub if_running: String,
    pub hold: bool,
    pub start_as_hidden: bool,
    pub grab_keyboard: bool,
    pub class: String,
    pub name: String,
    pub single_instance: bool,
    pub instance_group: String,
    pub listen_on: String,
    pub items: Vec<String>,
    pub help: bool,
}

impl Default for PanelOptions {
    fn default() -> Self {
        Self {
            config: Vec::new(),
            override_opts: Vec::new(),
            edge: Edge::Top,
            layer: Layer::Top,
            lines: String::new(),
            columns: String::new(),
            margin_top: 0,
            margin_left: 0,
            margin_bottom: 0,
            margin_right: 0,
            focus_policy: FocusPolicy::NotAllowed,
            exclusive_zone: 0,
            override_exclusive_zone: false,
            hide_on_focus_loss: false,
            output_name: String::new(),
            toggle_visibility: false,
            move_to_active_monitor: false,
            if_running: "ignore".to_owned(),
            hold: false,
            start_as_hidden: false,
            grab_keyboard: false,
            class: String::new(),
            name: String::new(),
            single_instance: false,
            instance_group: String::new(),
            listen_on: String::new(),
            items: Vec::new(),
            help: false,
        }
    }
}

pub fn parse_panel_args(args: &[String]) -> Result<PanelOptions, String> {
    let mut opts = PanelOptions::default();
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
        let mut number = || -> Result<i32, String> {
            take()
                .parse()
                .map_err(|_| format!("invalid number for {name}"))
        };
        match name {
            "-h" | "--help" => opts.help = true,
            "--edge" => {
                let value = take();
                opts.edge = Edge::parse(&value).ok_or_else(|| format!("invalid edge: {value}"))?
            }
            "--layer" => {
                let value = take();
                opts.layer =
                    Layer::parse(&value).ok_or_else(|| format!("invalid layer: {value}"))?
            }
            "--lines" => opts.lines = take(),
            "--columns" => opts.columns = take(),
            "--margin-top" => opts.margin_top = number()?.max(0),
            "--margin-left" => opts.margin_left = number()?.max(0),
            "--margin-bottom" => opts.margin_bottom = number()?.max(0),
            "--margin-right" => opts.margin_right = number()?.max(0),
            "--focus-policy" => {
                let value = take();
                opts.focus_policy = FocusPolicy::parse(&value)
                    .ok_or_else(|| format!("invalid focus policy: {value}"))?
            }
            "--exclusive-zone" => opts.exclusive_zone = number()?,
            "--override-exclusive-zone" => opts.override_exclusive_zone = true,
            "--hide-on-focus-loss" => opts.hide_on_focus_loss = true,
            "--output-name" => opts.output_name = take(),
            "--toggle-visibility" => opts.toggle_visibility = true,
            "--move-to-active-monitor" => opts.move_to_active_monitor = true,
            "--if-running" => opts.if_running = take(),
            "--hold" => opts.hold = true,
            "--start-as-hidden" => opts.start_as_hidden = true,
            "--grab-keyboard" => opts.grab_keyboard = true,
            "--class" => opts.class = take(),
            "--name" => opts.name = take(),
            "--single-instance" => opts.single_instance = true,
            "--instance-group" => opts.instance_group = take(),
            "--listen-on" => opts.listen_on = take(),
            "--config" | "-c" => opts.config.push(PathBuf::from(take())),
            "--override" | "-o" => opts.override_opts.push(take()),
            "--debug-rendering" | "--debug-input" => {}
            other if !other.starts_with('-') => opts.items.push(other.to_owned()),
            other => return Err(format!("unknown option: {other}")),
        }
        index += 1;
    }
    if opts.hide_on_focus_loss {
        opts.focus_policy = FocusPolicy::OnDemand;
    }
    Ok(opts)
}

pub fn layer_shell_config(opts: &PanelOptions) -> LayerShellConfig {
    let (x_cells, x_pixels) = dual_distance(&opts.columns, 1);
    let (y_cells, y_pixels) = dual_distance(&opts.lines, 1);
    let edge = opts.edge;
    let layer = if edge == Edge::Background {
        Layer::Background
    } else {
        opts.layer
    };
    LayerShellConfig {
        edge,
        layer,
        x_size_in_cells: x_cells,
        x_size_in_pixels: x_pixels,
        y_size_in_cells: y_cells,
        y_size_in_pixels: y_pixels,
        margin_top: opts.margin_top.max(0),
        margin_left: opts.margin_left.max(0),
        margin_bottom: opts.margin_bottom.max(0),
        margin_right: opts.margin_right.max(0),
        focus_policy: if opts.hide_on_focus_loss {
            FocusPolicy::OnDemand
        } else {
            opts.focus_policy
        },
        exclusive_zone: opts.exclusive_zone,
        override_exclusive_zone: opts.override_exclusive_zone,
        hide_on_focus_loss: opts.hide_on_focus_loss,
        output_name: opts.output_name.clone(),
    }
}

/// Incrementally update an existing layer-shell config from command-line
/// option strings (used by the single-instance control path).
pub fn update_layer_shell_config(
    existing: &mut LayerShellConfig,
    cli_options: &[String],
) -> Result<(), String> {
    let args: Vec<String> = cli_options
        .iter()
        .map(|item| {
            if item.starts_with('-') {
                item.clone()
            } else {
                format!("--{item}")
            }
        })
        .collect();
    let opts = parse_panel_args(&args)?;
    let fresh = layer_shell_config(&opts);
    // Apply the fields explicitly present in the CLI. This compact version
    // treats non-default values as explicit, matching the kitten's result for
    // the common single-instance cases.
    if opts.edge != PanelOptions::default().edge
        || args.iter().any(|item| item.starts_with("--edge"))
    {
        existing.edge = fresh.edge;
        existing.layer = fresh.layer;
    }
    if args.iter().any(|item| item.starts_with("--lines")) {
        existing.y_size_in_cells = fresh.y_size_in_cells;
        existing.y_size_in_pixels = fresh.y_size_in_pixels;
    }
    if args.iter().any(|item| item.starts_with("--columns")) {
        existing.x_size_in_cells = fresh.x_size_in_cells;
        existing.x_size_in_pixels = fresh.x_size_in_pixels;
    }
    for (flag, field) in [
        ("--margin-top", 0usize),
        ("--margin-left", 1),
        ("--margin-bottom", 2),
        ("--margin-right", 3),
    ] {
        if args.iter().any(|item| item.starts_with(flag)) {
            match field {
                0 => existing.margin_top = fresh.margin_top,
                1 => existing.margin_left = fresh.margin_left,
                2 => existing.margin_bottom = fresh.margin_bottom,
                _ => existing.margin_right = fresh.margin_right,
            }
        }
    }
    if args.iter().any(|item| item.starts_with("--focus-policy")) || opts.hide_on_focus_loss {
        existing.focus_policy = fresh.focus_policy;
    }
    if opts.hide_on_focus_loss {
        existing.hide_on_focus_loss = true;
        existing.focus_policy = FocusPolicy::OnDemand;
    }
    Ok(())
}

/// Build the kitty argv used to host the panel.
pub fn build_panel_command(kitty_exe: &Path, opts: &PanelOptions) -> Vec<String> {
    let mut argv = vec![
        kitty_exe.to_string_lossy().into_owned(),
        "+kitten".to_owned(),
        "panel".to_owned(),
    ];
    argv.push(format!("--edge={}", opts.edge.as_str()));
    argv.push(format!("--layer={}", opts.layer.as_str()));
    if !opts.lines.is_empty() {
        argv.push(format!("--lines={}", opts.lines));
    }
    if !opts.columns.is_empty() {
        argv.push(format!("--columns={}", opts.columns));
    }
    for (flag, value) in [
        ("--margin-top", opts.margin_top),
        ("--margin-left", opts.margin_left),
        ("--margin-bottom", opts.margin_bottom),
        ("--margin-right", opts.margin_right),
    ] {
        if value != 0 {
            argv.push(format!("{flag}={value}"));
        }
    }
    argv.push(format!("--focus-policy={}", opts.focus_policy.as_str()));
    if opts.exclusive_zone != 0 {
        argv.push(format!("--exclusive-zone={}", opts.exclusive_zone));
    }
    if opts.override_exclusive_zone {
        argv.push("--override-exclusive-zone".to_owned());
    }
    if opts.hide_on_focus_loss {
        argv.push("--hide-on-focus-loss".to_owned());
    }
    if !opts.output_name.is_empty() {
        argv.push(format!("--output-name={}", opts.output_name));
    }
    if opts.toggle_visibility {
        argv.push("--toggle-visibility".to_owned());
    }
    if opts.move_to_active_monitor {
        argv.push("--move-to-active-monitor".to_owned());
    }
    if opts.hold {
        argv.push("--hold".to_owned());
    }
    if opts.start_as_hidden {
        argv.push("--start-as-hidden".to_owned());
    }
    if opts.grab_keyboard {
        argv.push("--grab-keyboard".to_owned());
    }
    for config in &opts.config {
        argv.push(format!("--config={}", config.display()));
    }
    for override_opt in &opts.override_opts {
        argv.push(format!("--override={override_opt}"));
    }
    argv.extend(opts.items.iter().cloned());
    argv
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distances_and_enums() {
        assert_eq!(dual_distance("25", 1), (25, 0));
        assert_eq!(dual_distance("25c", 1), (25, 0));
        assert_eq!(dual_distance("25px", 1), (1, 25));
        assert_eq!(dual_distance("bad", 1), (1, 0));
        assert_eq!(Edge::parse("center-sized"), Some(Edge::CenterSized));
        assert_eq!(Layer::parse("overlay"), Some(Layer::Overlay));
    }

    #[test]
    fn parses_options_and_layer_config() {
        let args = [
            "--edge=bottom",
            "--layer=overlay",
            "--lines=25px",
            "--columns=80c",
            "--margin-top=4",
            "--hide-on-focus-loss",
            "--exclusive-zone=0",
            "--override-exclusive-zone",
            "sh",
        ]
        .iter()
        .map(|x| (*x).to_owned())
        .collect::<Vec<_>>();
        let opts = parse_panel_args(&args).unwrap();
        assert_eq!(opts.edge, Edge::Bottom);
        assert_eq!(opts.items, vec!["sh"]);
        let cfg = layer_shell_config(&opts);
        assert_eq!(cfg.y_size_in_pixels, 25);
        assert_eq!(cfg.x_size_in_cells, 80);
        assert_eq!(cfg.focus_policy, FocusPolicy::OnDemand);
        assert!(cfg.hide_on_focus_loss);
        assert!(cfg.override_exclusive_zone);
    }

    #[test]
    fn updates_config_incrementally() {
        let mut cfg = LayerShellConfig::default();
        update_layer_shell_config(&mut cfg, &["edge=bottom".to_owned(), "lines=10".to_owned()])
            .unwrap();
        assert_eq!(cfg.edge, Edge::Bottom);
        assert_eq!(cfg.y_size_in_cells, 10);
    }

    #[test]
    fn builds_command_line() {
        let opts = PanelOptions {
            edge: Edge::Bottom,
            lines: "20".to_owned(),
            items: vec!["top".to_owned()],
            ..PanelOptions::default()
        };
        let argv = build_panel_command(Path::new("/usr/bin/kitty"), &opts);
        assert_eq!(argv[0], "/usr/bin/kitty");
        assert!(argv.contains(&"--edge=bottom".to_owned()));
        assert!(argv.contains(&"--lines=20".to_owned()));
        assert_eq!(argv.last().map(String::as_str), Some("top"));
    }
}
