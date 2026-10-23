// SPDX-License-Identifier: GPL-3.0-or-later
//! Rust-native `kitten notify`.
//!
//! Desktop notifications are sent with kitty's chunked OSC 99 protocol and can
//! fall back to the widely implemented OSC 777 or a plain-text representation.
//! The module also contains the OSC 99 reply state machine used by `--wait`:
//! activation, close, untracked close (poll with `p=alive`) and live replies.

use std::fmt;
use std::fs;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use kitty_remote::base64::encode as encode_base64;

use crate::terminal::InteractiveTransport;

/// Default OSC 99 payload chunk size in base64 characters.
pub const CHUNK_SIZE: usize = 4096;

/// Default overall wait timeout.
pub const DEFAULT_WAIT_TIMEOUT: Duration = Duration::from_secs(30);

/// Usage text for `kitten notify`.
pub const NOTIFY_USAGE: &str = "\
Usage: kitten notify [options] TITLE [BODY...]

Send a desktop notification through kitty's OSC 99 protocol.

Options:
  -i, --identifier ID       Notification identifier (updates an existing one)
  -a, --app-name NAME       Application name (default kitten-notify)
  -n, --icon NAME           Icon name; may be repeated
  -p, --icon-path PATH      PNG/JPEG/GIF icon data
  -u, --urgency U           low, normal or critical
  -e, --expire-after TIME   NUMBER[s|m|h|d] or never
  -s, --sound-name NAME     system, silent or a platform sound name
  -t, --type TYPE           Notification type used by filter rules
  -b, --button TEXT         Add a button; may be repeated
  -P, --print-identifier    Print the identifier before sending
  -w, --wait                Wait for an OSC 99 reply
      --wait-till-closed    Alias for --wait
      --only-print-escape-code
                            Write the escape codes but do not wait
      --fallback MODE       auto, osc777, plain or none
      --plain               Shorthand for --fallback=plain
      --timeout SECS        Wait timeout (default 30)
  -h, --help                Show this help

With an identifier and an empty TITLE the notification is closed.";

/// Notification urgency.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Urgency {
    Low,
    #[default]
    Normal,
    Critical,
}

impl Urgency {
    /// OSC 99 numeric value.
    pub fn code(self) -> u8 {
        match self {
            Self::Low => 0,
            Self::Normal => 1,
            Self::Critical => 2,
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "low" => Some(Self::Low),
            "normal" => Some(Self::Normal),
            "critical" => Some(Self::Critical),
            _ => None,
        }
    }
}

/// How to encode the notification.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum NotificationFallback {
    /// OSC 99 when supported, otherwise OSC 777. This implementation emits
    /// OSC 99 unless the caller explicitly asks for a fallback.
    #[default]
    Auto,
    Osc777,
    Plain,
    None,
}

/// A desktop notification.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Notification {
    pub title: String,
    pub body: String,
    pub identifier: Option<String>,
    pub app_name: String,
    pub icon_names: Vec<String>,
    pub icon_path: Option<PathBuf>,
    pub icon_cache_id: Option<String>,
    pub icon_data: Option<Vec<u8>>,
    pub urgency: Urgency,
    pub expire_after: Option<Duration>,
    /// `None` means the platform policy (`-1`).
    pub sound: Option<String>,
    pub notification_type: Option<String>,
    pub buttons: Vec<String>,
}

impl Notification {
    fn metadata(&self, wait: bool, done: bool) -> Vec<String> {
        let mut items = Vec::new();
        if !self.app_name.is_empty() {
            items.push(format!("f={}", b64(&self.app_name)));
        }
        match self.urgency {
            Urgency::Low => items.push("u=0".to_owned()),
            Urgency::Normal => {}
            Urgency::Critical => items.push("u=2".to_owned()),
        }
        if let Some(expire) = self.expire_after {
            items.push(format!("w={}", expire.as_millis()));
        }
        if let Some(kind) = &self.notification_type {
            items.push(format!("t={}", b64(kind)));
        }
        if wait {
            items.push("c=1:a=report".to_owned());
        }
        for icon in &self.icon_names {
            items.push(format!("n={}", b64(icon)));
        }
        if let Some(cache_id) = &self.icon_cache_id {
            items.push(format!("g={cache_id}"));
        }
        if let Some(sound) = &self.sound {
            if sound != "system" {
                items.push(format!("s={}", b64(sound)));
            }
        }
        if done {
            items.push("d=1".to_owned());
        } else {
            items.push("d=0".to_owned());
        }
        items
    }
}

fn b64(text: &str) -> String {
    String::from_utf8(encode_base64(text.as_bytes(), true)).expect("base64 is ASCII")
}

fn sanitize_plain(text: &str) -> String {
    text.chars()
        .filter(|ch| !ch.is_control() || *ch == '\n' || *ch == '\t')
        .collect()
}

/// Encode the complete OSC 99 notification as chunked escape sequences.
///
/// The first frame carries metadata and no payload; title/body/icon/button
/// payloads follow, base64 encoded and split into `chunk_size` characters.
/// A final empty frame has `d=1`, telling the terminal the notification is
/// complete.
pub fn encode_osc99(
    notification: &Notification,
    identifier: &str,
    wait: bool,
    chunk_size: usize,
) -> Vec<Vec<u8>> {
    let prefix = format!("\x1b]99;i={identifier}");
    let mut frames = Vec::new();
    if notification.title.is_empty() {
        frames.push(format!("{prefix}:p=close;\x1b\\").into_bytes());
        return frames;
    }
    let metadata = notification.metadata(wait, false);
    let meta = if metadata.is_empty() {
        String::new()
    } else {
        format!(":{}", metadata.join(":"))
    };
    frames.push(format!("{prefix}:d=0{meta};\x1b\\").into_bytes());
    let chunk_size = chunk_size.max(4);
    let mut add_payload = |payload_type: &str, payload: &[u8]| {
        if payload.is_empty() {
            return;
        }
        let encoded = encode_base64(payload, true);
        let encoded = String::from_utf8(encoded).expect("base64 is ASCII");
        let p = if payload_type == "title" {
            String::new()
        } else {
            format!(":p={payload_type}")
        };
        let mut index = 0usize;
        while index < encoded.len() {
            let end = (index + chunk_size).min(encoded.len());
            frames.push(format!("{prefix}:d=0:e=1{p};{}\x1b\\", &encoded[index..end]).into_bytes());
            index = end;
        }
    };
    add_payload("title", notification.title.as_bytes());
    add_payload("body", notification.body.as_bytes());
    if let Some(data) = &notification.icon_data {
        add_payload("icon", data);
    }
    if !notification.buttons.is_empty() {
        add_payload("buttons", notification.buttons.join("\u{2028}").as_bytes());
    }
    frames.push(format!("{prefix}:d=1;\x1b\\").into_bytes());
    frames
}

/// Concatenate [`encode_osc99`] frames.
pub fn encode_osc99_bytes(
    notification: &Notification,
    identifier: &str,
    wait: bool,
    chunk_size: usize,
) -> Vec<u8> {
    encode_osc99(notification, identifier, wait, chunk_size)
        .into_iter()
        .flatten()
        .collect()
}

/// Encode the OSC 777 fallback (`notify;TITLE;BODY`).
pub fn encode_osc777(notification: &Notification) -> Vec<u8> {
    format!(
        "\x1b]777;notify;{};{}\x1b\\",
        sanitize_plain(&notification.title),
        sanitize_plain(&notification.body)
    )
    .into_bytes()
}

/// Encode a plain-text fallback.
pub fn encode_plain(notification: &Notification) -> String {
    if notification.body.is_empty() {
        sanitize_plain(&notification.title)
    } else {
        format!(
            "{}\n{}",
            sanitize_plain(&notification.title),
            sanitize_plain(&notification.body)
        )
    }
}

/// Encode a notification with the selected fallback policy.
pub fn encode_notification(
    notification: &Notification,
    identifier: &str,
    wait: bool,
    chunk_size: usize,
    fallback: NotificationFallback,
) -> Vec<u8> {
    match fallback {
        NotificationFallback::Osc777 => encode_osc777(notification),
        NotificationFallback::Plain => {
            let text = encode_plain(notification);
            if text.is_empty() {
                Vec::new()
            } else {
                format!("{text}\n").into_bytes()
            }
        }
        NotificationFallback::None | NotificationFallback::Auto => {
            encode_osc99_bytes(notification, identifier, wait, chunk_size)
        }
    }
}

/// Parsed notification command.
#[derive(Clone, Debug)]
pub struct NotifyOptions {
    pub notification: Notification,
    pub wait: bool,
    pub only_print_escape_code: bool,
    pub print_identifier: bool,
    pub fallback: NotificationFallback,
    pub write_timeout: Duration,
    pub chunk_size: usize,
    pub help: bool,
}

impl Default for NotifyOptions {
    fn default() -> Self {
        Self {
            notification: Notification {
                app_name: "kitten-notify".to_owned(),
                ..Notification::default()
            },
            wait: false,
            only_print_escape_code: false,
            print_identifier: false,
            fallback: NotificationFallback::Auto,
            write_timeout: DEFAULT_WAIT_TIMEOUT,
            chunk_size: CHUNK_SIZE,
            help: false,
        }
    }
}

/// Errors from the notify kitten.
#[derive(Debug)]
#[non_exhaustive]
pub enum NotifyError {
    Io(io::Error),
    InvalidOption { option: &'static str, value: String },
    MissingTitle,
    InvalidIdentifier(String),
    InvalidTimeout(String),
    InvalidTime(String),
    InvalidIcon(String),
    InvalidReply(String),
    Timeout(Duration),
}

impl fmt::Display for NotifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "I/O error: {err}"),
            Self::InvalidOption { option, value } => {
                write!(f, "invalid value for {option}: {value:?}")
            }
            Self::MissingTitle => f.write_str(
                "Must specify a non-empty TITLE for the notification or specify an identifier to close a notification.",
            ),
            Self::InvalidIdentifier(value) => write!(
                f,
                "Invalid identifier: {value} must be only English letters, numbers, hyphens, underscores, dots and plus signs."
            ),
            Self::InvalidTimeout(value) => write!(f, "invalid timeout: {value:?}"),
            Self::InvalidTime(value) => write!(f, "invalid expire time: {value:?}"),
            Self::InvalidIcon(message) => write!(f, "invalid icon: {message}"),
            Self::InvalidReply(message) => write!(f, "invalid OSC 99 reply: {message}"),
            Self::Timeout(duration) => write!(
                f,
                "timed out after {:.3}s waiting for an OSC 99 reply",
                duration.as_secs_f64()
            ),
        }
    }
}

impl std::error::Error for NotifyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<io::Error> for NotifyError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

/// Whether an OSC 99 identifier contains only safe characters.
pub fn is_valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 512
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'+' | b'.' | b'-'))
}

/// Generate a short random-ish identifier without an RNG dependency.
pub fn random_identifier() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut value = nanos ^ (u64::from(std::process::id()) << 32) ^ counter;
    const ALPHABET: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut out = String::from("kitten-");
    for _ in 0..12 {
        out.push(ALPHABET[(value % ALPHABET.len() as u64) as usize] as char);
        value /= ALPHABET.len() as u64;
    }
    out
}

fn parse_duration(raw: &str) -> Result<Option<Duration>, NotifyError> {
    if raw.is_empty() {
        return Ok(None);
    }
    if raw == "never" {
        return Ok(Some(Duration::ZERO));
    }
    let (number, multiplier) = match raw.as_bytes().last().copied() {
        Some(b's') => (&raw[..raw.len() - 1], 1.0),
        Some(b'm') => (&raw[..raw.len() - 1], 60.0),
        Some(b'h') => (&raw[..raw.len() - 1], 3600.0),
        Some(b'd') => (&raw[..raw.len() - 1], 86400.0),
        _ => (raw, 1.0),
    };
    let value: f64 = number
        .parse()
        .map_err(|_| NotifyError::InvalidTime(raw.to_owned()))?;
    if !value.is_finite() || value < 0.0 {
        return Err(NotifyError::InvalidTime(raw.to_owned()));
    }
    Ok(Some(Duration::from_secs_f64(value * multiplier)))
}

/// Parse arguments after `kitten notify`.
pub fn parse_notify_args(args: &[String]) -> Result<NotifyOptions, NotifyError> {
    let mut opts = NotifyOptions::default();
    let mut positionals = Vec::new();
    let mut index = 0usize;
    while index < args.len() {
        let arg = &args[index];
        if arg == "--" {
            positionals.extend_from_slice(&args[index + 1..]);
            break;
        }
        if !arg.starts_with('-') || arg == "-" {
            positionals.push(arg.clone());
            index += 1;
            continue;
        }
        let (name, inline) = match arg.split_once('=') {
            Some((name, value)) => (name, Some(value.to_owned())),
            None => (arg.as_str(), None),
        };
        let mut take = |option: &'static str| -> Result<String, NotifyError> {
            if let Some(value) = &inline {
                return Ok(value.clone());
            }
            index += 1;
            args.get(index)
                .cloned()
                .ok_or_else(|| NotifyError::InvalidOption {
                    option,
                    value: String::new(),
                })
        };
        match name {
            "-h" | "--help" => opts.help = true,
            "-i" | "--identifier" => opts.notification.identifier = Some(take("--identifier")?),
            "-a" | "--app-name" => opts.notification.app_name = take("--app-name")?,
            "-n" | "--icon" => opts.notification.icon_names.push(take("--icon")?),
            "-p" | "--icon-path" => {
                opts.notification.icon_path = Some(PathBuf::from(take("--icon-path")?))
            }
            "-g" | "--icon-cache-id" => {
                opts.notification.icon_cache_id = Some(take("--icon-cache-id")?)
            }
            "-u" | "--urgency" => {
                let value = take("--urgency")?;
                opts.notification.urgency =
                    Urgency::parse(&value).ok_or(NotifyError::InvalidOption {
                        option: "--urgency",
                        value,
                    })?;
            }
            "-e" | "--expire-after" => {
                opts.notification.expire_after = parse_duration(&take("--expire-after")?)?
            }
            "-s" | "--sound-name" => opts.notification.sound = Some(take("--sound-name")?),
            "-t" | "--type" => opts.notification.notification_type = Some(take("--type")?),
            "-b" | "--button" => opts.notification.buttons.push(take("--button")?),
            "-P" | "--print-identifier" => opts.print_identifier = true,
            "-w" | "--wait" | "--wait-till-closed" | "--wait-for-completion" => opts.wait = true,
            "--only-print-escape-code" => opts.only_print_escape_code = true,
            "--fallback" => {
                let value = take("--fallback")?;
                opts.fallback = match value.as_str() {
                    "auto" => NotificationFallback::Auto,
                    "osc777" | "777" => NotificationFallback::Osc777,
                    "plain" | "text" => NotificationFallback::Plain,
                    "none" | "off" => NotificationFallback::None,
                    _ => {
                        return Err(NotifyError::InvalidOption {
                            option: "--fallback",
                            value,
                        })
                    }
                };
            }
            "--plain" | "--plain-text" => opts.fallback = NotificationFallback::Plain,
            "--timeout" => {
                let value = take("--timeout")?;
                let seconds: f64 = value
                    .parse()
                    .map_err(|_| NotifyError::InvalidTimeout(value.clone()))?;
                if !seconds.is_finite() || seconds < 0.0 {
                    return Err(NotifyError::InvalidTimeout(value));
                }
                opts.write_timeout = Duration::from_secs_f64(seconds);
            }
            other => {
                return Err(NotifyError::InvalidOption {
                    option: "argument",
                    value: other.to_owned(),
                })
            }
        }
        index += 1;
    }
    if let Some(title) = positionals.first() {
        opts.notification.title = title.clone();
    }
    if positionals.len() > 1 {
        opts.notification.body = positionals[1..].join(" ");
    }
    if let Some(cache_id) = &opts.notification.icon_cache_id {
        if !is_valid_identifier(cache_id) {
            return Err(NotifyError::InvalidIdentifier(cache_id.clone()));
        }
    }
    if let Some(identifier) = &opts.notification.identifier {
        if !is_valid_identifier(identifier) {
            return Err(NotifyError::InvalidIdentifier(identifier.clone()));
        }
    }
    if opts.help {
        return Ok(opts);
    }
    if opts.notification.identifier.is_none() {
        if opts.notification.title.is_empty() {
            return Err(NotifyError::MissingTitle);
        }
        opts.notification.identifier = Some(random_identifier());
    }
    if let Some(path) = &opts.notification.icon_path {
        let data = fs::read(path).map_err(|err| {
            NotifyError::InvalidIcon(format!("cannot read {}: {err}", path.display()))
        })?;
        if !looks_like_image(&data) {
            return Err(NotifyError::InvalidIcon(
                "the icon must be a PNG, JPEG or GIF file".to_owned(),
            ));
        }
        opts.notification.icon_data = Some(data);
    }
    Ok(opts)
}

fn looks_like_image(data: &[u8]) -> bool {
    data.starts_with(b"\x89PNG\r\n\x1a\n")
        || data.starts_with(b"\xff\xd8\xff")
        || data.starts_with(b"GIF87a")
        || data.starts_with(b"GIF89a")
}

/// A parsed OSC 99 reply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotificationReply {
    pub identifier: String,
    pub payload_type: Option<String>,
    pub payload: String,
}

/// Parse the first OSC 99 reply in `data`.
pub fn parse_notify_reply(data: &[u8]) -> Result<Option<NotificationReply>, NotifyError> {
    let Some(start) = find_osc99(data) else {
        return Ok(None);
    };
    let rest = &data[start + 3..];
    let mut end = None;
    for (index, byte) in rest.iter().enumerate() {
        if *byte == 0x07 {
            end = Some(index);
            break;
        }
        if *byte == 0x1b && rest.get(index + 1) == Some(&b'\\') {
            end = Some(index);
            break;
        }
    }
    let Some(end) = end else {
        return Ok(None);
    };
    let text = std::str::from_utf8(&rest[..end])
        .map_err(|_| NotifyError::InvalidReply("reply is not UTF-8".to_owned()))?;
    let (metadata, payload) = text
        .split_once(';')
        .ok_or_else(|| NotifyError::InvalidReply("missing separator".to_owned()))?;
    let mut identifier = String::new();
    let mut payload_type = None;
    for part in metadata.split(':') {
        if part.is_empty() {
            continue;
        }
        let (key, value) = part
            .split_once('=')
            .ok_or_else(|| NotifyError::InvalidReply(format!("bad metadata {part:?}")))?;
        match key {
            "i" => identifier = value.to_owned(),
            "p" => payload_type = Some(value.to_owned()),
            _ => {}
        }
    }
    Ok(Some(NotificationReply {
        identifier,
        payload_type,
        payload: payload.to_owned(),
    }))
}

fn find_osc99(data: &[u8]) -> Option<usize> {
    for index in 0..data.len() {
        if data[index..].starts_with(b"99;")
            && (index == 0 || data[index - 1] == b']' || data[index - 1] == b';')
        {
            return Some(index);
        }
    }
    None
}

/// What the waiter should do after a reply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplyAction {
    /// Keep waiting for another reply.
    Wait,
    /// Send an OSC 99 `p=alive` query.
    Poll,
    /// The notification is gone.
    Done,
    /// The user activated the notification; the value is the button number
    /// (0 for the notification itself).
    Activated(u32),
    /// A reply for a different identifier or an unknown payload type.
    Ignore,
}

/// OSC 99 wait state machine.
#[derive(Clone, Debug)]
pub struct ReplyTracker {
    identifier: String,
    closed: bool,
    activated: Option<u32>,
}

impl ReplyTracker {
    pub fn new(identifier: impl Into<String>) -> Self {
        Self {
            identifier: identifier.into(),
            closed: false,
            activated: None,
        }
    }

    pub fn identifier(&self) -> &str {
        &self.identifier
    }

    pub fn activated(&self) -> Option<u32> {
        self.activated
    }

    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// Consume a reply.
    pub fn handle(&mut self, reply: &NotificationReply) -> Result<ReplyAction, NotifyError> {
        if reply.identifier != self.identifier {
            return Ok(ReplyAction::Ignore);
        }
        match reply.payload_type.as_deref() {
            Some("close") => {
                if reply.payload == "untracked" {
                    Ok(ReplyAction::Poll)
                } else {
                    self.closed = true;
                    Ok(ReplyAction::Done)
                }
            }
            Some("alive") => {
                let alive = reply.payload.split(',').any(|item| item == self.identifier);
                if alive {
                    Ok(ReplyAction::Wait)
                } else {
                    self.closed = true;
                    Ok(ReplyAction::Done)
                }
            }
            Some("?") => Ok(ReplyAction::Ignore),
            Some(_) => Ok(ReplyAction::Ignore),
            None => {
                let button = if reply.payload.trim().is_empty() {
                    0
                } else {
                    reply.payload.trim().parse().map_err(|_| {
                        NotifyError::InvalidReply(format!(
                            "invalid activation payload {:?}",
                            reply.payload
                        ))
                    })?
                };
                self.activated = Some(button);
                Ok(ReplyAction::Activated(button))
            }
        }
    }
}

/// Result of running a notification command.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NotifyOutcome {
    pub identifier: String,
    pub activation: Option<u32>,
    pub waited: bool,
}

/// Write a notification and optionally wait for replies.
pub fn run_notify(
    opts: &NotifyOptions,
    transport: &mut dyn InteractiveTransport,
    stdout: &mut dyn Write,
) -> Result<NotifyOutcome, NotifyError> {
    let identifier = opts
        .notification
        .identifier
        .clone()
        .unwrap_or_else(random_identifier);
    if !is_valid_identifier(&identifier) {
        return Err(NotifyError::InvalidIdentifier(identifier));
    }
    if opts.print_identifier {
        writeln!(stdout, "{identifier}")?;
        stdout.flush()?;
    }
    if opts.fallback == NotificationFallback::Plain {
        let text = encode_plain(&opts.notification);
        if !text.is_empty() {
            writeln!(stdout, "{text}")?;
            stdout.flush()?;
        }
        return Ok(NotifyOutcome {
            identifier,
            ..NotifyOutcome::default()
        });
    }
    if opts.fallback == NotificationFallback::Osc777 {
        transport.write_all(&encode_osc777(&opts.notification))?;
        transport.flush()?;
        return Ok(NotifyOutcome {
            identifier,
            ..NotifyOutcome::default()
        });
    }
    let frames = encode_osc99(&opts.notification, &identifier, opts.wait, opts.chunk_size);
    for frame in &frames {
        transport.write_all(frame)?;
    }
    transport.flush()?;
    if !opts.wait || opts.only_print_escape_code {
        return Ok(NotifyOutcome {
            identifier,
            ..NotifyOutcome::default()
        });
    }

    let mut tracker = ReplyTracker::new(identifier.clone());
    let deadline = Instant::now() + opts.write_timeout;
    let mut poll_pending = opts.notification.title.is_empty();
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        if poll_pending {
            transport.write_all(format!("\x1b]99;i={identifier}:p=alive;\x1b\\").as_bytes())?;
            transport.flush()?;
            poll_pending = false;
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(NotifyError::Timeout(opts.write_timeout));
        }
        match transport.read(&mut chunk, deadline - now) {
            Ok(0) => return Err(NotifyError::InvalidReply("terminal closed".to_owned())),
            Ok(n) => {
                buffer.extend_from_slice(&chunk[..n]);
                while let Some(reply) = parse_notify_reply(&buffer)? {
                    let consumed = find_osc99(&buffer).unwrap_or(0) + 3;
                    let rest = &buffer[consumed..];
                    let end = rest
                        .iter()
                        .position(|b| *b == 0x07)
                        .map(|x| x + 1)
                        .or_else(|| rest.windows(2).position(|w| w == b"\x1b\\").map(|x| x + 2))
                        .unwrap_or(rest.len());
                    buffer.drain(..consumed + end);
                    match tracker.handle(&reply)? {
                        ReplyAction::Done => {
                            return Ok(NotifyOutcome {
                                identifier,
                                activation: tracker.activated(),
                                waited: true,
                            })
                        }
                        ReplyAction::Activated(_) => {
                            return Ok(NotifyOutcome {
                                identifier,
                                activation: tracker.activated(),
                                waited: true,
                            })
                        }
                        ReplyAction::Poll => poll_pending = true,
                        ReplyAction::Wait | ReplyAction::Ignore => {}
                    }
                }
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) if err.kind() == io::ErrorKind::TimedOut => {
                return Err(NotifyError::Timeout(opts.write_timeout))
            }
            Err(err) => return Err(NotifyError::Io(err)),
        }
    }
}

/// Parse and run `kitten notify` arguments.
pub fn run_notify_cli(
    args: &[String],
    transport: &mut dyn InteractiveTransport,
    stdout: &mut dyn Write,
    stdin: &mut dyn Read,
) -> Result<NotifyOutcome, NotifyError> {
    let _ = stdin;
    let opts = parse_notify_args(args)?;
    if opts.help {
        stdout.write_all(NOTIFY_USAGE.as_bytes())?;
        stdout.write_all(b"\n")?;
        return Ok(NotifyOutcome::default());
    }
    run_notify(&opts, transport, stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::ScriptedTransport;

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|x| (*x).to_owned()).collect()
    }

    #[test]
    fn parses_title_body_and_metadata_options() {
        let opts = parse_notify_args(&args(&[
            "-i",
            "test",
            "--app-name=app",
            "--urgency",
            "critical",
            "--expire-after",
            "1.5s",
            "--icon",
            "one",
            "--icon",
            "two",
            "--button",
            "OK",
            "--type",
            "build",
            "Title",
            "Body",
            "here",
        ]))
        .unwrap();
        assert_eq!(opts.notification.title, "Title");
        assert_eq!(opts.notification.body, "Body here");
        assert_eq!(opts.notification.urgency, Urgency::Critical);
        assert_eq!(
            opts.notification.expire_after,
            Some(Duration::from_millis(1500))
        );
        assert_eq!(opts.notification.icon_names, ["one", "two"]);
        assert_eq!(opts.notification.buttons, ["OK"]);
        assert_eq!(
            opts.notification.notification_type.as_deref(),
            Some("build")
        );
    }

    #[test]
    fn rejects_missing_title_and_bad_identifier() {
        assert!(matches!(
            parse_notify_args(&[]),
            Err(NotifyError::MissingTitle)
        ));
        assert!(matches!(
            parse_notify_args(&args(&["--identifier=invalid:id"])),
            Err(NotifyError::InvalidIdentifier(_))
        ));
        assert!(matches!(
            parse_notify_args(&args(&["--urgency=urgent", "x"])),
            Err(NotifyError::InvalidOption { .. })
        ));
        assert!(matches!(
            parse_notify_args(&args(&["--expire-after=nope", "x"])),
            Err(NotifyError::InvalidTime(_))
        ));
    }

    #[test]
    fn close_escape_code_matches_kitty() {
        let notification = Notification {
            identifier: Some("test".to_owned()),
            ..Notification::default()
        };
        let frames = encode_osc99(&notification, "test", true, CHUNK_SIZE);
        assert_eq!(frames, vec![b"\x1b]99;i=test:p=close;\x1b\\".to_vec()]);
    }

    #[test]
    fn osc99_chunks_title_and_body() {
        let n = Notification {
            title: "A title".to_owned(),
            body: "A longer body".to_owned(),
            identifier: Some("id".to_owned()),
            app_name: "app".to_owned(),
            ..Notification::default()
        };
        let frames = encode_osc99(&n, "id", true, 8);
        assert_eq!(
            frames[0],
            b"\x1b]99;i=id:d=0:f=YXBw:c=1:a=report:d=0;\x1b\\"
        );
        assert!(frames
            .iter()
            .any(|f| f.starts_with(b"\x1b]99;i=id:d=0:e=1;QSB0aXRs")));
        assert!(frames
            .iter()
            .any(|f| f.starts_with(b"\x1b]99;i=id:d=0:e=1:p=body;")));
        assert!(frames.last().unwrap().starts_with(b"\x1b]99;i=id:d=1;"));
        assert!(frames.len() > 4);
    }

    #[test]
    fn urgency_and_sound_and_type_metadata() {
        let n = Notification {
            title: "T".to_owned(),
            app_name: String::new(),
            urgency: Urgency::Low,
            sound: Some("silent".to_owned()),
            notification_type: Some("build".to_owned()),
            ..Notification::default()
        };
        let bytes = encode_osc99_bytes(&n, "id", false, 100);
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains(":u=0:"));
        assert!(text.contains(":t=YnVpbGQ="));
        assert!(text.contains(":s=c2lsZW50"));
    }

    #[test]
    fn osc777_and_plain_fallbacks() {
        let n = Notification {
            title: "Hi".to_owned(),
            body: "There".to_owned(),
            ..Notification::default()
        };
        assert_eq!(encode_osc777(&n), b"\x1b]777;notify;Hi;There\x1b\\");
        assert_eq!(encode_plain(&n), "Hi\nThere");
        assert_eq!(
            encode_notification(&n, "id", false, 10, NotificationFallback::Osc777),
            encode_osc777(&n)
        );
        assert_eq!(
            encode_notification(&n, "id", false, 10, NotificationFallback::Plain),
            b"Hi\nThere\n"
        );
    }

    #[test]
    fn only_print_escape_code_writes_without_waiting() {
        let mut transport = ScriptedTransport::new();
        let mut out = Vec::new();
        let opts = NotifyOptions {
            wait: true,
            only_print_escape_code: true,
            notification: Notification {
                identifier: Some("test".to_owned()),
                ..Notification::default()
            },
            ..NotifyOptions::default()
        };
        let outcome = run_notify(&opts, &mut transport, &mut out).unwrap();
        assert_eq!(outcome.identifier, "test");
        assert_eq!(transport.written(), b"\x1b]99;i=test:p=close;\x1b\\");
    }

    #[test]
    fn reply_tracker_handles_activation_close_and_alive() {
        let mut tracker = ReplyTracker::new("x");
        let activation = NotificationReply {
            identifier: "x".to_owned(),
            payload_type: None,
            payload: "2".to_owned(),
        };
        assert_eq!(
            tracker.handle(&activation).unwrap(),
            ReplyAction::Activated(2)
        );
        assert_eq!(tracker.activated(), Some(2));

        let close = NotificationReply {
            identifier: "x".to_owned(),
            payload_type: Some("close".to_owned()),
            payload: String::new(),
        };
        assert_eq!(tracker.handle(&close).unwrap(), ReplyAction::Done);
        assert!(tracker.is_closed());

        let mut tracker = ReplyTracker::new("x");
        let untracked = NotificationReply {
            identifier: "x".to_owned(),
            payload_type: Some("close".to_owned()),
            payload: "untracked".to_owned(),
        };
        assert_eq!(tracker.handle(&untracked).unwrap(), ReplyAction::Poll);
        let alive = NotificationReply {
            identifier: "x".to_owned(),
            payload_type: Some("alive".to_owned()),
            payload: "a,x,b".to_owned(),
        };
        assert_eq!(tracker.handle(&alive).unwrap(), ReplyAction::Wait);
        let gone = NotificationReply {
            identifier: "x".to_owned(),
            payload_type: Some("alive".to_owned()),
            payload: "a,b".to_owned(),
        };
        assert_eq!(tracker.handle(&gone).unwrap(), ReplyAction::Done);
        let other = NotificationReply {
            identifier: "y".to_owned(),
            payload_type: None,
            payload: String::new(),
        };
        assert_eq!(tracker.handle(&other).unwrap(), ReplyAction::Ignore);
    }

    #[test]
    fn wait_state_machine_returns_activation() {
        let mut transport = ScriptedTransport::new();
        transport.push_reply(b"\x1b]99;i=id;1\x1b\\".to_vec());
        let opts = NotifyOptions {
            wait: true,
            write_timeout: Duration::from_millis(50),
            notification: Notification {
                title: "T".to_owned(),
                identifier: Some("id".to_owned()),
                ..Notification::default()
            },
            ..NotifyOptions::default()
        };
        let outcome = run_notify(&opts, &mut transport, &mut Vec::new()).unwrap();
        assert!(outcome.waited);
        assert_eq!(outcome.activation, Some(1));
    }

    #[test]
    fn wait_timeout_is_reported_without_hanging() {
        let mut transport = ScriptedTransport::new();
        transport.set_timeout(true);
        let opts = NotifyOptions {
            wait: true,
            write_timeout: Duration::from_millis(1),
            notification: Notification {
                title: "T".to_owned(),
                identifier: Some("id".to_owned()),
                ..Notification::default()
            },
            ..NotifyOptions::default()
        };
        let err = run_notify(&opts, &mut transport, &mut Vec::new()).unwrap_err();
        assert!(matches!(err, NotifyError::Timeout(_)));
    }

    #[test]
    fn close_wait_polls_untracked_then_done() {
        let mut transport = ScriptedTransport::new();
        transport.push_reply(b"\x1b]99;i=id:p=close;untracked\x1b\\".to_vec());
        transport.push_reply(b"\x1b]99;i=id:p=alive;\x1b\\".to_vec());
        let opts = NotifyOptions {
            wait: true,
            write_timeout: Duration::from_millis(50),
            notification: Notification {
                identifier: Some("id".to_owned()),
                ..Notification::default()
            },
            ..NotifyOptions::default()
        };
        let outcome = run_notify(&opts, &mut transport, &mut Vec::new()).unwrap();
        assert!(outcome.waited);
        let written = transport.written();
        assert!(written.starts_with(b"\x1b]99;i=id:p=close;\x1b\\"));
        assert!(written.ends_with(b"\x1b]99;i=id:p=alive;\x1b\\"));
    }

    #[test]
    fn parses_and_rejects_replies() {
        let reply = parse_notify_reply(b"\x1b]99;i=x:p=alive;a,x\x1b\\")
            .unwrap()
            .unwrap();
        assert_eq!(reply.identifier, "x");
        assert_eq!(reply.payload_type.as_deref(), Some("alive"));
        assert_eq!(reply.payload, "a,x");
        assert!(parse_notify_reply(b"\x1b]99;i=x:p=alive;")
            .unwrap()
            .is_none());
    }
}
