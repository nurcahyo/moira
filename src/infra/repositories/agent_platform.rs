//! Postgres repository for the agent platform (issue #214, plan 12 §3/§5).
//!
//! Schema + CRUD only. This owns the `skills` table's read/write surface; evals and flows CRUD
//! are a documented follow-up (their tables land in `migrations/0031_agent_platform.sql`).
//!
//! A concrete struct rather than a `dyn` trait: unlike `RuntimeRepository`, nothing here needs a
//! Postgres-free fake yet, and a concrete type keeps the async surface simple. The tiny
//! version-guard helpers (`version_conflict`, `lock_and_match_version`, `over_fetch_limit`) are
//! duplicated from `super::runtime` on purpose — the same deliberate duplication that module's
//! own doc comment records, so two repositories owning disjoint tables need not import each
//! other's internals. `commit_with_audit` is shared from `super::admin` because it writes the
//! one audit-in-transaction shape every admin write uses.

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::{
    domain::{
        AuditLogInsert, ListCursor, SkillCreateRequest, SkillHttpExecutorPatchRequest,
        SkillHttpExecutorRecord, SkillPatchRequest, SkillRecord,
    },
    error::AppError,
    infra::pg_rows::{
        http_method_to_db, skill_http_executor_record_from_row, skill_record_from_row,
    },
    orchestration::ParsedOperation,
};

use super::admin::commit_with_audit;

/// The column list every `skills` read and write returns, in one place so the `INSERT`,
/// `UPDATE`, list and get statements can never drift apart.
const SKILL_COLUMNS: &str = "id, skill_key, display_name, description, kind, params_schema, \
     tags, status, metadata, created_at, updated_at, deleted_at, version";

const SKILL_VERSION_FOR_UPDATE: &str =
    "select version from skills where id = $1 and deleted_at is null for update";

/// The column list every `skill_http_executors` read and write returns. Unlike
/// [`SKILL_COLUMNS`] there is no `deleted_at`/`version` — see
/// `domain::SkillHttpExecutorRecord`'s doc comment for why this table carries neither.
const EXECUTOR_COLUMNS: &str = "skill_id, method, url_template, allowed_host, header_template, \
     credential_id, timeout_ms, response_schema, created_at, updated_at";

#[derive(Clone)]
pub struct PgAgentPlatformRepository {
    pool: PgPool,
}

impl PgAgentPlatformRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn create_skill(
        &self,
        id: Uuid,
        request: &SkillCreateRequest,
        audit: AuditLogInsert,
    ) -> Result<SkillRecord, AppError> {
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query(&format!(
            "insert into skills (id, skill_key, display_name, description, kind, params_schema, \
             tags, metadata) values ($1, $2, $3, $4, $5, $6, $7, $8) returning {SKILL_COLUMNS}"
        ))
        .bind(id)
        .bind(&request.skill_key)
        .bind(&request.display_name)
        .bind(&request.description)
        .bind(skill_kind_to_db(&request.kind))
        .bind(&request.params_schema)
        .bind(&request.tags)
        .bind(&request.metadata)
        .fetch_one(&mut *tx)
        .await?;
        let record = skill_record_from_row(&row)?;
        commit_with_audit(tx, audit).await?;
        Ok(record)
    }

    pub async fn list_skills(
        &self,
        cursor: Option<ListCursor>,
        limit: i64,
    ) -> Result<Vec<SkillRecord>, AppError> {
        let rows = sqlx::query(&format!(
            "select {SKILL_COLUMNS} from skills where deleted_at is null \
             and ($1::timestamptz is null or (created_at, id) < ($1::timestamptz, $2::uuid)) \
             order by created_at desc, id desc limit $3"
        ))
        .bind(cursor.map(|c| c.ts))
        .bind(cursor.map(|c| c.id))
        .bind(over_fetch_limit(limit))
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(skill_record_from_row).collect()
    }

    pub async fn get_skill(&self, id: Uuid) -> Result<SkillRecord, AppError> {
        let row = sqlx::query(&format!(
            "select {SKILL_COLUMNS} from skills where id = $1 and deleted_at is null"
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("skill {id}")))?;
        skill_record_from_row(&row)
    }

    pub async fn patch_skill(
        &self,
        id: Uuid,
        expected_version: i64,
        request: &SkillPatchRequest,
        audit: AuditLogInsert,
    ) -> Result<SkillRecord, AppError> {
        let mut tx = self.pool.begin().await?;
        let current_version = lock_and_match_version(
            &mut tx,
            SKILL_VERSION_FOR_UPDATE,
            id,
            expected_version,
            format!("skill {id}"),
        )
        .await?;
        let row = sqlx::query(&format!(
            "update skills set \
                display_name = coalesce($2, display_name), \
                description = coalesce($3, description), \
                params_schema = coalesce($4, params_schema), \
                tags = coalesce($5, tags), \
                metadata = coalesce($6, metadata), \
                updated_at = now() \
             where id = $1 and deleted_at is null and version = $7 returning {SKILL_COLUMNS}"
        ))
        .bind(id)
        .bind(&request.display_name)
        .bind(&request.description)
        .bind(&request.params_schema)
        .bind(&request.tags)
        .bind(&request.metadata)
        .bind(current_version)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(version_conflict)?;
        let record = skill_record_from_row(&row)?;
        commit_with_audit(tx, audit).await?;
        Ok(record)
    }

    pub async fn set_skill_status(
        &self,
        id: Uuid,
        expected_version: i64,
        status: &str,
        audit: AuditLogInsert,
    ) -> Result<SkillRecord, AppError> {
        let mut tx = self.pool.begin().await?;
        let current_version = lock_and_match_version(
            &mut tx,
            SKILL_VERSION_FOR_UPDATE,
            id,
            expected_version,
            format!("skill {id}"),
        )
        .await?;
        let row = sqlx::query(&format!(
            "update skills set status = $2, updated_at = now() \
             where id = $1 and deleted_at is null and version = $3 returning {SKILL_COLUMNS}"
        ))
        .bind(id)
        .bind(status)
        .bind(current_version)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(version_conflict)?;
        let record = skill_record_from_row(&row)?;
        commit_with_audit(tx, audit).await?;
        Ok(record)
    }

    pub async fn soft_delete_skill(
        &self,
        id: Uuid,
        expected_version: i64,
        audit: AuditLogInsert,
    ) -> Result<(), AppError> {
        let mut tx = self.pool.begin().await?;
        let current_version = lock_and_match_version(
            &mut tx,
            SKILL_VERSION_FOR_UPDATE,
            id,
            expected_version,
            format!("skill {id}"),
        )
        .await?;
        let result = sqlx::query(
            "update skills set deleted_at = now(), updated_at = now() \
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

    /// Enables every live skill named by `ids` in one statement. Missing or soft-deleted ids are
    /// silently skipped (bulk semantics); the returned rows are exactly those that were enabled.
    pub async fn enable_skills_bulk(
        &self,
        ids: &[Uuid],
        audit: AuditLogInsert,
    ) -> Result<Vec<SkillRecord>, AppError> {
        let mut tx = self.pool.begin().await?;
        let rows = sqlx::query(&format!(
            "update skills set status = 'enabled', updated_at = now() \
             where id = any($1) and deleted_at is null \
             returning {SKILL_COLUMNS}"
        ))
        .bind(ids)
        .fetch_all(&mut *tx)
        .await?;
        let records = rows
            .iter()
            .map(skill_record_from_row)
            .collect::<Result<Vec<_>, _>>()?;
        commit_with_audit(tx, audit).await?;
        Ok(records)
    }

    /// Imports every parsed operation as a `skills` row (status defaults to `draft`) plus
    /// its `skill_http_executors` row, in one transaction — all-or-nothing, so a document
    /// that fails partway (for instance a `skill_key` collision with an existing row)
    /// leaves nothing behind rather than a half-imported registry.
    ///
    /// `base_url` and `allowed_host` are shared by every operation in `operations` — one
    /// import is always relative to one already-SSRF-validated server URL (the caller,
    /// `AgentPlatformService::import_skills`, validates it exactly once before calling
    /// this).
    pub async fn import_operations(
        &self,
        operations: &[ParsedOperation],
        base_url: &str,
        allowed_host: &str,
        audit: AuditLogInsert,
    ) -> Result<(Vec<SkillRecord>, Vec<SkillHttpExecutorRecord>), AppError> {
        let mut tx = self.pool.begin().await?;
        let mut skills = Vec::with_capacity(operations.len());
        let mut executors = Vec::with_capacity(operations.len());
        for operation in operations {
            let skill_id = Uuid::now_v7();
            let skill_row = sqlx::query(&format!(
                "insert into skills (id, skill_key, display_name, description, kind, \
                 params_schema, tags, metadata) values ($1, $2, $3, $4, 'tool', $5, $6, '{{}}') \
                 returning {SKILL_COLUMNS}"
            ))
            .bind(skill_id)
            .bind(&operation.skill_key)
            .bind(&operation.display_name)
            .bind(&operation.description)
            .bind(&operation.params_schema)
            .bind(&operation.tags)
            .fetch_one(&mut *tx)
            .await
            .map_err(skill_key_conflict_on_unique_violation)?;
            skills.push(skill_record_from_row(&skill_row)?);

            let url_template = format!("{base_url}{}", operation.path);
            let executor_row = sqlx::query(&format!(
                "insert into skill_http_executors (skill_id, method, url_template, \
                 allowed_host, header_template) values ($1, $2, $3, $4, '{{}}') \
                 returning {EXECUTOR_COLUMNS}"
            ))
            .bind(skill_id)
            .bind(http_method_to_db(&operation.method))
            .bind(&url_template)
            .bind(allowed_host)
            .fetch_one(&mut *tx)
            .await?;
            executors.push(skill_http_executor_record_from_row(&executor_row)?);
        }
        commit_with_audit(tx, audit).await?;
        Ok((skills, executors))
    }

    pub async fn get_executor(&self, skill_id: Uuid) -> Result<SkillHttpExecutorRecord, AppError> {
        let row = sqlx::query(&format!(
            "select {EXECUTOR_COLUMNS} from skill_http_executors where skill_id = $1"
        ))
        .bind(skill_id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| executor_not_found(skill_id))?;
        skill_http_executor_record_from_row(&row)
    }

    pub async fn list_executors(
        &self,
        cursor: Option<ListCursor>,
        limit: i64,
    ) -> Result<Vec<SkillHttpExecutorRecord>, AppError> {
        let rows = sqlx::query(&format!(
            "select {EXECUTOR_COLUMNS} from skill_http_executors \
             where ($1::timestamptz is null or (created_at, skill_id) < ($1::timestamptz, $2::uuid)) \
             order by created_at desc, skill_id desc limit $3"
        ))
        .bind(cursor.map(|c| c.ts))
        .bind(cursor.map(|c| c.id))
        .bind(over_fetch_limit(limit))
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(skill_http_executor_record_from_row)
            .collect()
    }

    /// `expected_updated_at` is this table's `If-Match` basis — see
    /// `domain::SkillHttpExecutorRecord`'s doc comment. `new_url_template`/`new_allowed_host`
    /// are `Some` together exactly when the caller validated a new `url_template` through
    /// SSRF and re-derived its host; passing them separately from `patch.url_template`
    /// keeps this repository from ever writing an `allowed_host` the service layer did not
    /// itself compute from a validated URL.
    pub async fn patch_executor(
        &self,
        skill_id: Uuid,
        expected_updated_at: DateTime<Utc>,
        patch: &SkillHttpExecutorPatchRequest,
        new_url_template: Option<&str>,
        new_allowed_host: Option<&str>,
        audit: AuditLogInsert,
    ) -> Result<SkillHttpExecutorRecord, AppError> {
        let mut tx = self.pool.begin().await?;
        let current_updated_at = sqlx::query_scalar::<_, DateTime<Utc>>(
            "select updated_at from skill_http_executors where skill_id = $1 for update",
        )
        .bind(skill_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| executor_not_found(skill_id))?;
        if current_updated_at != expected_updated_at {
            return Err(version_conflict());
        }
        let row = sqlx::query(&format!(
            "update skill_http_executors set \
                method = coalesce($2, method), \
                url_template = coalesce($3, url_template), \
                allowed_host = coalesce($4, allowed_host), \
                header_template = coalesce($5, header_template), \
                credential_id = coalesce($6, credential_id), \
                timeout_ms = coalesce($7, timeout_ms), \
                response_schema = coalesce($8, response_schema), \
                updated_at = now() \
             where skill_id = $1 returning {EXECUTOR_COLUMNS}"
        ))
        .bind(skill_id)
        .bind(patch.method.as_ref().map(http_method_to_db))
        .bind(new_url_template)
        .bind(new_allowed_host)
        .bind(&patch.header_template)
        .bind(patch.credential_id)
        .bind(patch.timeout_ms)
        .bind(&patch.response_schema)
        .fetch_one(&mut *tx)
        .await?;
        let record = skill_http_executor_record_from_row(&row)?;
        commit_with_audit(tx, audit).await?;
        Ok(record)
    }

    pub async fn delete_executor(
        &self,
        skill_id: Uuid,
        expected_updated_at: DateTime<Utc>,
        audit: AuditLogInsert,
    ) -> Result<(), AppError> {
        let mut tx = self.pool.begin().await?;
        let current_updated_at = sqlx::query_scalar::<_, DateTime<Utc>>(
            "select updated_at from skill_http_executors where skill_id = $1 for update",
        )
        .bind(skill_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| executor_not_found(skill_id))?;
        if current_updated_at != expected_updated_at {
            return Err(version_conflict());
        }
        sqlx::query("delete from skill_http_executors where skill_id = $1")
            .bind(skill_id)
            .execute(&mut *tx)
            .await?;
        commit_with_audit(tx, audit).await?;
        Ok(())
    }
}

fn executor_not_found(skill_id: Uuid) -> AppError {
    AppError::coded(
        axum::http::StatusCode::NOT_FOUND,
        "executor_not_found",
        format!("skill {skill_id} has no HTTP executor"),
    )
}

/// `skills_skill_key_active_unique` is the only unique index on live `skills` rows
/// (`migrations/0031_agent_platform.sql`), so any unique violation reaching an import
/// insert is a `skill_key` collision — either against an existing skill, or (rarely, since
/// `openapi_import::parse_openapi_document` already deduplicates within one document)
/// against a key generated by a concurrent import. Mapped to the existing generic
/// `conflict` code rather than a new catalog entry: the caller's remedy is the same either
/// way — rename the colliding operation's `operationId` and re-import — so a dedicated
/// code would not tell them anything the existing one does not.
fn skill_key_conflict_on_unique_violation(error: sqlx::Error) -> AppError {
    match &error {
        sqlx::Error::Database(database) if database.is_unique_violation() => AppError::conflict(
            "conflict",
            "an imported operation's skill_key collides with an existing skill",
        ),
        _ => AppError::from(error),
    }
}

fn skill_kind_to_db(kind: &crate::domain::SkillKind) -> &'static str {
    match kind {
        crate::domain::SkillKind::Tool => "tool",
        crate::domain::SkillKind::Guard => "guard",
    }
}

/// Over-fetches by one row so the caller can compute `has_more` without a second query.
/// Duplicated from `super::runtime::over_fetch_limit`.
fn over_fetch_limit(limit: i64) -> i64 {
    limit.saturating_add(1)
}

/// Twin of `version_conflict` in `super::runtime`/`super::admin` — one wire contract, kept in
/// sync by hand across modules that own disjoint tables.
fn version_conflict() -> AppError {
    AppError::conflict(
        "resource_version_conflict",
        "resource version does not match If-Match",
    )
}

/// Twin of `lock_and_match_version` in `super::runtime`: locks the target row with
/// `select … for update`, then evaluates the caller's `If-Match` inside the same transaction as
/// the write that follows. Absent row -> `NotFound`; genuine mismatch -> `409`.
async fn lock_and_match_version(
    conn: &mut sqlx::PgConnection,
    select_version_sql: &str,
    id: Uuid,
    expected_version: i64,
    resource: String,
) -> Result<i64, AppError> {
    let current_version = sqlx::query_scalar::<_, i64>(select_version_sql)
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(|| AppError::NotFound(resource))?;
    if current_version != expected_version {
        return Err(version_conflict());
    }
    Ok(current_version)
}
