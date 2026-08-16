//! `provider-health-check` (issue #83).
//!
//! Declared with `enabled_by_default: true` since before issue #90 existed, with no handler
//! behind it — the exact "spec lies to the caller" pattern #90's own doc comment calls out.
//! This module is the real handler: a cheap reachability probe per enabled provider, recorded
//! into the `provider_health_snapshots` rolling window (migration
//! `0032_provider_observability.sql`'s partial index; `provider_health_snapshots` itself is
//! migration `0005_provider_runtime.sql`) and surfaced at `GET
//! /api/v1/admin/providers/health`.
//!
//! # Fail soft, at both scopes that matter
//!
//! One provider timing out must not stop the rest of the run from probing — that failure
//! mode would make the very providers worth watching (the down ones) the ones most likely to
//! starve every other provider's probe out of the same run. And a failure to *record* one
//! probe's result must not fail the whole job — an operator learns less from one missing data
//! point than from a dead-lettered worker with no health data for the run at all. Only a
//! failure to even list which providers to probe (`ProviderObservabilityRepository::enabled_providers`
//! erroring) propagates, letting the queue's existing retry/backoff apply to that.

use std::{collections::HashMap, sync::Arc, time::Duration};

use async_trait::async_trait;
use reqwest::Client;
use serde_json::json;
use sqlx::PgPool;
use tokio::time::Instant;
use tracing::{info, warn};

use crate::{
    config::WorkerSettings,
    domain::ProviderType,
    infra::{
        metrics::MetricsRegistry,
        pg_rows::provider_type_from_db,
        repositories::{
            ClaimedJob, HealthSnapshotInsert, PgProviderObservabilityRepository,
            ProviderObservabilityRepository, ProviderProbeTarget,
        },
        workers::dispatch::JobHandler,
    },
};

/// Above this round-trip latency a *reachable* provider is classified `degraded` rather than
/// `healthy`. Not a `WorkerSettings` knob: it is a health-classification policy, not a
/// deployment-topology one, and the three-value classification domain is small enough that a
/// fixed threshold is easier to reason about than a fourth setting nobody has asked to tune
/// yet. Revisit if that changes.
const DEGRADED_LATENCY_THRESHOLD_MS: u64 = 2_000;

/// Classifies one probe outcome into `provider_health_snapshots.status`'s domain.
///
/// A pure function so the classification boundary — the exact millisecond at which
/// "reachable" becomes "degraded" — is unit-testable with no network and no clock. Never
/// returns `"unknown"`: that value means "no recent snapshot at all", a fact the read side
/// (`ProviderObservabilityRepository::provider_health_summaries`) derives from an *absence* of
/// rows, not a value a completed probe ever reports about itself.
pub fn classify_probe(
    reachable: bool,
    latency_ms: Option<u64>,
    degraded_threshold_ms: u64,
) -> &'static str {
    if !reachable {
        return "unhealthy";
    }
    match latency_ms {
        Some(ms) if ms > degraded_threshold_ms => "degraded",
        _ => "healthy",
    }
}

struct ProbeOutcome {
    reachable: bool,
    latency_ms: Option<u64>,
}

pub struct ProviderHealthCheckHandler {
    pool: PgPool,
    http: Client,
    metrics: MetricsRegistry,
    settings: Arc<WorkerSettings>,
}

impl ProviderHealthCheckHandler {
    pub fn new(
        pool: PgPool,
        http: Client,
        metrics: MetricsRegistry,
        settings: Arc<WorkerSettings>,
    ) -> Self {
        Self {
            pool,
            http,
            metrics,
            settings,
        }
    }

    /// One bare HTTP reachability probe against `target.base_url`.
    ///
    /// Any HTTP response — including a 4xx or 5xx — counts as reachable: this is a
    /// connectivity probe, not a credential-eligibility or capability check (issue #83's
    /// "credential availability & health eligibility" is explicitly a separate, later item).
    /// Only a transport-level failure (connection refused, DNS failure, TLS failure, or the
    /// timeout below) counts as unreachable, which is exactly what `reqwest::Client::send`
    /// surfaces as `Err` rather than a non-2xx `Ok(Response)`.
    async fn probe(&self, target: &ProviderProbeTarget) -> ProbeOutcome {
        let timeout = Duration::from_millis(self.settings.provider_health_probe_timeout_ms.max(1));
        let start = Instant::now();
        let result = self
            .http
            .get(&target.base_url)
            .timeout(timeout)
            .send()
            .await;
        let elapsed_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
        match result {
            Ok(_response) => ProbeOutcome {
                reachable: true,
                latency_ms: Some(elapsed_ms),
            },
            Err(_error) => ProbeOutcome {
                reachable: false,
                latency_ms: None,
            },
        }
    }
}

#[async_trait]
impl JobHandler for ProviderHealthCheckHandler {
    async fn handle(&self, job: &ClaimedJob) -> Result<(), String> {
        let repo = PgProviderObservabilityRepository::new(self.pool.clone());
        let targets = repo
            .enabled_providers()
            .await
            .map_err(|error| error.to_string())?;

        let mut probed = 0usize;
        let mut skipped = 0usize;
        // Aggregated per provider type for `set_provider_health_status`, mirroring
        // `moira_oauth_credential_status`'s "distribution across a bounded label", never a
        // per-provider-id gauge (an unbounded label in a scrape path).
        let mut counts: HashMap<ProviderType, [usize; 3]> = HashMap::new();

        for target in targets {
            let provider_type = match provider_type_from_db(target.provider_type.clone()) {
                Ok(provider_type) => provider_type,
                Err(error) => {
                    warn!(
                        job_id = %job.id,
                        provider_id = %target.id,
                        %error,
                        "provider-health-check could not classify this provider's type; skipping"
                    );
                    skipped += 1;
                    continue;
                }
            };

            let outcome = self.probe(&target).await;
            let status = classify_probe(
                outcome.reachable,
                outcome.latency_ms,
                DEGRADED_LATENCY_THRESHOLD_MS,
            );

            let insert = HealthSnapshotInsert {
                provider_id: target.id,
                status,
                circuit_state: "closed",
                latency_ms: outcome.latency_ms.and_then(|ms| i32::try_from(ms).ok()),
                failure_count: i32::from(!outcome.reachable),
                metadata: json!({}),
            };
            if let Err(error) = repo.record_health_snapshot(insert).await {
                warn!(
                    job_id = %job.id,
                    provider_id = %target.id,
                    %error,
                    "provider-health-check could not record this probe's snapshot; skipping"
                );
                skipped += 1;
                continue;
            }

            probed += 1;
            self.metrics
                .record_provider_health_probe(provider_type, status);
            let bucket = counts.entry(provider_type).or_insert([0, 0, 0]);
            match status {
                "healthy" => bucket[0] += 1,
                "degraded" => bucket[1] += 1,
                _ => bucket[2] += 1,
            }
        }

        for (provider_type, [healthy, degraded, unhealthy]) in counts {
            self.metrics
                .set_provider_health_status(provider_type, healthy, degraded, unhealthy);
        }

        let retention_hours = i32::try_from(
            self.settings
                .provider_health_snapshot_retention_hours
                .max(1),
        )
        .unwrap_or(i32::MAX);
        if let Err(error) = repo.prune_health_snapshots(retention_hours).await {
            // A failed prune costs disk, not correctness — the next run tries again.
            warn!(job_id = %job.id, %error, "provider-health-check snapshot prune failed; retrying next run");
        }

        info!(
            job_id = %job.id,
            probed,
            skipped,
            "provider-health-check run complete"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unreachable_probe_is_always_unhealthy_regardless_of_latency() {
        assert_eq!(classify_probe(false, None, 2_000), "unhealthy");
        assert_eq!(classify_probe(false, Some(5), 2_000), "unhealthy");
    }

    #[test]
    fn a_fast_reachable_probe_is_healthy() {
        assert_eq!(classify_probe(true, Some(50), 2_000), "healthy");
    }

    #[test]
    fn a_reachable_probe_with_no_measured_latency_is_healthy() {
        // Should not happen from `ProviderHealthCheckHandler::probe` (a reachable probe always
        // carries `Some` latency), but the pure function's contract should not silently
        // misclassify a `None` as unhealthy either.
        assert_eq!(classify_probe(true, None, 2_000), "healthy");
    }

    #[test]
    fn a_slow_reachable_probe_is_degraded() {
        assert_eq!(classify_probe(true, Some(2_001), 2_000), "degraded");
    }

    /// The boundary is exclusive: exactly at the threshold is still healthy, one millisecond
    /// over is degraded.
    #[test]
    fn the_degraded_threshold_boundary_is_exclusive() {
        assert_eq!(classify_probe(true, Some(2_000), 2_000), "healthy");
        assert_eq!(classify_probe(true, Some(2_001), 2_000), "degraded");
    }
}
