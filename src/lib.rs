#![recursion_limit = "256"]

pub mod app;
pub mod application;
pub mod config;
pub mod domain;
pub mod error;
pub mod http;
pub mod infra;
pub mod orchestration;
pub mod security;

use axum::{
    Router,
    body::Body,
    extract::{DefaultBodyLimit, State},
    http::{HeaderValue, Request, header},
    middleware::{self, Next},
    response::Response,
};
use std::time::Instant;
use tower_http::{
    cors::{Any, CorsLayer},
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    trace::TraceLayer,
};

use crate::app::AppState;

pub fn build_router(state: AppState) -> Router {
    let metrics_state = state.clone();
    http::router()
        .layer(middleware::from_fn_with_state(
            metrics_state,
            metrics_middleware,
        ))
        .layer(middleware::from_fn(secure_response_headers))
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
