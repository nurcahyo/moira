//! The `worker_jobs` persistence surface (plan 10 wave 2, finding P3-5).
//!
//! The repository owns the SQL and nothing else. Backoff arithmetic, the
//! dead-letter boundary and the dispatch loop live in `src/infra/workers/queue.rs`
//! — the same split as the cluster lease, where the SQL is here and the startup
//! gate is in `src/app`.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{PgPool, Row, postgres::PgRow};
use uuid::Uuid;

use crate::error::AppError;

/// A job handed to a worker for execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimedJob {
    pub id: Uuid,
    pub job_name: String,
    pub payload: Value,
    /// Already incremented by the claim. A job on its first run reports `1`, so
    /// "attempts used" and "attempts recorded" are the same number and the
    /// dead-letter boundary needs no off-by-one adjustment.
    pub attempts: i32,
    pub max_attempts: i32,
}

/// A job to enqueue.
#[derive(Debug, Clone)]
pub struct WorkerJobInsert {
    pub job_name: String,
    pub payload: Value,
    /// `None` means "as soon as possible".
    pub run_at: Option<DateTime<Utc>>,
    pub max_attempts: i32,
}

#[async_trait]
pub trait WorkerJobRepository: Send + Sync {
    /// Inserts a job **if** the pending backlog is below `max_pending`.
    ///
    /// `Ok(None)` means the cap was reached — a refusal, not an error, so the
    /// caller decides whether that is a `429`-shaped condition or a log line. The
    /// count and the insert are one statement so two concurrent enqueues cannot
    /// both observe room for the last slot.
    async fn enqueue(
        &self,
        job: WorkerJobInsert,
        max_pending: i64,
    ) -> Result<Option<Uuid>, AppError>;

    /// Claims up to `limit` due jobs for `claimed_by`.
    ///
    /// Safe to run on every replica simultaneously — that is what
    /// `for update skip locked` buys, and it is why the queue needs no leader.
    async fn claim_batch(&self, claimed_by: Uuid, limit: i64) -> Result<Vec<ClaimedJob>, AppError>;

    /// Marks a job completed.
    async fn complete(&self, id: Uuid) -> Result<(), AppError>;

    /// Records a failed attempt and schedules the retry at `run_at`.
    async fn reschedule(
        &self,
        id: Uuid,
        run_at: DateTime<Utc>,
        error: &str,
    ) -> Result<(), AppError>;

    /// Moves a job to `dead_letter`.
    async fn dead_letter(&self, id: Uuid, error: &str) -> Result<(), AppError>;

    /// Returns `running` jobs whose claim is older than `stale_seconds` to
    /// `pending`. Returns how many were reclaimed.
    async fn requeue_stale(&self, stale_seconds: i64) -> Result<u64, AppError>;

    /// Deletes terminal rows older than `retention_hours`. Returns how many.
    ///
    /// `i32`, not `i64`: `make_interval(hours => …)` takes an `int`, and binding a
    /// `bigint` fails at run time with `42883` rather than at compile time.
    async fn prune_terminal(&self, retention_hours: i32) -> Result<u64, AppError>;

    /// Jobs waiting to run, due or not.
    async fn pending_depth(&self) -> Result<i64, AppError>;
}

#[derive(Clone)]
pub struct PgWorkerJobRepository {
    pool: PgPool,
}

impl PgWorkerJobRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn claimed_job_from_row(row: &PgRow) -> Result<ClaimedJob, AppError> {
    Ok(ClaimedJob {
        id: row.try_get("id")?,
        job_name: row.try_get("job_name")?,
        payload: row.try_get("payload")?,
        attempts: row.try_get("attempts")?,
        max_attempts: row.try_get("max_attempts")?,
    })
}

#[async_trait]
impl WorkerJobRepository for PgWorkerJobRepository {
    async fn enqueue(
        &self,
        job: WorkerJobInsert,
        max_pending: i64,
    ) -> Result<Option<Uuid>, AppError> {
        let id: Option<Uuid> = sqlx::query_scalar(ENQUEUE_SQL)
            .bind(job.job_name)
            .bind(job.payload)
            .bind(job.run_at)
            .bind(job.max_attempts)
            .bind(max_pending)
            .fetch_optional(&self.pool)
            .await?;
        Ok(id)
    }

    async fn claim_batch(&self, claimed_by: Uuid, limit: i64) -> Result<Vec<ClaimedJob>, AppError> {
        let rows = sqlx::query(CLAIM_BATCH_SQL)
            .bind(claimed_by)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;
        rows.iter().map(claimed_job_from_row).collect()
    }

    async fn complete(&self, id: Uuid) -> Result<(), AppError> {
        sqlx::query(COMPLETE_SQL)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn reschedule(
        &self,
        id: Uuid,
        run_at: DateTime<Utc>,
        error: &str,
    ) -> Result<(), AppError> {
        sqlx::query(RESCHEDULE_SQL)
            .bind(id)
            .bind(run_at)
            .bind(error)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn dead_letter(&self, id: Uuid, error: &str) -> Result<(), AppError> {
        sqlx::query(DEAD_LETTER_SQL)
            .bind(id)
            .bind(error)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn requeue_stale(&self, stale_seconds: i64) -> Result<u64, AppError> {
        let result = sqlx::query(REQUEUE_STALE_SQL)
            .bind(stale_seconds)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }

    async fn prune_terminal(&self, retention_hours: i32) -> Result<u64, AppError> {
        let result = sqlx::query(PRUNE_TERMINAL_SQL)
            .bind(retention_hours)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }

    async fn pending_depth(&self) -> Result<i64, AppError> {
        sqlx::query_scalar(PENDING_DEPTH_SQL)
            .fetch_one(&self.pool)
            .await
            .map_err(AppError::from)
    }
}

/// The cap and the insert are one statement, so two concurrent enqueues cannot
/// both read `depth = max - 1` and both insert. `insert … select … where` inserts
/// zero rows when the predicate is false, which `fetch_optional` reads back as
/// `None` — the refusal, with no second round trip and no transaction to hold
/// open.
const ENQUEUE_SQL: &str = r#"
    insert into worker_jobs (job_name, payload, run_at, max_attempts)
    select $1, $2, coalesce($3, now()), $4
     where (select count(*) from worker_jobs where status = 'pending') < $5
    returning id
"#;

/// # `with victims as materialized`, and why that is a correctness requirement
///
/// The obvious spelling is `update … where id in (select … limit $2)`. **That
/// statement does not bound the number of rows it updates.** `limit` bounds one
/// *evaluation* of the sub-query, and the planner is free to evaluate it once per
/// outer row — a `Nested Loop Semi Join` with the sub-query on the inner side and
/// no `Materialize`. Each re-execution re-locks, `LockRows` skips rows the current
/// command has already modified (`TM_SelfModified`), and so every re-execution
/// returns a *different* id, each of which the outer scan then updates.
///
/// `TM_SelfModified` is returned for a tuple the current command updated exactly
/// as much as for one it deleted, and `nodeLockRows` skips both identically — so
/// this is not a `DELETE`-only hazard, whatever the shape's ubiquity in job-queue
/// blog posts suggests.
///
/// It is also not theoretical in this repository. `src/infra/workers/retention.rs`
/// documents a reproduced case against Moira's own schema where a `limit 1` batch
/// deleted 2 rows, which is how `retention_run_respects_the_configured_batch_size`
/// came to observe 21 and 22 deletions against a per-tick cap of 20.
///
/// `materialized` is explicit rather than assumed: PostgreSQL 12+ inlines
/// single-reference CTEs by default, and an inlined CTE is a sub-query again.
///
/// Do not "simplify" this back to `id in (select …)`.
const CLAIM_BATCH_SQL: &str = r#"
    with victims as materialized (
        select id
          from worker_jobs
         where status = 'pending'
           and run_at <= now()
         order by run_at
         limit $2
           for update skip locked
    )
    update worker_jobs
       set status = 'running',
           attempts = worker_jobs.attempts + 1,
           claimed_by = $1,
           claimed_at = now()
      from victims
     where worker_jobs.id = victims.id
    returning worker_jobs.id,
              worker_jobs.job_name,
              worker_jobs.payload,
              worker_jobs.attempts,
              worker_jobs.max_attempts
"#;

/// `status = 'running'` in the predicate, so a job another replica already
/// reclaimed as stale is not completed out from under its new owner.
const COMPLETE_SQL: &str = r#"
    update worker_jobs
       set status = 'completed',
           completed_at = now(),
           claimed_by = null,
           claimed_at = null
     where id = $1
       and status = 'running'
"#;

/// Back to `pending` with a future `run_at`. `attempts` is **not** incremented
/// here — the claim already did it, so incrementing again would halve the
/// effective retry budget.
const RESCHEDULE_SQL: &str = r#"
    update worker_jobs
       set status = 'pending',
           run_at = $2,
           last_error = $3,
           claimed_by = null,
           claimed_at = null
     where id = $1
       and status = 'running'
"#;

const DEAD_LETTER_SQL: &str = r#"
    update worker_jobs
       set status = 'dead_letter',
           dead_letter_at = now(),
           last_error = $2,
           claimed_by = null,
           claimed_at = null
     where id = $1
       and status = 'running'
"#;

/// A replica killed mid-job never writes a terminal state, so its row would stay
/// `running` forever. This is what makes the queue survive a pod kill.
///
/// `run_at = now()` rather than leaving the old value: the job is due
/// immediately, and the original `run_at` is in the past anyway.
///
/// No `limit`, so there is no victim sub-select to be re-executed — the bound is
/// the number of in-flight jobs in the cluster, which is bounded by concurrency
/// rather than by backlog.
const REQUEUE_STALE_SQL: &str = r#"
    update worker_jobs
       set status = 'pending',
           run_at = now(),
           claimed_by = null,
           claimed_at = null
     where status = 'running'
       and claimed_at < now() - make_interval(secs => $1)
"#;

/// Terminal rows, pruned by age. Bounded by age rather than by `limit`, for the
/// same reason the released-lease prune is.
const PRUNE_TERMINAL_SQL: &str = r#"
    delete from worker_jobs
     where (status = 'completed' and completed_at < now() - make_interval(hours => $1))
        or (status = 'dead_letter' and dead_letter_at < now() - make_interval(hours => $1))
"#;

const PENDING_DEPTH_SQL: &str = r#"
    select count(*) from worker_jobs where status = 'pending'
"#;

#[cfg(test)]
mod tests {
    use super::*;

    /// The single assertion this module exists to protect. See [`CLAIM_BATCH_SQL`].
    #[test]
    fn the_claim_uses_a_materialized_cte_and_not_an_in_subquery() {
        assert!(
            CLAIM_BATCH_SQL.contains("with victims as materialized"),
            "an inlined CTE is a sub-query again, and a re-executed sub-query makes \
             `limit` bound nothing"
        );
        assert!(CLAIM_BATCH_SQL.contains("for update skip locked"));
        assert!(
            !CLAIM_BATCH_SQL.contains("id in (select"),
            "this is the shape that deleted 43 of 43 rows on a `limit 1` batch"
        );
    }

    /// Claim increments; reschedule must not, or the retry budget halves.
    #[test]
    fn only_the_claim_increments_attempts() {
        assert!(CLAIM_BATCH_SQL.contains("attempts = worker_jobs.attempts + 1"));
        assert!(!RESCHEDULE_SQL.contains("attempts"));
        assert!(!DEAD_LETTER_SQL.contains("attempts"));
    }

    /// Every terminal or retry transition must be guarded on `running`, or a
    /// replica whose job was reclaimed as stale can overwrite the state of the
    /// replica that now owns it.
    #[test]
    fn terminal_transitions_are_guarded_on_the_running_state() {
        for sql in [COMPLETE_SQL, RESCHEDULE_SQL, DEAD_LETTER_SQL] {
            assert!(
                sql.contains("and status = 'running'"),
                "unguarded transition:\n{sql}"
            );
        }
    }

    /// The depth check and the insert must be one statement.
    #[test]
    fn the_capacity_check_and_the_insert_are_one_statement() {
        assert!(ENQUEUE_SQL.contains("insert into worker_jobs"));
        assert!(ENQUEUE_SQL.contains("where (select count(*) from worker_jobs"));
    }

    /// A prune bounded by `limit` would be exposed to the re-execution hazard
    /// documented on [`CLAIM_BATCH_SQL`]; both sweeps are bounded by age instead.
    #[test]
    fn the_sweeps_use_no_limit() {
        assert!(!PRUNE_TERMINAL_SQL.contains("limit"));
        assert!(!REQUEUE_STALE_SQL.contains("limit"));
    }
}
