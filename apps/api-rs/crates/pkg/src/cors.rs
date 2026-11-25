// Copyright (c) 2023-present Plane Software, Inc. and contributors
// SPDX-License-Identifier: AGPL-3.0-only
// See the LICENSE file for details.

//! `corsheaders.middleware.CorsMiddleware`, byte-for-byte.
//!
//! Measured against django-cors-headers 4.9.0 with Plane's settings
//! (`contracts/golden/middleware-cases.json`). Three behaviours are easy to get
//! wrong and are therefore spelled out here:
//!
//! * the configured origins are compared on `scheme` + `netloc`, and `netloc`
//!   is case-sensitive, so `https://WEB.example` does not match
//!   `https://web.example`;
//! * `Vary: origin` is added for every request the middleware sees, even when
//!   the request has no `Origin` header or the origin is rejected;
//! * a preflight is answered by this middleware alone, before any inner
//!   middleware runs. That is why a preflight carries no `X-Frame-Options`.

use crate::config::CorsConfig;

/// Space-separated? No: corsheaders joins with `", "`, exactly this order.
pub const ALLOW_HEADERS: [&str; 7] = [
    "accept",
    "authorization",
    "content-type",
    "user-agent",
    "x-csrftoken",
    "x-requested-with",
    "X-API-Key",
];

/// `default_methods` from `corsheaders/defaults.py`.
pub const ALLOW_METHODS: [&str; 6] = ["DELETE", "GET", "OPTIONS", "PATCH", "POST", "PUT"];

/// `CORS_PREFLIGHT_MAX_AGE` default; Plane does not override it.
pub const PREFLIGHT_MAX_AGE: u64 = 86_400;

/// Header names the middleware adds.
pub mod header {
    /// `Access-Control-Allow-Origin`.
    pub const ALLOW_ORIGIN: &str = "access-control-allow-origin";
    /// `Access-Control-Allow-Credentials`.
    pub const ALLOW_CREDENTIALS: &str = "access-control-allow-credentials";
    /// `Access-Control-Allow-Headers`.
    pub const ALLOW_HEADERS: &str = "access-control-allow-headers";
    /// `Access-Control-Allow-Methods`.
    pub const ALLOW_METHODS: &str = "access-control-allow-methods";
    /// `Access-Control-Max-Age`.
    pub const MAX_AGE: &str = "access-control-max-age";
    /// `Access-Control-Request-Method`, the preflight marker.
    pub const REQUEST_METHOD: &str = "access-control-request-method";
}

/// The settings the middleware reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cors {
    allowed_origins: Vec<String>,
    allow_all: bool,
    allow_credentials: bool,
}

impl Cors {
    /// Build from the config loader's `_cors_settings` port.
    #[must_use]
    pub fn new(config: &CorsConfig) -> Self {
        Self {
            allowed_origins: config.allowed_origins.clone(),
            allow_all: config.allow_all,
            allow_credentials: config.allow_credentials,
        }
    }

    /// `CORS_ALLOWED_ORIGINS` as loaded, for diagnostics and tests.
    #[must_use]
    pub fn allowed_origins(&self) -> &[String] {
        &self.allowed_origins
    }

    /// `check_preflight`: only `OPTIONS` plus `Access-Control-Request-Method`.
    #[must_use]
    pub fn is_preflight(&self, method: &str, has_request_method_header: bool) -> bool {
        method == "OPTIONS" && has_request_method_header
    }

    /// `origin_found_in_white_lists`.
    ///
    /// `CORS_ALLOWED_ORIGIN_REGEXES` is empty in Plane and the
    /// `check_request_enabled` signal has no receivers, so only the exact list
    /// matters.
    #[must_use]
    pub fn origin_allowed(&self, origin: &str) -> bool {
        if origin == "null" {
            return self.allowed_origins.iter().any(|value| value == "null");
        }
        let Some((scheme, netloc)) = split_origin(origin) else {
            return false;
        };
        self.allowed_origins.iter().any(|configured| {
            split_origin(configured).is_some_and(|(other_scheme, other_netloc)| {
                other_scheme == scheme && other_netloc == netloc
            })
        })
    }
}

/// `urlsplit(url).scheme` and `.netloc`.
///
/// Returns `None` when either is empty, because both `_url_in_whitelist` and
/// the origin branch require a full origin. The scheme is lowercased like
/// `urlsplit`; the netloc is returned verbatim, so case still matters.
#[must_use]
pub fn split_origin(value: &str) -> Option<(String, String)> {
    let (scheme, rest) = value.split_once("://")?;
    if scheme.is_empty() {
        return None;
    }
    let mut chars = scheme.chars();
    let first = chars.next()?;
    if !first.is_ascii_alphabetic() {
        return None;
    }
    if !chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '+' | '-' | '.')) {
        return None;
    }
    let netloc = rest
        .find(['/', '?', '#'])
        .map_or(rest, |index| &rest[..index]);
    if netloc.is_empty() {
        return None;
    }
    Some((scheme.to_ascii_lowercase(), netloc.to_string()))
}

/// Add a value to `Vary` the way `django.utils.cache.patch_vary_headers` does.
///
/// Existing entries keep their order and case; a `*` collapses the header to a
/// single `*`; the new value is only appended when missing (case-insensitive).
pub fn patch_vary(existing: Option<&str>, new_header: &str) -> String {
    let mut values: Vec<String> = existing
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();

    let present = values
        .iter()
        .any(|value| value.eq_ignore_ascii_case(new_header));
    if !present && !values.iter().any(|value| value == "*") {
        values.push(new_header.to_string());
    }
    if values.iter().any(|value| value == "*") {
        return "*".to_string();
    }
    values.join(", ")
}

/// Apply the response headers for one request.
///
/// `response_headers` is mutated in place. `request_method` and `origin` are the
/// two request inputs the middleware reads.
pub fn apply_response_headers(
    cors: &Cors,
    request_method: &str,
    origin: Option<&str>,
    response_headers: &mut dyn Headers,
) {
    let vary = patch_vary(response_headers.get("vary").as_deref(), "origin");
    response_headers.set("vary", &vary);

    let Some(origin) = origin else {
        return;
    };
    if !cors.allow_all && !cors.origin_allowed(origin) {
        return;
    }

    let allow_origin = if cors.allow_all && !cors.allow_credentials {
        "*"
    } else {
        origin
    };
    response_headers.set(header::ALLOW_ORIGIN, allow_origin);
    if cors.allow_credentials {
        response_headers.set(header::ALLOW_CREDENTIALS, "true");
    }
    if request_method == "OPTIONS" {
        response_headers.set(header::ALLOW_HEADERS, &ALLOW_HEADERS.join(", "));
        response_headers.set(header::ALLOW_METHODS, &ALLOW_METHODS.join(", "));
        response_headers.set(header::MAX_AGE, &PREFLIGHT_MAX_AGE.to_string());
    }
}

/// Minimal header view, so this module stays free of the HTTP crate.
pub trait Headers {
    /// Case-insensitive lookup.
    fn get(&self, name: &str) -> Option<String>;
    /// Case-insensitive set.
    fn set(&mut self, name: &str, value: &str);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[derive(Default)]
    struct Map(BTreeMap<String, String>);

    impl Headers for Map {
        fn get(&self, name: &str) -> Option<String> {
            self.0.get(&name.to_ascii_lowercase()).cloned()
        }

        fn set(&mut self, name: &str, value: &str) {
            self.0.insert(name.to_ascii_lowercase(), value.to_string());
        }
    }

    fn cors() -> Cors {
        Cors::new(&CorsConfig {
            allowed_origins: vec![
                "https://web.plane.so".into(),
                "https://admin.plane.so".into(),
                "https://space.plane.so".into(),
            ],
            allow_all: false,
            allow_credentials: true,
        })
    }

    #[test]
    fn vary_appends_once_and_keeps_order() {
        assert_eq!(patch_vary(None, "origin"), "origin");
        assert_eq!(
            patch_vary(Some("Accept-Encoding"), "origin"),
            "Accept-Encoding, origin"
        );
        assert_eq!(
            patch_vary(Some("Accept-Encoding, origin"), "origin"),
            "Accept-Encoding, origin"
        );
        assert_eq!(patch_vary(Some("Origin"), "origin"), "Origin");
        assert_eq!(patch_vary(Some("*"), "origin"), "*");
        assert_eq!(patch_vary(Some("a, *"), "origin"), "*");
    }

    #[test]
    fn origin_comparison_is_scheme_and_case_sensitive_netloc() {
        let cors = cors();
        assert!(cors.origin_allowed("https://web.plane.so"));
        assert!(cors.origin_allowed("https://web.plane.so/"));
        assert!(cors.origin_allowed("https://web.plane.so/some/path"));
        assert!(!cors.origin_allowed("https://WEB.plane.so"));
        assert!(!cors.origin_allowed("http://web.plane.so"));
        assert!(!cors.origin_allowed("https://evil.example.com"));
        assert!(!cors.origin_allowed("null"));
        assert!(!cors.origin_allowed(""));
    }

    #[test]
    fn preflight_needs_options_and_request_method_header() {
        let cors = cors();
        assert!(cors.is_preflight("OPTIONS", true));
        assert!(!cors.is_preflight("OPTIONS", false));
        assert!(!cors.is_preflight("GET", true));
    }

    #[test]
    fn headers_match_django() {
        let cors = cors();
        let mut headers = Map::default();
        apply_response_headers(&cors, "OPTIONS", Some("https://web.plane.so"), &mut headers);
        assert_eq!(headers.get("vary").as_deref(), Some("origin"));
        assert_eq!(
            headers.get("access-control-allow-origin").as_deref(),
            Some("https://web.plane.so")
        );
        assert_eq!(
            headers.get("access-control-allow-credentials").as_deref(),
            Some("true")
        );
        assert_eq!(
            headers.get("access-control-allow-headers").as_deref(),
            Some(
                "accept, authorization, content-type, user-agent, x-csrftoken, x-requested-with, X-API-Key"
            )
        );
        assert_eq!(
            headers.get("access-control-allow-methods").as_deref(),
            Some("DELETE, GET, OPTIONS, PATCH, POST, PUT")
        );
        assert_eq!(
            headers.get("access-control-max-age").as_deref(),
            Some("86400")
        );
    }

    #[test]
    fn rejected_origin_still_gets_vary() {
        let cors = cors();
        let mut headers = Map::default();
        apply_response_headers(&cors, "GET", Some("https://evil.example.com"), &mut headers);
        assert_eq!(headers.get("vary").as_deref(), Some("origin"));
        assert_eq!(headers.get("access-control-allow-origin"), None);
    }

    #[test]
    fn allow_all_without_credentials_answers_star() {
        let cors = Cors::new(&CorsConfig {
            allowed_origins: Vec::new(),
            allow_all: true,
            allow_credentials: false,
        });
        let mut headers = Map::default();
        apply_response_headers(&cors, "GET", Some("https://anything.test"), &mut headers);
        assert_eq!(
            headers.get("access-control-allow-origin").as_deref(),
            Some("*")
        );
        assert_eq!(headers.get("access-control-allow-credentials"), None);
    }
}
