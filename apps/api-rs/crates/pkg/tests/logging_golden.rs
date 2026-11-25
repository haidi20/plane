// Copyright (c) 2023-present Plane Software, Inc. and contributors
// SPDX-License-Identifier: AGPL-3.0-only
// See the LICENSE file for details.

//! Every logging case replayed from `contracts/golden/logging-cases.json`.
//!
//! The golden is recorded by `contracts/tools/capture_logging.py` from Plane's
//! own source (`plane/middleware/logger.py`, `plane/utils/ip_address.py`, the
//! `LOGGING` blocks in `plane/settings`) and from the real
//! `python-json-logger==4.0.0`, so these assertions compare against Django, not
//! against a re-typed expectation.

use std::path::{Path, PathBuf};

use chrono::{DateTime, TimeZone, Utc};
use plane_pkg::logging::{
    JsonScalar, MAX_LOGGED_BODY_LENGTH, PRODUCTION_LOGGERS, RequestLog, RequestParts,
    TRUNCATION_MARKER, client_ip, default_filter, production_level, render_line, safe_decode_body,
    should_log_route,
};
use serde_json::{Map, Value};

fn golden_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../contracts/golden/logging-cases.json")
}

fn golden() -> Value {
    let path = golden_path();
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    serde_json::from_str(&text).expect("golden json")
}

/// The `created` value in the golden as an instant, without losing the
/// milliseconds the formatter prints.
fn instant(millis: f64) -> DateTime<Utc> {
    let seconds = millis.trunc() as i64;
    let nanos = ((millis - millis.trunc()) * 1_000_000_000.0).round() as u32;
    Utc.timestamp_opt(seconds, nanos).single().expect("instant")
}

/// Map a recorded `extra` object back to the scalars [`render_line`] takes,
/// keeping the key order `python-json-logger` wrote.
fn scalars(extra: &Map<String, Value>) -> Vec<(&str, JsonScalar)> {
    extra
        .iter()
        .map(|(key, value)| {
            let scalar = match value {
                Value::Null => JsonScalar::Null,
                Value::Bool(flag) => JsonScalar::Bool(*flag),
                Value::Number(number) => match number.as_i64() {
                    Some(int) => JsonScalar::Int(int),
                    None => JsonScalar::Float(number.as_f64().expect("float")),
                },
                Value::String(text) => JsonScalar::Str(text.clone()),
                other => panic!("unsupported golden scalar {other:?}"),
            };
            (key.as_str(), scalar)
        })
        .collect()
}

#[test]
fn every_recorded_logger_level_matches() {
    let golden = golden();
    for profile in ["local", "production"] {
        let recorded = golden["loggers"][profile]
            .as_object()
            .unwrap_or_else(|| panic!("{profile} loggers"));
        assert_eq!(
            recorded.len(),
            PRODUCTION_LOGGERS.len(),
            "{profile}: logger count"
        );

        for (logger, levels) in recorded {
            for (mode, key) in [("debug", true), ("release", false)] {
                if profile == "local" {
                    // `local.py` hard-codes every level, so it does not depend
                    // on `DEBUG`; the `debug`/`release` columns are identical.
                    assert_eq!(
                        levels[mode].as_str().expect("level"),
                        levels["debug"].as_str().expect("level"),
                        "{logger}: local is DEBUG-independent"
                    );
                }
                let expected = levels[mode].as_str().expect("level");
                if profile == "production" {
                    assert_eq!(
                        production_level(logger, key),
                        expected,
                        "{profile}/{logger}/{mode}"
                    );
                }
            }
        }
    }

    // `local.py` lowers `plane.exception` to ERROR and raises nothing else, so
    // the Rust default stays correct for both modules only when the module is
    // production; the local module is the `debug`-independent case above.
    assert_eq!(production_level("plane.exception", false), "ERROR");
    assert_eq!(production_level("plane.exception", true), "DEBUG");
    assert_eq!(production_level("plane.external", false), "INFO");
    assert_eq!(production_level("plane.external", true), "INFO");
    assert_eq!(production_level("plane.api.request", false), "INFO");
    assert_eq!(production_level("plane.api.request", true), "DEBUG");

    let release = default_filter(false);
    assert!(release.starts_with("info,"), "{release}");
    assert!(release.contains("plane.exception=error"), "{release}");
    assert!(release.contains("plane.api.request=info"), "{release}");
    let debug = default_filter(true);
    assert!(debug.contains("plane.exception=debug"), "{debug}");
    assert!(debug.contains("plane.api.request=debug"), "{debug}");
}

#[test]
fn every_recorded_log_line_matches() {
    let golden = golden();
    let lines = golden["lines"].as_array().expect("lines");
    assert!(!lines.is_empty(), "golden has no lines");

    for case in lines {
        let name = case["name"].as_str().expect("name");
        let record = &case["record"];
        let expected = case["line"].as_str().expect("line");

        // `message` is the substituted `fmt` field; `extra` is everything the
        // middleware attached.
        let recorded: Value = serde_json::from_str(expected).expect("golden line json");
        let message = recorded["message"].as_str().expect("message");
        let extra = record["extra"].as_object().cloned().unwrap_or_default();

        let rendered = render_line(
            record["level"].as_str().expect("level"),
            instant(record["created"].as_f64().expect("created")),
            record["module"].as_str().expect("module"),
            record["logger"].as_str().expect("logger"),
            message,
            &scalars(&extra),
        );
        assert_eq!(rendered, expected, "{name}");
    }
}

#[test]
fn every_recorded_route_decision_matches() {
    for case in golden()["should_log_route"].as_array().expect("cases") {
        let path = case["path"].as_str().expect("path");
        let method = case["method"].as_str().expect("method");
        assert_eq!(
            should_log_route(path, method),
            case["log"].as_bool().expect("log"),
            "{method} {path}"
        );
    }
}

#[test]
fn every_recorded_client_ip_matches() {
    for case in golden()["client_ip"].as_array().expect("cases") {
        let forwarded = case["xff"].as_str();
        let remote = case["remote"].as_str();
        assert_eq!(
            client_ip(forwarded, remote).as_deref(),
            case["ip"].as_str(),
            "xff={forwarded:?} remote={remote:?}"
        );
    }
}

#[test]
fn every_recorded_body_decode_matches() {
    for case in golden()["safe_decode_body"].as_array().expect("cases") {
        let name = case["name"].as_str().expect("name");
        let payload = payload(name);

        let decoded = safe_decode_body(payload.as_deref());
        assert_eq!(decoded.as_deref(), case["decoded"].as_str(), "{name}");

        assert_eq!(
            payload.as_ref().map(Vec::len),
            case["length"].as_u64().map(|length| length as usize),
            "{name}: byte length"
        );
        assert_eq!(
            decoded.as_ref().map(|text| text.chars().count()),
            case["decoded_length"]
                .as_u64()
                .map(|length| length as usize),
            "{name}: decoded length"
        );
        assert_eq!(
            decoded
                .as_ref()
                .is_some_and(|text| text.ends_with(TRUNCATION_MARKER)),
            case["truncated"].as_bool().expect("truncated"),
            "{name}: truncated flag"
        );
    }
}

#[test]
fn request_log_matches_the_recorded_request_lines() {
    for case in golden()["lines"].as_array().expect("lines") {
        let record = &case["record"];
        let extra = &record["extra"];
        if record["logger"] != "plane.api.request" {
            continue;
        }
        let (Some(path), Some(method), Some(status)) = (
            extra["path"].as_str(),
            extra["method"].as_str(),
            extra["status_code"].as_u64(),
        ) else {
            continue;
        };

        let line: Value = serde_json::from_str(case["line"].as_str().expect("line")).expect("json");
        let request = RequestLog::new(&RequestParts {
            method,
            path,
            query: None,
            status_code: u16::try_from(status).expect("status fits u16"),
            duration: std::time::Duration::from_millis(extra["duration_ms"].as_u64().unwrap_or(0)),
            // The golden has no proxy header, so `remote_addr` is the peer.
            forwarded_for: None,
            remote_addr: extra["remote_addr"].as_str(),
            user_agent: extra["user_agent"].as_str(),
            user_id: extra["user_id"].as_i64(),
        });

        assert_eq!(request.message, line["message"].as_str().expect("message"));
        assert_eq!(request.path, path);
        assert_eq!(request.method, method);
        assert_eq!(u64::from(request.status_code), status);
        assert_eq!(
            request.user_agent,
            extra["user_agent"].as_str().unwrap_or_default()
        );
        assert_eq!(
            request.remote_addr.as_deref(),
            extra["remote_addr"].as_str()
        );
    }
}

/// Rebuild the exact bytes each golden body case was recorded from.
///
/// `capture_logging.py` stores the decoded text and its length rather than the
/// raw bytes, so the payloads are rebuilt from the same inputs. The byte-length
/// and decoded-length assertions above fail loudly if a rebuild drifts.
fn payload(name: &str) -> Option<Vec<u8>> {
    let limit = MAX_LOGGED_BODY_LENGTH;
    match name {
        "none" => None,
        "empty" => Some(Vec::new()),
        "json" => Some(br#"{"a": 1}"#.to_vec()),
        "png" => Some([b"\x89PNG\r\n\x1a\n".as_slice(), &[b'0'; 8]].concat()),
        "jpeg" => Some([b"\xff\xd8\xff\xe0".as_slice(), &[b'0'; 8]].concat()),
        "pdf" => Some(b"%PDF-1.4\n".to_vec()),
        "latin1" => Some(b"\xff\xfe\x00".to_vec()),
        "partial-magic" => Some(b"\x89P".to_vec()),
        "newline" => Some(b"line1\nline2".to_vec()),
        "at-limit" => Some(vec![b'x'; limit]),
        "over-limit" => Some(vec![b'y'; limit + 1]),
        "unicode-under-limit" => Some("\u{e9}".repeat(6000).into_bytes()),
        "unicode-over-limit" => Some(format!("{}z", "\u{e9}".repeat(6000)).into_bytes()),
        other => panic!("no Rust payload for golden case {other}"),
    }
}
