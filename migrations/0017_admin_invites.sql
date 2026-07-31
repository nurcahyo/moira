-- Plan 09 wave 2 — admin invitations, and ownership as row state.
--
-- Two changes, in one migration because they are one feature: an existing admin can
-- onboard a colleague without the bootstrap system key, and "who may do that" is a
-- property of an identity rather than a capability grant.
--
-- Numbering: `0015_worker_jobs.sql` is the highest shipped and plan 11 §0 A1 reserves
-- `0016`, so this is `0017` (plan 09 §0.5). Append-only, per `docs/project-structure.md`:
-- corrections are new migrations, never edits to a merged one.
--
-- ---------------------------------------------------------------------------------
-- Decision D1 (plan 09 §0.2): ownership is ROW STATE, not a scope.
-- ---------------------------------------------------------------------------------
-- The body of plan 09 wanted `moira:admins:manage` to be an *explicit* scope that
-- `moira:admin` does not imply. That is unimplementable without changing the
-- authorization core: `AuthorizationService::has_scope` (`src/security/authz.rs`) is
-- `contains(required) || (implying_actor && contains(ADMIN_SCOPE))` with no per-scope
-- opt-out, `ADMIN_IMPLYING_ACTOR_TYPES` contains `TrustedJwt`, and every grant this
-- table writes carries `moira:admin` by default. Under that design every admin would
-- already satisfy `moira:admins:manage`, so the ownership model, the transfer action
-- and the last-primary guard would all be decorative.
--
-- `is_primary` below is the replacement. It is checked directly in the handler, and it
-- makes the last-primary guard **writable as a query** — which under the scope design
-- it was not, because "who carries the scope" answers "everyone, by implication".
--
-- ---------------------------------------------------------------------------------
-- Decision D-W2-1: recovery invites are NOT built here.
-- ---------------------------------------------------------------------------------
-- Plan 09 specifies `is_recovery boolean` and `replaces_admin_identity_id uuid` for an
-- atomic revoke-and-grant swap. That behaviour belongs to the recovery UI (plan 09's
-- wave 5) and is not in this wave's scope, so the columns are deliberately absent: a
-- column no code writes is the schema equivalent of a catalog entry with no emitter,
-- which plan 07 §0.7 and plan 09 §0.5 both rule out.
-- Reversal condition: when the recovery flow is built, a forward migration adds both
-- columns and the redeem path gains the swap, in the same change as its tests.

alter table admin_identities
    add column if not exists is_primary boolean not null default false;

comment on column admin_identities.is_primary is
    'Ownership as row state (plan 09 D1). A primary admin may transfer ownership and revoke other grants; the last active primary cannot be cleared or revoked. Deliberately not a scope: moira:admin implies every scope for a trusted-JWT actor, so a scope could not express "not everyone".';

-- Backs the last-primary guard and the "is the caller primary" check, both of which run
-- on every ownership mutation.
create index if not exists admin_identities_primary_active_idx
    on admin_identities (id)
    where deleted_at is null and status = 'active' and is_primary;

-- Backfill, in two ordered steps, each a no-op once a primary exists.
--
-- Step 1 — the setup claimant. `setup_state` records exactly which identity claimed
-- setup (`0012`), so "the claimant is primary by default" needs no new claim and no
-- operator action.
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

-- Step 2 — the sole active grant, for a deployment whose claimant row was revoked or
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
-- admin_invites
-- ---------------------------------------------------------------------------------
-- A single-use, time-limited, email- or domain-bound token that authorises its holder
-- to *submit* a redemption. It is never a policy bypass: plan 07's deny-by-default
-- `allowed_email_domains` check (decision D3) runs on redemption exactly as it does on
-- a first claim, and an invite grants no exemption from it.
--
-- The secret-storage column set mirrors `system_api_keys`
-- (`0003_security_foundation.sql:262-278`) — prefix, Argon2id+pepper hash, fingerprint,
-- pepper version — renamed `token_*` because this is a token rather than an API key.
-- It is deliberately NOT the plain-SHA-256 shape finding P1-1 flags elsewhere.
--
-- Plan 09's body cites `admin_setup_tokens` as the template. That table does not exist:
-- decision D1 of plan 07 cut it (`0012:7-11`). `system_api_keys` is the real precedent.
create table if not exists admin_invites (
    id uuid primary key default gen_random_uuid(),

    -- Indexed lookup key. The Argon2id hash cannot be searched, so verification is
    -- prefix-lookup-then-verify, exactly as `AuthService::verify_api_key` does. This is
    -- also what bounds the anonymous preview endpoint's CPU cost: a caller who does not
    -- already hold a valid prefix causes zero Argon2 work.
    token_prefix varchar(64) not null,
    token_hash text not null,
    fingerprint varchar(128) not null,
    pepper_version varchar(64) not null,

    -- Exactly one of the two is set, enforced below. They are the *invite's own*
    -- constraint and are checked separately from the provider allow-list, because the
    -- remedies differ: reissue the invite versus widen the allow-list.
    email_constraint varchar(320),
    domain_constraint varchar(320),

    -- The (issuer, subject) of the admin who created it. Not an FK onto
    -- admin_identities: a system-key holder can create an invite during bootstrap and
    -- has no grant row, and an FK would make revoking the inviter cascade into the
    -- invite's audit trail.
    created_by_issuer text,
    created_by_subject varchar(256),
    created_by_actor_type varchar(32) not null
        check (created_by_actor_type in ('system_key', 'trusted_jwt', 'dev_admin')),

    -- 'expired' is deliberately NOT a legal value. Expiry is derived from `expires_at`
    -- on every read; storing it would require a sweeper that does not exist, and a
    -- status no code writes is a trap for the next query that filters on it.
    -- Reversal condition: if a sweeper is added, a forward migration widens this CHECK.
    status varchar(32) not null default 'pending'
        check (status in ('pending', 'consumed', 'revoked')),
    expires_at timestamptz not null,
    consumed_at timestamptz,
    consumed_issuer text,
    consumed_subject varchar(256),
    consumed_admin_identity_id uuid references admin_identities(id),
    revoked_at timestamptz,

    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    deleted_at timestamptz,
    version bigint not null default 1,

    constraint admin_invites_exactly_one_constraint check (
        (email_constraint is null) <> (domain_constraint is null)
    ),
    -- A consumed row must name who consumed it. Without this, "consumed by nobody" is a
    -- representable state and the audit trail has a hole exactly where a grant was made.
    constraint admin_invites_consumed_shape check (
        status <> 'consumed'
        or (consumed_at is not null
            and consumed_issuer is not null
            and consumed_subject is not null)
    )
);

create unique index if not exists admin_invites_token_prefix_active_unique
    on admin_invites (token_prefix)
    where deleted_at is null;

create index if not exists admin_invites_fingerprint_idx
    on admin_invites (fingerprint)
    where deleted_at is null;

-- The list endpoint's keyset: descending `(created_at, id)`, the same contract every
-- other admin list follows.
create index if not exists admin_invites_cursor_idx
    on admin_invites (created_at desc, id desc)
    where deleted_at is null;

-- Reuses `moira_bump_resource_version()` (`0004_admin_api_contract.sql:17-24`), the
-- function every other versioned table already uses. Do not define a bespoke variant.
drop trigger if exists admin_invites_bump_version on admin_invites;
create trigger admin_invites_bump_version
before update on admin_invites
for each row execute function moira_bump_resource_version();
