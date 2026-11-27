// Copyright (c) 2023-present Plane Software, Inc. and contributors
// SPDX-License-Identifier: AGPL-3.0-only
// See the LICENSE file for details.

//! Response headers Django adds through `SecurityMiddleware` and
//! `XFrameOptionsMiddleware`.
//!
//! Plane overrides none of the defaults, so the values are Django 5.2.17's
//! `global_settings` (`contracts/golden/middleware-cases.json`, cases
//! `simple-*`/`not-found*`):
//!
//! | Header                      | Value          |
//! |-----------------------------|----------------|
//! | `X-Frame-Options`           | `DENY`         |
//! | `X-Content-Type-Options`    | `nosniff`      |
//! | `Referrer-Policy`           | `same-origin`  |
//! | `Cross-Origin-Opener-Policy`| `same-origin`  |
//!
//! All four are `setdefault`: a handler that sets its own value wins.

/// `X-Frame-Options`, from `X_FRAME_OPTIONS`.
pub const X_FRAME_OPTIONS: (&str, &str) = ("x-frame-options", "DENY");

/// `X-Content-Type-Options`, from `SECURE_CONTENT_TYPE_NOSNIFF`.
pub const CONTENT_TYPE_NOSNIFF: (&str, &str) = ("x-content-type-options", "nosniff");

/// `Referrer-Policy`, from `SECURE_REFERRER_POLICY`.
pub const REFERRER_POLICY: (&str, &str) = ("referrer-policy", "same-origin");

/// `Cross-Origin-Opener-Policy`, from `SECURE_CROSS_ORIGIN_OPENER_POLICY`.
pub const CROSS_ORIGIN_OPENER_POLICY: (&str, &str) = ("cross-origin-opener-policy", "same-origin");

/// The four headers, in the order Django's middleware adds them.
pub const DEFAULT_HEADERS: [(&str, &str); 4] = [
    CONTENT_TYPE_NOSNIFF,
    REFERRER_POLICY,
    CROSS_ORIGIN_OPENER_POLICY,
    X_FRAME_OPTIONS,
];

/// Minimal header view, so this module stays free of the HTTP crate.
pub trait Headers {
    /// Case-insensitive lookup.
    fn get(&self, name: &str) -> Option<String>;
    /// Case-insensitive set.
    fn set(&mut self, name: &str, value: &str);
    /// Case-insensitive insert-if-absent.
    fn set_default(&mut self, name: &str, value: &str) {
        if self.get(name).is_none() {
            self.set(name, value);
        }
    }
}

/// Apply all four headers without overwriting a handler's value.
pub fn apply(headers: &mut dyn Headers) {
    for (name, value) in DEFAULT_HEADERS {
        headers.set_default(name, value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[derive(Default)]
    struct Map(BTreeMap<String, String>);

    impl Headers for Map {
        fn get(&self, name: &str) -> Option<String> {
            self.0.get(&name.to_ascii_lowercase()).cloned()
        }

        fn set(&mut self, name: &str, value: &str) {
            self.0.insert(name.to_ascii_lowercase(), value.to_string());
        }
    }

    #[test]
    fn adds_exactly_the_four_django_headers() {
        let mut headers = Map::default();
        apply(&mut headers);
        assert_eq!(headers.0.len(), 4);
        assert_eq!(headers.get("x-frame-options").as_deref(), Some("DENY"));
        assert_eq!(
            headers.get("x-content-type-options").as_deref(),
            Some("nosniff")
        );
        assert_eq!(
            headers.get("referrer-policy").as_deref(),
            Some("same-origin")
        );
        assert_eq!(
            headers.get("cross-origin-opener-policy").as_deref(),
            Some("same-origin")
        );
    }

    #[test]
    fn existing_values_win() {
        let mut headers = Map(BTreeMap::from([(
            "x-frame-options".to_string(),
            "SAMEORIGIN".to_string(),
        )]));
        apply(&mut headers);
        assert_eq!(
            headers.get("x-frame-options").as_deref(),
            Some("SAMEORIGIN")
        );
    }
}
