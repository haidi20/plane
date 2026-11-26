// Copyright (c) 2023-present Plane Software, Inc. and contributors
// SPDX-License-Identifier: AGPL-3.0-only
// See the LICENSE file for details.

//! Cross-cutting pieces: config, JSON shapes, logging, error bodies.
//!
//! Nothing here knows about HTTP handlers or tables.

pub mod actor;
pub mod api_key;
pub mod config;
pub mod cookie;
pub mod cors;
pub mod envelope;
pub mod error;
pub mod gzip;
pub mod json;
pub mod logging;
pub mod origin;
pub mod parser;
pub mod python_repr;
pub mod security;
pub mod session;
pub mod throttle;

pub use config::{
    Config, ConfigError, DEFAULT_SETTINGS_MODULE, ENV_SPECS, EnvSource, EnvSpec, ProcessEnv,
    Secret, Sensitivity, TIME_ZONE, is_valid_url, py_int,
};
pub use envelope::ApiError;
pub use error::ErrorBody;
pub use json::{JsonError, JsonResponse, RawNumber, compact_json, spaced_json};
pub use python_repr::{iso8601, iso8601_date, offset_for_timezone, repr_float};
