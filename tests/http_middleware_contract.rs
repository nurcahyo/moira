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
//! ## Which router each test drives
//!
//! Most tests here go through `MoiraHttpServer`, i.e. `moira::build_router` — the
//! production stack. Two go through `moira::http::router(policy)` directly, and only
//! because the property under test is the *timeout layer's placement*, which is
//! unobservable at the production value (`maximum_execution_timeout_seconds + 30`, never
//! below 30 s and always above every execution deadline). Those tests choose the timeout
//! value; the layer placement, and the SSE group's exemption from it, is production code
//! either way. `sse_streams_incrementally_through_the_production_middleware_stack` covers
//! the SSE contract through the full stack, because a response-rewriting middleware that
//! buffers a stream is exactly the regression `moira::http::router` alone cannot see.
//!
//! Concurrency discipline (`plans/CONVENTIONS.md` §3): no `sleep()`. Ordering is gated on
//! the mock provider's `ScriptGate` acknowledgements and on connection-pool starvation —
//! never on a duration.
//!
//! It used to say that a `Duration::ZERO` ceiling "resolves on a handler's first pend rather
//! than on wall-clock racing". That was wrong in both halves, and finding F22 is the bill:
//! `tokio::time::sleep(Duration::ZERO)` rounds its deadline up to the next whole
//! millisecond, so it is never ready on a first poll, and a warm loopback handler can beat
//! it. See `every_non_sse_route_group_is_governed_by_the_non_streaming_timeout` for the
//! measurements. **A duration is not an ordering primitive here, however small.**

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

/// Every non-SSE route **group** is genuinely governed by
/// [`RouterPolicy::non_streaming_timeout`], and the SSE group is genuinely exempt.
///
/// This is the test behind `RouterPolicy`'s doc comment ("Applied to every non-SSE route
/// group"). Nothing else asserts it: `slow_non_streaming_request_returns_504_with_the_
/// request_timeout_key` targets `/internal/test/slow`, which carries its **own** separate
/// `TimeoutLayer` (`PROBE_TIMEOUT`), so it proves the envelope mapper rather than the
/// production layer's placement. A review mutation that deleted `.layer(timeout)` from the
/// conversation and admin groups in `src/http/mod.rs` survived the entire suite.
///
/// **Why the ceiling is zero, and why zero alone is not enough (finding F22).** The
/// production value is `maximum_execution_timeout_seconds + 30`, i.e. never below 30 s and
/// deliberately above every execution deadline — unreachable inside a test. So the test
/// picks its own ceiling. It used to pick `Duration::ZERO` and stop there, on the reasoning
/// that an "already-elapsed" deadline fires on the handler's first pend. **Two halves of
/// that reasoning were false**, and together they made this assertion a sub-millisecond
/// wall-clock race that lost on CI run `30625512140` (`main`, docs-only commit, `left: 200`):
///
/// * `tower_http` 0.6's `Timeout::ResponseFuture` polls its **sleep first** and the inner
///   future second, not the other way round; and
/// * `tokio::time::sleep(Duration::ZERO)` is **not** already elapsed. `TimeSource::
///   deadline_to_tick` rounds every deadline *up* to the end of a whole millisecond, so a
///   zero-duration sleep resolves at the next millisecond boundary — measured here at 0 out
///   of 2000 first polls ready, p50 ≈ 1.3 ms. A zero ceiling is a real ~0–1 ms deadline.
///
/// A warm `GET /api/v1/admin/applications` — one `select` against a one-row table over
/// loopback — completes in p50 ≈ 0.64 ms, i.e. *inside* that window. Measured on an
/// otherwise idle machine, 6 of 60 identical probes returned `200`.
///
/// **What makes it deterministic now** is a precondition rather than a duration: the test
/// holds every connection in the fixture's pool across the probe loop. Each probed route
/// needs a pool connection to produce a response head, so no probe can complete until this
/// test releases the guards — which it does only after the assertions. The ceiling can then
/// be any value at all and `504` is the only reachable status; zero is chosen simply because
/// it is the smallest. No `sleep()`, no retry, no widened timeout: the race is removed, not
/// out-waited. Only the layer's presence is under test here; that the configured *value* is
/// `maximum_execution_timeout_seconds + 30` is asserted by
/// `the_non_streaming_timeout_sits_above_the_execution_deadline` (`src/lib.rs`).
///
/// The control arm — the same requests against the same router built with the production
/// policy, with the pool released — is what keeps this from degenerating into "everything is
/// a 504".
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_non_sse_route_group_is_governed_by_the_non_streaming_timeout() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let provider = MockOpenAiServer::start([ProviderScript::Stream {
        deltas: vec!["exempt-first".to_string(), "exempt-last".to_string()],
    }])
    .await;
    fixture
        .add_provider(provider.base_url(), 10, RuntimePolicy::default())
        .await;
    let consumer_key = fixture.enable_public_streaming().await;
    let client = Client::new();

    // One request per non-SSE route group — all four of them, including `operational`,
    // which nothing probed before F22 and whose `.layer(timeout)` was therefore free to be
    // deleted. Every one of these routes needs a PostgreSQL *connection* to produce a
    // response: `/health/ready` runs `select 1`, the admin listing queries directly, and the
    // two consumer-key routes hit the pool while authenticating. That is exactly what the
    // starvation gate below relies on, so a route added here that can answer without the
    // pool would silently reintroduce F22's race. They are all `GET`s on purpose: a request
    // body could in principle be extracted and rejected without ever reaching the pool, and
    // a `POST` that did survive would leave a side effect behind.
    let probes: [(&str, &str, bool); 4] = [
        ("operational", "/health/ready", false),
        ("admin", "/api/v1/admin/applications", false),
        ("conversation", "/api/v1/conversations", true),
        ("public execution", "/api/v1/models", true),
    ];

    let expired = RouterPolicy {
        non_streaming_timeout: Duration::ZERO,
        ..RouterPolicy::from_settings(&fixture.state.settings)
    };
    let timed_out_server =
        serve(moira::http::router(expired).with_state(fixture.state.clone())).await;

    // The gate that replaces F22's wall-clock race. With every connection held here, a
    // probed handler cannot reach PostgreSQL, so it cannot produce a response head at all
    // until this test drops the guards below — the timeout layer is the only thing that can
    // answer, whatever the ceiling's value happens to be.
    let mut held_connections = Vec::new();
    for slot in 0..fixture.pool.options().get_max_connections() {
        held_connections.push(
            timeout(WAIT, fixture.pool.acquire())
                .await
                .unwrap_or_else(|_| panic!("acquiring fixture connection {slot} timed out"))
                .unwrap_or_else(|error| panic!("acquire fixture connection {slot}: {error}")),
        );
    }
    // Asserted rather than assumed: a fixture pool that grew a spare connection would put
    // the race back without changing a line of this test.
    assert_eq!(
        fixture.pool.num_idle(),
        0,
        "the starvation gate left a connection available, so the probes below could still \
         race the ceiling"
    );

    for (group, path, needs_key) in probes {
        let mut builder = client
            .get(format!("{}{path}", timed_out_server.base_url))
            .header("x-request-id", format!("group-{}", Uuid::now_v7()));
        if needs_key {
            builder = builder.header("x-consumer-key", &consumer_key);
        }
        // This, not the `assert_eq!` below, is what a deleted `.layer(timeout)` looks like
        // now: with the pool starved the handler cannot answer at all, so an unlayered
        // group produces no response rather than a fast one. Verified by injection on the
        // admin and operational groups.
        let response = timeout(WAIT, builder.send())
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "the {group} probe never returned. With the fixture pool starved the \
                     only response this route can produce is the timeout layer's, so this \
                     is a {group} route group missing RouterPolicy::non_streaming_timeout"
                )
            })
            .unwrap_or_else(|_| panic!("{group} probe response"));
        assert_eq!(
            response.status(),
            StatusCode::GATEWAY_TIMEOUT,
            "the {group} route group is not layered with RouterPolicy::non_streaming_timeout"
        );
    }

    // Release the starvation gate: the SSE half and the control arm both have to reach
    // PostgreSQL. `PoolConnection`'s return path is asynchronous, so dropping the guards is
    // a request rather than a completion — the round trip below is the acknowledgement that
    // a connection is genuinely back in the pool, and it is why nothing here needs a sleep.
    drop(held_connections);
    timeout(WAIT, sqlx::query("select 1").execute(&fixture.pool))
        .await
        .expect("the fixture pool never recovered after the starvation gate was released")
        .expect("select 1 on the recovered fixture pool");

    // The SSE group must be exempt by construction: the same expired ceiling must not
    // touch it.
    let stream_response = timeout(
        WAIT,
        client
            .post(format!(
                "{}/api/v1/responses/stream",
                timed_out_server.base_url
            ))
            .header("x-consumer-key", &consumer_key)
            .header("x-request-id", format!("sse-exempt-{}", Uuid::now_v7()))
            .json(&public_response_request(&fixture.route_key))
            .send(),
    )
    .await
    .expect("SSE open timed out")
    .expect("SSE response");
    assert_eq!(
        stream_response.status(),
        StatusCode::OK,
        "the SSE group must not be governed by the non-streaming timeout"
    );
    assert!(
        stream_response.headers()[header::CONTENT_TYPE]
            .to_str()
            .unwrap_or_default()
            .starts_with("text/event-stream")
    );
    let streamed = read_until_completed(stream_response).await;
    assert!(
        streamed.contains("response.completed"),
        "the exempt stream must terminate normally: {streamed}"
    );

    timed_out_server.shutdown().await;

    // Control arm: with the production policy the very same requests are not 504s, so the
    // assertions above are about the timeout layer and not about the requests themselves.
    let normal_server = serve(
        moira::http::router(RouterPolicy::from_settings(&fixture.state.settings))
            .with_state(fixture.state.clone()),
    )
    .await;
    for (group, path, needs_key) in probes {
        let mut builder = client
            .get(format!("{}{path}", normal_server.base_url))
            .header("x-request-id", format!("group-ok-{}", Uuid::now_v7()));
        if needs_key {
            builder = builder.header("x-consumer-key", &consumer_key);
        }
        let response = timeout(WAIT, builder.send())
            .await
            .unwrap_or_else(|_| panic!("{group} control timed out at the test level"))
            .unwrap_or_else(|_| panic!("{group} control response"));
        assert_ne!(
            response.status(),
            StatusCode::GATEWAY_TIMEOUT,
            "the {group} control request must not 504 under the production ceiling"
        );
    }

    normal_server.shutdown().await;
    provider.shutdown().await;
}

async fn read_until_completed(response: Response) -> String {
    timeout(WAIT, async {
        let mut buffer = String::new();
        let mut chunks = response.bytes_stream();
        while let Some(chunk) = chunks.next().await {
            buffer.push_str(&String::from_utf8_lossy(&chunk.expect("SSE chunk")));
            if buffer.contains("response.completed") {
                break;
            }
        }
        buffer
    })
    .await
    .expect("the SSE stream never reached response.completed")
}

/// The SSE contract through the **production** stack: `build_router`, i.e. with
/// `CatchPanicLayer`, the infrastructure-error envelope mapper, the metrics middleware,
/// the secure-header middleware, `TraceLayer` and the request-id chain all present.
///
/// The previous version of this test used `moira::http::router` directly, so it proved
/// nothing about any of those layers — and buffering an SSE body is exactly the kind of
/// regression a response-rewriting middleware introduces. The ordering gate is the
/// provider's `ScriptGate`: the first frame must reach the client while the provider is
/// still parked, which is only possible if no layer in the stack is accumulating the body.
/// No `sleep()` on either side.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sse_streams_incrementally_through_the_production_middleware_stack() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };
    let stream_gate = ScriptGate::new();
    let provider = MockOpenAiServer::start([ProviderScript::HeldStream {
        first_delta: "sse-first".to_string(),
        remaining_deltas: vec!["sse-last".to_string()],
        gate: stream_gate.clone(),
    }])
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

    // `MoiraHttpServer::start` goes through `moira::build_router` — the production stack.
    let server = MoiraHttpServer::start(fixture.state.clone()).await;
    let client = Client::new();

    let stream_response = timeout(
        WAIT,
        client
            .post(format!("{}/api/v1/responses/stream", server.base_url))
            .header("x-consumer-key", &consumer_key)
            .header("x-request-id", format!("sse-prod-{}", Uuid::now_v7()))
            .json(&public_response_request(&fixture.route_key))
            .send(),
    )
    .await
    .expect("SSE open timed out")
    .expect("SSE response");

    assert_eq!(stream_response.status(), StatusCode::OK);
    let headers = stream_response.headers().clone();
    assert!(
        headers[header::CONTENT_TYPE]
            .to_str()
            .unwrap_or_default()
            .starts_with("text/event-stream")
    );
    assert!(
        !headers.contains_key(header::CONTENT_LENGTH),
        "a buffered response would carry a Content-Length: {headers:?}"
    );
    // The secure-header middleware must still have run on a streamed response.
    assert_eq!(headers[header::X_FRAME_OPTIONS], "DENY");
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");

    // The provider is parked after its first delta and has *not* been released. A frame
    // arriving now can only have been forwarded incrementally.
    stream_gate.wait_arrived().await;
    let mut chunks = stream_response.bytes_stream();
    let first_frame = timeout(WAIT, chunks.next())
        .await
        .expect("no SSE frame arrived while the provider was still parked")
        .expect("SSE stream ended before its first frame")
        .expect("SSE chunk");
    assert!(
        !first_frame.is_empty(),
        "the first forwarded frame must carry bytes"
    );
    let mut body = String::from_utf8_lossy(&first_frame).to_string();
    assert!(
        !stream_gate.is_completed(),
        "the provider must still be mid-stream when the first frame reaches the client"
    );

    stream_gate.release();
    while !body.contains("response.completed") {
        let chunk = timeout(WAIT, chunks.next())
            .await
            .expect("the SSE stream stalled after the provider was released")
            .expect("the SSE stream ended before response.completed")
            .expect("SSE chunk");
        body.push_str(&String::from_utf8_lossy(&chunk));
    }

    assert!(
        body.contains("sse-first") && body.contains("sse-last"),
        "the production stack must deliver every delta: {body}"
    );

    server.shutdown().await;
    provider.shutdown().await;
}
