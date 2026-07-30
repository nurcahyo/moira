-- Plan 10 wave 1 — cluster admission leases (finding P3-2).
--
-- `charts/moira/templates/_helpers.tpl`'s `moira.validateDeployment` is a Helm
-- *template-time* `fail`. It runs during `helm template`/`install`/`upgrade` and
-- nothing else, so `kubectl scale deployment/moira --replicas=3` walks straight
-- past it: the chart is never re-rendered. This table is the ceiling that cannot
-- be walked past, because every replica has to write a row here before it binds
-- a listener.
--
-- One row per live replica. A replica inserts on startup, renews `heartbeat_at`
-- on a timer, and stamps `released_at` on graceful shutdown. A replica that dies
-- without releasing is reclaimed by the next acquire, which stamps `released_at`
-- on any row whose heartbeat is older than the configured expiry.
--
-- # No `version` column, no `moira_bump_resource_version()` trigger
--
-- Deliberate. That trigger (`migrations/0004_admin_api_contract.sql`) exists to
-- give optimistic-concurrency resources an `If-Match` ETag. A lease is not one:
-- it has no HTTP surface at all, it is never edited by a caller, and its only
-- writer is the process that owns it. Adding a version column would mint an
-- ETag nobody can read and a trigger firing on every heartbeat — one write per
-- replica per ten seconds, forever.
--
-- # The table is not admin-visible
--
-- No route reads or writes it. It is a process-startup gate, which is precisely
-- why it adds no authenticated attack surface.

create table if not exists cluster_replica_leases (
    replica_id uuid primary key default gen_random_uuid(),
    -- The Kubernetes pod name, from the downward API (`POD_NAME` in
    -- `charts/moira/templates/deployment.yaml`). Outside Kubernetes the process
    -- falls back to `HOSTNAME` and then to a synthesised `unnamed-<replica>`
    -- label — see `resolve_pod_name` in `src/infra/repositories/cluster.rs`.
    -- The check exists because the fallback chain must never be allowed to
    -- degrade into writing `''` and leaving an operator with a table of
    -- anonymous rows during an incident.
    pod_name text not null check (length(btrim(pod_name)) > 0),
    acquired_at timestamptz not null default now(),
    heartbeat_at timestamptz not null default now(),
    released_at timestamptz
);

-- The admission query counts live leases (`released_at is null`) and the
-- reclaim step rewrites the stale ones by `heartbeat_at`. Partial on
-- `released_at is null` so the index stays the size of the cluster rather than
-- the size of its restart history.
create index if not exists cluster_replica_leases_heartbeat_idx
    on cluster_replica_leases (heartbeat_at)
    where released_at is null;

-- Ancient released rows are pruned by age during acquire; this index makes that
-- prune an index scan instead of a sequential one once the table has accumulated
-- a deployment's worth of restarts.
create index if not exists cluster_replica_leases_released_idx
    on cluster_replica_leases (released_at)
    where released_at is not null;
