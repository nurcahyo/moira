-- Plan 07 module 1 — admin identity claiming.
--
-- Binds Moira admin authority to a stable `(issuer, subject)` pair sourced from an
-- already-registered trusted JWT issuer, so that granting a *human* admin scope never
-- requires Moira to issue a password, a session, or a login page.
--
-- Scope decision D1 (plan 07 §0.2): the one-time setup-token credential path is
-- DEFERRED, not implemented. There is deliberately **no `admin_setup_tokens` table
-- here**. Plan 08's console declares `setup_token?: string` in its DTO and never sends
-- it; the whole path was specified and never exercised, so it is cut rather than built
-- and left untested. The reversal conditions live in `plans/reports/EXECUTION-LEDGER.md`.
--
-- Style follows `0003_security_foundation.sql` / `0004_admin_api_contract.sql`: uuid PK
-- with `gen_random_uuid()`, `created_at`/`updated_at timestamptz not null default now()`,
-- `deleted_at` on mutable tables, `varchar(32)` status enums with a CHECK, and partial
-- unique indexes qualified `where deleted_at is null`.

create table if not exists admin_identities (
    id uuid primary key default gen_random_uuid(),
    -- FK proves the grant is bound to a currently-registered issuer row. `issuer` is
    -- denormalised beside it so the hot-path lookup in `authenticate_admin` needs no
    -- join; the denormalisation cannot drift because `trusted_jwt_issuers_issuer_active_unique`
    -- (`0003_security_foundation.sql:254-256`) makes the string a key.
    trusted_jwt_issuer_id uuid not null references trusted_jwt_issuers(id),
    issuer text not null,
    subject varchar(256) not null,
    -- Deliberately nullable at the database level even though the application requires it
    -- on every write (plan 07 module 2, decision D5): migrations are append-only, and a
    -- future revocation/anonymisation path may need to clear the address. The invariant
    -- "every grant this plan writes has an email" is enforced in the service and asserted
    -- by a test, not by a NOT NULL that could never be relaxed again.
    email varchar(320),
    email_verified boolean not null default false,
    granted_scopes text[] not null default array['moira:admin'],
    -- 'setup_token' remains an accepted value even though D1 defers that path, so that
    -- reversing D1 needs no further migration. No code in this plan writes it.
    granted_by_actor_type varchar(32) not null
        check (granted_by_actor_type in ('system_key', 'setup_token')),
    granted_by_subject varchar(256),
    status varchar(32) not null default 'active'
        check (status in ('active', 'revoked', 'deleted')),
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    revoked_at timestamptz,
    deleted_at timestamptz,
    version bigint not null default 1
);

-- The uniqueness key is `(issuer, subject)` and never email: email is mutable and
-- reassignable, so keying on it would let an address recycled by an identity provider
-- inherit a grant (`plans/01` §4.4).
create unique index if not exists admin_identities_issuer_subject_active_unique
    on admin_identities (issuer, subject)
    where deleted_at is null;

-- Backs the per-request grant lookup, which runs on every trusted-JWT admin request.
create index if not exists admin_identities_lookup_idx
    on admin_identities (issuer, subject)
    where deleted_at is null and status = 'active';

-- Single-row table. `id boolean primary key default true check (id)` is the explicit
-- singleton idiom; no singleton convention existed in `0001`-`0011` to follow instead.
--
-- It records whether any admin identity has **ever** been claimed, independently of
-- `admin_identities.status`, so revoking the only admin cannot silently reopen the
-- unauthenticated land-grab window. Re-opening claim-ability after a revocation is a
-- deliberate operator action through the system-key path, never an automatic side effect.
create table if not exists setup_state (
    id boolean primary key default true check (id),
    claimed boolean not null default false,
    claimed_admin_identity_id uuid references admin_identities(id),
    claimed_at timestamptz,
    updated_at timestamptz not null default now()
);

insert into setup_state (id, claimed) values (true, false)
    on conflict (id) do nothing;

-- Reuses `moira_bump_resource_version()` (`0004_admin_api_contract.sql:17-24`), the
-- function every other versioned table already uses. Do not define a bespoke variant.
drop trigger if exists admin_identities_bump_version on admin_identities;
create trigger admin_identities_bump_version
before update on admin_identities
for each row execute function moira_bump_resource_version();
