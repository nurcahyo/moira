# Plan 02b — Real Idempotency Replay for Conversation/Memory/RAG Routes

Companion to `00-audit-report.md`, `01-roadmap-and-dependencies.md`, and `CONVENTIONS.md`. Addresses **P0-2**, and closes one slice of **P0-5**.

> **Binding decision context.** `CONVENTIONS.md` §0 **D1**: P0-2 is fixed by **implementing real replay**, not by removing the `Idempotency-Key` parameter or rejecting with `501`. §0 **D2**: the work is split, with honesty shipping first in **`plans/02a-mvp-boundary-honesty.md`** and replay here. This plan **stacks on 02a** (`CONVENTIONS.md` §1 table and rule 1).

---

## Summary

**Objective.** Make the `Idempotency-Key` header that four RAG write routes already advertise actually true. Today `src/http/conversation.rs:665,847,954,984` declare the header in `#[utoipa::path]`, but `src/application/conversation.rs` contains **zero** references to `idempotency`, `ctx.idempotency_key`, `claim_idempotency`, or `finalize_idempotency` — the header is never read. A client that retries `POST /api/v1/admin/rag-documents/{id}/ingest` after a timeout gets a **second superseding version row**, not a replayed response: silent duplication, exactly the failure mode idempotency exists to prevent.

**The approach is reuse, not invention.** Moira already has a working, transactional idempotency envelope: `AdminCommandRunner` / `AdminCommandSpec` / `AdminCommandMutation` (`src/application/admin_command.rs`) over `PgAdminCommandTransaction::claim_idempotency` / `begin_command_savepoint` / `finalize_idempotency` (`src/infra/repositories/admin.rs:559-687`), proven by `tests/admin_idempotency.rs` (9 tests) across ten admin operations. This plan routes the three affected `ConversationService` methods through **that exact machinery**. It builds no parallel ledger, no second hashing scheme, and no new SQL for claiming.

**Why ordered here.** P0-2 is a release blocker and must land before plan 05's OpenAPI-drift gate freezes a spec that advertises the header (`01-roadmap-and-dependencies.md` §3: `I02A --> I02B --> I05`). It stacks directly on 02a so the interim window in which the spec advertises unimplemented replay is exactly one PR wide.

**User-visible outcome.**
- Retrying `POST /api/v1/admin/rag-collections`, `POST /api/v1/admin/rag-collections/{collection_id}/documents`, `POST /api/v1/admin/rag-documents/{id}/ingest`, or `POST /api/v1/admin/rag-documents/{id}/reindex` with the same `Idempotency-Key` and the same body returns the **original response verbatim** (same status, same body, same `ETag`) and performs **no second mutation**.
- The same key with a **different** body (or a different document/collection) returns `409` `idempotency_conflict`.
- A concurrent duplicate that arrives while the first is still executing returns `409` `idempotency_in_progress` — and that error now finally has an i18n catalog entry (it is emitted at `src/infra/repositories/admin.rs:576,610` today with **no** catalog entry, a live P0-5 violation).
- A deterministic failure (e.g. `404 rag_document_not_found`) is replayed as the same failure envelope rather than re-executed, matching the admin-command contract (`src/error.rs:180-196` `is_cacheable_admin_failure`).
- Requests **without** the header behave exactly as before.
- The OpenAPI parameter description stops disclaiming replay and starts describing it; each of the four operations gains an explicit documented `409`.

**Included scope.**
- Routing `ConversationService::create_rag_collection`, `create_rag_document`, and `ingest_rag_document` through `AdminCommandRunner`.
- A connection-taking refactor of the three corresponding `PgConversationRepository` write methods so their SQL can run inside the runner's transaction (mirroring the existing `insert_audit_with_connection` precedent).
- Moving those three operations' audit writes **inside** the transaction so a rolled-back mutation cannot leave an orphan audit row.
- `moira.error.idempotency_in_progress` catalog entry + `docs/i18n-response-catalog.json` mirror, and the `src/lib.rs` module wiring that makes the catalog compile at all.
- OpenAPI: truthful `Idempotency-Key` description, explicit `409` responses, deletion of 02a's interim disclaimer sentence.
- Docs: `docs/idempotency.md` gains the RAG operations; `docs/public-api.md` / `docs/conversation-memory-rag-api.md` lose their "replay not implemented" caveats.
- Unit **and** e2e test layers, including barrier-gated concurrency coverage.

**Excluded scope (explicitly deferred).**
- **No new routes gain idempotency.** Conversation and memory create routes (`POST /api/v1/conversations`, `.../messages`, `/api/v1/memories`) do **not** declare `Idempotency-Key` today and do not gain it here — adding it would be new API surface, not a P0-2 fix. Tracked as a follow-up.
- **No `If-Match` / optimistic concurrency** on RAG mutations. `AdminCommandSpec::with_expected_version` exists (`src/application/admin_command.rs:86-89`) and these handlers already emit `ETag`, but they accept no `If-Match` today; adding it is a separate contract change. `expected_version` stays `None`. Flagged in Deferred follow-ups.
- **No change to `runtime_admin.rs`'s idempotency scheme.** Its two-phase, non-transactional `idempotency_replay`/`record_idempotency` (`src/application/runtime_admin.rs:621-693`) is recorded as deferred debt by plan 06 (Risks §, item (b)); this plan neither adopts nor fixes it — see Architecture for why.
- **No RAG intelligence.** No chunking, embeddings, retrieval, or summarization — plan 11.
- **No ledger retention worker.** `idempotency_records` cleanup is P1-5 / plan 04; this plan adds rows that nothing prunes and says so honestly in Risks.
- No change to the `/v1/responses` idempotency path (`src/application/public.rs:125-138,1010-1105`).

### Branch & PR (binding — `plans/CONVENTIONS.md` §1)

**Branch:** `plan/02b-idempotency-replay`, **stacked on `plan/02a-mvp-boundary-honesty`** (not on `main`) per `CONVENTIONS.md` §1 table and rule 1: the dependency graph in `01-roadmap-and-dependencies.md` §3 has `I02A --> I02B`, and this plan edits OpenAPI descriptions, docs paragraphs, and a test file that 02a creates. Rule 1 obligations:
- The PR description **must name the base PR** (02a's) explicitly.
- The branch **must be rebased onto `main` once 02a merges**, and the PR retargeted from 02a's branch to `main`.
- 02a's branch must never be force-pushed while this branch is stacked on it (rule 7) — coordinate with the 02a owner.

**Commits:** Conventional Commits. Expected prefixes: `feat:` (replay wiring), `refactor:` (the `_with_connection` repository seam), `fix:` (the missing `idempotency_in_progress` catalog entry, the `pub mod i18n;` wiring), `docs:`, `test:`.

**PR must not open until every gate in `CONVENTIONS.md` §2 passes locally.**

**PR description — required sections (all seven, none omitted):**
1. **Plan link** — `plans/02b-idempotency-replay.md`.
2. **Findings addressed** — `P0-2`; partial `P0-5` (module wiring + the `idempotency_in_progress` entry). Name the base PR (02a) and state that P0-1/P0-3 land there.
3. **Migrations included** — **none.** Verified: `idempotency_records` already exists with the exact shape needed (`migrations/0003_security_foundation.sql:347-361`), including the unique index `idempotency_records_unique (idempotency_key_hash, actor_fingerprint, operation)` at `:360-361` and `resource_id varchar(256)`, which comfortably holds the `doc_…`/`collection_…` public ids these routes produce.
4. **Breaking API/OpenAPI changes** — no shape change; **behavioral** change on four routes (retry now replays instead of duplicating) plus three OpenAPI edits: parameter description, explicit `409` responses, removal of 02a's interim disclaimer sentence. Must land before plan 05's OpenAPI freeze (`CONVENTIONS.md` §1 rule 6).
5. **Test evidence** — both layers: the unit layer (naming the new functions in `src/application/conversation.rs`, `src/application/admin.rs`, `src/http/mod.rs`, `src/i18n/catalog/`) **and** the e2e layer (`tests/rag_idempotency_replay.rs` against real PostgreSQL 16 + pgvector, plus the amended `tests/rag_ingestion_honesty.rs`).
6. **Rollback procedure** — see Risks & Rollback (`git revert` of the merge commit; no migration; ledger rows become inert and expire within 24h).
7. **Deferred follow-ups** — `If-Match` on RAG mutations; replay for conversation/memory create routes; `runtime_admin.rs` scheme unification (plan 06); ledger retention (plan 04, P1-5); the remaining P0-5 catalog gaps (plans 05/06).

**Done means merged**, with every Definition of Done box verified by a named, passing test (`CONVENTIONS.md` §1 rule 5, §3).

---

## Findings Addressed

### P0-2 — `Idempotency-Key` advertised but unimplemented on conversation/memory/RAG
- `src/http/conversation.rs` declares `("Idempotency-Key" = Option<String>, Header, description = "Optional replay key")` at exactly four operations — verified by grep, four occurrences, no more, no fewer:
  - `:665` `create_rag_collection` → `POST /api/v1/admin/rag-collections`
  - `:847` `create_rag_document` → `POST /api/v1/admin/rag-collections/{collection_id}/documents`
  - `:954` `ingest_rag_document` → `POST /api/v1/admin/rag-documents/{id}/ingest`
  - `:984` `reindex_rag_document` → `POST /api/v1/admin/rag-documents/{id}/reindex`, which is a **direct call-through** to `ingest_rag_document` (`:993-1000`, body: `ingest_rag_document(State(state), headers, Path(id), Json(request)).await`)
- `src/application/conversation.rs` has **zero** references to `idempotency`, `ctx.idempotency_key`, `claim_idempotency`, or `finalize_idempotency`. `RequestContext.idempotency_key` (`src/application/context.rs:11,30`) *is* populated from the `idempotency-key` header on every request, and is consumed by `/v1/responses` and the admin routes — but the three `ConversationService` methods behind these four routes (`create_rag_collection` `:749-774`, `create_rag_document` `:861-898`, `ingest_rag_document` `:947-979`) never read it.
- Real replay exists only for `/v1/responses` (`src/application/public.rs:129-138` claim/replay call, `claim_idempotency` `:1010-1054`, `replay_idempotency` `:1056-1084`, `finish_idempotency` `:1086-1105`) and for the ten admin commands covered by `PgAdminCommandTransaction::claim_idempotency`/`finalize_idempotency` (`src/infra/repositories/admin.rs:559,657`) — asserted by `atomic_admin_idempotency_contract_is_explicit` (`src/http/mod.rs:534`) over an explicit route list that includes **no** RAG/conversation/memory route.
- **Impact:** a client that retries `POST .../rag-documents/{id}/ingest` with the same key because of a timeout gets a second superseding version row (the `UPDATE … superseded_at = coalesce(...)` at `src/infra/repositories/conversation.rs:1166-1176` runs again and a version N+1 is inserted at `:1177-1197`), not a replayed response.

### P0-5 (partial) — the i18n catalog is orphaned, and `idempotency_in_progress` has no entry
- `src/lib.rs:3-11` declares `app, application, config, domain, error, http, infra, orchestration, security` — there is **no `pub mod i18n;`**, so the entire `src/i18n/` tree is never compiled, `moira::i18n::is_known_key` is unreachable from `tests/`, and `clippy -D warnings` never sees the module.
- `src/domain/mod.rs:3`'s `mod i18n;` resolves to `src/domain/i18n.rs`, a **different** module path (`crate::domain::i18n`) — adding `pub mod i18n;` at the crate root is therefore not a name collision, only a new compilation unit.
- `moira.error.idempotency_conflict` **exists** (`src/i18n/catalog/errors.rs:25`). `moira.error.idempotency_in_progress` **does not** — the code is emitted at `src/infra/repositories/admin.rs:576` (advisory-lock timeout) and `:610` (unfinalized in-flight record), asserted by `tests/admin_idempotency.rs:854`, and declared in ten `#[utoipa::path]` 409 descriptions in `src/http/admin.rs`. Because `message_key` is derived mechanically as `format!("moira.error.{}", code())` (`src/error.rs:146-148`), every one of those responses ships a key that resolves to nothing.
- **This plan is the first code path to make that error reachable on the conversation surface**, so it must not propagate the gap. 02b adds the entry and the module wiring. Coordination note: plan 06 also lists `idempotency_in_progress` in its catalog-gap table (`06-architecture-test-hygiene.md:200`) and plan 04 Wave 0 owns the `pub mod i18n;` line — see Architecture → *Ordering relationship to plans 03, 04, and 06* for exactly who does what if the order shifts.

---

## Architecture

### Which existing pattern this follows, and why

Moira has **two** idempotency implementations plus a third on the public path. 02b follows **`AdminCommandRunner`** and explicitly rejects the alternative:

| Candidate | Where | Properties | Verdict for 02b |
|---|---|---|---|
| **`AdminCommandRunner`** (`src/application/admin_command.rs:143-243`) | ten admin create/rotate commands | **Single transaction**: claim, mutation-in-savepoint, finalize, and audit all commit or roll back together. Advisory-lock serialization. Canonicalized request-hash envelope including operation + path + expected_version. Deterministic-failure caching with a typed `StoredAdminReplay::{Success,Failure}` envelope replayed as `AppError::Replayed`. | **Chosen.** |
| `RuntimeAdminService::idempotency_replay` / `record_idempotency` (`src/application/runtime_admin.rs:621-693`) | runtime-policy PUTs | **Two-phase and non-transactional**: reads the ledger *before* the mutation, writes the record *after* it, with no advisory lock, no savepoint, no in-progress state, and **no `If-Match` support at all**. A crash between mutation and `record_idempotency` leaves the mutation applied and unrecorded; two concurrent requests can both pass the pre-read and both mutate. Plan 06 records this as deferred debt (`06-architecture-test-hygiene.md` Risks, item (b)). | **Rejected.** Copying it would import a known-defective pattern into a P0 fix. |
| `PublicResponseService::claim_idempotency` / `replay_idempotency` (`src/application/public.rs:1010-1105`) | `POST /v1/responses` | Claim-then-replay against the same `idempotency_records` table, but tuned to the long-running execution lifecycle (an unfinalized record yields `409 execution_in_progress`) and using `public_actor_fingerprint`, not the admin fingerprint. | **Precedent, not template.** These four routes are admin-authenticated and short-lived; the admin envelope fits better. Do not reuse `public_actor_fingerprint` here. |

Consequence to state in review: **02b introduces no new hashing, no new SQL for claiming, and no new ledger table.** Every idempotency primitive it uses already exists and is already tested.

### Components & ownership boundaries

- `src/lib.rs` — **one line**: `pub mod i18n;` (Wave 0; see the ordering section — may already be present if plan 04 Wave 0 landed first).
- `src/i18n/catalog/errors.rs` — the new `moira.error.idempotency_in_progress` entry.
- `docs/i18n-response-catalog.json` — its documentation mirror.
- `src/infra/repositories/conversation.rs` — **the repository seam**: connection-taking variants of the three write methods.
- `src/application/conversation.rs` — `ConversationService`: routes three methods through `AdminCommandRunner`; owns the new `conversation_command_spec` and `conversation_audit` helpers.
- `src/application/admin.rs` — visibility only: `actor_fingerprint` and (if reused) `success_audit` promoted to `pub(crate)`. **No behavior change.**
- `src/http/conversation.rs` — OpenAPI parameter/operation descriptions and explicit `409` responses. No handler-body change.
- `docs/idempotency.md`, `docs/public-api.md`, `docs/conversation-memory-rag-api.md`, `docs/todo.md` — documentation.

Nothing here crosses the Moira/Rig boundary — this is credentials/authz/persistence-adjacent orchestration only.

### The repository seam (the one genuinely new piece of engineering)

`AdminCommandRunner::execute` hands the mutation closure a `&mut PgAdminCommandTransaction` (`src/application/admin_command.rs:155-157`). That type exposes **`pub fn connection(&mut self) -> &mut PgConnection`** (`src/infra/repositories/admin.rs:259`), and the codebase already has a precedent for connection-taking repository functions: `insert_audit_with_connection` (`src/infra/repositories/admin.rs:1850`, called at `:690` from the transaction and at `:1695` from a pooled connection).

Apply that precedent to conversation writes. Extract the SQL bodies of the three methods into free functions in `src/infra/repositories/conversation.rs`:

```
create_rag_collection_with_connection(conn, id, public_id, request)          -> RagCollectionRecord
create_rag_document_with_connection(conn, id, public_id, collection_public_id, request, content_hash) -> RagDocumentRecord
ingest_rag_document_with_connection(conn, public_id, request, content_hash)  -> RagDocumentRecord
```

The existing pooled methods (`PgConversationRepository::create_rag_collection` `:879`, `create_rag_document` `:1013`, `ingest_rag_document` `:1138`) become thin wrappers that `self.pool.begin()`, delegate, and `commit()` — preserving every other caller byte-for-byte.

**Hard constraint (state it in review):** the `_with_connection` bodies must contain **no `begin()` and no `commit()`**. `create_rag_document` (`:1021`) and `ingest_rag_document` (`:1144`) open their own transaction today; under the runner an inner `begin()` would nest a transaction inside the runner's savepoint and silently break the rollback semantics that `begin_command_savepoint`/`rollback_command_savepoint` (`src/infra/repositories/admin.rs:636-655`) depend on. The `for update` row lock (`:1146`) and the version-supersession `UPDATE` (`:1166-1176`) move **into** the runner's transaction unchanged, which strengthens them: they now hold for the whole claim→finalize window rather than only for the inner transaction.

### Audit atomicity

`ConversationService::audit` (`src/application/conversation.rs:1000-1027`) writes through the **pooled** `admin_repo.insert_audit`, so today an audit row can survive a failed mutation. Inside the runner the audit must go through `transaction.insert_audit(...)` (`src/infra/repositories/admin.rs:689-691`), exactly as `src/application/admin.rs:233-242` does with `success_audit`.

**Grounded caveat — do not silently change the audit format.** `admin.rs`'s `success_audit` writes `actor_type: Some(format!("{:?}", actor.actor_type).to_ascii_lowercase())` (`src/application/admin.rs:1415`), whereas `ConversationService::audit` writes `Some(format!("{:?}", actor.actor_type))` **without** lowercasing (`src/application/conversation.rs:1012`). Reusing `success_audit` verbatim would silently alter the recorded `actor_type` casing for the RAG surface. 02b therefore adds a dedicated `conversation_audit(...) -> AuditLogInsert` builder in `src/application/conversation.rs` that reproduces today's field mapping **exactly**, and passes it to `transaction.insert_audit(...)`. The casing divergence is pre-existing debt: record it for plan 06, do not fix it here.

### Operation identities and the request-hash envelope

`AdminCommandSpec::request_hash()` (`src/application/admin_command.rs:96-107`) canonicalizes `{version, operation, path, request, expected_version}` and hashes it, so operation identity and path are inside the hash. Assign:

| Route | `operation` | `path` envelope | Status |
|---|---|---|---|
| `POST /api/v1/admin/rag-collections` | `rag.collection.create` | `json!({})` | 201 |
| `POST /api/v1/admin/rag-collections/{collection_id}/documents` | `rag.document.create` | `json!({ "collection_id": collection_id })` | 201 |
| `POST /api/v1/admin/rag-documents/{id}/ingest` | `rag.document.ingest` | `json!({ "document_id": document_id })` | 200 |
| `POST /api/v1/admin/rag-documents/{id}/reindex` | `rag.document.ingest` (**same**) | `json!({ "document_id": document_id })` (**same**) | 200 |

**Decision on `/reindex` (explicit, tested, documented).** `reindex_rag_document` is a literal call-through to `ingest_rag_document` (`src/http/conversation.rs:993-1000`) and performs an identical mutation. It therefore shares one operation identity and one path envelope. Consequence: `/reindex` sent with a key already used on `/ingest` **with the same body** replays the ingest response instead of creating version N+1. This is the correct semantics for two aliases of one mutation, but it is surprising enough to require (a) a named unit test, (b) a named e2e test, and (c) an explicit sentence in `docs/idempotency.md`. The rejected alternative — discriminating the two routes inside the `path` envelope — would make the same key on the two aliases produce `409 idempotency_conflict`, which is worse UX for no correctness gain.

### Claim, replay, and conflict semantics (inherited, not invented)

From `PgAdminCommandTransaction::claim_idempotency` (`src/infra/repositories/admin.rs:559-634`):
1. `pg_try_advisory_xact_lock(advisory_lock_key(key_hash, actor_fingerprint, operation))` in a 20 ms poll loop with a **5-second deadline**; on timeout → `409 idempotency_in_progress` (`:574-579`).
2. Expired records for the same `(key_hash, actor_fingerprint, operation)` are deleted (`:583-596`), so a key is reusable after `expires_at`.
3. An existing record with a **different `request_hash`** → `409 idempotency_conflict` (`:602-607`).
4. An existing record that is **unfinalized** (`response_status` or `response_body` null) → `409 idempotency_in_progress` (`:608-613`).
5. Otherwise → `Replay(record)`; else insert an unfinalized claim row and return `Acquired` (`:617-633`).

On the mutation result, `AdminCommandRunner::execute` (`:182-241`) releases the savepoint and `finalize_idempotency` on success; on a **cacheable** failure (`AppError::is_cacheable_admin_failure`, `src/error.rs:180-196`: 400/404/409/422 and not `Sqlx`/`Reqwest`/`Redis`/`Internal`/`Unauthorized`/`Forbidden`) it rolls back to the savepoint, stores a `StoredAdminReplay::Failure` envelope, commits the ledger, and returns the error — so a replay yields `AppError::Replayed` with the original `code`/`message_key`/`message`/`details` and a **fresh `request_id`** (`admin_command.rs:272-281`, `error.rs` `error_response`). On any other failure the whole transaction rolls back and nothing is recorded.

Note that `claim_idempotency`'s own 409s are raised **before** the savepoint, propagating via `?` at `admin_command.rs:173` — they are never themselves cached as replayable failures. Good; do not change this.

### Actor isolation

`AdminCommandIdempotency.actor_fingerprint` (`src/application/admin_command.rs:29-32`) is part of the ledger's unique index. Use the **admin** `actor_fingerprint` from `src/application/admin.rs:1374-1384` (a `secret_fingerprint` over actor type, subject, api-key id, and delegated subject) — these four routes authenticate via `admin_actor` (`src/http/conversation.rs:25-37`), not `public_actor`. **Promote that function to `pub(crate)` and import it; do not copy the formula** — a divergent second copy would silently break cross-actor isolation, the property `tests/admin_idempotency.rs:657` (`trusted_actor_fingerprint_isolates_issuer_and_application_identity`) exists to protect.

### Database/migration changes

**None — verified.** `idempotency_records` (`migrations/0003_security_foundation.sql:347-358`) already has `idempotency_key_hash varchar(128)`, `actor_fingerprint varchar(128)`, `operation varchar(128)`, `request_hash varchar(128)`, `response_status integer`, `response_body jsonb`, `resource_id varchar(256)`, `expires_at timestamptz`, with the unique index at `:360-361`. The new operation names (`rag.collection.create`, `rag.document.create`, `rag.document.ingest`) are ≤ 24 chars; the `resource_id` values are `collection_<uuid>` / `doc_<uuid>` public ids, ≤ 45 chars.

### API & OpenAPI changes

- `Idempotency-Key` parameter description on all four operations becomes truthful, e.g.: `"Optional replay key. A repeated request with the same key and body replays the original response; the same key with a different body returns 409."`
- Each of the four operations gains an explicit `(status = 409, description = "Idempotency conflict, or an identical request is still in progress", body = ErrorResponse)` response, matching the convention already used across ten `#[utoipa::path]` blocks in `src/http/admin.rs`. Keep the existing `"4XX"`/`"5XX"` catch-alls alongside, matching `src/http/conversation.rs`'s house style.
- **Delete 02a's Sentence B** (`"Idempotency-Key is accepted but replay is not implemented yet; retrying can duplicate side effects."`) from the four operation descriptions. **Keep 02a's Sentence A** (the persistence-primitive boundary) — it remains true, and 02a's `conversation_memory_rag_operations_document_the_mvp_preview_boundary` test asserts only Sentence A and must keep passing unchanged.
- No request/response body shape changes. No route added or removed.

### Backward compatibility

- Requests **without** `Idempotency-Key` are entirely unaffected (`spec.idempotency` is `None`, the runner skips claim/finalize; `admin_command.rs:160-180,186`).
- Requests **with** the header change behavior from "duplicate" to "replay". This is the intended fix, and per `CONVENTIONS.md` §0 D1 it is the decided direction. It is safe for correct clients (a retry was never *meant* to duplicate) but must be called out in PR section 4 because it is observable.
- Response shapes and status codes are unchanged on the success path.

### Ordering relationship to plans 03, 04, and 06

**Plan 03 (security hardening — versioned HMAC idempotency hashing, P1-1).**
- Plan 03 replaces the unkeyed `request_hash` with a versioned HMAC + pepper and, because `idempotency_key_hash` is the ledger's **lookup index key** (not merely a compared value), it must implement **dual-lookup** — trying both the new versioned hash and the legacy unkeyed hash when locating a record (`03-security-hardening.md` Backward compatibility, and `00-audit-report.md` P1-1).
- **02b adds zero new hashing call sites.** Both hashes it depends on are computed *inside* shared code plan 03 already owns: the key hash at `src/application/admin_command.rs:163` (`request_hash(idempotency.key.as_bytes())`) and the request hash at `:96-107` (`AdminCommandSpec::request_hash`). Migrating those two places automatically covers the three new operations — plan 03's call-site list does **not** need a new entry for 02b.
- **One thing plan 03 must be told:** `src/application/conversation.rs:876` and `:967` compute *content* hashes via `request_hash(content.as_bytes())`. These are two of the seven conversation-file sites already enumerated in plan 03 (`:286,352,395,455,546,876,967`). 02b **moves them inside the runner closure**, so their line numbers shift. 02b does not add, remove, or duplicate any of them. The 02b PR description must flag the line-number drift so plan 03's sweep is re-grounded rather than applied to stale coordinates.
- **Order:** the roadmap critical path is `02a → 02b → 03`. Either order is functionally safe because 02b touches no hashing code directly; if 03 unexpectedly lands first, 02b needs no change at all.

**Plan 04 (durability & correctness).**
- **Hard dependency:** plan 04 **Wave 0** adds `pub mod i18n;` to `src/lib.rs` (`04-durability-correctness.md` Wave 0 / DoD "`pub mod i18n;` is wired in `src/lib.rs` so the catalog actually compiles"). Until that line exists, `src/i18n/` is not compiled, `moira::i18n::is_known_key` is unreachable from `tests/`, and any catalog entry 02b adds is dead text.
- **Resolution (coordinator-visible, not hand-waved):** the roadmap places 02b **before** plan 04, so **02b owns the line** — Wave 0 of this plan adds `pub mod i18n;` and absorbs whatever `clippy --all-targets -- -D warnings` surfaces once the module is compiled for the first time. Plan 04's Wave 0 item then **degrades to a verification step** ("assert the line is present") rather than an addition. If, and only if, plan 04 Wave 0 has already merged when 02b starts, 02b's Wave 0 adds only the catalog entry and skips the `src/lib.rs` edit. Exactly one plan writes that line; the coordinator decides which by looking at `git log`.
- Plan 04 also owns the **retention worker** (P1-5) that prunes `idempotency_records`. 02b creates ledger rows for three new operations that nothing currently deletes — flagged in Risks, not solved here.
- Plan 04's text currently says "plan 02 removes the false `Idempotency-Key` advertisement (P0-2) for these routes" (`04-durability-correctness.md:214`). That sentence is **stale** under `CONVENTIONS.md` §0 D1/D2 and must be corrected by plan 04's owner to reference 02b implementing replay. 02b does not edit plan 04 (file ownership), but the coordinator must action this.

**Plan 06 (architecture & test hygiene).**
- Plan 06 lists `moira.error.idempotency_in_progress` in its catalog-gap table (`06-architecture-test-hygiene.md:200`) and adds `every_coded_error_literal_in_src_has_a_catalog_entry` (`:212`), the test that would have caught it.
- **02b adds the entry first** (it must, because 02b makes the error reachable on a newly-idempotent surface). Plan 06's obligation becomes: **do not add a duplicate**, and keep its generic every-code test — which will simply find 02b's entry already present. The coordinator must make plan 06's owner aware. 02b also must not touch plan 06's other catalog gaps (`database_unavailable`, `upstream_error`, `configuration_error`, `database_error`, `http_client_error`, `redis_error`, `capacity_exhausted`, `routing_policy_provider_model_mismatch`) or the `docs/i18n-response-catalog.json` duplicate-key cleanup — those stay plan 05/06 scope.
- Plan 06 also records `runtime_admin.rs`'s non-transactional scheme as debt; 02b's Architecture section above is the rationale plan 06 should cite when unifying.

### Deployment implications

None. No new config, no new dependency, no migration, no infra. Single-replica assumptions unchanged — and in fact the advisory-lock claim is already multi-replica-correct because `pg_try_advisory_xact_lock` is a database-level lock.

### Failure & recovery behavior

- Mid-request crash after claim, before finalize: the whole transaction (claim row included) rolls back — the key is free for a clean retry. This is strictly better than `runtime_admin.rs`'s two-phase scheme, where a crash leaves the mutation applied and unrecorded.
- Concurrent duplicate: advisory lock serializes; the loser either waits ≤ 5 s and then replays the finalized record, or receives `409 idempotency_in_progress`.
- Deterministic failure (404/409/422/400): cached and replayed identically.
- Infrastructure failure (`Sqlx`, `Internal`, …): not cached; the transaction rolls back and the key is reusable.

---

## Detailed Implementation

### 1. `src/lib.rs` — compile the i18n catalog (Wave 0, conditional)

Add `pub mod i18n;` to the module list at `:3-11`, in alphabetical position between `pub mod http;` and `pub mod infra;`. This is **not** a name collision with `src/domain/i18n.rs`, which lives at the distinct path `crate::domain::i18n` (`src/domain/mod.rs:3`).

**Skip this step entirely if plan 04 Wave 0 has already added the line** (check `git log`/`git blame` on `src/lib.rs` before editing). Exactly one plan writes it.

Expect first-compilation fallout: `src/i18n/` has never been seen by `cargo clippy --all-targets -- -D warnings`. Budget for dead-code/style warnings on `I18nEntry`, `RESPONSE_ERROR_CATALOG`, `RESPONSE_NOTICE_CATALOG`, `all_entries`, `default_message_for_key`, `is_known_key` (`src/i18n/mod.rs:1-6`). Fix them by making the items genuinely used (this plan's tests call `is_known_key`, which helps) rather than by blanket `#[allow(dead_code)]`. **If the fallout exceeds a reviewable diff, stop and hand the line back to plan 04**, keeping 02b's catalog entry and downgrading its catalog assertions to the wire-envelope form 02a used — record the downgrade in the PR rather than shipping an unverified claim.

### 2. `src/i18n/catalog/errors.rs` — the missing entry

Add:
```rust
I18nEntry {
    key: "moira.error.idempotency_in_progress",
    default_message: "An identical request with this Idempotency-Key is already being processed. Retry shortly.",
    description: "Used when a concurrent request holding the same Idempotency-Key has claimed the ledger record but has not finished, or when the advisory lock could not be acquired within the deadline.",
},
```
Insert it **adjacent to `moira.error.idempotency_conflict` (`src/i18n/catalog/errors.rs:25`)**, following whatever ordering the file actually uses — verify at implementation time whether entries are strictly alphabetical or grouped by domain (`idempotency_conflict` at `:25` and `idempotency_not_supported_for_stream` at `:125` are 100 lines apart, so the file is **not** strictly alphabetical) and match the surrounding convention rather than imposing a new one.

Do **not** add `moira.error.idempotency_conflict` — it already exists. Do not touch any other catalog gap; those belong to plans 05/06.

### 3. `docs/i18n-response-catalog.json` — the mirror

Add the identical `{key, default_message, description}` object to the `entries` array in the same relative position (`CONVENTIONS.md` §4 rule 4 — hand-synced until plan 06's drift test lands; drift is a review failure).

**Do not dedupe the file.** It currently has 63 entries / 61 unique, with `moira.error.idempotency_conflict` and `moira.error.rate_limited` each appearing twice (finding P0-5 / P2-8). Cleaning that up is plan 06's job; 02b's only obligation is to **not add a third** `idempotency_conflict` and to leave the count at 64/62.

### 4. `src/infra/repositories/conversation.rs` — the connection seam

Extract three free functions, each taking `conn: &mut PgConnection` as its first argument and containing **no `begin()`/`commit()`**:

- **`create_rag_collection_with_connection`** — body of `PgConversationRepository::create_rag_collection` (`:879-907`), with `.fetch_one(&self.pool)` → `.fetch_one(&mut *conn)`.
- **`create_rag_document_with_connection`** — body of `create_rag_document` (`:1013-1084`) minus its `let mut tx = self.pool.begin()` (`:1021`) and `tx.commit()`, with every `&mut *tx` → `&mut *conn`. Preserves: the collection lookup and its `rag_collection_not_found` error (`:1022-1034`), the document insert through `rag_document_select` (`:1035-1055`), the conditional version-1 insert (`:1056-1074`, whose `'indexed'` literal 02a already replaced with a bound `pending`), the `current_version_id` update (`:1075-1079`), and 02a's post-version response-row re-select.
- **`ingest_rag_document_with_connection`** — body of `ingest_rag_document` (`:1138-1211`) minus `begin`/`commit`. Preserves verbatim: the `for update` document lookup (`:1145-1157`), the `max(version_number) + 1` computation (`:1158-1163`), the supersession `UPDATE` (`:1166-1176`), the version insert (`:1177-1197`), the `current_version_id` update (`:1198-1202`), and the response-row re-select (`:1203-1208`).

Rewrite the three existing pooled methods as wrappers: `let mut tx = self.pool.begin().await?; let record = <fn>(&mut tx, …).await?; tx.commit().await?; Ok(record)`. Every existing caller keeps working unchanged.

**Review invariant:** `grep -n 'begin()' src/infra/repositories/conversation.rs` must show the `begin()` calls only in the wrapper methods, never in a `_with_connection` function.

### 5. `src/application/admin.rs` — visibility only

Promote `fn actor_fingerprint(actor: &Actor) -> String` (`:1374-1384`) to `pub(crate) fn`. No body change, no call-site change in `admin.rs`. Do **not** promote or reuse `success_audit` (`:1405+`) — see §6 for why the conversation surface needs its own builder.

### 6. `src/application/conversation.rs` — route three methods through the runner

**(a) Helpers.** Add, next to the existing private `audit` (`:1000-1027`):

```rust
fn conversation_command_spec<T: Serialize>(
    ctx: &RequestContext,
    actor: &Actor,
    operation: &str,
    path: Value,
    request: &T,
) -> Result<AdminCommandSpec, AppError>
```
mirroring `admin_command_spec` (`src/application/admin.rs:1386-1403`) exactly: build the spec, then `with_idempotency(ctx.idempotency_key.as_ref().map(|key| AdminCommandIdempotency { key: key.clone(), actor_fingerprint: crate::application::admin::actor_fingerprint(actor) }))`. Leave `expected_version` at its default `None`.

```rust
fn conversation_audit(
    actor: &Actor, ctx: &RequestContext, action: &str,
    resource_type: &str, resource_id: Option<String>, metadata: Value,
) -> AuditLogInsert
```
reproducing the field mapping of today's `Self::audit` **exactly**, including the **non-lowercased** `actor_type: Some(format!("{:?}", actor.actor_type))` (`:1012`). Refactor the existing `Self::audit` to call this builder so there is one mapping, not two.

**(b) `create_rag_collection` (`:749-774`).** Keep the `moira:rag-collections:write` authz check and `validate_metadata` **outside** the runner (they are cheap, deterministic, and should fail before any ledger row is claimed — matching `src/application/admin.rs:204-205` where authz precedes `admin_command_spec`). Then:

```rust
let spec = conversation_command_spec(ctx, actor, "rag.collection.create", json!({}), &request)?;
let outcome = AdminCommandRunner::new(self.admin_repo.clone())
    .execute(spec, |transaction| Box::pin(async move {
        let id = Uuid::now_v7();
        let record = create_rag_collection_with_connection(
            transaction.connection(), id, &format!("collection_{id}"), &request).await?;
        transaction.insert_audit(conversation_audit(
            &actor, &ctx, "rag.collection.created", "rag_collection",
            Some(record.id.clone()),
            json!({ "application_id": record.application_id }))).await?;
        AdminCommandMutation::new(record.clone(), 201, Some(record.id.clone()))
    }))
    .await?;
Ok(outcome.response)
```
Note `Uuid::now_v7()` moves **inside** the closure so a replayed request never burns an id, and `record.id` is the `String` public id (`collection_…`), so `resource_id` is `Some(record.id.clone())`, not `.to_string()` of a `Uuid`. Clone `actor`/`ctx` before the closure, as `src/application/admin.rs:206-207` does.

**(c) `create_rag_document` (`:861-898`).** Same shape. Authz (`moira:rag-documents:write`), `validate_metadata`, and `validate_document` stay outside. The **content hash** (`request_hash(content.as_bytes())`, `:876`) moves inside the closure alongside the repository call — it is an input to the mutation, not to the idempotency envelope. `operation = "rag.document.create"`, `path = json!({ "collection_id": collection_id })`, status `201`.

**(d) `ingest_rag_document` (`:947-979`).** Authz (`moira:rag-documents:ingest`), the missing-content `rag_document_parse_failed` check (`:957-962`), `validate_content`, and `validate_metadata` stay outside the runner. The content hash (`:967`) moves inside. `operation = "rag.document.ingest"`, `path = json!({ "document_id": document_id })`, status `200`. `reindex_rag_document` needs **no application-layer change** — it call-throughs to the same handler and therefore to this same method.

**(e) Untouched.** `list_*`, `get_*`, `patch_*`, `set_rag_collection_status`, `delete_rag_document`, and every conversation/memory/policy method keep their current pooled implementations. Do not opportunistically wrap them.

### 7. `src/http/conversation.rs` — OpenAPI truth

For each of the four operations (`create_rag_collection` `:660-672`, `create_rag_document` `:840-855`, `ingest_rag_document` `:947-962`, `reindex_rag_document` `:977-992`):
- Replace the `Idempotency-Key` parameter description (02a set it to a "not implemented yet" string) with the truthful one from Architecture → *API & OpenAPI changes*.
- Add the explicit `409` response entry to `responses(...)`.
- Delete 02a's Sentence B from the operation description; keep Sentence A verbatim.

Handler bodies do not change. No parameter is added or removed — `grep -c '("Idempotency-Key' src/http/conversation.rs` stays at `4`.

> **Guard amended (issue #82).** Originally `grep -c 'Idempotency-Key' …`. Both forms return `4`
> against the current tree, so the guard was never broken. The anchored form is used because this
> very section rewrites the operation *descriptions*, and an unanchored count would rise or fall
> with prose that mentions the header rather than with the parameter declarations the guard exists
> to protect. The expected value is unchanged.

### 8. Documentation

- **`docs/idempotency.md`** — replace 02a's "Conversation, memory, and RAG endpoints do **not** replay today…" paragraph with the real contract: add the three new operations to the operation table (`rag.collection.create`, `rag.document.create`, `rag.document.ingest`), state the 24-hour retention window (`IDEMPOTENCY_RETENTION_HOURS`, `src/application/admin_command.rs:17`), the `409 idempotency_conflict` / `409 idempotency_in_progress` semantics, and — explicitly — that `/reindex` shares `rag.document.ingest` with `/ingest` so the same key replays across the two aliases. Note that conversation/memory create routes still do not support replay.
- **`docs/public-api.md`** — delete the fourth bullet of 02a's MVP-boundary section (the `Idempotency-Key` caveat) and replace it with a one-line statement that the RAG create/ingest/reindex routes now replay, pointing at `docs/idempotency.md`. **Leave the other three bullets** (no pipeline, no context injection, no summarization) untouched — they remain true.
- **`docs/conversation-memory-rag-api.md`** — update the "does not do" column of 02a's table: remove "replay", keep "retrieve / inject / summarize".
- **`docs/todo.md`** — annotate the Phase 2 idempotency-extension line (`:22`) as satisfied for the RAG slice by this plan, still open for runtime-policy/conversation/memory. Do not delete or renumber lines. **Line reference is historical:** `docs/todo.md` gained a marker legend under issue #82, so the line has moved. Find it by its text — the Phase 2 bullet beginning "Extend atomic idempotency and sanitized deterministic-failure replay" — not by number.

### 9. Tests — **both layers are mandatory** (`plans/CONVENTIONS.md` §3)

#### 9a. Unit layer — no database, `#[cfg(test)] mod tests` beside the code

| File | New test function | Asserts |
|---|---|---|
| `src/application/conversation.rs` (new `#[cfg(test)] mod tests`) | `conversation_command_hash_is_stable_across_object_key_order` | mirroring `admin_command.rs:327`: two specs built from the same logical request with different JSON key order hash identically — the canonicalization the replay contract depends on |
| `src/application/conversation.rs` | `conversation_command_hash_covers_operation_and_path` | `rag.document.ingest` on document A ≠ document B; `rag.collection.create` ≠ `rag.document.create` for the same body — proves both isolation dimensions are inside the hash envelope |
| `src/application/conversation.rs` | `ingest_and_reindex_share_one_operation_and_request_envelope` | pins the `/reindex` decision: specs built for the two routes with the same document and body produce the **same** `operation` and the **same** `request_hash` |
| `src/application/conversation.rs` | `conversation_command_spec_omits_idempotency_when_no_key_is_present` | `ctx.idempotency_key == None` ⇒ the spec carries no `AdminCommandIdempotency`, so the no-key path claims nothing |
| `src/application/conversation.rs` | `conversation_audit_preserves_the_existing_actor_type_casing` | `conversation_audit` writes the non-lowercased `actor_type` — guards the pre-existing format from a silent change when the audit moves into the transaction |
| `src/application/admin.rs` | `actor_fingerprint_is_shared_by_admin_and_conversation_commands` | one actor yields one fingerprint through the single promoted `pub(crate)` function — guards against a divergent copy being introduced in `conversation.rs` |
| `src/http/mod.rs` (existing `mod tests` at `:213`; helper `parameter_named` at `:653`) | `rag_write_routes_declare_the_idempotency_replay_contract` | on all four operations: `parameter_named(op, "Idempotency-Key")` is `true`, the parameter description no longer contains "not implemented", and an explicit `409` response is declared. A sibling to `atomic_admin_idempotency_contract_is_explicit` (`:534`) — kept separate because these routes are not admin-command routes |
| `src/http/mod.rs` | `rag_write_route_descriptions_no_longer_disclaim_idempotency` | 02a's Sentence B is gone from all four descriptions while Sentence A survives — the paired half of 02a's `rag_write_routes_carry_the_interim_idempotency_disclaimer`, which this plan deletes |
| `src/i18n/catalog/mod.rs` (existing `mod tests`) | `idempotency_in_progress_key_is_catalogued` | `is_known_key("moira.error.idempotency_in_progress")` is `true`, and the entry's `default_message` and `description` are both non-empty (`CONVENTIONS.md` §4 rules 1 and 5) |
| `src/i18n/catalog/mod.rs` | `idempotency_keys_are_catalogued_exactly_once` | `moira.error.idempotency_conflict` and `moira.error.idempotency_in_progress` each appear exactly once in `RESPONSE_ERROR_CATALOG` — guards against a duplicate when plan 06 later sweeps the catalog |

#### 9b. E2E layer — new file `tests/rag_idempotency_replay.rs`

Follows `tests/admin_idempotency.rs` exactly — that file is the closest exemplar: `mod support;`, a fixture over real PostgreSQL 16 + pgvector, a `post(router, path, key, if_match, body)` helper (`:168-211`) driving `router.oneshot(...)`, `assert_error` (`:223`) and `assert_replayed_error` (`:228`) helpers, and direct ledger/`audit_logs` SQL for assertions. Inherits the harness fail-closed rule: `MOIRA_TEST_DATABASE_URL` absent under `CI` ⇒ `panic!` (`tests/support/mod.rs:427-441`); absent locally ⇒ skip with a printed reason.

| Test function | Asserts |
|---|---|
| `repeated_rag_collection_create_with_the_same_key_replays_one_collection` | two identical `POST /api/v1/admin/rag-collections` with one key → identical `201` body **and** identical `ETag`; `select count(*) from rag_collections where …` is `1`; exactly one `audit_logs` row for `rag.collection.created` |
| `repeated_rag_document_create_with_the_same_key_replays_one_document` | same for `POST .../{collection_id}/documents`, including exactly one version-1 row when the create carries inline `content` |
| `repeated_ingest_with_the_same_key_replays_and_creates_exactly_one_version` | **the inversion of 02a's characterization test** — `select count(*) from rag_document_versions where document_id = …` is `1`, response bodies byte-identical, `ingestion_status` still `"pending"` (02a's contract not regressed) |
| `reindex_replays_an_ingest_performed_under_the_same_key` | pins the `/reindex` decision at the wire level: `POST /ingest` with key K, then `POST /reindex` with key K and the same body → replayed body, still one version row |
| `same_key_with_a_different_body_returns_idempotency_conflict` | `409`; `error.code == "idempotency_conflict"`; `error.message_key == "moira.error.idempotency_conflict"`; non-empty `error.message`; `moira::i18n::is_known_key(message_key)` is `true`; and **no second version row** |
| `same_key_on_a_different_document_returns_idempotency_conflict` | proves `path` is inside the hash envelope, not just the body |
| `different_actors_with_the_same_key_do_not_replay_each_others_responses` | actor-fingerprint isolation, mirroring `tests/admin_idempotency.rs:657` — two distinct admin actors, one key, two independent resources |
| `concurrent_same_key_ingests_produce_one_version_and_one_audit_row` | **`tokio::sync::Barrier` acknowledgement gate**, mirroring the `Barrier::new(3)` pattern at `tests/admin_idempotency.rs:518,1028,1128` and its `spawn_post` helper (`:1197`). **Never `sleep()`** (`CONVENTIONS.md` §3; finding P2-12). Exactly one `rag_document_versions` row, exactly one `rag.document.ingested` audit row, and exactly one ledger row; the losing request either replays the identical body or returns `409 idempotency_in_progress` |
| `in_progress_claim_returns_a_catalogued_idempotency_in_progress_error` | forces the unfinalized-record branch (`src/infra/repositories/admin.rs:608-613`) deterministically by inserting an unfinalized `idempotency_records` row for the computed `(key_hash, actor_fingerprint, operation)` before issuing the request — reusing the `advisory_lock_key` / fingerprint helpers the exemplar already has (`tests/admin_idempotency.rs:1275`). Asserts `409`, `code == "idempotency_in_progress"`, `message_key == "moira.error.idempotency_in_progress"`, non-empty `message`, and `moira::i18n::is_known_key(message_key)`. **This is the test that closes 02b's slice of P0-5** |
| `failed_ingest_replays_the_same_deterministic_failure` | ingest against a non-existent document id with key K → `404 rag_document_not_found`; retry with K → identical envelope via `assert_replayed_error` (fresh `request_id`, same `code`/`message_key`/`message`), exercising `is_cacheable_admin_failure` (`src/error.rs:180-196`) and `StoredAdminReplay::Failure` |
| `rolled_back_ingest_leaves_no_audit_row_and_no_partial_version` | atomicity of the moved audit write: a mutation that fails after the audit insert leaves **zero** `audit_logs` rows and zero new version rows — the property that only holds because the audit moved inside the transaction (§6) |
| `expired_ledger_record_is_purged_and_the_request_re_executes` | mirrors `tests/admin_idempotency.rs:717`; back-date `expires_at`, re-send the same key, confirm the expired-purge path (`src/infra/repositories/admin.rs:583-596`) runs and a second version is legitimately created |
| `requests_without_an_idempotency_key_still_create_new_versions` | no-key regression guard: two keyless ingests still produce two versions, and **no** `idempotency_records` row is written |

#### 9c. Amendments to 02a's e2e file

`tests/rag_ingestion_honesty.rs` — **delete** `repeated_ingest_with_the_same_idempotency_key_creates_two_versions_until_02b` (02a shipped it as an explicitly interim characterization test; its inverse now lives in `tests/rag_idempotency_replay.rs`). **Upgrade** `rag_document_error_responses_carry_catalog_message_keys` to also assert `moira::i18n::is_known_key(message_key)`, now that Wave 0 has made the catalog reachable — 02a could only assert the wire envelope. Leave every other 02a test untouched and passing.

#### 9d. Not covered by automated tests

The four documentation files in §8 have no automated assertion (markdown drift is plan 06's gate). Verify cross-references manually and record the check in the PR's Test evidence section.

---

## Multi-Agent Workflow

**Coordinator responsibilities.** Confirm 02a is merged (or that this branch is correctly stacked on it) before dispatch; decide the `pub mod i18n;` ownership question in Wave 0 by inspecting `git log` for plan 04 Wave 0; verify disjoint file sets per wave; run `cargo fmt --check` / `cargo clippy --workspace --all-targets --all-features -- -D warnings` / `cargo test` after each wave; hold the e2e concurrency test as the merge gate.

### Wave 0 (blocking, single agent) — make the catalog real
`src/lib.rs` (the `pub mod i18n;` line, conditional per §1), `src/i18n/catalog/errors.rs` (the new entry), `docs/i18n-response-catalog.json` (the mirror), plus any clippy fallout **inside `src/i18n/` only**. Blocking because every later wave's catalog assertion depends on the module compiling. If the clippy fallout is not reviewable in a small diff, this agent stops and escalates to the coordinator per §1's fallback.

### Wave 1 (sequential, single agent) — the repository seam
`src/infra/repositories/conversation.rs` only: the three `_with_connection` extractions plus the wrapper rewrites (§4). Sequential and alone because every consumer in Wave 2 depends on the new signatures, and because a stray `begin()` here is the single highest-risk defect in the plan.

### Wave 2 (sequential after Wave 1, single agent) — the application seam
`src/application/conversation.rs` **and** `src/application/admin.rs` (§5, §6). One agent, not two: the `pub(crate)` visibility change and its only consumer must land together, and splitting them across writers invites a duplicated fingerprint formula — the exact defect §5 forbids.

### Wave 3 (parallel, disjoint files)
- **Agent D — HTTP/OpenAPI:** `src/http/conversation.rs` (§7). No other file.
- **Agent E — docs:** `docs/idempotency.md`, `docs/public-api.md`, `docs/conversation-memory-rag-api.md`, `docs/todo.md` (§8). No source files.

### Wave 4 (parallel, disjoint files) — **two test layers, both mandatory**
- **Agent F — unit layer (§9a):** `src/application/conversation.rs`, `src/application/admin.rs`, `src/http/mod.rs`, `src/i18n/catalog/mod.rs`. No database.
- **Agent G — e2e layer (§9b, §9c):** the new `tests/rag_idempotency_replay.rs` **and** the amendments to `tests/rag_ingestion_honesty.rs`. Requires real PostgreSQL 16 + pgvector. Owns the `Barrier`-gated concurrency test — a `sleep()`-based interleaving is rejected in Wave 5.

F and G are file-disjoint (F re-enters `src/application/*.rs` only inside `#[cfg(test)] mod tests` blocks that Wave 2 did not create; if that overlap is uncomfortable, run F after Wave 3 sequentially rather than concurrently with G). **Both** must land before the PR opens.

### Wave 5 — read-only reviewer
Confirm: (1) `grep -c '("Idempotency-Key' src/http/conversation.rs` is still `4` — nothing removed (`CONVENTIONS.md` §0 D1; guard anchored on the `utoipa` parameter tuple per issue #82, expected value unchanged); (2) no `begin()`/`commit()` inside any `_with_connection` function; (3) `actor_fingerprint` has exactly one definition in the crate (`grep -rn 'fn actor_fingerprint(' src/` — one match, `src/application/admin/shared.rs:344`); (4) no route added/removed from `src/http/mod.rs`'s router table; (5) no plan-11 scope (`rag_chunks`, `rag_chunk_embeddings`, `conversation_summaries` absent from the diff); (6) no new `request_hash(` call sites beyond relocating the two existing conversation content-hash sites (`:876`, `:967`), so plan 03's sweep stays a one-place change; (7) `docs/i18n-response-catalog.json` gained exactly one entry and no duplicate key; (8) no `sleep()` in any new test.

> **Guard (3) amended (issue #82).** As originally written — `grep -rn 'fn actor_fingerprint' src/`
> — the check is **stale on arrival** and now returns `5`, not `1`. Commit `27fe021` unified the
> fingerprint and, in doing so, added four unit tests named `actor_fingerprint_*`
> (`src/application/admin/shared.rs:1063,1130,1147,1183`) that the unanchored pattern also matches.
> The property the guard is asserting — one definition — is still true; only the pattern was wrong.
> Anchoring on the opening paren counts definitions and not test names.
>
> **File paths in this section are historical.** `src/application/admin.rs` no longer exists; plan
> 06 split it into the module directory `src/application/admin/`. Read it as that directory.

**Conflict-avoidance strategy.** `src/infra/repositories/conversation.rs` is owned solely by Wave 1; `src/application/conversation.rs` and `src/application/admin.rs` solely by Wave 2 (production code) and Wave 4 Agent F (test modules); `src/http/conversation.rs` solely by Wave 3 Agent D. No two agents in the same wave share a file.

---

## Interfaces & Contracts

**Endpoints affected (no route added/removed, no path/method/parameter added or removed):**

| Method | Path | Change |
|---|---|---|
| POST | `/api/v1/admin/rag-collections` | **Behavior:** replays under `Idempotency-Key`. OpenAPI: truthful param description, explicit `409`, Sentence B removed |
| POST | `/api/v1/admin/rag-collections/{collection_id}/documents` | same |
| POST | `/api/v1/admin/rag-documents/{id}/ingest` | same |
| POST | `/api/v1/admin/rag-documents/{id}/reindex` | same; shares the `rag.document.ingest` operation identity with `/ingest` |

**Request/response shapes.** Unchanged. A replayed response is the **stored serialization of the original `RagCollectionRecord` / `RagDocumentRecord`**, decoded through `serde_json::from_value` in `replay_record` (`src/application/admin_command.rs:245-294`) — which is why both DTOs' `Deserialize` derives (`src/domain/conversation.rs:520,577`) are load-bearing and must not be removed. The `ETag` header replays consistently because the handlers derive it from `record.version` (`etag_headers(record.version)`), which is inside the replayed body.

**Status codes.**

| Condition | Status | Code |
|---|---|---|
| First request, success | `201` (creates) / `200` (ingest, reindex) | — |
| Same key + same body, after completion | **the original status**, replayed | — |
| Same key + different body / different path | `409` | `idempotency_conflict` |
| Same key while the first is in flight (lock timeout or unfinalized record) | `409` | `idempotency_in_progress` |
| Same key, original request failed deterministically (400/404/409/422) | **the original status**, replayed as `AppError::Replayed` | the original code |
| No key | unchanged | — |

**Headers.** `Idempotency-Key` is read from `RequestContext::from_headers` (`src/application/context.rs:30`, header name `idempotency-key`, empty values filtered out). No new request or response header. `If-Match` is **not** introduced (Deferred follow-ups).

**Scopes/authorization rules.** Unchanged: `moira:rag-collections:write`, `moira:rag-documents:write`, `moira:rag-documents:ingest`. Authorization is evaluated **before** any ledger claim, so an unauthorized request never occupies a key.

**Error codes & i18n message keys** (binding: `plans/CONVENTIONS.md` §4).

| Code | Status | Catalog key | State |
|---|---|---|---|
| `idempotency_conflict` | 409 | `moira.error.idempotency_conflict` | **Already exists** (`src/i18n/catalog/errors.rs:25`). Newly reachable from these routes; asserted, not added |
| `idempotency_in_progress` | 409 | `moira.error.idempotency_in_progress` | **Added by this plan** (§2) + mirrored into `docs/i18n-response-catalog.json` (§3) + asserted by `idempotency_in_progress_key_is_catalogued` and by the e2e `in_progress_claim_returns_a_catalogued_idempotency_in_progress_error` |

`message_key` is derived mechanically as `format!("moira.error.{}", code())` (`src/error.rs:146-148`) into `ErrorDetail { code, message_key, message, message_args, request_id, details }` (`src/error.rs:52-65`), so no handler-side plumbing is needed for either key.

**No new `moira.notice.*` entries.** A replayed response is a byte-identical copy of an already-catalogued original; this plan emits no new human-readable prose in any response body (`CONVENTIONS.md` §4 rule 2). The OpenAPI description text is spec metadata, not a payload.

**Idempotency behavior.** This is the plan's subject; fully specified in Architecture → *Claim, replay, and conflict semantics* and in the status-code table above. Retention is 24 hours (`IDEMPOTENCY_RETENTION_HOURS`, `src/application/admin_command.rs:17`); after `expires_at` the record is purged on the next claim and the key is legitimately reusable.

**Transaction boundaries.** Each of the three operations now runs in **one** transaction opened by `AdminCommandRunner::execute` via `begin_admin_command` (`src/application/admin_command.rs:159`), containing: advisory lock → expired-record purge → claim insert → `savepoint admin_command_mutation` → repository SQL (including the `for update` document lock) → audit insert → `release savepoint` → `finalize_idempotency` → `commit`. The previously-inner transactions in `create_rag_document`/`ingest_rag_document` are subsumed, not nested.

**Cache invalidation.** Not applicable — RAG collections/documents are not part of `RuntimeConfigCache`, so unlike `src/application/admin.rs:247-249` there is no `schedule_runtime_cache_invalidation()` to guard behind `!outcome.replayed`. Confirm this at implementation time by grepping the invalidation call sites; if a RAG-related invalidation is ever added, it **must** be gated on `!outcome.replayed`.

**Concurrency behavior.** Serialized per `(key_hash, actor_fingerprint, operation)` by `pg_try_advisory_xact_lock` with a 20 ms poll and 5-second deadline (`src/infra/repositories/admin.rs:563-581`). Requests with different keys, different actors, or different operations do not contend. Requests with no key do not take the lock at all.

**SSE behavior.** Not applicable — none of these routes stream.

---

## Verification

**Both test layers are required to merge** (`plans/CONVENTIONS.md` §3). Full breakdown in Detailed Implementation §9.

**Layer 1 — unit (`#[cfg(test)] mod tests`, no database).** In `src/application/conversation.rs`, `src/application/admin.rs`, `src/http/mod.rs`, `src/i18n/catalog/mod.rs`:
`conversation_command_hash_is_stable_across_object_key_order`, `conversation_command_hash_covers_operation_and_path`, `ingest_and_reindex_share_one_operation_and_request_envelope`, `conversation_command_spec_omits_idempotency_when_no_key_is_present`, `conversation_audit_preserves_the_existing_actor_type_casing`, `actor_fingerprint_is_shared_by_admin_and_conversation_commands`, `rag_write_routes_declare_the_idempotency_replay_contract`, `rag_write_route_descriptions_no_longer_disclaim_idempotency`, `idempotency_in_progress_key_is_catalogued`, `idempotency_keys_are_catalogued_exactly_once`.

**Layer 2 — e2e / integration (real HTTP surface, real PostgreSQL 16 + pgvector).** In the new `tests/rag_idempotency_replay.rs`, following `tests/support/mod.rs` and imitating `tests/admin_idempotency.rs`:
`repeated_rag_collection_create_with_the_same_key_replays_one_collection`, `repeated_rag_document_create_with_the_same_key_replays_one_document`, `repeated_ingest_with_the_same_key_replays_and_creates_exactly_one_version`, `reindex_replays_an_ingest_performed_under_the_same_key`, `same_key_with_a_different_body_returns_idempotency_conflict`, `same_key_on_a_different_document_returns_idempotency_conflict`, `different_actors_with_the_same_key_do_not_replay_each_others_responses`, `concurrent_same_key_ingests_produce_one_version_and_one_audit_row`, `in_progress_claim_returns_a_catalogued_idempotency_in_progress_error`, `failed_ingest_replays_the_same_deterministic_failure`, `rolled_back_ingest_leaves_no_audit_row_and_no_partial_version`, `expired_ledger_record_is_purged_and_the_request_re_executes`, `requests_without_an_idempotency_key_still_create_new_versions`.

Plus the §9c amendments to `tests/rag_ingestion_honesty.rs`.

**Concurrency discipline.** `concurrent_same_key_ingests_produce_one_version_and_one_audit_row` uses a `tokio::sync::Barrier` acknowledgement gate (the pattern at `tests/admin_idempotency.rs:518,1028,1128` with its `spawn_post` helper at `:1197`). **No `sleep()`-based interleaving anywhere in this plan's tests** — it is rejected in Wave 5 review (`CONVENTIONS.md` §3; finding P2-12).

**i18n verification** (`CONVENTIONS.md` §4 rule 5). Both halves are discharged with named tests: presence via `idempotency_in_progress_key_is_catalogued` and `idempotency_keys_are_catalogued_exactly_once`; live-response non-emptiness plus catalog resolution via `in_progress_claim_returns_a_catalogued_idempotency_in_progress_error` and `same_key_with_a_different_body_returns_idempotency_conflict`, both of which assert `moira::i18n::is_known_key(message_key)`. This is the **first** place in the repository where `is_known_key` is called by anything other than the orphaned module itself (verified: zero callers in `src/` or `tests/` today).

- **No regression** in `tests/admin_idempotency.rs` (9 tests), `tests/execution_lifecycle.rs` (14), `tests/public_authorization.rs`, `tests/http_error_contract.rs`, or 02a's `tests/rag_ingestion_honesty.rs` (minus the one deliberately deleted test).
- Migration validation: **N/A** — no migration; still run the clean-PostgreSQL gate to confirm no accidental schema drift.
- Secret-leak: `src/security/masking::tests` passes unchanged. Additionally confirm the ledger stores no secret material — these three operations' request bodies (`RagCollectionCreateRequest`, `RagDocumentCreateRequest`, `RagDocumentIngestRequest`) carry document content and metadata, **not** credentials, so the `response_body` stored for replay contains no secret. Unlike the credential-create/rotate admin commands, no `with_replay_response` sanitization (`src/application/admin_command.rs:126-140`) is needed — state this explicitly in review rather than assuming it.
- Required Rust gates, run verbatim and must pass clean:
  - `cargo fmt --check`
  - `cargo clippy --workspace --all-targets --all-features -- -D warnings` (**expect first-time warnings from the newly-compiled `src/i18n/` tree**; they must be fixed, not suppressed wholesale)
  - `cargo test --workspace --all-features`
  - clean PostgreSQL migration validation from an empty database
  - `cargo build --release --locked`

---

## Definition of Done

- [ ] All four routes replay: proven by `repeated_rag_collection_create_with_the_same_key_replays_one_collection`, `repeated_rag_document_create_with_the_same_key_replays_one_document`, `repeated_ingest_with_the_same_key_replays_and_creates_exactly_one_version`, and `reindex_replays_an_ingest_performed_under_the_same_key` — each asserting both the replayed response **and** a database row count of exactly one.
- [ ] `src/application/conversation.rs` reads `ctx.idempotency_key` and routes the three write methods through `AdminCommandRunner` — verified by `grep -n 'AdminCommandRunner' src/application/conversation.rs` returning the three call sites, and by the unit tests on `conversation_command_spec`.
- [ ] **No parallel idempotency system was built:** `grep -rn 'idempotency_records' src/application/conversation.rs src/infra/repositories/conversation.rs` returns nothing — all ledger access goes through `PgAdminCommandTransaction`.
- [ ] No `_with_connection` function in `src/infra/repositories/conversation.rs` contains `begin()` or `commit()` (Wave 5 check 2).
- [ ] `actor_fingerprint` has exactly **one** definition in the crate (Wave 5 check 3) — no divergent copy.
- [ ] Audit writes for the three operations are inside the transaction, proven by `rolled_back_ingest_leaves_no_audit_row_and_no_partial_version`.
- [ ] `moira.error.idempotency_in_progress` exists in `src/i18n/catalog/errors.rs` with a non-empty English `default_message` **and** `description`, is mirrored in `docs/i18n-response-catalog.json` exactly once, and is asserted by both a unit test and a live-response e2e test.
- [ ] `pub mod i18n;` is present in `src/lib.rs` (added here or verified as already added by plan 04 Wave 0), so `moira::i18n::is_known_key` is callable from `tests/` — and at least one test actually calls it.
- [ ] Generated OpenAPI still declares `Idempotency-Key` on all four operations, now with a truthful description and an explicit `409`; 02a's interim disclaimer sentence is gone and its boundary sentence survives — all verified by automated tests.
- [ ] 02a's `repeated_ingest_with_the_same_idempotency_key_creates_two_versions_until_02b` is **deleted**, and 02a's `rag_write_routes_carry_the_interim_idempotency_disclaimer` unit test is deleted, with their replacements passing.
- [ ] `docs/idempotency.md` lists the three new operations and documents the `/ingest`↔`/reindex` shared identity; `docs/public-api.md` and `docs/conversation-memory-rag-api.md` no longer claim replay is unimplemented.
- [ ] Requests without `Idempotency-Key` are unchanged, proven by `requests_without_an_idempotency_key_still_create_new_versions`.
- [ ] All five required Rust gates pass with zero warnings/failures, including on the newly-compiled `src/i18n/` tree.
- [ ] No existing test regressed (`tests/admin_idempotency.rs`, `tests/execution_lifecycle.rs`, `tests/public_authorization.rs`, `tests/http_error_contract.rs`, `tests/rag_ingestion_honesty.rs`).

### Cross-cutting compliance checklist (`plans/CONVENTIONS.md` §8 — binding)

- [ ] Work performed on branch `plan/02b-idempotency-replay`, **stacked on `plan/02a-mvp-boundary-honesty`**, with the base PR named in the description and the branch rebased onto `main` once 02a merges (`CONVENTIONS.md` §1 rule 1); 02a's branch never force-pushed while stacked (rule 7). PR opened with **all seven** required description sections.
- [ ] All gates in `CONVENTIONS.md` §2 pass: `cargo fmt --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace --all-features`, `cargo build --release --locked`, plus clean PostgreSQL migration validation from an empty database.
- [ ] **Unit tests delivered and passing** — all ten functions named in Verification Layer 1, in `#[cfg(test)] mod tests` beside the code, requiring no database.
- [ ] **E2E tests delivered and passing** — all thirteen functions named in Verification Layer 2, in `tests/rag_idempotency_replay.rs`, driving the real HTTP surface against real PostgreSQL 16 + pgvector via `tests/support/mod.rs`, fail-closed under `CI`.
- [ ] No concurrency test uses `sleep()`; `concurrent_same_key_ingests_produce_one_version_and_one_audit_row` uses a `tokio::sync::Barrier` acknowledgement gate.
- [ ] Every new error/notice string has an i18n key + English default in `src/i18n/catalog/errors.rs`, mirrored into `docs/i18n-response-catalog.json`, with a test asserting presence — satisfied by `moira.error.idempotency_in_progress`, its JSON mirror, `idempotency_in_progress_key_is_catalogued`, `idempotency_keys_are_catalogued_exactly_once`, and the two e2e tests asserting non-empty `message_key` + `message` and `is_known_key`.
- [ ] Frontend conventions (`CONVENTIONS.md` §5/§6) — **N/A**, this plan ships no console code.
- [ ] Auth conventions (`CONVENTIONS.md` §7) — **N/A** for configuration, but note the security-relevant property this plan **does** touch: replay is scoped by `actor_fingerprint`, so one actor can never replay another's response. Proven by `different_actors_with_the_same_key_do_not_replay_each_others_responses`.
- [ ] No secret-leak, verified by test — `src/security/masking::tests` unchanged, plus the explicit review finding that the three replayed bodies carry no credential material and therefore need no `with_replay_response` sanitization.
- [ ] Plan lands **before** plan 05's OpenAPI-drift gate freezes the spec (`CONVENTIONS.md` §1 rule 6).
- [ ] **Done means merged.** Every box above is verified by a named, passing test.

---

## Risks & Rollback

**Security risks.**
- *Cross-actor replay* — mitigated by construction: `actor_fingerprint` is part of the ledger's unique index and of the advisory-lock key. Proven by `different_actors_with_the_same_key_do_not_replay_each_others_responses`. The single-definition rule for `actor_fingerprint` (§5, Wave 5 check 3) exists precisely so this cannot drift.
- *Secret material in the ledger* — not applicable here (the three request bodies carry document content and metadata, not credentials), but it is why `AdminCommandMutation::with_replay_response` (`src/application/admin_command.rs:126-140`) exists for credential commands. **If a future RAG DTO ever gains a secret field, that operation must switch to `with_replay_response`.** Record this as a standing constraint in `docs/idempotency.md`.
- *Document content persisted twice* — the replay `response_body` stores the `RagDocumentRecord`, not `content_plain`, so no additional copy of document text lands in the ledger. Verify at implementation time that `RagDocumentRecord` genuinely carries no content body; if it does, treat that as a retention concern and raise it before merging.

**Correctness risks.**
- **Nested transaction (highest risk).** A leftover `begin()` inside a `_with_connection` function would nest a transaction inside the runner's savepoint and silently break rollback semantics. Mitigated by the Wave 1 single-agent rule, the explicit grep invariant (§4), Wave 5 check 2, and `rolled_back_ingest_leaves_no_audit_row_and_no_partial_version`.
- **Audit-format drift.** Reusing `admin.rs`'s `success_audit` would silently lowercase `actor_type` on the RAG surface. Mitigated by the dedicated `conversation_audit` builder and `conversation_audit_preserves_the_existing_actor_type_casing`.
- **`/reindex` sharing an operation identity with `/ingest`** is a deliberate, surprising semantic. Mitigated by two named tests and an explicit sentence in `docs/idempotency.md`. If product later wants the two aliases to be independently keyable, the change is to add a discriminator to the `path` envelope — cheap, but a behavior change requiring its own note.

**Performance / availability risks.**
- **Lock hold time.** These operations now hold the advisory lock, the claim row, and (for ingest) the `for update` document row for the whole claim→finalize window, where previously the document lock was held only for the inner transaction. Ingest is short (a handful of statements over direct text), so the window stays small, but a pathological large-content ingest lengthens it.
- **Poll-loop connection occupancy.** A contended key spins in a 20 ms poll for up to **5 seconds** (`src/infra/repositories/admin.rs:565-581`) while holding a pool connection. A client hammering one key can occupy connections for 5 s each. This is pre-existing behavior on ten admin routes, now extended to three more; it is a real capacity consideration under the current dev-scale pool sizing (finding P2-6, plan 06). Not changed here — flagged.
- **Unbounded ledger growth.** No retention worker exists yet (P1-5, plan 04): `WorkerRegistry` enumerates a `"retention-cleanup"` spec with **no execution body** (`src/infra/workers.rs:1-163`). 02b adds rows for three more operations that nothing prunes. Rows are small and carry a 24-hour `expires_at` (and are purged opportunistically on the next claim with the same key), so the practical risk is modest — but it is honestly a growth vector until plan 04 lands.

**Compatibility risks.** Behavioral change on four routes (retry replays instead of duplicating). Correct for any client that intended a retry; observable for a client that was (incorrectly) relying on repeated keyed POSTs to create multiple versions. Called out in PR section 4.

**Process risks.**
- **Stacked-branch churn.** This branch depends on 02a's `RagDocumentRecord` shape, OpenAPI description text, and `tests/rag_ingestion_honesty.rs`. If 02a's reviewers change the Sentence A/B split, this plan's §7 and its two OpenAPI tests must be re-grounded before implementation. Rebase promptly after 02a merges and re-run the full gate set.
- **`pub mod i18n;` double-ownership** with plan 04 Wave 0 — resolved by the explicit rule in Architecture → *Ordering relationship*, but the coordinator must actually make the call before Wave 0 dispatches.
- **Newly-compiled `src/i18n/` under `-D warnings`** may generate more cleanup than expected. Escalation path defined in §1: hand the module line back to plan 04 and downgrade this plan's catalog assertions, recording the downgrade rather than shipping an unverified claim.

**Rollback procedure.** `git revert` of the merge commit. No migration to roll back. Ledger rows written by this plan become inert on revert (nothing reads `rag.*` operations any more) and expire within 24 hours; no data corruption and no manual cleanup required. Reverting also restores the duplicate-on-retry behavior — so if a revert is needed, re-apply 02a's honesty text (Sentence B and the docs caveats) in the same revert PR so the spec does not go back to promising replay it no longer performs.

**Deliberately deferred follow-ups (tracked, not dropped):**
- **`If-Match` / optimistic concurrency** on the RAG mutations. `AdminCommandSpec::with_expected_version` is ready and the handlers already emit `ETag`; the missing piece is parsing `If-Match` and threading `expected_version`. Related to the TOCTOU window plan 06 records for the trusted-JWT-issuer handlers (`src/http/admin.rs:1449-1452,1480-1483,1532-1535,1563-1566`).
- **Replay for conversation and memory create routes** (`POST /api/v1/conversations`, `.../messages`, `/api/v1/memories`) — they do not advertise the header today, so extending replay there is new API surface, not a P0-2 fix. `docs/todo.md`'s Phase 2 idempotency-extension bullet tracks it (the `:22` line number in the original text has since moved), and it is now also GitHub issue #92.
- **Unifying `runtime_admin.rs`'s two-phase scheme** onto `AdminCommandRunner`, and `patch_credential`'s bypass of the runner (`src/application/admin.rs:583-612`) — both are plan 06 debt items.
- **Ledger retention worker** — plan 04 (P1-5).
- **Remaining P0-5 catalog gaps** (`database_unavailable`, `upstream_error`, `configuration_error`, `database_error`, `http_client_error`, `redis_error`, `capacity_exhausted`, `routing_policy_provider_model_mismatch`), the `docs/i18n-response-catalog.json` duplicate-key cleanup, and `every_coded_error_literal_in_src_has_a_catalog_entry` — plans 05/06.
- **Versioned HMAC idempotency hashing with dual-lookup** — plan 03 (P1-1), which changes the two shared helpers this plan relies on without needing any 02b-specific edit.
