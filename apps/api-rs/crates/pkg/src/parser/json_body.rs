// Copyright (c) 2023-present Plane Software, Inc. and contributors
// SPDX-License-Identifier: AGPL-3.0-only
// See the LICENSE file for details.

//! JSON decoding that matches CPython's **C scanner**, not the pure-Python one.
//!
//! DRF calls `json.load`; on CPython that uses `_json.scanstring`/`scan_once`,
//! whose error text differs from `json.decoder.py_scanstring` in two places:
//!
//! | Input                | C scanner                              | pure Python                     |
//! |----------------------|----------------------------------------|---------------------------------|
//! | `{"a":"\\q"}`        | `Invalid \escape ... (char 6)`         | `... 'q' ... (char 7)`          |
//! | `{"a":"x\x01y"}`     | `Invalid control character at ... (char 7)` | `... '\x01' ... (char 8)` |
//!
//! Offsets also follow C's sentinel quirks: whitespace skipping clamps to the
//! string length and never fails, so `{"a":1` reports a missing comma at
//! `char 6` (the length) rather than at the last byte.
//!
//! Values are kept lossless where Python is:
//!
//! * integers borrow the original text through [`RawNumber`] because Python
//!   integers are unbounded;
//! * floats become `f64`; `NaN`/`Infinity` fail at parse time because DRF
//!   passes `strict_constant`;
//! * a lone surrogate is kept inside [`PyString`] so rendering fails with
//!   [`crate::json::JsonError::LoneSurrogate`] (HTTP 500) instead of writing a
//!   replacement character.

use std::collections::HashMap;

use serde::ser::{Serialize, SerializeMap, Serializer};

use crate::json::{LONE_SURROGATE, RawNumber};

/// CPython's `sys.get_int_max_str_digits()` default.
pub const MAX_INT_DIGITS: usize = 4300;

/// CPython 3.12 allows 2997 nested containers at the default recursion limit.
pub const MAX_NESTING: usize = 2997;

/// A parsed JSON number.
#[derive(Debug, Clone, PartialEq)]
pub enum Number {
    /// Integer text, unchanged (Python's `int`).
    Integer(RawNumber<'static>),
    /// Float value (Python's `float`).
    Float(f64),
}

/// One piece of a Python string. Python strings may legally contain lone
/// surrogates; Rust strings may not, so those units are kept aside until the
/// serializer turns them into an error.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum StrSegment {
    Text(String),
    Lone(u32),
}

/// A decoded Python `str`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct PyString {
    segments: Vec<StrSegment>,
}

impl PyString {
    /// An empty string.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn push_str(&mut self, text: &str) {
        if !text.is_empty() {
            match self.segments.last_mut() {
                Some(StrSegment::Text(existing)) => existing.push_str(text),
                _ => self.segments.push(StrSegment::Text(text.to_string())),
            }
        }
    }

    fn push_char(&mut self, ch: char) {
        match self.segments.last_mut() {
            Some(StrSegment::Text(existing)) => existing.push(ch),
            _ => self.segments.push(StrSegment::Text(ch.to_string())),
        }
    }

    fn push_lone(&mut self, unit: u32) {
        self.segments.push(StrSegment::Lone(unit));
    }

    /// The string as UTF-8, or `None` when it contains a lone surrogate.
    #[must_use]
    pub fn as_str(&self) -> Option<String> {
        let mut text = String::new();
        for segment in &self.segments {
            match segment {
                StrSegment::Text(piece) => text.push_str(piece),
                StrSegment::Lone(_) => return None,
            }
        }
        Some(text)
    }

    /// `true` when Python could build the string but not UTF-8 encode it.
    #[must_use]
    pub fn has_lone_surrogate(&self) -> bool {
        self.segments
            .iter()
            .any(|segment| matches!(segment, StrSegment::Lone(_)))
    }
}

impl From<&str> for PyString {
    fn from(value: &str) -> Self {
        let mut string = Self::new();
        string.push_str(value);
        string
    }
}

impl From<String> for PyString {
    fn from(value: String) -> Self {
        let mut string = Self::new();
        string.push_str(&value);
        string
    }
}

impl Serialize for PyString {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if self.has_lone_surrogate() {
            return serializer.serialize_newtype_struct(LONE_SURROGATE, "");
        }
        serializer.serialize_str(&self.as_str().unwrap_or_default())
    }
}

/// A parsed JSON value that keeps Python's edge cases representable.
#[derive(Debug, Clone, PartialEq)]
pub enum JsonValue {
    /// `null`.
    Null,
    /// `true`/`false`.
    Bool(bool),
    /// A string, possibly holding a lone surrogate.
    String(PyString),
    /// A number.
    Number(Number),
    /// An array.
    Array(Vec<JsonValue>),
    /// An object; insertion order is preserved and duplicates keep the first
    /// position with the last value, exactly like `dict` construction.
    Object(Vec<(PyString, JsonValue)>),
}

impl JsonValue {
    /// Number of top-level values, used when merging `request.data`.
    #[must_use]
    pub fn len_hint(&self) -> usize {
        match self {
            Self::Array(values) => values.len(),
            Self::Object(values) => values.len(),
            _ => 1,
        }
    }

    fn object_from_pairs(pairs: Vec<(PyString, JsonValue)>) -> Self {
        let mut positions: HashMap<PyString, usize> = HashMap::new();
        let mut values: Vec<(PyString, JsonValue)> = Vec::new();
        for (key, value) in pairs {
            match positions.get(&key) {
                Some(&index) => values[index].1 = value,
                None => {
                    positions.insert(key.clone(), values.len());
                    values.push((key, value));
                }
            }
        }
        Self::Object(values)
    }
}

impl Serialize for JsonValue {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Null => serializer.serialize_none(),
            Self::Bool(value) => serializer.serialize_bool(*value),
            Self::String(value) => value.serialize(serializer),
            Self::Number(Number::Integer(text)) => text.serialize(serializer),
            Self::Number(Number::Float(value)) => serializer.serialize_f64(*value),
            Self::Array(values) => {
                use serde::ser::SerializeSeq;
                let mut sequence = serializer.serialize_seq(Some(values.len()))?;
                for value in values {
                    sequence.serialize_element(value)?;
                }
                sequence.end()
            }
            Self::Object(values) => {
                let mut map = serializer.serialize_map(Some(values.len()))?;
                for (key, value) in values {
                    map.serialize_entry(key, value)?;
                }
                map.end()
            }
        }
    }
}

/// Why `json.loads` (C scanner) rejected the text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonDecodeError {
    /// `ValueError` text, without DRF's `JSON parse error - ` prefix.
    pub message: String,
}

/// Failure from [`parse_json`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseFailure {
    /// `json.JSONDecodeError` or another `ValueError` raised while decoding.
    Decode(JsonDecodeError),
    /// `RecursionError`; DRF does not catch it, so the request becomes a 500.
    Recursion,
}

impl std::fmt::Display for ParseFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Decode(error) => formatter.write_str(&error.message),
            Self::Recursion => formatter.write_str("maximum recursion depth exceeded"),
        }
    }
}

impl std::error::Error for ParseFailure {}

impl std::fmt::Display for JsonDecodeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for JsonDecodeError {}

/// Parse a decoded JSON document.
///
/// # Errors
/// * [`ParseFailure::Decode`] carries CPython's message, which DRF prefixes
///   with `JSON parse error - `.
/// * [`ParseFailure::Recursion`] corresponds to CPython's `RecursionError`
///   (HTTP 500, not a 400).
pub fn parse_json(text: &str) -> Result<JsonValue, ParseFailure> {
    if text.starts_with('\u{feff}') {
        return Err(ParseFailure::Decode(JsonDecodeError {
            message: "Unexpected UTF-8 BOM (decode using utf-8-sig): line 1 column 1 (char 0)"
                .to_string(),
        }));
    }
    let mut parser = Parser {
        chars: text.char_indices().collect(),
        text,
    };
    let (value, end) = parser.parse_value(0)?;
    let trailing = parser.skip_whitespace(end);
    if trailing < parser.chars.len() {
        return Err(parser.error("Extra data", trailing));
    }
    Ok(value)
}

struct Parser<'a> {
    chars: Vec<(usize, char)>,
    text: &'a str,
}

/// One open container on the iterative parser's explicit stack.
enum Frame {
    Array {
        values: Vec<JsonValue>,
    },
    Object {
        pairs: Vec<(PyString, JsonValue)>,
        /// The key whose value is being parsed; empty between members.
        key: PyString,
    },
}

impl Frame {
    /// Collapse a closed frame into its value.
    fn into_value(self) -> JsonValue {
        match self {
            Self::Array { values } => JsonValue::Array(values),
            Self::Object { pairs, .. } => JsonValue::object_from_pairs(pairs),
        }
    }
}

impl<'a> Parser<'a> {
    fn skip_whitespace(&self, mut index: usize) -> usize {
        while index < self.chars.len() && matches!(self.chars[index].1, ' ' | '\t' | '\n' | '\r') {
            index += 1;
        }
        index
    }

    /// Raise `JSONDecodeError` for a character position, rendered the way
    /// Python computes `lineno`/`colno`/`char`.
    fn error(&self, message: &str, index: usize) -> ParseFailure {
        let index = index.min(self.chars.len());
        let char_offset = index;
        let byte_offset = self
            .chars
            .get(index)
            .map_or(self.text.len(), |(offset, _)| *offset);
        let before = &self.text[..byte_offset];
        let line = before.matches('\n').count() + 1;
        let column = before
            .rsplit('\n')
            .next()
            .map_or(1, |tail| tail.chars().count() + 1);
        ParseFailure::Decode(JsonDecodeError {
            message: format!("{message}: line {line} column {column} (char {char_offset})"),
        })
    }

    /// A bare `ValueError` (no line/column), as `PyLong_FromString` or
    /// `strict_constant` produce.
    fn plain_error(message: String) -> ParseFailure {
        ParseFailure::Decode(JsonDecodeError { message })
    }

    fn byte_offset(&self, index: usize) -> usize {
        self.chars
            .get(index)
            .map_or(self.text.len(), |(offset, _)| *offset)
    }

    /// Parse one value, iteratively.
    ///
    /// CPython's C scanner recurses once per container, which is why 2997
    /// nested arrays parse and 2998 raise `RecursionError`. A recursive port
    /// would blow the *Rust* stack long before that (a 2 MiB thread overflows
    /// at roughly 1000 levels), turning a valid request into a crash. The
    /// explicit stack below keeps memory on the heap, so the only depth limit
    /// is the one Python enforces.
    /// Parse one value without recursing per container.
    ///
    /// CPython's C scanner recurses once per container, which is why 2997
    /// nested arrays parse and 2998 raise `RecursionError`. A recursive port
    /// would exhaust the *Rust* stack long before that (a 2 MiB thread
    /// overflows near 1000 levels), turning a valid request into a crash. The
    /// explicit stack below keeps the frames on the heap, so the only depth
    /// limit is the one Python enforces.
    fn parse_value(&mut self, index: usize) -> Result<(JsonValue, usize), ParseFailure> {
        let mut stack: Vec<Frame> = Vec::new();
        let mut index = index;
        loop {
            index = self.skip_whitespace(index);
            let Some(&(_, ch)) = self.chars.get(index) else {
                return Err(self.error("Expecting value", index));
            };
            let mut value = match ch {
                '"' => {
                    let (value, next) = self.parse_string(index + 1)?;
                    index = next;
                    value
                }
                'n' if self.matches(index, "null") => {
                    index += 4;
                    JsonValue::Null
                }
                't' if self.matches(index, "true") => {
                    index += 4;
                    JsonValue::Bool(true)
                }
                'f' if self.matches(index, "false") => {
                    index += 5;
                    JsonValue::Bool(false)
                }
                'N' if self.matches(index, "NaN") => {
                    return Err(Self::plain_error(
                        "Out of range float values are not JSON compliant: 'NaN'".to_string(),
                    ));
                }
                'I' if self.matches(index, "Infinity") => {
                    return Err(Self::plain_error(
                        "Out of range float values are not JSON compliant: 'Infinity'".to_string(),
                    ));
                }
                '-' if self.matches(index, "-Infinity") => {
                    return Err(Self::plain_error(
                        "Out of range float values are not JSON compliant: '-Infinity'".to_string(),
                    ));
                }
                '[' => {
                    if stack.len() >= MAX_NESTING {
                        return Err(ParseFailure::Recursion);
                    }
                    let cursor = self.skip_whitespace(index + 1);
                    if cursor < self.chars.len() && self.chars[cursor].1 == ']' {
                        index = cursor + 1;
                        JsonValue::Array(Vec::new())
                    } else {
                        stack.push(Frame::Array { values: Vec::new() });
                        index = cursor;
                        continue;
                    }
                }
                '{' => {
                    if stack.len() >= MAX_NESTING {
                        return Err(ParseFailure::Recursion);
                    }
                    let cursor = self.skip_whitespace(index + 1);
                    if cursor < self.chars.len() && self.chars[cursor].1 == '}' {
                        index = cursor + 1;
                        JsonValue::Object(Vec::new())
                    } else {
                        let (key, next) = self.parse_member_key(cursor)?;
                        stack.push(Frame::Object {
                            pairs: Vec::new(),
                            key,
                        });
                        index = next;
                        continue;
                    }
                }
                _ => {
                    let (value, next) = self.parse_number(index)?;
                    index = next;
                    value
                }
            };

            // Deliver the finished value to the innermost open container,
            // closing frames until one still expects more input.
            loop {
                let Some(frame) = stack.last_mut() else {
                    return Ok((value, index));
                };
                let close = match frame {
                    Frame::Array { values } => {
                        values.push(value);
                        ']'
                    }
                    Frame::Object { pairs, key } => {
                        pairs.push((std::mem::take(key), value));
                        '}'
                    }
                };
                index = self.skip_whitespace(index);
                if index < self.chars.len() && self.chars[index].1 == close {
                    index += 1;
                    value = stack.pop().expect("frame exists").into_value();
                    continue;
                }
                if index >= self.chars.len() || self.chars[index].1 != ',' {
                    return Err(self.error("Expecting ',' delimiter", index));
                }
                index = self.skip_whitespace(index + 1);
                if let Frame::Object { key, .. } = stack.last_mut().expect("frame exists") {
                    let (name, next) = self.parse_member_key(index)?;
                    *key = name;
                    index = next;
                }
                break;
            }
        }
    }

    /// Read `"key":` inside an object and return the offset of the value.
    fn parse_member_key(&mut self, index: usize) -> Result<(PyString, usize), ParseFailure> {
        let cursor = self.skip_whitespace(index);
        if cursor >= self.chars.len() || self.chars[cursor].1 != '"' {
            return Err(self.error("Expecting property name enclosed in double quotes", cursor));
        }
        let (key, next) = self.parse_string(cursor + 1)?;
        let JsonValue::String(key) = key else {
            unreachable!("parse_string only returns strings");
        };
        let colon = self.skip_whitespace(next);
        if colon >= self.chars.len() || self.chars[colon].1 != ':' {
            return Err(self.error("Expecting ':' delimiter", colon));
        }
        Ok((key, self.skip_whitespace(colon + 1)))
    }

    fn matches(&self, index: usize, literal: &str) -> bool {
        let length = literal.chars().count();
        index + length <= self.chars.len()
            && self.chars[index..index + length]
                .iter()
                .zip(literal.chars())
                .all(|((_, actual), expected)| *actual == expected)
    }

    fn parse_string(&mut self, index: usize) -> Result<(JsonValue, usize), ParseFailure> {
        let start = index - 1;
        let mut output = PyString::new();
        let mut index = index;
        loop {
            let run_start = index;
            while index < self.chars.len() {
                let ch = self.chars[index].1;
                if ch == '"' || ch == '\\' {
                    break;
                }
                if (ch as u32) <= 0x1f {
                    return Err(self.error("Invalid control character at", index));
                }
                index += 1;
            }
            output.push_str(&self.slice_chars(run_start, index));
            if index >= self.chars.len() {
                return Err(self.error("Unterminated string starting at", start));
            }
            let ch = self.chars[index].1;
            index += 1;
            if ch == '"' {
                return Ok((JsonValue::String(output), index));
            }
            if index >= self.chars.len() {
                return Err(self.error("Unterminated string starting at", start));
            }
            let escape = self.chars[index].1;
            match escape {
                '"' => output.push_char('"'),
                '\\' => output.push_char('\\'),
                '/' => output.push_char('/'),
                'b' => output.push_char('\u{8}'),
                'f' => output.push_char('\u{c}'),
                'n' => output.push_char('\n'),
                'r' => output.push_char('\r'),
                't' => output.push_char('\t'),
                'u' => {
                    // `index` is the position of `u`. CPython requires five
                    // characters after it (four hex digits and one more).
                    if index + 5 >= self.chars.len() {
                        return Err(self.error("Invalid \\uXXXX escape", index));
                    }
                    let high = self.parse_hex4(index + 1, index)?;
                    index += 5;
                    if (0xd800..=0xdbff).contains(&high)
                        && index + 6 < self.chars.len()
                        && self.chars[index].1 == '\\'
                        && self.chars[index + 1].1 == 'u'
                    {
                        let low = self.parse_hex4(index + 2, index + 1)?;
                        if (0xdc00..=0xdfff).contains(&low) {
                            let combined = 0x1_0000 + ((high - 0xd800) << 10) + (low - 0xdc00);
                            output.push_char(
                                char::from_u32(combined).unwrap_or(char::REPLACEMENT_CHARACTER),
                            );
                            index += 6;
                            continue;
                        }
                    }
                    match u32_to_char(high) {
                        Some(ch) => output.push_char(ch),
                        None => output.push_lone(high),
                    }
                    continue;
                }
                _ => return Err(self.error("Invalid \\escape", index - 1)),
            }
            index += 1;
        }
    }

    fn slice_chars(&self, start: usize, end: usize) -> String {
        if start >= end {
            return String::new();
        }
        self.text[self.byte_offset(start)..self.byte_offset(end)].to_string()
    }

    /// Read four hex digits starting at `index`; `escape_index` is the position
    /// of the `u`, which is where CPython points its error.
    fn parse_hex4(&self, index: usize, escape_index: usize) -> Result<u32, ParseFailure> {
        let mut value = 0u32;
        for offset in 0..4 {
            let position = index + offset;
            let Some(&(_, ch)) = self.chars.get(position) else {
                return Err(self.error("Invalid \\uXXXX escape", escape_index));
            };
            let digit = match ch.to_digit(16) {
                Some(digit) if ch.is_ascii() => digit,
                _ => return Err(self.error("Invalid \\uXXXX escape", escape_index)),
            };
            value = (value << 4) | digit;
        }
        Ok(value)
    }

    fn parse_number(&mut self, start: usize) -> Result<(JsonValue, usize), ParseFailure> {
        let mut index = start;
        let mut is_float = false;
        if self.chars[index].1 == '-' {
            index += 1;
            if index >= self.chars.len() {
                return Err(self.error("Expecting value", start));
            }
        }
        match self.chars[index].1 {
            '1'..='9' => {
                index += 1;
                while index < self.chars.len() && self.chars[index].1.is_ascii_digit() {
                    index += 1;
                }
            }
            '0' => index += 1,
            _ => return Err(self.error("Expecting value", start)),
        }
        if index + 1 < self.chars.len()
            && self.chars[index].1 == '.'
            && self.chars[index + 1].1.is_ascii_digit()
        {
            is_float = true;
            index += 2;
            while index < self.chars.len() && self.chars[index].1.is_ascii_digit() {
                index += 1;
            }
        }
        if index + 1 < self.chars.len() && matches!(self.chars[index].1, 'e' | 'E') {
            let exponent_start = index;
            index += 1;
            if index < self.chars.len() && matches!(self.chars[index].1, '-' | '+') {
                index += 1;
            }
            while index < self.chars.len() && self.chars[index].1.is_ascii_digit() {
                index += 1;
            }
            if self.chars[index - 1].1.is_ascii_digit() {
                is_float = true;
            } else {
                index = exponent_start;
            }
        }
        let text = self.slice_chars(start, index);
        if is_float {
            let value = text.parse::<f64>().unwrap_or(if text.starts_with('-') {
                f64::NEG_INFINITY
            } else {
                f64::INFINITY
            });
            Ok((JsonValue::Number(Number::Float(value)), index))
        } else {
            let digits = text.trim_start_matches('-');
            if digits.len() > MAX_INT_DIGITS {
                return Err(Self::plain_error(format!(
                    "Exceeds the limit (4300 digits) for integer string conversion: value has {} digits; use sys.set_int_max_str_digits() to increase the limit",
                    digits.len()
                )));
            }
            // `int("-0")` is `0`, so the sign must not survive into the
            // rendered body. Floats keep it: `json.loads("-0.0")` stays
            // `-0.0` because `float("-0.0")` does.
            let text = if digits.bytes().all(|byte| byte == b'0') {
                "0".to_string()
            } else {
                text
            };
            Ok((
                JsonValue::Number(Number::Integer(RawNumber::owned(text))),
                index,
            ))
        }
    }
}

fn u32_to_char(value: u32) -> Option<char> {
    char::from_u32(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err(text: &str) -> String {
        match parse_json(text) {
            Ok(_) => panic!("expected an error for {text:?}"),
            Err(ParseFailure::Decode(error)) => error.message,
            Err(ParseFailure::Recursion) => "recursion".to_string(),
        }
    }

    #[test]
    fn messages_match_the_c_scanner() {
        let cases = [
            ("", "Expecting value: line 1 column 1 (char 0)"),
            (" ", "Expecting value: line 1 column 2 (char 1)"),
            (
                "{",
                "Expecting property name enclosed in double quotes: line 1 column 2 (char 1)",
            ),
            ("[", "Expecting value: line 1 column 2 (char 1)"),
            ("{\"a\":", "Expecting value: line 1 column 6 (char 5)"),
            (
                "{\"a\":1",
                "Expecting ',' delimiter: line 1 column 7 (char 6)",
            ),
            ("[1,", "Expecting value: line 1 column 4 (char 3)"),
            ("[1,]", "Expecting value: line 1 column 4 (char 3)"),
            (
                "{,}",
                "Expecting property name enclosed in double quotes: line 1 column 2 (char 1)",
            ),
            (
                "{\"a\":1,}",
                "Expecting property name enclosed in double quotes: line 1 column 8 (char 7)",
            ),
            ("tru", "Expecting value: line 1 column 1 (char 0)"),
            (
                "\"abc",
                "Unterminated string starting at: line 1 column 1 (char 0)",
            ),
            ("01", "Extra data: line 1 column 2 (char 1)"),
            ("1.", "Extra data: line 1 column 2 (char 1)"),
            ("1e", "Extra data: line 1 column 2 (char 1)"),
            ("1e+", "Extra data: line 1 column 2 (char 1)"),
            (
                "\n\n  {\"a\" 1}",
                "Expecting ':' delimiter: line 3 column 8 (char 9)",
            ),
            (
                "{\"a\":\n1 \"b\":2}",
                "Expecting ',' delimiter: line 2 column 3 (char 8)",
            ),
            (
                "\t{\"a\":\t\"\u{1}\"}",
                "Invalid control character at: line 1 column 9 (char 8)",
            ),
            (
                "{\"a\":\"x\u{1}y\"}",
                "Invalid control character at: line 1 column 8 (char 7)",
            ),
            (
                "{\"a\":\"\\q\"}",
                "Invalid \\escape: line 1 column 7 (char 6)",
            ),
            (
                "{\"a\":\"\\u00zz\"}",
                "Invalid \\uXXXX escape: line 1 column 8 (char 7)",
            ),
            ("{\"a\":1}x", "Extra data: line 1 column 8 (char 7)"),
            ("{\"a\":1} x", "Extra data: line 1 column 9 (char 8)"),
            ("[1]2", "Extra data: line 1 column 4 (char 3)"),
            ("{}[]", "Extra data: line 1 column 3 (char 2)"),
            (
                "{\"a\":1,\"b\":}",
                "Expecting value: line 1 column 12 (char 11)",
            ),
            (
                "{'a':1}",
                "Expecting property name enclosed in double quotes: line 1 column 2 (char 1)",
            ),
            (
                "{\"a\" 1}",
                "Expecting ':' delimiter: line 1 column 6 (char 5)",
            ),
            (
                "{\"a\":1 \"b\":2}",
                "Expecting ',' delimiter: line 1 column 8 (char 7)",
            ),
            ("[]]", "Extra data: line 1 column 3 (char 2)"),
            ("]]", "Expecting value: line 1 column 1 (char 0)"),
            ("1 2", "Extra data: line 1 column 3 (char 2)"),
            ("{\"a\":tru}", "Expecting value: line 1 column 6 (char 5)"),
            ("{\"a\":+1}", "Expecting value: line 1 column 6 (char 5)"),
            ("{\"a\":.5}", "Expecting value: line 1 column 6 (char 5)"),
            (
                "{\"a\":01}",
                "Expecting ',' delimiter: line 1 column 7 (char 6)",
            ),
            (
                "{\"a\":1.}",
                "Expecting ',' delimiter: line 1 column 7 (char 6)",
            ),
            (
                "{\"a\":1e}",
                "Expecting ',' delimiter: line 1 column 7 (char 6)",
            ),
            (
                "{\"a\":1e+}",
                "Expecting ',' delimiter: line 1 column 7 (char 6)",
            ),
            ("{\"a\":-}", "Expecting value: line 1 column 6 (char 5)"),
            (
                "{\"a\":\"\\u12\"}",
                "Invalid \\uXXXX escape: line 1 column 8 (char 7)",
            ),
            (
                "{\"a\":\"\\u\"}",
                "Invalid \\uXXXX escape: line 1 column 8 (char 7)",
            ),
            (
                "\u{feff}{\"a\":1}",
                "Unexpected UTF-8 BOM (decode using utf-8-sig): line 1 column 1 (char 0)",
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(err(input), expected, "{input:?}");
        }
    }

    #[test]
    fn constants_fail_like_strict_constant() {
        assert_eq!(
            err("{\"n\":NaN}"),
            "Out of range float values are not JSON compliant: 'NaN'"
        );
        assert_eq!(
            err("{\"n\":Infinity}"),
            "Out of range float values are not JSON compliant: 'Infinity'"
        );
        assert_eq!(
            err("{\"n\":-Infinity}"),
            "Out of range float values are not JSON compliant: '-Infinity'"
        );
    }

    #[test]
    fn valid_documents_keep_python_semantics() {
        assert!(matches!(
            parse_json("{\"a\":1}   "),
            Ok(JsonValue::Object(_))
        ));
        let value = parse_json("{\"a\":1,\"a\":2}").expect("parse");
        match value {
            JsonValue::Object(pairs) => assert_eq!(pairs.len(), 1),
            other => panic!("unexpected {other:?}"),
        }
        let value = parse_json("123456789012345678901234567890").expect("parse");
        match value {
            JsonValue::Number(Number::Integer(text)) => {
                assert_eq!(text.as_str(), "123456789012345678901234567890");
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(
            err(&"1".repeat(4301)),
            "Exceeds the limit (4300 digits) for integer string conversion: value has 4301 digits; use sys.set_int_max_str_digits() to increase the limit"
        );
        assert!(parse_json(&"1".repeat(4300)).is_ok());
    }

    #[test]
    fn lone_surrogates_survive_until_render() {
        let value = parse_json("{\"a\":\"\\ud83d\"}").expect("parse");
        let JsonValue::Object(pairs) = value else {
            panic!("expected object");
        };
        let JsonValue::String(string) = &pairs[0].1 else {
            panic!("expected string");
        };
        assert!(string.has_lone_surrogate());
        let value = parse_json("{\"a\":\"\\ud83dx\"}").expect("parse");
        let JsonValue::Object(pairs) = value else {
            panic!("expected object");
        };
        let JsonValue::String(string) = &pairs[0].1 else {
            panic!("expected string");
        };
        assert!(string.has_lone_surrogate());
        let value = parse_json("{\"a\":\"\\ud83d\\ude00\"}").expect("parse");
        let JsonValue::Object(pairs) = value else {
            panic!("expected object");
        };
        assert_eq!(pairs[0].1, JsonValue::String("\u{1f600}".into()));
    }

    #[test]
    fn nesting_limit_matches_cpython() {
        let ok = format!("{}{}", "[".repeat(MAX_NESTING), "]".repeat(MAX_NESTING));
        assert!(parse_json(&ok).is_ok());
        let deep = format!(
            "{}{}",
            "[".repeat(MAX_NESTING + 1),
            "]".repeat(MAX_NESTING + 1)
        );
        assert_eq!(parse_json(&deep), Err(ParseFailure::Recursion));
    }
}
