// SPDX-License-Identifier: GPL-3.0-or-later
//! Headless PTY terminal used by the `kitr` binary.
//!
//! The event loop combines [`kitty_os::ChildMonitor`] with
//! [`kitty_screen::ScreenHandler`]: PTY bytes are parsed by
//! [`kitty_screen::Parser`] as soon as they are read, input is forwarded to
//! the child, SIGWINCH resizes both the PTY and a text-preserving rebuild of
//! the screen, and the parent's termios state is restored on every exit path.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::io::{self, Write};
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use kitty_os::child::{ExitStatus, SpawnConfig};
use kitty_os::loop_utils::{poll, read_once, write_once, IoOutcome, PollFd, POLLIN};
use kitty_os::pty::{self, Termios, WindowSize};
use kitty_os::{ChildMonitor, LoopData, MonitorError};
use kitty_screen::{Parser, Screen, ScreenError, ScreenHandler};
use kitty_vt::CharType;

#[cfg(target_os = "linux")]
const SIGWINCH: i32 = 28;
#[cfg(not(target_os = "linux"))]
const SIGWINCH: i32 = 28;
const SIGTERM: i32 = 15;
const SIGKILL: i32 = 9;

const POLL_SLICE: Duration = Duration::from_millis(25);
const INPUT_CHUNK: usize = 64 * 1024;
const MAX_PENDING_INPUT: usize = 4 * 1024 * 1024;
const DEFAULT_SCROLLBACK: u32 = 10_000;

#[cfg(feature = "gui")]
pub mod gui;

pub mod remote_client;

static RUN_LOCK: Mutex<()> = Mutex::new(());

/// Parsed command line options.
#[derive(Clone, Debug)]
pub struct Options {
    /// `(columns, rows)`; `None` means use the terminal size or 80x24.
    pub size: Option<(u16, u16)>,
    pub scrollback: u32,
    pub dump_screen: bool,
    pub timeout: Option<Duration>,
    pub command: Vec<OsString>,
    /// True when the command followed `--`/`-e`; disables the `-c` shortcut.
    pub use_separator: bool,
    /// Whether to use the GUI, the headless runner, or decide from `$DISPLAY`.
    pub gui: GuiMode,
    /// Explicit `--config` path.
    pub config: Option<OsString>,
    /// Window title override.
    pub title: Option<String>,
    /// Render then write a top-down PPM and exit.
    pub screenshot: Option<std::path::PathBuf>,
    /// Delay after the first output before taking `--screenshot`.
    pub screenshot_delay: Duration,
    /// True when `--scrollback` was given on the command line.
    pub scrollback_explicit: bool,
    /// Load tabs/windows from this kitty session file.
    pub session: Option<std::path::PathBuf>,
    /// Remote-control socket (`unix:/path`, `unix:@abstract` or a path).
    pub listen_on: Option<OsString>,
    /// Create a layer-shell panel/QAT surface instead of a normal toplevel.
    pub layer_shell: Option<kitty_kittens::panel::LayerShellConfig>,
    /// Start a panel/QAT surface without mapping it (`--start-as-hidden`).
    pub layer_shell_start_hidden: bool,
}

/// Frontend selection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GuiMode {
    /// GUI when `$DISPLAY` is set (unless `--dump-screen` is used).
    #[default]
    Auto,
    /// Force the old headless runner.
    Headless,
    /// Force the X11/EGL frontend.
    Gui,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            size: None,
            scrollback: DEFAULT_SCROLLBACK,
            dump_screen: false,
            timeout: None,
            command: Vec::new(),
            use_separator: false,
            gui: GuiMode::Auto,
            config: None,
            title: None,
            screenshot: None,
            screenshot_delay: Duration::from_millis(150),
            scrollback_explicit: false,
            session: None,
            listen_on: None,
            layer_shell: None,
            layer_shell_start_hidden: false,
        }
    }
}

/// Result of parsing the CLI.
#[derive(Clone, Debug)]
#[allow(clippy::large_enum_variant)]
pub enum CliAction {
    Run(Options),
    Help,
    Version,
}

/// Argument parsing failure.
#[derive(Clone, Debug)]
pub enum ArgError {
    UnknownOption(String),
    MissingValue(&'static str),
    InvalidValue { option: &'static str, value: String },
}

impl core::fmt::Display for ArgError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::UnknownOption(option) => write!(f, "unknown option: {option}"),
            Self::MissingValue(option) => write!(f, "option {option} requires a value"),
            Self::InvalidValue { option, value } => {
                write!(f, "invalid value for {option}: {value}")
            }
        }
    }
}

impl std::error::Error for ArgError {}

/// Where the child's stdin comes from.
#[derive(Clone, Debug)]
pub enum Input {
    /// Inherit the process stdin. TTYs are put into raw mode and forwarded
    /// through the PTY; pipes are forwarded through a child stdin pipe.
    Inherit,
    /// Feed these bytes through a child stdin pipe, then close it.
    Bytes(Vec<u8>),
    /// Immediate EOF on the child's stdin.
    None,
}

/// Everything a completed headless session exposes to callers and tests.
pub struct RunOutcome {
    pub exit: ExitStatus,
    pub timed_out: bool,
    pub screen: Screen,
    pub output_bytes: u64,
    pub errors: Vec<String>,
}

impl RunOutcome {
    pub fn exit_code(&self) -> ExitCode {
        if self.timed_out {
            return ExitCode::from(124);
        }
        match self.exit {
            ExitStatus::Exited(code) if (0..=255).contains(&code) => ExitCode::from(code as u8),
            ExitStatus::Signaled(sig) => {
                ExitCode::from((128i32.saturating_add(sig)).clamp(0, 255) as u8)
            }
            _ => ExitCode::from(1),
        }
    }
}

const HELP: &str = "\
kitr - terminal\n\
\n\
Usage: kitr [OPTIONS] [-e|--] [COMMAND [ARGS...]]\n\
\n\
Options:\n\
  --size COLSxROWS   Initial PTY and screen size (default: terminal size or 80x24)\n\
  --scrollback N     Scrollback capacity in lines (default: 10000)\n\
  --dump-screen      Write the final screen as text to stdout (headless unless --gui)\n\
  --timeout SECONDS  Kill the child after SECONDS (fractional values allowed)\n\
  --headless         Force the old headless runner\n\
  --gui              Force the X11/EGL window frontend\n\
  --config PATH      Load configuration from PATH\n\
  --title TITLE      Set the window title\n\
  --screenshot FILE  Render a few frames, write FILE as PPM (P6), then exit\n\
  --session FILE     Restore tabs/windows/layouts from a kitty session file\n\
  --listen-on ADDR   Start remote control on unix:/path or unix:@abstract name\n\
  -h, --help         Show this help\n\
  -V, --version      Show the version\n\
\n\
The GUI is used by default when $DISPLAY is set. If no command is given,\n\
$SHELL (or /bin/sh) is started. Everything after -e or -- is passed through\n\
as argv without further option parsing. The shortcut `kitr -c 'script'`\n\
runs the default shell with -c.\n\
\n\
Remote control client:\n\
  kitr @ --to=unix:/path get-text [--extent=screen|all|selection]\n\
  kitr @ --to=unix:@name send-text 'text' | resize-window | close-window | screenshot\n\
  The socket can also come from $KITTY_LISTEN_ON.\n";

/// Parse `kitr` arguments.
pub fn parse_args<I, S>(args: I) -> Result<CliAction, ArgError>
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    let mut iter = args.into_iter().map(Into::into).peekable();
    let mut options = Options::default();
    while let Some(arg) = iter.next() {
        let text = arg.to_string_lossy();
        match text.as_ref() {
            "-h" | "--help" => return Ok(CliAction::Help),
            "-V" | "--version" => return Ok(CliAction::Version),
            "--dump-screen" => options.dump_screen = true,
            "--headless" => options.gui = GuiMode::Headless,
            "--gui" => options.gui = GuiMode::Gui,
            "--config" => {
                let value = iter.next().ok_or(ArgError::MissingValue("--config"))?;
                options.config = Some(value);
            }
            "--title" => {
                let value = iter.next().ok_or(ArgError::MissingValue("--title"))?;
                options.title = Some(value.to_string_lossy().into_owned());
            }
            "--screenshot" => {
                let value = iter.next().ok_or(ArgError::MissingValue("--screenshot"))?;
                options.screenshot = Some(std::path::PathBuf::from(value));
            }
            "--session" => {
                let value = iter.next().ok_or(ArgError::MissingValue("--session"))?;
                options.session = Some(std::path::PathBuf::from(value));
            }
            "--screenshot-delay" => {
                let value = iter
                    .next()
                    .ok_or(ArgError::MissingValue("--screenshot-delay"))?;
                options.screenshot_delay = Duration::from_millis(parse_u32(
                    "--screenshot-delay",
                    &value.to_string_lossy(),
                )? as u64);
            }
            "--listen-on" => {
                let value = iter.next().ok_or(ArgError::MissingValue("--listen-on"))?;
                options.listen_on = Some(value);
            }
            "-e" | "--" => {
                options.use_separator = true;
                options.command = iter.collect();
                return Ok(CliAction::Run(options));
            }
            "-c" => {
                options.command = vec![arg];
                options.command.extend(iter);
                return Ok(CliAction::Run(options));
            }
            "--size" => {
                let value = iter.next().ok_or(ArgError::MissingValue("--size"))?;
                options.size = Some(parse_size(&value.to_string_lossy())?);
            }
            "--scrollback" => {
                let value = iter.next().ok_or(ArgError::MissingValue("--scrollback"))?;
                options.scrollback = parse_u32("--scrollback", &value.to_string_lossy())?;
                options.scrollback_explicit = true;
            }
            "--timeout" => {
                let value = iter.next().ok_or(ArgError::MissingValue("--timeout"))?;
                options.timeout = Some(parse_timeout(&value.to_string_lossy())?);
            }
            _ if text.starts_with("--size=") => {
                options.size = Some(parse_size(&text["--size=".len()..])?);
            }
            _ if text.starts_with("--timeout=") => {
                options.timeout = Some(parse_timeout(&text["--timeout=".len()..])?);
            }
            _ if text.starts_with("--config=") => {
                options.config = Some(OsString::from(&text["--config=".len()..]));
            }
            _ if text.starts_with("--title=") => {
                options.title = Some(text["--title=".len()..].to_string());
            }
            _ if text.starts_with("--screenshot=") => {
                options.screenshot = Some(std::path::PathBuf::from(&text["--screenshot=".len()..]));
            }
            _ if text.starts_with("--session=") => {
                options.session = Some(std::path::PathBuf::from(&text["--session=".len()..]));
            }
            _ if text.starts_with("--listen-on=") => {
                options.listen_on = Some(OsString::from(&text["--listen-on=".len()..]));
            }
            _ if text.starts_with("--screenshot-delay=") => {
                options.screenshot_delay = Duration::from_millis(parse_u32(
                    "--screenshot-delay",
                    &text["--screenshot-delay=".len()..],
                )? as u64);
            }
            _ if text.starts_with("--scrollback=") => {
                options.scrollback = parse_u32("--scrollback", &text["--scrollback=".len()..])?;
                options.scrollback_explicit = true;
            }
            _ if text.starts_with('-') && text != "-" => {
                return Err(ArgError::UnknownOption(text.into_owned()));
            }
            _ => {
                options.command = vec![arg];
                options.command.extend(iter);
                return Ok(CliAction::Run(options));
            }
        }
    }
    Ok(CliAction::Run(options))
}

fn parse_size(value: &str) -> Result<(u16, u16), ArgError> {
    let (columns, rows) = value
        .split_once(['x', 'X'])
        .ok_or_else(|| ArgError::InvalidValue {
            option: "--size",
            value: value.to_string(),
        })?;
    let columns = columns.parse::<u16>().ok().filter(|v| *v > 0);
    let rows = rows.parse::<u16>().ok().filter(|v| *v > 0);
    match (columns, rows) {
        (Some(columns), Some(rows)) => Ok((columns, rows)),
        _ => Err(ArgError::InvalidValue {
            option: "--size",
            value: value.to_string(),
        }),
    }
}

fn parse_u32(option: &'static str, value: &str) -> Result<u32, ArgError> {
    value
        .parse::<u32>()
        .ok()
        .filter(|v| *v > 0)
        .ok_or_else(|| ArgError::InvalidValue {
            option,
            value: value.to_string(),
        })
}

fn parse_timeout(value: &str) -> Result<Duration, ArgError> {
    let seconds = value.parse::<f64>().map_err(|_| ArgError::InvalidValue {
        option: "--timeout",
        value: value.to_string(),
    })?;
    if !seconds.is_finite() || seconds < 0.0 {
        return Err(ArgError::InvalidValue {
            option: "--timeout",
            value: value.to_string(),
        });
    }
    Ok(Duration::from_secs_f64(seconds))
}

fn use_gui(options: &Options) -> bool {
    match options.gui {
        GuiMode::Gui => true,
        GuiMode::Headless => false,
        GuiMode::Auto => {
            !options.dump_screen
                && (options.screenshot.is_some()
                    || options.session.is_some()
                    || options.listen_on.is_some()
                    || options.layer_shell.is_some()
                    || std::env::var_os("DISPLAY").is_some()
                    || std::env::var_os("WAYLAND_DISPLAY").is_some())
        }
    }
}

/// Recognize the `kitty +kitten panel` / `quick-access-terminal` host form and
/// turn it into a GUI [`Options`] carrying a real layer-shell configuration.
fn kitten_host_options(args: &[OsString]) -> io::Result<Option<Options>> {
    let Some(first) = args.first() else {
        return Ok(None);
    };
    if !matches!(
        first.to_str(),
        Some("+kitten") | Some("kitten") | Some("--kitten")
    ) {
        return Ok(None);
    }
    let Some(name) = args.get(1).and_then(|value| value.to_str()) else {
        return Ok(None);
    };
    let rest: Vec<String> = args[2..]
        .iter()
        .map(|value| value.to_string_lossy().into_owned())
        .collect();
    let mut options = Options::default();
    match name {
        "panel" => {
            let panel = kitty_kittens::panel::parse_panel_args(&rest)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
            if panel.help {
                let mut stdout = io::stdout().lock();
                stdout.write_all(kitty_kittens::panel::PANEL_USAGE.as_bytes())?;
                stdout.flush()?;
                return Ok(None);
            }
            options.layer_shell = Some(kitty_kittens::panel::layer_shell_config(&panel));
            options.layer_shell_start_hidden = panel.start_as_hidden;
            options.title = Some(if panel.name.is_empty() {
                panel.class.clone()
            } else {
                panel.name.clone()
            });
            options.command = panel.items.into_iter().map(OsString::from).collect();
            Ok(Some(options))
        }
        "quick-access-terminal" | "quick-access" => {
            let qat = kitty_kittens::quick_access_terminal::parse_quick_access_args(&rest)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
            if qat.help {
                let mut stdout = io::stdout().lock();
                stdout.write_all(
                    kitty_kittens::quick_access_terminal::QUICK_ACCESS_USAGE.as_bytes(),
                )?;
                stdout.flush()?;
                return Ok(None);
            }
            let config = kitty_kittens::quick_access_terminal::load_config(
                &qat,
                &kitty_kittens::quick_access_terminal::config_paths(),
            )
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
            let launch = kitty_kittens::quick_access_terminal::build_quick_access_launch(
                std::path::Path::new("kitr"),
                &config,
                &qat,
                std::path::Path::new("."),
            );
            options.layer_shell = Some(launch.layer_shell);
            options.layer_shell_start_hidden = config.start_as_hidden;
            options.title = Some(config.app_id);
            options.command = qat.items.into_iter().map(OsString::from).collect();
            Ok(Some(options))
        }
        _ => Ok(None),
    }
}

/// Run `kitr` with process stdio and return the process exit code.
pub fn run<I, S>(args: I) -> io::Result<ExitCode>
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    let args: Vec<OsString> = args.into_iter().map(Into::into).collect();
    if args.first().is_some_and(|arg| arg == OsStr::new("@")) {
        return remote_client::run_client(&args[1..]);
    }
    let kitten_host = kitten_host_options(&args)?;
    let action = match kitten_host {
        Some(options) => CliAction::Run(options),
        None => parse_args(args).map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?,
    };
    match action {
        CliAction::Help => {
            let mut stdout = io::stdout().lock();
            stdout.write_all(HELP.as_bytes())?;
            stdout.flush()?;
            Ok(ExitCode::SUCCESS)
        }
        CliAction::Version => {
            let mut stdout = io::stdout().lock();
            writeln!(stdout, "kitr {}", env!("CARGO_PKG_VERSION"))?;
            stdout.flush()?;
            Ok(ExitCode::SUCCESS)
        }
        CliAction::Run(options) => {
            let gui = use_gui(&options);
            let outcome = if gui {
                #[cfg(feature = "gui")]
                {
                    gui::run_gui(&options, Input::Inherit)?
                }
                #[cfg(not(feature = "gui"))]
                {
                    return Err(io::Error::new(
                        io::ErrorKind::Unsupported,
                        "this build has no GUI support (rebuild with the gui feature)",
                    ));
                }
            } else {
                run_options(&options, Input::Inherit)?
            };
            if options.dump_screen {
                let text = dump_screen(&outcome.screen);
                let mut stdout = io::stdout().lock();
                stdout.write_all(text.as_bytes())?;
                stdout.flush()?;
            }
            Ok(outcome.exit_code())
        }
    }
}

/// Convenience runner for tests: parse CLI args and feed in-memory stdin.
pub fn run_capture<I, S>(args: I, input: &[u8]) -> io::Result<RunOutcome>
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    let action =
        parse_args(args).map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?;
    match action {
        CliAction::Run(options) => run_options(&options, Input::Bytes(input.to_vec())),
        CliAction::Help | CliAction::Version => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "help/version requested, no session to run",
        )),
    }
}

/// Spawn a child, drive it until exit/timeout and return the parsed screen.
pub fn run_options(options: &Options, input: Input) -> io::Result<RunOutcome> {
    let _guard = RUN_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let command = resolve_command(options);
    let requested_size = options.size;
    let mut size = requested_size
        .map(|(columns, rows)| WindowSize::new(rows, columns))
        .or_else(query_terminal_size)
        .unwrap_or_else(|| WindowSize::new(24, 80));
    if size.columns == 0 || size.rows == 0 {
        size = WindowSize::new(24, 80);
    }
    let track_terminal = requested_size.is_none();

    let stdin_is_tty = matches!(input, Input::Inherit) && pty::is_tty(0);
    let use_stdin_pipe = !stdin_is_tty;

    let mut config = SpawnConfig::new(command[0].clone());
    config.argv = command;
    config.env = inherited_env();
    config.initial_window_size = Some(size);
    config.stdin_pipe = use_stdin_pipe;

    let mut child = kitty_os::spawn(&config)?;
    let _raw = if stdin_is_tty {
        Some(RawModeGuard::enter()?)
    } else {
        None
    };

    let pipe_writer = if use_stdin_pipe {
        child.take_stdin_write()
    } else {
        None
    };
    if let Some(fd) = pipe_writer.as_ref() {
        kitty_os::set_nonblocking(fd.as_raw_fd(), true)?;
    }

    let screen = Screen::builder(size.columns as u32, size.rows as u32)
        .scrollback(options.scrollback)
        .build()
        .map_err(screen_error)?;
    let state = Rc::new(RefCell::new(ScreenState {
        parser: Parser::new(),
        handler: ScreenHandler::new(screen),
    }));

    let mut monitor = ChildMonitor::new(child)?;
    let callback_state = Rc::clone(&state);
    monitor.set_output_callback(move |data| {
        let mut state = callback_state.borrow_mut();
        let ScreenState { parser, handler } = &mut *state;
        handler.feed(parser, data);
    });

    let mut winch = LoopData::new(&[SIGWINCH]).ok();
    let mut feeder = match &input {
        Input::Bytes(bytes) => Some(PipeFeeder::new(
            pipe_writer,
            InputSource::Bytes(bytes.clone()),
        )),
        Input::Inherit if use_stdin_pipe => Some(PipeFeeder::new(pipe_writer, InputSource::Fd(0))),
        Input::Inherit => None,
        Input::None => Some(PipeFeeder::new(pipe_writer, InputSource::None)),
    };

    let started = Instant::now();
    let deadline = options.timeout.map(|timeout| started + timeout);
    let mut current_size = size;
    let mut last_size_check = Instant::now();
    let mut output_bytes = 0u64;
    let mut exit_status = None;
    let mut timed_out = false;

    loop {
        if let Some(feeder) = feeder.as_mut() {
            feeder.pump();
        }
        if stdin_is_tty {
            pump_tty_input(&mut monitor)?;
        }

        let now = Instant::now();
        if let Some(deadline) = deadline {
            if now >= deadline && exit_status.is_none() {
                timed_out = true;
                break;
            }
        }

        let slice = match deadline {
            Some(deadline) => deadline.saturating_duration_since(now).min(POLL_SLICE),
            None => POLL_SLICE,
        };
        let slice = if slice.is_zero() {
            Duration::from_millis(1)
        } else {
            slice
        };
        let outcome = monitor.poll_once(Some(slice)).map_err(monitor_error)?;
        output_bytes += outcome.output_bytes as u64;
        if let Some(status) = outcome.exited {
            exit_status = Some(status);
        }

        let mut winched = false;
        if let Some(signals) = winch.as_mut() {
            if let Ok(signals) = signals.read_signals() {
                winched = signals.iter().any(|signal| signal.signo == SIGWINCH);
            }
        }
        if winched || (track_terminal && last_size_check.elapsed() >= Duration::from_millis(250)) {
            resize_if_needed(&monitor, &state, &mut current_size, options.scrollback);
            last_size_check = Instant::now();
        }

        if exit_status.is_some() {
            drain_after_exit(&mut monitor, &mut output_bytes)?;
            break;
        }
    }

    if timed_out {
        terminate(&mut monitor, &mut output_bytes)?;
        exit_status = monitor.exit_status();
    }

    drop(monitor);
    let state = match Rc::try_unwrap(state) {
        Ok(cell) => cell.into_inner(),
        Err(state) => {
            let state = state.borrow();
            ScreenState {
                parser: Parser::new(),
                handler: ScreenHandler::new(
                    resize_keep_text(
                        &state.handler.screen,
                        state.handler.screen.columns,
                        state.handler.screen.lines,
                        options.scrollback,
                    )
                    .map_err(screen_error)?,
                ),
            }
        }
    };
    let errors = state.handler.errors;
    Ok(RunOutcome {
        exit: exit_status.unwrap_or(ExitStatus::Unknown(-1)),
        timed_out,
        screen: state.handler.screen,
        output_bytes,
        errors,
    })
}

struct ScreenState {
    parser: Parser,
    handler: ScreenHandler,
}

fn resolve_command(options: &Options) -> Vec<OsString> {
    if options.command.is_empty() {
        return vec![default_shell()];
    }
    if !options.use_separator && options.command[0] == OsStr::new("-c") {
        let mut command = vec![default_shell()];
        command.extend(options.command.iter().cloned());
        return command;
    }
    options.command.clone()
}

fn default_shell() -> OsString {
    std::env::var_os("SHELL")
        .filter(|shell| !shell.is_empty())
        .unwrap_or_else(|| OsString::from("/bin/sh"))
}

fn inherited_env() -> Vec<OsString> {
    let mut env: Vec<OsString> = std::env::vars_os()
        .map(|(key, value)| {
            let mut entry = key;
            entry.push("=");
            entry.push(value);
            entry
        })
        .collect();
    if !env
        .iter()
        .any(|entry| entry.as_bytes().starts_with(b"TERM="))
    {
        env.push(OsString::from("TERM=xterm-kitty"));
    }
    env
}

fn query_terminal_size() -> Option<WindowSize> {
    for fd in [0, 1, 2] {
        if pty::is_tty(fd) {
            if let Ok(size) = pty::window_size(fd) {
                if size.columns > 0 && size.rows > 0 {
                    return Some(size);
                }
            }
        }
    }
    None
}

fn resize_if_needed(
    monitor: &ChildMonitor,
    state: &Rc<RefCell<ScreenState>>,
    current: &mut WindowSize,
    scrollback: u32,
) {
    let Some(size) = query_terminal_size() else {
        return;
    };
    if size.columns == 0 || size.rows == 0 || size == *current {
        return;
    }
    let _ = monitor.resize(size);
    let mut state = state.borrow_mut();
    if let Ok(screen) = resize_keep_text(
        &state.handler.screen,
        size.columns as u32,
        size.rows as u32,
        scrollback,
    ) {
        state.handler.screen = screen;
    }
    *current = size;
}

fn drain_after_exit(monitor: &mut ChildMonitor, total: &mut u64) -> io::Result<()> {
    let deadline = Instant::now() + Duration::from_millis(150);
    while !monitor.is_eof() && Instant::now() < deadline {
        let outcome = monitor
            .poll_once(Some(Duration::from_millis(10)))
            .map_err(monitor_error)?;
        *total += outcome.output_bytes as u64;
        if outcome.timed_out && outcome.output_bytes == 0 {
            break;
        }
    }
    Ok(())
}

fn terminate(monitor: &mut ChildMonitor, total: &mut u64) -> io::Result<()> {
    let _ = monitor.child().signal_group(SIGTERM);
    let graceful = Instant::now() + Duration::from_millis(300);
    while monitor.exit_status().is_none() && Instant::now() < graceful {
        let outcome = monitor
            .poll_once(Some(Duration::from_millis(10)))
            .map_err(monitor_error)?;
        *total += outcome.output_bytes as u64;
    }
    if monitor.exit_status().is_none() {
        let _ = monitor.child().signal_group(SIGKILL);
        let hard = Instant::now() + Duration::from_secs(2);
        while monitor.exit_status().is_none() && Instant::now() < hard {
            let outcome = monitor
                .poll_once(Some(Duration::from_millis(10)))
                .map_err(monitor_error)?;
            *total += outcome.output_bytes as u64;
        }
    }
    for _ in 0..20 {
        if monitor.is_eof() {
            break;
        }
        let outcome = monitor
            .poll_once(Some(Duration::from_millis(5)))
            .map_err(monitor_error)?;
        *total += outcome.output_bytes as u64;
        if outcome.timed_out {
            break;
        }
    }
    Ok(())
}

fn monitor_error(error: MonitorError) -> io::Error {
    match error {
        MonitorError::Io(error) => error,
        other => io::Error::other(other.to_string()),
    }
}

fn screen_error(error: ScreenError) -> io::Error {
    io::Error::other(error.to_string())
}

fn pump_tty_input(monitor: &mut ChildMonitor) -> io::Result<()> {
    let mut buffer = [0u8; INPUT_CHUNK];
    loop {
        let mut fds = [PollFd::new(0, POLLIN)];
        if poll(&mut fds, 0)? == 0 || !fds[0].has_event(POLLIN) {
            return Ok(());
        }
        match read_once(0, &mut buffer)? {
            IoOutcome::Completed(0) => return Ok(()),
            IoOutcome::Completed(n) => match monitor.queue_write(&buffer[..n]) {
                Ok(()) => continue,
                Err(MonitorError::TooMuchData) => return Ok(()),
                Err(error) => return Err(monitor_error(error)),
            },
            IoOutcome::WouldBlock => return Ok(()),
            IoOutcome::Interrupted => continue,
        }
    }
}

struct RawModeGuard {
    fd: RawFd,
    saved: Termios,
}

impl RawModeGuard {
    fn enter() -> io::Result<Self> {
        let saved = pty::enter_raw_mode(0)?;
        Ok(Self { fd: 0, saved })
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = pty::restore_termios(self.fd, &self.saved);
    }
}

enum InputSource {
    Fd(RawFd),
    Bytes(Vec<u8>),
    None,
}

struct PipeFeeder {
    writer: Option<OwnedFd>,
    pending: VecDeque<u8>,
    source: InputSource,
    position: usize,
    eof: bool,
}

impl PipeFeeder {
    fn new(writer: Option<OwnedFd>, source: InputSource) -> Self {
        Self {
            writer,
            pending: VecDeque::new(),
            eof: matches!(source, InputSource::None),
            source,
            position: 0,
        }
    }

    fn pump(&mut self) {
        while !self.eof && self.pending.len() < MAX_PENDING_INPUT {
            match &mut self.source {
                InputSource::None => self.eof = true,
                InputSource::Bytes(bytes) => {
                    if self.position >= bytes.len() {
                        self.eof = true;
                    } else {
                        let available = &bytes[self.position..];
                        let count = available
                            .len()
                            .min(INPUT_CHUNK)
                            .min(MAX_PENDING_INPUT - self.pending.len());
                        self.pending.extend(&available[..count]);
                        self.position += count;
                    }
                }
                InputSource::Fd(fd) => {
                    let mut fds = [PollFd::new(*fd, POLLIN)];
                    match poll(&mut fds, 0) {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {
                            let mut buffer = [0u8; INPUT_CHUNK];
                            let count = buffer.len().min(MAX_PENDING_INPUT - self.pending.len());
                            match read_once(*fd, &mut buffer[..count]) {
                                Ok(IoOutcome::Completed(0)) => self.eof = true,
                                Ok(IoOutcome::Completed(n)) => {
                                    self.pending.extend(&buffer[..n]);
                                }
                                Ok(IoOutcome::WouldBlock) => break,
                                Ok(IoOutcome::Interrupted) => continue,
                                Err(_) => self.eof = true,
                            }
                        }
                    }
                }
            }
        }
        self.flush();
        if self.eof && self.pending.is_empty() {
            self.writer = None;
        }
    }

    fn flush(&mut self) {
        let Some(fd) = self.writer.as_ref().map(AsRawFd::as_raw_fd) else {
            return;
        };
        while !self.pending.is_empty() {
            let (front, _) = self.pending.as_slices();
            let outcome = write_once(fd, front);
            match outcome {
                Ok(IoOutcome::Completed(0)) => {
                    self.pending.clear();
                    self.writer = None;
                    self.eof = true;
                    return;
                }
                Ok(IoOutcome::Completed(n)) => {
                    self.pending.drain(..n);
                }
                Ok(IoOutcome::WouldBlock) => return,
                Ok(IoOutcome::Interrupted) => continue,
                Err(_) => {
                    self.pending.clear();
                    self.writer = None;
                    self.eof = true;
                    return;
                }
            }
        }
        if self.eof {
            self.writer = None;
        }
    }
}

/// Serialize the visible screen as text. Trailing empty rows are omitted;
/// rows are separated by `\n` and there is no trailing newline.
pub fn dump_screen(screen: &Screen) -> String {
    let mut lines: Vec<String> = (0..screen.lines).map(|y| screen.line_text(y)).collect();
    while lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}

/// Rebuild a screen at a new size, copying the visible text and cursor.
///
/// Scrollback is not carried across the rebuild. This intentionally lives in
/// the binary crate because `kitty-screen` does not implement resize/rewrap.
pub fn resize_keep_text(
    old: &Screen,
    columns: u32,
    lines: u32,
    scrollback: u32,
) -> Result<Screen, ScreenError> {
    let columns = columns.max(1);
    let lines = lines.max(1);
    let mut screen = Screen::builder(columns, lines)
        .scrollback(scrollback)
        .build()?;

    let wrap = screen.modes.contains(kitty_screen::Modes::DECAWM);
    screen.modes.remove(kitty_screen::Modes::DECAWM);
    for y in 0..lines.min(old.lines) {
        let text = old.line_text(y);
        if text.is_empty() {
            continue;
        }
        let chars: Vec<CharType> = text
            .chars()
            .filter(|ch| !ch.is_control())
            .take(columns as usize)
            .map(|ch| ch as CharType)
            .collect();
        if chars.is_empty() {
            continue;
        }
        screen.cursor_position(y + 1, 1);
        screen.draw_text(&chars);
    }
    screen.modes.set(kitty_screen::Modes::DECAWM, wrap);

    let mut cursor = old.cursor;
    cursor.x = cursor.x.min(columns - 1);
    cursor.y = cursor.y.min(lines - 1);
    screen.cursor = cursor;
    Ok(screen)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(command: &[&str]) -> Options {
        Options {
            size: Some((20, 5)),
            scrollback: 10,
            command: command.iter().map(OsString::from).collect(),
            ..Options::default()
        }
    }

    #[test]
    fn parses_panel_and_quick_access_host_invocations() {
        let args = [
            "+kitten",
            "panel",
            "--edge=bottom",
            "--layer=overlay",
            "--lines=10",
            "--app-id=kitty-panel",
            "--start-as-hidden",
            "sh",
        ]
        .iter()
        .map(OsString::from)
        .collect::<Vec<_>>();
        let options = kitten_host_options(&args).unwrap().unwrap();
        let layer = options.layer_shell.as_ref().unwrap();
        assert_eq!(layer.edge, kitty_kittens::panel::Edge::Bottom);
        assert_eq!(layer.layer, kitty_kittens::panel::Layer::Overlay);
        assert_eq!(layer.y_size_in_cells, 10);
        assert!(options.layer_shell_start_hidden);
        assert_eq!(options.command, vec![OsString::from("sh")]);

        let args = ["+kitten", "quick-access-terminal", "--override=lines=12"]
            .iter()
            .map(OsString::from)
            .collect::<Vec<_>>();
        let options = kitten_host_options(&args).unwrap().unwrap();
        assert_eq!(options.layer_shell.unwrap().y_size_in_cells, 12);
    }

    #[test]
    fn parses_short_and_long_options() {
        let action = parse_args([
            "--size",
            "100x40",
            "--scrollback=500",
            "--timeout",
            "1.5",
            "--dump-screen",
            "-e",
            "/bin/sh",
            "-c",
            "echo hi",
        ])
        .unwrap();
        let CliAction::Run(options) = action else {
            panic!("expected run action");
        };
        assert_eq!(options.size, Some((100, 40)));
        assert_eq!(options.scrollback, 500);
        assert_eq!(options.timeout, Some(Duration::from_millis(1500)));
        assert!(options.dump_screen);
        assert!(options.use_separator);
        assert_eq!(options.command.len(), 3);
        assert_eq!(options.command[2], "echo hi");
    }

    #[test]
    fn separator_passes_through_flag_like_arguments() {
        let action = parse_args(["--", "/bin/echo", "--help"]).unwrap();
        let CliAction::Run(options) = action else {
            panic!("expected run action");
        };
        assert_eq!(options.command, ["/bin/echo", "--help"]);
    }

    #[test]
    fn dash_c_shortcut_uses_the_default_shell() {
        let options = opts(&["-c", "printf hi"]);
        let command = resolve_command(&options);
        assert!(command.len() >= 3);
        assert_eq!(command[1], "-c");
        assert_eq!(command[2], "printf hi");
    }

    #[test]
    fn parses_gui_frontend_options() {
        let action = parse_args([
            "--gui",
            "--config",
            "/tmp/kitty.conf",
            "--title=hello",
            "--screenshot",
            "/tmp/out.ppm",
            "--scrollback",
            "42",
        ])
        .unwrap();
        let CliAction::Run(options) = action else {
            panic!("expected run action");
        };
        assert_eq!(options.gui, GuiMode::Gui);
        assert_eq!(
            options.config.as_deref(),
            Some(OsStr::new("/tmp/kitty.conf"))
        );
        assert_eq!(options.title.as_deref(), Some("hello"));
        assert_eq!(
            options.screenshot.as_deref(),
            Some(std::path::Path::new("/tmp/out.ppm"))
        );
        assert_eq!(options.scrollback, 42);
        assert!(options.scrollback_explicit);

        let action = parse_args([
            "--session=/tmp/test.session",
            "--listen-on",
            "unix:@kitr-test",
        ])
        .unwrap();
        let CliAction::Run(options) = action else {
            panic!("expected run action");
        };
        assert_eq!(
            options.session.as_deref(),
            Some(std::path::Path::new("/tmp/test.session"))
        );
        assert_eq!(
            options.listen_on.as_deref(),
            Some(OsStr::new("unix:@kitr-test"))
        );

        let action = parse_args(["--headless"]).unwrap();
        let CliAction::Run(options) = action else {
            panic!("expected run action");
        };
        assert_eq!(options.gui, GuiMode::Headless);
    }

    #[test]
    fn rejects_bad_values_and_unknown_options() {
        assert!(parse_args(["--size", "80"]).is_err());
        assert!(parse_args(["--size", "0x24"]).is_err());
        assert!(parse_args(["--scrollback", "0"]).is_err());
        assert!(parse_args(["--timeout", "nan"]).is_err());
        assert!(parse_args(["--wat"]).is_err());
    }

    #[test]
    fn dump_omits_trailing_blank_rows() {
        let mut screen = Screen::builder(10, 4).build().unwrap();
        let text: Vec<CharType> = "hello".chars().map(|ch| ch as CharType).collect();
        screen.draw_text(&text);
        assert_eq!(dump_screen(&screen), "hello");
    }

    #[test]
    fn resize_keeps_visible_text_and_clamps_the_cursor() {
        let mut screen = Screen::builder(10, 3).scrollback(4).build().unwrap();
        let text: Vec<CharType> = "hello".chars().map(|ch| ch as CharType).collect();
        screen.draw_text(&text);
        screen.cursor_position(3, 8);

        let resized = resize_keep_text(&screen, 20, 2, 4).unwrap();
        assert_eq!(resized.line_text(0), "hello");
        assert_eq!(resized.cursor.y, 1);
        assert_eq!(resized.cursor.x, 7);
    }

    #[test]
    fn exit_code_mapping_covers_signals_and_timeouts() {
        let mut outcome = RunOutcome {
            exit: ExitStatus::Exited(7),
            timed_out: false,
            screen: Screen::builder(1, 1).build().unwrap(),
            output_bytes: 0,
            errors: Vec::new(),
        };
        assert_eq!(
            format!("{:?}", outcome.exit_code()),
            "ExitCode(unix_exit_status(7))"
        );
        outcome.exit = ExitStatus::Signaled(15);
        assert_eq!(
            format!("{:?}", outcome.exit_code()),
            "ExitCode(unix_exit_status(143))"
        );
        outcome.exit = ExitStatus::Exited(0);
        outcome.timed_out = true;
        assert_eq!(
            format!("{:?}", outcome.exit_code()),
            "ExitCode(unix_exit_status(124))"
        );
    }
}
