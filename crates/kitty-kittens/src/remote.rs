// SPDX-License-Identifier: GPL-3.0-or-later
//! `kitten @` remote-control client.
//!
//! This is the same thin JSON front-end used by `kitr @`: it parses the
//! kitty remote-control command line, builds the `{"cmd", "version", "payload"}`
//! request, and sends it through [`kitty_remote::Client`]. The socket and
//! encryption key come from [`KittenEnvironment`](crate::env::KittenEnvironment)
//! (`KITTY_LISTEN_ON`, `KITTY_PUBLIC_KEY` and their `KITTY_KITTEN_*` overrides)
//! unless `--to`/`--password` are supplied.
//!
//! Linux abstract sockets (`unix:@name`) are supported. A process with no
//! `KITTY_LISTEN_ON` and no controlling-terminal socket fallback reports an
//! error; the terminal socket lookup remains a host integration gap.

use std::io::{self, Write};

use kitty_remote::base64::decode_strict;
use kitty_remote::client::{parse_public_key_env, Client, ClientConfig};
use kitty_remote::json::Value;
use kitty_remote::KITTY_COMPAT_VERSION;

use crate::env::KittenEnvironment;

/// Usage text for `kitten @`.
pub const REMOTE_USAGE: &str = "\
Usage: kitten @ [--to=unix:/path|unix:@name] COMMAND [ARGS...]

Commands: get-text, send-text, send-key, resize-window, close-window,
          close-tab, focus-window, set-window-title, get-colors,
          set-colors, screenshot, version, capabilities

Options:
  --to ADDR          Remote socket; defaults to $KITTY_LISTEN_ON
  --password VALUE   Remote-control password
  --password-env VAR Read the password from VAR (default: KITTY_RC_PASSWORD)
  --extent EXTENT    get-text extent: screen, all, selection (default: screen)
  --match SPEC       Window match expression
  --axis AXIS        resize-window axis: horizontal, vertical, reset
  --increment N      resize-window cell increment (default: 2)
  --format FMT       screenshot format: png or ppm (default: png)
  --output FILE      screenshot output path (client side)
  --stdin            read send-text data from stdin
  --no-output        suppress textual responses
  -h, --help";

/// Parsed `kitten @` options.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RemoteOptions {
    pub target: Option<String>,
    pub command: Option<String>,
    pub rest: Vec<String>,
    pub extent: String,
    pub format: String,
    pub output: Option<String>,
    pub match_spec: Option<String>,
    pub axis: String,
    pub increment: i64,
    pub title: Option<String>,
    pub keys: Vec<String>,
    pub stdin: bool,
    pub password: Option<String>,
    pub password_env: Option<String>,
    pub no_output: bool,
    pub help: bool,
}

impl RemoteOptions {
    fn with_defaults() -> Self {
        Self {
            extent: "screen".to_owned(),
            format: "png".to_owned(),
            axis: "horizontal".to_owned(),
            increment: 2,
            ..Self::default()
        }
    }
}

/// Errors from the remote-control client.
#[derive(Debug)]
#[non_exhaustive]
pub enum RemoteError {
    Io(io::Error),
    Remote(kitty_remote::Error),
    InvalidOption(String),
    Message(String),
}

impl std::fmt::Display for RemoteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "I/O error: {error}"),
            Self::Remote(error) => write!(f, "remote control error: {error}"),
            Self::InvalidOption(message) => f.write_str(message),
            Self::Message(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for RemoteError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Remote(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for RemoteError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<kitty_remote::Error> for RemoteError {
    fn from(value: kitty_remote::Error) -> Self {
        Self::Remote(value)
    }
}

fn invalid(message: impl Into<String>) -> RemoteError {
    RemoteError::InvalidOption(message.into())
}

/// Parse `kitten @` arguments (excluding the `@`).
pub fn parse_remote_args<I, S>(args: I) -> Result<RemoteOptions, RemoteError>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let args: Vec<String> = args.into_iter().map(Into::into).collect();
    let mut out = RemoteOptions::with_defaults();
    let mut index = 0usize;
    let mut options_done = false;
    while index < args.len() {
        let arg = &args[index];
        if options_done || !arg.starts_with('-') || arg == "-" {
            if out.command.is_none() {
                out.command = Some(arg.clone());
            } else {
                out.rest.push(arg.clone());
            }
            index += 1;
            continue;
        }
        if arg == "--" {
            options_done = true;
            index += 1;
            continue;
        }
        let (name, inline) = match arg.split_once('=') {
            Some((name, value)) => (name, Some(value.to_owned())),
            None => (arg.as_str(), None),
        };
        let mut take = |option: &'static str| -> Result<String, RemoteError> {
            if let Some(value) = &inline {
                return Ok(value.clone());
            }
            index += 1;
            args.get(index)
                .cloned()
                .ok_or_else(|| invalid(format!("{option} requires a value")))
        };
        match name {
            "--to" => out.target = Some(take("--to")?),
            "--extent" => out.extent = take("--extent")?,
            "--format" => out.format = take("--format")?,
            "--output" => out.output = Some(take("--output")?),
            "--match" => out.match_spec = Some(take("--match")?),
            "--axis" => out.axis = take("--axis")?,
            "--increment" => {
                out.increment = take("--increment")?
                    .parse()
                    .map_err(|_| invalid("invalid --increment"))?
            }
            "--title" => out.title = Some(take("--title")?),
            "--key" => out.keys.push(take("--key")?),
            "--password" => out.password = Some(take("--password")?),
            "--password-env" => out.password_env = Some(take("--password-env")?),
            "--stdin" => out.stdin = true,
            "--no-output" => out.no_output = true,
            "-h" | "--help" => out.help = true,
            other => return Err(invalid(format!("unknown remote control option: {other}"))),
        }
        index += 1;
    }
    Ok(out)
}

fn payload(fields: impl IntoIterator<Item = (&'static str, Value)>) -> Value {
    Value::object_from(fields)
}

fn request(command: &str, payload: Value) -> Value {
    Value::object_from([
        ("cmd", Value::String(command.to_string())),
        ("version", Value::String(KITTY_COMPAT_VERSION.to_string())),
        ("payload", payload),
    ])
}

/// Build the wire request for a parsed invocation.
pub fn build_request(args: &RemoteOptions) -> Result<Value, RemoteError> {
    let command = args.command.as_deref().unwrap_or("version");
    let match_value = || {
        args.match_spec
            .clone()
            .map(Value::String)
            .unwrap_or(Value::Null)
    };
    Ok(match command {
        "get-text" => request(
            "get-text",
            payload([
                ("extent", Value::String(args.extent.clone())),
                ("match", match_value()),
                ("ansi", Value::Bool(false)),
                ("cursor", Value::Bool(false)),
                ("clear_selection", Value::Bool(false)),
            ]),
        ),
        "send-text" => {
            let data = args.rest.join(" ");
            request(
                "send-text",
                payload([
                    ("data", Value::String(format!("text:{data}"))),
                    ("match", match_value()),
                    ("bracketed_paste", Value::String("auto".into())),
                ]),
            )
        }
        "send-key" => {
            let keys: Vec<Value> = if args.keys.is_empty() {
                args.rest.iter().cloned().map(Value::String).collect()
            } else {
                args.keys.iter().cloned().map(Value::String).collect()
            };
            request(
                "send-key",
                payload([("keys", Value::Array(keys)), ("match", match_value())]),
            )
        }
        "resize-window" => request(
            "resize-window",
            payload([
                ("match", match_value()),
                ("axis", Value::String(args.axis.clone())),
                ("increment", Value::from(args.increment)),
            ]),
        ),
        "close-window" => request(
            "close-window",
            payload([
                ("match", match_value()),
                ("ignore_no_match", Value::Bool(true)),
            ]),
        ),
        "close-tab" => request(
            "close-tab",
            payload([
                ("match", match_value()),
                ("ignore_no_match", Value::Bool(true)),
            ]),
        ),
        "focus-window" => request("focus-window", payload([("match", match_value())])),
        "set-window-title" => request(
            "set-window-title",
            payload([
                ("match", match_value()),
                (
                    "title",
                    args.title.clone().map(Value::String).unwrap_or(Value::Null),
                ),
            ]),
        ),
        "get-colors" => request("get-colors", Value::object()),
        "set-colors" => {
            let mut colors = Value::object();
            for item in &args.rest {
                if let Some((name, value)) = item.split_once('=') {
                    if let Ok(number) = u32::from_str_radix(value.trim_start_matches('#'), 16) {
                        colors.set(name.to_string(), Value::from(number));
                    }
                }
            }
            request(
                "set-colors",
                payload([("colors", colors), ("all", Value::Bool(true))]),
            )
        }
        "screenshot" => request(
            "screenshot",
            payload([
                ("match", match_value()),
                ("format", Value::String(args.format.clone())),
                (
                    "output_path",
                    Value::String(args.output.clone().unwrap_or_default()),
                ),
            ]),
        ),
        "version" | "capabilities" => request(command, Value::object()),
        other => return Err(invalid(format!("unknown remote control command: {other}"))),
    })
}

#[cfg(target_os = "linux")]
fn abstract_addr(name: &str) -> io::Result<std::os::unix::net::SocketAddr> {
    use std::os::linux::net::SocketAddrExt;
    std::os::unix::net::SocketAddr::from_abstract_name(name.as_bytes())
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))
}

fn connect(
    target: &str,
    password: Option<String>,
    public_key: Option<&str>,
) -> Result<Client, RemoteError> {
    let target = target.strip_prefix("unix:").unwrap_or(target).to_string();
    let config = ClientConfig {
        password,
        ..ClientConfig::default()
    };
    let public_key = match public_key {
        Some(value) => Some(
            parse_public_key_env(value).map_err(|error| RemoteError::Message(error.to_string()))?,
        ),
        None => None,
    };
    #[cfg(target_os = "linux")]
    if let Some(name) = target.strip_prefix('@') {
        let addr = abstract_addr(name)?;
        return Client::connect_addr(&addr, public_key, config).map_err(RemoteError::from);
    }
    Client::connect(&target, public_key, config).map_err(RemoteError::from)
}

fn error_message(response: &Value) -> String {
    response
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or("remote command failed")
        .to_string()
}

/// Run a parsed remote invocation using `environment` for defaults.
pub fn run_remote_options<W: Write>(
    options: &RemoteOptions,
    environment: &KittenEnvironment,
    stdout: &mut W,
) -> Result<(), RemoteError> {
    if options.help {
        stdout.write_all(REMOTE_USAGE.as_bytes())?;
        stdout.write_all(b"\n")?;
        return Ok(());
    }
    let command = options.command.as_deref().unwrap_or("version");
    if command == "help" {
        stdout.write_all(REMOTE_USAGE.as_bytes())?;
        stdout.write_all(b"\n")?;
        return Ok(());
    }
    let target = options
        .target
        .clone()
        .or_else(|| environment.socket().map(str::to_owned))
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            RemoteError::Message(
                "no remote socket: pass --to=unix:/path or unix:@name, or set KITTY_LISTEN_ON"
                    .to_owned(),
            )
        })?;
    let password = options
        .password
        .clone()
        .or_else(|| {
            options
                .password_env
                .as_ref()
                .and_then(|name| std::env::var(name).ok())
        })
        .or_else(|| environment.password.clone());
    let mut client = connect(&target, password, environment.public_key.as_deref())?;
    let request = build_request(options)?;
    let response = client.command(request)?;
    if response.get("ok").and_then(Value::as_bool) == Some(false) {
        return Err(RemoteError::Message(error_message(&response)));
    }
    let Some(data) = response.get("data") else {
        return Ok(());
    };
    match command {
        "screenshot" => {
            let encoded = data
                .as_str()
                .ok_or_else(|| RemoteError::Message("screenshot reply is not a string".into()))?;
            let bytes = decode_strict(encoded.as_bytes())
                .map_err(|error| RemoteError::Message(error.to_string()))?;
            if let Some(path) = &options.output {
                std::fs::write(path, &bytes)?;
            } else {
                stdout.write_all(&bytes)?;
            }
        }
        _ => {
            if !options.no_output {
                if let Some(text) = data.as_str() {
                    stdout.write_all(text.as_bytes())?;
                    stdout.write_all(b"\n")?;
                } else {
                    stdout.write_all(
                        data.to_json()
                            .map_err(|error| RemoteError::Message(error.to_string()))?
                            .as_bytes(),
                    )?;
                    stdout.write_all(b"\n")?;
                }
            }
        }
    }
    Ok(())
}

/// Parse and run `kitten @ ...`.
pub fn run_remote<I, S, W: Write>(
    args: I,
    environment: &KittenEnvironment,
    stdout: &mut W,
) -> Result<(), RemoteError>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let options = parse_remote_args(args)?;
    run_remote_options(&options, environment, stdout)
}
