// Copyright (c) 2023-present Plane Software, Inc. and contributors
// SPDX-License-Identifier: AGPL-3.0-only
// See the LICENSE file for details.

//! Business rules shared by HTTP handlers and background jobs.
//!
//! No axum, no sqlx: this crate must stay usable from the worker binary.

pub mod route;

pub use route::{RouteSpec, manifest};
