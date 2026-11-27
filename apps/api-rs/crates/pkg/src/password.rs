// Copyright (c) 2023-present Plane Software, Inc. and contributors
// SPDX-License-Identifier: AGPL-3.0-only
// See the LICENSE file for details.

//! Django's `pbkdf2_sha256` hasher in Rust: parse, verify, and rehash policy.
//!
//! Ported from `django/contrib/auth/hashers.py` (Django 5.2.17):
//!
//! | Behaviour | Source |
//! |-----------|--------|
//! | `pbkdf2_sha256$<iterations>$<salt>$<base64>` | `PBKDF2PasswordHasher.encode` |
//! | iterations read back from the stored hash | `decode().iterations` |
//! | `verify` re-encodes and compares the whole string | `verify` + `constant_time_compare` |
//! | `must_update` on iteration count **or** short salt | `must_update` + `must_update_salt` |
//! | `iterations=0` falls back to the default | `iterations = iterations or self.iterations` |
//! | `harden_runtime` burns the missing iterations | `harden_runtime` |
//! | `!` prefix means unusable | `is_password_usable` / `make_password(None)` |
//!
//! The values are measured, not restated: `contracts/tools/capture_password.py`
//! runs the real `PBKDF2PasswordHasher` source out of the venv's Django without
//! importing Django, and `crates/pkg/tests/password_golden.rs` replays
//! `contracts/golden/password-cases.json`.
//!
//! Scope: only `pbkdf2_sha256`. `PASSWORD_HASHERS` also lists `pbkdf2_sha1`,
//! `argon2`, `bcrypt_sha256`, and `scrypt`; a stored value with another prefix
//! is reported as [`PasswordError::NotSupported`] instead of being guessed.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;

/// `PBKDF2PasswordHasher.algorithm`.
pub const ALGORITHM: &str = "pbkdf2_sha256";

/// `PBKDF2PasswordHasher.iterations` in Django 5.2.17.
pub const ITERATIONS: u32 = 1_000_000;

/// `BasePasswordHasher.salt_entropy`.
pub const SALT_ENTROPY_BITS: u32 = 128;

/// `BasePasswordHasher.salt()` length: `ceil(128 / log2(62))`.
pub const SALT_CHARS: usize = 22;

/// `django.utils.crypto.RANDOM_STRING_CHARS`.
pub const RANDOM_STRING_CHARS: &str =
    "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";

/// `UNUSABLE_PASSWORD_PREFIX`.
pub const UNUSABLE_PREFIX: char = '!';

/// The parsed pieces of an encoded password.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Encoded {
    /// Work factor stored in the hash, read back on every verify.
    pub iterations: u32,
    /// Salt as stored; Django never re-derives it.
    pub salt: String,
    /// `base64.b64encode` of the derived key.
    pub hash: String,
}

/// Why a stored value could not be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PasswordError {
    /// The prefix names a hasher this port does not implement.
    NotSupported {
        /// The algorithm Django would have looked up.
        algorithm: String,
    },
    /// Not `<algorithm>$<iterations>$<salt>$<hash>`.
    Malformed,
    /// `_check_encode_args`: the salt is empty or contains `$`.
    InvalidSalt,
    /// `int()` succeeded but the value cannot be a work factor.
    InvalidIterations,
}

impl std::fmt::Display for PasswordError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotSupported { algorithm } => {
                write!(formatter, "unsupported password hasher: {algorithm}")
            }
            Self::Malformed => formatter.write_str("malformed encoded password"),
            Self::InvalidSalt => formatter.write_str("salt must be provided and cannot contain $"),
            Self::InvalidIterations => formatter.write_str("invalid pbkdf2 iteration count"),
        }
    }
}

impl std::error::Error for PasswordError {}

/// Work factor the running code applies, i.e. `hasher.iterations`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// `PBKDF2PasswordHasher.iterations` of the deployed hasher.
    pub iterations: u32,
}

impl Policy {
    /// The Django 5.2.17 default.
    pub const DEFAULT: Policy = Policy {
        iterations: ITERATIONS,
    };

    /// A policy with a different work factor, for tests and for replaying an
    /// older Django without recompiling.
    pub fn with_iterations(iterations: u32) -> Self {
        Self { iterations }
    }
}

/// `iterations = iterations or self.iterations`: zero means "use the default".
pub fn effective_iterations(iterations: u32) -> u32 {
    if iterations == 0 {
        ITERATIONS
    } else {
        iterations
    }
}

/// `PBKDF2PasswordHasher.decode`: four parts, then `int()` the second one.
/// Django only asserts the algorithm, so a wrong prefix is an error here rather
/// than a silent mismatch later.
pub fn parse(encoded: &str) -> Result<Encoded, PasswordError> {
    let mut parts = encoded.splitn(4, '$');
    let (Some(algorithm), Some(iterations), Some(salt), Some(hash)) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(PasswordError::Malformed);
    };
    if algorithm != ALGORITHM {
        return Err(PasswordError::NotSupported {
            algorithm: algorithm.to_string(),
        });
    }
    Ok(Encoded {
        iterations: python_int(iterations)?,
        salt: salt.to_string(),
        hash: hash.to_string(),
    })
}

/// `PBKDF2PasswordHasher.encode`. `None` and `Some(0)` both mean
/// [`ITERATIONS`], matching Python's `iterations or self.iterations`.
pub fn encode(
    password: &str,
    salt: &str,
    iterations: Option<u32>,
) -> Result<String, PasswordError> {
    // `BasePasswordHasher._check_encode_args`
    if salt.is_empty() || salt.contains('$') {
        return Err(PasswordError::InvalidSalt);
    }
    let iterations = effective_iterations(iterations.unwrap_or(0));
    let hash = BASE64.encode(pbkdf2_sha256(
        password.as_bytes(),
        salt.as_bytes(),
        iterations,
    ));
    Ok(format!("{ALGORITHM}${iterations}${salt}${hash}"))
}

/// `PBKDF2PasswordHasher.verify`: re-encode with the stored salt and work
/// factor, then compare the encoded strings in constant time.
pub fn verify(password: &str, encoded: &str) -> Result<bool, PasswordError> {
    let parsed = parse(encoded)?;
    let rendered = encode(password, &parsed.salt, Some(parsed.iterations))?;
    Ok(constant_time_compare(encoded, &rendered))
}

/// `PBKDF2PasswordHasher.must_update`: the work factor changed, or the salt is
/// shorter than the policy's entropy allows.
pub fn must_update(encoded: &str, policy: Policy) -> Result<bool, PasswordError> {
    let parsed = parse(encoded)?;
    Ok(parsed.iterations != policy.iterations || must_update_salt(&parsed.salt))
}

/// `must_update_salt`: `len(salt) * log2(len(RANDOM_STRING_CHARS)) < 128`.
pub fn must_update_salt(salt: &str) -> bool {
    let per_char = (RANDOM_STRING_CHARS.chars().count() as f64).log2();
    (salt.chars().count() as f64) * per_char < f64::from(SALT_ENTROPY_BITS)
}

/// `harden_runtime`: spend the iterations the stored hash is short of the
/// policy, discarding the result. Returns how many were spent.
pub fn harden_runtime(password: &str, encoded: &str, policy: Policy) -> Result<u32, PasswordError> {
    let parsed = parse(encoded)?;
    let extra = policy.iterations.saturating_sub(parsed.iterations);
    if extra > 0 {
        encode(password, &parsed.salt, Some(extra))?;
    }
    Ok(extra)
}

/// `is_password_usable`: `None` (no password set) and `!`-prefixed values are
/// not usable credentials.
pub fn is_usable(encoded: Option<&str>) -> bool {
    encoded.is_none_or(|value| !value.starts_with(UNUSABLE_PREFIX))
}

/// `mask_hash(hash, show=6)`: keep the first `show` characters, mask the rest.
pub fn mask_hash(hash: &str, show: usize) -> String {
    let kept: String = hash.chars().take(show).collect();
    let masked = hash.chars().count().saturating_sub(show);
    format!("{kept}{}", "*".repeat(masked))
}

/// `constant_time_compare`: compare the UTF-8 bytes without an early exit.
pub fn constant_time_compare(left: &str, right: &str) -> bool {
    left.as_bytes().ct_eq(right.as_bytes()).into()
}

/// `hashlib.pbkdf2_hmac("sha256", password, salt, iterations)` with a 32-byte
/// output: one PBKDF2 block, `T_1 = U_1 ^ ... ^ U_iterations`.
fn pbkdf2_sha256(password: &[u8], salt: &[u8], iterations: u32) -> [u8; 32] {
    let mut accumulated = [0_u8; 32];
    let mut previous = [0_u8; 32];

    // `U_1 = HMAC(password, salt || INT(1))`
    let mut mac =
        <Hmac<Sha256> as KeyInit>::new_from_slice(password).expect("HMAC accepts any key length");
    mac.update(salt);
    mac.update(&1_u32.to_be_bytes());
    previous.copy_from_slice(&mac.finalize().into_bytes());
    accumulated.copy_from_slice(&previous);

    for _ in 1..iterations {
        let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(password)
            .expect("HMAC accepts any key length");
        mac.update(&previous);
        previous.copy_from_slice(&mac.finalize().into_bytes());
        for (index, byte) in previous.iter().enumerate() {
            accumulated[index] ^= byte;
        }
    }
    accumulated
}

/// Python's `int()` for a stored iteration count.
///
/// Accepts surrounding whitespace, a leading `+`, and single underscores
/// between digits (`int("1_000") == 1000`). ASCII digits only: Python's `int()`
/// also reads Unicode decimal digits, which is recorded as a limitation rather
/// than guessed. A negative value parses in Python and then fails inside
/// `hashlib`, so it is reported as [`PasswordError::InvalidIterations`].
fn python_int(text: &str) -> Result<u32, PasswordError> {
    let trimmed = text.trim();
    let (negative, digits) = match trimmed.as_bytes().first() {
        Some(b'-') => (true, &trimmed[1..]),
        Some(b'+') => (false, &trimmed[1..]),
        _ => (false, trimmed),
    };
    if digits.is_empty() {
        return Err(PasswordError::Malformed);
    }
    let mut value: u64 = 0;
    let mut previous_underscore = true; // an underscore may not lead
    for character in digits.chars() {
        if character == '_' {
            if previous_underscore {
                return Err(PasswordError::Malformed);
            }
            previous_underscore = true;
            continue;
        }
        let Some(digit) = character.to_digit(10) else {
            return Err(PasswordError::Malformed);
        };
        if !character.is_ascii_digit() {
            return Err(PasswordError::Malformed);
        }
        previous_underscore = false;
        value = value
            .checked_mul(10)
            .and_then(|current| current.checked_add(u64::from(digit)))
            .ok_or(PasswordError::InvalidIterations)?;
    }
    if previous_underscore {
        return Err(PasswordError::Malformed);
    }
    if negative {
        return Err(PasswordError::InvalidIterations);
    }
    u32::try_from(value).map_err(|_| PasswordError::InvalidIterations)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_iterations_falls_back_to_the_default() {
        assert_eq!(effective_iterations(0), ITERATIONS);
        assert_eq!(effective_iterations(1), 1);
    }

    #[test]
    fn parse_reads_the_work_factor_from_the_hash() {
        let parsed = parse("pbkdf2_sha256$1_000$abc$Zm9v").expect("parsed");
        assert_eq!(parsed.iterations, 1_000);
        assert_eq!(parsed.salt, "abc");
        assert_eq!(parsed.hash, "Zm9v");
    }

    #[test]
    fn parse_accepts_python_int_syntax() {
        let spaced = parse("pbkdf2_sha256$ 1000 $s$h").expect("parsed");
        assert_eq!(spaced.iterations, 1_000);
        let signed = parse("pbkdf2_sha256$+1000$s$h").expect("parsed");
        assert_eq!(signed.iterations, 1_000);
        for bad in ["", "1__0", "_1", "1_", "abc", "1.5"] {
            let encoded = format!("pbkdf2_sha256${bad}$s$h");
            assert_eq!(parse(&encoded), Err(PasswordError::Malformed), "{bad:?}");
        }
        for bound in ["-1", "4294967296", "99999999999999999999999999"] {
            let encoded = format!("pbkdf2_sha256${bound}$s$h");
            assert_eq!(
                parse(&encoded),
                Err(PasswordError::InvalidIterations),
                "{bound:?}"
            );
        }
    }

    #[test]
    fn parse_rejects_other_algorithms_and_missing_parts() {
        assert_eq!(
            parse("pbkdf2_sha1$1000$s$h"),
            Err(PasswordError::NotSupported {
                algorithm: "pbkdf2_sha1".to_string()
            })
        );
        assert_eq!(parse("pbkdf2_sha256$1000$s"), Err(PasswordError::Malformed));
        assert_eq!(parse(""), Err(PasswordError::Malformed));
    }

    #[test]
    fn encode_and_verify_round_trip() {
        let encoded = encode(
            "correct horse battery staple",
            "vB6t9Xq2LmRw4ZpYc8Hd3g",
            Some(3),
        )
        .expect("encoded");
        assert!(encoded.starts_with("pbkdf2_sha256$3$vB6t9Xq2LmRw4ZpYc8Hd3g$"));
        assert_eq!(verify("correct horse battery staple", &encoded), Ok(true));
        assert_eq!(verify("wrong password", &encoded), Ok(false));
    }

    #[test]
    fn encode_rejects_an_unusable_salt() {
        assert_eq!(encode("pw", "", Some(3)), Err(PasswordError::InvalidSalt));
        assert_eq!(
            encode("pw", "a$b", Some(3)),
            Err(PasswordError::InvalidSalt)
        );
    }

    #[test]
    fn must_update_on_iterations_or_short_salt() {
        let current = encode("pw", "1234567890123456789012", Some(3)).expect("encoded");
        assert!(!must_update(&current, Policy::with_iterations(3)).expect("policy"));

        let old = encode("pw", "1234567890123456789012", Some(2)).expect("encoded");
        assert!(must_update(&old, Policy::with_iterations(3)).expect("policy"));

        let short = encode("pw", "123456789012345678901", Some(3)).expect("encoded");
        assert!(must_update(&short, Policy::with_iterations(3)).expect("policy"));
    }

    #[test]
    fn salt_entropy_boundary_is_twenty_two_characters() {
        assert!(must_update_salt(&"k".repeat(21)));
        assert!(!must_update_salt(&"k".repeat(SALT_CHARS)));
        assert!(!must_update_salt(&"k".repeat(23)));
    }

    #[test]
    fn harden_runtime_spends_the_shortfall() {
        let encoded = encode("pw", "salt", Some(10)).expect("encoded");
        assert_eq!(
            harden_runtime("pw", &encoded, Policy::with_iterations(30)),
            Ok(20)
        );
        assert_eq!(
            harden_runtime("pw", &encoded, Policy::with_iterations(5)),
            Ok(0)
        );
    }

    #[test]
    fn mask_hash_keeps_the_first_six_characters() {
        assert_eq!(mask_hash("abcdefghij", 6), "abcdef****");
        assert_eq!(mask_hash("short", 6), "short");
    }

    #[test]
    fn unusable_passwords_are_rejected() {
        assert!(is_usable(None));
        assert!(is_usable(Some("pbkdf2_sha256$1$s$h")));
        assert!(!is_usable(Some(
            "!0123456789012345678901234567890123456789"
        )));
    }

    #[test]
    fn constant_time_compare_matches_length_and_bytes() {
        assert!(constant_time_compare("abc", "abc"));
        assert!(!constant_time_compare("abc", "abd"));
        assert!(!constant_time_compare("abc", "abcd"));
        assert!(constant_time_compare("pässwörd", "pässwörd"));
    }
}
