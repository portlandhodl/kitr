// SPDX-License-Identifier: GPL-3.0-or-later
//! Rust-native `kitten unicode_input` data engine.
//!
//! # Scope and UI gap
//!
//! The original kitten is a full-screen terminal UI. This port implements the
//! reusable model behind it: a checked-in Unicode name table, name/code/
//! emoticon/favorites search, a codepoint model, favorites parsing and
//! serialization, recent-history deduplication, emoji variation selectors, and
//! an explicit key-event abstraction. It does not draw the table or manage an
//! alternate screen. The `kitten unicode_input` command therefore offers a
//! non-interactive query/list/emit interface instead of the Python/Go overlay.
//!
//! UI rendering remains a Python/Go kitten concern until kitty's UI toolkit is
//! available to Rust kittens.

use std::fmt;
use std::fs;
use std::io::{self, IsTerminal, Read, Write};
use std::path::Path;

use crate::unicode_table::ENTRIES;

/// Base used by the table's short numeric indices.
pub const INDEX_BASE: u32 = 36;

/// Character used to prefix a table index.
pub const INDEX_CHAR: char = '.';

/// Maximum number of recent characters kept in history.
pub const DEFAULT_HISTORY_LENGTH: usize = 128;

/// Symbols shown in the default (code) tab, mirroring the Go kitten.
pub const DEFAULT_SYMBOLS: &str = "‘’“”‹›«»‚„ 😀😛😇😈😉😍😎😮👍👎 —–§¶†‡©®™ →⇒•·°±−×÷¼½½¾ …µ¢£€¿¡¨´¸ˆ˜ ÀÁÂÃÄÅÆÇÈÉÊË ÌÍÎÏÐÑÒÓÔÕÖØ ŒŠÙÚÛÜÝŸÞßàá âãäåæçèéêëìí îïðñòóôõöøœš ùúûüýÿþªºαΩ∞";

/// Usage text for `kitten unicode_input`.
pub const UNICODE_INPUT_USAGE: &str = "\
Usage: kitten unicode_input [options]

Query Unicode characters by name or code point. No full-screen UI is drawn by
the Rust engine; use --list to inspect matches.

Options:
      --tab MODE            previous, code, name, emoticons or favorites
      --query TEXT          Search by Unicode name (or U+XXXX)
      --codepoint HEX       Select a hexadecimal code point
      --select N            Select a zero-based candidate index
      --emoji-variation V   none, graphic or text
      --favorites FILE      Read favorites from FILE
      --list                List candidates instead of emitting
      --emit                Emit the selected character (the default)
  -h, --help                Show this help

The interactive table/overlay is not implemented in Rust; only the search and
selection engine and a non-interactive command are available.";

/// An entry in the checked-in name table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UnicodeEntry {
    pub codepoint: char,
    pub name: &'static str,
}

/// All named code points in the checked-in table.
pub fn entries() -> impl Iterator<Item = UnicodeEntry> {
    ENTRIES.iter().map(|(codepoint, name)| UnicodeEntry {
        codepoint: char::from_u32(*codepoint).unwrap_or('\u{fffd}'),
        name,
    })
}

/// Return the Unicode name for a code point, when the table contains it.
pub fn name_for(codepoint: char) -> Option<&'static str> {
    let value = codepoint as u32;
    ENTRIES
        .binary_search_by_key(&value, |(cp, _)| *cp)
        .ok()
        .map(|index| ENTRIES[index].1)
}

/// Which search model is active.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Hex,
    Name,
    Emoticons,
    Favorites,
}

impl Mode {
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "code" | "hex" => Self::Hex,
            "name" => Self::Name,
            "emoticons" | "emoji" => Self::Emoticons,
            "favorites" | "favourite" | "favourites" => Self::Favorites,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hex => "code",
            Self::Name => "name",
            Self::Emoticons => "emoticons",
            Self::Favorites => "favorites",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::Hex => "F1",
            Self::Name => "F2",
            Self::Emoticons => "F3",
            Self::Favorites => "F4",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Hex => Self::Name,
            Self::Name => Self::Emoticons,
            Self::Emoticons => Self::Favorites,
            Self::Favorites => Self::Hex,
        }
    }

    pub fn previous(self) -> Self {
        match self {
            Self::Hex => Self::Favorites,
            Self::Name => Self::Hex,
            Self::Emoticons => Self::Name,
            Self::Favorites => Self::Emoticons,
        }
    }
}

/// Emoji variation selector policy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EmojiVariation {
    #[default]
    None,
    Graphic,
    Text,
}

impl EmojiVariation {
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "none" => Self::None,
            "graphic" | "emoji" => Self::Graphic,
            "text" => Self::Text,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Graphic => "graphic",
            Self::Text => "text",
        }
    }
}

/// Errors produced by the Unicode input engine.
#[derive(Debug)]
#[non_exhaustive]
pub enum UnicodeInputError {
    Io(io::Error),
    InvalidOption { option: &'static str, value: String },
    InvalidCodepoint(String),
    InvalidIndex(String),
    NoSelection,
}

impl fmt::Display for UnicodeInputError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "I/O error: {err}"),
            Self::InvalidOption { option, value } => {
                write!(f, "invalid value for {option}: {value:?}")
            }
            Self::InvalidCodepoint(value) => write!(f, "invalid Unicode code point: {value:?}"),
            Self::InvalidIndex(value) => write!(f, "invalid candidate index: {value:?}"),
            Self::NoSelection => f.write_str("no Unicode character is selected"),
        }
    }
}

impl std::error::Error for UnicodeInputError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<io::Error> for UnicodeInputError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

/// Whether a code point can be selected.
pub fn is_valid_codepoint(codepoint: char) -> bool {
    let value = codepoint as u32;
    !(value <= 32
        || value == 127
        || (128..=159).contains(&value)
        || (0xd800..=0xdfff).contains(&value)
        || value > char::MAX as u32)
}

/// Parse a hexadecimal code point, with or without a `U+`/`0x` prefix.
pub fn parse_codepoint(value: &str) -> Result<char, UnicodeInputError> {
    let trimmed = value.trim();
    let digits = trimmed
        .strip_prefix("U+")
        .or_else(|| trimmed.strip_prefix("u+"))
        .or_else(|| trimmed.strip_prefix("0x"))
        .or_else(|| trimmed.strip_prefix("0X"))
        .unwrap_or(trimmed);
    let codepoint = u32::from_str_radix(digits, 16)
        .ok()
        .and_then(char::from_u32)
        .ok_or_else(|| UnicodeInputError::InvalidCodepoint(value.to_owned()))?;
    if !is_valid_codepoint(codepoint) {
        return Err(UnicodeInputError::InvalidCodepoint(value.to_owned()));
    }
    Ok(codepoint)
}

/// Encode a zero-based table index in base 36.
pub fn encode_index(index: usize) -> String {
    if index == 0 {
        return "0".to_owned();
    }
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut value = index;
    let mut out = Vec::new();
    while value > 0 {
        out.push(DIGITS[value % 36]);
        value /= 36;
    }
    out.reverse();
    String::from_utf8(out).expect("base36 is ASCII")
}

/// Decode a base 36 table index.
pub fn decode_index(text: &str) -> Option<usize> {
    let text = text.trim_start_matches(INDEX_CHAR);
    usize::from_str_radix(text, 36).ok()
}

/// The default symbol set.
pub fn default_symbols() -> Vec<char> {
    DEFAULT_SYMBOLS
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect()
}

/// The emoticon range used by the F3 tab.
pub fn emoticons() -> Vec<char> {
    (0x1f600..=0x1f64f).filter_map(char::from_u32).collect()
}

fn query_words(query: &str) -> Vec<String> {
    query
        .split_whitespace()
        .filter(|word| *word != ".")
        .map(|word| word.to_ascii_uppercase())
        .collect()
}

/// Search the built-in names. Every query word must occur in the name.
pub fn search_by_name(query: &str) -> Vec<char> {
    let query = query.trim();
    if query.is_empty() {
        return Vec::new();
    }
    if let Ok(codepoint) = parse_codepoint(query) {
        return vec![codepoint];
    }
    let words = query_words(query);
    if words.is_empty() {
        return Vec::new();
    }
    ENTRIES
        .iter()
        .filter(|(_, name)| {
            let upper = name.to_ascii_uppercase();
            words.iter().all(|word| upper.contains(word))
        })
        .filter_map(|(cp, _)| char::from_u32(*cp))
        .collect()
}

/// Parse a favorites file.
pub fn parse_favorites(raw: &str) -> Vec<char> {
    let mut ans = Vec::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.split_once('#').map(|(left, _)| left).unwrap_or(line);
        let code_text = line.split_whitespace().next().unwrap_or("");
        if let Ok(codepoint) = u32::from_str_radix(code_text, 16)
            .ok()
            .and_then(char::from_u32)
            .filter(|ch| is_valid_codepoint(*ch))
            .ok_or(())
        {
            if !ans.contains(&codepoint) {
                ans.push(codepoint);
            }
        }
    }
    ans
}

/// Serialize favorites in the format understood by the original kitten.
pub fn serialize_favorites(favorites: &[char]) -> String {
    let mut out = String::from(
        "# Favorite characters for unicode input\n\
         # Enter the hex code for each favorite character on a new line. Blank lines are\n\
         # ignored and anything after a # is considered a comment.\n\n",
    );
    for ch in favorites {
        out.push_str(&format!(
            "{:x} # {} {}\n",
            *ch as u32,
            ch,
            name_for(*ch).unwrap_or("UNKNOWN")
        ));
    }
    out
}

/// Load favorites from a file.
pub fn load_favorites(path: &Path) -> Result<Vec<char>, UnicodeInputError> {
    let raw = fs::read_to_string(path)?;
    Ok(parse_favorites(&raw))
}

/// Save favorites to a file.
pub fn save_favorites(path: &Path, favorites: &[char]) -> Result<(), UnicodeInputError> {
    fs::write(path, serialize_favorites(favorites))?;
    Ok(())
}

/// Whether a character has an emoji presentation by default.
pub fn is_emoji_presentation_base(ch: char) -> bool {
    let value = ch as u32;
    matches!(
        value,
        0x231a..=0x231b
            | 0x23e9..=0x23ec
            | 0x23f0
            | 0x23f3
            | 0x25fd..=0x25fe
            | 0x2614..=0x2615
            | 0x2648..=0x2653
            | 0x267f
            | 0x2693
            | 0x26a1
            | 0x26aa..=0x26ab
            | 0x26bd..=0x26be
            | 0x26c4..=0x26c5
            | 0x26ce
            | 0x26d4
            | 0x26ea
            | 0x26f2..=0x26f3
            | 0x26f5
            | 0x26fa
            | 0x26fd
            | 0x2705
            | 0x270a..=0x270b
            | 0x2728
            | 0x274c
            | 0x274e
            | 0x2753..=0x2755
            | 0x2757
            | 0x2795..=0x2797
            | 0x27b0
            | 0x27bf
            | 0x2b1b..=0x2b1c
            | 0x2b50
            | 0x2b55
            | 0x1f300..=0x1faff
    )
}

/// Apply the emoji variation selector, if any.
pub fn resolve_char(ch: char, variation: EmojiVariation) -> String {
    let mut out = String::new();
    out.push(ch);
    if is_emoji_presentation_base(ch) {
        match variation {
            EmojiVariation::None => {}
            EmojiVariation::Graphic => out.push('\u{fe0f}'),
            EmojiVariation::Text => out.push('\u{fe0e}'),
        }
    }
    out
}

/// Key events understood by the engine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Backspace,
    Enter,
    Escape,
    Tab,
    BackTab,
    Up,
    Down,
    Left,
    Right,
    F(u8),
    Ctrl(char),
}

/// Result of feeding a key to the state machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyOutcome {
    Continue,
    Emit(String),
    Cancel,
    ModeChanged(Mode),
}

/// The interactive Unicode input model, independent of rendering.
#[derive(Clone, Debug)]
pub struct UnicodeInputState {
    mode: Mode,
    query: String,
    candidates: Vec<char>,
    selected: usize,
    favorites: Vec<char>,
    history: Vec<char>,
    emoji_variation: EmojiVariation,
    current: Option<char>,
    history_limit: usize,
}

impl Default for UnicodeInputState {
    fn default() -> Self {
        Self::new()
    }
}

impl UnicodeInputState {
    pub fn new() -> Self {
        let mut state = Self {
            mode: Mode::Hex,
            query: String::new(),
            candidates: Vec::new(),
            selected: 0,
            favorites: default_symbols(),
            history: default_symbols(),
            emoji_variation: EmojiVariation::None,
            current: None,
            history_limit: DEFAULT_HISTORY_LENGTH,
        };
        state.refresh();
        state
    }

    pub fn with_favorites(favorites: Vec<char>) -> Self {
        let mut state = Self::new();
        if !favorites.is_empty() {
            state.favorites = favorites;
        }
        state.set_mode(Mode::Favorites);
        state
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
        self.query.clear();
        self.selected = 0;
        self.refresh();
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn set_query(&mut self, query: impl Into<String>) {
        self.query = query.into();
        self.selected = 0;
        self.refresh();
    }

    pub fn set_emoji_variation(&mut self, variation: EmojiVariation) {
        self.emoji_variation = variation;
        self.refresh();
    }

    pub fn emoji_variation(&self) -> EmojiVariation {
        self.emoji_variation
    }

    pub fn candidates(&self) -> &[char] {
        &self.candidates
    }

    pub fn selected_index(&self) -> usize {
        self.selected
    }

    pub fn current(&self) -> Option<char> {
        self.current
    }

    pub fn favorites(&self) -> &[char] {
        &self.favorites
    }

    pub fn set_favorites(&mut self, favorites: Vec<char>) {
        self.favorites = favorites;
        if self.mode == Mode::Favorites {
            self.refresh();
        }
    }

    pub fn history(&self) -> &[char] {
        &self.history
    }

    pub fn history_limit(&self) -> usize {
        self.history_limit
    }

    pub fn set_history_limit(&mut self, limit: usize) {
        self.history_limit = limit.max(1);
        self.history.truncate(self.history_limit);
    }

    pub fn select_index(&mut self, index: usize) -> Result<char, UnicodeInputError> {
        if index >= self.candidates.len() {
            return Err(UnicodeInputError::InvalidIndex(index.to_string()));
        }
        self.selected = index;
        self.current = Some(self.candidates[index]);
        Ok(self.candidates[index])
    }

    /// The selected character with the emoji variation selector applied.
    pub fn resolved_current(&self) -> Option<String> {
        self.current
            .map(|ch| resolve_char(ch, self.emoji_variation))
    }

    /// Commit the current character and add it to the recent history.
    pub fn commit(&mut self) -> Option<String> {
        let ch = self.current?;
        let resolved = resolve_char(ch, self.emoji_variation);
        self.record_history(ch);
        Some(resolved)
    }

    /// Add a character to the front of the history, deduplicating it.
    pub fn record_history(&mut self, ch: char) {
        self.history.retain(|existing| *existing != ch);
        self.history.insert(0, ch);
        self.history.truncate(self.history_limit);
    }

    /// Remove all recent-history entries.
    pub fn clear_history(&mut self) {
        self.history.clear();
    }

    pub fn handle_text(&mut self, text: &str) -> KeyOutcome {
        for ch in text.chars() {
            match self.handle_key(Key::Char(ch)) {
                KeyOutcome::Continue => {}
                other => return other,
            }
        }
        KeyOutcome::Continue
    }

    /// Feed one key event to the model.
    pub fn handle_key(&mut self, key: Key) -> KeyOutcome {
        match key {
            Key::Escape => KeyOutcome::Cancel,
            Key::Ctrl('c') => KeyOutcome::Cancel,
            Key::F(1) | Key::Ctrl('1') => {
                self.set_mode(Mode::Hex);
                KeyOutcome::ModeChanged(Mode::Hex)
            }
            Key::F(2) | Key::Ctrl('2') => {
                self.set_mode(Mode::Name);
                KeyOutcome::ModeChanged(Mode::Name)
            }
            Key::F(3) | Key::Ctrl('3') => {
                self.set_mode(Mode::Emoticons);
                KeyOutcome::ModeChanged(Mode::Emoticons)
            }
            Key::F(4) | Key::Ctrl('4') => {
                self.set_mode(Mode::Favorites);
                KeyOutcome::ModeChanged(Mode::Favorites)
            }
            Key::Ctrl(']') => {
                let mode = self.mode.next();
                self.set_mode(mode);
                KeyOutcome::ModeChanged(mode)
            }
            Key::Ctrl('[') => {
                let mode = self.mode.previous();
                self.set_mode(mode);
                KeyOutcome::ModeChanged(mode)
            }
            Key::Enter => match self.commit() {
                Some(value) => KeyOutcome::Emit(value),
                None => KeyOutcome::Continue,
            },
            Key::Backspace => {
                self.query.pop();
                self.selected = 0;
                self.refresh();
                KeyOutcome::Continue
            }
            Key::Left | Key::BackTab => {
                self.move_selection(-1, 0);
                KeyOutcome::Continue
            }
            Key::Right => {
                self.move_selection(1, 0);
                KeyOutcome::Continue
            }
            Key::Up => {
                if self.mode == Mode::Hex {
                    self.adjust_hex(1);
                } else {
                    self.move_selection(0, -1);
                }
                KeyOutcome::Continue
            }
            Key::Down => {
                if self.mode == Mode::Hex {
                    self.adjust_hex(-1);
                } else {
                    self.move_selection(0, 1);
                }
                KeyOutcome::Continue
            }
            Key::Tab => {
                if self.mode == Mode::Hex {
                    self.adjust_hex(16);
                } else {
                    self.move_selection(1, 0);
                }
                KeyOutcome::Continue
            }
            Key::Char(ch) => {
                if ch.is_control() {
                    return KeyOutcome::Continue;
                }
                let acceptable = match self.mode {
                    Mode::Hex => ch.is_ascii_hexdigit() || ch == INDEX_CHAR,
                    Mode::Name => true,
                    Mode::Emoticons | Mode::Favorites => {
                        ch.is_ascii_alphanumeric() || ch == INDEX_CHAR
                    }
                };
                if acceptable {
                    self.query.push(ch);
                    self.selected = 0;
                    self.refresh();
                }
                KeyOutcome::Continue
            }
            _ => KeyOutcome::Continue,
        }
    }

    fn adjust_hex(&mut self, delta: i64) {
        let value = u32::from_str_radix(self.query.trim_start_matches(INDEX_CHAR), 16)
            .ok()
            .map(|value| value as i64 + delta)
            .unwrap_or(0)
            .clamp(32, char::MAX as i64) as u32;
        self.query = format!("{value:x}");
        self.refresh();
    }

    fn move_selection(&mut self, columns: isize, rows: isize) {
        if self.candidates.is_empty() {
            return;
        }
        let len = self.candidates.len() as isize;
        if columns != 0 {
            self.selected = ((self.selected as isize + columns).rem_euclid(len)) as usize;
        }
        if rows != 0 {
            self.selected = (self.selected as isize + rows).clamp(0, len - 1) as usize;
        }
        self.current = self.candidates.get(self.selected).copied();
    }

    fn refresh(&mut self) {
        match self.mode {
            Mode::Hex => {
                self.candidates = if self.history.is_empty() {
                    default_symbols()
                } else {
                    self.history.clone()
                };
                self.current = current_hex(&self.query, &self.candidates);
            }
            Mode::Name => {
                if self.query.trim().is_empty() {
                    self.candidates.clear();
                } else {
                    self.candidates = search_by_name(&self.query);
                }
                if self.selected >= self.candidates.len() {
                    self.selected = 0;
                }
                self.current = self.candidates.get(self.selected).copied();
                if !self.current.is_some_and(is_valid_codepoint) {
                    self.current = None;
                }
            }
            Mode::Emoticons => {
                self.candidates = emoticons();
                self.current = current_from_index(&self.query, &self.candidates);
            }
            Mode::Favorites => {
                self.candidates = self.favorites.clone();
                self.current = current_from_index(&self.query, &self.candidates);
            }
        }
        if self.mode != Mode::Name {
            self.selected = self.selected.min(self.candidates.len().saturating_sub(1));
        }
    }
}

fn current_hex(query: &str, candidates: &[char]) -> Option<char> {
    let query = query.trim();
    if query.is_empty() {
        return None;
    }
    if let Some(index_text) = query.strip_prefix(INDEX_CHAR) {
        if index_text.is_empty() {
            return None;
        }
        return decode_index(index_text).and_then(|index| candidates.get(index).copied());
    }
    parse_codepoint(query).ok()
}

fn current_from_index(query: &str, candidates: &[char]) -> Option<char> {
    let trimmed = query.trim_start_matches(INDEX_CHAR);
    if trimmed.is_empty() {
        return candidates.first().copied();
    }
    decode_index(trimmed).and_then(|index| candidates.get(index).copied())
}

/// Parsed command for the non-interactive unicode_input command.
#[derive(Clone, Debug)]
pub struct UnicodeInputCommand {
    pub tab: Mode,
    pub query: Option<String>,
    pub codepoint: Option<String>,
    pub select: Option<usize>,
    pub emoji_variation: EmojiVariation,
    pub favorites_path: Option<std::path::PathBuf>,
    pub list: bool,
    pub emit: bool,
    pub help: bool,
}

impl Default for UnicodeInputCommand {
    fn default() -> Self {
        Self {
            tab: Mode::Hex,
            query: None,
            codepoint: None,
            select: None,
            emoji_variation: EmojiVariation::None,
            favorites_path: None,
            list: false,
            emit: false,
            help: false,
        }
    }
}

/// Parse arguments after `kitten unicode_input`.
pub fn parse_unicode_input_args(args: &[String]) -> Result<UnicodeInputCommand, UnicodeInputError> {
    let mut cmd = UnicodeInputCommand::default();
    let mut index = 0usize;
    while index < args.len() {
        let arg = &args[index];
        let (name, inline) = match arg.split_once('=') {
            Some((name, value)) => (name, Some(value.to_owned())),
            None => (arg.as_str(), None),
        };
        let mut take = |option: &'static str| -> Result<String, UnicodeInputError> {
            if let Some(value) = &inline {
                return Ok(value.clone());
            }
            index += 1;
            args.get(index)
                .cloned()
                .ok_or_else(|| UnicodeInputError::InvalidOption {
                    option,
                    value: String::new(),
                })
        };
        match name {
            "-h" | "--help" => cmd.help = true,
            "--tab" => {
                let value = take("--tab")?;
                if value == "previous" {
                    cmd.tab = Mode::Hex;
                } else {
                    cmd.tab = Mode::parse(&value).ok_or({
                        UnicodeInputError::InvalidOption {
                            option: "--tab",
                            value,
                        }
                    })?;
                }
            }
            "--query" => cmd.query = Some(take("--query")?),
            "--codepoint" | "--code" => cmd.codepoint = Some(take("--codepoint")?),
            "--select" => {
                let value = take("--select")?;
                cmd.select = Some(
                    value
                        .parse()
                        .map_err(|_| UnicodeInputError::InvalidOption {
                            option: "--select",
                            value,
                        })?,
                );
            }
            "--emoji-variation" => {
                let value = take("--emoji-variation")?;
                cmd.emoji_variation = EmojiVariation::parse(&value).ok_or({
                    UnicodeInputError::InvalidOption {
                        option: "--emoji-variation",
                        value,
                    }
                })?;
            }
            "--favorites" => {
                cmd.favorites_path = Some(std::path::PathBuf::from(take("--favorites")?))
            }
            "--list" => cmd.list = true,
            "--emit" => cmd.emit = true,
            other => {
                return Err(UnicodeInputError::InvalidOption {
                    option: "argument",
                    value: other.to_owned(),
                })
            }
        }
        index += 1;
    }
    Ok(cmd)
}

/// Run the non-interactive unicode_input command.
pub fn run_unicode_input_cli(
    args: &[String],
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
) -> Result<(), UnicodeInputError> {
    let cmd = parse_unicode_input_args(args)?;
    if cmd.help {
        writeln!(stdout, "{UNICODE_INPUT_USAGE}")?;
        return Ok(());
    }
    let mut state = UnicodeInputState::new();
    state.set_emoji_variation(cmd.emoji_variation);
    if let Some(path) = &cmd.favorites_path {
        state.set_favorites(load_favorites(path)?);
    }
    state.set_mode(cmd.tab);
    if let Some(codepoint) = &cmd.codepoint {
        let ch = parse_codepoint(codepoint)?;
        state.query = format!("{:x}", ch as u32);
        state.refresh();
        state.current = Some(ch);
    } else if let Some(query) = &cmd.query {
        state.set_query(query.clone());
    } else if !io::stdin().is_terminal() {
        let mut query = String::new();
        stdin.read_to_string(&mut query)?;
        if !query.trim().is_empty() {
            state.set_query(query.trim().to_owned());
        }
    }
    if let Some(select) = cmd.select {
        state.select_index(select)?;
    }
    if cmd.list || (!cmd.emit && state.current().is_none()) {
        for (index, ch) in state.candidates().iter().enumerate() {
            if index >= 200 {
                break;
            }
            writeln!(
                stdout,
                "{}\tU+{:04X}\t{}\t{}",
                index,
                *ch as u32,
                ch,
                name_for(*ch).unwrap_or("UNKNOWN")
            )?;
        }
        return Ok(());
    }
    let selected = state.commit().ok_or(UnicodeInputError::NoSelection)?;
    writeln!(stdout, "{selected}")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|x| (*x).to_owned()).collect()
    }

    #[test]
    fn table_names_are_available_and_sorted() {
        assert_eq!(name_for('A'), Some("LATIN CAPITAL LETTER A"));
        assert_eq!(name_for('😀'), Some("GRINNING FACE"));
        assert_eq!(name_for('\u{10ffff}'), None);
        assert!(entries().count() > 3000);
        let mut previous = 0;
        for entry in entries() {
            assert!(entry.codepoint as u32 > previous);
            previous = entry.codepoint as u32;
        }
    }

    #[test]
    fn name_search_finds_common_characters() {
        let grinning = search_by_name("grinning face");
        assert_eq!(grinning.first().copied(), Some('😀'));
        let latin_a = search_by_name("latin capital letter a");
        assert_eq!(latin_a.first().copied(), Some('A'));
        assert!(search_by_name("no such character name").is_empty());
        assert_eq!(search_by_name("U+1F600"), vec!['😀']);
    }

    #[test]
    fn codepoint_parsing_and_validation() {
        assert_eq!(parse_codepoint("1f600").unwrap(), '😀');
        assert_eq!(parse_codepoint("U+0041").unwrap(), 'A');
        assert_eq!(parse_codepoint("0x41").unwrap(), 'A');
        assert!(parse_codepoint("zz").is_err());
        assert!(parse_codepoint("110000").is_err());
        assert!(parse_codepoint("20").is_err());
        assert!(parse_codepoint("d800").is_err());
        assert!(is_valid_codepoint('A'));
        assert!(!is_valid_codepoint(' '));
    }

    #[test]
    fn favorites_round_trip_and_comments() {
        let raw = "# comment\n41 # A\n\n1f600 # smile\nzz\n41\n";
        let favorites = parse_favorites(raw);
        assert_eq!(favorites, vec!['A', '😀']);
        let serialized = serialize_favorites(&favorites);
        assert!(serialized.contains("41 # A LATIN CAPITAL LETTER A"));
        assert_eq!(parse_favorites(&serialized), favorites);
    }

    #[test]
    fn history_deduplicates_and_respects_limit() {
        let mut state = UnicodeInputState::new();
        state.set_history_limit(3);
        state.clear_history();
        state.record_history('a');
        state.record_history('b');
        state.record_history('a');
        assert_eq!(state.history(), &['a', 'b']);
        state.record_history('c');
        state.record_history('d');
        assert_eq!(state.history(), &['d', 'c', 'a']);
    }

    #[test]
    fn name_mode_key_handling_selects_and_emits() {
        let mut state = UnicodeInputState::new();
        state.set_mode(Mode::Name);
        state.set_query("grinning face");
        assert_eq!(state.current(), Some('😀'));
        let outcome = state.handle_key(Key::Enter);
        assert_eq!(outcome, KeyOutcome::Emit("😀".to_owned()));
        state.set_query("latin capital letter a");
        assert!(state.candidates().len() > 1);
        let first = state.current();
        state.handle_key(Key::Right);
        assert_ne!(state.current(), first);
        assert!(matches!(state.handle_key(Key::Escape), KeyOutcome::Cancel));
        assert!(matches!(
            state.handle_key(Key::F(4)),
            KeyOutcome::ModeChanged(Mode::Favorites)
        ));
        assert_eq!(state.mode(), Mode::Favorites);
    }

    #[test]
    fn hex_mode_accepts_codes_and_index_hints() {
        let mut state = UnicodeInputState::new();
        state.handle_text("1f600");
        assert_eq!(state.current(), Some('😀'));
        state.set_query(".");
        assert_eq!(state.current(), None);
        state.set_query(".0");
        assert_eq!(state.current(), state.candidates().first().copied());
        state.set_query(format!(".{}", encode_index(2)));
        assert_eq!(state.current(), state.candidates().get(2).copied());
        state.set_query("41");
        state.handle_key(Key::Up);
        assert_eq!(state.query(), "42");
        assert_eq!(state.current(), Some('B'));
        state.handle_key(Key::Down);
        assert_eq!(state.query(), "41");
    }

    #[test]
    fn emoji_variation_and_commit_history() {
        let mut state = UnicodeInputState::new();
        state.set_mode(Mode::Name);
        state.set_query("grinning face");
        state.set_emoji_variation(EmojiVariation::Text);
        assert_eq!(state.resolved_current().unwrap(), "😀\u{fe0e}");
        let emitted = state.commit().unwrap();
        assert_eq!(emitted, "😀\u{fe0e}");
        assert_eq!(state.history().first(), Some(&'😀'));
        state.set_emoji_variation(EmojiVariation::Graphic);
        assert_eq!(resolve_char('A', EmojiVariation::Graphic), "A");
        assert_eq!(resolve_char('😀', EmojiVariation::Graphic), "😀\u{fe0f}");
    }

    #[test]
    fn index_encoding_round_trips() {
        for index in [0, 1, 9, 10, 35, 36, 1000] {
            assert_eq!(decode_index(&encode_index(index)), Some(index));
        }
        assert_eq!(decode_index(".10"), Some(36));
        assert_eq!(decode_index("!"), None);
    }

    #[test]
    fn cli_emits_and_lists() {
        let mut out = Vec::new();
        run_unicode_input_cli(&args(&["--codepoint=1f600"]), &mut io::empty(), &mut out).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "😀\n");

        let mut out = Vec::new();
        run_unicode_input_cli(
            &args(&["--tab=name", "--query=grinning face", "--list"]),
            &mut io::empty(),
            &mut out,
        )
        .unwrap();
        assert!(String::from_utf8(out).unwrap().contains("GRINNING FACE"));
    }

    #[test]
    fn cli_rejects_bad_arguments() {
        assert!(matches!(
            parse_unicode_input_args(&args(&["--tab=nope"])),
            Err(UnicodeInputError::InvalidOption { .. })
        ));
        let err = run_unicode_input_cli(
            &args(&["--codepoint=zz"]),
            &mut io::empty(),
            &mut Vec::new(),
        )
        .unwrap_err();
        assert!(matches!(err, UnicodeInputError::InvalidCodepoint(_)));
        let err = run_unicode_input_cli(
            &args(&["--codepoint=20"]),
            &mut io::empty(),
            &mut Vec::new(),
        )
        .unwrap_err();
        assert!(matches!(err, UnicodeInputError::InvalidCodepoint(_)));
    }
}
