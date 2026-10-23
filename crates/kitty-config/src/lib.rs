// SPDX-License-Identifier: GPL-3.0-or-later
//! Configuration and option handling ported from `kitty/shlex.c`,
//! `kitty/state.c`, `kitty/config.py`, `kitty/conf/utils.py`,
//! `kitty/options/*`, `kitty/cli.py`, `kitty/colors.c` and
//! `gen/color_names.py` (Copyright (C) 2016-2025 Kovid Goyal).
//!
//! # API
//!
//! * [`shlex`]: an exact port of kitty's shell-like lexer, including `$'...'`
//!   ANSI-C quoting and kitty's error messages.
//! * [`Color`]: named, `#rgb`, `#rrggbb`, 9/12-digit hex, `rgb:` and `rgbi:`
//!   parsing. `oklch()`/`lab()` are not part of this working subset.
//! * [`Options`]: typed storage for the supported options.
//! * [`Config::load`]: strict loading from paths plus CLI-style overrides;
//!   [`Config::load_lenient`] mirrors kitty by accumulating bad lines.
//!
//! # Supported option keys
//!
//! `font_family`, `font_size`, `bold_font`, `italic_font`, `bold_italic_font`,
//! `foreground`, `background`, `cursor`, `cursor_text_color` (alias
//! `cursor_text`), `selection_foreground`, `selection_background`,
//! `cursor_shape`, `cursor_shape_unfocused`, `cursor_blink_interval`,
//! `scrollback_lines`, `tab_bar_style`, `tab_bar_min_tabs`, `tab_bar_align`,
//! `tab_bar_background`, `active_tab_*`, `inactive_tab_*`, `tab_separator`,
//! `tab_fade`, `tab_powerline_style`, `window_padding_width`,
//! `window_margin_width`, `window_border_width`, `window_border_radius`,
//! `draw_minimal_borders`, `draw_window_borders_for_single_window`,
//! `active_border_color`, `inactive_border_color`, `bell_border_color`,
//! `background_opacity`, `shell`, `editor`, `kitty_mod`, `enabled_layouts`,
//! `env`, `map`, `mouse_map`, `action_alias`, `kitten_alias`,
//! `clear_all_shortcuts`, `clear_all_mouse_actions`, `background_blur`,
//! `inactive_text_alpha`, `strip_trailing_spaces`, and the `include`,
//! `globinclude` and `envinclude` directives.
//!
//! `map`/`action_alias`/`kitten_alias` are stored raw; keymap resolution and
//! action argument parsing are deliberately deferred. `geninclude` is rejected
//! rather than executing Python at runtime.
//!
//! # Explicitly unsupported option groups
//!
//! * macOS/window-system options (`macos_*`, `wayland_*`, `x11_*`,
//!   `linux_display_server`, window decoration/titlebar options).
//! * Window/tab visual layout options beyond padding, margins, borders,
//!   `tab_bar_style` and its colours/fonts/separators, `cursor_*`, and colors
//!   (title-bar decorations, tab-bar edges/margins, background images,
//!   scrollbars, shaders).
//! * Advanced behavior options: mouse mappings are raw only, keyboard modes,
//!   action aliases are raw only, font features/`modify_font`, keyboard
//!   remapping, clipboard/paste policy, remote control, kittens, sessions, and
//!   notifications. `color0`..`color255` are parsed; dynamic palette
//!   generation (`palette_generate`) is not.
//! * Advanced color syntax (`oklch()`, `lab()`, `color()`).
//!
//! Unknown keys are reported as [`BadLine`] entries, matching kitty's
//! `Ignoring unknown config key: ...` diagnostic.

pub mod color;
mod color_names;
pub mod config;
pub mod expand;
pub mod glob;
pub mod options;
pub mod shlex;

pub use color::Color;
pub use config::{BadLine, Config, ConfigError};
pub use options::{
    clamp_font_size, parse_dimension, CursorBlinkInterval, CursorShape, EasingFunction, Edges,
    FontSpec, JumpType, Modifiers, Options, TabBarAlign, TabBarEdge, TabBarStyle, TabFontStyle,
    TabPowerlineStyle, DELETE_ENV_VAR, MAXIMUM_FONT_SIZE, MINIMUM_FONT_SIZE,
};
pub use shlex::{Shlex, ShlexError};

#[cfg(test)]
mod tests;
