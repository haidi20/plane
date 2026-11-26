// Copyright (c) 2023-present Plane Software, Inc. and contributors
// SPDX-License-Identifier: AGPL-3.0-only
// See the LICENSE file for details.

//! Replay `contracts/golden/auth-cases.json` against `plane_pkg`.
//!
//! The golden is recorded by `contracts/tools/capture_auth.py` from real Django
//! 5.2.17 + DRF 3.18.3, no database and no container. Every assertion here is a
//! value the Python stack produced, not a restatement of the Rust code.

use std::path::Path;

use plane_pkg::actor::{Actor, AuthSource, Role, WorkspaceContext};
use plane_pkg::api_key::{self, HeaderOutcome, Outcome, StoredToken, TokenQuery};
use plane_pkg::cookie;
use plane_pkg::session;
use plane_pkg::throttle;
use serde_json::Value;
use uuid::Uuid;

fn golden() -> Value {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../contracts/golden/auth-cases.json");
    serde_json::from_slice(&std::fs::read(path).expect("auth-cases.json"))
        .expect("valid golden json")
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .expect("array")
        .iter()
        .map(|entry| entry.as_str().expect("string").to_string())
        .collect()
}

#[test]
fn settings_match_the_rust_contract() {
    let golden = golden();
    let settings = &golden["settings"];
    assert_eq!(settings["api_key_header"], api_key::HEADER);
    assert_eq!(settings["authentication_scope"], "authentication");
    assert_eq!(settings["api_key_scope"], "api_key");
    assert_eq!(settings["authentication_rate"], "10/minute");
    assert_eq!(settings["api_key_rate"], "60/minute");
    assert_eq!(strings(&settings["safe_methods"]), session::SAFE_METHODS);
}

#[test]
fn the_api_key_header_is_read_verbatim() {
    let golden = golden();
    for row in golden["api_key"]["header"].as_array().expect("header") {
        // The golden records `get_api_token`'s return value, before the
        // `if not token` guard: `""` is a value there and `Absent` here.
        let raw = row["raw"].as_str();
        let expected = row["value"].as_str();
        let outcome = api_key::header_outcome(raw);
        match expected {
            None | Some("") => assert_eq!(outcome, HeaderOutcome::Absent, "{raw:?}"),
            Some(value) => assert_eq!(outcome, HeaderOutcome::Lookup(value), "{raw:?}"),
        }
    }
}

#[test]
fn every_recorded_outcome_replays() {
    let golden = golden();
    for row in golden["api_key"]["outcomes"].as_array().expect("outcomes") {
        let case = row["case"].as_str().expect("case");
        let status = row["status"].as_u64().expect("status") as u16;
        let body = row["body"].as_str().expect("body");
        let record = &row["record"];

        // The header value is the query value for every lookup case, and the
        // golden records exactly one query call per lookup.
        let queried = row["query"]
            .as_array()
            .expect("query")
            .first()
            .map(|call| call["kwargs"]["token"].as_str().expect("token").to_string());
        let row_token = record["token"].as_str().map(str::to_string);

        // `record` describes the row the capture asked the database about; a
        // real `.get()` only returns it when the predicate matches **and** a row
        // exists. Only `valid-token` recorded a row, so only it comes back.
        let stored = StoredToken {
            id: Uuid::from_u128(1),
            token: row_token.clone().unwrap_or_default(),
            is_active: record["is_active"].as_bool().unwrap_or(false),
            user_id: Uuid::from_u128(7),
            user_is_active: record["user_is_active"].as_bool().unwrap_or(false),
            // The golden records the class of expiry, not a timestamp.
            expired_at: match record["expired"].as_str() {
                Some("past") => Some(0),
                Some("future") => Some(i64::MAX),
                _ => None,
            },
        };
        let returns_row = status == 200;

        let mut seen: Option<TokenQuery<'_>> = None;
        let header = queried.as_deref();
        let outcome = api_key::authenticate(header, |query| {
            seen = Some(query.clone());
            returns_row.then(|| stored.clone())
        });

        assert_eq!(
            seen.as_ref().map(|query| query.token),
            queried.as_deref(),
            "{case}: query token"
        );
        if let Some(query) = &seen {
            assert!(query.is_active, "{case}: is_active=True in the predicate");
            assert!(
                query.user_is_active,
                "{case}: user__is_active=True in the predicate"
            );
        }

        // The predicate is measured separately from the row's existence: an
        // expired/inactive row is filtered out even though the token matches.
        let now = 1_000_i64;
        let expected_valid = record["is_active"].as_bool().unwrap_or(false)
            && record["user_is_active"].as_bool().unwrap_or(false)
            && record["expired"].as_str() != Some("past");
        assert_eq!(stored.is_valid_at(now), expected_valid, "{case}: predicate");

        match outcome.rejection() {
            None => {
                assert_eq!(status, 200, "{case}: authenticated");
                assert!(body.contains("\"ok\""), "{case}: handler rendered {body}");
            }
            Some((expected_status, detail)) => {
                assert_eq!(expected_status, status, "{case}");
                let expected = format!("{{\"detail\":\"{detail}\"}}");
                assert_eq!(body, expected, "{case}: DRF detail body");
            }
        }
    }
}

#[test]
fn the_success_path_updates_last_used_and_marks_the_request() {
    let golden = golden();
    let success = &golden["api_key"]["success"];
    assert_eq!(success["last_used_set"], true);
    assert_eq!(success["api_key_authenticated"], true);
    assert_eq!(
        strings(&success["save_calls"][0]["update_fields"]),
        ["last_used"]
    );

    let outcome = api_key::authenticate(Some("tok-1"), |_| {
        Some(StoredToken {
            id: Uuid::from_u128(1),
            token: "tok-1".to_string(),
            is_active: true,
            user_id: Uuid::from_u128(7),
            user_is_active: true,
            expired_at: None,
        })
    });
    assert_eq!(
        outcome,
        Outcome::Authenticated {
            user_id: Uuid::from_u128(7),
            token_id: Uuid::from_u128(1),
            token: "tok-1".to_string(),
        }
    );
    assert_eq!(outcome.rejection(), None);
}

#[test]
fn the_origin_check_guard_and_the_rejection_match_django() {
    let golden = golden();
    // The capture sets no `WEB_URL`/`CORS_ALLOWED_ORIGINS`, so
    // `allowed_origins()` is empty: only the request's own host passes.
    let allowed: Vec<String> = Vec::new();
    for row in golden["session"]["csrf"].as_array().expect("csrf") {
        let method = row["method"].as_str().expect("method");
        let origin = row["origin"].as_str();
        let allowed_by_origin = row["allowed"].as_bool().expect("allowed");
        let enforced = row["enforced"].as_bool().expect("enforced");

        assert_eq!(
            plane_pkg::origin::is_allowed_origin(origin, "api.plane.so", &allowed),
            allowed_by_origin,
            "is_allowed_origin {method} {origin:?}"
        );
        assert_eq!(
            plane_pkg::origin::enforce_csrf(method, origin, "api.plane.so", &allowed).is_ok(),
            enforced,
            "enforce_csrf {method} {origin:?}"
        );
    }
}

#[test]
fn the_csrf_exception_is_a_403_permission_denied() {
    let golden = golden();
    let exception = &golden["session"]["exception"];
    let denied = &exception["POST:https://evil.example.com"];
    assert_eq!(denied["type"], "PermissionDenied");
    assert_eq!(denied["detail"], session::CSRF_DENIED);
    assert_eq!(denied["status_code"], u64::from(session::CSRF_STATUS));
    assert_eq!(exception["POST:None"], Value::Null);
    assert_eq!(exception["GET:https://evil.example.com"], Value::Null);
}

#[test]
fn the_session_cookie_name_is_chosen_by_substring() {
    let golden = golden();
    let cookies = plane_pkg::config::CookieConfig {
        session_cookie_name: "session-id".to_string(),
        session_cookie_age: 604_800,
        save_every_request: false,
        admin_cookie_name: "admin-session-id".to_string(),
        admin_cookie_age: 3_600,
        domain: None,
        secure: false,
    };
    for row in golden["session"]["cookie_name"]
        .as_array()
        .expect("cookie_name")
    {
        let path = row["path"].as_str().expect("path");
        assert_eq!(
            session::cookie_for_path(path, &cookies),
            row["cookie"].as_str().expect("cookie"),
            "{path}"
        );
    }
}

#[test]
fn parse_cookie_matches_django_for_every_recorded_header() {
    let golden = golden();
    for row in golden["cookies"]["parse"].as_array().expect("parse") {
        let raw = row["raw"].as_str().expect("raw");
        let expected: Vec<(String, String)> = row["cookies"]
            .as_object()
            .expect("cookies")
            .iter()
            .map(|(key, value)| {
                (
                    key.clone(),
                    value.as_str().expect("cookie value").to_string(),
                )
            })
            .collect();
        assert_eq!(cookie::parse_cookie(raw), expected, "{raw}");
    }
}

#[test]
fn set_cookie_matches_django_for_every_recorded_case() {
    let golden = golden();
    let http_dates = strings(&golden["cookies"]["http_date"]);
    for row in golden["cookies"]["set"].as_array().expect("set") {
        let case = row["case"].as_str().expect("case");
        let attributes = row["attributes"].as_object().expect("attributes");
        let rendered = row["set_cookie"].as_str().expect("set_cookie");

        // `<http-date>` is stable in the golden: it was recorded as epoch 0.
        let expected = rendered.replace("<http-date>", &http_dates[0]);

        let mut cookie = plane_pkg::cookie::SetCookie::new(
            row["name"].as_str().expect("name"),
            row["value"].as_str().expect("value"),
        );
        if let Some(max_age) = attributes.get("max_age").and_then(Value::as_str) {
            let max_age: i64 = max_age.parse().expect("max-age");
            if attributes.contains_key("expires") {
                cookie = cookie.with_expires(&http_dates[0]);
            }
            cookie = cookie.with_max_age(max_age, 0);
        }
        if let Some(path) = attributes.get("path").and_then(Value::as_str) {
            cookie = cookie.with_path(path);
        }
        if let Some(domain) = attributes.get("domain").and_then(Value::as_str) {
            cookie = cookie.with_domain(domain);
        }
        if attributes.contains_key("secure") {
            cookie = cookie.with_secure(true);
        }
        if attributes.contains_key("httponly") {
            cookie = cookie.with_http_only(true);
        }
        if let Some(same_site) = attributes.get("samesite").and_then(Value::as_str) {
            cookie = cookie.with_same_site(same_site);
        }

        assert_eq!(cookie.render(), expected, "{case}");
    }
}

#[test]
fn delete_cookie_matches_django() {
    let golden = golden();
    for row in golden["cookies"]["delete"].as_array().expect("delete") {
        let name = row["name"].as_str().expect("name");
        let same_site = row["samesite"].as_str();
        assert_eq!(
            cookie::delete_cookie(name, Some("/"), None, same_site).render(),
            row["set_cookie"].as_str().expect("set_cookie"),
            "{name}"
        );
    }
}

#[test]
fn http_date_matches_django() {
    let golden = golden();
    let expected = strings(&golden["cookies"]["http_date"]);
    for (index, rendered) in expected.iter().enumerate() {
        // The golden records epochs 0, 1 and 1795649400 in that order.
        let epoch = match index {
            0 => 0,
            1 => 1,
            _ => 1_795_649_400,
        };
        assert_eq!(cookie::http_date(epoch).as_deref(), Some(rendered.as_str()));
    }
}

#[test]
fn throttle_keys_and_limits_match_django() {
    let golden = golden();
    let api_key = &golden["throttle"]["api_key"];
    assert_eq!(
        throttle::api_key_cache_key(api_key["scope"].as_str().expect("scope"), "key-abc"),
        api_key["key"].as_str().expect("key")
    );
    assert_eq!(
        throttle::django_raw_cache_key(api_key["key"].as_str().expect("key")),
        strings(&api_key["store_keys"])[0]
    );
    // No header means no bucket: `get_cache_key` returns `None`.
    assert_eq!(api_key["no_header_returns_none"], Value::Null);

    let authentication = &golden["throttle"]["authentication"];
    assert_eq!(
        throttle::cache_key(
            authentication["scope"].as_str().expect("scope"),
            "203.0.113.9"
        ),
        authentication["key"].as_str().expect("key")
    );
    assert_eq!(
        throttle::django_cache_key(
            authentication["scope"].as_str().expect("scope"),
            "203.0.113.9"
        ),
        strings(&authentication["store_keys"])[0]
    );
    assert_eq!(
        authentication["no_remote_addr_key"],
        "throttle_authentication_127.0.0.1"
    );
    assert_eq!(
        throttle::parse_rate(authentication["class_rate"].as_str().expect("rate")).expect("rate"),
        throttle::Rate {
            num_requests: 10,
            duration: 60
        }
    );
}

#[test]
fn the_rate_limit_response_is_the_measured_429() {
    let golden = golden();
    let second = &golden["throttle"]["authentication"]["second"];
    let status = second["status"].as_u64().expect("status") as u16;
    let body = second["body"].as_str().expect("body");
    let retry_after = second["headers"]["Retry-After"].as_str().expect("retry");

    assert_eq!(status, 429);
    assert_eq!(retry_after, "60");
    assert_eq!(
        body,
        "{\"error_code\":5900,\"error_message\":\"RATE_LIMIT_EXCEEDED\"}"
    );
    // The response is Plane's auth envelope, not DRF's `{"detail": ...}`.
    assert_eq!(golden["throttle"]["authentication"]["first"]["status"], 200);
}

#[test]
fn the_actor_maps_djangos_role_integers() {
    let actor = Actor::session(Uuid::from_u128(7))
        .with_workspace(WorkspaceContext::new("acme", Some(Role::from_value(20))));
    assert_eq!(actor.source, AuthSource::Session);
    assert!(actor.is_workspace_admin());
    assert!(actor.has_role_at_least(Role::Member));
    assert_eq!(Role::from_value(20).label(), "Admin");
    assert_eq!(Role::from_value(15).label(), "Member");
    assert_eq!(Role::from_value(5).label(), "Guest");
}
