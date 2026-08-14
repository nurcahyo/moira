-- Issue #214 (plan 12 §3 / §5) — Agent platform: skills, evaluations, flows.
--
-- Workstream F, sub-plan 1 of 3: SCHEMA + CRUD only. There is no execution engine here —
-- the flow orchestrator and the Rig tool-call loop that would make a skill callable are
-- deferred to sub-plans 2/3 and depend on rig tools (#84). Every table below is a registry
-- or an append-only record; none of them is wired into any runtime cache yet.
--
-- # These tables are NOT runtime configuration
--
-- Unlike `agent_profiles`/`route_definitions`/`routing_policies`, none of the tables created
-- here fires `notify_moira_runtime_config_change()`. Nothing in `ProviderRuntimeCache`, the
-- runtime-config cache, or the circuit-breaker classifier (`src/infra/db.rs`) keys on a skill,
-- an eval suite, or a flow, so publishing on `moira_runtime_config` would only make every
-- replica drop its provider clients on a write that cannot change provider configuration —
-- exactly the F51/F52 defect that migrations 0022/0023/0024 removed. The classification guard
-- `tests/runtime_notify_inventory.rs` enforces set-equality between the trigger inventory and
-- `TRIGGERED_RESOURCE_TYPES`; adding a NOTIFY trigger here without classifying the table there
-- would fail it. When an execution engine later needs cache invalidation on these rows, add the
-- trigger and the classification together, per that migration's reversal note.
--
-- The `moira_bump_resource_version` trigger (independent of NOTIFY: it maintains the `version`
-- ETag for `If-Match`) IS attached to the three versioned registries — `skills`, `eval_suites`,
-- `agent_flows` — exactly as `0005` attaches it to `agent_profiles`.
--
-- # Soft-delete + optimistic concurrency
--
-- Named registries (`skills`, `eval_suites`, `agent_flows`) carry `deleted_at` with a
-- unique-while-live key index and a `version bigint`, matching `agent_profiles`. Child rows
-- (`eval_cases`, `agent_flow_steps`) and append-only run records (`eval_runs`,
-- `agent_flow_runs`, `agent_flow_step_runs`) do not — they are cascade-deleted with, or
-- reference, their parent and have no PATCH surface.
--
-- All statements are `create table if not exists` / `add column if not exists` and apply
-- cleanly against a fresh, empty database (verified: `sqlx migrate run` from empty).

-- =====================================================================================
-- Skills — declarative tool definitions, and skills-as-guards (plan 12 §3, §5).
--
-- `kind` is the tool-vs-guard axis from §5's "skills as guards": a `tool` skill is offered to
-- a model as a callable tool; a `guard` skill is a deterministic policy check evaluated before
-- a step and may only narrow access, never widen it (CONVENTIONS §7.5). `status` runs
-- draft -> enabled/disabled: imported or freshly authored skills land in `draft` and are
-- reviewed before an agent can call them (§5 decision 22, fail-closed posture).
-- =====================================================================================
create table if not exists skills (
    id uuid primary key default gen_random_uuid(),
    skill_key varchar(128) not null,
    display_name varchar(200) not null,
    description text,
    kind varchar(32) not null
        check (kind in ('tool', 'guard')),
    params_schema jsonb not null default '{}'::jsonb,
    tags text[] not null default '{}',
    status varchar(32) not null default 'draft'
        check (status in ('draft', 'enabled', 'disabled')),
    metadata jsonb not null default '{}'::jsonb,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    deleted_at timestamptz,
    version bigint not null default 1,
    constraint skills_skill_key_valid check (
        length(skill_key) between 1 and 128
        and skill_key ~ '^[a-z0-9]([a-z0-9_-]*[a-z0-9])?$'
    )
);

create unique index if not exists skills_skill_key_active_unique
    on skills (skill_key)
    where deleted_at is null;

create index if not exists skills_cursor_idx
    on skills (created_at desc, id desc)
    where deleted_at is null;

create index if not exists skills_status_cursor_idx
    on skills (status, created_at desc, id desc)
    where deleted_at is null;

create index if not exists skills_kind_cursor_idx
    on skills (kind, created_at desc, id desc)
    where deleted_at is null;

-- One-to-one HTTP execution template for an `invocation_kind = 'http'` skill (plan 12 §5).
--
-- `credential_id` references `provider_credentials` deliberately — it reuses that table's
-- `LocalSecretCipher`/`credential_aad()` envelope, masking, and rotation rather than inventing
-- a second secret store (decision 21). The row stores only a reference; the secret is decrypted
-- into a short-lived local at call time by workstream H's executor, never persisted resolved.
-- `allowed_host` is stored redundantly so execution-time SSRF validation checks the resolved
-- final URL's host without re-parsing a `{placeholder}` template (R20). This table is created
-- here (F owns the schema) but its CRUD and the executor runtime belong to workstream H (§5).
create table if not exists skill_http_executors (
    skill_id uuid primary key references skills(id) on delete cascade,
    method varchar(16) not null
        check (method in ('GET', 'POST', 'PUT', 'PATCH', 'DELETE')),
    url_template text not null,
    allowed_host text not null,
    header_template jsonb not null default '{}'::jsonb,
    credential_id uuid references provider_credentials(id) on delete set null,
    timeout_ms integer not null default 10000 check (timeout_ms > 0),
    response_schema jsonb,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

-- =====================================================================================
-- Evaluations — offline test suites and their runs (plan 12 §3).
-- =====================================================================================
create table if not exists eval_suites (
    id uuid primary key default gen_random_uuid(),
    suite_key varchar(128) not null,
    display_name varchar(200) not null,
    description text,
    status varchar(32) not null default 'active'
        check (status in ('active', 'disabled', 'deleted')),
    metadata jsonb not null default '{}'::jsonb,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    deleted_at timestamptz,
    version bigint not null default 1,
    constraint eval_suites_suite_key_valid check (
        length(suite_key) between 1 and 128
        and suite_key ~ '^[a-z0-9]([a-z0-9_-]*[a-z0-9])?$'
    )
);

create unique index if not exists eval_suites_suite_key_active_unique
    on eval_suites (suite_key)
    where deleted_at is null;

create index if not exists eval_suites_cursor_idx
    on eval_suites (created_at desc, id desc)
    where deleted_at is null;

create index if not exists eval_suites_status_cursor_idx
    on eval_suites (status, created_at desc, id desc)
    where deleted_at is null;

create table if not exists eval_cases (
    id uuid primary key default gen_random_uuid(),
    suite_id uuid not null references eval_suites(id) on delete cascade,
    input jsonb not null,
    expected jsonb not null,
    grading_kind varchar(32) not null
        check (grading_kind in ('exact_match', 'contains', 'schema_valid')),
    metadata jsonb not null default '{}'::jsonb,
    created_at timestamptz not null default now()
);

create index if not exists eval_cases_suite_cursor_idx
    on eval_cases (suite_id, created_at desc, id desc);

-- Offline suite runs and (later) online sampled scoring share this append-only table.
-- `execution_id` is set for online grading of a live execution; `suite_id` is null for pure
-- online sampling not tied to a suite. No `version`/`deleted_at`: a run is a fact, not config.
create table if not exists eval_runs (
    id uuid primary key default gen_random_uuid(),
    suite_id uuid references eval_suites(id) on delete set null,
    agent_profile_id uuid references agent_profiles(id) on delete set null,
    trigger_kind varchar(32) not null
        check (trigger_kind in ('offline_manual', 'offline_ci', 'online_sampled')),
    execution_id uuid,
    status varchar(32) not null default 'pending'
        check (status in ('pending', 'running', 'completed', 'failed')),
    score double precision,
    results jsonb not null default '{}'::jsonb,
    metadata jsonb not null default '{}'::jsonb,
    created_at timestamptz not null default now(),
    completed_at timestamptz
);

create index if not exists eval_runs_suite_cursor_idx
    on eval_runs (suite_id, created_at desc, id desc);

create index if not exists eval_runs_cursor_idx
    on eval_runs (created_at desc, id desc);

-- =====================================================================================
-- Multi-agent flows — a DAG of steps; MVP is a linear sequential chain (plan 12 §3).
-- =====================================================================================
create table if not exists agent_flows (
    id uuid primary key default gen_random_uuid(),
    flow_key varchar(128) not null,
    display_name varchar(200) not null,
    description text,
    status varchar(32) not null default 'active'
        check (status in ('active', 'disabled', 'deleted')),
    metadata jsonb not null default '{}'::jsonb,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    deleted_at timestamptz,
    version bigint not null default 1,
    constraint agent_flows_flow_key_valid check (
        length(flow_key) between 1 and 128
        and flow_key ~ '^[a-z0-9]([a-z0-9_-]*[a-z0-9])?$'
    )
);

create unique index if not exists agent_flows_flow_key_active_unique
    on agent_flows (flow_key)
    where deleted_at is null;

create index if not exists agent_flows_cursor_idx
    on agent_flows (created_at desc, id desc)
    where deleted_at is null;

create index if not exists agent_flows_status_cursor_idx
    on agent_flows (status, created_at desc, id desc)
    where deleted_at is null;

-- A step names an `agent_profiles` row (`agent_profile_id`, the "agent_ref") and carries an
-- on-failure policy. `on_failure` defaults to 'abort', matching decision 15's fail-closed
-- posture; 'continue' is reserved for the Growth stage. `step_order` gives the linear MVP
-- ordering; `input_mapping` describes how the prior step's output feeds this step.
create table if not exists agent_flow_steps (
    id uuid primary key default gen_random_uuid(),
    flow_id uuid not null references agent_flows(id) on delete cascade,
    step_key varchar(128) not null,
    step_order integer not null check (step_order >= 0),
    agent_profile_id uuid not null references agent_profiles(id),
    on_failure varchar(32) not null default 'abort'
        check (on_failure in ('abort', 'continue')),
    input_mapping jsonb not null default '{}'::jsonb,
    metadata jsonb not null default '{}'::jsonb,
    created_at timestamptz not null default now(),
    constraint agent_flow_steps_step_key_valid check (
        length(step_key) between 1 and 128
        and step_key ~ '^[a-z0-9]([a-z0-9_-]*[a-z0-9])?$'
    )
);

create unique index if not exists agent_flow_steps_flow_step_key_unique
    on agent_flow_steps (flow_id, step_key);

create unique index if not exists agent_flow_steps_flow_step_order_unique
    on agent_flow_steps (flow_id, step_order);

create index if not exists agent_flow_steps_flow_cursor_idx
    on agent_flow_steps (flow_id, step_order, id);

-- One row per flow run, one per step run (append-only), so each step is separately auditable
-- and correlates to the underlying pipeline execution via `execution_id`.
create table if not exists agent_flow_runs (
    id uuid primary key default gen_random_uuid(),
    flow_id uuid not null references agent_flows(id) on delete cascade,
    status varchar(32) not null default 'running'
        check (status in ('running', 'completed', 'failed', 'cancelled')),
    metadata jsonb not null default '{}'::jsonb,
    created_at timestamptz not null default now(),
    completed_at timestamptz
);

create index if not exists agent_flow_runs_flow_cursor_idx
    on agent_flow_runs (flow_id, created_at desc, id desc);

create index if not exists agent_flow_runs_cursor_idx
    on agent_flow_runs (created_at desc, id desc);

create table if not exists agent_flow_step_runs (
    id uuid primary key default gen_random_uuid(),
    flow_run_id uuid not null references agent_flow_runs(id) on delete cascade,
    step_id uuid not null references agent_flow_steps(id),
    execution_id uuid,
    status varchar(32) not null default 'pending'
        check (status in ('pending', 'running', 'completed', 'failed', 'skipped')),
    error_summary text,
    created_at timestamptz not null default now(),
    completed_at timestamptz
);

create index if not exists agent_flow_step_runs_run_cursor_idx
    on agent_flow_step_runs (flow_run_id, created_at, id);

-- =====================================================================================
-- agent_profiles — extended in place (decision 12: extend, do NOT create a parallel table).
--
-- The three placeholder JSONB columns from 0005 (`tool_policy`/`context_policy`/`memory_policy`)
-- are unchanged here; these additive columns carry the new registry references. They stay
-- nullable/empty-default so every existing row and the fail-closed resolution model are
-- untouched until an execution engine reads them (sub-plans 2/3).
-- =====================================================================================
alter table agent_profiles
    add column if not exists skill_refs uuid[] not null default '{}',
    add column if not exists eval_suite_refs uuid[] not null default '{}',
    add column if not exists memory_scope_refs jsonb not null default '{}'::jsonb;

-- =====================================================================================
-- Version-bump triggers (ETag maintenance for If-Match). NOT the NOTIFY trigger — see header.
-- =====================================================================================
drop trigger if exists skills_bump_version on skills;
create trigger skills_bump_version
before update on skills
for each row execute function moira_bump_resource_version();

drop trigger if exists eval_suites_bump_version on eval_suites;
create trigger eval_suites_bump_version
before update on eval_suites
for each row execute function moira_bump_resource_version();

drop trigger if exists agent_flows_bump_version on agent_flows;
create trigger agent_flows_bump_version
before update on agent_flows
for each row execute function moira_bump_resource_version();
