// SPDX-License-Identifier: GPL-3.0-or-later
//! Remote-control client helper used by the tests and examples.
//!
//! The helper mirrors kitty's `SocketIO` / `CommandEncrypter` behaviour: it
//! writes DCS-framed commands, wraps them in the X25519/AES-256-GCM envelope
//! when a server public key is supplied, and reads responses and asynchronous
//! notifications with the same framer as the server.
//!
//! Reads can be forced to one byte at a time with
//! [`ClientConfig::split_reads`], which exercises the incremental framing path.

use crate::base64::encode_b85;
use crate::commands::RC_ENCRYPTION_PROTOCOL_VERSION;
use crate::error::{Error, Result};
use crate::framing::{encode_message, Framer, DEFAULT_MAX_MESSAGE_SIZE};
use crate::json::{self, Value};
use crate::protocol::CryptoState;
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::os::unix::net::{SocketAddr, UnixStream};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Client configuration.
#[derive(Clone, Debug)]
pub struct ClientConfig {
    /// Version sent in envelopes and inner commands.
    pub version: String,
    /// Maximum accepted frame/payload size.
    pub max_message_size: usize,
    /// Read one byte at a time to exercise split-buffer framing.
    pub split_reads: bool,
    /// Optional socket read timeout.
    pub read_timeout: Option<Duration>,
    /// Password added to every command when encryption/authentication is used.
    pub password: Option<String>,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            version: crate::commands::KITTY_COMPAT_VERSION.to_string(),
            max_message_size: DEFAULT_MAX_MESSAGE_SIZE,
            split_reads: false,
            read_timeout: Some(Duration::from_secs(10)),
            password: None,
        }
    }
}

/// An asynchronous notification received from the server.
#[derive(Clone, Debug, PartialEq)]
pub struct Notification {
    /// Event name, for example `window_changed`.
    pub event: String,
    /// Event data.
    pub data: Value,
    /// Complete JSON object as received.
    pub raw: Value,
}

/// One incoming message.
#[derive(Clone, Debug, PartialEq)]
pub enum Incoming {
    /// A command response, including the asynchronous screenshot reply.
    Response(Value),
    /// An event notification.
    Notification(Notification),
}

/// A connected remote-control client.
pub struct Client {
    stream: UnixStream,
    framer: Framer,
    ready: VecDeque<Vec<u8>>,
    server_public_key: Option<Vec<u8>>,
    config: ClientConfig,
    next_id: u64,
    notifications: Vec<Notification>,
    pending_responses: Vec<Value>,
}

impl Client {
    /// Connect to `path` in plain mode.
    pub fn connect_plain(path: impl AsRef<Path>) -> Result<Self> {
        Self::connect(path, None, ClientConfig::default())
    }

    /// Connect to `path` with encryption using the server's raw public key.
    pub fn connect_encrypted(path: impl AsRef<Path>, server_public_key: &[u8]) -> Result<Self> {
        Self::connect(
            path,
            Some(server_public_key.to_vec()),
            ClientConfig::default(),
        )
    }

    /// Connect to an abstract-namespace Unix socket in plain mode.
    pub fn connect_addr_plain(addr: &SocketAddr) -> Result<Self> {
        Self::connect_addr(addr, None, ClientConfig::default())
    }

    /// Connect to an abstract-namespace Unix socket with X25519/AES-GCM
    /// encryption using the server's raw public key.
    pub fn connect_addr_encrypted(addr: &SocketAddr, server_public_key: &[u8]) -> Result<Self> {
        Self::connect_addr(
            addr,
            Some(server_public_key.to_vec()),
            ClientConfig::default(),
        )
    }

    /// Connect to an explicit [`SocketAddr`] (path or abstract namespace).
    pub fn connect_addr(
        addr: &SocketAddr,
        server_public_key: Option<Vec<u8>>,
        config: ClientConfig,
    ) -> Result<Self> {
        let stream = UnixStream::connect_addr(addr)?;
        Self::from_stream(stream, server_public_key, config)
    }

    /// Connect with an explicit public key (`None` for plain mode) and config.
    pub fn connect(
        path: impl AsRef<Path>,
        server_public_key: Option<Vec<u8>>,
        config: ClientConfig,
    ) -> Result<Self> {
        let stream = UnixStream::connect(path.as_ref())?;
        Self::from_stream(stream, server_public_key, config)
    }

    fn from_stream(
        stream: UnixStream,
        server_public_key: Option<Vec<u8>>,
        config: ClientConfig,
    ) -> Result<Self> {
        if let Some(timeout) = config.read_timeout {
            stream.set_read_timeout(Some(timeout))?;
        }
        stream.set_write_timeout(Some(Duration::from_secs(10)))?;
        Ok(Self {
            stream,
            framer: Framer::with_max_message_size(config.max_message_size),
            ready: VecDeque::new(),
            server_public_key,
            config,
            next_id: 1,
            notifications: Vec::new(),
            pending_responses: Vec::new(),
        })
    }

    /// Whether this client encrypts its commands.
    pub fn is_encrypted(&self) -> bool {
        self.server_public_key.is_some()
    }

    /// Mutable access to the configuration.
    pub fn config_mut(&mut self) -> &mut ClientConfig {
        &mut self.config
    }

    /// Read configuration.
    pub fn config(&self) -> &ClientConfig {
        &self.config
    }

    /// Send a command with an explicit correlation id, or no id when `None`.
    pub fn send(&mut self, mut request: Value, id: Option<Value>) -> Result<()> {
        if !request.is_object() {
            return Err(Error::Protocol(
                "a remote-control command must be a JSON object".into(),
            ));
        }
        if request.get("version").is_none() {
            request.set("version", Value::String(self.config.version.clone()));
        }
        if let Some(id) = id {
            request.set("id", id);
        }
        if let Some(password) = &self.config.password {
            request.set("password", Value::String(password.clone()));
        }
        let envelope = match &self.server_public_key {
            Some(public_key) => seal_request_with_timestamp(
                public_key,
                request,
                &self.config.version,
                now_ns(),
                self.config.max_message_size,
            )?,
            None => request,
        };
        let text = envelope.to_json()?;
        let frame = encode_message(text.as_bytes());
        self.stream.write_all(&frame)?;
        self.stream.flush()?;
        Ok(())
    }

    /// Send a command with an automatically allocated numeric id.
    pub fn send_auto(&mut self, request: Value) -> Result<Value> {
        let id = Value::from(self.next_id);
        self.next_id = self.next_id.wrapping_add(1).max(1);
        self.send(request, Some(id.clone()))?;
        Ok(id)
    }

    /// Send a command and wait for its response.
    pub fn command(&mut self, request: Value) -> Result<Value> {
        let id = self.send_auto(request)?;
        self.wait_for_id(&id)
    }

    /// Send a command without an id (exact kitty wire shape) and wait for the
    /// first response.
    pub fn command_no_id(&mut self, request: Value) -> Result<Value> {
        self.send(request, None)?;
        self.wait_for_any()
    }

    /// Send several commands (with ids) without waiting for replies.
    pub fn send_concurrent(&mut self, requests: Vec<Value>) -> Result<Vec<Value>> {
        let mut ids = Vec::with_capacity(requests.len());
        for request in requests {
            ids.push(self.send_auto(request)?);
        }
        Ok(ids)
    }

    /// Wait for a response with the given id, buffering notifications and
    /// unrelated responses.
    pub fn wait_for_id(&mut self, id: &Value) -> Result<Value> {
        if let Some(index) = self
            .pending_responses
            .iter()
            .position(|response| response.get("id") == Some(id))
        {
            return Ok(self.pending_responses.remove(index));
        }
        loop {
            match self.receive()? {
                Incoming::Notification(notification) => self.notifications.push(notification),
                Incoming::Response(response) => {
                    if response.get("id") == Some(id) {
                        return Ok(response);
                    }
                    self.pending_responses.push(response);
                }
            }
        }
    }

    /// Wait for the next response, buffering notifications.
    pub fn wait_for_any(&mut self) -> Result<Value> {
        if !self.pending_responses.is_empty() {
            return Ok(self.pending_responses.remove(0));
        }
        loop {
            match self.receive()? {
                Incoming::Notification(notification) => self.notifications.push(notification),
                Incoming::Response(response) => return Ok(response),
            }
        }
    }

    /// Receive the next message without buffering it.
    pub fn receive(&mut self) -> Result<Incoming> {
        loop {
            if self.ready.is_empty() {
                self.read_more()?;
            }
            let Some(payload) = self.ready.pop_front() else {
                continue;
            };
            let value = json::parse(&payload)?;
            if let Some(event) = value.get("event").and_then(Value::as_str) {
                let data = value.get("data").cloned().unwrap_or(Value::Null);
                return Ok(Incoming::Notification(Notification {
                    event: event.to_string(),
                    data,
                    raw: value,
                }));
            }
            return Ok(Incoming::Response(value));
        }
    }

    /// Receive only the next notification, buffering responses.
    pub fn wait_for_notification(&mut self) -> Result<Notification> {
        loop {
            match self.receive()? {
                Incoming::Notification(notification) => return Ok(notification),
                Incoming::Response(response) => self.pending_responses.push(response),
            }
        }
    }

    /// Notifications collected by [`Self::command`] and friends.
    pub fn notifications(&self) -> &[Notification] {
        &self.notifications
    }

    /// Drain collected notifications.
    pub fn take_notifications(&mut self) -> Vec<Notification> {
        std::mem::take(&mut self.notifications)
    }

    /// Half-close the write side, like kitty's `SocketIO.__exit__`.
    pub fn shutdown_write(&self) -> Result<()> {
        self.stream.shutdown(std::net::Shutdown::Write)?;
        Ok(())
    }

    /// Borrow the underlying socket (useful for tests).
    pub fn stream(&self) -> &UnixStream {
        &self.stream
    }

    fn read_more(&mut self) -> Result<()> {
        let mut buffer = [0u8; 8192];
        loop {
            let read = if self.config.split_reads {
                self.stream.read(&mut buffer[..1])?
            } else {
                self.stream.read(&mut buffer)?
            };
            if read == 0 {
                return Err(Error::ConnectionClosed);
            }
            let messages = self.framer.push(&buffer[..read])?;
            if !messages.is_empty() {
                self.ready.extend(messages);
                return Ok(());
            }
        }
    }
}

fn now_ns() -> i128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos() as i128)
        .unwrap_or(0)
}

/// Seal a request in kitty's encrypted envelope using the current time.
pub fn seal_request(
    server_public_key: &[u8],
    request: Value,
    version: &str,
    max_message_size: usize,
) -> Result<Value> {
    seal_request_with_timestamp(
        server_public_key,
        request,
        version,
        now_ns(),
        max_message_size,
    )
}

/// Seal a request with an explicit timestamp (used to test replay rejection).
pub fn seal_request_with_timestamp(
    server_public_key: &[u8],
    mut request: Value,
    version: &str,
    timestamp_ns: i128,
    max_message_size: usize,
) -> Result<Value> {
    if !request.is_object() {
        return Err(Error::Protocol(
            "a remote-control command must be a JSON object".into(),
        ));
    }
    if request.get("version").is_none() {
        request.set("version", Value::String(version.to_string()));
    }
    let timestamp = u64::try_from(timestamp_ns).map_err(|_| {
        Error::Protocol("the encrypted command timestamp must be non-negative".into())
    })?;
    request.set("timestamp", Value::from(timestamp));
    let plaintext = request.to_json()?;
    let state = CryptoState::new_client(server_public_key)?.with_max_message_size(max_message_size);
    let sealed = state.seal(plaintext.as_bytes())?;
    let envelope = Value::object_from([
        ("version", Value::String(version.to_string())),
        ("iv", Value::String(b85(&sealed.iv))),
        ("tag", Value::String(b85(&sealed.tag))),
        ("pubkey", Value::String(state.public_key_base85()?)),
        ("encrypted", Value::String(b85(&sealed.ciphertext))),
    ]);
    Ok(envelope)
}

fn b85(data: &[u8]) -> String {
    String::from_utf8(encode_b85(data)).expect("base85 is ASCII")
}

/// Decode a `KITTY_PUBLIC_KEY` value (`<proto>:<base85-public-key>`).
pub fn parse_public_key_env(value: &str) -> Result<Vec<u8>> {
    let (protocol, encoded) = value
        .split_once(':')
        .ok_or_else(|| Error::Protocol("KITTY_PUBLIC_KEY has no protocol prefix".into()))?;
    if protocol != RC_ENCRYPTION_PROTOCOL_VERSION {
        return Err(Error::Protocol(format!(
            "unsupported KITTY_PUBLIC_KEY protocol {protocol}"
        )));
    }
    crate::base64::decode_b85(encoded.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_key_env_roundtrip() {
        let bytes = crate::base64::encode_b85(b"hello");
        let text = format!("1:{}", String::from_utf8(bytes).unwrap());
        assert_eq!(parse_public_key_env(&text).unwrap(), b"hello");
        assert!(parse_public_key_env("2:abc").is_err());
    }
}
