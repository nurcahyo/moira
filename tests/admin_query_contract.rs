//! E2E coverage for finding P2-9 — unknown query parameters on the admin list routes.
//!
//! # Why a per-endpoint test and not one
//!
//! All twelve `GET /api/v1/admin/*` list routes deserialize the *same* `PageQuery`
//! (`src/domain/admin.rs`), so it is tempting to test one route and call the property
//! proven. That is exactly the assumption this file refuses to make: a future
//! per-endpoint query type (the documented follow-up to P2-9) would move routes off
//! `PageQuery` one at a time, and a single-route test would stay green while eleven
//! routes silently stopped rejecting anything. The rejection is asserted route by
//! route, and the route list is asserted to be complete.
//!
//! # Two properties, deliberately separated
//!
//! 1. **A parameter absent from the struct is rejected.** That is what
//!    `#[serde(deny_unknown_fields)]` buys, and it is the finding's actual subject.
//! 2. **A parameter present on the struct but meaningless to the endpoint is
//!    accepted and ignored.** `credential_type` exists for the credentials list and is
//!    accepted, unfiltered, by the applications list. This is *not* a bug report — it
//!    is the documented consequence of one shared query type, pinned here so that
//!    changing it is a deliberate, visible decision rather than an accident.
//!
//! # No database is needed for the rejection tests, on purpose
//!
//! `Query<PageQuery>` is the last extractor in every list handler's signature, and
//! authentication happens *inside* the handler body (`admin_actor`). A malformed query
//! string therefore never reaches the handler, never reaches `AppState::pool()`, and
//! never reaches the database — so the rejection tests run against a poolless router
//! and cannot be silently skipped when `MOIRA_TEST_DATABASE_URL` is unset. Only the
//! `200`-returning ignore test needs Postgres, and it uses the shared
//! `tests/support` fixture with that suite's usual skip behaviour.
//!
//! # A gap this file records rather than papers over
//!
//! `plans/06-architecture-test-hygiene.md` module 11 asks for the rejection to carry a
//! non-empty `message_key` **and** `message` (CONVENTIONS §4.5). It does not. axum's
//! `QueryRejection` is a bare `text/plain` `400` that never passes through `AppError`,
//! and `normalize_infrastructure_error` (`src/lib.rs`) only rewrites `413` and `504`.
//! Converting it would be an observable change to the wire contract, which plan 06's
//! premise explicitly excludes — so
//! `unknown_query_field_rejection_is_plain_text_and_precedes_authentication` records
//! the shape that actually ships, including the two things about it that are worth a
//! decision: no envelope, and no authentication.

mod support;

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use moira::{
    app::AppState, application::AdminService, build_router, config::Settings,
    domain::ApplicationCreateRequest,
};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

use support::LifecycleFixture;

/// Every `GET /api/v1/admin/*` route that takes `Query<PageQuery>`, with the path
/// parameters filled in by a syntactically valid value. The ids are random and match
/// nothing: a query-string rejection is decided before the handler looks anything up,
/// so a `404` here would itself be a finding.
fn admin_list_paths() -> Vec<(&'static str, String)> {
    let provider_id = Uuid::now_v7();
    vec![
        (
            "list_applications",
            "/api/v1/admin/applications".to_string(),
        ),
        ("list_providers", "/api/v1/admin/providers".to_string()),
        (
            "list_provider_models",
            format!("/api/v1/admin/providers/{provider_id}/models"),
        ),
        (
            "list_credentials",
            "/api/v1/admin/provider-credentials".to_string(),
        ),
        (
            "list_user_credentials",
            "/api/v1/admin/users/query-contract-user/provider-credentials".to_string(),
        ),
        ("list_system_keys", "/api/v1/admin/system-keys".to_string()),
        (
            "list_consumer_keys",
            "/api/v1/admin/consumer-keys".to_string(),
        ),
        (
            "list_trusted_jwt_issuers",
            "/api/v1/admin/jwt-issuers".to_string(),
        ),
        (
            "list_audit_events",
            "/api/v1/admin/audit-events".to_string(),
        ),
        ("list_route_definitions", "/api/v1/admin/routes".to_string()),
        (
            "list_routing_policies",
            "/api/v1/admin/routing-policies".to_string(),
        ),
        (
            "list_agent_profiles",
            "/api/v1/admin/agent-profiles".to_string(),
        ),
    ]
}

struct HttpResult {
    status: StatusCode,
    content_type: Option<String>,
    body: Vec<u8>,
}

impl HttpResult {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|error| panic!("expected a JSON body, got {:?}: {error}", self.text()))
    }
}

async fn get(router: &Router, path: &str) -> HttpResult {
    let request = Request::builder()
        .method("GET")
        .uri(path)
        .header("x-request-id", format!("query-contract-{}", Uuid::now_v7()))
        .body(Body::empty())
        .expect("GET request");
    let response = router
        .clone()
        .oneshot(request)
        .await
        .expect("HTTP response");
    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let body = to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("response body")
        .to_vec();
    HttpResult {
        status,
        content_type,
        body,
    }
}

fn poolless_router() -> Router {
    let state = AppState::new(Settings::default(), None).expect("app state without a pool");
    build_router(state).expect("router")
}

/// The finding itself, asserted on every list route rather than a representative one.
#[tokio::test]
async fn each_admin_list_endpoint_rejects_an_unknown_query_field() {
    let router = poolless_router();
    let paths = admin_list_paths();
    assert_eq!(
        paths.len(),
        12,
        "the admin surface has twelve Query<PageQuery> list routes; update this list \
         (and this count) when one is added or removed, or the new route ships untested"
    );

    for (handler, path) in &paths {
        let result = get(&router, &format!("{path}?not_a_real_field=1")).await;
        assert_eq!(
            result.status,
            StatusCode::BAD_REQUEST,
            "{handler} ({path}) accepted a query parameter that is not on PageQuery; \
             body was {:?}",
            result.text()
        );
    }
}

/// The control for the test above. Without it, a router that answered `400` to
/// *everything* — a broken route table, a mis-ordered layer — would look like proof
/// that `deny_unknown_fields` works.
#[tokio::test]
async fn each_admin_list_endpoint_accepts_the_same_request_without_the_unknown_field() {
    let router = poolless_router();

    for (handler, path) in &admin_list_paths() {
        let result = get(&router, path).await;
        assert_ne!(
            result.status,
            StatusCode::BAD_REQUEST,
            "{handler} ({path}) returned 400 with no query string at all, so the \
             rejection test above proves nothing about the unknown field"
        );
        // Poolless, so the handler fails at `AppState::pool()` — a 503 carrying the
        // standard envelope. The point is that it *reached* the handler.
        assert_eq!(
            result.status,
            StatusCode::SERVICE_UNAVAILABLE,
            "{handler} ({path}) should have reached the handler and failed on the \
             absent database; got {:?}",
            result.text()
        );
        assert_eq!(
            result.json()["error"]["code"],
            "database_unavailable",
            "{handler} ({path}) must still answer with the standard error envelope"
        );
    }
}

/// **Recorded gap, not an endorsement.**
///
/// `plans/06-architecture-test-hygiene.md` module 11 and its Definition of Done ask
/// for the unknown-query-field rejection to carry a non-empty `message_key` and
/// `message`. It does not, and this test pins exactly why so the claim is not lost:
///
/// * The body is `text/plain`, produced by axum's `QueryRejection`. It never passes
///   through `AppError`, so it has no `code`, no `message_key`, no `request_id`.
///   `normalize_infrastructure_error` (`src/lib.rs`) rewrites only `413` and `504`.
/// * The rejection happens in the extractor, i.e. **before** `admin_actor`. An
///   unauthenticated caller therefore receives the rejection — and axum's message
///   enumerates all 26 `PageQuery` field names, which is a (minor) disclosure of the
///   admin filter surface to an anonymous caller.
///
/// Fixing either would change the wire contract, which plan 06 excludes by premise.
/// This test is the tripwire: it goes red the day someone does fix it, which is when
/// the module 11 Definition-of-Done box can honestly be ticked.
#[tokio::test]
async fn unknown_query_field_rejection_is_plain_text_and_precedes_authentication() {
    let router = poolless_router();
    let result = get(&router, "/api/v1/admin/applications?not_a_real_field=1").await;

    assert_eq!(result.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        result.content_type.as_deref(),
        Some("text/plain; charset=utf-8"),
        "if this is now JSON the envelope gap has been closed — replace this test with \
         the message_key/message assertions plan 06 module 11 asked for"
    );

    let body = result.text();
    assert!(
        body.contains("not_a_real_field"),
        "the rejection should name the offending field, got: {body}"
    );
    assert!(
        serde_json::from_str::<Value>(&body).is_err(),
        "the rejection body is not an error envelope; got parseable JSON: {body}"
    );
    // No credential of any kind was sent, and the answer is still 400 rather than 401:
    // the extractor runs ahead of `admin_actor`.
    assert!(
        body.contains("credential_type") && body.contains("occurred_after"),
        "recorded disclosure: the rejection enumerates PageQuery's field names to an \
         unauthenticated caller. Got: {body}"
    );
}

/// The documented nuance from `PageQuery`'s doc comment, over real HTTP against real
/// Postgres: a field that exists on the struct is accepted by an endpoint that has no
/// use for it, and does not filter anything.
#[tokio::test]
async fn defined_but_unsupported_page_query_field_is_accepted_and_ignored() {
    let Some(fixture) = LifecycleFixture::new().await else {
        eprintln!(
            "skipping defined_but_unsupported_page_query_field_is_accepted_and_ignored: \
             MOIRA_TEST_DATABASE_URL is not set"
        );
        return;
    };
    let router = build_router(fixture.state.clone()).expect("router");

    // The anchor below needs a second page to exist, and `LifecycleFixture` creates
    // exactly one application. Seeding a second here makes the test self-sufficient on
    // an otherwise empty database instead of depending on rows a neighbouring suite
    // happened to leave behind. It is deleted again at the end of the test.
    let suffix = Uuid::now_v7().simple().to_string();
    let seeded = AdminService::new(&fixture.state)
        .expect("admin service")
        .create_application(
            &fixture.actor,
            &support::request_context(),
            ApplicationCreateRequest {
                external_application_id: Some(format!("query-contract-{suffix}")),
                application_slug: Some(format!("query-contract-{suffix}")),
                display_name: format!("Query contract {suffix}"),
                metadata: json!({ "test_fixture": true }),
            },
        )
        .await
        .expect("seed a second application");

    // Every suite in this repository shares one database and they run concurrently,
    // so comparing two unpinned first pages would compare two different result sets
    // whenever a neighbouring suite inserted an application between the round trips.
    // Both comparisons below are therefore anchored to a keyset cursor: the list is
    // ordered `created_at desc, id desc`, so rows *after* a cursor are strictly older
    // than it and a concurrent insert can never enter the window.
    let first_page = get(&router, "/api/v1/admin/applications?limit=1").await;
    assert_eq!(first_page.status, StatusCode::OK);
    let anchor = first_page.json()["pagination"]["next_cursor"]
        .as_str()
        .map(str::to_string)
        .expect(
            "the fixture created an application, so the list must have a second page \
             to anchor on",
        );

    // `credential_type` belongs to GET /api/v1/admin/provider-credentials. The
    // applications list neither uses nor rejects it.
    let filtered = get(
        &router,
        &format!("/api/v1/admin/applications?cursor={anchor}&limit=5&credential_type=api_key"),
    )
    .await;
    assert_eq!(
        filtered.status,
        StatusCode::OK,
        "a field defined on PageQuery must be accepted by every list route; got {:?}",
        filtered.text()
    );

    let unfiltered = get(
        &router,
        &format!("/api/v1/admin/applications?cursor={anchor}&limit=5"),
    )
    .await;
    assert_eq!(unfiltered.status, StatusCode::OK);

    // "Ignored" is the claim, so compare the rows rather than merely the status: a
    // parameter that silently changed the result set would still be a 200.
    assert_eq!(
        filtered.json()["data"],
        unfiltered.json()["data"],
        "credential_type must not filter the applications list — it is accepted and \
         ignored, which is the P2-9 nuance recorded on PageQuery"
    );

    // And the page is non-empty, so the comparison above is not two identical empty
    // pages agreeing about nothing.
    let rows = filtered.json()["data"]
        .as_array()
        .expect("data array")
        .len();
    assert!(
        rows > 0,
        "the anchored page came back empty even though the anchor promised more rows"
    );

    sqlx::query("delete from applications where id = $1")
        .bind(seeded.id)
        .execute(&fixture.pool)
        .await
        .expect("remove the seeded application");
}
