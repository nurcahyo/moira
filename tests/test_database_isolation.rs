//! Finding F27 — no integration suite may quietly adopt the shared test database.
//!
//! `support::TestDatabase` gives every fixture a private database cloned from the migrated
//! template and drops it in `Drop`, including while a test is unwinding from a panic. A
//! suite that instead resolves `MOIRA_TEST_DATABASE_URL` itself and opens its own pool
//! opts out of that entirely: whatever it writes is permanent, shared with every other
//! suite and with all of its own future runs.
//!
//! That is not hypothetical. `tests/jwks_hardening.rs` did exactly this and leaked ten
//! `trusted_jwt_issuers` rows per run — **160** of them by the time it was measured, every
//! one on the happy path, with the panic path leaking the same rows for the same reason.
//! `tests/http_middleware_contract.rs` did the same with one `active` `moira:admin`
//! `system_api_keys` row per run (finding F10 item 2), 42 of them. Both now use
//! `TestDatabase`, and each carries a `the_fixture_owns_a_disposable_database` test that
//! pins its own fixture.
//!
//! **This file is the part those two cannot cover.** A per-fixture assertion only sees the
//! fixture it was given; it says nothing about a *new* suite, or a new test in an existing
//! suite, that opens its own pool. The cheapest way to reintroduce F27 is to write one, and
//! nothing in a fixture-scoped guard would notice. So this scans the sources instead.
//!
//! **What it establishes.** That the set of files resolving `MOIRA_TEST_DATABASE_URL`
//! themselves is exactly the allowlist below, each entry carrying the reason it cannot use
//! a disposable clone. Adding a suite to that set is then a visible, reviewable diff rather
//! than a silent one.
//!
//! The allowlist has since shrunk by one. `tests/retention_worker.rs` was on it because
//! `retention::run_once` sweeps every expired row in the database it is connected to, so on
//! a shared database its delete counts included other suites' rows. That entry described a
//! real constraint and a wrong conclusion: the sweep is *database*-wide, never
//! cluster-wide, and it is database-wide on a private clone over the identical code path.
//! Moving the suite (finding F10 item 1) made its counts exactly assertable instead of
//! weakening them, and this file's second test is what forced the entry to be deleted
//! rather than left to rot.
//!
//! **`src/` is scanned too, since issue #77.** It was not, and that was the hole the whole
//! of #77 problem 3 came through: nine `#[cfg(test)]` modules under `src/**` resolved the
//! variable and ran `MIGRATOR` against the database it names, so one checkout's unmerged
//! migration broke every other checkout's lib tests — including `main`'s — with a message
//! naming a version its author had never seen. A guard that watched `tests/` only could
//! never have seen it. The library's tests now go through `src/test_support.rs`, and that
//! file is the single allowlisted entry for the whole of `src/`.
//!
//! **What it does not.** It matches source text, not behaviour: a suite that reached the
//! same URL indirectly — through a helper, a second environment variable, or a `var(` call
//! rustfmt happened to split across lines — would pass. It also cannot tell whether an
//! allowlisted suite *leaks*, only that its access is declared.

use std::{collections::BTreeSet, fs, path::PathBuf};

/// Files permitted to resolve `MOIRA_TEST_DATABASE_URL` themselves, each with the reason a
/// disposable clone is not available to it. Paths are relative to the crate root.
///
/// Entries are liabilities, not exemptions: `every_allowlist_entry_is_still_load_bearing`
/// fails if one stops being needed, so the list can only shrink without review.
const SHARED_DATABASE_ALLOWLIST: &[(&str, &str)] = &[
    (
        "tests/support/mod.rs",
        "owns the mechanism — it resolves the variable to build the maintenance, template \
         and per-fixture clone URLs that every other suite borrows",
    ),
    (
        "tests/security_foundation.rs",
        "asserts the migration contract itself, so it must apply migrations to a database \
         built from nothing rather than to a clone of an already-migrated template; it \
         already creates and force-drops its own database per run",
    ),
    (
        "src/test_support.rs",
        "owns the same mechanism for the library crate's own #[cfg(test)] modules — it \
         resolves the variable for its host and credentials only, and creates one private \
         database per test process rather than connecting to the one the URL names",
    ),
];

/// The roots scanned, each with a floor on how many `.rs` files it must yield.
///
/// Without a floor a mistyped directory, a `read_dir` that silently yielded nothing, or a
/// future move of this file would leave every assertion below iterating an empty set and
/// passing — the shape of a guard this project has already been bitten by six times. The
/// floors are **per root** on purpose: one number over both would let a broken `src/` scan
/// hide behind the size of `tests/`.
const SCANNED_ROOTS: &[(&str, usize)] = &[("tests", 30), ("src", 30)];

/// The call that hands a suite the shared database URL. Matching the `var("…")` call rather
/// than the bare variable name keeps the many suites that only *mention* it — in a doc
/// comment, or in a skip message — out of the result.
///
/// Assembled at runtime rather than written as a literal so that this file is scanned on
/// exactly the same terms as every other: spelled out here, the guard would be its own
/// first violation, and the only ways out are to special-case itself — which is a hole
/// someone can later hide a suite in — or to weaken the pattern.
fn shared_database_lookup() -> String {
    format!("var({:?})", "MOIRA_TEST_DATABASE_URL")
}

/// Drops whole-line comments so a `//!`, `///` or `//` line quoting the lookup is not
/// mistaken for a live one. Lines with trailing comments are kept intact: a false positive
/// there is a loud, easily-corrected failure, while stripping to the first `//` would
/// corrupt any line holding a `postgres://…` literal.
fn code_only(source: &str) -> String {
    source
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Every `.rs` file under each of [`SCANNED_ROOTS`], keyed by its path relative to the
/// crate root.
fn test_sources() -> Vec<(String, String)> {
    let crate_root = crate_root();
    let mut found = Vec::new();

    for (root_name, minimum) in SCANNED_ROOTS {
        let root = crate_root.join(root_name);
        let mut pending = vec![root.clone()];
        let mut in_this_root = 0usize;

        while let Some(directory) = pending.pop() {
            let entries = fs::read_dir(&directory)
                .unwrap_or_else(|err| panic!("read the directory {}: {err}", directory.display()));
            for entry in entries {
                let path = entry.expect("read a directory entry").path();
                if path.is_dir() {
                    pending.push(path);
                    continue;
                }
                if path.extension().is_none_or(|extension| extension != "rs") {
                    continue;
                }
                let name = path
                    .strip_prefix(&crate_root)
                    .expect("every scanned path is under the crate root")
                    .to_string_lossy()
                    .replace('\\', "/");
                let source = fs::read_to_string(&path)
                    .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
                found.push((name, source));
                in_this_root += 1;
            }
        }

        assert!(
            in_this_root >= *minimum,
            "expected at least {minimum} sources under {}, found {in_this_root} — the scan \
             is not reaching that tree, so every assertion in this file would pass \
             vacuously for it",
            root.display()
        );
    }

    found
}

/// The files that resolve the shared database URL themselves.
fn suites_opening_the_shared_database() -> BTreeSet<String> {
    let needle = shared_database_lookup();
    test_sources()
        .into_iter()
        .filter(|(_, source)| code_only(source).contains(&needle))
        .map(|(name, _)| name)
        .collect()
}

#[test]
fn no_unlisted_suite_resolves_the_shared_test_database_url() {
    let allowed: BTreeSet<String> = SHARED_DATABASE_ALLOWLIST
        .iter()
        .map(|(name, _)| (*name).to_string())
        .collect();
    let actual = suites_opening_the_shared_database();

    let unlisted: Vec<&String> = actual.difference(&allowed).collect();
    let needle = shared_database_lookup();
    assert!(
        unlisted.is_empty(),
        "these test sources resolve {needle} and so write to the shared \
         database that every suite and every previous run share: {unlisted:?}.\n\nUse \
         `support::TestDatabase::create()` (or a `LifecycleFixture`, which owns one) — it \
         clones the migrated template and drops the database in `Drop`, on the panic path \
         as well as the happy one. Rows written to the shared database are never cleaned \
         up by anything: that is finding F27, which had accumulated 160 \
         `trusted_jwt_issuers` rows and 42 active `moira:admin` API keys when it was \
         measured.\n\nUnder `src/`, use `crate::test_support::test_database()` instead — a \
         `#[cfg(test)]` module that migrates the shared database applies an unmerged \
         migration to it and breaks every other checkout's lib tests, which is issue \
         #77.\n\nIf a file genuinely cannot use a private database, add it to \
         SHARED_DATABASE_ALLOWLIST in this file with the reason."
    );
}

/// The other half: an allowlist that outlives its reasons is how an exemption list rots
/// into a blanket permission. An entry that no longer describes a real access silently
/// pre-authorises the next file to take that name.
#[test]
fn every_allowlist_entry_is_still_load_bearing() {
    let actual = suites_opening_the_shared_database();
    let needle = shared_database_lookup();

    for (name, reason) in SHARED_DATABASE_ALLOWLIST {
        assert!(
            actual.contains(*name),
            "`{name}` is on SHARED_DATABASE_ALLOWLIST but no longer resolves \
             {needle}. Delete the entry — a stale exemption pre-authorises \
             whatever file next takes that name. Recorded reason: {reason}"
        );
        assert!(
            !reason.trim().is_empty(),
            "`{name}` must record why it cannot use a disposable database"
        );
    }
}
