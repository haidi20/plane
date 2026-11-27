// Copyright (c) 2023-present Plane Software, Inc. and contributors
// SPDX-License-Identifier: AGPL-3.0-only
// See the LICENSE file for details.

//! Repository errors, mapped to DRF responses by the HTTP layer.

use thiserror::Error;

/// Failure modes a repository may return.
#[derive(Debug, Error)]
pub enum RepoError {
    /// Row not found; DRF answers 404 `{"detail": "Not found."}`.
    #[error("not found")]
    NotFound,
    /// Row exists but is soft-deleted.
    #[error("soft deleted")]
    SoftDeleted,
    /// Unique or foreign-key constraint violated.
    #[error("conflict: {0}")]
    Conflict(String),
    /// Driver or pool failure.
    #[error("database error: {0}")]
    Driver(String),
}

/// Repository result alias.
pub type RepoResult<T> = Result<T, RepoError>;
