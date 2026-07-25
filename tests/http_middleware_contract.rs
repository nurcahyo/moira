//! Plan 03 / finding P1-3, Verification layer 2 — the production HTTP middleware stack.
//!
//! Everything here is asserted over a **real socket** rather than through
//! `tower::ServiceExt::oneshot`, because the properties under test are exactly the ones
//! an in-process call cannot observe: layer ordering can strip a header, a panic without
//! `CatchPanicLayer` drops the TCP connection with no response at all, and an SSE body is
//! only "long-lived" once it is actually being written to a socket.
//!
//! ## The two probe routes
//!
//! Two of these tests need a handler that panics on demand and a handler that never
//! completes. Neither is reachable from any production route inside a test:
//!
//! * nothing in Moira panics on demand, and
//! * `RouterPolicy::non_streaming_timeout` is `maximum_execution_timeout_seconds + 30`,
//!   which is (a) never below 30 s and (b) always above the execution deadline by
//!   construction — so no production route can be made to reach it.
//!
//! An integration test also links the library compiled **without** `cfg(test)`, so a
//! `#[cfg(test)]` route would be invisible here. The two probes therefore live behind the
//! off-by-default `test-routes` cargo feature (`src/http/mod.rs`), which `cargo build
//! --release --locked` does not enable. `probe_routes_exist_only_under_the_test_routes_feature`
//! asserts both halves of that gate, so a build that accidentally shipped them would fail
//! this suite.
//!
//! Concurrency discipline (`plans/CONVENTIONS.md` §3): no `sleep()`. The SSE test's
//! ordering gate is a *completed 504 response* — a real elapsed timeout observed over the
//! wire — plus the mock provider's `ScriptGate` acknowledgements.

mod support;

use std::time::Duration;

use futures_util::StreamExt;
use moira::{
    app::AppState,
    config::Settings,
    http::{ADMIN_BODY_LIMIT_BYTES, RouterPolicy},
    infra::db,
};
use reqwest::{Client, Response, StatusCode, header};
use serde_json::{Value, json};
use sqlx::{PgPool, postgres::PgPoolOptions};
use tokio::{net::TcpListener, task::JoinHandle, time::timeout};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use support::{
    LifecycleFixture, MoiraHttpServer, RuntimePolicy,
    mock_openai::{MockOpenAiServer, ProviderScript, ScriptGate},
    public_response_request,
};

const WAIT: Duration = Duration::from_secs(20);

/// The four routes that return `ApiKeySecretResponse` — the same set covered by the
/// `once_only_key_responses_use_the_secret_envelope` OpenAPI test in `src/http/mod.rs`.
const ONCE_ONLY_SECRET_ROUTES: [&str; 4] = [
    "/api/v1/admin/system-keys",
    "/api/v1/admin/system-keys/00000000-0000-0000-0000-000000000000/rotate",
    "/api/v1/admin/consumer-keys",
    "/api/v1/admin/consumer-keys/00000000-0000-0000-0000-000000000000/rotate",
];

struct MiddlewareFixture {
    pool: PgPool,
    server: MoiraHttpServer,
    client: Client,
    public_limit: usize,
    suffix: String,
}

impl MiddlewareFixture {
    async fn new() -> Option<Self> {
        let database_url = match std::env::var("MOIRA_TEST_DATABASE_URL") {
            Ok(value) if !value.trim().is_empty() => value,
            _ if std::env::var("CI").is_ok_and(|value| value.eq_ignore_ascii_case("true")) => {
                panic!("MOIRA_TEST_DATABASE_URL is required when CI=true for HTTP middleware tests")
            }
            _ => {
                eprintln!("skipping HTTP middleware tests: MOIRA_TEST_DATABASE_URL is not set");
                return None;
            }
        };
        let pool = timeout(
            WAIT,
            PgPoolOptions::new()
                .max_connections(8)
                .connect(&database_url),
        )
        .await
        .expect("database connection timed out")
        .expect("connect middleware test database");
        timeout(WAIT, db::migrate(&pool))
            .await
            .expect("migrations timed out")
            .expect("run migrations");

        let settings = Settings::default();
        let public_limit = usize::try_from(settings.public_api.maximum_request_bytes)
            .expect("maximum_request_bytes fits usize");
        let state = AppState::new(settings, Some(pool.clone())).expect("middleware app state");
        let server = MoiraHttpServer::start(state).await;

        Some(Self {
            pool,
            server,
            client: Client::new(),
            public_limit,
            suffix: Uuid::now_v7().simple().to_string(),
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.server.base_url)
    }

    async fn get(&self, path: &str) -> Response {
        timeout(
            WAIT,
            self.client
                .get(self.url(path))
                .header("x-request-id", format!("mw-{}", Uuid::now_v7()))
                .send(),
        )
        .await
        .expect("GET timed out")
        .expect("GET response")
    }

    async fn post_raw(&self, path: &str, body: Vec<u8>) -> Response {
        timeout(
            WAIT,
            self.client
                .post(self.url(path))
                .header(header::CONTENT_TYPE, "application/json")
                .header("x-request-id", format!("mw-{}", Uuid::now_v7()))
                .body(body)
                .send(),
        )
        .await
        .expect("POST timed out")
        .expect("POST response")
    }

    async fn shutdown(self) {
        self.server.shutdown().await;
        self.pool.close().await;
    }
}

/// Asserts the full `ErrorResponse` envelope contract from `plans/CONVENTIONS.md` §4:
/// a code, a catalogued key, a non-empty English message, and a populated request id.
async fn assert_error_envelope(response: Response, status: StatusCode, code: &str) -> Value {
    assert_eq!(response.status(), status);
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(
        content_type.starts_with("application/json"),
        "infrastructure errors must be JSON envelopes, not bare text: {content_type}"
    );
    let body: Value = response.json().await.expect("JSON error envelope");
    let error = &body["error"];
    assert_eq!(error["code"], code, "body: {body}");
    let message_key = error["message_key"].as_str().expect("message_key");
    assert_eq!(message_key, format!("moira.error.{code}"));
    assert!(
        moira::i18n::is_known_key(message_key),
        "{message_key} is not in the i18n catalog"
    );
    assert!(
        !error["message"].as_str().expect("message").is_empty(),
        "message must carry the catalogued English default: {body}"
    );
    assert!(
        !error["request_id"].as_str().expect("request_id").is_empty(),
        "request_id must be populated: {body}"
    );
    body
}

// ---------------------------------------------------------------------------
// Body limits (P1-3: align the configured policy with the enforced layer)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn oversized_public_body_is_rejected_with_413_and_the_standard_envelope() {
    let Some(fixture) = MiddlewareFixture::new().await else {
        return;
    };
    let response = fixture
        .post_raw("/api/v1/responses", vec![b'a'; fixture.public_limit + 1])
        .await;

    // Before this plan the caller got Axum's bare `text/plain` 413 with no code, no
    // message_key and no request id.
    assert_error_envelope(response, StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large").await;

    fixture.shutdown().await;
}

#[tokio::test]
async fn public_body_at_the_configured_maximum_request_bytes_boundary_is_accepted() {
    let Some(fixture) = MiddlewareFixture::new().await else {
        return;
    };
    // The old global layer capped every route at 512 KiB while the documented policy said
    // 1 MiB. A body of exactly `PublicApiSettings.maximum_request_bytes` proves the
    // configured value is the enforced one. The payload is not valid JSON, so the request
    // is rejected later by the parser — the point is that the body-limit layer let it in.
    assert!(
        fixture.public_limit > 512 * 1024,
        "guarding against a weakened default"
    );
    let response = fixture
        .post_raw("/api/v1/responses", vec![b'a'; fixture.public_limit])
        .await;

    assert_ne!(
        response.status(),
        StatusCode::PAYLOAD_TOO_LARGE,
        "a body at exactly the configured limit must not be rejected by the limit layer"
    );

    fixture.shutdown().await;
}

#[tokio::test]
async fn admin_routes_enforce_their_own_distinct_body_limit() {
    let Some(fixture) = MiddlewareFixture::new().await else {
        return;
    };
    assert!(
        fixture.public_limit < ADMIN_BODY_LIMIT_BYTES,
        "the admin cap must sit above the public one for this test to mean anything"
    );

    // Above the public limit, below the admin limit: admitted, proving the limits are
    // genuinely per-route rather than one global layer.
    let admitted = fixture
        .post_raw(
            "/api/v1/admin/applications",
            vec![b'a'; fixture.public_limit + 1],
        )
        .await;
    assert_ne!(admitted.status(), StatusCode::PAYLOAD_TOO_LARGE);

    // Above the admin limit: the same envelope as the public route.
    let rejected = fixture
        .post_raw(
            "/api/v1/admin/applications",
            vec![b'a'; ADMIN_BODY_LIMIT_BYTES + 1],
        )
        .await;
    assert_error_envelope(rejected, StatusCode::PAYLOAD_TOO_LARGE, "payload_too_large").await;

    fixture.shutdown().await;
}

// ---------------------------------------------------------------------------
// Panic isolation and request timeout (probe routes)
// ---------------------------------------------------------------------------

/// Proves the probe routes are genuinely feature-gated in *both* directions, which is the
/// Definition-of-Done box "every test route is genuinely cfg-gated and cannot appear in a
/// release build". Runs in every configuration.
#[tokio::test]
async fn probe_routes_exist_only_under_the_test_routes_feature() {
    let Some(fixture) = MiddlewareFixture::new().await else {
        return;
    };
    let status = fixture.get("/internal/test/slow").await.status();

    #[cfg(feature = "test-routes")]
    assert_ne!(
        status,
        StatusCode::NOT_FOUND,
        "the probe routes must be routable when `test-routes` is on"
    );
    #[cfg(not(feature = "test-routes"))]
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "the probe routes must not exist without the `test-routes` feature — a release \
         build must not be able to reach them"
    );

    fixture.shutdown().await;
}

#[cfg(feature = "test-routes")]
#[tokio::test]
async fn panicking_handler_returns_500_envelope_without_panic_payload() {
    let Some(fixture) = MiddlewareFixture::new().await else {
        return;
    };
    // Without `CatchPanicLayer` this is not a 500 at all: the connection is dropped and
    // `send()` fails with a transport error. Getting a parseable envelope back over a real
    // socket is the whole assertion.
    let response = fixture.get("/internal/test/panic").await;
    let body = assert_error_envelope(
        response,
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal_error",
    )
    .await;

    // The probe panics with "moira probe panic: credential row 42 pepper v1 unwrap on
    // None". None of it may reach the caller: panic payloads routinely carry internal
    // state. Scoped to the caller-facing text fields — `request_id` is a random UUID and
    // will contain arbitrary hex digits.
    let rendered = format!("{}{}", body["error"]["message"], body["error"]["details"]);
    for fragment in [
        "credential",
        "pepper",
        "unwrap",
        "probe panic",
        "row 42",
        "None",
    ] {
        assert!(
            !rendered.contains(fragment),
            "panic payload fragment {fragment:?} leaked into the response: {body}"
        );
    }
    assert_eq!(
        body["error"]["details"],
        Value::Null,
        "a caught panic must not attach details: {body}"
    );

    fixture.shutdown().await;
}

#[cfg(feature = "test-routes")]
#[tokio::test]
async fn slow_non_streaming_request_returns_504_with_the_request_timeout_key() {
    let Some(fixture) = MiddlewareFixture::new().await else {
        return;
    };
    // The probe handler is `std::future::pending()`, so the deadline always wins and the
    // test is deterministic — no `sleep()`-based interleaving. `tower_http`'s
    // `TimeoutLayer` emits a header-less, body-less 504; the assertion is that Moira's
    // envelope mapper turns it into the standard `ErrorResponse` over the wire.
    let response = fixture.get("/internal/test/slow").await;
    assert_error_envelope(response, StatusCode::GATEWAY_TIMEOUT, "request_timeout").await;

    fixture.shutdown().await;
}

// ---------------------------------------------------------------------------
// Headers
// ---------------------------------------------------------------------------

#[tokio::test]
async fn security_headers_are_present_on_a_live_response() {
    let Some(fixture) = MiddlewareFixture::new().await else {
        return;
    };
    // The e2e counterpart to the `oneshot` unit test: layer ordering can strip headers in
    // ways an in-process call does not reveal, and the header set must survive on an error
    // response too, not only on a success.
    for path in ["/health/live", "/api/v1/admin/applications/not-a-uuid"] {
        let response = fixture.get(path).await;
        let headers = response.headers().clone();
        assert_eq!(headers[header::X_FRAME_OPTIONS], "DENY", "{path}");
        assert_eq!(
            headers[header::CONTENT_SECURITY_POLICY],
            "default-src 'none'",
            "{path}"
        );
        assert_eq!(headers[header::CACHE_CONTROL], "no-store", "{path}");
        assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff", "{path}");
        assert_eq!(headers[header::REFERRER_POLICY], "no-referrer", "{path}");
        // The fixture runs a non-production deployment, so HSTS must be absent: pinning
        // `http://localhost` to HTTPS breaks local tooling.
        assert!(
            !headers.contains_key(header::STRICT_TRANSPORT_SECURITY),
            "HSTS must not be sent outside a Production deployment ({path})"
        );
    }

    fixture.shutdown().await;
}

#[tokio::test]
async fn hsts_is_present_only_under_a_production_deployment() {
    let Some(fixture) = MiddlewareFixture::new().await else {
        return;
    };
    let mut settings = Settings::default();
    settings.deployment.environment = moira::config::DeploymentEnvironment::Production;
    let production = MoiraHttpServer::start(
        AppState::new(settings, Some(fixture.pool.clone())).expect("production app state"),
    )
    .await;

    let response = timeout(
        WAIT,
        fixture
            .client
            .get(format!("{}/health/live", production.base_url))
            .send(),
    )
    .await
    .expect("production GET timed out")
    .expect("production response");

    assert_eq!(
        response.headers()[header::STRICT_TRANSPORT_SECURITY],
        "max-age=63072000; includeSubDomains"
    );

    production.shutdown().await;
    fixture.shutdown().await;
}

#[tokio::test]
async fn once_only_secret_responses_carry_no_content_encoding() {
    let Some(fixture) = MiddlewareFixture::new().await else {
        return;
    };
    // Forward-looking regression guard for the rule documented on `build_router`: Moira
    // installs no `CompressionLayer` today, and these four routes must never gain one —
    // compressing a response that carries a once-only secret opens a BREACH-style side
    // channel.
    for path in ONCE_ONLY_SECRET_ROUTES {
        let response = timeout(
            WAIT,
            fixture
                .client
                .post(fixture.url(path))
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::ACCEPT_ENCODING, "gzip, deflate, br, zstd")
                .body("{}")
                .send(),
        )
        .await
        .expect("secret route timed out")
        .expect("secret route response");
        assert!(
            !response.headers().contains_key(header::CONTENT_ENCODING),
            "{path} returned a Content-Encoding header"
        );
    }

    // And on a response that genuinely carries a secret, not only on a rejected one.
    let created = timeout(
        WAIT,
        fixture
            .client
            .post(fixture.url("/api/v1/admin/system-keys"))
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT_ENCODING, "gzip, deflate, br, zstd")
            .body(
                json!({
                    "display_name": format!("middleware-contract {}", fixture.suffix),
                    "scopes": ["moira:admin"],
                    "expires_at": null,
                })
                .to_string(),
            )
            .send(),
    )
    .await
    .expect("system key creation timed out")
    .expect("system key response");

    assert_eq!(created.status(), StatusCode::CREATED);
    assert!(!created.headers().contains_key(header::CONTENT_ENCODING));
    let body: Value = created.json().await.expect("api key secret envelope");
    assert!(
        body["secret"].as_str().is_some_and(|s| !s.is_empty()),
        "this must be a real once-only-secret response: {body}"
    );

    fixture.shutdown().await;
}

// ---------------------------------------------------------------------------
// i18n contract over the live wire
// ---------------------------------------------------------------------------

#[tokio::test]
async fn middleware_error_responses_carry_non_empty_message_key_and_message() {
    let Some(fixture) = MiddlewareFixture::new().await else {
        return;
    };
    // `plans/CONVENTIONS.md` §4 rule 5, asserted on each infrastructure-produced status
    // this plan introduces or re-shapes. `assert_error_envelope` checks `is_known_key`,
    // a non-empty message and a populated request id for every one of them.
    assert_error_envelope(
        fixture
            .post_raw("/api/v1/responses", vec![b'a'; fixture.public_limit + 1])
            .await,
        StatusCode::PAYLOAD_TOO_LARGE,
        "payload_too_large",
    )
    .await;

    #[cfg(feature = "test-routes")]
    {
        assert_error_envelope(
            fixture.get("/internal/test/slow").await,
            StatusCode::GATEWAY_TIMEOUT,
            "request_timeout",
        )
        .await;
        assert_error_envelope(
            fixture.get("/internal/test/panic").await,
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
        )
        .await;
    }

    fixture.shutdown().await;
}

// ---------------------------------------------------------------------------
// SSE exemption — the highest-risk compatibility check in this plan
// ---------------------------------------------------------------------------

struct RawServer {
    base_url: String,
    shutdown: CancellationToken,
    task: JoinHandle<()>,
}

/// Serves a caller-supplied router, so the test can choose a `RouterPolicy`.
/// `moira::http::router` and `RouterPolicy` are both public API; only the timeout
/// *value* is test-chosen — the layer placement (and the SSE group's exemption from it)
/// is production code, unmodified.
async fn serve(router: axum::Router) -> RawServer {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("address");
    let shutdown = CancellationToken::new();
    let task_shutdown = shutdown.clone();
    let task = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(task_shutdown.cancelled_owned())
            .await
            .expect("serve");
    });
    RawServer {
        base_url: format!("http://{address}"),
        shutdown,
        task,
    }
}

impl RawServer {
    async fn shutdown(self) {
        self.shutdown.cancel();
        let _ = timeout(WAIT, self.task).await;
    }
}

/// `/api/v1/responses/stream` must outlive the timeout that governs non-streaming routes.
///
/// The ordering gate is a *completed* 504 on the same router: when that response comes
/// back, the non-streaming ceiling has demonstrably elapsed in real time while the SSE
/// connection was open. Only then is the stream released, and it must still complete
/// normally. No `sleep()` is involved on either side.
///
/// Why the timeout is set here rather than taken from settings: the production value is
/// `maximum_execution_timeout_seconds + 30`, and it is deliberately sized to sit *above*
/// every execution deadline — so no execution-bearing route can ever reach it, and no
/// stream (which is itself bounded by that deadline) can ever outlive it. The exemption is
/// only observable at a timeout below the execution deadline. That the production value
/// sits above the deadline is asserted separately by
/// `the_non_streaming_timeout_sits_above_the_execution_deadline` (`src/lib.rs`).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sse_stream_outlives_the_non_streaming_timeout() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let stream_gate = ScriptGate::new();
    let completion_gate = ScriptGate::new();
    let provider = MockOpenAiServer::start([
        ProviderScript::HeldStream {
            first_delta: "sse-first".to_string(),
            remaining_deltas: vec!["sse-last".to_string()],
            gate: stream_gate.clone(),
        },
        ProviderScript::HeldCompletion {
            text: "never delivered".to_string(),
            gate: completion_gate.clone(),
        },
    ])
    .await;
    fixture
        .add_provider(
            provider.base_url(),
            10,
            RuntimePolicy {
                request_timeout_ms: 9_000,
                stream_idle_timeout_ms: 9_000,
                ..RuntimePolicy::default()
            },
        )
        .await;
    let consumer_key = fixture.enable_public_streaming().await;

    const NON_STREAMING_CEILING: Duration = Duration::from_millis(400);
    let policy = RouterPolicy {
        non_streaming_timeout: NON_STREAMING_CEILING,
        ..RouterPolicy::from_settings(&fixture.state.settings)
    };
    let server = serve(moira::http::router(policy).with_state(fixture.state.clone())).await;
    let client = Client::new();

    // 1. Open the stream and wait until the provider acknowledges it is parked mid-stream.
    let stream_response = timeout(
        WAIT,
        client
            .post(format!("{}/api/v1/responses/stream", server.base_url))
            .header("x-consumer-key", &consumer_key)
            .header("x-request-id", format!("sse-{}", Uuid::now_v7()))
            .json(&public_response_request(&fixture.route_key))
            .send(),
    )
    .await
    .expect("SSE open timed out")
    .expect("SSE response");
    assert_eq!(stream_response.status(), StatusCode::OK);
    assert!(
        stream_response.headers()[header::CONTENT_TYPE]
            .to_str()
            .unwrap_or_default()
            .starts_with("text/event-stream")
    );
    stream_gate.wait_arrived().await;

    // 2. With the stream still open, drive a non-streaming request into the timeout. Its
    //    504 is the acknowledgement that the ceiling has elapsed.
    let timed_out = timeout(
        WAIT,
        client
            .post(format!("{}/api/v1/responses", server.base_url))
            .header("x-consumer-key", &consumer_key)
            .header("x-request-id", format!("slow-{}", Uuid::now_v7()))
            .json(&public_response_request(&fixture.route_key))
            .send(),
    )
    .await
    .expect("non-streaming request timed out at the test level")
    .expect("non-streaming response");
    assert_eq!(
        timed_out.status(),
        StatusCode::GATEWAY_TIMEOUT,
        "the non-streaming group must be governed by the timeout layer"
    );
    completion_gate.release();

    // 3. Release the stream. It has now been open strictly longer than the non-streaming
    //    ceiling and must still complete normally.
    stream_gate.release();
    let body = timeout(WAIT, async {
        let mut buffer = String::new();
        let mut chunks = stream_response.bytes_stream();
        while let Some(chunk) = chunks.next().await {
            buffer.push_str(&String::from_utf8_lossy(&chunk.expect("SSE chunk")));
            if buffer.contains("response.completed") {
                break;
            }
        }
        buffer
    })
    .await
    .expect("the SSE stream was cut off after the non-streaming timeout elapsed");

    assert!(
        body.contains("sse-first") && body.contains("sse-last"),
        "the stream must deliver every delta emitted after the ceiling elapsed: {body}"
    );
    assert!(
        body.contains("response.completed"),
        "the stream must terminate normally, not be aborted: {body}"
    );

    server.shutdown().await;
    provider.shutdown().await;
}
