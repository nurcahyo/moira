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
//! # The gap this file used to record, now closed (F2)
//!
//! `plans/06-architecture-test-hygiene.md` module 11 asked for the rejection to carry a
//! non-empty `message_key` **and** `message` (CONVENTIONS §4.5). Until F2 it did not:
//! axum's `QueryRejection` was a bare `text/plain` `400` that never passed through
//! `AppError`, and `normalize_infrastructure_error` (`src/lib.rs`) rewrote only `413`
//! and `504`. It now rewrites every non-JSON client- and server-error response, so the
//! rejection carries the standard envelope and says nothing about the query surface.
//!
//! `unknown_query_field_rejection_carries_the_error_envelope_and_enumerates_nothing`
//! (formerly `unknown_query_field_rejection_is_plain_text_and_precedes_authentication`,
//! which pinned the shipped defect) is the guard for both halves.

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
    get_with_headers(router, path, &[]).await
}

async fn get_with_headers(router: &Router, path: &str, headers: &[(&str, &str)]) -> HttpResult {
    let mut builder = Request::builder()
        .method("GET")
        .uri(path)
        .header("x-request-id", format!("query-contract-{}", Uuid::now_v7()));
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder.body(Body::empty()).expect("GET request");
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

async fn poolless_router() -> Router {
    let state = AppState::new(Settings::default(), None)
        .await
        .expect("app state without a pool");
    build_router(state).expect("router")
}

/// The finding itself, asserted on every list route rather than a representative one.
///
/// The envelope assertions were added when F2 closed. `plans/06-architecture-test-hygiene.md`
/// module 11 asked for "`400` plus a well-formed error envelope carrying a non-empty
/// `message_key` **and** `message`" here; until the rejection went through `AppError` only
/// the status could be checked, and the Definition-of-Done box could not honestly be ticked.
/// They live on *this* test, across all twelve routes, rather than only on the single-route
/// F2 guard below — the mapper is global, but the twelve routes are what a per-endpoint
/// query type would move off it one at a time.
#[tokio::test]
async fn each_admin_list_endpoint_rejects_an_unknown_query_field() {
    let router = poolless_router().await;
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
        let body = result.json();
        let error = &body["error"];
        assert_eq!(
            error["code"], "invalid_request",
            "{handler} ({path}) must reject with the standard envelope: {body}"
        );
        let message_key = error["message_key"]
            .as_str()
            .unwrap_or_else(|| panic!("{handler} ({path}) has no message_key: {body}"));
        assert!(
            !message_key.is_empty() && moira::i18n::is_known_key(message_key),
            "{handler} ({path}): {message_key:?} is empty or not in the i18n catalog"
        );
        assert!(
            !error["message"]
                .as_str()
                .unwrap_or_else(|| panic!("{handler} ({path}) has no message: {body}"))
                .is_empty(),
            "{handler} ({path}) must carry a non-empty message"
        );
        assert!(
            !error["request_id"]
                .as_str()
                .unwrap_or_else(|| panic!("{handler} ({path}) has no request_id: {body}"))
                .is_empty(),
            "{handler} ({path}) must carry a request_id"
        );
        assert!(
            !result.text().contains("not_a_real_field"),
            "{handler} ({path}) echoes the caller's field name back: {}",
            result.text()
        );
    }
}

/// The control for the test above. Without it, a router that answered `400` to
/// *everything* — a broken route table, a mis-ordered layer — would look like proof
/// that `deny_unknown_fields` works.
#[tokio::test]
async fn each_admin_list_endpoint_accepts_the_same_request_without_the_unknown_field() {
    let router = poolless_router().await;

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

/// Every `in: query` parameter name the committed OpenAPI document publishes for
/// `GET /api/v1/admin/applications` — i.e. `PageQuery`'s field names, read from the
/// artifact rather than retyped.
///
/// Deriving the list this way is the point: a hand-written list of 26 strings would
/// stop covering field 27 the moment somebody added one, and nothing would say so.
/// `docs/openapi.json` is generated from `PageQuery` itself and is gated against
/// drift by `committed_openapi_matches_the_generated_document`, so it cannot fall
/// behind the struct.
fn documented_query_parameter_names() -> Vec<String> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/openapi.json");
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    let document: Value = serde_json::from_str(&raw).expect("docs/openapi.json is JSON");
    let names: Vec<String> = document["paths"]["/api/v1/admin/applications"]["get"]["parameters"]
        .as_array()
        .expect("the applications list operation documents its parameters")
        .iter()
        .filter(|parameter| parameter["in"] == "query")
        .map(|parameter| {
            parameter["name"]
                .as_str()
                .expect("every parameter has a name")
                .to_string()
        })
        .collect();

    // Vacuity guard. A selector that matched nothing would make the enumeration
    // assertion below pass against a response that leaked every field name.
    assert!(
        names.len() >= 26,
        "expected at least PageQuery's 26 query parameters in docs/openapi.json, found \
         {}: {names:?} — the selector has drifted and the enumeration oracle below is \
         asserting nothing",
        names.len()
    );
    for expected in ["limit", "cursor", "credential_type", "occurred_after"] {
        assert!(
            names.iter().any(|name| name == expected),
            "{expected:?} is a PageQuery field but is missing from the derived list: {names:?}"
        );
    }
    names
}

/// Collects every string *value* in a JSON document, at any depth, ignoring object
/// keys.
///
/// Keys are excluded on purpose. `request_id` is both an envelope field name and a
/// `PageQuery` field name, so a substring check over the raw body would report a leak
/// for the envelope's own structural key on every single response. Values are where a
/// leak would actually land, and collecting them recursively keeps the oracle correct
/// if `ErrorDetail` ever grows a field.
fn json_string_values(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(text) => out.push(text.clone()),
        Value::Array(items) => items.iter().for_each(|item| json_string_values(item, out)),
        Value::Object(fields) => fields
            .values()
            .for_each(|field| json_string_values(field, out)),
        _ => {}
    }
}

/// **F2, closed.** The guard that replaces
/// `unknown_query_field_rejection_is_plain_text_and_precedes_authentication`, which
/// pinned the shipped defect: a `400 text/plain` with no `code`, no `message_key` and
/// no `request_id`, whose body enumerated all 26 `PageQuery` field names to a caller
/// that had presented no credential.
///
/// Three properties, and the third is the one that matters:
///
/// 1. **The envelope.** The rejection now goes through `AppError`, so it carries the
///    same `code` / `message_key` / `message` / `request_id` as every other error —
///    which is what `docs/openapi.json` already claimed for `4XX` on this operation
///    the whole time it was untrue.
/// 2. **It is still pre-authentication, deliberately.** No credential is sent here and
///    the answer is `400`, not `401`: `Query` is the last extractor and `admin_actor`
///    runs inside the handler, so the rejection precedes authentication. That is
///    recorded rather than fixed — see the decision note in `plans/reports/`. It is
///    acceptable *because of* property 3: the response is now identical for every
///    caller and every malformed query, so reaching it early reveals nothing that
///    `docs/openapi.json` does not already publish.
/// 3. **It enumerates nothing.** Asserted two ways, because a fix that closed only the
///    half its author imagined would pass either one alone: no documented query
///    parameter name appears in any value of the response, *and* the response does not
///    vary with the caller's input.
#[tokio::test]
async fn unknown_query_field_rejection_carries_the_error_envelope_and_enumerates_nothing() {
    let router = poolless_router().await;
    let documented = documented_query_parameter_names();

    // Random, so it cannot collide with a real field name and cannot be matched by a
    // hard-coded carve-out in the implementation.
    let probe = format!("zzzprobe{}", Uuid::now_v7().simple());
    assert!(
        !documented.iter().any(|name| name == &probe),
        "the probe field must not be a real PageQuery field, or the request would be \
         accepted and this test would assert nothing"
    );

    let result = get(&router, &format!("/api/v1/admin/applications?{probe}=1")).await;

    // 1 — the envelope.
    assert_eq!(
        result.status,
        StatusCode::BAD_REQUEST,
        "body: {}",
        result.text()
    );
    assert!(
        result
            .content_type
            .as_deref()
            .is_some_and(|value| value.trim_start().starts_with("application/json")),
        "the rejection must carry the JSON error envelope, got content-type {:?} and body \
         {:?}",
        result.content_type,
        result.text()
    );
    let body = result.json();
    let error = &body["error"];
    assert_eq!(
        error["code"], "invalid_request",
        "the rejection must carry a generic, catalogued code: {body}"
    );
    let message_key = error["message_key"]
        .as_str()
        .expect("message_key is a string");
    assert_eq!(message_key, "moira.error.invalid_request");
    // Checked against the catalog, not only against the literal above: a key that is
    // spelled consistently but absent from the catalog renders as nothing in a console.
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

    // 2 — still pre-authentication, and recorded as such. Asserted as *credential
    // independence* rather than as "not a 401": the 400 above already implies the latter,
    // so restating it would prove nothing. Sending a syntactically valid but wholly bogus
    // bearer token and getting byte-identical output (bar the request id) is what actually
    // shows the extractor decides before `admin_actor` ever looks at the header.
    let with_credential = get_with_headers(
        &router,
        &format!("/api/v1/admin/applications?{probe}=1"),
        &[("authorization", "Bearer not-a-real-token")],
    )
    .await;
    assert_eq!(
        with_credential.status,
        StatusCode::BAD_REQUEST,
        "a bogus credential changed the outcome, so the rejection is no longer decided \
         ahead of authentication. That is a deliberate design decision (recorded in \
         plans/reports/EXECUTION-LEDGER.md under F2) and must be re-recorded, not drifted \
         into: body {}",
        with_credential.text()
    );

    // 3a — no documented query parameter name survives into any value of the response.
    let mut values = Vec::new();
    json_string_values(&body, &mut values);
    assert!(
        !values.is_empty(),
        "no string values were collected from the envelope, so the check below is vacuous"
    );
    let haystack = values.join("\u{1f}");
    let leaked: Vec<&String> = documented
        .iter()
        .filter(|name| haystack.contains(name.as_str()))
        .collect();
    assert!(
        leaked.is_empty(),
        "the rejection enumerates {leaked:?} to an unauthenticated caller — F2 has \
         reopened. Response values: {values:?}"
    );
    assert!(
        !haystack.contains(&probe),
        "the rejection echoes the caller's own field name {probe:?} back: {values:?}"
    );

    // 3b — and the response does not vary with the caller's input at all. This catches
    // the partial fix that keeps the envelope but interpolates the rejection text into
    // `message`, which 3a would only catch for the *expected-fields* half of axum's
    // sentence.
    let other_probe = format!("zzzprobe{}", Uuid::now_v7().simple());
    let second = get(
        &router,
        &format!("/api/v1/admin/applications?{other_probe}=1"),
    )
    .await;
    let mut second_body = second.json();
    let mut first_body = body.clone();
    // `request_id` is the one field that legitimately differs per response.
    for document in [&mut first_body, &mut second_body] {
        document["error"]["request_id"] = Value::Null;
    }
    assert_eq!(
        first_body, second_body,
        "the rejection body varies with the caller's query string, so something from the \
         rejection is being echoed"
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
