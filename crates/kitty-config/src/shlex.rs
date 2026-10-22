// SPDX-License-Identifier: GPL-3.0-or-later
//! Port of `kitty/launcher/shlex.h` and the Python iterator wrapper in
//! `kitty/shlex.c` (Copyright (C) 2023, 2025 Kovid Goyal).
//!
//! The state machine, including the special `$'...'` ANSI-C quoting mode and
//! all error messages, is a direct port. Positions refer to Unicode scalar
//! indices in the source string, matching `Shlex.next_word()` in kitty.

use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShlexError {
    message: &'static str,
}

impl ShlexError {
    pub fn message(&self) -> &'static str {
        self.message
    }
}

impl fmt::Display for ShlexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message)
    }
}

impl std::error::Error for ShlexError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Normal,
    Word,
    StringWithoutEscapes,
    StringWithEscapes,
    AnsiCQuoted,
}

/// A shell-like lexer. Create it with [`Shlex::new`], then either iterate it or
/// call [`Shlex::next_word`] to obtain `(position, word)` pairs.
#[derive(Clone, Debug)]
pub struct Shlex {
    src: Vec<char>,
    support_ansi_c_quoting: bool,
    allow_empty: bool,
    pos: usize,
    word_start: usize,
    buf: String,
    state: State,
    yielded: bool,
    finished: bool,
}

impl Shlex {
    pub fn new(src: &str, support_ansi_c_quoting: bool) -> Self {
        Self {
            src: src.chars().collect(),
            support_ansi_c_quoting,
            allow_empty: false,
            pos: 0,
            word_start: 0,
            buf: String::new(),
            state: State::Normal,
            yielded: false,
            finished: false,
        }
    }

    /// Split like `kitty.utils.shlex_split`.
    pub fn split(src: &str, support_ansi_c_quoting: bool) -> Result<Vec<String>, ShlexError> {
        let mut lexer = Self::new(src, support_ansi_c_quoting);
        let mut ans = Vec::new();
        while let Some(item) = lexer.next_word() {
            ans.push(item?.1);
        }
        Ok(ans)
    }

    /// Split like `kitty.utils.shlex_split_with_positions`.
    pub fn split_with_positions(
        src: &str,
        support_ansi_c_quoting: bool,
    ) -> Result<Vec<(usize, String)>, ShlexError> {
        let mut lexer = Self::new(src, support_ansi_c_quoting);
        let mut ans = Vec::new();
        while let Some(item) = lexer.next_word() {
            ans.push(item?);
        }
        Ok(ans)
    }

    /// Equivalent to `Shlex.next_word()`. `None` terminates iteration; an
    /// `Err` is the exact error kitty raises as `ValueError`.
    pub fn next_word(&mut self) -> Option<Result<(usize, String), ShlexError>> {
        if self.finished {
            return None;
        }
        match self.next_word_impl() {
            Ok(Some(word)) => Some(Ok(word)),
            Ok(None) => {
                self.finished = true;
                None
            }
            Err(err) => {
                self.finished = true;
                Some(Err(err))
            }
        }
    }

    fn next_word_impl(&mut self) -> Result<Option<(usize, String)>, ShlexError> {
        let mut prev_word_ch = '\0';
        while self.pos < self.src.len() {
            let ch = self.read_ch();
            match self.state {
                State::Normal => match ch {
                    ' ' | '\n' | '\t' | '\r' => {}
                    '"' => {
                        self.state = State::StringWithEscapes;
                        self.start_word();
                    }
                    '\'' => {
                        self.state = State::StringWithoutEscapes;
                        self.start_word();
                    }
                    '\\' => {
                        self.start_word();
                        self.write_escape_ch()?;
                        self.state = State::Word;
                    }
                    _ => {
                        self.state = State::Word;
                        self.start_word();
                        self.buf.push(ch);
                        prev_word_ch = ch;
                    }
                },
                State::Word => match ch {
                    ' ' | '\n' | '\t' | '\r' => {
                        prev_word_ch = '\0';
                        self.state = State::Normal;
                        if !self.buf.is_empty() || self.allow_empty {
                            return Ok(Some(self.get_word()));
                        }
                    }
                    '"' => {
                        prev_word_ch = '\0';
                        self.state = State::StringWithEscapes;
                    }
                    '\'' => {
                        // prev_word_ch is only '$' if the immediately preceding
                        // character written to buf was an unquoted, unescaped '$'.
                        if self.support_ansi_c_quoting
                            && prev_word_ch == '$'
                            && !self.buf.is_empty()
                        {
                            self.buf.pop();
                            self.state = State::AnsiCQuoted;
                        } else {
                            self.state = State::StringWithoutEscapes;
                        }
                        prev_word_ch = '\0';
                    }
                    '\\' => {
                        prev_word_ch = '\0';
                        self.write_escape_ch()?;
                    }
                    _ => {
                        self.buf.push(ch);
                        prev_word_ch = ch;
                    }
                },
                State::StringWithoutEscapes => match ch {
                    '\'' => {
                        self.state = State::Word;
                        self.allow_empty = true;
                    }
                    _ => self.buf.push(ch),
                },
                State::StringWithEscapes => match ch {
                    '"' => {
                        self.state = State::Word;
                        self.allow_empty = true;
                    }
                    '\\' => self.write_escape_ch()?,
                    _ => self.buf.push(ch),
                },
                State::AnsiCQuoted => match ch {
                    '\'' => {
                        self.state = State::Word;
                        self.allow_empty = true;
                    }
                    '\\' => self.write_ansi_escape_ch()?,
                    _ => self.buf.push(ch),
                },
            }
        }
        match self.state {
            State::Word => {
                self.state = State::Normal;
                if !self.buf.is_empty() || self.allow_empty {
                    return Ok(Some(self.get_word()));
                }
                Ok(None)
            }
            State::StringWithEscapes | State::StringWithoutEscapes | State::AnsiCQuoted => {
                self.state = State::Normal;
                Err(ShlexError {
                    message: "Unterminated string at the end of input",
                })
            }
            State::Normal => {
                if self.yielded {
                    Ok(None)
                } else {
                    self.yielded = true;
                    Ok(Some((0, String::new())))
                }
            }
        }
    }

    fn read_ch(&mut self) -> char {
        let ch = self.src[self.pos];
        self.pos += 1;
        ch
    }

    fn start_word(&mut self) {
        self.word_start = self.pos - 1;
        self.buf.clear();
    }

    fn get_word(&mut self) -> (usize, String) {
        let pos = self.word_start;
        let ans = std::mem::take(&mut self.buf);
        self.allow_empty = false;
        self.yielded = true;
        (pos, ans)
    }

    fn write_escape_ch(&mut self) -> Result<(), ShlexError> {
        if self.pos < self.src.len() {
            let ch = self.read_ch();
            self.buf.push(ch);
            Ok(())
        } else {
            Err(ShlexError {
                message: "Trailing backslash at end of input data",
            })
        }
    }

    fn write_control_ch(&mut self) -> Result<(), ShlexError> {
        if self.pos >= self.src.len() {
            return Err(ShlexError {
                message: "Trailing \\c escape at end of input data",
            });
        }
        let ch = self.read_ch();
        self.buf.push(char::from((ch as u32 & 0x1f) as u8));
        Ok(())
    }

    fn read_valid_digits(&mut self, max: usize, is_valid: fn(char) -> bool) -> String {
        let mut ans = String::new();
        for _ in 0..max {
            if self.pos >= self.src.len() {
                break;
            }
            let ch = self.read_ch();
            if !is_valid(ch) {
                self.pos -= 1;
                break;
            }
            ans.push(ch);
        }
        ans
    }

    fn write_unich(&mut self, value: u32) {
        if let Some(ch) = char::from_u32(value) {
            self.buf.push(ch);
        }
    }

    fn write_octal_ch(&mut self, ch: char) -> Result<(), ShlexError> {
        let mut chars = String::from(ch);
        chars.push_str(&self.read_valid_digits(2, is_octal_digit));
        let value = u32::from_str_radix(&chars, 8).unwrap_or(0);
        self.write_unich(value);
        Ok(())
    }

    fn write_unicode_ch(&mut self, max: usize) -> Result<(), ShlexError> {
        let chars = self.read_valid_digits(max, is_hex_digit);
        if chars.is_empty() {
            return Err(ShlexError {
                message: "Trailing unicode escape at end of input data",
            });
        }
        let value = u32::from_str_radix(&chars, 16).unwrap_or(0);
        self.write_unich(value);
        Ok(())
    }

    fn write_ansi_escape_ch(&mut self) -> Result<(), ShlexError> {
        if self.pos >= self.src.len() {
            return Err(ShlexError {
                message: "Trailing backslash at end of input data",
            });
        }
        let ch = self.read_ch();
        match ch {
            'a' => self.buf.push('\u{7}'),
            'b' => self.buf.push('\u{8}'),
            'e' | 'E' => self.buf.push('\u{1b}'),
            'f' => self.buf.push('\u{c}'),
            'n' => self.buf.push('\n'),
            'r' => self.buf.push('\r'),
            't' => self.buf.push('\t'),
            'v' => self.buf.push('\u{b}'),
            '\\' => self.buf.push('\\'),
            '\'' => self.buf.push('\''),
            '"' => self.buf.push('"'),
            '?' => self.buf.push('?'),
            'c' => self.write_control_ch()?,
            'x' => self.write_unicode_ch(2)?,
            'u' => self.write_unicode_ch(4)?,
            'U' => self.write_unicode_ch(8)?,
            '0'..='7' => self.write_octal_ch(ch)?,
            _ => self.buf.push(ch),
        }
        Ok(())
    }
}

impl Iterator for Shlex {
    type Item = Result<String, ShlexError>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.next_word() {
            Some(Ok((_, word))) => Some(Ok(word)),
            Some(Err(err)) => Some(Err(err)),
            None => None,
        }
    }
}

fn is_octal_digit(ch: char) -> bool {
    ('0'..='7').contains(&ch)
}

fn is_hex_digit(ch: char) -> bool {
    ch.is_ascii_hexdigit()
}
