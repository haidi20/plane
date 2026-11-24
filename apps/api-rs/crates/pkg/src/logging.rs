// Copyright (c) 2023-present Plane Software, Inc. and contributors
// SPDX-License-Identifier: AGPL-3.0-only
// See the LICENSE file for details.

//! JSON logging with the fields the Django request logger emits.

use tracing_subscriber::{EnvFilter, fmt, prelude::*};

/// Install a JSON subscriber. `RUST_LOG` wins over the default level.
pub fn init() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().json().with_target(true))
        .try_init();
}
