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
//!    the route pointing at it. That is announced as a runtime event, a `warn!` and an audit
//!    row, and the guard asserts on the event and the row: logs are easy to assert and easy to
//!    lose.
//! 7. **The soft-delete twin of case 6**, because `delete_agent_profile` is a different admin
//!    method and is the one that could have hard-deleted the row.
//! 8. **The streaming twin of case 6.** Case 4's justification — "they are separate arms" —
//!    applies to this property too, and a per-path pairing with an empty cell is HANDOFF
//!    §3.4's twelfth entry.
//! 9. **The control for cases 6–8, and it is load-bearing.** A route that never had a
//!    profile must announce *nothing* and must keep executing. Without it, a fix that fired on
//!    `agent_profile_id` being `Some` rather than on the lookup failing would satisfy every
//!    guard above — and, since issue #79, would refuse every profile-less route in the
//!    deployment.
//! 10. **Issue #79 — the execution is refused.** The maintainer took F50's open product
//!     decision fail-closed on 2026-08-06: a route naming a missing or disabled agent profile
//!     fails the request rather than serving it without the profile's preamble. This case is
//!     the replacement for the `documents_` case that pinned the old lenient behaviour.
//! 11. **`409 agent_profile_disabled`** on `POST /api/v1/responses` — the caller-visible
//!     contract for a profile the operator switched off.
//! 12. **`404 agent_profile_not_found`** on the same endpoint for a soft-deleted profile. Cases
//!     11 and 12 are a pair: either alone is satisfied by an implementation that always answers
//!     one status, and the distinction is the thing worth guarding.
//! 13. **The streamed refusal**, which is a terminal `response.failed` event rather than an
//!     HTTP status, because the response head is written before the execution starts.
//! 14. **The global-default route selection arm carries the profile too.** Cases 1–13 all name
//!     the route explicitly, so they only ever drive `RouteSelectionReason::ExplicitHint`.
//! 15. **The rule-matched arm carries it too** — the third of the three arms.
//!
//! Cases 6–9 are the observability half and survive the fail-closed decision unchanged; cases
//! 10–13 are the refusal half. Keeping them apart is HANDOFF §3.4's `documents_`/`guards_`
//! distinction: the observability guards did not go red when the decision landed, which is
//! exactly what they were split out to achieve.
//!
//! # Cases 14 and 15 — issue #155 A1, the arms nothing drove
//!
//! `DefaultTaskRouter::select_route` builds a `RouteDecision` in three places, and each one
//! copies `agent_profile_id: route.agent_profile_id` independently. Until issue #155 every case
//! in this file passed a `route` in the request body, which takes the first of the three and
//! returns before the other two are reached — so changing either of the remaining arms to
//! `agent_profile_id: None` was a one-token edit that made every rule-matched and default-routed
//! request in the deployment run without its profile's preamble, with this entire suite green.
//! That is the exact fail-open issue #79 rejected, arrived at from the routing side instead of
//! the resolution side.
//!
//! Both cases assert the `reason` on the `route_selected` event as well as the wire body. The
//! reason is what makes them arms rather than repetitions of case 1: without it, a change that
//! quietly routed them back through the explicit-hint arm would keep them green while leaving
//! the arm they were written for untested again.

mod support;

use std::{sync::Arc, time::Duration};

use axum::http::StatusCode;
use moira::{
    application::RuntimeAdminService,
    domain::{
        AgentProfileCreateRequest, AgentProfileRecord, DiagnosticExecutionRequest,
        ExecutionOptions, RouteDefinitionCreateRequest, RouteDefinitionPatchRequest,
        RouteSelectionStrategy, RoutingPolicyCreateRequest,
    },
};
use serde_json::{Value, json};
use support::{
    LifecycleFixture, MoiraHttpServer, ProviderFixture, RuntimePolicy,
    mock_openai::{MockOpenAiServer, ProviderScript},
    request_context,
};
use uuid::Uuid;

const WAIT: Duration = Duration::from_secs(15);

/// The budget for a public-plane request, which is deliberately much larger than [`WAIT`].
///
/// These cases each drive two complete end-to-end requests — a successful control and the refusal
/// — on top of creating an application policy and a consumer key, while every other test in this
/// binary runs concurrently against the same machine and the same PostgreSQL instance. At `WAIT`
/// the disabled-profile case failed under that load and passed in 8s when run alone, which is a
/// timeout measuring the machine rather than the code.
///
/// It is not a latency assertion and must not be read as one. Nothing here is allowed to pass
/// *because* it was slow; the value exists only so a genuine hang fails the test instead of
/// hanging the suite.
const PUBLIC_WAIT: Duration = Duration::from_secs(120);

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

/// A prompt that trips `DefaultTaskRouter`'s rule-match arm, which matches on the substring
/// `"code"` in the first message's text. [`PROMPT`] deliberately does not contain it, so every
/// other case in this file — including the global-default one — cannot take that arm by accident.
const RULE_MATCHING_PROMPT: &str = "write me some code";

/// The one route key the rule-match arm looks up. Not a name this suite is free to choose:
/// `select_route` hardcodes it.
const RULE_MATCHED_ROUTE_KEY: &str = "coding";

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
    /// The provider, model and credential [`LifecycleFixture::add_provider`] created.
    ///
    /// Kept so a case can write a *second* routing policy pointing at the same mock — which is
    /// what makes a route other than the fixture's own route executable, and therefore what makes
    /// the rule-matched selection arm reachable at all.
    provider_fixture: ProviderFixture,
}

impl Case {
    async fn new(profile: Profile, scripts: Vec<ProviderScript>) -> Option<Self> {
        let fixture = LifecycleFixture::new().await?;
        let provider = MockOpenAiServer::start(scripts).await;
        let provider_fixture = fixture
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
            provider_fixture,
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

    /// A diagnostic execution naming this fixture's route — the explicit-hint arm.
    async fn diagnose(&self, stream: bool, options: ExecutionOptions) -> (StatusCode, Value) {
        self.diagnose_routed(
            Some(self.fixture.route_key.clone()),
            PROMPT,
            stream,
            options,
        )
        .await
    }

    /// As [`Self::diagnose`], with the route hint and the prompt under the caller's control.
    ///
    /// Both are what select the arm of `DefaultTaskRouter::select_route` the request takes:
    /// `route: None` skips the explicit-hint arm entirely, and the prompt's text is what the
    /// rule-match arm reads. Nothing else in this file needs either, which is exactly why the
    /// other two arms went untested until issue #155.
    async fn diagnose_routed(
        &self,
        route: Option<String>,
        prompt: &str,
        stream: bool,
        options: ExecutionOptions,
    ) -> (StatusCode, Value) {
        let request = DiagnosticExecutionRequest {
            application_id: Some(self.fixture.application_id),
            external_tenant_id: None,
            external_user_id: Some("f49-agent-profile".to_string()),
            route,
            provider_id: None,
            provider_model_id: None,
            credential_id: None,
            prompt: prompt.to_string(),
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

    /// How many requests have reached the mock provider so far.
    ///
    /// The refusal cases assert this *does not move* across the request under test, having first
    /// moved it with a request that succeeded. "Zero requests arrived" on its own is what a
    /// broken fixture looks like too — a provider that was never wired, a route that never
    /// resolved — so the count is taken before and after and the before is non-zero.
    async fn provider_request_count(&self) -> usize {
        self.provider.requests().await.len()
    }

    /// Makes this fixture's own route the one `get_default_route` returns, by disabling the
    /// `general` route migration `0005` seeds.
    ///
    /// The seeded route is why a hintless request could not otherwise reach this fixture's
    /// provider: `get_default_route` sorts `general` ahead of everything else, and no routing
    /// policy points at it, so a request routed there fails at admission with `no_eligible_model`
    /// long before anything about agent profiles is decided. Disabling it — rather than pointing
    /// a second routing policy at it — keeps the route under test the *same* route cases 1–13 use,
    /// with the same profile and the same provider, so the only thing that differs between case 1
    /// and case 14 is the selection arm.
    ///
    /// The premise is asserted rather than assumed, and deliberately **not** by re-running the
    /// repository's `order by`: a test that restated production's tie-break would agree with a
    /// broken one. With exactly one active route left, every possible default-selection rule has
    /// to return it.
    async fn make_this_fixtures_route_the_only_default(&self) {
        let seeded: Option<(Uuid, i64)> = sqlx::query_as(
            "select id, version from route_definitions where route_key = 'general' \
             and status = 'active' and deleted_at is null",
        )
        .fetch_optional(&self.fixture.pool)
        .await
        .expect("read the seeded general route");
        let (id, version) = seeded.expect(
            "migration 0005 seeds an active 'general' route, and get_default_route prefers it \
             over every other; if it is gone this helper is no longer doing anything and the \
             case below would be silently driving some other route",
        );
        RuntimeAdminService::new(&self.fixture.state)
            .expect("runtime admin service")
            .set_route_definition_enabled(
                &self.fixture.actor,
                &request_context(),
                id,
                version,
                false,
            )
            .await
            .expect("disable the seeded general route");

        let active: Vec<String> = sqlx::query_scalar(
            "select route_key from route_definitions where status = 'active' \
             and deleted_at is null",
        )
        .fetch_all(&self.fixture.pool)
        .await
        .expect("read the active routes");
        assert_eq!(
            active,
            vec![self.fixture.route_key.clone()],
            "exactly one active route must remain, and it must be the one carrying the agent \
             profile — otherwise 'the default route' is whichever route the repository's \
             tie-break happens to pick and this case asserts nothing about the profile"
        );
    }

    /// Creates the `coding` route the rule-match arm looks up, carrying this case's profile, and
    /// gives it a routing policy pointing at the same mock provider.
    ///
    /// A second routing policy rather than a moved one: this fixture's original route keeps
    /// working, so a request that fell back to it would still succeed — and would be caught by
    /// the `route_key` assertion rather than by a routing failure that could be mistaken for a
    /// fixture problem.
    async fn add_a_rule_matched_route_carrying_the_same_profile(&self) -> Uuid {
        let profile = self
            .profile
            .as_ref()
            .expect("only a case with an attached profile can share one");
        let admin = RuntimeAdminService::new(&self.fixture.state).expect("runtime admin service");
        let route = admin
            .create_route_definition(
                &self.fixture.actor,
                &request_context(),
                RouteDefinitionCreateRequest {
                    route_key: RULE_MATCHED_ROUTE_KEY.to_string(),
                    display_name: "F49 rule-matched route".to_string(),
                    description: None,
                    selection_strategy: RouteSelectionStrategy::Default,
                    agent_profile_id: Some(profile.id),
                    metadata: json!({ "test_fixture": true }),
                },
            )
            .await
            .expect("create the rule-matched route");
        assert_eq!(
            route.agent_profile_id,
            Some(profile.id),
            "the rule-matched route must actually carry the profile, or this case degenerates \
             into the no-profile control with a different route key"
        );
        admin
            .create_routing_policy(
                &self.fixture.actor,
                &request_context(),
                RoutingPolicyCreateRequest {
                    application_id: Some(self.fixture.application_id),
                    external_tenant_id: None,
                    route_id: route.id,
                    provider_id: self.provider_fixture.provider_id,
                    provider_model_id: self.provider_fixture.model_id,
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
                    timeout_ms: Some(RuntimePolicy::default().request_timeout_ms),
                    retry_policy: json!({}),
                    metadata: json!({ "test_fixture": true }),
                },
            )
            .await
            .expect("point the rule-matched route at the same mock provider");
        route.id
    }

    /// Enables the public plane for this fixture's application and returns a consumer key.
    ///
    /// The refusal has to be observed where a *caller* sees it, not only through the admin
    /// diagnostic endpoint: the diagnostic endpoint answers `200` with an outcome document
    /// whatever the execution did, so it can show that the execution failed but it cannot show
    /// the HTTP status or the error code the public contract promises.
    async fn public_key(&self) -> String {
        self.fixture.enable_public_streaming().await
    }

    /// `POST /api/v1/responses` — the non-streaming public path.
    async fn create_public_response(&self, key: &str) -> (StatusCode, Value) {
        let response = tokio::time::timeout(
            PUBLIC_WAIT,
            self.client
                .post(format!("{}/api/v1/responses", self.moira.base_url))
                .header("x-consumer-key", key)
                .header("x-request-id", format!("f50-{}", Uuid::now_v7()))
                .json(&support::public_response_request(&self.fixture.route_key))
                .send(),
        )
        .await
        .expect("public response request timed out")
        .expect("public response request");
        let status = response.status();
        let body = response.text().await.expect("public response body");
        (
            status,
            serde_json::from_str(&body).unwrap_or(Value::String(body)),
        )
    }

    /// `POST /api/v1/responses/stream` — the public SSE path, drained to its terminal event.
    ///
    /// Returns the transport status and every `data:` envelope. A refusal raised inside the
    /// execution arrives *on* the stream rather than as an HTTP status, because the response head
    /// is written before the execution starts; that is the contract this returns both halves of.
    async fn stream_public_response(&self, key: &str) -> (StatusCode, Vec<Value>) {
        let response = tokio::time::timeout(
            PUBLIC_WAIT,
            self.client
                .post(format!("{}/api/v1/responses/stream", self.moira.base_url))
                .header("x-consumer-key", key)
                .header("x-request-id", format!("f50-stream-{}", Uuid::now_v7()))
                .json(&support::public_response_request(&self.fixture.route_key))
                .send(),
        )
        .await
        .expect("public stream request timed out")
        .expect("public stream request");
        let status = response.status();
        let body = tokio::time::timeout(PUBLIC_WAIT, response.text())
            .await
            .expect("public stream body timed out")
            .expect("public stream body");
        let envelopes = body
            .lines()
            .filter_map(|line| line.strip_prefix("data: "))
            .filter_map(|data| serde_json::from_str::<Value>(data).ok())
            .collect();
        (status, envelopes)
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
/// # This guards the observability, not the outcome — and that is why it survived the decision
///
/// Whether Moira should also *refuse* was an open product decision when this case was written.
/// Issue #79 settled it fail-closed on 2026-08-06: the execution is now failed with
/// `agent_profile_disabled` before any provider is contacted, which is what
/// [`guards_a_disabled_agent_profile_fails_the_execution_before_any_provider_call`] asserts.
///
/// This case was deliberately written to be silent about that half. Silence from the operator's
/// side was a defect under *either* answer — both require the operator to be told, and they
/// differ only in whether the request is also refused — so this asserts the part both answers
/// share. It therefore did **not** go red when the decision landed, which is exactly what
/// splitting it out was for; the case that pinned the old fail-open outcome was named
/// `documents_current_behaviour_a_dangling_agent_profile_still_serves_the_request`, carried its
/// own reversal condition, and was deleted by the commit that flipped the behaviour. That is the
/// `documents_`/`guards_` distinction HANDOFF §3.4 asks for: had the two been merged, the
/// fail-closed fix would have had to edit a guard, which is the pinned-defect shape.
///
/// Note what the announcement now means. It is no longer "your traffic is being served
/// unguarded"; it is "your traffic is being refused, and here is the profile to re-enable or
/// detach". The event and the audit row are the operator-side half of a refusal the caller only
/// sees as a `409` or a `404`.
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
         runtime event — the route still names a profile the runtime cannot use, and since \
         issue #79 every request on it is refused until an operator re-enables it or detaches \
         it: {body}"
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
/// The announcement is made in `execute_inner`, *before* the two arms diverge, and the profile
/// lookup has exactly one call site in `src/`. So this case cannot fail today for a reason the
/// disable case would not also catch. It exists so that the day something moves the lookup, or
/// the streaming task grows its own resolution, the hole is already covered — which is precisely
/// the shape of the F49 fixture hole that made the whole branch untested for as long as it did.
///
/// **Changed by issue #79.** It used to assert that this execution reached the provider with
/// `"stream": true`, as proof that it had not silently taken the non-streaming path. That
/// evidence is gone by construction: the refusal now happens before either arm is chosen, so a
/// streamed request that names a dangling profile contacts nobody. The replacement premise is
/// the first script never being consumed, plus the control run below that proves this fixture
/// *can* stream — without which "no provider request" would be satisfied by a fixture that could
/// not have made one anyway.
#[tokio::test]
async fn guards_the_streaming_arm_announces_and_refuses_a_dangling_agent_profile_too() {
    let Some(case) = Case::new(
        Profile::Attached,
        vec![
            ProviderScript::Stream {
                deltas: vec!["ok".to_string()],
            },
            ProviderScript::Stream {
                deltas: vec!["unreachable".to_string()],
            },
        ],
    )
    .await
    else {
        return;
    };

    // Control: the streaming arm works on this fixture and really does stream.
    let (status, healthy) = case.diagnose(true, ExecutionOptions::default()).await;
    assert_eq!(status, StatusCode::OK, "diagnose failed: {healthy}");
    assert_eq!(healthy["outcome"]["status"], "succeeded", "{healthy}");
    let streamed = case.only_provider_body().await;
    assert_eq!(
        streamed["stream"], true,
        "the control must actually stream, or this case is the non-streaming one with extra \
         steps: {streamed}"
    );
    let before = case.provider_request_count().await;

    case.disable_profile().await;
    let (status, body) = case.diagnose(true, ExecutionOptions::default()).await;
    assert_eq!(status, StatusCode::OK, "diagnose failed: {body}");
    let execution_id = body["outcome"]["execution_id"]
        .as_str()
        .unwrap_or_else(|| panic!("no execution id: {body}"))
        .to_string();

    assert_eq!(
        body["outcome"]["status"], "failed",
        "the streaming arm must refuse a dangling agent profile exactly as the non-streaming \
         one does: {body}"
    );
    assert_eq!(
        body["outcome"]["failure"]["class"], "agent_profile_disabled",
        "{body}"
    );
    assert_eq!(
        case.provider_request_count().await,
        before,
        "the streaming arm reached the provider after the refusal; the second scripted stream \
         must never be consumed"
    );

    assert_eq!(
        events_of_type(&body, UNAVAILABLE_EVENT).len(),
        1,
        "the streaming arm dropped the same profile without announcing it: {body}"
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

/// **Issue #79, case 10 — the refusal itself, at the execution kernel.**
///
/// This replaces `documents_current_behaviour_a_dangling_agent_profile_still_serves_the_request`,
/// which asserted the opposite: `succeeded`, no failure, and a wire body with no preamble. That
/// case was named `documents_` and carried its own reversal condition — "when fail-closed vs
/// fail-open is decided, this case is wrong under the fail-closed answer". The maintainer decided
/// fail-closed on 2026-08-06, so it is wrong, and it is replaced rather than deleted because the
/// property it observed — *what happens to the request* — is exactly the property that changed and
/// still needs a guard, now pointing the other way.
///
/// # Why this asserts at the diagnostic endpoint and the two below assert over HTTP
///
/// They are different claims. This one says the *execution* refuses: no provider is contacted,
/// the outcome is `failed`, and the failure class is the one that names the condition. That is
/// the claim every caller-facing surface inherits, and it is the one a future entry point (a
/// worker, a summarization run, another API) inherits too without being listed here. The public
/// cases below say the *contract* a caller sees is the promised one — status and code — which the
/// diagnostic endpoint structurally cannot show, because it answers `200` with an outcome
/// document however the execution ended.
///
/// # The control is the whole test
///
/// The first execution succeeds against the same route, the same provider and the same active
/// profile, and it moves the provider's request counter. Without it, "the provider was not
/// contacted" is also what a mis-wired fixture produces, and this case would pass against a Moira
/// that refused everything.
#[tokio::test]
async fn guards_a_disabled_agent_profile_fails_the_execution_before_any_provider_call() {
    let Some(case) = Case::new(
        Profile::Attached,
        vec![
            ProviderScript::Completion {
                text: "ok".to_string(),
            },
            ProviderScript::Completion {
                text: "this script must never be consumed".to_string(),
            },
        ],
    )
    .await
    else {
        return;
    };

    let (status, healthy) = case.diagnose(false, ExecutionOptions::default()).await;
    assert_eq!(status, StatusCode::OK, "diagnose failed: {healthy}");
    assert_eq!(
        healthy["outcome"]["status"], "succeeded",
        "control: the same route must serve a request while its profile is active, or the \
         refusal below is not evidence about the profile: {healthy}"
    );
    let before = case.provider_request_count().await;
    assert_eq!(
        before, 1,
        "control: the successful execution must have reached the mock provider, or the \
         'no provider call' assertion below is vacuous"
    );

    case.disable_profile().await;
    let (status, body) = case.diagnose(false, ExecutionOptions::default()).await;
    assert_eq!(status, StatusCode::OK, "diagnose failed: {body}");
    assert_eq!(
        body["outcome"]["status"], "failed",
        "issue #79 decided fail-closed: a route whose agent profile is disabled must refuse \
         the request rather than serve it without the profile's preamble: {body}"
    );
    assert_eq!(
        body["outcome"]["failure"]["class"], "agent_profile_disabled",
        "the failure must name the condition, and specifically must not be the generic \
         internal_error or the route's own not-found class: {body}"
    );
    let message = body["outcome"]["failure"]["message"]
        .as_str()
        .unwrap_or_else(|| panic!("no failure message: {body}"));
    let profile = case.profile.as_ref().expect("attached profile");
    assert!(
        message.contains(&profile.profile_key) && message.contains(&profile.id.to_string()),
        "the message must name the profile by key and by id — whoever receives this has to be \
         able to fix the deployment without reading the server's logs: {message}"
    );
    assert!(
        message.contains(&case.fixture.route_key),
        "and the route that requires it, because the caller named a route and has never heard \
         of the profile: {message}"
    );
    assert!(
        !message.contains(PROFILE_PREAMBLE),
        "the preamble is the one field on an agent profile that can carry sensitive prompt \
         content; naming the profile must not mean quoting it: {message}"
    );

    assert_eq!(
        case.provider_request_count().await,
        before,
        "the refusal must happen before any provider is contacted: the second scripted \
         completion was consumed, so tokens were spent on a request Moira had already decided \
         to reject"
    );

    case.shutdown().await;
}

/// **Issue #79 — the public contract for a disabled profile: `409 agent_profile_disabled`.**
///
/// `409` and not `404`, and the difference is the point of the case: the profile exists, an
/// operator can see it on the admin plane, and the remedy is to switch it back on. A `404` here
/// would send whoever received it looking for a row that is right there. `409` is also already
/// declared on this operation, so the OpenAPI surface does not move.
///
/// The paired case below asserts the *other* status for the *other* condition. Neither is worth
/// much alone: a single-status implementation ("always 409", "always 404") satisfies exactly one
/// of them, and the pair is what makes the distinction a property rather than a coincidence.
#[tokio::test]
async fn guards_the_public_plane_refuses_a_disabled_agent_profile_with_409_and_a_code() {
    let Some(case) = Case::new(
        Profile::Attached,
        vec![
            ProviderScript::Completion {
                text: "ok".to_string(),
            },
            ProviderScript::Completion {
                text: "this script must never be consumed".to_string(),
            },
        ],
    )
    .await
    else {
        return;
    };
    let key = case.public_key().await;

    let (status, healthy) = case.create_public_response(&key).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "control: the public plane must serve this route while the profile is active: {healthy}"
    );
    let before = case.provider_request_count().await;
    assert_eq!(
        before, 1,
        "control: the successful POST reached the provider"
    );

    case.disable_profile().await;
    let (status, body) = case.create_public_response(&key).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "a route naming a disabled agent profile is a state conflict, not a missing resource \
         and not a provider failure: {body}"
    );
    assert_eq!(body["error"]["code"], "agent_profile_disabled", "{body}");
    assert_eq!(
        body["error"]["message_key"], "moira.error.agent_profile_disabled",
        "the code must resolve to a catalogued message key, or the caller gets a bare key with \
         no English fallback: {body}"
    );
    let message = body["error"]["message"]
        .as_str()
        .unwrap_or_else(|| panic!("no error message: {body}"));
    let profile = case.profile.as_ref().expect("attached profile");
    assert!(
        message.contains(&profile.profile_key) && message.contains(&profile.id.to_string()),
        "the caller's error must name the profile that has to be re-enabled: {message}"
    );
    assert_eq!(
        case.provider_request_count().await,
        before,
        "the public plane reached the provider for a request it then refused"
    );

    case.shutdown().await;
}

/// **Issue #79 — the public contract for a deleted profile: `404 agent_profile_not_found`.**
///
/// Soft-deleting is the other way to leave `route_definitions.agent_profile_id` dangling, and it
/// is a genuinely different remedy: the row cannot be re-enabled, because every admin write
/// filters `deleted_at is null` and `GET /api/v1/admin/agent-profiles/{id}` already answers `404`
/// for this id. Moira's answer to the caller agrees with the admin plane's answer about the same
/// row — two different statuses for one id would be worse than either.
///
/// `soft_delete_profile` asserts the route's reference survives the delete before the execution
/// runs, so this case cannot pass by the FK's `on delete set null` having quietly turned it into
/// the no-profile control.
#[tokio::test]
async fn guards_the_public_plane_refuses_a_deleted_agent_profile_with_404_and_a_code() {
    let Some(case) = Case::new(
        Profile::Attached,
        vec![
            ProviderScript::Completion {
                text: "ok".to_string(),
            },
            ProviderScript::Completion {
                text: "this script must never be consumed".to_string(),
            },
        ],
    )
    .await
    else {
        return;
    };
    let key = case.public_key().await;

    let (status, healthy) = case.create_public_response(&key).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "control: the public plane must serve this route while the profile is active: {healthy}"
    );
    let before = case.provider_request_count().await;
    assert_eq!(
        before, 1,
        "control: the successful POST reached the provider"
    );

    case.soft_delete_profile().await;
    let (status, body) = case.create_public_response(&key).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a soft-deleted profile is gone, not switched off; 409 would promise a state the \
         operator can flip back: {body}"
    );
    assert_eq!(body["error"]["code"], "agent_profile_not_found", "{body}");
    assert_eq!(
        body["error"]["message_key"], "moira.error.agent_profile_not_found",
        "{body}"
    );
    let message = body["error"]["message"]
        .as_str()
        .unwrap_or_else(|| panic!("no error message: {body}"));
    let profile = case.profile.as_ref().expect("attached profile");
    assert!(
        message.contains(&profile.id.to_string()),
        "the id is the only handle left once the row is gone, so it must be in the message: \
         {message}"
    );
    assert_eq!(
        case.provider_request_count().await,
        before,
        "the public plane reached the provider for a request it then refused"
    );

    case.shutdown().await;
}

/// **Issue #79 — the refusal on the public *stream*, where it cannot be an HTTP status.**
///
/// The response head is written before the execution starts, so a streamed request that names a
/// dangling profile gets `200` on the transport and the refusal as the terminal `response.failed`
/// event. That is the existing contract for every mid-stream failure; this case pins that the new
/// refusal uses it rather than, say, hanging, ending the stream with no terminal event, or
/// reporting `response.completed` with no output.
///
/// The `200` is asserted deliberately, not overlooked: a reader who sees only "status 200" in a
/// fail-closed test should be able to find here why that is the right answer and where the actual
/// refusal is.
#[tokio::test]
async fn guards_the_public_stream_refuses_a_dangling_agent_profile_in_its_terminal_event() {
    let Some(case) = Case::new(
        Profile::Attached,
        vec![
            ProviderScript::Stream {
                deltas: vec!["ok".to_string()],
            },
            ProviderScript::Stream {
                deltas: vec!["this script must never be consumed".to_string()],
            },
        ],
    )
    .await
    else {
        return;
    };
    let key = case.public_key().await;

    let (status, healthy) = case.stream_public_response(&key).await;
    assert_eq!(status, StatusCode::OK, "control: the stream must open");
    assert!(
        healthy
            .iter()
            .any(|event| event["type"] == "response.completed"),
        "control: the same route must complete a stream while its profile is active, or the \
         refusal below is not evidence about the profile: {healthy:?}"
    );
    let before = case.provider_request_count().await;
    assert_eq!(
        before, 1,
        "control: the successful stream reached the provider"
    );

    case.disable_profile().await;
    let (status, events) = case.stream_public_response(&key).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the transport still succeeds: the head is written before the execution starts, so the \
         refusal has to arrive on the stream"
    );
    let failed: Vec<&Value> = events
        .iter()
        .filter(|event| event["type"] == "response.failed")
        .collect();
    assert_eq!(
        failed.len(),
        1,
        "a refused stream must end with exactly one response.failed event: {events:?}"
    );
    assert_eq!(
        failed[0]["payload"]["error"]["code"], "agent_profile_disabled",
        "and it must carry the same code the non-streaming path returns: {:?}",
        failed[0]
    );
    assert!(
        !events
            .iter()
            .any(|event| event["type"] == "response.completed"),
        "a refused stream must not also report completion: {events:?}"
    );
    assert!(
        !events
            .iter()
            .any(|event| event["type"] == UNAVAILABLE_EVENT),
        "the operator-side agent_profile_unavailable event must stay off the caller's stream; \
         its payload names the route's internals and the caller has already been told what it \
         needs by the terminal error: {events:?}"
    );
    assert_eq!(
        case.provider_request_count().await,
        before,
        "the streaming public path reached the provider for a request it then refused"
    );

    case.shutdown().await;
}

/// The single `route_selected` payload of a diagnose response.
///
/// Exactly one is expected: an execution selects a route once. Asserting the count rather than
/// taking the first is what keeps "the reason was `global_default`" from being satisfied by a
/// response that also announced some other selection.
fn only_route_selection(body: &Value) -> &Value {
    let selections = events_of_type(body, "route_selected");
    assert_eq!(
        selections.len(),
        1,
        "an execution selects exactly one route: {body}"
    );
    selections[0]
}

/// Asserts the profile's three fields on a wire body, which is the property cases 14 and 15 share
/// with case 1 and the only thing that observably distinguishes a route that carried its profile
/// from one that dropped it.
fn assert_the_profile_reached_the_wire(wire: &Value, prompt: &str) {
    let messages = wire["messages"]
        .as_array()
        .unwrap_or_else(|| panic!("no messages on the wire: {wire}"));
    assert_eq!(
        messages.len(),
        2,
        "the profile's preamble must be prepended to the caller's one message: {wire}"
    );
    assert_eq!(messages[0]["role"], "system", "{wire}");
    assert_eq!(
        message_text(&messages[0]),
        PROFILE_PREAMBLE,
        "the agent profile's preamble must reach the provider verbatim: {wire}"
    );
    assert_eq!(messages[1]["role"], "user", "{wire}");
    assert_eq!(message_text(&messages[1]), prompt, "{wire}");
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
}

/// **Case 14 — issue #155 A1. The global-default arm carries the route's agent profile.**
///
/// The cheapest edit that breaks this property is one token: `agent_profile_id: None` in the
/// `GlobalDefault` arm of `DefaultTaskRouter::select_route`. Before this case, that edit left
/// every test in the tree green while making every default-routed request in a deployment run
/// without its profile's preamble — a preamble is where guardrails live, so the failure mode is
/// an unguarded model answering production traffic, arrived at from the routing side rather than
/// the resolution side issue #79 closed.
///
/// # What makes it an arm rather than a repetition of case 1
///
/// Two things, and both are load-bearing. The request carries **no** `route`, which is the only
/// way past the explicit-hint arm's early return; and the `reason` on the `route_selected` event
/// is asserted to be `global_default`, so a change that quietly routed this back through the
/// explicit-hint arm would go red here rather than passing on case 1's evidence.
///
/// `PROMPT` does not contain `"code"`, so the rule-match arm in between cannot claim it — and if
/// it somehow did, the reason assertion says so.
///
/// # Why the seeded route has to go
///
/// `get_default_route` sorts the `general` route migration `0005` seeds ahead of every other
/// route, and nothing points a routing policy at it, so a hintless request would otherwise fail
/// at admission with `no_eligible_model` — a `failed` outcome that never reaches the profile at
/// all. `Case::make_this_fixtures_route_the_only_default` disables it and asserts that exactly
/// one active route is left, so "the default route" cannot be a tie-break this test merely got
/// lucky with.
#[tokio::test]
async fn guards_the_global_default_route_arm_carries_its_agent_profile_to_the_wire() {
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
    case.make_this_fixtures_route_the_only_default().await;

    let (status, body) = case
        .diagnose_routed(None, PROMPT, false, ExecutionOptions::default())
        .await;
    assert_eq!(status, StatusCode::OK, "diagnose failed: {body}");
    assert_eq!(body["outcome"]["status"], "succeeded", "{body}");

    let selection = only_route_selection(&body);
    assert_eq!(
        selection["reason"], "global_default",
        "this case exists to drive the global-default arm; any other reason means the arm under \
         test never ran and the assertions below are case 1 again: {selection}"
    );
    assert_eq!(
        selection["route_key"].as_str(),
        Some(case.fixture.route_key.as_str()),
        "and it must have defaulted to the route that carries the profile: {selection}"
    );

    assert_the_profile_reached_the_wire(&case.only_provider_body().await, PROMPT);

    case.shutdown().await;
}

/// **Case 15 — issue #155 A1. The rule-matched arm carries it too.**
///
/// The third of the three `RouteDecision` constructions, and the same one-token edit applies to
/// it: `agent_profile_id: None` in the `RuleMatch` arm. The arm is reached by a prompt containing
/// `"code"` and an active route keyed `coding`, both of which this case supplies —
/// `RULE_MATCHING_PROMPT` and `Case::add_a_rule_matched_route_carrying_the_same_profile`.
///
/// # The seeded `general` route is left alone here, on purpose
///
/// It is this case's failure-mode detector. If the rule-match arm does not claim the request, the
/// global-default arm does, and it returns `general` — which has no routing policy, so the
/// execution fails at admission instead of quietly asserting the same thing case 14 does. The
/// route key and the reason are asserted anyway, because "it failed" and "it took the wrong arm
/// and still worked" should not be distinguished only by a coincidence of fixture wiring.
#[tokio::test]
async fn guards_the_rule_matched_route_arm_carries_its_agent_profile_to_the_wire() {
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
    let rule_matched_route_id = case
        .add_a_rule_matched_route_carrying_the_same_profile()
        .await;

    let (status, body) = case
        .diagnose_routed(
            None,
            RULE_MATCHING_PROMPT,
            false,
            ExecutionOptions::default(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "diagnose failed: {body}");
    assert_eq!(body["outcome"]["status"], "succeeded", "{body}");

    let selection = only_route_selection(&body);
    assert_eq!(
        selection["reason"], "rule_match",
        "this case exists to drive the rule-match arm: {selection}"
    );
    assert_eq!(
        selection["route_key"].as_str(),
        Some(RULE_MATCHED_ROUTE_KEY),
        "{selection}"
    );
    assert_eq!(
        selection["route_id"].as_str(),
        Some(rule_matched_route_id.to_string().as_str()),
        "the route the rule matched must be the one this case created and attached the profile \
         to, not this fixture's own route under a different name: {selection}"
    );

    assert_the_profile_reached_the_wire(&case.only_provider_body().await, RULE_MATCHING_PROMPT);

    case.shutdown().await;
}
