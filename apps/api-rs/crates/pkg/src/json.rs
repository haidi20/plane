// Copyright (c) 2023-present Plane Software, Inc. and contributors
// SPDX-License-Identifier: AGPL-3.0-only
// See the LICENSE file for details.

//! The two JSON renderings Django uses, kept byte-compatible.
//!
//! * `JsonResponse` (used by `plane/web/views.py` and the 404 handler) writes
//!   `{"key": "value"}` with a space after the colon and preserves key order.
//! * DRF renders compact JSON (`{"key":"value"}`).
//!
//! Mixing them up changes bytes on the wire, so both live here with names that
//! say which one a handler is supposed to use.

use serde::Serialize;

/// Compact rendering, matching DRF.
///
/// # Errors
/// Returns the underlying `serde_json` error when the value cannot be encoded.
pub fn compact_json<T: Serialize>(value: &T) -> serde_json::Result<String> {
    serde_json::to_string(value)
}

/// `JsonResponse`-style rendering: one space after `:` and after `,`, ASCII-only.
///
/// Django\'s encoder emits `{"status": "OK"}`; matching that spacing keeps
/// byte-level diffs empty for the door that Django still owns.
///
/// `json.dumps` also defaults to `ensure_ascii=True`, so Django writes
/// `"Ren\u00e9"` while DRF writes raw UTF-8. Both details are part of the wire
/// contract, so they live here rather than at the call sites.
///
/// # Errors
/// Returns the underlying `serde_json` error when the value cannot be encoded.
pub fn spaced_json<T: Serialize>(value: &T) -> serde_json::Result<String> {
    let compact = serde_json::to_string(value)?;
    Ok(escape_non_ascii(&space_after_separators(&compact)))
}

/// Escape every non-ASCII scalar the way Python\'s `json.dumps` does with the
/// default `ensure_ascii=True`: `\uXXXX` in lowercase hex, using a surrogate
/// pair for astral code points.
///
/// `serde_json` always emits raw UTF-8, so this runs only on the Django path.
fn escape_non_ascii(input: &str) -> String {
    if input.is_ascii() {
        return input.to_string();
    }
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        let code = ch as u32;
        if code <= 0x7f {
            out.push(ch);
        } else if code <= 0xffff {
            push_escape(&mut out, code);
        } else {
            // Astral code points become a UTF-16 surrogate pair, exactly like
            // Python\'s encoder.
            let adjusted = code - 0x1_0000;
            let high = 0xd800 + (adjusted >> 10);
            let low = 0xdc00 + (adjusted & 0x3ff);
            push_escape(&mut out, high);
            push_escape(&mut out, low);
        }
    }
    out
}

fn push_escape(out: &mut String, code: u32) {
    use std::fmt::Write as _;
    // `write!` into the existing buffer avoids one allocation per escape.
    let _ = write!(out, "\\u{code:04x}");
}

fn space_after_separators(input: &str) -> String {
    let mut out = String::with_capacity(input.len() + input.len() / 8);
    let mut in_string = false;
    let mut escaped = false;
    for ch in input.chars() {
        out.push(ch);
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            ':' | ',' => out.push(' '),
            _ => {}
        }
    }
    out
}

/// A rendered body plus the `Content-Type` it must carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonResponse {
    /// Already-serialized body.
    pub body: String,
    /// Value for the `Content-Type` header.
    pub content_type: &'static str,
}

impl JsonResponse {
    /// `JsonResponse`-style body (`application/json`).
    ///
    /// # Errors
    /// Returns the underlying `serde_json` error when the value cannot be encoded.
    pub fn django<T: Serialize>(value: &T) -> serde_json::Result<Self> {
        Ok(Self {
            body: spaced_json(value)?,
            content_type: "application/json",
        })
    }

    /// DRF-style compact body (`application/json`).
    ///
    /// # Errors
    /// Returns the underlying `serde_json` error when the value cannot be encoded.
    pub fn drf<T: Serialize>(value: &T) -> serde_json::Result<Self> {
        Ok(Self {
            body: compact_json(value)?,
            content_type: "application/json",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn django_health_matches_golden_bytes() {
        let rendered = spaced_json(&json!({"status": "OK"})).expect("encode");
        assert_eq!(rendered, r#"{"status": "OK"}"#);
    }

    #[test]
    fn drf_stays_compact() {
        let rendered = compact_json(&json!({"status": "OK"})).expect("encode");
        assert_eq!(rendered, r#"{"status":"OK"}"#);
    }

    #[test]
    fn separators_inside_strings_are_untouched() {
        let rendered = spaced_json(&json!({"a": "x:y,z"})).expect("encode");
        assert_eq!(rendered, r#"{"a": "x:y,z"}"#);
    }

    #[test]
    fn django_escapes_non_ascii_but_drf_keeps_utf8() {
        // Measured: JsonResponse -> b'{"name": "Ren\\u00e9 \\u2014 \\u4f60\\u597d"}'
        //           JSONRenderer -> b'{"name":"Ren\xc3\xa9 \xe2\x80\x94 \xe4\xbd\xa0\xe5\xa5\xbd"}'
        let value = json!({"name": "Ren\u{e9} \u{2014} \u{4f60}\u{597d}"});
        assert_eq!(
            spaced_json(&value).expect("encode"),
            r#"{"name": "Ren\u00e9 \u2014 \u4f60\u597d"}"#
        );
        assert_eq!(
            compact_json(&value).expect("encode"),
            "{\"name\":\"Ren\u{e9} \u{2014} \u{4f60}\u{597d}\"}"
        );
    }

    #[test]
    fn django_uses_surrogate_pairs_for_astral_characters() {
        // Measured: JsonResponse -> b'{"emoji": "\\ud83d\\ude00"}'
        let rendered = spaced_json(&json!({"emoji": "\u{1f600}"})).expect("encode");
        assert_eq!(rendered, r#"{"emoji": "\ud83d\ude00"}"#);
    }

    #[test]
    fn ascii_only_bodies_are_not_reallocated() {
        let rendered = spaced_json(&json!({"status": "OK"})).expect("encode");
        assert_eq!(rendered, r#"{"status": "OK"}"#);
    }
}
