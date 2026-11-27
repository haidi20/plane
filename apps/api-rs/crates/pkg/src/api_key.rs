// Copyright (c) 2023-present Plane Software, Inc. and contributors
// SPDX-License-Identifier: AGPL-3.0-only
// See the LICENSE file for details.

//! `X-Api-Key`: the header rule, the query predicate, and the status DRF
//! actually renders.
//!
//! Ported from `plane/api/middleware/api_authentication.py` and DRF 3.18.3.
//! Three details are easy to get wrong and are locked by
//! `contracts/golden/auth-cases.json`:
//!
//! 1. **A missing header is not an error.** `authenticate` returns `None`, so
//!    DRF's `IsAuthenticated` answers 401 "Authentication credentials were not
//!    provided.". An **empty** header behaves the same (`if not token`), but a
//!    whitespace-only header does **not**: `"   "` is truthy, so it is looked up
//!    and fails as an invalid token.
//! 2. **An invalid token is 403, not 401.** `APIKeyAuthentication` has no
//!    `authenticate_header`, so DRF's `handle_exception` coerces
//!    `AuthenticationFailed` to 403 and keeps the message.
//! 3. **The error is a `detail`, not Plane's `error` envelope.** The DRF
//!    exception handler in `plane/authentication/adapter/exception.py` only
//!    rewrites `Throttled`, so `{"detail": "Given API token is not valid"}`
//!    is rendered compact.

use uuid::Uuid;

/// Header name, from `APIKeyAuthentication.auth_header_name`.
pub const HEADER: &str = "X-Api-Key";

/// Message raised by `validate_api_token` when the row is missing or unusable.
pub const INVALID_TOKEN: &str = "Given API token is not valid";

/// Message DRF's `IsAuthenticated` produces for an anonymous caller.
pub const NOT_AUTHENTICATED: &str = "Authentication credentials were not provided.";

/// DRF status after coercion: 403 because there is no `WWW-Authenticate`.
pub const INVALID_TOKEN_STATUS: u16 = 403;

/// DRF status for an anonymous caller.
pub const NOT_AUTHENTICATED_STATUS: u16 = 401;

/// What `authenticate` does with the header value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeaderOutcome<'a> {
    /// No header, or an empty one: `authenticate` returns `None`.
    Absent,
    /// A non-empty value, looked up verbatim. Whitespace is **not** trimmed.
    Lookup(&'a str),
}

/// `get_api_token` + the `if not token` guard.
///
/// Django's `request.headers.get("X-Api-Key")` returns the value verbatim: a
/// padded `"  key  "` is looked up with the spaces. Python truthiness makes
/// `""` absent and `"   "` present.
#[must_use]
pub fn header_outcome(value: Option<&str>) -> HeaderOutcome<'_> {
    match value {
        None | Some("") => HeaderOutcome::Absent,
        Some(token) => HeaderOutcome::Lookup(token),
    }
}

/// The `APIToken.objects.get(...)` predicate, as data.
///
/// `expired_at > now OR expired_at IS NULL`, `token = <value>`, `is_active`,
/// and `user__is_active`. It is returned as a value rather than executed so the
/// repository layer (`[DATA] Pool Postgres`) can bind it. `expired_at__gt=now`
/// is strict: a row expiring exactly now is already invalid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenQuery<'a> {
    /// `token` column, compared exactly.
    pub token: &'a str,
    /// `is_active = True`.
    pub is_active: bool,
    /// `user__is_active = True`.
    pub user_is_active: bool,
}

impl<'a> TokenQuery<'a> {
    /// Build the predicate for a non-empty token.
    #[must_use]
    pub fn new(token: &'a str) -> Self {
        Self {
            token,
            is_active: true,
            user_is_active: true,
        }
    }
}

/// One `APIToken` row, reduced to the fields the authenticator reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredToken {
    /// `APIToken.id`, kept for the audit log.
    pub id: Uuid,
    /// `APIToken.token`.
    pub token: String,
    /// `APIToken.is_active`.
    pub is_active: bool,
    /// `APIToken.user_id`.
    pub user_id: Uuid,
    /// `User.is_active`.
    pub user_is_active: bool,
    /// `APIToken.expired_at`, as a unix timestamp in seconds.
    pub expired_at: Option<i64>,
}

impl StoredToken {
    /// Whether this row satisfies the query at `now` (unix seconds).
    ///
    /// `expired_at__gt=now` is strict, and `expired_at__isnull=True` passes.
    #[must_use]
    pub fn is_valid_at(&self, now: i64) -> bool {
        self.is_active && self.user_is_active && self.expired_at.is_none_or(|expired| expired > now)
    }
}

/// The result of `authenticate` for one request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// `authenticate` returned `None`; the permission class answers 401.
    Anonymous,
    /// The lookup failed; DRF renders 403 with [`INVALID_TOKEN`].
    InvalidToken,
    /// A valid row; `last_used` must be updated before the request continues.
    Authenticated {
        /// `APIToken.user_id`.
        user_id: Uuid,
        /// `APIToken.id`.
        token_id: Uuid,
        /// `APIToken.token`, which Django returns as `(user, token)`.
        token: String,
    },
}

impl Outcome {
    /// Status and `detail` for a request that ends here, if it ends here.
    ///
    /// `None` for [`Outcome::Authenticated`], because the handler runs.
    #[must_use]
    pub fn rejection(&self) -> Option<(u16, &'static str)> {
        match self {
            Self::Anonymous => Some((NOT_AUTHENTICATED_STATUS, NOT_AUTHENTICATED)),
            Self::InvalidToken => Some((INVALID_TOKEN_STATUS, INVALID_TOKEN)),
            Self::Authenticated { .. } => None,
        }
    }
}

/// `authenticate` against an already-fetched row set.
///
/// `find` is the repository lookup: it receives the [`TokenQuery`] and returns
/// the matching row, if any. Django's `.get()` raises `DoesNotExist` for both
/// "no row" and "two rows"; the latter is a `MultipleObjectsReturned` crash in
/// production, so a caller must not model it as an invalid token. This function
/// only sees `Option`, so the repository owns that distinction.
#[must_use]
pub fn authenticate<'a, F>(header: Option<&'a str>, mut find: F) -> Outcome
where
    F: FnMut(&TokenQuery<'a>) -> Option<StoredToken>,
{
    match header_outcome(header) {
        HeaderOutcome::Absent => Outcome::Anonymous,
        HeaderOutcome::Lookup(token) => match find(&TokenQuery::new(token)) {
            Some(row) if row.token == token => Outcome::Authenticated {
                user_id: row.user_id,
                token_id: row.id,
                token: row.token,
            },
            _ => Outcome::InvalidToken,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(id: u128) -> StoredToken {
        StoredToken {
            id: Uuid::from_u128(id),
            token: "tok-1".to_string(),
            is_active: true,
            user_id: Uuid::from_u128(100 + id),
            user_is_active: true,
            expired_at: None,
        }
    }

    #[test]
    fn a_missing_or_empty_header_is_anonymous() {
        assert_eq!(header_outcome(None), HeaderOutcome::Absent);
        assert_eq!(header_outcome(Some("")), HeaderOutcome::Absent);
        assert_eq!(header_outcome(Some("   ")), HeaderOutcome::Lookup("   "));
        assert_eq!(
            header_outcome(Some("  key-abc  ")),
            HeaderOutcome::Lookup("  key-abc  ")
        );
        // A repeated header reaches Django comma-joined, as one value.
        assert_eq!(header_outcome(Some("a,b")), HeaderOutcome::Lookup("a,b"));
    }

    #[test]
    fn authenticating_without_a_header_never_touches_the_repository() {
        let mut calls = 0;
        let outcome = authenticate(None, |_| {
            calls += 1;
            Some(token(1))
        });
        assert_eq!(outcome, Outcome::Anonymous);
        assert_eq!(calls, 0, "Django returns before the query");

        let mut calls = 0;
        let outcome = authenticate(Some(""), |_| {
            calls += 1;
            Some(token(1))
        });
        assert_eq!(outcome, Outcome::Anonymous);
        assert_eq!(calls, 0);
    }

    #[test]
    fn a_lookup_miss_is_an_invalid_token() {
        let outcome = authenticate(Some("nope"), |_| None);
        assert_eq!(outcome, Outcome::InvalidToken);
        assert_eq!(
            outcome.rejection(),
            Some((403, "Given API token is not valid"))
        );
    }

    #[test]
    fn whitespace_is_queried_verbatim_and_rejected() {
        let mut seen = None;
        let outcome = authenticate(Some("   "), |query| {
            seen = Some(query.token.to_string());
            None
        });
        assert_eq!(seen.as_deref(), Some("   "));
        assert_eq!(outcome, Outcome::InvalidToken);
    }

    #[test]
    fn a_valid_row_authenticates_and_returns_the_token() {
        let mut seen = None;
        let outcome = authenticate(Some("tok-1"), |query| {
            seen = Some((query.is_active, query.user_is_active));
            Some(token(1))
        });
        assert_eq!(seen, Some((true, true)));
        assert_eq!(
            outcome,
            Outcome::Authenticated {
                user_id: Uuid::from_u128(101),
                token_id: Uuid::from_u128(1),
                token: "tok-1".to_string(),
            }
        );
        assert_eq!(outcome.rejection(), None);
    }

    #[test]
    fn expiry_is_strict_and_null_expiry_never_expires() {
        let mut row = token(1);
        assert!(row.is_valid_at(1_000));
        row.expired_at = Some(1_001);
        assert!(row.is_valid_at(1_000));
        row.expired_at = Some(1_000);
        assert!(!row.is_valid_at(1_000), "expired_at__gt is strict");
        row.expired_at = Some(999);
        assert!(!row.is_valid_at(1_000));
    }

    #[test]
    fn inactive_token_or_inactive_user_is_rejected() {
        let mut row = token(1);
        row.is_active = false;
        assert!(!row.is_valid_at(0));
        row.is_active = true;
        row.user_is_active = false;
        assert!(!row.is_valid_at(0));
    }

    #[test]
    fn a_row_with_a_different_token_is_not_valid() {
        // The repository is expected to filter, but a wrong row must not pass.
        let outcome = authenticate(Some("tok-2"), |_| Some(token(1)));
        assert_eq!(outcome, Outcome::InvalidToken);
    }

    #[test]
    fn anonymous_rejection_is_401_with_drfs_message() {
        assert_eq!(
            Outcome::Anonymous.rejection(),
            Some((401, "Authentication credentials were not provided."))
        );
    }
}
