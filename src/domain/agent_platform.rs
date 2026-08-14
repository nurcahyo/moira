//! Agent-platform domain types (issue #214, plan 12 §3/§5): skills, evaluations, flows.
//!
//! Sub-plan 1 of 3 is SCHEMA + CRUD only — there is no execution engine here. These are the
//! serde/`utoipa` shapes for the admin registries created by `migrations/0031_agent_platform.sql`.
//! They deliberately introduce no new "agent" concept: `agent_profiles` is extended in place
//! (decision 12), so its additive columns live on `AgentProfileRecord` in `super::runtime`, not
//! here.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::domain::ResourceStatus;

// =====================================================================================
// Skills — declarative tool definitions and skills-as-guards.
// =====================================================================================

/// The tool-vs-guard axis (plan 12 §5, skills-as-guards). A `tool` skill is offered to a model
/// as a callable tool; a `guard` skill is a deterministic policy check that may only narrow
/// access, never widen it.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SkillKind {
    Tool,
    Guard,
}

/// Skill lifecycle: `draft` -> `enabled`/`disabled`. Freshly authored or imported skills land
/// in `draft` and are reviewed before an agent can call them (fail-closed, §5 decision 22).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SkillStatus {
    Draft,
    Enabled,
    Disabled,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SkillRecord {
    pub id: Uuid,
    pub skill_key: String,
    pub display_name: String,
    pub description: Option<String>,
    pub kind: SkillKind,
    pub params_schema: Value,
    pub tags: Vec<String>,
    pub status: SkillStatus,
    pub metadata: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SkillCreateRequest {
    pub skill_key: String,
    pub display_name: String,
    pub description: Option<String>,
    pub kind: SkillKind,
    #[serde(default)]
    pub params_schema: Value,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub metadata: Value,
}

/// `kind` is immutable after creation, and `status` moves only through enable/disable — neither
/// is patchable here, mirroring how `agent_profiles` keeps `profile_key`/`status` out of PATCH.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SkillPatchRequest {
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub params_schema: Option<Value>,
    pub tags: Option<Vec<String>>,
    pub metadata: Option<Value>,
}

/// Bulk-enable is the reason a large imported spec does not become hundreds of clicks
/// (§5 decision 22). No `If-Match`: it is a multi-row operation with no single row version.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SkillBulkEnableRequest {
    pub skill_ids: Vec<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SkillBulkEnableResponse {
    pub data: Vec<SkillRecord>,
}

// =====================================================================================
// HTTP executors — the one-to-one HTTP execution template for a `kind = 'tool'` skill
// (issue #237, plan 12 §5). Schema: `migrations/0031_agent_platform.sql`. There is
// deliberately no live-execution `Tool` impl here — that is `HttpSkillTool`, deferred to
// the rig tool loop (#84); this module only carries the admin-CRUD wire shapes.
// =====================================================================================

/// The HTTP methods `skill_http_executors.method` accepts (migration check constraint).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

/// `skill_http_executors` has no `version`/`deleted_at` columns — F's migration gives
/// optimistic concurrency only to the three named registries (`skills`, `eval_suites`,
/// `agent_flows`); this row is a versionless 1:1 child of a `skills` row, cascade-deleted
/// with it. `updated_at` is therefore this resource's `If-Match` basis instead of an
/// integer `version` — see `executor_etag_headers`/`require_executor_if_match` in
/// `src/http/agent_platform.rs`, which encode/parse it as a quoted RFC 3339 timestamp
/// (microsecond precision, matching Postgres `timestamptz`) rather than reusing the
/// shared `etag_headers`/`require_if_match` helpers, which are hard-wired to an `i64`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SkillHttpExecutorRecord {
    pub skill_id: Uuid,
    pub method: HttpMethod,
    pub url_template: String,
    /// The exact host `url_template` resolves to, checked at execution time by
    /// `security::ssrf::validate_outbound_url` without re-parsing the `{placeholder}`
    /// template. Always derived server-side from `url_template`'s host — never a
    /// client-settable field, or PATCH could point execution at a host the URL no longer
    /// names.
    pub allowed_host: String,
    /// Static, non-secret headers only (migration comment, `0031_agent_platform.sql`).
    pub header_template: Value,
    /// References `provider_credentials` — no inline secrets (decision 21).
    pub credential_id: Option<Uuid>,
    pub timeout_ms: i32,
    pub response_schema: Option<Value>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// `allowed_host` is intentionally absent — see [`SkillHttpExecutorRecord::allowed_host`].
/// Changing `url_template` re-derives and re-validates it server-side rather than taking a
/// client-supplied value. Follows `SkillPatchRequest`'s coalesce convention: an omitted
/// field means "leave unchanged", so this cannot clear `response_schema` to `null` — the
/// same limitation `SkillPatchRequest.description` already accepts.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SkillHttpExecutorPatchRequest {
    pub method: Option<HttpMethod>,
    pub url_template: Option<String>,
    pub header_template: Option<Value>,
    pub credential_id: Option<Uuid>,
    pub timeout_ms: Option<i32>,
    pub response_schema: Option<Value>,
}

/// `POST /api/v1/admin/skills/import` — the raw OpenAPI 3.x document to import (plan 12
/// §5). Parsing is pure and lives in `orchestration::openapi_import`; this is only the
/// wire envelope.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SkillImportRequest {
    pub document: Value,
}

/// One row per imported operation (draft `skills` row + its `skill_http_executors` row),
/// created together and returned together so the caller can review-then-enable
/// (§5 decision 22) without a second round trip per skill.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SkillImportResponse {
    pub imported_count: usize,
    pub skills: Vec<SkillRecord>,
    pub executors: Vec<SkillHttpExecutorRecord>,
}

// =====================================================================================
// Evaluations — offline suites, cases, and runs.
// =====================================================================================

/// Cheap, MVP-safe grading kinds (§3). `llm_judge` is deliberately absent (decision 14).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum GradingKind {
    ExactMatch,
    Contains,
    SchemaValid,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EvalTriggerKind {
    OfflineManual,
    OfflineCi,
    OnlineSampled,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EvalRunStatus {
    Pending,
    Running,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EvalSuiteRecord {
    pub id: Uuid,
    pub suite_key: String,
    pub display_name: String,
    pub description: Option<String>,
    pub status: ResourceStatus,
    pub metadata: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct EvalSuiteCreateRequest {
    pub suite_key: String,
    pub display_name: String,
    pub description: Option<String>,
    #[serde(default)]
    pub metadata: Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct EvalSuitePatchRequest {
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub metadata: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EvalCaseRecord {
    pub id: Uuid,
    pub suite_id: Uuid,
    pub input: Value,
    pub expected: Value,
    pub grading_kind: GradingKind,
    pub metadata: Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct EvalCaseCreateRequest {
    pub input: Value,
    pub expected: Value,
    pub grading_kind: GradingKind,
    #[serde(default)]
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct EvalRunRecord {
    pub id: Uuid,
    pub suite_id: Option<Uuid>,
    pub agent_profile_id: Option<Uuid>,
    pub trigger_kind: EvalTriggerKind,
    pub execution_id: Option<Uuid>,
    pub status: EvalRunStatus,
    pub score: Option<f64>,
    pub results: Value,
    pub metadata: Value,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

// =====================================================================================
// Multi-agent flows — a DAG of steps; MVP is a linear sequential chain.
// =====================================================================================

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum FlowStepOnFailure {
    Abort,
    Continue,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum FlowRunStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum FlowStepRunStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Skipped,
}

/// A flow's steps are managed as an ordered array inside this record rather than through a
/// separate `agent_flow_steps` sub-resource (decision 13: simplest contract, matches the
/// sequential-only MVP) — `create`/`patch` accept the whole ordered list in the request body
/// and this record echoes the current list back, so a client never has to reconcile a
/// separately-paginated child collection with its parent.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AgentFlowRecord {
    pub id: Uuid,
    pub flow_key: String,
    pub display_name: String,
    pub description: Option<String>,
    pub status: ResourceStatus,
    pub metadata: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub version: i64,
    pub steps: Vec<AgentFlowStepRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentFlowCreateRequest {
    pub flow_key: String,
    pub display_name: String,
    pub description: Option<String>,
    #[serde(default)]
    pub metadata: Value,
    /// The flow's steps, in the order they execute. May be empty — a flow can be authored
    /// before its steps are decided; it simply cannot run yet (there is no execution engine
    /// in this MVP regardless).
    #[serde(default)]
    pub steps: Vec<AgentFlowStepCreateRequest>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentFlowPatchRequest {
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub metadata: Option<Value>,
    /// `Some(steps)` atomically replaces the flow's entire ordered step list; `None` (the
    /// field omitted) leaves the existing steps untouched — the same coalesce convention
    /// every other patch request in this module follows for its scalar fields.
    pub steps: Option<Vec<AgentFlowStepCreateRequest>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AgentFlowStepRecord {
    pub id: Uuid,
    pub flow_id: Uuid,
    pub step_key: String,
    pub step_order: i32,
    pub agent_profile_id: Uuid,
    pub on_failure: FlowStepOnFailure,
    pub input_mapping: Value,
    pub metadata: Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentFlowStepCreateRequest {
    pub step_key: String,
    pub step_order: i32,
    pub agent_profile_id: Uuid,
    #[serde(default = "default_on_failure")]
    pub on_failure: FlowStepOnFailure,
    #[serde(default)]
    pub input_mapping: Value,
    #[serde(default)]
    pub metadata: Value,
}

fn default_on_failure() -> FlowStepOnFailure {
    FlowStepOnFailure::Abort
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AgentFlowRunRecord {
    pub id: Uuid,
    pub flow_id: Uuid,
    pub status: FlowRunStatus,
    pub metadata: Value,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}
