// Copyright (c) 2023-present Plane Software, Inc. and contributors
// SPDX-License-Identifier: AGPL-3.0-only
// See the LICENSE file for details.

//! Environment parity with Django.
//!
//! Names and defaults must match `apps/api/plane/settings/`, otherwise a
//! deployment switch silently changes behaviour during cutover.

use std::env;

/// Settings read once at process start.
#[derive(Debug, Clone)]
pub struct Config {
    /// `DJANGO_SETTINGS_MODULE` equivalent; selects defaults.
    pub settings_module: String,
    /// `DEBUG` as Django parses it (`1`/`true`).
    pub debug: bool,
    /// Socket address for the HTTP server.
    pub bind: String,
    /// `DATABASE_URL`; empty until the DB crate lands.
    pub database_url: String,
}

const DEFAULT_SETTINGS: &str = "plane.settings.production";

impl Config {
    /// Read the environment, applying Django\'s defaults.
    #[must_use]
    pub fn from_env() -> Self {
        Self {
            settings_module: env::var("DJANGO_SETTINGS_MODULE")
                .unwrap_or_else(|_| DEFAULT_SETTINGS.to_string()),
            debug: env_flag("DEBUG"),
            bind: env::var("PLANE_API_BIND").unwrap_or_else(|_| "0.0.0.0:8000".to_string()),
            database_url: env::var("DATABASE_URL").unwrap_or_default(),
        }
    }

    /// Settings module with the Django default applied.
    #[must_use]
    pub fn settings_module(&self) -> &str {
        if self.settings_module.is_empty() {
            DEFAULT_SETTINGS
        } else {
            &self.settings_module
        }
    }
}

/// Django treats `1`, `true`, `yes`, `on` (case-insensitive) as true.
#[must_use]
pub fn env_flag(name: &str) -> bool {
    match env::var(name) {
        Ok(value) => matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        Err(_) => false,
    }
}
