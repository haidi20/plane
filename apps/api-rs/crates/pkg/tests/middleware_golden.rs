// Copyright (c) 2023-present Plane Software, Inc. and contributors
// SPDX-License-Identifier: AGPL-3.0-only
// See the LICENSE file for details.

//! Replay `contracts/golden/middleware-cases.json` at the module level.
//!
//! The HTTP half lives in `crates/api/tests/middleware_golden.rs`; this file
//! covers the pure functions (origin check, throttle keys, CORS decisions) so a
//! regression is caught even when no server runs.

use std::path::Path;

use plane_pkg::cors::Headers;
use serde_json::Value;

fn golden() -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../../contracts/golden/middleware-cases.json");
    serde_json::from_slice(&std::fs::read(path).expect("middleware-cases.json"))
        .expect("valid golden json")
}

fn case<'a>(golden: &'a Value, name: &str) -> &'a Value {
    golden["cases"]
        .as_array()
        .expect("cases")
        .iter()
        .find(|entry| entry["name"] == name)
        .unwrap_or_else(|| panic!("case {name} missing"))
}

#[test]
fn origin_check_matches_django_for_every_recorded_case() {
    let golden = golden();
    let entry = case(&golden, "origin-check");
    let allowed = vec![
        "https://web.plane.so".to_string(),
        "https://admin.plane.so".to_string(),
    ];

    for observation in entry["observations"].as_array().expect("observations") {
        let method = observation["method"].as_str().expect("method");
        let origin = observation["origin"].as_str();
        let host = observation["host"].as_str().expect("host");
        let expected = observation["allowed"].as_bool().expect("allowed");

        // The golden calls `is_allowed_origin` itself; the safe-method guard
        // lives one level up in `enforce_csrf` and is tested in the unit suite.
        let actual = plane_pkg::origin::is_allowed_origin(origin, host, &allowed);
        let _ = method;
        assert_eq!(
            actual, expected,
            "origin check for {method} {origin:?} host {host}"
        );
    }
}

#[test]
fn origin_normalisation_matches_urlsplit() {
    let golden = golden();
    let entry = case(&golden, "origin-normalize");
    for observation in entry["observations"].as_array().expect("observations") {
        let raw = observation["raw"].as_str().expect("raw");
        let expected = observation["normalized"].as_str();
        assert_eq!(
            plane_pkg::origin::normalize_origin(raw).as_deref(),
            expected,
            "normalize {raw:?}"
        );
    }
}

#[test]
fn cors_headers_match_the_recorded_preflight_and_simple_cases() {
    let golden = golden();
    let settings = &golden["settings"];
    let origins: Vec<String> = settings["cors_allowed_origins"]
        .as_array()
        .expect("origins")
        .iter()
        .map(|value| value.as_str().expect("origin").to_string())
        .collect();
    let cors = plane_pkg::cors::Cors::new(&plane_pkg::config::CorsConfig {
        allowed_origins: origins,
        allow_all: settings["cors_allow_all"].as_bool().expect("allow_all"),
        allow_credentials: settings["cors_allow_credentials"]
            .as_bool()
            .expect("credentials"),
    });

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
    ] {
        let expected = case(&golden, name);
        let origin = expected["headers"]
            .as_object()
            .expect("headers")
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("access-control-allow-origin"))
            .and_then(|(_, value)| value.as_str());
        let request_origin = match name {
            "preflight-web" | "simple-web" => Some("https://web.plane.so"),
            "preflight-admin" => Some("https://admin.plane.so"),
            "preflight-space" => Some("https://space.plane.so"),
            "preflight-evil" | "simple-evil" => Some("https://evil.example.com"),
            "simple-null-origin" => Some("null"),
            "simple-upper-origin" => Some("https://WEB.plane.so"),
            "simple-trailing-slash" => Some("https://web.plane.so/"),
            "simple-subpath-origin" => Some("https://web.plane.so/some/path"),
            _ => None,
        };

        let method = if name.starts_with("preflight-") {
            "OPTIONS"
        } else {
            "GET"
        };
        let mut headers = TestHeaders::default();
        plane_pkg::cors::apply_response_headers(&cors, method, request_origin, &mut headers);

        assert_eq!(
            headers.get("access-control-allow-origin").as_deref(),
            origin,
            "{name}: allow-origin"
        );
        let expected_credentials = expected["headers"]
            .as_object()
            .expect("headers")
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("access-control-allow-credentials"))
            .and_then(|(_, value)| value.as_str());
        assert_eq!(
            headers.get("access-control-allow-credentials").as_deref(),
            expected_credentials,
            "{name}: allow-credentials"
        );
        let expected_vary = expected["headers"]
            .as_object()
            .expect("headers")
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("vary"))
            .and_then(|(_, value)| value.as_str());
        assert_eq!(
            headers.get("vary").as_deref(),
            expected_vary,
            "{name}: vary"
        );
    }
}

#[derive(Default)]
struct TestHeaders(std::collections::BTreeMap<String, String>);

impl plane_pkg::cors::Headers for TestHeaders {
    fn get(&self, name: &str) -> Option<String> {
        self.0.get(&name.to_ascii_lowercase()).cloned()
    }

    fn set(&mut self, name: &str, value: &str) {
        self.0.insert(name.to_ascii_lowercase(), value.to_string());
    }
}

#[test]
fn throttle_cases_match_drf() {
    let golden = golden();
    let api_key = case(&golden, "api-key-throttle");
    let rate =
        plane_pkg::throttle::parse_rate(api_key["rate"].as_str().expect("rate")).expect("rate");
    let key = plane_pkg::throttle::api_key_cache_key("api_key", "key-abc");
    assert_eq!(key, api_key["cache_key"].as_str().expect("cache key"));

    let store = plane_pkg::throttle::MemoryStore::new();
    for (index, expected) in api_key["observations"]
        .as_array()
        .expect("observations")
        .iter()
        .enumerate()
    {
        let decision =
            plane_pkg::throttle::allow(&store, rate, &key, 1_700_000_000.0 + index as f64);
        assert_eq!(
            decision.allowed,
            expected["allowed"].as_bool().expect("allowed"),
            "request {index}"
        );
        if decision.allowed {
            let remaining = expected["remaining"].as_u64().expect("remaining");
            assert_eq!(decision.remaining, Some(remaining));
            assert!(decision.reset.is_some());
        } else {
            assert!(expected["remaining"].is_null());
            assert!(decision.remaining.is_none());
        }
    }

    let anon = case(&golden, "anon-throttle");
    assert_eq!(
        plane_pkg::throttle::cache_key("anon", "203.0.113.9"),
        anon["cache_key"].as_str().expect("cache key")
    );
}

#[test]
fn security_header_values_match_django_defaults() {
    assert_eq!(plane_pkg::security::DEFAULT_HEADERS.len(), 4);
    assert_eq!(plane_pkg::security::CONTENT_TYPE_NOSNIFF.1, "nosniff");
    assert_eq!(plane_pkg::security::REFERRER_POLICY.1, "same-origin");
    assert_eq!(
        plane_pkg::security::CROSS_ORIGIN_OPENER_POLICY.1,
        "same-origin"
    );
    assert_eq!(plane_pkg::security::X_FRAME_OPTIONS.1, "DENY");
}
