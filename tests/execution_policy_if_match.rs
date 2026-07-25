//! E2E coverage for the required `If-Match` precondition on the application
//! execution-policy PUT (plan 04, finding P1-8).
//!
//! `PUT /api/v1/admin/applications/{id}/execution-policy` used to be the one versioned
//! mutation where `If-Match` was optional, so two concurrent writers could silently clobber
//! each other with no conflict signal. This file drives the real HTTP surface and pins the
//! three observable outcomes:
//!
//! * missing header  → `400` coded `if_match_required` (a **new** code — every sibling
//!   endpoint calling `require_if_match` now emits it too, in place of `bad_request`);
//! * malformed header → `400` still coded `bad_request`, deliberately unchanged;
//! * stale version   → the pre-existing `409 resource_version_conflict` envelope;
//! * current version → `200` with the version bumped and a matching `ETag`.
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
//! There is no `sleep()` and no concurrency: each test is an ordered sequence of request /
//! response round trips.
//!
//! Fail-closed behaviour is inherited verbatim from `tests/support/mod.rs` (`panic!` when
//! `CI=true` and `MOIRA_TEST_DATABASE_URL` is absent, matched on the **value** of `CI` per
//! `plans/CONVENTIONS.md` §3).

mod support;

use std::time::Duration;

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use tokio::time::timeout;
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
        let request = builder
            .body(body.map_or_else(Body::empty, |value| Body::from(value.to_string())))
            .expect("HTTP request");

        let response = timeout(WAIT, self.router.clone().oneshot(request))
            .await
            .expect("HTTP request timed out")
            .expect("HTTP response");
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

    // The version the caller just used is now stale, which is what makes If-Match a real
    // optimistic-concurrency control rather than a formality.
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
