// Copyright (c) 2023-present Plane Software, Inc. and contributors
// SPDX-License-Identifier: AGPL-3.0-only
// See the LICENSE file for details.

//! The access log, captured from a real server over TCP.
//!
//! No mocks: `plane-api` is started on an ephemeral port with `RUST_LOG` off,
//! requests are made, and the log lines the process wrote are parsed back. This
//! is the only way to prove the middleware runs after the handler (so
//! `status_code` and `duration_ms` are real) and that `GET /` is skipped while
//! `POST /` is logged — Django's `_should_log_route` exact match.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

struct Server {
    child: Child,
    base: String,
    lines: Arc<Mutex<Vec<String>>>,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    listener.local_addr().expect("local addr").port()
}

fn start_server() -> Server {
    let port = free_port();
    let mut child = Command::new(env!("CARGO_BIN_EXE_plane-api"))
        .env("PLANE_API_BIND", format!("127.0.0.1:{port}"))
        .env("SECRET_KEY", "logging-test-key")
        .env("DEBUG", "0")
        .env("RUST_LOG", "info")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn plane-api");

    // Django's console handler is a `StreamHandler`, which writes to stderr.
    let stderr = child.stderr.take().expect("stderr");
    let lines = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&lines);
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            sink.lock().expect("lock").push(line);
        }
    });

    let server = Server {
        child,
        base: format!("http://127.0.0.1:{port}"),
        lines,
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

/// Minimal HTTP/1.1 exchange; `std` only, so no client adds headers of its own.
fn request(base: &str, method: &str, path: &str, headers: &[(&str, &str)]) -> u16 {
    let host = base.strip_prefix("http://").expect("http base");
    let mut stream = std::net::TcpStream::connect(host).expect("connect");
    let mut head = format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n");
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes()).expect("write");
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).expect("read");
    String::from_utf8_lossy(&raw)
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .expect("status")
}

/// Wait until a line whose `path` matches arrives.
fn wait_for(lines: &Arc<Mutex<Vec<String>>>, path: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        for line in lines.lock().expect("lock").iter() {
            let Ok(value) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if value["name"] == "plane.api.request" && value["path"] == path {
                return value;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!(
        "no access-log line for {path}; saw {:?}",
        lines.lock().expect("lock")
    );
}

#[test]
fn access_log_matches_djangos_shape_and_skips_the_health_check() {
    let server = start_server();

    assert_eq!(
        request(&server.base, "GET", "/api/v1/workspaces/", &[]),
        404
    );
    let line = wait_for(&server.lines, "/api/v1/workspaces/");

    // Field names and order: the `fmt` fields first, then the seven `extra`.
    let keys: Vec<&str> = line
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        vec![
            "levelname",
            "asctime",
            "module",
            "name",
            "message",
            "path",
            "method",
            "status_code",
            "duration_ms",
            "remote_addr",
            "user_agent",
            "user_id",
            "request_id",
        ]
    );
    assert_eq!(line["levelname"], "INFO");
    assert_eq!(line["name"], "plane.api.request");
    assert_eq!(line["message"], "GET /api/v1/workspaces/ 404");
    assert_eq!(line["method"], "GET");
    assert_eq!(line["status_code"], 404);
    assert!(line["duration_ms"].is_u64());
    assert_eq!(line["user_agent"], "");
    assert!(line["user_id"].is_null());
    assert!(line["request_id"].is_string());

    // The proxy header is forwarded verbatim, first entry only.
    request(
        &server.base,
        "GET",
        "/api/v1/projects/",
        &[("X-Forwarded-For", "1.2.3.4, 5.6.7.8")],
    );
    let line = wait_for(&server.lines, "/api/v1/projects/");
    assert_eq!(line["remote_addr"], "1.2.3.4");

    // A caller-supplied id is kept, so a gateway can correlate.
    request(
        &server.base,
        "GET",
        "/api/v1/issues/",
        &[("X-Request-Id", "gw-123")],
    );
    let line = wait_for(&server.lines, "/api/v1/issues/");
    assert_eq!(line["request_id"], "gw-123");

    // Django logs the query string in `message` but not in `path`.
    request(&server.base, "GET", "/api/v1/cycles/?limit=10", &[]);
    let line = wait_for(&server.lines, "/api/v1/cycles/");
    assert_eq!(line["message"], "GET /api/v1/cycles/?limit=10 404");
}

#[test]
fn health_check_get_is_not_logged_but_post_is() {
    let server = start_server();

    assert_eq!(request(&server.base, "GET", "/", &[]), 200);
    // Give the (skipped) line a chance to appear before asserting it did not.
    std::thread::sleep(Duration::from_millis(300));
    let root_lines = server
        .lines
        .lock()
        .expect("lock")
        .iter()
        .filter(|line| line.contains("\"path\": \"/\""))
        .count();
    assert_eq!(root_lines, 0, "GET / must not be logged");

    assert_eq!(request(&server.base, "POST", "/", &[]), 200);
    let line = wait_for(&server.lines, "/");
    assert_eq!(line["message"], "POST / 200");
    assert_eq!(line["method"], "POST");
}
