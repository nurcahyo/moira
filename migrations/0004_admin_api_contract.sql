alter table applications
    add column if not exists version bigint not null default 1;

alter table providers
    add column if not exists version bigint not null default 1;

alter table provider_models
    add column if not exists version bigint not null default 1;

alter table provider_credentials
    add column if not exists version bigint not null default 1,
    add column if not exists display_name varchar(256);

alter table trusted_jwt_issuers
    add column if not exists version bigint not null default 1;

create or replace function moira_bump_resource_version()
returns trigger language plpgsql as $$
begin
    new.updated_at = now();
    new.version = old.version + 1;
    return new;
end;
$$;

drop trigger if exists applications_bump_version on applications;
create trigger applications_bump_version
before update on applications
for each row execute function moira_bump_resource_version();

drop trigger if exists providers_bump_version on providers;
create trigger providers_bump_version
before update on providers
for each row execute function moira_bump_resource_version();

drop trigger if exists provider_models_bump_version on provider_models;
create trigger provider_models_bump_version
before update on provider_models
for each row execute function moira_bump_resource_version();

drop trigger if exists provider_credentials_bump_version on provider_credentials;
create trigger provider_credentials_bump_version
before update on provider_credentials
for each row execute function moira_bump_resource_version();

drop trigger if exists trusted_jwt_issuers_bump_version on trusted_jwt_issuers;
create trigger trusted_jwt_issuers_bump_version
before update on trusted_jwt_issuers
for each row execute function moira_bump_resource_version();

create index if not exists applications_cursor_idx
    on applications (created_at desc, id desc)
    where deleted_at is null;

create index if not exists applications_status_cursor_idx
    on applications (status, created_at desc, id desc)
    where deleted_at is null;

create index if not exists providers_cursor_idx
    on providers (created_at desc, id desc)
    where deleted_at is null;

create index if not exists providers_status_cursor_idx
    on providers (status, created_at desc, id desc)
    where deleted_at is null;

create index if not exists provider_models_cursor_idx
    on provider_models (created_at desc, id desc)
    where deleted_at is null;

create index if not exists provider_models_provider_cursor_idx
    on provider_models (provider_id, created_at desc, id desc)
    where deleted_at is null;

create index if not exists provider_credentials_cursor_idx
    on provider_credentials (created_at desc, id desc)
    where deleted_at is null;

create index if not exists provider_credentials_filters_cursor_idx
    on provider_credentials (
        provider_id,
        credential_type,
        scope_type,
        application_id,
        external_user_id,
        external_tenant_id,
        status,
        created_at desc,
        id desc
    )
    where deleted_at is null;

create index if not exists trusted_jwt_issuers_cursor_idx
    on trusted_jwt_issuers (created_at desc, id desc)
    where deleted_at is null;

create index if not exists system_api_keys_cursor_idx
    on system_api_keys (created_at desc, id desc)
    where deleted_at is null;

create index if not exists consumer_api_keys_cursor_idx
    on consumer_api_keys (application_id, created_at desc, id desc)
    where deleted_at is null;

create index if not exists audit_logs_filters_cursor_idx
    on audit_logs (application_id, resource_type, action, result, occurred_at desc, id desc);

create or replace function notify_moira_runtime_config_change()
returns trigger language plpgsql as $$
declare
    changed_id uuid;
begin
    changed_id = case when tg_op = 'DELETE' then old.id else new.id end;
    perform pg_notify(
        'moira_runtime_config',
        json_build_object(
            'resource_type', tg_table_name,
            'resource_id', changed_id::text
        )::text
    );
    if tg_op = 'DELETE' then
        return old;
    end if;

    return new;
end;
$$;

drop trigger if exists applications_runtime_config_notify on applications;
create trigger applications_runtime_config_notify
after insert or update or delete on applications
for each row execute function notify_moira_runtime_config_change();

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

drop trigger if exists trusted_jwt_issuers_runtime_config_notify on trusted_jwt_issuers;
create trigger trusted_jwt_issuers_runtime_config_notify
after insert or update or delete on trusted_jwt_issuers
for each row execute function notify_moira_runtime_config_change();

drop trigger if exists system_api_keys_runtime_config_notify on system_api_keys;
create trigger system_api_keys_runtime_config_notify
after insert or update or delete on system_api_keys
for each row execute function notify_moira_runtime_config_change();

drop trigger if exists consumer_api_keys_runtime_config_notify on consumer_api_keys;
create trigger consumer_api_keys_runtime_config_notify
after insert or update or delete on consumer_api_keys
for each row execute function notify_moira_runtime_config_change();
