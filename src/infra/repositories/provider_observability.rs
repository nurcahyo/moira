//! Persistence for `provider_model_latency_stats` and `provider_health_snapshots` (issues
//! #211/#83, plan 12 §2 "Later" phase).
//!
//! One trait for both tables rather than two, because both are written by a periodic
//! maintenance job under `src/infra/workers/` and read back by an admin-facing surface, and
//! nothing else in the tree touches either table — splitting them into two single-method
//! traits would buy no test isolation that matters here. `AdminRepository` and friends split
//! by *owning HTTP surface*, not by table; this repository has no HTTP surface of its own
//! until `GET /api/v1/admin/providers/health` reads the health half.
//!
//! # `provider_model_latency_stats`
//!
//! Written by `latency-stats-aggregation` (`src/infra/workers/latency_stats.rs`), a naive
//! last-N measured stat over real `execution_attempts.latency_ms` durations — consolidated
//! decision 7. Not read anywhere in this tree yet: the deferred scoring consumer gated by
//! `routing_policies.scoring_enabled` is a follow-up, not part of this change.
//!
//! # `provider_health_snapshots`
//!
//! The table already existed (migration `0005_provider_runtime.sql`) with no reader or
//! writer anywhere in the tree. `provider-health-check`
//! (`src/infra/workers/provider_health_check.rs`) is the first writer; every row it writes
//! carries `provider_model_id = null`, because it probes a provider's reachability, not a
//! specific model's. `provider_health_summaries` is the first reader, backing `GET
//! /api/v1/admin/providers/health`.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{PgPool, Row, postgres::PgRow};
use uuid::Uuid;

use crate::error::AppError;

/// A provider worth probing: has a `base_url` to reach and is not soft-deleted or disabled.
#[derive(Debug, Clone)]
pub struct ProviderProbeTarget {
    pub id: Uuid,
    /// The database `provider_type` string (`src/infra/pg_rows.rs::provider_type_to_db`'s
    /// domain), kept as the raw string rather than the `ProviderType` enum: the probe and the
    /// metrics label both want a `&str`, and round-tripping through the enum would buy nothing.
    pub provider_type: String,
    pub display_name: String,
    pub base_url: String,
}

/// One health probe's outcome, ready to insert.
#[derive(Debug, Clone)]
pub struct HealthSnapshotInsert {
    pub provider_id: Uuid,
    /// One of `provider_health_snapshots.status`'s four values (`healthy`, `degraded`,
    /// `unhealthy`, `unknown`). `provider-health-check` never writes `unknown` — see
    /// `classify_probe` in `src/infra/workers/provider_health_check.rs` — but the column
    /// allows it for a future writer this repository does not know about.
    pub status: &'static str,
    /// Always `"closed"` from this writer: `provider-health-check` is a reachability probe,
    /// not the per-attempt circuit breaker (`src/orchestration/controls.rs`), and has no
    /// circuit state of its own to report. Carried rather than hardcoded in the insert SQL so
    /// a future caller with a real circuit state does not have to touch this repository.
    pub circuit_state: &'static str,
    pub latency_ms: Option<i32>,
    pub failure_count: i32,
    pub metadata: Value,
}

/// One provider's rolling health window, as `GET /api/v1/admin/providers/health` serves it.
#[derive(Debug, Clone)]
pub struct ProviderHealthSummaryRow {
    pub provider_id: Uuid,
    pub provider_type: String,
    pub display_name: String,
    /// The most recent snapshot's status, or `"unknown"` when the provider has never been
    /// probed (no snapshot in the window at all — not the same as a snapshot that itself says
    /// `unknown`, which this writer never produces).
    pub current_status: String,
    pub probes_total: i64,
    pub probes_successful: i64,
    pub average_latency_ms: Option<f64>,
    pub last_probe_at: Option<DateTime<Utc>>,
    pub last_success_at: Option<DateTime<Utc>>,
    pub last_failure_at: Option<DateTime<Utc>>,
}

#[async_trait]
pub trait ProviderObservabilityRepository: Send + Sync {
    /// Distinct `(provider_id, provider_model_id)` pairs with at least one `execution_attempts`
    /// row started within `lookback_hours` — the candidate set `latency-stats-aggregation`
    /// aggregates over. Bounded by recency so an aggregation run costs is proportional to
    /// recent traffic, not to the table's full history.
    async fn recent_candidate_pairs(
        &self,
        lookback_hours: i32,
    ) -> Result<Vec<(Uuid, Uuid)>, AppError>;

    /// The most recent `sample_size` successful attempt durations for one candidate pair, most
    /// recent first — the naive "last N" decision 7 specifies.
    async fn recent_success_latencies_ms(
        &self,
        provider_id: Uuid,
        provider_model_id: Uuid,
        sample_size: i64,
    ) -> Result<Vec<i64>, AppError>;

    /// Upserts one candidate pair's computed stats. `sample_count` is the number of durations
    /// the p50/p95 were computed from, which may be less than `sample_size` for a pair with
    /// little history.
    async fn upsert_latency_stats(
        &self,
        provider_id: Uuid,
        provider_model_id: Uuid,
        p50_latency_ms: Option<i32>,
        p95_latency_ms: Option<i32>,
        sample_count: i64,
    ) -> Result<(), AppError>;

    /// Every active, non-deleted provider with a `base_url` to probe. A provider with no
    /// `base_url` (some native clients resolve their own default) is not returned: there is
    /// nothing for a bare HTTP reachability probe to reach.
    async fn enabled_providers(&self) -> Result<Vec<ProviderProbeTarget>, AppError>;

    async fn record_health_snapshot(&self, insert: HealthSnapshotInsert) -> Result<(), AppError>;

    /// Deletes provider-level (`provider_model_id is null`) snapshots older than
    /// `older_than_hours`. Returns how many.
    async fn prune_health_snapshots(&self, older_than_hours: i32) -> Result<u64, AppError>;

    /// One rolling summary per non-deleted provider, aggregated over snapshots observed after
    /// `since`. A provider with zero snapshots in the window still appears, with
    /// `current_status = "unknown"` and every count at zero — the honest answer for "we have
    /// not probed this provider recently", not an absence a client has to special-case.
    async fn provider_health_summaries(
        &self,
        since: DateTime<Utc>,
    ) -> Result<Vec<ProviderHealthSummaryRow>, AppError>;
}

#[derive(Clone)]
pub struct PgProviderObservabilityRepository {
    pool: PgPool,
}

impl PgProviderObservabilityRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl ProviderObservabilityRepository for PgProviderObservabilityRepository {
    async fn recent_candidate_pairs(
        &self,
        lookback_hours: i32,
    ) -> Result<Vec<(Uuid, Uuid)>, AppError> {
        let rows = sqlx::query(RECENT_CANDIDATE_PAIRS_SQL)
            .bind(lookback_hours)
            .fetch_all(&self.pool)
            .await?;
        rows.iter()
            .map(|row| {
                Ok((
                    row.try_get::<Uuid, _>("provider_id")?,
                    row.try_get::<Uuid, _>("provider_model_id")?,
                ))
            })
            .collect()
    }

    async fn recent_success_latencies_ms(
        &self,
        provider_id: Uuid,
        provider_model_id: Uuid,
        sample_size: i64,
    ) -> Result<Vec<i64>, AppError> {
        let rows = sqlx::query(RECENT_SUCCESS_LATENCIES_SQL)
            .bind(provider_id)
            .bind(provider_model_id)
            .bind(sample_size)
            .fetch_all(&self.pool)
            .await?;
        rows.iter()
            .map(|row| row.try_get::<i64, _>("latency_ms").map_err(AppError::from))
            .collect()
    }

    async fn upsert_latency_stats(
        &self,
        provider_id: Uuid,
        provider_model_id: Uuid,
        p50_latency_ms: Option<i32>,
        p95_latency_ms: Option<i32>,
        sample_count: i64,
    ) -> Result<(), AppError> {
        sqlx::query(UPSERT_LATENCY_STATS_SQL)
            .bind(provider_id)
            .bind(provider_model_id)
            .bind(p50_latency_ms)
            .bind(p95_latency_ms)
            .bind(sample_count)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn enabled_providers(&self) -> Result<Vec<ProviderProbeTarget>, AppError> {
        let rows = sqlx::query(ENABLED_PROVIDERS_SQL)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(provider_probe_target_from_row).collect()
    }

    async fn record_health_snapshot(&self, insert: HealthSnapshotInsert) -> Result<(), AppError> {
        sqlx::query(RECORD_HEALTH_SNAPSHOT_SQL)
            .bind(insert.provider_id)
            .bind(insert.status)
            .bind(insert.circuit_state)
            .bind(insert.failure_count)
            .bind(insert.latency_ms)
            .bind(insert.metadata)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn prune_health_snapshots(&self, older_than_hours: i32) -> Result<u64, AppError> {
        let result = sqlx::query(PRUNE_HEALTH_SNAPSHOTS_SQL)
            .bind(older_than_hours)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }

    async fn provider_health_summaries(
        &self,
        since: DateTime<Utc>,
    ) -> Result<Vec<ProviderHealthSummaryRow>, AppError> {
        let rows = sqlx::query(PROVIDER_HEALTH_SUMMARIES_SQL)
            .bind(since)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(health_summary_row_from_row).collect()
    }
}

fn provider_probe_target_from_row(row: &PgRow) -> Result<ProviderProbeTarget, AppError> {
    Ok(ProviderProbeTarget {
        id: row.try_get("id")?,
        provider_type: row.try_get("provider_type")?,
        display_name: row.try_get("display_name")?,
        base_url: row.try_get("base_url")?,
    })
}

fn health_summary_row_from_row(row: &PgRow) -> Result<ProviderHealthSummaryRow, AppError> {
    Ok(ProviderHealthSummaryRow {
        provider_id: row.try_get("provider_id")?,
        provider_type: row.try_get("provider_type")?,
        display_name: row.try_get("display_name")?,
        current_status: row.try_get("current_status")?,
        probes_total: row.try_get("probes_total")?,
        probes_successful: row.try_get("probes_successful")?,
        average_latency_ms: row.try_get("average_latency_ms")?,
        last_probe_at: row.try_get("last_probe_at")?,
        last_success_at: row.try_get("last_success_at")?,
        last_failure_at: row.try_get("last_failure_at")?,
    })
}

const RECENT_CANDIDATE_PAIRS_SQL: &str = r#"
    select distinct provider_id, provider_model_id
    from execution_attempts
    where started_at > now() - make_interval(hours => $1)
      and provider_id is not null
      and provider_model_id is not null
"#;

const RECENT_SUCCESS_LATENCIES_SQL: &str = r#"
    select latency_ms
    from execution_attempts
    where provider_id = $1
      and provider_model_id = $2
      and status = 'succeeded'
      and latency_ms is not null
    order by started_at desc
    limit $3
"#;

/// `on conflict` on the primary key means a pair with fresh traffic always reflects only its
/// latest computation — there is no history of a pair's stats over time, by design: this is a
/// rolling snapshot for routing, not an audit trail.
const UPSERT_LATENCY_STATS_SQL: &str = r#"
    insert into provider_model_latency_stats
        (provider_id, provider_model_id, p50_latency_ms, p95_latency_ms, sample_count, updated_at)
    values ($1, $2, $3, $4, $5, now())
    on conflict (provider_id, provider_model_id)
    do update set p50_latency_ms = excluded.p50_latency_ms,
                  p95_latency_ms = excluded.p95_latency_ms,
                  sample_count = excluded.sample_count,
                  updated_at = now()
"#;

const ENABLED_PROVIDERS_SQL: &str = r#"
    select id, provider_type, display_name, base_url
    from providers
    where deleted_at is null
      and status = 'active'
      and base_url is not null
    order by display_name, id
"#;

const RECORD_HEALTH_SNAPSHOT_SQL: &str = r#"
    insert into provider_health_snapshots
        (id, provider_id, provider_model_id, status, circuit_state, observed_at, failure_count, latency_ms, metadata)
    values (gen_random_uuid(), $1, null, $2, $3, now(), $4, $5, $6)
"#;

const PRUNE_HEALTH_SNAPSHOTS_SQL: &str = r#"
    delete from provider_health_snapshots
    where provider_model_id is null
      and observed_at < now() - make_interval(hours => $1)
"#;

/// `probes_successful` counts every snapshot whose status is not `unhealthy` — a `degraded`
/// probe still reached the provider, it was merely slow, which is a distinct fact from a probe
/// that could not connect at all. See `classify_probe`
/// (`src/infra/workers/provider_health_check.rs`) for where that line is drawn.
///
/// `latest` is a second lateral join rather than `agg`'s own `order by … limit 1` because an
/// aggregate query cannot also project a non-aggregated column without a `group by` that would
/// defeat the point of aggregating in the first place; two small lateral subqueries per
/// provider is the standard shape for "an aggregate plus the single latest row" in Postgres.
///
/// **Both** laterals bound on `$1`, and the one on `latest` is not decoration. Without it
/// `current_status` is the newest snapshot *ever* recorded, which is a different question from
/// the one every doc comment on this surface answers — see
/// [`ProviderHealthSummaryRow::current_status`] and
/// [`ProviderObservabilityRepository::provider_health_summaries`], both of which promise
/// `unknown` for a provider with no snapshot inside the window. The two answers diverge
/// exactly when the probe stops running: an operator disables the provider (so
/// `ENABLED_PROVIDERS_SQL` no longer returns it) or turns workers off, `prune_health_snapshots`
/// stops running too because it only ever runs from inside `provider-health-check` itself, and
/// the surface then reports `{status: "healthy", probes_total: 0, last_probe_at: null}`
/// indefinitely for a provider nobody has probed in days. Issue #251 finding 3.
///
/// `avg(latency_ms)` is cast to `double precision` **in SQL**, and that cast is load-bearing,
/// not cosmetic. `provider_health_snapshots.latency_ms` is `integer`
/// (`migrations/0005_provider_runtime.sql`), and Postgres `avg(integer)` returns `numeric`.
/// This crate builds sqlx without `bigdecimal` and without `rust_decimal` (`Cargo.toml`), so no
/// `NUMERIC` decoder is compiled in at all: `f64`'s `Type<Postgres>` is `FLOAT8` and
/// `Row::try_get` runs a type-compatibility check on every non-NULL value, so decoding a
/// `numeric` into `Option<f64>` is a hard `ColumnDecode` error — a 500 on `GET
/// /api/v1/admin/providers/health` for as long as any reachable probe is in the window, not a
/// rounding wart. It only *looked* correct because the two states that make the average NULL
/// (no snapshots at all, or every snapshot `unhealthy`, which stores `latency_ms = null`) skip
/// the check entirely. Casting server-side keeps the wire type FLOAT8, which is the one type
/// this build can decode.
///
/// `provider_health_summary_reports_the_average_latency_over_http` in
/// `tests/workers/latency_health_oauth.rs` is the regression test: it drives a real probe to a
/// real snapshot and reads the route, which is the only shape that exercises the decode.
const PROVIDER_HEALTH_SUMMARIES_SQL: &str = r#"
    select
        p.id as provider_id,
        p.provider_type,
        p.display_name,
        coalesce(agg.probes_total, 0) as probes_total,
        coalesce(agg.probes_successful, 0) as probes_successful,
        agg.average_latency_ms,
        agg.last_probe_at,
        agg.last_success_at,
        agg.last_failure_at,
        coalesce(latest.status, 'unknown') as current_status
    from providers p
    left join lateral (
        select
            count(*) as probes_total,
            count(*) filter (where status <> 'unhealthy') as probes_successful,
            avg(latency_ms)::double precision as average_latency_ms,
            max(observed_at) as last_probe_at,
            max(observed_at) filter (where status <> 'unhealthy') as last_success_at,
            max(observed_at) filter (where status = 'unhealthy') as last_failure_at
        from provider_health_snapshots s
        where s.provider_id = p.id
          and s.provider_model_id is null
          and s.observed_at > $1
    ) agg on true
    left join lateral (
        select status
        from provider_health_snapshots s2
        where s2.provider_id = p.id
          and s2.provider_model_id is null
          and s2.observed_at > $1
        order by observed_at desc
        limit 1
    ) latest on true
    where p.deleted_at is null
    order by p.display_name, p.id
"#;

#[cfg(test)]
mod tests {
    use super::*;

    /// The upsert must key off the table's actual primary key, or two concurrent aggregation
    /// runs for different pairs would clobber each other's rows instead of writing side by
    /// side.
    #[test]
    fn the_latency_upsert_conflicts_on_the_composite_primary_key() {
        assert!(UPSERT_LATENCY_STATS_SQL.contains("on conflict (provider_id, provider_model_id)"));
    }

    /// The probe target query must not return a provider with nothing to probe.
    #[test]
    fn enabled_providers_excludes_rows_with_no_base_url() {
        assert!(ENABLED_PROVIDERS_SQL.contains("base_url is not null"));
    }

    /// Every write this repository makes to `provider_health_snapshots` must be a provider-level
    /// row (`provider_model_id = null`); a per-model row here would collide with the semantics
    /// `provider_health_snapshots_provider_recent_idx` (migration 0032) was built for.
    #[test]
    fn health_snapshots_are_always_written_at_provider_scope() {
        assert!(RECORD_HEALTH_SNAPSHOT_SQL.contains("values (gen_random_uuid(), $1, null,"));
    }

    /// `avg(integer)` is `numeric` in Postgres, and this build has no `NUMERIC` decoder (see
    /// the constant's own doc comment). The cast to `double precision` is the only reason
    /// `average_latency_ms` can be read into `Option<f64>` at all, so an edit that drops it
    /// must go red here as well as in the end-to-end test — this one costs no database.
    #[test]
    fn the_health_summary_average_is_cast_to_float8_in_sql() {
        assert!(
            PROVIDER_HEALTH_SUMMARIES_SQL.contains("avg(latency_ms)::double precision"),
            "avg(latency_ms) must be cast in SQL: this build decodes FLOAT8, never NUMERIC"
        );
    }
}
