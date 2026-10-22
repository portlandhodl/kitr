// SPDX-License-Identifier: GPL-3.0-or-later
//! Rust-native `kitten hints`.
//!
//! This port implements the data and matching engine of the hints kitten: URL,
//! path, line, line-number (`linenum`), word, hash, IP, OSC 8 hyperlink and
//! custom-regex matching; unique short hint labels; multi-selection joining;
//! and launch-plan construction. It does **not** render an overlay UI. Callers
//! can print the generated labels/matches, choose labels programmatically, and
//! turn the selection into a paste/copy/open plan. The interactive overlay
//! remains a Python/Go kitten concern.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io::{self, IsTerminal, Read, Write};
use std::path::PathBuf;

use crate::regex_lite::{Regex, RegexError};

/// Pattern used for `--type=line` and the default `--type=regex`.
pub const DEFAULT_REGEX: &str = r"(?m)^\s*(.+?)\s*$";

/// Default hint alphabet.
pub const DEFAULT_ALPHABET: &str = "0123456789abcdefghijklmnopqrstuvwxyz";

/// Default URL schemes.
pub const DEFAULT_URL_PREFIXES: &[&str] = &["http", "https", "ftp", "file", "ssh", "kitty"];

/// Usage text for `kitten hints`.
pub const HINTS_USAGE: &str = "\
Usage: kitten hints [options]

Generate keyboard hints over text read from STDIN, --text or --input files.

Options:
      --type T            url, path, line, linenum, hash, word, regex,
                          hyperlink or ip (default url)
      --regex PATTERN     Pattern for --type=regex/linenum
      --alphabet CHARS    Hint alphabet (default 0-9a-z)
      --hints-offset N    First numerical hint index (default 1)
      --prefix-free       Make no hint a prefix of another
      --ascending         Number hints top to bottom instead of bottom-up
  -m, --multiple          Select multiple matches (affects joining)
      --multiple-joiner J auto, space, newline, empty, json or an index
      --minimum-match-length N
      --word-characters C Additional word characters
      --url-prefixes LIST Comma-separated schemes
  -p, --program P         Program/action; repeatable (-, @, *, @name, default)
  -a, --action A          paste, copy, primary or a program
      --hyperlink         Shorthand for --type=hyperlink
      --text TEXT         Use TEXT instead of/in addition to STDIN; repeatable
      --input FILE        Read text from FILE; repeatable
      --history FILE      Append previously captured text; repeatable
      --select LABEL      Select a label; repeatable
      --all               Select every generated hint
      --list              List hints (the default when nothing is selected)
  -h, --help              Show this help

No overlay UI is drawn. The command prints labels and, for selected hints,
the resulting paste/copy/open launch plan.";

/// Text matcher to use.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HintType {
    #[default]
    Url,
    Path,
    Line,
    Linenum,
    Hash,
    Word,
    Regex,
    Hyperlink,
    Ip,
}

impl HintType {
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "url" | "urls" => Self::Url,
            "path" | "paths" => Self::Path,
            "line" | "lines" => Self::Line,
            "linenum" | "line_number" => Self::Linenum,
            "hash" | "hashes" => Self::Hash,
            "word" | "words" => Self::Word,
            "regex" => Self::Regex,
            "hyperlink" | "hyperlinks" => Self::Hyperlink,
            "ip" => Self::Ip,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Url => "url",
            Self::Path => "path",
            Self::Line => "line",
            Self::Linenum => "linenum",
            Self::Hash => "hash",
            Self::Word => "word",
            Self::Regex => "regex",
            Self::Hyperlink => "hyperlink",
            Self::Ip => "ip",
        }
    }
}

/// Trailing-space policy for multi-selection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TrailingSpace {
    #[default]
    Auto,
    Always,
    Never,
}

/// Options controlling hint generation and action planning.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HintOptions {
    pub hint_type: HintType,
    pub regex: String,
    pub alphabet: String,
    pub hints_offset: usize,
    pub ascending: bool,
    pub prefix_free: bool,
    pub minimum_match_length: usize,
    pub word_characters: String,
    pub url_prefixes: Vec<String>,
    pub programs: Vec<String>,
    pub action: Option<String>,
    pub multiple: bool,
    pub multiple_joiner: String,
    pub add_trailing_space: TrailingSpace,
    pub cwd: PathBuf,
    pub hyperlink: bool,
}

impl Default for HintOptions {
    fn default() -> Self {
        Self {
            hint_type: HintType::Url,
            regex: DEFAULT_REGEX.to_owned(),
            alphabet: DEFAULT_ALPHABET.to_owned(),
            hints_offset: 1,
            ascending: false,
            prefix_free: false,
            minimum_match_length: 3,
            word_characters: String::new(),
            url_prefixes: Vec::new(),
            programs: Vec::new(),
            action: None,
            multiple: false,
            multiple_joiner: "auto".to_owned(),
            add_trailing_space: TrailingSpace::Auto,
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            hyperlink: false,
        }
    }
}

/// Errors produced by the hints engine.
#[derive(Debug)]
#[non_exhaustive]
pub enum HintError {
    Io(io::Error),
    InvalidOption { option: &'static str, value: String },
    InvalidAlphabet(String),
    InvalidRegex(String),
    NoMatches { hint_type: String, pattern: String },
    UnknownLabel(String),
    NoSelection,
    InvalidPlan(String),
}

impl fmt::Display for HintError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "I/O error: {err}"),
            Self::InvalidOption { option, value } => {
                write!(f, "invalid value for {option}: {value:?}")
            }
            Self::InvalidAlphabet(value) => {
                write!(
                    f,
                    "the hint alphabet needs at least two unique characters, got {value:?}"
                )
            }
            Self::InvalidRegex(message) => write!(f, "invalid regular expression: {message}"),
            Self::NoMatches { hint_type, pattern } => {
                if pattern.is_empty() {
                    write!(f, "No {hint_type} found")
                } else {
                    write!(f, "No {hint_type} found with pattern: {pattern}")
                }
            }
            Self::UnknownLabel(label) => write!(f, "unknown hint label: {label:?}"),
            Self::NoSelection => f.write_str("no hints were selected"),
            Self::InvalidPlan(message) => write!(f, "invalid launch plan: {message}"),
        }
    }
}

impl std::error::Error for HintError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<io::Error> for HintError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

/// One match before label assignment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mark {
    pub start: usize,
    pub end: usize,
    pub text: String,
    pub groupdict: BTreeMap<String, String>,
    pub is_hyperlink: bool,
    pub group_id: Option<String>,
}

/// A generated hint with its label and location.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hint {
    pub label: String,
    pub index: usize,
    pub start: usize,
    pub end: usize,
    pub text: String,
    pub groupdict: BTreeMap<String, String>,
    pub is_hyperlink: bool,
    pub group_id: Option<String>,
    pub line: usize,
    pub column: usize,
}

/// Result of matching a block of text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HintResult {
    pub text: String,
    pub hints: Vec<Hint>,
    pub pattern: String,
}

impl HintResult {
    /// Find a hint by its exact label.
    pub fn by_label(&self, label: &str) -> Option<&Hint> {
        self.hints.iter().find(|hint| hint.label == label)
    }

    /// Select hints in input-label order, rejecting unknown labels.
    pub fn select_labels(&self, labels: &[String]) -> Result<Vec<&Hint>, HintError> {
        let mut ans = Vec::new();
        for label in labels {
            let hint = self
                .by_label(label)
                .ok_or_else(|| HintError::UnknownLabel(label.clone()))?;
            if !ans
                .iter()
                .any(|existing: &&Hint| existing.label == hint.label)
            {
                ans.push(hint);
            }
        }
        Ok(ans)
    }
}

/// Where a chosen hint should go.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HintAction {
    Paste,
    Clipboard,
    Primary,
    Buffer(String),
    Open,
}

/// A launch plan for selected hints. No terminal overlay is involved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HintLaunchPlan {
    pub action: HintAction,
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub text: String,
}

/// Encode zero-based hint index `num` in the breadth-first order of the
/// prefix tree. For alphabet "abc": a, b, c, aa, ab, ...
pub fn encode_hint(num: usize, alphabet: &str) -> String {
    let chars: Vec<char> = alphabet.chars().collect();
    if chars.is_empty() {
        return String::new();
    }
    let len = chars.len();
    let mut num = num as isize;
    let mut out = Vec::new();
    while num >= 0 {
        let modulo = (num as usize) % len;
        out.push(chars[modulo]);
        num /= len as isize;
        num -= 1;
    }
    out.iter().rev().collect()
}

/// Reverse [`encode_hint`].
pub fn decode_hint(hint: &str, alphabet: &str) -> Option<usize> {
    if hint.is_empty() {
        return None;
    }
    let chars: Vec<char> = alphabet.chars().collect();
    if chars.is_empty() {
        return None;
    }
    let mut value = 0usize;
    for ch in hint.chars() {
        let index = chars.iter().position(|candidate| *candidate == ch)?;
        value = value.checked_mul(chars.len())?.checked_add(index + 1)?;
    }
    value.checked_sub(1)
}

/// Number of initial hints to skip so that the next `n` hints are prefix-free.
pub fn hints_to_skip(n: usize, alphabet_size: usize) -> usize {
    if alphabet_size <= 1 {
        return 0;
    }
    let n = n.max(2);
    (n - 2) / (alphabet_size - 1)
}

fn unique_alphabet(raw: &str) -> String {
    let mut seen = Vec::new();
    for ch in raw.chars() {
        if !seen.contains(&ch) {
            seen.push(ch);
        }
    }
    seen.into_iter().collect()
}

/// Assign unique labels to marks.
pub fn assign_labels(
    marks: &[Mark],
    alphabet: &str,
    offset: usize,
    ascending: bool,
    prefix_free: bool,
) -> Result<Vec<String>, HintError> {
    let alphabet = unique_alphabet(if alphabet.is_empty() {
        DEFAULT_ALPHABET
    } else {
        alphabet
    });
    if alphabet.chars().count() < 2 {
        return Err(HintError::InvalidAlphabet(alphabet));
    }
    let effective_offset = if prefix_free {
        offset.max(hints_to_skip(marks.len(), alphabet.chars().count()))
    } else {
        offset
    };
    let count = marks.len();
    let mut labels = Vec::with_capacity(count);
    for index in 0..count {
        let base = if ascending { index } else { count - 1 - index };
        labels.push(encode_hint(base + effective_offset, &alphabet));
    }
    Ok(labels)
}

#[derive(Clone, Debug)]
struct RawMark {
    start: usize,
    end: usize,
    text: String,
    groupdict: BTreeMap<String, String>,
    is_hyperlink: bool,
    group_id: Option<String>,
}

/// Generate hints over `text`.
pub fn generate_hints(text: &str, opts: &HintOptions) -> Result<HintResult, HintError> {
    let (sanitized, hyperlinks) = process_escape_codes(text);
    let mut marks = match opts.hint_type {
        HintType::Url => find_urls(&sanitized, opts),
        HintType::Path => find_paths(&sanitized, opts),
        HintType::Line => find_lines(&sanitized, opts),
        HintType::Linenum => find_linenums(&sanitized, opts)?,
        HintType::Hash => find_regex_marks(&sanitized, "[0-9a-f][0-9a-f\r]{6,127}", opts, false)?,
        HintType::Word => find_words(&sanitized, opts),
        HintType::Regex => {
            if opts.regex == DEFAULT_REGEX {
                find_lines(&sanitized, opts)
            } else {
                find_regex_marks(&sanitized, &opts.regex, opts, true)?
            }
        }
        HintType::Hyperlink => hyperlinks,
        HintType::Ip => find_regex_marks(
            &sanitized,
            r"(?:(?:\d{1,3}\.){3}\d{1,3}|(?:[a-fA-F0-9]{0,4}:){2,7}[a-fA-F0-9]{0,4})",
            opts,
            false,
        )?,
    };
    marks.retain(|mark| {
        mark.start < mark.end
            && mark.end <= sanitized.len()
            && sanitized
                .get(mark.start..mark.end)
                .is_some_and(|slice| !slice.is_empty())
    });
    if opts.hint_type == HintType::Hyperlink {
        marks.retain(|mark| {
            sanitized
                .get(mark.start..mark.end)
                .is_some_and(|slice| slice.chars().count() >= opts.minimum_match_length)
        });
    }
    dedupe_marks(&mut marks);
    if marks.is_empty() {
        return Err(HintError::NoMatches {
            hint_type: opts.hint_type.as_str().to_owned(),
            pattern: if opts.hint_type == HintType::Regex || opts.hint_type == HintType::Linenum {
                opts.regex.clone()
            } else {
                String::new()
            },
        });
    }
    let label_marks: Vec<Mark> = marks
        .iter()
        .map(|mark| Mark {
            start: mark.start,
            end: mark.end,
            text: mark.text.clone(),
            groupdict: mark.groupdict.clone(),
            is_hyperlink: mark.is_hyperlink,
            group_id: mark.group_id.clone(),
        })
        .collect();
    let labels = assign_labels(
        &label_marks,
        &opts.alphabet,
        opts.hints_offset,
        opts.ascending,
        opts.prefix_free,
    )?;
    let suffix = match opts.add_trailing_space {
        TrailingSpace::Always => " ",
        TrailingSpace::Never => "",
        TrailingSpace::Auto => {
            if opts.multiple {
                " "
            } else {
                ""
            }
        }
    };
    let mut hints = Vec::with_capacity(marks.len());
    for (index, (mark, label)) in marks.iter().zip(labels).enumerate() {
        let (line, column) = line_column(&sanitized, mark.start);
        hints.push(Hint {
            label,
            index,
            start: mark.start,
            end: mark.end,
            text: format!("{}{}", mark.text, suffix),
            groupdict: mark.groupdict.clone(),
            is_hyperlink: mark.is_hyperlink,
            group_id: mark.group_id.clone(),
            line,
            column,
        });
    }
    Ok(HintResult {
        text: sanitized,
        hints,
        pattern: opts.regex.clone(),
    })
}

fn dedupe_marks(marks: &mut Vec<RawMark>) {
    marks.sort_by(|a, b| {
        a.start
            .cmp(&b.start)
            .then_with(|| b.end.cmp(&a.end))
            .then_with(|| a.text.cmp(&b.text))
    });
    let mut ans: Vec<RawMark> = Vec::with_capacity(marks.len());
    for mark in marks.drain(..) {
        if ans
            .last()
            .is_some_and(|previous| previous.start == mark.start && previous.end == mark.end)
        {
            continue;
        }
        if ans.iter().any(|previous| {
            previous.start <= mark.start && mark.end <= previous.end && previous.text == mark.text
        }) {
            continue;
        }
        ans.push(mark);
    }
    *marks = ans;
}

/// Compute one-based line and column for a byte offset.
pub fn line_column(text: &str, offset: usize) -> (usize, usize) {
    let before = text.get(..offset).unwrap_or(text);
    let line = before.bytes().filter(|byte| *byte == b'\n').count() + 1;
    let line_start = before.rfind('\n').map(|index| index + 1).unwrap_or(0);
    let column = before[line_start..].chars().count() + 1;
    (line, column)
}

fn strip_control(text: &str) -> String {
    text.chars()
        .filter(|ch| !matches!(*ch, '\r' | '\n' | '\0'))
        .collect()
}

fn process_escape_codes(text: &str) -> (String, Vec<RawMark>) {
    let bytes = text.as_bytes();
    let mut output = String::with_capacity(text.len());
    let mut hyperlinks = Vec::new();
    let mut active: Option<(usize, String, Option<String>)> = None;
    let mut index = 0usize;
    while index < bytes.len() {
        if bytes[index] != 0x1b {
            let ch = text[index..].chars().next().unwrap();
            output.push(ch);
            index += ch.len_utf8();
            continue;
        }
        if index + 1 >= bytes.len() {
            break;
        }
        match bytes[index + 1] {
            b'[' => {
                let mut end = index + 2;
                while end < bytes.len() && !(0x40..=0x7e).contains(&bytes[end]) {
                    end += 1;
                }
                index = (end + 1).min(bytes.len());
            }
            b']' => {
                let body_start = index + 2;
                let mut end = body_start;
                let mut terminator = 0usize;
                while end < bytes.len() {
                    if bytes[end] == 0x07 {
                        terminator = 1;
                        break;
                    }
                    if bytes[end] == 0x1b && bytes.get(end + 1) == Some(&b'\\') {
                        terminator = 2;
                        break;
                    }
                    end += 1;
                }
                if terminator == 0 {
                    break;
                }
                let body = &text[body_start..end];
                if let Some(rest) = body.strip_prefix("8;") {
                    let (params, url) = rest.split_once(';').unwrap_or(("", rest));
                    if let Some((start, old_url, old_id)) = active.take() {
                        hyperlinks.push(RawMark {
                            start,
                            end: output.len(),
                            text: old_url,
                            groupdict: BTreeMap::new(),
                            is_hyperlink: true,
                            group_id: old_id,
                        });
                    }
                    if !url.is_empty() {
                        let id = params
                            .split(':')
                            .find_map(|entry| entry.strip_prefix("id="))
                            .filter(|value| !value.is_empty())
                            .map(str::to_owned);
                        active = Some((output.len(), url.to_owned(), id));
                    }
                }
                index = end + terminator;
            }
            _ => index += 2,
        }
    }
    if let Some((start, url, id)) = active {
        hyperlinks.push(RawMark {
            start,
            end: output.len(),
            text: url,
            groupdict: BTreeMap::new(),
            is_hyperlink: true,
            group_id: id,
        });
    }
    (output, hyperlinks)
}

fn find_lines(text: &str, opts: &HintOptions) -> Vec<RawMark> {
    let mut marks = Vec::new();
    let mut offset = 0usize;
    for line in text.split_inclusive('\n') {
        let bytes = line.as_bytes();
        let mut start = 0usize;
        while start < bytes.len() && bytes[start].is_ascii_whitespace() {
            start += 1;
        }
        let mut end = bytes.len();
        while end > start && bytes[end - 1].is_ascii_whitespace() {
            end -= 1;
        }
        if end > start {
            let value = text[offset + start..offset + end].to_owned();
            if value.chars().count() >= opts.minimum_match_length {
                marks.push(RawMark {
                    start: offset + start,
                    end: offset + end,
                    text: strip_control(&value),
                    groupdict: BTreeMap::new(),
                    is_hyperlink: false,
                    group_id: None,
                });
            }
        }
        offset += line.len();
    }
    marks
}

fn is_word_character(ch: char, extra: &[char]) -> bool {
    ch.is_alphanumeric() || ch == '_' || extra.contains(&ch)
}

fn find_words(text: &str, opts: &HintOptions) -> Vec<RawMark> {
    let extra: Vec<char> = opts.word_characters.chars().collect();
    let mut marks = Vec::new();
    let mut run_start: Option<(usize, usize)> = None;
    let mut run_chars = 0usize;
    for (byte, ch) in text.char_indices() {
        if is_word_character(ch, &extra) {
            if run_start.is_none() {
                run_start = Some((byte, byte));
                run_chars = 0;
            }
            run_chars += 1;
            run_start.as_mut().unwrap().1 = byte + ch.len_utf8();
        } else if let Some((start, end)) = run_start.take() {
            push_word(&mut marks, text, start, end, run_chars, opts);
        }
    }
    if let Some((start, end)) = run_start {
        push_word(&mut marks, text, start, end, run_chars, opts);
    }
    marks
}

fn push_word(
    marks: &mut Vec<RawMark>,
    text: &str,
    start: usize,
    end: usize,
    chars: usize,
    opts: &HintOptions,
) {
    if chars >= opts.minimum_match_length {
        let value = text[start..end].to_owned();
        marks.push(RawMark {
            start,
            end,
            text: value,
            groupdict: BTreeMap::new(),
            is_hyperlink: false,
            group_id: None,
        });
    }
}

fn find_urls(text: &str, opts: &HintOptions) -> Vec<RawMark> {
    let prefixes: Vec<String> = if opts.url_prefixes.is_empty() {
        DEFAULT_URL_PREFIXES
            .iter()
            .map(|x| (*x).to_owned())
            .collect()
    } else {
        opts.url_prefixes.clone()
    };
    let mut marks = Vec::new();
    let mut index = 0usize;
    while index < text.len() {
        let Some(ch) = text[index..].chars().next() else {
            break;
        };
        let lower = text[index..].to_ascii_lowercase();
        let mut matched = None;
        for prefix in &prefixes {
            let needle = format!("{}://", prefix.to_ascii_lowercase());
            if lower.starts_with(&needle) {
                matched = Some(prefix.len() + 3);
                break;
            }
        }
        let Some(scheme_len) = matched else {
            index += ch.len_utf8();
            continue;
        };
        let mut end = index + scheme_len;
        for (offset, candidate) in text[end..].char_indices() {
            if candidate.is_whitespace()
                || candidate.is_control()
                || matches!(candidate, '"' | '<' | '>' | '`' | '\\')
            {
                break;
            }
            end += candidate.len_utf8();
            let _ = offset;
        }
        let (start, mut end) = trim_url(text, index, end);
        if start < end && text[start..end].chars().count() >= opts.minimum_match_length {
            if start >= 5 && text[..start].ends_with("link:") {
                if let Some(open) = text[start..end].rfind('[') {
                    end = start + open;
                }
            }
            if end >= 2 && text[end - 2..end].ends_with("`_") {
                end -= 2;
            }
            if start < end {
                let value = strip_control(&text[start..end]);
                marks.push(RawMark {
                    start,
                    end,
                    text: value,
                    groupdict: BTreeMap::new(),
                    is_hyperlink: false,
                    group_id: None,
                });
            }
        }
        index = end.max(index + ch.len_utf8());
    }
    marks
}

fn trim_url(text: &str, start: usize, mut end: usize) -> (usize, usize) {
    while end > start {
        let Some(last) = text[start..end].chars().next_back() else {
            break;
        };
        if matches!(last, ',' | '.' | '?' | '!' | ';' | ':') {
            end -= last.len_utf8();
        } else {
            break;
        }
    }
    if start > 0 {
        let before = text[..start].chars().next_back();
        if let Some(open) = before {
            if let Some(close) = closing_bracket(open) {
                if let Some(offset) = text[start..end].find(close) {
                    end = start + offset;
                }
            }
        }
    }
    (start, end)
}

fn closing_bracket(ch: char) -> Option<char> {
    Some(match ch {
        '(' => ')',
        '[' => ']',
        '{' => '}',
        '<' => '>',
        '"' => '"',
        '\'' => '\'',
        '“' => '”',
        '‘' => '’',
        _ => return None,
    })
}

fn trim_token(text: &str, mut start: usize, mut end: usize) -> (usize, usize) {
    while let Some(ch) = text[start..end].chars().next() {
        if matches!(ch, '(' | '[' | '{' | '<' | '"' | '\'' | '“' | '‘') {
            start += ch.len_utf8();
        } else {
            break;
        }
    }
    while end > start {
        let Some(ch) = text[start..end].chars().next_back() else {
            break;
        };
        if matches!(
            ch,
            ',' | '.'
                | ';'
                | '!'
                | '?'
                | ')'
                | ']'
                | '}'
                | '>'
                | '"'
                | '\''
                | '`'
                | '“'
                | '”'
                | '‘'
                | '’'
        ) || ch == ':'
        {
            end -= ch.len_utf8();
        } else if ch == '_' && text[start..end].ends_with("`_") {
            end -= 2;
        } else {
            break;
        }
    }
    (start, end)
}

fn looks_like_path(token: &str) -> bool {
    if token.is_empty() || token.contains("://") {
        return false;
    }
    if token.contains('/') || token.contains('\\') {
        return true;
    }
    if let Some((_, extension)) = token.rsplit_once('.') {
        let len = extension.chars().count();
        if (2..=8).contains(&len)
            && extension.chars().all(|ch| ch.is_ascii_alphanumeric())
            && !extension.chars().all(|ch| ch.is_ascii_digit())
        {
            return true;
        }
    }
    token.starts_with('~') || token == "." || token == ".."
}

fn whitespace_tokens(text: &str) -> Vec<(usize, usize)> {
    let mut ans = Vec::new();
    let mut start = None;
    for (byte, ch) in text.char_indices() {
        if ch.is_whitespace() {
            if let Some(begin) = start.take() {
                ans.push((begin, byte));
            }
        } else if start.is_none() {
            start = Some(byte);
        }
    }
    if let Some(begin) = start {
        ans.push((begin, text.len()));
    }
    ans
}

fn find_paths(text: &str, opts: &HintOptions) -> Vec<RawMark> {
    let mut marks = Vec::new();
    for (token_start, token_end) in whitespace_tokens(text) {
        let (start, end) = trim_token(text, token_start, token_end);
        if start >= end {
            continue;
        }
        let token = &text[start..end];
        if !looks_like_path(token) || token.chars().count() < opts.minimum_match_length {
            continue;
        }
        marks.push(RawMark {
            start,
            end,
            text: token.to_owned(),
            groupdict: BTreeMap::new(),
            is_hyperlink: false,
            group_id: None,
        });
    }
    marks
}

fn find_linenums(text: &str, opts: &HintOptions) -> Result<Vec<RawMark>, HintError> {
    if opts.regex != DEFAULT_REGEX {
        return find_linenums_regex(text, opts);
    }
    let mut marks = Vec::new();
    for (token_start, token_end) in whitespace_tokens(text) {
        let (start, end) = trim_token(text, token_start, token_end);
        if start >= end {
            continue;
        }
        let token = &text[start..end];
        let core = token.trim_end_matches(':');
        let parts: Vec<&str> = core.split(':').collect();
        if parts.len() < 2 {
            continue;
        }
        let last_numeric = parts
            .last()
            .is_some_and(|part| !part.is_empty() && part.chars().all(|ch| ch.is_ascii_digit()));
        if !last_numeric {
            continue;
        }
        let (path_end, line, column) = if parts.len() >= 3 {
            let second_last = parts[parts.len() - 2];
            if !second_last.is_empty() && second_last.chars().all(|ch| ch.is_ascii_digit()) {
                (
                    parts.len() - 2,
                    second_last.parse::<usize>().unwrap_or(0),
                    parts.last().unwrap().parse::<usize>().unwrap_or(1),
                )
            } else {
                (
                    parts.len() - 1,
                    parts.last().unwrap().parse::<usize>().unwrap_or(0),
                    1,
                )
            }
        } else {
            (
                parts.len() - 1,
                parts.last().unwrap().parse::<usize>().unwrap_or(0),
                1,
            )
        };
        let path = parts[..path_end].join(":");
        if path.is_empty() || !looks_like_path(&path) {
            continue;
        }
        let mut groupdict = BTreeMap::new();
        groupdict.insert("path".to_owned(), expanduser(&path));
        groupdict.insert("line".to_owned(), line.to_string());
        groupdict.insert("column".to_owned(), column.to_string());
        if core.chars().count() < opts.minimum_match_length {
            continue;
        }
        marks.push(RawMark {
            start,
            end,
            text: core.to_owned(),
            groupdict,
            is_hyperlink: false,
            group_id: None,
        });
    }
    Ok(marks)
}

fn find_linenums_regex(text: &str, opts: &HintOptions) -> Result<Vec<RawMark>, HintError> {
    let regex = Regex::new(&opts.regex).map_err(|err| HintError::InvalidRegex(err.to_string()))?;
    let mut marks = Vec::new();
    for matched in regex.find_iter(text) {
        let mut groupdict = BTreeMap::new();
        for (name, value) in matched.named_groups(text) {
            groupdict.insert(name, value.to_owned());
        }
        let Some(path) = groupdict.get("path").cloned() else {
            continue;
        };
        let mut path = expanduser(&path);
        if let Some((prefix, suffix)) = path.rsplit_once(':') {
            if !suffix.is_empty() && suffix.chars().all(|ch| ch.is_ascii_digit()) {
                groupdict
                    .entry("line".to_owned())
                    .or_insert_with(|| suffix.to_owned());
                path = prefix.to_owned();
            }
        }
        let line = groupdict
            .get("line")
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(0);
        if line == 0 {
            continue;
        }
        groupdict.insert("path".to_owned(), path);
        groupdict.insert("line".to_owned(), line.to_string());
        groupdict
            .entry("column".to_owned())
            .or_insert_with(|| "1".to_owned());
        let value = strip_control(&text[matched.start..matched.end]);
        if value.chars().count() >= opts.minimum_match_length {
            marks.push(RawMark {
                start: matched.start,
                end: matched.end,
                text: value,
                groupdict,
                is_hyperlink: false,
                group_id: None,
            });
        }
    }
    Ok(marks)
}

fn expanduser(path: &str) -> String {
    if path == "~" || path.starts_with("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return format!("{}{}", PathBuf::from(home).display(), &path[1..]);
        }
    }
    path.to_owned()
}

fn find_regex_marks(
    text: &str,
    pattern: &str,
    opts: &HintOptions,
    _select_group: bool,
) -> Result<Vec<RawMark>, HintError> {
    let regex = Regex::new(pattern).map_err(|err: RegexError| {
        HintError::InvalidRegex(format!("{err} (pattern {pattern:?})"))
    })?;
    let mut marks = Vec::new();
    for matched in regex.find_iter(text) {
        let mut start = matched.start;
        let mut end = matched.end;
        let mut groupdict = BTreeMap::new();
        let named = matched.named_groups(text);
        for (name, value) in &named {
            groupdict.insert(name.clone(), strip_control(value));
        }
        if named.is_empty() && matched.groups.len() > 1 {
            if let Some((group_start, group_end)) = matched.groups[1] {
                start = start.max(group_start);
                end = end.min(group_end);
            }
        }
        if text[matched.start..matched.end].chars().count() < opts.minimum_match_length {
            continue;
        }
        let value = strip_control(&text[start..end]);
        marks.push(RawMark {
            start,
            end,
            text: value,
            groupdict,
            is_hyperlink: false,
            group_id: None,
        });
    }
    Ok(marks)
}

/// Join selected hints according to `--multiple-joiner`.
pub fn join_hints(hints: &[&Hint], opts: &HintOptions) -> String {
    let matches: Vec<&str> = hints.iter().map(|hint| hint.text.as_str()).collect();
    if matches.is_empty() {
        return String::new();
    }
    if let Ok(index) = opts.multiple_joiner.parse::<isize>() {
        let len = matches.len() as isize;
        let selected = if index < 0 { len + index } else { index };
        if selected >= 0 && selected < len {
            return matches[selected as usize].to_owned();
        }
        return matches[matches.len() - 1].to_owned();
    }
    match opts.multiple_joiner.as_str() {
        "json" => {
            let mut out = String::from("[\n");
            for (index, value) in matches.iter().enumerate() {
                out.push('\t');
                out.push_str(&json_string(value));
                if index + 1 < matches.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            out.push(']');
            out
        }
        "newline" => matches.join("\n"),
        "space" => matches.join(" "),
        "empty" => matches.concat(),
        "auto" => {
            let separator = if matches!(opts.hint_type, HintType::Line | HintType::Url) {
                "\n"
            } else {
                " "
            };
            matches.join(separator)
        }
        other => matches.join(other),
    }
}

fn json_string(value: &str) -> String {
    let mut out = String::from("\"");
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if ch.is_control() => out.push_str(&format!("\\u{:04x}", ch as u32)),
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

/// Build launch plans for selected hints.
pub fn build_launch_plans(
    selected: &[&Hint],
    opts: &HintOptions,
) -> Result<Vec<HintLaunchPlan>, HintError> {
    if selected.is_empty() {
        return Err(HintError::NoSelection);
    }
    let text = join_hints(selected, opts);
    let programs: Vec<String> = if opts.programs.is_empty() {
        vec![opts.action.clone().unwrap_or_else(|| "default".to_owned())]
    } else {
        opts.programs.clone()
    };
    let mut plans = Vec::new();
    for program in programs {
        let lower = program.to_ascii_lowercase();
        let action = match lower.as_str() {
            "-" | "paste" => {
                plans.push(HintLaunchPlan {
                    action: HintAction::Paste,
                    program: "-".to_owned(),
                    args: Vec::new(),
                    cwd: opts.cwd.clone(),
                    text: text.clone(),
                });
                continue;
            }
            "*" | "primary" => {
                plans.push(HintLaunchPlan {
                    action: HintAction::Primary,
                    program: "*".to_owned(),
                    args: Vec::new(),
                    cwd: opts.cwd.clone(),
                    text: text.clone(),
                });
                continue;
            }
            "copy" | "clipboard" | "@" => {
                plans.push(HintLaunchPlan {
                    action: HintAction::Clipboard,
                    program: "@".to_owned(),
                    args: Vec::new(),
                    cwd: opts.cwd.clone(),
                    text: text.clone(),
                });
                continue;
            }
            _ if program.starts_with('@') => {
                plans.push(HintLaunchPlan {
                    action: HintAction::Buffer(program[1..].to_owned()),
                    program: program.clone(),
                    args: Vec::new(),
                    cwd: opts.cwd.clone(),
                    text: text.clone(),
                });
                continue;
            }
            _ => HintAction::Open,
        };
        let (program_name, mut args) = if let Some(rest) = program.strip_prefix("launch ") {
            ("launch".to_owned(), split_shell_words(rest))
        } else if lower == "default" {
            ("xdg-open".to_owned(), Vec::new())
        } else {
            (program.clone(), Vec::new())
        };
        let has_groups = selected.iter().any(|hint| !hint.groupdict.is_empty());
        if has_groups {
            for hint in selected {
                for (key, value) in &hint.groupdict {
                    args.push(format!("{key}={value}"));
                }
            }
        } else {
            args.push(text.clone());
        }
        plans.push(HintLaunchPlan {
            action,
            program: program_name,
            args,
            cwd: opts.cwd.clone(),
            text: text.clone(),
        });
    }
    Ok(plans)
}

fn split_shell_words(input: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escaped = false;
    for ch in input.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        if ch == '\\' && quote != Some('\'') {
            escaped = true;
            continue;
        }
        if let Some(q) = quote {
            if ch == q {
                quote = None;
            } else {
                current.push(ch);
            }
            continue;
        }
        if matches!(ch, '\'' | '"') {
            quote = Some(ch);
        } else if ch.is_whitespace() {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
        } else {
            current.push(ch);
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

/// Parsed hints command.
#[derive(Clone, Debug, Default)]
pub struct HintsCommand {
    pub options: HintOptions,
    pub text: Vec<String>,
    pub input_files: Vec<PathBuf>,
    pub history_files: Vec<PathBuf>,
    pub selects: Vec<String>,
    pub all: bool,
    pub list: bool,
    pub help: bool,
}

/// Parse arguments after `kitten hints`.
pub fn parse_hints_args(args: &[String]) -> Result<HintsCommand, HintError> {
    let mut cmd = HintsCommand::default();
    let mut index = 0usize;
    while index < args.len() {
        let arg = &args[index];
        if !arg.starts_with('-') || arg == "-" {
            cmd.text.push(arg.clone());
            index += 1;
            continue;
        }
        let (name, inline) = match arg.split_once('=') {
            Some((name, value)) => (name, Some(value.to_owned())),
            None => (arg.as_str(), None),
        };
        let mut take = |option: &'static str| -> Result<String, HintError> {
            if let Some(value) = &inline {
                return Ok(value.clone());
            }
            index += 1;
            args.get(index)
                .cloned()
                .ok_or_else(|| HintError::InvalidOption {
                    option,
                    value: String::new(),
                })
        };
        match name {
            "-h" | "--help" => cmd.help = true,
            "--type" => {
                let value = take("--type")?;
                cmd.options.hint_type = HintType::parse(&value).ok_or({
                    HintError::InvalidOption {
                        option: "--type",
                        value,
                    }
                })?;
            }
            "--regex" => cmd.options.regex = take("--regex")?,
            "--alphabet" => cmd.options.alphabet = take("--alphabet")?,
            "--hints-offset" => {
                let value = take("--hints-offset")?;
                cmd.options.hints_offset = value.parse().map_err(|_| HintError::InvalidOption {
                    option: "--hints-offset",
                    value,
                })?;
            }
            "--prefix-free" => cmd.options.prefix_free = true,
            "--ascending" => cmd.options.ascending = true,
            "-m" | "--multiple" => cmd.options.multiple = true,
            "--multiple-joiner" => cmd.options.multiple_joiner = take("--multiple-joiner")?,
            "--minimum-match-length" => {
                let value = take("--minimum-match-length")?;
                cmd.options.minimum_match_length =
                    value.parse().map_err(|_| HintError::InvalidOption {
                        option: "--minimum-match-length",
                        value,
                    })?;
            }
            "--word-characters" => cmd.options.word_characters = take("--word-characters")?,
            "--url-prefixes" => {
                cmd.options.url_prefixes = take("--url-prefixes")?
                    .split(',')
                    .map(|x| x.trim().to_owned())
                    .filter(|x| !x.is_empty())
                    .collect()
            }
            "-p" | "--program" => cmd.options.programs.push(take("--program")?),
            "-a" | "--action" => cmd.options.action = Some(take("--action")?),
            "--hyperlink" => {
                cmd.options.hyperlink = true;
                cmd.options.hint_type = HintType::Hyperlink;
            }
            "--text" => cmd.text.push(take("--text")?),
            "--input" => cmd.input_files.push(PathBuf::from(take("--input")?)),
            "--history" => cmd.history_files.push(PathBuf::from(take("--history")?)),
            "--select" => cmd.selects.push(take("--select")?),
            "--all" => cmd.all = true,
            "--list" => cmd.list = true,
            "--add-trailing-space" => {
                let value = take("--add-trailing-space")?;
                cmd.options.add_trailing_space = match value.as_str() {
                    "always" => TrailingSpace::Always,
                    "never" => TrailingSpace::Never,
                    "auto" => TrailingSpace::Auto,
                    _ => {
                        return Err(HintError::InvalidOption {
                            option: "--add-trailing-space",
                            value,
                        })
                    }
                };
            }
            other => {
                return Err(HintError::InvalidOption {
                    option: "argument",
                    value: other.to_owned(),
                })
            }
        }
        index += 1;
    }
    Ok(cmd)
}

fn read_sources(cmd: &HintsCommand, stdin: &mut dyn Read) -> Result<String, HintError> {
    let mut parts = Vec::new();
    if !cmd.text.is_empty() {
        parts.push(cmd.text.join("\n"));
    }
    for path in cmd.input_files.iter().chain(cmd.history_files.iter()) {
        let data = fs::read_to_string(path).map_err(|err| {
            HintError::Io(io::Error::new(
                err.kind(),
                format!("cannot read {}: {err}", path.display()),
            ))
        })?;
        parts.push(data);
    }
    if parts.is_empty() {
        if io::stdin().is_terminal() {
            return Err(HintError::InvalidOption {
                option: "input",
                value: "no text provided; pipe text on STDIN or use --text/--input".to_owned(),
            });
        }
        let mut data = String::new();
        stdin.read_to_string(&mut data)?;
        parts.push(data);
    }
    Ok(parts.join("\n"))
}

/// Run `kitten hints` without an overlay. Prints labels, or launch plans for
/// selected labels.
pub fn run_hints_cli(
    args: &[String],
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
) -> Result<(), HintError> {
    let cmd = parse_hints_args(args)?;
    if cmd.help {
        writeln!(stdout, "{HINTS_USAGE}")?;
        return Ok(());
    }
    let text = read_sources(&cmd, stdin)?;
    let result = generate_hints(&text, &cmd.options)?;
    if cmd.selects.is_empty() && !cmd.all {
        for hint in &result.hints {
            writeln!(
                stdout,
                "{}\t{}:{}\t{}",
                hint.label, hint.line, hint.column, hint.text
            )?;
        }
        return Ok(());
    }
    let labels: Vec<String> = if cmd.all {
        result.hints.iter().map(|hint| hint.label.clone()).collect()
    } else {
        cmd.selects.clone()
    };
    let selected = result.select_labels(&labels)?;
    for plan in build_launch_plans(&selected, &cmd.options)? {
        match &plan.action {
            HintAction::Paste => writeln!(stdout, "paste\t{}", plan.text)?,
            HintAction::Clipboard => writeln!(stdout, "clipboard\t{}", plan.text)?,
            HintAction::Primary => writeln!(stdout, "primary\t{}", plan.text)?,
            HintAction::Buffer(name) => writeln!(stdout, "buffer:{name}\t{}", plan.text)?,
            HintAction::Open => {
                write!(stdout, "open\t{}", plan.program)?;
                for arg in &plan.args {
                    write!(stdout, "\t{arg}")?;
                }
                writeln!(stdout)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|x| (*x).to_owned()).collect()
    }

    fn options(kind: HintType) -> HintOptions {
        HintOptions {
            hint_type: kind,
            ..HintOptions::default()
        }
    }

    #[test]
    fn hint_encoding_matches_go_examples() {
        assert_eq!(encode_hint(0, "abc"), "a");
        assert_eq!(encode_hint(1, "abc"), "b");
        assert_eq!(encode_hint(2, "abc"), "c");
        assert_eq!(encode_hint(3, "abc"), "aa");
        assert_eq!(encode_hint(4, "abc"), "ab");
        assert_eq!(encode_hint(5, "abc"), "ac");
        assert_eq!(encode_hint(12, "abc"), "aaa");
        for index in 0..200 {
            let hint = encode_hint(index, "abc");
            assert_eq!(decode_hint(&hint, "abc"), Some(index));
        }
    }

    #[test]
    fn prefix_free_labels_are_unique_and_prefix_free() {
        assert_eq!(hints_to_skip(5, 3), 1);
        let marks: Vec<Mark> = (0..5)
            .map(|index| Mark {
                start: index,
                end: index + 1,
                text: index.to_string(),
                groupdict: BTreeMap::new(),
                is_hyperlink: false,
                group_id: None,
            })
            .collect();
        let labels = assign_labels(&marks, "abc", 0, true, true).unwrap();
        assert_eq!(labels, ["b", "c", "aa", "ab", "ac"]);
        for (index, label) in labels.iter().enumerate() {
            for other in labels.iter().skip(index + 1) {
                assert!(!other.starts_with(label));
                assert!(!label.starts_with(other));
            }
        }
        assert!(assign_labels(&marks, "a", 0, true, false).is_err());
    }

    #[test]
    fn descending_offset_is_applied() {
        let marks: Vec<Mark> = (0..3)
            .map(|index| Mark {
                start: index,
                end: index + 1,
                text: index.to_string(),
                groupdict: BTreeMap::new(),
                is_hyperlink: false,
                group_id: None,
            })
            .collect();
        assert_eq!(
            assign_labels(&marks, DEFAULT_ALPHABET, 1, false, false).unwrap(),
            ["3", "2", "1"]
        );
    }

    #[test]
    fn overlapping_and_identical_marks_are_deduped() {
        let mut marks = vec![
            RawMark {
                start: 0,
                end: 3,
                text: "abc".to_owned(),
                groupdict: BTreeMap::new(),
                is_hyperlink: false,
                group_id: None,
            },
            RawMark {
                start: 0,
                end: 3,
                text: "abc".to_owned(),
                groupdict: BTreeMap::new(),
                is_hyperlink: false,
                group_id: None,
            },
            RawMark {
                start: 1,
                end: 2,
                text: "abc".to_owned(),
                groupdict: BTreeMap::new(),
                is_hyperlink: false,
                group_id: None,
            },
            RawMark {
                start: 5,
                end: 8,
                text: "abc".to_owned(),
                groupdict: BTreeMap::new(),
                is_hyperlink: false,
                group_id: None,
            },
        ];
        dedupe_marks(&mut marks);
        assert_eq!(marks.len(), 2);
        assert_eq!((marks[0].start, marks[0].end), (0, 3));
        assert_eq!((marks[1].start, marks[1].end), (5, 8));
    }

    #[test]
    fn url_matching_strips_punctuation_and_brackets() {
        let text = "See (https://example.com/a?b=1). Also link:https://x.test/[label]";
        let result = generate_hints(text, &options(HintType::Url)).unwrap();
        assert_eq!(result.hints[0].text, "https://example.com/a?b=1");
        assert_eq!(result.hints[1].text, "https://x.test/");
    }

    #[test]
    fn path_matching_and_line_columns() {
        let text = "first /tmp/a.rs second src/main.py and C:\\x\\y.txt";
        let result = generate_hints(text, &options(HintType::Path)).unwrap();
        let texts: Vec<&str> = result.hints.iter().map(|hint| hint.text.as_str()).collect();
        assert_eq!(texts, ["/tmp/a.rs", "src/main.py", "C:\\x\\y.txt"]);
        assert_eq!(result.hints[0].line, 1);
        assert_eq!(result.hints[0].column, 7);
    }

    #[test]
    fn line_number_extraction_with_column_and_dedupe() {
        let text = "error at /tmp/a.rs:12:3: boom\n/tmp/a.rs:12:3";
        let result = generate_hints(text, &options(HintType::Linenum)).unwrap();
        assert_eq!(result.hints.len(), 2);
        let first = &result.hints[0];
        assert_eq!(first.groupdict.get("path").unwrap(), "/tmp/a.rs");
        assert_eq!(first.groupdict.get("line").unwrap(), "12");
        assert_eq!(first.groupdict.get("column").unwrap(), "3");
        assert_eq!(first.line, 1);
    }

    #[test]
    fn line_and_word_matching() {
        let lines = "  alpha  \n\tbeta gamma\t\n";
        let result = generate_hints(lines, &options(HintType::Line)).unwrap();
        assert_eq!(
            result
                .hints
                .iter()
                .map(|h| h.text.as_str())
                .collect::<Vec<_>>(),
            ["alpha", "beta gamma"]
        );
        let word_opts = HintOptions {
            minimum_match_length: 4,
            ..options(HintType::Word)
        };
        let result = generate_hints("alpha beta gamma", &word_opts).unwrap();
        assert_eq!(
            result
                .hints
                .iter()
                .map(|h| h.text.as_str())
                .collect::<Vec<_>>(),
            ["alpha", "beta", "gamma"]
        );
        assert_eq!(result.hints[0].label, "3");
    }

    #[test]
    fn custom_regex_named_groups_and_capture_selection() {
        let mut opts = options(HintType::Regex);
        opts.regex = r"(?P<name>\w+)=(\d+)".to_owned();
        let result = generate_hints("a=1 b=22", &opts).unwrap();
        assert_eq!(result.hints.len(), 2);
        assert_eq!(result.hints[0].groupdict.get("name").unwrap(), "a");
        assert_eq!(result.hints[1].text, "b=22");

        opts.regex = r"(\w+)=(\d+)".to_owned();
        let result = generate_hints("a=1 b=22", &opts).unwrap();
        assert_eq!(result.hints[0].text, "a");
        assert_eq!(result.hints[1].text, "b");
    }

    #[test]
    fn default_regex_is_line_matching_and_invalid_regex_errors() {
        let result = generate_hints(" one \n\n two \n", &options(HintType::Regex)).unwrap();
        assert_eq!(
            result
                .hints
                .iter()
                .map(|h| h.text.as_str())
                .collect::<Vec<_>>(),
            ["one", "two"]
        );
        let mut opts = options(HintType::Regex);
        opts.regex = "(".to_owned();
        assert!(matches!(
            generate_hints("x", &opts),
            Err(HintError::InvalidRegex(_))
        ));
    }

    #[test]
    fn hyperlinks_are_extracted_from_osc8() {
        let text = "\x1b]8;id=42;https://example.com\x1b\\click\x1b]8;;\x1b\\";
        let result = generate_hints(text, &options(HintType::Hyperlink)).unwrap();
        assert_eq!(result.hints.len(), 1);
        assert_eq!(result.hints[0].text, "https://example.com");
        assert_eq!(result.hints[0].group_id.as_deref(), Some("42"));
        assert!(result.hints[0].is_hyperlink);
    }

    #[test]
    fn no_matches_and_short_matches_are_errors() {
        assert!(matches!(
            generate_hints("no links", &options(HintType::Url)),
            Err(HintError::NoMatches { .. })
        ));
        let opts = HintOptions {
            minimum_match_length: 30,
            ..options(HintType::Word)
        };
        assert!(matches!(
            generate_hints("short words", &opts),
            Err(HintError::NoMatches { .. })
        ));
    }

    #[test]
    fn multiple_joiner_modes() {
        let result = generate_hints("alpha beta", &options(HintType::Word)).unwrap();
        let selected: Vec<&Hint> = result.hints.iter().collect();
        let mut opts = options(HintType::Word);
        opts.multiple_joiner = "auto".to_owned();
        assert_eq!(join_hints(&selected, &opts), "alpha beta");
        opts.multiple_joiner = "json".to_owned();
        assert_eq!(
            join_hints(&selected, &opts),
            "[\n\t\"alpha\",\n\t\"beta\"\n]"
        );
        opts.multiple_joiner = "0".to_owned();
        assert_eq!(join_hints(&selected, &opts), "alpha");
        opts.multiple_joiner = "-1".to_owned();
        assert_eq!(join_hints(&selected, &opts), "beta");
    }

    #[test]
    fn launch_plans_for_copy_paste_open_and_groups() {
        let mut opts = options(HintType::Word);
        opts.multiple = true;
        let result = generate_hints("alpha beta", &opts).unwrap();
        let selected: Vec<&Hint> = result.hints.iter().collect();
        opts.programs = vec!["@".to_owned(), "-".to_owned()];
        let plans = build_launch_plans(&selected, &opts).unwrap();
        assert_eq!(plans[0].action, HintAction::Clipboard);
        assert_eq!(plans[0].text, "alpha  beta ");
        assert_eq!(plans[1].action, HintAction::Paste);

        let mut regex_opts = options(HintType::Regex);
        regex_opts.regex = r"(?P<name>\w+)=(\d+)".to_owned();
        regex_opts.programs = vec!["echo".to_owned()];
        let result = generate_hints("a=1", &regex_opts).unwrap();
        let plans = build_launch_plans(&[&result.hints[0]], &regex_opts).unwrap();
        assert_eq!(plans[0].program, "echo");
        assert!(plans[0].args.contains(&"name=a".to_owned()));
    }

    #[test]
    fn parses_cli_options_and_rejects_bad_values() {
        let cmd = parse_hints_args(&args(&[
            "--type",
            "regex",
            "--regex=foo",
            "--alphabet=ab",
            "--multiple",
            "--program",
            "@",
            "--select",
            "a",
            "--history",
            "hist.txt",
            "text",
        ]))
        .unwrap();
        assert_eq!(cmd.options.hint_type, HintType::Regex);
        assert_eq!(cmd.options.regex, "foo");
        assert!(cmd.options.multiple);
        assert_eq!(cmd.options.programs, ["@"]);
        assert_eq!(cmd.selects, ["a"]);
        assert_eq!(cmd.text, ["text"]);
        assert!(matches!(
            parse_hints_args(&args(&["--type=nope"])),
            Err(HintError::InvalidOption { .. })
        ));
        assert!(matches!(
            parse_hints_args(&args(&["--hints-offset=x"])),
            Err(HintError::InvalidOption { .. })
        ));
    }

    #[test]
    fn run_cli_lists_and_selects_labels_without_ui() {
        let mut out = Vec::new();
        run_hints_cli(
            &args(&["--type=word", "--text=alpha beta"]),
            &mut io::empty(),
            &mut out,
        )
        .unwrap();
        let listing = String::from_utf8(out).unwrap();
        assert!(listing.contains("alpha"));
        assert!(listing.contains("beta"));

        let mut out = Vec::new();
        run_hints_cli(
            &args(&[
                "--type=word",
                "--text=alpha beta",
                "--program=@",
                "--select=2",
            ]),
            &mut io::empty(),
            &mut out,
        )
        .unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "clipboard\talpha\n");
    }
}
