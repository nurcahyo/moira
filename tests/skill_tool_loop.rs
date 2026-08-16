//! End-to-end coverage for the rig tool loop and `HttpSkillTool` (issue #84, plan 12 §5).
//!
//! Everything a `skill_refs`-bearing agent profile does, driven through the *real* execution
//! path: `MoiraExecutionService::execute` → `build_completion_request` → a scripted
//! OpenAI-compatible provider that answers with a tool call → `HttpSkillTool` against a
//! scripted skill target → the tool result round-tripped into a second provider turn → the
//! final answer.
//!
//! # Two scripted servers, no real network
//!
//! Owner policy is mocks only, and both halves are mocked separately on purpose:
//!
//! * [`mock_openai::MockOpenAiServer`] plays the model, so `rig-core`'s own response decoding
//!   is what turns the scripted JSON into an `AssistantContent::ToolCall`;
//! * [`SkillTargetServer`] below plays the imported third-party API, so what is asserted is
//!   the request `HttpSkillTool` actually built — method, path, query, headers — rather than
//!   a restatement of the code that built it.
//!
//! # Why `skill_execution.allow_insecure_dev_urls` is on
//!
//! A scripted target lives on `http://127.0.0.1:PORT`, which the execution-time SSRF guard
//! refuses on both the scheme and the address rule. The flag is the same dev-only escape
//! hatch `public_api.image_urls.allow_insecure_dev_urls` already is, `Settings::validate`
//! hard-fails production while it is true, and it relaxes only the *address space* — the
//! `allowed_host` equality check still runs, which `a_skill_call_cannot_be_redirected_to_
//! another_host` proves by leaving the flag on and still being refused.
//!
//! The rows are written with SQL rather than through `POST /api/v1/admin/skills/import`,
//! because import refuses a loopback server URL in every environment (by design — see
//! `AgentPlatformService::validate_skill_url`). `tests/skill_import.rs` owns the import path;
//! this file owns what happens after a skill exists.
//!
//! Skips (never fails) when no test database is configured, per CONVENTIONS §3.

mod support;

use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::any,
};
use moira::{
    application::RuntimeAdminService,
    domain::{
        AgentProfileCreateRequest, ExecutionFailureClass, ExecutionStatus,
        RouteDefinitionPatchRequest,
    },
};
use serde_json::{Value, json};
use tokio::{net::TcpListener, task::JoinHandle};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use support::{
    LifecycleFixture, RuntimePolicy,
    mock_openai::{MockOpenAiServer, ProviderScript},
    request_context,
};

/// The secret the skill's credential holds. Asserted against by name, the way
/// `tests/execution_lifecycle.rs` asserts against `sk-lifecycle-secret`: the point of naming
/// it is that a leak is greppable.
const SKILL_SECRET: &str = "sk-skill-target-secret";

// =====================================================================================
// The scripted skill target — the third-party API an operator imported.
// =====================================================================================

#[derive(Debug, Clone)]
struct RecordedSkillCall {
    method: String,
    path: String,
    query: Option<String>,
    authorization: Option<String>,
    request_id: Option<String>,
    body: Option<Value>,
}

#[derive(Debug)]
struct SkillTargetState {
    calls: Mutex<Vec<RecordedSkillCall>>,
    status: StatusCode,
    body: String,
}

struct SkillTargetServer {
    address: SocketAddr,
    state: Arc<SkillTargetState>,
    shutdown: CancellationToken,
    task: JoinHandle<()>,
}

impl SkillTargetServer {
    async fn start(status: StatusCode, body: Value) -> Self {
        let state = Arc::new(SkillTargetState {
            calls: Mutex::new(Vec::new()),
            status,
            body: body.to_string(),
        });
        let app = Router::new()
            .fallback(any(handle_skill_call))
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind skill target");
        let address = listener.local_addr().expect("skill target address");
        let shutdown = CancellationToken::new();
        let task_shutdown = shutdown.clone();
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(task_shutdown.cancelled_owned())
                .await
                .expect("serve skill target");
        });
        Self {
            address,
            state,
            shutdown,
            task,
        }
    }

    fn origin(&self) -> String {
        format!("http://{}", self.address)
    }

    fn calls(&self) -> Vec<RecordedSkillCall> {
        self.state.calls.lock().expect("skill call log").clone()
    }

    async fn shutdown(self) {
        self.shutdown.cancel();
        tokio::time::timeout(Duration::from_secs(5), self.task)
            .await
            .expect("skill target shutdown timed out")
            .expect("skill target task panicked");
    }
}

async fn handle_skill_call(
    State(state): State<Arc<SkillTargetState>>,
    request: axum::extract::Request,
) -> Response {
    let method = request.method().to_string();
    let uri = request.uri().clone();
    let headers: HeaderMap = request.headers().clone();
    let body = axum::body::to_bytes(request.into_body(), usize::MAX)
        .await
        .unwrap_or_else(|_| Bytes::new());
    state
        .calls
        .lock()
        .expect("skill call log")
        .push(RecordedSkillCall {
            method,
            path: uri.path().to_string(),
            query: uri.query().map(str::to_string),
            authorization: headers
                .get(header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string),
            request_id: headers
                .get("x-request-id")
                .and_then(|value| value.to_str().ok())
                .map(str::to_string),
            body: if body.is_empty() {
                None
            } else {
                serde_json::from_slice(&body).ok()
            },
        });
    (
        state.status,
        [(header::CONTENT_TYPE, "application/json")],
        state.body.clone(),
    )
        .into_response()
}

// =====================================================================================
// Fixture wiring.
// =====================================================================================

struct SkillFixture {
    fixture: LifecycleFixture,
    provider: MockOpenAiServer,
    target: SkillTargetServer,
    agent_profile_id: Uuid,
}

/// How one skill row is configured. Every field is something the tests below vary.
struct SkillSeed {
    skill_key: String,
    kind: &'static str,
    status: &'static str,
    params_schema: Value,
    metadata: Value,
    method: &'static str,
    /// Appended to the target's origin. `{placeholder}` syntax preserved.
    path_template: String,
    /// Written verbatim, so a test can store an `allowed_host` the URL does not name.
    allowed_host: Option<String>,
    with_credential: bool,
    /// `base_url` of the provider the seeded credential hangs off. `None` means the skill
    /// target itself, which is the only configuration
    /// `domain::credential_binding_permits_host` permits; a test that wants the *refused*
    /// shape sets some other host here.
    credential_provider_base_url: Option<String>,
    executor: bool,
}

impl SkillSeed {
    fn tool(skill_key: &str, path_template: &str) -> Self {
        Self {
            skill_key: skill_key.to_string(),
            kind: "tool",
            status: "enabled",
            params_schema: json!({
                "type": "object",
                "properties": { "order_id": { "type": "string" } },
                "required": ["order_id"]
            }),
            metadata: json!({}),
            method: "GET",
            path_template: path_template.to_string(),
            allowed_host: None,
            with_credential: false,
            credential_provider_base_url: None,
            executor: true,
        }
    }

    fn guard(skill_key: &str, guard_policy: Value) -> Self {
        Self {
            skill_key: skill_key.to_string(),
            kind: "guard",
            status: "enabled",
            params_schema: json!({}),
            metadata: json!({ "guard": guard_policy }),
            method: "GET",
            path_template: "/unused".to_string(),
            allowed_host: None,
            with_credential: false,
            credential_provider_base_url: None,
            executor: false,
        }
    }
}

impl SkillFixture {
    async fn new(scripts: Vec<ProviderScript>, target_body: Value) -> Option<Self> {
        Self::with_target(scripts, StatusCode::OK, target_body).await
    }

    async fn with_target(
        scripts: Vec<ProviderScript>,
        target_status: StatusCode,
        target_body: Value,
    ) -> Option<Self> {
        let fixture = LifecycleFixture::with_settings(|settings| {
            // See this file's header for why this is on and what it does *not* relax.
            settings.skill_execution.allow_insecure_dev_urls = true;
            settings.skill_execution.maximum_tool_turns = 4;
        })
        .await?;
        let provider = MockOpenAiServer::start(scripts).await;
        let target = SkillTargetServer::start(target_status, target_body).await;
        fixture
            .add_provider(provider.base_url(), 10, RuntimePolicy::default())
            .await;

        let runtime = RuntimeAdminService::new(&fixture.state).expect("runtime admin service");
        let suffix = Uuid::now_v7().simple().to_string();
        let profile = runtime
            .create_agent_profile(
                &fixture.actor,
                &request_context(),
                AgentProfileCreateRequest {
                    profile_key: format!("skills-{suffix}"),
                    display_name: "Skill loop profile".to_string(),
                    preamble: Some("you may call tools".to_string()),
                    temperature: None,
                    max_tokens: None,
                    // Populated and still unread: only `skill_refs` puts tools on the wire.
                    // This is the end-to-end twin of the unit guard in
                    // `src/application/execution.rs`.
                    tool_policy: json!({
                        "tools": [{ "name": "from_tool_policy", "parameters": {} }]
                    }),
                    context_policy: json!({}),
                    memory_policy: json!({}),
                    metadata: json!({ "test_fixture": true }),
                },
            )
            .await
            .expect("create agent profile");
        let route = runtime
            .get_route_definition(&fixture.actor, fixture.route_id)
            .await
            .expect("get route definition");
        runtime
            .patch_route_definition(
                &fixture.actor,
                &request_context(),
                fixture.route_id,
                route.version,
                RouteDefinitionPatchRequest {
                    agent_profile_id: Some(profile.id),
                    ..RouteDefinitionPatchRequest::default()
                },
            )
            .await
            .expect("attach the agent profile to the route");

        Some(Self {
            fixture,
            provider,
            target,
            agent_profile_id: profile.id,
        })
    }

    /// Writes one `skills` row (plus its `skill_http_executors` row when the seed asks for
    /// one) and appends it to the agent profile's `skill_refs`.
    async fn seed_skill(&self, seed: SkillSeed) -> Uuid {
        let skill_id = Uuid::now_v7();
        sqlx::query(
            "insert into skills (id, skill_key, display_name, description, kind, \
             params_schema, tags, status, metadata) \
             values ($1, $2, $3, $4, $5, $6, '{}', $7, $8)",
        )
        .bind(skill_id)
        .bind(&seed.skill_key)
        .bind(format!("Skill {}", seed.skill_key))
        .bind(format!("call {}", seed.skill_key))
        .bind(seed.kind)
        .bind(&seed.params_schema)
        .bind(seed.status)
        .bind(&seed.metadata)
        .execute(&self.fixture.pool)
        .await
        .expect("seed skill row");

        if seed.executor {
            let url_template = format!("{}{}", self.target.origin(), seed.path_template);
            let allowed_host = seed
                .allowed_host
                .clone()
                .unwrap_or_else(|| "127.0.0.1".to_string());
            let credential_id = if seed.with_credential {
                let base_url = seed
                    .credential_provider_base_url
                    .clone()
                    .unwrap_or_else(|| self.target.origin());
                Some(self.seed_skill_credential(base_url).await)
            } else {
                None
            };
            sqlx::query(
                "insert into skill_http_executors (skill_id, method, url_template, \
                 allowed_host, header_template, credential_id, timeout_ms) \
                 values ($1, $2, $3, $4, $5, $6, 5000)",
            )
            .bind(skill_id)
            .bind(seed.method)
            .bind(&url_template)
            .bind(&allowed_host)
            .bind(json!({ "x-skill-header": "static" }))
            .bind(credential_id)
            .execute(&self.fixture.pool)
            .await
            .expect("seed skill executor row");
        }

        sqlx::query("update agent_profiles set skill_refs = skill_refs || $1::uuid where id = $2")
            .bind(skill_id)
            .bind(self.agent_profile_id)
            .execute(&self.fixture.pool)
            .await
            .expect("append to skill_refs");
        skill_id
    }

    /// A `provider_credentials` row the skill executor references (decision 21 — skills
    /// reuse that table's envelope rather than a second secret store). Written through the
    /// admin service so the secret is sealed exactly as production seals it, which is what
    /// makes the decrypt-at-call-time path real.
    async fn seed_skill_credential(&self, provider_base_url: String) -> Uuid {
        use moira::domain::{
            CredentialCreateRequest, CredentialScope, CredentialSecret, CredentialType,
            ProviderCreateRequest, ProviderType,
        };
        let admin =
            moira::application::AdminService::new(&self.fixture.state).expect("admin service");
        let suffix = Uuid::now_v7().simple().to_string();
        // A provider of its own: the skill's credential must not be reachable through the
        // completion provider's own resolution ladder, and giving it a separate provider row
        // is what proves the executor's explicit `credential_id` is what selected it.
        //
        // `base_url` names the skill target because a credential may only be sent to the
        // host its own provider declares (issue #253 finding 1,
        // `domain::credential_binding_permits_host`). This fixture models the legitimate
        // configuration — the operator registered the third-party endpoint as a provider and
        // put its key on that row — so that the negative case, a credential borrowed from
        // some *other* provider, is a refusal rather than the default.
        let provider = admin
            .create_provider(
                &self.fixture.actor,
                &request_context(),
                ProviderCreateRequest {
                    provider_type: ProviderType::Custom,
                    display_name: format!("Skill target {suffix}"),
                    base_url: Some(provider_base_url),
                    metadata: json!({ "test_fixture": true }),
                },
            )
            .await
            .expect("create skill credential provider");
        admin
            .create_credential(
                &self.fixture.actor,
                &request_context(),
                CredentialCreateRequest {
                    provider_id: provider.id,
                    credential_type: CredentialType::ApiKey,
                    scope: CredentialScope::Global,
                    secret: CredentialSecret::ApiKey {
                        api_key: SKILL_SECRET.to_string(),
                    },
                    display_name: Some("Skill credential".to_string()),
                    priority: 100,
                    expires_at: None,
                    metadata: json!({ "test_fixture": true }),
                },
            )
            .await
            .expect("create skill credential")
            .id
    }

    async fn execute(&self) -> moira::domain::ExecutionOutcome {
        self.fixture
            .execution_service()
            .execute_with_events(self.fixture.command(false))
            .await
            .expect("execution service call")
            .0
    }

    async fn execute_with_events(
        &self,
    ) -> (
        moira::domain::ExecutionOutcome,
        Vec<moira::domain::RuntimeEventEnvelope>,
    ) {
        self.fixture
            .execution_service()
            .execute_with_events(self.fixture.command(false))
            .await
            .expect("execution service call")
    }

    async fn shutdown(self) {
        self.provider.shutdown().await;
        self.target.shutdown().await;
    }
}

// =====================================================================================
// Cases.
// =====================================================================================

/// **The whole loop, end to end.** A profile with one enabled skill; the model asks for it;
/// `HttpSkillTool` calls the scripted target; the result comes back as a tool result and the
/// model's second turn is the answer Moira returns.
///
/// The assertions are ordered from the outside in — the caller's answer, then the provider's
/// two request bodies, then the target's one request — so a failure names the first stage
/// that broke rather than the last.
#[tokio::test]
async fn an_enabled_skill_is_advertised_called_and_round_tripped_into_the_answer() {
    let Some(fixture) = SkillFixture::new(
        vec![
            ProviderScript::ToolCallCompletion {
                call_id: "call_1".to_string(),
                name: "orders_get".to_string(),
                arguments: json!({ "order_id": "A-1" }),
            },
            ProviderScript::Completion {
                text: "order A-1 is shipped".to_string(),
            },
        ],
        json!({ "order_id": "A-1", "state": "shipped" }),
    )
    .await
    else {
        return;
    };
    fixture
        .seed_skill(SkillSeed::tool("orders_get", "/orders/{order_id}"))
        .await;

    let outcome = fixture.execute().await;
    assert_eq!(
        outcome.status,
        ExecutionStatus::Succeeded,
        "{:?}",
        outcome.failure
    );
    assert_eq!(outcome.output_text.as_deref(), Some("order A-1 is shipped"));

    let provider_requests = fixture.provider.requests().await;
    assert_eq!(
        provider_requests.len(),
        2,
        "the loop must make exactly two model calls: the tool-calling turn and the answer"
    );

    // Turn 1: the tool is advertised, and it is the skill's, not `tool_policy`'s.
    let first = &provider_requests[0].body;
    let advertised: Vec<&str> = first["tools"]
        .as_array()
        .expect("turn 1 must advertise tools")
        .iter()
        .map(|tool| tool["function"]["name"].as_str().expect("tool name"))
        .collect();
    assert_eq!(advertised, vec!["orders_get"], "{first}");
    assert!(
        !first.to_string().contains("from_tool_policy"),
        "issue #84 wired `skill_refs`, not `tool_policy`; nothing derived from the profile's \
         tool_policy may reach the wire: {first}"
    );

    // Turn 2: the assistant's tool call and exactly one tool result are in the history.
    let second = &provider_requests[1].body;
    let messages = second["messages"].as_array().expect("turn 2 messages");
    let assistant_tool_calls = messages
        .iter()
        .filter(|message| message["role"] == "assistant")
        .filter_map(|message| message.get("tool_calls"))
        .count();
    assert_eq!(
        assistant_tool_calls, 1,
        "turn 2 must replay exactly one assistant tool-call message: {second}"
    );
    let tool_results: Vec<&Value> = messages
        .iter()
        .filter(|message| message["role"] == "tool")
        .collect();
    assert_eq!(
        tool_results.len(),
        1,
        "every tool call must be answered exactly once: {second}"
    );
    assert_eq!(tool_results[0]["tool_call_id"], "call_1", "{second}");
    assert!(
        tool_results[0]["content"]
            .as_str()
            .unwrap_or_default()
            .contains("shipped"),
        "the target's body must reach the model as the tool result: {second}"
    );

    // The target saw the request `HttpSkillTool` built.
    let calls = fixture.target.calls();
    assert_eq!(calls.len(), 1, "the skill must be called exactly once");
    assert_eq!(calls[0].method, "GET");
    assert_eq!(
        calls[0].path, "/orders/A-1",
        "the path placeholder must be filled from the model's argument"
    );
    assert_eq!(calls[0].query, None);
    assert!(
        calls[0].request_id.is_some(),
        "the caller scope must reach the target as a correlation header"
    );

    fixture.shutdown().await;
}

/// **The credential is decrypted at call time and never leaves the request.**
///
/// Two properties in one case because they are the same property observed on two surfaces:
/// the target must receive the secret as a bearer token, and nothing that comes *back* —
/// the outcome, the answer, the provider's request bodies — may contain it.
#[tokio::test]
async fn a_skill_credential_reaches_the_target_and_nothing_else() {
    let Some(fixture) = SkillFixture::new(
        vec![
            ProviderScript::ToolCallCompletion {
                call_id: "call_1".to_string(),
                name: "orders_get".to_string(),
                arguments: json!({ "order_id": "A-2" }),
            },
            ProviderScript::Completion {
                text: "done".to_string(),
            },
        ],
        json!({ "state": "ok" }),
    )
    .await
    else {
        return;
    };
    let mut seed = SkillSeed::tool("orders_get", "/orders/{order_id}");
    seed.with_credential = true;
    fixture.seed_skill(seed).await;

    let outcome = fixture.execute().await;
    assert_eq!(
        outcome.status,
        ExecutionStatus::Succeeded,
        "{:?}",
        outcome.failure
    );

    let calls = fixture.target.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].authorization.as_deref(),
        Some(format!("Bearer {SKILL_SECRET}").as_str()),
        "the executor's credential_id must be resolved, decrypted and sent"
    );
    assert!(
        calls[0].body.is_none(),
        "a GET skill must not carry a request body: {:?}",
        calls[0].body
    );

    let outcome_text = format!("{outcome:?}");
    assert!(
        !outcome_text.contains(SKILL_SECRET),
        "the skill secret must never appear in an execution outcome"
    );
    for request in fixture.provider.requests().await {
        assert!(
            !request.body.to_string().contains(SKILL_SECRET),
            "the skill secret must never be sent to the model"
        );
    }

    fixture.shutdown().await;
}

/// Issue #253 finding 1, at execution time. A `skill_http_executors` row that names a
/// credential belonging to a provider which does not serve the executor's `allowed_host` is
/// refused before the credential is decrypted, so the secret is never assembled, never sent,
/// and the skill target is never called.
///
/// `AgentPlatformService::patch_executor` refuses that binding on the way in, so the only way
/// such a row exists is the way this test makes one — written straight to the table, which is
/// also how a row stored before that rule existed would look. That is exactly why the rule is
/// enforced in both places: a write-time-only check leaves every pre-existing row live.
///
/// The refusal is terminal rather than in-band, unlike the `allowed_host` mismatch below: a
/// missing or unusable skill credential fails the execution (`skill_credential`'s doc comment
/// gives the reasoning — calling unauthenticated would at best 401), and this is the same
/// class of "this skill cannot be used at all" as a dangling `skill_refs` entry.
#[tokio::test]
async fn a_skill_credential_from_a_provider_that_does_not_serve_the_host_is_never_sent() {
    let Some(fixture) = SkillFixture::new(
        vec![ProviderScript::Completion {
            text: "unreachable".to_string(),
        }],
        json!({ "state": "never-reached" }),
    )
    .await
    else {
        return;
    };
    let mut seed = SkillSeed::tool("orders_get", "/orders/{order_id}");
    seed.with_credential = true;
    // A public https host that is not the skill target — the SSRF guard has no objection to
    // it, which is the whole point: passing that guard is not entitlement.
    seed.credential_provider_base_url = Some("https://collector.elsewhere.test".to_string());
    fixture.seed_skill(seed).await;

    let outcome = fixture.execute().await;
    assert_eq!(
        outcome.status,
        ExecutionStatus::Failed,
        "an executor bound to a credential its provider cannot entitle must refuse the \
         execution rather than call unauthenticated"
    );
    assert_eq!(
        outcome.failure.as_ref().map(|failure| failure.class),
        Some(ExecutionFailureClass::SkillUnavailable),
        "{:?}",
        outcome.failure
    );
    assert!(
        fixture.target.calls().is_empty(),
        "the skill target must never be called"
    );
    assert_eq!(
        fixture.provider.call_count().await,
        0,
        "the tool set is built before the candidate loop, so this is refused before any \
         provider is contacted"
    );
    let outcome_text = format!("{outcome:?}");
    assert!(
        !outcome_text.contains(SKILL_SECRET),
        "the refused credential's plaintext must not appear anywhere in the outcome"
    );

    fixture.shutdown().await;
}

/// **Execution-time SSRF, at the point risk R20 names.** The stored `allowed_host` says one
/// thing and the URL says another, which is the state a compromised or careless write can
/// produce even though import and PATCH both derive `allowed_host` server-side.
///
/// `allow_insecure_dev_urls` is still on for this case, which is the point: the escape hatch
/// relaxes the *address space*, never the "a skill may only talk to its own host" rule. The
/// call is refused in-band, so the model gets a classified failure and still answers.
#[tokio::test]
async fn a_skill_call_cannot_be_redirected_to_another_host() {
    let Some(fixture) = SkillFixture::new(
        vec![
            ProviderScript::ToolCallCompletion {
                call_id: "call_1".to_string(),
                name: "orders_get".to_string(),
                arguments: json!({ "order_id": "A-3" }),
            },
            ProviderScript::Completion {
                text: "i could not reach that tool".to_string(),
            },
        ],
        json!({ "state": "unreachable" }),
    )
    .await
    else {
        return;
    };
    let mut seed = SkillSeed::tool("orders_get", "/orders/{order_id}");
    seed.allowed_host = Some("api.elsewhere.test".to_string());
    fixture.seed_skill(seed).await;

    let (outcome, events) = fixture.execute_with_events().await;
    assert_eq!(
        outcome.status,
        ExecutionStatus::Succeeded,
        "a refused tool call stays in-band: {:?}",
        outcome.failure
    );
    assert!(
        fixture.target.calls().is_empty(),
        "the request must be refused before it is issued"
    );

    let tool_results: Vec<&Value> = events
        .iter()
        .filter(|event| event.payload.get("tool_name").is_some())
        .map(|event| &event.payload)
        .collect();
    assert_eq!(tool_results.len(), 1, "{events:?}");
    assert_eq!(tool_results[0]["outcome"], "error", "{tool_results:?}");
    assert_eq!(
        tool_results[0]["failure_kind"], "permission_denied",
        "an address refusal must classify as permission_denied, not as a transport error"
    );

    fixture.shutdown().await;
}

/// **Guards run before dispatch and are fail-closed.** The guard's allow-list does not name
/// the tool, so the call is refused without the target ever being contacted — and the
/// denial reaches the model as a keyed result it can act on.
#[tokio::test]
async fn a_guard_refuses_a_skill_call_before_it_is_dispatched() {
    let Some(fixture) = SkillFixture::new(
        vec![
            ProviderScript::ToolCallCompletion {
                call_id: "call_1".to_string(),
                name: "orders_get".to_string(),
                arguments: json!({ "order_id": "A-4" }),
            },
            ProviderScript::Completion {
                text: "i am not allowed to do that".to_string(),
            },
        ],
        json!({ "state": "never reached" }),
    )
    .await
    else {
        return;
    };
    fixture
        .seed_skill(SkillSeed::tool("orders_get", "/orders/{order_id}"))
        .await;
    fixture
        .seed_skill(SkillSeed::guard(
            "orders_guard",
            json!({ "allowed_skill_keys": ["something_else"] }),
        ))
        .await;

    let (outcome, events) = fixture.execute_with_events().await;
    assert_eq!(
        outcome.status,
        ExecutionStatus::Succeeded,
        "{:?}",
        outcome.failure
    );
    assert!(
        fixture.target.calls().is_empty(),
        "a guard denial must short-circuit before the outbound call"
    );

    let denial = events
        .iter()
        .map(|event| &event.payload)
        .find(|payload| payload.get("guard_reason").map(Value::is_string) == Some(true))
        .expect("a guard denial must be observable as a runtime event");
    assert_eq!(denial["outcome"], "denied", "{denial}");
    assert_eq!(denial["guard_key"], "orders_guard", "{denial}");
    assert_eq!(denial["guard_reason"], "skill_not_allowed", "{denial}");

    let second = &fixture.provider.requests().await[1].body;
    assert!(
        second.to_string().contains("skill_guard_denied"),
        "the model must be told, in a keyed form, that a guard refused the call: {second}"
    );

    fixture.shutdown().await;
}

/// **A guard whose policy cannot be read denies everything it governs.** The fail-closed
/// arm, end to end: an operator's typo must not silently switch the control off.
#[tokio::test]
async fn an_unreadable_guard_policy_denies_every_call_it_governs() {
    let Some(fixture) = SkillFixture::new(
        vec![
            ProviderScript::ToolCallCompletion {
                call_id: "call_1".to_string(),
                name: "orders_get".to_string(),
                arguments: json!({ "order_id": "A-5" }),
            },
            ProviderScript::Completion {
                text: "refused".to_string(),
            },
        ],
        json!({ "state": "never reached" }),
    )
    .await
    else {
        return;
    };
    fixture
        .seed_skill(SkillSeed::tool("orders_get", "/orders/{order_id}"))
        .await;
    // `metadata` has no `guard` object at all — the shape a hand-authored guard row starts
    // in, and the one an operator most plausibly forgets to fill.
    let mut broken = SkillSeed::guard("broken_guard", json!({}));
    broken.metadata = json!({ "note": "no guard object here" });
    fixture.seed_skill(broken).await;

    let (outcome, events) = fixture.execute_with_events().await;
    assert_eq!(outcome.status, ExecutionStatus::Succeeded);
    assert!(fixture.target.calls().is_empty());
    let denial = events
        .iter()
        .map(|event| &event.payload)
        .find(|payload| payload.get("guard_reason").map(Value::is_string) == Some(true))
        .expect("an unreadable guard must still produce a denial event");
    assert_eq!(denial["guard_reason"], "policy_unreadable", "{denial}");

    fixture.shutdown().await;
}

/// **A `skill_refs` entry that cannot be used refuses the execution.** Fail-closed, the same
/// decision issue #79 took for a dangling `agent_profile_id`: an agent quietly missing a
/// skill it was configured with answers wrongly and nobody is told.
///
/// Three seeds in one case because the property is the *set*: an implementation that
/// refused only the missing id, or only the draft row, would leave the other two silently
/// dropping a configured skill.
#[tokio::test]
async fn an_unusable_skill_reference_refuses_the_execution() {
    for seed in [
        // Enabled, but a tool with no executor row: nothing to call.
        {
            let mut seed = SkillSeed::tool("orphan_tool", "/orders/{order_id}");
            seed.executor = false;
            seed
        },
        // Never reviewed (§5 decision 22's fail-closed default).
        {
            let mut seed = SkillSeed::tool("draft_tool", "/orders/{order_id}");
            seed.status = "draft";
            seed
        },
        // Switched off by an operator.
        {
            let mut seed = SkillSeed::tool("disabled_tool", "/orders/{order_id}");
            seed.status = "disabled";
            seed
        },
    ] {
        let skill_key = seed.skill_key.clone();
        let Some(fixture) = SkillFixture::new(
            vec![ProviderScript::Completion {
                text: "must never be reached".to_string(),
            }],
            json!({}),
        )
        .await
        else {
            return;
        };
        fixture.seed_skill(seed).await;

        let outcome = fixture.execute().await;
        assert_eq!(
            outcome.status,
            ExecutionStatus::Failed,
            "{skill_key} must refuse the execution"
        );
        assert_eq!(
            outcome.failure.as_ref().map(|failure| failure.class),
            Some(ExecutionFailureClass::SkillUnavailable),
            "{skill_key}: {:?}",
            outcome.failure
        );
        assert_eq!(
            fixture.provider.call_count().await,
            0,
            "{skill_key} must be refused before any provider is contacted"
        );
        fixture.shutdown().await;
    }
}

/// **A `skill_refs` id no live row answers is refused too**, and separately from the three
/// above: this one never had a `skills` row at all, which is the state a soft-delete leaves
/// behind and the one a set-based implementation is most likely to skip silently.
#[tokio::test]
async fn a_dangling_skill_reference_refuses_the_execution() {
    let Some(fixture) = SkillFixture::new(
        vec![ProviderScript::Completion {
            text: "must never be reached".to_string(),
        }],
        json!({}),
    )
    .await
    else {
        return;
    };
    sqlx::query("update agent_profiles set skill_refs = array[$1::uuid] where id = $2")
        .bind(Uuid::now_v7())
        .bind(fixture.agent_profile_id)
        .execute(&fixture.fixture.pool)
        .await
        .expect("point skill_refs at an id nothing answers");

    let outcome = fixture.execute().await;
    assert_eq!(
        outcome.failure.as_ref().map(|failure| failure.class),
        Some(ExecutionFailureClass::SkillUnavailable),
        "{:?}",
        outcome.failure
    );
    assert_eq!(fixture.provider.call_count().await, 0);

    fixture.shutdown().await;
}

/// **The turn budget terminates a model that will not stop calling tools.** Every scripted
/// turn is another tool call, so the loop can only end by exhausting the budget — and it
/// must end as `DeadlineExceeded` rather than looping until the execution timeout, which
/// would be indistinguishable from a slow provider.
///
/// The two counts are asserted together because the boundary between them is the property
/// (issue #252 finding 3): the budget is `4` **model calls**, and the fourth of those has no
/// successor to read a tool result, so it must not dispatch. Four dispatches would mean the
/// last one fired a real request — `HttpMethod` admits `POST`/`PUT`/`PATCH`/`DELETE` against
/// an operator's third-party API — purely to have its result discarded by the failure below,
/// with the caller told only `504`.
#[tokio::test]
async fn a_model_that_never_stops_calling_tools_exhausts_the_turn_budget() {
    let scripts = (0..8)
        .map(|index| ProviderScript::ToolCallCompletion {
            call_id: format!("call_{index}"),
            name: "orders_get".to_string(),
            arguments: json!({ "order_id": format!("A-{index}") }),
        })
        .collect();
    let Some(fixture) = SkillFixture::new(scripts, json!({ "state": "shipped" })).await else {
        return;
    };
    fixture
        .seed_skill(SkillSeed::tool("orders_get", "/orders/{order_id}"))
        .await;

    let outcome = fixture.execute().await;
    assert_eq!(outcome.status, ExecutionStatus::Failed);
    assert_eq!(
        outcome.failure.as_ref().map(|failure| failure.class),
        Some(ExecutionFailureClass::DeadlineExceeded),
        "{:?}",
        outcome.failure
    );
    assert_eq!(
        fixture.provider.call_count().await,
        4,
        "the loop must stop at `skill_execution.maximum_tool_turns` model calls"
    );
    assert_eq!(
        fixture.target.calls().len(),
        3,
        "the fourth turn asked for a tool too, but no turn is left to read the result, so it \
         must not have been dispatched: {:?}",
        fixture.target.calls()
    );

    fixture.shutdown().await;
}

/// **A loop that runs out of turns still owes an account of what it did (issue #252).**
///
/// The turn-budget exit above is the failure that discards the most: by the time it fires,
/// `maximum_tool_turns` provider calls have been made and billed, and every tool call they
/// asked for has already left the process as real outbound HTTP — `HttpMethod` admits `POST`,
/// `PUT`, `PATCH` and `DELETE`, so those can be mutations of an operator's third-party API.
/// Returning a bare `ExecutionFailure` dropped both facts: no `ToolResult` event named the
/// dispatches, and the attempt recorded `UsageSummary::default()`, which means *unknown* and
/// so skipped the `usage_records` row entirely — four calls invoiced, none metered.
///
/// Deliberately not asserted here: *how many* dispatches there should be. The last permitted
/// turn dispatching at all is its own question (issue #252 finding 3); what this case pins is
/// that every dispatch which did happen is accounted for.
#[tokio::test]
async fn an_exhausted_turn_budget_still_reports_its_tool_calls_and_its_tokens() {
    let scripts = (0..8)
        .map(|index| ProviderScript::ToolCallCompletion {
            call_id: format!("call_{index}"),
            name: "orders_get".to_string(),
            arguments: json!({ "order_id": format!("B-{index}") }),
        })
        .collect();
    let Some(fixture) = SkillFixture::new(scripts, json!({ "state": "shipped" })).await else {
        return;
    };
    fixture
        .seed_skill(SkillSeed::tool("orders_get", "/orders/{order_id}"))
        .await;

    let (outcome, events) = fixture.execute_with_events().await;
    assert_eq!(
        outcome.failure.as_ref().map(|failure| failure.class),
        Some(ExecutionFailureClass::DeadlineExceeded),
        "{:?}",
        outcome.failure
    );

    let dispatched = fixture.target.calls().len();
    assert!(
        dispatched > 0,
        "the scenario is only meaningful if the loop really called the target"
    );
    let tool_results: Vec<&Value> = events
        .iter()
        .filter(|event| event.payload.get("tool_name").is_some())
        .map(|event| &event.payload)
        .collect();
    assert_eq!(
        tool_results.len(),
        dispatched,
        "every request that reached the target must be on the runtime-event surface, even \
         though the attempt around it failed: {events:?}"
    );
    assert!(
        tool_results
            .iter()
            .all(|payload| payload["outcome"] == "success"),
        "{tool_results:?}"
    );

    // Four completions at prompt 4 / completion 2 each — the scripted mock's fixed figures,
    // so the total is arithmetic rather than a guess.
    let attempt = outcome.attempts.first().expect("one attempt was made");
    assert_eq!(
        attempt.usage.total_tokens,
        Some(24),
        "the four billed completions must be metered, not recorded as unknown: {:?}",
        attempt.usage
    );
    assert_eq!(attempt.usage.input_tokens, Some(16), "{:?}", attempt.usage);
    assert_eq!(attempt.usage.output_tokens, Some(8), "{:?}", attempt.usage);
    assert_eq!(
        outcome.usage.total_tokens,
        Some(24),
        "the outcome reports what its own attempts reported"
    );

    let metered: i64 =
        sqlx::query_scalar("select count(*) from usage_records where execution_id = $1")
            .bind(outcome.execution_id)
            .fetch_one(&fixture.fixture.pool)
            .await
            .expect("count usage records");
    assert_eq!(
        metered, 1,
        "an all-`None` usage skips `insert_usage_record`, so dropping the counts also dropped \
         the billing row for four calls the provider will invoice"
    );

    fixture.shutdown().await;
}

/// **A loop that succeeds meters every turn it made, not just the one that answered
/// (issue #252 finding 4).**
///
/// The sibling above pins the failure path. This is the success path, where the under-count
/// was larger and quieter: `run_tool_loop` overwrote its output each turn and reported only
/// the last one, so a four-turn execution put three billed completions outside
/// `usage_records` — and the hidden ones are the *expensive* ones, because each later turn
/// re-sends the whole grown history plus every tool result. The row is per attempt and this
/// attempt made all of the calls, so the sum is the figure that row is for.
///
/// The scripted mock bills a tool-calling turn 4/2 and an answering turn 2/1, so the correct
/// total is arithmetic rather than a guess — and, importantly, the two turns bill *different*
/// amounts, so "reports the last turn" and "reports the sum" cannot coincide.
#[tokio::test]
async fn a_successful_loop_meters_every_turn_not_only_the_one_that_answered() {
    let Some(fixture) = SkillFixture::new(
        vec![
            ProviderScript::ToolCallCompletion {
                call_id: "call_1".to_string(),
                name: "orders_get".to_string(),
                arguments: json!({ "order_id": "D-1" }),
            },
            ProviderScript::Completion {
                text: "order D-1 is shipped".to_string(),
            },
        ],
        json!({ "state": "shipped" }),
    )
    .await
    else {
        return;
    };
    fixture
        .seed_skill(SkillSeed::tool("orders_get", "/orders/{order_id}"))
        .await;

    let outcome = fixture.execute().await;
    assert_eq!(
        outcome.status,
        ExecutionStatus::Succeeded,
        "{:?}",
        outcome.failure
    );
    assert_eq!(
        fixture.provider.call_count().await,
        2,
        "the scenario is only meaningful if both turns really billed"
    );
    assert_eq!(fixture.target.calls().len(), 1);

    let attempt = outcome.attempts.first().expect("one attempt was made");
    assert_eq!(
        attempt.usage.total_tokens,
        Some(9),
        "6 for the tool-calling turn plus 3 for the answer; reporting only the answer's 3 \
         hides the turn that carried the tools: {:?}",
        attempt.usage
    );
    assert_eq!(attempt.usage.input_tokens, Some(6), "{:?}", attempt.usage);
    assert_eq!(attempt.usage.output_tokens, Some(3), "{:?}", attempt.usage);
    assert_eq!(
        outcome.usage.total_tokens,
        Some(9),
        "the outcome reports what its own attempts reported"
    );

    // `usage_records` is the surface `infra::repositories::public` reports spend from, so the
    // wire figure being right is not enough — the persisted row has to carry the same total.
    let metered: (Option<i64>, Option<i64>, Option<i64>) = sqlx::query_as(
        "select input_tokens, output_tokens, total_tokens from usage_records \
         where execution_id = $1",
    )
    .bind(outcome.execution_id)
    .fetch_one(&fixture.fixture.pool)
    .await
    .expect("the attempt must have written exactly one usage row");
    assert_eq!(
        metered,
        (Some(6), Some(3), Some(9)),
        "the billing row must carry both completions, not the last one"
    );

    fixture.shutdown().await;
}

/// **The other failure exit: the provider dies on a later turn.**
///
/// Turn 1 asks for the tool and the request really is issued; turn 2 answers `400`, which ends
/// the attempt. The tool call is already spent and turn 1 is already billed, and both used to
/// vanish with the `?` that propagated the provider's failure.
#[tokio::test]
async fn a_provider_failure_mid_loop_keeps_the_turn_it_already_spent() {
    let Some(fixture) = SkillFixture::new(
        vec![
            ProviderScript::ToolCallCompletion {
                call_id: "call_1".to_string(),
                name: "orders_get".to_string(),
                arguments: json!({ "order_id": "C-1" }),
            },
            ProviderScript::HttpError {
                status: StatusCode::BAD_REQUEST,
                body: json!({ "error": { "message": "no" } }).to_string(),
            },
        ],
        json!({ "state": "shipped" }),
    )
    .await
    else {
        return;
    };
    fixture
        .seed_skill(SkillSeed::tool("orders_get", "/orders/{order_id}"))
        .await;

    let (outcome, events) = fixture.execute_with_events().await;
    assert_eq!(
        outcome.status,
        ExecutionStatus::Failed,
        "{:?}",
        outcome.failure
    );
    assert_eq!(
        fixture.target.calls().len(),
        1,
        "turn 1's tool call really did reach the target before the provider failed"
    );

    let tool_results: Vec<&Value> = events
        .iter()
        .filter(|event| event.payload.get("tool_name").is_some())
        .map(|event| &event.payload)
        .collect();
    assert_eq!(
        tool_results.len(),
        1,
        "the dispatch survives the provider failure that followed it: {events:?}"
    );

    let attempt = outcome.attempts.first().expect("one attempt was made");
    assert_eq!(
        attempt.usage.total_tokens,
        Some(6),
        "turn 1 answered and was billed; only turn 2 failed: {:?}",
        attempt.usage
    );

    fixture.shutdown().await;
}

/// **Structured output plus skills is refused, not silently mangled (finding F48).**
///
/// `rig-core` drops `response_format` whenever tools are advertised on turn 1, with no
/// warning. Sending the request anyway would produce prose and a `StructuredOutputInvalid`
/// one layer later, blaming the caller's schema for a drop rig performed. The unit guard
/// `resolved_skill_refs_do_reach_the_wire_and_take_the_schema_with_them` proves the drop on
/// Rig's own encoder; this proves Moira refuses the combination before it can happen.
#[tokio::test]
async fn structured_output_combined_with_skills_is_refused_before_the_provider_is_called() {
    let Some(fixture) = SkillFixture::new(
        vec![ProviderScript::Completion {
            text: "must never be reached".to_string(),
        }],
        json!({}),
    )
    .await
    else {
        return;
    };
    fixture
        .seed_skill(SkillSeed::tool("orders_get", "/orders/{order_id}"))
        .await;

    let mut command = fixture.fixture.command(false);
    command.options.output_schema = Some(json!({
        "type": "object",
        "properties": { "answer": { "type": "string" } },
        "required": ["answer"]
    }));
    let outcome = fixture
        .fixture
        .execution_service()
        .execute_with_events(command)
        .await
        .expect("execution service call")
        .0;
    assert_eq!(
        outcome.failure.as_ref().map(|failure| failure.class),
        Some(ExecutionFailureClass::InvalidExecutionRequest),
        "{:?}",
        outcome.failure
    );
    assert_eq!(fixture.provider.call_count().await, 0);

    fixture.shutdown().await;
}

/// **Streaming plus skills is refused while streamed tools are deferred.**
///
/// The streamed path surfaces `ToolCallStarted`/`ToolCallDelta` items but has no way to feed
/// a tool *result* back into a new stream, so advertising tools there would offer the model
/// something Moira cannot satisfy. Refused loudly rather than half-built; this case is what
/// goes red when streamed tools are implemented, which is the intended prompt to delete it.
#[tokio::test]
async fn streaming_combined_with_skills_is_refused_while_streamed_tools_are_deferred() {
    let Some(fixture) = SkillFixture::new(
        vec![ProviderScript::Stream {
            deltas: vec!["must never be reached".to_string()],
        }],
        json!({}),
    )
    .await
    else {
        return;
    };
    fixture
        .seed_skill(SkillSeed::tool("orders_get", "/orders/{order_id}"))
        .await;

    let outcome = fixture
        .fixture
        .execution_service()
        .execute_with_events(fixture.fixture.command(true))
        .await
        .expect("execution service call")
        .0;
    assert_eq!(
        outcome.failure.as_ref().map(|failure| failure.class),
        Some(ExecutionFailureClass::InvalidExecutionRequest),
        "{:?}",
        outcome.failure
    );
    assert_eq!(fixture.provider.call_count().await, 0);

    fixture.shutdown().await;
}

/// **A model-authored argument cannot escape its path segment.** The unit test proves the
/// encoding; this proves what the target actually received, which is the only place a
/// normalisation performed by `reqwest`, `url` or hyper on the way out would show up.
#[tokio::test]
async fn a_traversal_argument_reaches_the_target_as_one_encoded_segment() {
    let Some(fixture) = SkillFixture::new(
        vec![
            ProviderScript::ToolCallCompletion {
                call_id: "call_1".to_string(),
                name: "orders_get".to_string(),
                arguments: json!({ "order_id": "../../admin" }),
            },
            ProviderScript::Completion {
                text: "done".to_string(),
            },
        ],
        json!({ "state": "ok" }),
    )
    .await
    else {
        return;
    };
    fixture
        .seed_skill(SkillSeed::tool("orders_get", "/orders/{order_id}"))
        .await;

    let outcome = fixture.execute().await;
    assert_eq!(
        outcome.status,
        ExecutionStatus::Succeeded,
        "{:?}",
        outcome.failure
    );
    let calls = fixture.target.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].path, "/orders/%2E%2E%2F%2E%2E%2Fadmin",
        "the argument must arrive as one percent-encoded segment, never as a traversal"
    );

    fixture.shutdown().await;
}

/// **A profile with no `skill_refs` sends no tools at all.** The control, and it is
/// load-bearing: without it, an implementation that advertised a fixed tool list for every
/// agent profile would satisfy every case above.
#[tokio::test]
async fn a_profile_without_skill_refs_still_sends_no_tools() {
    let Some(fixture) = SkillFixture::new(
        vec![ProviderScript::Completion {
            text: "plain answer".to_string(),
        }],
        json!({}),
    )
    .await
    else {
        return;
    };

    let outcome = fixture.execute().await;
    assert_eq!(
        outcome.status,
        ExecutionStatus::Succeeded,
        "{:?}",
        outcome.failure
    );
    let body = &fixture.provider.requests().await[0].body;
    assert!(body.get("tools").is_none(), "{body}");
    assert!(body.get("tool_choice").is_none(), "{body}");

    fixture.shutdown().await;
}
