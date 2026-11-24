// Copyright (c) 2023-present Plane Software, Inc. and contributors
// SPDX-License-Identifier: AGPL-3.0-only
// See the LICENSE file for details.

//! `plane-api` binary.

use plane_api::router;
use plane_pkg::{Config, logging};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    logging::init();
    let config = Config::from_env();
    let app = router();

    let listener = tokio::net::TcpListener::bind(&config.bind).await?;
    tracing::info!(
        bind = %config.bind,
        settings = %config.settings_module(),
        debug = config.debug,
        "plane-api listening"
    );
    axum::serve(listener, app).await?;
    Ok(())
}
