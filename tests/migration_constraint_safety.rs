//! Issue #250 finding 1 — a CHECK or FOREIGN KEY added to a request-rate table must not be
//! added in the validating form.
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
//! `0030` did not follow it, on `execution_attempts`, which is the highest-volume table in the
//! schema (one row per upstream provider attempt). Prose in `0027` did not stop that, so this is
//! the mechanical version.
//!
//! # What exactly is asserted, and the one thing it cannot assert
//!
//! Migrations are append-only here — `docs/project-structure.md:19`,
//! `migrations/0018_admin_identity_granted_by_invite.sql:20-21`, `tests/support/mod.rs:916` — so
//! `0030` cannot be repaired in place and its own scan is not undone by anything. What can be
//! fixed is the **current** definition of the constraint, which is what a fresh install ends on
//! and what the next author copies. So the rule is applied to the *last* migration that adds each
//! `(table, constraint)` pair: `0034` re-adds `execution_attempts_selection_reason_valid` in the
//! safe shape and is therefore the definition this test judges. Delete `0034` and this test goes
//! red naming `0030`.
//!
//! The residual — one deploy, on a database that crosses `0030` with a populated
//! `execution_attempts` — is recorded in `docs/release-notes.md` with the manual pre-step that
//! avoids it. A test cannot close that; only not having shipped `0030` could have.
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
    path::PathBuf,
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

fn migrations_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("migrations")
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

/// The last `add constraint` for each `(table, constraint)` pair across the whole history.
fn latest_constraint_definitions() -> BTreeMap<(String, String), AddConstraint> {
    let mut latest: BTreeMap<(String, String), AddConstraint> = BTreeMap::new();

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
            let table = tokens[2].to_string();
            latest.insert(
                (table.clone(), constraint.clone()),
                AddConstraint {
                    file: file.clone(),
                    table: table.clone(),
                    constraint,
                    not_valid: statement.ends_with(" not valid"),
                    statement_index: index,
                    file_leaves_transaction,
                    table_created_in_same_file: created_here.contains(&table),
                    validations_in_file: validations_in_file.clone(),
                },
            );
        }
    }

    latest
}

/// The rule, applied to every constraint whose current definition targets a request-rate table.
///
/// Failure here is not stylistic. It means the next deploy that runs the offending migration
/// holds ACCESS EXCLUSIVE on a table the whole fleet reads and writes, for as long as a full scan
/// of it takes.
#[test]
fn a_constraint_on_a_high_volume_table_is_added_not_valid_and_validated_separately() {
    let definitions = latest_constraint_definitions();

    let mut judged = 0usize;
    for definition in definitions.values() {
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
        } = definition;

        assert!(
            *not_valid,
            "{file} adds {constraint} to {table} with a validating ADD CONSTRAINT. {table} grows \
             with traffic, so that scan runs under ACCESS EXCLUSIVE and blocks every reader and \
             writer of it, fleet-wide, for its duration. Add it `not valid` and validate it in a \
             separate statement — see migrations/0027_content_encryption_keyring.sql:12-36. If \
             the offending migration has already shipped, it cannot be edited; append one that \
             re-adds the constraint in the safe shape, as 0034 does for 0030."
        );
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
        judged >= 6,
        "only {judged} constraint definitions were judged, which is fewer than the six this tree \
         is known to have (0027's five plus execution_attempts_selection_reason_valid). The \
         parser has stopped seeing statements it used to see, and a rule that matches nothing \
         passes silently"
    );
}

/// The two facts the test above depends on, pinned so a parser regression cannot quietly turn it
/// into a no-op.
///
/// The first is the finding itself: `execution_attempts_selection_reason_valid` must currently be
/// defined by `0034`, not by `0030`. The second is a known-good case, so "everything passes"
/// cannot be achieved by a parser that recognises nothing.
#[test]
fn the_known_cases_are_the_ones_the_rule_is_reading() {
    let definitions = latest_constraint_definitions();

    let selection_reason = definitions
        .get(&(
            "execution_attempts".to_string(),
            "execution_attempts_selection_reason_valid".to_string(),
        ))
        .expect("execution_attempts_selection_reason_valid must be defined by some migration");
    assert!(
        selection_reason.file.starts_with("0034_"),
        "the current definition of execution_attempts_selection_reason_valid comes from {}, not \
         from the 0034 correction. 0030 added it in the validating form and cannot be edited, so \
         removing the correction leaves the hazard as the shape a reader will copy",
        selection_reason.file
    );

    let single_form = definitions
        .get(&(
            "conversation_messages".to_string(),
            "conversation_messages_content_single_form".to_string(),
        ))
        .expect("0027's exclusivity CHECK must be visible to the parser");
    assert!(
        single_form.not_valid && single_form.file_leaves_transaction,
        "0027's constraint is the reference implementation of the safe shape; if the parser reads \
         it as unsafe, the rule above is testing its own bugs"
    );
}
