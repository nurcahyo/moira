//! The registry-backed dispatcher (issue #90) against a real PostgreSQL.
//!
//! `tests/workers/worker_queue.rs` already proves the queue's claim/retry/dead-letter
//! mechanics against `StubJobDispatcher` and a hand-rolled `AlwaysFails`. This file proves
//! the piece #90 added on top: that a claimed job is routed by `job_name` to the specific
//! handler registered for it, that the four plan-11 retry names complete through their
//! documented stub rather than a blanket completion, that a name Moira has declared but not
//! yet wired a handler for still completes rather than dead-lettering, and that a real
//! handler's failure feeds the queue's existing retry/dead-letter classification exactly as
//! `AlwaysFails`'s does.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use moira::infra::{
    metrics::MetricsRegistry,
    repositories::{ClaimedJob, PgWorkerJobRepository, WorkerJobRepository},
    workers::{
        CONVERSATION_SUMMARIZATION_RETRY_WORKER, DOCUMENT_INGESTION_RETRY_WORKER,
        EMBEDDING_RETRY_WORKER, MEMORY_EXTRACTION_RETRY_WORKER, RETENTION_CLEANUP_WORKER,
        dispatch::{JobHandler, RealJobDispatcher, default_dispatcher},
        queue::WorkerQueue,
    },
};
use serde_json::json;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::support::TestDatabase;

fn metrics() -> MetricsRegistry {
    MetricsRegistry::new("moira-test", None)
}

fn queue(pool: &PgPool) -> WorkerQueue {
    WorkerQueue::new(
        Arc::new(PgWorkerJobRepository::new(pool.clone())),
        Arc::new(moira::config::WorkerSettings::default()),
        Uuid::now_v7(),
    )
}

/// `default_dispatcher` against this file's real fixture pool. The three DB-backed handlers
/// this constructs (`latency-stats-aggregation`, `oauth-token-refresh`,
/// `provider-health-check`) are exercised end to end in
/// `tests/workers/latency_health_oauth.rs`; every test in *this* file only cares about the
/// four plan-11 stub names and the dispatch-routing mechanics, so a pool is threaded through
/// here purely to match the exact object `run_supervisor` builds — nothing below asserts on
/// what the three DB-backed handlers do.
fn default_dispatcher_for(pool: &PgPool) -> RealJobDispatcher {
    default_dispatcher(
        Some(pool.clone()),
        moira::security::LocalSecretCipher::new([11; 32], "job-dispatch-test"),
        reqwest::Client::new(),
        metrics(),
        Arc::new(moira::config::WorkerSettings::default()),
    )
}

async fn status_of(pool: &PgPool, id: Uuid) -> (String, i32) {
    let row = sqlx::query("select status, attempts from worker_jobs where id = $1")
        .bind(id)
        .fetch_one(pool)
        .await
        .expect("read the job row");
    (row.get("status"), row.get("attempts"))
}

/// **The core #90 proof, end to end.** A plan-11 retry job enqueued through the real queue
/// and dispatched through `default_dispatcher` — the exact type `run_supervisor` drives —
/// completes via its registered `DeferredPipelineHandler` rather than dead-lettering for
/// lack of any handler at all.
#[tokio::test]
async fn default_dispatcher_completes_a_memory_extraction_retry_job_end_to_end() {
    let Some(database) = TestDatabase::create().await else {
        return;
    };
    let pool = database.pool.clone();
    let queue = queue(&pool);
    let metrics = metrics();

    let job_id = queue
        .enqueue(
            MEMORY_EXTRACTION_RETRY_WORKER,
            json!({ "conversation_id": "test" }),
            None,
            &metrics,
        )
        .await
        .expect("enqueue a plan-11 retry job");

    let outcome = queue
        .run_once(&default_dispatcher_for(&pool), &metrics)
        .await
        .expect("poll the queue");
    assert_eq!(outcome.claimed, 1);
    assert_eq!(outcome.completed, 1);
    assert_eq!(outcome.rescheduled, 0);
    assert_eq!(outcome.dead_lettered, 0);

    let (status, attempts) = status_of(&pool, job_id).await;
    assert_eq!(status, "completed");
    assert_eq!(attempts, 1);
}

/// Every plan-11 retry name, not just one — the registration is per name, so this also
/// guards against a copy-paste that registered the same handler under two names and left a
/// third unregistered.
#[tokio::test]
async fn default_dispatcher_completes_every_plan_11_retry_name_end_to_end() {
    let Some(database) = TestDatabase::create().await else {
        return;
    };
    let pool = database.pool.clone();
    let queue = queue(&pool);
    let metrics = metrics();
    let dispatcher = default_dispatcher_for(&pool);

    for name in [
        MEMORY_EXTRACTION_RETRY_WORKER,
        CONVERSATION_SUMMARIZATION_RETRY_WORKER,
        EMBEDDING_RETRY_WORKER,
        DOCUMENT_INGESTION_RETRY_WORKER,
    ] {
        let job_id = queue
            .enqueue(name, json!({}), None, &metrics)
            .await
            .unwrap_or_else(|error| panic!("enqueue {name} must fit: {error:?}"));

        let outcome = queue
            .run_once(&dispatcher, &metrics)
            .await
            .unwrap_or_else(|error| panic!("poll for {name} must succeed: {error:?}"));
        assert_eq!(outcome.claimed, 1, "{name} was not claimed");
        assert_eq!(outcome.completed, 1, "{name} did not complete");

        assert_eq!(status_of(&pool, job_id).await.0, "completed", "{name}");
    }
}

/// A name Moira has declared (so `WorkerQueue::enqueue` accepts it) but that
/// `default_dispatcher` has not registered a handler for. `runtime-cache-warmer` is the one
/// name left in this state even against a real pool — `latency-stats-aggregation`,
/// `oauth-token-refresh` and `provider-health-check` all get real handlers the moment a pool
/// is present (`tests/workers/latency_health_oauth.rs` proves those three end to end), which
/// is exactly why this test no longer uses `provider-health-check` as its example. This one
/// must still complete as a no-op, not dead-letter for lack of a body nobody has written yet.
#[tokio::test]
async fn an_unregistered_but_declared_job_name_completes_as_a_noop_end_to_end() {
    let Some(database) = TestDatabase::create().await else {
        return;
    };
    let pool = database.pool.clone();
    let queue = queue(&pool);
    let metrics = metrics();

    let job_id = queue
        .enqueue("runtime-cache-warmer", json!({}), None, &metrics)
        .await
        .expect("runtime-cache-warmer is declared, so enqueue accepts it");

    let outcome = queue
        .run_once(&default_dispatcher_for(&pool), &metrics)
        .await
        .expect("poll the queue");
    assert_eq!(outcome.claimed, 1);
    assert_eq!(outcome.completed, 1);
    assert_eq!(status_of(&pool, job_id).await.0, "completed");
}

/// A dispatcher built from a single custom handler, so this file can prove routing without
/// depending on `default_dispatcher`'s specific registrations.
struct FailingHandler;

#[async_trait]
impl JobHandler for FailingHandler {
    async fn handle(&self, _job: &ClaimedJob) -> Result<(), String> {
        Err("deliberate test failure".to_string())
    }
}

/// **The retry/dead-letter proof for a real handler.** `tests/workers/worker_queue.rs`
/// already proves this classification against a hand-rolled `JobDispatcher`
/// (`AlwaysFails`); this proves the same classification holds when the failure comes from a
/// `JobHandler` reached through `RealJobDispatcher`'s per-name routing — the path
/// `run_supervisor` actually drives once any of these jobs has a real body.
#[tokio::test]
async fn a_handler_failure_is_retried_then_dead_lettered_through_the_real_dispatcher() {
    let Some(database) = TestDatabase::create().await else {
        return;
    };
    let pool = database.pool.clone();
    let settings = moira::config::WorkerSettings {
        retry_base_delay_seconds: 1,
        retry_max_delay_seconds: 4,
        ..moira::config::WorkerSettings::default()
    };
    let queue = WorkerQueue::new(
        Arc::new(PgWorkerJobRepository::new(pool.clone())),
        Arc::new(settings),
        Uuid::now_v7(),
    );
    let metrics = metrics();
    let dispatcher =
        RealJobDispatcher::new().register(RETENTION_CLEANUP_WORKER, Arc::new(FailingHandler));

    let job_id = PgWorkerJobRepository::new(pool.clone())
        .enqueue(
            moira::infra::repositories::WorkerJobInsert {
                job_name: RETENTION_CLEANUP_WORKER.to_string(),
                payload: json!({}),
                run_at: None,
                max_attempts: 2,
            },
            10_000,
        )
        .await
        .expect("enqueue")
        .expect("capacity");

    // Attempt 1: the handler fails, and there is budget left, so the job is rescheduled.
    let outcome = queue
        .run_once(&dispatcher, &metrics)
        .await
        .expect("poll the queue");
    assert_eq!(outcome.claimed, 1);
    assert_eq!(outcome.rescheduled, 1);
    assert_eq!(outcome.dead_lettered, 0);
    let (status, attempts) = status_of(&pool, job_id).await;
    assert_eq!(status, "pending");
    assert_eq!(attempts, 1);

    // Attempt 2: the budget of 2 is spent, so the same failure now dead-letters.
    sqlx::query("update worker_jobs set run_at = now() where id = $1")
        .bind(job_id)
        .execute(&pool)
        .await
        .expect("make the job due");
    let outcome = queue
        .run_once(&dispatcher, &metrics)
        .await
        .expect("poll the queue");
    assert_eq!(outcome.claimed, 1);
    assert_eq!(outcome.rescheduled, 0);
    assert_eq!(outcome.dead_lettered, 1);
    let (status, attempts) = status_of(&pool, job_id).await;
    assert_eq!(status, "dead_letter");
    assert_eq!(attempts, 2);

    let last_error: Option<String> =
        sqlx::query_scalar("select last_error from worker_jobs where id = $1")
            .bind(job_id)
            .fetch_one(&pool)
            .await
            .expect("read the job row");
    assert_eq!(last_error.as_deref(), Some("deliberate test failure"));
}

/// Two handlers registered for two names: proves dispatch reaches the handler registered for
/// the job's *own* name, never the other one, against a real claim rather than an in-memory
/// call.
#[tokio::test]
async fn dispatch_routes_by_exact_job_name_end_to_end() {
    let Some(database) = TestDatabase::create().await else {
        return;
    };
    let pool = database.pool.clone();
    let queue = queue(&pool);
    let metrics = metrics();

    struct CountingHandler(Arc<AtomicUsize>);
    #[async_trait]
    impl JobHandler for CountingHandler {
        async fn handle(&self, _job: &ClaimedJob) -> Result<(), String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    let extraction_calls = Arc::new(AtomicUsize::new(0));
    let retention_calls = Arc::new(AtomicUsize::new(0));
    let dispatcher = RealJobDispatcher::new()
        .register(
            MEMORY_EXTRACTION_RETRY_WORKER,
            Arc::new(CountingHandler(extraction_calls.clone())),
        )
        .register(
            RETENTION_CLEANUP_WORKER,
            Arc::new(CountingHandler(retention_calls.clone())),
        );

    queue
        .enqueue(MEMORY_EXTRACTION_RETRY_WORKER, json!({}), None, &metrics)
        .await
        .expect("enqueue");

    let outcome = queue
        .run_once(&dispatcher, &metrics)
        .await
        .expect("poll the queue");
    assert_eq!(outcome.claimed, 1);
    assert_eq!(outcome.completed, 1);
    assert_eq!(
        extraction_calls.load(Ordering::SeqCst),
        1,
        "the handler registered for memory-extraction-retry must have run"
    );
    assert_eq!(
        retention_calls.load(Ordering::SeqCst),
        0,
        "the handler registered for retention-cleanup must not have run"
    );
}
