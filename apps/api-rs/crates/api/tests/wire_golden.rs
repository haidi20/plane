// Copyright (c) 2023-present Plane Software, Inc. and contributors
// SPDX-License-Identifier: AGPL-3.0-only
// See the LICENSE file for details.

//! Compare real HTTP responses with the frozen Django golden files.
//!
//! No mocks: the binary is built, started on an ephemeral port, and every case
//! under `contracts/golden/<name>/` is fetched over TCP. Bodies are compared
//! byte-for-byte. Headers are compared on the subset the capture kept, because
//! `Date`, `Server`, and hop-by-hop headers are never part of the contract.

use std::collections::BTreeMap;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

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

fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    listener.local_addr().expect("local addr").port()
}

fn start_server() -> Server {
    let port = free_port();
    let binary = env!("CARGO_BIN_EXE_plane-api");
    let child = Command::new(binary)
        .env("PLANE_API_BIND", format!("127.0.0.1:{port}"))
        .env("SECRET_KEY", "wire-golden-test-key")
        .env("DEBUG", "0")
        .env("RUST_LOG", "error")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn plane-api");

    // Wrap immediately: `Server::drop` kills and reaps the child on every exit
    // path, including the timeout below.
    let server = Server {
        child,
        base: format!("http://127.0.0.1:{port}"),
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return server;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("plane-api did not start on {}", server.base);
}

/// Minimal HTTP/1.1 GET. The client is `std` only so the harness never drags a
/// second HTTP stack (and its own header defaults) into a byte comparison.
fn get(base: &str, path: &str) -> (u16, BTreeMap<String, String>, Vec<u8>) {
    use std::io::{Read, Write};

    let host = base.strip_prefix("http://").expect("http base");
    let mut stream = std::net::TcpStream::connect(host).expect("connect");
    let request =
        format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nAccept: */*\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).expect("write request");

    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).expect("read response");

    let split = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("header terminator");
    let head = String::from_utf8_lossy(&raw[..split]).to_string();
    let mut body = raw[split + 4..].to_vec();

    // axum answers with Content-Length here; chunked handling stays out of the
    // harness on purpose so a mismatch surfaces instead of being decoded away.
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
        body = decode_chunked(&body);
    }
    (status, headers, body)
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

fn golden_dir(name: &str) -> PathBuf {
    repo_root().join("contracts/golden").join(name)
}

fn request_path(name: &str) -> String {
    let request =
        std::fs::read_to_string(golden_dir(name).join("request.http")).expect("request.http");
    request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .expect("request line")
        .to_string()
}

fn expected_status(name: &str) -> u16 {
    let response =
        std::fs::read_to_string(golden_dir(name).join("response.http")).expect("response.http");
    response
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .expect("status")
}

fn expected_headers(name: &str) -> BTreeMap<String, String> {
    let response =
        std::fs::read_to_string(golden_dir(name).join("response.http")).expect("response.http");
    let mut headers = BTreeMap::new();
    for line in response.lines().skip(1) {
        let Some((key, value)) = line.split_once(':') else {
            break;
        };
        headers.insert(key.trim().to_ascii_lowercase(), value.trim().to_string());
    }
    headers
}

/// Golden bodies are stored two ways on purpose:
///
/// * `body.json` - parsed and re-printed by the capture script, so it is
///   compared as a JSON value (key order and spacing are not contract);
/// * `body.raw` - byte-exact for HTML/plain text, compared verbatim.
fn assert_body(name: &str, actual: &[u8]) {
    let directory = golden_dir(name);
    let json_path = directory.join("body.json");
    if json_path.exists() {
        let expected: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&json_path).expect("body.json"))
                .expect("golden json");
        let actual: serde_json::Value = serde_json::from_slice(actual).expect("response json");
        assert_eq!(actual, expected, "{name}: json body");
        return;
    }

    let expected = std::fs::read(directory.join("body.raw")).expect("body.raw");
    assert_eq!(
        String::from_utf8_lossy(actual),
        String::from_utf8_lossy(&expected),
        "{name}: raw body"
    );
}

fn assert_case(server: &Server, name: &str) {
    let path = request_path(name);
    let (status, headers, body) = get(&server.base, &path);

    assert_eq!(status, expected_status(name), "{name}: status");
    let expected = expected_headers(name);
    for (key, value) in &expected {
        assert_eq!(
            headers.get(key).map(String::as_str),
            Some(value.as_str()),
            "{name}: header `{key}`"
        );
    }

    assert_body(name, &body);
}

#[test]
fn health_matches_django_golden() {
    let server = start_server();
    assert_case(&server, "django-root");
}

#[test]
fn robots_matches_django_golden() {
    let server = start_server();
    assert_case(&server, "django-robots");
}
