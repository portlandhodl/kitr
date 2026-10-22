// SPDX-License-Identifier: GPL-3.0-or-later
//! `kitr @` remote-control client mode.
//!
//! This is deliberately a thin JSON front-end over the `kitty-remote` client.
//! It supports path and Linux abstract (`unix:@name`) sockets, optional
//! `$KITTY_PUBLIC_KEY` encryption, and the commands exercised by the GUI
//! remote tests: `get-text`, `send-text`, `send-key`, `resize-window`,
//! `close-window`, `close-tab`, `focus-window`, `set-window-title`,
//! `get-colors`, `set-colors` and `screenshot`.

use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::process::ExitCode;

use kitty_remote::base64::decode_strict;
use kitty_remote::client::{parse_public_key_env, Client, ClientConfig};
use kitty_remote::json::Value;
use kitty_remote::KITTY_COMPAT_VERSION;

struct Args {
    target: Option<String>,
    command: Option<String>,
    rest: Vec<String>,
    extent: String,
    format: String,
    output: Option<String>,
    match_spec: Option<String>,
    axis: String,
    increment: i64,
    title: Option<String>,
    keys: Vec<String>,
    stdin: bool,
    password: Option<String>,
    no_output: bool,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            target: None,
            command: None,
            rest: Vec::new(),
            extent: "screen".into(),
            format: "png".into(),
            output: None,
            match_spec: None,
            axis: "horizontal".into(),
            increment: 2,
            title: None,
            keys: Vec::new(),
            stdin: false,
            password: None,
            no_output: false,
        }
    }
}

fn take_value(args: &[String], index: &mut usize, name: &str) -> io::Result<String> {
    *index += 1;
    args.get(*index).cloned().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{name} requires a value"),
        )
    })
}

fn parse(args: &[OsString]) -> io::Result<Args> {
    let args: Vec<String> = args
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let mut out = Args::default();
    let mut i = 0usize;
    while i < args.len() {
        let arg = &args[i];
        let mut inline = None;
        let name = if let Some((name, value)) = arg.split_once('=') {
            inline = Some(value.to_string());
            name
        } else {
            arg.as_str()
        };
        let value = |i: &mut usize| -> io::Result<String> {
            if let Some(value) = inline.clone() {
                Ok(value)
            } else {
                take_value(&args, i, name)
            }
        };
        match name {
            "--to" => out.target = Some(value(&mut i)?),
            "--extent" => out.extent = value(&mut i)?,
            "--format" => out.format = value(&mut i)?,
            "--output" => out.output = Some(value(&mut i)?),
            "--match" => out.match_spec = Some(value(&mut i)?),
            "--axis" => out.axis = value(&mut i)?,
            "--increment" => {
                out.increment = value(&mut i)?.parse().map_err(|_| {
                    io::Error::new(io::ErrorKind::InvalidInput, "invalid --increment")
                })?
            }
            "--title" => out.title = Some(value(&mut i)?),
            "--key" => out.keys.push(value(&mut i)?),
            "--password" => out.password = Some(value(&mut i)?),
            "--stdin" => out.stdin = true,
            "--no-output" => out.no_output = true,
            "-h" | "--help" => {
                out.command = Some("help".into());
                return Ok(out);
            }
            other if other.starts_with('-') => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("unknown remote control option: {other}"),
                ))
            }
            other => {
                if out.command.is_none() {
                    out.command = Some(other.to_string());
                } else {
                    out.rest.push(other.to_string());
                }
            }
        }
        i += 1;
    }
    Ok(out)
}

fn help() {
    println!(
        "Usage: kitr @ [--to=unix:/path|unix:@name] COMMAND [ARGS...]\n\
\n\
Commands: get-text, send-text, send-key, resize-window, close-window,\n\
          close-tab, focus-window, set-window-title, get-colors,\n\
          set-colors, screenshot, version, capabilities\n\
\n\
Options:\n\
  --to ADDR          Remote socket; defaults to $KITTY_LISTEN_ON\n\
  --extent EXTENT    get-text extent: screen, all, selection (default: screen)\n\
  --match SPEC       Window match expression\n\
  --axis AXIS        resize-window axis: horizontal, vertical, reset\n\
  --increment N      resize-window cell increment (default: 2)\n\
  --format FMT       screenshot format: png or ppm (default: png)\n\
  --output FILE      screenshot output path (client side)\n\
  --stdin            read send-text data from stdin\n\
  --no-output        suppress textual responses\n"
    );
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

#[cfg(target_os = "linux")]
fn abstract_addr(name: &str) -> io::Result<std::os::unix::net::SocketAddr> {
    use std::os::linux::net::SocketAddrExt;
    std::os::unix::net::SocketAddr::from_abstract_name(name.as_bytes())
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))
}

fn connect(target: &str, password: Option<String>) -> io::Result<Client> {
    let target = target.strip_prefix("unix:").unwrap_or(target).to_string();
    let config = ClientConfig {
        password,
        ..ClientConfig::default()
    };
    let public_key = std::env::var("KITTY_PUBLIC_KEY")
        .ok()
        .and_then(|value| parse_public_key_env(&value).ok());
    #[cfg(target_os = "linux")]
    if let Some(name) = target.strip_prefix('@') {
        let addr = abstract_addr(name)?;
        return Client::connect_addr(&addr, public_key, config)
            .map_err(|error| io::Error::other(error.to_string()));
    }
    Client::connect(&target, public_key, config)
        .map_err(|error| io::Error::other(error.to_string()))
}

fn error_message(response: &Value) -> String {
    response
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or("remote command failed")
        .to_string()
}

fn run(mut client: Client, args: Args) -> io::Result<()> {
    let command = args.command.as_deref().unwrap_or("version");
    if command == "help" {
        help();
        return Ok(());
    }
    let match_value = || {
        args.match_spec
            .clone()
            .map(Value::String)
            .unwrap_or(Value::Null)
    };
    let req = match command {
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
            let mut data = args.rest.join(" ");
            if args.stdin {
                let mut buffer = String::new();
                io::stdin().read_to_string(&mut buffer)?;
                data = buffer;
            }
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
        other => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unknown remote control command: {other}"),
            ))
        }
    };
    let response = client
        .command(req)
        .map_err(|error| io::Error::other(error.to_string()))?;
    if response.get("ok").and_then(Value::as_bool) == Some(false) {
        return Err(io::Error::other(error_message(&response)));
    }
    let Some(data) = response.get("data") else {
        return Ok(());
    };
    match command {
        "screenshot" => {
            let encoded = data
                .as_str()
                .ok_or_else(|| io::Error::other("screenshot reply is not a string"))?;
            let bytes = decode_strict(encoded.as_bytes())
                .map_err(|error| io::Error::other(error.to_string()))?;
            if let Some(path) = &args.output {
                std::fs::write(path, &bytes)?;
            } else {
                io::stdout().lock().write_all(&bytes)?;
            }
        }
        _ => {
            if !args.no_output {
                if let Some(text) = data.as_str() {
                    println!("{text}");
                } else {
                    println!("{}", data.to_json().unwrap_or_default());
                }
            }
        }
    }
    Ok(())
}

/// Run `kitr @ ...`. The first argument is the token after `@`.
pub fn run_client(args: &[OsString]) -> io::Result<ExitCode> {
    let args = parse(args)?;
    if args.command.as_deref() == Some("help") {
        help();
        return Ok(ExitCode::SUCCESS);
    }
    let target = args
        .target
        .clone()
        .or_else(|| std::env::var("KITTY_LISTEN_ON").ok())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "no remote socket: pass --to=unix:/path or unix:@name, or set KITTY_LISTEN_ON",
            )
        })?;
    let client = connect(&target, args.password.clone())?;
    run(client, args)?;
    Ok(ExitCode::SUCCESS)
}
