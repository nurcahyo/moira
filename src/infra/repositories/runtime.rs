use chrono::Utc;
use serde_json::Value;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::{
    domain::{
        AgentProfileCreateRequest, AgentProfilePatchRequest, AgentProfileRecord, AttemptStatus,
        CredentialDecisionSource, CredentialRecord, CredentialType, ExecutionFailureClass,
        ModelCandidate, ProviderRuntimePolicyPutRequest, ProviderRuntimePolicyRecord,
        RouteDefinitionCreateRequest, RouteDefinitionPatchRequest, RouteDefinitionRecord,
        RoutingPolicyCreateRequest, RoutingPolicyPatchRequest, RoutingPolicyRecord, UsageSummary,
    },
    error::AppError,
    infra::pg_rows::{
        agent_profile_record_from_row, credential_record_from_row, credential_type_to_db,
        provider_runtime_policy_record_from_row, provider_type_from_db,
        route_definition_record_from_row, route_selection_strategy_to_db,
        routing_policy_record_from_row, runtime_policy_status_to_db,
    },
    security::EncryptedSecret,
};

#[derive(Debug, Clone)]
pub struct PgRuntimeRepository {
    pool: PgPool,
}

#[derive(Debug, Clone)]
pub struct RuntimeCredentialCandidate {
    pub record: CredentialRecord,
    pub encrypted: EncryptedSecret,
    pub source: CredentialDecisionSource,
}

#[derive(Debug, Clone)]
pub struct ExecutionAttemptInsert {
    pub id: Uuid,
    pub request_id: String,
    pub execution_id: Uuid,
    pub attempt_number: i32,
    pub application_id: Option<Uuid>,
    pub external_tenant_id: Option<String>,
    pub external_user_id: Option<String>,
    pub route_id: Uuid,
    pub provider_id: Uuid,
    pub provider_model_id: Uuid,
    pub credential_id: Uuid,
    pub metadata: Value,
}

#[derive(Debug, Clone)]
pub struct ExecutionAttemptUpdate {
    pub status: AttemptStatus,
    pub failure_class: Option<ExecutionFailureClass>,
    pub provider_status_code: Option<i32>,
    pub latency_ms: Option<i64>,
    pub usage: UsageSummary,
    pub provider_request_id: Option<String>,
    pub metadata: Value,
}

#[derive(Debug, Clone)]
pub struct UsageRecordInsert {
    pub id: Uuid,
    pub request_id: String,
    pub execution_id: Uuid,
    pub attempt_id: Uuid,
    pub application_id: Option<Uuid>,
    pub external_tenant_id: Option<String>,
    pub external_user_id: Option<String>,
    pub provider_id: Uuid,
    pub provider_model_id: Uuid,
    pub credential_id: Uuid,
    pub usage: UsageSummary,
    pub metadata: Value,
}

impl PgRuntimeRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn create_route_definition(
        &self,
        id: Uuid,
        request: &RouteDefinitionCreateRequest,
    ) -> Result<RouteDefinitionRecord, AppError> {
        let row = sqlx::query(
            r#"
            insert into route_definitions
                (id, route_key, display_name, description, selection_strategy, agent_profile_id, metadata)
            values ($1, $2, $3, $4, $5, $6, $7)
            returning id, route_key, display_name, description, status, selection_strategy,
                      agent_profile_id, metadata, created_at, updated_at, deleted_at, version
            "#,
        )
        .bind(id)
        .bind(&request.route_key)
        .bind(&request.display_name)
        .bind(&request.description)
        .bind(route_selection_strategy_to_db(&request.selection_strategy))
        .bind(request.agent_profile_id)
        .bind(&request.metadata)
        .fetch_one(&self.pool)
        .await?;
        route_definition_record_from_row(&row)
    }

    pub async fn list_route_definitions(
        &self,
        limit: i64,
    ) -> Result<Vec<RouteDefinitionRecord>, AppError> {
        let rows = sqlx::query(
            r#"
            select id, route_key, display_name, description, status, selection_strategy,
                   agent_profile_id, metadata, created_at, updated_at, deleted_at, version
            from route_definitions
            where deleted_at is null
            order by created_at desc, id desc
            limit $1
            "#,
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(route_definition_record_from_row).collect()
    }

    pub async fn get_route_definition(&self, id: Uuid) -> Result<RouteDefinitionRecord, AppError> {
        let row = sqlx::query(&route_definition_select(
            "where id = $1 and deleted_at is null",
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("route {id}")))?;
        route_definition_record_from_row(&row)
    }

    pub async fn get_active_route_by_key(
        &self,
        route_key: &str,
    ) -> Result<Option<RouteDefinitionRecord>, AppError> {
        let row = sqlx::query(&route_definition_select(
            "where route_key = $1 and status = 'active' and deleted_at is null",
        ))
        .bind(route_key)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref()
            .map(route_definition_record_from_row)
            .transpose()
    }

    pub async fn get_default_route(&self) -> Result<Option<RouteDefinitionRecord>, AppError> {
        let row = sqlx::query(&route_definition_select(
            "where status = 'active' and deleted_at is null order by case when route_key = 'general' then 0 else 1 end, created_at asc limit 1",
        ))
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref()
            .map(route_definition_record_from_row)
            .transpose()
    }

    pub async fn patch_route_definition(
        &self,
        id: Uuid,
        request: &RouteDefinitionPatchRequest,
    ) -> Result<RouteDefinitionRecord, AppError> {
        let strategy = request
            .selection_strategy
            .as_ref()
            .map(route_selection_strategy_to_db);
        let row = sqlx::query(
            r#"
            update route_definitions
            set display_name = coalesce($2, display_name),
                description = coalesce($3, description),
                selection_strategy = coalesce($4, selection_strategy),
                agent_profile_id = coalesce($5, agent_profile_id),
                metadata = coalesce($6, metadata),
                updated_at = now()
            where id = $1 and deleted_at is null
            returning id, route_key, display_name, description, status, selection_strategy,
                      agent_profile_id, metadata, created_at, updated_at, deleted_at, version
            "#,
        )
        .bind(id)
        .bind(&request.display_name)
        .bind(&request.description)
        .bind(strategy)
        .bind(request.agent_profile_id)
        .bind(&request.metadata)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("route {id}")))?;
        route_definition_record_from_row(&row)
    }

    pub async fn set_route_definition_status(
        &self,
        id: Uuid,
        status: &str,
    ) -> Result<RouteDefinitionRecord, AppError> {
        let row = sqlx::query(
            r#"
            update route_definitions
            set status = $2,
                deleted_at = case when $2 = 'deleted' then coalesce(deleted_at, now()) else deleted_at end,
                updated_at = now()
            where id = $1 and deleted_at is null
            returning id, route_key, display_name, description, status, selection_strategy,
                      agent_profile_id, metadata, created_at, updated_at, deleted_at, version
            "#,
        )
        .bind(id)
        .bind(status)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("route {id}")))?;
        route_definition_record_from_row(&row)
    }

    pub async fn soft_delete_route_definition(&self, id: Uuid) -> Result<(), AppError> {
        let result = sqlx::query(
            "update route_definitions set status = 'deleted', deleted_at = now(), updated_at = now() where id = $1 and deleted_at is null",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        ensure_affected(result.rows_affected(), format!("route {id}"))
    }

    pub async fn create_routing_policy(
        &self,
        id: Uuid,
        request: &RoutingPolicyCreateRequest,
    ) -> Result<RoutingPolicyRecord, AppError> {
        let row = sqlx::query(routing_policy_insert_sql())
            .bind(id)
            .bind(request.application_id)
            .bind(&request.external_tenant_id)
            .bind(request.route_id)
            .bind(request.provider_id)
            .bind(request.provider_model_id)
            .bind(request.priority)
            .bind(request.weight)
            .bind(request.cost_weight)
            .bind(request.latency_weight)
            .bind(request.quality_weight)
            .bind(&request.privacy_class)
            .bind(&request.required_capabilities)
            .bind(request.maximum_cost_per_request)
            .bind(request.maximum_input_tokens)
            .bind(request.maximum_output_tokens)
            .bind(request.timeout_ms)
            .bind(&request.retry_policy)
            .bind(&request.metadata)
            .fetch_one(&self.pool)
            .await?;
        routing_policy_record_from_row(&row)
    }

    pub async fn list_routing_policies(
        &self,
        limit: i64,
    ) -> Result<Vec<RoutingPolicyRecord>, AppError> {
        let rows = sqlx::query(
            r#"
            select id, application_id, external_tenant_id, route_id, provider_id,
                   provider_model_id, priority, weight, cost_weight, latency_weight,
                   quality_weight, privacy_class, required_capabilities,
                   maximum_cost_per_request, maximum_input_tokens, maximum_output_tokens,
                   timeout_ms, retry_policy, status, metadata, created_at, updated_at,
                   deleted_at, version
            from routing_policies
            where deleted_at is null
            order by created_at desc, id desc
            limit $1
            "#,
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(routing_policy_record_from_row).collect()
    }

    pub async fn get_routing_policy(&self, id: Uuid) -> Result<RoutingPolicyRecord, AppError> {
        let row = sqlx::query(&routing_policy_select(
            "where id = $1 and deleted_at is null",
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("routing policy {id}")))?;
        routing_policy_record_from_row(&row)
    }

    pub async fn provider_model_belongs_to_provider(
        &self,
        provider_id: Uuid,
        provider_model_id: Uuid,
    ) -> Result<bool, AppError> {
        sqlx::query_scalar(
            "select exists(select 1 from provider_models where id = $1 and provider_id = $2)",
        )
        .bind(provider_model_id)
        .bind(provider_id)
        .fetch_one(&self.pool)
        .await
        .map_err(AppError::from)
    }

    pub async fn patch_routing_policy(
        &self,
        id: Uuid,
        request: &RoutingPolicyPatchRequest,
    ) -> Result<RoutingPolicyRecord, AppError> {
        let row = sqlx::query(
            r#"
            update routing_policies
            set application_id = coalesce($2, application_id),
                external_tenant_id = coalesce($3, external_tenant_id),
                route_id = coalesce($4, route_id),
                provider_id = coalesce($5, provider_id),
                provider_model_id = coalesce($6, provider_model_id),
                priority = coalesce($7, priority),
                weight = coalesce($8, weight),
                cost_weight = coalesce($9, cost_weight),
                latency_weight = coalesce($10, latency_weight),
                quality_weight = coalesce($11, quality_weight),
                privacy_class = coalesce($12, privacy_class),
                required_capabilities = coalesce($13, required_capabilities),
                maximum_cost_per_request = coalesce($14, maximum_cost_per_request),
                maximum_input_tokens = coalesce($15, maximum_input_tokens),
                maximum_output_tokens = coalesce($16, maximum_output_tokens),
                timeout_ms = coalesce($17, timeout_ms),
                retry_policy = coalesce($18, retry_policy),
                metadata = coalesce($19, metadata),
                updated_at = now()
            where id = $1 and deleted_at is null
            returning id, application_id, external_tenant_id, route_id, provider_id,
                      provider_model_id, priority, weight, cost_weight, latency_weight,
                      quality_weight, privacy_class, required_capabilities,
                      maximum_cost_per_request, maximum_input_tokens, maximum_output_tokens,
                      timeout_ms, retry_policy, status, metadata, created_at, updated_at,
                      deleted_at, version
            "#,
        )
        .bind(id)
        .bind(request.application_id)
        .bind(&request.external_tenant_id)
        .bind(request.route_id)
        .bind(request.provider_id)
        .bind(request.provider_model_id)
        .bind(request.priority)
        .bind(request.weight)
        .bind(request.cost_weight)
        .bind(request.latency_weight)
        .bind(request.quality_weight)
        .bind(&request.privacy_class)
        .bind(&request.required_capabilities)
        .bind(request.maximum_cost_per_request)
        .bind(request.maximum_input_tokens)
        .bind(request.maximum_output_tokens)
        .bind(request.timeout_ms)
        .bind(&request.retry_policy)
        .bind(&request.metadata)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("routing policy {id}")))?;
        routing_policy_record_from_row(&row)
    }

    pub async fn set_routing_policy_status(
        &self,
        id: Uuid,
        status: &str,
    ) -> Result<RoutingPolicyRecord, AppError> {
        let row = sqlx::query(
            r#"
            update routing_policies
            set status = $2,
                deleted_at = case when $2 = 'deleted' then coalesce(deleted_at, now()) else deleted_at end,
                updated_at = now()
            where id = $1 and deleted_at is null
            returning id, application_id, external_tenant_id, route_id, provider_id,
                      provider_model_id, priority, weight, cost_weight, latency_weight,
                      quality_weight, privacy_class, required_capabilities,
                      maximum_cost_per_request, maximum_input_tokens, maximum_output_tokens,
                      timeout_ms, retry_policy, status, metadata, created_at, updated_at,
                      deleted_at, version
            "#,
        )
        .bind(id)
        .bind(status)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("routing policy {id}")))?;
        routing_policy_record_from_row(&row)
    }

    pub async fn soft_delete_routing_policy(&self, id: Uuid) -> Result<(), AppError> {
        let result = sqlx::query(
            "update routing_policies set status = 'deleted', deleted_at = now(), updated_at = now() where id = $1 and deleted_at is null",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        ensure_affected(result.rows_affected(), format!("routing policy {id}"))
    }

    pub async fn create_agent_profile(
        &self,
        id: Uuid,
        request: &AgentProfileCreateRequest,
    ) -> Result<AgentProfileRecord, AppError> {
        let row = sqlx::query(
            r#"
            insert into agent_profiles
                (id, profile_key, display_name, preamble, temperature, max_tokens,
                 tool_policy, context_policy, memory_policy, metadata)
            values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
            returning id, profile_key, display_name, preamble, temperature, max_tokens,
                      tool_policy, context_policy, memory_policy, status, metadata,
                      created_at, updated_at, deleted_at, version
            "#,
        )
        .bind(id)
        .bind(&request.profile_key)
        .bind(&request.display_name)
        .bind(&request.preamble)
        .bind(request.temperature)
        .bind(request.max_tokens)
        .bind(&request.tool_policy)
        .bind(&request.context_policy)
        .bind(&request.memory_policy)
        .bind(&request.metadata)
        .fetch_one(&self.pool)
        .await?;
        agent_profile_record_from_row(&row)
    }

    pub async fn list_agent_profiles(
        &self,
        limit: i64,
    ) -> Result<Vec<AgentProfileRecord>, AppError> {
        let rows = sqlx::query(
            r#"
            select id, profile_key, display_name, preamble, temperature, max_tokens,
                   tool_policy, context_policy, memory_policy, status, metadata,
                   created_at, updated_at, deleted_at, version
            from agent_profiles
            where deleted_at is null
            order by created_at desc, id desc
            limit $1
            "#,
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(agent_profile_record_from_row).collect()
    }

    pub async fn get_agent_profile(&self, id: Uuid) -> Result<AgentProfileRecord, AppError> {
        let row = sqlx::query(&agent_profile_select(
            "where id = $1 and deleted_at is null",
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("agent profile {id}")))?;
        agent_profile_record_from_row(&row)
    }

    pub async fn get_active_agent_profile(
        &self,
        id: Uuid,
    ) -> Result<Option<AgentProfileRecord>, AppError> {
        let row = sqlx::query(&agent_profile_select(
            "where id = $1 and status = 'active' and deleted_at is null",
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(agent_profile_record_from_row).transpose()
    }

    pub async fn patch_agent_profile(
        &self,
        id: Uuid,
        request: &AgentProfilePatchRequest,
    ) -> Result<AgentProfileRecord, AppError> {
        let row = sqlx::query(
            r#"
            update agent_profiles
            set display_name = coalesce($2, display_name),
                preamble = coalesce($3, preamble),
                temperature = coalesce($4, temperature),
                max_tokens = coalesce($5, max_tokens),
                tool_policy = coalesce($6, tool_policy),
                context_policy = coalesce($7, context_policy),
                memory_policy = coalesce($8, memory_policy),
                metadata = coalesce($9, metadata),
                updated_at = now()
            where id = $1 and deleted_at is null
            returning id, profile_key, display_name, preamble, temperature, max_tokens,
                      tool_policy, context_policy, memory_policy, status, metadata,
                      created_at, updated_at, deleted_at, version
            "#,
        )
        .bind(id)
        .bind(&request.display_name)
        .bind(&request.preamble)
        .bind(request.temperature)
        .bind(request.max_tokens)
        .bind(&request.tool_policy)
        .bind(&request.context_policy)
        .bind(&request.memory_policy)
        .bind(&request.metadata)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("agent profile {id}")))?;
        agent_profile_record_from_row(&row)
    }

    pub async fn set_agent_profile_status(
        &self,
        id: Uuid,
        status: &str,
    ) -> Result<AgentProfileRecord, AppError> {
        let row = sqlx::query(
            r#"
            update agent_profiles
            set status = $2,
                deleted_at = case when $2 = 'deleted' then coalesce(deleted_at, now()) else deleted_at end,
                updated_at = now()
            where id = $1 and deleted_at is null
            returning id, profile_key, display_name, preamble, temperature, max_tokens,
                      tool_policy, context_policy, memory_policy, status, metadata,
                      created_at, updated_at, deleted_at, version
            "#,
        )
        .bind(id)
        .bind(status)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("agent profile {id}")))?;
        agent_profile_record_from_row(&row)
    }

    pub async fn soft_delete_agent_profile(&self, id: Uuid) -> Result<(), AppError> {
        let result = sqlx::query(
            "update agent_profiles set status = 'deleted', deleted_at = now(), updated_at = now() where id = $1 and deleted_at is null",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        ensure_affected(result.rows_affected(), format!("agent profile {id}"))
    }

    pub async fn get_provider_runtime_policy(
        &self,
        provider_id: Uuid,
    ) -> Result<ProviderRuntimePolicyRecord, AppError> {
        let row = sqlx::query(&provider_runtime_policy_select("where provider_id = $1"))
            .bind(provider_id)
            .fetch_optional(&self.pool)
            .await?;
        match row {
            Some(row) => provider_runtime_policy_record_from_row(&row),
            None => Ok(default_provider_runtime_policy(provider_id)),
        }
    }

    pub async fn put_provider_runtime_policy(
        &self,
        provider_id: Uuid,
        request: &ProviderRuntimePolicyPutRequest,
    ) -> Result<ProviderRuntimePolicyRecord, AppError> {
        let status = request.status.as_ref().map(runtime_policy_status_to_db);
        let row = sqlx::query(
            r#"
            insert into provider_runtime_policies
                (provider_id, connect_timeout_ms, request_timeout_ms, stream_idle_timeout_ms,
                 max_concurrent_requests, max_concurrent_streams, retry_limit,
                 retry_base_delay_ms, retry_max_delay_ms, circuit_failure_threshold,
                 circuit_open_duration_ms, status)
            values (
                $1,
                coalesce($2, 5000),
                coalesce($3, 120000),
                coalesce($4, 30000),
                coalesce($5, 100),
                coalesce($6, 50),
                coalesce($7, 2),
                coalesce($8, 100),
                coalesce($9, 2000),
                coalesce($10, 5),
                coalesce($11, 30000),
                coalesce($12, 'active')
            )
            on conflict (provider_id) do update set
                connect_timeout_ms = coalesce($2, provider_runtime_policies.connect_timeout_ms),
                request_timeout_ms = coalesce($3, provider_runtime_policies.request_timeout_ms),
                stream_idle_timeout_ms = coalesce($4, provider_runtime_policies.stream_idle_timeout_ms),
                max_concurrent_requests = coalesce($5, provider_runtime_policies.max_concurrent_requests),
                max_concurrent_streams = coalesce($6, provider_runtime_policies.max_concurrent_streams),
                retry_limit = coalesce($7, provider_runtime_policies.retry_limit),
                retry_base_delay_ms = coalesce($8, provider_runtime_policies.retry_base_delay_ms),
                retry_max_delay_ms = coalesce($9, provider_runtime_policies.retry_max_delay_ms),
                circuit_failure_threshold = coalesce($10, provider_runtime_policies.circuit_failure_threshold),
                circuit_open_duration_ms = coalesce($11, provider_runtime_policies.circuit_open_duration_ms),
                status = coalesce($12, provider_runtime_policies.status),
                updated_at = now()
            returning id, provider_id, connect_timeout_ms, request_timeout_ms,
                      stream_idle_timeout_ms, max_concurrent_requests, max_concurrent_streams,
                      retry_limit, retry_base_delay_ms, retry_max_delay_ms,
                      circuit_failure_threshold, circuit_open_duration_ms, status,
                      updated_at, version
            "#,
        )
        .bind(provider_id)
        .bind(request.connect_timeout_ms)
        .bind(request.request_timeout_ms)
        .bind(request.stream_idle_timeout_ms)
        .bind(request.max_concurrent_requests)
        .bind(request.max_concurrent_streams)
        .bind(request.retry_limit)
        .bind(request.retry_base_delay_ms)
        .bind(request.retry_max_delay_ms)
        .bind(request.circuit_failure_threshold)
        .bind(request.circuit_open_duration_ms)
        .bind(status)
        .fetch_one(&self.pool)
        .await?;
        provider_runtime_policy_record_from_row(&row)
    }

    pub async fn ensure_application_active(&self, application_id: Uuid) -> Result<(), AppError> {
        let exists = sqlx::query_scalar::<_, bool>(
            "select exists(select 1 from applications where id = $1 and status = 'active' and deleted_at is null)",
        )
        .bind(application_id)
        .fetch_one(&self.pool)
        .await?;
        if exists {
            Ok(())
        } else {
            Err(AppError::Forbidden("application is not active".to_string()))
        }
    }

    pub async fn list_model_candidates(
        &self,
        route_id: Uuid,
        application_id: Option<Uuid>,
        external_tenant_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<ModelCandidate>, AppError> {
        let rows = sqlx::query(
            r#"
            select
                rp.id as policy_id,
                p.id as provider_id,
                p.version as provider_version,
                p.provider_type,
                p.display_name as provider_display_name,
                p.base_url,
                pm.id as provider_model_id,
                pm.version as model_version,
                pm.model_key,
                pm.capabilities,
                rp.priority as policy_priority,
                rp.weight,
                rp.timeout_ms,
                rp.retry_policy,
                coalesce(prp.id, p.id) as runtime_policy_id,
                p.id as runtime_provider_id,
                coalesce(prp.connect_timeout_ms, 5000) as connect_timeout_ms,
                coalesce(prp.request_timeout_ms, 120000) as request_timeout_ms,
                coalesce(prp.stream_idle_timeout_ms, 30000) as stream_idle_timeout_ms,
                coalesce(prp.max_concurrent_requests, 100) as max_concurrent_requests,
                coalesce(prp.max_concurrent_streams, 50) as max_concurrent_streams,
                coalesce(prp.retry_limit, 2) as retry_limit,
                coalesce(prp.retry_base_delay_ms, 100) as retry_base_delay_ms,
                coalesce(prp.retry_max_delay_ms, 2000) as retry_max_delay_ms,
                coalesce(prp.circuit_failure_threshold, 5) as circuit_failure_threshold,
                coalesce(prp.circuit_open_duration_ms, 30000) as circuit_open_duration_ms,
                coalesce(prp.status, 'active') as runtime_status,
                coalesce(prp.updated_at, p.updated_at) as runtime_updated_at,
                coalesce(prp.version, 1) as runtime_version
            from routing_policies rp
            join providers p on p.id = rp.provider_id
            join provider_models pm
              on pm.id = rp.provider_model_id
             and pm.provider_id = rp.provider_id
            left join provider_runtime_policies prp on prp.provider_id = p.id
            where rp.route_id = $1
              and rp.status = 'active'
              and rp.deleted_at is null
              and p.status = 'active'
              and p.deleted_at is null
              and pm.status = 'active'
              and pm.deleted_at is null
              and (rp.application_id is null or ($2::uuid is not null and rp.application_id = $2))
              and (rp.external_tenant_id is null or ($3::text is not null and rp.external_tenant_id = $3))
              and coalesce(prp.status, 'active') = 'active'
            order by
              case
                when rp.application_id is not null and rp.external_tenant_id is not null then 0
                when rp.application_id is not null then 1
                when rp.external_tenant_id is not null then 2
                else 3
              end,
              rp.priority asc,
              rp.weight desc,
              p.id asc,
              pm.id asc
            limit $4
            "#,
        )
        .bind(route_id)
        .bind(application_id)
        .bind(external_tenant_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        rows.iter()
            .map(|row| {
                Ok(ModelCandidate {
                    policy_id: row.try_get("policy_id")?,
                    provider_id: row.try_get("provider_id")?,
                    provider_version: row.try_get("provider_version")?,
                    provider_type: provider_type_from_db(
                        row.try_get::<String, _>("provider_type")?,
                    )?,
                    provider_display_name: row.try_get("provider_display_name")?,
                    base_url: row.try_get("base_url")?,
                    provider_model_id: row.try_get("provider_model_id")?,
                    model_version: row.try_get("model_version")?,
                    model_key: row.try_get("model_key")?,
                    capabilities: row.try_get("capabilities")?,
                    policy_priority: row.try_get("policy_priority")?,
                    weight: row.try_get("weight")?,
                    timeout_ms: row.try_get("timeout_ms")?,
                    retry_policy: row.try_get("retry_policy")?,
                    runtime_policy: ProviderRuntimePolicyRecord {
                        id: row.try_get("runtime_policy_id")?,
                        provider_id: row.try_get("runtime_provider_id")?,
                        connect_timeout_ms: row.try_get("connect_timeout_ms")?,
                        request_timeout_ms: row.try_get("request_timeout_ms")?,
                        stream_idle_timeout_ms: row.try_get("stream_idle_timeout_ms")?,
                        max_concurrent_requests: row.try_get("max_concurrent_requests")?,
                        max_concurrent_streams: row.try_get("max_concurrent_streams")?,
                        retry_limit: row.try_get("retry_limit")?,
                        retry_base_delay_ms: row.try_get("retry_base_delay_ms")?,
                        retry_max_delay_ms: row.try_get("retry_max_delay_ms")?,
                        circuit_failure_threshold: row.try_get("circuit_failure_threshold")?,
                        circuit_open_duration_ms: row.try_get("circuit_open_duration_ms")?,
                        status: crate::infra::pg_rows::runtime_policy_status_from_db(
                            row.try_get::<String, _>("runtime_status")?,
                        )?,
                        updated_at: row.try_get("runtime_updated_at")?,
                        version: row.try_get("runtime_version")?,
                    },
                })
            })
            .collect()
    }

    pub async fn resolve_runtime_credential(
        &self,
        provider_id: Uuid,
        credential_types: &[CredentialType],
        application_id: Option<Uuid>,
        external_tenant_id: Option<&str>,
        external_user_id: Option<&str>,
        explicit_credential_id: Option<Uuid>,
    ) -> Result<Option<RuntimeCredentialCandidate>, AppError> {
        let credential_type_values = credential_types
            .iter()
            .map(|credential_type| credential_type_to_db(credential_type).to_string())
            .collect::<Vec<_>>();
        let rows = sqlx::query(
            r#"
            select id, provider_id, credential_type, scope_type, external_tenant_id,
                   application_id, external_user_id, encryption_algorithm,
                   encryption_version, encrypted_data_key, nonce, encrypted_payload,
                   secret_fingerprint, masked_secret, status, priority, expires_at,
                   last_validated_at, last_used_at, metadata, display_name,
                   created_at, updated_at, deleted_at, version
            from provider_credentials
            where provider_id = $1
              and credential_type = any($2)
              and status = 'active'
              and deleted_at is null
              and (expires_at is null or expires_at > now())
              and ($6::uuid is null or id = $6)
              and (
                (scope_type = 'user'
                    and $5::text is not null
                    and external_user_id = $5
                    and (application_id is null or ($3::uuid is not null and application_id = $3))
                    and (external_tenant_id is null or ($4::text is not null and external_tenant_id = $4)))
                or (scope_type = 'application'
                    and $3::uuid is not null
                    and application_id = $3
                    and (external_tenant_id is null or ($4::text is not null and external_tenant_id = $4)))
                or (scope_type = 'tenant'
                    and $4::text is not null
                    and external_tenant_id = $4)
                or (scope_type = 'global')
              )
            order by
              case
                when $6::uuid is not null and id = $6 then 0
                when scope_type = 'user' and application_id is not null and external_tenant_id is not null then 1
                when scope_type = 'user' and application_id is not null then 2
                when scope_type = 'user' and external_tenant_id is not null then 3
                when scope_type = 'user' then 4
                when scope_type = 'application' and external_tenant_id is not null then 5
                when scope_type = 'application' then 6
                when scope_type = 'tenant' then 7
                else 8
              end,
              priority asc,
              last_validated_at desc nulls last,
              last_used_at desc nulls last,
              created_at desc,
              id desc
            limit 1
            "#,
        )
        .bind(provider_id)
        .bind(&credential_type_values)
        .bind(application_id)
        .bind(external_tenant_id)
        .bind(external_user_id)
        .bind(explicit_credential_id)
        .fetch_optional(&self.pool)
        .await?;

        let Some(row) = rows else {
            return Ok(None);
        };
        let record = credential_record_from_row(&row)?;
        let source = if explicit_credential_id == Some(record.id) {
            CredentialDecisionSource::Explicit
        } else {
            credential_source_from_record(&record)
        };
        Ok(Some(RuntimeCredentialCandidate {
            encrypted: EncryptedSecret {
                algorithm: row.try_get("encryption_algorithm")?,
                version: row.try_get("encryption_version")?,
                key_id: String::new(),
                encrypted_data_key: row.try_get("encrypted_data_key")?,
                nonce: row.try_get("nonce")?,
                ciphertext: row.try_get("encrypted_payload")?,
            },
            record,
            source,
        }))
    }

    pub async fn insert_attempt_started(
        &self,
        insert: &ExecutionAttemptInsert,
    ) -> Result<(), AppError> {
        sqlx::query(
            r#"
            insert into execution_attempts
                (id, request_id, execution_id, attempt_number, application_id,
                 external_tenant_id, external_user_id, route_id, provider_id,
                 provider_model_id, credential_id, status, metadata)
            values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, 'started', $12)
            "#,
        )
        .bind(insert.id)
        .bind(&insert.request_id)
        .bind(insert.execution_id)
        .bind(insert.attempt_number)
        .bind(insert.application_id)
        .bind(&insert.external_tenant_id)
        .bind(&insert.external_user_id)
        .bind(insert.route_id)
        .bind(insert.provider_id)
        .bind(insert.provider_model_id)
        .bind(insert.credential_id)
        .bind(&insert.metadata)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn update_attempt(
        &self,
        attempt_id: Uuid,
        update: &ExecutionAttemptUpdate,
    ) -> Result<(), AppError> {
        sqlx::query(
            r#"
            update execution_attempts
            set status = $2,
                failure_class = $3,
                provider_status_code = $4,
                completed_at = now(),
                latency_ms = $5,
                input_tokens = $6,
                output_tokens = $7,
                cached_input_tokens = $8,
                reasoning_tokens = $9,
                total_tokens = $10,
                provider_request_id = $11,
                metadata = $12
            where id = $1
            "#,
        )
        .bind(attempt_id)
        .bind(attempt_status_to_db(update.status))
        .bind(update.failure_class.map(execution_failure_class_to_db))
        .bind(update.provider_status_code)
        .bind(update.latency_ms)
        .bind(update.usage.input_tokens.map(u64_to_i64).transpose()?)
        .bind(update.usage.output_tokens.map(u64_to_i64).transpose()?)
        .bind(
            update
                .usage
                .cached_input_tokens
                .map(u64_to_i64)
                .transpose()?,
        )
        .bind(update.usage.reasoning_tokens.map(u64_to_i64).transpose()?)
        .bind(update.usage.total_tokens.map(u64_to_i64).transpose()?)
        .bind(&update.provider_request_id)
        .bind(&update.metadata)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn insert_usage_record(&self, insert: &UsageRecordInsert) -> Result<(), AppError> {
        sqlx::query(
            r#"
            insert into usage_records
                (id, request_id, execution_id, attempt_id, application_id,
                 external_tenant_id, external_user_id, provider_id, provider_model_id,
                 credential_id, input_tokens, output_tokens, cached_input_tokens,
                 reasoning_tokens, total_tokens, estimated_total_cost, currency, metadata)
            values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10,
                    $11, $12, $13, $14, $15, null, null, $16)
            "#,
        )
        .bind(insert.id)
        .bind(&insert.request_id)
        .bind(insert.execution_id)
        .bind(insert.attempt_id)
        .bind(insert.application_id)
        .bind(&insert.external_tenant_id)
        .bind(&insert.external_user_id)
        .bind(insert.provider_id)
        .bind(insert.provider_model_id)
        .bind(insert.credential_id)
        .bind(insert.usage.input_tokens.map(u64_to_i64).transpose()?)
        .bind(insert.usage.output_tokens.map(u64_to_i64).transpose()?)
        .bind(
            insert
                .usage
                .cached_input_tokens
                .map(u64_to_i64)
                .transpose()?,
        )
        .bind(insert.usage.reasoning_tokens.map(u64_to_i64).transpose()?)
        .bind(insert.usage.total_tokens.map(u64_to_i64).transpose()?)
        .bind(&insert.metadata)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn touch_credential_used(&self, credential_id: Uuid) -> Result<(), AppError> {
        sqlx::query(
            "update provider_credentials set last_used_at = now(), updated_at = now() where id = $1",
        )
        .bind(credential_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

fn route_definition_select(suffix: &str) -> String {
    format!(
        r#"
        select id, route_key, display_name, description, status, selection_strategy,
               agent_profile_id, metadata, created_at, updated_at, deleted_at, version
        from route_definitions
        {suffix}
        "#
    )
}

fn routing_policy_insert_sql() -> &'static str {
    r#"
    insert into routing_policies
        (id, application_id, external_tenant_id, route_id, provider_id,
         provider_model_id, priority, weight, cost_weight, latency_weight,
         quality_weight, privacy_class, required_capabilities,
         maximum_cost_per_request, maximum_input_tokens, maximum_output_tokens,
         timeout_ms, retry_policy, metadata)
    values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10,
            $11, $12, $13, $14, $15, $16, $17, $18, $19)
    returning id, application_id, external_tenant_id, route_id, provider_id,
              provider_model_id, priority, weight, cost_weight, latency_weight,
              quality_weight, privacy_class, required_capabilities,
              maximum_cost_per_request, maximum_input_tokens, maximum_output_tokens,
              timeout_ms, retry_policy, status, metadata, created_at, updated_at,
              deleted_at, version
    "#
}

fn routing_policy_select(suffix: &str) -> String {
    format!(
        r#"
        select id, application_id, external_tenant_id, route_id, provider_id,
               provider_model_id, priority, weight, cost_weight, latency_weight,
               quality_weight, privacy_class, required_capabilities,
               maximum_cost_per_request, maximum_input_tokens, maximum_output_tokens,
               timeout_ms, retry_policy, status, metadata, created_at, updated_at,
               deleted_at, version
        from routing_policies
        {suffix}
        "#
    )
}

fn agent_profile_select(suffix: &str) -> String {
    format!(
        r#"
        select id, profile_key, display_name, preamble, temperature, max_tokens,
               tool_policy, context_policy, memory_policy, status, metadata,
               created_at, updated_at, deleted_at, version
        from agent_profiles
        {suffix}
        "#
    )
}

fn provider_runtime_policy_select(suffix: &str) -> String {
    format!(
        r#"
        select id, provider_id, connect_timeout_ms, request_timeout_ms,
               stream_idle_timeout_ms, max_concurrent_requests, max_concurrent_streams,
               retry_limit, retry_base_delay_ms, retry_max_delay_ms,
               circuit_failure_threshold, circuit_open_duration_ms, status,
               updated_at, version
        from provider_runtime_policies
        {suffix}
        "#
    )
}

fn default_provider_runtime_policy(provider_id: Uuid) -> ProviderRuntimePolicyRecord {
    ProviderRuntimePolicyRecord {
        id: provider_id,
        provider_id,
        connect_timeout_ms: 5_000,
        request_timeout_ms: 120_000,
        stream_idle_timeout_ms: 30_000,
        max_concurrent_requests: 100,
        max_concurrent_streams: 50,
        retry_limit: 2,
        retry_base_delay_ms: 100,
        retry_max_delay_ms: 2_000,
        circuit_failure_threshold: 5,
        circuit_open_duration_ms: 30_000,
        status: crate::domain::RuntimePolicyStatus::Active,
        updated_at: Utc::now(),
        version: 1,
    }
}

fn credential_source_from_record(record: &CredentialRecord) -> CredentialDecisionSource {
    match record.scope_type {
        crate::domain::ScopeType::User
            if record.application_id.is_some() && record.external_tenant_id.is_some() =>
        {
            CredentialDecisionSource::UserApplicationTenant
        }
        crate::domain::ScopeType::User if record.application_id.is_some() => {
            CredentialDecisionSource::UserApplication
        }
        crate::domain::ScopeType::User if record.external_tenant_id.is_some() => {
            CredentialDecisionSource::UserTenant
        }
        crate::domain::ScopeType::User => CredentialDecisionSource::User,
        crate::domain::ScopeType::Application if record.external_tenant_id.is_some() => {
            CredentialDecisionSource::ApplicationTenant
        }
        crate::domain::ScopeType::Application => CredentialDecisionSource::Application,
        crate::domain::ScopeType::Tenant => CredentialDecisionSource::Tenant,
        crate::domain::ScopeType::Global => CredentialDecisionSource::Global,
    }
}

pub fn execution_failure_class_to_db(class: ExecutionFailureClass) -> &'static str {
    match class {
        ExecutionFailureClass::InvalidExecutionRequest => "invalid_execution_request",
        ExecutionFailureClass::ApplicationUnavailable => "application_unavailable",
        ExecutionFailureClass::RouteNotFound => "route_not_found",
        ExecutionFailureClass::RouteForbidden => "route_forbidden",
        ExecutionFailureClass::ModelNotFound => "model_not_found",
        ExecutionFailureClass::ModelForbidden => "model_forbidden",
        ExecutionFailureClass::ModelCapabilityMismatch => "model_capability_mismatch",
        ExecutionFailureClass::NoEligibleModel => "no_eligible_model",
        ExecutionFailureClass::CredentialNotFound => "credential_not_found",
        ExecutionFailureClass::CredentialForbidden => "credential_forbidden",
        ExecutionFailureClass::CredentialExpired => "credential_expired",
        ExecutionFailureClass::CredentialDisabled => "credential_disabled",
        ExecutionFailureClass::CredentialDecryptionFailed => "credential_decryption_failed",
        ExecutionFailureClass::ProviderConfigurationInvalid => "provider_configuration_invalid",
        ExecutionFailureClass::ProviderUnavailable => "provider_unavailable",
        ExecutionFailureClass::ProviderRateLimited => "provider_rate_limited",
        ExecutionFailureClass::ProviderTimeout => "provider_timeout",
        ExecutionFailureClass::ProviderConnectionFailed => "provider_connection_failed",
        ExecutionFailureClass::ProviderAuthenticationFailed => "provider_authentication_failed",
        ExecutionFailureClass::ProviderInvalidResponse => "provider_invalid_response",
        ExecutionFailureClass::ProviderUpstreamError => "provider_upstream_error",
        ExecutionFailureClass::CircuitOpen => "circuit_open",
        ExecutionFailureClass::CapacityExhausted => "capacity_exhausted",
        ExecutionFailureClass::RequestCancelled => "request_cancelled",
        ExecutionFailureClass::DeadlineExceeded => "deadline_exceeded",
        ExecutionFailureClass::StructuredOutputInvalid => "structured_output_invalid",
        ExecutionFailureClass::StreamBackpressureExceeded => "stream_backpressure_exceeded",
        ExecutionFailureClass::InternalError => "internal_error",
    }
}

fn attempt_status_to_db(status: AttemptStatus) -> &'static str {
    match status {
        AttemptStatus::Started => "started",
        AttemptStatus::Succeeded => "succeeded",
        AttemptStatus::Failed => "failed",
        AttemptStatus::Cancelled => "cancelled",
    }
}

fn u64_to_i64(value: u64) -> Result<i64, AppError> {
    i64::try_from(value)
        .map_err(|_| AppError::Internal("usage value exceeds storage range".to_string()))
}

fn ensure_affected(rows: u64, target: String) -> Result<(), AppError> {
    if rows == 0 {
        Err(AppError::NotFound(target))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{CredentialScope, CredentialStatus, ScopeType};

    #[test]
    fn credential_source_matches_phase_three_precedence() {
        let record = CredentialRecord {
            id: Uuid::now_v7(),
            provider_id: Uuid::now_v7(),
            credential_type: CredentialType::ApiKey,
            scope: CredentialScope::User {
                external_user_id: "user".to_string(),
                application_id: Some(Uuid::now_v7()),
                external_tenant_id: Some("tenant".to_string()),
            },
            display_name: None,
            scope_type: ScopeType::User,
            external_tenant_id: Some("tenant".to_string()),
            application_id: Some(Uuid::now_v7()),
            external_user_id: Some("user".to_string()),
            encryption_algorithm: "AES-256-GCM".to_string(),
            encryption_version: 1,
            secret_fingerprint: "fp".to_string(),
            masked_secret: "****".to_string(),
            status: CredentialStatus::Active,
            priority: 100,
            expires_at: None,
            last_validated_at: None,
            last_used_at: None,
            metadata: serde_json::json!({}),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            deleted_at: None,
            version: 1,
        };
        assert_eq!(
            credential_source_from_record(&record),
            CredentialDecisionSource::UserApplicationTenant
        );
    }
}
