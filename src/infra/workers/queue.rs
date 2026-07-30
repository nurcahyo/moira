//! The durable worker queue (plan 10 wave 2, finding P3-5).
//!
//! Before this module, background work that failed was gone: the supervisor loop
//! had a metrics tick and a retention sweep, no job table, no retry, no
//! dead-letter, no record that anything had been asked for. A job enqueued here
//! survives a pod restart, is retried with exponential backoff, and lands in
//! `dead_letter` rather than being retried forever or dropped silently.
//!
//! # No leader election, and that is the design
//!
//! Every replica claims from this queue simultaneously. The claim is
//! `for update skip locked` over a materialised victim set, so no two claimers
//! can ever receive the same row — see `CLAIM_BATCH_SQL` in
//! `src/infra/repositories/worker_jobs.rs`. Leadership would only be needed to
//! *enqueue* a periodic singleton job, which is a separate concern and already
//! has its mechanism in `super::leader`.
//!
//! # There is no production producer yet, deliberately
//!
//! Plan 10's scope is the queue and the dispatch loop; the job *bodies* —
//! memory extraction, summarisation, embedding, document ingestion — belong to
//! plan 11, and the retention sweep runs directly rather than through the queue
//! because it is leader-gated and needs no per-job durability. So
//! [`WorkerQueue::enqueue`] currently has tests as its only callers. That is the
//! plan's stated shape, not an oversight: shipping the plumbing separately is
//! what lets plan 11 swap a stub handler for a real body without touching any of
//! this.

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde_json::Value;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::{
    config::WorkerSettings,
    error::AppError,
    infra::{
        metrics::MetricsRegistry,
        repositories::{ClaimedJob, WorkerJobInsert, WorkerJobRepository},
    },
};

/// Attempts a job gets before it is dead-lettered, when the enqueuer does not say.
const DEFAULT_MAX_ATTEMPTS: i32 = 5;

/// The delay before a failed attempt is retried.
///
/// Deterministic, with **no jitter**. Jitter exists to de-synchronise a thundering
/// herd of clients retrying the same failed dependency; the jobs here are claimed
/// one at a time by whichever replica gets there first, and `for update skip
/// locked` already staggers them. Adding jitter would buy nothing and would make
/// `backoff_is_deterministic_for_a_given_attempt_count` untestable without
/// bounds-checking a random number, so the choice is made and pinned rather than
/// left open.
///
/// `attempts` is the count **after** the claim's increment, so a job on its first
/// failure has `attempts == 1` and waits `base` — not `base * 2`.
pub fn retry_delay(attempts: i32, base_seconds: u64, max_seconds: u64) -> Duration {
    let base = base_seconds.max(1);
    let max = max_seconds.max(base);
    let exponent = u32::try_from(attempts.max(1) - 1).unwrap_or(u32::MAX);
    // `checked_shl`/`checked_mul` rather than `<<`: attempt 64 would otherwise
    // shift a `u64` by its own width, which is UB-adjacent and in release builds
    // wraps to a *small* delay — a retry storm arriving exactly when the backoff
    // was supposed to be at its longest.
    let scaled = base
        .checked_shl(exponent)
        .filter(|value| *value <= max)
        .unwrap_or(max);
    Duration::from_secs(scaled)
}

/// Whether a job that just failed has any attempts left.
///
/// The boundary, spelled once: `attempts` is post-increment, so
/// `attempts == max_attempts` means the budget is spent and the job dead-letters,
/// while `attempts == max_attempts - 1` retries.
pub fn attempts_exhausted(attempts: i32, max_attempts: i32) -> bool {
    attempts >= max_attempts.max(1)
}

/// Executes one claimed job.
///
/// A trait so a test can inject a handler that always fails — which is the only
/// way to exercise the retry and dead-letter paths without waiting for a real
/// job body to misbehave.
#[async_trait]
pub trait JobDispatcher: Send + Sync {
    /// `Err(message)` records a failed attempt. The message is stored in
    /// `worker_jobs.last_error`, so it must not carry a secret — it is read by
    /// anyone with database access and it is not redacted on the way in.
    async fn dispatch(&self, job: &ClaimedJob) -> Result<(), String>;
}

/// The dispatcher Moira ships today: every declared job name completes
/// immediately with a log line.
///
/// Not `todo!()` and not a panic. A stub that completes is what makes the queue
/// and the leader election independently testable *now*, and it means plan 11
/// replaces one `impl` rather than reshaping the loop around it.
#[derive(Debug, Default, Clone, Copy)]
pub struct StubJobDispatcher;

#[async_trait]
impl JobDispatcher for StubJobDispatcher {
    async fn dispatch(&self, job: &ClaimedJob) -> Result<(), String> {
        if !super::is_known_job_name(&job.job_name) {
            // Not a panic and not a silent success: an unknown name means the row
            // was written by something that bypassed `enqueue`'s check, and
            // completing it would hide that. Failing it lets the retry budget run
            // out and the row land in `dead_letter`, where an operator can see it.
            return Err(format!("no handler is registered for job {}", job.job_name));
        }
        debug!(
            job_id = %job.id,
            job_name = %job.job_name,
            attempts = job.attempts,
            "worker job dispatched to the stub handler"
        );
        Ok(())
    }
}

/// What one poll of the queue did. Returned for tests and for the tick log.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QueueTickOutcome {
    pub reclaimed: u64,
    pub claimed: usize,
    pub completed: usize,
    pub rescheduled: usize,
    pub dead_lettered: usize,
    pub pruned: u64,
}

/// The durable queue, as the supervisor drives it.
#[derive(Clone)]
pub struct WorkerQueue {
    repository: Arc<dyn WorkerJobRepository>,
    settings: Arc<WorkerSettings>,
    /// Recorded in `worker_jobs.claimed_by` for diagnostics only. Claim
    /// correctness comes from `for update skip locked`, never from this value —
    /// which is why a process without an admission lease can still mint one.
    replica_id: Uuid,
}

impl std::fmt::Debug for WorkerQueue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkerQueue")
            .field("replica_id", &self.replica_id)
            .finish_non_exhaustive()
    }
}

impl WorkerQueue {
    pub fn new(
        repository: Arc<dyn WorkerJobRepository>,
        settings: Arc<WorkerSettings>,
        replica_id: Uuid,
    ) -> Self {
        Self {
            repository,
            settings,
            replica_id,
        }
    }

    /// How many jobs one poll claims.
    ///
    /// Reuses `workers.max_concurrent_jobs` rather than introducing a
    /// `claim_batch_size` knob that would mean the same thing: the batch a replica
    /// claims is exactly the work it is willing to run at once, and two settings
    /// that must agree are two settings that can disagree.
    fn claim_batch_size(&self) -> i64 {
        i64::try_from(self.settings.max_concurrent_jobs.max(1)).unwrap_or(i64::MAX)
    }

    /// Enqueues a job.
    ///
    /// # Errors
    ///
    /// `moira.error.worker_queue_capacity_exceeded` (429) when the pending backlog
    /// has reached `workers.queue_max_pending_jobs`. It is a `429` rather than a
    /// `503` because it is backpressure — the request is well-formed, the queue is
    /// momentarily full, and retrying later is the correct client behaviour.
    ///
    /// The error has no HTTP surface *in this plan*, but it is an `AppError` and
    /// will propagate to a response the moment a synchronous caller enqueues —
    /// which plan 11's summarisation and extraction hooks do — so the catalog
    /// entry ships with the code rather than after it.
    pub async fn enqueue(
        &self,
        job_name: &str,
        payload: Value,
        run_at: Option<DateTime<Utc>>,
        metrics: &MetricsRegistry,
    ) -> Result<Uuid, AppError> {
        if !super::is_known_job_name(job_name) {
            // An `Internal`, not a validation error: job names come from code, so
            // an unknown one is a programming mistake rather than bad input.
            return Err(AppError::Internal(format!(
                "worker queue asked to enqueue unknown job {job_name:?}; declare it in \
                 WORKER_JOB_NAMES in src/infra/workers.rs"
            )));
        }
        let inserted = self
            .repository
            .enqueue(
                WorkerJobInsert {
                    job_name: job_name.to_string(),
                    payload,
                    run_at,
                    max_attempts: DEFAULT_MAX_ATTEMPTS,
                },
                self.settings.queue_max_pending_jobs,
            )
            .await?;

        match inserted {
            Some(id) => Ok(id),
            None => {
                metrics.record_worker_queue_enqueue_rejected();
                warn!(
                    job_name,
                    max_pending = self.settings.queue_max_pending_jobs,
                    "worker queue refused an enqueue: the pending-depth cap is reached"
                );
                Err(AppError::coded(
                    axum::http::StatusCode::TOO_MANY_REQUESTS,
                    "worker_queue_capacity_exceeded",
                    "the background job queue is at capacity",
                ))
            }
        }
    }

    /// One poll: reclaim, claim, dispatch, settle.
    ///
    /// Reclaim runs **first**. A replica that died mid-job left its row `running`,
    /// and claiming before reclaiming would mean the current poll never sees work
    /// that has been stuck since the last one — a job orphaned by a pod kill would
    /// wait a full extra poll interval for no reason.
    pub async fn run_once(
        &self,
        dispatcher: &dyn JobDispatcher,
        metrics: &MetricsRegistry,
    ) -> Result<QueueTickOutcome, AppError> {
        let mut outcome = QueueTickOutcome {
            reclaimed: self
                .repository
                .requeue_stale(
                    i64::try_from(self.settings.queue_stale_claim_seconds).unwrap_or(i64::MAX),
                )
                .await?,
            ..QueueTickOutcome::default()
        };
        if outcome.reclaimed > 0 {
            warn!(
                reclaimed = outcome.reclaimed,
                stale_after_seconds = self.settings.queue_stale_claim_seconds,
                "requeued worker jobs whose claimer never reported an outcome"
            );
        }

        let claimed = self
            .repository
            .claim_batch(self.replica_id, self.claim_batch_size())
            .await?;
        outcome.claimed = claimed.len();

        for job in &claimed {
            metrics.record_worker_job_claimed(&job.job_name, 1);
            match dispatcher.dispatch(job).await {
                Ok(()) => {
                    self.repository.complete(job.id).await?;
                    metrics.record_worker_job_completed(&job.job_name);
                    outcome.completed += 1;
                }
                Err(error) => {
                    self.settle_failure(job, &error, metrics, &mut outcome)
                        .await?;
                }
            }
        }

        outcome.pruned = self
            .repository
            .prune_terminal(
                i32::try_from(self.settings.dead_letter_retention_hours.max(1)).unwrap_or(i32::MAX),
            )
            .await?;

        Ok(outcome)
    }

    /// Retry with backoff, or dead-letter if the budget is spent.
    async fn settle_failure(
        &self,
        job: &ClaimedJob,
        error: &str,
        metrics: &MetricsRegistry,
        outcome: &mut QueueTickOutcome,
    ) -> Result<(), AppError> {
        if attempts_exhausted(job.attempts, job.max_attempts) {
            self.repository.dead_letter(job.id, error).await?;
            metrics.record_worker_job_dead_lettered(&job.job_name);
            outcome.dead_lettered += 1;
            // `warn`, not `debug`: a dead letter is work the deployment was asked
            // to do and will now never do. It is the one queue event that deserves
            // an operator's attention without a dashboard.
            warn!(
                job_id = %job.id,
                job_name = %job.job_name,
                attempts = job.attempts,
                error,
                "worker job dead-lettered after exhausting its attempts"
            );
            return Ok(());
        }

        let delay = retry_delay(
            job.attempts,
            self.settings.retry_base_delay_seconds,
            self.settings.retry_max_delay_seconds,
        );
        let run_at = Utc::now()
            + chrono::Duration::from_std(delay).unwrap_or_else(|_| chrono::Duration::seconds(1));
        self.repository.reschedule(job.id, run_at, error).await?;
        metrics.record_worker_job_failed(&job.job_name);
        outcome.rescheduled += 1;
        info!(
            job_id = %job.id,
            job_name = %job.job_name,
            attempts = job.attempts,
            retry_in_seconds = delay.as_secs(),
            error,
            "worker job failed; scheduled for retry"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(base: u64, max: u64) -> WorkerSettings {
        WorkerSettings {
            retry_base_delay_seconds: base,
            retry_max_delay_seconds: max,
            ..WorkerSettings::default()
        }
    }

    #[test]
    fn backoff_grows_exponentially_from_retry_base_delay() {
        let settings = settings(5, 300);
        let delay = |attempts| {
            retry_delay(
                attempts,
                settings.retry_base_delay_seconds,
                settings.retry_max_delay_seconds,
            )
            .as_secs()
        };
        // Attempt counts are post-increment, so the first failure waits `base`.
        assert_eq!(delay(1), 5);
        assert_eq!(delay(2), 10);
        assert_eq!(delay(3), 20);
        assert_eq!(delay(4), 40);
    }

    #[test]
    fn backoff_is_clamped_at_retry_max_delay() {
        assert_eq!(retry_delay(20, 5, 300).as_secs(), 300);
        assert_eq!(retry_delay(7, 5, 300).as_secs(), 300);
    }

    /// A shift by the width of the type wraps in release builds, which would turn
    /// the longest backoff into the shortest one — a retry storm at exactly the
    /// wrong moment.
    #[test]
    fn backoff_does_not_wrap_at_absurd_attempt_counts() {
        assert_eq!(retry_delay(i32::MAX, 5, 300).as_secs(), 300);
        assert_eq!(retry_delay(64, 1, 900).as_secs(), 900);
        assert_eq!(retry_delay(65, 1, 900).as_secs(), 900);
    }

    #[test]
    fn backoff_is_deterministic_for_a_given_attempt_count() {
        assert_eq!(retry_delay(3, 5, 300), retry_delay(3, 5, 300));
    }

    /// A zero or negative attempt count must not underflow the exponent.
    #[test]
    fn backoff_treats_a_zero_attempt_count_as_the_first_attempt() {
        assert_eq!(retry_delay(0, 5, 300).as_secs(), 5);
        assert_eq!(retry_delay(-3, 5, 300).as_secs(), 5);
    }

    /// The boundary that decides whether work is retried or abandoned.
    #[test]
    fn job_moves_to_dead_letter_exactly_at_max_attempts() {
        assert!(!attempts_exhausted(4, 5));
        assert!(attempts_exhausted(5, 5));
        assert!(attempts_exhausted(6, 5));
    }

    /// `max_attempts` is `check (max_attempts > 0)` in the schema, but a zero
    /// arriving from anywhere else must dead-letter rather than retry forever.
    #[test]
    fn a_zero_attempt_budget_dead_letters_rather_than_looping() {
        assert!(attempts_exhausted(1, 0));
    }

    #[tokio::test]
    async fn the_stub_dispatcher_completes_declared_jobs_and_fails_unknown_ones() {
        let dispatcher = StubJobDispatcher;
        let job = |name: &str| ClaimedJob {
            id: Uuid::now_v7(),
            job_name: name.to_string(),
            payload: Value::Null,
            attempts: 1,
            max_attempts: 5,
        };
        assert!(
            dispatcher
                .dispatch(&job(super::super::RETENTION_CLEANUP_WORKER))
                .await
                .is_ok()
        );
        // A row written around `enqueue`'s check must not be silently completed.
        assert!(
            dispatcher
                .dispatch(&job("not-a-declared-job"))
                .await
                .is_err()
        );
    }
}
