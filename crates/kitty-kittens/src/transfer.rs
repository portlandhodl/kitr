// SPDX-License-Identifier: GPL-3.0-or-later
//! File transfer over the TTY, ported from `kittens/transfer` and kitty's
//! `file_transmission.py`.
//!
//! The wire format is OSC 5113 carrying `FileTransmissionCommand` records.
//! Enum fields are serialized by name (for example `ac=file`), strings marked
//! base64 use unpadded standard base64, as in kitty. This module implements
//! both directions and the protocol state machines; the rsync delta option is
//! accepted but transfers use the simple byte-stream transport.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use kitty_remote::base64::{decode as b64decode, encode as b64encode};
use kitty_remote::sha256;

use crate::rsync;
use crate::terminal::InteractiveTransport;

pub const FILE_TRANSFER_CODE: &str = "5113";
pub const CHUNK_SIZE: usize = 4096;
pub const DATA_CHUNK_SIZE: usize = 1024 * 1024;

pub const TRANSFER_USAGE: &str = "\
Usage: kitten transfer [options] source_files_or_directories destination_path

Transfer files over the TTY device, including over SSH.

Options:
  -d, --direction download|send|upload|receive  Direction (default download)
  -m, --mode normal|mirror                      Argument interpretation
      --compress auto|never|always              Compression policy
  -p, --permissions-bypass PASSWORD             Skip the confirmation popup
  -c, --confirm-paths                           Show and confirm the mapping
  -x, --transmit-deltas                         Request rsync-style deltas
  -h, --help                                    Show this help";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Action {
    #[default]
    Invalid,
    Send,
    File,
    Data,
    EndData,
    Receive,
    Cancel,
    Status,
    Finish,
}

impl Action {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Invalid => "invalid",
            Self::Send => "send",
            Self::File => "file",
            Self::Data => "data",
            Self::EndData => "end_data",
            Self::Receive => "receive",
            Self::Cancel => "cancel",
            Self::Status => "status",
            Self::Finish => "finish",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "invalid" => Self::Invalid,
            "send" => Self::Send,
            "file" => Self::File,
            "data" => Self::Data,
            "end_data" => Self::EndData,
            "receive" => Self::Receive,
            "cancel" => Self::Cancel,
            "status" => Self::Status,
            "finish" => Self::Finish,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Compression {
    #[default]
    None,
    Zlib,
}

impl Compression {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Zlib => "zlib",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FileType {
    #[default]
    Regular,
    Directory,
    Symlink,
    Link,
}

impl FileType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Regular => "regular",
            Self::Directory => "directory",
            Self::Symlink => "symlink",
            Self::Link => "link",
        }
    }

    pub fn short(self) -> &'static str {
        match self {
            Self::Regular => "fil",
            Self::Directory => "dir",
            Self::Symlink => "sym",
            Self::Link => "lnk",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TransmissionType {
    #[default]
    Simple,
    Rsync,
}

impl TransmissionType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Simple => "simple",
            Self::Rsync => "rsync",
        }
    }
}

/// One OSC 5113 protocol record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileTransmissionCommand {
    pub action: Action,
    pub compression: Compression,
    pub ftype: FileType,
    pub ttype: TransmissionType,
    pub quiet: u8,
    pub mtime: i64,
    pub permissions: u32,
    pub size: i64,
    pub id: String,
    pub file_id: String,
    pub bypass: String,
    pub name: String,
    pub status: String,
    pub parent: String,
    pub data: Vec<u8>,
}

impl Default for FileTransmissionCommand {
    fn default() -> Self {
        Self {
            action: Action::Invalid,
            compression: Compression::None,
            ftype: FileType::Regular,
            ttype: TransmissionType::Simple,
            quiet: 0,
            mtime: -1,
            permissions: u32::MAX,
            size: -1,
            id: String::new(),
            file_id: String::new(),
            bypass: String::new(),
            name: String::new(),
            status: String::new(),
            parent: String::new(),
            data: Vec::new(),
        }
    }
}

impl FileTransmissionCommand {
    pub fn file_name(&self) -> &str {
        &self.name
    }

    fn field(&self, key: &str) -> Option<String> {
        let text = match key {
            "ac" => self.action.as_str().to_owned(),
            "zip" => self.compression.as_str().to_owned(),
            "ft" => self.ftype.as_str().to_owned(),
            "tt" => self.ttype.as_str().to_owned(),
            "q" => self.quiet.to_string(),
            "id" => self.id.clone(),
            "fid" => self.file_id.clone(),
            "pw" => b64(self.bypass.as_bytes()),
            "n" => b64(self.name.as_bytes()),
            "st" => b64(self.status.as_bytes()),
            "pr" => self.parent.clone(),
            "mod" => self.mtime.to_string(),
            "prm" => self.permissions.to_string(),
            "sz" => self.size.to_string(),
            "d" => b64(&self.data),
            _ => return None,
        };
        let is_default = match key {
            "ac" => self.action == Action::Invalid,
            "zip" => self.compression == Compression::None,
            "ft" => self.ftype == FileType::Regular,
            "tt" => self.ttype == TransmissionType::Simple,
            "q" => self.quiet == 0,
            "id" | "fid" | "pw" | "n" | "st" | "pr" => text.is_empty(),
            "mod" => self.mtime == -1,
            "prm" => self.permissions == u32::MAX,
            "sz" => self.size == -1,
            "d" => self.data.is_empty(),
            _ => true,
        };
        if is_default {
            None
        } else {
            Some(text)
        }
    }

    /// Serialize without the OSC prefix.
    pub fn serialize(&self) -> String {
        let mut out = String::new();
        for key in [
            "ac", "zip", "ft", "tt", "q", "id", "fid", "pw", "n", "st", "pr", "mod", "prm", "sz",
            "d",
        ] {
            if let Some(value) = self.field(key) {
                if !out.is_empty() {
                    out.push(';');
                }
                out.push_str(key);
                out.push('=');
                out.push_str(&value);
            }
        }
        out
    }

    /// Parse a serialized record. Unknown fields are ignored, matching kitty.
    pub fn deserialize(data: &[u8]) -> Result<Self, TransferError> {
        let text = std::str::from_utf8(data)
            .map_err(|_| TransferError::Protocol("not UTF-8".to_owned()))?;
        let mut ans = Self::default();
        for item in text.split(';') {
            let Some((key, value)) = item.split_once('=') else {
                continue;
            };
            match key {
                "ac" => {
                    ans.action = Action::parse(value)
                        .ok_or_else(|| TransferError::Protocol(format!("bad action {value:?}")))?
                }
                "zip" => {
                    ans.compression = if value == "zlib" {
                        Compression::Zlib
                    } else {
                        Compression::None
                    }
                }
                "ft" => {
                    ans.ftype = match value {
                        "directory" => FileType::Directory,
                        "symlink" => FileType::Symlink,
                        "link" => FileType::Link,
                        _ => FileType::Regular,
                    }
                }
                "tt" => {
                    ans.ttype = if value == "rsync" {
                        TransmissionType::Rsync
                    } else {
                        TransmissionType::Simple
                    }
                }
                "q" => ans.quiet = value.parse().unwrap_or(0),
                "id" => ans.id = sanitize(value),
                "fid" => ans.file_id = sanitize(value),
                "pw" => ans.bypass = String::from_utf8_lossy(&b64decode(value.as_bytes())).into(),
                "n" => ans.name = String::from_utf8_lossy(&b64decode(value.as_bytes())).into(),
                "st" => ans.status = String::from_utf8_lossy(&b64decode(value.as_bytes())).into(),
                "pr" => ans.parent = sanitize(value),
                "mod" => ans.mtime = value.parse().unwrap_or(-1),
                "prm" => ans.permissions = value.parse().unwrap_or(u32::MAX),
                "sz" => ans.size = value.parse().unwrap_or(-1),
                "d" => ans.data = b64decode(value.as_bytes()),
                _ => {}
            }
        }
        if ans.action == Action::Invalid {
            return Err(TransferError::Protocol(
                "no valid action specified in file transmission command".to_owned(),
            ));
        }
        Ok(ans)
    }

    /// OSC 5113 frame including the request id.
    pub fn to_frame(&self, request_id: &str) -> Vec<u8> {
        format!(
            "\x1b]{};id={};{}\x1b\\",
            FILE_TRANSFER_CODE,
            request_id,
            self.serialize()
        )
        .into_bytes()
    }
}

fn b64(data: &[u8]) -> String {
    String::from_utf8(b64encode(data, false)).expect("base64 is ASCII")
}

/// Strip characters kitty considers unsafe in non-base64 string fields.
pub fn sanitize(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || "_:./@-".contains(*ch))
        .collect()
}

/// Incremental OSC 5113 extractor. It accepts split reads and both ST and BEL
/// terminators.
#[derive(Default)]
pub struct Osc5113Parser {
    buffer: Vec<u8>,
}

impl Osc5113Parser {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn feed(&mut self, data: &[u8]) {
        self.buffer.extend_from_slice(data);
    }

    /// Remove and return the next complete 5113 payload (the part after
    /// `5113;`), if present.
    pub fn next_payload(&mut self) -> Option<Vec<u8>> {
        let prefix = format!("\x1b]{FILE_TRANSFER_CODE};").into_bytes();
        let start = find_subslice(&self.buffer, &prefix)?;
        let body_start = start + prefix.len();
        let mut index = body_start;
        while index < self.buffer.len() {
            if self.buffer[index] == 0x07 {
                let payload = self.buffer[body_start..index].to_vec();
                self.buffer.drain(..=index);
                return Some(payload);
            }
            if self.buffer[index] == 0x1b && self.buffer.get(index + 1) == Some(&b'\\') {
                let payload = self.buffer[body_start..index].to_vec();
                self.buffer.drain(..index + 2);
                return Some(payload);
            }
            index += 1;
        }
        // Drop anything before the prefix so the buffer cannot grow forever.
        if start > 0 {
            self.buffer.drain(..start);
        }
        None
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Direction {
    #[default]
    Download,
    Upload,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Normal,
    Mirror,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CompressPolicy {
    #[default]
    Auto,
    Never,
    Always,
}

#[derive(Clone, Debug)]
pub struct TransferOptions {
    pub direction: Direction,
    pub mode: Mode,
    pub compress: CompressPolicy,
    pub bypass: String,
    pub confirm_paths: bool,
    pub transmit_deltas: bool,
    pub help: bool,
}

impl Default for TransferOptions {
    fn default() -> Self {
        Self {
            direction: Direction::Download,
            mode: Mode::Normal,
            compress: CompressPolicy::Auto,
            bypass: String::new(),
            confirm_paths: false,
            transmit_deltas: false,
            help: false,
        }
    }
}

pub fn parse_transfer_args(
    args: &[String],
) -> Result<(TransferOptions, Vec<String>), TransferError> {
    let mut opts = TransferOptions::default();
    let mut rest = Vec::new();
    let mut index = 0usize;
    while index < args.len() {
        let arg = &args[index];
        if arg == "--" {
            rest.extend_from_slice(&args[index + 1..]);
            break;
        }
        if !arg.starts_with('-') || arg == "-" {
            rest.push(arg.clone());
            index += 1;
            continue;
        }
        let (name, inline) = arg
            .split_once('=')
            .map_or((arg.as_str(), None), |(a, b)| (a, Some(b.to_owned())));
        let mut take = |option: &'static str| -> Result<String, TransferError> {
            if let Some(value) = &inline {
                return Ok(value.clone());
            }
            index += 1;
            args.get(index)
                .cloned()
                .ok_or_else(|| TransferError::Usage(format!("{option} requires a value")))
        };
        match name {
            "-h" | "--help" => opts.help = true,
            "-d" | "--direction" => {
                opts.direction = match take("--direction")?.as_str() {
                    "send" | "download" => Direction::Download,
                    "receive" | "upload" => Direction::Upload,
                    other => {
                        return Err(TransferError::Usage(format!("invalid direction: {other}")))
                    }
                }
            }
            "-m" | "--mode" => {
                opts.mode = match take("--mode")?.as_str() {
                    "normal" => Mode::Normal,
                    "mirror" => Mode::Mirror,
                    other => return Err(TransferError::Usage(format!("invalid mode: {other}"))),
                }
            }
            "--compress" => {
                opts.compress = match take("--compress")?.as_str() {
                    "auto" => CompressPolicy::Auto,
                    "never" => CompressPolicy::Never,
                    "always" => CompressPolicy::Always,
                    other => {
                        return Err(TransferError::Usage(format!("invalid compress: {other}")))
                    }
                }
            }
            "-p" | "--permissions-bypass" => opts.bypass = take("--permissions-bypass")?,
            "-c" | "--confirm-paths" => opts.confirm_paths = true,
            "-x" | "--transmit-deltas" => opts.transmit_deltas = true,
            other => return Err(TransferError::Usage(format!("unknown option: {other}"))),
        }
        index += 1;
    }
    Ok((opts, rest))
}

/// Load a bypass password from the forms accepted by the kitten.
pub fn read_bypass(loc: &str) -> io::Result<String> {
    if loc.is_empty() {
        return Ok(String::new());
    }
    if loc == "-" {
        let mut data = String::new();
        io::stdin().read_to_string(&mut data)?;
        return Ok(data.trim().to_owned());
    }
    if loc.chars().all(|c| c.is_ascii_digit()) {
        if let Ok(fd) = loc.parse::<i32>() {
            let mut file = unsafe { File::from_raw_fd_checked(fd) };
            let mut data = String::new();
            file.read_to_string(&mut data)?;
            return Ok(data.trim().to_owned());
        }
    }
    if let Some(rest) = loc.strip_prefix('~') {
        let home = std::env::var("HOME").unwrap_or_default();
        let path = PathBuf::from(home).join(rest.trim_start_matches('/'));
        return Ok(fs::read_to_string(path)?.trim().to_owned());
    }
    if loc.starts_with('.') || loc.starts_with('/') {
        return Ok(fs::read_to_string(loc)?.trim().to_owned());
    }
    Ok(loc.to_owned())
}

// A tiny helper to avoid pulling in libc for this optional path. It is never
// used for descriptors owned by the standard library.
trait FromRawFdChecked {
    unsafe fn from_raw_fd_checked(fd: i32) -> File;
}

impl FromRawFdChecked for File {
    unsafe fn from_raw_fd_checked(fd: i32) -> File {
        use std::os::fd::FromRawFd;
        File::from_raw_fd(fd)
    }
}

#[derive(Debug)]
pub enum TransferError {
    Io(io::Error),
    Usage(String),
    Protocol(String),
    Permission(String),
}

impl std::fmt::Display for TransferError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(err) => write!(f, "{err}"),
            Self::Usage(msg) | Self::Protocol(msg) | Self::Permission(msg) => f.write_str(msg),
        }
    }
}

impl std::error::Error for TransferError {}

impl From<io::Error> for TransferError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

/// Expand a leading `~` only, like the kitten.
pub fn expand_home(path: &str, home: &Path) -> PathBuf {
    if path == "~" {
        return home.to_path_buf();
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return home.join(rest);
    }
    PathBuf::from(path)
}

fn absolute(path: &Path, cwd: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

fn delta_spool_dir() -> PathBuf {
    std::env::temp_dir().join(format!("kitr-transfer-deltas-{}", std::process::id()))
}

fn remote_join(base: &str, child: &str) -> String {
    if base.is_empty() {
        return child.to_owned();
    }
    format!("{}/{}", base.trim_end_matches('/'), child)
}

/// One file queued for sending.
#[derive(Clone, Debug)]
pub struct SendFile {
    pub local_path: PathBuf,
    pub remote_path: String,
    pub file_type: FileType,
    pub size: i64,
    pub mtime_ns: i64,
    pub permissions: u32,
    pub file_id: String,
    pub remote_id: String,
    pub hard_link_target: Option<String>,
    pub symlink_target: Option<String>,
    pub state: SendFileState,
    pub compressed: bool,
    pub ttype: TransmissionType,
    pub delta: Option<Vec<u8>>,
    /// Disk-backed delta for large files (see `rsync::DISK_DELTA_THRESHOLD`).
    pub delta_path: Option<PathBuf>,
    pub delta_len: u64,
    pub sent: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SendFileState {
    #[default]
    Waiting,
    WaitingForData,
    Transmitting,
    Finished,
    Acknowledged,
    Failed,
}

fn should_compress(path: &Path, policy: CompressPolicy) -> bool {
    match policy {
        CompressPolicy::Always => true,
        CompressPolicy::Never => false,
        CompressPolicy::Auto => {
            let ext = path
                .extension()
                .and_then(|x| x.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            !matches!(
                ext.as_str(),
                "zip"
                    | "odt"
                    | "odp"
                    | "pptx"
                    | "docx"
                    | "gz"
                    | "bz2"
                    | "xz"
                    | "svgz"
                    | "png"
                    | "jpg"
                    | "jpeg"
                    | "gif"
                    | "webp"
                    | "mp4"
                    | "mkv"
                    | "avi"
                    | "mov"
            )
        }
    }
}

/// Collect the files for a send operation, recursing into directories and
/// detecting hard links, matching `kittens/transfer/send.go`.
pub fn collect_send_files(
    opts: &TransferOptions,
    args: &[String],
    cwd: &Path,
    home: &Path,
) -> Result<Vec<SendFile>, TransferError> {
    let (sources, remote_base) = if opts.mode == Mode::Mirror {
        (args.to_vec(), String::new())
    } else {
        if args.len() < 2 {
            return Err(TransferError::Usage(
                "Must specify at least one local path and one remote path".to_owned(),
            ));
        }
        let mut base = args[args.len() - 1].clone();
        if args.len() > 2 && !base.ends_with('/') {
            base.push('/');
        }
        (args[..args.len() - 1].to_vec(), base)
    };
    let home_prefix = format!("{}/", home.to_string_lossy().trim_end_matches('/'));
    let mut out = Vec::new();
    let mut counter = 0usize;
    for source in sources {
        let source_path = if opts.mode == Mode::Mirror {
            let abs = absolute(&expand_home(&source, home), cwd);
            let text = abs.to_string_lossy().to_string();
            if let Some(rest) = text.strip_prefix(&home_prefix) {
                format!("~/{}", rest)
            } else {
                text
            }
        } else {
            source
        };
        let expanded = absolute(&expand_home(&source_path, home), cwd);
        let mapped = if opts.mode == Mode::Mirror {
            source_path.clone()
        } else if remote_base.ends_with('/') {
            let base_name = Path::new(&source_path)
                .file_name()
                .map(|x| x.to_string_lossy().into_owned())
                .unwrap_or_else(|| source_path.clone());
            remote_join(&remote_base, &base_name)
        } else {
            remote_base.clone()
        };
        collect_one(
            opts,
            &expanded,
            &source_path,
            &mapped,
            &mut counter,
            &mut out,
        )?;
    }
    let mut by_key: HashMap<(u64, u64), String> = HashMap::new();
    for file in &mut out {
        let meta = fs::symlink_metadata(&file.local_path)?;
        let key = (meta.dev(), meta.ino());
        if meta.file_type().is_file() {
            if let Some(first) = by_key.get(&key) {
                if first != &file.file_id {
                    file.file_type = FileType::Link;
                    file.hard_link_target = Some(format!("fid:{first}"));
                }
            } else {
                by_key.insert(key, file.file_id.clone());
            }
        }
    }
    Ok(out)
}

fn collect_one(
    opts: &TransferOptions,
    expanded: &Path,
    display: &str,
    remote: &str,
    counter: &mut usize,
    out: &mut Vec<SendFile>,
) -> Result<(), TransferError> {
    let meta = fs::symlink_metadata(expanded)?;
    *counter += 1;
    let file_id = format!("{:x}", *counter);
    let file_type = if meta.is_dir() {
        FileType::Directory
    } else if meta.file_type().is_symlink() {
        FileType::Symlink
    } else {
        FileType::Regular
    };
    let symlink_target = if file_type == FileType::Symlink {
        fs::read_link(expanded)
            .ok()
            .map(|target| format!("path:{}", target.to_string_lossy()))
    } else {
        None
    };
    let mtime_ns = meta
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos() as i64)
        .unwrap_or(-1);
    out.push(SendFile {
        local_path: expanded.to_path_buf(),
        remote_path: remote.to_owned(),
        file_type,
        size: meta.len() as i64,
        mtime_ns,
        permissions: meta.permissions().mode() & 0o7777,
        file_id: file_id.clone(),
        remote_id: file_id,
        hard_link_target: None,
        symlink_target,
        state: SendFileState::Waiting,
        compressed: file_type == FileType::Regular
            && meta.len() > 4096
            && should_compress(expanded, opts.compress),
        ttype: TransmissionType::Simple,
        delta: None,
        delta_path: None,
        delta_len: 0,
        sent: 0,
    });
    if file_type == FileType::Directory {
        let child_remote = remote.trim_end_matches('/').to_owned();
        let mut entries: Vec<_> = fs::read_dir(expanded)?.collect::<Result<_, _>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let name = entry.file_name().to_string_lossy().into_owned();
            let child_display = format!("{}/{}", display.trim_end_matches('/'), name);
            let child_expanded = entry.path();
            let child_remote = format!("{}/{}", child_remote, name);
            collect_one(
                opts,
                &child_expanded,
                &child_display,
                &child_remote,
                counter,
                out,
            )?;
        }
    }
    Ok(())
}

/// Sender state machine. Feed terminal responses with [`SendEngine::handle`]
/// and write the frames returned by [`SendEngine::pump`].
pub struct SendEngine {
    pub request_id: String,
    pub files: Vec<SendFile>,
    pub state: SendState,
    pub bypass: String,
    pub confirm_paths: bool,
    pub use_rsync: bool,
    pub errors: Vec<String>,
    signatures: HashMap<String, Vec<u8>>,
    metadata_sent: bool,
    next_send: usize,
    finished: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendState {
    WaitingForPermission,
    Granted,
    Denied,
    Canceled,
    Transferring,
    Done,
}

impl SendEngine {
    pub fn new(opts: &TransferOptions, files: Vec<SendFile>) -> Self {
        let request_id = new_request_id();
        let bypass = if opts.bypass.is_empty() {
            String::new()
        } else {
            sha256_bypass(&request_id, &opts.bypass)
        };
        Self {
            request_id,
            files,
            state: SendState::WaitingForPermission,
            bypass,
            confirm_paths: opts.confirm_paths,
            use_rsync: opts.transmit_deltas,
            errors: Vec::new(),
            signatures: HashMap::new(),
            metadata_sent: false,
            next_send: 0,
            finished: false,
        }
    }

    /// Frames to write before any response is received.
    pub fn start_frames(&self) -> Vec<Vec<u8>> {
        let command = FileTransmissionCommand {
            action: Action::Send,
            bypass: self.bypass.clone(),
            ..FileTransmissionCommand::default()
        };
        vec![command.to_frame(&self.request_id)]
    }

    /// Handle a terminal response. Returns frames that must be written.
    pub fn handle(&mut self, cmd: FileTransmissionCommand) -> Result<Vec<Vec<u8>>, TransferError> {
        if !cmd.id.is_empty() && cmd.id != self.request_id {
            return Ok(Vec::new());
        }
        if cmd.action == Action::Status && cmd.status == "CANCELED" {
            self.state = SendState::Canceled;
            return Ok(Vec::new());
        }
        let mut frames = Vec::new();
        match self.state {
            SendState::WaitingForPermission => {
                if cmd.action == Action::Status {
                    if cmd.status.starts_with("OK") {
                        self.state = SendState::Granted;
                        frames.extend(self.metadata_frames());
                    } else {
                        self.state = SendState::Denied;
                        self.errors.push(cmd.status);
                    }
                }
            }
            SendState::Granted | SendState::Transferring => match cmd.action {
                Action::Status if !cmd.file_id.is_empty() => {
                    let Some(file) = self.files.iter_mut().find(|f| f.file_id == cmd.file_id)
                    else {
                        return Ok(frames);
                    };
                    if cmd.status.starts_with("STARTED") {
                        self.state = SendState::Transferring;
                        if !cmd.name.is_empty() {
                            file.remote_path = cmd.name;
                        }
                        if file.file_type == FileType::Directory {
                            file.state = SendFileState::Finished;
                        } else if cmd.ttype == TransmissionType::Rsync {
                            file.state = SendFileState::WaitingForData;
                        } else {
                            file.state = SendFileState::Transmitting;
                        }
                    } else if cmd.status.starts_with("PROGRESS") {
                        file.sent = cmd.size.max(0) as u64;
                    } else {
                        file.state = SendFileState::Acknowledged;
                        if cmd.status == "OK" {
                            file.state = SendFileState::Acknowledged;
                        } else {
                            file.state = SendFileState::Failed;
                            self.errors.push(cmd.status);
                        }
                    }
                }
                Action::Data | Action::EndData if !cmd.file_id.is_empty() => {
                    let Some(index) = self.files.iter().position(|f| f.file_id == cmd.file_id)
                    else {
                        return Ok(frames);
                    };
                    if self.files[index].state != SendFileState::WaitingForData {
                        return Ok(frames);
                    }
                    let buffer = self.signatures.entry(cmd.file_id.clone()).or_default();
                    buffer.extend_from_slice(&cmd.data);
                    if cmd.action == Action::EndData {
                        let signature = self.signatures.remove(&cmd.file_id).unwrap_or_default();
                        let parsed = rsync::parse_signature(&signature)
                            .map_err(|err| TransferError::Protocol(err.to_string()))?;
                        let source = fs::read(&self.files[index].local_path)?;
                        if rsync::should_spill_delta(source.len() as u64) {
                            let dir = delta_spool_dir();
                            fs::create_dir_all(&dir)?;
                            let path = dir.join(format!("{}.delta", self.files[index].file_id));
                            let len = rsync::create_delta_file(
                                &self.files[index].local_path,
                                &parsed,
                                &path,
                            )
                            .map_err(|err| TransferError::Protocol(err.to_string()))?;
                            self.files[index].delta_path = Some(path);
                            self.files[index].delta_len = len;
                        } else {
                            let delta = rsync::create_delta(&source, &parsed)
                                .map_err(|err| TransferError::Protocol(err.to_string()))?;
                            self.files[index].delta = Some(delta);
                        }
                        self.files[index].state = SendFileState::Transmitting;
                    }
                }
                _ => {}
            },
            SendState::Denied | SendState::Canceled | SendState::Done => {}
        }
        frames.extend(self.pump());
        Ok(frames)
    }

    fn metadata_frames(&mut self) -> Vec<Vec<u8>> {
        if self.metadata_sent {
            return Vec::new();
        }
        self.metadata_sent = true;
        let mut frames = Vec::with_capacity(self.files.len());
        for file in &mut self.files {
            let use_rsync =
                self.use_rsync && file.file_type == FileType::Regular && file.size > 4096;
            file.ttype = if use_rsync {
                TransmissionType::Rsync
            } else {
                TransmissionType::Simple
            };
            let mut cmd = FileTransmissionCommand {
                action: Action::File,
                ftype: file.file_type,
                file_id: file.file_id.clone(),
                name: file.remote_path.clone(),
                status: file.remote_id.clone(),
                mtime: file.mtime_ns,
                permissions: file.permissions,
                size: file.size,
                ttype: file.ttype,
                compression: if use_rsync {
                    Compression::None
                } else if file.compressed {
                    Compression::Zlib
                } else {
                    Compression::None
                },
                ..FileTransmissionCommand::default()
            };
            if let Some(target) = &file.hard_link_target {
                cmd.data = target.as_bytes().to_vec();
            } else if let Some(target) = &file.symlink_target {
                cmd.data = target.as_bytes().to_vec();
            }
            frames.push(cmd.to_frame(&self.request_id));
        }
        frames
    }

    /// Generate the next data chunk and any new completion frames.
    pub fn pump(&mut self) -> Vec<Vec<u8>> {
        let mut frames = Vec::new();
        while self.next_send < self.files.len() {
            let index = self.next_send;
            if matches!(
                self.files[index].state,
                SendFileState::Waiting | SendFileState::WaitingForData
            ) {
                break;
            }
            if self.files[index].state != SendFileState::Transmitting {
                self.next_send += 1;
                continue;
            }
            frames.extend(self.chunk_for(index).unwrap_or_default());
            if self.files[index].state != SendFileState::Transmitting {
                self.next_send += 1;
            } else {
                break;
            }
        }
        if self.files.iter().all(|f| {
            matches!(
                f.state,
                SendFileState::Finished | SendFileState::Acknowledged | SendFileState::Failed
            )
        }) {
            self.state = SendState::Done;
            self.finished = true;
            frames.push(
                FileTransmissionCommand {
                    action: Action::Finish,
                    ..FileTransmissionCommand::default()
                }
                .to_frame(&self.request_id),
            );
        }
        frames
    }

    fn chunk_for(&mut self, index: usize) -> Option<Vec<Vec<u8>>> {
        let file = &mut self.files[index];
        let payload = match file.file_type {
            FileType::Directory => {
                file.state = SendFileState::Finished;
                return Some(Vec::new());
            }
            FileType::Symlink => {
                let target = file.symlink_target.clone().unwrap_or_default();
                file.state = SendFileState::Finished;
                target.into_bytes()
            }
            FileType::Link => {
                file.state = SendFileState::Finished;
                file.hard_link_target
                    .clone()
                    .unwrap_or_default()
                    .into_bytes()
            }
            FileType::Regular => {
                let (mut data, compress) = if let Some(path) = file.delta_path.take() {
                    let data = fs::read(&path).unwrap_or_default();
                    let _ = fs::remove_file(&path);
                    (data, false)
                } else if let Some(delta) = file.delta.take() {
                    (delta, false)
                } else {
                    let data = fs::read(&file.local_path).ok().unwrap_or_default();
                    (
                        data,
                        file.compressed && file.ttype == TransmissionType::Simple,
                    )
                };
                if compress {
                    if let Ok(compressed) = zlib_compress(&data) {
                        data = compressed;
                    }
                }
                file.state = SendFileState::Finished;
                data
            }
        };
        if payload.is_empty() {
            return Some(vec![FileTransmissionCommand {
                action: Action::EndData,
                file_id: file.file_id.clone(),
                ..FileTransmissionCommand::default()
            }
            .to_frame(&self.request_id)]);
        }
        let mut frames = Vec::new();
        let chunks: Vec<&[u8]> = payload.chunks(CHUNK_SIZE).collect();
        for (i, chunk) in chunks.iter().enumerate() {
            let last = i + 1 == chunks.len();
            let command = FileTransmissionCommand {
                action: if last { Action::EndData } else { Action::Data },
                file_id: file.file_id.clone(),
                data: chunk.to_vec(),
                ..FileTransmissionCommand::default()
            };
            frames.push(command.to_frame(&self.request_id));
        }
        Some(frames)
    }

    pub fn is_done(&self) -> bool {
        self.finished
            || matches!(
                self.state,
                SendState::Denied | SendState::Canceled | SendState::Done
            )
    }
}

/// A file discovered on the terminal side during an upload.
#[derive(Clone, Debug)]
pub struct ReceiveFile {
    pub spec_id: usize,
    pub remote_id: String,
    pub remote_path: String,
    pub parent: String,
    pub file_type: FileType,
    pub size: i64,
    pub mtime_ns: i64,
    pub permissions: u32,
    pub compression: Compression,
    pub ttype: TransmissionType,
    pub expects_delta: bool,
    pub target: String,
    pub dest_path: PathBuf,
    pub state: ReceiveFileState,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ReceiveFileState {
    #[default]
    Waiting,
    Transmitting,
    Done,
    Failed,
}

/// Receiver state machine for uploads.
pub struct ReceiveEngine {
    pub request_id: String,
    pub specs: Vec<String>,
    pub dest: String,
    pub state: ReceiveState,
    pub bypass: String,
    pub mode: Mode,
    pub files: Vec<ReceiveFile>,
    pub errors: Vec<String>,
    pub remote_home: String,
    pub use_rsync: bool,
    queued: HashSet<String>,
    received: HashMap<String, Vec<u8>>,
    delta_files: HashMap<String, PathBuf>,
    signatures: HashMap<String, Vec<u8>>,
    finished: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReceiveState {
    WaitingForPermission,
    Metadata,
    Transferring,
    Done,
    Canceled,
}

impl ReceiveEngine {
    pub fn new(opts: &TransferOptions, specs: Vec<String>, dest: String) -> Self {
        let request_id = new_request_id();
        let bypass = if opts.bypass.is_empty() {
            String::new()
        } else {
            sha256_bypass(&request_id, &opts.bypass)
        };
        Self {
            request_id,
            specs,
            dest,
            state: ReceiveState::WaitingForPermission,
            bypass,
            mode: opts.mode,
            files: Vec::new(),
            errors: Vec::new(),
            remote_home: String::new(),
            use_rsync: opts.transmit_deltas,
            queued: HashSet::new(),
            received: HashMap::new(),
            delta_files: HashMap::new(),
            signatures: HashMap::new(),
            finished: false,
        }
    }

    pub fn start_frames(&self) -> Vec<Vec<u8>> {
        let mut frames = Vec::new();
        frames.push(
            FileTransmissionCommand {
                action: Action::Receive,
                bypass: self.bypass.clone(),
                size: self.specs.len() as i64,
                ..FileTransmissionCommand::default()
            }
            .to_frame(&self.request_id),
        );
        for (i, spec) in self.specs.iter().enumerate() {
            frames.push(
                FileTransmissionCommand {
                    action: Action::File,
                    file_id: i.to_string(),
                    name: spec.clone(),
                    ..FileTransmissionCommand::default()
                }
                .to_frame(&self.request_id),
            );
        }
        frames
    }

    pub fn handle(&mut self, cmd: FileTransmissionCommand) -> Result<Vec<Vec<u8>>, TransferError> {
        if !cmd.id.is_empty() && cmd.id != self.request_id {
            return Ok(Vec::new());
        }
        if cmd.action == Action::Status && cmd.status == "CANCELED" {
            self.state = ReceiveState::Canceled;
            return Ok(Vec::new());
        }
        let mut frames = Vec::new();
        match self.state {
            ReceiveState::WaitingForPermission => {
                if cmd.action == Action::Status {
                    if cmd.status.starts_with("OK") {
                        self.state = ReceiveState::Metadata;
                    } else {
                        self.errors.push(cmd.status);
                        self.state = ReceiveState::Done;
                    }
                }
            }
            ReceiveState::Metadata => match cmd.action {
                Action::File => {
                    let spec_id = cmd.file_id.parse::<usize>().unwrap_or(usize::MAX);
                    let target = String::from_utf8_lossy(&cmd.data).into_owned();
                    self.files.push(ReceiveFile {
                        spec_id,
                        remote_id: cmd.status.clone(),
                        remote_path: cmd.name.clone(),
                        parent: cmd.parent.clone(),
                        file_type: cmd.ftype,
                        size: cmd.size.max(0),
                        mtime_ns: cmd.mtime,
                        permissions: cmd.permissions,
                        compression: cmd.compression,
                        ttype: cmd.ttype,
                        expects_delta: false,
                        target,
                        dest_path: PathBuf::new(),
                        state: ReceiveFileState::Waiting,
                    });
                }
                Action::Status if cmd.file_id.is_empty() => {
                    if cmd.status.starts_with("OK") {
                        self.remote_home = cmd.name.clone();
                        self.assign_paths()?;
                        self.state = ReceiveState::Transferring;
                        frames.extend(self.request_next()?);
                    } else {
                        self.errors.push(cmd.status);
                        self.state = ReceiveState::Done;
                    }
                }
                Action::Status if !cmd.file_id.is_empty() => {
                    self.errors.push(cmd.status);
                }
                _ => {}
            },
            ReceiveState::Transferring => match cmd.action {
                Action::Status if !cmd.file_id.is_empty() => {
                    if cmd.status.starts_with("STARTED") {
                        if let Some(file) =
                            self.files.iter_mut().find(|f| f.remote_id == cmd.file_id)
                        {
                            file.state = ReceiveFileState::Transmitting;
                        }
                    } else {
                        if let Some(file) =
                            self.files.iter_mut().find(|f| f.remote_id == cmd.file_id)
                        {
                            file.state = ReceiveFileState::Failed;
                        }
                        self.errors.push(cmd.status);
                    }
                }
                Action::Data | Action::EndData => {
                    let Some(index) = self.files.iter().position(|f| f.remote_id == cmd.file_id)
                    else {
                        return Ok(frames);
                    };
                    let spill = self.files[index].expects_delta
                        && rsync::should_spill_delta(self.files[index].size.max(0) as u64);
                    if spill {
                        let dir = delta_spool_dir();
                        fs::create_dir_all(&dir)?;
                        let path = self
                            .delta_files
                            .entry(cmd.file_id.clone())
                            .or_insert_with(|| dir.join(format!("recv-{}.delta", cmd.file_id)));
                        use std::io::Write;
                        let mut file = fs::OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(&*path)?;
                        file.write_all(&cmd.data)?;
                    } else {
                        let buffer = self.received.entry(cmd.file_id.clone()).or_default();
                        buffer.extend_from_slice(&cmd.data);
                    }
                    if cmd.action == Action::EndData {
                        let expects_delta = self.files[index].expects_delta;
                        if spill {
                            let delta_path =
                                self.delta_files.remove(&cmd.file_id).unwrap_or_default();
                            let signature = self.signatures.get(&cmd.file_id);
                            let block_size = signature
                                .and_then(|sig| rsync::parse_signature_header(sig).ok())
                                .map(|size| size as usize)
                                .unwrap_or(rsync::DEFAULT_BLOCK_SIZE);
                            let target_path = self.files[index].dest_path.clone();
                            rsync::apply_delta_file(
                                &target_path,
                                &delta_path,
                                block_size,
                                &target_path,
                            )
                            .map_err(|err| TransferError::Protocol(err.to_string()))?;
                            let _ = fs::remove_file(&delta_path);
                        } else {
                            let data = self.received.remove(&cmd.file_id).unwrap_or_default();
                            let data = if expects_delta {
                                let signature = self.signatures.get(&cmd.file_id);
                                let block_size = signature
                                    .and_then(|sig| rsync::parse_signature_header(sig).ok())
                                    .map(|size| size as usize)
                                    .unwrap_or(rsync::DEFAULT_BLOCK_SIZE);
                                let target_path = self.files[index].dest_path.clone();
                                let target = fs::read(&target_path).unwrap_or_default();
                                rsync::apply_delta_with_block_size(&target, &data, block_size)
                                    .map_err(|err| TransferError::Protocol(err.to_string()))?
                            } else if self.files[index].file_type == FileType::Regular
                                && self.files[index].compression == Compression::Zlib
                            {
                                zlib_decompress(&data, self.files[index].size as usize)
                                    .unwrap_or(data)
                            } else {
                                data
                            };
                            write_received(&mut self.files[index], &data)?;
                        }
                        self.files[index].state = ReceiveFileState::Done;
                    }
                }
                _ => {}
            },
            ReceiveState::Done | ReceiveState::Canceled => {}
        }
        if self.state == ReceiveState::Transferring {
            frames.extend(self.request_next()?);
            if self.files.iter().all(|f| {
                !matches!(
                    f.file_type,
                    FileType::Regular | FileType::Symlink | FileType::Link
                ) || matches!(f.state, ReceiveFileState::Done | ReceiveFileState::Failed)
            }) {
                finalize_receive(&self.files)?;
                self.state = ReceiveState::Done;
                self.finished = true;
                frames.push(
                    FileTransmissionCommand {
                        action: Action::Finish,
                        ..FileTransmissionCommand::default()
                    }
                    .to_frame(&self.request_id),
                );
            }
        }
        Ok(frames)
    }

    fn request_next(&mut self) -> Result<Vec<Vec<u8>>, TransferError> {
        let mut frames = Vec::new();
        for index in 0..self.files.len() {
            if self.files[index].file_type == FileType::Directory {
                if self.files[index].state == ReceiveFileState::Waiting {
                    fs::create_dir_all(&self.files[index].dest_path)?;
                    self.files[index].state = ReceiveFileState::Done;
                }
                continue;
            }
            if self.files[index].state != ReceiveFileState::Waiting
                || self.queued.contains(&self.files[index].remote_id)
            {
                continue;
            }
            let remote_id = self.files[index].remote_id.clone();
            self.queued.insert(remote_id.clone());
            let should_rsync = self.use_rsync
                && self.files[index].file_type == FileType::Regular
                && self.files[index].size > 4096
                && fs::metadata(&self.files[index].dest_path)
                    .map(|meta| meta.is_file() && meta.len() > 4096)
                    .unwrap_or(false);
            let mut signature = None;
            if should_rsync {
                let target = fs::read(&self.files[index].dest_path)?;
                let block_size = rsync::block_size_for(target.len() as u64) as u32;
                let serialized = rsync::create_signature(&target, block_size)
                    .map_err(|err| TransferError::Protocol(err.to_string()))?;
                signature = Some(serialized);
            }
            self.files[index].ttype = if signature.is_some() {
                TransmissionType::Rsync
            } else {
                TransmissionType::Simple
            };
            self.files[index].expects_delta = signature.is_some();
            frames.push(
                FileTransmissionCommand {
                    action: Action::File,
                    file_id: remote_id.clone(),
                    name: self.files[index].remote_path.clone(),
                    ttype: self.files[index].ttype,
                    compression: if self.files[index].file_type == FileType::Regular
                        && self.files[index].size > 4096
                        && signature.is_none()
                    {
                        Compression::Zlib
                    } else {
                        Compression::None
                    },
                    ..FileTransmissionCommand::default()
                }
                .to_frame(&self.request_id),
            );
            if let Some(signature) = signature {
                self.signatures.insert(remote_id.clone(), signature.clone());
                let chunks: Vec<&[u8]> = signature.chunks(DATA_CHUNK_SIZE).collect();
                for (chunk_index, chunk) in chunks.iter().enumerate() {
                    let last = chunk_index + 1 == chunks.len();
                    frames.push(
                        FileTransmissionCommand {
                            action: if last { Action::EndData } else { Action::Data },
                            file_id: remote_id.clone(),
                            data: chunk.to_vec(),
                            ..FileTransmissionCommand::default()
                        }
                        .to_frame(&self.request_id),
                    );
                }
                if chunks.is_empty() {
                    frames.push(
                        FileTransmissionCommand {
                            action: Action::EndData,
                            file_id: remote_id,
                            ..FileTransmissionCommand::default()
                        }
                        .to_frame(&self.request_id),
                    );
                }
            }
        }
        Ok(frames)
    }

    fn assign_paths(&mut self) -> Result<(), TransferError> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        let mut roots: HashMap<usize, PathBuf> = HashMap::new();
        let spec_count = self.files.len();
        let dest_is_dir = self.dest.ends_with('/')
            || spec_count > 1
            || Path::new(&expand_home(&self.dest, &home)).is_dir();
        for file in &self.files {
            if file.spec_id == usize::MAX {
                continue;
            }
            let root = if self.mode == Mode::Mirror {
                let path = expand_home(&file.remote_path, &home);
                path.parent().unwrap_or(Path::new(".")).to_path_buf()
            } else if dest_is_dir {
                expand_home(&self.dest, &home)
                    .join(Path::new(&file.remote_path).file_name().unwrap_or_default())
            } else {
                expand_home(&self.dest, &home)
            };
            roots.entry(file.spec_id).or_insert(root);
        }
        for file in &mut self.files {
            let Some(root) = roots.get(&file.spec_id) else {
                continue;
            };
            let name = Path::new(&file.remote_path)
                .file_name()
                .map(|x| x.to_owned())
                .unwrap_or_default();
            file.dest_path = if self.mode == Mode::Normal && !dest_is_dir {
                root.clone()
            } else {
                root.join(name)
            };
        }
        // Non-directory files placed under a parent may be processed before
        // their parent's path is known; resolve until stable.
        for _ in 0..self.files.len().max(1) {
            let snapshot: HashMap<String, PathBuf> = self
                .files
                .iter()
                .map(|f| (f.remote_id.clone(), f.dest_path.clone()))
                .collect();
            for file in &mut self.files {
                if !file.parent.is_empty() {
                    if let Some(parent) = snapshot.get(&file.parent) {
                        let name = Path::new(&file.remote_path)
                            .file_name()
                            .map(|x| x.to_owned())
                            .unwrap_or_default();
                        if parent != Path::new("") {
                            file.dest_path = parent.join(name);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    pub fn is_done(&self) -> bool {
        self.finished || matches!(self.state, ReceiveState::Done | ReceiveState::Canceled)
    }
}

fn write_received(file: &mut ReceiveFile, data: &[u8]) -> io::Result<()> {
    if let Some(parent) = file.dest_path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    match file.file_type {
        FileType::Symlink => {
            let _ = fs::remove_file(&file.dest_path);
            let target = if file.target.is_empty() {
                String::from_utf8_lossy(data).into_owned()
            } else {
                file.target.clone()
            };
            std::os::unix::fs::symlink(target, &file.dest_path)?;
        }
        FileType::Link => {
            let _ = fs::remove_file(&file.dest_path);
            fs::write(&file.dest_path, data)?;
        }
        _ => {
            fs::write(&file.dest_path, data)?;
            let _ = fs::set_permissions(
                &file.dest_path,
                fs::Permissions::from_mode(file.permissions & 0o7777),
            );
            if file.mtime_ns >= 0 {
                if let Ok(file_handle) = OpenOptions::new().write(true).open(&file.dest_path) {
                    let time = UNIX_EPOCH + Duration::from_nanos(file.mtime_ns as u64);
                    let times = fs::FileTimes::new().set_modified(time).set_accessed(time);
                    let _ = file_handle.set_times(times);
                }
            }
        }
    }
    Ok(())
}

fn finalize_receive(files: &[ReceiveFile]) -> io::Result<()> {
    for file in files {
        if file.file_type == FileType::Directory {
            fs::create_dir_all(&file.dest_path)?;
        }
    }
    Ok(())
}

pub fn new_request_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{:x}{:04x}", std::process::id(), (nanos as u64) & 0xffff)
}

pub fn sha256_bypass(request_id: &str, password: &str) -> String {
    let digest = sha256(format!("{request_id};{password}").as_bytes());
    let mut out = String::from("sha256:");
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

#[link(name = "z")]
extern "C" {
    fn compressBound(source_len: std::os::raw::c_ulong) -> std::os::raw::c_ulong;
    fn compress2(
        dest: *mut u8,
        dest_len: *mut std::os::raw::c_ulong,
        source: *const u8,
        source_len: std::os::raw::c_ulong,
        level: std::os::raw::c_int,
    ) -> std::os::raw::c_int;
    fn uncompress(
        dest: *mut u8,
        dest_len: *mut std::os::raw::c_ulong,
        source: *const u8,
        source_len: std::os::raw::c_ulong,
    ) -> std::os::raw::c_int;
}

pub fn zlib_compress(data: &[u8]) -> io::Result<Vec<u8>> {
    let bound = unsafe { compressBound(data.len() as std::os::raw::c_ulong) } as usize;
    let mut out = vec![0u8; bound];
    let mut out_len = bound as std::os::raw::c_ulong;
    let ret = unsafe {
        compress2(
            out.as_mut_ptr(),
            &mut out_len,
            data.as_ptr(),
            data.len() as std::os::raw::c_ulong,
            6,
        )
    };
    if ret != 0 {
        return Err(io::Error::other(format!("zlib compress error {ret}")));
    }
    out.truncate(out_len as usize);
    Ok(out)
}

pub fn zlib_decompress(data: &[u8], expected: usize) -> io::Result<Vec<u8>> {
    let mut capacity = expected.max(1);
    loop {
        let mut out = vec![0u8; capacity];
        let mut out_len = capacity as std::os::raw::c_ulong;
        let ret = unsafe {
            uncompress(
                out.as_mut_ptr(),
                &mut out_len,
                data.as_ptr(),
                data.len() as std::os::raw::c_ulong,
            )
        };
        if ret == 0 {
            out.truncate(out_len as usize);
            return Ok(out);
        }
        if ret == -5 && capacity < 512 * 1024 * 1024 {
            capacity = capacity.saturating_mul(2);
            continue;
        }
        return Err(io::Error::other(format!("zlib decompress error {ret}")));
    }
}

/// Run one transfer using a terminal transport. Returns the shell status.
pub fn run_transfer(
    opts: &TransferOptions,
    args: &[String],
    cwd: &Path,
    home: &Path,
    transport: &mut dyn InteractiveTransport,
    out: &mut dyn Write,
) -> Result<(), TransferError> {
    if opts.help {
        writeln!(out, "{TRANSFER_USAGE}")?;
        return Ok(());
    }
    if opts.direction == Direction::Download {
        let files = collect_send_files(opts, args, cwd, home)?;
        if files.is_empty() {
            return Err(TransferError::Usage(
                "Must specify at least one file to transfer".to_owned(),
            ));
        }
        let mut engine = SendEngine::new(opts, files);
        for file in &engine.files {
            if file.file_type == FileType::Regular {
                writeln!(
                    out,
                    "Sending {} -> {}",
                    file.local_path.display(),
                    file.remote_path
                )?;
            }
        }
        let mut parser = Osc5113Parser::new();
        for frame in engine.start_frames() {
            transport.write_all(&frame)?;
        }
        transport.flush()?;
        let mut buffer = [0u8; 8192];
        while !engine.is_done() {
            let n = match transport.read(&mut buffer, Duration::from_millis(200)) {
                Ok(n) => n,
                Err(err) if err.kind() == io::ErrorKind::TimedOut => continue,
                Err(err) => return Err(err.into()),
            };
            if n == 0 {
                break;
            }
            parser.feed(&buffer[..n]);
            while let Some(payload) = parser.next_payload() {
                let cmd = FileTransmissionCommand::deserialize(&payload)?;
                for frame in engine.handle(cmd)? {
                    transport.write_all(&frame)?;
                }
                transport.flush()?;
            }
        }
        if !engine.errors.is_empty() {
            return Err(TransferError::Protocol(engine.errors.join("\n")));
        }
    } else {
        let (specs, dest) = if opts.mode == Mode::Mirror {
            if args.is_empty() {
                return Err(TransferError::Usage(
                    "Must specify at least one file to transfer".to_owned(),
                ));
            }
            (args.to_vec(), String::new())
        } else {
            if args.len() < 2 {
                return Err(TransferError::Usage(
                    "Must specify at least one source and a destination file to transfer"
                        .to_owned(),
                ));
            }
            (
                args[..args.len() - 1].to_vec(),
                args[args.len() - 1].clone(),
            )
        };
        let mut engine = ReceiveEngine::new(opts, specs, dest);
        let mut parser = Osc5113Parser::new();
        for frame in engine.start_frames() {
            transport.write_all(&frame)?;
        }
        transport.flush()?;
        let mut buffer = [0u8; 8192];
        while !engine.is_done() {
            let n = match transport.read(&mut buffer, Duration::from_millis(200)) {
                Ok(n) => n,
                Err(err) if err.kind() == io::ErrorKind::TimedOut => continue,
                Err(err) => return Err(err.into()),
            };
            if n == 0 {
                break;
            }
            parser.feed(&buffer[..n]);
            while let Some(payload) = parser.next_payload() {
                let cmd = FileTransmissionCommand::deserialize(&payload)?;
                for frame in engine.handle(cmd)? {
                    transport.write_all(&frame)?;
                }
                transport.flush()?;
            }
        }
        for file in &engine.files {
            writeln!(out, "{} -> {}", file.remote_path, file.dest_path.display())?;
        }
        if !engine.errors.is_empty() {
            return Err(TransferError::Protocol(engine.errors.join("\n")));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "kitty-transfer-{}-{}-{}",
            std::process::id(),
            label,
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn command_roundtrip_matches_kitty_field_names() {
        let cmd = FileTransmissionCommand {
            action: Action::File,
            ftype: FileType::Symlink,
            file_id: "7".to_owned(),
            name: "a b/c".to_owned(),
            size: 12,
            mtime: 42,
            permissions: 0o644,
            data: b"target".to_vec(),
            ..FileTransmissionCommand::default()
        };
        let encoded = cmd.serialize();
        assert!(encoded.contains("ac=file"));
        assert!(encoded.contains("ft=symlink"));
        assert!(encoded.contains("fid=7"));
        assert!(!encoded.contains("a b"));
        let decoded = FileTransmissionCommand::deserialize(encoded.as_bytes()).unwrap();
        assert_eq!(decoded.action, Action::File);
        assert_eq!(decoded.ftype, FileType::Symlink);
        assert_eq!(decoded.name, "a b/c");
        assert_eq!(decoded.data, b"target");
    }

    #[test]
    fn osc_parser_handles_split_frames() {
        let frame = FileTransmissionCommand {
            action: Action::Status,
            status: "OK".to_owned(),
            id: "x".to_owned(),
            ..FileTransmissionCommand::default()
        }
        .to_frame("x");
        let (a, b) = frame.split_at(7);
        let mut parser = Osc5113Parser::new();
        parser.feed(a);
        assert!(parser.next_payload().is_none());
        parser.feed(b);
        let payload = parser.next_payload().unwrap();
        assert!(String::from_utf8_lossy(&payload).contains("st=T0s"));
    }

    #[test]
    fn zlib_roundtrip() {
        let data = b"hello hello hello hello".repeat(100);
        let compressed = zlib_compress(&data).unwrap();
        assert_eq!(zlib_decompress(&compressed, data.len()).unwrap(), data);
    }

    #[test]
    fn collect_send_tree_and_remote_paths() {
        let dir = temp_dir("collect");
        fs::create_dir_all(dir.join("d")).unwrap();
        fs::write(dir.join("d/a.txt"), b"hello").unwrap();
        let opts = TransferOptions::default();
        let args = vec![
            dir.join("d").to_string_lossy().into_owned(),
            "/tmp/dest/".to_owned(),
        ];
        let files = collect_send_files(&opts, &args, &dir, &dir).unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].file_type, FileType::Directory);
        assert!(files[1].remote_path.starts_with("/tmp/dest/d/a.txt"));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn sender_permission_metadata_and_data() {
        let dir = temp_dir("send");
        fs::write(dir.join("a.txt"), b"abc").unwrap();
        let opts = TransferOptions::default();
        let args = vec![
            dir.join("a.txt").to_string_lossy().into_owned(),
            "/tmp/b.txt".to_owned(),
        ];
        let files = collect_send_files(&opts, &args, &dir, &dir).unwrap();
        let mut engine = SendEngine::new(&opts, files);
        assert_eq!(engine.start_frames().len(), 1);
        let mut frames = engine
            .handle(FileTransmissionCommand {
                action: Action::Status,
                id: engine.request_id.clone(),
                status: "OK".to_owned(),
                ..FileTransmissionCommand::default()
            })
            .unwrap();
        assert!(frames
            .iter()
            .any(|f| String::from_utf8_lossy(f).contains("ac=file")));
        let started = FileTransmissionCommand {
            action: Action::Status,
            id: engine.request_id.clone(),
            file_id: "1".to_owned(),
            status: "STARTED".to_owned(),
            ..FileTransmissionCommand::default()
        };
        frames = engine.handle(started).unwrap();
        assert!(frames
            .iter()
            .any(|f| String::from_utf8_lossy(f).contains("ac=end_data")));
        engine
            .handle(FileTransmissionCommand {
                action: Action::Status,
                id: engine.request_id.clone(),
                file_id: "1".to_owned(),
                status: "OK".to_owned(),
                ..FileTransmissionCommand::default()
            })
            .unwrap();
        assert!(engine.is_done());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn sender_uses_rsync_signature_to_transmit_a_delta() {
        let dir = temp_dir("rsync-send");
        let source_path = dir.join("source.txt");
        let target_path = dir.join("target.txt");
        let source = b"the quick brown fox jumps over the lazy dog\n".repeat(6000);
        let mut target = source.clone();
        target[1000..1010].copy_from_slice(b"0123456789");
        target.extend_from_slice(b"a new tail\n");
        fs::write(&source_path, &source).unwrap();
        fs::write(&target_path, &target).unwrap();
        let opts = TransferOptions {
            transmit_deltas: true,
            ..TransferOptions::default()
        };
        let args = vec![
            source_path.to_string_lossy().into_owned(),
            target_path.to_string_lossy().into_owned(),
        ];
        let files = collect_send_files(&opts, &args, &dir, &dir).unwrap();
        let mut engine = SendEngine::new(&opts, files);
        let metadata = engine
            .handle(FileTransmissionCommand {
                action: Action::Status,
                id: engine.request_id.clone(),
                status: "OK".to_owned(),
                ..FileTransmissionCommand::default()
            })
            .unwrap();
        let _ = engine
            .handle(FileTransmissionCommand {
                action: Action::Status,
                id: engine.request_id.clone(),
                file_id: "1".to_owned(),
                status: "STARTED".to_owned(),
                ttype: TransmissionType::Rsync,
                ..FileTransmissionCommand::default()
            })
            .unwrap();
        assert!(metadata
            .iter()
            .any(|frame| String::from_utf8_lossy(frame).contains("tt=rsync")));
        let signature = rsync::create_signature(&target, 128).unwrap();
        engine
            .handle(FileTransmissionCommand {
                action: Action::Data,
                id: engine.request_id.clone(),
                file_id: "1".to_owned(),
                data: signature[..40].to_vec(),
                ..FileTransmissionCommand::default()
            })
            .unwrap();
        let frames = engine
            .handle(FileTransmissionCommand {
                action: Action::EndData,
                id: engine.request_id.clone(),
                file_id: "1".to_owned(),
                data: signature[40..].to_vec(),
                ..FileTransmissionCommand::default()
            })
            .unwrap();
        let mut delta = Vec::new();
        for frame in &frames {
            let mut parser = Osc5113Parser::new();
            parser.feed(frame);
            let payload = parser.next_payload().unwrap();
            let command = FileTransmissionCommand::deserialize(&payload).unwrap();
            if matches!(command.action, Action::Data | Action::EndData) {
                delta.extend_from_slice(&command.data);
            }
        }
        assert!(!delta.is_empty());
        assert!(delta.len() < source.len());
        let parsed = rsync::parse_signature(&signature).unwrap();
        assert_eq!(
            rsync::apply_delta_with_block_size(&target, &delta, parsed.block_size as usize)
                .unwrap(),
            source
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn receiver_requests_and_applies_rsync_delta() {
        let dir = temp_dir("rsync-recv");
        let dest = dir.join("dest.txt");
        let existing = b"0123456789abcdef".repeat(4000);
        let source = b"0123456789abcdef".repeat(4000);
        fs::write(&dest, &existing).unwrap();
        let opts = TransferOptions {
            direction: Direction::Upload,
            transmit_deltas: true,
            ..TransferOptions::default()
        };
        let mut engine = ReceiveEngine::new(
            &opts,
            vec!["/remote/source.txt".to_owned()],
            dest.to_string_lossy().into_owned(),
        );
        engine
            .handle(FileTransmissionCommand {
                action: Action::Status,
                id: engine.request_id.clone(),
                status: "OK".to_owned(),
                ..FileTransmissionCommand::default()
            })
            .unwrap();
        engine
            .handle(FileTransmissionCommand {
                action: Action::File,
                id: engine.request_id.clone(),
                file_id: "0".to_owned(),
                status: "remote-1".to_owned(),
                name: "/remote/source.txt".to_owned(),
                size: source.len() as i64,
                ..FileTransmissionCommand::default()
            })
            .unwrap();
        let frames = engine
            .handle(FileTransmissionCommand {
                action: Action::Status,
                id: engine.request_id.clone(),
                status: "OK".to_owned(),
                name: "/home/remote".to_owned(),
                ..FileTransmissionCommand::default()
            })
            .unwrap();
        let mut signature = Vec::new();
        let mut requested_rsync = false;
        for frame in &frames {
            let mut parser = Osc5113Parser::new();
            parser.feed(frame);
            let payload = parser.next_payload().unwrap();
            let command = FileTransmissionCommand::deserialize(&payload).unwrap();
            if command.action == Action::File {
                requested_rsync = command.ttype == TransmissionType::Rsync;
            } else if matches!(command.action, Action::Data | Action::EndData) {
                signature.extend_from_slice(&command.data);
            }
        }
        assert!(requested_rsync);
        let parsed = rsync::parse_signature(&signature).unwrap();
        let delta = rsync::create_delta(&source, &parsed).unwrap();
        assert_eq!(
            rsync::apply_delta_with_block_size(&existing, &delta, parsed.block_size as usize)
                .unwrap(),
            source
        );
        engine
            .handle(FileTransmissionCommand {
                action: Action::Status,
                id: engine.request_id.clone(),
                file_id: "remote-1".to_owned(),
                status: "STARTED".to_owned(),
                ..FileTransmissionCommand::default()
            })
            .unwrap();
        engine
            .handle(FileTransmissionCommand {
                action: Action::EndData,
                id: engine.request_id.clone(),
                file_id: "remote-1".to_owned(),
                data: delta,
                ..FileTransmissionCommand::default()
            })
            .unwrap();
        assert!(engine.is_done());
        assert_eq!(fs::read(&dest).unwrap(), source);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn receiver_writes_simple_file() {
        let dir = temp_dir("recv");
        let opts = TransferOptions {
            direction: Direction::Upload,
            ..TransferOptions::default()
        };
        let mut engine = ReceiveEngine::new(
            &opts,
            vec!["/src/a.txt".to_owned()],
            dir.join("out.txt").to_string_lossy().into_owned(),
        );
        engine
            .handle(FileTransmissionCommand {
                action: Action::Status,
                id: engine.request_id.clone(),
                status: "OK".to_owned(),
                ..FileTransmissionCommand::default()
            })
            .unwrap();
        engine
            .handle(FileTransmissionCommand {
                action: Action::File,
                id: engine.request_id.clone(),
                file_id: "0".to_owned(),
                status: "remote-1".to_owned(),
                name: "/src/a.txt".to_owned(),
                size: 3,
                ..FileTransmissionCommand::default()
            })
            .unwrap();
        let frames = engine
            .handle(FileTransmissionCommand {
                action: Action::Status,
                id: engine.request_id.clone(),
                status: "OK".to_owned(),
                name: "/home/remote".to_owned(),
                ..FileTransmissionCommand::default()
            })
            .unwrap();
        assert!(frames
            .iter()
            .any(|f| String::from_utf8_lossy(f).contains("fid=remote-1")));
        engine
            .handle(FileTransmissionCommand {
                action: Action::Status,
                id: engine.request_id.clone(),
                file_id: "remote-1".to_owned(),
                status: "STARTED".to_owned(),
                ..FileTransmissionCommand::default()
            })
            .unwrap();
        engine
            .handle(FileTransmissionCommand {
                action: Action::EndData,
                id: engine.request_id.clone(),
                file_id: "remote-1".to_owned(),
                data: b"abc".to_vec(),
                ..FileTransmissionCommand::default()
            })
            .unwrap();
        assert!(engine.is_done());
        assert_eq!(fs::read(dir.join("out.txt")).unwrap(), b"abc");
        fs::remove_dir_all(dir).unwrap();
    }
}
