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
//! # Every test owns a private database (finding F10 item 1)
//!
//! This was the last suite writing to the long-lived database
//! `MOIRA_TEST_DATABASE_URL` names, and so the last source of cross-run coupling
//! in `tests/`. It now uses [`support::TestDatabase`] like every other suite: a
//! clone of the migrated template, dropped in `Drop` — including while the test
//! is unwinding from a panic, which is exactly when a leak would otherwise be
//! permanent.
//!
//! **That change was not cosmetic; it fixed two defects that fed each other.**
//!
//! 1. *An exact count against a sweep that was not this test's.* A sweep deletes
//!    every expired row in the database it is connected to, not only the rows the
//!    calling test seeded. On a shared database a sibling suite's expired rows
//!    landed in this test's `RetentionOutcome`, so the counts could only be
//!    asserted with `>=`, and the one place that genuinely needed equality — the
//!    per-tick cap — was defended by a cluster-wide advisory lock that serialised
//!    every sweep in every test binary and every worktree on the machine.
//! 2. *A leak that poisoned later runs.* Rows were backdated a century so that
//!    `order by expires_at` reached them before anything a concurrent fixture had
//!    left expired. Cleanup was a `delete` at the end of the test body, which a
//!    failing assertion skips. So one bad run left century-backdated rows behind
//!    permanently, and they then sorted *ahead* of the next run's — the failure
//!    reproduced itself for ever, against a database no test could clean.
//!
//! On a private clone neither exists. The table contains exactly what the test put
//! in it, the counts are asserted with `==`, the advisory lock is gone, the century
//! backdating is gone, and there is no cleanup path to skip: the whole database is
//! discarded either way.
//!
//! **Isolation does not weaken what is under test.** A PostgreSQL connection is
//! scoped to one database, so `run_once` was never *cluster*-wide — it is
//! database-wide, and it is database-wide on a clone in exactly the same way, over
//! the same code path. No test here asserts that a sweep reaches rows it did not
//! itself seed; the original module comment said the opposite, that every
//! assertion is scoped by id. What isolation removes is other suites' rows, which
//! were never the subject.
//!
//! [`the_fixture_owns_a_disposable_database`] is the guard that says it has not
//! been moved back.

mod support;

use std::{future::Future, time::Duration};

use moira::{
    app::AppState,
    config::{Settings, WorkerSettings},
    infra::{
        metrics::MetricsRegistry,
        workers::{self, retention},
    },
};
use sqlx::PgPool;
use tokio::{sync::oneshot, time::timeout};
use uuid::Uuid;

use support::TestDatabase;

const DATABASE_TIMEOUT: Duration = Duration::from_secs(30);

/// How long [`a_running_supervisor_dispatches_a_retention_sweep`] waits to observe
/// a sweep before declaring the dispatch wiring dead. Generous relative to what a
/// working supervisor needs (its first retention tick fires immediately), so the
/// budget is only ever spent by an actual failure.
const SUPERVISOR_OBSERVATION_BUDGET: Duration = Duration::from_secs(15);

/// A private, migrated database for one test.
///
/// Returns `None` — and the caller returns early — when `MOIRA_TEST_DATABASE_URL`
/// is unset outside CI, which is [`support::TestDatabase`]'s own fail-closed
/// behaviour: it panics when `CI=true` and prints the `skipping database-backed
/// tests` line `scripts/gates.sh` asserts on otherwise. The skip message this
/// suite used to print, `skipping retention worker tests: …`, matched neither of
/// the gate's two patterns, so a silent skip here was invisible to the gate.
async fn test_database() -> Option<TestDatabase> {
    TestDatabase::create().await
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

/// Every row in a retention table, not only this test's.
///
/// Meaningless on a shared database and precise on a private one: the table holds
/// exactly what the test put there, so a whole-table count is a direct statement
/// about what the sweep did and did not delete. It is the assertion that catches a
/// sweep deleting *more* than the rows named below — which a set of by-id
/// existence checks, however many, cannot.
async fn table_count(pool: &PgPool, table: &str) -> i64 {
    let sql = match table {
        "idempotency_records" => "select count(*) from idempotency_records",
        "responses" => "select count(*) from responses",
        other => panic!("unexpected table {other}"),
    };
    sqlx::query_scalar::<_, i64>(sql)
        .fetch_one(pool)
        .await
        .expect("table count")
}

/// Bounded polling. Returns `true` as soon as `condition` holds, `false` once
/// `budget` is spent.
///
/// Deliberately not a sleep-then-assert: the assertion downstream is gated on an
/// *observed* state change, so a working supervisor satisfies it on the first or
/// second iteration and the test costs milliseconds. Only a genuine failure pays
/// the full budget. The paced tick exists solely so the loop does not spin the
/// connection pool that the supervisor itself must acquire from — the first
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
    let Some(database) = test_database().await else {
        return;
    };
    let pool = &database.pool;
    let marker = format!("ret-{}", Uuid::now_v7());

    // Expired an hour ago.
    let expired = insert_idempotency_record(pool, &marker, 1, 3_600).await;
    // Expires in an hour (negative age => future).
    let live = insert_idempotency_record(pool, &marker, 2, -3_600).await;

    let outcome = retention::run_once(
        pool,
        &settings(500),
        &MetricsRegistry::new("moira-retention-test", None),
    )
    .await
    .expect("retention sweep");

    assert!(
        !row_exists(pool, "idempotency_records", expired).await,
        "an expired idempotency record must be pruned"
    );
    assert!(
        row_exists(pool, "idempotency_records", live).await,
        "an unexpired idempotency record must survive the sweep"
    );
    assert_eq!(
        outcome.idempotency_records_deleted, 1,
        "one row was expired, so the sweep must report exactly one deletion, got {outcome:?}"
    );
    assert_eq!(
        outcome.responses_deleted, 0,
        "no `responses` row was seeded, so the sweep must report none deleted, got {outcome:?}"
    );
    assert_eq!(
        table_count(pool, "idempotency_records").await,
        1,
        "the live row must be the only one left in the table"
    );
}

#[tokio::test]
async fn retention_run_deletes_expired_responses_and_keeps_rows_with_null_or_future_expires_at() {
    let Some(database) = test_database().await else {
        return;
    };
    let pool = &database.pool;
    let marker = format!("ret-{}", Uuid::now_v7());

    let expired = insert_response(pool, &marker, 1, "now() - interval '1 hour'").await;
    let future = insert_response(pool, &marker, 2, "now() + interval '1 hour'").await;
    // The common case: no retention policy configured, so `expires_at` is NULL.
    // A NULL must never be treated as "expired long ago".
    let never = insert_response(pool, &marker, 3, "null").await;

    let outcome = retention::run_once(
        pool,
        &settings(500),
        &MetricsRegistry::new("moira-retention-test", None),
    )
    .await
    .expect("retention sweep");

    assert!(
        !row_exists(pool, "responses", expired).await,
        "an expired response must be pruned"
    );
    assert!(
        row_exists(pool, "responses", future).await,
        "a response expiring in the future must survive"
    );
    assert!(
        row_exists(pool, "responses", never).await,
        "a response with a NULL expires_at must survive"
    );
    assert_eq!(
        outcome.responses_deleted, 1,
        "one response was expired, so the sweep must report exactly one deletion, got {outcome:?}"
    );
    assert_eq!(
        table_count(pool, "responses").await,
        2,
        "the future-dated and NULL rows must both be left behind"
    );
}

#[tokio::test]
async fn retention_run_respects_the_configured_batch_size() {
    let Some(database) = test_database().await else {
        return;
    };
    let pool = &database.pool;
    let marker = format!("ret-{}", Uuid::now_v7());

    // `batch_size = 1` gives a per-tick cap of 1 * PER_TICK_BATCH_BUDGET, which is
    // what this test pins: one sweep must stop at the cap and leave the rest.
    let per_tick_cap = retention::RetentionPlan::PER_TICK_BATCH_BUDGET as usize;
    let seeded = per_tick_cap * 2 + 3;

    // Expired an hour ago. These used to be backdated a century so that
    // `order by expires_at` reached them before whatever a concurrent suite had
    // left expired on the shared database; on a private clone they are the only
    // expired rows there are, and the backdating was also what made a leak from a
    // failed run poison every later one.
    let mut ids = Vec::with_capacity(seeded);
    for nth in 0..seeded {
        ids.push(insert_idempotency_record(pool, &marker, nth, 3_600).await);
    }

    let first = retention::run_once(
        pool,
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
        surviving(pool, "idempotency_records", &ids).await,
        seeded - per_tick_cap,
        "the remainder of the backlog must be left for the next tick"
    );
    assert_eq!(
        table_count(pool, "idempotency_records").await,
        (seeded - per_tick_cap) as i64,
        "the cap bounds the whole sweep, so no row outside the seeded backlog may go either"
    );

    // Repeated ticks drain the backlog rather than stalling on it.
    for _ in 0..4 {
        retention::run_once(
            pool,
            &settings(1),
            &MetricsRegistry::new("moira-retention-test", None),
        )
        .await
        .expect("drain sweep");
    }
    assert_eq!(
        surviving(pool, "idempotency_records", &ids).await,
        0,
        "successive ticks must drain the whole backlog"
    );
}

/// A batch must delete **at most `limit` rows whatever plan PostgreSQL chooses**.
///
/// This is the regression test for the defect that made
/// [`retention_run_respects_the_configured_batch_size`] report 21 and 22 deletions
/// against a per-tick cap of 20. The cap arithmetic in `RetentionPlan` was never
/// wrong; the SQL was. `where id in (select … order by expires_at limit $1 for
/// update skip locked)` bounds one *evaluation* of the sub-query, and the planner
/// may evaluate it once per outer row — when it picks a `Nested Loop Semi Join`
/// with the sub-query inner and un-materialised, each re-execution skips the rows
/// this command already deleted and returns a fresh victim, which the outer scan
/// then deletes too. Measured against this schema: a `limit 1` batch deleting 2
/// rows in the shape the shared test database naturally reaches, and 42 of 42 rows
/// when the shape is forced.
///
/// The plan is forced rather than coaxed, because the natural trigger is a
/// *statistics* state (`reltuples = 0` left by an autoanalyze after a bulk delete,
/// with rows inserted before the next analyze) and a test that depends on planner
/// statistics is a test that passes vacuously the day the statistics move. Forcing
/// the join shape asserts the invariant directly: the victim set is computed once,
/// so no plan can exceed the bound. `set local` keeps the forcing inside this
/// transaction, so the connection returns to the pool unmodified, and the rollback
/// puts the deleted rows back for the next iteration to find.
#[tokio::test]
async fn a_retention_batch_never_deletes_more_rows_than_its_limit_under_a_hostile_plan() {
    let Some(database) = test_database().await else {
        return;
    };
    let pool = &database.pool;
    let marker = format!("ret-{}", Uuid::now_v7());

    let mut ids = Vec::new();
    for nth in 0..24 {
        ids.push(insert_idempotency_record(pool, &marker, nth, 3_600).await);
    }

    let sql = retention::batch_delete_sql("idempotency_records")
        .expect("idempotency_records is a retention table");

    for limit in [1_i64, 3, 7] {
        let mut tx = pool.begin().await.expect("begin hostile-plan transaction");
        // Every join strategy that would evaluate the victim query once is denied,
        // leaving the planner with the re-executing nested loop.
        for guc in [
            "set local enable_material = off",
            "set local enable_hashagg = off",
            "set local enable_hashjoin = off",
            "set local enable_mergejoin = off",
            "set local enable_sort = off",
        ] {
            sqlx::query(guc)
                .execute(&mut *tx)
                .await
                .expect("force the hostile join shape");
        }

        let deleted = sqlx::query(sql)
            .bind(limit)
            .execute(&mut *tx)
            .await
            .expect("hostile-plan retention batch")
            .rows_affected();

        assert_eq!(
            deleted, limit as u64,
            "a batch asking for {limit} row(s) deleted {deleted}; the per-tick cap is only a \
             bound if one batch is, so this is retention removing more than it was authorised to"
        );

        tx.rollback().await.expect("restore the batch's rows");
    }

    assert_eq!(
        surviving(pool, "idempotency_records", &ids).await,
        ids.len(),
        "every batch above was rolled back, so all {} rows must still be present",
        ids.len()
    );
}

#[tokio::test]
async fn retention_run_does_not_block_a_concurrent_idempotency_claim() {
    let Some(database) = test_database().await else {
        return;
    };
    let pool = &database.pool;
    let marker = format!("ret-{}", Uuid::now_v7());

    // Two expired rows. One will be locked by a simulated claim transaction; the
    // other must still be swept while that lock is held.
    let locked = insert_idempotency_record(pool, &marker, 1, 3_600).await;
    let unlocked = insert_idempotency_record(pool, &marker, 2, 3_600).await;

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
            pool,
            &settings(500),
            &MetricsRegistry::new("moira-retention-test", None),
        ),
    )
    .await
    .expect("retention sweep blocked behind a concurrent row lock")
    .expect("retention sweep");

    assert!(
        row_exists(pool, "idempotency_records", locked).await,
        "a row locked by a concurrent claim must be skipped, not stolen"
    );
    assert!(
        !row_exists(pool, "idempotency_records", unlocked).await,
        "an unlocked expired row must still be swept while another row is locked"
    );
    assert_eq!(
        outcome.idempotency_records_deleted, 1,
        "exactly one of the two expired rows was claimable, got {outcome:?}"
    );

    release_tx.send(()).expect("release the claim transaction");
    holder.await.expect("claim transaction task");

    // Once the lock is gone the skipped row is picked up on the next tick — the
    // skip defers work, it does not drop it.
    retention::run_once(
        pool,
        &settings(500),
        &MetricsRegistry::new("moira-retention-test", None),
    )
    .await
    .expect("follow-up sweep");
    assert!(
        !row_exists(pool, "idempotency_records", locked).await,
        "a previously locked row must be swept on a later tick"
    );
}

#[tokio::test]
async fn retention_run_records_deleted_counts_for_observability() {
    let Some(database) = test_database().await else {
        return;
    };
    let pool = &database.pool;
    let marker = format!("ret-{}", Uuid::now_v7());

    insert_idempotency_record(pool, &marker, 1, 3_600).await;
    insert_response(pool, &marker, 1, "now() - interval '1 hour'").await;

    let metrics = MetricsRegistry::new("moira-retention-test", None);
    let before = counters(&metrics);

    let outcome = retention::run_once(pool, &settings(500), &metrics)
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
    // Exact, not `>= 2`: the database holds exactly the two rows seeded above, so a
    // counter that over-reports is as much a defect as one that under-reports.
    assert_eq!(
        after.idempotency_deleted - before.idempotency_deleted,
        1,
        "one expired idempotency record was seeded, got {outcome:?}"
    );
    assert_eq!(
        after.responses_deleted - before.responses_deleted,
        1,
        "one expired response was seeded, got {outcome:?}"
    );
    assert_eq!(
        outcome.total_deleted(),
        2,
        "both seeded rows and only those should have been swept, got {outcome:?}"
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
/// The supervisor keeps sweeping on a timer until it is shut down. On the shared
/// database that made it a hazard to every sibling test and the reason this file
/// held a cluster-wide advisory lock; on its own database it can only reach its
/// own rows. It is still shut down the instant the observation lands, before any
/// assertion can panic out of the test and strand it.
#[tokio::test]
async fn a_running_supervisor_dispatches_a_retention_sweep() {
    let Some(database) = test_database().await else {
        return;
    };
    let pool = &database.pool;
    let marker = format!("ret-{}", Uuid::now_v7());

    // Seeded before the supervisor starts, so the very first tick can claim it.
    let expired = insert_idempotency_record(pool, &marker, 1, 3_600).await;
    // A live row, so a passing test means "the worker ran and swept correctly",
    // not merely "something deleted rows".
    let live = insert_idempotency_record(pool, &marker, 2, -3_600).await;

    let mut settings = Settings::default();
    settings.workers.enabled = true;
    settings.workers.retention_batch_size = 500;
    // The floor accepted by `RetentionPlan::interval_seconds`. It costs the test
    // nothing: `tokio::time::interval` yields its first tick immediately, so this
    // bounds a *retry*, not the first sweep.
    settings.workers.retention_interval_seconds = 1;

    let state = AppState::new(settings, Some(pool.clone()))
        .await
        .expect("supervisor app state");
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
        !row_exists(pool, "idempotency_records", expired).await
    })
    .await;

    // Before the assertions, so a failure cannot leave a live sweeper running
    // while the test unwinds.
    supervisor.shutdown().await;

    assert!(
        swept,
        "a running supervisor must dispatch a retention sweep and delete the expired row \
         within {SUPERVISOR_OBSERVATION_BUDGET:?}; it did not, so the dispatch wiring is dead"
    );
    // `AppState::new` mints a fresh registry, so this counter observes only sweeps
    // this supervisor dispatched.
    assert!(
        counters(&state.metrics).runs >= 1,
        "the supervisor's sweep must be counted, so an operator can see the worker is alive"
    );
    assert!(
        row_exists(pool, "idempotency_records", live).await,
        "the supervisor's sweep must spare an unexpired row"
    );
    assert_eq!(
        table_count(pool, "idempotency_records").await,
        1,
        "a supervisor sweeping on a timer must delete the expired row and stop there"
    );
}

// ---------------------------------------------------------------------------
// Anti-leak guard
// ---------------------------------------------------------------------------

/// Finding F10 item 1: every row this suite writes must land in a database that is
/// thrown away when the test ends, not in the long-lived one
/// `MOIRA_TEST_DATABASE_URL` names.
///
/// **What this establishes.** That [`test_database`] hands out a per-test clone owned
/// by [`support::TestDatabase`], whose `Drop` drops the database unconditionally — on
/// a dedicated thread with its own runtime, so it runs while the test is unwinding
/// from a panic. That is the case that mattered here: this suite seeds expired rows
/// and had no cleanup path a failing assertion did not skip, so before this change one
/// failed run left rows behind permanently.
///
/// **What it does not establish.** Not that `idempotency_records` is empty anywhere
/// else; on a shared database "the table is empty" would prove nothing, and assertion
/// (c) is meaningful *only* because (a) and (b) have established the database is
/// private and freshly cloned. Not that teardown succeeds — a `SIGKILL`ed process
/// never runs `Drop` and leaves a whole database for `sweep_leaked_databases` to
/// collect an hour later. And not that any other suite is leak-free:
/// `tests/test_database_isolation.rs` carries that, and it fails in both directions,
/// so leaving `retention_worker.rs` on its `SHARED_DATABASE_ALLOWLIST` after this
/// change would itself be an assertion failure.
#[tokio::test]
async fn the_fixture_owns_a_disposable_database() {
    let Some(database) = test_database().await else {
        return;
    };
    let live = timeout(
        DATABASE_TIMEOUT,
        sqlx::query_scalar::<_, String>("select current_database()").fetch_one(&database.pool),
    )
    .await
    .expect("current_database timed out")
    .expect("current_database");

    // (a) Not the shared database. This is the assertion that turns red the moment
    //     this suite is pointed back at `MOIRA_TEST_DATABASE_URL`.
    let shared = support::shared_database_name().expect("a fixture was built, so the URL parses");
    assert_ne!(
        live, shared,
        "the retention suite is sweeping the shared test database `{shared}`. That is \
         finding F10 item 1 in both of its halves: every expired row any other suite left \
         behind lands in this suite's delete counts, and every row this suite seeds \
         survives a failed assertion for ever"
    );

    // (b) It is this test's own clone, named in the shape `TestDatabase::drop` tears
    //     down and `sweep_leaked_databases` collects if the process dies first.
    assert_eq!(
        live,
        database.name(),
        "the pool must be connected to the database `TestDatabase` owns and drops; a pool \
         pointing anywhere else outlives the teardown"
    );
    assert!(
        live.starts_with("moira_test_") && !live.starts_with("moira_test_template_"),
        "a fixture database must carry the disposable `moira_test_<unix>_<uuid>` name that \
         teardown and the leak sweep both key on, found `{live}`"
    );

    // (c) Cloned from the empty template, so nothing an earlier run of this suite wrote
    //     is visible to this one — which is what makes every `==` count above exact.
    assert_eq!(
        table_count(&database.pool, "idempotency_records").await,
        0,
        "a freshly cloned database must hold no idempotency records; anything here came \
         from an earlier run, and the exact delete counts asserted above would be reading \
         it"
    );
    assert_eq!(
        table_count(&database.pool, "responses").await,
        0,
        "a freshly cloned database must hold no responses"
    );
}
