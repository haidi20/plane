// Copyright (c) 2023-present Plane Software, Inc. and contributors
// SPDX-License-Identifier: AGPL-3.0-only
// See the LICENSE file for details.

//! Emit the route manifest compared against `contracts/routes.json`.
//!
//! ```text
//! cargo run -q -p plane-api --bin route-manifest > contracts/routes.rust.json
//! python contracts/tools/route_parity.py --rust contracts/routes.rust.json
//! ```

use std::fs;
use std::io::{self, Write};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let manifest = plane_domain::manifest();
    let rendered = serde_json::to_string_pretty(&manifest)? + "\n";

    // Shell redirection is encoding-dependent (PowerShell writes UTF-16), so the
    // binary writes the file itself and guarantees UTF-8 without BOM + LF.
    match std::env::args().nth(1).as_deref() {
        Some("--out") => {
            let path = std::env::args()
                .nth(2)
                .ok_or("usage: route-manifest [--out <path>]")?;
            fs::write(&path, rendered.as_bytes())?;
            eprintln!(
                "route-manifest -> {path} ({} routes)",
                manifest.routes.len()
            );
        }
        None => {
            let stdout = io::stdout();
            let mut lock = stdout.lock();
            lock.write_all(rendered.as_bytes())?;
        }
        Some(other) => return Err(format!("unknown argument `{other}`").into()),
    }
    Ok(())
}
