-- Issue #211 (workstream D, plan 12 §2) + issue #83 — provider observability: measured
-- latency stats for the deferred `routing_policies.scoring_enabled` scoring phase, and a
-- rolling provider-health window for `GET /api/v1/admin/providers/health`.
--
-- # provider_model_latency_stats
--
-- Exactly plan 12 §2's schema sketch. Populated by the new `latency-stats-aggregation`
-- worker job (`src/infra/workers/latency_stats.rs`) from real `execution_attempts.latency_ms`
-- durations — consolidated decision 7 (naive last-N *measured* stat, not declared-only).
--
-- Nothing on the request path reads this table yet: `DefaultModelRouter` does not consume it
-- until the scoring phase gated by `routing_policies.scoring_enabled` (migration 0029) grows
-- a real candidate-ranking reader, which is explicitly out of scope for the change that adds
-- this migration (score-consumption in `src/application/execution.rs` is a follow-up, noted
-- in that PR's description). This migration ships the write side and a read path ahead of
-- that consumer — the same "declare/store ahead of the reader" shape 0029's `scoring_enabled`
-- column already used, not a repeat of the R7 dead-column trap: unlike `cost_weight` /
-- `latency_weight` / `quality_weight` (unused since migration 0005), this table has a real
-- writer from the moment this migration ships.
create table if not exists provider_model_latency_stats (
    provider_id uuid not null references providers(id) on delete cascade,
    provider_model_id uuid not null references provider_models(id) on delete cascade,
    p50_latency_ms integer,
    p95_latency_ms integer,
    sample_count bigint not null default 0 check (sample_count >= 0),
    updated_at timestamptz not null default now(),
    primary key (provider_id, provider_model_id)
);

-- # Rolling provider-health window (issue #83)
--
-- `provider_health_snapshots` already exists (migration `0005_provider_runtime.sql`) and has
-- carried the right shape since then — `status`, `circuit_state`, `latency_ms`,
-- `failure_count`, `observed_at` — but has had zero readers or writers in this tree: the R7
-- dead-column pattern, at table scope rather than column scope. This migration does not
-- redefine it; the new `provider-health-check` worker job
-- (`src/infra/workers/provider_health_check.rs`) writes one row per provider per probe with
-- `provider_model_id = null` (a provider-level reachability probe, not a per-model one — #83
-- asks for provider health, not per-model health), and
-- `GET /api/v1/admin/providers/health` reads a rolling aggregate of those rows.
--
-- The existing `provider_health_latest_idx (provider_id, provider_model_id, observed_at
-- desc)` does not serve that read well: every row this job writes has `provider_model_id =
-- null`, so a query for "recent rows for this provider" would still have to consult every
-- row under the provider rather than use the index's second column to narrow anything. A
-- partial index scoped to exactly the rows this job writes is what the rolling-window read
-- and the per-provider prune sweep both actually need.
create index if not exists provider_health_snapshots_provider_recent_idx
    on provider_health_snapshots (provider_id, observed_at desc)
    where provider_model_id is null;

-- # Cost — deliberately deferred, not built here
--
-- Issue #83 also asks for cost normalization (`estimated_total_cost` populated from a
-- per-provider-model pricing table, currently hardcoded to `"cost_estimation": "unavailable"`
-- at `src/application/execution.rs:593`). That column write lives in the same file this
-- change's owning PR was told not to touch (another workstream owns it tonight), and landing
-- a declared-cost schema with no consumer wired in the same change would reproduce the exact
-- R7 dead-column trap plan 12 §2 already flags for `cost_weight`/`latency_weight`/
-- `quality_weight`. Left as a follow-up; not started here.
