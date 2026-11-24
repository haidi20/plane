// Copyright (c) 2023-present Plane Software, Inc. and contributors
// SPDX-License-Identifier: AGPL-3.0-only
// See the LICENSE file for details.

//! Cross-cutting pieces: config, JSON shapes, logging.
//!
//! Nothing here knows about HTTP handlers or tables.

pub mod config;
pub mod json;
pub mod logging;

pub use config::Config;
pub use json::{JsonResponse, compact_json, spaced_json};
