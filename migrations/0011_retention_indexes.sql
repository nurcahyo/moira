-- Plan 04 (finding P1-5): the retention/cleanup worker
-- (`src/infra/workers/retention.rs`) sweeps expired rows in bounded batches with
--
--     delete from t where id in (
--       select id from t where expires_at < now()
--       order by expires_at limit $1 for update skip locked)
--
-- Without an index on `expires_at` that sub-select is a sequential scan plus a
-- sort of the whole table on every batch -- which gets worse exactly as the table
-- gets big enough to need pruning, and would make the sweep itself the thing that
-- degrades the database. These indexes turn each batch into a bounded index scan.
--
-- Both are additive and idempotent. On a large live table, consider issuing the
-- equivalent `create index concurrently` out of band before deploying; sqlx runs
-- migrations inside a transaction, where `concurrently` is not permitted, so this
-- file uses the plain form.

-- `idempotency_records.expires_at` is `not null`, so a plain index covers every
-- row and there is nothing to filter on.
create index if not exists idempotency_records_expires_at_idx
    on idempotency_records (expires_at);

-- `responses.expires_at` is nullable and is NULL for every response whose
-- application sets no retention policy -- typically the majority. A partial index
-- keeps those rows out of it entirely; the sweep's predicate states
-- `expires_at is not null and expires_at < now()` so it matches this index
-- without depending on the planner proving the implication itself.
create index if not exists responses_expires_at_idx
    on responses (expires_at)
    where expires_at is not null;

-- Deleting a `responses` row is not a lone row delete. Three tables reference it
-- with `on delete set null`, so every deleted response fires a referential-
-- integrity trigger that must FIND the referencing rows in each child table:
--
--   conversation_messages.response_id       -- indexed already
--     (conversation_messages_response_idx, migration 0007)
--   memory_records.source_response_id       -- NOT indexed before this migration
--   memory_extraction_runs.response_id      -- NOT indexed before this migration
--
-- An unindexed child column makes that a sequential scan of the child table PER
-- DELETED PARENT ROW. Measured on PostgreSQL 18.3 with 200k `responses` and 200k
-- `memory_records`, one 500-row retention batch:
--
--   without these indexes: 12,735 ms  (memory_records RI trigger alone: 12,689 ms)
--   with these indexes:        55 ms  (memory_records RI trigger alone:     48 ms)
--
-- The `expires_at` indexes above are not enough on their own: they make choosing
-- the victims fast while the cascade stays quadratic. A 12-second "short batch"
-- holds row locks for 12 seconds, which is precisely the behaviour the batching
-- and `skip locked` exist to prevent -- so these two indexes are part of the
-- retention feature, not an unrelated optimisation.
--
-- Partial (`where ... is not null`) because both columns are NULL for the large
-- majority of rows, and the RI trigger never probes for NULL.
create index if not exists memory_records_source_response_idx
    on memory_records (source_response_id)
    where source_response_id is not null;

create index if not exists memory_extraction_runs_response_idx
    on memory_extraction_runs (response_id)
    where response_id is not null;
