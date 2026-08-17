//! #347 — the input-message bound is per application, nested inside the deployment ceiling.
//!
//! # Why this is not covered by the unit tests next to `validate_policy_request`
//!
//! Those prove the *write* is refused when it would have no effect. They say nothing about
//! whether the stored value is ever consulted, and "a setting that exists, is settable, and is
//! not read" is the defect #347 was filed about — so a suite that only tested the setter would
//! reproduce the bug's own shape. Everything here goes through `POST /api/v1/responses`.
//!
//! # Two bounds, and the test needs both directions
//!
//! `validate_public_request` takes `min(policy.maximum_input_messages, deployment ceiling)`.
//! Testing only the tightened case would pass against an implementation that ignored the
//! deployment value entirely; testing only the ceiling would pass against one that ignored the
//! policy. Both are asserted, and the second one matters most on the day an operator lowers the
//! deployment ceiling under applications that were provisioned when it was higher.

mod support;

use axum::http::StatusCode;
use moira::{
    application::PublicExecutionService,
    domain::{
        ApplicationExecutionPolicyPutRequest, PublicContentPart, PublicInputMessage,
        PublicMessageRole, PublicResponseRequest,
    },
};
use serde_json::Value;
use support::{
    LifecycleFixture, MoiraHttpServer, RuntimePolicy, mock_openai::MockOpenAiServer,
    mock_openai::ProviderScript, public_response_request, request_context,
};

const REPLY: &str = "within the limit";

async fn set_message_limit(fixture: &LifecycleFixture, maximum_input_messages: i32) {
    PublicExecutionService::new(&fixture.state)
        .expect("public service")
        .put_application_execution_policy(
            &fixture.actor,
            &request_context(),
            fixture.application_id,
            None,
            ApplicationExecutionPolicyPutRequest {
                maximum_input_messages: Some(maximum_input_messages),
                ..ApplicationExecutionPolicyPutRequest::default()
            },
        )
        .await
        .expect("set the per-application message limit");
}

/// `n` user messages of one content part each.
///
/// One part per message on purpose: `maximum_input_items` bounds the total content parts and
/// would otherwise be the thing refusing these requests, which would make every assertion below
/// a statement about the wrong limit. #347 was filed having conflated exactly these two.
fn request_with_messages(route: &str, n: usize) -> PublicResponseRequest {
    PublicResponseRequest {
        input: (0..n)
            .map(|index| PublicInputMessage {
                role: PublicMessageRole::User,
                content: vec![PublicContentPart::InputText {
                    text: format!("message {index}"),
                }],
            })
            .collect(),
        ..public_response_request(route)
    }
}

async fn post(
    moira: &MoiraHttpServer,
    key: &str,
    request: &PublicResponseRequest,
) -> (StatusCode, Value) {
    let response = reqwest::Client::new()
        .post(format!("{}/api/v1/responses", moira.base_url))
        .header("x-consumer-key", key)
        .json(request)
        .send()
        .await
        .expect("send the public response request");
    let status = response.status();
    let body = response.json().await.unwrap_or(Value::Null);
    (status, body)
}

/// The tightened direction: an application below the deployment default refuses at *its* bound.
///
/// The deployment ceiling here is the shipped 128, so before #347 a three-message request was
/// accepted by every application on the deployment and there was no way to say otherwise for one
/// of them.
#[tokio::test]
async fn an_application_limit_below_the_deployment_ceiling_is_what_refuses() {
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
    set_message_limit(&fixture, 2).await;
    let moira = MoiraHttpServer::start(fixture.state.clone()).await;

    // The control, and it runs first. Without it a refusal below proves only that the fixture
    // is broken — every one of these requests would fail for a dozen unrelated reasons.
    let (status, body) = post(
        &moira,
        &consumer_key,
        &request_with_messages(&fixture.route_key, 2),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "two messages are at the application's limit and must be accepted, got {body}"
    );

    let (status, body) = post(
        &moira,
        &consumer_key,
        &request_with_messages(&fixture.route_key, 3),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "three messages exceed this application's limit of two and must be refused, got {body}"
    );
    assert_eq!(
        body["error"]["code"], "input_too_large",
        "the refusal must be the input bound and not something incidental, got {body}"
    );
}

/// The ceiling direction: a stored value above the deployment ceiling does not win.
///
/// # How this state is reached, and why it is not contrived
///
/// `validate_policy_request` refuses a write above the ceiling, so the service cannot be used to
/// create this row — which is the point of that refusal. The state still arises in production by
/// the other route: the row is written while the ceiling is high, and the ceiling is **lowered
/// underneath it**. Nothing rewrites policy rows when configuration changes.
///
/// The fixture's settings are fixed at construction (`with_settings`), so the row is written with
/// SQL rather than by time-travelling the configuration. The `update` below is standing in for
/// "this row predates the current ceiling", not for a database edit anybody would perform.
#[tokio::test]
async fn lowering_the_deployment_ceiling_binds_applications_already_above_it() {
    let Some(fixture) = LifecycleFixture::with_settings(|settings| {
        settings.public_api.maximum_messages = 1;
    })
    .await
    else {
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

    // The row as it would have been left by an operator writing 64 when the ceiling was 128.
    sqlx::query(
        "insert into application_execution_policies (application_id, maximum_input_messages) \
         values ($1, 64) \
         on conflict (application_id) do update set maximum_input_messages = 64",
    )
    .bind(fixture.application_id)
    .execute(&fixture.pool)
    .await
    .expect("write a policy row that predates the lowered ceiling");

    // The service refuses to create this state, which is the other half of the guarantee.
    let refused = PublicExecutionService::new(&fixture.state)
        .expect("public service")
        .put_application_execution_policy(
            &fixture.actor,
            &request_context(),
            fixture.application_id,
            None,
            ApplicationExecutionPolicyPutRequest {
                maximum_input_messages: Some(64),
                ..ApplicationExecutionPolicyPutRequest::default()
            },
        )
        .await;
    assert!(
        refused.is_err(),
        "writing 64 against a ceiling of 1 must be refused, or the ceiling is advisory"
    );

    let moira = MoiraHttpServer::start(fixture.state.clone()).await;

    let (status, body) = post(
        &moira,
        &consumer_key,
        &request_with_messages(&fixture.route_key, 1),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "one message is within the lowered ceiling and must still work, got {body}"
    );

    let (status, body) = post(
        &moira,
        &consumer_key,
        &request_with_messages(&fixture.route_key, 2),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "the application's stored 64 must not survive the deployment ceiling dropping to 1, \
         got {body}"
    );
    assert_eq!(body["error"]["code"], "input_too_large");
}
