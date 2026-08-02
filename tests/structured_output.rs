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

mod support;

use std::{sync::Arc, time::Duration};

use axum::http::StatusCode;
use moira::domain::{DiagnosticExecutionRequest, ExecutionOptions};
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

struct Case {
    fixture: LifecycleFixture,
    provider: MockOpenAiServer,
    moira: MoiraHttpServer,
    client: reqwest::Client,
}

impl Case {
    async fn new(scripts: Vec<ProviderScript>) -> Option<Self> {
        let fixture = LifecycleFixture::new().await?;
        let provider = MockOpenAiServer::start(scripts).await;
        fixture
            .add_provider(provider.base_url(), 10, RuntimePolicy::default())
            .await;
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
            provider,
            moira,
            client: reqwest::Client::new(),
        })
    }

    /// Posts one diagnostic execution and returns `(status, body)`.
    ///
    /// The body is built from the typed `DiagnosticExecutionRequest` rather than hand-written
    /// JSON: the DTO carries `#[serde(deny_unknown_fields)]`, so a hand-written body that
    /// drifted from the struct would fail as a 422 that looks like a routing problem.
    async fn diagnose(&self, stream: bool, output_schema: Option<Value>) -> (StatusCode, Value) {
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
        self.provider.shutdown().await;
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
    let requests = case.provider.requests().await;
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
        "a non-conforming reply must not fail the execution: {body}"
    );
    assert_eq!(body["outcome"]["structured_output"], Value::Null, "{body}");
    assert_eq!(
        body["outcome"]["output_text"], "I am afraid I cannot do that.",
        "{body}"
    );
    assert_eq!(body["outcome"]["failure"], Value::Null, "{body}");

    case.shutdown().await;
}
