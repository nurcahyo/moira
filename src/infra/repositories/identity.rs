//! Admin identity grant persistence (plan 07, module 5).
//!
//! Backs `migrations/0012_admin_identity_claims.sql`: the `admin_identities` grant table
//! and the `setup_state` singleton.
//!
//! # Trait from day one
//!
//! Unlike the repositories P2-3 had to retrofit, this one ships as a trait plus a single
//! Postgres implementation from its first commit, so a later plan never has to re-shape a
//! surface that already has callers.
//!
//! # Decision D1: no setup-token methods
//!
//! The one-time setup-token credential path is deferred (plan 07 §0.2 D1), so there is no
//! `admin_setup_tokens` table and consequently no `insert_setup_token` /
//! `consume_setup_token` here. The system-key path is the only credential path, and it
//! needs no network dependency, which is what keeps air-gapped operation viable.

use async_trait::async_trait;
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use sqlx::{PgConnection, PgPool, Row};
use uuid::Uuid;

use crate::{
    domain::{AdminIdentityStatus, AdminInviteConstraint, AdminInviteStatus, ListCursor},
    error::AppError,
};

/// One row of `admin_identities`.
///
/// Deliberately not [`crate::domain::AdminIdentityRecord`]: that type carries the
/// `notice` i18n envelope, which is a presentation concern the repository has no business
/// constructing. The application layer maps this into it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminIdentityGrant {
    pub id: Uuid,
    pub trusted_jwt_issuer_id: Uuid,
    pub issuer: String,
    pub subject: String,
    /// Nullable in the schema so a future anonymisation path can clear it; every grant
    /// *this plan* writes has one, enforced at the service.
    pub email: Option<String>,
    pub email_verified: bool,
    pub granted_scopes: Vec<String>,
    /// Ownership as row state (plan 09 decision D1). Read on every ownership mutation
    /// to answer "may this caller manage other admins", a question a scope could not
    /// answer because `moira:admin` implies every scope for a trusted-JWT actor.
    pub is_primary: bool,
    pub status: AdminIdentityStatus,
    pub created_at: DateTime<Utc>,
    pub version: i64,
}

/// Everything `insert_grant` needs, gathered into one struct so the call site reads as a
/// record rather than as nine positional arguments.
#[derive(Debug, Clone)]
pub struct AdminIdentityGrantInsert {
    pub id: Uuid,
    pub trusted_jwt_issuer_id: Uuid,
    /// Denormalised beside the FK so the module-7 hot-path lookup needs no join.
    pub issuer: String,
    pub subject: String,
    pub email: String,
    pub email_verified: bool,
    pub granted_scopes: Vec<String>,
    /// `'system_key'`, `'admin_invite'` or `'setup_token'` — recorded honestly, and the
    /// CHECK in `migrations/0018` is the enforcement.
    ///
    /// The claim path writes `'system_key'` and the redeem path writes `'admin_invite'`.
    /// They must stay distinguishable: `'system_key'` means the bootstrap break-glass
    /// credential was presented, which is an event a deployment alerts on, so an invite
    /// borrowing that value would raise the alarm on every routine onboarding *and* hide
    /// the real thing among them. `'setup_token'` stays legal but unwritten under D1.
    pub granted_by_actor_type: String,
    pub granted_by_subject: Option<String>,
}

/// One row of `admin_invites`, with **no** secret material in it.
///
/// The token, its Argon2id hash, its prefix and its fingerprint are all deliberately
/// absent: this is the shape a list, a get, an idempotent replay and the audit metadata
/// all share, and the only way to keep the token out of every one of them is for the
/// type they are built from not to carry it. [`AdminInviteCandidate`] is the one place
/// the hash is read, and it exists solely to verify a presented token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminInviteRow {
    pub id: Uuid,
    pub constraint: AdminInviteConstraint,
    pub value: String,
    pub status: AdminInviteStatus,
    pub expires_at: DateTime<Utc>,
    pub created_by_subject: Option<String>,
    pub consumed_at: Option<DateTime<Utc>>,
    pub consumed_subject: Option<String>,
    pub created_at: DateTime<Utc>,
    pub version: i64,
}

impl AdminInviteRow {
    /// Expiry is **derived**, never stored: `admin_invites.status` has no `'expired'`
    /// value because nothing sweeps for one, so a status column claiming otherwise would
    /// be a fact no code maintains.
    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        self.expires_at <= now
    }
}

/// A prefix-matched invite plus the hash needed to verify a presented token.
///
/// Separate from [`AdminInviteRow`] so that reading the hash is a distinct, greppable
/// act rather than a field that travels everywhere the record does.
#[derive(Debug, Clone)]
pub struct AdminInviteCandidate {
    pub record: AdminInviteRow,
    pub token_hash: String,
}

/// Everything `insert_invite` needs, gathered so the call site reads as a record.
#[derive(Debug, Clone)]
pub struct AdminInviteInsert {
    pub id: Uuid,
    pub token_prefix: String,
    pub token_hash: String,
    pub fingerprint: String,
    pub pepper_version: String,
    pub constraint: AdminInviteConstraint,
    pub value: String,
    pub created_by_issuer: Option<String>,
    pub created_by_subject: Option<String>,
    /// `'system_key'`, `'trusted_jwt'` or `'dev_admin'` — recorded honestly, so an audit
    /// can tell a break-glass invite from one an existing admin issued.
    pub created_by_actor_type: String,
    pub expires_at: DateTime<Utc>,
}

/// Serialises every ownership mutation deployment-wide.
///
/// The last-primary guard is a *set* predicate ("is there another active primary?"), so
/// two concurrent clears of two different primaries could each observe the other and
/// both succeed, leaving zero primaries — the exact lockout the guard exists to prevent.
/// Row-level `for update` cannot close it without a lock ordering that deadlocks under
/// the symmetric case, so a single transaction-scoped advisory lock is taken instead.
/// Ownership transfers are rare operator actions; serialising them costs nothing.
const OWNERSHIP_LOCK_KEY: i64 = i64::from_be_bytes(*b"moiraown");

#[async_trait]
pub trait AdminIdentityRepository: Send + Sync {
    /// Whether any admin identity has **ever** been claimed.
    ///
    /// Read independently of `admin_identities.status`, so revoking the only admin cannot
    /// silently reopen the unauthenticated land-grab window.
    async fn setup_claimed(&self) -> Result<bool, AppError>;

    /// The grant lookup for a `(issuer, subject)` pair, backed by
    /// `admin_identities_lookup_idx`.
    ///
    /// # This is a hot path and a plain pool read, deliberately
    ///
    /// It runs on admin requests authenticated by a trusted JWT, so it is a single
    /// `fetch_optional` against the pool — never a transaction, never an advisory lock.
    ///
    /// # Call it from `authenticate_admin`, not from `authenticate_trusted_jwt`
    ///
    /// Plan 07 §0.2 **D2**. `authenticate_admin` and `authenticate_caller` both delegate to
    /// `authenticate_trusted_jwt`, and `authenticate_caller` returns that actor verbatim to
    /// the public execution API. Unioning `moira:admin` onto the actor inside the shared
    /// function would therefore put admin authority on `POST /api/v1/responses`, where —
    /// combined with admin implication — it satisfies
    /// `moira:execution:override-credential`, `override-model` and
    /// `moira:identity:delegate`. Admin-identity grants apply on the **admin plane only**;
    /// this method takes a pool and two strings precisely so it can be called from the
    /// narrow call site rather than from the shared one.
    async fn find_active_grant(
        &self,
        issuer: &str,
        subject: &str,
    ) -> Result<Option<AdminIdentityGrant>, AppError>;

    /// Resolves a claim's target issuer to its `trusted_jwt_issuers` row.
    ///
    /// Moira never accepts a free-text issuer at claim time: an issuer with no active,
    /// registered row is `400 unregistered_trusted_issuer`.
    async fn resolve_active_issuer(&self, issuer: &str) -> Result<Uuid, AppError>;

    /// Inserts the grant **inside the caller's transaction**.
    ///
    /// Takes a `&mut PgConnection` rather than using the pool so it can run under
    /// `AdminCommandRunner`'s idempotency envelope, which hands the mutation closure a
    /// `PgAdminCommandTransaction` and exposes its connection. Writing through the pool
    /// here would put the grant outside the savepoint that makes a failed command
    /// leave no trace.
    ///
    /// # It also decides ownership (plan 09 finding F20, decision D-F20)
    ///
    /// The grant becomes primary **iff the deployment has no active primary**, so the
    /// first admin a deployment ever gets is its owner and every later one is not. Before
    /// `0019` nothing outside `0017`'s one-shot migration-time backfill ever wrote
    /// `is_primary`, and that backfill runs against an empty table on a greenfield
    /// deployment — so ownership was permanently unreachable there, and with it the whole
    /// transfer endpoint, unless an operator reached for the break-glass system key.
    ///
    /// This is why the method takes the ownership lock; see [`take_ownership_lock`].
    async fn insert_grant(
        &self,
        conn: &mut PgConnection,
        insert: &AdminIdentityGrantInsert,
    ) -> Result<AdminIdentityGrant, AppError>;

    /// Flips the `setup_state` singleton, in the caller's transaction.
    ///
    /// The `and claimed = false` guard makes this self-idempotent: a second call is a
    /// zero-row update, not an error and not a rewrite of the first claimant.
    async fn mark_setup_claimed(
        &self,
        conn: &mut PgConnection,
        admin_identity_id: Uuid,
    ) -> Result<(), AppError>;

    // -------------------------------------------------------------------------------
    // Plan 09 wave 2 — grant administration (plan 07 deferred these three).
    // -------------------------------------------------------------------------------

    /// Descending `(created_at, id)` keyset over live grants, over-fetching by one — the
    /// same contract every other admin list follows. Revoked grants are **included**:
    /// "who used to hold admin" is exactly what an operator auditing an incident needs,
    /// and hiding them would make the list disagree with the audit log.
    async fn list_grants(
        &self,
        cursor: Option<ListCursor>,
        limit: i64,
    ) -> Result<Vec<AdminIdentityGrant>, AppError>;

    /// Sets or clears ownership, in the caller's transaction, under the ownership lock.
    ///
    /// Returns `admin_identity_last_primary` rather than succeeding when clearing the
    /// flag would leave zero active primaries — the lockout guard, expressible as a
    /// query only because ownership is row state (decision D1).
    ///
    /// **Setting it is a transfer, not an addition** (decision D-F20): the incumbent is
    /// demoted in the same transaction, so ownership moves rather than accumulating.
    /// Under `admin_identities_single_active_primary` that is the only behaviour the
    /// schema admits.
    async fn set_primary(
        &self,
        conn: &mut PgConnection,
        id: Uuid,
        expected_version: i64,
        is_primary: bool,
    ) -> Result<AdminIdentityGrant, AppError>;

    /// **Soft** revoke: `status = 'revoked'`, `revoked_at = now()`. Never a row delete —
    /// the grant is audit history, and the `(issuer, subject)` uniqueness key must keep
    /// blocking a silent re-grant.
    ///
    /// Deliberately does **not** touch `setup_state.claimed`: setup-required is a
    /// one-way transition (plan 07), so revoking the last admin leaves system-key
    /// break-glass as the re-entry path rather than reopening the land-grab window.
    ///
    /// # Consequence of decision D-F20 worth knowing before you call it
    ///
    /// Revoking the **owner** is refused with `admin_identity_last_primary`, because a
    /// revocation clears `is_primary` and the guard does not care which statement is
    /// doing the clearing. Since the first grant on a deployment is now its owner, that
    /// makes a deployment's *sole* admin non-revocable through this path: transfer
    /// ownership to someone else first, then revoke. That is the lockout guard working as
    /// specified, not an oversight — an admin plane with no owner is precisely the state
    /// finding F20 describes.
    async fn revoke_grant(
        &self,
        conn: &mut PgConnection,
        id: Uuid,
    ) -> Result<AdminIdentityGrant, AppError>;

    // -------------------------------------------------------------------------------
    // Plan 09 wave 2 — invitations.
    // -------------------------------------------------------------------------------

    async fn insert_invite(
        &self,
        conn: &mut PgConnection,
        insert: &AdminInviteInsert,
    ) -> Result<AdminInviteRow, AppError>;

    /// The prefix half of prefix-lookup-then-verify.
    ///
    /// A pool read, and the **only** query that returns a token hash. Callers must feed
    /// the result to `ApiKeyHasher::verify`; a prefix match alone proves nothing, since
    /// the prefix is a plaintext substring of the token.
    ///
    /// This is also what bounds the anonymous preview endpoint's cost: a caller with no
    /// valid prefix gets `Ok(None)` without any Argon2 work.
    async fn find_invite_by_prefix(
        &self,
        token_prefix: &str,
    ) -> Result<Option<AdminInviteCandidate>, AppError>;

    async fn list_invites(
        &self,
        cursor: Option<ListCursor>,
        limit: i64,
    ) -> Result<Vec<AdminInviteRow>, AppError>;

    async fn get_invite(&self, id: Uuid) -> Result<AdminInviteRow, AppError>;

    async fn revoke_invite(
        &self,
        conn: &mut PgConnection,
        id: Uuid,
    ) -> Result<AdminInviteRow, AppError>;

    /// The single-winner gate: `select … for update`, re-check, then a conditional
    /// update, all inside the caller's transaction.
    ///
    /// The re-check is **not** redundant with the service's pre-envelope validation.
    /// That validation exists so a *policy* rejection never consumes the invite; this
    /// one exists so two simultaneous redemptions of the same valid invite produce
    /// exactly one grant. Removing either reintroduces a different bug.
    async fn consume_invite(
        &self,
        conn: &mut PgConnection,
        id: Uuid,
        consumed_issuer: &str,
        consumed_subject: &str,
        admin_identity_id: Uuid,
    ) -> Result<AdminInviteRow, AppError>;
}

#[derive(Debug, Clone)]
pub struct PgAdminIdentityRepository {
    pool: PgPool,
}

impl PgAdminIdentityRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

const GRANT_COLUMNS: &str = "id, trusted_jwt_issuer_id, issuer, subject, email, email_verified, \
                             granted_scopes, is_primary, status, created_at, version";

/// Every column of `admin_invites` that leaves this module, and deliberately none of the
/// four secret-storage columns. [`INVITE_CANDIDATE_COLUMNS`] is the single exception.
const INVITE_COLUMNS: &str = "id, email_constraint, domain_constraint, status, expires_at, \
                              created_by_subject, consumed_at, consumed_subject, created_at, \
                              version";

/// [`INVITE_COLUMNS`] plus the Argon2id hash, used by exactly one query.
const INVITE_CANDIDATE_COLUMNS: &str = "id, email_constraint, domain_constraint, status, \
                                        expires_at, created_by_subject, consumed_at, \
                                        consumed_subject, created_at, version, token_hash";

#[async_trait]
impl AdminIdentityRepository for PgAdminIdentityRepository {
    async fn setup_claimed(&self) -> Result<bool, AppError> {
        let claimed = sqlx::query_scalar::<_, bool>("select claimed from setup_state where id")
            .fetch_optional(&self.pool)
            .await?;
        // `0012` seeds the row, so `None` cannot happen on a migrated database. Reading it
        // as "not claimed" rather than erroring keeps the unauthenticated status endpoint
        // from turning a missing singleton into a 500 the caller can do nothing about.
        Ok(claimed.unwrap_or(false))
    }

    async fn find_active_grant(
        &self,
        issuer: &str,
        subject: &str,
    ) -> Result<Option<AdminIdentityGrant>, AppError> {
        let row = sqlx::query(&format!(
            "select {GRANT_COLUMNS} from admin_identities \
             where issuer = $1 and subject = $2 and deleted_at is null and status = 'active'"
        ))
        .bind(issuer)
        .bind(subject)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(grant_from_row).transpose()
    }

    async fn resolve_active_issuer(&self, issuer: &str) -> Result<Uuid, AppError> {
        sqlx::query_scalar::<_, Uuid>(
            "select id from trusted_jwt_issuers \
             where issuer = $1 and status = 'active' and deleted_at is null",
        )
        .bind(issuer)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| {
            // `AppError::coded`, never `AppError::BadRequest`: the latter derives the
            // generic `bad_request` code and would drop the specific key plan 08 binds to.
            AppError::coded(
                StatusCode::BAD_REQUEST,
                "unregistered_trusted_issuer",
                "the target issuer is not a registered, active trusted JWT issuer",
            )
        })
    }

    async fn insert_grant(
        &self,
        conn: &mut PgConnection,
        insert: &AdminIdentityGrantInsert,
    ) -> Result<AdminIdentityGrant, AppError> {
        // This statement *decides ownership*, so it takes the ownership lock like every
        // other statement that does. Without it the `not exists` below and the insert
        // that depends on it are one statement in READ COMMITTED but two facts in time:
        // two claims arriving together would each observe no owner, each decide they are
        // first, and the second would be refused by
        // `admin_identities_single_active_primary` — turning a perfectly legitimate
        // second grant into a `500`. Under the lock the loser simply sees the winner's
        // committed row and is *not primary*, which is the outcome that belongs to a race
        // for ownership. The index stays as the backstop, not as the mechanism.
        take_ownership_lock(conn).await?;
        let row = sqlx::query(&format!(
            r#"
            insert into admin_identities
                (id, trusted_jwt_issuer_id, issuer, subject, email, email_verified,
                 granted_scopes, granted_by_actor_type, granted_by_subject, is_primary)
            values (
                $1, $2, $3, $4, $5, $6, $7, $8, $9,
                -- Decision D-F20. Computed in SQL rather than read into Rust and passed
                -- back down, so there is no window between asking the question and acting
                -- on the answer even if a future caller forgets the lock above.
                not exists (
                    select 1 from admin_identities existing
                    where existing.deleted_at is null
                      and existing.status = 'active'
                      and existing.is_primary
                )
            )
            returning {GRANT_COLUMNS}
            "#
        ))
        .bind(insert.id)
        .bind(insert.trusted_jwt_issuer_id)
        .bind(&insert.issuer)
        .bind(&insert.subject)
        .bind(&insert.email)
        .bind(insert.email_verified)
        .bind(&insert.granted_scopes)
        .bind(&insert.granted_by_actor_type)
        .bind(&insert.granted_by_subject)
        .fetch_one(conn)
        .await
        .map_err(already_claimed_on_unique_violation)?;
        grant_from_row(&row)
    }

    async fn mark_setup_claimed(
        &self,
        conn: &mut PgConnection,
        admin_identity_id: Uuid,
    ) -> Result<(), AppError> {
        sqlx::query(
            "update setup_state \
             set claimed = true, claimed_admin_identity_id = $1, claimed_at = now(), \
                 updated_at = now() \
             where id and claimed = false",
        )
        .bind(admin_identity_id)
        .execute(conn)
        .await?;
        Ok(())
    }

    async fn list_grants(
        &self,
        cursor: Option<ListCursor>,
        limit: i64,
    ) -> Result<Vec<AdminIdentityGrant>, AppError> {
        let (keyset, limit_param) = match cursor {
            Some(_) => ("and (created_at, id) < ($1::timestamptz, $2::uuid)", "$3"),
            None => ("", "$1"),
        };
        let sql = format!(
            "select {GRANT_COLUMNS} from admin_identities \
             where deleted_at is null {keyset} \
             order by created_at desc, id desc limit {limit_param}"
        );
        let query = sqlx::query(&sql);
        let query = match cursor {
            Some(cursor) => query.bind(cursor.ts).bind(cursor.id),
            None => query,
        };
        let rows = query
            .bind(limit.saturating_add(1))
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(grant_from_row).collect()
    }

    async fn set_primary(
        &self,
        conn: &mut PgConnection,
        id: Uuid,
        expected_version: i64,
        is_primary: bool,
    ) -> Result<AdminIdentityGrant, AppError> {
        take_ownership_lock(conn).await?;
        let current = lock_grant(conn, id).await?;
        if current.status != AdminIdentityStatus::Active {
            // Promoting a revoked identity would create an owner who cannot authenticate;
            // demoting one is a no-op dressed as an action. Both are the same conflict.
            return Err(already_revoked(id));
        }
        if current.version != expected_version {
            return Err(version_conflict());
        }
        if current.is_primary && !is_primary {
            require_another_active_primary(conn, id).await?;
        }
        if is_primary && !current.is_primary {
            // Decision D-F20: a transfer **moves** the flag. It is not a choice between
            // two reasonable behaviours any more —
            // `admin_identities_single_active_primary` refuses a promotion that leaves
            // the incumbent in place, so "set" would simply be a `500`. Moving it is also
            // the honest reading of the endpoint's name: an operation that could
            // accumulate owners is not a transfer.
            demote_active_primaries_other_than(conn, id).await?;
        }

        let row = sqlx::query(&format!(
            "update admin_identities set is_primary = $2 \
             where id = $1 and deleted_at is null and version = $3 \
             returning {GRANT_COLUMNS}"
        ))
        .bind(id)
        .bind(is_primary)
        .bind(expected_version)
        .fetch_optional(conn)
        .await?
        .ok_or_else(version_conflict)?;
        grant_from_row(&row)
    }

    async fn revoke_grant(
        &self,
        conn: &mut PgConnection,
        id: Uuid,
    ) -> Result<AdminIdentityGrant, AppError> {
        take_ownership_lock(conn).await?;
        let current = lock_grant(conn, id).await?;
        if current.status != AdminIdentityStatus::Active {
            // The pinned emitter for `admin_identity_already_revoked` (plan 09 §0.5): a
            // repeat revoke under a fresh `Idempotency-Key` is exactly this path. A `204`
            // here would leave the code with no emitter at all, which §0.5 says is a
            // reason to drop it rather than to keep it uncovered.
            return Err(already_revoked(id));
        }
        if current.is_primary {
            require_another_active_primary(conn, id).await?;
        }

        let row = sqlx::query(&format!(
            "update admin_identities \
             set status = 'revoked', revoked_at = now(), is_primary = false \
             where id = $1 and deleted_at is null and status = 'active' \
             returning {GRANT_COLUMNS}"
        ))
        .bind(id)
        .fetch_optional(conn)
        .await?
        .ok_or_else(|| already_revoked(id))?;
        grant_from_row(&row)
    }

    async fn insert_invite(
        &self,
        conn: &mut PgConnection,
        insert: &AdminInviteInsert,
    ) -> Result<AdminInviteRow, AppError> {
        let (email_constraint, domain_constraint) = match insert.constraint {
            AdminInviteConstraint::Email => (Some(insert.value.as_str()), None),
            AdminInviteConstraint::Domain => (None, Some(insert.value.as_str())),
        };
        let row = sqlx::query(&format!(
            r#"
            insert into admin_invites
                (id, token_prefix, token_hash, fingerprint, pepper_version,
                 email_constraint, domain_constraint, created_by_issuer, created_by_subject,
                 created_by_actor_type, expires_at)
            values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
            returning {INVITE_COLUMNS}
            "#
        ))
        .bind(insert.id)
        .bind(&insert.token_prefix)
        .bind(&insert.token_hash)
        .bind(&insert.fingerprint)
        .bind(&insert.pepper_version)
        .bind(email_constraint)
        .bind(domain_constraint)
        .bind(&insert.created_by_issuer)
        .bind(&insert.created_by_subject)
        .bind(&insert.created_by_actor_type)
        .bind(insert.expires_at)
        .fetch_one(conn)
        .await?;
        invite_from_row(&row)
    }

    async fn find_invite_by_prefix(
        &self,
        token_prefix: &str,
    ) -> Result<Option<AdminInviteCandidate>, AppError> {
        let row = sqlx::query(&format!(
            "select {INVITE_CANDIDATE_COLUMNS} from admin_invites \
             where token_prefix = $1 and deleted_at is null"
        ))
        .bind(token_prefix)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref()
            .map(|row| {
                Ok(AdminInviteCandidate {
                    record: invite_from_row(row)?,
                    token_hash: row.try_get("token_hash")?,
                })
            })
            .transpose()
    }

    async fn list_invites(
        &self,
        cursor: Option<ListCursor>,
        limit: i64,
    ) -> Result<Vec<AdminInviteRow>, AppError> {
        let (keyset, limit_param) = match cursor {
            Some(_) => ("and (created_at, id) < ($1::timestamptz, $2::uuid)", "$3"),
            None => ("", "$1"),
        };
        let sql = format!(
            "select {INVITE_COLUMNS} from admin_invites \
             where deleted_at is null {keyset} \
             order by created_at desc, id desc limit {limit_param}"
        );
        let query = sqlx::query(&sql);
        let query = match cursor {
            Some(cursor) => query.bind(cursor.ts).bind(cursor.id),
            None => query,
        };
        let rows = query
            .bind(limit.saturating_add(1))
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(invite_from_row).collect()
    }

    async fn get_invite(&self, id: Uuid) -> Result<AdminInviteRow, AppError> {
        let row = sqlx::query(&format!(
            "select {INVITE_COLUMNS} from admin_invites where id = $1 and deleted_at is null"
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(invite_not_found)?;
        invite_from_row(&row)
    }

    async fn revoke_invite(
        &self,
        conn: &mut PgConnection,
        id: Uuid,
    ) -> Result<AdminInviteRow, AppError> {
        let row = sqlx::query(&format!(
            "update admin_invites set status = 'revoked', revoked_at = now() \
             where id = $1 and deleted_at is null and status = 'pending' \
             returning {INVITE_COLUMNS}"
        ))
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?;
        match row {
            Some(row) => invite_from_row(&row),
            // Distinguish "no such invite" from "already consumed/revoked" rather than
            // collapsing both onto 404, because the operator's next action differs.
            None => {
                let existing = sqlx::query_scalar::<_, String>(
                    "select status from admin_invites where id = $1 and deleted_at is null",
                )
                .bind(id)
                .fetch_optional(conn)
                .await?;
                Err(match existing.as_deref() {
                    Some("consumed") => already_consumed(),
                    Some("revoked") => invite_revoked(),
                    _ => invite_not_found(),
                })
            }
        }
    }

    async fn consume_invite(
        &self,
        conn: &mut PgConnection,
        id: Uuid,
        consumed_issuer: &str,
        consumed_subject: &str,
        admin_identity_id: Uuid,
    ) -> Result<AdminInviteRow, AppError> {
        let locked = sqlx::query(&format!(
            "select {INVITE_COLUMNS} from admin_invites \
             where id = $1 and deleted_at is null for update"
        ))
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(invite_not_found)?;
        let locked = invite_from_row(&locked)?;
        match locked.status {
            AdminInviteStatus::Consumed => return Err(already_consumed()),
            AdminInviteStatus::Revoked => return Err(invite_revoked()),
            AdminInviteStatus::Pending => {}
        }
        if locked.is_expired(Utc::now()) {
            return Err(invite_expired());
        }

        let row = sqlx::query(&format!(
            "update admin_invites \
             set status = 'consumed', consumed_at = now(), consumed_issuer = $2, \
                 consumed_subject = $3, consumed_admin_identity_id = $4 \
             where id = $1 and deleted_at is null and status = 'pending' \
             returning {INVITE_COLUMNS}"
        ))
        .bind(id)
        .bind(consumed_issuer)
        .bind(consumed_subject)
        .bind(admin_identity_id)
        .fetch_optional(conn)
        .await?
        .ok_or_else(already_consumed)?;
        invite_from_row(&row)
    }
}

/// Serialises every ownership decision; see [`OWNERSHIP_LOCK_KEY`].
async fn take_ownership_lock(conn: &mut PgConnection) -> Result<(), AppError> {
    sqlx::query("select pg_advisory_xact_lock($1)")
        .bind(OWNERSHIP_LOCK_KEY)
        .execute(conn)
        .await?;
    Ok(())
}

/// Reads the target grant under `for update`, so the version check and the
/// last-primary count both see a row nobody else can move underneath them.
async fn lock_grant(conn: &mut PgConnection, id: Uuid) -> Result<AdminIdentityGrant, AppError> {
    let row = sqlx::query(&format!(
        "select {GRANT_COLUMNS} from admin_identities \
         where id = $1 and deleted_at is null for update"
    ))
    .bind(id)
    .fetch_optional(conn)
    .await?
    .ok_or_else(|| grant_not_found(id))?;
    grant_from_row(&row)
}

/// The last-primary guard, as a query.
///
/// This is the concrete payoff of decision D1. Under plan 09's original scope design the
/// question "who else is primary" answered *"everyone, by implication"*, because
/// `moira:admin` implies every scope for a trusted-JWT actor — so there was nothing to
/// count and no guard to write.
/// Clears ownership from whoever currently holds it, so a promotion can take it.
///
/// Runs under [`OWNERSHIP_LOCK_KEY`] and, by `admin_identities_single_active_primary`,
/// touches at most one row. The version trigger fires on that row, so the demoted grant's
/// ETag changes — a console still holding the old one gets `resource_version_conflict`
/// rather than silently acting on a record whose ownership moved underneath it.
async fn demote_active_primaries_other_than(
    conn: &mut PgConnection,
    excluding: Uuid,
) -> Result<(), AppError> {
    sqlx::query(
        "update admin_identities set is_primary = false \
         where deleted_at is null and status = 'active' and is_primary and id <> $1",
    )
    .bind(excluding)
    .execute(conn)
    .await?;
    Ok(())
}

async fn require_another_active_primary(
    conn: &mut PgConnection,
    excluding: Uuid,
) -> Result<(), AppError> {
    let other = sqlx::query_scalar::<_, Uuid>(
        "select id from admin_identities \
         where deleted_at is null and status = 'active' and is_primary and id <> $1 limit 1",
    )
    .bind(excluding)
    .fetch_optional(conn)
    .await?;
    if other.is_some() {
        Ok(())
    } else {
        Err(AppError::conflict(
            "admin_identity_last_primary",
            "this is the last admin identity that can manage other admins",
        ))
    }
}

fn version_conflict() -> AppError {
    AppError::conflict(
        "resource_version_conflict",
        "resource version does not match If-Match",
    )
}

fn grant_not_found(id: Uuid) -> AppError {
    AppError::coded(
        StatusCode::NOT_FOUND,
        "admin_identity_not_found",
        format!("admin identity {id} was not found"),
    )
}

fn already_revoked(id: Uuid) -> AppError {
    AppError::conflict(
        "admin_identity_already_revoked",
        format!("admin identity {id} has already been revoked"),
    )
}

fn invite_not_found() -> AppError {
    AppError::coded(
        StatusCode::NOT_FOUND,
        "invite_not_found",
        "no invitation matches this token",
    )
}

fn already_consumed() -> AppError {
    AppError::conflict(
        "invite_already_consumed",
        "this invitation has already been redeemed",
    )
}

fn invite_revoked() -> AppError {
    AppError::coded(
        StatusCode::FORBIDDEN,
        "invite_revoked",
        "this invitation has been revoked",
    )
}

fn invite_expired() -> AppError {
    AppError::coded(
        StatusCode::FORBIDDEN,
        "invite_expired",
        "this invitation has expired",
    )
}

fn invite_from_row(row: &sqlx::postgres::PgRow) -> Result<AdminInviteRow, AppError> {
    let email_constraint: Option<String> = row.try_get("email_constraint")?;
    let domain_constraint: Option<String> = row.try_get("domain_constraint")?;
    // `admin_invites_exactly_one_constraint` makes the both-null and both-set cases
    // unrepresentable, so reaching the error arm means the CHECK was dropped. Saying so
    // beats inventing a default that would silently widen an invite's audience.
    let (constraint, value) = match (email_constraint, domain_constraint) {
        (Some(email), None) => (AdminInviteConstraint::Email, email),
        (None, Some(domain)) => (AdminInviteConstraint::Domain, domain),
        _ => {
            return Err(AppError::Internal(
                "admin_invites row carries neither exactly one constraint".to_string(),
            ));
        }
    };
    Ok(AdminInviteRow {
        id: row.try_get("id")?,
        constraint,
        value,
        status: invite_status_from_db(row.try_get::<String, _>("status")?)?,
        expires_at: row.try_get("expires_at")?,
        created_by_subject: row.try_get("created_by_subject")?,
        consumed_at: row.try_get("consumed_at")?,
        consumed_subject: row.try_get("consumed_subject")?,
        created_at: row.try_get("created_at")?,
        version: row.try_get("version")?,
    })
}

/// `'expired'` is deliberately not handled, because `0017`'s CHECK makes it illegal:
/// expiry is derived from `expires_at`. Mapping an unknown value onto `Pending` would
/// make an unredeemable invite look redeemable.
fn invite_status_from_db(value: String) -> Result<AdminInviteStatus, AppError> {
    match value.as_str() {
        "pending" => Ok(AdminInviteStatus::Pending),
        "consumed" => Ok(AdminInviteStatus::Consumed),
        "revoked" => Ok(AdminInviteStatus::Revoked),
        _ => Err(AppError::Internal(format!(
            "unknown admin invite status {value}"
        ))),
    }
}

/// The one unique index on `admin_identities` whose violation is **not** a claim conflict.
const SINGLE_ACTIVE_PRIMARY_INDEX: &str = "admin_identities_single_active_primary";

/// The `409` that `admin_identities_issuer_subject_active_unique` produces.
///
/// This is the database-level backstop: it holds even if the command runner's advisory
/// lock window is somehow raced, which is why the constraint exists rather than a
/// read-then-write check in the service.
///
/// # It matches by constraint name, not by "any unique violation"
///
/// `0019` gave this table a second unique index. A violation of
/// `admin_identities_single_active_primary` means the ownership lock failed to serialise
/// two grants — an internal invariant break, not "this identity is taken". Reporting it as
/// `admin_identity_already_claimed` would send an operator to inspect the wrong row and
/// would hide a real defect inside a routine-looking `409`; letting it fall through to
/// `AppError::Sqlx` is the honest answer, and deliberately mints no new error code for a
/// condition no correct code path can reach (a catalogued code with no emitter is what
/// plan 09 §0.5 rules out).
///
/// A violation that reports *no* constraint name still maps to the claim conflict:
/// `admin_identities_issuer_subject_active_unique` is the only other unique index an
/// insert here can hit, and preserving the existing `409` matters more than reacting to a
/// driver that stopped naming constraints.
fn already_claimed_on_unique_violation(error: sqlx::Error) -> AppError {
    let is_claim_conflict = match &error {
        sqlx::Error::Database(database) => {
            database.is_unique_violation()
                && database.constraint() != Some(SINGLE_ACTIVE_PRIMARY_INDEX)
        }
        _ => false,
    };
    if is_claim_conflict {
        AppError::conflict(
            "admin_identity_already_claimed",
            "this identity has already been granted admin access",
        )
    } else {
        AppError::from(error)
    }
}

fn grant_from_row(row: &sqlx::postgres::PgRow) -> Result<AdminIdentityGrant, AppError> {
    Ok(AdminIdentityGrant {
        id: row.try_get("id")?,
        trusted_jwt_issuer_id: row.try_get("trusted_jwt_issuer_id")?,
        issuer: row.try_get("issuer")?,
        subject: row.try_get("subject")?,
        email: row.try_get("email")?,
        email_verified: row.try_get("email_verified")?,
        granted_scopes: row.try_get("granted_scopes")?,
        is_primary: row.try_get("is_primary")?,
        status: admin_identity_status_from_db(row.try_get::<String, _>("status")?)?,
        created_at: row.try_get("created_at")?,
        version: row.try_get("version")?,
    })
}

/// `'deleted'` is a legal column value that nothing writes: `find_active_grant` filters
/// `status = 'active'`, `insert_grant` returns a row that has just taken the `'active'`
/// default, and `revoke_grant` writes `'revoked'`. `list_grants` is the one query that
/// projects the column unfiltered, so a hand-written `'deleted'` row would reach here —
/// and erroring is the right answer. Mapping it onto `Revoked` would be a lie about what
/// the database says, and mapping it onto `Active` would show a stripped grant as live.
fn admin_identity_status_from_db(value: String) -> Result<AdminIdentityStatus, AppError> {
    match value.as_str() {
        "active" => Ok(AdminIdentityStatus::Active),
        "revoked" => Ok(AdminIdentityStatus::Revoked),
        _ => Err(AppError::Internal(format!(
            "unknown admin identity status {value}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    /// `AdminIdentityService` stores its repository behind `Arc<dyn …>`, which only
    /// compiles while the trait stays object-safe.
    #[test]
    fn admin_identity_repository_trait_is_object_safe() {
        fn assert_object_safe(_: Arc<dyn AdminIdentityRepository>) {}
        let _ = assert_object_safe;
    }

    #[test]
    fn grant_columns_never_select_secret_material() {
        for forbidden in [
            "key_hash",
            "key_prefix",
            "encrypted_payload",
            "encrypted_data_key",
            "secret_fingerprint",
            "masked_secret",
            "token",
        ] {
            assert!(
                !GRANT_COLUMNS.contains(forbidden),
                "the grant projection must not select {forbidden}"
            );
        }
    }

    /// The invite projection every read path shares must never carry the token, its
    /// hash, its prefix or its fingerprint. `INVITE_CANDIDATE_COLUMNS` is the single
    /// deliberate exception, and it exists only so a presented token can be verified.
    #[test]
    fn invite_columns_never_select_secret_material() {
        for forbidden in [
            "token_hash",
            "token_prefix",
            "fingerprint",
            "pepper_version",
        ] {
            assert!(
                !INVITE_COLUMNS.contains(forbidden),
                "the invite projection must not select {forbidden}"
            );
        }
        assert!(
            INVITE_CANDIDATE_COLUMNS.contains("token_hash"),
            "the verification projection is the one place the hash is read"
        );
        for forbidden in ["token_prefix", "fingerprint", "pepper_version"] {
            assert!(
                !INVITE_CANDIDATE_COLUMNS.contains(forbidden),
                "even the verification projection must not select {forbidden}"
            );
        }
    }

    #[test]
    fn invite_status_parsing_rejects_a_status_no_migration_allows() {
        assert_eq!(
            invite_status_from_db("pending".to_string()).expect("pending parses"),
            AdminInviteStatus::Pending
        );
        assert_eq!(
            invite_status_from_db("consumed".to_string()).expect("consumed parses"),
            AdminInviteStatus::Consumed
        );
        assert_eq!(
            invite_status_from_db("revoked".to_string()).expect("revoked parses"),
            AdminInviteStatus::Revoked
        );
        // `0017` deliberately omits `'expired'`: expiry is derived from `expires_at`,
        // because nothing sweeps for it. A silent mapping onto `Pending` here would make
        // an unredeemable invite read as redeemable.
        assert!(invite_status_from_db("expired".to_string()).is_err());
    }

    /// Expiry is a comparison against `expires_at`, never a stored status — `0017`'s
    /// CHECK has no `'expired'` value precisely because nothing sweeps for one. The
    /// boundary is asserted too: `expires_at` is the first instant the invite is dead,
    /// not the last instant it is alive, so a token cannot be redeemed on its deadline.
    #[test]
    fn expiry_is_derived_from_the_timestamp_not_from_a_status() {
        let deadline = Utc::now();
        let row = AdminInviteRow {
            id: Uuid::nil(),
            constraint: AdminInviteConstraint::Domain,
            value: "example.com".to_string(),
            status: AdminInviteStatus::Pending,
            expires_at: deadline,
            created_by_subject: None,
            consumed_at: None,
            consumed_subject: None,
            created_at: deadline - chrono::Duration::hours(1),
            version: 1,
        };
        assert!(!row.is_expired(deadline - chrono::Duration::seconds(1)));
        assert!(row.is_expired(deadline), "the deadline itself is expired");
        assert!(row.is_expired(deadline + chrono::Duration::seconds(1)));
        // The status is untouched by any of that: a pending row stays pending, which is
        // why every read path derives rather than trusting the column.
        assert_eq!(row.status, AdminInviteStatus::Pending);
    }

    /// The ownership lock is a shared constant, not a per-call-site number: two callers
    /// hashing "ownership" differently would each hold a lock the other ignores, and the
    /// last-primary guard would be back to racing.
    #[test]
    fn the_ownership_lock_key_is_a_single_stable_constant() {
        assert_eq!(OWNERSHIP_LOCK_KEY, i64::from_be_bytes(*b"moiraown"));
    }

    #[test]
    fn only_active_and_revoked_are_grant_statuses() {
        assert_eq!(
            admin_identity_status_from_db("active".to_string()).expect("active parses"),
            AdminIdentityStatus::Active
        );
        assert_eq!(
            admin_identity_status_from_db("revoked".to_string()).expect("revoked parses"),
            AdminIdentityStatus::Revoked
        );
        assert!(admin_identity_status_from_db("deleted".to_string()).is_err());
    }

    /// A non-unique database failure must stay a database failure — collapsing every
    /// `sqlx::Error` onto `409 admin_identity_already_claimed` would tell a caller their
    /// identity is taken when the real problem was, say, a dropped connection.
    #[test]
    fn only_a_unique_violation_becomes_already_claimed() {
        let mapped = already_claimed_on_unique_violation(sqlx::Error::RowNotFound);
        assert!(matches!(mapped, AppError::Sqlx(_)));
    }

    /// The ownership index is named in exactly one place, and it is the same string the
    /// migration creates. A rename on either side that forgot the other would restore the
    /// old behaviour — every unique violation on this table reported as
    /// `admin_identity_already_claimed` — with no test noticing, because the mapping
    /// would still *work*, just for the wrong index.
    #[test]
    fn the_single_primary_index_name_matches_the_migration_that_creates_it() {
        let migration = include_str!("../../../migrations/0019_single_primary_admin.sql");
        assert!(
            migration.contains(&format!(
                "create unique index if not exists {SINGLE_ACTIVE_PRIMARY_INDEX}"
            )),
            "{SINGLE_ACTIVE_PRIMARY_INDEX} is not the index 0019 creates"
        );
        // And the predicate is the one the last-primary guard counts over. A partial index
        // that forgot `deleted_at is null` or `status = 'active'` would let a revoked grant
        // keep the ownership slot occupied, which is unreachable through the API and
        // therefore unrecoverable without SQL.
        assert!(
            migration.contains("where deleted_at is null and status = 'active' and is_primary"),
            "the ownership index must be partial on exactly the set the guard counts"
        );
    }

    /// `0019`'s repair steps must survive being run against a database that `0017` already
    /// backfilled. Migrations are append-only and neither can be edited afterwards, so the
    /// only thing standing between a re-run and a silent authority change is the
    /// `not exists` guard on each step.
    #[test]
    fn the_ownership_backfill_is_guarded_against_a_deployment_that_already_has_an_owner() {
        let migration = include_str!("../../../migrations/0019_single_primary_admin.sql");
        let promotions = migration.matches("set is_primary = true").count();
        assert_eq!(
            promotions, 2,
            "0019 promotes in exactly two steps: the setup claimant, then the sole grant"
        );
        assert_eq!(
            migration
                .matches("select 1 from admin_identities existing")
                .count(),
            promotions,
            "every promotion must be guarded by 'no active primary exists', or a re-run \
             would move ownership on a deployment that already has an owner"
        );
    }

    #[tokio::test]
    async fn migrated_database_supports_the_identity_reads() {
        let Some(database) = crate::test_support::test_database().await else {
            return;
        };
        let repo = PgAdminIdentityRepository::new(database.pool().clone());

        // The singleton row is seeded by `0012`, so this resolves rather than defaulting.
        repo.setup_claimed().await.expect("setup state reads");
        assert!(
            repo.find_active_grant("https://issuer.invalid", "nobody")
                .await
                .expect("grant lookup runs")
                .is_none()
        );
        let error = repo
            .resolve_active_issuer("https://unregistered.invalid")
            .await
            .expect_err("an unregistered issuer is refused");
        assert_eq!(error.status(), StatusCode::BAD_REQUEST);
        assert!(
            error.to_string().contains("unregistered_trusted_issuer"),
            "unexpected error: {error}"
        );
    }
}
