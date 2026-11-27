// Copyright (c) 2023-present Plane Software, Inc. and contributors
// SPDX-License-Identifier: AGPL-3.0-only
// See the LICENSE file for details.

//! Turning `ErrorBody` into an axum response.
//!
//! Django sets `Content-Type: application/json` without a charset for both
//! renderers; axum\'s `Json` would add one, so headers are set explicitly.

use axum::body::Body;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use plane_pkg::ErrorBody;

/// Convert an `ErrorBody` into a fully-formed response.
#[must_use]
pub fn render(error: ErrorBody) -> Response {
    let status = StatusCode::from_u16(error.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let mut response = (
        status,
        [(header::CONTENT_TYPE, "application/json")],
        Body::from(error.body),
    )
        .into_response();
    if let Some(allow) = error.allow {
        if let Ok(value) = allow.parse() {
            response.headers_mut().insert(header::ALLOW, value);
        }
    }
    response
}

/// Fallback for unmatched paths: Django\'s `handler404`.
pub async fn not_found() -> Response {
    render(ErrorBody::page_not_found())
}
