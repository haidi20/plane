// Copyright (c) 2023-present Plane Software, Inc. and contributors
// SPDX-License-Identifier: AGPL-3.0-only
// See the LICENSE file for details.

//! Request id and one access log line per request, like Django's
//! `RequestLoggerMiddleware`.
//!
//! Django has no request-id middleware. The id is only a value in the log line
//! explicitly allowed to differ between runs (`docs/rust-api.md` 8.7), and no
//! response header carries it, so this middleware adds nothing to the wire.
//!
//! The line is rendered by [`plane_pkg::logging::render_line`] rather than from
//! `tracing` fields, because `tracing` cannot express `null` and its field
//! order is declaration order. Plane's field order (`path`, `method`,
//! `status_code`, ...) is part of the log contract — an aggregator keys on it
//! and `api_activity_logs` stores the same names — so it is built once and
//! handed to the subscriber as a pre-rendered line.

use std::net::SocketAddr;
use std::time::Instant;

use axum::extract::{ConnectInfo, Request};
use axum::http::header;
use axum::middleware::Next;
use axum::response::Response;
use plane_pkg::logging::{JsonScalar, RequestLog, RequestParts, render_line, should_log_route};
use uuid::Uuid;

/// Header a proxy may use to hand down its own correlation id.
///
/// Plane does not read it today, so it is not part of the wire contract; it is
/// honoured here so a gateway can correlate without a code change. When absent
/// a v4 UUID is generated.
pub const REQUEST_ID_HEADER: &str = "x-request-id";

/// Log every request except `GET /`, carrying a request id.
pub async fn log_request(mut request: Request, next: Next) -> Response {
    let started = Instant::now();

    let method = request.method().as_str().to_string();
    let path = request.uri().path().to_string();
    let query = request.uri().query().map(str::to_string);
    let forwarded_for = header_value(&request, "x-forwarded-for");
    let user_agent = header_value(&request, header::USER_AGENT.as_str());
    let remote_addr = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|info| info.0.ip().to_string());
    let request_id = header_value(&request, REQUEST_ID_HEADER)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| Uuid::new_v4().to_string());

    request
        .extensions_mut()
        .insert(RequestId(request_id.clone()));

    let response = next.run(request).await;
    let duration = started.elapsed();
    let status = response.status().as_u16();

    if should_log_route(&path, &method) {
        let request_log = RequestLog::new(&RequestParts {
            method: &method,
            path: &path,
            query: query.as_deref(),
            status_code: status,
            duration,
            forwarded_for: forwarded_for.as_deref(),
            remote_addr: remote_addr.as_deref(),
            user_agent: user_agent.as_deref(),
            // No `Actor` yet: the auth extractor card owns it. Django writes
            // `null` for an anonymous request, which is this case.
            user_id: None,
        });
        let mut extra = request_log.extra_fields();
        // `python-json-logger` appends unknown `extra` keys in the order they
        // were attached; the middleware attaches the id after the seven above.
        extra.push(("request_id", JsonScalar::Str(request_id.clone())));
        let line = render_line(
            "INFO",
            chrono::Utc::now(),
            "middleware",
            "plane.api.request",
            &request_log.message,
            &extra,
        );
        // The target keeps `RUST_LOG=plane.api.request=debug` working and the
        // level matches `plane.api.request=INFO`; the subscriber writes the
        // pre-rendered line verbatim.
        tracing::info!(target: "plane.api.request", plane_line = %line, "request");
    }

    response
}

/// The correlation id attached to the request, for handlers and jobs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestId(pub String);

fn header_value(request: &Request, name: &str) -> Option<String> {
    request
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}
