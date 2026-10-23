// SPDX-License-Identifier: GPL-3.0-or-later
//! Hyperlinked grep, ported from `kittens/hyperlinked_grep`.
//!
//! `rg` output is parsed and file paths are wrapped in OSC 8 file:// links
//! while preserving match/context lines and heading behavior. Argument parsing
//! mirrors the Go kitten: `rg --help` supplies which options take values and
//! their short aliases. The parser is exposed separately so it can be tested
//! without running ripgrep.

use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Write};
use std::process::{Command, Stdio};

pub const HYPERLINKED_GREP_USAGE: &str = "\
Usage: kitten hyperlinked_grep [rg options] PATTERN [PATH ...]

Run ripgrep and add OSC 8 hyperlinks to file names.

  --kitten=hyperlink=none|all|matching_lines|file_headers|context_lines
  -h, --help";

#[derive(Clone, Debug, Default)]
pub struct RgOptionTable {
    pub expecting_args: HashMap<String, bool>,
    pub alias_map: HashMap<String, String>,
}

impl RgOptionTable {
    /// Parse the option section of `rg --help`.
    pub fn parse_help(text: &str) -> Result<Self, String> {
        let mut out = Self::default();
        let mut options_started = false;
        for line in text.lines() {
            if !options_started {
                if line.trim_end().ends_with("OPTIONS:") {
                    options_started = true;
                }
                continue;
            }
            let trimmed = line.trim_start();
            let indent = line.len() - trimmed.len();
            if indent == 0 || indent >= 8 {
                continue;
            }
            let expecting_arg = trimmed.contains('=') || trimmed.contains('<');
            let mut singles = Vec::new();
            let mut longs = Vec::new();
            for piece in trimmed.split(',') {
                let piece = piece.trim();
                if let Some(long) = piece.strip_prefix("--") {
                    let name = long
                        .split('=')
                        .next()
                        .unwrap_or(long)
                        .split_whitespace()
                        .next()
                        .unwrap_or(long);
                    longs.push(name.to_owned());
                } else if let Some(short) = piece.strip_prefix('-') {
                    let name = short.split_whitespace().next().unwrap_or(short);
                    if !name.is_empty() {
                        singles.push(name.to_owned());
                    }
                }
            }
            let Some(primary) = longs.first().cloned() else {
                return Err(format!("Failed to parse rg help output line: {line}"));
            };
            for alias in singles {
                out.alias_map.insert(alias, primary.clone());
            }
            for alias in longs.iter().skip(1) {
                out.alias_map.insert(alias.clone(), primary.clone());
            }
            out.expecting_args.insert(primary, expecting_arg);
        }
        if out.expecting_args.is_empty() || out.alias_map.is_empty() {
            return Err("Failed to parse rg help output, could not find any options".to_owned());
        }
        Ok(out)
    }

    /// A fallback table covering the common `rg` options, used if `rg --help`
    /// is unavailable.
    pub fn fallback() -> Self {
        let mut out = Self::default();
        for (name, expects) in [
            ("after-context", true),
            ("before-context", true),
            ("context", true),
            ("color", true),
            ("glob", true),
            ("iglob", true),
            ("ignore-file", true),
            ("max-count", true),
            ("regexp", true),
            ("replace", true),
            ("type", true),
            ("type-not", true),
            ("threads", true),
            ("context-separator", true),
            ("field-context-separator", true),
            ("field-match-separator", true),
            ("path-separator", true),
            ("encoding", true),
            ("pre", true),
            ("pre-glob", true),
            ("sort", true),
            ("sortr", true),
            ("engine", true),
            ("dfa-size-limit", true),
            ("max-columns", true),
            ("max-columns-preview", false),
            ("hidden", false),
            ("no-heading", false),
            ("heading", false),
            ("line-number", false),
            ("no-line-number", false),
            ("with-filename", false),
            ("no-filename", false),
            ("pretty", false),
            ("smart-case", false),
            ("ignore-case", false),
            ("case-sensitive", false),
            ("files", false),
            ("files-with-matches", false),
            ("files-without-match", false),
            ("count", false),
            ("count-matches", false),
            ("vimgrep", false),
            ("stats", false),
            ("null", false),
            ("null-data", false),
            ("type-list", false),
            ("version", false),
            ("help", false),
            ("kitten", true),
        ] {
            out.expecting_args.insert(name.to_owned(), expects);
        }
        for (short, long) in [
            ("A", "after-context"),
            ("B", "before-context"),
            ("C", "context"),
            ("g", "glob"),
            ("t", "type"),
            ("T", "type-not"),
            ("j", "threads"),
            ("e", "regexp"),
            ("r", "replace"),
            ("E", "encoding"),
            ("c", "count"),
            ("l", "files-with-matches"),
            ("L", "files-without-match"),
            ("H", "with-filename"),
            ("n", "line-number"),
            ("i", "ignore-case"),
            ("S", "smart-case"),
            ("s", "case-sensitive"),
            ("M", "max-columns"),
        ] {
            out.alias_map.insert(short.to_owned(), long.to_owned());
        }
        out
    }
}

#[derive(Clone, Debug)]
pub struct KittenOptions {
    pub matching_lines: bool,
    pub context_lines: bool,
    pub file_headers: bool,
    pub with_filename: bool,
    pub heading: bool,
    pub line_number: bool,
    pub stats: bool,
    pub count: bool,
    pub count_matches: bool,
    pub files: bool,
    pub files_with_matches: bool,
    pub files_without_match: bool,
    pub vimgrep: bool,
}

impl Default for KittenOptions {
    fn default() -> Self {
        Self {
            matching_lines: true,
            context_lines: true,
            file_headers: true,
            with_filename: true,
            heading: true,
            line_number: true,
            stats: false,
            count: false,
            count_matches: false,
            files: false,
            files_with_matches: false,
            files_without_match: false,
            vimgrep: false,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct GrepPlan {
    pub delegate: bool,
    pub sanitized_args: Vec<String>,
    pub kitten: KittenOptions,
    pub context_separator: String,
    pub field_context_separator: String,
    pub field_match_separator: String,
}

pub fn parse_grep_args(args: &[String], table: &RgOptionTable) -> Result<GrepPlan, String> {
    let mut plan = GrepPlan {
        kitten: KittenOptions::default(),
        context_separator: "--".to_owned(),
        field_context_separator: "-".to_owned(),
        field_match_separator: "-".to_owned(),
        ..GrepPlan::default()
    };
    let mut expecting: Option<String> = None;
    let mut index = 0usize;
    while index < args.len() {
        let arg = &args[index];
        if let Some(key) = expecting.take() {
            handle_option_arg(&mut plan, table, &key, arg, false)?;
            index += 1;
            continue;
        }
        if arg == "--" {
            plan.sanitized_args.extend_from_slice(&args[index..]);
            break;
        }
        if let Some(rest) = arg.strip_prefix("--") {
            let (raw_name, inline) = rest
                .split_once('=')
                .map_or((rest, None), |(a, b)| (a, Some(b.to_owned())));
            let name = table
                .alias_map
                .get(raw_name)
                .cloned()
                .unwrap_or_else(|| raw_name.to_owned());
            if let Some(value) = inline {
                if table.expecting_args.contains_key(&name) {
                    handle_option_arg(&mut plan, table, &name, &value, true)?;
                } else {
                    plan.sanitized_args.push(arg.clone());
                }
            } else if table.expecting_args.get(&name).copied().unwrap_or(false) {
                expecting = Some(name);
            } else {
                handle_bool_option(&mut plan, &name);
                plan.sanitized_args.push(arg.clone());
            }
        } else if let Some(rest) = arg.strip_prefix('-') {
            if rest.is_empty() {
                plan.sanitized_args.push(arg.clone());
                index += 1;
                continue;
            }
            let chars: Vec<char> = rest.chars().collect();
            let mut all_known = true;
            for ch in &chars {
                if !table.alias_map.contains_key(&ch.to_string()) {
                    all_known = false;
                    break;
                }
            }
            if !all_known {
                plan.sanitized_args.push(arg.clone());
            } else {
                for (pos, ch) in chars.iter().enumerate() {
                    let target = table
                        .alias_map
                        .get(&ch.to_string())
                        .cloned()
                        .unwrap_or_default();
                    if table.expecting_args.get(&target).copied().unwrap_or(false) {
                        let rest: String = chars[pos + 1..].iter().collect();
                        if rest.is_empty() {
                            expecting = Some(target);
                        } else {
                            handle_option_arg(&mut plan, table, &target, &rest, false)?;
                        }
                        break;
                    }
                    handle_bool_option(&mut plan, &target);
                    plan.sanitized_args.push(format!("-{ch}"));
                }
            }
        } else {
            plan.sanitized_args.push(arg.clone());
        }
        index += 1;
    }
    if !plan.kitten.with_filename
        || plan.context_separator != "--"
        || plan.field_context_separator != "-"
        || plan.field_match_separator != "-"
    {
        plan.delegate = true;
    }
    Ok(plan)
}

fn handle_option_arg(
    plan: &mut GrepPlan,
    table: &RgOptionTable,
    key: &str,
    value: &str,
    with_equals: bool,
) -> Result<(), String> {
    if key != "kitten" {
        if with_equals {
            plan.sanitized_args.push(format!("--{key}={value}"));
        } else {
            plan.sanitized_args.push(format!("--{key}"));
            plan.sanitized_args.push(value.to_owned());
        }
    }
    match key {
        "path-separator" => {
            if value != std::path::MAIN_SEPARATOR.to_string() {
                plan.delegate = true;
            }
        }
        "context-separator" => plan.context_separator = value.to_owned(),
        "field-context-separator" => plan.field_context_separator = value.to_owned(),
        "field-match-separator" => plan.field_match_separator = value.to_owned(),
        "kitten" => {
            let (key, value) = value
                .split_once('=')
                .ok_or_else(|| format!("Unknown --kitten option: {value}"))?;
            if key != "hyperlink" {
                return Err(format!("Unknown --kitten option: {key}"));
            }
            for item in value.split(',') {
                match item {
                    "none" => {
                        plan.kitten.context_lines = false;
                        plan.kitten.file_headers = false;
                        plan.kitten.matching_lines = false;
                    }
                    "all" => {
                        plan.kitten.context_lines = true;
                        plan.kitten.file_headers = true;
                        plan.kitten.matching_lines = true;
                    }
                    "matching_lines" => plan.kitten.matching_lines = true,
                    "file_headers" => plan.kitten.file_headers = true,
                    "context_lines" => plan.kitten.context_lines = true,
                    other => return Err(format!("hyperlink option invalid: {other}")),
                }
            }
        }
        other => {
            let _ = table;
            let _ = other;
        }
    }
    Ok(())
}

fn handle_bool_option(plan: &mut GrepPlan, key: &str) {
    match key {
        "no-context-separator" => plan.context_separator.clear(),
        "no-filename" => plan.kitten.with_filename = false,
        "with-filename" => plan.kitten.with_filename = true,
        "heading" => plan.kitten.heading = true,
        "no-heading" => plan.kitten.heading = false,
        "line-number" => plan.kitten.line_number = true,
        "no-line-number" => plan.kitten.line_number = false,
        "pretty" => {
            plan.kitten.line_number = true;
            plan.kitten.heading = true;
        }
        "stats" => plan.kitten.stats = true,
        "count" => plan.kitten.count = true,
        "count-matches" => plan.kitten.count_matches = true,
        "files" => plan.kitten.files = true,
        "files-with-matches" => plan.kitten.files_with_matches = true,
        "files-without-match" => plan.kitten.files_without_match = true,
        "vimgrep" => plan.kitten.vimgrep = true,
        "null" | "null-data" | "type-list" | "version" | "help" => plan.delegate = true,
        _ => {}
    }
}

/// Build a file:// URL for a path, escaping each path segment.
pub fn file_url(path: &str, hostname: &str) -> String {
    let absolute = if std::path::Path::new(path).is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()
            .map(|dir| dir.join(path).to_string_lossy().into_owned())
            .unwrap_or_else(|_| path.to_owned())
    };
    let mut url = format!("file://{hostname}");
    if !absolute.starts_with('/') {
        url.push('/');
    }
    for (index, segment) in absolute.split('/').enumerate() {
        if index > 0 {
            url.push('/');
        }
        url.push_str(&percent_encode(segment));
    }
    url
}

fn percent_encode(segment: &str) -> String {
    let mut out = String::new();
    for byte in segment.bytes() {
        if byte.is_ascii_alphanumeric() || "-._~!$&'()*+,;=:@".contains(byte as char) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Wrap a path in an OSC 8 hyperlink.
pub fn hyperlink(url: &str, text: &str, fragment: &str) -> String {
    let frag = if fragment.is_empty() {
        String::new()
    } else {
        format!("#{fragment}")
    };
    format!("\x1b]8;;{url}{frag}\x1b\\{text}\x1b]8;;\x1b\\")
}

/// A line-oriented processor for `rg --pretty --with-filename` output.
#[derive(Clone, Debug, Default)]
pub struct HyperlinkState {
    pub current_file: String,
    pub in_stats: bool,
    pub hostname: String,
}

impl HyperlinkState {
    pub fn new(hostname: impl Into<String>) -> Self {
        Self {
            hostname: hostname.into(),
            ..Self::default()
        }
    }

    /// Process one line. Returns the lines to write (without trailing newline).
    pub fn process_line(&mut self, line: &str, opts: &KittenOptions) -> Vec<String> {
        let stripped = strip_ansi(line);
        if stripped.is_empty() {
            self.current_file.clear();
            return vec![String::new()];
        }
        if self.in_stats {
            return vec![line.to_owned()];
        }
        if opts.stats && stripped.trim_end().ends_with(" matches") {
            self.in_stats = true;
            return vec![line.to_owned()];
        }
        if opts.heading && self.current_file.is_empty() && !looks_like_match(&stripped) {
            self.current_file = stripped.trim().to_owned();
            let url = file_url(&self.current_file, &self.hostname);
            return vec![hyperlink(&url, line, "")];
        }
        if let Some((path, line_no, col)) = parse_match_line(&stripped) {
            if !path.is_empty() {
                self.current_file = path.to_owned();
            }
            let file = if path.is_empty() {
                self.current_file.clone()
            } else {
                path.to_owned()
            };
            if file.is_empty() {
                return vec![line.to_owned()];
            }
            let url = file_url(&file, &self.hostname);
            if opts.line_number {
                let fragment = if let Some(col) = col {
                    format!("{line_no}:{col}")
                } else {
                    format!("{line_no}")
                };
                return vec![hyperlink(&url, line, &fragment)];
            }
            return vec![hyperlink(&url, line, "")];
        }
        vec![line.to_owned()]
    }
}

fn looks_like_match(line: &str) -> bool {
    parse_match_line(line).is_some()
}

fn parse_match_line(line: &str) -> Option<(&str, usize, Option<usize>)> {
    // vimgrep: file:line:col:text
    if let Some((path, rest)) = line.split_once(':') {
        let mut parts = rest.splitn(3, ':');
        if let (Ok(line_no), Some(col)) = (
            parts.next().unwrap_or("").parse::<usize>(),
            parts.next().and_then(|x| x.parse::<usize>().ok()),
        ) {
            if parts.next().is_some() {
                return Some((path, line_no, Some(col)));
            }
        }
        let mut parts = rest.splitn(2, ':');
        if let Ok(line_no) = parts.next().unwrap_or("").parse::<usize>() {
            if parts.next().is_some() {
                return Some((path, line_no, None));
            }
        }
    }
    // Context lines: file-line-text or file:line-text when pretty formatting
    for separator in [':', '-'] {
        if let Some((path, rest)) = line.split_once(separator) {
            let digits: String = rest.chars().take_while(|ch| ch.is_ascii_digit()).collect();
            if !digits.is_empty() {
                if let Ok(line_no) = digits.parse::<usize>() {
                    let tail = &rest[digits.len()..];
                    if tail.starts_with([':', '-']) {
                        return Some((path, line_no, None));
                    }
                }
            }
        }
    }
    None
}

fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\x1b' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('[') => {
                for next in chars.by_ref() {
                    if ('@'..='~').contains(&next) {
                        break;
                    }
                }
            }
            Some(']') => {
                let mut previous = '\0';
                for next in chars.by_ref() {
                    if previous == '\x1b' && next == '\\' {
                        break;
                    }
                    previous = next;
                }
            }
            Some(_) | None => {}
        }
    }
    out
}

/// Run ripgrep and hyperlink its output.
pub fn run_rg(args: &[String], out: &mut dyn Write) -> io::Result<i32> {
    let help = Command::new("rg").arg("--help").output();
    let table = help
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| RgOptionTable::parse_help(&String::from_utf8_lossy(&output.stdout)).ok())
        .unwrap_or_else(RgOptionTable::fallback);
    let plan = match parse_grep_args(args, &table) {
        Ok(plan) => plan,
        Err(err) => {
            writeln!(out, "{err}")?;
            return Ok(2);
        }
    };
    if plan.delegate {
        let status = Command::new("rg").args(&plan.sanitized_args).status()?;
        return Ok(status.code().unwrap_or(1));
    }
    let mut command_args = vec!["--pretty".to_owned(), "--with-filename".to_owned()];
    command_args.extend(plan.sanitized_args);
    let mut child = Command::new("rg")
        .args(&command_args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("no rg stdout"))?;
    let hostname = std::env::var("HOSTNAME").unwrap_or_default();
    let mut state = HyperlinkState::new(hostname);
    for line in BufReader::new(stdout).lines() {
        let line = line?;
        for processed in state.process_line(&line, &plan.kitten) {
            writeln!(out, "{processed}")?;
        }
    }
    let status = child.wait()?;
    Ok(status.code().unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    const HELP: &str = "ripgrep 14\n\nUSAGE:\n    rg [OPTIONS] PATTERN\n\nOPTIONS:\n    -A, --after-context <NUM>\n    -B, --before-context <NUM>\n    -C, --context <NUM>\n        --color <WHEN>\n    -g, --glob <GLOB>\n    -i, --ignore-case\n    -n, --line-number\n        --no-heading\n        --kitten <OPTS>\n";

    #[test]
    fn help_table() {
        let table = RgOptionTable::parse_help(HELP).unwrap();
        assert!(table.expecting_args["after-context"]);
        assert!(!table.expecting_args["ignore-case"]);
        assert_eq!(table.alias_map["A"], "after-context");
        assert_eq!(table.alias_map["n"], "line-number");
    }

    #[test]
    fn parses_short_and_long_options() {
        let table = RgOptionTable::fallback();
        let args = [
            "-i",
            "-A",
            "3",
            "--glob=*.rs",
            "--kitten=hyperlink=none",
            "needle",
            "src",
        ]
        .iter()
        .map(|x| (*x).to_owned())
        .collect::<Vec<_>>();
        let plan = parse_grep_args(&args, &table).unwrap();
        assert_eq!(
            plan.sanitized_args,
            vec!["-i", "--after-context", "3", "--glob=*.rs", "needle", "src"]
        );
        assert!(!plan.kitten.context_lines);
        assert!(!plan.kitten.file_headers);
    }

    #[test]
    fn urls_and_hyperlinks() {
        let url = file_url("/tmp/a b/c.rs", "host");
        assert_eq!(url, "file://host/tmp/a%20b/c.rs");
        let link = hyperlink(&url, "a b/c.rs", "12:3");
        assert!(link.starts_with("\x1b]8;;file://host/tmp/a%20b/c.rs#12:3\x1b\\"));
        assert!(link.ends_with("\x1b]8;;\x1b\\"));
    }

    #[test]
    fn line_processor_links_matches_and_headers() {
        let opts = KittenOptions::default();
        let mut state = HyperlinkState::new("h");
        let header = state.process_line("src/main.rs", &opts);
        assert!(header[0].contains("file://h/"));
        assert!(header[0].contains("src/main.rs"));
        let line = state.process_line("src/main.rs:12:needle", &opts);
        assert!(line[0].contains("#12"));
        assert!(line[0].contains("needle"));
    }

    #[test]
    fn ansi_is_stripped() {
        assert_eq!(strip_ansi("\x1b[31mred\x1b[0m"), "red");
        assert_eq!(strip_ansi("\x1b]8;;url\x1b\\text\x1b]8;;\x1b\\"), "text");
    }
}
