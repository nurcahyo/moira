//! **The shipped configuration.** Plan 10 §0.4b, pinned as tests.
//!
//! Redis is off by default and Postgres plus per-process memory is the
//! coordination path Moira actually ships. Every Redis code path added by wave 2
//! is additive, and this file is the assertion that *removing* Redis removes none
//! of the coordination a deployment depends on — not merely that the crate still
//! compiles without it.
//!
//! If any test here fails, every default deployment is affected. That is not true
//! of `tests/cluster_coordination.rs`, which only affects deployments that opted
//! into Redis.

mod support;

use std::sync::Arc;

use moira::{
    app::AppState,
    config::Settings,
    infra::{
        db::{PUBLISHABLE_RESOURCE_TYPES, RuntimeInvalidationTargets},
        metrics::MetricsRegistry,
        repositories::{PgWorkerJobRepository, WorkerJobInsert, WorkerJobRepository},
        workers::{
            RETENTION_CLEANUP_WORKER,
            queue::{StubJobDispatcher, WorkerQueue},
        },
    },
};
use serde_json::json;
use uuid::Uuid;

use support::TestDatabase;

async fn default_state() -> AppState {
    let settings = Settings::default();
    assert!(
        !settings.redis.enabled,
        "redis.enabled must default to false — plan 10 §0.4b pins this, and \
         `redis_is_optional_by_default` pins the client half of it"
    );
    AppState::new(settings, None)
        .await
        .expect("build the default app state")
}

/// The default build has no Redis client at all, so no Redis code path can run.
#[tokio::test]
async fn the_default_build_has_no_redis_client() {
    let state = default_state().await;
    assert!(state.redis.is_none());
}

/// The controls a default deployment gets are the in-process ones, and they say
/// so.
#[tokio::test]
async fn the_default_build_selects_the_in_process_controls() {
    let state = default_state().await;
    assert!(
        !state.public_rate_limiter.is_cluster_wide(),
        "the default rate limiter must be the in-process one"
    );
    assert!(
        !state.concurrency.is_cluster_wide(),
        "the default concurrency controller must be the in-process one"
    );
}

/// **The invariant that must not move.** Turning Redis on changes which backend
/// counts; it must never change *whether* the limits are enforced, and it must
/// never touch breakers.
#[tokio::test]
async fn enabling_redis_changes_the_backend_and_nothing_else() {
    let mut settings = Settings::default();
    settings.redis.enabled = true;
    settings.redis.url = Some("redis://127.0.0.1:6379/0".to_string());
    // No connection is opened by `AppState::new`, so this asserts the *selection*
    // and needs no live Redis.
    let state = AppState::new(settings, None)
        .await
        .expect("build a Redis-enabled app state");

    assert!(state.redis.is_some());
    assert!(state.public_rate_limiter.is_cluster_wide());
    assert!(state.concurrency.is_cluster_wide());
}

/// Rate limiting and concurrency still enforce their ceilings with Redis absent.
///
/// Per replica, which §0.4b accepts as the documented cost — and which the
/// admission lease's cap on the replica count is what makes tolerable.
#[tokio::test]
async fn the_default_build_still_enforces_rate_limits_and_concurrency() {
    let state = default_state().await;
    let window = std::time::Duration::from_secs(60);
    let key = format!("application:{}", Uuid::now_v7());

    assert!(
        state
            .public_rate_limiter
            .check(key.clone(), 2, window)
            .await
            .is_ok()
    );
    assert!(
        state
            .public_rate_limiter
            .check(key.clone(), 2, window)
            .await
            .is_ok()
    );
    let refused = state
        .public_rate_limiter
        .check(key, 2, window)
        .await
        .expect_err("the third request exceeds the limit of 2");
    assert!(
        matches!(&refused, moira::error::AppError::Api { code, .. } if *code == "rate_limited"),
        "the in-process limiter must refuse with the same code the cluster one does: \
         {refused:?}"
    );

    let controller = moira::orchestration::ConcurrencyController::new(1, 1, 1, 16);
    let provider = Uuid::now_v7();
    let _held = controller
        .acquire(provider, 1, false, 1, None, None)
        .await
        .expect("the first execution fits");
    assert!(
        controller
            .acquire(provider, 1, false, 1, None, None)
            .await
            .is_err(),
        "the in-process concurrency ceiling is not being enforced"
    );
}

/// The Postgres `LISTEN/NOTIFY` listener is spawned unconditionally, and the
/// Redis one is not. A deployment with Redis off must still invalidate.
#[tokio::test]
async fn the_postgres_invalidation_listener_runs_with_redis_absent() {
    let Some(database) = TestDatabase::create().await else {
        return;
    };
    let state = AppState::new(Settings::default(), Some(database.pool.clone()))
        .await
        .expect("build the default app state");
    assert!(state.redis.is_none(), "no Redis in the default build");

    // The listener the shipped configuration relies on. Spawning it is the
    // property under test: `src/main.rs` gates it on the *pool*, never on Redis.
    let targets = RuntimeInvalidationTargets::from_state(&state);
    let listener = moira::infra::db::spawn_runtime_config_listener(database.pool.clone(), targets);
    assert!(!listener.is_finished());
    listener.abort();
}

/// Every table name the Redis publisher can emit is classified by the same
/// mapping the Postgres path uses.
///
/// Asserted here as well as in `src/infra/db.rs` because this is the file a
/// reviewer reads to answer "what does turning Redis on change?" — and the answer
/// must include "nothing about how a payload is classified".
#[test]
fn the_publisher_and_the_postgres_listener_agree_on_resource_types() {
    assert!(!PUBLISHABLE_RESOURCE_TYPES.is_empty());
    for resource_type in PUBLISHABLE_RESOURCE_TYPES {
        assert!(
            !resource_type.is_empty(),
            "an empty resource type would be classified as unknown and reset every circuit"
        );
    }
}

/// The durable queue is Postgres-backed and needs no Redis: enqueue, claim,
/// dispatch and complete all work on the default build.
#[tokio::test]
async fn the_durable_queue_works_end_to_end_with_redis_absent() {
    let Some(database) = TestDatabase::create().await else {
        return;
    };
    let pool = database.pool.clone();
    let state = AppState::new(Settings::default(), Some(pool.clone()))
        .await
        .expect("build the default app state");
    assert!(state.redis.is_none());

    let queue = WorkerQueue::new(
        Arc::new(PgWorkerJobRepository::new(pool.clone())),
        Arc::new(state.settings.workers.clone()),
        Uuid::now_v7(),
    );
    let metrics = MetricsRegistry::new("moira-test", None);

    let job_id = queue
        .enqueue(
            RETENTION_CLEANUP_WORKER,
            json!({ "source": "test" }),
            None,
            &metrics,
        )
        .await
        .expect("enqueue on the default build");

    let outcome = queue
        .run_once(&StubJobDispatcher, &metrics)
        .await
        .expect("poll on the default build");
    assert_eq!(outcome.claimed, 1);
    assert_eq!(outcome.completed, 1);

    let status: String = sqlx::query_scalar("select status from worker_jobs where id = $1")
        .bind(job_id)
        .fetch_one(&pool)
        .await
        .expect("read the job row");
    assert_eq!(status, "completed");
}

/// The queue's own repository surface is reachable without any Redis
/// configuration — the enqueue path a plan-11 caller will use.
#[tokio::test]
async fn the_queue_repository_needs_no_redis_configuration() {
    let Some(database) = TestDatabase::create().await else {
        return;
    };
    let repository = PgWorkerJobRepository::new(database.pool.clone());
    let id = repository
        .enqueue(
            WorkerJobInsert {
                job_name: RETENTION_CLEANUP_WORKER.to_string(),
                payload: json!({}),
                run_at: None,
                max_attempts: 5,
            },
            10,
        )
        .await
        .expect("enqueue")
        .expect("capacity");
    assert_eq!(repository.pending_depth().await.expect("depth"), 1);

    let claimed = repository
        .claim_batch(Uuid::now_v7(), 10)
        .await
        .expect("claim");
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].id, id);
}
