//! Issue #250 finding 1 — keeps `0030`'s validating `ADD CONSTRAINT` from ever reaching a
//! populated `execution_attempts`.
//!
//! # The hazard
//!
//! `migrations/0030_execution_attempt_candidate_observability.sql` adds
//! `execution_attempts_selection_reason_valid` with a bare `ALTER TABLE … ADD CONSTRAINT`. That
//! form takes `ACCESS EXCLUSIVE` **and** performs the validating scan while holding it. The lock
//! is on the table, not on the migrating replica, so on `execution_attempts` — one row per
//! upstream provider attempt, the highest-volume table in the schema — every reader and writer in
//! the fleet blocks for the length of a full scan, from a migration that reads like a one-liner.
//!
//! `migrations/0027_content_encryption_keyring.sql:12-36` states the shape this repository uses
//! instead: `ADD CONSTRAINT … NOT VALID` (`ACCESS EXCLUSIVE`, no scan), a commit, then
//! `VALIDATE CONSTRAINT` (`SHARE UPDATE EXCLUSIVE`, scans, blocks neither readers nor writers).
//!
//! # Why the fix is here and not in a migration
//!
//! `0030` shipped on `main` and migrations are append-only here — `docs/project-structure.md:19`,
//! `migrations/0018_admin_identity_granted_by_invite.sql:20-21`, `src/test_support.rs`'s
//! `VersionMismatch` diagnosis — because `sqlx` checksums every file and a database that applied
//! the old bytes then refuses to boot. So `0030` cannot be repaired in place.
//!
//! **And nothing appended after `0030` can help either.** A follow-up migration runs *after*
//! `0030` has already taken the lock and done the scan. `migrations/0034_…` re-installs the
//! constraint in the safe shape, which fixes the definition a future reader copies and which
//! `tests/migration_constraint_safety.rs` pins — but it does not, and cannot, make `0030` cheap.
//!
//! The only code that runs **before** `sqlx` reaches `0030` is the code that calls `sqlx`. That is
//! this module. [`defuse_pending_hot_table_constraints`] applies `0030`'s own three statements in
//! the non-blocking order and then records `0030` as applied, so `Migrator::run` finds the version
//! in `_sqlx_migrations` and never executes its `ADD CONSTRAINT` at all
//! (`sqlx_core::migrate::migrator::Migrator::run_direct` skips any version already present, after
//! comparing checksums).
//!
//! # Why forging the ledger row is safe here, and what makes it stay safe
//!
//! Three properties, each of which is asserted rather than asserted-about:
//!
//! * **The statements are `0030`'s, not a paraphrase.** [`ZERO030_STATEMENTS`] holds the three
//!   statements verbatim; the only edit is ` not valid` appended to the third. Before doing
//!   anything, [`embedded_0030_is_unchanged`] normalises the *embedded* `0030` and refuses to act
//!   unless it is exactly those three statements. If `0030` is ever renumbered, edited or removed,
//!   this module stands down and `sqlx` behaves as it did before — slow, but never wrong.
//! * **The checksum is read, never typed.** It comes from the embedded [`Migration`] itself, so
//!   the row can only ever match. A wrong checksum is not silent — `Migrator::run` fails with
//!   `VersionMismatch(30)` and applies nothing — but it is also not possible from here.
//! * **The row says what happened.** Its `description` is [`PRE_APPLIED_DESCRIPTION`], not the
//!   stock one derived from the filename, so an operator reading `_sqlx_migrations` can see that
//!   `0030` was pre-applied rather than run. `sqlx` reads only `version` and `checksum` from that
//!   table (`AppliedMigration`), so the text is free to be honest.
//!
//! # What it deliberately does not cover
//!
//! A deployment that migrates with `sqlx-cli` instead of `moira migrate` never calls this. That
//! path still runs `0030` as written, and `docs/release-notes.md` keeps the manual procedure for
//! it.

use std::time::Instant;

use sqlx::{Connection as _, migrate::Migration};
use tracing::{info, warn};

use crate::{error::AppError, infra::db::MIGRATOR};

/// The migration whose `ADD CONSTRAINT` this module exists to keep from ever running.
const HAZARDOUS_VERSION: i64 = 30;

/// The constraint `0030` installs, and the one validated separately here.
const CONSTRAINT: &str = "execution_attempts_selection_reason_valid";

/// The advisory lock [`crate::infra::db::migrate`] holds across the preflight **and** the migrator
/// run that follows it.
///
/// `sqlx` takes a lock of its own inside `Migrator::run`, keyed on the database name
/// (`sqlx_postgres::migrate::generate_lock_id`), which serialises two migrators against each
/// other. It cannot serialise a migrator against something that runs *before* it, and that is
/// exactly what this module is. Without this key, one process could be pre-applying `0030` while
/// another is already inside `Migrator::run` and about to execute it: the second would take the
/// blocking scan anyway and then fail on the primary key of the ledger row the first had just
/// written. Held over both halves, that interleaving cannot happen between Moira processes.
///
/// It does not reach a `sqlx-cli` migrating the same database at the same moment. Neither does
/// anything else here; see the module header.
pub(crate) const MIGRATE_LOCK_KEY: i64 = i64::from_be_bytes(*b"moiramig");

/// What `_sqlx_migrations.description` says for a pre-applied `0030`.
///
/// Deliberately not the stock description `sqlx` derives from the filename. An operator reading
/// the ledger should be able to tell that the scan was taken in the non-blocking shape, and a
/// test can tell whether this module ran or whether `sqlx` executed `0030` itself.
const PRE_APPLIED_DESCRIPTION: &str =
    "execution attempt candidate observability (pre-applied NOT VALID + VALIDATE, issue #250)";

/// `0030`'s three statements, verbatim.
///
/// Verbatim is the point: the effect this module applies must be `0030`'s effect, and the way to
/// guarantee that is to run `0030`'s own SQL rather than a re-derivation of it. The single edit is
/// ` not valid` appended to the third statement, which is the entire fix.
///
/// [`embedded_0030_is_unchanged`] compares these against the migration `sqlx` actually embedded,
/// and `the_statements_this_module_replaces_are_still_0030s` compares them against the file on
/// disk. Either drifting stands this module down rather than letting it apply something else.
const ZERO030_STATEMENTS: [&str; 3] = [
    "alter table execution_attempts
    add column if not exists candidate_rank integer,
    add column if not exists candidate_score double precision,
    add column if not exists selection_reason varchar(64)",
    "alter table execution_attempts
    drop constraint if exists execution_attempts_selection_reason_valid",
    "alter table execution_attempts
    add constraint execution_attempts_selection_reason_valid check (
        selection_reason is null
        or selection_reason in ('priority', 'explicit_hint', 'scored', 'fallback_after_failure')
    )",
];

/// Lowercased, comment-free, whitespace-collapsed — the form two spellings of the same SQL agree
/// on and two different statements do not.
fn normalise(sql: &str) -> String {
    sql.lines()
        .filter(|line| !line.trim_start().starts_with("--"))
        .collect::<Vec<_>>()
        .join("\n")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// The statements of a migration file, normalised, in order.
fn normalised_statements(sql: &str) -> Vec<String> {
    normalise(sql)
        .split(';')
        .map(|statement| statement.trim().to_string())
        .filter(|statement| !statement.is_empty())
        .collect()
}

/// Whether the migration `sqlx` embedded is still the one [`ZERO030_STATEMENTS`] transcribes.
///
/// Append-only migrations make this unfalsifiable in practice, which is exactly why it is checked:
/// the cost of being wrong is a database whose schema differs from what its ledger claims, and the
/// cost of the check is a string comparison once per process.
fn embedded_0030_is_unchanged(sql: &str) -> bool {
    normalised_statements(sql)
        == ZERO030_STATEMENTS
            .iter()
            .map(|statement| normalise(statement))
            .collect::<Vec<_>>()
}

/// The embedded `0030`, or `None` if this tree no longer has one.
fn embedded_0030() -> Option<&'static Migration> {
    MIGRATOR
        .iter()
        .find(|migration| migration.version == HAZARDOUS_VERSION)
}

/// Pre-applies `0030` in the non-blocking shape when, and only when, the migrator is about to run
/// it against a table that already exists.
///
/// A no-op — two catalog lookups — on every other database, including every fresh install, where
/// `execution_attempts` does not exist yet and `0030` will land on an empty table anyway.
///
/// **The caller must already hold [`MIGRATE_LOCK_KEY`] on this connection, and must keep holding
/// it until the migrator has finished.** The connection is threaded in rather than acquired here
/// for that reason: the lock, the pre-apply and `Migrator::run` have to be one critical section on
/// one session, and a function that took a `PgPool` could not promise that.
///
/// Errors here fail the migration rather than falling through to `sqlx`: reaching this point means
/// the ledger says `0030` has not run and the table is there, so falling through is precisely the
/// fleet-wide stall this exists to prevent, and doing it silently would be worse than not booting.
pub(crate) async fn defuse_pending_hot_table_constraints(
    conn: &mut sqlx::PgConnection,
) -> Result<(), AppError> {
    let Some(migration) = embedded_0030() else {
        warn!(
            version = HAZARDOUS_VERSION,
            "migration is absent from this tree; the pre-apply preflight stands down"
        );
        return Ok(());
    };
    if !embedded_0030_is_unchanged(&migration.sql) {
        warn!(
            version = HAZARDOUS_VERSION,
            "migration no longer matches the statements the preflight transcribes; standing down \
             rather than applying something else. sqlx will run it as written."
        );
        return Ok(());
    }
    pre_apply(conn, migration).await
}

async fn pre_apply(conn: &mut sqlx::PgConnection, migration: &Migration) -> Result<(), AppError> {
    // No ledger table means an empty database: `0030` cannot run before `0005` creates the table,
    // and it will find it empty when it does.
    let ledger_exists: bool =
        sqlx::query_scalar("select to_regclass('public._sqlx_migrations') is not null")
            .fetch_one(&mut *conn)
            .await?;
    if !ledger_exists {
        return Ok(());
    }

    let already_applied: bool = sqlx::query_scalar(
        "select exists (select 1 from _sqlx_migrations where version = $1 and success)",
    )
    .bind(HAZARDOUS_VERSION)
    .fetch_one(&mut *conn)
    .await?;
    if already_applied {
        return Ok(());
    }

    // Behind `0005`. The table will be created empty later in the same run and `0030` will scan
    // nothing; pre-applying against a table that does not exist would just fail.
    //
    // The converse — the table exists but the database is *well* behind `0030`, say at `0020` with
    // years of attempts in it — is deliberately still handled. `sqlx` applies versions in order and
    // is content to find a later one already recorded (`run_direct` looks each version up in the
    // applied set and skips a hit), and nothing between `0005` and `0030` touches
    // `execution_attempts`' columns: `grep -l execution_attempts migrations/` returns `0005`,
    // `0025` (prose only), `0030` and `0034`. Restricting this to "exactly one migration short"
    // would leave the worst case — the oldest, busiest database — as the one that still blocks.
    let table_exists: bool =
        sqlx::query_scalar("select to_regclass('public.execution_attempts') is not null")
            .fetch_one(&mut *conn)
            .await?;
    if !table_exists {
        return Ok(());
    }

    let started = Instant::now();

    // Group one: metadata only. `add column` without a default is a catalog write in PostgreSQL 11
    // and later, and `add constraint … not valid` performs no scan, so `ACCESS EXCLUSIVE` is held
    // for the length of three catalog updates rather than for the length of a table scan.
    let mut metadata = conn.begin().await?;
    sqlx::query(ZERO030_STATEMENTS[0])
        .execute(&mut *metadata)
        .await?;
    sqlx::query(ZERO030_STATEMENTS[1])
        .execute(&mut *metadata)
        .await?;
    sqlx::query(&format!("{} not valid", ZERO030_STATEMENTS[2]))
        .execute(&mut *metadata)
        .await?;
    metadata.commit().await?;

    // Group two, after that commit released `ACCESS EXCLUSIVE`: the scan, under
    // `SHARE UPDATE EXCLUSIVE`, which blocks neither readers nor writers. Inserts into
    // `execution_attempts` keep working throughout — the property the split exists for, and the
    // reason the two groups cannot share a transaction.
    //
    // The ledger row rides in this transaction rather than a third: a crash between a successful
    // validation and an unrecorded row would leave `0030` still pending, and the next run would
    // hand `sqlx` a table that already has the constraint — where `0030`'s `drop … / add` would
    // re-take the scan under `ACCESS EXCLUSIVE`, which is the outcome this module exists to
    // prevent. Together, they either both happen or neither does, and neither does is re-runnable.
    let mut validate = conn.begin().await?;
    sqlx::query(&format!(
        "alter table execution_attempts validate constraint {CONSTRAINT}"
    ))
    .execute(&mut *validate)
    .await?;
    sqlx::query(
        "insert into _sqlx_migrations \
             (version, description, installed_on, success, checksum, execution_time) \
         values ($1, $2, now(), true, $3, $4) \
         on conflict (version) do nothing",
    )
    .bind(migration.version)
    .bind(PRE_APPLIED_DESCRIPTION)
    .bind(&*migration.checksum)
    .bind(i64::try_from(started.elapsed().as_nanos()).unwrap_or(i64::MAX))
    .execute(&mut *validate)
    .await?;
    validate.commit().await?;

    info!(
        version = HAZARDOUS_VERSION,
        constraint = CONSTRAINT,
        elapsed_ms = started.elapsed().as_millis() as u64,
        "pre-applied the migration without holding ACCESS EXCLUSIVE across the validating scan"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use super::*;
    use crate::test_support;

    fn zero030_on_disk() -> String {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("migrations")
            .join("0030_execution_attempt_candidate_observability.sql");
        fs::read_to_string(&path).unwrap_or_else(|error| {
            panic!(
                "0030 must be readable at {}: {error}. If it was renumbered or removed, \
                 src/infra/migration_preflight.rs is transcribing a migration that no longer \
                 exists and must be revisited, not deleted",
                path.display()
            )
        })
    }

    /// The transcription is the whole safety argument: this module applies `0030`'s effect only
    /// insofar as [`ZERO030_STATEMENTS`] really is `0030`.
    #[test]
    fn the_statements_this_module_replaces_are_still_0030s() {
        assert!(
            embedded_0030_is_unchanged(&zero030_on_disk()),
            "migrations/0030 no longer normalises to the three statements \
             src/infra/migration_preflight.rs transcribes. The preflight stands down when this \
             happens, so the ACCESS EXCLUSIVE scan comes back — update ZERO030_STATEMENTS \
             deliberately, do not delete this test.\n\nOn disk:\n{:#?}\n\nTranscribed:\n{:#?}",
            normalised_statements(&zero030_on_disk()),
            ZERO030_STATEMENTS
                .iter()
                .map(|statement| normalise(statement))
                .collect::<Vec<_>>()
        );
    }

    /// The third statement is the hazard, and appending ` not valid` is the entire repair. Pinned
    /// so a reordering of [`ZERO030_STATEMENTS`] cannot quietly append it to the `add column`.
    #[test]
    fn the_statement_made_not_valid_is_the_add_constraint() {
        assert!(
            ZERO030_STATEMENTS[2].contains("add constraint"),
            "the statement the preflight appends ` not valid` to must be the ADD CONSTRAINT"
        );
        assert!(
            !normalise(ZERO030_STATEMENTS[2]).ends_with("not valid"),
            "0030's ADD CONSTRAINT is transcribed in its original validating form; the ` not \
             valid` is appended at execution time so the transcription can be compared to 0030 \
             byte for byte"
        );
    }

    /// The embedded `0030` and the file on disk are the same bytes, so an edit to either is caught
    /// by [`the_statements_this_module_replaces_are_still_0030s`] above.
    #[test]
    fn the_embedded_0030_is_the_file_on_disk() {
        let embedded = embedded_0030().expect("this tree embeds a migration 30");
        assert_eq!(
            normalised_statements(&embedded.sql),
            normalised_statements(&zero030_on_disk())
        );
    }

    /// The mechanism, end to end, on a database standing exactly where `0030` is next.
    ///
    /// The assertion that matters is the middle one: after the preflight and **before** the
    /// migrator has run, `_sqlx_migrations` already carries version 30 with the embedded checksum.
    /// `Migrator::run_direct` skips any version it finds there, so `0030`'s validating
    /// `ADD CONSTRAINT` cannot execute against this database — and the `db::migrate` call that
    /// follows proves the row is accepted rather than tripping `VersionMismatch(30)`.
    #[tokio::test]
    async fn pre_applying_0030_leaves_a_validated_constraint_and_a_ledger_row_sqlx_accepts() {
        let Some(database) = test_support::partially_migrated_database(HAZARDOUS_VERSION - 1).await
        else {
            return;
        };
        let pool = database.pool();

        let pending: Option<String> =
            sqlx::query_scalar("select description from _sqlx_migrations where version = $1")
                .bind(HAZARDOUS_VERSION)
                .fetch_optional(pool)
                .await
                .expect("read the ledger");
        assert_eq!(
            pending, None,
            "the fixture must stand one migration short of 0030 for this test to mean anything"
        );

        // A row, so the validating scan has something to scan and the constraint has something to
        // be true of.
        sqlx::query(
            "insert into execution_attempts (request_id, execution_id, attempt_number, status) \
             values ('req_preflight', gen_random_uuid(), 1, 'succeeded')",
        )
        .execute(pool)
        .await
        .expect("seed an attempt row");

        let mut conn = pool.acquire().await.expect("acquire a connection");
        defuse_pending_hot_table_constraints(&mut conn)
            .await
            .expect("pre-apply 0030");
        drop(conn);

        let (description, checksum): (String, Vec<u8>) =
            sqlx::query_as("select description, checksum from _sqlx_migrations where version = $1")
                .bind(HAZARDOUS_VERSION)
                .fetch_one(pool)
                .await
                .expect("0030 must be recorded as applied before the migrator ever sees it");
        assert_eq!(description, PRE_APPLIED_DESCRIPTION);
        assert_eq!(
            checksum,
            embedded_0030().expect("embedded 0030").checksum.to_vec(),
            "the recorded checksum must be the embedded one, or the next boot fails with \
             VersionMismatch(30)"
        );

        let validated: bool = sqlx::query_scalar(
            "select convalidated from pg_constraint \
             where conrelid = 'execution_attempts'::regclass and conname = $1",
        )
        .bind(CONSTRAINT)
        .fetch_one(pool)
        .await
        .expect("the constraint must exist");
        assert!(
            validated,
            "the constraint was added NOT VALID and must then have been validated in its own \
             transaction — otherwise existing rows were never proven against it"
        );

        crate::infra::db::migrate(pool)
            .await
            .expect("the rest of the migration set must apply on top of the pre-applied row");

        let after: String =
            sqlx::query_scalar("select description from _sqlx_migrations where version = $1")
                .bind(HAZARDOUS_VERSION)
                .fetch_one(pool)
                .await
                .expect("read the ledger");
        assert_eq!(
            after, PRE_APPLIED_DESCRIPTION,
            "the migrator must have skipped 0030, leaving the pre-applied row untouched"
        );

        database.discard().await;
    }

    /// The wiring. The test above proves the mechanism works when called; this one proves
    /// `db::migrate` — the one entry point `moira migrate`, `moira serve --migrate-on-startup`,
    /// `bootstrap-system-key` and `execute-test` all go through — actually calls it.
    ///
    /// Without the call, `sqlx` runs `0030` itself and writes the stock description derived from
    /// the filename. That is the difference this asserts on.
    #[tokio::test]
    async fn db_migrate_pre_applies_0030_rather_than_letting_sqlx_run_it() {
        let Some(database) = test_support::partially_migrated_database(HAZARDOUS_VERSION - 1).await
        else {
            return;
        };
        let pool = database.pool();

        crate::infra::db::migrate(pool).await.expect("migrate");

        let description: String =
            sqlx::query_scalar("select description from _sqlx_migrations where version = $1")
                .bind(HAZARDOUS_VERSION)
                .fetch_one(pool)
                .await
                .expect("0030 must be recorded");
        assert_eq!(
            description, PRE_APPLIED_DESCRIPTION,
            "db::migrate let sqlx execute 0030 itself. That statement takes ACCESS EXCLUSIVE on \
             execution_attempts and holds it across a full scan, which blocks every reader and \
             writer of the table fleet-wide for the duration — see \
             src/infra/migration_preflight.rs"
        );

        database.discard().await;
    }
}
