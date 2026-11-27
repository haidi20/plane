// Copyright (c) 2023-present Plane Software, Inc. and contributors
// SPDX-License-Identifier: AGPL-3.0-only
// See the LICENSE file for details.

//! Repositories and query helpers.
//!
//! The pool itself lands with the `[DATA] Pool Postgres rw/ro` card; this crate
//! exists now so `api` and `worker` never reach into tables directly.

pub mod error;
pub mod page;

pub use error::{RepoError, RepoResult};
pub use page::{Cursor, Page};
