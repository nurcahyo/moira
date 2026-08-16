//! The durable worker queue against a real PostgreSQL (plan 10 wave 2, P3-5).
//!
//! Everything here runs on the **default, Redis-off** configuration. The queue is
//! Postgres-backed by design — plan 10 §0.4b — so a deployment that never enables
//! Redis gets all of this, and these tests are the proof.

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use async_trait::async_trait;
use chrono::{Duration as ChronoDuration, Utc};
use moira::{
    config::WorkerSettings,
    error::AppError,
    infra::{
        metrics::MetricsRegistry,
        repositories::{ClaimedJob, PgWorkerJobRepository, WorkerJobInsert, WorkerJobRepository},
        workers::{
            RETENTION_CLEANUP_WORKER,
            queue::{JobDispatcher, StubJobDispatcher, WorkerQueue},
        },
    },
};
use serde_json::json;
use sqlx::{PgPool, Row};
use tokio::sync::Barrier;
use uuid::Uuid;

use crate::support::TestDatabase;

/// A dispatcher that always fails, so the retry and dead-letter paths can be
/// exercised without waiting for a real job body to misbehave.
struct AlwaysFails;

#[async_trait]
impl JobDispatcher for AlwaysFails {
    async fn dispatch(&self, _job: &ClaimedJob) -> Result<(), String> {
        Err("deliberate test failure".to_string())
    }
}

fn metrics() -> MetricsRegistry {
    MetricsRegistry::new("moira-test", None)
}

fn settings() -> WorkerSettings {
    WorkerSettings {
        // 1s rather than the 5s default so a rescheduled job's `run_at` is close
        // enough to now that the test can reason about it, and the clamp still has
        // something to clamp.
        retry_base_delay_seconds: 1,
        retry_max_delay_seconds: 4,
        ..WorkerSettings::default()
    }
}

fn queue(pool: &PgPool, settings: WorkerSettings) -> WorkerQueue {
    WorkerQueue::new(
        Arc::new(PgWorkerJobRepository::new(pool.clone())),
        Arc::new(settings),
        Uuid::now_v7(),
    )
}

async fn insert_job(pool: &PgPool, max_attempts: i32) -> Uuid {
    PgWorkerJobRepository::new(pool.clone())
        .enqueue(
            WorkerJobInsert {
                job_name: RETENTION_CLEANUP_WORKER.to_string(),
                payload: json!({ "test": true }),
                run_at: None,
                max_attempts,
            },
            10_000,
        )
        .await
        .expect("enqueue")
        .expect("the queue was not at capacity")
}

async fn status_of(pool: &PgPool, id: Uuid) -> (String, i32) {
    let row = sqlx::query("select status, attempts from worker_jobs where id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .expect("read the job row");
    (row.get("status"), row.get("attempts"))
}

/// Makes a rescheduled job due now, instead of sleeping out its backoff.
///
/// CONVENTIONS §3 bans timing guesses in interleaving tests. The backoff is real
/// and is asserted separately (`retry_delay`'s unit tests, and
/// `a_failed_job_is_rescheduled_into_the_future` below); what these tests need is
/// the *next* attempt, not the wait.
async fn make_due(pool: &PgPool, id: Uuid) {
    sqlx::query("update worker_jobs set run_at = now() where id = $1")
        .bind(id)
        .execute(pool)
        .await
        .expect("make the job due");
}

/// **The core P3-5 proof.** Two workers, one job, released together: exactly one
/// claim.
///
/// The guarantee under test is `for update skip locked` inside the materialised
/// victim CTE. Mutation-checked: replacing the CTE with
/// `where id in (select … limit $2)` still passes this test at one row — which is
/// why `claim_batch_is_bounded_by_its_limit` below exists as well, and why the
/// SQL-shape assertion in the repository is not redundant with either.
#[tokio::test]
async fn concurrent_claimers_never_claim_the_same_job() {
    let Some(database) = TestDatabase::create().await else {
        return;
    };
    let pool = database.pool.clone();
    let job_id = insert_job(&pool, 5).await;

    let barrier = Arc::new(Barrier::new(2));
    let mut handles = Vec::new();
    for _ in 0..2 {
        let repository = PgWorkerJobRepository::new(pool.clone());
        let barrier = barrier.clone();
        let replica = Uuid::now_v7();
        handles.push(tokio::spawn(async move {
            // The gate, not a sleep: both tasks are inside `claim_batch` at
            // overlapping times, which is the only way the race is real.
            barrier.wait().await;
            repository.claim_batch(replica, 10).await.expect("claim")
        }));
    }

    let mut claimed = Vec::new();
    for handle in handles {
        claimed.extend(handle.await.expect("claimer task"));
    }

    assert_eq!(
        claimed.len(),
        1,
        "two claimers returned {} rows for one job; `for update skip locked` is not \
         holding",
        claimed.len()
    );
    assert_eq!(claimed[0].id, job_id);
    assert_eq!(
        claimed[0].attempts, 1,
        "the claim increments attempts, so the first run reports 1"
    );
}

/// The bound the materialised CTE exists to provide.
///
/// This is the assertion that fails when the claim is written as
/// `where id in (select … limit $2)`: the planner may re-execute the sub-query per
/// outer row, `LockRows` skips rows the current command already modified, and each
/// re-execution returns fresh ids — so a `limit 1` batch claims an unbounded
/// number. The same shape already produced a real defect in this repository's
/// retention sweep.
#[tokio::test]
async fn claim_batch_is_bounded_by_its_limit() {
    let Some(database) = TestDatabase::create().await else {
        return;
    };
    let pool = database.pool.clone();
    for _ in 0..12 {
        insert_job(&pool, 5).await;
    }

    let repository = PgWorkerJobRepository::new(pool.clone());
    let claimed = repository
        .claim_batch(Uuid::now_v7(), 1)
        .await
        .expect("claim");
    assert_eq!(
        claimed.len(),
        1,
        "a batch of 1 claimed {} rows out of 12",
        claimed.len()
    );

    let still_pending: i64 = repository.pending_depth().await.expect("depth");
    assert_eq!(still_pending, 11);
}

/// A future `run_at` is what expresses backoff, so it must genuinely hide the row.
#[tokio::test]
async fn a_job_scheduled_in_the_future_is_not_claimable() {
    let Some(database) = TestDatabase::create().await else {
        return;
    };
    let repository = PgWorkerJobRepository::new(database.pool.clone());
    repository
        .enqueue(
            WorkerJobInsert {
                job_name: RETENTION_CLEANUP_WORKER.to_string(),
                payload: json!({}),
                run_at: Some(Utc::now() + ChronoDuration::hours(1)),
                max_attempts: 5,
            },
            10_000,
        )
        .await
        .expect("enqueue")
        .expect("capacity");

    assert!(
        repository
            .claim_batch(Uuid::now_v7(), 10)
            .await
            .expect("claim")
            .is_empty()
    );
}

#[tokio::test]
async fn a_failed_job_is_rescheduled_into_the_future() {
    let Some(database) = TestDatabase::create().await else {
        return;
    };
    let pool = database.pool.clone();
    let job_id = insert_job(&pool, 5).await;
    let queue = queue(&pool, settings());

    let outcome = queue
        .run_once(&AlwaysFails, &metrics())
        .await
        .expect("poll the queue");
    assert_eq!(outcome.claimed, 1);
    assert_eq!(outcome.rescheduled, 1);
    assert_eq!(outcome.dead_lettered, 0);

    let (status, attempts) = status_of(&pool, job_id).await;
    assert_eq!(status, "pending");
    assert_eq!(attempts, 1);

    // The retry is genuinely deferred, not immediately re-claimable.
    assert!(
        PgWorkerJobRepository::new(pool.clone())
            .claim_batch(Uuid::now_v7(), 10)
            .await
            .expect("claim")
            .is_empty(),
        "a rescheduled job must not be claimable before its backoff elapses"
    );
}

/// The whole point of a dead-letter state: never retried forever, never silently
/// dropped.
#[tokio::test]
async fn a_job_moves_to_dead_letter_after_max_attempts_and_is_never_reclaimed() {
    let Some(database) = TestDatabase::create().await else {
        return;
    };
    let pool = database.pool.clone();
    let job_id = insert_job(&pool, 3).await;
    let queue = queue(&pool, settings());
    let metrics = metrics();

    for attempt in 1..=3 {
        make_due(&pool, job_id).await;
        let outcome = queue
            .run_once(&AlwaysFails, &metrics)
            .await
            .expect("poll the queue");
        assert_eq!(outcome.claimed, 1, "attempt {attempt} was not claimed");
        let (status, attempts) = status_of(&pool, job_id).await;
        assert_eq!(attempts, attempt);
        if attempt < 3 {
            assert_eq!(status, "pending", "attempt {attempt} should retry");
        } else {
            assert_eq!(status, "dead_letter", "the budget of 3 is spent");
        }
    }

    // Terminal means terminal: no `run_at` reset makes it claimable again.
    make_due(&pool, job_id).await;
    assert!(
        PgWorkerJobRepository::new(pool.clone())
            .claim_batch(Uuid::now_v7(), 10)
            .await
            .expect("claim")
            .is_empty(),
        "a dead-lettered job was reclaimed"
    );
}

/// **The durability claim.** A replica that dies mid-job never writes a terminal
/// state; without the stale-claim sweep the row stays `running` forever and the
/// work is lost exactly as it was before this table existed.
#[tokio::test]
async fn a_job_orphaned_by_a_dead_replica_is_completed_by_another() {
    let Some(database) = TestDatabase::create().await else {
        return;
    };
    let pool = database.pool.clone();
    let job_id = insert_job(&pool, 5).await;

    // Replica A claims and then "dies" — the claim is never settled.
    let dead_replica = Uuid::now_v7();
    let claimed = PgWorkerJobRepository::new(pool.clone())
        .claim_batch(dead_replica, 10)
        .await
        .expect("claim");
    assert_eq!(claimed.len(), 1);
    assert_eq!(status_of(&pool, job_id).await.0, "running");

    // Age the claim rather than waiting out the threshold.
    sqlx::query("update worker_jobs set claimed_at = now() - interval '1 hour' where id = $1")
        .bind(job_id)
        .execute(&pool)
        .await
        .expect("age the claim");

    // Replica B polls: reclaim runs first, so the orphan is picked up in the same
    // poll rather than the one after.
    let queue = queue(
        &pool,
        WorkerSettings {
            queue_stale_claim_seconds: 60,
            ..settings()
        },
    );
    let outcome = queue
        .run_once(&StubJobDispatcher, &metrics())
        .await
        .expect("poll the queue");
    assert_eq!(outcome.reclaimed, 1, "the orphaned claim was not reclaimed");
    assert_eq!(outcome.claimed, 1);
    assert_eq!(outcome.completed, 1);

    let (status, attempts) = status_of(&pool, job_id).await;
    assert_eq!(status, "completed");
    assert_eq!(attempts, 2, "the reclaim costs an attempt, as it must");
}

/// A live claim must not be reclaimed out from under the replica running it.
#[tokio::test]
async fn a_live_claim_is_not_reclaimed() {
    let Some(database) = TestDatabase::create().await else {
        return;
    };
    let pool = database.pool.clone();
    let job_id = insert_job(&pool, 5).await;
    let repository = PgWorkerJobRepository::new(pool.clone());
    repository
        .claim_batch(Uuid::now_v7(), 10)
        .await
        .expect("claim");

    assert_eq!(
        repository.requeue_stale(300).await.expect("requeue"),
        0,
        "a claim seconds old was treated as stale"
    );
    assert_eq!(status_of(&pool, job_id).await.0, "running");
}

/// The emitter for `moira.error.worker_queue_capacity_exceeded`.
#[tokio::test]
async fn enqueue_beyond_the_depth_cap_returns_worker_queue_capacity_exceeded() {
    let Some(database) = TestDatabase::create().await else {
        return;
    };
    let pool = database.pool.clone();
    let queue = queue(
        &pool,
        WorkerSettings {
            queue_max_pending_jobs: 2,
            ..settings()
        },
    );
    let metrics = metrics();

    for slot in 1..=2 {
        queue
            .enqueue(RETENTION_CLEANUP_WORKER, json!({}), None, &metrics)
            .await
            .unwrap_or_else(|error| panic!("slot {slot} must fit: {error:?}"));
    }

    let error = queue
        .enqueue(RETENTION_CLEANUP_WORKER, json!({}), None, &metrics)
        .await
        .expect_err("the third enqueue exceeds the cap of 2");
    let AppError::Api {
        status,
        code,
        message,
        ..
    } = &error
    else {
        panic!("expected a coded API error, got {error:?}");
    };
    assert_eq!(*status, axum::http::StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(*code, "worker_queue_capacity_exceeded");
    assert!(!message.is_empty());

    // The catalog entry the code derives its message key from.
    assert!(moira::i18n::is_known_key(
        "moira.error.worker_queue_capacity_exceeded"
    ));
    assert!(
        moira::i18n::default_message_for_key("moira.error.worker_queue_capacity_exceeded")
            .is_some_and(|message| !message.is_empty())
    );
}

/// The cap counts *pending* work, so draining frees the slot. A cap that counted
/// every row would turn into a permanent refusal after the deployment's first
/// busy day.
#[tokio::test]
async fn the_depth_cap_frees_up_as_the_queue_drains() {
    let Some(database) = TestDatabase::create().await else {
        return;
    };
    let pool = database.pool.clone();
    let queue = queue(
        &pool,
        WorkerSettings {
            queue_max_pending_jobs: 1,
            ..settings()
        },
    );
    let metrics = metrics();

    queue
        .enqueue(RETENTION_CLEANUP_WORKER, json!({}), None, &metrics)
        .await
        .expect("the first job fits");
    assert!(
        queue
            .enqueue(RETENTION_CLEANUP_WORKER, json!({}), None, &metrics)
            .await
            .is_err()
    );

    queue
        .run_once(&StubJobDispatcher, &metrics)
        .await
        .expect("drain");

    queue
        .enqueue(RETENTION_CLEANUP_WORKER, json!({}), None, &metrics)
        .await
        .expect("the slot is free once the first job completed");
}

/// A job name outside `WORKER_JOB_NAMES` must never reach the table: it would be
/// undispatchable, and its `job_name` would be an unbounded metric label.
#[tokio::test]
async fn enqueueing_an_undeclared_job_name_is_refused() {
    let Some(database) = TestDatabase::create().await else {
        return;
    };
    let queue = queue(&database.pool, settings());
    assert!(
        queue
            .enqueue("not-a-declared-job", json!({}), None, &metrics())
            .await
            .is_err()
    );
}

// ---------------------------------------------------------------------------------------
// The two bounds on the dispatch loop, and panic isolation — issue #251 findings 4 and 5.
//
// Before #244 none of this was reachable: `StubJobDispatcher` is a name check and a log
// line, so it can neither hang nor panic. #247 put reqwest, sqlx and arbitrary handler
// bodies inside that same unprotected, unbounded, sequential await.
// ---------------------------------------------------------------------------------------

/// A dispatcher that never returns, standing in for `oauth-token-refresh` walking its
/// 100-credential batch against an identity provider that accepts connections and then
/// says nothing.
struct NeverReturns {
    entered: Arc<AtomicUsize>,
}

#[async_trait]
impl JobDispatcher for NeverReturns {
    async fn dispatch(&self, _job: &ClaimedJob) -> Result<(), String> {
        self.entered.fetch_add(1, Ordering::SeqCst);
        // Far beyond any budget in this file, so a pass can only come from the timeout.
        tokio::time::sleep(std::time::Duration::from_secs(3_600)).await;
        Ok(())
    }
}

/// Panics on the first job it is handed and succeeds on every one after, so a single test
/// can assert both that the panic became a failed attempt and that the poll survived it.
struct PanicsOnce {
    seen: Arc<AtomicUsize>,
}

#[async_trait]
impl JobDispatcher for PanicsOnce {
    async fn dispatch(&self, _job: &ClaimedJob) -> Result<(), String> {
        if self.seen.fetch_add(1, Ordering::SeqCst) == 0 {
            panic!("a deliberate handler panic");
        }
        Ok(())
    }
}

/// Completes every job, and raises a flag once it has handled `stop_after` of them, so the
/// caller's stop predicate flips part-way through a batch with no timing involved.
struct StopsItselfAfter {
    handled: Arc<AtomicUsize>,
    stop_after: usize,
    stop: Arc<AtomicBool>,
}

#[async_trait]
impl JobDispatcher for StopsItselfAfter {
    async fn dispatch(&self, _job: &ClaimedJob) -> Result<(), String> {
        if self.handled.fetch_add(1, Ordering::SeqCst) + 1 >= self.stop_after {
            self.stop.store(true, Ordering::SeqCst);
        }
        Ok(())
    }
}

async fn last_error_of(pool: &PgPool, id: Uuid) -> Option<String> {
    sqlx::query("select last_error from worker_jobs where id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .expect("read the job row")
        .get("last_error")
}

/// The per-job half of finding 4: a handler that hangs is recorded as a failed attempt and
/// retried, instead of holding the claim until another replica reclaims the row and runs the
/// same job a second time.
#[tokio::test]
async fn a_handler_that_hangs_is_failed_at_its_per_job_timeout_and_retried() {
    let Some(database) = TestDatabase::create().await else {
        return;
    };
    let pool = database.pool.clone();
    let job_id = insert_job(&pool, 5).await;

    let entered = Arc::new(AtomicUsize::new(0));
    let outcome = queue(
        &pool,
        WorkerSettings {
            queue_job_timeout_seconds: 1,
            // Far above the per-job timeout, so the *per-job* bound is what this test
            // pins — the batch bound below has its own test.
            queue_stale_claim_seconds: 3_600,
            ..settings()
        },
    )
    .run_once(
        &NeverReturns {
            entered: entered.clone(),
        },
        &metrics(),
    )
    .await
    .expect("poll the queue");

    assert_eq!(entered.load(Ordering::SeqCst), 1, "the handler never ran");
    assert_eq!(outcome.claimed, 1);
    assert_eq!(outcome.completed, 0, "a hung handler must not be completed");
    assert_eq!(outcome.rescheduled, 1);
    assert_eq!(outcome.undispatched, 0);

    let (status, _attempts) = status_of(&pool, job_id).await;
    assert_eq!(status, "pending", "the job must be retried, not abandoned");
    let error = last_error_of(&pool, job_id)
        .await
        .expect("a recorded error");
    assert!(
        error.contains("exceeded its"),
        "the row must say the budget ended it: {error}"
    );
}

/// The batch half of finding 4, and the one that closes the concrete failure: eight jobs
/// each inside their own per-job timeout can still keep one poll running past
/// `queue_stale_claim_seconds`, at which point another replica's `requeue_stale` hands a
/// row this replica is still executing to a second executor.
///
/// The per-job timeout here is set *above* the batch budget on purpose. That pair is
/// exactly what `Settings::validate_coordination` now rejects, and running it anyway is the
/// point: the batch bound has to hold on the values as given, because `WorkerSettings` is
/// constructible without ever passing through validation.
#[tokio::test]
async fn a_batch_that_exhausts_its_dispatch_budget_leaves_the_rest_running_for_the_sweep() {
    let Some(database) = TestDatabase::create().await else {
        return;
    };
    let pool = database.pool.clone();
    let first = insert_job(&pool, 5).await;
    let second = insert_job(&pool, 5).await;

    let entered = Arc::new(AtomicUsize::new(0));
    let outcome = queue(
        &pool,
        WorkerSettings {
            max_concurrent_jobs: 2,
            // 80% of 2s is a 1s budget for the whole batch. The first job's slice is
            // `min(per-job timeout, time left on the batch budget)`, so with the per-job
            // timeout an hour away the slice *is* the remaining budget — the first job
            // times out precisely when the batch deadline passes, leaving nothing for the
            // second. No sleep in the test, and no race to lose.
            queue_stale_claim_seconds: 2,
            queue_job_timeout_seconds: 3_600,
            ..settings()
        },
    )
    .run_once(
        &NeverReturns {
            entered: entered.clone(),
        },
        &metrics(),
    )
    .await
    .expect("poll the queue");

    assert_eq!(outcome.claimed, 2);
    assert_eq!(
        entered.load(Ordering::SeqCst),
        1,
        "the second job must never have been handed to a handler"
    );
    assert_eq!(outcome.undispatched, 1);
    assert_eq!(outcome.rescheduled, 1, "the first job timed out");

    assert_eq!(status_of(&pool, first).await.0, "pending");
    assert_eq!(
        status_of(&pool, second).await.0,
        "running",
        "an undispatched job must be left for `requeue_stale`, not completed or failed by \
         a replica that never ran it"
    );
}

/// The shutdown half of finding 4. `run_supervisor` awaits the poll inline inside one arm
/// of its `select!`, so before this the `shutdown.changed()` arm could not fire until the
/// whole claimed batch had been dispatched. The stop predicate is read *between* jobs, so
/// no handler is cancelled half-written.
#[tokio::test]
async fn a_stop_signal_part_way_through_a_batch_stops_dispatching_the_rest() {
    let Some(database) = TestDatabase::create().await else {
        return;
    };
    let pool = database.pool.clone();
    let first = insert_job(&pool, 5).await;
    let second = insert_job(&pool, 5).await;
    let third = insert_job(&pool, 5).await;

    let handled = Arc::new(AtomicUsize::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let dispatcher = StopsItselfAfter {
        handled: handled.clone(),
        stop_after: 1,
        stop: stop.clone(),
    };
    let stop_predicate = {
        let stop = stop.clone();
        move || stop.load(Ordering::SeqCst)
    };

    let outcome = queue(
        &pool,
        WorkerSettings {
            max_concurrent_jobs: 3,
            ..settings()
        },
    )
    .run_once_until(&dispatcher, &metrics(), &stop_predicate)
    .await
    .expect("poll the queue");

    assert_eq!(outcome.claimed, 3);
    assert_eq!(
        handled.load(Ordering::SeqCst),
        1,
        "dispatching continued after the stop signal"
    );
    assert_eq!(outcome.completed, 1, "the in-flight job must still settle");
    assert_eq!(outcome.undispatched, 2);
    assert_eq!(status_of(&pool, first).await.0, "completed");
    for id in [second, third] {
        assert_eq!(status_of(&pool, id).await.0, "running");
    }
}

/// Finding 5. `run_supervisor` is a bare `tokio::spawn` whose `JoinHandle` is awaited only
/// by `WorkerSupervisor::shutdown`, which discards the `JoinError` — so an unwinding
/// handler killed the supervisor task for the remaining life of the process (no polls, no
/// retention sweep, no `moira_worker_tick`, no `leader.resign()`) while `/health/ready`,
/// which does not consult the supervisor, kept the pod in service.
#[tokio::test]
async fn a_panicking_handler_fails_its_own_job_and_leaves_the_poll_running() {
    let Some(database) = TestDatabase::create().await else {
        return;
    };
    let pool = database.pool.clone();
    let panicking = insert_job(&pool, 5).await;
    let survivor = insert_job(&pool, 5).await;

    let seen = Arc::new(AtomicUsize::new(0));
    let outcome = queue(
        &pool,
        WorkerSettings {
            max_concurrent_jobs: 2,
            ..settings()
        },
    )
    .run_once(&PanicsOnce { seen: seen.clone() }, &metrics())
    .await
    .expect("the poll itself must return, not unwind");

    assert_eq!(
        seen.load(Ordering::SeqCst),
        2,
        "the poll stopped at the panic"
    );
    assert_eq!(outcome.rescheduled, 1);
    assert_eq!(outcome.completed, 1);
    assert_eq!(status_of(&pool, panicking).await.0, "pending");
    assert_eq!(status_of(&pool, survivor).await.0, "completed");

    let error = last_error_of(&pool, panicking)
        .await
        .expect("a recorded error");
    assert!(error.contains("panicked"), "{error}");
    assert!(
        !error.contains("a deliberate handler panic"),
        "the panic payload is arbitrary handler text and `worker_jobs.last_error` is stored \
         unredacted, so it must not be carried into the row: {error}"
    );
}
