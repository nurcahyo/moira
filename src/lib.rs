#![recursion_limit = "256"]

pub mod app;
pub mod application;
pub mod config;
pub mod domain;
pub mod error;
pub mod http;
pub mod i18n;
pub mod infra;
pub mod orchestration;
pub mod security;

use axum::{
    Router,
    body::{Body, to_bytes},
    extract::{DefaultBodyLimit, State},
    http::{HeaderValue, Request, header},
    middleware::{self, Next},
    response::Response,
};
use serde_json::Value;
use std::time::Instant;
use tower_http::{
    cors::{Any, CorsLayer},
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, RequestId, SetRequestIdLayer},
    trace::TraceLayer,
};

use crate::app::AppState;

pub fn build_router(state: AppState) -> Router {
    let metrics_state = state.clone();
    http::router()
        .layer(middleware::from_fn(enrich_error_request_id))
        .layer(middleware::from_fn_with_state(
            metrics_state,
            metrics_middleware,
        ))
        .layer(middleware::from_fn(secure_response_headers))
        .layer(middleware::from_fn(capture_request_id))
        .layer(DefaultBodyLimit::max(512 * 1024))
        .layer(TraceLayer::new_for_http())
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods(Any)
                .allow_headers(Any),
        )
        .with_state(state)
}

async fn enrich_error_request_id(req: Request<Body>, next: Next) -> Response {
    let response = next.run(req).await;
    let Some(request_id) = response
        .headers()
        .get(header::HeaderName::from_static("x-request-id"))
        .and_then(|value| value.to_str().ok())
        .map(ToOwned::to_owned)
    else {
        return response;
    };

    if !response.status().is_client_error() && !response.status().is_server_error() {
        return response;
    }

    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if !content_type.starts_with("application/json") {
        return response;
    }

    let (mut parts, body) = response.into_parts();
    let bytes = to_bytes(body, usize::MAX)
        .await
        .expect("error response bodies should be fully buffered");
    let Ok(mut value) = serde_json::from_slice::<Value>(&bytes) else {
        return Response::from_parts(parts, Body::from(bytes));
    };

    let Some(error) = value.get_mut("error").and_then(Value::as_object_mut) else {
        return Response::from_parts(parts, Body::from(bytes));
    };

    let request_id_value = error.entry("request_id".to_string()).or_insert(Value::Null);
    if matches!(request_id_value, Value::String(current) if !current.is_empty()) {
        return Response::from_parts(parts, Body::from(bytes));
    }

    *request_id_value = Value::String(request_id);
    parts.headers.remove(header::CONTENT_LENGTH);
    let body = serde_json::to_vec(&value).expect("serialize enriched error response");
    Response::from_parts(parts, Body::from(body))
}

async fn metrics_middleware(
    State(state): State<AppState>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let started = Instant::now();
    let response = next.run(req).await;
    state
        .metrics
        .record_http_response(response.status(), started.elapsed());
    response
}

async fn secure_response_headers(req: Request<Body>, next: Next) -> Response {
    let mut response = next.run(req).await;
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    response
}

async fn capture_request_id(req: Request<Body>, next: Next) -> Response {
    let request_id = req
        .extensions()
        .get::<RequestId>()
        .and_then(|request_id| request_id.header_value().to_str().ok())
        .map(ToOwned::to_owned)
        .or_else(|| {
            req.headers()
                .get(header::HeaderName::from_static("x-request-id"))
                .and_then(|value| value.to_str().ok())
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| uuid::Uuid::now_v7().to_string());

    crate::error::REQUEST_ID
        .scope(Some(request_id), next.run(req))
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{app::AppState, config::Settings};

    #[test]
    fn router_builds_with_phase_one_routes() {
        let state = AppState::new(Settings::default(), None).unwrap();
        let _router = build_router(state);
    }
}
