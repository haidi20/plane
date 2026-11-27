// Copyright (c) 2023-present Plane Software, Inc. and contributors
// SPDX-License-Identifier: AGPL-3.0-only
// See the LICENSE file for details.

//! Cookie bytes: `parse_cookie`, `_unquote`, `Morsel.OutputString`, `http_date`.
//!
//! The session door reads and writes cookies, so the exact bytes matter. Four
//! pieces of CPython/Django are reproduced here, measured against
//! `contracts/golden/auth-cases.json` (`cookies.*`):
//!
//! | Behaviour | Source |
//! |-----------|--------|
//! | a chunk without `=` becomes key `""` | `django.http.cookie.parse_cookie` |
//! | duplicates keep the **last** value | dict assignment |
//! | `"..."` is unquoted, `\073` becomes `;` | `http.cookies._unquote` |
//! | attributes sorted by lowercase name | `http.cookies.Morsel.OutputString` |
//!
//! `Morsel.OutputString` sorts `self.items()` and skips empty values, which is
//! why a session cookie renders `expires`, `HttpOnly`, `Max-Age`, `Path`,
//! `SameSite`, `Secure` and not in call order. The `%3s` month in `_getdate`
//! is a no-op for the three-letter English names, so `http_date` is chrono's
//! `%a, %d %b %Y %H:%M:%S GMT` exactly.

use chrono::{DateTime, Utc};

/// `string.ascii_letters + string.digits + "!#$%&'*+-.^_`|~:"`.
const LEGAL_CHARS: &str =
    "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789!#$%&'*+-.^_`|~:";

/// `_UnescapedChars = _LegalChars + " ()/<=>?@[]{}"`.
const UNESCAPED_CHARS: &str =
    "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789!#$%&'*+-.^_`|~: ()/<=>?@[]{}";

/// `expires` Django writes from `delete_cookie`.
pub const EPOCH: &str = "Thu, 01 Jan 1970 00:00:00 GMT";

/// Python's `str.strip()` whitespace: Unicode `White_Space` plus the four
/// separators CPython also treats as space (`\x1c`-`\x1f`).
fn is_python_space(value: char) -> bool {
    value.is_whitespace() || matches!(value, '\u{1c}'..='\u{1f}')
}

fn py_strip(value: &str) -> &str {
    value.trim_matches(is_python_space)
}

/// `_is_legal_key(...).fullmatch`: can this value go out unquoted?
#[must_use]
pub fn is_legal_value(value: &str) -> bool {
    !value.is_empty() && value.chars().all(|value| LEGAL_CHARS.contains(value))
}

/// `http.cookies._quote`: leave legal values alone, otherwise quote and escape.
///
/// Bytes outside `_UnescapedChars` become `\%03o`, except `"` and `\`, which
/// become `\"` and `\\`.
#[must_use]
pub fn quote(value: &str) -> String {
    if is_legal_value(value) {
        return value.to_string();
    }
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('"');
    for character in value.chars() {
        match character {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            other if UNESCAPED_CHARS.contains(other) => quoted.push(other),
            other => {
                let mut buffer = [0_u8; 4];
                for byte in other.encode_utf8(&mut buffer).as_bytes() {
                    quoted.push_str(&format!("\\{byte:03o}"));
                }
            }
        }
    }
    quoted.push('"');
    quoted
}

/// `http.cookies._unquote_replace`: `\073` is octal, `\x` is `x`.
fn push_unquoted(inner: &str) -> String {
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars().peekable();
    while let Some(character) = chars.next() {
        if character != '\\' {
            out.push(character);
            continue;
        }
        let digits: Vec<char> = chars.clone().take(3).collect();
        if digits.len() == 3
            && ('0'..='3').contains(&digits[0])
            && digits[1..].iter().all(|digit| ('0'..='7').contains(digit))
        {
            let value = digits
                .iter()
                .fold(0_u32, |acc, digit| acc * 8 + digit.to_digit(8).unwrap_or(0));
            if let Some(decoded) = char::from_u32(value) {
                out.push(decoded);
            }
            for _ in 0..3 {
                chars.next();
            }
        } else if let Some(escaped) = chars.next() {
            out.push(escaped);
        } else {
            out.push('\\');
        }
    }
    out
}

/// `http.cookies._unquote`: only a value wrapped in double quotes is decoded.
#[must_use]
pub fn unquote(value: &str) -> String {
    if value.len() < 2 || !value.starts_with('"') || !value.ends_with('"') {
        return value.to_string();
    }
    push_unquoted(&value[1..value.len() - 1])
}

/// `django.http.cookie.parse_cookie`, duplicates resolved to the last value.
///
/// The returned vector keeps Python's dict insertion order: the position of a
/// repeated key is the first occurrence, its value the last one.
#[must_use]
pub fn parse_cookie(raw: &str) -> Vec<(String, String)> {
    let mut cookies: Vec<(String, String)> = Vec::new();
    for chunk in raw.split(';') {
        let (key, value) = match chunk.split_once('=') {
            Some((key, value)) => (py_strip(key), py_strip(value)),
            // "Assume an empty name per bugzilla.mozilla.org/show_bug.cgi?id=169091"
            None => ("", py_strip(chunk)),
        };
        if key.is_empty() && value.is_empty() {
            continue;
        }
        let value = unquote(value);
        match cookies.iter_mut().find(|(stored, _)| stored == key) {
            Some((_, stored)) => *stored = value,
            None => cookies.push((key.to_string(), value)),
        }
    }
    cookies
}

/// Value of one cookie, matched exactly (case-sensitive).
#[must_use]
pub fn cookie_value<'a>(cookies: &'a [(String, String)], name: &str) -> Option<&'a str> {
    cookies
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}

/// `django.utils.http.http_date` / `formatdate(usegmt=True)`.
///
/// `None` when the timestamp is outside chrono's range, where Python would
/// raise `OverflowError`/`OSError` from `time.gmtime`.
#[must_use]
pub fn http_date(epoch_seconds: i64) -> Option<String> {
    let time = DateTime::<Utc>::from_timestamp(epoch_seconds, 0)?;
    Some(time.format("%a, %d %b %Y %H:%M:%S GMT").to_string())
}

/// One `Set-Cookie` value, in the shape `Morsel` holds it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SetCookie {
    /// Cookie name.
    pub name: String,
    /// Raw value; quoted only if it is not a legal token.
    pub value: String,
    /// `expires`, already a date string ([`http_date`]).
    pub expires: Option<String>,
    /// `max-age`, in seconds.
    pub max_age: Option<i64>,
    /// `path`; Django's default is `/`.
    pub path: Option<String>,
    /// `domain`.
    pub domain: Option<String>,
    /// `secure` flag.
    pub secure: bool,
    /// `httponly` flag.
    pub http_only: bool,
    /// `samesite`; Django validates it is lax, none or strict.
    pub same_site: Option<String>,
}

impl SetCookie {
    /// A cookie with a name, a value and Django's default `Path=/`.
    #[must_use]
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
            path: Some("/".to_string()),
            ..Self::default()
        }
    }

    /// `max-age`; `expires` is derived as `now + max_age` if it is still unset,
    /// which is what `HttpResponse.set_cookie` does for IE.
    #[must_use]
    pub fn with_max_age(mut self, max_age: i64, now: i64) -> Self {
        self.max_age = Some(max_age);
        if self.expires.is_none() {
            self.expires = http_date(now.saturating_add(max_age));
        }
        self
    }

    /// Explicit `expires` date, already formatted with [`http_date`].
    #[must_use]
    pub fn with_expires(mut self, expires: impl Into<String>) -> Self {
        self.expires = Some(expires.into());
        self
    }

    /// `path`.
    #[must_use]
    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }

    /// `domain`.
    #[must_use]
    pub fn with_domain(mut self, domain: impl Into<String>) -> Self {
        self.domain = Some(domain.into());
        self
    }

    /// `secure` flag.
    #[must_use]
    pub fn with_secure(mut self, secure: bool) -> Self {
        self.secure = secure;
        self
    }

    /// `httponly` flag.
    #[must_use]
    pub fn with_http_only(mut self, http_only: bool) -> Self {
        self.http_only = http_only;
        self
    }

    /// `samesite`; Django rejects anything but lax, none or strict.
    #[must_use]
    pub fn with_same_site(mut self, same_site: impl Into<String>) -> Self {
        self.same_site = Some(same_site.into());
        self
    }

    /// `Morsel.OutputString`: `<name>=<coded value>` then attributes sorted by
    /// their lowercase name, empty ones skipped, flags without a value.
    #[must_use]
    pub fn render(&self) -> String {
        let mut pairs: Vec<(&str, String)> = Vec::new();
        if let Some(domain) = &self.domain {
            pairs.push(("domain", format!("Domain={domain}")));
        }
        if let Some(expires) = &self.expires {
            pairs.push(("expires", format!("expires={expires}")));
        }
        if self.http_only {
            pairs.push(("httponly", "HttpOnly".to_string()));
        }
        if let Some(max_age) = self.max_age {
            pairs.push(("max-age", format!("Max-Age={max_age}")));
        }
        if let Some(path) = &self.path {
            pairs.push(("path", format!("Path={path}")));
        }
        if let Some(same_site) = &self.same_site {
            pairs.push(("samesite", format!("SameSite={same_site}")));
        }
        if self.secure {
            pairs.push(("secure", "Secure".to_string()));
        }
        pairs.sort_by(|left, right| left.0.cmp(right.0));

        let mut rendered = vec![format!("{}={}", self.name, quote(&self.value))];
        rendered.extend(pairs.into_iter().map(|(_, pair)| pair));
        rendered.join("; ")
    }
}

/// `HttpResponse.delete_cookie`, including the `Secure` rule.
///
/// Browsers may ignore the deletion when the cookie is `__Host-`/`__Secure-`
/// prefixed or `SameSite=None`, so Django adds `Secure` in those cases.
#[must_use]
pub fn delete_cookie(
    name: &str,
    path: Option<&str>,
    domain: Option<&str>,
    same_site: Option<&str>,
) -> SetCookie {
    let secure = name.starts_with("__Secure-")
        || name.starts_with("__Host-")
        || same_site.is_some_and(|value| value.eq_ignore_ascii_case("none"));
    SetCookie {
        name: name.to_string(),
        value: String::new(),
        expires: Some(EPOCH.to_string()),
        max_age: Some(0),
        path: path.map(str::to_string),
        domain: domain.map(str::to_string),
        secure,
        http_only: false,
        same_site: same_site.map(str::to_string),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_matches_django_for_the_recorded_headers() {
        let cases = [
            ("session-id=abc123", vec![("session-id", "abc123")]),
            (
                "  session-id = abc123 ; other=1",
                vec![("session-id", "abc123"), ("other", "1")],
            ),
            (
                "session-id=first; session-id=second",
                vec![("session-id", "second")],
            ),
            ("session-id=\"abc123\"", vec![("session-id", "abc123")]),
            ("Session-Id=abc123", vec![("Session-Id", "abc123")]),
            ("session-id", vec![("", "session-id")]),
            (
                "session-id=; other=1",
                vec![("session-id", ""), ("other", "1")],
            ),
            (
                "$Version=1; session-id=abc; $Path=/",
                vec![("$Version", "1"), ("session-id", "abc"), ("$Path", "/")],
            ),
            (
                "session-id=\"a;b\"",
                vec![("session-id", "\"a"), ("", "b\"")],
            ),
            ("session-id=\"abc", vec![("session-id", "\"abc")]),
            ("session-id=a,b", vec![("session-id", "a,b")]),
            ("session-id=\"a\\073b\"", vec![("session-id", "a;b")]),
            ("session-id=\"a b\"", vec![("session-id", "a b")]),
            ("session-id=\"a\\\"b\"", vec![("session-id", "a\"b")]),
            ("session-id=a%20b", vec![("session-id", "a%20b")]),
        ];
        for (raw, expected) in cases {
            let actual = parse_cookie(raw);
            let expected: Vec<(String, String)> = expected
                .into_iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect();
            assert_eq!(actual, expected, "{raw}");
        }
    }

    #[test]
    fn a_duplicate_key_keeps_its_first_position_and_last_value() {
        let parsed = parse_cookie("a=1; b=2; a=3");
        assert_eq!(
            parsed,
            vec![
                ("a".to_string(), "3".to_string()),
                ("b".to_string(), "2".to_string())
            ]
        );
    }

    #[test]
    fn quoting_follows_python_str_translate() {
        assert_eq!(quote("abc123"), "abc123");
        assert_eq!(quote(""), "\"\"");
        assert_eq!(quote("a b;c"), "\"a b\\073c\"");
        assert_eq!(quote("a\"b"), "\"a\\\"b\"");
        assert_eq!(quote("a\\b"), "\"a\\\\b\"");
        assert_eq!(unquote("\"a b\\073c\""), "a b;c");
        assert_eq!(unquote("\"a\\\"b\""), "a\"b");
        assert_eq!(unquote("\"abc"), "\"abc");
    }

    #[test]
    fn http_date_is_imf_fixdate() {
        assert_eq!(http_date(0).as_deref(), Some(EPOCH));
        assert_eq!(
            http_date(1).as_deref(),
            Some("Thu, 01 Jan 1970 00:00:01 GMT")
        );
        assert_eq!(
            http_date(1_795_649_400).as_deref(),
            Some("Wed, 25 Nov 2026 23:30:00 GMT")
        );
    }

    #[test]
    fn set_cookie_matches_the_recorded_bytes() {
        let session = SetCookie::new("session-id", "abc123")
            .with_max_age(604_800, 0)
            .with_expires(EPOCH)
            .with_http_only(true)
            .with_same_site("Lax");
        assert_eq!(
            session.render(),
            "session-id=abc123; expires=Thu, 01 Jan 1970 00:00:00 GMT; HttpOnly; Max-Age=604800; Path=/; SameSite=Lax"
        );

        let admin = SetCookie::new("admin-session-id", "adm1")
            .with_max_age(3_600, 0)
            .with_expires(EPOCH)
            .with_http_only(true)
            .with_same_site("Lax")
            .with_secure(true);
        assert_eq!(
            admin.render(),
            "admin-session-id=adm1; expires=Thu, 01 Jan 1970 00:00:00 GMT; HttpOnly; Max-Age=3600; Path=/; SameSite=Lax; Secure"
        );

        let browser = SetCookie::new("session-id", "abc")
            .with_http_only(true)
            .with_same_site("Lax");
        assert_eq!(
            browser.render(),
            "session-id=abc; HttpOnly; Path=/; SameSite=Lax"
        );
    }

    #[test]
    fn set_cookie_derives_expires_from_max_age_and_quotes_values() {
        let quoted = SetCookie::new("s", "a b;c");
        assert_eq!(quoted.render(), "s=\"a b\\073c\"; Path=/");
        let empty = SetCookie::new("s", "");
        assert_eq!(empty.render(), "s=\"\"; Path=/");

        let derived = SetCookie::new("s", "v").with_max_age(60, 1_700_000_000);
        assert_eq!(
            derived.expires.as_deref(),
            Some("Tue, 14 Nov 2023 22:14:20 GMT")
        );
    }

    #[test]
    fn delete_cookie_adds_secure_for_prefixes_and_samesite_none() {
        assert_eq!(
            delete_cookie("session-id", Some("/"), None, Some("Lax")).render(),
            "session-id=\"\"; expires=Thu, 01 Jan 1970 00:00:00 GMT; Max-Age=0; Path=/; SameSite=Lax"
        );
        assert_eq!(
            delete_cookie("__Host-session", Some("/"), None, Some("None")).render(),
            "__Host-session=\"\"; expires=Thu, 01 Jan 1970 00:00:00 GMT; Max-Age=0; Path=/; SameSite=None; Secure"
        );
        assert_eq!(
            delete_cookie("__Secure-x", Some("/"), None, None).render(),
            "__Secure-x=\"\"; expires=Thu, 01 Jan 1970 00:00:00 GMT; Max-Age=0; Path=/; Secure"
        );
    }
}
