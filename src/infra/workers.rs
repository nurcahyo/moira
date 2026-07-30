pub mod leader;
pub mod queue;
pub mod retention;

use std::{sync::Arc, time::Duration};

use serde::Serialize;
use tokio::{sync::watch, task::JoinHandle};
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::{app::AppState, config::WorkerSettings, infra::repositories::PgWorkerJobRepository};

/// Spec name of the retention/cleanup worker. Referenced rather than typed as a
/// literal at the call site so the spec table stays the single source of truth.
pub const RETENTION_CLEANUP_WORKER: &str = "retention-cleanup";

/// Every job name the queue and the metrics layer will ever see.
///
/// A closed set, and that is the point: `job_name` is a metric **label**, and
/// `src/infra/metrics.rs` seeds one zero-valued series per name at process start.
/// A name minted at runtime — from a payload, from a caller, from a typo — would
/// open the label set, which is the memory-exhaustion vector the metrics module's
/// header calls out. `worker_job_names_match_the_spec_table` pins this list
/// against [`WorkerRegistry::new`]'s specs so the two cannot drift.
pub const WORKER_JOB_NAMES: &[&str] = &[
    "memory-extraction-retry",
    "conversation-summarization-retry",
    "embedding-retry",
    "document-ingestion-retry",
    "oauth-token-refresh",
    "provider-health-check",
    RETENTION_CLEANUP_WORKER,
    "runtime-cache-warmer",
];

/// Jobs that are leader-gated, and therefore the only ones with a meaningful
/// `moira_worker_leader_held` gauge. Pinned against `leader::LEADER_LOCK_KEYS` by
/// `every_leader_gated_worker_has_a_lock_key`.
pub const LEADER_GATED_WORKERS: &[&str] = &[RETENTION_CLEANUP_WORKER];

/// Whether `name` is one of Moira's declared job names.
///
/// The single gate between an arbitrary string and a metric label.
pub fn is_known_job_name(name: &str) -> bool {
    WORKER_JOB_NAMES.contains(&name)
}

#[derive(Debug, Clone)]
pub struct WorkerRegistry {
    settings: Arc<WorkerSettings>,
    specs: Arc<Vec<WorkerSpec>>,
    /// Whether the supervisor takes a leader lock before a singleton job.
    ///
    /// Already resolved: `Settings::leader_election_enabled` folds
    /// `workers.leader_election_enabled`'s `None` against
    /// `cluster.admission_enabled`, so this is a plain `bool` and there is
    /// exactly one place that decides it.
    leader_election_enabled: bool,
}

#[derive(Debug, Clone)]
pub struct WorkerSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub enabled_by_default: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkerSnapshot {
    pub enabled: bool,
    pub max_concurrent_jobs: usize,
    pub shutdown_grace_seconds: u64,
    pub retry_base_delay_seconds: u64,
    pub retry_max_delay_seconds: u64,
    pub dead_letter_retention_hours: u64,
    pub workers: Vec<WorkerStatus>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkerStatus {
    pub name: &'static str,
    pub description: &'static str,
    pub configured: bool,
}

#[derive(Debug)]
pub struct WorkerSupervisor {
    shutdown: watch::Sender<bool>,
    handle: JoinHandle<()>,
}

impl WorkerRegistry {
    pub fn new(settings: WorkerSettings, leader_election_enabled: bool) -> Self {
        Self {
            settings: Arc::new(settings),
            leader_election_enabled,
            specs: Arc::new(vec![
                WorkerSpec {
                    name: "memory-extraction-retry",
                    description: "Retries failed memory extraction runs.",
                    enabled_by_default: true,
                },
                WorkerSpec {
                    name: "conversation-summarization-retry",
                    description: "Retries failed conversation summary jobs.",
                    enabled_by_default: true,
                },
                WorkerSpec {
                    name: "embedding-retry",
                    description: "Retries memory and RAG embedding jobs.",
                    enabled_by_default: true,
                },
                WorkerSpec {
                    name: "document-ingestion-retry",
                    description: "Retries failed RAG document ingestion jobs.",
                    enabled_by_default: true,
                },
                WorkerSpec {
                    name: "oauth-token-refresh",
                    description: "Refreshes OAuth credentials before expiration.",
                    enabled_by_default: false,
                },
                WorkerSpec {
                    name: "provider-health-check",
                    description: "Continuously records provider health windows.",
                    enabled_by_default: true,
                },
                WorkerSpec {
                    name: RETENTION_CLEANUP_WORKER,
                    description: "Expires responses, idempotency records, vectors, and tombstones.",
                    enabled_by_default: true,
                },
                WorkerSpec {
                    name: "runtime-cache-warmer",
                    description: "Warms frequently used runtime configuration.",
                    enabled_by_default: false,
                },
            ]),
        }
    }

    pub fn enabled(&self) -> bool {
        self.settings.enabled
    }

    /// Whether singleton jobs are leader-gated in this process.
    ///
    /// Note the interaction with [`Self::enabled`]: workers off means
    /// `spawn_supervisor` returns `None`, so there is no tick loop, no
    /// singleton job and therefore **nothing to elect a leader for** — leader
    /// election is dead whatever this returns. The shipped Helm chart sets
    /// `MOIRA_WORKERS__ENABLED: "false"`, which is why
    /// `charts/moira/templates/_helpers.tpl` refuses a multi-replica release
    /// with workers disabled rather than letting an operator deploy a cluster
    /// that elects a leader for no jobs.
    pub fn leader_election_enabled(&self) -> bool {
        self.leader_election_enabled
    }

    /// The single definition of "this worker will actually run": workers are on
    /// for the process **and** this spec is on by default. `snapshot()` and the
    /// supervisor's dispatch both go through here so a status report can never
    /// disagree with what the tick loop does.
    fn spec_configured(&self, spec: &WorkerSpec) -> bool {
        self.settings.enabled && spec.enabled_by_default
    }

    pub fn is_configured(&self, name: &str) -> bool {
        self.specs
            .iter()
            .find(|spec| spec.name == name)
            .is_some_and(|spec| self.spec_configured(spec))
    }

    pub fn snapshot(&self) -> WorkerSnapshot {
        WorkerSnapshot {
            enabled: self.settings.enabled,
            max_concurrent_jobs: self.settings.max_concurrent_jobs,
            shutdown_grace_seconds: self.settings.shutdown_grace_seconds,
            retry_base_delay_seconds: self.settings.retry_base_delay_seconds,
            retry_max_delay_seconds: self.settings.retry_max_delay_seconds,
            dead_letter_retention_hours: self.settings.dead_letter_retention_hours,
            workers: self
                .specs
                .iter()
                .map(|spec| WorkerStatus {
                    name: spec.name,
                    description: spec.description,
                    configured: self.spec_configured(spec),
                })
                .collect(),
        }
    }

    pub fn spawn_supervisor(&self, state: AppState) -> Option<WorkerSupervisor> {
        if !self.settings.enabled {
            return None;
        }
        let (shutdown, shutdown_rx) = watch::channel(false);
        let registry = self.clone();
        let handle = tokio::spawn(async move {
            registry.run_supervisor(state, shutdown_rx).await;
        });
        Some(WorkerSupervisor { shutdown, handle })
    }

    async fn run_supervisor(&self, state: AppState, mut shutdown: watch::Receiver<bool>) {
        let mut interval = tokio::time::interval(Duration::from_secs(
            self.settings.retry_base_delay_seconds.max(1),
        ));

        // A retention sweep is orders of magnitude more expensive than a base
        // tick, so it gets its own cadence rather than riding the tick interval.
        let retention_configured = self.is_configured(RETENTION_CLEANUP_WORKER);
        let retention_period =
            Duration::from_secs(retention::RetentionPlan::interval_seconds(&self.settings));
        let mut retention_interval = tokio::time::interval(retention_period);
        // `Delay` rather than the default `Burst`: if a sweep overruns its period
        // we want the next one deferred, not a backlog of ticks fired
        // back-to-back against a database that is evidently already busy.
        retention_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        // The retention sweep is Moira's only singleton job today, so it is the
        // only thing leadership gates. Constructed even when election is off:
        // the `Disabled` state short-circuits without a database round trip, so
        // there is no branch here that a default deployment pays for.
        let mut leader = leader::LeaderElection::new(
            RETENTION_CLEANUP_WORKER,
            self.leader_election_enabled && retention_configured,
        );
        // Reported from the first tick so the family is not merely seeded but
        // *correct* from process start: a gauge that only appears once leadership
        // changes hands would read as "no data" on the replica that never wins.
        state
            .metrics
            .record_worker_leadership(RETENTION_CLEANUP_WORKER, false);

        // The queue polls on its own cadence, independent of both the base tick
        // and the retention sweep. Every replica polls — the claim is safe under
        // any replica count by construction — so this arm is deliberately *not*
        // leader-gated.
        let queue = state.pool.as_ref().map(|pool| {
            queue::WorkerQueue::new(
                Arc::new(PgWorkerJobRepository::new(pool.clone())),
                self.settings.clone(),
                // Per-process and diagnostic only. Claim correctness comes from
                // `for update skip locked`, never from this id, which is why it
                // does not have to be the admission lease's `replica_id` — a
                // process with admission off has no lease and still needs to claim.
                Uuid::now_v7(),
            )
        });
        let dispatcher = queue::StubJobDispatcher;
        let mut queue_interval = tokio::time::interval(Duration::from_secs(
            self.settings.queue_poll_interval_seconds.max(1),
        ));
        // `Delay` for the same reason the retention sweep uses it: a poll that
        // overruns its period must defer the next one rather than build a backlog
        // of ticks against a database that is evidently already busy.
        queue_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        info!(
            max_concurrent_jobs = self.settings.max_concurrent_jobs,
            retention_configured,
            retention_interval_seconds = retention_period.as_secs(),
            retention_batch_size = self.settings.retention_batch_size,
            leader_election_enabled = self.leader_election_enabled,
            "moira worker supervisor started"
        );

        loop {
            tokio::select! {
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        info!("moira worker supervisor stopping");
                        break;
                    }
                }
                _ = interval.tick() => {
                    state.metrics.record_worker_tick();
                }
                _ = retention_interval.tick(), if retention_configured => {
                    let leads = leader.should_run(state.pool.as_ref()).await;
                    state
                        .metrics
                        .record_worker_leadership(RETENTION_CLEANUP_WORKER, leads);
                    if leads {
                        self.run_retention_cleanup(&state).await;
                    } else {
                        debug!(
                            job_name = RETENTION_CLEANUP_WORKER,
                            "retention sweep skipped: another replica holds leadership"
                        );
                    }
                }
                _ = queue_interval.tick(), if queue.is_some() => {
                    let Some(queue) = queue.as_ref() else { continue };
                    Self::run_queue_poll(queue, &dispatcher, &state).await;
                }
            }
        }

        // Before the supervisor's task ends, so a rolling update hands
        // leadership to the next replica in milliseconds rather than waiting for
        // PostgreSQL to notice a closed socket.
        leader.resign().await;
        state
            .metrics
            .record_worker_leadership(RETENTION_CLEANUP_WORKER, false);
    }

    /// One queue poll. Never propagates, for the same reason the retention sweep
    /// does not: a failing poll must not take the supervisor — and with it every
    /// other worker — down. The next tick retries, and a job left `running` by a
    /// failure here is reclaimed by the stale-claim sweep.
    async fn run_queue_poll(
        queue: &queue::WorkerQueue,
        dispatcher: &dyn queue::JobDispatcher,
        state: &AppState,
    ) {
        match queue.run_once(dispatcher, &state.metrics).await {
            Ok(outcome) if outcome.claimed > 0 || outcome.reclaimed > 0 => info!(
                reclaimed = outcome.reclaimed,
                claimed = outcome.claimed,
                completed = outcome.completed,
                rescheduled = outcome.rescheduled,
                dead_lettered = outcome.dead_lettered,
                pruned = outcome.pruned,
                "worker queue poll settled"
            ),
            Ok(_) => debug!("worker queue poll found nothing to claim"),
            // `AppError::Sqlx` renders as a constant string, so no database detail
            // leaks here.
            Err(error) => warn!(%error, "worker queue poll failed; retrying on the next tick"),
        }
    }

    /// One retention sweep. Never propagates: a failing sweep must not take the
    /// supervisor — and with it every other worker — down. The next tick retries,
    /// and the deletes are idempotent, so a lost sweep costs only latency.
    async fn run_retention_cleanup(&self, state: &AppState) {
        let Some(pool) = state.pool.as_ref() else {
            debug!("retention cleanup skipped: no database configured");
            return;
        };

        match retention::run_once(pool, &self.settings, &state.metrics).await {
            Ok(outcome) if outcome.total_deleted() > 0 => info!(
                idempotency_records_deleted = outcome.idempotency_records_deleted,
                responses_deleted = outcome.responses_deleted,
                batches_run = outcome.batches_run,
                hit_per_tick_cap = outcome.hit_per_tick_cap,
                "retention cleanup deleted expired rows"
            ),
            Ok(outcome) => debug!(
                batches_run = outcome.batches_run,
                "retention cleanup found nothing to delete"
            ),
            // The message is a sanitised `AppError` display; `AppError::Sqlx`
            // renders as a constant string, so no database detail leaks here.
            Err(error) => warn!(%error, "retention cleanup failed; retrying on the next tick"),
        }
    }
}

impl WorkerSupervisor {
    pub async fn shutdown(self) {
        if self.shutdown.send(true).is_err() {
            warn!("worker supervisor already stopped");
        }
        let _ = self.handle.await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> WorkerRegistry {
        WorkerRegistry::new(WorkerSettings::default(), false)
    }

    /// `WORKER_JOB_NAMES` is what `src/infra/metrics.rs` seeds one zero-valued
    /// series per entry from, and what `WorkerQueue::enqueue` validates against.
    /// A spec added without a matching entry would be enqueueable and invisible;
    /// an entry with no spec would seed a series for a job that cannot run.
    #[test]
    fn worker_job_names_match_the_spec_table() {
        let declared: std::collections::BTreeSet<&str> = WORKER_JOB_NAMES.iter().copied().collect();
        let specs: std::collections::BTreeSet<&str> =
            registry().specs.iter().map(|spec| spec.name).collect();
        assert_eq!(declared, specs);
        assert_eq!(
            declared.len(),
            WORKER_JOB_NAMES.len(),
            "a duplicated name would seed the same metric series twice"
        );
    }

    #[test]
    fn every_leader_gated_worker_has_a_lock_key() {
        for name in LEADER_GATED_WORKERS {
            assert!(
                leader::leader_lock_key(name).is_some(),
                "{name} is leader-gated but has no declared advisory-lock key, so it \
                 would silently run on every replica"
            );
            assert!(is_known_job_name(name));
        }
    }

    #[test]
    fn an_undeclared_job_name_is_rejected() {
        assert!(!is_known_job_name("memory-extraction"));
        assert!(!is_known_job_name(""));
        assert!(is_known_job_name(RETENTION_CLEANUP_WORKER));
    }
}
