//! E2E coverage for opaque keyset list pagination (plan 04, finding P1-4).
//!
//! # What this file is actually testing
//!
//! The bug P1-4 describes is not "one page comes back wrong" — a single-page test passes
//! happily against a completely broken cursor. The bugs keyset pagination introduces are
//! **skips** and **duplicates**, and both are only visible across a *full walk*. So every
//! pagination test here seeds more rows than fit in one page, walks the whole list through
//! `pagination.next_cursor`, and asserts the concatenation of every page equals the seeded
//! set exactly once, in the endpoint's documented order.
//!
//! The walk helper ([`walk_all`]) fails the moment a row is returned twice, which is
//! precisely the symptom of a `cursor` parameter that is accepted and then ignored.
//!
//! # Expected order is read back from the database, never assumed
//!
//! Each test asks Postgres for the order its endpoint documents (`order by created_at desc,
//! id desc`, `updated_at desc, id desc`, `occurred_at desc, id desc`, `sequence_number asc`)
//! and compares the HTTP walk against *that*. Hard-coding "reverse creation order" would
//! quietly pass if two rows shared a timestamp — the exact case the `id` tiebreaker exists
//! for, and the case the nine admin list queries got wrong before this plan.
//!
//! # Shared-database discipline
//!
//! Every suite in this repository runs against one database, and rows survive between runs.
//! Two rules keep this file honest:
//!
//! 1. **Every fixture-owned identifier carries a per-fixture `Uuid`.** Nothing here can match
//!    a row left behind by an earlier run, so no test can sit permanently green on stale data.
//! 2. **Assertions are made on seeded ids, never on table-wide counts.** For the globally
//!    scoped admin lists the seeded rows are pushed to the head of the ordering with a future
//!    timestamp and the walk stops as soon as all of them have been seen; foreign rows are
//!    ignored for ordering but still counted for duplicate detection.
//!
//! Seeded rows are deleted at the end of each test rather than left to accumulate.
//!
//! # No `sleep()`
//!
//! There is none in this file, and none is needed: every test is a sequence of ordered
//! request/response round trips with no concurrency to interleave.
//!
//! Fail-closed behaviour is inherited verbatim from `tests/support/mod.rs` (`panic!` when
//! `CI=true` and `MOIRA_TEST_DATABASE_URL` is absent, matched on the **value** of `CI` per
//! `plans/CONVENTIONS.md` §3).

mod support;

use std::collections::{BTreeSet, HashSet};
use std::time::Duration;

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use chrono::{Duration as ChronoDuration, Utc};
use moira::{
    application::{AdminService, ConversationService, RuntimeAdminService},
    domain::{
        ApplicationCreateRequest, ConsumerKeyCreateRequest, MemoryPolicyPutRequest, PageQuery,
        RouteDefinitionCreateRequest, RouteSelectionStrategy,
    },
};
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio::time::timeout;
use tower::ServiceExt;
use uuid::Uuid;

use support::{LifecycleFixture, request_context};

const WAIT: Duration = Duration::from_secs(15);

/// Hard stop for a walk. Generous enough for any list this file seeds, small enough that a
/// cursor that never advances fails the test instead of hanging the suite.
const MAX_PAGES: usize = 64;

// ---------------------------------------------------------------------------
// HTTP plumbing
// ---------------------------------------------------------------------------

struct HttpResult {
    status: StatusCode,
    body: Value,
}

impl HttpResult {
    fn rows(&self) -> Vec<Value> {
        self.body["data"]
            .as_array()
            .unwrap_or_else(|| panic!("list response has no `data` array: {}", self.body))
            .clone()
    }

    fn pagination(&self) -> &Value {
        let object = self
            .body
            .as_object()
            .unwrap_or_else(|| panic!("expected a JSON object, got: {}", self.body));
        object
            .get("pagination")
            .unwrap_or_else(|| panic!("list response has no `pagination` object: {}", self.body))
    }

    fn has_more(&self) -> bool {
        self.pagination()["has_more"]
            .as_bool()
            .unwrap_or_else(|| panic!("`pagination.has_more` is not a bool: {}", self.body))
    }

    /// `None` for a JSON `null`, `Some` for a string. Panics on any other shape so a wrong
    /// type cannot be silently read as "no more pages".
    fn next_cursor(&self) -> Option<String> {
        let raw = self
            .pagination()
            .as_object()
            .and_then(|pagination| pagination.get("next_cursor"))
            .unwrap_or_else(|| panic!("`pagination.next_cursor` is missing: {}", self.body));
        match raw {
            Value::Null => None,
            Value::String(value) => Some(value.clone()),
            other => panic!("`pagination.next_cursor` is neither null nor a string: {other}"),
        }
    }

    fn error_field(&self, name: &str) -> &Value {
        self.body["error"]
            .as_object()
            .unwrap_or_else(|| panic!("expected an error envelope, got: {}", self.body))
            .get(name)
            .unwrap_or_else(|| panic!("error envelope has no `{name}`: {}", self.body))
    }
}

async fn get(router: &Router, path: &str, consumer_key: Option<&str>) -> HttpResult {
    let mut builder = Request::builder()
        .method("GET")
        .uri(path)
        .header("x-request-id", format!("pagination-{}", Uuid::now_v7()));
    if let Some(key) = consumer_key {
        builder = builder.header("x-consumer-key", key);
    }
    send(router, builder.body(Body::empty()).expect("GET request")).await
}

async fn post(router: &Router, path: &str, consumer_key: Option<&str>, body: Value) -> HttpResult {
    let mut builder = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .header("x-request-id", format!("pagination-{}", Uuid::now_v7()));
    if let Some(key) = consumer_key {
        builder = builder.header("x-consumer-key", key);
    }
    send(
        router,
        builder
            .body(Body::from(body.to_string()))
            .expect("POST request"),
    )
    .await
}

async fn send(router: &Router, request: Request<Body>) -> HttpResult {
    let response = timeout(WAIT, router.clone().oneshot(request))
        .await
        .expect("HTTP request timed out")
        .expect("HTTP response");
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

// ---------------------------------------------------------------------------
// Walking a list to the end
// ---------------------------------------------------------------------------

struct Walk {
    rows: Vec<Value>,
    pages: usize,
    /// True when the walk ended because the server said `has_more: false`.
    exhausted: bool,
    /// The `next_cursor` of the final page fetched.
    final_next_cursor: Option<String>,
}

impl Walk {
    fn ids(&self) -> Vec<String> {
        self.rows
            .iter()
            .map(|row| {
                row["id"]
                    .as_str()
                    .unwrap_or_else(|| panic!("listed row has no string `id`: {row}"))
                    .to_string()
            })
            .collect()
    }
}

/// Pages through `base_path` until the server reports no more rows, or until every id in
/// `stop_once_seen` has been returned (whichever comes first).
///
/// Two invariants are enforced on every page, because both are silent in a single-page test:
///
/// * **No row is ever returned twice.** A `cursor` that is accepted and ignored re-serves
///   page one forever; this is what catches it, on page two, with a message that names the
///   repeated row.
/// * **`has_more` and `next_cursor` agree.** A `has_more: true` with a null cursor strands
///   the caller; a cursor with `has_more: false` invites an extra request that returns
///   nothing.
async fn walk_all(
    router: &Router,
    base_path: &str,
    consumer_key: Option<&str>,
    limit: usize,
    stop_once_seen: &BTreeSet<String>,
) -> Walk {
    let mut rows: Vec<Value> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut cursor: Option<String> = None;
    let mut pages = 0_usize;

    loop {
        pages += 1;
        assert!(
            pages <= MAX_PAGES,
            "walk of {base_path} did not terminate within {MAX_PAGES} pages — the cursor is \
             probably not advancing"
        );

        let separator = if base_path.contains('?') { '&' } else { '?' };
        let mut path = format!("{base_path}{separator}limit={limit}");
        if let Some(cursor) = &cursor {
            path.push('&');
            path.push_str("cursor=");
            path.push_str(cursor);
        }

        let result = get(router, &path, consumer_key).await;
        assert_eq!(
            result.status,
            StatusCode::OK,
            "page {pages} of {base_path} failed: {}",
            result.body
        );

        let page = result.rows();
        assert!(
            page.len() <= limit,
            "page {pages} of {base_path} returned {} rows for limit={limit}",
            page.len()
        );

        for row in &page {
            let id = row["id"]
                .as_str()
                .unwrap_or_else(|| panic!("listed row has no string `id`: {row}"))
                .to_string();
            assert!(
                seen.insert(id.clone()),
                "row {id} was returned twice while paging {base_path} (page {pages}) — a \
                 duplicate means the cursor did not advance the keyset predicate"
            );
        }
        rows.extend(page);

        let has_more = result.has_more();
        let next_cursor = result.next_cursor();
        assert_eq!(
            has_more,
            next_cursor.is_some(),
            "page {pages} of {base_path} reported has_more={has_more} with next_cursor={:?}",
            next_cursor
        );

        if !has_more {
            return Walk {
                rows,
                pages,
                exhausted: true,
                final_next_cursor: next_cursor,
            };
        }

        if !stop_once_seen.is_empty() && stop_once_seen.iter().all(|id| seen.contains(id)) {
            return Walk {
                rows,
                pages,
                exhausted: false,
                final_next_cursor: next_cursor,
            };
        }

        cursor = next_cursor;
    }
}

/// Asserts the walk returned exactly `expected`, in order, once each.
///
/// Rows belonging to other suites (the admin lists are global) are filtered out first; the
/// duplicate check inside [`walk_all`] already covers them.
fn assert_seeded_order(walk: &Walk, expected: &[String], what: &str) {
    let expected_set: HashSet<&String> = expected.iter().collect();
    let observed: Vec<String> = walk
        .ids()
        .into_iter()
        .filter(|id| expected_set.contains(id))
        .collect();
    assert_eq!(
        observed,
        expected,
        "{what}: paging through returned the seeded rows in the wrong order, or skipped some \
         (walked {} pages, {} rows total)",
        walk.pages,
        walk.rows.len()
    );
}

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

struct PaginationFixture {
    inner: LifecycleFixture,
    router: Router,
    /// Per-fixture uniqueness suffix. Every identifier this file writes carries it, so no
    /// assertion can be satisfied by a row an earlier run left behind.
    suffix: String,
    consumer_key: String,
}

impl PaginationFixture {
    async fn new() -> Option<Self> {
        let inner = LifecycleFixture::new().await?;
        // Turns on the conversation policy the caller-facing tests need. The consumer key it
        // returns lacks `moira:conversations:read`, so this file mints its own below.
        inner.enable_public_streaming().await;
        // The memory list is gated by its own policy and answers `403 memory_disabled`
        // before it ever looks at a cursor. It is enabled here so the cross-endpoint cursor
        // test observes the cursor rejection rather than the policy gate.
        ConversationService::new(&inner.state)
            .expect("conversation service")
            .put_memory_policy(
                &inner.actor,
                &request_context(),
                inner.application_id,
                MemoryPolicyPutRequest {
                    enabled: Some(true),
                    user_can_list: Some(true),
                    ..MemoryPolicyPutRequest::default()
                },
            )
            .await
            .expect("enable memory policy for pagination tests");
        let router = moira::build_router(inner.state.clone()).expect("build Moira test router");
        let suffix = Uuid::now_v7().simple().to_string();
        let consumer_key = AdminService::new(&inner.state)
            .expect("admin service")
            .create_consumer_key(
                &inner.actor,
                &request_context(),
                ConsumerKeyCreateRequest {
                    application_id: inner.application_id,
                    display_name: format!("Pagination client {suffix}"),
                    scopes: vec![
                        "moira:conversations:create".to_string(),
                        "moira:conversations:read".to_string(),
                        "moira:conversations:write".to_string(),
                        "moira:memories:read".to_string(),
                    ],
                    expires_at: None,
                },
            )
            .await
            .expect("create pagination consumer key")
            .secret
            .expect("consumer key secret");

        let fixture = Self {
            inner,
            router,
            suffix,
            consumer_key,
        };
        fixture.purge_previous_runs().await;
        Some(fixture)
    }

    /// Deletes anything this file seeded on a previous run.
    ///
    /// The admin lists are global and this file deliberately dates its rows into the future
    /// so they sort first — which means a test that panics before its own cleanup would park
    /// permanent junk at the head of `/api/v1/admin/applications` for every later suite, and
    /// would eventually let a walk assertion be satisfied by leftovers instead of by the rows
    /// the test just wrote. Purging on setup makes that self-healing.
    ///
    /// Safe to run unconditionally: every predicate below is this file's own prefix, and the
    /// `LifecycleFixture` mutex means no other test in this binary is mid-flight. It installs
    /// no schema and no triggers — it is three ordinary `delete` statements.
    async fn purge_previous_runs(&self) {
        for statement in [
            "delete from audit_logs where resource_type = 'pagination_probe'",
            "delete from route_definitions where route_key like 'pag\\_%'",
            "delete from applications where application_slug like 'pag-%'",
        ] {
            sqlx::query(statement)
                .execute(self.pool())
                .await
                .unwrap_or_else(|error| panic!("purge `{statement}` failed: {error}"));
        }
    }

    fn pool(&self) -> &PgPool {
        &self.inner.pool
    }

    fn key(&self) -> Option<&str> {
        Some(self.consumer_key.as_str())
    }

    // -- applications -------------------------------------------------------

    /// Creates `count` applications and pins their `created_at`.
    ///
    /// `created_at` is set explicitly and in the **future** for two reasons: it puts the
    /// seeded rows at the head of the global `created_at desc` ordering so the walk does not
    /// have to traverse every row an earlier suite left behind, and it lets the tie test
    /// give several rows one identical timestamp. The `applications_bump_version` trigger
    /// rewrites `updated_at` and `version` on update but leaves `created_at` alone.
    async fn seed_applications(&self, count: usize, tied: bool) -> Vec<Uuid> {
        let admin = AdminService::new(&self.inner.state).expect("admin service");
        let mut ids = Vec::with_capacity(count);
        for nth in 0..count {
            let record = admin
                .create_application(
                    &self.inner.actor,
                    &request_context(),
                    ApplicationCreateRequest {
                        external_application_id: None,
                        application_slug: Some(format!("pag-{}-{nth}", self.suffix)),
                        display_name: format!("Pagination {} #{nth}", self.suffix),
                        metadata: json!({ "suite": "list_pagination" }),
                    },
                )
                .await
                .expect("create pagination application");
            ids.push(record.id);
        }

        let base = Utc::now() + ChronoDuration::days(3_650);
        for (nth, id) in ids.iter().enumerate() {
            let created_at = if tied {
                base
            } else {
                base + ChronoDuration::minutes(nth as i64)
            };
            sqlx::query("update applications set created_at = $2 where id = $1")
                .bind(id)
                .bind(created_at)
                .execute(self.pool())
                .await
                .expect("pin application created_at");
        }
        ids
    }

    async fn expected_application_order(&self, ids: &[Uuid]) -> Vec<String> {
        sqlx::query_scalar::<_, Uuid>(
            "select id from applications where id = any($1) and deleted_at is null \
             order by created_at desc, id desc",
        )
        .bind(ids)
        .fetch_all(self.pool())
        .await
        .expect("read documented application order")
        .into_iter()
        .map(|id| id.to_string())
        .collect()
    }

    async fn delete_applications(&self, ids: &[Uuid]) {
        sqlx::query("delete from applications where id = any($1)")
            .bind(ids)
            .execute(self.pool())
            .await
            .expect("clean up seeded applications");
    }

    // -- route definitions --------------------------------------------------

    async fn seed_route_definitions(&self, count: usize, tied: bool) -> Vec<Uuid> {
        let runtime = RuntimeAdminService::new(&self.inner.state).expect("runtime admin service");
        let mut ids = Vec::with_capacity(count);
        for nth in 0..count {
            let record = runtime
                .create_route_definition(
                    &self.inner.actor,
                    &request_context(),
                    RouteDefinitionCreateRequest {
                        route_key: format!("pag_{}_{nth}", self.suffix),
                        display_name: format!("Pagination route {} #{nth}", self.suffix),
                        description: None,
                        selection_strategy: RouteSelectionStrategy::Default,
                        agent_profile_id: None,
                        metadata: json!({ "suite": "list_pagination" }),
                    },
                )
                .await
                .expect("create pagination route definition");
            ids.push(record.id);
        }

        let base = Utc::now() + ChronoDuration::days(3_650);
        for (nth, id) in ids.iter().enumerate() {
            let created_at = if tied {
                base
            } else {
                base + ChronoDuration::minutes(nth as i64)
            };
            sqlx::query("update route_definitions set created_at = $2 where id = $1")
                .bind(id)
                .bind(created_at)
                .execute(self.pool())
                .await
                .expect("pin route definition created_at");
        }
        ids
    }

    async fn expected_route_order(&self, ids: &[Uuid]) -> Vec<String> {
        sqlx::query_scalar::<_, Uuid>(
            "select id from route_definitions where id = any($1) and deleted_at is null \
             order by created_at desc, id desc",
        )
        .bind(ids)
        .fetch_all(self.pool())
        .await
        .expect("read documented route order")
        .into_iter()
        .map(|id| id.to_string())
        .collect()
    }

    async fn delete_route_definitions(&self, ids: &[Uuid]) {
        sqlx::query("delete from route_definitions where id = any($1)")
            .bind(ids)
            .execute(self.pool())
            .await
            .expect("clean up seeded route definitions");
    }

    // -- audit events -------------------------------------------------------

    /// Audit rows are inserted directly: they are append-only by design and there is no write
    /// API for them. The read path — which is what pagination is about — is still driven over
    /// real HTTP.
    async fn seed_audit_events(&self, count: usize) -> Vec<Uuid> {
        let base = Utc::now() + ChronoDuration::days(3_650);
        let mut ids = Vec::with_capacity(count);
        for nth in 0..count {
            let id = sqlx::query_scalar::<_, Uuid>(
                r#"
                insert into audit_logs
                    (occurred_at, request_id, actor_type, resource_type, resource_id, action,
                     result, metadata)
                values ($1, $2, 'dev_admin', 'pagination_probe', $3, 'pagination.probe',
                        'success', '{"suite":"list_pagination"}'::jsonb)
                returning id
                "#,
            )
            .bind(base + ChronoDuration::minutes(nth as i64))
            .bind(format!("pag-audit-{}-{nth}", self.suffix))
            .bind(format!("pag-{}-{nth}", self.suffix))
            .fetch_one(self.pool())
            .await
            .expect("insert pagination audit event");
            ids.push(id);
        }
        ids
    }

    async fn expected_audit_order(&self, ids: &[Uuid]) -> Vec<String> {
        sqlx::query_scalar::<_, Uuid>(
            "select id from audit_logs where id = any($1) order by occurred_at desc, id desc",
        )
        .bind(ids)
        .fetch_all(self.pool())
        .await
        .expect("read documented audit order")
        .into_iter()
        .map(|id| id.to_string())
        .collect()
    }

    async fn delete_audit_events(&self, ids: &[Uuid]) {
        sqlx::query("delete from audit_logs where id = any($1)")
            .bind(ids)
            .execute(self.pool())
            .await
            .expect("clean up seeded audit events");
    }

    // -- conversations ------------------------------------------------------

    async fn seed_conversations(&self, count: usize) -> Vec<String> {
        let mut ids = Vec::with_capacity(count);
        for nth in 0..count {
            let created = post(
                &self.router,
                "/api/v1/conversations",
                self.key(),
                json!({
                    "title": format!("Pagination {} #{nth}", self.suffix),
                    "metadata": {"suite": "list_pagination"}
                }),
            )
            .await;
            assert_eq!(
                created.status,
                StatusCode::CREATED,
                "create conversation failed: {}",
                created.body
            );
            ids.push(
                created.body["id"]
                    .as_str()
                    .expect("conversation id")
                    .to_string(),
            );
        }
        ids
    }

    /// Forces groups of conversations onto one shared `updated_at`.
    ///
    /// `moira_bump_resource_version` rewrites `updated_at` to `now()` on every update, and
    /// `now()` is the *transaction* timestamp — so one statement touching several rows stamps
    /// them all identically. That is exactly the tie the `(updated_at, id)` keyset has to
    /// break, and it is produced by the production trigger rather than by the test writing a
    /// timestamp the trigger would have overwritten anyway.
    async fn tie_conversation_timestamps(&self, groups: &[&[String]]) {
        for group in groups {
            sqlx::query("update conversations set metadata = metadata where public_id = any($1)")
                .bind(group.to_vec())
                .execute(self.pool())
                .await
                .expect("tie conversation updated_at");
        }
    }

    async fn expected_conversation_order(&self, ids: &[String]) -> Vec<String> {
        sqlx::query_scalar::<_, String>(
            "select public_id from conversations where public_id = any($1) and deleted_at is null \
             order by updated_at desc, id desc",
        )
        .bind(ids)
        .fetch_all(self.pool())
        .await
        .expect("read documented conversation order")
    }

    async fn distinct_conversation_timestamps(&self, ids: &[String]) -> i64 {
        sqlx::query_scalar::<_, i64>(
            "select count(distinct updated_at) from conversations where public_id = any($1)",
        )
        .bind(ids)
        .fetch_one(self.pool())
        .await
        .expect("count distinct conversation timestamps")
    }

    async fn soft_delete_conversations(&self, ids: &[String]) {
        sqlx::query("update conversations set deleted_at = now() where public_id = any($1)")
            .bind(ids)
            .execute(self.pool())
            .await
            .expect("soft delete conversations");
    }

    // -- messages -----------------------------------------------------------

    async fn seed_messages(&self, conversation_id: &str, count: usize) -> Vec<String> {
        let mut ids = Vec::with_capacity(count);
        for nth in 0..count {
            let created = post(
                &self.router,
                &format!("/api/v1/conversations/{conversation_id}/messages"),
                self.key(),
                json!({
                    "role": "user",
                    "content": format!("pagination message {} #{nth}", self.suffix),
                    "metadata": {"suite": "list_pagination"}
                }),
            )
            .await;
            assert_eq!(
                created.status,
                StatusCode::CREATED,
                "create message failed: {}",
                created.body
            );
            ids.push(created.body["id"].as_str().expect("message id").to_string());
        }
        ids
    }
}

/// Flips one character of a cursor to a different character from the same base64url
/// alphabet, so the value still decodes as base64 and the failure has to come from the
/// integrity tag rather than from the decoder.
fn tamper(cursor: &str) -> String {
    let mut chars: Vec<char> = cursor.chars().collect();
    assert!(!chars.is_empty(), "cannot tamper with an empty cursor");
    let index = chars.len() / 2;
    chars[index] = if chars[index] == 'A' { 'B' } else { 'A' };
    let tampered: String = chars.into_iter().collect();
    assert_ne!(tampered, cursor, "tampering must change the cursor");
    tampered
}

fn assert_invalid_cursor(result: &HttpResult) {
    assert_eq!(
        result.status,
        StatusCode::BAD_REQUEST,
        "expected 400 for a bad cursor, got: {}",
        result.body
    );
    assert_eq!(result.error_field("code"), "invalid_cursor");

    let message_key = result
        .error_field("message_key")
        .as_str()
        .expect("message_key is a string");
    assert_eq!(message_key, "moira.error.invalid_cursor");
    // Asserted against the catalog itself, not against a string literal repeated here: a key
    // that is spelled right but missing from the catalog has no translation and would ship a
    // blank string to every non-English console.
    assert!(
        moira::i18n::is_known_key(message_key),
        "{message_key} is not in the response catalog"
    );
    assert!(
        !result
            .error_field("message")
            .as_str()
            .expect("message is a string")
            .is_empty(),
        "the invalid-cursor error must carry a non-empty default English message"
    );
}

// ---------------------------------------------------------------------------
// Admin lists — global, ordered by `created_at desc, id desc`
// ---------------------------------------------------------------------------

#[tokio::test]
async fn admin_application_list_pages_through_without_duplicates_or_gaps() {
    let Some(fixture) = PaginationFixture::new().await else {
        return;
    };

    let ids = fixture.seed_applications(7, false).await;
    let expected = fixture.expected_application_order(&ids).await;
    assert_eq!(
        expected.len(),
        7,
        "all seeded applications must be listable"
    );
    let stop: BTreeSet<String> = expected.iter().cloned().collect();

    let walk = walk_all(
        &fixture.router,
        "/api/v1/admin/applications",
        None,
        2,
        &stop,
    )
    .await;

    assert!(
        walk.pages > 1,
        "seeding 7 rows with limit=2 must require more than one page"
    );
    assert_seeded_order(&walk, &expected, "admin applications");

    fixture.delete_applications(&ids).await;
}

#[tokio::test]
async fn rows_tied_on_the_sort_timestamp_are_ordered_deterministically_by_id() {
    let Some(fixture) = PaginationFixture::new().await else {
        return;
    };

    // Six applications sharing one identical `created_at`. Before this plan the admin list
    // queries ordered by `created_at desc` alone, so these six had no defined relative order
    // at all and a page boundary could land anywhere among them — returning one row twice and
    // another never. This is the test that pins the `id desc` tiebreaker.
    let ids = fixture.seed_applications(6, true).await;
    let distinct = sqlx::query_scalar::<_, i64>(
        "select count(distinct created_at) from applications where id = any($1)",
    )
    .bind(&ids)
    .fetch_one(fixture.pool())
    .await
    .expect("count distinct created_at");
    assert_eq!(
        distinct, 1,
        "the tie test is worthless unless every seeded row shares one timestamp"
    );

    let expected = fixture.expected_application_order(&ids).await;
    let stop: BTreeSet<String> = expected.iter().cloned().collect();

    let first = walk_all(
        &fixture.router,
        "/api/v1/admin/applications",
        None,
        2,
        &stop,
    )
    .await;
    assert!(first.pages > 1, "6 tied rows at limit=2 must span pages");
    assert_seeded_order(&first, &expected, "tied admin applications, first walk");

    // Ordering must also be stable: the same walk repeated must produce the same sequence,
    // otherwise a caller resuming from a stored cursor sees a different result set.
    let second = walk_all(
        &fixture.router,
        "/api/v1/admin/applications",
        None,
        2,
        &stop,
    )
    .await;
    assert_seeded_order(&second, &expected, "tied admin applications, second walk");

    fixture.delete_applications(&ids).await;
}

/// Localises the two failing HTTP admin walks above — it is a diagnostic, not a substitute.
///
/// This drives `AdminService::list_applications` with a real [`PageQuery`], which is
/// everything the HTTP walk does **except** the handler line itself. It pages cleanly,
/// identical `created_at` and all, which proves the cursor codec, the keyset predicate, the
/// `id desc` tiebreaker and the `has_more`/`next_cursor` arithmetic are all correct for the
/// admin lists. What is not correct is the one thing this test skips: the nine handlers in
/// `src/http/admin.rs` call `list_*(&actor, query.limit())`, and the `i64` conversion has no
/// room for a cursor, so `PageQuery.cursor` is dropped before the service ever sees it.
///
/// Keep this test after the handlers are fixed: if it ever goes red while the HTTP walks stay
/// green, the fault has moved below the handler.
#[tokio::test]
async fn admin_application_pagination_is_correct_below_the_http_handler() {
    let Some(fixture) = PaginationFixture::new().await else {
        return;
    };

    let ids = fixture.seed_applications(6, true).await;
    let expected = fixture.expected_application_order(&ids).await;
    let expected_set: HashSet<String> = expected.iter().cloned().collect();

    let admin = AdminService::new(&fixture.inner.state).expect("admin service");
    let mut observed: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut cursor: Option<String> = None;
    let mut pages = 0_usize;

    loop {
        pages += 1;
        assert!(
            pages <= MAX_PAGES,
            "the service-level walk did not terminate"
        );

        let query = PageQuery {
            limit: Some(2),
            cursor: cursor.clone(),
            ..PageQuery::default()
        };
        let page = admin
            .list_applications(&fixture.inner.actor, &query)
            .await
            .expect("service-level application page");

        for record in &page.data {
            let id = record.id.to_string();
            assert!(
                seen.insert(id.clone()),
                "row {id} was returned twice by the service-level walk (page {pages})"
            );
            if expected_set.contains(&id) {
                observed.push(id);
            }
        }

        assert_eq!(
            page.pagination.has_more,
            page.pagination.next_cursor.is_some(),
            "has_more and next_cursor disagree on page {pages}"
        );
        if !page.pagination.has_more || expected_set.iter().all(|id| seen.contains(id)) {
            break;
        }
        cursor = page.pagination.next_cursor.clone();
    }

    assert!(pages > 1, "6 tied rows at limit=2 must span pages");
    assert_eq!(
        observed, expected,
        "the service layer paged the tied rows in the wrong order or skipped some"
    );

    fixture.delete_applications(&ids).await;
}

#[tokio::test]
async fn admin_audit_event_list_pages_through_by_occurred_at() {
    let Some(fixture) = PaginationFixture::new().await else {
        return;
    };

    // `audit_logs` is the one admin list keyed on `occurred_at`, not `created_at` — a cursor
    // built from the wrong column would page through a different key space entirely.
    let ids = fixture.seed_audit_events(7).await;
    let expected = fixture.expected_audit_order(&ids).await;
    assert_eq!(expected.len(), 7);
    let stop: BTreeSet<String> = expected.iter().cloned().collect();

    let walk = walk_all(
        &fixture.router,
        "/api/v1/admin/audit-events",
        None,
        2,
        &stop,
    )
    .await;

    assert!(walk.pages > 1, "7 rows at limit=2 must span pages");
    assert_seeded_order(&walk, &expected, "admin audit events");

    fixture.delete_audit_events(&ids).await;
}

// ---------------------------------------------------------------------------
// Runtime-admin lists — global, ordered by `created_at desc, id desc`
// ---------------------------------------------------------------------------

#[tokio::test]
async fn route_definition_list_pages_through_without_duplicates_or_gaps() {
    let Some(fixture) = PaginationFixture::new().await else {
        return;
    };

    let ids = fixture.seed_route_definitions(7, false).await;
    let expected = fixture.expected_route_order(&ids).await;
    assert_eq!(expected.len(), 7);
    let stop: BTreeSet<String> = expected.iter().cloned().collect();

    let walk = walk_all(&fixture.router, "/api/v1/admin/routes", None, 2, &stop).await;

    assert!(walk.pages > 1, "7 rows at limit=2 must span pages");
    assert_seeded_order(&walk, &expected, "route definitions");

    fixture.delete_route_definitions(&ids).await;
}

#[tokio::test]
async fn route_definitions_tied_on_created_at_are_ordered_deterministically_by_id() {
    let Some(fixture) = PaginationFixture::new().await else {
        return;
    };

    let ids = fixture.seed_route_definitions(6, true).await;
    let distinct = sqlx::query_scalar::<_, i64>(
        "select count(distinct created_at) from route_definitions where id = any($1)",
    )
    .bind(&ids)
    .fetch_one(fixture.pool())
    .await
    .expect("count distinct created_at");
    assert_eq!(
        distinct, 1,
        "the tie test is worthless unless every seeded row shares one timestamp"
    );

    let expected = fixture.expected_route_order(&ids).await;
    let stop: BTreeSet<String> = expected.iter().cloned().collect();

    let walk = walk_all(&fixture.router, "/api/v1/admin/routes", None, 2, &stop).await;

    assert!(walk.pages > 1, "6 tied rows at limit=2 must span pages");
    assert_seeded_order(&walk, &expected, "tied route definitions");

    fixture.delete_route_definitions(&ids).await;
}

// ---------------------------------------------------------------------------
// Conversation-domain lists
// ---------------------------------------------------------------------------

#[tokio::test]
async fn conversation_list_pages_through_by_updated_at_with_id_tiebreaker() {
    let Some(fixture) = PaginationFixture::new().await else {
        return;
    };

    let ids = fixture.seed_conversations(6).await;
    // Three groups of two. Each group shares one `updated_at`, so the walk exercises both
    // halves of the key: the timestamp orders the groups, `id desc` orders within a group.
    fixture
        .tie_conversation_timestamps(&[&ids[0..2], &ids[2..4], &ids[4..6]])
        .await;

    let distinct = fixture.distinct_conversation_timestamps(&ids).await;
    assert!(
        distinct < ids.len() as i64,
        "the seeded conversations must contain at least one timestamp tie, got {distinct} \
         distinct timestamps for {} rows",
        ids.len()
    );

    let expected = fixture.expected_conversation_order(&ids).await;
    assert_eq!(expected.len(), 6);
    let stop: BTreeSet<String> = expected.iter().cloned().collect();

    // The list is scoped to this fixture's application by the consumer key, so the walk sees
    // only these six rows and must terminate on its own.
    let walk = walk_all(
        &fixture.router,
        "/api/v1/conversations",
        fixture.key(),
        2,
        &stop,
    )
    .await;

    assert!(walk.pages > 1, "6 rows at limit=2 must span pages");
    assert!(
        walk.exhausted,
        "an application-scoped list must run out of rows rather than page forever"
    );
    assert_seeded_order(&walk, &expected, "conversations");
    assert_eq!(
        walk.ids().len(),
        6,
        "the scoped walk must return exactly the seeded rows and nothing else"
    );
}

#[tokio::test]
async fn conversation_message_list_pages_through_by_ascending_sequence_number() {
    let Some(fixture) = PaginationFixture::new().await else {
        return;
    };

    let conversation = fixture.seed_conversations(1).await.remove(0);
    let expected = fixture.seed_messages(&conversation, 7).await;

    // The message list is the one ASCENDING cursor in the codebase (`sequence_number > $n`).
    // An inverted comparison here returns an empty page two instead of the next rows, which a
    // single-page test cannot see.
    let stop: BTreeSet<String> = expected.iter().cloned().collect();
    let walk = walk_all(
        &fixture.router,
        &format!("/api/v1/conversations/{conversation}/messages"),
        fixture.key(),
        2,
        &stop,
    )
    .await;

    assert!(walk.pages > 1, "7 messages at limit=2 must span pages");
    assert!(walk.exhausted, "the message list must terminate");
    assert_seeded_order(&walk, &expected, "conversation messages");

    let sequences: Vec<i64> = walk
        .rows
        .iter()
        .map(|row| row["sequence_number"].as_i64().expect("sequence_number"))
        .collect();
    let mut ascending = sequences.clone();
    ascending.sort_unstable();
    assert_eq!(
        sequences, ascending,
        "the message list is documented ascending; the walk returned {sequences:?}"
    );
    assert_eq!(
        sequences.len(),
        sequences.iter().collect::<HashSet<_>>().len(),
        "sequence numbers must be unique within a conversation"
    );
}

#[tokio::test]
async fn last_page_reports_has_more_false_and_null_next_cursor() {
    let Some(fixture) = PaginationFixture::new().await else {
        return;
    };

    let ids = fixture.seed_conversations(5).await;
    let stop: BTreeSet<String> = ids.iter().cloned().collect();

    let walk = walk_all(
        &fixture.router,
        "/api/v1/conversations",
        fixture.key(),
        2,
        &stop,
    )
    .await;

    assert!(walk.exhausted, "the walk must reach a real last page");
    assert!(
        walk.final_next_cursor.is_none(),
        "the last page must carry a null next_cursor, got {:?}",
        walk.final_next_cursor
    );

    // A single page that already holds everything is the same contract, taken in one request.
    let single = get(
        &fixture.router,
        "/api/v1/conversations?limit=50",
        fixture.key(),
    )
    .await;
    assert_eq!(single.status, StatusCode::OK, "{}", single.body);
    assert_eq!(single.rows().len(), 5);
    assert!(!single.has_more());
    assert_eq!(single.next_cursor(), None);
}

// ---------------------------------------------------------------------------
// Cursor validation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn tampered_cursor_returns_400_invalid_cursor_with_i18n_keys() {
    let Some(fixture) = PaginationFixture::new().await else {
        return;
    };

    fixture.seed_conversations(3).await;

    let first = get(
        &fixture.router,
        "/api/v1/conversations?limit=1",
        fixture.key(),
    )
    .await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.body);
    let cursor = first
        .next_cursor()
        .expect("a first page of 1 of 3 rows must offer a next cursor");

    // Sanity: the untampered cursor is accepted, so the 400 below is attributable to the
    // tampering and not to the endpoint rejecting every cursor.
    let honest = get(
        &fixture.router,
        &format!("/api/v1/conversations?limit=1&cursor={cursor}"),
        fixture.key(),
    )
    .await;
    assert_eq!(
        honest.status,
        StatusCode::OK,
        "an untampered cursor must be accepted: {}",
        honest.body
    );

    let tampered = tamper(&cursor);
    let rejected = get(
        &fixture.router,
        &format!("/api/v1/conversations?limit=1&cursor={tampered}"),
        fixture.key(),
    )
    .await;
    assert_invalid_cursor(&rejected);

    // Garbage that is not even base64 must land on the same coded error, never a 500.
    let garbage = get(
        &fixture.router,
        "/api/v1/conversations?limit=1&cursor=not-a-real-cursor",
        fixture.key(),
    )
    .await;
    assert_invalid_cursor(&garbage);
}

#[tokio::test]
async fn cursor_from_a_different_list_endpoint_is_rejected_with_400_invalid_cursor() {
    let Some(fixture) = PaginationFixture::new().await else {
        return;
    };

    fixture.seed_conversations(3).await;

    let first = get(
        &fixture.router,
        "/api/v1/conversations?limit=1",
        fixture.key(),
    )
    .await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.body);
    let cursor = first.next_cursor().expect("conversation cursor");

    // `/api/v1/memories` pages on the same `(updated_at, id)` shape, so nothing about the
    // payload distinguishes the two — only the per-endpoint scope mixed into the integrity
    // tag does. Without it this request would silently return a page of the wrong list.
    let crossed = get(
        &fixture.router,
        &format!("/api/v1/memories?limit=1&cursor={cursor}"),
        fixture.key(),
    )
    .await;
    assert_invalid_cursor(&crossed);

    // The same property on the admin surface, where two lists share both the key shape and
    // the record type's `id` column.
    let route_ids = fixture.seed_route_definitions(2, false).await;
    let routes = get(&fixture.router, "/api/v1/admin/routes?limit=1", None).await;
    assert_eq!(routes.status, StatusCode::OK, "{}", routes.body);
    let route_cursor = routes.next_cursor().expect("route cursor");

    let crossed_admin = get(
        &fixture.router,
        &format!("/api/v1/admin/agent-profiles?limit=1&cursor={route_cursor}"),
        None,
    )
    .await;
    assert_invalid_cursor(&crossed_admin);

    fixture.delete_route_definitions(&route_ids).await;
}

#[tokio::test]
async fn cursor_pointing_past_deleted_rows_returns_an_empty_final_page() {
    let Some(fixture) = PaginationFixture::new().await else {
        return;
    };

    let ids = fixture.seed_conversations(4).await;

    let first = get(
        &fixture.router,
        "/api/v1/conversations?limit=2",
        fixture.key(),
    )
    .await;
    assert_eq!(first.status, StatusCode::OK, "{}", first.body);
    assert!(first.has_more());
    let cursor = first.next_cursor().expect("next cursor");

    let returned: HashSet<String> = first
        .rows()
        .iter()
        .map(|row| row["id"].as_str().expect("conversation id").to_string())
        .collect();
    let remaining: Vec<String> = ids
        .into_iter()
        .filter(|id| !returned.contains(id))
        .collect();
    assert_eq!(remaining.len(), 2);

    // Everything the live cursor points at disappears between pages. Keyset pagination has a
    // defined answer for this — an empty final page — and it must not be a 500 or a repeat of
    // page one.
    fixture.soft_delete_conversations(&remaining).await;

    let second = get(
        &fixture.router,
        &format!("/api/v1/conversations?limit=2&cursor={cursor}"),
        fixture.key(),
    )
    .await;
    assert_eq!(
        second.status,
        StatusCode::OK,
        "a cursor past deleted rows must still be a 200: {}",
        second.body
    );
    assert!(
        second.rows().is_empty(),
        "expected an empty final page, got {}",
        second.body
    );
    assert!(!second.has_more());
    assert_eq!(second.next_cursor(), None);
}
