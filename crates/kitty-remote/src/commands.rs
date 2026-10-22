// SPDX-License-Identifier: GPL-3.0-or-later
//! JSON command payloads and dispatch for the remote-control server.
//!
//! The wire shapes follow `kitty/rc/*.py`: command names use hyphens, the
//! command object carries `cmd`, `version` and an optional `payload` object,
//! and responses are `{"ok": true, "data": ...}` or
//! `{"ok": false, "error": ...}`. Unknown commands use kitty's error text.
//!
//! The actual terminal/window work is delegated to [`RemoteHandler`]. The
//! server crate only knows how to parse the kitty payloads and build replies,
//! which keeps the socket protocol independently testable.

use crate::base64::encode as encode_base64;
use crate::json::{Number, Value};
use std::fmt;
use std::sync::Arc;

/// Version reported by the capabilities/version replies. This tracks the kitty
/// source from which the protocol was ported.
pub const KITTY_COMPAT_VERSION: &str = "0.49.2";
/// Encryption protocol version used in the `enc_proto` envelope field.
pub const RC_ENCRYPTION_PROTOCOL_VERSION: &str = "1";

/// Commands understood by this layer.
pub const IMPLEMENTED_COMMANDS: &[&str] = &[
    "capabilities",
    "version",
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
];

/// A command error, rendered as kitty's `{"ok": false, "error": ...}` object.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandError {
    /// The error message shown to the client.
    pub message: String,
    /// Optional traceback, matching kitty's `tb` field for uncaught errors.
    pub traceback: Option<String>,
}

impl CommandError {
    /// Create an error from a message.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            traceback: None,
        }
    }

    /// Create an error with an attached traceback string.
    pub fn with_traceback(message: impl Into<String>, traceback: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            traceback: Some(traceback.into()),
        }
    }

    /// The error kitty produces for a command that does not exist.
    pub fn unknown(command: &str) -> Self {
        Self::new(format!("Unknown kitty remote control command: {command}"))
    }

    /// The error used by the default trait methods for a known command that
    /// this handler does not implement.
    pub fn unsupported(command: &str) -> Self {
        Self::new(format!("Unsupported remote control command: {command}"))
    }

    /// An invalid payload error.
    pub fn invalid(command: &str, detail: impl fmt::Display) -> Self {
        Self::new(format!("Invalid payload for command {command}: {detail}"))
    }
}

impl fmt::Display for CommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CommandError {}

/// Result returned by [`RemoteHandler`] methods.
pub type CommandResult = Result<Value, CommandError>;

/// Identity of a connected peer, from `SO_PEERCRED`/`getpeereid` when
/// available.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PeerInfo {
    /// Process id on platforms that expose it.
    pub pid: Option<u32>,
    /// Effective uid of the peer.
    pub uid: u32,
    /// Effective gid of the peer.
    pub gid: u32,
    /// Whether the credentials were obtained from the kernel.
    pub credentials_verified: bool,
}

/// Callback used by a connection to emit an event notification.
pub type EventNotifier = Arc<dyn Fn(&str, Value) -> Result<(), String> + Send + Sync>;

/// Per-request context handed to command handlers.
#[derive(Clone)]
pub struct RequestContext {
    /// Peer identity.
    pub peer: PeerInfo,
    /// Request correlation id, echoed in the reply when the client supplied
    /// the non-kitty `id` field.
    pub request_id: Option<Value>,
    /// Whether the request arrived inside an encrypted envelope.
    pub encrypted: bool,
    notifier: Option<EventNotifier>,
}

impl RequestContext {
    /// Create a context. The notifier is normally installed by the server.
    pub fn new(
        peer: PeerInfo,
        request_id: Option<Value>,
        encrypted: bool,
        notifier: Option<EventNotifier>,
    ) -> Self {
        Self {
            peer,
            request_id,
            encrypted,
            notifier,
        }
    }

    /// Emit a notification (for example `window_changed` or `tab_changed`) to
    /// the client that sent this request. Notifications are written before the
    /// command response, preserving per-connection ordering.
    pub fn notify(&self, event: impl Into<String>, data: Value) -> Result<(), CommandError> {
        let Some(notifier) = &self.notifier else {
            return Err(CommandError::new(
                "this request has no notification channel",
            ));
        };
        notifier(&event.into(), data).map_err(CommandError::new)
    }

    /// Whether notifications can be emitted for this request.
    pub fn can_notify(&self) -> bool {
        self.notifier.is_some()
    }
}

impl fmt::Debug for RequestContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RequestContext")
            .field("peer", &self.peer)
            .field("request_id", &self.request_id)
            .field("encrypted", &self.encrypted)
            .field("can_notify", &self.can_notify())
            .finish()
    }
}

/// Extent selector for `get-text`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextExtent {
    /// Currently visible screen.
    Screen,
    /// Output of the first command on screen.
    FirstCmdOutputOnScreen,
    /// Output of the last command.
    LastCmdOutput,
    /// Output of the last visited command.
    LastVisitedCmdOutput,
    /// Output of the last non-empty command.
    LastNonEmptyOutput,
    /// Screen plus scrollback.
    All,
    /// Currently selected text.
    Selection,
    /// The non-visible alternate screen.
    Alternate,
    /// Alternate screen plus its scrollback.
    AlternateScrollback,
}

impl TextExtent {
    fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "screen" => Self::Screen,
            "first_cmd_output_on_screen" => Self::FirstCmdOutputOnScreen,
            "last_cmd_output" => Self::LastCmdOutput,
            "last_visited_cmd_output" => Self::LastVisitedCmdOutput,
            "last_non_empty_output" => Self::LastNonEmptyOutput,
            "all" => Self::All,
            "selection" => Self::Selection,
            "alternate" => Self::Alternate,
            "alternate_scrollback" => Self::AlternateScrollback,
            _ => return None,
        })
    }

    /// Wire name used by kitty.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Screen => "screen",
            Self::FirstCmdOutputOnScreen => "first_cmd_output_on_screen",
            Self::LastCmdOutput => "last_cmd_output",
            Self::LastVisitedCmdOutput => "last_visited_cmd_output",
            Self::LastNonEmptyOutput => "last_non_empty_output",
            Self::All => "all",
            Self::Selection => "selection",
            Self::Alternate => "alternate",
            Self::AlternateScrollback => "alternate_scrollback",
        }
    }
}

/// Parsed `get-text` request.
#[derive(Clone, Debug, PartialEq)]
pub struct GetTextRequest {
    /// `match` window expression.
    pub match_spec: Option<String>,
    /// Which text to return.
    pub extent: TextExtent,
    /// Include ANSI formatting.
    pub ansi: bool,
    /// Append cursor position/style escapes.
    pub cursor: bool,
    /// Add wrap markers.
    pub wrap_markers: bool,
    /// Clear the selection after reading it.
    pub clear_selection: bool,
    /// Use the window in which the command was invoked.
    pub self_window: bool,
}

/// How `send-text` data is encoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SendTextData {
    /// UTF-8 text (`text:` prefix).
    Text(String),
    /// Raw bytes (`base64:` prefix, standard base64).
    Base64(Vec<u8>),
    /// A kitty key event (`kitty-key:` prefix).
    KittyKey(String),
    /// A broadcast session control payload (`session:` prefix).
    Session(String),
}

/// Bracketed-paste mode for `send-text`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BracketedPaste {
    /// Never wrap.
    Disable,
    /// Wrap if the child enabled bracketed paste.
    Auto,
    /// Always wrap.
    Enable,
}

impl BracketedPaste {
    fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "disable" => Self::Disable,
            "auto" => Self::Auto,
            "enable" => Self::Enable,
            _ => return None,
        })
    }
}

/// Parsed `send-text` request.
#[derive(Clone, Debug, PartialEq)]
pub struct SendTextRequest {
    /// Data to deliver.
    pub data: SendTextData,
    /// `match` window expression.
    pub match_spec: Option<String>,
    /// `match_tab` expression.
    pub match_tab: Option<String>,
    /// Match every window.
    pub all: bool,
    /// Skip the active window.
    pub exclude_active: bool,
    /// Bracketed paste mode.
    pub bracketed_paste: BracketedPaste,
}

/// Parsed `send-key` request.
#[derive(Clone, Debug, PartialEq)]
pub struct SendKeyRequest {
    /// Key names such as `ctrl+a`.
    pub keys: Vec<String>,
    /// `match` window expression.
    pub match_spec: Option<String>,
    /// `match_tab` expression.
    pub match_tab: Option<String>,
    /// Match every window.
    pub all: bool,
    /// Skip the active window.
    pub exclude_active: bool,
}

/// Axis for `resize-window`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResizeAxis {
    /// Change width.
    Horizontal,
    /// Change height.
    Vertical,
    /// Reset the layout.
    Reset,
}

impl ResizeAxis {
    fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "horizontal" => Self::Horizontal,
            "vertical" => Self::Vertical,
            "reset" => Self::Reset,
            _ => return None,
        })
    }
}

/// Parsed `resize-window` request.
#[derive(Clone, Debug, PartialEq)]
pub struct ResizeWindowRequest {
    /// `match` window expression.
    pub match_spec: Option<String>,
    /// Use the invoking window.
    pub self_window: bool,
    /// Number of cells to change.
    pub increment: i64,
    /// Fraction of remaining resize steps (0.0..=1.0).
    pub fraction: f64,
    /// Resize axis.
    pub axis: ResizeAxis,
    /// Restore a saved interactive resize session on reset.
    pub restore_entry_layout: bool,
}

/// Parsed `close-window` request.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct CloseWindowRequest {
    /// `match` window expression.
    pub match_spec: Option<String>,
    /// Use the invoking window.
    pub self_window: bool,
    /// Do not error when nothing matches.
    pub ignore_no_match: bool,
}

/// Parsed `close-tab` request.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct CloseTabRequest {
    /// `match` tab expression.
    pub match_spec: Option<String>,
    /// Use the tab containing the invoking window.
    pub self_window: bool,
    /// Do not error when nothing matches.
    pub ignore_no_match: bool,
}

/// Parsed `focus-window` request.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct FocusWindowRequest {
    /// `match` window expression.
    pub match_spec: Option<String>,
}

/// Parsed `set-window-title` request.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct SetWindowTitleRequest {
    /// New title, or `None` to restore the child's title.
    pub title: Option<String>,
    /// `match` window expression.
    pub match_spec: Option<String>,
    /// Allow the child to change the title again.
    pub temporary: bool,
    /// Use the invoking window.
    pub self_window: bool,
}

/// Parsed `get-colors` request.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct GetColorsRequest {
    /// `match` window expression.
    pub match_spec: Option<String>,
    /// Return configured rather than current colors.
    pub configured: bool,
}

/// Parsed `set-colors` request.
#[derive(Clone, Debug, PartialEq)]
pub struct SetColorsRequest {
    /// Window match expression (`match_window`).
    pub match_window: Option<String>,
    /// Tab match expression (`match_tab`).
    pub match_tab: Option<String>,
    /// Change every window.
    pub all: bool,
    /// Also change configured colors.
    pub configured: bool,
    /// Restore colors captured at startup.
    pub reset: bool,
    /// Object of color name to 24-bit integer, or `null` for nullable colors.
    pub colors: Value,
    /// Optional transparent background color specification.
    pub transparent_background_colors: Option<String>,
}

impl Default for SetColorsRequest {
    fn default() -> Self {
        Self {
            match_window: None,
            match_tab: None,
            all: false,
            configured: false,
            reset: false,
            colors: Value::object(),
            transparent_background_colors: None,
        }
    }
}

/// Image format produced by a screenshot handler.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScreenshotFormat {
    /// PNG image bytes (what kitty's `screenshot` command returns).
    Png,
    /// Binary PPM image bytes (P6).
    Ppm,
}

impl ScreenshotFormat {
    fn parse(text: &str) -> Option<Self> {
        Some(match text.to_ascii_lowercase().as_str() {
            "png" => Self::Png,
            "ppm" => Self::Ppm,
            _ => return None,
        })
    }

    /// MIME-like name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Ppm => "ppm",
        }
    }
}

/// Parsed `screenshot` request.
#[derive(Clone, Debug, PartialEq)]
pub struct ScreenshotRequest {
    /// `match` window expression.
    pub match_spec: Option<String>,
    /// `match_tab` expression.
    pub match_tab: Option<String>,
    /// Save to this path on the server instead of returning bytes.
    pub output_path: String,
    /// Image format the handler should produce.
    pub format: ScreenshotFormat,
}

/// Image passed to the screenshot callback.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScreenshotImage {
    /// Encoded image bytes.
    pub bytes: Vec<u8>,
    /// Encoding of `bytes`.
    pub format: ScreenshotFormat,
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
}

impl ScreenshotImage {
    /// Create a screenshot image description.
    pub fn new(bytes: Vec<u8>, format: ScreenshotFormat, width: u32, height: u32) -> Self {
        Self {
            bytes,
            format,
            width,
            height,
        }
    }

    /// Create a PNG screenshot with unknown dimensions.
    pub fn png(bytes: Vec<u8>) -> Self {
        Self::new(bytes, ScreenshotFormat::Png, 0, 0)
    }

    /// Create a PPM screenshot with unknown dimensions.
    pub fn ppm(bytes: Vec<u8>) -> Self {
        Self::new(bytes, ScreenshotFormat::Ppm, 0, 0)
    }
}

/// Callback invoked by a screenshot handler, possibly from another thread.
pub type ScreenshotCallback = Box<dyn FnOnce(ScreenshotImage) + Send>;

/// Sender used by the server for asynchronous replies.
pub type AsyncResponseFn = Arc<dyn Fn(Value) -> Result<(), String> + Send + Sync>;

/// Terminal/window operations needed by the server.
///
/// Every method has a default implementation returning a kitty-compatible
/// "unsupported" error, so tests and embedders only implement what they need.
pub trait RemoteHandler: Send + Sync {
    /// Return text from a window.
    fn get_text(&self, _ctx: &RequestContext, _request: GetTextRequest) -> CommandResult {
        Err(CommandError::unsupported("get-text"))
    }

    /// Send text to matched windows.
    fn send_text(&self, _ctx: &RequestContext, _request: SendTextRequest) -> CommandResult {
        Err(CommandError::unsupported("send-text"))
    }

    /// Send key presses to matched windows.
    fn send_key(&self, _ctx: &RequestContext, _request: SendKeyRequest) -> CommandResult {
        Err(CommandError::unsupported("send-key"))
    }

    /// Resize a window.
    fn resize_window(&self, _ctx: &RequestContext, _request: ResizeWindowRequest) -> CommandResult {
        Err(CommandError::unsupported("resize-window"))
    }

    /// Close matched windows.
    fn close_window(&self, _ctx: &RequestContext, _request: CloseWindowRequest) -> CommandResult {
        Err(CommandError::unsupported("close-window"))
    }

    /// Close matched tabs.
    fn close_tab(&self, _ctx: &RequestContext, _request: CloseTabRequest) -> CommandResult {
        Err(CommandError::unsupported("close-tab"))
    }

    /// Focus a window.
    fn focus_window(&self, _ctx: &RequestContext, _request: FocusWindowRequest) -> CommandResult {
        Err(CommandError::unsupported("focus-window"))
    }

    /// Set a window title.
    fn set_window_title(
        &self,
        _ctx: &RequestContext,
        _request: SetWindowTitleRequest,
    ) -> CommandResult {
        Err(CommandError::unsupported("set-window-title"))
    }

    /// Get terminal colors as kitty.conf syntax.
    fn get_colors(&self, _ctx: &RequestContext, _request: GetColorsRequest) -> CommandResult {
        Err(CommandError::unsupported("get-colors"))
    }

    /// Set terminal colors.
    fn set_colors(&self, _ctx: &RequestContext, _request: SetColorsRequest) -> CommandResult {
        Err(CommandError::unsupported("set-colors"))
    }

    /// Produce a screenshot. The handler must invoke `callback` exactly once
    /// when it accepts the request; returning `Ok(Value::Null)` means the
    /// response will be sent asynchronously.
    fn screenshot(
        &self,
        _ctx: &RequestContext,
        _request: ScreenshotRequest,
        _callback: ScreenshotCallback,
    ) -> CommandResult {
        Err(CommandError::unsupported("screenshot"))
    }
}

/// What the server should do after dispatch.
#[derive(Clone, Debug, PartialEq)]
pub enum DispatchOutcome {
    /// Send this complete response object.
    Response(Value),
    /// The request set `no_response`, or the command suppresses responses.
    NoResponse,
    /// The handler accepted an asynchronous screenshot request.
    Async,
}

/// Build a success response, omitting `data` when it is null/absent.
pub fn success_response(data: Option<Value>, id: Option<Value>) -> Value {
    let mut object = vec![("ok".to_string(), Value::Bool(true))];
    if let Some(data) = data {
        if !data.is_null() {
            object.push(("data".to_string(), data));
        }
    }
    if let Some(id) = id {
        object.push(("id".to_string(), id));
    }
    Value::Object(object)
}

/// Build an error response in kitty's shape.
pub fn error_response(error: CommandError, id: Option<Value>) -> Value {
    let mut object = vec![
        ("ok".to_string(), Value::Bool(false)),
        ("error".to_string(), Value::String(error.message)),
    ];
    if let Some(traceback) = error.traceback {
        object.push(("tb".to_string(), Value::String(traceback)));
    }
    if let Some(id) = id {
        object.push(("id".to_string(), id));
    }
    Value::Object(object)
}

/// Capabilities reply for the current protocol version.
pub fn capabilities_value(server_version: &str) -> Value {
    Value::object_from([
        ("version", Value::String(server_version.to_string())),
        (
            "protocol_version",
            Value::String(RC_ENCRYPTION_PROTOCOL_VERSION.to_string()),
        ),
        (
            "encryption_protocol",
            Value::String(RC_ENCRYPTION_PROTOCOL_VERSION.to_string()),
        ),
        (
            "commands",
            Value::Array(
                IMPLEMENTED_COMMANDS
                    .iter()
                    .map(|name| Value::String((*name).to_string()))
                    .collect(),
            ),
        ),
    ])
}

/// Dispatch one parsed request.
///
/// `server_version` is used for kitty's client-newer-than-server check and for
/// the `version`/`capabilities` replies. `async_sender`, when present, is used
/// by the screenshot callback.
pub fn execute(
    handler: &dyn RemoteHandler,
    ctx: &RequestContext,
    request: &Value,
    server_version: &str,
    async_sender: Option<AsyncResponseFn>,
) -> DispatchOutcome {
    let id = ctx.request_id.clone();
    let Some(object) = request.as_object() else {
        return DispatchOutcome::Response(error_response(
            CommandError::new("JSON payload of remote command must be an object"),
            id,
        ));
    };
    let _ = object;
    let no_response = request
        .get("no_response")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let Some(version) = request.get("version") else {
        return finish_error(
            CommandError::new(
                "JSON payload of remote command is invalid, must be an object with a version field",
            ),
            id,
            no_response,
        );
    };
    if let Some((client_major, client_minor)) = version_parts(version) {
        if let Some((server_major, server_minor)) =
            version_parts(&Value::String(server_version.to_string()))
        {
            if (client_major, client_minor) > (server_major, server_minor) {
                return finish_error(
                    CommandError::new(
                        "The kitty client you are using to send remote commands is newer than this kitty instance. This is not supported.",
                    ),
                    id,
                    no_response,
                );
            }
        }
    }

    let Some(cmd_value) = request.get("cmd") else {
        return finish_error(
            CommandError::new("JSON payload of remote command has no cmd field"),
            id,
            no_response,
        );
    };
    let Some(cmd) = cmd_value.as_str() else {
        return finish_error(
            CommandError::new("The cmd field of a remote command must be a string"),
            id,
            no_response,
        );
    };
    let normalized = cmd.replace('_', "-");

    let empty_payload = Value::object();
    let payload = match request.get("payload") {
        None | Some(Value::Null) => &empty_payload,
        Some(value) if value.is_object() => value,
        Some(_) => {
            return finish_error(
                CommandError::new("The payload field of a remote command must be an object"),
                id,
                no_response,
            );
        }
    };

    let result = match normalized.as_str() {
        "version" => Ok(Value::object_from([
            ("version", Value::String(server_version.to_string())),
            (
                "protocol_version",
                Value::String(RC_ENCRYPTION_PROTOCOL_VERSION.to_string()),
            ),
        ])),
        "capabilities" => Ok(capabilities_value(server_version)),
        "get-text" => parse_get_text(payload).and_then(|request| handler.get_text(ctx, request)),
        "send-text" => parse_send_text(payload).and_then(|request| handler.send_text(ctx, request)),
        "send-key" => parse_send_key(payload).and_then(|request| handler.send_key(ctx, request)),
        "resize-window" => {
            parse_resize_window(payload).and_then(|request| handler.resize_window(ctx, request))
        }
        "close-window" => {
            parse_close_window(payload).and_then(|request| handler.close_window(ctx, request))
        }
        "close-tab" => parse_close_tab(payload).and_then(|request| handler.close_tab(ctx, request)),
        "focus-window" => {
            parse_focus_window(payload).and_then(|request| handler.focus_window(ctx, request))
        }
        "set-window-title" => parse_set_window_title(payload)
            .and_then(|request| handler.set_window_title(ctx, request)),
        "get-colors" => {
            parse_get_colors(payload).and_then(|request| handler.get_colors(ctx, request))
        }
        "set-colors" => {
            parse_set_colors(payload).and_then(|request| handler.set_colors(ctx, request))
        }
        "screenshot" => {
            let Some(sender) = async_sender else {
                return finish_error(
                    CommandError::new("This connection cannot receive asynchronous responses"),
                    id,
                    no_response,
                );
            };
            let parsed = parse_screenshot(payload);
            let Ok(request) = parsed else {
                return finish_error(parsed.unwrap_err(), id, no_response);
            };
            let response_id = id.clone();
            let callback: ScreenshotCallback = Box::new(move |image| {
                let data =
                    String::from_utf8(encode_base64(&image.bytes, true)).expect("base64 is ASCII");
                let response = success_response(Some(Value::String(data)), response_id);
                let _ = sender(response);
            });
            match handler.screenshot(ctx, request, callback) {
                Ok(_) => return DispatchOutcome::Async,
                Err(error) => return finish_error(error, id, no_response),
            }
        }
        _ => {
            return finish_error(CommandError::unknown(cmd), id, no_response);
        }
    };

    match result {
        Ok(data) => {
            if no_response {
                DispatchOutcome::NoResponse
            } else {
                DispatchOutcome::Response(success_response(Some(data), id))
            }
        }
        Err(error) => finish_error(error, id, no_response),
    }
}

fn finish_error(error: CommandError, id: Option<Value>, no_response: bool) -> DispatchOutcome {
    if no_response {
        DispatchOutcome::NoResponse
    } else {
        DispatchOutcome::Response(error_response(error, id))
    }
}

fn version_parts(value: &Value) -> Option<(u64, u64)> {
    match value {
        Value::String(text) => {
            let mut parts = text.split('.');
            let major = parts.next()?.parse().ok()?;
            let minor = parts.next().unwrap_or("0").parse().ok()?;
            Some((major, minor))
        }
        Value::Array(items) => {
            let major = items.first()?.as_u64()?;
            let minor = items.get(1).and_then(Value::as_u64).unwrap_or(0);
            Some((major, minor))
        }
        _ => None,
    }
}

fn string_field(payload: &Value, key: &str) -> Result<Option<String>, CommandError> {
    match payload.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) if text.is_empty() => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(other) => Err(CommandError::new(format!(
            "The {key} field must be a string, not {other}"
        ))),
    }
}

fn bool_field(payload: &Value, key: &str, default: bool) -> bool {
    match payload.get(key) {
        None | Some(Value::Null) => default,
        Some(Value::Bool(value)) => *value,
        Some(Value::Number(number)) => number.as_f64() != 0.0,
        Some(Value::String(text)) => !text.is_empty(),
        Some(Value::Array(items)) => !items.is_empty(),
        Some(Value::Object(items)) => !items.is_empty(),
    }
}

fn integer_field(payload: &Value, key: &str, default: i64) -> Result<i64, CommandError> {
    match payload.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(value) => value
            .as_i64()
            .ok_or_else(|| CommandError::new(format!("The {key} field must be an integer"))),
    }
}

fn float_field(payload: &Value, key: &str, default: f64) -> Result<f64, CommandError> {
    match payload.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(value) => {
            let number = value
                .as_f64()
                .ok_or_else(|| CommandError::new(format!("The {key} field must be a number")))?;
            if !number.is_finite() {
                return Err(CommandError::new(format!("The {key} field must be finite")));
            }
            Ok(number)
        }
    }
}

fn strings_field(payload: &Value, key: &str) -> Result<Vec<String>, CommandError> {
    let Some(value) = payload.get(key) else {
        return Err(CommandError::new(format!("The {key} field is required")));
    };
    let Some(items) = value.as_array() else {
        return Err(CommandError::new(format!(
            "The {key} field must be a list of strings"
        )));
    };
    let mut ans = Vec::with_capacity(items.len());
    for item in items {
        let Some(text) = item.as_str() else {
            return Err(CommandError::new(format!(
                "The {key} field must contain only strings"
            )));
        };
        ans.push(text.to_string());
    }
    Ok(ans)
}

fn parse_get_text(payload: &Value) -> Result<GetTextRequest, CommandError> {
    let extent_text = string_field(payload, "extent")?.unwrap_or_else(|| "screen".into());
    let extent = TextExtent::parse(&extent_text).ok_or_else(|| {
        CommandError::invalid("get-text", format!("unknown extent {extent_text:?}"))
    })?;
    Ok(GetTextRequest {
        match_spec: string_field(payload, "match")?,
        extent,
        ansi: bool_field(payload, "ansi", false),
        cursor: bool_field(payload, "cursor", false),
        wrap_markers: bool_field(payload, "wrap_markers", false),
        clear_selection: bool_field(payload, "clear_selection", false),
        self_window: bool_field(payload, "self", false),
    })
}

fn parse_send_text(payload: &Value) -> Result<SendTextRequest, CommandError> {
    let raw = string_field(payload, "data")?
        .ok_or_else(|| CommandError::new("The data field is required for send-text"))?;
    let (encoding, body) = raw.split_once(':').unwrap_or((&raw, ""));
    let data = match encoding {
        "text" => SendTextData::Text(body.to_string()),
        "base64" => {
            let bytes = crate::base64::decode_strict(body.as_bytes())
                .map_err(|_| CommandError::new("Invalid base64 data for send-text"))?;
            SendTextData::Base64(bytes)
        }
        "kitty-key" => SendTextData::KittyKey(body.to_string()),
        "session" => SendTextData::Session(body.to_string()),
        other => {
            return Err(CommandError::new(format!(
                "Invalid encoding for send-text data: {other}"
            )));
        }
    };
    let bracketed_text =
        string_field(payload, "bracketed_paste")?.unwrap_or_else(|| "disable".into());
    let bracketed_paste = BracketedPaste::parse(&bracketed_text).ok_or_else(|| {
        CommandError::invalid(
            "send-text",
            format!("unknown bracketed_paste mode {bracketed_text:?}"),
        )
    })?;
    Ok(SendTextRequest {
        data,
        match_spec: string_field(payload, "match")?,
        match_tab: string_field(payload, "match_tab")?,
        all: bool_field(payload, "all", false),
        exclude_active: bool_field(payload, "exclude_active", false),
        bracketed_paste,
    })
}

fn parse_send_key(payload: &Value) -> Result<SendKeyRequest, CommandError> {
    Ok(SendKeyRequest {
        keys: strings_field(payload, "keys")?,
        match_spec: string_field(payload, "match")?,
        match_tab: string_field(payload, "match_tab")?,
        all: bool_field(payload, "all", false),
        exclude_active: bool_field(payload, "exclude_active", false),
    })
}

fn parse_resize_window(payload: &Value) -> Result<ResizeWindowRequest, CommandError> {
    let axis_text = string_field(payload, "axis")?.unwrap_or_else(|| "horizontal".into());
    let axis = ResizeAxis::parse(&axis_text).ok_or_else(|| {
        CommandError::invalid("resize-window", format!("unknown axis {axis_text:?}"))
    })?;
    let fraction = float_field(payload, "fraction", 0.0)?;
    if !(0.0..=1.0).contains(&fraction) {
        return Err(CommandError::new(
            "Resize fraction must be between zero and one",
        ));
    }
    Ok(ResizeWindowRequest {
        match_spec: string_field(payload, "match")?,
        self_window: bool_field(payload, "self", false),
        increment: integer_field(payload, "increment", 2)?,
        fraction,
        axis,
        restore_entry_layout: bool_field(payload, "restore_entry_layout", false),
    })
}

fn parse_close_window(payload: &Value) -> Result<CloseWindowRequest, CommandError> {
    Ok(CloseWindowRequest {
        match_spec: string_field(payload, "match")?,
        self_window: bool_field(payload, "self", false),
        ignore_no_match: bool_field(payload, "ignore_no_match", false),
    })
}

fn parse_close_tab(payload: &Value) -> Result<CloseTabRequest, CommandError> {
    Ok(CloseTabRequest {
        match_spec: string_field(payload, "match")?,
        self_window: bool_field(payload, "self", false),
        ignore_no_match: bool_field(payload, "ignore_no_match", false),
    })
}

fn parse_focus_window(payload: &Value) -> Result<FocusWindowRequest, CommandError> {
    Ok(FocusWindowRequest {
        match_spec: string_field(payload, "match")?,
    })
}

fn parse_set_window_title(payload: &Value) -> Result<SetWindowTitleRequest, CommandError> {
    Ok(SetWindowTitleRequest {
        title: string_field(payload, "title")?,
        match_spec: string_field(payload, "match")?,
        temporary: bool_field(payload, "temporary", false),
        self_window: bool_field(payload, "self", false),
    })
}

fn parse_get_colors(payload: &Value) -> Result<GetColorsRequest, CommandError> {
    Ok(GetColorsRequest {
        match_spec: string_field(payload, "match")?,
        configured: bool_field(payload, "configured", false),
    })
}

fn parse_set_colors(payload: &Value) -> Result<SetColorsRequest, CommandError> {
    let colors = match payload.get("colors") {
        None | Some(Value::Null) => Value::object(),
        Some(value) if value.is_object() => value.clone(),
        Some(_) => {
            return Err(CommandError::new(
                "The colors field must be an object mapping names to integers",
            ));
        }
    };
    let transparent_background_colors = match colors.get("transparent_background_colors") {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) if text.is_empty() => Some(String::new()),
        Some(Value::String(text)) => Some(text.clone()),
        Some(_) => {
            return Err(CommandError::new(
                "transparent_background_colors must be a string",
            ));
        }
    };
    Ok(SetColorsRequest {
        match_window: string_field(payload, "match_window")?,
        match_tab: string_field(payload, "match_tab")?,
        all: bool_field(payload, "all", false),
        configured: bool_field(payload, "configured", false),
        reset: bool_field(payload, "reset", false),
        colors,
        transparent_background_colors,
    })
}

fn parse_screenshot(payload: &Value) -> Result<ScreenshotRequest, CommandError> {
    let format_text = string_field(payload, "format")?.unwrap_or_else(|| "png".into());
    let format = ScreenshotFormat::parse(&format_text).ok_or_else(|| {
        CommandError::invalid("screenshot", format!("unknown format {format_text:?}"))
    })?;
    Ok(ScreenshotRequest {
        match_spec: string_field(payload, "match")?,
        match_tab: string_field(payload, "match_tab")?,
        output_path: string_field(payload, "output_path")?.unwrap_or_default(),
        format,
    })
}

/// Return the color value as a 24-bit integer, accepting both JSON integers
/// and kitty's `#rrggbb` strings (the latter is only used by some frontends).
pub fn color_as_u32(value: &Value) -> Option<u32> {
    match value {
        Value::Number(Number::I64(v)) => u32::try_from(*v).ok(),
        Value::Number(Number::U64(v)) => u32::try_from(*v).ok(),
        Value::String(text) => {
            let text = text.strip_prefix('#').unwrap_or(text);
            u32::from_str_radix(text, 16).ok()
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_comparison() {
        assert_eq!(
            version_parts(&Value::String("0.49.2".into())),
            Some((0, 49))
        );
        assert_eq!(
            version_parts(&Value::Array(vec![Value::from(1u64), Value::from(2u64)])),
            Some((1, 2))
        );
        assert_eq!(version_parts(&Value::Bool(true)), None);
    }

    #[test]
    fn color_conversion() {
        assert_eq!(color_as_u32(&Value::from(0xabcdefu64)), Some(0xabcdef));
        assert_eq!(color_as_u32(&Value::from("#00ff00")), Some(0x00ff00));
        assert_eq!(color_as_u32(&Value::Null), None);
    }
}
