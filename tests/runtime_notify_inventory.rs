//! F52 — the set of tables wired to `moira_runtime_config`, taken from the database.
//!
//! # The finding is the guard, not the tables
//!
//! `every_triggered_table_has_a_scope` in `src/infra/db.rs` proves every table on this
//! channel classifies to something other than `CircuitResetScope::All` — an unclassified
//! table means a write to it discards every provider's earned breaker health on every
//! replica. Its doc comment said the list was *"pinned here against the trigger list in
//! `migrations/`"*.
//!
//! It was not pinned. It was **retyped**: 21 table names as a Rust array literal, checked
//! against a schema that had 24. The three it omitted — `legacy_providers`,
//! `legacy_routing_policies` and `legacy_provider_credentials`, created by
//! `0003_security_foundation.sql`'s `alter table providers rename to legacy_providers`,
//! which carries the trigger with it — were exactly the three that fell through to the
//! `other =>` arm. The guard passed, and had always passed, because it was comparing the
//! classifier to a copy of the classifier.
//!
//! **A guard that pins X against a hand-written list of X is not a guard, it is a
//! duplicate.** This file is the independent source: `pg_trigger` is written by
//! PostgreSQL from the migrations themselves, so it cannot agree with `db.rs` by having
//! been typed from it.
//!
//! # Counted by trigger *function*, never by trigger *name*
//!
//! Two naming facts make a name-based query wrong, and both are load-bearing:
//!
//! * `auth_provider_settings`'s trigger is called `auth_provider_settings_notify`, not
//!   `…_runtime_config_notify`, so `tgname like '%runtime_config_notify%'` misses it.
//! * `ALTER TABLE … RENAME` renames the table but not its triggers, so
//!   `legacy_providers` carried a trigger still called `providers_runtime_config_notify`
//!   — a name-based query would have reported it under the wrong table entirely.
//!
//! Joining `pg_proc` on `tgfoid` and filtering by `proname` is immune to both. That is
//! how the finder's first count came back 23 instead of 24.
//!
//! # `tgisinternal`
//!
//! Excluded because foreign-key and constraint enforcement are implemented as internal
//! triggers. They are not user triggers and never call this function, but leaving them in
//! would make the query's result depend on the FK graph.

mod support;

use std::collections::BTreeSet;

use moira::infra::db::TRIGGERED_RESOURCE_TYPES;
use sqlx::Row;
use support::TestDatabase;

/// The trigger function every table on this channel executes.
const NOTIFY_FUNCTION: &str = "notify_moira_runtime_config_change";

/// A floor on the inventory, in the shape `MINIMUM_SUITES_SCANNED` uses one file over in
/// `tests/test_database_isolation.rs`.
///
/// Without it, a query that returned nothing — a renamed function, a schema-qualification
/// mistake, a `pg_proc` join that silently matched no rows — would make every assertion
/// below compare two empty sets and pass. That is the failure this project has been bitten
/// by repeatedly, and it is the one a derived inventory is most exposed to: the whole
/// point of deriving the list is that nobody checks it by eye.
const MINIMUM_TRIGGERED_TABLES: usize = 15;

/// Every table whose triggers execute [`NOTIFY_FUNCTION`], read from the catalogue.
async fn triggered_tables(db: &TestDatabase) -> BTreeSet<String> {
    let rows = sqlx::query(
        "select distinct c.relname as table_name \
         from pg_trigger t \
         join pg_class c on c.oid = t.tgrelid \
         join pg_proc p on p.oid = t.tgfoid \
         where p.proname = $1 and not t.tgisinternal",
    )
    .bind(NOTIFY_FUNCTION)
    .fetch_all(&db.pool)
    .await
    .expect("read the runtime-config trigger inventory from pg_trigger");

    let tables: BTreeSet<String> = rows
        .into_iter()
        .map(|row| row.get::<String, _>("table_name"))
        .collect();

    assert!(
        tables.len() >= MINIMUM_TRIGGERED_TABLES,
        "found only {} tables executing {NOTIFY_FUNCTION}, expected at least \
         {MINIMUM_TRIGGERED_TABLES} — the catalogue query is not reaching the trigger \
         list, so every assertion in this file would pass against an empty set",
        tables.len()
    );
    tables
}

/// The property F52 records: the classifier's inventory is the schema's, exactly.
///
/// Set equality in **both** directions, and each direction catches a different mistake:
///
/// * A table in the schema and not in the constant is the F52 defect itself — it falls to
///   the `other =>` arm and resets every breaker on every replica, silently.
/// * A table in the constant and not in the schema is a stale entry. Harmless at runtime,
///   but it is how the constant starts drifting back into being a hand-maintained list
///   that happens to resemble the schema — and the next reader has no way to tell which
///   entries are real.
#[tokio::test]
async fn the_notify_trigger_inventory_is_exactly_the_classified_set() {
    let Some(db) = TestDatabase::create().await else {
        return;
    };

    let actual = triggered_tables(&db).await;
    let classified: BTreeSet<String> = TRIGGERED_RESOURCE_TYPES
        .iter()
        .map(|name| (*name).to_string())
        .collect();

    let unclassified: Vec<&String> = actual.difference(&classified).collect();
    assert!(
        unclassified.is_empty(),
        "these tables fire {NOTIFY_FUNCTION} but are absent from \
         TRIGGERED_RESOURCE_TYPES in src/infra/db.rs, so a write to any of them falls to \
         the `other =>` arm and discards every provider's breaker health on every \
         replica: {unclassified:?}. Classify them there, or drop their trigger."
    );

    let stale: Vec<&String> = classified.difference(&actual).collect();
    assert!(
        stale.is_empty(),
        "these names are classified in src/infra/db.rs but no longer fire \
         {NOTIFY_FUNCTION}: {stale:?}. Remove them from TRIGGERED_RESOURCE_TYPES — a \
         constant that is allowed to carry names the schema does not have is a \
         hand-maintained list again, which is the drift F52 records."
    );
}

/// The three tables `0003` renamed must not be on this channel.
///
/// Stated separately from the set comparison above even though that comparison already
/// covers them, because this is the *specific* regression and it has a specific cause
/// nobody expects: `ALTER TABLE … RENAME` carries triggers with it. A future migration
/// that renames a table out of service will re-introduce it unless someone knows to look,
/// and a failure naming the mechanism is worth more than a set difference that does not.
#[tokio::test]
async fn the_legacy_tables_renamed_by_0003_do_not_notify() {
    let Some(db) = TestDatabase::create().await else {
        return;
    };

    let actual = triggered_tables(&db).await;
    for table in [
        "legacy_providers",
        "legacy_routing_policies",
        "legacy_provider_credentials",
    ] {
        assert!(
            !actual.contains(table),
            "{table} still fires {NOTIFY_FUNCTION}. `alter table … rename` in \
             migrations/0003 carried the trigger across with the table, and the payload \
             carries tg_table_name, so every write to it announces a resource type the \
             classifier has never heard of and resets every circuit on every replica."
        );
    }

    // The control: the query can see triggers on tables that do have them, so the three
    // assertions above are not passing because nothing was found.
    assert!(
        actual.contains("providers"),
        "the live `providers` table is missing from the inventory, so the absence of the \
         legacy tables proves nothing about the query"
    );
}
