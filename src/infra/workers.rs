use std::{sync::Arc, time::Duration};

use serde::Serialize;
use tokio::{sync::watch, task::JoinHandle};
use tracing::{info, warn};

use crate::{app::AppState, config::WorkerSettings};

#[derive(Debug, Clone)]
pub struct WorkerRegistry {
    settings: Arc<WorkerSettings>,
    specs: Arc<Vec<WorkerSpec>>,
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
    pub fn new(settings: WorkerSettings) -> Self {
        Self {
            settings: Arc::new(settings),
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
                    name: "retention-cleanup",
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
                    configured: self.settings.enabled && spec.enabled_by_default,
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
        info!(
            max_concurrent_jobs = self.settings.max_concurrent_jobs,
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
            }
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
