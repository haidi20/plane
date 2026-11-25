// Copyright (c) 2023-present Plane Software, Inc. and contributors
// SPDX-License-Identifier: AGPL-3.0-only
// See the LICENSE file for details.

//! The Django middleware chain, as axum layers.
//!
//! `Router::layer` wraps, so the calls run in reverse: the last `.layer()` is
//! the outermost. `MIDDLEWARE` in `plane/settings/common.py` lists outermost
//! first, which means the calls below are written bottom-up:
//!
//! ```text
//! CorsMiddleware             outermost (answers preflight itself)
//! SecurityMiddleware         |
//! XFrameOptionsMiddleware    |
//! GZipMiddleware             |
//! RequestBodySizeLimit       |
//! RequestLogger              innermost
//! ```
//!
//! The preflight short-circuit is deliberate: corsheaders answers an `OPTIONS`
//! with `Access-Control-Request-Method` before any inner middleware runs, so the
//! response has `Vary` and `Access-Control-*` but no `X-Frame-Options`. The
//! golden case `preflight-web` locks that.
//!
//! `SECURE_SSL_REDIRECT` is off in Plane (and this binary does not terminate
//! TLS), so there is no redirect branch to port.

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::Request;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use plane_pkg::cors::{self, Cors};
use plane_pkg::gzip;
use plane_pkg::security;
use plane_pkg::{Config, ErrorBody};

/// `DATA_UPLOAD_MAX_MEMORY_SIZE`, which Plane wires to `FILE_SIZE_LIMIT`.
#[derive(Debug, Clone, Copy)]
pub struct BodyLimit(pub usize);

/// One header view over `http::HeaderMap`, shared by both `Headers` traits.
struct MapHeaders<'a>(&'a mut HeaderMap);

impl MapHeaders<'_> {
    fn get(&self, name: &str) -> Option<String> {
        self.0
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    }

    fn set(&mut self, name: &str, value: &str) {
        if let (Ok(name), Ok(value)) = (
            header::HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(value),
        ) {
            self.0.insert(name, value);
        }
    }
}

impl cors::Headers for MapHeaders<'_> {
    fn get(&self, name: &str) -> Option<String> {
        MapHeaders::get(self, name)
    }

    fn set(&mut self, name: &str, value: &str) {
        MapHeaders::set(self, name, value);
    }
}

impl security::Headers for MapHeaders<'_> {
    fn get(&self, name: &str) -> Option<String> {
        MapHeaders::get(self, name)
    }

    fn set(&mut self, name: &str, value: &str) {
        MapHeaders::set(self, name, value);
    }
}

/// Attach every wire middleware for the loaded config.
pub fn install(router: Router, config: &Config) -> Router {
    let cors = Cors::new(&config.cors);
    let limit = usize::try_from(config.limits.data_upload_max_memory_size)
        .ok()
        .filter(|value| *value > 0)
        .unwrap_or(usize::MAX);

    // `layer` wraps, so the **last** call is the outermost. Django's list is
    // outermost-first, hence: body limit innermost, then gzip, then security,
    // then CORS. `log_request` was applied before this function ran.
    router
        .layer(axum::middleware::from_fn_with_state(
            BodyLimit(limit),
            body_limit,
        ))
        .layer(axum::middleware::from_fn(gzip_response))
        .layer(axum::middleware::from_fn(security_headers))
        .layer(axum::middleware::from_fn_with_state(cors, cors_response))
}

/// `CorsMiddleware`: answer a preflight here, otherwise patch the response.
async fn cors_response(
    axum::extract::State(cors): axum::extract::State<Cors>,
    request: Request,
    next: Next,
) -> Response {
    let method = request.method().as_str().to_string();
    let origin = request
        .headers()
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let is_preflight = cors.is_preflight(
        &method,
        request.headers().contains_key(cors::header::REQUEST_METHOD),
    );

    let mut response = if is_preflight {
        // `HttpResponse(headers={"content-length": "0"})` keeps Django's
        // default `Content-Type: text/html; charset=utf-8`.
        let mut response = Response::new(Body::empty());
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/html; charset=utf-8"),
        );
        response
            .headers_mut()
            .insert(header::CONTENT_LENGTH, HeaderValue::from_static("0"));
        response
    } else {
        next.run(request).await
    };

    cors::apply_response_headers(
        &cors,
        &method,
        origin.as_deref(),
        &mut MapHeaders(response.headers_mut()),
    );
    response
}

/// `SecurityMiddleware` + `XFrameOptionsMiddleware`, applied after the handler.
async fn security_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    security::apply(&mut MapHeaders(response.headers_mut()));
    response
}

/// `GZipMiddleware.process_response`.
async fn gzip_response(request: Request, next: Next) -> Response {
    let accept_encoding = request
        .headers()
        .get(header::ACCEPT_ENCODING)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let response = next.run(request).await;

    let has_content_encoding = response.headers().contains_key(header::CONTENT_ENCODING);
    let (mut parts, body) = response.into_parts();
    let Ok(bytes) = to_bytes(body, usize::MAX).await else {
        return Response::from_parts(parts, Body::empty());
    };

    if !gzip::should_consider(bytes.len(), has_content_encoding) {
        return Response::from_parts(parts, Body::from(bytes));
    }

    // `patch_vary_headers(response, ("Accept-Encoding",))` runs before the
    // `Accept-Encoding` check, so a rejected compression still adds `Vary`.
    let current = parts
        .headers
        .get(header::VARY)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let vary = cors::patch_vary(current.as_deref(), "Accept-Encoding");
    if let Ok(value) = HeaderValue::from_str(&vary) {
        parts.headers.insert(header::VARY, value);
    }

    if !gzip::accepts_gzip(&accept_encoding) {
        return Response::from_parts(parts, Body::from(bytes));
    }
    let Some(compressed) = gzip::compress_if_shorter(&bytes) else {
        return Response::from_parts(parts, Body::from(bytes));
    };

    parts.headers.insert(
        header::CONTENT_ENCODING,
        HeaderValue::from_static(gzip::CONTENT_ENCODING),
    );
    if let Ok(value) = HeaderValue::from_str(&compressed.len().to_string()) {
        parts.headers.insert(header::CONTENT_LENGTH, value);
    }
    if let Some(etag) = parts
        .headers
        .get(header::ETAG)
        .and_then(|value| value.to_str().ok())
    {
        if let Ok(value) = HeaderValue::from_str(&gzip::weaken_etag(etag)) {
            parts.headers.insert(header::ETAG, value);
        }
    }
    Response::from_parts(parts, Body::from(compressed))
}

/// `RequestBodySizeLimitMiddleware`.
///
/// Django raises while reading `request.body`, before the view. Here the limit
/// is enforced before the handler and the body is buffered, so a handler always
/// sees the same bytes and a slow client cannot hold the slot. The one-byte
/// overshoot matches Django's `read(limit + 1)`.
async fn body_limit(
    axum::extract::State(BodyLimit(limit)): axum::extract::State<BodyLimit>,
    request: Request,
    next: Next,
) -> Response {
    if let Some(length) = request
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
    {
        if length > limit as u64 {
            return too_large();
        }
    }

    let (parts, body) = request.into_parts();
    match to_bytes(body, limit.saturating_add(1)).await {
        Ok(bytes) if bytes.len() > limit => too_large(),
        Ok(bytes) => {
            next.run(Request::from_parts(parts, Body::from(bytes)))
                .await
        }
        Err(_) => too_large(),
    }
}

fn too_large() -> Response {
    let body = ErrorBody::request_body_too_large();
    (
        StatusCode::from_u16(body.status).unwrap_or(StatusCode::PAYLOAD_TOO_LARGE),
        [(header::CONTENT_TYPE, "application/json")],
        Body::from(body.body),
    )
        .into_response()
}
