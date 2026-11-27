// Copyright (c) 2023-present Plane Software, Inc. and contributors
// SPDX-License-Identifier: AGPL-3.0-only
// See the LICENSE file for details.

//! Python's `repr(float)` and DRF's datetime format, reproduced byte for byte.
//!
//! `serde_json` writes the shortest round-trippable form, which is almost what
//! Python does but not the same text:
//!
//! | value        | Python `repr` | `serde_json` |
//! |--------------|---------------|--------------|
//! | `1e16`       | `1e+16`       | `1e16`       |
//! | `1e-5`       | `1e-05`       | `0.00001`    |
//! | `-0.0`       | `-0.0`        | `-0.0`       |
//!
//! Every `FloatField` in Plane goes through this, so a `sort_order` of `65535`
//! must be written `65535.0` and a `1e16` must keep the `+` and the two-digit
//! exponent. The rules here are measured, not guessed:
//! `contracts/golden/serializer-float-cases.json` holds 218 `repr()` outputs
//! recorded from CPython 3.12.

use chrono::{DateTime, FixedOffset, Offset, Utc};

/// `repr(float)` from CPython, including `-0.0` and the exponent spelling.
///
/// CPython switches to scientific notation when the decimal exponent is below
/// `-4` or at least `16`, pads the exponent to two digits, and always writes a
/// sign. That is exactly `format!("{:?}", value)` on a `f64` *except* for the
/// exponent padding, so the shortest round-trip digits come from `{:?}` and the
/// exponent is normalised afterwards.
#[must_use]
pub fn repr_float(value: f64) -> String {
    if value.is_nan() {
        return "nan".to_string();
    }
    if value.is_infinite() {
        return if value.is_sign_negative() {
            "-inf"
        } else {
            "inf"
        }
        .to_string();
    }

    // `{:?}` gives the shortest round-trippable digits in Rust, the same set of
    // digits Python uses, but spells the exponent differently.
    let rendered = format!("{value:?}");
    let Some((mantissa, exponent)) = rendered.split_once(['e', 'E']) else {
        // No exponent: Rust already wrote `65535.0` / `0.0001`, matching Python.
        return rendered;
    };

    let exponent_value: i32 = exponent.parse().expect("integer exponent from `{:?}`");
    let sign = if exponent_value < 0 { '-' } else { '+' };
    format!("{mantissa}e{sign}{:02}", exponent_value.unsigned_abs())
}

/// DRF `DateTimeField.to_representation` for an aware value.
///
/// DRF calls `value.isoformat()`, replaces a `+00:00` suffix with `Z`, and drops
/// the microseconds when they are zero. The offset comes from the *active*
/// timezone, which `TimezoneMixin` sets to the user's `user_timezone`; a Jakarta
/// user therefore sees `+07:00`, not `Z`. Callers pass the offset they resolved.
#[must_use]
pub fn iso8601(value: DateTime<Utc>, offset: FixedOffset) -> String {
    let local = value.with_timezone(&offset);
    let base = local.format("%Y-%m-%dT%H:%M:%S").to_string();
    let micros = local.timestamp_subsec_micros();
    let suffix = if micros == 0 {
        String::new()
    } else {
        format!(".{micros:06}")
    };
    format!("{base}{suffix}{}", offset_suffix(&local))
}

/// Offset spelled the way `datetime.isoformat()` does: `Z` for UTC, otherwise
/// `+HH:MM` (or `-HH:MM`), never seconds.
fn offset_suffix(local: &DateTime<chrono::FixedOffset>) -> String {
    let seconds = local.offset().local_minus_utc();
    if seconds == 0 {
        return "Z".to_string();
    }
    let sign = if seconds < 0 { '-' } else { '+' };
    let total = seconds.unsigned_abs();
    format!("{sign}{:02}:{:02}", total / 3600, (total % 3600) / 60)
}

/// Resolve the fixed offset Django would use for `user_timezone` at `instant`.
///
/// `TimezoneMixin` calls `timezone.activate(user.timezone)`; the serializer then
/// formats the value in that zone. A DST-observing zone therefore has two
/// possible offsets, so the offset must come from the instant, not just the name.
///
/// # Errors
/// Returns the unknown name when the IANA name is not in the `chrono-tz` table;
/// callers fall back to UTC, which is Django's own default.
pub fn offset_for_timezone(timezone: &str, instant: DateTime<Utc>) -> Result<FixedOffset, String> {
    let zone: chrono_tz::Tz = timezone.parse().map_err(|_| timezone.to_string())?;
    Ok(instant.with_timezone(&zone).offset().fix())
}

/// `DateField`: plain `YYYY-MM-DD`, no timezone involved.
#[must_use]
pub fn iso8601_date(value: chrono::NaiveDate) -> String {
    value.format("%Y-%m-%d").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floats_match_python_repr() {
        let cases = [
            (0.0, "0.0"),
            (-0.0, "-0.0"),
            (65535.0, "65535.0"),
            (300.0, "300.0"),
            (1e16, "1e+16"),
            (1e20, "1e+20"),
            (1e-5, "1e-05"),
            (1e-7, "1e-07"),
            (1.5e-10, "1.5e-10"),
            (5e-324, "5e-324"),
            (1.7976931348623157e308, "1.7976931348623157e+308"),
            (1.0 / 3.0, "0.3333333333333333"),
            (1234567890.1234567, "1234567890.1234567"),
            (0.0001, "0.0001"),
        ];
        for (value, expected) in cases {
            assert_eq!(repr_float(value), expected, "{value}");
        }
    }

    #[test]
    fn every_frozen_float_case_matches() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../contracts/golden/serializer-float-cases.json");
        let raw = std::fs::read_to_string(path).expect("float case table");
        let cases: Vec<serde_json::Value> = serde_json::from_str(&raw).expect("parse");
        assert_eq!(cases.len(), 218, "case count");
        for case in cases {
            let value = case["value"].as_f64().expect("number");
            let expected = case["repr"].as_str().expect("repr");
            assert_eq!(repr_float(value), expected, "value {value}");
        }
    }

    #[test]
    fn non_finite_values_match_python_repr() {
        assert_eq!(repr_float(f64::NAN), "nan");
        assert_eq!(repr_float(f64::INFINITY), "inf");
        assert_eq!(repr_float(f64::NEG_INFINITY), "-inf");
    }

    #[test]
    fn iso8601_matches_drf_in_utc() {
        let value = DateTime::parse_from_rfc3339("2026-11-24T17:21:52.123759Z")
            .expect("parse")
            .with_timezone(&Utc);
        assert_eq!(
            iso8601(value, FixedOffset::east_opt(0).expect("utc")),
            "2026-11-24T17:21:52.123759Z"
        );
    }

    #[test]
    fn iso8601_drops_a_zero_microsecond_but_keeps_the_offset() {
        let value = DateTime::parse_from_rfc3339("2026-11-24T17:21:52Z")
            .expect("parse")
            .with_timezone(&Utc);
        assert_eq!(
            iso8601(value, FixedOffset::east_opt(0).expect("utc")),
            "2026-11-24T17:21:52Z"
        );
        let micros = DateTime::parse_from_rfc3339("2026-11-24T17:21:52.001Z")
            .expect("parse")
            .with_timezone(&Utc);
        assert_eq!(
            iso8601(micros, FixedOffset::east_opt(0).expect("utc")),
            "2026-11-24T17:21:52.001000Z"
        );
    }

    #[test]
    fn iso8601_renders_the_users_offset_not_z() {
        // Measured: TimezoneMixin activates `user_timezone`, so Jakarta gets
        // `+07:00` at the same instant, and the date can move a day forward.
        let value = DateTime::parse_from_rfc3339("2026-11-24T17:21:52.123759Z")
            .expect("parse")
            .with_timezone(&Utc);
        let jakarta = FixedOffset::east_opt(7 * 3600).expect("offset");
        assert_eq!(iso8601(value, jakarta), "2026-11-25T00:21:52.123759+07:00");
        let kolkata = FixedOffset::east_opt(5 * 3600 + 1800).expect("offset");
        assert_eq!(iso8601(value, kolkata), "2026-11-24T22:51:52.123759+05:30");
        let new_york = FixedOffset::west_opt(5 * 3600).expect("offset");
        assert_eq!(iso8601(value, new_york), "2026-11-24T12:21:52.123759-05:00");
    }

    #[test]
    fn timezone_names_resolve_the_offset_for_the_instant() {
        let summer = DateTime::parse_from_rfc3339("2026-07-01T12:00:00Z")
            .expect("parse")
            .with_timezone(&Utc);
        let winter = DateTime::parse_from_rfc3339("2026-01-01T12:00:00Z")
            .expect("parse")
            .with_timezone(&Utc);
        // New York is -04:00 in July and -05:00 in January: the offset cannot be
        // cached per name.
        assert_eq!(
            offset_for_timezone("America/New_York", summer).expect("summer"),
            FixedOffset::west_opt(4 * 3600).expect("offset")
        );
        assert_eq!(
            offset_for_timezone("America/New_York", winter).expect("winter"),
            FixedOffset::west_opt(5 * 3600).expect("offset")
        );
        assert_eq!(
            offset_for_timezone("Asia/Jakarta", summer).expect("jakarta"),
            FixedOffset::east_opt(7 * 3600).expect("offset")
        );
        assert_eq!(
            offset_for_timezone("Not/AZone", summer),
            Err("Not/AZone".to_string())
        );
    }

    #[test]
    fn dates_have_no_time_and_no_offset() {
        let date = chrono::NaiveDate::from_ymd_opt(2026, 1, 2).expect("date");
        assert_eq!(iso8601_date(date), "2026-01-02");
    }
}
