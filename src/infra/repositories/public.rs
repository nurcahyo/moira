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
    security::request_hash,
};

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

impl PgPublicRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn get_or_create_application_execution_policy(
        &self,
        application_id: Uuid,
    ) -> Result<ApplicationExecutionPolicyRecord, AppError> {
        if let Some(row) = sqlx::query(
            r#"
            select id, application_id, responses_enabled, streaming_enabled, tools_enabled,
                   vision_enabled, structured_output_enabled,
                   caller_system_instructions_allowed, model_overrides_allowed,
                   route_overrides_allowed, provider_overrides_allowed,
                   credential_overrides_allowed, timeout_overrides_allowed,
                   persistence_mode, response_retention_seconds, maximum_request_bytes,
                   maximum_input_items, maximum_output_tokens, maximum_timeout_ms,
                   rate_limit_requests_per_minute, rate_limit_streams_per_minute,
                   metadata, updated_at, version
            from application_execution_policies
            where application_id = $1
            "#,
        )
        .bind(application_id)
        .fetch_optional(&self.pool)
        .await?
        {
            return application_execution_policy_record_from_row(&row);
        }

        let row = sqlx::query(
            r#"
            insert into application_execution_policies (application_id)
            values ($1)
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
        .fetch_one(&self.pool)
        .await?;
        application_execution_policy_record_from_row(&row)
    }

    pub async fn put_application_execution_policy(
        &self,
        application_id: Uuid,
        request: &ApplicationExecutionPolicyPutRequest,
    ) -> Result<ApplicationExecutionPolicyRecord, AppError> {
        let persistence_mode = request
            .persistence_mode
            .map(response_persistence_mode_to_db);
        let row = sqlx::query(
            r#"
            insert into application_execution_policies (
                application_id, responses_enabled, streaming_enabled, tools_enabled,
                vision_enabled, structured_output_enabled, caller_system_instructions_allowed,
                model_overrides_allowed, route_overrides_allowed, provider_overrides_allowed,
                credential_overrides_allowed, timeout_overrides_allowed, persistence_mode,
                response_retention_seconds, maximum_request_bytes, maximum_input_items,
                maximum_output_tokens, maximum_timeout_ms, rate_limit_requests_per_minute,
                rate_limit_streams_per_minute, metadata
            )
            values (
                $1,
                coalesce($2, true),
                coalesce($3, true),
                coalesce($4, false),
                coalesce($5, true),
                coalesce($6, true),
                coalesce($7, false),
                coalesce($8, false),
                coalesce($9, false),
                coalesce($10, false),
                coalesce($11, false),
                coalesce($12, false),
                coalesce($13, 'metadata_only'),
                coalesce($14, 2592000),
                coalesce($15, 1048576),
                coalesce($16, 128),
                coalesce($17, 8192),
                coalesce($18, 600000),
                coalesce($19, 120),
                coalesce($20, 60),
                coalesce($21, '{}'::jsonb)
            )
            on conflict (application_id) do update set
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
        .fetch_one(&self.pool)
        .await?;
        application_execution_policy_record_from_row(&row)
    }

    pub async fn claim_idempotency(
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

    pub async fn get_idempotency_record(
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

    pub async fn finish_idempotency(
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

    pub async fn insert_response_started(
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

    pub async fn complete_response(
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

    pub async fn fail_response(
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

    pub async fn cancel_response(
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

    pub async fn find_response_authorized(
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

    pub async fn find_execution_authorized(
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

    pub async fn list_executions_authorized(
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

    pub async fn list_usage_authorized(
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

    pub async fn list_visible_models(
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
                   or (($2::uuid is null or rp.application_id is null or rp.application_id = $2)
                       and ($3::text is null or rp.external_tenant_id is null or rp.external_tenant_id = $3)))
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

    pub async fn find_visible_model_id_by_key(
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
                   or (($3::uuid is null or rp.application_id is null or rp.application_id = $3)
                       and ($4::text is null or rp.external_tenant_id is null or rp.external_tenant_id = $4)))
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

    pub async fn list_visible_routes(
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
                   or rp.id is null
                   or (($2::uuid is null or rp.application_id is null or rp.application_id = $2)
                       and ($3::text is null or rp.external_tenant_id is null or rp.external_tenant_id = $3)))
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

pub fn idempotency_record(
    key: &str,
    actor_fingerprint: String,
    operation: &str,
    request_hash_value: String,
) -> IdempotencyRecord {
    IdempotencyRecord {
        id: Uuid::now_v7(),
        idempotency_key_hash: request_hash(key.as_bytes()),
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
