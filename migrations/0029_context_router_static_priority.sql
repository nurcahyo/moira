-- Issue #213 — Workstream D: context router, MVP-static slice (plans/12-feature-expansion-
-- brainstorm.md §2). Static priority chains + attempt-level routing observability. Scoring
-- (complexity-weighted candidate re-ranking, `provider_model_latency_stats`) is explicitly
-- deferred — see the PR description and #90.
--
-- application_routing_defaults: the per-application priority default the context router falls
-- back to when a caller does not supply `ExecutionOptions.priority` (§2 "Priority" — request
-- priority is not the same axis as `routing_policies.priority`, which ranks policy rows against
-- each other for the same request). `complexity_weight_profile` is carried now, ahead of any
-- reader, as the seam the deferred scoring consumer (Later phase, gated by
-- `routing_policies.scoring_enabled` below) will read — nothing in this migration or the Rust it
-- ships with consumes either column's *value* yet; both are admin-readable/writable today via
-- `GET`/`PUT /api/v1/admin/applications/{id}/routing-defaults` so this is a genuinely stored,
-- round-tripping resource rather than a silently-inert one (contrast with the `cost_weight` /
-- `latency_weight` / `quality_weight` columns on `routing_policies`, unused since migration 0005
-- — R7 in plans/12 §2 — which were writable through the routing-policy admin API but never read
-- anywhere, including by that same API's own responses being the only place they were visible).
--
-- Shaped like the existing per-parent-id singleton policy tables (`provider_runtime_policies`,
-- `application_execution_policies`): primary key is the owning id (no surrogate `id` column —
-- nothing needs to reference a row here by anything other than `application_id`), `version` for
-- optimistic concurrency on `PUT` (mirrors `provider_runtime_policies`'s *optional* If-Match
-- posture, not `application_execution_policies`'s required one — see the PUT handler).
--
-- Deliberately does NOT carry the `notify_moira_runtime_config_change` trigger every sibling
-- policy table in `src/infra/db.rs::CIRCUIT_UNAFFECTED_RESOURCE_TYPES` has. That trigger's
-- function reads `NEW.id`/`OLD.id` (migration 0004), which is exactly the surrogate `id` column
-- this table intentionally omits, and — more to the point — nothing in this slice caches this
-- table's data (`RuntimeConfigCache` holds provider config and auth settings only; the execution
-- path does not read `application_routing_defaults` at all yet, since MVP priority is a static
-- selector, not a scoring input). Attaching a cross-replica invalidation trigger for a cache that
-- does not exist would be dead machinery, not defence in depth. Revisit when the deferred scoring
-- consumer starts reading this table on the request path.
create table if not exists application_routing_defaults (
    application_id uuid primary key references applications(id) on delete cascade,
    default_priority integer not null default 100 check (default_priority >= 0),
    complexity_weight_profile jsonb not null default '{}'::jsonb,
    updated_at timestamptz not null default now(),
    version bigint not null default 1
);

drop trigger if exists application_routing_defaults_bump_version on application_routing_defaults;
create trigger application_routing_defaults_bump_version
before update on application_routing_defaults
for each row execute function moira_bump_resource_version();

-- Opt-in switch for the deferred weighted-scoring consumer (decision 6, plans/12 §2). Defaults
-- false so every existing deployment's candidate ordering is byte-for-byte unchanged until an
-- operator opts in; not yet exposed through the routing-policy admin DTOs (`RoutingPolicyRecord`
-- / `*CreateRequest` / `*PatchRequest`) and not yet read by `list_model_candidates` or
-- `DefaultModelRouter` — both are Later-phase work. Landed now, ahead of any reader or writer, so
-- the column exists for that phase to alter into rather than add from scratch; it cannot be
-- mistaken for wired today because nothing outside this migration can set it to anything but its
-- default.
alter table routing_policies
    add column if not exists scoring_enabled boolean not null default false;
