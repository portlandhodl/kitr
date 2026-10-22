// SPDX-License-Identifier: GPL-3.0-or-later
//! A tiny backtracking regular-expression engine used by the hints kitten.
//!
//! The workspace intentionally has no third-party regex crate. This engine
//! covers the constructs used by kitty's hint patterns and by typical
//! `--regex` values:
//!
//! * literals, `.`, character classes and the `\d \w \s \b \n \r \t` escapes;
//! * capturing groups, `(?:...)`, `(?P<name>...)` and `(?<name>...)`;
//! * alternation, `* + ? {m} {m,} {m,n}` and lazy quantifiers;
//! * `^`, `$` and the `(?m)`, `(?s)`, `(?i)` flags.
//!
//! It is not a complete PCRE implementation. Look-around, back-references and
//! Unicode property classes are rejected with an error instead of silently
//! producing different matches.

use std::fmt;

type Capture = Option<(usize, usize)>;
type MatchState = (usize, Vec<Capture>);

/// A compiled lightweight regular expression.
#[derive(Clone, Debug)]
pub(crate) struct Regex {
    ast: Node,
    names: Vec<Option<String>>,
    group_count: usize,
    multiline: bool,
    dotall: bool,
    case_insensitive: bool,
}

/// One match, with character offsets converted to byte offsets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RegexMatch {
    pub start: usize,
    pub end: usize,
    pub groups: Vec<Option<(usize, usize)>>,
    pub names: Vec<Option<String>>,
}

impl RegexMatch {
    pub fn group_text<'a>(&self, text: &'a str, index: usize) -> Option<&'a str> {
        let (start, end) = self.groups.get(index).copied().flatten()?;
        text.get(start..end)
    }

    pub fn named_groups<'a>(&self, text: &'a str) -> Vec<(String, &'a str)> {
        let mut ans = Vec::new();
        for (index, name) in self.names.iter().enumerate().skip(1) {
            let Some(name) = name else { continue };
            if let Some(value) = self.group_text(text, index) {
                ans.push((name.clone(), value));
            }
        }
        ans
    }
}

/// A regex compilation/matching error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RegexError(pub String);

impl fmt::Display for RegexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Debug)]
enum Node {
    Empty,
    Literal(char),
    Any,
    Class(CharClass),
    Start,
    End,
    WordBoundary(bool),
    Group {
        index: usize,
        node: Box<Node>,
    },
    Concat(Vec<Node>),
    Alt(Vec<Node>),
    Repeat {
        node: Box<Node>,
        min: usize,
        max: Option<usize>,
        greedy: bool,
    },
}

#[derive(Clone, Debug)]
struct CharClass {
    negated: bool,
    items: Vec<ClassItem>,
}

#[derive(Clone, Debug)]
enum ClassItem {
    Range(char, char),
    Digit(bool),
    Word(bool),
    Space(bool),
}

impl Regex {
    /// Compile a pattern.
    pub fn new(pattern: &str) -> Result<Self, RegexError> {
        let mut parser = Parser::new(pattern)?;
        let (multiline, dotall, case_insensitive) = parser.parse_flags();
        let ast = parser.parse_alt()?;
        if parser.pos != parser.chars.len() {
            return Err(parser.error("unexpected trailing input"));
        }
        Ok(Self {
            ast,
            names: parser.names,
            group_count: parser.group_count,
            multiline,
            dotall,
            case_insensitive,
        })
    }

    /// Find all non-overlapping matches.
    pub fn find_iter(&self, text: &str) -> Vec<RegexMatch> {
        let chars: Vec<char> = text.chars().collect();
        let byte_offsets = char_byte_offsets(text, chars.len());
        let mut ans = Vec::new();
        let mut start = 0usize;
        while start <= chars.len() {
            match self.match_at(&chars, start) {
                Some((end, captures)) => {
                    let start_byte = byte_offsets[start];
                    let end_byte = byte_offsets[end];
                    let mut groups: Vec<Option<(usize, usize)>> = captures
                        .iter()
                        .map(|capture| capture.map(|(s, e)| (byte_offsets[s], byte_offsets[e])))
                        .collect();
                    if let Some(group) = groups.first_mut() {
                        *group = Some((start_byte, end_byte));
                    }
                    ans.push(RegexMatch {
                        start: start_byte,
                        end: end_byte,
                        groups,
                        names: self.names.clone(),
                    });
                    if end > start {
                        start = end;
                    } else {
                        start += 1;
                    }
                }
                None => start += 1,
            }
        }
        ans
    }

    fn match_at(&self, chars: &[char], start: usize) -> Option<MatchState> {
        let mut captures: Vec<Capture> = vec![None; self.group_count + 1];
        let states = self.match_node(&self.ast, chars, start, &captures, 0);
        let (end, found) = states.into_iter().next()?;
        captures.copy_from_slice(&found);
        Some((end, captures))
    }

    fn match_node(
        &self,
        node: &Node,
        chars: &[char],
        pos: usize,
        captures: &[Capture],
        depth: usize,
    ) -> Vec<MatchState> {
        if depth > 512 {
            return Vec::new();
        }
        match node {
            Node::Empty => vec![(pos, captures.to_vec())],
            Node::Literal(expected) => {
                let Some(actual) = chars.get(pos) else {
                    return Vec::new();
                };
                let matches = if self.case_insensitive {
                    actual.to_lowercase().eq(expected.to_lowercase())
                } else {
                    actual == expected
                };
                if matches {
                    vec![(pos + 1, captures.to_vec())]
                } else {
                    Vec::new()
                }
            }
            Node::Any => match chars.get(pos) {
                Some(ch) if self.dotall || *ch != '\n' => vec![(pos + 1, captures.to_vec())],
                _ => Vec::new(),
            },
            Node::Class(class) => match chars.get(pos) {
                Some(ch) if class.matches(*ch) => vec![(pos + 1, captures.to_vec())],
                _ => Vec::new(),
            },
            Node::Start => {
                let ok = pos == 0 || (self.multiline && chars.get(pos - 1) == Some(&'\n'));
                if ok {
                    vec![(pos, captures.to_vec())]
                } else {
                    Vec::new()
                }
            }
            Node::End => {
                let ok = pos == chars.len() || (self.multiline && chars.get(pos) == Some(&'\n'));
                if ok {
                    vec![(pos, captures.to_vec())]
                } else {
                    Vec::new()
                }
            }
            Node::WordBoundary(want) => {
                let before = pos > 0 && is_word(chars[pos - 1]);
                let after = chars.get(pos).is_some_and(|ch| is_word(*ch));
                if (before != after) == *want {
                    vec![(pos, captures.to_vec())]
                } else {
                    Vec::new()
                }
            }
            Node::Group { index, node } => {
                let mut ans = Vec::new();
                for (end, mut found) in self.match_node(node, chars, pos, captures, depth + 1) {
                    found[*index] = Some((pos, end));
                    ans.push((end, found));
                }
                ans
            }
            Node::Concat(nodes) => {
                let mut states = vec![(pos, captures.to_vec())];
                for child in nodes {
                    let mut next = Vec::new();
                    for (state_pos, state_captures) in states {
                        next.extend(self.match_node(
                            child,
                            chars,
                            state_pos,
                            &state_captures,
                            depth + 1,
                        ));
                        if next.len() > 100_000 {
                            break;
                        }
                    }
                    states = next;
                    if states.is_empty() {
                        break;
                    }
                }
                states
            }
            Node::Alt(branches) => {
                let mut ans = Vec::new();
                for branch in branches {
                    ans.extend(self.match_node(branch, chars, pos, captures, depth + 1));
                }
                ans
            }
            Node::Repeat {
                node,
                min,
                max,
                greedy,
            } => {
                let mut results = Vec::new();
                self.repeat(
                    node,
                    chars,
                    pos,
                    captures,
                    0,
                    *min,
                    *max,
                    *greedy,
                    depth + 1,
                    &mut results,
                );
                results
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn repeat(
        &self,
        node: &Node,
        chars: &[char],
        pos: usize,
        captures: &[Option<(usize, usize)>],
        count: usize,
        min: usize,
        max: Option<usize>,
        greedy: bool,
        depth: usize,
        out: &mut Vec<MatchState>,
    ) {
        if out.len() > 100_000 || depth > 512 {
            return;
        }
        let can_stop = count >= min;
        let can_continue = max.is_none_or(|max| count < max);
        let stop = |out: &mut Vec<_>| {
            if can_stop {
                out.push((pos, captures.to_vec()));
            }
        };
        if greedy {
            if can_continue {
                for (end, found) in self.match_node(node, chars, pos, captures, depth + 1) {
                    if end == pos {
                        // An empty repetition cannot make progress.
                        continue;
                    }
                    self.repeat(
                        node,
                        chars,
                        end,
                        &found,
                        count + 1,
                        min,
                        max,
                        greedy,
                        depth + 1,
                        out,
                    );
                }
            }
            stop(out);
        } else {
            stop(out);
            if can_continue {
                for (end, found) in self.match_node(node, chars, pos, captures, depth + 1) {
                    if end == pos {
                        continue;
                    }
                    self.repeat(
                        node,
                        chars,
                        end,
                        &found,
                        count + 1,
                        min,
                        max,
                        greedy,
                        depth + 1,
                        out,
                    );
                }
            }
        }
    }
}

impl CharClass {
    fn matches(&self, ch: char) -> bool {
        let matched = self.items.iter().any(|item| match item {
            ClassItem::Range(start, end) => *start <= ch && ch <= *end,
            ClassItem::Digit(want) => ch.is_ascii_digit() == *want,
            ClassItem::Word(want) => is_word(ch) == *want,
            ClassItem::Space(want) => ch.is_whitespace() == *want,
        });
        matched != self.negated
    }
}

fn is_word(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

fn char_byte_offsets(text: &str, char_count: usize) -> Vec<usize> {
    let mut offsets = Vec::with_capacity(char_count + 1);
    for (byte, _) in text.char_indices() {
        offsets.push(byte);
    }
    offsets.push(text.len());
    offsets
}

struct Parser {
    chars: Vec<char>,
    pos: usize,
    group_count: usize,
    names: Vec<Option<String>>,
    multiline: bool,
    dotall: bool,
    case_insensitive: bool,
}

impl Parser {
    fn new(pattern: &str) -> Result<Self, RegexError> {
        Ok(Self {
            chars: pattern.chars().collect(),
            pos: 0,
            group_count: 0,
            names: vec![None],
            multiline: false,
            dotall: false,
            case_insensitive: false,
        })
    }

    fn error(&self, message: impl Into<String>) -> RegexError {
        RegexError(format!("{} at character {}", message.into(), self.pos))
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn next(&mut self) -> Option<char> {
        let ch = self.peek()?;
        self.pos += 1;
        Some(ch)
    }

    fn eat(&mut self, ch: char) -> bool {
        if self.peek() == Some(ch) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn parse_flags(&mut self) -> (bool, bool, bool) {
        loop {
            if self.peek() != Some('(') || self.chars.get(self.pos + 1) != Some(&'?') {
                break;
            }
            let mut look = self.pos + 2;
            let mut flags = Vec::new();
            while let Some(ch) = self.chars.get(look).copied() {
                if ch == ')' {
                    break;
                }
                if !matches!(ch, 'm' | 's' | 'i') {
                    return (self.multiline, self.dotall, self.case_insensitive);
                }
                flags.push(ch);
                look += 1;
            }
            if self.chars.get(look) != Some(&')') || flags.is_empty() {
                break;
            }
            for flag in flags {
                match flag {
                    'm' => self.multiline = true,
                    's' => self.dotall = true,
                    'i' => self.case_insensitive = true,
                    _ => {}
                }
            }
            self.pos = look + 1;
        }
        (self.multiline, self.dotall, self.case_insensitive)
    }

    fn parse_alt(&mut self) -> Result<Node, RegexError> {
        let mut branches = vec![self.parse_concat()?];
        while self.eat('|') {
            branches.push(self.parse_concat()?);
        }
        Ok(if branches.len() == 1 {
            branches.remove(0)
        } else {
            Node::Alt(branches)
        })
    }

    fn parse_concat(&mut self) -> Result<Node, RegexError> {
        let mut nodes = Vec::new();
        while let Some(ch) = self.peek() {
            if ch == ')' || ch == '|' {
                break;
            }
            nodes.push(self.parse_quantified()?);
        }
        Ok(match nodes.len() {
            0 => Node::Empty,
            1 => nodes.remove(0),
            _ => Node::Concat(nodes),
        })
    }

    fn parse_quantified(&mut self) -> Result<Node, RegexError> {
        let mut node = self.parse_atom()?;
        loop {
            let (min, max) = match self.peek() {
                Some('*') => {
                    self.pos += 1;
                    (0, None)
                }
                Some('+') => {
                    self.pos += 1;
                    (1, None)
                }
                Some('?') => {
                    self.pos += 1;
                    (0, Some(1))
                }
                Some('{') => match self.parse_braces()? {
                    Some(bounds) => bounds,
                    None => break,
                },
                _ => break,
            };
            let greedy = !self.eat('?');
            node = Node::Repeat {
                node: Box::new(node),
                min,
                max,
                greedy,
            };
        }
        Ok(node)
    }

    fn parse_braces(&mut self) -> Result<Option<(usize, Option<usize>)>, RegexError> {
        let start = self.pos;
        self.pos += 1;
        let mut first = String::new();
        while let Some(ch) = self.peek() {
            if ch.is_ascii_digit() {
                first.push(ch);
                self.pos += 1;
            } else {
                break;
            }
        }
        if first.is_empty() {
            self.pos = start;
            return Ok(None);
        }
        let min: usize = first
            .parse()
            .map_err(|_| self.error("invalid repetition"))?;
        let max = if self.eat(',') {
            let mut second = String::new();
            while let Some(ch) = self.peek() {
                if ch.is_ascii_digit() {
                    second.push(ch);
                    self.pos += 1;
                } else {
                    break;
                }
            }
            if second.is_empty() {
                None
            } else {
                Some(
                    second
                        .parse()
                        .map_err(|_| self.error("invalid repetition"))?,
                )
            }
        } else {
            Some(min)
        };
        if !self.eat('}') {
            self.pos = start;
            return Ok(None);
        }
        Ok(Some((min, max)))
    }

    fn parse_atom(&mut self) -> Result<Node, RegexError> {
        let Some(ch) = self.next() else {
            return Ok(Node::Empty);
        };
        match ch {
            '.' => Ok(Node::Any),
            '^' => Ok(Node::Start),
            '$' => Ok(Node::End),
            '(' => self.parse_group(),
            '[' => self.parse_class(),
            '\\' => self.parse_escape(false),
            ')' => Err(self.error("unmatched )")),
            other => Ok(Node::Literal(other)),
        }
    }

    fn parse_group(&mut self) -> Result<Node, RegexError> {
        let mut named = None;
        let mut capturing = true;
        if self.eat('?') {
            match self.peek() {
                Some(':') => {
                    self.pos += 1;
                    capturing = false;
                }
                Some('P') if self.chars.get(self.pos + 1) == Some(&'<') => {
                    self.pos += 2;
                    named = Some(self.parse_group_name()?);
                }
                Some('<') => {
                    if matches!(self.chars.get(self.pos + 1), Some('=') | Some('!')) {
                        return Err(self.error("look-around is not supported"));
                    }
                    self.pos += 1;
                    named = Some(self.parse_group_name()?);
                }
                Some('=') | Some('!') => {
                    return Err(self.error("look-around is not supported"));
                }
                _ => return Err(self.error("unsupported group syntax")),
            }
        }
        let index = if capturing {
            self.group_count += 1;
            self.names.push(named.clone());
            self.group_count
        } else {
            0
        };
        let inner = self.parse_alt()?;
        if !self.eat(')') {
            return Err(self.error("missing )"));
        }
        if index == 0 {
            Ok(inner)
        } else {
            Ok(Node::Group {
                index,
                node: Box::new(inner),
            })
        }
    }

    fn parse_group_name(&mut self) -> Result<String, RegexError> {
        let mut name = String::new();
        while let Some(ch) = self.peek() {
            if ch == '>' {
                self.pos += 1;
                if name.is_empty() {
                    return Err(self.error("empty group name"));
                }
                return Ok(name);
            }
            name.push(ch);
            self.pos += 1;
        }
        Err(self.error("unterminated group name"))
    }

    fn parse_class(&mut self) -> Result<Node, RegexError> {
        let negated = self.eat('^');
        let mut items = Vec::new();
        let mut first = true;
        loop {
            let Some(ch) = self.next() else {
                return Err(self.error("unterminated character class"));
            };
            if ch == ']' && !first {
                break;
            }
            first = false;
            let start = if ch == '\\' {
                match self.class_escape()? {
                    ClassAtom::Char(ch) => ch,
                    ClassAtom::Item(item) => {
                        items.push(item);
                        continue;
                    }
                }
            } else {
                ch
            };
            if self.peek() == Some('-') && self.chars.get(self.pos + 1).is_some_and(|ch| *ch != ']')
            {
                self.pos += 1;
                let Some(end_ch) = self.next() else {
                    return Err(self.error("unterminated class range"));
                };
                let end = if end_ch == '\\' {
                    match self.class_escape()? {
                        ClassAtom::Char(ch) => ch,
                        ClassAtom::Item(_) => {
                            return Err(self.error("invalid class range"));
                        }
                    }
                } else {
                    end_ch
                };
                if end < start {
                    return Err(self.error("invalid class range"));
                }
                items.push(ClassItem::Range(start, end));
            } else {
                items.push(ClassItem::Range(start, start));
            }
        }
        Ok(Node::Class(CharClass { negated, items }))
    }

    fn class_escape(&mut self) -> Result<ClassAtom, RegexError> {
        let Some(ch) = self.next() else {
            return Err(self.error("trailing backslash"));
        };
        Ok(match ch {
            'd' => ClassAtom::Item(ClassItem::Digit(true)),
            'D' => ClassAtom::Item(ClassItem::Digit(false)),
            'w' => ClassAtom::Item(ClassItem::Word(true)),
            'W' => ClassAtom::Item(ClassItem::Word(false)),
            's' => ClassAtom::Item(ClassItem::Space(true)),
            'S' => ClassAtom::Item(ClassItem::Space(false)),
            'n' => ClassAtom::Char('\n'),
            'r' => ClassAtom::Char('\r'),
            't' => ClassAtom::Char('\t'),
            'f' => ClassAtom::Char('\x0c'),
            'v' => ClassAtom::Char('\x0b'),
            '0' => ClassAtom::Char('\0'),
            'x' => ClassAtom::Char(self.parse_hex(2)?),
            'u' => ClassAtom::Char(self.parse_hex(4)?),
            other => ClassAtom::Char(other),
        })
    }

    fn parse_hex(&mut self, digits: usize) -> Result<char, RegexError> {
        let mut value = 0u32;
        for _ in 0..digits {
            let Some(ch) = self.next() else {
                return Err(self.error("truncated hex escape"));
            };
            let digit = ch
                .to_digit(16)
                .ok_or_else(|| self.error("invalid hex escape"))?;
            value = value * 16 + digit;
        }
        char::from_u32(value).ok_or_else(|| self.error("invalid code point"))
    }

    fn parse_escape(&mut self, _in_class: bool) -> Result<Node, RegexError> {
        let Some(ch) = self.next() else {
            return Err(self.error("trailing backslash"));
        };
        Ok(match ch {
            'd' => Node::Class(CharClass {
                negated: false,
                items: vec![ClassItem::Digit(true)],
            }),
            'D' => Node::Class(CharClass {
                negated: false,
                items: vec![ClassItem::Digit(false)],
            }),
            'w' => Node::Class(CharClass {
                negated: false,
                items: vec![ClassItem::Word(true)],
            }),
            'W' => Node::Class(CharClass {
                negated: false,
                items: vec![ClassItem::Word(false)],
            }),
            's' => Node::Class(CharClass {
                negated: false,
                items: vec![ClassItem::Space(true)],
            }),
            'S' => Node::Class(CharClass {
                negated: false,
                items: vec![ClassItem::Space(false)],
            }),
            'b' => Node::WordBoundary(true),
            'B' => Node::WordBoundary(false),
            'n' => Node::Literal('\n'),
            'r' => Node::Literal('\r'),
            't' => Node::Literal('\t'),
            'f' => Node::Literal('\x0c'),
            'v' => Node::Literal('\x0b'),
            '0' => Node::Literal('\0'),
            'x' => Node::Literal(self.parse_hex(2)?),
            'u' => Node::Literal(self.parse_hex(4)?),
            other => Node::Literal(other),
        })
    }
}

enum ClassAtom {
    Char(char),
    Item(ClassItem),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literals_anchors_and_alternation() {
        let re = Regex::new(r"^(cat|dog)$").unwrap();
        assert_eq!(re.find_iter("cat").len(), 1);
        assert_eq!(re.find_iter("dog").len(), 1);
        assert!(re.find_iter("catdog").is_empty());
    }

    #[test]
    fn classes_quantifiers_and_groups() {
        let re = Regex::new(r"(?P<word>[A-Za-z]+) +(\d+)").unwrap();
        let matches = re.find_iter("one 12 two 345");
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].group_text("one 12 two 345", 1), Some("one"));
        assert_eq!(matches[0].group_text("one 12 two 345", 2), Some("12"));
        assert_eq!(
            matches[0].named_groups("one 12 two 345"),
            vec![("word".to_owned(), "one")]
        );
    }

    #[test]
    fn multiline_and_lazy() {
        let re = Regex::new(r"(?m)^\s*(.+?)\s*$").unwrap();
        let matches = re.find_iter("  one  \n\ttwo\t\n");
        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].group_text("  one  \n\ttwo\t\n", 1), Some("one"));
        assert_eq!(matches[1].group_text("  one  \n\ttwo\t\n", 1), Some("two"));
    }

    #[test]
    fn word_boundaries_and_case_flag() {
        let re = Regex::new(r"(?i)\bcat\b").unwrap();
        assert_eq!(re.find_iter("Cat cat scatter").len(), 2);
    }

    #[test]
    fn bad_patterns_are_errors() {
        assert!(Regex::new("(").is_err());
        assert!(Regex::new("[abc").is_err());
        assert!(Regex::new("(?=x)").is_err());
        assert!(Regex::new("(?P<>x)").is_err());
    }

    #[test]
    fn utf8_offsets_are_bytes() {
        let re = Regex::new(r"\p").unwrap();
        let matches = re.find_iter("aλp");
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].start, 3);
    }
}
