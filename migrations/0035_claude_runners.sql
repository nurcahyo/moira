-- Issue #275 (workstream R2 of #272) — the Moira-side mirror of containerised Claude runners.
--
-- # What this table is, and what it is NOT
--
-- It is a **mirror**, not the source of truth. `moira-runner` is stateless across restarts and
-- rebuilds its own view from Docker container labels (`moira.runner.id`, `moira.runner.expires_at`)
-- — see the frozen v1 control contract. This table exists so that Moira can (a) name a runner in
-- its own admin API without leaking the Docker id, (b) remember which provider credential a
-- finished runner was linked to, and (c) answer a console poll without a Docker round trip when
-- the runner has already reached a terminal state.
--
-- Moira holds no Docker access at all. Every state here arrived over HTTP from `moira-runner`.
--
-- # The token is not here, and there is no column it could go in
--
-- The minted token travels from `GET /v1/runners/{id}/token` straight into the existing credential
-- chain (`CredentialAdminService::create_credential`: AAD build, `cipher.encrypt`, fingerprint,
-- mask, audit row). `credential_id` below is a *reference* to the row that resulted. There is
-- deliberately no encrypted-payload column on this table: a second place that can hold provider
-- secret material is a second place to get the envelope wrong.
--
-- # Not runtime configuration
--
-- Like `skills`/`eval_suites`/`agent_flows` in 0031, this table does NOT fire
-- `notify_moira_runtime_config_change()`. Nothing in `ProviderRuntimeCache` or the runtime-config
-- cache keys on a runner row — the credential the runner produces is what routing sees, and
-- `provider_credentials` already carries its own invalidation. Adding a NOTIFY trigger here
-- without classifying the table in `tests/runtime_notify_inventory.rs` would fail that guard.
--
-- The `moira_bump_resource_version()` trigger (independent of NOTIFY: it maintains the `version`
-- ETag for `If-Match`) IS attached, exactly as 0031 attaches it to its three registries. That
-- trigger is `before update` only, so a freshly inserted row is at **version 1**, not 2.
--
-- All statements are `create table if not exists` / `create index if not exists` and apply
-- cleanly against a fresh, empty database.

create table if not exists claude_runners (
    id uuid primary key default gen_random_uuid(),
    -- Operator-facing name. The same charset `moira-runner` accepts for its own `label`
    -- (`[a-z0-9-]`, <= 64) because it is forwarded verbatim and becomes part of a container name.
    label varchar(64) not null,
    -- The runner id `moira-runner` minted (`POST /v1/runners` -> `{"id": "<uuid>"}`). Stored as
    -- text rather than uuid: it is an opaque identifier owned by another service, and the day its
    -- format changes should be a deserialisation change here, not a migration.
    runner_reference varchar(128) not null,
    -- The contract's state machine, plus one Moira-only terminal state:
    --   provisioning -> awaiting_authorization -> exchanging -> ready -> linked
    --                        |                        |
    --                        +------------------------+--> failed
    --   any state past expires_at --> expired
    -- `linked` means "the token was fetched and stored as a provider credential". It exists only
    -- on this side: `moira-runner` has no idea what Moira did with the token, and its own `ready`
    -- is one-shot, so a row that is still `ready` after a finalize attempt is a row whose token
    -- was never successfully persisted.
    state varchar(32) not null default 'provisioning'
        check (state in (
            'provisioning',
            'awaiting_authorization',
            'exchanging',
            'ready',
            'linked',
            'failed',
            'expired'
        )),
    -- The `https://claude.com/cai/oauth/authorize?...` line `moira-runner` scraped out of the
    -- container's tty stream. Not a secret: it is a public authorization URL the operator opens in
    -- their own browser, and it carries a PKCE challenge, not a credential.
    authorization_url text,
    -- The contract's error code vocabulary (`runner_failed`, `docker_unavailable`, ...) as reported
    -- upstream, kept verbatim so an operator can correlate with the runner service's own logs.
    error_code varchar(64),
    expires_at timestamptz,
    -- The provider credential minted from this runner's token. `on delete set null` rather than
    -- cascade: deleting a credential must not silently erase the audit-relevant fact that a runner
    -- existed and completed.
    credential_id uuid references provider_credentials(id) on delete set null,
    provider_id uuid references providers(id) on delete set null,
    -- =================================================================================
    -- Credential scope — whose Claude account this runner is for.
    --
    -- A tenant that has subscribed its own Claude account provisions a runner at
    -- `scope_type = 'tenant'`; the platform's own account stays `'global'`, which is the
    -- default and preserves the behaviour of a deployment that never sets a scope.
    -- `PgRuntimeRepository::resolve_runtime_credential` already ranks tenant (7) above
    -- global (8), so a tenant credential wins at execution time with no change to
    -- resolution — this migration only records which of the two a runner is producing.
    --
    -- **Decided at provision time, and not cosmetic.** The scope is part of the credential
    -- AAD (`credential_aad`, `src/security/crypto.rs`), so the credential finalize writes is
    -- sealed under it and cannot be re-scoped afterwards without re-encrypting. Storing the
    -- scope on the runner rather than taking it on the finalize request is what makes the
    -- runner's ownership visible in list/get and removes the window in which the two could
    -- disagree.
    --
    -- The column shapes and the CHECK below are transcribed from `provider_credentials`
    -- (`migrations/0003_security_foundation.sql`) rather than reinvented: a runner scope the
    -- credential table would refuse is a finalize that fails *after* the one-shot token has
    -- already been spent, which destroys the runner.
    -- =================================================================================
    scope_type varchar(32) not null default 'global' check (scope_type in (
        'global',
        'tenant',
        'application',
        'user'
    )),
    external_tenant_id varchar(256),
    application_id uuid references applications(id) on delete cascade,
    external_user_id varchar(256),
    metadata jsonb not null default '{}'::jsonb,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    deleted_at timestamptz,
    version bigint not null default 1,
    constraint claude_runners_external_ids_valid check (
        (external_tenant_id is null or (
            length(external_tenant_id) between 1 and 256
            and external_tenant_id !~ '[[:cntrl:]]'
        ))
        and (external_user_id is null or (
            length(external_user_id) between 1 and 256
            and external_user_id !~ '[[:cntrl:]]'
        ))
    ),
    constraint claude_runners_scope_check check (
        (scope_type = 'global'
            and external_tenant_id is null
            and application_id is null
            and external_user_id is null)
        or (scope_type = 'tenant'
            and external_tenant_id is not null
            and application_id is null
            and external_user_id is null)
        or (scope_type = 'application'
            and application_id is not null
            and external_user_id is null)
        or (scope_type = 'user'
            and external_user_id is not null)
    )
);

-- Unique-while-live label, matching the `agent_profiles`/`skills` shape: a soft-deleted runner
-- must not block reusing its name, but two live runners called the same thing would make the
-- console's own list ambiguous.
create unique index if not exists claude_runners_label_live_idx
    on claude_runners (label)
    where deleted_at is null;

-- The reverse lookup used when a control-plane reply has to be matched back to a row.
create unique index if not exists claude_runners_reference_live_idx
    on claude_runners (runner_reference)
    where deleted_at is null;

-- The `(created_at desc, id desc)` keyset every admin list pages on.
create index if not exists claude_runners_cursor_idx
    on claude_runners (created_at desc, id desc)
    where deleted_at is null;

drop trigger if exists claude_runners_bump_version on claude_runners;
create trigger claude_runners_bump_version
before update on claude_runners
for each row execute function moira_bump_resource_version();
