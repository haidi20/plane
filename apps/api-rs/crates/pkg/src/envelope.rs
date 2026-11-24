// Copyright (c) 2023-present Plane Software, Inc. and contributors
// SPDX-License-Identifier: AGPL-3.0-only
// See the LICENSE file for details.

//! Plane's error envelope, the one `BaseAPIView.handle_exception` produces.
//!
//! DRF maps its own exceptions; everything else escapes that layer and is caught
//! by Plane's override, which turns five Python exception families into a fixed
//! envelope. Both halves are visible on the wire and they differ:
//!
//! * framework errors keep DRF's shape (`{"detail": ...}`, field maps, lists);
//! * everything else becomes `{"error": "<fixed sentence>"}`.
//!
//! Measured with `contracts/tools/capture_errors.py` against the real
//! `plane/api/views/base.py`. Note that `BaseAPIView` and `BaseViewSet` say
//! different things for a missing object, so the caller picks the variant.

use crate::error::ErrorBody;

/// Why a request failed, in the shape Plane's handler renders.
///
/// This is a closed set on purpose: two call sites cannot drift into two
/// slightly different sentences for the same condition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiError {
    /// `ParseError` raised in `initial()` when the body is not an object/array.
    NonContainerBody,
    /// `IntegrityError` that reached the handler.
    InvalidPayload,
    /// Django `ValidationError` that reached the handler.
    InvalidDetail,
    /// `ObjectDoesNotExist` from a `BaseAPIView`.
    ResourceMissing,
    /// `ObjectDoesNotExist` from a `BaseViewSet`.
    ObjectMissing,
    /// `KeyError` such as a missing workspace slug.
    MissingKey,
    /// Anything else: logged, never described to the client.
    Unexpected,
}

impl ApiError {
    /// Render the envelope exactly as `plane/api/views/base.py` does.
    ///
    /// `status` values are measured, not assumed: an `APIException` built with a
    /// second positional argument gets that argument as its *code*, so it keeps
    /// the default 500 even though a 400 was intended.
    #[must_use]
    pub fn body(self) -> ErrorBody {
        match self {
            Self::NonContainerBody => {
                ErrorBody::detail(400, "Request body must be a JSON object or array.")
            }
            Self::InvalidPayload => ErrorBody::envelope(400, "The payload is not valid"),
            Self::InvalidDetail => ErrorBody::envelope(400, "Please provide valid detail"),
            Self::ResourceMissing => {
                ErrorBody::envelope(404, "The requested resource does not exist.")
            }
            Self::ObjectMissing => ErrorBody::envelope(404, "The required object does not exist."),
            Self::MissingKey => ErrorBody::envelope(400, "The required key does not exist."),
            Self::Unexpected => {
                ErrorBody::envelope(500, "Something went wrong please try again later")
            }
        }
    }

    /// The body DRF produces for an `APIException` that its own mapper handles:
    /// `{"detail": ...}` at status **500**, never Plane's `{"error": ...}`.
    ///
    /// `APIException("msg", 400)` keeps `status_code = 500` because the second
    /// positional argument is the error *code*, not the status. Recorded as-is
    /// so the port cannot "fix" the behaviour and break clients that rely on it.
    #[must_use]
    pub fn api_exception(detail: &str) -> ErrorBody {
        ErrorBody::detail(500, detail)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_uses_compact_drf_json_with_a_single_error_key() {
        assert_eq!(
            ApiError::InvalidPayload.body().body,
            r#"{"error":"The payload is not valid"}"#
        );
        assert_eq!(
            ApiError::InvalidDetail.body().body,
            r#"{"error":"Please provide valid detail"}"#
        );
        assert_eq!(
            ApiError::MissingKey.body().body,
            r#"{"error":"The required key does not exist."}"#
        );
        assert_eq!(
            ApiError::Unexpected.body().body,
            r#"{"error":"Something went wrong please try again later"}"#
        );
    }

    #[test]
    fn the_two_handlers_disagree_about_a_missing_object() {
        // Measured: BaseAPIView -> "resource", BaseViewSet -> "object".
        assert_eq!(
            ApiError::ResourceMissing.body().body,
            r#"{"error":"The requested resource does not exist."}"#
        );
        assert_eq!(
            ApiError::ObjectMissing.body().body,
            r#"{"error":"The required object does not exist."}"#
        );
        assert_ne!(
            ApiError::ResourceMissing.body().body,
            ApiError::ObjectMissing.body().body
        );
    }

    #[test]
    fn statuses_match_the_measured_table() {
        let cases = [
            (ApiError::NonContainerBody, 400),
            (ApiError::InvalidPayload, 400),
            (ApiError::InvalidDetail, 400),
            (ApiError::ResourceMissing, 404),
            (ApiError::ObjectMissing, 404),
            (ApiError::MissingKey, 400),
            (ApiError::Unexpected, 500),
        ];
        for (error, expected) in cases {
            assert_eq!(error.body().status, expected, "{error:?}");
        }
    }

    #[test]
    fn a_parse_error_keeps_drf_detail_shape() {
        let body = ApiError::NonContainerBody.body();
        assert_eq!(
            body.body,
            r#"{"detail":"Request body must be a JSON object or array."}"#
        );
        assert_eq!(body.status, 400);
    }

    #[test]
    fn api_exception_with_a_positional_code_still_returns_500() {
        // The second argument is `code`, so DRF leaves `status_code` alone.
        let body = ApiError::api_exception("Please check the view");
        assert_eq!(body.status, 500);
        assert_eq!(body.body, r#"{"detail":"Please check the view"}"#);
    }
}
