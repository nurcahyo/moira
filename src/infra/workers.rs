pub mod leader;
pub mod retention;

use std::{sync::Arc, time::Duration};

use serde::Serialize;
use tokio::{sync::watch, task::JoinHandle};
use tracing::{debug, info, warn};

use crate::{app::AppState, config::WorkerSettings};

/// Spec name of the retention/cleanup worker. Referenced rather than typed as a
/// literal at the call site so the spec table stays the single source of truth.
pub const RETENTION_CLEANUP_WORKER: &str = "retention-cleanup";

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
                    if leader.should_run(state.pool.as_ref()).await {
                        self.run_retention_cleanup(&state).await;
                    } else {
                        debug!(
                            job_name = RETENTION_CLEANUP_WORKER,
                            "retention sweep skipped: another replica holds leadership"
                        );
                    }
                }
            }
        }

        // Before the supervisor's task ends, so a rolling update hands
        // leadership to the next replica in milliseconds rather than waiting for
        // PostgreSQL to notice a closed socket.
        leader.resign().await;
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
