// Copyright (c) 2023-present Plane Software, Inc. and contributors
// SPDX-License-Identifier: AGPL-3.0-only
// See the LICENSE file for details.

//! Session door: which cookie is read, and the Origin check that replaces CSRF.
//!
//! Two Django classes make up this contract:
//!
//! * `plane.authentication.middleware.session.SessionMiddleware` chooses the
//!   cookie by **substring** — `if "instances" in request.path` — so
//!   `/api/v1/my-instances/x/` reads `admin-session-id` too. That looks like a
//!   bug, and it is measured as-is rather than "fixed":
//!   `contracts/golden/auth-cases.json`, `session.cookie_name`.
//! * `plane.authentication.session.BaseSessionAuthentication`, whose
//!   `enforce_csrf` is ported in `plane_pkg::origin`. The parts that belong to
//!   the session door specifically — the cookie name, the safe-method list and
//!   the exception Django raises — live here.

use crate::config::CookieConfig;

pub use crate::origin::{DENIED as CSRF_DENIED, SAFE_METHODS};

/// Django's `SAFE_METHODS` comparison is case-sensitive: `"get"` is not safe.
///
/// `str::eq` is used on purpose. `enforce_csrf` writes
/// `if request.method in SAFE_METHODS`, and Django's `WSGIRequest.method` is
/// upper-cased by the WSGI server, so a lowercase method only reaches the check
/// from a non-standard caller. The golden records both spellings.
#[must_use]
pub fn is_safe_method(method: &str) -> bool {
    SAFE_METHODS.contains(&method)
}

/// Which cookie `SessionMiddleware.process_request` reads for this path.
///
/// `settings.ADMIN_SESSION_COOKIE_NAME` when `"instances"` appears anywhere in
/// the path, `settings.SESSION_COOKIE_NAME` otherwise. The substring test is
/// Django's, not a prefix match.
#[must_use]
pub fn cookie_for_path<'a>(path: &str, cookies: &'a CookieConfig) -> &'a str {
    if path.contains("instances") {
        &cookies.admin_cookie_name
    } else {
        &cookies.session_cookie_name
    }
}

/// The cookie value Django would hand to `SessionStore`, if the header has it.
///
/// Cookie-name lookup is exact and case-sensitive: Django's `request.COOKIES`
/// is a plain dict built by `parse_cookie`, and `.get("session-id")` does not
/// match `Session-Id`. A missing cookie is `None`, which makes Django start an
/// empty session.
#[must_use]
pub fn session_key<'a>(
    path: &str,
    jar: &'a [(&'a str, String)],
    cookies: &CookieConfig,
) -> Option<&'a str> {
    let name = cookie_for_path(path, cookies);
    jar.iter()
        .find(|(key, _)| *key == name)
        .map(|(_, value)| value.as_str())
}

/// `BaseSessionAuthentication.authenticate` returns `(user, None)`, and skips
/// the CSRF check entirely when the session has no active user.
///
/// This is the guard that decides whether `enforce_csrf` runs at all:
/// `if not user or not user.is_active: return None`.
#[must_use]
pub fn requires_origin_check(authenticated: bool, user_active: bool) -> bool {
    authenticated && user_active
}

/// The status DRF finally renders for a rejected origin.
///
/// `PermissionDenied` is not coerced: only `NotAuthenticated` and
/// `AuthenticationFailed` are, so the answer is a real 403, not a 401.
pub const CSRF_STATUS: u16 = 403;

/// Full session-door decision for one request.
///
/// Returns `Ok(())` when the request may continue, or `Err((status, message))`
/// with the exact pair Django produces. `origin` is the `Origin` header,
/// `request_host` is `request.get_host()`, and `allowed` is
/// `allowed_origins()`.
///
/// # Errors
///
/// `Err((403, "CSRF Failed: origin not allowed"))` for an unsafe method from a
/// disallowed origin, which is `PermissionDenied` and therefore 403.
pub fn authorize(
    method: &str,
    authenticated: bool,
    user_active: bool,
    origin: Option<&str>,
    request_host: &str,
    allowed: &[String],
) -> Result<(), (u16, &'static str)> {
    if !requires_origin_check(authenticated, user_active) {
        // No session user: `BaseSessionAuthentication` returned `None` before
        // any Origin check, and `IsAuthenticated` answers the 401 instead.
        return Ok(());
    }
    crate::origin::enforce_csrf(method, origin, request_host, allowed)
        .map_err(|message| (CSRF_STATUS, message))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cookies() -> CookieConfig {
        CookieConfig {
            session_cookie_name: "session-id".to_string(),
            session_cookie_age: 604_800,
            save_every_request: false,
            admin_cookie_name: "admin-session-id".to_string(),
            admin_cookie_age: 3_600,
            domain: None,
            secure: false,
        }
    }

    #[test]
    fn the_safe_method_list_matches_django_and_is_case_sensitive() {
        for method in ["GET", "HEAD", "OPTIONS", "TRACE"] {
            assert!(is_safe_method(method), "{method}");
        }
        for method in ["get", "Get", "POST", "PATCH", "DELETE", "PUT"] {
            assert!(!is_safe_method(method), "{method}");
        }
        assert_eq!(SAFE_METHODS, ["GET", "HEAD", "OPTIONS", "TRACE"]);
    }

    #[test]
    fn the_admin_cookie_is_chosen_by_substring() {
        // Measured: `/api/v1/my-instances/x/` also reads the admin cookie.
        for path in [
            "/api/instances/",
            "/api/instancesfoo/",
            "/api/v1/my-instances/x/",
            "/api/v1/instances/x/",
        ] {
            assert_eq!(
                cookie_for_path(path, &cookies()),
                "admin-session-id",
                "{path}"
            );
        }
        for path in ["/api/v1/x/", "/api/users/me/", "/", "/auth/sign-in/"] {
            assert_eq!(cookie_for_path(path, &cookies()), "session-id", "{path}");
        }
    }

    #[test]
    fn the_cookie_lookup_is_exact_and_case_sensitive() {
        let jar = vec![
            ("Session-Id", "upper".to_string()),
            ("session-id", "abc123".to_string()),
        ];
        assert_eq!(session_key("/api/v1/x/", &jar, &cookies()), Some("abc123"));
        assert_eq!(
            session_key("/api/instances/x/", &jar, &cookies()),
            None,
            "no admin cookie in the jar"
        );
        let empty = vec![("session-id", String::new())];
        assert_eq!(session_key("/api/v1/x/", &empty, &cookies()), Some(""));
    }

    #[test]
    fn an_anonymous_session_never_reaches_the_origin_check() {
        // `if not user or not user.is_active: return None`
        assert!(!requires_origin_check(false, true));
        assert!(!requires_origin_check(true, false));
        assert!(!requires_origin_check(false, false));
        assert!(requires_origin_check(true, true));

        let allowed = vec!["https://web.plane.so".to_string()];
        assert_eq!(
            authorize(
                "POST",
                false,
                true,
                Some("https://evil.test"),
                "api.test",
                &allowed
            ),
            Ok(())
        );
        assert_eq!(
            authorize(
                "POST",
                true,
                true,
                Some("https://evil.test"),
                "api.test",
                &allowed
            ),
            Err((403, "CSRF Failed: origin not allowed"))
        );
        assert_eq!(
            authorize(
                "GET",
                true,
                true,
                Some("https://evil.test"),
                "api.test",
                &allowed
            ),
            Ok(()),
            "safe methods return before the origin is read"
        );
    }
}
