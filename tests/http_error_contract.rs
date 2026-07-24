use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use moira::{app::AppState, build_router, config::Settings};
use serde_json::Value;
use tower::ServiceExt;

#[tokio::test]
async fn error_response_body_includes_i18n_fields_and_request_id() {
    let state = AppState::new(Settings::default(), None).expect("app state");
    let app = build_router(state).expect("router");

    let request = Request::builder()
        .uri("/health/ready")
        .body(Body::empty())
        .expect("request");

    let response = app.oneshot(request).await.expect("response");
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

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
    assert!(value["error"]["request_id"].is_string());
    assert!(
        !value["error"]["request_id"]
            .as_str()
            .expect("request id string")
            .is_empty()
    );
    assert!(value["error"]["details"].is_null());
}
