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

/// `JsonResponse`-style rendering: one space after `:` and after `,`.
///
/// Django\'s encoder emits `{"status": "OK"}`; matching that spacing keeps
/// byte-level diffs empty for the door that Django still owns.
///
/// # Errors
/// Returns the underlying `serde_json` error when the value cannot be encoded.
pub fn spaced_json<T: Serialize>(value: &T) -> serde_json::Result<String> {
    let compact = serde_json::to_string(value)?;
    Ok(space_after_separators(&compact))
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
}
