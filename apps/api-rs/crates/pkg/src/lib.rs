// Copyright (c) 2023-present Plane Software, Inc. and contributors
// SPDX-License-Identifier: AGPL-3.0-only
// See the LICENSE file for details.

//! Cross-cutting pieces: config, JSON shapes, logging, error bodies.
//!
//! Nothing here knows about HTTP handlers or tables.

pub mod config;
pub mod envelope;
pub mod error;
pub mod json;
pub mod logging;
pub mod python_repr;

pub use config::Config;
pub use envelope::ApiError;
pub use error::ErrorBody;
pub use json::{JsonError, JsonResponse, compact_json, spaced_json};
pub use python_repr::{iso8601, iso8601_date, offset_for_timezone, repr_float};
