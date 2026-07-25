//! Plan 03 / finding P1-2, Verification layer 2 — SSRF-hardened JWKS fetching.
//!
//! Drives the real HTTP surface (`Authorization: Bearer …` against an admin route)
//! against a real PostgreSQL, so every assertion covers the whole path:
//! `authenticate_admin` → `authenticate_trusted_jwt` → `load_issuer` (a real
//! `trusted_jwt_issuers` row) → `JwksCache::load` → `fetch_jwks_hardened` → the
//! `audit_logs` write.
//!
//! **How a rejection is observed.** Every JWKS failure deliberately surfaces to the
//! caller as the *generic* `401 unauthorized` — telling the caller "your IdP's JWKS
//! response was 1.2 MB" or "that hostname resolved to 10.0.0.5" would turn the auth path
//! into an SSRF oracle. So the caller-visible response cannot distinguish the reasons,
//! and these tests read the server-side `audit_logs` row instead (action `jwks_fetch`,
//! result `denied`, `metadata->>'reason'`). That is also the DoD's "rejects, with an
//! audit-log entry, every one of …" evidence.
//!
//! The upstream IdP is a bare `tokio::net::TcpListener` speaking just enough HTTP/1.1,
//! following the pattern already used in `tests/support/mod.rs` — no new mock-HTTP
//! dev-dependency, and no `wiremock`.
//!
//! Concurrency discipline (`plans/CONVENTIONS.md` §3): no `sleep()` anywhere. The
//! singleflight test gates on a `Semaphore` acknowledgement from the stub plus a
//! `tokio::sync::Barrier` fan-out; the timeout test's stub is released by the test, never
//! by elapsed time.
//!
//! Cross-test isolation: issuer names, JWKS paths and application slugs all carry a
//! per-fixture `Uuid::now_v7()` suffix, and every audit assertion is scoped to this
//! fixture's own `jwks_url`, so two test binaries can run concurrently against the same
//! database without contending.

mod support;

use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use moira::{app::AppState, config::Settings, infra::db};
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};
use sqlx::{PgPool, postgres::PgPoolOptions};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::{Barrier, Notify, Semaphore},
    time::timeout,
};
use uuid::Uuid;

use support::MoiraHttpServer;

const WAIT: Duration = Duration::from_secs(20);
const JWKS_TIMEOUT_MS: u64 = 400;
const JWKS_MAX_BYTES: usize = 4096;
const EMPTY_JWKS: &str = r#"{"keys":[]}"#;

// ---------------------------------------------------------------------------
// Upstream IdP stub
// ---------------------------------------------------------------------------

struct StubPlan {
    status_line: &'static str,
    content_type: &'static str,
    body: String,
    /// Stream the body chunked with no `Content-Length`, so the size cap can only be
    /// enforced by the streaming counter — the case a header check cannot catch.
    chunked: bool,
    /// Held before the *first* response is written. The acknowledgement gate the
    /// singleflight and timeout tests use instead of a `sleep`.
    hold: Option<Arc<Notify>>,
    /// Answer every request after the first with a 500 — an IdP that worked once and
    /// then broke.
    fail_after_first: bool,
}

impl StubPlan {
    fn json(body: &str) -> Self {
        Self {
            status_line: "HTTP/1.1 200 OK",
            content_type: "application/json",
            body: body.to_string(),
            chunked: false,
            hold: None,
            fail_after_first: false,
        }
    }
}

struct Stub {
    url: String,
    hits: Arc<AtomicUsize>,
    /// Incremented when a body write failed part-way, i.e. the client hung up mid-body.
    aborted_writes: Arc<AtomicUsize>,
    /// One permit per accepted connection; never missed, unlike `Notify::notify_waiters`.
    arrived: Arc<Semaphore>,
}

impl Stub {
    async fn wait_for_first_request(&self) {
        timeout(WAIT, self.arrived.acquire())
            .await
            .expect("the JWKS stub never received a request")
            .expect("stub arrival semaphore closed")
            .forget();
    }

    fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }
}

async fn spawn_stub(path: &str, plan: StubPlan) -> Stub {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind the JWKS stub");
    let address = listener.local_addr().expect("stub address");
    let hits = Arc::new(AtomicUsize::new(0));
    let aborted_writes = Arc::new(AtomicUsize::new(0));
    let arrived = Arc::new(Semaphore::new(0));
    let (counter, aborted, signal) = (hits.clone(), aborted_writes.clone(), arrived.clone());

    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let seen = counter.fetch_add(1, Ordering::SeqCst);
            signal.add_permits(1);

            if seen == 0
                && let Some(hold) = plan.hold.as_ref()
            {
                hold.notified().await;
            }

            // Drain the request head so the client's write completes.
            let mut scratch = [0_u8; 2048];
            let _ = socket.read(&mut scratch).await;

            if plan.fail_after_first && seen > 0 {
                let _ = socket
                    .write_all(
                        b"HTTP/1.1 500 Internal Server Error\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                    )
                    .await;
                let _ = socket.flush().await;
                continue;
            }

            if plan.chunked {
                let head = format!(
                    "{}\r\nContent-Type: {}\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
                    plan.status_line, plan.content_type
                );
                if socket.write_all(head.as_bytes()).await.is_err() {
                    aborted.fetch_add(1, Ordering::SeqCst);
                    continue;
                }
                let mut wrote_everything = true;
                for slice in plan.body.as_bytes().chunks(8192) {
                    let framed = format!(
                        "{:x}\r\n{}\r\n",
                        slice.len(),
                        String::from_utf8_lossy(slice)
                    );
                    if socket.write_all(framed.as_bytes()).await.is_err()
                        || socket.flush().await.is_err()
                    {
                        wrote_everything = false;
                        break;
                    }
                }
                if wrote_everything {
                    let _ = socket.write_all(b"0\r\n\r\n").await;
                    let _ = socket.flush().await;
                } else {
                    aborted.fetch_add(1, Ordering::SeqCst);
                }
                continue;
            }

            let response = format!(
                "{}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                plan.status_line,
                plan.content_type,
                plan.body.len(),
                plan.body
            );
            let _ = socket.write_all(response.as_bytes()).await;
            let _ = socket.flush().await;
        }
    });

    Stub {
        url: format!("http://{address}{path}"),
        hits,
        aborted_writes,
        arrived,
    }
}

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

struct JwksFixture {
    pool: PgPool,
    server: MoiraHttpServer,
    client: Client,
    suffix: String,
}

impl JwksFixture {
    /// `dev_override == false` is the production posture (https-only, denied ranges
    /// refused). `true` is the documented dev escape hatch, and the only way a loopback
    /// stub IdP is reachable at all — which is itself the proof that the hardening, not
    /// the transport, is what refuses loopback in the other tests.
    async fn new(dev_override: bool) -> Option<Self> {
        let database_url = match std::env::var("MOIRA_TEST_DATABASE_URL") {
            Ok(value) if !value.trim().is_empty() => value,
            _ if std::env::var("CI").is_ok_and(|value| value.eq_ignore_ascii_case("true")) => {
                panic!("MOIRA_TEST_DATABASE_URL is required when CI=true for JWKS hardening tests")
            }
            _ => {
                eprintln!("skipping JWKS hardening tests: MOIRA_TEST_DATABASE_URL is not set");
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
        .expect("connect JWKS test database");
        timeout(WAIT, db::migrate(&pool))
            .await
            .expect("migrations timed out")
            .expect("run migrations");

        let mut settings = Settings::default();
        settings.auth.jwks.allow_insecure_dev_urls = dev_override;
        settings.auth.jwks.timeout_ms = JWKS_TIMEOUT_MS;
        settings.auth.jwks.max_response_bytes = JWKS_MAX_BYTES;
        let state = AppState::new(settings, Some(pool.clone())).expect("JWKS test app state");
        let server = MoiraHttpServer::start(state).await;

        Some(Self {
            pool,
            server,
            client: Client::new(),
            suffix: Uuid::now_v7().simple().to_string(),
        })
    }

    fn issuer(&self, label: &str) -> String {
        format!("https://idp.invalid/{label}-{}", self.suffix)
    }

    fn jwks_path(&self, label: &str) -> String {
        format!("/{label}-{}/jwks.json", self.suffix)
    }

    async fn register_issuer(&self, issuer: &str, jwks_url: &str) {
        timeout(
            WAIT,
            sqlx::query(
                "insert into trusted_jwt_issuers
                    (id, issuer, jwks_url, allowed_algorithms, status)
                 values ($1, $2, $3, array['RS256'], 'active')",
            )
            .bind(Uuid::now_v7())
            .bind(issuer)
            .bind(jwks_url)
            .execute(&self.pool),
        )
        .await
        .expect("issuer insert timed out")
        .expect("insert trusted jwt issuer");
    }

    /// An unsigned, structurally valid JWT. The signature is never reached: the JWKS
    /// fetch happens first, which is exactly the code path under test.
    fn token(&self, issuer: &str) -> String {
        let header = URL_SAFE_NO_PAD
            .encode(json!({ "alg": "RS256", "typ": "JWT", "kid": "probe-key" }).to_string());
        let payload = URL_SAFE_NO_PAD
            .encode(json!({ "iss": issuer, "sub": format!("probe-{}", self.suffix) }).to_string());
        format!(
            "{header}.{payload}.{}",
            URL_SAFE_NO_PAD.encode(b"not-a-real-signature")
        )
    }

    async fn authenticate(&self, issuer: &str) -> (StatusCode, Value) {
        let response = timeout(
            WAIT,
            self.client
                .get(format!(
                    "{}/api/v1/admin/applications",
                    self.server.base_url
                ))
                .header("authorization", format!("Bearer {}", self.token(issuer)))
                .header("x-request-id", format!("jwks-{}", Uuid::now_v7()))
                .send(),
        )
        .await
        .expect("auth request timed out")
        .expect("auth response");
        let status = response.status();
        let bytes = response.bytes().await.expect("auth body");
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes)
                .unwrap_or_else(|_| panic!("non-JSON body: {}", String::from_utf8_lossy(&bytes)))
        };
        (status, body)
    }

    /// Denial reasons recorded for this fixture's JWKS URL, newest last.
    async fn denial_reasons(&self, jwks_url: &str) -> Vec<String> {
        timeout(
            WAIT,
            sqlx::query_scalar::<_, String>(
                "select metadata->>'reason'
                 from audit_logs
                 where action = 'jwks_fetch'
                   and result = 'denied'
                   and metadata->>'jwks_url' = $1
                 order by occurred_at",
            )
            .bind(jwks_url)
            .fetch_all(&self.pool),
        )
        .await
        .expect("audit query timed out")
        .expect("audit query")
    }

    async fn expect_denial(&self, jwks_url: &str, reason: &str) {
        let reasons = self.denial_reasons(jwks_url).await;
        assert!(
            reasons.iter().any(|recorded| recorded == reason),
            "expected an audited `{reason}` denial for {jwks_url}, recorded: {reasons:?}"
        );
    }

    async fn post_issuer(&self, jwks_url: &str, label: &str) -> (StatusCode, Value) {
        let response = timeout(
            WAIT,
            self.client
                .post(format!("{}/api/v1/admin/jwt-issuers", self.server.base_url))
                .header("content-type", "application/json")
                .header("x-request-id", format!("jwks-reg-{}", Uuid::now_v7()))
                .body(
                    json!({
                        "issuer": self.issuer(label),
                        "jwks_url": jwks_url,
                        "allowed_algorithms": ["RS256"],
                    })
                    .to_string(),
                )
                .send(),
        )
        .await
        .expect("issuer registration timed out")
        .expect("issuer registration response");
        let status = response.status();
        let body: Value = response.json().await.unwrap_or(Value::Null);
        (status, body)
    }

    async fn shutdown(self) {
        self.server.shutdown().await;
    }
}

fn assert_generic_unauthorized(status: StatusCode, body: &Value) {
    assert_eq!(status, StatusCode::UNAUTHORIZED, "body: {body}");
    let error = &body["error"];
    assert_eq!(
        error["code"], "unauthorized",
        "a JWKS failure must surface as the generic unauthorized error, not a specific \
         one: {body}"
    );
    assert_eq!(error["message_key"], "moira.error.unauthorized");
    assert!(moira::i18n::is_known_key(
        error["message_key"].as_str().expect("message_key")
    ));
    assert!(!error["message"].as_str().expect("message").is_empty());
    assert!(
        !error["request_id"].as_str().expect("request_id").is_empty(),
        "the envelope must carry a request id: {body}"
    );
}

// ---------------------------------------------------------------------------
// Registration-time rejection
// ---------------------------------------------------------------------------

#[tokio::test]
async fn jwks_url_with_http_scheme_is_rejected_at_issuer_registration() {
    let Some(fixture) = JwksFixture::new(false).await else {
        return;
    };
    let (status, body) = fixture
        .post_issuer("http://idp.example.com/jwks.json", "http-scheme")
        .await;

    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");
    assert_eq!(
        body["error"]["code"], "jwks_url_rejected",
        "an admin's own bad input must carry the catalogued registration code: {body}"
    );
    assert_eq!(
        body["error"]["message_key"],
        "moira.error.jwks_url_rejected"
    );
    assert!(moira::i18n::is_known_key(
        body["error"]["message_key"].as_str().expect("message_key")
    ));
    assert!(
        !body["error"]["message"]
            .as_str()
            .expect("message")
            .is_empty()
    );

    fixture.shutdown().await;
}

#[tokio::test]
async fn jwks_url_resolving_to_a_private_address_is_rejected_at_issuer_registration() {
    let Some(fixture) = JwksFixture::new(false).await else {
        return;
    };
    // `https://`, so a scheme-only check passes it. Only an address-range check on the
    // resolved IP refuses it — which is the whole point of P1-2.
    let (status, body) = fixture
        .post_issuer("https://169.254.169.254/latest/meta-data/", "metadata")
        .await;

    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "the cloud metadata endpoint must not be registrable: {body}"
    );
    assert_eq!(body["error"]["code"], "jwks_url_rejected", "body: {body}");
    assert_eq!(
        body["error"]["message_key"],
        "moira.error.jwks_url_rejected"
    );

    let stored = sqlx::query_scalar::<_, i64>(
        "select count(*) from trusted_jwt_issuers where jwks_url = $1",
    )
    .bind("https://169.254.169.254/latest/meta-data/")
    .fetch_one(&fixture.pool)
    .await
    .expect("issuer count");
    assert_eq!(stored, 0, "a rejected URL must not reach the table");

    fixture.shutdown().await;
}

// ---------------------------------------------------------------------------
// Fetch-time rejection — one test per denial reason
// ---------------------------------------------------------------------------

#[tokio::test]
async fn non_https_jwks_url_is_rejected_at_verification_time() {
    let Some(fixture) = JwksFixture::new(false).await else {
        return;
    };
    let stub = spawn_stub(&fixture.jwks_path("scheme"), StubPlan::json(EMPTY_JWKS)).await;
    let issuer = fixture.issuer("scheme");
    fixture.register_issuer(&issuer, &stub.url).await;

    let (status, body) = fixture.authenticate(&issuer).await;
    assert_generic_unauthorized(status, &body);
    fixture.expect_denial(&stub.url, "scheme").await;
    assert_eq!(
        stub.hits(),
        0,
        "an http:// URL must be refused before any connection is made"
    );

    fixture.shutdown().await;
}

#[tokio::test]
async fn jwks_url_resolving_to_a_denied_address_is_rejected_at_verification_time() {
    let Some(fixture) = JwksFixture::new(false).await else {
        return;
    };
    let issuer = fixture.issuer("ip-range");
    let jwks_url = "https://127.0.0.1/jwks.json";
    fixture.register_issuer(&issuer, jwks_url).await;

    let (status, body) = fixture.authenticate(&issuer).await;
    assert_generic_unauthorized(status, &body);
    fixture.expect_denial(jwks_url, "ip_range").await;

    fixture.shutdown().await;
}

#[tokio::test]
async fn oversized_jwks_response_is_abandoned_before_full_buffering() {
    let Some(fixture) = JwksFixture::new(true).await else {
        return;
    };
    // Chunked and far over the cap, with no `Content-Length` to inspect: only a running
    // counter over the streamed body can catch this, which is the difference between a
    // real cap and a decorative one. The body is sized well beyond any plausible socket
    // buffer so that "the client hung up" shows up as a failed write on the stub rather
    // than being absorbed silently by the kernel.
    let stub = spawn_stub(
        &fixture.jwks_path("oversized"),
        StubPlan {
            chunked: true,
            ..StubPlan::json(&"x".repeat(JWKS_MAX_BYTES * 1024))
        },
    )
    .await;
    let issuer = fixture.issuer("oversized");
    fixture.register_issuer(&issuer, &stub.url).await;

    let (status, body) = fixture.authenticate(&issuer).await;
    assert_generic_unauthorized(status, &body);
    fixture.expect_denial(&stub.url, "size").await;
    assert_eq!(stub.hits(), 1);
    assert_eq!(
        stub.aborted_writes.load(Ordering::SeqCst),
        1,
        "the stub must observe the connection closed before it finished writing — proof \
         the cap is enforced while streaming, not after buffering the whole body"
    );

    fixture.shutdown().await;
}

#[tokio::test]
async fn non_json_jwks_content_type_is_rejected() {
    let Some(fixture) = JwksFixture::new(true).await else {
        return;
    };
    let stub = spawn_stub(
        &fixture.jwks_path("content-type"),
        StubPlan {
            content_type: "text/html",
            body: "<html>internal service</html>".to_string(),
            ..StubPlan::json(EMPTY_JWKS)
        },
    )
    .await;
    let issuer = fixture.issuer("content-type");
    fixture.register_issuer(&issuer, &stub.url).await;

    let (status, body) = fixture.authenticate(&issuer).await;
    assert_generic_unauthorized(status, &body);
    fixture.expect_denial(&stub.url, "content_type").await;

    fixture.shutdown().await;
}

/// The other half of the content-type rule: refusing the registered
/// `application/jwk-set+json` media type would break real IdPs, so it must be accepted.
#[tokio::test]
async fn the_registered_jwk_set_content_type_is_accepted() {
    let Some(fixture) = JwksFixture::new(true).await else {
        return;
    };
    let stub = spawn_stub(
        &fixture.jwks_path("jwk-set"),
        StubPlan {
            content_type: "application/jwk-set+json; charset=utf-8",
            ..StubPlan::json(EMPTY_JWKS)
        },
    )
    .await;
    let issuer = fixture.issuer("jwk-set");
    fixture.register_issuer(&issuer, &stub.url).await;

    // The token is unsigned, so authentication still ends in 401 — but for "no matching
    // JWKS key", which is *after* a successful fetch. The absence of any audited denial
    // is what proves the fetch itself succeeded.
    let (status, body) = fixture.authenticate(&issuer).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "body: {body}");
    assert_eq!(
        fixture.denial_reasons(&stub.url).await,
        Vec::<String>::new(),
        "application/jwk-set+json must not be denied"
    );
    assert_eq!(stub.hits(), 1);

    fixture.shutdown().await;
}

#[tokio::test]
async fn slow_jwks_response_is_abandoned_at_the_configured_timeout() {
    let Some(fixture) = JwksFixture::new(true).await else {
        return;
    };
    // The stub holds the first response until the test releases it, and the test only
    // releases it *after* the fetch has already been abandoned. The deadline ends the
    // fetch; no `sleep()` is involved on either side.
    let hold = Arc::new(Notify::new());
    let stub = spawn_stub(
        &fixture.jwks_path("slow"),
        StubPlan {
            hold: Some(hold.clone()),
            ..StubPlan::json(EMPTY_JWKS)
        },
    )
    .await;
    let issuer = fixture.issuer("slow");
    fixture.register_issuer(&issuer, &stub.url).await;

    let (status, body) = fixture.authenticate(&issuer).await;
    assert_generic_unauthorized(status, &body);
    fixture.expect_denial(&stub.url, "timeout").await;
    hold.notify_waiters();

    fixture.shutdown().await;
}

#[tokio::test]
async fn first_ever_fetch_failure_surfaces_as_an_auth_failure() {
    let Some(fixture) = JwksFixture::new(true).await else {
        return;
    };
    let stub = spawn_stub(
        &fixture.jwks_path("first-failure"),
        StubPlan {
            status_line: "HTTP/1.1 500 Internal Server Error",
            ..StubPlan::json(EMPTY_JWKS)
        },
    )
    .await;
    let issuer = fixture.issuer("first-failure");
    fixture.register_issuer(&issuer, &stub.url).await;

    // With nothing ever cached there is no last-known-good value to fall back to, so the
    // failure must propagate. The negative half of the retention rule: never fail open.
    let (status, body) = fixture.authenticate(&issuer).await;
    assert_generic_unauthorized(status, &body);
    fixture.expect_denial(&stub.url, "status").await;

    fixture.shutdown().await;
}

/// Retention half of the availability rule, as far as it is reachable from outside the
/// library.
///
/// **Honest scope note.** `JWKS_CACHE_TTL` is a 300 s constant and `JwksCache::expire_all`
/// is `#[cfg(test)]`, so an integration test cannot force a *past-TTL* refresh; the
/// "serve the stale entry when the refresh fails" branch is proven at unit level by
/// `a_failed_refresh_serves_the_last_known_good_cached_jwks` (`src/security/auth.rs`).
/// What this test proves end to end is the user-visible consequence: once a key set has
/// been cached, an IdP that starts failing does not break authentication, and Moira stops
/// talking to it entirely.
#[tokio::test]
async fn a_failed_upstream_does_not_break_auth_while_a_cached_key_set_is_retained() {
    let Some(fixture) = JwksFixture::new(true).await else {
        return;
    };
    let stub = spawn_stub(
        &fixture.jwks_path("retention"),
        StubPlan {
            fail_after_first: true,
            ..StubPlan::json(EMPTY_JWKS)
        },
    )
    .await;
    let issuer = fixture.issuer("retention");
    fixture.register_issuer(&issuer, &stub.url).await;

    let (first_status, first_body) = fixture.authenticate(&issuer).await;
    assert_eq!(first_status, StatusCode::UNAUTHORIZED, "{first_body}");
    assert_eq!(
        fixture.denial_reasons(&stub.url).await,
        Vec::<String>::new(),
        "the warming fetch must succeed"
    );
    assert_eq!(stub.hits(), 1);

    for _ in 0..3 {
        let (status, body) = fixture.authenticate(&issuer).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    }
    assert_eq!(
        stub.hits(),
        1,
        "a cached key set must not be re-fetched, so a broken IdP is never even contacted"
    );
    assert_eq!(
        fixture.denial_reasons(&stub.url).await,
        Vec::<String>::new(),
        "no denial may be recorded while the cached key set is being served"
    );

    fixture.shutdown().await;
}

/// The SSRF-oracle guard: the rejection is fully recorded server-side and fully invisible
/// client-side.
#[tokio::test]
async fn jwks_rejection_is_audited_without_leaking_the_resolved_ip_to_the_caller() {
    let Some(fixture) = JwksFixture::new(false).await else {
        return;
    };
    let issuer = fixture.issuer("oracle");
    let jwks_url = "https://169.254.169.254/latest/meta-data/iam/security-credentials/";
    fixture.register_issuer(&issuer, jwks_url).await;

    let (status, body) = fixture.authenticate(&issuer).await;
    assert_generic_unauthorized(status, &body);

    // Server side: the reason is recorded in full.
    fixture.expect_denial(jwks_url, "ip_range").await;

    // Client side: nothing about the address or the decision.
    let rendered = body.to_string();
    for leak in ["169.254", "ip_range", "meta-data", "metadata", "denied"] {
        assert!(
            !rendered.contains(leak),
            "the response leaked {leak:?}, turning the auth path into an SSRF oracle: \
             {rendered}"
        );
    }

    fixture.shutdown().await;
}

/// Singleflight: N concurrent cache-miss authentications for one issuer collapse into a
/// single upstream call.
///
/// The stub holds its first response until the test releases it. Without singleflight,
/// every racer would find the cache empty and open its own connection while the first is
/// still parked, so the hit counter would read `RACERS`, not `1`. The gate is an
/// acknowledgement (`Semaphore` permit from the stub) plus a `Barrier` fan-out — never a
/// `sleep()`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_cache_miss_fetches_are_singleflighted_to_one_upstream_call() {
    let Some(fixture) = JwksFixture::new(true).await else {
        return;
    };
    let hold = Arc::new(Notify::new());
    let stub = spawn_stub(
        &fixture.jwks_path("singleflight"),
        StubPlan {
            hold: Some(hold.clone()),
            ..StubPlan::json(EMPTY_JWKS)
        },
    )
    .await;
    let issuer = fixture.issuer("singleflight");
    fixture.register_issuer(&issuer, &stub.url).await;

    const RACERS: usize = 5;
    let barrier = Arc::new(Barrier::new(RACERS + 1));
    let mut tasks = Vec::with_capacity(RACERS);
    for _ in 0..RACERS {
        let client = fixture.client.clone();
        let url = format!("{}/api/v1/admin/applications", fixture.server.base_url);
        let token = fixture.token(&issuer);
        let gate = barrier.clone();
        tasks.push(tokio::spawn(async move {
            gate.wait().await;
            client
                .get(url)
                .header("authorization", format!("Bearer {token}"))
                .send()
                .await
                .expect("concurrent auth response")
                .status()
        }));
    }
    barrier.wait().await;

    // Acknowledgement gate: the stub has accepted the one request the singleflight lock
    // let through. Every other racer is now parked on that lock.
    stub.wait_for_first_request().await;
    hold.notify_waiters();

    for task in tasks {
        let status = timeout(WAIT, task)
            .await
            .expect("concurrent auth timed out")
            .expect("concurrent auth task panicked");
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    assert_eq!(
        stub.hits(),
        1,
        "{RACERS} concurrent cache misses must coalesce into exactly one upstream fetch"
    );
    assert_eq!(
        fixture.denial_reasons(&stub.url).await,
        Vec::<String>::new()
    );

    fixture.shutdown().await;
}
