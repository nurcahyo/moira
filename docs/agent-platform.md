# Agent platform: skills, evaluations, flows

Issue #214 (plan 12 §3/§5). This is **sub-plan 1 of 3: schema + CRUD only**. There is no
execution engine here — the multi-agent flow orchestrator and the Rig tool-call loop that would
make a skill callable are deferred to sub-plans 2/3 and depend on rig tools (#84).

## What landed

- **Migration `0031_agent_platform.sql`** — every table the workstream needs, so the relationship
  graph (workstream G) can read them immediately:
  - `skills` — declarative tool/guard definitions.
  - `skill_http_executors` — one-to-one HTTP execution template per skill (table only; its CRUD
    and the executor runtime belong to workstream H, §5).
  - `eval_suites`, `eval_cases`, `eval_runs` — offline evaluation suites and their runs.
  - `agent_flows`, `agent_flow_steps`, `agent_flow_runs`, `agent_flow_step_runs` — a DAG of steps
    (MVP is a linear sequential chain) and its run records.
  - Additive columns on `agent_profiles` (`skill_refs`, `eval_suite_refs`, `memory_scope_refs`) —
    extended **in place** per decision 12, never a parallel `agents` table.
- **Skills admin CRUD** over `/api/v1/admin/skills`.

Evaluations and flows CRUD are a **documented follow-up** — their tables and domain types already
exist; only the admin services/handlers remain.

## Skills

A skill is a stored, declarative tool definition — never caller-supplied code. Two axes:

- `kind` — `tool` (offered to a model as a callable tool) or `guard` (a deterministic policy
  check evaluated before a step; it may only **narrow** access, never widen it — CONVENTIONS
  §7.5).
- `status` — `draft` → `enabled`/`disabled`. Freshly authored or imported skills land in `draft`
  and are reviewed before an agent can call them (fail-closed, §5 decision 22).

`params_schema` is the JSON Schema for the tool's arguments. `tags` support later declarative skill
routing (§5). Optimistic concurrency uses the `version` ETag (`If-Match`), exactly like
`agent_profiles`.

### Endpoints

| Method | Path | Scope | Notes |
|---|---|---|---|
| `POST` | `/api/v1/admin/skills` | `moira:skills:write` | `Idempotency-Key` replay; 201 + `ETag` |
| `GET` | `/api/v1/admin/skills` | `moira:skills:read` | keyset pagination |
| `GET` | `/api/v1/admin/skills/{id}` | `moira:skills:read` | 404 when soft-deleted |
| `PATCH` | `/api/v1/admin/skills/{id}` | `moira:skills:write` | requires `If-Match` |
| `DELETE` | `/api/v1/admin/skills/{id}` | `moira:skills:delete` | soft delete; requires `If-Match` |
| `POST` | `/api/v1/admin/skills/{id}/enable` | `moira:skills:write` | requires `If-Match` |
| `POST` | `/api/v1/admin/skills/{id}/disable` | `moira:skills:write` | requires `If-Match` |
| `POST` | `/api/v1/admin/skills/bulk-enable` | `moira:skills:write` | enables up to 500 ids in one call |

`kind` is immutable after creation and `status` moves only through enable/disable — neither is
patchable, mirroring how `agent_profiles` keeps `profile_key`/`status` out of PATCH.

## These tables are not runtime configuration

Unlike `agent_profiles`/`route_definitions`/`routing_policies`, none of the agent-platform tables
fires `notify_moira_runtime_config_change()`. Nothing in `ProviderRuntimeCache`, the runtime-config
cache, or the circuit-breaker classifier (`src/infra/db.rs`) keys on a skill, eval suite, or flow,
so publishing on `moira_runtime_config` would only drop provider clients on a write that cannot
change provider configuration — the exact F51/F52 defect migrations 0022/0023/0024 removed. When an
execution engine later needs cache invalidation on these rows, add the NOTIFY trigger **and** the
`TRIGGERED_RESOURCE_TYPES` classification together (`tests/runtime_notify_inventory.rs` enforces the
set-equality). The `version`-bump trigger, which is independent of NOTIFY, is attached to the three
versioned registries (`skills`, `eval_suites`, `agent_flows`).

## Module layout

- `migrations/0031_agent_platform.sql`
- `src/domain/agent_platform.rs` — serde/`utoipa` types.
- `src/infra/repositories/agent_platform.rs` — Postgres row/SQL surface for `skills`.
- `src/application/agent_platform.rs` — `AgentPlatformService` (scope check, idempotency, audit,
  pagination, `If-Match`).
- `src/http/agent_platform.rs` — admin handlers, registered additively in `src/http/mod.rs`.
