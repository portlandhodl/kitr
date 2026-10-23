// SPDX-License-Identifier: GPL-3.0-or-later
//! Interactive Unicode-input overlay.
//!
//! The search/selection model lives in [`crate::unicode_input`]. This module
//! draws a tabbed table on the alternate screen and maps terminal keys onto
//! that model, returning the selected character.

use std::io;
use std::time::Duration;

use crate::tui::{self, Key, KeyParser, Size, Tty};
use crate::unicode_input::{self, EmojiVariation, KeyOutcome, Mode, UnicodeInputState};

pub struct UnicodeUiOptions {
    pub initial_mode: Option<Mode>,
    pub emoji_variation: EmojiVariation,
}

impl Default for UnicodeUiOptions {
    fn default() -> Self {
        Self {
            initial_mode: None,
            emoji_variation: EmojiVariation::None,
        }
    }
}

fn map_key(key: Key) -> unicode_input::Key {
    match key {
        Key::Char(ch) => unicode_input::Key::Char(ch),
        Key::Backspace => unicode_input::Key::Backspace,
        Key::Enter => unicode_input::Key::Enter,
        Key::Esc => unicode_input::Key::Escape,
        Key::Tab => unicode_input::Key::Tab,
        Key::BackTab => unicode_input::Key::BackTab,
        Key::Up => unicode_input::Key::Up,
        Key::Down => unicode_input::Key::Down,
        Key::Left => unicode_input::Key::Left,
        Key::Right => unicode_input::Key::Right,
        Key::Ctrl(ch) => unicode_input::Key::Ctrl(ch),
        Key::Home => unicode_input::Key::Ctrl('a'),
        Key::End => unicode_input::Key::Ctrl('e'),
        Key::PageUp => unicode_input::Key::Ctrl('['),
        Key::PageDown => unicode_input::Key::Ctrl(']'),
        Key::Delete => unicode_input::Key::Backspace,
        Key::Unknown(_) => unicode_input::Key::Char('\0'),
    }
}

fn render(state: &UnicodeInputState, size: Size) -> String {
    let mut text = String::new();
    let tabs = [Mode::Hex, Mode::Name, Mode::Emoticons, Mode::Favorites];
    for mode in tabs {
        if mode == state.mode() {
            text.push_str(&format!("\x1b[7m {} \x1b[0m", mode.as_str()));
        } else {
            text.push_str(&format!(" {} ", mode.as_str()));
        }
    }
    text.push_str(&format!("  > {}\r\n", state.query()));
    let rows = size.rows.saturating_sub(3) as usize;
    let cols = size.cols.max(20) as usize;
    let per_row = (cols / 14).max(1);
    let candidates = state.candidates();
    let start = (state.selected_index() / per_row).saturating_sub(rows / 2) * per_row;
    let mut count = 0usize;
    for (index, ch) in candidates.iter().enumerate().skip(start) {
        if count >= rows * per_row {
            break;
        }
        let marker = if index == state.selected_index() {
            '>'
        } else {
            ' '
        };
        let name = unicode_input::name_for(*ch).unwrap_or("");
        let label = if state.mode() == Mode::Name {
            tui::fit(name, 12)
        } else {
            format!("{index:>4}")
        };
        text.push_str(&format!(
            "{marker}{label} {} ",
            unicode_input::resolve_char(*ch, state.emoji_variation())
        ));
        count += 1;
        if count.is_multiple_of(per_row) {
            text.push_str("\r\n");
        }
    }
    text.push_str("\r\n");
    let current = state
        .current()
        .map(|ch| {
            format!(
                "Selected: {} U+{:x} {}",
                unicode_input::resolve_char(ch, state.emoji_variation()),
                ch as u32,
                unicode_input::name_for(ch).unwrap_or("")
            )
        })
        .unwrap_or_else(|| "No selection".to_owned());
    text.push_str(&current);
    text.push_str("   Tab modes  Enter insert  Esc close");
    text
}

/// Run the interactive unicode input UI. Returns the selected string, or
/// `None` when cancelled.
pub fn run_unicode_input_ui(
    tty: &mut dyn Tty,
    options: &UnicodeUiOptions,
    favorites: Option<Vec<char>>,
) -> io::Result<Option<String>> {
    let mut state = match favorites {
        Some(favorites) => UnicodeInputState::with_favorites(favorites),
        None => UnicodeInputState::new(),
    };
    state.set_emoji_variation(options.emoji_variation);
    if let Some(mode) = options.initial_mode {
        state.set_mode(mode);
    }
    let mut parser = KeyParser::new();
    tui::enter_screen(tty)?;
    loop {
        let size = tty.size().unwrap_or(Size::new(80, 24));
        tui::clear_screen(tty)?;
        tty.write_all(render(&state, size).as_bytes())?;
        tty.flush()?;
        let Some(key) = tui::read_key(tty, &mut parser, Duration::from_millis(500))? else {
            continue;
        };
        match state.handle_key(map_key(key)) {
            KeyOutcome::Emit(text) => {
                tui::leave_screen(tty)?;
                return Ok(Some(text));
            }
            KeyOutcome::Cancel => {
                tui::leave_screen(tty)?;
                return Ok(None);
            }
            KeyOutcome::Continue | KeyOutcome::ModeChanged(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::FakeTty;

    #[test]
    fn ui_emits_selected_character() {
        let mut tty = FakeTty::new(80, 24, b"1f600\r");
        let options = UnicodeUiOptions {
            initial_mode: Some(Mode::Hex),
            ..UnicodeUiOptions::default()
        };
        let result = run_unicode_input_ui(&mut tty, &options, None).unwrap();
        assert_eq!(result.as_deref(), Some("😀"));
        let text = tty.output_text();
        assert!(text.contains("code"));
        assert!(text.contains("Selected:"));
    }

    #[test]
    fn ui_searches_by_name() {
        let mut tty = FakeTty::new(80, 24, b"\x1b[12uGRINNING FACE\r");
        let options = UnicodeUiOptions {
            initial_mode: Some(Mode::Name),
            ..UnicodeUiOptions::default()
        };
        let result = run_unicode_input_ui(&mut tty, &options, None).unwrap();
        assert_eq!(result.as_deref(), Some("😀"));
    }

    #[test]
    fn ui_cancels() {
        let options = UnicodeUiOptions::default();
        let mut tty = FakeTty::new(80, 24, b"\x1b");
        assert_eq!(
            run_unicode_input_ui(&mut tty, &options, None).unwrap(),
            None
        );
    }
}
