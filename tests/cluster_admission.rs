//! Cluster admission leases against a real PostgreSQL (plan 10 wave 1, P3-2).
//!
//! Every test here runs on its **own** database, cloned from the migrated
//! template by [`support::TestDatabase`]. That is not tidiness: the admission
//! decision is a `count(*)` over the whole `cluster_replica_leases` table, so two
//! suites sharing a database would count each other's replicas and every ceiling
//! assertion would depend on what else happened to be running. The advisory lock
//! that serialises admission is database-scoped too, so a private database also
//! keeps it away from `b"MOIRARET"` in `tests/retention_worker.rs`.

mod support;

use std::{sync::Arc, time::Duration};

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use moira::{
    app::{AppState, CLUSTER_LEASE_DENIED_CODE, ClusterLeaseStatus, cluster_lease},
    build_router,
    config::{ClusterSettings, Settings},
    infra::repositories::{
        ClusterLeaseOutcome, ClusterLeaseRepository, PgClusterLeaseRepository, resolve_pod_name,
    },
};
use serde_json::Value;
use sqlx::PgPool;
use tokio::{sync::Barrier, task::JoinSet};
use uuid::Uuid;

use support::TestDatabase;

/// Enough for a contended `CREATE DATABASE … TEMPLATE` plus the barrier-released
/// acquisitions behind it. Spent only by a real failure.
const CONTENTION_BUDGET: Duration = Duration::from_secs(30);

fn cluster(max_replicas: u32) -> ClusterSettings {
    ClusterSettings {
        admission_enabled: true,
        max_replicas,
        lease_heartbeat_seconds: 1,
        lease_expiry_seconds: 2,
    }
}

fn repository(pool: &PgPool) -> PgClusterLeaseRepository {
    PgClusterLeaseRepository::new(pool.clone())
}

#[tokio::test]
async fn a_lease_is_granted_up_to_the_ceiling_and_denied_beyond_it() {
    let Some(database) = TestDatabase::create().await else {
        return;
    };
    let repository = repository(&database.pool);

    let first = repository
        .acquire("pod-a", 2, 30)
        .await
        .expect("the first acquire runs");
    let second = repository
        .acquire("pod-b", 2, 30)
        .await
        .expect("the second acquire runs");
    let third = repository
        .acquire("pod-c", 2, 30)
        .await
        .expect("the third acquire runs");

    assert!(
        matches!(first, ClusterLeaseOutcome::Granted(_)),
        "the first of two replicas must be admitted, got {first:?}"
    );
    assert!(
        matches!(second, ClusterLeaseOutcome::Granted(_)),
        "the second of two replicas must be admitted, got {second:?}"
    );
    assert!(
        matches!(
            third,
            ClusterLeaseOutcome::Denied {
                live_leases: 2,
                max_replicas: 2
            }
        ),
        "a third replica against max_replicas=2 must be denied, got {third:?}"
    );
}

/// The race the advisory lock exists for.
///
/// Without `pg_advisory_xact_lock` serialising admission, both contenders read
/// `count = 0`, both conclude there is room, and both insert — a ceiling of one
/// admitting two replicas. Barrier-released, never `sleep`-staggered, so the two
/// transactions genuinely overlap.
#[tokio::test]
async fn exactly_one_of_two_concurrent_replicas_is_admitted_to_a_single_slot() {
    let Some(database) = TestDatabase::create().await else {
        return;
    };
    let barrier = Arc::new(Barrier::new(2));
    let mut contenders = JoinSet::new();
    for index in 0..2 {
        let repository = repository(&database.pool);
        let barrier = barrier.clone();
        contenders.spawn(async move {
            barrier.wait().await;
            repository
                .acquire(&format!("pod-{index}"), 1, 30)
                .await
                .expect("acquire runs")
        });
    }

    let mut granted = 0;
    let mut denied = 0;
    let outcomes = tokio::time::timeout(CONTENTION_BUDGET, contenders.join_all())
        .await
        .expect("both contenders finished within the budget");
    for outcome in &outcomes {
        match outcome {
            ClusterLeaseOutcome::Granted(_) => granted += 1,
            ClusterLeaseOutcome::Denied { .. } => denied += 1,
        }
    }

    assert_eq!(
        (granted, denied),
        (1, 1),
        "one slot must admit exactly one of two concurrent replicas; got {outcomes:?}"
    );
    assert_eq!(
        repository(&database.pool)
            .live_lease_count(30)
            .await
            .expect("count live leases"),
        1,
        "a second row in the table is the lost-update the admission lock prevents"
    );
}

#[tokio::test]
async fn a_stale_lease_is_reclaimed_and_its_slot_reused() {
    let Some(database) = TestDatabase::create().await else {
        return;
    };
    let repository = repository(&database.pool);

    let ClusterLeaseOutcome::Granted(first) = repository
        .acquire("pod-crashed", 1, 30)
        .await
        .expect("the first acquire runs")
    else {
        panic!("the first replica against an empty table must be admitted");
    };

    // The pod died: no release, and its heartbeat stops. Backdated rather than
    // waited out, so the test asserts the reclaim *rule* and never the clock.
    sqlx::query("update cluster_replica_leases set heartbeat_at = now() - interval '1 hour' where replica_id = $1")
        .bind(first.replica_id)
        .execute(&database.pool)
        .await
        .expect("age the crashed replica's heartbeat");

    let second = repository
        .acquire("pod-replacement", 1, 30)
        .await
        .expect("the replacement acquire runs");
    assert!(
        matches!(second, ClusterLeaseOutcome::Granted(_)),
        "a slot held only by an expired heartbeat must be reclaimable, got {second:?}"
    );

    assert!(
        !repository
            .renew(first.replica_id)
            .await
            .expect("renewing a reclaimed lease runs"),
        "the crashed replica must not be able to renew a lease another replica now holds"
    );
}

#[tokio::test]
async fn releasing_a_lease_frees_its_slot_immediately() {
    let Some(database) = TestDatabase::create().await else {
        return;
    };
    let repository = repository(&database.pool);

    let ClusterLeaseOutcome::Granted(grant) = repository
        .acquire("pod-a", 1, 30)
        .await
        .expect("acquire runs")
    else {
        panic!("the first replica must be admitted");
    };
    assert!(
        matches!(
            repository
                .acquire("pod-b", 1, 30)
                .await
                .expect("acquire runs"),
            ClusterLeaseOutcome::Denied { .. }
        ),
        "the slot must be occupied before the release proves anything"
    );

    repository
        .release(grant.replica_id)
        .await
        .expect("release runs");
    // Idempotent: graceful shutdown must not fail on a lease already reclaimed.
    repository
        .release(grant.replica_id)
        .await
        .expect("releasing twice is not an error");

    assert!(
        matches!(
            repository
                .acquire("pod-b", 1, 30)
                .await
                .expect("acquire runs"),
            ClusterLeaseOutcome::Granted(_)
        ),
        "a released slot must be reusable without waiting out the expiry"
    );
}

#[tokio::test]
async fn renewal_keeps_a_lease_out_of_the_reclaim_window() {
    let Some(database) = TestDatabase::create().await else {
        return;
    };
    let repository = repository(&database.pool);
    let ClusterLeaseOutcome::Granted(grant) = repository
        .acquire("pod-a", 1, 30)
        .await
        .expect("acquire runs")
    else {
        panic!("the first replica must be admitted");
    };

    // One second short of the expiry: reclaimable on the next acquire unless the
    // renewal below moves the heartbeat forward.
    sqlx::query("update cluster_replica_leases set heartbeat_at = now() - interval '29 seconds' where replica_id = $1")
        .bind(grant.replica_id)
        .execute(&database.pool)
        .await
        .expect("age the heartbeat");
    assert!(
        repository
            .renew(grant.replica_id)
            .await
            .expect("renew runs"),
        "a live lease must renew"
    );

    assert_eq!(
        repository
            .live_lease_count(30)
            .await
            .expect("count live leases"),
        1,
        "a renewed lease must still be inside the expiry window"
    );
    assert!(
        matches!(
            repository
                .acquire("pod-b", 1, 30)
                .await
                .expect("acquire runs"),
            ClusterLeaseOutcome::Denied { .. }
        ),
        "a renewed lease must not be reclaimable"
    );
}

/// The startup gate, through the function `src/main.rs` actually calls.
#[tokio::test]
async fn the_startup_gate_refuses_to_start_a_replica_beyond_the_ceiling() {
    let Some(database) = TestDatabase::create().await else {
        return;
    };
    let settings = cluster(1);

    let first_status = ClusterLeaseStatus::not_enforced();
    let first = cluster_lease::acquire(Some(&database.pool), &settings, &first_status)
        .await
        .expect("the first replica is admitted")
        .expect("a granted lease yields a handle");
    assert_eq!(first_status.label(), "held");

    let second_status = ClusterLeaseStatus::not_enforced();
    let denial = cluster_lease::acquire(Some(&database.pool), &settings, &second_status)
        .await
        .expect_err("the second replica must be refused, not admitted");
    assert!(
        denial
            .to_string()
            .contains("cluster admission lease denied"),
        "the startup failure must name the reason so an operator can tell it from any \
         other startup error; got {denial}"
    );

    first.release().await;

    // And the refusal is not permanent: once the first replica releases, the next
    // process starts. A gate that stayed closed would turn one bad rollout into a
    // cluster that never comes back.
    let third_status = ClusterLeaseStatus::not_enforced();
    let third = cluster_lease::acquire(Some(&database.pool), &settings, &third_status)
        .await
        .expect("a released slot admits the next replica")
        .expect("a granted lease yields a handle");
    third.release().await;
}

/// §0 O6: the chart ships `MOIRA_DATABASE__MIGRATE_ON_STARTUP: "false"` and runs
/// migrations from a separate Job, so a replica can start before the table
/// exists. That must be a warning and a start, not a crash loop that sends the
/// operator looking for a capacity problem they do not have.
#[tokio::test]
async fn a_missing_lease_table_warns_and_starts_instead_of_failing() {
    let Some(database) = TestDatabase::create().await else {
        return;
    };
    sqlx::query("drop table cluster_replica_leases")
        .execute(&database.pool)
        .await
        .expect("simulate a database the migration Job has not reached");

    let status = ClusterLeaseStatus::not_enforced();
    let handle = cluster_lease::acquire(Some(&database.pool), &cluster(1), &status)
        .await
        .expect("a missing table must not fail startup");
    assert!(handle.is_none(), "there is no lease to hold");
    assert!(
        !status.is_denied(),
        "an un-migrated database must not make the replica report not-ready"
    );
}

/// The only user-visible response this plan adds.
#[tokio::test]
async fn readyz_reports_cluster_lease_denied_when_the_lease_is_lost_mid_run() {
    let Some(database) = TestDatabase::create().await else {
        return;
    };
    let settings = cluster(1);

    let app_settings = Settings {
        cluster: settings.clone(),
        ..Settings::default()
    };
    let state = AppState::new(app_settings, Some(database.pool.clone()))
        .await
        .expect("app state");
    let router = build_router(state.clone()).expect("router");

    let handle = cluster_lease::acquire(Some(&database.pool), &settings, &state.cluster_lease)
        .await
        .expect("the replica is admitted")
        .expect("a granted lease yields a handle");

    let healthy = get(&router, "/health/ready").await;
    assert_eq!(
        healthy.0,
        StatusCode::OK,
        "a replica holding its lease must be ready; body {}",
        healthy.1
    );

    // The mid-run loss this test exists for: another replica reclaimed the row
    // while this process was serving. Forced directly rather than by waiting out
    // a heartbeat, so the assertion is on the state transition and not on time.
    sqlx::query("update cluster_replica_leases set released_at = now() where replica_id = $1")
        .bind(handle.replica_id())
        .execute(&database.pool)
        .await
        .expect("reclaim the lease out from under the running replica");

    // Bounded poll, per the house pattern in `tests/admin_idempotency.rs`: the
    // heartbeat is the code under test and there is no signal to subscribe to.
    let denied = poll_until(CONTENTION_BUDGET, || async {
        state.cluster_lease.is_denied()
    })
    .await;
    assert!(
        denied,
        "the heartbeat must notice a reclaimed lease within {CONTENTION_BUDGET:?}"
    );

    let (status, body) = get(&router, "/health/ready").await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "a replica outside the ceiling must stop reporting ready; body {body}"
    );
    let error = &body["error"];
    assert_eq!(error["code"], CLUSTER_LEASE_DENIED_CODE);
    assert_eq!(error["message_key"], "moira.error.cluster_lease_denied");
    assert!(
        !error["message"].as_str().unwrap_or_default().is_empty(),
        "the catalog must supply an English default message; got {body}"
    );

    handle.release().await;
}

/// A replica the downward API never labelled still writes a usable row.
#[tokio::test]
async fn a_pod_name_fallback_satisfies_the_not_null_check() {
    let Some(database) = TestDatabase::create().await else {
        return;
    };
    let name = resolve_pod_name(None, None, Uuid::now_v7());
    let outcome = repository(&database.pool)
        .acquire(&name, 1, 30)
        .await
        .expect("a fallback pod name must be accepted by the column check");
    assert!(matches!(outcome, ClusterLeaseOutcome::Granted(_)));

    let stored: String = sqlx::query_scalar("select pod_name from cluster_replica_leases limit 1")
        .fetch_one(&database.pool)
        .await
        .expect("read the stored pod name");
    assert_eq!(stored, name);
}

/// The `check (length(btrim(pod_name)) > 0)` that keeps the fallback chain
/// honest. If this ever stops failing, the chain is free to write blanks.
#[tokio::test]
async fn a_blank_pod_name_is_rejected_by_the_database() {
    let Some(database) = TestDatabase::create().await else {
        return;
    };
    let error = repository(&database.pool)
        .acquire("   ", 1, 30)
        .await
        .expect_err("a blank pod name must not reach the table");
    assert!(
        error.to_string().contains("database"),
        "the rejection must come from the database constraint; got {error}"
    );
}

async fn get(router: &axum::Router, path: &str) -> (StatusCode, Value) {
    use tower::ServiceExt;

    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .uri(path)
                .body(Body::empty())
                .expect("build request"),
        )
        .await
        .expect("router responds");
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 64 * 1024)
        .await
        .expect("read body");
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

/// A bounded poll, not a `sleep`.
///
/// The house pattern where the code under test offers no signal to subscribe to
/// (`tests/admin_idempotency.rs`, `tests/execution_lifecycle.rs`): it returns as
/// soon as the condition holds, and its budget is only ever spent by a failure.
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
