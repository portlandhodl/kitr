// SPDX-License-Identifier: GPL-3.0-or-later
//! Query-terminal kitten, ported from `kittens/query_terminal`.
//!
//! Queries use DCS `+q<hex-name>` requests and `0|1+r<hex-name>=<hex-value>`
//! responses, exactly as kitty's `query_terminal` and screen code do. The
//! terminal-side values this port can answer are held in [`TerminalState`];
//! a GUI can populate the font/color fields and use [`answer_query`].

use std::collections::BTreeMap;
use std::io::Write;
use std::time::{Duration, Instant};

use crate::terminal::InteractiveTransport;

pub const QUERY_USAGE: &str = "\
Usage: kitten query-terminal [--wait-for SECONDS] [query1 query2 ...]

Queries: name version font_family bold_font italic_font bold_italic_font
         font_size dpi_x dpi_y foreground background background_opacity
         clipboard_control os_name allow_hyperlinks";

pub const QUERY_NAMES: &[&str] = &[
    "name",
    "version",
    "font_family",
    "bold_font",
    "italic_font",
    "bold_italic_font",
    "font_size",
    "dpi_x",
    "dpi_y",
    "foreground",
    "background",
    "background_opacity",
    "clipboard_control",
    "os_name",
    "allow_hyperlinks",
];

pub const QUERY_HELP: &[(&str, &str)] = &[
    ("name", "Terminal name (e.g. xterm-kitty)"),
    ("version", "Terminal version"),
    ("font_family", "The current font's PostScript name"),
    ("bold_font", "The current bold font's PostScript name"),
    ("italic_font", "The current italic font's PostScript name"),
    (
        "bold_italic_font",
        "The current bold-italic font's PostScript name",
    ),
    ("font_size", "The current font size in points"),
    ("dpi_x", "The current DPI on the x-axis"),
    ("dpi_y", "The current DPI on the y-axis"),
    ("foreground", "The current foreground color as #RRGGBB"),
    ("background", "The current background color as #RRGGBB"),
    (
        "background_opacity",
        "The current background opacity (0..1)",
    ),
    ("clipboard_control", "The clipboard_control configuration"),
    ("os_name", "The name of the operating system"),
    ("allow_hyperlinks", "yes, no or ask"),
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Query {
    pub name: &'static str,
    pub query_name: String,
    pub encoded_name: String,
    pub help: &'static str,
}

impl Query {
    pub fn new(name: &'static str) -> Option<Self> {
        let help = QUERY_HELP
            .iter()
            .find(|(candidate, _)| *candidate == name)
            .map(|(_, help)| *help)?;
        let query_name = if name == "name" {
            "name".to_owned()
        } else {
            format!("kitty-query-{name}")
        };
        let encoded_name = hex_encode(query_name.as_bytes());
        Some(Self {
            name,
            query_name,
            encoded_name,
            help,
        })
    }

    pub fn escape_code(&self) -> String {
        format!("\x1bP+q{}\x1b\\", self.encoded_name)
    }
}

pub fn all_queries() -> Vec<Query> {
    QUERY_NAMES
        .iter()
        .filter_map(|name| Query::new(name))
        .collect()
}

fn hex_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len() * 2);
    for byte in data {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn hex_decode(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(text.len() / 2);
    for i in (0..text.len()).step_by(2) {
        out.push(u8::from_str_radix(&text[i..i + 2], 16).ok()?);
    }
    Some(out)
}

/// Answer for the local terminal side. Fields that are `None` produce an
/// empty result, like an unsupported query.
#[derive(Clone, Debug, Default)]
pub struct TerminalState {
    pub app_name: String,
    pub version: String,
    pub font_family: Option<String>,
    pub bold_font: Option<String>,
    pub italic_font: Option<String>,
    pub bold_italic_font: Option<String>,
    pub font_size: Option<f64>,
    pub dpi_x: Option<f64>,
    pub dpi_y: Option<f64>,
    pub foreground: String,
    pub background: String,
    pub background_opacity: f64,
    pub clipboard_control: Vec<String>,
    pub os_name: String,
    pub allow_hyperlinks: AllowHyperlinks,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AllowHyperlinks {
    No,
    #[default]
    Yes,
    Ask,
}

pub fn answer_query(state: &TerminalState, name: &str) -> Option<String> {
    Some(match name {
        "name" => state.app_name.clone(),
        "version" => state.version.clone(),
        "font_family" => state.font_family.clone()?,
        "bold_font" => state.bold_font.clone()?,
        "italic_font" => state.italic_font.clone()?,
        "bold_italic_font" => state.bold_italic_font.clone()?,
        "font_size" => format!("{}", state.font_size?),
        "dpi_x" => format!("{}", state.dpi_x?),
        "dpi_y" => format!("{}", state.dpi_y?),
        "foreground" => state.foreground.clone(),
        "background" => state.background.clone(),
        "background_opacity" => format!("{}", state.background_opacity),
        "clipboard_control" => state.clipboard_control.join(" "),
        "os_name" => state.os_name.clone(),
        "allow_hyperlinks" => match state.allow_hyperlinks {
            AllowHyperlinks::No => "no".to_owned(),
            AllowHyperlinks::Yes => "yes".to_owned(),
            AllowHyperlinks::Ask => "ask".to_owned(),
        },
        _ => return None,
    })
}

/// Encode a DCS query response for one query. `found == false` uses the `0`
/// status byte, producing an empty answer.
pub fn encode_query_response(query: &Query, value: Option<&str>) -> Vec<u8> {
    let status = if value.is_some() { '1' } else { '0' };
    let payload = value
        .map(|value| format!("={}", hex_encode(value.as_bytes())))
        .unwrap_or_default();
    format!("\x1bP{status}+r{}{payload}\x1b\\", query.encoded_name).into_bytes()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueryResponse {
    pub found: bool,
    pub name: String,
    pub value: Option<String>,
}

/// Parse all complete DCS query responses in `data`.
pub fn parse_query_responses(data: &[u8]) -> Vec<QueryResponse> {
    let text = String::from_utf8_lossy(data);
    let mut out = Vec::new();
    let mut index = 0usize;
    while let Some(start) = text[index..].find("\x1bP") {
        let start = index + start;
        let Some(end_rel) = text[start + 2..].find("\x1b\\") else {
            break;
        };
        let end = start + 2 + end_rel;
        let body = &text[start + 2..end];
        index = end + 2;
        let Some(rest) = body
            .strip_prefix("1+r")
            .or_else(|| body.strip_prefix("0+r"))
        else {
            continue;
        };
        let found = body.starts_with("1+r");
        let comma = rest.find(['=', ';']);
        let (name_hex, value) = match comma {
            Some(comma) => {
                let name_hex = &rest[..comma];
                let value = if rest.as_bytes()[comma] == b'=' {
                    let encoded = &rest[comma + 1..];
                    hex_decode(encoded).map(|raw| String::from_utf8_lossy(&raw).into_owned())
                } else {
                    None
                };
                (name_hex, value)
            }
            None => (rest, None),
        };
        let name_bytes = hex_decode(name_hex).unwrap_or_default();
        let name = String::from_utf8_lossy(&name_bytes).into_owned();
        out.push(QueryResponse { found, name, value });
    }
    out
}

#[derive(Clone, Debug)]
pub struct QueryOptions {
    pub wait_for: Duration,
    pub queries: Vec<String>,
    pub help: bool,
}

impl Default for QueryOptions {
    fn default() -> Self {
        Self {
            wait_for: Duration::from_secs(10),
            queries: QUERY_NAMES.iter().map(|x| (*x).to_owned()).collect(),
            help: false,
        }
    }
}

pub fn parse_query_args(args: &[String]) -> Result<QueryOptions, String> {
    let mut opts = QueryOptions::default();
    let mut queries = Vec::new();
    let mut index = 0usize;
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
            "--wait-for" => {
                let value = take();
                opts.wait_for = Duration::from_secs_f64(
                    value
                        .parse()
                        .map_err(|_| format!("invalid wait time: {value}"))?,
                );
            }
            other if !other.starts_with('-') => queries.push(other.to_owned()),
            other => return Err(format!("unknown option: {other}")),
        }
        index += 1;
    }
    if !queries.is_empty() && !queries.iter().any(|q| q == "all") {
        for query in &queries {
            if !QUERY_NAMES.contains(&query.as_str()) {
                return Err(format!("Unknown query: {query}"));
            }
        }
        opts.queries = queries;
    }
    Ok(opts)
}

/// Run queries over a terminal transport, printing `query: value` lines.
pub fn run_query(
    opts: &QueryOptions,
    transport: &mut dyn InteractiveTransport,
    out: &mut dyn Write,
) -> Result<BTreeMap<String, Option<String>>, QueryError> {
    if opts.help {
        writeln!(out, "{QUERY_USAGE}")?;
        return Ok(BTreeMap::new());
    }
    let queries: Vec<Query> = opts
        .queries
        .iter()
        .filter_map(|name| Query::new(QUERY_NAMES.iter().find(|x| **x == name).copied()?))
        .collect();
    for query in &queries {
        transport.write_all(query.escape_code().as_bytes())?;
    }
    transport.write_all(b"\x1b[c")?;
    transport.flush()?;
    let mut answers = BTreeMap::new();
    for query in &queries {
        answers.insert(query.name.to_owned(), None);
    }
    let deadline = Instant::now() + opts.wait_for;
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    let mut complete = false;
    while Instant::now() < deadline && !complete {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match transport.read(&mut chunk, remaining) {
            Ok(0) => break,
            Ok(n) => {
                buffer.extend_from_slice(&chunk[..n]);
                for response in parse_query_responses(&buffer) {
                    if let Some(query) = queries
                        .iter()
                        .find(|query| query.query_name == response.name)
                    {
                        if let Some((_, value)) =
                            answers.iter_mut().find(|(name, _)| *name == query.name)
                        {
                            *value = response.value;
                        }
                    }
                }
                if has_primary_da(&buffer) {
                    complete = true;
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::TimedOut => break,
            Err(err) => return Err(err.into()),
        }
    }
    for (name, value) in &answers {
        match value {
            Some(value) => writeln!(out, "{name}: {value}")?,
            None => writeln!(out, "{name}:")?,
        }
    }
    Ok(answers)
}

fn has_primary_da(data: &[u8]) -> bool {
    let mut index = 0usize;
    while index + 2 < data.len() {
        if data[index] == 0x1b && data[index + 1] == b'[' {
            let mut cursor = index + 2;
            while cursor < data.len() {
                match data[cursor] {
                    b'c' => return true,
                    0x1b => break,
                    _ => cursor += 1,
                }
            }
        }
        index += 1;
    }
    false
}

#[derive(Debug)]
pub enum QueryError {
    Io(std::io::Error),
}

impl std::fmt::Display for QueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for QueryError {}

impl From<std::io::Error> for QueryError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::ScriptedTransport;

    #[test]
    fn query_names_and_escape_codes() {
        let query = Query::new("version").unwrap();
        assert_eq!(query.query_name, "kitty-query-version");
        assert_eq!(
            query.escape_code(),
            "\x1bP+q6b697474792d71756572792d76657273696f6e\x1b\\"
        );
        assert_eq!(Query::new("name").unwrap().query_name, "name");
    }

    #[test]
    fn response_roundtrip() {
        let query = Query::new("foreground").unwrap();
        let frame = encode_query_response(&query, Some("#112233"));
        let responses = parse_query_responses(&frame);
        assert_eq!(responses.len(), 1);
        assert!(responses[0].found);
        assert_eq!(responses[0].name, "kitty-query-foreground");
        assert_eq!(responses[0].value.as_deref(), Some("#112233"));
        let missing = parse_query_responses(&encode_query_response(&query, None));
        assert_eq!(missing[0].value, None);
    }

    #[test]
    fn local_answers() {
        let state = TerminalState {
            app_name: "xterm-kitty".to_owned(),
            version: "0.49.2".to_owned(),
            foreground: "#ffffff".to_owned(),
            background: "#000000".to_owned(),
            background_opacity: 0.9,
            clipboard_control: vec!["write-clipboard".to_owned(), "read-clipboard".to_owned()],
            os_name: "linux".to_owned(),
            allow_hyperlinks: AllowHyperlinks::Ask,
            ..TerminalState::default()
        };
        assert_eq!(answer_query(&state, "name").as_deref(), Some("xterm-kitty"));
        assert_eq!(
            answer_query(&state, "allow_hyperlinks").as_deref(),
            Some("ask")
        );
        assert_eq!(
            answer_query(&state, "clipboard_control").as_deref(),
            Some("write-clipboard read-clipboard")
        );
        assert_eq!(answer_query(&state, "font_family"), None);
    }

    #[test]
    fn run_queries_over_transport() {
        let mut transport = ScriptedTransport::new();
        let mut reply = Vec::new();
        for name in ["kitty-query-version", "kitty-query-foreground"] {
            let query = Query {
                name: "x",
                query_name: name.to_owned(),
                encoded_name: hex_encode(name.as_bytes()),
                help: "",
            };
            reply.extend_from_slice(&encode_query_response(
                &query,
                Some(if name.ends_with("version") {
                    "1.0"
                } else {
                    "#abcdef"
                }),
            ));
        }
        reply.extend_from_slice(b"\x1b[?62;c");
        transport.push_reply(reply);
        let opts = QueryOptions {
            wait_for: Duration::from_millis(10),
            queries: vec!["version".to_owned(), "foreground".to_owned()],
            help: false,
        };
        let mut out = Vec::new();
        let answers = run_query(&opts, &mut transport, &mut out).unwrap();
        assert_eq!(answers["version"].as_deref(), Some("1.0"));
        assert_eq!(answers["foreground"].as_deref(), Some("#abcdef"));
        let text = String::from_utf8_lossy(&out);
        assert!(text.contains("version: 1.0"));
    }
}
