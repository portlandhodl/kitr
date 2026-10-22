// SPDX-License-Identifier: GPL-3.0-or-later
//! The `kitten` binary: native launcher, native kittens, remote control and
//! shell integration snippets.

use std::ffi::OsString;
use std::io::{self, IsTerminal, Read, Write};
use std::process::ExitCode;

use kitty_kittens::cli::{execute_command, parse_kitten_command, CommandAction};
use kitty_kittens::clipboard::{parse_clipboard_args, run_clipboard, ClipboardAction};
use kitty_kittens::env::KittenEnvironment;
use kitty_kittens::hints::run_hints_cli;
use kitty_kittens::icat::{parse_icat_args, run_icat, IcatArgs, ICAT_USAGE};
use kitty_kittens::launch::{BootstrapEnv, LaunchOptions};
use kitty_kittens::native::native_kitten;
use kitty_kittens::notify::run_notify_cli;
use kitty_kittens::remote::{parse_remote_args, run_remote_options};
use kitty_kittens::shell_integration::{discover_shell_integration_dir, install_snippets, Shell};
use kitty_kittens::terminal::StdioTransport;
use kitty_kittens::unicode_input::run_unicode_input_cli;
use kitty_kittens::{DiscoveryConfig, KittenRegistry};

fn io_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn print_shell_integration(args: &[String]) -> Result<ExitCode, String> {
    let value = args.first().map(String::as_str).unwrap_or("");
    let inline = value
        .strip_prefix("--shell-integration=")
        .or_else(|| value.strip_prefix("shell-integration="));
    let name = inline.or_else(|| match value {
        "--shell-integration" | "shell-integration" => args.get(1).map(String::as_str),
        _ => None,
    });
    let name = name.filter(|value| !value.is_empty());
    let shell = match name {
        Some(name) => Some(
            Shell::parse(name)
                .ok_or_else(|| format!("unsupported shell: {name} (expected bash, zsh or fish)"))?,
        ),
        None => None,
    };
    let directory = discover_shell_integration_dir()
        .or_else(|| {
            std::env::var_os("KITTY_INSTALLATION_DIR")
                .map(|dir| std::path::PathBuf::from(dir).join("shell-integration"))
        })
        .ok_or_else(|| {
            "shell integration directory not found; set KITTY_SHELL_INTEGRATION_DIR or KITTY_INSTALLATION_DIR"
                .to_owned()
        })?;
    println!("{}", install_snippets(&directory, shell));
    Ok(ExitCode::SUCCESS)
}

fn run_native_icat(args: &[String]) -> Result<ExitCode, String> {
    let mut parsed: IcatArgs = parse_icat_args(args).map_err(io_error)?;
    if parsed.help {
        println!("{ICAT_USAGE}");
        return Ok(ExitCode::SUCCESS);
    }
    if parsed.options.window_id.is_none() {
        parsed.options.window_id = KittenEnvironment::from_process_env().window_id;
    }
    if parsed.files.is_empty()
        && parsed.options.stdin == kitty_kittens::icat::StdinMode::Auto
        && io::stdin().is_terminal()
    {
        parsed.options.stdin = kitty_kittens::icat::StdinMode::No;
    }
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut stdin = stdin.lock();
    let mut stdout = stdout.lock();
    run_icat(&parsed, &mut stdin, &mut stdout).map_err(io_error)?;
    stdout.flush().map_err(io_error)?;
    Ok(ExitCode::SUCCESS)
}

fn run_remote_command(args: &[String]) -> Result<ExitCode, String> {
    let environment = KittenEnvironment::from_process_env();
    let mut options = parse_remote_args(args).map_err(io_error)?;
    if options.stdin && options.command.as_deref() == Some("send-text") {
        let mut data = String::new();
        io::stdin().read_to_string(&mut data).map_err(io_error)?;
        options.rest = vec![data.trim_end_matches(['\r', '\n']).to_owned()];
        options.stdin = false;
    }
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    run_remote_options(&options, &environment, &mut stdout).map_err(io_error)?;
    stdout.flush().map_err(io_error)?;
    Ok(ExitCode::SUCCESS)
}

fn run_native_clipboard(args: &[String]) -> Result<ExitCode, String> {
    let opts = parse_clipboard_args(args).map_err(io_error)?;
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut stdin = stdin.lock();
    let mut stdout = stdout.lock();
    // Only the get path needs to read terminal replies. Using a reader thread
    // for filter mode would race with reading the copied text from stdin.
    let mut transport = if opts.action == ClipboardAction::Get {
        StdioTransport::new()
    } else {
        StdioTransport::with_io(io::empty(), io::stdout())
    };
    if opts.help {
        use kitty_kittens::clipboard::CLIPBOARD_USAGE;
        writeln!(stdout, "{CLIPBOARD_USAGE}").map_err(io_error)?;
        return Ok(ExitCode::SUCCESS);
    }
    run_clipboard(&opts, &mut transport, &mut stdin, &mut stdout).map_err(io_error)?;
    stdout.flush().map_err(io_error)?;
    Ok(ExitCode::SUCCESS)
}

fn run_native_notify(args: &[String]) -> Result<ExitCode, String> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut stdin = stdin.lock();
    let mut stdout = stdout.lock();
    let mut transport = StdioTransport::new();
    run_notify_cli(args, &mut transport, &mut stdout, &mut stdin).map_err(io_error)?;
    stdout.flush().map_err(io_error)?;
    Ok(ExitCode::SUCCESS)
}

fn run_native_hints(args: &[String]) -> Result<ExitCode, String> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut stdin = stdin.lock();
    let mut stdout = stdout.lock();
    run_hints_cli(args, &mut stdin, &mut stdout).map_err(io_error)?;
    stdout.flush().map_err(io_error)?;
    Ok(ExitCode::SUCCESS)
}

fn run_native_unicode_input(args: &[String]) -> Result<ExitCode, String> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut stdin = stdin.lock();
    let mut stdout = stdout.lock();
    run_unicode_input_cli(args, &mut stdin, &mut stdout).map_err(io_error)?;
    stdout.flush().map_err(io_error)?;
    Ok(ExitCode::SUCCESS)
}

/// Detect `kitten NAME`, `kitten +NAME`, `kitten run NAME` and
/// `kitten run -- NAME` when NAME is implemented natively.
fn native_invocation(args: &[String]) -> Option<(&'static str, &[String])> {
    let (name, rest) = match args.first().map(String::as_str) {
        Some("run") => match args.get(1).map(String::as_str) {
            Some("--") => (args.get(2).map(String::as_str)?, &args[3..]),
            Some(name) => (name, &args[2..]),
            None => return None,
        },
        Some(name) => (name, &args[1..]),
        None => return None,
    };
    let name = name.strip_prefix('+').unwrap_or(name);
    let native = native_kitten(name)?;
    Some((native.name, rest))
}

fn run_native(name: &str, args: &[String]) -> Result<ExitCode, String> {
    match name {
        "icat" => run_native_icat(args),
        "clipboard" => run_native_clipboard(args),
        "notify" => run_native_notify(args),
        "hints" => run_native_hints(args),
        "unicode_input" => run_native_unicode_input(args),
        other => Err(format!("missing native dispatcher for {other}")),
    }
}

fn real_main(args: Vec<String>) -> Result<ExitCode, String> {
    if let Some((name, rest)) = native_invocation(&args) {
        return run_native(name, rest);
    }
    if args.first().map(String::as_str) == Some("@") {
        return run_remote_command(&args[1..]);
    }
    if matches!(
        args.first().map(String::as_str),
        Some("--shell-integration") | Some("shell-integration")
    ) || args.first().is_some_and(|arg| {
        arg.starts_with("--shell-integration=") || arg.starts_with("shell-integration=")
    }) {
        return print_shell_integration(&args);
    }

    let registry = KittenRegistry::discover(&DiscoveryConfig::from_env());
    let command = parse_kitten_command(args).map_err(io_error)?;
    let launch_options = LaunchOptions {
        bootstrap: BootstrapEnv::from_process_env(),
        ..LaunchOptions::default()
    };
    match execute_command(&registry, command, &launch_options).map_err(io_error)? {
        CommandAction::Print(text) => {
            println!("{text}");
            Ok(ExitCode::SUCCESS)
        }
        CommandAction::Passthrough(invocation) => run_remote_command(invocation.args()),
        CommandAction::Run(plan) => {
            if plan.detach {
                plan.spawn_detached().map_err(io_error)?;
                return Ok(ExitCode::SUCCESS);
            }
            let status = plan.status().map_err(io_error)?;
            Ok(if status.success() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            })
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(OsString::into_string)
        .collect::<Result<Vec<_>, _>>()
        .unwrap_or_else(|_| {
            eprintln!("kitten: arguments must be valid UTF-8");
            std::process::exit(1);
        });
    match real_main(args) {
        Ok(code) => code,
        Err(message) => {
            eprintln!("kitten: {message}");
            ExitCode::FAILURE
        }
    }
}
