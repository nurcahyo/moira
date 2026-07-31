-- Plan 09 wave 2 — `admin_identities.granted_by_actor_type` tells the truth about invites.
--
-- Wave 2's redeem path wrote `granted_by_actor_type = 'system_key'` for a grant no system
-- key produced. It was written that way because `0012`'s CHECK admits only 'system_key'
-- and 'setup_token', and the wave chose the legal-but-false value over a schema change.
--
-- That is the wrong trade in an **audit** column. `granted_by_actor_type` exists to answer
-- "which credential produced this grant", and the answer it gave was one an incident
-- responder would act on: 'system_key' means the bootstrap break-glass credential was used,
-- which is exactly the event a deployment alerts on. Every redemption would have raised
-- that signal falsely, and — worse — a genuine break-glass grant would have been
-- indistinguishable from routine onboarding.
--
-- The provenance was recoverable from elsewhere (`admin_invites.consumed_admin_identity_id`
-- is a real FK, and the audit row carries `admin_invite_id`), which is why this was a
-- defect rather than data loss. "Recoverable by joining another table" is not the same as
-- "correct here", and a column whose stated meaning disagrees with its contents is exactly
-- the kind of fact the next query filters on.
--
-- Numbering: `0017` is this wave's, so this is `0018`. Append-only per
-- `docs/project-structure.md` — `0012` is merged and is not edited.

-- `0012` declares the CHECK inline on the column, so Postgres named it
-- `admin_identities_granted_by_actor_type_check`. `if exists` keeps this migration
-- runnable against a database where an earlier hand-repair already dropped it, rather than
-- failing the whole migration run on a constraint that is already gone.
alter table admin_identities
    drop constraint if exists admin_identities_granted_by_actor_type_check;

-- Re-added under an explicit name, so the next widening does not have to guess at
-- Postgres's generated one.
--
-- 'setup_token' is retained even though decision D1 still defers that path: dropping a
-- value from a CHECK is the one direction that can fail against existing rows, and keeping
-- it means reversing D1 needs no further migration. That is `0012`'s own reasoning, applied
-- unchanged.
alter table admin_identities
    add constraint admin_identities_granted_by_actor_type_check
    check (granted_by_actor_type in ('system_key', 'setup_token', 'admin_invite'));

comment on column admin_identities.granted_by_actor_type is
    'Which credential produced this grant: system_key (the bootstrap break-glass path, via POST /api/v1/admin/setup/claim), admin_invite (a redeemed admin_invites row - see admin_invites.consumed_admin_identity_id), or setup_token (deferred, never written). An incident responder filters on this, so it is recorded honestly rather than collapsed onto whatever value the CHECK already admitted.';

-- Deliberately **no backfill**. Rewriting existing 'system_key' rows to 'admin_invite'
-- would require joining `admin_invites.consumed_admin_identity_id`, and that join is
-- exactly as trustworthy as the column it would be correcting — but it would also rewrite
-- audit history in place, turning "this row was recorded wrongly" into "this row was
-- silently changed". Any deployment that ran wave 2 before this migration keeps its
-- original rows; the FK link is the authority for those, and this comment is the record of
-- why they disagree.
