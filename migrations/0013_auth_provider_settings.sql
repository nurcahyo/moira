-- Plan 07 module 1 — runtime auth-provider settings (CONVENTIONS §7.2).
--
-- Which auth methods this deployment offers, and with what policy, is *runtime*
-- configuration owned by Moira's database — consistent with how providers, models,
-- routing and credentials already work — not build-time environment.
--
-- Decision D7 (binding): **there is no secret envelope on this table.** No
-- `encrypted_payload`, `encryption_algorithm`, `encryption_version`,
-- `encrypted_data_key`, `nonce`, `secret_fingerprint` or `masked_secret` column, and
-- therefore no envelope-completeness CHECK. The OAuth client secret is owned by the
-- console and lives in the console's own `console_auth` database, because Better Auth
-- needs the plaintext in-process to run the authorization-code exchange, and Moira's
-- secret envelope is deliberately write-only — making a secret readable over HTTP would
-- break the invariant that a decrypted secret never crosses a network boundary. Every
-- column below is therefore safe to read.
--
-- Do NOT "restore symmetry" with `provider_credentials` by adding envelope columns here.
-- The asymmetry is the decision, not an oversight. `provider_credentials` (the AI-provider
-- API keys) keeps its envelope exactly as it is; D7 does not touch it.

create table if not exists auth_provider_settings (
    id uuid primary key default gen_random_uuid(),
    method varchar(32) not null
        check (method in ('google_oauth', 'generic_oidc', 'jwks')),
    display_name varchar(256) not null,
    enabled boolean not null default false,

    -- non-secret configuration (CONVENTIONS §7.2)
    issuer text,
    discovery_url text,
    authorization_url text,
    token_url text,
    userinfo_url text,
    jwks_url text,
    -- Non-secret, and the D7 drift-protection anchor: the console reads this and
    -- compares its fingerprint against the one stored beside its own client secret.
    client_id text,
    requested_scopes text[] not null default array['openid', 'email', 'profile'],
    allowed_email_domains text[] not null default '{}',
    allowed_algorithms text[] not null default array['RS256'],
    expected_audiences text[] not null default '{}',
    redirect_uris text[] not null default '{}',
    trusted_jwt_issuer_id uuid references trusted_jwt_issuers(id),

    -- NO client-secret columns. Decision D7: the OAuth client secret is owned by
    -- the console and stored in the console's own console_auth database. Moira
    -- never stores it and never returns it. Do not add an envelope here.

    metadata jsonb not null default '{}'::jsonb,
    status varchar(32) not null default 'active'
        check (status in ('active', 'disabled', 'deleted')),
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    deleted_at timestamptz,
    version bigint not null default 1,

    constraint auth_provider_settings_method_shape check (
        (method = 'jwks'
            and jwks_url is not null
            and client_id is null)
        or (method in ('google_oauth', 'generic_oidc')
            and client_id is not null
            and (issuer is not null or discovery_url is not null))
    )
);

create unique index if not exists auth_provider_settings_method_issuer_active_unique
    on auth_provider_settings (method, (coalesce(issuer, '')))
    where deleted_at is null;

create index if not exists auth_provider_settings_enabled_idx
    on auth_provider_settings (method)
    where deleted_at is null and status = 'active' and enabled;

create index if not exists auth_provider_settings_cursor_idx
    on auth_provider_settings (created_at desc, id desc)
    where deleted_at is null;

-- Reuses `moira_bump_resource_version()` (`0004_admin_api_contract.sql:17-24`).
drop trigger if exists auth_provider_settings_bump_version on auth_provider_settings;
create trigger auth_provider_settings_bump_version
before update on auth_provider_settings
for each row execute function moira_bump_resource_version();

-- Reuses `notify_moira_runtime_config_change()` (`0004_admin_api_contract.sql:107-127`),
-- which emits `{"resource_type": <table>, "resource_id": <id>}` on the existing
-- `moira_runtime_config` channel. This is how CONVENTIONS §7.2's "changing auth settings
-- must invalidate the runtime cache through the existing LISTEN/NOTIFY path" is met — no
-- new channel and no new mechanism. Attachment style matches `0004:132-162`.
--
-- ⚠️ REQUIRED COMPANION CHANGE (plan 07 §0.1 B3), in a later wave of this plan:
-- `circuit_reset_scope` (`src/infra/db.rs`) maps unknown `resource_type` values to
-- `CircuitResetScope::All` plus a `warn!`. `auth_provider_settings` is unknown to it, so
-- until `"auth_provider_settings"` is added to `CIRCUIT_UNAFFECTED_RESOURCE_TYPES`, every
-- write here discards every provider circuit breaker. Breaker state is earned by observing
-- real failures and cannot be rebuilt, unlike the version-keyed runtime caches. Auth
-- settings do not affect provider health, so `Unaffected` is the honest classification.
drop trigger if exists auth_provider_settings_notify on auth_provider_settings;
create trigger auth_provider_settings_notify
after insert or update or delete on auth_provider_settings
for each row execute function notify_moira_runtime_config_change();
