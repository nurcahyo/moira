//! Issue #77 problem 2 — the leaked-database sweep must not destroy a neighbouring
//! worktree's template.
//!
//! # The bug this pins
//!
//! `support::sweep_leaked_databases` used to drop **every** `moira_test_template_*` that
//! was not the calling process's own. The template is named after a SHA-256 of the
//! migration set, so two worktrees whose `migrations/` differ by a single file have
//! different template names and each one's sweep destroyed the other's — mid-run, because
//! nothing holds a template alive between `prepare_template` and the last fixture. The
//! victim then failed with
//!
//! ```text
//! clone the migrated template database: … code: "3D000",
//! message: "template database \"moira_test_template_…\" does not exist"
//! ```
//!
//! which is indistinguishable from a real regression until someone reads the panic.
//! `plans/reports/EXECUTION-LEDGER.md` records it happening twice on one branch with no
//! code change between the runs, and names the cure: the sweep needs an age or an
//! owning-run marker before it may drop a template it does not recognise, because "no
//! client backend" cannot tell an idle neighbour from a leak.
//!
//! # What is asserted here, and why both halves
//!
//! An implementation that spares every template it does not own would close the flake and
//! open a disk leak — ~10 MB per abandoned migration set, forever. So this suite asserts
//! **both** directions against a real cluster:
//!
//! * a template claimed within the grace period survives a sweep (the neighbour);
//! * a template whose claim has expired is reclaimed (the genuine leak);
//! * an unmarked template survives *and comes back marked*, so a template that predates
//!   the marker becomes reclaimable one grace period later rather than never;
//! * an aged, connectionless fixture database is still reclaimed, unchanged.
//!
//! # Why the exclusive lock is held around each case
//!
//! These cases fabricate databases that match the sweep's own patterns. A neighbouring
//! worktree running the *old* code would drop them out from under the assertions. Taking
//! `support::TEMPLATE_LOCK_KEY` exclusively is not a workaround for that: it is exactly
//! the lock `prepare_template` holds while sweeping, so holding it here blocks any
//! neighbour's sweep for the length of the case, and running the sweep under it reproduces
//! the production call shape rather than a convenient one.

mod support;

use sqlx::{Connection, PgConnection};
use support::{
    TEMPLATE_IDLE_GRACE_SECONDS, TEMPLATE_LOCK_KEY, TEMPLATE_PREFIX, TemplateVerdict,
    now_epoch_seconds, sweep_leaked_databases, template_marker, template_sweep_verdict,
};
use uuid::Uuid;

/// Holds the exclusive template-maintenance lock and cleans up the databases a case
/// fabricated, on the panic path as well as the happy one.
struct SweepHarness {
    maintenance: PgConnection,
    current_template: String,
    fabricated: Vec<String>,
}

impl SweepHarness {
    /// `None` only when the deliberate no-database opt-out is in force.
    async fn open() -> Option<Self> {
        let (mut maintenance, current_template) = support::maintenance_connection().await?;
        sqlx::query("select pg_advisory_lock($1)")
            .bind(TEMPLATE_LOCK_KEY)
            .execute(&mut maintenance)
            .await
            .expect("take the exclusive template maintenance lock");
        Some(Self {
            maintenance,
            current_template,
            fabricated: Vec::new(),
        })
    }

    /// A database name of the given shape that no other worktree can be using.
    fn unique(kind: &str) -> String {
        format!("{kind}{}", Uuid::now_v7().simple())
    }

    async fn create(&mut self, name: &str, comment: Option<&str>) {
        sqlx::raw_sql(&format!("drop database if exists \"{name}\" with (force)"))
            .execute(&mut self.maintenance)
            .await
            .expect("clear a previous fabricated database");
        sqlx::raw_sql(&format!("create database \"{name}\""))
            .execute(&mut self.maintenance)
            .await
            .expect("fabricate a database for the sweep");
        self.fabricated.push(name.to_string());
        if let Some(comment) = comment {
            sqlx::raw_sql(&format!("comment on database \"{name}\" is '{comment}'"))
                .execute(&mut self.maintenance)
                .await
                .expect("stamp the fabricated database");
        }
    }

    async fn sweep(&mut self) {
        sweep_leaked_databases(&mut self.maintenance, &self.current_template).await;
    }

    async fn exists(&mut self, name: &str) -> bool {
        sqlx::query_scalar::<_, i32>("select 1 from pg_database where datname = $1")
            .bind(name)
            .fetch_optional(&mut self.maintenance)
            .await
            .expect("look up a fabricated database")
            .is_some()
    }

    /// `None` both when the database carries no comment and when it is no longer there —
    /// the caller asserts on existence first, and a `RowNotFound` raised here would
    /// replace that assertion's message with a meaningless one.
    async fn comment_on(&mut self, name: &str) -> Option<String> {
        sqlx::query_scalar::<_, Option<String>>(
            "select shobj_description(oid, 'pg_database') from pg_database where datname = $1",
        )
        .bind(name)
        .fetch_optional(&mut self.maintenance)
        .await
        .expect("read a fabricated database's marker")
        .flatten()
    }

    async fn close(mut self) {
        for name in std::mem::take(&mut self.fabricated) {
            let _ = sqlx::raw_sql(&format!("drop database if exists \"{name}\" with (force)"))
                .execute(&mut self.maintenance)
                .await;
        }
        let _ = sqlx::query("select pg_advisory_unlock($1)")
            .bind(TEMPLATE_LOCK_KEY)
            .execute(&mut self.maintenance)
            .await;
        let _ = self.maintenance.close().await;
    }
}

/// The neighbour case, end to end: a template belonging to a *different* migration set,
/// claimed a minute ago, is still there after this process sweeps.
///
/// This is the exact condition that produced the `3D000`: same cluster, two templates,
/// only one of which this process recognises.
#[tokio::test]
async fn a_recently_claimed_foreign_template_survives_the_sweep() {
    let Some(mut harness) = SweepHarness::open().await else {
        return;
    };
    let neighbour = SweepHarness::unique(TEMPLATE_PREFIX);
    let claimed_a_minute_ago = template_marker(now_epoch_seconds() - 60);
    harness
        .create(&neighbour, Some(&claimed_a_minute_ago))
        .await;

    harness.sweep().await;

    let survived = harness.exists(&neighbour).await;
    harness.close().await;
    assert!(
        survived,
        "a foreign template claimed 60s ago was dropped; that is the cross-worktree \
         `3D000` this suite exists to prevent"
    );
}

/// The other half. Without this the fix would trade a flake for an unbounded disk leak.
#[tokio::test]
async fn a_template_whose_claim_expired_is_reclaimed() {
    let Some(mut harness) = SweepHarness::open().await else {
        return;
    };
    let abandoned = SweepHarness::unique(TEMPLATE_PREFIX);
    let expired = template_marker(now_epoch_seconds() - TEMPLATE_IDLE_GRACE_SECONDS - 60);
    harness.create(&abandoned, Some(&expired)).await;

    harness.sweep().await;

    let survived = harness.exists(&abandoned).await;
    harness.close().await;
    assert!(
        !survived,
        "a template unclaimed for longer than the grace period was left behind — the \
         sweep no longer reclaims genuine leaks"
    );
}

/// A template built before markers existed must not become immortal. The sweep spares it
/// once and stamps it, which makes it reclaimable one grace period later.
#[tokio::test]
async fn an_unmarked_foreign_template_is_spared_and_adopted() {
    let Some(mut harness) = SweepHarness::open().await else {
        return;
    };
    let legacy = SweepHarness::unique(TEMPLATE_PREFIX);
    harness.create(&legacy, None).await;

    harness.sweep().await;

    let survived = harness.exists(&legacy).await;
    let marker = harness.comment_on(&legacy).await;
    harness.close().await;
    assert!(
        survived,
        "an unmarked template was dropped; the sweep cannot tell a legacy template from \
         a leak, so it must spare it"
    );
    let marker = marker.expect("the sweep must adopt an unmarked template by stamping it");
    assert_eq!(
        template_sweep_verdict(Some(&marker), now_epoch_seconds()),
        TemplateVerdict::Spare,
        "the adopted marker must read as a fresh claim, otherwise the next sweep would \
         drop it immediately: {marker}"
    );
    assert_eq!(
        template_sweep_verdict(
            Some(&marker),
            now_epoch_seconds() + TEMPLATE_IDLE_GRACE_SECONDS
        ),
        TemplateVerdict::Drop,
        "an adopted template must become reclaimable a grace period later, or the sweep \
         has traded a flake for a permanent disk leak: {marker}"
    );
}

/// The sweep's original job, unchanged by the template rule: a fixture database left by a
/// killed test process is still reclaimed, and a young one is still left alone.
#[tokio::test]
async fn aged_fixture_databases_are_still_reclaimed_and_young_ones_are_not() {
    let Some(mut harness) = SweepHarness::open().await else {
        return;
    };
    let now = now_epoch_seconds();
    let leaked = format!(
        "moira_test_{}_{}",
        now - support::LEAKED_DATABASE_GRACE_SECONDS - 60,
        Uuid::now_v7().simple()
    );
    let young = format!("moira_test_{}_{}", now, Uuid::now_v7().simple());
    harness.create(&leaked, None).await;
    harness.create(&young, None).await;

    harness.sweep().await;

    let leaked_survived = harness.exists(&leaked).await;
    let young_survived = harness.exists(&young).await;
    harness.close().await;
    assert!(
        !leaked_survived,
        "an aged, connectionless fixture database was not reclaimed"
    );
    assert!(
        young_survived,
        "a fixture database created seconds ago was reclaimed — a live fixture is \
         briefly connectionless between CREATE DATABASE and its pool's first connection"
    );
}

/// The decision itself, at its boundaries, with no cluster involved.
///
/// The end-to-end cases above cannot pin the boundary: fabricating a template that is
/// exactly one second inside the grace period and sweeping it is a race with the clock.
#[test]
fn the_sweep_verdict_is_exact_at_the_grace_boundary() {
    let now = 1_000_000_u64;
    assert_eq!(
        template_sweep_verdict(None, now),
        TemplateVerdict::Adopt,
        "no marker means the sweep does not recognise the template and must not drop it"
    );
    assert_eq!(
        template_sweep_verdict(Some("something else entirely"), now),
        TemplateVerdict::Adopt,
        "a comment this harness did not write is not a claim it may act on"
    );
    assert_eq!(
        template_sweep_verdict(Some(&template_marker(now)), now),
        TemplateVerdict::Spare
    );
    assert_eq!(
        template_sweep_verdict(
            Some(&template_marker(now - TEMPLATE_IDLE_GRACE_SECONDS + 1)),
            now
        ),
        TemplateVerdict::Spare,
        "one second inside the grace period is still a live claim"
    );
    assert_eq!(
        template_sweep_verdict(
            Some(&template_marker(now - TEMPLATE_IDLE_GRACE_SECONDS)),
            now
        ),
        TemplateVerdict::Drop,
        "the grace period is inclusive at its far end"
    );
    // A clock that went backwards must not turn into a `now - last_used` underflow and a
    // colossal age that reclaims a live neighbour's template.
    assert_eq!(
        template_sweep_verdict(Some(&template_marker(now + 3_600)), now),
        TemplateVerdict::Spare,
        "a marker from the future is a clock skew, not a leak"
    );
}
