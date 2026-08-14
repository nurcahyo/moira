//! F35 and F46 — response-format handling on both public endpoints, over real HTTP and out to
//! a real provider socket.
//!
//! The unit tests in `src/application/public.rs` prove the *translation* and the *predicate*.
//! These prove the *wiring*: that the decision survives policy validation, routing and the Rig
//! boundary, and either lands on the provider request as `response_format` or never leaves
//! Moira at all. HANDOFF §3.4 is explicit that a predicate test and a wiring test are different
//! tests, and that every laundering finding in this repository has had a correct predicate.
//!
//! The two files' subjects are coupled on purpose. F35 refused `text.format.json_object` on the
//! compat endpoint *because* the native path translated it into a schema satisfied only by
//! `{}`; F46 removed that translation. The native cases live here so the next person to revisit
//! either refusal sees both in one place.
//!
//! This file previously ended with a test that guarded nothing and documented the native defect
//! instead — `documents_native_json_object_reaching_the_provider_as_an_empty_object_schema`. It
//! was replaced by the native guards below when F46 was fixed, which is the outcome its own doc
//! comment predicted.

mod support;

use moira::{
    app::AppState,
    application::RuntimeAdminService,
    config::Settings,
    domain::{
        AgentProfileCreateRequest, AgentProfileRecord, RouteDefinitionPatchRequest,
        RoutingPolicyCreateRequest,
    },
};
use reqwest::StatusCode;
use serde_json::{Value, json};
use support::{
    LifecycleFixture, MoiraHttpServer, ProviderFixture, RuntimePolicy,
    mock_openai::{MockOpenAiServer, ProviderScript},
};
use uuid::Uuid;

/// A model that advertises structured output, which routing requires for any non-`text`
/// response format.
fn structured_output_model() -> Value {
    json!({ "streaming": true, "structured_output": true })
}

/// `POST /v1/responses` carries no route — the compat DTO has no field for one — so
/// `DefaultTaskRouter` falls through to `get_default_route`, which migration `0005` seeds as
/// `general` and which its `route_key = 'general'` ordering clause always prefers over a
/// fixture's own route. Binding the provider to `general` is therefore not test scaffolding
/// convenience: it is the only route a compat request can ever reach.
async fn bind_provider_to_the_default_route(
    fixture: &LifecycleFixture,
    provider: &ProviderFixture,
) {
    let route_id = default_route_id(fixture).await;
    RuntimeAdminService::new(&fixture.state)
        .expect("runtime admin service")
        .create_routing_policy(
            &fixture.actor,
            &support::request_context(),
            RoutingPolicyCreateRequest {
                application_id: Some(fixture.application_id),
                external_tenant_id: None,
                route_id,
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
                timeout_ms: Some(2_000),
                retry_policy: json!({}),
                metadata: json!({ "test_fixture": true }),
            },
        )
        .await
        .expect("bind the default route to the mock provider");
}

/// The `general` route seeded by migration `0005` — the only route a compat request can reach.
async fn default_route_id(fixture: &LifecycleFixture) -> Uuid {
    sqlx::query_scalar(
        "select id from route_definitions where route_key = 'general' and deleted_at is null",
    )
    .fetch_one(&fixture.pool)
    .await
    .expect("migration 0005 seeds the 'general' route")
}

async fn compat_fixture() -> Option<LifecycleFixture> {
    LifecycleFixture::with_settings(|settings| {
        settings.public_api.openai_responses_compat_enabled = true;
    })
    .await
}

fn caller_schema() -> Value {
    json!({
        "type": "object",
        "title": "caller_title",
        "properties": { "answer": { "type": "string" } },
        "required": ["answer"],
        "additionalProperties": false
    })
}

/// The whole of F35 in one assertion: a caller's `text.format.json_schema` reaches the
/// provider.
///
/// Before the fix this returned 200 with `response_format` absent from the provider request
/// entirely — the caller's schema was accepted by `deny_unknown_fields`, read by nothing,
/// and answered with prose.
#[tokio::test]
async fn compat_text_format_json_schema_reaches_the_provider_as_response_format() {
    let Some(fixture) = compat_fixture().await else {
        return;
    };
    let provider = MockOpenAiServer::start([ProviderScript::Completion {
        text: "{\"answer\":\"yes\"}".to_string(),
    }])
    .await;
    let bound = fixture
        .add_provider_with_capabilities(
            provider.base_url(),
            10,
            RuntimePolicy::default(),
            structured_output_model(),
        )
        .await;
    bind_provider_to_the_default_route(&fixture, &bound).await;
    let consumer_key = fixture.enable_public_streaming().await;
    let moira = MoiraHttpServer::start(fixture.state.clone()).await;

    let response = reqwest::Client::new()
        .post(format!("{}/v1/responses", moira.base_url))
        .header("x-consumer-key", &consumer_key)
        .json(&json!({
            "input": "answer me",
            "text": { "format": {
                "type": "json_schema",
                "name": "answer",
                "schema": caller_schema(),
                "strict": true
            } }
        }))
        .send()
        .await
        .expect("send compat request");
    let status = response.status();
    let body = response.text().await.expect("compat response body");
    assert_eq!(status, StatusCode::OK, "compat request failed: {body}");

    let requests = provider.requests().await;
    assert_eq!(
        requests.len(),
        1,
        "the provider must have been called exactly once"
    );
    let sent = &requests[0].body;
    assert_eq!(
        sent["response_format"]["type"], "json_schema",
        "the caller's schema never reached the provider: {sent}"
    );
    let sent_schema = &sent["response_format"]["json_schema"]["schema"];
    assert_eq!(
        sent_schema["properties"]["answer"]["type"], "string",
        "the provider received a different schema from the caller's: {sent}"
    );

    moira.shutdown().await;
    provider.shutdown().await;
}

/// `json_object` is refused over the wire, with a coded envelope rather than a 200 and prose.
#[tokio::test]
async fn compat_text_format_json_object_is_refused_over_http() {
    let Some(fixture) = compat_fixture().await else {
        return;
    };
    let provider = MockOpenAiServer::start([ProviderScript::Completion {
        text: "must-not-be-reached".to_string(),
    }])
    .await;
    let bound = fixture
        .add_provider_with_capabilities(
            provider.base_url(),
            10,
            RuntimePolicy::default(),
            structured_output_model(),
        )
        .await;
    bind_provider_to_the_default_route(&fixture, &bound).await;
    let consumer_key = fixture.enable_public_streaming().await;
    let moira = MoiraHttpServer::start(fixture.state.clone()).await;

    let response = reqwest::Client::new()
        .post(format!("{}/v1/responses", moira.base_url))
        .header("x-consumer-key", &consumer_key)
        .json(&json!({
            "input": "answer me",
            "text": { "format": { "type": "json_object" } }
        }))
        .send()
        .await
        .expect("send json_object compat request");
    let status = response.status();
    let body: Value = response.json().await.expect("json_object error envelope");
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "json_object must be refused, got {status}: {body}"
    );
    assert_eq!(body["error"]["code"], "unsupported_request_option");
    assert_eq!(
        body["error"]["message_key"],
        "moira.error.unsupported_request_option"
    );

    // The refusal is pre-dispatch: nothing was sent upstream and nothing was billed.
    assert_eq!(
        provider.call_count().await,
        0,
        "a refused request must not reach the provider"
    );

    moira.shutdown().await;
    provider.shutdown().await;
}

/// A `text` key Moira does not honour is refused rather than accepted and ignored. `verbosity`
/// is a real OpenAI Responses option, so this is the exact shape F35 described — an option a
/// caller genuinely sends, which Moira does not implement.
#[tokio::test]
async fn compat_text_options_moira_does_not_honour_are_refused_over_http() {
    let Some(fixture) = compat_fixture().await else {
        return;
    };
    let provider = MockOpenAiServer::start([ProviderScript::Completion {
        text: "must-not-be-reached".to_string(),
    }])
    .await;
    let bound = fixture
        .add_provider_with_capabilities(
            provider.base_url(),
            10,
            RuntimePolicy::default(),
            structured_output_model(),
        )
        .await;
    bind_provider_to_the_default_route(&fixture, &bound).await;
    let consumer_key = fixture.enable_public_streaming().await;
    let moira = MoiraHttpServer::start(fixture.state.clone()).await;

    let response = reqwest::Client::new()
        .post(format!("{}/v1/responses", moira.base_url))
        .header("x-consumer-key", &consumer_key)
        .json(&json!({ "input": "answer me", "text": { "verbosity": "low" } }))
        .send()
        .await
        .expect("send verbosity compat request");
    let status = response.status();
    let body = response.text().await.expect("verbosity response body");
    assert!(
        status.is_client_error(),
        "an unhonoured text option must not be accepted, got {status}: {body}"
    );
    assert_eq!(
        provider.call_count().await,
        0,
        "a refused request must not reach the provider"
    );

    moira.shutdown().await;
    provider.shutdown().await;
}

/// A `text.format` naming prose keeps working exactly as before. Rejecting a request whose
/// semantics Moira already satisfies would be a regression, not a fix.
#[tokio::test]
async fn compat_text_format_text_still_returns_prose() {
    let Some(fixture) = compat_fixture().await else {
        return;
    };
    let provider = MockOpenAiServer::start([ProviderScript::Completion {
        text: "plain prose".to_string(),
    }])
    .await;
    let bound = fixture
        .add_provider(provider.base_url(), 10, RuntimePolicy::default())
        .await;
    bind_provider_to_the_default_route(&fixture, &bound).await;
    let consumer_key = fixture.enable_public_streaming().await;
    let moira = MoiraHttpServer::start(fixture.state.clone()).await;

    let response = reqwest::Client::new()
        .post(format!("{}/v1/responses", moira.base_url))
        .header("x-consumer-key", &consumer_key)
        .json(&json!({
            "input": "answer me",
            "text": { "format": { "type": "text" } }
        }))
        .send()
        .await
        .expect("send text compat request");
    let status = response.status();
    let body = response.text().await.expect("text response body");
    assert_eq!(status, StatusCode::OK, "text format failed: {body}");

    let requests = provider.requests().await;
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0].body.get("response_format").is_none(),
        "a text format must not constrain the provider: {}",
        requests[0].body
    );

    moira.shutdown().await;
    provider.shutdown().await;
}

/// One native `POST /api/v1/responses` against a fixture wired for structured output.
///
/// Returns `(status, body, provider_call_count)`. The count is taken *after* the response, so
/// "no provider call" means no call was made during the whole request, not merely before it.
async fn native_response(
    stream: bool,
    response_format: Value,
) -> Option<(StatusCode, String, usize)> {
    let fixture = LifecycleFixture::new().await?;
    // A script with one entry: if the request were dispatched despite the refusal, the mock has
    // a reply ready and would answer 200. The refusal is not being proven by an empty script.
    let provider = MockOpenAiServer::start([ProviderScript::Completion {
        text: "{}".to_string(),
    }])
    .await;
    fixture
        .add_provider_with_capabilities(
            provider.base_url(),
            10,
            RuntimePolicy::default(),
            structured_output_model(),
        )
        .await;
    let consumer_key = fixture.enable_public_streaming().await;
    let moira = MoiraHttpServer::start(fixture.state.clone()).await;

    let path = if stream {
        "/api/v1/responses/stream"
    } else {
        "/api/v1/responses"
    };
    let response = reqwest::Client::new()
        .post(format!("{}{path}", moira.base_url))
        .header("x-consumer-key", &consumer_key)
        .json(&json!({
            "input": [{ "role": "user", "content": [{ "type": "input_text", "text": "answer me" }] }],
            "route": fixture.route_key,
            "response_format": response_format
        }))
        .send()
        .await
        .expect("send native request");
    let status = response.status();
    let body = response.text().await.expect("native response body");
    let calls = provider.call_count().await;

    moira.shutdown().await;
    provider.shutdown().await;
    Some((status, body, calls))
}

/// F46 — the whole finding in one case, on the native endpoint that carried it.
///
/// Before the fix this returned **200 `succeeded`** with the provider request carrying
/// `{"type":"object","properties":{},"additionalProperties":false,"required":[]}` under
/// `strict: true` — a schema satisfied by exactly one document, `{}`, for a caller who asked
/// for free-form JSON.
///
/// **Why the control case is in the same test.** Asserting "422 and the provider was never
/// called" is satisfiable by a fixture that could not reach the provider *at all* — a routing
/// failure raises 422 too, and `call_count() == 0` would then be true for the wrong reason.
/// HANDOFF §3.4's toothless guards all had that shape. The `json_schema` control proves the
/// same fixture does reach the provider, so the zero is attributable to the refusal. The two
/// halves are asserted as one fact, not two independent ones.
#[tokio::test]
async fn native_json_object_is_refused_and_never_reaches_the_provider() {
    let Some((status, body, calls)) =
        native_response(false, json!({ "type": "json_object" })).await
    else {
        return;
    };
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "json_object must be refused on the native path, got {status}: {body}"
    );
    let envelope: Value = serde_json::from_str(&body).expect("coded error envelope");
    assert_eq!(
        envelope["error"]["code"], "unsupported_request_option",
        "the refusal must be the json_object one, not a routing or policy failure: {body}"
    );
    assert_eq!(
        envelope["error"]["message_key"], "moira.error.unsupported_request_option",
        "{body}"
    );
    assert_eq!(
        calls, 0,
        "a refused request must not reach the provider or be billed: {body}"
    );

    // The control. Same fixture, same route, same model — a format Moira does honour must
    // still reach the provider, or the assertions above prove only that the fixture is broken.
    let Some((status, body, calls)) = native_response(
        false,
        json!({ "type": "json_schema", "name": "answer", "schema": caller_schema() }),
    )
    .await
    else {
        return;
    };
    assert_eq!(
        status,
        StatusCode::OK,
        "the control request must succeed, or the refusal above is unattributable: {body}"
    );
    assert_eq!(
        calls, 1,
        "the control request must reach the provider: {body}"
    );
}

/// The streaming twin, and it guards a distinct failure mode rather than repeating the last one.
///
/// `POST /api/v1/responses/stream` answers with SSE. A refusal raised after the response head is
/// written cannot be a 422 — it becomes a `200 text/event-stream` carrying an error event, which
/// is precisely the "200 that contradicts the request" shape F46 is about. Only asserting the
/// status on the streaming path proves the refusal happens before the stream begins.
#[tokio::test]
async fn native_json_object_is_refused_before_the_stream_begins() {
    let Some((status, body, calls)) = native_response(true, json!({ "type": "json_object" })).await
    else {
        return;
    };
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "the streaming path must refuse before the SSE head is written, got {status}: {body}"
    );
    let envelope: Value = serde_json::from_str(&body).expect("coded error envelope");
    assert_eq!(
        envelope["error"]["code"], "unsupported_request_option",
        "{body}"
    );
    assert_eq!(
        calls, 0,
        "a refused stream must not reach the provider: {body}"
    );
}

// ---------------------------------------------------------------------------------------------
// F45 — `json_schema.strict = false` is refused rather than accepted and inverted.
// ---------------------------------------------------------------------------------------------

/// A schema with an **optional** property, so the strict-mode rewrite is observable.
///
/// `caller_schema()` declares its only property required, which makes it useless for seeing what
/// strict mode does to a schema: `required` comes back identical either way. This one declares
/// `note` optional, and `sanitize_schema` promotes it.
fn schema_with_an_optional_property() -> Value {
    json!({
        "type": "object",
        "title": "caller_title",
        "properties": {
            "answer": { "type": "string" },
            "note": { "type": "string" }
        },
        "required": ["answer"],
        "additionalProperties": false
    })
}

/// **The premise, measured on the socket rather than cited.**
///
/// F45's refusal rests on two facts about `rig-core` 0.40 that a reader has to take on trust
/// otherwise: `"strict": true` is hardcoded in the OpenAI encoder, and `sanitize_schema`
/// rewrites `required` to the full property-key list. This drives Moira's real request through
/// Rig's real encoder to a real socket and reads what left the process.
///
/// The second assertion is the one that matters, and it is why `strict: false` is refused rather
/// than documented: **the caller's optional property came back mandatory.** A caller asking for
/// non-strict validation is not asking for a slightly stricter version of their schema; they are
/// asking for a schema Moira will not send. `additionalProperties` is asserted too because
/// OpenAI's strict mode requires it and a schema that omitted it would be rewritten as well.
///
/// A red here means `rig-core` changed and F45's premise no longer holds — which is precisely
/// the reversal condition on `refuse_non_strict_json_schema`. Check the vendored crate's
/// `providers/openai/completion/mod.rs` before touching the refusal.
#[tokio::test]
async fn rig_0_40_still_hardcodes_strict_true_and_promotes_optional_properties_to_required() {
    let Some(fixture) = compat_fixture().await else {
        return;
    };
    let provider = MockOpenAiServer::start([ProviderScript::Completion {
        text: "{\"answer\":\"yes\",\"note\":\"n\"}".to_string(),
    }])
    .await;
    let bound = fixture
        .add_provider_with_capabilities(
            provider.base_url(),
            10,
            RuntimePolicy::default(),
            structured_output_model(),
        )
        .await;
    bind_provider_to_the_default_route(&fixture, &bound).await;
    let consumer_key = fixture.enable_public_streaming().await;
    let moira = MoiraHttpServer::start(fixture.state.clone()).await;

    let response = reqwest::Client::new()
        .post(format!("{}/api/v1/responses", moira.base_url))
        .header("x-consumer-key", &consumer_key)
        .json(&json!({
            "input": [{ "role": "user", "content": [{ "type": "input_text", "text": "answer me" }] }],
            "route": fixture.route_key,
            // Accepted: this is the value Moira can actually deliver.
            "response_format": {
                "type": "json_schema",
                "name": "caller_name",
                "schema": schema_with_an_optional_property(),
                "strict": true
            }
        }))
        .send()
        .await
        .expect("send native strict request");
    let status = response.status();
    let body = response.text().await.expect("native response body");
    assert_eq!(
        status,
        StatusCode::OK,
        "strict=true must be honoured: {body}"
    );

    let requests = provider.requests().await;
    assert_eq!(
        requests.len(),
        1,
        "the provider must be called exactly once"
    );
    let sent = &requests[0].body;
    let json_schema = &sent["response_format"]["json_schema"];

    assert_eq!(
        json_schema["strict"], true,
        "rig-core 0.40 hardcodes strict: true. If this is now false, the encoder gained a \
         strictness input and F45's refusal must be revisited: {sent}"
    );
    assert_eq!(
        json_schema["schema"]["required"],
        json!(["answer", "note"]),
        "sanitize_schema must still promote every declared property to required — this is the \
         concrete harm a silently-upgraded strict: false inflicts on a caller's schema: {sent}"
    );
    assert_eq!(
        json_schema["schema"]["additionalProperties"], false,
        "{sent}"
    );
    // F45's other half, unchanged and deliberately so: the provider-side name comes from the
    // schema's `title`, never from `response_format.name`. Documented on `PublicResponseFormat`.
    assert_eq!(
        json_schema["name"], "caller_title",
        "the wire name still comes from the schema's title, not from the caller's `name`. If \
         this is now `caller_name`, something started rewriting the schema: {sent}"
    );

    moira.shutdown().await;
    provider.shutdown().await;
}

/// F45 on the native endpoint, with both controls on the same helper.
///
/// The control structure is F46's and it is load-bearing twice over. "422 and the provider was
/// never called" is satisfiable by a fixture that cannot reach the provider at all — a routing
/// failure is also a 422 — so a case that *does* reach the provider is what makes the zero
/// attributable. And because the refusal is on a value rather than a variant, refusing the whole
/// `json_schema` variant would be the cheapest way to make the first assertion green: the two
/// controls (`strict` omitted, `strict: true`) are what stop that.
#[tokio::test]
async fn native_json_schema_strict_false_is_refused_and_never_reaches_the_provider() {
    let Some((status, body, calls)) = native_response(
        false,
        json!({
            "type": "json_schema",
            "name": "answer",
            "schema": caller_schema(),
            "strict": false
        }),
    )
    .await
    else {
        return;
    };
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "strict=false must be refused, not silently answered in strict mode, got {status}: {body}"
    );
    let envelope: Value = serde_json::from_str(&body).expect("coded error envelope");
    assert_eq!(
        envelope["error"]["code"], "unsupported_request_option",
        "the refusal must be the strict one, not a routing or policy failure: {body}"
    );
    assert_eq!(
        envelope["error"]["message_key"], "moira.error.unsupported_request_option",
        "{body}"
    );
    assert_eq!(
        calls, 0,
        "a refused request must not reach the provider or be billed: {body}"
    );

    // Control 1 — `strict` omitted. The common case, and the one F35 was right to protect.
    let Some((status, body, calls)) = native_response(
        false,
        json!({ "type": "json_schema", "name": "answer", "schema": caller_schema() }),
    )
    .await
    else {
        return;
    };
    assert_eq!(
        status,
        StatusCode::OK,
        "an omitted strict must still be accepted, or the refusal above is a contract break \
         rather than a fix: {body}"
    );
    assert_eq!(
        calls, 1,
        "the omitted-strict control must reach the provider: {body}"
    );

    // Control 2 — an explicit `strict: true`, which is what rig actually sends.
    let Some((status, body, calls)) = native_response(
        false,
        json!({
            "type": "json_schema",
            "name": "answer",
            "schema": caller_schema(),
            "strict": true
        }),
    )
    .await
    else {
        return;
    };
    assert_eq!(
        status,
        StatusCode::OK,
        "strict=true must still be accepted: {body}"
    );
    assert_eq!(
        calls, 1,
        "the strict=true control must reach the provider: {body}"
    );
}

/// The streaming twin. A refusal raised after the SSE head is written becomes a `200
/// text/event-stream` carrying an error event — the "200 that contradicts the request" shape
/// this whole family exists to prevent — so only asserting the status on the streaming path
/// proves the refusal happens before the stream begins.
#[tokio::test]
async fn native_json_schema_strict_false_is_refused_before_the_stream_begins() {
    let Some((status, body, calls)) = native_response(
        true,
        json!({
            "type": "json_schema",
            "name": "answer",
            "schema": caller_schema(),
            "strict": false
        }),
    )
    .await
    else {
        return;
    };
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "the streaming path must refuse before the SSE head is written, got {status}: {body}"
    );
    let envelope: Value = serde_json::from_str(&body).expect("coded error envelope");
    assert_eq!(
        envelope["error"]["code"], "unsupported_request_option",
        "{body}"
    );
    assert_eq!(
        calls, 0,
        "a refused stream must not reach the provider: {body}"
    );
}

/// The two endpoints must refuse the same shape, which is the property F35 and F46 both
/// insisted on and the reason F35 declined to refuse `strict: false` on the compat path alone.
///
/// This is also the case that would have caught the old `strict.unwrap_or(false)` in
/// `compat_response_format`: the compat DTO always carried `Option<bool>`, and the translation
/// was where "the caller omitted it" became "the caller asked for non-strict".
#[tokio::test]
async fn compat_text_format_strict_false_is_refused_over_http() {
    let Some(fixture) = compat_fixture().await else {
        return;
    };
    // Conforming JSON rather than the `must-not-be-reached` marker the two refusal-only cases
    // above use. **Issue #80 is why**: this case has a control request that *does* reach the
    // provider, carrying a schema, so a prose reply would now fail it with `422
    // structured_output_invalid` — a real refusal, but not the one under test, and the control
    // exists precisely to prove the fixture can answer. The refused request is still proven not
    // to reach the provider by `call_count() == 0`, which is what carried that meaning all along.
    let provider = MockOpenAiServer::start([ProviderScript::Completion {
        text: "{\"answer\":\"reached-only-by-the-control\"}".to_string(),
    }])
    .await;
    let bound = fixture
        .add_provider_with_capabilities(
            provider.base_url(),
            10,
            RuntimePolicy::default(),
            structured_output_model(),
        )
        .await;
    bind_provider_to_the_default_route(&fixture, &bound).await;
    let consumer_key = fixture.enable_public_streaming().await;
    let moira = MoiraHttpServer::start(fixture.state.clone()).await;
    let client = reqwest::Client::new();

    let response = client
        .post(format!("{}/v1/responses", moira.base_url))
        .header("x-consumer-key", &consumer_key)
        .json(&json!({
            "input": "answer me",
            "text": { "format": {
                "type": "json_schema",
                "name": "answer",
                "schema": caller_schema(),
                "strict": false
            } }
        }))
        .send()
        .await
        .expect("send compat strict=false request");
    let status = response.status();
    let body = response.text().await.expect("compat response body");
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "the compat endpoint must refuse strict=false exactly as the native one does, got \
         {status}: {body}"
    );
    let envelope: Value = serde_json::from_str(&body).expect("coded error envelope");
    assert_eq!(
        envelope["error"]["code"], "unsupported_request_option",
        "{body}"
    );
    assert_eq!(
        provider.call_count().await,
        0,
        "a refused compat request must not reach the provider: {body}"
    );

    // Control on the same fixture: omitting `strict` must still work, or the assertion above is
    // proving the fixture is broken rather than that the refusal fired.
    let response = client
        .post(format!("{}/v1/responses", moira.base_url))
        .header("x-consumer-key", &consumer_key)
        .json(&json!({
            "input": "answer me",
            "text": { "format": {
                "type": "json_schema",
                "name": "answer",
                "schema": caller_schema()
            } }
        }))
        .send()
        .await
        .expect("send compat control request");
    let status = response.status();
    let body = response.text().await.expect("compat control body");
    assert_eq!(
        status,
        StatusCode::OK,
        "the omitted-strict control must succeed: {body}"
    );
    assert_eq!(provider.call_count().await, 1, "{body}");

    moira.shutdown().await;
    provider.shutdown().await;
}

// ---------------------------------------------------------------------------------------------
// Issue #157 item 2, and item C2 of issue #155 — the compat endpoint's published error contract,
// driven rather than read.
//
// #155's C2 records the shape of the gap precisely: `POST /v1/responses` publishes an error
// contract that *no* test drives, and the argument for every status in it is "compat shares
// `failure_http_status`". That is an argument from shared code, and this endpoint is exactly the
// place where such an argument is weakest: it owns `compat_response_format`, a translation layer
// the native path does not run, and it resolves its route by falling through to
// `get_default_route` rather than by honouring a caller's hint. #157 then proposed *adding* two
// more statuses to that unguarded contract, which would have compounded the problem — declaring
// a status an endpoint cannot return is the same defect as omitting one it can.
//
// So the two statuses #157 adds are asserted here on the socket before they are declared.
// ---------------------------------------------------------------------------------------------

/// Creates an active agent profile and points the seeded default route at it.
///
/// The default route rather than the fixture's own: `POST /v1/responses` carries no route field,
/// so `general` is the only route it can resolve to (see
/// [`bind_provider_to_the_default_route`]). Attaching the profile anywhere else would leave the
/// case below driving a route with no profile at all, which is the vacuous-pass shape #155 is
/// about — hence the read-back assertion.
async fn attach_agent_profile_to_the_default_route(
    fixture: &LifecycleFixture,
) -> AgentProfileRecord {
    let admin = RuntimeAdminService::new(&fixture.state).expect("runtime admin service");
    let profile = admin
        .create_agent_profile(
            &fixture.actor,
            &support::request_context(),
            AgentProfileCreateRequest {
                profile_key: format!("compat-409-{}", Uuid::now_v7().simple()),
                display_name: "compat 409 profile".to_string(),
                preamble: Some(
                    "This preamble exists only to make the profile non-trivial.".to_string(),
                ),
                temperature: None,
                max_tokens: None,
                tool_policy: json!({}),
                context_policy: json!({}),
                memory_policy: json!({}),
                metadata: json!({ "test_fixture": true }),
            },
        )
        .await
        .expect("create agent profile");

    let route_id = default_route_id(fixture).await;
    // Read the version back rather than assuming `1`: `bind_provider_to_the_default_route` and
    // the migrations both touch this row, and an assumed version fails as a `409` from the
    // optimistic-concurrency check — which would look exactly like the status under test.
    let version: i64 = sqlx::query_scalar("select version from route_definitions where id = $1")
        .bind(route_id)
        .fetch_one(&fixture.pool)
        .await
        .expect("read the default route version");
    admin
        .patch_route_definition(
            &fixture.actor,
            &support::request_context(),
            route_id,
            version,
            RouteDefinitionPatchRequest {
                agent_profile_id: Some(profile.id),
                ..RouteDefinitionPatchRequest::default()
            },
        )
        .await
        .expect("attach the agent profile to the default route");

    let attached: Option<Uuid> =
        sqlx::query_scalar("select agent_profile_id from route_definitions where id = $1")
            .bind(route_id)
            .fetch_one(&fixture.pool)
            .await
            .expect("read the default route's agent_profile_id");
    assert_eq!(
        attached,
        Some(profile.id),
        "the default route must actually carry the profile, or the case below drives a route \
         with no profile and asserts nothing"
    );
    profile
}

/// **#157 / #155 C2 — `409 agent_profile_disabled` is reachable on `POST /v1/responses`.**
///
/// The control comes first and is load-bearing twice over. `409` with `call_count() == 0` is
/// satisfiable by a fixture that could never reach the provider at all, and a compat fixture has
/// two extra ways to be broken that the native one does not: the default route may carry no
/// routing policy, and `compat_response_format` may refuse the request before routing is even
/// consulted. A `200` that reaches the provider on the *same* fixture, over the *same* endpoint,
/// with the *only* difference being `status = 'disabled'` on the profile, is what makes the
/// refusal attributable to the profile rather than to the fixture.
///
/// The answer to #155 C2's open question — does the compat DTO's translation layer change the
/// failure behaviour? — is **no**: `openai_compat_to_public` runs *before* dispatch and produces
/// a `PublicResponseRequest`, after which `create_response` is the native path exactly. The
/// status, the code and the message key below are the same three
/// `guards_the_public_plane_refuses_a_disabled_agent_profile_with_409_and_a_code`
/// (`tests/agent_profile_wire.rs`) asserts on `POST /api/v1/responses`, which is the symmetry
/// the new declaration rests on.
#[tokio::test]
async fn compat_refuses_a_disabled_agent_profile_with_409_exactly_as_the_native_path_does() {
    let Some(fixture) = compat_fixture().await else {
        return;
    };
    let provider = MockOpenAiServer::start([
        ProviderScript::Completion {
            text: "reached only by the control".to_string(),
        },
        ProviderScript::Completion {
            text: "this script must never be consumed".to_string(),
        },
    ])
    .await;
    let bound = fixture
        .add_provider(provider.base_url(), 10, RuntimePolicy::default())
        .await;
    bind_provider_to_the_default_route(&fixture, &bound).await;
    let profile = attach_agent_profile_to_the_default_route(&fixture).await;
    let consumer_key = fixture.enable_public_streaming().await;
    let moira = MoiraHttpServer::start(fixture.state.clone()).await;
    let client = reqwest::Client::new();

    let control = client
        .post(format!("{}/v1/responses", moira.base_url))
        .header("x-consumer-key", &consumer_key)
        .json(&json!({ "input": "answer me" }))
        .send()
        .await
        .expect("send the compat control request");
    let status = control.status();
    let body = control.text().await.expect("compat control body");
    assert_eq!(
        status,
        StatusCode::OK,
        "control: the compat endpoint must serve the default route while the profile is active, \
         or the refusal below is unattributable: {body}"
    );
    let before = provider.call_count().await;
    assert_eq!(
        before, 1,
        "control: the request reached the provider: {body}"
    );

    RuntimeAdminService::new(&fixture.state)
        .expect("runtime admin service")
        .set_agent_profile_enabled(
            &fixture.actor,
            &support::request_context(),
            profile.id,
            profile.version,
            false,
        )
        .await
        .expect("disable the agent profile");

    let refused = client
        .post(format!("{}/v1/responses", moira.base_url))
        .header("x-consumer-key", &consumer_key)
        .json(&json!({ "input": "answer me" }))
        .send()
        .await
        .expect("send the compat request against the disabled profile");
    let status = refused.status();
    let body = refused.text().await.expect("compat refusal body");
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "the compat endpoint must answer a disabled agent profile with the same 409 the native \
         path does — this is the status #157 declares on the operation, got {status}: {body}"
    );
    let envelope: Value = serde_json::from_str(&body).expect("coded error envelope");
    assert_eq!(
        envelope["error"]["code"], "agent_profile_disabled",
        "the 409 must be the profile one, not an idempotency or optimistic-concurrency conflict \
         — a 409 for the wrong reason would make the declaration true by accident: {body}"
    );
    assert_eq!(
        envelope["error"]["message_key"], "moira.error.agent_profile_disabled",
        "{body}"
    );
    assert_eq!(
        provider.call_count().await,
        before,
        "a refused compat request must not reach the provider or be billed: {body}"
    );

    moira.shutdown().await;
    provider.shutdown().await;
}

/// **#157 — `503` is reachable on `POST /v1/responses`.**
///
/// `503` was declared on both native response operations and absent here for no stated reason.
/// Rather than adding it on the symmetry argument alone, this drives it: `public_actor` asks for
/// `state.pool()` before it does anything else, and `AppError::DatabaseUnavailable` is a `503`,
/// so an `AppState` built with no pool answers `503` on this operation for a reason that has
/// nothing to do with the caller's request.
///
/// No database, deliberately — this is the one case in this file that runs in every environment,
/// and #155 E1 is about exactly the guards that green-SKIP without Postgres.
///
/// The `404` control is the half that stops this passing for the wrong reason. `503` is only
/// reachable *after* the feature-flag check, so a run with the flag left off would answer `404`
/// and never reach `public_actor` at all; asserting both over equally poolless states proves the
/// flag was on and that the `503` came from the pool.
#[tokio::test]
async fn compat_answers_503_when_the_database_is_unavailable() {
    let mut settings = Settings::default();
    settings.public_api.openai_responses_compat_enabled = true;
    let state = AppState::new(settings, None)
        .await
        .expect("poolless app state");
    let moira = MoiraHttpServer::start(state).await;
    let client = reqwest::Client::new();

    let response = client
        .post(format!("{}/v1/responses", moira.base_url))
        .json(&json!({ "input": "answer me" }))
        .send()
        .await
        .expect("send the compat request against a poolless state");
    let status = response.status();
    let body = response.text().await.expect("compat 503 body");
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "the compat endpoint must answer 503 when the database it needs to authenticate the \
         caller is absent — this is the status #157 declares on the operation, got {status}: \
         {body}"
    );

    // The control: with the flag off the handler refuses before it ever asks for the pool, so
    // the 503 above cannot have come from a disabled endpoint.
    let mut disabled = Settings::default();
    disabled.public_api.openai_responses_compat_enabled = false;
    let disabled_state = AppState::new(disabled, None)
        .await
        .expect("poolless app state with compat off");
    let disabled_moira = MoiraHttpServer::start(disabled_state).await;
    let response = client
        .post(format!("{}/v1/responses", disabled_moira.base_url))
        .json(&json!({ "input": "answer me" }))
        .send()
        .await
        .expect("send the compat request against a disabled endpoint");
    let status = response.status();
    let body = response.text().await.expect("compat 404 body");
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "control: a disabled compatibility endpoint must still answer 404, which is the other \
         meaning #157 adds to that status's description, got {status}: {body}"
    );

    disabled_moira.shutdown().await;
    moira.shutdown().await;
}
