use chrono::{Duration, Utc};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    app::AppState,
    application::RequestContext,
    domain::{
        AgentProfileCreateRequest, AgentProfilePatchRequest, AgentProfileRecord, AuditLogInsert,
        AuditResult, IdempotencyRecord, ListResponse, ProviderRuntimePolicyPutRequest,
        ProviderRuntimePolicyRecord, RouteDefinitionCreateRequest, RouteDefinitionPatchRequest,
        RouteDefinitionRecord, RoutingPolicyCreateRequest, RoutingPolicyPatchRequest,
        RoutingPolicyRecord,
    },
    error::AppError,
    infra::repositories::{AdminRepository, PgAdminRepository, PgRuntimeRepository},
    security::{Actor, request_hash, secret_fingerprint},
};

pub struct RuntimeAdminService<'a> {
    state: &'a AppState,
    runtime_repo: PgRuntimeRepository,
    admin_repo: PgAdminRepository,
}

impl<'a> RuntimeAdminService<'a> {
    pub fn new(state: &'a AppState) -> Result<Self, AppError> {
        let pool = state.pool()?.clone();
        Ok(Self {
            state,
            runtime_repo: PgRuntimeRepository::new(pool.clone()),
            admin_repo: PgAdminRepository::new(pool),
        })
    }

    pub async fn create_route_definition(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        request: RouteDefinitionCreateRequest,
    ) -> Result<RouteDefinitionRecord, AppError> {
        self.state.authz.require(actor, "moira:routes:write")?;
        if let Some(replay) = self
            .idempotency_replay(ctx, actor, "route.create", &request)
            .await?
        {
            return Ok(replay);
        }
        validate_key("route_key", &request.route_key)?;
        validate_display_name(&request.display_name)?;
        validate_metadata(&request.metadata)?;
        let record = self
            .runtime_repo
            .create_route_definition(Uuid::now_v7(), &request)
            .await?;
        self.invalidate_runtime().await;
        self.audit_success(
            actor,
            ctx,
            "route.create",
            "route",
            Some(record.id.to_string()),
            json!({ "route_key": record.route_key }),
        )
        .await?;
        self.record_idempotency(ctx, actor, "route.create", &request, &record)
            .await?;
        Ok(record)
    }

    pub async fn list_route_definitions(
        &self,
        actor: &Actor,
        limit: i64,
    ) -> Result<ListResponse<RouteDefinitionRecord>, AppError> {
        self.state.authz.require(actor, "moira:routes:read")?;
        self.runtime_repo
            .list_route_definitions(limit)
            .await
            .map(ListResponse::new)
    }

    pub async fn get_route_definition(
        &self,
        actor: &Actor,
        id: Uuid,
    ) -> Result<RouteDefinitionRecord, AppError> {
        self.state.authz.require(actor, "moira:routes:read")?;
        self.runtime_repo.get_route_definition(id).await
    }

    pub async fn patch_route_definition(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        id: Uuid,
        request: RouteDefinitionPatchRequest,
    ) -> Result<RouteDefinitionRecord, AppError> {
        self.state.authz.require(actor, "moira:routes:write")?;
        if let Some(display_name) = &request.display_name {
            validate_display_name(display_name)?;
        }
        if let Some(metadata) = &request.metadata {
            validate_metadata(metadata)?;
        }
        let record = self
            .runtime_repo
            .patch_route_definition(id, &request)
            .await?;
        self.invalidate_runtime().await;
        self.audit_success(
            actor,
            ctx,
            "route.update",
            "route",
            Some(id.to_string()),
            json!({}),
        )
        .await?;
        Ok(record)
    }

    pub async fn delete_route_definition(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        id: Uuid,
    ) -> Result<(), AppError> {
        self.state.authz.require(actor, "moira:routes:delete")?;
        self.runtime_repo.soft_delete_route_definition(id).await?;
        self.invalidate_runtime().await;
        self.audit_success(
            actor,
            ctx,
            "route.delete",
            "route",
            Some(id.to_string()),
            json!({}),
        )
        .await
    }

    pub async fn set_route_definition_enabled(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        id: Uuid,
        enabled: bool,
    ) -> Result<RouteDefinitionRecord, AppError> {
        self.state.authz.require(actor, "moira:routes:write")?;
        let record = self
            .runtime_repo
            .set_route_definition_status(id, if enabled { "active" } else { "disabled" })
            .await?;
        self.invalidate_runtime().await;
        self.audit_success(
            actor,
            ctx,
            if enabled {
                "route.enable"
            } else {
                "route.disable"
            },
            "route",
            Some(id.to_string()),
            json!({}),
        )
        .await?;
        Ok(record)
    }

    pub async fn create_routing_policy(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        request: RoutingPolicyCreateRequest,
    ) -> Result<RoutingPolicyRecord, AppError> {
        self.state
            .authz
            .require(actor, "moira:routing-policies:write")?;
        if let Some(replay) = self
            .idempotency_replay(ctx, actor, "routing_policy.create", &request)
            .await?
        {
            return Ok(replay);
        }
        validate_routing_policy(&request)?;
        self.ensure_provider_model_belongs_to_provider(
            request.provider_id,
            request.provider_model_id,
        )
        .await?;
        let record = self
            .runtime_repo
            .create_routing_policy(Uuid::now_v7(), &request)
            .await?;
        self.invalidate_runtime().await;
        self.audit_success(
            actor,
            ctx,
            "routing_policy.create",
            "routing_policy",
            Some(record.id.to_string()),
            json!({ "route_id": record.route_id }),
        )
        .await?;
        self.record_idempotency(ctx, actor, "routing_policy.create", &request, &record)
            .await?;
        Ok(record)
    }

    pub async fn list_routing_policies(
        &self,
        actor: &Actor,
        limit: i64,
    ) -> Result<ListResponse<RoutingPolicyRecord>, AppError> {
        self.state
            .authz
            .require(actor, "moira:routing-policies:read")?;
        self.runtime_repo
            .list_routing_policies(limit)
            .await
            .map(ListResponse::new)
    }

    pub async fn get_routing_policy(
        &self,
        actor: &Actor,
        id: Uuid,
    ) -> Result<RoutingPolicyRecord, AppError> {
        self.state
            .authz
            .require(actor, "moira:routing-policies:read")?;
        self.runtime_repo.get_routing_policy(id).await
    }

    pub async fn patch_routing_policy(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        id: Uuid,
        request: RoutingPolicyPatchRequest,
    ) -> Result<RoutingPolicyRecord, AppError> {
        self.state
            .authz
            .require(actor, "moira:routing-policies:write")?;
        validate_routing_policy_patch(&request)?;
        let current = self.runtime_repo.get_routing_policy(id).await?;
        let (provider_id, provider_model_id) = effective_routing_policy_target(
            current.provider_id,
            current.provider_model_id,
            &request,
        );
        self.ensure_provider_model_belongs_to_provider(provider_id, provider_model_id)
            .await?;
        let record = self.runtime_repo.patch_routing_policy(id, &request).await?;
        self.invalidate_runtime().await;
        self.audit_success(
            actor,
            ctx,
            "routing_policy.update",
            "routing_policy",
            Some(id.to_string()),
            json!({}),
        )
        .await?;
        Ok(record)
    }

    async fn ensure_provider_model_belongs_to_provider(
        &self,
        provider_id: Uuid,
        provider_model_id: Uuid,
    ) -> Result<(), AppError> {
        if self
            .runtime_repo
            .provider_model_belongs_to_provider(provider_id, provider_model_id)
            .await?
        {
            return Ok(());
        }
        Err(AppError::unprocessable(
            "routing_policy_provider_model_mismatch",
            "provider_model_id must belong to provider_id",
        ))
    }

    pub async fn delete_routing_policy(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        id: Uuid,
    ) -> Result<(), AppError> {
        self.state
            .authz
            .require(actor, "moira:routing-policies:delete")?;
        self.runtime_repo.soft_delete_routing_policy(id).await?;
        self.invalidate_runtime().await;
        self.audit_success(
            actor,
            ctx,
            "routing_policy.delete",
            "routing_policy",
            Some(id.to_string()),
            json!({}),
        )
        .await
    }

    pub async fn set_routing_policy_enabled(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        id: Uuid,
        enabled: bool,
    ) -> Result<RoutingPolicyRecord, AppError> {
        self.state
            .authz
            .require(actor, "moira:routing-policies:write")?;
        let record = self
            .runtime_repo
            .set_routing_policy_status(id, if enabled { "active" } else { "disabled" })
            .await?;
        self.invalidate_runtime().await;
        self.audit_success(
            actor,
            ctx,
            if enabled {
                "routing_policy.enable"
            } else {
                "routing_policy.disable"
            },
            "routing_policy",
            Some(id.to_string()),
            json!({}),
        )
        .await?;
        Ok(record)
    }

    pub async fn create_agent_profile(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        request: AgentProfileCreateRequest,
    ) -> Result<AgentProfileRecord, AppError> {
        self.state
            .authz
            .require(actor, "moira:agent-profiles:write")?;
        if let Some(replay) = self
            .idempotency_replay(ctx, actor, "agent_profile.create", &request)
            .await?
        {
            return Ok(replay);
        }
        validate_key("profile_key", &request.profile_key)?;
        validate_display_name(&request.display_name)?;
        validate_agent_profile(
            request.temperature,
            request.max_tokens,
            &request.tool_policy,
            &request.context_policy,
            &request.memory_policy,
            &request.metadata,
        )?;
        let record = self
            .runtime_repo
            .create_agent_profile(Uuid::now_v7(), &request)
            .await?;
        self.invalidate_runtime().await;
        self.audit_success(
            actor,
            ctx,
            "agent_profile.create",
            "agent_profile",
            Some(record.id.to_string()),
            json!({ "profile_key": record.profile_key }),
        )
        .await?;
        self.record_idempotency(ctx, actor, "agent_profile.create", &request, &record)
            .await?;
        Ok(record)
    }

    pub async fn list_agent_profiles(
        &self,
        actor: &Actor,
        limit: i64,
    ) -> Result<ListResponse<AgentProfileRecord>, AppError> {
        self.state
            .authz
            .require(actor, "moira:agent-profiles:read")?;
        self.runtime_repo
            .list_agent_profiles(limit)
            .await
            .map(ListResponse::new)
    }

    pub async fn get_agent_profile(
        &self,
        actor: &Actor,
        id: Uuid,
    ) -> Result<AgentProfileRecord, AppError> {
        self.state
            .authz
            .require(actor, "moira:agent-profiles:read")?;
        self.runtime_repo.get_agent_profile(id).await
    }

    pub async fn patch_agent_profile(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        id: Uuid,
        request: AgentProfilePatchRequest,
    ) -> Result<AgentProfileRecord, AppError> {
        self.state
            .authz
            .require(actor, "moira:agent-profiles:write")?;
        if let Some(display_name) = &request.display_name {
            validate_display_name(display_name)?;
        }
        validate_agent_profile(
            request.temperature,
            request.max_tokens,
            request.tool_policy.as_ref().unwrap_or(&Value::Null),
            request.context_policy.as_ref().unwrap_or(&Value::Null),
            request.memory_policy.as_ref().unwrap_or(&Value::Null),
            request.metadata.as_ref().unwrap_or(&Value::Null),
        )?;
        let record = self.runtime_repo.patch_agent_profile(id, &request).await?;
        self.invalidate_runtime().await;
        self.audit_success(
            actor,
            ctx,
            "agent_profile.update",
            "agent_profile",
            Some(id.to_string()),
            json!({}),
        )
        .await?;
        Ok(record)
    }

    pub async fn delete_agent_profile(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        id: Uuid,
    ) -> Result<(), AppError> {
        self.state
            .authz
            .require(actor, "moira:agent-profiles:delete")?;
        self.runtime_repo.soft_delete_agent_profile(id).await?;
        self.invalidate_runtime().await;
        self.audit_success(
            actor,
            ctx,
            "agent_profile.delete",
            "agent_profile",
            Some(id.to_string()),
            json!({}),
        )
        .await
    }

    pub async fn set_agent_profile_enabled(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        id: Uuid,
        enabled: bool,
    ) -> Result<AgentProfileRecord, AppError> {
        self.state
            .authz
            .require(actor, "moira:agent-profiles:write")?;
        let record = self
            .runtime_repo
            .set_agent_profile_status(id, if enabled { "active" } else { "disabled" })
            .await?;
        self.invalidate_runtime().await;
        self.audit_success(
            actor,
            ctx,
            if enabled {
                "agent_profile.enable"
            } else {
                "agent_profile.disable"
            },
            "agent_profile",
            Some(id.to_string()),
            json!({}),
        )
        .await?;
        Ok(record)
    }

    pub async fn get_provider_runtime_policy(
        &self,
        actor: &Actor,
        provider_id: Uuid,
    ) -> Result<ProviderRuntimePolicyRecord, AppError> {
        self.state
            .authz
            .require(actor, "moira:runtime-policies:read")?;
        self.runtime_repo
            .get_provider_runtime_policy(provider_id)
            .await
    }

    pub async fn put_provider_runtime_policy(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        provider_id: Uuid,
        expected_version: Option<i64>,
        request: ProviderRuntimePolicyPutRequest,
    ) -> Result<ProviderRuntimePolicyRecord, AppError> {
        self.state
            .authz
            .require(actor, "moira:runtime-policies:write")?;
        let idempotency_request = ProviderRuntimePolicyIdempotencyRequest {
            provider_id,
            request: &request,
        };
        if let Some(replay) = self
            .idempotency_replay(
                ctx,
                actor,
                "provider_runtime_policy.upsert",
                &idempotency_request,
            )
            .await?
        {
            return Ok(replay);
        }
        validate_runtime_policy(&request)?;
        match self
            .runtime_repo
            .get_provider_runtime_policy(provider_id)
            .await
        {
            Ok(existing) => match expected_version {
                Some(expected) if existing.version == expected => {}
                Some(_) => {
                    return Err(AppError::conflict(
                        "resource_version_conflict",
                        "resource version does not match If-Match",
                    ));
                }
                None => {
                    return Err(AppError::BadRequest(
                        "If-Match header is required".to_string(),
                    ));
                }
            },
            Err(AppError::NotFound(_)) => {}
            Err(error) => return Err(error),
        }
        let record = self
            .runtime_repo
            .put_provider_runtime_policy(provider_id, &request)
            .await?;
        self.invalidate_runtime().await;
        self.audit_success(
            actor,
            ctx,
            "provider_runtime_policy.upsert",
            "provider_runtime_policy",
            Some(provider_id.to_string()),
            json!({ "provider_id": provider_id }),
        )
        .await?;
        self.record_idempotency(
            ctx,
            actor,
            "provider_runtime_policy.upsert",
            &idempotency_request,
            &record,
        )
        .await?;
        Ok(record)
    }

    async fn invalidate_runtime(&self) {
        self.state.runtime_cache.invalidate_all().await;
        self.state.runtime_handles.invalidate_all().await;
        self.state.circuits.reset_all().await;
    }

    async fn audit_success(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        action: &str,
        resource_type: &str,
        resource_id: Option<String>,
        metadata: Value,
    ) -> Result<(), AppError> {
        self.admin_repo
            .insert_audit(AuditLogInsert {
                request_id: Some(ctx.request_id.clone()),
                actor_type: Some(format!("{:?}", actor.actor_type)),
                actor_subject: actor.subject.clone(),
                delegated_subject: actor.delegated_subject.clone(),
                external_user_id: actor.external_user_id.clone(),
                external_tenant_id: actor.external_tenant_id.clone(),
                application_id: actor.internal_application_id,
                resource_type: resource_type.to_string(),
                resource_id,
                action: action.to_string(),
                result: AuditResult::Success,
                source_ip: ctx.source_ip,
                user_agent: ctx.user_agent.clone(),
                metadata,
            })
            .await
    }

    async fn idempotency_replay<Req, Resp>(
        &self,
        ctx: &RequestContext,
        actor: &Actor,
        operation: &str,
        request: &Req,
    ) -> Result<Option<Resp>, AppError>
    where
        Req: Serialize,
        Resp: DeserializeOwned,
    {
        let Some(key) = &ctx.idempotency_key else {
            return Ok(None);
        };
        let actor_fingerprint = actor_fingerprint(actor);
        let key_hash = request_hash(key.as_bytes());
        let request_hash = normalized_request_hash(request)?;
        let Some(record) = self
            .admin_repo
            .get_idempotency_record(&key_hash, &actor_fingerprint, operation)
            .await?
        else {
            return Ok(None);
        };
        if record.request_hash != request_hash {
            return Err(AppError::conflict(
                "idempotency_conflict",
                "same Idempotency-Key was used with a different request",
            ));
        }
        let Some(response_body) = record.response_body else {
            return Ok(None);
        };
        serde_json::from_value(response_body)
            .map(Some)
            .map_err(|err| AppError::Internal(format!("decode idempotent response: {err}")))
    }

    async fn record_idempotency<Req, Resp>(
        &self,
        ctx: &RequestContext,
        actor: &Actor,
        operation: &str,
        request: &Req,
        response: &Resp,
    ) -> Result<(), AppError>
    where
        Req: Serialize,
        Resp: Serialize,
    {
        let Some(key) = &ctx.idempotency_key else {
            return Ok(());
        };
        let response_body = serde_json::to_value(response).ok();
        let resource_id = response_body
            .as_ref()
            .and_then(|value| value.get("id"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        let record = IdempotencyRecord {
            id: Uuid::now_v7(),
            idempotency_key_hash: request_hash(key.as_bytes()),
            actor_fingerprint: actor_fingerprint(actor),
            operation: operation.to_string(),
            request_hash: normalized_request_hash(request)?,
            response_status: Some(200),
            response_body,
            resource_id,
            expires_at: Utc::now() + Duration::hours(24),
        };
        self.admin_repo.put_idempotency_record(&record).await?;
        Ok(())
    }
}

#[derive(Serialize)]
struct ProviderRuntimePolicyIdempotencyRequest<'a> {
    provider_id: Uuid,
    request: &'a ProviderRuntimePolicyPutRequest,
}

fn actor_fingerprint(actor: &Actor) -> String {
    secret_fingerprint(
        format!(
            "{:?}:{}:{}",
            actor.actor_type,
            actor.subject.as_deref().unwrap_or(""),
            actor
                .api_key_id
                .map(|id| id.to_string())
                .unwrap_or_default()
        )
        .as_bytes(),
    )
}

fn normalized_request_hash<T: Serialize>(request: &T) -> Result<String, AppError> {
    serde_json::to_vec(request)
        .map(|bytes| request_hash(&bytes))
        .map_err(|err| AppError::BadRequest(format!("invalid idempotent request: {err}")))
}

fn validate_key(label: &str, value: &str) -> Result<(), AppError> {
    if value.is_empty() || value.len() > 128 {
        return Err(AppError::BadRequest(format!(
            "{label} must be 1-128 characters"
        )));
    }
    let first = value.chars().next().unwrap();
    let last = value.chars().next_back().unwrap();
    if !first.is_ascii_alphanumeric() || !last.is_ascii_alphanumeric() {
        return Err(AppError::BadRequest(format!(
            "{label} must start and end with an alphanumeric character"
        )));
    }
    if value
        .chars()
        .any(|ch| !(ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_' || ch == '-'))
    {
        return Err(AppError::BadRequest(format!(
            "{label} may contain only lowercase ASCII letters, digits, hyphen, or underscore"
        )));
    }
    Ok(())
}

fn validate_display_name(value: &str) -> Result<(), AppError> {
    if value.trim().is_empty() || value.len() > 200 {
        return Err(AppError::BadRequest(
            "display_name must be 1-200 characters".to_string(),
        ));
    }
    Ok(())
}

fn validate_metadata<T: Serialize>(value: &T) -> Result<(), AppError> {
    let bytes = serde_json::to_vec(value)
        .map_err(|err| AppError::BadRequest(format!("metadata is invalid JSON: {err}")))?;
    if bytes.len() > 16 * 1024 {
        return Err(AppError::BadRequest(
            "metadata must be at most 16KiB".to_string(),
        ));
    }
    Ok(())
}

fn validate_routing_policy(request: &RoutingPolicyCreateRequest) -> Result<(), AppError> {
    if request.priority < 0 || request.weight < 0 {
        return Err(AppError::BadRequest(
            "routing policy priority and weight must be non-negative".to_string(),
        ));
    }
    if request.weight == 0 {
        return Err(AppError::BadRequest(
            "routing policy weight zero disables weighted selection; use disable instead"
                .to_string(),
        ));
    }
    if request
        .required_capabilities
        .iter()
        .any(|value| value.trim().is_empty() || value.len() > 64)
    {
        return Err(AppError::BadRequest(
            "required_capabilities contains an invalid value".to_string(),
        ));
    }
    validate_metadata(&request.retry_policy)?;
    validate_metadata(&request.metadata)
}

fn validate_routing_policy_patch(request: &RoutingPolicyPatchRequest) -> Result<(), AppError> {
    if request.priority.is_some_and(|value| value < 0)
        || request.weight.is_some_and(|value| value < 0)
    {
        return Err(AppError::BadRequest(
            "routing policy priority and weight must be non-negative".to_string(),
        ));
    }
    if let Some(capabilities) = &request.required_capabilities
        && capabilities
            .iter()
            .any(|value| value.trim().is_empty() || value.len() > 64)
    {
        return Err(AppError::BadRequest(
            "required_capabilities contains an invalid value".to_string(),
        ));
    }
    if let Some(retry_policy) = &request.retry_policy {
        validate_metadata(retry_policy)?;
    }
    if let Some(metadata) = &request.metadata {
        validate_metadata(metadata)?;
    }
    Ok(())
}

fn effective_routing_policy_target(
    current_provider_id: Uuid,
    current_provider_model_id: Uuid,
    request: &RoutingPolicyPatchRequest,
) -> (Uuid, Uuid) {
    (
        request.provider_id.unwrap_or(current_provider_id),
        request
            .provider_model_id
            .unwrap_or(current_provider_model_id),
    )
}

fn validate_agent_profile(
    temperature: Option<f64>,
    max_tokens: Option<i64>,
    tool_policy: &Value,
    context_policy: &Value,
    memory_policy: &Value,
    metadata: &Value,
) -> Result<(), AppError> {
    if temperature.is_some_and(|value| !(0.0..=2.0).contains(&value)) {
        return Err(AppError::BadRequest(
            "temperature must be between 0 and 2".to_string(),
        ));
    }
    if max_tokens.is_some_and(|value| value <= 0) {
        return Err(AppError::BadRequest(
            "max_tokens must be positive".to_string(),
        ));
    }
    validate_metadata(tool_policy)?;
    validate_metadata(context_policy)?;
    validate_metadata(memory_policy)?;
    validate_metadata(metadata)
}

fn validate_runtime_policy(request: &ProviderRuntimePolicyPutRequest) -> Result<(), AppError> {
    let positive = [
        request.connect_timeout_ms,
        request.request_timeout_ms,
        request.stream_idle_timeout_ms,
        request.max_concurrent_requests,
        request.max_concurrent_streams,
        request.circuit_failure_threshold,
        request.circuit_open_duration_ms,
    ];
    if positive.iter().flatten().any(|value| *value <= 0) {
        return Err(AppError::BadRequest(
            "runtime policy positive fields must be greater than zero".to_string(),
        ));
    }
    if request.retry_limit.is_some_and(|value| value < 0)
        || request.retry_base_delay_ms.is_some_and(|value| value < 0)
        || request.retry_max_delay_ms.is_some_and(|value| value < 0)
    {
        return Err(AppError::BadRequest(
            "runtime policy retry fields must be non-negative".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_keys_are_slug_like() {
        assert!(validate_key("route_key", "coding_v1").is_ok());
        assert!(validate_key("route_key", "Coding").is_err());
        assert!(validate_key("route_key", "-coding").is_err());
    }

    #[test]
    fn runtime_policy_rejects_zero_concurrency() {
        let request = ProviderRuntimePolicyPutRequest {
            max_concurrent_requests: Some(0),
            ..ProviderRuntimePolicyPutRequest::default()
        };
        assert!(validate_runtime_policy(&request).is_err());
    }

    #[test]
    fn routing_policy_patch_uses_effective_provider_and_model() {
        let current_provider_id = Uuid::now_v7();
        let current_provider_model_id = Uuid::now_v7();
        let replacement_provider_id = Uuid::now_v7();
        let replacement_provider_model_id = Uuid::now_v7();

        let provider_patch = RoutingPolicyPatchRequest {
            provider_id: Some(replacement_provider_id),
            ..RoutingPolicyPatchRequest::default()
        };
        assert_eq!(
            effective_routing_policy_target(
                current_provider_id,
                current_provider_model_id,
                &provider_patch,
            ),
            (replacement_provider_id, current_provider_model_id)
        );

        let model_patch = RoutingPolicyPatchRequest {
            provider_model_id: Some(replacement_provider_model_id),
            ..RoutingPolicyPatchRequest::default()
        };
        assert_eq!(
            effective_routing_policy_target(
                current_provider_id,
                current_provider_model_id,
                &model_patch,
            ),
            (current_provider_id, replacement_provider_model_id)
        );
    }
}
