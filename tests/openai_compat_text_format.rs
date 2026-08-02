//! F35 — the OpenAI-compat endpoint's `text.format`, over real HTTP and out to a real
//! provider socket.
//!
//! The unit tests in `src/application/public.rs` prove the *translation*. These prove the
//! *wiring*: that the translated format survives policy validation, routing, the Rig
//! boundary and `rig-core`'s own encoder, and lands on the provider request as
//! `response_format`. HANDOFF §3.4 is explicit that a predicate test and a wiring test are
//! different tests, and that every laundering finding in this repository has had a correct
//! predicate.
//!
//! The last test in this file guards nothing — it *documents* the behaviour that decided
//! F35's shape, and says so in its name.

mod support;

use moira::{application::RuntimeAdminService, domain::RoutingPolicyCreateRequest};
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
    let route_id: Uuid = sqlx::query_scalar(
        "select id from route_definitions where route_key = 'general' and deleted_at is null",
    )
    .fetch_one(&fixture.pool)
    .await
    .expect("migration 0005 seeds the 'general' route");
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

/// DOCUMENTS CURRENT BEHAVIOUR — this is not a guard.
///
/// It pins the reason F35 refuses `text.format.json_object` instead of translating it. On the
/// *native* endpoint, `response_format: {"type":"json_object"}` becomes the output schema
/// `{"type":"object"}`, which `rig-core`'s OpenAI encoder completes to
/// `{"type":"object","properties":{},"additionalProperties":false,"required":[]}` and sends
/// under `strict: true`. That schema is satisfied by exactly one document — `{}` — so a caller
/// asking for free-form JSON is constrained to the empty object.
///
/// Recorded in the ledger as its own finding. If someone fixes the native path, this test goes
/// red and the compat refusal above should be revisited in the same change; that coupling is
/// the point of asserting it here rather than describing it in prose.
#[tokio::test]
async fn documents_native_json_object_reaching_the_provider_as_an_empty_object_schema() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let provider = MockOpenAiServer::start([ProviderScript::Completion {
        text: "{}".to_string(),
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
            "response_format": { "type": "json_object" }
        }))
        .send()
        .await
        .expect("send native json_object request");
    let status = response.status();
    let body = response.text().await.expect("native json_object body");
    assert_eq!(status, StatusCode::OK, "native json_object failed: {body}");

    let requests = provider.requests().await;
    assert_eq!(requests.len(), 1);
    let json_schema = &requests[0].body["response_format"]["json_schema"];
    assert_eq!(
        json_schema["strict"], true,
        "rig hardcodes strict: {}",
        requests[0].body
    );
    assert_eq!(
        json_schema["schema"],
        json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false,
            "required": []
        }),
        "json_object reached the provider as something other than the empty-object schema: {}",
        requests[0].body
    );

    moira.shutdown().await;
    provider.shutdown().await;
}
