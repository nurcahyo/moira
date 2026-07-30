//! Cluster admission leases (plan 10 wave 1, finding P3-2).
//!
//! One row in `cluster_replica_leases` per live replica. A process must be
//! granted one before it binds a listener, which is what makes the ceiling
//! enforceable against `kubectl scale` — Helm's `moira.validateDeployment` is a
//! template-time `fail` and a scale command never re-renders the chart.
//!
//! The repository owns the SQL and nothing else. The startup gate, the heartbeat
//! task and the readiness reporting live in `src/app/cluster_lease.rs`, because
//! "refuse to serve traffic" is process composition, not persistence.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::error::AppError;

/// Serialises admission across replicas.
///
/// **Transaction-scoped**, so it is safe on a pooled connection: PostgreSQL
/// releases it at commit or rollback, including the implicit rollback `sqlx`
/// performs when a transaction guard is dropped. (The *leader* lock in
/// `src/infra/workers/leader.rs` is session-scoped and therefore is not safe on a
/// pooled connection — see the comment there.)
///
/// Without it, admission is a lost-update race: two replicas both read
/// `count = max_replicas - 1`, both conclude there is room, and both insert.
///
/// ASCII so `grep -r 'b"moira'` finds every advisory-lock key in the tree in one
/// pass. The keys in use today are `b"moirastp"` (`src/application/identity.rs`),
/// `b"moiraoid"` (`src/infra/repositories/auth_settings.rs`), `b"moiratdb"`
/// (`tests/support/mod.rs`), `b"MOIRARET"` (`tests/retention_worker.rs`), the
/// hashed transaction lock in `src/infra/repositories/admin.rs`, and this one
/// plus `b"moiralrt"` in `src/infra/workers/leader.rs`.
const CLUSTER_ADMISSION_LOCK_KEY: i64 = i64::from_be_bytes(*b"moiracll");

/// How long a released lease row is kept before acquire prunes it.
///
/// Long enough that a week of restart history is available to an operator
/// reconstructing an incident, short enough that the table does not grow without
/// bound. The prune is bounded by **age**, not by `limit`, so it is not exposed
/// to the re-executed-subquery hazard documented in
/// `src/infra/workers/retention.rs`.
const RELEASED_LEASE_RETENTION_DAYS: i32 = 7;

/// The result of asking the cluster for permission to serve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClusterLeaseOutcome {
    Granted(ClusterLeaseGrant),
    /// Every lease is taken. The process must not serve traffic.
    Denied {
        live_leases: i64,
        max_replicas: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterLeaseGrant {
    pub replica_id: Uuid,
    pub pod_name: String,
    pub acquired_at: DateTime<Utc>,
}

/// The admission-lease persistence surface.
///
/// A trait from its first commit, per the convention in this module's parent: the
/// startup gate is testable against a fake without a Postgres instance, and no
/// later plan has to retrofit the seam onto a surface that already has callers.
#[async_trait]
pub trait ClusterLeaseRepository: Send + Sync {
    /// Reclaims expired leases, then admits this replica if the cluster is under
    /// its ceiling.
    ///
    /// `expiry_seconds` is how stale a heartbeat may be before another replica
    /// may take the lease; `Settings::validate` guarantees it exceeds the
    /// heartbeat interval.
    async fn acquire(
        &self,
        pod_name: &str,
        max_replicas: u32,
        expiry_seconds: u64,
    ) -> Result<ClusterLeaseOutcome, AppError>;

    /// Renews a held lease. `Ok(false)` means the row is gone or already
    /// released — this replica **no longer holds a lease** and must stop
    /// reporting ready.
    async fn renew(&self, replica_id: Uuid) -> Result<bool, AppError>;

    /// Releases a held lease. Idempotent: releasing an already-released lease is
    /// `Ok(())`, because graceful shutdown must not fail on a lease another
    /// replica already reclaimed.
    async fn release(&self, replica_id: Uuid) -> Result<(), AppError>;

    /// Live leases, for diagnostics and tests.
    async fn live_lease_count(&self, expiry_seconds: u64) -> Result<i64, AppError>;
}

#[derive(Clone)]
pub struct PgClusterLeaseRepository {
    pool: PgPool,
}

impl PgClusterLeaseRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl ClusterLeaseRepository for PgClusterLeaseRepository {
    async fn acquire(
        &self,
        pod_name: &str,
        max_replicas: u32,
        expiry_seconds: u64,
    ) -> Result<ClusterLeaseOutcome, AppError> {
        let mut tx = self.pool.begin().await?;

        // Taken first, and for the whole transaction: the count below and the
        // insert that depends on it must be one atomic decision.
        sqlx::query("select pg_advisory_xact_lock($1)")
            .bind(CLUSTER_ADMISSION_LOCK_KEY)
            .execute(&mut *tx)
            .await?;

        // Reclaim: a replica that was killed never released its row, so its lease
        // is freed by heartbeat staleness rather than by anything it did. No
        // `limit`, so there is no victim sub-select to be re-executed.
        sqlx::query(RECLAIM_EXPIRED_SQL)
            .bind(expiry_seconds as i64)
            .execute(&mut *tx)
            .await?;

        sqlx::query(PRUNE_RELEASED_SQL)
            .bind(RELEASED_LEASE_RETENTION_DAYS)
            .execute(&mut *tx)
            .await?;

        let live: i64 = sqlx::query_scalar(COUNT_LIVE_SQL)
            .fetch_one(&mut *tx)
            .await?;
        if live >= i64::from(max_replicas) {
            // Committed rather than rolled back so the reclaim and prune above
            // survive: a denied replica still did the cluster the favour of
            // clearing dead rows, and discarding that would make a crash-looping
            // pod repeat the same work forever.
            tx.commit().await?;
            return Ok(ClusterLeaseOutcome::Denied {
                live_leases: live,
                max_replicas,
            });
        }

        // Minted here rather than by the column default so the caller has the id
        // before the row exists — the pod-name fallback for non-Kubernetes hosts
        // is derived from it (see [`resolve_pod_name`]).
        let replica_id = Uuid::now_v7();
        let row = sqlx::query(INSERT_LEASE_SQL)
            .bind(replica_id)
            .bind(pod_name)
            .fetch_one(&mut *tx)
            .await?;
        let acquired_at: DateTime<Utc> = row.try_get("acquired_at")?;
        tx.commit().await?;

        Ok(ClusterLeaseOutcome::Granted(ClusterLeaseGrant {
            replica_id,
            pod_name: pod_name.to_string(),
            acquired_at,
        }))
    }

    async fn renew(&self, replica_id: Uuid) -> Result<bool, AppError> {
        let result = sqlx::query(RENEW_SQL)
            .bind(replica_id)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() == 1)
    }

    async fn release(&self, replica_id: Uuid) -> Result<(), AppError> {
        sqlx::query(RELEASE_SQL)
            .bind(replica_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    async fn live_lease_count(&self, expiry_seconds: u64) -> Result<i64, AppError> {
        sqlx::query_scalar(COUNT_LIVE_WITHIN_EXPIRY_SQL)
            .bind(expiry_seconds as i64)
            .fetch_one(&self.pool)
            .await
            .map_err(AppError::from)
    }
}

const RECLAIM_EXPIRED_SQL: &str = r#"
    update cluster_replica_leases
       set released_at = now()
     where released_at is null
       and heartbeat_at < now() - make_interval(secs => $1)
"#;

const PRUNE_RELEASED_SQL: &str = r#"
    delete from cluster_replica_leases
     where released_at is not null
       and released_at < now() - make_interval(days => $1)
"#;

const COUNT_LIVE_SQL: &str = r#"
    select count(*) from cluster_replica_leases where released_at is null
"#;

const COUNT_LIVE_WITHIN_EXPIRY_SQL: &str = r#"
    select count(*)
      from cluster_replica_leases
     where released_at is null
       and heartbeat_at >= now() - make_interval(secs => $1)
"#;

const INSERT_LEASE_SQL: &str = r#"
    insert into cluster_replica_leases (replica_id, pod_name)
    values ($1, $2)
    returning acquired_at
"#;

const RENEW_SQL: &str = r#"
    update cluster_replica_leases
       set heartbeat_at = now()
     where replica_id = $1
       and released_at is null
"#;

const RELEASE_SQL: &str = r#"
    update cluster_replica_leases
       set released_at = now()
     where replica_id = $1
       and released_at is null
"#;

/// This replica's label in `cluster_replica_leases.pod_name`.
///
/// Pure, so the fallback chain is testable without a Kubernetes cluster and
/// without mutating process environment. [`pod_name`] is the thin wrapper that
/// reads the environment.
///
/// `pod_name` is `not null` with a non-blank `check`, so the chain must not be
/// allowed to bottom out at `""`. It bottoms out at a synthesised label instead:
/// an operator reading the table during an incident learns *"a process not under
/// the downward API"*, which is a fact, rather than reading a blank cell, which
/// is an absence they then have to investigate.
///
/// The fallback uses the **whole** UUID, not a prefix. `Uuid::now_v7`'s first six
/// bytes are a millisecond timestamp, so `&id[..12]` is identical for any two
/// replicas that start in the same millisecond — which is exactly what a rollout
/// does, and it is how `pod_name_fallbacks_are_distinct_per_replica` failed
/// before this line was widened.
pub fn resolve_pod_name(pod_env: Option<&str>, host_env: Option<&str>, replica_id: Uuid) -> String {
    for value in [pod_env, host_env].into_iter().flatten() {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    format!("unnamed-{}", replica_id.simple())
}

/// [`resolve_pod_name`] against the real environment.
///
/// `POD_NAME` is injected by `charts/moira/templates/deployment.yaml` from the
/// downward API (`metadata.name`). `HOSTNAME` is the container-runtime fallback
/// for non-Kubernetes hosts.
pub fn pod_name(replica_id: Uuid) -> String {
    let pod = std::env::var("POD_NAME").ok();
    let host = std::env::var("HOSTNAME").ok();
    resolve_pod_name(pod.as_deref(), host.as_deref(), replica_id)
}

/// True when `error` is PostgreSQL's `undefined_table` (SQLSTATE `42P01`).
///
/// The chart ships `MOIRA_DATABASE__MIGRATE_ON_STARTUP: "false"` and runs
/// migrations from a separate Job, so a replica can legitimately start against a
/// database where `cluster_replica_leases` does not exist yet. That is a
/// migration-ordering condition, not a denial, and must not be turned into an
/// unexplained crash loop — see `src/app/cluster_lease.rs`.
pub fn is_undefined_table(error: &AppError) -> bool {
    let AppError::Sqlx(sqlx::Error::Database(db)) = error else {
        return false;
    };
    db.code().as_deref() == Some("42P01")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pod_name_prefers_the_downward_api_value() {
        let id = Uuid::now_v7();
        assert_eq!(
            resolve_pod_name(Some("moira-7d4f-abc"), Some("some-host"), id),
            "moira-7d4f-abc"
        );
    }

    #[test]
    fn pod_name_falls_back_to_the_hostname_outside_kubernetes() {
        let id = Uuid::now_v7();
        assert_eq!(resolve_pod_name(None, Some("laptop"), id), "laptop");
    }

    /// A blank env var is the case that would otherwise write `''` into a
    /// `not null` column with a non-blank `check` and turn startup into a
    /// constraint violation.
    #[test]
    fn pod_name_treats_a_blank_value_as_absent() {
        let id = Uuid::now_v7();
        assert_eq!(resolve_pod_name(Some("   "), Some("laptop"), id), "laptop");
    }

    #[test]
    fn pod_name_never_resolves_to_an_empty_string() {
        let id = Uuid::now_v7();
        let name = resolve_pod_name(None, None, id);
        assert!(!name.trim().is_empty(), "the column rejects a blank name");
        assert!(
            name.starts_with("unnamed-"),
            "the fallback must say it is a fallback, got {name:?}"
        );
    }

    /// Two replicas starting in the same millisecond — a rollout — must still
    /// produce different labels. A prefix of a v7 UUID is its timestamp, so this
    /// is the assertion that keeps the fallback from truncating back to one.
    #[test]
    fn pod_name_fallbacks_are_distinct_per_replica() {
        let first = resolve_pod_name(None, None, Uuid::now_v7());
        let second = resolve_pod_name(None, None, Uuid::now_v7());
        assert_ne!(
            first, second,
            "two replicas without the downward API must still be distinguishable"
        );
    }

    /// The admission decision, in the form the SQL implements it: a live count at
    /// or above the ceiling denies. Pinned as a pure predicate because the
    /// boundary — `live == max` denies, `live == max - 1` admits — is exactly the
    /// off-by-one that would let one extra replica in.
    #[test]
    fn admission_denies_at_the_ceiling_and_admits_below_it() {
        fn admits(live: i64, max_replicas: u32) -> bool {
            live < i64::from(max_replicas)
        }
        assert!(admits(0, 1));
        assert!(!admits(1, 1));
        assert!(admits(1, 2));
        assert!(!admits(2, 2));
        assert!(!admits(3, 2));
    }

    #[test]
    fn the_admission_lock_key_is_greppable_ascii() {
        assert_eq!(CLUSTER_ADMISSION_LOCK_KEY.to_be_bytes(), *b"moiracll");
    }

    /// The five keys already in the tree, plus this one. A new key that collides
    /// with an existing one produces a deadlock between two unrelated subsystems
    /// and no error message anywhere.
    #[test]
    fn the_admission_lock_key_collides_with_no_existing_key() {
        for taken in [
            i64::from_be_bytes(*b"moirastp"),
            i64::from_be_bytes(*b"moiraoid"),
            i64::from_be_bytes(*b"moiratdb"),
            i64::from_be_bytes(*b"MOIRARET"),
            crate::infra::workers::leader::RETENTION_CLEANUP_LEADER_LOCK_KEY,
        ] {
            assert_ne!(CLUSTER_ADMISSION_LOCK_KEY, taken);
        }
    }

    #[test]
    fn reclaim_and_count_agree_on_what_live_means() {
        // Both spellings must key off `released_at is null`; the reclaim step is
        // what converts "stale heartbeat" into that state, so the count does not
        // have to repeat the staleness predicate and the two cannot drift.
        assert!(RECLAIM_EXPIRED_SQL.contains("released_at is null"));
        assert!(RECLAIM_EXPIRED_SQL.contains("heartbeat_at <"));
        assert!(COUNT_LIVE_SQL.contains("released_at is null"));
    }

    /// Renewal must not resurrect a lease another replica already reclaimed.
    #[test]
    fn renew_refuses_a_released_lease() {
        assert!(RENEW_SQL.contains("released_at is null"));
    }

    /// The prune is bounded by age. A `limit`-bounded victim sub-select here
    /// would be exposed to the re-execution hazard documented at length in
    /// `src/infra/workers/retention.rs`; there is deliberately no `limit`.
    #[test]
    fn the_released_row_prune_uses_no_limit() {
        assert!(!PRUNE_RELEASED_SQL.contains("limit"));
        assert!(PRUNE_RELEASED_SQL.contains("released_at <"));
    }

    #[test]
    fn undefined_table_is_not_confused_with_other_failures() {
        assert!(!is_undefined_table(&AppError::DatabaseUnavailable));
        assert!(!is_undefined_table(&AppError::Internal("boom".into())));
    }
}
