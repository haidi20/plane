// Copyright (c) 2023-present Plane Software, Inc. and contributors
// SPDX-License-Identifier: AGPL-3.0-only
// See the LICENSE file for details.

//! Pins the dev profile the workspace promises: opt-level 0, debug 1,
//! incremental. Without it, `cargo check` silently falls back to Cargo's
//! default (full debug info, no explicit opt-level) and the measured
//! warm-check budget in `docs/rust-api.md` (12.9) stops being true.

use std::fs;

fn workspace_manifest() -> String {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../Cargo.toml");
    fs::read_to_string(path).expect("workspace Cargo.toml")
}

fn profile_dev_value<'a>(manifest: &'a str, key: &str) -> Option<&'a str> {
    let mut in_profile = false;
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_profile = line == "[profile.dev]";
            continue;
        }
        if !in_profile {
            continue;
        }
        let (name, value) = line.split_once('=')?;
        if name.trim() == key {
            return Some(value.trim());
        }
    }
    None
}

#[test]
fn dev_profile_keeps_fast_incremental_builds() {
    let manifest = workspace_manifest();

    assert_eq!(profile_dev_value(&manifest, "opt-level"), Some("0"));
    assert_eq!(profile_dev_value(&manifest, "debug"), Some("1"));
    assert_eq!(profile_dev_value(&manifest, "incremental"), Some("true"));
}
