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

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use secrecy::SecretString;
use serde_json::Value;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::{
    domain::{
        AgentFlowCreateRequest, AgentFlowPatchRequest, AgentFlowRecord, AgentFlowRunRecord,
        AgentFlowStepCreateRequest, AgentFlowStepRecord, AgentFlowStepRunRecord, AgentSkillBinding,
        AuditLogInsert, EvalCaseCreateRequest, EvalCaseRecord, EvalRunRecord, EvalRunStatus,
        EvalSuiteCreateRequest, EvalSuitePatchRequest, EvalSuiteRecord, EvalTriggerKind,
        FlowRunStatus, FlowStepRunStatus, ListCursor, ResolvedCredential, SkillCreateRequest,
        SkillCredentialOutcome, SkillHttpExecutorPatchRequest, SkillHttpExecutorRecord,
        SkillPatchRequest, SkillRecord, credential_binding_permits_host,
    },
    error::AppError,
    infra::pg_rows::{
        agent_flow_record_from_row, agent_flow_run_record_from_row,
        agent_flow_step_record_from_row, agent_flow_step_run_record_from_row,
        credential_record_from_row, credential_type_to_db, eval_case_record_from_row,
        eval_run_record_from_row, eval_run_status_to_db, eval_suite_record_from_row,
        eval_trigger_kind_to_db, flow_run_status_to_db, flow_step_on_failure_to_db,
        flow_step_run_status_to_db, grading_kind_to_db, http_method_to_db, scope_type_to_db,
        skill_http_executor_record_from_row, skill_record_from_row,
    },
    orchestration::ParsedOperation,
    security::{
        CredentialAadParts, EncryptedSecret, LocalSecretCipher, SecretCipher, credential_aad,
        credential_secret_field,
    },
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

/// The column list every `eval_suites` read and write returns.
const EVAL_SUITE_COLUMNS: &str = "id, suite_key, display_name, description, status, metadata, \
     created_at, updated_at, deleted_at, version";

const EVAL_SUITE_VERSION_FOR_UPDATE: &str =
    "select version from eval_suites where id = $1 and deleted_at is null for update";

/// `eval_cases` carries no `version`/`deleted_at` — child rows have no PATCH surface
/// (migration header, `0031_agent_platform.sql`).
const EVAL_CASE_COLUMNS: &str = "id, suite_id, input, expected, grading_kind, metadata, created_at";

/// `eval_runs` is append-only; produced by execution, never written through this admin
/// surface (F2's read-only-runs decision).
const EVAL_RUN_COLUMNS: &str = "id, suite_id, agent_profile_id, trigger_kind, execution_id, \
     status, score, results, metadata, created_at, completed_at";

/// The column list every `agent_flows` read and write returns.
const FLOW_COLUMNS: &str = "id, flow_key, display_name, description, status, metadata, \
     created_at, updated_at, deleted_at, version";

const FLOW_VERSION_FOR_UPDATE: &str =
    "select version from agent_flows where id = $1 and deleted_at is null for update";

/// `agent_flow_steps` carries no `version`/`deleted_at` — same reason as [`EVAL_CASE_COLUMNS`].
/// Steps are replaced wholesale through the owning flow's `PATCH`, never addressed
/// individually.
const FLOW_STEP_COLUMNS: &str = "id, flow_id, step_key, step_order, agent_profile_id, \
     on_failure, input_mapping, metadata, created_at";

/// `agent_flow_runs` is append-only; produced by the flow orchestrator (issue #214, the
/// execution half), never written through the admin CRUD surface.
const FLOW_RUN_COLUMNS: &str = "id, flow_id, status, metadata, created_at, completed_at";

/// `agent_flow_step_runs` is append-only; one row per step attempted within a flow run.
const FLOW_STEP_RUN_COLUMNS: &str = "id, flow_run_id, step_id, execution_id, status, \
     error_summary, created_at, completed_at";

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
            let parsed_url = url::Url::parse(&url_template).map_err(|e| {
                AppError::unprocessable(
                    "invalid_openapi_spec",
                    format!("invalid url_template '{url_template}': {e}"),
                )
            })?;
            if !parsed_url.username().is_empty() || parsed_url.password().is_some() {
                return Err(AppError::unprocessable(
                    "invalid_openapi_spec",
                    format!("url_template '{url_template}' must not contain userinfo"),
                ));
            }
            let host = parsed_url.host_str().unwrap_or("");
            if !host.eq_ignore_ascii_case(allowed_host) {
                return Err(AppError::unprocessable(
                    "invalid_openapi_spec",
                    format!(
                        "url_template host '{host}' does not match allowed_host '{allowed_host}'"
                    ),
                ));
            }

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

    /// Resolves an agent profile's `skill_refs` into the rows the rig tool loop needs
    /// (issue #84), **in `skill_refs` order**.
    ///
    /// Order is part of the contract, not an accident of the query plan: `ToolSet` is an
    /// `IndexMap`, so registration order is what a provider sees and what an idempotency
    /// key over the advertised tool list would hash. Assembling in Rust from the array
    /// rather than relying on `order by` over an `any($1)` result is what makes it stable.
    ///
    /// Three cheap statements rather than one `unnest ... left join` chain: `skills` and
    /// `skill_http_executors` share four column names (`created_at`, `updated_at`, and the
    /// `id`/`skill_id` pair), so a single joined row would need an aliasing scheme that
    /// [`skill_record_from_row`] and [`skill_http_executor_record_from_row`] do not speak
    /// — and duplicating those mappers to teach them aliases is exactly the drift this
    /// module's shared column constants exist to prevent.
    ///
    /// A reference no live row answers comes back as [`AgentSkillBinding`] with
    /// `skill: None` rather than being dropped: the caller must be able to tell "this
    /// profile has no skills" from "this profile names a skill that is gone".
    pub async fn resolve_agent_skills(
        &self,
        agent_profile_id: Uuid,
    ) -> Result<Vec<AgentSkillBinding>, AppError> {
        let skill_ids = sqlx::query_scalar::<_, Vec<Uuid>>(
            "select skill_refs from agent_profiles where id = $1",
        )
        .bind(agent_profile_id)
        .fetch_optional(&self.pool)
        .await?
        .unwrap_or_default();
        if skill_ids.is_empty() {
            return Ok(Vec::new());
        }

        let skill_rows = sqlx::query(&format!(
            "select {SKILL_COLUMNS} from skills where id = any($1) and deleted_at is null"
        ))
        .bind(&skill_ids)
        .fetch_all(&self.pool)
        .await?;
        let skills = skill_rows
            .iter()
            .map(skill_record_from_row)
            .collect::<Result<Vec<_>, _>>()?;

        let executor_rows = sqlx::query(&format!(
            "select {EXECUTOR_COLUMNS} from skill_http_executors where skill_id = any($1)"
        ))
        .bind(&skill_ids)
        .fetch_all(&self.pool)
        .await?;
        let executors = executor_rows
            .iter()
            .map(skill_http_executor_record_from_row)
            .collect::<Result<Vec<_>, _>>()?;

        Ok(skill_ids
            .into_iter()
            .map(|skill_id| AgentSkillBinding {
                skill_id,
                skill: skills.iter().find(|row| row.id == skill_id).cloned(),
                executor: executors
                    .iter()
                    .find(|row| row.skill_id == skill_id)
                    .cloned(),
            })
            .collect())
    }

    /// Fetches and decrypts the `provider_credentials` row a `skill_http_executors` row
    /// references (decision 21 — skills reuse that table rather than inventing a second
    /// secret store).
    ///
    /// Deliberately **not** routed through `RuntimeRepository::resolve_runtime_credential`:
    /// that method implements the provider-scoped precedence ladder (explicit id, then
    /// user, application, tenant, global), and a skill's credential is none of those — the
    /// executor row names one exact credential id, chosen by the operator who configured
    /// the skill, and no caller-supplied scope may redirect it to a different row. The
    /// active/not-expired/not-deleted filters are the same, so a revoked or expired
    /// credential yields [`SkillCredentialOutcome::Unusable`] here just as it yields no
    /// candidate there. A soft-deleted provider is refused for the same reason: it no longer
    /// declares anything, so nothing it owns is entitled to a destination — and a credential
    /// nobody can see on the admin plane must not keep being sent by a skill.
    ///
    /// That last rule is tested for in Rust rather than joined away with
    /// `and p.deleted_at is null`, so the outcome can be
    /// [`SkillCredentialOutcome::ProviderDeleted`] instead of an indistinguishable
    /// [`SkillCredentialOutcome::Unusable`]. The behaviour is identical — nothing is
    /// decrypted either way — but the operator-facing message can then name the table that is
    /// actually wrong. `providers.id` is `credential.provider_id`'s foreign key, so the join
    /// still matches exactly one row.
    ///
    /// Never returns a secret the executor's destination is not entitled to. `allowed_host`
    /// is the executor's SSRF-validated host, and the credential's owning provider must
    /// declare the same host in its `base_url` — see
    /// [`credential_binding_permits_host`](crate::domain::credential_binding_permits_host)
    /// for why that is the rule and why a provider with no `base_url` is refused. The check
    /// runs **before** `cipher.decrypt`, and it is enforced here rather than only on the
    /// admin write path because a row stored before that rule existed is otherwise still
    /// live. The provider is joined into the same statement, so this costs no extra round
    /// trip.
    ///
    /// Returns [`SkillCredentialOutcome::Unusable`] when the row is absent or carries no
    /// usable secret. The caller decides what that means; this never falls back to an
    /// unauthenticated call.
    pub async fn resolve_skill_credential(
        &self,
        cipher: &LocalSecretCipher,
        credential_id: Uuid,
        allowed_host: &str,
    ) -> Result<SkillCredentialOutcome, AppError> {
        // Every credential column is qualified because `providers` shares eight column names
        // with `provider_credentials` (`id`, `status`, `metadata`, `display_name`, the four
        // timestamps, `version`); an unqualified list would silently bind the wrong side.
        let Some(row) = sqlx::query(
            "select c.id, c.provider_id, c.credential_type, c.scope_type, \
                    c.external_tenant_id, c.application_id, c.external_user_id, \
                    c.encryption_algorithm, c.encryption_version, c.encrypted_data_key, \
                    c.nonce, c.encrypted_payload, c.secret_fingerprint, c.masked_secret, \
                    c.status, c.priority, c.expires_at, c.last_validated_at, c.last_used_at, \
                    c.metadata, c.display_name, c.created_at, c.updated_at, c.deleted_at, \
                    c.version, p.base_url as provider_base_url, \
                    p.deleted_at as provider_deleted_at \
             from provider_credentials c \
             join providers p on p.id = c.provider_id \
             where c.id = $1 and c.status = 'active' and c.deleted_at is null \
               and (c.expires_at is null or c.expires_at > now())",
        )
        .bind(credential_id)
        .fetch_optional(&self.pool)
        .await?
        else {
            return Ok(SkillCredentialOutcome::Unusable);
        };

        // Checked before the host rule, and the inventory query in `docs/agent-platform.md`
        // classifies in the same order: repairing the `base_url` of a deleted provider fixes
        // nothing, so an operator must not be sent down that path first.
        let provider_deleted_at: Option<DateTime<Utc>> = row.try_get("provider_deleted_at")?;
        if provider_deleted_at.is_some() {
            return Ok(SkillCredentialOutcome::ProviderDeleted);
        }

        let provider_base_url: Option<String> = row.try_get("provider_base_url")?;
        if !credential_binding_permits_host(provider_base_url.as_deref(), allowed_host) {
            return Ok(SkillCredentialOutcome::HostNotEntitled);
        }

        let record = credential_record_from_row(&row)?;
        let encrypted = EncryptedSecret {
            algorithm: row.try_get("encryption_algorithm")?,
            version: row.try_get("encryption_version")?,
            key_id: String::new(),
            encrypted_data_key: row.try_get("encrypted_data_key")?,
            nonce: row.try_get("nonce")?,
            ciphertext: row.try_get("encrypted_payload")?,
        };
        let aad = credential_aad(CredentialAadParts {
            credential_id: record.id,
            provider_id: record.provider_id,
            credential_type: credential_type_to_db(&record.credential_type),
            scope_type: scope_type_to_db(&record.scope_type),
            external_tenant_id: record.external_tenant_id.as_deref(),
            application_id: record.application_id,
            external_user_id: record.external_user_id.as_deref(),
            encryption_version: record.encryption_version,
        });
        let plaintext = cipher.decrypt(&encrypted, aad.as_bytes())?;
        let config: Value = serde_json::from_slice(&plaintext)
            .map_err(|_| AppError::Config("provider credential payload is invalid".to_string()))?;
        let Some(field) = credential_secret_field(record.credential_type) else {
            return Ok(SkillCredentialOutcome::Unusable);
        };
        let Some(secret) = config
            .get(field)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        else {
            return Ok(SkillCredentialOutcome::Unusable);
        };
        Ok(SkillCredentialOutcome::Resolved(Box::new(
            ResolvedCredential {
                credential_id: record.id,
                credential_version: record.version,
                credential_type: record.credential_type,
                secret: SecretString::new(secret.to_string()),
                config,
            },
        )))
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

    // =================================================================================
    // Eval suites (issue #214, plan 12 §3 — the deferred CRUD half of PR #227's schema).
    // =================================================================================

    pub async fn create_eval_suite(
        &self,
        id: Uuid,
        request: &EvalSuiteCreateRequest,
        audit: AuditLogInsert,
    ) -> Result<EvalSuiteRecord, AppError> {
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query(&format!(
            "insert into eval_suites (id, suite_key, display_name, description, metadata) \
             values ($1, $2, $3, $4, $5) returning {EVAL_SUITE_COLUMNS}"
        ))
        .bind(id)
        .bind(&request.suite_key)
        .bind(&request.display_name)
        .bind(&request.description)
        .bind(&request.metadata)
        .fetch_one(&mut *tx)
        .await?;
        let record = eval_suite_record_from_row(&row)?;
        commit_with_audit(tx, audit).await?;
        Ok(record)
    }

    pub async fn list_eval_suites(
        &self,
        cursor: Option<ListCursor>,
        limit: i64,
    ) -> Result<Vec<EvalSuiteRecord>, AppError> {
        let rows = sqlx::query(&format!(
            "select {EVAL_SUITE_COLUMNS} from eval_suites where deleted_at is null \
             and ($1::timestamptz is null or (created_at, id) < ($1::timestamptz, $2::uuid)) \
             order by created_at desc, id desc limit $3"
        ))
        .bind(cursor.map(|c| c.ts))
        .bind(cursor.map(|c| c.id))
        .bind(over_fetch_limit(limit))
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(eval_suite_record_from_row).collect()
    }

    pub async fn get_eval_suite(&self, id: Uuid) -> Result<EvalSuiteRecord, AppError> {
        let row = sqlx::query(&format!(
            "select {EVAL_SUITE_COLUMNS} from eval_suites where id = $1 and deleted_at is null"
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("eval suite {id}")))?;
        eval_suite_record_from_row(&row)
    }

    pub async fn patch_eval_suite(
        &self,
        id: Uuid,
        expected_version: i64,
        request: &EvalSuitePatchRequest,
        audit: AuditLogInsert,
    ) -> Result<EvalSuiteRecord, AppError> {
        let mut tx = self.pool.begin().await?;
        let current_version = lock_and_match_version(
            &mut tx,
            EVAL_SUITE_VERSION_FOR_UPDATE,
            id,
            expected_version,
            format!("eval suite {id}"),
        )
        .await?;
        let row = sqlx::query(&format!(
            "update eval_suites set \
                display_name = coalesce($2, display_name), \
                description = coalesce($3, description), \
                metadata = coalesce($4, metadata), \
                updated_at = now() \
             where id = $1 and deleted_at is null and version = $5 returning {EVAL_SUITE_COLUMNS}"
        ))
        .bind(id)
        .bind(&request.display_name)
        .bind(&request.description)
        .bind(&request.metadata)
        .bind(current_version)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(version_conflict)?;
        let record = eval_suite_record_from_row(&row)?;
        commit_with_audit(tx, audit).await?;
        Ok(record)
    }

    pub async fn soft_delete_eval_suite(
        &self,
        id: Uuid,
        expected_version: i64,
        audit: AuditLogInsert,
    ) -> Result<(), AppError> {
        let mut tx = self.pool.begin().await?;
        let current_version = lock_and_match_version(
            &mut tx,
            EVAL_SUITE_VERSION_FOR_UPDATE,
            id,
            expected_version,
            format!("eval suite {id}"),
        )
        .await?;
        let result = sqlx::query(
            "update eval_suites set deleted_at = now(), updated_at = now() \
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

    // =================================================================================
    // Eval cases — a child of one suite; no version/PATCH surface (migration header).
    // =================================================================================

    pub async fn create_eval_case(
        &self,
        id: Uuid,
        suite_id: Uuid,
        request: &EvalCaseCreateRequest,
        audit: AuditLogInsert,
    ) -> Result<EvalCaseRecord, AppError> {
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query(&format!(
            "insert into eval_cases (id, suite_id, input, expected, grading_kind, metadata) \
             values ($1, $2, $3, $4, $5, $6) returning {EVAL_CASE_COLUMNS}"
        ))
        .bind(id)
        .bind(suite_id)
        .bind(&request.input)
        .bind(&request.expected)
        .bind(grading_kind_to_db(&request.grading_kind))
        .bind(&request.metadata)
        .fetch_one(&mut *tx)
        .await?;
        let record = eval_case_record_from_row(&row)?;
        commit_with_audit(tx, audit).await?;
        Ok(record)
    }

    pub async fn list_eval_cases(
        &self,
        suite_id: Uuid,
        cursor: Option<ListCursor>,
        limit: i64,
    ) -> Result<Vec<EvalCaseRecord>, AppError> {
        let rows = sqlx::query(&format!(
            "select {EVAL_CASE_COLUMNS} from eval_cases where suite_id = $1 \
             and ($2::timestamptz is null or (created_at, id) < ($2::timestamptz, $3::uuid)) \
             order by created_at desc, id desc limit $4"
        ))
        .bind(suite_id)
        .bind(cursor.map(|c| c.ts))
        .bind(cursor.map(|c| c.id))
        .bind(over_fetch_limit(limit))
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(eval_case_record_from_row).collect()
    }

    /// Hard delete, scoped to `suite_id` so a case id from one suite can never delete a row
    /// under another — the same "the path's ownership is part of the predicate" discipline
    /// every other nested-resource delete in this codebase follows.
    pub async fn delete_eval_case(
        &self,
        suite_id: Uuid,
        case_id: Uuid,
        audit: AuditLogInsert,
    ) -> Result<(), AppError> {
        let mut tx = self.pool.begin().await?;
        let result = sqlx::query("delete from eval_cases where id = $1 and suite_id = $2")
            .bind(case_id)
            .bind(suite_id)
            .execute(&mut *tx)
            .await?;
        if result.rows_affected() == 0 {
            return Err(AppError::NotFound(format!("eval case {case_id}")));
        }
        commit_with_audit(tx, audit).await?;
        Ok(())
    }

    // =================================================================================
    // Eval runs — read-only via the admin CRUD surface; produced by the offline eval
    // runner (issue #214, the execution half) through `insert_eval_run`.
    // =================================================================================

    pub async fn list_eval_runs(
        &self,
        suite_id: Uuid,
        cursor: Option<ListCursor>,
        limit: i64,
    ) -> Result<Vec<EvalRunRecord>, AppError> {
        let rows = sqlx::query(&format!(
            "select {EVAL_RUN_COLUMNS} from eval_runs where suite_id = $1 \
             and ($2::timestamptz is null or (created_at, id) < ($2::timestamptz, $3::uuid)) \
             order by created_at desc, id desc limit $4"
        ))
        .bind(suite_id)
        .bind(cursor.map(|c| c.ts))
        .bind(cursor.map(|c| c.id))
        .bind(over_fetch_limit(limit))
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(eval_run_record_from_row).collect()
    }

    /// Every case of a suite, in creation order — the offline eval runner grades them all in
    /// one pass, so it reads the whole set rather than a keyset page.
    pub async fn all_eval_cases(&self, suite_id: Uuid) -> Result<Vec<EvalCaseRecord>, AppError> {
        let rows = sqlx::query(&format!(
            "select {EVAL_CASE_COLUMNS} from eval_cases where suite_id = $1 \
             order by created_at asc, id asc"
        ))
        .bind(suite_id)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(eval_case_record_from_row).collect()
    }

    /// Writes one terminal `eval_runs` row for a completed offline run. `trigger_kind` is
    /// always `offline_manual` on this path (decision 14 — no CI or online sampling yet);
    /// `results` carries the per-case pass/fail detail and `score` the pass rate. `completed_at`
    /// is stamped `now()` because the offline runner is inline: the row is written once, in its
    /// final state.
    #[allow(clippy::too_many_arguments)]
    pub async fn insert_eval_run(
        &self,
        id: Uuid,
        suite_id: Uuid,
        agent_profile_id: Uuid,
        trigger_kind: EvalTriggerKind,
        status: EvalRunStatus,
        score: Option<f64>,
        results: &Value,
        metadata: &Value,
    ) -> Result<EvalRunRecord, AppError> {
        let row = sqlx::query(&format!(
            "insert into eval_runs (id, suite_id, agent_profile_id, trigger_kind, status, \
             score, results, metadata, completed_at) \
             values ($1, $2, $3, $4, $5, $6, $7, $8, now()) returning {EVAL_RUN_COLUMNS}"
        ))
        .bind(id)
        .bind(suite_id)
        .bind(agent_profile_id)
        .bind(eval_trigger_kind_to_db(&trigger_kind))
        .bind(eval_run_status_to_db(&status))
        .bind(score)
        .bind(results)
        .bind(metadata)
        .fetch_one(&self.pool)
        .await?;
        eval_run_record_from_row(&row)
    }

    // =================================================================================
    // Flows (issue #214, plan 12 §3). Steps live inside these methods, not as a separate
    // CRUD surface — see `domain::AgentFlowRecord`'s doc comment.
    // =================================================================================

    pub async fn create_flow(
        &self,
        id: Uuid,
        request: &AgentFlowCreateRequest,
        audit: AuditLogInsert,
    ) -> Result<AgentFlowRecord, AppError> {
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query(&format!(
            "insert into agent_flows (id, flow_key, display_name, description, metadata) \
             values ($1, $2, $3, $4, $5) returning {FLOW_COLUMNS}"
        ))
        .bind(id)
        .bind(&request.flow_key)
        .bind(&request.display_name)
        .bind(&request.description)
        .bind(&request.metadata)
        .fetch_one(&mut *tx)
        .await?;
        let mut record = agent_flow_record_from_row(&row)?;
        record.steps = insert_flow_steps(&mut tx, id, &request.steps).await?;
        commit_with_audit(tx, audit).await?;
        Ok(record)
    }

    pub async fn list_flows(
        &self,
        cursor: Option<ListCursor>,
        limit: i64,
    ) -> Result<Vec<AgentFlowRecord>, AppError> {
        let rows = sqlx::query(&format!(
            "select {FLOW_COLUMNS} from agent_flows where deleted_at is null \
             and ($1::timestamptz is null or (created_at, id) < ($1::timestamptz, $2::uuid)) \
             order by created_at desc, id desc limit $3"
        ))
        .bind(cursor.map(|c| c.ts))
        .bind(cursor.map(|c| c.id))
        .bind(over_fetch_limit(limit))
        .fetch_all(&self.pool)
        .await?;
        let mut records = rows
            .iter()
            .map(agent_flow_record_from_row)
            .collect::<Result<Vec<_>, _>>()?;
        let flow_ids: Vec<Uuid> = records.iter().map(|record| record.id).collect();
        let mut steps_by_flow = self.fetch_steps_for_flows(&flow_ids).await?;
        for record in &mut records {
            record.steps = steps_by_flow.remove(&record.id).unwrap_or_default();
        }
        Ok(records)
    }

    pub async fn get_flow(&self, id: Uuid) -> Result<AgentFlowRecord, AppError> {
        let row = sqlx::query(&format!(
            "select {FLOW_COLUMNS} from agent_flows where id = $1 and deleted_at is null"
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("flow {id}")))?;
        let mut record = agent_flow_record_from_row(&row)?;
        record.steps = self.fetch_flow_steps(id).await?;
        Ok(record)
    }

    /// When `request.steps` is `Some`, the flow's entire step list is replaced atomically
    /// (delete-then-reinsert, same transaction as the flow row's own `UPDATE`); when `None`
    /// the existing steps are left untouched and simply re-read to build the response.
    pub async fn patch_flow(
        &self,
        id: Uuid,
        expected_version: i64,
        request: &AgentFlowPatchRequest,
        audit: AuditLogInsert,
    ) -> Result<AgentFlowRecord, AppError> {
        let mut tx = self.pool.begin().await?;
        let current_version = lock_and_match_version(
            &mut tx,
            FLOW_VERSION_FOR_UPDATE,
            id,
            expected_version,
            format!("flow {id}"),
        )
        .await?;
        let row = sqlx::query(&format!(
            "update agent_flows set \
                display_name = coalesce($2, display_name), \
                description = coalesce($3, description), \
                metadata = coalesce($4, metadata), \
                updated_at = now() \
             where id = $1 and deleted_at is null and version = $5 returning {FLOW_COLUMNS}"
        ))
        .bind(id)
        .bind(&request.display_name)
        .bind(&request.description)
        .bind(&request.metadata)
        .bind(current_version)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(version_conflict)?;
        let mut record = agent_flow_record_from_row(&row)?;
        record.steps = if let Some(steps) = &request.steps {
            sqlx::query("delete from agent_flow_steps where flow_id = $1")
                .bind(id)
                .execute(&mut *tx)
                .await?;
            insert_flow_steps(&mut tx, id, steps).await?
        } else {
            fetch_flow_steps_with_connection(&mut tx, id).await?
        };
        commit_with_audit(tx, audit).await?;
        Ok(record)
    }

    pub async fn soft_delete_flow(
        &self,
        id: Uuid,
        expected_version: i64,
        audit: AuditLogInsert,
    ) -> Result<(), AppError> {
        let mut tx = self.pool.begin().await?;
        let current_version = lock_and_match_version(
            &mut tx,
            FLOW_VERSION_FOR_UPDATE,
            id,
            expected_version,
            format!("flow {id}"),
        )
        .await?;
        let result = sqlx::query(
            "update agent_flows set deleted_at = now(), updated_at = now() \
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

    async fn fetch_flow_steps(&self, flow_id: Uuid) -> Result<Vec<AgentFlowStepRecord>, AppError> {
        let rows = sqlx::query(&format!(
            "select {FLOW_STEP_COLUMNS} from agent_flow_steps where flow_id = $1 \
             order by step_order, id"
        ))
        .bind(flow_id)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(agent_flow_step_record_from_row).collect()
    }

    /// [`fetch_flow_steps`](Self::fetch_flow_steps)'s batched twin for [`list_flows`](Self::list_flows):
    /// one query for every step of every flow on the page, grouped in Rust, so a page of `N`
    /// flows costs two queries total rather than `N + 1`.
    async fn fetch_steps_for_flows(
        &self,
        flow_ids: &[Uuid],
    ) -> Result<HashMap<Uuid, Vec<AgentFlowStepRecord>>, AppError> {
        if flow_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let rows = sqlx::query(&format!(
            "select {FLOW_STEP_COLUMNS} from agent_flow_steps where flow_id = any($1) \
             order by flow_id, step_order, id"
        ))
        .bind(flow_ids)
        .fetch_all(&self.pool)
        .await?;
        let mut grouped: HashMap<Uuid, Vec<AgentFlowStepRecord>> = HashMap::new();
        for row in &rows {
            let step = agent_flow_step_record_from_row(row)?;
            grouped.entry(step.flow_id).or_default().push(step);
        }
        Ok(grouped)
    }

    // =================================================================================
    // Flow runs — read-only via the admin CRUD surface; produced by the flow orchestrator
    // (issue #214, the execution half) through the insert/finalize helpers below.
    // =================================================================================

    pub async fn list_flow_runs(
        &self,
        flow_id: Uuid,
        cursor: Option<ListCursor>,
        limit: i64,
    ) -> Result<Vec<AgentFlowRunRecord>, AppError> {
        let rows = sqlx::query(&format!(
            "select {FLOW_RUN_COLUMNS} from agent_flow_runs where flow_id = $1 \
             and ($2::timestamptz is null or (created_at, id) < ($2::timestamptz, $3::uuid)) \
             order by created_at desc, id desc limit $4"
        ))
        .bind(flow_id)
        .bind(cursor.map(|c| c.ts))
        .bind(cursor.map(|c| c.id))
        .bind(over_fetch_limit(limit))
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(agent_flow_run_record_from_row).collect()
    }

    /// Opens a flow run in the `running` state before any step executes, so a concurrent
    /// `GET .../flows/{id}/runs` can see the run in flight and a run left `running` is the
    /// honest signal that the orchestrator died mid-flow.
    pub async fn insert_flow_run(
        &self,
        id: Uuid,
        flow_id: Uuid,
        metadata: &Value,
    ) -> Result<AgentFlowRunRecord, AppError> {
        let row = sqlx::query(&format!(
            "insert into agent_flow_runs (id, flow_id, metadata) values ($1, $2, $3) \
             returning {FLOW_RUN_COLUMNS}"
        ))
        .bind(id)
        .bind(flow_id)
        .bind(metadata)
        .fetch_one(&self.pool)
        .await?;
        agent_flow_run_record_from_row(&row)
    }

    /// Moves a flow run to a terminal state (`completed`/`failed`/`cancelled`) and stamps
    /// `completed_at`.
    pub async fn finalize_flow_run(
        &self,
        id: Uuid,
        status: FlowRunStatus,
    ) -> Result<AgentFlowRunRecord, AppError> {
        let row = sqlx::query(&format!(
            "update agent_flow_runs set status = $2, completed_at = now() where id = $1 \
             returning {FLOW_RUN_COLUMNS}"
        ))
        .bind(id)
        .bind(flow_run_status_to_db(&status))
        .fetch_one(&self.pool)
        .await?;
        agent_flow_run_record_from_row(&row)
    }

    /// Opens one step run in the `running` state.
    pub async fn insert_flow_step_run(
        &self,
        id: Uuid,
        flow_run_id: Uuid,
        step_id: Uuid,
    ) -> Result<AgentFlowStepRunRecord, AppError> {
        let row = sqlx::query(&format!(
            "insert into agent_flow_step_runs (id, flow_run_id, step_id, status) \
             values ($1, $2, $3, 'running') returning {FLOW_STEP_RUN_COLUMNS}"
        ))
        .bind(id)
        .bind(flow_run_id)
        .bind(step_id)
        .fetch_one(&self.pool)
        .await?;
        agent_flow_step_run_record_from_row(&row)
    }

    /// Moves a step run to a terminal state, correlating it to the underlying pipeline
    /// `execution_id` and carrying a sanitized `error_summary` on the failure path.
    pub async fn finalize_flow_step_run(
        &self,
        id: Uuid,
        status: FlowStepRunStatus,
        execution_id: Option<Uuid>,
        error_summary: Option<&str>,
    ) -> Result<AgentFlowStepRunRecord, AppError> {
        let row = sqlx::query(&format!(
            "update agent_flow_step_runs set status = $2, execution_id = $3, \
             error_summary = $4, completed_at = now() where id = $1 \
             returning {FLOW_STEP_RUN_COLUMNS}"
        ))
        .bind(id)
        .bind(flow_step_run_status_to_db(&status))
        .bind(execution_id)
        .bind(error_summary)
        .fetch_one(&self.pool)
        .await?;
        agent_flow_step_run_record_from_row(&row)
    }

    /// Every step run of a flow run, in creation order — the run response returns them so the
    /// caller sees per-step results without a second round trip.
    pub async fn list_flow_step_runs(
        &self,
        flow_run_id: Uuid,
    ) -> Result<Vec<AgentFlowStepRunRecord>, AppError> {
        let rows = sqlx::query(&format!(
            "select {FLOW_STEP_RUN_COLUMNS} from agent_flow_step_runs where flow_run_id = $1 \
             order by created_at asc, id asc"
        ))
        .bind(flow_run_id)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(agent_flow_step_run_record_from_row)
            .collect()
    }
}

/// Inserts `steps` (each with a fresh id) for `flow_id` inside the caller's transaction and
/// returns them in `step_order` order. Shared by `create_flow` and `patch_flow`'s
/// steps-replace path so the insert shape cannot drift between the two.
async fn insert_flow_steps(
    conn: &mut sqlx::PgConnection,
    flow_id: Uuid,
    steps: &[AgentFlowStepCreateRequest],
) -> Result<Vec<AgentFlowStepRecord>, AppError> {
    let mut inserted = Vec::with_capacity(steps.len());
    for step in steps {
        let row = sqlx::query(&format!(
            "insert into agent_flow_steps (id, flow_id, step_key, step_order, agent_profile_id, \
             on_failure, input_mapping, metadata) values ($1, $2, $3, $4, $5, $6, $7, $8) \
             returning {FLOW_STEP_COLUMNS}"
        ))
        .bind(Uuid::now_v7())
        .bind(flow_id)
        .bind(&step.step_key)
        .bind(step.step_order)
        .bind(step.agent_profile_id)
        .bind(flow_step_on_failure_to_db(&step.on_failure))
        .bind(&step.input_mapping)
        .bind(&step.metadata)
        .fetch_one(&mut *conn)
        .await?;
        inserted.push(agent_flow_step_record_from_row(&row)?);
    }
    inserted.sort_by_key(|step| step.step_order);
    Ok(inserted)
}

/// [`PgAgentPlatformRepository::fetch_flow_steps`]'s in-transaction twin, for `patch_flow`'s
/// steps-unchanged path (must read inside the same transaction as the row lock it is
/// serialized against).
async fn fetch_flow_steps_with_connection(
    conn: &mut sqlx::PgConnection,
    flow_id: Uuid,
) -> Result<Vec<AgentFlowStepRecord>, AppError> {
    let rows = sqlx::query(&format!(
        "select {FLOW_STEP_COLUMNS} from agent_flow_steps where flow_id = $1 order by step_order, id"
    ))
    .bind(flow_id)
    .fetch_all(&mut *conn)
    .await?;
    rows.iter().map(agent_flow_step_record_from_row).collect()
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
