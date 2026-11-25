// Copyright (c) 2023-present Plane Software, Inc. and contributors
// SPDX-License-Identifier: AGPL-3.0-only
// See the LICENSE file for details.

//! `Content-Type` parsing, ported from `django.utils.http`.
//!
//! `_parseparam` is not a simple `split(';')`: an even number of unescaped `"`
//! before a `;` means the separator is inside a quoted value. The quoting rules
//! also matter for multipart boundaries (`boundary="---x"`), which is why this
//! lives in one place rather than in each parser.
//!
//! `_MediaType.match` compares every parameter on the *left* side only, which is
//! the parser's declared media type. Parameters the request adds are ignored:
//! `media_type_matches("application/json", "application/json; charset=utf-8")`
//! is `True` in DRF, so a charset never costs a 415. The direction matters —
//! `media_type_matches("application/json; charset=utf-8", "application/json")`
//! is `False` — but DRF always passes the bare parser type on the left.

use std::collections::BTreeMap;

/// A parsed `Content-Type` value: the main type plus its parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedContentType {
    /// Lowercased `type/subtype`.
    pub media_type: String,
    /// Parameters with lowercased names and unfolded values.
    pub params: BTreeMap<String, String>,
}

impl ParsedContentType {
    /// The value DRF sees when the header is absent.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            media_type: String::new(),
            params: BTreeMap::new(),
        }
    }

    /// `true` for `application/x-www-form-urlencoded` and `multipart/form-data`.
    #[must_use]
    pub fn is_form(&self) -> bool {
        self.media_type == "application/x-www-form-urlencoded"
            || self.media_type == "multipart/form-data"
    }
}

/// Port of `django.utils.http._parseparam`.
///
/// The loop mirrors the Python source line for line, including `end = -1` when
/// no `;` remains.
fn parse_params(line: &str) -> Vec<String> {
    let owned = format!(";{line}");
    let mut rest: &str = &owned;
    let mut output = Vec::new();
    while let Some(stripped) = rest.strip_prefix(';') {
        rest = stripped;
        let mut end = rest.find(';').map_or(-1, |index| index as isize);
        while end > 0 {
            let slice = &rest[..end as usize];
            let quotes = slice.matches('"').count() as isize;
            let escaped = slice.matches("\\\"").count() as isize;
            if (quotes - escaped) % 2 == 0 {
                break;
            }
            end = match rest[(end as usize + 1)..].find(';') {
                Some(offset) => end + 1 + offset as isize,
                None => -1,
            };
        }
        if end < 0 {
            end = rest.len() as isize;
        }
        output.push(rest[..end as usize].trim().to_string());
        rest = &rest[end as usize..];
    }
    output
}

/// Port of `django.utils.http.parse_header_parameters`.
#[must_use]
pub fn parse_header_parameters(line: &str) -> ParsedContentType {
    let parts = parse_params(line);
    let mut iter = parts.into_iter();
    let media_type = iter.next().unwrap_or_default().to_lowercase();
    let mut params = BTreeMap::new();
    for part in iter {
        let Some(index) = part.find('=') else {
            continue;
        };
        let mut name = part[..index].trim().to_lowercase();
        let mut value = part[index + 1..].trim().to_string();
        let mut has_encoding = false;
        if name.ends_with('*') {
            name.pop();
            if part.matches('\'').count() == 2 {
                has_encoding = true;
            }
        }
        if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
            value = value[1..value.len() - 1]
                .replace("\\\\", "\\")
                .replace("\\\"", "\"");
        }
        if has_encoding {
            let mut pieces = value.splitn(3, '\'');
            let encoding = pieces.next().unwrap_or("utf-8");
            let _language = pieces.next().unwrap_or("");
            let encoded = pieces.next().unwrap_or("");
            value = decode_percent(encoded, encoding);
        }
        params.insert(name, value);
    }
    ParsedContentType { media_type, params }
}

fn decode_percent(value: &str, encoding: &str) -> String {
    match super::charset::resolve(encoding) {
        super::charset::CharsetResolution::Use(charset) => {
            super::charset::percent_decode(value, charset)
        }
        _ => value.to_string(),
    }
}

/// `rest_framework.request.is_form_media_type`.
#[must_use]
pub fn is_form_media_type(media_type: &str) -> bool {
    if media_type.is_empty() || media_type.len() > super::MAX_MEDIA_TYPE_LENGTH {
        return false;
    }
    parse_header_parameters(media_type).is_form()
}

/// `rest_framework.utils.mediatypes.media_type_matches`.
///
/// `left` is the parser's declared media type, `right` the request's.
#[must_use]
pub fn media_type_matches(left: &str, right: &ParsedContentType) -> bool {
    let lhs = parse_header_parameters(left);
    for (key, value) in &lhs.params {
        if key == "q" {
            continue;
        }
        if right.params.get(key) != Some(value) {
            return false;
        }
    }
    let (lhs_main, lhs_sub) = split_type(&lhs.media_type);
    let (rhs_main, rhs_sub) = split_type(&right.media_type);
    if lhs_sub != "*" && rhs_sub != "*" && rhs_sub != lhs_sub {
        return false;
    }
    if lhs_main != "*" && rhs_main != "*" && rhs_main != lhs_main {
        return false;
    }
    true
}

fn split_type(media_type: &str) -> (&str, &str) {
    match media_type.split_once('/') {
        Some((main, sub)) => (main, sub),
        None => (media_type, ""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parameters_keep_quotes_and_params_apart() {
        let parsed = parse_header_parameters("Application/JSON; Charset=UTF-8");
        assert_eq!(parsed.media_type, "application/json");
        assert_eq!(
            parsed.params.get("charset").map(String::as_str),
            Some("UTF-8")
        );
        let parsed = parse_header_parameters("multipart/form-data; boundary=\"----x\"");
        assert_eq!(
            parsed.params.get("boundary").map(String::as_str),
            Some("----x")
        );
        let parsed = parse_header_parameters("text/plain; x=1; x=2");
        assert_eq!(parsed.params.get("x").map(String::as_str), Some("2"));
    }

    #[test]
    fn semicolons_inside_quotes_do_not_split() {
        let parsed = parse_header_parameters("multipart/form-data; boundary=\"a;b\"");
        assert_eq!(parsed.media_type, "multipart/form-data");
        assert_eq!(
            parsed.params.get("boundary").map(String::as_str),
            Some("a;b")
        );
    }

    #[test]
    fn matching_follows_drf() {
        let json = parse_header_parameters("application/json");
        assert!(media_type_matches("application/json", &json));
        let with_charset = parse_header_parameters("application/json; charset=utf-8");
        // Only the left side's parameters are compared, so a request charset
        // never prevents a match.
        assert!(media_type_matches("application/json", &with_charset));
        // The reverse direction does not hold: the parser would require a
        // charset the request omitted.
        let bare = parse_header_parameters("application/json");
        let with_charset_left = parse_header_parameters("application/json; charset=utf-8");
        assert!(!media_type_matches(
            "application/json; charset=utf-8",
            &bare
        ));
        assert!(media_type_matches(
            "application/json; charset=utf-8",
            &with_charset_left
        ));
        let text = parse_header_parameters("text/plain");
        assert!(!media_type_matches("application/json", &text));
        let json_upper = parse_header_parameters("Application/JSON; Charset=UTF-8");
        assert!(media_type_matches("application/json", &json_upper));
        let empty = ParsedContentType::empty();
        assert!(!media_type_matches("application/json", &empty));
        assert!(!is_form_media_type(""));
        assert!(is_form_media_type("application/x-www-form-urlencoded"));
        assert!(is_form_media_type("multipart/form-data; boundary=x"));
    }
}
