// SPDX-License-Identifier: GPL-3.0-or-later
//! Kitten discovery, launching and protocol helpers, ported from
//! `kitty/kittens.c` and the Python/Go kitten implementations.
//!
//! # Scope
//!
//! This crate is the host-side library for kitty kittens. It provides:
//!
//! * [`discovery`]: ordered filesystem discovery of Python kittens,
//!   executables and JSON manifests, with aliases and collision rules.
//! * [`launch`]: construction of a complete process launch (argv, environment,
//!   cwd, stdio), including kitty's `KITTY_KITTEN_*`, `KITTY_WINDOW_ID` and
//!   `KITTY_LISTEN_ON` bootstrap variables.
//! * [`protocol`]: incremental DCS framing for kitten results, overlays,
//!   remote-print and `@kitty-cmd` remote-control payloads, plus the
//!   [`protocol::KittenHost`] callback trait.
//! * [`shell_integration`]: discovery of the shell-integration resource
//!   directory, generation of the bash/zsh/fish handoff environment, and
//!   `--shell-integration` install snippets.
//! * [`cli`]: the `kitten run/list/help/manifest/shell-integration` command
//!   parser, `run --detach`/stdio options and the `kitten @` passthrough hook.
//! * [`tui`]: dependency-free ANSI UI primitives, key parsing, list/fuzzy
//!   helpers and the [`tui::FakeTty`] test backend.
//! * [`icat`]: the Rust-native image kitten (`a=T,f=100`, RGB/RGBA, chunking,
//!   placement, scaling, clear and stdin).
//! * [`clipboard`] and [`clipboard_mime`]: OSC 52 text plus OSC 5522 file/MIME
//!   reads and writes, aliases and MIME selection.
//! * [`notify`]: chunked OSC 99 notifications, OSC 777/plain fallbacks and the
//!   `--wait` reply state machine (the desktop notification service protocol).
//! * [`hints`]: URL/path/line/word/regex/hyperlink matching, hint labels and
//!   launch-plan construction (no overlay).
//! * [`unicode_input`] and [`unicode_input_ui`]: the Unicode search model and
//!   an interactive ANSI table overlay.
//! * [`transfer`]: OSC 5113 file transfer, both directions, metadata,
//!   symlinks/hardlinks, chunking, zlib compression and permission bypass.
//! * [`ssh`]: ssh.conf host blocks, copy/env directives, ssh argument and
//!   connection parsing, bootstrap environment serialization, askpass and TOTP.
//! * [`themes`]: theme metadata/settings, local collections, search, config
//!   updates, OSC color escapes and a family picker.
//! * [`show_key`], [`query_terminal`] and [`resize_window`]: key display in
//!   legacy/kitty modes, DCS capability queries, and resize settings/commands.
//! * [`broadcast`], [`ask`], [`choose_files`], [`choose_fonts`] and
//!   [`command_palette`]: interactive engines and ANSI UIs.
//! * [`desktop_ui`]: appearance settings, xdg-desktop-portal installation data
//!   and a live session-bus `Settings`/`FileChooser` portal service.
//! * [`diff`] and [`pager`]: anchored diff/patch parsing, side-by-side text,
//!   search/scroll UI and a follow-capable input reader.
//! * [`dnd`]: OSC 72 drag/drop framing, URI/data-URI parsing and local copy.
//! * [`hyperlinked_grep`], [`panel`], [`quick_access_terminal`] and
//!   [`remote_file`]: ripgrep hyperlinks, layer-shell configuration/command
//!   construction, QAT command construction and remote-file helpers.
//! * [`remote`]: the `kitten @` client built on [`kitty_remote::Client`].
//! * [`env`]: shared `KITTY_WINDOW_ID`/`KITTY_LISTEN_ON`/`KITTY_PUBLIC_KEY`
//!   and `KITTY_KITTEN_*` environment parsing.
//!
//! # Explicit non-goals / limitations
//!
//! * No Python runtime is embedded. Python kittens are launched with an
//!   external interpreter (or directly through their shebang).
//! * Image/video previews and syntax highlighting are not ported.
//! * The transfer engine now supports both simple streams and rsync block
//!   deltas. Delta mode is only used for regular files above 4 KiB.
//! * The desktop-ui portal service is live on the session bus. The file chooser
//!   runs the native `choose-files` kitten; drawing an actual layer-shell panel
//!   remains the GUI's responsibility.
//! * The SSH kitten builds and hands off the bootstrap tar through shared
//!   memory and implements the askpass/TOTP flow. The OpenSSH control-master
//!   optimisation and remote-control forwarding are represented in the launch
//!   plan but are not applied by the minimal CLI wrapper.
//! * Theme ZIP cache downloads and the upstream theme repository are left to
//!   the caller; local `.conf` theme directories are supported.
//! * No remote-control socket server is started. `kitten @` arguments are
//!   returned verbatim by [`cli`]; [`remote`] connects to an existing kitty
//!   socket.
//!
//! # Example
//!
//! ```no_run
//! use kitty_kittens::{DiscoveryConfig, KittenRegistry, LaunchOptions};
//! use kitty_kittens::cli::{parse_kitten_command, execute_command, CommandAction};
//!
//! let registry = KittenRegistry::discover(&DiscoveryConfig::from_env());
//! let command = parse_kitten_command(["run", "hints", "--"])?;
//! if let CommandAction::Run(plan) = execute_command(&registry, command, &LaunchOptions::default())? {
//!     plan.spawn()?.wait()?;
//! }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

pub mod ask;
pub mod broadcast;
pub mod choose_files;
pub mod choose_fonts;
pub mod cli;
pub mod clipboard;
pub mod clipboard_mime;
pub mod command_palette;
pub mod desktop_ui;
pub mod diff;
pub mod discovery;
pub mod dnd;
pub mod env;
pub mod error;
pub mod hints;
pub mod hyperlinked_grep;
pub mod icat;
pub mod launch;
pub mod manifest;
pub mod native;
pub mod notify;
pub mod pager;
pub mod panel;
pub mod protocol;
pub mod query_terminal;
pub mod quick_access_terminal;
mod regex_lite;
pub mod remote;
pub mod remote_file;
pub mod resize_window;
pub mod rsync;
pub mod shell_integration;
pub mod shm;
pub mod show_key;
pub mod ssh;
pub mod terminal;
pub mod themes;
pub mod transfer;
pub mod tui;
pub mod unicode_input;
pub mod unicode_input_ui;
mod unicode_table;
mod xxh3;

pub use cli::{
    execute_args, execute_command, parse_kitten_command, AtInvocation, CommandAction,
    KittenCommand, RunOptions, USAGE,
};
pub use clipboard::{
    base64_chunks, decode_osc52_base64, decode_osc52_reply, encode_osc52_read_request,
    encode_osc52_write, encode_osc52_write_frames, parse_clipboard_args, run_clipboard,
    run_clipboard_cli, ClipboardAction, ClipboardAction as ClipboardMode, ClipboardError,
    ClipboardOptions, ClipboardTarget, CLIPBOARD_USAGE,
    DEFAULT_CHUNK_SIZE as CLIPBOARD_DEFAULT_CHUNK_SIZE,
};
pub use desktop_ui::{
    serve_portal, ChooserLauncher, FileChooserRequest, FileChooserResult, PortalConfig,
};
pub use discovery::{
    validate_name, BuiltinKitten, DiscoveryConfig, DiscoveryDiagnostic, KittenEntry,
    KittenRegistry, KittenSource, SearchRoot, BUILTIN_KITTENS,
};
pub use env::{parse_window_id, KittenEnvironment};
pub use error::{Error, Result};
pub use hints::{
    assign_labels, build_launch_plans, decode_hint, encode_hint, generate_hints, hints_to_skip,
    join_hints, line_column, parse_hints_args, run_hints_cli, Hint, HintAction, HintError,
    HintLaunchPlan, HintOptions, HintResult, HintType, HintsCommand, Mark, TrailingSpace,
    DEFAULT_ALPHABET, DEFAULT_REGEX, HINTS_USAGE,
};
pub use icat::{
    clear_frames, encode_png, encode_png_frames, parse_icat_args, run_icat, Align, FitMode,
    IcatArgs, IcatError, IcatOptions, OutputFormat, Place, Scale, StdinMode, WindowSize,
    DEFAULT_CHUNK_SIZE, FALLBACK_WINDOW, ICAT_USAGE,
};
pub use launch::{
    build_launch, BootstrapEnv, LaunchOptions, LaunchPlan, PythonInterpreter, StdioMode,
};
pub use manifest::{KittenKind, KittenManifest, MANIFEST_FILENAMES};
pub use native::{is_native_kitten, native_kitten, NativeKitten, NATIVE_KITTENS};
pub use notify::{
    encode_notification, encode_osc777, encode_osc99, encode_osc99_bytes, encode_plain,
    is_valid_identifier, parse_notify_args, parse_notify_reply, random_identifier, run_notify,
    run_notify_cli, Notification, NotificationFallback, NotificationReply, NotifyError,
    NotifyOptions, NotifyOutcome, ReplyAction, ReplyTracker, Urgency, NOTIFY_USAGE,
};
pub use panel::{build_panel_launch, PanelLaunch};
pub use protocol::{
    dispatch_event, encode_kitten_result, encode_overlay, encode_overlay_ready, encode_print,
    encode_remote_command, encode_remote_command_value, encode_title, parse_dcs_payload,
    sanitize_print, DcsFramer, KittenDcs, KittenFramer, KittenHost, KittenProtocol, OverlaySpec,
    CMD_PREFIX, DCS_START, KITTEN_RESULT_PREFIX, OVERLAY_PREFIX, OVERLAY_READY_PREFIX,
    PRINT_PREFIX, TITLE_PREFIX,
};
pub use quick_access_terminal::{build_quick_access_launch, QuickAccessLaunch};
pub use remote::{
    build_request, parse_remote_args, run_remote, run_remote_options, RemoteError, RemoteOptions,
    REMOTE_USAGE,
};
pub use remote_file::{
    get_editor, handle_remote_file_action, CommandOutput, CommandRunner, CommandSpec, SystemRunner,
};
pub use rsync::{
    apply_delta, apply_delta_with_block_size, create_delta, create_signature, parse_signature,
    BlockHash, Signature,
};
pub use shell_integration::{
    as_fish_str_literal, as_str_literal, discover_shell_integration_dir,
    discover_shell_integration_dir_from, install_snippets, serialize_env, serialize_env_for_path,
    shell_integration_snippet, Shell, ShellHandoff, ShellIntegration, ShellIntegrationOptions,
};
pub use shm::SharedMemory;
pub use ssh::{
    askpass_response, build_bootstrap_plan, build_bootstrap_plan_with_options,
    create_bootstrap_data_shm, encode_ssh_data_request, make_bootstrap_tar, perform_askpass,
    prepare_ssh_session, run_askpass_cli, AskpassHost, AskpassRequest, BootstrapOptions,
    BootstrapPlan, BootstrapResources, SshSessionPlan, TtyAskpassHost,
};
pub use terminal::{
    InteractiveTransport, ScriptedTransport, StdioTransport, DEFAULT_REPLY_TIMEOUT,
};
pub use unicode_input::{
    decode_index, default_symbols, emoticons, encode_index, entries, is_emoji_presentation_base,
    is_valid_codepoint, load_favorites, name_for, parse_codepoint, parse_favorites,
    parse_unicode_input_args, resolve_char, run_unicode_input_cli, save_favorites, search_by_name,
    serialize_favorites, EmojiVariation, Key, KeyOutcome, Mode, UnicodeEntry, UnicodeInputCommand,
    UnicodeInputError, UnicodeInputState, UNICODE_INPUT_USAGE,
};
