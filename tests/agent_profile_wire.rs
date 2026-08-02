//! Finding F49 — the agent-profile branch of `build_completion_request`, observed at the wire.
//!
//! # The hole this suite closes
//!
//! `build_completion_request` (`src/application/execution.rs`) reads three fields off an
//! `AgentProfileRecord`: `preamble`, `temperature` and `max_tokens`. Until this file existed,
//! **no integration test in the tree had ever built a request from an agent profile at all.**
//! Every route definition a fixture creates passes `agent_profile_id: None`, the raw-SQL
//! inserts in `tests/public_authorization.rs` omit the column, and the seeded `general` route
//! in `migrations/0005_provider_runtime.sql` omits it too — so `route.agent_profile_id` was
//! `NULL` on every end-to-end path, `agent_profile` was `None` at every call site, and the
//! entire branch was dead in the test suite while being live in production.
//!
//! Note the column's real home: it is `route_definitions.agent_profile_id`, not
//! `routing_policies.agent_profile_id` as F48's doc comment and the ledger's F49 entry both
//! say. `RoutingPolicyRecord` has no such field. The substance of the finding was right; the
//! table name was not.
//!
//! # Why the diagnostic endpoint
//!
//! `POST /api/v1/admin/runtime/diagnose` passes `ExecutionOptions` through verbatim, which is
//! what makes the *precedence* case below expressible: the public plane derives those options
//! from its own DTO, so a public-plane test could not set `options.temperature` to a value
//! distinct from the profile's. Same reasoning as `tests/structured_output.rs`, which drives
//! the same endpoint for the same reason.
//!
//! # What each case is for, and what it is discriminating against
//!
//! 1. **The branch reaches the wire.** A profile is attached to the route, and the body that
//!    actually arrived at the mock provider carries the preamble as a `system` message, the
//!    profile's `temperature`, and the profile's `max_tokens`.
//! 2. **The control, and it is load-bearing.** The same fixture with **no** profile attached
//!    must send no system message, no `temperature` and no `max_tokens`. Without this case,
//!    hardcoding `temperature: Some(0.37)` into `build_completion_request` would satisfy case 1
//!    — the profile would be provably irrelevant and case 1 would not notice. It is also the
//!    executable statement of F49's premise: this is what every other fixture in the tree
//!    sends.
//! 3. **Precedence, in the direction the code chose.** In case 1 `options.temperature` is
//!    `None`, which makes `options.or_else(profile)` and `profile.or(options)` produce the same
//!    wire body — the two are indistinguishable there by construction (HANDOFF §3.4, seventh
//!    entry). Case 3 sets both to different values so the order is observable.
//! 4. **The streaming path.** Both arms share the one `build_completion_request` call today,
//!    but they are separate arms; a rebuild inside the stream branch would be invisible to
//!    cases 1–3. `"stream": true` is asserted on the same body so the case cannot pass by
//!    silently having taken the non-streaming path.
//! 5. **`tool_policy` is not forwarded.** The integration counterpart of F48's unit guard —
//!    see that case's own comment for why it is *not* a substitute for it.
//! 6. **Finding F50, and this case DOCUMENTS CURRENT BEHAVIOUR rather than guarding it.**
//!    Disabling an agent profile leaves the route pointing at it and silently degrades every
//!    execution on that route to a profile-less request. Read that case's comment before
//!    changing it.

mod support;

use std::{sync::Arc, time::Duration};

use axum::http::StatusCode;
use moira::{
    application::RuntimeAdminService,
    domain::{
        AgentProfileCreateRequest, AgentProfileRecord, DiagnosticExecutionRequest,
        ExecutionOptions, RouteDefinitionPatchRequest,
    },
};
use serde_json::{Value, json};
use support::{
    LifecycleFixture, MoiraHttpServer, RuntimePolicy,
    mock_openai::{MockOpenAiServer, ProviderScript},
    request_context,
};
use uuid::Uuid;

const WAIT: Duration = Duration::from_secs(15);

/// Deliberately unlike anything else the request could carry, so an assertion that finds it can
/// only have found the profile's copy of it.
const PROFILE_PREAMBLE: &str = "F49: this preamble exists only on the agent profile.";

/// `0.37` and `271` are chosen to be values nothing else in the system produces: no default,
/// no clamp and no fixture uses them. `RoutingPolicyCreateRequest.maximum_output_tokens` is
/// `None` in `support::LifecycleFixture`, so nothing competes to set `max_tokens` either.
const PROFILE_TEMPERATURE: f64 = 0.37;
const PROFILE_MAX_TOKENS: i64 = 271;

/// The caller's own values for case 3 — different from the profile's in both fields, and
/// different again from any default.
const CALLER_TEMPERATURE: f64 = 1.25;
const CALLER_MAX_TOKENS: u64 = 999;

/// The tool the profile's `tool_policy` declares. Case 5 asserts it does *not* reach the wire.
const PROFILE_TOOL_NAME: &str = "f49_lookup";

const PROMPT: &str = "say something";

/// Whether the fixture attaches an agent profile to its route.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Profile {
    Attached,
    None,
}

struct Case {
    fixture: LifecycleFixture,
    provider: MockOpenAiServer,
    moira: MoiraHttpServer,
    client: reqwest::Client,
    profile: Option<AgentProfileRecord>,
}

impl Case {
    async fn new(profile: Profile, scripts: Vec<ProviderScript>) -> Option<Self> {
        let fixture = LifecycleFixture::new().await?;
        let provider = MockOpenAiServer::start(scripts).await;
        fixture
            .add_provider(provider.base_url(), 10, RuntimePolicy::default())
            .await;
        let attached = if profile == Profile::Attached {
            Some(attach_agent_profile(&fixture).await)
        } else {
            None
        };
        assert_eq!(
            route_agent_profile_id(&fixture).await.is_some(),
            profile == Profile::Attached,
            "the fixture's own premise failed: this suite exists because a route whose \
             agent_profile_id is NULL exercises none of the code under test, so a case that \
             silently failed to attach one would assert nothing"
        );

        // `diagnostic_endpoint_enabled` is `false` in `Settings::default()`, so the route 404s
        // without this. Same clone-and-override `tests/structured_output.rs` performs.
        let mut state = fixture.state.clone();
        let mut settings = (*fixture.state.settings).clone();
        settings.runtime.diagnostic_endpoint_enabled = true;
        state.settings = Arc::new(settings);
        let moira = MoiraHttpServer::start(state).await;
        Some(Self {
            fixture,
            provider,
            moira,
            client: reqwest::Client::new(),
            profile: attached,
        })
    }

    /// Disables the attached profile, leaving `route_definitions.agent_profile_id` pointing at
    /// it. Used by the last case in this file.
    async fn disable_profile(&self) {
        let profile = self
            .profile
            .as_ref()
            .expect("only a case with an attached profile can disable one");
        RuntimeAdminService::new(&self.fixture.state)
            .expect("runtime admin service")
            .set_agent_profile_enabled(
                &self.fixture.actor,
                &request_context(),
                profile.id,
                profile.version,
                false,
            )
            .await
            .expect("disable the agent profile");
        assert_eq!(
            route_agent_profile_id(&self.fixture).await,
            Some(profile.id),
            "disabling must not clear the route's reference — a NULL here would make the case \
             below indistinguishable from the no-profile control"
        );
    }

    async fn diagnose(&self, stream: bool, options: ExecutionOptions) -> (StatusCode, Value) {
        let request = DiagnosticExecutionRequest {
            application_id: Some(self.fixture.application_id),
            external_tenant_id: None,
            external_user_id: Some("f49-agent-profile".to_string()),
            route: Some(self.fixture.route_key.clone()),
            provider_id: None,
            provider_model_id: None,
            credential_id: None,
            prompt: PROMPT.to_string(),
            stream,
            options: ExecutionOptions {
                timeout_ms: Some(5_000),
                stream,
                ..options
            },
            metadata: json!({ "test_fixture": true }),
        };
        let response = tokio::time::timeout(
            WAIT,
            self.client
                .post(format!(
                    "{}/api/v1/admin/runtime/diagnose",
                    self.moira.base_url
                ))
                .header("x-request-id", format!("f49-{}", Uuid::now_v7()))
                .json(&serde_json::to_value(&request).expect("serialize diagnostic request"))
                .send(),
        )
        .await
        .expect("diagnose request timed out")
        .expect("diagnose request");
        let status = response.status();
        let body = response.text().await.expect("diagnose body");
        (
            status,
            serde_json::from_str(&body).unwrap_or(Value::String(body)),
        )
    }

    /// The one body that reached the provider.
    ///
    /// Asserting the count is not decoration: every assertion below is on `requests[0]`, and a
    /// suite whose request never left the process would otherwise index-panic in a way that
    /// reads like a wiring bug rather than the vacuous pass it is (HANDOFF §2.3).
    async fn only_provider_body(&self) -> Value {
        let requests = self.provider.requests().await;
        assert_eq!(
            requests.len(),
            1,
            "exactly one request must have reached the provider; got {}",
            requests.len()
        );
        requests[0].body.clone()
    }

    async fn shutdown(self) {
        self.provider.shutdown().await;
        self.moira.shutdown().await;
    }
}

/// Creates an active agent profile and points the fixture's route definition at it.
///
/// `patch_route_definition` is used rather than a route created with the profile inline,
/// because `LifecycleFixture` owns route creation and every other suite depends on its current
/// shape. The version is read back rather than assumed to be `1`: an assumed version would make
/// this helper break as a `409` the first time anything else patches the route first.
async fn attach_agent_profile(fixture: &LifecycleFixture) -> AgentProfileRecord {
    let admin = RuntimeAdminService::new(&fixture.state).expect("runtime admin service");
    let profile = admin
        .create_agent_profile(
            &fixture.actor,
            &request_context(),
            AgentProfileCreateRequest {
                profile_key: format!("f49-{}", Uuid::now_v7().simple()),
                display_name: "F49 wire profile".to_string(),
                preamble: Some(PROFILE_PREAMBLE.to_string()),
                temperature: Some(PROFILE_TEMPERATURE),
                max_tokens: Some(PROFILE_MAX_TOKENS),
                // Non-empty on purpose. `tool_policy` is the field a tool-calling
                // implementation reads first, and case 5 asserts what happens to it today.
                tool_policy: json!({
                    "tools": [{
                        "name": PROFILE_TOOL_NAME,
                        "description": "look something up",
                        "parameters": { "type": "object", "properties": {} }
                    }]
                }),
                context_policy: json!({}),
                memory_policy: json!({}),
                metadata: json!({ "test_fixture": true }),
            },
        )
        .await
        .expect("create agent profile");

    let version: i64 = sqlx::query_scalar("select version from route_definitions where id = $1")
        .bind(fixture.route_id)
        .fetch_one(&fixture.pool)
        .await
        .expect("read route version");
    admin
        .patch_route_definition(
            &fixture.actor,
            &request_context(),
            fixture.route_id,
            version,
            RouteDefinitionPatchRequest {
                agent_profile_id: Some(profile.id),
                ..RouteDefinitionPatchRequest::default()
            },
        )
        .await
        .expect("attach the agent profile to the route");
    profile
}

async fn route_agent_profile_id(fixture: &LifecycleFixture) -> Option<Uuid> {
    sqlx::query_scalar("select agent_profile_id from route_definitions where id = $1")
        .bind(fixture.route_id)
        .fetch_one(&fixture.pool)
        .await
        .expect("read route agent_profile_id")
}

/// The text of an OpenAI chat message, whichever of the two legal encodings Rig used.
///
/// `rig-core` 0.40 builds the preamble as `Message::System { content: OneOrMany<SystemContent> }`,
/// which serialises as `[{"type":"text","text":"..."}]`; some provider `finalize_request_body`
/// hooks flatten the same field to a bare string. Which one arrives is Rig's business. What is
/// Moira's business — the exact text, under the `system` role, in position 0 — is asserted at
/// the call sites, and an empty or absent `content` still fails those.
fn message_text(message: &Value) -> String {
    match &message["content"] {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| part["text"].as_str())
            .collect::<Vec<_>>()
            .join(""),
        other => panic!("unexpected message content encoding: {other}"),
    }
}

/// **Case 1 — the branch reaches the wire.**
///
/// Cheapest edits that break the property, each of which turns this red:
/// `preamble: None`, `temperature: command.options.temperature` (dropping the `or_else`),
/// `max_tokens: command.options.max_tokens`, or removing the `get_active_agent_profile` lookup
/// at `execution.rs:191` so `agent_profile` is `None` again.
#[tokio::test]
async fn an_agent_profile_puts_its_preamble_temperature_and_max_tokens_on_the_provider_wire() {
    let Some(case) = Case::new(
        Profile::Attached,
        vec![ProviderScript::Completion {
            text: "ok".to_string(),
        }],
    )
    .await
    else {
        return;
    };

    let (status, body) = case.diagnose(false, ExecutionOptions::default()).await;
    assert_eq!(status, StatusCode::OK, "diagnose failed: {body}");
    assert_eq!(body["outcome"]["status"], "succeeded", "{body}");

    let wire = case.only_provider_body().await;
    let messages = wire["messages"]
        .as_array()
        .unwrap_or_else(|| panic!("no messages on the wire: {wire}"));
    assert_eq!(
        messages.len(),
        2,
        "the profile's preamble must be prepended to the caller's one message: {wire}"
    );
    assert_eq!(
        messages[0]["role"], "system",
        "the preamble must arrive as the leading system message: {wire}"
    );
    assert_eq!(
        message_text(&messages[0]),
        PROFILE_PREAMBLE,
        "the agent profile's preamble must reach the provider verbatim: {wire}"
    );
    assert_eq!(messages[1]["role"], "user", "{wire}");
    assert_eq!(message_text(&messages[1]), PROMPT, "{wire}");

    assert_eq!(
        wire["temperature"].as_f64(),
        Some(PROFILE_TEMPERATURE),
        "the agent profile's temperature must reach the provider: {wire}"
    );
    assert_eq!(
        wire["max_tokens"].as_u64(),
        Some(PROFILE_MAX_TOKENS as u64),
        "the agent profile's max_tokens must reach the provider: {wire}"
    );

    case.shutdown().await;
}

/// **Case 2 — the control, and the executable statement of F49's premise.**
///
/// This is the body every *other* fixture in the tree produces. It is what makes case 1
/// discriminating: hardcoding the profile's values into `build_completion_request` would leave
/// case 1 green and turn this red.
#[tokio::test]
async fn without_an_agent_profile_the_same_route_sends_no_preamble_temperature_or_max_tokens() {
    let Some(case) = Case::new(
        Profile::None,
        vec![ProviderScript::Completion {
            text: "ok".to_string(),
        }],
    )
    .await
    else {
        return;
    };

    let (status, body) = case.diagnose(false, ExecutionOptions::default()).await;
    assert_eq!(status, StatusCode::OK, "diagnose failed: {body}");
    assert_eq!(body["outcome"]["status"], "succeeded", "{body}");

    let wire = case.only_provider_body().await;
    let messages = wire["messages"]
        .as_array()
        .unwrap_or_else(|| panic!("no messages on the wire: {wire}"));
    assert_eq!(
        messages.len(),
        1,
        "with no profile there is no preamble, so the caller's message must be the only one: \
         {wire}"
    );
    assert_eq!(messages[0]["role"], "user", "{wire}");
    assert!(
        wire.get("temperature").is_none(),
        "with no profile and no caller value, temperature must be absent — a value here means \
         something other than the agent profile is setting it, and case 1 proves nothing: \
         {wire}"
    );
    assert!(
        wire.get("max_tokens").is_none(),
        "with no profile and no caller value, max_tokens must be absent: {wire}"
    );

    case.shutdown().await;
}

/// **Case 3 — precedence, in the direction the code chose.**
///
/// `build_completion_request` writes `command.options.temperature.or_else(|| profile...)`. Case
/// 1 cannot see that order: with `options.temperature == None` both orderings produce the same
/// byte. Here they differ, and the caller's value must win — inverting either `or_else` turns
/// this red while leaving case 1 green.
///
/// The preamble is re-asserted so the case cannot pass by the profile having been dropped
/// entirely: "the caller's temperature is on the wire" is also what a missing profile looks
/// like.
#[tokio::test]
async fn a_callers_own_temperature_and_max_tokens_win_over_the_agent_profiles() {
    let Some(case) = Case::new(
        Profile::Attached,
        vec![ProviderScript::Completion {
            text: "ok".to_string(),
        }],
    )
    .await
    else {
        return;
    };

    let (status, body) = case
        .diagnose(
            false,
            ExecutionOptions {
                temperature: Some(CALLER_TEMPERATURE),
                max_tokens: Some(CALLER_MAX_TOKENS),
                ..ExecutionOptions::default()
            },
        )
        .await;
    assert_eq!(status, StatusCode::OK, "diagnose failed: {body}");
    assert_eq!(body["outcome"]["status"], "succeeded", "{body}");

    let wire = case.only_provider_body().await;
    assert_eq!(
        wire["temperature"].as_f64(),
        Some(CALLER_TEMPERATURE),
        "the caller's temperature must override the profile's {PROFILE_TEMPERATURE}: {wire}"
    );
    assert_eq!(
        wire["max_tokens"].as_u64(),
        Some(CALLER_MAX_TOKENS),
        "the caller's max_tokens must override the profile's {PROFILE_MAX_TOKENS}: {wire}"
    );
    assert_eq!(
        message_text(&wire["messages"][0]),
        PROFILE_PREAMBLE,
        "the profile must still be in force — overriding two of its fields must not drop it: \
         {wire}"
    );

    case.shutdown().await;
}

/// **Case 4 — the streaming arm.**
///
/// `execute_rig_stream` and `execute_rig_completion` are handed the same `CompletionRequest`
/// today, so this is a guard against that stopping being true, not a second observation of the
/// same fact. Rig also encodes a stream differently (`stream()` merges `stream: true` and
/// `stream_options` into the serialised body before sending), so this is a genuinely different
/// wire path.
///
/// `"stream": true` is asserted on the same body: without it the case would still pass if the
/// request had quietly gone down the non-streaming arm, which is precisely the drift it is here
/// to catch.
#[tokio::test]
async fn the_streaming_arm_carries_the_same_agent_profile_fields_to_the_wire() {
    let Some(case) = Case::new(
        Profile::Attached,
        vec![ProviderScript::Stream {
            deltas: vec!["ok".to_string()],
        }],
    )
    .await
    else {
        return;
    };

    let (status, body) = case.diagnose(true, ExecutionOptions::default()).await;
    assert_eq!(status, StatusCode::OK, "diagnose failed: {body}");
    assert_eq!(body["outcome"]["status"], "succeeded", "{body}");

    let wire = case.only_provider_body().await;
    assert_eq!(
        wire["stream"], true,
        "this case must observe the streaming encoder, not the completion one: {wire}"
    );
    assert_eq!(
        message_text(&wire["messages"][0]),
        PROFILE_PREAMBLE,
        "the preamble must reach the wire on the streaming arm too: {wire}"
    );
    assert_eq!(wire["messages"][0]["role"], "system", "{wire}");
    assert_eq!(
        wire["temperature"].as_f64(),
        Some(PROFILE_TEMPERATURE),
        "{wire}"
    );
    assert_eq!(
        wire["max_tokens"].as_u64(),
        Some(PROFILE_MAX_TOKENS as u64),
        "{wire}"
    );

    case.shutdown().await;
}

/// **Case 5 — `tool_policy` is not forwarded, and this is not a replacement for F48's guard.**
///
/// `build_completion_request` hardcodes `tools: Vec::new()`, and the public plane refuses
/// caller-declared tools outright. The profile in this fixture *does* declare a tool, so this
/// case observes the one input a tool-calling implementation would read
/// (`AgentProfileRecord::tool_policy`) and asserts nothing derived from it reached the
/// provider.
///
/// **Read this before deleting `moiras_request_still_carries_its_schema_onto_rigs_openai_wire_body`
/// in `src/application/execution.rs`.** That unit guard and this case fail together under the
/// same mutation, but they say different things and only one of them says the dangerous one:
///
/// * this case says *tools appeared on the wire*;
/// * F48's guard says *and therefore `rig-core` silently dropped `response_format`*, because
///   `should_apply_response_format` also requires `tools.is_empty() || history_has_tool_result`.
///
/// No case in this file sends an `output_schema`, so none of them can observe the drop. Whoever
/// enables tool calling will make this case red and be tempted to update it; F48's guard is the
/// one that tells them what else just broke.
#[tokio::test]
async fn an_agent_profiles_tool_policy_does_not_become_a_tool_list_on_the_wire() {
    let Some(case) = Case::new(
        Profile::Attached,
        vec![ProviderScript::Completion {
            text: "ok".to_string(),
        }],
    )
    .await
    else {
        return;
    };

    let (status, body) = case.diagnose(false, ExecutionOptions::default()).await;
    assert_eq!(status, StatusCode::OK, "diagnose failed: {body}");
    assert_eq!(body["outcome"]["status"], "succeeded", "{body}");

    let wire = case.only_provider_body().await;
    // The profile really does declare one, so this is not vacuous.
    assert!(
        !wire.to_string().contains(PROFILE_TOOL_NAME),
        "the agent profile's tool_policy must not reach the provider in any form: enabling tool \
         calling means deciding what happens to structured output on turn 1 first — see F48 in \
         plans/reports/EXECUTION-LEDGER.md and the guard named in this test's comment: {wire}"
    );
    assert!(
        wire.get("tools").is_none(),
        "Moira sends no tools in this phase: {wire}"
    );
    assert!(
        wire.get("tool_choice").is_none(),
        "Moira sends no tool_choice in this phase: {wire}"
    );

    case.shutdown().await;
}

/// **Finding F50 — DOCUMENTS CURRENT BEHAVIOUR. This is not a guard, and it is expected to go
/// red when the behaviour is decided.**
///
/// `execution.rs:191` resolves the profile with `get_active_agent_profile`, whose query filters
/// `status = 'active' and deleted_at is null`. Disabling a profile
/// (`RuntimeAdminService::set_agent_profile_enabled(.., false)`) leaves
/// `route_definitions.agent_profile_id` pointing at the row — the FK is `on delete set null`
/// and `soft_delete_agent_profile` never issues a `DELETE`, so neither disabling nor deleting
/// clears it. The lookup then returns `Ok(None)` and the match arm above treats that
/// identically to "this route has no profile": **no failure, no `warn!`, no runtime event, no
/// audit row.** Every subsequent execution on the route silently loses its preamble, its
/// temperature and its max_tokens.
///
/// A preamble is where guardrails live, so the failure mode is not a missing nicety — it is an
/// unguarded model answering production traffic with a `succeeded` status. The comparison that
/// makes it look wrong: an unresolvable *route* is a `RouteNotFound` failure, so the agent
/// profile is the only runtime reference in this path whose disappearance is silent.
///
/// **It is recorded rather than fixed because the fix is a product decision**, and guessing it
/// would be the worse error. Fail-closed (refuse the execution) is safer but breaks any
/// deployment that disables a profile expecting its routes to keep serving; fail-open with a
/// `warn!` and a runtime event is cheaper but still serves the unguarded request.
///
/// **Reversal condition:** decide fail-closed vs observable-fail-open for a dangling
/// `agent_profile_id`. On either decision this case is *wrong* and must be rewritten — a
/// fail-closed fix makes it red at `outcome.status`, an observable fail-open fix leaves it
/// green while the thing it documents (silence) is no longer true. It is named
/// `documents_` and not `guards_` for exactly the reason HANDOFF §3.4 gives twice: a test that
/// pins current behaviour can hold a defect in place as firmly as a guard holds a property.
#[tokio::test]
async fn documents_current_behaviour_a_disabled_agent_profile_is_silently_ignored() {
    let Some(case) = Case::new(
        Profile::Attached,
        vec![ProviderScript::Completion {
            text: "ok".to_string(),
        }],
    )
    .await
    else {
        return;
    };
    case.disable_profile().await;

    let (status, body) = case.diagnose(false, ExecutionOptions::default()).await;
    assert_eq!(status, StatusCode::OK, "diagnose failed: {body}");
    assert_eq!(
        body["outcome"]["status"], "succeeded",
        "today a dangling agent_profile_id does not fail the execution — if this is now `failed`, \
         the F50 decision has been made fail-closed and this case must be rewritten as a guard: \
         {body}"
    );
    assert_eq!(
        body["outcome"]["failure"],
        Value::Null,
        "no failure is reported either: {body}"
    );

    let wire = case.only_provider_body().await;
    assert_eq!(
        wire["messages"].as_array().map(Vec::len),
        Some(1),
        "F50: the disabled profile's preamble is dropped with no signal of any kind: {wire}"
    );
    assert!(
        wire.get("temperature").is_none(),
        "F50: and its temperature with it: {wire}"
    );
    assert!(
        wire.get("max_tokens").is_none(),
        "F50: and its max_tokens: {wire}"
    );

    case.shutdown().await;
}
