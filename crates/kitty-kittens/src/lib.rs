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
//!   directory and generation of the bash/zsh/fish handoff environment.
//! * [`cli`]: the library-only `kitten run/list/help/manifest` command parser
//!   and the `kitten @` passthrough hook.
//!
//! # Explicit non-goals
//!
//! * No Python runtime is embedded. Python kittens are launched with an
//!   external interpreter (or directly through their shebang).
//! * No remote-control socket server is started. `kitten @` arguments are
//!   returned verbatim for the caller's client layer.
//! * Shell integration does not inject code into a running shell; it only
//!   computes the handoff environment and serialized environment commands.
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

pub mod cli;
pub mod discovery;
pub mod error;
pub mod launch;
pub mod manifest;
pub mod protocol;
pub mod shell_integration;

pub use cli::{
    execute_args, execute_command, parse_kitten_command, AtInvocation, CommandAction,
    KittenCommand, USAGE,
};
pub use discovery::{
    validate_name, BuiltinKitten, DiscoveryConfig, DiscoveryDiagnostic, KittenEntry,
    KittenRegistry, KittenSource, SearchRoot, BUILTIN_KITTENS,
};
pub use error::{Error, Result};
pub use launch::{
    build_launch, BootstrapEnv, LaunchOptions, LaunchPlan, PythonInterpreter, StdioMode,
};
pub use manifest::{KittenKind, KittenManifest, MANIFEST_FILENAMES};
pub use protocol::{
    dispatch_event, encode_kitten_result, encode_overlay, encode_overlay_ready, encode_print,
    encode_remote_command, encode_remote_command_value, encode_title, parse_dcs_payload,
    sanitize_print, DcsFramer, KittenDcs, KittenFramer, KittenHost, KittenProtocol, OverlaySpec,
    CMD_PREFIX, DCS_START, KITTEN_RESULT_PREFIX, OVERLAY_PREFIX, OVERLAY_READY_PREFIX,
    PRINT_PREFIX, TITLE_PREFIX,
};
pub use shell_integration::{
    as_fish_str_literal, as_str_literal, discover_shell_integration_dir,
    discover_shell_integration_dir_from, serialize_env, serialize_env_for_path, Shell,
    ShellHandoff, ShellIntegration, ShellIntegrationOptions,
};
