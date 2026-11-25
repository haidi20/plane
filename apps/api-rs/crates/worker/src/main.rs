// Copyright (c) 2023-present Plane Software, Inc. and contributors
// SPDX-License-Identifier: AGPL-3.0-only
// See the LICENSE file for details.

//! `plane-worker` binary. Jobs land with the Jobs/Non-HTTP cards.

use plane_pkg::{Config, ProcessEnv, logging};

#[tokio::main]
async fn main() {
    let config = match Config::load() {
        Ok(config) => config,
        Err(error) => {
            tracing::error!(%error, "invalid environment; refusing to start");
            std::process::exit(2);
        }
    };
    // After the config load: the per-logger levels depend on `DEBUG`.
    logging::init_for(config.debug);
    tracing::info!(
        settings = %config.settings_module(),
        env = ?config.redacted(&ProcessEnv),
        "plane-worker started; no jobs registered yet"
    );
    let _ = tokio::signal::ctrl_c().await;
}
