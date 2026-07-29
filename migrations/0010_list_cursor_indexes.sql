-- Plan 04 (P1-4) — indexes required by real keyset pagination.
--
-- Shared file: one section per Wave-1 specialist. APPEND a new section; never rewrite
-- another specialist's section.
--
-- Scope rule for this file: an index lands here only when `EXPLAIN (ANALYZE, BUFFERS)`
-- against a seeded database showed the planner sorting (or filtering the keyset
-- predicate after the fact) *without* it, and not sorting *with* it. Most admin list
-- queries already have an exact-match cursor index from
-- `0004_admin_api_contract.sql:51-106`; those are deliberately absent here.

-- ---------------------------------------------------------------------------
-- Specialist A — admin lists (`src/infra/repositories/admin.rs`)
-- ---------------------------------------------------------------------------
--
-- Seeded 50k rows/table (100k + a 20k-row occurred_at tie group for audit_logs),
-- PostgreSQL 18.3, `limit 51` (limit 50 + the over-fetched row), cursor placed at the
-- midpoint of each table. Six of the nine admin lists needed nothing — applications,
-- providers, provider_models, provider_credentials (unfiltered), system_api_keys and
-- trusted_jwt_issuers already resolve to a plain `Index Scan` on their
-- `0004_admin_api_contract.sql` cursor index. The three below did not.

-- `list_consumer_keys` orders by `created_at desc, id desc` with no application filter,
-- but the only cursor index on this table (`consumer_api_keys_cursor_idx`) leads with
-- `application_id`, so it cannot supply that ordering.
--   before: Seq Scan + top-N heapsort, 25k rows scanned, 961 buffers, 47.1 ms
--   after:  Index Scan, 9 buffers, 0.036 ms
-- The un-cursored first page benefits identically (132.8 ms -> 0.017 ms), so this also
-- fixes a pre-existing slow path rather than only the new one.
create index if not exists consumer_api_keys_created_cursor_idx
    on consumer_api_keys (created_at desc, id desc)
    where deleted_at is null;

-- `list_user_credentials` filters on `external_user_id`, which is not the leading column
-- of any existing index. The planner fell back to `provider_credentials_resolution_idx`
-- and applied the keyset predicate as a post-fetch `Filter` — the failure mode that
-- degrades keyset pagination back into offset pagination as pages get deeper.
--   before: Bitmap Index Scan on the resolution index, keyset applied as Filter
--           (26 rows removed), 153 buffers, 24.8 ms
--   after:  Bitmap Index Scan on this index with the keyset as an Index Cond,
--           27 buffers, 0.058 ms
-- A small Sort node survives because one user owns few enough rows that the planner
-- prefers a bitmap scan; the win here is pushing the cursor predicate into the index,
-- not removing that sort.
create index if not exists provider_credentials_user_cursor_idx
    on provider_credentials (external_user_id, created_at desc, id desc)
    where deleted_at is null;

-- `list_audit_logs` orders by `occurred_at desc, id desc`. `audit_logs_occurred_at_idx`
-- covers only `occurred_at`, so the new `id` tiebreaker forces an Incremental Sort and
-- leaves the keyset predicate as a `Filter`.
--
-- This matters far more than the uniform-timestamp case suggests: audit rows are written
-- inside a transaction and `now()` is the *transaction* timestamp, so every audit row a
-- batch writes shares one `occurred_at` exactly. Measured against a 20k-row tie group,
-- paging into the middle of it:
--   before: Index Scan on audit_logs_occurred_at_idx + Incremental Sort, 10,000 rows
--           read and 10,001 discarded by Filter, 286 buffers, 2.61 ms
--   after:  Index Scan, keyset fully an Index Cond, 54 buffers, 0.035 ms
-- (With small tie groups the planner keeps the narrower index and the difference is
-- negligible; the tie-group case is the one that decides it. No partial predicate:
-- audit_logs has no `deleted_at`.)
create index if not exists audit_logs_occurred_at_cursor_idx
    on audit_logs (occurred_at desc, id desc);

-- ---------------------------------------------------------------------------
-- Specialist B — runtime-admin lists (`src/infra/repositories/runtime.rs`)
-- ---------------------------------------------------------------------------
--
-- `list_route_definitions`, `list_routing_policies`, `list_agent_profiles` all order by
-- `created_at desc, id desc` with no other filter, and `0005_provider_runtime.sql` already
-- ships an exact-match cursor index per table (`route_definitions_cursor_idx`,
-- `routing_policies_cursor_idx`, `agent_profiles_cursor_idx`, all `(created_at desc, id
-- desc) where deleted_at is null`) — the same shape the plan describes needing to add.
--
-- Verified against `moira_test` (PostgreSQL 18.3) with the seed data already present
-- (route_definitions: 5,422 rows; routing_policies: 1,150 rows), cursor placed mid-table,
-- `limit 51` (limit 50 + the over-fetched row), using the exact keyset predicate this plan
-- adds (`$1::timestamptz is null or (created_at, id) < ($1::timestamptz, $2::uuid)`):
--   route_definitions: Index Scan using route_definitions_cursor_idx, Index Cond carries
--       the full keyset predicate, no Sort node, 5 buffers, 0.369 ms.
--   routing_policies:  Index Only Scan using routing_policies_cursor_idx, same shape,
--       5 buffers, 0.043 ms.
-- The un-cursored first page (both cursor parameters bound `NULL`, exercising the
-- `is null` branch) resolves to the same `Index Scan`/`Index Only Scan` with no predicate
-- evaluated, confirming the `NULL`-short-circuit shape used by every list in this plan does
-- not defeat the index.
-- `agent_profiles` shares byte-identical index shape and query shape (empty table in this
-- environment; the plan is structurally the same as `route_definitions`, which is nonempty
-- and was measured directly).
--
-- No new index added. `deleted_at is null` is redundant with the partial index predicate,
-- not a reason to widen it.

-- ---------------------------------------------------------------------------
-- Specialist C — conversation-domain lists (`src/infra/repositories/conversation.rs`)
-- ---------------------------------------------------------------------------
--
-- Method: an isolated database seeded with 60k rows per table (60k conversations and 60k
-- memories across 2 applications x 4 tenants x 200 users; 60k RAG collections; one
-- collection holding 60k documents each with a version row; one conversation holding 60k
-- messages), PostgreSQL 18.3, `limit 51` (limit 50 + the over-fetched row), cursor placed
-- mid-result, running the exact `EXPLAIN (ANALYZE, BUFFERS)` shape this plan ships.
-- `updated_at` was deliberately truncated to the minute so ~600 rows share each value and
-- the `id` tiebreaker is exercised rather than being decorative.
--
-- Every one of the five lists started out as a Seq Scan + top-N heapsort with the keyset
-- predicate applied as a post-fetch `Filter` — the degradation that turns keyset pagination
-- back into offset pagination, getting slower the deeper the caller pages.
--
-- Why the indexes already in `0007_conversations_memory_rag.sql` did not help: three of them
-- (`conversations_owner_cursor_idx`, `memory_records_scope_cursor_idx`,
-- `rag_collections_visible_idx`) index `coalesce(external_tenant_id, '')` and
-- `coalesce(external_user_id, '')`, while these queries filter the bare columns
-- (`($n::text is null or external_tenant_id = $n)`). An expression index cannot serve a
-- plain-column predicate, so they were unusable here. Matching the queries to those
-- expressions instead was rejected: `coalesce(col,'') = coalesce($n,'')` would make a NULL
-- parameter mean "match rows whose value is NULL" rather than "do not filter", which is a
-- different query.
--
-- `conversation_messages` needs NO new index — see the note at the end.

-- `list_conversations_authorized`, scoped caller (the ConsumerKey/TrustedJwt path, where
-- `conversation_access` binds application + tenant + user).
--   before: Seq Scan, 59,842 rows discarded by Filter, top-N heapsort, 1,462 buffers, 8.4 ms
--   after:  Index Scan, full keyset as an Index Cond, no Sort, 54 buffers, 0.12 ms
-- Re-measured with this index dropped but the one below present, to confirm it is not
-- redundant: 121.2 ms and back to a Seq Scan. A selective owner (159 of 60,001 rows) cannot
-- be served by a timestamp-leading index.
create index if not exists conversations_owner_keyset_idx
    on conversations (
        application_id,
        external_tenant_id,
        external_user_id,
        updated_at desc,
        id desc
    )
    where deleted_at is null;

-- `list_conversations_authorized`, privileged caller (SystemKey/DevAdmin listing every
-- conversation), where the whole owner predicate folds away and the index above has no
-- leading column to anchor on.
--   before: Seq Scan, 30,001 rows discarded by Filter, top-N heapsort, 1,462 buffers, 11.2 ms
--   after:  Index Scan, keyset as an Index Cond, no Sort, 31 buffers, 0.09 ms
-- Also picked up by the application-scoped-but-not-user-scoped shape (0.07 ms).
create index if not exists conversations_updated_cursor_idx
    on conversations (updated_at desc, id desc)
    where deleted_at is null;

-- `list_memories_authorized` — identical query shape and identical result to the two above.
--   scoped:     8.5 ms / 2,069 buffers  ->  0.13 ms / 54 buffers
--   privileged: Seq Scan + heapsort     ->  0.08 ms / 40 buffers
create index if not exists memory_records_owner_keyset_idx
    on memory_records (
        application_id,
        external_tenant_id,
        external_user_id,
        updated_at desc,
        id desc
    )
    where deleted_at is null;

create index if not exists memory_records_updated_cursor_idx
    on memory_records (updated_at desc, id desc)
    where deleted_at is null;

-- `list_rag_collections`. One index covers both shapes, so only one is added: the
-- application/tenant filters are optional and, when present, are cheap enough to apply as a
-- Filter on top of an ordered index walk.
--   unfiltered:      14.8 ms / 1,500 buffers  ->  0.06 ms / 28 buffers
--   app+tenant:      12.1 ms / 1,500 buffers  ->  0.09 ms / 107 buffers
-- An additional `(application_id, external_tenant_id, created_at desc, id desc)` index was
-- built and measured: the planner did not choose it and the filtered case did not improve
-- (0.086 ms either way). Deliberately not added.
create index if not exists rag_collections_created_cursor_idx
    on rag_collections (created_at desc, id desc)
    where deleted_at is null;

-- `list_rag_documents`. `rag_documents_collection_cursor_idx` (0007) is
-- `(collection_id, status, created_at desc, id desc)`; this query does not constrain
-- `status`, so that index cannot supply the `created_at` ordering. This one removes the gap.
--
-- This index only pays off together with the scalar-subquery rewrite of the query (see
-- `LIST_RAG_DOCUMENTS_SQL` in `src/infra/repositories/conversation.rs`) — with the original
-- join form the planner cannot treat `collection_id` as a constant and ignores the index
-- entirely (measured: 31.4 ms, still a Seq Scan). With both:
--   before: Seq Scan + top-N heapsort, 2,900 buffers, 21.4 ms
--   after:  Index Scan, keyset as an Index Cond, no Sort, 36 buffers, 0.07 ms
create index if not exists rag_documents_collection_created_cursor_idx
    on rag_documents (collection_id, created_at desc, id desc)
    where deleted_at is null;

-- `list_messages`: NO index added, deliberately.
--
-- `conversation_messages_sequence_unique (conversation_id, sequence_number)` already has the
-- exact shape this query needs; the query simply could not reach it while `conversation_id`
-- came from a join. Rewriting the join as a scalar subquery (see `LIST_MESSAGES_SQL`) turns
-- it into an InitPlan constant and the existing index does the rest:
--   before: Hash Join + Seq Scan, 30,000 rows discarded by Filter, top-N heapsort,
--           1,468 buffers, 28.5 ms
--   after:  InitPlan + Index Scan, whole predicate as an Index Cond, no Sort,
--           10 buffers, 0.10 ms
-- Adding an index here would have hidden a query bug behind write amplification.
