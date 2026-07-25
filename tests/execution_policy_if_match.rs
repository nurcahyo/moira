//! E2E coverage for the required `If-Match` precondition on the application
//! execution-policy PUT (plan 04, finding P1-8).
//!
//! `PUT /api/v1/admin/applications/{id}/execution-policy` used to be the one versioned
//! mutation where `If-Match` was optional, so two concurrent writers could silently clobber
//! each other with no conflict signal. This file drives the real HTTP surface and pins every
//! observable outcome:
//!
//! * missing header  → `400` coded `if_match_required` (a **new** code — every sibling
//!   endpoint calling `require_if_match` now emits it too, in place of `bad_request`);
//! * malformed header → `400` still coded `bad_request`, deliberately unchanged;
//! * stale version   → the pre-existing `409 resource_version_conflict` envelope;
//! * current version → `200` with the version bumped and a matching `ETag`;
//! * two writers racing on the *same* current version → exactly one `200` and one `409`.
//!
//! Both message keys are checked against the response catalog itself
//! (`moira::i18n::is_known_key`) rather than against a literal repeated in the test, so a key
//! that is spelled correctly but never registered — and would therefore render as an empty
//! string in a translated console — still fails.
//!
//! Every identifier written here carries a per-fixture `Uuid`, and the fixture creates its
//! own application, so nothing in this file can be satisfied by a row an earlier run left in
//! the shared database.
//!
//! The last test is the one that matters most:
//! `concurrent_execution_policy_puts_with_the_same_version_yield_exactly_one_success_and_one_409`
//! releases two writers holding the *same currently-valid* `If-Match` through a
//! [`tokio::sync::Barrier`]. Before the repository moved the version comparison into the same
//! transaction as the write, that scenario produced two `200`s and a silently lost update:
//! `If-Match` was validated on one pooled connection and the write issued unconditionally on
//! another, so it rejected only *stale* versions — never a genuine race. The sequential tests
//! above cannot see that; only this one proves the SQL-level version check closed the TOCTOU.
//! It is an acknowledgement gate, not a timing guess: there is no `sleep()` anywhere in this
//! file.
//!
//! Fail-closed behaviour is inherited verbatim from `tests/support/mod.rs` (`panic!` when
//! `CI=true` and `MOIRA_TEST_DATABASE_URL` is absent, matched on the **value** of `CI` per
//! `plans/CONVENTIONS.md` §3).

mod support;

use std::{sync::Arc, time::Duration};

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, Response, StatusCode},
};
use serde_json::{Value, json};
use tokio::{sync::Barrier, time::timeout};
use tower::ServiceExt;
use uuid::Uuid;

use support::LifecycleFixture;

const WAIT: Duration = Duration::from_secs(15);

struct HttpResult {
    status: StatusCode,
    body: Value,
    etag: Option<String>,
}

impl HttpResult {
    fn version(&self) -> i64 {
        self.body["version"]
            .as_i64()
            .unwrap_or_else(|| panic!("response carries no numeric `version`: {}", self.body))
    }

    fn error_field(&self, name: &str) -> &Value {
        self.body["error"]
            .as_object()
            .unwrap_or_else(|| panic!("expected an error envelope, got: {}", self.body))
            .get(name)
            .unwrap_or_else(|| panic!("error envelope has no `{name}`: {}", self.body))
    }

    fn error_str(&self, name: &str) -> &str {
        self.error_field(name)
            .as_str()
            .unwrap_or_else(|| panic!("error field `{name}` is not a string: {}", self.body))
    }
}

struct Fixture {
    inner: LifecycleFixture,
    router: Router,
    suffix: String,
}

impl Fixture {
    async fn new() -> Option<Self> {
        let inner = LifecycleFixture::new().await?;
        let router = moira::build_router(inner.state.clone()).expect("build Moira test router");
        let suffix = Uuid::now_v7().simple().to_string();
        Some(Self {
            inner,
            router,
            suffix,
        })
    }

    fn path(&self) -> String {
        format!(
            "/api/v1/admin/applications/{}/execution-policy",
            self.inner.application_id
        )
    }

    async fn get_policy(&self) -> HttpResult {
        self.send("GET", &self.path(), None, None).await
    }

    /// `if_match` is passed as a raw string so a malformed value is expressible.
    async fn put_policy(&self, if_match: Option<&str>, body: Value) -> HttpResult {
        self.send("PUT", &self.path(), if_match, Some(body)).await
    }

    fn body(&self, requests_per_minute: i32) -> Value {
        json!({
            "responses_enabled": true,
            "streaming_enabled": true,
            "rate_limit_requests_per_minute": requests_per_minute,
            "metadata": {"suite": "execution_policy_if_match", "fixture": self.suffix}
        })
    }

    async fn send(
        &self,
        method: &str,
        path: &str,
        if_match: Option<&str>,
        body: Option<Value>,
    ) -> HttpResult {
        let request = build_request(method, path, if_match, body);
        let response = timeout(WAIT, self.router.clone().oneshot(request))
            .await
            .expect("HTTP request timed out")
            .expect("HTTP response");
        read_result(response).await
    }
}

/// Builds a request without sending it, so a concurrency test can do all of the setup work
/// *before* the barrier releases and leave only the HTTP call inside the raced window.
fn build_request(
    method: &str,
    path: &str,
    if_match: Option<&str>,
    body: Option<Value>,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header("x-request-id", format!("if-match-{}", Uuid::now_v7()));
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    if let Some(value) = if_match {
        builder = builder.header("if-match", value);
    }
    builder
        .body(body.map_or_else(Body::empty, |value| Body::from(value.to_string())))
        .expect("HTTP request")
}

async fn read_result(response: Response<Body>) -> HttpResult {
    let status = response.status();
    let etag = response
        .headers()
        .get("etag")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body");
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("JSON response body")
    };
    HttpResult { status, body, etag }
}

/// Asserts the full i18n contract on an error envelope: the code, the derived key, that the
/// key is actually catalogued, and that a non-empty English default came with it.
fn assert_coded_error(result: &HttpResult, status: StatusCode, code: &str) {
    assert_eq!(
        result.status, status,
        "unexpected status; body was: {}",
        result.body
    );
    assert_eq!(result.error_str("code"), code, "body: {}", result.body);

    let message_key = result.error_str("message_key");
    assert_eq!(message_key, format!("moira.error.{code}"));
    assert!(
        moira::i18n::is_known_key(message_key),
        "{message_key} is not registered in the response catalog"
    );
    assert!(
        !result.error_str("message").is_empty(),
        "a coded error must carry a non-empty default English message"
    );
    assert!(
        result.error_field("message_args").is_object(),
        "message_args must be a structured object, got: {}",
        result.error_field("message_args")
    );
    assert!(
        !result.error_str("request_id").is_empty(),
        "the error envelope must propagate a request id"
    );
}

#[tokio::test]
async fn execution_policy_put_without_if_match_returns_400_with_a_keyed_error() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };

    let before = fixture.get_policy().await;
    assert_eq!(before.status, StatusCode::OK, "{}", before.body);
    let version_before = before.version();

    let rejected = fixture.put_policy(None, fixture.body(11)).await;

    // The dedicated code, not the generic `bad_request` this path used to return. The
    // response shape, status, and message text are unchanged; only `code`/`message_key` are
    // more specific, and every sibling endpoint calling `require_if_match` moved with it.
    assert_coded_error(&rejected, StatusCode::BAD_REQUEST, "if_match_required");

    // The rejection must be a rejection: a missing precondition may not write anything.
    let after = fixture.get_policy().await;
    assert_eq!(
        after.version(),
        version_before,
        "a request rejected for a missing If-Match must not have mutated the policy"
    );
}

#[tokio::test]
async fn execution_policy_put_with_a_malformed_if_match_stays_bad_request() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };

    // Deliberately *not* `if_match_invalid`. This plan gave the missing-header branch its own
    // code and left the malformed-header branches on `bad_request`; pinning that here stops a
    // later change from splitting them without anyone noticing the contract moved.
    let rejected = fixture
        .put_policy(Some("not-a-version"), fixture.body(12))
        .await;
    assert_coded_error(&rejected, StatusCode::BAD_REQUEST, "bad_request");
}

#[tokio::test]
async fn execution_policy_put_with_stale_if_match_returns_409_resource_version_conflict() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };

    let current = fixture.get_policy().await;
    assert_eq!(current.status, StatusCode::OK, "{}", current.body);
    let version = current.version();

    // A version that cannot be current, in the direction a lost update actually takes: the
    // caller read version N, someone else wrote, and the caller is now behind.
    let stale = version - 1;
    let rejected = fixture
        .put_policy(Some(&stale.to_string()), fixture.body(13))
        .await;

    assert_coded_error(&rejected, StatusCode::CONFLICT, "resource_version_conflict");

    let after = fixture.get_policy().await;
    assert_eq!(
        after.version(),
        version,
        "a stale-version write must be refused, not applied"
    );
}

#[tokio::test]
async fn execution_policy_put_with_current_if_match_succeeds_and_bumps_the_version() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };

    let current = fixture.get_policy().await;
    assert_eq!(current.status, StatusCode::OK, "{}", current.body);
    let version = current.version();

    let accepted = fixture
        .put_policy(Some(&version.to_string()), fixture.body(17))
        .await;
    assert_eq!(
        accepted.status,
        StatusCode::OK,
        "a current If-Match must be accepted: {}",
        accepted.body
    );
    assert!(
        accepted.version() > version,
        "an accepted write must advance the version: {} -> {}",
        version,
        accepted.version()
    );
    assert_eq!(
        accepted.body["rate_limit_requests_per_minute"], 17,
        "the accepted write must actually be applied: {}",
        accepted.body
    );
    assert_eq!(
        accepted.etag.as_deref(),
        Some(format!("\"{}\"", accepted.version()).as_str()),
        "the response ETag must carry the new version so the caller can chain writes"
    );

    // The version the caller just used is now stale. Sequentially that is all this proves —
    // a *stale* version is refused. It says nothing about two writers arriving with the same
    // still-valid version, which is the case that actually loses updates; see
    // `concurrent_execution_policy_puts_with_the_same_version_yield_exactly_one_success_and_one_409`
    // for the property that makes If-Match a real optimistic-concurrency control rather than a
    // formality.
    let replayed = fixture
        .put_policy(Some(&version.to_string()), fixture.body(19))
        .await;
    assert_coded_error(&replayed, StatusCode::CONFLICT, "resource_version_conflict");

    // And the ETag from the successful write is immediately usable as the next If-Match.
    let chained = fixture
        .put_policy(Some(&accepted.version().to_string()), fixture.body(23))
        .await;
    assert_eq!(
        chained.status,
        StatusCode::OK,
        "the version returned by the previous write must be accepted: {}",
        chained.body
    );
    assert!(chained.version() > accepted.version());
}

/// The test named by plan 04's DoD: the one that proves the SQL-level version check actually
/// closed the TOCTOU between reading the current version and writing.
///
/// Two writers read the *same* current version — both `If-Match` values are valid at the moment
/// they are formed — and are then released simultaneously through a [`Barrier`]. Exactly one
/// may win.
///
/// Against the pre-fix code (`get_or_create…` on one pooled connection, compare in Rust, then an
/// unconditional `insert … on conflict do update` on another) this reproduced 2 successes and 0
/// conflicts: the second write silently clobbered the first, and the version advanced twice
/// with only one writer's values surviving.
///
/// The barrier is an acknowledgement gate, not a delay: `Barrier::wait` returns only once every
/// writer has arrived, so the race window is opened deterministically with no `sleep()`. A
/// multi-threaded runtime is required so the two requests occupy two pool connections at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_execution_policy_puts_with_the_same_version_yield_exactly_one_success_and_one_409()
 {
    let Some(fixture) = Fixture::new().await else {
        return;
    };

    let current = fixture.get_policy().await;
    assert_eq!(current.status, StatusCode::OK, "{}", current.body);
    let version = current.version();

    // Distinguishable payloads, so the surviving row identifies which writer actually won and a
    // "last write silently wins" outcome cannot masquerade as a correct one.
    let writers = [31_i32, 97_i32];
    let barrier = Arc::new(Barrier::new(writers.len()));

    let mut handles = Vec::with_capacity(writers.len());
    for rpm in writers {
        let router = fixture.router.clone();
        let path = fixture.path();
        let body = fixture.body(rpm);
        let if_match = version.to_string();
        let barrier = Arc::clone(&barrier);
        handles.push(tokio::spawn(async move {
            // Everything that can be done ahead of the race is done ahead of the race.
            let request = build_request("PUT", &path, Some(&if_match), Some(body));
            barrier.wait().await;
            let response = timeout(WAIT, router.oneshot(request))
                .await
                .expect("concurrent HTTP request timed out")
                .expect("concurrent HTTP response");
            (rpm, read_result(response).await)
        }));
    }

    let mut results = Vec::with_capacity(writers.len());
    for handle in handles {
        results.push(handle.await.expect("concurrent writer task"));
    }

    let successes: Vec<_> = results
        .iter()
        .filter(|(_, result)| result.status == StatusCode::OK)
        .collect();
    let conflicts: Vec<_> = results
        .iter()
        .filter(|(_, result)| result.status == StatusCode::CONFLICT)
        .collect();

    let observed: Vec<_> = results
        .iter()
        .map(|(rpm, result)| format!("rpm={rpm} status={} body={}", result.status, result.body))
        .collect();
    assert_eq!(
        (successes.len(), conflicts.len()),
        (1, 1),
        "two writers holding the same valid If-Match must resolve to exactly one success and \
         one conflict; observed: {observed:#?}"
    );

    let (winning_rpm, winner) = successes[0];
    assert_eq!(
        winner.version(),
        version + 1,
        "the single winning write must advance the version exactly once: {}",
        winner.body
    );

    // The loser must be the ordinary 409 envelope every other versioned endpoint produces, so
    // a client needs no new code path to handle it.
    assert_coded_error(
        &conflicts[0].1,
        StatusCode::CONFLICT,
        "resource_version_conflict",
    );

    // And the durable state must be the winner's, at the winner's version: neither a blend of the
    // two writes nor the loser's values applied on top.
    let settled = fixture.get_policy().await;
    assert_eq!(settled.status, StatusCode::OK, "{}", settled.body);
    assert_eq!(
        settled.version(),
        version + 1,
        "a refused write must not have advanced the version a second time: {}",
        settled.body
    );
    assert_eq!(
        settled.body["rate_limit_requests_per_minute"], *winning_rpm,
        "the persisted policy must be the one the successful writer sent: {}",
        settled.body
    );
}
