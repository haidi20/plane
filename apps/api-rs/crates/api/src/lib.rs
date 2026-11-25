// Copyright (c) 2023-present Plane Software, Inc. and contributors
// SPDX-License-Identifier: AGPL-3.0-only
// See the LICENSE file for details.

//! HTTP surface. One module per Django door, so a route diff maps 1:1.

pub mod app;
pub mod errors;
pub mod middleware;
pub mod routes;
pub mod web;

pub use app::router;
pub use middleware::RequestId;
