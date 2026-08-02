//! F40 — what `GET /api/v1/responses/{id}` says about output it does not have.
//!
//! # The finding's premise did not hold, and the code around it still lied
//!
//! F40 reported an empty `output` array for a *completed, persisted* response. That state is
//! unreachable: every `ResponseTerminalUpdate` in `src/application/public.rs` hardcodes
//! `output_persisted: false`, the column defaults to `false`, and nothing in the tree writes
//! `true` — `docs/response-persistence.md` says as much. So a completed response always took
//! the `output_unavailable` branch, and the empty array was reached only by responses that are
//! queued, in progress, failed or cancelled, where it is the honest answer.
//!
//! What *was* wrong is the reason it gave. `"metadata_only_persistence"` was a literal,
//! emitted whatever the application had configured — true for the default mode and false for
//! the other three, which is worse than no explanation because it names a cause the operator
//! did not choose.
//!
//! # Why this suite exists when `src/application/public.rs` already has the matrix
//!
//! The unit tests there call `public_response_from_record` directly and cover every mode. They
//! cannot tell whether the field the fix reads is populated on the path a caller uses. That is
//! F49's lesson — "it asserts on the real wire" is not "it reaches the code you changed" —
//! and it applies exactly here: `output_unavailable_reason` reads
//! `output_summary.persistence_mode`, and if `find_response_authorized` did not select
//! `output_summary`, or if `terminal_update_from_outcome` did not write the mode into it,
//! every unit case would still pass while the live endpoint answered
//! `"metadata_only_persistence"` forever.
//!
//! So nothing here hand-builds a row. The policy is set through the real service, the response
//! is produced by a real `POST` against a mock provider, and the assertion reads the real
//! `GET` body.
//!
//! # The variable the fixture would otherwise collapse
//!
//! A response completed under mode X, read back while the application is still on mode X,
//! cannot distinguish "the reason comes from the row" from "the reason comes from the
//! application's current policy". Those differ the moment an operator changes the setting, and
//! only one of them is right: the row is the historical fact.
//! [`the_reason_is_the_mode_the_response_completed_under`] separates them by moving the policy
//! after the response is finished.

mod support;

use axum::http::StatusCode;
use moira::{
    application::PublicExecutionService,
    domain::{ApplicationExecutionPolicyPutRequest, ResponsePersistenceMode},
};
use serde_json::Value;
use support::{
    LifecycleFixture, MoiraHttpServer, RuntimePolicy, mock_openai::MockOpenAiServer,
    mock_openai::ProviderScript, public_response_request, request_context,
};

/// What the provider is scripted to say, so "the model produced text" is a fact the
/// assertions can rest on rather than an assumption.
const REPLY: &str = "the model did produce an answer";

async fn set_persistence_mode(fixture: &LifecycleFixture, mode: ResponsePersistenceMode) {
    PublicExecutionService::new(&fixture.state)
        .expect("public service")
        .put_application_execution_policy(
            &fixture.actor,
            &request_context(),
            fixture.application_id,
            None,
            ApplicationExecutionPolicyPutRequest {
                persistence_mode: Some(mode),
                ..ApplicationExecutionPolicyPutRequest::default()
            },
        )
        .await
        .expect("set the persistence mode");
}

/// The single content part of a completed response's `output`.
///
/// Panics rather than returning an option: an empty `output` on a completed response is
/// precisely the defect F40 named, so it must fail here loudly and not be filtered away.
fn sole_content_part(body: &Value) -> &Value {
    let output = body["output"]
        .as_array()
        .unwrap_or_else(|| panic!("output must be an array, got {}", body["output"]));
    assert_eq!(
        output.len(),
        1,
        "a completed response must carry exactly one output item, got {output:?}"
    );
    let content = output[0]["content"]
        .as_array()
        .expect("content must be an array");
    assert_eq!(
        content.len(),
        1,
        "expected exactly one content part, got {content:?}"
    );
    &content[0]
}

/// Drives one real completion and returns `(response id, consumer key, http server)`.
async fn complete_one_response(
    fixture: &LifecycleFixture,
    moira: &MoiraHttpServer,
    consumer_key: &str,
) -> String {
    let response = reqwest::Client::new()
        .post(format!("{}/api/v1/responses", moira.base_url))
        .header("x-consumer-key", consumer_key)
        .json(&public_response_request(&fixture.route_key))
        .send()
        .await
        .expect("send the public response request");
    assert_eq!(response.status(), StatusCode::OK, "the POST must succeed");
    let body: Value = response.json().await.expect("public response body");

    // The POST answers with the text itself. Asserted here because it is the other half of
    // the property: the model really did produce output, so a later `output_unavailable` is a
    // statement about persistence and never about an empty reply.
    assert_eq!(
        sole_content_part(&body)["type"],
        "output_text",
        "the creating request must return the text, got {body}"
    );
    assert_eq!(sole_content_part(&body)["text"], REPLY);

    // Returned prefixed, which is what `GET /api/v1/responses/{response_id}` parses.
    body["id"].as_str().expect("response id").to_string()
}

async fn get_response(moira: &MoiraHttpServer, consumer_key: &str, id: &str) -> Value {
    let response = reqwest::Client::new()
        .get(format!("{}/api/v1/responses/{id}", moira.base_url))
        .header("x-consumer-key", consumer_key)
        .send()
        .await
        .expect("send the response read");
    assert_eq!(response.status(), StatusCode::OK, "the GET must succeed");
    response.json().await.expect("response read body")
}

/// The live endpoint reports the mode the response was completed under, for a mode that is
/// not the default.
///
/// `plain_content` is chosen deliberately: it is the mode whose old answer was most wrong —
/// the operator asked for the body to be stored, nothing stores it, and the API used to blame
/// metadata-only persistence.
#[tokio::test]
async fn a_completed_response_names_the_persistence_mode_it_ran_under() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let provider = MockOpenAiServer::start([
        ProviderScript::Completion {
            text: REPLY.to_string(),
        },
        ProviderScript::Completion {
            text: REPLY.to_string(),
        },
    ])
    .await;
    fixture
        .add_provider(provider.base_url(), 10, RuntimePolicy::default())
        .await;
    let consumer_key = fixture.enable_public_streaming().await;
    set_persistence_mode(&fixture, ResponsePersistenceMode::PlainContent).await;
    let moira = MoiraHttpServer::start(fixture.state.clone()).await;

    let id = complete_one_response(&fixture, &moira, &consumer_key).await;
    let body = get_response(&moira, &consumer_key, &id).await;

    assert_eq!(
        body["status"], "completed",
        "the read-back response must be completed for this case to mean anything"
    );
    let part = sole_content_part(&body);
    assert_eq!(part["type"], "output_unavailable");
    assert_eq!(
        part["reason"], "content_persistence_not_implemented",
        "the endpoint reported the wrong cause for plain_content, got {body}"
    );

    moira.shutdown().await;
    provider.shutdown().await;
}

/// The reason is a property of the response, not of the application's settings right now.
///
/// The policy is moved to `metadata_only` *after* the response has completed. A reason read
/// from the live policy would follow it; a reason read from the row does not.
#[tokio::test]
async fn the_reason_is_the_mode_the_response_completed_under() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let provider = MockOpenAiServer::start([ProviderScript::Completion {
        text: REPLY.to_string(),
    }])
    .await;
    fixture
        .add_provider(provider.base_url(), 10, RuntimePolicy::default())
        .await;
    let consumer_key = fixture.enable_public_streaming().await;
    set_persistence_mode(&fixture, ResponsePersistenceMode::PlainContent).await;
    let moira = MoiraHttpServer::start(fixture.state.clone()).await;

    let id = complete_one_response(&fixture, &moira, &consumer_key).await;

    set_persistence_mode(&fixture, ResponsePersistenceMode::MetadataOnly).await;

    let part = sole_content_part(&get_response(&moira, &consumer_key, &id).await).clone();
    assert_eq!(
        part["reason"], "content_persistence_not_implemented",
        "the reason followed the application's current policy instead of the mode the \
         response actually completed under"
    );

    moira.shutdown().await;
    provider.shutdown().await;
}

/// The latent inversion, forced into reach.
///
/// `output_persisted` is written `false` by every code path, so this state cannot be produced
/// through the API — the column is flipped directly. The old branch sent exactly this row to
/// an empty array, which is the symptom F40 reported and the one that will arrive for real the
/// day content persistence is implemented.
///
/// This also proves the GET query carries `output_persisted` through to the mapper, which the
/// unit cases assume and cannot check.
#[tokio::test]
async fn a_row_claiming_persisted_output_is_not_served_as_an_empty_array() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let provider = MockOpenAiServer::start([ProviderScript::Completion {
        text: REPLY.to_string(),
    }])
    .await;
    fixture
        .add_provider(provider.base_url(), 10, RuntimePolicy::default())
        .await;
    let consumer_key = fixture.enable_public_streaming().await;
    let moira = MoiraHttpServer::start(fixture.state.clone()).await;

    let id = complete_one_response(&fixture, &moira, &consumer_key).await;
    let affected = sqlx::query("update responses set output_persisted = true where id = $1::uuid")
        .bind(id.trim_start_matches("resp_"))
        .execute(&fixture.pool)
        .await
        .expect("flip output_persisted")
        .rows_affected();
    assert_eq!(affected, 1, "the response row was not found to flip");

    let body = get_response(&moira, &consumer_key, &id).await;
    assert_eq!(
        body["output_persisted"], true,
        "the fixture did not reach the state under test"
    );
    let part = sole_content_part(&body);
    assert_eq!(part["type"], "output_unavailable");
    assert_eq!(part["reason"], "persisted_output_not_loaded");

    moira.shutdown().await;
    provider.shutdown().await;
}
