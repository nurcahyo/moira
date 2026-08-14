# Agent platform: skills, evaluations, flows

Issue #214 (plan 12 §3/§5), extended by issue #237 (plan 12 §5, workstream H: the OpenAPI import
pipeline and `skill_http_executors` CRUD). This is **still schema + CRUD only** — there is no
execution engine here — the multi-agent flow orchestrator and the Rig tool-call loop that would
make a skill callable are deferred and depend on rig tools (#84).

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
- **The OpenAPI import pipeline** (`POST /api/v1/admin/skills/import`) and **`skill_http_executors`
  CRUD** — issue #237, workstream H. See [OpenAPI import and HTTP executors](#openapi-import-and-http-executors)
  below.

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

## OpenAPI import and HTTP executors

Issue #237 (plan 12 §5, workstream H). Imports an OpenAPI 3.x document into draft skills plus one
`skill_http_executors` row per operation. **MVP scope: import + executor CRUD only.** There is no
live execution — the `HttpSkillTool` that would actually call an executor is deferred to the rig
tool loop (#84).

### Import pipeline

`POST /api/v1/admin/skills/import` (`moira:skills:write`, `Idempotency-Key` replay, 201):

1. Parses `request.document` with `orchestration::openapi_import::parse_openapi_document` — pure,
   no I/O — deriving one operation per `(path, method)` for the five methods
   `skill_http_executors.method` accepts (`GET`/`POST`/`PUT`/`PATCH`/`DELETE`).
2. **Rejects outright** (never silently truncates) a document defining more than **300**
   operations (§5 decision 23), returning `import_cap_exceeded` with the true count in the error
   envelope's `details`.
3. SSRF-validates the document's `servers[0].url` through
   `security::ssrf::validate_outbound_url` — the same guard that hardens JWKS fetches — before any
   database write. A blocked host (private/loopback/link-local/metadata range, or a non-`https`
   scheme) returns `ssrf_blocked_host` without revealing the resolved address or the specific
   denial reason.
4. On success, creates one `draft`/`tool` `skills` row plus one `skill_http_executors` row per
   operation, in a single transaction — all-or-nothing.

Every imported skill lands `draft`, exactly like a hand-authored one (§5 decision 22) — the
operator reviews and enables via the existing `/enable`/`/bulk-enable` endpoints above.

`params_schema` is derived from each operation's `parameters` (flattened to top-level properties)
and `requestBody`'s `application/json` schema (nested under a `body` property). Component `$ref`s
(`#/components/parameters/...`, `#/components/schemas/...`) are resolved one level against the
same document; a `$ref` nested inside an already-resolved schema is left as written. See the
module docs in `src/orchestration/openapi_import.rs` for the exact rules.

### `skill_http_executors` CRUD

`skill_http_executors` has **no `version`/`deleted_at` column** — F's migration gives optimistic
concurrency only to the three named registries. Its `If-Match` basis is instead `updated_at`,
carried as a quoted RFC 3339 timestamp (microsecond precision) rather than the integer `ETag`
every other admin resource uses.

| Method | Path | Scope | Notes |
|---|---|---|---|
| `GET` | `/api/v1/admin/skill-executors` | `moira:skills:read` | keyset pagination, ordered `(created_at, skill_id)` |
| `GET` | `/api/v1/admin/skills/{id}/executor` | `moira:skills:read` | 404 `executor_not_found` when the skill has none |
| `PATCH` | `/api/v1/admin/skills/{id}/executor` | `moira:skills:write` | requires `If-Match` (quoted RFC 3339 `updated_at`) |
| `DELETE` | `/api/v1/admin/skills/{id}/executor` | `moira:skills:delete` | requires `If-Match` |

`allowed_host` is **never client-settable**: PATCHing `url_template` re-runs the SSRF validation
against the new URL and re-derives `allowed_host` from it server-side, so the two columns can
never drift apart. `credential_id`, when set, must reference a live `provider_credentials` row —
checked at PATCH time — and carries no inline secret (decision 21). There is no `POST` to
hand-author an executor in this MVP; every row today comes from the import pipeline.

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
- `src/infra/repositories/agent_platform.rs` — Postgres row/SQL surface for `skills` and
  `skill_http_executors`.
- `src/application/agent_platform.rs` — `AgentPlatformService` (scope check, idempotency, audit,
  pagination, `If-Match`).
- `src/http/agent_platform.rs` — admin handlers, registered additively in `src/http/mod.rs`.
- `src/orchestration/openapi_import.rs` — pure OpenAPI 3.x parsing (issue #237): no I/O, so the
  300-operation cap, `$ref` resolution, and skill-key derivation are unit-testable without a
  network or a database.
- `tests/skill_import.rs` — end-to-end coverage over real Postgres: import, the operation cap,
  the SSRF block, and the executor CRUD lifecycle.
