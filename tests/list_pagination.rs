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
//! ## What gets deleted, stated exactly
//!
//! Every test ends in [`PaginationFixture::cleanup`], which deletes **by id**: the rows the
//! test seeded, and the `LifecycleFixture` application and route the fixture itself created.
//! Deleting the application cascades to the conversations, messages, consumer key and policy
//! rows underneath it, and the audit rows are removed first because their `application_id` FK
//! is `on delete set null` and would otherwise survive detached.
//!
//! This file previously claimed seeded rows were "deleted at the end of each test", and the
//! claim was false. Measured across one green run against the shared test database, the tables
//! moved by `applications +20, route_definitions +20, conversations +22, conversation_messages
//! +7, audit_logs +187` — every run, forever; the database had reached ~51 000 audit rows.
//! Only the *admin-list* rows were ever deleted; the fixture's own application, its consumer
//! keys, every conversation and message written through the public API, and the entire audit
//! trail of all of it were not.
//!
//! The same measurement after this change is `+0` on all five tables. Re-run it whenever this
//! file grows a new seeder — the counts are the only thing that can tell you the sentence
//! above is still true.
//!
//! A test that **panics** still skips its own cleanup, so [`PaginationFixture::purge_stale_runs`]
//! runs at fixture setup as the backstop. Every predicate there is bounded to rows untouched
//! for ten minutes, so it can only ever reclaim a *previous* run's wreckage — a second copy of
//! this binary running concurrently against the same database has rows seconds old and is
//! never touched. An unbounded purge would make two concurrent copies delete each other's
//! fixtures, which is a self-inflicted flake rather than a bug worth finding.
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
use std::sync::Mutex;
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
    /// The field that identifies a row in this list. `"id"` for every admin list; the
    /// public lists name theirs differently (`response_id`, `execution_id`) and one of them
    /// has no `id` field at all, so it is a parameter rather than a constant.
    id_field: String,
}

impl Walk {
    fn ids(&self) -> Vec<String> {
        self.rows
            .iter()
            .map(|row| row_id(row, &self.id_field))
            .collect()
    }
}

fn row_id(row: &Value, id_field: &str) -> String {
    row[id_field]
        .as_str()
        .unwrap_or_else(|| panic!("listed row has no string `{id_field}`: {row}"))
        .to_string()
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
    walk_all_by(router, base_path, consumer_key, limit, stop_once_seen, "id").await
}

/// [`walk_all`] for a list whose rows are identified by something other than `id`.
async fn walk_all_by(
    router: &Router,
    base_path: &str,
    consumer_key: Option<&str>,
    limit: usize,
    stop_once_seen: &BTreeSet<String>,
    id_field: &str,
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
            let id = row_id(row, id_field);
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
                id_field: id_field.to_string(),
            };
        }

        if !stop_once_seen.is_empty() && stop_once_seen.iter().all(|id| seen.contains(id)) {
            return Walk {
                rows,
                pages,
                exhausted: false,
                final_next_cursor: next_cursor,
                id_field: id_field.to_string(),
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

/// Every row this fixture wrote, by primary key.
///
/// Tracked as ids rather than re-derived from a `like` pattern at teardown so that cleanup is
/// exact: it cannot match a row belonging to another run, and it cannot miss one whose slug
/// convention later changes.
#[derive(Default)]
struct Seeded {
    applications: Vec<Uuid>,
    route_definitions: Vec<Uuid>,
    audit_events: Vec<Uuid>,
}

struct PaginationFixture {
    inner: LifecycleFixture,
    router: Router,
    /// Per-fixture uniqueness suffix. Every identifier this file writes carries it, so no
    /// assertion can be satisfied by a row an earlier run left behind.
    suffix: String,
    consumer_key: String,
    seeded: Mutex<Seeded>,
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
                    // Issue #93's memory walk seeds its rows through `POST /v1/memories`,
                    // which is gated separately from listing them.
                    manual_memory_enabled: Some(true),
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
                        // Issue #93. The four public lists are consumer-key scoped, and the
                        // memory walk needs to write the rows it then pages through.
                        "moira:memories:create".to_string(),
                        "moira:executions:read".to_string(),
                        "moira:usage:read".to_string(),
                        "moira:models:read".to_string(),
                        "moira:routes:read".to_string(),
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
            seeded: Mutex::new(Seeded::default()),
        };
        fixture.purge_stale_runs().await;
        Some(fixture)
    }

    /// Deletes what a *previous*, panicking run of this file left behind.
    ///
    /// The admin lists are global and this file deliberately dates its rows into the future so
    /// they sort first — which means a test that panics before [`Self::cleanup`] parks junk at
    /// the head of `/api/v1/admin/applications` for every later suite, and would eventually let
    /// a walk assertion be satisfied by leftovers instead of by the rows the test just wrote.
    ///
    /// # Why every predicate carries a ten-minute floor
    ///
    /// The obvious version of this purge — "delete everything matching this file's prefix" —
    /// is unsafe the moment two copies of this binary run against one database, because each
    /// would delete the other's live fixtures at setup and both would fail on rows that
    /// vanished mid-test. `cargo test` never launches the same binary twice, but running it
    /// twice by hand is exactly what one does to hunt a flake, and a suite that breaks under
    /// that is a suite whose flakes cannot be investigated.
    ///
    /// So each predicate is bounded to rows nothing has touched for ten minutes. A concurrent
    /// run's rows are seconds old and are never matched; a crashed run's are reclaimed on the
    /// next invocation. `updated_at` is the right column for the bound because the version
    /// trigger stamps it with `now()` on the seed-time `update`, whereas `created_at` is
    /// deliberately dated 3 650 days into the future by the seeders and says nothing about age.
    /// Audit probes have no `updated_at`, so [`Self::seed_audit_events`] records the real wall
    /// clock in `metadata.seeded_at` for this predicate to read.
    ///
    /// It installs no schema and no triggers — these are four ordinary `delete` statements.
    async fn purge_stale_runs(&self) {
        for statement in [
            "delete from audit_logs where resource_type = 'pagination_probe' \
             and (metadata->>'seeded_at')::timestamptz < now() - interval '10 minutes'",
            "delete from conversations where metadata->>'suite' = 'list_pagination' \
             and updated_at < now() - interval '10 minutes'",
            "delete from route_definitions where route_key like 'pag\\_%' \
             and updated_at < now() - interval '10 minutes'",
            "delete from applications where application_slug like 'pag-%' \
             and updated_at < now() - interval '10 minutes'",
        ] {
            sqlx::query(statement)
                .execute(self.pool())
                .await
                .unwrap_or_else(|error| panic!("purge `{statement}` failed: {error}"));
        }
    }

    /// Deletes every row this fixture caused to exist, by id.
    ///
    /// Consumes the fixture so the `LifecycleFixture` serialisation guard is released only
    /// after the database is clean, and so a test cannot keep using a fixture whose rows are
    /// gone.
    ///
    /// Order is load-bearing. `audit_logs.application_id` is `on delete set null`, so audit
    /// rows have to go **before** the application they belong to or they survive as orphans
    /// with a null owner and nothing left to identify them by. The application goes last
    /// because deleting it cascades to the conversations, messages, consumer keys and policy
    /// rows this file created underneath it — which is why there is no separate statement for
    /// any of those.
    async fn cleanup(self) {
        let seeded = self
            .seeded
            .into_inner()
            .expect("no test holds the seeded lock across an await");

        let application_ids: Vec<Uuid> = seeded
            .applications
            .iter()
            .copied()
            .chain(std::iter::once(self.inner.application_id))
            .collect();
        let route_ids: Vec<Uuid> = seeded
            .route_definitions
            .iter()
            .copied()
            .chain(std::iter::once(self.inner.route_id))
            .collect();
        // `resource_id` is text and holds the uuid of whatever the audited action touched.
        let audited_resources: Vec<String> = application_ids
            .iter()
            .chain(route_ids.iter())
            .map(Uuid::to_string)
            .collect();

        let pool = self.inner.pool.clone();

        sqlx::query("delete from audit_logs where id = any($1)")
            .bind(&seeded.audit_events)
            .execute(&pool)
            .await
            .expect("clean up seeded audit events");
        // Everything this file wrote over HTTP: `get`/`post` stamp `x-request-id` with this
        // prefix, and no other suite uses it.
        sqlx::query("delete from audit_logs where request_id like 'pagination-%'")
            .execute(&pool)
            .await
            .expect("clean up audit rows from this file's HTTP requests");
        sqlx::query("delete from audit_logs where application_id = any($1)")
            .bind(&application_ids)
            .execute(&pool)
            .await
            .expect("clean up audit rows owned by this fixture's applications");
        sqlx::query("delete from audit_logs where resource_id = any($1)")
            .bind(&audited_resources)
            .execute(&pool)
            .await
            .expect("clean up audit rows naming this fixture's rows");
        // `consumer_key.create` audits name the key, not the application, and the keys
        // themselves are about to disappear with the application below — so they are resolved
        // here, while the join still exists. Two per test (the fixture mints one, and
        // `enable_public_streaming` mints another); without this they were the entire residue.
        sqlx::query(
            "delete from audit_logs where resource_type = 'consumer_api_key' \
             and resource_id in (select id::text from consumer_api_keys \
             where application_id = any($1))",
        )
        .bind(&application_ids)
        .execute(&pool)
        .await
        .expect("clean up audit rows for this fixture's consumer keys");

        sqlx::query("delete from route_definitions where id = any($1)")
            .bind(&route_ids)
            .execute(&pool)
            .await
            .expect("clean up seeded route definitions");
        sqlx::query("delete from applications where id = any($1)")
            .bind(&application_ids)
            .execute(&pool)
            .await
            .expect("clean up seeded applications");
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
        self.seeded
            .lock()
            .expect("seeded lock")
            .applications
            .extend(ids.iter().copied());
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
        self.seeded
            .lock()
            .expect("seeded lock")
            .route_definitions
            .extend(ids.iter().copied());
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

    // -- audit events -------------------------------------------------------

    /// Audit rows are inserted directly: they are append-only by design and there is no write
    /// API for them. The read path — which is what pagination is about — is still driven over
    /// real HTTP.
    ///
    /// `metadata.seeded_at` carries the real wall clock. `occurred_at` is dated 3 650 days
    /// forward so these rows lead the global `occurred_at desc` ordering, which leaves the row
    /// with no column that says how old it actually is — and [`Self::purge_stale_runs`] needs
    /// one to tell a previous run's leftovers from a concurrent run's live data.
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
                        'success',
                        jsonb_build_object('suite', 'list_pagination', 'seeded_at', now()))
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
        self.seeded
            .lock()
            .expect("seeded lock")
            .audit_events
            .extend(ids.iter().copied());
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

    // -- RAG documents ------------------------------------------------------

    /// Creates a collection and `count` documents in it, all over the admin HTTP surface.
    ///
    /// Both live under this fixture's application, so [`Self::cleanup`] reclaims them by
    /// cascade with no statement of their own.
    async fn seed_rag_documents(&self, count: usize) -> (String, Vec<String>) {
        let collection = post(
            &self.router,
            "/api/v1/admin/rag-collections",
            None,
            json!({
                "application_id": self.inner.application_id,
                "collection_key": format!("pag-rag-{}", self.suffix),
                "display_name": format!("Pagination RAG {}", self.suffix),
                "visibility": "application",
                "metadata": {"suite": "list_pagination"}
            }),
        )
        .await;
        assert_eq!(
            collection.status,
            StatusCode::CREATED,
            "create RAG collection failed: {}",
            collection.body
        );
        let collection_id = collection.body["id"]
            .as_str()
            .expect("collection id")
            .to_string();

        let mut ids = Vec::with_capacity(count);
        for nth in 0..count {
            let created = post(
                &self.router,
                &format!("/api/v1/admin/rag-collections/{collection_id}/documents"),
                None,
                json!({
                    "title": format!("Pagination document {} #{nth}", self.suffix),
                    "source_type": "direct_text",
                    "mime_type": "text/plain",
                    "content": format!("pagination rag body {nth}"),
                    "metadata": {"suite": "list_pagination"}
                }),
            )
            .await;
            assert_eq!(
                created.status,
                StatusCode::CREATED,
                "create RAG document failed: {}",
                created.body
            );
            ids.push(
                created.body["id"]
                    .as_str()
                    .expect("document id")
                    .to_string(),
            );
        }
        (collection_id, ids)
    }

    async fn expected_rag_document_order(&self, collection_id: &str) -> Vec<String> {
        sqlx::query_scalar::<_, String>(
            "select d.public_id from rag_documents d \
             join rag_collections c on c.id = d.collection_id \
             where c.public_id = $1 and d.deleted_at is null \
             order by d.created_at desc, d.id desc",
        )
        .bind(collection_id)
        .fetch_all(self.pool())
        .await
        .expect("read documented RAG document order")
    }

    // -- issue #93: the four public lists ----------------------------------
    //
    // All four are scoped to this fixture's application through the consumer key, so each
    // walk runs out of rows on its own and `exhausted` is asserted rather than a stop set.

    /// One timestamp per row, or one shared timestamp for every row when `tied`.
    ///
    /// The tied form is the case the `id` tiebreaker exists for. It is not hypothetical for
    /// these tables: `usage_records.occurred_at` defaults to `now()`, which is the
    /// *transaction* timestamp, so a burst of usage rows written together shares one value
    /// exactly.
    fn pinned_timestamp(
        base: chrono::DateTime<Utc>,
        nth: usize,
        tied: bool,
    ) -> chrono::DateTime<Utc> {
        if tied {
            base
        } else {
            base + ChronoDuration::minutes(nth as i64)
        }
    }

    /// `responses` rows, inserted directly.
    ///
    /// There is no public write path that produces an execution summary without a live
    /// provider, and pagination is a property of the read path. Returns the **response**
    /// ids, because `(created_at, responses.id)` is the key `/v1/executions` orders by —
    /// `execution_id` is a different column and would page through a different key space.
    async fn seed_executions(&self, count: usize, tied: bool) -> Vec<Uuid> {
        let base = Utc::now() + ChronoDuration::days(3_650);
        let mut ids = Vec::with_capacity(count);
        for nth in 0..count {
            let id = sqlx::query_scalar::<_, Uuid>(
                r#"
                insert into responses
                    (execution_id, request_id, application_id, status, created_at,
                     started_at, completed_at, usage_summary, metadata)
                values ($1, $2, $3, 'completed', $4, $4, $4, '{}'::jsonb,
                        jsonb_build_object('suite', 'list_pagination'))
                returning id
                "#,
            )
            .bind(Uuid::now_v7())
            .bind(format!("pag-exec-{}-{nth}", self.suffix))
            .bind(self.inner.application_id)
            .bind(Self::pinned_timestamp(base, nth, tied))
            .fetch_one(self.pool())
            .await
            .expect("insert pagination response");
            ids.push(id);
        }
        ids
    }

    /// The order `/v1/executions` documents, as the public `resp_…` identifiers it returns.
    async fn expected_execution_order(&self, ids: &[Uuid]) -> Vec<String> {
        sqlx::query_scalar::<_, Uuid>(
            "select id from responses where id = any($1) order by created_at desc, id desc",
        )
        .bind(ids)
        .fetch_all(self.pool())
        .await
        .expect("read documented execution order")
        .into_iter()
        .map(|id| format!("resp_{id}"))
        .collect()
    }

    /// `usage_records` rows, one per execution so `execution_id` identifies a row uniquely.
    async fn seed_usage_records(&self, count: usize, tied: bool) -> Vec<Uuid> {
        let base = Utc::now() + ChronoDuration::days(3_650);
        let mut ids = Vec::with_capacity(count);
        for nth in 0..count {
            let id = sqlx::query_scalar::<_, Uuid>(
                r#"
                insert into usage_records
                    (request_id, execution_id, application_id, input_tokens, output_tokens,
                     total_tokens, occurred_at, metadata)
                values ($1, $2, $3, 10, 20, 30, $4,
                        jsonb_build_object('suite', 'list_pagination'))
                returning id
                "#,
            )
            .bind(format!("pag-usage-{}-{nth}", self.suffix))
            .bind(Uuid::now_v7())
            .bind(self.inner.application_id)
            .bind(Self::pinned_timestamp(base, nth, tied))
            .fetch_one(self.pool())
            .await
            .expect("insert pagination usage record");
            ids.push(id);
        }
        ids
    }

    async fn expected_usage_order(&self, ids: &[Uuid]) -> Vec<String> {
        sqlx::query_scalar::<_, Uuid>(
            "select execution_id from usage_records where id = any($1) \
             order by occurred_at desc, id desc",
        )
        .bind(ids)
        .fetch_all(self.pool())
        .await
        .expect("read documented usage order")
        .into_iter()
        .map(|id| format!("exec_{id}"))
        .collect()
    }

    /// A provider, a model under it and an application-scoped routing policy per model.
    ///
    /// One provider each: `provider_models` is unique on `(provider_id, model_key)` among
    /// live rows, and reusing one provider would force distinct model keys for no gain.
    async fn seed_visible_models(&self, count: usize, tied: bool) -> Vec<Uuid> {
        let mut ids = Vec::with_capacity(count);
        for nth in 0..count {
            let provider_id = self.create_provider(nth).await;
            let model_id = self.create_provider_model(provider_id, nth).await;
            self.create_routing_policy(self.inner.route_id, provider_id, model_id, nth)
                .await;
            ids.push(model_id);
        }

        let base = Utc::now() + ChronoDuration::days(3_650);
        for (nth, id) in ids.iter().enumerate() {
            sqlx::query("update provider_models set created_at = $2 where id = $1")
                .bind(id)
                .bind(Self::pinned_timestamp(base, nth, tied))
                .execute(self.pool())
                .await
                .expect("pin provider model created_at");
        }
        ids
    }

    async fn expected_visible_model_order(&self, ids: &[Uuid]) -> Vec<String> {
        sqlx::query_scalar::<_, Uuid>(
            "select id from provider_models where id = any($1) and deleted_at is null \
             order by created_at desc, id desc",
        )
        .bind(ids)
        .fetch_all(self.pool())
        .await
        .expect("read documented visible model order")
        .into_iter()
        .map(|id| id.to_string())
        .collect()
    }

    /// Routes made visible to a consumer key, which requires an application-scoped routing
    /// policy on each — a bare `route_definitions` row is invisible to `/v1/routes`.
    async fn seed_visible_routes(&self, count: usize, tied: bool) -> Vec<Uuid> {
        let provider_id = self.create_provider(9_000).await;
        let model_id = self.create_provider_model(provider_id, 9_000).await;
        let ids = self.seed_route_definitions(count, tied).await;
        for (nth, route_id) in ids.iter().enumerate() {
            self.create_routing_policy(*route_id, provider_id, model_id, nth)
                .await;
        }
        ids
    }

    async fn create_provider(&self, nth: usize) -> Uuid {
        let created = post(
            &self.router,
            "/api/v1/admin/providers",
            None,
            json!({
                "provider_type": "open_ai_compatible",
                "display_name": format!("Pagination provider {} #{nth}", self.suffix),
                "metadata": {"suite": "list_pagination"}
            }),
        )
        .await;
        assert_eq!(
            created.status,
            StatusCode::CREATED,
            "create provider failed: {}",
            created.body
        );
        parse_uuid(&created.body, "provider id")
    }

    async fn create_provider_model(&self, provider_id: Uuid, nth: usize) -> Uuid {
        let created = post(
            &self.router,
            &format!("/api/v1/admin/providers/{provider_id}/models"),
            None,
            json!({
                "model_key": format!("pag-model-{}-{nth}", self.suffix),
                "display_name": format!("Pagination model #{nth}"),
                "capabilities": {"text": true, "streaming": true}
            }),
        )
        .await;
        assert_eq!(
            created.status,
            StatusCode::CREATED,
            "create provider model failed: {}",
            created.body
        );
        parse_uuid(&created.body, "provider model id")
    }

    async fn create_routing_policy(
        &self,
        route_id: Uuid,
        provider_id: Uuid,
        provider_model_id: Uuid,
        priority: usize,
    ) {
        let created = post(
            &self.router,
            "/api/v1/admin/routing-policies",
            None,
            json!({
                "application_id": self.inner.application_id,
                "route_id": route_id,
                "provider_id": provider_id,
                "provider_model_id": provider_model_id,
                "priority": priority as i64,
                "weight": 1,
                "metadata": {"suite": "list_pagination"}
            }),
        )
        .await;
        assert_eq!(
            created.status,
            StatusCode::CREATED,
            "create routing policy failed: {}",
            created.body
        );
    }

    // -- issue #93: the nine admin lists that had no walk test ---------------

    async fn seed_providers(&self, count: usize, tied: bool) -> Vec<Uuid> {
        let mut ids = Vec::with_capacity(count);
        for nth in 0..count {
            ids.push(self.create_provider(nth).await);
        }
        self.pin_created_at("providers", &ids, tied).await;
        ids
    }

    async fn seed_provider_models(&self, provider_id: Uuid, count: usize, tied: bool) -> Vec<Uuid> {
        let mut ids = Vec::with_capacity(count);
        for nth in 0..count {
            ids.push(self.create_provider_model(provider_id, nth).await);
        }
        self.pin_created_at("provider_models", &ids, tied).await;
        ids
    }

    /// Provider credentials, either global-scoped (the `/admin/provider-credentials` list)
    /// or bound to one external user (the `/admin/users/{id}/provider-credentials` list).
    async fn seed_credentials(
        &self,
        provider_id: Uuid,
        count: usize,
        external_user_id: Option<&str>,
        tied: bool,
    ) -> Vec<Uuid> {
        let mut ids = Vec::with_capacity(count);
        for nth in 0..count {
            let scope = match external_user_id {
                Some(user) => json!({"type": "user", "external_user_id": user}),
                None => json!({"type": "global"}),
            };
            let created = post(
                &self.router,
                "/api/v1/admin/provider-credentials",
                None,
                json!({
                    "provider_id": provider_id,
                    "credential_type": "api_key",
                    "scope": scope,
                    "secret": {"api_key": format!("sk-pagination-{}-{nth}", self.suffix)},
                    "display_name": format!("Pagination credential {} #{nth}", self.suffix),
                    "priority": 100,
                    "metadata": {"suite": "list_pagination"}
                }),
            )
            .await;
            assert_eq!(
                created.status,
                StatusCode::CREATED,
                "create provider credential failed: {}",
                created.body
            );
            ids.push(parse_uuid(&created.body, "credential id"));
        }
        self.pin_created_at("provider_credentials", &ids, tied)
            .await;
        ids
    }

    async fn seed_system_keys(&self, count: usize, tied: bool) -> Vec<Uuid> {
        let mut ids = Vec::with_capacity(count);
        for nth in 0..count {
            let created = post(
                &self.router,
                "/api/v1/admin/system-keys",
                None,
                json!({
                    "display_name": format!("Pagination system key {} #{nth}", self.suffix),
                    "scopes": ["moira:models:read"]
                }),
            )
            .await;
            assert_eq!(
                created.status,
                StatusCode::CREATED,
                "create system key failed: {}",
                created.body
            );
            ids.push(parse_uuid(&created.body, "system key id"));
        }
        self.pin_created_at("system_api_keys", &ids, tied).await;
        ids
    }

    async fn seed_consumer_keys(&self, count: usize, tied: bool) -> Vec<Uuid> {
        let mut ids = Vec::with_capacity(count);
        for nth in 0..count {
            let created = post(
                &self.router,
                "/api/v1/admin/consumer-keys",
                None,
                json!({
                    "application_id": self.inner.application_id,
                    "display_name": format!("Pagination consumer key {} #{nth}", self.suffix),
                    "scopes": ["moira:models:read"]
                }),
            )
            .await;
            assert_eq!(
                created.status,
                StatusCode::CREATED,
                "create consumer key failed: {}",
                created.body
            );
            ids.push(parse_uuid(&created.body, "consumer key id"));
        }
        self.pin_created_at("consumer_api_keys", &ids, tied).await;
        ids
    }

    /// Trusted JWT issuers.
    ///
    /// `example.com` subdomains are used deliberately: registration re-runs the JWKS URL
    /// policy, which treats a name that does not resolve as acceptable (availability, not
    /// security) but refuses anything resolving into denied address space. A loopback URL
    /// would be refused here even though the fixture allows insecure dev URLs elsewhere.
    async fn seed_trusted_jwt_issuers(&self, count: usize, tied: bool) -> Vec<Uuid> {
        let mut ids = Vec::with_capacity(count);
        for nth in 0..count {
            let created = post(
                &self.router,
                "/api/v1/admin/jwt-issuers",
                None,
                json!({
                    "issuer": format!("https://pag-{}-{nth}.example.com/", self.suffix),
                    "jwks_url": format!(
                        "https://pag-{}-{nth}.example.com/.well-known/jwks.json",
                        self.suffix
                    ),
                    "expected_audiences": ["moira"]
                }),
            )
            .await;
            assert_eq!(
                created.status,
                StatusCode::CREATED,
                "create trusted JWT issuer failed: {}",
                created.body
            );
            ids.push(parse_uuid(&created.body, "trusted JWT issuer id"));
        }
        self.pin_created_at("trusted_jwt_issuers", &ids, tied).await;
        ids
    }

    async fn seed_rag_collections(&self, count: usize, tied: bool) -> Vec<String> {
        let mut public_ids = Vec::with_capacity(count);
        for nth in 0..count {
            let created = post(
                &self.router,
                "/api/v1/admin/rag-collections",
                None,
                json!({
                    "application_id": self.inner.application_id,
                    "collection_key": format!("pag-collection-{}-{nth}", self.suffix),
                    "display_name": format!("Pagination collection {} #{nth}", self.suffix),
                    "visibility": "application",
                    "metadata": {"suite": "list_pagination"}
                }),
            )
            .await;
            assert_eq!(
                created.status,
                StatusCode::CREATED,
                "create RAG collection failed: {}",
                created.body
            );
            public_ids.push(
                created.body["id"]
                    .as_str()
                    .expect("collection id")
                    .to_string(),
            );
        }

        let base = Utc::now() + ChronoDuration::days(3_650);
        for (nth, public_id) in public_ids.iter().enumerate() {
            sqlx::query("update rag_collections set created_at = $2 where public_id = $1")
                .bind(public_id)
                .bind(Self::pinned_timestamp(base, nth, tied))
                .execute(self.pool())
                .await
                .expect("pin RAG collection created_at");
        }
        public_ids
    }

    async fn seed_memories(&self, count: usize, tied: bool) -> Vec<String> {
        let mut public_ids = Vec::with_capacity(count);
        for nth in 0..count {
            let created = post(
                &self.router,
                "/api/v1/memories",
                self.key(),
                json!({
                    "type": "fact",
                    "content": format!("pagination memory {} #{nth}", self.suffix),
                    "metadata": {"suite": "list_pagination"}
                }),
            )
            .await;
            assert_eq!(
                created.status,
                StatusCode::CREATED,
                "create memory failed: {}",
                created.body
            );
            public_ids.push(created.body["id"].as_str().expect("memory id").to_string());
        }

        // `/v1/memories` orders by `updated_at desc, id desc`, and the version trigger
        // rewrites `updated_at` on every update — so one statement touching several rows
        // stamps them all with the same transaction timestamp, which is exactly the tie.
        let mut groups: Vec<Vec<String>> = Vec::new();
        if tied {
            for pair in public_ids.chunks(2) {
                groups.push(pair.to_vec());
            }
        } else {
            for id in &public_ids {
                groups.push(vec![id.clone()]);
            }
        }
        for group in groups {
            sqlx::query("update memory_records set metadata = metadata where public_id = any($1)")
                .bind(&group)
                .execute(self.pool())
                .await
                .expect("stamp memory updated_at");
        }
        public_ids
    }

    async fn expected_memory_order(&self, ids: &[String]) -> Vec<String> {
        sqlx::query_scalar::<_, String>(
            "select public_id from memory_records where public_id = any($1) \
             and deleted_at is null order by updated_at desc, id desc",
        )
        .bind(ids)
        .fetch_all(self.pool())
        .await
        .expect("read documented memory order")
    }

    /// Rewrites `created_at` on rows this fixture just seeded, pushing them to the head of
    /// the global ordering and — when `tied` — onto one shared timestamp.
    ///
    /// The table name is interpolated, which is safe and checked: every caller passes a
    /// literal from the closed set below, and the ids are bound.
    async fn pin_created_at(&self, table: &str, ids: &[Uuid], tied: bool) {
        assert!(
            [
                "providers",
                "provider_models",
                "provider_credentials",
                "system_api_keys",
                "consumer_api_keys",
                "trusted_jwt_issuers",
            ]
            .contains(&table),
            "{table} is not one of the tables this helper is allowed to touch"
        );
        let base = Utc::now() + ChronoDuration::days(3_650);
        for (nth, id) in ids.iter().enumerate() {
            sqlx::query(&format!("update {table} set created_at = $2 where id = $1"))
                .bind(id)
                .bind(Self::pinned_timestamp(base, nth, tied))
                .execute(self.pool())
                .await
                .unwrap_or_else(|error| panic!("pin {table} created_at: {error}"));
        }
    }

    /// The order a global admin list documents, for a table keyed on `created_at`.
    async fn expected_created_at_order(&self, table: &str, ids: &[Uuid]) -> Vec<String> {
        let soft_deleted = table != "system_api_keys" && table != "consumer_api_keys";
        let predicate = if soft_deleted {
            "and deleted_at is null"
        } else {
            ""
        };
        sqlx::query_scalar::<_, Uuid>(&format!(
            "select id from {table} where id = any($1) {predicate} \
             order by created_at desc, id desc"
        ))
        .bind(ids)
        .fetch_all(self.pool())
        .await
        .unwrap_or_else(|error| panic!("read documented {table} order: {error}"))
        .into_iter()
        .map(|id| id.to_string())
        .collect()
    }

    async fn distinct_created_at(&self, table: &str, ids: &[Uuid]) -> i64 {
        sqlx::query_scalar::<_, i64>(&format!(
            "select count(distinct created_at) from {table} where id = any($1)"
        ))
        .bind(ids)
        .fetch_one(self.pool())
        .await
        .unwrap_or_else(|error| panic!("count distinct {table} created_at: {error}"))
    }
}

/// Reads the created row's `id` out of a create response.
///
/// Most admin creates return the record itself; the two key creates wrap it in `resource`
/// alongside the one-time secret, so both shapes are accepted rather than one of them being
/// silently read as a missing id.
fn parse_uuid(body: &Value, what: &str) -> Uuid {
    let raw = body["id"]
        .as_str()
        .or_else(|| body["resource"]["id"].as_str())
        .unwrap_or_else(|| panic!("{what} is missing from {body}"));
    Uuid::parse_str(raw).unwrap_or_else(|error| panic!("{what} is not a uuid: {raw} ({error})"))
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

    fixture.cleanup().await;
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

    fixture.cleanup().await;
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

    fixture.cleanup().await;
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

    fixture.cleanup().await;
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

    fixture.cleanup().await;
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

    fixture.cleanup().await;
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

    fixture.cleanup().await;
}

/// The tenth list endpoint: `GET /admin/rag-collections/{id}/documents`.
///
/// This route used to take **no query parameters at all**. Its handler passed a hard-coded
/// `limit = 50` and its `params(..)` declared only `collection_id` — yet the response ran the
/// same `paginate` as every other list, so a collection with more than fifty documents came
/// back with `has_more: true` and a real `next_cursor` that the route had no parameter to
/// accept. Document fifty-one was unreachable over HTTP, and the response said otherwise.
///
/// The walk below is the assertion that closes it: `limit=2` forces several page boundaries,
/// and reaching the end proves the `cursor` the response advertises is one the route will
/// actually take back. Before the fix this test cannot pass — page two is page one again, and
/// `walk_all` fails on the first repeated id.
#[tokio::test]
async fn rag_document_list_pages_through_the_cursor_it_advertises() {
    let Some(fixture) = PaginationFixture::new().await else {
        return;
    };

    let (collection_id, ids) = fixture.seed_rag_documents(7).await;
    let expected = fixture.expected_rag_document_order(&collection_id).await;
    assert_eq!(
        expected.len(),
        7,
        "all seeded documents must be listable, got {expected:?}"
    );
    assert_eq!(
        expected.iter().collect::<HashSet<_>>(),
        ids.iter().collect::<HashSet<_>>(),
        "the database order must contain exactly the seeded documents"
    );

    let stop: BTreeSet<String> = expected.iter().cloned().collect();
    let walk = walk_all(
        &fixture.router,
        &format!("/api/v1/admin/rag-collections/{collection_id}/documents"),
        None,
        2,
        &stop,
    )
    .await;

    assert!(walk.pages > 1, "7 documents at limit=2 must span pages");
    assert!(
        walk.exhausted,
        "the list is scoped to one collection and must run out of rows rather than page forever"
    );
    assert_seeded_order(&walk, &expected, "RAG documents");

    // The `limit` parameter has to be honoured too: the old handler ignored whatever the
    // caller asked for and always used 50, which a walk alone would not notice.
    let single = get(
        &fixture.router,
        &format!("/api/v1/admin/rag-collections/{collection_id}/documents?limit=3"),
        None,
    )
    .await;
    assert_eq!(single.status, StatusCode::OK, "{}", single.body);
    assert_eq!(
        single.rows().len(),
        3,
        "the route must honour `limit`, not its old hard-coded 50: {}",
        single.body
    );
    assert!(single.has_more());

    // And a bad cursor on this route must fail closed like every other list.
    let rejected = get(
        &fixture.router,
        &format!("/api/v1/admin/rag-collections/{collection_id}/documents?cursor=not-a-cursor"),
        None,
    )
    .await;
    assert_invalid_cursor(&rejected);

    fixture.cleanup().await;
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

    fixture.cleanup().await;
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

    fixture.cleanup().await;
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

    fixture.cleanup().await;
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
    fixture.seed_route_definitions(2, false).await;
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

    fixture.cleanup().await;
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

    fixture.cleanup().await;
}

// ---------------------------------------------------------------------------
// Issue #93 — the four public lists
//
// Before this change all four advertised a `cursor` and bound it to no SQL. `/v1/executions`
// and `/v1/usage` accepted the parameter and ignored it, so page two was page one again;
// `/v1/models` and `/v1/routes` had no `cursor` parameter at all and a hard-coded limit of
// 200, so row 201 was unreachable and the response said nothing about it. A single-page test
// passes against every one of those bugs — only a full walk does not.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn public_execution_list_pages_through_the_cursor_it_advertises() {
    let Some(fixture) = PaginationFixture::new().await else {
        return;
    };

    let ids = fixture.seed_executions(7, false).await;
    let expected = fixture.expected_execution_order(&ids).await;
    assert_eq!(expected.len(), 7, "all seeded executions must be listable");

    let stop: BTreeSet<String> = expected.iter().cloned().collect();
    let walk = walk_all_by(
        &fixture.router,
        "/api/v1/executions",
        fixture.key(),
        2,
        &stop,
        "response_id",
    )
    .await;

    assert!(walk.pages > 1, "7 executions at limit=2 must span pages");
    assert!(
        walk.exhausted,
        "the list is scoped to this application and must run out of rows"
    );
    assert_seeded_order(&walk, &expected, "public executions");
    assert_eq!(
        walk.ids().len(),
        7,
        "the scoped walk must return exactly the seeded rows and nothing else"
    );

    let rejected = get(
        &fixture.router,
        "/api/v1/executions?cursor=not-a-cursor",
        fixture.key(),
    )
    .await;
    assert_invalid_cursor(&rejected);

    fixture.cleanup().await;
}

#[tokio::test]
async fn public_executions_tied_on_created_at_are_ordered_deterministically_by_id() {
    let Some(fixture) = PaginationFixture::new().await else {
        return;
    };

    // Six responses sharing one `created_at`. A cursor keyed on the timestamp alone has no
    // defined position inside this group, so a page boundary landing in it returns one row
    // twice and another never — which is what `walk_all` fails on.
    let ids = fixture.seed_executions(6, true).await;
    let distinct = sqlx::query_scalar::<_, i64>(
        "select count(distinct created_at) from responses where id = any($1)",
    )
    .bind(&ids)
    .fetch_one(fixture.pool())
    .await
    .expect("count distinct created_at");
    assert_eq!(
        distinct, 1,
        "the tie test is worthless unless every seeded row shares one timestamp"
    );

    let expected = fixture.expected_execution_order(&ids).await;
    let stop: BTreeSet<String> = expected.iter().cloned().collect();

    let first = walk_all_by(
        &fixture.router,
        "/api/v1/executions",
        fixture.key(),
        2,
        &stop,
        "response_id",
    )
    .await;
    assert!(first.pages > 1, "6 tied rows at limit=2 must span pages");
    assert_seeded_order(&first, &expected, "tied public executions, first walk");

    // Repeating the walk must produce the same sequence, or a caller resuming from a stored
    // cursor sees a different result set.
    let second = walk_all_by(
        &fixture.router,
        "/api/v1/executions",
        fixture.key(),
        2,
        &stop,
        "response_id",
    )
    .await;
    assert_seeded_order(&second, &expected, "tied public executions, second walk");

    fixture.cleanup().await;
}

#[tokio::test]
async fn public_execution_list_last_page_is_exactly_full_and_reports_no_more() {
    let Some(fixture) = PaginationFixture::new().await else {
        return;
    };

    // Six rows at limit=3: the final page is exactly full. This is the boundary an
    // over-fetch of `limit + 1` exists to get right — a `has_more` computed from
    // `rows.len() == limit` instead would claim a further page here and strand the caller on
    // an empty one, and a `<=` keyset predicate would re-serve the boundary row.
    let ids = fixture.seed_executions(6, false).await;
    let expected = fixture.expected_execution_order(&ids).await;
    let stop: BTreeSet<String> = expected.iter().cloned().collect();

    let walk = walk_all_by(
        &fixture.router,
        "/api/v1/executions",
        fixture.key(),
        3,
        &stop,
        "response_id",
    )
    .await;

    assert!(walk.exhausted, "the walk must reach a real last page");
    assert_eq!(
        walk.pages, 2,
        "6 rows at limit=3 is exactly two full pages, not three"
    );
    assert!(
        walk.final_next_cursor.is_none(),
        "an exactly-full last page must still carry a null next_cursor, got {:?}",
        walk.final_next_cursor
    );
    assert_seeded_order(&walk, &expected, "public executions at an exact boundary");

    fixture.cleanup().await;
}

#[tokio::test]
async fn public_usage_list_pages_through_the_cursor_it_advertises() {
    let Some(fixture) = PaginationFixture::new().await else {
        return;
    };

    // `usage_records.occurred_at` defaults to `now()` — the transaction timestamp — so rows
    // written together really do collide in production. Seeded tied for exactly that reason.
    let ids = fixture.seed_usage_records(7, true).await;
    let distinct = sqlx::query_scalar::<_, i64>(
        "select count(distinct occurred_at) from usage_records where id = any($1)",
    )
    .bind(&ids)
    .fetch_one(fixture.pool())
    .await
    .expect("count distinct occurred_at");
    assert_eq!(
        distinct, 1,
        "the tie test is worthless unless every seeded row shares one timestamp"
    );

    let expected = fixture.expected_usage_order(&ids).await;
    assert_eq!(expected.len(), 7);
    let stop: BTreeSet<String> = expected.iter().cloned().collect();

    let walk = walk_all_by(
        &fixture.router,
        "/api/v1/usage",
        fixture.key(),
        2,
        &stop,
        "execution_id",
    )
    .await;

    assert!(walk.pages > 1, "7 usage rows at limit=2 must span pages");
    assert!(walk.exhausted, "the scoped usage list must terminate");
    assert_seeded_order(&walk, &expected, "public usage");

    let rejected = get(
        &fixture.router,
        "/api/v1/usage?cursor=not-a-cursor",
        fixture.key(),
    )
    .await;
    assert_invalid_cursor(&rejected);

    fixture.cleanup().await;
}

#[tokio::test]
async fn public_model_list_pages_through_the_cursor_it_advertises() {
    let Some(fixture) = PaginationFixture::new().await else {
        return;
    };

    let ids = fixture.seed_visible_models(7, false).await;
    let expected = fixture.expected_visible_model_order(&ids).await;
    assert_eq!(expected.len(), 7, "all seeded models must be visible");
    let stop: BTreeSet<String> = expected.iter().cloned().collect();

    let walk = walk_all(&fixture.router, "/api/v1/models", fixture.key(), 2, &stop).await;

    assert!(walk.pages > 1, "7 models at limit=2 must span pages");
    assert!(walk.exhausted, "the scoped model list must terminate");
    assert_seeded_order(&walk, &expected, "public models");
    assert_eq!(
        walk.ids().len(),
        7,
        "the walk must return exactly the visible models and nothing else"
    );

    // The route used to ignore `limit` entirely — it had no such parameter and passed a
    // hard-coded 200 — which a walk alone would not notice.
    let single = get(&fixture.router, "/api/v1/models?limit=3", fixture.key()).await;
    assert_eq!(single.status, StatusCode::OK, "{}", single.body);
    assert_eq!(
        single.rows().len(),
        3,
        "the route must honour `limit`: {}",
        single.body
    );
    assert!(single.has_more());

    let rejected = get(
        &fixture.router,
        "/api/v1/models?cursor=not-a-cursor",
        fixture.key(),
    )
    .await;
    assert_invalid_cursor(&rejected);

    fixture.cleanup().await;
}

#[tokio::test]
async fn public_route_list_pages_through_the_cursor_it_advertises() {
    let Some(fixture) = PaginationFixture::new().await else {
        return;
    };

    let ids = fixture.seed_visible_routes(7, false).await;
    let expected = fixture.expected_route_order(&ids).await;
    assert_eq!(expected.len(), 7, "all seeded routes must be visible");
    let stop: BTreeSet<String> = expected.iter().cloned().collect();

    let walk = walk_all(&fixture.router, "/api/v1/routes", fixture.key(), 2, &stop).await;

    assert!(walk.pages > 1, "7 routes at limit=2 must span pages");
    assert!(walk.exhausted, "the scoped route list must terminate");
    assert_seeded_order(&walk, &expected, "public routes");

    let rejected = get(
        &fixture.router,
        "/api/v1/routes?cursor=not-a-cursor",
        fixture.key(),
    )
    .await;
    assert_invalid_cursor(&rejected);

    fixture.cleanup().await;
}

#[tokio::test]
async fn a_public_cursor_is_refused_by_a_different_public_list() {
    let Some(fixture) = PaginationFixture::new().await else {
        return;
    };

    // `/v1/executions` and `/v1/usage` both page a `(timestamp, uuid)` key over rows keyed
    // by the same execution, so nothing in the cursor payload distinguishes them — only the
    // per-endpoint scope mixed into the integrity tag does. Without it this request would
    // silently return a page of the wrong list rather than a `400`.
    fixture.seed_executions(3, false).await;
    fixture.seed_usage_records(3, false).await;

    let executions = get(&fixture.router, "/api/v1/executions?limit=1", fixture.key()).await;
    assert_eq!(executions.status, StatusCode::OK, "{}", executions.body);
    let execution_cursor = executions.next_cursor().expect("execution cursor");

    let crossed = get(
        &fixture.router,
        &format!("/api/v1/usage?limit=1&cursor={execution_cursor}"),
        fixture.key(),
    )
    .await;
    assert_invalid_cursor(&crossed);

    // And a tampered cursor from the endpoint that minted it must fail closed too, so the
    // rejection above is attributable to the scope rather than to blanket refusal.
    let honest = get(
        &fixture.router,
        &format!("/api/v1/executions?limit=1&cursor={execution_cursor}"),
        fixture.key(),
    )
    .await;
    assert_eq!(
        honest.status,
        StatusCode::OK,
        "an untampered cursor must be accepted: {}",
        honest.body
    );
    let tampered = tamper(&execution_cursor);
    let rejected = get(
        &fixture.router,
        &format!("/api/v1/executions?limit=1&cursor={tampered}"),
        fixture.key(),
    )
    .await;
    assert_invalid_cursor(&rejected);

    fixture.cleanup().await;
}

// ---------------------------------------------------------------------------
// Issue #93 — the nine admin lists that plan 04 fixed but never walked
//
// Each of these already ran the shared keyset mechanism; none had a test that proved it end
// to end over HTTP. `applications`, `routes`, `audit-events`, `conversations`,
// `rag-collections/{id}/documents` and conversation messages are covered above.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn admin_provider_list_pages_through_the_cursor_it_advertises() {
    let Some(fixture) = PaginationFixture::new().await else {
        return;
    };

    let ids = fixture.seed_providers(7, false).await;
    let expected = fixture.expected_created_at_order("providers", &ids).await;
    assert_eq!(expected.len(), 7);
    let stop: BTreeSet<String> = expected.iter().cloned().collect();

    let walk = walk_all(&fixture.router, "/api/v1/admin/providers", None, 2, &stop).await;

    assert!(walk.pages > 1, "7 providers at limit=2 must span pages");
    assert_seeded_order(&walk, &expected, "admin providers");

    fixture.cleanup().await;
}

#[tokio::test]
async fn admin_provider_model_list_pages_through_the_cursor_it_advertises() {
    let Some(fixture) = PaginationFixture::new().await else {
        return;
    };

    // This list is narrowed by a path parameter, so its keyset predicate carries one fixed
    // parameter before the cursor — the numbering `KeysetTail` exists to keep straight.
    let provider_id = fixture.create_provider(0).await;
    let ids = fixture.seed_provider_models(provider_id, 7, true).await;
    assert_eq!(
        fixture.distinct_created_at("provider_models", &ids).await,
        1,
        "seeded tied, to exercise the id tiebreaker"
    );

    let expected = fixture
        .expected_created_at_order("provider_models", &ids)
        .await;
    let stop: BTreeSet<String> = expected.iter().cloned().collect();

    let walk = walk_all(
        &fixture.router,
        &format!("/api/v1/admin/providers/{provider_id}/models"),
        None,
        2,
        &stop,
    )
    .await;

    assert!(walk.pages > 1, "7 models at limit=2 must span pages");
    assert!(
        walk.exhausted,
        "a list narrowed to one provider must run out of rows"
    );
    assert_seeded_order(&walk, &expected, "admin provider models");

    fixture.cleanup().await;
}

#[tokio::test]
async fn admin_credential_lists_page_through_the_cursor_they_advertise() {
    let Some(fixture) = PaginationFixture::new().await else {
        return;
    };

    let provider_id = fixture.create_provider(0).await;
    let external_user_id = format!("pag-user-{}", fixture.suffix);
    let global = fixture.seed_credentials(provider_id, 7, None, false).await;
    let per_user = fixture
        .seed_credentials(provider_id, 7, Some(&external_user_id), false)
        .await;

    // The global list sees both sets; the per-user list sees only its own.
    let all: Vec<Uuid> = global.iter().chain(per_user.iter()).copied().collect();
    let expected_all = fixture
        .expected_created_at_order("provider_credentials", &all)
        .await;
    let stop_all: BTreeSet<String> = expected_all.iter().cloned().collect();
    let walk = walk_all(
        &fixture.router,
        "/api/v1/admin/provider-credentials",
        None,
        3,
        &stop_all,
    )
    .await;
    assert!(walk.pages > 1, "14 credentials at limit=3 must span pages");
    assert_seeded_order(&walk, &expected_all, "admin provider credentials");

    let expected_user = fixture
        .expected_created_at_order("provider_credentials", &per_user)
        .await;
    let stop_user: BTreeSet<String> = expected_user.iter().cloned().collect();
    let user_walk = walk_all(
        &fixture.router,
        &format!("/api/v1/admin/users/{external_user_id}/provider-credentials"),
        None,
        2,
        &stop_user,
    )
    .await;
    assert!(user_walk.pages > 1, "7 rows at limit=2 must span pages");
    assert!(
        user_walk.exhausted,
        "a list narrowed to one external user must run out of rows"
    );
    assert_seeded_order(&user_walk, &expected_user, "admin user credentials");
    assert_eq!(
        user_walk.ids().len(),
        7,
        "the per-user list must not leak the globally scoped credentials"
    );

    fixture.cleanup().await;
}

#[tokio::test]
async fn admin_api_key_lists_page_through_the_cursor_they_advertise() {
    let Some(fixture) = PaginationFixture::new().await else {
        return;
    };

    let system_ids = fixture.seed_system_keys(7, false).await;
    let expected_system = fixture
        .expected_created_at_order("system_api_keys", &system_ids)
        .await;
    let stop_system: BTreeSet<String> = expected_system.iter().cloned().collect();
    let system_walk = walk_all(
        &fixture.router,
        "/api/v1/admin/system-keys",
        None,
        2,
        &stop_system,
    )
    .await;
    assert!(system_walk.pages > 1, "7 keys at limit=2 must span pages");
    assert_seeded_order(&system_walk, &expected_system, "admin system keys");

    // Seeded tied: the two key lists share one query builder, and a missing tiebreaker there
    // would break both.
    let consumer_ids = fixture.seed_consumer_keys(6, true).await;
    assert_eq!(
        fixture
            .distinct_created_at("consumer_api_keys", &consumer_ids)
            .await,
        1,
        "the tie test is worthless unless every seeded row shares one timestamp"
    );
    let expected_consumer = fixture
        .expected_created_at_order("consumer_api_keys", &consumer_ids)
        .await;
    let stop_consumer: BTreeSet<String> = expected_consumer.iter().cloned().collect();
    let consumer_walk = walk_all(
        &fixture.router,
        "/api/v1/admin/consumer-keys",
        None,
        2,
        &stop_consumer,
    )
    .await;
    assert!(
        consumer_walk.pages > 1,
        "6 tied rows at limit=2 must span pages"
    );
    assert_seeded_order(&consumer_walk, &expected_consumer, "admin consumer keys");

    fixture.cleanup().await;
}

#[tokio::test]
async fn admin_trusted_jwt_issuer_list_pages_through_the_cursor_it_advertises() {
    let Some(fixture) = PaginationFixture::new().await else {
        return;
    };

    let ids = fixture.seed_trusted_jwt_issuers(7, false).await;
    let expected = fixture
        .expected_created_at_order("trusted_jwt_issuers", &ids)
        .await;
    assert_eq!(expected.len(), 7);
    let stop: BTreeSet<String> = expected.iter().cloned().collect();

    let walk = walk_all(&fixture.router, "/api/v1/admin/jwt-issuers", None, 2, &stop).await;

    assert!(walk.pages > 1, "7 issuers at limit=2 must span pages");
    assert_seeded_order(&walk, &expected, "admin trusted JWT issuers");

    fixture.cleanup().await;
}

#[tokio::test]
async fn admin_rag_collection_list_pages_through_the_cursor_it_advertises() {
    let Some(fixture) = PaginationFixture::new().await else {
        return;
    };

    let ids = fixture.seed_rag_collections(7, false).await;
    let expected = sqlx::query_scalar::<_, String>(
        "select public_id from rag_collections where public_id = any($1) \
         and deleted_at is null order by created_at desc, id desc",
    )
    .bind(&ids)
    .fetch_all(fixture.pool())
    .await
    .expect("read documented RAG collection order");
    assert_eq!(expected.len(), 7);
    let stop: BTreeSet<String> = expected.iter().cloned().collect();

    let walk = walk_all(
        &fixture.router,
        "/api/v1/admin/rag-collections",
        None,
        2,
        &stop,
    )
    .await;

    assert!(walk.pages > 1, "7 collections at limit=2 must span pages");
    assert_seeded_order(&walk, &expected, "admin RAG collections");

    fixture.cleanup().await;
}

#[tokio::test]
async fn memory_list_pages_through_the_cursor_it_advertises() {
    let Some(fixture) = PaginationFixture::new().await else {
        return;
    };

    // Three pairs, each pair sharing one `updated_at` written by the production version
    // trigger rather than by the test — the same tie the conversation walk exercises, on the
    // other list that pages by `updated_at`.
    let ids = fixture.seed_memories(6, true).await;
    let expected = fixture.expected_memory_order(&ids).await;
    assert_eq!(expected.len(), 6);
    let distinct = sqlx::query_scalar::<_, i64>(
        "select count(distinct updated_at) from memory_records where public_id = any($1)",
    )
    .bind(&ids)
    .fetch_one(fixture.pool())
    .await
    .expect("count distinct updated_at");
    assert!(
        distinct < ids.len() as i64,
        "the seeded memories must contain at least one timestamp tie, got {distinct} \
         distinct timestamps for {} rows",
        ids.len()
    );

    let stop: BTreeSet<String> = expected.iter().cloned().collect();
    let walk = walk_all(&fixture.router, "/api/v1/memories", fixture.key(), 2, &stop).await;

    assert!(walk.pages > 1, "6 memories at limit=2 must span pages");
    assert!(walk.exhausted, "the scoped memory list must terminate");
    assert_seeded_order(&walk, &expected, "memories");

    fixture.cleanup().await;
}
