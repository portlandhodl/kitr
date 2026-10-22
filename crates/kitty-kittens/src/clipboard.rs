// SPDX-License-Identifier: GPL-3.0-or-later
//! Rust-native `kitten clipboard`.
//!
//! This is the OSC 52 filter-mode implementation. It deliberately does not try
//! to reproduce kitty's MIME-aware `OSC 5522` file transfer kitten; instead it
//! provides the portable text clipboard path used over SSH and by scripts:
//!
//! * write UTF-8 text from arguments or stdin (`OSC 52 ; c ; <base64>`);
//! * read the clipboard/primary selection and print the decoded bytes;
//! * clear the clipboard/primary selection;
//! * split long payloads into bounded base64 chunks while keeping a single
//!   OSC sequence, as required by kitty's incremental OSC 52 parser.
//!
//! Replies are read through [`InteractiveTransport`], so a terminal that never
//! answers produces a timeout instead of a hung process.

use std::fmt;
use std::io::{self, Read, Write};
use std::time::{Duration, Instant};

use kitty_remote::base64::{decode_strict, encode as encode_base64};

use crate::terminal::{InteractiveTransport, DEFAULT_REPLY_TIMEOUT};

/// Maximum accepted payload size by default (8 MiB of decoded data).
pub const DEFAULT_MAX_SIZE: usize = 8 * 1024 * 1024;

/// Maximum base64 characters per write chunk by default.
pub const DEFAULT_CHUNK_SIZE: usize = 4096;

/// Usage text for `kitten clipboard`.
pub const CLIPBOARD_USAGE: &str = "\
Usage: kitten clipboard [options] [TEXT...]

Read, write or clear the system clipboard through OSC 52.

Options:
  -g, --get-clipboard   Read the selection and write it to STDOUT
      --clear           Clear the selection
  -p, --primary         Use the primary selection instead of the clipboard
  -c, --clipboard       Use the clipboard (the default)
      --windows         Force OSC 52 (accepted for compatibility)
      --stdin           Read the text to copy from STDIN
      --stdout          Write the selection to STDOUT (the default for --get)
      --max-size N      Maximum decoded/encoded payload in bytes (default 8 MiB)
      --chunk-size N    Maximum base64 characters per write chunk (default 4096)
      --timeout SECS    Reply timeout in seconds (default 0.75)
  -h, --help            Show this help

With no TEXT and no --get/--clear, STDIN is copied. Both --primary and
--clipboard may be given to update both selections.";

/// Which selection an OSC 52 operation addresses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipboardTarget {
    Clipboard,
    Primary,
}

impl ClipboardTarget {
    /// The OSC 52 `where` field for this selection.
    pub fn where_field(self) -> &'static str {
        match self {
            Self::Clipboard => "c",
            Self::Primary => "p",
        }
    }

    fn parse_where(value: &str) -> Self {
        if value.split(';').next().is_some_and(|x| x.contains('p')) {
            Self::Primary
        } else {
            Self::Clipboard
        }
    }
}

impl fmt::Display for ClipboardTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Clipboard => "clipboard",
            Self::Primary => "primary selection",
        })
    }
}

/// What the kitten should do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClipboardAction {
    Get,
    Set,
    Clear,
}

/// Parsed clipboard options.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClipboardOptions {
    pub action: ClipboardAction,
    pub targets: Vec<ClipboardTarget>,
    pub text: Vec<String>,
    pub read_stdin: bool,
    pub write_stdout: bool,
    pub max_size: usize,
    pub chunk_size: usize,
    pub timeout: Duration,
    /// Accepted for compatibility. OSC 52 is already the Windows/SSH path in
    /// this implementation.
    pub windows: bool,
    /// Print usage instead of running.
    pub help: bool,
}

impl Default for ClipboardOptions {
    fn default() -> Self {
        Self {
            action: ClipboardAction::Set,
            targets: vec![ClipboardTarget::Clipboard],
            text: Vec::new(),
            read_stdin: false,
            write_stdout: true,
            max_size: DEFAULT_MAX_SIZE,
            chunk_size: DEFAULT_CHUNK_SIZE,
            timeout: DEFAULT_REPLY_TIMEOUT,
            windows: false,
            help: false,
        }
    }
}

/// Errors from the clipboard kitten.
#[derive(Debug)]
#[non_exhaustive]
pub enum ClipboardError {
    Io(io::Error),
    InvalidOption { option: &'static str, value: String },
    TooLarge { size: usize, max: usize },
    InvalidBase64(String),
    InvalidReply(String),
    Timeout(Duration),
    MissingVehicle,
}

impl fmt::Display for ClipboardError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "I/O error: {err}"),
            Self::InvalidOption { option, value } => {
                write!(f, "invalid value for {option}: {value:?}")
            }
            Self::TooLarge { size, max } => {
                write!(f, "clipboard payload is too large ({size} bytes, maximum {max})")
            }
            Self::InvalidBase64(message) => write!(f, "invalid base64 clipboard data: {message}"),
            Self::InvalidReply(message) => write!(f, "invalid OSC 52 reply: {message}"),
            Self::Timeout(duration) => write!(
                f,
                "timed out after {:.3}s waiting for an OSC 52 reply (is this a supported terminal?)",
                duration.as_secs_f64()
            ),
            Self::MissingVehicle => f.write_str("no terminal transport available"),
        }
    }
}

impl std::error::Error for ClipboardError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<io::Error> for ClipboardError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

/// Parse arguments after `kitten clipboard`.
pub fn parse_clipboard_args(args: &[String]) -> Result<ClipboardOptions, ClipboardError> {
    let mut opts = ClipboardOptions::default();
    let mut targets = Vec::new();
    let mut wants_get = false;
    let mut wants_clear = false;
    let mut index = 0usize;
    while index < args.len() {
        let arg = &args[index];
        if arg == "--" {
            opts.text.extend_from_slice(&args[index + 1..]);
            break;
        }
        if !arg.starts_with('-') || arg == "-" {
            opts.text.push(arg.clone());
            index += 1;
            continue;
        }
        let (name, inline) = match arg.split_once('=') {
            Some((name, value)) => (name, Some(value.to_owned())),
            None => (arg.as_str(), None),
        };
        let mut take = |option: &'static str| -> Result<String, ClipboardError> {
            if let Some(value) = &inline {
                return Ok(value.clone());
            }
            index += 1;
            args.get(index)
                .cloned()
                .ok_or_else(|| ClipboardError::InvalidOption {
                    option,
                    value: String::new(),
                })
        };
        match name {
            "-h" | "--help" => opts.help = true,
            "-g" | "--get-clipboard" | "--get" => wants_get = true,
            "--clear" => wants_clear = true,
            "-p" | "--primary" | "--use-primary" => targets.push(ClipboardTarget::Primary),
            "-c" | "--clipboard" => targets.push(ClipboardTarget::Clipboard),
            "--windows" => opts.windows = true,
            "--stdin" => opts.read_stdin = true,
            "--stdout" => opts.write_stdout = true,
            "--max-size" => {
                let value = take("--max-size")?;
                opts.max_size = parse_usize("--max-size", &value)?;
            }
            "--chunk-size" => {
                let value = take("--chunk-size")?;
                opts.chunk_size = parse_usize("--chunk-size", &value)?;
                if opts.chunk_size < 4 {
                    return Err(ClipboardError::InvalidOption {
                        option: "--chunk-size",
                        value,
                    });
                }
            }
            "--timeout" => {
                let value = take("--timeout")?;
                let seconds: f64 = value.parse().map_err(|_| ClipboardError::InvalidOption {
                    option: "--timeout",
                    value: value.clone(),
                })?;
                if !seconds.is_finite() || seconds < 0.0 {
                    return Err(ClipboardError::InvalidOption {
                        option: "--timeout",
                        value,
                    });
                }
                opts.timeout = Duration::from_secs_f64(seconds);
            }
            other => {
                return Err(ClipboardError::InvalidOption {
                    option: "argument",
                    value: other.to_owned(),
                })
            }
        }
        index += 1;
    }
    if wants_get && wants_clear {
        return Err(ClipboardError::InvalidOption {
            option: "argument",
            value: "--get-clipboard cannot be combined with --clear".to_owned(),
        });
    }
    opts.action = if wants_get {
        ClipboardAction::Get
    } else if wants_clear {
        ClipboardAction::Clear
    } else {
        ClipboardAction::Set
    };
    if !targets.is_empty() {
        let mut deduped = Vec::new();
        for target in targets {
            if !deduped.contains(&target) {
                deduped.push(target);
            }
        }
        opts.targets = deduped;
    }
    if opts.action == ClipboardAction::Set
        && opts.text.is_empty()
        && !opts.read_stdin
        && io::stdin().is_terminal()
    {
        // In filter mode this would read from a terminal and block. The caller
        // can still force it with --stdin.
        opts.read_stdin = true;
    }
    Ok(opts)
}

fn parse_usize(option: &'static str, value: &str) -> Result<usize, ClipboardError> {
    value.parse().map_err(|_| ClipboardError::InvalidOption {
        option,
        value: value.to_owned(),
    })
}

/// Split `data` into input-byte chunks whose base64 encodings concatenate to
/// the encoding of the complete input.
///
/// Every returned string except the last is independently aligned to a 3-byte
/// boundary, so no padding appears in the middle of the stream. `chunk_size` is
/// measured in base64 characters.
pub fn base64_chunks(data: &[u8], chunk_size: usize) -> Vec<String> {
    let chunk_size = chunk_size.max(4);
    let input_chunk = (chunk_size / 4) * 3;
    if data.is_empty() {
        return vec![String::new()];
    }
    let mut out = Vec::with_capacity(data.len().div_ceil(input_chunk));
    for chunk in data.chunks(input_chunk.max(3)) {
        let encoded = encode_base64(chunk, false);
        out.push(String::from_utf8(encoded).expect("base64 is ASCII"));
    }
    out
}

/// Encode a complete OSC 52 write (or clear, when `data` is empty).
pub fn encode_osc52_write(
    target: ClipboardTarget,
    data: &[u8],
    chunk_size: usize,
) -> Result<Vec<u8>, ClipboardError> {
    Ok(encode_osc52_write_frames(target, data, chunk_size)?
        .into_iter()
        .flatten()
        .collect())
}

/// Encode an OSC 52 write as a sequence of write frames.
///
/// The frames must be written in order and share one `ESC ] 52 ; ... ESC \`
/// sequence; only the final frame contains the terminator. This is what lets a
/// caller apply backpressure between chunks without every chunk becoming a
/// separate (replacing) clipboard write.
pub fn encode_osc52_write_frames(
    target: ClipboardTarget,
    data: &[u8],
    chunk_size: usize,
) -> Result<Vec<Vec<u8>>, ClipboardError> {
    let mut frames = Vec::new();
    let payload = encode_base64(data, false);
    if payload.is_empty() {
        frames.push(format!("\x1b]52;{};\x1b\\", target.where_field()).into_bytes());
        return Ok(frames);
    }
    let chunk_size = chunk_size.max(4);
    let mut first = format!("\x1b]52;{};", target.where_field()).into_bytes();
    let mut index = 0usize;
    while index < payload.len() {
        let end = (index + chunk_size).min(payload.len());
        if index == 0 {
            first.extend_from_slice(&payload[index..end]);
            frames.push(std::mem::take(&mut first));
        } else {
            frames.push(payload[index..end].to_vec());
        }
        index = end;
    }
    frames
        .last_mut()
        .expect("payload is non-empty")
        .extend_from_slice(b"\x1b\\");
    Ok(frames)
}

/// Encode the OSC 52 query used to read a selection.
pub fn encode_osc52_read_request(target: ClipboardTarget) -> Vec<u8> {
    format!("\x1b]52;{};?\x1b\\", target.where_field()).into_bytes()
}

/// Decode the base64 body of an OSC 52 reply, accepting omitted padding.
pub fn decode_osc52_base64(text: &str) -> Result<Vec<u8>, ClipboardError> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(Vec::new());
    }
    let without_padding = text.trim_end_matches('=');
    if without_padding.is_empty() {
        return Ok(Vec::new());
    }
    let padded = match without_padding.len() % 4 {
        0 => without_padding.to_owned(),
        2 => format!("{without_padding}=="),
        3 => format!("{without_padding}="),
        _ => {
            return Err(ClipboardError::InvalidBase64(
                "length is not a valid base64 group".to_owned(),
            ))
        }
    };
    decode_strict(padded.as_bytes()).map_err(|_| {
        ClipboardError::InvalidBase64("characters outside the base64 alphabet".to_owned())
    })
}

/// Parse one complete OSC 52 reply from `data`.
///
/// Returns `Ok(None)` when no terminator is present yet, allowing callers to
/// continue reading. Both `BEL` and `ESC \` terminators are accepted, as are
/// replies without the leading `ESC ]` (as seen from some multiplexers).
pub fn decode_osc52_reply(data: &[u8]) -> Result<Option<Vec<u8>>, ClipboardError> {
    let mut start = None;
    for index in 0..data.len() {
        if data[index..].starts_with(b"52;")
            && (index == 0 || data[index - 1] == b']' || data[index - 1] == b';')
        {
            start = Some(index);
            break;
        }
    }
    let Some(start) = start else {
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
    let end = match end {
        Some(end) => end,
        None if start == 0 && !data.starts_with(b"\x1b]") => rest.len(),
        None => return Ok(None),
    };
    let body = &rest[..end];
    let Some(separator) = body.iter().position(|byte| *byte == b';') else {
        return Err(ClipboardError::InvalidReply(
            "missing selection separator".to_owned(),
        ));
    };
    let where_field = String::from_utf8_lossy(&body[..separator]);
    let _ = ClipboardTarget::parse_where(&where_field);
    let payload = std::str::from_utf8(&body[separator + 1..])
        .map_err(|_| ClipboardError::InvalidBase64("reply is not ASCII".to_owned()))?;
    Ok(Some(decode_osc52_base64(payload)?))
}

fn read_reply(
    transport: &mut dyn InteractiveTransport,
    timeout: Duration,
    max_size: usize,
) -> Result<Vec<u8>, ClipboardError> {
    let deadline = Instant::now() + timeout;
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match decode_osc52_reply(&buffer) {
            Ok(Some(data)) => {
                if data.len() > max_size {
                    return Err(ClipboardError::TooLarge {
                        size: data.len(),
                        max: max_size,
                    });
                }
                return Ok(data);
            }
            Ok(None) => {}
            Err(err) => return Err(err),
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(ClipboardError::Timeout(timeout));
        }
        match transport.read(&mut chunk, deadline - now) {
            Ok(0) => {
                if buffer.is_empty() {
                    return Err(ClipboardError::InvalidReply(
                        "terminal closed without replying".to_owned(),
                    ));
                }
                return Err(ClipboardError::InvalidReply(
                    "terminal closed mid-reply".to_owned(),
                ));
            }
            Ok(n) => {
                buffer.extend_from_slice(&chunk[..n]);
                if buffer.len() > max_size.saturating_mul(2).max(64 * 1024) {
                    return Err(ClipboardError::TooLarge {
                        size: buffer.len(),
                        max: max_size,
                    });
                }
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) if err.kind() == io::ErrorKind::TimedOut => {
                return Err(ClipboardError::Timeout(timeout))
            }
            Err(err) => return Err(ClipboardError::Io(err)),
        }
    }
}

/// Execute parsed clipboard options.
///
/// `stdin` is only read for `Set` in filter mode. `stdout` receives raw bytes
/// for `Get`.
pub fn run_clipboard(
    opts: &ClipboardOptions,
    transport: &mut dyn InteractiveTransport,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
) -> Result<(), ClipboardError> {
    match opts.action {
        ClipboardAction::Get => {
            let data = run_get(opts, transport)?;
            if opts.write_stdout {
                stdout.write_all(&data)?;
                stdout.flush()?;
            }
            Ok(())
        }
        ClipboardAction::Clear => {
            for target in &opts.targets {
                for frame in encode_osc52_write_frames(*target, &[], opts.chunk_size)? {
                    transport.write_all(&frame)?;
                }
            }
            transport.flush()?;
            Ok(())
        }
        ClipboardAction::Set => {
            let mut data = Vec::new();
            if !opts.text.is_empty() {
                for (index, part) in opts.text.iter().enumerate() {
                    if index > 0 {
                        data.push(b' ');
                    }
                    data.extend_from_slice(part.as_bytes());
                }
            } else {
                let limit = opts.max_size.saturating_add(1) as u64;
                stdin
                    .take(limit)
                    .read_to_end(&mut data)
                    .map_err(ClipboardError::Io)?;
                if data.len() > opts.max_size {
                    return Err(ClipboardError::TooLarge {
                        size: data.len(),
                        max: opts.max_size,
                    });
                }
            }
            if data.is_empty() && opts.text.is_empty() {
                return Err(ClipboardError::InvalidOption {
                    option: "stdin",
                    value: "empty input".to_owned(),
                });
            }
            if data.len() > opts.max_size {
                return Err(ClipboardError::TooLarge {
                    size: data.len(),
                    max: opts.max_size,
                });
            }
            for target in &opts.targets {
                for frame in encode_osc52_write_frames(*target, &data, opts.chunk_size)? {
                    transport.write_all(&frame)?;
                }
            }
            transport.flush()?;
            Ok(())
        }
    }
}

fn run_get(
    opts: &ClipboardOptions,
    transport: &mut dyn InteractiveTransport,
) -> Result<Vec<u8>, ClipboardError> {
    let target = opts
        .targets
        .first()
        .copied()
        .ok_or(ClipboardError::MissingVehicle)?;
    transport.write_all(&encode_osc52_read_request(target))?;
    transport.flush()?;
    read_reply(transport, opts.timeout, opts.max_size)
}

/// Parse and run `kitten clipboard` arguments.
pub fn run_clipboard_cli(
    args: &[String],
    transport: &mut dyn InteractiveTransport,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
) -> Result<(), ClipboardError> {
    let opts = parse_clipboard_args(args)?;
    if opts.help {
        stdout.write_all(CLIPBOARD_USAGE.as_bytes())?;
        stdout.write_all(b"\n")?;
        return Ok(());
    }
    run_clipboard(&opts, transport, stdin, stdout)
}

use std::io::IsTerminal;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::ScriptedTransport;

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|x| (*x).to_owned()).collect()
    }

    #[test]
    fn parses_targets_and_actions() {
        let opts = parse_clipboard_args(&args(&[
            "--primary",
            "--clipboard",
            "-g",
            "--timeout=0.25",
            "--chunk-size",
            "8",
        ]))
        .unwrap();
        assert_eq!(opts.action, ClipboardAction::Get);
        assert_eq!(
            opts.targets,
            vec![ClipboardTarget::Primary, ClipboardTarget::Clipboard]
        );
        assert_eq!(opts.timeout, Duration::from_millis(250));
        assert_eq!(opts.chunk_size, 8);
        assert!(!opts.windows);
        assert!(
            parse_clipboard_args(&args(&["--windows", "--clear"]))
                .unwrap()
                .windows
        );
    }

    #[test]
    fn rejects_conflicting_and_bad_options() {
        assert!(matches!(
            parse_clipboard_args(&args(&["--get", "--clear"])),
            Err(ClipboardError::InvalidOption { .. })
        ));
        assert!(matches!(
            parse_clipboard_args(&args(&["--chunk-size", "3"])),
            Err(ClipboardError::InvalidOption { .. })
        ));
        assert!(matches!(
            parse_clipboard_args(&args(&["--timeout", "nope"])),
            Err(ClipboardError::InvalidOption { .. })
        ));
        assert!(matches!(
            parse_clipboard_args(&args(&["--what"])),
            Err(ClipboardError::InvalidOption { .. })
        ));
    }

    #[test]
    fn write_encoding_uses_osc52_and_omits_padding() {
        let encoded = encode_osc52_write(ClipboardTarget::Clipboard, b"hello", 4096).unwrap();
        assert_eq!(encoded, b"\x1b]52;c;aGVsbG8\x1b\\");
        let primary = encode_osc52_write(ClipboardTarget::Primary, b"hi", 4096).unwrap();
        assert_eq!(primary, b"\x1b]52;p;aGk\x1b\\");
    }

    #[test]
    fn base64_chunks_concatenate_and_decode() {
        let data = b"the quick brown fox jumps over the lazy dog";
        let chunks = base64_chunks(data, 8);
        assert!(chunks.iter().all(|chunk| chunk.len() <= 8));
        let joined: String = chunks.concat();
        let decoded = decode_osc52_base64(&joined).unwrap();
        assert_eq!(decoded, data);
        assert_eq!(base64_chunks(b"", 8), vec![String::new()]);
    }

    #[test]
    fn chunked_frames_form_one_osc_sequence() {
        let data = vec![b'x'; 100];
        let frames = encode_osc52_write_frames(ClipboardTarget::Clipboard, &data, 8).unwrap();
        assert!(frames.len() > 2);
        assert!(frames[0].starts_with(b"\x1b]52;c;"));
        assert!(frames[0].len() <= 8 + b"\x1b]52;c;".len());
        assert!(frames
            .iter()
            .skip(1)
            .all(|frame| !frame.starts_with(b"\x1b]")));
        assert!(frames.last().unwrap().ends_with(b"\x1b\\"));
        let whole: Vec<u8> = frames.concat();
        let decoded = decode_osc52_reply(&whole).unwrap().unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn clear_is_an_empty_osc52_payload() {
        let encoded = encode_osc52_write(ClipboardTarget::Primary, b"", 10).unwrap();
        assert_eq!(encoded, b"\x1b]52;p;\x1b\\");
        assert!(decode_osc52_reply(&encoded).unwrap().unwrap().is_empty());
    }

    #[test]
    fn decodes_bel_st_and_unpadded_replies() {
        assert_eq!(
            decode_osc52_reply(b"\x1b]52;c;aGVsbG8=\x07")
                .unwrap()
                .unwrap(),
            b"hello"
        );
        assert_eq!(
            decode_osc52_reply(b"52;c;aGVsbG8").unwrap().unwrap(),
            b"hello"
        );
        assert!(decode_osc52_reply(b"\x1b]52;c;aGVsbG8=").unwrap().is_none());
        assert!(decode_osc52_reply(b"\x1b]52;c;!!!!\x1b\\").is_err());
        assert!(decode_osc52_reply(b"\x1b]52;c;A\x1b\\").is_err());
    }

    #[test]
    fn get_writes_request_and_decodes_reply() {
        let mut transport = ScriptedTransport::new();
        transport.push_reply(b"\x1b]52;p;cHJpbWFyeQ==\x1b\\".to_vec());
        let opts = ClipboardOptions {
            action: ClipboardAction::Get,
            targets: vec![ClipboardTarget::Primary],
            timeout: Duration::from_millis(10),
            ..ClipboardOptions::default()
        };
        let mut out = Vec::new();
        run_clipboard(&opts, &mut transport, &mut io::empty(), &mut out).unwrap();
        assert_eq!(out, b"primary");
        assert_eq!(transport.written(), b"\x1b]52;p;?\x1b\\");
    }

    #[test]
    fn get_timeout_is_reported_without_hanging() {
        let mut transport = ScriptedTransport::new();
        transport.set_timeout(true);
        let opts = ClipboardOptions {
            action: ClipboardAction::Get,
            timeout: Duration::from_millis(1),
            ..ClipboardOptions::default()
        };
        let err =
            run_clipboard(&opts, &mut transport, &mut io::empty(), &mut Vec::new()).unwrap_err();
        assert!(matches!(err, ClipboardError::Timeout(_)));
    }

    #[test]
    fn set_reads_stdin_and_respects_max_size() {
        let mut transport = ScriptedTransport::new();
        let opts = ClipboardOptions {
            max_size: 5,
            chunk_size: 100,
            ..ClipboardOptions::default()
        };
        run_clipboard(&opts, &mut transport, &mut &b"hello"[..], &mut Vec::new()).unwrap();
        assert_eq!(transport.written(), b"\x1b]52;c;aGVsbG8\x1b\\");
        let err = run_clipboard(&opts, &mut transport, &mut &b"toolong"[..], &mut Vec::new())
            .unwrap_err();
        assert!(matches!(err, ClipboardError::TooLarge { .. }));
    }

    #[test]
    fn set_text_and_both_targets() {
        let mut transport = ScriptedTransport::new();
        let opts = ClipboardOptions {
            targets: vec![ClipboardTarget::Clipboard, ClipboardTarget::Primary],
            text: vec!["hello".to_owned(), "world".to_owned()],
            ..ClipboardOptions::default()
        };
        run_clipboard(&opts, &mut transport, &mut io::empty(), &mut Vec::new()).unwrap();
        let written = transport.written();
        assert_eq!(
            written,
            b"\x1b]52;c;aGVsbG8gd29ybGQ\x1b\\\x1b]52;p;aGVsbG8gd29ybGQ\x1b\\"
        );
    }

    #[test]
    fn clear_sends_empty_payload() {
        let mut transport = ScriptedTransport::new();
        let opts = ClipboardOptions {
            action: ClipboardAction::Clear,
            ..ClipboardOptions::default()
        };
        run_clipboard(&opts, &mut transport, &mut io::empty(), &mut Vec::new()).unwrap();
        assert_eq!(transport.written(), b"\x1b]52;c;\x1b\\");
    }
}
