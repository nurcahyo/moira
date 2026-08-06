//! Finding F29 — a caller that asks for structured output actually receives it.
//!
//! # Why this suite drives `POST /api/v1/admin/runtime/diagnose`
//!
//! `ExecutionOutcome.structured_output` is unreachable from the public response API:
//! `PublicResponse` has no such field, so a public-plane test could not observe the value even
//! if it were populated. The runtime diagnostic endpoint is the **only** surface in the tree
//! that serialises an `ExecutionOutcome` verbatim, so it is the only place a black-box
//! assertion on this field is possible at all. Asserting on the service return value instead
//! would prove the kernel fills the struct but not that the field survives serialisation, and
//! this field's entire history is "populated somewhere, `null` everywhere it is read".
//!
//! # The four properties, and why each needs its own case
//!
//! 1. A schema-carrying request whose reply is JSON yields the parsed value **and** the raw
//!    text. The pair matters: `output_text` must not be replaced or reformatted, because two
//!    call sites (memory extraction, summarization) prefer `structured_output` and fall back to
//!    `output_text`, and a change to either alone silently changes what they parse.
//! 2. The same, over a **stream**. `execute_rig_stream` never constructs a
//!    `RuntimeCompletionOutput` — it accumulates text itself — so it is a genuinely separate
//!    code path, and a fix applied at the Rig boundary would cover only case 1. Splitting the
//!    JSON across deltas also pins that the parse happens on the *accumulated* text.
//! 3. **The gate.** A request with no `output_schema` whose reply happens to be valid JSON must
//!    still report `structured_output: null`. This is the case that protects conversation
//!    summarization, which sends no schema and stores `output_text` as a content-addressed
//!    body — see `tests/conversation_summarization.rs`.
//! 4. **Fail hard — issue #80, decided 2026-08-06.** A schema-carrying request whose reply is not
//!    JSON fails the execution with `structured_output_invalid`, which a public caller receives as
//!    **422**. It used to succeed with `structured_output: null` and the prose in `output_text`,
//!    which is the same document a legitimately empty answer produces — so a caller could not tell
//!    "the provider did not comply" from "the answer was empty". Three cases, because the property
//!    has three surfaces: the completion path, the streaming path, and the public HTTP contract
//!    that is the whole point of the decision. `StructuredOutputInvalid` is in neither
//!    `is_retryable` nor `is_fallback_eligible` nor `is_circuit_failure`, so the failure is
//!    terminal on the first non-conforming reply — asserted here as "the provider was called
//!    exactly once".
//!
//!    All three of F29's preconditions were discharged first — F39 landed, the disposition is
//!    recorded and guarded in `src/orchestration/controls.rs` rather than true by omission, and
//!    `run_extraction` reads `execution.status`. The doc comment on `structured_output_from_text`
//!    in `src/application/execution.rs` carries the full argument, including the boundary: `null`,
//!    `{}` and `[]` all parse, so an empty *answer* is still a `200`.

mod support;

use std::{sync::Arc, time::Duration};

use axum::http::StatusCode;
use moira::domain::{DiagnosticExecutionRequest, ExecutionOptions, ProviderType};
use serde_json::{Value, json};
use support::{
    LifecycleFixture, MoiraHttpServer, RuntimePolicy,
    mock_openai::{MockOpenAiServer, ProviderScript},
};

const WAIT: Duration = Duration::from_secs(15);

/// The smallest schema that is a legal `rig_core::schemars::Schema` and says something.
///
/// Deliberately trivial: this suite is about whether the *reply* is parsed, not about schema
/// expressiveness — and Moira validates nothing beyond "this is a JSON object or bool"
/// (`build_completion_request`), so a richer schema would imply a check that does not exist.
fn trivial_object_schema() -> Value {
    json!({
        "type": "object",
        "properties": { "a": { "type": "integer" } },
        "required": ["a"]
    })
}

/// One provider to stand up behind the route: its type, what it claims it can do, how strongly
/// routing should prefer it, and what it will reply.
///
/// `priority` is `routing_policies.priority`, ordered **ascending** — a lower number is
/// preferred. The F39 fallback case depends on that direction, because it has to make the
/// disqualified provider the one routing *would* have chosen.
struct ProviderSpec {
    provider_type: ProviderType,
    capabilities: Value,
    priority: i32,
    scripts: Vec<ProviderScript>,
}

impl ProviderSpec {
    /// An OpenAI-compatible provider advertising nothing but streaming — the original default.
    fn openai_compatible(scripts: Vec<ProviderScript>) -> Self {
        Self {
            provider_type: ProviderType::OpenAiCompatible,
            capabilities: json!({ "streaming": true }),
            priority: 10,
            scripts,
        }
    }
}

struct Case {
    fixture: LifecycleFixture,
    providers: Vec<MockOpenAiServer>,
    moira: MoiraHttpServer,
    client: reqwest::Client,
}

impl Case {
    async fn new(scripts: Vec<ProviderScript>) -> Option<Self> {
        Self::with_providers(vec![ProviderSpec::openai_compatible(scripts)]).await
    }

    async fn with_providers(specs: Vec<ProviderSpec>) -> Option<Self> {
        let fixture = LifecycleFixture::new().await?;
        let mut providers = Vec::with_capacity(specs.len());
        for spec in specs {
            let provider = MockOpenAiServer::start(spec.scripts).await;
            fixture
                .add_typed_provider_with_capabilities(
                    spec.provider_type,
                    provider.base_url(),
                    spec.priority,
                    RuntimePolicy::default(),
                    spec.capabilities,
                )
                .await;
            providers.push(provider);
        }
        // `diagnostic_endpoint_enabled` is `false` in `config/default.toml` and in
        // `Settings::default()`, so the route 404s without this. Same clone-and-override
        // `tests/conversation_summarization.rs` performs for `prometheus_enabled`.
        let mut state = fixture.state.clone();
        let mut settings = (*fixture.state.settings).clone();
        settings.runtime.diagnostic_endpoint_enabled = true;
        state.settings = Arc::new(settings);
        let moira = MoiraHttpServer::start(state).await;
        Some(Self {
            fixture,
            providers,
            moira,
            client: reqwest::Client::new(),
        })
    }

    fn provider(&self) -> &MockOpenAiServer {
        &self.providers[0]
    }

    /// Posts one diagnostic execution and returns `(status, body)`.
    ///
    /// The body is built from the typed `DiagnosticExecutionRequest` rather than hand-written
    /// JSON: the DTO carries `#[serde(deny_unknown_fields)]`, so a hand-written body that
    /// drifted from the struct would fail as a 422 that looks like a routing problem.
    async fn diagnose(&self, stream: bool, output_schema: Option<Value>) -> (StatusCode, Value) {
        self.diagnose_requiring(stream, output_schema, Vec::new())
            .await
    }

    /// As [`Self::diagnose`], with `required_capabilities` under the caller's control.
    ///
    /// The public plane derives this list from the response format
    /// (`application/public.rs`), but the diagnostic DTO passes `ExecutionOptions` through
    /// verbatim, so a diagnostic request carries an **empty** list unless one is supplied. That
    /// is what makes both halves of F39 observable from one endpoint: with the list empty the
    /// candidate filter is bypassed and the request reaches the provider, exposing what Rig put
    /// on the wire; with it populated the filter runs, exposing Moira's admission decision.
    async fn diagnose_requiring(
        &self,
        stream: bool,
        output_schema: Option<Value>,
        required_capabilities: Vec<String>,
    ) -> (StatusCode, Value) {
        let request = DiagnosticExecutionRequest {
            application_id: Some(self.fixture.application_id),
            external_tenant_id: None,
            external_user_id: Some("f29-structured-output".to_string()),
            route: Some(self.fixture.route_key.clone()),
            provider_id: None,
            provider_model_id: None,
            credential_id: None,
            prompt: "return the object".to_string(),
            stream,
            options: ExecutionOptions {
                timeout_ms: Some(5_000),
                stream,
                output_schema,
                required_capabilities,
                ..ExecutionOptions::default()
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
                .header("x-request-id", format!("f29-{}", uuid::Uuid::now_v7()))
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

    async fn shutdown(self) {
        for provider in self.providers {
            provider.shutdown().await;
        }
        self.moira.shutdown().await;
    }
}

/// The primary case. Against a build that hardcodes `structured_output: None` this fails on the
/// first assertion, `null` vs `{"a":1}`, with no timing dependency.
#[tokio::test]
async fn a_schema_carrying_completion_returns_the_parsed_structured_output() {
    let Some(case) = Case::new(vec![ProviderScript::Completion {
        text: "{\"a\":1}".to_string(),
    }])
    .await
    else {
        return;
    };

    let (status, body) = case.diagnose(false, Some(trivial_object_schema())).await;
    assert_eq!(status, StatusCode::OK, "diagnose failed: {body}");
    assert_eq!(body["outcome"]["status"], "succeeded", "{body}");
    assert_eq!(
        body["outcome"]["structured_output"],
        json!({ "a": 1 }),
        "the schema-constrained reply must reach the caller as a parsed value: {body}"
    );
    assert_eq!(
        body["outcome"]["output_text"], "{\"a\":1}",
        "the raw text must survive unchanged alongside the parsed value: {body}"
    );

    // The schema really did reach the provider, so this is not a test of a request Moira
    // silently dropped.
    let requests = case.provider().requests().await;
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].body["response_format"]["type"], "json_schema",
        "the diagnostic request must carry the schema on the wire: {}",
        requests[0].body
    );

    case.shutdown().await;
}

/// The streaming twin. Without it `execute_rig_stream` is uncovered and the two paths drift.
///
/// The JSON is split across two deltas on purpose: it pins that the parse runs on the
/// accumulated text rather than on any single chunk, which is the only way a per-chunk
/// implementation could pass case 1 and still be wrong here.
#[tokio::test]
async fn a_schema_carrying_stream_returns_the_parsed_structured_output() {
    let Some(case) = Case::new(vec![ProviderScript::Stream {
        deltas: vec!["{\"a\"".to_string(), ":1}".to_string()],
    }])
    .await
    else {
        return;
    };

    let (status, body) = case.diagnose(true, Some(trivial_object_schema())).await;
    assert_eq!(status, StatusCode::OK, "diagnose failed: {body}");
    assert_eq!(body["outcome"]["status"], "succeeded", "{body}");
    assert_eq!(
        body["outcome"]["structured_output"],
        json!({ "a": 1 }),
        "the streamed reply must be parsed from the accumulated text: {body}"
    );
    assert_eq!(
        body["outcome"]["output_text"], "{\"a\":1}",
        "the accumulated text must survive unchanged alongside the parsed value: {body}"
    );

    case.shutdown().await;
}

/// **The gate.** No `output_schema` means no parse, even when the reply is valid JSON.
///
/// This is the assertion that protects conversation summarization: it sends no schema,
/// `parse_summary` accepts any non-empty prose, and the consumer prefers `structured_output`
/// via `.map(|value| value.to_string())`. An ungated parse would re-serialise a summary that
/// happened to be valid JSON and store the re-serialised form — silently changing
/// `summary_hash`, which is documented as a content address.
///
/// The cheapest edit that breaks the property is deleting the `wants_structured` test in
/// `structured_output_from_text`; that edit turns this case red, and
/// `a_summary_that_is_valid_json_is_stored_verbatim` in `tests/conversation_summarization.rs`
/// red with it.
#[tokio::test]
async fn a_reply_that_is_json_is_not_parsed_when_no_schema_was_requested() {
    let Some(case) = Case::new(vec![ProviderScript::Completion {
        text: "{\"a\":1}".to_string(),
    }])
    .await
    else {
        return;
    };

    let (status, body) = case.diagnose(false, None).await;
    assert_eq!(status, StatusCode::OK, "diagnose failed: {body}");
    assert_eq!(body["outcome"]["status"], "succeeded", "{body}");
    assert_eq!(
        body["outcome"]["structured_output"],
        Value::Null,
        "a reply that merely happens to be JSON must not be parsed when the caller asked for \
         no schema: {body}"
    );
    assert_eq!(body["outcome"]["output_text"], "{\"a\":1}", "{body}");

    case.shutdown().await;
}

/// The reply a non-conforming model sends. Held as a constant because two assertions are about
/// it: that it does not come back as an answer, and that it does not come back inside the error.
const NON_CONFORMING_REPLY: &str = "I am afraid I cannot do that.";

/// **Fail hard, issue #80.** A non-conforming reply ends the execution instead of leaving the
/// field `null` on a `succeeded` outcome.
///
/// Replaces `a_reply_that_is_not_json_leaves_the_field_null_and_still_succeeds`, which asserted
/// the opposite on the same fixture and whose own failure message named this change as the thing
/// that would make it wrong. That case was correct for F29 and is wrong from #80 onward; the
/// catalog description it protected has been widened in the same commit.
///
/// **Four assertions, and each one is a different way the flip could be half-done:**
///
/// 1. the outcome is `failed` and the class is `structured_output_invalid` — not
///    `provider_invalid_response`, and not a routing failure;
/// 2. `output_text` is absent. A 422 that still carried the model's prose would hand the caller
///    something that reads like an answer next to an error, which is the ambiguity the decision
///    exists to remove;
/// 3. the failure message does not contain the reply. `failure.message` is copied verbatim into
///    the public error envelope, so a message built from the provider's bytes would put untrusted
///    output under Moira's own error surface;
/// 4. the provider was called **exactly once**. `StructuredOutputInvalid` is in neither
///    `is_retryable` nor `is_fallback_eligible`, and a second call here would be the cheapest
///    silent way to violate that — the disposition is asserted as behaviour, not as membership.
#[tokio::test]
async fn a_reply_that_is_not_json_fails_the_execution_with_structured_output_invalid() {
    let Some(case) = Case::new(vec![ProviderScript::Completion {
        text: NON_CONFORMING_REPLY.to_string(),
    }])
    .await
    else {
        return;
    };

    let (status, body) = case.diagnose(false, Some(trivial_object_schema())).await;
    assert_eq!(status, StatusCode::OK, "diagnose failed: {body}");
    assert_eq!(
        body["outcome"]["status"], "failed",
        "issue #80: a reply that is not JSON must fail a schema-carrying execution rather than \
         reporting success with a null value: {body}"
    );
    assert_eq!(
        body["outcome"]["failure"]["class"], "structured_output_invalid",
        "the failure must name the structured-output contract, not the transport: {body}"
    );
    assert_eq!(body["outcome"]["structured_output"], Value::Null, "{body}");
    assert_eq!(
        body["outcome"]["output_text"],
        Value::Null,
        "a failed structured execution must not return the prose as though it were an answer: \
         {body}"
    );
    let message = body["outcome"]["failure"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        !message.contains(NON_CONFORMING_REPLY),
        "the failure message is copied verbatim into the public error envelope and must not echo \
         the provider's reply: {message}"
    );

    assert_eq!(
        case.provider().requests().await.len(),
        1,
        "the provider must be called exactly once: this class is neither retryable nor \
         fallback-eligible, so a second call would mean the disposition in \
         src/orchestration/controls.rs is not what its own guard says"
    );

    case.shutdown().await;
}

/// **Fail hard on the stream.** The streaming twin, and it guards a distinct failure mode.
///
/// `execute_rig_stream` never constructs a `RuntimeCompletionOutput` — it accumulates text itself
/// — so the flip has to be applied there separately, and the cheapest half-done version of this
/// change is applying it to the completion arm only. Every other case in this file stays green
/// through that omission: case 2 sends conforming JSON, and the case above never streams.
///
/// The deltas are split so the failure is provably raised on the *accumulated* text rather than
/// on a chunk, which is the same property case 2 pins for the success path.
///
/// It also pins the harder half of the decision: the caller has **already received these deltas**
/// when the failure is raised. Failing anyway is deliberate — the deltas were text, and the
/// caller asked for a value.
#[tokio::test]
async fn a_stream_whose_reply_is_not_json_fails_the_execution_with_structured_output_invalid() {
    let Some(case) = Case::new(vec![ProviderScript::Stream {
        deltas: vec!["I am afraid ".to_string(), "I cannot do that.".to_string()],
    }])
    .await
    else {
        return;
    };

    let (status, body) = case.diagnose(true, Some(trivial_object_schema())).await;
    assert_eq!(status, StatusCode::OK, "diagnose failed: {body}");
    assert_eq!(
        body["outcome"]["status"], "failed",
        "issue #80 applies to the streaming path too, and this is the arm a completion-only fix \
         leaves behind: {body}"
    );
    assert_eq!(
        body["outcome"]["failure"]["class"], "structured_output_invalid",
        "{body}"
    );
    assert_eq!(body["outcome"]["structured_output"], Value::Null, "{body}");
    assert_eq!(
        body["outcome"]["output_text"],
        Value::Null,
        "the accumulated prose must not be reported as an answer: {body}"
    );

    assert_eq!(
        case.provider().requests().await.len(),
        1,
        "no retry and no fallback on the streaming path either"
    );

    case.shutdown().await;
}

/// One `POST /api/v1/responses` carrying a `json_schema` response format, against a fixture wired
/// for structured output, answered by `reply`.
///
/// Returns `(status, raw body, provider call count)`. The body is returned as a string rather than
/// parsed so a case can assert on the *bytes* the caller received — which is how "the provider's
/// reply is not echoed anywhere in the envelope" is checked, including in fields a targeted
/// assertion would not look at.
///
/// The count is taken after the response, so "one call" covers the whole request rather than the
/// part before the failure.
async fn public_response(reply: &str) -> Option<(StatusCode, String, usize)> {
    let fixture = LifecycleFixture::new().await?;
    let provider = MockOpenAiServer::start(vec![ProviderScript::Completion {
        text: reply.to_string(),
    }])
    .await;
    fixture
        .add_provider_with_capabilities(
            provider.base_url(),
            10,
            RuntimePolicy::default(),
            // A schema-carrying public request requires this capability
            // (`required_capabilities` in `application/public.rs`), so the default
            // streaming-only model would fail at routing before reaching the provider.
            json!({ "streaming": true, "structured_output": true }),
        )
        .await;
    let consumer_key = fixture.enable_public_streaming().await;
    let moira = MoiraHttpServer::start(fixture.state.clone()).await;

    let response = tokio::time::timeout(
        WAIT,
        reqwest::Client::new()
            .post(format!("{}/api/v1/responses", moira.base_url))
            .header("x-consumer-key", &consumer_key)
            .header("x-request-id", format!("i80-{}", uuid::Uuid::now_v7()))
            .json(&json!({
                "input": [{ "role": "user", "content": [{ "type": "input_text", "text": "return the object" }] }],
                "route": fixture.route_key,
                "response_format": {
                    "type": "json_schema",
                    "name": "trivial",
                    "schema": trivial_object_schema()
                }
            }))
            .send(),
    )
    .await
    .expect("public response request timed out")
    .expect("public response request");
    let status = response.status();
    let body = response.text().await.expect("public response body");
    let calls = provider.call_count().await;

    moira.shutdown().await;
    provider.shutdown().await;
    Some((status, body, calls))
}

/// **The decision itself, where a caller can see it: `422`, not a `200` with a null value.**
///
/// The three cases above drive `POST /api/v1/admin/runtime/diagnose`, which answers `200` with an
/// outcome document whatever the execution did — so it can show that the execution failed, but it
/// cannot show the status code or the error code the public contract promises. Issue #80 is a
/// statement about that contract, so it is asserted on the contract.
///
/// **The control is in the same test and is not decoration.** "422 and the code is
/// `structured_output_invalid`" is also what a *schema* rejection produces
/// (`validate_response_format`, `build_completion_request` — the class's other two emitters), and
/// a fixture whose model could not be routed a schema at all would fail before any provider call
/// with the same status. The conforming control proves this fixture reaches the provider and
/// returns the parsed value, so the refusal is attributable to the reply rather than to the
/// request or to the wiring — the vacuous-pass shape HANDOFF §2.3 keeps finding.
#[tokio::test]
async fn a_public_caller_receives_422_rather_than_a_null_structured_output() {
    let Some((status, body, calls)) = public_response(NON_CONFORMING_REPLY).await else {
        return;
    };
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "issue #80: a reply that is not JSON must reach the caller as 422, got {status}: {body}"
    );
    let envelope: Value = serde_json::from_str(&body).expect("coded error envelope");
    assert_eq!(
        envelope["error"]["code"], "structured_output_invalid",
        "the caller must be told which contract failed: {body}"
    );
    assert_eq!(
        envelope["error"]["message_key"], "moira.error.structured_output_invalid",
        "the code must resolve to its catalog entry, which is what makes the message \
         translatable: {body}"
    );
    assert!(
        !body.contains(NON_CONFORMING_REPLY),
        "the error envelope must not carry the provider's reply back to the caller: {body}"
    );
    assert_eq!(calls, 1, "exactly one provider call, no retry, no fallback");

    // The control. Same fixture, same schema, same route — a reply that *is* JSON must still be
    // answered `200` with the parsed value, or the refusal above is unattributable.
    let Some((status, body, calls)) = public_response("{\"a\":1}").await else {
        return;
    };
    assert_eq!(
        status,
        StatusCode::OK,
        "the control request must succeed, or the 422 above proves only that the fixture is \
         broken: {body}"
    );
    assert_eq!(
        calls, 1,
        "the control request must reach the provider: {body}"
    );
}

// ---------------------------------------------------------------------------------------------
// Finding F39 — the capability gate reconciled against what Rig will actually send.
// ---------------------------------------------------------------------------------------------

/// **The premise, measured rather than cited.**
///
/// `rig-core` 0.40's DeepSeek extension sets `SUPPORTS_RESPONSE_FORMAT = false`, and
/// `providers/openai/completion/mod.rs` then discards `output_schema` with only a
/// `tracing::warn!` — a warning Moira cannot observe. This drives a real DeepSeek
/// `CompletionModel` (DeepSeek is OpenAI-compatible on the wire, so the same mock serves it)
/// and reads the body that actually left the process.
///
/// It is the exact contrast of the first case in this file, which asserts
/// `response_format.type == "json_schema"` for an OpenAI-compatible provider given the same
/// request. Same request, same mock, different provider type, opposite wire.
///
/// `required_capabilities` is deliberately **empty** so the candidate filter does not run: this
/// case must keep observing the wire after the filter starts excluding DeepSeek, or the premise
/// would become untestable the moment it is acted on.
///
/// A red here means Rig changed and F39's premise no longer holds — verify
/// `SUPPORTS_RESPONSE_FORMAT` in the vendored crate before touching anything else.
#[tokio::test]
async fn rig_drops_the_schema_before_the_wire_on_deepseek() {
    let Some(case) = Case::with_providers(vec![ProviderSpec {
        provider_type: ProviderType::DeepSeek,
        capabilities: json!({ "streaming": true, "structured_output": true }),
        priority: 10,
        scripts: vec![ProviderScript::Completion {
            text: "{\"a\":1}".to_string(),
        }],
    }])
    .await
    else {
        return;
    };

    let (status, body) = case.diagnose(false, Some(trivial_object_schema())).await;
    assert_eq!(status, StatusCode::OK, "diagnose failed: {body}");
    assert_eq!(body["outcome"]["status"], "succeeded", "{body}");

    let requests = case.provider().requests().await;
    assert_eq!(requests.len(), 1, "the request must have reached DeepSeek");
    assert!(
        requests[0].body.get("response_format").is_none(),
        "rig-core 0.40 drops output_schema for DeepSeek; a response_format on the wire means \
         the premise of finding F39 has changed: {}",
        requests[0].body
    );

    case.shutdown().await;
}

/// **The fix.** A DeepSeek row that *claims* `structured_output` cannot serve a request that
/// requires it, because the claim is contradicted by Rig before the wire.
///
/// The row here is the exact lie F39 describes: `structured_output: true` on a provider type
/// whose schema Rig discards. Before the fix this routed, sent a schema-less request, got prose
/// back and reported `succeeded` — a wrong answer with a success status, which is the whole
/// finding. After it, the candidate is filtered out and — with no other candidate — the
/// execution fails honestly.
///
/// The zero-request assertion is the load-bearing one: `no_eligible_model` alone would also be
/// produced by a fixture that simply failed to wire the provider up, and that shape of vacuous
/// pass has bitten this repo repeatedly (HANDOFF §2.3). Asserting the provider was reachable and
/// deliberately not chosen is what distinguishes the two.
#[tokio::test]
async fn a_deepseek_row_claiming_structured_output_is_not_routed_a_structured_request() {
    let Some(case) = Case::with_providers(vec![ProviderSpec {
        provider_type: ProviderType::DeepSeek,
        capabilities: json!({ "streaming": true, "structured_output": true }),
        priority: 10,
        scripts: vec![ProviderScript::Completion {
            text: "I am afraid I cannot do that.".to_string(),
        }],
    }])
    .await
    else {
        return;
    };

    let (status, body) = case
        .diagnose_requiring(
            false,
            Some(trivial_object_schema()),
            vec!["structured_output".to_string()],
        )
        .await;
    assert_eq!(status, StatusCode::OK, "diagnose failed: {body}");
    assert_eq!(
        body["outcome"]["status"], "failed",
        "a provider that cannot receive the schema must not serve a structured request: {body}"
    );
    assert_eq!(
        body["outcome"]["failure"]["class"], "no_eligible_model",
        "{body}"
    );
    assert_eq!(
        case.provider().requests().await.len(),
        0,
        "the request must be refused at admission, not sent and silently unconstrained"
    );

    case.shutdown().await;
}

/// **What the fix buys a real deployment.** With a capable peer configured, the disqualified
/// provider falls out of routing and the request is served correctly instead of failing.
///
/// DeepSeek is given the **lower** `priority` number, so it is the candidate routing would
/// otherwise have chosen — without that the test would pass on ordering alone and prove nothing.
/// This is the case that shows the fix is not merely a new way to fail: the caller who used to
/// get unconstrained prose with `succeeded` now gets the schema-constrained answer.
#[tokio::test]
async fn a_structured_request_routes_past_deepseek_to_a_provider_that_sends_the_schema() {
    let Some(case) = Case::with_providers(vec![
        ProviderSpec {
            provider_type: ProviderType::DeepSeek,
            capabilities: json!({ "streaming": true, "structured_output": true }),
            priority: 1,
            scripts: vec![ProviderScript::Completion {
                text: "prose, because the schema never arrived".to_string(),
            }],
        },
        ProviderSpec {
            provider_type: ProviderType::OpenAiCompatible,
            capabilities: json!({ "streaming": true, "structured_output": true }),
            priority: 10,
            scripts: vec![ProviderScript::Completion {
                text: "{\"a\":1}".to_string(),
            }],
        },
    ])
    .await
    else {
        return;
    };

    let (status, body) = case
        .diagnose_requiring(
            false,
            Some(trivial_object_schema()),
            vec!["structured_output".to_string()],
        )
        .await;
    assert_eq!(status, StatusCode::OK, "diagnose failed: {body}");
    assert_eq!(body["outcome"]["status"], "succeeded", "{body}");
    assert_eq!(
        body["outcome"]["structured_output"],
        json!({ "a": 1 }),
        "the request must be served by the provider that can honour the schema: {body}"
    );

    assert_eq!(
        case.providers[0].requests().await.len(),
        0,
        "the preferred-but-incapable DeepSeek provider must not have been called"
    );
    let served = case.providers[1].requests().await;
    assert_eq!(served.len(), 1, "the capable provider must have served it");
    assert_eq!(
        served[0].body["response_format"]["type"], "json_schema",
        "the schema must reach the provider that was chosen: {}",
        served[0].body
    );

    case.shutdown().await;
}
