// SPDX-License-Identifier: GPL-3.0-or-later
//! End-to-end tests for the Unix-socket remote-control layer.

use kitty_remote::framing::{encode_message, Framer};
use kitty_remote::json::Value;
use kitty_remote::{
    parse_public_key_env, seal_request, seal_request_with_timestamp, Client, ClientConfig,
    CloseTabRequest, CloseWindowRequest, CommandError, CommandResult, EncryptionMode,
    FocusWindowRequest, GetColorsRequest, GetTextRequest, PeerInfo, RemoteHandler, RemoteServer,
    RequestContext, ResizeAxis, ResizeWindowRequest, ScreenshotFormat, ScreenshotImage,
    ScreenshotRequest, SendKeyRequest, SendTextData, SendTextRequest, ServerConfig, ServerHandle,
    SetColorsRequest, SetWindowTitleRequest, TextExtent, KITTY_COMPAT_VERSION,
};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

static SOCKET_COUNTER: AtomicUsize = AtomicUsize::new(0);

fn socket_path(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "kitty-remote-test-{}-{}-{tag}.sock",
        std::process::id(),
        SOCKET_COUNTER.fetch_add(1, Ordering::Relaxed)
    ))
}

fn command(name: &str, payload: Value) -> Value {
    Value::object_from([("cmd", Value::from(name)), ("payload", payload)])
}

fn object<const N: usize>(items: [(&str, Value); N]) -> Value {
    Value::object_from(items)
}

fn data_str(response: &Value) -> &str {
    response
        .get("data")
        .and_then(Value::as_str)
        .expect("response has string data")
}

fn assert_ok(response: &Value) {
    assert_eq!(
        response.get("ok").and_then(Value::as_bool),
        Some(true),
        "unexpected response: {response}"
    );
}

fn assert_error(response: &Value) -> &str {
    assert_eq!(
        response.get("ok").and_then(Value::as_bool),
        Some(false),
        "unexpected response: {response}"
    );
    response
        .get("error")
        .and_then(Value::as_str)
        .expect("error response has error string")
}

fn now_ns() -> i128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos() as i128
}

#[derive(Default)]
struct HandlerState {
    text: String,
    colors: String,
    screenshot: Option<ScreenshotImage>,
    get_text: Option<GetTextRequest>,
    send_text: Option<SendTextRequest>,
    send_key: Option<SendKeyRequest>,
    resize: Option<ResizeWindowRequest>,
    close_window: Option<CloseWindowRequest>,
    close_tab: Option<CloseTabRequest>,
    focus: Option<FocusWindowRequest>,
    title: Option<SetWindowTitleRequest>,
    get_colors: Option<GetColorsRequest>,
    set_colors: Option<SetColorsRequest>,
    screenshot_request: Option<ScreenshotRequest>,
    peer: Option<PeerInfo>,
    calls: Vec<&'static str>,
    emit_events: usize,
    resize_result: bool,
    screenshot_error: Option<String>,
}

struct TestHandler {
    state: Mutex<HandlerState>,
}

impl TestHandler {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(HandlerState::default()),
        })
    }

    fn state(&self) -> std::sync::MutexGuard<'_, HandlerState> {
        self.state.lock().unwrap()
    }

    fn configure(self: &Arc<Self>, f: impl FnOnce(&mut HandlerState)) -> Arc<Self> {
        f(&mut self.state.lock().unwrap());
        self.clone()
    }
}

impl RemoteHandler for TestHandler {
    fn get_text(&self, ctx: &RequestContext, request: GetTextRequest) -> CommandResult {
        let mut state = self.state.lock().unwrap();
        state.calls.push("get-text");
        state.peer = Some(ctx.peer);
        state.get_text = Some(request);
        let events = state.emit_events;
        let text = state.text.clone();
        drop(state);
        for index in 0..events {
            ctx.notify(
                "window_changed",
                object([("index", Value::from(index as u64))]),
            )
            .unwrap();
        }
        Ok(Value::String(text))
    }

    fn send_text(&self, ctx: &RequestContext, request: SendTextRequest) -> CommandResult {
        let mut state = self.state.lock().unwrap();
        state.calls.push("send-text");
        state.peer = Some(ctx.peer);
        state.send_text = Some(request);
        Ok(Value::Null)
    }

    fn send_key(&self, ctx: &RequestContext, request: SendKeyRequest) -> CommandResult {
        let mut state = self.state.lock().unwrap();
        state.calls.push("send-key");
        state.peer = Some(ctx.peer);
        state.send_key = Some(request);
        Ok(Value::Null)
    }

    fn resize_window(&self, ctx: &RequestContext, request: ResizeWindowRequest) -> CommandResult {
        let mut state = self.state.lock().unwrap();
        state.calls.push("resize-window");
        state.peer = Some(ctx.peer);
        state.resize = Some(request);
        Ok(Value::Bool(state.resize_result))
    }

    fn close_window(&self, ctx: &RequestContext, request: CloseWindowRequest) -> CommandResult {
        let mut state = self.state.lock().unwrap();
        state.calls.push("close-window");
        state.peer = Some(ctx.peer);
        state.close_window = Some(request);
        Ok(Value::Null)
    }

    fn close_tab(&self, ctx: &RequestContext, request: CloseTabRequest) -> CommandResult {
        let mut state = self.state.lock().unwrap();
        state.calls.push("close-tab");
        state.peer = Some(ctx.peer);
        state.close_tab = Some(request);
        Ok(Value::Null)
    }

    fn focus_window(&self, ctx: &RequestContext, request: FocusWindowRequest) -> CommandResult {
        let mut state = self.state.lock().unwrap();
        state.calls.push("focus-window");
        state.peer = Some(ctx.peer);
        state.focus = Some(request);
        Ok(Value::Null)
    }

    fn set_window_title(
        &self,
        ctx: &RequestContext,
        request: SetWindowTitleRequest,
    ) -> CommandResult {
        let mut state = self.state.lock().unwrap();
        state.calls.push("set-window-title");
        state.peer = Some(ctx.peer);
        state.title = Some(request);
        Ok(Value::Null)
    }

    fn get_colors(&self, ctx: &RequestContext, request: GetColorsRequest) -> CommandResult {
        let mut state = self.state.lock().unwrap();
        state.calls.push("get-colors");
        state.peer = Some(ctx.peer);
        state.get_colors = Some(request);
        Ok(Value::String(state.colors.clone()))
    }

    fn set_colors(&self, ctx: &RequestContext, request: SetColorsRequest) -> CommandResult {
        let mut state = self.state.lock().unwrap();
        state.calls.push("set-colors");
        state.peer = Some(ctx.peer);
        state.set_colors = Some(request);
        Ok(Value::Null)
    }

    fn screenshot(
        &self,
        ctx: &RequestContext,
        request: ScreenshotRequest,
        callback: kitty_remote::ScreenshotCallback,
    ) -> CommandResult {
        let mut state = self.state.lock().unwrap();
        state.calls.push("screenshot");
        state.peer = Some(ctx.peer);
        state.screenshot_request = Some(request);
        if let Some(error) = &state.screenshot_error {
            return Err(CommandError::new(error.clone()));
        }
        match state.screenshot.take() {
            Some(image) => {
                drop(state);
                callback(image);
                Ok(Value::Null)
            }
            None => Err(CommandError::new("no screenshot configured")),
        }
    }
}

struct EmptyHandler;

impl RemoteHandler for EmptyHandler {}

struct TestServer {
    handle: Option<ServerHandle>,
    path: PathBuf,
}

impl TestServer {
    fn start(handler: Arc<dyn RemoteHandler>, config: ServerConfig) -> Self {
        let path = socket_path("server");
        let server = RemoteServer::bind(&path, handler, config).unwrap();
        let handle = server.start().unwrap();
        Self {
            handle: Some(handle),
            path,
        }
    }

    fn with_handler(handler: Arc<TestHandler>) -> Self {
        Self::start(handler, ServerConfig::local_test())
    }

    fn handle(&self) -> &ServerHandle {
        self.handle.as_ref().unwrap()
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn public_key(&self) -> Vec<u8> {
        self.handle().key().public_key().to_vec()
    }

    fn client(&self) -> Client {
        Client::connect_encrypted(&self.path, &self.public_key()).unwrap()
    }

    fn client_with(&self, config: ClientConfig) -> Client {
        Client::connect(&self.path, Some(self.public_key()), config).unwrap()
    }

    fn plain_client(&self) -> Client {
        Client::connect_plain(&self.path).unwrap()
    }

    fn shutdown(&mut self) {
        if let Some(mut handle) = self.handle.take() {
            handle.shutdown();
        }
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.shutdown();
        let _ = std::fs::remove_file(&self.path);
    }
}

fn raw_connect(path: &Path) -> UnixStream {
    let stream = UnixStream::connect(path).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    stream
}

fn write_raw(stream: &mut UnixStream, value: &Value) {
    let frame = encode_message(value.to_json().unwrap().as_bytes());
    stream.write_all(&frame).unwrap();
    stream.flush().unwrap();
}

fn read_raw_response(stream: &mut UnixStream) -> Value {
    let mut framer = Framer::new();
    let mut buffer = [0u8; 4096];
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if Instant::now() > deadline {
            panic!("timed out waiting for raw response");
        }
        match stream.read(&mut buffer) {
            Ok(0) => panic!("connection closed before a response arrived"),
            Ok(read) => {
                let messages = framer.push(&buffer[..read]).unwrap();
                if let Some(payload) = messages.into_iter().next() {
                    return kitty_remote::json::parse(&payload).unwrap();
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(error) => panic!("read failed: {error}"),
        }
    }
}

fn expect_closed_without_response(stream: &mut UnixStream) {
    let mut buffer = [0u8; 4096];
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if Instant::now() > deadline {
            panic!("timed out waiting for the server to close the connection");
        }
        match stream.read(&mut buffer) {
            Ok(0) => return,
            Ok(read) => {
                let text = String::from_utf8_lossy(&buffer[..read]);
                panic!("expected no response, got {read} bytes: {text}");
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(error) => panic!("read failed: {error}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Handshake and transport
// ---------------------------------------------------------------------------

#[test]
fn encrypted_handshake_and_capabilities() {
    let handler = TestHandler::new();
    let server = TestServer::with_handler(handler);
    let env = server.handle().public_key_env();
    assert!(env.starts_with("1:"));
    assert_eq!(parse_public_key_env(&env).unwrap(), server.public_key());

    let mut client = server.client();
    let response = client
        .command(command("capabilities", Value::object()))
        .unwrap();
    assert_ok(&response);
    let data = response.get("data").unwrap();
    assert_eq!(
        data.get("encryption_protocol").and_then(Value::as_str),
        Some("1")
    );
    let commands = data.get("commands").unwrap().as_array().unwrap();
    assert!(commands
        .iter()
        .any(|item| item.as_str() == Some("get-text")));
}

#[test]
fn plain_local_mode_is_accepted_when_optional() {
    let handler = TestHandler::new();
    let server = TestServer::with_handler(handler);
    let mut client = server.plain_client();
    let response = client.command(command("version", Value::object())).unwrap();
    assert_ok(&response);
    assert_eq!(
        response
            .get("data")
            .unwrap()
            .get("version")
            .and_then(Value::as_str),
        Some(KITTY_COMPAT_VERSION)
    );
}

#[test]
fn plain_mode_rejected_when_encryption_required() {
    let handler = TestHandler::new();
    let config = ServerConfig {
        encryption: EncryptionMode::Required,
        ..ServerConfig::local_test()
    };
    let server = TestServer::start(handler, config);
    let mut client = server.plain_client();
    let response = client
        .command_no_id(command("version", Value::object()))
        .unwrap();
    assert!(assert_error(&response).contains("required"));
}

#[test]
fn encrypted_mode_rejected_when_encryption_disabled() {
    let handler = TestHandler::new();
    let config = ServerConfig {
        encryption: EncryptionMode::Disabled,
        ..ServerConfig::local_test()
    };
    let server = TestServer::start(handler, config);
    let mut client = server.client();
    let response = client
        .command_no_id(command("version", Value::object()))
        .unwrap();
    assert!(assert_error(&response).contains("disabled"));
}

#[test]
fn tampered_ciphertext_is_rejected_and_connection_closes() {
    let handler = TestHandler::new();
    let server = TestServer::with_handler(handler);
    let mut envelope = seal_request(
        &server.public_key(),
        command("version", Value::object()),
        KITTY_COMPAT_VERSION,
        1024,
    )
    .unwrap();
    let encrypted = envelope.get("encrypted").unwrap().as_str().unwrap();
    let mut bytes = kitty_remote::base64::decode_b85(encrypted.as_bytes()).unwrap();
    bytes[0] ^= 0x01;
    envelope.set(
        "encrypted",
        Value::String(String::from_utf8(kitty_remote::base64::encode_b85(&bytes)).unwrap()),
    );
    let mut raw = raw_connect(server.path());
    write_raw(&mut raw, &envelope);
    expect_closed_without_response(&mut raw);
}

#[test]
fn tampered_tag_is_rejected_and_connection_closes() {
    let handler = TestHandler::new();
    let server = TestServer::with_handler(handler);
    let mut envelope = seal_request(
        &server.public_key(),
        command("version", Value::object()),
        KITTY_COMPAT_VERSION,
        1024,
    )
    .unwrap();
    let tag = envelope.get("tag").unwrap().as_str().unwrap();
    let mut bytes = kitty_remote::base64::decode_b85(tag.as_bytes()).unwrap();
    bytes[0] ^= 0x01;
    envelope.set(
        "tag",
        Value::String(String::from_utf8(kitty_remote::base64::encode_b85(&bytes)).unwrap()),
    );
    let mut raw = raw_connect(server.path());
    write_raw(&mut raw, &envelope);
    expect_closed_without_response(&mut raw);
}

#[test]
fn wrong_server_public_key_fails_handshake() {
    let handler = TestHandler::new();
    let server = TestServer::with_handler(handler);
    let wrong_key = kitty_remote::EllipticCurveKey::generate()
        .unwrap()
        .public_key()
        .unwrap();
    let config = ClientConfig {
        read_timeout: Some(Duration::from_millis(500)),
        ..ClientConfig::default()
    };
    let mut client = Client::connect(server.path(), Some(wrong_key), config).unwrap();
    assert!(client.command(command("version", Value::object())).is_err());
}

#[test]
fn replayed_old_timestamp_is_rejected() {
    let handler = TestHandler::new();
    let server = TestServer::with_handler(handler);
    let envelope = seal_request_with_timestamp(
        &server.public_key(),
        command("version", Value::object()),
        KITTY_COMPAT_VERSION,
        now_ns() - 6 * 60 * 1_000_000_000,
        1024,
    )
    .unwrap();
    let mut raw = raw_connect(server.path());
    write_raw(&mut raw, &envelope);
    expect_closed_without_response(&mut raw);
}

#[test]
fn wrong_password_returns_kitty_error() {
    let handler = TestHandler::new();
    let config = ServerConfig {
        password: Some("secret".into()),
        ..ServerConfig::local_test()
    };
    let server = TestServer::start(handler, config);
    let client_config = ClientConfig {
        password: Some("wrong".into()),
        ..ClientConfig::default()
    };
    let mut client = server.client_with(client_config);
    let response = client.command(command("version", Value::object())).unwrap();
    assert!(assert_error(&response).contains("rejected this password"));
}

#[test]
fn correct_password_allows_command() {
    let handler = TestHandler::new();
    let config = ServerConfig {
        password: Some("secret".into()),
        ..ServerConfig::local_test()
    };
    let server = TestServer::start(handler, config);
    let client_config = ClientConfig {
        password: Some("secret".into()),
        ..ClientConfig::default()
    };
    let mut client = server.client_with(client_config);
    let response = client.command(command("version", Value::object())).unwrap();
    assert_ok(&response);
}

#[test]
fn peer_credentials_are_reported_to_the_handler() {
    let handler = TestHandler::new().configure(|state| state.text = "hello".into());
    let server = TestServer::with_handler(handler.clone());
    let mut client = server.client();
    let response = client
        .command(command("get-text", Value::object()))
        .unwrap();
    assert_ok(&response);
    let state = handler.state();
    let peer = state.peer.expect("handler saw a peer");
    if cfg!(target_os = "linux") {
        assert!(peer.credentials_verified);
        assert_eq!(peer.pid, Some(std::process::id()));
    }
}

#[test]
fn socket_path_is_removed_on_shutdown() {
    let handler = TestHandler::new();
    let mut server = TestServer::with_handler(handler);
    assert!(server.path().exists());
    server.shutdown();
    assert!(!server.path().exists());
}

#[test]
fn binding_replaces_a_stale_socket() {
    let path = socket_path("stale");
    {
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        drop(listener);
    }
    assert!(path.exists());
    let handler = TestHandler::new();
    let server = RemoteServer::bind(&path, handler, ServerConfig::local_test()).unwrap();
    let mut handle = server.start().unwrap();
    assert!(path.exists());
    handle.shutdown();
    assert!(!path.exists());
}

#[test]
fn client_split_reads_still_parse_responses() {
    let handler = TestHandler::new().configure(|state| state.text = "split".into());
    let server = TestServer::with_handler(handler);
    let config = ClientConfig {
        split_reads: true,
        ..ClientConfig::default()
    };
    let mut client = server.client_with(config);
    let response = client
        .command(command("get-text", Value::object()))
        .unwrap();
    assert_eq!(data_str(&response), "split");
}

#[test]
fn server_accepts_requests_written_one_byte_at_a_time() {
    let handler = TestHandler::new();
    let server = TestServer::start(handler, ServerConfig::local_test());
    let mut raw = raw_connect(server.path());
    let request = object([
        ("cmd", Value::from("capabilities")),
        ("version", Value::from(KITTY_COMPAT_VERSION)),
    ]);
    let frame = encode_message(request.to_json().unwrap().as_bytes());
    for byte in frame {
        raw.write_all(&[byte]).unwrap();
        raw.flush().unwrap();
    }
    let response = read_raw_response(&mut raw);
    assert_ok(&response);
}

#[test]
fn multiple_frames_in_one_write_are_all_processed() {
    let handler = TestHandler::new().configure(|state| state.text = "many".into());
    let server = TestServer::with_handler(handler);
    let mut client = server.client();
    let ids = client
        .send_concurrent(vec![
            command("version", Value::object()),
            command("get-text", Value::object()),
            command("version", Value::object()),
        ])
        .unwrap();
    for id in ids {
        let response = client.wait_for_id(&id).unwrap();
        assert_ok(&response);
        assert_eq!(response.get("id"), Some(&id));
    }
}

#[test]
fn concurrent_responses_correlate_by_id() {
    let handler = TestHandler::new().configure(|state| state.text = "parallel".into());
    let server = TestServer::with_handler(handler.clone());
    let mut client = server.client();
    let requests: Vec<Value> = (0..32)
        .map(|_| command("get-text", Value::object()))
        .collect();
    let ids = client.send_concurrent(requests).unwrap();
    let mut received = 0;
    for id in &ids {
        let response = client.wait_for_id(id).unwrap();
        assert_eq!(response.get("id"), Some(id));
        if id.as_u64().unwrap() % 2 == 0 {
            assert_eq!(data_str(&response), "parallel");
        }
        received += 1;
    }
    assert_eq!(received, 32);
}

#[test]
fn notifications_from_handler_precede_response_and_keep_order() {
    let handler = TestHandler::new().configure(|state| {
        state.text = "events".into();
        state.emit_events = 5;
    });
    let server = TestServer::with_handler(handler);
    let mut client = server.client();
    let response = client
        .command(command("get-text", Value::object()))
        .unwrap();
    assert_ok(&response);
    let notifications = client.take_notifications();
    assert_eq!(notifications.len(), 5);
    for (index, notification) in notifications.iter().enumerate() {
        assert_eq!(notification.event, "window_changed");
        assert_eq!(
            notification.data.get("index").and_then(Value::as_u64),
            Some(index as u64)
        );
    }
}

#[test]
fn server_can_broadcast_asynchronous_notifications() {
    let handler = TestHandler::new();
    let server = TestServer::with_handler(handler);
    let mut client = server.client();
    assert_ok(&client.command(command("version", Value::object())).unwrap());
    assert_eq!(
        server
            .handle()
            .notify_event("tab_changed", object([("tab_id", Value::from(7u64))])),
        1
    );
    let notification = client.wait_for_notification().unwrap();
    assert_eq!(notification.event, "tab_changed");
    assert_eq!(
        notification.data.get("tab_id").and_then(Value::as_u64),
        Some(7)
    );
}

#[test]
fn oversized_message_is_rejected_without_hanging() {
    let handler = TestHandler::new();
    let config = ServerConfig {
        max_message_size: 256,
        ..ServerConfig::local_test()
    };
    let server = TestServer::start(handler, config);
    let mut raw = raw_connect(server.path());
    let request = object([
        ("cmd", Value::from("get-text")),
        ("version", Value::from(KITTY_COMPAT_VERSION)),
        (
            "payload",
            object([("padding", Value::from("x".repeat(600)))]),
        ),
    ]);
    write_raw(&mut raw, &request);
    let response = read_raw_response(&mut raw);
    assert!(assert_error(&response).contains("exceeds the limit"));
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

#[test]
fn get_text_screen_extent_and_flags() {
    let handler = TestHandler::new().configure(|state| state.text = "screen text".into());
    let server = TestServer::with_handler(handler.clone());
    let mut client = server.client();
    let response = client
        .command(command(
            "get-text",
            object([
                ("extent", Value::from("screen")),
                ("ansi", Value::Bool(true)),
                ("cursor", Value::Bool(true)),
                ("wrap_markers", Value::Bool(true)),
                ("clear_selection", Value::Bool(true)),
                ("match", Value::from("id:3")),
            ]),
        ))
        .unwrap();
    assert_eq!(data_str(&response), "screen text");
    let state = handler.state();
    let request = state.get_text.clone().unwrap();
    assert_eq!(request.extent, TextExtent::Screen);
    assert!(request.ansi && request.cursor && request.wrap_markers && request.clear_selection);
    assert_eq!(request.match_spec.as_deref(), Some("id:3"));
    assert!(!request.self_window);
}

#[test]
fn get_text_history_and_selection_extents() {
    let handler = TestHandler::new().configure(|state| state.text = "text".into());
    let server = TestServer::with_handler(handler.clone());
    let mut client = server.client();
    for (name, expected) in [
        ("all", TextExtent::All),
        ("selection", TextExtent::Selection),
        ("alternate", TextExtent::Alternate),
        ("alternate_scrollback", TextExtent::AlternateScrollback),
    ] {
        let response = client
            .command(command(
                "get-text",
                object([("extent", Value::from(name)), ("self", Value::Bool(true))]),
            ))
            .unwrap();
        assert_ok(&response);
        assert_eq!(handler.state().get_text.as_ref().unwrap().extent, expected);
    }
}

#[test]
fn send_text_text_and_base64_payloads() {
    let handler = TestHandler::new();
    let server = TestServer::with_handler(handler.clone());
    let mut client = server.client();
    let response = client
        .command(command(
            "send-text",
            object([
                ("data", Value::from("text:hello world")),
                ("match", Value::from("all")),
                ("all", Value::Bool(true)),
                ("exclude_active", Value::Bool(true)),
                ("bracketed_paste", Value::from("auto")),
            ]),
        ))
        .unwrap();
    assert_ok(&response);
    {
        let state = handler.state();
        let request = state.send_text.as_ref().unwrap();
        assert_eq!(request.data, SendTextData::Text("hello world".into()));
        assert!(request.all && request.exclude_active);
        assert_eq!(request.bracketed_paste, kitty_remote::BracketedPaste::Auto);
    }

    let encoded = kitty_remote::base64::encode(b"raw\x00bytes", true);
    let response = client
        .command(command(
            "send-text",
            object([(
                "data",
                Value::String(format!("base64:{}", String::from_utf8(encoded).unwrap())),
            )]),
        ))
        .unwrap();
    assert_ok(&response);
    assert_eq!(
        handler.state().send_text.as_ref().unwrap().data,
        SendTextData::Base64(b"raw\x00bytes".to_vec())
    );
}

#[test]
fn send_key_preserves_keys_and_selectors() {
    let handler = TestHandler::new();
    let server = TestServer::with_handler(handler.clone());
    let mut client = server.client();
    let response = client
        .command(command(
            "send-key",
            object([
                (
                    "keys",
                    Value::Array(vec![Value::from("ctrl+a"), Value::from("ctrl+b")]),
                ),
                ("match_tab", Value::from("all")),
                ("exclude_active", Value::Bool(true)),
            ]),
        ))
        .unwrap();
    assert_ok(&response);
    let state = handler.state();
    let request = state.send_key.as_ref().unwrap();
    assert_eq!(request.keys, vec!["ctrl+a", "ctrl+b"]);
    assert_eq!(request.match_tab.as_deref(), Some("all"));
    assert!(request.exclude_active);
}

#[test]
fn resize_window_parses_increment_fraction_and_axis() {
    let handler = TestHandler::new().configure(|state| state.resize_result = true);
    let server = TestServer::with_handler(handler.clone());
    let mut client = server.client();
    let response = client
        .command(command(
            "resize-window",
            object([
                ("match", Value::from("id:2")),
                ("increment", Value::from(-3i64)),
                ("fraction", Value::from(0.25f64)),
                ("axis", Value::from("vertical")),
                ("restore_entry_layout", Value::Bool(true)),
            ]),
        ))
        .unwrap();
    assert_ok(&response);
    assert_eq!(response.get("data").and_then(Value::as_bool), Some(true));
    let state = handler.state();
    let request = state.resize.as_ref().unwrap();
    assert_eq!(request.increment, -3);
    assert_eq!(request.fraction, 0.25);
    assert_eq!(request.axis, ResizeAxis::Vertical);
    assert!(request.restore_entry_layout);
}

#[test]
fn resize_window_rejects_invalid_fraction() {
    let handler = TestHandler::new();
    let server = TestServer::with_handler(handler);
    let mut client = server.client();
    let response = client
        .command(command(
            "resize-window",
            object([("fraction", Value::from(1.5f64))]),
        ))
        .unwrap();
    assert!(assert_error(&response).contains("between zero and one"));
}

#[test]
fn close_window_request_reaches_handler() {
    let handler = TestHandler::new();
    let server = TestServer::with_handler(handler.clone());
    let mut client = server.client();
    let response = client
        .command(command(
            "close-window",
            object([
                ("match", Value::from("id:4")),
                ("ignore_no_match", Value::Bool(true)),
                ("self", Value::Bool(true)),
            ]),
        ))
        .unwrap();
    assert_ok(&response);
    let request = handler.state().close_window.clone().unwrap();
    assert_eq!(request.match_spec.as_deref(), Some("id:4"));
    assert!(request.ignore_no_match && request.self_window);
}

#[test]
fn close_tab_request_reaches_handler() {
    let handler = TestHandler::new();
    let server = TestServer::with_handler(handler.clone());
    let mut client = server.client();
    let response = client
        .command(command(
            "close-tab",
            object([("match", Value::from("all")), ("self", Value::Bool(false))]),
        ))
        .unwrap();
    assert_ok(&response);
    let request = handler.state().close_tab.clone().unwrap();
    assert_eq!(request.match_spec.as_deref(), Some("all"));
    assert!(!request.self_window);
}

#[test]
fn focus_window_request_reaches_handler() {
    let handler = TestHandler::new();
    let server = TestServer::with_handler(handler.clone());
    let mut client = server.client();
    let response = client
        .command(command(
            "focus-window",
            object([("match", Value::from("title:editor"))]),
        ))
        .unwrap();
    assert_ok(&response);
    assert_eq!(
        handler
            .state()
            .focus
            .as_ref()
            .unwrap()
            .match_spec
            .as_deref(),
        Some("title:editor")
    );
}

#[test]
fn set_window_title_temporary_and_permanent() {
    let handler = TestHandler::new();
    let server = TestServer::with_handler(handler.clone());
    let mut client = server.client();
    let response = client
        .command(command(
            "set-window-title",
            object([
                ("title", Value::from("new title")),
                ("temporary", Value::Bool(true)),
                ("match", Value::from("all")),
            ]),
        ))
        .unwrap();
    assert_ok(&response);
    let request = handler.state().title.clone().unwrap();
    assert_eq!(request.title.as_deref(), Some("new title"));
    assert!(request.temporary);
    assert_eq!(request.match_spec.as_deref(), Some("all"));

    let response = client
        .command(command("set-window-title", Value::object()))
        .unwrap();
    assert_ok(&response);
    assert_eq!(handler.state().title.as_ref().unwrap().title, None);
}

#[test]
fn get_colors_returns_handler_text() {
    let handler = TestHandler::new()
        .configure(|state| state.colors = "background #000000\nforeground #ffffff".into());
    let server = TestServer::with_handler(handler.clone());
    let mut client = server.client();
    let response = client
        .command(command(
            "get-colors",
            object([
                ("configured", Value::Bool(true)),
                ("match", Value::from("id:5")),
            ]),
        ))
        .unwrap();
    assert_eq!(
        data_str(&response),
        "background #000000\nforeground #ffffff"
    );
    let request = handler.state().get_colors.clone().unwrap();
    assert!(request.configured);
    assert_eq!(request.match_spec.as_deref(), Some("id:5"));
}

#[test]
fn set_colors_preserves_colors_and_transparency() {
    let handler = TestHandler::new();
    let server = TestServer::with_handler(handler.clone());
    let mut client = server.client();
    let response = client
        .command(command(
            "set-colors",
            object([
                (
                    "colors",
                    object([
                        ("foreground", Value::from(0x112233u64)),
                        ("background", Value::from(0x445566u64)),
                        ("transparent_background_colors", Value::from("red@0.5")),
                    ]),
                ),
                ("all", Value::Bool(true)),
                ("configured", Value::Bool(true)),
                ("match_window", Value::from("all")),
            ]),
        ))
        .unwrap();
    assert_ok(&response);
    let request = handler.state().set_colors.clone().unwrap();
    assert!(request.all && request.configured && !request.reset);
    assert_eq!(
        kitty_remote::color_as_u32(request.colors.get("foreground").unwrap()),
        Some(0x112233)
    );
    assert_eq!(
        request.transparent_background_colors.as_deref(),
        Some("red@0.5")
    );
}

#[test]
fn screenshot_png_callback_returns_base64_data() {
    let png = b"\x89PNG\r\n\x1a\nfake".to_vec();
    let handler = TestHandler::new().configure(|state| {
        state.screenshot = Some(ScreenshotImage::new(
            png.clone(),
            ScreenshotFormat::Png,
            10,
            20,
        ));
    });
    let server = TestServer::with_handler(handler.clone());
    let mut client = server.client();
    let response = client
        .command(command(
            "screenshot",
            object([("match", Value::from("id:9"))]),
        ))
        .unwrap();
    assert_ok(&response);
    let decoded = kitty_remote::base64::decode_strict(data_str(&response).as_bytes()).unwrap();
    assert_eq!(decoded, png);
    let request = handler.state().screenshot_request.clone().unwrap();
    assert_eq!(request.format, ScreenshotFormat::Png);
    assert_eq!(request.match_spec.as_deref(), Some("id:9"));
}

#[test]
fn screenshot_ppm_callback_is_supported() {
    let ppm = b"P6\n1 1\n255\n\xff\x00\x00".to_vec();
    let handler = TestHandler::new().configure(|state| {
        state.screenshot = Some(ScreenshotImage::new(
            ppm.clone(),
            ScreenshotFormat::Ppm,
            1,
            1,
        ));
    });
    let server = TestServer::with_handler(handler);
    let mut client = server.client();
    let response = client
        .command(command(
            "screenshot",
            object([("format", Value::from("ppm"))]),
        ))
        .unwrap();
    assert_eq!(
        kitty_remote::base64::decode_strict(data_str(&response).as_bytes()).unwrap(),
        ppm
    );
}

#[test]
fn screenshot_handler_error_returns_error_object() {
    let handler = TestHandler::new()
        .configure(|state| state.screenshot_error = Some("cannot capture".into()));
    let server = TestServer::with_handler(handler);
    let mut client = server.client();
    let response = client
        .command(command("screenshot", Value::object()))
        .unwrap();
    assert_eq!(assert_error(&response), "cannot capture");
}

#[test]
fn capabilities_and_version_replies() {
    let handler = TestHandler::new();
    let server = TestServer::with_handler(handler);
    let mut client = server.client();
    let version = client.command(command("version", Value::object())).unwrap();
    assert_ok(&version);
    assert_eq!(
        version
            .get("data")
            .unwrap()
            .get("version")
            .and_then(Value::as_str),
        Some(KITTY_COMPAT_VERSION)
    );
    let capabilities = client
        .command(command("capabilities", Value::object()))
        .unwrap();
    assert_ok(&capabilities);
    let commands = capabilities
        .get("data")
        .unwrap()
        .get("commands")
        .unwrap()
        .as_array()
        .unwrap();
    for expected in [
        "get-text",
        "send-text",
        "send-key",
        "resize-window",
        "close-window",
        "close-tab",
        "focus-window",
        "set-window-title",
        "get-colors",
        "set-colors",
        "screenshot",
    ] {
        assert!(commands.iter().any(|item| item.as_str() == Some(expected)));
    }
}

#[test]
fn unknown_command_uses_kitty_error_text() {
    let handler = TestHandler::new();
    let server = TestServer::with_handler(handler);
    let mut client = server.client();
    let response = client
        .command(command("frobnicate", Value::object()))
        .unwrap();
    assert_eq!(
        assert_error(&response),
        "Unknown kitty remote control command: frobnicate"
    );
}

#[test]
fn unsupported_command_uses_kitty_error_object() {
    let handler = Arc::new(EmptyHandler);
    let server = TestServer::start(handler, ServerConfig::local_test());
    let mut client = server.client();
    let response = client
        .command(command("get-text", Value::object()))
        .unwrap();
    assert_eq!(
        assert_error(&response),
        "Unsupported remote control command: get-text"
    );
}

#[test]
fn newer_client_version_is_rejected() {
    let handler = TestHandler::new();
    let server = TestServer::with_handler(handler);
    let mut client = server.client();
    let mut request = command("version", Value::object());
    request.set("version", Value::from("99.0.0"));
    let response = client.command(request).unwrap();
    assert!(assert_error(&response).contains("newer than this kitty"));
}

#[test]
fn no_response_command_is_not_answered() {
    let handler = TestHandler::new().configure(|state| state.text = "quiet".into());
    let server = TestServer::with_handler(handler.clone());
    let config = ClientConfig {
        read_timeout: Some(Duration::from_millis(150)),
        ..ClientConfig::default()
    };
    let mut client = server.client_with(config);
    let mut request = command("get-text", Value::object());
    request.set("no_response", Value::Bool(true));
    client.send(request, None).unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while handler.state().calls.is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(handler.state().calls, vec!["get-text"]);
    assert!(client.receive().is_err());
}
