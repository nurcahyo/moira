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
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentFlowCreateRequest {
    pub flow_key: String,
    pub display_name: String,
    pub description: Option<String>,
    #[serde(default)]
    pub metadata: Value,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentFlowPatchRequest {
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub metadata: Option<Value>,
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
