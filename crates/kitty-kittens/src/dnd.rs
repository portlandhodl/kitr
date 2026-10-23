// SPDX-License-Identifier: GPL-3.0-or-later
//! Drag-and-drop kitten, ported from `kittens/dnd`.
//!
//! Implements the tty DnD protocol (OSC 72) command encoding/decoding, drop
//! destination decoding, URI-list/data-URI parsing and local copy/move with
//! hard-link optimization. Image thumbnails and remote tree streaming require
//! the graphics protocol and are represented but not rendered here.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use kitty_remote::base64::{decode as b64decode, encode as b64encode};

use crate::tui::{self, Key, KeyParser, Tty};

pub const DND_CODE: u16 = 72;
pub const DND_USAGE: &str = "\
Usage: kitten dnd [options] [files to drag]

Perform drag and drop operations over the tty.

Options:
      --drag MIME:PATH
      --drop MIME[:PATH]
      --drop-dest DIR
      --copy-mode auto|independent
      --confirm-drop-overwrite
      --drop-anywhere disallowed|copy|move
      --drag-thumbnail PATH
      --drag-thumbnail-size N
      --exit-on EVENT[,EVENT...]
  -h, --help";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DndCommand {
    pub kind: u8,
    pub has_more: bool,
    pub operation: i32,
    pub x: i32,
    pub y: i32,
    pub xp: i32,
    pub yp: i32,
    pub payload: Vec<u8>,
}

impl DndCommand {
    pub fn new(kind: u8) -> Self {
        Self {
            kind,
            ..Self::default()
        }
    }

    /// Encode one OSC 72 command, chunking base64 payloads as kitty does.
    pub fn encode(&self) -> Vec<Vec<u8>> {
        let mut header = format!("\x1b]{DND_CODE};t={}", self.kind as char);
        for (key, value) in [
            ('o', self.operation),
            ('x', self.x),
            ('y', self.y),
            ('X', self.xp),
            ('Y', self.yp),
        ] {
            if value != 0 {
                header.push_str(&format!(":{key}={value}"));
            }
        }
        if self.payload.is_empty() {
            header.push_str("\x1b\\");
            return vec![header.into_bytes()];
        }
        let payload = if matches!(self.kind, b'r' | b'p') {
            String::from_utf8_lossy(&b64encode(&self.payload, false)).into_owned()
        } else {
            String::from_utf8_lossy(&self.payload).into_owned()
        };
        let mut frames = Vec::new();
        for (index, chunk) in payload.as_bytes().chunks(4096).enumerate() {
            let last = index + 4096 >= payload.len();
            if index == 0 {
                frames.push(format!("{header}:m={};\x1b\\", if last { 0 } else { 1 }).into_bytes());
                continue;
            }
            frames.push(
                format!("\x1b]{DND_CODE};m={};\x1b\\", if last { 0 } else { 1 }).into_bytes(),
            );
            // Kitty writes the payload between the metadata and terminator;
            // combine it here so callers just write each frame.
            let metadata_len = frames.last().map(Vec::len).unwrap_or(0);
            let frame = frames.last_mut().expect("just pushed");
            frame.truncate(metadata_len.saturating_sub(2));
            frame.extend_from_slice(chunk);
            frame.extend_from_slice(b"\x1b\\");
        }
        // The first frame above includes the terminator before the payload;
        // rebuild it correctly.
        if frames.len() == 1 {
            let frame = &mut frames[0];
            let text = String::from_utf8_lossy(frame).into_owned();
            let cleaned = text.replace("\x1b\\", "");
            *frame = format!(
                "{cleaned}{}\x1b\\",
                String::from_utf8_lossy(payload.as_bytes())
            )
            .into_bytes();
        }
        frames
    }
}

#[derive(Clone, Debug, Default)]
pub struct DndAccumulator {
    metadata: Option<DndCommand>,
    payload: Vec<u8>,
}

impl DndAccumulator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a parsed command. Returns a complete command once `m=0`.
    pub fn push(&mut self, mut command: DndCommand) -> Option<DndCommand> {
        if self.metadata.is_none() {
            self.metadata = Some(command.clone());
            self.payload.clear();
        }
        self.payload.extend_from_slice(&command.payload);
        if command.has_more {
            command.payload.clear();
            return None;
        }
        let mut complete = self.metadata.take().unwrap_or_default();
        let payload = std::mem::take(&mut self.payload);
        complete.payload = if matches!(complete.kind, b'r' | b'p') {
            b64decode(&payload)
        } else {
            payload
        };
        complete.has_more = false;
        Some(complete)
    }
}

/// Parse one OSC 72 body (the part after `72;`).
pub fn parse_dnd_body(body: &[u8]) -> Result<DndCommand, String> {
    let text = String::from_utf8_lossy(body);
    let (controls, payload) = text
        .split_once(';')
        .map_or((text.as_ref(), ""), |(a, b)| (a, b));
    let mut command = DndCommand::default();
    for field in controls.split(':') {
        let Some((key, value)) = field.split_once('=') else {
            continue;
        };
        let key = key.as_bytes().first().copied().unwrap_or(0);
        let number = value.parse::<i32>().unwrap_or(0);
        match key {
            b't' => command.kind = value.as_bytes().first().copied().unwrap_or(0),
            b'm' => command.has_more = value == "1",
            b'o' => command.operation = number,
            b'x' => command.x = number,
            b'y' => command.y = number,
            b'X' => command.xp = number,
            b'Y' => command.yp = number,
            _ => {}
        }
    }
    command.payload = payload.as_bytes().to_vec();
    if command.kind == 0 {
        return Err("DnD command has no type".to_owned());
    }
    Ok(command)
}

/// Extract complete OSC 72 bodies from a byte stream.
pub fn parse_dnd_frames(data: &[u8]) -> Vec<DndCommand> {
    let prefix = format!("\x1b]{DND_CODE};").into_bytes();
    let mut out = Vec::new();
    let mut index = 0usize;
    while index < data.len() {
        let Some(start) = find(&data[index..], &prefix) else {
            break;
        };
        let start = index + start + prefix.len();
        let Some(end_rel) = data[start..].windows(2).position(|w| w == b"\x1b\\") else {
            break;
        };
        let end = start + end_rel;
        if let Ok(command) = parse_dnd_body(&data[start..end]) {
            out.push(command);
        }
        index = end + 2;
    }
    out
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DragSource {
    pub mime: String,
    pub path: Option<PathBuf>,
    pub data: Vec<u8>,
    pub human_name: String,
}

impl DragSource {
    pub fn from_spec(spec: &str) -> Result<Self, String> {
        let (mime, source) = spec
            .split_once(':')
            .ok_or_else(|| format!("invalid drag source {spec}, must be MIME:PATH"))?;
        if source.is_empty() {
            return Ok(Self {
                mime: mime.to_owned(),
                ..Self::default()
            });
        }
        if source == "-" || source == "/dev/stdin" {
            let mut data = Vec::new();
            io::stdin()
                .read_to_end(&mut data)
                .map_err(|err| err.to_string())?;
            return Ok(Self {
                mime: mime.to_owned(),
                data,
                human_name: "STDIN".to_owned(),
                ..Self::default()
            });
        }
        Ok(Self {
            mime: mime.to_owned(),
            path: Some(expand_tilde(source)),
            human_name: source.to_owned(),
            ..Self::default()
        })
    }

    pub fn data(&self) -> io::Result<Vec<u8>> {
        if !self.data.is_empty() {
            return Ok(self.data.clone());
        }
        match &self.path {
            Some(path) => fs::read(path),
            None => Ok(Vec::new()),
        }
    }
}

fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(path)
}

#[derive(Clone, Debug)]
pub struct DropDestination {
    pub mime: String,
    pub path: Option<PathBuf>,
    pub data: Vec<u8>,
    pub completed: bool,
}

impl DropDestination {
    pub fn new(mime: &str, path: Option<PathBuf>) -> Self {
        Self {
            mime: mime.to_owned(),
            path,
            data: Vec::new(),
            completed: false,
        }
    }

    pub fn add_base64(&mut self, chunk: &[u8]) -> Result<(), String> {
        let decoded = b64decode(chunk);
        self.data.extend_from_slice(&decoded);
        Ok(())
    }

    pub fn finish(&mut self) -> io::Result<()> {
        self.completed = true;
        if let Some(path) = &self.path {
            if let Some(parent) = path.parent() {
                if !parent.as_os_str().is_empty() {
                    fs::create_dir_all(parent)?;
                }
            }
            fs::write(path, &self.data)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParsedUri {
    Path(PathBuf),
    Data { mime: String, data: Vec<u8> },
}

/// Parse a `text/uri-list` payload.
pub fn parse_uri_list(text: &str) -> Vec<ParsedUri> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("file://") {
            let path = if let Some(pos) = rest.find('/') {
                &rest[pos..]
            } else {
                rest
            };
            out.push(ParsedUri::Path(percent_decode(path)));
        } else if let Some(rest) = line.strip_prefix("data:") {
            let (meta, payload) = rest.split_once(',').unwrap_or((rest, ""));
            let mime = meta.split(';').next().unwrap_or("").to_owned();
            let data = if meta.contains(";base64") {
                b64decode(payload.as_bytes())
            } else {
                percent_decode(payload)
                    .to_string_lossy()
                    .as_bytes()
                    .to_vec()
            };
            out.push(ParsedUri::Data { mime, data });
        } else {
            out.push(ParsedUri::Path(PathBuf::from(line)));
        }
    }
    out
}

fn percent_decode(text: &str) -> PathBuf {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let Ok(value) = u8::from_str_radix(&text[index + 1..index + 3], 16) {
                out.push(value);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    PathBuf::from(String::from_utf8_lossy(&out).into_owned())
}

pub fn uniqify_child_names(mut names: Vec<String>, case_sensitive: bool) -> Vec<String> {
    if case_sensitive {
        return names;
    }
    let mut seen = std::collections::HashSet::new();
    for name in &mut names {
        let mut candidate = name.clone();
        let mut counter = 1;
        while !seen.insert(candidate.to_lowercase()) {
            candidate = format!("case-conflict-{counter}-{name}");
            counter += 1;
        }
        *name = candidate;
    }
    names
}

pub fn find_overwrites(source: &Path, destination: &Path) -> io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let name = entry.file_name();
        let target = destination.join(&name);
        if target.exists() {
            out.push(target);
        }
    }
    Ok(out)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DropAction {
    #[default]
    Copy,
    Move,
}

/// Copy or move local files into `dest_dir`, using hard links for copy when
/// allowed and falling back to a recursive copy.
pub fn copy_uris(
    uris: &[PathBuf],
    dest_dir: &Path,
    action: DropAction,
    copy_mode: &str,
) -> io::Result<usize> {
    fs::create_dir_all(dest_dir)?;
    let disallow_hardlinks = action == DropAction::Copy && copy_mode == "independent";
    let mut count = 0usize;
    for source in uris {
        let Some(name) = source.file_name() else {
            continue;
        };
        let dest = dest_dir.join(name);
        if source.is_dir() {
            copy_dir(source, &dest)?;
        } else if source.is_file() {
            if action == DropAction::Move {
                fs::rename(source, &dest).or_else(|_| -> io::Result<()> {
                    if !disallow_hardlinks && fs::hard_link(source, &dest).is_ok() {
                        fs::remove_file(source)?;
                        Ok(())
                    } else {
                        fs::copy(source, &dest)?;
                        fs::remove_file(source)?;
                        Ok(())
                    }
                })?;
            } else if !disallow_hardlinks && fs::hard_link(source, &dest).is_ok() {
                // linked
            } else {
                fs::copy(source, &dest)?;
            }
        } else if source.is_symlink() {
            let target = fs::read_link(source)?;
            let _ = fs::remove_file(&dest);
            std::os::unix::fs::symlink(target, &dest)?;
        }
        count += 1;
    }
    Ok(count)
}

fn copy_dir(source: &Path, dest: &Path) -> io::Result<()> {
    fs::create_dir_all(dest)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let target = dest.join(entry.file_name());
        let path = entry.path();
        if path.is_symlink() {
            let link = fs::read_link(&path)?;
            let _ = fs::remove_file(&target);
            std::os::unix::fs::symlink(link, target)?;
        } else if path.is_dir() {
            copy_dir(&path, &target)?;
        } else {
            fs::copy(&path, &target)?;
        }
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub struct DndOptions {
    pub drag: Vec<DragSource>,
    pub drop: BTreeMap<String, Option<PathBuf>>,
    pub drop_dest: Option<PathBuf>,
    pub copy_mode: String,
    pub confirm_overwrite: bool,
    pub drop_anywhere: String,
    pub drag_thumbnail: Option<PathBuf>,
    pub drag_thumbnail_size: u32,
    pub exit_on: Vec<String>,
    pub help: bool,
}

impl Default for DndOptions {
    fn default() -> Self {
        Self {
            drag: Vec::new(),
            drop: BTreeMap::new(),
            drop_dest: None,
            copy_mode: "auto".to_owned(),
            confirm_overwrite: false,
            drop_anywhere: "disallowed".to_owned(),
            drag_thumbnail: None,
            drag_thumbnail_size: 512,
            exit_on: vec!["esc-key".to_owned()],
            help: false,
        }
    }
}

impl DndOptions {
    pub fn has_exit_on(&self, event: &str) -> bool {
        self.exit_on.iter().any(|item| item == event)
    }
}

pub fn parse_dnd_args(args: &[String]) -> Result<DndOptions, String> {
    let mut opts = DndOptions::default();
    let mut index = 0usize;
    let mut positional = Vec::new();
    while index < args.len() {
        let arg = &args[index];
        let (name, inline) = arg
            .split_once('=')
            .map_or((arg.as_str(), None), |(a, b)| (a, Some(b.to_owned())));
        let mut take = || -> String {
            if let Some(value) = inline.clone() {
                return value;
            }
            index += 1;
            args.get(index).cloned().unwrap_or_default()
        };
        match name {
            "-h" | "--help" => opts.help = true,
            "--drag" => opts.drag.push(DragSource::from_spec(&take())?),
            "--drop" => {
                let spec = take();
                let (mime, path) = spec
                    .split_once(':')
                    .map_or((spec.as_str(), ""), |(mime, path)| (mime, path));
                opts.drop.insert(
                    mime.to_owned(),
                    if path.is_empty() {
                        None
                    } else {
                        Some(expand_tilde(path))
                    },
                );
            }
            "--drop-dest" => opts.drop_dest = Some(expand_tilde(&take())),
            "--copy-mode" => opts.copy_mode = take(),
            "--confirm-drop-overwrite" => opts.confirm_overwrite = true,
            "--drop-anywhere" => opts.drop_anywhere = take(),
            "--drag-thumbnail" => opts.drag_thumbnail = Some(expand_tilde(&take())),
            "--drag-thumbnail-size" => {
                opts.drag_thumbnail_size = take()
                    .parse()
                    .map_err(|_| "invalid drag-thumbnail-size".to_owned())?
            }
            "--exit-on" => {
                opts.exit_on = take()
                    .split(',')
                    .map(str::trim)
                    .map(str::to_owned)
                    .collect()
            }
            other if !other.starts_with('-') => positional.push(other.to_owned()),
            other => return Err(format!("unknown option: {other}")),
        }
        index += 1;
    }
    if !positional.is_empty() {
        let uris = positional.iter().map(PathBuf::from).collect::<Vec<_>>();
        let text = uris
            .iter()
            .map(|path| {
                format!(
                    "file://{}",
                    path.canonicalize()
                        .unwrap_or_else(|_| path.clone())
                        .to_string_lossy()
                )
            })
            .collect::<Vec<_>>()
            .join("\r\n");
        opts.drag.push(DragSource {
            mime: "text/uri-list".to_owned(),
            data: text.into_bytes(),
            human_name: "Files".to_owned(),
            path: None,
        });
    }
    if opts.drop.is_empty() {
        opts.drop
            .insert("text/uri-list".to_owned(), Some(PathBuf::new()));
    }
    Ok(opts)
}

fn new_machine_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{:x}", nanos)
}

/// Build the setup frames for accepting drops and offering drags.
pub fn setup_frames(opts: &DndOptions) -> Vec<Vec<u8>> {
    let mut frames = Vec::new();
    let mut accept = DndCommand::new(b'a');
    accept.payload = opts
        .drop
        .keys()
        .cloned()
        .collect::<Vec<_>>()
        .join(" ")
        .into_bytes();
    frames.extend(accept.encode());
    let mut machine = DndCommand::new(b'a');
    machine.x = 1;
    machine.payload = new_machine_id().into_bytes();
    frames.extend(machine.encode());
    if !opts.drag.is_empty() {
        let mut offer = DndCommand::new(b'o');
        offer.x = 1;
        offer.payload = new_machine_id().into_bytes();
        frames.extend(offer.encode());
        for (index, source) in opts.drag.iter().enumerate() {
            if let Ok(data) = source.data() {
                if data.len() < 64 * 1024 * 1024 {
                    let mut preload = DndCommand::new(b'p');
                    preload.x = index as i32;
                    preload.payload = data;
                    frames.extend(preload.encode());
                }
            }
        }
    }
    frames
}

pub fn teardown_frames(opts: &DndOptions) -> Vec<Vec<u8>> {
    let mut frames = Vec::new();
    if !opts.drop.is_empty() {
        frames.extend(DndCommand::new(b'A').encode());
    }
    if !opts.drag.is_empty() {
        let mut stop = DndCommand::new(b'o');
        stop.x = 2;
        frames.extend(stop.encode());
    }
    frames
}

/// Run a minimal DnD UI: set up the protocol, handle drop data and quit on the
/// configured key. This is enough to exercise the engine on a real tty.
pub fn run_dnd_ui(tty: &mut dyn Tty, opts: &DndOptions) -> io::Result<()> {
    let mut parser = KeyParser::new();
    for frame in setup_frames(opts) {
        tty.write_all(&frame)?;
    }
    tty.write_all(b"Drag and drop\r\nEsc to quit\r\n")?;
    tty.flush()?;
    let mut accumulator = DndAccumulator::new();
    let mut dest = DropDestination::new(
        "text/plain",
        Some(
            opts.drop_dest
                .clone()
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_default())
                .join("drop-data"),
        ),
    );
    loop {
        let key = match tui::read_key(tty, &mut parser, Duration::from_millis(500)) {
            Ok(key) => key,
            Err(_) => continue,
        };
        if let Some(Key::Esc) = key {
            if opts.has_exit_on("esc-key") {
                break;
            }
        }
        if let Some(Key::Ctrl('c')) = key {
            break;
        }
        let mut buf = [0u8; 4096];
        if let Ok(n) = tty.read(&mut buf, Duration::ZERO) {
            if n > 0 {
                for command in parse_dnd_frames(&buf[..n]) {
                    if let Some(command) = accumulator.push(command) {
                        match command.kind {
                            b'r' => {
                                dest.data.extend_from_slice(&command.payload);
                                let _ = dest.finish();
                            }
                            b'm' | b'M' => {
                                let text = String::from_utf8_lossy(&command.payload);
                                let uris: Vec<PathBuf> = parse_uri_list(&text)
                                    .into_iter()
                                    .filter_map(|uri| match uri {
                                        ParsedUri::Path(path) => Some(path),
                                        _ => None,
                                    })
                                    .collect();
                                let target = opts
                                    .drop_dest
                                    .clone()
                                    .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
                                let _ = copy_uris(
                                    &uris,
                                    &target,
                                    if command.operation == 2 {
                                        DropAction::Move
                                    } else {
                                        DropAction::Copy
                                    },
                                    &opts.copy_mode,
                                );
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
    }
    for frame in teardown_frames(opts) {
        tty.write_all(&frame)?;
    }
    tty.flush()
}

pub fn run_dnd_cli(
    args: &[String],
    tty: Option<&mut dyn Tty>,
    out: &mut dyn Write,
) -> Result<(), String> {
    let opts = parse_dnd_args(args)?;
    if opts.help {
        writeln!(out, "{DND_USAGE}").map_err(|err| err.to_string())?;
        return Ok(());
    }
    let Some(tty) = tty else {
        return Err("The DnD kitten requires a terminal".to_owned());
    };
    run_dnd_ui(tty, &opts).map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::FakeTty;

    #[test]
    fn command_encoding_and_parsing() {
        let mut command = DndCommand::new(b'o');
        command.operation = 3;
        command.x = 1;
        command.payload = b"text/plain".to_vec();
        let frames = command.encode();
        let joined: Vec<u8> = frames.into_iter().flatten().collect();
        assert!(String::from_utf8_lossy(&joined).contains("t=o"));
        let parsed = parse_dnd_frames(&joined);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].kind, b'o');
        assert_eq!(parsed[0].operation, 3);
        assert_eq!(parsed[0].payload, b"text/plain");
    }

    #[test]
    fn chunk_accumulator() {
        let mut acc = DndAccumulator::new();
        let mut first = DndCommand::new(b'r');
        first.has_more = true;
        first.payload = b64encode(b"hello ", false);
        assert!(acc.push(first).is_none());
        let mut last = DndCommand::new(b'r');
        last.payload = b64encode(b"world", false);
        let complete = acc.push(last).unwrap();
        assert_eq!(complete.payload, b"hello world");
    }

    #[test]
    fn uri_list_and_percent_decoding() {
        let uris = parse_uri_list("file:///tmp/a%20b\n# comment\ndata:text/plain;base64,aGk=\n");
        assert_eq!(uris[0], ParsedUri::Path(PathBuf::from("/tmp/a b")));
        assert_eq!(
            uris[1],
            ParsedUri::Data {
                mime: "text/plain".to_owned(),
                data: b"hi".to_vec()
            }
        );
    }

    #[test]
    fn copy_and_move_files() {
        let dir = std::env::temp_dir().join(format!("kitty-dnd-{}-1", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let source = dir.join("source");
        let dest = dir.join("dest");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("a.txt"), b"hello").unwrap();
        let count = copy_uris(&[source.join("a.txt")], &dest, DropAction::Copy, "auto").unwrap();
        assert_eq!(count, 1);
        assert_eq!(fs::read(dest.join("a.txt")).unwrap(), b"hello");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn setup_and_teardown_frames() {
        let mut opts = DndOptions::default();
        opts.drag.push(DragSource {
            mime: "text/plain".to_owned(),
            data: b"hello".to_vec(),
            ..DragSource::default()
        });
        let setup = setup_frames(&opts);
        assert!(!setup.is_empty());
        let teardown = teardown_frames(&opts);
        assert!(!teardown.is_empty());
        let text: String = setup.iter().flatten().map(|byte| *byte as char).collect();
        assert!(text.contains("t=a"));
        assert!(text.contains("t=o"));
    }

    #[test]
    fn ui_setup_and_quit() {
        let opts = DndOptions::default();
        let mut tty = FakeTty::new(80, 24, b"\x1b");
        run_dnd_ui(&mut tty, &opts).unwrap();
        assert!(tty.output_text().contains("Drag and drop"));
        assert!(tty.output_text().contains("72;"));
    }

    #[test]
    fn parses_drag_and_drop_specs() {
        let args = ["--drop=text/plain", "/tmp/file"]
            .iter()
            .map(|x| (*x).to_owned())
            .collect::<Vec<_>>();
        let opts = parse_dnd_args(&args).unwrap();
        assert!(opts.drop.contains_key("text/plain"));
        assert_eq!(opts.drag[0].mime, "text/uri-list");
    }
}
