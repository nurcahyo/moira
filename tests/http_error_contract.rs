//! The shape of Moira's public error envelope, asserted over real HTTP.
//!
//! Every error this API returns is the same object — `code`, `message_key`, `message`,
//! `message_args`, `request_id`, `details` — and the value of that promise is entirely in it
//! being kept by *every* error, not by the one that happened to get a test. The two tests
//! here take one error from each end of the stack: a middleware-level failure that never
//! reaches a handler, and a request-validation failure raised deep inside the application
//! layer.

mod support;

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use moira::{
    app::AppState,
    application::RuntimeAdminService,
    build_router,
    config::Settings,
    domain::{RouteDefinitionCreateRequest, RouteSelectionStrategy},
};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

use support::{LifecycleFixture, request_context};

async fn get_json(router: &Router, uri: &str, request_id: &str) -> (StatusCode, Value) {
    let request = Request::builder()
        .method("GET")
        .uri(uri)
        .header("x-request-id", request_id)
        .body(Body::empty())
        .expect("request");

    let response = router
        .clone()
        .oneshot(request)
        .await
        .expect("HTTP response");
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body");
    let body: Value = serde_json::from_slice(&bytes).expect("json body");
    (status, body)
}

#[tokio::test]
async fn error_response_body_includes_i18n_fields_and_request_id() {
    let state = AppState::new(Settings::default(), None).expect("app state");
    let app = build_router(state).expect("router");

    let request_id = "contract-request-id";
    let request = Request::builder()
        .uri("/health/ready")
        .header("x-request-id", request_id)
        .body(Body::empty())
        .expect("request");

    let response = app.oneshot(request).await.expect("response");
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        response
            .headers()
            .get("x-request-id")
            .and_then(|value| value.to_str().ok()),
        Some(request_id)
    );

    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body");
    let value: Value = serde_json::from_slice(&body).expect("json body");

    assert_eq!(value["error"]["code"], "database_unavailable");
    assert_eq!(
        value["error"]["message_key"],
        "moira.error.database_unavailable"
    );
    assert_eq!(value["error"]["message"], "database is not configured");
    assert_eq!(value["error"]["message_args"], serde_json::json!({}));
    assert_eq!(value["error"]["request_id"], request_id);
    assert!(value["error"]["details"].is_null());
}

/// `400 invalid_cursor` is the error every list endpoint owes a client that replays a stale,
/// truncated, tampered, or foreign pagination cursor (plan 04, P1-4).
///
/// It is worth a contract test of its own because it is the one error a *correct* client can
/// provoke by accident — a cursor stored across a deploy, a cursor copied between two lists —
/// and therefore the one whose `message_key` a console will actually render to a human. A key
/// that is spelled right but missing from the catalog translates to nothing, so the assertion
/// below runs the key through [`moira::i18n::is_known_key`] rather than comparing it to a
/// string literal repeated from the source; a literal would agree with a typo that shipped.
///
/// `request_id` is asserted for the same reason it exists: this is a `400` raised inside the
/// application layer, after the request has passed authentication and been routed, so it is
/// the case where the correlation id has the furthest to travel and the most to be dropped by.
#[tokio::test]
async fn invalid_cursor_error_response_carries_message_key_and_message() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let router = moira::build_router(fixture.state.clone()).expect("build Moira test router");

    // Two ways to be invalid that reach the same coded error by different routes: input that
    // is not even base64, and a well-formed cursor minted for a different list. Both are
    // asserted here so a change that special-cases one of them cannot pass.
    //
    // A second route is created so `?limit=1` is guaranteed to over-fetch and mint a cursor.
    // Relying on rows an earlier suite left in the shared database would make this case
    // silently skip itself on a fresh one.
    let extra_route = RuntimeAdminService::new(&fixture.state)
        .expect("runtime admin service")
        .create_route_definition(
            &fixture.actor,
            &request_context(),
            RouteDefinitionCreateRequest {
                route_key: format!("errctr_{}", Uuid::now_v7().simple()),
                display_name: "Error contract route".to_string(),
                description: None,
                selection_strategy: RouteSelectionStrategy::Default,
                agent_profile_id: None,
                metadata: json!({ "suite": "http_error_contract" }),
            },
        )
        .await
        .expect("create second route definition");

    let route_cursor = {
        let (status, body) = get_json(&router, "/api/v1/admin/routes?limit=1", "seed").await;
        assert_eq!(status, StatusCode::OK, "seed route page failed: {body}");
        body["pagination"]["next_cursor"]
            .as_str()
            .map(str::to_string)
            .expect("two routes at limit=1 must mint a next_cursor")
    };

    let cases: Vec<(&str, String)> = vec![
        (
            "garbage",
            "/api/v1/admin/applications?limit=1&cursor=not-a-real-cursor".to_string(),
        ),
        (
            "cursor minted for a different list",
            format!("/api/v1/admin/applications?limit=1&cursor={route_cursor}"),
        ),
    ];

    for (what, uri) in cases {
        let request_id = format!("invalid-cursor-{}", Uuid::now_v7());
        let (status, body) = get_json(&router, &uri, &request_id).await;

        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "{what}: expected 400, got {status} with body {body}"
        );

        let error = body["error"]
            .as_object()
            .unwrap_or_else(|| panic!("{what}: expected an error envelope, got {body}"));

        assert_eq!(error["code"], "invalid_cursor", "{what}");

        let message_key = error["message_key"]
            .as_str()
            .unwrap_or_else(|| panic!("{what}: message_key is not a string: {body}"));
        assert!(
            moira::i18n::is_known_key(message_key),
            "{what}: {message_key} is not in the response catalog, so it renders as nothing"
        );

        let message = error["message"]
            .as_str()
            .unwrap_or_else(|| panic!("{what}: message is not a string: {body}"));
        assert!(
            !message.trim().is_empty(),
            "{what}: the invalid-cursor error must carry a non-empty default English message"
        );
        // The rejected cursor must not be echoed back: it is reflected input and tells the
        // caller nothing they can act on.
        assert!(
            !message.contains("not-a-real-cursor"),
            "{what}: the error message reflects the offending cursor back: {message}"
        );

        assert_eq!(
            error["request_id"], request_id,
            "{what}: the correlation id must survive an application-layer 400"
        );
        assert!(
            error["message_args"].is_object(),
            "{what}: message_args must always be an object: {body}"
        );
    }

    sqlx::query("delete from route_definitions where id = $1")
        .bind(extra_route.id)
        .execute(&fixture.pool)
        .await
        .expect("clean up the seeded route definition");
}

/// Plan 11 wave 2 added two catalog keys. Both must resolve, and neither may be a duplicate of
/// a key that already covered the condition.
///
/// `is_known_key`/`default_message_for_key` are the resolution path `src/error.rs`'s
/// `format!("moira.error.{}", code())` derivation ends at, so a key that is spelled right in
/// the code and missing from the catalog produces an empty message rather than a failure —
/// which is exactly why this assertion is worth making explicitly.
#[test]
fn the_new_retrieval_error_keys_resolve_through_the_catalog() {
    for key in [
        "moira.error.context_length_exceeded",
        "moira.error.retrieval_unavailable",
    ] {
        assert!(
            moira::i18n::is_known_key(key),
            "{key} is missing from the Rust catalog"
        );
        let message = moira::i18n::default_message_for_key(key)
            .unwrap_or_else(|| panic!("{key} resolves to no default message"));
        assert!(!message.is_empty(), "{key} has an empty default message");
    }
}

/// The naming trap §0 flagged: `context_required_content_too_large` already existed and is a
/// *different* condition — one oversized required item, not an over-budget assembled context.
/// Both must remain, and they must not share a message.
#[test]
fn the_two_context_size_keys_stay_distinct() {
    let assembled = moira::i18n::default_message_for_key("moira.error.context_length_exceeded")
        .expect("assembled-budget key");
    let single_item =
        moira::i18n::default_message_for_key("moira.error.context_required_content_too_large")
            .expect("pre-existing single-item key");
    assert_ne!(
        assembled, single_item,
        "two keys with the same message means one of them should not exist"
    );
}

/// Keys plan 11 wave 2 deliberately reuses rather than duplicating.
#[test]
fn the_reused_retrieval_adjacent_keys_still_resolve() {
    for key in [
        "moira.error.conversation_archived",
        "moira.error.conversation_not_found",
        "moira.error.embedding_provider_unsupported",
        "moira.error.embedding_request_failed",
        "moira.error.embedding_response_invalid",
        "moira.error.forbidden",
        "moira.error.rag_document_too_large",
    ] {
        assert!(
            moira::i18n::is_known_key(key),
            "{key} was expected to already exist and be reused"
        );
    }
}
