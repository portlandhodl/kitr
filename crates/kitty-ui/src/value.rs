// SPDX-License-Identifier: GPL-3.0-or-later
//! A tiny JSON-like value type used for layout state and session files.
//!
//! kitty serialises layout state with Python's :mod:`json`. We deliberately do
//! not depend on a JSON crate, so this module contains the small amount of
//! parsing and serialisation that the session/layout code needs.

use std::collections::BTreeMap;
use std::fmt;

/// A dynamically typed value matching the subset of JSON used by kitty.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    List(Vec<Value>),
    Map(BTreeMap<String, Value>),
}

impl Value {
    pub fn map() -> BTreeMap<String, Value> {
        BTreeMap::new()
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            Value::Float(f) if f.fract() == 0.0 => Some(*f as i64),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Int(i) => Some(*i as f64),
            Value::Float(f) => Some(*f),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    pub fn as_list(&self) -> Option<&[Value]> {
        match self {
            Value::List(l) => Some(l),
            _ => None,
        }
    }

    pub fn as_map(&self) -> Option<&BTreeMap<String, Value>> {
        match self {
            Value::Map(m) => Some(m),
            _ => None,
        }
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.as_map().and_then(|m| m.get(key))
    }

    pub fn get_i64(&self, key: &str) -> Option<i64> {
        self.get(key).and_then(Value::as_i64)
    }

    pub fn get_f64(&self, key: &str) -> Option<f64> {
        self.get(key).and_then(Value::as_f64)
    }

    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.get(key).and_then(Value::as_str)
    }

    /// Serialise with sorted object keys, matching Python's ``json.dumps`` for
    /// dicts built with ``sort_keys``-like determinism (kitty does not rely on
    /// key order, but deterministic output makes session files diff-friendly).
    pub fn to_json(&self) -> String {
        let mut out = String::new();
        self.write_json(&mut out);
        out
    }

    fn write_json(&self, out: &mut String) {
        match self {
            Value::Null => out.push_str("null"),
            Value::Bool(true) => out.push_str("true"),
            Value::Bool(false) => out.push_str("false"),
            Value::Int(i) => out.push_str(&i.to_string()),
            Value::Float(f) => {
                if f.is_finite() {
                    if f.fract() == 0.0 {
                        out.push_str(&format!("{:.1}", f));
                    } else {
                        out.push_str(&format!("{}", f));
                    }
                } else {
                    out.push_str("null");
                }
            }
            Value::Str(s) => write_json_string(s, out),
            Value::List(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    item.write_json(out);
                }
                out.push(']');
            }
            Value::Map(map) => {
                out.push('{');
                for (i, (k, v)) in map.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    write_json_string(k, out);
                    out.push_str(": ");
                    v.write_json(out);
                }
                out.push('}');
            }
        }
    }

    pub fn from_json(raw: &str) -> Result<Value, JsonError> {
        let mut p = Parser {
            bytes: raw.as_bytes(),
            pos: 0,
        };
        p.skip_ws();
        let ans = p.parse_value()?;
        p.skip_ws();
        if p.pos != p.bytes.len() {
            return Err(JsonError::new("trailing data after JSON value", p.pos));
        }
        Ok(ans)
    }
}

fn write_json_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonError {
    pub message: String,
    pub position: usize,
}

impl JsonError {
    fn new(message: impl Into<String>, position: usize) -> Self {
        Self {
            message: message.into(),
            position,
        }
    }
}

impl fmt::Display for JsonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at byte {}", self.message, self.position)
    }
}

impl std::error::Error for JsonError {}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn skip_ws(&mut self) {
        while self.pos < self.bytes.len()
            && matches!(self.bytes[self.pos], b' ' | b'\t' | b'\n' | b'\r')
        {
            self.pos += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn parse_value(&mut self) -> Result<Value, JsonError> {
        match self.peek() {
            None => Err(JsonError::new("unexpected end of JSON", self.pos)),
            Some(b'{') => self.parse_object(),
            Some(b'[') => self.parse_array(),
            Some(b'"') => self.parse_string().map(Value::Str),
            Some(b't') => self.parse_literal("true", Value::Bool(true)),
            Some(b'f') => self.parse_literal("false", Value::Bool(false)),
            Some(b'n') => self.parse_literal("null", Value::Null),
            Some(c) if c == b'-' || c.is_ascii_digit() => self.parse_number(),
            Some(_) => Err(JsonError::new("invalid JSON value", self.pos)),
        }
    }

    fn parse_literal(&mut self, lit: &str, val: Value) -> Result<Value, JsonError> {
        if self.bytes[self.pos..].starts_with(lit.as_bytes()) {
            self.pos += lit.len();
            Ok(val)
        } else {
            Err(JsonError::new("invalid JSON literal", self.pos))
        }
    }

    fn parse_object(&mut self) -> Result<Value, JsonError> {
        self.pos += 1; // {
        let mut map = BTreeMap::new();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(Value::Map(map));
        }
        loop {
            self.skip_ws();
            if self.peek() != Some(b'"') {
                return Err(JsonError::new("expected object key string", self.pos));
            }
            let key = self.parse_string()?;
            self.skip_ws();
            if self.peek() != Some(b':') {
                return Err(JsonError::new("expected ':' after object key", self.pos));
            }
            self.pos += 1;
            self.skip_ws();
            let val = self.parse_value()?;
            map.insert(key, val);
            self.skip_ws();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                }
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(Value::Map(map));
                }
                _ => return Err(JsonError::new("expected ',' or '}' in object", self.pos)),
            }
        }
    }

    fn parse_array(&mut self) -> Result<Value, JsonError> {
        self.pos += 1; // [
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(Value::List(items));
        }
        loop {
            self.skip_ws();
            items.push(self.parse_value()?);
            self.skip_ws();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                }
                Some(b']') => {
                    self.pos += 1;
                    return Ok(Value::List(items));
                }
                _ => return Err(JsonError::new("expected ',' or ']' in array", self.pos)),
            }
        }
    }

    fn parse_string(&mut self) -> Result<String, JsonError> {
        self.pos += 1; // opening quote
        let mut ans = String::new();
        loop {
            let c = self
                .peek()
                .ok_or_else(|| JsonError::new("unterminated JSON string", self.pos))?;
            match c {
                b'"' => {
                    self.pos += 1;
                    return Ok(ans);
                }
                b'\\' => {
                    self.pos += 1;
                    let esc = self
                        .peek()
                        .ok_or_else(|| JsonError::new("unterminated JSON escape", self.pos))?;
                    self.pos += 1;
                    match esc {
                        b'"' => ans.push('"'),
                        b'\\' => ans.push('\\'),
                        b'/' => ans.push('/'),
                        b'b' => ans.push('\u{08}'),
                        b'f' => ans.push('\u{0c}'),
                        b'n' => ans.push('\n'),
                        b'r' => ans.push('\r'),
                        b't' => ans.push('\t'),
                        b'u' => {
                            let cp = self.parse_hex4()?;
                            if (0xD800..0xDC00).contains(&cp) {
                                if self.peek() != Some(b'\\') {
                                    return Err(JsonError::new("lone surrogate", self.pos));
                                }
                                self.pos += 1;
                                if self.peek() != Some(b'u') {
                                    return Err(JsonError::new("lone surrogate", self.pos));
                                }
                                self.pos += 1;
                                let low = self.parse_hex4()?;
                                if !(0xDC00..0xE000).contains(&low) {
                                    return Err(JsonError::new("invalid surrogate pair", self.pos));
                                }
                                let scalar = 0x10000 + ((cp - 0xD800) << 10) + (low - 0xDC00);
                                ans.push(char::from_u32(scalar).ok_or_else(|| {
                                    JsonError::new("invalid unicode escape", self.pos)
                                })?);
                            } else {
                                ans.push(char::from_u32(cp).ok_or_else(|| {
                                    JsonError::new("invalid unicode escape", self.pos)
                                })?);
                            }
                        }
                        _ => return Err(JsonError::new("invalid JSON escape", self.pos)),
                    }
                }
                c if c < 0x20 => {
                    return Err(JsonError::new("control character in JSON string", self.pos));
                }
                _ => {
                    // Consume one UTF-8 scalar.
                    let s = std::str::from_utf8(&self.bytes[self.pos..])
                        .map_err(|_| JsonError::new("invalid UTF-8 in JSON", self.pos))?;
                    let ch = s.chars().next().unwrap();
                    ans.push(ch);
                    self.pos += ch.len_utf8();
                }
            }
        }
    }

    fn parse_hex4(&mut self) -> Result<u32, JsonError> {
        if self.pos + 4 > self.bytes.len() {
            return Err(JsonError::new("truncated unicode escape", self.pos));
        }
        let mut val = 0u32;
        for _ in 0..4 {
            let c = self.bytes[self.pos];
            let d = match c {
                b'0'..=b'9' => (c - b'0') as u32,
                b'a'..=b'f' => (c - b'a' + 10) as u32,
                b'A'..=b'F' => (c - b'A' + 10) as u32,
                _ => return Err(JsonError::new("invalid unicode escape", self.pos)),
            };
            val = val * 16 + d;
            self.pos += 1;
        }
        Ok(val)
    }

    fn parse_number(&mut self) -> Result<Value, JsonError> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
            self.pos += 1;
        }
        let mut is_float = false;
        if self.peek() == Some(b'.') {
            is_float = true;
            self.pos += 1;
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                self.pos += 1;
            }
        }
        if matches!(self.peek(), Some(b'e') | Some(b'E')) {
            is_float = true;
            self.pos += 1;
            if matches!(self.peek(), Some(b'+') | Some(b'-')) {
                self.pos += 1;
            }
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                self.pos += 1;
            }
        }
        let text = std::str::from_utf8(&self.bytes[start..self.pos])
            .map_err(|_| JsonError::new("invalid number", start))?;
        if is_float {
            text.parse::<f64>()
                .map(Value::Float)
                .map_err(|_| JsonError::new("invalid number", start))
        } else {
            text.parse::<i64>()
                .map(Value::Int)
                .map_err(|_| JsonError::new("invalid integer", start))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_roundtrip() {
        let raw = r#"{"b": [1, 2.5, true, null], "a": "x\n\"y\""}"#;
        let v = Value::from_json(raw).unwrap();
        assert_eq!(v.get_str("a"), Some("x\n\"y\""));
        let again = Value::from_json(&v.to_json()).unwrap();
        assert_eq!(v, again);
    }

    #[test]
    fn json_rejects_garbage() {
        assert!(Value::from_json("{").is_err());
        assert!(Value::from_json("[1,]").is_err());
        assert!(Value::from_json("1 2").is_err());
    }
}
