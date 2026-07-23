do $$
begin
    if to_regclass('public.routing_policies') is not null
        and exists (
            select 1
            from information_schema.columns
            where table_schema = 'public'
              and table_name = 'routing_policies'
              and column_name = 'tenant_id'
        )
        and to_regclass('public.legacy_routing_policies') is null then
        alter table routing_policies rename to legacy_routing_policies;
    end if;

    if to_regclass('public.provider_credentials') is not null
        and exists (
            select 1
            from information_schema.columns
            where table_schema = 'public'
              and table_name = 'provider_credentials'
              and column_name = 'owner_scope'
        )
        and to_regclass('public.legacy_provider_credentials') is null then
        alter table provider_credentials rename to legacy_provider_credentials;
    end if;

    if to_regclass('public.providers') is not null
        and exists (
            select 1
            from information_schema.columns
            where table_schema = 'public'
              and table_name = 'providers'
              and column_name = 'kind'
        )
        and to_regclass('public.legacy_providers') is null then
        alter table providers rename to legacy_providers;
    end if;

    if to_regclass('public.applications') is not null
        and exists (
            select 1
            from information_schema.columns
            where table_schema = 'public'
              and table_name = 'applications'
              and column_name = 'name'
        )
        and to_regclass('public.legacy_applications') is null then
        alter table applications rename to legacy_applications;
    end if;
end $$;

create table if not exists applications (
    id uuid primary key default gen_random_uuid(),
    external_application_id varchar(256),
    application_slug varchar(128),
    display_name varchar(200) not null,
    status varchar(32) not null default 'active'
        check (status in ('active', 'disabled', 'deleted')),
    metadata jsonb not null default '{}'::jsonb,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    deleted_at timestamptz,
    constraint applications_identifier_required check (
        external_application_id is not null
        or application_slug is not null
    ),
    constraint applications_external_id_valid check (
        external_application_id is null
        or (
            length(external_application_id) between 1 and 256
            and external_application_id !~ '[[:cntrl:]]'
        )
    ),
    constraint applications_slug_valid check (
        application_slug is null
        or (
            length(application_slug) between 1 and 128
            and application_slug ~ '^[a-z0-9]([a-z0-9_-]*[a-z0-9])?$'
        )
    )
);

create unique index if not exists applications_external_application_id_active_unique
    on applications (external_application_id)
    where deleted_at is null and external_application_id is not null;

create unique index if not exists applications_application_slug_active_unique
    on applications (application_slug)
    where deleted_at is null and application_slug is not null;

create table if not exists providers (
    id uuid primary key default gen_random_uuid(),
    provider_type varchar(64) not null check (provider_type in (
        'openai_compatible',
        'openai',
        'anthropic',
        'gemini',
        'deepseek',
        'azure_openai',
        'local',
        'custom'
    )),
    display_name varchar(200) not null,
    base_url text,
    status varchar(32) not null default 'active'
        check (status in ('active', 'disabled', 'deleted')),
    metadata jsonb not null default '{}'::jsonb,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    deleted_at timestamptz
);

create index if not exists providers_status_idx
    on providers (status)
    where deleted_at is null;

create table if not exists provider_models (
    id uuid primary key default gen_random_uuid(),
    provider_id uuid not null references providers(id) on delete cascade,
    model_key varchar(200) not null,
    display_name varchar(200),
    capabilities jsonb not null default '{}'::jsonb,
    status varchar(32) not null default 'active'
        check (status in ('active', 'disabled', 'deprecated')),
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    deleted_at timestamptz
);

create unique index if not exists provider_models_provider_model_key_active_unique
    on provider_models (provider_id, model_key)
    where deleted_at is null;

create index if not exists provider_models_provider_status_idx
    on provider_models (provider_id, status)
    where deleted_at is null;

create table if not exists provider_credentials (
    id uuid primary key default gen_random_uuid(),
    provider_id uuid not null references providers(id) on delete cascade,
    credential_type varchar(64) not null check (credential_type in (
        'api_key',
        'oauth2',
        'bearer_token',
        'basic_auth',
        'custom_headers',
        'azure_openai',
        'service_account'
    )),
    scope_type varchar(32) not null check (scope_type in (
        'global',
        'tenant',
        'application',
        'user'
    )),
    external_tenant_id varchar(256),
    application_id uuid references applications(id) on delete cascade,
    external_user_id varchar(256),
    encrypted_payload bytea not null,
    encryption_algorithm varchar(64) not null,
    encryption_version integer not null check (encryption_version > 0),
    encrypted_data_key bytea,
    nonce bytea not null,
    secret_fingerprint varchar(128) not null,
    masked_secret varchar(128) not null,
    status varchar(32) not null default 'active'
        check (status in ('active', 'disabled', 'expired', 'deleted', 'validation_failed')),
    priority integer not null default 100 check (priority >= 0),
    expires_at timestamptz,
    last_validated_at timestamptz,
    last_used_at timestamptz,
    metadata jsonb not null default '{}'::jsonb,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    deleted_at timestamptz,
    constraint provider_credentials_external_ids_valid check (
        (external_tenant_id is null or (
            length(external_tenant_id) between 1 and 256
            and external_tenant_id !~ '[[:cntrl:]]'
        ))
        and (external_user_id is null or (
            length(external_user_id) between 1 and 256
            and external_user_id !~ '[[:cntrl:]]'
        ))
    ),
    constraint provider_credentials_scope_check check (
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

create index if not exists provider_credentials_resolution_idx
    on provider_credentials (
        provider_id,
        status,
        scope_type,
        external_user_id,
        application_id,
        external_tenant_id,
        priority
    )
    where deleted_at is null;

create index if not exists provider_credentials_fingerprint_idx
    on provider_credentials (secret_fingerprint)
    where deleted_at is null;

create index if not exists provider_credentials_scope_idx
    on provider_credentials (
        provider_id,
        credential_type,
        scope_type,
        coalesce(external_tenant_id, ''),
        coalesce(application_id::text, ''),
        coalesce(external_user_id, ''),
        priority
    )
    where deleted_at is null and status <> 'deleted';

create table if not exists trusted_jwt_issuers (
    id uuid primary key default gen_random_uuid(),
    issuer text not null,
    jwks_url text not null,
    expected_audiences text[] not null default '{}',
    allowed_algorithms text[] not null default array['RS256'],
    subject_claim varchar(128) not null default 'sub',
    user_id_claim varchar(128),
    tenant_id_claim varchar(128),
    application_id_claim varchar(128),
    roles_claim varchar(128),
    scopes_claim varchar(128),
    delegated_user_claim varchar(128),
    delegated_tenant_claim varchar(128),
    clock_skew_seconds integer not null default 60 check (clock_skew_seconds >= 0),
    allow_delegation boolean not null default false,
    status varchar(32) not null default 'active'
        check (status in ('active', 'disabled', 'deleted')),
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    deleted_at timestamptz
);

create unique index if not exists trusted_jwt_issuers_issuer_active_unique
    on trusted_jwt_issuers (issuer)
    where deleted_at is null;

create index if not exists trusted_jwt_issuers_status_idx
    on trusted_jwt_issuers (status)
    where deleted_at is null;

create table if not exists system_api_keys (
    id uuid primary key default gen_random_uuid(),
    display_name varchar(200) not null,
    key_prefix varchar(64) not null,
    key_hash text not null,
    fingerprint varchar(128) not null,
    pepper_version varchar(64) not null,
    scopes text[] not null default '{}',
    status varchar(32) not null default 'active'
        check (status in ('active', 'revoked', 'expired', 'deleted')),
    expires_at timestamptz,
    last_used_at timestamptz,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    revoked_at timestamptz,
    deleted_at timestamptz
);

create unique index if not exists system_api_keys_prefix_active_unique
    on system_api_keys (key_prefix)
    where deleted_at is null;

create index if not exists system_api_keys_fingerprint_idx
    on system_api_keys (fingerprint)
    where deleted_at is null;

create table if not exists consumer_api_keys (
    id uuid primary key default gen_random_uuid(),
    application_id uuid references applications(id) on delete cascade,
    display_name varchar(200) not null,
    key_prefix varchar(64) not null,
    key_hash text not null,
    fingerprint varchar(128) not null,
    pepper_version varchar(64) not null,
    scopes text[] not null default '{}',
    status varchar(32) not null default 'active'
        check (status in ('active', 'revoked', 'expired', 'deleted')),
    expires_at timestamptz,
    last_used_at timestamptz,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    revoked_at timestamptz,
    deleted_at timestamptz
);

create unique index if not exists consumer_api_keys_prefix_active_unique
    on consumer_api_keys (key_prefix)
    where deleted_at is null;

create index if not exists consumer_api_keys_application_idx
    on consumer_api_keys (application_id, status)
    where deleted_at is null;

create index if not exists consumer_api_keys_fingerprint_idx
    on consumer_api_keys (fingerprint)
    where deleted_at is null;

create table if not exists audit_logs (
    id uuid primary key default gen_random_uuid(),
    occurred_at timestamptz not null default now(),
    request_id varchar(128),
    actor_type varchar(32),
    actor_subject varchar(256),
    delegated_subject varchar(256),
    external_user_id varchar(256),
    external_tenant_id varchar(256),
    application_id uuid references applications(id) on delete set null,
    resource_type varchar(128) not null,
    resource_id varchar(256),
    action varchar(128) not null,
    result varchar(32) not null check (result in ('success', 'denied', 'failed')),
    source_ip inet,
    user_agent text,
    metadata jsonb not null default '{}'::jsonb
);

create index if not exists audit_logs_occurred_at_idx
    on audit_logs (occurred_at desc);

create index if not exists audit_logs_actor_idx
    on audit_logs (actor_subject, occurred_at desc);

create index if not exists audit_logs_resource_idx
    on audit_logs (resource_type, resource_id, occurred_at desc);

create table if not exists idempotency_records (
    id uuid primary key default gen_random_uuid(),
    idempotency_key_hash varchar(128) not null,
    actor_fingerprint varchar(128) not null,
    operation varchar(128) not null,
    request_hash varchar(128) not null,
    response_status integer,
    response_body jsonb,
    resource_id varchar(256),
    created_at timestamptz not null default now(),
    expires_at timestamptz not null
);

create unique index if not exists idempotency_records_unique
    on idempotency_records (idempotency_key_hash, actor_fingerprint, operation);

insert into applications (
    id,
    external_application_id,
    application_slug,
    display_name,
    metadata,
    created_at,
    updated_at
)
select
    gen_random_uuid(),
    left(id, 256),
    null,
    left(name, 200),
    metadata,
    created_at,
    updated_at
from legacy_applications
where to_regclass('public.legacy_applications') is not null
on conflict do nothing;

insert into providers (
    id,
    provider_type,
    display_name,
    base_url,
    status,
    metadata,
    created_at,
    updated_at
)
select
    id,
    case
        when kind in ('openai_compatible', 'openai', 'anthropic', 'gemini', 'deepseek', 'azure_openai', 'local')
            then kind
        else 'custom'
    end,
    left(name, 200),
    base_url,
    case when enabled then 'active' else 'disabled' end,
    jsonb_build_object(
        'legacy_timeout_ms', timeout_ms,
        'legacy_tenant_id', tenant_id,
        'legacy_application_id', application_id
    ) || metadata,
    created_at,
    updated_at
from legacy_providers
where to_regclass('public.legacy_providers') is not null
on conflict do nothing;

insert into provider_models (
    id,
    provider_id,
    model_key,
    display_name,
    capabilities,
    status,
    created_at,
    updated_at
)
select
    gen_random_uuid(),
    id,
    left(default_model, 200),
    left(default_model, 200),
    '{}'::jsonb,
    case when enabled then 'active' else 'disabled' end,
    created_at,
    updated_at
from legacy_providers
where to_regclass('public.legacy_providers') is not null
  and default_model is not null
on conflict do nothing;

create or replace function notify_moira_runtime_config_change()
returns trigger language plpgsql as $$
begin
    perform pg_notify(
        'moira_runtime_config',
        json_build_object(
            'table', tg_table_name,
            'operation', tg_op,
            'changed_at', now()
        )::text
    );
    if tg_op = 'DELETE' then
        return old;
    end if;

    return new;
end;
$$;

drop trigger if exists providers_runtime_config_notify on providers;
create trigger providers_runtime_config_notify
after insert or update or delete on providers
for each row execute function notify_moira_runtime_config_change();

drop trigger if exists provider_models_runtime_config_notify on provider_models;
create trigger provider_models_runtime_config_notify
after insert or update or delete on provider_models
for each row execute function notify_moira_runtime_config_change();

drop trigger if exists provider_credentials_runtime_config_notify on provider_credentials;
create trigger provider_credentials_runtime_config_notify
after insert or update or delete on provider_credentials
for each row execute function notify_moira_runtime_config_change();
