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
//! 6. **Finding F50 — the disabled profile is announced.** Disabling an agent profile leaves
//!    the route pointing at it and degrades every execution on that route to a profile-less
//!    request. That is now announced as a runtime event, a `warn!` and an audit row, and the
//!    guard asserts on the event and the row: logs are easy to assert and easy to lose.
//! 7. **The soft-delete twin of case 6**, because `delete_agent_profile` is a different admin
//!    method and is the one that could have hard-deleted the row.
//! 8. **The streaming twin of case 6.** Case 4's justification — "they are separate arms" —
//!    applies to this property too, and a per-path pairing with an empty cell is HANDOFF
//!    §3.4's twelfth entry.
//! 9. **The control for cases 6–8, and it is load-bearing.** A route that never had a
//!    profile must announce *nothing*. Without it, a fix that fired on `agent_profile_id`
//!    being `Some` rather than on the lookup failing would satisfy every guard above.
//! 10. **What F50 still has not decided, and this case DOCUMENTS CURRENT BEHAVIOUR rather
//!     than guarding it.** The request is still served, unguarded, with a `succeeded`
//!     status. Whether it should instead be refused is a product decision; read that case's
//!     comment before changing it.
//!
//! Cases 6–9 survive that decision and case 10 does not, which is why they are separate
//! cases rather than one — merging them would make the observability guard go red on a
//! fail-closed fix, the pinned-defect shape HANDOFF §3.4 records twice.

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

    /// Soft-deletes the attached profile, leaving the route's reference intact.
    ///
    /// The twin of [`Self::disable_profile`], and it is not redundant: F50 names *two*
    /// operations that leave a dangling reference, and they take different code paths in
    /// `RuntimeAdminService` — `set_agent_profile_enabled` writes `status = 'disabled'`,
    /// `delete_agent_profile` writes `status = 'deleted', deleted_at = now()`. Only the
    /// second could plausibly have issued a real `DELETE`, which the FK's
    /// `on delete set null` would turn into "this route has no profile" — the case that must
    /// stay silent. The assertion below is what proves it does not.
    async fn soft_delete_profile(&self) {
        let profile = self
            .profile
            .as_ref()
            .expect("only a case with an attached profile can delete one");
        RuntimeAdminService::new(&self.fixture.state)
            .expect("runtime admin service")
            .delete_agent_profile(
                &self.fixture.actor,
                &request_context(),
                profile.id,
                profile.version,
            )
            .await
            .expect("soft-delete the agent profile");
        assert_eq!(
            route_agent_profile_id(&self.fixture).await,
            Some(profile.id),
            "soft-deleting must not clear the route's reference — a NULL here would make \
             this indistinguishable from the no-profile control, and would mean F50's \
             premise is wrong for the delete path"
        );
    }

    /// Every audit row this execution wrote, as `(action, result)`.
    ///
    /// Read from the database rather than from the response, because the audit row is the
    /// signal that has to outlive the request — the whole reason it is one of the three.
    async fn audit_actions(&self, execution_id: &str) -> Vec<(String, String)> {
        sqlx::query_as::<_, (String, String)>(
            "select action, result from audit_logs where resource_type = 'execution' \
             and resource_id = $1 order by occurred_at",
        )
        .bind(execution_id)
        .fetch_all(&self.fixture.pool)
        .await
        .expect("read the execution's audit rows")
    }

    async fn shutdown(self) {
        self.provider.shutdown().await;
        self.moira.shutdown().await;
    }
}

/// The payloads of every runtime event of one type in a diagnose response.
///
/// `POST /api/v1/admin/runtime/diagnose` returns `Vec<RuntimeEventEnvelope>` verbatim, which
/// is what makes the runtime event assertable here at all — and it is the signal worth
/// asserting on. A guard that only proved "a `warn!` was emitted" would be weak: log lines
/// are easy to assert and easy to lose, and nothing consumes them structurally.
fn events_of_type<'a>(body: &'a Value, event_type: &str) -> Vec<&'a Value> {
    body["events"]
        .as_array()
        .unwrap_or_else(|| panic!("no events array on the diagnose response: {body}"))
        .iter()
        .filter(|event| event["event_type"] == event_type)
        .map(|event| &event["payload"])
        .collect()
}

const UNAVAILABLE_EVENT: &str = "agent_profile_unavailable";
const UNAVAILABLE_AUDIT_ACTION: &str = "agent_profile.unavailable";

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

/// **Finding F50 — the guard. A dangling `agent_profile_id` is announced, not swallowed.**
///
/// `execution.rs` resolves the profile with `get_active_agent_profile`, whose query filters
/// `status = 'active' and deleted_at is null`. Disabling a profile
/// (`set_agent_profile_enabled(.., false)`) or soft-deleting it (`delete_agent_profile`, which
/// writes `status = 'deleted', deleted_at = now()` and never a `DELETE`) leaves
/// `route_definitions.agent_profile_id` pointing at the row. The lookup returns `Ok(None)`, and
/// until this guard existed the match arm treated that identically to "this route has no
/// profile": **no failure, no `warn!`, no runtime event, no audit row.** Every execution on
/// the route lost its preamble, its temperature and its max_tokens and reported `succeeded`.
/// A preamble is where guardrails live, so the failure mode is an unguarded model answering
/// production traffic.
///
/// # This guards the observability, not the outcome
///
/// Whether Moira should also *refuse* is an open product decision — fail-closed is safer but
/// breaks any deployment that disables a profile expecting its routes to keep serving,
/// observable fail-open is cheaper but still serves the unguarded request. Silence is a defect
/// under *either* answer: both require the operator to be told and differ only in whether the
/// request is also refused. So this case asserts the part both answers share, and it survives
/// the decision either way. The remaining fail-open behaviour is documented — not guarded — by
/// [`documents_current_behaviour_a_dangling_agent_profile_still_serves_the_request`], which is
/// the case that becomes wrong when the decision lands. Keeping them apart is the
/// `documents_`/`guards_` distinction HANDOFF §3.4 asks for: merging them would make this guard
/// go red on a fail-closed fix, which is exactly the pinned-defect shape.
///
/// # Three controls, and each answers a different cheap edit
///
/// 1. **Before disabling.** The same fixture, the same route, an *active* profile: no event and
///    no audit row. Without it, moving the announcement out of the `resolved.is_none()` branch
///    and firing it whenever `agent_profile_id` is `Some` would leave everything below green.
/// 2. **Liveness of the event channel.** `route_selected` must be present in both responses, so
///    "no `agent_profile_unavailable` event" cannot pass against an empty events array.
/// 3. **Liveness of the audit trail.** `execution.started` must be present for both executions,
///    so "no `agent_profile.unavailable` row" cannot pass against an execution that wrote no
///    audit rows at all.
///
/// Both operations that produce a dangling reference are driven — disable in this case,
/// soft-delete in [`guards_a_soft_deleted_agent_profile_is_announced_too`] — because they are
/// different admin methods and only one of them could ever have hard-deleted the row.
#[tokio::test]
async fn guards_a_disabled_agent_profile_is_announced_rather_than_silently_ignored() {
    let Some(case) = Case::new(
        Profile::Attached,
        vec![
            ProviderScript::Completion {
                text: "ok".to_string(),
            },
            ProviderScript::Completion {
                text: "ok".to_string(),
            },
        ],
    )
    .await
    else {
        return;
    };

    // Control 1: the profile is still active, so nothing is announced.
    let (status, healthy) = case.diagnose(false, ExecutionOptions::default()).await;
    assert_eq!(status, StatusCode::OK, "diagnose failed: {healthy}");
    let healthy_execution = healthy["outcome"]["execution_id"]
        .as_str()
        .unwrap_or_else(|| panic!("no execution id: {healthy}"))
        .to_string();
    assert!(
        events_of_type(&healthy, UNAVAILABLE_EVENT).is_empty(),
        "a route whose agent profile resolves must announce nothing; this event fires on \
         the resolution *failing*, not on the route having a profile: {healthy}"
    );
    assert!(
        !events_of_type(&healthy, "route_selected").is_empty(),
        "control: route_selected is missing, so the absence asserted above proves nothing \
         about this event in particular: {healthy}"
    );
    let healthy_audit = case.audit_actions(&healthy_execution).await;
    assert!(
        !healthy_audit
            .iter()
            .any(|(action, _)| action == UNAVAILABLE_AUDIT_ACTION),
        "an active profile wrote an agent_profile.unavailable audit row: {healthy_audit:?}"
    );
    assert!(
        healthy_audit
            .iter()
            .any(|(action, _)| action == "execution.started"),
        "control: this execution wrote no audit rows at all, so the absence asserted above \
         proves nothing: {healthy_audit:?}"
    );

    // The finding: the route keeps pointing at a profile the runtime will not use.
    case.disable_profile().await;
    let (status, body) = case.diagnose(false, ExecutionOptions::default()).await;
    assert_eq!(status, StatusCode::OK, "diagnose failed: {body}");
    let execution_id = body["outcome"]["execution_id"]
        .as_str()
        .unwrap_or_else(|| panic!("no execution id: {body}"))
        .to_string();

    let announced = events_of_type(&body, UNAVAILABLE_EVENT);
    assert_eq!(
        announced.len(),
        1,
        "F50: a disabled agent profile must announce itself exactly once as a structured \
         runtime event — the request is now running without the preamble its route \
         declares: {body}"
    );
    let payload = announced[0];
    assert_eq!(
        payload["agent_profile_id"].as_str(),
        Some(
            case.profile
                .as_ref()
                .expect("attached profile")
                .id
                .to_string()
                .as_str()
        ),
        "the event must name the profile that vanished, or an operator cannot find it: \
         {payload}"
    );
    assert_eq!(
        payload["route_id"].as_str(),
        Some(case.fixture.route_id.to_string().as_str()),
        "and the route still pointing at it: {payload}"
    );
    assert_eq!(
        payload["route_key"].as_str(),
        Some(case.fixture.route_key.as_str()),
        "and that route's key, which is what an operator actually recognises: {payload}"
    );

    let audit = case.audit_actions(&execution_id).await;
    assert!(
        audit
            .iter()
            .any(|(action, result)| action == UNAVAILABLE_AUDIT_ACTION && result == "failed"),
        "F50: the degradation must also leave a durable audit row — the runtime event lives \
         only as long as the response and the warn! only as long as the log: {audit:?}"
    );

    case.shutdown().await;
}

/// **F50, the second way to produce a dangling reference.**
///
/// Not redundant with the disable case. F50 names disabling *and* soft-deleting, and they are
/// different `RuntimeAdminService` methods writing different `status` values. Soft-delete is
/// the one that could plausibly have been a real `DELETE` — which the FK's
/// `on delete set null` would have turned into "this route has no profile", the case that must
/// stay silent — so this case is simultaneously the guard for the delete path and the proof
/// that F50's premise holds for it. `Case::soft_delete_profile` asserts the reference survives
/// before the execution runs.
#[tokio::test]
async fn guards_a_soft_deleted_agent_profile_is_announced_too() {
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
    case.soft_delete_profile().await;

    let (status, body) = case.diagnose(false, ExecutionOptions::default()).await;
    assert_eq!(status, StatusCode::OK, "diagnose failed: {body}");
    let execution_id = body["outcome"]["execution_id"]
        .as_str()
        .unwrap_or_else(|| panic!("no execution id: {body}"))
        .to_string();

    assert_eq!(
        events_of_type(&body, UNAVAILABLE_EVENT).len(),
        1,
        "a soft-deleted agent profile degrades the route exactly as a disabled one does and \
         must be announced exactly as loudly: {body}"
    );
    let audit = case.audit_actions(&execution_id).await;
    assert!(
        audit
            .iter()
            .any(|(action, _)| action == UNAVAILABLE_AUDIT_ACTION),
        "no audit row for the soft-deleted profile: {audit:?}"
    );

    case.shutdown().await;
}

/// **F50 on the streaming arm — the empty cell in this suite's own matrix.**
///
/// Case 4 justifies its existence with "both arms share the one `build_completion_request`
/// call today, but they are separate arms". HANDOFF §3.4's twelfth entry is that when a suite
/// justifies a case by "this is a separate path", the justification applies to **every**
/// property the suite tests, not just the one that prompted it — a per-path pairing is a
/// matrix and this one had a hole in it.
///
/// The announcement is currently made in `execute_inner`, *before* the two arms diverge, and
/// `get_active_agent_profile` has exactly one call site in `src/`. So this case cannot fail
/// today for a reason the disable case would not also catch. It exists so that the day
/// something moves the lookup, or the streaming task grows its own resolution, the hole is
/// already covered — which is precisely the shape of the F49 fixture hole that made the whole
/// branch untested for as long as it did.
#[tokio::test]
async fn guards_the_streaming_arm_announces_a_dangling_agent_profile_too() {
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
    case.disable_profile().await;

    let (status, body) = case.diagnose(true, ExecutionOptions::default()).await;
    assert_eq!(status, StatusCode::OK, "diagnose failed: {body}");
    let execution_id = body["outcome"]["execution_id"]
        .as_str()
        .unwrap_or_else(|| panic!("no execution id: {body}"))
        .to_string();

    // The case cannot pass by silently having taken the non-streaming path, for the same
    // reason case 4 asserts this.
    let wire = case.only_provider_body().await;
    assert_eq!(
        wire["stream"], true,
        "this case must actually stream, or it is case 6 with extra steps: {wire}"
    );

    assert_eq!(
        events_of_type(&body, UNAVAILABLE_EVENT).len(),
        1,
        "the streaming arm dropped the same profile just as silently: {body}"
    );
    let audit = case.audit_actions(&execution_id).await;
    assert!(
        audit
            .iter()
            .any(|(action, _)| action == UNAVAILABLE_AUDIT_ACTION),
        "no audit row on the streaming arm: {audit:?}"
    );

    case.shutdown().await;
}

/// **F50's control, and it is the one that keeps the finding from becoming noise.**
///
/// A route that was never given an agent profile is the *normal* case — it is what every other
/// fixture in this tree produces, and it must stay exactly as silent as it was. The distinction
/// is available at the call site because `route.agent_profile_id` is read before the lookup:
/// `None` is "no profile", `Some(id)` with an unresolved lookup is "the profile vanished". A
/// fix that could not tell them apart would announce a degradation on every profile-less
/// execution in the deployment, which is how an operator signal becomes something nobody reads.
///
/// Both liveness controls are here for the reason §3.4 gives: an assertion that nothing
/// happened is worthless unless something can happen.
#[tokio::test]
async fn guards_a_route_with_no_agent_profile_announces_nothing() {
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
    let execution_id = body["outcome"]["execution_id"]
        .as_str()
        .unwrap_or_else(|| panic!("no execution id: {body}"))
        .to_string();

    assert!(
        events_of_type(&body, UNAVAILABLE_EVENT).is_empty(),
        "a route with no agent profile is not a degraded route; announcing one here makes \
         the signal fire on every profile-less execution in the deployment: {body}"
    );
    assert!(
        !events_of_type(&body, "route_selected").is_empty(),
        "control: route_selected is missing, so the silence asserted above proves nothing: \
         {body}"
    );

    let audit = case.audit_actions(&execution_id).await;
    assert!(
        !audit
            .iter()
            .any(|(action, _)| action == UNAVAILABLE_AUDIT_ACTION),
        "a route with no agent profile wrote an agent_profile.unavailable audit row: \
         {audit:?}"
    );
    assert!(
        audit
            .iter()
            .any(|(action, _)| action == "execution.started"),
        "control: this execution wrote no audit rows at all: {audit:?}"
    );

    case.shutdown().await;
}

/// **F50 — DOCUMENTS CURRENT BEHAVIOUR. This is not a guard and it is expected to go red when
/// the product decision lands.**
///
/// What is *not* yet decided is whether Moira should refuse a request whose route points at a
/// profile that no longer resolves. Today it serves it: `succeeded`, no failure, and a wire
/// body with no preamble, no temperature and no max_tokens — an unguarded model answering
/// production traffic, which is why the finding is worth a decision rather than a shrug.
///
/// It is named `documents_` and not `guards_` for the reason HANDOFF §3.4 gives twice: a test
/// that pins current behaviour can hold a defect in place as firmly as a guard holds a
/// property. Its predecessor was named
/// `documents_current_behaviour_a_disabled_agent_profile_is_silently_ignored`, and that name
/// stopped being true the moment the announcement shipped — the condition is observed now, it
/// is simply not refused.
///
/// **Reversal condition:** when fail-closed vs fail-open is decided, this case is wrong under
/// the fail-closed answer (it reds at `outcome.status`) and is simply redundant under the
/// fail-open one. Delete or rewrite it then. The observability asserted by
/// [`guards_a_disabled_agent_profile_is_announced_rather_than_silently_ignored`] is **not**
/// revisited by that decision, which is why the two are separate cases.
#[tokio::test]
async fn documents_current_behaviour_a_dangling_agent_profile_still_serves_the_request() {
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
        "today a dangling agent_profile_id does not fail the execution — if this is now \
         `failed`, the F50 decision has been taken fail-closed and this case must be \
         deleted: {body}"
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
        "the disabled profile's preamble is dropped and the request proceeds without it: \
         {wire}"
    );
    assert!(
        wire.get("temperature").is_none(),
        "and its temperature with it: {wire}"
    );
    assert!(
        wire.get("max_tokens").is_none(),
        "and its max_tokens: {wire}"
    );

    case.shutdown().await;
}
