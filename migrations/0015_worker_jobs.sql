-- Plan 10 wave 2 — the durable worker queue (finding P3-5).
--
-- Before this table the supervisor loop had two arms: a metrics tick and a
-- retention sweep. Background work that failed was gone — no retry, no
-- dead-letter, no record it had ever been asked for. This is where it lives now.
--
-- # No `moira_runtime_config` NOTIFY trigger, deliberately
--
-- Every other table this plan touches is either config (triggered) or a lease
-- (not). This one is neither: its rows change on every claim, every failure and
-- every completion, so attaching the trigger would put one notification per job
-- transition onto the channel that invalidates every runtime cache in the
-- cluster. `circuit_reset_scope` (`src/infra/db.rs`) has never heard of
-- `worker_jobs`, so each of those notifications would fall into its unknown arm
-- and reset **every provider circuit breaker in every replica** — the exact
-- defect that function's `CIRCUIT_UNAFFECTED_RESOURCE_TYPES` list exists to
-- prevent. If a later change ever does attach the trigger here, it must add the
-- entry in the same change; the comment above that list says so in as many words.
--
-- # No `moira_bump_resource_version()` trigger either
--
-- Same reasoning as `cluster_replica_leases`: that trigger mints an `If-Match`
-- ETag for optimistically-concurrent HTTP resources. A job is not one. It has no
-- route, and its only concurrent writers are claim statements already serialised
-- by `for update skip locked`.

create table if not exists worker_jobs (
    id uuid primary key default gen_random_uuid(),
    -- Matches a `WorkerSpec.name` from `src/infra/workers.rs`. Not a foreign key
    -- — the spec table is code, not data — but `WORKER_JOB_NAMES` is a closed set
    -- and `WorkerQueue::enqueue` refuses anything outside it, so a job nobody can
    -- dispatch cannot be written in the first place.
    job_name text not null check (length(btrim(job_name)) > 0),
    payload jsonb not null default '{}'::jsonb,
    -- Four states, not the six an earlier draft listed.
    --
    -- `claimed` is absent because it would always be entered and left by the same
    -- statement as `running` — two names for one state is a lie the schema would
    -- then have to be queried around.
    --
    -- `failed` is absent because a failed attempt with retries left is not a
    -- terminal state: the row goes back to `pending` with a future `run_at`,
    -- `attempts` incremented and `last_error` recorded. That *is* the retry, and
    -- keeping it in `pending` means the claim query needs one predicate rather
    -- than two. A failure with no retries left goes to `dead_letter`.
    status text not null default 'pending'
        check (status in ('pending', 'running', 'completed', 'dead_letter')),
    -- When the job becomes claimable. Backoff is expressed by pushing this
    -- forward, which is why a retrying job stays `pending` and is still invisible
    -- to the claim query until its delay elapses.
    run_at timestamptz not null default now(),
    attempts integer not null default 0 check (attempts >= 0),
    max_attempts integer not null default 5 check (max_attempts > 0),
    -- The `cluster_replica_leases.replica_id` of the claimer, when there is one.
    -- Not a foreign key: a lease row is pruned seven days after release, and a
    -- completed job should not be deleted along with it.
    claimed_by uuid,
    claimed_at timestamptz,
    last_error text,
    dead_letter_at timestamptz,
    created_at timestamptz not null default now(),
    completed_at timestamptz
);

-- The claim query orders by `run_at` over `status = 'pending'`. Partial, so the
-- index is the size of the *backlog* rather than of every job the deployment has
-- ever run — which is the difference between an index that stays in cache and one
-- that does not.
create index if not exists worker_jobs_claim_idx
    on worker_jobs (run_at)
    where status = 'pending';

-- Reclaiming jobs whose claimer died scans by `claimed_at` over `running` rows
-- only. That set is bounded by the cluster's in-flight concurrency, so this index
-- is tiny and the sweep is cheap enough to run on every poll.
create index if not exists worker_jobs_stale_idx
    on worker_jobs (claimed_at)
    where status = 'running';

-- Dead letters are pruned by age (`workers.dead_letter_retention_hours`).
create index if not exists worker_jobs_dead_letter_idx
    on worker_jobs (dead_letter_at)
    where status = 'dead_letter';

-- Completed rows are pruned by age too, and by the same sweep.
create index if not exists worker_jobs_completed_idx
    on worker_jobs (completed_at)
    where status = 'completed';
