// Copyright (c) 2023-present Plane Software, Inc. and contributors
// SPDX-License-Identifier: AGPL-3.0-only
// See the LICENSE file for details.

//! Replay `contracts/golden/middleware-cases.json` over real HTTP.
//!
//! The golden file is produced by `contracts/tools/capture_middleware.py` from
//! Django 5.2.17 + django-cors-headers 4.9.0 + DRF 3.18.3. Nothing is mocked:
//! the binary is spawned on an ephemeral port and every case is a TCP request.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

const ORIGINS: [&str; 3] = [
    "https://web.plane.so",
    "https://admin.plane.so",
    "https://space.plane.so",
];

struct Server {
    child: Child,
    base: String,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../..")
        .canonicalize()
        .expect("repo root")
}

fn golden() -> Value {
    let path = repo_root().join("contracts/golden/middleware-cases.json");
    serde_json::from_slice(&std::fs::read(path).expect("middleware-cases.json"))
        .expect("valid golden json")
}

fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    listener.local_addr().expect("local addr").port()
}

fn start_server() -> Server {
    let port = free_port();
    let child = Command::new(env!("CARGO_BIN_EXE_plane-api"))
        .env("PLANE_API_BIND", format!("127.0.0.1:{port}"))
        .env("SECRET_KEY", "middleware-golden-test-key")
        .env("DEBUG", "0")
        .env("RUST_LOG", "error")
        .env("CORS_ALLOWED_ORIGINS", ORIGINS.join(","))
        .env("FILE_SIZE_LIMIT", "5242880")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn plane-api");

    let server = Server {
        child,
        base: format!("127.0.0.1:{port}"),
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if std::net::TcpStream::connect(&server.base).is_ok() {
            return server;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("plane-api did not start on {}", server.base);
}

struct Reply {
    status: u16,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

fn send(server: &Server, method: &str, path: &str, headers: &[(&str, &str)]) -> Reply {
    let mut stream = std::net::TcpStream::connect(&server.base).expect("connect");
    let mut head = format!(
        "{method} {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n",
        server.base
    );
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes()).expect("write");
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).expect("read");

    let split = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("header terminator");
    let head = String::from_utf8_lossy(&raw[..split]).to_string();
    let mut body = raw[split + 4..].to_vec();

    let mut status = 0;
    let mut headers = BTreeMap::new();
    for (index, line) in head.split("\r\n").enumerate() {
        if index == 0 {
            status = line
                .split_whitespace()
                .nth(1)
                .and_then(|code| code.parse().ok())
                .expect("status");
            continue;
        }
        if let Some((key, value)) = line.split_once(':') {
            headers.insert(key.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }
    if headers.get("transfer-encoding").map(String::as_str) == Some("chunked") {
        body = decode_chunked(&body);
    }
    Reply {
        status,
        headers,
        body,
    }
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

fn case<'a>(golden: &'a Value, name: &str) -> &'a Value {
    golden["cases"]
        .as_array()
        .expect("cases")
        .iter()
        .find(|entry| entry["name"] == name)
        .unwrap_or_else(|| panic!("case {name} missing"))
}

fn origin_headers<'a>(
    origin: Option<&'a str>,
    request_method: Option<&'a str>,
) -> Vec<(&'a str, &'a str, &'a str)> {
    match (origin, request_method) {
        (Some(origin), Some(method)) => vec![
            ("Origin", origin, ""),
            ("Access-Control-Request-Method", method, ""),
        ],
        (Some(origin), None) => vec![("Origin", origin, "")],
        _ => Vec::new(),
    }
}

#[test]
fn cors_preflight_and_simple_requests_match_django() {
    let golden = golden();
    let server = start_server();

    for name in [
        "preflight-web",
        "preflight-admin",
        "preflight-space",
        "preflight-evil",
        "simple-web",
        "simple-evil",
        "simple-no-origin",
        "simple-null-origin",
        "simple-upper-origin",
        "simple-trailing-slash",
        "simple-subpath-origin",
        "options-no-acrm",
        "head-web",
    ] {
        let expected = case(&golden, name);
        let method = expected["method"].as_str().expect("method");
        let path = expected["path"].as_str().expect("path");
        let origin = match name {
            "preflight-web" | "options-no-acrm" | "head-web" | "simple-web" => Some(ORIGINS[0]),
            "preflight-admin" => Some(ORIGINS[1]),
            "preflight-space" => Some(ORIGINS[2]),
            "preflight-evil" => Some("https://evil.example.com"),
            "simple-evil" => Some("https://evil.example.com"),
            "simple-null-origin" => Some("null"),
            "simple-upper-origin" => Some("https://WEB.plane.so"),
            "simple-trailing-slash" => Some("https://web.plane.so/"),
            "simple-subpath-origin" => Some("https://web.plane.so/some/path"),
            _ => None,
        };
        let request_method = if name.starts_with("preflight-") {
            Some("POST")
        } else {
            None
        };
        let headers: Vec<(&str, &str)> = origin_headers(origin, request_method)
            .into_iter()
            .map(|(name, value, _)| (name, value))
            .collect();

        let reply = send(&server, method, path, &headers);
        let expected_status =
            u16::try_from(expected["status"].as_u64().expect("status")).expect("status fits");
        assert_eq!(reply.status, expected_status, "{name}: status");

        let expected_headers = expected["headers"].as_object().expect("headers");
        for (key, value) in expected_headers {
            let value = value.as_str().expect("header value");
            assert_eq!(
                reply
                    .headers
                    .get(&key.to_ascii_lowercase())
                    .map(String::as_str),
                Some(value),
                "{name}: header {key}"
            );
        }
        if let Some(body) = expected["body_text"].as_str() {
            assert_eq!(String::from_utf8_lossy(&reply.body), body, "{name}: body");
        }
        if name.starts_with("preflight-") {
            assert!(
                !reply.headers.contains_key("x-frame-options"),
                "{name}: a preflight is answered before SecurityMiddleware"
            );
        }
    }
}

#[test]
fn security_headers_match_django() {
    let golden = golden();
    let server = start_server();

    let reply = send(&server, "GET", "/robots.txt", &[]);
    assert_eq!(reply.status, 200);
    assert_eq!(
        reply.headers.get("x-frame-options").map(String::as_str),
        Some("DENY")
    );
    assert_eq!(
        reply
            .headers
            .get("x-content-type-options")
            .map(String::as_str),
        Some("nosniff")
    );
    assert_eq!(
        reply.headers.get("referrer-policy").map(String::as_str),
        Some("same-origin")
    );
    assert_eq!(
        reply
            .headers
            .get("cross-origin-opener-policy")
            .map(String::as_str),
        Some("same-origin")
    );

    // A 404 goes through the same middleware chain in Django.
    let not_found = case(&golden, "not-found");
    let reply = send(
        &server,
        "GET",
        not_found["path"].as_str().expect("path"),
        &[("Origin", ORIGINS[0])],
    );
    assert_eq!(reply.status, 404);
    // Django's capture keeps the original header casing; compare case-insensitively.
    let expected: BTreeMap<String, String> = not_found["headers"]
        .as_object()
        .expect("headers")
        .iter()
        .map(|(key, value)| {
            (
                key.to_ascii_lowercase(),
                value.as_str().expect("value").to_string(),
            )
        })
        .collect();
    for key in [
        "x-frame-options",
        "x-content-type-options",
        "referrer-policy",
        "cross-origin-opener-policy",
        "vary",
        "access-control-allow-origin",
        "access-control-allow-credentials",
    ] {
        assert_eq!(
            reply.headers.get(key),
            expected.get(key),
            "404: header {key}"
        );
    }
}

#[test]
fn gzip_matches_djangos_decisions() {
    let golden = golden();
    let server = start_server();

    // Short body: no compression and, crucially, no `Vary: Accept-Encoding`.
    let reply = send(&server, "GET", "/", &[("Accept-Encoding", "gzip")]);
    assert_eq!(reply.status, 200);
    assert_eq!(reply.headers.get("content-encoding"), None);
    assert_eq!(
        reply.headers.get("vary").map(String::as_str),
        Some("origin")
    );

    // `gzip;q=0` still compresses: Python's regex is `\bgzip\b`.
    let q_zero = send(&server, "GET", "/", &[("Accept-Encoding", "gzip;q=0")]);
    assert_eq!(q_zero.status, 200);
    assert_eq!(
        q_zero.headers.get("vary").map(String::as_str),
        Some("origin"),
        "body is 16 bytes, so it stops before Vary is patched"
    );

    // `GZIP` and `xgzip` do not match.
    for header in ["GZIP", "xgzip"] {
        let reply = send(&server, "GET", "/", &[("Accept-Encoding", header)]);
        assert_eq!(reply.headers.get("content-encoding"), None, "{header}");
    }

    // The 413 body is 111 bytes: same early return.
    let large = case(&golden, "body-too-large");
    assert_eq!(large["status"], 413);
}

#[test]
fn body_limit_answers_the_django_413_bytes() {
    let golden = golden();
    let server = start_server();
    let expected = case(&golden, "body-too-large");

    // Django rejects from `CONTENT_LENGTH` alone (`_check_data_too_big`), before
    // reading a byte. Sending the declared length without the body proves the
    // middleware does the same instead of waiting for 5 MB.
    let mut stream = std::net::TcpStream::connect(&server.base).expect("connect");
    let head = format!(
        "POST /big/ HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nOrigin: {}\r\nContent-Type: application/octet-stream\r\nContent-Length: 5242881\r\n\r\n",
        server.base, ORIGINS[0],
    );
    stream.write_all(head.as_bytes()).expect("write head");
    let mut raw = Vec::new();
    let _ = stream.read_to_end(&mut raw);

    let split = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("header terminator");
    let head = String::from_utf8_lossy(&raw[..split]).to_string();
    let body = String::from_utf8_lossy(&raw[split + 4..]).to_string();
    assert!(head.starts_with("HTTP/1.1 413"), "{head}");
    assert_eq!(
        body,
        expected["body_text"].as_str().expect("body_text"),
        "413 body"
    );
}

#[test]
fn throttle_keys_and_window_match_drf() {
    let golden = golden();

    let api_key = case(&golden, "api-key-throttle");
    assert_eq!(
        plane_pkg::throttle::cache_key("api_key", "key-abc"),
        api_key["cache_key"].as_str().expect("cache key")
    );
    let observations = api_key["observations"].as_array().expect("observations");
    let rate = plane_pkg::throttle::parse_rate("2/minute").expect("rate");
    let store = plane_pkg::throttle::MemoryStore::new();
    let key = plane_pkg::throttle::cache_key("api_key", "key-abc");
    for (index, expected) in observations.iter().enumerate() {
        let decision = plane_pkg::throttle::allow(&store, rate, &key, 1_000.0 + index as f64);
        assert_eq!(
            decision.allowed,
            expected["allowed"].as_bool().expect("allowed")
        );
        if decision.allowed {
            assert_eq!(
                decision.remaining,
                expected["remaining"].as_u64(),
                "remaining on success"
            );
            assert!(expected["reset"].is_number());
        } else {
            assert!(expected["remaining"].is_null(), "Plane leaves it unset");
        }
    }

    let anon = case(&golden, "anon-throttle");
    assert_eq!(
        plane_pkg::throttle::cache_key("anon", "203.0.113.9"),
        anon["cache_key"].as_str().expect("cache key")
    );
}

#[test]
fn the_golden_file_is_self_consistent() {
    let golden = golden();
    assert_eq!(
        golden["settings"]["cors_allowed_origins"]
            .as_array()
            .expect("origins")
            .len(),
        3
    );
    assert_eq!(golden["settings"]["data_upload_max_memory_size"], 5_242_880);
    assert!(golden["cases"].as_array().expect("cases").len() >= 20);
}
