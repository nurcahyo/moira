//! The durable worker queue against a real PostgreSQL (plan 10 wave 2, P3-5).
//!
//! Everything here runs on the **default, Redis-off** configuration. The queue is
//! Postgres-backed by design — plan 10 §0.4b — so a deployment that never enables
//! Redis gets all of this, and these tests are the proof.

mod support;

use std::sync::Arc;

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

use support::TestDatabase;

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
