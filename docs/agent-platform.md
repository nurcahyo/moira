# Agent platform: skills, evaluations, flows

Issue #214 (plan 12 §3/§5), extended by issue #237 (the OpenAPI import pipeline and
`skill_http_executors` CRUD), by F2 (the evals/flows CRUD section below), and by issue #84 (the
Rig tool loop that makes an enabled skill callable — see
[Executing skills](#executing-skills-the-rig-tool-loop)). The **multi-agent flow orchestrator is
still deferred**: `agent_flows` and its run tables carry CRUD but nothing executes a flow.

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

- **Eval suites/cases CRUD** (`/api/v1/admin/eval-suites`) and **flows CRUD**
  (`/api/v1/admin/flows`) — F2, below.

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
3. Enforces a byte budget alongside that count, returning `invalid_openapi_spec`: the input
   document may not exceed **512 KiB**, any one derived `params_schema` may not exceed **64
   KiB**, and the derived schemas may not exceed **2 MiB** in aggregate across the whole import.
   The aggregate cap is the one that matters — `$ref` resolution deep-clones the referenced
   schema once per operation, so 300 operations sharing one modest `$ref` amplify hundreds of
   times over while every per-operation figure stays unremarkable. Charges are measured on the
   referenced value and refused before the clone.
4. SSRF-validates the document's `servers[0].url` through
   `security::ssrf::validate_outbound_url` — the same guard that hardens JWKS fetches — before any
   database write. A blocked host (private/loopback/link-local/metadata range, or a non-`https`
   scheme) returns `ssrf_blocked_host` without revealing the resolved address or the specific
   denial reason.
5. On success, creates one `draft`/`tool` `skills` row plus one `skill_http_executors` row per
   operation, in a single transaction — all-or-nothing.

Every imported skill lands `draft`, exactly like a hand-authored one (§5 decision 22) — the
operator reviews and enables via the existing `/enable`/`/bulk-enable` endpoints above.

`params_schema` is derived from each operation's `parameters` — both the operation's own and the
path item's, which OpenAPI says every operation under that path inherits, with the operation's
winning on a `(name, in)` tie — flattened to top-level properties, plus `requestBody`'s
`application/json` schema nested under a `body` property. Component `$ref`s
(`#/components/parameters/...`, `#/components/schemas/...`) are resolved one level against the
same document; a `$ref` nested inside an already-resolved schema is left as written. See the
module docs in `src/orchestration/openapi_import.rs` for the exact rules.

Nothing stops a spec from declaring a **parameter** named `body`. When one does, the request body
is nested under the first free name from `request_body`, `request_body_2`, … and the schema
carries `"x-moira-body-property": "<that name>"` at its root. The executor never assumes the name:
it asks `openapi_import::body_property_name`, the one function that decides, so a schema whose
body moved cannot be dispatched as though it had not. Schemas with no such property are
unchanged — the annotation is written only when the default name was taken.

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

#### A credential may only be bound to its own provider's host

**This rule is a behaviour change. Read it before upgrading if any skill executor carries a
`credential_id`.**

Issue #253 finding 1: the bound credential is decrypted at call time and sent as
`Authorization: Bearer <plaintext>`, and an existence-only check on `credential_id` made
`moira:skills:write` equivalent to reading the plaintext of *every* row in
`provider_credentials` — bind one to `https://collector.attacker.example`, which passes the
SSRF guard like any other public host, and read it off the wire. No other admin scope grants
that; the credentials surface only ever returns masked values.

The rule is entitlement by destination: the credential's owning provider must declare the
executor's `allowed_host` as its `providers.base_url` host. A provider with **no** `base_url`
(one left on its vendor default) entitles nothing — there is no host to compare against, and
inventing the vendor default would tie the rule to a hostname table kept in step with
`rig-core`. It is enforced at PATCH time and again at execution time, before decryption.

**The failure mode this introduces.** An executor row that was legal before the rule and is not
legal under it keeps existing, and nothing rejects it at deploy time — but at execution the
credential is refused, and because `skill_refs` resolution is fail-closed the **whole
execution** fails with `SkillUnavailable`, not just the one tool call. The refusal is logged
server-side with the `credential_id` and the `allowed_host`; the caller sees only the class.
There is no migration, because no automatic repair is safe: silently unbinding the credential
would make the skill call unauthenticated, and silently widening the provider's `base_url`
would grant the entitlement the rule exists to withhold.

**A soft-deleted provider is the second way a working row stops working, and no host
comparison can see it.** The execution-time lookup requires the credential's owning
`providers` row to be live, because a provider that no longer exists declares no host and so
entitles no destination. That refuses a credential whose own row is still `active`, unexpired
and undeleted — nothing about the credential changed — and it is invisible to the host rule,
because such a provider's `base_url` may name the executor's `allowed_host` exactly. The
inventory query below therefore checks `providers.deleted_at` **first** and reports these
separately as `reason = 'provider_deleted'`; repairing the `base_url` of a deleted provider
fixes nothing. At execution the message names the real cause ("belongs to a provider that has
been deleted; the credential itself is still live") rather than blaming the credential. Only
repairs 2 and 3 below apply — never repair 1.

**Find the affected rows before you deploy:**

```sql
with binding as (
    select e.skill_id,
           e.allowed_host,
           c.id         as credential_id,
           p.id         as provider_id,
           p.base_url,
           p.deleted_at as provider_deleted_at,
           -- The authority: base_url with the scheme, any userinfo, and everything from the
           -- first '/', '?' or '#' removed. btrim matches the code's own base_url.trim().
           regexp_replace(
             regexp_replace(
               regexp_replace(btrim(p.base_url), '^[A-Za-z][A-Za-z0-9+.-]*://', ''),
               '^[^/?#]*@', ''),
             '[/?#].*$', '') as authority,
           -- Whether the value is shaped like something Url::parse can turn into a host at
           -- all. Without a 'scheme://' there is no host to extract, whatever the characters
           -- spell: 'api.vendor.example/v1' is not a URL, and 'api.vendor.example:8443/v1'
           -- parses as a *scheme* named api.vendor.example carrying an opaque path. The code
           -- refuses both, while the string extraction above happily returns
           -- 'api.vendor.example' for both -- so the shape is tested, not assumed.
           btrim(p.base_url) ~ '^[A-Za-z][A-Za-z0-9+.-]*://' as has_authority
    from skill_http_executors e
    join provider_credentials c on c.id = e.credential_id
    join providers p            on p.id = c.provider_id
    where e.credential_id is not null
), split as (
    select b.*,
           -- An IPv6 literal keeps its brackets, because Url::host_str() returns them and
           -- allowed_host is stored as that function produced it. Splitting on ':' here
           -- would return '[' and report every IPv6 provider as broken.
           case when b.authority like '[%'
                then lower(left(b.authority, position(']' in b.authority)))
                else lower(split_part(b.authority, ':', 1))
           end as base_url_host,
           -- Whatever the ':' split dropped, kept so it can be checked rather than ignored.
           case when b.authority like '[%'
                then substr(b.authority, position(']' in b.authority) + 1)
                else substr(b.authority, length(split_part(b.authority, ':', 1)) + 1)
           end as port_suffix
    from binding b
), resolved as (
    select s.*,
           -- What follows the host must be nothing, or a port Url::parse would accept.
           -- ':nope' and ':99999' both fail it outright, and a bare ':' split would throw
           -- them away and compare a host the code never produced.
           s.has_authority
             and s.port_suffix ~ '^(:[0-9]{0,5})?$'
             and case when s.port_suffix ~ '^:[0-9]{1,5}$'
                      then substr(s.port_suffix, 2)::int <= 65535
                      else true
                 end as base_url_parses
    from split s
)
select skill_id,
       allowed_host,
       credential_id,
       provider_id,
       base_url,
       case when provider_deleted_at is not null              then 'provider_deleted'
            when base_url is not null and not base_url_parses then 'base_url_unparseable'
            else 'host_mismatch' end as reason
from resolved
where provider_deleted_at is not null
   or base_url is null
   or not base_url_parses
   or base_url_host is distinct from lower(allowed_host)
order by reason, skill_id;
```

**The query over-reports, and you must not treat a hit as proof.** Postgres has no URL parser,
so the host above is extracted with string operations while `credential_binding_permits_host`
uses `Url::host_str()`. The extraction handles the forms that actually diverge in practice —
scheme, userinfo (`https://api.vendor.example@evil.example/`), port, path/query/fragment,
surrounding whitespace, bracketed IPv6 literals — but it does not normalise an IPv6 address
(`[0:0:0:0:0:0:0:1]` against `[::1]`), apply IDNA/punycode, or percent-decode. Those forms
compare unequal in SQL and equal in the code, so the query can name a row the code accepts.
"The query returned N rows" is not the set that is broken — confirm each one before repairing
it, especially before repair 1.

**An empty result means "nothing found", not "nothing broken".** Silence in the other
direction — a row the code refuses that the query never names — is the one that sends an
operator into a deploy, so be exact about what is established here and what is not. This
section used to claim the query "cannot miss a row the code refuses". It could: with
`allowed_host = 'api.vendor.example'`, a `base_url` of `api.vendor.example/v1` went unreported,
because a string extraction returns the host a value appears to spell while `Url::parse`
rejects a value with no scheme at all. `api.vendor.example:8443/v1` was missed the same way,
and that one does parse — as a *scheme* named `api.vendor.example` with no host. The
`has_authority` and port checks above exist to close exactly those, and `:nope` and `:99999`
with them. None of those shapes can be written through `PATCH /api/v1/admin/providers/{id}`,
which rejects them; they arrive by migration, restore or direct SQL, which is precisely the
population this query exists to survey.

What is established is bounded by a test rather than by argument:
`tests/skill_import.rs::the_documented_inventory_query_finds_every_row_the_binding_rule_refuses`
executes *this block*, unmodified, against a real database and compares its verdict on each
seeded row against what `resolve_skill_credential` returns for that same row. The shapes it
seeds are the shapes the query and the code are known to agree on. A shape outside that set has
been proved in neither direction — if you meet one, seed it there rather than reasoning about
it here.

**Repair each one, in whichever way is actually true of your deployment:**

1. **Give the provider a `base_url` naming the executor's host — only if that provider really
   is served there.** `providers.base_url` is not a label for this rule: it is that provider's
   live completion endpoint. `RigRuntimeFactory::build_completion_model` hands it to the
   openai / anthropic / gemini / deepseek client builder **together with the decrypted API
   key**, so setting it redirects every completion for that provider, and that provider's key,
   to the host you name. For the case this section singles out — a provider left on its vendor
   default with `base_url = NULL` — that is exactly the wrong repair: it does not grant the
   skill an exception, it moves the provider. Use repair 2 or 3 there.
   (`PATCH /api/v1/admin/providers/{id}`, an audited write.)
2. Repoint the executor at a credential whose provider does serve that host
   (`PATCH /api/v1/admin/skills/{id}/executor` with a new `credential_id`).
3. `DELETE /api/v1/admin/skills/{id}/executor` and re-import, if the executor was wrong.

**A non-conforming row stays editable.** PATCH refuses only patches that *move* the binding —
a different `credential_id`, or a `url_template` whose host differs from the stored
`allowed_host`. A patch that leaves both exactly as they are is allowed, so `timeout_ms`,
`method`, `header_template` and `response_schema` can still be edited, and the row can still be
deleted. (The first release of this rule re-validated on every patch, which made such a row
un-patchable for unrelated edits — a refusal that moved no secret, since execution already
refuses to send one, and only blocked cleanup.)

## Evaluations

Issue #214, F2 (plan 12 §3, decision 14). An eval suite is a named, versioned registry of
offline test cases; a case pairs an `input` fixture with an `expected` value and a `grading_kind`.
`eval_runs` are **not writable through this admin surface** — a run is produced by executing a
suite (deferred; see "What's deferred" below), so the only endpoint is a read-only list.

| Method | Path | Scope | Notes |
|---|---|---|---|
| `POST` | `/api/v1/admin/eval-suites` | `moira:evals:write` | `Idempotency-Key` replay; 201 + `ETag` |
| `GET` | `/api/v1/admin/eval-suites` | `moira:evals:read` | keyset pagination |
| `GET` | `/api/v1/admin/eval-suites/{id}` | `moira:evals:read` | 404 when soft-deleted |
| `PATCH` | `/api/v1/admin/eval-suites/{id}` | `moira:evals:write` | requires `If-Match` |
| `DELETE` | `/api/v1/admin/eval-suites/{id}` | `moira:evals:delete` | soft delete; requires `If-Match` |
| `POST` | `/api/v1/admin/eval-suites/{id}/cases` | `moira:evals:write` | 201; suite must be live |
| `GET` | `/api/v1/admin/eval-suites/{id}/cases` | `moira:evals:read` | keyset pagination |
| `DELETE` | `/api/v1/admin/eval-suites/{id}/cases/{case_id}` | `moira:evals:delete` | hard delete; no `If-Match` — `eval_cases` has no `version`/`updated_at` column |
| `GET` | `/api/v1/admin/eval-suites/{id}/runs` | `moira:evals:read` | read-only; keyset pagination |

`grading_kind` is restricted to `exact_match` / `contains` / `schema_valid` (decision 14) —
`llm_judge` grading is deliberately absent from the MVP given its cost.

## Multi-agent flows

Issue #214, F2 (plan 12 §3, decision 13). A flow is a named, versioned sequence of steps; the
MVP is sequential-only (linear chain, no branching). **Steps travel inside the flow's own
`create`/`patch` request body as an ordered array, not through a separate steps sub-resource** —
the simplest contract for the sequential-only MVP. `PATCH` with a `steps` field present replaces
the entire ordered list atomically; omitting `steps` leaves the existing list untouched, the same
coalesce convention every other field on this surface follows. Every response (`create`, `get`,
`list`, `patch`) echoes the flow's current step list back.

Each step names an `agent_profiles` row via `agent_profile_id`. That reference is validated
fail-closed: a step naming a missing or deleted agent profile is rejected with `400` before the
flow (or its patched step list) is ever written — the write never lands half-valid.
`agent_flow_runs` are **not writable through this admin surface**, for the same reason
`eval_runs` are not: they are produced by running a flow, and there is no flow-run engine yet.

| Method | Path | Scope | Notes |
|---|---|---|---|
| `POST` | `/api/v1/admin/flows` | `moira:flows:write` | `Idempotency-Key` replay; 201 + `ETag`; `steps` optional (defaults to `[]`) |
| `GET` | `/api/v1/admin/flows` | `moira:flows:read` | keyset pagination; each row includes its `steps` |
| `GET` | `/api/v1/admin/flows/{id}` | `moira:flows:read` | 404 when soft-deleted |
| `PATCH` | `/api/v1/admin/flows/{id}` | `moira:flows:write` | requires `If-Match`; `steps` present replaces the whole list |
| `DELETE` | `/api/v1/admin/flows/{id}` | `moira:flows:delete` | soft delete; requires `If-Match` |
| `GET` | `/api/v1/admin/flows/{id}/runs` | `moira:flows:read` | read-only; keyset pagination |

**What's deferred, explicitly**: there is **no execution endpoint** — no `POST
/api/v1/admin/flows/{id}/run`, no way to score an eval suite. A flow can be fully authored (all
its steps, in order, each naming a live agent profile) but cannot run; an eval suite can be fully
populated with cases but cannot be graded. The rig tool loop those two were waiting on has since
landed (issue #84, the section below), so the remaining work is the offline-eval runner and the
flow orchestrator that walks the step DAG through the existing execution pipeline — a follow-up
to #84's next stage, not to this one.

## Executing skills: the rig tool loop

Issue #84 (plan 12 §5), the partial slice that makes an **enabled** skill actually callable by a
model. The multi-agent flow engine is still deferred.

An agent profile's `skill_refs` is the whole selection mechanism — §5's MVP tier, "the router is
the agent author". Resolution runs once per execution, before any provider is chosen:

1. Each `skill_refs` id is classified (`domain::SkillResolution::classify`). An enabled
   `kind = 'tool'` row with its executor becomes a callable tool; an enabled `kind = 'guard'` row
   becomes a guard; **anything else refuses the execution** with `409 skill_unavailable` — missing,
   still `draft`, disabled, or a tool with no executor row. Fail-closed, the same decision issue
   #79 took for a dangling `agent_profile_id`: an agent silently missing a skill it was configured
   with answers wrongly and nobody is told.
2. Each tool's `params_schema` becomes the `ToolDefinition.parameters` on the wire, in `skill_refs`
   order. `agent_profiles.tool_policy` is still **not** read — it is an unspecified placeholder
   column from migration 0005, and pinned tests hold it that way.
3. The loop (`orchestration::skill_tool::run_tool_loop`) issues up to
   `skill_execution.maximum_tool_turns` model calls. Each turn's tool calls become one assistant
   message plus exactly one user message carrying every tool result, in call order — the shape
   providers require for parallel calls.

### What a call does

`HttpSkillTool` fills `{placeholder}` segments from the model's arguments (percent-encoded, so an
argument cannot escape its segment), sends every remaining declared argument as a query parameter,
sends the argument `openapi_import::body_property_name` names — `body` unless a parameter took
that name, see above — as the JSON request body on `POST`/`PUT`/`PATCH`, and re-runs
`security::ssrf::validate_outbound_url` on the **resolved** URL plus an `allowed_host` equality
check (plan 12 risk R20 — import-time validation cannot cover a URL that only exists at call time).
The executor's `credential_id` is decrypted per execution into an `Authorization: Bearer` header;
it is never a tool argument, never in the tool's output, and never in a `Debug` rendering.

Failures stay **in-band** by default: a refused address, a timeout or an upstream error becomes a
classified tool result the model can recover from. Only an exhausted turn budget terminates the
attempt (`deadline_exceeded`).

### Guards

A `kind = 'guard'` skill in `skill_refs` is a deterministic policy check evaluated **before** every
dispatch, from its `metadata.guard` object:

```json
{ "guard": { "allowed_skill_keys": ["orders_get"], "denied_skill_keys": [], "required_scopes": [] } }
```

Guards **narrow only** (CONVENTIONS §7.5): the first denial wins and no later guard can reverse it,
and no field can grant a scope the caller does not already hold. A guard whose policy cannot be
parsed denies everything it governs. A denial reaches the model as a keyed
`skill_guard_denied` result and is recorded as a `tool_result` runtime event carrying
`guard_key` and the reason (`skill_not_allowed`, `skill_denied`, `missing_scope`,
`policy_unreadable`).

### Deliberately deferred

- **Streamed tools.** The streamed path surfaces `ToolCallStarted`/`ToolCallDelta` but cannot feed
  a tool *result* back into a new stream, so `stream: true` plus skills is refused
  (`invalid_execution_request`) rather than advertising tools nothing can satisfy.
- **Structured output plus skills**, refused for finding F48: `rig-core` silently drops
  `response_format` whenever tools are advertised on turn 1, so the combination would return prose
  and blame the caller's schema.
- **Caller-declared tools** on the public API remain rejected (`unsupported_tool`); this slice
  enables *operator-configured* skills only.

### Settings (`[skill_execution]`)

| Key | Default | Meaning |
|---|---|---|
| `maximum_tool_turns` | `4` | Total model calls one tool-bearing execution may make |
| `maximum_advertised_tools` | `32` | Ceiling on tools one request may advertise |
| `maximum_response_bytes` | `65536` | Ceiling on the skill response fed back to the model |
| `dns_timeout_ms` | `5000` | Budget for the execution-time SSRF hostname resolution |
| `allow_insecure_dev_urls` | `false` | Dev-only; production start-up refuses to come up while true. Import never honours it |

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
- `src/infra/repositories/agent_platform.rs` — Postgres row/SQL surface for `skills`,
  `skill_http_executors`, `eval_suites`/`eval_cases`/`eval_runs`, and
  `agent_flows`/`agent_flow_steps`/`agent_flow_runs`.
- `src/application/agent_platform.rs` — `AgentPlatformService` (scope check, idempotency, audit,
  pagination, `If-Match`).
- `src/http/agent_platform.rs` — admin handlers, registered additively in `src/http/mod.rs`.
- `src/orchestration/openapi_import.rs` — pure OpenAPI 3.x parsing (issue #237): no I/O, so the
  300-operation cap, the byte budget, `$ref` resolution, and skill-key derivation are
  unit-testable without a network or a database.
- `tests/skill_import.rs` — end-to-end coverage over real Postgres: import, the operation cap,
  the SSRF block, and the executor CRUD lifecycle.
- `tests/agent_platform.rs` — end-to-end coverage over real Postgres for skills, eval
  suites/cases, and flows/steps: full CRUD lifecycles, stale `If-Match`, and idempotent create
  replay.
- `src/orchestration/skill_tool.rs` — `HttpSkillTool` (the `rig_core::tool::Tool` impl), the
  `ToolSet` assembly, and the multi-turn loop (issue #84). A widening of the Rig seam, listed in
  `tests/rig_boundary.rs`'s allow-list with its argument.
- `tests/skill_tool_loop.rs` — end-to-end coverage of the loop against two scripted servers (a
  model and a skill target): advertise, call, round-trip, guards, SSRF, the turn budget, and the
  two deliberate refusals.
