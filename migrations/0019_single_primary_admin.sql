-- Plan 09 finding F20 — on every deployment created after `0017`, no admin was ever primary.
--
-- `0017` made ownership row state and bootstrapped it with a one-shot, migration-time
-- `UPDATE`. On a greenfield deployment that `UPDATE` runs against an **empty**
-- `admin_identities`, and nothing in the application ever writes `is_primary` except
-- `PATCH /api/v1/admin/admin-identities/{id}`, which requires the caller to be primary
-- already. So ownership bootstrapped only on deployments that already had a claimant when
-- `0017` ran. Everywhere else `admin_identity_last_primary` guarded a permanently empty
-- set, and the transfer endpoint was reachable only through the system-key break-glass
-- path — which is the credential the invitation flow exists to let an operator retire.
--
-- ---------------------------------------------------------------------------------
-- Decision D-F20 (user, 2026-07-31): ownership is a SINGLE primary, set at claim time.
-- ---------------------------------------------------------------------------------
-- The grant that first flips `setup_state.claimed` becomes primary automatically, so a
-- fresh deployment has an owner without operator intervention. Transfer **moves** the
-- flag rather than adding a holder, and the last-primary guard prevents clearing it.
--
-- The application writer is `insert_grant` (`src/infra/repositories/identity.rs`), which
-- computes the value as "no active primary exists" while holding the same `moiraown`
-- transaction advisory lock every other ownership mutation takes. That lock — not this
-- index — is what makes the loser of a race *not primary* rather than *failed*. The index
-- below is the database-level backstop, in exactly the sense
-- `admin_identities_issuer_subject_active_unique` is the backstop for a claim: it holds
-- even if a future code path forgets the lock, and it turns "two owners" from a state the
-- schema permits into one it refuses.
--
-- Reversal condition: going to *multiple* primaries is a schema change, not a config
-- toggle. It means dropping `admin_identities_single_active_primary`, turning the
-- last-primary guard into a last-any-primary guard, and changing transfer back from
-- "move the flag" to "set the flag" — one deliberate migration with its own tests.

-- ---------------------------------------------------------------------------------
-- Step 1 — collapse any pre-existing set of primaries to exactly one.
-- ---------------------------------------------------------------------------------
-- Not hypothetical. `PATCH .../{id}` with `{"is_primary": true}` set the flag without
-- clearing anyone else's, so a break-glass caller could promote two identities on a
-- deployment that `0017` had backfilled. The unique index below would refuse to build on
-- such a deployment, and a migration that fails on real data is worse than one that
-- repairs it.
--
-- The survivor is the setup claimant when it is among the primaries, and otherwise the
-- oldest grant. Deterministic, and never a row chosen for being convenient. `coalesce`
-- is load-bearing: `id = null` is `null`, and `desc` puts nulls *first* in PostgreSQL,
-- so an unset `claimed_admin_identity_id` would otherwise sort a random row to the top.
with keep as (
    select id
    from admin_identities
    where deleted_at is null
      and status = 'active'
      and is_primary
    order by coalesce(
                 id = (select claimed_admin_identity_id from setup_state where id),
                 false
             ) desc,
             created_at asc,
             id asc
    limit 1
)
update admin_identities
set is_primary = false
where deleted_at is null
  and status = 'active'
  and is_primary
  and id is distinct from (select id from keep);

-- ---------------------------------------------------------------------------------
-- Step 2 — bootstrap the deployments `0017` could not reach.
-- ---------------------------------------------------------------------------------
-- Textually `0017`'s two backfill steps, re-run. On a deployment where `0017` succeeded
-- both are no-ops (the `not exists` guard sees the primary it already set); on the
-- greenfield deployments F20 describes, this is the repair. They are repeated rather
-- than referenced because migrations are append-only: `0017` cannot be edited, and a
-- deployment that has already run it will never run it again.
--
-- Step 2a — the setup claimant. `setup_state` records exactly which identity claimed
-- setup, so "the claimant is primary by default" needs no new claim and no operator
-- action.
update admin_identities
set is_primary = true
where deleted_at is null
  and status = 'active'
  and id = (select claimed_admin_identity_id from setup_state where id)
  and not exists (
      select 1 from admin_identities existing
      where existing.deleted_at is null
        and existing.status = 'active'
        and existing.is_primary
  );

-- Step 2b — the sole active grant, for a deployment whose claimant row was revoked or
-- whose singleton predates the FK being populated. Deliberately restricted to *exactly
-- one* active row: promoting an arbitrary member of a set of several would be a silent
-- authority grant, and zero active rows is a legitimate state whose re-entry path is
-- system-key break-glass.
update admin_identities
set is_primary = true
where deleted_at is null
  and status = 'active'
  and not exists (
      select 1 from admin_identities existing
      where existing.deleted_at is null
        and existing.status = 'active'
        and existing.is_primary
  )
  and (
      select count(*) from admin_identities candidate
      where candidate.deleted_at is null and candidate.status = 'active'
  ) = 1;

-- ---------------------------------------------------------------------------------
-- Step 3 — at most one active primary, enforced by the schema.
-- ---------------------------------------------------------------------------------
-- The index key is `is_primary` and the predicate admits only rows where it is true, so
-- every indexed row carries the identical key and a second one collides. That is the
-- single-row idiom stated in terms of a real column rather than a constant expression,
-- which keeps it obviously immutable and obviously about ownership.
--
-- Partial on `deleted_at is null and status = 'active'` for the same reason every other
-- uniqueness key in this schema is: a revoked grant is audit history and must not keep
-- the ownership slot occupied. `revoke_grant` clears the flag on the way out, so a
-- revoked row never holds it anyway — this index is what makes that a rule instead of a
-- convention.
create unique index if not exists admin_identities_single_active_primary
    on admin_identities (is_primary)
    where deleted_at is null and status = 'active' and is_primary;

comment on index admin_identities_single_active_primary is
    'Decision D-F20: ownership is a single primary. The advisory lock in insert_grant/set_primary is the mechanism; this index is the invariant. Going to several primaries means dropping this index and widening the last-primary guard, not flipping a setting.';
