//! Agent-platform admin service (issue #214, plan 12 §3/§5): skills CRUD.
//!
//! Sub-plan 1 of 3, SCHEMA + CRUD only — no execution engine. This mirrors
//! [`crate::application::RuntimeAdminService`]'s shape (scope check, idempotency replay/record,
//! audit written inside the write's own transaction, keyset pagination, `If-Match`), because a
//! skill registry is the same kind of admin resource as an agent profile. It deliberately does
//! **not** invalidate any runtime cache: nothing in `ProviderRuntimeCache` or the runtime-config
//! cache keys on a skill yet (the tool loop is deferred to #84), so a skill write is not runtime
//! configuration — see the migration header for why these tables carry no NOTIFY trigger.
//!
//! Evals and flows CRUD are a documented follow-up; their tables and domain types already exist.

use std::{collections::HashSet, sync::Arc};

use axum::http::StatusCode;
use chrono::{DateTime, Duration, Utc};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    app::AppState,
    application::{RequestContext, admin::actor_fingerprint},
    domain::{
        AgentFlowCreateRequest, AgentFlowPatchRequest, AgentFlowRecord, AgentFlowRunRecord,
        AgentFlowStepCreateRequest, AuditLogInsert, AuditResult, CursorScope,
        EvalCaseCreateRequest, EvalCaseRecord, EvalRunRecord, EvalSuiteCreateRequest,
        EvalSuitePatchRequest, EvalSuiteRecord, IdempotencyRecord, ListCursor, ListResponse,
        Pagination, SkillBulkEnableRequest, SkillBulkEnableResponse, SkillCreateRequest,
        SkillHttpExecutorPatchRequest, SkillHttpExecutorRecord, SkillImportRequest,
        SkillImportResponse, SkillPatchRequest, SkillRecord,
    },
    error::AppError,
    infra::repositories::{
        AdminRepository, PgAdminRepository, PgAgentPlatformRepository, PgRuntimeRepository,
        RuntimeRepository,
    },
    orchestration::{OpenApiImportError, parse_openapi_document},
    security::{
        Actor, OutboundUrlDenial, OutboundUrlPolicy, SystemResolver, validate_outbound_url,
    },
};

/// Keyset cursor scope for `GET /api/v1/admin/skills`, minted and validated only here so a
/// cursor for this list can never be replayed against another list.
const SKILLS_SCOPE: CursorScope = CursorScope::new("admin.skills");

/// Keyset cursor scope for `GET /api/v1/admin/skill-executors`.
const EXECUTORS_SCOPE: CursorScope = CursorScope::new("admin.skill_executors");

/// Keyset cursor scopes for the F2 evals/flows CRUD surface (issue #214, plan 12 §3). Each
/// nested list (cases, eval runs, flow runs) shares one scope across every parent id, the
/// same convention `rag_documents` uses for `GET .../rag-collections/{id}/documents` — the
/// parent id narrows the SQL `WHERE`, not the cursor's scope label.
const EVAL_SUITES_SCOPE: CursorScope = CursorScope::new("admin.eval_suites");
const EVAL_CASES_SCOPE: CursorScope = CursorScope::new("admin.eval_cases");
const EVAL_RUNS_SCOPE: CursorScope = CursorScope::new("admin.eval_runs");
const FLOWS_SCOPE: CursorScope = CursorScope::new("admin.flows");
const FLOW_RUNS_SCOPE: CursorScope = CursorScope::new("admin.flow_runs");

/// Guard against an unbounded step array on a single flow — same role `MAX_BULK_ENABLE`
/// plays for skills.
const MAX_FLOW_STEPS: usize = 100;

/// Largest skill-id batch a single bulk-enable accepts. A guard against an unbounded array, in
/// the spirit of the 300-operation import cap (§5 decision 23).
const MAX_BULK_ENABLE: usize = 500;

/// DNS-resolution timeout applied to the outbound-URL SSRF guard for a skill's server URL —
/// shared by import (the document's `servers[0].url`) and by an executor PATCH that changes
/// `url_template`. A local constant rather than a new `Settings` field: this hardening is
/// mandatory for every deployment (plan 12 §5, "SSRF safety is mandatory from day one"), not an
/// operator-tunable knob the way `auth.jwks`'s equivalent is for JWKS fetches.
const SKILL_URL_DNS_TIMEOUT_MS: u64 = 5_000;

/// Ceiling `skill_http_executors.timeout_ms` may be set to via PATCH. The database only
/// enforces `> 0`; this additionally bounds it so a configured per-call timeout cannot
/// outlive Moira's own execution deadlines by an unbounded amount.
const MAX_EXECUTOR_TIMEOUT_MS: i32 = 5 * 60 * 1000;

pub struct AgentPlatformService<'a> {
    state: &'a AppState,
    repo: PgAgentPlatformRepository,
    admin_repo: PgAdminRepository,
    /// Held as `dyn RuntimeRepository`, matching `RuntimeAdminService`'s own field, for
    /// exactly one read: confirming a flow step's `agent_profile_id` names a live row before
    /// the step is stored (fail-closed on a missing agent profile, product decisions
    /// 2026-08-06).
    runtime_repo: Arc<dyn RuntimeRepository>,
}

impl<'a> AgentPlatformService<'a> {
    pub fn new(state: &'a AppState) -> Result<Self, AppError> {
        let pool = state.pool()?.clone();
        Ok(Self {
            state,
            repo: PgAgentPlatformRepository::new(pool.clone()),
            admin_repo: PgAdminRepository::new(pool.clone()),
            runtime_repo: Arc::new(PgRuntimeRepository::new(pool)),
        })
    }

    pub async fn create_skill(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        request: SkillCreateRequest,
    ) -> Result<SkillRecord, AppError> {
        self.state.authz.require(actor, "moira:skills:write")?;
        if let Some(replay) = self
            .idempotency_replay(ctx, actor, "skill.create", &request)
            .await?
        {
            return Ok(replay);
        }
        validate_key("skill_key", &request.skill_key)?;
        validate_display_name(&request.display_name)?;
        validate_json_object("params_schema", &request.params_schema)?;
        validate_tags(&request.tags)?;
        validate_metadata(&request.metadata)?;
        let id = Uuid::now_v7();
        let record = self
            .repo
            .create_skill(
                id,
                &request,
                self.audit(
                    actor,
                    ctx,
                    "skill.create",
                    "skill",
                    Some(id.to_string()),
                    json!({ "skill_key": &request.skill_key, "kind": &request.kind }),
                ),
            )
            .await?;
        self.record_idempotency(ctx, actor, "skill.create", &request, &record)
            .await?;
        Ok(record)
    }

    pub async fn list_skills(
        &self,
        actor: &Actor,
        cursor: Option<&str>,
        limit: i64,
    ) -> Result<ListResponse<SkillRecord>, AppError> {
        self.state.authz.require(actor, "moira:skills:read")?;
        let cursor = ListCursor::decode_optional(cursor, SKILLS_SCOPE)?;
        let rows = self.repo.list_skills(cursor, limit).await?;
        Ok(paginate(rows, limit, SKILLS_SCOPE))
    }

    pub async fn get_skill(&self, actor: &Actor, id: Uuid) -> Result<SkillRecord, AppError> {
        self.state.authz.require(actor, "moira:skills:read")?;
        self.repo.get_skill(id).await
    }

    pub async fn patch_skill(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        id: Uuid,
        expected_version: i64,
        request: SkillPatchRequest,
    ) -> Result<SkillRecord, AppError> {
        self.state.authz.require(actor, "moira:skills:write")?;
        if let Some(display_name) = &request.display_name {
            validate_display_name(display_name)?;
        }
        if let Some(params_schema) = &request.params_schema {
            validate_json_object("params_schema", params_schema)?;
        }
        if let Some(tags) = &request.tags {
            validate_tags(tags)?;
        }
        if let Some(metadata) = &request.metadata {
            validate_metadata(metadata)?;
        }
        self.repo
            .patch_skill(
                id,
                expected_version,
                &request,
                self.audit(
                    actor,
                    ctx,
                    "skill.update",
                    "skill",
                    Some(id.to_string()),
                    json!({}),
                ),
            )
            .await
    }

    pub async fn delete_skill(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        id: Uuid,
        expected_version: i64,
    ) -> Result<(), AppError> {
        self.state.authz.require(actor, "moira:skills:delete")?;
        self.repo
            .soft_delete_skill(
                id,
                expected_version,
                self.audit(
                    actor,
                    ctx,
                    "skill.delete",
                    "skill",
                    Some(id.to_string()),
                    json!({}),
                ),
            )
            .await
    }

    pub async fn set_skill_enabled(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        id: Uuid,
        expected_version: i64,
        enabled: bool,
    ) -> Result<SkillRecord, AppError> {
        self.state.authz.require(actor, "moira:skills:write")?;
        self.repo
            .set_skill_status(
                id,
                expected_version,
                if enabled { "enabled" } else { "disabled" },
                self.audit(
                    actor,
                    ctx,
                    if enabled {
                        "skill.enable"
                    } else {
                        "skill.disable"
                    },
                    "skill",
                    Some(id.to_string()),
                    json!({}),
                ),
            )
            .await
    }

    pub async fn bulk_enable_skills(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        request: SkillBulkEnableRequest,
    ) -> Result<SkillBulkEnableResponse, AppError> {
        self.state.authz.require(actor, "moira:skills:write")?;
        if request.skill_ids.is_empty() {
            return Err(AppError::BadRequest(
                "skill_ids must contain at least one id".to_string(),
            ));
        }
        if request.skill_ids.len() > MAX_BULK_ENABLE {
            return Err(AppError::BadRequest(format!(
                "skill_ids must contain at most {MAX_BULK_ENABLE} ids"
            )));
        }
        let data = self
            .repo
            .enable_skills_bulk(
                &request.skill_ids,
                self.audit(
                    actor,
                    ctx,
                    "skill.bulk_enable",
                    "skill",
                    None,
                    json!({ "requested": request.skill_ids.len() }),
                ),
            )
            .await?;
        Ok(SkillBulkEnableResponse { data })
    }

    /// `POST /api/v1/admin/skills/import` (plan 12 §5). Parses `request.document` with
    /// [`parse_openapi_document`] (pure — enforces the 300-operation cap, §5 decision 23),
    /// SSRF-validates the document's server URL through
    /// [`validate_outbound_url`](crate::security::validate_outbound_url) — the same guard
    /// `security::ssrf` already applies to JWKS fetches — and, only once that succeeds,
    /// creates one `draft` `skills` row plus one `skill_http_executors` row per operation in
    /// a single transaction.
    ///
    /// The SSRF check runs **before** any database write and validates the server URL
    /// exactly once for the whole document: every derived operation shares one
    /// already-SSRF-validated `allowed_host` (`domain::SkillHttpExecutorRecord::allowed_host`),
    /// so per-operation `url_template`s can differ only in path, never in host.
    pub async fn import_skills(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        request: SkillImportRequest,
    ) -> Result<SkillImportResponse, AppError> {
        self.state.authz.require(actor, "moira:skills:write")?;
        if let Some(replay) = self
            .idempotency_replay(ctx, actor, "skill.import", &request)
            .await?
        {
            return Ok(replay);
        }

        let parsed =
            parse_openapi_document(&request.document).map_err(import_parse_error_to_app_error)?;
        let validated_url = validate_skill_url(&parsed.base_url).await?;
        let allowed_host = validated_url
            .host_str()
            .expect("validate_outbound_url guarantees a validated URL carries a host")
            .to_string();

        let (skills, executors) = self
            .repo
            .import_operations(
                &parsed.operations,
                &parsed.base_url,
                &allowed_host,
                self.audit(
                    actor,
                    ctx,
                    "skill.import",
                    "skill",
                    None,
                    json!({
                        "operation_count": parsed.operations.len(),
                        "base_url": &parsed.base_url,
                    }),
                ),
            )
            .await?;

        let response = SkillImportResponse {
            imported_count: skills.len(),
            skills,
            executors,
        };
        self.record_idempotency(ctx, actor, "skill.import", &request, &response)
            .await?;
        Ok(response)
    }

    pub async fn get_executor(
        &self,
        actor: &Actor,
        skill_id: Uuid,
    ) -> Result<SkillHttpExecutorRecord, AppError> {
        self.state.authz.require(actor, "moira:skills:read")?;
        self.repo.get_executor(skill_id).await
    }

    pub async fn list_executors(
        &self,
        actor: &Actor,
        cursor: Option<&str>,
        limit: i64,
    ) -> Result<ListResponse<SkillHttpExecutorRecord>, AppError> {
        self.state.authz.require(actor, "moira:skills:read")?;
        let cursor = ListCursor::decode_optional(cursor, EXECUTORS_SCOPE)?;
        let rows = self.repo.list_executors(cursor, limit).await?;
        Ok(paginate_executors(rows, limit, EXECUTORS_SCOPE))
    }

    /// `expected_updated_at` is this resource's `If-Match` basis instead of an integer
    /// `version` — see `domain::SkillHttpExecutorRecord`'s doc comment. When
    /// `request.url_template` is set, the new URL is SSRF-validated and its host replaces
    /// `allowed_host` server-side; `request.url_template` alone can never set `allowed_host`
    /// to a value the URL does not actually resolve to.
    pub async fn patch_executor(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        skill_id: Uuid,
        expected_updated_at: DateTime<Utc>,
        request: SkillHttpExecutorPatchRequest,
    ) -> Result<SkillHttpExecutorRecord, AppError> {
        self.state.authz.require(actor, "moira:skills:write")?;
        if let Some(timeout_ms) = request.timeout_ms {
            validate_executor_timeout_ms(timeout_ms)?;
        }
        if let Some(credential_id) = request.credential_id {
            // Existence-only check: confirms the reference is live before it is stored.
            // `provider_credentials` owns its own secret handling — this never reads a
            // secret.
            self.admin_repo.get_credential(credential_id).await?;
        }
        let (new_url_template, new_allowed_host) = match &request.url_template {
            Some(url_template) => {
                let validated_url = validate_skill_url(url_template).await?;
                let host = validated_url
                    .host_str()
                    .expect("validate_outbound_url guarantees a validated URL carries a host")
                    .to_string();
                (Some(url_template.clone()), Some(host))
            }
            None => (None, None),
        };
        self.repo
            .patch_executor(
                skill_id,
                expected_updated_at,
                &request,
                new_url_template.as_deref(),
                new_allowed_host.as_deref(),
                self.audit(
                    actor,
                    ctx,
                    "skill_executor.update",
                    "skill_http_executor",
                    Some(skill_id.to_string()),
                    json!({}),
                ),
            )
            .await
    }

    pub async fn delete_executor(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        skill_id: Uuid,
        expected_updated_at: DateTime<Utc>,
    ) -> Result<(), AppError> {
        self.state.authz.require(actor, "moira:skills:delete")?;
        self.repo
            .delete_executor(
                skill_id,
                expected_updated_at,
                self.audit(
                    actor,
                    ctx,
                    "skill_executor.delete",
                    "skill_http_executor",
                    Some(skill_id.to_string()),
                    json!({}),
                ),
            )
            .await
    }

    // =====================================================================================
    // Eval suites (issue #214, plan 12 §3 — the deferred CRUD half of PR #227's schema).
    // =====================================================================================

    pub async fn create_eval_suite(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        request: EvalSuiteCreateRequest,
    ) -> Result<EvalSuiteRecord, AppError> {
        self.state.authz.require(actor, "moira:evals:write")?;
        if let Some(replay) = self
            .idempotency_replay(ctx, actor, "eval_suite.create", &request)
            .await?
        {
            return Ok(replay);
        }
        validate_key("suite_key", &request.suite_key)?;
        validate_display_name(&request.display_name)?;
        validate_metadata(&request.metadata)?;
        let id = Uuid::now_v7();
        let record = self
            .repo
            .create_eval_suite(
                id,
                &request,
                self.audit(
                    actor,
                    ctx,
                    "eval_suite.create",
                    "eval_suite",
                    Some(id.to_string()),
                    json!({ "suite_key": &request.suite_key }),
                ),
            )
            .await?;
        self.record_idempotency(ctx, actor, "eval_suite.create", &request, &record)
            .await?;
        Ok(record)
    }

    pub async fn list_eval_suites(
        &self,
        actor: &Actor,
        cursor: Option<&str>,
        limit: i64,
    ) -> Result<ListResponse<EvalSuiteRecord>, AppError> {
        self.state.authz.require(actor, "moira:evals:read")?;
        let cursor = ListCursor::decode_optional(cursor, EVAL_SUITES_SCOPE)?;
        let rows = self.repo.list_eval_suites(cursor, limit).await?;
        Ok(paginate_by_created_at(
            rows,
            limit,
            EVAL_SUITES_SCOPE,
            |r| (r.created_at, r.id),
        ))
    }

    pub async fn get_eval_suite(
        &self,
        actor: &Actor,
        id: Uuid,
    ) -> Result<EvalSuiteRecord, AppError> {
        self.state.authz.require(actor, "moira:evals:read")?;
        self.repo.get_eval_suite(id).await
    }

    pub async fn patch_eval_suite(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        id: Uuid,
        expected_version: i64,
        request: EvalSuitePatchRequest,
    ) -> Result<EvalSuiteRecord, AppError> {
        self.state.authz.require(actor, "moira:evals:write")?;
        if let Some(display_name) = &request.display_name {
            validate_display_name(display_name)?;
        }
        if let Some(metadata) = &request.metadata {
            validate_metadata(metadata)?;
        }
        self.repo
            .patch_eval_suite(
                id,
                expected_version,
                &request,
                self.audit(
                    actor,
                    ctx,
                    "eval_suite.update",
                    "eval_suite",
                    Some(id.to_string()),
                    json!({}),
                ),
            )
            .await
    }

    pub async fn delete_eval_suite(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        id: Uuid,
        expected_version: i64,
    ) -> Result<(), AppError> {
        self.state.authz.require(actor, "moira:evals:delete")?;
        self.repo
            .soft_delete_eval_suite(
                id,
                expected_version,
                self.audit(
                    actor,
                    ctx,
                    "eval_suite.delete",
                    "eval_suite",
                    Some(id.to_string()),
                    json!({}),
                ),
            )
            .await
    }

    // =====================================================================================
    // Eval cases — a child of one suite; no version/PATCH surface (migration header).
    // =====================================================================================

    pub async fn create_eval_case(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        suite_id: Uuid,
        request: EvalCaseCreateRequest,
    ) -> Result<EvalCaseRecord, AppError> {
        self.state.authz.require(actor, "moira:evals:write")?;
        validate_json_present("input", &request.input)?;
        validate_json_present("expected", &request.expected)?;
        validate_metadata(&request.metadata)?;
        // Existence-only check: confirms the suite is live before a case is attached to it,
        // the same posture `patch_executor`'s `credential_id` check takes.
        self.repo.get_eval_suite(suite_id).await?;
        let id = Uuid::now_v7();
        self.repo
            .create_eval_case(
                id,
                suite_id,
                &request,
                self.audit(
                    actor,
                    ctx,
                    "eval_case.create",
                    "eval_case",
                    Some(id.to_string()),
                    json!({ "suite_id": suite_id }),
                ),
            )
            .await
    }

    pub async fn list_eval_cases(
        &self,
        actor: &Actor,
        suite_id: Uuid,
        cursor: Option<&str>,
        limit: i64,
    ) -> Result<ListResponse<EvalCaseRecord>, AppError> {
        self.state.authz.require(actor, "moira:evals:read")?;
        let cursor = ListCursor::decode_optional(cursor, EVAL_CASES_SCOPE)?;
        let rows = self.repo.list_eval_cases(suite_id, cursor, limit).await?;
        Ok(paginate_by_created_at(rows, limit, EVAL_CASES_SCOPE, |r| {
            (r.created_at, r.id)
        }))
    }

    pub async fn delete_eval_case(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        suite_id: Uuid,
        case_id: Uuid,
    ) -> Result<(), AppError> {
        self.state.authz.require(actor, "moira:evals:delete")?;
        self.repo
            .delete_eval_case(
                suite_id,
                case_id,
                self.audit(
                    actor,
                    ctx,
                    "eval_case.delete",
                    "eval_case",
                    Some(case_id.to_string()),
                    json!({ "suite_id": suite_id }),
                ),
            )
            .await
    }

    // =====================================================================================
    // Eval runs — read-only. `eval_runs` is produced by execution, never writable through
    // this admin surface.
    // =====================================================================================

    pub async fn list_eval_runs(
        &self,
        actor: &Actor,
        suite_id: Uuid,
        cursor: Option<&str>,
        limit: i64,
    ) -> Result<ListResponse<EvalRunRecord>, AppError> {
        self.state.authz.require(actor, "moira:evals:read")?;
        let cursor = ListCursor::decode_optional(cursor, EVAL_RUNS_SCOPE)?;
        let rows = self.repo.list_eval_runs(suite_id, cursor, limit).await?;
        Ok(paginate_by_created_at(rows, limit, EVAL_RUNS_SCOPE, |r| {
            (r.created_at, r.id)
        }))
    }

    // =====================================================================================
    // Flows (issue #214, plan 12 §3). Steps travel inside the flow's own body — see
    // `domain::AgentFlowRecord`'s doc comment — so there is no separate steps CRUD here.
    // =====================================================================================

    pub async fn create_flow(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        request: AgentFlowCreateRequest,
    ) -> Result<AgentFlowRecord, AppError> {
        self.state.authz.require(actor, "moira:flows:write")?;
        if let Some(replay) = self
            .idempotency_replay(ctx, actor, "flow.create", &request)
            .await?
        {
            return Ok(replay);
        }
        validate_key("flow_key", &request.flow_key)?;
        validate_display_name(&request.display_name)?;
        validate_metadata(&request.metadata)?;
        validate_flow_steps(&request.steps)?;
        self.ensure_steps_reference_existing_agents(&request.steps)
            .await?;
        let id = Uuid::now_v7();
        let record = self
            .repo
            .create_flow(
                id,
                &request,
                self.audit(
                    actor,
                    ctx,
                    "flow.create",
                    "flow",
                    Some(id.to_string()),
                    json!({ "flow_key": &request.flow_key, "step_count": request.steps.len() }),
                ),
            )
            .await?;
        self.record_idempotency(ctx, actor, "flow.create", &request, &record)
            .await?;
        Ok(record)
    }

    pub async fn list_flows(
        &self,
        actor: &Actor,
        cursor: Option<&str>,
        limit: i64,
    ) -> Result<ListResponse<AgentFlowRecord>, AppError> {
        self.state.authz.require(actor, "moira:flows:read")?;
        let cursor = ListCursor::decode_optional(cursor, FLOWS_SCOPE)?;
        let rows = self.repo.list_flows(cursor, limit).await?;
        Ok(paginate_by_created_at(rows, limit, FLOWS_SCOPE, |r| {
            (r.created_at, r.id)
        }))
    }

    pub async fn get_flow(&self, actor: &Actor, id: Uuid) -> Result<AgentFlowRecord, AppError> {
        self.state.authz.require(actor, "moira:flows:read")?;
        self.repo.get_flow(id).await
    }

    pub async fn patch_flow(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        id: Uuid,
        expected_version: i64,
        request: AgentFlowPatchRequest,
    ) -> Result<AgentFlowRecord, AppError> {
        self.state.authz.require(actor, "moira:flows:write")?;
        if let Some(display_name) = &request.display_name {
            validate_display_name(display_name)?;
        }
        if let Some(metadata) = &request.metadata {
            validate_metadata(metadata)?;
        }
        if let Some(steps) = &request.steps {
            validate_flow_steps(steps)?;
            self.ensure_steps_reference_existing_agents(steps).await?;
        }
        self.repo
            .patch_flow(
                id,
                expected_version,
                &request,
                self.audit(
                    actor,
                    ctx,
                    "flow.update",
                    "flow",
                    Some(id.to_string()),
                    json!({}),
                ),
            )
            .await
    }

    pub async fn delete_flow(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        id: Uuid,
        expected_version: i64,
    ) -> Result<(), AppError> {
        self.state.authz.require(actor, "moira:flows:delete")?;
        self.repo
            .soft_delete_flow(
                id,
                expected_version,
                self.audit(
                    actor,
                    ctx,
                    "flow.delete",
                    "flow",
                    Some(id.to_string()),
                    json!({}),
                ),
            )
            .await
    }

    // =====================================================================================
    // Flow runs — read-only. `agent_flow_runs` is produced by the (not-yet-built) flow
    // orchestrator (#84 follow-up); there is no execution endpoint in this MVP.
    // =====================================================================================

    pub async fn list_flow_runs(
        &self,
        actor: &Actor,
        flow_id: Uuid,
        cursor: Option<&str>,
        limit: i64,
    ) -> Result<ListResponse<AgentFlowRunRecord>, AppError> {
        self.state.authz.require(actor, "moira:flows:read")?;
        let cursor = ListCursor::decode_optional(cursor, FLOW_RUNS_SCOPE)?;
        let rows = self.repo.list_flow_runs(flow_id, cursor, limit).await?;
        Ok(paginate_by_created_at(rows, limit, FLOW_RUNS_SCOPE, |r| {
            (r.created_at, r.id)
        }))
    }

    /// Fail-closed on a missing agent profile (product decisions 2026-08-06): every distinct
    /// `agent_profile_id` named by `steps` must be a live `agent_profiles` row before the
    /// flow (or its patched step list) is written. Deduplicated so a flow that legitimately
    /// reuses one agent across several steps checks it once, not once per step.
    async fn ensure_steps_reference_existing_agents(
        &self,
        steps: &[AgentFlowStepCreateRequest],
    ) -> Result<(), AppError> {
        let mut checked = HashSet::new();
        for step in steps {
            if !checked.insert(step.agent_profile_id) {
                continue;
            }
            self.runtime_repo
                .get_agent_profile(step.agent_profile_id)
                .await
                .map_err(|_| {
                    AppError::BadRequest(format!(
                        "flow step '{}' references a missing agent profile {}",
                        step.step_key, step.agent_profile_id
                    ))
                })?;
        }
        Ok(())
    }

    /// Builds this service's audit row; the repository writes it inside the write's own
    /// transaction, exactly as `RuntimeAdminService::runtime_audit` does.
    fn audit(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        action: &str,
        resource_type: &str,
        resource_id: Option<String>,
        metadata: Value,
    ) -> AuditLogInsert {
        AuditLogInsert {
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
        }
    }

    /// Current-formula idempotency replay. Unlike `RuntimeAdminService`, these are brand-new
    /// tables with no rows written under any legacy fingerprint/key-hash spelling, so the
    /// historical sweep is unnecessary here.
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
        let request_bytes = serde_json::to_vec(request)
            .map_err(|err| AppError::BadRequest(format!("invalid idempotent request: {err}")))?;
        let key_hash = hasher.hash(key.as_bytes());
        let fingerprint = actor_fingerprint(hasher, actor);
        let Some(record) = self
            .admin_repo
            .get_idempotency_record(&key_hash, &fingerprint, operation)
            .await?
        else {
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
        let request_bytes = serde_json::to_vec(request)
            .map_err(|err| AppError::BadRequest(format!("invalid idempotent request: {err}")))?;
        let record = IdempotencyRecord {
            id: Uuid::now_v7(),
            idempotency_key_hash: hasher.hash(key.as_bytes()),
            actor_fingerprint: actor_fingerprint(hasher, actor),
            operation: operation.to_string(),
            request_hash: hasher.hash(&request_bytes),
            response_status: Some(200),
            response_body,
            resource_id,
            expires_at: Utc::now() + Duration::hours(24),
        };
        self.admin_repo.put_idempotency_record(&record).await?;
        Ok(())
    }
}

/// Trims a `limit + 1`-row over-fetch to `limit`, computes `has_more`, and encodes
/// `next_cursor` from the last returned row. Mirrors `runtime_admin::paginate_by_created_at`.
fn paginate(
    mut rows: Vec<SkillRecord>,
    limit: i64,
    scope: CursorScope,
) -> ListResponse<SkillRecord> {
    let has_more = (rows.len() as i64) > limit;
    if has_more {
        rows.truncate(limit.max(0) as usize);
    }
    let next_cursor = if has_more {
        rows.last()
            .map(|record| ListCursor::new(record.created_at, record.id).encode(scope))
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

/// [`paginate`]'s twin for `skill_http_executors`, whose keyset is `(created_at, skill_id)`
/// rather than `(created_at, id)` — the table has no separate `id` column, `skill_id` is its
/// primary key.
fn paginate_executors(
    mut rows: Vec<SkillHttpExecutorRecord>,
    limit: i64,
    scope: CursorScope,
) -> ListResponse<SkillHttpExecutorRecord> {
    let has_more = (rows.len() as i64) > limit;
    if has_more {
        rows.truncate(limit.max(0) as usize);
    }
    let next_cursor = if has_more {
        rows.last()
            .map(|record| ListCursor::new(record.created_at, record.skill_id).encode(scope))
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

/// Generic twin of [`paginate`]/[`paginate_executors`] for every F2 record type (eval suites,
/// eval cases, eval runs, flows, flow runs) — one function rather than five near-identical
/// copies, since all five share the `(created_at, id)` keyset. Mirrors
/// `runtime_admin::paginate_by_created_at` exactly.
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

/// Maps a pure parse failure from `orchestration::openapi_import` onto the two catalogued
/// codes the wire contract promises: `import_cap_exceeded` carries the true operation count
/// in `details` (never silently truncated, §5 decision 23); every other parse failure is the
/// generic `invalid_openapi_spec`.
fn import_parse_error_to_app_error(error: OpenApiImportError) -> AppError {
    match error {
        OpenApiImportError::TooManyOperations { found, cap } => AppError::coded_with_details(
            StatusCode::BAD_REQUEST,
            "import_cap_exceeded",
            format!(
                "The OpenAPI document defines {found} operations, which exceeds the \
                 {cap}-operation import cap."
            ),
            json!({ "operation_count": found, "cap": cap }),
        ),
        other => AppError::coded(
            StatusCode::BAD_REQUEST,
            "invalid_openapi_spec",
            format!("The OpenAPI document could not be parsed: {other}"),
        ),
    }
}

/// SSRF-validates a skill's server URL — the document's `servers[0].url` on import, or a new
/// `url_template` on an executor PATCH — through the same
/// [`validate_outbound_url`](crate::security::validate_outbound_url) guard `security::ssrf`
/// already applies to JWKS fetches. Always enforced (`allow_insecure: false`): unlike JWKS,
/// this is not an operator-configured trust boundary with its own dev override, it is
/// outbound HTTP whose destination an admin's pasted document or edit fully controls (plan 12
/// §5, "SSRF safety is mandatory from day one").
async fn validate_skill_url(raw_url: &str) -> Result<url::Url, AppError> {
    let policy = OutboundUrlPolicy {
        subject: "skill http executor url",
        dns_timeout: std::time::Duration::from_millis(SKILL_URL_DNS_TIMEOUT_MS),
        allowed_hosts: Vec::new(),
        reject_credentials: true,
        allow_insecure: false,
    };
    validate_outbound_url(raw_url, &policy, &SystemResolver)
        .await
        .map_err(|denial| ssrf_blocked_error(&denial))
}

/// The denial `detail` can name a resolved internal address — logged server-side only, per
/// the same posture `security::ssrf`'s own JWKS path takes, so the response body never turns
/// into an SSRF oracle that confirms which internal hosts exist.
fn ssrf_blocked_error(denial: &OutboundUrlDenial) -> AppError {
    tracing::warn!(
        reason = denial.reason().as_str(),
        detail = denial.detail(),
        "skill server url blocked by the outbound SSRF policy"
    );
    AppError::coded(
        StatusCode::BAD_REQUEST,
        "ssrf_blocked_host",
        "The server URL was rejected by the outbound SSRF policy.",
    )
}

/// The database only enforces `timeout_ms > 0`; this additionally caps it so a PATCH cannot
/// configure a per-call timeout that outlives Moira's own execution deadlines by an unbounded
/// amount.
fn validate_executor_timeout_ms(timeout_ms: i32) -> Result<(), AppError> {
    if timeout_ms <= 0 || timeout_ms > MAX_EXECUTOR_TIMEOUT_MS {
        return Err(AppError::BadRequest(format!(
            "timeout_ms must be between 1 and {MAX_EXECUTOR_TIMEOUT_MS}"
        )));
    }
    Ok(())
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

/// A skill's `params_schema` is a JSON Schema for its tool arguments; it must be a JSON object
/// (or the empty default), never a scalar or array.
fn validate_json_object(label: &str, value: &Value) -> Result<(), AppError> {
    if value.is_null() || value.is_object() {
        Ok(())
    } else {
        Err(AppError::BadRequest(format!(
            "{label} must be a JSON object"
        )))
    }
}

/// An eval case's `input`/`expected` are required JSON documents — the wire type is `Value`
/// rather than `Option<Value>` so the key must be present, but a present-and-`null` value
/// (`"input": null`) still deserializes cleanly, so this rejects it explicitly rather than
/// letting a case with no real fixture reach the database.
fn validate_json_present(label: &str, value: &Value) -> Result<(), AppError> {
    if value.is_null() {
        Err(AppError::BadRequest(format!("{label} must not be null")))
    } else {
        Ok(())
    }
}

/// Validates one flow's step array before it ever reaches the repository: format and
/// uniqueness are checked here so a bad request is a clean 400, not a unique-constraint
/// violation surfacing as a 500. Referential validation (does `agent_profile_id` exist) is a
/// separate, `async` check — see `AgentPlatformService::ensure_steps_reference_existing_agents`.
fn validate_flow_steps(steps: &[AgentFlowStepCreateRequest]) -> Result<(), AppError> {
    if steps.len() > MAX_FLOW_STEPS {
        return Err(AppError::BadRequest(format!(
            "a flow may define at most {MAX_FLOW_STEPS} steps"
        )));
    }
    let mut keys = HashSet::new();
    let mut orders = HashSet::new();
    for step in steps {
        validate_key("step_key", &step.step_key)?;
        validate_json_object("input_mapping", &step.input_mapping)?;
        validate_metadata(&step.metadata)?;
        if step.step_order < 0 {
            return Err(AppError::BadRequest("step_order must be >= 0".to_string()));
        }
        if !keys.insert(step.step_key.clone()) {
            return Err(AppError::BadRequest(format!(
                "duplicate step_key '{}' within one flow",
                step.step_key
            )));
        }
        if !orders.insert(step.step_order) {
            return Err(AppError::BadRequest(format!(
                "duplicate step_order {} within one flow",
                step.step_order
            )));
        }
    }
    Ok(())
}

fn validate_tags(tags: &[String]) -> Result<(), AppError> {
    if tags.len() > 64 {
        return Err(AppError::BadRequest(
            "tags must contain at most 64 entries".to_string(),
        ));
    }
    if tags
        .iter()
        .any(|tag| tag.trim().is_empty() || tag.len() > 64)
    {
        return Err(AppError::BadRequest(
            "each tag must be 1-64 characters".to_string(),
        ));
    }
    Ok(())
}

fn validate_metadata(value: &Value) -> Result<(), AppError> {
    let bytes = serde_json::to_vec(value)
        .map_err(|err| AppError::BadRequest(format!("metadata is invalid JSON: {err}")))?;
    if bytes.len() > 16 * 1024 {
        return Err(AppError::BadRequest(
            "metadata must be at most 16KiB".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod skill_import_tests {
    use super::*;

    /// The cap-exceeded parse failure must reach the wire as the catalogued
    /// `import_cap_exceeded` code with the true count in `details` — never silently
    /// truncated (§5 decision 23).
    #[test]
    fn too_many_operations_maps_to_import_cap_exceeded_with_details() {
        let error = import_parse_error_to_app_error(OpenApiImportError::TooManyOperations {
            found: 301,
            cap: 300,
        });
        assert_eq!(error.status(), StatusCode::BAD_REQUEST);
        let response = error.error_response(None);
        assert_eq!(response.error.code, "import_cap_exceeded");
        assert_eq!(
            response.error.message_key,
            "moira.error.import_cap_exceeded"
        );
        let details = response.error.details.expect("details must be present");
        assert_eq!(details["operation_count"], 301);
        assert_eq!(details["cap"], 300);
    }

    /// Every other parse failure — bad version, missing server, no operations, and so on —
    /// maps to the single generic `invalid_openapi_spec` code.
    #[test]
    fn other_parse_failures_map_to_invalid_openapi_spec() {
        for failure in [
            OpenApiImportError::NotAnObject,
            OpenApiImportError::UnsupportedVersion,
            OpenApiImportError::MissingServerUrl,
            OpenApiImportError::InvalidServerUrl("not a url".to_string()),
            OpenApiImportError::NoOperations,
            // The three byte-budget refusals are 400s on the same generic code: they are a
            // property of the submitted document, and the message carries the numbers.
            OpenApiImportError::DocumentTooLarge {
                bytes: 3_000_000,
                cap: 524_288,
            },
            OpenApiImportError::OperationSchemaTooLarge {
                skill_key: "op0".to_string(),
                bytes: 200_000,
                cap: 65_536,
            },
            OpenApiImportError::TotalSchemaTooLarge {
                bytes: 90_000_000,
                cap: 2_097_152,
            },
        ] {
            let error = import_parse_error_to_app_error(failure);
            assert_eq!(error.status(), StatusCode::BAD_REQUEST);
            let response = error.error_response(None);
            assert_eq!(response.error.code, "invalid_openapi_spec");
            assert_eq!(
                response.error.message_key,
                "moira.error.invalid_openapi_spec"
            );
        }
    }

    /// The cloud metadata endpoint is the canonical SSRF target — a skill import or executor
    /// PATCH pointing at it must be refused before any database write, and the rejection
    /// must carry the catalogued `ssrf_blocked_host` code without leaking *why* (the denial
    /// reason and resolved address stay server-side, in the `tracing::warn!` this function
    /// emits, exactly like the JWKS path it shares its guard with).
    #[tokio::test]
    async fn validate_skill_url_blocks_the_cloud_metadata_endpoint() {
        let error = validate_skill_url("https://169.254.169.254/latest/meta-data/")
            .await
            .expect_err("the metadata endpoint must be refused");
        assert_eq!(error.status(), StatusCode::BAD_REQUEST);
        let response = error.error_response(None);
        assert_eq!(response.error.code, "ssrf_blocked_host");
        assert_eq!(response.error.message_key, "moira.error.ssrf_blocked_host");
        // The response must not leak the resolved address or the specific denial reason.
        assert!(!response.error.message.contains("169.254.169.254"));
    }

    #[tokio::test]
    async fn validate_skill_url_blocks_a_non_https_scheme() {
        let error = validate_skill_url("http://api.example.com/")
            .await
            .expect_err("a non-https server url must be refused");
        let response = error.error_response(None);
        assert_eq!(response.error.code, "ssrf_blocked_host");
    }

    #[test]
    fn executor_timeout_ms_rejects_zero_and_negative_values() {
        assert!(validate_executor_timeout_ms(0).is_err());
        assert!(validate_executor_timeout_ms(-1).is_err());
    }

    #[test]
    fn executor_timeout_ms_rejects_values_over_the_ceiling() {
        assert!(validate_executor_timeout_ms(MAX_EXECUTOR_TIMEOUT_MS + 1).is_err());
        assert!(validate_executor_timeout_ms(MAX_EXECUTOR_TIMEOUT_MS).is_ok());
        assert!(validate_executor_timeout_ms(1).is_ok());
    }
}

/// Pure unit coverage for the F2 (issue #214, plan 12 §3) evals/flows validators — no
/// database needed, following the same split `skill_import_tests` uses for pure logic versus
/// `tests/agent_platform.rs`'s end-to-end Postgres coverage.
#[cfg(test)]
mod f2_validation_tests {
    use super::*;

    fn sample_step(step_key: &str, step_order: i32) -> AgentFlowStepCreateRequest {
        AgentFlowStepCreateRequest {
            step_key: step_key.to_string(),
            step_order,
            agent_profile_id: Uuid::now_v7(),
            on_failure: crate::domain::FlowStepOnFailure::Abort,
            input_mapping: json!({}),
            metadata: json!({}),
        }
    }

    #[test]
    fn validate_flow_steps_accepts_an_empty_or_well_formed_list() {
        assert!(validate_flow_steps(&[]).is_ok());
        assert!(validate_flow_steps(&[sample_step("first", 0), sample_step("second", 1)]).is_ok());
    }

    #[test]
    fn validate_flow_steps_rejects_a_duplicate_step_key() {
        let error = validate_flow_steps(&[sample_step("same", 0), sample_step("same", 1)])
            .expect_err("duplicate step_key must be rejected");
        assert_eq!(error.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn validate_flow_steps_rejects_a_duplicate_step_order() {
        let error = validate_flow_steps(&[sample_step("a", 0), sample_step("b", 0)])
            .expect_err("duplicate step_order must be rejected");
        assert_eq!(error.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn validate_flow_steps_rejects_a_negative_step_order() {
        assert!(validate_flow_steps(&[sample_step("a", -1)]).is_err());
    }

    #[test]
    fn validate_flow_steps_rejects_a_malformed_step_key() {
        assert!(validate_flow_steps(&[sample_step("Not A Valid Key!", 0)]).is_err());
    }

    #[test]
    fn validate_flow_steps_rejects_more_than_the_cap() {
        let steps: Vec<_> = (0..=MAX_FLOW_STEPS as i32)
            .map(|order| sample_step(&format!("step-{order}"), order))
            .collect();
        assert!(validate_flow_steps(&steps).is_err());
    }

    #[test]
    fn validate_flow_steps_accepts_exactly_the_cap() {
        let steps: Vec<_> = (0..MAX_FLOW_STEPS as i32)
            .map(|order| sample_step(&format!("step-{order}"), order))
            .collect();
        assert!(validate_flow_steps(&steps).is_ok());
    }

    #[test]
    fn validate_json_present_rejects_null_and_accepts_everything_else() {
        assert!(validate_json_present("input", &Value::Null).is_err());
        assert!(validate_json_present("input", &json!({})).is_ok());
        assert!(validate_json_present("input", &json!("a string")).is_ok());
        assert!(validate_json_present("input", &json!(0)).is_ok());
    }

    #[test]
    fn grading_kind_round_trips_through_the_db_encoding() {
        use crate::domain::GradingKind;
        use crate::infra::pg_rows::{grading_kind_from_db, grading_kind_to_db};

        for kind in [
            GradingKind::ExactMatch,
            GradingKind::Contains,
            GradingKind::SchemaValid,
        ] {
            let encoded = grading_kind_to_db(&kind).to_string();
            assert_eq!(grading_kind_from_db(encoded).unwrap(), kind);
        }
    }

    #[test]
    fn flow_step_on_failure_round_trips_through_the_db_encoding() {
        use crate::domain::FlowStepOnFailure;
        use crate::infra::pg_rows::{flow_step_on_failure_from_db, flow_step_on_failure_to_db};

        for value in [FlowStepOnFailure::Abort, FlowStepOnFailure::Continue] {
            let encoded = flow_step_on_failure_to_db(&value).to_string();
            assert_eq!(flow_step_on_failure_from_db(encoded).unwrap(), value);
        }
    }

    #[test]
    fn unknown_grading_kind_from_db_is_a_classification_error_not_a_panic() {
        use crate::infra::pg_rows::grading_kind_from_db;

        let error = grading_kind_from_db("llm_judge".to_string())
            .expect_err("llm_judge is deliberately absent from the MVP grading set");
        assert_eq!(error.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }
}
