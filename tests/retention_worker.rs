//! E2E coverage for the retention/cleanup worker (plan 04, finding P1-5).
//!
//! Drives `retention::run_once` against a real PostgreSQL + pgvector database.
//! There is no HTTP surface for a background worker — its "real external surface"
//! is the database, so that is what these tests exercise: real tables, real
//! `expires_at` semantics, real row locks.
//!
//! One test, [`a_running_supervisor_dispatches_a_retention_sweep`], deliberately
//! does *not* call `run_once`: it starts the real supervisor and watches a row
//! disappear. Without it the whole dispatch path is untested and the sweep could
//! be correct but never invoked.
//!
//! **Shared-database discipline.** These tests run against the same database as
//! every other suite, and the sweep is global by construction — it deletes every
//! expired row it can claim, not only this test's. So each test seeds rows carrying
//! a unique marker and asserts on *those* rows by id, never on table-wide counts.
//! Where a test needs its rows chosen first, it backdates them a century so no
//! plausible concurrent fixture sorts ahead of them under `order by expires_at`.
//! Tests that assert on a sweep's *counts* additionally take [`sweep_guard`], since
//! no marker can stop a sibling sweep from deleting this test's rows and counting
//! them as its own.

use std::{future::Future, time::Duration};

use moira::{
    app::AppState,
    config::{Settings, WorkerSettings},
    infra::{
        metrics::MetricsRegistry,
        workers::{self, retention},
    },
};
use sqlx::{PgPool, Postgres, Transaction, postgres::PgPoolOptions};
use tokio::{sync::oneshot, time::timeout};
use uuid::Uuid;

const DATABASE_TIMEOUT: Duration = Duration::from_secs(30);

/// How long [`a_running_supervisor_dispatches_a_retention_sweep`] waits to observe
/// a sweep before declaring the dispatch wiring dead. Generous relative to what a
/// working supervisor needs (its first retention tick fires immediately), so the
/// budget is only ever spent by an actual failure.
const SUPERVISOR_OBSERVATION_BUDGET: Duration = Duration::from_secs(15);

/// Advisory-lock key serialising every retention sweep in the test suite.
const RETENTION_SWEEP_LOCK: i64 = 0x4D4F_4952_4152_4554;

/// Serialises the tests in this file against each other.
///
/// A retention sweep is **global**: it deletes every expired row it can claim,
/// including rows a sibling test seeded a millisecond ago. Run in parallel, these
/// tests steal each other's rows and each one's `RetentionOutcome` counts a mixture
/// of both — which is not a bug in the worker, it is the worker behaving exactly as
/// specified while two tests share one table. So each test takes this lock for its
/// whole body and the sweeps run one at a time.
///
/// It is a **transaction-scoped** advisory lock deliberately: PostgreSQL releases it
/// when the transaction ends, including the implicit rollback `sqlx` performs when
/// the guard is dropped by a panicking test. A session-scoped lock would survive the
/// connection's return to the pool and deadlock every later test.
///
/// This serialises across test binaries and processes too, not just within this file.
async fn sweep_guard(pool: &PgPool) -> Transaction<'static, Postgres> {
    let mut guard = pool.begin().await.expect("begin sweep guard transaction");
    timeout(
        DATABASE_TIMEOUT,
        sqlx::query("select pg_advisory_xact_lock($1)")
            .bind(RETENTION_SWEEP_LOCK)
            .execute(&mut *guard),
    )
    .await
    .expect("timed out waiting for the retention sweep lock")
    .expect("acquire the retention sweep lock");
    guard
}

/// Fail-closed exactly as `tests/support/mod.rs` does — matching on the **value**
/// of `CI`, never merely on the variable being present (`CONVENTIONS.md` §3).
async fn test_pool() -> Option<PgPool> {
    let database_url = match std::env::var("MOIRA_TEST_DATABASE_URL") {
        Ok(value) if !value.trim().is_empty() => value,
        _ if std::env::var("CI").is_ok_and(|value| value.eq_ignore_ascii_case("true")) => {
            panic!("MOIRA_TEST_DATABASE_URL is required when CI=true for retention worker tests")
        }
        _ => {
            eprintln!("skipping retention worker tests: MOIRA_TEST_DATABASE_URL is not set");
            return None;
        }
    };

    let pool = timeout(
        DATABASE_TIMEOUT,
        PgPoolOptions::new()
            .max_connections(4)
            .connect(&database_url),
    )
    .await
    .expect("retention database connection timed out")
    .expect("connect retention test database");

    timeout(DATABASE_TIMEOUT, sqlx::migrate!().run(&pool))
        .await
        .expect("retention migrations timed out")
        .expect("run retention migrations");

    Some(pool)
}

fn settings(batch_size: usize) -> WorkerSettings {
    WorkerSettings {
        enabled: true,
        retention_batch_size: batch_size,
        ..WorkerSettings::default()
    }
}

/// Inserts an `idempotency_records` row. `age_seconds` is subtracted from `now()`
/// to build `expires_at`, so a positive value is already expired.
async fn insert_idempotency_record(
    pool: &PgPool,
    marker: &str,
    nth: usize,
    age_seconds: i64,
) -> Uuid {
    sqlx::query_scalar::<_, Uuid>(
        r#"
        insert into idempotency_records
            (idempotency_key_hash, actor_fingerprint, operation, request_hash, expires_at)
        values ($1, $2, $3, $4, now() - make_interval(secs => $5))
        returning id
        "#,
    )
    .bind(format!("{marker}-key-{nth}"))
    .bind(format!("{marker}-actor"))
    .bind(format!("{marker}-op-{nth}"))
    .bind(format!("{marker}-req-{nth}"))
    .bind(age_seconds as f64)
    .fetch_one(pool)
    .await
    .expect("insert idempotency record")
}

/// Inserts a `responses` row. `expires_at` is passed as a raw SQL interval
/// expression so the NULL case is expressible.
async fn insert_response(pool: &PgPool, marker: &str, nth: usize, expires_at_sql: &str) -> Uuid {
    let sql = format!(
        r#"
        insert into responses (execution_id, request_id, status, expires_at)
        values (gen_random_uuid(), $1, 'completed', {expires_at_sql})
        returning id
        "#
    );
    sqlx::query_scalar::<_, Uuid>(&sql)
        .bind(format!("{marker}-req-{nth}"))
        .fetch_one(pool)
        .await
        .expect("insert response")
}

async fn row_exists(pool: &PgPool, table: &str, id: Uuid) -> bool {
    let sql = match table {
        "idempotency_records" => "select exists(select 1 from idempotency_records where id = $1)",
        "responses" => "select exists(select 1 from responses where id = $1)",
        other => panic!("unexpected table {other}"),
    };
    sqlx::query_scalar::<_, bool>(sql)
        .bind(id)
        .fetch_one(pool)
        .await
        .expect("existence check")
}

/// Bounded polling. Returns `true` as soon as `condition` holds, `false` once
/// `budget` is spent.
///
/// Deliberately not a sleep-then-assert: the assertion downstream is gated on an
/// *observed* state change, so a working supervisor satisfies it on the first or
/// second iteration and the test costs milliseconds. Only a genuine failure pays
/// the full budget. The paced tick exists solely so the loop does not spin the
/// four-connection pool that the supervisor itself must acquire from — the first
/// `interval` tick completes immediately, so it adds no latency to the happy path.
async fn poll_until<F, Fut>(budget: Duration, mut condition: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    let mut ticker = tokio::time::interval(Duration::from_millis(20));
    timeout(budget, async move {
        loop {
            ticker.tick().await;
            if condition().await {
                return;
            }
        }
    })
    .await
    .is_ok()
}

async fn surviving(pool: &PgPool, table: &str, ids: &[Uuid]) -> usize {
    let mut alive = 0;
    for id in ids {
        if row_exists(pool, table, *id).await {
            alive += 1;
        }
    }
    alive
}

#[tokio::test]
async fn retention_run_deletes_expired_idempotency_records_and_keeps_live_rows() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let _sweep_guard = sweep_guard(&pool).await;
    let marker = format!("ret-{}", Uuid::now_v7());

    // Expired an hour ago.
    let expired = insert_idempotency_record(&pool, &marker, 1, 3_600).await;
    // Expires in an hour (negative age => future).
    let live = insert_idempotency_record(&pool, &marker, 2, -3_600).await;

    let outcome = retention::run_once(
        &pool,
        &settings(500),
        &MetricsRegistry::new("moira-retention-test", None),
    )
    .await
    .expect("retention sweep");

    assert!(
        !row_exists(&pool, "idempotency_records", expired).await,
        "an expired idempotency record must be pruned"
    );
    assert!(
        row_exists(&pool, "idempotency_records", live).await,
        "an unexpired idempotency record must survive the sweep"
    );
    assert!(
        outcome.idempotency_records_deleted >= 1,
        "the sweep must report the row it deleted, got {outcome:?}"
    );

    sqlx::query("delete from idempotency_records where id = $1")
        .bind(live)
        .execute(&pool)
        .await
        .expect("cleanup");
}

#[tokio::test]
async fn retention_run_deletes_expired_responses_and_keeps_rows_with_null_or_future_expires_at() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let _sweep_guard = sweep_guard(&pool).await;
    let marker = format!("ret-{}", Uuid::now_v7());

    let expired = insert_response(&pool, &marker, 1, "now() - interval '1 hour'").await;
    let future = insert_response(&pool, &marker, 2, "now() + interval '1 hour'").await;
    // The common case: no retention policy configured, so `expires_at` is NULL.
    // A NULL must never be treated as "expired long ago".
    let never = insert_response(&pool, &marker, 3, "null").await;

    let outcome = retention::run_once(
        &pool,
        &settings(500),
        &MetricsRegistry::new("moira-retention-test", None),
    )
    .await
    .expect("retention sweep");

    assert!(
        !row_exists(&pool, "responses", expired).await,
        "an expired response must be pruned"
    );
    assert!(
        row_exists(&pool, "responses", future).await,
        "a response expiring in the future must survive"
    );
    assert!(
        row_exists(&pool, "responses", never).await,
        "a response with a NULL expires_at must survive"
    );
    assert!(
        outcome.responses_deleted >= 1,
        "the sweep must report the response it deleted, got {outcome:?}"
    );

    sqlx::query("delete from responses where id = any($1)")
        .bind(vec![future, never])
        .execute(&pool)
        .await
        .expect("cleanup");
}

#[tokio::test]
async fn retention_run_respects_the_configured_batch_size() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let _sweep_guard = sweep_guard(&pool).await;
    let marker = format!("ret-{}", Uuid::now_v7());

    // `batch_size = 1` gives a per-tick cap of 1 * PER_TICK_BATCH_BUDGET, which is
    // what this test pins: one sweep must stop at the cap and leave the rest.
    let per_tick_cap = retention::RetentionPlan::PER_TICK_BATCH_BUDGET as usize;
    let seeded = per_tick_cap * 2 + 3;

    // Backdated a century so `order by expires_at` reaches these rows before any
    // row a concurrent suite might have left expired.
    let mut ids = Vec::with_capacity(seeded);
    for nth in 0..seeded {
        ids.push(insert_idempotency_record(&pool, &marker, nth, 3_153_600_000).await);
    }

    let first = retention::run_once(
        &pool,
        &settings(1),
        &MetricsRegistry::new("moira-retention-test", None),
    )
    .await
    .expect("first sweep");
    assert!(
        first.hit_per_tick_cap,
        "a backlog larger than the per-tick cap must be reported as capped, got {first:?}"
    );
    assert_eq!(
        first.idempotency_records_deleted, per_tick_cap as u64,
        "one sweep must delete exactly the per-tick cap, not the whole backlog"
    );
    assert_eq!(
        surviving(&pool, "idempotency_records", &ids).await,
        seeded - per_tick_cap,
        "the remainder of the backlog must be left for the next tick"
    );

    // Repeated ticks drain the backlog rather than stalling on it.
    for _ in 0..4 {
        retention::run_once(
            &pool,
            &settings(1),
            &MetricsRegistry::new("moira-retention-test", None),
        )
        .await
        .expect("drain sweep");
    }
    assert_eq!(
        surviving(&pool, "idempotency_records", &ids).await,
        0,
        "successive ticks must drain the whole backlog"
    );
}

#[tokio::test]
async fn retention_run_does_not_block_a_concurrent_idempotency_claim() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let _sweep_guard = sweep_guard(&pool).await;
    let marker = format!("ret-{}", Uuid::now_v7());

    // Two expired rows. One will be locked by a simulated claim transaction; the
    // other must still be swept while that lock is held.
    let locked = insert_idempotency_record(&pool, &marker, 1, 3_600).await;
    let unlocked = insert_idempotency_record(&pool, &marker, 2, 3_600).await;

    // Acknowledgement gates, not sleeps: `locked_tx` fires once the row lock is
    // definitely held, `release_tx` releases the holder once the sweep has run.
    let (locked_tx, locked_rx) = oneshot::channel::<()>();
    let (release_tx, release_rx) = oneshot::channel::<()>();

    let holder_pool = pool.clone();
    let holder = tokio::spawn(async move {
        let mut tx = holder_pool.begin().await.expect("begin claim transaction");
        // The shape the real claim path takes on this table: a row lock inside a
        // transaction (`src/infra/repositories/admin.rs`, `claim_idempotency`).
        sqlx::query("select id from idempotency_records where id = $1 for update")
            .bind(locked)
            .fetch_one(&mut *tx)
            .await
            .expect("lock the claimed row");
        locked_tx.send(()).expect("signal that the lock is held");
        release_rx.await.expect("await release signal");
        tx.rollback().await.expect("release the claim transaction");
    });

    locked_rx.await.expect("await lock acknowledgement");

    // The load-bearing assertion: with a row lock held on a row the sweep wants,
    // the sweep still COMPLETES. Without `skip locked` this await would block
    // until the holder released, and the bounded timeout would fire.
    let outcome = timeout(
        Duration::from_secs(5),
        retention::run_once(
            &pool,
            &settings(500),
            &MetricsRegistry::new("moira-retention-test", None),
        ),
    )
    .await
    .expect("retention sweep blocked behind a concurrent row lock")
    .expect("retention sweep");

    assert!(
        row_exists(&pool, "idempotency_records", locked).await,
        "a row locked by a concurrent claim must be skipped, not stolen"
    );
    assert!(
        !row_exists(&pool, "idempotency_records", unlocked).await,
        "an unlocked expired row must still be swept while another row is locked"
    );
    assert!(outcome.idempotency_records_deleted >= 1);

    release_tx.send(()).expect("release the claim transaction");
    holder.await.expect("claim transaction task");

    // Once the lock is gone the skipped row is picked up on the next tick — the
    // skip defers work, it does not drop it.
    retention::run_once(
        &pool,
        &settings(500),
        &MetricsRegistry::new("moira-retention-test", None),
    )
    .await
    .expect("follow-up sweep");
    assert!(
        !row_exists(&pool, "idempotency_records", locked).await,
        "a previously locked row must be swept on a later tick"
    );
}

#[tokio::test]
async fn retention_run_records_deleted_counts_for_observability() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let _sweep_guard = sweep_guard(&pool).await;
    let marker = format!("ret-{}", Uuid::now_v7());

    insert_idempotency_record(&pool, &marker, 1, 3_600).await;
    insert_response(&pool, &marker, 1, "now() - interval '1 hour'").await;

    let metrics = MetricsRegistry::new("moira-retention-test", None);
    let before = counters(&metrics);

    let outcome = retention::run_once(&pool, &settings(500), &metrics)
        .await
        .expect("retention sweep");

    let after = counters(&metrics);
    assert_eq!(
        after.runs,
        before.runs + 1,
        "every sweep must be counted, so an operator can see the worker is alive"
    );
    assert_eq!(
        after.idempotency_deleted - before.idempotency_deleted,
        outcome.idempotency_records_deleted,
        "the idempotency_records counter must match what the sweep reported"
    );
    assert_eq!(
        after.responses_deleted - before.responses_deleted,
        outcome.responses_deleted,
        "the responses counter must match what the sweep reported"
    );
    assert!(
        outcome.total_deleted() >= 2,
        "both seeded rows should have been swept, got {outcome:?}"
    );
}

/// The retention counters as an operator would actually read them.
///
/// Plan 05 deleted `MetricsRegistry::snapshot()` along with the hand-rolled renderer, so the
/// exported text is now the single source of truth — a struct read-back would be a second one,
/// free to drift from what `/metrics` really serves. Parsing the exposition here keeps these
/// assertions testing the thing an operator sees rather than an internal field.
#[derive(Debug, Clone, Copy)]
struct RetentionCounters {
    runs: u64,
    idempotency_deleted: u64,
    responses_deleted: u64,
}

fn counters(metrics: &MetricsRegistry) -> RetentionCounters {
    let rendered = metrics.render_prometheus("moira-retention-test", false, true);
    RetentionCounters {
        runs: counter_value(&rendered, "moira_retention_runs_total", None),
        idempotency_deleted: counter_value(
            &rendered,
            "moira_retention_rows_deleted_total",
            Some("table=\"idempotency_records\""),
        ),
        responses_deleted: counter_value(
            &rendered,
            "moira_retention_rows_deleted_total",
            Some("table=\"responses\""),
        ),
    }
}

/// Reads one counter out of the Prometheus exposition.
///
/// Returns 0 when the series is absent: a counter that has never been incremented is genuinely
/// zero, and the callers here compare deltas, so treating "absent" as 0 is correct rather than a
/// silent failure. `label` selects one series within a labelled family.
fn counter_value(rendered: &str, metric: &str, label: Option<&str>) -> u64 {
    rendered
        .lines()
        .filter(|line| !line.starts_with('#'))
        .filter(|line| line.starts_with(metric))
        .filter(|line| label.is_none_or(|needle| line.contains(needle)))
        .filter_map(|line| line.rsplit_once(' '))
        .filter_map(|(_, value)| value.trim().parse::<f64>().ok())
        .map(|value| value as u64)
        .next()
        .unwrap_or(0)
}

/// The only test here that exercises the **dispatch wiring** rather than the sweep.
///
/// Every other test in this file calls [`retention::run_once`] directly. That
/// covers the sweep thoroughly and covers the supervisor not at all: deleting the
/// retention branch from `WorkerRegistry::run_supervisor`
/// (`src/infra/workers.rs`) — e.g. by gating its tick on `if false &&
/// retention_configured` — left the entire suite green, because nothing in
/// `tests/` so much as named `spawn_supervisor`. The definition of done says a
/// *running* retention worker deletes expired rows, so this test starts a real
/// `WorkerSupervisor` over a real [`AppState`] and never calls `run_once` at all.
/// If the supervisor stops dispatching sweeps, this is the test that notices.
///
/// It takes [`sweep_guard`] like its siblings, and for a stronger reason: unlike
/// a direct `run_once` call, the supervisor keeps sweeping on a timer until it is
/// shut down, so it would happily delete rows a sibling test seeded. It is shut
/// down the instant the observation lands, before any assertion can panic out of
/// the test and strand it.
#[tokio::test]
async fn a_running_supervisor_dispatches_a_retention_sweep() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let _sweep_guard = sweep_guard(&pool).await;
    let marker = format!("ret-{}", Uuid::now_v7());

    // Seeded before the supervisor starts, so the very first tick can claim it.
    // Backdated a century for the usual reason: `order by expires_at` reaches this
    // row in the first batch whatever else this shared database has left expired.
    let expired = insert_idempotency_record(&pool, &marker, 1, 3_153_600_000).await;
    // A live row, so a passing test means "the worker ran and swept correctly",
    // not merely "something deleted rows".
    let live = insert_idempotency_record(&pool, &marker, 2, -3_600).await;

    let mut settings = Settings::default();
    settings.workers.enabled = true;
    settings.workers.retention_batch_size = 500;
    // The floor accepted by `RetentionPlan::interval_seconds`. It costs the test
    // nothing: `tokio::time::interval` yields its first tick immediately, so this
    // bounds a *retry*, not the first sweep.
    settings.workers.retention_interval_seconds = 1;

    let state = AppState::new(settings, Some(pool.clone())).expect("supervisor app state");
    // Guards against the test silently arming nothing: if the retention spec ever
    // stops being configured by default, the assertions below would pass or fail
    // for reasons unrelated to dispatch.
    assert!(
        state
            .workers
            .is_configured(workers::RETENTION_CLEANUP_WORKER),
        "the retention worker must be configured, or this test proves nothing"
    );

    let supervisor = state
        .workers
        .spawn_supervisor(state.clone())
        .expect("workers are enabled, so a supervisor must be spawned");

    let swept = poll_until(SUPERVISOR_OBSERVATION_BUDGET, || async {
        !row_exists(&pool, "idempotency_records", expired).await
    })
    .await;

    // Before the assertions, so a failure cannot leave a live sweeper running
    // against the shared database while the guard unwinds.
    supervisor.shutdown().await;

    assert!(
        swept,
        "a running supervisor must dispatch a retention sweep and delete the expired row \
         within {SUPERVISOR_OBSERVATION_BUDGET:?}; it did not, so the dispatch wiring is dead"
    );
    // `AppState::new` mints a fresh registry, so this counter observes only sweeps
    // this supervisor dispatched — no other suite can inflate it.
    assert!(
        counters(&state.metrics).runs >= 1,
        "the supervisor's sweep must be counted, so an operator can see the worker is alive"
    );
    assert!(
        row_exists(&pool, "idempotency_records", live).await,
        "the supervisor's sweep must spare an unexpired row"
    );

    sqlx::query("delete from idempotency_records where id = $1")
        .bind(live)
        .execute(&pool)
        .await
        .expect("cleanup");
}
