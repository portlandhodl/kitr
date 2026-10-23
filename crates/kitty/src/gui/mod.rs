// SPDX-License-Identifier: GPL-3.0-or-later
//! X11 + EGL GUI frontend with tabs, kitty-ui layouts and remote control.
//!
//! One native window owns a [`kitty_ui::TabManager`]. Each tab contains one or
//! more PTY panes; layouts allocate pane rectangles in cell coordinates and
//! the renderer draws every pane with its own `RenderConfig` origin/viewport.
//! The tab bar is a one-row [`Screen`] rendered through the same font atlas.

mod colors;
mod fonts;
mod input;
mod pane;
mod remote;

use std::collections::{BTreeMap, HashMap};
use std::ffi::{OsStr, OsString};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::{Duration, Instant};

use colors::ConfigColorResolver;
use fonts::{FontRasterizer, FontSet};
use input::{key_event, mouse_button, mouse_modifiers, send_mouse};
use pane::{Pane, SpawnSpec};

use kitty_config::{
    Config, CursorShape as ConfigCursorShape, TabBarStyle, TabFontStyle, TabPowerlineStyle,
};
use kitty_fonts::RasterCache;
use kitty_input::mouse::MouseAction;
use kitty_input::{InputContext, KeyAction, KeyEvent, KeyMap, MappingResult, Modifiers};
use kitty_os::child::ExitStatus;
use kitty_remote::{
    color_as_u32, CommandError, CommandResult, GetTextRequest, ScreenshotFormat, ScreenshotImage,
    SendTextData, ServerConfig, TextExtent, Value as RemoteValue,
};
use kitty_render::{
    pane_border_rects, AtlasConfig, CellMetrics, ColorResolver, DecorationInstance, DecorationKind,
    Frame, GammaMode, GlBackend, GlBackendConfig, GlFns, InstanceFlags, LineRange, PaneBorderPane,
    RenderConfig, RendererCore, Rgba8, SelectionRange, SolidRect,
};
use kitty_screen::{CursorShape, Modes, Screen, SelectionMode};
use kitty_ui::{
    parse_sessions, AddWindowOptions, Edge, Edges, LayoutAddOptions, LayoutConfig, Region,
    RestorePlan, SessionOptions, TabId, TabManager, Window, WindowId, WindowLocation,
};
use kitty_window::{
    EglSurface, Selection as WindowSelection, WindowBackend, WindowConfig, WindowEvent,
};

use crate::{resolve_command, Input, Options, RunOutcome};
use remote::{encode_png, encode_ppm, encode_screenshot, RemoteBridge, RemoteJob};

const CELL_FALLBACK_WIDTH: u32 = 8;
const CELL_FALLBACK_HEIGHT: u32 = 16;
const IDLE_POLL: Duration = Duration::from_millis(50);
const REMOTE_POLL: Duration = Duration::from_millis(5);

fn io_err(message: impl Into<String>) -> io::Error {
    io::Error::other(message.into())
}

fn config_cursor(shape: ConfigCursorShape) -> CursorShape {
    match shape {
        ConfigCursorShape::NoCursor => CursorShape::NoCursor,
        ConfigCursorShape::Block => CursorShape::Block,
        ConfigCursorShape::Beam => CursorShape::Beam,
        ConfigCursorShape::Underline => CursorShape::Underline,
        ConfigCursorShape::Hollow => CursorShape::Hollow,
    }
}

fn layout_base(name: &str) -> &str {
    name.split_once(':').map(|(base, _)| base).unwrap_or(name)
}

/// Default title for a window: the basename of the launched program, matching
/// kitty's `Window.default_title`.
fn default_window_title(argv: &[OsString]) -> String {
    let name = argv
        .first()
        .and_then(|arg| Path::new(arg).file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "kitr".to_string());
    if matches!(name.as_str(), "ssh" | "mosh" | "telnet") {
        let mut args = argv.iter().skip(1);
        while let Some(arg) = args.next() {
            let arg = arg.to_string_lossy();
            if arg.starts_with('-') {
                if matches!(
                    arg.as_ref(),
                    "-p" | "-l"
                        | "-o"
                        | "-i"
                        | "-F"
                        | "-J"
                        | "-c"
                        | "-D"
                        | "-E"
                        | "-e"
                        | "-m"
                        | "-S"
                        | "-b"
                        | "-w"
                        | "-O"
                        | "-Q"
                        | "-R"
                        | "-W"
                ) {
                    args.next();
                }
                continue;
            }
            return format!("{name}: {arg}");
        }
    }
    name
}

/// Titles come from untrusted OSC data; keep them on one line.
fn sanitize_title(title: &str) -> String {
    title.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Enabled layouts for a new tab. When `splits` is enabled it is made the
/// current layout so Ctrl+Shift+Enter has explicit horizontal/vertical split
/// semantics; otherwise the configured order is used unchanged.
fn effective_layouts(config: &kitty_config::Options) -> Vec<String> {
    let mut layouts = config.enabled_layouts.clone();
    if layouts.is_empty() {
        layouts.push("splits".into());
    }
    if let Some(index) = layouts
        .iter()
        .position(|name| layout_base(name) == "splits")
    {
        let splits = layouts.remove(index);
        layouts.insert(0, splits);
    }
    layouts
}

fn pack_sgr(color: Rgba8) -> u32 {
    ((color.r as u32) << 24) | ((color.g as u32) << 16) | ((color.b as u32) << 8) | 2
}

fn force_full_rows(frame: &mut kitty_render::Frame, lines: u32) {
    frame.damage.full_redraw = false;
    frame.damage.full_redraw_reason = None;
    frame.damage.lines.clear();
    frame
        .damage
        .lines
        .push(LineRange::new(0, lines.max(1) as usize));
    frame.damage.cursor_moved = true;
}

fn rgba_of(color: kitty_config::Color) -> Rgba8 {
    Rgba8::rgb(color.red, color.green, color.blue)
}

/// kitty converts margins/padding from points using the logical DPI. The
/// logical DPI is 96 * the window scale factor on X11/EGL here.
fn pt_to_px(value: f64, scale: f64) -> f32 {
    (value * scale * (96.0 / 72.0)).round().max(0.0) as f32
}

fn blend_srgb(top: Rgba8, bottom: Rgba8, alpha: f64) -> Rgba8 {
    let a = alpha.clamp(0.0, 1.0);
    let mix = |t: u8, b: u8| (a * t as f64 + (1.0 - a) * b as f64).round() as u8;
    Rgba8::rgb(
        mix(top.r, bottom.r),
        mix(top.g, bottom.g),
        mix(top.b, bottom.b),
    )
}

fn tab_set_style(screen: &mut Screen, fg: Rgba8, bg: Rgba8, font: TabFontStyle) {
    screen.cursor.sgr.bg = pack_sgr(bg);
    screen.cursor.sgr.fg = pack_sgr(fg);
    screen.cursor.sgr.bold = font.bold;
    screen.cursor.sgr.italic = font.italic;
}

fn tab_draw_chars(screen: &mut Screen, chars: &[char], fg: Rgba8, bg: Rgba8, font: TabFontStyle) {
    for ch in chars {
        if screen.cursor.x >= screen.columns {
            break;
        }
        tab_set_style(screen, fg, bg, font);
        screen.draw_text(&[*ch as u32]);
    }
}

fn tab_title_chars(title: &str, max_cells: usize) -> Vec<char> {
    let title = title.replace('\n', " ");
    let chars: Vec<char> = title.chars().collect();
    if chars.len() <= max_cells || max_cells == 0 {
        return chars.into_iter().take(max_cells).collect();
    }
    let mut ans: Vec<char> = chars.into_iter().take(max_cells - 1).collect();
    ans.push('…');
    ans
}

fn bar_fg_for_tabs(opts: &kitty_config::Options) -> Rgba8 {
    rgba_of(opts.inactive_tab_foreground)
}

fn tab_ideal_width(style: TabBarStyle, title: &str, opts: &kitty_config::Options) -> usize {
    let title_len = title.chars().count();
    match style {
        TabBarStyle::Hidden => 0,
        TabBarStyle::Powerline => title_len + 4,
        TabBarStyle::Separator => {
            let sep_len = opts.tab_separator.chars().count();
            title_len + sep_len
        }
        TabBarStyle::Slant => title_len + 4,
        TabBarStyle::Fade | TabBarStyle::Custom => title_len + opts.tab_fade.len() + 1,
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_tab_powerline(
    screen: &mut Screen,
    title: &str,
    x: u32,
    max_len: u32,
    active: bool,
    next_active: bool,
    tab_bg: impl Fn(bool) -> Rgba8,
    tab_fg: impl Fn(bool) -> Rgba8,
    bar_bg: Rgba8,
    bar_fg: Rgba8,
    style: TabPowerlineStyle,
    active_font: &TabFontStyle,
    inactive_font: &TabFontStyle,
) {
    let (sep, _soft) = match style {
        TabPowerlineStyle::Angled => ('\u{e0b0}', '\u{e0b1}'),
        TabPowerlineStyle::Round => ('\u{e0b4}', '\u{e0b5}'),
        TabPowerlineStyle::Slanted => ('\u{e0bc}', '\u{2571}'),
    };
    let font = if active { *active_font } else { *inactive_font };
    let bg = tab_bg(active);
    let fg = tab_fg(active);
    let next_bg = if next_active {
        tab_bg(true)
    } else if x == 0 {
        bar_bg
    } else {
        tab_bg(false)
    };
    let next_fg = if next_active { tab_fg(true) } else { bar_fg };
    // A leading space is only drawn at the very start of the bar; later tabs
    // butt directly against the previous separator (kitty's start_draw logic).
    let overhead = if x == 0 { 4 } else { 3 };
    if x == 0 {
        tab_draw_chars(screen, &[' '], fg, bg, font);
    }
    let title_chars = tab_title_chars(title, max_len.saturating_sub(overhead) as usize);
    tab_draw_chars(screen, &title_chars, fg, bg, font);
    tab_draw_chars(screen, &[' '], fg, bg, font);
    tab_draw_chars(screen, &[sep], bg, next_bg, font);
    tab_draw_chars(screen, &[' '], next_fg, next_bg, font);
}

#[allow(clippy::too_many_arguments)]
fn draw_tab_separator(
    screen: &mut Screen,
    title: &str,
    _x: u32,
    max_len: u32,
    is_last: bool,
    active: bool,
    tab_bg: impl Fn(bool) -> Rgba8,
    tab_fg: impl Fn(bool) -> Rgba8,
    inactive_bg: Rgba8,
    bar_fg: Rgba8,
    separator: &str,
    active_font: &TabFontStyle,
    inactive_font: &TabFontStyle,
) {
    let font = if active { *active_font } else { *inactive_font };
    let bg = tab_bg(active);
    let fg = tab_fg(active);
    let sep_len = separator.chars().count();
    let title_chars = tab_title_chars(title, max_len.saturating_sub(sep_len as u32) as usize);
    tab_draw_chars(screen, &title_chars, fg, bg, font);
    let sep_chars: Vec<char> = separator.chars().collect();
    if is_last {
        let spaces: Vec<char> = std::iter::repeat_n(' ', sep_len).collect();
        tab_draw_chars(screen, &spaces, fg, bg, font);
    } else {
        // kitty's default separator has a leading space drawn with the tab
        // background, followed by the glyph on the inactive background.
        if let Some((&first, rest)) = sep_chars.split_first() {
            if first == ' ' {
                tab_draw_chars(screen, &[' '], fg, bg, font);
                tab_draw_chars(screen, rest, bar_fg, inactive_bg, font);
            } else {
                tab_draw_chars(screen, &sep_chars, bar_fg, inactive_bg, font);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_tab_slant(
    screen: &mut Screen,
    title: &str,
    _x: u32,
    max_len: u32,
    active: bool,
    tab_bg: impl Fn(bool) -> Rgba8,
    tab_fg: impl Fn(bool) -> Rgba8,
    bar_bg: Rgba8,
    active_font: &TabFontStyle,
    inactive_font: &TabFontStyle,
) {
    let font = if active { *active_font } else { *inactive_font };
    let bg = tab_bg(active);
    let fg = tab_fg(active);
    let title_chars = tab_title_chars(title, max_len.saturating_sub(4) as usize);
    tab_draw_chars(screen, &['\u{e0bc}'], bar_bg, bg, font);
    tab_draw_chars(screen, &[' '], fg, bg, font);
    tab_draw_chars(screen, &title_chars, fg, bg, font);
    tab_draw_chars(screen, &[' '], fg, bg, font);
    tab_draw_chars(screen, &['\u{e0be}'], bar_bg, bg, font);
}

#[allow(clippy::too_many_arguments)]
fn draw_tab_fade(
    screen: &mut Screen,
    title: &str,
    _x: u32,
    max_len: u32,
    active: bool,
    tab_bg: impl Fn(bool) -> Rgba8,
    tab_fg: impl Fn(bool) -> Rgba8,
    bar_bg: Rgba8,
    active_font: &TabFontStyle,
    inactive_font: &TabFontStyle,
) {
    let font = if active { *active_font } else { *inactive_font };
    let bg = tab_bg(active);
    let fg = tab_fg(active);
    let fade = [0.25, 0.5, 0.75, 1.0];
    for alpha in fade {
        let color = blend_srgb(bg, bar_bg, alpha);
        tab_draw_chars(screen, &[' '], color, color, font);
    }
    let title_chars = tab_title_chars(title, max_len.saturating_sub(9) as usize);
    tab_draw_chars(screen, &title_chars, fg, bg, font);
    for alpha in fade.into_iter().rev() {
        let color = blend_srgb(bg, bar_bg, alpha);
        tab_draw_chars(screen, &[' '], color, color, font);
    }
    tab_draw_chars(screen, &[' '], fg, bar_bg, font);
}

fn draw_frame(
    backend: &mut Option<GlBackend>,
    renderer: &RendererCore,
    frame: &kitty_render::Frame,
    config: &RenderConfig,
) -> io::Result<()> {
    backend
        .as_mut()
        .ok_or_else(|| io_err("OpenGL backend is shut down"))?
        .render_with_atlas(frame, config, &renderer.atlas)
        .map(|_| ())
        .map_err(|error| io_err(error.to_string()))
}

/// A live GUI terminal session.
///
/// The public methods operate on the active tab and active pane. Tests use
/// them to inject synthetic events, switch tabs, capture the framebuffer and
/// inspect the active screen.
fn layer_shell_window_config(
    config: &kitty_kittens::panel::LayerShellConfig,
) -> kitty_window::LayerShellConfig {
    use kitty_kittens::panel::{Edge, FocusPolicy, Layer};
    let edge = match config.edge {
        Edge::Top => kitty_window::LayerShellEdge::Top,
        Edge::Bottom => kitty_window::LayerShellEdge::Bottom,
        Edge::Left => kitty_window::LayerShellEdge::Left,
        Edge::Right => kitty_window::LayerShellEdge::Right,
        Edge::Center => kitty_window::LayerShellEdge::Center,
        Edge::None => kitty_window::LayerShellEdge::None,
        Edge::CenterSized => kitty_window::LayerShellEdge::CenterSized,
        Edge::Background => kitty_window::LayerShellEdge::Top,
    };
    let layer = if config.edge == Edge::Background {
        kitty_window::LayerShellLayer::Background
    } else {
        match config.layer {
            Layer::Background => kitty_window::LayerShellLayer::Background,
            Layer::Bottom => kitty_window::LayerShellLayer::Bottom,
            Layer::Top => kitty_window::LayerShellLayer::Top,
            Layer::Overlay => kitty_window::LayerShellLayer::Overlay,
        }
    };
    let focus = match config.focus_policy {
        FocusPolicy::NotAllowed => kitty_window::LayerShellFocus::NotAllowed,
        FocusPolicy::Exclusive => kitty_window::LayerShellFocus::Exclusive,
        FocusPolicy::OnDemand => kitty_window::LayerShellFocus::OnDemand,
    };
    let width = if config.x_size_in_pixels > 0 {
        config.x_size_in_pixels.max(0) as u32
    } else {
        (config.x_size_in_cells.max(1) as u32) * CELL_FALLBACK_WIDTH
    };
    let height = if config.y_size_in_pixels > 0 {
        config.y_size_in_pixels.max(0) as u32
    } else {
        (config.y_size_in_cells.max(1) as u32) * CELL_FALLBACK_HEIGHT
    };
    kitty_window::LayerShellConfig {
        layer,
        edge,
        width,
        height,
        margin_top: config.margin_top,
        margin_left: config.margin_left,
        margin_bottom: config.margin_bottom,
        margin_right: config.margin_right,
        focus,
        exclusive_zone: config.exclusive_zone,
        override_exclusive_zone: config.override_exclusive_zone,
        output_name: config.output_name.clone(),
        namespace: "kitty-panel".to_owned(),
    }
}

pub struct GuiSession {
    backend: Option<GlBackend>,
    surface: EglSurface,
    window: WindowBackend,
    renderer: RendererCore,
    fonts: FontSet,
    raster_cache: RasterCache,
    resolver: ConfigColorResolver,
    config_options: kitty_config::Options,

    tab_manager: TabManager,
    panes: HashMap<WindowId, Pane>,
    next_window_id: WindowId,

    padding: (f32, f32, f32, f32),
    metrics: CellMetrics,
    pixel_size: (u32, u32),
    columns: u32,
    rows: u32,
    tab_bar_px: f32,
    tab_ranges: Vec<(usize, u32, u32)>,

    keymap: KeyMap,
    scrollback: u32,
    listen_env: Option<String>,
    remote_rx: Option<Receiver<RemoteJob>>,
    remote_server: Option<kitty_remote::ServerHandle>,

    need_redraw: bool,
    frame_count: u64,
    output_bytes: u64,
    should_close: bool,
    mouse_down: bool,
    /// Visual cell under the pointer, used for OSC 8 hover highlighting.
    hover_cell: Option<(u32, u32)>,
    /// Previous left click for double/triple click selection.
    last_click: Option<(Instant, (u32, u32), u8)>,
    last_title: Option<String>,
    timed_out: bool,
    exit_status: Option<ExitStatus>,
    final_errors: Vec<String>,
    final_screen: Option<Screen>,
    fallback_screen: Screen,
}

impl GuiSession {
    pub fn open(options: &Options, input: Input) -> io::Result<Self> {
        Self::open_on_display(options, input, None)
    }

    /// Open on an explicit X display name (or `$DISPLAY` when `None`).
    pub fn open_on_display(
        options: &Options,
        input: Input,
        display_name: Option<&str>,
    ) -> io::Result<Self> {
        let config = load_config(options);
        let cfg = config.options.clone();
        for warning in &config.warnings {
            eprintln!("kitr: config: {warning}");
        }
        for bad in &config.bad_lines {
            eprintln!("kitr: config: {bad}");
        }
        let plan = load_session_plan(options, &cfg)?;
        let initial_tab_count = plan
            .as_ref()
            .and_then(|plan| plan.os_windows.first())
            .map(|os_window| os_window.tabs.len())
            .unwrap_or(1)
            .max(1);

        let title = options.title.clone().unwrap_or_else(|| "kitr".to_string());
        let requested = options.size.unwrap_or((80, 24));
        let provisional_width = requested.0 as u32 * CELL_FALLBACK_WIDTH;
        let provisional_height = (requested.1 as u32 + 1) * CELL_FALLBACK_HEIGHT;

        let mut window_config = WindowConfig::new(
            title.clone(),
            provisional_width.max(1),
            provisional_height.max(1),
        );
        if let Some(layer_shell) = &options.layer_shell {
            window_config = window_config.layer_shell(layer_shell_window_config(layer_shell));
            if options.layer_shell_start_hidden {
                window_config.visible = false;
            }
        }
        let mut window = WindowBackend::open_on_display(&window_config, display_name)
            .map_err(|error| io_err(format!("cannot open window: {error}")))?;
        let scale = window.scale_factor().clamp(0.5, 8.0);

        let fonts = FontSet::load(&cfg, scale)
            .map_err(|error| io_err(format!("cannot load fonts: {error}")))?;
        let metrics: CellMetrics = fonts.cell_metrics().into();
        let cell_w = metrics.cell_width.max(1.0).round() as u32;
        let cell_h = metrics.cell_height.max(1.0).round() as u32;
        let padding = (
            pt_to_px(cfg.window_padding_width.left, scale),
            pt_to_px(cfg.window_padding_width.top, scale),
            pt_to_px(cfg.window_padding_width.right, scale),
            pt_to_px(cfg.window_padding_width.bottom, scale),
        );
        let (columns, rows) = requested;
        let tab_bar_px = cfg.tab_bar_height(initial_tab_count, cell_h as f32);
        // kitty treats `initial_window_height` as the total window height:
        // a visible tab bar takes one row out of the central grid rather than
        // growing the window.
        let content_rows = rows.saturating_sub(u16::from(tab_bar_px > 0.0)).max(1);
        let pixel_width = columns as u32 * cell_w + (padding.0 + padding.2).max(0.0) as u32;
        let pixel_height = rows as u32 * cell_h + (padding.1 + padding.3).max(0.0) as u32;
        window.set_size(pixel_width.max(1), pixel_height.max(1));
        window.set_size_limits(cell_w, cell_h, None, None);

        let surface = EglSurface::new(&window)
            .map_err(|error| io_err(format!("cannot create EGL window surface: {error}")))?;
        surface
            .make_current()
            .map_err(|error| io_err(format!("cannot make EGL context current: {error}")))?;
        // SAFETY: the EGL context created by kitty-window is current.
        let gl = unsafe { GlFns::load() }
            .map_err(|error| io_err(format!("cannot load OpenGL entry points: {error}")))?;
        let (surface_width, surface_height) = surface.size();
        let backend = GlBackend::with_loader_config(
            gl,
            surface_width,
            surface_height,
            // kitty composites text and colours in linear light into an
            // sRGB framebuffer; plain sRGB blending makes light-on-dark text
            // look thinner than in kitty.
            GlBackendConfig {
                gamma: GammaMode::Linear,
                srgb_compositing: true,
                ..GlBackendConfig::default()
            },
        )
        .map_err(|error| io_err(format!("cannot create OpenGL backend: {error}")))?;

        let scrollback = if options.scrollback_explicit {
            options.scrollback
        } else {
            cfg.scrollback_lines.min(u32::MAX as u64) as u32
        }
        .max(1);

        let render_config = RenderConfig {
            metrics,
            origin_x: padding.0,
            origin_y: padding.1,
            font_id: 0,
            cursor_beam_thickness: (cell_w as f32 * 0.125).max(1.0),
            cursor_underline_thickness: (cell_h as f32 * 0.08).max(1.0),
            cursor_hollow_thickness: 1.0,
            undercurl_style: 0,
            emit_default_backgrounds: false,
            text_contrast: 1.0 + (cfg.text_contrast as f32) * 0.01,
            text_gamma_adjustment: if cfg.text_gamma_adjustment < 0.01 {
                1.0
            } else {
                1.0 / cfg.text_gamma_adjustment as f32
            },
            text_old_gamma: cfg.text_old_gamma,
            animation_cache_dir: Some(std::env::temp_dir()),
            ..RenderConfig::default()
        };
        let renderer = RendererCore::new(render_config, AtlasConfig::default());
        let resolver = ConfigColorResolver::new(&cfg);

        let keymap = build_keymap(&cfg);

        let (remote_rx, remote_server, listen_env) = start_remote(options.listen_on.as_deref())?;

        let fallback = Screen::builder(1, 1)
            .build()
            .map_err(|error| io_err(error.to_string()))?;
        let mut session = Self {
            backend: Some(backend),
            surface,
            window,
            renderer,
            fonts,
            raster_cache: RasterCache::new(),
            resolver,
            config_options: cfg.clone(),
            tab_manager: TabManager::new(1),
            panes: HashMap::new(),
            next_window_id: 1,
            padding,
            metrics,
            pixel_size: (surface_width, surface_height),
            columns: columns as u32,
            rows: content_rows as u32,
            tab_bar_px,
            tab_ranges: Vec::new(),
            keymap,
            scrollback,
            listen_env,
            remote_rx,
            remote_server,
            need_redraw: true,
            frame_count: 0,
            output_bytes: 0,
            should_close: false,
            mouse_down: false,
            hover_cell: None,
            last_click: None,
            last_title: Some(title),
            timed_out: false,
            exit_status: None,
            final_errors: Vec::new(),
            final_screen: None,
            fallback_screen: fallback,
        };

        session.initialize(options, input, plan)?;
        session.relayout_all();
        session.update_window_title();
        Ok(session)
    }

    fn initialize(
        &mut self,
        options: &Options,
        input: Input,
        plan: Option<RestorePlan>,
    ) -> io::Result<()> {
        if let Some(plan) = plan {
            self.restore_plan(plan)?;
        } else {
            let layouts = effective_layouts(&self.config_options);
            let layout = layouts[0].clone();
            let tab_id = self
                .tab_manager
                .add_tab("", None, &layouts, self.layout_config())
                .map_err(io_err)?;
            let idx = self
                .tab_manager
                .tabs()
                .iter()
                .position(|tab| tab.id == tab_id)
                .unwrap_or(0);
            if let Some(tab) = self.tab_manager.tab_mut(idx) {
                let _ = tab.set_enabled_layouts(&layouts);
                if let Err(error) = tab.set_layout(&layout) {
                    eprintln!("kitr: layout: {error}");
                }
            }
            let spec = self.spawn_spec_from_options(options);
            self.add_pane_at(idx, spec, LayoutAddOptions::default())?;
            if let Input::Bytes(bytes) = &input {
                if let Some(pane) = self.active_pane_mut() {
                    pane.write(bytes);
                }
            }
        }
        self.need_redraw = true;
        Ok(())
    }

    fn restore_plan(&mut self, plan: RestorePlan) -> io::Result<()> {
        let Some(os_window) = plan.os_windows.first().cloned() else {
            return Err(io_err("session defines no OS windows"));
        };
        if plan.os_windows.len() > 1 {
            eprintln!(
                "kitr: session defines {} OS windows; this build opens the first one and ignores the rest",
                plan.os_windows.len()
            );
        }
        let tabs = os_window.tabs.clone();
        for tab_restore in &tabs {
            let mut layouts = tab_restore.enabled_layouts.clone();
            if layouts.is_empty() {
                layouts = effective_layouts(&self.config_options);
            }
            let tab_id = self
                .tab_manager
                .add_tab(
                    tab_restore.name.clone(),
                    tab_restore.cwd.clone(),
                    &layouts,
                    self.layout_config(),
                )
                .map_err(|error| io_err(format!("session: {error}")))?;
            let tab_idx = self
                .tab_manager
                .tabs()
                .iter()
                .position(|tab| tab.id == tab_id)
                .unwrap_or(0);
            let mut restored_windows = Vec::new();
            for window_spec in &tab_restore.windows {
                let wid = self.alloc_window_id();
                let title = window_spec
                    .title
                    .clone()
                    .or_else(|| window_spec.launch.title.clone())
                    .unwrap_or_else(|| {
                        let argv: Vec<OsString> =
                            window_spec.launch.args.iter().map(OsString::from).collect();
                        if argv.is_empty() {
                            default_window_title(&[self.default_shell()])
                        } else {
                            default_window_title(&argv)
                        }
                    });
                let mut window = Window::new(wid, title);
                self.apply_window_decorations(&mut window);
                window.serialized_id = if window_spec.serialized_id != 0 {
                    window_spec.serialized_id
                } else {
                    wid
                };
                window.cwd = window_spec.cwd.clone();
                window.launch = window_spec.launch.clone();
                restored_windows.push(window);
            }
            {
                let tab = self.tab_manager.tab_mut(tab_idx).expect("new tab");
                if let Err(error) = tab.set_enabled_layouts(&layouts) {
                    eprintln!("kitr: session: {error}");
                }
                if let Err(error) = tab.set_layout(&tab_restore.layout) {
                    eprintln!("kitr: session: {error}");
                }
                for (index, window) in restored_windows.into_iter().enumerate() {
                    if index == 0 {
                        tab.add_window_raw(
                            window,
                            AddWindowOptions {
                                make_active: true,
                                ..Default::default()
                            },
                        );
                        continue;
                    }
                    let location = window
                        .launch
                        .location
                        .as_deref()
                        .and_then(WindowLocation::from_kitty_name)
                        .unwrap_or(WindowLocation::Default);
                    tab.add_window(
                        window,
                        &LayoutAddOptions {
                            location,
                            ..Default::default()
                        },
                    );
                }
                if let Some(state) = &tab_restore.layout_state {
                    if !tab.unserialize_layout_state_from_session(state) {
                        eprintln!(
                            "kitr: session: could not restore layout state for tab {:?}",
                            tab_restore.name
                        );
                    }
                }
                tab.apply_layout();
                let ids = tab.windows().all_window_ids().to_vec();
                let mut focus_id = ids.get(tab_restore.active_window_idx).copied();
                for (index, spec) in tab_restore.windows.iter().enumerate() {
                    let Some(needle) = spec.focus_matching_window.as_deref() else {
                        continue;
                    };
                    let matches = ids
                        .get(index)
                        .and_then(|wid| tab.windows().window(*wid))
                        .map(|window| window.effective_title().contains(needle))
                        .unwrap_or(false);
                    if matches {
                        focus_id = ids.get(index).copied();
                    }
                }
                if let Some(wid) = focus_id {
                    tab.set_active_window(wid);
                }
            }
            let ids = self
                .tab_manager
                .tab(tab_idx)
                .map(|tab| tab.windows().all_window_ids().to_vec())
                .unwrap_or_default();
            for (index, wid) in ids.iter().copied().enumerate() {
                let Some(window_spec) = tab_restore.windows.get(index) else {
                    continue;
                };
                let (pane_columns, pane_rows) = self
                    .pane_geometry(tab_idx, wid)
                    .map(|(columns, rows, _, _)| (columns, rows))
                    .unwrap_or((self.columns, self.rows));
                let spec = self.spawn_spec_from_restore(window_spec, wid);
                let pane = Pane::spawn(
                    &spec,
                    pane_columns,
                    pane_rows,
                    self.scrollback,
                    config_cursor(self.config_options.cursor_shape),
                    self.metrics.cell_width.max(1.0).round() as u32,
                    self.metrics.cell_height.max(1.0).round() as u32,
                )?;
                self.panes.insert(wid, pane);
            }
        }
        let mut active = os_window
            .active_tab_idx
            .min(self.tab_manager.tabs().len().saturating_sub(1));
        if let Some(spec) = os_window.focus_tab_spec.as_deref() {
            let needle = spec.strip_prefix("title:").unwrap_or(spec);
            if let Some(index) = self
                .tab_manager
                .tabs()
                .iter()
                .position(|tab| tab.effective_title().contains(needle))
            {
                active = index;
            }
        }
        self.tab_manager.set_active_tab_idx(active);
        Ok(())
    }

    fn alloc_window_id(&mut self) -> WindowId {
        let id = self.next_window_id;
        self.next_window_id += 1;
        id
    }

    fn layout_config(&self) -> LayoutConfig {
        let cw = self.metrics.cell_width.max(1.0).round() as i32;
        let ch = self.metrics.cell_height.max(1.0).round() as i32;
        let left = self.padding.0.round() as i32;
        let top = (self.tab_bar_top_px() + self.padding.1).round() as i32;
        let width = (self.pixel_size.0 as f32 - self.padding.0 - self.padding.2)
            .max(cw as f32)
            .round() as i32;
        let height = (self.pixel_size.1 as f32
            - self.tab_bar_top_px()
            - self.tab_bar_bottom_px()
            - self.padding.1
            - self.padding.3)
            .max(ch as f32)
            .round() as i32;
        let opts = &self.config_options;
        let has_margin = opts.window_margin_width.left > 0.0
            || opts.window_margin_width.top > 0.0
            || opts.window_margin_width.right > 0.0
            || opts.window_margin_width.bottom > 0.0;
        let draw_minimal_borders = opts.draw_minimal_borders && !has_margin;
        LayoutConfig {
            central: Region::new(left, top, width, height),
            cell_width: cw,
            cell_height: ch,
            draw_minimal_borders,
            alignment_x: 0,
            alignment_y: 0,
        }
    }

    fn apply_window_decorations(&self, window: &mut Window) {
        let scale = self.window.scale_factor();
        let opts = &self.config_options;
        let margin = opts.window_margin_width;
        let padding = opts.window_padding_width;
        window.margin = Edges::new(
            pt_to_px(margin.left, scale) as i32,
            pt_to_px(margin.top, scale) as i32,
            pt_to_px(margin.right, scale) as i32,
            pt_to_px(margin.bottom, scale) as i32,
        );
        window.padding = Edges::new(
            pt_to_px(padding.left, scale) as i32,
            pt_to_px(padding.top, scale) as i32,
            pt_to_px(padding.right, scale) as i32,
            pt_to_px(padding.bottom, scale) as i32,
        );
        let border = (opts.window_border_width * scale).round();
        window.border_width = if opts.window_border_width > 0.0 {
            border.max(1.0) as i32
        } else {
            0
        };
    }

    fn apply_decorations_to_all(&mut self) {
        let mut updates = Vec::new();
        for tab in self.tab_manager.tabs() {
            for wid in tab.windows().all_window_ids() {
                updates.push(*wid);
            }
        }
        for wid in updates {
            let Some(tab_idx) = self.tab_index_for_window(wid) else {
                continue;
            };
            let mut decorated = self
                .tab_manager
                .tab(tab_idx)
                .and_then(|tab| tab.windows().window(wid))
                .cloned();
            if let Some(window) = decorated.as_mut() {
                self.apply_window_decorations(window);
            }
            if let Some(window) = decorated {
                if let Some(target) = self
                    .tab_manager
                    .tab_mut(tab_idx)
                    .and_then(|tab| tab.windows_mut().window_mut(wid))
                {
                    *target = window;
                }
            }
        }
    }

    fn spawn_spec_from_options(&self, options: &Options) -> SpawnSpec {
        SpawnSpec {
            argv: resolve_command(options),
            cwd: None,
            env: self.base_env(None),
            title: options.title.clone(),
        }
    }

    fn spawn_spec_from_restore(
        &self,
        window: &kitty_ui::WindowRestore,
        wid: WindowId,
    ) -> SpawnSpec {
        let mut argv: Vec<OsString> = window.launch.args.iter().map(OsString::from).collect();
        if argv.is_empty() {
            argv = vec![self.default_shell()];
        }
        let mut env = self.base_env(Some(wid));
        for (key, value) in &window.launch.env {
            env.push((OsString::from(key), OsString::from(value)));
        }
        for (key, value) in &window.launch.vars {
            env.push((
                OsString::from(format!("KITTY_VAR_{key}")),
                OsString::from(value),
            ));
        }
        SpawnSpec {
            argv,
            cwd: window.cwd.clone(),
            env,
            title: window.title.clone().or_else(|| window.launch.title.clone()),
        }
    }

    fn default_shell(&self) -> OsString {
        let shell = self.config_options.shell.as_str();
        if shell.is_empty() || shell == "." {
            crate::default_shell()
        } else {
            OsString::from(shell)
        }
    }

    fn base_env(&self, window_id: Option<WindowId>) -> Vec<(OsString, OsString)> {
        let mut env: Vec<(OsString, OsString)> = self
            .config_options
            .env
            .iter()
            .map(|(key, value)| (OsString::from(key), OsString::from(value)))
            .collect();
        if let Some(wid) = window_id {
            env.push((
                OsString::from("KITTY_WINDOW_ID"),
                OsString::from(wid.to_string()),
            ));
        }
        if let Some(address) = &self.listen_env {
            env.push((OsString::from("KITTY_LISTEN_ON"), OsString::from(address)));
        }
        env
    }

    fn new_tab_spec(&self) -> SpawnSpec {
        SpawnSpec {
            argv: vec![self.default_shell()],
            cwd: None,
            env: self.base_env(None),
            title: None,
        }
    }

    fn add_pane_at(
        &mut self,
        tab_idx: usize,
        spec: SpawnSpec,
        opts: LayoutAddOptions,
    ) -> io::Result<WindowId> {
        let wid = self.alloc_window_id();
        let title = spec.title.clone().unwrap_or_else(|| {
            if spec.argv.is_empty() {
                default_window_title(&[self.default_shell()])
            } else {
                default_window_title(&spec.argv)
            }
        });
        let mut window = Window::new(wid, title.clone());
        self.apply_window_decorations(&mut window);
        let tab = self
            .tab_manager
            .tab_mut(tab_idx)
            .ok_or_else(|| io_err("invalid tab index"))?;
        tab.add_window(window, &opts);
        tab.apply_layout();
        let (columns, rows) = self
            .pane_geometry(tab_idx, wid)
            .map(|(columns, rows, _, _)| (columns, rows))
            .unwrap_or((self.columns, self.rows));
        let spec = SpawnSpec {
            env: spec
                .env
                .into_iter()
                .chain(self.base_env(Some(wid)))
                .collect(),
            title: Some(title),
            ..spec
        };
        match Pane::spawn(
            &spec,
            columns,
            rows,
            self.scrollback,
            config_cursor(self.config_options.cursor_shape),
            self.metrics.cell_width.max(1.0).round() as u32,
            self.metrics.cell_height.max(1.0).round() as u32,
        ) {
            Ok(pane) => {
                self.panes.insert(wid, pane);
                // A split changes the geometry of the existing panes too.
                self.relayout_all();
                self.need_redraw = true;
                Ok(wid)
            }
            Err(error) => {
                if let Some(tab) = self.tab_manager.tab_mut(tab_idx) {
                    tab.close_window(wid);
                }
                Err(error)
            }
        }
    }

    fn new_tab(&mut self) -> io::Result<()> {
        let layouts = effective_layouts(&self.config_options);
        let layout = layouts[0].clone();
        let tab_id = self
            .tab_manager
            .add_tab("", None, &layouts, self.layout_config())
            .map_err(io_err)?;
        let idx = self
            .tab_manager
            .tabs()
            .iter()
            .position(|tab| tab.id == tab_id)
            .unwrap_or(0);
        if let Some(tab) = self.tab_manager.tab_mut(idx) {
            let _ = tab.set_enabled_layouts(&layouts);
            let _ = tab.set_layout(&layout);
        }
        let spec = self.new_tab_spec();
        self.add_pane_at(idx, spec, LayoutAddOptions::default())?;
        self.update_tab_bar_chrome();
        self.need_redraw = true;
        self.update_window_title();
        Ok(())
    }

    fn close_tab_at(&mut self, idx: usize) -> bool {
        let Some(tab) = self.tab_manager.tab(idx) else {
            return false;
        };
        let ids = tab.windows().all_window_ids().to_vec();
        for wid in ids {
            if let Some(mut pane) = self.panes.remove(&wid) {
                pane.terminate();
            }
        }
        if self.tab_manager.tabs().len() > 1 {
            self.tab_manager.close_tab(idx);
        } else {
            self.should_close = true;
        }
        self.update_tab_bar_chrome();
        self.need_redraw = true;
        self.update_window_title();
        true
    }

    fn close_window_id(&mut self, wid: WindowId) -> bool {
        let Some(tab_idx) = self.tab_index_for_window(wid) else {
            return false;
        };
        let single = self
            .tab_manager
            .tab(tab_idx)
            .map(|tab| tab.windows().len() <= 1)
            .unwrap_or(true);
        if single {
            return self.close_tab_at(tab_idx);
        }
        if let Some(mut pane) = self.panes.remove(&wid) {
            pane.terminate();
        }
        if let Some(tab) = self.tab_manager.tab_mut(tab_idx) {
            tab.close_window(wid);
        }
        self.relayout_all();
        self.need_redraw = true;
        self.update_window_title();
        true
    }

    fn tab_index_for_window(&self, wid: WindowId) -> Option<usize> {
        self.tab_manager
            .tabs()
            .iter()
            .position(|tab| tab.windows().window(wid).is_some())
    }

    fn relayout_all(&mut self) {
        self.apply_decorations_to_all();
        let config = self.layout_config();
        let count = self.tab_manager.tabs().len();
        for tab_idx in 0..count {
            let ids = {
                let Some(tab) = self.tab_manager.tab_mut(tab_idx) else {
                    continue;
                };
                tab.layouts_mut().set_config(config);
                tab.apply_layout();
                tab.windows().all_window_ids().to_vec()
            };
            for wid in ids {
                let Some((columns, rows, _, _)) = self.pane_geometry(tab_idx, wid) else {
                    continue;
                };
                if let Some(pane) = self.panes.get_mut(&wid) {
                    if pane.resize(columns, rows) {
                        self.need_redraw = true;
                    }
                }
            }
        }
    }

    fn pane_pixel_rect(&self, tab_idx: usize, wid: WindowId) -> Option<(f32, f32, f32, f32)> {
        let window = self.tab_manager.tab(tab_idx)?.windows().window(wid)?;
        let geometry = window.geometry;
        let cw = self.metrics.cell_width.max(1.0);
        let ch = self.metrics.cell_height.max(1.0);
        Some((
            geometry.left as f32,
            geometry.top as f32,
            ((geometry.right - geometry.left) as f32).max(cw),
            ((geometry.bottom - geometry.top) as f32).max(ch),
        ))
    }

    fn pane_geometry(&self, tab_idx: usize, wid: WindowId) -> Option<(u32, u32, i32, i32)> {
        let (x, y, width, height) = self.pane_pixel_rect(tab_idx, wid)?;
        let cw = self.metrics.cell_width.max(1.0);
        let ch = self.metrics.cell_height.max(1.0);
        let columns = (width / cw).floor().max(1.0) as u32;
        let rows = (height / ch).floor().max(1.0) as u32;
        let left = (x / cw).round() as i32;
        let top = (y / ch).round() as i32;
        Some((columns, rows, left, top))
    }

    fn pane_origin(&self, tab_idx: usize, wid: WindowId) -> Option<(f32, f32)> {
        let (x, y, _, _) = self.pane_pixel_rect(tab_idx, wid)?;
        Some((x, y))
    }

    fn active_pane(&self) -> Option<&Pane> {
        let wid = self.tab_manager.active_window_id()?;
        self.panes.get(&wid)
    }

    fn active_pane_mut(&mut self) -> Option<&mut Pane> {
        let wid = self.tab_manager.active_window_id()?;
        self.panes.get_mut(&wid)
    }

    pub fn screen(&self) -> &Screen {
        if let Some(pane) = self.active_pane() {
            return pane.screen();
        }
        if let Some(screen) = &self.final_screen {
            return screen;
        }
        &self.fallback_screen
    }

    pub fn frame_count(&self) -> u64 {
        self.frame_count
    }

    pub fn output_bytes(&self) -> u64 {
        self.output_bytes
    }

    pub fn child_pid(&self) -> i32 {
        self.active_pane().map(|pane| pane.child.pid()).unwrap_or(0)
    }

    pub fn window_size(&self) -> (u32, u32) {
        self.pixel_size
    }

    /// Width and height of one terminal cell in device pixels.
    pub fn cell_size(&self) -> (f32, f32) {
        (self.metrics.cell_width, self.metrics.cell_height)
    }

    /// Title currently applied to the OS window.
    pub fn window_title(&self) -> &str {
        self.window.title()
    }

    /// Read the current clipboard contents (best effort; for tests).
    pub fn clipboard_text(&mut self) -> Option<String> {
        self.window
            .clipboard_text_blocking(WindowSelection::Clipboard, Duration::from_millis(500))
            .ok()
    }

    /// Rendered tab hit boxes as `(tab index, start cell, end cell)`.
    pub fn tab_ranges(&self) -> &[(usize, u32, u32)] {
        &self.tab_ranges
    }

    /// Whether the tab bar is currently drawn at the top edge.
    pub fn tab_bar_at_top(&self) -> bool {
        self.config_options.tab_bar_edge.is_top()
    }

    /// Height of the tab-bar strip in device pixels (zero when hidden).
    pub fn tab_bar_height(&self) -> f32 {
        self.tab_bar_px
    }

    /// Whether the tab bar is currently visible.
    pub fn tab_bar_visible(&self) -> bool {
        self.tab_bar_px > 0.0
    }

    pub fn tab_count(&self) -> usize {
        self.tab_manager.tabs().len()
    }

    pub fn active_tab_index(&self) -> usize {
        self.tab_manager.active_tab_idx()
    }

    pub fn active_tab_id(&self) -> Option<TabId> {
        self.tab_manager.active_tab().map(|tab| tab.id)
    }

    pub fn tab_titles(&self) -> Vec<String> {
        self.tab_manager
            .tabs()
            .iter()
            .map(|tab| tab.effective_title())
            .collect()
    }

    pub fn pane_count(&self) -> usize {
        self.panes.len()
    }

    pub fn active_window_id(&self) -> Option<WindowId> {
        self.tab_manager.active_window_id()
    }

    pub fn pane_rect(&self, wid: WindowId) -> Option<(u32, u32, u32, u32)> {
        let tab_idx = self.tab_index_for_window(wid)?;
        let (columns, rows, left, top) = self.pane_geometry(tab_idx, wid)?;
        Some((left as u32, top as u32, columns, rows))
    }

    pub fn select_tab(&mut self, idx: usize) -> bool {
        let changed = self.tab_manager.set_active_tab_idx(idx);
        if changed {
            self.need_redraw = true;
            self.update_window_title();
        }
        changed
    }

    pub fn new_tab_for_test(&mut self, command: &[&str]) -> io::Result<()> {
        let layouts = effective_layouts(&self.config_options);
        let layout = layouts[0].clone();
        let tab_id = self
            .tab_manager
            .add_tab("", None, &layouts, self.layout_config())
            .map_err(io_err)?;
        let idx = self
            .tab_manager
            .tabs()
            .iter()
            .position(|tab| tab.id == tab_id)
            .unwrap_or(0);
        if let Some(tab) = self.tab_manager.tab_mut(idx) {
            let _ = tab.set_layout(&layout);
        }
        let spec = SpawnSpec {
            argv: command.iter().map(OsString::from).collect(),
            cwd: None,
            env: self.base_env(None),
            title: None,
        };
        self.add_pane_at(idx, spec, LayoutAddOptions::default())?;
        self.update_tab_bar_chrome();
        self.need_redraw = true;
        self.update_window_title();
        Ok(())
    }

    pub fn inject_event(&mut self, event: &WindowEvent) -> io::Result<()> {
        self.window
            .inject_event(event)
            .map_err(|error| io_err(error.to_string()))
    }

    pub fn resize_window(&mut self, width: u32, height: u32) {
        self.window.set_size(width, height);
    }

    pub fn write_child(&mut self, bytes: &[u8]) {
        if let Some(pane) = self.active_pane_mut() {
            pane.write(bytes);
        }
    }

    /// Process one bounded event-loop iteration. Returns false when the
    /// session has stopped (all tabs closed, close request or timeout).
    pub fn step(&mut self, timeout: Duration) -> io::Result<bool> {
        self.read_all()?;
        self.flush_all()?;
        self.check_children()?;
        self.process_remote_jobs()?;

        if self.should_close {
            return Ok(false);
        }

        let timeout_ms = if self.need_redraw {
            0
        } else {
            let limit = if self.remote_rx.is_some() {
                timeout.min(REMOTE_POLL)
            } else {
                timeout
            };
            limit.as_millis().min(i32::MAX as u128) as i32
        };
        let events = self.window.poll_events(timeout_ms.max(0));
        for event in events {
            self.handle_window_event(event)?;
        }
        if self.should_close {
            return Ok(false);
        }
        self.read_all()?;
        self.flush_all()?;
        self.check_children()?;
        self.process_remote_jobs()?;

        if self.need_redraw {
            self.render()?;
        }
        Ok(!self.should_close)
    }

    /// Render one frame regardless of damage.
    pub fn render_now(&mut self) -> io::Result<()> {
        self.need_redraw = true;
        self.render()
    }

    /// Read the current framebuffer as top-down RGBA8.
    pub fn capture_rgba(&mut self) -> io::Result<(u32, u32, Vec<u8>)> {
        self.surface
            .make_current()
            .map_err(|error| io_err(error.to_string()))?;
        let (width, height) = self.surface.refresh_size();
        let mut pixels = self
            .surface
            .read_pixels()
            .map_err(|error| io_err(error.to_string()))?;
        let stride = width as usize * 4;
        if stride > 0 {
            let mut scratch = vec![0u8; stride];
            for row in 0..height as usize / 2 {
                let top = row * stride;
                let bottom = (height as usize - 1 - row) * stride;
                scratch.copy_from_slice(&pixels[top..top + stride]);
                pixels.copy_within(bottom..bottom + stride, top);
                pixels[bottom..bottom + stride].copy_from_slice(&scratch);
            }
        }
        Ok((width, height, pixels))
    }

    pub fn write_screenshot(&mut self, path: &Path) -> io::Result<()> {
        let (width, height, pixels) = self.capture_rgba()?;
        write_ppm(path, width, height, &pixels)
    }

    /// Run until the session stops or `timeout` elapses.
    pub fn run(
        mut self,
        timeout: Option<Duration>,
        screenshot: Option<&Path>,
        screenshot_delay: Duration,
    ) -> io::Result<RunOutcome> {
        let started = Instant::now();
        let deadline = timeout.map(|t| started + t);
        let mut content_at: Option<Instant> = None;
        let mut screenshot_written = false;
        loop {
            if let Some(deadline) = deadline {
                if Instant::now() >= deadline {
                    self.timed_out = true;
                    break;
                }
            }
            if screenshot.is_some() && self.output_bytes > 0 && content_at.is_none() {
                content_at = Some(Instant::now());
            }
            if let (Some(path), Some(at)) = (screenshot, content_at) {
                if at.elapsed() >= screenshot_delay {
                    for _ in 0..3 {
                        self.render_now()?;
                    }
                    self.write_screenshot(path)?;
                    screenshot_written = true;
                    break;
                }
            }
            if screenshot.is_some()
                && self.output_bytes == 0
                && started.elapsed() >= Duration::from_secs(1)
            {
                break;
            }
            let alive = self.step(IDLE_POLL)?;
            if !alive {
                break;
            }
        }
        if screenshot.is_some() && !screenshot_written {
            for _ in 0..3 {
                let _ = self.render_now();
            }
            if let Some(path) = screenshot {
                let _ = self.write_screenshot(path);
            }
        }
        self.shutdown();
        if screenshot.is_some() && !self.timed_out {
            self.exit_status = Some(ExitStatus::Exited(0));
        }
        Ok(self.into_outcome())
    }

    /// Terminate all children, stop the remote server and release GPU state.
    pub fn shutdown(&mut self) {
        if let Some(wid) = self.tab_manager.active_window_id() {
            if let Some(pane) = self.panes.get_mut(&wid) {
                let placeholder = Screen::builder(1, 1).build().expect("1x1 screen");
                self.final_screen = Some(std::mem::replace(&mut pane.handler.screen, placeholder));
            }
        }
        let active = self.tab_manager.active_window_id();
        let mut any_status = None;
        for (wid, mut pane) in self.panes.drain() {
            pane.terminate();
            if any_status.is_none() {
                any_status = pane.exit_status;
            }
            if Some(wid) == active {
                self.exit_status = pane.exit_status;
            }
        }
        if self.exit_status.is_none() {
            self.exit_status = any_status;
        }
        if let Some(mut server) = self.remote_server.take() {
            server.shutdown();
        }
        let _ = self.surface.make_current();
        let _ = self.backend.take();
    }

    fn into_outcome(mut self) -> RunOutcome {
        let errors = self.final_errors;
        let placeholder = Screen::builder(1, 1).build().expect("1x1 screen");
        let screen = self
            .final_screen
            .take()
            .unwrap_or_else(|| std::mem::replace(&mut self.fallback_screen, placeholder));
        RunOutcome {
            exit: self.exit_status.unwrap_or(ExitStatus::Unknown(-1)),
            timed_out: self.timed_out,
            screen,
            output_bytes: self.output_bytes,
            errors,
        }
    }

    // {{{ PTY I/O

    fn read_all(&mut self) -> io::Result<()> {
        let mut had_output = false;
        let mut titles = Vec::new();
        for (wid, pane) in self.panes.iter_mut() {
            let count = pane.read()?;
            if count > 0 {
                had_output = true;
                self.output_bytes += count as u64;
            }
            if let Some(title) = pane.tracker.title.take() {
                let title = sanitize_title(&title);
                let title = if title.is_empty() {
                    pane.default_title.clone()
                } else {
                    title
                };
                if pane.last_title.as_deref() != Some(title.as_str()) {
                    pane.last_title = Some(title.clone());
                    titles.push((*wid, title));
                }
            }
        }
        if had_output {
            self.need_redraw = true;
        }
        let active = self.tab_manager.active_window_id();
        let mut active_changed = false;
        for (wid, title) in titles {
            if let Some(tab_idx) = self.tab_index_for_window(wid) {
                if let Some(tab) = self.tab_manager.tab_mut(tab_idx) {
                    if let Some(window) = tab.windows_mut().window_mut(wid) {
                        window.title = title;
                        active_changed |= Some(wid) == active;
                    }
                }
            }
        }
        if active_changed {
            self.update_window_title();
        }
        Ok(())
    }

    fn flush_all(&mut self) -> io::Result<()> {
        for pane in self.panes.values_mut() {
            pane.flush()?;
        }
        Ok(())
    }

    fn check_children(&mut self) -> io::Result<()> {
        let mut closed = Vec::new();
        for (wid, pane) in self.panes.iter_mut() {
            pane.check()?;
            if let Some(at) = pane.exited_at {
                if at.elapsed() >= Duration::from_millis(200) {
                    closed.push(*wid);
                }
            }
        }
        for wid in closed {
            self.close_window_id(wid);
        }
        Ok(())
    }

    // }}}
    // {{{ Window events

    fn handle_window_event(&mut self, event: WindowEvent) -> io::Result<()> {
        match event {
            WindowEvent::CloseRequested => self.should_close = true,
            WindowEvent::Resize { .. } => self.handle_resize()?,
            WindowEvent::Expose { .. } => self.need_redraw = true,
            WindowEvent::Focus { focused } => {
                self.need_redraw = true;
                if self
                    .active_pane()
                    .map(|pane| pane.screen().modes.contains(Modes::FOCUS_TRACKING))
                    .unwrap_or(false)
                {
                    self.write_child(if focused { b"\x1b[I" } else { b"\x1b[O" });
                }
            }
            WindowEvent::Key(key) => self.handle_key(&key),
            WindowEvent::MouseButton {
                button,
                pressed,
                x,
                y,
                modifiers,
            } => self.handle_mouse_button(button, pressed, x, y, modifiers),
            WindowEvent::MouseMove { x, y, modifiers } => self.handle_mouse_move(x, y, modifiers),
            WindowEvent::Scroll {
                x,
                y,
                delta_y,
                modifiers,
                ..
            } => self.handle_scroll(x, y, delta_y, modifiers),
            WindowEvent::Leave { .. } => {
                self.mouse_down = false;
                self.hover_cell = None;
                self.window
                    .set_pointer_shape(kitty_window::PointerShape::Default);
            }
            WindowEvent::SelectionNotify { .. } | WindowEvent::SelectionRequest { .. } => {}
            WindowEvent::Wakeup
            | WindowEvent::Move { .. }
            | WindowEvent::Enter { .. }
            | WindowEvent::SelectionClear { .. } => {}
            _ => {}
        }
        Ok(())
    }

    /// Current tab-bar strip height in device pixels. Zero when the style is
    /// hidden or when fewer than `tab_bar_min_tabs` tabs exist.
    fn tab_bar_height_px(&self) -> f32 {
        self.config_options.tab_bar_height(
            self.tab_manager.tabs().len(),
            self.metrics.cell_height.max(1.0),
        )
    }

    fn tab_bar_top_px(&self) -> f32 {
        if self.tab_bar_px > 0.0 && self.config_options.tab_bar_edge.is_top() {
            self.tab_bar_px
        } else {
            0.0
        }
    }

    fn tab_bar_bottom_px(&self) -> f32 {
        if self.tab_bar_px > 0.0 && self.config_options.tab_bar_edge.is_bottom() {
            self.tab_bar_px
        } else {
            0.0
        }
    }

    fn in_tab_bar(&self, y: f64) -> bool {
        if self.tab_bar_px <= 0.0 {
            return false;
        }
        match self.config_options.tab_bar_edge {
            kitty_config::TabBarEdge::Top => y < self.tab_bar_px as f64,
            kitty_config::TabBarEdge::Bottom => {
                y >= (self.pixel_size.1 as f32 - self.tab_bar_px) as f64
            }
            _ => false,
        }
    }

    fn tab_bar_origin_y(&self) -> f32 {
        match self.config_options.tab_bar_edge {
            kitty_config::TabBarEdge::Top => 0.0,
            _ => (self.pixel_size.1 as f32 - self.tab_bar_px).max(0.0),
        }
    }

    /// React to tab-bar visibility changes (new tab, closed tab) the same way
    /// kitty does: the window size is unchanged and the central grid loses or
    /// gains one row for the bar.
    fn update_tab_bar_chrome(&mut self) {
        let new_height = self.tab_bar_height_px();
        if (new_height - self.tab_bar_px).abs() < 0.5 {
            return;
        }
        self.tab_bar_px = new_height;
        let cell_h = self.metrics.cell_height.max(1.0);
        let usable_h =
            (self.pixel_size.1 as f32 - self.padding.1 - self.padding.3 - new_height).max(cell_h);
        let rows = (usable_h / cell_h).floor().max(1.0) as u32;
        if rows != self.rows {
            self.rows = rows;
            self.relayout_all();
        }
        self.renderer.invalidate();
        self.need_redraw = true;
    }

    fn handle_resize(&mut self) -> io::Result<()> {
        self.surface
            .make_current()
            .map_err(|error| io_err(error.to_string()))?;
        let (width, height) = self.surface.refresh_size();
        let cell_w = self.metrics.cell_width.max(1.0);
        let cell_h = self.metrics.cell_height.max(1.0);
        self.tab_bar_px = self.tab_bar_height_px();
        let usable_w = (width as f32 - self.padding.0 - self.padding.2).max(cell_w);
        let usable_h =
            (height as f32 - self.padding.1 - self.padding.3 - self.tab_bar_px).max(cell_h);
        let columns = (usable_w / cell_w).floor().max(1.0) as u32;
        let rows = (usable_h / cell_h).floor().max(1.0) as u32;
        self.pixel_size = (width, height);
        if columns != self.columns || rows != self.rows {
            self.columns = columns;
            self.rows = rows;
            self.relayout_all();
            self.renderer.invalidate();
            self.need_redraw = true;
        }
        if let Some(backend) = self.backend.as_mut() {
            backend
                .set_size(width, height)
                .map_err(|error| io_err(error.to_string()))?;
        }
        Ok(())
    }

    fn handle_key(&mut self, key: &kitty_window::KeyInput) {
        let event = key_event(key);
        if matches!(event.action, KeyAction::Release) {
            self.encode_and_write(&event);
            return;
        }
        let result = self.keymap.dispatch(&event);
        match result {
            MappingResult::Action(action) => {
                let action = action.clone();
                self.execute_action(&action);
            }
            MappingResult::Consumed => {}
            MappingResult::NotConsumed(flush) => {
                for event in flush {
                    self.encode_and_write(&event);
                }
                self.encode_and_write(&event);
            }
        }
    }

    fn encode_and_write(&mut self, event: &KeyEvent) {
        let Some(pane) = self.active_pane_mut() else {
            return;
        };
        let ctx = pane.input_context();
        match input::encode_key_for_screen(event, &ctx) {
            kitty_input::KeyOutput::Bytes(bytes) => pane.write(&bytes),
            kitty_input::KeyOutput::Text(text) => pane.write(text.as_bytes()),
            kitty_input::KeyOutput::None | kitty_input::KeyOutput::Shortcut => {}
        }
    }

    fn execute_action(&mut self, action: &str) {
        let words: Vec<&str> = action.split_whitespace().collect();
        let Some(name) = words.first().copied() else {
            return;
        };
        match name {
            "new_tab" | "new_os_window" => {
                let _ = self.new_tab();
            }
            "close_tab" => {
                self.close_tab_at(self.tab_manager.active_tab_idx());
            }
            "next_tab" => {
                self.tab_manager.next_tab(1);
                self.need_redraw = true;
                self.update_window_title();
            }
            "previous_tab" => {
                self.tab_manager.next_tab(-1);
                self.need_redraw = true;
                self.update_window_title();
            }
            "new_window" => {
                let location = match words.get(1).copied() {
                    Some("horizontal") => WindowLocation::VSplit,
                    Some("vertical") => WindowLocation::HSplit,
                    Some("left") | Some("before") => WindowLocation::Before,
                    Some("right") | Some("after") => WindowLocation::After,
                    _ => WindowLocation::Split,
                };
                self.split_active(location);
            }
            "new_window_horizontal" | "vsplit" => self.split_active(WindowLocation::VSplit),
            "new_window_vertical" | "hsplit" => self.split_active(WindowLocation::HSplit),
            "close_window" => {
                if let Some(wid) = self.tab_manager.active_window_id() {
                    self.close_window_id(wid);
                }
            }
            "next_window" => {
                if let Some(tab) = self.tab_manager.active_tab_mut() {
                    tab.next_window(1);
                }
                self.need_redraw = true;
                self.update_window_title();
            }
            "previous_window" => {
                if let Some(tab) = self.tab_manager.active_tab_mut() {
                    tab.next_window(-1);
                }
                self.need_redraw = true;
                self.update_window_title();
            }
            "neighboring_window" | "focus_window" => {
                if let Some(edge) = words.get(1).and_then(|word| edge_from_name(word)) {
                    self.focus_direction(edge);
                }
            }
            "move_window" => {
                let delta = match words.get(1).copied() {
                    Some("left") | Some("up") => -1,
                    Some("right") | Some("down") => 1,
                    Some(value) => value.parse::<i64>().unwrap_or(0),
                    None => 0,
                };
                if delta != 0 {
                    let changed = self
                        .tab_manager
                        .active_tab_mut()
                        .map(|tab| tab.move_window(delta))
                        .unwrap_or(false);
                    if changed {
                        self.relayout_all();
                        self.need_redraw = true;
                    }
                }
            }
            "resize_window" => {
                let (increment, horizontal) = match words.get(1).copied() {
                    Some("wider") => (2.0, true),
                    Some("narrower") => (-2.0, true),
                    Some("taller") => (2.0, false),
                    Some("shorter") => (-2.0, false),
                    _ => (0.0, true),
                };
                if increment != 0.0
                    && self
                        .tab_manager
                        .active_tab_mut()
                        .map(|tab| tab.resize_active_window(increment, horizontal))
                        .unwrap_or(false)
                {
                    self.relayout_all();
                    self.need_redraw = true;
                }
            }
            "next_layout" | "toggle_layout" => {
                if let Some(tab) = self.tab_manager.active_tab_mut() {
                    tab.next_layout(1);
                }
                self.relayout_all();
                self.need_redraw = true;
            }
            "previous_layout" => {
                if let Some(tab) = self.tab_manager.active_tab_mut() {
                    tab.next_layout(-1);
                }
                self.relayout_all();
                self.need_redraw = true;
            }
            "scrollback_up" => {
                if let Some(pane) = self.active_pane_mut() {
                    pane.scrollback_up();
                }
                self.renderer.invalidate();
                self.need_redraw = true;
            }
            "scrollback_down" => {
                if let Some(pane) = self.active_pane_mut() {
                    pane.scrollback_down();
                }
                self.renderer.invalidate();
                self.need_redraw = true;
            }
            "copy_to_clipboard" => self.copy_selection(),
            "paste_from_clipboard" => self.paste_clipboard(),
            "launch_kitten" | "kitten" => {
                if let Some(name) = words.get(1) {
                    let args: Vec<String> = words[2..].iter().map(|x| (*x).to_string()).collect();
                    if let Err(error) = self.launch_kitten(name, &args) {
                        eprintln!("kitr: kitten {name}: {error}");
                    }
                }
            }
            _ => {}
        }
    }

    fn launch_kitten(&mut self, name: &str, args: &[String]) -> io::Result<()> {
        let registry = kitty_kittens::KittenRegistry::discover_default();
        let entry = registry.lookup(name).ok_or_else(|| {
            io_err(format!(
                "unknown kitten {name:?} (searched {} paths)",
                registry.searched_paths().len()
            ))
        })?;
        let mut bootstrap = kitty_kittens::BootstrapEnv::from_process_env();
        bootstrap.window_id = self.tab_manager.active_window_id();
        bootstrap.listen_on = self.listen_env.clone();
        let options = kitty_kittens::LaunchOptions {
            bootstrap,
            ..Default::default()
        };
        let plan = kitty_kittens::build_launch(entry, name, args, &options)
            .map_err(|error| io_err(error.to_string()))?;
        let spec = SpawnSpec {
            argv: plan.argv,
            cwd: Some(plan.cwd),
            env: plan.env.into_iter().collect(),
            title: Some(format!("kitten: {name}")),
        };
        let tab_idx = self.tab_manager.active_tab_idx();
        self.add_pane_at(
            tab_idx,
            spec,
            LayoutAddOptions {
                location: WindowLocation::Split,
                ..Default::default()
            },
        )?;
        Ok(())
    }

    fn split_active(&mut self, location: WindowLocation) {
        let tab_idx = self.tab_manager.active_tab_idx();
        let spec = self.new_tab_spec();
        let _ = self.add_pane_at(
            tab_idx,
            spec,
            LayoutAddOptions {
                location,
                ..Default::default()
            },
        );
        self.update_window_title();
    }

    fn focus_direction(&mut self, edge: Edge) {
        let Some(tab) = self.tab_manager.active_tab_mut() else {
            return;
        };
        tab.neighboring_window(edge);
        self.need_redraw = true;
        self.update_window_title();
    }

    fn copy_selection(&mut self) {
        let Some(pane) = self.active_pane() else {
            return;
        };
        if !pane.screen().has_selection() {
            return;
        }
        let text = pane.screen().selected_text();
        let _ = self
            .window
            .set_clipboard_text(WindowSelection::Clipboard, &text);
        let _ = self.window.set_primary_text(&text);
    }

    fn paste_clipboard(&mut self) {
        let Ok(text) = self
            .window
            .clipboard_text_blocking(WindowSelection::Clipboard, Duration::from_millis(250))
        else {
            return;
        };
        let Some(pane) = self.active_pane_mut() else {
            return;
        };
        if pane.screen().modes.contains(Modes::BRACKETED_PASTE) {
            pane.write(b"\x1b[200~");
            pane.write(text.as_bytes());
            pane.write(b"\x1b[201~");
        } else {
            pane.write(text.as_bytes());
        }
    }

    fn handle_mouse_button(
        &mut self,
        button: kitty_window::MouseButton,
        pressed: bool,
        x: f64,
        y: f64,
        modifiers: kitty_window::Modifiers,
    ) {
        if self.in_tab_bar(y) {
            if pressed {
                if let Some(idx) = self.tab_at_x(x) {
                    match button {
                        kitty_window::MouseButton::Left => {
                            self.select_tab(idx);
                        }
                        kitty_window::MouseButton::Middle => {
                            self.close_tab_at(idx);
                        }
                        _ => {}
                    }
                }
            }
            return;
        }
        if let Some(wid) = self.pane_at(x, y) {
            if Some(wid) != self.tab_manager.active_window_id() {
                if let Some(tab) = self.tab_manager.active_tab_mut() {
                    tab.set_active_window(wid);
                }
                self.need_redraw = true;
                self.update_window_title();
            }
        }
        let Some(wid) = self.tab_manager.active_window_id() else {
            return;
        };
        let Some((origin_x, origin_y)) = self.pane_origin(self.tab_manager.active_tab_idx(), wid)
        else {
            return;
        };
        let Some(pane) = self.panes.get_mut(&wid) else {
            return;
        };
        let ctx = pane.input_context();
        let mods = mouse_modifiers(modifiers);
        let geometry = input::MouseGeometryState {
            left: 0.0,
            top: 0.0,
            cell_width: self.metrics.cell_width as f64,
            cell_height: self.metrics.cell_height as f64,
            columns: pane.columns,
            lines: pane.rows,
        };
        let Some(position) = geometry.position(x - origin_x as f64, y - origin_y as f64) else {
            return;
        };
        if input::mouse_goes_to_child(&ctx, mods) {
            let kbutton = mouse_button(button);
            let action = if pressed {
                MouseAction::Press
            } else {
                MouseAction::Release
            };
            if let Some(bytes) = send_mouse(&ctx, &position, kbutton, action, mods) {
                pane.write(&bytes);
            }
            self.mouse_down = pressed;
            return;
        }
        if button != kitty_window::MouseButton::Left {
            return;
        }
        if pressed {
            self.mouse_down = true;
            let cell = (position.cell_x, position.cell_y);
            let clicks = match self.last_click {
                Some((at, previous, count))
                    if at.elapsed() < Duration::from_millis(400) && previous == cell =>
                {
                    (count % 3) + 1
                }
                _ => 1,
            };
            self.last_click = Some((Instant::now(), cell, clicks));
            if clicks == 1 {
                pane.handler
                    .screen
                    .start_selection(position.cell_x, position.cell_y);
            } else {
                let mode = if clicks == 2 {
                    SelectionMode::Word
                } else {
                    SelectionMode::Line
                };
                pane.handler.screen.start_selection_mode(
                    position.cell_x,
                    position.cell_y,
                    mode,
                    false,
                    true,
                );
            }
        } else {
            self.mouse_down = false;
            pane.handler
                .screen
                .update_selection_ext(position.cell_x, position.cell_y, true, true);
            if pane.screen().has_selection() {
                let text = pane.screen().selected_text();
                let _ = self.window.set_primary_text(&text);
            }
            self.need_redraw = true;
        }
    }

    fn handle_mouse_move(&mut self, x: f64, y: f64, modifiers: kitty_window::Modifiers) {
        if self.in_tab_bar(y) {
            let shape = if self.tab_at_x(x).is_some() {
                kitty_window::PointerShape::Pointer
            } else {
                kitty_window::PointerShape::Default
            };
            self.window.set_pointer_shape(shape);
            return;
        }
        self.window
            .set_pointer_shape(if self.pane_at(x, y).is_some() {
                kitty_window::PointerShape::Text
            } else {
                kitty_window::PointerShape::Default
            });
        let Some(wid) = self.tab_manager.active_window_id() else {
            return;
        };
        let Some((origin_x, origin_y)) = self.pane_origin(self.tab_manager.active_tab_idx(), wid)
        else {
            return;
        };
        let Some(pane) = self.panes.get_mut(&wid) else {
            return;
        };
        let ctx = pane.input_context();
        let mods = mouse_modifiers(modifiers);
        let geometry = input::MouseGeometryState {
            left: 0.0,
            top: 0.0,
            cell_width: self.metrics.cell_width as f64,
            cell_height: self.metrics.cell_height as f64,
            columns: pane.columns,
            lines: pane.rows,
        };
        let Some(position) = geometry.position(x - origin_x as f64, y - origin_y as f64) else {
            return;
        };
        if input::mouse_goes_to_child(&ctx, mods) {
            let action = if self.mouse_down {
                MouseAction::Drag
            } else {
                MouseAction::Move
            };
            if let Some(bytes) = send_mouse(&ctx, &position, None, action, mods) {
                pane.write(&bytes);
            }
            return;
        }
        if self.mouse_down && pane.screen().selections().in_progress {
            pane.handler
                .screen
                .update_selection(position.cell_x, position.cell_y);
            self.need_redraw = true;
        }
        self.hover_cell = Some((position.cell_x, position.cell_y));
    }

    fn handle_scroll(&mut self, x: f64, y: f64, delta_y: f64, modifiers: kitty_window::Modifiers) {
        if self.in_tab_bar(y) {
            return;
        }
        let Some(wid) = self.tab_manager.active_window_id() else {
            return;
        };
        let Some((origin_x, origin_y)) = self.pane_origin(self.tab_manager.active_tab_idx(), wid)
        else {
            return;
        };
        let Some(pane) = self.panes.get_mut(&wid) else {
            return;
        };
        let ctx = pane.input_context();
        let mods = mouse_modifiers(modifiers);
        let geometry = input::MouseGeometryState {
            left: 0.0,
            top: 0.0,
            cell_width: self.metrics.cell_width as f64,
            cell_height: self.metrics.cell_height as f64,
            columns: pane.columns,
            lines: pane.rows,
        };
        let Some(position) = geometry.position(x - origin_x as f64, y - origin_y as f64) else {
            return;
        };
        if input::mouse_goes_to_child(&ctx, mods) {
            let button = if delta_y > 0.0 {
                kitty_input::mouse::MouseButton::WheelUp
            } else {
                kitty_input::mouse::MouseButton::WheelDown
            };
            if let Some(bytes) = kitty_input::mouse::encode_mouse_scroll(
                &position,
                ctx.modes().mouse_tracking_protocol,
                button,
                mods,
            ) {
                pane.write(&input::with_csi(bytes));
            }
        } else if delta_y > 0.0 {
            pane.scrollback_up();
            self.need_redraw = true;
        } else {
            pane.scrollback_down();
            self.need_redraw = true;
        }
    }

    fn pane_at(&self, x: f64, y: f64) -> Option<WindowId> {
        let tab_idx = self.tab_manager.active_tab_idx();
        let tab = self.tab_manager.tab(tab_idx)?;
        for wid in tab.windows().all_window_ids() {
            let Some(window) = tab.windows().window(*wid) else {
                continue;
            };
            if !window.is_visible_in_layout {
                continue;
            }
            let Some((x0, y0, width, height)) = self.pane_pixel_rect(tab_idx, *wid) else {
                continue;
            };
            let x0 = x0 as f64;
            let y0 = y0 as f64;
            let x1 = x0 + width as f64;
            let y1 = y0 + height as f64;
            if x >= x0 && x < x1 && y >= y0 && y < y1 {
                return Some(*wid);
            }
        }
        None
    }

    fn tab_at_x(&self, x: f64) -> Option<usize> {
        // `tab_ranges` are cell columns; pointer coordinates are device pixels.
        let cell_width = self.metrics.cell_width.max(1.0) as f64;
        let x = (x.max(0.0) / cell_width).floor() as u32;
        self.tab_ranges
            .iter()
            .find(|(_, start, end)| x >= *start && x < *end)
            .map(|(idx, _, _)| *idx)
    }

    // }}}
    // {{{ Rendering

    /// Split borders and background gaps in window pixel coordinates.
    ///
    /// The UI layout currently works in whole cells, so borders are derived
    /// from the pane rectangles after layout rather than being reserved inside
    /// the cell grid. Minimal borders only appear on edges shared by two
    /// panes; full borders appear when `draw_minimal_borders` is off, margins
    /// are configured, or a single window explicitly asks for them.
    fn pane_chrome(&self, tab_idx: usize) -> (Vec<SolidRect>, Vec<SolidRect>) {
        let opts = &self.config_options;
        if opts.window_border_width <= 0.0 {
            return (Vec::new(), Vec::new());
        }
        let Some(tab) = self.tab_manager.tab(tab_idx) else {
            return (Vec::new(), Vec::new());
        };
        let active_id = self.tab_manager.active_window_id();
        let mut panes = Vec::new();
        for wid in tab.windows().all_window_ids() {
            let Some(window) = tab.windows().window(*wid) else {
                continue;
            };
            if !window.is_visible_in_layout {
                continue;
            }
            let Some((x, y, width, height)) = self.pane_pixel_rect(tab_idx, *wid) else {
                continue;
            };
            let rect = kitty_render::Rect::new(x, y, width, height);
            panes.push(PaneBorderPane {
                rect,
                active: Some(*wid) == active_id && !window.needs_attention,
                window_id: *wid as i64,
            });
        }
        if panes.is_empty() {
            return (Vec::new(), Vec::new());
        }
        let single = panes.len() == 1;
        if single && !opts.draw_window_borders_for_single_window {
            return (Vec::new(), Vec::new());
        }
        let margin = opts
            .window_margin_width
            .left
            .max(opts.window_margin_width.top)
            .max(
                opts.window_margin_width
                    .right
                    .max(opts.window_margin_width.bottom),
            );
        let draw_all = !opts.draw_minimal_borders || margin > 0.0 || single;
        let scale = self.window.scale_factor();
        let thickness = (opts.window_border_width * scale).round().max(1.0) as f32;
        let inactive = rgba_of(opts.inactive_border_color);
        let active = opts.active_border_color.map(rgba_of).unwrap_or(inactive);
        let bell = rgba_of(opts.bell_border_color);
        let mut borders = Vec::new();
        for line in pane_border_rects(&panes, thickness, draw_all) {
            let color = if line.active { active } else { inactive };
            let color = if !line.active {
                tab.windows()
                    .window(line.window_id.unsigned_abs() as WindowId)
                    .filter(|window| window.needs_attention)
                    .map(|_| bell)
                    .unwrap_or(color)
            } else {
                color
            };
            borders.push(SolidRect {
                rect: line.rect,
                color,
            });
        }
        (Vec::new(), borders)
    }

    fn build_tab_bar(&self) -> (Screen, Vec<(usize, u32, u32)>) {
        let columns = self.columns.max(1);
        let mut screen = Screen::builder(columns, 1)
            .scrollback(1)
            .build()
            .expect("1-row screen");
        screen.cursor.shape = CursorShape::NoCursor;
        let opts = &self.config_options;
        let bar_bg = opts
            .tab_bar_background
            .map(rgba_of)
            .unwrap_or_else(|| self.resolver.default_bg());
        let active_bg = rgba_of(opts.active_tab_background);
        let active_fg = rgba_of(opts.active_tab_foreground);
        let inactive_bg = rgba_of(opts.inactive_tab_background);
        let inactive_fg = rgba_of(opts.inactive_tab_foreground);
        let tab_bg = |active: bool| if active { active_bg } else { inactive_bg };
        let tab_fg = |active: bool| if active { active_fg } else { inactive_fg };

        // Paint the whole strip with the tab-bar background first; every tab
        // then overwrites its own cells, like TabBar.update_horizontal().
        screen.cursor_position(1, 1);
        screen.cursor.sgr.bg = pack_sgr(bar_bg);
        screen.cursor.sgr.fg = pack_sgr(inactive_fg);
        screen.cursor.sgr.bold = false;
        screen.cursor.sgr.italic = false;
        let fill: Vec<u32> = std::iter::repeat_n(b' ' as u32, columns as usize).collect();
        screen.draw_text(&fill);

        let tabs = self.tab_manager.tabs();
        let count = tabs.len();
        if count == 0 {
            return (screen, Vec::new());
        }
        let titles: Vec<String> = tabs
            .iter()
            .map(|tab| tab.effective_title().to_string())
            .collect();
        let style = opts.tab_bar_style;
        let default_max = ((columns as usize / count.max(1)).saturating_sub(1)).max(1);
        let ideal: Vec<usize> = titles
            .iter()
            .map(|title| tab_ideal_width(style, title, opts))
            .collect();
        let widths: Vec<usize> = ideal
            .iter()
            .map(|ideal| (*ideal).min(default_max.max(1)))
            .collect();

        let mut ranges = Vec::new();
        let mut x = 0u32;
        for (idx, title) in titles.iter().enumerate() {
            if x >= columns {
                break;
            }
            let active = idx == self.tab_manager.active_tab_idx();
            let is_last = idx + 1 == count;
            let next_active = !is_last && idx + 1 == self.tab_manager.active_tab_idx();
            let max_len = widths[idx] as u32;
            let start = x;
            screen.cursor_position(1, x + 1);
            match style {
                TabBarStyle::Hidden => {}
                TabBarStyle::Powerline => {
                    draw_tab_powerline(
                        &mut screen,
                        title,
                        x,
                        max_len,
                        active,
                        next_active,
                        tab_bg,
                        tab_fg,
                        bar_bg,
                        inactive_fg,
                        opts.tab_powerline_style,
                        &opts.active_tab_font_style,
                        &opts.inactive_tab_font_style,
                    );
                }
                TabBarStyle::Separator => {
                    draw_tab_separator(
                        &mut screen,
                        title,
                        x,
                        max_len,
                        is_last,
                        active,
                        tab_bg,
                        tab_fg,
                        inactive_bg,
                        bar_fg_for_tabs(opts),
                        &opts.tab_separator,
                        &opts.active_tab_font_style,
                        &opts.inactive_tab_font_style,
                    );
                }
                TabBarStyle::Slant => {
                    draw_tab_slant(
                        &mut screen,
                        title,
                        x,
                        max_len,
                        active,
                        tab_bg,
                        tab_fg,
                        bar_bg,
                        &opts.active_tab_font_style,
                        &opts.inactive_tab_font_style,
                    );
                }
                TabBarStyle::Fade | TabBarStyle::Custom => {
                    draw_tab_fade(
                        &mut screen,
                        title,
                        x,
                        max_len,
                        active,
                        tab_bg,
                        tab_fg,
                        bar_bg,
                        &opts.active_tab_font_style,
                        &opts.inactive_tab_font_style,
                    );
                }
            }
            let end = screen.cursor.x.min(columns);
            if end > start {
                ranges.push((idx, start, end));
            }
            x = end.max(start);
        }
        screen.cursor.shape = CursorShape::NoCursor;
        (screen, ranges)
    }

    fn render(&mut self) -> io::Result<()> {
        self.surface
            .make_current()
            .map_err(|error| io_err(error.to_string()))?;

        let cell_w = self.metrics.cell_width.max(1.0);
        let cell_h = self.metrics.cell_height.max(1.0);
        {
            let backend = self
                .backend
                .as_mut()
                .ok_or_else(|| io_err("OpenGL backend is shut down"))?;
            backend
                .clear(self.resolver.default_bg())
                .map_err(|error| io_err(error.to_string()))?;
        }

        // Tab strip. Hidden style and the single-tab case reserve no space and
        // draw nothing, matching kitty's tab bar visibility rules.
        let tab_idx = self.tab_manager.active_tab_idx();
        if self.tab_bar_px > 0.0 {
            let (mut bar_screen, ranges) = self.build_tab_bar();
            self.tab_ranges = ranges;
            self.renderer.generator.config.origin_x = 0.0;
            self.renderer.generator.config.origin_y = self.tab_bar_origin_y();
            self.renderer.generator.config.viewport_width = self.pixel_size.0 as f32;
            self.renderer.generator.config.viewport_height = self.tab_bar_px;
            let mut bar_frame = {
                let font_metrics = self.fonts.cell_metrics();
                let mut rasterizer = FontRasterizer {
                    fonts: &mut self.fonts,
                    cache: &mut self.raster_cache,
                    metrics: font_metrics,
                };
                self.renderer
                    .prepare_frame(&bar_screen, &mut rasterizer, &self.resolver)
            };
            force_full_rows(&mut bar_frame, 1);
            let bar_config = self.renderer.generator.config.clone();
            draw_frame(&mut self.backend, &self.renderer, &bar_frame, &bar_config)?;
            self.renderer.finish_frame(&mut bar_screen);
        } else {
            self.tab_ranges.clear();
        }

        // Panes, each with its own origin/viewport.
        let ids = self
            .tab_manager
            .tab(tab_idx)
            .map(|tab| tab.windows().all_window_ids().to_vec())
            .unwrap_or_default();
        let active = self.tab_manager.active_window_id();
        for wid in ids {
            let visible = self
                .tab_manager
                .tab(tab_idx)
                .and_then(|tab| tab.windows().window(wid))
                .map(|window| window.is_visible_in_layout)
                .unwrap_or(false);
            if !visible {
                continue;
            }
            let Some((columns, rows, _, _)) = self.pane_geometry(tab_idx, wid) else {
                continue;
            };
            let Some((origin_x, origin_y)) = self.pane_origin(tab_idx, wid) else {
                continue;
            };
            self.renderer.generator.config.origin_x = origin_x;
            self.renderer.generator.config.origin_y = origin_y;
            self.renderer.generator.config.viewport_width = columns as f32 * cell_w;
            self.renderer.generator.config.viewport_height = rows as f32 * cell_h;
            let Some(pane) = self.panes.get_mut(&wid) else {
                continue;
            };
            self.renderer.generator.config.selections = pane
                .handler
                .screen
                .selections()
                .items()
                .iter()
                .map(SelectionRange::from_selection)
                .collect();
            self.renderer.generator.config.hyperlink_hover = self
                .hover_cell
                .filter(|(hx, hy)| {
                    *hx < pane.handler.screen.columns && *hy < pane.handler.screen.lines
                })
                .map(|(x, y)| kitty_render::HyperlinkHover { x, y });
            let saved_cursor = pane.handler.screen.cursor.shape;
            if Some(wid) != active {
                pane.handler.screen.cursor.shape = CursorShape::NoCursor;
            }
            let mut frame = {
                let font_metrics = self.fonts.cell_metrics();
                let mut rasterizer = FontRasterizer {
                    fonts: &mut self.fonts,
                    cache: &mut self.raster_cache,
                    metrics: font_metrics,
                };
                self.renderer
                    .prepare_frame(&pane.handler.screen, &mut rasterizer, &self.resolver)
            };
            pane.handler.screen.cursor.shape = saved_cursor;
            let lines = pane.handler.screen.lines;
            force_full_rows(&mut frame, lines);
            let config = self.renderer.generator.config.clone();
            draw_frame(&mut self.backend, &self.renderer, &frame, &config)?;
            self.renderer.finish_frame(&mut pane.handler.screen);
        }

        // Split borders are drawn last as a transparent overlay so they are
        // not erased by the per-pane scissor clears.
        let (gaps, borders) = self.pane_chrome(tab_idx);
        if !gaps.is_empty() || !borders.is_empty() {
            let mut chrome = Frame {
                default_fg: self.resolver.default_fg(),
                default_bg: self.resolver.default_bg(),
                skip_clear: true,
                ..Frame::default()
            };
            chrome.damage.full_redraw = true;
            for gap in gaps {
                chrome.backgrounds.push(kitty_render::BackgroundInstance {
                    rect: gap.rect,
                    cell_x: 0,
                    cell_y: 0,
                    color: gap.color,
                    is_default_bg: false,
                    flags: InstanceFlags::default(),
                });
            }
            for border in borders {
                chrome.decorations.push(DecorationInstance {
                    rect: border.rect,
                    cell_x: 0,
                    cell_y: 0,
                    kind: DecorationKind::PaneBorder,
                    color: border.color,
                    flags: InstanceFlags::default(),
                });
            }
            let chrome_config = RenderConfig {
                metrics: self.metrics,
                origin_x: 0.0,
                origin_y: 0.0,
                viewport_width: self.pixel_size.0 as f32,
                viewport_height: self.pixel_size.1 as f32,
                ..RenderConfig::default()
            };
            draw_frame(&mut self.backend, &self.renderer, &chrome, &chrome_config)?;
        }

        self.surface
            .swap_buffers()
            .map_err(|error| io_err(error.to_string()))?;
        self.frame_count += 1;
        self.need_redraw = false;
        Ok(())
    }

    // }}}
    // {{{ Remote control

    fn process_remote_jobs(&mut self) -> io::Result<()> {
        loop {
            let job = {
                let Some(rx) = self.remote_rx.as_ref() else {
                    return Ok(());
                };
                match rx.try_recv() {
                    Ok(job) => job,
                    Err(std::sync::mpsc::TryRecvError::Empty) => return Ok(()),
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        self.remote_rx = None;
                        return Ok(());
                    }
                }
            };
            match job {
                RemoteJob::GetText(request, reply) => {
                    let _ = reply.send(self.handle_get_text(request));
                }
                RemoteJob::SendText(request, reply) => {
                    let _ = reply.send(self.handle_send_text(request));
                }
                RemoteJob::SendKey(request, reply) => {
                    let _ = reply.send(self.handle_send_key(request));
                }
                RemoteJob::ResizeWindow(request, reply) => {
                    let _ = reply.send(self.handle_resize_window(request));
                }
                RemoteJob::CloseWindow(request, reply) => {
                    let _ = reply.send(self.handle_close_window(request));
                }
                RemoteJob::CloseTab(request, reply) => {
                    let _ = reply.send(self.handle_close_tab(request));
                }
                RemoteJob::FocusWindow(request, reply) => {
                    let _ = reply.send(self.handle_focus_window(request));
                }
                RemoteJob::SetWindowTitle(request, reply) => {
                    let _ = reply.send(self.handle_set_window_title(request));
                }
                RemoteJob::GetColors(reply) => {
                    let _ = reply.send(Ok(RemoteValue::String(self.resolver.as_kitty_conf())));
                }
                RemoteJob::SetColors(request, reply) => {
                    let _ = reply.send(self.handle_set_colors(request));
                }
                RemoteJob::Screenshot(request, reply) => {
                    let _ = reply.send(self.handle_screenshot(request));
                }
            }
        }
    }

    fn resolve_window_for_match(&self, spec: Option<&str>) -> Option<(usize, WindowId)> {
        let spec = spec?;
        if let Some(id) = spec.strip_prefix("id:") {
            let id = id.parse::<WindowId>().ok()?;
            return self.tab_index_for_window(id).map(|tab_idx| (tab_idx, id));
        }
        let needle = spec.strip_prefix("title:").unwrap_or(spec);
        for (tab_idx, tab) in self.tab_manager.tabs().iter().enumerate() {
            for wid in tab.windows().all_window_ids() {
                if let Some(window) = tab.windows().window(*wid) {
                    if window.effective_title().contains(needle) {
                        return Some((tab_idx, *wid));
                    }
                }
            }
        }
        None
    }

    fn handle_get_text(&mut self, request: GetTextRequest) -> CommandResult {
        let tab_idx = self.tab_manager.active_tab_idx();
        let wid =
            if let Some((_, wid)) = self.resolve_window_for_match(request.match_spec.as_deref()) {
                wid
            } else if request.match_spec.is_some() {
                return Err(CommandError::new("No matching window found"));
            } else {
                self.tab_manager
                    .active_window_id()
                    .ok_or_else(|| CommandError::new("No active window"))?
            };
        let Some(pane) = self.panes.get(&wid) else {
            return Err(CommandError::new("Window has no screen"));
        };
        {
            let screen = pane.screen();
            let text = match request.extent {
                TextExtent::Selection => screen.selected_text(),
                TextExtent::All => {
                    let mut lines = Vec::new();
                    for lnum in (0..screen.historybuf().count).rev() {
                        lines.push(screen.history_line_text(lnum));
                    }
                    for y in 0..screen.lines {
                        lines.push(screen.line_text(y));
                    }
                    while lines.last().is_some_and(|line| line.is_empty()) {
                        lines.pop();
                    }
                    lines.join("\n")
                }
                _ => crate::dump_screen(screen),
            };
            if request.extent == TextExtent::Selection && request.clear_selection {
                if let Some(pane) = self.panes.get_mut(&wid) {
                    pane.handler.screen.clear_selection();
                }
            }
            let _ = tab_idx;
            Ok(RemoteValue::String(text))
        }
    }

    fn target_windows(
        &self,
        match_spec: Option<&str>,
        all: bool,
        exclude_active: bool,
    ) -> Vec<WindowId> {
        let active = self.tab_manager.active_window_id();
        if let Some((_, wid)) = self.resolve_window_for_match(match_spec) {
            return vec![wid];
        }
        if match_spec.is_some() {
            return Vec::new();
        }
        if all {
            let tab_idx = self.tab_manager.active_tab_idx();
            return self
                .tab_manager
                .tab(tab_idx)
                .map(|tab| tab.windows().all_window_ids().to_vec())
                .unwrap_or_default()
                .into_iter()
                .filter(|wid| !exclude_active || Some(*wid) != active)
                .collect();
        }
        active.into_iter().collect()
    }

    fn handle_send_text(&mut self, request: kitty_remote::SendTextRequest) -> CommandResult {
        let targets = self.target_windows(
            request.match_spec.as_deref(),
            request.all,
            request.exclude_active,
        );
        if targets.is_empty() {
            return Err(CommandError::new("No matching window found"));
        }
        let bytes = match request.data {
            SendTextData::Text(text) => text.into_bytes(),
            SendTextData::Base64(bytes) => bytes,
            SendTextData::KittyKey(text) => {
                return Err(CommandError::new(format!(
                    "kitty-key send-text is not supported: {text}"
                )))
            }
            SendTextData::Session(_) => {
                return Err(CommandError::new(
                    "session broadcast send-text is not supported",
                ))
            }
        };
        for wid in targets {
            if let Some(pane) = self.panes.get_mut(&wid) {
                let wrap = match request.bracketed_paste {
                    kitty_remote::BracketedPaste::Disable => false,
                    kitty_remote::BracketedPaste::Enable => true,
                    kitty_remote::BracketedPaste::Auto => {
                        pane.screen().modes.contains(Modes::BRACKETED_PASTE)
                    }
                };
                if wrap {
                    pane.write(b"\x1b[200~");
                    pane.write(&bytes);
                    pane.write(b"\x1b[201~");
                } else {
                    pane.write(&bytes);
                }
            }
        }
        Ok(RemoteValue::Null)
    }

    fn handle_send_key(&mut self, request: kitty_remote::SendKeyRequest) -> CommandResult {
        let targets = self.target_windows(
            request.match_spec.as_deref(),
            request.all,
            request.exclude_active,
        );
        if targets.is_empty() {
            return Err(CommandError::new("No matching window found"));
        }
        let mut chunks = Vec::new();
        for key in &request.keys {
            let shortcut = kitty_input::parse_shortcut(key).ok_or_else(|| {
                CommandError::new(format!("Invalid key name for send-key: {key}"))
            })?;
            let no_text_mods = shortcut.mods.bits()
                & (Modifiers::CTRL.bits() | Modifiers::ALT.bits() | Modifiers::SUPER.bits())
                == 0;
            let text = if no_text_mods {
                char::from_u32(shortcut.key).map(|ch| ch.to_string())
            } else {
                None
            };
            let event = KeyEvent {
                key: shortcut.key,
                shifted_key: 0,
                alternate_key: 0,
                native_key: 0,
                modifiers: shortcut.mods,
                action: KeyAction::Press,
                text,
            };
            let Some((_, wid)) = targets.iter().map(|wid| (0usize, *wid)).next() else {
                continue;
            };
            let Some(pane) = self.panes.get(&wid) else {
                continue;
            };
            let ctx = pane.input_context();
            match input::encode_key_for_screen(&event, &ctx) {
                kitty_input::KeyOutput::Bytes(bytes) => chunks.push(bytes),
                kitty_input::KeyOutput::Text(text) => chunks.push(text.into_bytes()),
                kitty_input::KeyOutput::None | kitty_input::KeyOutput::Shortcut => {}
            }
        }
        for wid in targets {
            if let Some(pane) = self.panes.get_mut(&wid) {
                for chunk in &chunks {
                    pane.write(chunk);
                }
            }
        }
        Ok(RemoteValue::Null)
    }

    fn handle_resize_window(
        &mut self,
        request: kitty_remote::ResizeWindowRequest,
    ) -> CommandResult {
        if let Some((tab_idx, wid)) = self.resolve_window_for_match(request.match_spec.as_deref()) {
            self.tab_manager.set_active_tab_idx(tab_idx);
            if let Some(tab) = self.tab_manager.tab_mut(tab_idx) {
                tab.set_active_window(wid);
            }
        }
        let tab_idx = self.tab_manager.active_tab_idx();
        let Some(tab) = self.tab_manager.tab_mut(tab_idx) else {
            return Err(CommandError::new("No active tab"));
        };
        match request.axis {
            kitty_remote::ResizeAxis::Reset => {
                tab.layouts_mut().remove_all_biases();
                tab.apply_layout();
            }
            kitty_remote::ResizeAxis::Horizontal => {
                if !tab.resize_active_window(request.increment as f64, true) {
                    return Err(CommandError::new("Cannot resize this window horizontally"));
                }
            }
            kitty_remote::ResizeAxis::Vertical => {
                if !tab.resize_active_window(request.increment as f64, false) {
                    return Err(CommandError::new("Cannot resize this window vertically"));
                }
            }
        }
        self.relayout_all();
        self.need_redraw = true;
        Ok(RemoteValue::Null)
    }

    fn handle_close_window(&mut self, request: kitty_remote::CloseWindowRequest) -> CommandResult {
        let wid =
            if let Some((_, wid)) = self.resolve_window_for_match(request.match_spec.as_deref()) {
                wid
            } else if request.match_spec.is_some() {
                if request.ignore_no_match {
                    return Ok(RemoteValue::Null);
                }
                return Err(CommandError::new("No matching window found"));
            } else {
                match self.tab_manager.active_window_id() {
                    Some(wid) => wid,
                    None => {
                        if request.ignore_no_match {
                            return Ok(RemoteValue::Null);
                        }
                        return Err(CommandError::new("No active window"));
                    }
                }
            };
        self.close_window_id(wid);
        Ok(RemoteValue::Null)
    }

    fn handle_close_tab(&mut self, request: kitty_remote::CloseTabRequest) -> CommandResult {
        let idx = if let Some(spec) = request.match_spec.as_deref() {
            let needle = spec.strip_prefix("title:").unwrap_or(spec);
            self.tab_manager
                .tabs()
                .iter()
                .position(|tab| tab.effective_title().contains(needle))
        } else {
            Some(self.tab_manager.active_tab_idx())
        };
        let Some(idx) = idx else {
            if request.ignore_no_match {
                return Ok(RemoteValue::Null);
            }
            return Err(CommandError::new("No matching tab found"));
        };
        self.close_tab_at(idx);
        Ok(RemoteValue::Null)
    }

    fn handle_focus_window(&mut self, request: kitty_remote::FocusWindowRequest) -> CommandResult {
        let Some((tab_idx, wid)) = self.resolve_window_for_match(request.match_spec.as_deref())
        else {
            return Err(CommandError::new("No matching window found"));
        };
        self.tab_manager.set_active_tab_idx(tab_idx);
        if let Some(tab) = self.tab_manager.tab_mut(tab_idx) {
            tab.set_active_window(wid);
        }
        self.need_redraw = true;
        self.update_window_title();
        Ok(RemoteValue::Null)
    }

    fn handle_set_window_title(
        &mut self,
        request: kitty_remote::SetWindowTitleRequest,
    ) -> CommandResult {
        let (tab_idx, wid) = if let Some((tab_idx, wid)) =
            self.resolve_window_for_match(request.match_spec.as_deref())
        {
            (tab_idx, wid)
        } else if request.match_spec.is_some() {
            return Err(CommandError::new("No matching window found"));
        } else {
            let Some(wid) = self.tab_manager.active_window_id() else {
                return Err(CommandError::new("No active window"));
            };
            (self.tab_manager.active_tab_idx(), wid)
        };
        if let Some(tab) = self.tab_manager.tab_mut(tab_idx) {
            if let Some(window) = tab.windows_mut().window_mut(wid) {
                window.override_title = request.title.clone();
            }
        }
        self.need_redraw = true;
        self.update_window_title();
        Ok(RemoteValue::Null)
    }

    fn handle_set_colors(&mut self, request: kitty_remote::SetColorsRequest) -> CommandResult {
        if request.reset {
            self.resolver = ConfigColorResolver::new(&self.config_options);
        } else if let Some(items) = request.colors.as_object() {
            for (name, value) in items {
                let Some(number) = color_as_u32(value) else {
                    continue;
                };
                let color = Rgba8::rgb(
                    ((number >> 16) & 0xff) as u8,
                    ((number >> 8) & 0xff) as u8,
                    (number & 0xff) as u8,
                );
                if let Some(index) = name
                    .strip_prefix("color")
                    .and_then(|index| index.parse::<u8>().ok())
                {
                    self.resolver.set_palette_color(index, color);
                } else {
                    self.resolver.set_named_color(name, color);
                }
            }
        }
        self.need_redraw = true;
        Ok(RemoteValue::Null)
    }

    fn handle_screenshot(
        &mut self,
        request: kitty_remote::ScreenshotRequest,
    ) -> Result<ScreenshotImage, CommandError> {
        self.render_now()
            .map_err(|error| CommandError::new(error.to_string()))?;
        let (width, height, rgba) = self
            .capture_rgba()
            .map_err(|error| CommandError::new(error.to_string()))?;
        let bytes = match request.format {
            ScreenshotFormat::Png => encode_png(width, height, &rgba),
            ScreenshotFormat::Ppm => encode_ppm(width, height, &rgba),
        };
        if !request.output_path.is_empty() {
            std::fs::write(&request.output_path, &bytes)
                .map_err(|error| CommandError::new(error.to_string()))?;
        }
        Ok(encode_screenshot(bytes, request.format, width, height))
    }

    fn update_window_title(&mut self) {
        let title = self
            .tab_manager
            .active_tab()
            .map(|tab| tab.effective_title())
            .filter(|title| !title.is_empty())
            .unwrap_or_else(|| "kitr".to_string());
        if self.last_title.as_deref() != Some(title.as_str()) {
            let _ = self.window.set_title(&title);
            self.last_title = Some(title);
        }
    }

    // }}}
}

fn edge_from_name(name: &str) -> Option<Edge> {
    match name {
        "left" => Some(Edge::Left),
        "right" => Some(Edge::Right),
        "top" | "up" => Some(Edge::Top),
        "bottom" | "down" => Some(Edge::Bottom),
        _ => None,
    }
}

fn build_keymap(config: &kitty_config::Options) -> KeyMap {
    let mut keys = KeyMap::new(Modifiers(config.kitty_mod.0));
    if !config.clear_all_shortcuts {
        for line in [
            "kitty_mod+t new_tab",
            "kitty_mod+w close_tab",
            "kitty_mod+left previous_tab",
            "kitty_mod+right next_tab",
            "kitty_mod+enter new_window",
            "ctrl+alt+left neighboring_window left",
            "ctrl+alt+right neighboring_window right",
            "ctrl+alt+up neighboring_window up",
            "ctrl+alt+down neighboring_window down",
            "kitty_mod+alt+left neighboring_window left",
            "kitty_mod+alt+right neighboring_window right",
            "kitty_mod+alt+up neighboring_window up",
            "kitty_mod+alt+down neighboring_window down",
            "kitty_mod+alt+shift+left move_window left",
            "kitty_mod+alt+shift+right move_window right",
            "kitty_mod+alt+shift+up move_window up",
            "kitty_mod+alt+shift+down move_window down",
            "kitty_mod+alt+plus resize_window wider",
            "kitty_mod+alt+minus resize_window narrower",
            "kitty_mod+alt+page_up resize_window taller",
            "kitty_mod+alt+page_down resize_window shorter",
            "kitty_mod+space next_layout",
            "kitty_mod+c copy_to_clipboard",
            "kitty_mod+v paste_from_clipboard",
            "shift+page_up scrollback_up",
            "shift+page_down scrollback_down",
        ] {
            if let Err(error) = keys.add_line(line) {
                eprintln!("kitr: default keymap: {error}");
            }
        }
    }
    for line in &config.map {
        if let Err(error) = keys.add_line(line) {
            eprintln!("kitr: map: {error}");
        }
    }
    keys
}

fn load_session_plan(
    options: &Options,
    config: &kitty_config::Options,
) -> io::Result<Option<RestorePlan>> {
    let Some(path) = &options.session else {
        return Ok(None);
    };
    let raw = std::fs::read_to_string(path)
        .map_err(|error| io_err(format!("cannot read session {}: {error}", path.display())))?;
    let shell = if config.shell.is_empty() || config.shell == "." {
        crate::default_shell().to_string_lossy().into_owned()
    } else {
        config.shell.clone()
    };
    let session_options = SessionOptions {
        enabled_layouts: config.enabled_layouts.clone(),
        shell,
    };
    let environ: BTreeMap<String, String> = std::env::vars().collect();
    let path_text = path.to_string_lossy();
    let sessions = parse_sessions(
        &raw,
        &session_options,
        Some(&environ),
        &path_text,
        &path_text,
    )
    .map_err(|error| io_err(format!("session {}: {error}", path.display())))?;
    if sessions.is_empty() {
        return Err(io_err(format!(
            "session {} defines no tabs",
            path.display()
        )));
    }
    if sessions.len() > 1 {
        eprintln!(
            "kitr: session defines {} OS windows; using the first",
            sessions.len()
        );
    }
    Ok(Some(sessions[0].restore_plan()))
}

type RemoteRuntime = (
    Option<Receiver<RemoteJob>>,
    Option<kitty_remote::ServerHandle>,
    Option<String>,
);

fn start_remote(listen_on: Option<&OsStr>) -> io::Result<RemoteRuntime> {
    let Some(listen_on) = listen_on else {
        return Ok((None, None, None));
    };
    let raw = listen_on.to_string_lossy();
    let address = raw.strip_prefix("unix:").unwrap_or(&raw);
    let (bridge, receiver) = RemoteBridge::new();
    let handler: Arc<dyn kitty_remote::RemoteHandler> = Arc::new(bridge);
    let config = ServerConfig::default();
    #[cfg(target_os = "linux")]
    if let Some(name) = address.strip_prefix('@') {
        use std::os::linux::net::SocketAddrExt;
        let addr = std::os::unix::net::SocketAddr::from_abstract_name(name.as_bytes())
            .map_err(|error| io_err(format!("invalid abstract socket name: {error}")))?;
        let server = kitty_remote::RemoteServer::bind_addr(&addr, handler, config)
            .map_err(|error| io_err(format!("cannot listen on unix:@{name}: {error}")))?;
        let handle = server
            .start()
            .map_err(|error| io_err(format!("cannot start remote server: {error}")))?;
        return Ok((Some(receiver), Some(handle), Some(format!("unix:@{name}"))));
    }
    let path = Path::new(address);
    let server = kitty_remote::RemoteServer::bind(path, handler, config)
        .map_err(|error| io_err(format!("cannot listen on {}: {error}", path.display())))?;
    let handle = server
        .start()
        .map_err(|error| io_err(format!("cannot start remote server: {error}")))?;
    Ok((
        Some(receiver),
        Some(handle),
        Some(format!("unix:{}", path.display())),
    ))
}

fn load_config(options: &Options) -> Config {
    let paths: Vec<PathBuf> = match options.config.as_deref() {
        // kitty's `--config NONE` skips config files entirely.
        Some(path) if path == "NONE" => Vec::new(),
        Some(path) => vec![PathBuf::from(path)],
        None => default_config_path().into_iter().collect(),
    };
    Config::load_lenient(&paths, &[] as &[&str])
}

/// The first existing `kitty.conf`, searched in kitty's config-directory
/// order: only `$KITTY_CONFIG_DIRECTORY` when set, otherwise
/// `$XDG_CONFIG_HOME/kitty` (or `~/.config/kitty`), then `$XDG_CONFIG_DIRS`.
fn default_config_path() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("KITTY_CONFIG_DIRECTORY") {
        let path = PathBuf::from(dir).join("kitty.conf");
        return path.is_file().then_some(path);
    }
    let mut dirs = Vec::new();
    if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME").filter(|dir| !dir.is_empty()) {
        dirs.push(PathBuf::from(dir).join("kitty"));
    } else if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(home).join(".config/kitty"));
    }
    let system = std::env::var("XDG_CONFIG_DIRS").unwrap_or_else(|_| "/etc/xdg".to_owned());
    for dir in system.split(':').filter(|dir| !dir.is_empty()) {
        dirs.push(PathBuf::from(dir).join("kitty"));
    }
    dirs.into_iter()
        .map(|dir| dir.join("kitty.conf"))
        .find(|path| path.is_file())
}

fn write_ppm(path: &Path, width: u32, height: u32, rgba: &[u8]) -> io::Result<()> {
    let needed = width as usize * height as usize * 4;
    if rgba.len() < needed {
        return Err(io_err("framebuffer is smaller than the surface"));
    }
    let mut out = io::BufWriter::new(std::fs::File::create(path)?);
    write!(out, "P6\n{width} {height}\n255\n")?;
    let mut row = Vec::with_capacity(width as usize * 3);
    for y in 0..height as usize {
        row.clear();
        for x in 0..width as usize {
            let i = (y * width as usize + x) * 4;
            row.extend_from_slice(&rgba[i..i + 3]);
        }
        out.write_all(&row)?;
    }
    out.flush()
}

/// Run a GUI session using the CLI options, printing a screenshot if asked.
pub fn run_gui(options: &Options, input: Input) -> io::Result<RunOutcome> {
    let session = GuiSession::open(options, input)?;
    let timeout = options
        .timeout
        .or_else(|| options.screenshot.as_ref().map(|_| Duration::from_secs(10)));
    session.run(
        timeout,
        options.screenshot.as_deref(),
        options.screenshot_delay,
    )
}

#[cfg(test)]
mod keymap_tests {
    use super::*;

    #[test]
    fn default_kitty_mod_shortcuts_match() {
        let config = kitty_config::Options::default();
        let mut keys = build_keymap(&config);
        let event = KeyEvent {
            key: b't' as u32,
            shifted_key: 0,
            alternate_key: 0,
            native_key: 0,
            modifiers: Modifiers::CTRL | Modifiers::SHIFT,
            action: KeyAction::Press,
            text: None,
        };
        assert_eq!(
            keys.dispatch(&event),
            MappingResult::Action("new_tab".into())
        );
        let event = KeyEvent {
            key: kitty_input::key_constants::GLFW_FKEY_LEFT,
            shifted_key: 0,
            alternate_key: 0,
            native_key: 0,
            modifiers: Modifiers::CTRL | Modifiers::SHIFT,
            action: KeyAction::Press,
            text: None,
        };
        assert_eq!(
            keys.dispatch(&event),
            MappingResult::Action("previous_tab".into())
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{default_window_title, sanitize_title, tab_title_chars};
    use std::ffi::OsString;

    fn argv(items: &[&str]) -> Vec<OsString> {
        items.iter().map(OsString::from).collect()
    }

    #[test]
    fn default_titles_use_the_program_basename() {
        assert_eq!(default_window_title(&argv(&["/usr/bin/bash"])), "bash");
        assert_eq!(
            default_window_title(&argv(&["/bin/sh", "-c", "true"])),
            "sh"
        );
    }

    #[test]
    fn default_titles_include_the_ssh_host() {
        assert_eq!(
            default_window_title(&argv(&["ssh", "-p", "2222", "user@example.com"])),
            "ssh: user@example.com"
        );
        assert_eq!(
            default_window_title(&argv(&["/usr/bin/mosh", "example.com"])),
            "mosh: example.com"
        );
    }

    #[test]
    fn default_titles_fall_back_to_kitr() {
        assert_eq!(default_window_title(&[]), "kitr");
        assert_eq!(default_window_title(&argv(&[""])), "kitr");
    }

    #[test]
    fn titles_are_collapsed_onto_one_line() {
        assert_eq!(sanitize_title("build\r\nhost"), "build host");
        assert_eq!(sanitize_title("  spaced\tout  "), "spaced out");
    }

    #[test]
    fn tab_titles_truncate_with_an_ellipsis() {
        assert_eq!(
            tab_title_chars("abcdef", 4).into_iter().collect::<String>(),
            "abc…"
        );
        assert_eq!(
            tab_title_chars("abc", 10).into_iter().collect::<String>(),
            "abc"
        );
        assert_eq!(
            tab_title_chars("abc", 0).into_iter().collect::<String>(),
            ""
        );
    }
}
