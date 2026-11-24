// Copyright (c) 2023-present Plane Software, Inc. and contributors
// SPDX-License-Identifier: AGPL-3.0-only
// See the LICENSE file for details.

//! Every error body, checked byte-for-byte against the frozen Django golden.
//!
//! No mocks: a real axum server is started on an ephemeral port and the cases
//! are fetched over TCP. The bodies come from `plane_pkg::ErrorBody` through
//! `plane_api::errors::render`, which is the same path a handler takes, so a
//! stray charset, a dropped `Allow`, or a lost space fails here.
//!
//! The golden files in `contracts/golden/error-*` were recorded by
//! `contracts/tools/capture_errors.py` with Django 5.2.17 + DRF 3.18.3.
//!
//! Route mounting is test-local on purpose: production `ROUTES` only lists
//! routes that have handlers, and these error doors do not have one yet (the
//! auth and DRF endpoint cards own them). What is under test here is the
//! renderer, not the route surface.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use axum::Router;
use axum::routing::any;
use plane_pkg::{ApiError, ErrorBody};
use tokio::net::TcpListener;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../..")
        .canonicalize()
        .expect("repo root")
}

fn golden(name: &str) -> PathBuf {
    repo_root().join("contracts/golden").join(name)
}

fn expected_status(name: &str) -> u16 {
    let response = std::fs::read_to_string(golden(name).join("response.http")).expect("response");
    response
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .expect("status")
}

fn expected_headers(name: &str) -> BTreeMap<String, String> {
    let response = std::fs::read_to_string(golden(name).join("response.http")).expect("response");
    let mut headers = BTreeMap::new();
    for line in response.lines().skip(1) {
        let Some((key, value)) = line.split_once(':') else {
            break;
        };
        headers.insert(key.trim().to_ascii_lowercase(), value.trim().to_string());
    }
    headers
}

fn expected_body(name: &str) -> Vec<u8> {
    std::fs::read(golden(name).join("body.raw")).expect("body.raw")
}

/// Raw HTTP/1.1 request. `std` only, so no client library normalises anything
/// away before the comparison.
fn request(
    base: &str,
    method: &str,
    path: &str,
    content_type: Option<&str>,
    body: Option<&[u8]>,
) -> (u16, BTreeMap<String, String>, Vec<u8>) {
    let host = base.strip_prefix("http://").expect("http base");
    let mut stream = std::net::TcpStream::connect(host).expect("connect");
    let mut head =
        format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\nAccept: application/json\r\n");
    if let Some(value) = content_type {
        head.push_str(&format!("Content-Type: {value}\r\n"));
    }
    if let Some(payload) = body {
        head.push_str(&format!("Content-Length: {}\r\n", payload.len()));
    }
    head.push_str("Connection: close\r\n\r\n");
    stream.write_all(head.as_bytes()).expect("write head");
    if let Some(payload) = body {
        stream.write_all(payload).expect("write body");
    }

    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).expect("read response");
    let split = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("header terminator");
    let head = String::from_utf8_lossy(&raw[..split]).to_string();
    let mut payload = raw[split + 4..].to_vec();

    let mut status = 0_u16;
    let mut headers = BTreeMap::new();
    for (index, line) in head.split("\r\n").enumerate() {
        if index == 0 {
            status = line
                .split_whitespace()
                .nth(1)
                .and_then(|code| code.parse().ok())
                .expect("status line");
            continue;
        }
        if let Some((key, value)) = line.split_once(':') {
            headers.insert(key.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }
    if headers.get("transfer-encoding").map(String::as_str) == Some("chunked") {
        payload = decode_chunked(&payload);
    }
    (status, headers, payload)
}

fn decode_chunked(mut input: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let Some(line_end) = input.windows(2).position(|window| window == b"\r\n") else {
            break;
        };
        let size_text = String::from_utf8_lossy(&input[..line_end]).to_string();
        let size = usize::from_str_radix(size_text.trim(), 16).expect("chunk size");
        input = &input[line_end + 2..];
        if size == 0 {
            break;
        }
        out.extend_from_slice(&input[..size]);
        input = &input[size + 2..];
    }
    out
}

async fn start(app: Router) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{address}")
}

/// One case: golden name, mount path, method, and how to produce the body.
struct Case {
    name: &'static str,
    path: &'static str,
    method: &'static str,
    render: fn() -> ErrorBody,
}

const CASES: &[Case] = &[
    Case {
        name: "error-401",
        path: "/api/users/me/",
        method: "GET",
        render: || ErrorBody::not_authenticated().with_allow("GET, PATCH, DELETE, HEAD, OPTIONS"),
    },
    Case {
        name: "error-403",
        path: "/api/forbidden/",
        method: "GET",
        render: || ErrorBody::permission_denied().with_allow("GET, HEAD, OPTIONS"),
    },
    Case {
        name: "error-404-drf",
        path: "/api/missing/",
        method: "GET",
        render: || ErrorBody::not_found().with_allow("GET, HEAD, OPTIONS"),
    },
    Case {
        name: "error-405",
        path: "/api/public/",
        method: "DELETE",
        render: || ErrorBody::method_not_allowed("DELETE").with_allow("GET, HEAD, OPTIONS"),
    },
    Case {
        name: "error-400-field",
        path: "/api/body/",
        method: "POST",
        render: || {
            ErrorBody::field_errors(&[("name", "This field is required.")])
                .with_allow("POST, OPTIONS")
        },
    },
    Case {
        name: "error-400-non-field",
        path: "/api/non-field/",
        method: "POST",
        render: || {
            ErrorBody::non_field_errors(&["Something went wrong."]).with_allow("POST, OPTIONS")
        },
    },
    Case {
        name: "error-400-raised",
        path: "/api/raised/",
        method: "POST",
        render: || {
            ErrorBody::raised_validation_error("Something went wrong.").with_allow("POST, OPTIONS")
        },
    },
    Case {
        name: "error-415",
        path: "/api/unsupported/",
        method: "POST",
        render: || ErrorBody::unsupported_media_type("text/plain").with_allow("POST, OPTIONS"),
    },
    Case {
        name: "error-429",
        path: "/api/throttled/",
        method: "GET",
        render: || ErrorBody::rate_limited().with_allow("GET, HEAD, OPTIONS"),
    },
    Case {
        name: "unicode-drf",
        path: "/api/unicode/",
        method: "GET",
        render: || {
            ErrorBody::ok(
                200,
                serde_json::json!({
                    "name": "Ren\u{e9} \u{2014} \u{4f60}\u{597d}",
                    "emoji": "\u{1f600}",
                }),
            )
            .with_allow("GET, HEAD, OPTIONS")
        },
    },
    Case {
        name: "error-413",
        path: "/api/too-large/",
        method: "POST",
        render: ErrorBody::request_body_too_large,
    },
    // Plane's own `handle_exception` envelope. Same renderer, different producer:
    // these bodies come from `plane/api/views/base.py`, not from DRF's mapper.
    Case {
        name: "envelope-parse-error",
        path: "/api/envelope-parse-error/",
        method: "GET",
        render: || {
            ApiError::NonContainerBody
                .body()
                .with_allow("GET, HEAD, OPTIONS")
        },
    },
    Case {
        name: "envelope-integrity",
        path: "/api/envelope-integrity/",
        method: "GET",
        render: || {
            ApiError::InvalidPayload
                .body()
                .with_allow("GET, HEAD, OPTIONS")
        },
    },
    Case {
        name: "envelope-validation",
        path: "/api/envelope-validation/",
        method: "GET",
        render: || {
            ApiError::InvalidDetail
                .body()
                .with_allow("GET, HEAD, OPTIONS")
        },
    },
    Case {
        name: "envelope-object-missing",
        path: "/api/envelope-object-missing/",
        method: "GET",
        render: || {
            ApiError::ResourceMissing
                .body()
                .with_allow("GET, HEAD, OPTIONS")
        },
    },
    Case {
        name: "envelope-viewset-object-missing",
        path: "/api/envelope-viewset-object-missing/",
        method: "GET",
        render: || {
            ApiError::ObjectMissing
                .body()
                .with_allow("GET, HEAD, OPTIONS")
        },
    },
    Case {
        name: "envelope-key-error",
        path: "/api/envelope-key-error/",
        method: "GET",
        render: || ApiError::MissingKey.body().with_allow("GET, HEAD, OPTIONS"),
    },
    Case {
        name: "envelope-runtime",
        path: "/api/envelope-runtime/",
        method: "GET",
        render: || ApiError::Unexpected.body().with_allow("GET, HEAD, OPTIONS"),
    },
    Case {
        name: "envelope-api-exception",
        path: "/api/envelope-api-exception/",
        method: "GET",
        render: || {
            ApiError::api_exception("Please check the view").with_allow("GET, HEAD, OPTIONS")
        },
    },
];

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_error_body_matches_the_django_bytes() {
    let mut app = Router::new();
    for case in CASES {
        app = app.route(
            case.path,
            any(|| async { plane_api::errors::render((case.render)()) }),
        );
    }

    let base = start(app).await;
    for case in CASES {
        let (status, headers, body) = request(&base, case.method, case.path, None, None);
        assert_eq!(status, expected_status(case.name), "{}: status", case.name);

        for (key, value) in expected_headers(case.name) {
            assert_eq!(
                headers.get(&key),
                Some(&value),
                "{}: header `{key}`",
                case.name
            );
        }
        for key in ["content-type", "allow"] {
            if !expected_headers(case.name).contains_key(key) {
                assert!(
                    !headers.contains_key(key),
                    "{}: unexpected `{key}` header",
                    case.name
                );
            }
        }

        assert_eq!(body, expected_body(case.name), "{}: body bytes", case.name);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unmatched_path_falls_back_to_the_django_404() {
    let base = start(Router::new().fallback(plane_api::errors::not_found)).await;
    let (status, headers, body) = request(&base, "GET", "/api/does-not-exist/", None, None);

    assert_eq!(status, 404);
    assert_eq!(
        headers.get("content-type").map(String::as_str),
        Some("application/json")
    );
    assert!(!headers.contains_key("allow"));
    assert_eq!(body, br#"{"error": "Page not found."}"#);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn drf_bodies_stay_compact_and_django_bodies_keep_spaces() {
    let base = start(
        Router::new()
            .route(
                "/drf",
                any(|| async { plane_api::errors::render(ErrorBody::not_found()) }),
            )
            .route(
                "/django",
                any(|| async { plane_api::errors::render(ErrorBody::page_not_found()) }),
            ),
    )
    .await;

    let (_, _, drf) = request(&base, "GET", "/drf", None, None);
    let (_, _, django) = request(&base, "GET", "/django", None, None);

    assert!(
        !String::from_utf8_lossy(&drf).contains(": "),
        "DRF path must not gain a space: {}",
        String::from_utf8_lossy(&drf)
    );
    assert!(
        String::from_utf8_lossy(&django).contains("\": \""),
        "Django path must keep the space: {}",
        String::from_utf8_lossy(&django)
    );
}
