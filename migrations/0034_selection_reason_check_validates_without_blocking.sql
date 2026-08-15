-- no-transaction
-- Issue #250 finding 1 — `0030` added `execution_attempts_selection_reason_valid` with a bare,
-- validating `ADD CONSTRAINT`.
--
-- `execution_attempts` is the highest-volume table in the schema: `MoiraExecutionService` writes
-- one row per upstream provider attempt (`insert_attempt_started`, `src/application/execution.rs`,
-- and `docs/retry-and-fallback.md`: "Each upstream attempt is persisted separately"), and the
-- usage and attempt read endpoints select from it. A bare `ADD CONSTRAINT` takes ACCESS EXCLUSIVE
-- *and* performs the validating scan while holding it, and the lock is on the table rather than
-- on the migrating replica — so on a database with a real attempt history every execution in the
-- fleet blocks for the length of that scan.
--
-- `migrations/0027_content_encryption_keyring.sql:12-36` states the pattern this repository uses
-- for exactly that hazard, and states why each half is load-bearing: `-- no-transaction` on the
-- first line, `ADD CONSTRAINT ... NOT VALID` (ACCESS EXCLUSIVE, no scan), a commit, and then
-- `VALIDATE CONSTRAINT` (SHARE UPDATE EXCLUSIVE, scans, blocks neither readers nor writers). The
-- split is worth nothing inside one transaction, because locks are held to commit; that is why
-- this file leaves the runner's transaction rather than merely writing two statements.
--
-- ===================================================================================
-- What this migration buys, and what it does not
-- ===================================================================================
--
-- It re-installs the same constraint in that shape. The last definition of
-- `execution_attempts_selection_reason_valid` in the migration history — the one a fresh install
-- ends on, and the one the next author reads when they copy a CHECK onto a hot table — is now the
-- safe one, and `tests/migration_constraint_safety.rs` pins that mechanically for every future
-- `add constraint` against a high-volume table.
--
-- **It does not make `0030` cheap.** As far as the migration set is concerned, a database that has
-- not yet applied `0030` still applies it first and still eats that scan under ACCESS EXCLUSIVE.
-- Nothing appended after `0030` can change what `0030` does — the paragraph after next names the
-- thing that can, and it is not a migration. Only editing `0030` could, and merged migrations are
-- not edited in this repository — `docs/project-structure.md:19`,
-- `migrations/0018_admin_identity_granted_by_invite
-- .sql:20-21` ("`0012` is merged and is not edited"), and `tests/support/mod.rs:916` ("Editing
-- 0003 is not an option either — it is already applied to every existing database and `sqlx`
-- checksums migration files"). Changing the bytes of a shipped file makes every database that
-- already applied it refuse to boot, loudly and with no in-band remedy, while CI — which always
-- starts from an empty database — stays green. That trade is not worth taking to save a scan on
-- a table that is empty on every install created after `0030` shipped.
--
-- **What actually keeps `0030` from blocking is `src/infra/migration_preflight.rs`**, because the
-- only code that runs *before* the migrator reaches `0030` is the code that calls the migrator.
-- `db::migrate` pre-applies `0030`'s own three statements in this same order and records `0030` as
-- applied, so `Migrator::run` skips it. This file remains the correction to the *definition* — the
-- shape a fresh install ends on and the next author copies, pinned by
-- `tests/migration_constraint_safety.rs`. The residual it does not cover is a deployment that
-- migrates with `sqlx-cli` rather than a Moira process; `docs/release-notes.md` keeps the manual
-- pre-step for that.
--
-- **This migration costs one full scan of `execution_attempts` on every database that already has
-- the constraint.** `drop constraint` / `add … not valid` clears `pg_constraint.convalidated`, so
-- the `VALIDATE CONSTRAINT` below re-scans the table even though every row already satisfies the
-- check — PostgreSQL skips validation only for a constraint already *marked* valid, which this
-- file has just un-marked. The scan takes SHARE UPDATE EXCLUSIVE and blocks nothing, but on a
-- large table it is minutes of I/O and it is not a no-op. That is the price of having one
-- unconditional, idempotent, plain-statement definition rather than a `DO` block that a reader —
-- and `tests/migration_constraint_safety.rs`'s parser — would have to reason about.
--
-- ===================================================================================
-- Idempotence, which `-- no-transaction` makes mandatory
-- ===================================================================================
--
-- Outside the runner's transaction, a failure part-way through leaves the earlier groups applied
-- and the migration unrecorded, so the next boot re-runs the file from the top. Every statement
-- below survives that: `drop constraint if exists` / `add constraint` is `0020`'s pairing, and
-- `VALIDATE CONSTRAINT` against an already-validated constraint is a no-op.
--
-- The constraint text is character-for-character `0030`'s. This migration changes *how* the
-- constraint is installed, never *what* it admits — and because the constraint was already
-- enforced before this file runs (by `0030`, or by the preflight that pre-applies it), there is no
-- row anywhere that the re-validation below can fail on.

begin;

alter table execution_attempts
    drop constraint if exists execution_attempts_selection_reason_valid;

alter table execution_attempts
    add constraint execution_attempts_selection_reason_valid check (
        selection_reason is null
        or selection_reason in ('priority', 'explicit_hint', 'scored', 'fallback_after_failure')
    ) not valid;

commit;

-- The scan. SHARE UPDATE EXCLUSIVE only, taken after the commit above released the ACCESS
-- EXCLUSIVE — which is the whole reason this file leaves the runner's transaction. Inserts into
-- `execution_attempts` keep working throughout, which is the property the split exists for.
begin;

alter table execution_attempts
    validate constraint execution_attempts_selection_reason_valid;

commit;
