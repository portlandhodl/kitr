// SPDX-License-Identifier: GPL-3.0-or-later
//! `kitten @` against an in-process kitty-remote server.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use kitty_kittens::env::KittenEnvironment;
use kitty_kittens::remote::{
    build_request, parse_remote_args, run_remote, run_remote_options, RemoteError,
};
use kitty_remote::json::Value;
use kitty_remote::{
    CommandResult, GetTextRequest, RemoteHandler, RemoteServer, RequestContext, ServerConfig,
    ServerHandle,
};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct Handler;

impl RemoteHandler for Handler {
    fn get_text(&self, _ctx: &RequestContext, _request: GetTextRequest) -> CommandResult {
        Ok(Value::String("hello from kitty".to_owned()))
    }
}

struct Server {
    path: PathBuf,
    handle: Option<ServerHandle>,
}

impl Server {
    fn start(config: ServerConfig) -> Self {
        let path = std::env::temp_dir().join(format!(
            "kitty-kittens-remote-{}-{}.sock",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_file(&path);
        let server = RemoteServer::bind(&path, Arc::new(Handler), config).unwrap();
        let handle = server.start().unwrap();
        Self {
            path,
            handle: Some(handle),
        }
    }

    fn environment(&self) -> KittenEnvironment {
        KittenEnvironment {
            listen_on: Some(format!("unix:{}", self.path.display())),
            ..KittenEnvironment::default()
        }
    }

    fn public_key_env(&self) -> String {
        self.handle.as_ref().unwrap().key().public_key_env()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if let Some(mut handle) = self.handle.take() {
            handle.shutdown();
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

#[test]
fn parses_global_options_and_command_arguments() {
    let options = parse_remote_args([
        "--to",
        "unix:/tmp/kitty.sock",
        "--match=id:7",
        "--no-output",
        "get-text",
        "--extent",
        "all",
    ])
    .unwrap();
    assert_eq!(options.target.as_deref(), Some("unix:/tmp/kitty.sock"));
    assert_eq!(options.match_spec.as_deref(), Some("id:7"));
    assert!(options.no_output);
    assert_eq!(options.command.as_deref(), Some("get-text"));
    assert_eq!(options.extent, "all");

    let error = parse_remote_args(["--bogus"]).unwrap_err();
    assert!(matches!(error, RemoteError::InvalidOption(_)));
}

#[test]
fn builds_requests_for_common_commands() {
    let options = parse_remote_args(["send-key", "ctrl+c"]).unwrap();
    let request = build_request(&options).unwrap();
    assert_eq!(request.get("cmd").and_then(Value::as_str), Some("send-key"));
    assert_eq!(
        request.get("payload").and_then(|value| value.get("keys")),
        Some(&Value::Array(vec![Value::String("ctrl+c".to_owned())]))
    );

    let options = parse_remote_args(["unknown-command"]).unwrap();
    assert!(build_request(&options).is_err());
}

#[test]
fn get_text_roundtrips_over_a_plain_socket_from_the_environment() {
    let server = Server::start(ServerConfig::local_test());
    let environment = server.environment();
    assert_eq!(environment.window_id, None);
    let mut output = Vec::new();
    run_remote(["get-text"], &environment, &mut output).unwrap();
    assert_eq!(String::from_utf8(output).unwrap(), "hello from kitty\n");
}

#[test]
fn encrypted_environment_key_is_used() {
    let server = Server::start(ServerConfig::local_test());
    let mut environment = server.environment();
    environment.public_key = Some(server.public_key_env());
    environment.listen_on = Some(format!("unix:{}", server.path.display()));
    let mut output = Vec::new();
    run_remote(["get-text"], &environment, &mut output).unwrap();
    assert!(String::from_utf8(output)
        .unwrap()
        .contains("hello from kitty"));
}

#[test]
fn command_line_target_overrides_the_environment() {
    let server = Server::start(ServerConfig::local_test());
    let environment = KittenEnvironment::default();
    let target = format!("--to=unix:{}", server.path.display());
    let mut output = Vec::new();
    run_remote([target.as_str(), "get-text"], &environment, &mut output).unwrap();
    assert!(String::from_utf8(output)
        .unwrap()
        .contains("hello from kitty"));
}

#[test]
fn password_is_forwarded_to_a_password_protected_server() {
    let mut config = ServerConfig::local_test();
    config.password = Some("hunter2".to_owned());
    let server = Server::start(config);
    let mut environment = server.environment();
    environment.password = Some("hunter2".to_owned());
    let mut output = Vec::new();
    run_remote(["get-text"], &environment, &mut output).unwrap();
    assert!(String::from_utf8(output)
        .unwrap()
        .contains("hello from kitty"));
}

#[test]
fn missing_socket_is_a_clear_error() {
    let environment = KittenEnvironment::default();
    let error = run_remote(["get-text"], &environment, &mut Vec::new()).unwrap_err();
    assert!(matches!(error, RemoteError::Message(message) if message.contains("no remote socket")));
}

#[test]
fn no_output_suppresses_text_responses() {
    let server = Server::start(ServerConfig::local_test());
    let environment = server.environment();
    let options = parse_remote_args(["--no-output", "get-text"]).unwrap();
    let mut output = Vec::new();
    run_remote_options(&options, &environment, &mut output).unwrap();
    assert!(output.is_empty());
}
