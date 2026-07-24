create table if not exists application_execution_policies (
    id uuid not null default gen_random_uuid(),
    application_id uuid primary key references applications(id) on delete cascade,
    responses_enabled boolean not null default true,
    streaming_enabled boolean not null default true,
    tools_enabled boolean not null default false,
    vision_enabled boolean not null default true,
    structured_output_enabled boolean not null default true,
    caller_system_instructions_allowed boolean not null default false,
    model_overrides_allowed boolean not null default false,
    route_overrides_allowed boolean not null default false,
    provider_overrides_allowed boolean not null default false,
    credential_overrides_allowed boolean not null default false,
    timeout_overrides_allowed boolean not null default false,
    persistence_mode varchar(32) not null default 'metadata_only'
        check (persistence_mode in ('none', 'metadata_only', 'encrypted_content', 'plain_content')),
    response_retention_seconds bigint not null default 2592000
        check (response_retention_seconds >= 0),
    maximum_request_bytes bigint not null default 1048576
        check (maximum_request_bytes > 0),
    maximum_input_items integer not null default 128
        check (maximum_input_items > 0),
    maximum_output_tokens bigint not null default 8192
        check (maximum_output_tokens > 0),
    maximum_timeout_ms bigint not null default 600000
        check (maximum_timeout_ms > 0),
    rate_limit_requests_per_minute integer not null default 120
        check (rate_limit_requests_per_minute > 0),
    rate_limit_streams_per_minute integer not null default 60
        check (rate_limit_streams_per_minute > 0),
    metadata jsonb not null default '{}'::jsonb,
    updated_at timestamptz not null default now(),
    version bigint not null default 1
);

create unique index if not exists application_execution_policies_id_unique
    on application_execution_policies (id);

create table if not exists responses (
    id uuid primary key default gen_random_uuid(),
    execution_id uuid not null unique,
    request_id varchar(128) not null,
    application_id uuid references applications(id) on delete set null,
    external_tenant_id varchar(256),
    external_user_id varchar(256),
    status varchar(32) not null default 'in_progress'
        check (status in ('queued', 'in_progress', 'completed', 'failed', 'cancelled')),
    route_id uuid references route_definitions(id) on delete set null,
    provider_id uuid references providers(id) on delete set null,
    provider_model_id uuid references provider_models(id) on delete set null,
    output_summary jsonb not null default '{}'::jsonb,
    usage_summary jsonb not null default '{}'::jsonb,
    metadata jsonb not null default '{}'::jsonb,
    failure_class varchar(128),
    failure_message text,
    output_persisted boolean not null default false,
    created_at timestamptz not null default now(),
    started_at timestamptz,
    completed_at timestamptz,
    failed_at timestamptz,
    cancelled_at timestamptz,
    expires_at timestamptz,
    version bigint not null default 1,
    constraint responses_external_ids_valid check (
        (external_tenant_id is null or (
            length(external_tenant_id) between 1 and 256
            and external_tenant_id !~ '[[:cntrl:]]'
        ))
        and (external_user_id is null or (
            length(external_user_id) between 1 and 256
            and external_user_id !~ '[[:cntrl:]]'
        ))
    )
);

create index if not exists responses_owner_cursor_idx
    on responses (
        application_id,
        coalesce(external_tenant_id, ''),
        coalesce(external_user_id, ''),
        created_at desc,
        id desc
    );

create index if not exists responses_request_idx
    on responses (request_id, created_at desc);

create index if not exists responses_status_idx
    on responses (status, created_at desc);

create index if not exists responses_execution_idx
    on responses (execution_id);

create index if not exists usage_records_public_filters_idx
    on usage_records (
        application_id,
        coalesce(external_tenant_id, ''),
        coalesce(external_user_id, ''),
        provider_id,
        provider_model_id,
        occurred_at desc,
        id desc
    );

drop trigger if exists application_execution_policies_bump_version on application_execution_policies;
create trigger application_execution_policies_bump_version
before update on application_execution_policies
for each row execute function moira_bump_resource_version();

drop trigger if exists responses_bump_version on responses;
create trigger responses_bump_version
before update on responses
for each row execute function moira_bump_resource_version();

drop trigger if exists application_execution_policies_runtime_config_notify
on application_execution_policies;
create trigger application_execution_policies_runtime_config_notify
after insert or update or delete on application_execution_policies
for each row execute function notify_moira_runtime_config_change();
