create table if not exists tenants (
    id text primary key,
    name text not null,
    metadata jsonb not null default '{}'::jsonb,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create table if not exists applications (
    id text primary key,
    tenant_id text references tenants(id) on delete cascade,
    name text not null,
    metadata jsonb not null default '{}'::jsonb,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create table if not exists providers (
    id uuid primary key default gen_random_uuid(),
    tenant_id text references tenants(id) on delete cascade,
    application_id text references applications(id) on delete cascade,
    name text not null,
    kind text not null check (kind in (
        'openai_compatible',
        'openai',
        'anthropic',
        'gemini',
        'deepseek',
        'azure_openai',
        'local'
    )),
    base_url text not null,
    default_model text not null,
    enabled boolean not null default true,
    timeout_ms integer not null default 120000 check (timeout_ms > 0),
    metadata jsonb not null default '{}'::jsonb,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    constraint provider_scope_consistency check (
        application_id is null or tenant_id is not null
    )
);

create table if not exists provider_credentials (
    id uuid primary key default gen_random_uuid(),
    provider_id uuid not null references providers(id) on delete cascade,
    owner_scope text not null check (owner_scope in ('global', 'tenant', 'application', 'user')),
    owner_id text,
    key_id text not null,
    nonce_base64 text not null,
    ciphertext_base64 text not null,
    enabled boolean not null default true,
    expires_at timestamptz,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    constraint credential_owner_consistency check (
        (owner_scope = 'global' and owner_id is null)
        or (owner_scope <> 'global' and owner_id is not null)
    )
);

create unique index if not exists provider_credentials_owner_unique
    on provider_credentials (provider_id, owner_scope, coalesce(owner_id, ''));

create table if not exists routing_policies (
    id uuid primary key default gen_random_uuid(),
    tenant_id text references tenants(id) on delete cascade,
    application_id text references applications(id) on delete cascade,
    provider_id uuid not null references providers(id) on delete restrict,
    model text,
    enabled boolean not null default true,
    metadata jsonb not null default '{}'::jsonb,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    constraint routing_scope_consistency check (
        application_id is null or tenant_id is not null
    )
);

create unique index if not exists routing_policies_scope_unique
    on routing_policies (coalesce(tenant_id, ''), coalesce(application_id, ''));

create table if not exists audit_events (
    id uuid primary key default gen_random_uuid(),
    tenant_id text,
    application_id text,
    actor_subject text,
    action text not null,
    resource_type text not null,
    resource_id text,
    metadata jsonb not null default '{}'::jsonb,
    created_at timestamptz not null default now()
);

create index if not exists providers_scope_idx on providers (tenant_id, application_id);
create index if not exists providers_enabled_idx on providers (enabled);
create index if not exists provider_credentials_lookup_idx
    on provider_credentials (provider_id, owner_scope, owner_id, enabled);
create index if not exists audit_events_created_at_idx on audit_events (created_at desc);

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

drop trigger if exists provider_credentials_runtime_config_notify on provider_credentials;
create trigger provider_credentials_runtime_config_notify
after insert or update or delete on provider_credentials
for each row execute function notify_moira_runtime_config_change();

drop trigger if exists routing_policies_runtime_config_notify on routing_policies;
create trigger routing_policies_runtime_config_notify
after insert or update or delete on routing_policies
for each row execute function notify_moira_runtime_config_change();
