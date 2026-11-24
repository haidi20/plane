// Copyright (c) 2023-present Plane Software, Inc. and contributors
// SPDX-License-Identifier: AGPL-3.0-only
// See the LICENSE file for details.

//! Router assembly, in Django include order.

use axum::Router;
use axum::routing::any;

use crate::web;

/// Build the HTTP router.
///
/// Order matters: `api/v1/` and `api/public/` are registered before `api/`,
/// exactly like `plane/urls.py`, because a shared prefix must not shadow a
/// more specific door.
pub fn router() -> Router {
    Router::new()
        .route("/robots.txt", any(web::robots_txt))
        .route("/", any(web::health_check))
}
