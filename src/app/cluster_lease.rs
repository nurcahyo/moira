//! The cluster admission gate (plan 10 wave 1, finding P3-2).
//!
//! `src/infra/repositories/cluster.rs` owns the SQL. This module owns the
//! decision that follows from it: whether the process serves traffic at all, how
//! the lease is kept alive, and what `/health/ready` reports when it is lost.
//!
//! # Why the gate is a startup failure and not an HTTP error
//!
//! A denied replica has no business binding a listener. Exiting non-zero makes
//! the pod fail to become `Ready`, which is the Kubernetes-native way to cap
//! replica count no matter what asked for the scale-up — `kubectl scale`
//! included, which is exactly what walks past the Helm template guard.
//!
//! # Why `readyz` also reports it
//!
//! A replica can *lose* its lease mid-run: renewal fails long enough against a
//! reachable-but-contended database that another replica reclaims it. If it kept
//! serving traffic while outside the admission ceiling, P3-2 would be fixed only
//! at startup. [`ClusterLeaseStatus`] is what `src/http/health.rs` reads.

use std::{
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::Duration,
};

use sqlx::PgPool;
use tokio::{sync::watch, task::JoinHandle};
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::{
    config::ClusterSettings,
    error::AppError,
    infra::repositories::{
        ClusterLeaseOutcome, ClusterLeaseRepository, PgClusterLeaseRepository, is_undefined_table,
        pod_name,
    },
};

/// The error code `/health/ready` returns when this replica has lost its lease.
///
/// The catalog key is derived as `moira.error.{code}` (`src/error.rs`), so this
/// string and `src/i18n/catalog/errors.rs`'s `moira.error.cluster_lease_denied`
/// must stay byte-identical.
pub const CLUSTER_LEASE_DENIED_CODE: &str = "cluster_lease_denied";

const STATE_NOT_ENFORCED: u8 = 0;
const STATE_HELD: u8 = 1;
const STATE_LOST: u8 = 2;

/// What this replica's admission lease is doing, readable from the request path.
///
/// An `AtomicU8` rather than a lock: `/health/ready` reads it on every probe and
/// the heartbeat writes it every few seconds, so a contended mutex would be the
/// only thing either of them ever waited on.
#[derive(Clone, Debug)]
pub struct ClusterLeaseStatus {
    state: Arc<AtomicU8>,
}

impl Default for ClusterLeaseStatus {
    fn default() -> Self {
        Self::not_enforced()
    }
}

impl ClusterLeaseStatus {
    /// Admission is off, or there is no database. Readiness is unaffected —
    /// this is what every default deployment, CLI mode and test carries.
    pub fn not_enforced() -> Self {
        Self {
            state: Arc::new(AtomicU8::new(STATE_NOT_ENFORCED)),
        }
    }

    fn set(&self, value: u8) {
        self.state.store(value, Ordering::Release);
    }

    /// True only when admission is enforced *and* the lease has been lost. A
    /// replica with admission off is never "denied".
    pub fn is_denied(&self) -> bool {
        self.state.load(Ordering::Acquire) == STATE_LOST
    }

    /// The value `/health/ready` and `/health/live` report for the lease.
    pub fn label(&self) -> &'static str {
        match self.state.load(Ordering::Acquire) {
            STATE_HELD => "held",
            STATE_LOST => "lost",
            _ => "not_enforced",
        }
    }
}

/// A held admission lease, plus the task keeping it alive.
///
/// Dropping this **does not** release the lease — release is an `async` database
/// write and `Drop` cannot await. A dropped handle's lease is instead reclaimed
/// by heartbeat expiry, which is the same path a killed pod takes. Call
/// [`ClusterLeaseHandle::release`] on the graceful-shutdown path so a rolling
/// update does not have to wait out `lease_expiry_seconds`.
pub struct ClusterLeaseHandle {
    replica_id: Uuid,
    repository: PgClusterLeaseRepository,
    status: ClusterLeaseStatus,
    shutdown: watch::Sender<bool>,
    heartbeat: JoinHandle<()>,
}

/// Hand-written for the same reason as [`LeaderLock`](crate::infra::workers::leader::LeaderLock)'s:
/// the repository holds a `PgPool`, and a lease handle is precisely the sort of
/// value a failing test or a panic message formats.
impl std::fmt::Debug for ClusterLeaseHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClusterLeaseHandle")
            .field("replica_id", &self.replica_id)
            .field("status", &self.status.label())
            .finish_non_exhaustive()
    }
}

impl ClusterLeaseHandle {
    pub fn replica_id(&self) -> Uuid {
        self.replica_id
    }

    /// Stops the heartbeat and releases the lease.
    ///
    /// Release failure is logged, never propagated: a shutdown that fails
    /// because the lease could not be released would turn a clean exit into a
    /// non-zero one for a row that expires on its own within
    /// `lease_expiry_seconds` regardless.
    pub async fn release(self) {
        let _ = self.shutdown.send(true);
        self.heartbeat.abort();
        let _ = self.heartbeat.await;
        match self.repository.release(self.replica_id).await {
            Ok(()) => info!(replica_id = %self.replica_id, "cluster admission lease released"),
            Err(error) => warn!(
                %error,
                replica_id = %self.replica_id,
                "releasing the cluster admission lease failed; it will expire on its own"
            ),
        }
        self.status.set(STATE_NOT_ENFORCED);
    }
}

/// Acquires this process's admission lease, or refuses to start.
///
/// Returns `Ok(None)` — proceed without a lease — in exactly three cases, each
/// of which is a *deliberate* non-enforcement rather than a silent one:
///
/// * admission is disabled (the default);
/// * there is no database (`MOIRA_DATABASE__REQUIRE=false` development runs);
/// * `cluster_replica_leases` does not exist yet.
///
/// The last one is the operationally interesting one. The chart ships
/// `MOIRA_DATABASE__MIGRATE_ON_STARTUP: "false"` and applies migrations from a
/// separate Job, so a replica genuinely can start against a database where this
/// table has not been created. Treating SQLSTATE `42P01` as a denial would turn a
/// migration-ordering condition into an unexplained crash loop, which is strictly
/// worse than a loud warning: the operator would be debugging the wrong problem.
pub async fn acquire(
    pool: Option<&PgPool>,
    settings: &ClusterSettings,
    status: &ClusterLeaseStatus,
) -> Result<Option<ClusterLeaseHandle>, AppError> {
    if !settings.admission_enabled {
        return Ok(None);
    }
    let Some(pool) = pool else {
        warn!(
            "cluster.admission_enabled is set but no database is configured; the replica \
             ceiling is not enforced"
        );
        return Ok(None);
    };

    let repository = PgClusterLeaseRepository::new(pool.clone());
    let replica_id_seed = Uuid::now_v7();
    let pod = pod_name(replica_id_seed);

    let outcome = match repository
        .acquire(&pod, settings.max_replicas, settings.lease_expiry_seconds)
        .await
    {
        Ok(outcome) => outcome,
        Err(error) if is_undefined_table(&error) => {
            warn!(
                pod_name = %pod,
                "cluster_replica_leases does not exist yet, so the replica ceiling is not \
                 enforced for this process. Run the migration Job (or set \
                 MOIRA_DATABASE__MIGRATE_ON_STARTUP=true) and restart."
            );
            return Ok(None);
        }
        Err(error) => return Err(error),
    };

    match outcome {
        ClusterLeaseOutcome::Denied {
            live_leases,
            max_replicas,
        } => {
            // A structured field, not only prose, so an operator can tell this
            // apart from every other startup failure without parsing a message.
            error!(
                reason = CLUSTER_LEASE_DENIED_CODE,
                pod_name = %pod,
                live_leases,
                max_replicas,
                "cluster admission lease denied: the configured replica ceiling is already met"
            );
            Err(AppError::Config(format!(
                "cluster admission lease denied: {live_leases} of {max_replicas} leases are \
                 live. Raise cluster.max_replicas or reduce the replica count."
            )))
        }
        ClusterLeaseOutcome::Granted(grant) => {
            status.set(STATE_HELD);
            info!(
                replica_id = %grant.replica_id,
                pod_name = %grant.pod_name,
                max_replicas = settings.max_replicas,
                "cluster admission lease acquired"
            );
            let (shutdown, shutdown_rx) = watch::channel(false);
            let heartbeat = spawn_heartbeat(
                repository.clone(),
                grant.replica_id,
                status.clone(),
                Duration::from_secs(settings.lease_heartbeat_seconds.max(1)),
                shutdown_rx,
            );
            Ok(Some(ClusterLeaseHandle {
                replica_id: grant.replica_id,
                repository,
                status: status.clone(),
                shutdown,
                heartbeat,
            }))
        }
    }
}

/// Renews the lease until shutdown.
///
/// A renewal that returns `false` means another replica reclaimed the row — this
/// process is outside the ceiling and must stop reporting ready. It does **not**
/// exit: killing a process that is mid-request would turn a transient database
/// stall into dropped traffic, and failing readiness already takes the pod out of
/// the Service's endpoints, which is the outcome that matters. A renewal that
/// *errors* is a database problem, logged and retried — `readyz` is failing on
/// the `select 1` anyway.
fn spawn_heartbeat(
    repository: PgClusterLeaseRepository,
    replica_id: Uuid,
    status: ClusterLeaseStatus,
    period: Duration,
    mut shutdown: watch::Receiver<bool>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(period);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // The first tick fires immediately and would renew a lease acquired
        // microseconds ago.
        ticker.tick().await;
        loop {
            tokio::select! {
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        return;
                    }
                }
                _ = ticker.tick() => match repository.renew(replica_id).await {
                    Ok(true) => status.set(STATE_HELD),
                    Ok(false) => {
                        status.set(STATE_LOST);
                        error!(
                            reason = CLUSTER_LEASE_DENIED_CODE,
                            %replica_id,
                            "cluster admission lease lost; this replica is outside the ceiling \
                             and is reporting not-ready"
                        );
                    }
                    Err(error) => warn!(
                        %error,
                        %replica_id,
                        "renewing the cluster admission lease failed; retrying on the next beat"
                    ),
                },
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_default_status_is_not_enforced_and_never_denies_readiness() {
        let status = ClusterLeaseStatus::default();
        assert!(!status.is_denied());
        assert_eq!(status.label(), "not_enforced");
    }

    #[test]
    fn only_a_lost_lease_denies_readiness() {
        let status = ClusterLeaseStatus::not_enforced();
        status.set(STATE_HELD);
        assert!(!status.is_denied());
        assert_eq!(status.label(), "held");
        status.set(STATE_LOST);
        assert!(status.is_denied());
        assert_eq!(status.label(), "lost");
    }

    /// Clones share the cell, or the heartbeat would be updating a status the
    /// request path never reads.
    #[test]
    fn a_cloned_status_observes_the_same_state() {
        let status = ClusterLeaseStatus::not_enforced();
        let observer = status.clone();
        status.set(STATE_LOST);
        assert!(observer.is_denied());
    }

    /// The code and its catalog key must not drift: `AppError::message_key`
    /// derives one from the other.
    #[test]
    fn the_denied_code_has_a_catalog_entry() {
        assert!(crate::i18n::is_known_key(&format!(
            "moira.error.{CLUSTER_LEASE_DENIED_CODE}"
        )));
    }

    #[tokio::test]
    async fn admission_disabled_acquires_nothing() {
        let settings = ClusterSettings::default();
        assert!(!settings.admission_enabled, "the default must stay off");
        let status = ClusterLeaseStatus::not_enforced();
        let handle = acquire(None, &settings, &status)
            .await
            .expect("a disabled gate never fails");
        assert!(handle.is_none());
        assert_eq!(status.label(), "not_enforced");
    }

    /// Enabled but pool-less must not be a startup failure: a development run
    /// with `MOIRA_DATABASE__REQUIRE=false` has nothing to admit against.
    #[tokio::test]
    async fn admission_without_a_database_warns_rather_than_failing() {
        let settings = ClusterSettings {
            admission_enabled: true,
            ..ClusterSettings::default()
        };
        let status = ClusterLeaseStatus::not_enforced();
        let handle = acquire(None, &settings, &status)
            .await
            .expect("a pool-less gate must not fail startup");
        assert!(handle.is_none());
        assert!(!status.is_denied());
    }
}
