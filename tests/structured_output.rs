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
//!    "the provider did not comply" from "the answer was empty". Four cases, because the property
//!    has four surfaces: the completion path, the streaming path, the public HTTP contract that is
//!    the whole point of the decision, and the public *stream*, where the same code cannot be a
//!    status and arrives as the terminal `response.failed` event instead.
//!    `StructuredOutputInvalid` is in neither `is_retryable` nor `is_fallback_eligible` nor
//!    `is_circuit_failure`, so the failure is terminal on the first non-conforming reply —
//!    asserted here as "the provider was called exactly once".
//! 5. **The refusal is metered.** The provider answered and billed for the answer, so the refused
//!    attempt still writes `execution_attempts.usage` and a `usage_records` row — as it did when
//!    the same request succeeded. Without it the flip would have opened a caller-reachable hole
//!    for unmetered provider spend, since routing a schema at a backend that does not honour it
//!    is caller-controlled input against a class Moira cannot verify.
//!
//!    All three of F29's preconditions were discharged first — F39 landed, the disposition is
//!    recorded and guarded in `src/orchestration/controls.rs` rather than true by omission, and
//!    `run_extraction` reads `execution.status`. The doc comment on `structured_output_from_text`
//!    in `src/application/execution.rs` carries the full argument, including the boundary: `null`,
//!    `{}` and `[]` all parse, so an empty *answer* is still a `200`.
//! 6. **The negative control for the metering guard — issue #155 A2.** Case 5 says a refused
//!    reply *is* metered; on its own that does not pin the `if` that keeps every *other* failure
//!    from being metered, and deleting it succeeds silently because migration `0005` makes all
//!    five token columns nullable. So an ordinary provider 500 is driven too, and asserted to
//!    write **no** `usage_records` row at all.
//! 7. **The empty controls — issue #155 A3.** The `null`/`{}`/`[]` boundary was pinned only by a
//!    unit test calling `structured_output_from_text` directly, so an edit anywhere else on the
//!    path could have turned an empty answer into a `422` with the tree green. All three now ride
//!    as public controls inside case 4's public case.

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
        self.diagnose_with(
            stream,
            ExecutionOptions {
                output_schema,
                required_capabilities,
                ..ExecutionOptions::default()
            },
        )
        .await
    }

    /// As [`Self::diagnose`], with the whole `ExecutionOptions` under the caller's control.
    ///
    /// `timeout_ms` and `stream` are still forced, because they have to agree with the `stream`
    /// argument and with this suite's budget. Everything else is the caller's — which is what
    /// lets a case pin `max_retries: 0` and so assert on an *exact* number of attempts rather
    /// than on whatever `maximum_retries_per_candidate` happens to be configured to.
    async fn diagnose_with(&self, stream: bool, options: ExecutionOptions) -> (StatusCode, Value) {
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

/// One `usage_records` row, which is what billing and `GET /api/v1/usage` read.
#[derive(Debug, sqlx::FromRow)]
struct UsageRow {
    input_tokens: Option<i64>,
    output_tokens: Option<i64>,
    total_tokens: Option<i64>,
    metadata: Value,
}

/// **The refused reply is still metered — issue #80 review.**
///
/// This is the one property of the flip that is about money rather than about correctness, and it
/// is the one a reader is most likely to assume comes for free. It does not: the failure lands on
/// the attempt loop's failure arm, which records `UsageSummary::default()` and writes no
/// `usage_records` row for every *other* failure it handles. Before the flip this exact request
/// **succeeded**, so the provider's counts reached `execution_attempts.usage` and `usage_records`
/// through the success arm. A fail-hard implementation that simply raised the failure would
/// therefore have turned a metered request into unmetered provider spend — reachable on demand by
/// sending a schema to a backend that does not honour it, which is caller-controlled input
/// (`route`, `provider`, `model`) against an unverifiable backend class.
///
/// Both paths are driven, because the usage is carried at two independent call sites — one in
/// `execute_rig_completion`, one at the end of `execute_rig_stream` — and reverting either alone
/// must red this case. The mock reports `prompt_tokens: 2, completion_tokens: 1, total_tokens: 3`
/// on both, so the assertion is on the exact counts rather than on "a row exists": a row built
/// from `UsageSummary::default()` is all-`NULL`, and all-`NULL` is precisely the shape this case
/// exists to reject.
///
/// The `usage_records` row is the load-bearing one — it is what billing and quota read
/// (`GET /api/v1/usage`, `docs/execution-and-usage-api.md`) — but the outcome document is
/// asserted too, because finding F38 was an outcome that contradicted its own `attempts` array
/// about exactly this field.
#[tokio::test]
async fn a_refused_reply_is_still_metered_for_the_tokens_the_provider_billed() {
    for stream in [false, true] {
        let script = if stream {
            ProviderScript::Stream {
                deltas: vec!["I am afraid ".to_string(), "I cannot do that.".to_string()],
            }
        } else {
            ProviderScript::Completion {
                text: NON_CONFORMING_REPLY.to_string(),
            }
        };
        let Some(case) = Case::new(vec![script]).await else {
            return;
        };

        let (status, body) = case.diagnose(stream, Some(trivial_object_schema())).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "diagnose failed (stream={stream}): {body}"
        );
        assert_eq!(
            body["outcome"]["failure"]["class"], "structured_output_invalid",
            "control: this case is only about the refusal path (stream={stream}): {body}"
        );
        let execution_id: uuid::Uuid = body["outcome"]["execution_id"]
            .as_str()
            .expect("execution id")
            .parse()
            .expect("execution id is a uuid");

        let usage_rows: Vec<UsageRow> = sqlx::query_as(
            "select input_tokens, output_tokens, total_tokens, metadata from usage_records \
             where execution_id = $1",
        )
        .bind(execution_id)
        .fetch_all(&case.fixture.pool)
        .await
        .expect("read usage_records");
        assert_eq!(
            usage_rows.len(),
            1,
            "a provider call that was answered and billed must be metered even when Moira \
             refuses the answer; no row here is unmetered provider spend a caller can summon \
             (stream={stream}): {body}"
        );
        let metered = &usage_rows[0];
        assert_eq!(
            (
                metered.input_tokens,
                metered.output_tokens,
                metered.total_tokens
            ),
            (Some(2), Some(1), Some(3)),
            "the row must carry the counts the provider reported, not the all-NULL row a \
             defaulted UsageSummary writes (stream={stream})"
        );
        assert_eq!(
            metered.metadata["attempt_outcome"], "failed",
            "billing must be able to tell a metered refusal from a metered answer without \
             joining back to execution_attempts (stream={stream}): {}",
            metered.metadata
        );
        assert_eq!(
            metered.metadata["failure_class"], "structured_output_invalid",
            "and it must say which refusal (stream={stream}): {}",
            metered.metadata
        );

        let attempts: Vec<(String, Option<i64>)> = sqlx::query_as(
            "select status, total_tokens from execution_attempts where execution_id = $1",
        )
        .bind(execution_id)
        .fetch_all(&case.fixture.pool)
        .await
        .expect("read execution_attempts");
        assert_eq!(attempts.len(), 1, "exactly one attempt (stream={stream})");
        assert_eq!(attempts[0].0, "failed", "(stream={stream})");
        assert_eq!(
            attempts[0].1,
            Some(3),
            "the attempt row is what an operator reads per attempt, and it must not disagree \
             with the usage record written from the same counts (stream={stream})"
        );

        assert_eq!(
            body["outcome"]["usage"]["total_tokens"], 3,
            "the outcome must report what the execution cost; F38 was this field hardcoded to \
             all-null next to an attempts array that knew better (stream={stream}): {body}"
        );
        assert_eq!(
            body["outcome"]["attempts"][0]["usage"]["total_tokens"], 3,
            "and the two halves of the document must agree (stream={stream}): {body}"
        );

        case.shutdown().await;
    }
}

/// The error body the mock returns with its `500`.
///
/// Deliberately not a JSON document and not a token count: the point of the case below is a
/// failure the provider reported *nothing* measurable about, which is the input
/// `usage_was_reported` reads.
const PROVIDER_FAILURE_BODY: &str = "the provider fell over";

/// **The negative control for the metering guard — issue #155 A2.**
///
/// The case above proves a *refused* reply is metered. On its own that is only half a property:
/// the mechanism that keeps it from metering **every** failure is a single `if` —
/// `usage_was_reported(&usage)` in the attempt loop's failure arm — and deleting it succeeds
/// silently. Migration `0005` makes all five token columns nullable, so an ordinary provider
/// failure would write an all-`NULL` `usage_records` row instead of erroring, and no test in the
/// tree went red. That edit inverts what the table means: `usage_records` has only ever held
/// attempts that were billed, and a billing job that counts rows rather than summing tokens would
/// start charging for every 500 the deployment absorbs.
///
/// The two `usage_records is empty` assertions in `tests/execution_lifecycle.rs` are not this
/// guard. They are about the terminal-persistence deadline — a *successful* execution whose
/// writes were cut short — and they run on a path that never reaches this arm.
///
/// # The three controls, and what each one rules out
///
/// 1. **The provider was called exactly once.** "No usage row" is also what a request refused at
///    admission produces — `no_eligible_model`, a bad credential, a route that does not resolve —
///    and such a request never reaches the arm this case is about. One call proves it did.
/// 2. **Exactly one `execution_attempts` row, and its status is `failed`.** The attempt row is
///    written by `complete_failed_attempt`, immediately above the `if` under test, so its
///    presence is what makes the absence below meaningful: the arm ran, and chose not to meter.
/// 3. **That row's `total_tokens` is `NULL`.** This is the reading `usage_was_reported` acts on —
///    all-`None` is *unknown*, not zero — stated as a fact about this fixture rather than assumed.
///    If the mock ever started reporting counts on an error response, the property under test
///    would change meaning and this assertion is what would say so.
///
/// `max_retries: 0` is set so "exactly one" is exact: with the default retry budget a retryable
/// class would produce several attempts, and the case would have to assert a count it does not
/// control. `allow_fallback: false` does the same for the candidate list.
#[tokio::test]
async fn a_plain_provider_failure_writes_no_usage_record_at_all() {
    let Some(case) = Case::new(vec![ProviderScript::HttpError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        body: PROVIDER_FAILURE_BODY.to_string(),
    }])
    .await
    else {
        return;
    };

    let (status, body) = case
        .diagnose_with(
            false,
            ExecutionOptions {
                max_retries: Some(0),
                max_fallbacks: Some(0),
                allow_fallback: false,
                ..ExecutionOptions::default()
            },
        )
        .await;
    assert_eq!(status, StatusCode::OK, "diagnose failed: {body}");
    assert_eq!(
        body["outcome"]["status"], "failed",
        "control: this case is about an ordinary provider failure, so the execution must have \
         failed: {body}"
    );
    assert_eq!(
        body["outcome"]["failure"]["class"], "provider_unavailable",
        "control: a 5xx from the provider, not a structured-output refusal — those are metered, \
         and confusing the two would make this case assert the opposite of what it means: {body}"
    );
    let execution_id: uuid::Uuid = body["outcome"]["execution_id"]
        .as_str()
        .expect("execution id")
        .parse()
        .expect("execution id is a uuid");

    assert_eq!(
        case.provider().requests().await.len(),
        1,
        "control: the provider must actually have been called, or 'no usage row' is just what a \
         request refused before any attempt looks like: {body}"
    );

    let attempts: Vec<(String, Option<i64>)> = sqlx::query_as(
        "select status, total_tokens from execution_attempts where execution_id = $1",
    )
    .bind(execution_id)
    .fetch_all(&case.fixture.pool)
    .await
    .expect("read execution_attempts");
    assert_eq!(
        attempts.len(),
        1,
        "control: exactly one attempt, so the absence below is about one trip through the \
         failure arm rather than an execution that never made one: {attempts:?}"
    );
    assert_eq!(attempts[0].0, "failed", "{attempts:?}");
    assert_eq!(
        attempts[0].1, None,
        "control: the provider reported no counts, which is the input usage_was_reported reads; \
         a mock that started reporting them on an error would change what this case tests"
    );

    let metered: i64 =
        sqlx::query_scalar("select count(*) from usage_records where execution_id = $1")
            .bind(execution_id)
            .fetch_one(&case.fixture.pool)
            .await
            .expect("count usage_records");
    assert_eq!(
        metered, 0,
        "a provider failure that billed nothing must be metered nowhere: an all-NULL row here is \
         a billing record asserting zero tokens for a call that never completed, and it inverts \
         what usage_records has always meant — see issue #155 A2 and usage_was_reported in \
         src/application/execution.rs"
    );

    case.shutdown().await;
}

/// One schema-carrying public request, answered by `script`, against a fixture wired for
/// structured output.
///
/// `path` selects the endpoint — `responses` or `responses/stream` — because the two publish
/// *different* contracts for the same failure (a `422` status versus a `200` carrying a terminal
/// `response.failed` event) and the difference is exactly what the streaming case has to observe.
///
/// Returns `(status, raw body, provider call count)`. The body is returned as a string rather than
/// parsed so a case can assert on the *bytes* the caller received — which is how "the provider's
/// reply is not echoed anywhere in the envelope" is checked, including in fields a targeted
/// assertion would not look at; for the stream it is also the raw SSE frames.
///
/// The count is taken after the response, so "one call" covers the whole request rather than the
/// part before the failure.
async fn public_request(path: &str, script: ProviderScript) -> Option<(StatusCode, String, usize)> {
    let fixture = LifecycleFixture::new().await?;
    let provider = MockOpenAiServer::start(vec![script]).await;
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
            .post(format!("{}/api/v1/{path}", moira.base_url))
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

/// The non-streaming public request, which is what the `422` contract is about.
async fn public_response(reply: &str) -> Option<(StatusCode, String, usize)> {
    public_request(
        "responses",
        ProviderScript::Completion {
            text: reply.to_string(),
        },
    )
    .await
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
/// with the same status. The conforming control proves this fixture reaches the provider and that
/// its reply is accepted — asserted as `200`, one provider call, `status: completed` and the
/// provider's own document in the output text, since `PublicResponse` has no structured-output
/// field to read the parsed value from. The refusal is therefore attributable to the reply rather
/// than to the request or to the wiring — the vacuous-pass shape HANDOFF §2.3 keeps finding.
///
/// # The empty controls — issue #155 A3
///
/// The half of issue #80 most worth protecting is the boundary: `null`, `{}` and `[]` are all
/// valid JSON, so an *empty answer* is still a `200`. Until issue #155 that was pinned only by
/// `a_schema_carrying_reply_that_is_not_json_is_a_failure_rather_than_a_none`, a unit test that
/// calls `structured_output_from_text` directly — so any edit **outside** that function would have
/// turned legitimately empty answers into `422`s with the whole tree green. The one-line version
/// is `if structured_output.as_ref().is_none_or(Value::is_null) { return Err(..) }` in
/// `execute_rig_completion`, which reads like a tightening of exactly the decision this file is
/// about and is wrong.
///
/// All three documented empties are driven rather than one, because the plausible edits do not
/// all catch the same value: a null check misses `{}`, an emptiness check on objects misses
/// `null`, and a truthiness check catches all three. Each is a complete public request against a
/// fresh fixture, which is the cost of asserting this where a caller can see it.
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
    // answered `200`, carrying the provider's own document, or the refusal above is
    // unattributable.
    const CONFORMING_REPLY: &str = "{\"a\":1}";
    let Some((status, body, calls)) = public_response(CONFORMING_REPLY).await else {
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
    let control: Value = serde_json::from_str(&body).expect("public response envelope");
    assert_eq!(
        control["status"], "completed",
        "a 200 whose response is not `completed` would not show that the schema-carrying request \
         was actually answered: {body}"
    );
    // `PublicResponse` has no structured-output field — that is why the three cases above use the
    // diagnostic endpoint — so the observable proof that the reply was accepted rather than
    // merely tolerated is the provider's document arriving as the output text.
    assert_eq!(
        control["output"][0]["content"][0]["text"], CONFORMING_REPLY,
        "the control must return the provider's own reply, or 'this fixture reaches the provider \
         and its reply is accepted' is asserted only by the status code: {body}"
    );

    // The empty controls. An answer with nothing in it is still an answer: these three are the
    // exact values `structured_output_from_text`'s doc comment names as the boundary of the
    // decision, and each one must come back as a `200` carrying itself.
    for empty_reply in ["{}", "null", "[]"] {
        let Some((status, body, calls)) = public_response(empty_reply).await else {
            return;
        };
        assert_eq!(
            status,
            StatusCode::OK,
            "issue #80 refuses replies that are not JSON, not replies that are empty; `{empty_reply}` \
             is valid JSON and must still be a 200: {body}"
        );
        assert_eq!(
            calls, 1,
            "the empty control must reach the provider (reply={empty_reply}): {body}"
        );
        let empty: Value = serde_json::from_str(&body).expect("public response envelope");
        assert_eq!(
            empty["status"], "completed",
            "an empty answer completes; a 200 that is not `completed` would mean the request was \
             refused somewhere the status code cannot show (reply={empty_reply}): {body}"
        );
        assert_eq!(
            empty["output"][0]["content"][0]["text"], empty_reply,
            "and it must carry the provider's own document, so this cannot pass against a \
             response that succeeded with the answer discarded (reply={empty_reply}): {body}"
        );
    }
}

/// **The other half of the published contract: on the stream the refusal is an event, not a
/// status.**
///
/// `docs/release-notes.md` promises operators two different things for one failure — `422` on
/// `POST /api/v1/responses`, and `200` on `POST /api/v1/responses/stream` with the same code
/// arriving as the terminal `response.failed` event *after the deltas the caller has already
/// received*. The case above pins the first. Without this one the second is published and
/// unguarded: the response head is written before the execution starts, so a stream cannot report
/// this failure the way the non-streaming path does, and the plausible half-done shapes — ending
/// the stream with no terminal event, or reporting `response.completed` with no output — are
/// invisible to every other case in this file.
///
/// **The deltas are asserted to have arrived**, which is the uncomfortable part of the decision
/// stated as behaviour: the caller was streamed prose and the request is failed anyway, because
/// the deltas were text and the caller asked for a value. It is also what distinguishes this from
/// a request refused before it ran — a schema rejection produces the same terminal code with no
/// deltas at all.
#[tokio::test]
async fn a_public_stream_carries_the_refusal_as_its_terminal_event_after_the_deltas() {
    let Some((status, body, calls)) = public_request(
        "responses/stream",
        ProviderScript::Stream {
            deltas: vec!["I am afraid ".to_string(), "I cannot do that.".to_string()],
        },
    )
    .await
    else {
        return;
    };
    assert_eq!(
        status,
        StatusCode::OK,
        "the transport still succeeds: the head is written before the execution starts, so the \
         refusal has to arrive on the stream: {body}"
    );
    assert_eq!(calls, 1, "exactly one provider call: {body}");

    let events: Vec<Value> = body
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter_map(|data| serde_json::from_str::<Value>(data).ok())
        .collect();
    let deltas: Vec<&Value> = events
        .iter()
        .filter(|event| event["type"] == "response.output_text.delta")
        .collect();
    assert!(
        !deltas.is_empty(),
        "the caller must already have received the text before the refusal, or this case is not \
         about the streaming decision at all: {events:?}"
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
        failed[0]["payload"]["error"]["code"], "structured_output_invalid",
        "and it must carry the same code the non-streaming path returns as a 422: {:?}",
        failed[0]
    );
    assert!(
        !events
            .iter()
            .any(|event| event["type"] == "response.completed"),
        "a refused stream must not also report completion: {events:?}"
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
