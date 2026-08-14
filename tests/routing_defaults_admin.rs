//! E2E coverage for `GET`/`PUT /api/v1/admin/applications/{id}/routing-defaults` (issue #213,
//! context router MVP-static slice).
//!
//! Mirrors the provider runtime-policy admin surface this endpoint was built against
//! (`src/application/runtime_admin.rs::get_provider_runtime_policy`/`put_provider_runtime_policy`):
//! `If-Match` is *optional* at the HTTP layer but, because
//! `RuntimeAdminService::put_application_routing_defaults` always reads a row first —
//! `get_application_routing_defaults` never returns `NotFound`, it materialises the declared
//! defaults instead — a write with no `If-Match` at all is refused even the first time. That is
//! the same contract `tests/execution_policy_if_match.rs` and
//! `tests/runtime_config_invalidation.rs` already exercise for the two sibling endpoints; this
//! file is the routing-defaults instance of it, driven over the real HTTP surface per
//! `plans/CONVENTIONS.md` §3.
//!
//! Fail-closed behaviour is inherited from `tests/support/mod.rs` (`panic!` when `CI=true` and
//! `MOIRA_TEST_DATABASE_URL` is absent).

mod support;

use std::time::Duration;

use axum::{
    body::{Body, to_bytes},
    http::{Request, Response, StatusCode},
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

    fn error_str(&self, name: &str) -> &str {
        self.body["error"]
            .as_object()
            .unwrap_or_else(|| panic!("expected an error envelope, got: {}", self.body))
            .get(name)
            .unwrap_or_else(|| panic!("error envelope has no `{name}`: {}", self.body))
            .as_str()
            .unwrap_or_else(|| panic!("error field `{name}` is not a string: {}", self.body))
    }
}

async fn send(
    fixture: &LifecycleFixture,
    method: &str,
    path: &str,
    if_match: Option<&str>,
    body: Option<Value>,
) -> HttpResult {
    let router = moira::build_router(fixture.state.clone()).expect("build Moira test router");
    let mut builder = Request::builder().method(method).uri(path).header(
        "x-request-id",
        format!("routing-defaults-{}", Uuid::now_v7()),
    );
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    if let Some(value) = if_match {
        builder = builder.header("if-match", value);
    }
    let request = builder
        .body(body.map_or_else(Body::empty, |value| Body::from(value.to_string())))
        .expect("HTTP request");
    let response: Response<Body> = timeout(WAIT, router.oneshot(request))
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

fn path(application_id: Uuid) -> String {
    format!("/api/v1/admin/applications/{application_id}/routing-defaults")
}

/// A brand-new application has never had a row: `GET` must still succeed, answering the
/// declared defaults from `default_application_routing_defaults` rather than `404` — the same
/// "no row yet" shape `GET .../runtime-policy` has for a provider that was just created.
#[tokio::test]
async fn get_before_any_put_returns_the_declared_defaults() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };

    let result = send(&fixture, "GET", &path(fixture.application_id), None, None).await;
    assert_eq!(result.status, StatusCode::OK, "{}", result.body);
    assert_eq!(
        result.body["application_id"],
        fixture.application_id.to_string()
    );
    assert_eq!(result.body["default_priority"], 100);
    assert_eq!(result.body["complexity_weight_profile"], json!({}));
    assert_eq!(result.version(), 1);
    assert_eq!(
        result.etag.as_deref(),
        Some("\"1\""),
        "the ETag must reflect the declared-default version so a caller can chain a PUT"
    );
}

/// `PUT` with no `If-Match` at all is refused, even against a row that has never been written —
/// because the service always reads (and materialises) a row before deciding, there is no
/// "creation" case that skips the precondition.
#[tokio::test]
async fn put_without_if_match_is_rejected_and_writes_nothing() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };

    let rejected = send(
        &fixture,
        "PUT",
        &path(fixture.application_id),
        None,
        Some(json!({ "default_priority": 250 })),
    )
    .await;
    assert_eq!(
        rejected.status,
        StatusCode::BAD_REQUEST,
        "{}",
        rejected.body
    );
    assert_eq!(rejected.error_str("code"), "bad_request");

    let after = send(&fixture, "GET", &path(fixture.application_id), None, None).await;
    assert_eq!(
        after.body["default_priority"], 100,
        "a rejected write must not have mutated the row"
    );
}

/// A stale `If-Match` is a `409`, the same envelope every versioned admin write produces.
#[tokio::test]
async fn put_with_stale_if_match_is_a_conflict() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };

    let rejected = send(
        &fixture,
        "PUT",
        &path(fixture.application_id),
        Some("999"),
        Some(json!({ "default_priority": 250 })),
    )
    .await;
    assert_eq!(rejected.status, StatusCode::CONFLICT, "{}", rejected.body);
    assert_eq!(rejected.error_str("code"), "resource_version_conflict");
}

/// The full round trip: `PUT` with the declared-default version, then a `GET` that reflects it,
/// then a chained `PUT` using the ETag the first write returned — proving the endpoint is a real
/// optimistic-concurrency resource, not merely a settings blob.
#[tokio::test]
async fn put_then_get_round_trips_and_the_returned_etag_chains() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };

    let accepted = send(
        &fixture,
        "PUT",
        &path(fixture.application_id),
        Some("1"),
        Some(json!({
            "default_priority": 250,
            "complexity_weight_profile": {
                "high": {"cost": 0.1, "latency": 0.5, "quality": 1.0}
            }
        })),
    )
    .await;
    assert_eq!(accepted.status, StatusCode::OK, "{}", accepted.body);
    assert_eq!(accepted.body["default_priority"], 250);
    assert_eq!(accepted.version(), 2);
    assert_eq!(accepted.etag.as_deref(), Some("\"2\""));

    let fetched = send(&fixture, "GET", &path(fixture.application_id), None, None).await;
    assert_eq!(fetched.status, StatusCode::OK, "{}", fetched.body);
    assert_eq!(fetched.body["default_priority"], 250);
    assert_eq!(
        fetched.body["complexity_weight_profile"]["high"]["quality"], 1.0,
        "complexity_weight_profile must round-trip: it is not yet read by the execution path \
         (issue #213 MVP-static slice), but it must still be stored and returned faithfully"
    );

    // A partial PUT (only `default_priority`) must leave `complexity_weight_profile` untouched
    // — the same `coalesce(...)` partial-update contract every sibling PUT endpoint has.
    let partial = send(
        &fixture,
        "PUT",
        &path(fixture.application_id),
        Some(&accepted.version().to_string()),
        Some(json!({ "default_priority": 300 })),
    )
    .await;
    assert_eq!(partial.status, StatusCode::OK, "{}", partial.body);
    assert_eq!(partial.body["default_priority"], 300);
    assert_eq!(
        partial.body["complexity_weight_profile"]["high"]["quality"], 1.0,
        "an unset field in the PUT body must not clobber the previously stored value"
    );

    // The version just used is now stale.
    let replayed = send(
        &fixture,
        "PUT",
        &path(fixture.application_id),
        Some(&accepted.version().to_string()),
        Some(json!({ "default_priority": 999 })),
    )
    .await;
    assert_eq!(replayed.status, StatusCode::CONFLICT, "{}", replayed.body);
}

/// `default_priority` must be non-negative, mirroring the check constraint the migration puts
/// on the column and the positive-field guards every sibling `validate_*` function in
/// `runtime_admin.rs` has.
#[tokio::test]
async fn negative_default_priority_is_rejected() {
    let Some(fixture) = LifecycleFixture::new().await else {
        return;
    };

    let rejected = send(
        &fixture,
        "PUT",
        &path(fixture.application_id),
        Some("1"),
        Some(json!({ "default_priority": -1 })),
    )
    .await;
    assert_eq!(
        rejected.status,
        StatusCode::BAD_REQUEST,
        "{}",
        rejected.body
    );
    assert_eq!(rejected.error_str("code"), "bad_request");

    let after = send(&fixture, "GET", &path(fixture.application_id), None, None).await;
    assert_eq!(after.body["default_priority"], 100);
}
