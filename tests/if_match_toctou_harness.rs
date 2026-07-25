//! Failing-first evidence for the `If-Match` TOCTOU (plan 06 module 17 → plan **06b**).
//!
//! Every versioned admin mutation except two validates `If-Match` like this
//! (`src/http/admin.rs`, 33 sites):
//!
//! ```text
//! ensure_version(
//!     service.get_X(&actor, id).await?.version,   // read, on one pooled connection
//!     require_if_match(&headers)?,
//! )?;
//! service.patch_X(&actor, &ctx, id, request).await?;  // write, on another, unconditional
//! ```
//!
//! The comparison happens in Rust, against a version fetched by an earlier, separate
//! statement. Nothing carries the caller's expectation into the `UPDATE`. So the check
//! rejects only versions that were *already* stale when the request arrived; two writers
//! that both read the same currently-valid version both pass the check and both write, and
//! the first writer's values are lost with no conflict reported to anyone.
//!
//! The two exceptions are the template for the fix, and they are the reason this is a
//! provable bug rather than a theory:
//!
//! * `rotate_credential` (`src/http/admin.rs:902`) — plan 04. `expected_version` is passed
//!   down to `PgAdminRepository::rotate_credential`
//!   (`src/infra/repositories/admin.rs:444`), which takes a `select … for update` row lock
//!   inside the command transaction and compares there.
//! * `put_application_execution_policy` (`src/http/admin.rs:335`) — plan 04. Same shape plus
//!   a `where … and version = $22` predicate on the `UPDATE`
//!   (`src/infra/repositories/public.rs:312`), so a zero-row result *is* the conflict.
//!
//! `tests/execution_policy_if_match.rs` already proves the second one holds under a race.
//! This file is that same race pointed at two sites that have **not** been fixed — one per
//! owning service, because the two services sit behind different repositories and different
//! idempotency schemes and must be converted separately:
//!
//! | site | handler | service | repository write |
//! |---|---|---|---|
//! | `src/http/admin.rs:199` | `patch_application` | `AdminService` | `PgAdminRepository::patch_application` |
//! | `src/http/admin.rs:1724` | `patch_route_definition` | `RuntimeAdminService` | `PgRuntimeRepository::patch_route_definition` |
//!
//! # These tests are `#[ignore]`d, and they are supposed to fail
//!
//! They are not aspirational and they are not flaky. Run them today and each reports **two**
//! `200`s where the contract allows one, with the losing writer's `display_name` gone from the
//! database and the version advanced twice. That is the lost update, observed end to end
//! through the real HTTP surface.
//!
//! They are `#[ignore]`d only so `cargo test` stays green while the fix is sequenced into
//! plan 06b — module 17 deliberately delivers the evidence and not the fix, because adding
//! `expected_version` to the service signatures contradicts plan 06 module 6's requirement
//! that `AdminService`'s 46 signatures are provably unchanged. Plan 06b removes both
//! `#[ignore]` attributes as its definition of done. Do not delete these tests, and do not
//! relax the assertions to make them pass; the assertion *is* the specification.
//!
//! ```bash
//! export MOIRA_TEST_DATABASE_URL='postgres://postgres:postgres@127.0.0.1:5432/moira'
//! cargo test --test if_match_toctou_harness -- --ignored --nocapture
//! ```
//!
//! # How the race is opened
//!
//! Through a [`tokio::sync::Barrier`], exactly as
//! `tests/execution_policy_if_match.rs` does. `Barrier::wait` returns only once every writer
//! has arrived, so the window is opened by acknowledgement rather than by guessing at a
//! delay: there is no `sleep()` in this file, per `plans/CONVENTIONS.md` §3. Request building
//! happens before the barrier so that only the HTTP call itself is inside the raced window,
//! and a multi-threaded runtime is required so the two requests genuinely occupy two pool
//! connections at once.
//!
//! Database isolation and fail-closed behaviour are inherited from [`support::LifecycleFixture`]:
//! each fixture owns a private migrated database (plan 06 module 13), and the suite skips when
//! `MOIRA_TEST_DATABASE_URL` is absent but panics if `CI=true`.

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
}

impl HttpResult {
    fn version(&self) -> i64 {
        self.body["version"]
            .as_i64()
            .unwrap_or_else(|| panic!("response carries no numeric `version`: {}", self.body))
    }

    fn display_name(&self) -> &str {
        self.body["display_name"]
            .as_str()
            .unwrap_or_else(|| panic!("response carries no `display_name`: {}", self.body))
    }
}

struct Fixture {
    inner: LifecycleFixture,
    router: Router,
}

impl Fixture {
    async fn new() -> Option<Self> {
        let inner = LifecycleFixture::new().await?;
        let router = moira::build_router(inner.state.clone()).expect("build Moira test router");
        Some(Self { inner, router })
    }

    fn application_path(&self) -> String {
        format!("/api/v1/admin/applications/{}", self.inner.application_id)
    }

    fn route_path(&self) -> String {
        format!("/api/v1/admin/routes/{}", self.inner.route_id)
    }

    async fn get(&self, path: &str) -> HttpResult {
        let request = build_request("GET", path, None, None);
        let response = timeout(WAIT, self.router.clone().oneshot(request))
            .await
            .expect("HTTP request timed out")
            .expect("HTTP response");
        read_result(response).await
    }
}

/// Builds a request without sending it, so all setup happens *before* the barrier releases
/// and only the HTTP call sits inside the raced window.
fn build_request(
    method: &str,
    path: &str,
    if_match: Option<&str>,
    body: Option<Value>,
) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header("x-request-id", format!("toctou-{}", Uuid::now_v7()));
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
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body");
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("JSON response body")
    };
    HttpResult { status, body }
}

/// Releases two `PATCH`es holding the **same, currently valid** `If-Match` and returns what
/// each one got back, paired with the `display_name` it tried to write.
async fn race_two_patches(
    router: &Router,
    path: &str,
    version: i64,
    names: [&'static str; 2],
) -> Vec<(&'static str, HttpResult)> {
    let barrier = Arc::new(Barrier::new(names.len()));
    let mut handles = Vec::with_capacity(names.len());

    for name in names {
        let router = router.clone();
        let request = build_request(
            "PATCH",
            path,
            Some(&version.to_string()),
            Some(json!({ "display_name": name })),
        );
        let barrier = Arc::clone(&barrier);
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            let response = timeout(WAIT, router.oneshot(request))
                .await
                .expect("concurrent HTTP request timed out")
                .expect("concurrent HTTP response");
            (name, read_result(response).await)
        }));
    }

    let mut results = Vec::with_capacity(names.len());
    for handle in handles {
        results.push(handle.await.expect("concurrent writer task"));
    }
    results
}

/// The shared assertion: of two writers holding the same valid `If-Match`, exactly one may
/// win, the version may advance exactly once, and the row that survives must be the winner's
/// — not a blend, and not the loser's values written on top.
async fn assert_exactly_one_writer_wins(
    fixture: &Fixture,
    path: &str,
    version_before: i64,
    results: &[(&'static str, HttpResult)],
) {
    let observed: Vec<_> = results
        .iter()
        .map(|(name, result)| {
            format!(
                "display_name={name:?} status={} body={}",
                result.status, result.body
            )
        })
        .collect();

    let successes: Vec<_> = results
        .iter()
        .filter(|(_, result)| result.status == StatusCode::OK)
        .collect();
    let conflicts: Vec<_> = results
        .iter()
        .filter(|(_, result)| result.status == StatusCode::CONFLICT)
        .collect();

    assert_eq!(
        (successes.len(), conflicts.len()),
        (1, 1),
        "two writers holding the same valid If-Match must resolve to exactly one 200 and one \
         409 `resource_version_conflict`. Two successes means the If-Match check ran against a \
         version read on a different connection than the write, and one writer's update was \
         silently lost. Observed: {observed:#?}"
    );

    assert_eq!(
        conflicts[0].1.body["error"]["code"], "resource_version_conflict",
        "the loser must be the ordinary versioned-conflict envelope every other endpoint \
         already emits, so no client needs a new code path: {}",
        conflicts[0].1.body
    );

    let (winning_name, winner) = successes[0];
    assert_eq!(
        winner.version(),
        version_before + 1,
        "the single winning write must advance the version exactly once: {}",
        winner.body
    );

    let settled = fixture.get(path).await;
    assert_eq!(settled.status, StatusCode::OK, "{}", settled.body);
    assert_eq!(
        settled.version(),
        version_before + 1,
        "a refused write must not have advanced the version a second time: {}",
        settled.body
    );
    assert_eq!(
        settled.display_name(),
        *winning_name,
        "the persisted row must be the one the successful writer sent: {}",
        settled.body
    );
}

/// `AdminService` representative — 21 of the 33 sites reach the database this way.
///
/// `patch_application` (`src/http/admin.rs:190`) reads the version through
/// `AdminService::get_application`, compares it in `ensure_version`, then calls
/// `AdminService::patch_application` → `PgAdminRepository::patch_application`
/// (`src/infra/repositories/admin.rs:866`), whose `UPDATE` has no `version` predicate and runs
/// on a connection the read never touched.
///
/// Expected failure today: `(successes, conflicts)` is `(2, 0)`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "plan 06b closes the If-Match TOCTOU; see plans/06b-if-match-toctou.md"]
async fn concurrent_application_patches_with_the_same_version_must_yield_one_success_and_one_409() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };

    let path = fixture.application_path();
    let before = fixture.get(&path).await;
    assert_eq!(before.status, StatusCode::OK, "{}", before.body);
    let version = before.version();

    let results = race_two_patches(
        &fixture.router,
        &path,
        version,
        ["toctou-application-a", "toctou-application-b"],
    )
    .await;

    assert_exactly_one_writer_wins(&fixture, &path, version, &results).await;
}

/// `RuntimeAdminService` representative — the other 12 sites.
///
/// Same defect, different service, different repository, and — per plan 06 module 17 §17.2 —
/// a different idempotency scheme, which is why plan 06b must convert this family separately
/// and only after module 16 unifies `actor_fingerprint`.
/// `patch_route_definition` (`src/http/admin.rs:1715`) reaches
/// `PgRuntimeRepository::patch_route_definition` (`src/infra/repositories/runtime.rs:338`).
///
/// Expected failure today: `(successes, conflicts)` is `(2, 0)`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "plan 06b closes the If-Match TOCTOU; see plans/06b-if-match-toctou.md"]
async fn concurrent_route_patches_with_the_same_version_must_yield_one_success_and_one_409() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };

    let path = fixture.route_path();
    let before = fixture.get(&path).await;
    assert_eq!(before.status, StatusCode::OK, "{}", before.body);
    let version = before.version();

    let results = race_two_patches(
        &fixture.router,
        &path,
        version,
        ["toctou-route-a", "toctou-route-b"],
    )
    .await;

    assert_exactly_one_writer_wins(&fixture, &path, version, &results).await;
}
