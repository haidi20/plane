// Copyright (c) 2023-present Plane Software, Inc. and contributors
// SPDX-License-Identifier: AGPL-3.0-only
// See the LICENSE file for details.

//! Cursor pagination shared by list endpoints.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Opaque cursor: the last row\'s `(created_at, id)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    /// ISO-8601 timestamp of the last row on the previous page.
    pub created_at: String,
    /// Tie-breaker so equal timestamps never repeat or skip a row.
    pub id: Uuid,
}

/// One page of rows plus the cursor for the next request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Page<T> {
    /// Rows in Django order.
    pub results: Vec<T>,
    /// `None` on the last page.
    pub next: Option<Cursor>,
    /// Total rows, matching DRF\'s `count`.
    pub count: u64,
}

impl<T> Page<T> {
    /// Build a page, deriving `next` from the last row.
    #[must_use]
    pub fn new(results: Vec<T>, count: u64, next: Option<Cursor>) -> Self {
        Self {
            results,
            next,
            count,
        }
    }
}
