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
//! Concurrency discipline (`plans/CONVENTIONS.md` §3): no `sleep()` anywhere. Every gate
//! is a `Semaphore` permit — `Notify` is deliberately *not* used, because
//! `Notify::notify_waiters()` drops the notification when no waiter is currently parked,
//! which is a latent lost-wakeup whenever the notifier can win the race against the
//! waiter's first poll. Permits are never lost.
//!
//! **Cross-test and cross-*run* isolation.** Every fixture owns a private database cloned
//! from the migrated template (`support::TestDatabase`), dropped when the fixture is
//! dropped — including when the test panics. Nothing this suite writes survives the run.
//!
//! It did not always. This suite used to open its own pool straight onto the shared
//! `MOIRA_TEST_DATABASE_URL` database, and every run left ten `trusted_jwt_issuers` rows
//! behind for ever (finding F27); `audit_logs`, which is append-only and never swept,
//! accumulated alongside them. Two dimensions were added to scope every audit assertion,
//! and they are **kept** now that the database is private:
//!
//! 1. `metadata->>'jwks_url'` — every JWKS URL this fixture registers carries a
//!    per-fixture `Uuid::now_v7()` suffix, **including the ones whose host must stay in a
//!    denied range** (the suffix goes in the *path*, so `127.0.0.1` and
//!    `169.254.169.254` remain the literal hosts under test); and
//! 2. `occurred_at > fixture_start`, read from the database clock before the fixture's
//!    server is started.
//!
//! Both are kept deliberately. Before this scoping existed, two tests asserted against
//! unsuffixed URLs and matched audit rows written by *previous runs*, so they passed on
//! the shared `moira_test` database even with the SSRF check mutated out entirely. A
//! private database makes that impossible today, but the scoping costs nothing and is the
//! only thing that would still hold if the fixture were ever moved back onto a shared
//! pool. `the_fixture_owns_a_disposable_database` is the guard that says it has not been.
//!
//! **Singleflight is not asserted here.** The e2e shape this suite used to carry
//! (N racers, hold the leader at the stub, assert `hits == 1`) cannot observe the
//! property: the only acknowledgement available is "the stub accepted the leader's
//! connection", and releasing on that signal lets the leader warm the cache while the
//! followers still have a TCP connect, an auth pass and an issuer `SELECT` ahead of them
//! — so the followers take the warm-cache fast path and `hits` reads `1` whether or not
//! the lock exists. It survived a mutation that deleted the singleflight lock outright.
//! Singleflight is proven instead by the unit test in `src/security/auth.rs`, which the
//! same mutation kills. What is asserted here is the adjacent, genuinely observable
//! property: a warm cache absorbs a concurrent burst without any further upstream call.

mod support;

use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use moira::{app::AppState, config::Settings};
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::{Barrier, Semaphore},
    time::timeout,
};
use uuid::Uuid;

use support::{MoiraHttpServer, TestDatabase};

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
    /// Held before the *first* response is written. The acknowledgement gate the timeout
    /// test uses instead of a `sleep`.
    ///
    /// A `Semaphore`, not a `Notify`: the stub adds its arrival permit *before* it parks
    /// here, so a `Notify::notify_waiters()` issued by the test in that window would be
    /// dropped outright and the stub would hang forever. A permit added to a semaphore is
    /// never lost, whichever side wins the race.
    hold: Option<Arc<Semaphore>>,
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
}

impl Stub {
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
    let (counter, aborted) = (hits.clone(), aborted_writes.clone());

    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let seen = counter.fetch_add(1, Ordering::SeqCst);

            if seen == 0
                && let Some(hold) = plan.hold.as_ref()
            {
                hold.acquire()
                    .await
                    .expect("stub hold semaphore closed")
                    .forget();
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
    /// The database clock immediately before this fixture's server starts. Every audit
    /// assertion is scoped to rows newer than this, so a row written by a *previous run*
    /// of this suite can never satisfy an assertion made by this one. `audit_logs` is
    /// append-only, so on a reused database a denial assertion would otherwise stay green
    /// forever once any run had recorded it.
    started_at: DateTime<Utc>,
    /// Declared last so it is dropped last: the database outlives every field that still
    /// holds a connection to it.
    database: TestDatabase,
}

impl JwksFixture {
    /// `dev_override == false` is the production posture (https-only, denied ranges
    /// refused). `true` is the documented dev escape hatch, and the only way a loopback
    /// stub IdP is reachable at all — which is itself the proof that the hardening, not
    /// the transport, is what refuses loopback in the other tests.
    async fn new(dev_override: bool) -> Option<Self> {
        // A private database cloned from the migrated template, torn down by
        // `TestDatabase`'s `Drop` — on the panic path as well as the happy one. This
        // suite previously opened its own pool on the shared `MOIRA_TEST_DATABASE_URL`
        // database and every one of the ten `register_issuer` calls below leaked a row
        // that nothing ever deleted (finding F27). `TestDatabase::create` keeps the
        // fail-closed skip contract: `None` when the variable is unset, a panic when
        // `CI=true`.
        let database = TestDatabase::create().await?;
        let pool = database.pool.clone();

        // Read the *database* clock, not the test process's: `occurred_at` is written by
        // PostgreSQL, so comparing against a locally-taken timestamp would depend on the
        // two clocks agreeing.
        let started_at = timeout(
            WAIT,
            sqlx::query_scalar::<_, DateTime<Utc>>("select clock_timestamp()").fetch_one(&pool),
        )
        .await
        .expect("database clock read timed out")
        .expect("database clock");

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
            started_at,
            database,
        })
    }

    /// A JWKS URL on a host that must stay in a denied range, carrying this fixture's
    /// suffix in the **path** so the audit assertion is scoped to this run while the host
    /// under test is unchanged.
    fn denied_host_url(&self, host: &str, label: &str) -> String {
        format!("https://{host}{}", self.jwks_path(label))
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

    /// Denial reasons recorded for this fixture's JWKS URL **during this run**, newest
    /// last.
    ///
    /// Both scoping dimensions matter. `jwks_url` alone is not enough: it is suffixed per
    /// fixture, but a suffix only isolates *concurrent* binaries — a URL that is not
    /// suffixed at all (as two of these tests used to use) matches this run's rows and
    /// every previous run's rows alike. `occurred_at` alone is not enough either, because
    /// another suite may write a `jwks_fetch` denial concurrently.
    async fn denial_reasons(&self, jwks_url: &str) -> Vec<String> {
        timeout(
            WAIT,
            sqlx::query_scalar::<_, String>(
                "select metadata->>'reason'
                 from audit_logs
                 where action = 'jwks_fetch'
                   and result = 'denied'
                   and metadata->>'jwks_url' = $1
                   and occurred_at > $2
                 order by occurred_at",
            )
            .bind(jwks_url)
            .bind(self.started_at)
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
// Anti-leak guard
// ---------------------------------------------------------------------------

/// Finding F27: every row this suite writes must land in a database that is thrown away
/// when the test ends, not in the long-lived one `MOIRA_TEST_DATABASE_URL` names.
///
/// **What this establishes.** That `JwksFixture` is bound to a per-fixture clone owned by
/// `support::TestDatabase`, whose `Drop` drops the database unconditionally — on a
/// dedicated thread with its own runtime, so it runs while the test is unwinding from a
/// panic, which is exactly when a leak would otherwise be permanent. Ten of the tests
/// below insert a `trusted_jwt_issuers` row and none of them deletes it; the disposable
/// database, not any per-test cleanup, is what makes that safe.
///
/// **What it does not establish.** Not that `trusted_jwt_issuers` is empty anywhere else —
/// on a shared database another suite writes to, "the table is empty" would prove nothing,
/// and assertion (c) below is meaningful *only* because (a) and (b) have already
/// established that this database is private and freshly cloned. Not that the teardown
/// itself succeeds: a `SIGKILL`ed process never runs `Drop` at all, and leaves a whole
/// database behind for `sweep_leaked_databases` to collect an hour later. And not that
/// some *other* suite is leak-free — `tests/test_database_isolation.rs` carries that.
#[tokio::test]
async fn the_fixture_owns_a_disposable_database() {
    let Some(fixture) = JwksFixture::new(false).await else {
        return;
    };
    let live = timeout(
        WAIT,
        sqlx::query_scalar::<_, String>("select current_database()").fetch_one(&fixture.pool),
    )
    .await
    .expect("current_database timed out")
    .expect("current_database");

    // (a) Not the shared database. This is the assertion that turns red the moment the
    //     fixture is pointed back at `MOIRA_TEST_DATABASE_URL`.
    let shared = support::shared_database_name().expect("a fixture was built, so the URL parses");
    assert_ne!(
        live, shared,
        "the JWKS fixture is writing to the shared test database `{shared}`, so every \
         `register_issuer` call in this suite leaks a `trusted_jwt_issuers` row that \
         nothing ever deletes — finding F27, which was 160 rows when it was measured"
    );

    // (b) It is this fixture's own clone, named in the shape `TestDatabase::drop` tears
    //     down and `sweep_leaked_databases` collects if the process dies first.
    assert_eq!(
        live,
        fixture.database.name(),
        "the fixture's pool must be connected to the database `TestDatabase` owns and \
         drops; a pool pointing anywhere else outlives the teardown"
    );
    assert!(
        live.starts_with("moira_test_") && !live.starts_with("moira_test_template_"),
        "a fixture database must carry the disposable `moira_test_<unix>_<uuid>` name that \
         teardown and the leak sweep both key on, found `{live}`"
    );

    // (c) Cloned from the empty template, so nothing an earlier run of this suite wrote is
    //     visible to this one.
    let carried_over = timeout(
        WAIT,
        sqlx::query_scalar::<_, i64>("select count(*) from trusted_jwt_issuers")
            .fetch_one(&fixture.pool),
    )
    .await
    .expect("issuer count timed out")
    .expect("issuer count");
    assert_eq!(
        carried_over, 0,
        "a freshly cloned fixture database must carry no `trusted_jwt_issuers` rows at all; \
         finding one means the template was polluted or this pool is on a reused database"
    );

    fixture.shutdown().await;
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
    // resolved IP refuses it — which is the whole point of P1-2. The host is the literal
    // cloud metadata address; the per-fixture suffix lives in the path, so the
    // `trusted_jwt_issuers` count below cannot be satisfied — or violated — by a row a
    // previous run leaked into the shared database.
    let jwks_url = fixture.denied_host_url("169.254.169.254", "metadata");
    let (status, body) = fixture.post_issuer(&jwks_url, "metadata").await;

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
    .bind(&jwks_url)
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
    // Loopback host — the property under test — with a per-fixture path so the audited
    // denial this asserts on can only have been written by *this* run.
    let jwks_url = fixture.denied_host_url("127.0.0.1", "ip-range");
    fixture.register_issuer(&issuer, &jwks_url).await;

    let (status, body) = fixture.authenticate(&issuer).await;
    assert_generic_unauthorized(status, &body);
    fixture.expect_denial(&jwks_url, "ip_range").await;

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
    // fetch; no `sleep()` is involved on either side. The hold is a permit rather than a
    // `Notify`, so the release can never be dropped for want of a parked waiter.
    let hold = Arc::new(Semaphore::new(0));
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
    hold.add_permits(1);

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
    // The literal AWS/GCP/Azure metadata address, with this fixture's suffix in the path
    // so the audit assertion below is scoped to this run rather than to every run that
    // ever registered the same well-known URL.
    let jwks_url = format!(
        "https://169.254.169.254/latest/meta-data/iam/security-credentials/{}",
        fixture.suffix
    );
    fixture.register_issuer(&issuer, &jwks_url).await;

    let (status, body) = fixture.authenticate(&issuer).await;
    assert_generic_unauthorized(status, &body);

    // Server side: the reason is recorded in full.
    fixture.expect_denial(&jwks_url, "ip_range").await;

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

/// A warm JWKS cache absorbs a concurrent burst without any further upstream call.
///
/// **Scope, stated precisely.** This is *not* a singleflight test and must not be read as
/// one. Singleflight is the cache-**miss** coalescing property, and it is not observable
/// from outside the library: the only acknowledgement an external stub can offer is "the
/// leader's connection was accepted", and releasing the leader on that signal lets it
/// finish its fetch and warm the cache while the followers still have a TCP connect, an
/// auth pass and an issuer `SELECT` in front of them. The followers then take the
/// warm-cache path and the upstream hit counter reads `1` with or without the lock — which
/// is exactly what happened: the earlier e2e test with this shape survived a mutation that
/// removed the singleflight lock entirely, across ten consecutive runs. Singleflight is
/// proven by the unit test in `src/security/auth.rs`, which that same mutation kills.
///
/// What *is* observable, and is asserted here, is the cache-**hit** half: once a key set
/// is cached, `RACERS` simultaneous authentications for that issuer contact the upstream
/// zero further times. A mutation that dropped the cache read (or shortened its lifetime
/// to nothing) makes the hit counter read `RACERS + 1`.
///
/// The fan-out gate is a `tokio::sync::Barrier`; the warm-up is a completed HTTP response,
/// not a `sleep()`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_authentications_against_a_warm_cache_make_no_further_upstream_calls() {
    let Some(fixture) = JwksFixture::new(true).await else {
        return;
    };
    let stub = spawn_stub(&fixture.jwks_path("warm-cache"), StubPlan::json(EMPTY_JWKS)).await;
    let issuer = fixture.issuer("warm-cache");
    fixture.register_issuer(&issuer, &stub.url).await;

    // Warm the cache with one completed request. Its 401 is "no matching key", i.e. the
    // fetch itself succeeded — confirmed by the absence of any audited denial.
    let (status, body) = fixture.authenticate(&issuer).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(
        fixture.denial_reasons(&stub.url).await,
        Vec::<String>::new(),
        "the warming fetch must succeed"
    );
    assert_eq!(
        stub.hits(),
        1,
        "the warming fetch is the only upstream call"
    );

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
        "{RACERS} concurrent authentications against a warm cache must not re-contact the \
         upstream IdP"
    );
    assert_eq!(
        fixture.denial_reasons(&stub.url).await,
        Vec::<String>::new()
    );

    fixture.shutdown().await;
}
