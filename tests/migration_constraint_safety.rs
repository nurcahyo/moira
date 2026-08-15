//! Issue #250 finding 1 — a CHECK or FOREIGN KEY added to a request-rate table must not be added
//! in the validating form, and one that already shipped in it must be defused before the migrator
//! can reach it.
//!
//! # The hazard, in one paragraph
//!
//! `ALTER TABLE … ADD CONSTRAINT` takes ACCESS EXCLUSIVE **and** runs the validating scan while
//! holding it. The lock is on the table, not on the migrating replica, so on a table that grows
//! with traffic every reader and writer in the fleet blocks for the length of that scan — during
//! a rolling deploy, from a migration that looks like a one-liner.
//! `migrations/0027_content_encryption_keyring.sql:12-36` states the alternative and why each
//! half of it is load-bearing: `-- no-transaction` on the first line, `ADD CONSTRAINT … NOT VALID`
//! (ACCESS EXCLUSIVE, no scan), a commit, then `VALIDATE CONSTRAINT` (SHARE UPDATE EXCLUSIVE,
//! scans, blocks nobody). Inside one transaction the split is decoration, because locks are held
//! to commit — which is why the `-- no-transaction` line is checked here and not assumed.
//!
//! # What the first version of this file got wrong, because it matters
//!
//! It judged only the **last** `add constraint` for each `(table, constraint)` pair. `0030` adds
//! `execution_attempts_selection_reason_valid` in the validating form and `0034` re-adds it in the
//! safe one, so under that rule `0030` was invisible: the test's verdict did not depend on `0030`'s
//! text at all, and would have been identical had the hazard never existed. It asserted that a
//! file had been added, not that a hazard had been removed.
//!
//! So the rule below judges **every** `add constraint` statement in the history. `0030` fails it,
//! and it is not editable — `docs/project-structure.md:19`,
//! `migrations/0018_admin_identity_granted_by_invite.sql:20-21`, `src/test_support.rs`'s
//! `VersionMismatch` diagnosis: `sqlx` checksums migration files, so a database that applied the
//! old bytes would refuse to boot. A shipped hazard therefore has exactly one honest disposition,
//! and it is the one [`DEFUSED_BEFORE_THE_MIGRATOR_RUNS_THEM`] encodes: something that runs
//! **before** the migrator must make sure the statement never executes. That something is
//! `src/infra/migration_preflight.rs`, and this file checks it is really there and really names
//! the migration it claims to defuse. Delete the preflight and this test reds naming `0030` — not
//! naming a missing follow-up migration.
//!
//! # Two deliberate exemptions
//!
//! * **Configuration tables.** `0018` and `0020` add validating CHECKs to `admin_identities` and
//!   `auth_provider_settings`. Those hold single to double digits of rows and always will; the
//!   scan is free and demanding the two-phase dance there would be ceremony. Only tables whose
//!   row count grows with *traffic or content* are in [`HIGH_VOLUME_TABLES`].
//! * **A table created by the same migration.** `0007` adds two FKs to tables it creates a few
//!   hundred lines earlier. Those tables are empty at that instant, so there is nothing to scan
//!   and nothing to block.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

/// Tables whose row count grows with traffic or stored content rather than with configuration.
///
/// The test is only as good as this list, so the membership rule is stated rather than left to
/// taste: a table belongs here if something other than an operator writes it — the execution
/// path, the ingestion path, a worker, or an audit trail. A table an admin writes by hand does
/// not, however important it is.
///
/// Erring towards inclusion is close to free: the cost of a false positive is that one future
/// migration writes three statements instead of one, and the cost of a false negative is the
/// outage this file exists to prevent.
const HIGH_VOLUME_TABLES: &[&str] = &[
    "agent_flow_runs",
    "agent_flow_step_runs",
    "audit_events",
    "audit_logs",
    "context_plans",
    "conversation_messages",
    "conversation_summaries",
    "conversations",
    "eval_runs",
    "execution_attempts",
    "idempotency_records",
    "memory_embeddings",
    "memory_extraction_runs",
    "memory_records",
    "provider_health_snapshots",
    "rag_chunk_embeddings",
    "rag_chunks",
    "rag_document_versions",
    "rag_documents",
    "rag_ingestion_runs",
    "responses",
    "retrieval_runs",
    "usage_records",
    "worker_jobs",
];

/// A statement that shipped in the validating form and is neutralised by code that runs before
/// the migrator does.
struct ShippedHazard {
    /// The migration file the statement lives in.
    file: &'static str,
    /// Its `sqlx` version number — the number the preflight has to key off.
    version: i64,
    table: &'static str,
    constraint: &'static str,
    /// The module that guarantees the statement never runs against a populated table.
    defused_by: &'static str,
}

/// **Entries here are not exemptions. They are debts, and each one has to be paid by code.**
///
/// A validating `ADD CONSTRAINT` on a hot table that has already shipped cannot be edited away.
/// The only remaining place to stand is before the migrator, so an entry is admitted only when
/// [`every_shipped_hazard_is_really_defused`] can find the named module and see it naming this
/// migration back. Nothing in this list is satisfied by prose, by a follow-up migration, or by a
/// release note.
///
/// The list is one long and [`the_list_of_shipped_hazards_has_not_grown`] keeps it that way, so a
/// second entry is a decision somebody makes on purpose rather than a line that slips in.
const DEFUSED_BEFORE_THE_MIGRATOR_RUNS_THEM: &[ShippedHazard] = &[ShippedHazard {
    file: "0030_execution_attempt_candidate_observability.sql",
    version: 30,
    table: "execution_attempts",
    constraint: "execution_attempts_selection_reason_valid",
    defused_by: "src/infra/migration_preflight.rs",
}];

/// One `alter table … add constraint …`, with everything needed to judge it.
#[derive(Debug, Clone)]
struct AddConstraint {
    file: String,
    table: String,
    constraint: String,
    /// The statement ends in `not valid`, so it takes ACCESS EXCLUSIVE for a metadata write and
    /// performs no scan.
    not_valid: bool,
    /// Position of the statement within its file, so a `validate` can be required to come after
    /// the `add` rather than merely to exist.
    statement_index: usize,
    /// `-- no-transaction` on the first line of the file. Without it the two statements share one
    /// transaction, the lock is held to commit, and the split buys nothing.
    file_leaves_transaction: bool,
    /// The same file creates the table, so it is empty when the constraint lands.
    table_created_in_same_file: bool,
    /// `(constraint name, statement index)` for every `validate constraint` in the same file.
    validations_in_file: Vec<(String, usize)>,
}

fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn migrations_dir() -> PathBuf {
    repository_root().join("migrations")
}

/// Comment lines are dropped before parsing.
///
/// This repository's migrations carry long prose headers that quote SQL — `0027`, `0034` and this
/// file's own subject matter all contain the words "add constraint" in commentary. Parsing them
/// as statements would make the test assert on paragraphs.
fn strip_comments(sql: &str) -> String {
    sql.lines()
        .filter(|line| !line.trim_start().starts_with("--"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Lowercased, whitespace-collapsed statements, in file order.
fn statements(sql: &str) -> Vec<String> {
    strip_comments(sql)
        .split(';')
        .map(|statement| statement.split_whitespace().collect::<Vec<_>>().join(" "))
        .map(|statement| statement.to_lowercase())
        .filter(|statement| !statement.is_empty())
        .collect()
}

/// The token after `add constraint` / `validate constraint` / `create table if not exists`,
/// stripped of anything that is not part of an identifier.
fn name_after(tokens: &[&str], first: &str, second: &str) -> Option<String> {
    tokens.windows(3).find_map(|window| {
        (window[0] == first && window[1] == second).then(|| {
            window[2]
                .trim_matches(|c: char| !c.is_alphanumeric() && c != '_')
                .to_string()
        })
    })
}

fn read_migrations() -> Vec<(String, String)> {
    let mut files: Vec<_> = fs::read_dir(migrations_dir())
        .expect("read the migrations directory")
        .map(|entry| entry.expect("read a migrations directory entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "sql"))
        .collect();
    files.sort();
    assert!(
        files.len() > 25,
        "the migrations directory resolved to {} files, which cannot be right — a parser that \
         reads nothing passes every assertion below",
        files.len()
    );
    files
        .into_iter()
        .map(|path| {
            let name = path
                .file_name()
                .expect("a migration file name")
                .to_string_lossy()
                .to_string();
            (name, fs::read_to_string(&path).expect("read a migration"))
        })
        .collect()
}

/// The table a `create table` statement creates, in either form this tree writes.
///
/// A table created by the same migration that constrains it is empty at that moment, which is the
/// exemption this feeds.
fn created_table_name(statement: &str, tokens: &[&str]) -> Option<String> {
    if statement.starts_with("create table if not exists ") {
        return name_after(tokens, "not", "exists");
    }
    if statement.starts_with("create table ") {
        return tokens.get(2).map(|table| (*table).to_string());
    }
    None
}

/// **Every** `add constraint` in the history, in file order — not one per `(table, constraint)`.
///
/// Keeping every occurrence is the correction described in this file's header. A later migration
/// that re-adds the same constraint safely repairs the definition a fresh install ends on; it does
/// not repair the earlier statement, which still executes on every database that has not yet
/// crossed it.
fn every_add_constraint() -> Vec<AddConstraint> {
    let mut found = Vec::new();

    for (file, sql) in read_migrations() {
        let file_leaves_transaction = sql.starts_with("-- no-transaction");
        let statements = statements(&sql);

        let mut created_here: BTreeSet<String> = BTreeSet::new();
        let mut validations_in_file: Vec<(String, usize)> = Vec::new();
        for (index, statement) in statements.iter().enumerate() {
            let tokens: Vec<&str> = statement.split(' ').collect();
            if let Some(table) = created_table_name(statement, &tokens) {
                created_here.insert(table);
            }
            if let Some(name) = name_after(&tokens, "validate", "constraint") {
                validations_in_file.push((name, index));
            }
        }

        for (index, statement) in statements.iter().enumerate() {
            if !statement.starts_with("alter table ") {
                continue;
            }
            let tokens: Vec<&str> = statement.split(' ').collect();
            let Some(constraint) = name_after(&tokens, "add", "constraint") else {
                continue;
            };
            let Some(table) = tokens.get(2).map(|table| (*table).to_string()) else {
                continue;
            };
            found.push(AddConstraint {
                file: file.clone(),
                table: table.clone(),
                constraint,
                not_valid: statement.ends_with(" not valid"),
                statement_index: index,
                file_leaves_transaction,
                table_created_in_same_file: created_here.contains(&table),
                validations_in_file: validations_in_file.clone(),
            });
        }
    }

    found
}

fn shipped_hazard(definition: &AddConstraint) -> Option<&'static ShippedHazard> {
    DEFUSED_BEFORE_THE_MIGRATOR_RUNS_THEM.iter().find(|hazard| {
        hazard.file == definition.file
            && hazard.table == definition.table
            && hazard.constraint == definition.constraint
    })
}

/// The rule, applied to every `add constraint` against a request-rate table, everywhere in the
/// history.
///
/// Failure here is not stylistic. It means a deploy that runs the offending migration holds ACCESS
/// EXCLUSIVE on a table the whole fleet reads and writes, for as long as a full scan of it takes.
#[test]
fn every_constraint_on_a_high_volume_table_is_added_not_valid_and_validated_separately() {
    let mut judged = 0usize;
    let mut hazards_seen: BTreeSet<(String, String, String)> = BTreeSet::new();

    for definition in every_add_constraint() {
        if !HIGH_VOLUME_TABLES.contains(&definition.table.as_str()) {
            continue;
        }
        if definition.table_created_in_same_file {
            continue;
        }
        judged += 1;

        let AddConstraint {
            file,
            table,
            constraint,
            not_valid,
            statement_index,
            file_leaves_transaction,
            validations_in_file,
            ..
        } = &definition;

        if !not_valid {
            let hazard = shipped_hazard(&definition);
            assert!(
                hazard.is_some(),
                "{file} adds {constraint} to {table} with a validating ADD CONSTRAINT. {table} \
                 grows with traffic, so that scan runs under ACCESS EXCLUSIVE and blocks every \
                 reader and writer of it, fleet-wide, for its duration. Add it `not valid` and \
                 validate it in a separate statement — see \
                 migrations/0027_content_encryption_keyring.sql:12-36.\n\nIf this migration has \
                 already shipped it cannot be edited, and appending a migration that re-adds the \
                 constraint safely does NOT help: the appended one runs after this one has \
                 already taken the lock. The statement has to be stopped before the migrator \
                 reaches it — see src/infra/migration_preflight.rs — and then declared in \
                 DEFUSED_BEFORE_THE_MIGRATOR_RUNS_THEM in this file."
            );
            hazards_seen.insert((file.clone(), table.clone(), constraint.clone()));
            continue;
        }

        assert!(
            *file_leaves_transaction,
            "{file} adds {constraint} `not valid` but does not start with `-- no-transaction`, so \
             the ADD and the VALIDATE share one transaction. Locks are held to commit, so ACCESS \
             EXCLUSIVE is held across the scan anyway and the split is decoration — the exact \
             trap migrations/0027_content_encryption_keyring.sql:26-30 warns about."
        );
        let validated_after = validations_in_file
            .iter()
            .any(|(name, index)| name == constraint && index > statement_index);
        assert!(
            validated_after,
            "{file} adds {constraint} to {table} `not valid` and never validates it, so the \
             constraint is enforced on new rows but never proven against the existing ones. Add \
             `alter table {table} validate constraint {constraint};` after the commit boundary."
        );
    }

    assert!(
        judged >= 7,
        "only {judged} add-constraint statements against high-volume tables were judged, which is \
         fewer than the seven this tree is known to have (0027's five, 0030's and 0034's). The \
         parser has stopped seeing statements it used to see, and a rule that matches nothing \
         passes silently"
    );
    assert_eq!(
        hazards_seen.len(),
        DEFUSED_BEFORE_THE_MIGRATOR_RUNS_THEM.len(),
        "DEFUSED_BEFORE_THE_MIGRATOR_RUNS_THEM lists {} shipped hazards but the parser found {} \
         validating statements to match them against. An entry that matches nothing is an \
         exemption for a statement that no longer exists, and it will silently cover the next one \
         that takes its place",
        DEFUSED_BEFORE_THE_MIGRATOR_RUNS_THEM.len(),
        hazards_seen.len()
    );
}

/// The other half of the rule above: the debt is only cancelled if the code that cancels it is
/// there.
///
/// This is what stops the list from becoming an allowlist. It reds if the preflight module is
/// deleted, renamed, or stops naming the migration and the constraint it claims to defuse — and
/// [`every_constraint_on_a_high_volume_table_is_added_not_valid_and_validated_separately`] then
/// has nothing left to lean on either.
#[test]
fn every_shipped_hazard_is_really_defused() {
    for hazard in DEFUSED_BEFORE_THE_MIGRATOR_RUNS_THEM {
        let path = repository_root().join(hazard.defused_by);
        let source = read_defusing_module(&path, hazard);

        assert!(
            source.contains(&hazard.version.to_string()),
            "{} claims to defuse {} but never mentions version {}. The preflight has to key off \
             the version number the migrator uses, or it defuses nothing",
            hazard.defused_by,
            hazard.file,
            hazard.version
        );
        assert!(
            source.contains(hazard.constraint),
            "{} claims to defuse {} but never mentions {}, the constraint whose validating ADD is \
             the hazard",
            hazard.defused_by,
            hazard.file,
            hazard.constraint
        );
        assert!(
            source.contains("not valid") && source.contains("validate constraint"),
            "{} must install {} with ADD CONSTRAINT … NOT VALID and a separate VALIDATE \
             CONSTRAINT. Anything else re-creates the scan it exists to avoid",
            hazard.defused_by,
            hazard.constraint
        );
        assert!(
            fs::read_to_string(repository_root().join("src/infra/db.rs"))
                .expect("read src/infra/db.rs")
                .contains("defuse_pending_hot_table_constraints"),
            "src/infra/db.rs::migrate must call the preflight. A preflight nothing calls is the \
             same as no preflight, and every migrating entry point in this tree goes through that \
             one function"
        );

        let migration = migrations_dir().join(hazard.file);
        assert!(
            migration.is_file(),
            "{} names {} but that migration does not exist. If it was renumbered, the preflight \
             is keyed to a version that will never be applied",
            hazard.defused_by,
            hazard.file
        );
    }
}

fn read_defusing_module(path: &Path, hazard: &ShippedHazard) -> String {
    fs::read_to_string(path).unwrap_or_else(|error| {
        panic!(
            "{} is declared as the code that stops {}'s validating ADD CONSTRAINT on {} from \
             running, and it cannot be read: {error}.\n\nWithout it, every database that has not \
             yet applied {} takes ACCESS EXCLUSIVE on {} and holds it across a full scan.",
            hazard.defused_by, hazard.file, hazard.table, hazard.file, hazard.table
        )
    })
}

/// A ratchet, not an assertion about correctness.
///
/// Every entry costs a permanent piece of production code that has to keep working. One is a
/// repair; a growing list is a habit.
#[test]
fn the_list_of_shipped_hazards_has_not_grown() {
    assert_eq!(
        DEFUSED_BEFORE_THE_MIGRATOR_RUNS_THEM.len(),
        1,
        "a second shipped validating constraint on a hot table has been admitted. That is a \
         decision, not a formality: it means another migration went out in a shape this test \
         exists to prevent, and another preflight now has to run before every migration forever. \
         Raise this number deliberately, with the reason in the commit message"
    );
}

/// The cases the rule depends on being visible, pinned so a parser regression cannot quietly turn
/// it into a no-op.
///
/// `0030`'s statement must still be read as **unsafe** — if the parser ever reads it as `not
/// valid`, the whole rule above is satisfied by a bug. `0027`'s is the known-good case, so
/// "everything passes" cannot be achieved by a parser that recognises nothing.
#[test]
fn the_known_cases_are_the_ones_the_rule_is_reading() {
    let all = every_add_constraint();
    let by_key = |file: &str, constraint: &str| {
        all.iter()
            .find(|definition| {
                definition.file.starts_with(file) && definition.constraint == constraint
            })
            .unwrap_or_else(|| panic!("{file} must contain an add-constraint for {constraint}"))
    };

    let shipped = by_key("0030_", "execution_attempts_selection_reason_valid");
    assert!(
        !shipped.not_valid,
        "0030's ADD CONSTRAINT is being read as `not valid`. It is not — it is the validating \
         form, and it is the reason src/infra/migration_preflight.rs exists. A parser that reads \
         it as safe makes the rule above vacuous"
    );
    assert!(
        !shipped.table_created_in_same_file,
        "0030 does not create execution_attempts (0005 does), so the empty-table exemption must \
         not apply to it"
    );

    let reference = by_key("0027_", "conversation_messages_content_single_form");
    assert!(
        reference.not_valid && reference.file_leaves_transaction,
        "0027's constraint is the reference implementation of the safe shape; if the parser reads \
         it as unsafe, the rule above is testing its own bugs"
    );

    let correction = by_key("0034_", "execution_attempts_selection_reason_valid");
    assert!(
        correction.not_valid && correction.file_leaves_transaction,
        "0034 re-installs the constraint in the safe shape and must be read as safe"
    );
}

/// Sanity on the exemption for configuration tables: it must be a decision about the *table*, not
/// a hole big enough for the hot ones to fall through.
#[test]
fn the_high_volume_table_list_still_covers_the_tables_the_schema_has() {
    let mut constrained: BTreeMap<String, usize> = BTreeMap::new();
    for definition in every_add_constraint() {
        *constrained.entry(definition.table).or_default() += 1;
    }
    assert!(
        constrained.contains_key("execution_attempts"),
        "execution_attempts must be visible to the parser as a constrained table"
    );
    assert!(
        HIGH_VOLUME_TABLES.contains(&"execution_attempts"),
        "execution_attempts is one row per upstream provider attempt and must stay on the \
         high-volume list"
    );
}
