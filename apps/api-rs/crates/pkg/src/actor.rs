// Copyright (c) 2023-present Plane Software, Inc. and contributors
// SPDX-License-Identifier: AGPL-3.0-only
// See the LICENSE file for details.

//! Who is making the request, and what Django does with each source.
//!
//! Django has no `Actor` class: the identity arrives as `request.user` plus the
//! `crum` thread-local, and the workspace role is looked up per view. The Rust
//! side needs one value to thread through handlers, jobs and repositories
//! (`docs/rust-api.md` 7.2 and 9.4), so this module names the parts and keeps
//! the mapping honest:
//!
//! | Field          | Django source                                              |
//! |----------------|------------------------------------------------------------|
//! | `user_id`      | `request.user.id`                                          |
//! | `api_token_id` | `APIToken` row used by `APIKeyAuthentication`              |
//! | `source`       | which authenticator produced the identity                  |
//! | `workspace`    | URL slug, then `WorkspaceMember`/`Workspace.owner`         |
//! | `role`         | `WorkspaceMember.role`, integer 20/15/5                    |
//!
//! `workspace` and `role` cannot be filled without a database, so they are
//! modelled as optional and never guessed. `role` keeps Django's integers:
//! `ROLE_CHOICES = ((20, "Admin"), (15, "Member"), (5, "Guest"))` in
//! `plane/db/models/workspace.py`, and an unknown value is preserved verbatim
//! instead of being refused, because Django stores a `PositiveSmallIntegerField`
//! and compares it with `role__in=[...]`.

use std::fmt;

/// Which authenticator produced the identity.
///
/// Django has three doors (`docs/rust-api.md` 8.1): the user session cookie on
/// `/api/*`, the API key on `/api/v1/*`, and the admin session on
/// `/api/instances/*`. OAuth flows end in a session, so they are `Session`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AuthSource {
    /// `BaseSessionAuthentication` over `session-id`.
    Session,
    /// `APIKeyAuthentication` over `X-Api-Key`.
    ApiKey,
    /// `BaseSessionAuthentication` over `admin-session-id`.
    AdminSession,
}

impl AuthSource {
    /// The cookie or header this source reads, for logs and tests.
    #[must_use]
    pub const fn credential(self) -> &'static str {
        match self {
            Self::Session => "session-id",
            Self::ApiKey => "X-Api-Key",
            Self::AdminSession => "admin-session-id",
        }
    }

    /// `true` for the two cookie doors, which go through the Origin check.
    #[must_use]
    pub const fn uses_cookie(self) -> bool {
        matches!(self, Self::Session | Self::AdminSession)
    }
}

/// Django's workspace/project role integers.
///
/// Both `WorkspaceMember.role` and `ProjectMember.role` use the same three
/// values. `Custom(u16)` exists so a row that predates a choices change is
/// carried through instead of being dropped: Django itself does not validate
/// on read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Role {
    /// `5`, "Guest".
    Guest,
    /// `15`, "Member".
    Member,
    /// `20`, "Admin".
    Admin,
    /// A stored value outside the current choices.
    Custom(u16),
}

impl Role {
    /// The integer Django stores.
    #[must_use]
    pub const fn value(self) -> u16 {
        match self {
            Self::Guest => 5,
            Self::Member => 15,
            Self::Admin => 20,
            Self::Custom(value) => value,
        }
    }

    /// Map a stored integer; never fails, like reading the column.
    #[must_use]
    pub const fn from_value(value: u16) -> Self {
        match value {
            5 => Self::Guest,
            15 => Self::Member,
            20 => Self::Admin,
            other => Self::Custom(other),
        }
    }

    /// The label in `ROLE_CHOICES`.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Guest => "Guest",
            Self::Member => "Member",
            Self::Admin => "Admin",
            Self::Custom(_) => "Unknown",
        }
    }
}

impl fmt::Display for Role {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

/// The workspace a request names, plus the caller's role in it.
///
/// Both fields come from the URL slug plus a membership row; neither is implied
/// by the cookie. A request that never names a workspace has `None` here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceContext {
    /// `Workspace.slug`, as it appeared in the URL.
    pub slug: String,
    /// `WorkspaceMember.role`; `None` when the caller is the owner without a
    /// membership row, which Plane's create path does not produce but a
    /// pre-existing row can have.
    pub role: Option<Role>,
}

impl WorkspaceContext {
    /// Build from the URL slug and the stored role.
    #[must_use]
    pub fn new(slug: impl Into<String>, role: Option<Role>) -> Self {
        Self {
            slug: slug.into(),
            role,
        }
    }
}

/// One authenticated caller.
///
/// `api_token_id` is set only for `AuthSource::ApiKey`; Django returns the raw
/// token string from `APIKeyAuthentication.authenticate`, and the row id is what
/// an audit trail needs. Keeping it optional means "no API key" is representable
/// without a sentinel UUID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Actor {
    /// `request.user.id`.
    pub user_id: uuid::Uuid,
    /// `APIToken.id` behind the `X-Api-Key` header, when there is one.
    pub api_token_id: Option<uuid::Uuid>,
    /// Which door authenticated this request.
    pub source: AuthSource,
    /// Workspace from the URL plus the caller's role, when known.
    pub workspace: Option<WorkspaceContext>,
}

impl Actor {
    /// A session-authenticated actor with no workspace yet.
    #[must_use]
    pub fn session(user_id: uuid::Uuid) -> Self {
        Self {
            user_id,
            api_token_id: None,
            source: AuthSource::Session,
            workspace: None,
        }
    }

    /// An API-key actor. The token id is the row, not the secret.
    #[must_use]
    pub fn api_key(user_id: uuid::Uuid, api_token_id: uuid::Uuid) -> Self {
        Self {
            user_id,
            api_token_id: Some(api_token_id),
            source: AuthSource::ApiKey,
            workspace: None,
        }
    }

    /// Attach the workspace and role resolved from the URL.
    #[must_use]
    pub fn with_workspace(mut self, workspace: WorkspaceContext) -> Self {
        self.workspace = Some(workspace);
        self
    }

    /// `true` when the caller may act as an admin in the current workspace.
    ///
    /// Mirrors `allow_permission(..., level="WORKSPACE")` with `ROLE.ADMIN`,
    /// which is the only role compared with `role=ROLE.ADMIN.value` in
    /// `plane/app/permissions/base.py`. A missing workspace or role denies.
    #[must_use]
    pub fn is_workspace_admin(&self) -> bool {
        self.workspace.as_ref().and_then(|workspace| workspace.role) == Some(Role::Admin)
    }

    /// `true` when the caller has at least the given member-level role.
    ///
    /// Django compares against an explicit `role__in=[...]` list, and the three
    /// values are ordered, so the comparison is equivalent for the current
    /// choices.
    #[must_use]
    pub fn has_role_at_least(&self, role: Role) -> bool {
        self.workspace
            .as_ref()
            .and_then(|workspace| workspace.role)
            .is_some_and(|current| current.value() >= role.value())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn workspace_user() -> Uuid {
        Uuid::from_u128(7)
    }

    #[test]
    fn roles_keep_djangos_integers() {
        assert_eq!(Role::Admin.value(), 20);
        assert_eq!(Role::Member.value(), 15);
        assert_eq!(Role::Guest.value(), 5);
        assert_eq!(Role::from_value(20), Role::Admin);
        assert_eq!(Role::from_value(15), Role::Member);
        assert_eq!(Role::from_value(5), Role::Guest);
        assert_eq!(Role::from_value(0), Role::Custom(0));
        assert_eq!(Role::from_value(99), Role::Custom(99));
        assert_eq!(Role::Custom(99).value(), 99);
        assert_eq!(Role::Custom(99).label(), "Unknown");
        assert_eq!(Role::Admin.to_string(), "Admin");
    }

    #[test]
    fn sources_name_the_credential_django_reads() {
        assert_eq!(AuthSource::Session.credential(), "session-id");
        assert_eq!(AuthSource::ApiKey.credential(), "X-Api-Key");
        assert_eq!(AuthSource::AdminSession.credential(), "admin-session-id");
        assert!(AuthSource::Session.uses_cookie());
        assert!(AuthSource::AdminSession.uses_cookie());
        assert!(!AuthSource::ApiKey.uses_cookie());
    }

    #[test]
    fn a_session_actor_starts_without_workspace_or_token() {
        let actor = Actor::session(workspace_user());
        assert_eq!(actor.user_id, workspace_user());
        assert_eq!(actor.api_token_id, None);
        assert_eq!(actor.source, AuthSource::Session);
        assert_eq!(actor.workspace, None);
        assert!(!actor.is_workspace_admin());
        assert!(!actor.has_role_at_least(Role::Guest));
    }

    #[test]
    fn an_api_key_actor_carries_the_row_not_the_secret() {
        let token = Uuid::from_u128(9);
        let actor = Actor::api_key(workspace_user(), token);
        assert_eq!(actor.api_token_id, Some(token));
        assert_eq!(actor.source, AuthSource::ApiKey);
    }

    #[test]
    fn workspace_role_answers_admin_and_member_checks() {
        let admin = Actor::session(workspace_user())
            .with_workspace(WorkspaceContext::new("acme", Some(Role::Admin)));
        assert!(admin.is_workspace_admin());
        assert!(admin.has_role_at_least(Role::Member));
        assert_eq!(admin.workspace.as_ref().expect("workspace").slug, "acme");

        let guest = Actor::session(workspace_user())
            .with_workspace(WorkspaceContext::new("acme", Some(Role::Guest)));
        assert!(!guest.is_workspace_admin());
        assert!(!guest.has_role_at_least(Role::Member));
        assert!(guest.has_role_at_least(Role::Guest));

        // A row without a role is not an admin and not a member.
        let unknown =
            Actor::session(workspace_user()).with_workspace(WorkspaceContext::new("acme", None));
        assert!(!unknown.is_workspace_admin());
        assert!(!unknown.has_role_at_least(Role::Guest));
    }

    #[test]
    fn a_custom_role_compares_by_its_stored_value() {
        let actor = Actor::session(workspace_user())
            .with_workspace(WorkspaceContext::new("acme", Some(Role::Custom(25))));
        assert!(actor.has_role_at_least(Role::Admin));
        assert!(!actor.is_workspace_admin());
    }
}
