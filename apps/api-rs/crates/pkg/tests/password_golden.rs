// Copyright (c) 2023-present Plane Software, Inc. and contributors
// SPDX-License-Identifier: AGPL-3.0-only
// See the LICENSE file for details.

//! Replay `contracts/golden/password-cases.json` against `plane_pkg::password`.
//!
//! The golden is recorded by `contracts/tools/capture_password.py`, which pulls
//! `PBKDF2PasswordHasher` and its helpers out of the venv's Django 5.2.17 with
//! `ast` and executes them. Every value below came from Django's own code; no
//! database and no container were involved.
//!
//! Rows marked `slow` use the production work factor (1_000_000) or a 260_000
//! hash. They are replayed by the `#[ignore]` test, which is meant to run with
//! `--release`:
//!
//! ```text
//! cargo test -p plane-pkg --release --test password_golden -- --ignored
//! ```

use std::path::Path;

use plane_pkg::password::{self, PasswordError, Policy};
use serde_json::Value;

fn golden() -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../../contracts/golden/password-cases.json");
    serde_json::from_slice(&std::fs::read(path).expect("password-cases.json"))
        .expect("valid golden json")
}

fn slow(row: &Value) -> bool {
    row["slow"].as_bool().unwrap_or(false)
}

#[test]
fn the_recording_is_stamped_with_the_measured_versions() {
    let golden = golden();
    assert_eq!(golden["schema"], "plane/password-cases@1");
    assert_eq!(golden["django"], "5.2.17");
    let python = golden["python"].as_str().expect("python version");
    assert!(
        python.starts_with("3.12."),
        "the golden must be recorded with the API interpreter, got {python}"
    );
}

#[test]
fn the_rust_constants_match_the_hasher() {
    let hasher = &golden()["hasher"];
    assert_eq!(hasher["algorithm"], password::ALGORITHM);
    assert_eq!(hasher["iterations"], password::ITERATIONS);
    assert_eq!(hasher["salt_entropy"], password::SALT_ENTROPY_BITS);
    assert_eq!(hasher["salt_length"], password::SALT_CHARS);
    assert_eq!(hasher["random_string_chars"], password::RANDOM_STRING_CHARS);
    assert_eq!(hasher["digest"], "sha256");
}

#[test]
fn encode_matches_django() {
    let golden = golden();
    for row in golden["encode"].as_array().expect("encode") {
        if slow(row) {
            continue;
        }
        let password_text = row["password"].as_str().expect("password");
        let salt = row["salt"].as_str().expect("salt");
        let iterations = row["iterations"].as_u64().map(|value| value as u32);
        match row["error"].as_str() {
            None => {
                let expected = row["encoded"].as_str().expect("encoded");
                let encoded = password::encode(password_text, salt, iterations).expect("encoded");
                assert_eq!(encoded, expected, "{password_text:?}");
            }
            Some("ValueError") => assert_eq!(
                password::encode(password_text, salt, iterations),
                Err(PasswordError::InvalidSalt),
                "{salt:?}"
            ),
            Some(other) => panic!("unexpected capture error: {other}"),
        }
    }
}

#[test]
fn verify_matches_django() {
    let golden = golden();
    for row in golden["verify"].as_array().expect("verify") {
        if slow(row) {
            continue;
        }
        let password_text = row["password"].as_str().expect("password");
        let encoded = row["encoded"].as_str().expect("encoded");
        match row["error"].as_str() {
            None => {
                let expected = row["ok"].as_bool().expect("ok");
                assert_eq!(
                    password::verify(password_text, encoded),
                    Ok(expected),
                    "{encoded}"
                );
            }
            Some("AssertionError") => assert_eq!(
                password::verify(password_text, encoded),
                Err(PasswordError::NotSupported {
                    algorithm: "pbkdf2_sha1".to_string()
                }),
                "{encoded}"
            ),
            Some("ValueError") => assert_eq!(
                password::verify(password_text, encoded),
                Err(PasswordError::Malformed),
                "{encoded}"
            ),
            Some(other) => panic!("unexpected capture error: {other}"),
        }
    }
}

#[test]
fn parse_follows_pythons_int() {
    let golden = golden();
    for row in golden["parse"].as_array().expect("parse") {
        let encoded = row["encoded"].as_str().expect("encoded");
        match row["iterations"].as_i64() {
            Some(expected) if expected < 0 => assert_eq!(
                password::parse(encoded).map(|parsed| parsed.iterations),
                Err(PasswordError::InvalidIterations),
                "{encoded}"
            ),
            Some(expected) => {
                let parsed = password::parse(encoded).expect("parsed");
                assert_eq!(parsed.iterations, expected as u32, "{encoded}");
            }
            None => assert_eq!(
                password::parse(encoded).map(|parsed| parsed.iterations),
                Err(PasswordError::Malformed),
                "{encoded}"
            ),
        }
    }
}

#[test]
fn must_update_matches_django() {
    let golden = golden();
    for row in golden["must_update"].as_array().expect("must_update") {
        let encoded = row["encoded"].as_str().expect("encoded");
        let current = row["policy_iterations"].as_u64().expect("policy") as u32;
        let expected = row["must_update"].as_bool().expect("must_update");
        assert_eq!(
            password::must_update(encoded, Policy::with_iterations(current)),
            Ok(expected),
            "{}",
            row["note"].as_str().unwrap_or("")
        );
    }
}

#[test]
fn salt_entropy_matches_django() {
    let golden = golden();
    for row in golden["must_update_salt"].as_array().expect("salts") {
        let salt = row["salt"].as_str().expect("salt");
        assert_eq!(
            password::must_update_salt(salt),
            row["must_update"].as_bool().expect("must_update"),
            "{} characters",
            salt.len()
        );
    }
}

#[test]
fn harden_runtime_spends_the_same_iterations() {
    let golden = golden();
    for row in golden["harden_runtime"].as_array().expect("harden") {
        let encoded = row["encoded"].as_str().expect("encoded");
        let current = row["policy_iterations"].as_u64().expect("policy") as u32;
        let spent = row["spent_iterations"].as_u64().expect("spent") as u32;
        assert_eq!(
            password::harden_runtime(
                "correct horse battery staple",
                encoded,
                Policy::with_iterations(current)
            ),
            Ok(spent),
            "{current}"
        );
    }
}

#[test]
fn mask_hash_and_compare_match_django() {
    let golden = golden();
    for row in golden["mask_hash"].as_array().expect("mask") {
        let hash = row["hash"].as_str().expect("hash");
        let show = row["show"].as_u64().expect("show") as usize;
        assert_eq!(
            password::mask_hash(hash, show),
            row["masked"].as_str().expect("masked")
        );
    }
    for row in golden["constant_time_compare"].as_array().expect("compare") {
        let left = row["left"].as_str().expect("left");
        let right = row["right"].as_str().expect("right");
        assert_eq!(
            password::constant_time_compare(left, right),
            row["equal"].as_bool().expect("equal"),
            "{left:?} / {right:?}"
        );
    }
}

#[test]
fn unusable_passwords_match_django() {
    let golden = golden();
    for row in golden["usable"].as_array().expect("usable") {
        assert_eq!(
            password::is_usable(row["encoded"].as_str()),
            row["usable"].as_bool().expect("usable"),
            "{:?}",
            row["encoded"]
        );
    }
}

/// Production work factor: 1_000_000 iterations for a new hash, plus the slow
/// verify rows (260_000 and 1_000_000). Debug builds are too slow for this, so
/// it is ignored by default and meant for `cargo test --release ... -- --ignored`.
#[test]
#[ignore = "1M pbkdf2 iterations: run with --release"]
fn production_and_old_hashes_round_trip() {
    let golden = golden();

    for row in golden["encode"].as_array().expect("encode") {
        if !slow(row) {
            continue;
        }
        let password_text = row["password"].as_str().expect("password");
        let salt = row["salt"].as_str().expect("salt");
        let iterations = row["iterations"].as_u64().map(|value| value as u32);
        assert_eq!(
            password::encode(password_text, salt, iterations).expect("encoded"),
            row["encoded"].as_str().expect("encoded")
        );
    }

    for row in golden["verify"].as_array().expect("verify") {
        if !slow(row) {
            continue;
        }
        let password_text = row["password"].as_str().expect("password");
        let encoded = row["encoded"].as_str().expect("encoded");
        assert_eq!(
            password::verify(password_text, encoded),
            Ok(row["ok"].as_bool().expect("ok")),
            "{encoded}"
        );
    }
}
