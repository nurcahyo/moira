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
//! 4. **No fail-hard.** A schema-carrying request whose reply is prose must still succeed with
//!    the prose in `output_text`. `StructuredOutputInvalid` is in neither `is_retryable` nor
//!    `is_fallback_eligible` nor `is_circuit_failure`, so failing here would kill the execution
//!    with no retry and no fallback — and on DeepSeek, where Rig drops the schema before the
//!    wire (finding F39), it would fail *every* structured request. See the ledger's F29 entry.
//!
//!    **This is now a policy choice rather than a blocked one.** All three of F29's preconditions
//!    have been discharged — F39 landed, the disposition above is recorded and guarded in
//!    `src/orchestration/controls.rs` rather than merely true by omission, and `run_extraction`
//!    reads `execution.status`. The fail-hard variant is deliberately left unshipped so that the
//!    blast-radius decision gets its own diff; the two cases below (and the streaming twin) are
//!    what it has to replace when it does. The doc comment on `structured_output_from_text` in
//!    `src/application/execution.rs` carries the full argument.

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

/// **No fail-hard.** A non-conforming reply leaves the field `null` and changes nothing else.
///
/// The tripwire for anyone who adopts the fail-hard variant without also doing F39: this case
/// and `an_unparseable_extraction_reply_fails_the_run_and_writes_no_memory` both go red.
///
/// **F42 — this case is also what makes the `moira.error.structured_output_invalid` catalog
/// entry true.** That entry used to assert a second emitter, "or the model's output does not
/// conform to it", and there is none: both real emitters reject the *caller's schema*
/// (`validate_response_format`, `build_completion_request`). The catalog description now says
/// so, and this is the assertion that would have to change first if it ever stopped being so —
/// hence the pointer in the failure message below. A prose claim nothing observes is not a
/// claim; this is the thing that observes it.
#[tokio::test]
async fn a_reply_that_is_not_json_leaves_the_field_null_and_still_succeeds() {
    let Some(case) = Case::new(vec![ProviderScript::Completion {
        text: "I am afraid I cannot do that.".to_string(),
    }])
    .await
    else {
        return;
    };

    let (status, body) = case.diagnose(false, Some(trivial_object_schema())).await;
    assert_eq!(status, StatusCode::OK, "diagnose failed: {body}");
    assert_eq!(
        body["outcome"]["status"], "succeeded",
        "a non-conforming reply must not fail the execution. If this is now intentional (the \
         fail-hard variant), widen the moira.error.structured_output_invalid description in \
         src/i18n/catalog/errors.rs AND docs/i18n-response-catalog.json in the same change — it \
         currently states that no model-output-non-conformance path exists (F42): {body}"
    );
    assert_eq!(body["outcome"]["structured_output"], Value::Null, "{body}");
    assert_eq!(
        body["outcome"]["output_text"], "I am afraid I cannot do that.",
        "{body}"
    );
    assert_eq!(body["outcome"]["failure"], Value::Null, "{body}");

    case.shutdown().await;
}

/// **No fail-hard, on the stream.** F42 — added because the suite's own header argues for it and
/// then did not do it.
///
/// The header says `execute_rig_stream` "is a genuinely separate code path, and a fix applied at
/// the Rig boundary would cover only case 1". That argument was applied to the *conforming*
/// reply (case 2) and not to the non-conforming one, which left the cheapest falsifying edit
/// unguarded: adding the fail-hard variant to the **streaming arm only** leaves all seven
/// existing cases green — case 2 sends conforming JSON and never reaches the branch, and case 4
/// never streams. Verified by running it, not by reading.
///
/// This case and the completion twin above are what make the
/// `moira.error.structured_output_invalid` catalog entry's "no model-output-non-conformance
/// path exists" true on *both* execution paths rather than on the one that was easy to write.
#[tokio::test]
async fn a_stream_whose_reply_is_not_json_leaves_the_field_null_and_still_succeeds() {
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
        body["outcome"]["status"], "succeeded",
        "a non-conforming streamed reply must not fail the execution either. If this is now \
         intentional (the fail-hard variant), widen the moira.error.structured_output_invalid \
         description in src/i18n/catalog/errors.rs AND docs/i18n-response-catalog.json in the \
         same change — it currently states that no model-output-non-conformance path exists \
         (F42): {body}"
    );
    assert_eq!(body["outcome"]["structured_output"], Value::Null, "{body}");
    assert_eq!(
        body["outcome"]["output_text"], "I am afraid I cannot do that.",
        "the accumulated text must survive unchanged: {body}"
    );
    assert_eq!(body["outcome"]["failure"], Value::Null, "{body}");

    case.shutdown().await;
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
