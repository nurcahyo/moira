# Iteration 10 — Multi-Replica Readiness

Post-MVP. Addresses **P3-1..P3-5**. Companion source: `00-audit-report.md`, `01-roadmap-and-dependencies.md`.

---

## Summary

**Objective.** Replace Moira's per-process, in-memory distributed-state (rate limiting, execution concurrency permits, circuit breakers, idempotency execution locking) with Redis-backed equivalents so that Moira can safely run more than one API replica, and replace the current Helm-template-only `replicaCount==1` guard with a real admission control that cannot be bypassed by `kubectl scale`. This iteration also adds leader election for singleton workers and durable worker queues, both currently absent.

**Why ordered here.** Every earlier iteration (02–09) is scoped to a **single-replica** MVP by explicit design decision (`01-roadmap-and-dependencies.md` §1.4). Building distributed controls is a large, self-contained, purely additive capability with no bearing on MVP honesty, security hardening, durability, observability, or identity. It is the single item separating "self-hosted single-box gateway" from "horizontally-scaled service," and nothing else in the roadmap depends on it. Conversely, RAG/memory intelligence (plan 11) benefits from — but does not require — this iteration; note the loose `I10 -.enables scaled.-> I11` edge in the dependency graph.

**User-visible outcome.** Operators can set `replicaCount > 1` (or enable Helm `autoscaling`) and get correct behavior: rate limits, concurrency ceilings, and circuit-breaker state are enforced cluster-wide rather than per-pod; a stalled/duplicated Idempotency-Key request from a retrying client is coordinated across replicas; exactly one replica runs each singleton maintenance job; queued background work survives a pod restart and is retried/dead-lettered instead of silently vanishing.

**Included scope.** Redis-backed: public rate limiter, execution concurrency permits (global/provider/provider-stream/application/user), circuit-breaker registry. A Postgres-backed (not Helm-template-only) replica-count admission gate. Redis pub/sub runtime-config invalidation *added alongside* the existing Postgres LISTEN/NOTIFY (not a replacement). Leader election for singleton workers. A durable worker queue (retry/backoff/dead-letter/metrics) replacing the current no-op supervisor tick loop. Idempotency **execution locking** moved to Redis while Postgres remains the durable ledger of record.

**Excluded scope.** Session affinity / sticky routing (not applicable — Moira has no human sessions until plan 08/09). Any change to the Postgres LISTEN/NOTIFY runtime-config invalidation mechanism itself (`src/infra/db.rs:43-80`) — it already works cross-instance and is out of scope; this iteration only *adds* a Redis channel as defense-in-depth / lower-latency signal. Implementing the actual bodies of the memory-extraction/summarization/embedding/document-ingestion worker jobs named in `WorkerRegistry::new` (`src/infra/workers.rs:47-91`) — those job *bodies* belong to plan 11 (memory/RAG) and plan 04 (retention cleanup); this iteration only builds the durable **queue/leader-election plumbing** those jobs will run on. Rig execution changes. Any admin/public API surface changes beyond the new admission-gate error response.

---

## Branch & Pull Request

Binding: `plans/CONVENTIONS.md` §1. Where anything below conflicts with CONVENTIONS.md, **CONVENTIONS.md wins**.

- **Branch:** `plan/10-multi-replica-readiness` — branched from the **current `main`**, never from another plan branch. This plan is **post-MVP**; it has no hard dependency on plans 02–09 landing first, but in practice it is sequenced after them (see `01-roadmap-and-dependencies.md`). If the coordinator elects to stack it on an unmerged base branch, the PR description **must name the base PR** and the branch must be rebased once that base merges (§1.1). Never force-push this branch if another plan stacks on it (§1.7).
- **Shape: ONE pull request.** Unlike plan 11 (which is internally phased and lands as a stacked series), plan 10 is a single cohesive capability whose Definition of Done — "in-memory default behavior is byte-for-byte unchanged, *and* multi-replica mode is correct" — cannot be evaluated from a partial slice. The internal Waves 0–3 (§ Multi-Agent Workflow) are **execution waves on one branch**, not separate PRs. Wave checkpoints run the §2 gates locally; only the final state is reviewed as a PR.
- **Commits:** Conventional Commits (`feat:`, `fix:`, `test:`, `docs:`, `refactor:`, `chore:`), matching existing history style (`feat: make admin commands atomic`). Suggested commit boundaries mirror the waves: `feat: add redis lock and lua counter primitives`, `feat: add redis-backed rate limiter, concurrency and circuit backends`, `feat: add postgres cluster admission lease`, `feat: add durable worker queue and leader election`, `test: add multi-replica e2e and redis chaos suites`, `docs: document multi-replica rollout`.
- **The PR must not be opened until every gate in CONVENTIONS.md §2 passes locally** (§1.3).
- **Required PR description sections** (§1.4), all mandatory:
  - **Plan link** — `plans/10-multi-replica-readiness.md`.
  - **Findings addressed** — `P3-1`, `P3-2`, `P3-3`, `P3-4`, `P3-5` (and the un-numbered `docs/todo.md:89` idempotency-locking item).
  - **Migrations included** — the new `migrations/0009_multi_replica_readiness.sql` (verified next-free number: the highest existing migration is `0008_response_updated_at.sql`, so **0009 is free** — re-confirm at implementation time anyway).
  - **Breaking API/OpenAPI changes** — **none.** This plan adds no routes and changes no DTOs; state this explicitly rather than omitting the section. Because it is OpenAPI-neutral it is unaffected by the §1.6 "land before plan 05 freezes the spec" ordering rule.
  - **Test evidence** — unit + e2e output summary (see § Verification for the exact named files and test functions that must appear).
  - **Rollback procedure** — see § Risks & Rollback (config-only revert: `cluster.multi_replica_enabled=false` + `replicaCount=1`).
  - **Deferred follow-ups** — see § Risks & Rollback's deferred list.
- **Done means merged** (§1.5): this plan is **not** done when the PR opens. It is done when the PR is **merged with all gates green** and every Definition-of-Done bullet is objectively verified by a named, passing test (§3 "Implemented is not done").

---

## Findings Addressed

- **P3-1** — In-memory rate limiting, concurrency permits, and circuit breakers are per-process. Evidence: `src/orchestration/controls.rs:150-611` — `InMemoryRateLimiter` (203-212, `buckets: Arc<Mutex<HashMap<String, RateLimitBucket>>>`), `ConcurrencyController` (150-348, `Arc<Semaphore>` + per-scope `DynamicLimiter` maps), `CircuitBreakerRegistry` (494-611, `states: Arc<Mutex<HashMap<(Uuid, Uuid), CircuitEntry>>>`). All are constructed once in `AppState::new` (`src/app/state.rs:71-79`) and held as `Clone`-able `Arc`-wrapped state (`src/app/state.rs:34-36`) — each pod has its own independent view. `docs/todo.md:86-87` (Phase 6).
- **P3-2** — No cluster admission / DB lease preventing `kubectl scale` past 1 replica. Evidence: `charts/moira/templates/_helpers.tpl:16-26` — `moira.validateDeployment` is a Helm **template-time** `fail` on `.Values.replicaCount != 1` or `.Values.autoscaling.enabled`; it only runs during `helm template`/`helm install`/`helm upgrade`. `kubectl scale deployment/moira --replicas=3` bypasses it entirely because it never re-renders the chart. `docs/todo.md:88`.
- **P3-3** — Redis is connected but functionally idle. Evidence: `src/infra/redis.rs` exposes `ping()` (45-59, used only by `src/http/health.rs:60-61` in `/health/ready`) and `publish_runtime_invalidation()` (61-72) which **has zero callers anywhere in `src/`** (verified via `grep -rn publish_runtime_invalidation src/` → only the definition). No limiter, no lock, no subscriber. `docs/todo.md:86,87,89,90`.
- **P3-4** — No leader election for singleton workers. Evidence: `src/infra/workers.rs:119-153` — `spawn_supervisor` starts exactly one `tokio::spawn` per process on a fixed interval that only calls `state.metrics.record_worker_tick()` (149); there is no coordination primitive, so with N replicas each would independently attempt any real job body once implemented (retention cleanup, provider health probing, OAuth refresh — the very `WorkerSpec` names already declared at `src/infra/workers.rs:50-91`). `docs/todo.md:91`.
- **P3-5** — No durable worker queues. Evidence: `src/infra/workers.rs:120-153` supervisor loop has no job table, no enqueue/dequeue API, no retry/backoff, no dead-letter handling, no per-job metrics beyond a single tick counter. `docs/todo.md:92,93`.
- Related but **explicitly out of scope for this iteration**: runtime-config cache invalidation is **already** cross-instance correct via Postgres LISTEN/NOTIFY (`src/infra/db.rs:43-80`, `spawn_runtime_config_listener` / `listen_once`), confirmed a **positive finding** in the audit (`00-audit-report.md` "Positive findings"). Do not duplicate or replace this; only add Redis pub/sub as an additional, lower-latency signal per `docs/todo.md:90`.
- Idempotency locking today: `src/infra/repositories/admin.rs:559-634` `claim_idempotency` uses `pg_try_advisory_xact_lock` inside a Postgres transaction with a 5s poll-retry loop (563-581) — this is **already multi-replica-safe** for admin commands (a positive finding, "Atomic admin idempotency is genuinely correct"). The public `/v1/responses` idempotency path (`src/application/public.rs:125-137`, `claim_idempotency`/`replay_idempotency` referenced at 129-137) uses the same Postgres-backed pattern (see `src/infra/repositories/public.rs:741` for `request_hash` usage referenced in P1-1). Because the Postgres advisory-lock mechanism is already correct and multi-replica-safe by construction, the idempotency-locking item at **`docs/todo.md:89`** ("Move HTTP idempotency execution locking to distributed Redis locks while keeping PostgreSQL as the durable replay ledger" — part of the Phase 6 bundle; it has no dedicated P3 number, P3-5 proper is durable queues) is a **performance/contention optimization**, not a correctness requirement — it removes the 5s poll-retry-under-contention cost by using Redis `SET NX PX` for the *first* line of defense, falling back to the existing Postgres advisory lock as the source of truth. Scope it accordingly (§ Detailed Implementation).

---

## Architecture

### Components & ownership (per `docs/project-structure.md`)

| Component | Module | Notes |
|---|---|---|
| `RedisRateLimiter` | `src/orchestration/controls.rs` (new struct, alongside existing `InMemoryRateLimiter`) | Orchestration owns runtime behavior per `docs/project-structure.md` §Boundaries. |
| `RedisConcurrencyController` | `src/orchestration/controls.rs` | Same file as `ConcurrencyController`; both implementations share the `ExecutionPermits` return type so callers in `src/application/execution.rs` are unaffected. |
| `RedisCircuitBreakerRegistry` | `src/orchestration/controls.rs` | Mirrors `CircuitBreakerRegistry`'s public API (`before_call`, `on_success`, `on_failure`, `reset_all`). |
| Redis client extensions (script eval, pub/sub subscribe, distributed lock) | `src/infra/redis.rs` | `infra` owns external persistence/connection concerns per project-structure boundaries; `RedisClient` already lives here. |
| Cluster admission lease | new migration + `src/infra/db.rs` (new function) + `src/app` startup path | DB-backed, so lease bookkeeping belongs in `infra`; the startup check that refuses to serve traffic belongs in `src/app` (process composition). |
| Leader election | `src/infra/workers.rs` (extend `WorkerRegistry`/`WorkerSupervisor`) | Worker orchestration already lives here. |
| Durable worker queue | new `src/infra/workers.rs` submodule (or `src/infra/worker_queue.rs`) + new migration for a `worker_jobs` table | `infra` owns persistence; `WorkerRegistry` in the same module already declares the job names this queue will run. |
| Config | `src/config/settings.rs` — extend `RedisSettings` (186-192) and `WorkerSettings` (194-202) | `config` owns static infra config per boundaries. |

### Data flow

1. **Rate limiting / concurrency**: `src/application/execution.rs` currently calls `state.concurrency.acquire(...)` and `state.public_rate_limiter.check(...)`. Both call sites move behind a feature-selected implementation (`enum ConcurrencyBackend { InMemory(ConcurrencyController), Redis(RedisConcurrencyController) }`) chosen once at `AppState::new` based on `settings.redis.enabled` — callers keep calling the same method names, only the backend changes. No call-site changes in `execution.rs` beyond the type held in `AppState`.
2. **Circuit breaker**: same pattern — `state.circuits.before_call/on_success/on_failure` keep their signatures; a Redis-backed implementation replaces the in-memory `HashMap` with Redis hashes keyed `moira:circuit:{provider_id}:{model_id}`, using Lua (`EVAL`) for the atomic check-and-transition (`Closed→Open`, `Open→HalfOpen`) that today is done under a single `tokio::Mutex` (`src/orchestration/controls.rs:520-562`).
3. **Idempotency**: `claim_idempotency` in `src/infra/repositories/admin.rs:559-634` gains an optional fast-path: before starting the Postgres transaction, attempt `SET moira:idem-lock:{key_hash}:{actor_fingerprint}:{operation} NX PX <ttl>` in Redis. On success, proceed to the existing Postgres transaction/advisory-lock path unchanged (Postgres remains sole durable ledger — no schema change). On failure (another replica holds the Redis lock), return the existing `idempotency_in_progress` 409 immediately, skipping the 5s poll loop. If Redis is unavailable/disabled, behavior is byte-for-byte what it is today (Postgres advisory lock only) — this is additive, never a hard dependency.
4. **Cluster admission**: on process startup (`src/app`), before binding the HTTP listener, Moira acquires/renews a row in a new `cluster_replica_leases` table via `INSERT ... ON CONFLICT` bounded by `max_replicas` (a new `Settings` field, default `1`). If the lease cannot be acquired (already `max_replicas` live leases with an unexpired heartbeat), the process logs a fatal error and exits non-zero — this fails `readyz`/pod startup, which is the correct Kubernetes-native way to cap replicas regardless of `kubectl scale`. The lease is renewed on a heartbeat interval and released on graceful shutdown; a background reaper (or a `WHERE heartbeat_at < now() - interval` predicate in the acquire query) reclaims leases from crashed pods.
5. **Redis pub/sub invalidation**: `src/infra/db.rs`'s existing `spawn_runtime_config_listener`/`listen_once` (43-80) stays exactly as is. A **new**, independent task in the same module subscribes to `RedisClient::invalidation_channel()` (already defined, `src/infra/redis.rs:41-43`; the field is set at `:33` from `settings.invalidation_channel.clone()`) and, on message, calls the same `cache.invalidate_all()/runtime_handles.invalidate_all()/circuits.reset_all()` triplet. Every runtime-config-mutating admin path (`ConversationService::put_*_policy`, `AdminService` PUT handlers) additionally calls `state.redis.publish_runtime_invalidation(...)` (already implemented, just never called) after the existing `state.runtime_cache.invalidate_all()` local call. Postgres LISTEN/NOTIFY remains authoritative; Redis is a latency optimization and a second signal path in case NOTIFY delivery to a given pod is delayed.
6. **Leader election**: `WorkerRegistry::spawn_supervisor` (`src/infra/workers.rs:119-129`) gains a Redis (or Postgres advisory-lock, see Open Decision below) leader-lock acquisition before running singleton-only job bodies (retention cleanup, provider health probing, cache warming); non-leader replicas keep running the per-replica-safe parts (job dequeue/execute is safe to run on every replica once jobs are queued and claimed row-by-row — see below) but skip the singleton `tick`-driven jobs.
7. **Durable worker queue**: new `worker_jobs` table (migration, see below) with `status`, `run_at`, `attempts`, `max_attempts`, `payload jsonb`, `dead_letter_at`. Workers claim a batch via `UPDATE ... SET status='claimed', claimed_by=$replica_id WHERE id IN (SELECT id FROM worker_jobs WHERE status='pending' AND run_at <= now() ORDER BY run_at FOR UPDATE SKIP LOCKED LIMIT $n) RETURNING *` — this is safe on every replica simultaneously by construction (no leader election needed for the queue itself, only for *enqueuing* singleton periodic jobs like "run retention cleanup every N minutes", which is where leader election in point 6 applies).

### Security boundaries — distributed-state topology

- Redis holds **no secrets, no plaintext credentials, no PII**: rate-limit/concurrency/circuit keys are `provider_id`/`application_id`/hashed-`external_user_id`/UUIDs only; idempotency locks store only the existing `key_hash`/`actor_fingerprint` (already-hashed values per P1-1's HMAC pepper design in plan 03) as the Redis key, no value payload beyond a lock marker.
- Redis connection reuses the existing `RedisSettings` (`url`, `namespace`) — no new secret material; if Redis requires auth, the password lives in the existing `url` (already the pattern for `MOIRA_REDIS__URL`).
- Redis is **never** a second source of truth for authorization or credential data — only ephemeral coordination state (counters, locks, pub/sub signals). If Redis is unreachable, execution must **fail closed** for concurrency/rate-limiting (reject the request, same posture as today's in-memory limiter reaching capacity) but **fail open** for the idempotency Redis fast-path and the pub/sub invalidation-signal add-on (Postgres remains authoritative for both, so Redis unavailability degrades latency/contention, not correctness).
- Cluster admission lease table is admin-invisible (no HTTP surface) — it is a process-startup gate only, reducing attack surface (no new authenticated endpoint to defend).

### DB/migration changes

New migration `migrations/0009_multi_replica_readiness.sql` (numbering continues after the existing highest migration — verify actual next number at implementation time by listing `migrations/`):
- `cluster_replica_leases` — `replica_id uuid primary key default gen_random_uuid()`, `pod_name text not null`, `acquired_at timestamptz not null default now()`, `heartbeat_at timestamptz not null default now()`, `released_at timestamptz`. Index on `heartbeat_at` for the reaper predicate.
- `worker_leader_leases` — `job_name text primary key`, `holder_replica_id uuid not null`, `acquired_at timestamptz not null`, `heartbeat_at timestamptz not null`. (Only needed if the **Postgres advisory-lock leader election** option is chosen over Redis — see Open Decision.)
- `worker_jobs` — `id uuid primary key default gen_random_uuid()`, `job_name text not null`, `payload jsonb not null default '{}'::jsonb`, `status text not null default 'pending' check (status in ('pending','claimed','running','completed','failed','dead_letter'))`, `run_at timestamptz not null default now()`, `attempts int not null default 0`, `max_attempts int not null default 5`, `claimed_by uuid`, `claimed_at timestamptz`, `last_error text`, `dead_letter_at timestamptz`, `created_at timestamptz not null default now()`. Index on `(status, run_at)` for the `SKIP LOCKED` claim query.

No changes to existing migration 0007 (memory/RAG) tables — the `worker_jobs.job_name` values will match the existing `WorkerSpec.name` strings (`"memory-extraction-retry"`, `"retention-cleanup"`, etc., `src/infra/workers.rs:52-90`) so this plan's queue and plan 04/11's job bodies compose without a further migration.

Sketch of the core `worker_jobs` DDL (final column list/constraints to be finalized by the implementing agent against the project's existing migration style, e.g. `moira_bump_resource_version()`-style triggers are **not** needed here since jobs are not optimistically-concurrent resources):

```sql
create table if not exists worker_jobs (
    id uuid primary key default gen_random_uuid(),
    job_name text not null,
    payload jsonb not null default '{}'::jsonb,
    status text not null default 'pending'
        check (status in ('pending', 'claimed', 'running', 'completed', 'failed', 'dead_letter')),
    run_at timestamptz not null default now(),
    attempts integer not null default 0 check (attempts >= 0),
    max_attempts integer not null default 5 check (max_attempts > 0),
    claimed_by uuid,
    claimed_at timestamptz,
    last_error text,
    dead_letter_at timestamptz,
    created_at timestamptz not null default now(),
    completed_at timestamptz
);

create index if not exists worker_jobs_claim_idx
    on worker_jobs (status, run_at)
    where status in ('pending', 'failed');

create table if not exists cluster_replica_leases (
    replica_id uuid primary key default gen_random_uuid(),
    pod_name text not null,
    acquired_at timestamptz not null default now(),
    heartbeat_at timestamptz not null default now(),
    released_at timestamptz
);

create index if not exists cluster_replica_leases_heartbeat_idx
    on cluster_replica_leases (heartbeat_at)
    where released_at is null;
```

The claim query for Wave 3's `WorkerQueue::claim_batch` follows the standard Postgres job-queue pattern:

```sql
update worker_jobs
set status = 'claimed', claimed_by = $1, claimed_at = now()
where id in (
    select id from worker_jobs
    where status = 'pending' and run_at <= now()
    order by run_at
    for update skip locked
    limit $2
)
returning *;
```

This is safe under arbitrary replica counts by construction (`for update skip locked` guarantees no two concurrent claimers ever return the same row), which is precisely why the durable-queue layer itself needs no leader election — only the *periodic enqueue* of singleton jobs (§ leader election below) does.

### API & OpenAPI changes

No public/admin **route** changes — no path is added, removed, or renamed, and no request DTO changes. Two behaviors are added:

1. **Startup lease denial (non-routed).** The process exits non-zero at startup if the cluster admission lease cannot be acquired — this surfaces to operators via pod `CrashLoopBackOff`/exit status and structured startup logs, not an HTTP response, so it carries no i18n key.
2. **`readyz` reports lease state (REQUIRED — upgraded from "nice-to-have" during the CONVENTIONS re-audit).** `GET /health/ready` (`src/http/health.rs:53-83`, which already calls `redis.ping()` at `:60-61`) must return `503` with `error.code = "cluster_lease_denied"` when this replica does not hold a live lease. Rationale: a replica can *lose* its lease mid-run (heartbeat renewal failing against a reachable-but-contended database) — if it keeps serving traffic while outside the admission ceiling, P3-2 is not actually fixed, only fixed-at-startup. This is the **only user-visible response this plan adds**, and it is the sole justification for the new `moira.error.cluster_lease_denied` catalog entry (§ i18n catalog additions).

Because `readyz`'s **response body shape** is unchanged (it already returns the standard error envelope on failure) and only a new `code` value is introduced, the OpenAPI surface change is limited to documenting the additional `503` condition. The PR description's "Breaking API/OpenAPI changes" section should record this as **non-breaking**. Per the repo's own `CLAUDE.md`, the `moira-openapi` skill must still be run for this status-code/description change.

### Backward compatibility

- `RedisSettings.enabled=false` (today's default posture per `redis_is_optional_by_default` test, `src/infra/redis.rs:83-89`) must continue to produce **exactly today's single-replica in-memory behavior** — the Redis-backed limiter/concurrency/circuit-breaker/leader-election/queue code paths are only selected when Redis is enabled AND a new `Settings.cluster.multi_replica_enabled` (name TBD) flag is set. This keeps single-replica MVP deployments (02–09) unaffected by this iteration landing in the same binary.
- The Helm `moira.validateDeployment` guard (`_helpers.tpl:16-26`) **stays in place** even after this iteration ships — it remains a cheap, first line of defense for `helm install`/`upgrade`. It is only safe to relax (bump the allowed `replicaCount`) once the distributed controls are live *and* the cluster-admission lease enforces the real ceiling — see Ordering below.

### Deployment implications

- New `values.yaml` keys: `redis.enabled`, `cluster.maxReplicas` (or similar), threaded through to `MOIRA_REDIS__ENABLED`, `MOIRA_CLUSTER__MAX_REPLICAS`.
- Helm `_helpers.tpl`'s `moira.validateDeployment` gains a **second mode**: if `.Values.cluster.multiReplicaEnabled` is true, allow `replicaCount` up to `.Values.cluster.maxReplicas` instead of hard-failing at `!= 1`; autoscaling guard relaxes correspondingly. This is a template-time change and remains a defense-in-depth companion to the DB-backed lease, not a replacement for it (§ Ordering below explains why both layers matter).
- Requires a Redis deployment/managed instance in any environment that sets `multiReplicaEnabled: true`; single-replica environments are unaffected (Redis remains optional per current default).

### Worked example — a rate-limited request under 3 replicas

Before this iteration: client sends 100 requests/minute against a policy limit of 60/minute, load-balanced roughly evenly across 3 pods. Each pod's `InMemoryRateLimiter` (`src/orchestration/controls.rs:453-491`) independently allows up to 60/minute *per pod* — the client can push ~180/minute before any pod starts rejecting, silently violating the configured policy by 3x.

After this iteration (`multi_replica_enabled=true`): the same 100 requests/minute hit `RedisRateLimiter::check`, which increments a shared Redis counter keyed `moira:ratelimit:{application_id}:{window_bucket}`. All 3 pods observe the same counter. Once the 61st request in the current window arrives (regardless of which pod receives it), `check` returns the existing `429 rate_limited` error (`src/orchestration/controls.rs:481-487`'s message/status code, unchanged) — the policy is enforced against the true cluster-wide request rate, matching what a single-replica deployment already guarantees today.

### Failure & recovery

- **Redis down, multi-replica mode on**: rate limiting and concurrency permits must fail closed (reject new work with `429`/`capacity_exhausted`, matching `CapacityExhaustion`'s existing error shape at `src/orchestration/controls.rs:406-425`) rather than silently falling back to unlimited or per-pod-only limits, which would defeat the purpose of the multi-replica control. Circuit breaker fail-mode: default to `Closed` (fail open) with a metric/log warning — a stuck-open circuit-breaker check must never itself become an outage; provider-level failures still surface through normal execution error handling.
- **Redis down, idempotency fast path**: fall back transparently to the existing Postgres advisory-lock path (already correct); no user-visible change beyond added latency.
- **Redis down, pub/sub invalidation**: no-op; Postgres LISTEN/NOTIFY (already working) is unaffected and remains the sole invalidation path.
- **Cluster lease holder crashes**: heartbeat expiry (configurable, e.g. 30s) frees the lease row for a new replica to claim; no manual intervention required.
- **Worker job crashes mid-processing**: `claimed_at` older than a stale-claim threshold is requeued to `pending` by the claim query's `run_at`/staleness check; `attempts` increments; after `max_attempts` the row moves to `dead_letter` with `last_error` populated and is excluded from further claims, surfaced via a metric (`worker_jobs_dead_letter_total`).

---

## Detailed Implementation

### `src/infra/redis.rs`
- Add `acquire_lock(key: &str, ttl: Duration) -> Result<Option<RedisLockGuard>, AppError>` using `SET key value NX PX ttl_ms`, and a paired `release_lock` (`DEL` guarded by the value token, via a small Lua script to avoid releasing a lock acquired by someone else after expiry/renewal race — the classic Redlock single-node caveat; document as a known single-instance-Redis limitation, not full Redlock, since Moira does not require Redlock-grade guarantees for this use case).
- Add `subscribe_invalidation(&self) -> impl Stream<Item=String>` (or a callback-based loop mirroring `spawn_runtime_config_listener`'s reconnect-with-backoff shape) wrapping `redis::aio::PubSub`.
- Add a small Lua-script-backed atomic counter/window helper (`incr_with_window`) for the Redis rate limiter and dynamic concurrency counters, mirroring the semantics already coded in `InMemoryRateLimiter::check` (`src/orchestration/controls.rs:461-490`) and `DynamicLimiter::try_acquire`/`Drop` (350-394) so behavior parity is exact (same window-reset-on-expiry semantics, same "count >= limit.max(1)" rejection rule).

### `src/orchestration/controls.rs`
- Add `RedisRateLimiter` implementing the same `check(key: String, limit: u32, window: Duration) -> Result<(), AppError>` signature as `InMemoryRateLimiter::check` (461), backed by the Redis Lua helper above, namespaced under `RedisClient::key(...)`.
- Add `RedisConcurrencyController` implementing `acquire`/`acquire_scoped` with the same signature as `ConcurrencyController` (233-260), returning the same `ExecutionPermits` — but permits now hold a `RedisPermitGuard` (decrements a Redis counter on `Drop`, using a `tokio::spawn`-detached best-effort decrement since `Drop` cannot be `async`; document this as a known limitation — a crashed process leaks a permit until a TTL-based Redis key expiry reclaims it, so **every Redis-backed counter must carry an expiry** as a safety net, unlike the in-memory version which has none because process death frees the `Arc` naturally).
- Add `RedisCircuitBreakerRegistry` mirroring `CircuitBreakerRegistry`'s `before_call`/`on_success`/`on_failure`/`reset_all` (impl at 513-611), state stored as a Redis hash per `(provider_id, model_id)`, transitions done via a single `EVAL` script for atomicity (replacing the `tokio::Mutex<HashMap>` critical section).
- Introduce a small internal enum/trait so `AppState` can hold either backend without leaking the choice into `src/application/execution.rs` call sites — e.g. `pub enum RateLimiterBackend { InMemory(InMemoryRateLimiter), Redis(RedisRateLimiter) }` with a `check` inherent method that matches and delegates; same pattern for concurrency and circuits. Keep the existing `InMemoryRateLimiter`/`ConcurrencyController`/`CircuitBreakerRegistry` types and their tests untouched — this is additive.

### `src/app/state.rs`
- `AppState::new` (42-102) selects the backend: if `settings.redis.enabled && settings.cluster.multi_replica_enabled` (new field), construct the Redis-backed variants using `redis.clone()`; else construct today's in-memory variants exactly as now (63-79 unchanged in the false branch).
- Add cluster-admission-lease acquisition as a new async step called from `src/app`'s process bootstrap (likely `src/app/mod.rs` or wherever `AppState::new`/`db::migrate`/`spawn_runtime_config_listener` are currently orchestrated — locate the exact bootstrap file before implementing) — this is a startup-blocking call, not part of `AppState::new` itself (which is synchronous today), so it belongs in the async `main`/bootstrap path alongside the existing `db::migrate(&pool)` and `spawn_runtime_config_listener` calls.

### `src/config/settings.rs`
- Extend `RedisSettings` (186-192): no new fields strictly required (namespace/url/invalidation_channel already sufficient) — add `lock_ttl_seconds: u64` (default e.g. 30) for the idempotency fast-path and leader-lease TTLs.
- New `ClusterSettings { multi_replica_enabled: bool, max_replicas: u32, lease_heartbeat_seconds: u64, lease_expiry_seconds: u64 }`, defaulting `multi_replica_enabled: false, max_replicas: 1` so existing deployments are unaffected until explicitly opted in.
- Extend `WorkerSettings` (194-202) with `leader_election_enabled: bool` (mirrors `multi_replica_enabled` by default) and `queue_poll_interval_seconds: u64`, `claim_batch_size: usize`, `stale_claim_seconds: u64`.

### `src/infra/db.rs`
- New function `acquire_cluster_lease(pool: &PgPool, settings: &ClusterSettings, pod_name: &str) -> Result<ClusterLeaseHandle, AppError>` — `INSERT` a new row after first deleting/counting rows with `heartbeat_at >= now() - interval` bounded by `max_replicas`; if the live-lease count already equals `max_replicas`, return an error the bootstrap path treats as fatal.
- New function `spawn_redis_invalidation_listener(redis: RedisClient, cache: RuntimeConfigCache, runtime_handles: ProviderRuntimeCache, circuits: CircuitBreakerRegistry) -> JoinHandle<()>` — structurally parallel to `spawn_runtime_config_listener` (43-57) but subscribing to `redis.subscribe_invalidation()` instead of `PgListener`; same reconnect-with-backoff loop shape (49-56).
- Every existing call site that already invokes `state.runtime_cache.invalidate_all()` after a runtime-config admin mutation (e.g. `ConversationService::put_conversation_policy` `src/application/conversation.rs:615`, `put_memory_policy:654`, `put_retrieval_policy:695`, `put_embedding_policy:736`, and the equivalent `AdminService` PUT handlers) gets one added line: `if let Some(redis) = &self.state.redis { let _ = redis.publish_runtime_invalidation(&payload).await; }` — best-effort, errors logged not propagated (Postgres NOTIFY already fired via the DB trigger `notify_moira_runtime_config_change()`, e.g. migration 0007:528-567, so Redis publish failure must never fail the mutation).

### `src/infra/workers.rs`
- Extend `WorkerRegistry`/`WorkerSupervisor` (9-163) with:
  - A `worker_id: Uuid` (or reuse the cluster-lease `replica_id`) generated once per process.
  - `try_acquire_leadership(job_name: &str) -> bool` — Postgres advisory-lock (`pg_try_advisory_lock` on a hash of `job_name`, held for the process lifetime via a dedicated long-lived connection) **or** Redis `SET NX PX` with periodic renewal (see Open Decision below — recommend Postgres advisory lock for leader election specifically, since it requires no new infra and Moira already uses this exact pattern successfully for admin idempotency, `src/infra/repositories/admin.rs:567`).
  - New `WorkerQueue` type wrapping the `worker_jobs` table: `enqueue(job_name, payload, run_at)`, `claim_batch(limit) -> Vec<ClaimedJob>` (the `SKIP LOCKED` query from Architecture §7), `complete(job_id)`, `fail(job_id, error, backoff)` (computes next `run_at` via `retry_base_delay_seconds`/`retry_max_delay_seconds` exponential backoff, already present as config fields at `src/config/settings.rs:199-200`, and moves to `dead_letter` once `attempts >= max_attempts`).
  - `run_supervisor` (131-153) is restructured: the existing tick loop keeps recording `state.metrics.record_worker_tick()`, but now also (a) on the leader replica only, enqueues periodic singleton jobs (e.g. "retention-cleanup" every N minutes) via `WorkerQueue::enqueue`, and (b) on every replica, polls `WorkerQueue::claim_batch` and dispatches claimed jobs to a job-name → handler map. This iteration adds the **plumbing and the dispatch loop**; it intentionally does not implement the handler bodies for `memory-extraction-retry`, `conversation-summarization-retry`, `embedding-retry`, `document-ingestion-retry` (those depend on plan 11's actual memory/RAG pipeline existing first) — register them as no-op/`todo!()`-free stub handlers that immediately `complete()` with a log line, so the queue and leader election are independently testable now, and plan 04/11 later swap the stub for the real body without touching this plumbing.

### `src/infra/metrics.rs`
- Add counters/gauges: `worker_jobs_claimed_total`, `worker_jobs_completed_total`, `worker_jobs_failed_total`, `worker_jobs_dead_letter_total`, `worker_leader_held` (gauge 0/1 per job_name per replica), `redis_lock_acquire_failures_total`, `cluster_lease_denied_total`. (Note: full Prometheus histograms are plan 05's scope; this iteration only needs to extend the existing `AtomicU64`-counter style — **verified layout: the `AtomicU64` counter fields live in `MetricsInner` at `src/infra/metrics.rs:19-27`; `MetricsSnapshot` (plain `u64`) is at `:30-41` and the `record_*` methods at `:43-92`** — so each new counter needs an `AtomicU64` field, a snapshot field, and a `record_*` method. Do not attempt histogram work here.)

### `migrations/0009_multi_replica_readiness.sql`
- As specified in Architecture § DB/migration changes. Append-only per `docs/project-structure.md`'s migration convention. **Verified during the CONVENTIONS re-audit: the highest existing migration is `0008_response_updated_at.sql` (full set `0001`–`0008`), so `0009` is free.** Re-confirm by listing `migrations/*.sql` before creating the file anyway — an intervening plan (03, 04, 07) may land a migration first, in which case this file takes the next free number and the PR description's "Migrations included" section records the actual filename.

### `charts/moira/templates/_helpers.tpl` and `charts/moira/values.yaml`
- `_helpers.tpl:16-26` `moira.validateDeployment`: add the `multiReplicaEnabled` branch described in Architecture § Deployment implications, preserving the existing hard-fail as the default (unchanged) behavior when the new value is unset/false. Sketch:

```gotemplate
{{- define "moira.validateDeployment" -}}
{{- if .Values.cluster.multiReplicaEnabled -}}
{{- if gt (int .Values.replicaCount) (int .Values.cluster.maxReplicas) -}}
{{- fail (printf "replicaCount %d exceeds cluster.maxReplicas %d" (int .Values.replicaCount) (int .Values.cluster.maxReplicas)) -}}
{{- end -}}
{{- if not .Values.redis.enabled -}}
{{- fail "cluster.multiReplicaEnabled requires redis.enabled" -}}
{{- end -}}
{{- else -}}
{{- if ne (int .Values.replicaCount) 1 -}}
{{- fail "Moira MVP requires replicaCount=1 because concurrency and rate limits are process-local; set cluster.multiReplicaEnabled to opt in" -}}
{{- end -}}
{{- if .Values.autoscaling.enabled -}}
{{- fail "Moira MVP does not support autoscaling unless cluster.multiReplicaEnabled is set" -}}
{{- end -}}
{{- end -}}
{{- if not .Values.secret.name -}}
{{- fail "secret.name must reference an existing Secret" -}}
{{- end -}}
{{- end -}}
```

  (Existing `secret.name` guard, `_helpers.tpl:23-25`, is preserved unchanged.)
- `values.yaml` (72 lines today) — **verified current state:** there is **no top-level `redis:` block and no `cluster:` block**. Redis appears only as env strings nested under `config:` — `MOIRA_REDIS__ENABLED: "false"` (`:61`), `MOIRA_REDIS__NAMESPACE` (`:62`), `MOIRA_REDIS__INVALIDATION_CHANNEL` (`:63`) — and **`MOIRA_REDIS__URL` is not present at all**. Existing top-level keys: `replicaCount`, `image`, `serviceAccount`, `service`, `ingress`, `resources`, `autoscaling`, `podDisruptionBudget`, `networkPolicy`, `serviceMonitor`, `config`, `secret`.
  Therefore this plan **adds** a top-level `redis: {enabled: false, url: "", namespace: "moira", invalidationChannel: "moira:runtime-config", lockTtlSeconds: 30}` block and a top-level `cluster: {multiReplicaEnabled: false, maxReplicas: 1, leaseHeartbeatSeconds: 10, leaseExpirySeconds: 30}` block, and rewires the three existing `config:` env strings to render **from** those blocks so there is a single source of truth rather than two places to set Redis on. Note `redis.url` is a **connection string that may embed a password** — it must be templated from the referenced Secret (`secret.name`, already guarded at `_helpers.tpl:23-25`), never written as a plaintext value in `values.yaml`.

### Configuration reference (new/extended settings, env var mapping follows the existing `MOIRA_<SECTION>__<FIELD>` convention already used for `MOIRA_REDIS__URL`, `src/infra/redis.rs:26`)

| Setting | Default | Env var | Purpose |
|---|---|---|---|
| `cluster.multi_replica_enabled` | `false` | `MOIRA_CLUSTER__MULTI_REPLICA_ENABLED` | Master switch selecting Redis-backed vs. in-memory control-plane backends in `AppState::new`. |
| `cluster.max_replicas` | `1` | `MOIRA_CLUSTER__MAX_REPLICAS` | Ceiling enforced by the Postgres `cluster_replica_leases` admission gate, independent of Helm's template-time guard. |
| `cluster.lease_heartbeat_seconds` | `10` | `MOIRA_CLUSTER__LEASE_HEARTBEAT_SECONDS` | How often a live replica renews `cluster_replica_leases.heartbeat_at`. |
| `cluster.lease_expiry_seconds` | `30` | `MOIRA_CLUSTER__LEASE_EXPIRY_SECONDS` | Staleness threshold after which a crashed replica's lease is reclaimable. |
| `redis.lock_ttl_seconds` | `30` | `MOIRA_REDIS__LOCK_TTL_SECONDS` | TTL for idempotency fast-path locks and leader-election renewals (if Redis-based leader election is chosen, see Open Decisions). |
| `workers.leader_election_enabled` | mirrors `cluster.multi_replica_enabled` | `MOIRA_WORKERS__LEADER_ELECTION_ENABLED` | Gates whether `WorkerSupervisor` attempts leadership acquisition before enqueuing singleton jobs. |
| `workers.queue_poll_interval_seconds` | `5` | `MOIRA_WORKERS__QUEUE_POLL_INTERVAL_SECONDS` | How often each replica polls `worker_jobs` for claimable work. |
| `workers.claim_batch_size` | `10` | `MOIRA_WORKERS__CLAIM_BATCH_SIZE` | Rows claimed per poll via the `SKIP LOCKED` query. |
| `workers.stale_claim_seconds` | `300` | `MOIRA_WORKERS__STALE_CLAIM_SECONDS` | Threshold after which a `claimed`/`running` job with no completion is requeued. |

All new settings default to values that preserve exactly today's single-replica behavior — this table exists so the Wave 1 "Helm/config agent" and the Wave 2 bootstrap agent agree on field names before either starts (see Multi-Agent Workflow, Wave 0).

### i18n catalog additions (CONVENTIONS.md §4 — binding)

Every user-visible response this plan can produce must carry a stable `message_key` **and** a default English message. The derivation rule is `format!("moira.error.{}", code())` (`src/error.rs:146-148`, verified), so **the catalog key suffix must exactly equal the `code` string passed to `AppError::coded`**. Entries go in `src/i18n/catalog/errors.rs` (`RESPONSE_ERROR_CATALOG`) / `src/i18n/catalog/notices.rs` (`RESPONSE_NOTICE_CATALOG`) and must be mirrored into `docs/i18n-response-catalog.json` **in the same PR** (§4.4 — hand-synced today; drift is a review failure until plan 06 adds the drift test).

**Verified state of the real catalog at audit time** (61 unique keys, Rust catalog and JSON mirror confirmed in sync):

| Key | Status | Notes |
|---|---|---|
| `moira.error.rate_limited` | **Already exists** (`errors.rs:44-48`) | Emitted today by `InMemoryRateLimiter::check` via `AppError::coded(429, "rate_limited", "public execution rate limit exceeded")` (`src/orchestration/controls.rs:481-487`, verified). `RedisRateLimiter` **must emit the byte-identical code string** so the derived key is unchanged — no new entry, but the contract-equivalence test below must prove it. |
| `moira.error.idempotency_conflict` | **Already exists** (`errors.rs:24-28`) | Unchanged by the Redis fast path. |
| `moira.error.capacity_exhausted` | **MISSING — this plan must add it** | **Verified pre-existing §4 violation on this plan's exact path.** `src/application/public.rs:1971` maps `ExecutionFailureClass::CapacityExhausted` to the wire code `"capacity_exhausted"` with `429` (`:1939`), so a real `429` response is returned today whose `message_key` is `moira.error.capacity_exhausted` — a key that does **not** exist in `RESPONSE_ERROR_CATALOG`. Since this plan replaces the backend that decides capacity exhaustion, it must close the gap. `default_message`: "Execution capacity is exhausted." `description`: "Used when global, provider, application, or user execution capacity is unavailable." |
| `moira.error.idempotency_in_progress` | **MISSING — this plan must add it** | **Verified pre-existing §4 violation on this plan's exact path.** The code `"idempotency_in_progress"` is emitted at `src/infra/repositories/admin.rs:576` and `:610`, asserted by `tests/admin_idempotency.rs:854`, and declared in ten `#[utoipa::path]` `409` descriptions across `src/http/admin.rs` — but the catalog contains only `idempotency_conflict` and `idempotency_not_supported_for_stream`. This plan's Redis fast path is the *new emitter* of this exact 409, so it must add the entry. `default_message`: "An identical request with this idempotency key is already in progress." `description`: "Used when a concurrent request holds the idempotency claim for the same key." |
| `moira.error.cluster_lease_denied` | **New** | See the scope note below — surfaced by `/health/ready` as `503` when this replica's `cluster_replica_leases` row is lost/expired mid-run. `default_message`: "This replica does not hold a valid cluster admission lease." `description`: "Used when the replica-count admission lease is denied or has expired." |
| `moira.error.worker_queue_capacity_exceeded` | **New** | Returned by `WorkerQueue::enqueue` when the configured pending-depth cap is reached. It has no HTTP surface *in this plan*, but it is an `AppError` that will propagate to a response the moment a synchronous caller enqueues (plan 11's summarization/extraction hooks do exactly this), so the entry lands with the code, not later. `default_message`: "The background job queue is at capacity." `description`: "Used when a background job cannot be enqueued because the queue depth limit is reached." |

**Honest non-additions** (do not invent keys for these):
- **Dead-lettering** is an internal terminal state observed via the `worker_jobs_dead_letter_total` metric and structured logs. This plan adds **no** HTTP surface that reports it, so it gets **no** `moira.notice.*` entry. If a future plan adds a job-status endpoint, that plan adds the key.
- **Cluster-lease denial at process startup** produces a fatal structured log and a non-zero exit, not a response — there is no HTTP envelope to attach a key to. The `moira.error.cluster_lease_denied` key above is justified only by the *mid-run* `readyz` case.

**Scope addition discovered during re-audit (was previously an under-specified "nice-to-have"):** § API & OpenAPI changes currently defers `readyz` reporting cluster-lease state to "a future iteration ... flag as an open nice-to-have." That is now **in scope and required**. Reason: once a replica can *lose* its lease mid-run (heartbeat renewal failing against a reachable-but-contended DB), a pod that keeps serving traffic while outside the admission ceiling defeats the entire point of P3-2. `/health/ready` (`src/http/health.rs:53-83`, which already calls `redis.ping()` at `:60-61`) must therefore fail with `503` + `moira.error.cluster_lease_denied` when the lease is not held. This is the only user-visible response this plan adds, and it is exactly why the key above is needed.

**Required i18n test** (§4.5, exemplar `tests/http_error_contract.rs`): assert every key this plan adds resolves through `crate::i18n::catalog::is_known_key` / `default_message_for_key` (`src/i18n/catalog/mod.rs:30-38`, already implemented), and assert the corresponding HTTP responses carry a **non-empty `message_key` and non-empty `message`**. Named tests are listed in § Verification.

---

## Multi-Agent Workflow

**Wave 0 — Coordinator setup (sequential, blocking).** One agent reads this plan file plus the four grounding files listed at the top, confirms the actual next-free migration number, confirms the actual location of process bootstrap (`src/app/mod.rs` or `main.rs` — wherever `AppState::new`/`db::migrate`/`spawn_runtime_config_listener` are wired together today), and posts a short "ground truth" note the other waves read before starting. This avoids two agents guessing differently at file paths not explicitly given here.

**Wave 1 — Parallel, disjoint file ownership (4 agents).**
1. **Redis primitives agent** — owns `src/infra/redis.rs` only (lock/subscribe/Lua-counter helpers). No other agent touches this file in Wave 1.
2. **Distributed controls agent** — owns `src/orchestration/controls.rs` (additive: new `Redis*` structs + backend enum wrappers). Depends on Wave 1's `RedisClient` additions being *declared* (agrees on signatures via the Wave 0 note) but can write against a stub signature and adjust once Wave 1 lands — recommend running Wave 1 fully before Wave 1's controls agent starts if signature churn risk is judged too high; otherwise both can run in parallel if the coordinator freezes the `RedisClient` public method signatures first.
3. **Migration + admission-lease agent** — owns the new migration file and `src/infra/db.rs` additions (`acquire_cluster_lease`, `spawn_redis_invalidation_listener`). No overlap with controls.rs or redis.rs (only calls into `RedisClient`'s already-frozen public methods).
4. **Helm/config agent** — owns `charts/moira/templates/_helpers.tpl`, `charts/moira/values.yaml`, `src/config/settings.rs` (new `ClusterSettings`, `RedisSettings`/`WorkerSettings` field additions). Fully disjoint from the other three.

**Wave 2 — Sequential, single owner (depends on Wave 1 completing).**
- One agent wires `src/app/state.rs` (backend selection) and the process-bootstrap file (lease acquisition call, Redis-invalidation-listener spawn) — this file necessarily touches the union of what Wave 1 built, so it must run after Wave 1 merges, not in parallel with it.

**Wave 3 — Parallel, disjoint (2 agents), depends on Wave 2.**
1. **Worker queue agent** — owns `src/infra/workers.rs` (leader election + `WorkerQueue` + dispatch loop) and `src/infra/metrics.rs` additions.
2. **Publish-invalidation call-site agent** — owns the small edits across `src/application/conversation.rs` (and the equivalent `AdminService` PUT handlers in `src/application/admin.rs`) adding the `publish_runtime_invalidation` call after each existing `invalidate_all()`. Disjoint files from the worker-queue agent.

**Checkpoints.** After each wave: `cargo fmt --check` + `cargo clippy --workspace --all-targets --all-features -- -D warnings` + `cargo test --workspace --all-features` must pass before the next wave starts (sequential waves depend on compiling code; parallel-wave agents within a wave should each run these locally on their own files/impacted tests before declaring done, but the **gate** is enforced once per wave after merge).

**Read-only reviewers.** After Wave 3, a review-only agent (no edit tools) re-reads the diff against this plan's Architecture section, specifically checking: (a) in-memory backends remain default/unaffected when `multi_replica_enabled=false`, (b) no Redis code path can silently produce **wrong** rate-limit/concurrency answers under Redis unavailability (must fail closed per Architecture §Failure & recovery), (c) Postgres LISTEN/NOTIFY code (`src/infra/db.rs:43-80`) is untouched byte-for-byte.

**Conflict avoidance.** The file-ownership table above is the source of truth; no two agents in the same wave touch the same file. `src/orchestration/controls.rs` and `src/infra/redis.rs` are the highest-conflict-risk files (both touched across waves) — the coordinator note in Wave 0 must freeze their public signatures before Wave 1 starts to avoid rework.

---

## Interfaces & Contracts

No new HTTP endpoints. Behavioral/contract changes are internal:

- **Rate-limit / concurrency rejection**: unchanged HTTP contract — still surfaces as today's `429`/`capacity_exhausted`-style `ExecutionFailure` → `AppError` mapping (`src/orchestration/controls.rs:406-425`); only the *backend* deciding rejection changes, never the response shape.
- **Idempotency**: unchanged HTTP contract (`409 idempotency_in_progress` / `409 idempotency_conflict`, same `message_key`s as today, `src/infra/repositories/admin.rs:575-613`). The Redis fast path only changes *latency*, never the outcome or status code — this must be verified by a contract test asserting identical response bodies with Redis enabled vs. disabled for the same conflicting-request scenario.
- **Process startup failure (lease denied)**: not an HTTP contract — process exits non-zero, pod never reaches `Ready`. Document the exit code and log message so operators can distinguish "lease denied" from other startup failures (e.g. `AppError::Config`-style structured log with a distinct `reason: "cluster_lease_denied"` field).
- **Transaction boundaries**: `claim_idempotency`'s existing Postgres transaction (`src/infra/repositories/admin.rs:559-634`) is unchanged; the Redis fast path executes *before* that transaction opens, as a pure optimization, never inside it.
- **Cache invalidation**: dual-path (Postgres NOTIFY, authoritative; Redis pub/sub, additive/faster) — both must independently be idempotent (`invalidate_all()`/`reset_all()` are already idempotent, verified by existing code).
- **Concurrency behavior**: the `Drop`-based release semantics of the `DynamicPermit` guards held inside `ExecutionPermits` (`src/orchestration/controls.rs:389-394`) must be preserved by the Redis-backed permit guard — verify no permit leak on panic-during-drop by testing a forced task abort mid-execution.
- **SSE**: unaffected — streaming concurrency permits (`_provider_stream`) use the same `ExecutionPermits` type regardless of backend.

---

## Verification

**Binding rule (CONVENTIONS.md §3): both a unit layer and an e2e layer are mandatory. A plan with only one layer is incomplete and must not be merged.** "E2E" means the behavior is exercised through its real external surface (HTTP, or a real process-lifecycle/bootstrap path) against a **real PostgreSQL 16 + pgvector** *and* a **real Redis 7** — not through an internal function call.

**Environment facts (verified):** CI already provisions both services — `.github/workflows/ci.yml:13-25` runs `pgvector/pgvector:pg16` and `:26-34` runs `redis:7-alpine`, with `MOIRA_REDIS__ENABLED: true` / `MOIRA_REDIS__URL: redis://localhost:6379/0` at `:39-40`. **No test touches Redis today** (verified: zero Redis references across `tests/`), so the first deliverable of the test work is extending the harness, not just adding cases.

### Harness prerequisites (do this first)

- Extend `tests/support/mod.rs` (496 lines today) with a `test_redis()` helper mirroring the existing `test_pool()` contract at `:427-471`, including the **fail-closed-in-CI** pattern verified at `:430-441`: when `MOIRA_TEST_REDIS_URL` (or the existing `MOIRA_REDIS__URL`) is absent **and** `CI` is set, `panic!` — never silently skip. Outside CI, `eprintln!` + skip, exactly as `test_pool` does today.
- Every Redis-backed test must **namespace its keys per test** (reuse the UUIDv7-style isolation the harness already relies on, `00-audit-report.md` P2-13) and flush only its own namespace on teardown — never `FLUSHDB`, which would break parallel test runs.
- Add a two-instance fixture: `LifecycleFixture` (`tests/support/mod.rs:114-407`) gains the ability to build **two `AppState`s sharing one Postgres pool and one Redis namespace**, each fronted by its own `MoiraHttpServer::start` (`:83-103`). This is the "two simulated instances" substrate every distributed e2e below requires; it is a genuine e2e surface (two independent HTTP servers, two independent control-plane backends) without needing two OS processes.

### Unit layer (no database, no Redis — pure logic, colocated `#[cfg(test)] mod tests`)

**Correction to a prior claim in this plan:** an earlier draft asserted "`src/orchestration/controls.rs`'s absence of inline tests today." That is **wrong** — verified: `controls.rs` already has a `#[cfg(test)] mod tests` block at **lines 683-942** (~260 lines). New unit tests extend that existing module; they do not create the first one.

Pure-logic functions must be factored so they are testable **without** a Redis or Postgres connection (take `now: Instant`/`DateTime<Utc>` and counter values as parameters rather than reading the clock or the socket inline). This is a design constraint on the implementation, not just a testing preference.

- `src/orchestration/controls.rs` (extend the existing `mod tests` at 683-942) — **token-bucket / window math:**
  - `token_bucket_resets_count_when_window_elapsed`
  - `token_bucket_rejects_at_exactly_limit_not_above` (pins the verified `bucket.count >= limit.max(1)` rule at `controls.rs:481`)
  - `token_bucket_treats_zero_limit_as_one` (pins the `limit.max(1)` clamp)
  - `token_bucket_math_is_identical_between_backends` (same inputs → same allow/deny decision for the in-memory and Redis window arithmetic, asserted on the pure function, before any I/O)
  - `capacity_scope_maps_to_expected_failure_flags` (pins the verified `retryable=false` / `fallback_eligible` mapping at `controls.rs:406-425`)
- `src/orchestration/controls.rs` — **leader-election state transitions** (pure state machine, extracted from the lock I/O):
  - `leader_state_follower_to_leader_on_successful_acquire`
  - `leader_state_leader_to_follower_on_renewal_failure`
  - `leader_state_renewal_within_ttl_keeps_leadership`
  - `leader_state_expired_lease_never_reports_leader`
- `src/infra/db.rs` (new colocated `mod tests`) — **lease / TTL arithmetic:**
  - `lease_is_live_when_heartbeat_within_expiry`
  - `lease_is_reclaimable_when_heartbeat_older_than_expiry`
  - `lease_expiry_must_exceed_heartbeat_interval` (config validation: `lease_expiry_seconds` ≤ `lease_heartbeat_seconds` is a misconfiguration that must be rejected at startup, not discovered as lease flapping in production)
  - `live_lease_count_excludes_released_and_stale_rows`
- `src/infra/workers.rs` (new colocated `mod tests`) — **retry/backoff computation and claim logic:**
  - `backoff_grows_exponentially_from_retry_base_delay` (against the verified existing config fields `retry_base_delay_seconds`/`retry_max_delay_seconds`, `src/config/settings.rs:199-200`)
  - `backoff_is_clamped_at_retry_max_delay`
  - `backoff_is_deterministic_for_a_given_attempt_count` (or, if jitter is added, `backoff_jitter_stays_within_declared_bounds` — pick one and test it; do not leave jitter unspecified)
  - `job_moves_to_dead_letter_exactly_at_max_attempts` (boundary: `attempts == max_attempts` dead-letters, `attempts == max_attempts - 1` retries)
  - `claim_predicate_selects_only_pending_and_due_jobs` (pure predicate over a fixture row set: `status='pending' and run_at <= now`)
  - `claim_predicate_includes_stale_claimed_jobs_past_threshold`
- `src/infra/redis.rs` (extend the existing `mod tests`; note the verified `redis_is_optional_by_default` test spans **83-90**, not 83-89) — **lock-token logic:**
  - `lock_release_script_rejects_mismatched_token` (the value-token guard from § Risks & Rollback, asserted on the script's decision logic)
  - `redis_key_namespacing_is_stable_and_collision_free`

### E2E layer (`tests/`, real PostgreSQL 16 + pgvector **and** real Redis 7)

**Concurrency rule (CONVENTIONS.md §3, finding P2-12): every interleaving test must use acknowledgement gates — `tokio::sync::Barrier`, `oneshot`/`mpsc` channels, or `Notify` — never `sleep()`.** New sleep-based interleaving is rejected in review. The existing sleep-based tests at `tests/admin_idempotency.rs:977,1259` and `tests/execution_lifecycle.rs:979,1002` are the anti-pattern to avoid, not the exemplar. Where a test genuinely must observe a **timeout or TTL elapsing** (lease expiry, leader handover), do not sleep the wall clock: make the TTL/heartbeat interval a test-injected configuration value set to a small deterministic value, and gate the *observation* on a channel signal from the code under test — assert on the resulting state transition, not on elapsed time.

- **`tests/distributed_rate_limit.rs`** (new) — distributed rate limiting across two simulated instances:
  - `rate_limit_is_enforced_cluster_wide_across_two_instances` — the core P3-1 proof: with limit N, drive both instances concurrently through their real HTTP surfaces behind a `Barrier`; assert exactly N succeed and the rest receive `429`, rather than N-per-instance (the pre-iteration bug from § Worked example).
  - `rate_limit_response_body_is_identical_across_backends` — same request, in-memory vs Redis backend; assert identical status, `error.code == "rate_limited"`, and identical non-empty `message_key`/`message`.
  - `concurrency_permits_are_enforced_cluster_wide_across_two_instances`
  - `concurrency_permit_is_released_when_instance_task_is_aborted` (the permit-leak guard from § Interfaces & Contracts)
  - `circuit_opened_on_instance_a_is_observed_open_on_instance_b`
- **`tests/cluster_admission.rs`** (new) — admission-lease enforcement:
  - `lease_denied_once_max_replicas_reached` — bootstrap a third instance against `max_replicas=2`; assert the bootstrap call returns the fatal lease-denied error.
  - `stale_lease_reclaimed_after_heartbeat_expiry`
  - `lease_released_on_graceful_shutdown`
  - `readyz_returns_503_and_cluster_lease_denied_when_lease_lost_mid_run` — the e2e for the § i18n scope addition; asserts status `503`, `error.code == "cluster_lease_denied"`, and non-empty `message_key` + `message`.
- **`tests/worker_leader_election.rs`** (new) — single winner:
  - `exactly_one_of_n_instances_holds_leadership` — N instances race through a `Barrier`; assert exactly one reports leader (not zero, not two).
  - `leadership_transfers_to_a_follower_after_holder_releases`
  - `singleton_job_is_enqueued_exactly_once_across_handover` (not zero, not twice)
  - `non_leader_instances_still_claim_and_execute_queued_jobs` (pins the design point that only *enqueue* is leader-gated, not *claim*)
- **`tests/worker_queue.rs`** (new) — durable claim/retry/dead-letter:
  - `concurrent_claimers_never_claim_same_job` — N claimers gated on a `Barrier`, asserting the `FOR UPDATE SKIP LOCKED` guarantee.
  - `failed_job_backs_off_and_is_retried_until_max_attempts`
  - `job_moves_to_dead_letter_after_max_attempts_and_is_never_reclaimed`
  - `stale_claimed_job_is_requeued_after_claim_expiry`
  - `job_survives_instance_restart_and_is_completed_by_another_instance` (the durability claim — the whole point of P3-5)
  - `queue_capacity_exceeded_returns_worker_queue_capacity_exceeded` — asserts the new i18n key's code and non-empty `message_key`/`message`.
- **`tests/redis_chaos.rs`** (new) — **Redis-failure behavior. The fail-open vs fail-closed choice is an explicit, tested decision, not an emergent behavior.** The binding decision table:

  | Subsystem | Redis unavailable → | Rationale |
  |---|---|---|
  | Rate limiter | **FAIL CLOSED** (`429`) | Serving unlimited traffic silently violates the configured policy — the exact bug this plan exists to fix. |
  | Concurrency permits | **FAIL CLOSED** (`429` / capacity exhausted) | Same. |
  | Circuit breaker | **FAIL OPEN** (treat as `Closed`) + warn metric | A breaker that cannot be read must not itself become the outage; real provider failures still surface through normal execution error handling. |
  | Idempotency fast path | **FAIL OPEN** (fall through to the Postgres advisory lock) | Postgres remains the durable ledger of record; Redis only removes contention latency. |
  | Redis pub/sub invalidation | **FAIL OPEN** (no-op) | Postgres `LISTEN/NOTIFY` (`src/infra/db.rs:43-80`) is authoritative and unaffected. |
  | Cluster admission lease | **N/A** — Postgres-backed, does not use Redis | Deliberate: the admission ceiling must not depend on the optional dependency. |

  - `rate_limit_fails_closed_when_redis_unreachable`
  - `concurrency_fails_closed_when_redis_unreachable`
  - `circuit_breaker_fails_open_when_redis_unreachable`
  - `idempotency_falls_back_to_postgres_when_redis_unreachable`
  - `pubsub_invalidation_is_a_noop_when_redis_unreachable_and_notify_still_works`
  - `every_redis_call_is_bounded_by_a_timeout_when_redis_is_slow` — latency injection; asserts no unbounded await, mirroring the verified `timeout(self.connect_timeout, ...)` pattern at `src/infra/redis.rs:46-58`.
  - `startup_fails_when_redis_unreachable_and_multi_replica_enabled` — encodes Open Decision 2's recommendation; if the decision flips, this test flips with it, but the behavior must be **pinned by a test either way**.
- **`tests/idempotency_redis_fastpath.rs`** (new, or extend `tests/admin_idempotency.rs`):
  - `redis_and_postgres_paths_produce_identical_response_bodies` — the contract-equivalence test required by § Interfaces & Contracts.
  - `idempotency_in_progress_response_carries_message_key_and_message` — closes the verified catalog gap; asserts `409`, `error.code == "idempotency_in_progress"`, non-empty `message_key` + `message`.
- **`tests/http_error_contract.rs`** (extend the existing exemplar) — the §4.5 i18n presence assertions:
  - `new_multi_replica_error_keys_exist_in_catalog` — asserts `is_known_key` (`src/i18n/catalog/mod.rs:30-32`) for `moira.error.capacity_exhausted`, `moira.error.idempotency_in_progress`, `moira.error.cluster_lease_denied`, `moira.error.worker_queue_capacity_exceeded`, and that `moira.error.rate_limited` still resolves.
  - `i18n_json_mirror_matches_rust_catalog_for_new_keys` — manual-sync guard per §4.4 until plan 06's drift test lands.

### Other required verification

- **Migration**: clean apply of `migrations/0009_*.sql` against a fresh DB **and** against the full existing chain (verified: highest existing migration is `0008_response_updated_at.sql`, so **0009 is free** — re-confirm at implementation time), per the existing CI migration-contract job.
- **Query-plan note**: not a pgvector concern in this plan (that is plan 11's scope). Required deliverable: a short doc note on the `worker_jobs` claim query's `EXPLAIN ANALYZE` at a realistic queue depth (e.g. 10k pending rows) confirming the `(status, run_at)` index is selected and does not degrade under load.
- **Security / no-secret-in-Redis**: `tests/redis_chaos.rs` or a colocated test asserts no secret, credential, ciphertext, nonce, or raw PII value is ever written as a Redis key or value — only UUIDs, already-hashed fingerprints, and counters. Mirrors the existing no-secret-leak philosophy in `src/security/masking::tests`.
- **Default-path regression**: with `multi_replica_enabled=false`, the entire pre-existing suite must pass **unchanged** — this is the Definition of Done's first bullet and is verified by running `cargo test --workspace --all-features` with no Redis env set.
- **Required Rust gates (CONVENTIONS.md §2, verbatim — run at every wave checkpoint and before the PR opens):**
  - `cargo fmt --check`
  - `cargo clippy --workspace --all-targets --all-features -- -D warnings`
  - `cargo test --workspace --all-features`
  - `cargo build --release --locked`
  - clean PostgreSQL migration validation (apply 0001→0009 on an empty database)

---

## Definition of Done

- With `redis.enabled=false` or `cluster.multi_replica_enabled=false` (the default), all behavior, tests, and metrics are byte-for-byte identical to pre-iteration code — verified by running the full existing test suite unchanged and diffing metrics/log output on a sample run.
- With multi-replica mode enabled and ≥2 real replicas running against shared Postgres+Redis in a test environment: a rate limit configured at N requests/window is enforced as N **total across both replicas**, not N-per-replica (verified by an integration test driving both replicas concurrently).
- Concurrency ceilings (global/provider/application/user) are similarly enforced cluster-wide, verified the same way.
- Circuit-breaker state opened by replica A is observed as open by replica B within one Redis-invalidation round-trip (verified by a test that trips the breaker on one client connection and asserts the next call routed to the "other" logical replica sees `CircuitOpen`).
- `kubectl scale --replicas=N` beyond the configured `cluster.maxReplicas` results in the (N+1)th pod failing to become `Ready` (lease denied), not silently running with degraded/incorrect distributed state.
- Killing the current worker-leader process results in exactly one other replica taking over leadership within the configured TTL, verified by asserting singleton jobs are neither skipped nor double-enqueued across the handover.
- A job enqueued to `worker_jobs`, whose handler fails every attempt, transitions to `dead_letter` after `max_attempts` and is visible via the new metrics — never retried forever, never silently dropped.
- All Verification-section gates pass, including the Redis-chaos tests and the migration-chain validation.
- Documentation updated: `docs/todo.md` Phase 6 items covered by this iteration are marked done or rewritten to reflect what actually shipped; `charts/moira/values.yaml`/README gain operator-facing notes on enabling multi-replica mode.

### CONVENTIONS.md §8 compliance checklist (binding — every box must be ticked before merge)

- [ ] Work performed on branch **`plan/10-multi-replica-readiness`**; PR opened with all seven required description sections (Plan link · Findings addressed · Migrations included · Breaking API/OpenAPI changes · Test evidence · Rollback procedure · Deferred follow-ups).
- [ ] All gates in CONVENTIONS.md §2 pass: `cargo fmt --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace --all-features`, `cargo build --release --locked`, plus clean migration application from an empty database. (Frontend gates: **N/A** — this plan touches no console code.)
- [ ] **Unit tests** delivered and passing — token-bucket math, lease/TTL arithmetic, leader-election state transitions, worker retry/backoff computation, and queue claim-predicate logic, all named in § Verification.
- [ ] **E2E tests** delivered and passing — HTTP-level against a **real PostgreSQL 16 + pgvector and a real Redis 7**: `tests/distributed_rate_limit.rs`, `tests/cluster_admission.rs`, `tests/worker_leader_election.rs`, `tests/worker_queue.rs`, `tests/redis_chaos.rs`, `tests/idempotency_redis_fastpath.rs`. (Playwright: **N/A** — no console surface.)
- [ ] **No new `sleep()`-based interleaving** in any concurrency test; acknowledgement gates (`Barrier`/`oneshot`/`Notify`) used throughout (finding P2-12).
- [ ] DB/Redis-dependent tests **fail closed in CI** — `panic!` when `CI` is set and the required connection env var is absent, matching the existing pattern at `tests/support/mod.rs:430-441`.
- [ ] Every new error/notice string has an i18n **key + English `default_message` + `description`** in `src/i18n/catalog/errors.rs`, **mirrored into `docs/i18n-response-catalog.json` in the same PR**, with a test asserting presence: `moira.error.capacity_exhausted`, `moira.error.idempotency_in_progress`, `moira.error.cluster_lease_denied`, `moira.error.worker_queue_capacity_exceeded`. Verified: `moira.error.rate_limited` and `moira.error.idempotency_conflict` **already exist** and must be reused, not duplicated.
- [ ] Every new error `code` string exactly matches its catalog key suffix, per the verified derivation rule `format!("moira.error.{}", code())` (`src/error.rs:146-148`).
- [ ] Frontend toolchain / Atomic Design items (§8 bullet 6): **N/A** — no console code in this plan.
- [ ] Auth-touching items (§8 bullet 7): **N/A** — this plan adds no authenticated endpoint and no auth configuration. The cluster-admission lease is deliberately an unauthenticated, non-routed startup gate.
- [ ] **No secret-leak: verified by test** — the no-secret-in-Redis assertion in § Verification proves no credential, ciphertext, nonce, or raw PII is ever written to a Redis key or value.
- [ ] Every finding claimed closed (`P3-1`..`P3-5`) is backed by a **named, passing test** — "implemented" is not "done" (§3).
- [ ] PR **merged** with all gates green (§1.5) — the plan is not done at PR-open.

---

## Risks & Rollback

- **Security**: a Redis-lock-release Lua-script bug that releases a lock acquired by a different holder (classic non-Redlock single-instance caveat) could cause two replicas to believe they are the sole idempotency-lock holder or leader simultaneously. Mitigation: value-token-guarded release (only release if the stored value matches the acquirer's token) — this is a standard mitigation, not full Redlock, and must be explicitly unit-tested (two concurrent "acquire→hold→release" sequences with an injected delay must never both succeed).
- **Data-migration**: the new tables (`cluster_replica_leases`, `worker_leader_leases`/reuse via Postgres advisory locks, `worker_jobs`) are pure additions with no backfill and no touch to existing tables — lowest-risk migration category; still verify against the full migration chain per Verification.
- **Compatibility**: default-off flags (`multi_replica_enabled: false`) mean this iteration is safe to merge and deploy to existing single-replica MVP environments with zero behavior change — this is the primary risk-reduction lever and must be preserved throughout implementation (any code path that runs unconditionally regardless of the flag is a defect).
- **Deployment**: enabling multi-replica mode requires Redis to be provisioned and reachable; document this as a hard prerequisite in the Helm chart's values comments and README before any operator flips the flag.
- **Rollback procedure**: setting `redis.enabled=false`/`cluster.multi_replica_enabled=false` and scaling back to `replicaCount=1` fully reverts to pre-iteration behavior without a code rollback (the in-memory code paths are untouched and remain the default). If a code-level rollback is needed, this iteration's changes are additive-only (no modification to existing `InMemoryRateLimiter`/`ConcurrencyController`/`CircuitBreakerRegistry`/`spawn_runtime_config_listener` behavior), so reverting the commit range is low-risk for existing deployments.
- **Deferred follow-ups (explicitly out of scope, flag for later plans)**: full Redlock-grade multi-Redis-node locking (only relevant at much larger scale — flag as a future decision if Moira ever runs Redis in a non-single-primary topology); implementing the actual job handler bodies for memory/RAG workers (plan 11) and retention cleanup (plan 04); `readyz`/`healthz` surfacing cluster-lease/leader state (nice-to-have, not required); full OpenTelemetry/Prometheus histogram wiring for the new Redis/queue metrics (plan 05's broader observability scope covers histograms — this plan only adds counters).

---

## Rollout Sequencing Relative to the Helm Guard

The dependency-graph note in `01-roadmap-and-dependencies.md` §1.4 is explicit: "the in-memory limiter/circuit state (P3-1) is the only thing between us and multi-replica, and it is deliberately deferred." This plan is what removes that blocker, and the rollout must happen in this order, never skipped or reordered:

1. Ship this plan's Redis-backed controls, cluster-admission lease, leader election, and durable queue, **fully behind the `multi_replica_enabled=false` default** (Definition of Done's first bullet).
2. Validate in a staging/canary environment with `multi_replica_enabled=true` and `replicaCount > 1`, running the full Verification suite against real multi-pod traffic, not just the automated test suite.
3. Only after step 2 passes does an operator flip the Helm `cluster.multiReplicaEnabled` value and raise `replicaCount` in production — and only up to `cluster.maxReplicas`, which the Postgres lease enforces independently of what Helm allows.
4. The original hard-coded `replicaCount==1` Helm guard (`_helpers.tpl:16-26`, pre-this-plan) must **never** be relaxed as a standalone change ahead of the Redis-backed controls landing — doing so would silently reintroduce every bug this plan exists to fix (per-pod rate limits, per-pod circuit breakers, per-pod concurrency ceilings) with no correctness signal until a production incident surfaces it.

---

## Open Product & Technical Decisions

1. **Leader-election primitive**: Postgres advisory lock (reuses existing, proven pattern; zero new infra) vs. Redis `SET NX PX` with renewal (consistent with the rest of this plan's Redis-centric design, but reintroduces the non-Redlock caveat for a role — leadership — where flapping is more costly than for a rate-limit counter). **Recommendation**: Postgres advisory lock for leader election specifically (lower risk, matches the already-audited-as-correct admin-idempotency pattern at `src/infra/repositories/admin.rs:567`); Redis for rate/concurrency/circuit state (higher throughput, ok to be slightly lossy). Needs explicit confirmation before Wave 1 starts.
2. **Redis unreachable at startup with `multi_replica_enabled=true`**: fail process startup (safest, matches the cluster-lease fail-closed posture) vs. degrade to in-memory-only with a loud warning (more available, but silently reintroduces the exact per-process-state bug this iteration exists to fix). **Recommendation**: fail startup — a multi-replica deployment silently running per-pod-local state is worse than a clear crash-loop.
3. **`cluster.maxReplicas` default and who sets it**: this is a capacity-planning/product decision (how many replicas does the first production multi-replica deployment actually need?), not purely technical — flag for product input before the Helm chart ships a non-1 default anywhere.
4. Exact bootstrap file path is **not** guessed in this plan — Wave 0 must confirm it against the live repo before implementation (see Multi-Agent Workflow, Wave 0). Migration numbering is now **resolved, not open**: `0009` is verified free (highest existing is `0008_response_updated_at.sql`); Wave 0 only needs to re-check that no concurrently-landing plan has taken it.
5. **`readyz` lease reporting is now decided, not open** (see § API & OpenAPI changes): it is **required**, returning `503` + `moira.error.cluster_lease_denied` when the lease is not held. Earlier drafts listed this as an open nice-to-have; the CONVENTIONS re-audit closed it because mid-run lease loss otherwise leaves P3-2 fixed only at startup.
6. **Rate-limit fail-closed semantics under Redis loss** are decided (§ Verification's decision table) but have a product consequence worth confirming: a Redis outage will return `429` to *all* callers rather than degrading to per-pod limits. That is the correct correctness posture, but it converts a Redis outage into a full traffic outage. Confirm with product/ops whether a bounded, explicitly-logged, time-limited grace window (e.g. fail-open for the first N seconds of Redis unavailability, then fail closed) is wanted instead. Do **not** implement a grace window silently — if it is chosen, it must be a named config field with its own named test.

### Re-audit corrections applied (verified against source at audit commit)

These were wrong or under-specified in earlier drafts of this plan and have been corrected in place:

- **`src/orchestration/controls.rs` already has tests.** An earlier draft claimed "controls.rs's absence of inline tests today" to justify test placement. Verified false: a `#[cfg(test)] mod tests` block exists at **lines 683-942**. New unit tests extend it.
- **`src/infra/metrics.rs:19-91` was too coarse.** The `AtomicU64` counters are at **19-27** only; `:30-41` is the plain-`u64` snapshot struct and `:43-92` the `record_*` methods. Each new counter touches all three.
- **`src/infra/redis.rs` `redis_is_optional_by_default`** spans **83-90**, not 83-89.
- **`charts/moira/values.yaml` has no `redis:` or `cluster:` top-level block** — Redis exists only as three env strings under `config:` at `:61-63`, with no `MOIRA_REDIS__URL` key at all.
- **CI already provisions both backing services** (`.github/workflows/ci.yml:13-25` pgvector/pg16, `:26-34` redis:7-alpine, env at `:39-40`) — but **zero tests use Redis today**, so harness work in `tests/support/mod.rs` is a prerequisite deliverable, not a given.
- **Two pre-existing i18n violations sit on this plan's exact code paths** (`moira.error.capacity_exhausted`, `moira.error.idempotency_in_progress` — both emitted, neither catalogued). This plan closes both; see § i18n catalog additions.
- **Confirmed still accurate:** `publish_runtime_invalidation` (`src/infra/redis.rs:61-72`) has **zero callers** in `src/`; `_helpers.tpl:16-26` is template-time-only; `src/infra/db.rs:43-80` LISTEN/NOTIFY is intact and must stay byte-for-byte untouched; `claim_idempotency` at `src/infra/repositories/admin.rs:559-634` with `pg_try_advisory_xact_lock` at `:567`.
