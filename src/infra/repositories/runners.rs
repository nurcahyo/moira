//! Claude runner mirror persistence (issue #275, workstream R2 of #272).
//!
//! Backs `migrations/0035_claude_runners.sql`.
//!
//! # There is no secret column here, and therefore no secret in this file
//!
//! The token a runner mints never touches this table. It travels from `moira-runner`'s one-shot
//! token read straight into `provider_credentials` through the existing credential chain, and
//! what lands here is the resulting `credential_id`. There is **no encrypted-payload parameter,
//! no `load_secret`-style read-back, and nothing for one to be added to** — the same prohibition
//! `auth_settings.rs` records for D7, for the same reason: a second place that can hold provider
//! secret material is a second place to get the envelope wrong.
//!
//! # Optimistic concurrency
//!
//! `soft_delete` locks its row with `select … for update` and compares the caller's `If-Match`
//! **inside the same transaction as the write**, mirroring `lock_and_match_version` in
//! [`super::admin`]. The state-machine transitions deliberately do not take a version
//! precondition — see the note on `IF_MATCH_OPERATIONS` in `src/http/mod.rs` — they are guarded
//! by an explicit `from` state instead, which is a stronger check: a version match proves only
//! that nobody else wrote, while a state match proves the transition is legal.

use async_trait::async_trait;
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{PgConnection, PgPool, Row, postgres::PgRow};
use uuid::Uuid;

use crate::{
    domain::{AuditLogInsert, ClaudeRunnerRecord, ClaudeRunnerState, CredentialScope, ListCursor},
    error::AppError,
    infra::{
        pg_rows::{credential_scope_from_parts, scope_type_from_db, scope_type_to_db},
        repositories::admin::commit_with_audit,
    },
};

/// What a refresh or a transition writes back onto a row.
///
/// A struct rather than five parameters because four of the five are optional and a positional
/// call site would be five `None`s in a row that a reviewer cannot check.
#[derive(Debug, Clone, Default)]
pub struct RunnerStateUpdate {
    pub state: Option<ClaudeRunnerState>,
    pub authorization_url: Option<String>,
    pub error_code: Option<String>,
    pub expires_at: Option<DateTime<Utc>>,
    pub credential_id: Option<Uuid>,
}

/// Everything an insert needs.
///
/// A struct rather than a positional argument list: clippy refuses nine parameters, and it is
/// right to — `provider_id`, `expires_at` and `metadata` are all optional-ish and two of them are
/// `Option`, so a positional call site is a row of values a reviewer cannot check against its
/// header.
#[derive(Debug, Clone)]
pub struct ClaudeRunnerInsert<'a> {
    pub id: Uuid,
    pub label: &'a str,
    pub runner_reference: &'a str,
    pub provider_id: Option<Uuid>,
    /// Whose Claude account this runner is for. Decided here and never again: the credential
    /// finalize writes is sealed under it (`credential_aad`), so it cannot be corrected later
    /// without re-encrypting.
    pub scope: &'a CredentialScope,
    pub expires_at: Option<DateTime<Utc>>,
    pub metadata: &'a Value,
}

#[async_trait]
pub trait ClaudeRunnerRepository: Send + Sync {
    /// Inserts the mirror row **inside the caller's transaction**, so the write sits under
    /// `AdminCommandRunner`'s idempotency savepoint.
    async fn create(
        &self,
        conn: &mut PgConnection,
        insert: ClaudeRunnerInsert<'_>,
    ) -> Result<ClaudeRunnerRecord, AppError>;

    /// Over-fetches by one, exactly like every other admin list.
    async fn list(
        &self,
        cursor: Option<ListCursor>,
        limit: i64,
    ) -> Result<Vec<ClaudeRunnerRecord>, AppError>;

    async fn get(&self, id: Uuid) -> Result<ClaudeRunnerRecord, AppError>;

    /// Applies `update`, refusing the write unless the row is currently in one of `from`.
    ///
    /// `from` is the whole of the state-machine guard and it is not optional: a transition that
    /// accepts any starting state is not a state machine. An empty slice would silently mean
    /// "always refuse", so it is a programming error the caller cannot express — every caller
    /// passes a literal.
    async fn transition(
        &self,
        id: Uuid,
        from: &[ClaudeRunnerState],
        update: RunnerStateUpdate,
        audit: Option<AuditLogInsert>,
    ) -> Result<ClaudeRunnerRecord, AppError>;

    async fn soft_delete(
        &self,
        id: Uuid,
        expected_version: i64,
        audit: AuditLogInsert,
    ) -> Result<(), AppError>;
}

#[derive(Debug, Clone)]
pub struct PgClaudeRunnerRepository {
    pool: PgPool,
}

impl PgClaudeRunnerRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

/// Every column of the table except `deleted_at`, which is a predicate rather than a payload.
/// There is deliberately no secret column in this list, and none on the table to add.
const RECORD_COLUMNS: &str = "id, label, runner_reference, state, authorization_url, error_code, \
                              expires_at, credential_id, provider_id, scope_type, \
                              external_tenant_id, application_id, external_user_id, metadata, \
                              created_at, updated_at, version";

const RUNNER_VERSION_FOR_UPDATE: &str =
    "select version from claude_runners where id = $1 and deleted_at is null for update";

#[async_trait]
impl ClaudeRunnerRepository for PgClaudeRunnerRepository {
    async fn create(
        &self,
        conn: &mut PgConnection,
        insert: ClaudeRunnerInsert<'_>,
    ) -> Result<ClaudeRunnerRecord, AppError> {
        let ClaudeRunnerInsert {
            id,
            label,
            runner_reference,
            provider_id,
            scope,
            expires_at,
            metadata,
        } = insert;
        let row = sqlx::query(&format!(
            r#"
            insert into claude_runners
                (id, label, runner_reference, state, expires_at, provider_id, metadata,
                 scope_type, external_tenant_id, application_id, external_user_id)
            values ($1, $2, $3, 'provisioning', $4, $5, $6, $7, $8, $9, $10)
            returning {RECORD_COLUMNS}
            "#
        ))
        .bind(id)
        .bind(label)
        .bind(runner_reference)
        .bind(expires_at)
        .bind(provider_id)
        .bind(metadata)
        // The same encoders `provider_credentials` uses, not a second spelling: a runner scope
        // the credential table would refuse is a finalize that fails after the one-shot token
        // has been spent.
        .bind(scope_type_to_db(&scope.scope_type()))
        .bind(scope.external_tenant_id())
        .bind(scope.application_id())
        .bind(scope.external_user_id())
        .fetch_one(conn)
        .await
        .map_err(map_constraint_violation)?;
        record_from_row(&row)
    }

    async fn list(
        &self,
        cursor: Option<ListCursor>,
        limit: i64,
    ) -> Result<Vec<ClaudeRunnerRecord>, AppError> {
        let (keyset, limit_param) = match cursor {
            Some(_) => ("and (created_at, id) < ($1::timestamptz, $2::uuid)", "$3"),
            None => ("", "$1"),
        };
        let sql = format!(
            "select {RECORD_COLUMNS} from claude_runners \
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
        rows.iter().map(record_from_row).collect()
    }

    async fn get(&self, id: Uuid) -> Result<ClaudeRunnerRecord, AppError> {
        let row = sqlx::query(&format!(
            "select {RECORD_COLUMNS} from claude_runners where id = $1 and deleted_at is null"
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| runner_not_found(id))?;
        record_from_row(&row)
    }

    async fn transition(
        &self,
        id: Uuid,
        from: &[ClaudeRunnerState],
        update: RunnerStateUpdate,
        audit: Option<AuditLogInsert>,
    ) -> Result<ClaudeRunnerRecord, AppError> {
        let allowed: Vec<String> = from
            .iter()
            .map(|state| state_to_db(*state).to_string())
            .collect();
        let mut tx = self.pool.begin().await?;
        // Lock first so the state check and the write cannot straddle a concurrent transition.
        let current: Option<String> = sqlx::query_scalar(
            "select state from claude_runners where id = $1 and deleted_at is null for update",
        )
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
        let current = current.ok_or_else(|| runner_not_found(id))?;
        if !allowed.contains(&current) {
            return Err(runner_wrong_state());
        }
        let row = sqlx::query(&format!(
            r#"
            update claude_runners
            set state = coalesce($2, state),
                authorization_url = coalesce($3, authorization_url),
                error_code = coalesce($4, error_code),
                expires_at = coalesce($5, expires_at),
                credential_id = coalesce($6, credential_id),
                updated_at = now()
            where id = $1 and deleted_at is null
            returning {RECORD_COLUMNS}
            "#
        ))
        .bind(id)
        .bind(update.state.map(|state| state_to_db(state).to_string()))
        .bind(update.authorization_url.as_deref())
        .bind(update.error_code.as_deref())
        .bind(update.expires_at)
        .bind(update.credential_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_constraint_violation)?
        .ok_or_else(|| runner_not_found(id))?;
        let record = record_from_row(&row)?;
        match audit {
            Some(audit) => commit_with_audit(tx, audit).await?,
            None => tx.commit().await?,
        }
        Ok(record)
    }

    async fn soft_delete(
        &self,
        id: Uuid,
        expected_version: i64,
        audit: AuditLogInsert,
    ) -> Result<(), AppError> {
        let mut tx = self.pool.begin().await?;
        let current_version = lock_runner_version(&mut tx, id, expected_version).await?;
        let result = sqlx::query(
            "update claude_runners \
             set deleted_at = now(), updated_at = now() \
             where id = $1 and deleted_at is null and version = $2",
        )
        .bind(id)
        .bind(current_version)
        .execute(&mut *tx)
        .await?;
        if result.rows_affected() == 0 {
            return Err(version_conflict());
        }
        commit_with_audit(tx, audit).await?;
        Ok(())
    }
}

async fn lock_runner_version(
    conn: &mut PgConnection,
    id: Uuid,
    expected_version: i64,
) -> Result<i64, AppError> {
    let current_version = sqlx::query_scalar::<_, i64>(RUNNER_VERSION_FOR_UPDATE)
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(|| runner_not_found(id))?;
    if current_version != expected_version {
        return Err(version_conflict());
    }
    Ok(current_version)
}

/// `pub(crate)` so the service layer can refuse a stale precondition **before** it performs the
/// irreversible half of a delete. See `ClaudeRunnerService::delete`: the authoritative check is
/// still the one inside `soft_delete`'s transaction, but a check that only runs after the
/// container has already been destroyed does not prevent the lost update it exists for.
pub(crate) fn version_conflict() -> AppError {
    AppError::conflict(
        "resource_version_conflict",
        "resource version does not match If-Match",
    )
}

pub(crate) fn runner_not_found(id: Uuid) -> AppError {
    AppError::coded(
        StatusCode::NOT_FOUND,
        "runner_not_found",
        format!("claude runner {id} was not found"),
    )
}

/// The one code for "this runner is not in a state that permits the requested transition".
///
/// Deliberately the same string the runner service's own contract uses, and emitted from both
/// sides of the boundary: Moira's mirror refuses first (the common path, one round trip saved),
/// and a relayed upstream `409 runner_wrong_state` maps onto the same code. One code means an
/// operator reads one remedy rather than two failures that look unrelated.
pub(crate) fn runner_wrong_state() -> AppError {
    AppError::conflict(
        "runner_wrong_state",
        "the runner is not in a state that permits this operation",
    )
}

fn map_constraint_violation(error: sqlx::Error) -> AppError {
    let sqlx::Error::Database(database) = &error else {
        return AppError::from(error);
    };
    if database.is_unique_violation() {
        return AppError::conflict(
            "duplicate_runner_label",
            "a live runner already uses this label",
        );
    }
    AppError::from(error)
}

pub(crate) fn state_to_db(state: ClaudeRunnerState) -> &'static str {
    match state {
        ClaudeRunnerState::Provisioning => "provisioning",
        ClaudeRunnerState::AwaitingAuthorization => "awaiting_authorization",
        ClaudeRunnerState::Exchanging => "exchanging",
        ClaudeRunnerState::Ready => "ready",
        ClaudeRunnerState::Linked => "linked",
        ClaudeRunnerState::Failed => "failed",
        ClaudeRunnerState::Expired => "expired",
    }
}

pub(crate) fn state_from_db(value: &str) -> Result<ClaudeRunnerState, AppError> {
    Ok(match value {
        "provisioning" => ClaudeRunnerState::Provisioning,
        "awaiting_authorization" => ClaudeRunnerState::AwaitingAuthorization,
        "exchanging" => ClaudeRunnerState::Exchanging,
        "ready" => ClaudeRunnerState::Ready,
        "linked" => ClaudeRunnerState::Linked,
        "failed" => ClaudeRunnerState::Failed,
        "expired" => ClaudeRunnerState::Expired,
        other => {
            return Err(AppError::Internal(format!(
                "unknown claude runner state {other}"
            )));
        }
    })
}

fn record_from_row(row: &PgRow) -> Result<ClaudeRunnerRecord, AppError> {
    Ok(ClaudeRunnerRecord {
        id: row.try_get("id")?,
        label: row.try_get("label")?,
        runner_reference: row.try_get("runner_reference")?,
        state: state_from_db(&row.try_get::<String, _>("state")?)?,
        authorization_url: row.try_get("authorization_url")?,
        error_code: row.try_get("error_code")?,
        expires_at: row.try_get::<Option<DateTime<Utc>>, _>("expires_at")?,
        credential_id: row.try_get("credential_id")?,
        provider_id: row.try_get("provider_id")?,
        // Rebuilt through the SAME assembler `provider_credentials` rows use
        // (`pg_rows::credential_scope_from_parts`), so a runner's scope and the scope its
        // credential was sealed under can never be two different readings of the same columns.
        scope: credential_scope_from_parts(
            scope_type_from_db(row.try_get::<String, _>("scope_type")?)?,
            row.try_get("external_tenant_id")?,
            row.try_get("application_id")?,
            row.try_get("external_user_id")?,
        )?,
        metadata: row.try_get::<Value, _>("metadata")?,
        created_at: row.try_get::<DateTime<Utc>, _>("created_at")?,
        updated_at: row.try_get::<DateTime<Utc>, _>("updated_at")?,
        version: row.try_get("version")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The encoder and the decoder must be exact inverses, and both must agree with the CHECK
    /// constraint in `migrations/0035`. A drift here is a row this binary can write and cannot
    /// read back, which surfaces as a `500` on a list rather than at the write.
    #[test]
    fn every_state_round_trips_through_the_database_encoding() {
        for state in [
            ClaudeRunnerState::Provisioning,
            ClaudeRunnerState::AwaitingAuthorization,
            ClaudeRunnerState::Exchanging,
            ClaudeRunnerState::Ready,
            ClaudeRunnerState::Linked,
            ClaudeRunnerState::Failed,
            ClaudeRunnerState::Expired,
        ] {
            assert_eq!(
                state_from_db(state_to_db(state)).expect("known state"),
                state
            );
        }
    }

    #[test]
    fn an_unknown_state_is_refused_rather_than_guessed() {
        assert!(state_from_db("teleporting").is_err());
    }
}
