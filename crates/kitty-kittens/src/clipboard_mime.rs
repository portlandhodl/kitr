// SPDX-License-Identifier: GPL-3.0-or-later
//! Clipboard file/MIME mode, ported from `kittens/clipboard`.
//!
//! The legacy text mode uses OSC 52 (see [`crate::clipboard`]). File and
//! multi-MIME transfers use OSC 5522 with metadata fields (`type`, `loc`,
//! `mime`, `pw`, `name`, `status`) and base64 payloads. This module implements
//! the wire encoding, MIME selection/aliases and the file reading/writing
//! helpers used by the CLI.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use kitty_remote::base64::{decode as b64decode, encode as b64encode};

use crate::terminal::InteractiveTransport;

pub const OSC_NUMBER: &str = "5522";
pub const CHUNK_SIZE: usize = 4096;
pub const DEFAULT_WAIT_TIMEOUT: Duration = Duration::from_secs(30);

pub const CLIPBOARD_MIME_USAGE: &str = "\
Usage: kitten clipboard [options] [files to copy to/from]

Options:
  -g, --get-clipboard          Read the clipboard to the given files
  -p, --use-primary            Use the primary selection
  -m, --mime MIME              MIME type for the corresponding file
  -a, --alias FROM=TO          Treat MIME types as equivalent
      --wait-for-completion    Wait until the clipboard write is complete
  -h, --help";

/// Metadata attached to an OSC 5522 packet.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MimeMetadata {
    pub packet_type: String,
    pub location: Option<String>,
    pub mime: Option<String>,
    pub password: Option<String>,
    pub human_name: Option<String>,
    pub status: Option<String>,
}

impl MimeMetadata {
    pub fn encode(&self) -> String {
        let mut out = format!("\x1b]{OSC_NUMBER};");
        let mut first = true;
        let mut add = |key: &str, value: &str| {
            if !first {
                out.push(':');
            }
            first = false;
            out.push_str(key);
            out.push('=');
            if key == "mime" {
                out.push_str(&String::from_utf8_lossy(&b64encode(value.as_bytes(), true)));
            } else {
                out.push_str(value);
            }
        };
        add("type", &self.packet_type);
        if let Some(loc) = &self.location {
            add("loc", loc);
        }
        if let Some(mime) = &self.mime {
            add("mime", mime);
        }
        if let Some(password) = &self.password {
            out.push_str(&format!(
                ":pw={}",
                String::from_utf8_lossy(&b64encode(password.as_bytes(), true))
            ));
        }
        if let Some(name) = &self.human_name {
            out.push_str(&format!(
                ":name={}",
                String::from_utf8_lossy(&b64encode(name.as_bytes(), true))
            ));
        }
        if let Some(status) = &self.status {
            out.push_str(&format!(":status={status}"));
        }
        out
    }

    pub fn payload_frame(&self, payload: &[u8]) -> Vec<u8> {
        let mut text = self.encode();
        if !payload.is_empty() {
            text.push(';');
            text.push_str(&String::from_utf8_lossy(&b64encode(payload, true)));
        }
        text.push_str("\x1b\\");
        text.into_bytes()
    }
}

/// Encode a full OSC 5522 packet.
pub fn encode_mime(metadata: &MimeMetadata, payload: &[u8]) -> Vec<u8> {
    metadata.payload_frame(payload)
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParsedMimePacket {
    pub metadata: MimeMetadata,
    pub payload: Vec<u8>,
}

/// Parse the first OSC 5522 packet in `data`.
pub fn parse_mime_packet(data: &[u8]) -> Result<Option<ParsedMimePacket>, String> {
    let prefix = format!("\x1b]{OSC_NUMBER};").into_bytes();
    let Some(start) = find_subslice(data, &prefix) else {
        return Ok(None);
    };
    let body_start = start + prefix.len();
    let mut end = None;
    let mut terminator = 0usize;
    for (index, byte) in data[body_start..].iter().enumerate() {
        if *byte == 0x07 {
            end = Some(body_start + index);
            terminator = 1;
            break;
        }
        if *byte == 0x1b && data.get(body_start + index + 1) == Some(&b'\\') {
            end = Some(body_start + index);
            terminator = 2;
            break;
        }
    }
    let Some(end) = end else {
        return Ok(None);
    };
    let _ = terminator;
    let text = String::from_utf8_lossy(&data[body_start..end]);
    let (metadata_text, payload_text) = text
        .split_once(';')
        .map_or((text.as_ref(), ""), |(m, p)| (m, p));
    let mut metadata = MimeMetadata::default();
    for field in metadata_text.split(':') {
        let Some((key, value)) = field.split_once('=') else {
            continue;
        };
        match key {
            "type" => metadata.packet_type = value.to_owned(),
            "loc" => metadata.location = Some(value.to_owned()),
            "mime" => {
                metadata.mime =
                    Some(String::from_utf8_lossy(&b64decode(value.as_bytes())).into_owned())
            }
            "pw" => {
                metadata.password =
                    Some(String::from_utf8_lossy(&b64decode(value.as_bytes())).into_owned())
            }
            "name" => {
                metadata.human_name =
                    Some(String::from_utf8_lossy(&b64decode(value.as_bytes())).into_owned())
            }
            "status" => metadata.status = Some(value.to_owned()),
            _ => {}
        }
    }
    let payload = if payload_text.is_empty() {
        Vec::new()
    } else {
        b64decode(payload_text.as_bytes())
    };
    Ok(Some(ParsedMimePacket { metadata, payload }))
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Textual MIME types recognized by kitty, used to add a `text/*` alias.
const TEXTUAL_MIMES: &[&str] = &[
    "application/json",
    "application/javascript",
    "application/xml",
    "application/x-yaml",
    "application/x-shellscript",
    "application/x-perl",
    "application/x-python",
];

pub fn is_textual_mime(mime: &str) -> bool {
    mime.starts_with("text/") || TEXTUAL_MIMES.contains(&mime)
}

/// Guess a MIME type from the file extension. Returns an empty string when
/// unknown, matching the kitten's requirement for `--mime`.
pub fn guess_mime(path: &Path) -> String {
    let ext = path
        .extension()
        .and_then(|x| x.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let mime = match ext.as_str() {
        "txt" | "log" | "conf" | "md" | "rst" => "text/plain",
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "csv" => "text/csv",
        "js" | "mjs" => "application/javascript",
        "json" => "application/json",
        "xml" => "application/xml",
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "bmp" => "image/bmp",
        "tif" | "tiff" => "image/tiff",
        "mp4" => "video/mp4",
        "mkv" => "video/x-matroska",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        "ogg" => "audio/ogg",
        "wav" => "audio/wav",
        "zip" => "application/zip",
        "gz" => "application/gzip",
        "tar" => "application/x-tar",
        "py" => "text/x-python",
        "rs" => "text/x-rust",
        "sh" => "application/x-shellscript",
        _ => "",
    };
    mime.to_owned()
}

/// Parse `--alias from=to` specifications.
pub fn parse_aliases(items: &[String]) -> Result<BTreeMap<String, Vec<String>>, String> {
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for item in items {
        let (from, to) = item
            .split_once('=')
            .ok_or_else(|| format!("Invalid MIME alias: {item:?}"))?;
        out.entry(from.trim().to_owned())
            .or_default()
            .push(to.trim().to_owned());
    }
    Ok(out)
}

/// Choose the MIME type to request for an output file.
pub fn choose_mime(
    requested: &str,
    available: &[String],
    aliases: &BTreeMap<String, Vec<String>>,
) -> Option<String> {
    if requested == "." {
        return Some(".".to_owned());
    }
    if available.iter().any(|mime| mime == requested) {
        return Some(requested.to_owned());
    }
    if let Some(candidates) = aliases.get(requested) {
        for candidate in candidates {
            if available.iter().any(|mime| mime == candidate) {
                return Some(candidate.clone());
            }
        }
    }
    if let Some(found) = available.iter().find(|mime| wildcard_mime(requested, mime)) {
        return Some(found.clone());
    }
    if is_textual_mime(requested) {
        if let Some(found) = available.iter().find(|mime| mime.as_str() == "text/plain") {
            return Some(found.clone());
        }
    }
    None
}

fn wildcard_mime(pattern: &str, mime: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let m: Vec<char> = mime.chars().collect();
    fn inner(p: &[char], m: &[char]) -> bool {
        match p.first() {
            None => m.is_empty(),
            Some('*') => inner(&p[1..], m) || (!m.is_empty() && inner(p, &m[1..])),
            Some('?') => !m.is_empty() && inner(&p[1..], &m[1..]),
            Some(ch) => m.first() == Some(ch) && inner(&p[1..], &m[1..]),
        }
    }
    inner(&p, &m)
}

#[derive(Clone, Debug)]
pub struct MimeInput {
    pub path: PathBuf,
    pub mime: String,
    pub alias_mimes: Vec<String>,
    pub is_stream: bool,
}

impl MimeInput {
    pub fn new(path: PathBuf, mime: Option<String>) -> Result<Self, String> {
        let mime = match mime {
            Some(mime) => mime,
            None if path == Path::new("/dev/stdin") => "text/plain".to_owned(),
            None => guess_mime(&path),
        };
        if mime.is_empty() {
            return Err(format!(
                "Could not guess MIME type for {} use the --mime option",
                path.display()
            ));
        }
        Ok(Self {
            is_stream: path == Path::new("/dev/stdin"),
            path,
            mime,
            alias_mimes: Vec::new(),
        })
    }

    pub fn read_all(&self) -> io::Result<Vec<u8>> {
        if self.is_stream {
            let mut data = Vec::new();
            io::stdin().read_to_end(&mut data)?;
            Ok(data)
        } else {
            fs::read(&self.path)
        }
    }
}

/// Prepare inputs, adding a `text/plain` alias to the first textual input when
/// no input already provides it.
pub fn prepare_inputs(
    mut inputs: Vec<MimeInput>,
    aliases: &BTreeMap<String, Vec<String>>,
) -> Vec<MimeInput> {
    for input in &mut inputs {
        input.alias_mimes = aliases.get(&input.mime).cloned().unwrap_or_default();
    }
    let has_text_plain = inputs.iter().any(|input| {
        input.mime == "text/plain" || input.alias_mimes.iter().any(|mime| mime == "text/plain")
    });
    if !has_text_plain {
        if let Some(input) = inputs.iter_mut().find(|input| is_textual_mime(&input.mime)) {
            input.alias_mimes.push("text/plain".to_owned());
        }
    }
    inputs
}

#[derive(Clone, Debug)]
pub struct MimeOutput {
    pub path: PathBuf,
    pub mime: String,
    pub is_stream: bool,
}

impl MimeOutput {
    pub fn new(path: PathBuf, mime: Option<String>) -> Self {
        let mime = mime.unwrap_or_else(|| guess_mime(&path));
        let is_stream = path == Path::new("/dev/stdout") || path == Path::new("/dev/stderr");
        Self {
            path,
            mime,
            is_stream,
        }
    }

    pub fn write_all(&self, data: &[u8]) -> io::Result<()> {
        if self.path == Path::new("/dev/stdout") {
            io::stdout().write_all(data)
        } else if self.path == Path::new("/dev/stderr") {
            io::stderr().write_all(data)
        } else {
            if let Some(parent) = self.path.parent() {
                if !parent.as_os_str().is_empty() {
                    fs::create_dir_all(parent)?;
                }
            }
            fs::write(&self.path, data)
        }
    }
}

/// Send all inputs to the terminal clipboard with OSC 5522.
pub fn write_mime_inputs(
    inputs: &[MimeInput],
    use_primary: bool,
    password: &str,
    human_name: &str,
    wait: bool,
    transport: &mut dyn InteractiveTransport,
    timeout: Duration,
) -> Result<(), String> {
    let make_metadata = |packet_type: &str, mime: &str| MimeMetadata {
        packet_type: packet_type.to_owned(),
        location: use_primary.then(|| "primary".to_owned()),
        mime: (!mime.is_empty()).then(|| mime.to_owned()),
        password: (packet_type == "write" && !password.is_empty()).then(|| password.to_owned()),
        human_name: (packet_type == "write" && !human_name.is_empty())
            .then(|| human_name.to_owned()),
        status: None,
    };
    transport
        .write_all(&encode_mime(&make_metadata("write", ""), &[]))
        .map_err(|err| err.to_string())?;
    for input in inputs {
        let data = input.read_all().map_err(|err| err.to_string())?;
        for chunk in data.chunks(CHUNK_SIZE) {
            transport
                .write_all(&encode_mime(&make_metadata("wdata", &input.mime), chunk))
                .map_err(|err| err.to_string())?;
        }
        if !input.alias_mimes.is_empty() {
            transport
                .write_all(&encode_mime(
                    &make_metadata("walias", &input.mime),
                    input.alias_mimes.join(" ").as_bytes(),
                ))
                .map_err(|err| err.to_string())?;
        }
    }
    transport
        .write_all(&encode_mime(&make_metadata("wdata", ""), &[]))
        .map_err(|err| err.to_string())?;
    transport.flush().map_err(|err| err.to_string())?;
    if !wait {
        return Ok(());
    }
    let deadline = Instant::now() + timeout;
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match transport.read(&mut chunk, remaining) {
            Ok(0) => break,
            Ok(n) => {
                buffer.extend_from_slice(&chunk[..n]);
                while let Some(packet) = parse_mime_packet(&buffer)? {
                    let prefix = format!("\x1b]{OSC_NUMBER};").into_bytes();
                    if let Some(start) = find_subslice(&buffer, &prefix) {
                        if let Some(end) = buffer[start..].iter().position(|b| *b == 0x1b) {
                            buffer.drain(..start + end + 2);
                        } else {
                            break;
                        }
                    } else {
                        break;
                    }
                    if packet.metadata.packet_type != "write" {
                        continue;
                    }
                    match packet.metadata.status.as_deref() {
                        Some("DONE") => return Ok(()),
                        Some("EIO") => {
                            return Err("Could not write to clipboard: I/O error".to_owned())
                        }
                        Some("EINVAL") => {
                            return Err("Could not write to clipboard: invalid base64".to_owned())
                        }
                        Some("ENOSYS") => {
                            return Err("Could not write to primary selection".to_owned())
                        }
                        Some("EPERM") => {
                            return Err("Could not write to clipboard: permission denied".to_owned())
                        }
                        Some("EBUSY") => {
                            return Err("Could not write to clipboard: try again later".to_owned())
                        }
                        Some("EFBIG") => {
                            return Err("Could not write to clipboard: too much data".to_owned())
                        }
                        Some(status) => {
                            return Err(format!("Could not write to clipboard: {status}"))
                        }
                        None => {}
                    }
                }
            }
            Err(err) if err.kind() == io::ErrorKind::TimedOut => break,
            Err(err) => return Err(err.to_string()),
        }
    }
    Err("timed out waiting for the clipboard write to complete".to_owned())
}

/// Request MIME data from the terminal and write it to the output files.
///
/// This follows kitty's two-phase protocol: first `read` with payload `.`
/// lists the available MIME types, then a second `read` with the chosen types
/// (space separated in the payload) retrieves the data as `status=DATA`
/// packets terminated by `status=DONE`.
pub fn read_mime_outputs(
    outputs: &[MimeOutput],
    use_primary: bool,
    timeout: Duration,
    transport: &mut dyn InteractiveTransport,
) -> Result<(), String> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    let mut available: Vec<String> = Vec::new();
    let mut requested: BTreeMap<String, Vec<MimeOutput>> = BTreeMap::new();
    let mut pending: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut completed = vec![false; outputs.len()];
    let mut phase = 0u8;
    let mut deadline = Instant::now() + timeout;

    let send_read_list =
        |transport: &mut dyn InteractiveTransport, mimes: Option<&str>| -> Result<(), String> {
            let metadata = MimeMetadata {
                packet_type: "read".to_owned(),
                location: use_primary.then(|| "primary".to_owned()),
                ..MimeMetadata::default()
            };
            let payload = mimes.unwrap_or(".").as_bytes();
            transport
                .write_all(&encode_mime(&metadata, payload))
                .map_err(|err| err.to_string())?;
            transport.flush().map_err(|err| err.to_string())
        };
    send_read_list(transport, None)?;

    while Instant::now() < deadline {
        if completed.iter().all(|done| *done) {
            return Ok(());
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        match transport.read(&mut chunk, remaining) {
            Ok(0) => break,
            Ok(n) => {
                buffer.extend_from_slice(&chunk[..n]);
                while let Some(packet) = parse_mime_packet(&buffer)? {
                    let prefix = format!("\x1b]{OSC_NUMBER};").into_bytes();
                    let start = find_subslice(&buffer, &prefix).unwrap_or(0);
                    let end = buffer[start..]
                        .iter()
                        .position(|b| *b == 0x1b)
                        .map(|x| x + 2)
                        .or_else(|| {
                            buffer[start..]
                                .iter()
                                .position(|b| *b == 0x07)
                                .map(|x| x + 1)
                        })
                        .unwrap_or(buffer.len() - start);
                    buffer.drain(..start + end);

                    if packet.metadata.packet_type != "read" {
                        continue;
                    }
                    match packet.metadata.status.as_deref() {
                        Some("DATA") => {
                            if phase == 0 {
                                let list = String::from_utf8_lossy(&packet.payload);
                                available
                                    .extend(list.split_whitespace().map(|mime| mime.to_owned()));
                            } else if let Some(mime) = &packet.metadata.mime {
                                pending
                                    .entry(mime.clone())
                                    .or_default()
                                    .extend_from_slice(&packet.payload);
                            }
                        }
                        Some("DONE") => {
                            if phase == 0 {
                                if available.is_empty() {
                                    return Err("The clipboard is empty".to_owned());
                                }
                                let aliases = BTreeMap::new();
                                for (index, output) in outputs.iter().enumerate() {
                                    match choose_mime(&output.mime, &available, &aliases) {
                                        Some(mime) if mime == "." => {
                                            output
                                                .write_all(available.join("\n").as_bytes())
                                                .map_err(|err| err.to_string())?;
                                            completed[index] = true;
                                        }
                                        Some(mime) => {
                                            requested.entry(mime).or_default().push(output.clone());
                                        }
                                        None => {
                                            return Err(format!(
                                                "The MIME type {} for {} is not available",
                                                output.mime,
                                                output.path.display()
                                            ))
                                        }
                                    }
                                }
                                if requested.is_empty() {
                                    return Ok(());
                                }
                                let list = requested.keys().cloned().collect::<Vec<_>>().join(" ");
                                send_read_list(transport, Some(&list))?;
                                phase = 1;
                                deadline = Instant::now() + timeout;
                            } else {
                                for (mime, outputs_for_mime) in &requested {
                                    let data = pending.remove(mime).unwrap_or_default();
                                    for output in outputs_for_mime {
                                        output.write_all(&data).map_err(|err| err.to_string())?;
                                        if let Some(index) = outputs.iter().position(|candidate| {
                                            candidate.path == output.path
                                                && candidate.mime == output.mime
                                        }) {
                                            completed[index] = true;
                                        }
                                    }
                                }
                                return if completed.iter().all(|done| *done) {
                                    Ok(())
                                } else {
                                    Err("No data for the requested MIME types".to_owned())
                                };
                            }
                        }
                        Some("OK") | None => {}
                        Some(status) => return Err(status.to_owned()),
                    }
                }
            }
            Err(err) if err.kind() == io::ErrorKind::TimedOut => break,
            Err(err) => return Err(err.to_string()),
        }
    }
    Err("timed out or incomplete clipboard read".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::ScriptedTransport;

    #[test]
    fn metadata_packet_roundtrip() {
        let metadata = MimeMetadata {
            packet_type: "wdata".to_owned(),
            location: Some("primary".to_owned()),
            mime: Some("text/plain".to_owned()),
            ..MimeMetadata::default()
        };
        let frame = encode_mime(&metadata, b"hello");
        let parsed = parse_mime_packet(&frame).unwrap().unwrap();
        assert_eq!(parsed.metadata.packet_type, "wdata");
        assert_eq!(parsed.metadata.location.as_deref(), Some("primary"));
        assert_eq!(parsed.metadata.mime.as_deref(), Some("text/plain"));
        assert_eq!(parsed.payload, b"hello");
    }

    #[test]
    fn mime_guess_aliases_and_selection() {
        assert_eq!(guess_mime(Path::new("a.png")), "image/png");
        assert_eq!(guess_mime(Path::new("a.unknown")), "");
        let aliases = parse_aliases(&["text/plain=text/x-rst".to_owned()]).unwrap();
        assert_eq!(
            choose_mime("text/plain", &["text/x-rst".to_owned()], &aliases).as_deref(),
            Some("text/x-rst")
        );
        assert_eq!(
            choose_mime("image/*", &["image/png".to_owned()], &aliases).as_deref(),
            Some("image/png")
        );
        assert_eq!(
            choose_mime(".", &["image/png".to_owned()], &aliases).as_deref(),
            Some(".")
        );
    }

    #[test]
    fn text_plain_alias_added_once() {
        let inputs = vec![MimeInput::new(PathBuf::from("a.py"), None).unwrap()];
        let prepared = prepare_inputs(inputs, &BTreeMap::new());
        assert!(prepared[0].alias_mimes.contains(&"text/plain".to_owned()));
    }

    #[test]
    fn write_waits_for_done() {
        let dir = std::env::temp_dir().join(format!("kitty-clip-{}-1", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.txt");
        fs::write(&path, b"hello").unwrap();
        let input = MimeInput::new(path, None).unwrap();
        let mut transport = ScriptedTransport::new();
        transport.push_reply(
            MimeMetadata {
                packet_type: "write".to_owned(),
                status: Some("DONE".to_owned()),
                ..MimeMetadata::default()
            }
            .payload_frame(&[]),
        );
        write_mime_inputs(
            &[input],
            false,
            "",
            "",
            true,
            &mut transport,
            Duration::from_millis(5),
        )
        .unwrap();
        let text = transport.written_text();
        assert!(text.contains("type=write"));
        assert!(text.contains("type=wdata"));
        assert!(text.contains("aGVsbG8="));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn read_writes_output() {
        let dir = std::env::temp_dir().join(format!("kitty-clip-{}-2", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let out = MimeOutput::new(dir.join("out.txt"), Some("text/plain".to_owned()));
        let mut transport = ScriptedTransport::new();
        let mut reply = MimeMetadata {
            packet_type: "read".to_owned(),
            status: Some("DATA".to_owned()),
            ..MimeMetadata::default()
        }
        .payload_frame(b"text/plain");
        reply.extend_from_slice(
            &MimeMetadata {
                packet_type: "read".to_owned(),
                status: Some("DONE".to_owned()),
                ..MimeMetadata::default()
            }
            .payload_frame(&[]),
        );
        reply.extend_from_slice(
            &MimeMetadata {
                packet_type: "read".to_owned(),
                status: Some("DATA".to_owned()),
                mime: Some("text/plain".to_owned()),
                ..MimeMetadata::default()
            }
            .payload_frame(b"data"),
        );
        reply.extend_from_slice(
            &MimeMetadata {
                packet_type: "read".to_owned(),
                status: Some("DONE".to_owned()),
                ..MimeMetadata::default()
            }
            .payload_frame(&[]),
        );
        transport.push_reply(reply);
        read_mime_outputs(&[out], false, Duration::from_millis(50), &mut transport).unwrap();
        assert_eq!(fs::read(dir.join("out.txt")).unwrap(), b"data");
        let _ = fs::remove_dir_all(dir);
    }
}
