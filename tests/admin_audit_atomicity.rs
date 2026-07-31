//! Forced-failure evidence that an admin write and its `audit_logs` row commit together —
//! or not at all.
//!
//! # The finding this closes
//!
//! `plans/reports/EXECUTION-LEDGER.md` carried the one-liner *"Admin write + audit row still
//! non-atomic"* with no owner and no analysis. It is true of **36** mutation sites and false
//! of **20**: every path that runs its write inside `AdminCommandRunner::execute` writes its
//! audit row through `PgAdminCommandTransaction::insert_audit`, in the same transaction, and
//! is already atomic. Every path that does not — the whole `PATCH` / `DELETE` /
//! `enable`-`disable` family, plus all thirteen `RuntimeAdminService` mutations — committed
//! the write on one pooled connection and then wrote the audit row on **another**.
//!
//! Only one direction of divergence was reachable, and it is the serious one: the write
//! commits and the audit row does not. **An administrative change with no record of it.**
//!
//! # The injection is a header, not a fault library
//!
//! `RequestContext::from_headers` (`src/application/context.rs:17`) takes `x-request-id`
//! from the caller **verbatim and unbounded**, and `audit_logs.request_id` is
//! `varchar(128)` (`migrations/0003_security_foundation.sql:322`). A caller-supplied request
//! id of 129 characters or more therefore makes the audit `INSERT` — and only the audit
//! `INSERT` — fail with SQLSTATE `22001`, deterministically, from one ordinary HTTP request.
//!
//! That is what makes this a proof rather than an argument. Before the fix the `PATCH` below
//! returns `500`, the application's `display_name` **is changed in the database**, its
//! `version` is advanced, and `audit_logs` holds nothing for it. The admin action happened
//! and nobody can tell.
//!
//! Observed at `cdb2f46`, before the fix:
//!
//! ```text
//! the write must be rolled back with its audit row: at cdb2f46 the UPDATE commits and the
//! audit INSERT does not, which is the finding
//!   left: "audit-suppressed"
//!  right: "Lifecycle …"
//! ```
//!
//! # Why the assertions are shaped the way they are
//!
//! Every test here asserts its own **premise** before asserting the property, because the
//! property alone passes in the broken arrangement too:
//!
//! * an assertion that the audit row is absent is satisfied by a `PATCH` that never ran;
//! * an assertion that the row is present is satisfied by a `PATCH` that was never injected.
//!
//! So [`the_write_and_its_audit_row_are_lost_together`] pins the status as `500` (the
//! injection fired), the resource as **unchanged** (the write did not survive it), and the
//! audit table as empty for that resource — and
//! [`the_same_write_without_the_injection_commits_both_rows`] pins `200` plus exactly one
//! audit row over the identical request, so the first test cannot be passing because the
//! endpoint is broken.
//!
//! **If someone bounds or sanitises `x-request-id`, these tests fail rather than pass**: the
//! injected `PATCH` starts returning `200`, and the `500` premise is the first assertion in
//! the test. Replace the lever (a `before insert on audit_logs` trigger that `raise`s, in
//! this fixture's private database) rather than deleting the test.
//!
//! ```bash
//! export MOIRA_TEST_DATABASE_URL='postgres://postgres:postgres@127.0.0.1:5432/moira'
//! cargo test --test admin_audit_atomicity
//! ```

mod support;

use std::time::Duration;

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, Response, StatusCode},
};
use serde_json::{Value, json};
use tokio::time::timeout;
use tower::ServiceExt;
use uuid::Uuid;

use support::LifecycleFixture;

const WAIT: Duration = Duration::from_secs(15);

/// One character over `audit_logs.request_id`'s `varchar(128)`.
///
/// Deliberately minimal: a 1 000-character value would also trip any header-size limit a
/// future proxy adds, and then the test would be measuring the proxy.
fn oversized_request_id() -> String {
    "a".repeat(129)
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

    async fn send(&self, request: Request<Body>) -> (StatusCode, Value) {
        let response = timeout(WAIT, self.router.clone().oneshot(request))
            .await
            .expect("HTTP request timed out")
            .expect("HTTP response");
        read_result(response).await
    }

    /// The `display_name` as it stands **in the database**, not as an endpoint reports it.
    async fn stored_display_name(&self) -> String {
        sqlx::query_scalar::<_, String>("select display_name from applications where id = $1")
            .bind(self.inner.application_id)
            .fetch_one(&self.inner.pool)
            .await
            .expect("read the application row")
    }

    async fn stored_version(&self) -> i64 {
        sqlx::query_scalar::<_, i64>("select version from applications where id = $1")
            .bind(self.inner.application_id)
            .fetch_one(&self.inner.pool)
            .await
            .expect("read the application version")
    }

    /// How many `audit_logs` rows name this application as their resource, for `action`.
    async fn audit_rows(&self, action: &str) -> i64 {
        sqlx::query_scalar::<_, i64>(
            "select count(*) from audit_logs where resource_type = 'application'
               and resource_id = $1 and action = $2",
        )
        .bind(self.inner.application_id.to_string())
        .bind(action)
        .fetch_one(&self.inner.pool)
        .await
        .expect("count audit rows")
    }

    async fn applications_named(&self, slug: &str) -> i64 {
        sqlx::query_scalar::<_, i64>("select count(*) from applications where application_slug = $1")
            .bind(slug)
            .fetch_one(&self.inner.pool)
            .await
            .expect("count applications")
    }
}

fn patch_request(path: &str, version: i64, display_name: &str, request_id: &str) -> Request<Body> {
    Request::builder()
        .method("PATCH")
        .uri(path)
        .header("x-request-id", request_id)
        .header("if-match", version.to_string())
        .header("content-type", "application/json")
        .body(Body::from(
            json!({ "display_name": display_name }).to_string(),
        ))
        .expect("HTTP request")
}

async fn read_result(response: Response<Body>) -> (StatusCode, Value) {
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body");
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, body)
}

/// **The finding.** A write whose audit row cannot be written must leave no write behind.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_write_and_its_audit_row_are_lost_together() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };

    let before_name = fixture.stored_display_name().await;
    let before_version = fixture.stored_version().await;
    let before_audits = fixture.audit_rows("application.update").await;

    let (status, body) = fixture
        .send(patch_request(
            &fixture.application_path(),
            before_version,
            "audit-suppressed",
            &oversized_request_id(),
        ))
        .await;

    // Premise: the injection fired. A `200` here means `x-request-id` is now bounded
    // somewhere upstream and this test is no longer injecting anything — fix the lever,
    // do not delete the test.
    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "the oversized x-request-id must make the audit INSERT fail; body: {body}"
    );

    assert_eq!(
        fixture.stored_display_name().await,
        before_name,
        "the write must be rolled back with its audit row: at cdb2f46 the UPDATE commits \
         and the audit INSERT does not, which is the finding"
    );
    assert_eq!(
        fixture.stored_version().await,
        before_version,
        "a rolled-back write must not advance the row's version"
    );
    assert_eq!(
        fixture.audit_rows("application.update").await,
        before_audits,
        "no audit row may survive a rolled-back write either"
    );
}

/// Non-vacuity anchor: the identical request, uninjected, commits **both** rows.
///
/// Without this, [`the_write_and_its_audit_row_are_lost_together`] would pass just as well
/// against a `PATCH` endpoint that had been deleted.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_same_write_without_the_injection_commits_both_rows() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };

    let before_version = fixture.stored_version().await;
    let before_audits = fixture.audit_rows("application.update").await;

    let (status, body) = fixture
        .send(patch_request(
            &fixture.application_path(),
            before_version,
            "audit-recorded",
            &format!("atomicity-{}", Uuid::now_v7()),
        ))
        .await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        fixture.stored_display_name().await,
        "audit-recorded",
        "the uninjected write must commit"
    );
    assert_eq!(
        fixture.audit_rows("application.update").await,
        before_audits + 1,
        "the uninjected write must leave exactly one audit row"
    );
}

/// The already-atomic half of the finding, pinned so a refactor cannot quietly undo it.
///
/// `create_application` runs inside `AdminCommandRunner::execute` and writes its audit row
/// through the command transaction. It passed this before the fix as well — which is the
/// point: 20 of the 56 admin mutation sites were never part of the finding, and the ledger's
/// one-liner did not say so.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_create_inside_the_command_envelope_was_already_atomic() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };

    let slug = format!("atomicity-{}", Uuid::now_v7().simple());
    let request = Request::builder()
        .method("POST")
        .uri("/api/v1/admin/applications")
        .header("x-request-id", oversized_request_id())
        .header("content-type", "application/json")
        .body(Body::from(
            json!({
                "external_application_id": slug,
                "application_slug": slug,
                "display_name": "Envelope control",
            })
            .to_string(),
        ))
        .expect("HTTP request");

    let (status, body) = fixture.send(request).await;

    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "the oversized x-request-id must make the audit INSERT fail here too; body: {body}"
    );
    assert_eq!(
        fixture.applications_named(&slug).await,
        0,
        "the command envelope rolls the create back with its audit row"
    );
}
