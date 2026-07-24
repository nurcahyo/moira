# Iteration 04 — Durability & Correctness

Companion to `00-audit-report.md` and `01-roadmap-and-dependencies.md`. MVP gate (P1). Depends on iteration 03 (security/middleware primitives are reused for the deadline/timeout work); gates iteration 10 (multi-replica).

---

## Summary

**Objective.** Close five correctness/durability gaps that are load-bearing for a "controlled MVP" claim: admin/runtime/conversation list endpoints silently ignore the `cursor` parameter they advertise; nothing ever deletes expired idempotency records or expired response rows; the execution deadline only bounds the provider call, not credential resolution/runtime construction/persistence; the client-stall cancellation path has no DB-backed proof it actually releases resources; and `PUT .../execution-policy` is the one versioned mutation where `If-Match` is optional, making it the one place concurrent writers can silently clobber each other.

**Why ordered here.** These are pure correctness/durability fixes with no product-facing behavior change (pagination becomes real instead of fake; storage stops growing unbounded; a timeout ceiling is honored; a known-safe code path gets a test; a header becomes required like all its siblings). They depend on 03's middleware/error conventions (timeout composition, error envelope) but have no dependency on the honesty relabeling in 02 beyond sharing the same repositories. They must land before 05's OpenAPI-diff gate freezes the spec, since P1-8 changes the `If-Match` parameter from optional to required for one endpoint (a documented breaking change we want captured once, not twice).

**User-visible outcome.** Admin/runtime/conversation list endpoints page through results deterministically via an opaque cursor; storage for idempotency records and expired responses stays bounded; a request that hits its total deadline fails cleanly no matter which phase is slow; a stalled (non-disconnecting) SSE client cannot leak a permit or leave a response stuck `in_progress`; concurrent `PUT execution-policy` calls get a `409` instead of a silent lost update.

**Included scope.** P1-4 (real keyset pagination), P1-5 (retention/cleanup worker), P1-6 (deadline covers all execution phases), P1-7 (DB-backed stalled-reader test), P1-8 (require `If-Match` on execution-policy PUT).

**Excluded scope.** Idempotency replay for conversation/memory/RAG routes (P0-2, handled structurally in plan 02; full implementation is an **optional extension** noted at the end of this plan, not required for this iteration). Leader-election-gated retention (plan 10 — this iteration ships a single-replica plain-tokio-task version only, matching the current single-replica constraint). `AdminService` decomposition (plan 06). Public API (`/v1/executions`, `/v1/usage`) cursor pagination — same silent-cursor bug, different query types (`ExecutionQuery` at `src/domain/public.rs:370`, `UsageQuery` at `:348`; tracked separately at `docs/todo.md:53`), called out as an optional extension below since it reuses the codec this plan builds but touches `src/application/public.rs` / `src/infra/repositories/public.rs`, which plan 03/05 specialists may also be editing.

---

## Branch & Pull Request

Binding: `plans/CONVENTIONS.md` §1. Where anything below this section conflicts with `CONVENTIONS.md`, `CONVENTIONS.md` wins.

- **Branch:** `plan/04-durability-correctness`. Cut from the **current `main`**, not from another plan's branch. If the roadmap in `01-roadmap-and-dependencies.md` forces stacking on `plan/03-security-hardening` (03 owns the middleware/error primitives Module 6 composes with), the PR description must name the base PR explicitly and this branch must be rebased once that base merges. Never force-push once another plan branch is stacked on this one.
- **Commits:** Conventional Commits, matching existing history style (`feat: make admin commands atomic`, `test: align idempotency harness with replay envelope`). Suggested split, one commit per module: `feat: add opaque list cursor codec`, `feat: implement keyset pagination for admin/runtime/conversation lists`, `feat: add retention cleanup worker`, `feat: bound execution deadline across all phases`, `test: prove stalled SSE reader releases permits`, `feat!: require If-Match on execution-policy PUT`, `feat: add invalid_cursor and if_match_required i18n entries`, `docs: reconcile todo.md with landed durability work`.
- **PR is not opened until every gate in `CONVENTIONS.md` §2 passes locally** — see the Verification section's "Required Rust gates" block plus clean-database migration validation.
- **PR description — required sections** (§1.4), all mandatory:
  - **Plan link** — `plans/04-durability-correctness.md`.
  - **Findings addressed** — P1-4, P1-5, P1-6, P1-7, P1-8 (from `plans/00-audit-report.md:90,96,102,108,114`).
  - **Migrations included** — `migrations/0009_list_cursor_indexes.sql` (may be empty/omitted if `EXPLAIN` shows no index is needed — say which), `migrations/0010_retention_indexes.sql`.
  - **Breaking API/OpenAPI changes** — **YES.** `PUT /api/v1/admin/applications/{id}/execution-policy` changes its `If-Match` header parameter from `Option<i64>` (optional) to `i64` (**required**) at `src/http/admin.rs:319`; callers omitting the header now receive `400` instead of a silent write. Additionally, if the dedicated `if_match_required` error code (see the i18n Contract section) is adopted, the `error.code` string on the missing-header path changes from `bad_request` to `if_match_required` on **every** versioned mutation that calls `require_if_match` (`src/http/admin.rs:66-77`, used at `:195,224,253,284,439,468,497` and siblings) — an observable error-code change with no response-shape change; state which choice was made and enumerate the affected endpoints.
  - **Test evidence** — unit-test output summary (`cargo test --lib`) plus e2e output summary from the DB-backed suites named in Verification, run against real PostgreSQL 16 + pgvector.
  - **Rollback procedure** — see Risks & Rollback below; summarize it in the PR, don't just link.
  - **Deferred follow-ups** — leader-election gating (plan 10), public-API cursor pagination, conversation/memory/RAG idempotency replay.
- **Ordering (hard requirement, `CONVENTIONS.md` §1.6):** this plan carries a **BREAKING OpenAPI change** (P1-8) and therefore **must be merged into `main` before plan 05 (`plan/05-observability-ci-gates`) regenerates and commits `docs/openapi.json` and lands its OpenAPI-drift gate.** If plan 05's gate freezes the spec first, it freezes the pre-P1-8 contract and this plan's change has to be re-litigated through a red gate. Plan 05's own Branch & Pull Request section carries the mirror-image statement; if the two ever disagree, this ordering rule (from `CONVENTIONS.md` §1.6) is authoritative. Practical check before opening this PR: confirm `docs/openapi.json` does not yet exist on `main`, or that plan 05's PR is still open and its author has been told to regenerate after this merges.
- **Done means merged** (§1.5): this plan is **not** done when the PR opens. It is done when the PR is merged with all gates green and every Definition of Done box objectively verified by a **named, passing test** (§3 "Definition of Done addition") — "implemented" is not "done."

---

## Findings Addressed

### P1-4 — `cursor` param accepted but silently ignored
- `src/domain/admin.rs:8-17` `ListResponse<T>`/`Pagination{next_cursor,has_more}` and `:31-61` `PageQuery` (`cursor: Option<String>` at `:36`) are defined and part of the OpenAPI contract (`src/http/admin.rs:128` `params(PageQuery)`).
- `src/http/admin.rs:136-146` `list_applications` calls `AdminService::list_applications(&actor, query.limit())` — the `cursor` field of `query` is never read.
- `src/application/admin.rs:93-103` `list_applications` forwards only `limit` to the repository.
- `src/infra/repositories/admin.rs:754-769` `list_applications` SQL is `... where deleted_at is null order by created_at desc limit $1` — no cursor predicate, no `id` tiebreaker (ties on `created_at` are not deterministically ordered).
- The same shape (`ListResponse<T>` return, `PageQuery`/no-cursor SQL) repeats for **9** `AdminService` list methods (`src/application/admin.rs:93,253,390,546,558,871,974,1161,1283`), **3** `RuntimeAdminService` list methods (`src/application/runtime_admin.rs:72,213,386`, backed by concrete repos in `src/infra/repositories/runtime.rs`, which already use `order by created_at desc, id desc` at lines 120, 280, 457 — the tiebreaker exists but the cursor predicate does not), and **5** `ConversationService` list methods (`src/application/conversation.rs:119,238,481,776,900`, backed by `src/infra/repositories/conversation.rs`). All 17 share `PageQuery`/`ListResponse`/`Pagination` from `src/domain/admin.rs`.
- **Sort keys are not uniform** — the cursor design must accommodate this, verified per query:
  - Admin (`src/infra/repositories/admin.rs`): 8 of 9 order by `created_at desc` with **no** `id` tiebreaker (`:761,882,1009,1142,1153,1569,1926,1937`); `list_audit_logs` orders by **`occurred_at desc`** (`:1705`), also without a tiebreaker.
  - Conversation (`src/infra/repositories/conversation.rs`): tiebreakers mostly already exist but keys differ — conversations `order by c.updated_at desc, c.id desc` (`:538`), memories `order by m.updated_at desc, m.id desc` (`:811`), rag_collections `order by created_at desc, id desc` (`:921`), rag_documents `order by d.created_at desc, d.id desc` (`:1096`), and messages `order by m.sequence_number asc` (`:713`) — a monotonic per-conversation integer, ascending, needing an integer cursor rather than a `(timestamp, id)` pair.
- `docs/todo.md:20` (Phase 2): "Replace simplified list pagination with real opaque cursor pagination using stable `created_at DESC, id DESC` ordering and `has_more`/`next_cursor` calculation." — matches this finding (note the TODO's `created_at DESC` wording undersells the per-endpoint key variance above); rewrite to "done" once this lands.
- **Current behavior:** supplying `?cursor=...` has no effect; `next_cursor` is always `null`; `has_more` is always `false`; large tables are silently truncated at `limit` (default/max governed by each domain type's `limit()` helper, e.g. `src/domain/admin.rs:64`).

### P1-5 — No retention/cleanup for expired idempotency records & metadata-only responses
- `src/infra/workers.rs:81-85` registers a `WorkerSpec { name: "retention-cleanup", description: "Expires responses, idempotency records, vectors, and tombstones.", enabled_by_default: true }`, but `run_supervisor` (`:131-153`) only ticks a timer and calls `state.metrics.record_worker_tick()` — there is **no** job dispatch, no per-worker execution, no SQL anywhere in the crate that deletes from `idempotency_records` or expired `responses`.
- Target tables already exist with the right columns: `idempotency_records.expires_at timestamptz not null` (`migrations/0003_security_foundation.sql:347-361`, unique index on `(idempotency_key_hash, actor_fingerprint, operation)`); `responses.expires_at timestamptz` nullable (`migrations/0006_public_execution_api.sql:62`), populated per-response by `retention_expires_at(&prepared.policy)` (defined `src/application/public.rs:1845`, called at `:161,317`).
- The advisory-lock claim path (`claim_idempotency`, `src/infra/repositories/admin.rs:559-596`) already does an opportunistic single-key delete of expired idempotency rows *for the exact key being claimed* (`:583-596`) — that is not a substitute for bulk retention; unclaimed expired rows accumulate forever.
- `docs/todo.md:50` (Phase 4): "Add retention cleanup for expired `responses` and idempotency records." — matches exactly.
- **Impact:** unbounded growth of `idempotency_records` and expired `responses` rows — storage exhaustion and slow index scans over time; no metric exists to observe this.

### P1-6 — Execution deadline does not bound credential resolution, runtime construction, or terminal persistence
- `src/application/execution.rs:132` computes `execution_deadline = Instant::now() + Duration::from_millis(total_timeout_ms)`.
- `:327-336` (inside the retry loop) only *rejects a new attempt* once `remaining_execution_time(execution_deadline)` (`:1863-1867`) returns `None` — a check done once per loop iteration, not a bound on any single phase.
- `resolve_credential` (`:772-856`, called at `:249`) is a plain `.await` on `runtime_repo.resolve_runtime_credential(...)` — no `timeout`/`select!`.
- `runtime_handle` (`:857-...`, called at `:304-306`) is a plain `.await` on `runtime_handles.get_or_insert_with(...)` (cache miss triggers `factory.build_completion_model`, itself a plain await) — no timeout.
- Only the provider call itself is bounded: `:496-503` computes `attempt_timeout = remaining.min(Duration::from_millis(runtime_policy.timeout_ms))` and wraps `execution` in `tokio::time::timeout`.
- Terminal persistence after a successful call — `update_attempt` (`:509-522`), `insert_usage_record` (`:523-538`), `touch_credential_used` (`:539-541`) — are three sequential plain awaits with `?`, none timeout-wrapped; a DB stall here extends the request indefinitely with no deadline enforcement, though the client's own HTTP timeout may still fire.
- `docs/todo.md:39` (Phase 3): "Extend the total execution deadline across slow routing, credential resolution, runtime construction, and terminal persistence without abandoning active-attempt cleanup." — matches exactly.
- **Impact:** a slow credential decrypt (AES-256-GCM + DB round-trip), a cold runtime-handle build (Rig client construction), or a DB stall during terminal persistence can each extend total execution well past the caller's advertised `timeout_ms`, defeating the deadline contract silently.

### P1-7 — Streaming client-stall cancellation has no DB-backed integration test
- `src/application/public.rs:386-449` `supervise_public_stream` races `public_tx.closed()` (true disconnect, `:428`) against `handle.events.recv()`, and `send_public_event` (called at `:441`, body at `:1791-1802`) races `tx.closed()` against `tokio::time::timeout(send_timeout, tx.send(event))` — this is the code path that should protect against a client that keeps the TCP connection open but stops reading (backpressure on the bounded `mpsc::Sender`), as distinct from a real disconnect.
- A **unit-level** test of exactly this branch already exists: `stalled_public_consumer_hits_bounded_send_timeout` (`src/application/public.rs:2094`) asserts `send_public_event` returns `false` on a full `mpsc::channel(1)` after `send_timeout`. It proves the function's contract in isolation only — no DB, no permit, no attempt/response rows; the missing piece is strictly the end-to-end DB-backed proof below.
- `src/application/execution.rs:504` `drop(permits)` unconditionally releases the acquired concurrency permit(s) after the `tokio::select!` between cancellation and the provider timeout, regardless of which branch won.
- `tests/execution_lifecycle.rs:733-874` `public_sse_disconnect_persists_cancellation_and_reuses_capacity` **does** exist and is DB-backed (asserts on `responses`, `execution_attempts`, `conversation_messages` row states), but it exercises a **real disconnect**: the test `drop(body)`s the response stream at line 809, which closes the client's TCP socket, driving the `public_tx.closed()` branch. It does not exercise a client that keeps the connection open and simply stops consuming (`response.bytes_stream()` not polled/dropped, no socket close) — the scenario that would exercise the bounded `send_timeout` branch of `send_public_event` instead of `tx.closed()`.
- `docs/todo.md:46` (Phase 4): "Add a database-backed public SSE test for a connected client that stops reading without disconnecting, proving bounded send timeout releases permits and leaves no attempt `started` or response `in_progress`." — matches exactly, and is explicitly still open even though the disconnect test exists.
- **Impact:** unverified — a subtle interleaving between `send_timeout` firing and the DB update ordering in the cancellation path could theoretically leak a permit or leave a response stuck at `in_progress`; there is currently no test that would catch it.

### P1-8 — `If-Match` optional on application execution-policy PUT
- `src/http/admin.rs:317-320` declares `("If-Match" = Option<i64>, Header, description = "Optional current resource version")` for `put_application_execution_policy`, and `:338` calls `optional_if_match(&headers)?` (`:79-84`, returns `Ok(None)` when the header is absent) instead of `require_if_match(&headers)?` (`:66-77`, used by every other versioned mutation, e.g. `patch_application` at `:193-196`, `delete_application` at `:222-225`, `enable_application` at `:251-254`).
- Downstream, `PublicExecutionService::put_application_execution_policy` (`src/application/public.rs:817-856`, called from `src/http/admin.rs:338`) applies the version check only when `Some(expected)` — and even then as a **non-atomic** check-then-act: `get_or_create_application_execution_policy`, compare `current.version != expected` → `409 resource_version_conflict`, then a separate `put_application_execution_policy` repo call (`src/infra/repositories/public.rs:117`). So `None` skips the check entirely (this finding), and even `Some` has a TOCTOU window between compare and write.
- `docs/todo.md:21` (Phase 2): "Require `If-Match` consistently on every versioned mutation and upsert, including application execution policy PUT, and return `409 resource_version_conflict` for stale versions." — matches exactly.
- **Impact:** two concurrent `PUT .../execution-policy` calls can race with no conflict signal; the second write silently overwrites the first (lost update) — unlike every other admin mutation, which returns `409 resource_version_conflict` via `ensure_version` (`:86-95`).

### Re-audit corrections (2026-07-25, against `CONVENTIONS.md`)

All file:line citations in P1-4 … P1-8 above were re-verified against the working tree and **still hold**, with these confirmations and two new blocking discoveries:

- Confirmed: `src/http/admin.rs:319` is still `("If-Match" = Option<i64>, ...)` and `:338` still calls `optional_if_match(&headers)?`; every sibling (`:175,205,234,265,419,449,478,507`) declares `= i64` and calls `require_if_match` (`:195,224,253,284,439,468,497`). `require_if_match` at `:66-77` returns `AppError::BadRequest("If-Match header is required")` (`:69`) / `"If-Match header is invalid"` (`:71,76`); `optional_if_match` at `:79-84` delegates to `require_if_match` when the header is present (`:81`).
- Confirmed: `src/domain/admin.rs` `ListResponse`/`Pagination` (lines 7-17), `ListResponse::new` hardcoding `next_cursor: None, has_more: false` (`:19-28`), `PageQuery.cursor: Option<String>` (`:36`), `PageQuery::limit()` clamping to `1..=200` with a default of 50 (`:62-66`).
- Confirmed: `src/infra/workers.rs:81-85` registers the `retention-cleanup` `WorkerSpec` and `run_supervisor` (`:131-153`) still only ticks and calls `state.metrics.record_worker_tick()` (`:149`) — no job dispatch exists.
- Confirmed: `tests/support/mock_openai.rs` already provides `ScriptGate` (`:41-72`) and `ProviderScript::StalledStream { first_delta, gate }` (`:110-113`), and `tests/support/mod.rs:430-437` fails closed in CI (`panic!` when **`CI=true`** and `MOIRA_TEST_DATABASE_URL` is absent (value check per `CONVENTIONS.md` §3 — never `var_os("CI").is_some()`)) — Module 7's acknowledgement-gated test needs **no new harness primitive**, only a new test function. `tests/execution_lifecycle.rs` currently has 14 `#[tokio::test]` functions, with the disconnect test at `:733`.
- **NEW (blocking, i18n):** `grep "invalid_cursor"` over `src/`, `tests/`, and `docs/` returns **zero** matches — the `400 invalid_cursor` code this plan introduces is genuinely new and has no catalog entry. See the i18n Contract section.
- **NEW (blocking, i18n infrastructure):** **the entire `src/i18n/` tree is orphaned and never compiled.** `src/lib.rs:3-11` declares `app, application, config, domain, error, http, infra, orchestration, security` — there is **no `pub mod i18n;`**. The `mod i18n;` at `src/domain/mod.rs:3` resolves to `src/domain/i18n.rs` (the `ResponseText`/`ResponseTextArgs` DTO), **not** to `src/i18n/mod.rs`. Consequently `src/i18n/catalog/{mod,errors,notices}.rs` — the `RESPONSE_ERROR_CATALOG` (57 entries), `RESPONSE_NOTICE_CATALOG` (4 entries), `is_known_key`, `default_message_for_key`, and the `#[cfg(test)] mod tests` at `src/i18n/catalog/mod.rs:40-64` — are dead files that neither compile nor run. Adding a catalog entry today is a **no-op** and no test can assert its presence. `CONVENTIONS.md` §4's machinery therefore does not exist in the build yet, even though §4 describes it as "existing machinery (reuse it)". This plan must wire it (one line) before its own i18n test can exist — see the i18n Contract section. This is a discovery about the codebase, not a change to §4: the catalog files and their shapes are exactly as §4 describes, they are simply not reachable from `crate`.
- **NEW (minor, docs mirror):** `docs/i18n-response-catalog.json` contains 63 entries but only 61 unique keys — `moira.error.idempotency_conflict` and `moira.error.rate_limited` each appear twice. Key *coverage* matches the Rust catalog exactly (no missing/extra keys in either direction), so this is a duplication defect, not drift. Dedupe opportunistically when adding this plan's key; the systematic drift test is plan 06's job (P2-8).

---

## Architecture

### Components & ownership (per `docs/project-structure.md`)
- **Domain** (`src/domain/`): `PageQuery`/`ListResponse`/`Pagination` (admin.rs), `ExecutionQuery`/`UsageQuery` (public.rs, excluded from this iteration's core scope) stay as pure DTOs; add a new `src/domain/pagination.rs` module owning the opaque-cursor codec so it is reusable across admin/runtime/conversation repositories without creating a dependency from `domain` back into `infra`. Because verified sort keys vary per endpoint (`created_at`, `updated_at`, `occurred_at` — all `(timestamptz, id)` shaped — plus `sequence_number asc` for messages), the codec carries a generic key: `ListCursor { ts: DateTime<Utc>, id: Uuid }` for all timestamp-ordered lists, plus a `SeqCursor(i64)` variant (or a small enum over both) for `list_messages`.
- **Application** (`src/application/`): `AdminService`, `RuntimeAdminService`, `ConversationService` gain cursor-aware list method signatures; `MoiraExecutionService` (`execution.rs`) gains phase-level deadline wrapping; `PublicExecutionService`'s `put_application_execution_policy` keeps its current signature but the HTTP layer stops calling it with `None`.
- **Infra** (`src/infra/repositories/`): `admin.rs`, `runtime.rs`, `conversation.rs` repository methods gain a cursor predicate and `(created_at, id)` tiebreaker in SQL, and return one extra row (`limit + 1`) to compute `has_more` without a second `count(*)` query. `src/infra/workers.rs` gains the actual retention-cleanup job body, invoked from the existing `run_supervisor` tick loop (not a new supervisor).
- **HTTP** (`src/http/admin.rs`): `put_application_execution_policy`'s `#[utoipa::path]` params change from `Option<i64>` to `i64` (required), and the handler switches from `optional_if_match` to `require_if_match`.
- **Config** (`src/config/settings.rs`): extend `WorkerSettings` with retention-specific fields (batch size, tick multiplier / dedicated interval, TTL overrides if not already fully derived from `expires_at`).

### Data flow
- **Pagination:** client sends `?cursor=<opaque>&limit=N` → HTTP layer passes `PageQuery` through unchanged → application service decodes the cursor (via `src/domain/pagination.rs`) into that endpoint's sort key (`(ts, id)` for timestamp-ordered lists; a sequence number for messages), validates it (tamper/cross-endpoint rejection — see Interfaces & Contracts), passes it to the repository → repository SQL adds a keyset predicate against the query's **existing** sort key — e.g. `and (created_at, id) < ($1, $2)` for `created_at desc, id desc` lists, `(updated_at, id) < ...` for conversations/memories, `(occurred_at, id) < ...` for audit logs, `sequence_number > $1` for messages (asc) — fetches `limit + 1` rows → application service trims to `limit`, sets `has_more = fetched_count > limit`, encodes the last returned row's key as `next_cursor` when `has_more`. Do **not** change any endpoint's existing sort key to force uniformity — that would silently reorder results.
- **Retention:** the existing `WorkerSupervisor::run_supervisor` tick (`src/infra/workers.rs:131-153`) calls a new `retention::run_once(&state.pool, &settings.workers)` on a cadence independent of the base tick interval (e.g. every Nth tick, configurable) — executes bounded, batched `DELETE ... WHERE expires_at < now() LIMIT batch_size` (Postgres doesn't support `DELETE ... LIMIT` directly; use `DELETE FROM t WHERE id IN (SELECT id FROM t WHERE expires_at < now() LIMIT $1 FOR UPDATE SKIP LOCKED)`) against `idempotency_records` and `responses`, looping batches until a batch is empty or a per-tick cap is hit, then records deleted-row counts via `MetricsRegistry`.
- **Deadline:** `execute_inner`'s per-phase calls (`resolve_credential`, `runtime_handle`, terminal persistence writes) are each wrapped in `tokio::time::timeout(remaining_execution_time(execution_deadline)?, phase_future)`, mapping a timeout to the existing `deadline_failure()` (`:1869-1874`) and preserving the existing active-attempt cleanup (permit drop, attempt status update where already reachable) rather than abandoning it.
- **Stalled-reader test:** a new integration test constructs a client that opens the SSE connection, reads the first delta (proving the stream started), then **stops polling the body without closing the connection** (hold the `reqwest::Response` open, never call `.bytes_stream().next()` again) while the mock provider continues emitting deltas past the channel buffer, forcing `send_public_event`'s `tokio::time::timeout(send_timeout, tx.send(event))` branch to fire — asserted via an acknowledgement gate (`ScriptGate`, already used at `tests/execution_lifecycle.rs:737`) rather than a bare `sleep`.
- **If-Match:** unchanged transaction shape; only the header requirement and the `ensure_version` call become unconditional, matching every sibling mutation.

### Security boundaries
No new trust boundary. The cursor is **opaque** (base64 of a small serialized struct with a keyed integrity tag — see below) so a client cannot infer table layout or forge an arbitrary offset; it is not a secret and does not need encryption, but it must be **tamper-evident** so a mutated cursor fails closed with a clear `400`, not silent misbehavior or a SQL error leak. Retention deletes touch no encrypted/secret columns. Deadline enforcement does not change credential handling. `If-Match` requirement change is a stricter (not weaker) authorization/consistency posture.

### DB/migration changes
- **P1-4:** no schema change required for `applications`/`providers`/`credentials`/`system_keys`/`consumer_keys`/`trusted_jwt_issuers`/`audit_logs` (admin) — all already have `created_at`/`id`; add a composite index `(created_at desc, id desc)` (filtered by `deleted_at is null` where applicable) per listed table if the query planner would otherwise do a sort — check `EXPLAIN` during implementation; add via a new migration `migrations/0009_list_cursor_indexes.sql` only for tables that need it (most already have a PK on `id` plus an implicit `created_at` scan; add indexes only where verified necessary, do not add speculative ones). `route_definitions`, `routing_policies`, `agent_profiles` (runtime) and the conversation-domain tables get the same treatment. Tiebreaker gap is concentrated in `admin.rs`: all 9 admin list queries lack the `id desc` tiebreaker (8 on `created_at desc`, `list_audit_logs` on `occurred_at desc` — see Findings). Runtime queries already have it; conversation queries already have it except `list_messages`, which orders by `sequence_number asc` — unique per conversation, so no tiebreaker is needed there.
- **P1-5:** no schema change — `expires_at` columns already exist on both target tables. Add a migration `migrations/0009_list_cursor_indexes.sql` (or a separate `0010_retention_indexes.sql` if index changes ship separately from cursor work — coordinate numbering with whichever wave lands first) adding `idempotency_records (expires_at)` and `responses (expires_at) where expires_at is not null` partial indexes so the batched `DELETE ... WHERE expires_at < now()` does an index scan, not a sequential scan, as the tables grow.
- **P1-6, P1-7, P1-8:** no schema change.

### API & OpenAPI changes
- **P1-4:** `PageQuery.cursor` becomes functional (no shape change — already `Option<String>`); `ListResponse.pagination.next_cursor`/`has_more` become real. No breaking change to request/response shapes, only to *behavior* (cursor no longer silently ignored). Response bodies for the affected `list_*` endpoints are otherwise unchanged.
- **P1-8:** `put_application_execution_policy`'s `If-Match` parameter changes from `Option<i64>` (optional) to `i64` (required) in the `#[utoipa::path]` annotation — this **is** a breaking OpenAPI contract change (previously-optional header becomes mandatory); document it explicitly in the PR description and in this plan's Definition of Done. No other endpoint's contract changes.
- P1-5, P1-6, P1-7: no OpenAPI surface change (worker internals, timeout behavior, and a new test respectively).

### Backward compatibility
- P1-4 is additive/behavior-fixing: clients that never sent `cursor` see identical first-page behavior (same `limit`, same default ordering) except `has_more`/`next_cursor` now populate correctly instead of always being `false`/`null` — a caller that previously assumed "no more pages ever" and ignored these fields is unaffected; a caller that already handles `has_more` correctly starts working.
- P1-8 breaks any existing caller of `PUT .../execution-policy` that omits `If-Match` — this is intentional (matches every other mutation) and must be called out as a breaking change in the release notes for this iteration.
- P1-5/P1-6/P1-7 have no API compatibility impact.

### Deployment implications
- P1-5's retention worker runs inside the existing `WorkerSupervisor` tokio task (already spawned in `main.rs:56`) — no new process, no new deployment unit. It is safe under the current single-replica constraint (P3-2/P3-4 in the audit — no leader election exists yet); running it on more than one replica would cause redundant (but not incorrect — `DELETE ... WHERE expires_at < now()` is idempotent and `SKIP LOCKED` prevents double-processing) work. Document explicitly: this worker is **not yet multi-replica-safe in the "avoid duplicate work" sense** but **is safe from a correctness standpoint** (no double-delete corruption) — full leader-election gating is plan 10's job.
- No new external dependency, no new config secret. New `WorkerSettings` fields need defaults in `Settings::default()`/`settings.rs:678` region and in `charts/moira` values if the Helm chart exposes worker tuning (verify during implementation; if it doesn't currently expose worker settings, no chart change needed).

### Failure & recovery
- **Pagination:** a malformed/tampered cursor returns `400 invalid_cursor` (new error code) rather than a `500` or a silently-wrong page. A cursor pointing past the end of data (row deleted between pages) returns an empty page with `has_more:false` — standard keyset-pagination behavior, no special-casing needed.
- **Retention:** each batch runs in its own short transaction; a mid-batch crash leaves already-committed batches deleted and the rest untouched — safe to resume on the next tick. `SKIP LOCKED` prevents a stuck retention job from blocking concurrent idempotency-claim transactions that touch the same rows (`src/infra/repositories/admin.rs:560-593`'s single-key delete).
- **Deadline:** a timeout on any newly-wrapped phase must still perform the same cleanup the provider-call timeout path already performs (permit release, attempt status if an attempt row was already created, audit event) — implementation must audit each phase's error path individually rather than assume a uniform wrapper is safe (e.g., `resolve_credential`'s failure path returns `ExecutionFailure` directly at `:249-267`, no attempt row exists yet at that point; `runtime_handle`'s failure path at `:304-322` is the same; only the post-provider-call persistence writes at `:509-541` have an attempt row already `started` and need explicit terminal-status handling on timeout, since those are the ones currently unprotected but *after* a successful provider response — a timeout there must not silently drop a completed-but-unpersisted result without at least a best-effort persistence retry or an explicit "output committed but not durably recorded" audit trail).
- **P1-7 test:** if the test reveals an actual bug (permit leak or stuck `in_progress` row), fixing it is in scope for this iteration, not deferred — the finding explicitly says the code is "plausibly correct" but unverified; if verification fails, the fix belongs here since it is the same file/subsystem.
- **P1-8:** stale `If-Match` returns the existing `409 resource_version_conflict` envelope (`src/http/admin.rs:86-95` `ensure_version`) — identical shape to every other endpoint's conflict response, so client-side handling requires no new code path.

---

## Detailed Implementation

### Module 1 — Shared cursor codec (new file)
**`src/domain/pagination.rs`** (new):
- `pub struct ListCursor { pub ts: DateTime<Utc>, pub id: Uuid }` — `ts` is the endpoint's sort timestamp (`created_at`, `updated_at`, or `occurred_at`); plus `pub struct SeqCursor(pub i64)` (or one enum covering both) for the `sequence_number asc` message list.
- `impl ListCursor { pub fn encode(&self) -> String; pub fn decode(raw: &str) -> Result<Self, AppError>; }` (same for `SeqCursor`) — encoding: serialize the key as a small fixed-format string (e.g. `format!("{}|{}", ts.to_rfc3339(), id)`), append a short HMAC/truncated-hash tag keyed by a process-local (or config-provided) pagination secret so a client cannot construct an arbitrary cursor by hand (defense in depth — this is not a security boundary since cursors are non-secret positional pointers, but tamper-evidence prevents malformed-input SQL errors and confusing cross-endpoint reuse), then base64 (`URL_SAFE_NO_PAD`, matching the pattern already used in `src/security/masking.rs:1,7`). Decoding verifies the tag and rejects on mismatch with the existing coded-error constructor `AppError::coded(StatusCode::BAD_REQUEST, "invalid_cursor", "...")` (`src/error.rs:78-84`; siblings `conflict` at `:86`, `unprocessable` at `:90`), matching `resource_version_conflict`'s pattern in `src/http/admin.rs:90-93`.
- Export from `src/domain/mod.rs`.
- Unit tests colocated: round-trip encode/decode; tamper (flip a byte) → decode error; wrong-length/garbage input → decode error, not panic.

### Module 2 — Admin list pagination
**`src/infra/repositories/admin.rs`**: for each of the 9 `AdminRepository` `list_*` trait methods used by paginated endpoints (`list_applications:754`, `list_providers:~875`, `list_provider_models:998`, `list_credentials`/`list_user_credentials:1141-1147`, `list_system_keys`/`list_consumer_keys` around `1413-1417`, `list_trusted_jwt_issuers:1564`, `list_audit_logs:1698`, `list_keys:1912` if separately paginated — verify exact method boundaries against the trait defined at `:61-` before editing):
- Change trait signature from `(&self, limit: i64)` to `(&self, cursor: Option<ListCursor>, limit: i64) -> Result<(Vec<T>, bool /* has_more */), AppError>` (or return `limit+1` rows and let the caller trim — pick one convention and apply uniformly; recommend repository returns the raw over-fetched rows and the application layer trims + computes `has_more`, keeping repository SQL simple).
- SQL change: add `and (created_at, id) < ($N::timestamptz, $N+1::uuid)` when a cursor is present (row-key strictly *less than* the cursor, since ordering is descending; bind `cursor.ts`, `cursor.id`), add the missing `id desc` tiebreaker, keep `order by created_at desc, id desc limit $M` where `$M = limit + 1`. For `list_audit_logs` the key column is `occurred_at` (`:1705`), not `created_at` — same shape, different column.
- **`src/application/admin.rs`**: each `list_*` method decodes `PageQuery.cursor` via `ListCursor::decode`, calls the updated repository method, trims to `limit`, builds `Pagination { next_cursor: has_more.then(|| last_row_cursor.encode()), has_more }`, returns `ListResponse { data, pagination }` instead of `ListResponse::new(data)` (the current always-empty-pagination constructor at `src/domain/admin.rs:19-28` — keep `ListResponse::new` for any genuinely non-paginated caller, add a new constructor `ListResponse::paginated(data, pagination)` or extend `new` to take `Pagination` explicitly).
- **`src/http/admin.rs`**: no signature change needed at the HTTP layer (`list_applications` etc. already pass `query` — extend to pass `query.cursor()` decoded, or pass the raw `PageQuery` through and let the application layer own decoding — prefer the latter to keep HTTP thin, matching existing convention where `query.limit()` is the only method called at the HTTP layer today).

### Module 3 — Runtime-admin list pagination
**`src/infra/repositories/runtime.rs`** and **`src/application/runtime_admin.rs`**: identical treatment for `list_route_definitions` (`runtime_admin.rs:72`), `list_routing_policies` (`:213`), `list_agent_profiles` (`:386`) — these repos are concrete-only (no trait, per audit P2-3; do not add a trait here, that's plan 06's job, just add the parameter to the concrete impl methods). These queries already order by `created_at desc, id desc` (lines 120, 280, 457) — only the cursor *predicate* is missing, not the tiebreaker.

### Module 4 — Conversation-domain list pagination
**`src/infra/repositories/conversation.rs`** and **`src/application/conversation.rs`**: identical treatment for `list_conversations` (`:119`), `list_messages` (`:238`), `list_memories` (`:481`), `list_rag_collections` (`:776`), `list_rag_documents` (`:900`). All five repository queries already have deterministic ordering (verified — see Findings): conversations/memories use `(updated_at desc, id desc)` (`conversation.rs:538,811` — cursor key is **updated_at**, and note a row updated mid-sweep can move ahead of the cursor and be re-seen or skipped; acceptable standard keyset semantics, document it), rag_collections/rag_documents use `(created_at desc, id desc)` (`:921,1096`), and messages use `sequence_number asc` (`:713`) — use `SeqCursor` with a `sequence_number > $N` predicate there, no tiebreaker needed.

### Module 5 — Retention/cleanup worker
**`src/infra/workers.rs`**:
- Add `mod retention;` (new file `src/infra/workers/retention.rs`, converting `workers.rs` into a small module directory `src/infra/workers/mod.rs` + `retention.rs` — or keep flat with a `retention_cleanup` free function in `workers.rs` if the project prefers flat modules; check `src/infra/` sibling conventions before choosing, e.g. `src/infra/repositories/` is already a directory, suggesting a `src/infra/workers/` directory is consistent).
- `retention::run_once(pool: &PgPool, settings: &WorkerSettings, metrics: &MetricsRegistry) -> Result<RetentionOutcome, AppError>`: loops batched deletes against `idempotency_records` and `responses` (`where expires_at < now()`, using `id in (select id from t where expires_at < now() order by expires_at limit $batch for update skip locked)` to bound work and avoid long locks), returns counts.
- `run_supervisor` (`workers.rs:131-153`): add a second `tokio::time::interval` (or a tick counter on the existing interval) gated by `WorkerSpec { name: "retention-cleanup", .. }` being configured (`self.settings.enabled && spec.enabled_by_default`, already computed in `snapshot()` at `:99-117` — reuse that logic rather than duplicating it), calling `retention::run_once` and recording `state.metrics.record_retention_cleanup(outcome)` (new metrics method — see plan 05 for the metrics registry itself; this iteration adds the *call site* and a minimal counter if plan 05 hasn't landed histograms yet, coordinate with plan 05 owner on whether to add `MetricsRegistry::record_retention_deleted(table: &str, count: u64)` here or there — recommend adding it here since it's needed for this iteration's Verification section, and plan 05 can later upgrade it to a proper histogram/labeled metric without changing the call site signature materially).
- **`src/config/settings.rs`**: extend `WorkerSettings` (`:195-202`) with `pub retention_batch_size: usize` and `pub retention_interval_seconds: u64` (default e.g. 500 rows/batch, every 5 minutes — independent of `retry_base_delay_seconds`, which drives the base supervisor tick at `src/infra/workers.rs:132-134`), with defaults added alongside the existing `WorkerSettings` defaults (the `retry_base_delay_seconds` default sits at `src/config/settings.rs:663`; the `:678` region is `TelemetrySettings` — plan 05's territory, don't collide).
- **New migration** `migrations/0009_retention_indexes.sql`: partial indexes `create index if not exists idempotency_records_expires_at_idx on idempotency_records (expires_at);` and `create index if not exists responses_expires_at_idx on responses (expires_at) where expires_at is not null;`.

### Module 6 — Execution deadline coverage
**`src/application/execution.rs`**:
- Wrap `self.resolve_credential(&command, &candidate).await` (`:249`) as `tokio::time::timeout(remaining_execution_time(execution_deadline).ok_or_else(deadline_failure_as_execution_failure)?, self.resolve_credential(&command, &candidate)).await`, mapping a `Elapsed` to the existing `ExecutionFailure` deadline class (need an `ExecutionFailure`-typed deadline constructor alongside `deadline_failure()` at `:1869-1874`, which currently returns a plain `ExecutionFailure` usable directly — reuse it, wrapping the inner `Result<Result<_, ExecutionFailure>, Elapsed>` and flattening).
- Same treatment for `self.runtime_handle(&provider, &candidate, &credential).await` (`:304`).
- For terminal persistence (`:509-541`, the three sequential `update_attempt` / `insert_usage_record` / `touch_credential_used` awaits): wrap the **group** in a single `tokio::time::timeout(remaining, async { ...three awaits... }).await` rather than each individually (they're a logical unit — partial completion of this group on timeout is already a known-hard problem per the Failure & Recovery section above; on timeout here, log/audit an explicit `"execution.terminal_persistence_deadline_exceeded"` event distinct from a normal deadline failure, since the provider call *succeeded* and output may already be committed elsewhere — do not silently reclassify a successful execution as failed without this distinction, matching the existing `attempt_timeout_failure(bounded_by_total_deadline, output_committed)` pattern at `:1876-1893` which already models "output committed" as a special case).
- Verify `remaining_execution_time` (`:1863-1867`) is called fresh before each newly-wrapped phase (deadline shrinks monotonically across phases — do not reuse a stale `remaining` computed before an earlier phase's actual elapsed time).

### Module 7 — Stalled-reader integration test
**`tests/execution_lifecycle.rs`** (new test, colocated with `public_sse_disconnect_persists_cancellation_and_reuses_capacity` at `:733`):
- New test `public_sse_stalled_reader_without_disconnect_releases_permit_and_terminates_response` (or similar name).
- Reuse `MockOpenAiServer` / `ProviderScript::StalledStream` + `ScriptGate` (`tests/support/mock_openai.rs`, same pattern as the existing disconnect test) to get a controlled first delta, then continue emitting deltas from the provider side (gate-controlled, not sleep-based) while the test harness **holds the `reqwest::Response` open and stops polling `.bytes_stream()`** — do not `drop(body)`; instead, park the future (e.g. `tokio::spawn` it and never `.await` it, or simply stop calling `.next()`), so the TCP socket stays open and the server-side `mpsc::Sender` backs up until `send_public_event`'s `send_timeout` elapses.
- Configure a short `send_timeout` for the test (check how `send_timeout` is threaded into `supervise_public_stream` — likely from `RuntimePolicy`/settings; use the test fixture's existing knobs, e.g. `RuntimePolicy { .. }` as used at `tests/execution_lifecycle.rs:752-757`, or a dedicated test-only settings override) so the test doesn't need a long real-time wait — assert completion via `wait_for_attempt_status`-style polling (`:986-1008` pattern) with a bounded `timeout(Duration::from_secs(5), ...)`, not a bare `sleep`.
- Assertions (mirroring the disconnect test's assertions at `:813-857`): `execution_attempts` has no row with `status = 'started'` for this `execution_id`; `responses` has no row with `status = 'in_progress'`; the concurrency permit was released (assert via a follow-up execution reusing capacity, same pattern as `:859-871`).
- If this test fails against current code, fix the underlying bug in `src/application/public.rs`/`src/application/execution.rs` as part of this same PR — do not land a known-red test.

### Module 8 — Require `If-Match` on execution-policy PUT
**`src/http/admin.rs`**:
- `:319`: change `("If-Match" = Option<i64>, Header, description = "Optional current resource version")` to `("If-Match" = i64, Header, description = "Required current resource version")` (matching every sibling, e.g. `:175`, `:205`, `:234`).
- `:338`: replace `optional_if_match(&headers)?` with `require_if_match(&headers)?` (passing `Some(version)` through, or changing the service parameter to plain `i64`). The service layer already enforces the version when given `Some` (verified — `src/application/public.rs:828-841`), so **no** HTTP-layer `ensure_version` call is needed; keep enforcement in the service, one place only.
- The service's existing check is check-then-act (compare in Rust, then a separate unconditional repo `put`), leaving a TOCTOU window between compare and write. Close it in the same PR by making the repo update version-checked in SQL (`update application_execution_policies set ... where application_id = $1 and version = $2` returning the row, `409` on zero rows — in `src/infra/repositories/public.rs:117`'s `put_application_execution_policy`), or explicitly document deferring that to plan 06 if the diff proves invasive; the required-header change must not wait on it.
- Confirm `docs/todo.md:21`'s scope ("including application execution policy PUT") is now satisfied and update the TODO line to reflect completion in this iteration (mechanical `docs/todo.md` edit is in scope for this plan since it directly tracks these findings).

### Module 9 — i18n catalog wiring and entries (`CONVENTIONS.md` §4)
**`src/lib.rs`** (one line, blocking prerequisite): add `pub mod i18n;` to the module list at `:3-11`. Without it, `src/i18n/mod.rs` and everything under `src/i18n/catalog/` remain orphaned files that are never compiled (see Re-audit corrections), so no catalog entry has any effect and no test can assert one. Expect the first compile after wiring to surface `dead_code`/unused warnings under `-D warnings` for `RESPONSE_NOTICE_CATALOG`/`default_message_for_key` if nothing references them yet — the new tests in Verification reference `is_known_key` and `default_message_for_key`, and `#[allow(dead_code)]` is **not** an acceptable substitute for a reference; if a warning remains after the tests land, resolve it by using the item, not by silencing it. **Ownership resolved: plan 02b owns this line**, since the roadmap order is `02a → 02b → 03 → 04` and 02b's Wave 0 wires it. This plan therefore *consumes and verifies* it rather than adding it — keep the line, not the ownership, as the shared artifact. If 02b has slipped and the line is absent on `main` at this plan's Wave 0, add it here and record that in the PR.

**`src/i18n/catalog/errors.rs`** — add two entries, alphabetically placed among the existing `moira.error.*` block (`:8-294`):
- `I18nEntry { key: "moira.error.invalid_cursor", default_message: "The pagination cursor is invalid.", description: "Used when a list cursor is malformed, tampered with, expired in format, or was issued for a different list endpoint." }` — the code emitted by `ListCursor::decode`/`SeqCursor::decode` via `AppError::coded(StatusCode::BAD_REQUEST, "invalid_cursor", ...)`. `src/error.rs:146-148` derives `message_key` as `format!("moira.error.{}", code())`, so the suffix **must** be exactly `invalid_cursor`.
- `I18nEntry { key: "moira.error.if_match_required", default_message: "The If-Match header is required for this request.", description: "Used when a versioned mutation is called without the If-Match precondition header." }` — required by `CONVENTIONS.md` §4.1 for P1-8's new failure path. **Decision with blast radius, call it out in the PR:** `require_if_match` (`src/http/admin.rs:66-77`) currently returns `AppError::BadRequest`, whose code is `bad_request` (`src/error.rs:130`) and whose key `moira.error.bad_request` already exists. Switching it to `AppError::coded(StatusCode::BAD_REQUEST, "if_match_required", "If-Match header is required")` gives P1-8's path its own key as §4 requires, but changes the `error.code` string on **every** endpoint that calls `require_if_match` (`:195,224,253,284,439,468,497` and siblings) from `bad_request` to `if_match_required`. Status code, response shape, and `message` text are unchanged; only `code`/`message_key` change. Recommend taking it (it is strictly more specific and this plan is already the "If-Match consistency" plan), enumerate the affected endpoints in the PR's Breaking-changes section, and update any existing test asserting `bad_request` on a missing `If-Match`. If the reviewer rejects the blast radius, the fallback is to leave `require_if_match` on `AppError::BadRequest` and record in the PR that P1-8's missing-header path is covered by the pre-existing `moira.error.bad_request` entry — the fallback still satisfies §4 (the path has a key and an English default), it is just less specific.
- Do **not** add a `moira.error.if_match_invalid` entry unless the malformed-header branches (`:71,76`) are also given their own code — this plan does not change those; they stay `bad_request`. Say so in the PR rather than leaving it ambiguous.

**`docs/i18n-response-catalog.json`** — mirror both new entries into the `entries` array in the same PR (§4.4; the mirror is hand-synced until plan 06 adds the drift test). While editing, drop the two duplicate objects noted in Re-audit corrections (`moira.error.idempotency_conflict`, `moira.error.rate_limited`) so the mirror's 63 entries become 61 unique ones matching the Rust catalog exactly.

**No `moira.notice.*` entry is required by this plan.** Every surface it touches is either an error path, an unchanged `200` list body (`ListResponse<T>` carries no human-readable text), or an internal worker with no user-visible response. Retention deletions surface as metrics/`WorkerSnapshot` fields, not as notice strings. If an implementer finds themselves writing an English literal into a handler response, stop and add a `moira.notice.*` entry instead (§4.2) — that is a scope change worth flagging, not a silent inline string.

**Existing keys this plan reuses, verified present, no new entry needed:** `moira.error.resource_version_conflict` (`errors.rs:224-228`, "Used when If-Match does not match the stored version") for P1-8's `409`, and `moira.error.bad_request` (`:9-13`) if the fallback above is taken.

### Optional extension (flagged, not required for this iteration)
- ~~**Idempotency for conversation/memory/RAG routes**~~ — **RESOLVED and REASSIGNED. No longer an option for this plan.** Per `CONVENTIONS.md` §0 **D1/D2**, P0-2 is fixed by *implementing real replay* (not by removing the advertisement), and that work is owned by **plan 02b** (`plan/02b-idempotency-replay`), which lands **before** this plan in the roadmap order `02a → 02b → 03 → 04`. 02b routes `create_rag_collection`/`create_rag_document`/`ingest_rag_document` through the existing `AdminCommandRunner` envelope. **This plan must not implement, duplicate, or re-plan replay.** Two live couplings to respect: (a) 02b introduces a `_with_connection` repository seam on `ConversationService`'s paths, so this plan's Module 4 pagination edits to the same file must be rebased onto 02b's landed state rather than assuming the pre-02b shape; (b) 02b adds unpruned `idempotency_records` rows, which **this plan's retention worker (P1-5, Module 2) is responsible for pruning** — that dependency is real and in-scope here.
- **Public API cursor pagination** (`UsageQuery`/`ExecutionQuery` at `src/domain/public.rs:348,370`, `list_executions_authorized`/`list_usage_authorized` at `src/infra/repositories/public.rs:519,545`; `docs/todo.md:53`): identical bug, reuses the `src/domain/pagination.rs` codec built in Module 1, but touches files plan 03/05 specialists may also be editing this iteration — recommend a fast-follow PR immediately after this plan lands, using the same codec.

---

## Multi-Agent Workflow

**Coordinator** owns `src/domain/pagination.rs` creation (Module 1) first — every other wave depends on it. Coordinator also owns `docs/todo.md` edits (mechanical, low-conflict-risk, done last after all waves report completion) and final cross-module wiring review.

### Wave 0 (sequential, blocking) — coordinator or one specialist
- Module 1: `src/domain/pagination.rs` + export from `src/domain/mod.rs`. Small, fast, unblocks Waves 1-3.
- Module 9's one-line prerequisite: `pub mod i18n;` in `src/lib.rs`. **Expected to be already present**, since plan 02b owns it and lands first (`02a → 02b → 03 → 04`); this plan's Wave 0 **verifies** it rather than adding it, and only adds it if 02b slipped. Either way it must be in place before Module 1's `invalid_cursor` unit test and every downstream i18n assertion, which depend on the catalog actually compiling. Coordinator confirms `cargo test --lib i18n` runs the previously-dead `src/i18n/catalog/mod.rs:40-64` tests before releasing later waves; record in the PR which plan landed the line.

### Wave 1 (parallel after Wave 0; three specialists, disjoint files)
- **Specialist A — admin pagination**: Module 2. Files: `src/infra/repositories/admin.rs`, `src/application/admin.rs`. Read-only touch of `src/http/admin.rs` (only if HTTP-layer cursor decoding is chosen over application-layer decoding — prefer application-layer to avoid touching this file, which Specialist D also edits in Wave 2).
- **Specialist B — runtime-admin pagination**: Module 3. Files: `src/infra/repositories/runtime.rs`, `src/application/runtime_admin.rs`. Disjoint from A and C.
- **Specialist C — conversation pagination**: Module 4. Files: `src/infra/repositories/conversation.rs`, `src/application/conversation.rs`. Disjoint from A and B.
- New migration file for cursor-supporting indexes, if any are found necessary during A/B/C's `EXPLAIN` checks, should be coordinated through the coordinator to avoid two specialists claiming the same migration filename (`migrations/0009_*.sql`) — reserve `0009_list_cursor_indexes.sql` for Wave 1 and `0010_retention_indexes.sql` for Wave 2 up front to avoid a collision, even if one ends up empty.

### Wave 2 (parallel with Wave 1; independent subsystem, zero file overlap)
- **Specialist D — retention worker**: Module 5. Files: `src/infra/workers.rs` (or new `src/infra/workers/` directory + `retention.rs`), `src/config/settings.rs` (additive fields only — coordinate with plan 03/05 specialists who may also touch `settings.rs`; claim the specific line range for `WorkerSettings` explicitly in the PR to ease merge), `migrations/0010_retention_indexes.sql`.

### Wave 3 (sequential after Wave 1 lands, since it touches the same execution.rs Module 6 will need a stable base; parallel with Wave 2)
- **Specialist E — execution deadline**: Module 6. Files: `src/application/execution.rs` only. No overlap with Waves 1/2.
- **Specialist F — stalled-reader test**: Module 7. Files: `tests/execution_lifecycle.rs`, possibly `tests/support/mock_openai.rs` if the existing `ProviderScript`/`ScriptGate` primitives need a small extension to support "keep emitting after send_timeout" — coordinate with Specialist E, since a discovered bug in this test may require a fix inside `src/application/execution.rs` or `src/application/public.rs` (the latter is otherwise untouched this iteration — flag any `public.rs` edit clearly in the PR since no other specialist claims that file).

### Wave 4 (small, independent, can run any time after Wave 0)
- **Specialist G — If-Match required**: Module 8. Files: `src/http/admin.rs` only (the specific `put_application_execution_policy` function and its `#[utoipa::path]` block — narrow diff, low collision risk even though Specialist A may also touch this file for HTTP-layer cursor wiring; sequence G after A if both must touch `admin.rs`, or have G go first since it's a 10-line change). If the `if_match_required` code decision (Module 9) is taken, G also edits `require_if_match` (`:66-77`) — still the same file, still narrow.
- **Specialist H — i18n entries + catalog tests**: Module 9's catalog work (the `src/lib.rs` line itself lands in Wave 0). Files: `src/i18n/catalog/errors.rs`, `docs/i18n-response-catalog.json`, and the catalog assertions in `src/i18n/catalog/mod.rs`'s `#[cfg(test)] mod tests`. Zero overlap with A-G. Sequence H **after** G's code decision is made, since the entry set depends on whether `if_match_required` is adopted; H must not guess.

### Checkpoints
- After Wave 0: coordinator confirms `cargo build` compiles with the new pagination module and its unit tests pass before releasing Waves 1-4.
- After Waves 1-4: coordinator runs the full verification suite (below) once, not per-specialist, to catch cross-module interference (e.g., two specialists both adding a migration numbered `0009`).
- **Read-only reviewer**: one agent reviews the full diff for (a) SQL injection safety in the new cursor predicates (must be parameterized, never string-interpolated), (b) consistent `has_more`/`next_cursor` semantics across all ~17 touched list endpoints, (c) that Module 6's phase-timeout error paths don't regress the existing `attempt_timeout_failure`/`output_committed` distinction. This reviewer makes no edits.

### Conflict avoidance
- `src/domain/mod.rs` export line: only Wave 0 touches it.
- `migrations/`: reserve filenames up front (`0009_list_cursor_indexes.sql`, `0010_retention_indexes.sql`) even if a wave ends up not needing one, to prevent two specialists picking the same number.
- `src/config/settings.rs`: only Specialist D touches `WorkerSettings` in this plan; if plan 03/05 specialists also touch this file, merge order should put whichever lands first as the base — no field-level overlap expected since each plan adds disjoint fields.
- `src/http/admin.rs`: touched by both Specialist A (optionally) and Specialist G — sequence G first (small, fast) or have A avoid this file entirely (recommended — decode cursors in the application layer).

---

## Interfaces & Contracts

### Pagination (P1-4)
- **Endpoints affected**: all `GET` list endpoints under `/api/v1/admin/*` returning `ListResponse<T>` (`list_applications`, `list_providers`, `list_provider_models`, `list_credentials`, `list_system_keys`, `list_consumer_keys`, `list_trusted_jwt_issuers`, `list_audit_logs`, `list_route_definitions`, `list_routing_policies`, `list_agent_profiles`) and conversation-domain equivalents (`list_conversations`, `list_messages`, `list_memories`, `list_rag_collections`, `list_rag_documents`).
- **Request**: `?cursor=<opaque-base64-string>&limit=N` (unchanged shape, `cursor` now functional).
- **Response**: `{ "data": [...], "pagination": { "next_cursor": "<opaque>" | null, "has_more": bool } }` (unchanged shape, values now correct).
- **Status codes**: `200` unchanged; new `400 invalid_cursor` when the `cursor` value fails tamper/format validation (i18n message key following the existing `moira.error.*` convention in `src/i18n/catalog/errors.rs` / `docs/i18n-response-catalog.json` — add the new key to both, since P2-8 notes the catalog is hand-synced and this plan should not introduce a new drift).
- **Concurrency**: keyset pagination is safe under concurrent inserts/deletes — a row inserted after the cursor position is silently excluded from the current pagination sweep (standard, acceptable keyset semantics); a row deleted between pages simply isn't returned.
- **Cache invalidation**: N/A, list endpoints are not cached.

### Retention worker (P1-5)
- No public API surface change. Internal: `WorkerSnapshot` (`src/infra/workers.rs:22-31`, exposed via `/metrics`/admin diagnostics if applicable) may gain a `last_retention_run_at`/`last_retention_deleted_counts` field for operational visibility — optional, coordinate with plan 05 if a proper metrics histogram supersedes this.

### Execution deadline (P1-6)
- No API shape change. Behavioral contract: `timeout_ms` (request-level, `ExecutionOptions.timeout_ms`) now bounds credential resolution + runtime construction + provider call + terminal persistence as one budget, not just the provider call. A request that previously succeeded by exceeding its nominal deadline during a slow credential fetch will now correctly fail with the existing `deadline_exceeded` failure class (already part of `ExecutionFailureClass` per `:328,338-341`) — this is a **behavior tightening**, not a new error code.

### Stalled-reader test (P1-7)
- Test-only; no production interface change (validates existing `send_timeout` contract already documented implicitly by `send_public_event`'s signature at `src/application/public.rs:1791`).

### If-Match required (P1-8)
- **Endpoint**: `PUT /api/v1/admin/applications/{id}/execution-policy`.
- **Before**: `If-Match` header optional; omitting it skips the version check (silent overwrite).
- **After**: `If-Match` header **required**; omitting it returns `400` with the existing `AppError::BadRequest("If-Match header is required")` message (`src/http/admin.rs:69`, identical to every sibling endpoint's behavior); a stale version returns `409 resource_version_conflict` (`:90-93`, identical envelope to every sibling).
- **Transaction boundary**: unchanged — whichever layer ends up owning the version check (see Module 8 implementation note) must remain within the same transaction/atomic operation as the update to avoid a TOCTOU gap.

### i18n contract (`CONVENTIONS.md` §4)
- **Envelope is unchanged**: every error this plan can produce serializes as `ErrorResponse { error: ErrorDetail { code, message_key, message, message_args, request_id, details } }` (`src/error.rs:52-65`), with `message_key = format!("moira.error.{}", code)` (`:146-148`). This plan adds no new envelope field and no new response shape.
- **New keys** (see Module 9): `moira.error.invalid_cursor` (P1-4's `400`) and `moira.error.if_match_required` (P1-8's missing-header `400`, decision-gated). Both need an English `default_message` **and** a `description` in `src/i18n/catalog/errors.rs`, mirrored into `docs/i18n-response-catalog.json` in the same PR.
- **Reused keys**: `moira.error.resource_version_conflict` (P1-8's `409`, already present at `errors.rs:224-228`); `moira.error.bad_request` (fallback path only).
- **New notice keys: none** — this plan introduces no user-visible success string (rationale in Module 9).
- **`message_args`**: `invalid_cursor` must carry **structured** args only if it carries any at all — e.g. `{"parameter": "cursor"}`. Never interpolate the offending cursor value into `message`, and never emit pre-formatted English prose as an arg (§4.3). Echoing the raw client-supplied cursor back is also a reflected-input smell; keep the message generic.
- **Prerequisite**: none of this is observable until `pub mod i18n;` is wired in `src/lib.rs` (Wave 0) — the catalog is currently uncompiled (see Re-audit corrections).

---

## Verification

Both layers are mandatory (`CONVENTIONS.md` §3): a unit layer beside the code with no database, **and** an e2e layer under `tests/` driving the real HTTP surface against real PostgreSQL 16 + pgvector via the `tests/support/mod.rs` harness. Test names below are binding — the Definition of Done references them by name, per §3's "a named, passing test proves the behavior."

### Unit (`#[cfg(test)] mod tests`, no database)

**`src/domain/pagination.rs`** (new file, colocated tests) — cursor encode/decode:
- `list_cursor_round_trips_through_encode_and_decode`
- `seq_cursor_round_trips_through_encode_and_decode`
- `tampered_list_cursor_is_rejected_as_invalid_cursor` — flip one base64 character, assert `decode` returns the `invalid_cursor`-coded error, not a decoded value.
- `truncated_or_garbage_cursor_is_rejected_without_panicking` — empty string, non-base64 bytes, valid base64 of nonsense, oversized input; assert `Err`, assert no panic.
- `cursor_issued_for_a_different_sort_key_is_rejected` — a `SeqCursor` payload offered to `ListCursor::decode` (and vice versa) fails closed; proves cross-endpoint reuse cannot silently produce a wrong page.
- `invalid_cursor_error_carries_the_expected_code_and_message_key` — assert the produced `AppError` yields `code == "invalid_cursor"` and `message_key == "moira.error.invalid_cursor"` via `error_response(None)` (`src/error.rs:94-110`), and that `message` is non-empty.
- `encoded_cursor_contains_no_row_content_beyond_the_sort_key` — decode the payload and assert it carries only the timestamp+id (or sequence number), guarding the Security-boundaries claim by construction.

**Keyset predicate construction** — extract the predicate/limit arithmetic into pure helpers so it is unit-testable without a database (e.g. a `keyset` helper module used by all three repository files); tests colocated with that helper:
- `keyset_predicate_is_omitted_when_no_cursor_is_supplied`
- `keyset_predicate_uses_strict_less_than_for_descending_lists` — `(created_at, id) < ($n, $n+1)`.
- `keyset_predicate_uses_the_occurred_at_column_for_audit_logs` — guards the one admin query whose key is not `created_at` (`src/infra/repositories/admin.rs:1705`).
- `keyset_predicate_uses_strict_greater_than_for_ascending_sequence_lists` — `sequence_number > $n` for `list_messages` (`conversation.rs:713`).
- `keyset_predicate_binds_parameters_and_never_interpolates_values` — assert the generated fragment contains only `$N` placeholders and no cursor-derived literal; this is the SQL-injection guard the read-only reviewer checks for, made mechanical.
- `over_fetch_limit_is_limit_plus_one`

**`src/application/admin.rs`** (and the mirrored helpers used by `runtime_admin.rs` / `conversation.rs`) — page assembly, pure, no DB:
- `has_more_is_false_when_exactly_limit_rows_are_available`
- `has_more_is_true_and_page_is_trimmed_when_limit_plus_one_rows_are_fetched`
- `next_cursor_encodes_the_last_returned_row_not_the_over_fetched_row` — the classic off-by-one that silently skips a row between pages.
- `next_cursor_is_none_when_has_more_is_false`

**`src/application/execution.rs`** — deadline budget arithmetic, pure, no DB:
- `remaining_execution_time_is_none_once_the_deadline_has_passed` (covers `:1863-1867`)
- `remaining_execution_time_shrinks_monotonically_across_successive_phases` — proves the "recompute fresh per phase" rule in Module 6, catching a stale-`remaining` regression.
- `phase_budget_is_the_minimum_of_remaining_budget_and_phase_timeout` — mirrors the existing `attempt_timeout` computation at `:496-503`.
- `terminal_persistence_timeout_maps_to_the_output_committed_failure_class` — asserts a timeout in the `:509-541` group is **not** classified as a plain deadline failure, preserving the `attempt_timeout_failure(bounded_by_total_deadline, output_committed)` distinction at `:1876-1893`.
- `zero_or_negative_remaining_budget_never_produces_an_unbounded_timeout` — guards against `Duration::ZERO` being treated as "no limit."

**`src/i18n/catalog/mod.rs`** (existing `#[cfg(test)] mod tests` at `:40-64`, now actually compiled) — i18n presence:
- `pagination_and_precondition_error_keys_are_present_in_the_catalog` — `is_known_key("moira.error.invalid_cursor")` and `is_known_key("moira.error.if_match_required")` (the latter only if the code is adopted).
- `new_catalog_entries_have_non_empty_default_messages_and_descriptions` — `default_message_for_key` returns `Some(non-empty)` for both, and the matching `I18nEntry.description` is non-empty.
- The pre-existing `response_catalog_keys_are_unique` test (`:45-50`) now runs for the first time and guards against the duplicate-entry defect found in the docs mirror recurring in the Rust catalog.

### E2E / integration (under `tests/`, real PostgreSQL 16 + pgvector, `tests/support/mod.rs` harness)

Every file below must follow the existing fail-closed pattern (`tests/support/mod.rs:430-437`: `panic!` when **`CI=true`** and `MOIRA_TEST_DATABASE_URL` is absent (value check per `CONVENTIONS.md` §3 — never `var_os("CI").is_some()`)) so these cannot silently skip in CI.

**`tests/list_pagination.rs`** (new):
- `admin_application_list_pages_through_without_duplicates_or_gaps` — seed >2×`limit` applications, walk every page via `next_cursor`, assert the concatenated ids equal the full ordered set exactly once.
- `admin_audit_log_list_pages_through_by_occurred_at` — the non-`created_at` key path.
- `conversation_list_pages_through_by_updated_at_with_id_tiebreaker`
- `conversation_message_list_pages_through_by_ascending_sequence_number`
- `last_page_reports_has_more_false_and_null_next_cursor`
- `rows_tied_on_the_sort_timestamp_are_ordered_deterministically_by_id` — seed several rows with an identical `created_at`, page through twice, assert identical ordering; this is the tiebreaker gap the 9 admin queries have today.
- `tampered_cursor_returns_400_invalid_cursor_with_i18n_keys` — flip a character, assert `400`, `error.code == "invalid_cursor"`, `error.message_key == "moira.error.invalid_cursor"`, and non-empty `error.message`.
- `cursor_from_a_different_list_endpoint_is_rejected_with_400_invalid_cursor`
- `cursor_pointing_past_deleted_rows_returns_an_empty_final_page` — delete the rows a live cursor points at, assert `200` with empty `data` and `has_more:false`, not a `500`.

**`tests/retention_worker.rs`** (new):
- `retention_run_deletes_expired_idempotency_records_and_keeps_live_rows`
- `retention_run_deletes_expired_responses_and_keeps_rows_with_null_or_future_expires_at`
- `retention_run_respects_the_configured_batch_size` — seed more than one batch, assert the per-call bound holds and that repeated runs drain the backlog.
- `retention_run_does_not_block_a_concurrent_idempotency_claim` — exercises `SKIP LOCKED` against the single-key delete at `src/infra/repositories/admin.rs:583-596`; use an acknowledgement gate to order the two transactions, **not** `sleep`.
- `retention_run_records_deleted_counts_for_observability` — asserts the counts surface through `MetricsRegistry`/`WorkerSnapshot` so plan 05 has a real call site to upgrade.

**`tests/execution_lifecycle.rs`** (additions alongside the existing 14 tests):
- `public_sse_stalled_reader_without_disconnect_releases_permit_and_terminates_response` — **the P1-7 test.** Acknowledgement-gated via the existing `ScriptGate` (`tests/support/mock_openai.rs:41-72`) and `ProviderScript::StalledStream { first_delta, gate }` (`:110-113`); read the first delta, then hold the `reqwest::Response` open and stop polling `.bytes_stream()` (do **not** `drop(body)` — that is what the existing `public_sse_disconnect_persists_cancellation_and_reuses_capacity` at `:733` already covers). Assert: no `execution_attempts` row left at `status = 'started'`, no `responses` row left at `status = 'in_progress'`, and the permit is reusable by a follow-up execution (mirroring `:859-871`). Completion is awaited with the existing bounded `wait_for_attempt_status` polling helper (`:986-1008`) wrapped in `tokio::time::timeout`, **never** a bare `sleep` (P2-12).
- `slow_credential_resolution_is_bounded_by_the_total_execution_deadline` — gate-held credential resolution; assert the request fails with the `deadline_exceeded` class before the provider is ever called, and that the permit is released.
- `slow_runtime_handle_construction_is_bounded_by_the_total_execution_deadline`
- `terminal_persistence_timeout_is_recorded_as_output_committed_not_as_a_plain_failure` — if a clean fault-injection seam for the `:509-541` group is infeasible without invasive test-only plumbing, **document that explicitly in the PR** and rely on the named unit test above instead; do not silently drop the coverage.

**`tests/execution_policy_if_match.rs`** (new — P1-8's e2e; a dedicated file rather than extending `tests/admin_idempotency.rs`, which Specialist ownership already covers):
- `execution_policy_put_without_if_match_returns_400_with_a_keyed_error` — assert `400`, and assert the i18n contract holds: non-empty `error.message_key` and non-empty `error.message`; assert the exact `code`/`message_key` matching whichever Module 9 decision was taken.
- `execution_policy_put_with_stale_if_match_returns_409_resource_version_conflict` — assert `error.message_key == "moira.error.resource_version_conflict"`.
- `execution_policy_put_with_current_if_match_succeeds_and_bumps_the_version`
- `concurrent_execution_policy_puts_with_the_same_version_yield_exactly_one_success_and_one_409` — two writers released simultaneously through an acknowledgement gate (not `sleep`); this is the test that fails today because of the check-then-act TOCTOU window described in P1-8, and it is the one that proves the SQL-level version check in Module 8 actually closed it.

**`tests/http_error_contract.rs`** (extend the existing single-test file — the §4.5 exemplar):
- `invalid_cursor_error_response_carries_message_key_and_message` — drive a real list endpoint with a tampered cursor through the router and assert the full envelope (`code`, `message_key`, non-empty `message`, `message_args` object, propagated `request_id`), matching the shape the existing test asserts at `:36-44`.

### Migration
- `migrations/0009_list_cursor_indexes.sql` and `migrations/0010_retention_indexes.sql` (if both are non-empty) apply cleanly against a fresh database and are idempotent (`if not exists` guards, matching existing migration style throughout `migrations/`). Exercised by the existing clean-database CI job (`.github/workflows/ci.yml:50-58`).

### OpenAPI validation
- `src/http/mod.rs` route-coverage tests (existing `#[cfg(test)] mod tests`, `generated_openapi_covers_every_registered_route`) still pass.
- New unit test in the same module: `execution_policy_put_declares_if_match_as_required` — serializes `documented_router().into_openapi()` and asserts the `If-Match` parameter object for `put /api/v1/admin/applications/{id}/execution-policy` has `"required": true`. This is the artifact plan 05's frozen `docs/openapi.json` must capture; plan 05 mirrors it with `committed_openapi_marks_if_match_required_on_execution_policy_put` against the committed file.

### Security/secret-leak
- Cursor contents never include any secret/credential field (they only ever encode a sort timestamp+`id`, or a sequence number) — enforced by the named unit test `encoded_cursor_contains_no_row_content_beyond_the_sort_key` above, not merely by implementation-review convention.
- No new secret material reaches logs, responses, or audit metadata in this plan; the systematic leak-snapshot suites are plan 05's deliverable (P1-10) and will cover these endpoints once landed.

### Production-config
- Confirm `WorkerSettings` retention defaults are sane for a production-scale table (batch size not so small it never keeps up, not so large it holds long locks) — document the chosen defaults' rationale in the PR.

### Required Rust gates (verbatim, all must pass before merge)
```
cargo fmt --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```
Plus: clean PostgreSQL migration validation (the existing CI job `Verify migrations against clean pgvector PostgreSQL` in `.github/workflows/ci.yml:50-58`, exercised locally against a fresh `createdb`), and:
```
cargo build --release --locked
```

---

## Definition of Done

Nothing here may be checked on the strength of "implemented" — each box requires the **named, passing test** from Verification (`CONVENTIONS.md` §3).

### Findings

- [ ] `cursor`/`next_cursor`/`has_more` are functionally correct on all ~17 `ListResponse`-returning list endpoints enumerated in Modules 2-4, proven by `tests/list_pagination.rs::admin_application_list_pages_through_without_duplicates_or_gaps`, `..._audit_log_list_pages_through_by_occurred_at`, `conversation_message_list_pages_through_by_ascending_sequence_number`, `rows_tied_on_the_sort_timestamp_are_ordered_deterministically_by_id`, and `last_page_reports_has_more_false_and_null_next_cursor`; tamper and cross-endpoint rejection proven by `tampered_cursor_returns_400_invalid_cursor_with_i18n_keys` and `cursor_from_a_different_list_endpoint_is_rejected_with_400_invalid_cursor`.
- [ ] A running retention worker deletes expired `idempotency_records` and `responses` rows in bounded batches on a configurable interval, proven by `tests/retention_worker.rs::retention_run_deletes_expired_idempotency_records_and_keeps_live_rows`, `..._deletes_expired_responses_and_keeps_rows_with_null_or_future_expires_at`, and `retention_run_respects_the_configured_batch_size`; the job is visible in `WorkerSnapshot`/metrics per `retention_run_records_deleted_counts_for_observability`.
- [ ] `execute_inner`'s total deadline demonstrably bounds credential resolution and runtime-handle construction (not only the provider call), proven by `slow_credential_resolution_is_bounded_by_the_total_execution_deadline` and `slow_runtime_handle_construction_is_bounded_by_the_total_execution_deadline`; active-attempt cleanup (permit release, audit) is preserved on every newly-wrapped timeout path, and the output-committed distinction is preserved per `terminal_persistence_timeout_maps_to_the_output_committed_failure_class`.
- [ ] `tests/execution_lifecycle.rs::public_sse_stalled_reader_without_disconnect_releases_permit_and_terminates_response` passes, proving a stalled (non-disconnecting) SSE reader leaves no `execution_attempts.status = 'started'` row and no `responses.status = 'in_progress'` row and that the permit is reusable — driven by a `ScriptGate` acknowledgement gate, containing **no** `sleep`-based interleaving (P2-12). If it uncovers a real bug, the bug is fixed in this same PR; no known-red test is landed.
- [ ] `PUT /api/v1/admin/applications/{id}/execution-policy` returns `400` when `If-Match` is missing and `409 resource_version_conflict` on a stale version, proven by `tests/execution_policy_if_match.rs` (all four named tests, including `concurrent_execution_policy_puts_with_the_same_version_yield_exactly_one_success_and_one_409`); the generated OpenAPI marks `If-Match` as `required: true`, proven by `execution_policy_put_declares_if_match_as_required`.
- [ ] `docs/todo.md` lines `:20` (Phase 2 pagination), `:21` (Phase 2 If-Match), `:39` (Phase 3 deadline), `:46` (Phase 4 stalled-reader test), and `:50` (Phase 4 retention) are updated to reflect completion.
- [ ] No regression in existing `tests/admin_idempotency.rs` (9 tests), `tests/execution_lifecycle.rs` (14 pre-existing tests), `tests/public_authorization.rs`, `tests/http_error_contract.rs`, or `tests/security_foundation.rs` — full suite green.

### `CONVENTIONS.md` §8 compliance checklist

- [ ] Work performed on branch `plan/04-durability-correctness`; PR opened with all seven required description sections (§1.4), including the **Breaking API/OpenAPI changes** section naming P1-8's required-`If-Match` flip and, if adopted, the `bad_request` → `if_match_required` code change and every endpoint it affects.
- [ ] This PR is **merged before** plan 05 (`plan/05-observability-ci-gates`) commits `docs/openapi.json` and enables its OpenAPI-drift gate (§1.6); confirmed with plan 05's owner and recorded in the PR.
- [ ] All gates in §2 pass: `cargo fmt --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace --all-features`, `cargo build --release --locked`, plus clean-database migration validation for `0009`/`0010`.
- [ ] **Unit tests** delivered and passing — every named test in Verification § "Unit," covering cursor encode/decode, keyset predicate construction, page assembly, and deadline budget arithmetic; all are `#[cfg(test)] mod tests` beside the code and require no database.
- [ ] **E2E tests** delivered and passing at the HTTP level against real PostgreSQL 16 + pgvector via `tests/support/mod.rs` — `tests/list_pagination.rs`, `tests/retention_worker.rs`, `tests/execution_policy_if_match.rs`, and the `tests/execution_lifecycle.rs` additions; all fail closed in CI when `MOIRA_TEST_DATABASE_URL` is unset.
- [ ] No new `sleep()`-based interleaving anywhere in the added tests (§3); every concurrency test uses an acknowledgement gate.
- [ ] `pub mod i18n;` is wired in `src/lib.rs` so the catalog actually compiles, and `moira.error.invalid_cursor` (plus `moira.error.if_match_required` if adopted) exists in `src/i18n/catalog/errors.rs` with a non-empty English `default_message` and `description`, mirrored into `docs/i18n-response-catalog.json` in this same PR, with presence proven by `pagination_and_precondition_error_keys_are_present_in_the_catalog` and the envelope proven by `invalid_cursor_error_response_carries_message_key_and_message`.
- [ ] No handler introduced by this plan returns a hardcoded human string without a catalog entry (§4.2); no `moira.notice.*` entry was needed, and the PR says so explicitly.
- [ ] No secret-leak: verified by `encoded_cursor_contains_no_row_content_beyond_the_sort_key` (cursors carry only sort keys) and by the new SQL-parameterization unit test `keyset_predicate_binds_parameters_and_never_interpolates_values`.
- [ ] (Frontend / Auth-touching §8 items) **N/A** — this plan ships no console code and touches no authentication configuration; recorded as N/A rather than silently omitted.

---

## Risks & Rollback

**Security**: none of these changes weaken a security boundary; P1-8 strictly tightens one (If-Match now required). The cursor codec's tamper tag is defense-in-depth, not a security control per se — do not conflate it with actual encryption; document this clearly so a future reviewer doesn't assume cursors carry confidentiality guarantees they don't need.

**Data-migration**: the two new migrations are purely additive (`create index if not exists`) — zero risk to existing data, safe to run against a live production database (index creation on `idempotency_records`/`responses` may briefly increase write latency during index build on very large tables; use `create index concurrently` if table size at deploy time warrants it — flag this as an implementation-time decision based on actual row counts, not a default).

**Compatibility**: P1-8 is a deliberate breaking change for one endpoint's `If-Match` header — call it out explicitly in release notes; no other endpoint's contract changes. P1-4's behavior change (cursor now works) is backward-compatible by construction (see Backward compatibility section).

**Deployment**: the retention worker runs inside the existing supervisor task — no new deployment unit, no new secret. Retention batch size/interval should be conservative on first deploy (small batches, longer interval) and tuned up once observed safe in production — this is a config-only rollback lever, not a code rollback.

**Rollback procedure**: each wave's changes are independently revertible (disjoint files per the Multi-Agent Workflow section) — if P1-6's deadline-tightening causes unexpected timeouts in production (e.g., a legitimately slow credential provider), the specific `tokio::time::timeout` wraps in `execution.rs` can be reverted independently of the pagination/retention/If-Match changes, which have no coupling to it. Migrations are additive-only and do not need a down-migration; disabling the retention worker is a one-line config flip (`WorkerSettings.enabled = false` or a dedicated per-spec toggle) without a deploy if settings are hot-reloadable, otherwise a fast redeploy.

**Deferred follow-ups**: leader-election-gating for the retention worker (plan 10); public API (`/v1/executions`, `/v1/usage`) cursor pagination using the same codec (optional extension, noted above); conversation/memory/RAG idempotency replay (optional extension, cross-referenced to plan 02).
