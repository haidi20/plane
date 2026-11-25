// Copyright (c) 2023-present Plane Software, Inc. and contributors
// SPDX-License-Identifier: AGPL-3.0-only
// See the LICENSE file for details.

//! `plane/web/urls.py` equivalent: the health and robots door.
//!
//! Golden bytes live in `contracts/golden/django-root` and
//! `contracts/golden/django-robots`. Both are plain `HttpResponse`/
//! `JsonResponse`, not DRF, so the bodies are spaced.

use axum::body::Body;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};

/// `GET /` (and every other method; Django\'s function view ignores it).
pub async fn health_check() -> Response {
    match plane_pkg::JsonResponse::django(&serde_json::json!({ "status": "OK" })) {
        Ok(rendered) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, rendered.content_type)],
            Body::from(rendered.body),
        )
            .into_response(),
        Err(error) => internal_error(&error),
    }
}

/// `GET /robots.txt`.
pub async fn robots_txt() -> Response {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "text/plain")],
        Body::from("User-agent: *\nDisallow: /"),
    )
        .into_response()
}

/// Django answers 500 with an HTML page; until the error mapper lands we log and
/// return the same status without leaking internals.
fn internal_error(error: &plane_pkg::JsonError) -> Response {
    tracing::error!(%error, "failed to render JSON response");
    (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error").into_response()
}
