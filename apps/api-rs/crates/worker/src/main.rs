// Copyright (c) 2023-present Plane Software, Inc. and contributors
// SPDX-License-Identifier: AGPL-3.0-only
// See the LICENSE file for details.

//! `plane-worker` binary. Jobs land with the Jobs/Non-HTTP cards.

use plane_pkg::{Config, logging};

#[tokio::main]
async fn main() {
    logging::init();
    let config = Config::from_env();
    tracing::info!(
        settings = %config.settings_module(),
        "plane-worker started; no jobs registered yet"
    );
    let _ = tokio::signal::ctrl_c().await;
}
