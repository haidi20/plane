// Copyright (c) 2023-present Plane Software, Inc. and contributors
// SPDX-License-Identifier: AGPL-3.0-only
// See the LICENSE file for details.

//! Router assembly, in Django include order.

use axum::Router;
use axum::routing::any;

use crate::{errors, middleware, web, wire};

/// Build the HTTP router.
///
/// Order matters: `api/v1/` and `api/public/` are registered before `api/`,
/// exactly like `plane/urls.py`, because a shared prefix must not shadow a
/// more specific door. Unmatched paths fall through to Django\'s custom 404
/// (`plane.app.views.error_404`), which answers spaced JSON, not DRF JSON.
pub fn router(config: &plane_pkg::Config) -> Router {
    let routes = Router::new()
        .route("/robots.txt", any(web::robots_txt))
        .route("/", any(web::health_check))
        .fallback(errors::not_found);

    // Outermost layer, like Django's `MIDDLEWARE` entry: the line is written
    // after the handler ran, so `status_code` and `duration_ms` are real.
    let routes = routes.layer(axum::middleware::from_fn(middleware::log_request));

    // CORS, security, gzip and the body limit, in Django's middleware order.
    wire::install(routes, config)
}
