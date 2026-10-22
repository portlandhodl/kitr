// SPDX-License-Identifier: GPL-3.0-or-later
//! Unix-socket remote-control server.
//!
//! This layers kitty's socket behaviour on top of the existing
//! [`crate::crypto`] and [`crate::framing`] primitives:
//!
//! * one encrypted or plain JSON command per DCS frame
//!   (`ESC P @kitty-cmd ... ESC \`);
//! * kitty's X25519 + AES-256-GCM envelope (`pubkey`, `iv`, `tag`,
//!   `encrypted`, `enc_proto`) when encryption is enabled;
//! * `SO_PEERCRED` uid verification for local Unix peers where the platform
//!   exposes it;
//! * response correlation through an optional `id` field (a backwards
//!   compatible extension: kitty clients do not send it and see exactly the
//!   usual `{"ok": ...}` shape);
//! * asynchronous screenshot replies and event notifications;
//! * a configurable inbound message-size limit.
//!
//! Plain local mode matches kitty's `NoEncryption` path and is useful for
//! trusted local test harnesses.

use crate::base64::decode_b85;
use crate::commands::{
    error_response, execute, AsyncResponseFn, CommandError, DispatchOutcome, PeerInfo,
    RemoteHandler, RequestContext, KITTY_COMPAT_VERSION, RC_ENCRYPTION_PROTOCOL_VERSION,
};
use crate::crypto::{EllipticCurveKey, Secret, GCM_IV_LEN, GCM_TAG_LEN};
use crate::error::{Error, Result};
use crate::framing::{encode_message, Framer, DEFAULT_MAX_MESSAGE_SIZE};
use crate::json::{self, Value};
use crate::protocol::{CryptoState, SealedMessage};
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::{SocketAddr, UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Maximum accepted timestamp skew for encrypted commands (kitty uses five
/// minutes).
pub const MAX_TIMESTAMP_SKEW_NS: i128 = 5 * 60 * 1_000_000_000;

/// Whether plain (unencrypted) commands are accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EncryptionMode {
    /// Accept both encrypted and plain commands (kitty's behaviour when a
    /// local client does not have `KITTY_PUBLIC_KEY`).
    Optional,
    /// Reject plain commands.
    Required,
    /// Reject encrypted commands and only accept plain local commands.
    Disabled,
}

/// Policy for verifying the Unix peer's credentials.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PeerCredentialPolicy {
    /// Do not check the peer uid.
    Any,
    /// Require the peer's effective uid to equal the server's effective uid.
    SameUid,
}

/// Server configuration.
#[derive(Clone, Debug)]
pub struct ServerConfig {
    /// Maximum payload size accepted by the DCS framer and AES-GCM state.
    pub max_message_size: usize,
    /// Encryption policy.
    pub encryption: EncryptionMode,
    /// Peer credential policy.
    pub peer_credentials: PeerCredentialPolicy,
    /// If set, requests must carry this password.
    pub password: Option<String>,
    /// Version reported to clients. Kitty only compares the first two
    /// components against the client version.
    pub version: String,
    /// Close an idle connection after this long. Pending asynchronous
    /// screenshot replies extend the wait.
    pub idle_timeout: Duration,
    /// Socket read/write and shutdown polling granularity.
    pub poll_interval: Duration,
    /// Maximum simultaneous connections.
    pub max_connections: usize,
    /// Permissions applied to the socket path.
    pub socket_mode: Option<u32>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            max_message_size: DEFAULT_MAX_MESSAGE_SIZE,
            encryption: EncryptionMode::Optional,
            peer_credentials: default_peer_policy(),
            password: None,
            version: KITTY_COMPAT_VERSION.to_string(),
            idle_timeout: Duration::from_secs(30),
            poll_interval: Duration::from_millis(25),
            max_connections: 256,
            socket_mode: Some(0o600),
        }
    }
}

impl ServerConfig {
    /// A configuration suitable for a local trusted test harness: encrypted or
    /// plain mode, same-uid credentials, short timeouts and a small message
    /// limit.
    pub fn local_test() -> Self {
        Self {
            max_message_size: 1024,
            encryption: EncryptionMode::Optional,
            peer_credentials: PeerCredentialPolicy::SameUid,
            password: None,
            version: KITTY_COMPAT_VERSION.to_string(),
            idle_timeout: Duration::from_secs(5),
            poll_interval: Duration::from_millis(5),
            max_connections: 32,
            socket_mode: Some(0o600),
        }
    }
}

fn default_peer_policy() -> PeerCredentialPolicy {
    #[cfg(target_os = "linux")]
    {
        PeerCredentialPolicy::SameUid
    }
    #[cfg(not(target_os = "linux"))]
    {
        PeerCredentialPolicy::Any
    }
}

/// The long-lived X25519 key kitty publishes via `KITTY_PUBLIC_KEY`.
#[derive(Clone)]
pub struct ServerKey {
    private: Secret,
    public: Vec<u8>,
}

impl ServerKey {
    /// Generate a fresh server key.
    pub fn generate() -> Result<Self> {
        let key = EllipticCurveKey::generate()?;
        Self::from_key(&key)
    }

    /// Construct a server key from a raw X25519 private key.
    pub fn from_private_bytes(private: &[u8]) -> Result<Self> {
        let key = EllipticCurveKey::from_private_bytes(private)?;
        Self::from_key(&key)
    }

    fn from_key(key: &EllipticCurveKey) -> Result<Self> {
        Ok(Self {
            private: key.private_key()?,
            public: key.public_key()?,
        })
    }

    /// Raw public key bytes.
    pub fn public_key(&self) -> &[u8] {
        &self.public
    }

    /// Base85 public key, as used by the `pubkey` envelope field.
    pub fn public_key_base85(&self) -> String {
        String::from_utf8(crate::base64::encode_b85(&self.public)).expect("base85 is ASCII")
    }

    /// The value kitty puts in `KITTY_PUBLIC_KEY` (`<proto>:<base85>`).
    pub fn public_key_env(&self) -> String {
        format!(
            "{RC_ENCRYPTION_PROTOCOL_VERSION}:{}",
            self.public_key_base85()
        )
    }

    fn crypto_state(&self, max_message_size: usize) -> Result<CryptoState> {
        let key = EllipticCurveKey::from_private_bytes(self.private.as_slice())?;
        Ok(CryptoState::new_server_with_key(key).with_max_message_size(max_message_size))
    }
}

impl std::fmt::Debug for ServerKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerKey")
            .field("public_key_base85", &self.public_key_base85())
            .finish_non_exhaustive()
    }
}

/// A bound but not yet running server.
pub struct RemoteServer {
    listener: UnixListener,
    path: PathBuf,
    key: ServerKey,
    handler: Arc<dyn RemoteHandler>,
    config: Arc<ServerConfig>,
    registry: Arc<Registry>,
}

impl RemoteServer {
    /// Bind a Unix socket at `path`, generating a fresh server key.
    pub fn bind(
        path: impl AsRef<Path>,
        handler: Arc<dyn RemoteHandler>,
        config: ServerConfig,
    ) -> Result<Self> {
        Self::bind_with_key(path, handler, config, ServerKey::generate()?)
    }

    /// Bind with a caller-supplied server key (useful for tests and stable
    /// `KITTY_PUBLIC_KEY` values).
    pub fn bind_with_key(
        path: impl AsRef<Path>,
        handler: Arc<dyn RemoteHandler>,
        config: ServerConfig,
        key: ServerKey,
    ) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        remove_stale_socket(&path)?;
        let listener = UnixListener::bind(&path)?;
        listener.set_nonblocking(true)?;
        if let Some(mode) = config.socket_mode {
            fs::set_permissions(&path, fs::Permissions::from_mode(mode))?;
        }
        Ok(Self {
            listener,
            path,
            key,
            handler,
            config: Arc::new(config),
            registry: Arc::new(Registry::default()),
        })
    }

    /// Bind an abstract-namespace Unix socket (Linux `unix:@name`).
    ///
    /// This is a small convenience wrapper over [`UnixListener::bind_addr`];
    /// the caller constructs the [`SocketAddr`] with
    /// `std::os::linux::net::SocketAddrExt::from_abstract_name`.
    pub fn bind_addr(
        addr: &SocketAddr,
        handler: Arc<dyn RemoteHandler>,
        config: ServerConfig,
    ) -> Result<Self> {
        let key = ServerKey::generate()?;
        let listener = UnixListener::bind_addr(addr)?;
        listener.set_nonblocking(true)?;
        // Abstract sockets have no filesystem path to unlink on shutdown.
        let path = PathBuf::new();
        Ok(Self {
            listener,
            path,
            key,
            handler,
            config: Arc::new(config),
            registry: Arc::new(Registry::default()),
        })
    }

    /// The server's published public key.
    pub fn key(&self) -> &ServerKey {
        &self.key
    }

    /// Socket path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Start the accept loop on a background thread.
    pub fn start(self) -> Result<ServerHandle> {
        let shutdown = Arc::new(AtomicBool::new(false));
        let registry = self.registry.clone();
        let path = self.path.clone();
        let key = self.key.clone();
        let thread_shutdown = shutdown.clone();
        let thread = thread::Builder::new()
            .name("kitty-rc-server".into())
            .spawn(move || {
                accept_loop(
                    self.listener,
                    self.handler,
                    self.config,
                    self.registry,
                    self.key,
                    thread_shutdown,
                );
            })?;
        Ok(ServerHandle {
            shutdown,
            path,
            key,
            registry,
            thread: Some(thread),
            stopped: false,
        })
    }

    /// Run the accept loop on the current thread.
    pub fn run(self) -> Result<()> {
        accept_loop(
            self.listener,
            self.handler,
            self.config,
            self.registry,
            self.key,
            Arc::new(AtomicBool::new(false)),
        );
        Ok(())
    }
}

/// Handle to a running server.
pub struct ServerHandle {
    shutdown: Arc<AtomicBool>,
    path: PathBuf,
    key: ServerKey,
    registry: Arc<Registry>,
    thread: Option<JoinHandle<()>>,
    stopped: bool,
}

impl ServerHandle {
    /// Socket path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Server public key.
    pub fn key(&self) -> &ServerKey {
        &self.key
    }

    /// Value suitable for `KITTY_PUBLIC_KEY`.
    pub fn public_key_env(&self) -> String {
        self.key.public_key_env()
    }

    /// Number of currently registered connections.
    pub fn connection_count(&self) -> usize {
        self.registry.len()
    }

    /// Send an event notification to every connected client. The wire shape is
    /// `{"ok": true, "event": "...", "data": ...}`. Returns the number of
    /// clients the notification was written to.
    pub fn notify_event(&self, event: impl AsRef<str>, data: Value) -> usize {
        let value = notification_value(event.as_ref(), data);
        self.registry.broadcast(&value)
    }

    /// Stop accepting connections, close peers, join the accept thread and
    /// remove the socket path. Idempotent.
    pub fn shutdown(&mut self) {
        if self.stopped {
            return;
        }
        self.stopped = true;
        self.shutdown.store(true, Ordering::SeqCst);
        self.registry.shutdown_all();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = fs::remove_file(&self.path);
    }

    /// Stop the server and wait for threads to finish.
    pub fn wait(mut self) {
        self.shutdown();
    }
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Build the notification object written to clients.
pub fn notification_value(event: &str, data: Value) -> Value {
    Value::object_from([
        ("ok", Value::Bool(true)),
        ("event", Value::String(event.to_string())),
        ("data", data),
    ])
}

fn remove_stale_socket(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_socket() {
                fs::remove_file(path)?;
                Ok(())
            } else {
                Err(Error::Protocol(format!(
                    "refusing to replace non-socket path {}",
                    path.display()
                )))
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(Error::Io(error)),
    }
}

struct Connection {
    writer: Mutex<UnixStream>,
    peer: PeerInfo,
    pending_async: AtomicUsize,
    closed: AtomicBool,
}

impl Connection {
    fn new(writer: UnixStream, peer: PeerInfo) -> Self {
        Self {
            writer: Mutex::new(writer),
            peer,
            pending_async: AtomicUsize::new(0),
            closed: AtomicBool::new(false),
        }
    }

    fn send_value(&self, value: &Value) -> std::result::Result<(), String> {
        if self.closed.load(Ordering::Relaxed) {
            return Err("connection is closed".into());
        }
        let text = value.to_json().map_err(|error| error.to_string())?;
        let frame = encode_message(text.as_bytes());
        let mut writer = self
            .writer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        writer.write_all(&frame).map_err(|error| error.to_string())
    }

    fn send_notification(&self, event: &str, data: Value) -> std::result::Result<(), String> {
        self.send_value(&notification_value(event, data))
    }

    fn async_done(&self) {
        let _ = self
            .pending_async
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
                value.checked_sub(1)
            });
    }

    fn shutdown(&self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        let writer = self
            .writer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _ = writer.shutdown(std::net::Shutdown::Both);
    }
}

#[derive(Default)]
struct Registry {
    connections: Mutex<Vec<Weak<Connection>>>,
}

impl Registry {
    fn register(&self, connection: &Arc<Connection>) {
        let mut connections = self
            .connections
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        connections.retain(|weak| weak.strong_count() > 0);
        connections.push(Arc::downgrade(connection));
    }

    fn len(&self) -> usize {
        let mut connections = self
            .connections
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        connections.retain(|weak| weak.strong_count() > 0);
        connections.len()
    }

    fn broadcast(&self, value: &Value) -> usize {
        let connections: Vec<Arc<Connection>> = {
            let mut connections = self
                .connections
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            connections.retain(|weak| weak.strong_count() > 0);
            connections.iter().filter_map(Weak::upgrade).collect()
        };
        let mut sent = 0;
        for connection in connections {
            if connection.send_value(value).is_ok() {
                sent += 1;
            }
        }
        sent
    }

    fn shutdown_all(&self) {
        let connections: Vec<Arc<Connection>> = {
            let mut connections = self
                .connections
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            connections
                .drain(..)
                .filter_map(|weak| weak.upgrade())
                .collect()
        };
        for connection in connections {
            connection.shutdown();
        }
    }
}

fn accept_loop(
    listener: UnixListener,
    handler: Arc<dyn RemoteHandler>,
    config: Arc<ServerConfig>,
    registry: Arc<Registry>,
    key: ServerKey,
    shutdown: Arc<AtomicBool>,
) {
    let mut connections: Vec<JoinHandle<()>> = Vec::new();
    while !shutdown.load(Ordering::SeqCst) {
        connections.retain(|thread| !thread.is_finished());
        match listener.accept() {
            Ok((stream, _)) => {
                let _ = stream.set_nonblocking(false);
                let peer = peer_info(&stream);
                if !peer_allowed(peer, config.peer_credentials) {
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                    continue;
                }
                if registry.len() >= config.max_connections {
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                    continue;
                }
                let writer = match stream.try_clone() {
                    Ok(writer) => writer,
                    Err(_) => continue,
                };
                let _ = writer
                    .set_write_timeout(Some(config.poll_interval.max(Duration::from_millis(100))));
                let connection = Arc::new(Connection::new(writer, peer));
                registry.register(&connection);
                let handler = handler.clone();
                let config = config.clone();
                let key = key.clone();
                let registry = registry.clone();
                let shutdown = shutdown.clone();
                if let Ok(thread) =
                    thread::Builder::new()
                        .name("kitty-rc-peer".into())
                        .spawn(move || {
                            connection_loop(
                                stream, connection, handler, config, key, registry, shutdown,
                            );
                        })
                {
                    connections.push(thread);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(config.poll_interval);
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => {
                if shutdown.load(Ordering::SeqCst) {
                    break;
                }
                thread::sleep(config.poll_interval);
            }
        }
    }
    for thread in connections {
        let _ = thread.join();
    }
}

#[allow(clippy::too_many_arguments)]
fn connection_loop(
    mut stream: UnixStream,
    connection: Arc<Connection>,
    handler: Arc<dyn RemoteHandler>,
    config: Arc<ServerConfig>,
    key: ServerKey,
    registry: Arc<Registry>,
    shutdown: Arc<AtomicBool>,
) {
    let _ = stream.set_read_timeout(Some(config.poll_interval));
    let mut framer = Framer::with_max_message_size(config.max_message_size);
    let mut buffer = [0u8; 8192];
    let mut last_activity = Instant::now();
    loop {
        if shutdown.load(Ordering::SeqCst) {
            break;
        }
        match stream.read(&mut buffer) {
            Ok(0) => {
                wait_for_async(&connection, &config, &shutdown);
                break;
            }
            Ok(read) => {
                last_activity = Instant::now();
                match framer.push(&buffer[..read]) {
                    Ok(messages) => {
                        for payload in messages {
                            if !handle_payload(&payload, &connection, &handler, &config, &key) {
                                return;
                            }
                        }
                    }
                    Err(error) => {
                        let response = error_response(CommandError::new(error.to_string()), None);
                        let _ = connection.send_value(&response);
                        break;
                    }
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                if connection.pending_async.load(Ordering::SeqCst) == 0
                    && last_activity.elapsed() >= config.idle_timeout
                {
                    break;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    let _ = registry;
}

fn wait_for_async(connection: &Connection, config: &ServerConfig, shutdown: &AtomicBool) {
    let deadline = Instant::now() + config.idle_timeout;
    while connection.pending_async.load(Ordering::SeqCst) > 0
        && Instant::now() < deadline
        && !shutdown.load(Ordering::SeqCst)
    {
        thread::sleep(config.poll_interval.min(Duration::from_millis(10)));
    }
}

fn handle_payload(
    payload: &[u8],
    connection: &Arc<Connection>,
    handler: &Arc<dyn RemoteHandler>,
    config: &ServerConfig,
    key: &ServerKey,
) -> bool {
    let Ok(mut request) = json::parse(payload) else {
        return false;
    };
    if !request.is_object() {
        return false;
    }
    let encrypted = request.get("encrypted").is_some();
    if encrypted {
        if config.encryption == EncryptionMode::Disabled {
            let response = error_response(
                CommandError::new("Encrypted remote control is disabled"),
                None,
            );
            let _ = connection.send_value(&response);
            return false;
        }
        match decrypt_envelope(&request, key, config) {
            Ok(inner) => request = inner,
            Err(_) => return false,
        }
    } else {
        if config.encryption == EncryptionMode::Required {
            let response = error_response(
                CommandError::new("Encrypted remote control is required"),
                None,
            );
            let _ = connection.send_value(&response);
            return true;
        }
    }

    if !request.is_object() || request.get("version").is_none() {
        return false;
    }

    if let Some(expected) = &config.password {
        let supplied = request
            .get("password")
            .and_then(Value::as_str)
            .unwrap_or("");
        if !constant_time_eq(supplied.as_bytes(), expected.as_bytes()) {
            let response = error_response(
                CommandError::new(
                    "The user rejected this password or it is disallowed by remote_control_password in kitty.conf",
                ),
                request.get("id").cloned(),
            );
            let _ = connection.send_value(&response);
            return true;
        }
    }
    request.remove("password");
    request.remove("timestamp");

    let request_id = request.get("id").cloned();
    let conn = connection.clone();
    let notifier = {
        let conn = conn.clone();
        Arc::new(move |event: &str, data: Value| conn.send_notification(event, data))
            as Arc<dyn Fn(&str, Value) -> std::result::Result<(), String> + Send + Sync>
    };
    let ctx = RequestContext::new(connection.peer, request_id, encrypted, Some(notifier));

    let is_screenshot = request
        .get("cmd")
        .and_then(Value::as_str)
        .map(|cmd| cmd.replace('_', "-") == "screenshot")
        .unwrap_or(false);
    if is_screenshot {
        connection.pending_async.fetch_add(1, Ordering::SeqCst);
    }
    let async_connection = connection.clone();
    let async_sender: AsyncResponseFn = Arc::new(move |value| {
        let result = async_connection.send_value(&value);
        async_connection.async_done();
        result
    });

    let outcome = execute(
        handler.as_ref(),
        &ctx,
        &request,
        &config.version,
        Some(async_sender),
    );
    match outcome {
        DispatchOutcome::Response(value) => {
            if is_screenshot {
                connection.async_done();
            }
            if connection.send_value(&value).is_err() {
                return false;
            }
        }
        DispatchOutcome::NoResponse => {
            if is_screenshot {
                connection.async_done();
            }
        }
        DispatchOutcome::Async => {}
    }
    true
}

fn decrypt_envelope(envelope: &Value, key: &ServerKey, config: &ServerConfig) -> Result<Value> {
    if envelope.get("version").is_none() {
        return Err(Error::Protocol(
            "encrypted envelope has no version field".into(),
        ));
    }
    let enc_proto = envelope
        .get("enc_proto")
        .and_then(Value::as_str)
        .unwrap_or(RC_ENCRYPTION_PROTOCOL_VERSION);
    if enc_proto != RC_ENCRYPTION_PROTOCOL_VERSION {
        return Err(Error::Protocol(format!(
            "unsupported encryption protocol {enc_proto}"
        )));
    }
    let pubkey = decode_b85_field(envelope, "pubkey")?;
    if pubkey.is_empty() {
        return Err(Error::Protocol(
            "encrypted envelope has no public key".into(),
        ));
    }
    let iv = decode_b85_field(envelope, "iv")?;
    let tag = decode_b85_field(envelope, "tag")?;
    let ciphertext = decode_b85_field(envelope, "encrypted")?;
    if iv.len() != GCM_IV_LEN {
        return Err(Error::InvalidIvLength(iv.len()));
    }
    if tag.len() != GCM_TAG_LEN {
        return Err(Error::InvalidTagLength(tag.len()));
    }
    let mut iv_array = [0u8; GCM_IV_LEN];
    iv_array.copy_from_slice(&iv);
    let mut tag_array = [0u8; GCM_TAG_LEN];
    tag_array.copy_from_slice(&tag);

    let mut state = key.crypto_state(config.max_message_size)?;
    state.establish(&pubkey)?;
    let sealed = SealedMessage::new(iv_array, tag_array, ciphertext);
    let plaintext = state.open(&sealed)?;
    let inner = json::parse(&plaintext)?;
    if !inner.is_object() || inner.get("version").is_none() {
        return Err(Error::Protocol(
            "decrypted payload is not an object with a version field".into(),
        ));
    }
    let timestamp = inner
        .get("timestamp")
        .and_then(timestamp_ns)
        .ok_or_else(|| Error::Protocol("encrypted payload has no timestamp".into()))?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| Error::Protocol("system clock is before the Unix epoch".into()))?
        .as_nanos() as i128;
    if (now - timestamp).abs() > MAX_TIMESTAMP_SKEW_NS {
        return Err(Error::Protocol(format!(
            "encrypted payload timestamp is {:.1} seconds from now",
            (timestamp - now) as f64 / 1e9
        )));
    }
    Ok(inner)
}

fn decode_b85_field(envelope: &Value, field: &str) -> Result<Vec<u8>> {
    let text = envelope
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| Error::Protocol(format!("encrypted envelope has no {field} field")))?;
    decode_b85(text.as_bytes())
}

fn timestamp_ns(value: &Value) -> Option<i128> {
    match value.as_number()? {
        json::Number::I64(v) => Some(v as i128),
        json::Number::U64(v) => Some(v as i128),
        json::Number::F64(v) if v.is_finite() && v.fract() == 0.0 => Some(v as i128),
        json::Number::F64(_) => None,
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let mut diff = a.len() ^ b.len();
    let len = a.len().max(b.len());
    for index in 0..len {
        let left = a.get(index).copied().unwrap_or(0);
        let right = b.get(index).copied().unwrap_or(0);
        diff |= usize::from(left ^ right);
    }
    diff == 0
}

fn peer_allowed(peer: PeerInfo, policy: PeerCredentialPolicy) -> bool {
    match policy {
        PeerCredentialPolicy::Any => true,
        PeerCredentialPolicy::SameUid => peer.credentials_verified && peer.uid == current_euid(),
    }
}

#[cfg(target_os = "linux")]
fn current_euid() -> u32 {
    extern "C" {
        fn geteuid() -> u32;
    }
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { geteuid() }
}

#[cfg(not(target_os = "linux"))]
fn current_euid() -> u32 {
    0
}

#[cfg(target_os = "linux")]
fn peer_info(stream: &UnixStream) -> PeerInfo {
    use std::os::fd::AsRawFd;
    use std::os::raw::{c_int, c_void};

    #[repr(C)]
    struct Ucred {
        pid: i32,
        uid: u32,
        gid: u32,
    }

    extern "C" {
        fn getsockopt(
            sockfd: c_int,
            level: c_int,
            optname: c_int,
            optval: *mut c_void,
            optlen: *mut u32,
        ) -> c_int;
    }

    const SOL_SOCKET: c_int = 1;
    const SO_PEERCRED: c_int = 17;

    let mut cred = Ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = std::mem::size_of::<Ucred>() as u32;
    // SAFETY: cred and len are valid for the duration of the call and
    // SO_PEERCRED is supported on Linux Unix sockets.
    let result = unsafe {
        getsockopt(
            stream.as_raw_fd(),
            SOL_SOCKET,
            SO_PEERCRED,
            (&mut cred as *mut Ucred).cast::<c_void>(),
            &mut len,
        )
    };
    if result == 0 && len as usize >= std::mem::size_of::<Ucred>() {
        PeerInfo {
            pid: u32::try_from(cred.pid).ok(),
            uid: cred.uid,
            gid: cred.gid,
            credentials_verified: true,
        }
    } else {
        PeerInfo::default()
    }
}

#[cfg(not(target_os = "linux"))]
fn peer_info(_stream: &UnixStream) -> PeerInfo {
    PeerInfo::default()
}
