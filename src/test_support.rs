//! Database support for the library crate's own `#[cfg(test)]` modules — issue #77.
//!
//! # Why this module exists at all
//!
//! Until now the database-backed unit tests under `src/**/tests` connected straight to
//! `MOIRA_TEST_DATABASE_URL` and ran `MIGRATOR` **against that database**. Two comments in
//! the tree said in so many words that adding a test-support module to the library crate
//! "would be a structural change for a test-only concern" — a reasonable call when the
//! only cost was twenty lines of duplicated advisory-lock scaffolding. It stopped being
//! the right call when the cost was measured:
//!
//! * A branch carrying an unmerged migration applied it to the shared database the moment
//!   its lib tests ran. Every *other* worktree — including `main` — then failed every
//!   database-backed lib test with
//!   `migration 21 was previously applied but is missing in the resolved migrations`,
//!   naming a migration its author had never heard of. `cargo test` stops after the lib
//!   target fails, so `scripts/gates.sh` reported `0 of 36` integration targets and the
//!   whole suite never ran. It is **not repairable from the affected side**: deleting the
//!   `_sqlx_migrations` row breaks the branch that owns it.
//! * Every row those tests wrote was permanent and shared. That is finding F27, which the
//!   integration suites were moved off for exactly this reason (160 leaked
//!   `trusted_jwt_issuers` rows, 42 live `moira:admin` keys) — the lib tests were the
//!   remaining hole.
//!
//! # What it does instead
//!
//! One private database per **test process**, created from nothing and migrated once:
//! `moira_test_<epoch>_<uuid>`, deliberately the same name grammar
//! `tests/support/mod.rs` gives a fixture clone, so the existing leak sweep reclaims it
//! with no new rule. Nothing this crate's tests do can now reach the database
//! `MOIRA_TEST_DATABASE_URL` names: it is read only to borrow the host, port and
//! credentials, and the connection that is actually opened is to the maintenance database.
//! The poisoning failure mode is therefore not "better reported" — it is unreachable.
//!
//! **A whole process, not a whole test.** `tests/support` can afford a database per
//! fixture because it clones a migrated template in ~50 ms; here there is no template, so
//! the ~26 migrations are replayed once and the result shared by every test in the binary.
//! The two advisory locks that already guard the singleton rows
//! (`SetupStateLock`, `IssuerlessSlotLock`) stay exactly as they were — they now serialise
//! the threads of one process against each other instead of every process on the cluster,
//! which is strictly less contention for the same guarantee.
//!
//! **Only the URL is cached; the pool is per call.** Each `#[tokio::test]` owns its runtime
//! and shuts it down when it ends, so a `PgPool` parked in a `static` is driven by a
//! reactor that is already gone by the second test — measured here as `PoolTimedOut` and
//! `A Tokio 1.x context was found, but it is being shutdown`. See [`LIB_DATABASE`].
//!
//! **The database outlives the process by up to an hour.** There is no process-exit hook a
//! `static` can hang teardown off, so reclamation is delegated to the same age-bounded
//! sweep that reclaims a fixture killed by `SIGKILL`; [`sweep_leaked_lib_databases`] runs
//! that sweep here too, so a bare `cargo test --lib` is self-cleaning rather than relying
//! on an integration target to tidy up after it. An hour of grace is what makes that safe
//! without a liveness marker: this database is briefly connectionless between tests, and a
//! whole lib-test run is three orders of magnitude inside the window.

use std::{
    env,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use sqlx::{
    Connection, PgConnection, PgPool, migrate::MigrateError, postgres::PgPoolOptions,
    postgres::PgQueryResult,
};
use tokio::{sync::OnceCell, time::timeout};
use url::Url;
use uuid::Uuid;

use crate::infra::db::MIGRATOR;

const MAINTENANCE_DATABASE: &str = "postgres";
const DATABASE_TIMEOUT: Duration = Duration::from_secs(60);

/// Mirrors `tests/support/mod.rs`. A lib-test database with no session attached and older
/// than this was left behind by a process that died.
const LEAKED_DATABASE_GRACE_SECONDS: u64 = 3_600;

/// Mirrors `tests/support/mod.rs::TEMPLATE_LOCK_KEY` **by value, deliberately**.
///
/// The two harnesses sweep the same set of databases on the same cluster and must not do
/// it concurrently, and there is no code they can share: one is a `#[cfg(test)]` module of
/// the library crate, the other an integration-test module. A named constant in each, with
/// this note in both directions, is the only honest way to write "the same lock".
const TEMPLATE_LOCK_KEY: i64 = i64::from_be_bytes(*b"moiratdb");

/// The one deliberately-named way to run this tree's tests with no database.
///
/// Same spelling and same semantics as `tests/support/mod.rs::ALLOW_NO_DATABASE`; see
/// [`refuse_to_run_without_a_database`].
pub(crate) const ALLOW_NO_DATABASE: &str = "MOIRA_TEST_ALLOW_NO_DATABASE";

/// The URL of this process's private database, resolved once.
///
/// **A `String`, never a `PgPool` or a `PgConnection`, and that is not a style choice.**
/// Every `#[tokio::test]` builds its own runtime and tears it down when the test ends, so a
/// pool parked in a `static` outlives the reactor that drives it: the second test to use it
/// gets `PoolTimedOut`, and any long-lived connection gets
/// `A Tokio 1.x context was found, but it is being shutdown`. Both were observed here
/// before this was a `String`. Only plain data may cross the boundary between two tests'
/// runtimes; `tests/support/mod.rs` caches its `DatabaseOrigin` for exactly the same
/// reason.
static LIB_DATABASE: OnceCell<Option<String>> = OnceCell::const_new();

/// A handle onto the process-private database: a pool built inside the **calling test's**
/// runtime, plus the URL for tests that also need a session of their own.
pub(crate) struct LibTestDatabase {
    pool: PgPool,
    url: String,
    name: String,
}

impl LibTestDatabase {
    pub(crate) fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// The URL of this process's private database — for the tests that need a dedicated
    /// session of their own to hold a session-scoped advisory lock.
    pub(crate) fn url(&self) -> &str {
        &self.url
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }
}

/// The message a suite dies with when no database is configured and the opt-out is absent.
///
/// # Why the absence of a database is a failure and not a skip
///
/// It used to be `let Ok(url) = env::var(..) else { eprintln!("skipping …"); return; }` at
/// nine call sites, and every one of them then reported **success**. A test binary that
/// cannot reach a database asserted nothing and said so in one line of stderr nobody
/// reads; `plans/reports/EXECUTION-LEDGER.md` records a round of results invalidated
/// exactly that way. `scripts/gates.sh` grew a grep for the skip line, which helped, but
/// put the whole guarantee in a shell script downstream of the thing being guaranteed.
fn refuse_to_run_without_a_database(reason: &str) -> ! {
    panic!(
        "{reason}\n\n\
         Database-backed tests refuse to run without a database. Start PostgreSQL and set \
         MOIRA_TEST_DATABASE_URL, for example:\n\n    \
         export MOIRA_TEST_DATABASE_URL=postgres://postgres:postgres@127.0.0.1:5432/moira\n\n\
         The role it names needs CREATEDB: the library's own tests migrate a private \
         database per test process and never touch the one the URL names.\n\n\
         If you genuinely mean to run the suite with no database — knowing that this \
         silently removes almost all of Moira's coverage — set {ALLOW_NO_DATABASE}=1. It is \
         ignored when CI=true, and the skip line it prints still reds `scripts/gates.sh`."
    )
}

/// Announces a skip on the **process's** stderr rather than the one `libtest` captures.
///
/// Mirrors `tests/support/mod.rs::announce_skip`, and for the same measured reason:
/// `libtest` prints a passing test's captured output nowhere, so a skip announced with
/// `eprintln!` is invisible to `scripts/gates.sh`'s zero-skip assertion. Writing to
/// `std::io::stderr()` directly bypasses the capture, which is installed on
/// `std::io::_eprint`.
fn announce_skip(message: &str) {
    use std::io::Write as _;
    let _ = writeln!(std::io::stderr(), "{message}");
}

/// Whether the opt-out is in force. `CI=true` overrides it: a CI run that cannot reach a
/// database has a broken service container, and a workflow file must not be able to turn
/// the gate off with one line of YAML.
fn no_database_opt_out_is_set() -> bool {
    if env::var("CI").is_ok_and(|value| value.eq_ignore_ascii_case("true")) {
        return false;
    }
    env::var(ALLOW_NO_DATABASE).is_ok_and(|value| matches!(value.trim(), "1" | "true" | "TRUE"))
}

/// `MOIRA_TEST_DATABASE_URL` parsed for its host, port and credentials only.
///
/// The database component is discarded on purpose — this module never connects to it.
fn database_origin() -> Option<Url> {
    match env::var("MOIRA_TEST_DATABASE_URL") {
        Ok(value) if !value.trim().is_empty() => {
            Some(Url::parse(value.trim()).expect("MOIRA_TEST_DATABASE_URL must be a valid URL"))
        }
        _ if no_database_opt_out_is_set() => {
            announce_skip(&format!(
                "skipping database-backed library tests: MOIRA_TEST_DATABASE_URL is not set \
                 and {ALLOW_NO_DATABASE} is in force"
            ));
            None
        }
        _ => refuse_to_run_without_a_database("MOIRA_TEST_DATABASE_URL is not set (or is empty)."),
    }
}

fn url_for(origin: &Url, database: &str) -> String {
    let mut url = origin.clone();
    url.set_path(database);
    url.to_string()
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the system clock is after the Unix epoch")
        .as_secs()
}

/// The epoch encoded in a fixture-shaped database name. Anything else is not this
/// harness's to reclaim.
fn fixture_database_age_seconds(name: &str) -> Option<u64> {
    let created = name
        .strip_prefix("moira_test_")?
        .split('_')
        .next()?
        .parse::<u64>()
        .ok()?;
    Some(unix_seconds().saturating_sub(created))
}

/// A pool onto this process's private, migrated database — or `None` when the no-database
/// opt-out is deliberately in force.
///
/// The *database* is created and migrated once per process; the *pool* is new on every
/// call, built inside the caller's runtime and dropped with the caller. That split is what
/// makes the ~26-migration cost a one-off without parking a pool in a `static` that outlives
/// the runtime driving it. Four connections is well above what any of these tests use
/// concurrently and well under `max_connections`.
pub(crate) async fn test_database() -> Option<LibTestDatabase> {
    let url = LIB_DATABASE
        .get_or_init(|| async {
            let origin = database_origin()?;
            Some(create_private_database(&origin).await)
        })
        .await
        .clone()?;
    let name = Url::parse(&url)
        .expect("the private database URL is well formed")
        .path()
        .trim_start_matches('/')
        .to_string();
    let pool = timeout(
        DATABASE_TIMEOUT,
        PgPoolOptions::new().max_connections(4).connect(&url),
    )
    .await
    .expect("library-test database connection timed out")
    .expect("connect the library-test database");
    Some(LibTestDatabase { pool, url, name })
}

/// Creates and migrates the database, and returns its URL. Everything it opens is closed
/// before it returns — see [`LIB_DATABASE`].
async fn create_private_database(origin: &Url) -> String {
    let maintenance_url = url_for(origin, MAINTENANCE_DATABASE);
    let mut maintenance = connect(&maintenance_url).await;
    sweep_leaked_lib_databases(&mut maintenance).await;

    let name = format!("moira_test_{}_{}", unix_seconds(), Uuid::now_v7().simple());
    run(
        sqlx::raw_sql(&format!("create database \"{name}\"")).execute(&mut maintenance),
        "create the private library-test database",
    )
    .await;
    let _ = maintenance.close().await;

    let url = url_for(origin, &name);
    let pool = timeout(
        DATABASE_TIMEOUT,
        PgPoolOptions::new().max_connections(1).connect(&url),
    )
    .await
    .expect("library-test database connection timed out")
    .expect("connect the library-test database");
    migrate(&pool, &name).await;
    pool.close().await;
    url
}

/// Runs the migration set, translating the one failure whose stock message sends the
/// reader hunting for a bug that is not there.
///
/// # Why this translation is here even though the condition is now unreachable
///
/// A private database is empty when it is created, so `VersionMissing` cannot arise from
/// this harness's own use. It arises the moment anyone points a migrator at a database
/// **another checkout has already migrated** — which is what every database-backed lib
/// test in this crate did until issue #77, and what anyone will do again the first time
/// they set `MOIRA_TEST_DATABASE_URL` to a colleague's database or reuse an old one. The
/// stock text names a version number and nothing else:
///
/// ```text
/// migration 21 was previously applied but is missing in the resolved migrations
/// ```
///
/// A reader who has never seen migration 21 has no way to get from that to "a different
/// branch applied its unmerged migration to a database this tree also uses". Naming the
/// database, the cause and the remedy costs nothing and is covered by
/// `the_missing_migration_explanation_names_the_cause_and_the_remedy` below.
async fn migrate(pool: &PgPool, database: &str) {
    let outcome = timeout(DATABASE_TIMEOUT, MIGRATOR.run(pool))
        .await
        .expect("library-test database migrations timed out");
    if let Err(error) = outcome {
        panic!("{}", explain_migration_failure(database, &error));
    }
}

/// The text [`migrate`] dies with, as a pure function so it can be asserted on.
fn explain_migration_failure(database: &str, error: &MigrateError) -> String {
    let diagnosis = match error {
        MigrateError::VersionMissing(version) => Some(format!(
            "Migration {version} has been applied to \"{database}\" but does not exist in this \
             tree's migrations/ directory.\n\n\
             This is almost never a fault in the code under test. It happens when a checkout \
             carrying an unmerged migration runs its tests against a database that another \
             checkout also uses: the migration lands in that database's _sqlx_migrations, and \
             every tree without the file then refuses to migrate. The cost is not borne by the \
             branch that added the migration — it is borne by every other one, naming a version \
             number its author has never seen.\n\n\
             Remedy: give this run a database of its own. MOIRA_TEST_DATABASE_URL should name a \
             database no other checkout shares, and its role needs CREATEDB so the harnesses can \
             create the private databases they actually run against. Do not delete the \
             _sqlx_migrations row: that repairs this tree by breaking the one that owns the \
             migration."
        )),
        MigrateError::VersionMismatch(version) => Some(format!(
            "Migration {version} exists in this tree's migrations/ but its checksum differs from \
             the copy applied to \"{database}\".\n\n\
             Migrations are append-only in this repository, so an edited migration file is the \
             usual cause; a database shared with a checkout that edited one is the other. \
             Remedy: revert the edit and add a new migration instead, or point \
             MOIRA_TEST_DATABASE_URL at a database no other checkout shares."
        )),
        _ => None,
    };
    match diagnosis {
        Some(diagnosis) => {
            format!("migrating the test database \"{database}\" failed: {error}\n\n{diagnosis}")
        }
        None => format!("migrating the test database \"{database}\" failed: {error}"),
    }
}

/// Reclaims fixture-shaped databases left behind by a killed test process.
///
/// A deliberately narrower sweep than `tests/support/mod.rs`'s: it only ever considers the
/// `moira_test_<epoch>_<uuid>` shape, never a `moira_test_template_*` or a
/// `moira_test_building_*`. Templates are the integration harness's to reason about — they
/// need the owning-run marker that harness maintains — and a second implementation of that
/// rule here would be a second chance to get it wrong.
async fn sweep_leaked_lib_databases(maintenance: &mut PgConnection) {
    // The same lock `tests/support/mod.rs` sweeps under, so the two harnesses cannot sweep
    // or create databases concurrently on one cluster.
    if sqlx::query("select pg_advisory_lock($1)")
        .bind(TEMPLATE_LOCK_KEY)
        .execute(&mut *maintenance)
        .await
        .is_err()
    {
        return;
    }
    let idle: Vec<String> = sqlx::query_scalar(
        "select d.datname from pg_database d \
         where d.datname like 'moira\\_test\\_%' \
           and not exists (\
                 select 1 from pg_stat_activity a \
                 where a.datname = d.datname and a.backend_type = 'client backend')",
    )
    .fetch_all(&mut *maintenance)
    .await
    .unwrap_or_default();

    for name in idle {
        if !fixture_database_age_seconds(&name)
            .is_some_and(|age| age >= LEAKED_DATABASE_GRACE_SECONDS)
        {
            continue;
        }
        // Best effort: a leaked database is not worth failing a test run over.
        let _ = sqlx::raw_sql(&format!("drop database if exists \"{name}\" with (force)"))
            .execute(&mut *maintenance)
            .await;
    }
    let _ = sqlx::query("select pg_advisory_unlock($1)")
        .bind(TEMPLATE_LOCK_KEY)
        .execute(&mut *maintenance)
        .await;
}

async fn connect(url: &str) -> PgConnection {
    timeout(DATABASE_TIMEOUT, PgConnection::connect(url))
        .await
        .expect("test database connection timed out")
        .expect("connect the test database")
}

async fn run<'a>(
    operation: impl Future<Output = Result<PgQueryResult, sqlx::Error>> + 'a,
    description: &str,
) {
    timeout(DATABASE_TIMEOUT, operation)
        .await
        .unwrap_or_else(|_| panic!("{description} timed out"))
        .unwrap_or_else(|error| panic!("{description}: {error}"));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The private database is private: nothing this crate's tests run against is the
    /// database `MOIRA_TEST_DATABASE_URL` names.
    ///
    /// Asserted rather than assumed because the whole of issue #77 problem 3 is one
    /// `PgPool::connect(&database_url)` that looked harmless at nine call sites.
    #[tokio::test]
    async fn library_tests_never_run_against_the_shared_database() {
        let Some(database) = test_database().await else {
            return;
        };
        let shared = env::var("MOIRA_TEST_DATABASE_URL").expect("a database is configured");
        let shared_name = Url::parse(&shared)
            .expect("a valid URL")
            .path()
            .trim_start_matches('/')
            .to_string();

        assert_ne!(
            database.name(),
            shared_name,
            "the library test harness is running against the shared database; an unmerged \
             migration applied here breaks every other worktree's lib tests"
        );
        assert!(
            database.name().starts_with("moira_test_"),
            "the private database must carry the fixture name grammar so the leak sweep \
             reclaims it: {}",
            database.name()
        );
        let live: String = sqlx::query_scalar("select current_database()")
            .fetch_one(database.pool())
            .await
            .expect("read the connected database name");
        assert_eq!(live, database.name());
    }

    /// Issue #77 problem 3, the diagnostic half: the message a victim of a poisoned
    /// database sees must name the cause and the remedy, not just a version number.
    ///
    /// The condition is fabricated against a real database rather than mocked, because
    /// what is being pinned is that `sqlx` reports this situation as `VersionMissing` —
    /// an assumption about a dependency, and the only part of the translation that can
    /// rot silently.
    #[tokio::test]
    async fn the_missing_migration_explanation_names_the_cause_and_the_remedy() {
        let Some(_database) = test_database().await else {
            return;
        };
        let origin = database_origin().expect("a database is configured");
        let maintenance_url = url_for(&origin, MAINTENANCE_DATABASE);
        let mut maintenance = connect(&maintenance_url).await;
        let name = format!("moira_test_{}_{}", unix_seconds(), Uuid::now_v7().simple());
        run(
            sqlx::raw_sql(&format!("create database \"{name}\"")).execute(&mut maintenance),
            "create a database to poison",
        )
        .await;

        let poisoned = PgPool::connect(&url_for(&origin, &name))
            .await
            .expect("connect the poisoned database");
        // Migrated first, so `_sqlx_migrations` is the table sqlx itself created — the
        // alternative, hand-writing that DDL here, would pin an internal schema this test
        // has no business knowing.
        MIGRATOR
            .run(&poisoned)
            .await
            .expect("migrate the database to be poisoned");
        // Exactly what a neighbouring checkout's unmerged migration leaves behind.
        sqlx::query(
            "insert into _sqlx_migrations \
             (version, description, installed_on, success, checksum, execution_time) \
             values (99999, 'a neighbour''s unmerged migration', now(), true, '\\x00'::bytea, 0)",
        )
        .execute(&poisoned)
        .await
        .expect("record a migration this tree does not have");

        let error = MIGRATOR
            .run(&poisoned)
            .await
            .expect_err("migrating over a version this tree does not have must fail");
        poisoned.close().await;
        let _ = sqlx::raw_sql(&format!("drop database if exists \"{name}\" with (force)"))
            .execute(&mut maintenance)
            .await;
        let _ = maintenance.close().await;

        let explanation = explain_migration_failure(&name, &error);
        eprintln!("--- explain_migration_failure ---\n{explanation}\n--- end ---");
        assert!(
            matches!(error, MigrateError::VersionMissing(99999)),
            "sqlx no longer reports a poisoned database as VersionMissing, so the \
             translation below would silently stop firing: {error:?}"
        );
        for expected in [
            &name as &str,
            "99999",
            "unmerged migration",
            "MOIRA_TEST_DATABASE_URL",
            "CREATEDB",
            "Do not delete the _sqlx_migrations row",
        ] {
            assert!(
                explanation.contains(expected),
                "the explanation must contain {expected:?}: {explanation}"
            );
        }
    }

    /// An unrecognised failure must not be dressed up as a diagnosis it is not.
    #[test]
    fn an_unrelated_migration_failure_is_reported_without_a_false_diagnosis() {
        let explanation =
            explain_migration_failure("moira_test_0_x", &MigrateError::ForceNotSupported);
        assert!(explanation.contains("moira_test_0_x"));
        assert!(!explanation.contains("unmerged migration"));
    }
}
