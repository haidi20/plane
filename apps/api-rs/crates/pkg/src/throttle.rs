// Copyright (c) 2023-present Plane Software, Inc. and contributors
// SPDX-License-Identifier: AGPL-3.0-only
// See the LICENSE file for details.

//! DRF `SimpleRateThrottle` semantics, key and TTL included.
//!
//! Ported from DRF 3.18.3 (`rest_framework/throttling.py`) and Plane's
//! `plane/api/rate_limit.py`, with the golden answers in
//! `contracts/golden/middleware-cases.json`:
//!
//! | Observation        | Value                                    |
//! |--------------------|------------------------------------------|
//! | cache key (api key)| `api_key:key-abc`                         |
//! | cache key (anon)   | `throttle_anon_203.0.113.9`               |
//! | rate `2/minute`    | 2 requests, 60 s window                   |
//! | 3rd request        | denied, no `X-RateLimit-*`                |
//!
//! The two scopes do **not** share a format. `ApiKeyRateThrottle.get_cache_key`
//! overrides DRF and returns `f"{scope}:{api_key}"`, while
//! `AuthenticationThrottle` keeps `cache_format = "throttle_{scope}_{ident}"`.
//! That difference is a published contract: rotating the API key resets its
//! bucket, and the raw header value is part of the cache key.
//!
//! Django's cache adds `"{prefix}:{version}:"` around the key
//! (`BaseCache.make_key`), which with Plane's defaults is `":1:"`.
//!
//! The store is a trait because the real deployment needs Redis. Plane's
//! `[DATA] Pool Postgres & repository base` card owns that connection; this
//! module keeps the algorithm and the bytes testable without it.

use std::collections::HashMap;
use std::sync::Mutex;

/// Prefix `BaseCache.make_key` prepends with the default prefix and version.
pub const CACHE_PREFIX: &str = ":1:";

/// Result of parsing `"60/minute"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rate {
    /// Requests allowed inside the window.
    pub num_requests: u64,
    /// Window length in seconds.
    pub duration: u64,
}

/// Why a rate string could not be parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateError {
    /// No `/` separator.
    MissingSeparator,
    /// The count is not a decimal integer.
    InvalidCount,
    /// The period does not start with `s`, `m`, `h`, or `d`.
    InvalidPeriod,
}

impl std::fmt::Display for RateError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingSeparator => formatter.write_str("rate must look like `60/minute`"),
            Self::InvalidCount => formatter.write_str("rate count must be an integer"),
            Self::InvalidPeriod => formatter.write_str("rate period must start with s, m, h, or d"),
        }
    }
}

impl std::error::Error for RateError {}

/// `SimpleRateThrottle.parse_rate`.
///
/// DRF splits on `/` exactly once (`num, period = rate.split('/')`), so a value
/// with two slashes raises `ValueError` there; here it is
/// [`RateError::MissingSeparator`] for the same outcome: reject the value.
///
/// # Errors
///
/// Returns [`RateError`] when the string is not `"<count>/<period>"`.
pub fn parse_rate(rate: &str) -> Result<Rate, RateError> {
    let (count, period) = rate.split_once('/').ok_or(RateError::MissingSeparator)?;
    if period.contains('/') {
        return Err(RateError::MissingSeparator);
    }
    let num_requests = count.parse::<u64>().map_err(|_| RateError::InvalidCount)?;
    // DRF indexes `period[0]`, so `"minutes"` and `"m"` behave the same.
    let duration = match period.as_bytes().first() {
        Some(b's') => 1,
        Some(b'm') => 60,
        Some(b'h') => 3_600,
        Some(b'd') => 86_400,
        _ => return Err(RateError::InvalidPeriod),
    };
    Ok(Rate {
        num_requests,
        duration,
    })
}

/// `SimpleRateThrottle.cache_format`, used by `AnonRateThrottle` and
/// `AuthenticationThrottle`.
#[must_use]
pub fn cache_key(scope: &str, ident: &str) -> String {
    format!("throttle_{scope}_{ident}")
}

/// `ApiKeyRateThrottle.get_cache_key`: `f"{scope}:{api_key}"`.
///
/// Deliberately **not** [`cache_key`]. The raw header value is used verbatim
/// (no trimming, no hashing), and `None`/`""` means "no bucket" because
/// `allow_request` returns `None` from `get_cache_key` and DRF then allows the
/// request without touching the store.
#[must_use]
pub fn api_key_cache_key(scope: &str, api_key: &str) -> String {
    format!("{scope}:{api_key}")
}

/// The key Django's cache actually stores, prefix and version included.
#[must_use]
pub fn django_cache_key(scope: &str, ident: &str) -> String {
    format!("{CACHE_PREFIX}{}", cache_key(scope, ident))
}

/// `BaseCache.make_key` around an already-built key.
///
/// Used for scopes that override `get_cache_key` (the API key does), where
/// [`django_cache_key`] would add the wrong prefix.
#[must_use]
pub fn django_raw_cache_key(key: &str) -> String {
    format!("{CACHE_PREFIX}{key}")
}

/// `BaseThrottle.get_ident` from DRF.
///
/// * `num_proxies == Some(0)` or no `X-Forwarded-For` -> `REMOTE_ADDR`;
/// * otherwise the `n`-th address from the **right**, trimmed;
/// * `num_proxies == None` -> `X-Forwarded-For` with all whitespace removed.
///
/// Note the asymmetry with the logger port (`plane/utils/ip_address.py`), which
/// takes the first entry verbatim. Both are real; they are different functions.
#[must_use]
pub fn get_ident(
    forwarded_for: Option<&str>,
    remote_addr: Option<&str>,
    num_proxies: Option<u32>,
) -> Option<String> {
    let Some(num_proxies) = num_proxies else {
        return match forwarded_for {
            Some(value) => Some(value.split_whitespace().collect::<String>()),
            None => remote_addr.map(str::to_string),
        };
    };
    if num_proxies == 0 {
        return remote_addr.map(str::to_string);
    }
    let Some(forwarded_for) = forwarded_for else {
        return remote_addr.map(str::to_string);
    };
    let addrs: Vec<&str> = forwarded_for.split(',').collect();
    let take = usize::try_from(num_proxies).unwrap_or(usize::MAX);
    let index = addrs.len().saturating_sub(take);
    addrs.get(index).map(|value| value.trim().to_string())
}

/// One observation of the throttle window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Decision {
    /// `true` when the request is allowed.
    pub allowed: bool,
    /// `X-RateLimit-Remaining`, only on success, like Plane's override.
    pub remaining: Option<u64>,
    /// `X-RateLimit-Reset` (unix seconds), only on success.
    pub reset: Option<i64>,
    /// `Retry-After` seconds, only on failure.
    pub retry_after: Option<u64>,
}

/// Storage the throttle needs.
///
/// DRF stores a list of timestamps and refreshes its TTL on every write; both
/// must be preserved, so the methods mirror `cache.get` / `cache.set`.
pub trait Store: Send + Sync {
    /// Current history for a key, oldest last.
    fn get(&self, key: &str) -> Vec<f64>;
    /// Replace the history and set its TTL.
    fn set(&self, key: &str, history: Vec<f64>, ttl_seconds: u64);
    /// Remove one key, for tests and admin tooling.
    fn clear(&self, key: &str);
}

/// In-memory [`Store`]. Correct for one process; Redis replaces it in production.
#[derive(Debug, Default)]
pub struct MemoryStore {
    inner: Mutex<HashMap<String, Vec<f64>>>,
}

impl MemoryStore {
    /// Empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl Store for MemoryStore {
    fn get(&self, key: &str) -> Vec<f64> {
        self.inner
            .lock()
            .expect("throttle store lock")
            .get(key)
            .cloned()
            .unwrap_or_default()
    }

    fn set(&self, key: &str, history: Vec<f64>, _ttl_seconds: u64) {
        self.inner
            .lock()
            .expect("throttle store lock")
            .insert(key.to_string(), history);
    }

    fn clear(&self, key: &str) {
        self.inner.lock().expect("throttle store lock").remove(key);
    }
}

/// `SimpleRateThrottle.allow_request` plus Plane's `X-RateLimit-*` override.
///
/// `now` is passed in so the sliding window is testable; production passes the
/// wall clock (`f64` seconds, like `time.time()`).
pub fn allow(store: &dyn Store, rate: Rate, key: &str, now: f64) -> Decision {
    let mut history = store.get(key);
    // `while history and history[-1] <= now - duration: history.pop()`
    while history
        .last()
        .is_some_and(|last| *last <= now - rate.duration as f64)
    {
        history.pop();
    }

    if u64::try_from(history.len()).unwrap_or(u64::MAX) >= rate.num_requests {
        let wait = wait_seconds(&history, rate.duration, rate.num_requests, now);
        return Decision {
            allowed: false,
            remaining: None,
            reset: None,
            retry_after: wait,
        };
    }

    // `history.insert(0, now)` then `cache.set(key, history, duration)`.
    history.insert(0, now);
    store.set(key, history.clone(), rate.duration);

    let used = u64::try_from(history.len()).unwrap_or(u64::MAX);
    let available = rate.num_requests.saturating_sub(used);
    Decision {
        allowed: true,
        remaining: Some(available),
        reset: Some((now + rate.duration as f64) as i64),
        retry_after: None,
    }
}

/// `Throttled.get_wait()` -> `SimpleRateThrottle.wait()`, rounded up like
/// `DRF` does before rendering `Retry-After`.
fn wait_seconds(history: &[f64], duration: u64, num_requests: u64, now: f64) -> Option<u64> {
    let oldest = *history.last()?;
    let remaining_duration = duration as f64 - (now - oldest);
    let available = i64::try_from(num_requests).unwrap_or(i64::MAX)
        - i64::try_from(history.len()).unwrap_or(i64::MAX)
        + 1;
    if available <= 0 {
        return None;
    }
    let wait = remaining_duration / available as f64;
    Some(wait.max(0.0).ceil() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_rate_matches_drf() {
        assert_eq!(
            parse_rate("60/minute"),
            Ok(Rate {
                num_requests: 60,
                duration: 60
            })
        );
        assert_eq!(
            parse_rate("5/m"),
            Ok(Rate {
                num_requests: 5,
                duration: 60
            })
        );
        assert_eq!(
            parse_rate("1/s"),
            Ok(Rate {
                num_requests: 1,
                duration: 1
            })
        );
        assert_eq!(parse_rate("10/hour").map(|rate| rate.duration), Ok(3_600));
        assert_eq!(parse_rate("10/day").map(|rate| rate.duration), Ok(86_400));
        assert_eq!(parse_rate("10"), Err(RateError::MissingSeparator));
        assert_eq!(parse_rate("x/minute"), Err(RateError::InvalidCount));
        assert_eq!(parse_rate("10/w Minute"), Err(RateError::InvalidPeriod));
    }

    #[test]
    fn cache_keys_match_the_golden() {
        // `ApiKeyRateThrottle` overrides `get_cache_key`.
        assert_eq!(api_key_cache_key("api_key", "key-abc"), "api_key:key-abc");
        assert_eq!(
            django_raw_cache_key(&api_key_cache_key("api_key", "key-abc")),
            ":1:api_key:key-abc"
        );
        // `AnonRateThrottle` / `AuthenticationThrottle` keep `cache_format`.
        assert_eq!(
            cache_key("anon", "203.0.113.9"),
            "throttle_anon_203.0.113.9"
        );
        assert_eq!(
            django_cache_key("authentication", "203.0.113.9"),
            ":1:throttle_authentication_203.0.113.9"
        );
    }

    #[test]
    fn ident_uses_num_proxies_like_drf() {
        // `num_proxies is None` -> `"".join(xff.split())`: whitespace removed,
        // the comma stays. Confirmed against DRF's source, not assumed.
        assert_eq!(
            get_ident(Some("1.2.3.4, 5.6.7.8"), Some("9.9.9.9"), None),
            Some("1.2.3.4,5.6.7.8".to_string())
        );
        assert_eq!(
            get_ident(Some("1.2.3.4, 5.6.7.8"), Some("9.9.9.9"), Some(1)),
            Some("5.6.7.8".to_string())
        );
        assert_eq!(
            get_ident(Some("1.2.3.4, 5.6.7.8"), Some("9.9.9.9"), Some(2)),
            Some("1.2.3.4".to_string())
        );
        assert_eq!(
            get_ident(Some("1.2.3.4, 5.6.7.8"), Some("9.9.9.9"), Some(0)),
            Some("9.9.9.9".to_string())
        );
        assert_eq!(
            get_ident(None, Some("9.9.9.9"), Some(2)),
            Some("9.9.9.9".to_string())
        );
        assert_eq!(get_ident(None, None, None), None);
    }

    #[test]
    fn sliding_window_allows_then_blocks() {
        let store = MemoryStore::new();
        let rate = parse_rate("2/minute").expect("rate");
        let key = api_key_cache_key("api_key", "key-abc");

        let first = allow(&store, rate, &key, 1_000.0);
        assert!(first.allowed);
        assert_eq!(first.remaining, Some(1));
        assert_eq!(first.reset, Some(1_060));

        let second = allow(&store, rate, &key, 1_001.0);
        assert!(second.allowed);
        assert_eq!(second.remaining, Some(0));

        let third = allow(&store, rate, &key, 1_002.0);
        assert!(!third.allowed);
        assert_eq!(
            third.remaining, None,
            "Plane sets the header only on success"
        );
        assert_eq!(third.reset, None);
        // `wait()` is `remaining_duration / (num_requests - len(history) + 1)`
        // then `ceil()`: (60 - 2) / (2 - 2 + 1) = 58.
        assert_eq!(third.retry_after, Some(58));

        // The window slides: after 60 s the old entries are dropped.
        let later = allow(&store, rate, &key, 1_061.0);
        assert!(later.allowed);
        assert_eq!(later.remaining, Some(1));
    }

    #[test]
    fn remaining_never_goes_below_zero() {
        let store = MemoryStore::new();
        let rate = parse_rate("1/minute").expect("rate");
        let key = cache_key("anon", "1.1.1.1");
        assert_eq!(allow(&store, rate, &key, 10.0).remaining, Some(0));
        assert!(!allow(&store, rate, &key, 11.0).allowed);
    }
}
