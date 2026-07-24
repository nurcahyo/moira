use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use moira::{app::AppState, build_router, config::Settings};
use serde_json::Value;
use tower::ServiceExt;

#[tokio::test]
async fn error_response_body_includes_i18n_fields_and_request_id() {
    let state = AppState::new(Settings::default(), None).expect("app state");
    let app = build_router(state);
    let request_id = "req_test_123";

    let request = Request::builder()
        .uri("/health/ready")
        .header(header::HeaderName::from_static("x-request-id"), request_id)
        .body(Body::empty())
        .expect("request");

    let response = app.oneshot(request).await.expect("response");
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        response
            .headers()
            .get(header::HeaderName::from_static("x-request-id"))
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
