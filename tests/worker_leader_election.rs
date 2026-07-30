//! Worker leader election against a real PostgreSQL (plan 10 wave 1, P3-4).
//!
//! The property under test is *exactly one*. A leader-election test that only
//! ever has one candidate proves nothing at all — it cannot distinguish "the
//! lock works" from "the lock is a no-op that returns true" — so every test here
//! that claims a winner runs at least two contenders released together by a
//! [`Barrier`], never staggered by a `sleep`.
//!
//! Each test takes its own database from [`support::TestDatabase`]. PostgreSQL
//! advisory locks are database-scoped, which is what keeps the `b"moiralrt"`
//! leader key away from `b"MOIRARET"` in `tests/retention_worker.rs` — a
//! test-only guard, but one taken against the same server.

mod support;

use std::{sync::Arc, time::Duration};

use moira::{
    app::AppState,
    config::{ClusterSettings, Settings},
    infra::workers::{
        self,
        leader::{LeaderElection, LeaderLock, LeaderState, leader_lock_key},
    },
};
use sqlx::PgPool;
use tokio::{sync::Barrier, task::JoinSet};

use support::TestDatabase;

/// Enough for barrier-released contenders on a contended database. Spent only by
/// a real failure.
const CONTENTION_BUDGET: Duration = Duration::from_secs(30);

/// How long [`a_supervisor_follower_runs_no_retention_sweep`] waits for a sweep
/// that must never come. Generous relative to the 1s retention cadence it runs
/// with, so a passing test means "the follower was gated", not "the test was
/// impatient".
const NON_OCCURRENCE_BUDGET: Duration = Duration::from_secs(5);

/// N contenders race for one lock. Returns how many won.
async fn contend(pool: &PgPool, contenders: usize) -> usize {
    let barrier = Arc::new(Barrier::new(contenders));
    let mut set = JoinSet::new();
    for _ in 0..contenders {
        let pool = pool.clone();
        let barrier = barrier.clone();
        set.spawn(async move {
            barrier.wait().await;
            LeaderLock::try_acquire(&pool, workers::RETENTION_CLEANUP_WORKER)
                .await
                .expect("the acquisition attempt itself must not error")
        });
    }
    let locks = tokio::time::timeout(CONTENTION_BUDGET, set.join_all())
        .await
        .expect("every contender finished within the budget");

    let winners = locks.iter().filter(|lock| lock.is_some()).count();
    // Released here rather than dropped, so the next phase of a test is not
    // racing PostgreSQL's reaping of a closed backend.
    for lock in locks.into_iter().flatten() {
        lock.release().await;
    }
    winners
}

#[tokio::test]
async fn exactly_one_of_two_contenders_holds_leadership() {
    let Some(database) = TestDatabase::create().await else {
        return;
    };
    assert_eq!(
        contend(&database.pool, 2).await,
        1,
        "two contenders for one leader lock must produce exactly one leader — not zero \
         (the lock is unreachable) and not two (the lock is a no-op)"
    );
}

/// Four, because two can pass by accident on a serialising scheduler and four
/// cannot.
#[tokio::test]
async fn exactly_one_of_four_contenders_holds_leadership() {
    let Some(database) = TestDatabase::create().await else {
        return;
    };
    assert_eq!(contend(&database.pool, 4).await, 1);
}

/// The failure mode a pooled connection would produce.
///
/// `pg_try_advisory_lock` is session-scoped and **re-entrant within a session**.
/// If the lock were taken on a pooled connection, the leader's connection would
/// go back to the pool still holding it, a follower could draw that same
/// connection, and its `pg_try_advisory_lock` would succeed — two leaders. This
/// test forces exactly that scenario: a pool of one connection, so any second
/// contender is guaranteed to reuse the first one's socket if the lock is not on
/// a detached connection.
#[tokio::test]
async fn a_second_contender_never_inherits_leadership_through_a_shared_pool() {
    let Some(database) = TestDatabase::create_with_max_connections(1).await else {
        return;
    };

    let first = LeaderLock::try_acquire(&database.pool, workers::RETENTION_CLEANUP_WORKER)
        .await
        .expect("the first acquisition runs")
        .expect("an uncontended lock must be granted");

    let second = tokio::time::timeout(
        CONTENTION_BUDGET,
        LeaderLock::try_acquire(&database.pool, workers::RETENTION_CLEANUP_WORKER),
    )
    .await
    .expect("the second acquisition must not hang: the leader's connection is not the pool's")
    .expect("the second acquisition runs");

    assert!(
        second.is_none(),
        "a follower must not inherit leadership through a connection the pool handed back"
    );

    first.release().await;
}

#[tokio::test]
async fn leadership_transfers_to_a_follower_after_the_holder_releases() {
    let Some(database) = TestDatabase::create().await else {
        return;
    };
    let held = LeaderLock::try_acquire(&database.pool, workers::RETENTION_CLEANUP_WORKER)
        .await
        .expect("acquire runs")
        .expect("an uncontended lock must be granted");
    assert_eq!(held.job_name(), workers::RETENTION_CLEANUP_WORKER);

    assert!(
        LeaderLock::try_acquire(&database.pool, workers::RETENTION_CLEANUP_WORKER)
            .await
            .expect("acquire runs")
            .is_none(),
        "a held lock must refuse a second holder before the transfer proves anything"
    );

    held.release().await;

    let successor = LeaderLock::try_acquire(&database.pool, workers::RETENTION_CLEANUP_WORKER)
        .await
        .expect("acquire runs");
    assert!(
        successor.is_some(),
        "leadership must transfer once the holder resigns, or a rolling update leaves the \
         singleton job unowned"
    );
    successor.expect("checked").release().await;
}

/// Leadership is sticky. A leader that re-contested from scratch every tick
/// would hand the job back and forth with a follower.
#[tokio::test]
async fn a_leader_keeps_leadership_across_ticks_and_a_follower_stays_a_follower() {
    let Some(database) = TestDatabase::create().await else {
        return;
    };
    let mut leader = LeaderElection::new(workers::RETENTION_CLEANUP_WORKER, true);
    let mut follower = LeaderElection::new(workers::RETENTION_CLEANUP_WORKER, true);

    assert!(leader.should_run(Some(&database.pool)).await);
    assert_eq!(leader.state(), LeaderState::Leader);

    for _ in 0..3 {
        assert!(
            leader.should_run(Some(&database.pool)).await,
            "the holder must stay leader across ticks"
        );
        assert!(
            !follower.should_run(Some(&database.pool)).await,
            "the other replica must stay a follower while the lock is held"
        );
        assert_eq!(follower.state(), LeaderState::Follower);
    }

    leader.resign().await;
    assert_eq!(leader.state(), LeaderState::Follower);
    assert!(
        follower.should_run(Some(&database.pool)).await,
        "the follower must be promoted once the leader resigns"
    );
    follower.resign().await;
}

/// The default path, unchanged: election off means every replica runs the job.
#[tokio::test]
async fn a_disabled_election_lets_every_replica_run_the_job() {
    let Some(database) = TestDatabase::create().await else {
        return;
    };
    let mut first = LeaderElection::new(workers::RETENTION_CLEANUP_WORKER, false);
    let mut second = LeaderElection::new(workers::RETENTION_CLEANUP_WORKER, false);

    assert!(first.should_run(Some(&database.pool)).await);
    assert!(
        second.should_run(Some(&database.pool)).await,
        "with election off, a second replica must still sweep — this is the pre-plan-10 \
         behaviour every single-replica deployment relies on"
    );
    assert_eq!(first.state(), LeaderState::Disabled);
    assert_eq!(second.state(), LeaderState::Disabled);
}

/// Wiring test: a supervisor whose leadership is already held elsewhere must not
/// sweep. Without this, the election could be perfect and the supervisor could
/// still ignore it.
#[tokio::test]
async fn a_supervisor_follower_runs_no_retention_sweep() {
    let Some(database) = TestDatabase::create().await else {
        return;
    };

    // Held by "another replica" for the whole test.
    let incumbent = LeaderLock::try_acquire(&database.pool, workers::RETENTION_CLEANUP_WORKER)
        .await
        .expect("acquire runs")
        .expect("an uncontended lock must be granted");

    let mut settings = Settings {
        cluster: ClusterSettings {
            // Election mirrors admission, so this is what turns it on.
            admission_enabled: true,
            ..ClusterSettings::default()
        },
        ..Settings::default()
    };
    settings.workers.enabled = true;
    settings.workers.retention_interval_seconds = 1;

    let state = AppState::new(settings, Some(database.pool.clone())).expect("supervisor state");
    assert!(
        state.workers.leader_election_enabled(),
        "leader election must be on, or this test proves nothing"
    );
    assert!(
        state
            .workers
            .is_configured(workers::RETENTION_CLEANUP_WORKER),
        "the retention worker must be configured, or this test proves nothing"
    );

    let expired = insert_expired_idempotency_record(&database.pool).await;
    let supervisor = state
        .workers
        .spawn_supervisor(state.clone())
        .expect("workers are enabled, so a supervisor must be spawned");

    let survived = row_still_present_for(&database.pool, expired, NON_OCCURRENCE_BUDGET).await;
    // Before the assertion, so a failure cannot strand a sweeper.
    supervisor.shutdown().await;
    incumbent.release().await;

    assert!(
        survived,
        "a follower must not run the retention sweep; the expired row was deleted, so the \
         leader gate is not wired into the supervisor"
    );
}

/// The same supervisor, with leadership available: it must actually sweep. The
/// test above alone would also pass if the sweep were simply broken.
#[tokio::test]
async fn a_supervisor_leader_runs_the_retention_sweep() {
    let Some(database) = TestDatabase::create().await else {
        return;
    };
    let mut settings = Settings {
        cluster: ClusterSettings {
            admission_enabled: true,
            ..ClusterSettings::default()
        },
        ..Settings::default()
    };
    settings.workers.enabled = true;
    settings.workers.retention_interval_seconds = 1;

    let state = AppState::new(settings, Some(database.pool.clone())).expect("supervisor state");
    let expired = insert_expired_idempotency_record(&database.pool).await;
    let supervisor = state
        .workers
        .spawn_supervisor(state.clone())
        .expect("workers are enabled, so a supervisor must be spawned");

    let swept = poll_until(CONTENTION_BUDGET, || async {
        !row_exists(&database.pool, expired).await
    })
    .await;
    supervisor.shutdown().await;

    assert!(
        swept,
        "an uncontended supervisor must win leadership and sweep within {CONTENTION_BUDGET:?}"
    );
}

#[tokio::test]
async fn an_undeclared_singleton_job_is_an_error_not_a_silent_ungated_run() {
    let Some(database) = TestDatabase::create().await else {
        return;
    };
    assert_eq!(leader_lock_key("provider-health-check"), None);
    let error = LeaderLock::try_acquire(&database.pool, "provider-health-check")
        .await
        .expect_err("an undeclared job must not quietly acquire nothing and report success");
    assert!(
        error.to_string().contains("provider-health-check"),
        "the error must name the job so the fix is obvious; got {error}"
    );
}

async fn insert_expired_idempotency_record(pool: &PgPool) -> uuid::Uuid {
    // Backdated far enough that `order by expires_at` reaches it in the first
    // batch whatever else the fixture database holds.
    let marker = uuid::Uuid::now_v7().simple().to_string();
    sqlx::query_scalar::<_, uuid::Uuid>(
        "insert into idempotency_records \
             (idempotency_key_hash, actor_fingerprint, operation, request_hash, expires_at) \
         values ($1, $2, $3, $4, now() - interval '100 years') \
         returning id",
    )
    .bind(format!("key-{marker}"))
    .bind(format!("actor-{marker}"))
    .bind(format!("op-{marker}"))
    .bind(format!("req-{marker}"))
    .fetch_one(pool)
    .await
    .expect("seed an expired idempotency record")
}

async fn row_exists(pool: &PgPool, id: uuid::Uuid) -> bool {
    sqlx::query_scalar::<_, bool>("select exists(select 1 from idempotency_records where id = $1)")
        .bind(id)
        .fetch_one(pool)
        .await
        .expect("probe the row")
}

/// True when the row is still there after the whole budget — a bounded
/// *non*-occurrence check, which is the only honest way to assert "this did not
/// happen".
async fn row_still_present_for(pool: &PgPool, id: uuid::Uuid, budget: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + budget;
    while tokio::time::Instant::now() < deadline {
        if !row_exists(pool, id).await {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    row_exists(pool, id).await
}

async fn poll_until<F, Fut>(budget: Duration, mut condition: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + budget;
    loop {
        if condition().await {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}
