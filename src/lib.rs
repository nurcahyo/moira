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
    body::Body,
    extract::State,
    http::{HeaderValue, Request, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use std::{any::Any, time::Instant};
use tower_http::{
    catch_panic::CatchPanicLayer,
    cors::{AllowOrigin, Any as AnyOrigin, CorsLayer},
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    trace::TraceLayer,
};
use tracing::Span;

use crate::{
    app::AppState,
    config::{CorsSettings, DeploymentEnvironment},
    error::{AppError, REQUEST_ID},
    http::RouterPolicy,
};

/// i18n key for the caught-panic response. Panics deliberately reuse the existing
/// `internal_error` code — telling a caller "this was a panic" is information
/// disclosure with no client benefit. The panic payload goes to `tracing` only.
const INTERNAL_ERROR_KEY: &str = "moira.error.internal_error";
const INTERNAL_ERROR_FALLBACK: &str = "An unexpected error occurred.";
const PAYLOAD_TOO_LARGE_KEY: &str = "moira.error.payload_too_large";
const PAYLOAD_TOO_LARGE_FALLBACK: &str = "The request body exceeds the maximum allowed size.";
const REQUEST_TIMEOUT_KEY: &str = "moira.error.request_timeout";
const REQUEST_TIMEOUT_FALLBACK: &str = "The request timed out before it could be completed.";

/// Assembles Moira's HTTP stack.
///
/// **Compression is deliberately absent.** Moira installs no `CompressionLayer`
/// anywhere. If response compression is ever added it **MUST** exclude the four
/// once-only-secret routes that return `ApiKeySecretResponse` —
/// `POST /api/v1/admin/system-keys`, `POST /api/v1/admin/system-keys/{id}/rotate`,
/// `POST /api/v1/admin/consumer-keys`, `POST /api/v1/admin/consumer-keys/{id}/rotate` —
/// because compressing an attacker-influenced response that also carries a
/// once-only secret opens a BREACH-style compression side channel. The regression
/// guard is `once_only_secret_routes_carry_no_content_encoding` below.
///
/// Layer order (innermost first, i.e. the order the `.layer()` calls appear):
/// per-route body limits and the non-SSE timeout live in [`http::router`], then
/// `CatchPanicLayer`, then the infrastructure-error envelope mapper, then metrics,
/// secure headers, tracing, and the request-id chain.
pub fn build_router(state: AppState) -> Result<Router, AppError> {
    let metrics_state = state.clone();
    let hsts_enabled = matches!(
        state.settings.deployment.environment,
        DeploymentEnvironment::Production
    );
    let mut router = http::router(RouterPolicy::from_settings(&state.settings))
        // Innermost: catches panics raised by handlers/extractors so the caller gets a
        // 500 envelope instead of a dropped connection. Sitting inside the layers below
        // means the synthesised 500 still receives secure headers and is counted by the
        // metrics middleware.
        .layer(CatchPanicLayer::custom(handle_panic))
        .layer(middleware::from_fn(infrastructure_error_envelope))
        .layer(middleware::from_fn_with_state(
            metrics_state,
            metrics_middleware,
        ))
        .layer(middleware::from_fn_with_state(
            hsts_enabled,
            secure_response_headers,
        ))
        .layer(TraceLayer::new_for_http().make_span_with(redacted_request_span))
        .layer(middleware::from_fn(request_id_context))
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid));

    if let Some(cors) = cors_layer(&state.settings.cors)? {
        router = router.layer(cors);
    }

    Ok(router.with_state(state))
}

fn catalog_message(key: &str, fallback: &'static str) -> String {
    i18n::default_message_for_key(key)
        .unwrap_or(fallback)
        .to_string()
}

/// Turns a panic payload into the standard error envelope.
///
/// The payload itself is written to `tracing::error!` and **never** to the response:
/// panic messages routinely carry internal state (row ids, credential fingerprints,
/// partially-formatted secrets).
fn handle_panic(payload: Box<dyn Any + Send + 'static>) -> Response {
    tracing::error!(
        panic.payload = %panic_payload_description(payload.as_ref()),
        "handler panicked; returning a 500 error envelope"
    );
    AppError::Internal(catalog_message(INTERNAL_ERROR_KEY, INTERNAL_ERROR_FALLBACK)).into_response()
}

fn panic_payload_description(payload: &(dyn Any + Send + 'static)) -> String {
    if let Some(message) = payload.downcast_ref::<&'static str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "<non-string panic payload>".to_string()
    }
}

/// Rewrites the bodyless/plain-text responses produced by infrastructure layers into
/// Moira's standard `ErrorResponse` envelope.
///
/// Two producers exist today and neither goes through `AppError`:
/// * Axum's `DefaultBodyLimit` rejects an oversized body with a bare `text/plain` 413
///   carrying no `code`, no `message_key` and no `request_id`.
/// * `tower_http`'s `TimeoutLayer` returns a completely empty 504.
///
/// Responses that already carry `application/json` are left untouched — that is what
/// keeps a genuine application-level 504 (`ExecutionFailureClass::ProviderTimeout` /
/// `DeadlineExceeded`, `src/application/public.rs`) from being clobbered with the
/// transport-level `request_timeout` code.
async fn infrastructure_error_envelope(req: Request<Body>, next: Next) -> Response {
    normalize_infrastructure_error(next.run(req).await)
}

fn normalize_infrastructure_error(response: Response) -> Response {
    let error = match response.status() {
        StatusCode::PAYLOAD_TOO_LARGE => AppError::coded(
            StatusCode::PAYLOAD_TOO_LARGE,
            "payload_too_large",
            catalog_message(PAYLOAD_TOO_LARGE_KEY, PAYLOAD_TOO_LARGE_FALLBACK),
        ),
        StatusCode::GATEWAY_TIMEOUT => AppError::coded(
            StatusCode::GATEWAY_TIMEOUT,
            "request_timeout",
            catalog_message(REQUEST_TIMEOUT_KEY, REQUEST_TIMEOUT_FALLBACK),
        ),
        _ => return response,
    };

    if is_json_response(&response) {
        return response;
    }
    error.into_response()
}

fn is_json_response(response: &Response) -> bool {
    response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.trim_start().starts_with("application/json"))
}

/// Tracing span for the HTTP layer.
///
/// `tower_http`'s `DefaultMakeSpan` already omits headers unless `include_headers(true)`
/// is set, so the previous bare `TraceLayer::new_for_http()` was not leaking
/// `Authorization`. This replacement makes the exclusion explicit and additionally
/// narrows `uri` to `uri.path()` (dropping any query string) and records the request
/// id for correlation. It records **no** header values — in particular not
/// `Authorization`, `X-Api-Key`, `X-System-Key` or `X-Consumer-Key`.
fn redacted_request_span(request: &Request<Body>) -> Span {
    tracing::debug_span!(
        "http_request",
        method = %request.method(),
        path = %request.uri().path(),
        version = ?request.version(),
        request_id = request
            .headers()
            .get("x-request-id")
            .and_then(|value| value.to_str().ok())
            .unwrap_or(""),
    )
}

async fn request_id_context(req: Request<Body>, next: Next) -> Response {
    let request_id = req
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    REQUEST_ID.scope(request_id, next.run(req)).await
}

fn cors_layer(settings: &CorsSettings) -> Result<Option<CorsLayer>, AppError> {
    if settings.allowed_origins.is_empty() {
        return Ok(None);
    }

    let mut layer = CorsLayer::new()
        .allow_methods(AnyOrigin)
        .allow_headers(AnyOrigin);
    if settings.allowed_origins.iter().any(|origin| origin == "*") {
        layer = layer.allow_origin(AnyOrigin);
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

/// `Strict-Transport-Security` value applied in production deployments only:
/// two years, subdomains included. Deliberately no `preload` token — opting a domain
/// into the browser preload list is an operator decision, not a library default.
const HSTS_VALUE: &str = "max-age=63072000; includeSubDomains";

async fn secure_response_headers(
    State(hsts_enabled): State<bool>,
    req: Request<Body>,
    next: Next,
) -> Response {
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
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    // Moira serves only application/json, text/event-stream and text/plain; no script,
    // style, image or frame context is ever needed.
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'"),
    );
    // HSTS is production-only: sending it unconditionally pins http://localhost to
    // HTTPS in developer browsers and breaks local tooling.
    if hsts_enabled {
        headers.insert(
            header::STRICT_TRANSPORT_SECURITY,
            HeaderValue::from_static(HSTS_VALUE),
        );
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{app::AppState, config::Settings, error::ErrorResponse};
    use axum::{
        body::to_bytes,
        http::{Method, Request},
    };
    use tower::ServiceExt;

    const ONCE_ONLY_SECRET_ROUTES: [&str; 4] = [
        "/api/v1/admin/system-keys",
        "/api/v1/admin/system-keys/00000000-0000-0000-0000-000000000000/rotate",
        "/api/v1/admin/consumer-keys",
        "/api/v1/admin/consumer-keys/00000000-0000-0000-0000-000000000000/rotate",
    ];

    fn router_for(settings: Settings) -> Router {
        build_router(AppState::new(settings, None).unwrap()).unwrap()
    }

    async fn send(router: Router, request: Request<Body>) -> Response {
        router.oneshot(request).await.unwrap()
    }

    async fn error_body(response: Response) -> ErrorResponse {
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).expect("standard error envelope")
    }

    #[test]
    fn router_builds_with_phase_one_routes() {
        let state = AppState::new(Settings::default(), None).unwrap();
        let _router = build_router(state).unwrap();
    }

    #[test]
    fn router_still_builds_with_the_full_middleware_stack() {
        // Catches layer-ordering/type breakage in the timeout, catch-panic, body-limit
        // and tracing layers at compile+construction time, in both HSTS modes.
        let _development = router_for(Settings::default());

        let mut production = Settings::default();
        production.deployment.environment = DeploymentEnvironment::Production;
        let _production = router_for(production);
    }

    #[tokio::test]
    async fn secure_response_headers_include_frame_options_and_csp() {
        let response = send(
            router_for(Settings::default()),
            Request::builder()
                .uri("/health/live")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

        let headers = response.headers();
        assert_eq!(headers[header::X_FRAME_OPTIONS], "DENY");
        assert_eq!(
            headers[header::CONTENT_SECURITY_POLICY],
            "default-src 'none'"
        );
        // The three pre-existing headers must survive the reworked stack.
        assert_eq!(headers[header::CACHE_CONTROL], "no-store");
        assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        assert_eq!(headers[header::REFERRER_POLICY], "no-referrer");
    }

    #[tokio::test]
    async fn hsts_is_absent_outside_production() {
        for environment in [
            DeploymentEnvironment::Development,
            DeploymentEnvironment::Test,
        ] {
            let mut settings = Settings::default();
            settings.deployment.environment = environment;
            let response = send(
                router_for(settings),
                Request::builder()
                    .uri("/health/live")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
            assert!(
                !response
                    .headers()
                    .contains_key(header::STRICT_TRANSPORT_SECURITY),
                "HSTS must not be sent in {environment:?}"
            );
        }
    }

    #[tokio::test]
    async fn hsts_is_present_under_a_production_deployment_environment() {
        let mut settings = Settings::default();
        settings.deployment.environment = DeploymentEnvironment::Production;
        let response = send(
            router_for(settings),
            Request::builder()
                .uri("/health/live")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

        assert_eq!(
            response.headers()[header::STRICT_TRANSPORT_SECURITY],
            "max-age=63072000; includeSubDomains"
        );
    }

    #[tokio::test]
    async fn panic_response_body_contains_no_panic_payload() {
        let payload = "credential row 42 pepper v1 unwrap on None";
        let response = handle_panic(Box::new(payload));

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = error_body(response).await;
        assert_eq!(body.error.code, "internal_error");
        assert_eq!(body.error.message_key, INTERNAL_ERROR_KEY);
        assert!(i18n::is_known_key(&body.error.message_key));
        assert!(!body.error.message.is_empty());
        assert!(!body.error.request_id.is_empty());
        for fragment in ["credential", "pepper", "unwrap", "42"] {
            assert!(
                !body.error.message.contains(fragment),
                "panic payload fragment {fragment:?} leaked into the client message"
            );
        }
    }

    #[test]
    fn panic_payload_description_never_returns_an_empty_label() {
        assert_eq!(panic_payload_description(&"boom"), "boom");
        assert_eq!(
            panic_payload_description(&"boom".to_string()),
            "boom".to_string()
        );
        assert_eq!(
            panic_payload_description(&7u32),
            "<non-string panic payload>"
        );
    }

    #[tokio::test]
    async fn oversized_public_body_is_rejected_with_the_standard_envelope() {
        let settings = Settings::default();
        let limit = usize::try_from(settings.public_api.maximum_request_bytes).unwrap();
        let oversized = vec![b'a'; limit + 1];

        let response = send(
            router_for(settings),
            Request::builder()
                .method(Method::POST)
                .uri("/api/v1/responses")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(oversized))
                .unwrap(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert!(is_json_response(&response));
        let body = error_body(response).await;
        assert_eq!(body.error.code, "payload_too_large");
        assert_eq!(body.error.message_key, PAYLOAD_TOO_LARGE_KEY);
        assert!(i18n::is_known_key(&body.error.message_key));
        assert!(!body.error.message.is_empty());
        assert!(!body.error.request_id.is_empty());
    }

    #[tokio::test]
    async fn public_body_at_the_configured_maximum_request_bytes_is_not_rejected() {
        // The load-bearing half of the pair above: the old global layer capped every
        // route at 512 KiB, so a 1 MiB body used to 413. Accepting a body of exactly
        // `PublicApiSettings.maximum_request_bytes` is what proves the *configured*
        // value is now the enforced one. (The body is not valid JSON, so the request
        // is rejected later by the parser — the point is that it is not rejected by
        // the body-limit layer.)
        let settings = Settings::default();
        let limit = usize::try_from(settings.public_api.maximum_request_bytes).unwrap();
        assert!(limit > 512 * 1024, "guarding against a weakened default");
        let at_limit = vec![b'a'; limit];

        let response = send(
            router_for(settings),
            Request::builder()
                .method(Method::POST)
                .uri("/api/v1/responses")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(at_limit))
                .unwrap(),
        )
        .await;

        assert_ne!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn conversation_routes_enforce_their_own_fixed_limit() {
        let oversized = vec![b'a'; http::CONVERSATION_BODY_LIMIT_BYTES + 1];
        const { assert!(http::CONVERSATION_BODY_LIMIT_BYTES < http::ADMIN_BODY_LIMIT_BYTES) };

        let response = send(
            router_for(Settings::default()),
            Request::builder()
                .method(Method::POST)
                .uri("/api/v1/conversations")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(oversized))
                .unwrap(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(error_body(response).await.error.code, "payload_too_large");
    }

    #[tokio::test]
    async fn admin_routes_allow_bodies_above_the_public_limit() {
        // Proves the limits are genuinely per-route rather than one global layer: a body
        // larger than the public limit must not be rejected on an admin route.
        let settings = Settings::default();
        let limit = usize::try_from(settings.public_api.maximum_request_bytes).unwrap();
        assert!(limit < http::ADMIN_BODY_LIMIT_BYTES);
        let body = vec![b'a'; limit + 1];

        let response = send(
            router_for(settings),
            Request::builder()
                .method(Method::POST)
                .uri("/api/v1/admin/applications")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await;

        assert_ne!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn admin_bodies_above_the_admin_limit_are_rejected_with_the_same_envelope() {
        let oversized = vec![b'a'; http::ADMIN_BODY_LIMIT_BYTES + 1];

        let response = send(
            router_for(Settings::default()),
            Request::builder()
                .method(Method::POST)
                .uri("/api/v1/admin/applications")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(oversized))
                .unwrap(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let body = error_body(response).await;
        assert_eq!(body.error.code, "payload_too_large");
    }

    #[tokio::test]
    async fn timeout_responses_are_mapped_to_the_request_timeout_envelope() {
        // The mapper's input is exactly what `tower_http`'s `TimeoutLayer` emits: the
        // configured status with a default (empty, header-less) response.
        let mut timed_out = Response::new(Body::empty());
        *timed_out.status_mut() = StatusCode::GATEWAY_TIMEOUT;

        let response = normalize_infrastructure_error(timed_out);
        assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
        assert!(is_json_response(&response));

        let body = error_body(response).await;
        assert_eq!(body.error.code, "request_timeout");
        assert_eq!(body.error.message_key, REQUEST_TIMEOUT_KEY);
        assert!(i18n::is_known_key(&body.error.message_key));
        assert!(!body.error.message.is_empty());
        assert!(!body.error.request_id.is_empty());
    }

    #[tokio::test]
    async fn the_timeout_layer_and_the_envelope_mapper_compose_into_a_504_envelope() {
        // Deterministic: the handler future never completes, so the timeout always wins.
        // No `sleep()`-based interleaving is involved.
        let router: Router = Router::new()
            .route(
                "/never",
                axum::routing::get(|| async {
                    std::future::pending::<()>().await;
                    StatusCode::OK
                }),
            )
            .layer(tower_http::timeout::TimeoutLayer::with_status_code(
                StatusCode::GATEWAY_TIMEOUT,
                std::time::Duration::from_millis(1),
            ))
            .layer(middleware::from_fn(infrastructure_error_envelope));

        let response = send(
            router,
            Request::builder()
                .uri("/never")
                .body(Body::empty())
                .unwrap(),
        )
        .await;

        assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
        let body = error_body(response).await;
        assert_eq!(body.error.code, "request_timeout");
        assert_eq!(body.error.message_key, REQUEST_TIMEOUT_KEY);
        assert!(!body.error.message.is_empty());
        assert!(!body.error.request_id.is_empty());
    }

    #[test]
    fn the_non_streaming_timeout_sits_above_the_execution_deadline() {
        let settings = Settings::default();
        let policy = RouterPolicy::from_settings(&settings);

        assert_eq!(
            policy.non_streaming_timeout.as_secs(),
            settings.runtime.maximum_execution_timeout_seconds
                + http::NON_STREAMING_TIMEOUT_BUFFER_SECONDS
        );
        assert!(
            policy.non_streaming_timeout.as_secs()
                > settings.runtime.maximum_execution_timeout_seconds
        );
        assert_eq!(
            policy.public_body_limit_bytes,
            usize::try_from(settings.public_api.maximum_request_bytes).unwrap()
        );
        assert_eq!(policy.admin_body_limit_bytes, http::ADMIN_BODY_LIMIT_BYTES);
    }

    #[tokio::test]
    async fn application_level_json_errors_are_never_rewritten() {
        // `ExecutionFailureClass::ProviderTimeout` already maps to a 504 with its own
        // code; the infrastructure mapper must leave it alone.
        let original = AppError::coded(
            StatusCode::GATEWAY_TIMEOUT,
            "upstream_timeout",
            "provider timed out",
        )
        .into_response();

        let response = normalize_infrastructure_error(original);
        let body = error_body(response).await;
        assert_eq!(body.error.code, "upstream_timeout");
    }

    #[tokio::test]
    async fn once_only_secret_routes_carry_no_content_encoding() {
        // Regression guard for the compression rule documented on `build_router`: today
        // there is no `CompressionLayer`, and these four routes must never gain one.
        for path in ONCE_ONLY_SECRET_ROUTES {
            let response = send(
                router_for(Settings::default()),
                Request::builder()
                    .method(Method::POST)
                    .uri(path)
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::ACCEPT_ENCODING, "gzip, deflate, br, zstd")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await;

            assert!(
                !response.headers().contains_key(header::CONTENT_ENCODING),
                "{path} returned a Content-Encoding header; once-only secret responses \
                 must never be compressed"
            );
        }
    }

    #[test]
    fn middleware_error_messages_come_from_the_i18n_catalog() {
        for (key, fallback) in [
            (INTERNAL_ERROR_KEY, INTERNAL_ERROR_FALLBACK),
            (PAYLOAD_TOO_LARGE_KEY, PAYLOAD_TOO_LARGE_FALLBACK),
            (REQUEST_TIMEOUT_KEY, REQUEST_TIMEOUT_FALLBACK),
        ] {
            assert!(i18n::is_known_key(key), "{key} is missing from the catalog");
            let message = catalog_message(key, fallback);
            assert!(!message.is_empty());
            assert_eq!(message, fallback, "{key} drifted from the Rust catalog");
        }
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
