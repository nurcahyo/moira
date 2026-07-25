//! Retention / cleanup worker (plan 04, finding P1-5).
//!
//! Two tables accumulate rows that nothing else ever deletes:
//!
//! * `idempotency_records` — every admin command that goes through the
//!   idempotency envelope commits a ledger row with a `not null expires_at`.
//!   The claim path (`src/infra/repositories/admin.rs`, `claim_idempotency`)
//!   opportunistically deletes expired rows *for the exact key being claimed*,
//!   which prunes nothing for keys that are never presented again. The growth is
//!   directly caller-driven: an actor holding a single narrow scope mints one row
//!   per request simply by using a fresh `Idempotency-Key` each time, including
//!   on requests that fail.
//! * `responses` — public execution responses carry a nullable `expires_at`
//!   derived from the application's retention policy. Nothing acts on it.
//!
//! This module is the delete side of both.
//!
//! # Multi-replica posture (both halves, honestly)
//!
//! **Not multi-replica-safe in the "avoid duplicate work" sense.** There is no
//! leader election in Moira today (that is plan 10). Every replica that has
//! workers enabled runs its own sweep on its own schedule, so N replicas do up to
//! N times the scanning work.
//!
//! **But correct under concurrency.** The delete is idempotent — a row that
//! another replica already removed simply is not there to remove again — and each
//! batch selects its victims with `for update skip locked`, so two concurrent
//! sweeps partition the expired rows between themselves instead of contending for
//! them, and neither one blocks (nor is blocked by) the row locks taken by a
//! concurrent idempotency claim. No double-delete corruption, no lock convoy, no
//! wrong counts beyond each replica reporting only the rows it personally deleted.
//!
//! Both halves are true at once. A comment claiming only the first would suggest
//! this worker is unsafe to run today; a comment claiming only the second would
//! suggest leader election is unnecessary. Neither is right.

use serde::Serialize;
use sqlx::PgPool;
use tracing::debug;

use crate::{config::WorkerSettings, error::AppError, infra::metrics::MetricsRegistry};

/// Table name recorded against the `idempotency_records` deletion counter.
pub const TABLE_IDEMPOTENCY_RECORDS: &str = "idempotency_records";
/// Table name recorded against the `responses` deletion counter.
pub const TABLE_RESPONSES: &str = "responses";

/// What one sweep actually did. Counts are per-process: they report rows this
/// replica deleted, not rows that expired.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct RetentionOutcome {
    pub idempotency_records_deleted: u64,
    pub responses_deleted: u64,
    /// Number of batch statements executed across both tables.
    pub batches_run: u32,
    /// True when a table stopped because it hit the per-tick cap rather than
    /// because it ran out of expired rows — i.e. there is a backlog left for the
    /// next tick.
    pub hit_per_tick_cap: bool,
}

impl RetentionOutcome {
    pub fn total_deleted(&self) -> u64 {
        self.idempotency_records_deleted
            .saturating_add(self.responses_deleted)
    }

    fn record_table(&mut self, table: &str, deleted: u64) {
        match table {
            TABLE_IDEMPOTENCY_RECORDS => {
                self.idempotency_records_deleted =
                    self.idempotency_records_deleted.saturating_add(deleted);
            }
            TABLE_RESPONSES => {
                self.responses_deleted = self.responses_deleted.saturating_add(deleted);
            }
            _ => {}
        }
    }
}

/// The pure batching policy, split out from the SQL so it is unit-testable
/// without a database.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetentionPlan {
    batch_size: i64,
    per_tick_cap: u64,
}

impl RetentionPlan {
    /// Upper clamp on a configured batch size. A batch far larger than this stops
    /// being "short" — it holds row locks and WAL for long enough to matter — so
    /// a fat-fingered config value is capped rather than honoured.
    pub const MAX_BATCH_SIZE: usize = 10_000;

    /// How many batches' worth of rows a single tick may delete from one table
    /// before yielding until the next tick. Bounds the worst-case duration of one
    /// sweep when a huge backlog exists, so the worker degrades to "drains over
    /// several ticks" instead of "runs for minutes".
    pub const PER_TICK_BATCH_BUDGET: u64 = 20;

    pub fn from_settings(settings: &WorkerSettings) -> Self {
        let batch_size = settings
            .retention_batch_size
            .clamp(1, Self::MAX_BATCH_SIZE)
            .min(i64::MAX as usize) as i64;
        Self {
            batch_size,
            per_tick_cap: (batch_size as u64).saturating_mul(Self::PER_TICK_BATCH_BUDGET),
        }
    }

    pub fn batch_size(&self) -> i64 {
        self.batch_size
    }

    pub fn per_tick_cap(&self) -> u64 {
        self.per_tick_cap
    }

    /// Size of the next batch for a table that has already deleted
    /// `deleted_so_far` rows this tick, or `None` when the per-tick cap is
    /// exhausted. The returned size never overshoots the cap, so the cap is an
    /// exact bound and not an approximate one.
    pub fn next_batch_size(&self, deleted_so_far: u64) -> Option<i64> {
        let remaining = self.per_tick_cap.checked_sub(deleted_so_far)?;
        if remaining == 0 {
            return None;
        }
        Some(remaining.min(self.batch_size as u64) as i64)
    }

    /// Whether to attempt another batch after one that requested `requested` rows
    /// and deleted `deleted`.
    ///
    /// A short batch means this sweep found no more claimable expired rows —
    /// either the table is drained, or the remainder is locked by a concurrent
    /// transaction and was skipped. Both cases resolve on the next tick, so we
    /// stop rather than spin.
    pub fn should_continue(deleted: u64, requested: i64) -> bool {
        requested > 0 && deleted >= requested as u64
    }

    /// Interval between sweeps, with `0` normalised to `1` — `tokio` panics on a
    /// zero-length interval period.
    pub fn interval_seconds(settings: &WorkerSettings) -> u64 {
        settings.retention_interval_seconds.max(1)
    }
}

/// Runs one retention sweep across both target tables.
///
/// Each batch is a single `DELETE` statement and therefore its own transaction:
/// a crash mid-sweep leaves already-committed batches deleted and the rest for
/// the next tick. There is deliberately no enclosing transaction — one would turn
/// the whole sweep into a single long-lived write transaction, which is exactly
/// what the batching exists to avoid.
pub async fn run_once(
    pool: &PgPool,
    settings: &WorkerSettings,
    metrics: &MetricsRegistry,
) -> Result<RetentionOutcome, AppError> {
    let plan = RetentionPlan::from_settings(settings);
    let mut outcome = RetentionOutcome::default();

    for table in [TABLE_IDEMPOTENCY_RECORDS, TABLE_RESPONSES] {
        let table_outcome = sweep_table(pool, table, plan).await?;
        outcome.record_table(table, table_outcome.deleted);
        outcome.batches_run = outcome.batches_run.saturating_add(table_outcome.batches);
        outcome.hit_per_tick_cap |= table_outcome.hit_cap;
        if table_outcome.deleted > 0 {
            metrics.record_retention_deleted(table, table_outcome.deleted);
        }
    }

    metrics.record_retention_run();
    Ok(outcome)
}

#[derive(Debug, Default)]
struct TableSweep {
    deleted: u64,
    batches: u32,
    hit_cap: bool,
}

async fn sweep_table(
    pool: &PgPool,
    table: &str,
    plan: RetentionPlan,
) -> Result<TableSweep, AppError> {
    let mut sweep = TableSweep::default();
    loop {
        let Some(requested) = plan.next_batch_size(sweep.deleted) else {
            sweep.hit_cap = true;
            break;
        };

        let deleted = delete_expired_batch(pool, table, requested).await?;
        sweep.deleted = sweep.deleted.saturating_add(deleted);
        sweep.batches = sweep.batches.saturating_add(1);
        debug!(
            table,
            requested, deleted, "retention batch deleted expired rows"
        );

        if !RetentionPlan::should_continue(deleted, requested) {
            break;
        }
    }
    Ok(sweep)
}

/// One batch, one statement, one transaction.
///
/// PostgreSQL has no `DELETE ... LIMIT`, so the victims are chosen by a bounded
/// sub-select. `for update skip locked` is load-bearing, not decoration: the
/// idempotency claim path deletes expired rows for the key it is claiming while
/// holding an advisory transaction lock, so without `skip locked` a retention
/// batch that reached those rows first would block that claim — a user-facing
/// request — behind a background sweep. With it, the sweep steps over any locked
/// row and picks it up on a later tick.
///
/// The table name is chosen from a closed set of `&'static str` constants in this
/// module, never from caller input; the bound is a bind parameter.
async fn delete_expired_batch(pool: &PgPool, table: &str, limit: i64) -> Result<u64, AppError> {
    let sql = delete_sql(table).ok_or_else(|| {
        AppError::Internal(format!(
            "retention worker asked to sweep unknown table {table}"
        ))
    })?;
    let result = sqlx::query(sql).bind(limit).execute(pool).await?;
    Ok(result.rows_affected())
}

/// Closed-set lookup from table name to its batch-delete statement. Pure, so the
/// "no caller string ever reaches SQL" property is unit-testable.
fn delete_sql(table: &str) -> Option<&'static str> {
    match table {
        TABLE_IDEMPOTENCY_RECORDS => Some(
            r#"
            delete from idempotency_records
            where id in (
                select id
                from idempotency_records
                where expires_at < now()
                order by expires_at
                limit $1
                for update skip locked
            )
            "#,
        ),
        // `expires_at is not null` is redundant against `< now()` but stated
        // explicitly so the partial index `responses_expires_at_idx`
        // (`migrations/0011_retention_indexes.sql`) is matched without relying on
        // the planner's predicate-implication prover.
        TABLE_RESPONSES => Some(
            r#"
            delete from responses
            where id in (
                select id
                from responses
                where expires_at is not null
                  and expires_at < now()
                order by expires_at
                limit $1
                for update skip locked
            )
            "#,
        ),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(batch_size: usize, interval_seconds: u64) -> WorkerSettings {
        WorkerSettings {
            retention_batch_size: batch_size,
            retention_interval_seconds: interval_seconds,
            ..WorkerSettings::default()
        }
    }

    #[test]
    fn plan_uses_the_configured_batch_size() {
        let plan = RetentionPlan::from_settings(&settings(500, 300));
        assert_eq!(plan.batch_size(), 500);
        assert_eq!(
            plan.per_tick_cap(),
            500 * RetentionPlan::PER_TICK_BATCH_BUDGET
        );
    }

    #[test]
    fn zero_batch_size_is_clamped_up_to_one_so_the_sweep_can_never_stall() {
        let plan = RetentionPlan::from_settings(&settings(0, 300));
        assert_eq!(plan.batch_size(), 1);
        assert_eq!(plan.next_batch_size(0), Some(1));
    }

    #[test]
    fn oversized_batch_size_is_clamped_to_the_maximum() {
        let plan = RetentionPlan::from_settings(&settings(usize::MAX, 300));
        assert_eq!(plan.batch_size(), RetentionPlan::MAX_BATCH_SIZE as i64);
    }

    #[test]
    fn next_batch_size_is_the_full_batch_until_the_cap_is_approached() {
        let plan = RetentionPlan::from_settings(&settings(100, 300));
        assert_eq!(plan.next_batch_size(0), Some(100));
        assert_eq!(plan.next_batch_size(900), Some(100));
    }

    #[test]
    fn next_batch_size_is_trimmed_so_the_per_tick_cap_is_never_overshot() {
        let plan = RetentionPlan::from_settings(&settings(100, 300));
        let cap = plan.per_tick_cap();
        assert_eq!(plan.next_batch_size(cap - 40), Some(40));
    }

    #[test]
    fn next_batch_size_is_none_once_the_per_tick_cap_is_reached() {
        let plan = RetentionPlan::from_settings(&settings(100, 300));
        assert_eq!(plan.next_batch_size(plan.per_tick_cap()), None);
        assert_eq!(plan.next_batch_size(plan.per_tick_cap() + 1), None);
    }

    #[test]
    fn a_full_batch_continues_the_loop() {
        assert!(RetentionPlan::should_continue(500, 500));
    }

    #[test]
    fn a_short_batch_stops_the_loop() {
        assert!(!RetentionPlan::should_continue(499, 500));
    }

    #[test]
    fn an_empty_batch_stops_the_loop() {
        assert!(!RetentionPlan::should_continue(0, 500));
    }

    #[test]
    fn batching_a_backlog_drains_it_in_ceil_divided_batches() {
        // Simulates the loop in `sweep_table` against a table holding `backlog`
        // expired rows, with no database involved.
        fn simulate(plan: RetentionPlan, mut backlog: u64) -> (u64, u32, bool) {
            let mut deleted_total = 0;
            let mut batches = 0;
            loop {
                let Some(requested) = plan.next_batch_size(deleted_total) else {
                    return (deleted_total, batches, true);
                };
                let deleted = backlog.min(requested as u64);
                backlog -= deleted;
                deleted_total += deleted;
                batches += 1;
                if !RetentionPlan::should_continue(deleted, requested) {
                    return (deleted_total, batches, false);
                }
            }
        }

        let plan = RetentionPlan::from_settings(&settings(10, 300));

        // Exact multiple: 3 full batches, then a 4th that comes back empty.
        assert_eq!(simulate(plan, 30), (30, 4, false));
        // Remainder: 2 full batches, then a short one that ends the sweep.
        assert_eq!(simulate(plan, 25), (25, 3, false));
        // Empty table: one batch, nothing deleted.
        assert_eq!(simulate(plan, 0), (0, 1, false));
    }

    #[test]
    fn a_backlog_larger_than_the_per_tick_cap_stops_at_the_cap_and_reports_it() {
        fn simulate(plan: RetentionPlan, mut backlog: u64) -> (u64, bool) {
            let mut deleted_total = 0;
            loop {
                let Some(requested) = plan.next_batch_size(deleted_total) else {
                    return (deleted_total, true);
                };
                let deleted = backlog.min(requested as u64);
                backlog -= deleted;
                deleted_total += deleted;
                if !RetentionPlan::should_continue(deleted, requested) {
                    return (deleted_total, false);
                }
            }
        }

        let plan = RetentionPlan::from_settings(&settings(10, 300));
        let cap = plan.per_tick_cap();
        let (deleted, hit_cap) = simulate(plan, cap * 3);
        assert_eq!(
            deleted, cap,
            "a single tick must not exceed the per-tick cap"
        );
        assert!(
            hit_cap,
            "the leftover backlog must be reported for the next tick"
        );
    }

    #[test]
    fn interval_seconds_never_returns_zero() {
        assert_eq!(RetentionPlan::interval_seconds(&settings(500, 0)), 1);
        assert_eq!(RetentionPlan::interval_seconds(&settings(500, 300)), 300);
    }

    #[test]
    fn outcome_records_each_table_against_its_own_counter() {
        let mut outcome = RetentionOutcome::default();
        outcome.record_table(TABLE_IDEMPOTENCY_RECORDS, 7);
        outcome.record_table(TABLE_RESPONSES, 5);
        outcome.record_table(TABLE_IDEMPOTENCY_RECORDS, 3);
        assert_eq!(outcome.idempotency_records_deleted, 10);
        assert_eq!(outcome.responses_deleted, 5);
        assert_eq!(outcome.total_deleted(), 15);
    }

    #[test]
    fn only_the_two_retention_tables_resolve_to_sql() {
        assert!(delete_sql(TABLE_IDEMPOTENCY_RECORDS).is_some());
        assert!(delete_sql(TABLE_RESPONSES).is_some());
        for hostile in [
            "",
            "responses; drop table responses",
            "pg_catalog.pg_class",
            "IDEMPOTENCY_RECORDS",
            "audit_logs",
        ] {
            assert!(
                delete_sql(hostile).is_none(),
                "unexpected SQL for table name {hostile:?}"
            );
        }
    }

    #[test]
    fn batch_delete_sql_binds_its_limit_and_skips_locked_rows() {
        for table in [TABLE_IDEMPOTENCY_RECORDS, TABLE_RESPONSES] {
            let sql = delete_sql(table).expect("known table");
            // The bound is a bind parameter, never an interpolated literal.
            assert!(
                sql.contains("limit $1"),
                "{table} batch is not parameterised"
            );
            // `skip locked` is what keeps a sweep from blocking a concurrent
            // idempotency claim; losing it would be a silent regression.
            assert!(
                sql.contains("for update skip locked"),
                "{table} batch lost skip locked"
            );
            assert!(sql.contains("expires_at < now()"));
        }
        assert!(
            delete_sql(TABLE_RESPONSES)
                .expect("known table")
                .contains("expires_at is not null"),
            "responses batch must match the partial index predicate"
        );
    }
}
