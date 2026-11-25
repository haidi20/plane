// Copyright (c) 2023-present Plane Software, Inc. and contributors
// SPDX-License-Identifier: AGPL-3.0-only
// See the LICENSE file for details.

//! `plane-api` binary.

use plane_api::router;
use plane_pkg::{Config, ProcessEnv, logging};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
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
        debug = config.debug,
        env = ?config.redacted(&ProcessEnv),
        "environment loaded"
    );
    let app = router();

    let listener = tokio::net::TcpListener::bind(&config.bind).await?;
    tracing::info!(bind = %config.bind, "plane-api listening");
    axum::serve(listener, app).await?;
    Ok(())
}
