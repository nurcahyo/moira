use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    app::AppState,
    // The one crate-wide fingerprint formula (plan 06, Module 16). This module previously
    // had its own 3-field copy writing the same `idempotency_records` unique index; what
    // remains of it is `legacy_actor_fingerprint`, which is read-only.
    application::{RequestContext, admin::actor_fingerprint},
    domain::{
        AgentProfileCreateRequest, AgentProfilePatchRequest, AgentProfileRecord, AuditLogInsert,
        AuditResult, CursorScope, IdempotencyRecord, ListCursor, ListResponse, Pagination,
        ProviderRuntimePolicyPutRequest, ProviderRuntimePolicyRecord, RouteDefinitionCreateRequest,
        RouteDefinitionPatchRequest, RouteDefinitionRecord, RoutingPolicyCreateRequest,
        RoutingPolicyPatchRequest, RoutingPolicyRecord,
    },
    error::AppError,
    infra::repositories::{
        AdminRepository, PgAdminRepository, PgRuntimeRepository, RuntimeRepository,
    },
    orchestration::CircuitResetScope,
    security::{Actor, secret_fingerprint},
};

/// Per-endpoint cursor scopes (plan 04, Module 3). Declared once and reused for both encode
/// (building `next_cursor`) and decode (validating a client-supplied `cursor`), so a cursor
/// minted for one of these lists can never be replayed against another list — see
/// `domain::pagination`'s module docs for why that matters.
const ROUTE_DEFINITIONS_SCOPE: CursorScope = CursorScope::new("admin.route_definitions");
const ROUTING_POLICIES_SCOPE: CursorScope = CursorScope::new("admin.routing_policies");
const AGENT_PROFILES_SCOPE: CursorScope = CursorScope::new("admin.agent_profiles");

pub struct RuntimeAdminService<'a> {
    state: &'a AppState,
    /// Held as `dyn RuntimeRepository` (plan 06, Module 8 / P2-3) rather than as a concrete
    /// `PgRuntimeRepository`, so the runtime read/write surface can be swapped for a fake.
    ///
    /// Note for whoever writes the first Postgres-free `RuntimeAdminService` unit test: this
    /// field alone is not enough. `audit_success` and `idempotency_replay` go through
    /// `admin_repo`, whose only implementation is `PgAdminRepository` — and *that* requires a
    /// live `PgPool` at construction. Making this service fully Postgres-free therefore also
    /// needs an `AdminRepository` fake (60+ methods), which is outside Module 8's four-trait
    /// scope. Until then this seam is real but not yet exercised.
    runtime_repo: Arc<dyn RuntimeRepository>,
    admin_repo: PgAdminRepository,
}

impl<'a> RuntimeAdminService<'a> {
    pub fn new(state: &'a AppState) -> Result<Self, AppError> {
        let pool = state.pool()?.clone();
        Ok(Self {
            state,
            runtime_repo: Arc::new(PgRuntimeRepository::new(pool.clone())),
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
        self.invalidate_runtime(CircuitResetScope::Unaffected).await;
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
        cursor: Option<&str>,
        limit: i64,
    ) -> Result<ListResponse<RouteDefinitionRecord>, AppError> {
        self.state.authz.require(actor, "moira:routes:read")?;
        let cursor = ListCursor::decode_optional(cursor, ROUTE_DEFINITIONS_SCOPE)?;
        let rows = self
            .runtime_repo
            .list_route_definitions(cursor, limit)
            .await?;
        Ok(paginate_by_created_at(
            rows,
            limit,
            ROUTE_DEFINITIONS_SCOPE,
            |record| (record.created_at, record.id),
        ))
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
        self.invalidate_runtime(CircuitResetScope::Unaffected).await;
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
        self.invalidate_runtime(CircuitResetScope::Unaffected).await;
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
        self.invalidate_runtime(CircuitResetScope::Unaffected).await;
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
        self.invalidate_runtime(CircuitResetScope::Unaffected).await;
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
        cursor: Option<&str>,
        limit: i64,
    ) -> Result<ListResponse<RoutingPolicyRecord>, AppError> {
        self.state
            .authz
            .require(actor, "moira:routing-policies:read")?;
        let cursor = ListCursor::decode_optional(cursor, ROUTING_POLICIES_SCOPE)?;
        let rows = self
            .runtime_repo
            .list_routing_policies(cursor, limit)
            .await?;
        Ok(paginate_by_created_at(
            rows,
            limit,
            ROUTING_POLICIES_SCOPE,
            |record| (record.created_at, record.id),
        ))
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
        self.invalidate_runtime(CircuitResetScope::Unaffected).await;
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
        self.invalidate_runtime(CircuitResetScope::Unaffected).await;
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
        self.invalidate_runtime(CircuitResetScope::Unaffected).await;
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
        self.invalidate_runtime(CircuitResetScope::Unaffected).await;
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
        cursor: Option<&str>,
        limit: i64,
    ) -> Result<ListResponse<AgentProfileRecord>, AppError> {
        self.state
            .authz
            .require(actor, "moira:agent-profiles:read")?;
        let cursor = ListCursor::decode_optional(cursor, AGENT_PROFILES_SCOPE)?;
        let rows = self.runtime_repo.list_agent_profiles(cursor, limit).await?;
        Ok(paginate_by_created_at(
            rows,
            limit,
            AGENT_PROFILES_SCOPE,
            |record| (record.created_at, record.id),
        ))
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
        self.invalidate_runtime(CircuitResetScope::Unaffected).await;
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
        self.invalidate_runtime(CircuitResetScope::Unaffected).await;
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
        self.invalidate_runtime(CircuitResetScope::Unaffected).await;
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
        self.invalidate_runtime(CircuitResetScope::Unaffected).await;
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

    /// Drops both runtime caches, and clears exactly the breaker entries the write that
    /// just happened can plausibly have invalidated.
    ///
    /// This called `CircuitBreakerRegistry::reset_all` until plan 06 Module 14, so every
    /// runtime-admin mutation discarded the health of *every* provider in the process:
    /// renaming a route re-closed a circuit that was open because some unrelated provider
    /// was still timing out, and the next request went straight back at it. Module 14
    /// fixed the same defect on the `LISTEN`/`NOTIFY` side (`src/infra/db.rs`) but could
    /// not reach this file; this is the service-side half of it.
    ///
    /// The scope is a parameter rather than a constant so the compiler puts the question
    /// to every future caller. Deriving it here from the method name would be a guess,
    /// and a guess is wrong in both directions — too narrow leaves a breaker stale, too
    /// wide is the bug above.
    ///
    /// Every call site today passes [`CircuitResetScope::Unaffected`], which is a
    /// property of this service's *write set* rather than a coincidence. A breaker entry
    /// is keyed on `(provider_id, model_id)` and earned by observing a provider actually
    /// fail; this service writes four tables, and none of them can change whether a
    /// provider is answering:
    ///
    /// * `route_definitions` — the four route-definition mutations. A route is a named
    ///   entry point and the row carries neither a provider nor a model, so no breaker
    ///   entry can name it.
    /// * `routing_policies` — the four routing-policy mutations. These rows *do* carry a
    ///   `(provider_id, provider_model_id)` pair, which makes them look scopeable, but
    ///   the pair says where traffic should be sent, not whether the provider is up.
    ///   Binding a route to a provider does not make a failing provider healthy, and
    ///   clearing its breaker on that write is precisely how traffic gets sent back at it.
    /// * `agent_profiles` — the four agent-profile mutations. Temperature, token limits
    ///   and tool/context/memory policy; the row carries no provider identity at all.
    /// * `provider_runtime_policies` — `put_provider_runtime_policy`, the only caller
    ///   holding a `provider_id` and therefore the only one where `Provider(id)` is even
    ///   expressible. It is still `Unaffected`, for the reason recorded at
    ///   `CIRCUIT_UNAFFECTED_RESOURCE_TYPES` in `src/infra/db.rs`: [`before_call`] already
    ///   resets any entry whose stored `policy_version` disagrees with the policy it is
    ///   handed, so a policy edit self-heals exactly where the breaker is consulted.
    ///   Resetting here as well would throw away every breaker for that provider on an
    ///   edit to, say, `max_concurrent_streams`.
    ///
    /// What makes those four answers more than opinion: each of these writes also fires
    /// the `moira_runtime_config` trigger, and `circuit_reset_scope` in `src/infra/db.rs`
    /// independently classifies all four tables as `Unaffected`. The two paths have to
    /// agree — the notification reaches every process, this call only the process that
    /// served the request — so a disagreement would leave the writing node's breaker
    /// state diverging from every other node's for the very same row.
    ///
    /// [`before_call`]: crate::orchestration::CircuitBreakerRegistry::before_call
    async fn invalidate_runtime(&self, circuits: CircuitResetScope) {
        // Both caches stay unconditional, as they are on the NOTIFY path: they are keyed
        // by row version and rebuild from a query, so re-reading them costs one. Breaker
        // state cannot be rebuilt, which is why it is the only thing scoped.
        self.state.runtime_cache.invalidate_all().await;
        self.state.runtime_handles.invalidate_all().await;
        self.state.circuits.reset_for_resource(circuits).await;
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
        let hasher = &self.state.idempotency_hasher;
        let request_bytes = normalized_request_bytes(request)?;

        // Two independent dimensions of the unique index have changed under deployed rows,
        // so the read path sweeps both and the write path emits only the current pair:
        //
        //   * `idempotency_key_hash` — unkeyed SHA-256 → keyed HMAC (plan 03, P1-1);
        //   * `actor_fingerprint`    — this module's 3-field formula → the crate-wide
        //     10-field one (plan 06, Module 16 / P2-15).
        //
        // Three of the four combinations are reachable in a live ledger. The fourth
        // (current fingerprint + legacy key hash) is not — a row carrying the pre-plan-03
        // key hash necessarily predates plan 06 too, so it also carries the legacy
        // fingerprint — but it is still probed, because it costs one indexed lookup on a
        // path that has already missed twice and dropping it would narrow replay coverage
        // on an argument about deploy ordering rather than about the data.
        //
        // Order is load-bearing: the current fingerprint is tried first so a post-deploy
        // row always wins, and a legacy hit is only ever *read*. `record_idempotency`
        // writes `actor_fingerprint(actor)` unconditionally, so the legacy value can never
        // re-enter the ledger and the fallback drains as rows expire.
        //
        // TODO(plan-07): delete `legacy_actor_fingerprint` and the second half of this
        // sweep once every ledger row written before plan 06 shipped has expired.
        // `idempotency_records.expires_at` is set 24h ahead (`record_idempotency` below),
        // so the window closes 24h after the deploy that carries Module 16; the earliest
        // safe removal date is therefore deploy-date + 1 day.
        let actor_fingerprint = actor_fingerprint(actor);
        let legacy_actor_fingerprint = legacy_actor_fingerprint(actor);
        let key_hashes = [
            hasher.hash(key.as_bytes()),
            hasher.legacy_hash(key.as_bytes()),
        ];
        let mut record = None;
        'sweep: for fingerprint in [&actor_fingerprint, &legacy_actor_fingerprint] {
            for key_hash in &key_hashes {
                record = self
                    .admin_repo
                    .get_idempotency_record(key_hash, fingerprint, operation)
                    .await?;
                if record.is_some() {
                    break 'sweep;
                }
            }
        }

        let Some(record) = record else {
            return Ok(None);
        };
        if !hasher.verify(&request_bytes, &record.request_hash) {
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
        let hasher = &self.state.idempotency_hasher;
        let record = IdempotencyRecord {
            id: Uuid::now_v7(),
            idempotency_key_hash: hasher.hash(key.as_bytes()),
            actor_fingerprint: actor_fingerprint(actor),
            operation: operation.to_string(),
            request_hash: hasher.hash(&normalized_request_bytes(request)?),
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

/// The fingerprint this module wrote **before** plan 06 unified the three formulas.
///
/// Read-only, and deliberately not `pub`: `idempotency_replay` consults it so a ledger row
/// written by the previous release still replays, and nothing else may call it. It hashes
/// only `{actor_type, subject, api_key_id}`, which is why two actors differing solely by
/// trusted-JWT issuer, tenant, application or delegated subject used to collide here — the
/// P2-15 hole. `tests::the_legacy_runtime_admin_fingerprint_collided_across_issuer_tenant_and_delegation`
/// pins that collision so this stays a documented historical value rather than something a
/// later reader mistakes for a second live formula.
///
/// TODO(plan-07): delete together with the legacy half of `idempotency_replay`'s sweep,
/// once 24h (the `expires_at` window set in `record_idempotency`) have elapsed since the
/// deploy carrying plan 06 Module 16.
fn legacy_actor_fingerprint(actor: &Actor) -> String {
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

/// The canonical bytes an idempotent runtime-admin request hashes to.
///
/// Returns bytes rather than a digest so the read path can run them through
/// `IdempotencyHasher::verify`, which accepts both the current keyed digest and the
/// pre-switch unkeyed one.
fn normalized_request_bytes<T: Serialize>(request: &T) -> Result<Vec<u8>, AppError> {
    serde_json::to_vec(request)
        .map_err(|err| AppError::BadRequest(format!("invalid idempotent request: {err}")))
}

/// Trims a `limit + 1`-row over-fetch (see `PgRuntimeRepository::list_route_definitions`)
/// down to `limit`, computes `has_more`, and encodes `next_cursor` from the last row that is
/// actually returned — never from the discarded over-fetched row, which would silently skip
/// a row on the following page.
fn paginate_by_created_at<T>(
    mut rows: Vec<T>,
    limit: i64,
    scope: CursorScope,
    key: impl Fn(&T) -> (DateTime<Utc>, Uuid),
) -> ListResponse<T> {
    let has_more = (rows.len() as i64) > limit;
    if has_more {
        rows.truncate(limit.max(0) as usize);
    }
    let next_cursor = if has_more {
        rows.last().map(|record| {
            let (ts, id) = key(record);
            ListCursor::new(ts, id).encode(scope)
        })
    } else {
        None
    };
    ListResponse {
        data: rows,
        pagination: Pagination {
            next_cursor,
            has_more,
        },
    }
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

    const TEST_SCOPE: CursorScope = CursorScope::new("test.paginate_by_created_at");

    fn sample_rows(count: usize) -> Vec<(DateTime<Utc>, Uuid)> {
        let base = Utc::now();
        (0..count)
            .map(|i| (base - Duration::seconds(i as i64), Uuid::now_v7()))
            .collect()
    }

    #[test]
    fn has_more_is_false_when_exactly_limit_rows_are_available() {
        let rows = sample_rows(5);
        let page = paginate_by_created_at(rows.clone(), 5, TEST_SCOPE, |row| *row);
        assert!(!page.pagination.has_more);
        assert_eq!(page.pagination.next_cursor, None);
        assert_eq!(page.data.len(), 5);
        assert_eq!(page.data, rows);
    }

    #[test]
    fn has_more_is_true_and_page_is_trimmed_when_limit_plus_one_rows_are_fetched() {
        let rows = sample_rows(6);
        let page = paginate_by_created_at(rows.clone(), 5, TEST_SCOPE, |row| *row);
        assert!(page.pagination.has_more);
        assert_eq!(page.data.len(), 5);
        assert_eq!(page.data, &rows[..5]);
    }

    #[test]
    fn next_cursor_encodes_the_last_returned_row_not_the_over_fetched_row() {
        let rows = sample_rows(6);
        let page = paginate_by_created_at(rows.clone(), 5, TEST_SCOPE, |row| *row);
        let (expected_ts, expected_id) = rows[4];
        let expected = ListCursor::new(expected_ts, expected_id).encode(TEST_SCOPE);
        assert_eq!(page.pagination.next_cursor, Some(expected));
        // The 6th (over-fetched, trimmed) row must not be what next_cursor points at.
        let (sixth_ts, sixth_id) = rows[5];
        let sixth_encoded = ListCursor::new(sixth_ts, sixth_id).encode(TEST_SCOPE);
        assert_ne!(page.pagination.next_cursor, Some(sixth_encoded));
    }

    #[test]
    fn next_cursor_is_none_when_has_more_is_false() {
        let rows = sample_rows(3);
        let page = paginate_by_created_at(rows, 5, TEST_SCOPE, |row| *row);
        assert!(!page.pagination.has_more);
        assert_eq!(page.pagination.next_cursor, None);
    }

    #[test]
    fn empty_result_set_is_not_has_more() {
        let page: ListResponse<(DateTime<Utc>, Uuid)> =
            paginate_by_created_at(Vec::new(), 5, TEST_SCOPE, |row| *row);
        assert!(!page.pagination.has_more);
        assert_eq!(page.pagination.next_cursor, None);
        assert!(page.data.is_empty());
    }

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

    /// The bug Module 16 fixes, pinned in one test so it cannot be re-argued.
    ///
    /// Plan 06 §16.4 asks for the unified formula's isolation tests to be "observed failing
    /// against the old 3-field formula before that formula was deleted". A transcript of a
    /// deleted test proves that once, to whoever read it. This proves it on every `cargo
    /// test` run, for as long as the legacy value survives, and it proves the *pair* of
    /// claims that matter together: the old formula collided, and the one now writing the
    /// ledger does not.
    ///
    /// Each case varies exactly one identity field. All four were invisible to
    /// `{actor_type, subject, api_key_id}`, so on the pre-plan-06 code every
    /// `create_route_definition`, `create_routing_policy`, `create_agent_profile` and
    /// `put_provider_runtime_policy` call by actor A could replay actor B's stored
    /// response given the same `Idempotency-Key`.
    #[test]
    fn the_legacy_runtime_admin_fingerprint_collided_across_issuer_tenant_and_delegation() {
        use crate::security::ActorType;

        let base = Actor {
            actor_type: ActorType::TrustedJwt,
            subject: Some("shared-subject".to_string()),
            api_key_id: None,
            trusted_jwt_issuer_id: Some(Uuid::nil()),
            ..Actor::default()
        };

        let variants: [(&str, Actor); 5] = [
            (
                "trusted_jwt_issuer_id",
                Actor {
                    trusted_jwt_issuer_id: Some(Uuid::now_v7()),
                    ..base.clone()
                },
            ),
            (
                "tenant_id",
                Actor {
                    tenant_id: Some("other-tenant".to_string()),
                    ..base.clone()
                },
            ),
            (
                "external_tenant_id",
                Actor {
                    external_tenant_id: Some("other-tenant".to_string()),
                    ..base.clone()
                },
            ),
            (
                "internal_application_id",
                Actor {
                    internal_application_id: Some(Uuid::now_v7()),
                    ..base.clone()
                },
            ),
            (
                "delegated_subject",
                Actor {
                    delegated_subject: Some("other-user".to_string()),
                    ..base.clone()
                },
            ),
        ];

        for (field, variant) in variants {
            assert_eq!(
                legacy_actor_fingerprint(&base),
                legacy_actor_fingerprint(&variant),
                "the pre-plan-06 formula is supposed to be blind to `{field}` — if this \
                 stops holding, the legacy fallback is reading a value production never \
                 wrote and pre-deploy rows will not replay"
            );
            assert_ne!(
                actor_fingerprint(&base),
                actor_fingerprint(&variant),
                "the unified formula must isolate replay across `{field}`"
            );
        }
    }

    /// The fallback is a *read* concession, not a second write format. If the two formulas
    /// ever agreed, `idempotency_replay`'s second sweep pass would be redundant; if
    /// `record_idempotency` ever emitted the legacy value, the hole would be back.
    #[test]
    fn the_legacy_and_unified_fingerprints_are_distinct_values() {
        use crate::security::ActorType;

        let actor = Actor {
            actor_type: ActorType::SystemKey,
            subject: Some("system".to_string()),
            api_key_id: Some(Uuid::now_v7()),
            ..Actor::default()
        };

        assert_ne!(actor_fingerprint(&actor), legacy_actor_fingerprint(&actor));
    }
}
