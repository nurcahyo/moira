//! End-to-end coverage for the flow-execution engine and the offline eval runner (issue #214,
//! plan 12 §3 — the execution half of the agent platform).
//!
//! Everything runs against a real Postgres and the scripted OpenAI-compatible provider
//! ([`mock_openai::MockOpenAiServer`]) — owner policy is mocks only, no real provider and no
//! real tokens. Each flow step and each eval case is a full pipeline execution
//! (`MoiraExecutionService::execute_with_events`) driven through `agent_profile_hint`, so what
//! these tests assert is the *persisted* result: the `agent_flow_runs`/`agent_flow_step_runs`
//! rows a flow leaves behind, the `eval_runs` row a suite leaves behind, and the fail-closed
//! abort (decision 15) — not a restatement of the code that produced them.
//!
//! The provider is scripted FIFO: one [`ProviderScript::Completion`] is consumed per model
//! call, in the order the runner issues executions. A flow of N successful steps needs N
//! scripts; a suite of N cases needs N. The fail-closed test deliberately provides fewer,
//! because the aborted step and every step after it must never reach the provider.
//!
//! Skips (never fails) when no test database is configured, per CONVENTIONS §3.

mod support;

use moira::{
    application::{AgentPlatformService, FlowEvalExecutionService, RuntimeAdminService},
    domain::{
        AgentFlowCreateRequest, AgentFlowStepCreateRequest, AgentProfileCreateRequest,
        EvalCaseCreateRequest, EvalRunRequest, EvalRunStatus, EvalSuiteCreateRequest,
        FlowRunRequest, FlowRunStatus, FlowStepOnFailure, FlowStepRunStatus, GradingKind,
        RoutingPolicyCreateRequest,
    },
    security::Actor,
};
use serde_json::json;
use uuid::Uuid;

use support::{
    LifecycleFixture, ProviderFixture, RuntimePolicy,
    mock_openai::{MockOpenAiServer, ProviderScript},
    request_context,
};

/// A flow step or an eval case runs with the command's `application_id` bound to the fixture's
/// application, because the fixture's routing policy is scoped to that application — a run
/// under a different (or absent) application would find no model candidate and fail at
/// routing before the agent-profile hint ever mattered. The runner derives the command's
/// `application_id` from the actor, so the actor must carry it.
fn app_bound_actor(fixture: &LifecycleFixture) -> Actor {
    let mut actor = fixture.actor.clone();
    actor.internal_application_id = Some(fixture.application_id);
    actor
}

/// Wires this fixture's provider onto the **deployment's default route**, which is what a flow
/// step and an eval case actually execute on.
///
/// Neither runner sets a `route_hint` (see `application::flow_eval_execution` — model and route
/// selection stay route-owned, plan 12 §3), so the pipeline resolves the route through
/// `RuntimeRepository::get_default_route`. That query prefers `route_key = 'general'` and
/// otherwise takes the oldest active route — it does **not** read any `metadata.default` flag —
/// and `migrations/0005_provider_runtime.sql` seeds exactly such a `general` route into every
/// database. So the default route is always the seeded `general` one, never
/// `LifecycleFixture`'s own bespoke `route_{suffix}`.
///
/// `LifecycleFixture::add_provider` attaches its routing policy to that bespoke route, which is
/// right for suites that drive executions with an explicit `route:` hint (`fixture.command()`
/// passes one) but leaves the default route with no candidates. Without this helper every step
/// and case fails `no_eligible_model` before the agent-profile hint is ever reached — a fixture
/// artifact, not a defect in the engine.
///
/// The second policy is added rather than the fixture's being moved, and the seeded route is
/// left enabled rather than disabled, because that is the shape of a real deployment: the
/// default route is wired, and an operator's extra named routes coexist with it. Kept local to
/// this file on purpose — `tests/support/mod.rs` is shared and other suites depend on the
/// default route having no candidates.
async fn wire_default_route(fixture: &LifecycleFixture, provider: &ProviderFixture) {
    let default_route_id: Uuid = sqlx::query_scalar(
        "select id from route_definitions where route_key = 'general' \
         and status = 'active' and deleted_at is null",
    )
    .fetch_one(&fixture.pool)
    .await
    .expect("the seeded 'general' route exists (migrations/0005_provider_runtime.sql)");

    RuntimeAdminService::new(&fixture.state)
        .expect("runtime admin service")
        .create_routing_policy(
            &fixture.actor,
            &request_context(),
            RoutingPolicyCreateRequest {
                application_id: Some(fixture.application_id),
                external_tenant_id: None,
                route_id: default_route_id,
                provider_id: provider.provider_id,
                provider_model_id: provider.model_id,
                priority: 10,
                weight: 1,
                cost_weight: 0.0,
                latency_weight: 0.0,
                quality_weight: 0.0,
                privacy_class: None,
                required_capabilities: Vec::new(),
                maximum_cost_per_request: None,
                maximum_input_tokens: None,
                maximum_output_tokens: None,
                timeout_ms: Some(5_000),
                retry_policy: json!({}),
                metadata: json!({ "test_fixture": true }),
            },
        )
        .await
        .expect("wire the default route to this fixture's provider");
}

/// Creates a plain agent profile (no skills) and returns its id.
async fn create_profile(fixture: &LifecycleFixture, key_hint: &str) -> Uuid {
    let suffix = Uuid::now_v7().simple().to_string();
    RuntimeAdminService::new(&fixture.state)
        .expect("runtime admin service")
        .create_agent_profile(
            &fixture.actor,
            &request_context(),
            AgentProfileCreateRequest {
                profile_key: format!("{key_hint}-{suffix}"),
                display_name: format!("Profile {key_hint}"),
                preamble: Some("answer briefly".to_string()),
                temperature: None,
                max_tokens: None,
                tool_policy: json!({}),
                context_policy: json!({}),
                memory_policy: json!({}),
                metadata: json!({ "test_fixture": true }),
            },
        )
        .await
        .expect("create agent profile")
        .id
}

/// The whole happy path: a two-step flow, each step one full execution, the first step's
/// output chained into the second step's prompt (the default input-passing convention).
#[tokio::test]
async fn a_flow_runs_its_steps_in_order_and_chains_output() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let provider = MockOpenAiServer::start(vec![
        ProviderScript::Completion {
            text: "output one".to_string(),
        },
        ProviderScript::Completion {
            text: "final two".to_string(),
        },
    ])
    .await;
    let wiring = fixture
        .add_provider(provider.base_url(), 10, RuntimePolicy::default())
        .await;
    wire_default_route(&fixture, &wiring).await;

    let step_a = create_profile(&fixture, "flow-a").await;
    let step_b = create_profile(&fixture, "flow-b").await;
    let actor = app_bound_actor(&fixture);
    let flow = AgentPlatformService::new(&fixture.state)
        .expect("agent platform service")
        .create_flow(
            &actor,
            &request_context(),
            AgentFlowCreateRequest {
                flow_key: format!("flow-{}", Uuid::now_v7().simple()),
                display_name: "Two-step flow".to_string(),
                description: None,
                metadata: json!({}),
                steps: vec![
                    AgentFlowStepCreateRequest {
                        step_key: "first".to_string(),
                        step_order: 0,
                        agent_profile_id: step_a,
                        on_failure: FlowStepOnFailure::Abort,
                        input_mapping: json!({}),
                        metadata: json!({}),
                    },
                    AgentFlowStepCreateRequest {
                        step_key: "second".to_string(),
                        step_order: 1,
                        agent_profile_id: step_b,
                        on_failure: FlowStepOnFailure::Abort,
                        input_mapping: json!({}),
                        metadata: json!({}),
                    },
                ],
            },
        )
        .await
        .expect("create flow");

    let result = FlowEvalExecutionService::new(&fixture.state)
        .expect("flow eval service")
        .run_flow(
            &actor,
            &request_context(),
            flow.id,
            FlowRunRequest {
                input: Some(json!("seed")),
            },
        )
        .await
        .expect("run flow");

    assert_eq!(result.run.status, FlowRunStatus::Completed);
    assert!(result.run.completed_at.is_some());
    assert_eq!(result.steps.len(), 2, "one step-run row per step");
    for step in &result.steps {
        assert_eq!(step.status, FlowStepRunStatus::Completed);
        assert!(
            step.execution_id.is_some(),
            "a completed step correlates to its pipeline execution"
        );
        assert!(step.error_summary.is_none());
    }

    // The persisted rows exist and are reachable through the admin read surface.
    let runs = AgentPlatformService::new(&fixture.state)
        .expect("agent platform service")
        .list_flow_runs(&actor, flow.id, None, 50)
        .await
        .expect("list flow runs");
    assert_eq!(runs.data.len(), 1);
    assert_eq!(runs.data[0].id, result.run.id);

    // Input passing: the first request carries the run input, the second carries step one's
    // output — the default convention (previous step's text becomes the next step's input).
    let requests = provider.requests().await;
    assert_eq!(requests.len(), 2, "exactly one model call per step");
    assert!(
        request_mentions(&requests[0].body, "seed"),
        "step one runs with the flow's run input"
    );
    assert!(
        request_mentions(&requests[1].body, "output one"),
        "step two runs with step one's output"
    );

    provider.shutdown().await;
}

/// Fail-closed abort (decision 15): the first failing step stops the run, marks it failed, and
/// no later step runs — proved by the absence of a third step-run row and the provider never
/// being called for the step after the failure.
#[tokio::test]
async fn a_failing_step_aborts_the_flow_and_no_later_step_runs() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    // Only the first step reaches the provider: the second fails fail-closed (a dangling skill
    // ref) before any model call, and the third must never run at all.
    let provider = MockOpenAiServer::start(vec![ProviderScript::Completion {
        text: "output one".to_string(),
    }])
    .await;
    let wiring = fixture
        .add_provider(provider.base_url(), 10, RuntimePolicy::default())
        .await;
    wire_default_route(&fixture, &wiring).await;

    let ok_first = create_profile(&fixture, "abort-a").await;
    let failing = create_profile(&fixture, "abort-b").await;
    let never_runs = create_profile(&fixture, "abort-c").await;
    // Point the failing profile at a skill that does not exist: the execution refuses
    // fail-closed with SkillUnavailable before any provider call (issue #84 posture).
    sqlx::query("update agent_profiles set skill_refs = array[$1::uuid] where id = $2")
        .bind(Uuid::now_v7())
        .bind(failing)
        .execute(&fixture.pool)
        .await
        .expect("attach a dangling skill ref");

    let actor = app_bound_actor(&fixture);
    let flow = AgentPlatformService::new(&fixture.state)
        .expect("agent platform service")
        .create_flow(
            &actor,
            &request_context(),
            AgentFlowCreateRequest {
                flow_key: format!("flow-{}", Uuid::now_v7().simple()),
                display_name: "Aborting flow".to_string(),
                description: None,
                metadata: json!({}),
                steps: vec![
                    step(0, "s0", ok_first),
                    step(1, "s1", failing),
                    step(2, "s2", never_runs),
                ],
            },
        )
        .await
        .expect("create flow");

    let result = FlowEvalExecutionService::new(&fixture.state)
        .expect("flow eval service")
        .run_flow(
            &actor,
            &request_context(),
            flow.id,
            FlowRunRequest::default(),
        )
        .await
        .expect("run flow");

    assert_eq!(result.run.status, FlowRunStatus::Failed);
    assert_eq!(
        result.steps.len(),
        2,
        "the run aborted after the failing step; the third step never produced a row"
    );
    assert_eq!(result.steps[0].status, FlowStepRunStatus::Completed);
    assert_eq!(result.steps[1].status, FlowStepRunStatus::Failed);
    assert!(
        result.steps[1].error_summary.is_some(),
        "the aborting step records a sanitized failure summary"
    );
    assert_eq!(
        provider.call_count().await,
        1,
        "the step after the failure never reached the provider"
    );

    provider.shutdown().await;
}

/// The offline eval runner grades each case against a target agent profile and scores the
/// suite by pass rate (decision 14 — exact_match / contains / schema_valid only).
#[tokio::test]
async fn an_offline_eval_run_grades_each_case_and_scores_the_suite() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    // Scripts align with the case creation order (`all_eval_cases` is created_at asc):
    //   pong        -> case 1 exact_match "pong"          -> pass
    //   world order -> case 2 contains "or"               -> pass
    //   no          -> case 3 exact_match "yes"           -> fail
    //   {"state":…} -> case 4 schema_valid {required:[…]} -> pass
    let provider = MockOpenAiServer::start(vec![
        ProviderScript::Completion {
            text: "pong".to_string(),
        },
        ProviderScript::Completion {
            text: "world order".to_string(),
        },
        ProviderScript::Completion {
            text: "no".to_string(),
        },
        ProviderScript::Completion {
            text: "{\"state\":\"shipped\"}".to_string(),
        },
    ])
    .await;
    let wiring = fixture
        .add_provider(provider.base_url(), 10, RuntimePolicy::default())
        .await;
    wire_default_route(&fixture, &wiring).await;

    let target = create_profile(&fixture, "eval-target").await;
    let actor = app_bound_actor(&fixture);
    let service = AgentPlatformService::new(&fixture.state).expect("agent platform service");
    let suite = service
        .create_eval_suite(
            &actor,
            &request_context(),
            EvalSuiteCreateRequest {
                suite_key: format!("suite-{}", Uuid::now_v7().simple()),
                display_name: "Grading suite".to_string(),
                description: None,
                metadata: json!({}),
            },
        )
        .await
        .expect("create eval suite");
    for (input, expected, kind) in [
        (json!("ping"), json!("pong"), GradingKind::ExactMatch),
        (json!("ping"), json!("or"), GradingKind::Contains),
        (json!("ping"), json!("yes"), GradingKind::ExactMatch),
        (
            json!("give me json"),
            json!({ "type": "object", "required": ["state"] }),
            GradingKind::SchemaValid,
        ),
    ] {
        service
            .create_eval_case(
                &actor,
                &request_context(),
                suite.id,
                EvalCaseCreateRequest {
                    input,
                    expected,
                    grading_kind: kind,
                    metadata: json!({}),
                },
            )
            .await
            .expect("create eval case");
    }

    let run = FlowEvalExecutionService::new(&fixture.state)
        .expect("flow eval service")
        .run_eval_suite(
            &actor,
            &request_context(),
            suite.id,
            EvalRunRequest {
                agent_profile_id: Some(target),
            },
        )
        .await
        .expect("run eval suite");

    assert_eq!(run.status, EvalRunStatus::Completed);
    assert_eq!(run.agent_profile_id, Some(target));
    let results = run
        .results
        .get("cases")
        .and_then(|c| c.as_array())
        .expect("cases array");
    assert_eq!(results.len(), 4);
    assert_eq!(run.results["passed"], json!(3));
    assert_eq!(run.results["total"], json!(4));
    let score = run.score.expect("a score");
    assert!((score - 0.75).abs() < 1e-9, "3 of 4 cases pass: {score}");

    // The run is persisted and reachable through the admin read surface.
    let runs = service
        .list_eval_runs(&actor, suite.id, None, 50)
        .await
        .expect("list eval runs");
    assert_eq!(runs.data.len(), 1);
    assert_eq!(runs.data[0].id, run.id);

    provider.shutdown().await;
}

/// An eval run with no resolvable target is refused fail-closed — an eval measures a subject.
#[tokio::test]
async fn an_eval_run_without_a_target_is_refused() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let actor = app_bound_actor(&fixture);
    let suite = AgentPlatformService::new(&fixture.state)
        .expect("agent platform service")
        .create_eval_suite(
            &actor,
            &request_context(),
            EvalSuiteCreateRequest {
                suite_key: format!("suite-{}", Uuid::now_v7().simple()),
                display_name: "Targetless suite".to_string(),
                description: None,
                metadata: json!({}),
            },
        )
        .await
        .expect("create eval suite");

    let error = FlowEvalExecutionService::new(&fixture.state)
        .expect("flow eval service")
        .run_eval_suite(
            &actor,
            &request_context(),
            suite.id,
            EvalRunRequest {
                agent_profile_id: None,
            },
        )
        .await
        .expect_err("a run with no target must be refused");
    assert_eq!(error.error_response(None).error.code, "eval_target_missing");
}

fn step(order: i32, key: &str, agent_profile_id: Uuid) -> AgentFlowStepCreateRequest {
    AgentFlowStepCreateRequest {
        step_key: key.to_string(),
        step_order: order,
        agent_profile_id,
        on_failure: FlowStepOnFailure::Abort,
        input_mapping: json!({}),
        metadata: json!({}),
    }
}

/// Whether a scripted provider request's chat body mentions `needle` anywhere in its messages.
fn request_mentions(body: &serde_json::Value, needle: &str) -> bool {
    body.get("messages")
        .and_then(|m| m.as_array())
        .map(|messages| {
            messages
                .iter()
                .any(|message| message.to_string().contains(needle))
        })
        .unwrap_or(false)
}
