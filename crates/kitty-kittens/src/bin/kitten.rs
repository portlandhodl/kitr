// SPDX-License-Identifier: GPL-3.0-or-later
//! The `kitten` binary: native launcher, native kittens, remote control and
//! shell integration snippets.

use std::ffi::OsString;
use std::io::{self, IsTerminal, Read, Write};
use std::process::ExitCode;

use kitty_kittens::cli::{execute_command, parse_kitten_command, CommandAction};
use kitty_kittens::clipboard::{parse_clipboard_args, run_clipboard, ClipboardAction};
use kitty_kittens::clipboard_mime::MimeInput;
use kitty_kittens::env::KittenEnvironment;
use kitty_kittens::hints::run_hints_cli;
use kitty_kittens::icat::{parse_icat_args, run_icat, IcatArgs, ICAT_USAGE};
use kitty_kittens::launch::{BootstrapEnv, LaunchOptions};
use kitty_kittens::native::native_kitten;
use kitty_kittens::notify::run_notify_cli;
use kitty_kittens::remote::{parse_remote_args, run_remote_options};
use kitty_kittens::shell_integration::{discover_shell_integration_dir, install_snippets, Shell};
use kitty_kittens::terminal::StdioTransport;
use kitty_kittens::tui::{StdTty, Tty};
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

fn open_tty() -> Result<StdTty, String> {
    StdTty::open().map_err(|err| format!("cannot open controlling terminal: {err}"))
}

fn home_dir() -> std::path::PathBuf {
    std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_default()
}

fn config_dir() -> std::path::PathBuf {
    if let Some(dir) = std::env::var_os("KITTY_CONFIG_DIRECTORY") {
        return std::path::PathBuf::from(dir);
    }
    if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME") {
        return std::path::PathBuf::from(dir).join("kitty");
    }
    home_dir().join(".config/kitty")
}

fn run_native_transfer(args: &[String]) -> Result<ExitCode, String> {
    use kitty_kittens::transfer::{self, TransferOptions};
    let (opts, rest) = transfer::parse_transfer_args(args).map_err(io_error)?;
    if opts.help {
        println!("{}", transfer::TRANSFER_USAGE);
        return Ok(ExitCode::SUCCESS);
    }
    let opts = TransferOptions {
        bypass: if opts.bypass.is_empty() {
            String::new()
        } else {
            transfer::read_bypass(&opts.bypass).map_err(io_error)?
        },
        ..opts
    };
    let cwd = std::env::current_dir().map_err(io_error)?;
    let home = home_dir();
    let mut transport = StdioTransport::new();
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    transfer::run_transfer(&opts, &rest, &cwd, &home, &mut transport, &mut stdout)
        .map_err(io_error)?;
    stdout.flush().map_err(io_error)?;
    Ok(ExitCode::SUCCESS)
}

fn run_native_ssh(args: &[String]) -> Result<ExitCode, String> {
    use kitty_kittens::ssh;
    let (cli_config, passthrough) = ssh::parse_ssh_cli(args).map_err(io_error)?;
    if cli_config.hostname == "__help__" {
        println!("{}", ssh::SSH_USAGE);
        return Ok(ExitCode::SUCCESS);
    }
    if passthrough.is_empty() {
        println!("{}", ssh::SSH_USAGE);
        return Ok(ExitCode::SUCCESS);
    }
    let (ssh_args, server_args, raw_passthrough, _) =
        ssh::parse_ssh_args(&passthrough, &["--kitten"]).map_err(io_error)?;
    if raw_passthrough || server_args.is_empty() {
        let mut argv = vec!["ssh".to_owned()];
        argv.extend(passthrough);
        let status = ssh::run_ssh_command(argv, std::env::vars().collect()).map_err(io_error)?;
        return Ok(if status == 0 {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        });
    }
    let destination = &server_args[0];
    let data = ssh::connection_data(&[vec!["ssh".to_owned()], passthrough.clone()].concat());
    let (username, hostname) = data
        .map(|data| {
            let (user, host) = data
                .hostname
                .split_once('@')
                .map_or(("", data.hostname.as_str()), |(user, host)| (user, host));
            (user.to_owned(), host.to_owned())
        })
        .unwrap_or_else(|| (String::new(), destination.clone()));
    let mut host = ssh::load_ssh_config().for_host(&hostname, &username);
    host.copy.extend(cli_config.copy.clone());
    host.env.extend(cli_config.env.clone());
    if cli_config.delegate != ssh::SshHostConfig::new().delegate {
        host.delegate = cli_config.delegate.clone();
    }
    if cli_config.askpass != ssh::SshHostConfig::new().askpass {
        host.askpass = cli_config.askpass.clone();
    }
    let mut control_conn =
        ssh::connection_data(&[vec!["ssh".to_owned()], passthrough.clone()].concat())
            .unwrap_or_else(|| ssh::SshConnectionData {
                binary: "ssh".to_owned(),
                hostname: destination.clone(),
                ..ssh::SshConnectionData::default()
            });
    control_conn.hostname = destination.clone();
    let pid = std::env::var("KITTY_PID")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or_else(std::process::id);
    control_conn.control_pid = pid;
    if host.share_connections {
        control_conn.ssh_kitten_cmdline = Vec::new();
        // Keep the master's ControlPath identical to the session command.
        control_conn.identity_file.clear();
    }
    if host.forward_remote_control {
        if !host.share_connections {
            return Err(
                "Cannot use forward_remote_control=yes without share_connections=yes as it relies on SSH Controlmasters"
                    .to_owned(),
            );
        }
        let listen_on = std::env::var("KITTY_LISTEN_ON").map_err(|_| {
            "forward_remote_control=yes requires KITTY_LISTEN_ON to be set by kitty".to_owned()
        })?;
        let mut runner = ssh::SystemSshRunner;
        let forwarded = ssh::forward_remote_control(&control_conn, &listen_on, &mut runner)
            .map_err(|error| error.to_string())?;
        eprintln!(
            "kitr: forwarding remote control on {}{}",
            forwarded.listen_on,
            if forwarded.recovered {
                " (recovered stale ControlMaster)"
            } else {
                ""
            }
        );
        host.env
            .push(format!("KITTY_LISTEN_ON {}", forwarded.listen_on));
    }
    let options = ssh::BootstrapOptions::default();
    let mut plan = ssh::prepare_ssh_session(
        &host,
        &ssh_args,
        &server_args,
        &std::collections::BTreeMap::new(),
        &options,
        &|key| std::env::var(key).ok(),
    )
    .map_err(io_error)?;
    if host.share_connections && !control_conn.is_ssh_kitten {
        plan.ssh_command
            .splice(1..1, ssh::connection_sharing_args(pid));
    }
    let Some(program) = plan.ssh_command.first() else {
        return Err("empty ssh command".to_owned());
    };
    let mut command = std::process::Command::new(program);
    command.args(&plan.ssh_command[1..]);
    for (key, value) in &plan.environment {
        command.env(key, value);
    }
    let status = command.status().map_err(io_error)?;
    Ok(if status.success() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn run_native_themes(args: &[String]) -> Result<ExitCode, String> {
    use kitty_kittens::themes;
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    let mut tty = if themes::parse_themes_args(args).name.is_none() && !wants_help(args) {
        Some(open_tty()?)
    } else {
        None
    };
    themes::run_themes_cli(
        args,
        tty.as_mut().map(|tty| tty as &mut dyn Tty),
        &mut stdout,
    )
    .map_err(io_error)?;
    stdout.flush().map_err(io_error)?;
    Ok(ExitCode::SUCCESS)
}

fn run_native_show_key(args: &[String]) -> Result<ExitCode, String> {
    use kitty_kittens::show_key;
    let opts = show_key::parse_show_key_args(args)?;
    if opts.help {
        println!("{}", show_key::SHOW_KEY_USAGE);
        return Ok(ExitCode::SUCCESS);
    }
    let mut tty = open_tty()?;
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    if opts.key_mode == show_key::KeyMode::Kitty {
        show_key::run_kitty_ui(&mut tty, &mut stdout).map_err(io_error)?;
    } else {
        show_key::run_legacy_ui(&mut tty, &opts, &mut stdout).map_err(io_error)?;
    }
    stdout.flush().map_err(io_error)?;
    Ok(ExitCode::SUCCESS)
}

fn run_native_query_terminal(args: &[String]) -> Result<ExitCode, String> {
    use kitty_kittens::query_terminal;
    let opts = query_terminal::parse_query_args(args)?;
    let mut transport = StdioTransport::new();
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    query_terminal::run_query(&opts, &mut transport, &mut stdout).map_err(io_error)?;
    stdout.flush().map_err(io_error)?;
    Ok(ExitCode::SUCCESS)
}

fn run_native_resize(args: &[String]) -> Result<ExitCode, String> {
    use kitty_kittens::resize_window;
    let opts = resize_window::parse_resize_args(args)?;
    if opts.help {
        println!("{}", resize_window::RESIZE_USAGE);
        return Ok(ExitCode::SUCCESS);
    }
    let path = config_dir().join("resize-window.json");
    let mut settings = resize_window::ResizeSettings::load(&path);
    if let Some(strategy) = opts.strategy {
        settings.strategy = strategy.as_str().to_owned();
    }
    let mut tty = open_tty()?;
    resize_window::run_resize_ui(&mut tty, &opts, &mut settings, &path).map_err(io_error)?;
    Ok(ExitCode::SUCCESS)
}

fn run_native_broadcast(args: &[String]) -> Result<ExitCode, String> {
    use kitty_kittens::broadcast;
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    if wants_help(args) {
        writeln!(stdout, "{}", broadcast::BROADCAST_USAGE).map_err(io_error)?;
        return Ok(ExitCode::SUCCESS);
    }
    let mut tty = open_tty()?;
    broadcast::run_broadcast_cli(args, &mut tty, &mut stdout)?;
    stdout.flush().map_err(io_error)?;
    Ok(ExitCode::SUCCESS)
}

fn run_native_ask(args: &[String]) -> Result<ExitCode, String> {
    use kitty_kittens::ask;
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    if wants_help(args) {
        writeln!(stdout, "{}", ask::ASK_USAGE).map_err(io_error)?;
        return Ok(ExitCode::SUCCESS);
    }
    let mut tty = open_tty()?;
    ask::run_ask_cli(args, &mut tty, &mut stdout)?;
    stdout.flush().map_err(io_error)?;
    Ok(ExitCode::SUCCESS)
}

fn run_native_choose_files(args: &[String]) -> Result<ExitCode, String> {
    use kitty_kittens::choose_files;
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    let mut tty = if wants_help(args) {
        None
    } else {
        Some(open_tty()?)
    };
    choose_files::run_choose_files_cli(
        args,
        tty.as_mut().map(|tty| tty as &mut dyn Tty),
        &mut stdout,
    )?;
    stdout.flush().map_err(io_error)?;
    Ok(ExitCode::SUCCESS)
}

fn run_native_choose_fonts(args: &[String]) -> Result<ExitCode, String> {
    use kitty_kittens::choose_fonts;
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    let mut tty = if wants_help(args) {
        None
    } else {
        Some(open_tty()?)
    };
    choose_fonts::run_choose_fonts_cli(
        args,
        tty.as_mut().map(|tty| tty as &mut dyn Tty),
        &mut stdout,
    )?;
    stdout.flush().map_err(io_error)?;
    Ok(ExitCode::SUCCESS)
}

fn run_native_command_palette(args: &[String]) -> Result<ExitCode, String> {
    use kitty_kittens::command_palette;
    if args.iter().any(|arg| arg == "-h" || arg == "--help") {
        println!("{}", command_palette::COMMAND_PALETTE_USAGE);
        return Ok(ExitCode::SUCCESS);
    }
    let mut text = String::new();
    io::stdin().read_to_string(&mut text).map_err(io_error)?;
    let data = command_palette::InputData::from_json(&text).map_err(|err| err.to_string())?;
    let mut tty = open_tty()?;
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    command_palette::run_command_palette_cli(args, &data, Some(&mut tty), &mut stdout)?;
    stdout.flush().map_err(io_error)?;
    Ok(ExitCode::SUCCESS)
}

fn run_native_desktop_ui(args: &[String]) -> Result<ExitCode, String> {
    use kitty_kittens::desktop_ui;
    let exe = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("kitten"));
    let text = desktop_ui::run_desktop_ui_command(args, &exe)?;
    if !text.is_empty() {
        println!("{text}");
    }
    Ok(ExitCode::SUCCESS)
}

fn run_native_diff(args: &[String]) -> Result<ExitCode, String> {
    use kitty_kittens::diff;
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    let mut tty = open_tty().ok();
    diff::run_diff_cli(
        args,
        tty.as_mut().map(|tty| tty as &mut dyn Tty),
        &mut stdout,
    )?;
    stdout.flush().map_err(io_error)?;
    Ok(ExitCode::SUCCESS)
}

fn run_native_dnd(args: &[String]) -> Result<ExitCode, String> {
    use kitty_kittens::dnd;
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    let mut tty = if wants_help(args) {
        None
    } else {
        Some(open_tty()?)
    };
    dnd::run_dnd_cli(
        args,
        tty.as_mut().map(|tty| tty as &mut dyn Tty),
        &mut stdout,
    )?;
    stdout.flush().map_err(io_error)?;
    Ok(ExitCode::SUCCESS)
}

fn run_native_hyperlinked_grep(args: &[String]) -> Result<ExitCode, String> {
    use kitty_kittens::hyperlinked_grep;
    if args.iter().any(|arg| arg == "-h" || arg == "--help") {
        println!("{}", hyperlinked_grep::HYPERLINKED_GREP_USAGE);
        return Ok(ExitCode::SUCCESS);
    }
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    let status = hyperlinked_grep::run_rg(args, &mut stdout).map_err(io_error)?;
    stdout.flush().map_err(io_error)?;
    Ok(if status == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn run_native_pager(args: &[String]) -> Result<ExitCode, String> {
    use kitty_kittens::pager;
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    if wants_help(args) {
        writeln!(stdout, "{}", pager::PAGER_USAGE).map_err(io_error)?;
        return Ok(ExitCode::SUCCESS);
    }
    let mut tty = open_tty()?;
    pager::run_pager_cli(args, &mut tty, &mut stdout)?;
    stdout.flush().map_err(io_error)?;
    Ok(ExitCode::SUCCESS)
}

fn exec_kitty(argv: Vec<String>) -> Result<ExitCode, String> {
    let Some(program) = argv.first() else {
        return Err("empty kitty command".to_owned());
    };
    let status = std::process::Command::new(program)
        .args(&argv[1..])
        .status()
        .map_err(io_error)?;
    Ok(if status.success() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn host_terminal_exe() -> std::path::PathBuf {
    if let Some(exe) = std::env::var_os("KITTY_EXE").or_else(|| std::env::var_os("KITR_EXE")) {
        return std::path::PathBuf::from(exe);
    }
    if let Ok(exe) = std::env::current_exe() {
        for name in ["kitr", "kitty"] {
            let candidate = exe.with_file_name(name);
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    std::path::PathBuf::from("kitty")
}

fn run_native_panel(args: &[String]) -> Result<ExitCode, String> {
    use kitty_kittens::panel;
    let opts = panel::parse_panel_args(args)?;
    if opts.help {
        println!("{}", panel::PANEL_USAGE);
        return Ok(ExitCode::SUCCESS);
    }
    exec_kitty(panel::build_panel_command(&host_terminal_exe(), &opts))
}

fn run_native_quick_access(args: &[String]) -> Result<ExitCode, String> {
    use kitty_kittens::quick_access_terminal;
    let opts = quick_access_terminal::parse_quick_access_args(args)?;
    if opts.help {
        println!("{}", quick_access_terminal::QUICK_ACCESS_USAGE);
        return Ok(ExitCode::SUCCESS);
    }
    let config = quick_access_terminal::load_config(&opts, &quick_access_terminal::config_paths())?;
    exec_kitty(quick_access_terminal::build_quick_access_command(
        &host_terminal_exe(),
        &config,
        &opts,
        &config_dir(),
    ))
}

fn run_native_remote_file(args: &[String]) -> Result<ExitCode, String> {
    use kitty_kittens::remote_file;
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    let result = remote_file::run_remote_file_cli(args, &mut stdout)?;
    stdout.flush().map_err(io_error)?;
    if let Some(path) = result {
        println!("{path}");
    }
    Ok(ExitCode::SUCCESS)
}

fn run_native_clipboard_mime(args: &[String]) -> Result<ExitCode, String> {
    use kitty_kittens::clipboard_mime::{self, MimeOutput};
    if wants_help(args) {
        println!("{}", clipboard_mime::CLIPBOARD_MIME_USAGE);
        return Ok(ExitCode::SUCCESS);
    }
    let mut get = false;
    let mut primary = false;
    let mut wait = false;
    let mut mimes: Vec<String> = Vec::new();
    let mut aliases: Vec<String> = Vec::new();
    let mut password = String::new();
    let mut human_name = String::new();
    let mut files: Vec<String> = Vec::new();
    let mut index = 0usize;
    while index < args.len() {
        let arg = &args[index];
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
            "-g" | "--get-clipboard" => get = true,
            "-p" | "--use-primary" => primary = true,
            "-m" | "--mime" => mimes.push(take()),
            "-a" | "--alias" => aliases.push(take()),
            "--wait-for-completion" => wait = true,
            "--password" => password = take(),
            "--human-name" => human_name = take(),
            "-h" | "--help" => {
                println!("{}", clipboard_mime::CLIPBOARD_MIME_USAGE);
                return Ok(ExitCode::SUCCESS);
            }
            other if !other.starts_with('-') => files.push(other.to_owned()),
            other => return Err(format!("unknown clipboard option: {other}")),
        }
        index += 1;
    }
    let mut transport = StdioTransport::new();
    if get {
        let mut outputs = Vec::new();
        for (position, file) in files.iter().enumerate() {
            outputs.push(MimeOutput::new(
                std::path::PathBuf::from(file),
                mimes.get(position).cloned(),
            ));
        }
        if outputs.is_empty() {
            outputs.push(MimeOutput::new(
                std::path::PathBuf::from("/dev/stdout"),
                mimes.first().cloned(),
            ));
        }
        clipboard_mime::read_mime_outputs(
            &outputs,
            primary,
            clipboard_mime::DEFAULT_WAIT_TIMEOUT,
            &mut transport,
        )?;
    } else {
        let mut inputs = Vec::new();
        if files.is_empty() {
            files.push("/dev/stdin".to_owned());
        }
        for (position, file) in files.iter().enumerate() {
            inputs.push(MimeInput::new(
                std::path::PathBuf::from(file),
                mimes.get(position).cloned(),
            )?);
        }
        let alias_map = clipboard_mime::parse_aliases(&aliases)?;
        let inputs = clipboard_mime::prepare_inputs(inputs, &alias_map);
        clipboard_mime::write_mime_inputs(
            &inputs,
            primary,
            &password,
            &human_name,
            wait,
            &mut transport,
            clipboard_mime::DEFAULT_WAIT_TIMEOUT,
        )?;
    }
    Ok(ExitCode::SUCCESS)
}

fn clipboard_is_mime_mode(args: &[String]) -> bool {
    args.iter().any(|arg| {
        arg == "--mime"
            || arg.starts_with("--mime=")
            || arg == "-m"
            || arg == "--alias"
            || arg.starts_with("--alias=")
            || arg == "--get-clipboard"
            || arg == "-g"
    })
}

fn wants_help(args: &[String]) -> bool {
    args.iter().any(|arg| arg == "-h" || arg == "--help")
}

fn run_native_unicode_input(args: &[String]) -> Result<ExitCode, String> {
    use kitty_kittens::unicode_input::{self, Mode};
    use kitty_kittens::unicode_input_ui::{self, UnicodeUiOptions};
    let interactive = args.is_empty()
        || args
            .iter()
            .any(|arg| arg == "--ui" || arg == "--interactive");
    if interactive && StdTty::is_terminal() {
        let mut mode = None;
        let mut variation = unicode_input::EmojiVariation::None;
        let mut index = 0usize;
        while index < args.len() {
            let arg = &args[index];
            if let Some(value) = arg.strip_prefix("--tab=") {
                mode = Mode::parse(value);
            } else if arg == "--tab" {
                index += 1;
                mode = args.get(index).and_then(|value| Mode::parse(value));
            } else if let Some(value) = arg.strip_prefix("--emoji-variation=") {
                variation = unicode_input::EmojiVariation::parse(value).unwrap_or(variation);
            }
            index += 1;
        }
        let favorites_path = config_dir().join("unicode-input-favorites.conf");
        let favorites = unicode_input::load_favorites(&favorites_path).ok();
        let mut tty = open_tty()?;
        let options = UnicodeUiOptions {
            initial_mode: mode,
            emoji_variation: variation,
        };
        if let Some(text) = unicode_input_ui::run_unicode_input_ui(&mut tty, &options, favorites)
            .map_err(io_error)?
        {
            println!("{text}");
        }
        return Ok(ExitCode::SUCCESS);
    }
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
        "clipboard" if clipboard_is_mime_mode(args) => run_native_clipboard_mime(args),
        "clipboard" => run_native_clipboard(args),
        "notify" => run_native_notify(args),
        "hints" => run_native_hints(args),
        "unicode_input" => run_native_unicode_input(args),
        "transfer" => run_native_transfer(args),
        "ssh" => run_native_ssh(args),
        "themes" => run_native_themes(args),
        "show-key" => run_native_show_key(args),
        "query-terminal" => run_native_query_terminal(args),
        "resize-window" => run_native_resize(args),
        "broadcast" => run_native_broadcast(args),
        "ask" => run_native_ask(args),
        "choose-files" => run_native_choose_files(args),
        "choose-fonts" => run_native_choose_fonts(args),
        "command-palette" => run_native_command_palette(args),
        "desktop-ui" => run_native_desktop_ui(args),
        "diff" => run_native_diff(args),
        "dnd" => run_native_dnd(args),
        "hyperlinked-grep" => run_native_hyperlinked_grep(args),
        "pager" => run_native_pager(args),
        "panel" => run_native_panel(args),
        "quick-access-terminal" => run_native_quick_access(args),
        "remote-file" => run_native_remote_file(args),
        other => Err(format!("missing native dispatcher for {other}")),
    }
}

fn real_main(args: Vec<String>) -> Result<ExitCode, String> {
    if std::env::var("KITTY_KITTEN_RUN_MODULE").as_deref() == Ok("ssh_askpass") {
        let code = kitty_kittens::ssh::run_askpass_cli();
        return Ok(if code == 0 {
            ExitCode::SUCCESS
        } else {
            ExitCode::from(code as u8)
        });
    }
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
