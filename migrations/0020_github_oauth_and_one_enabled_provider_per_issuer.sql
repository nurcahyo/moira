-- Plan 09 wave 4 (stage 4A) — GitHub's shape, and the invariant that makes the
-- admission-policy lookup deterministic.
--
-- Numbering: `0019_single_primary_admin.sql` merged with PR #39 and `0016` is a permanent
-- gap, so this is `0020`. Append-only per `docs/project-structure.md` — `0013` is merged
-- and is not edited; everything below drops and re-adds rather than altering in place.
--
-- Three changes, and the first two must ship together:
--
--   (a) `auth_provider_settings_method_check` admits `github_oauth`.
--   (b) `auth_provider_settings_method_shape` gains a GitHub arm.
--   (c) at most one enabled provider may be bound to any one trusted JWT issuer.
--
-- (a) alone is a green migration that still cannot accept a GitHub row: `0013` declares the
-- method values inline (Postgres auto-named that CHECK `auth_provider_settings_method_check`)
-- *and* a separate `auth_provider_settings_method_shape` whose only OAuth arm requires
-- `issuer is not null or discovery_url is not null`. GitHub OAuth is not OIDC and has
-- neither, so a GitHub insert passes the widened method CHECK and then fails the shape
-- CHECK. Verified empirically against this schema, not inferred. `tests/auth_provider_settings.rs`
-- asserts exactly that ordering, so shipping only (a) is a red test rather than a
-- discovery in wave 5.
--
-- Explicitly NOT touched:
--   * `auth_provider_settings_method_issuer_active_unique` on `(method, coalesce(issuer,''))`.
--     Two consequences of leaving it stand, stated here rather than discovered later:
--     **exactly one `github_oauth` row per deployment** (GitHub's `issuer` is null, so every
--     GitHub row keys to `('github_oauth','')`) — one GitHub org per console — and **at most
--     one discovery-only generic-OIDC row with a null `issuer`**. Both are wave-5 questions.
--   * `admin_identities`. Per-provider issuer strings (stage 4B) give distinct
--     `(issuer, subject)` pairs under the existing
--     `admin_identities_issuer_subject_active_unique`, so nothing there needs widening.

-- ---------------------------------------------------------------------------------
-- (a) The method vocabulary.
-- ---------------------------------------------------------------------------------
-- `0013` spells the CHECK inline on the column, so Postgres generated the name below.
-- Re-added under that same explicit name — `0018`'s precedent for
-- `admin_identities_granted_by_actor_type_check` — so the next widening does not have to
-- guess at a generated one. `if exists` keeps this runnable against a database where a
-- hand-repair already dropped it.
alter table auth_provider_settings
    drop constraint if exists auth_provider_settings_method_check;

alter table auth_provider_settings
    add constraint auth_provider_settings_method_check
    check (method in ('google_oauth', 'generic_oidc', 'jwks', 'github_oauth'));

-- ---------------------------------------------------------------------------------
-- (b) GitHub's shape.
-- ---------------------------------------------------------------------------------
-- GitHub OAuth publishes no discovery document and issues no `id_token`, so the OIDC arm's
-- `issuer is not null or discovery_url is not null` cannot be satisfied. Its endpoints are
-- fixed and must be configured directly. `issuer` and `discovery_url` are required to be
-- **null** rather than merely optional, because a `github_oauth` row carrying an `issuer`
-- would enter the second stage of `admission_policy`'s lookup — the mode-3 bring-your-own-JWKS
-- path — where it does not belong, and would occupy a `(method, issuer)` uniqueness slot
-- under a value nothing verifies against.
alter table auth_provider_settings
    drop constraint if exists auth_provider_settings_method_shape;

alter table auth_provider_settings
    add constraint auth_provider_settings_method_shape check (
        (method = 'jwks'
            and jwks_url is not null
            and client_id is null)
        or (method in ('google_oauth', 'generic_oidc')
            and client_id is not null
            and (issuer is not null or discovery_url is not null))
        or (method = 'github_oauth'
            and client_id is not null
            and authorization_url is not null
            and token_url is not null
            and issuer is null
            and discovery_url is null)
    );

-- ---------------------------------------------------------------------------------
-- (c) One enabled provider per trusted JWT issuer.
-- ---------------------------------------------------------------------------------
-- Finding F23. `governing_policy` selected `limit 1` over
-- `issuer = $1 or trusted_jwt_issuer_id = $2` and broke ties on `created_at asc, id asc`.
-- On a console-mediated deployment `$1` is the *console's* issuer while every provider row's
-- `issuer` column holds the *IdP's*, so no legitimately-bound row ever matched on issuer,
-- every candidate tied on the first sort key, and **the oldest row bound to that trusted
-- issuer supplied `allowed_email_domains` for every claim and every redemption** — whichever
-- provider actually authenticated the human.
--
-- This index makes that state unrepresentable: with at most one enabled row per trusted
-- issuer, the first stage of `admission_policy` returns at most one row and nothing is
-- ordered. The lookup still refuses a duplicate set rather than taking the first row of one,
-- because an index is not a substitute for a query that cannot be wrong.
--
-- NULLS ARE DISTINCT HERE, AND DELIBERATELY SO. `trusted_jwt_issuer_id` is nullable and an
-- unbound row is legitimate — a `jwks` row registered for mode-3 verification, or a provider
-- configured but not yet bound. Several may coexist. The standing rule that a unique index
-- *widened* over a nullable column must be spelled `nulls not distinct` is about preserving
-- a uniqueness that already held over the narrower key; this index is new and its whole
-- subject is the non-null binding, so `nulls not distinct` would refuse the second unbound
-- row and break `jwks` configuration outright.
--
-- ---------------------------------------------------------------------------------
-- IF THIS MIGRATION FAILS HERE, THE DEPLOYMENT IS ALREADY AMBIGUOUS. THAT IS THE POINT.
-- ---------------------------------------------------------------------------------
-- The failure reads:
--
--   ERROR: could not create unique index "auth_provider_settings_one_enabled_per_trusted_issuer"
--   DETAIL: Key (trusted_jwt_issuer_id)=(…) is duplicated.
--
-- `database.migrate_on_startup` must be false in production (`src/config/settings.rs`), and
-- `charts/moira/templates/migration-job.yaml` runs production migrations as a Helm
-- pre-install/pre-upgrade Job. So this is not a CrashLoopBackOff: the hook fails, the upgrade
-- aborts, and the old pods keep serving on the old schema. That is the correct outcome —
-- the deployment is in a state where which admission policy governs depends on row order,
-- and an operator has to say which provider is meant to govern.
--
-- OPERATOR REMEDY. Find the duplicates:
--
--   select trusted_jwt_issuer_id, array_agg(id order by created_at) as provider_ids
--     from auth_provider_settings
--    where enabled and status = 'active' and deleted_at is null
--      and trusted_jwt_issuer_id is not null
--    group by trusted_jwt_issuer_id having count(*) > 1;
--
-- then disable all but the one that should govern — through
-- `POST /api/v1/admin/auth/providers/{id}/disable`, so the change is audited and the runtime
-- cache is invalidated — and re-run the upgrade.
--
-- THIS MIGRATION DELIBERATELY CONTAINS NO `update auth_provider_settings set enabled = false`.
-- Repairing the data here would be a silent change to **who can obtain Moira admin**,
-- executed with no operator present, that rolling the binary back cannot undo. A failed
-- upgrade is recoverable in one command; a wrong admission policy applied quietly is not.
create unique index if not exists auth_provider_settings_one_enabled_per_trusted_issuer
    on auth_provider_settings (trusted_jwt_issuer_id)
    where enabled and status = 'active' and deleted_at is null;

comment on index auth_provider_settings_one_enabled_per_trusted_issuer is
    'Finding F23: at most one enabled provider may be bound to any one trusted JWT issuer, so admission_policy''s first-stage lookup returns at most one row and no ORDER BY decides which admission policy governs. Nulls are distinct on purpose - unbound rows (jwks, or not yet bound) are legitimate and several may coexist. An already-ambiguous deployment fails this index at upgrade time by design; the remedy is to disable the row that should not govern, never an unattended UPDATE inside a migration.';
