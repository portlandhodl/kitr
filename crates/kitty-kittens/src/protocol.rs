// SPDX-License-Identifier: GPL-3.0-or-later
//! Kitten DCS protocol host.
//!
//! Kittens communicate with the terminal over DCS frames. This module owns the
//! byte-level framing for the messages that matter to a kitten host:
//!
//! | frame | payload |
//! |---|---|
//! | `ESC P @kitty-cmd...ESC \` | remote-control JSON (request or result) |
//! | `ESC P @kitty-kitten-result\|...ESC \` | base85-encoded JSON result |
//! | `ESC P @kitty-overlay-ready\|ESC \` | overlay is ready |
//! | `ESC P @kitty-overlay\|...ESC \` | open an overlay (JSON metadata) |
//! | `ESC P @kitty-print\|...ESC \` | base64-encoded text |
//! | `ESC P @kitty-title\|...ESC \` | set the window title |
//!
//! Remote-control framing, base64/base85 and JSON are taken from
//! [`kitty_remote`], so the bytes are identical to kitty's Python/C
//! implementation. The parser is incremental and can be fed arbitrary chunks,
//! including one byte at a time. No socket server or Python runtime is
//! involved.

use std::fmt;

use kitty_remote::base64::{decode_b85, encode, encode_b85};
use kitty_remote::framing::{encode_message, DCS_TERMINATOR};
use kitty_remote::json::{self, Value};

use crate::error::{Error, Result};

/// DCS introducer (`ESC P`).
pub const DCS_START: &[u8] = b"\x1bP";
/// Prefix of a remote-control command or response.
pub const CMD_PREFIX: &[u8] = b"@kitty-cmd";
/// Prefix of a kitten result frame.
pub const KITTEN_RESULT_PREFIX: &[u8] = b"@kitty-kitten-result|";
/// Prefix of an overlay-ready notification.
pub const OVERLAY_READY_PREFIX: &[u8] = b"@kitty-overlay-ready|";
/// Prefix of an overlay-open request (kitr extension).
pub const OVERLAY_PREFIX: &[u8] = b"@kitty-overlay|";
/// Prefix of a remote-print chunk.
pub const PRINT_PREFIX: &[u8] = b"@kitty-print|";
/// Prefix of a window-title request (kitr extension).
pub const TITLE_PREFIX: &[u8] = b"@kitty-title|";
/// Prefix shared by all kitty DCS frames.
pub const KITTY_PREFIX: &[u8] = b"@kitty-";
/// Default maximum payload accepted by [`DcsFramer`] (16 MiB).
pub const DEFAULT_MAX_MESSAGE_SIZE: usize = 16 * 1024 * 1024;

/// Metadata carried by an overlay-open frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OverlaySpec {
    pub title: Option<String>,
    pub ready: bool,
    pub close_on_exit: bool,
    pub payload: Option<Value>,
}

/// A parsed kitty DCS frame.
#[derive(Clone, Debug, PartialEq)]
pub enum KittenDcs {
    /// `@kitty-cmd...`, usually a remote-control result. `json` is `None` when
    /// the body is not valid JSON.
    RemoteCommand { body: Vec<u8>, json: Option<Value> },
    /// A kitten result (base85 JSON).
    KittenResult(Value),
    /// An overlay-ready notification.
    OverlayReady,
    /// An overlay-open request.
    Overlay(OverlaySpec),
    /// Text printed through the terminal's remote-print channel.
    Print(String),
    /// A window-title request.
    Title(String),
    /// Any other `@kitty-*` frame. The name excludes the `@kitty-` prefix and
    /// any trailing `|`.
    Other { name: String, payload: Vec<u8> },
}

impl KittenDcs {
    /// Short stable name, useful for logging.
    pub fn name(&self) -> &str {
        match self {
            Self::RemoteCommand { .. } => "cmd",
            Self::KittenResult(_) => "kitten-result",
            Self::OverlayReady => "overlay-ready",
            Self::Overlay(_) => "overlay",
            Self::Print(_) => "print",
            Self::Title(_) => "title",
            Self::Other { name, .. } => name,
        }
    }
}

impl fmt::Display for KittenDcs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Encode a remote-control JSON payload using kitty's `@kitty-cmd` framing.
pub fn encode_remote_command_value(value: &Value) -> Result<Vec<u8>> {
    Ok(encode_message(value.to_json()?.as_bytes()))
}

/// Encode a raw remote-control payload using `@kitty-cmd` framing.
pub fn encode_remote_command(payload: &[u8]) -> Vec<u8> {
    encode_message(payload)
}

/// Encode a kitten result as `@kitty-kitten-result|` + base85(JSON).
pub fn encode_kitten_result(value: &Value) -> Result<Vec<u8>> {
    let json = value.to_json()?;
    let mut out = Vec::with_capacity(
        DCS_START.len() + KITTEN_RESULT_PREFIX.len() + json.len() * 5 / 4 + DCS_TERMINATOR.len(),
    );
    out.extend_from_slice(DCS_START);
    out.extend_from_slice(KITTEN_RESULT_PREFIX);
    out.extend_from_slice(&encode_b85(json.as_bytes()));
    out.extend_from_slice(DCS_TERMINATOR);
    Ok(out)
}

/// Encode an overlay-ready notification.
pub fn encode_overlay_ready() -> Vec<u8> {
    frame(OVERLAY_READY_PREFIX, &[])
}

/// Encode an overlay-open request.
pub fn encode_overlay(spec: &OverlaySpec) -> Result<Vec<u8>> {
    let mut object: Vec<(String, Value)> = Vec::new();
    if let Some(title) = &spec.title {
        object.push(("title".to_owned(), Value::from(title.as_str())));
    }
    if spec.ready {
        object.push(("ready".to_owned(), Value::Bool(true)));
    }
    if spec.close_on_exit {
        object.push(("close_on_exit".to_owned(), Value::Bool(true)));
    }
    if let Some(payload) = &spec.payload {
        object.push(("data".to_owned(), payload.clone()));
    }
    let json = Value::Object(object).to_json()?;
    Ok(frame(OVERLAY_PREFIX, json.as_bytes()))
}

/// Encode text for the remote-print channel. The text is base64 encoded, as in
/// kitty's `DebugPrintln` and `kittens.tui.loop.debug_write`.
pub fn encode_print(text: &str) -> Vec<u8> {
    frame(PRINT_PREFIX, &encode(text.as_bytes(), true))
}

/// Encode a window-title request.
pub fn encode_title(title: &str) -> Vec<u8> {
    frame(TITLE_PREFIX, title.as_bytes())
}

fn frame(prefix: &[u8], payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(DCS_START.len() + prefix.len() + payload.len() + 2);
    out.extend_from_slice(DCS_START);
    out.extend_from_slice(prefix);
    out.extend_from_slice(payload);
    out.extend_from_slice(DCS_TERMINATOR);
    out
}

/// Parse one DCS payload (the bytes between `ESC P` and `ESC \`).
pub fn parse_dcs_payload(payload: &[u8]) -> Result<KittenDcs> {
    if let Some(body) = payload.strip_prefix(CMD_PREFIX) {
        let json = json::parse(body).ok();
        return Ok(KittenDcs::RemoteCommand {
            body: body.to_vec(),
            json,
        });
    }
    if let Some(body) = payload.strip_prefix(KITTEN_RESULT_PREFIX) {
        let raw = decode_b85(body).map_err(|err| Error::InvalidBase85(err.to_string()))?;
        let value = json::parse(&raw).map_err(|err| Error::InvalidJson(err.to_string()))?;
        return Ok(KittenDcs::KittenResult(value));
    }
    if payload.starts_with(OVERLAY_READY_PREFIX) {
        return Ok(KittenDcs::OverlayReady);
    }
    if let Some(body) = payload.strip_prefix(OVERLAY_PREFIX) {
        return Ok(KittenDcs::Overlay(parse_overlay(body)?));
    }
    if let Some(body) = payload.strip_prefix(PRINT_PREFIX) {
        let raw = kitty_remote::base64::decode(body);
        return Ok(KittenDcs::Print(sanitize_print(&raw)));
    }
    if let Some(body) = payload.strip_prefix(TITLE_PREFIX) {
        return Ok(KittenDcs::Title(String::from_utf8_lossy(body).into_owned()));
    }
    let name = payload
        .strip_prefix(KITTY_PREFIX)
        .unwrap_or(payload)
        .split(|byte| *byte == b'|')
        .next()
        .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
        .unwrap_or_default();
    Ok(KittenDcs::Other {
        name,
        payload: payload.to_vec(),
    })
}

fn parse_overlay(body: &[u8]) -> Result<OverlaySpec> {
    if body.is_empty() {
        return Ok(OverlaySpec::default());
    }
    let value =
        json::parse(body).map_err(|err| Error::InvalidJson(format!("overlay metadata: {err}")))?;
    let Some(items) = value.as_object() else {
        return Err(Error::InvalidDcs(
            "overlay metadata must be a JSON object".to_owned(),
        ));
    };
    let mut spec = OverlaySpec::default();
    for (key, value) in items {
        match key.as_str() {
            "title" => {
                spec.title = value.as_str().map(str::to_owned);
            }
            "ready" => spec.ready = value.as_bool().unwrap_or(false),
            "close_on_exit" => spec.close_on_exit = value.as_bool().unwrap_or(false),
            "data" => spec.payload = Some(value.clone()),
            _ => {}
        }
    }
    Ok(spec)
}

/// Replace C0 control codes except newline, space and tab with the
/// corresponding Unicode control pictures, matching kitty's
/// `replace_c0_codes_except_nl_space_tab()`. DEL is mapped the same way.
pub fn sanitize_print(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .chars()
        .map(|ch| {
            let code = ch as u32;
            if (code <= 0x08) || (0x0b..=0x1f).contains(&code) || code == 0x7f {
                char::from_u32(0x2400 + code).unwrap_or(ch)
            } else {
                ch
            }
        })
        .collect()
}

/// Incremental parser for generic DCS frames.
///
/// It accepts any DCS payload, ignores noise before `ESC P`, and retains
/// partial frames. The payload limit defaults to 16 MiB.
#[derive(Clone, Debug)]
pub struct DcsFramer {
    buf: Vec<u8>,
    max_message_size: usize,
}

impl Default for DcsFramer {
    fn default() -> Self {
        Self::new()
    }
}

impl DcsFramer {
    pub fn new() -> Self {
        Self::with_max_message_size(DEFAULT_MAX_MESSAGE_SIZE)
    }

    pub fn with_max_message_size(max_message_size: usize) -> Self {
        Self {
            buf: Vec::new(),
            max_message_size,
        }
    }

    pub fn buffered_len(&self) -> usize {
        self.buf.len()
    }

    pub fn max_message_size(&self) -> usize {
        self.max_message_size
    }

    pub fn clear(&mut self) {
        self.buf.clear();
    }

    /// Feed bytes and return all complete DCS payloads.
    pub fn push(&mut self, data: &[u8]) -> Result<Vec<Vec<u8>>> {
        self.buf.extend_from_slice(data);
        let mut frames = Vec::new();
        loop {
            if !self.buf.starts_with(DCS_START) {
                match find_subslice(&self.buf, DCS_START) {
                    Some(pos) => {
                        self.buf.drain(..pos);
                    }
                    None => {
                        let keep = DCS_START.len().saturating_sub(1);
                        if self.buf.len() > keep {
                            let drain = self.buf.len() - keep;
                            self.buf.drain(..drain);
                        }
                        break;
                    }
                }
            }
            let search_from = DCS_START.len();
            match find_subslice(&self.buf[search_from..], DCS_TERMINATOR) {
                Some(relative) => {
                    let end = search_from + relative;
                    let payload_len = end - search_from;
                    if payload_len > self.max_message_size {
                        self.buf.clear();
                        return Err(Error::InvalidDcs(format!(
                            "payload of {payload_len} bytes exceeds the limit of {} bytes",
                            self.max_message_size
                        )));
                    }
                    frames.push(self.buf[search_from..end].to_vec());
                    self.buf.drain(..end + DCS_TERMINATOR.len());
                }
                None => {
                    let pending = self.buf.len() - search_from;
                    if pending > self.max_message_size {
                        self.buf.clear();
                        return Err(Error::InvalidDcs(format!(
                            "payload of {pending} bytes exceeds the limit of {} bytes",
                            self.max_message_size
                        )));
                    }
                    break;
                }
            }
        }
        Ok(frames)
    }

    /// Assert that no partial frame is buffered.
    pub fn finish(self) -> Result<()> {
        if self.buf.is_empty() {
            Ok(())
        } else {
            Err(Error::InvalidDcs("incomplete DCS frame".to_owned()))
        }
    }
}

/// Incremental parser producing [`KittenDcs`] values.
#[derive(Clone, Debug)]
pub struct KittenFramer {
    dcs: DcsFramer,
}

impl Default for KittenFramer {
    fn default() -> Self {
        Self::new()
    }
}

impl KittenFramer {
    pub fn new() -> Self {
        Self {
            dcs: DcsFramer::new(),
        }
    }

    pub fn with_max_message_size(max_message_size: usize) -> Self {
        Self {
            dcs: DcsFramer::with_max_message_size(max_message_size),
        }
    }

    pub fn buffered_len(&self) -> usize {
        self.dcs.buffered_len()
    }

    pub fn push(&mut self, data: &[u8]) -> Result<Vec<KittenDcs>> {
        let payloads = self.dcs.push(data)?;
        let mut events = Vec::with_capacity(payloads.len());
        for payload in payloads {
            events.push(parse_dcs_payload(&payload)?);
        }
        Ok(events)
    }

    pub fn finish(self) -> Result<()> {
        self.dcs.finish()
    }
}

/// Callback interface for terminal/window actions initiated by a kitten.
///
/// All methods have no-op defaults so an embedding application only needs to
/// implement the actions it supports. The methods take `&mut self`, allowing
/// the host to accumulate state or forward events.
pub trait KittenHost {
    /// Set the title of a window.
    fn set_window_title(&mut self, _window_id: u64, _title: &str) {}

    /// Open an overlay over a window.
    fn open_overlay(&mut self, _window_id: u64, _overlay: &OverlaySpec) {}

    /// The overlay is ready for input.
    fn overlay_ready(&mut self, _window_id: u64) {}

    /// Print text to the terminal.
    fn print(&mut self, _window_id: u64, _text: &str) {}

    /// Receive a remote-control result.
    fn remote_command_result(&mut self, _window_id: u64, _result: &Value) {}

    /// Receive the result returned by a kitten's `main()`/handler.
    fn kitten_result(&mut self, _window_id: u64, _result: &Value) {}
}

/// Dispatch one parsed event to a host.
pub fn dispatch_event<H: KittenHost>(host: &mut H, window_id: u64, event: &KittenDcs) {
    match event {
        KittenDcs::RemoteCommand {
            json: Some(value), ..
        } => host.remote_command_result(window_id, value),
        KittenDcs::KittenResult(value) => host.kitten_result(window_id, value),
        KittenDcs::OverlayReady => host.overlay_ready(window_id),
        KittenDcs::Overlay(spec) => {
            host.open_overlay(window_id, spec);
            if spec.ready {
                host.overlay_ready(window_id);
            }
        }
        KittenDcs::Print(text) => host.print(window_id, text),
        KittenDcs::Title(title) => host.set_window_title(window_id, title),
        KittenDcs::RemoteCommand { json: None, .. } | KittenDcs::Other { .. } => {}
    }
}

/// A [`KittenHost`] plus an incremental framer.
#[derive(Clone, Debug)]
pub struct KittenProtocol<H> {
    host: H,
    window_id: u64,
    framer: KittenFramer,
}

impl<H: KittenHost> KittenProtocol<H> {
    pub fn new(host: H, window_id: u64) -> Self {
        Self {
            host,
            window_id,
            framer: KittenFramer::new(),
        }
    }

    pub fn with_max_message_size(host: H, window_id: u64, max: usize) -> Self {
        Self {
            host,
            window_id,
            framer: KittenFramer::with_max_message_size(max),
        }
    }

    pub fn host(&self) -> &H {
        &self.host
    }

    pub fn host_mut(&mut self) -> &mut H {
        &mut self.host
    }

    pub fn window_id(&self) -> u64 {
        self.window_id
    }

    /// Feed bytes, dispatch every complete event to the host, and return the
    /// events.
    pub fn feed(&mut self, data: &[u8]) -> Result<Vec<KittenDcs>> {
        let events = self.framer.push(data)?;
        for event in &events {
            dispatch_event(&mut self.host, self.window_id, event);
        }
        Ok(events)
    }

    /// Dispatch an already parsed event.
    pub fn dispatch(&mut self, event: &KittenDcs) {
        dispatch_event(&mut self.host, self.window_id, event);
    }

    /// Assert there is no partial frame.
    pub fn finish(self) -> Result<()> {
        self.framer.finish()
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    if haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Default)]
    struct Recorder {
        titles: Vec<(u64, String)>,
        overlays: Vec<(u64, OverlaySpec)>,
        ready: Vec<u64>,
        prints: Vec<(u64, String)>,
        commands: Vec<(u64, Value)>,
        results: Vec<(u64, Value)>,
    }

    impl KittenHost for Recorder {
        fn set_window_title(&mut self, window_id: u64, title: &str) {
            self.titles.push((window_id, title.to_owned()));
        }

        fn open_overlay(&mut self, window_id: u64, overlay: &OverlaySpec) {
            self.overlays.push((window_id, overlay.clone()));
        }

        fn overlay_ready(&mut self, window_id: u64) {
            self.ready.push(window_id);
        }

        fn print(&mut self, window_id: u64, text: &str) {
            self.prints.push((window_id, text.to_owned()));
        }

        fn remote_command_result(&mut self, window_id: u64, result: &Value) {
            self.commands.push((window_id, result.clone()));
        }

        fn kitten_result(&mut self, window_id: u64, result: &Value) {
            self.results.push((window_id, result.clone()));
        }
    }

    fn sample_value() -> Value {
        Value::object_from([
            ("answer", Value::from(42_i64)),
            ("text", Value::from("hello")),
        ])
    }

    fn collect_split_at_every_byte(framed: &[u8]) {
        for split in 0..=framed.len() {
            let mut framer = KittenFramer::new();
            let mut events = framer.push(&framed[..split]).unwrap();
            events.extend(framer.push(&framed[split..]).unwrap());
            assert_eq!(events.len(), 1, "split at {split}");
            framer.finish().unwrap();
        }
        let mut framer = KittenFramer::new();
        let mut events = Vec::new();
        for byte in framed {
            events.extend(framer.push(std::slice::from_ref(byte)).unwrap());
        }
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn result_round_trip_and_every_byte_split() {
        let value = sample_value();
        let framed = encode_kitten_result(&value).unwrap();
        collect_split_at_every_byte(&framed);
        let events = KittenFramer::new().push(&framed).unwrap();
        assert_eq!(events, vec![KittenDcs::KittenResult(value)]);
    }

    #[test]
    fn overlay_ready_round_trip_and_every_byte_split() {
        let framed = encode_overlay_ready();
        collect_split_at_every_byte(&framed);
        let events = KittenFramer::new().push(&framed).unwrap();
        assert_eq!(events, vec![KittenDcs::OverlayReady]);
    }

    #[test]
    fn overlay_round_trip_and_every_byte_split() {
        let spec = OverlaySpec {
            title: Some("Demo".to_owned()),
            ready: true,
            close_on_exit: true,
            payload: Some(Value::from("data")),
        };
        let framed = encode_overlay(&spec).unwrap();
        collect_split_at_every_byte(&framed);
        let events = KittenFramer::new().push(&framed).unwrap();
        assert_eq!(events, vec![KittenDcs::Overlay(spec)]);
    }

    #[test]
    fn print_round_trip_and_every_byte_split() {
        let framed = encode_print("hello\nworld\t!");
        collect_split_at_every_byte(&framed);
        let events = KittenFramer::new().push(&framed).unwrap();
        assert_eq!(events, vec![KittenDcs::Print("hello\nworld\t!".to_owned())]);
    }

    #[test]
    fn title_round_trip_and_every_byte_split() {
        let framed = encode_title("A title");
        collect_split_at_every_byte(&framed);
        let events = KittenFramer::new().push(&framed).unwrap();
        assert_eq!(events, vec![KittenDcs::Title("A title".to_owned())]);
    }

    #[test]
    fn remote_command_round_trip_and_every_byte_split() {
        let value = Value::object_from([("cmd", Value::from("ls")), ("ok", Value::Bool(true))]);
        let framed = encode_remote_command_value(&value).unwrap();
        assert!(framed.starts_with(DCS_START));
        collect_split_at_every_byte(&framed);
        let events = KittenFramer::new().push(&framed).unwrap();
        assert_eq!(
            events,
            vec![KittenDcs::RemoteCommand {
                body: value.to_json().unwrap().into_bytes(),
                json: Some(value)
            }]
        );
    }

    #[test]
    fn multiple_frames_and_noise() {
        let mut wire = b"noise\x1bXstill noise".to_vec();
        wire.extend_from_slice(&encode_overlay_ready());
        wire.extend_from_slice(&encode_print("one"));
        wire.extend_from_slice(&encode_kitten_result(&Value::from(2_i64)).unwrap());
        let events = KittenFramer::new().push(&wire).unwrap();
        assert_eq!(events.len(), 3);
        assert_eq!(events[0], KittenDcs::OverlayReady);
        assert_eq!(events[1], KittenDcs::Print("one".to_owned()));
        assert_eq!(events[2], KittenDcs::KittenResult(Value::from(2_i64)));
    }

    #[test]
    fn host_callbacks_are_invoked() {
        let mut protocol = KittenProtocol::new(Recorder::default(), 9);
        let mut wire = encode_title("T").to_vec();
        wire.extend_from_slice(
            &encode_overlay(&OverlaySpec {
                title: Some("O".to_owned()),
                ready: true,
                close_on_exit: false,
                payload: None,
            })
            .unwrap(),
        );
        wire.extend_from_slice(&encode_print("P"));
        wire.extend_from_slice(&encode_remote_command_value(&Value::from("R")).unwrap());
        wire.extend_from_slice(&encode_kitten_result(&Value::from("K")).unwrap());
        let events = protocol.feed(&wire).unwrap();
        assert_eq!(events.len(), 5);
        let host = protocol.host();
        assert_eq!(host.titles, vec![(9, "T".to_owned())]);
        assert_eq!(host.overlays.len(), 1);
        assert_eq!(host.overlays[0].0, 9);
        assert_eq!(host.ready, vec![9]);
        assert_eq!(host.prints, vec![(9, "P".to_owned())]);
        assert_eq!(host.commands, vec![(9, Value::from("R"))]);
        assert_eq!(host.results, vec![(9, Value::from("K"))]);
    }

    #[test]
    fn malformed_result_and_overlay_are_errors() {
        let bad_b85 = frame(KITTEN_RESULT_PREFIX, b"not valid base85!!!");
        assert!(KittenFramer::new().push(&bad_b85).is_err());
        let bad_json = frame(KITTEN_RESULT_PREFIX, b"!!!!");
        assert!(KittenFramer::new().push(&bad_json).is_err());
        let bad_overlay = frame(OVERLAY_PREFIX, b"[]");
        assert!(KittenFramer::new().push(&bad_overlay).is_err());
    }

    #[test]
    fn incomplete_frame_is_reported() {
        let framed = encode_overlay_ready();
        let mut framer = KittenFramer::new();
        assert!(framer.push(&framed[..5]).unwrap().is_empty());
        assert!(matches!(framer.clone().finish(), Err(Error::InvalidDcs(_))));
    }

    #[test]
    fn message_size_limit_is_enforced() {
        let mut framer = KittenFramer::with_max_message_size(4);
        let framed = encode_title("12345");
        assert!(matches!(framer.push(&framed), Err(Error::InvalidDcs(_))));
        assert_eq!(framer.buffered_len(), 0);
    }

    #[test]
    fn print_sanitizes_control_codes() {
        let mut payload = Vec::new();
        payload.extend_from_slice(b"a");
        payload.push(1);
        payload.extend_from_slice(b"\n\t ");
        payload.push(0x7f);
        let text = sanitize_print(&payload);
        assert_eq!(text, "a\u{2401}\n\t \u{247f}");
    }

    #[test]
    fn unknown_kitty_frame_is_preserved() {
        let payload = b"@kitty-echo|abc";
        let framed = frame(b"", payload);
        let events = KittenFramer::new().push(&framed).unwrap();
        assert_eq!(
            events,
            vec![KittenDcs::Other {
                name: "echo".to_owned(),
                payload: payload.to_vec()
            }]
        );
    }

    #[test]
    fn empty_host_methods_are_available() {
        struct Empty;
        impl KittenHost for Empty {}
        let mut protocol = KittenProtocol::new(Empty, 1);
        protocol
            .feed(&encode_kitten_result(&Value::Null).unwrap())
            .unwrap();
        assert_eq!(protocol.window_id(), 1);
    }
}
