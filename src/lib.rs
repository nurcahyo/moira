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
    cors::{AllowOrigin, Any, CorsLayer},
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    trace::TraceLayer,
};

use crate::{app::AppState, config::CorsSettings, error::AppError};

pub fn build_router(state: AppState) -> Result<Router, AppError> {
    let metrics_state = state.clone();
    let mut router = http::router()
        .layer(middleware::from_fn_with_state(
            metrics_state,
            metrics_middleware,
        ))
        .layer(middleware::from_fn(secure_response_headers))
        .layer(DefaultBodyLimit::max(512 * 1024))
        .layer(TraceLayer::new_for_http())
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid));

    if let Some(cors) = cors_layer(&state.settings.cors)? {
        router = router.layer(cors);
    }

    Ok(router.with_state(state))
}

fn cors_layer(settings: &CorsSettings) -> Result<Option<CorsLayer>, AppError> {
    if settings.allowed_origins.is_empty() {
        return Ok(None);
    }

    let mut layer = CorsLayer::new().allow_methods(Any).allow_headers(Any);
    if settings.allowed_origins.iter().any(|origin| origin == "*") {
        layer = layer.allow_origin(Any);
    } else {
        let origins = settings.allowed_origin_headers()?;
        layer = layer.allow_origin(AllowOrigin::list(origins));
    }
    Ok(Some(layer))
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
        let _router = build_router(state).unwrap();
    }

    #[test]
    fn cors_can_be_disabled_or_allowlisted() {
        assert!(
            cors_layer(&CorsSettings {
                allowed_origins: Vec::new()
            })
            .unwrap()
            .is_none()
        );
        assert!(
            cors_layer(&CorsSettings {
                allowed_origins: vec!["https://admin.example.com".to_string()]
            })
            .unwrap()
            .is_some()
        );
    }

    #[test]
    fn invalid_cors_origin_is_rejected() {
        let error = cors_layer(&CorsSettings {
            allowed_origins: vec!["not a valid origin".to_string()],
        })
        .unwrap_err();
        assert!(error.to_string().contains("invalid CORS allowed origin"));
    }
}
