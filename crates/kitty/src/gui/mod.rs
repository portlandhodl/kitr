// SPDX-License-Identifier: GPL-3.0-or-later
//! X11 + EGL GUI frontend.
//!
//! The window, EGL surface, fonts and input layers live in their own crates;
//! this module owns the event loop that ties them to the terminal screen and
//! the child PTY. Rendering uses `kitty-render`'s GL backend through a
//! caller-provided current EGL surface (see `GlBackend::with_loader`).
//!
//! DPI handling is deliberately coarse: the X11 backend exposes the `Xft.dpi`
//! scale factor, and the configured font size is multiplied by it when the
//! faces are loaded. Window pixels and cell metrics are therefore already
//! scaled; there is no separate per-monitor fractional scaling or text
//! re-layout beyond that.

mod colors;
mod fonts;
mod input;

use std::collections::VecDeque;
use std::io::{self, Write};
use std::path::Path;
use std::time::{Duration, Instant};

use colors::ConfigColorResolver;
use fonts::{FontRasterizer, FontSet};
use input::{key_event, mouse_button, mouse_modifiers, send_mouse, ModeTracker};
use kitty_input::mouse::MouseAction;
use kitty_input::InputContext;

use kitty_config::{Config, CursorShape as ConfigCursorShape};
use kitty_fonts::RasterCache;
use kitty_os::child::{ExitStatus, SpawnConfig};
use kitty_os::pty::WindowSize;
use kitty_os::{loop_utils, ChildProcess};
use kitty_render::{
    AtlasConfig, CellMetrics, GlBackend, GlBackendConfig, GlFns, RenderConfig, RendererCore,
};
use kitty_screen::{CursorShape, Modes, Parser, Screen, ScreenHandler};
use kitty_window::{Selection as WindowSelection, WindowConfig, WindowEvent, X11Window};

use crate::{inherited_env, resolve_command, Input, Options, RunOutcome};

const CELL_FALLBACK_WIDTH: u32 = 8;
const CELL_FALLBACK_HEIGHT: u32 = 16;
const MAX_PENDING_INPUT: usize = 4 * 1024 * 1024;
const IDLE_POLL: Duration = Duration::from_millis(50);
const SCREENSHOT_SETTLE: Duration = Duration::from_millis(150);

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

/// A live GUI terminal session.
///
/// This type is public so integration tests can drive individual frames,
/// inject synthetic window events, resize the native window and capture the
/// framebuffer. The `kitr` binary uses [`run_gui`] instead.
pub struct GuiSession {
    // Keep the GL backend before the surface/window so callers that forget to
    // call `shutdown` still tear down GL resources first. `backend` is an
    // Option so `shutdown` can drop it while the context is current.
    backend: Option<GlBackend>,
    surface: kitty_window::EglWindowSurface,
    window: X11Window,
    renderer: RendererCore,
    fonts: FontSet,
    raster_cache: RasterCache,
    resolver: ConfigColorResolver,
    parser: Parser,
    handler: ScreenHandler,
    tracker: ModeTracker,
    child: ChildProcess,
    write_queue: VecDeque<u8>,
    read_buffer: Vec<u8>,
    padding: (f32, f32, f32, f32),
    metrics: CellMetrics,
    pixel_size: (u32, u32),
    columns: u32,
    rows: u32,
    need_redraw: bool,
    frame_count: u64,
    output_bytes: u64,
    pty_eof: bool,
    exit_status: Option<ExitStatus>,
    exited_at: Option<Instant>,
    timed_out: bool,
    should_close: bool,
    mouse_down: bool,
    last_title: Option<String>,
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

        let title = options.title.clone().unwrap_or_else(|| "kitr".to_string());
        let requested = options.size.unwrap_or((80, 24));
        let provisional_width = requested.0 as u32 * CELL_FALLBACK_WIDTH;
        let provisional_height = requested.1 as u32 * CELL_FALLBACK_HEIGHT;

        let mut window = X11Window::open(
            &WindowConfig::new(
                title.clone(),
                provisional_width.max(1),
                provisional_height.max(1),
            ),
            display_name,
        )
        .map_err(|error| io_err(format!("cannot open X11 window: {error}")))?;
        let scale = window.scale_factor().clamp(0.5, 8.0);

        let fonts = FontSet::load(&cfg, scale)
            .map_err(|error| io_err(format!("cannot load fonts: {error}")))?;
        let metrics: CellMetrics = fonts.cell_metrics().into();
        let cell_w = metrics.cell_width.max(1.0).round() as u32;
        let cell_h = metrics.cell_height.max(1.0).round() as u32;
        let padding = (
            (cfg.window_padding_width.left * scale).round() as f32,
            (cfg.window_padding_width.top * scale).round() as f32,
            (cfg.window_padding_width.right * scale).round() as f32,
            (cfg.window_padding_width.bottom * scale).round() as f32,
        );
        let (columns, rows) = requested;
        let pixel_width = columns as u32 * cell_w + (padding.0 + padding.2).max(0.0) as u32;
        let pixel_height = rows as u32 * cell_h + (padding.1 + padding.3).max(0.0) as u32;
        window.set_size(pixel_width.max(1), pixel_height.max(1));
        window.set_size_limits(cell_w, cell_h, None, None);

        let surface = kitty_window::EglWindowSurface::new(&window)
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
            GlBackendConfig::default(),
        )
        .map_err(|error| io_err(format!("cannot create OpenGL backend: {error}")))?;

        let scrollback = if options.scrollback_explicit {
            options.scrollback
        } else {
            cfg.scrollback_lines.min(u32::MAX as u64) as u32
        }
        .max(1);
        let mut screen = Screen::builder(columns as u32, rows as u32)
            .scrollback(scrollback)
            .build()
            .map_err(|error| io_err(error.to_string()))?;
        screen.cursor.shape = config_cursor(cfg.cursor_shape);

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
            ..RenderConfig::default()
        };
        let renderer = RendererCore::new(render_config, AtlasConfig::default());
        let resolver = ConfigColorResolver::new(&cfg);

        let command = resolve_command(options);
        let mut spawn = SpawnConfig::new(command[0].clone());
        spawn.argv = command;
        spawn.env = inherited_env();
        spawn.initial_window_size = Some(WindowSize::new(rows, columns));
        spawn.stdin_pipe = false;
        let mut child = kitty_os::spawn(&spawn)?;
        // The child calls setsid() before exec. Wait for that (and for exec
        // failure reporting) so signal_group() can never target our own group
        // if shutdown happens immediately after opening.
        let _ = child.wait_for_exec(Some(Duration::from_secs(2)));
        loop_utils::set_nonblocking(child.master_fd(), true)?;

        let mut write_queue = VecDeque::new();
        if let Input::Bytes(bytes) = &input {
            write_queue.extend(bytes);
        }

        Ok(Self {
            backend: Some(backend),
            surface,
            window,
            renderer,
            fonts,
            raster_cache: RasterCache::new(),
            resolver,
            parser: Parser::new(),
            handler: ScreenHandler::new(screen),
            tracker: ModeTracker::new(),
            child,
            write_queue,
            read_buffer: vec![0u8; 64 * 1024],
            padding,
            metrics,
            pixel_size: (surface_width, surface_height),
            columns: columns as u32,
            rows: rows as u32,
            need_redraw: true,
            frame_count: 0,
            output_bytes: 0,
            pty_eof: false,
            exit_status: None,
            exited_at: None,
            timed_out: false,
            should_close: false,
            mouse_down: false,
            last_title: Some(title),
        })
    }

    pub fn screen(&self) -> &Screen {
        &self.handler.screen
    }

    pub fn frame_count(&self) -> u64 {
        self.frame_count
    }

    pub fn output_bytes(&self) -> u64 {
        self.output_bytes
    }

    pub fn child_pid(&self) -> i32 {
        self.child.pid()
    }

    pub fn window_size(&self) -> (u32, u32) {
        self.pixel_size
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
        if self.write_queue.len() + bytes.len() <= MAX_PENDING_INPUT {
            self.write_queue.extend(bytes);
        }
    }

    /// Process one bounded event-loop iteration. Returns false when the
    /// session has stopped (child exit, close request or timeout).
    pub fn step(&mut self, timeout: Duration) -> io::Result<bool> {
        self.read_child()?;
        self.flush_child()?;
        self.check_child()?;

        if self.should_close || (self.exit_status.is_some() && self.pty_eof) {
            return Ok(false);
        }
        if let Some(at) = self.exited_at {
            if at.elapsed() >= Duration::from_millis(200) {
                self.pty_eof = true;
                return Ok(false);
            }
        }

        let timeout_ms = if self.need_redraw {
            0
        } else {
            timeout.as_millis().min(i32::MAX as u128) as i32
        };
        let events = self.window.poll_events(timeout_ms.max(0));
        for event in events {
            self.handle_window_event(event)?;
        }
        if self.should_close {
            return Ok(false);
        }
        self.read_child()?;
        self.flush_child()?;
        self.check_child()?;

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

    /// Run until the child exits, the window closes or `timeout` elapses.
    /// When `screenshot` is set, capture the framebuffer after the first
    /// child output has had time to settle and exit.
    pub fn run(
        mut self,
        timeout: Option<Duration>,
        screenshot: Option<&Path>,
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
                if at.elapsed() >= SCREENSHOT_SETTLE {
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
            // A screenshot run intentionally stops a still-running child, so
            // report success rather than the SIGTERM exit status.
            self.exit_status = Some(ExitStatus::Exited(0));
        }
        Ok(self.into_outcome())
    }

    /// Terminate the child, drain the PTY and release GPU resources.
    pub fn shutdown(&mut self) {
        if self.exit_status.is_none() {
            let _ = self.child.signal_group(15);
            let start = Instant::now();
            while self.exit_status.is_none() && start.elapsed() < Duration::from_millis(300) {
                let _ = self.read_child();
                let _ = self.flush_child();
                let _ = self.check_child();
                if self.exit_status.is_none() {
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
            if self.exit_status.is_none() {
                let _ = self.child.signal_group(9);
                let start = Instant::now();
                while self.exit_status.is_none() && start.elapsed() < Duration::from_secs(2) {
                    let _ = self.check_child();
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        }
        for _ in 0..20 {
            let _ = self.read_child();
            if self.pty_eof {
                break;
            }
            let _ = self.flush_child();
            std::thread::sleep(Duration::from_millis(5));
        }
        if let Some(status) = self.exit_status {
            self.exit_status = Some(status);
        } else {
            self.exit_status = self.child.try_wait().ok().flatten();
        }
        // Drop GL objects while the EGL context is still current.
        let _ = self.surface.make_current();
        let _ = self.backend.take();
    }

    fn into_outcome(mut self) -> RunOutcome {
        let errors = std::mem::take(&mut self.handler.errors);
        let placeholder = Screen::builder(1, 1).build().expect("1x1 screen");
        let screen = std::mem::replace(&mut self.handler.screen, placeholder);
        RunOutcome {
            exit: self.exit_status.unwrap_or(ExitStatus::Unknown(-1)),
            timed_out: self.timed_out,
            screen,
            output_bytes: self.output_bytes,
            errors,
        }
    }

    // {{{ PTY I/O

    fn read_child(&mut self) -> io::Result<()> {
        if self.pty_eof {
            return Ok(());
        }
        let mut chunks = 0usize;
        while chunks < 16 {
            chunks += 1;
            match loop_utils::read_once(self.child.master_fd(), &mut self.read_buffer)? {
                loop_utils::IoOutcome::Completed(0) => {
                    self.pty_eof = true;
                    break;
                }
                loop_utils::IoOutcome::Completed(n) => {
                    let chunk = self.read_buffer[..n].to_vec();
                    self.tracker.feed(&chunk);
                    self.handler.feed(&mut self.parser, &chunk);
                    self.output_bytes += n as u64;
                    self.need_redraw = true;
                    if let Some(title) = self.tracker.title.take() {
                        if self.last_title.as_deref() != Some(title.as_str()) {
                            let _ = self.window.set_title(&title);
                            self.last_title = Some(title);
                        }
                    }
                }
                loop_utils::IoOutcome::WouldBlock => break,
                loop_utils::IoOutcome::Interrupted => continue,
            }
        }
        Ok(())
    }

    fn flush_child(&mut self) -> io::Result<()> {
        while !self.write_queue.is_empty() {
            let (front, _) = self.write_queue.as_slices();
            match loop_utils::write_once(self.child.master_fd(), front)? {
                loop_utils::IoOutcome::Completed(0) => {
                    self.write_queue.clear();
                    break;
                }
                loop_utils::IoOutcome::Completed(n) => {
                    self.write_queue.drain(..n);
                }
                loop_utils::IoOutcome::WouldBlock => break,
                loop_utils::IoOutcome::Interrupted => continue,
            }
        }
        Ok(())
    }

    fn check_child(&mut self) -> io::Result<()> {
        if self.exit_status.is_none() {
            if let Some(status) = self.child.try_wait()? {
                self.exit_status = Some(status);
                self.exited_at = Some(Instant::now());
            }
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
                if self.handler.screen.modes.contains(Modes::FOCUS_TRACKING) {
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
            WindowEvent::Leave { .. } => self.mouse_down = false,
            WindowEvent::SelectionNotify { .. } | WindowEvent::SelectionRequest { .. } => {}
            WindowEvent::Wakeup
            | WindowEvent::Move { .. }
            | WindowEvent::Enter { .. }
            | WindowEvent::SelectionClear { .. } => {}
        }
        Ok(())
    }

    fn handle_resize(&mut self) -> io::Result<()> {
        self.surface
            .make_current()
            .map_err(|error| io_err(error.to_string()))?;
        let (width, height) = self.surface.refresh_size();
        let cell_w = self.metrics.cell_width.max(1.0);
        let cell_h = self.metrics.cell_height.max(1.0);
        let usable_w = (width as f32 - self.padding.0 - self.padding.2).max(cell_w);
        let usable_h = (height as f32 - self.padding.1 - self.padding.3).max(cell_h);
        let columns = (usable_w / cell_w).floor().max(1.0) as u32;
        let rows = (usable_h / cell_h).floor().max(1.0) as u32;
        self.pixel_size = (width, height);
        if columns != self.columns || rows != self.rows {
            self.columns = columns;
            self.rows = rows;
            self.handler.screen.resize(rows, columns);
            let _ = self.child.resize(WindowSize::new(
                rows.min(u16::MAX as u32) as u16,
                columns.min(u16::MAX as u32) as u16,
            ));
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
        if matches!(event.action, kitty_input::KeyAction::Release) {
            // Release events only produce bytes when the kitty protocol asks
            // for them; the encoder decides. Shortcuts fire on press only.
        } else if key.modifiers.control && key.modifiers.shift {
            let base = event.key;
            if base == b'c' as u32 {
                self.copy_selection();
                return;
            }
            if base == b'v' as u32 {
                self.paste_clipboard();
                return;
            }
        }
        if matches!(event.action, kitty_input::KeyAction::Release) {
            let ctx = input::input_context(&self.handler.screen, &self.tracker);
            if let kitty_input::KeyOutput::Bytes(bytes) = input::encode_key_for_screen(&event, &ctx)
            {
                self.write_child(&bytes);
            }
            return;
        }
        if key.modifiers.shift && key.keysym == kitty_window::keysym::PAGE_UP {
            self.scrollback_up();
            return;
        }
        if key.modifiers.shift && key.keysym == kitty_window::keysym::PAGE_DOWN {
            self.scrollback_down();
            return;
        }
        let ctx = input::input_context(&self.handler.screen, &self.tracker);
        match input::encode_key_for_screen(&event, &ctx) {
            kitty_input::KeyOutput::Bytes(bytes) => self.write_child(&bytes),
            kitty_input::KeyOutput::Text(text) => self.write_child(text.as_bytes()),
            kitty_input::KeyOutput::None | kitty_input::KeyOutput::Shortcut => {}
        }
    }

    fn copy_selection(&mut self) {
        if !self.handler.screen.has_selection() {
            return;
        }
        let text = self.handler.screen.selected_text();
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
        if self.handler.screen.modes.contains(Modes::BRACKETED_PASTE) {
            self.write_child(b"\x1b[200~");
            self.write_child(text.as_bytes());
            self.write_child(b"\x1b[201~");
        } else {
            self.write_child(text.as_bytes());
        }
    }

    fn scrollback_up(&mut self) {
        if self.handler.screen.is_using_alternate_linebuf() {
            return;
        }
        let count = self.handler.screen.lines.saturating_sub(1).max(1);
        self.handler
            .screen
            .reverse_scroll_and_fill_from_scrollback(count);
        self.need_redraw = true;
        self.renderer.invalidate();
    }

    fn scrollback_down(&mut self) {
        if self.handler.screen.is_using_alternate_linebuf() {
            return;
        }
        let count = self.handler.screen.lines.saturating_sub(1).max(1);
        self.handler.screen.scroll(count);
        self.need_redraw = true;
        self.renderer.invalidate();
    }

    fn mouse_geometry(&self) -> input::MouseGeometryState {
        input::MouseGeometryState {
            left: self.padding.0 as f64,
            top: self.padding.1 as f64,
            cell_width: self.metrics.cell_width as f64,
            cell_height: self.metrics.cell_height as f64,
            columns: self.columns,
            lines: self.rows,
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
        let ctx = input::input_context(&self.handler.screen, &self.tracker);
        let mods = mouse_modifiers(modifiers);
        let Some(position) = self.mouse_geometry().position(x, y) else {
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
                self.write_child(&bytes);
            }
            self.mouse_down = pressed;
            return;
        }
        if button != kitty_window::MouseButton::Left {
            return;
        }
        if pressed {
            self.mouse_down = true;
            self.handler
                .screen
                .start_selection(position.cell_x, position.cell_y);
        } else {
            self.mouse_down = false;
            self.handler
                .screen
                .update_selection_ext(position.cell_x, position.cell_y, true, true);
            if self.handler.screen.has_selection() {
                let text = self.handler.screen.selected_text();
                let _ = self.window.set_primary_text(&text);
            }
            self.need_redraw = true;
        }
    }

    fn handle_mouse_move(&mut self, x: f64, y: f64, modifiers: kitty_window::Modifiers) {
        let ctx = input::input_context(&self.handler.screen, &self.tracker);
        let mods = mouse_modifiers(modifiers);
        let Some(position) = self.mouse_geometry().position(x, y) else {
            return;
        };
        if input::mouse_goes_to_child(&ctx, mods) {
            let action = if self.mouse_down {
                MouseAction::Drag
            } else {
                MouseAction::Move
            };
            if let Some(bytes) = send_mouse(&ctx, &position, None, action, mods) {
                self.write_child(&bytes);
            }
            return;
        }
        if self.mouse_down && self.handler.screen.selections().in_progress {
            self.handler
                .screen
                .update_selection(position.cell_x, position.cell_y);
            self.need_redraw = true;
        }
    }

    fn handle_scroll(&mut self, x: f64, y: f64, delta_y: f64, modifiers: kitty_window::Modifiers) {
        let ctx = input::input_context(&self.handler.screen, &self.tracker);
        let mods = mouse_modifiers(modifiers);
        let Some(position) = self.mouse_geometry().position(x, y) else {
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
                self.write_child(&bytes);
            }
        } else if delta_y > 0.0 {
            self.scrollback_up();
        } else {
            self.scrollback_down();
        }
    }

    // }}}
    // {{{ Rendering

    fn render(&mut self) -> io::Result<()> {
        self.surface
            .make_current()
            .map_err(|error| io_err(error.to_string()))?;
        let font_metrics = self.fonts.cell_metrics();
        let mut rasterizer = FontRasterizer {
            fonts: &mut self.fonts,
            cache: &mut self.raster_cache,
            metrics: font_metrics,
        };
        let frame =
            self.renderer
                .prepare_frame(&self.handler.screen, &mut rasterizer, &self.resolver);
        let render_config = self.renderer.generator.config.clone();
        self.backend
            .as_mut()
            .ok_or_else(|| io_err("OpenGL backend is shut down"))?
            .sync_atlas(&self.renderer.atlas, &frame)
            .map_err(|error| io_err(error.to_string()))?;
        self.backend
            .as_mut()
            .ok_or_else(|| io_err("OpenGL backend is shut down"))?
            .render(&frame, &render_config)
            .map_err(|error| io_err(error.to_string()))?;
        self.surface
            .swap_buffers()
            .map_err(|error| io_err(error.to_string()))?;
        self.renderer.finish_frame(&mut self.handler.screen);
        self.frame_count += 1;
        self.need_redraw = false;
        Ok(())
    }

    // }}}
}

fn load_config(options: &Options) -> Config {
    let paths: Vec<std::path::PathBuf> = options
        .config
        .iter()
        .map(std::path::PathBuf::from)
        .collect();
    Config::load_lenient(&paths, &[] as &[&str])
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
    session.run(timeout, options.screenshot.as_deref())
}
