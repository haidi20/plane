// Copyright (c) 2023-present Plane Software, Inc. and contributors
// SPDX-License-Identifier: AGPL-3.0-only
// See the LICENSE file for details.

//! `route-manifest --out` writes a readable manifest, and creates the output
//! directory when it does not exist.
//!
//! `apps/api-rs/contracts/` is gitignored, so a fresh checkout has no such
//! directory. Without `create_dir_all`, `just parity` and the CI
//! `route-contract` job fail with `NotFound` before any comparison runs.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "plane-route-manifest-{name}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

#[test]
fn out_creates_a_missing_parent_directory() {
    let dir = temp_dir("missing-parent");
    let target = dir.join("contracts").join("routes.rust.json");
    assert!(!target.parent().expect("parent").exists());

    let status = Command::new(env!("CARGO_BIN_EXE_route-manifest"))
        .arg("--out")
        .arg(&target)
        .status()
        .expect("run route-manifest");

    assert!(status.success(), "route-manifest exited with {status}");
    let written = fs::read_to_string(&target).expect("manifest file exists");
    let parsed: serde_json::Value = serde_json::from_str(&written).expect("valid JSON");
    assert_eq!(parsed["generated_by"], "plane-api --bin route-manifest");
    assert!(
        !written.starts_with('\u{feff}'),
        "manifest must not have a BOM"
    );

    fs::remove_dir_all(&dir).expect("clean temp dir");
}
