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
//! Plan 10's scope was the queue itself; issue #90 added the per-name dispatch loop on top
//! of it (`super::dispatch::RealJobDispatcher`). The job *bodies* — memory extraction,
//! summarisation, embedding, document ingestion — still belong to plan 11: those pipelines
//! run inline on the response path today (see `extract_memories` in
//! `src/application/conversation.rs`), and reaching them from a queue job needs an `Actor`
//! and a `RequestContext` a bare job payload does not carry. So [`WorkerQueue::enqueue`]
//! currently has tests as its only callers. That is deliberate, not an oversight: shipping
//! the queue and the dispatch plumbing separately from the pipeline bodies is what lets
//! plan 11 swap `dispatch::DeferredPipelineHandler` for a real handler per job name without
//! touching any of this.

use std::{panic::AssertUnwindSafe, sync::Arc, time::Duration};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use futures_util::FutureExt;
use serde_json::Value;
use tokio::time::Instant;
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

/// The share of `workers.queue_stale_claim_seconds` one poll may spend inside
/// handlers before it stops dispatching the rest of its claimed batch.
///
/// This is the queue's central safety invariant, and it is a *derived* percentage
/// rather than a knob on purpose: the thing that must hold is "a poll finishes
/// dispatching before its own claims go stale", and two independent settings that
/// must satisfy an inequality are two settings that can be made to violate it.
/// `queue_job_timeout_seconds` bounds one job; this bounds the batch, which is the
/// half that actually matters — the batch is `max_concurrent_jobs` jobs wide
/// (8 by default) and dispatch is sequential, so eight jobs each finishing inside
/// their own per-job timeout can still overrun the stale threshold together.
///
/// What goes wrong without it, concretely: `requeue_stale` on any replica flips a
/// still-running job back to `pending`, a second replica claims and runs the *same*
/// job concurrently, and the first replica then calls `complete()` on a row it no
/// longer owns. Issue #251 finding 4.
///
/// The remaining 20% is headroom for the settle writes (`complete`,
/// `reschedule`, `dead_letter`) and `prune_terminal`, which run after the last
/// dispatch and are still inside the claim.
const DISPATCH_BUDGET_PERCENT_OF_STALE_CLAIM: u64 = 80;

/// Runs one job body under a timeout, with a panic caught rather than unwound.
///
/// Two separate hazards, one wrapper, because both end the same way — an `Err`
/// string the queue already knows how to retry and dead-letter — and because
/// neither can be handled at the call site once it has escaped.
///
/// **Timeout.** `budget` is the caller's remaining slice of
/// [`DISPATCH_BUDGET_PERCENT_OF_STALE_CLAIM`]. Cancelling at the budget is what
/// keeps a claim from going stale under its own executor; see that constant.
///
/// **Panic.** `run_supervisor` is a bare `tokio::spawn` whose `JoinHandle` is only
/// awaited by `WorkerSupervisor::shutdown`, which discards the `JoinError`. So
/// before this wrapper a panic in *any* handler unwound through `run_once` and
/// killed the supervisor task for the remaining life of the process: no further
/// polls, no retention sweep, no `moira_worker_tick`, and no `leader.resign()` —
/// while `/health/ready`, which does not consult the supervisor, kept the pod in
/// service. Issue #251 finding 5.
///
/// The panic *payload* is deliberately dropped rather than formatted into the
/// message. [`JobDispatcher::dispatch`]'s contract is that its `Err` lands in
/// `worker_jobs.last_error` unredacted, and a panic message is arbitrary
/// handler-chosen text — an `expect` on a decrypted value would put plaintext in a
/// table read by anyone with database access. The default panic hook has already
/// written the payload and its location to stderr, where it belongs.
pub async fn dispatch_guarded(
    dispatcher: &dyn JobDispatcher,
    job: &ClaimedJob,
    budget: Duration,
) -> Result<(), String> {
    // `AssertUnwindSafe` because `&dyn JobDispatcher` and `&ClaimedJob` are shared
    // references the caller still owns after this returns: nothing here can observe
    // a broken invariant left by the unwind, since the handler's own state is
    // behind `&self` and the queue re-reads every row it acts on.
    let attempt = AssertUnwindSafe(dispatcher.dispatch(job)).catch_unwind();
    match tokio::time::timeout(budget, attempt).await {
        Ok(Ok(result)) => result,
        Ok(Err(_payload)) => Err(format!(
            "the handler for job {} panicked; see the process log for the panic itself",
            job.job_name
        )),
        Err(_elapsed) => Err(format!(
            "the handler for job {} exceeded its {}s slice of this poll's dispatch budget",
            job.job_name,
            budget.as_secs()
        )),
    }
}

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

/// A dispatcher that completes every declared job name immediately with a log line,
/// regardless of what it is.
///
/// **No longer what `run_supervisor` drives** — issue #90 replaced that with
/// `super::dispatch::RealJobDispatcher`, which routes by `job_name` to a registered
/// per-name handler instead of treating every declared name identically. This type is
/// retained because it is still useful for exercising the queue's claim/retry/dead-letter
/// mechanics (`tests/workers/worker_queue.rs`, `tests/workers/coordination_default_path.rs`)
/// independently of job semantics — those tests care that a claimed job gets *completed*,
/// not which handler completed it.
///
/// Not `todo!()` and not a panic, for the same reason it never was: completing a declared
/// name is what makes the queue testable with no handler in the loop at all.
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
    /// Jobs this poll claimed but never handed to a handler, because the poll's
    /// dispatch budget ran out or shutdown was signalled part-way through the
    /// batch. Their rows stay `running` and are reclaimed by `requeue_stale` —
    /// the same path a pod kill takes, which is why leaving them is safe and
    /// completing or failing them here would not be: this replica has not run
    /// them and has no outcome to report.
    pub undispatched: usize,
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

    /// How long one poll may spend dispatching its whole claimed batch.
    ///
    /// Derived from `queue_stale_claim_seconds`, never configured beside it — see
    /// [`DISPATCH_BUDGET_PERCENT_OF_STALE_CLAIM`]. Clamped rather than validated
    /// here because `WorkerSettings` is constructed directly by tests and by
    /// callers that never run `Settings::validate`, so the invariant has to hold
    /// on the values as given, not on the values as checked.
    fn dispatch_budget(&self) -> Duration {
        let stale = self.settings.queue_stale_claim_seconds.max(1);
        Duration::from_secs(
            (stale.saturating_mul(DISPATCH_BUDGET_PERCENT_OF_STALE_CLAIM) / 100).max(1),
        )
    }

    /// The ceiling on one job body, before the batch budget narrows it further.
    fn job_timeout(&self) -> Duration {
        Duration::from_secs(self.settings.queue_job_timeout_seconds.max(1))
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

    /// Enqueues `job_name` unless it already has a `pending`/`running` row, or the
    /// pending-depth cap is reached.
    ///
    /// `Ok(None)` covers both refusals and is not an error: an idle-checked
    /// periodic maintenance enqueue finding its own name already queued is the
    /// expected steady state, not a capacity problem a caller needs to react to
    /// the way [`Self::enqueue`]'s callers do. See
    /// `WorkerRegistry::enqueue_due_maintenance_jobs` in `src/infra/workers.rs`
    /// for the only caller today.
    pub async fn enqueue_periodic(
        &self,
        job_name: &str,
        metrics: &MetricsRegistry,
    ) -> Result<Option<Uuid>, AppError> {
        if !super::is_known_job_name(job_name) {
            return Err(AppError::Internal(format!(
                "worker queue asked to enqueue unknown job {job_name:?}; declare it in \
                 WORKER_JOB_NAMES in src/infra/workers.rs"
            )));
        }
        let inserted = self
            .repository
            .enqueue_if_idle(
                WorkerJobInsert {
                    job_name: job_name.to_string(),
                    payload: Value::Null,
                    run_at: None,
                    max_attempts: DEFAULT_MAX_ATTEMPTS,
                },
                self.settings.queue_max_pending_jobs,
            )
            .await?;
        if inserted.is_none() {
            // Distinguishing "already idle-checked out" from "capacity cap hit" would
            // need a second query on every miss; neither warrants a metric or a log
            // above debug in `enqueue_due_maintenance_jobs`, so the caller does not
            // need the distinction.
            metrics.record_worker_queue_enqueue_rejected();
        }
        Ok(inserted)
    }

    /// One poll: reclaim, claim, dispatch, settle. Runs the whole claimed batch.
    ///
    /// Equivalent to [`Self::run_once_until`] with a stop signal that never fires.
    /// Keep using this from tests and from any caller with no shutdown to observe;
    /// `run_supervisor` uses the `_until` form so a stop signal is not queued
    /// behind the rest of the batch.
    pub async fn run_once(
        &self,
        dispatcher: &dyn JobDispatcher,
        metrics: &MetricsRegistry,
    ) -> Result<QueueTickOutcome, AppError> {
        self.run_once_until(dispatcher, metrics, &|| false).await
    }

    /// One poll: reclaim, claim, dispatch, settle.
    ///
    /// Reclaim runs **first**. A replica that died mid-job left its row `running`,
    /// and claiming before reclaiming would mean the current poll never sees work
    /// that has been stuck since the last one — a job orphaned by a pod kill would
    /// wait a full extra poll interval for no reason.
    ///
    /// # Two bounds on the dispatch loop
    ///
    /// `stop` is consulted **between** jobs, never mid-job: a job already inside a
    /// handler runs to its own timeout, and only the *rest* of the batch is left
    /// for another replica. That is the difference between a graceful stop and
    /// cancelling a half-written handler, and it is why shutdown is a predicate
    /// here rather than a `select!` arm racing the whole poll.
    ///
    /// The batch budget ([`Self::dispatch_budget`]) is the other bound, and it is
    /// the one that closes issue #251 finding 4: without it a batch of eight jobs,
    /// each individually inside its own timeout, can still keep this poll running
    /// past `queue_stale_claim_seconds`, at which point another replica's
    /// `requeue_stale` hands the row it is still executing to a second executor.
    ///
    /// Jobs neither bound let through are counted in
    /// [`QueueTickOutcome::undispatched`] and left `running` on purpose; see that
    /// field.
    pub async fn run_once_until(
        &self,
        dispatcher: &dyn JobDispatcher,
        metrics: &MetricsRegistry,
        stop: &(dyn Fn() -> bool + Send + Sync),
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
        // Every claimed row, not only the ones that reach a handler: the metric
        // names what the claim query took, and `outcome.claimed` is the same
        // number, so recording it per dispatch would make the two disagree exactly
        // when a batch is cut short — the case an operator is looking at the
        // metric to understand.
        for job in &claimed {
            metrics.record_worker_job_claimed(&job.job_name, 1);
        }

        let deadline = Instant::now() + self.dispatch_budget();
        let job_timeout = self.job_timeout();

        for (dispatched, job) in claimed.iter().enumerate() {
            let stopping = stop();
            let remaining = deadline.saturating_duration_since(Instant::now());
            if stopping || remaining.is_zero() {
                outcome.undispatched = claimed.len() - dispatched;
                warn!(
                    undispatched = outcome.undispatched,
                    dispatched,
                    shutting_down = stopping,
                    budget_seconds = self.dispatch_budget().as_secs(),
                    stale_after_seconds = self.settings.queue_stale_claim_seconds,
                    "worker queue stopped dispatching part-way through its claimed batch; \
                     the remaining rows stay running until the stale-claim sweep reclaims them"
                );
                break;
            }
            match dispatch_guarded(dispatcher, job, job_timeout.min(remaining)).await {
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

    // -----------------------------------------------------------------------------------
    // `dispatch_guarded` — issue #251 findings 4 and 5
    // -----------------------------------------------------------------------------------

    fn guarded_test_job() -> ClaimedJob {
        ClaimedJob {
            id: Uuid::now_v7(),
            job_name: super::super::RETENTION_CLEANUP_WORKER.to_string(),
            payload: Value::Null,
            attempts: 1,
            max_attempts: 5,
        }
    }

    /// A handler that never returns, standing in for `oauth-token-refresh` walking
    /// 100 credentials against an IdP that accepts connections and never answers.
    struct NeverReturns;

    #[async_trait]
    impl JobDispatcher for NeverReturns {
        async fn dispatch(&self, _job: &ClaimedJob) -> Result<(), String> {
            // Far longer than any budget a test would set, so the timeout is what
            // ends this and the test cannot pass by the sleep happening to finish.
            tokio::time::sleep(Duration::from_secs(3_600)).await;
            Ok(())
        }
    }

    struct Panics;

    #[async_trait]
    impl JobDispatcher for Panics {
        async fn dispatch(&self, _job: &ClaimedJob) -> Result<(), String> {
            panic!("a handler bug reached from a job body");
        }
    }

    struct Succeeds;

    #[async_trait]
    impl JobDispatcher for Succeeds {
        async fn dispatch(&self, _job: &ClaimedJob) -> Result<(), String> {
            Ok(())
        }
    }

    struct FailsWithItsOwnMessage;

    #[async_trait]
    impl JobDispatcher for FailsWithItsOwnMessage {
        async fn dispatch(&self, _job: &ClaimedJob) -> Result<(), String> {
            Err("the handler's own message".to_string())
        }
    }

    /// The bound that stops a wedged handler holding a claim past the point where
    /// another replica reclaims the row and runs the same job concurrently.
    #[tokio::test]
    async fn a_handler_that_outlives_its_budget_is_failed_rather_than_awaited() {
        let error = dispatch_guarded(
            &NeverReturns,
            &guarded_test_job(),
            Duration::from_millis(20),
        )
        .await
        .expect_err("a handler that never returns must not be reported as a success");
        assert!(
            error.contains("exceeded its"),
            "the recorded error must say the budget is what ended it: {error}"
        );
    }

    /// Before this, a panic here unwound through `run_once` and killed the whole
    /// supervisor task — no further polls, no retention sweep, no `leader.resign()`
    /// — while the pod stayed in service because `/health/ready` does not consult
    /// the supervisor.
    #[tokio::test]
    async fn a_panicking_handler_is_failed_instead_of_unwinding_the_supervisor() {
        let error = dispatch_guarded(&Panics, &guarded_test_job(), Duration::from_secs(30))
            .await
            .expect_err("a panicking handler must surface as a failed attempt");
        assert!(
            error.contains("panicked"),
            "the recorded error must name the panic: {error}"
        );
        // The panic payload is arbitrary handler text and this string is written to
        // `worker_jobs.last_error` unredacted, so it must not be carried through.
        assert!(
            !error.contains("a handler bug reached from a job body"),
            "the panic payload must not reach `worker_jobs.last_error`: {error}"
        );
    }

    /// The guard must be transparent to every handler that behaves, in both
    /// directions — a wrapper that swallowed an `Err` would complete failed jobs.
    #[tokio::test]
    async fn a_handler_inside_its_budget_passes_its_own_result_through() {
        assert!(
            dispatch_guarded(&Succeeds, &guarded_test_job(), Duration::from_secs(30))
                .await
                .is_ok()
        );
        assert_eq!(
            dispatch_guarded(
                &FailsWithItsOwnMessage,
                &guarded_test_job(),
                Duration::from_secs(30)
            )
            .await,
            Err("the handler's own message".to_string())
        );
    }

    /// The invariant the whole batch bound exists for: a poll must finish
    /// dispatching before its own claims become reclaimable.
    #[test]
    fn the_batch_dispatch_budget_stays_below_the_stale_claim_threshold() {
        let queue = |stale: u64| WorkerQueue {
            repository: Arc::new(NoRepository),
            settings: Arc::new(WorkerSettings {
                queue_stale_claim_seconds: stale,
                ..WorkerSettings::default()
            }),
            replica_id: Uuid::now_v7(),
        };
        for stale in [1_u64, 2, 5, 60, 300, 3_600, 86_400] {
            let budget = queue(stale).dispatch_budget().as_secs();
            assert!(
                budget < stale.max(2),
                "a {stale}s stale-claim threshold produced a {budget}s dispatch budget, which \
                 does not leave the poll time to settle inside its own claim"
            );
            assert!(budget >= 1, "the budget must never round down to zero");
        }
        // A misconfigured zero must not produce an unbounded (or zero) budget.
        assert_eq!(queue(0).dispatch_budget().as_secs(), 1);
    }

    /// The per-job ceiling is independent of the batch bound and must also survive
    /// a zero from a `WorkerSettings` built without `Settings::validate`.
    #[test]
    fn the_per_job_timeout_is_never_zero() {
        let queue = |timeout: u64| WorkerQueue {
            repository: Arc::new(NoRepository),
            settings: Arc::new(WorkerSettings {
                queue_job_timeout_seconds: timeout,
                ..WorkerSettings::default()
            }),
            replica_id: Uuid::now_v7(),
        };
        assert_eq!(queue(0).job_timeout().as_secs(), 1);
        assert_eq!(queue(45).job_timeout().as_secs(), 45);
    }

    /// A repository the budget arithmetic never reaches. `todo!()` rather than a
    /// working fake so a test that starts touching the database here fails loudly
    /// instead of silently exercising an unrealistic in-memory queue.
    struct NoRepository;

    #[async_trait]
    impl WorkerJobRepository for NoRepository {
        async fn enqueue(
            &self,
            _job: WorkerJobInsert,
            _max_pending: i64,
        ) -> Result<Option<Uuid>, AppError> {
            todo!("the dispatch-budget tests never enqueue")
        }
        async fn enqueue_if_idle(
            &self,
            _job: WorkerJobInsert,
            _max_pending: i64,
        ) -> Result<Option<Uuid>, AppError> {
            todo!("the dispatch-budget tests never enqueue")
        }
        async fn claim_batch(
            &self,
            _replica_id: Uuid,
            _limit: i64,
        ) -> Result<Vec<ClaimedJob>, AppError> {
            todo!("the dispatch-budget tests never claim")
        }
        async fn complete(&self, _id: Uuid) -> Result<(), AppError> {
            todo!("the dispatch-budget tests never settle")
        }
        async fn reschedule(
            &self,
            _id: Uuid,
            _run_at: DateTime<Utc>,
            _error: &str,
        ) -> Result<(), AppError> {
            todo!("the dispatch-budget tests never settle")
        }
        async fn dead_letter(&self, _id: Uuid, _error: &str) -> Result<(), AppError> {
            todo!("the dispatch-budget tests never settle")
        }
        async fn requeue_stale(&self, _stale_after_seconds: i64) -> Result<u64, AppError> {
            todo!("the dispatch-budget tests never reclaim")
        }
        async fn prune_terminal(&self, _retention_hours: i32) -> Result<u64, AppError> {
            todo!("the dispatch-budget tests never prune")
        }
        async fn pending_depth(&self) -> Result<i64, AppError> {
            todo!("the dispatch-budget tests never read the queue depth")
        }
    }
}
