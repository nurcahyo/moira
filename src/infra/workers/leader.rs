//! Leader election for singleton workers (plan 10 wave 1, finding P3-4).
//!
//! Exactly one replica should run a singleton maintenance job. Today there is
//! exactly one such job — the retention sweep (`super::RETENTION_CLEANUP_WORKER`)
//! — and until this module every replica with workers enabled ran its own,
//! which is correct under concurrency but does up to N times the scanning work.
//!
//! # Postgres advisory lock, not Redis
//!
//! Per plan 10's Open Decision 1: leadership flapping is more costly than a lossy
//! rate-limit counter, the pattern is already proven in this tree, and it needs no
//! infrastructure that a Moira deployment does not already have.
//!
//! # The lock is keyed by a table, never by a hash of the job name
//!
//! `pg_try_advisory_lock(hash(job_name))` produces an opaque integer that can
//! collide with any other advisory-lock key in the process — silently, and with
//! a deadlock rather than an error as the symptom. [`LEADER_LOCK_KEYS`] is a
//! closed table of ASCII keys instead, so `grep -r 'b"moira'` finds every key in
//! the tree, and a job name with no entry is a hard error rather than a fresh
//! hash nobody has checked.

use sqlx::{Connection, PgPool};
use tracing::{debug, warn};

use crate::error::AppError;

/// Leader lock for [`super::RETENTION_CLEANUP_WORKER`].
///
/// `moiralrt` = moira / leader / retention. Checked against every other key in
/// the tree by `the_leader_lock_key_collides_with_no_existing_key` below, which
/// includes `b"MOIRARET"` — the *test-only* guard in `tests/retention_worker.rs`.
/// That one is taken against the same database as everything else, so it is a
/// live collision risk for this module's own tests even though it never runs in
/// production.
pub const RETENTION_CLEANUP_LEADER_LOCK_KEY: i64 = i64::from_be_bytes(*b"moiralrt");

/// Job name to advisory-lock key. Closed set, by construction.
const LEADER_LOCK_KEYS: &[(&str, i64)] = &[(
    // Referenced, never re-typed: the spec table in `super` stays the single
    // source of truth for the name, so a rename cannot leave this table pointing
    // at a job that no longer exists.
    super::RETENTION_CLEANUP_WORKER,
    RETENTION_CLEANUP_LEADER_LOCK_KEY,
)];

/// The advisory-lock key for a singleton job, or `None` if the job has no
/// leader lock declared.
pub fn leader_lock_key(job_name: &str) -> Option<i64> {
    LEADER_LOCK_KEYS
        .iter()
        .find(|(name, _)| *name == job_name)
        .map(|(_, key)| *key)
}

/// Whether this process is currently entitled to run a given singleton job.
///
/// Extracted from the lock I/O so the transitions are unit-testable with no
/// database: every arm below is reachable from a `bool` the caller already has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaderState {
    /// Election is off. **Every replica runs the job** — this is the
    /// pre-plan-10 behaviour, preserved exactly, and it is what a single-replica
    /// deployment gets by default.
    Disabled,
    /// Election is on and another replica holds the lock.
    Follower,
    /// Election is on and this replica holds the lock.
    Leader,
}

impl LeaderState {
    /// Whether the singleton job body should run this tick.
    pub fn runs_singleton_job(self) -> bool {
        matches!(self, Self::Disabled | Self::Leader)
    }

    /// Folds the outcome of an acquisition attempt.
    ///
    /// `Disabled` is absorbing: a process that was never electing does not
    /// become a follower because an attempt it never made did not succeed.
    pub fn after_acquire_attempt(self, acquired: bool) -> Self {
        match self {
            Self::Disabled => Self::Disabled,
            _ if acquired => Self::Leader,
            _ => Self::Follower,
        }
    }

    /// Folds the outcome of a liveness check on a held lock.
    ///
    /// A session-scoped advisory lock dies with its session. If the session is
    /// gone the lock is *already* released and another replica may hold it, so
    /// continuing to believe otherwise is how two leaders happen.
    pub fn after_liveness_check(self, alive: bool) -> Self {
        match self {
            Self::Leader if !alive => Self::Follower,
            other => other,
        }
    }
}

/// A held session-scoped advisory lock.
///
/// # Why the connection is not a pooled one
///
/// `pg_try_advisory_lock` takes a **session**-level lock: it is released by the
/// session that took it, or when that session ends. A pooled connection is
/// returned to the pool still holding the lock, where a later checkout silently
/// inherits it — and every subsequent `pg_try_advisory_lock` on that connection
/// succeeds re-entrantly, so a follower that happens to draw the same connection
/// concludes it is the leader.
///
/// Binding the lock to a connection the pool has given up instead ties its
/// lifetime to a socket: dropping the guard closes the socket, PostgreSQL reaps
/// the backend, and the lock is released — including while unwinding from a
/// panic, which is the case that matters, since it is precisely when no explicit
/// release can run.
///
/// [`sqlx::pool::PoolConnection::detach`] is what makes it a dedicated
/// connection without threading a database URL through the worker layer: the
/// returned `PgConnection` is removed from the pool permanently (the pool opens a
/// replacement), so it can never be handed to anyone else.
///
/// The two worked examples of this idiom in the tree —
/// `SetupStateLock` (`src/application/identity.rs`) and `IssuerlessSlotLock`
/// (`src/infra/repositories/auth_settings.rs`) — are both in `#[cfg(test)]`
/// modules. This is the first production use.
pub struct LeaderLock {
    session: sqlx::PgConnection,
    job_name: String,
    key: i64,
}

/// Hand-written so the guard is printable without printing a connection.
///
/// The connection is the one field that must never appear: it carries the
/// database URL, and this type is exactly the sort of thing a test or a panic
/// message formats.
impl std::fmt::Debug for LeaderLock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LeaderLock")
            .field("job_name", &self.job_name)
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

impl LeaderLock {
    /// Takes the lock if it is free. `Ok(None)` means another replica holds it,
    /// which is a normal outcome and not an error.
    pub async fn try_acquire(pool: &PgPool, job_name: &str) -> Result<Option<Self>, AppError> {
        let key = leader_lock_key(job_name).ok_or_else(|| {
            // Not a `warn!` and a `false`: a job name with no declared key means
            // the caller believes a job is leader-gated and it is not, so the
            // job would silently run on every replica.
            AppError::Internal(format!(
                "no leader lock key is declared for worker {job_name:?}; add one to \
                 LEADER_LOCK_KEYS in src/infra/workers/leader.rs"
            ))
        })?;

        let mut session = pool.acquire().await?.detach();
        let acquired: bool = sqlx::query_scalar("select pg_try_advisory_lock($1)")
            .bind(key)
            .fetch_one(&mut session)
            .await?;

        if !acquired {
            // Closed rather than dropped so the backend goes away promptly
            // instead of at the next GC of the connection.
            let _ = session.close().await;
            return Ok(None);
        }

        Ok(Some(Self {
            session,
            job_name: job_name.to_string(),
            key,
        }))
    }

    pub fn job_name(&self) -> &str {
        &self.job_name
    }

    /// Whether the lock's session is still usable.
    ///
    /// Cheap, and checked every tick: a lock whose session died is a lock this
    /// process no longer holds, and the only way to notice is to touch the
    /// connection.
    pub async fn is_alive(&mut self) -> bool {
        self.session.ping().await.is_ok()
    }

    /// Releases the lock and closes the session.
    ///
    /// Dropping the guard would also release it — the session closes and
    /// PostgreSQL reaps the backend — but an explicit unlock hands leadership
    /// over in milliseconds instead of at the mercy of the server noticing a
    /// closed socket, which is what makes handover on a rolling update fast.
    pub async fn release(mut self) {
        if let Err(error) = sqlx::query("select pg_advisory_unlock($1)")
            .bind(self.key)
            .execute(&mut self.session)
            .await
        {
            // Not fatal: closing the session below releases the lock anyway.
            debug!(%error, job_name = %self.job_name, "leader lock unlock failed; the session close releases it");
        }
        let _ = self.session.close().await;
    }
}

/// The election, as the supervisor drives it.
///
/// Holds the lock across ticks — leadership is sticky, so a leader does not
/// hand the job back and forth with a follower on every cadence — and
/// re-verifies it each tick, so a dead session demotes this replica instead of
/// producing a second leader.
pub struct LeaderElection {
    job_name: &'static str,
    state: LeaderState,
    lock: Option<LeaderLock>,
}

impl LeaderElection {
    pub fn new(job_name: &'static str, enabled: bool) -> Self {
        Self {
            job_name,
            state: if enabled {
                LeaderState::Follower
            } else {
                LeaderState::Disabled
            },
            lock: None,
        }
    }

    pub fn state(&self) -> LeaderState {
        self.state
    }

    /// Whether this replica should run the singleton job on this tick.
    ///
    /// With election disabled this is unconditionally `true` and no database
    /// round trip happens at all, which is what keeps the default path identical
    /// to pre-plan-10 behaviour.
    ///
    /// A failed acquisition attempt returns `false`. That is deliberate: if the
    /// database cannot answer "may I lead", the job body — which is itself a
    /// database sweep — has nothing to do anyway, and guessing `true` under an
    /// unreachable database is how a partition produces N leaders.
    pub async fn should_run(&mut self, pool: Option<&PgPool>) -> bool {
        if matches!(self.state, LeaderState::Disabled) {
            return true;
        }
        let Some(pool) = pool else {
            // No database means no lock to take and no sweep to run; the caller
            // logs and skips.
            self.state = LeaderState::Follower;
            return false;
        };

        if let Some(lock) = self.lock.as_mut() {
            let alive = lock.is_alive().await;
            self.state = self.state.after_liveness_check(alive);
            if alive {
                return true;
            }
            warn!(
                job_name = self.job_name,
                "leader lock session died; standing down and re-contesting"
            );
            self.lock = None;
        }

        match LeaderLock::try_acquire(pool, self.job_name).await {
            Ok(Some(lock)) => {
                self.lock = Some(lock);
                self.state = self.state.after_acquire_attempt(true);
                debug!(job_name = self.job_name, "acquired worker leadership");
                true
            }
            Ok(None) => {
                self.state = self.state.after_acquire_attempt(false);
                false
            }
            Err(error) => {
                warn!(
                    %error,
                    job_name = self.job_name,
                    "leader lock acquisition failed; skipping this tick"
                );
                self.state = self.state.after_acquire_attempt(false);
                false
            }
        }
    }

    /// Releases leadership, if held. Called on supervisor shutdown so a rolling
    /// update hands the job over immediately.
    pub async fn resign(&mut self) {
        if let Some(lock) = self.lock.take() {
            lock.release().await;
        }
        if !matches!(self.state, LeaderState::Disabled) {
            self.state = LeaderState::Follower;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leader_state_follower_to_leader_on_successful_acquire() {
        assert_eq!(
            LeaderState::Follower.after_acquire_attempt(true),
            LeaderState::Leader
        );
    }

    #[test]
    fn leader_state_leader_to_follower_on_renewal_failure() {
        assert_eq!(
            LeaderState::Leader.after_liveness_check(false),
            LeaderState::Follower
        );
    }

    #[test]
    fn leader_state_renewal_within_ttl_keeps_leadership() {
        assert_eq!(
            LeaderState::Leader.after_liveness_check(true),
            LeaderState::Leader
        );
    }

    #[test]
    fn leader_state_failed_acquire_stays_follower() {
        assert_eq!(
            LeaderState::Follower.after_acquire_attempt(false),
            LeaderState::Follower
        );
    }

    /// Disabled must be absorbing in both directions, or turning election off
    /// would still be able to demote a replica out of running the job — which is
    /// the whole of the default path.
    #[test]
    fn disabled_election_never_becomes_follower() {
        assert_eq!(
            LeaderState::Disabled.after_acquire_attempt(false),
            LeaderState::Disabled
        );
        assert_eq!(
            LeaderState::Disabled.after_liveness_check(false),
            LeaderState::Disabled
        );
        assert!(LeaderState::Disabled.runs_singleton_job());
    }

    #[test]
    fn only_a_leader_or_a_disabled_election_runs_the_singleton_job() {
        assert!(LeaderState::Leader.runs_singleton_job());
        assert!(!LeaderState::Follower.runs_singleton_job());
    }

    #[test]
    fn the_retention_worker_has_a_declared_leader_lock_key() {
        assert_eq!(
            leader_lock_key(super::super::RETENTION_CLEANUP_WORKER),
            Some(RETENTION_CLEANUP_LEADER_LOCK_KEY)
        );
    }

    /// An undeclared job name must not silently fall back to "no gate".
    #[test]
    fn an_undeclared_job_has_no_leader_lock_key() {
        assert_eq!(leader_lock_key("memory-extraction-retry"), None);
        assert_eq!(leader_lock_key(""), None);
    }

    #[test]
    fn the_leader_lock_key_is_greppable_ascii() {
        assert_eq!(
            RETENTION_CLEANUP_LEADER_LOCK_KEY.to_be_bytes(),
            *b"moiralrt"
        );
    }

    /// `b"MOIRARET"` is test-only, but it is taken against the same database as
    /// everything else, so a collision with it would deadlock this module's own
    /// concurrency test against `tests/retention_worker.rs`.
    #[test]
    fn the_leader_lock_key_collides_with_no_existing_key() {
        for taken in [
            i64::from_be_bytes(*b"moirastp"),
            i64::from_be_bytes(*b"moiraoid"),
            i64::from_be_bytes(*b"moiratdb"),
            i64::from_be_bytes(*b"MOIRARET"),
        ] {
            assert_ne!(RETENTION_CLEANUP_LEADER_LOCK_KEY, taken);
        }
    }

    /// Every declared key must be distinct, or two singleton jobs would elect one
    /// shared leader and one of them would never run.
    #[test]
    fn declared_leader_lock_keys_are_unique() {
        let mut seen = std::collections::BTreeSet::new();
        for (name, key) in LEADER_LOCK_KEYS {
            assert!(seen.insert(*key), "duplicate leader lock key for {name}");
        }
    }

    #[tokio::test]
    async fn a_disabled_election_never_touches_the_database() {
        let mut election = LeaderElection::new(super::super::RETENTION_CLEANUP_WORKER, false);
        assert_eq!(election.state(), LeaderState::Disabled);
        // `None` pool: a disabled election that consulted the database would
        // return `false` here and the retention sweep would stop running on
        // every single-replica deployment.
        assert!(election.should_run(None).await);
        assert_eq!(election.state(), LeaderState::Disabled);
    }

    #[tokio::test]
    async fn an_enabled_election_without_a_database_does_not_claim_leadership() {
        let mut election = LeaderElection::new(super::super::RETENTION_CLEANUP_WORKER, true);
        assert!(!election.should_run(None).await);
        assert_eq!(election.state(), LeaderState::Follower);
    }
}
