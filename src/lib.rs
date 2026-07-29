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
    extract::{MatchedPath, State},
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
/// per-route body limits, the request-body ingestion timeout and the response-head
/// timeout live in [`http::router`], then `CatchPanicLayer`, then the
/// infrastructure-error envelope mapper, then metrics, secure headers, tracing, and the
/// request-id chain.
///
/// **Known residual gap — `CatchPanicLayer` does not cover streaming response bodies.**
/// `tower_http::catch_panic` wraps exactly two things: the synchronous `inner.call(req)`
/// and the response *future*. It does not wrap the response **body**. A panic raised
/// while a handler is running, or while an extractor is running, is therefore caught and
/// converted by [`handle_panic`] into a 500 envelope; a panic raised inside a body that
/// has already been handed back is not. Moira's SSE path
/// (`Sse::new(stream)` over an `async_stream` in `src/http/public.rs`) is exactly that
/// second shape, so a panic inside the stream produces **zero bytes and a bare
/// connection close** — no status, no envelope — rather than a 500. Measured:
/// `/internal/test/panic` (handler panic) returns 500 with the full envelope, while a
/// panic raised from inside a streaming body returns nothing at all. The process
/// survives either way; the blast radius is one connection.
///
/// This is **latent, not live**: the SSE handler and the runtime stream it wraps contain
/// no `unwrap`, `expect` or `panic!` today. It is documented rather than fixed because
/// the only real fixes are to wrap every streamed item in `catch_unwind` (which costs a
/// `Box`/`AssertUnwindSafe` per chunk on the hot path and cannot produce a status code
/// anyway — the head is already sent, so the best possible outcome is an
/// `event: error` frame, not a 500) or to forbid panics in stream code by review. The
/// invariant to hold instead: **no `unwrap`/`expect`/`panic!`/slicing/indexing in any
/// code reachable from a streamed item**. Any panic that does occur there is still
/// recorded by the runtime's own error handling, not silently dropped.
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
    // The route label is the matched route **template** (`/api/v1/admin/applications/{id}`),
    // taken from `MatchedPath`, never `req.uri().path()`. A resolved path carries
    // request-scoped UUIDs, which would make the histogram's label set unbounded — a
    // memory-exhaustion vector in the scrape path, not merely untidy output. Requests that
    // matched no route carry no `MatchedPath`; `None` is folded into a single constant label
    // inside `MetricsRegistry`, again rather than falling back to the raw URI.
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map(|matched| matched.as_str().to_owned());
    let method = req.method().clone();
    let response = next.run(req).await;
    let status = response.status();
    let latency = started.elapsed();
    state.metrics.record_http_response(status, latency);
    state
        .metrics
        .record_http_latency(route.as_deref(), &method, status, latency);
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

    #[tokio::test]
    async fn metrics_middleware_labels_routes_by_template_never_by_resolved_path() {
        // The half of the low-cardinality contract that lives outside `MetricsRegistry`:
        // `metrics_middleware` must read `MatchedPath`, not `req.uri().path()`. Driven
        // through the real router so the assertion covers the actual extension plumbing.
        let application_id = "3f1a5c4e-9d2b-4f7a-8c11-6b0d5e7a2f93";
        let state = AppState::new(Settings::default(), None).unwrap();
        let metrics = state.metrics.clone();
        let router = build_router(state).unwrap();

        // A parameterised admin route: unauthenticated, so it 401s, but it *matched*, which
        // is all `MatchedPath` needs.
        let _ = send(
            router.clone(),
            Request::builder()
                .uri(format!("/api/v1/admin/applications/{application_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        // A path that matches nothing at all: the fallback label must be a constant.
        let _ = send(
            router,
            Request::builder()
                .uri(format!("/definitely/not/a/route/{application_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await;

        let rendered = metrics.render_prometheus("moira", false, false);
        assert!(
            rendered.contains("route=\"/api/v1/admin/applications/{id}\""),
            "expected the matched route template in:\n{rendered}"
        );
        assert!(
            !rendered.contains(application_id),
            "the resolved path's identifier reached a metric label:\n{rendered}"
        );
        assert!(
            !rendered.contains("/definitely/not/a/route"),
            "an unmatched raw path reached a metric label:\n{rendered}"
        );
        assert!(rendered.contains("route=\"unmatched\""));
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

    #[test]
    fn every_route_group_including_the_sse_group_is_bounded() {
        // The SSE group used to carry no timeout at all, which removed the bound on
        // request *ingestion* as well as the one on the response body. Both bounds must
        // now be present and non-zero for every group.
        let policy = RouterPolicy::from_settings(&Settings::default());

        assert_eq!(
            policy.streaming_head_timeout, policy.non_streaming_timeout,
            "the SSE group's head phase is bounded by the execution deadline like every \
             other route; only its response body escapes the ceiling"
        );
        assert!(!policy.streaming_head_timeout.is_zero());
        assert_eq!(
            policy.request_body_idle_timeout.as_secs(),
            http::REQUEST_BODY_IDLE_TIMEOUT_SECONDS
        );
        assert!(policy.request_body_idle_timeout < policy.non_streaming_timeout);
    }

    #[test]
    fn the_public_body_limit_is_clamped_to_an_enforceable_ceiling() {
        // `maximum_request_bytes` is an unbounded i64 that `Settings::validate` does not
        // bound, so `i64::MAX` used to become an effectively unlimited in-memory body.
        let mut absurd = Settings::default();
        absurd.public_api.maximum_request_bytes = i64::MAX;
        assert_eq!(
            RouterPolicy::from_settings(&absurd).public_body_limit_bytes,
            http::MAX_PUBLIC_BODY_LIMIT_BYTES
        );

        let mut just_over = Settings::default();
        just_over.public_api.maximum_request_bytes =
            i64::try_from(http::MAX_PUBLIC_BODY_LIMIT_BYTES).unwrap() + 1;
        assert_eq!(
            RouterPolicy::from_settings(&just_over).public_body_limit_bytes,
            http::MAX_PUBLIC_BODY_LIMIT_BYTES
        );

        // Exactly at the ceiling, and every legitimate value below it, is untouched.
        let mut at_ceiling = Settings::default();
        at_ceiling.public_api.maximum_request_bytes =
            i64::try_from(http::MAX_PUBLIC_BODY_LIMIT_BYTES).unwrap();
        assert_eq!(
            RouterPolicy::from_settings(&at_ceiling).public_body_limit_bytes,
            http::MAX_PUBLIC_BODY_LIMIT_BYTES
        );

        // Non-positive and overflowing values keep falling back to the default.
        for configured in [0, -1, i64::MIN] {
            let mut settings = Settings::default();
            settings.public_api.maximum_request_bytes = configured;
            assert_eq!(
                RouterPolicy::from_settings(&settings).public_body_limit_bytes,
                http::CONVERSATION_BODY_LIMIT_BYTES,
                "unusable value {configured} must fall back, not clamp upward"
            );
        }
    }

    /// The load-bearing half of the SSE exemption: a `TimeoutLayer` on a route that
    /// returns a streaming body must bound only the response *head*.
    ///
    /// `tower_http::timeout::Timeout`'s response future drops its `Sleep` as soon as the
    /// inner service resolves to a `Response`; the body is polled afterwards, outside the
    /// tower stack. This test pins that behaviour so the SSE group can safely carry a
    /// head timeout. The single `sleep` is the assertion itself — proving a stream
    /// outlives a duration requires that duration to elapse — and every frame boundary is
    /// an explicit acknowledgement handshake, not a timing race.
    #[tokio::test]
    async fn a_timeout_layer_never_caps_an_already_returned_streaming_body() {
        use axum::body::Bytes;
        use futures_util::StreamExt;
        use std::sync::{Arc, Mutex};
        use std::time::Duration;

        const HEAD_TIMEOUT: Duration = Duration::from_millis(50);

        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<&'static str>();
        let receiver = Arc::new(Mutex::new(Some(rx)));

        let router: Router = Router::new()
            .route(
                "/stream",
                axum::routing::get(move || {
                    let receiver = receiver.clone();
                    async move {
                        let mut rx = receiver
                            .lock()
                            .unwrap()
                            .take()
                            .expect("the route is called once");
                        Response::new(Body::from_stream(async_stream::stream! {
                            while let Some(chunk) = rx.recv().await {
                                yield Ok::<_, std::convert::Infallible>(Bytes::from_static(
                                    chunk.as_bytes(),
                                ));
                            }
                        }))
                    }
                }),
            )
            .layer(tower_http::timeout::TimeoutLayer::with_status_code(
                StatusCode::GATEWAY_TIMEOUT,
                HEAD_TIMEOUT,
            ));

        tx.send("first").unwrap();
        let response = send(
            router,
            Request::builder()
                .uri("/stream")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);

        let mut frames = response.into_body().into_data_stream();
        assert_eq!(
            frames.next().await.expect("first frame").unwrap(),
            Bytes::from_static(b"first")
        );

        // Hold the body open for many multiples of the head timeout.
        tokio::time::sleep(HEAD_TIMEOUT * 6).await;

        tx.send("last").unwrap();
        drop(tx);
        assert_eq!(
            frames
                .next()
                .await
                .expect("frame after the timeout")
                .unwrap(),
            Bytes::from_static(b"last"),
            "the timeout layer must not cap a body it already handed back"
        );
        assert!(frames.next().await.is_none(), "the stream must end cleanly");
    }

    /// Generous relative to every timeout the probes below configure, and orders of
    /// magnitude below the >= 630 s (or unbounded) window the connection was held for
    /// before this fix.
    const PROBE_PATIENCE: std::time::Duration = std::time::Duration::from_secs(10);

    /// The three routes that consume a request body and were measured parked: the two
    /// SSE-group routes plus one non-SSE control.
    const STALLED_BODY_PROBE_PATHS: [&str; 3] = [
        "/api/v1/responses/stream",
        "/v1/responses",
        "/api/v1/responses",
    ];

    /// Opens a real socket, writes request headers announcing a body, never sends the
    /// body, and returns the first bytes the server writes back.
    ///
    /// This is the exact shape of the measured denial of service: authentication is never
    /// reached, because `public_actor` runs inside the handler, *after* `Json` extraction.
    /// `DefaultBodyLimit` bounds bytes, not time, and neither `stream_idle_timeout_ms` nor
    /// `maximum_execution_timeout_seconds` engages because execution never begins.
    async fn stalled_body_status_line(policy: RouterPolicy, path: &str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let state = AppState::new(Settings::default(), None).unwrap();
        let app: Router = http::router(policy).with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
        socket
            .write_all(
                format!(
                    "POST {path} HTTP/1.1\r\nHost: localhost\r\n\
                     Content-Type: application/json\r\nContent-Length: 4096\r\n\r\n"
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        socket.flush().await.unwrap();

        let mut head = [0u8; 64];
        let read = tokio::time::timeout(PROBE_PATIENCE, socket.read(&mut head))
            .await
            .unwrap_or_else(|_| {
                panic!("{path} parked the connection: no response within {PROBE_PATIENCE:?}")
            })
            .unwrap_or_else(|error| panic!("{path} socket error: {error}"));
        server.abort();

        assert!(read > 0, "{path} closed without writing a response");
        String::from_utf8_lossy(&head[..read]).to_string()
    }

    /// The other half of the SSE exemption: the streaming group's response *head* is
    /// bounded, so `/v1/responses` with `stream: false` — a fully non-streaming JSON
    /// route that shares the group only because it shares a path with the streaming one —
    /// satisfies the plan's "every non-SSE route runs under `TimeoutLayer`" requirement.
    ///
    /// Deterministic without a database and without racing a timer: the handler cannot
    /// complete, because the request body it is waiting on never arrives, so the head
    /// timeout is the only thing that can produce a response. The ingestion timeout is
    /// pushed far out so that it cannot be what fires.
    #[tokio::test]
    async fn the_streaming_group_head_is_governed_by_a_timeout() {
        use std::time::Duration;

        let policy = RouterPolicy {
            non_streaming_timeout: Duration::from_millis(300),
            streaming_head_timeout: Duration::from_millis(300),
            request_body_idle_timeout: Duration::from_secs(3_600),
            ..RouterPolicy::default()
        };

        for path in STALLED_BODY_PROBE_PATHS {
            let status_line = stalled_body_status_line(policy, path).await;
            assert!(
                status_line.starts_with("HTTP/1.1 504"),
                "{path} is not covered by a response-head timeout, got: {status_line}"
            );
        }
    }

    /// The DoS this closes, reproduced over a real socket.
    ///
    /// Before this fix the SSE group carried no timeout of any kind — the head timeout was
    /// deliberately omitted to avoid capping a stream, which also removed the bound on
    /// request ingestion — so `/api/v1/responses/stream` and `/v1/responses` held a
    /// connection task and a file descriptor indefinitely for an unauthenticated client
    /// that wrote headers and no body. Here the head timeout is left at its production
    /// value (>= 630 s, so it cannot be what fires) and only the ingestion bound is tight.
    #[tokio::test]
    async fn a_stalled_request_body_cannot_park_a_connection_forever() {
        use std::time::Duration;

        let policy = RouterPolicy {
            request_body_idle_timeout: Duration::from_millis(300),
            ..RouterPolicy::default()
        };
        assert!(
            policy.streaming_head_timeout > PROBE_PATIENCE,
            "the head timeout must not be what releases the connection here"
        );

        for path in STALLED_BODY_PROBE_PATHS {
            let status_line = stalled_body_status_line(policy, path).await;
            assert!(
                status_line.starts_with("HTTP/1.1 4"),
                "{path} should reject the stalled body with a 4xx, got: {status_line}"
            );
        }
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
