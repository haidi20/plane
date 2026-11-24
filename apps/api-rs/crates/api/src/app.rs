// Copyright (c) 2023-present Plane Software, Inc. and contributors
// SPDX-License-Identifier: AGPL-3.0-only
// See the LICENSE file for details.

//! Router assembly, in Django include order.

use axum::Router;
use axum::routing::any;

use crate::{errors, web};

/// Build the HTTP router.
///
/// Order matters: `api/v1/` and `api/public/` are registered before `api/`,
/// exactly like `plane/urls.py`, because a shared prefix must not shadow a
/// more specific door. Unmatched paths fall through to Django\'s custom 404
/// (`plane.app.views.error_404`), which answers spaced JSON, not DRF JSON.
pub fn router() -> Router {
    Router::new()
        .route("/robots.txt", any(web::robots_txt))
        .route("/", any(web::health_check))
        .fallback(errors::not_found)
}
