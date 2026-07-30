//! The Redis implementation of [`ClusterCoordinator`].
//!
//! Two things live here and nowhere else: the mapping from the control layer's
//! vocabulary onto Redis commands, and the decision to count a failure before
//! handing it back.
//!
//! # Why this is not `impl ClusterCoordinator for RedisClient`
//!
//! Because a failed coordination call is a metric, and `MetricsRegistry` is
//! constructed alongside — not before — the Redis client. Pairing them in a
//! wrapper keeps `RedisClient` a pure transport and keeps the "count the failure"
//! rule in one place rather than at five call sites that each have to remember it.
//!
//! # This type is only ever constructed when Redis is enabled
//!
//! Which is not the default. `AppState::new` builds it from
//! `Option<RedisClient>`, so a default deployment never has one and every control
//! that would consult it takes its in-process arm instead.

use std::time::Duration;

use async_trait::async_trait;

use crate::{
    error::AppError,
    infra::{
        metrics::{MetricsRegistry, RedisOperation},
        redis::RedisClient,
    },
    orchestration::ClusterCoordinator,
};

#[derive(Clone)]
pub struct RedisCoordinator {
    redis: RedisClient,
    metrics: MetricsRegistry,
}

/// The Redis client's own `Debug` already withholds the connection URL; this
/// forwards to it rather than deriving, so a `MetricsRegistry` (which prints a
/// recorder) does not end up in a panic message either.
impl std::fmt::Debug for RedisCoordinator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedisCoordinator")
            .field("redis", &self.redis)
            .finish_non_exhaustive()
    }
}

impl RedisCoordinator {
    pub fn new(redis: RedisClient, metrics: MetricsRegistry) -> Self {
        Self { redis, metrics }
    }

    /// Counts the failure and returns it unchanged.
    ///
    /// The control layer turns every one of these into a refusal, so without a
    /// counter a Redis outage looks identical to genuine saturation: a wall of
    /// `429`s and no way to tell which. This metric is the difference.
    fn count<T>(
        &self,
        operation: RedisOperation,
        outcome: Result<T, AppError>,
    ) -> Result<T, AppError> {
        if outcome.is_err() {
            self.metrics.record_redis_operation_failure(operation);
        }
        outcome
    }
}

#[async_trait]
impl ClusterCoordinator for RedisCoordinator {
    async fn check_rate_window(
        &self,
        key: &str,
        limit: u32,
        window: Duration,
    ) -> Result<bool, AppError> {
        let outcome = self.redis.check_rate_window(key, limit, window).await;
        self.count(RedisOperation::RateLimit, outcome)
    }

    async fn try_acquire_permit(
        &self,
        key: &str,
        limit: usize,
        ttl: Duration,
    ) -> Result<bool, AppError> {
        let outcome = self.redis.try_acquire_permit(key, limit, ttl).await;
        self.count(RedisOperation::PermitAcquire, outcome)
    }

    async fn release_permit(&self, key: &str) -> Result<(), AppError> {
        let outcome = self.redis.release_permit(key).await;
        self.count(RedisOperation::PermitRelease, outcome)
    }

    fn key(&self, suffix: &str) -> String {
        self.redis.key(suffix)
    }
}
