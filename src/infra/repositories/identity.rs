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

use crate::{domain::AdminIdentityStatus, error::AppError};

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
    /// `'system_key'` or `'setup_token'` — recorded honestly. Under D1 only
    /// `'system_key'` is ever written; the CHECK keeps the other value legal so reversing
    /// D1 needs no further migration.
    pub granted_by_actor_type: String,
    pub granted_by_subject: Option<String>,
}

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
                             granted_scopes, status, created_at, version";

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
        let row = sqlx::query(&format!(
            r#"
            insert into admin_identities
                (id, trusted_jwt_issuer_id, issuer, subject, email, email_verified,
                 granted_scopes, granted_by_actor_type, granted_by_subject)
            values ($1, $2, $3, $4, $5, $6, $7, $8, $9)
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
}

/// The `409` that `admin_identities_issuer_subject_active_unique` produces.
///
/// This is the database-level backstop: it holds even if the command runner's advisory
/// lock window is somehow raced, which is why the constraint exists rather than a
/// read-then-write check in the service.
fn already_claimed_on_unique_violation(error: sqlx::Error) -> AppError {
    match &error {
        sqlx::Error::Database(database) if database.is_unique_violation() => AppError::conflict(
            "admin_identity_already_claimed",
            "this identity has already been granted admin access",
        ),
        _ => AppError::from(error),
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
        status: admin_identity_status_from_db(row.try_get::<String, _>("status")?)?,
        created_at: row.try_get("created_at")?,
        version: row.try_get("version")?,
    })
}

/// `'deleted'` is a legal column value that no query in this module can return —
/// `find_active_grant` filters `status = 'active'`, and `insert_grant` returns a row that
/// has just taken the `'active'` default. Mapping it onto `Revoked` would be a lie about
/// what the database says, so it is an internal error instead of a plausible-looking
/// answer.
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

    #[tokio::test]
    async fn migrated_database_supports_the_identity_reads() {
        let Ok(database_url) = std::env::var("MOIRA_TEST_DATABASE_URL") else {
            eprintln!(
                "skipping admin identity repository integration: set MOIRA_TEST_DATABASE_URL"
            );
            return;
        };
        let pool = PgPool::connect(&database_url).await.expect("connect");
        crate::infra::db::migrate(&pool).await.expect("migrate");
        let repo = PgAdminIdentityRepository::new(pool);

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
