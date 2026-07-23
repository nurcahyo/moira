create table if not exists agent_profiles (
    id uuid primary key default gen_random_uuid(),
    profile_key varchar(128) not null,
    display_name varchar(200) not null,
    preamble text,
    temperature double precision,
    max_tokens bigint,
    tool_policy jsonb not null default '{}'::jsonb,
    context_policy jsonb not null default '{}'::jsonb,
    memory_policy jsonb not null default '{}'::jsonb,
    status varchar(32) not null default 'active'
        check (status in ('active', 'disabled', 'deleted')),
    metadata jsonb not null default '{}'::jsonb,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    deleted_at timestamptz,
    version bigint not null default 1,
    constraint agent_profiles_profile_key_valid check (
        length(profile_key) between 1 and 128
        and profile_key ~ '^[a-z0-9]([a-z0-9_-]*[a-z0-9])?$'
    ),
    constraint agent_profiles_temperature_valid check (
        temperature is null or (temperature >= 0 and temperature <= 2)
    ),
    constraint agent_profiles_max_tokens_valid check (
        max_tokens is null or max_tokens > 0
    )
);

create unique index if not exists agent_profiles_profile_key_active_unique
    on agent_profiles (profile_key)
    where deleted_at is null;

create index if not exists agent_profiles_cursor_idx
    on agent_profiles (created_at desc, id desc)
    where deleted_at is null;

create index if not exists agent_profiles_status_cursor_idx
    on agent_profiles (status, created_at desc, id desc)
    where deleted_at is null;

create table if not exists route_definitions (
    id uuid primary key default gen_random_uuid(),
    route_key varchar(128) not null,
    display_name varchar(200) not null,
    description text,
    status varchar(32) not null default 'active'
        check (status in ('active', 'disabled', 'deleted')),
    selection_strategy varchar(64) not null default 'default'
        check (selection_strategy in ('explicit', 'rules', 'default')),
    agent_profile_id uuid references agent_profiles(id) on delete set null,
    metadata jsonb not null default '{}'::jsonb,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    deleted_at timestamptz,
    version bigint not null default 1,
    constraint route_definitions_route_key_valid check (
        length(route_key) between 1 and 128
        and route_key ~ '^[a-z0-9]([a-z0-9_-]*[a-z0-9])?$'
    )
);

create unique index if not exists route_definitions_route_key_active_unique
    on route_definitions (route_key)
    where deleted_at is null;

create index if not exists route_definitions_cursor_idx
    on route_definitions (created_at desc, id desc)
    where deleted_at is null;

create index if not exists route_definitions_status_cursor_idx
    on route_definitions (status, created_at desc, id desc)
    where deleted_at is null;

create table if not exists routing_policies (
    id uuid primary key default gen_random_uuid(),
    application_id uuid references applications(id) on delete cascade,
    external_tenant_id varchar(256),
    route_id uuid not null references route_definitions(id) on delete cascade,
    provider_id uuid not null references providers(id) on delete cascade,
    provider_model_id uuid not null references provider_models(id) on delete cascade,
    priority integer not null default 100 check (priority >= 0),
    weight integer not null default 1 check (weight >= 0),
    cost_weight double precision not null default 0 check (cost_weight >= 0),
    latency_weight double precision not null default 0 check (latency_weight >= 0),
    quality_weight double precision not null default 0 check (quality_weight >= 0),
    privacy_class varchar(64),
    required_capabilities text[] not null default '{}',
    maximum_cost_per_request double precision,
    maximum_input_tokens bigint,
    maximum_output_tokens bigint,
    timeout_ms integer,
    retry_policy jsonb not null default '{}'::jsonb,
    status varchar(32) not null default 'active'
        check (status in ('active', 'disabled', 'deleted')),
    metadata jsonb not null default '{}'::jsonb,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    deleted_at timestamptz,
    version bigint not null default 1,
    constraint routing_policies_external_tenant_valid check (
        external_tenant_id is null
        or (
            length(external_tenant_id) between 1 and 256
            and external_tenant_id !~ '[[:cntrl:]]'
        )
    ),
    constraint routing_policies_token_limits_valid check (
        (maximum_input_tokens is null or maximum_input_tokens > 0)
        and (maximum_output_tokens is null or maximum_output_tokens > 0)
    ),
    constraint routing_policies_timeout_valid check (
        timeout_ms is null or timeout_ms > 0
    )
);

create index if not exists routing_policies_route_scope_idx
    on routing_policies (
        route_id,
        application_id,
        coalesce(external_tenant_id, ''),
        status,
        priority,
        created_at desc,
        id desc
    )
    where deleted_at is null;

create index if not exists routing_policies_provider_model_idx
    on routing_policies (provider_id, provider_model_id, status)
    where deleted_at is null;

create index if not exists routing_policies_cursor_idx
    on routing_policies (created_at desc, id desc)
    where deleted_at is null;

create table if not exists provider_runtime_policies (
    id uuid not null default gen_random_uuid(),
    provider_id uuid primary key references providers(id) on delete cascade,
    connect_timeout_ms integer not null default 5000 check (connect_timeout_ms > 0),
    request_timeout_ms integer not null default 120000 check (request_timeout_ms > 0),
    stream_idle_timeout_ms integer not null default 30000 check (stream_idle_timeout_ms > 0),
    max_concurrent_requests integer not null default 100 check (max_concurrent_requests > 0),
    max_concurrent_streams integer not null default 50 check (max_concurrent_streams > 0),
    retry_limit integer not null default 2 check (retry_limit >= 0),
    retry_base_delay_ms integer not null default 100 check (retry_base_delay_ms >= 0),
    retry_max_delay_ms integer not null default 2000 check (retry_max_delay_ms >= 0),
    circuit_failure_threshold integer not null default 5 check (circuit_failure_threshold > 0),
    circuit_open_duration_ms integer not null default 30000 check (circuit_open_duration_ms > 0),
    status varchar(32) not null default 'active'
        check (status in ('active', 'disabled')),
    updated_at timestamptz not null default now(),
    version bigint not null default 1
);

create unique index if not exists provider_runtime_policies_id_unique
    on provider_runtime_policies (id);

create table if not exists provider_health_snapshots (
    id uuid primary key default gen_random_uuid(),
    provider_id uuid not null references providers(id) on delete cascade,
    provider_model_id uuid references provider_models(id) on delete cascade,
    status varchar(32) not null default 'unknown'
        check (status in ('healthy', 'degraded', 'unhealthy', 'unknown')),
    circuit_state varchar(32) not null default 'closed'
        check (circuit_state in ('closed', 'open', 'half_open')),
    observed_at timestamptz not null default now(),
    failure_count integer not null default 0 check (failure_count >= 0),
    latency_ms integer,
    metadata jsonb not null default '{}'::jsonb
);

create index if not exists provider_health_latest_idx
    on provider_health_snapshots (provider_id, provider_model_id, observed_at desc);

create table if not exists execution_attempts (
    id uuid primary key default gen_random_uuid(),
    request_id varchar(128) not null,
    execution_id uuid not null,
    attempt_number integer not null check (attempt_number > 0),
    application_id uuid references applications(id) on delete set null,
    external_tenant_id varchar(256),
    external_user_id varchar(256),
    route_id uuid references route_definitions(id) on delete set null,
    provider_id uuid references providers(id) on delete set null,
    provider_model_id uuid references provider_models(id) on delete set null,
    credential_id uuid references provider_credentials(id) on delete set null,
    status varchar(32) not null check (
        status in ('started', 'succeeded', 'failed', 'cancelled')
    ),
    failure_class varchar(128),
    provider_status_code integer,
    started_at timestamptz not null default now(),
    completed_at timestamptz,
    latency_ms bigint,
    input_tokens bigint,
    output_tokens bigint,
    cached_input_tokens bigint,
    reasoning_tokens bigint,
    total_tokens bigint,
    estimated_cost double precision,
    provider_request_id varchar(256),
    metadata jsonb not null default '{}'::jsonb,
    constraint execution_attempts_external_ids_valid check (
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

create unique index if not exists execution_attempts_execution_attempt_number_unique
    on execution_attempts (execution_id, attempt_number);

create index if not exists execution_attempts_request_idx
    on execution_attempts (request_id, started_at desc);

create index if not exists execution_attempts_runtime_idx
    on execution_attempts (provider_id, provider_model_id, status, started_at desc);

create table if not exists usage_records (
    id uuid primary key default gen_random_uuid(),
    request_id varchar(128) not null,
    execution_id uuid not null,
    attempt_id uuid references execution_attempts(id) on delete set null,
    application_id uuid references applications(id) on delete set null,
    external_tenant_id varchar(256),
    external_user_id varchar(256),
    provider_id uuid references providers(id) on delete set null,
    provider_model_id uuid references provider_models(id) on delete set null,
    credential_id uuid references provider_credentials(id) on delete set null,
    input_tokens bigint,
    output_tokens bigint,
    cached_input_tokens bigint,
    reasoning_tokens bigint,
    total_tokens bigint,
    estimated_input_cost double precision,
    estimated_output_cost double precision,
    estimated_total_cost double precision,
    currency varchar(3),
    occurred_at timestamptz not null default now(),
    metadata jsonb not null default '{}'::jsonb
);

create index if not exists usage_records_runtime_idx
    on usage_records (application_id, provider_id, provider_model_id, occurred_at desc);

create index if not exists usage_records_execution_idx
    on usage_records (execution_id, occurred_at desc);

drop trigger if exists agent_profiles_bump_version on agent_profiles;
create trigger agent_profiles_bump_version
before update on agent_profiles
for each row execute function moira_bump_resource_version();

drop trigger if exists route_definitions_bump_version on route_definitions;
create trigger route_definitions_bump_version
before update on route_definitions
for each row execute function moira_bump_resource_version();

drop trigger if exists routing_policies_bump_version on routing_policies;
create trigger routing_policies_bump_version
before update on routing_policies
for each row execute function moira_bump_resource_version();

drop trigger if exists provider_runtime_policies_bump_version on provider_runtime_policies;
create trigger provider_runtime_policies_bump_version
before update on provider_runtime_policies
for each row execute function moira_bump_resource_version();

drop trigger if exists agent_profiles_runtime_config_notify on agent_profiles;
create trigger agent_profiles_runtime_config_notify
after insert or update or delete on agent_profiles
for each row execute function notify_moira_runtime_config_change();

drop trigger if exists route_definitions_runtime_config_notify on route_definitions;
create trigger route_definitions_runtime_config_notify
after insert or update or delete on route_definitions
for each row execute function notify_moira_runtime_config_change();

drop trigger if exists routing_policies_runtime_config_notify on routing_policies;
create trigger routing_policies_runtime_config_notify
after insert or update or delete on routing_policies
for each row execute function notify_moira_runtime_config_change();

drop trigger if exists provider_runtime_policies_runtime_config_notify on provider_runtime_policies;
create trigger provider_runtime_policies_runtime_config_notify
after insert or update or delete on provider_runtime_policies
for each row execute function notify_moira_runtime_config_change();

insert into route_definitions (id, route_key, display_name, description, selection_strategy, metadata)
values (
    gen_random_uuid(),
    'general',
    'General',
    'Default general-purpose runtime route.',
    'default',
    jsonb_build_object('seeded_by', '0005_provider_runtime')
)
on conflict do nothing;
