//! Flow-execution engine and offline eval runner (issue #214, plan 12 §3 — the execution
//! half of the agent platform; the registries + CRUD are `application::agent_platform`).
//!
//! Both surfaces are **callers of the existing execution pipeline, never a parallel one**
//! (plan 12 §3). A flow step and an eval case each resolve one `agent_profiles` row and run
//! it through `MoiraExecutionService::execute_with_events`, so every step/case gets the same
//! routing, authorization, budgeting, retry/fallback, deadline and audit treatment as public
//! traffic — including the rig tool loop when the profile carries `skill_refs`. The only new
//! mechanism is `ExecutionCommand::agent_profile_hint`, which targets a profile directly
//! rather than through a route (see that field's doc comment).
//!
//! # Decisions this module implements (plans/12 §3)
//!
//! * **Flows are sequential-only** (decision 13): steps run in `step_order`, one at a time,
//!   the previous step's output feeding the next (unless the step's `input_mapping` says
//!   otherwise — see [`resolve_step_input`]).
//! * **Flows fail closed** (decision 15): the first step whose underlying execution does not
//!   succeed aborts the whole run — no later step runs, and the flow run is marked `failed`.
//!   Per-step `on_failure = 'continue'` is a stored column but is *not* honored in this MVP;
//!   it is reserved for the Growth stage, so every failure aborts.
//! * **Offline evals grade with `exact_match` / `contains` / `schema_valid` only**
//!   (decision 14): no LLM-judge. Grading is pure and cheap — see [`grade_case`].
//!
//! # Which route a step or case runs on
//!
//! **A flow step and an eval case carry no route hint, so they run on the deployment's default
//! route** — `RuntimeRepository::get_default_route`, which prefers `route_key = 'general'` (the
//! route `migrations/0005_provider_runtime.sql` seeds) and otherwise takes the oldest active
//! route. The step's agent profile overrides the *profile* (preamble, parameters,
//! `skill_refs`), never the route or the model: plan 12 §3 keeps model/route selection owned by
//! `route_definitions` and the existing routers, so "an agent does not pick its own model
//! outside that pipeline".
//!
//! The practical consequence is worth stating plainly, because it is a deployment
//! prerequisite rather than a code path: **if the default route has no routing policy, every
//! step and every case fails `no_eligible_model`** before the agent-profile hint is reached.
//! That is the correct fail-closed answer — a flow cannot invent a model — but it is a
//! configuration fault, so the operator's remedy is to wire the default route, not to change a
//! flow.
//!
//! Letting a step (or a flow) pin its own route is a deliberate follow-up, not an oversight:
//! `agent_flow_steps` has no route column, `route_hint` is scope-gated
//! (`moira:execution:override-route`), and per-step route pinning would move routing authority
//! into the flow registry — a product decision plan 12 §3 has not taken.
//!
//! # Posture: inline, not queued
//!
//! Both runs execute inline within the request (decision 24's inline posture), reusing each
//! underlying execution's own deadline/timeout controls rather than the request as a whole
//! carrying one. A large suite or a long flow therefore blocks its HTTP request for the sum
//! of its executions; moving either onto `RealJobDispatcher` (#244) with a `running` run id
//! returned immediately is a documented follow-up, deliberately not built here so the MVP
//! stays a thin, auditable caller of the existing pipeline.

use std::sync::Arc;

use axum::http::StatusCode;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    app::AppState,
    application::{MoiraExecutionService, RequestContext},
    domain::{
        AgentFlowRunResult, AuditLogInsert, AuditResult, CallerRuntimeIdentity, DomainMessage,
        EvalCaseRecord, EvalRunRecord, EvalRunRequest, EvalRunStatus, EvalTriggerKind,
        ExecutionCommand, ExecutionOptions, ExecutionOutcome, ExecutionStatus, FlowRunRequest,
        FlowRunStatus, FlowStepRunStatus, GradingKind, ResourceStatus,
    },
    error::AppError,
    infra::repositories::{
        AdminRepository, PgAdminRepository, PgAgentPlatformRepository, PgRuntimeRepository,
        RuntimeRepository,
    },
    security::Actor,
};

/// The execution half of the agent platform. Separate from
/// [`crate::application::AgentPlatformService`] (which owns the CRUD registries) so the
/// already-large admin service does not also carry the pipeline-calling logic.
pub struct FlowEvalExecutionService<'a> {
    state: &'a AppState,
    repo: PgAgentPlatformRepository,
    admin_repo: PgAdminRepository,
    runtime_repo: Arc<dyn RuntimeRepository>,
}

impl<'a> FlowEvalExecutionService<'a> {
    pub fn new(state: &'a AppState) -> Result<Self, AppError> {
        let pool = state.pool()?.clone();
        Ok(Self {
            state,
            repo: PgAgentPlatformRepository::new(pool.clone()),
            admin_repo: PgAdminRepository::new(pool.clone()),
            runtime_repo: Arc::new(PgRuntimeRepository::new(pool)),
        })
    }

    // =====================================================================================
    // Flows.
    // =====================================================================================

    /// Runs every step of a flow in `step_order`, fail-closed (decision 15), and persists one
    /// `agent_flow_runs` row plus one `agent_flow_step_runs` row per step attempted.
    pub async fn run_flow(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        flow_id: Uuid,
        request: FlowRunRequest,
    ) -> Result<AgentFlowRunResult, AppError> {
        self.state.authz.require(actor, "moira:flows:write")?;
        let flow = self.repo.get_flow(flow_id).await?;
        if flow.status != ResourceStatus::Active {
            return Err(flow_not_runnable("the flow is not active"));
        }
        if flow.steps.is_empty() {
            return Err(flow_not_runnable("the flow has no steps to run"));
        }

        let run_input = request
            .input
            .as_ref()
            .map(input_to_prompt)
            .unwrap_or_default();

        let execution = MoiraExecutionService::new(self.state.clone())?;
        let flow_run = self
            .repo
            .insert_flow_run(Uuid::now_v7(), flow_id, &json!({ "trigger": "admin_run" }))
            .await?;

        let mut previous_output: Option<String> = None;
        let mut final_status = FlowRunStatus::Completed;
        // `get_flow` returns steps already ordered by `step_order` (repository `fetch_flow_steps`).
        for step in &flow.steps {
            let prompt =
                resolve_step_input(&step.input_mapping, previous_output.as_deref(), &run_input);
            let step_run = self
                .repo
                .insert_flow_step_run(Uuid::now_v7(), flow_run.id, step.id)
                .await?;
            let command = self.build_command(
                actor,
                ctx,
                step.agent_profile_id,
                prompt,
                json!({ "flow_run_id": flow_run.id, "step_id": step.id }),
            );
            let execution_id = command.execution_id;
            let (outcome, _events) = execution.execute_with_events(command).await?;
            match outcome.status {
                ExecutionStatus::Succeeded => {
                    self.repo
                        .finalize_flow_step_run(
                            step_run.id,
                            FlowStepRunStatus::Completed,
                            Some(execution_id),
                            None,
                        )
                        .await?;
                    previous_output = Some(outcome_text(&outcome));
                }
                ExecutionStatus::Failed | ExecutionStatus::Cancelled => {
                    let summary = failure_summary(&outcome);
                    self.repo
                        .finalize_flow_step_run(
                            step_run.id,
                            FlowStepRunStatus::Failed,
                            Some(execution_id),
                            Some(&summary),
                        )
                        .await?;
                    // Fail-closed abort (decision 15): stop here, mark the run failed, run no
                    // further steps. `on_failure = 'continue'` is deferred to the Growth stage.
                    final_status = FlowRunStatus::Failed;
                    break;
                }
            }
        }

        let run = self
            .repo
            .finalize_flow_run(flow_run.id, final_status)
            .await?;
        let steps = self.repo.list_flow_step_runs(flow_run.id).await?;
        self.audit(
            actor,
            ctx,
            "flow.run",
            "flow_run",
            Some(run.id.to_string()),
            json!({ "flow_id": flow_id, "status": run.status, "step_count": steps.len() }),
        )
        .await?;
        Ok(AgentFlowRunResult { run, steps })
    }

    // =====================================================================================
    // Offline evals.
    // =====================================================================================

    /// Grades every case of a suite against a target agent profile and writes one terminal
    /// `eval_runs` row (`trigger_kind = 'offline_manual'`) with per-case pass/fail detail.
    pub async fn run_eval_suite(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        suite_id: Uuid,
        request: EvalRunRequest,
    ) -> Result<EvalRunRecord, AppError> {
        self.state.authz.require(actor, "moira:evals:write")?;
        let suite = self.repo.get_eval_suite(suite_id).await?;
        if suite.status != ResourceStatus::Active {
            return Err(eval_suite_not_runnable("the eval suite is not active"));
        }

        // Target precedence: the caller's explicit id, then the suite's own
        // `metadata.target_agent_profile_id`. Neither present is a hard refusal — an eval with
        // no subject cannot measure anything.
        let target = request
            .agent_profile_id
            .or_else(|| {
                suite
                    .metadata
                    .get("target_agent_profile_id")
                    .and_then(Value::as_str)
                    .and_then(|value| Uuid::parse_str(value).ok())
            })
            .ok_or_else(|| {
                eval_target_missing(
                    "no target agent profile: pass agent_profile_id or set \
                     metadata.target_agent_profile_id on the suite",
                )
            })?;
        // Fail-closed on a missing target, before any case runs.
        self.runtime_repo
            .get_agent_profile(target)
            .await
            .map_err(|_| eval_target_missing("the target agent profile does not exist"))?;

        let cases = self.repo.all_eval_cases(suite_id).await?;
        if cases.is_empty() {
            return Err(eval_suite_not_runnable("the eval suite has no cases"));
        }

        let execution = MoiraExecutionService::new(self.state.clone())?;
        let mut passed = 0usize;
        let mut results = Vec::with_capacity(cases.len());
        for case in &cases {
            let prompt = input_to_prompt(&case.input);
            let command = self.build_command(
                actor,
                ctx,
                target,
                prompt,
                json!({ "eval_suite_id": suite_id, "eval_case_id": case.id }),
            );
            let (outcome, _events) = execution.execute_with_events(command).await?;
            let grade = grade_case(case, &outcome);
            if grade.passed {
                passed += 1;
            }
            results.push(json!({
                "case_id": case.id,
                "grading_kind": case.grading_kind,
                "passed": grade.passed,
                "reason": grade.reason,
            }));
        }

        let total = cases.len();
        // `total >= 1` here (empty was refused above), so the division is always defined.
        let score = passed as f64 / total as f64;
        let run = self
            .repo
            .insert_eval_run(
                Uuid::now_v7(),
                suite_id,
                target,
                EvalTriggerKind::OfflineManual,
                EvalRunStatus::Completed,
                Some(score),
                &json!({ "cases": results, "passed": passed, "total": total }),
                &json!({ "trigger": "admin_run" }),
            )
            .await?;
        self.audit(
            actor,
            ctx,
            "eval_suite.run",
            "eval_run",
            Some(run.id.to_string()),
            json!({ "suite_id": suite_id, "agent_profile_id": target, "passed": passed, "total": total }),
        )
        .await?;
        Ok(run)
    }

    /// Builds an [`ExecutionCommand`] that targets `agent_profile_id` directly (via
    /// `agent_profile_hint`) with a single user message. No route/provider/model/credential
    /// hint is set, so model selection stays route-owned (the default route) and no override
    /// scope is required of the admin caller — see the field's doc comment.
    fn build_command(
        &self,
        actor: &Actor,
        ctx: &RequestContext,
        agent_profile_id: Uuid,
        prompt: String,
        metadata: Value,
    ) -> ExecutionCommand {
        ExecutionCommand {
            request_id: ctx.request_id.clone(),
            execution_id: Uuid::now_v7(),
            identity: caller_identity(actor),
            application_id: actor.internal_application_id,
            external_tenant_id: actor.external_tenant_id.clone(),
            external_user_id: actor
                .external_user_id
                .clone()
                .or_else(|| actor.subject.clone()),
            messages: vec![DomainMessage::user(prompt)],
            route_hint: None,
            provider_hint: None,
            model_hint: None,
            credential_hint: None,
            agent_profile_hint: Some(agent_profile_id),
            options: ExecutionOptions::default(),
            metadata,
        }
    }

    async fn audit(
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
}

fn caller_identity(actor: &Actor) -> CallerRuntimeIdentity {
    CallerRuntimeIdentity {
        actor_type: format!("{:?}", actor.actor_type),
        subject: actor.subject.clone(),
        external_user_id: actor.external_user_id.clone(),
        external_tenant_id: actor.external_tenant_id.clone(),
        application_id: actor.internal_application_id,
        scopes: actor.scopes.clone(),
    }
}

fn flow_not_runnable(detail: &str) -> AppError {
    AppError::coded(
        StatusCode::UNPROCESSABLE_ENTITY,
        "flow_not_runnable",
        format!("this flow cannot be run: {detail}"),
    )
}

fn eval_suite_not_runnable(detail: &str) -> AppError {
    AppError::coded(
        StatusCode::UNPROCESSABLE_ENTITY,
        "eval_suite_not_runnable",
        format!("this eval suite cannot be run: {detail}"),
    )
}

fn eval_target_missing(detail: &str) -> AppError {
    AppError::coded(
        StatusCode::UNPROCESSABLE_ENTITY,
        "eval_target_missing",
        format!("this eval run has no target agent profile: {detail}"),
    )
}

/// The text an execution produced, for chaining into the next flow step: the model's text
/// answer, or the compact JSON of a structured answer when there is no text.
fn outcome_text(outcome: &ExecutionOutcome) -> String {
    if let Some(text) = &outcome.output_text {
        return text.clone();
    }
    match &outcome.structured_output {
        Some(value) => value.to_string(),
        None => String::new(),
    }
}

/// A sanitized, privacy-safe summary of a failed step's underlying execution, stored in the
/// step run's `error_summary`. Carries the failure class token (already snake_case, no
/// provider body, no prompt) and nothing model- or target-authored; the console renders the
/// generic `moira.error.flow_step_failed` message alongside it.
fn failure_summary(outcome: &ExecutionOutcome) -> String {
    match &outcome.failure {
        Some(failure) => {
            let class = serde_json::to_value(failure.class)
                .ok()
                .and_then(|value| value.as_str().map(str::to_string))
                .unwrap_or_else(|| "unknown".to_string());
            format!("flow step failed ({class})")
        }
        None => "flow step failed".to_string(),
    }
}

/// Turns an eval-case `input` (or a flow's run input) into a single prompt string.
///
/// A bare JSON string is the prompt; an object with a string `prompt` field uses that; any
/// other shape is carried as its compact JSON, which is at least round-trippable rather than
/// `[object Object]`-shaped. Documented deliberately narrow for the MVP — richer
/// message-array inputs are a follow-up.
fn input_to_prompt(input: &Value) -> String {
    match input {
        Value::String(text) => text.clone(),
        Value::Object(map) => map
            .get("prompt")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| input.to_string()),
        other => other.to_string(),
    }
}

/// How a flow step's input is chosen (plan 12 §3 — the documented passing convention).
///
/// The default (an empty `input_mapping`, or one this MVP does not recognise) is *the
/// previous step's output becomes this step's input*, falling back to the flow's run input
/// for the first step. A step overrides that with `input_mapping`:
///
/// * `{ "literal": "..." }` — a fixed prompt, ignoring prior output.
/// * `{ "source": "run_input" }` — always the flow's run input.
/// * `{ "source": "previous" }` — the previous step's output (the default, made explicit).
///
/// Richer templating (interpolating fields of a structured prior output) is deferred.
fn resolve_step_input(
    input_mapping: &Value,
    previous_output: Option<&str>,
    run_input: &str,
) -> String {
    if let Some(map) = input_mapping.as_object() {
        if let Some(literal) = map.get("literal").and_then(Value::as_str) {
            return literal.to_string();
        }
        if let Some(source) = map.get("source").and_then(Value::as_str) {
            match source {
                "run_input" => return run_input.to_string(),
                "previous" => return previous_output.unwrap_or(run_input).to_string(),
                _ => {}
            }
        }
    }
    previous_output.unwrap_or(run_input).to_string()
}

/// One case's grade, plus a stable machine-readable reason token for the run's `results`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CaseGrade {
    passed: bool,
    /// `pass` | `mismatch` | `no_output` | `not_json` | `schema_violation` | `execution_failed`.
    reason: &'static str,
}

impl CaseGrade {
    const fn pass() -> Self {
        Self {
            passed: true,
            reason: "pass",
        }
    }
    const fn fail(reason: &'static str) -> Self {
        Self {
            passed: false,
            reason,
        }
    }
}

/// Grades one case against its execution outcome (decision 14 — exact_match/contains/
/// schema_valid only). An execution that did not succeed fails the case with
/// `execution_failed` rather than being graded against absent output.
fn grade_case(case: &EvalCaseRecord, outcome: &ExecutionOutcome) -> CaseGrade {
    if outcome.status != ExecutionStatus::Succeeded {
        return CaseGrade::fail("execution_failed");
    }
    let output = outcome.output_text.as_deref();
    match case.grading_kind {
        GradingKind::ExactMatch => grade_exact_match(&case.expected, output),
        GradingKind::Contains => grade_contains(&case.expected, output),
        GradingKind::SchemaValid => {
            grade_schema_valid(&case.expected, outcome.structured_output.as_ref(), output)
        }
    }
}

/// `expected` compared exactly to the trimmed output. A string `expected` compares by its
/// content; any other JSON compares by its compact serialization.
fn grade_exact_match(expected: &Value, output: Option<&str>) -> CaseGrade {
    let Some(output) = output else {
        return CaseGrade::fail("no_output");
    };
    if output.trim() == expected_to_string(expected).trim() {
        CaseGrade::pass()
    } else {
        CaseGrade::fail("mismatch")
    }
}

/// `expected` (as a string) must be a substring of the output.
fn grade_contains(expected: &Value, output: Option<&str>) -> CaseGrade {
    let Some(output) = output else {
        return CaseGrade::fail("no_output");
    };
    if output.contains(&expected_to_string(expected)) {
        CaseGrade::pass()
    } else {
        CaseGrade::fail("mismatch")
    }
}

/// The output must be JSON (a structured output, or text that parses as JSON) and satisfy the
/// schema in `expected`.
///
/// **MVP structural validator, not a full JSON Schema implementation.** It checks `type`,
/// object `required`, and recurses one level into declared `properties`; it does not handle
/// `$ref`, `oneOf`/`anyOf`/`allOf`, `format`, numeric bounds, array item schemas, or
/// `additionalProperties`. This matches decision 14's "cheap and MVP-safe" framing and the
/// structural checks `orchestration::skill_tool` already performs on tool arguments.
fn grade_schema_valid(
    schema: &Value,
    structured: Option<&Value>,
    output: Option<&str>,
) -> CaseGrade {
    let value = match structured {
        Some(value) => value.clone(),
        None => {
            let Some(output) = output else {
                return CaseGrade::fail("no_output");
            };
            match serde_json::from_str::<Value>(output.trim()) {
                Ok(value) => value,
                Err(_) => return CaseGrade::fail("not_json"),
            }
        }
    };
    if schema_structurally_valid(&value, schema) {
        CaseGrade::pass()
    } else {
        CaseGrade::fail("schema_violation")
    }
}

fn expected_to_string(expected: &Value) -> String {
    match expected {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn schema_structurally_valid(value: &Value, schema: &Value) -> bool {
    let Some(schema) = schema.as_object() else {
        // A non-object schema constrains nothing in this MVP validator.
        return true;
    };
    if let Some(expected_type) = schema.get("type").and_then(Value::as_str)
        && !json_matches_type(value, expected_type)
    {
        return false;
    }
    if let Some(required) = schema.get("required").and_then(Value::as_array) {
        let Some(object) = value.as_object() else {
            return false;
        };
        for name in required {
            if let Some(name) = name.as_str()
                && !object.contains_key(name)
            {
                return false;
            }
        }
    }
    if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
        let Some(object) = value.as_object() else {
            return false;
        };
        for (name, subschema) in properties {
            if let Some(subvalue) = object.get(name)
                && !schema_structurally_valid(subvalue, subschema)
            {
                return false;
            }
        }
    }
    true
}

fn json_matches_type(value: &Value, expected_type: &str) -> bool {
    match expected_type {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        "number" => value.is_number(),
        "integer" => value.is_i64() || value.is_u64(),
        // An unknown type keyword constrains nothing rather than failing every value.
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::UsageSummary;

    fn outcome(
        status: ExecutionStatus,
        text: Option<&str>,
        structured: Option<Value>,
    ) -> ExecutionOutcome {
        ExecutionOutcome {
            request_id: "req".to_string(),
            execution_id: Uuid::now_v7(),
            status,
            output_text: text.map(str::to_string),
            structured_output: structured,
            usage: UsageSummary::default(),
            route: None,
            model: None,
            attempts: Vec::new(),
            failure: None,
        }
    }

    fn case(grading_kind: GradingKind, expected: Value) -> EvalCaseRecord {
        EvalCaseRecord {
            id: Uuid::now_v7(),
            suite_id: Uuid::now_v7(),
            input: json!("hello"),
            expected,
            grading_kind,
            metadata: json!({}),
            created_at: chrono::Utc::now(),
        }
    }

    // ---- Step input passing (plan 12 §3 convention) ---------------------------------------

    #[test]
    fn the_first_step_reads_the_run_input_and_later_steps_read_the_previous_output() {
        // Default mapping, first step: previous_output is None -> run input.
        assert_eq!(resolve_step_input(&json!({}), None, "seed"), "seed");
        // Default mapping, later step: the previous step's output.
        assert_eq!(
            resolve_step_input(&json!({}), Some("from step 1"), "seed"),
            "from step 1"
        );
    }

    #[test]
    fn a_step_can_override_the_default_passing_with_its_input_mapping() {
        assert_eq!(
            resolve_step_input(&json!({ "literal": "fixed" }), Some("prev"), "seed"),
            "fixed"
        );
        assert_eq!(
            resolve_step_input(&json!({ "source": "run_input" }), Some("prev"), "seed"),
            "seed"
        );
        assert_eq!(
            resolve_step_input(&json!({ "source": "previous" }), Some("prev"), "seed"),
            "prev"
        );
        // An unrecognised source falls back to the default (previous, then run input).
        assert_eq!(
            resolve_step_input(&json!({ "source": "who-knows" }), None, "seed"),
            "seed"
        );
    }

    #[test]
    fn input_to_prompt_reads_string_and_prompt_object_and_falls_back_to_json() {
        assert_eq!(input_to_prompt(&json!("hi")), "hi");
        assert_eq!(input_to_prompt(&json!({ "prompt": "do it" })), "do it");
        assert_eq!(input_to_prompt(&json!({ "x": 1 })), "{\"x\":1}");
    }

    // ---- Grading (decision 14) ------------------------------------------------------------

    #[test]
    fn exact_match_grades_on_the_trimmed_string() {
        assert!(grade_exact_match(&json!("shipped"), Some("  shipped \n")).passed);
        assert!(!grade_exact_match(&json!("shipped"), Some("not shipped")).passed);
        assert_eq!(grade_exact_match(&json!("x"), None).reason, "no_output");
    }

    #[test]
    fn contains_grades_on_substring() {
        assert!(grade_contains(&json!("order"), Some("your order A-1 is shipped")).passed);
        assert!(!grade_contains(&json!("refund"), Some("your order A-1 is shipped")).passed);
    }

    #[test]
    fn schema_valid_accepts_a_conforming_object_and_rejects_a_missing_required_field() {
        let schema = json!({
            "type": "object",
            "properties": { "state": { "type": "string" } },
            "required": ["state"]
        });
        // From a structured output.
        assert!(grade_schema_valid(&schema, Some(&json!({ "state": "shipped" })), None).passed);
        // From JSON text.
        assert!(grade_schema_valid(&schema, None, Some("{\"state\":\"shipped\"}")).passed);
        // Missing the required field.
        assert_eq!(
            grade_schema_valid(&schema, Some(&json!({ "other": 1 })), None).reason,
            "schema_violation"
        );
        // Not JSON at all.
        assert_eq!(
            grade_schema_valid(&schema, None, Some("plain prose")).reason,
            "not_json"
        );
    }

    #[test]
    fn a_wrong_top_level_type_fails_schema_validation() {
        let schema = json!({ "type": "object" });
        assert!(!schema_structurally_valid(&json!(["a", "b"]), &schema));
        assert!(schema_structurally_valid(&json!({}), &schema));
    }

    // ---- Fail-closed grade of a failed execution ------------------------------------------

    #[test]
    fn a_failed_execution_fails_the_case_regardless_of_grading_kind() {
        let failed = outcome(ExecutionStatus::Failed, None, None);
        for kind in [
            GradingKind::ExactMatch,
            GradingKind::Contains,
            GradingKind::SchemaValid,
        ] {
            let grade = grade_case(&case(kind, json!("anything")), &failed);
            assert!(!grade.passed);
            assert_eq!(grade.reason, "execution_failed");
        }
    }

    #[test]
    fn a_succeeded_execution_is_graded_against_its_output() {
        let ok = outcome(ExecutionStatus::Succeeded, Some("shipped"), None);
        assert!(grade_case(&case(GradingKind::ExactMatch, json!("shipped")), &ok).passed);
        assert!(grade_case(&case(GradingKind::Contains, json!("ship")), &ok).passed);
    }

    #[test]
    fn outcome_text_prefers_text_then_structured_then_empty() {
        assert_eq!(
            outcome_text(&outcome(ExecutionStatus::Succeeded, Some("t"), None)),
            "t"
        );
        assert_eq!(
            outcome_text(&outcome(
                ExecutionStatus::Succeeded,
                None,
                Some(json!({ "a": 1 }))
            )),
            "{\"a\":1}"
        );
        assert_eq!(
            outcome_text(&outcome(ExecutionStatus::Succeeded, None, None)),
            ""
        );
    }
}
