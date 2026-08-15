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
-- **It does not make `0030` cheap.** A database that has not yet applied `0030` still applies it
-- first and still eats that scan under ACCESS EXCLUSIVE. Nothing appended after `0030` can change
-- what `0030` does; only editing `0030` could, and merged migrations are not edited in this
-- repository — `docs/project-structure.md:19`, `migrations/0018_admin_identity_granted_by_invite
-- .sql:20-21` ("`0012` is merged and is not edited"), and `tests/support/mod.rs:916` ("Editing
-- 0003 is not an option either — it is already applied to every existing database and `sqlx`
-- checksums migration files"). Changing the bytes of a shipped file makes every database that
-- already applied it refuse to boot, loudly and with no in-band remedy, while CI — which always
-- starts from an empty database — stays green. That trade is not worth taking to save a scan on
-- a table that is empty on every install created after `0030` shipped.
--
-- The residual is therefore one deploy: a database that crosses `0030` with a populated
-- `execution_attempts`. It is written down in `docs/release-notes.md` under "Unreleased", with
-- the manual pre-step that avoids it, rather than left for an operator to discover from a stalled
-- fleet.
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
-- constraint is installed, never *what* it admits — and because `0030` already validated it,
-- there is no row anywhere that the re-validation below can fail on.

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
