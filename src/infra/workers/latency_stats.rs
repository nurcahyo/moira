//! `latency-stats-aggregation` (plan 12 §2, consolidated decision 7; issue #211).
//!
//! Computes a naive last-N p50/p95 over real `execution_attempts.latency_ms` durations for
//! every `(provider_id, provider_model_id)` pair with recent traffic, and writes the result
//! into `provider_model_latency_stats` (migration `0032_provider_observability.sql`). Decision
//! 7 explicitly chose this over a declared-only signal: latency is *measured*, not
//! operator-entered.
//!
//! Nothing on the request path reads this table yet — see the migration's header comment.
//! Wiring `DefaultModelRouter` to consume it is gated on `routing_policies.scoring_enabled`
//! and touches `src/application/execution.rs`, which is explicitly out of scope for the change
//! that adds this handler (another workstream owns that file). This handler ships the writer
//! and stops there.

use std::sync::Arc;

use async_trait::async_trait;
use sqlx::PgPool;
use tracing::{info, warn};

use crate::{
    config::WorkerSettings,
    infra::{
        repositories::{
            ClaimedJob, PgProviderObservabilityRepository, ProviderObservabilityRepository,
        },
        workers::dispatch::JobHandler,
    },
};

/// The naive last-N percentile computation (consolidated decision 7), extracted as a pure
/// function so its arithmetic is unit-testable with no database.
///
/// Returns `(p50_latency_ms, p95_latency_ms, sample_count)`. `sample_count` is
/// `latencies_ms.len()`, always returned even when the percentiles are `None` (an empty
/// input), because the caller stores it either way — a pair that briefly has zero recent
/// successes should read as "0 samples", not disappear from the table.
///
/// Uses the nearest-rank method: for percentile `p` over `n` sorted values, rank =
/// `ceil(p * n / 100)` (minimum 1), 1-indexed. This is the same method most percentile
/// calculators use for a finite sample and needs no interpolation, which matters here because
/// a latency in whole milliseconds has no meaningful "value between two samples".
pub fn compute_latency_stats(mut latencies_ms: Vec<i64>) -> (Option<i32>, Option<i32>, i64) {
    let sample_count = i64::try_from(latencies_ms.len()).unwrap_or(i64::MAX);
    if latencies_ms.is_empty() {
        return (None, None, 0);
    }
    latencies_ms.sort_unstable();
    let p50 = nearest_rank_percentile(&latencies_ms, 50);
    let p95 = nearest_rank_percentile(&latencies_ms, 95);
    // `i32::try_from` rather than `as`: a latency this large (over 24 days in milliseconds)
    // is not a plausible provider round trip, and silently wrapping it into a small or
    // negative stored value would be a worse failure mode than the stat going missing for
    // one aggregation run.
    (
        i32::try_from(p50).ok(),
        i32::try_from(p95).ok(),
        sample_count,
    )
}

/// `sorted` must already be sorted ascending and non-empty.
fn nearest_rank_percentile(sorted: &[i64], percentile: u32) -> i64 {
    let n = sorted.len();
    let rank = (percentile as usize * n).div_ceil(100).max(1);
    let index = rank.saturating_sub(1).min(n - 1);
    sorted[index]
}

pub struct LatencyStatsAggregationHandler {
    pool: PgPool,
    settings: Arc<WorkerSettings>,
}

impl LatencyStatsAggregationHandler {
    pub fn new(pool: PgPool, settings: Arc<WorkerSettings>) -> Self {
        Self { pool, settings }
    }
}

#[async_trait]
impl JobHandler for LatencyStatsAggregationHandler {
    /// Never fails for a single pair's problem — a failed upsert for one
    /// `(provider_id, provider_model_id)` pair is logged and skipped, so one bad row cannot
    /// dead-letter the whole aggregation run. Only a failure to even list the candidate pairs
    /// (a real database problem) propagates, which lets the queue's existing retry/backoff
    /// apply to *that*.
    async fn handle(&self, job: &ClaimedJob) -> Result<(), String> {
        let repo = PgProviderObservabilityRepository::new(self.pool.clone());
        let lookback_hours =
            i32::try_from(self.settings.latency_stats_lookback_hours.max(1)).unwrap_or(i32::MAX);
        let sample_size = self.settings.latency_stats_sample_size.max(1);

        let pairs = repo
            .recent_candidate_pairs(lookback_hours)
            .await
            .map_err(|error| error.to_string())?;

        let mut aggregated = 0usize;
        let mut skipped = 0usize;
        for (provider_id, provider_model_id) in pairs {
            let latencies = match repo
                .recent_success_latencies_ms(provider_id, provider_model_id, sample_size)
                .await
            {
                Ok(values) => values,
                Err(error) => {
                    warn!(
                        job_id = %job.id,
                        %provider_id,
                        %provider_model_id,
                        %error,
                        "latency-stats-aggregation could not read recent durations for this pair; skipping"
                    );
                    skipped += 1;
                    continue;
                }
            };
            let (p50, p95, sample_count) = compute_latency_stats(latencies);
            match repo
                .upsert_latency_stats(provider_id, provider_model_id, p50, p95, sample_count)
                .await
            {
                Ok(()) => aggregated += 1,
                Err(error) => {
                    warn!(
                        job_id = %job.id,
                        %provider_id,
                        %provider_model_id,
                        %error,
                        "latency-stats-aggregation could not write stats for this pair; skipping"
                    );
                    skipped += 1;
                }
            }
        }

        info!(
            job_id = %job.id,
            aggregated,
            skipped,
            "latency-stats-aggregation run complete"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_sample_reports_zero_with_no_percentiles() {
        assert_eq!(compute_latency_stats(Vec::new()), (None, None, 0));
    }

    #[test]
    fn a_single_sample_is_its_own_p50_and_p95() {
        assert_eq!(compute_latency_stats(vec![120]), (Some(120), Some(120), 1));
    }

    /// Ten evenly spaced values, nearest-rank method: p50 rank = ceil(0.50*10) = 5th value
    /// (1-indexed) = index 4; p95 rank = ceil(0.95*10) = 10th value = index 9 (the max).
    #[test]
    fn percentiles_use_the_nearest_rank_method() {
        let latencies: Vec<i64> = (1..=10).map(|n| n * 100).collect(); // 100..=1000
        let (p50, p95, count) = compute_latency_stats(latencies);
        assert_eq!(p50, Some(500));
        assert_eq!(p95, Some(1000));
        assert_eq!(count, 10);
    }

    /// Input order must not matter — the function sorts internally.
    #[test]
    fn percentiles_are_order_independent() {
        let ascending = compute_latency_stats(vec![10, 20, 30, 40, 50]);
        let shuffled = compute_latency_stats(vec![40, 10, 50, 20, 30]);
        assert_eq!(ascending, shuffled);
    }

    /// A pair with very little history still gets a stat, computed from whatever samples
    /// exist — decision 7 is "naive last-N", not "last-N or nothing".
    #[test]
    fn a_small_sample_still_produces_a_stat() {
        let (p50, p95, count) = compute_latency_stats(vec![200, 400]);
        assert_eq!(count, 2);
        assert!(p50.is_some());
        assert!(p95.is_some());
    }

    /// The p95 of a sample never sits below its p50 — a sanity bound any correct
    /// nearest-rank implementation must satisfy for a non-decreasing sorted sample.
    #[test]
    fn p95_is_never_below_p50() {
        let (p50, p95, _) = compute_latency_stats(vec![50, 900, 100, 800, 150, 700, 200]);
        assert!(p95.unwrap() >= p50.unwrap());
    }
}
