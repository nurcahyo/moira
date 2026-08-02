use async_trait::async_trait;
use chrono::{Duration, Utc};
use serde_json::{Value, json};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::{
    domain::{
        ApplicationExecutionPolicyPutRequest, ApplicationExecutionPolicyRecord, ExecutionQuery,
        IdempotencyRecord, PublicExecutionSummary, PublicModelCapabilities, PublicModelResource,
        PublicResponseRecord, PublicRouteResource, PublicUsageRecord, PublicUsageSummary,
        ResponsePersistenceMode, UsageQuery,
    },
    error::AppError,
    infra::pg_rows::{
        application_execution_policy_record_from_row, execution_failure_class_from_db,
        provider_type_from_db, public_response_record_from_row, response_persistence_mode_to_db,
    },
    security::IdempotencyHasher,
};

use super::policy_row::get_or_create_policy_row;

/// The execution-policy projection, spelled out rather than `*`.
///
/// Named because [`get_or_create_policy_row`] interpolates it into two statements and both
/// must agree: a `select` and a `returning` that disagreed on columns would fail only on
/// whichever path happened to run second.
const APPLICATION_EXECUTION_POLICY_COLUMNS: &str = "id, application_id, responses_enabled, \
     streaming_enabled, tools_enabled, vision_enabled, structured_output_enabled, \
     caller_system_instructions_allowed, model_overrides_allowed, route_overrides_allowed, \
     provider_overrides_allowed, credential_overrides_allowed, timeout_overrides_allowed, \
     persistence_mode, response_retention_seconds, maximum_request_bytes, maximum_input_items, \
     maximum_output_tokens, maximum_timeout_ms, rate_limit_requests_per_minute, \
     rate_limit_streams_per_minute, metadata, updated_at, version";

#[derive(Debug, Clone)]
pub struct PgPublicRepository {
    pool: PgPool,
}

#[derive(Debug, Clone)]
pub struct PublicAccess {
    pub privileged: bool,
    pub application_id: Option<Uuid>,
    pub external_tenant_id: Option<String>,
    pub external_user_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ResponseStartedInsert {
    pub id: Uuid,
    pub execution_id: Uuid,
    pub request_id: String,
    pub application_id: Option<Uuid>,
    pub external_tenant_id: Option<String>,
    pub external_user_id: Option<String>,
    pub conversation_public_id: Option<String>,
    pub metadata: Value,
    pub expires_at: Option<chrono::DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct ResponseTerminalUpdate {
    pub route_id: Option<Uuid>,
    pub provider_id: Option<Uuid>,
    pub provider_model_id: Option<Uuid>,
    pub output_summary: Value,
    pub usage: PublicUsageSummary,
    pub failure_class: Option<String>,
    pub failure_message: Option<String>,
    pub output_persisted: bool,
}

#[derive(Debug, Clone)]
pub enum IdempotencyClaim {
    Claimed,
    Replay(IdempotencyRecord),
}

/// The public-API persistence surface: application execution policy, the public idempotency
/// envelope, response/execution lifecycle rows, and the caller-scoped read models.
///
/// Extracted as a trait (plan 06, Module 8 / P2-3) so `PublicExecutionService` can be unit-tested
/// against a fake instead of a live Postgres. Mirrors [`AdminRepository`](super::AdminRepository):
/// the trait carries the documentation, the `#[async_trait] impl` below carries only SQL.
///
/// Every `*_authorized` / `*_visible_*` method takes a [`PublicAccess`] and applies the caller's
/// scoping **in SQL**. That is deliberate and load-bearing: an implementation that returned rows
/// outside the supplied access scope would be a cross-tenant disclosure, not a bug in the caller.
#[async_trait]
pub trait PublicRepository: Send + Sync {
    async fn get_or_create_application_execution_policy(
        &self,
        application_id: Uuid,
    ) -> Result<ApplicationExecutionPolicyRecord, AppError>;

    /// Upserts the policy under a SQL-level optimistic-concurrency check.
    ///
    /// The version comparison **must** live in the same transaction as the write. The previous
    /// shape read the current version on one pooled connection, compared it in Rust, and then
    /// issued an unconditional `on conflict do update` on a possibly different connection:
    /// two writers holding the same currently-valid `If-Match` both passed the comparison and
    /// both wrote, so one update was silently lost and neither caller saw a conflict.
    ///
    /// This follows the pattern already established by
    /// `PgAdminRepository`'s `rotate_credential`: `select … for update` inside a transaction to
    /// serialise the writers, and — belt and braces, because the row lock alone is easy to
    /// regress — the `update` itself carries `and version = $22`, with zero affected rows
    /// mapped onto the same `409 resource_version_conflict` envelope every other versioned
    /// endpoint already returns.
    async fn put_application_execution_policy(
        &self,
        application_id: Uuid,
        expected_version: Option<i64>,
        request: &ApplicationExecutionPolicyPutRequest,
    ) -> Result<ApplicationExecutionPolicyRecord, AppError>;

    async fn claim_idempotency(
        &self,
        record: &IdempotencyRecord,
    ) -> Result<IdempotencyClaim, AppError>;

    async fn get_idempotency_record(
        &self,
        key_hash: &str,
        actor_fingerprint: &str,
        operation: &str,
    ) -> Result<Option<IdempotencyRecord>, AppError>;

    async fn finish_idempotency(
        &self,
        key_hash: &str,
        actor_fingerprint: &str,
        operation: &str,
        response_status: i32,
        response_body: &Value,
        resource_id: Option<&str>,
    ) -> Result<(), AppError>;

    async fn insert_response_started(
        &self,
        insert: &ResponseStartedInsert,
    ) -> Result<PublicResponseRecord, AppError>;

    async fn complete_response(
        &self,
        id: Uuid,
        update: &ResponseTerminalUpdate,
    ) -> Result<PublicResponseRecord, AppError>;

    async fn fail_response(
        &self,
        id: Uuid,
        update: &ResponseTerminalUpdate,
    ) -> Result<PublicResponseRecord, AppError>;

    async fn cancel_response(
        &self,
        id: Uuid,
        update: &ResponseTerminalUpdate,
    ) -> Result<PublicResponseRecord, AppError>;

    async fn find_response_authorized(
        &self,
        id: Uuid,
        access: &PublicAccess,
    ) -> Result<PublicResponseRecord, AppError>;

    async fn find_execution_authorized(
        &self,
        execution_id: Uuid,
        access: &PublicAccess,
    ) -> Result<PublicExecutionSummary, AppError>;

    async fn list_executions_authorized(
        &self,
        access: &PublicAccess,
        query: &ExecutionQuery,
    ) -> Result<Vec<PublicExecutionSummary>, AppError>;

    async fn list_usage_authorized(
        &self,
        access: &PublicAccess,
        query: &UsageQuery,
    ) -> Result<Vec<PublicUsageRecord>, AppError>;

    async fn list_visible_models(
        &self,
        access: &PublicAccess,
        limit: i64,
    ) -> Result<Vec<PublicModelResource>, AppError>;

    async fn find_visible_model_id_by_key(
        &self,
        access: &PublicAccess,
        model_key: &str,
    ) -> Result<Option<Uuid>, AppError>;

    async fn list_visible_routes(
        &self,
        access: &PublicAccess,
        limit: i64,
    ) -> Result<Vec<PublicRouteResource>, AppError>;
}

impl PgPublicRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl PublicRepository for PgPublicRepository {
    /// F47. This member never had the other four's write amplification — it already read
    /// first — but its insert carried no `on conflict` clause, so two concurrent first
    /// requests for a new application raced and one failed with a duplicate-key error on
    /// the hot path of `POST /v1/responses`. [`super::policy_row`] closes that without
    /// reintroducing a write on the steady-state path.
    async fn get_or_create_application_execution_policy(
        &self,
        application_id: Uuid,
    ) -> Result<ApplicationExecutionPolicyRecord, AppError> {
        let row = get_or_create_policy_row(
            &self.pool,
            "application_execution_policies",
            APPLICATION_EXECUTION_POLICY_COLUMNS,
            application_id,
        )
        .await?;
        application_execution_policy_record_from_row(&row)
    }

    async fn put_application_execution_policy(
        &self,
        application_id: Uuid,
        expected_version: Option<i64>,
        request: &ApplicationExecutionPolicyPutRequest,
    ) -> Result<ApplicationExecutionPolicyRecord, AppError> {
        let persistence_mode = request
            .persistence_mode
            .map(response_persistence_mode_to_db);

        let mut tx = self.pool.begin().await?;

        // Materialise the defaulted row if this application has never had a policy, so the
        // first write is still an upsert and `select … for update` below always has a row to
        // lock. The column defaults are identical to the literals the old insert branch
        // coalesced against, so a first write lands on exactly the same values.
        sqlx::query(
            r#"
            insert into application_execution_policies (application_id)
            values ($1)
            on conflict (application_id) do nothing
            "#,
        )
        .bind(application_id)
        .execute(&mut *tx)
        .await?;

        let current_version = sqlx::query_scalar::<_, i64>(
            "select version from application_execution_policies where application_id = $1 for update",
        )
        .bind(application_id)
        .fetch_one(&mut *tx)
        .await?;

        if expected_version.is_some_and(|expected| expected != current_version) {
            return Err(AppError::conflict(
                "resource_version_conflict",
                "resource version does not match If-Match",
            ));
        }

        let row = sqlx::query(
            r#"
            update application_execution_policies set
                responses_enabled = coalesce($2, application_execution_policies.responses_enabled),
                streaming_enabled = coalesce($3, application_execution_policies.streaming_enabled),
                tools_enabled = coalesce($4, application_execution_policies.tools_enabled),
                vision_enabled = coalesce($5, application_execution_policies.vision_enabled),
                structured_output_enabled = coalesce($6, application_execution_policies.structured_output_enabled),
                caller_system_instructions_allowed = coalesce($7, application_execution_policies.caller_system_instructions_allowed),
                model_overrides_allowed = coalesce($8, application_execution_policies.model_overrides_allowed),
                route_overrides_allowed = coalesce($9, application_execution_policies.route_overrides_allowed),
                provider_overrides_allowed = coalesce($10, application_execution_policies.provider_overrides_allowed),
                credential_overrides_allowed = coalesce($11, application_execution_policies.credential_overrides_allowed),
                timeout_overrides_allowed = coalesce($12, application_execution_policies.timeout_overrides_allowed),
                persistence_mode = coalesce($13, application_execution_policies.persistence_mode),
                response_retention_seconds = coalesce($14, application_execution_policies.response_retention_seconds),
                maximum_request_bytes = coalesce($15, application_execution_policies.maximum_request_bytes),
                maximum_input_items = coalesce($16, application_execution_policies.maximum_input_items),
                maximum_output_tokens = coalesce($17, application_execution_policies.maximum_output_tokens),
                maximum_timeout_ms = coalesce($18, application_execution_policies.maximum_timeout_ms),
                rate_limit_requests_per_minute = coalesce($19, application_execution_policies.rate_limit_requests_per_minute),
                rate_limit_streams_per_minute = coalesce($20, application_execution_policies.rate_limit_streams_per_minute),
                metadata = coalesce($21, application_execution_policies.metadata),
                updated_at = now()
            where application_id = $1 and version = $22
            returning id, application_id, responses_enabled, streaming_enabled, tools_enabled,
                      vision_enabled, structured_output_enabled,
                      caller_system_instructions_allowed, model_overrides_allowed,
                      route_overrides_allowed, provider_overrides_allowed,
                      credential_overrides_allowed, timeout_overrides_allowed,
                      persistence_mode, response_retention_seconds, maximum_request_bytes,
                      maximum_input_items, maximum_output_tokens, maximum_timeout_ms,
                      rate_limit_requests_per_minute, rate_limit_streams_per_minute,
                      metadata, updated_at, version
            "#,
        )
        .bind(application_id)
        .bind(request.responses_enabled)
        .bind(request.streaming_enabled)
        .bind(request.tools_enabled)
        .bind(request.vision_enabled)
        .bind(request.structured_output_enabled)
        .bind(request.caller_system_instructions_allowed)
        .bind(request.model_overrides_allowed)
        .bind(request.route_overrides_allowed)
        .bind(request.provider_overrides_allowed)
        .bind(request.credential_overrides_allowed)
        .bind(request.timeout_overrides_allowed)
        .bind(persistence_mode)
        .bind(request.response_retention_seconds)
        .bind(request.maximum_request_bytes)
        .bind(request.maximum_input_items)
        .bind(request.maximum_output_tokens)
        .bind(request.maximum_timeout_ms)
        .bind(request.rate_limit_requests_per_minute)
        .bind(request.rate_limit_streams_per_minute)
        .bind(&request.metadata)
        .bind(current_version)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| {
            AppError::conflict(
                "resource_version_conflict",
                "resource version does not match If-Match",
            )
        })?;
        let record = application_execution_policy_record_from_row(&row)?;
        tx.commit().await?;
        Ok(record)
    }

    async fn claim_idempotency(
        &self,
        record: &IdempotencyRecord,
    ) -> Result<IdempotencyClaim, AppError> {
        let result = sqlx::query(
            r#"
            insert into idempotency_records
                (id, idempotency_key_hash, actor_fingerprint, operation, request_hash,
                 response_status, response_body, resource_id, expires_at)
            values ($1, $2, $3, $4, $5, null, null, null, $6)
            on conflict (idempotency_key_hash, actor_fingerprint, operation) do nothing
            "#,
        )
        .bind(record.id)
        .bind(&record.idempotency_key_hash)
        .bind(&record.actor_fingerprint)
        .bind(&record.operation)
        .bind(&record.request_hash)
        .bind(record.expires_at)
        .execute(&self.pool)
        .await?;
        if result.rows_affected() == 1 {
            return Ok(IdempotencyClaim::Claimed);
        }
        let existing = self
            .get_idempotency_record(
                &record.idempotency_key_hash,
                &record.actor_fingerprint,
                &record.operation,
            )
            .await?
            .ok_or_else(|| {
                AppError::conflict("execution_in_progress", "execution is in progress")
            })?;
        Ok(IdempotencyClaim::Replay(existing))
    }

    async fn get_idempotency_record(
        &self,
        key_hash: &str,
        actor_fingerprint: &str,
        operation: &str,
    ) -> Result<Option<IdempotencyRecord>, AppError> {
        let row = sqlx::query(
            r#"
            select id, idempotency_key_hash, actor_fingerprint, operation, request_hash,
                   response_status, response_body, resource_id, expires_at
            from idempotency_records
            where idempotency_key_hash = $1
              and actor_fingerprint = $2
              and operation = $3
              and expires_at > now()
            "#,
        )
        .bind(key_hash)
        .bind(actor_fingerprint)
        .bind(operation)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| {
            Ok(IdempotencyRecord {
                id: row.try_get("id")?,
                idempotency_key_hash: row.try_get("idempotency_key_hash")?,
                actor_fingerprint: row.try_get("actor_fingerprint")?,
                operation: row.try_get("operation")?,
                request_hash: row.try_get("request_hash")?,
                response_status: row.try_get("response_status")?,
                response_body: row.try_get("response_body")?,
                resource_id: row.try_get("resource_id")?,
                expires_at: row.try_get("expires_at")?,
            })
        })
        .transpose()
    }

    async fn finish_idempotency(
        &self,
        key_hash: &str,
        actor_fingerprint: &str,
        operation: &str,
        response_status: i32,
        response_body: &Value,
        resource_id: Option<&str>,
    ) -> Result<(), AppError> {
        sqlx::query(
            r#"
            update idempotency_records
            set response_status = $4,
                response_body = $5,
                resource_id = $6
            where idempotency_key_hash = $1
              and actor_fingerprint = $2
              and operation = $3
            "#,
        )
        .bind(key_hash)
        .bind(actor_fingerprint)
        .bind(operation)
        .bind(response_status)
        .bind(response_body)
        .bind(resource_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    async fn insert_response_started(
        &self,
        insert: &ResponseStartedInsert,
    ) -> Result<PublicResponseRecord, AppError> {
        let row = sqlx::query(&response_select(
            r#"
            insert into responses (
                id, execution_id, request_id, application_id, external_tenant_id,
                external_user_id, conversation_id, status, metadata, started_at, expires_at
            )
            values ($1, $2, $3, $4, $5, $6,
                    (select id from conversations where public_id = $7),
                    'in_progress', $8, now(), $9)
            returning *
            "#,
        ))
        .bind(insert.id)
        .bind(insert.execution_id)
        .bind(&insert.request_id)
        .bind(insert.application_id)
        .bind(&insert.external_tenant_id)
        .bind(&insert.external_user_id)
        .bind(&insert.conversation_public_id)
        .bind(&insert.metadata)
        .bind(insert.expires_at)
        .fetch_one(&self.pool)
        .await?;
        public_response_record_from_row(&row)
    }

    async fn complete_response(
        &self,
        id: Uuid,
        update: &ResponseTerminalUpdate,
    ) -> Result<PublicResponseRecord, AppError> {
        let usage = serde_json::to_value(&update.usage)
            .map_err(|err| AppError::Internal(format!("encode usage summary: {err}")))?;
        let row = sqlx::query(&response_select(
            r#"
            update responses
            set status = 'completed',
                route_id = $2,
                provider_id = $3,
                provider_model_id = $4,
                output_summary = $5,
                usage_summary = $6,
                output_persisted = $7,
                completed_at = now()
            where id = $1 and status in ('queued', 'in_progress')
            returning *
            "#,
        ))
        .bind(id)
        .bind(update.route_id)
        .bind(update.provider_id)
        .bind(update.provider_model_id)
        .bind(&update.output_summary)
        .bind(&usage)
        .bind(update.output_persisted)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::conflict("response_terminal", "response is already terminal"))?;
        public_response_record_from_row(&row)
    }

    async fn fail_response(
        &self,
        id: Uuid,
        update: &ResponseTerminalUpdate,
    ) -> Result<PublicResponseRecord, AppError> {
        let usage = serde_json::to_value(&update.usage)
            .map_err(|err| AppError::Internal(format!("encode usage summary: {err}")))?;
        let row = sqlx::query(&response_select(
            r#"
            update responses
            set status = 'failed',
                route_id = $2,
                provider_id = $3,
                provider_model_id = $4,
                output_summary = $5,
                usage_summary = $6,
                failure_class = $7,
                failure_message = $8,
                output_persisted = $9,
                failed_at = now()
            where id = $1 and status in ('queued', 'in_progress')
            returning *
            "#,
        ))
        .bind(id)
        .bind(update.route_id)
        .bind(update.provider_id)
        .bind(update.provider_model_id)
        .bind(&update.output_summary)
        .bind(&usage)
        .bind(&update.failure_class)
        .bind(&update.failure_message)
        .bind(update.output_persisted)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::conflict("response_terminal", "response is already terminal"))?;
        public_response_record_from_row(&row)
    }

    async fn cancel_response(
        &self,
        id: Uuid,
        update: &ResponseTerminalUpdate,
    ) -> Result<PublicResponseRecord, AppError> {
        let usage = serde_json::to_value(&update.usage)
            .map_err(|err| AppError::Internal(format!("encode usage summary: {err}")))?;
        let row = sqlx::query(&response_select(
            r#"
            update responses
            set status = 'cancelled',
                route_id = $2,
                provider_id = $3,
                provider_model_id = $4,
                output_summary = $5,
                usage_summary = $6,
                failure_class = $7,
                failure_message = $8,
                output_persisted = $9,
                cancelled_at = now()
            where id = $1 and status in ('queued', 'in_progress')
            returning *
            "#,
        ))
        .bind(id)
        .bind(update.route_id)
        .bind(update.provider_id)
        .bind(update.provider_model_id)
        .bind(&update.output_summary)
        .bind(&usage)
        .bind(&update.failure_class)
        .bind(&update.failure_message)
        .bind(update.output_persisted)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::conflict("response_terminal", "response is already terminal"))?;
        public_response_record_from_row(&row)
    }

    async fn find_response_authorized(
        &self,
        id: Uuid,
        access: &PublicAccess,
    ) -> Result<PublicResponseRecord, AppError> {
        let row = sqlx::query(&response_select(
            r#"
            select r.*
            from responses r
            where r.id = $1
              and ($2::boolean
                   or (($3::uuid is null or r.application_id = $3)
                       and ($4::text is null or r.external_tenant_id = $4)
                       and ($5::text is null or r.external_user_id = $5)))
            "#,
        ))
        .bind(id)
        .bind(access.privileged)
        .bind(access.application_id)
        .bind(&access.external_tenant_id)
        .bind(&access.external_user_id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("response".to_string()))?;
        public_response_record_from_row(&row)
    }

    async fn find_execution_authorized(
        &self,
        execution_id: Uuid,
        access: &PublicAccess,
    ) -> Result<PublicExecutionSummary, AppError> {
        let row = sqlx::query(&execution_summary_sql(
            r#"
            where r.execution_id = $1
              and ($2::boolean
                   or (($3::uuid is null or r.application_id = $3)
                       and ($4::text is null or r.external_tenant_id = $4)
                       and ($5::text is null or r.external_user_id = $5)))
            "#,
        ))
        .bind(execution_id)
        .bind(access.privileged)
        .bind(access.application_id)
        .bind(&access.external_tenant_id)
        .bind(&access.external_user_id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::NotFound("execution".to_string()))?;
        execution_summary_from_row(&row)
    }

    async fn list_executions_authorized(
        &self,
        access: &PublicAccess,
        query: &ExecutionQuery,
    ) -> Result<Vec<PublicExecutionSummary>, AppError> {
        let rows = sqlx::query(&format!(
            "{}\norder by r.created_at desc, r.id desc\nlimit $5",
            execution_summary_sql(
                r#"
            where ($1::boolean
                   or (($2::uuid is null or r.application_id = $2)
                       and ($3::text is null or r.external_tenant_id = $3)
                       and ($4::text is null or r.external_user_id = $4)))
            "#
            )
        ))
        .bind(access.privileged)
        .bind(access.application_id)
        .bind(&access.external_tenant_id)
        .bind(&access.external_user_id)
        .bind(query.limit())
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(execution_summary_from_row).collect()
    }

    async fn list_usage_authorized(
        &self,
        access: &PublicAccess,
        query: &UsageQuery,
    ) -> Result<Vec<PublicUsageRecord>, AppError> {
        let rows = sqlx::query(
            r#"
            select u.execution_id, p.provider_type, pm.model_key,
                   u.input_tokens, u.output_tokens, u.cached_input_tokens,
                   u.reasoning_tokens, u.total_tokens, u.estimated_total_cost,
                   u.currency, u.occurred_at
            from usage_records u
            left join responses r on r.execution_id = u.execution_id
            left join providers p on p.id = u.provider_id
            left join provider_models pm on pm.id = u.provider_model_id
            where ($1::boolean
                   or (($2::uuid is null or u.application_id = $2)
                       and ($3::text is null or u.external_tenant_id = $3)
                       and ($4::text is null or u.external_user_id = $4)))
              and ($5::uuid is null or u.application_id = $5)
              and ($6::text is null or u.external_tenant_id = $6)
              and ($7::text is null or u.external_user_id = $7)
              and ($8::uuid is null or u.provider_id = $8)
              and ($9::uuid is null or u.provider_model_id = $9)
              and ($10::uuid is null or r.route_id = $10)
              and ($11::timestamptz is null or u.occurred_at >= $11)
              and ($12::timestamptz is null or u.occurred_at <= $12)
            order by u.occurred_at desc, u.id desc
            limit $13
            "#,
        )
        .bind(access.privileged)
        .bind(access.application_id)
        .bind(&access.external_tenant_id)
        .bind(&access.external_user_id)
        .bind(query.application_id)
        .bind(&query.external_tenant_id)
        .bind(&query.external_user_id)
        .bind(query.provider_id)
        .bind(query.provider_model_id)
        .bind(query.route_id)
        .bind(query.occurred_after)
        .bind(query.occurred_before)
        .bind(query.limit())
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(usage_record_from_row).collect()
    }

    async fn list_visible_models(
        &self,
        access: &PublicAccess,
        limit: i64,
    ) -> Result<Vec<PublicModelResource>, AppError> {
        let rows = sqlx::query(
            r#"
            select distinct on (pm.id)
                   pm.id, pm.model_key, pm.display_name, pm.capabilities, p.provider_type
            from routing_policies rp
            join provider_models pm on pm.id = rp.provider_model_id
            join providers p on p.id = rp.provider_id
            where rp.status = 'active'
              and rp.deleted_at is null
              and pm.status = 'active'
              and pm.deleted_at is null
              and p.status = 'active'
              and p.deleted_at is null
              and ($1::boolean
                   or ((rp.application_id is null or rp.application_id = $2)
                       and (rp.external_tenant_id is null
                            or ($3::text is not null and rp.external_tenant_id = $3))))
            order by pm.id, rp.priority asc, rp.weight desc
            limit $4
            "#,
        )
        .bind(access.privileged)
        .bind(access.application_id)
        .bind(&access.external_tenant_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(public_model_from_row).collect()
    }

    async fn find_visible_model_id_by_key(
        &self,
        access: &PublicAccess,
        model_key: &str,
    ) -> Result<Option<Uuid>, AppError> {
        sqlx::query_scalar(
            r#"
            select pm.id
            from routing_policies rp
            join provider_models pm on pm.id = rp.provider_model_id
            join providers p on p.id = rp.provider_id
            where pm.model_key = $1
              and rp.status = 'active'
              and rp.deleted_at is null
              and pm.status = 'active'
              and pm.deleted_at is null
              and p.status = 'active'
              and p.deleted_at is null
              and ($2::boolean
                   or ((rp.application_id is null or rp.application_id = $3)
                       and (rp.external_tenant_id is null
                            or ($4::text is not null and rp.external_tenant_id = $4))))
            order by rp.priority asc, rp.weight desc, pm.id asc
            limit 1
            "#,
        )
        .bind(model_key)
        .bind(access.privileged)
        .bind(access.application_id)
        .bind(&access.external_tenant_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(AppError::from)
    }

    async fn list_visible_routes(
        &self,
        access: &PublicAccess,
        limit: i64,
    ) -> Result<Vec<PublicRouteResource>, AppError> {
        let rows = sqlx::query(
            r#"
            select rd.id, rd.route_key, rd.display_name, rd.description,
                   coalesce(jsonb_agg(distinct pm.capabilities) filter (where pm.id is not null), '[]'::jsonb) as capabilities
            from route_definitions rd
            left join routing_policies rp on rp.route_id = rd.id
                and rp.status = 'active'
                and rp.deleted_at is null
            left join provider_models pm on pm.id = rp.provider_model_id
                and pm.status = 'active'
                and pm.deleted_at is null
            where rd.status = 'active'
              and rd.deleted_at is null
              and ($1::boolean
                   or (rp.id is not null
                       and (rp.application_id is null or rp.application_id = $2)
                       and (rp.external_tenant_id is null
                            or ($3::text is not null and rp.external_tenant_id = $3))))
            group by rd.id, rd.route_key, rd.display_name, rd.description
            order by rd.route_key asc
            limit $4
            "#,
        )
        .bind(access.privileged)
        .bind(access.application_id)
        .bind(&access.external_tenant_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(public_route_from_row).collect()
    }
}

pub fn default_application_execution_policy(
    application_id: Uuid,
) -> ApplicationExecutionPolicyRecord {
    ApplicationExecutionPolicyRecord {
        id: application_id,
        application_id,
        responses_enabled: true,
        streaming_enabled: true,
        tools_enabled: false,
        vision_enabled: true,
        structured_output_enabled: true,
        caller_system_instructions_allowed: false,
        model_overrides_allowed: false,
        route_overrides_allowed: false,
        provider_overrides_allowed: false,
        credential_overrides_allowed: false,
        timeout_overrides_allowed: false,
        persistence_mode: ResponsePersistenceMode::MetadataOnly,
        response_retention_seconds: 2_592_000,
        maximum_request_bytes: 1_048_576,
        maximum_input_items: 128,
        maximum_output_tokens: 8192,
        maximum_timeout_ms: 600_000,
        rate_limit_requests_per_minute: 120,
        rate_limit_streams_per_minute: 60,
        metadata: json!({ "source": "default" }),
        updated_at: Utc::now(),
        version: 1,
    }
}

/// Builds the ledger row for a fresh `/v1/responses` idempotency claim.
///
/// The key hash is keyed by the deployment pepper (plan 03, P1-1): the raw
/// `Idempotency-Key` is caller-supplied and its unkeyed digest was offline-guessable from
/// a database read alone.
pub fn idempotency_record(
    hasher: &IdempotencyHasher,
    key: &str,
    actor_fingerprint: String,
    operation: &str,
    request_hash_value: String,
) -> IdempotencyRecord {
    IdempotencyRecord {
        id: Uuid::now_v7(),
        idempotency_key_hash: hasher.hash(key.as_bytes()),
        actor_fingerprint,
        operation: operation.to_string(),
        request_hash: request_hash_value,
        response_status: None,
        response_body: None,
        resource_id: None,
        expires_at: Utc::now() + Duration::hours(24),
    }
}

fn response_select(inner: &str) -> String {
    format!(
        r#"
        with response_rows as ({inner})
        select response_rows.id, response_rows.execution_id, response_rows.request_id,
               response_rows.application_id, response_rows.external_tenant_id,
               response_rows.external_user_id, response_rows.conversation_id,
               c.public_id as conversation_public_id, response_rows.status, response_rows.route_id,
               rd.route_key, response_rows.provider_id, p.provider_type,
               response_rows.provider_model_id, pm.model_key, response_rows.output_summary,
               response_rows.usage_summary, response_rows.metadata, response_rows.failure_class,
               response_rows.failure_message, response_rows.output_persisted,
               response_rows.created_at, response_rows.started_at, response_rows.completed_at,
               response_rows.failed_at, response_rows.cancelled_at, response_rows.expires_at,
               response_rows.version
        from response_rows
        left join route_definitions rd on rd.id = response_rows.route_id
        left join conversations c on c.id = response_rows.conversation_id
        left join providers p on p.id = response_rows.provider_id
        left join provider_models pm on pm.id = response_rows.provider_model_id
        "#
    )
}

fn execution_summary_sql(where_clause: &str) -> String {
    format!(
        r#"
        select r.id as response_id, r.execution_id, r.request_id, r.status,
               r.route_id, rd.route_key, r.provider_model_id, r.provider_id,
               p.provider_type, pm.model_key, r.started_at, r.completed_at,
               r.created_at, r.usage_summary, r.failure_class,
               count(ea.id)::bigint as attempt_count,
               coalesce(
                   max(ea.latency_ms),
                   (extract(epoch from (coalesce(r.completed_at, r.failed_at, r.cancelled_at, now()) - coalesce(r.started_at, r.created_at))) * 1000)::bigint
               ) as latency_ms
        from responses r
        left join execution_attempts ea on ea.execution_id = r.execution_id
        left join route_definitions rd on rd.id = r.route_id
        left join providers p on p.id = r.provider_id
        left join provider_models pm on pm.id = r.provider_model_id
        {where_clause}
        group by r.id, rd.route_key, p.provider_type, pm.model_key
        "#
    )
}

fn execution_summary_from_row(
    row: &sqlx::postgres::PgRow,
) -> Result<PublicExecutionSummary, AppError> {
    let status = crate::infra::pg_rows::public_response_status_from_db(row.try_get("status")?)?;
    let usage = row
        .try_get::<Value, _>("usage_summary")
        .ok()
        .and_then(|value| serde_json::from_value::<PublicUsageSummary>(value).ok())
        .unwrap_or_default();
    let provider_type = row
        .try_get::<Option<String>, _>("provider_type")?
        .map(provider_type_from_db)
        .transpose()?;
    let failure_class = row
        .try_get::<Option<String>, _>("failure_class")?
        .map(execution_failure_class_from_db)
        .transpose()?;
    Ok(PublicExecutionSummary {
        execution_id: format!("exec_{}", row.try_get::<Uuid, _>("execution_id")?),
        response_id: format!("resp_{}", row.try_get::<Uuid, _>("response_id")?),
        request_id: row.try_get("request_id")?,
        status,
        route: row
            .try_get::<Option<Uuid>, _>("route_id")?
            .zip(row.try_get::<Option<String>, _>("route_key")?)
            .map(|(id, key)| crate::domain::PublicRouteRef { id, key }),
        model: match (
            row.try_get::<Option<Uuid>, _>("provider_model_id")?,
            provider_type,
            row.try_get::<Option<String>, _>("model_key")?,
        ) {
            (Some(id), Some(provider), Some(key)) => {
                Some(crate::domain::PublicModelRef { id, provider, key })
            }
            _ => None,
        },
        attempt_count: row.try_get("attempt_count")?,
        started_at: row.try_get("started_at")?,
        completed_at: row.try_get("completed_at")?,
        latency_ms: row.try_get("latency_ms")?,
        usage,
        failure_class,
    })
}

fn usage_record_from_row(row: &sqlx::postgres::PgRow) -> Result<PublicUsageRecord, AppError> {
    let provider = row
        .try_get::<Option<String>, _>("provider_type")?
        .map(provider_type_from_db)
        .transpose()?;
    Ok(PublicUsageRecord {
        execution_id: format!("exec_{}", row.try_get::<Uuid, _>("execution_id")?),
        provider,
        model: row.try_get("model_key")?,
        input_tokens: i64_to_u64(row.try_get("input_tokens")?)?,
        output_tokens: i64_to_u64(row.try_get("output_tokens")?)?,
        cached_input_tokens: i64_to_u64(row.try_get("cached_input_tokens")?)?,
        reasoning_tokens: i64_to_u64(row.try_get("reasoning_tokens")?)?,
        total_tokens: i64_to_u64(row.try_get("total_tokens")?)?,
        estimated_cost: row.try_get("estimated_total_cost")?,
        currency: row.try_get("currency")?,
        occurred_at: row.try_get("occurred_at")?,
    })
}

fn public_model_from_row(row: &sqlx::postgres::PgRow) -> Result<PublicModelResource, AppError> {
    let capabilities: Value = row.try_get("capabilities")?;
    Ok(PublicModelResource {
        id: row.try_get("id")?,
        key: row.try_get("model_key")?,
        provider: provider_type_from_db(row.try_get("provider_type")?)?,
        display_name: row.try_get("display_name")?,
        capabilities: public_capabilities_from_value(&capabilities),
    })
}

fn public_route_from_row(row: &sqlx::postgres::PgRow) -> Result<PublicRouteResource, AppError> {
    let capabilities_value: Value = row.try_get("capabilities")?;
    let mut capabilities = Vec::new();
    collect_capability_names(&capabilities_value, &mut capabilities);
    capabilities.sort();
    capabilities.dedup();
    Ok(PublicRouteResource {
        id: row.try_get("id")?,
        key: row.try_get("route_key")?,
        display_name: row.try_get("display_name")?,
        description: row.try_get("description")?,
        capabilities,
    })
}

fn public_capabilities_from_value(value: &Value) -> PublicModelCapabilities {
    PublicModelCapabilities {
        text: capability_bool(value, "text").unwrap_or(true),
        vision: capability_bool(value, "vision").unwrap_or(false),
        tools: capability_bool(value, "tools").unwrap_or(false),
        streaming: capability_bool(value, "streaming").unwrap_or(true),
        structured_output: capability_bool(value, "structured_output").unwrap_or(false),
    }
}

fn collect_capability_names(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Array(items) => items
            .iter()
            .for_each(|item| collect_capability_names(item, out)),
        Value::Object(map) => {
            for (key, value) in map {
                if value.as_bool() == Some(true) {
                    out.push(key.clone());
                }
            }
        }
        _ => {}
    }
}

fn capability_bool(value: &Value, key: &str) -> Option<bool> {
    value
        .get(key)
        .and_then(Value::as_bool)
        .or_else(|| value.get("capabilities")?.get(key)?.as_bool())
}

fn i64_to_u64(value: Option<i64>) -> Result<Option<u64>, AppError> {
    value
        .map(u64::try_from)
        .transpose()
        .map_err(|_| AppError::Internal("usage value was negative".to_string()))
}

/// In-memory [`PublicRepository`] for unit tests (plan 06, Module 8 / P2-3).
///
/// Backs the public **idempotency envelope** and the execution-policy read — the slice
/// `PublicExecutionService` needs to make a replay decision — with real state, and returns an
/// explicit `not_stubbed` error for every other method. That is deliberate: a fake that silently
/// returned `Ok(default)` for an unimplemented read would let a test pass while exercising
/// nothing. Extend the backed set as unit tests need it.
///
/// Carries no credential material; the public surface has none.
#[cfg(test)]
#[derive(Debug, Default)]
pub(crate) struct InMemoryPublicRepository {
    policies: std::sync::Mutex<std::collections::HashMap<Uuid, ApplicationExecutionPolicyRecord>>,
    idempotency:
        std::sync::Mutex<std::collections::HashMap<(String, String, String), IdempotencyRecord>>,
}

#[cfg(test)]
fn not_stubbed(method: &str) -> AppError {
    AppError::Internal(format!("InMemoryPublicRepository::{method} is not stubbed"))
}

#[cfg(test)]
impl InMemoryPublicRepository {
    pub(crate) fn with_policy(policy: ApplicationExecutionPolicyRecord) -> Self {
        let fake = Self::default();
        fake.policies
            .lock()
            .unwrap()
            .insert(policy.application_id, policy);
        fake
    }
}

#[cfg(test)]
#[async_trait]
impl PublicRepository for InMemoryPublicRepository {
    async fn get_or_create_application_execution_policy(
        &self,
        application_id: Uuid,
    ) -> Result<ApplicationExecutionPolicyRecord, AppError> {
        Ok(self
            .policies
            .lock()
            .unwrap()
            .entry(application_id)
            .or_insert_with(|| default_application_execution_policy(application_id))
            .clone())
    }

    /// Same semantics as the SQL: the first claim for a `(key, actor, operation)` triple wins and
    /// every later one replays the stored record.
    async fn claim_idempotency(
        &self,
        record: &IdempotencyRecord,
    ) -> Result<IdempotencyClaim, AppError> {
        let key = (
            record.idempotency_key_hash.clone(),
            record.actor_fingerprint.clone(),
            record.operation.clone(),
        );
        let mut guard = self.idempotency.lock().unwrap();
        match guard.get(&key) {
            Some(existing) => Ok(IdempotencyClaim::Replay(existing.clone())),
            None => {
                guard.insert(key, record.clone());
                Ok(IdempotencyClaim::Claimed)
            }
        }
    }

    async fn get_idempotency_record(
        &self,
        key_hash: &str,
        actor_fingerprint: &str,
        operation: &str,
    ) -> Result<Option<IdempotencyRecord>, AppError> {
        Ok(self
            .idempotency
            .lock()
            .unwrap()
            .get(&(
                key_hash.to_string(),
                actor_fingerprint.to_string(),
                operation.to_string(),
            ))
            .cloned())
    }

    async fn finish_idempotency(
        &self,
        key_hash: &str,
        actor_fingerprint: &str,
        operation: &str,
        response_status: i32,
        response_body: &Value,
        resource_id: Option<&str>,
    ) -> Result<(), AppError> {
        let mut guard = self.idempotency.lock().unwrap();
        let Some(record) = guard.get_mut(&(
            key_hash.to_string(),
            actor_fingerprint.to_string(),
            operation.to_string(),
        )) else {
            return Err(not_stubbed("finish_idempotency (unclaimed key)"));
        };
        record.response_status = Some(response_status);
        record.response_body = Some(response_body.clone());
        record.resource_id = resource_id.map(str::to_string);
        Ok(())
    }

    async fn put_application_execution_policy(
        &self,
        _application_id: Uuid,
        _expected_version: Option<i64>,
        _request: &ApplicationExecutionPolicyPutRequest,
    ) -> Result<ApplicationExecutionPolicyRecord, AppError> {
        Err(not_stubbed("put_application_execution_policy"))
    }

    async fn insert_response_started(
        &self,
        _insert: &ResponseStartedInsert,
    ) -> Result<PublicResponseRecord, AppError> {
        Err(not_stubbed("insert_response_started"))
    }

    async fn complete_response(
        &self,
        _id: Uuid,
        _update: &ResponseTerminalUpdate,
    ) -> Result<PublicResponseRecord, AppError> {
        Err(not_stubbed("complete_response"))
    }

    async fn fail_response(
        &self,
        _id: Uuid,
        _update: &ResponseTerminalUpdate,
    ) -> Result<PublicResponseRecord, AppError> {
        Err(not_stubbed("fail_response"))
    }

    async fn cancel_response(
        &self,
        _id: Uuid,
        _update: &ResponseTerminalUpdate,
    ) -> Result<PublicResponseRecord, AppError> {
        Err(not_stubbed("cancel_response"))
    }

    async fn find_response_authorized(
        &self,
        _id: Uuid,
        _access: &PublicAccess,
    ) -> Result<PublicResponseRecord, AppError> {
        Err(not_stubbed("find_response_authorized"))
    }

    async fn find_execution_authorized(
        &self,
        _execution_id: Uuid,
        _access: &PublicAccess,
    ) -> Result<PublicExecutionSummary, AppError> {
        Err(not_stubbed("find_execution_authorized"))
    }

    async fn list_executions_authorized(
        &self,
        _access: &PublicAccess,
        _query: &ExecutionQuery,
    ) -> Result<Vec<PublicExecutionSummary>, AppError> {
        Err(not_stubbed("list_executions_authorized"))
    }

    async fn list_usage_authorized(
        &self,
        _access: &PublicAccess,
        _query: &UsageQuery,
    ) -> Result<Vec<PublicUsageRecord>, AppError> {
        Err(not_stubbed("list_usage_authorized"))
    }

    async fn list_visible_models(
        &self,
        _access: &PublicAccess,
        _limit: i64,
    ) -> Result<Vec<PublicModelResource>, AppError> {
        Err(not_stubbed("list_visible_models"))
    }

    async fn find_visible_model_id_by_key(
        &self,
        _access: &PublicAccess,
        _model_key: &str,
    ) -> Result<Option<Uuid>, AppError> {
        Err(not_stubbed("find_visible_model_id_by_key"))
    }

    async fn list_visible_routes(
        &self,
        _access: &PublicAccess,
        _limit: i64,
    ) -> Result<Vec<PublicRouteResource>, AppError> {
        Err(not_stubbed("list_visible_routes"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(key: &str, actor: &str) -> IdempotencyRecord {
        IdempotencyRecord {
            id: Uuid::now_v7(),
            idempotency_key_hash: key.to_string(),
            actor_fingerprint: actor.to_string(),
            operation: "response.create".to_string(),
            request_hash: "request-hash".to_string(),
            response_status: None,
            response_body: None,
            resource_id: None,
            expires_at: Utc::now() + Duration::hours(24),
        }
    }

    /// P2-3: the public idempotency envelope — claim, finish, replay — is now exercisable in a
    /// unit test. Before `PublicRepository` existed this needed a live Postgres.
    #[tokio::test]
    async fn fake_public_repository_supports_execution_service_unit_test() {
        let application_id = Uuid::now_v7();
        let repo = InMemoryPublicRepository::with_policy(ApplicationExecutionPolicyRecord {
            streaming_enabled: false,
            ..default_application_execution_policy(application_id)
        });

        let policy = repo
            .get_or_create_application_execution_policy(application_id)
            .await
            .expect("policy");
        assert!(policy.responses_enabled);
        assert!(!policy.streaming_enabled);

        // First claim wins.
        let first = repo
            .claim_idempotency(&record("key", "actor"))
            .await
            .unwrap();
        assert!(matches!(first, IdempotencyClaim::Claimed));

        repo.finish_idempotency(
            "key",
            "actor",
            "response.create",
            200,
            &json!({"id": "r1"}),
            Some("r1"),
        )
        .await
        .unwrap();

        // The same triple replays the finished response.
        let second = repo
            .claim_idempotency(&record("key", "actor"))
            .await
            .unwrap();
        let IdempotencyClaim::Replay(stored) = second else {
            panic!("expected a replay");
        };
        assert_eq!(stored.response_status, Some(200));
        assert_eq!(stored.resource_id.as_deref(), Some("r1"));

        // A different actor fingerprint is a different envelope — the isolation the unique index
        // exists to provide.
        let other = repo
            .claim_idempotency(&record("key", "other-actor"))
            .await
            .unwrap();
        assert!(matches!(other, IdempotencyClaim::Claimed));
    }

    /// A method the fake does not back fails loudly rather than returning a plausible empty
    /// result, so a unit test cannot silently exercise nothing.
    #[tokio::test]
    async fn unbacked_fake_methods_fail_loudly() {
        let repo = InMemoryPublicRepository::default();
        let error = repo
            .list_visible_routes(
                &PublicAccess {
                    privileged: true,
                    application_id: None,
                    external_tenant_id: None,
                    external_user_id: None,
                },
                10,
            )
            .await
            .expect_err("unbacked method");
        assert!(error.to_string().contains("not stubbed"));
    }
}
