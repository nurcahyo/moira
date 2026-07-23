create table if not exists application_conversation_policies (
    id uuid not null default gen_random_uuid(),
    application_id uuid primary key references applications(id) on delete cascade,
    conversations_enabled boolean not null default true,
    conversation_content_persistence varchar(32) not null default 'plain_content'
        check (conversation_content_persistence in ('none', 'metadata_only', 'plain_content', 'encrypted_content')),
    default_retention_days integer not null default 90 check (default_retention_days >= 0),
    maximum_retention_days integer not null default 365 check (maximum_retention_days >= 0),
    history_strategy varchar(64) not null default 'summary_plus_recent'
        check (history_strategy in ('recent_messages', 'summary_plus_recent', 'full_until_limit')),
    maximum_recent_messages integer not null default 24 check (maximum_recent_messages > 0),
    maximum_history_tokens integer not null default 12000 check (maximum_history_tokens > 0),
    summarization_enabled boolean not null default false,
    summary_trigger_tokens integer not null default 8000 check (summary_trigger_tokens > 0),
    summary_target_tokens integer not null default 1000 check (summary_target_tokens > 0),
    minimum_messages_since_summary integer not null default 8 check (minimum_messages_since_summary >= 0),
    memory_enabled boolean not null default false,
    memory_extraction_enabled boolean not null default false,
    memory_retrieval_enabled boolean not null default false,
    memory_consent_mode varchar(64) not null default 'explicit_only'
        check (memory_consent_mode in ('disabled', 'explicit_only', 'application_managed', 'automatic_with_user_controls')),
    rag_enabled boolean not null default false,
    default_collection_ids uuid[] not null default array[]::uuid[],
    caller_can_create_conversations boolean not null default true,
    caller_can_delete_conversations boolean not null default true,
    caller_can_export_conversations boolean not null default false,
    protected_instruction_policy varchar(64) not null default 'exclude_from_exports',
    metadata jsonb not null default '{}'::jsonb,
    updated_at timestamptz not null default now(),
    version bigint not null default 1
);

create unique index if not exists application_conversation_policies_id_unique
    on application_conversation_policies (id);

create table if not exists application_memory_policies (
    id uuid not null default gen_random_uuid(),
    application_id uuid primary key references applications(id) on delete cascade,
    enabled boolean not null default false,
    consent_mode varchar(64) not null default 'explicit_only'
        check (consent_mode in ('disabled', 'explicit_only', 'application_managed', 'automatic_with_user_controls')),
    allowed_memory_types text[] not null default array[
        'preference', 'fact', 'goal', 'constraint', 'relationship',
        'project_context', 'decision', 'instruction', 'temporary_state'
    ]::text[],
    allowed_sensitivity_levels text[] not null default array['normal']::text[],
    automatic_extraction_enabled boolean not null default false,
    automatic_retrieval_enabled boolean not null default false,
    manual_memory_enabled boolean not null default false,
    minimum_extraction_confidence double precision not null default 0.8
        check (minimum_extraction_confidence between 0 and 1),
    minimum_retrieval_confidence double precision not null default 0.6
        check (minimum_retrieval_confidence between 0 and 1),
    maximum_memory_count_per_user integer not null default 1000 check (maximum_memory_count_per_user > 0),
    maximum_memory_tokens_per_request integer not null default 1500 check (maximum_memory_tokens_per_request > 0),
    default_ttl_days integer check (default_ttl_days is null or default_ttl_days >= 0),
    maximum_ttl_days integer check (maximum_ttl_days is null or maximum_ttl_days >= 0),
    user_can_list boolean not null default true,
    user_can_edit boolean not null default true,
    user_can_delete boolean not null default true,
    user_can_disable boolean not null default true,
    metadata jsonb not null default '{}'::jsonb,
    updated_at timestamptz not null default now(),
    version bigint not null default 1
);

create unique index if not exists application_memory_policies_id_unique
    on application_memory_policies (id);

create table if not exists application_retrieval_policies (
    id uuid not null default gen_random_uuid(),
    application_id uuid primary key references applications(id) on delete cascade,
    enabled boolean not null default false,
    memory_retrieval_enabled boolean not null default false,
    rag_retrieval_enabled boolean not null default false,
    allowed_collection_ids uuid[] not null default array[]::uuid[],
    default_collection_ids uuid[] not null default array[]::uuid[],
    maximum_memory_results integer not null default 10 check (maximum_memory_results > 0),
    maximum_chunk_results integer not null default 8 check (maximum_chunk_results > 0),
    maximum_memory_tokens integer not null default 1500 check (maximum_memory_tokens > 0),
    maximum_rag_tokens integer not null default 4000 check (maximum_rag_tokens > 0),
    semantic_weight double precision not null default 0.7 check (semantic_weight >= 0),
    keyword_weight double precision not null default 0.2 check (keyword_weight >= 0),
    recency_weight double precision not null default 0.05 check (recency_weight >= 0),
    importance_weight double precision not null default 0.05 check (importance_weight >= 0),
    minimum_memory_score double precision not null default 0.5 check (minimum_memory_score between 0 and 1),
    minimum_chunk_score double precision not null default 0.5 check (minimum_chunk_score between 0 and 1),
    maximum_chunks_per_document integer not null default 3 check (maximum_chunks_per_document > 0),
    diversity_enabled boolean not null default true,
    metadata jsonb not null default '{}'::jsonb,
    updated_at timestamptz not null default now(),
    version bigint not null default 1
);

create unique index if not exists application_retrieval_policies_id_unique
    on application_retrieval_policies (id);

create table if not exists application_embedding_policies (
    id uuid not null default gen_random_uuid(),
    application_id uuid primary key references applications(id) on delete cascade,
    embedding_provider_id uuid references providers(id) on delete set null,
    embedding_model_id uuid references provider_models(id) on delete set null,
    embedding_dimension integer check (embedding_dimension is null or embedding_dimension > 0),
    batch_size integer not null default 32 check (batch_size > 0),
    maximum_input_tokens integer not null default 8192 check (maximum_input_tokens > 0),
    timeout_ms integer not null default 60000 check (timeout_ms > 0),
    memory_embeddings_enabled boolean not null default false,
    rag_embeddings_enabled boolean not null default false,
    failure_behavior varchar(64) not null default 'continue_without_semantic_retrieval',
    metadata jsonb not null default '{}'::jsonb,
    updated_at timestamptz not null default now(),
    version bigint not null default 1
);

create unique index if not exists application_embedding_policies_id_unique
    on application_embedding_policies (id);

create table if not exists conversations (
    id uuid primary key default gen_random_uuid(),
    public_id varchar(80) not null unique,
    application_id uuid not null references applications(id) on delete cascade,
    external_tenant_id varchar(256),
    external_user_id varchar(256),
    title varchar(512),
    status varchar(32) not null default 'active'
        check (status in ('active', 'archived', 'deleted')),
    conversation_policy_id uuid references application_conversation_policies(id) on delete set null,
    metadata jsonb not null default '{}'::jsonb,
    message_count bigint not null default 0,
    last_message_at timestamptz,
    retention_expires_at timestamptz,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    archived_at timestamptz,
    deleted_at timestamptz,
    version bigint not null default 1,
    constraint conversations_public_id_valid check (public_id ~ '^conv_[0-9a-fA-F-]{36}$'),
    constraint conversations_external_ids_valid check (
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

create index if not exists conversations_owner_cursor_idx
    on conversations (
        application_id,
        coalesce(external_tenant_id, ''),
        coalesce(external_user_id, ''),
        status,
        updated_at desc,
        id desc
    )
    where deleted_at is null;

create table if not exists conversation_messages (
    id uuid primary key default gen_random_uuid(),
    public_id varchar(80) not null unique,
    conversation_id uuid not null references conversations(id) on delete cascade,
    response_id uuid references responses(id) on delete set null,
    execution_id uuid,
    role varchar(32) not null check (role in ('system', 'developer', 'user', 'assistant', 'tool')),
    message_type varchar(32) not null default 'input'
        check (message_type in ('input', 'output', 'summary', 'tool_call', 'tool_result', 'marker')),
    sequence_number bigint not null,
    content_encrypted bytea,
    content_plain text,
    content_hash varchar(128) not null,
    content_size_bytes bigint not null default 0 check (content_size_bytes >= 0),
    tool_name varchar(256),
    tool_call_id varchar(256),
    parent_message_id uuid references conversation_messages(id) on delete set null,
    token_count bigint,
    created_at timestamptz not null default now(),
    deleted_at timestamptz,
    metadata jsonb not null default '{}'::jsonb,
    constraint conversation_messages_public_id_valid check (public_id ~ '^msg_[0-9a-fA-F-]{36}$'),
    constraint conversation_messages_sequence_unique unique (conversation_id, sequence_number)
);

create index if not exists conversation_messages_conversation_sequence_idx
    on conversation_messages (conversation_id, sequence_number, id)
    where deleted_at is null;

create index if not exists conversation_messages_response_idx
    on conversation_messages (response_id)
    where response_id is not null;

create table if not exists conversation_summaries (
    id uuid primary key default gen_random_uuid(),
    conversation_id uuid not null references conversations(id) on delete cascade,
    summary_version bigint not null,
    covers_through_sequence bigint not null,
    summary_text_encrypted bytea,
    summary_text_plain text,
    summary_hash varchar(128) not null,
    token_count bigint,
    model_provider_id uuid references providers(id) on delete set null,
    provider_model_id uuid references provider_models(id) on delete set null,
    prompt_template_version varchar(64) not null default 'v1',
    created_at timestamptz not null default now(),
    superseded_at timestamptz,
    metadata jsonb not null default '{}'::jsonb,
    constraint conversation_summary_boundary_unique unique (conversation_id, covers_through_sequence)
);

create index if not exists conversation_summaries_active_idx
    on conversation_summaries (conversation_id, covers_through_sequence desc)
    where superseded_at is null;

alter table responses
    add column if not exists conversation_id uuid references conversations(id) on delete set null;

create index if not exists responses_conversation_idx
    on responses (conversation_id, created_at desc)
    where conversation_id is not null;

create table if not exists memory_records (
    id uuid primary key default gen_random_uuid(),
    public_id varchar(80) not null unique,
    application_id uuid not null references applications(id) on delete cascade,
    external_tenant_id varchar(256),
    external_user_id varchar(256),
    conversation_id uuid references conversations(id) on delete set null,
    memory_scope varchar(64) not null default 'conversation'
        check (memory_scope in ('conversation', 'user_application', 'tenant_application', 'application')),
    memory_type varchar(64) not null check (memory_type in (
        'preference', 'fact', 'goal', 'constraint', 'relationship',
        'project_context', 'decision', 'instruction', 'temporary_state'
    )),
    memory_key varchar(256),
    content_encrypted bytea,
    content_plain text,
    content_hash varchar(128) not null,
    importance double precision not null default 0.5 check (importance between 0 and 1),
    confidence double precision not null default 1.0 check (confidence between 0 and 1),
    sensitivity varchar(32) not null default 'normal'
        check (sensitivity in ('normal', 'personal', 'restricted')),
    status varchar(32) not null default 'active'
        check (status in ('candidate', 'active', 'rejected', 'superseded', 'expired', 'deleted')),
    source_message_ids uuid[] not null default array[]::uuid[],
    source_response_id uuid references responses(id) on delete set null,
    source_extraction_run_id uuid,
    valid_from timestamptz,
    valid_until timestamptz,
    last_confirmed_at timestamptz,
    last_used_at timestamptz,
    use_count bigint not null default 0,
    contradicts_memory_id uuid references memory_records(id) on delete set null,
    resolution_status varchar(64),
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    deleted_at timestamptz,
    version bigint not null default 1,
    metadata jsonb not null default '{}'::jsonb,
    constraint memory_records_public_id_valid check (public_id ~ '^mem_[0-9a-fA-F-]{36}$'),
    constraint memory_records_scope_valid check (
        (memory_scope = 'conversation' and conversation_id is not null)
        or (memory_scope = 'user_application' and external_user_id is not null)
        or (memory_scope = 'tenant_application' and external_tenant_id is not null)
        or (memory_scope = 'application')
    )
);

create index if not exists memory_records_scope_cursor_idx
    on memory_records (
        application_id,
        coalesce(external_tenant_id, ''),
        coalesce(external_user_id, ''),
        memory_scope,
        memory_type,
        status,
        updated_at desc,
        id desc
    )
    where deleted_at is null;

create table if not exists memory_embeddings (
    memory_id uuid not null references memory_records(id) on delete cascade,
    embedding_model_id uuid references provider_models(id) on delete set null,
    embedding_version integer not null default 1,
    dimension integer not null check (dimension > 0),
    embedding vector(1536),
    created_at timestamptz not null default now(),
    superseded_at timestamptz,
    primary key (memory_id, embedding_version)
);

create index if not exists memory_embeddings_active_hnsw_idx
    on memory_embeddings using hnsw (embedding vector_cosine_ops)
    where superseded_at is null and embedding is not null;

create table if not exists memory_extraction_runs (
    id uuid primary key default gen_random_uuid(),
    conversation_id uuid references conversations(id) on delete set null,
    response_id uuid references responses(id) on delete set null,
    status varchar(32) not null default 'pending'
        check (status in ('pending', 'running', 'completed', 'failed')),
    input_message_ids uuid[] not null default array[]::uuid[],
    candidate_count integer not null default 0,
    accepted_count integer not null default 0,
    rejected_count integer not null default 0,
    provider_id uuid references providers(id) on delete set null,
    provider_model_id uuid references provider_models(id) on delete set null,
    started_at timestamptz not null default now(),
    completed_at timestamptz,
    failure_class varchar(128),
    metadata jsonb not null default '{}'::jsonb
);

alter table memory_records
    add constraint memory_records_extraction_run_fk
    foreign key (source_extraction_run_id) references memory_extraction_runs(id) on delete set null;

create table if not exists rag_collections (
    id uuid primary key default gen_random_uuid(),
    public_id varchar(90) not null unique,
    application_id uuid not null references applications(id) on delete cascade,
    external_tenant_id varchar(256),
    collection_key varchar(200) not null,
    display_name varchar(256) not null,
    description text,
    status varchar(32) not null default 'active'
        check (status in ('active', 'disabled', 'deleted')),
    visibility varchar(32) not null default 'application'
        check (visibility in ('application', 'tenant', 'restricted')),
    metadata jsonb not null default '{}'::jsonb,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    deleted_at timestamptz,
    version bigint not null default 1,
    constraint rag_collections_public_id_valid check (public_id ~ '^collection_[0-9a-fA-F-]{36}$')
);

create unique index if not exists rag_collections_key_active_unique
    on rag_collections (application_id, coalesce(external_tenant_id, ''), collection_key)
    where deleted_at is null;

create index if not exists rag_collections_visible_idx
    on rag_collections (application_id, coalesce(external_tenant_id, ''), status, visibility, created_at desc, id desc)
    where deleted_at is null;

create table if not exists rag_documents (
    id uuid primary key default gen_random_uuid(),
    public_id varchar(80) not null unique,
    collection_id uuid not null references rag_collections(id) on delete cascade,
    external_document_id varchar(256),
    title varchar(512) not null,
    source_type varchar(64) not null,
    source_uri text,
    mime_type varchar(128) not null,
    status varchar(32) not null default 'active'
        check (status in ('active', 'disabled', 'deleted')),
    current_version_id uuid,
    metadata jsonb not null default '{}'::jsonb,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    deleted_at timestamptz,
    version bigint not null default 1,
    constraint rag_documents_public_id_valid check (public_id ~ '^doc_[0-9a-fA-F-]{36}$')
);

create index if not exists rag_documents_collection_cursor_idx
    on rag_documents (collection_id, status, created_at desc, id desc)
    where deleted_at is null;

create table if not exists rag_document_versions (
    id uuid primary key default gen_random_uuid(),
    document_id uuid not null references rag_documents(id) on delete cascade,
    version_number integer not null,
    content_encrypted bytea,
    content_plain text,
    content_hash varchar(128) not null,
    content_size_bytes bigint not null default 0 check (content_size_bytes >= 0),
    source_etag varchar(512),
    source_last_modified timestamptz,
    ingestion_status varchar(32) not null default 'pending'
        check (ingestion_status in ('pending', 'downloading', 'parsing', 'chunking', 'embedding', 'indexed', 'failed', 'superseded')),
    created_at timestamptz not null default now(),
    superseded_at timestamptz,
    metadata jsonb not null default '{}'::jsonb,
    constraint rag_document_versions_unique unique (document_id, version_number)
);

alter table rag_documents
    add constraint rag_documents_current_version_fk
    foreign key (current_version_id) references rag_document_versions(id) on delete set null;

create table if not exists rag_chunks (
    id uuid primary key default gen_random_uuid(),
    public_id varchar(90) not null unique,
    document_version_id uuid not null references rag_document_versions(id) on delete cascade,
    collection_id uuid not null references rag_collections(id) on delete cascade,
    chunk_index integer not null,
    chunk_text_encrypted bytea,
    chunk_text_plain text,
    chunk_hash varchar(128) not null,
    token_count bigint,
    start_offset integer,
    end_offset integer,
    section_title varchar(512),
    metadata jsonb not null default '{}'::jsonb,
    created_at timestamptz not null default now(),
    constraint rag_chunks_public_id_valid check (public_id ~ '^chunk_[0-9a-fA-F-]{36}$'),
    constraint rag_chunks_version_index_unique unique (document_version_id, chunk_index)
);

create index if not exists rag_chunks_collection_idx
    on rag_chunks (collection_id, document_version_id, chunk_index);

create table if not exists rag_chunk_embeddings (
    chunk_id uuid not null references rag_chunks(id) on delete cascade,
    embedding_model_id uuid references provider_models(id) on delete set null,
    embedding_version integer not null default 1,
    dimension integer not null check (dimension > 0),
    embedding vector(1536),
    created_at timestamptz not null default now(),
    superseded_at timestamptz,
    primary key (chunk_id, embedding_version)
);

create index if not exists rag_chunk_embeddings_active_hnsw_idx
    on rag_chunk_embeddings using hnsw (embedding vector_cosine_ops)
    where superseded_at is null and embedding is not null;

create table if not exists rag_ingestion_runs (
    id uuid primary key default gen_random_uuid(),
    document_id uuid references rag_documents(id) on delete set null,
    document_version_id uuid references rag_document_versions(id) on delete set null,
    status varchar(32) not null default 'pending'
        check (status in ('pending', 'running', 'completed', 'failed')),
    content_hash varchar(128),
    chunk_count integer not null default 0,
    embedded_chunk_count integer not null default 0,
    started_at timestamptz not null default now(),
    completed_at timestamptz,
    failure_class varchar(128),
    metadata jsonb not null default '{}'::jsonb
);

create table if not exists context_plans (
    id uuid primary key default gen_random_uuid(),
    execution_id uuid not null,
    conversation_id uuid references conversations(id) on delete set null,
    strategy varchar(64) not null,
    model_context_limit bigint,
    reserved_output_tokens bigint,
    estimated_input_tokens bigint,
    included_message_ids uuid[] not null default array[]::uuid[],
    included_summary_id uuid references conversation_summaries(id) on delete set null,
    included_memory_ids uuid[] not null default array[]::uuid[],
    included_chunk_ids uuid[] not null default array[]::uuid[],
    excluded_counts jsonb not null default '{}'::jsonb,
    truncation_reason varchar(128),
    created_at timestamptz not null default now(),
    metadata jsonb not null default '{}'::jsonb
);

create index if not exists context_plans_execution_idx
    on context_plans (execution_id);

create table if not exists retrieval_runs (
    id uuid primary key default gen_random_uuid(),
    execution_id uuid,
    conversation_id uuid references conversations(id) on delete set null,
    query_hash varchar(128),
    embedding_model_id uuid references provider_models(id) on delete set null,
    memory_candidate_count integer not null default 0,
    memory_returned_count integer not null default 0,
    chunk_candidate_count integer not null default 0,
    chunk_returned_count integer not null default 0,
    latency_ms bigint,
    status varchar(32) not null default 'completed'
        check (status in ('completed', 'failed')),
    failure_class varchar(128),
    created_at timestamptz not null default now(),
    metadata jsonb not null default '{}'::jsonb
);

create index if not exists retrieval_runs_execution_idx
    on retrieval_runs (execution_id);

drop trigger if exists application_conversation_policies_bump_version on application_conversation_policies;
create trigger application_conversation_policies_bump_version
before update on application_conversation_policies
for each row execute function moira_bump_resource_version();

drop trigger if exists application_memory_policies_bump_version on application_memory_policies;
create trigger application_memory_policies_bump_version
before update on application_memory_policies
for each row execute function moira_bump_resource_version();

drop trigger if exists application_retrieval_policies_bump_version on application_retrieval_policies;
create trigger application_retrieval_policies_bump_version
before update on application_retrieval_policies
for each row execute function moira_bump_resource_version();

drop trigger if exists application_embedding_policies_bump_version on application_embedding_policies;
create trigger application_embedding_policies_bump_version
before update on application_embedding_policies
for each row execute function moira_bump_resource_version();

drop trigger if exists conversations_bump_version on conversations;
create trigger conversations_bump_version
before update on conversations
for each row execute function moira_bump_resource_version();

drop trigger if exists memory_records_bump_version on memory_records;
create trigger memory_records_bump_version
before update on memory_records
for each row execute function moira_bump_resource_version();

drop trigger if exists rag_collections_bump_version on rag_collections;
create trigger rag_collections_bump_version
before update on rag_collections
for each row execute function moira_bump_resource_version();

drop trigger if exists rag_documents_bump_version on rag_documents;
create trigger rag_documents_bump_version
before update on rag_documents
for each row execute function moira_bump_resource_version();

drop trigger if exists application_conversation_policies_runtime_config_notify on application_conversation_policies;
create trigger application_conversation_policies_runtime_config_notify
after insert or update or delete on application_conversation_policies
for each row execute function notify_moira_runtime_config_change();

drop trigger if exists application_memory_policies_runtime_config_notify on application_memory_policies;
create trigger application_memory_policies_runtime_config_notify
after insert or update or delete on application_memory_policies
for each row execute function notify_moira_runtime_config_change();

drop trigger if exists application_retrieval_policies_runtime_config_notify on application_retrieval_policies;
create trigger application_retrieval_policies_runtime_config_notify
after insert or update or delete on application_retrieval_policies
for each row execute function notify_moira_runtime_config_change();

drop trigger if exists application_embedding_policies_runtime_config_notify on application_embedding_policies;
create trigger application_embedding_policies_runtime_config_notify
after insert or update or delete on application_embedding_policies
for each row execute function notify_moira_runtime_config_change();

drop trigger if exists conversations_runtime_config_notify on conversations;
create trigger conversations_runtime_config_notify
after insert or update or delete on conversations
for each row execute function notify_moira_runtime_config_change();

drop trigger if exists memory_records_runtime_config_notify on memory_records;
create trigger memory_records_runtime_config_notify
after insert or update or delete on memory_records
for each row execute function notify_moira_runtime_config_change();

drop trigger if exists rag_collections_runtime_config_notify on rag_collections;
create trigger rag_collections_runtime_config_notify
after insert or update or delete on rag_collections
for each row execute function notify_moira_runtime_config_change();

drop trigger if exists rag_documents_runtime_config_notify on rag_documents;
create trigger rag_documents_runtime_config_notify
after insert or update or delete on rag_documents
for each row execute function notify_moira_runtime_config_change();
