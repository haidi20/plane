// Copyright (c) 2023-present Plane Software, Inc. and contributors
// SPDX-License-Identifier: AGPL-3.0-only
// See the LICENSE file for details.

//! `application/x-www-form-urlencoded` bodies, as `QueryDict` reads them.
//!
//! Three details are easy to get wrong and all three are observable:
//!
//! * `QueryDict` decodes the whole body with the header charset first and falls
//!   back to Latin-1 when that fails; the percent-decoding then runs on the
//!   resulting text.
//! * `parse_qsl` compares `max_num_fields` against `1 + qs.count('&')`, not
//!   against the number of pairs it yields, so a trailing `&` can trip the
//!   limit on an otherwise empty body.
//! * `unquote_plus` replaces `+` with a space **before** decoding, and only
//!   ASCII runs are percent-decoded.

use std::collections::HashMap;

use super::charset::{Charset, percent_decode};
use super::{ParserError, ParserLimits};

/// `request.POST`: an ordered map of repeated keys.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FormFields {
    entries: Vec<(String, Vec<String>)>,
    positions: HashMap<String, usize>,
}

impl FormFields {
    /// An empty map.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Append one value, keeping every repetition in arrival order.
    pub fn append(&mut self, key: String, value: String) {
        match self.positions.get(&key) {
            Some(&index) => self.entries[index].1.push(value),
            None => {
                self.positions.insert(key.clone(), self.entries.len());
                self.entries.push((key, vec![value]));
            }
        }
    }

    /// Number of distinct keys.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` when no key was seen.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterate `(key, values)` in first-insertion order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &[String])> {
        self.entries
            .iter()
            .map(|(key, values)| (key.as_str(), values.as_slice()))
    }

    /// All values for `key`.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&[String]> {
        self.positions
            .get(key)
            .map(|&index| self.entries[index].1.as_slice())
    }
}

impl serde::Serialize for FormFields {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(self.entries.len()))?;
        for (key, values) in &self.entries {
            map.serialize_entry(key, values)?;
        }
        map.end()
    }
}

/// Parse a URL-encoded body the way DRF's `FormParser` does.
///
/// `QueryDict` is constructed from the raw bytes with `encoding` and
/// `max_num_fields=settings.DATA_UPLOAD_MAX_NUMBER_FIELDS`; a body that fails
/// strict decoding is retried as ISO-8859-1 before the same parse.
///
/// # Errors
/// Returns [`ParserError::TooManyFields`] when the `&` count exceeds the limit.
pub fn parse_urlencoded(
    body: &[u8],
    charset: Charset,
    max_fields: usize,
) -> Result<FormFields, ParserError> {
    let text = match charset.decode_strict(body) {
        Ok(text) => text,
        Err(_) => body.iter().map(|byte| char::from(*byte)).collect(),
    };
    parse_qsl(&text, charset, max_fields)
}

/// Port of `urllib.parse.parse_qsl` as Django calls it.
fn parse_qsl(query: &str, charset: Charset, max_fields: usize) -> Result<FormFields, ParserError> {
    if query.is_empty() {
        return Ok(FormFields::new());
    }
    // `parse_qsl` checks `1 + qs.count('&')` before yielding anything.
    let counted = 1 + query.matches('&').count();
    if max_fields < counted {
        return Err(ParserError::TooManyFields);
    }
    let mut fields = FormFields::new();
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (name, value) = match pair.split_once('=') {
            Some((name, value)) => (name, value),
            None => (pair, ""),
        };
        fields.append(unquote_plus(name, charset), unquote_plus(value, charset));
    }
    Ok(fields)
}

fn unquote_plus(value: &str, charset: Charset) -> String {
    percent_decode(&value.replace('+', " "), charset)
}

/// Parse a URL-encoded body with default limits.
///
/// # Errors
/// Returns [`ParserError`] for the same reasons as [`parse_urlencoded`].
pub fn parse_urlencoded_with(
    body: &[u8],
    charset: Charset,
    limits: ParserLimits,
) -> Result<FormFields, ParserError> {
    parse_urlencoded(body, charset, limits.max_fields)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_keys_keep_order() {
        let fields = parse_urlencoded(
            b"name=Ren%C3%A9&dup=1&dup=2",
            Charset::Utf8,
            ParserLimits::default().max_fields,
        )
        .expect("parse");
        assert_eq!(fields.get("name"), Some(&["Ren\u{e9}".to_string()][..]));
        assert_eq!(
            fields.get("dup"),
            Some(&["1".to_string(), "2".to_string()][..])
        );
    }

    #[test]
    fn plus_and_percent_follow_python() {
        let fields = parse_urlencoded(
            b"a=1+2&b=%2B3",
            Charset::Utf8,
            ParserLimits::default().max_fields,
        )
        .expect("parse");
        assert_eq!(fields.get("a"), Some(&["1 2".to_string()][..]));
        assert_eq!(fields.get("b"), Some(&["+3".to_string()][..]));
        let fields = parse_urlencoded(
            b"a=%zz&b=%",
            Charset::Utf8,
            ParserLimits::default().max_fields,
        )
        .expect("parse");
        assert_eq!(fields.get("a"), Some(&["%zz".to_string()][..]));
        assert_eq!(fields.get("b"), Some(&["%".to_string()][..]));
    }

    #[test]
    fn latin1_charset_is_honoured() {
        let fields = parse_urlencoded(
            b"name=Ren\xe9",
            Charset::Latin1,
            ParserLimits::default().max_fields,
        )
        .expect("parse");
        assert_eq!(fields.get("name"), Some(&["Ren\u{e9}".to_string()][..]));
    }

    #[test]
    fn field_limit_counts_separators() {
        assert!(parse_urlencoded(b"a=1&", Charset::Utf8, 1).is_err());
        assert!(parse_urlencoded(b"&&a=1", Charset::Utf8, 1).is_err());
        assert!(parse_urlencoded(b"a=1", Charset::Utf8, 1).is_ok());
    }
}
