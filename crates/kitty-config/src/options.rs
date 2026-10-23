// SPDX-License-Identifier: GPL-3.0-or-later
//! Typed option storage and value parsers ported from `kitty/config.py`,
//! `kitty/options/parse.py`, `kitty/options/types.py`,
//! `kitty/options/utils.py` and `kitty/conf/utils.py`
//! (Copyright (C) 2016-2025 Kovid Goyal).

use std::collections::BTreeMap;

use crate::color::Color;
use crate::expand::expand_vars;
use crate::shlex::Shlex;

/// Sentinel used by kitty's `env` option to mean "remove this variable".
pub const DELETE_ENV_VAR: &str = "_delete_this_env_var_";
pub const MINIMUM_FONT_SIZE: f64 = 4.0;
pub const MAXIMUM_FONT_SIZE: f64 = 256.0;
pub const FONT_SIZE_MULTIPLE: f64 = 10.0;

pub fn clamp_font_size(new_size: f64, configured_size: f64) -> f64 {
    let upper = (configured_size * FONT_SIZE_MULTIPLE).max(MAXIMUM_FONT_SIZE);
    new_size.clamp(MINIMUM_FONT_SIZE, upper)
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Edges<T> {
    pub left: T,
    pub top: T,
    pub right: T,
    pub bottom: T,
}

impl<T: Copy> Edges<T> {
    pub const fn all(value: T) -> Self {
        Self {
            left: value,
            top: value,
            right: value,
            bottom: value,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct FontSpec {
    pub family: Option<String>,
    pub style: Option<String>,
    pub postscript_name: Option<String>,
    pub full_name: Option<String>,
    pub system: Option<String>,
    pub axes: Vec<(String, f64)>,
    pub variable_name: Option<String>,
    pub features: Vec<String>,
    pub created_from_string: String,
}

impl FontSpec {
    pub fn auto() -> Self {
        Self {
            family: None,
            style: None,
            postscript_name: None,
            full_name: None,
            system: Some("auto".into()),
            axes: Vec::new(),
            variable_name: None,
            features: Vec::new(),
            created_from_string: "auto".into(),
        }
    }

    pub fn system(name: &str) -> Self {
        Self {
            family: None,
            style: None,
            postscript_name: None,
            full_name: None,
            system: Some(name.into()),
            axes: Vec::new(),
            variable_name: None,
            features: Vec::new(),
            created_from_string: name.into(),
        }
    }

    /// Port of `FontSpec.from_setting()`.
    pub fn from_setting(spec: &str) -> Result<Self, String> {
        if spec == "auto" {
            return Ok(Self::auto());
        }
        let items = Shlex::split(spec, false).map_err(|e| e.to_string())?;
        let Some(first) = items.first() else {
            return Ok(Self::system(spec));
        };
        if !first.contains('=') {
            return Ok(Self::system(spec));
        }
        let mut ans = Self {
            family: None,
            style: None,
            postscript_name: None,
            full_name: None,
            system: None,
            axes: Vec::new(),
            variable_name: None,
            features: Vec::new(),
            created_from_string: spec.into(),
        };
        for item in items {
            let (key, value) = item.split_once('=').ok_or_else(|| {
                format!(
                    "The font specification: {spec} is not valid as {item} does not contain an ="
                )
            })?;
            match key {
                "family" => ans.family = Some(value.into()),
                "style" => ans.style = Some(value.into()),
                "full_name" => ans.full_name = Some(value.into()),
                "postscript_name" => ans.postscript_name = Some(value.into()),
                "variable_name" => ans.variable_name = Some(value.into()),
                "features" => ans
                    .features
                    .extend(value.split_whitespace().map(str::to_string)),
                _ => {
                    let number = value.parse::<f64>().map_err(|_| {
                        format!("The font specification: {spec} is not valid as {value} is not a number")
                    })?;
                    ans.axes.push((key.into(), number));
                }
            }
        }
        Ok(ans)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum JumpType {
    #[default]
    End,
    Start,
    None,
    Both,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub enum EasingFunction {
    #[default]
    None,
    Steps {
        num_steps: u32,
        jump_type: JumpType,
    },
    Linear {
        linear_x: Vec<f64>,
        linear_y: Vec<f64>,
    },
    CubicBezier {
        cubic_bezier_points: [f64; 4],
    },
}

impl EasingFunction {
    pub fn cubic_bezier(params: &str) -> Result<Self, String> {
        let normalized = params.replace(',', " ");
        let parts: Vec<&str> = normalized.split_whitespace().collect();
        if parts.len() != 4 {
            return Err("cubic-bezier easing function must have four points".into());
        }
        let result = [
            parse_unit_float(parts[0])?,
            parse_f64(parts[1])?,
            parse_unit_float(parts[2])?,
            parse_f64(parts[3])?,
        ];
        Ok(Self::CubicBezier {
            cubic_bezier_points: result,
        })
    }

    pub fn linear(params: &str) -> Result<Self, String> {
        let parts: Vec<&str> = params.split(',').collect();
        if parts.len() < 2 {
            return Err("Must specify at least two points for the linear easing function".into());
        }
        let mut xaxis: Vec<f64> = Vec::new();
        let mut yaxis: Vec<f64> = Vec::new();

        fn balance(
            end: f64,
            xaxis: &mut Vec<f64>,
            yaxis: &[f64],
            params: &str,
        ) -> Result<(), String> {
            let extra = yaxis.len() as isize - xaxis.len() as isize;
            if extra <= 0 {
                return Ok(());
            }
            let extra = extra as usize;
            let start = xaxis.last().copied().unwrap_or(0.0);
            let delta = (end - start) / (extra.saturating_sub(1).max(1) as f64);
            if delta <= 0.0 {
                return Err(format!(
                    "Linear easing curve must have strictly increasing points: {params} does not"
                ));
            }
            if !xaxis.is_empty() {
                for i in 0..extra {
                    xaxis.push(start + (i + 1) as f64 * delta);
                }
            } else {
                for i in 0..extra {
                    xaxis.push(i as f64 * delta);
                }
            }
            Ok(())
        }

        for raw in parts {
            let points: Vec<&str> = raw.split_whitespace().collect();
            let Some(first) = points.first() else {
                return Err("Linear easing point is empty".into());
            };
            let y = parse_unit_float(first)?;
            match points.len() {
                1 => yaxis.push(y),
                2 => {
                    let x = parse_percent(points[1])?;
                    balance(x, &mut xaxis, &yaxis, params)?;
                    xaxis.push(x);
                    yaxis.push(y);
                }
                3 => {
                    let x1 = parse_percent(points[1])?;
                    balance(x1, &mut xaxis, &yaxis, params)?;
                    xaxis.push(x1);
                    yaxis.push(y);
                    let x2 = parse_percent(points[2])?;
                    balance(x2, &mut xaxis, &yaxis, params)?;
                    xaxis.push(x2);
                    yaxis.push(y);
                }
                _ => {
                    return Err(format!(
                        "{raw} has too many points for a linear easing curve parameter"
                    ))
                }
            }
        }
        balance(1.0, &mut xaxis, &yaxis, params)?;
        Ok(Self::Linear {
            linear_x: xaxis,
            linear_y: yaxis,
        })
    }

    pub fn steps(params: &str) -> Result<Self, String> {
        let normalized = params.replace(',', " ");
        let parts: Vec<&str> = normalized.split_whitespace().collect();
        let mut jump_type = JumpType::End;
        let mut n;
        if parts.len() == 2 {
            n = parts[0]
                .parse::<i64>()
                .map_err(|_| format!("{} is not a valid number of steps", parts[0]))?;
            jump_type = match parts[1].to_lowercase().as_str() {
                "jump-start" | "start" => JumpType::Start,
                "end" | "jump-end" => JumpType::End,
                "jump-none" => JumpType::None,
                "jump-both" => JumpType::Both,
                other => {
                    return Err(format!(
                        "{other} is not a valid jump type for a linear easing function"
                    ))
                }
            };
            n = if jump_type == JumpType::None {
                n.max(2)
            } else {
                n.max(1)
            };
        } else {
            let Some(first) = parts.first() else {
                return Err("steps() needs a number of steps".into());
            };
            n = first
                .parse::<i64>()
                .map_err(|_| format!("{first} is not a valid number of steps"))?
                .max(1);
        }
        Ok(Self::Steps {
            num_steps: n.max(0) as u32,
            jump_type,
        })
    }
}

pub fn parse_animation(
    spec: &str,
    default_interval: f64,
) -> Result<(f64, EasingFunction, EasingFunction), String> {
    if let Ok(interval) = spec.trim().parse::<f64>() {
        return Ok((interval, EasingFunction::None, EasingFunction::None));
    }
    let mut interval = default_interval;
    let mut funcs = [EasingFunction::None, EasingFunction::None];

    let mut parse_func = |name: &str, params: &str| -> Result<(), String> {
        let idx = if !matches!(funcs[0], EasingFunction::None) {
            1
        } else {
            0
        };
        if !matches!(funcs[idx], EasingFunction::None) {
            return Err(format!("{spec} specified more than two easing functions"));
        }
        funcs[idx] = match name {
            "cubic-bezier" => EasingFunction::cubic_bezier(params)?,
            "linear" => EasingFunction::linear(params)?,
            "steps" => EasingFunction::steps(params)?,
            other => return Err(format!("{other} is not a valid easing function")),
        };
        Ok(())
    };

    for (name, params) in animation_tokens(spec) {
        if let Some(params) = params {
            parse_func(name, params)?;
            continue;
        }
        if let Ok(value) = name.parse::<f64>() {
            interval = value;
            continue;
        }
        match name {
            "ease-in-out" => parse_func("cubic-bezier", "0.42, 0, 0.58, 1")?,
            "linear" => parse_func("cubic-bezier", "0, 0, 1, 1")?,
            "ease" => parse_func("cubic-bezier", "0.25, 0.1, 0.25, 1")?,
            "ease-out" => parse_func("cubic-bezier", "0, 0, 0.58, 1")?,
            "ease-in" => parse_func("cubic-bezier", "0.42, 0, 1, 1")?,
            "step-start" => parse_func("steps", "1, start")?,
            "step-end" => parse_func("steps", "1, end")?,
            other => return Err(format!("{other} is not a valid easing function")),
        }
    }
    Ok((interval, funcs[0].clone(), funcs[1].clone()))
}

fn animation_tokens(spec: &str) -> Vec<(&str, Option<&str>)> {
    let bytes = spec.as_bytes();
    let mut ans = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let start = i;
        while i < bytes.len() && is_animation_token_byte(bytes[i]) {
            i += 1;
        }
        if start == i {
            i += 1;
            continue;
        }
        let name = &spec[start..i];
        let mut params = None;
        if i < bytes.len() && bytes[i] == b'(' {
            if let Some(close) = spec[i + 1..].find(')') {
                let pstart = i + 1;
                let pend = pstart + close;
                params = Some(&spec[pstart..pend]);
                i = pend + 1;
            }
        }
        ans.push((name, params));
    }
    ans
}

fn is_animation_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'+' | b'.')
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub enum CursorShape {
    NoCursor = 0,
    Block = 1,
    Beam = 2,
    Underline = 3,
    Hollow = 4,
}

impl CursorShape {
    pub fn from_setting(value: &str) -> Result<Self, String> {
        match value.to_lowercase().as_str() {
            "block" => Ok(Self::Block),
            "beam" => Ok(Self::Beam),
            "underline" => Ok(Self::Underline),
            other => Err(format!(
                "Invalid cursor shape: {other} allowed values are block, beam, underline"
            )),
        }
    }

    pub fn from_unfocused_setting(value: &str) -> Result<Self, String> {
        match value.to_lowercase().as_str() {
            "block" => Ok(Self::Block),
            "beam" => Ok(Self::Beam),
            "underline" => Ok(Self::Underline),
            "hollow" => Ok(Self::Hollow),
            "unchanged" => Ok(Self::NoCursor),
            other => Err(format!(
                "Invalid unfocused cursor shape: {other} allowed values are block, beam, underline, hollow, unchanged"
            )),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct CursorBlinkInterval {
    pub interval: f64,
    pub first: EasingFunction,
    pub second: EasingFunction,
}

impl Default for CursorBlinkInterval {
    fn default() -> Self {
        Self {
            interval: -1.0,
            first: EasingFunction::None,
            second: EasingFunction::None,
        }
    }
}

impl CursorBlinkInterval {
    pub fn parse(spec: &str) -> Result<Self, String> {
        let (interval, first, second) = parse_animation(spec, -1.0)?;
        Ok(Self {
            interval,
            first,
            second,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabBarStyle {
    Fade,
    Hidden,
    Powerline,
    Separator,
    Slant,
    Custom,
}

impl TabBarStyle {
    pub fn from_setting(value: &str) -> Result<Self, String> {
        match value.to_lowercase().as_str() {
            "fade" => Ok(Self::Fade),
            "hidden" => Ok(Self::Hidden),
            "powerline" => Ok(Self::Powerline),
            "separator" => Ok(Self::Separator),
            "slant" => Ok(Self::Slant),
            "custom" => Ok(Self::Custom),
            other => Err(format!(
                "The value {other} is not a valid choice for tab_bar_style"
            )),
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fade => "fade",
            Self::Hidden => "hidden",
            Self::Powerline => "powerline",
            Self::Separator => "separator",
            Self::Slant => "slant",
            Self::Custom => "custom",
        }
    }

    /// Whether the style ever draws a tab bar. Visibility also depends on
    /// [`Options::tab_bar_should_be_visible`].
    pub const fn is_hidden(self) -> bool {
        matches!(self, Self::Hidden)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TabPowerlineStyle {
    #[default]
    Angled,
    Round,
    Slanted,
}

impl TabPowerlineStyle {
    pub fn from_setting(value: &str) -> Result<Self, String> {
        match value.to_lowercase().as_str() {
            "angled" => Ok(Self::Angled),
            "round" => Ok(Self::Round),
            "slanted" => Ok(Self::Slanted),
            other => Err(format!(
                "The value {other} is not a valid choice for tab_powerline_style"
            )),
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Angled => "angled",
            Self::Round => "round",
            Self::Slanted => "slanted",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TabBarEdge {
    Top,
    #[default]
    Bottom,
    Left,
    Right,
}

impl TabBarEdge {
    pub fn from_setting(value: &str) -> Result<Self, String> {
        match value.to_lowercase().as_str() {
            "top" => Ok(Self::Top),
            "bottom" => Ok(Self::Bottom),
            "left" => Ok(Self::Left),
            "right" => Ok(Self::Right),
            other => Err(format!(
                "The value {other} is not a valid choice for tab_bar_edge"
            )),
        }
    }

    pub const fn is_horizontal(self) -> bool {
        matches!(self, Self::Top | Self::Bottom)
    }

    pub const fn is_top(self) -> bool {
        matches!(self, Self::Top)
    }

    pub const fn is_bottom(self) -> bool {
        matches!(self, Self::Bottom)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TabBarAlign {
    #[default]
    Start,
    Center,
    End,
}

impl TabBarAlign {
    pub fn from_setting(value: &str) -> Result<Self, String> {
        match value.to_lowercase().as_str() {
            "start" => Ok(Self::Start),
            "center" => Ok(Self::Center),
            "end" => Ok(Self::End),
            other => Err(format!(
                "The value {other} is not a valid choice for tab_bar_align"
            )),
        }
    }
}

/// Bold/italic flags used by `active_tab_font_style` and
/// `inactive_tab_font_style`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TabFontStyle {
    pub bold: bool,
    pub italic: bool,
}

impl TabFontStyle {
    pub fn from_setting(value: &str) -> Result<Self, String> {
        let lowered = value.to_lowercase();
        let (bold, italic) = match lowered.as_str() {
            "normal" | "none" => (false, false),
            "bold" => (true, false),
            "italic" => (false, true),
            "bold-italic" | "italic-bold" => (true, true),
            other => {
                return Err(format!(
                    "The value {other} is not a valid choice for tab font style"
                ))
            }
        };
        Ok(Self { bold, italic })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers(pub u32);

impl Modifiers {
    pub const NONE: Self = Self(0);
    pub const SHIFT: u32 = 0x0001;
    pub const ALT: u32 = 0x0002;
    pub const CONTROL: u32 = 0x0004;
    pub const SUPER: u32 = 0x0008;
    pub const HYPER: u32 = 0x0010;
    pub const META: u32 = 0x0020;
    pub const CAPS_LOCK: u32 = 0x0040;
    pub const NUM_LOCK: u32 = 0x0080;
    pub const KITTY: u32 = 0x0100;

    pub fn contains(self, bit: u32) -> bool {
        self.0 & bit != 0
    }

    /// Port of `to_modifiers()` / `parse_mods()`.
    pub fn parse(value: &str) -> Result<Self, String> {
        let mut bits = 0;
        for part in value.split('+') {
            let mapped = map_modifier(part);
            let bit = match mapped.as_str() {
                "SHIFT" => Self::SHIFT,
                "ALT" => Self::ALT,
                "CONTROL" => Self::CONTROL,
                "SUPER" => Self::SUPER,
                "HYPER" => Self::HYPER,
                "META" => Self::META,
                "CAPS_LOCK" => Self::CAPS_LOCK,
                "NUM_LOCK" => Self::NUM_LOCK,
                "KITTY" => Self::KITTY,
                "NONE" => continue,
                other => {
                    return Err(format!(
                        "Shortcut: {value} has unknown modifier {other}, ignoring"
                    ))
                }
            };
            bits |= bit;
        }
        Ok(Self(bits))
    }
}

fn map_modifier(value: &str) -> String {
    let upper = value.to_uppercase();
    let mapped = match upper.as_str() {
        "⌃" => "CONTROL",
        "CTRL" => "CONTROL",
        "⇧" => "SHIFT",
        "⌥" => "ALT",
        "OPTION" => "ALT",
        "OPT" => "ALT",
        "⌘" => "SUPER",
        "COMMAND" => "SUPER",
        "CMD" => "SUPER",
        "KITTY_MOD" => "KITTY",
        other => other,
    };
    mapped.to_string()
}

pub const LAYOUT_NAMES: &[&str] = &[
    "fat",
    "grid",
    "horizontal",
    "splits",
    "stack",
    "tall",
    "vertical",
];

pub fn to_layout_names(raw: &str) -> Result<Vec<String>, String> {
    let mut ans: Vec<String> = Vec::new();
    for raw_part in raw.split(',') {
        let part = raw_part.trim().to_lowercase();
        if part == "*" || part == "all" {
            for name in LAYOUT_NAMES {
                if !ans.iter().any(|x| x == name) {
                    ans.push((*name).to_string());
                }
            }
            continue;
        }
        let name = part.split(':').next().unwrap_or("");
        if !LAYOUT_NAMES.contains(&name) {
            return Err(format!("The window layout {part} is unknown"));
        }
        if !ans.iter().any(|x| x == &part) {
            ans.push(part);
        }
    }
    Ok(ans)
}

#[derive(Clone, Debug, PartialEq)]
pub struct Options {
    pub font_family: FontSpec,
    pub font_size: f64,
    pub bold_font: FontSpec,
    pub italic_font: FontSpec,
    pub bold_italic_font: FontSpec,

    pub foreground: Color,
    pub background: Color,
    pub cursor: Option<Color>,
    /// `None` is kitty's special `cursor_text_color background` value.
    pub cursor_text_color: Option<Color>,
    pub selection_foreground: Option<Color>,
    pub selection_background: Option<Color>,

    pub cursor_shape: CursorShape,
    pub cursor_shape_unfocused: CursorShape,
    pub cursor_blink_interval: CursorBlinkInterval,
    pub scrollback_lines: u64,

    pub tab_bar_style: TabBarStyle,
    pub tab_bar_edge: TabBarEdge,
    pub tab_bar_min_tabs: u32,
    pub tab_bar_align: TabBarAlign,
    pub tab_bar_background: Option<Color>,
    pub active_tab_foreground: Color,
    pub active_tab_background: Color,
    pub inactive_tab_foreground: Color,
    pub inactive_tab_background: Color,
    pub active_tab_font_style: TabFontStyle,
    pub inactive_tab_font_style: TabFontStyle,
    pub tab_separator: String,
    pub tab_fade: Vec<f64>,
    pub tab_powerline_style: TabPowerlineStyle,
    pub window_padding_width: Edges<f64>,
    pub window_margin_width: Edges<f64>,
    pub window_border_width: f64,
    pub window_border_radius: f64,
    pub draw_minimal_borders: bool,
    pub draw_window_borders_for_single_window: bool,
    pub active_border_color: Option<Color>,
    pub inactive_border_color: Color,
    pub bell_border_color: Color,
    pub background_opacity: f64,

    pub shell: String,
    pub editor: String,
    pub kitty_mod: Modifiers,
    pub enabled_layouts: Vec<String>,

    pub env: BTreeMap<String, String>,
    /// `color0`..`color255` palette entries.
    pub colors: BTreeMap<u16, Color>,

    /// Raw `map` values. Action parsing/resolution is intentionally not part
    /// of this working subset.
    pub map: Vec<String>,
    /// Raw `mouse_map` values.
    pub mouse_map: Vec<String>,
    /// Raw `action_alias` values.
    pub action_alias: Vec<String>,
    /// Raw `kitten_alias` values.
    pub kitten_alias: Vec<String>,
    pub clear_all_shortcuts: bool,
    pub clear_all_mouse_actions: bool,

    pub background_blur: i64,
    pub inactive_text_alpha: f64,
    pub strip_trailing_spaces: String,
    /// Raw `text_composition_strategy` setting.
    pub text_composition_strategy: String,
    /// Gamma adjustment derived from `text_composition_strategy`.
    pub text_gamma_adjustment: f64,
    /// Contrast percentage derived from `text_composition_strategy`.
    pub text_contrast: f64,
    /// True for the pre-0.28 gamma-incorrect blending mode.
    pub text_old_gamma: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            font_family: FontSpec::system("monospace"),
            font_size: 11.0,
            bold_font: FontSpec::auto(),
            italic_font: FontSpec::auto(),
            bold_italic_font: FontSpec::auto(),
            foreground: Color::new(221, 221, 221),
            background: Color::new(0, 0, 0),
            cursor: Some(Color::new(204, 204, 204)),
            cursor_text_color: Some(Color::new(17, 17, 17)),
            selection_foreground: Some(Color::new(0, 0, 0)),
            selection_background: Some(Color::new(255, 250, 205)),
            cursor_shape: CursorShape::Block,
            cursor_shape_unfocused: CursorShape::Hollow,
            cursor_blink_interval: CursorBlinkInterval::default(),
            scrollback_lines: 2000,
            tab_bar_style: TabBarStyle::Fade,
            tab_bar_edge: TabBarEdge::Bottom,
            tab_bar_min_tabs: 2,
            tab_bar_align: TabBarAlign::Start,
            tab_bar_background: None,
            active_tab_foreground: Color::new(0x00, 0x00, 0x00),
            active_tab_background: Color::new(0xee, 0xee, 0xee),
            inactive_tab_foreground: Color::new(0x44, 0x44, 0x44),
            inactive_tab_background: Color::new(0x99, 0x99, 0x99),
            active_tab_font_style: TabFontStyle {
                bold: true,
                italic: true,
            },
            inactive_tab_font_style: TabFontStyle::default(),
            tab_separator: " ┇".into(),
            tab_fade: vec![0.25, 0.5, 0.75, 1.0],
            tab_powerline_style: TabPowerlineStyle::Angled,
            window_padding_width: Edges::all(0.0),
            window_margin_width: Edges::all(0.0),
            window_border_width: 0.5,
            window_border_radius: 0.0,
            draw_minimal_borders: true,
            draw_window_borders_for_single_window: false,
            active_border_color: Some(Color::new(0x00, 0xff, 0x00)),
            inactive_border_color: Color::new(0xcc, 0xcc, 0xcc),
            bell_border_color: Color::new(0xff, 0x5a, 0x00),
            background_opacity: 1.0,
            shell: ".".into(),
            editor: ".".into(),
            kitty_mod: Modifiers(Modifiers::CONTROL | Modifiers::SHIFT),
            enabled_layouts: LAYOUT_NAMES.iter().map(|x| (*x).to_string()).collect(),
            env: BTreeMap::new(),
            colors: BTreeMap::new(),
            map: Vec::new(),
            mouse_map: Vec::new(),
            action_alias: Vec::new(),
            kitten_alias: Vec::new(),
            clear_all_shortcuts: false,
            clear_all_mouse_actions: false,
            background_blur: 0,
            inactive_text_alpha: 1.0,
            strip_trailing_spaces: "never".into(),
            text_composition_strategy: "platform".into(),
            text_gamma_adjustment: 1.0,
            text_contrast: 0.0,
            text_old_gamma: false,
        }
    }
}

impl Options {
    pub fn new() -> Self {
        Self::default()
    }

    /// kitty's tab-bar visibility rules: `tab_bar_style hidden` always hides
    /// it, and `tab_bar_min_tabs` (default 2) hides it while there are fewer
    /// tabs. A `tab_bar_min_tabs` of zero keeps it visible for any tab count.
    pub fn tab_bar_should_be_visible(&self, tab_count: usize) -> bool {
        if self.tab_bar_style.is_hidden() || tab_count == 0 {
            return false;
        }
        if self.tab_bar_min_tabs == 0 {
            return true;
        }
        tab_count as u64 >= self.tab_bar_min_tabs as u64
    }

    /// Height of the tab-bar strip in device pixels for the given cell height.
    /// Hidden bars reserve zero pixels.
    pub fn tab_bar_height(&self, tab_count: usize, cell_height: f32) -> f32 {
        if self.tab_bar_should_be_visible(tab_count) {
            cell_height.max(0.0)
        } else {
            0.0
        }
    }

    /// Apply one `key value` pair. Returns `Ok(false)` for an unknown key.
    pub fn apply(&mut self, key: &str, value: &str) -> Result<bool, String> {
        if let Some(number) = key.strip_prefix("color") {
            if !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()) {
                let index: u16 = number
                    .parse()
                    .map_err(|_| format!("Invalid palette color index: {key}"))?;
                if index > 255 {
                    return Ok(false);
                }
                self.colors.insert(index, Color::parse_required(value)?);
                return Ok(true);
            }
        }
        match key {
            "font_family" => self.font_family = FontSpec::from_setting(value)?,
            "font_size" => self.font_size = parse_f64(value)?.max(MINIMUM_FONT_SIZE),
            "bold_font" => self.bold_font = FontSpec::from_setting(value)?,
            "italic_font" => self.italic_font = FontSpec::from_setting(value)?,
            "bold_italic_font" => self.bold_italic_font = FontSpec::from_setting(value)?,

            "foreground" => self.foreground = Color::parse_required(value)?,
            "background" => self.background = Color::parse_required(value)?,
            "cursor" => self.cursor = parse_color_or_none(value)?,
            "cursor_text_color" | "cursor_text" => {
                self.cursor_text_color = if value.eq_ignore_ascii_case("background") {
                    None
                } else {
                    Some(Color::parse_required(value)?)
                }
            }
            "selection_foreground" => self.selection_foreground = parse_color_or_none(value)?,
            "selection_background" => self.selection_background = parse_color_or_none(value)?,

            "cursor_shape" => self.cursor_shape = CursorShape::from_setting(value)?,
            "cursor_shape_unfocused" => {
                self.cursor_shape_unfocused = CursorShape::from_unfocused_setting(value)?
            }
            "cursor_blink_interval" => {
                self.cursor_blink_interval = CursorBlinkInterval::parse(value)?
            }
            "scrollback_lines" => {
                let parsed = value
                    .trim()
                    .parse::<i64>()
                    .map_err(|_| format!("Invalid scrollback_lines value: {value}"))?;
                self.scrollback_lines = if parsed < 0 {
                    u32::MAX as u64
                } else {
                    parsed as u64
                };
            }

            "tab_bar_style" => self.tab_bar_style = TabBarStyle::from_setting(value)?,
            "tab_bar_edge" => self.tab_bar_edge = TabBarEdge::from_setting(value)?,
            "tab_bar_min_tabs" => {
                let parsed = value
                    .trim()
                    .parse::<u32>()
                    .map_err(|_| format!("Invalid tab_bar_min_tabs value: {value}"))?;
                self.tab_bar_min_tabs = parsed;
            }
            "tab_bar_align" => self.tab_bar_align = TabBarAlign::from_setting(value)?,
            "tab_bar_background" => self.tab_bar_background = parse_color_or_none(value)?,
            "active_tab_foreground" => self.active_tab_foreground = Color::parse_required(value)?,
            "active_tab_background" => self.active_tab_background = Color::parse_required(value)?,
            "inactive_tab_foreground" => {
                self.inactive_tab_foreground = Color::parse_required(value)?
            }
            "inactive_tab_background" => {
                self.inactive_tab_background = Color::parse_required(value)?
            }
            "active_tab_font_style" => {
                self.active_tab_font_style = TabFontStyle::from_setting(value)?
            }
            "inactive_tab_font_style" => {
                self.inactive_tab_font_style = TabFontStyle::from_setting(value)?
            }
            "tab_separator" => {
                let parts = Shlex::split(value, false).map_err(|error| error.to_string())?;
                self.tab_separator = parts.join(" ");
            }
            "tab_fade" => {
                self.tab_fade = value
                    .split(|c: char| c.is_whitespace() || c == ',')
                    .filter(|part| !part.is_empty())
                    .map(parse_unit_float)
                    .collect::<Result<Vec<_>, _>>()?;
                if self.tab_fade.is_empty() {
                    return Err(format!("Invalid tab_fade value: {value}"));
                }
            }
            "tab_powerline_style" => {
                self.tab_powerline_style = TabPowerlineStyle::from_setting(value)?
            }
            "window_padding_width" => self.window_padding_width = parse_edge_width(value, true)?,
            "window_margin_width" => self.window_margin_width = parse_edge_width(value, true)?,
            "window_border_width" => {
                self.window_border_width = parse_dimension(value)?;
            }
            "window_border_radius" => {
                self.window_border_radius = parse_dimension(value)?;
            }
            "draw_minimal_borders" => self.draw_minimal_borders = parse_bool(value),
            "draw_window_borders_for_single_window" => {
                self.draw_window_borders_for_single_window = parse_bool(value)
            }
            "active_border_color" => self.active_border_color = parse_color_or_none(value)?,
            "inactive_border_color" => self.inactive_border_color = Color::parse_required(value)?,
            "bell_border_color" => self.bell_border_color = Color::parse_required(value)?,
            "background_opacity" => self.background_opacity = parse_unit_float(value)?,

            "shell" => self.shell = value.to_string(),
            "editor" => self.editor = value.to_string(),
            "kitty_mod" => self.kitty_mod = Modifiers::parse(value)?,
            "enabled_layouts" => self.enabled_layouts = to_layout_names(value)?,

            "env" => self.apply_env(value)?,

            "map" => self.map.push(value.to_string()),
            "mouse_map" => self.mouse_map.push(value.to_string()),
            "action_alias" => self.action_alias.push(value.to_string()),
            "kitten_alias" => self.kitten_alias.push(value.to_string()),
            "clear_all_shortcuts" => {
                self.clear_all_shortcuts = parse_bool(value);
                if self.clear_all_shortcuts {
                    self.map.clear();
                }
            }
            "clear_all_mouse_actions" => {
                self.clear_all_mouse_actions = parse_bool(value);
                if self.clear_all_mouse_actions {
                    self.mouse_map.clear();
                }
            }

            "background_blur" => {
                self.background_blur = value
                    .trim()
                    .parse::<i64>()
                    .map_err(|_| format!("Invalid integer: {value}"))?
            }
            "inactive_text_alpha" => self.inactive_text_alpha = parse_signed_unit_float(value)?,
            "text_composition_strategy" => {
                self.apply_text_composition_strategy(value)?;
            }
            "strip_trailing_spaces" => {
                let lowered = value.to_lowercase();
                if !matches!(lowered.as_str(), "always" | "never" | "smart") {
                    return Err(format!(
                        "The value {lowered} is not a valid choice for strip_trailing_spaces"
                    ));
                }
                self.strip_trailing_spaces = lowered;
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    fn apply_text_composition_strategy(&mut self, value: &str) -> Result<(), String> {
        self.text_composition_strategy = value.trim().to_string();
        self.text_old_gamma = false;
        self.text_gamma_adjustment = 1.0;
        self.text_contrast = 0.0;
        let value = value.trim();
        if value == "platform" {
            #[cfg(target_os = "macos")]
            {
                self.text_gamma_adjustment = 1.7;
                self.text_contrast = 30.0;
            }
        } else if value == "legacy" {
            self.text_old_gamma = true;
        } else {
            let parts: Vec<&str> = value.split_whitespace().collect();
            if parts.is_empty() || parts.len() > 2 {
                return Err(
                    "text_composition_strategy must be 'platform', 'legacy' or up to two numbers"
                        .into(),
                );
            }
            let gamma = parts[0]
                .parse::<f64>()
                .map_err(|_| format!("Invalid text gamma adjustment: {}", parts[0]))?;
            self.text_gamma_adjustment = gamma.max(0.01);
            if let Some(contrast) = parts.get(1) {
                let contrast = contrast
                    .parse::<f64>()
                    .map_err(|_| format!("Invalid text contrast: {contrast}"))?;
                self.text_contrast = contrast.clamp(0.0, 100.0);
            }
        }
        Ok(())
    }

    fn apply_env(&mut self, value: &str) -> Result<(), String> {
        let value = value.trim();
        if value.is_empty() {
            return Ok(());
        }
        if let Some((key, val)) = value.split_once('=') {
            let key = key.trim();
            let val = val.trim();
            if !key.is_empty() {
                let val = if val.is_empty() {
                    String::new()
                } else {
                    expand_vars(val, &self.env, true)
                };
                self.env.insert(key.to_string(), val);
            }
        } else {
            self.env
                .insert(value.to_string(), DELETE_ENV_VAR.to_string());
        }
        Ok(())
    }
}

pub fn parse_f64(value: &str) -> Result<f64, String> {
    value
        .trim()
        .parse::<f64>()
        .map_err(|_| format!("Invalid number: {value}"))
}

pub fn parse_unit_float(value: &str) -> Result<f64, String> {
    Ok(parse_f64(value)?.clamp(0.0, 1.0))
}

pub fn parse_signed_unit_float(value: &str) -> Result<f64, String> {
    Ok(parse_f64(value)?.clamp(-1.0, 1.0))
}

pub fn parse_percent(value: &str) -> Result<f64, String> {
    let value = value.strip_suffix('%').unwrap_or(value);
    Ok(parse_f64(value)? / 100.0)
}

pub fn parse_bool(value: &str) -> bool {
    matches!(value.to_lowercase().as_str(), "y" | "yes" | "true")
}

pub fn parse_color_or_none(value: &str) -> Result<Option<Color>, String> {
    if value.eq_ignore_ascii_case("none") {
        Ok(None)
    } else {
        Ok(Some(Color::parse_required(value)?))
    }
}

/// Parse a `window_border_width`/`window_border_radius` value. Values are
/// stored in points; a `px` suffix is accepted and treated as one point per
/// pixel (the GUI applies the display scale when converting to device pixels).
pub fn parse_dimension(value: &str) -> Result<f64, String> {
    let value = value.trim();
    let number = value
        .strip_suffix("pt")
        .or_else(|| value.strip_suffix("px"))
        .unwrap_or(value);
    let parsed = parse_f64(number)?;
    if parsed < 0.0 {
        return Err(format!("Invalid dimension: {value}"));
    }
    Ok(parsed)
}

pub fn parse_edge_width(value: &str, positive: bool) -> Result<Edges<f64>, String> {
    let parts: Vec<&str> = value.split_whitespace().collect();
    let convert = |part: &str| -> Result<f64, String> {
        let val = parse_f64(part)?;
        Ok(if positive { val.max(0.0) } else { val })
    };
    match parts.len() {
        1 => {
            let v = convert(parts[0])?;
            Ok(Edges::all(v))
        }
        2 => {
            let v = convert(parts[0])?;
            let h = convert(parts[1])?;
            Ok(Edges {
                left: h,
                top: v,
                right: h,
                bottom: v,
            })
        }
        3 => {
            let top = convert(parts[0])?;
            let h = convert(parts[1])?;
            let bottom = convert(parts[2])?;
            Ok(Edges {
                left: h,
                top,
                right: h,
                bottom,
            })
        }
        4 => {
            let top = convert(parts[0])?;
            let right = convert(parts[1])?;
            let bottom = convert(parts[2])?;
            let left = convert(parts[3])?;
            Ok(Edges {
                left,
                top,
                right,
                bottom,
            })
        }
        _ => Err(format!("Invalid edge width specification: {value}")),
    }
}
