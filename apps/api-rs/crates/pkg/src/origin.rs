// Copyright (c) 2023-present Plane Software, Inc. and contributors
// SPDX-License-Identifier: AGPL-3.0-only
// See the LICENSE file for details.

//! `plane/authentication/session.py`: the Origin check that replaces CSRF.
//!
//! `BaseSessionAuthentication.enforce_csrf` is called by DRF for every
//! cookie-authenticated unsafe method. It allows the request when:
//!
//! 1. the method is safe (`GET`, `HEAD`, `OPTIONS`, `TRACE`);
//! 2. there is no `Origin` header (non-browser client);
//! 3. the origin is the request's own host, compared on `host[:port]` only;
//! 4. the origin is one of the configured URLs, normalised to `scheme://host[:port]`.
//!
//! Anything else is a 403 with `CSRF Failed: origin not allowed`.
//!
//! Note the difference from `plane_pkg::cors`: CORS compares `scheme` **and**
//! case-sensitive `netloc`, while this check compares only `host[:port]` against
//! the request's own host, so `http://` and `https://` are the same origin here.
//! Both are measured; they are not interchangeable.

/// Methods DRF never checks.
pub const SAFE_METHODS: [&str; 4] = ["GET", "HEAD", "OPTIONS", "TRACE"];

/// Message `PermissionDenied` renders for a rejected origin.
pub const DENIED: &str = "CSRF Failed: origin not allowed";

/// `normalize_origin`: `scheme://host[:port]`, lowercased; `None` without both.
///
/// `urlsplit` also rejects an unbalanced IPv6 bracket, which maps to `None`.
#[must_use]
pub fn normalize_origin(url: &str) -> Option<String> {
    let (scheme, rest) = url.trim().split_once("://")?;
    if scheme.is_empty() {
        return None;
    }
    let netloc = rest
        .find(['/', '?', '#'])
        .map_or(rest, |index| &rest[..index]);
    if netloc.is_empty() {
        return None;
    }
    if netloc.matches('[').count() != netloc.matches(']').count() {
        return None;
    }
    Some(format!("{scheme}://{netloc}").to_ascii_lowercase())
}

/// Port of `enforce_csrf`: the safe-method guard **then** the origin check.
///
/// Measured (`contracts/golden/middleware-cases.json`, `origin-check`): a `GET`
/// from an evil origin is allowed because the guard returns before the origin
/// is looked at. `is_allowed_origin` on its own would deny it, so the two must
/// stay separate.
///
/// The method comparison is Django's `request.method in SAFE_METHODS`, which is
/// **case-sensitive**: `"get"` is not safe (measured in
/// `contracts/golden/auth-cases.json`, `session.csrf`). A lowercase method only
/// reaches this point from a non-WSGI caller, because the WSGI server upper-cases
/// `REQUEST_METHOD`; the port keeps the stricter rule anyway.
pub fn enforce_csrf(
    method: &str,
    origin: Option<&str>,
    request_host: &str,
    allowed: &[String],
) -> Result<(), &'static str> {
    if SAFE_METHODS.contains(&method) {
        return Ok(());
    }
    if is_allowed_origin(origin, request_host, allowed) {
        Ok(())
    } else {
        Err(DENIED)
    }
}

/// Port of `is_allowed_origin` (no safe-method guard).
///
/// `request_host` is Django's `request.get_host()`; `allowed` is the set
/// `allowed_origins()` builds from the five base URLs and `CORS_ALLOWED_ORIGINS`
/// (which equals `CSRF_TRUSTED_ORIGINS` in Plane).
#[must_use]
pub fn is_allowed_origin(origin: Option<&str>, request_host: &str, allowed: &[String]) -> bool {
    let Some(origin) = origin else {
        return true;
    };
    let Some(normalized) = normalize_origin(origin) else {
        return false;
    };
    // `origin.split("://", 1)[1] == request.get_host().lower()`
    if normalized
        .split_once("://")
        .is_some_and(|(_, host)| host.eq_ignore_ascii_case(request_host))
    {
        return true;
    }
    let normalized_allowed: Vec<String> = allowed
        .iter()
        .filter_map(|url| normalize_origin(url))
        .collect();
    normalized_allowed.iter().any(|value| value == &normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allowed() -> Vec<String> {
        vec![
            "https://web.plane.so".into(),
            "https://admin.plane.so".into(),
        ]
    }

    #[test]
    fn safe_methods_pass_even_from_an_evil_origin() {
        // Measured: `is_allowed_origin` alone denies this, `enforce_csrf` allows.
        assert!(!is_allowed_origin(
            Some("https://evil.test"),
            "api.test",
            &allowed()
        ));
        assert_eq!(
            enforce_csrf("GET", Some("https://evil.test"), "api.test", &allowed()),
            Ok(())
        );
        assert_eq!(
            enforce_csrf("POST", Some("https://evil.test"), "api.test", &allowed()),
            Err(DENIED)
        );
    }

    #[test]
    fn the_safe_method_guard_is_case_sensitive_like_django() {
        // `if request.method in SAFE_METHODS` does not lower-case.
        for method in ["get", "Get", "head", "trace"] {
            assert_eq!(
                enforce_csrf(method, Some("https://evil.test"), "api.test", &allowed()),
                Err(DENIED),
                "{method}"
            );
        }
        // Without an Origin the origin check passes, so the answer is `Ok`.
        assert_eq!(enforce_csrf("get", None, "api.test", &allowed()), Ok(()));
    }

    #[test]
    fn a_missing_origin_is_a_non_browser_client() {
        assert!(is_allowed_origin(None, "api.test", &allowed()));
        assert_eq!(enforce_csrf("POST", None, "api.test", &allowed()), Ok(()));
    }

    #[test]
    fn compares_only_host_and_port_for_the_own_origin() {
        assert!(is_allowed_origin(
            Some("http://api.test"),
            "api.test",
            &allowed()
        ));
        assert!(is_allowed_origin(
            Some("https://API.TEST"),
            "api.test",
            &allowed()
        ));
        assert!(!is_allowed_origin(
            Some("http://api.test:8080"),
            "api.test",
            &allowed()
        ));
    }

    #[test]
    fn configured_origins_are_matched_after_normalisation() {
        assert!(is_allowed_origin(
            Some("https://WEB.plane.so/"),
            "api.test",
            &allowed()
        ));
        assert!(is_allowed_origin(
            Some("https://web.plane.so/a/b"),
            "api.test",
            &allowed()
        ));
        assert!(!is_allowed_origin(
            Some("https://evil.test"),
            "api.test",
            &allowed()
        ));
    }

    #[test]
    fn opaque_and_malformed_origins_are_denied() {
        assert!(!is_allowed_origin(Some("null"), "api.test", &allowed()));
        assert!(!is_allowed_origin(Some(""), "api.test", &allowed()));
        assert!(!is_allowed_origin(
            Some("http://[::1"),
            "api.test",
            &allowed()
        ));
        assert_eq!(normalize_origin("web.plane.so"), None);
        // `urlsplit("https://:8080")` has a netloc, so Django keeps it.
        assert_eq!(
            normalize_origin("https://:8080").as_deref(),
            Some("https://:8080")
        );
    }
}
