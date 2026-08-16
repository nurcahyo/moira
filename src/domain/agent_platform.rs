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
// Skill resolution and guards — the read side the rig tool loop drives (issue #84).
//
// Pure classification only, in the spirit of `runtime::AgentProfileResolution::classify`:
// no SQL, no Rig, no HTTP. `infra::repositories::agent_platform` produces the raw rows,
// `application::execution` decides what a refusal means, and `orchestration::skill_tool`
// turns an allowed [`SkillResolution::Tool`] into a live `rig_core::tool::Tool`.
//
// Deliberately **not** `ToSchema`: none of these types is on an HTTP route, and deriving
// the schema would put them into `docs/openapi.json` for nothing.
// =====================================================================================

/// One `agent_profiles.skill_refs` entry joined against the rows it names.
///
/// `skill` is `None` for a reference no live `skills` row answers — the dangling-reference
/// state F50 taught us to keep distinguishable rather than collapsing into "no skills".
#[derive(Debug, Clone)]
pub struct AgentSkillBinding {
    pub skill_id: Uuid,
    pub skill: Option<SkillRecord>,
    pub executor: Option<SkillHttpExecutorRecord>,
}

/// Why a `skill_refs` entry cannot be used for this execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillUnusableReason {
    /// No live `skills` row has that id (never existed, or soft-deleted).
    Missing,
    /// The row is there and is not `enabled` — either still `draft` (never reviewed,
    /// §5 decision 22) or switched off by an operator.
    NotEnabled,
    /// A `kind = 'tool'` skill with no `skill_http_executors` row: there is nothing to
    /// call, so advertising it would offer the model a tool that can only fail.
    NoExecutor,
}

impl SkillUnusableReason {
    /// Stable, machine-filterable token for audit metadata, runtime events and log fields.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::NotEnabled => "not_enabled",
            Self::NoExecutor => "no_executor",
        }
    }
}

/// What one `skill_refs` entry resolved to.
#[derive(Debug, Clone)]
pub enum SkillResolution {
    /// An enabled `kind = 'tool'` skill with its HTTP executor: advertise and dispatch it.
    Tool {
        skill: Box<SkillRecord>,
        executor: Box<SkillHttpExecutorRecord>,
    },
    /// An enabled `kind = 'guard'` skill: evaluated before dispatch, never advertised.
    Guard { skill: Box<SkillRecord> },
    /// Refused — see [`SkillUnusableReason`].
    Unusable {
        skill_id: Uuid,
        reason: SkillUnusableReason,
    },
}

impl SkillResolution {
    /// Classifies one binding. Fail-closed in the same direction as
    /// [`crate::domain::AgentProfileResolution::classify`]: anything that is not
    /// unambiguously an enabled, complete row becomes [`Self::Unusable`], and the caller
    /// decides whether that refuses the execution.
    ///
    /// A guard needs no executor: it is a deterministic policy check, never an outbound
    /// call (plan 12 §5 — model-backed guards are an Enterprise-stage item).
    pub fn classify(binding: AgentSkillBinding) -> Self {
        let AgentSkillBinding {
            skill_id,
            skill,
            executor,
        } = binding;
        let Some(skill) = skill else {
            return Self::Unusable {
                skill_id,
                reason: SkillUnusableReason::Missing,
            };
        };
        if skill.deleted_at.is_some() || skill.status != SkillStatus::Enabled {
            return Self::Unusable {
                skill_id,
                reason: SkillUnusableReason::NotEnabled,
            };
        }
        match skill.kind {
            SkillKind::Guard => Self::Guard {
                skill: Box::new(skill),
            },
            SkillKind::Tool => match executor {
                Some(executor) => Self::Tool {
                    skill: Box::new(skill),
                    executor: Box::new(executor),
                },
                None => Self::Unusable {
                    skill_id,
                    reason: SkillUnusableReason::NoExecutor,
                },
            },
        }
    }
}

// =====================================================================================
// Which stored credential a skill executor may carry (issue #253 finding 1).
// =====================================================================================

/// Whether a stored `provider_credentials` row may be bound to — and therefore sent to — a
/// skill executor whose SSRF-validated destination host is `allowed_host`.
///
/// # What this closes
///
/// `skill_http_executors.credential_id` is dereferenced at call time, decrypted, and
/// attached as `Authorization: Bearer <plaintext>` by
/// [`crate::orchestration::HttpSkillTool`]. Before this rule the only check on that id was
/// that the row existed, so a holder of `moira:skills:write` could bind *any* provider
/// credential in the deployment to *any* host that passes the SSRF guard — and
/// `https://collector.attacker.example` passes it, because it is an ordinary public host.
/// That made `moira:skills:write` silently equivalent to reading the plaintext of every row
/// in `provider_credentials`, a capability no other admin scope grants: the credentials
/// surface only ever returns masked values.
///
/// The rule is entitlement by destination: a credential belongs to a provider, that provider
/// declares where it talks (`providers.base_url`), and the secret may only be sent to that
/// same host. Binding then moves no secret anywhere it was not already going.
///
/// # Why a provider with no `base_url` is refused
///
/// `providers.base_url` is `Option`: a provider left on its vendor default (`openai`,
/// `anthropic`, …) has none, and the default endpoint is Rig's business, not a value stored
/// here. There is therefore no host this function could compare against, and the fail-closed
/// answer is the only safe one — inventing the vendor default would make this rule's
/// correctness depend on a table of hostnames kept in step with `rig-core`. An operator who
/// genuinely wants such a credential on a skill sets that provider's `base_url` explicitly,
/// which is a visible, audited admin write.
///
/// Comparison is on the parsed host only — never on the URL string — so
/// `https://api.vendor.example@evil.example/` cannot masquerade as `api.vendor.example`:
/// `Url` puts that value in the userinfo and reports the host as `evil.example`. Case is
/// folded because hostnames are case-insensitive and `allowed_host` is stored as the
/// `url` crate produced it.
pub fn credential_binding_permits_host(
    provider_base_url: Option<&str>,
    allowed_host: &str,
) -> bool {
    let Some(base_url) = provider_base_url else {
        return false;
    };
    let Ok(parsed) = url::Url::parse(base_url.trim()) else {
        return false;
    };
    let Some(host) = parsed.host_str() else {
        return false;
    };
    !allowed_host.is_empty() && host.eq_ignore_ascii_case(allowed_host)
}

/// What resolving a `skill_http_executors.credential_id` produced.
///
/// Four outcomes rather than `Option`, because "there is no usable row", "the row is real but
/// its provider is gone" and "the row is real but is not entitled to this destination" call
/// for different operator remedies and must not collapse into one message. They did once:
/// a soft-deleted provider arrived as [`Unusable`](Self::Unusable), so the operator was told
/// the credential was "missing, expired or revoked" while the `provider_credentials` row sat
/// there active and unexpired, pointing them at the one table that was fine.
#[derive(Debug)]
pub enum SkillCredentialOutcome {
    Resolved(Box<crate::domain::ResolvedCredential>),
    /// No live, active, unexpired `provider_credentials` row answers the id, or it carries
    /// no secret field with an HTTP form.
    Unusable,
    /// The credential row itself is live, but its owning `providers` row is soft-deleted.
    ///
    /// A provider that no longer exists declares nothing, so it entitles no destination —
    /// and a credential nobody can see on the admin plane must not keep being sent by a
    /// skill. Distinct from [`Unusable`](Self::Unusable) because the remedy is on the
    /// provider or the executor, not on the credential, and distinct from
    /// [`HostNotEntitled`](Self::HostNotEntitled) because such a row's `base_url` may name
    /// the executor's host exactly: no host comparison can find it. Reported separately by
    /// the pre-deploy inventory query in `docs/agent-platform.md`, which is the only tool an
    /// operator has for finding these before an upgrade turns them into failed executions.
    ProviderDeleted,
    /// The row exists, but its provider's configured `base_url` host is not the executor's
    /// `allowed_host` — see [`credential_binding_permits_host`]. Refused **before**
    /// decryption: a secret that is not going to be sent is not worth unsealing.
    HostNotEntitled,
}

/// A deterministic guard policy, parsed from a `kind = 'guard'` skill's `metadata.guard`
/// object (plan 12 §5, "skills as guards").
///
/// **Guards narrow, never widen** (CONVENTIONS §7.5, the same direction-of-trust rule as
/// the no-scope-claim invariant). Every field below can only remove a tool call that
/// Moira's own authorization already permitted; there is no field that grants anything,
/// and adding one would be the bug this type exists to prevent.
///
/// MVP shape is a policy check, not a model call: no provider, no cost, no latency beyond
/// a couple of string comparisons.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GuardPolicy {
    /// When `Some`, only these skill keys may be called. `None` means this guard does not
    /// narrow on that axis — **not** "allow everything on every axis", since the other
    /// fields still apply.
    pub allowed_skill_keys: Option<Vec<String>>,
    /// Skill keys this guard refuses outright. Evaluated after `allowed_skill_keys`, so a
    /// key in both is denied.
    pub denied_skill_keys: Vec<String>,
    /// Scopes the caller must already hold. The guard never grants them — it only refuses
    /// when one is absent, which is why this can never widen access.
    pub required_scopes: Vec<String>,
}

/// Why [`GuardPolicy::parse`] refused to produce a policy.
///
/// A parse failure is **not** an empty policy: an unreadable guard denies everything it
/// governs, because the alternative is that a typo in an operator's JSON silently disables
/// the control they wrote it to add.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardPolicyError {
    /// `metadata` carries no `guard` object at all.
    Missing,
    /// `metadata.guard` is present but is not an object, or a field has the wrong type.
    Malformed,
}

impl GuardPolicy {
    /// Parses `metadata.guard` off a `kind = 'guard'` skill's `metadata` column.
    pub fn parse(metadata: &Value) -> Result<Self, GuardPolicyError> {
        let Some(guard) = metadata.get("guard") else {
            return Err(GuardPolicyError::Missing);
        };
        let guard = guard.as_object().ok_or(GuardPolicyError::Malformed)?;
        let allowed_skill_keys = match guard.get("allowed_skill_keys") {
            None | Some(Value::Null) => None,
            Some(value) => Some(string_list(value)?),
        };
        let denied_skill_keys = match guard.get("denied_skill_keys") {
            None | Some(Value::Null) => Vec::new(),
            Some(value) => string_list(value)?,
        };
        let required_scopes = match guard.get("required_scopes") {
            None | Some(Value::Null) => Vec::new(),
            Some(value) => string_list(value)?,
        };
        Ok(Self {
            allowed_skill_keys,
            denied_skill_keys,
            required_scopes,
        })
    }
}

fn string_list(value: &Value) -> Result<Vec<String>, GuardPolicyError> {
    let array = value.as_array().ok_or(GuardPolicyError::Malformed)?;
    array
        .iter()
        .map(|entry| {
            entry
                .as_str()
                .map(str::to_string)
                .ok_or(GuardPolicyError::Malformed)
        })
        .collect()
}

/// Why a guard refused one tool call. Stable tokens: they are metric labels and runtime
/// event payload values, and the model sees the token rather than a sentence assembled
/// per call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardDenialReason {
    /// The guard has an allow-list and the skill key is not on it.
    SkillNotAllowed,
    /// The guard's deny-list names the skill key.
    SkillDenied,
    /// The caller does not hold a scope the guard requires.
    MissingScope,
    /// The guard's own policy could not be read — denied rather than ignored.
    PolicyUnreadable,
}

impl GuardDenialReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SkillNotAllowed => "skill_not_allowed",
            Self::SkillDenied => "skill_denied",
            Self::MissingScope => "missing_scope",
            Self::PolicyUnreadable => "policy_unreadable",
        }
    }
}

/// A guard's verdict on one tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardVerdict {
    Allow,
    Deny {
        guard_key: String,
        reason: GuardDenialReason,
    },
}

/// One enabled `kind = 'guard'` skill, reduced to what evaluation needs.
#[derive(Debug, Clone)]
pub struct SkillGuard {
    pub guard_key: String,
    pub policy: Result<GuardPolicy, GuardPolicyError>,
}

impl SkillGuard {
    pub fn from_record(record: &SkillRecord) -> Self {
        Self {
            guard_key: record.skill_key.clone(),
            policy: GuardPolicy::parse(&record.metadata),
        }
    }
}

/// Everything a guard is allowed to look at. Deliberately tiny: a guard sees the caller's
/// already-granted scopes and the skill key being called, never the prompt, never a
/// credential, never the model's arguments.
#[derive(Debug, Clone, Copy)]
pub struct GuardContext<'a> {
    pub caller_scopes: &'a [String],
}

/// Evaluates every guard against one tool call, **fail-closed and in order**: the first
/// denial wins and no later guard can reverse it, which is the mechanical statement of
/// "a guard may only narrow".
pub fn evaluate_guards(
    guards: &[SkillGuard],
    skill_key: &str,
    context: GuardContext<'_>,
) -> GuardVerdict {
    for guard in guards {
        let policy = match &guard.policy {
            Ok(policy) => policy,
            Err(_) => {
                return GuardVerdict::Deny {
                    guard_key: guard.guard_key.clone(),
                    reason: GuardDenialReason::PolicyUnreadable,
                };
            }
        };
        if let Some(allowed) = &policy.allowed_skill_keys
            && !allowed.iter().any(|key| key == skill_key)
        {
            return GuardVerdict::Deny {
                guard_key: guard.guard_key.clone(),
                reason: GuardDenialReason::SkillNotAllowed,
            };
        }
        if policy.denied_skill_keys.iter().any(|key| key == skill_key) {
            return GuardVerdict::Deny {
                guard_key: guard.guard_key.clone(),
                reason: GuardDenialReason::SkillDenied,
            };
        }
        for required in &policy.required_scopes {
            if !context.caller_scopes.iter().any(|scope| scope == required) {
                return GuardVerdict::Deny {
                    guard_key: guard.guard_key.clone(),
                    reason: GuardDenialReason::MissingScope,
                };
            }
        }
    }
    GuardVerdict::Allow
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

/// One `agent_flow_step_runs` row — a single step's execution within a flow run (issue #214,
/// plan 12 §3). Append-only, produced by the flow orchestrator, never authored through the
/// admin CRUD surface. `execution_id` correlates the step to the underlying pipeline execution
/// so an operator can join it against `execution_attempts`/`usage_records`/audit rows;
/// `error_summary` carries a sanitized failure reason (a message key, never a provider body)
/// on the step that aborted the run (decision 15).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AgentFlowStepRunRecord {
    pub id: Uuid,
    pub flow_run_id: Uuid,
    pub step_id: Uuid,
    pub execution_id: Option<Uuid>,
    pub status: FlowStepRunStatus,
    pub error_summary: Option<String>,
    pub created_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

/// `POST /api/v1/admin/flows/{id}/run` body. The optional `input` seeds the first step's
/// prompt; when omitted the first step runs with an empty prompt (a flow whose first step
/// reads only from its agent profile's preamble is legitimate). A step may still declare
/// `input_mapping` to draw from `run_input` or a literal instead of the previous step's
/// output — see `application::flow_eval_execution` for the passing convention.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct FlowRunRequest {
    #[serde(default)]
    pub input: Option<Value>,
}

/// `POST /api/v1/admin/flows/{id}/run` response: the finalized flow run plus one step-run row
/// per step attempted, in step order. Returned with `200` even when the run failed — a
/// step failure is data (the run's `status` is `failed` and the aborting step's
/// `error_summary` names why), not an HTTP error.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AgentFlowRunResult {
    pub run: AgentFlowRunRecord,
    pub steps: Vec<AgentFlowStepRunRecord>,
}

/// `POST /api/v1/admin/eval-suites/{id}/run` body. `agent_profile_id` names the target the
/// suite's cases are graded against; when omitted it falls back to the suite's
/// `metadata.target_agent_profile_id`. If neither resolves, the run is refused with
/// `eval_target_missing` — an eval with no subject cannot measure anything.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct EvalRunRequest {
    #[serde(default)]
    pub agent_profile_id: Option<Uuid>,
}

#[cfg(test)]
mod credential_binding_tests {
    use super::credential_binding_permits_host;

    #[test]
    fn a_credential_may_be_bound_to_its_own_providers_host() {
        assert!(credential_binding_permits_host(
            Some("https://api.vendor.example/v1"),
            "api.vendor.example"
        ));
    }

    /// Hostnames are case-insensitive, and `allowed_host` is stored as the `url` crate
    /// produced it rather than as the operator typed it.
    ///
    /// The empty and boundary ports are the counterpart to
    /// [`an_unparseable_or_hostless_base_url_entitles_nothing`]: `Url::parse` accepts both,
    /// so the port check the documented inventory query grew must not report them either.
    #[test]
    fn the_host_comparison_folds_case_and_ignores_path_port_and_whitespace() {
        for base in [
            "  https://API.Vendor.Example:8443/v1/chat  ",
            "https://api.vendor.example:/v1",
            "https://api.vendor.example:65535/v1",
        ] {
            assert!(
                credential_binding_permits_host(Some(base), "api.vendor.example"),
                "{base} names api.vendor.example and must stay entitled"
            );
        }
    }

    /// The whole point of the rule: issue #253 finding 1's exfiltration destination.
    #[test]
    fn a_credential_may_not_be_bound_to_an_unrelated_public_host() {
        assert!(!credential_binding_permits_host(
            Some("https://api.openai.com/v1"),
            "collector.attacker.example"
        ));
    }

    /// `https://api.vendor.example@evil.example/` parses with host `evil.example` and
    /// `api.vendor.example` as userinfo. Comparing the parsed host rather than the string is
    /// what makes that a refusal instead of a match.
    #[test]
    fn a_userinfo_prefix_cannot_impersonate_the_allowed_host() {
        assert!(!credential_binding_permits_host(
            Some("https://api.vendor.example@evil.example/v1"),
            "api.vendor.example"
        ));
        assert!(credential_binding_permits_host(
            Some("https://api.vendor.example@evil.example/v1"),
            "evil.example"
        ));
    }

    /// A suffix or prefix of the allowed host is a different host.
    #[test]
    fn a_neighbouring_hostname_is_not_the_allowed_host() {
        for base in [
            "https://evil-api.vendor.example",
            "https://api.vendor.example.evil.test",
            "https://vendor.example",
        ] {
            assert!(
                !credential_binding_permits_host(Some(base), "api.vendor.example"),
                "{base} must not satisfy api.vendor.example"
            );
        }
    }

    /// Fail-closed: a provider left on its vendor default has no stored host to compare
    /// against, so no credential of its may be bound anywhere.
    #[test]
    fn a_provider_with_no_base_url_entitles_nothing() {
        assert!(!credential_binding_permits_host(None, "api.openai.com"));
    }

    /// The shapes here are also the ones the documented inventory query in
    /// `docs/agent-platform.md` used to miss, so this list and that query's `has_authority`
    /// and port checks describe the same set. Each spells `api.vendor.example` plainly
    /// enough for a string extraction to return it, and each is refused:
    ///
    /// * `api.vendor.example/v1` has no scheme, so it is not an absolute URL at all.
    /// * `api.vendor.example:8443/v1` *does* parse — as a scheme named `api.vendor.example`
    ///   carrying the opaque path `8443/v1`, with no host for `host_str()` to return.
    /// * `:nope` and `:99999` are not ports `Url::parse` accepts, so neither value parses,
    ///   and the host the operator can read in the string is never produced.
    #[test]
    fn an_unparseable_or_hostless_base_url_entitles_nothing() {
        for base in [
            "not-a-url",
            "/relative/path",
            "mailto:ops@vendor.example",
            "api.vendor.example/v1",
            "api.vendor.example:8443/v1",
            "https://api.vendor.example:nope/v1",
            "https://api.vendor.example:99999/v1",
        ] {
            assert!(
                !credential_binding_permits_host(Some(base), "api.vendor.example"),
                "{base} must entitle nothing"
            );
        }
    }

    /// An executor row with an empty `allowed_host` must not become a wildcard, whatever
    /// wrote it.
    #[test]
    fn an_empty_allowed_host_is_never_satisfied() {
        assert!(!credential_binding_permits_host(
            Some("https://api.vendor.example"),
            ""
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn skill(kind: SkillKind, status: SkillStatus) -> SkillRecord {
        SkillRecord {
            id: Uuid::now_v7(),
            skill_key: "orders_get".to_string(),
            display_name: "Get order".to_string(),
            description: Some("look an order up".to_string()),
            kind,
            params_schema: json!({"type": "object", "properties": {}}),
            tags: Vec::new(),
            status,
            metadata: json!({}),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            deleted_at: None,
            version: 1,
        }
    }

    fn executor(skill_id: Uuid) -> SkillHttpExecutorRecord {
        SkillHttpExecutorRecord {
            skill_id,
            method: HttpMethod::Get,
            url_template: "https://api.example.test/orders".to_string(),
            allowed_host: "api.example.test".to_string(),
            header_template: json!({}),
            credential_id: None,
            timeout_ms: 5_000,
            response_schema: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    /// The four ways a `skill_refs` entry fails to become a callable tool, each with its
    /// own reason token. One test rather than four because the discrimination *between*
    /// them is the property — an implementation that collapsed them into a single
    /// "unusable" token would leave an operator unable to tell "I never created it" from
    /// "I forgot to enable it", which are opposite remedies.
    #[test]
    fn only_an_enabled_tool_skill_with_an_executor_becomes_a_callable_tool() {
        let id = Uuid::now_v7();
        assert!(matches!(
            SkillResolution::classify(AgentSkillBinding {
                skill_id: id,
                skill: None,
                executor: None,
            }),
            SkillResolution::Unusable {
                reason: SkillUnusableReason::Missing,
                ..
            }
        ));

        let draft = skill(SkillKind::Tool, SkillStatus::Draft);
        assert!(matches!(
            SkillResolution::classify(AgentSkillBinding {
                skill_id: draft.id,
                executor: Some(executor(draft.id)),
                skill: Some(draft),
            }),
            SkillResolution::Unusable {
                reason: SkillUnusableReason::NotEnabled,
                ..
            }
        ));

        let disabled = skill(SkillKind::Tool, SkillStatus::Disabled);
        assert!(matches!(
            SkillResolution::classify(AgentSkillBinding {
                skill_id: disabled.id,
                executor: Some(executor(disabled.id)),
                skill: Some(disabled),
            }),
            SkillResolution::Unusable {
                reason: SkillUnusableReason::NotEnabled,
                ..
            }
        ));

        let orphan = skill(SkillKind::Tool, SkillStatus::Enabled);
        assert!(matches!(
            SkillResolution::classify(AgentSkillBinding {
                skill_id: orphan.id,
                skill: Some(orphan),
                executor: None,
            }),
            SkillResolution::Unusable {
                reason: SkillUnusableReason::NoExecutor,
                ..
            }
        ));

        let usable = skill(SkillKind::Tool, SkillStatus::Enabled);
        assert!(matches!(
            SkillResolution::classify(AgentSkillBinding {
                skill_id: usable.id,
                executor: Some(executor(usable.id)),
                skill: Some(usable),
            }),
            SkillResolution::Tool { .. }
        ));
    }

    /// A soft-deleted row that still says `enabled` is gone, not enabled — the same
    /// reading `AgentProfileResolution::classify` takes of a half-written row.
    #[test]
    fn a_soft_deleted_skill_is_unusable_whatever_its_status_column_says() {
        let mut record = skill(SkillKind::Tool, SkillStatus::Enabled);
        record.deleted_at = Some(Utc::now());
        assert!(matches!(
            SkillResolution::classify(AgentSkillBinding {
                skill_id: record.id,
                executor: Some(executor(record.id)),
                skill: Some(record),
            }),
            SkillResolution::Unusable {
                reason: SkillUnusableReason::NotEnabled,
                ..
            }
        ));
    }

    /// A guard needs no executor — it never makes an outbound call.
    #[test]
    fn an_enabled_guard_skill_resolves_without_an_executor() {
        let guard = skill(SkillKind::Guard, SkillStatus::Enabled);
        assert!(matches!(
            SkillResolution::classify(AgentSkillBinding {
                skill_id: guard.id,
                skill: Some(guard),
                executor: None,
            }),
            SkillResolution::Guard { .. }
        ));
    }

    #[test]
    fn guard_policy_parses_every_narrowing_axis() {
        let policy = GuardPolicy::parse(&json!({
            "guard": {
                "allowed_skill_keys": ["orders_get"],
                "denied_skill_keys": ["orders_delete"],
                "required_scopes": ["moira:execution:use-tools"]
            }
        }))
        .expect("a well-formed guard policy parses");
        assert_eq!(
            policy,
            GuardPolicy {
                allowed_skill_keys: Some(vec!["orders_get".to_string()]),
                denied_skill_keys: vec!["orders_delete".to_string()],
                required_scopes: vec!["moira:execution:use-tools".to_string()],
            }
        );
    }

    /// The fail-closed half, and the reason [`GuardPolicy::parse`] returns a `Result`
    /// rather than a default: a guard whose JSON an operator mistyped must deny, not
    /// silently stop guarding.
    #[test]
    fn an_unreadable_guard_policy_denies_rather_than_permitting_everything() {
        assert_eq!(
            GuardPolicy::parse(&json!({})),
            Err(GuardPolicyError::Missing)
        );
        assert_eq!(
            GuardPolicy::parse(&json!({ "guard": "yes" })),
            Err(GuardPolicyError::Malformed)
        );
        assert_eq!(
            GuardPolicy::parse(&json!({ "guard": { "required_scopes": [7] } })),
            Err(GuardPolicyError::Malformed)
        );

        let guards = vec![SkillGuard {
            guard_key: "broken".to_string(),
            policy: Err(GuardPolicyError::Malformed),
        }];
        assert_eq!(
            evaluate_guards(&guards, "orders_get", GuardContext { caller_scopes: &[] }),
            GuardVerdict::Deny {
                guard_key: "broken".to_string(),
                reason: GuardDenialReason::PolicyUnreadable,
            }
        );
    }

    #[test]
    fn guards_narrow_on_each_axis_and_allow_when_none_of_them_object() {
        let scopes = vec!["moira:execution:use-tools".to_string()];
        let allow_list = SkillGuard {
            guard_key: "allow_list".to_string(),
            policy: Ok(GuardPolicy {
                allowed_skill_keys: Some(vec!["orders_get".to_string()]),
                ..GuardPolicy::default()
            }),
        };
        assert_eq!(
            evaluate_guards(
                std::slice::from_ref(&allow_list),
                "orders_get",
                GuardContext {
                    caller_scopes: &scopes
                }
            ),
            GuardVerdict::Allow
        );
        assert_eq!(
            evaluate_guards(
                std::slice::from_ref(&allow_list),
                "orders_delete",
                GuardContext {
                    caller_scopes: &scopes
                }
            ),
            GuardVerdict::Deny {
                guard_key: "allow_list".to_string(),
                reason: GuardDenialReason::SkillNotAllowed,
            }
        );

        let deny_list = SkillGuard {
            guard_key: "deny_list".to_string(),
            policy: Ok(GuardPolicy {
                denied_skill_keys: vec!["orders_get".to_string()],
                ..GuardPolicy::default()
            }),
        };
        assert_eq!(
            evaluate_guards(
                std::slice::from_ref(&deny_list),
                "orders_get",
                GuardContext {
                    caller_scopes: &scopes
                }
            ),
            GuardVerdict::Deny {
                guard_key: "deny_list".to_string(),
                reason: GuardDenialReason::SkillDenied,
            }
        );

        let scoped = SkillGuard {
            guard_key: "scoped".to_string(),
            policy: Ok(GuardPolicy {
                required_scopes: vec!["moira:execution:use-tools".to_string()],
                ..GuardPolicy::default()
            }),
        };
        assert_eq!(
            evaluate_guards(
                std::slice::from_ref(&scoped),
                "orders_get",
                GuardContext {
                    caller_scopes: &scopes
                }
            ),
            GuardVerdict::Allow
        );
        assert_eq!(
            evaluate_guards(
                std::slice::from_ref(&scoped),
                "orders_get",
                GuardContext { caller_scopes: &[] }
            ),
            GuardVerdict::Deny {
                guard_key: "scoped".to_string(),
                reason: GuardDenialReason::MissingScope,
            }
        );
    }

    /// **Guards may only narrow.** A permissive guard cannot re-open what a stricter one
    /// closed, because evaluation returns on the first denial rather than folding verdicts
    /// together. Ordering a permissive guard on *both* sides of the strict one is what
    /// makes the case load-bearing: an implementation that let the last verdict win, or
    /// that treated any `Allow` as sufficient, would answer `Allow` here.
    #[test]
    fn a_later_permissive_guard_cannot_reopen_what_an_earlier_one_denied() {
        let guards = vec![
            SkillGuard {
                guard_key: "permissive".to_string(),
                policy: Ok(GuardPolicy::default()),
            },
            SkillGuard {
                guard_key: "strict".to_string(),
                policy: Ok(GuardPolicy {
                    denied_skill_keys: vec!["orders_get".to_string()],
                    ..GuardPolicy::default()
                }),
            },
            SkillGuard {
                guard_key: "permissive_again".to_string(),
                policy: Ok(GuardPolicy {
                    allowed_skill_keys: Some(vec!["orders_get".to_string()]),
                    ..GuardPolicy::default()
                }),
            },
        ];
        assert_eq!(
            evaluate_guards(&guards, "orders_get", GuardContext { caller_scopes: &[] }),
            GuardVerdict::Deny {
                guard_key: "strict".to_string(),
                reason: GuardDenialReason::SkillDenied,
            }
        );
    }
}
