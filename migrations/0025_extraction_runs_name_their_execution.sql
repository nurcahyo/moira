-- F54 — a failed extraction run could not be correlated to the execution that failed.
--
-- `memory_extraction_runs` already records *what* went wrong: `failure_class` has been on the
-- table since 0007 and has carried the execution's own class since F29's third precondition.
-- What it could not answer was the operator's next question — *which provider, which model,
-- which attempts, what the provider actually said* — because the run row held no key to the
-- execution.
--
-- The only correlation that existed was a string convention: `run_extraction` sets
-- `request_id = format!("memory-extraction-{run_id}")` and the execution kernel writes that
-- into `audit_logs.request_id` and `execution_attempts.request_id`. Nothing enforced the
-- format, nothing tested it, and no document named it, so the join was
-- `like 'memory-extraction-%'` plus parsing a uuid out of a varchar.
--
-- # Why a bare `execution_id uuid` and not a foreign key
--
-- There is nothing to reference. `execution_id` is not the primary key of any table: it is
-- `unique` on `responses` and a plain indexed column on `execution_attempts`. A foreign key to
-- `responses(execution_id)` would also be wrong on its own terms — a `responses` row exists
-- only when the application's `persistence_mode` says so, while `execution_attempts` and
-- `audit_logs` are written regardless, so the FK would fail exactly on the deployments that
-- persist least.
--
-- `response_id` on this table is **not** that key and is not being repurposed. It references
-- the *triggering turn's* response — the conversation turn whose completion caused extraction
-- to run — which is a different execution from the extraction's own.
--
-- This is the shape the schema already uses for the same problem, twice, in the migration that
-- created this table: `context_plans.execution_id` and `retrieval_runs.execution_id` are both
-- bare uuids with an index and no FK (`0007…:448-449`, `0007…:467-486`). Extraction was the
-- odd one out.
--
-- # Nullable
--
-- Two reasons, and only the second is about history.
--
--   1. Rows written before this migration have no execution to name.
--   2. `insert_memory_extraction_run` opens the row *before* the completion call, deliberately,
--      so that a run which dies mid-call leaves a `'running'` row rather than no row. The id is
--      minted by the caller and written on that insert, so a mid-call death keeps its
--      correlation — but the earliest failure paths in `extract_memories` return before any
--      execution is described, and those rows must be able to say so.
--
-- # Reversal
--
-- If executions ever get a table of their own with `execution_id` as its primary key, this
-- column should become a real foreign key to it in the same change. Until then the barrier is
-- the type on `MemoryExtractionRunInsert`, which takes a required `Uuid`: the writer cannot
-- open a run row without naming the execution it is about to run.

alter table memory_extraction_runs
    add column if not exists execution_id uuid;

create index if not exists memory_extraction_runs_execution_idx
    on memory_extraction_runs (execution_id);
