// SPDX-License-Identifier: GPL-3.0-or-later
//! Command-palette kitten, ported from `kittens/command_palette`.
//!
//! Keybindings, mouse bindings and unmapped actions are represented as
//! [`Binding`] records grouped by mode and category. Search is the Go kitten's
//! word-aware matcher (exact/prefix/edit-distance, with compound-token
//! handling) and the UI returns the selected action definition.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::time::Duration;

use kitty_remote::json::{self, Value};

use crate::tui::{self, Key, KeyParser, Tty};

pub const COMMAND_PALETTE_USAGE: &str = "\
Usage: kitten command-palette

Browse and trigger keyboard shortcuts and actions.";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Binding {
    pub key: String,
    pub action: String,
    pub action_display: String,
    pub definition: String,
    pub help: String,
    pub long_help: String,
    pub alias: String,
    pub category: String,
    pub mode: String,
    pub is_mouse: bool,
}

#[derive(Clone, Debug, Default)]
pub struct InputData {
    /// mode -> category -> bindings
    pub modes: BTreeMap<String, BTreeMap<String, Vec<Binding>>>,
    pub mouse: Vec<Binding>,
    pub mode_order: Vec<String>,
    pub category_order: BTreeMap<String, Vec<String>>,
}

impl InputData {
    pub fn from_value(value: &Value) -> Result<Self, String> {
        let mut out = Self::default();
        if let Some(modes) = value.get("modes").and_then(Value::as_object) {
            for (mode, categories) in modes {
                let mut cat_map = BTreeMap::new();
                if let Some(categories) = categories.as_object() {
                    for (category, bindings) in categories {
                        let mut list = Vec::new();
                        if let Some(bindings) = bindings.as_array() {
                            for binding in bindings {
                                let mut item = Binding::default();
                                if let Some(object) = binding.as_object() {
                                    for (key, value) in object {
                                        let text = value.as_str().unwrap_or("");
                                        match key.as_str() {
                                            "key" => item.key = text.to_owned(),
                                            "action" => item.action = text.to_owned(),
                                            "action_display" => {
                                                item.action_display = text.to_owned()
                                            }
                                            "definition" => item.definition = text.to_owned(),
                                            "help" => item.help = text.to_owned(),
                                            "long_help" => item.long_help = text.to_owned(),
                                            "alias" => item.alias = text.to_owned(),
                                            _ => {}
                                        }
                                    }
                                }
                                item.category = category.clone();
                                item.mode = mode.clone();
                                list.push(item);
                            }
                        }
                        cat_map.insert(category.clone(), list);
                    }
                }
                out.modes.insert(mode.clone(), cat_map);
            }
        }
        if let Some(mouse) = value.get("mouse").and_then(Value::as_array) {
            for binding in mouse {
                let mut item = Binding::default();
                if let Some(object) = binding.as_object() {
                    for (key, value) in object {
                        let text = value.as_str().unwrap_or("");
                        match key.as_str() {
                            "key" => item.key = text.to_owned(),
                            "action" => item.action = text.to_owned(),
                            "action_display" => item.action_display = text.to_owned(),
                            "help" => item.help = text.to_owned(),
                            _ => {}
                        }
                    }
                }
                item.is_mouse = true;
                out.mouse.push(item);
            }
        }
        if let Some(order) = value.get("mode_order").and_then(Value::as_array) {
            out.mode_order = order
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect();
        }
        if let Some(order) = value.get("category_order").and_then(Value::as_object) {
            for (mode, categories) in order {
                if let Some(categories) = categories.as_array() {
                    out.category_order.insert(
                        mode.clone(),
                        categories
                            .iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect(),
                    );
                }
            }
        }
        Ok(out)
    }

    pub fn from_json(text: &str) -> Result<Self, String> {
        let value = json::parse(text.as_bytes()).map_err(|err| err.to_string())?;
        Self::from_value(&value)
    }

    pub fn all_bindings(&self) -> Vec<Binding> {
        let mut out = Vec::new();
        for categories in self.modes.values() {
            for bindings in categories.values() {
                out.extend(bindings.iter().cloned());
            }
        }
        out.extend(self.mouse.iter().cloned());
        out
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct WordToken {
    word: String,
    start: usize,
    end: usize,
}

fn is_word_delimiter(ch: char) -> bool {
    matches!(ch, '_' | ' ' | '+' | '/' | '-')
}

fn tokenize_words(text: &str) -> Vec<WordToken> {
    let chars: Vec<char> = text.to_lowercase().chars().collect();
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    for (index, ch) in chars.iter().enumerate() {
        if is_word_delimiter(*ch) {
            if let Some(begin) = start.take() {
                out.push(WordToken {
                    word: chars[begin..index].iter().collect(),
                    start: begin,
                    end: index,
                });
            }
        } else if start.is_none() {
            start = Some(index);
        }
    }
    if let Some(begin) = start {
        out.push(WordToken {
            word: chars[begin..].iter().collect(),
            start: begin,
            end: chars.len(),
        });
    }
    out
}

fn tokenize_query(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|word| word.to_lowercase())
        .collect()
}

fn rune_range(start: usize, end: usize) -> Vec<usize> {
    (start..end).collect()
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    let mut current = vec![0usize; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        current[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            current[j + 1] = (previous[j + 1] + 1)
                .min(current[j] + 1)
                .min(previous[j] + cost);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[b.len()]
}

fn match_single_word(query: &str, words: &[WordToken]) -> (i64, Vec<usize>) {
    let mut best_score = 0i64;
    let mut best_positions = Vec::new();
    for word in words {
        let (score, positions) = if word.word == query {
            (4, rune_range(word.start, word.end))
        } else if word.word.starts_with(query) {
            (
                3,
                rune_range(word.start, word.start + query.chars().count()),
            )
        } else if query.chars().count() >= 4 && word.word.chars().count() >= 4 {
            match levenshtein(query, &word.word) {
                1 => (2, rune_range(word.start, word.end)),
                2 => (1, rune_range(word.start, word.end)),
                _ => (0, Vec::new()),
            }
        } else {
            (0, Vec::new())
        };
        if score > best_score {
            best_score = score;
            best_positions = positions;
        }
    }
    (best_score, best_positions)
}

fn best_word_match(query: &str, words: &[WordToken], column: &str) -> (i64, Vec<usize>) {
    if !query.contains(['_', '+', '/', '-']) {
        return match_single_word(query, words);
    }
    let lower = column.to_lowercase();
    if let Some(byte_index) = lower.find(query) {
        let rune_index = lower[..byte_index].chars().count();
        let parts = query
            .split(is_word_delimiter)
            .filter(|part| !part.is_empty())
            .count()
            .max(1);
        return (
            4 * parts as i64,
            rune_range(rune_index, rune_index + query.chars().count()),
        );
    }
    let mut total = 0i64;
    let mut positions = Vec::new();
    for part in query
        .split(is_word_delimiter)
        .filter(|part| !part.is_empty())
    {
        let (score, mut found) = match_single_word(part, words);
        total += score;
        positions.append(&mut found);
    }
    (total, positions)
}

#[derive(Clone, Debug)]
pub struct DisplayItem {
    pub binding: Binding,
    key_text: String,
    action_text: String,
    category_text: String,
    key_words: Vec<WordToken>,
    action_words: Vec<WordToken>,
    category_words: Vec<WordToken>,
}

impl DisplayItem {
    pub fn new(binding: Binding) -> Self {
        let key_text = if binding.key.is_empty() {
            "(unmapped)".to_owned()
        } else {
            binding.key.clone()
        };
        let action_text = if binding.action_display.is_empty() {
            binding.action.clone()
        } else {
            binding.action_display.clone()
        };
        let category_text = binding.category.clone();
        Self {
            key_words: tokenize_words(&key_text),
            action_words: tokenize_words(&action_text),
            category_words: tokenize_words(&category_text),
            key_text,
            action_text,
            category_text,
            binding,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct MatchInfo {
    pub key: Vec<usize>,
    pub action: Vec<usize>,
    pub category: Vec<usize>,
}

#[derive(Clone, Debug)]
pub struct ScoredItem {
    pub index: usize,
    pub score: i64,
    pub matched: usize,
    pub info: MatchInfo,
}

/// Rank display items against a query, matching the Go kitten's columns and
/// requiring every query token to match at least one column.
pub fn rank_items(items: &[DisplayItem], query: &str) -> Vec<ScoredItem> {
    let tokens = tokenize_query(query);
    let mut out = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let mut score = 0i64;
        let mut matched = 0usize;
        let mut info = MatchInfo::default();
        let mut all = true;
        for token in &tokens {
            let (key_score, key_pos) = best_word_match(token, &item.key_words, &item.key_text);
            let (action_score, action_pos) =
                best_word_match(token, &item.action_words, &item.action_text);
            let (category_score, category_pos) =
                best_word_match(token, &item.category_words, &item.category_text);
            let (token_score, positions, column) =
                if action_score >= key_score && action_score >= category_score {
                    (action_score, action_pos, 0)
                } else if key_score >= category_score {
                    (key_score, key_pos, 1)
                } else {
                    (category_score, category_pos, 2)
                };
            if token_score == 0 {
                all = false;
                break;
            }
            score += token_score;
            matched += 1;
            match column {
                0 => info.action.extend(positions),
                1 => info.key.extend(positions),
                _ => info.category.extend(positions),
            }
        }
        if all {
            let key_bonus = item.key_words.len() as i64;
            out.push(ScoredItem {
                index,
                score: score * 100 + key_bonus,
                matched,
                info,
            });
        }
    }
    out.sort_by(|a, b| b.score.cmp(&a.score).then(a.index.cmp(&b.index)));
    out
}

/// Flatten bindings in mode/category order for display.
pub fn flatten_bindings(data: &InputData) -> Vec<DisplayItem> {
    let mut out = Vec::new();
    let mode_order = if data.mode_order.is_empty() {
        data.modes.keys().cloned().collect::<Vec<_>>()
    } else {
        data.mode_order.clone()
    };
    for mode in mode_order {
        let Some(categories) = data.modes.get(&mode) else {
            continue;
        };
        let order = data
            .category_order
            .get(&mode)
            .cloned()
            .unwrap_or_else(|| categories.keys().cloned().collect());
        for category in order {
            if let Some(bindings) = categories.get(&category) {
                for binding in bindings {
                    let mut binding = binding.clone();
                    binding.mode = mode.clone();
                    binding.category = category.clone();
                    out.push(DisplayItem::new(binding));
                }
            }
        }
    }
    for binding in &data.mouse {
        out.push(DisplayItem::new(binding.clone()));
    }
    out
}

/// The action definition to execute for a binding.
pub fn action_for(item: &DisplayItem) -> String {
    if item.binding.definition.is_empty() {
        item.binding.action.clone()
    } else {
        item.binding.definition.clone()
    }
}

/// Interactive palette. Returns the selected action definition.
pub fn run_command_palette_ui(tty: &mut dyn Tty, data: &InputData) -> io::Result<Option<String>> {
    let items = flatten_bindings(data);
    let mut query = String::new();
    let mut filtered = rank_items(&items, "");
    let mut selected = 0usize;
    let mut offset = 0usize;
    let mut parser = KeyParser::new();
    tui::enter_screen(tty)?;
    loop {
        let size = tty.size().unwrap_or(tui::Size::new(80, 24));
        tui::clear_screen(tty)?;
        let mut text = format!("\x1b[1mCommand palette\x1b[0m  /{query}\r\n");
        let rows = size.rows.saturating_sub(3) as usize;
        if selected < offset {
            offset = selected;
        } else if selected >= offset + rows {
            offset = selected + 1 - rows;
        }
        for (row, item) in filtered.iter().skip(offset).take(rows).enumerate() {
            let display = &items[item.index];
            let marker = if offset + row == selected { ">" } else { " " };
            text.push_str(&format!(
                "{} {:<24} {:<40} {}\r\n",
                marker,
                tui::fit(&display.key_text, 24),
                tui::fit(&display.action_text, 40),
                display.category_text
            ));
        }
        text.push_str(&format!(
            "\r\n{} results  Ctrl+j/k move  Enter: run  Esc: quit",
            filtered.len()
        ));
        tty.write_all(text.as_bytes())?;
        tty.flush()?;
        let Some(key) = tui::read_key(tty, &mut parser, Duration::from_millis(500))? else {
            continue;
        };
        match key {
            Key::Esc | Key::Ctrl('c') | Key::Ctrl('d') => break,
            Key::Ctrl('j') | Key::Down => {
                if !filtered.is_empty() {
                    selected = (selected + 1) % filtered.len();
                }
            }
            Key::Ctrl('k') | Key::Up => {
                if !filtered.is_empty() {
                    selected = (selected + filtered.len() - 1) % filtered.len();
                }
            }
            Key::PageDown => selected = (selected + rows).min(filtered.len().saturating_sub(1)),
            Key::PageUp => selected = selected.saturating_sub(rows),
            Key::Enter => {
                if let Some(item) = filtered.get(selected) {
                    tui::leave_screen(tty)?;
                    return Ok(Some(action_for(&items[item.index])));
                }
            }
            Key::Backspace => {
                query.pop();
                filtered = rank_items(&items, &query);
                selected = 0;
            }
            Key::Char(ch) => {
                query.push(ch);
                filtered = rank_items(&items, &query);
                selected = 0;
            }
            _ => {}
        }
    }
    tui::leave_screen(tty)?;
    Ok(None)
}

pub fn run_command_palette_cli(
    _args: &[String],
    data: &InputData,
    tty: Option<&mut dyn Tty>,
    out: &mut dyn Write,
) -> Result<(), String> {
    let Some(tty) = tty else {
        return Err("The command palette requires a terminal".to_owned());
    };
    if let Some(action) = run_command_palette_ui(tty, data).map_err(|err| err.to_string())? {
        writeln!(out, "{action}").map_err(|err| err.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::FakeTty;

    fn sample() -> InputData {
        let json = r#"{
            "modes": {"": {"Window": [
                {"key":"ctrl+shift+enter","action":"new_window","action_display":"new_window","definition":"new_window","help":"New window"},
                {"key":"ctrl+shift+w","action":"close_window","action_display":"close_window","definition":"close_window","help":"Close window"}
            ], "Tabs": [
                {"key":"ctrl+shift+t","action":"new_tab","action_display":"new_tab","definition":"new_tab","help":"New tab"}
            ]}},
            "mouse": [{"key":"ctrl+left click","action":"open_url","action_display":"open_url","help":"Open URL"}],
            "mode_order": [""],
            "category_order": {"": ["Window", "Tabs"]}
        }"#;
        InputData::from_json(json).unwrap()
    }

    #[test]
    fn tokenization_and_word_matching() {
        let words = tokenize_words("new_window_action");
        assert_eq!(words.len(), 3);
        assert_eq!(match_single_word("new", &words).0, 4);
        assert_eq!(match_single_word("win", &words).0, 3);
        assert_eq!(match_single_word("windew", &words).0, 2);
        let compound = tokenize_words("mouse_selection");
        assert!(best_word_match("mouse_selection", &compound, "mouse_selection").0 > 0);
    }

    #[test]
    fn ranking_filters_and_orders() {
        let data = sample();
        let items = flatten_bindings(&data);
        assert_eq!(items.len(), 4);
        let ranked = rank_items(&items, "new window");
        assert!(!ranked.is_empty());
        assert_eq!(items[ranked[0].index].binding.action, "new_window");
        assert!(rank_items(&items, "zzzz").is_empty());
    }

    #[test]
    fn action_definition() {
        let data = sample();
        let items = flatten_bindings(&data);
        let item = items
            .iter()
            .find(|item| item.binding.action == "new_tab")
            .unwrap();
        assert_eq!(action_for(item), "new_tab");
    }

    #[test]
    fn ui_filters_and_returns_action() {
        let data = sample();
        let mut tty = FakeTty::new(80, 24, b"new tab\r");
        let action = run_command_palette_ui(&mut tty, &data).unwrap();
        assert_eq!(action.as_deref(), Some("new_tab"));
        assert!(tty.output_text().contains("Command palette"));
    }
}
