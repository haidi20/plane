// Copyright (c) 2023-present Plane Software, Inc. and contributors
// SPDX-License-Identifier: AGPL-3.0-only
// See the LICENSE file for details.

//! Error bodies, byte-compatible with Django and DRF.
//!
//! Two renderers exist and they must not be mixed:
//!
//! * DRF (`rest_framework.renderers.JSONRenderer`) writes **compact** JSON and
//!   owns every `/api/*`, `/api/v1/*`, `/api/public/*`, `/api/instances/*` and
//!   `/auth/*` answer.
//! * Django (`django.http.JsonResponse`) writes JSON with a space after `:` and
//!   `,` and owns the 404 handler and the 413 middleware.
//!
//! The exact bytes below were measured against Django 5.2.17 + DRF 3.18.3 by
//! running the real packages (no container needed).

use crate::json::spaced_json;
use serde_json::{Map, Value, json};

/// One error response: status, DRF-vs-Django shape, and the body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorBody {
    /// HTTP status code.
    pub status: u16,
    /// `true` when Django's spaced `JsonResponse` must render the body.
    pub django_style: bool,
    /// Already-encoded JSON.
    pub body: String,
    /// `Allow` header. DRF sets it on *every* response (not only 405), because
    /// `APIView.default_response_headers` always emits it. The value depends on
    /// the view, so callers attach it with [`ErrorBody::with_allow`].
    pub allow: Option<&'static str>,
}

impl ErrorBody {
    /// `404` from Django's `handler404` (`custom_404_view`).
    #[must_use]
    pub fn page_not_found() -> Self {
        Self::django(404, json!({"error": "Page not found."}))
    }

    /// `413` from `RequestBodySizeLimitMiddleware`.
    #[must_use]
    pub fn request_body_too_large() -> Self {
        Self::django(
            413,
            json!({
                "error": "REQUEST_BODY_TOO_LARGE",
                "detail": "The size of the request body exceeds the maximum allowed size.",
            }),
        )
    }

    /// Any DRF-owned success body. Kept next to the error constructors so the
    /// two renderers are chosen in one place: a 200 from a DRF view is compact,
    /// never spaced.
    #[must_use]
    pub fn ok(status: u16, body: Value) -> Self {
        Self::drf(status, body)
    }

    /// DRF `{"detail": "..."}` at any status. Used for framework errors and for
    /// the `ParseError` Plane raises in `initial()`.
    #[must_use]
    pub fn detail(status: u16, message: &str) -> Self {
        Self::drf(status, json!({ "detail": message }))
    }

    /// Plane's own envelope: `{"error": "<fixed sentence>"}`.
    ///
    /// `BaseAPIView.handle_exception` renders this for the Python exception
    /// families DRF does not handle. It is DRF JSON (compact), not `JsonResponse`.
    #[must_use]
    pub fn envelope(status: u16, message: &str) -> Self {
        Self::drf(status, json!({ "error": message }))
    }

    /// `401` for an anonymous request to an authenticated DRF view.
    #[must_use]
    pub fn not_authenticated() -> Self {
        Self::drf(
            401,
            json!({"detail": "Authentication credentials were not provided."}),
        )
    }

    /// `403` from DRF `PermissionDenied` without a custom message.
    #[must_use]
    pub fn permission_denied() -> Self {
        Self::drf(
            403,
            json!({"detail": "You do not have permission to perform this action."}),
        )
    }

    /// `404` from DRF (`Http404` / `get_object_or_404`).
    #[must_use]
    pub fn not_found() -> Self {
        Self::drf(404, json!({"detail": "Not found."}))
    }

    /// `405` from DRF. `Allow` is added by [`ErrorBody::with_allow`] because the
    /// same body is rendered for every view.
    #[must_use]
    pub fn method_not_allowed(method: &str) -> Self {
        Self::drf(
            405,
            json!({"detail": format!("Method \"{method}\" not allowed.")}),
        )
    }

    /// Attach the `Allow` header DRF would have emitted for the view.
    #[must_use]
    pub fn with_allow(mut self, allow: &'static str) -> Self {
        self.allow = Some(allow);
        self
    }

    /// `415` from DRF's content negotiation.
    #[must_use]
    pub fn unsupported_media_type(media_type: &str) -> Self {
        Self::drf(
            415,
            json!({"detail": format!("Unsupported media type \"{media_type}\" in request.")}),
        )
    }

    /// `429` rewritten by `auth_exception_handler` into the auth envelope.
    #[must_use]
    pub fn rate_limited() -> Self {
        Self::drf(
            429,
            json!({"error_code": 5900, "error_message": "RATE_LIMIT_EXCEEDED"}),
        )
    }

    /// `400` field errors: `{"field": ["message"]}`.
    #[must_use]
    pub fn field_errors(fields: &[(&str, &str)]) -> Self {
        let mut object = Map::new();
        for (field, message) in fields {
            object.insert(
                (*field).to_string(),
                Value::Array(vec![Value::String((*message).to_string())]),
            );
        }
        Self::drf(400, Value::Object(object))
    }

    /// `400` from a serializer's `validate()`: DRF nests it under
    /// `non_field_errors`.
    #[must_use]
    pub fn non_field_errors(messages: &[&str]) -> Self {
        Self::drf(400, json!({ "non_field_errors": messages }))
    }

    /// `400` raised by a view (not a serializer): DRF renders a bare list.
    #[must_use]
    pub fn raised_validation_error(message: &str) -> Self {
        Self::drf(400, json!([message]))
    }

    fn drf(status: u16, body: Value) -> Self {
        Self {
            status,
            django_style: false,
            body: serde_json::to_string(&body).unwrap_or_else(|_| "{}".to_string()),
            allow: None,
        }
    }

    #[allow(clippy::needless_pass_by_value)]
    fn django(status: u16, body: Value) -> Self {
        // Render through the shared encoder so a change to `spaced_json`
        // cannot silently desync the 404/413 bodies from Django.
        Self {
            status,
            django_style: true,
            body: spaced_json(&body).unwrap_or_else(|_| "{}".to_string()),
            allow: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drf_401_matches_measured_bytes() {
        assert_eq!(
            ErrorBody::not_authenticated().body,
            r#"{"detail":"Authentication credentials were not provided."}"#
        );
    }

    #[test]
    fn drf_403_matches_measured_bytes() {
        assert_eq!(
            ErrorBody::permission_denied().body,
            r#"{"detail":"You do not have permission to perform this action."}"#
        );
    }

    #[test]
    fn drf_404_matches_measured_bytes() {
        assert_eq!(ErrorBody::not_found().body, r#"{"detail":"Not found."}"#);
    }

    #[test]
    fn drf_405_matches_measured_bytes() {
        let error = ErrorBody::method_not_allowed("PUT").with_allow("GET, POST, HEAD, OPTIONS");
        assert_eq!(error.status, 405);
        assert_eq!(error.body, r#"{"detail":"Method \"PUT\" not allowed."}"#);
        assert_eq!(error.allow, Some("GET, POST, HEAD, OPTIONS"));
    }

    #[test]
    fn allow_is_opt_in_for_every_drf_error() {
        // DRF emits `Allow` on 401/403/404/415/429 too; only the value differs.
        for error in [
            ErrorBody::not_authenticated(),
            ErrorBody::permission_denied(),
            ErrorBody::not_found(),
            ErrorBody::unsupported_media_type("text/plain"),
            ErrorBody::rate_limited(),
        ] {
            assert_eq!(error.allow, None, "constructors stay view-agnostic");
            assert_eq!(
                error.with_allow("GET, HEAD, OPTIONS").allow,
                Some("GET, HEAD, OPTIONS")
            );
        }
    }

    #[test]
    fn drf_415_matches_measured_bytes() {
        assert_eq!(
            ErrorBody::unsupported_media_type("text/plain").body,
            r#"{"detail":"Unsupported media type \"text/plain\" in request."}"#
        );
    }

    #[test]
    fn drf_429_uses_auth_envelope() {
        let error = ErrorBody::rate_limited();
        assert_eq!(
            error.body,
            r#"{"error_code":5900,"error_message":"RATE_LIMIT_EXCEEDED"}"#
        );
    }

    #[test]
    fn drf_400_keeps_the_three_measured_shapes() {
        assert_eq!(
            ErrorBody::field_errors(&[("name", "This field is required.")]).body,
            r#"{"name":["This field is required."]}"#
        );
        assert_eq!(
            ErrorBody::non_field_errors(&["Something went wrong."]).body,
            r#"{"non_field_errors":["Something went wrong."]}"#
        );
        assert_eq!(
            ErrorBody::raised_validation_error("Something went wrong.").body,
            r#"["Something went wrong."]"#
        );
    }

    #[test]
    fn django_404_and_413_keep_spaces() {
        assert_eq!(
            ErrorBody::page_not_found().body,
            r#"{"error": "Page not found."}"#
        );
        let too_large = ErrorBody::request_body_too_large();
        assert_eq!(too_large.status, 413);
        assert!(
            too_large
                .body
                .starts_with(r#"{"error": "REQUEST_BODY_TOO_LARGE", "detail": "#)
        );
    }

    #[test]
    fn renderers_are_distinguishable() {
        assert!(ErrorBody::page_not_found().django_style);
        assert!(!ErrorBody::not_found().django_style);
    }
}
