//! The Redis-backed arm of rate limiting and concurrency, against a real Redis 7
//! (plan 10 wave 2, finding P3-1).
//!
//! Redis is **off by default** — plan 10 §0.4b — so nothing here runs on a
//! default build. Its companion is `tests/coordination_default_path.rs`, which
//! asserts the shipped configuration still coordinates correctly with Redis
//! absent; that file is the one that must never be allowed to fail.
//!
//! # Two "replicas" without two processes
//!
//! Every test below builds **two independent `ConcurrencyController`s or
//! `ClusterRateLimiter`s** over one `RedisCoordinator`. That is precisely the
//! topology of two pods: separate in-process state, shared coordination. Two OS
//! processes would test the same property and nothing more, at the cost of a
//! fixture that cannot run under `cargo test`.

mod support;

use std::{sync::Arc, time::Duration};

use moira::{
    infra::{coordination::RedisCoordinator, metrics::MetricsRegistry, redis::RedisClient},
    orchestration::{ClusterCoordinator, ClusterRateLimiter, ConcurrencyController},
};
use tokio::sync::Barrier;
use uuid::Uuid;

use support::test_redis;

fn coordinator(redis: RedisClient) -> Arc<dyn ClusterCoordinator> {
    Arc::new(RedisCoordinator::new(
        redis,
        MetricsRegistry::new("moira-test", None),
    ))
}

const TTL: Duration = Duration::from_secs(60);

/// **The core P3-1 proof for rate limiting.** Two replicas, one policy limit,
/// enforced as N *total* rather than N *each*.
///
/// Before this backend existed, the same scenario admitted 2N — the "worked
/// example" in plan 10's Architecture section, where 3 pods let a client push 3x
/// its configured rate.
#[tokio::test]
async fn a_rate_limit_is_enforced_across_two_replicas() {
    let Some(redis) = test_redis() else {
        return;
    };
    let coordinator = coordinator(redis);
    let replica_a = ClusterRateLimiter::new(coordinator.clone());
    let replica_b = ClusterRateLimiter::new(coordinator.clone());
    let key = format!("application:{}", Uuid::now_v7());
    let window = Duration::from_secs(60);

    // Limit 3, split across two replicas: two admissions on A, one on B.
    assert!(replica_a.check(key.clone(), 3, window).await.is_ok());
    assert!(replica_a.check(key.clone(), 3, window).await.is_ok());
    assert!(replica_b.check(key.clone(), 3, window).await.is_ok());

    // The fourth is refused by **either** replica: the window is shared.
    assert!(
        replica_b.check(key.clone(), 3, window).await.is_err(),
        "replica B admitted a fourth request against a shared limit of 3"
    );
    assert!(replica_a.check(key.clone(), 3, window).await.is_err());
}

/// The refusal must be the same `429 rate_limited` the in-process limiter
/// produces, so the wire contract does not change with the deployment shape.
#[tokio::test]
async fn the_cluster_rate_limit_refusal_carries_the_unchanged_wire_contract() {
    let Some(redis) = test_redis() else {
        return;
    };
    let limiter = ClusterRateLimiter::new(coordinator(redis));
    let key = format!("application:{}", Uuid::now_v7());
    let window = Duration::from_secs(60);

    assert!(limiter.check(key.clone(), 1, window).await.is_ok());
    let error = limiter
        .check(key, 1, window)
        .await
        .expect_err("the second request exceeds the limit of 1");
    let moira::error::AppError::Api {
        status,
        code,
        message,
        ..
    } = &error
    else {
        panic!("expected a coded API error, got {error:?}");
    };
    assert_eq!(*status, axum::http::StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(*code, "rate_limited");
    assert!(!message.is_empty());
    assert!(moira::i18n::is_known_key("moira.error.rate_limited"));
}

/// Concurrent checks through a `Barrier`: the shared counter must admit exactly
/// `limit` and no more, whatever the interleaving.
#[tokio::test]
async fn concurrent_rate_limit_checks_admit_exactly_the_limit() {
    let Some(redis) = test_redis() else {
        return;
    };
    let coordinator = coordinator(redis);
    let key = format!("application:{}", Uuid::now_v7());
    let window = Duration::from_secs(60);
    let limit = 4u32;
    let racers = 12;

    let barrier = Arc::new(Barrier::new(racers));
    let mut handles = Vec::new();
    for _ in 0..racers {
        // A separate limiter per task: each stands for its own replica.
        let limiter = ClusterRateLimiter::new(coordinator.clone());
        let barrier = barrier.clone();
        let key = key.clone();
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            limiter.check(key, limit, window).await.is_ok()
        }));
    }

    let mut admitted = 0;
    for handle in handles {
        if handle.await.expect("racer task") {
            admitted += 1;
        }
    }
    assert_eq!(
        admitted, limit as usize,
        "{racers} concurrent checks admitted {admitted} against a limit of {limit}"
    );
}

/// **The core P3-1 proof for concurrency permits.**
#[tokio::test]
async fn concurrency_permits_are_enforced_across_two_replicas() {
    let Some(redis) = test_redis() else {
        return;
    };
    let coordinator = coordinator(redis);
    // Each replica's local ceiling is 2, the cluster ceiling is 2, so the pair
    // must admit 2 in total.
    let replica_a = ConcurrencyController::new(2, 2, 2, 64).with_cluster(coordinator.clone(), TTL);
    let replica_b = ConcurrencyController::new(2, 2, 2, 64).with_cluster(coordinator.clone(), TTL);
    assert!(replica_a.is_cluster_wide() && replica_b.is_cluster_wide());
    let provider = Uuid::now_v7();

    let _a = replica_a
        .acquire(provider, 2, false, 2, None, None)
        .await
        .expect("replica A's first permit");
    let _b = replica_b
        .acquire(provider, 2, false, 2, None, None)
        .await
        .expect("replica B's first permit");

    assert!(
        replica_b
            .acquire(provider, 2, false, 2, None, None)
            .await
            .is_err(),
        "replica B admitted a third execution against a cluster ceiling of 2; without \
         the shared counter it would still have had a local slot free"
    );
}

/// Releasing on one replica must free the slot for the other, or the cluster
/// ceiling ratchets down to zero over the life of the deployment.
#[tokio::test]
async fn a_permit_released_on_one_replica_frees_the_slot_on_another() {
    let Some(redis) = test_redis() else {
        return;
    };
    let coordinator = coordinator(redis);
    let replica_a = ConcurrencyController::new(1, 1, 1, 64).with_cluster(coordinator.clone(), TTL);
    let replica_b = ConcurrencyController::new(1, 1, 1, 64).with_cluster(coordinator.clone(), TTL);
    let provider = Uuid::now_v7();

    let held = replica_a
        .acquire(provider, 1, false, 1, None, None)
        .await
        .expect("replica A takes the only slot");
    assert!(
        replica_b
            .acquire(provider, 1, false, 1, None, None)
            .await
            .is_err()
    );

    drop(held);

    // The release is a detached task. Bounded polling rather than a sleep: a
    // genuinely stuck release fails the test instead of hanging it, and the
    // observation is of the resulting state rather than of elapsed time.
    let mut acquired = false;
    for _ in 0..200 {
        if let Ok(permit) = replica_b.acquire(provider, 1, false, 1, None, None).await {
            drop(permit);
            acquired = true;
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(
        acquired,
        "replica A's released slot never became visible to replica B"
    );
}

/// **Fail closed.** A Redis that cannot be reached must refuse, never admit.
///
/// The URL points at a closed port rather than at a stopped container, so the
/// test needs no orchestration and asserts the same code path: a coordinator call
/// that returns `Err`.
#[tokio::test]
async fn an_unreachable_redis_refuses_rather_than_admitting_unlimited_traffic() {
    let settings = moira::config::RedisSettings {
        enabled: true,
        // Reserved-discard port: nothing listens, and the connect fails fast.
        url: Some("redis://127.0.0.1:9/0".to_string()),
        connect_timeout_seconds: 1,
        ..moira::config::RedisSettings::default()
    };
    let redis = RedisClient::from_settings(&settings)
        .expect("build the client")
        .expect("Redis is enabled");
    let coordinator = coordinator(redis);

    let limiter = ClusterRateLimiter::new(coordinator.clone());
    assert!(
        limiter
            .check("k".to_string(), 1_000, Duration::from_secs(60))
            .await
            .is_err(),
        "an unreachable Redis admitted traffic against a limit it could not read"
    );

    let controller = ConcurrencyController::new(64, 64, 64, 64).with_cluster(coordinator, TTL);
    assert!(
        controller
            .acquire(Uuid::now_v7(), 64, false, 64, None, None)
            .await
            .is_err(),
        "an unreachable Redis admitted unbounded concurrency"
    );
}

/// Every Redis call on the request path is timeout-bounded.
///
/// The check is that a connect to an unroutable address returns rather than
/// hanging — an unbounded await here would stall a request thread for as long as
/// the network takes to give up, which on some kernels is minutes.
#[tokio::test]
async fn a_redis_call_to_an_unreachable_host_is_bounded_by_its_timeout() {
    let settings = moira::config::RedisSettings {
        enabled: true,
        // TEST-NET-1 (RFC 5737): guaranteed not to be routed anywhere.
        url: Some("redis://192.0.2.1:6379/0".to_string()),
        connect_timeout_seconds: 1,
        ..moira::config::RedisSettings::default()
    };
    let redis = RedisClient::from_settings(&settings)
        .expect("build the client")
        .expect("Redis is enabled");

    // Generous relative to the 1s timeout, tight relative to a TCP connect that
    // is not bounded at all.
    let outcome = tokio::time::timeout(
        Duration::from_secs(10),
        redis.check_rate_window("k", 1, Duration::from_secs(60)),
    )
    .await;
    assert!(
        outcome.is_ok(),
        "the Redis call was not bounded by connect_timeout_seconds"
    );
    assert!(outcome.expect("bounded").is_err());
}

/// No secret, credential or raw caller identifier may ever become a Redis key.
///
/// Redis is operationally visible — `KEYS`, `MONITOR`, an RDB dump, a managed
/// provider's console — in ways process memory is not, so a key is a value.
#[tokio::test]
async fn no_redis_key_carries_a_raw_caller_identifier() {
    let Some(redis) = test_redis() else {
        return;
    };
    let namespace = redis.namespace().to_string();
    let coordinator = coordinator(redis.clone());
    let controller = ConcurrencyController::new(4, 4, 4, 64).with_cluster(coordinator, TTL);
    let user = "person@example.com";

    let _permit = controller
        .acquire(Uuid::now_v7(), 4, false, 4, None, Some(user))
        .await
        .expect("permit");

    // Scoped to this test's namespace, so it never reads another suite's keys and
    // never needs FLUSHDB.
    let keys = redis
        .scan_namespace_for_tests()
        .await
        .expect("scan the test namespace");
    assert!(
        keys.iter().any(|key| key.contains(":permit:user:")),
        "the user scope was never counted: {keys:?}"
    );
    for key in &keys {
        assert!(
            key.starts_with(&namespace),
            "key escaped the namespace: {key}"
        );
        assert!(!key.contains(user), "raw identifier in a Redis key: {key}");
        assert!(
            !key.contains("example.com"),
            "raw identifier in a Redis key: {key}"
        );
    }
}
