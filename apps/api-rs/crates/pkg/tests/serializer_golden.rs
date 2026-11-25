// Copyright (c) 2023-present Plane Software, Inc. and contributors
// SPDX-License-Identifier: AGPL-3.0-only
// See the LICENSE file for details.

//! Byte comparison against the DRF capture in `contracts/golden/serializer-*`.
//!
//! Each case replays the same field values through [`plane_pkg::compact_json`]
//! and compares the result with the `body.raw` written by `capture_serializer.py`.

use chrono::{DateTime, FixedOffset, NaiveDate, Utc};
use plane_pkg::compact_json;
use plane_pkg::python_repr::{iso8601, iso8601_date, offset_for_timezone};
use serde::Serialize;
use std::path::{Path, PathBuf};

const UUID: &str = "deb5350a-f8c9-4cd5-9875-5323dc6cab2a";
const NAME: &str = "Ren\u{e9} \u{2014} \u{4f60}\u{597d}";

#[derive(Serialize)]
struct Fields {
    id: &'static str,
    created_at: String,
    updated_at: String,
    start_date: Option<String>,
    estimate: Option<f64>,
    sort_order: f64,
    tiny: f64,
    big: f64,
    name: Option<&'static str>,
    description: Option<&'static str>,
}

fn golden(name: &str) -> Vec<u8> {
    let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../../contracts/golden")
        .join(name)
        .join("body.raw");
    std::fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn instant() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-11-24T17:21:52.123759Z")
        .expect("instant")
        .with_timezone(&Utc)
}

fn fixed_offset(name: &str) -> FixedOffset {
    offset_for_timezone(name, instant()).expect("offset")
}

fn fields(micros: bool, offset: FixedOffset) -> Fields {
    let value = if micros {
        instant()
    } else {
        DateTime::parse_from_rfc3339("2026-11-24T17:21:52Z")
            .expect("instant")
            .with_timezone(&Utc)
    };
    let updated = if micros {
        value
    } else {
        // `updated_at` in the no-micros case is 1000 us, not 1 us.
        DateTime::parse_from_rfc3339("2026-11-24T17:21:52.001Z")
            .expect("instant")
            .with_timezone(&Utc)
    };
    Fields {
        id: UUID,
        created_at: iso8601(value, offset),
        updated_at: iso8601(updated, offset),
        start_date: Some(iso8601_date(
            NaiveDate::from_ymd_opt(2026, 1, 2).expect("date"),
        )),
        estimate: Some(3.5),
        sort_order: 65535.0,
        tiny: 1e-7,
        big: 1e16,
        name: Some(NAME),
        description: None,
    }
}

fn assert_matches(case: &str, value: &Fields) {
    let rendered = compact_json(value).expect("encode");
    let expected = golden(case);
    assert_eq!(
        rendered.as_bytes(),
        expected.as_slice(),
        "{case}\nrendered: {rendered}\nexpected: {}",
        String::from_utf8_lossy(&expected)
    );
}

#[test]
fn utc_fields_match_the_drf_body() {
    assert_matches("serializer-fields", &fields(true, fixed_offset("UTC")));
}

#[test]
fn jakarta_fields_are_not_z() {
    let case = fields(true, fixed_offset("Asia/Jakarta"));
    assert!(case.created_at.ends_with("+07:00"), "{}", case.created_at);
    assert_matches("serializer-fields-jakarta", &case);
}

#[test]
fn zero_microseconds_are_dropped_but_a_thousand_are_kept() {
    assert_matches(
        "serializer-fields-no-micros",
        &fields(false, fixed_offset("UTC")),
    );
}

#[test]
fn nulls_stay_null_and_absent_fields_stay_absent() {
    let mut case = fields(true, fixed_offset("UTC"));
    case.start_date = None;
    case.estimate = None;
    case.name = None;
    case.description = None;
    let rendered = compact_json(&case).expect("encode");
    assert_eq!(rendered.as_bytes(), golden("serializer-fields-nulls"));
    assert!(!rendered.contains("missing"), "{rendered}");
}

#[test]
fn float_cases_match_the_drf_body() {
    let mut case = fields(true, fixed_offset("UTC"));
    case.sort_order = 0.0;
    case.tiny = 1e-5;
    case.big = 1.7976931348623157e308;
    let rendered = compact_json(&case).expect("encode");
    assert!(rendered.contains("\"tiny\":1e-05"), "{rendered}");
    assert!(
        rendered.contains("\"big\":1.7976931348623157e+308"),
        "{rendered}"
    );
    assert!(rendered.contains("\"sort_order\":0.0"), "{rendered}");
    assert_eq!(rendered.as_bytes(), golden("serializer-fields-floats"));
}
