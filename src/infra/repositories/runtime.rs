use async_trait::async_trait;
use chrono::Utc;
use serde_json::Value;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::{
    domain::{
        AgentProfileCreateRequest, AgentProfilePatchRequest, AgentProfileRecord, AttemptStatus,
        AuditLogInsert, CredentialDecisionSource, CredentialRecord, CredentialType,
        ExecutionFailureClass, ListCursor, ModelCandidate, ProviderRuntimePolicyPutRequest,
        ProviderRuntimePolicyRecord, ResolvedProviderConfiguration, RouteDefinitionCreateRequest,
        RouteDefinitionPatchRequest, RouteDefinitionRecord, RoutingPolicyCreateRequest,
        RoutingPolicyPatchRequest, RoutingPolicyRecord, UsageSummary,
    },
    error::AppError,
    infra::{
        pg_rows::{
            agent_profile_record_from_row, credential_record_from_row, credential_type_to_db,
            provider_runtime_policy_record_from_row, provider_type_from_db,
            route_definition_record_from_row, route_selection_strategy_to_db,
            routing_policy_record_from_row, runtime_policy_status_to_db,
        },
        repositories::admin::insert_audit_with_connection,
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

/// The runtime persistence surface: route definitions, routing policies, agent profiles and
/// provider runtime policies (the runtime-admin write side), plus model-candidate resolution,
/// credential resolution, execution-attempt rows and usage records (the execution read/write side).
///
/// Extracted as a trait (plan 06, Module 8 / P2-3) so `RuntimeAdminService` and the execution
/// pipeline can be unit-tested against a fake instead of a live Postgres. Mirrors
/// [`AdminRepository`](super::AdminRepository): the trait carries the documentation, the
/// `#[async_trait] impl` below carries only SQL.
///
/// `resolve_runtime_credential` is the single live credential-precedence implementation in the
/// process; it returns encrypted material only — decryption stays in `src/security/crypto.rs`, so
/// an implementation of this trait never sees or produces plaintext secrets.
///
/// # Every mutation carries its `audit` row, and the write commits it
///
/// A method that changes state takes an [`AuditLogInsert`] and writes it **on the same
/// connection, inside the same transaction** as the change itself. It is a required
/// parameter rather than a convention because the alternative had already gone wrong:
/// thirty-six admin mutations used to write the row afterwards through
/// `PgAdminRepository::insert_audit`, which acquires a *second* pooled connection. The two
/// statements then commit separately, and any failure between them — a `22001` from an
/// over-long `x-request-id`, a pool timeout, a dropped request future, a killed pod —
/// leaves the administrative change committed with no record of it.
///
/// Only that direction was ever reachable, and it is the serious one. The audit row is
/// written *after* the row lock and version check, so a `409` or `404` still writes
/// nothing.
///
/// Do not add a mutating method here without an `audit` parameter, and do not restore a
/// caller-side `insert_audit` on a success path. `audit_denied`
/// (`src/application/admin/shared.rs`) is the one deliberate exception: it records a
/// refusal, so there is no write for it to be atomic with, and it is swallowed on purpose.
/// Pinned end to end by `tests/admin_audit_atomicity.rs`.
#[async_trait]
pub trait RuntimeRepository: Send + Sync {
    async fn create_route_definition(
        &self,
        id: Uuid,
        request: &RouteDefinitionCreateRequest,
        audit: AuditLogInsert,
    ) -> Result<RouteDefinitionRecord, AppError>;

    /// Lists route definitions newest-first, keyset-paginated on `(created_at, id)`.
    ///
    /// `cursor` bounds the page to rows strictly *after* the previously returned last row
    /// (i.e. strictly less than the cursor under `created_at desc, id desc` ordering); `None`
    /// returns the first page. Returns up to `limit + 1` rows so the caller (application
    /// layer) can detect `has_more` without a second `count(*)` query and must trim the
    /// over-fetched row itself before returning it to a client. `cursor.ts`/`cursor.id` are
    /// always bound as parameters, never interpolated into the query text — see
    /// `keyset_predicate_binds_parameters_and_never_interpolates_values` below, which pins
    /// the query text used here.
    async fn list_route_definitions(
        &self,
        cursor: Option<ListCursor>,
        limit: i64,
    ) -> Result<Vec<RouteDefinitionRecord>, AppError>;

    async fn get_route_definition(&self, id: Uuid) -> Result<RouteDefinitionRecord, AppError>;

    async fn get_active_route_by_key(
        &self,
        route_key: &str,
    ) -> Result<Option<RouteDefinitionRecord>, AppError>;

    async fn get_default_route(&self) -> Result<Option<RouteDefinitionRecord>, AppError>;

    /// `expected_version` is the caller's `If-Match` expectation, evaluated by the
    /// implementation *inside the same transaction as the write* (plan 06b). The HTTP layer
    /// no longer pre-reads and compares: a version read on one pooled connection and compared
    /// in Rust cannot exclude a concurrent writer, so two callers holding the same
    /// currently-valid version both used to succeed and the first one's values were lost.
    ///
    /// A mismatch is a `409` `resource_version_conflict`; an absent row stays `404`.
    /// The same contract applies to every method on this trait taking `expected_version`.
    async fn patch_route_definition(
        &self,
        id: Uuid,
        expected_version: i64,
        request: &RouteDefinitionPatchRequest,
        audit: AuditLogInsert,
    ) -> Result<RouteDefinitionRecord, AppError>;

    async fn set_route_definition_status(
        &self,
        id: Uuid,
        expected_version: i64,
        status: &str,
        audit: AuditLogInsert,
    ) -> Result<RouteDefinitionRecord, AppError>;

    async fn soft_delete_route_definition(
        &self,
        id: Uuid,
        expected_version: i64,
        audit: AuditLogInsert,
    ) -> Result<(), AppError>;

    async fn create_routing_policy(
        &self,
        id: Uuid,
        request: &RoutingPolicyCreateRequest,
        audit: AuditLogInsert,
    ) -> Result<RoutingPolicyRecord, AppError>;

    /// See [`Self::list_route_definitions`] for the keyset-pagination contract this shares.
    async fn list_routing_policies(
        &self,
        cursor: Option<ListCursor>,
        limit: i64,
    ) -> Result<Vec<RoutingPolicyRecord>, AppError>;

    async fn get_routing_policy(&self, id: Uuid) -> Result<RoutingPolicyRecord, AppError>;

    async fn provider_model_belongs_to_provider(
        &self,
        provider_id: Uuid,
        provider_model_id: Uuid,
    ) -> Result<bool, AppError>;

    /// See [`Self::patch_route_definition`] for the `expected_version` contract.
    async fn patch_routing_policy(
        &self,
        id: Uuid,
        expected_version: i64,
        request: &RoutingPolicyPatchRequest,
        audit: AuditLogInsert,
    ) -> Result<RoutingPolicyRecord, AppError>;

    async fn set_routing_policy_status(
        &self,
        id: Uuid,
        expected_version: i64,
        status: &str,
        audit: AuditLogInsert,
    ) -> Result<RoutingPolicyRecord, AppError>;

    async fn soft_delete_routing_policy(
        &self,
        id: Uuid,
        expected_version: i64,
        audit: AuditLogInsert,
    ) -> Result<(), AppError>;

    async fn create_agent_profile(
        &self,
        id: Uuid,
        request: &AgentProfileCreateRequest,
        audit: AuditLogInsert,
    ) -> Result<AgentProfileRecord, AppError>;

    /// See [`Self::list_route_definitions`] for the keyset-pagination contract this shares.
    async fn list_agent_profiles(
        &self,
        cursor: Option<ListCursor>,
        limit: i64,
    ) -> Result<Vec<AgentProfileRecord>, AppError>;

    async fn get_agent_profile(&self, id: Uuid) -> Result<AgentProfileRecord, AppError>;

    async fn get_active_agent_profile(
        &self,
        id: Uuid,
    ) -> Result<Option<AgentProfileRecord>, AppError>;

    /// See [`Self::patch_route_definition`] for the `expected_version` contract.
    async fn patch_agent_profile(
        &self,
        id: Uuid,
        expected_version: i64,
        request: &AgentProfilePatchRequest,
        audit: AuditLogInsert,
    ) -> Result<AgentProfileRecord, AppError>;

    async fn set_agent_profile_status(
        &self,
        id: Uuid,
        expected_version: i64,
        status: &str,
        audit: AuditLogInsert,
    ) -> Result<AgentProfileRecord, AppError>;

    async fn soft_delete_agent_profile(
        &self,
        id: Uuid,
        expected_version: i64,
        audit: AuditLogInsert,
    ) -> Result<(), AppError>;

    async fn get_provider_runtime_policy(
        &self,
        provider_id: Uuid,
    ) -> Result<ProviderRuntimePolicyRecord, AppError>;

    async fn put_provider_runtime_policy(
        &self,
        provider_id: Uuid,
        request: &ProviderRuntimePolicyPutRequest,
        audit: AuditLogInsert,
    ) -> Result<ProviderRuntimePolicyRecord, AppError>;

    async fn ensure_application_active(&self, application_id: Uuid) -> Result<(), AppError>;

    async fn list_model_candidates(
        &self,
        route_id: Uuid,
        application_id: Option<Uuid>,
        external_tenant_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<ModelCandidate>, AppError>;

    async fn resolve_runtime_credential(
        &self,
        provider_id: Uuid,
        credential_types: &[CredentialType],
        application_id: Option<Uuid>,
        external_tenant_id: Option<&str>,
        external_user_id: Option<&str>,
        explicit_credential_id: Option<Uuid>,
    ) -> Result<Option<RuntimeCredentialCandidate>, AppError>;

    async fn insert_attempt_started(&self, insert: &ExecutionAttemptInsert)
    -> Result<(), AppError>;

    async fn update_attempt(
        &self,
        attempt_id: Uuid,
        update: &ExecutionAttemptUpdate,
    ) -> Result<(), AppError>;

    async fn insert_usage_record(&self, insert: &UsageRecordInsert) -> Result<(), AppError>;

    async fn touch_credential_used(&self, credential_id: Uuid) -> Result<(), AppError>;
}

impl PgRuntimeRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl RuntimeRepository for PgRuntimeRepository {
    async fn create_route_definition(
        &self,
        id: Uuid,
        request: &RouteDefinitionCreateRequest,
        audit: AuditLogInsert,
    ) -> Result<RouteDefinitionRecord, AppError> {
        // A transaction for one `INSERT`, so the audit row shares its fate — see the
        // trait's `audit` contract.
        let mut tx = self.pool.begin().await?;
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
        .fetch_one(&mut *tx)
        .await?;
        let record = route_definition_record_from_row(&row)?;
        insert_audit_with_connection(&mut tx, audit).await?;
        tx.commit().await?;
        Ok(record)
    }

    async fn list_route_definitions(
        &self,
        cursor: Option<ListCursor>,
        limit: i64,
    ) -> Result<Vec<RouteDefinitionRecord>, AppError> {
        let rows = sqlx::query(LIST_ROUTE_DEFINITIONS_SQL)
            .bind(cursor.map(|c| c.ts))
            .bind(cursor.map(|c| c.id))
            .bind(over_fetch_limit(limit))
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(route_definition_record_from_row).collect()
    }

    async fn get_route_definition(&self, id: Uuid) -> Result<RouteDefinitionRecord, AppError> {
        let row = sqlx::query(&route_definition_select(
            "where id = $1 and deleted_at is null",
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("route {id}")))?;
        route_definition_record_from_row(&row)
    }

    async fn get_active_route_by_key(
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

    async fn get_default_route(&self) -> Result<Option<RouteDefinitionRecord>, AppError> {
        let row = sqlx::query(&route_definition_select(
            "where status = 'active' and deleted_at is null order by case when route_key = 'general' then 0 else 1 end, created_at asc limit 1",
        ))
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref()
            .map(route_definition_record_from_row)
            .transpose()
    }

    async fn patch_route_definition(
        &self,
        id: Uuid,
        expected_version: i64,
        request: &RouteDefinitionPatchRequest,
        audit: AuditLogInsert,
    ) -> Result<RouteDefinitionRecord, AppError> {
        let strategy = request
            .selection_strategy
            .as_ref()
            .map(route_selection_strategy_to_db);
        let mut tx = self.pool.begin().await?;
        let current_version = lock_and_match_version(
            &mut tx,
            ROUTE_DEFINITION_VERSION_FOR_UPDATE,
            id,
            expected_version,
            format!("route {id}"),
        )
        .await?;
        let row = sqlx::query(
            r#"
            update route_definitions
            set display_name = coalesce($2, display_name),
                description = coalesce($3, description),
                selection_strategy = coalesce($4, selection_strategy),
                agent_profile_id = coalesce($5, agent_profile_id),
                metadata = coalesce($6, metadata),
                updated_at = now()
            where id = $1 and deleted_at is null and version = $7
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
        .bind(current_version)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(version_conflict)?;
        let record = route_definition_record_from_row(&row)?;
        insert_audit_with_connection(&mut tx, audit).await?;
        tx.commit().await?;
        Ok(record)
    }

    async fn set_route_definition_status(
        &self,
        id: Uuid,
        expected_version: i64,
        status: &str,
        audit: AuditLogInsert,
    ) -> Result<RouteDefinitionRecord, AppError> {
        let mut tx = self.pool.begin().await?;
        let current_version = lock_and_match_version(
            &mut tx,
            ROUTE_DEFINITION_VERSION_FOR_UPDATE,
            id,
            expected_version,
            format!("route {id}"),
        )
        .await?;
        let row = sqlx::query(
            r#"
            update route_definitions
            set status = $2,
                deleted_at = case when $2 = 'deleted' then coalesce(deleted_at, now()) else deleted_at end,
                updated_at = now()
            where id = $1 and deleted_at is null and version = $3
            returning id, route_key, display_name, description, status, selection_strategy,
                      agent_profile_id, metadata, created_at, updated_at, deleted_at, version
            "#,
        )
        .bind(id)
        .bind(status)
        .bind(current_version)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(version_conflict)?;
        let record = route_definition_record_from_row(&row)?;
        insert_audit_with_connection(&mut tx, audit).await?;
        tx.commit().await?;
        Ok(record)
    }

    async fn soft_delete_route_definition(
        &self,
        id: Uuid,
        expected_version: i64,
        audit: AuditLogInsert,
    ) -> Result<(), AppError> {
        let mut tx = self.pool.begin().await?;
        let current_version = lock_and_match_version(
            &mut tx,
            ROUTE_DEFINITION_VERSION_FOR_UPDATE,
            id,
            expected_version,
            format!("route {id}"),
        )
        .await?;
        let result = sqlx::query(
            "update route_definitions set status = 'deleted', deleted_at = now(), updated_at = now() where id = $1 and deleted_at is null and version = $2",
        )
        .bind(id)
        .bind(current_version)
        .execute(&mut *tx)
        .await?;
        if result.rows_affected() == 0 {
            return Err(version_conflict());
        }
        insert_audit_with_connection(&mut tx, audit).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn create_routing_policy(
        &self,
        id: Uuid,
        request: &RoutingPolicyCreateRequest,
        audit: AuditLogInsert,
    ) -> Result<RoutingPolicyRecord, AppError> {
        // A transaction for one `INSERT`, so the audit row shares its fate.
        let mut tx = self.pool.begin().await?;
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
            .fetch_one(&mut *tx)
            .await?;
        let record = routing_policy_record_from_row(&row)?;
        insert_audit_with_connection(&mut tx, audit).await?;
        tx.commit().await?;
        Ok(record)
    }

    async fn list_routing_policies(
        &self,
        cursor: Option<ListCursor>,
        limit: i64,
    ) -> Result<Vec<RoutingPolicyRecord>, AppError> {
        let rows = sqlx::query(LIST_ROUTING_POLICIES_SQL)
            .bind(cursor.map(|c| c.ts))
            .bind(cursor.map(|c| c.id))
            .bind(over_fetch_limit(limit))
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(routing_policy_record_from_row).collect()
    }

    async fn get_routing_policy(&self, id: Uuid) -> Result<RoutingPolicyRecord, AppError> {
        let row = sqlx::query(&routing_policy_select(
            "where id = $1 and deleted_at is null",
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("routing policy {id}")))?;
        routing_policy_record_from_row(&row)
    }

    async fn provider_model_belongs_to_provider(
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

    async fn patch_routing_policy(
        &self,
        id: Uuid,
        expected_version: i64,
        request: &RoutingPolicyPatchRequest,
        audit: AuditLogInsert,
    ) -> Result<RoutingPolicyRecord, AppError> {
        let mut tx = self.pool.begin().await?;
        let current_version = lock_and_match_version(
            &mut tx,
            ROUTING_POLICY_VERSION_FOR_UPDATE,
            id,
            expected_version,
            format!("routing policy {id}"),
        )
        .await?;
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
            where id = $1 and deleted_at is null and version = $20
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
        .bind(current_version)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(version_conflict)?;
        let record = routing_policy_record_from_row(&row)?;
        insert_audit_with_connection(&mut tx, audit).await?;
        tx.commit().await?;
        Ok(record)
    }

    async fn set_routing_policy_status(
        &self,
        id: Uuid,
        expected_version: i64,
        status: &str,
        audit: AuditLogInsert,
    ) -> Result<RoutingPolicyRecord, AppError> {
        let mut tx = self.pool.begin().await?;
        let current_version = lock_and_match_version(
            &mut tx,
            ROUTING_POLICY_VERSION_FOR_UPDATE,
            id,
            expected_version,
            format!("routing policy {id}"),
        )
        .await?;
        let row = sqlx::query(
            r#"
            update routing_policies
            set status = $2,
                deleted_at = case when $2 = 'deleted' then coalesce(deleted_at, now()) else deleted_at end,
                updated_at = now()
            where id = $1 and deleted_at is null and version = $3
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
        .bind(current_version)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(version_conflict)?;
        let record = routing_policy_record_from_row(&row)?;
        insert_audit_with_connection(&mut tx, audit).await?;
        tx.commit().await?;
        Ok(record)
    }

    async fn soft_delete_routing_policy(
        &self,
        id: Uuid,
        expected_version: i64,
        audit: AuditLogInsert,
    ) -> Result<(), AppError> {
        let mut tx = self.pool.begin().await?;
        let current_version = lock_and_match_version(
            &mut tx,
            ROUTING_POLICY_VERSION_FOR_UPDATE,
            id,
            expected_version,
            format!("routing policy {id}"),
        )
        .await?;
        let result = sqlx::query(
            "update routing_policies set status = 'deleted', deleted_at = now(), updated_at = now() where id = $1 and deleted_at is null and version = $2",
        )
        .bind(id)
        .bind(current_version)
        .execute(&mut *tx)
        .await?;
        if result.rows_affected() == 0 {
            return Err(version_conflict());
        }
        insert_audit_with_connection(&mut tx, audit).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn create_agent_profile(
        &self,
        id: Uuid,
        request: &AgentProfileCreateRequest,
        audit: AuditLogInsert,
    ) -> Result<AgentProfileRecord, AppError> {
        // A transaction for one `INSERT`, so the audit row shares its fate.
        let mut tx = self.pool.begin().await?;
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
        .fetch_one(&mut *tx)
        .await?;
        let record = agent_profile_record_from_row(&row)?;
        insert_audit_with_connection(&mut tx, audit).await?;
        tx.commit().await?;
        Ok(record)
    }

    async fn list_agent_profiles(
        &self,
        cursor: Option<ListCursor>,
        limit: i64,
    ) -> Result<Vec<AgentProfileRecord>, AppError> {
        let rows = sqlx::query(LIST_AGENT_PROFILES_SQL)
            .bind(cursor.map(|c| c.ts))
            .bind(cursor.map(|c| c.id))
            .bind(over_fetch_limit(limit))
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(agent_profile_record_from_row).collect()
    }

    async fn get_agent_profile(&self, id: Uuid) -> Result<AgentProfileRecord, AppError> {
        let row = sqlx::query(&agent_profile_select(
            "where id = $1 and deleted_at is null",
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| AppError::NotFound(format!("agent profile {id}")))?;
        agent_profile_record_from_row(&row)
    }

    async fn get_active_agent_profile(
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

    async fn patch_agent_profile(
        &self,
        id: Uuid,
        expected_version: i64,
        request: &AgentProfilePatchRequest,
        audit: AuditLogInsert,
    ) -> Result<AgentProfileRecord, AppError> {
        let mut tx = self.pool.begin().await?;
        let current_version = lock_and_match_version(
            &mut tx,
            AGENT_PROFILE_VERSION_FOR_UPDATE,
            id,
            expected_version,
            format!("agent profile {id}"),
        )
        .await?;
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
            where id = $1 and deleted_at is null and version = $10
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
        .bind(current_version)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(version_conflict)?;
        let record = agent_profile_record_from_row(&row)?;
        insert_audit_with_connection(&mut tx, audit).await?;
        tx.commit().await?;
        Ok(record)
    }

    async fn set_agent_profile_status(
        &self,
        id: Uuid,
        expected_version: i64,
        status: &str,
        audit: AuditLogInsert,
    ) -> Result<AgentProfileRecord, AppError> {
        let mut tx = self.pool.begin().await?;
        let current_version = lock_and_match_version(
            &mut tx,
            AGENT_PROFILE_VERSION_FOR_UPDATE,
            id,
            expected_version,
            format!("agent profile {id}"),
        )
        .await?;
        let row = sqlx::query(
            r#"
            update agent_profiles
            set status = $2,
                deleted_at = case when $2 = 'deleted' then coalesce(deleted_at, now()) else deleted_at end,
                updated_at = now()
            where id = $1 and deleted_at is null and version = $3
            returning id, profile_key, display_name, preamble, temperature, max_tokens,
                      tool_policy, context_policy, memory_policy, status, metadata,
                      created_at, updated_at, deleted_at, version
            "#,
        )
        .bind(id)
        .bind(status)
        .bind(current_version)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(version_conflict)?;
        let record = agent_profile_record_from_row(&row)?;
        insert_audit_with_connection(&mut tx, audit).await?;
        tx.commit().await?;
        Ok(record)
    }

    async fn soft_delete_agent_profile(
        &self,
        id: Uuid,
        expected_version: i64,
        audit: AuditLogInsert,
    ) -> Result<(), AppError> {
        let mut tx = self.pool.begin().await?;
        let current_version = lock_and_match_version(
            &mut tx,
            AGENT_PROFILE_VERSION_FOR_UPDATE,
            id,
            expected_version,
            format!("agent profile {id}"),
        )
        .await?;
        let result = sqlx::query(
            "update agent_profiles set status = 'deleted', deleted_at = now(), updated_at = now() where id = $1 and deleted_at is null and version = $2",
        )
        .bind(id)
        .bind(current_version)
        .execute(&mut *tx)
        .await?;
        if result.rows_affected() == 0 {
            return Err(version_conflict());
        }
        insert_audit_with_connection(&mut tx, audit).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn get_provider_runtime_policy(
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

    async fn put_provider_runtime_policy(
        &self,
        provider_id: Uuid,
        request: &ProviderRuntimePolicyPutRequest,
        audit: AuditLogInsert,
    ) -> Result<ProviderRuntimePolicyRecord, AppError> {
        let status = request.status.as_ref().map(runtime_policy_status_to_db);
        // A transaction for one upsert, so the audit row shares its fate.
        let mut tx = self.pool.begin().await?;
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
        .fetch_one(&mut *tx)
        .await?;
        let record = provider_runtime_policy_record_from_row(&row)?;
        insert_audit_with_connection(&mut tx, audit).await?;
        tx.commit().await?;
        Ok(record)
    }

    async fn ensure_application_active(&self, application_id: Uuid) -> Result<(), AppError> {
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

    async fn list_model_candidates(
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

    async fn resolve_runtime_credential(
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

    async fn insert_attempt_started(
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

    async fn update_attempt(
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

    async fn insert_usage_record(&self, insert: &UsageRecordInsert) -> Result<(), AppError> {
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

    async fn touch_credential_used(&self, credential_id: Uuid) -> Result<(), AppError> {
        sqlx::query(
            "update provider_credentials set last_used_at = now(), updated_at = now() where id = $1",
        )
        .bind(credential_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

/// Keyset predicate shared by every `list_*` query in this file: `$1`/`$2` are the cursor's
/// `(ts, id)` bound as parameters (both `NULL` for the first page, which short-circuits the
/// `is null` branch and matches every row), and the strict `<` under `created_at desc, id
/// desc` ordering returns exactly the rows after the previously returned last row.
const LIST_ROUTE_DEFINITIONS_SQL: &str = r#"
    select id, route_key, display_name, description, status, selection_strategy,
           agent_profile_id, metadata, created_at, updated_at, deleted_at, version
    from route_definitions
    where deleted_at is null
      and ($1::timestamptz is null or (created_at, id) < ($1::timestamptz, $2::uuid))
    order by created_at desc, id desc
    limit $3
    "#;

const LIST_ROUTING_POLICIES_SQL: &str = r#"
    select id, application_id, external_tenant_id, route_id, provider_id,
           provider_model_id, priority, weight, cost_weight, latency_weight,
           quality_weight, privacy_class, required_capabilities,
           maximum_cost_per_request, maximum_input_tokens, maximum_output_tokens,
           timeout_ms, retry_policy, status, metadata, created_at, updated_at,
           deleted_at, version
    from routing_policies
    where deleted_at is null
      and ($1::timestamptz is null or (created_at, id) < ($1::timestamptz, $2::uuid))
    order by created_at desc, id desc
    limit $3
    "#;

const LIST_AGENT_PROFILES_SQL: &str = r#"
    select id, profile_key, display_name, preamble, temperature, max_tokens,
           tool_policy, context_policy, memory_policy, status, metadata,
           created_at, updated_at, deleted_at, version
    from agent_profiles
    where deleted_at is null
      and ($1::timestamptz is null or (created_at, id) < ($1::timestamptz, $2::uuid))
    order by created_at desc, id desc
    limit $3
    "#;

/// Over-fetches by one row so the caller can compute `has_more` without a second query.
fn over_fetch_limit(limit: i64) -> i64 {
    limit.saturating_add(1)
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

/// The `409` a stale `If-Match` produces. Identical code and message to the ones the HTTP
/// layer used to emit from its own pre-read comparison, so moving the check down here is
/// invisible on the wire.
///
/// Twin of `version_conflict` in `super::admin`, which is private there. Deliberately
/// duplicated rather than shared: the two modules own disjoint tables and neither should
/// have to import the other's internals. If you change the code or message here, change it
/// there too — they are one wire contract.
fn version_conflict() -> AppError {
    AppError::conflict(
        "resource_version_conflict",
        "resource version does not match If-Match",
    )
}

/// The row-lock statements paired with `lock_and_match_version`, one per versioned runtime
/// entity. Each takes the row's id as `$1` and every one of them ends in `for update`; a
/// variant that does not is a silent reopening of the write window. The `deleted_at is null`
/// clause mirrors the `where` clause of the `UPDATE` each one guards, so a soft-deleted row
/// is `404` here exactly as it was before.
const ROUTE_DEFINITION_VERSION_FOR_UPDATE: &str =
    "select version from route_definitions where id = $1 and deleted_at is null for update";
const ROUTING_POLICY_VERSION_FOR_UPDATE: &str =
    "select version from routing_policies where id = $1 and deleted_at is null for update";
const AGENT_PROFILE_VERSION_FOR_UPDATE: &str =
    "select version from agent_profiles where id = $1 and deleted_at is null for update";

/// Evaluate the caller's `If-Match` expectation where it is actually safe: on a row already
/// locked by `select … for update`, inside the same transaction as the write that follows.
///
/// `select_version_sql` must select `version` for the target row and end in `for update`, with
/// `$1` bound to `id`. Returns the locked version so the caller can carry `and version = $N`
/// on the write itself.
///
/// The absent-row branch stays `NotFound` and only a genuine mismatch becomes `409`. Folding
/// the predicate into the existing `UPDATE` without this pre-read would collapse both onto the
/// update's own zero-row branch and turn a stale `If-Match` into a `404`.
///
/// Twin of `lock_and_match_version` in `super::admin`; see `version_conflict` above.
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

/// Resolves the provider configuration and model key behind an embedding policy's
/// `(embedding_provider_id, embedding_model_id)` pair.
///
/// Returns `None` when either row is missing, soft-deleted or inactive, or when the model does
/// not belong to the provider — a policy that outlived the provider it names must degrade to an
/// honest ingestion failure, never to a lookup against a deleted provider's credentials.
///
/// A free function rather than a `RuntimeRepository` trait method: it has exactly one caller,
/// and `InMemoryRuntimeRepository` would have to stub a method no unit test drives.
pub(crate) async fn find_embedding_model_target(
    pool: &PgPool,
    provider_id: Uuid,
    provider_model_id: Uuid,
) -> Result<Option<(ResolvedProviderConfiguration, String)>, AppError> {
    let row = sqlx::query(
        r#"
            select p.id as provider_id,
                   p.version as provider_version,
                   p.provider_type,
                   p.display_name as provider_display_name,
                   p.base_url,
                   pm.model_key
            from providers p
            join provider_models pm on pm.id = $2 and pm.provider_id = p.id
            where p.id = $1
              and p.status = 'active'
              and p.deleted_at is null
              and pm.status = 'active'
              and pm.deleted_at is null
            "#,
    )
    .bind(provider_id)
    .bind(provider_model_id)
    .fetch_optional(pool)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    Ok(Some((
        ResolvedProviderConfiguration {
            provider_id: row.try_get("provider_id")?,
            provider_version: row.try_get("provider_version")?,
            provider_type: provider_type_from_db(row.try_get::<String, _>("provider_type")?)?,
            display_name: row.try_get("provider_display_name")?,
            base_url: row.try_get("base_url")?,
        },
        row.try_get("model_key")?,
    )))
}

/// Resolves and decrypts the credential an embedding call should use.
///
/// Reuses [`RuntimeRepository::resolve_runtime_credential`] rather than re-implementing the
/// scope-precedence ladder (explicit id, then user, application, tenant, global): a second copy
/// of that ordering would silently give embeddings different credential precedence from
/// completions for the same provider.
///
/// Scoped to `application_id` with no tenant or user, because an embedding run belongs to the
/// application that owns the collection, not to whoever happened to trigger the ingest.
pub(crate) async fn resolve_embedding_credential(
    pool: &PgPool,
    cipher: &crate::security::LocalSecretCipher,
    provider_id: Uuid,
    provider_type: crate::domain::ProviderType,
    application_id: Uuid,
) -> Result<Option<crate::domain::ResolvedCredential>, AppError> {
    use crate::security::SecretCipher;

    let credential_types: &[CredentialType] = match provider_type {
        crate::domain::ProviderType::AzureOpenAi => {
            &[CredentialType::AzureOpenAi, CredentialType::ApiKey]
        }
        _ => &[CredentialType::ApiKey, CredentialType::BearerToken],
    };
    let Some(candidate) = PgRuntimeRepository::new(pool.clone())
        .resolve_runtime_credential(
            provider_id,
            credential_types,
            Some(application_id),
            None,
            None,
            None,
        )
        .await?
    else {
        return Ok(None);
    };

    let record = candidate.record;
    let aad = crate::security::credential_aad(crate::security::CredentialAadParts {
        credential_id: record.id,
        provider_id: record.provider_id,
        credential_type: credential_type_to_db(&record.credential_type),
        scope_type: crate::infra::pg_rows::scope_type_to_db(&record.scope_type),
        external_tenant_id: record.external_tenant_id.as_deref(),
        application_id: record.application_id,
        external_user_id: record.external_user_id.as_deref(),
        encryption_version: record.encryption_version,
    });
    let plaintext = cipher.decrypt(&candidate.encrypted, aad.as_bytes())?;
    let config: Value = serde_json::from_slice(&plaintext)
        .map_err(|_| AppError::Config("provider credential payload is invalid".to_string()))?;
    let Some(field) = crate::security::credential_secret_field(record.credential_type) else {
        return Ok(None);
    };
    let Some(secret) = config
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    Ok(Some(crate::domain::ResolvedCredential {
        credential_id: record.id,
        credential_version: record.version,
        credential_type: record.credential_type,
        secret: secrecy::SecretString::new(secret.to_string()),
        config,
    }))
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

    #[test]
    fn over_fetch_limit_is_limit_plus_one() {
        assert_eq!(over_fetch_limit(1), 2);
        assert_eq!(over_fetch_limit(50), 51);
        assert_eq!(over_fetch_limit(200), 201);
    }

    #[test]
    fn over_fetch_limit_saturates_instead_of_overflowing() {
        assert_eq!(over_fetch_limit(i64::MAX), i64::MAX);
    }

    #[test]
    fn keyset_predicate_uses_strict_less_than_for_descending_lists() {
        for sql in [
            LIST_ROUTE_DEFINITIONS_SQL,
            LIST_ROUTING_POLICIES_SQL,
            LIST_AGENT_PROFILES_SQL,
        ] {
            assert!(
                sql.contains("(created_at, id) < ($1::timestamptz, $2::uuid)"),
                "missing strict keyset predicate in {sql}"
            );
            assert!(
                sql.contains("order by created_at desc, id desc"),
                "missing the existing (created_at desc, id desc) tiebreaker in {sql}"
            );
            assert!(
                sql.contains("limit $3"),
                "over-fetch bind must be the third parameter in {sql}"
            );
        }
    }

    #[test]
    fn keyset_predicate_binds_parameters_and_never_interpolates_values() {
        // Every cursor-derived value must reach the query as a bound `$N` placeholder, never
        // as a literal spliced into the SQL text. These are `&'static str` literals (not
        // built with `format!`/string concatenation of caller input), so this is a structural
        // guard against a future edit accidentally interpolating a value: only `$1`, `$2`,
        // `$3` may appear as parameter markers, and no other `$`-prefixed token should.
        for sql in [
            LIST_ROUTE_DEFINITIONS_SQL,
            LIST_ROUTING_POLICIES_SQL,
            LIST_AGENT_PROFILES_SQL,
        ] {
            let placeholders: Vec<&str> = sql.match_indices('$').map(|(i, _)| &sql[i..]).collect();
            for placeholder in placeholders {
                let marker: String = placeholder
                    .chars()
                    .take_while(|c| c.is_ascii_digit() || *c == '$')
                    .collect();
                assert!(
                    marker == "$1" || marker == "$2" || marker == "$3",
                    "unexpected placeholder {marker:?} in {sql}"
                );
            }
        }
    }
}

/// In-memory [`RuntimeRepository`] for unit tests (plan 06, Module 8 / P2-3).
///
/// Backs the **route lookup and model-candidate** slice — the reads the execution pipeline makes
/// before it ever talks to a provider — with real state, and returns an explicit `not_stubbed`
/// error for every other method. A fake that answered unbacked reads with `Ok(vec![])` would let
/// a test pass while exercising nothing, so unbacked methods fail loudly instead.
///
/// Holds **no credential material**: `resolve_runtime_credential` is deliberately left unbacked
/// rather than seeded with a synthetic ciphertext, so no test can grow a dependency on fake
/// encrypted bytes.
#[cfg(test)]
#[derive(Debug, Default)]
pub(crate) struct InMemoryRuntimeRepository {
    routes: Vec<RouteDefinitionRecord>,
    /// Keyed by `route_id`, because that is the one scoping dimension a [`ModelCandidate`]
    /// actually carries. The SQL additionally filters on `application_id` and
    /// `external_tenant_id`, but `ModelCandidate` has no fields for either — a policy's scope is
    /// dropped on the way out of the query — so a fake cannot reproduce that filter and this one
    /// deliberately does not pretend to. Application/tenant scoping stays covered by the
    /// Postgres-backed integration tests.
    candidates: Vec<(Uuid, ModelCandidate)>,
    /// Identity and `version` only, seeded to make the `If-Match` rejection branches of the
    /// versioned mutations reachable without Postgres (plan 06b §2.3). See [`match_version`]
    /// for exactly how much these are allowed to decide.
    routing_policies: Vec<RoutingPolicyRecord>,
    /// See [`Self::routing_policies`].
    agent_profiles: Vec<AgentProfileRecord>,
}

#[cfg(test)]
fn not_stubbed(method: &str) -> AppError {
    AppError::Internal(format!(
        "InMemoryRuntimeRepository::{method} is not stubbed"
    ))
}

/// The fake's stand-in for [`lock_and_match_version`].
///
/// The nine versioned mutations cannot be *performed* here — the trait takes `&self` and this
/// fake owns no interior mutability — so their success path stays `not_stubbed`, loudly, exactly
/// as it was before plan 06b. What the fake can and must do is decide the two *rejection*
/// branches the way Postgres decides them, so `If-Match` handling is exercisable without a
/// database:
///
/// * entity never seeded → `not_stubbed`; the fake does not pretend a row is absent when it
///   simply has no rows to speak for, which is what would let a `404` assertion pass against
///   nothing at all;
/// * seeded, but `id` is not among the rows → the same `NotFound` the Pg impl returns, with the
///   same resource string;
/// * seeded and found, but `version` differs → `resource_version_conflict`, the same `409`;
/// * seeded, found, versions agree → `not_stubbed`, because the write itself is still unbacked.
#[cfg(test)]
fn match_version(
    rows: impl IntoIterator<Item = (Uuid, i64)>,
    id: Uuid,
    expected_version: i64,
    resource: String,
    method: &str,
) -> AppError {
    let mut seeded = false;
    for (row_id, version) in rows {
        seeded = true;
        if row_id == id {
            return if version == expected_version {
                not_stubbed(method)
            } else {
                version_conflict()
            };
        }
    }
    if seeded {
        AppError::NotFound(resource)
    } else {
        not_stubbed(method)
    }
}

#[cfg(test)]
impl InMemoryRuntimeRepository {
    pub(crate) fn new(
        routes: Vec<RouteDefinitionRecord>,
        candidates: Vec<(Uuid, ModelCandidate)>,
    ) -> Self {
        Self {
            routes,
            candidates,
            ..Self::default()
        }
    }

    pub(crate) fn with_routing_policies(mut self, rows: Vec<RoutingPolicyRecord>) -> Self {
        self.routing_policies = rows;
        self
    }

    pub(crate) fn with_agent_profiles(mut self, rows: Vec<AgentProfileRecord>) -> Self {
        self.agent_profiles = rows;
        self
    }

    fn route_versions(&self) -> impl Iterator<Item = (Uuid, i64)> + '_ {
        self.routes.iter().map(|row| (row.id, row.version))
    }

    fn routing_policy_versions(&self) -> impl Iterator<Item = (Uuid, i64)> + '_ {
        self.routing_policies
            .iter()
            .map(|row| (row.id, row.version))
    }

    fn agent_profile_versions(&self) -> impl Iterator<Item = (Uuid, i64)> + '_ {
        self.agent_profiles.iter().map(|row| (row.id, row.version))
    }
}

#[cfg(test)]
#[async_trait]
impl RuntimeRepository for InMemoryRuntimeRepository {
    async fn get_active_route_by_key(
        &self,
        route_key: &str,
    ) -> Result<Option<RouteDefinitionRecord>, AppError> {
        Ok(self
            .routes
            .iter()
            .find(|route| {
                route.route_key == route_key
                    && route.status == crate::domain::ResourceStatus::Active
                    && route.deleted_at.is_none()
            })
            .cloned())
    }

    async fn get_default_route(&self) -> Result<Option<RouteDefinitionRecord>, AppError> {
        Ok(self
            .routes
            .iter()
            .find(|route| {
                route.status == crate::domain::ResourceStatus::Active
                    && route.deleted_at.is_none()
                    && route
                        .metadata
                        .get("default")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
            })
            .cloned())
    }

    /// Reproduces the two parts of the SQL contract that are observable from `ModelCandidate`:
    /// candidates belong to one route, and they come back ordered by ascending `policy_priority`
    /// then descending `weight`, over-fetched to `limit`. Application/tenant scoping is not
    /// reproduced — see the `candidates` field.
    async fn list_model_candidates(
        &self,
        route_id: Uuid,
        _application_id: Option<Uuid>,
        _external_tenant_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<ModelCandidate>, AppError> {
        let mut out: Vec<ModelCandidate> = self
            .candidates
            .iter()
            .filter(|(id, _)| *id == route_id)
            .map(|(_, candidate)| candidate.clone())
            .collect();
        out.sort_by_key(|candidate| (candidate.policy_priority, -candidate.weight));
        out.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
        Ok(out)
    }

    async fn create_route_definition(
        &self,
        _id: Uuid,
        _request: &RouteDefinitionCreateRequest,
        _audit: AuditLogInsert,
    ) -> Result<RouteDefinitionRecord, AppError> {
        Err(not_stubbed("create_route_definition"))
    }

    async fn list_route_definitions(
        &self,
        _cursor: Option<ListCursor>,
        _limit: i64,
    ) -> Result<Vec<RouteDefinitionRecord>, AppError> {
        Err(not_stubbed("list_route_definitions"))
    }

    async fn get_route_definition(&self, _id: Uuid) -> Result<RouteDefinitionRecord, AppError> {
        Err(not_stubbed("get_route_definition"))
    }

    async fn patch_route_definition(
        &self,
        id: Uuid,
        expected_version: i64,
        _request: &RouteDefinitionPatchRequest,
        _audit: AuditLogInsert,
    ) -> Result<RouteDefinitionRecord, AppError> {
        Err(match_version(
            self.route_versions(),
            id,
            expected_version,
            format!("route {id}"),
            "patch_route_definition",
        ))
    }

    async fn set_route_definition_status(
        &self,
        id: Uuid,
        expected_version: i64,
        _status: &str,
        _audit: AuditLogInsert,
    ) -> Result<RouteDefinitionRecord, AppError> {
        Err(match_version(
            self.route_versions(),
            id,
            expected_version,
            format!("route {id}"),
            "set_route_definition_status",
        ))
    }

    async fn soft_delete_route_definition(
        &self,
        id: Uuid,
        expected_version: i64,
        _audit: AuditLogInsert,
    ) -> Result<(), AppError> {
        Err(match_version(
            self.route_versions(),
            id,
            expected_version,
            format!("route {id}"),
            "soft_delete_route_definition",
        ))
    }

    async fn create_routing_policy(
        &self,
        _id: Uuid,
        _request: &RoutingPolicyCreateRequest,
        _audit: AuditLogInsert,
    ) -> Result<RoutingPolicyRecord, AppError> {
        Err(not_stubbed("create_routing_policy"))
    }

    async fn list_routing_policies(
        &self,
        _cursor: Option<ListCursor>,
        _limit: i64,
    ) -> Result<Vec<RoutingPolicyRecord>, AppError> {
        Err(not_stubbed("list_routing_policies"))
    }

    async fn get_routing_policy(&self, _id: Uuid) -> Result<RoutingPolicyRecord, AppError> {
        Err(not_stubbed("get_routing_policy"))
    }

    async fn provider_model_belongs_to_provider(
        &self,
        _provider_id: Uuid,
        _provider_model_id: Uuid,
    ) -> Result<bool, AppError> {
        Err(not_stubbed("provider_model_belongs_to_provider"))
    }

    async fn patch_routing_policy(
        &self,
        id: Uuid,
        expected_version: i64,
        _request: &RoutingPolicyPatchRequest,
        _audit: AuditLogInsert,
    ) -> Result<RoutingPolicyRecord, AppError> {
        Err(match_version(
            self.routing_policy_versions(),
            id,
            expected_version,
            format!("routing policy {id}"),
            "patch_routing_policy",
        ))
    }

    async fn set_routing_policy_status(
        &self,
        id: Uuid,
        expected_version: i64,
        _status: &str,
        _audit: AuditLogInsert,
    ) -> Result<RoutingPolicyRecord, AppError> {
        Err(match_version(
            self.routing_policy_versions(),
            id,
            expected_version,
            format!("routing policy {id}"),
            "set_routing_policy_status",
        ))
    }

    async fn soft_delete_routing_policy(
        &self,
        id: Uuid,
        expected_version: i64,
        _audit: AuditLogInsert,
    ) -> Result<(), AppError> {
        Err(match_version(
            self.routing_policy_versions(),
            id,
            expected_version,
            format!("routing policy {id}"),
            "soft_delete_routing_policy",
        ))
    }

    async fn create_agent_profile(
        &self,
        _id: Uuid,
        _request: &AgentProfileCreateRequest,
        _audit: AuditLogInsert,
    ) -> Result<AgentProfileRecord, AppError> {
        Err(not_stubbed("create_agent_profile"))
    }

    async fn list_agent_profiles(
        &self,
        _cursor: Option<ListCursor>,
        _limit: i64,
    ) -> Result<Vec<AgentProfileRecord>, AppError> {
        Err(not_stubbed("list_agent_profiles"))
    }

    async fn get_agent_profile(&self, _id: Uuid) -> Result<AgentProfileRecord, AppError> {
        Err(not_stubbed("get_agent_profile"))
    }

    async fn get_active_agent_profile(
        &self,
        _id: Uuid,
    ) -> Result<Option<AgentProfileRecord>, AppError> {
        Err(not_stubbed("get_active_agent_profile"))
    }

    async fn patch_agent_profile(
        &self,
        id: Uuid,
        expected_version: i64,
        _request: &AgentProfilePatchRequest,
        _audit: AuditLogInsert,
    ) -> Result<AgentProfileRecord, AppError> {
        Err(match_version(
            self.agent_profile_versions(),
            id,
            expected_version,
            format!("agent profile {id}"),
            "patch_agent_profile",
        ))
    }

    async fn set_agent_profile_status(
        &self,
        id: Uuid,
        expected_version: i64,
        _status: &str,
        _audit: AuditLogInsert,
    ) -> Result<AgentProfileRecord, AppError> {
        Err(match_version(
            self.agent_profile_versions(),
            id,
            expected_version,
            format!("agent profile {id}"),
            "set_agent_profile_status",
        ))
    }

    async fn soft_delete_agent_profile(
        &self,
        id: Uuid,
        expected_version: i64,
        _audit: AuditLogInsert,
    ) -> Result<(), AppError> {
        Err(match_version(
            self.agent_profile_versions(),
            id,
            expected_version,
            format!("agent profile {id}"),
            "soft_delete_agent_profile",
        ))
    }

    async fn get_provider_runtime_policy(
        &self,
        _provider_id: Uuid,
    ) -> Result<ProviderRuntimePolicyRecord, AppError> {
        Err(not_stubbed("get_provider_runtime_policy"))
    }

    async fn put_provider_runtime_policy(
        &self,
        _provider_id: Uuid,
        _request: &ProviderRuntimePolicyPutRequest,
        _audit: AuditLogInsert,
    ) -> Result<ProviderRuntimePolicyRecord, AppError> {
        Err(not_stubbed("put_provider_runtime_policy"))
    }

    async fn ensure_application_active(&self, _application_id: Uuid) -> Result<(), AppError> {
        Err(not_stubbed("ensure_application_active"))
    }

    async fn resolve_runtime_credential(
        &self,
        _provider_id: Uuid,
        _credential_types: &[CredentialType],
        _application_id: Option<Uuid>,
        _external_tenant_id: Option<&str>,
        _external_user_id: Option<&str>,
        _explicit_credential_id: Option<Uuid>,
    ) -> Result<Option<RuntimeCredentialCandidate>, AppError> {
        Err(not_stubbed("resolve_runtime_credential"))
    }

    async fn insert_attempt_started(
        &self,
        _insert: &ExecutionAttemptInsert,
    ) -> Result<(), AppError> {
        Err(not_stubbed("insert_attempt_started"))
    }

    async fn update_attempt(
        &self,
        _attempt_id: Uuid,
        _update: &ExecutionAttemptUpdate,
    ) -> Result<(), AppError> {
        Err(not_stubbed("update_attempt"))
    }

    async fn insert_usage_record(&self, _insert: &UsageRecordInsert) -> Result<(), AppError> {
        Err(not_stubbed("insert_usage_record"))
    }

    async fn touch_credential_used(&self, _credential_id: Uuid) -> Result<(), AppError> {
        Err(not_stubbed("touch_credential_used"))
    }
}

#[cfg(test)]
mod repository_trait_tests {
    use super::*;
    use crate::domain::{ProviderType, ResourceStatus, RouteSelectionStrategy};

    fn route(route_key: &str, default: bool) -> RouteDefinitionRecord {
        RouteDefinitionRecord {
            id: Uuid::now_v7(),
            route_key: route_key.to_string(),
            display_name: route_key.to_string(),
            description: None,
            status: ResourceStatus::Active,
            selection_strategy: RouteSelectionStrategy::Default,
            agent_profile_id: None,
            metadata: serde_json::json!({ "default": default }),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            deleted_at: None,
            version: 1,
        }
    }

    fn candidate(model_key: &str, priority: i32) -> ModelCandidate {
        ModelCandidate {
            policy_id: Uuid::now_v7(),
            provider_id: Uuid::now_v7(),
            provider_version: 1,
            provider_type: ProviderType::OpenAiCompatible,
            provider_display_name: "fake provider".to_string(),
            base_url: Some("https://provider.invalid/v1".to_string()),
            provider_model_id: Uuid::now_v7(),
            model_version: 1,
            model_key: model_key.to_string(),
            capabilities: serde_json::json!({}),
            policy_priority: priority,
            weight: 100,
            timeout_ms: None,
            retry_policy: serde_json::json!({}),
            runtime_policy: default_provider_runtime_policy(Uuid::now_v7()),
        }
    }

    /// P2-3: route resolution and candidate ordering — the reads the execution pipeline makes
    /// before it selects a provider — are now exercisable without Postgres.
    #[tokio::test]
    async fn fake_runtime_repository_supports_candidate_selection_unit_test() {
        let primary = route("primary", true);
        let secondary = route("secondary", false);
        let repo = InMemoryRuntimeRepository::new(
            vec![primary.clone(), secondary.clone()],
            vec![
                (primary.id, candidate("slow-model", 200)),
                (primary.id, candidate("fast-model", 100)),
                (secondary.id, candidate("other-model", 100)),
            ],
        );

        assert_eq!(
            repo.get_active_route_by_key("secondary")
                .await
                .unwrap()
                .map(|route| route.id),
            Some(secondary.id)
        );
        assert!(
            repo.get_active_route_by_key("absent")
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            repo.get_default_route()
                .await
                .unwrap()
                .map(|route| route.id),
            Some(primary.id)
        );

        // Lowest `policy_priority` first, and a route's candidates never leak into another's.
        let candidates = repo
            .list_model_candidates(primary.id, None, None, 10)
            .await
            .unwrap();
        assert_eq!(
            candidates
                .iter()
                .map(|candidate| candidate.model_key.as_str())
                .collect::<Vec<_>>(),
            vec!["fast-model", "slow-model"]
        );
        assert_eq!(
            repo.list_model_candidates(secondary.id, None, None, 10)
                .await
                .unwrap()
                .len(),
            1
        );
        // `limit` is honoured.
        assert_eq!(
            repo.list_model_candidates(primary.id, None, None, 1)
                .await
                .unwrap()
                .len(),
            1
        );
    }

    fn routing_policy(version: i64) -> RoutingPolicyRecord {
        RoutingPolicyRecord {
            id: Uuid::now_v7(),
            application_id: None,
            external_tenant_id: None,
            route_id: Uuid::now_v7(),
            provider_id: Uuid::now_v7(),
            provider_model_id: Uuid::now_v7(),
            priority: 100,
            weight: 100,
            cost_weight: 0.0,
            latency_weight: 0.0,
            quality_weight: 0.0,
            privacy_class: None,
            required_capabilities: Vec::new(),
            maximum_cost_per_request: None,
            maximum_input_tokens: None,
            maximum_output_tokens: None,
            timeout_ms: None,
            retry_policy: serde_json::json!({}),
            status: ResourceStatus::Active,
            metadata: serde_json::json!({}),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            deleted_at: None,
            version,
        }
    }

    fn agent_profile(version: i64) -> AgentProfileRecord {
        AgentProfileRecord {
            id: Uuid::now_v7(),
            profile_key: "profile".to_string(),
            display_name: "profile".to_string(),
            preamble: None,
            temperature: None,
            max_tokens: None,
            tool_policy: serde_json::json!({}),
            context_policy: serde_json::json!({}),
            memory_policy: serde_json::json!({}),
            status: ResourceStatus::Active,
            metadata: serde_json::json!({}),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            deleted_at: None,
            version,
        }
    }

    /// The `AuditLogInsert` every write method now demands, for tests that only care about
    /// the refusal *before* the write. Its content is irrelevant here; what matters is that
    /// the signature makes an auditless mutation impossible to write down.
    fn probe_audit() -> AuditLogInsert {
        AuditLogInsert {
            request_id: None,
            actor_type: None,
            actor_subject: None,
            delegated_subject: None,
            external_user_id: None,
            external_tenant_id: None,
            application_id: None,
            resource_type: "test".to_string(),
            resource_id: None,
            action: "test".to_string(),
            result: crate::domain::AuditResult::Success,
            source_ip: None,
            user_agent: None,
            metadata: Value::Null,
        }
    }

    fn assert_version_conflict(error: &AppError, method: &str) {
        assert_eq!(
            error.status(),
            axum::http::StatusCode::CONFLICT,
            "{method} must reject a stale If-Match with 409"
        );
        assert_eq!(
            error.error_response(None).error.code,
            "resource_version_conflict",
            "{method} must use the same conflict code the HTTP layer used to emit"
        );
    }

    /// Plan 06b §2.3: the fake has to reject a version mismatch with the *same*
    /// `resource_version_conflict` the Pg implementation produces, otherwise nothing below the
    /// HTTP layer ever exercises the branch that closes the TOCTOU. All nine versioned
    /// mutations, because the whole point of moving the check into the repository is that no
    /// caller can forget it — a method that silently ignored `expected_version` would look
    /// identical from the service layer.
    #[tokio::test]
    async fn fake_runtime_repository_rejects_a_stale_if_match_on_every_versioned_mutation() {
        let route = route("primary", false);
        let policy = routing_policy(7);
        let profile = agent_profile(9);
        let repo = InMemoryRuntimeRepository::new(vec![route.clone()], Vec::new())
            .with_routing_policies(vec![policy.clone()])
            .with_agent_profiles(vec![profile.clone()]);

        let stale = |current: i64| current - 1;

        assert_version_conflict(
            &repo
                .patch_route_definition(
                    route.id,
                    stale(route.version),
                    &RouteDefinitionPatchRequest::default(),
                    probe_audit(),
                )
                .await
                .expect_err("stale If-Match must not reach the write"),
            "patch_route_definition",
        );
        assert_version_conflict(
            &repo
                .set_route_definition_status(
                    route.id,
                    stale(route.version),
                    "disabled",
                    probe_audit(),
                )
                .await
                .expect_err("stale If-Match must not reach the write"),
            "set_route_definition_status",
        );
        assert_version_conflict(
            &repo
                .soft_delete_route_definition(route.id, stale(route.version), probe_audit())
                .await
                .expect_err("stale If-Match must not reach the write"),
            "soft_delete_route_definition",
        );

        assert_version_conflict(
            &repo
                .patch_routing_policy(
                    policy.id,
                    stale(policy.version),
                    &RoutingPolicyPatchRequest::default(),
                    probe_audit(),
                )
                .await
                .expect_err("stale If-Match must not reach the write"),
            "patch_routing_policy",
        );
        assert_version_conflict(
            &repo
                .set_routing_policy_status(
                    policy.id,
                    stale(policy.version),
                    "disabled",
                    probe_audit(),
                )
                .await
                .expect_err("stale If-Match must not reach the write"),
            "set_routing_policy_status",
        );
        assert_version_conflict(
            &repo
                .soft_delete_routing_policy(policy.id, stale(policy.version), probe_audit())
                .await
                .expect_err("stale If-Match must not reach the write"),
            "soft_delete_routing_policy",
        );

        assert_version_conflict(
            &repo
                .patch_agent_profile(
                    profile.id,
                    stale(profile.version),
                    &AgentProfilePatchRequest::default(),
                    probe_audit(),
                )
                .await
                .expect_err("stale If-Match must not reach the write"),
            "patch_agent_profile",
        );
        assert_version_conflict(
            &repo
                .set_agent_profile_status(
                    profile.id,
                    stale(profile.version),
                    "disabled",
                    probe_audit(),
                )
                .await
                .expect_err("stale If-Match must not reach the write"),
            "set_agent_profile_status",
        );
        assert_version_conflict(
            &repo
                .soft_delete_agent_profile(profile.id, stale(profile.version), probe_audit())
                .await
                .expect_err("stale If-Match must not reach the write"),
            "soft_delete_agent_profile",
        );
    }

    /// The distinction the recipe insists on: a row that is genuinely gone stays `404` with the
    /// resource string it always had, and only a real mismatch is `409`. Collapsing the two —
    /// which is what appending `and version = $N` to the `UPDATE` without a locked pre-read
    /// would do — would report "no such route" for a route that exists.
    #[tokio::test]
    async fn fake_runtime_repository_separates_an_absent_row_from_a_stale_if_match() {
        let route = route("primary", false);
        let repo = InMemoryRuntimeRepository::new(vec![route.clone()], Vec::new());
        let absent = Uuid::now_v7();

        let error = repo
            .patch_route_definition(
                absent,
                1,
                &RouteDefinitionPatchRequest::default(),
                probe_audit(),
            )
            .await
            .expect_err("an absent route cannot be patched");
        assert_eq!(error.status(), axum::http::StatusCode::NOT_FOUND);
        assert_eq!(error.to_string(), format!("not found: route {absent}"));

        // A matching version is not a success here — the fake cannot persist the write, and
        // saying otherwise would let a test pass while exercising nothing.
        let error = repo
            .patch_route_definition(
                route.id,
                route.version,
                &RouteDefinitionPatchRequest::default(),
                probe_audit(),
            )
            .await
            .expect_err("the fake does not perform the write");
        assert!(error.to_string().contains("not stubbed"));

        // An entity the fake was never seeded for still fails loudly rather than inventing a 404.
        let error = repo
            .patch_agent_profile(
                absent,
                1,
                &AgentProfilePatchRequest::default(),
                probe_audit(),
            )
            .await
            .expect_err("agent profiles are not seeded on this fake");
        assert!(error.to_string().contains("not stubbed"));
    }

    /// The fake never hands back synthetic credential material; the credential path stays
    /// Postgres-backed on purpose.
    #[tokio::test]
    async fn fake_runtime_repository_refuses_to_resolve_credentials() {
        let repo = InMemoryRuntimeRepository::default();
        let error = repo
            .resolve_runtime_credential(
                Uuid::now_v7(),
                &[CredentialType::ApiKey],
                None,
                None,
                None,
                None,
            )
            .await
            .expect_err("credential resolution is not faked");
        assert!(error.to_string().contains("not stubbed"));
    }
}
