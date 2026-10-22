// SPDX-License-Identifier: GPL-3.0-or-later
//! A small, dependency-free JSON value, parser and serializer.
//!
//! Kitty's remote-control wire format is JSON (see `remote_control.py`). The
//! rest of this crate only needs a conservative subset of JSON: objects,
//! arrays, strings, numbers, booleans and null. This module deliberately does
//! not depend on `serde_json` so the crate remains buildable offline. It also
//! keeps the distinction between signed/unsigned integers and floating point
//! numbers, which matters for kitty payload fields such as colors, versions and
//! timestamps.
//!
//! The serializer emits UTF-8 directly (rather than Python's default
//! `ensure_ascii=True` escaping). Both forms are valid JSON and are accepted by
//! kitty. Object key order is preserved.

use std::fmt;

/// Maximum nesting depth accepted by the parser.
pub const MAX_DEPTH: usize = 128;

/// A JSON number, preserving the integer/float distinction.
#[derive(Clone, Copy, Debug)]
pub enum Number {
    /// A negative or otherwise signed integer.
    I64(i64),
    /// A non-negative integer that does not fit in `i64`.
    U64(u64),
    /// A floating point number.
    F64(f64),
}

impl Number {
    /// Return the value as `i64` if it is exactly representable.
    pub fn as_i64(self) -> Option<i64> {
        match self {
            Self::I64(v) => Some(v),
            Self::U64(v) => i64::try_from(v).ok(),
            Self::F64(v) => {
                if v.is_finite() && v.fract() == 0.0 && v >= i64::MIN as f64 && v <= i64::MAX as f64
                {
                    Some(v as i64)
                } else {
                    None
                }
            }
        }
    }

    /// Return the value as `u64` if it is a non-negative integer.
    pub fn as_u64(self) -> Option<u64> {
        match self {
            Self::I64(v) => u64::try_from(v).ok(),
            Self::U64(v) => Some(v),
            Self::F64(v) => {
                if v.is_finite() && v.fract() == 0.0 && v >= 0.0 && v <= u64::MAX as f64 {
                    Some(v as u64)
                } else {
                    None
                }
            }
        }
    }

    /// Return the value as `f64`.
    pub fn as_f64(self) -> f64 {
        match self {
            Self::I64(v) => v as f64,
            Self::U64(v) => v as f64,
            Self::F64(v) => v,
        }
    }

    /// Whether this is a floating point number (including an integral float
    /// such as `1.0`).
    pub fn is_float(self) -> bool {
        matches!(self, Self::F64(_))
    }
}

impl PartialEq for Number {
    fn eq(&self, other: &Self) -> bool {
        match (*self, *other) {
            (Self::I64(a), Self::I64(b)) => a == b,
            (Self::U64(a), Self::U64(b)) => a == b,
            (Self::I64(a), Self::U64(b)) | (Self::U64(b), Self::I64(a)) => a >= 0 && a as u64 == b,
            (Self::F64(a), Self::F64(b)) => a.to_bits() == b.to_bits(),
            (Self::F64(a), Self::I64(b)) | (Self::I64(b), Self::F64(a)) => a == b as f64,
            (Self::F64(a), Self::U64(b)) | (Self::U64(b), Self::F64(a)) => a == b as f64,
        }
    }
}

impl Eq for Number {}

/// A JSON value.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// JSON `null`.
    Null,
    /// `true` or `false`.
    Bool(bool),
    /// A number.
    Number(Number),
    /// A string.
    String(String),
    /// An array.
    Array(Vec<Value>),
    /// An object with insertion order preserved.
    Object(Vec<(String, Value)>),
}

impl Value {
    /// Create an empty object.
    pub fn object() -> Self {
        Self::Object(Vec::new())
    }

    /// Create an object from key/value pairs.
    pub fn object_from<I, K>(items: I) -> Self
    where
        I: IntoIterator<Item = (K, Value)>,
        K: Into<String>,
    {
        let mut ans = Self::object();
        for (key, value) in items {
            ans.set(key, value);
        }
        ans
    }

    /// Create an array.
    pub fn array<I: IntoIterator<Item = Value>>(items: I) -> Self {
        Self::Array(items.into_iter().collect())
    }

    /// Look up a field on an object. Later duplicates win, matching Python's
    /// `json` module.
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Self::Object(items) => items.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// Mutable lookup on an object.
    pub fn get_mut(&mut self, key: &str) -> Option<&mut Value> {
        match self {
            Self::Object(items) => items
                .iter_mut()
                .rev()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v),
            _ => None,
        }
    }

    /// Insert or replace a field on an object. Returns `false` for non-objects.
    pub fn set(&mut self, key: impl Into<String>, value: Value) -> bool {
        let key = key.into();
        match self {
            Self::Object(items) => {
                if let Some((_, slot)) = items.iter_mut().find(|(k, _)| *k == key) {
                    *slot = value;
                } else {
                    items.push((key, value));
                }
                true
            }
            _ => false,
        }
    }

    /// Remove a field from an object.
    pub fn remove(&mut self, key: &str) -> Option<Value> {
        match self {
            Self::Object(items) => {
                let pos = items.iter().rposition(|(k, _)| k == key)?;
                Some(items.remove(pos).1)
            }
            _ => None,
        }
    }

    /// Whether this is an object.
    pub fn is_object(&self) -> bool {
        matches!(self, Self::Object(_))
    }

    /// Borrow the object entries.
    pub fn as_object(&self) -> Option<&[(String, Value)]> {
        match self {
            Self::Object(items) => Some(items),
            _ => None,
        }
    }

    /// Mutably borrow the object entries.
    pub fn as_object_mut(&mut self) -> Option<&mut Vec<(String, Value)>> {
        match self {
            Self::Object(items) => Some(items),
            _ => None,
        }
    }

    /// Borrow the array elements.
    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Self::Array(items) => Some(items),
            _ => None,
        }
    }

    /// Whether this is a string.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(s) => Some(s),
            _ => None,
        }
    }

    /// Whether this is a boolean.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// Whether this is a number.
    pub fn as_number(&self) -> Option<Number> {
        match self {
            Self::Number(n) => Some(*n),
            _ => None,
        }
    }

    /// Return an integer field.
    pub fn as_i64(&self) -> Option<i64> {
        self.as_number().and_then(Number::as_i64)
    }

    /// Return an unsigned integer field.
    pub fn as_u64(&self) -> Option<u64> {
        self.as_number().and_then(Number::as_u64)
    }

    /// Return a floating point field.
    pub fn as_f64(&self) -> Option<f64> {
        self.as_number().map(Number::as_f64)
    }

    /// Whether this is `null`.
    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }

    /// Serialize to a JSON string.
    pub fn to_json(&self) -> Result<String, JsonError> {
        let mut out = String::new();
        self.write_to(&mut out)?;
        Ok(out)
    }

    /// Serialize into an existing string.
    pub fn write_to(&self, out: &mut String) -> Result<(), JsonError> {
        write_value(self, out, 0)
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.to_json() {
            Ok(text) => f.write_str(&text),
            Err(err) => write!(f, "<invalid JSON: {err}>"),
        }
    }
}

impl From<bool> for Value {
    fn from(v: bool) -> Self {
        Self::Bool(v)
    }
}
impl From<i64> for Value {
    fn from(v: i64) -> Self {
        Self::Number(Number::I64(v))
    }
}
impl From<u64> for Value {
    fn from(v: u64) -> Self {
        Self::Number(Number::U64(v))
    }
}
impl From<u32> for Value {
    fn from(v: u32) -> Self {
        Self::Number(Number::U64(v as u64))
    }
}
impl From<usize> for Value {
    fn from(v: usize) -> Self {
        Self::Number(Number::U64(v as u64))
    }
}
impl From<f64> for Value {
    fn from(v: f64) -> Self {
        Self::Number(Number::F64(v))
    }
}
impl From<&str> for Value {
    fn from(v: &str) -> Self {
        Self::String(v.to_owned())
    }
}
impl From<String> for Value {
    fn from(v: String) -> Self {
        Self::String(v)
    }
}
impl From<Vec<Value>> for Value {
    fn from(v: Vec<Value>) -> Self {
        Self::Array(v)
    }
}
impl From<()> for Value {
    fn from(_: ()) -> Self {
        Self::Null
    }
}

impl<T: Into<Value>> From<Option<T>> for Value {
    fn from(v: Option<T>) -> Self {
        match v {
            Some(value) => value.into(),
            None => Self::Null,
        }
    }
}

/// A JSON parse or serialization error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JsonError {
    /// Human readable description.
    pub message: String,
    /// Byte offset in the input, or `None` while serializing.
    pub offset: Option<usize>,
}

impl JsonError {
    fn at(message: impl Into<String>, offset: usize) -> Self {
        Self {
            message: message.into(),
            offset: Some(offset),
        }
    }

    fn serializing(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            offset: None,
        }
    }
}

impl fmt::Display for JsonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.offset {
            Some(offset) => write!(f, "{} at byte {offset}", self.message),
            None => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for JsonError {}

/// Parse a complete JSON document from UTF-8 bytes.
pub fn parse(input: &[u8]) -> Result<Value, JsonError> {
    let mut parser = Parser { input, pos: 0 };
    parser.skip_ws();
    let value = parser.parse_value(0)?;
    parser.skip_ws();
    if parser.pos != input.len() {
        return Err(JsonError::at(
            "trailing characters after JSON value",
            parser.pos,
        ));
    }
    Ok(value)
}

/// Parse a complete JSON document from a string.
pub fn parse_str(input: &str) -> Result<Value, JsonError> {
    parse(input.as_bytes())
}

struct Parser<'a> {
    input: &'a [u8],
    pos: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.input.get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        while let Some(b) = self.peek() {
            if matches!(b, b' ' | b'\t' | b'\n' | b'\r') {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn expect(&mut self, byte: u8) -> Result<(), JsonError> {
        if self.peek() == Some(byte) {
            self.pos += 1;
            Ok(())
        } else {
            Err(JsonError::at(
                format!("expected '{}'", byte as char),
                self.pos,
            ))
        }
    }

    fn parse_value(&mut self, depth: usize) -> Result<Value, JsonError> {
        if depth > MAX_DEPTH {
            return Err(JsonError::at(
                "maximum JSON nesting depth exceeded",
                self.pos,
            ));
        }
        match self.peek() {
            None => Err(JsonError::at("unexpected end of JSON input", self.pos)),
            Some(b'n') => {
                self.literal(b"null")?;
                Ok(Value::Null)
            }
            Some(b't') => {
                self.literal(b"true")?;
                Ok(Value::Bool(true))
            }
            Some(b'f') => {
                self.literal(b"false")?;
                Ok(Value::Bool(false))
            }
            Some(b'"') => Ok(Value::String(self.parse_string()?)),
            Some(b'[') => self.parse_array(depth + 1),
            Some(b'{') => self.parse_object(depth + 1),
            Some(b'-' | b'0'..=b'9') => Ok(Value::Number(self.parse_number()?)),
            Some(other) => Err(JsonError::at(
                format!("unexpected byte 0x{other:02x} while parsing JSON value"),
                self.pos,
            )),
        }
    }

    fn literal(&mut self, word: &[u8]) -> Result<(), JsonError> {
        if self.input[self.pos..].starts_with(word) {
            self.pos += word.len();
            Ok(())
        } else {
            Err(JsonError::at("invalid JSON literal", self.pos))
        }
    }

    fn parse_array(&mut self, depth: usize) -> Result<Value, JsonError> {
        self.expect(b'[')?;
        self.skip_ws();
        let mut items = Vec::new();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(Value::Array(items));
        }
        loop {
            self.skip_ws();
            items.push(self.parse_value(depth)?);
            self.skip_ws();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                }
                Some(b']') => {
                    self.pos += 1;
                    return Ok(Value::Array(items));
                }
                _ => return Err(JsonError::at("expected ',' or ']' in array", self.pos)),
            }
        }
    }

    fn parse_object(&mut self, depth: usize) -> Result<Value, JsonError> {
        self.expect(b'{')?;
        self.skip_ws();
        let mut items: Vec<(String, Value)> = Vec::new();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(Value::Object(items));
        }
        loop {
            self.skip_ws();
            if self.peek() != Some(b'"') {
                return Err(JsonError::at("expected string key in object", self.pos));
            }
            let key = self.parse_string()?;
            self.skip_ws();
            self.expect(b':')?;
            self.skip_ws();
            let value = self.parse_value(depth)?;
            if let Some((_, slot)) = items.iter_mut().find(|(k, _)| *k == key) {
                *slot = value;
            } else {
                items.push((key, value));
            }
            self.skip_ws();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                }
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(Value::Object(items));
                }
                _ => return Err(JsonError::at("expected ',' or '}' in object", self.pos)),
            }
        }
    }

    fn parse_string(&mut self) -> Result<String, JsonError> {
        self.expect(b'"')?;
        let mut out = String::new();
        let mut run_start = self.pos;
        loop {
            let Some(byte) = self.peek() else {
                return Err(JsonError::at("unterminated string", self.pos));
            };
            match byte {
                b'"' => {
                    self.push_utf8_run(run_start, self.pos, &mut out)?;
                    self.pos += 1;
                    return Ok(out);
                }
                b'\\' => {
                    self.push_utf8_run(run_start, self.pos, &mut out)?;
                    self.pos += 1;
                    let escape = self
                        .peek()
                        .ok_or_else(|| JsonError::at("unterminated escape", self.pos))?;
                    self.pos += 1;
                    match escape {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{0008}'),
                        b'f' => out.push('\u{000c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let ch = self.parse_unicode_escape()?;
                            out.push(ch);
                        }
                        _ => {
                            return Err(JsonError::at("invalid string escape", self.pos - 1));
                        }
                    }
                    run_start = self.pos;
                }
                0x00..=0x1f => {
                    return Err(JsonError::at(
                        "unescaped control character in string",
                        self.pos,
                    ));
                }
                _ => self.pos += 1,
            }
        }
    }

    fn push_utf8_run(&self, start: usize, end: usize, out: &mut String) -> Result<(), JsonError> {
        if start == end {
            return Ok(());
        }
        let text = std::str::from_utf8(&self.input[start..end])
            .map_err(|_| JsonError::at("invalid UTF-8 in string", start))?;
        out.push_str(text);
        Ok(())
    }

    fn parse_hex4(&mut self) -> Result<u16, JsonError> {
        if self.pos + 4 > self.input.len() {
            return Err(JsonError::at("truncated \\u escape", self.pos));
        }
        let mut value = 0u16;
        for _ in 0..4 {
            let b = self.input[self.pos];
            let digit = match b {
                b'0'..=b'9' => (b - b'0') as u16,
                b'a'..=b'f' => (b - b'a' + 10) as u16,
                b'A'..=b'F' => (b - b'A' + 10) as u16,
                _ => return Err(JsonError::at("invalid hex digit in \\u escape", self.pos)),
            };
            value = value * 16 + digit;
            self.pos += 1;
        }
        Ok(value)
    }

    fn parse_unicode_escape(&mut self) -> Result<char, JsonError> {
        let first = self.parse_hex4()?;
        if (0xd800..=0xdbff).contains(&first) {
            if !self.input[self.pos..].starts_with(b"\\u") {
                return Err(JsonError::at(
                    "high surrogate without a following low surrogate",
                    self.pos,
                ));
            }
            self.pos += 2;
            let second = self.parse_hex4()?;
            if !(0xdc00..=0xdfff).contains(&second) {
                return Err(JsonError::at("invalid low surrogate", self.pos - 4));
            }
            let code = 0x10000 + (((first as u32 - 0xd800) << 10) | (second as u32 - 0xdc00));
            char::from_u32(code)
                .ok_or_else(|| JsonError::at("invalid unicode code point", self.pos - 6))
        } else if (0xdc00..=0xdfff).contains(&first) {
            Err(JsonError::at("unpaired low surrogate", self.pos - 4))
        } else {
            char::from_u32(first as u32)
                .ok_or_else(|| JsonError::at("invalid unicode code point", self.pos - 4))
        }
    }

    fn parse_number(&mut self) -> Result<Number, JsonError> {
        let start = self.pos;
        let negative = self.peek() == Some(b'-');
        if negative {
            self.pos += 1;
        }
        match self.peek() {
            Some(b'0') => {
                self.pos += 1;
                if matches!(self.peek(), Some(b'0'..=b'9')) {
                    return Err(JsonError::at("leading zeros are not allowed", self.pos));
                }
            }
            Some(b'1'..=b'9') => {
                self.pos += 1;
                while matches!(self.peek(), Some(b'0'..=b'9')) {
                    self.pos += 1;
                }
            }
            _ => return Err(JsonError::at("expected digit in number", self.pos)),
        }

        let mut is_float = false;
        if self.peek() == Some(b'.') {
            is_float = true;
            self.pos += 1;
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(JsonError::at(
                    "expected digit after decimal point",
                    self.pos,
                ));
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            is_float = true;
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(JsonError::at("expected digit in exponent", self.pos));
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
        }

        let text = std::str::from_utf8(&self.input[start..self.pos])
            .map_err(|_| JsonError::at("invalid UTF-8 in number", start))?;
        if !is_float {
            if negative {
                if let Ok(value) = text.parse::<i64>() {
                    return Ok(Number::I64(value));
                }
            } else if let Ok(value) = text.parse::<u64>() {
                return Ok(Number::U64(value));
            }
            if let Ok(value) = text.parse::<i64>() {
                return Ok(Number::I64(value));
            }
        }
        let value = text
            .parse::<f64>()
            .map_err(|_| JsonError::at("invalid number", start))?;
        if !value.is_finite() {
            return Err(JsonError::at("non-finite number", start));
        }
        Ok(Number::F64(value))
    }
}

fn write_value(value: &Value, out: &mut String, depth: usize) -> Result<(), JsonError> {
    if depth > MAX_DEPTH {
        return Err(JsonError::serializing(
            "maximum JSON nesting depth exceeded while serializing",
        ));
    }
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(Number::I64(v)) => out.push_str(&v.to_string()),
        Value::Number(Number::U64(v)) => out.push_str(&v.to_string()),
        Value::Number(Number::F64(v)) => {
            if !v.is_finite() {
                return Err(JsonError::serializing(
                    "cannot serialize a non-finite number",
                ));
            }
            if *v == 0.0 {
                if v.is_sign_negative() {
                    out.push_str("-0.0");
                } else {
                    out.push_str("0.0");
                }
            } else if v.fract() == 0.0 && v.abs() < 1e16 {
                out.push_str(&format!("{v:.1}"));
            } else {
                out.push_str(&v.to_string());
            }
        }
        Value::String(text) => write_string(text, out),
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_value(item, out, depth + 1)?;
            }
            out.push(']');
        }
        Value::Object(items) => {
            out.push('{');
            for (index, (key, item)) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_string(key, out);
                out.push(':');
                write_value(item, out, depth + 1)?;
            }
            out.push('}');
        }
    }
    Ok(())
}

fn write_string(text: &str, out: &mut String) {
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{0008}' => out.push_str("\\b"),
            '\u{000c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_scalars() {
        assert_eq!(parse_str("null").unwrap(), Value::Null);
        assert_eq!(parse_str("true").unwrap(), Value::Bool(true));
        assert_eq!(parse_str("false").unwrap(), Value::Bool(false));
        assert_eq!(
            parse_str("\"hello\"").unwrap(),
            Value::String("hello".into())
        );
        assert_eq!(parse_str(" 42 ").unwrap(), Value::Number(Number::U64(42)));
        assert_eq!(parse_str("-42").unwrap(), Value::Number(Number::I64(-42)));
        assert_eq!(parse_str("1.5").unwrap(), Value::Number(Number::F64(1.5)));
        assert_eq!(
            parse_str("1e3").unwrap(),
            Value::Number(Number::F64(1000.0))
        );
    }

    #[test]
    fn parses_objects_and_arrays() {
        let value = parse_str(r#"{"a":[1,2,{"b":null}],"c":true}"#).unwrap();
        assert_eq!(value.get("c"), Some(&Value::Bool(true)));
        assert_eq!(
            value.get("a").unwrap().as_array().unwrap()[2].get("b"),
            Some(&Value::Null)
        );
        assert!(parse_str("[1,]").is_err());
        assert!(parse_str("{\"a\":1,}").is_err());
    }

    #[test]
    fn parses_string_escapes() {
        let value = parse_str(r#""a\n\t\"\\\/\b\f\r""#).unwrap();
        assert_eq!(value, Value::String("a\n\t\"\\/\u{8}\u{c}\r".into()));
    }

    #[test]
    fn parses_unicode_and_surrogate_pairs() {
        assert_eq!(
            parse_str(r#""\u00e9\u20ac""#).unwrap(),
            Value::String("é€".into())
        );
        assert_eq!(
            parse_str(r#""\ud83d\ude00""#).unwrap(),
            Value::String("😀".into())
        );
        assert!(parse_str(r#""\ud83d""#).is_err());
        assert!(parse_str(r#""\ude00""#).is_err());
        assert!(parse_str(r#""\ud83d\u0041""#).is_err());
    }

    #[test]
    fn rejects_invalid_strings() {
        assert!(parse_str("\"unterminated").is_err());
        assert!(parse_str("\"bad\\qescape\"").is_err());
        assert!(parse_str("\"raw\nnewline\"").is_err());
        assert!(parse(b"\"\xff\"").is_err());
    }

    #[test]
    fn parses_number_edges() {
        assert_eq!(
            parse_str("9223372036854775807").unwrap(),
            Value::Number(Number::U64(i64::MAX as u64))
        );
        assert_eq!(
            parse_str("18446744073709551615").unwrap(),
            Value::Number(Number::U64(u64::MAX))
        );
        assert_eq!(
            parse_str("-9223372036854775808").unwrap(),
            Value::Number(Number::I64(i64::MIN))
        );
        assert!(parse_str("18446744073709551616")
            .unwrap()
            .as_f64()
            .is_some());
        assert!(parse_str("01").is_err());
        assert!(parse_str("+1").is_err());
        assert!(parse_str("1.").is_err());
        assert!(parse_str(".1").is_err());
        assert!(parse_str("1e").is_err());
        assert!(parse_str("-").is_err());
    }

    #[test]
    fn duplicate_keys_use_last_value() {
        let value = parse_str(r#"{"a":1,"a":2}"#).unwrap();
        assert_eq!(value.get("a"), Some(&Value::Number(Number::U64(2))));
        assert_eq!(value.as_object().unwrap().len(), 1);
    }

    #[test]
    fn serializer_escapes_control_characters_and_roundtrips() {
        let value = Value::String("\u{0}\u{1}\n\r\t\"\\é😀".into());
        let text = value.to_json().unwrap();
        assert!(text.contains("\\u0000"));
        assert!(text.contains("\\n"));
        assert_eq!(parse_str(&text).unwrap(), value);
    }

    #[test]
    fn serializer_numbers() {
        assert_eq!(Value::from(1.0f64).to_json().unwrap(), "1.0");
        assert_eq!(Value::from(-0.0f64).to_json().unwrap(), "-0.0");
        assert_eq!(Value::from(1.5f64).to_json().unwrap(), "1.5");
        assert_eq!(
            Value::from(u64::MAX).to_json().unwrap(),
            "18446744073709551615"
        );
        assert!(Value::from(f64::NAN).to_json().is_err());
        assert!(Value::from(f64::INFINITY).to_json().is_err());
    }

    #[test]
    fn serializer_preserves_object_order() {
        let value = parse_str(r#"{"b":1,"a":2}"#).unwrap();
        assert_eq!(value.to_json().unwrap(), r#"{"b":1,"a":2}"#);
    }

    #[test]
    fn rejects_trailing_data_and_deep_nesting() {
        assert!(parse_str("{} {}").is_err());
        let deep = "[".repeat(MAX_DEPTH + 2) + &"]".repeat(MAX_DEPTH + 2);
        assert!(parse_str(&deep).is_err());
    }
}
