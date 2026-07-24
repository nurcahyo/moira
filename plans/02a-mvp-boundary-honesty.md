# Plan 02a — MVP Boundary Honesty & API Truth-in-Advertising

Companion to `00-audit-report.md`, `01-roadmap-and-dependencies.md`, and `CONVENTIONS.md`. Addresses **P0-1** and **P0-3**.

> **Split notice.** This plan is the first half of the former plan 02. Per `CONVENTIONS.md` §0 decision **D1**, P0-2 (`Idempotency-Key` advertised but unimplemented) is fixed by **implementing real replay**, not by removing the parameter — and per decision **D2** that work is a separate, stacked branch/PR: **`plans/02b-idempotency-replay.md`**. Consequently **02a must not remove the `Idempotency-Key` parameter from any route.** The parameter stays in the spec because 02b is about to make it true.

---

## Summary

**Objective.** Make Moira's public and admin API surface tell the truth about what it does today. Conversation, explicit-memory, and RAG endpoints are real, durable, DB-backed **persistence and configuration primitives**, but they are not an intelligence pipeline: there is no retrieval, no chunking, no embeddings, no context injection, no summarization, and the RAG ingestion status is hardcoded to a value that claims completed indexing regardless of what actually happened.

**Why ordered here.** This is the cheapest, fastest way to close the P0 truth-in-advertising gap: relabelling and documenting the boundary is far smaller than building the retrieval pipeline (deferred to plan 11) or implementing genuine idempotent replay (02b). Splitting the replay work out is precisely what lets this honesty gate ship immediately — 02a carries **no migration, no concurrency test, and no new transactional machinery**. It must also land before plan 05's OpenAPI-drift CI gate freezes the spec, so that the spec that gets locked is the honest one (`01-roadmap-and-dependencies.md` §3: `I02A --> I02B --> I05`).

**User-visible outcome.**
- `POST /api/v1/admin/rag-documents/{id}/ingest` (and `/reindex`) return an honest ingestion status: the response and the persisted `rag_document_versions.ingestion_status` reflect that content was **stored**, not that it was chunked/embedded/indexed for retrieval.
- `RagDocumentRecord` gains a machine-readable `ingestion_status` field so callers can see this without reading docs.
- `POST /v1/responses` / `/v1/responses/stream` responses keep `citations: []` but the OpenAPI description and `docs/public-api.md` now say explicitly that citations are always empty because RAG retrieval is not wired into the prompt.
- The conversation/memory/RAG operations carry an OpenAPI `description` stating the preview/primitive boundary, so the caveat lives in the generated spec and not only in prose docs.
- The four RAG write routes **keep** their `Idempotency-Key` parameter, but its description and the docs now state honestly that replay is **not yet implemented and lands in 02b** — replacing a silent false promise with a dated, tracked one for the short window between the two PRs.
- `docs/public-api.md`, `docs/conversation-memory-rag-api.md`, `docs/idempotency.md`, and `docs/document-ingestion.md` state the MVP preview boundary in plain language.
- `docs/todo.md` Phase 5 items are cross-referenced as the deferred implementation (plan 11), not silently forgotten.

**Included scope.**
- Honest `ingestion_status` in both the DB write path (**both** write sites) and the API response shape for RAG documents.
- OpenAPI `description` text on conversation/memory/RAG operations stating the preview/primitive boundary, plus a separate, explicitly interim sentence about idempotency that 02b deletes.
- `docs/public-api.md`, `docs/conversation-memory-rag-api.md`, `docs/idempotency.md`, `docs/document-ingestion.md` updates; `docs/todo.md` annotation.
- Contract tests: OpenAPI assertions on the boundary text and on the *survival* of the `Idempotency-Key` parameter; row-mapping unit tests; an e2e file asserting honest status end to end.

**Excluded scope (explicitly deferred).**
- **No idempotency implementation and no removal of the `Idempotency-Key` parameter.** That is **02b** (`plans/02b-idempotency-replay.md`). 02a must not touch `AdminCommandRunner`, `claim_idempotency`, `finalize_idempotency`, or `ctx.idempotency_key`.
- **No RAG/memory intelligence implementation** — no chunking, no embeddings, no semantic retrieval, no context injection into prompts, no summarization. That is plan 11 (`11-rag-memory-intelligence.md`).
- No changes to `/v1/responses` idempotency (already correctly implemented at `src/application/public.rs:125-134`, `claim_idempotency` at `:1010-1054`, `replay_idempotency` at `:1056`) — do not touch that code path.
- No changes to `ContextPlanner::deterministic_phase_five_order()` (`src/application/conversation.rs:37-48`) or `prepare_response_conversation` (`:314-380`) logic — only their **documentation**, not their behavior.
- No new database tables, no schema changes. The existing `RagIngestionStatus` enum (`src/domain/conversation.rs:130-139`) already defines all eight variants (`Pending` through `Superseded`), matching the DB CHECK constraint exactly — this plan uses existing variants honestly and adds none.

### Branch & PR (binding — `plans/CONVENTIONS.md` §1)

**Branch:** `plan/02a-mvp-boundary-honesty`, cut from the current `main`. This plan is **not** stacked on any other plan branch. **`plan/02b-idempotency-replay` stacks on it** (`CONVENTIONS.md` §1 table), and plans 03/05 sequence after it (`01` §3). **Never force-push this branch** once 02b has branched from it (`CONVENTIONS.md` §1 rule 7).

**Commits:** Conventional Commits, matching existing history style (`feat: make admin commands atomic`). Expected prefixes: `feat:` (the `ingestion_status` field), `fix:` (the false `'indexed'` write), `docs:` (the docs updates + `docs/todo.md` annotation), `test:` (the new unit + e2e layers).

**PR must not open until every gate in `CONVENTIONS.md` §2 passes locally** (the five Rust gates enumerated under Verification below).

**PR description — required sections (all seven, none omitted):**
1. **Plan link** — `plans/02a-mvp-boundary-honesty.md`.
2. **Findings addressed** — `P0-1`, `P0-3` (from `plans/00-audit-report.md`). **P0-2 is explicitly NOT addressed here** — name `plans/02b-idempotency-replay.md` as the follow-on PR.
3. **Migrations included** — **none** (verified: `'pending'` is already the column default and is inside the existing CHECK constraint at `migrations/0007_conversations_memory_rag.sql:382-383`). If the optional `'indexed' → 'pending'` backfill in Risks & Rollback is taken, name it here explicitly.
4. **Breaking API/OpenAPI changes** — none breaking; one additive schema change (`RagDocumentRecord.ingestion_status`) and description-text updates. **State explicitly that no parameter was removed** — the `Idempotency-Key` parameter survives on all four RAG write routes per decision D1. Because this plan must land **before** plan 05's OpenAPI-drift gate freezes the spec (`CONVENTIONS.md` §1 rule 6), include the OpenAPI diff.
5. **Test evidence** — output summary of both layers: the unit layer (`cargo test --workspace --all-features`, naming the new `src/http/mod.rs`, `src/infra/pg_rows.rs`, and `src/domain/conversation.rs` test functions) **and** the e2e layer (`tests/rag_ingestion_honesty.rs` run against real PostgreSQL 16 + pgvector with `MOIRA_TEST_DATABASE_URL` set).
6. **Rollback procedure** — see Risks & Rollback below (`git revert` of the merge commit; inverse `UPDATE` if the backfill was taken).
7. **Deferred follow-ups** — 02b (real idempotency replay, stacked on this branch), plan 11 (retrieval pipeline), plan 04 (ledger retention), plan 05/06 (drift gates).

**Done means merged.** This plan is complete when the PR is **merged with all gates green** and every Definition of Done box is objectively verified by a named, passing test (`CONVENTIONS.md` §1 rule 5, §3 "Definition of Done addition").

---

## Findings Addressed

### P0-1 — RAG/memory/summarization endpoints look functional but are no-ops
- `src/infra/repositories/conversation.rs:1138-1211` (`ingest_rag_document`): stores `content_plain` and `content_hash` into a new `rag_document_versions` row, but the `INSERT` at `:1177-1197` hardcodes `'indexed'` as the literal `ingestion_status` value (in the `VALUES` clause at `:1184`) regardless of whether any chunking/embedding happened (none does — `rag_chunks`/`rag_chunk_embeddings` are never written by this or any other code path, confirmed absent by grep).
- **Second `'indexed'` write site:** `create_rag_document` (`src/infra/repositories/conversation.rs:1013-1084`) also inserts a version-1 `rag_document_versions` row with the hardcoded `'indexed'` literal (`:1058-1074`, `VALUES` at `:1064`) whenever the create request carries inline `content`. Both write sites must be fixed; the DoD grep for the `'indexed'` write literal covers both.
- **Verified during grounding:** `RagDocumentRecord` (`src/domain/conversation.rs:577-593`) does **not currently expose `ingestion_status` to API callers at all** — the field only exists as a DB column, mapped by `rag_document_record_from_row` (`src/infra/pg_rows.rs:626-644`), which never reads it. Today's dishonesty is therefore entirely inside the database (a false audit trail), not yet in the JSON response — but the fix must cover both: (a) stop writing a false DB value, and (b) surface the true value in the API response so future consumers (including the plan-11 pipeline) have an honest signal to build on.
- `src/application/public.rs:1657` (inside `public_response_from_record`, `:1614`): `citations: Vec::new()` unconditionally — every `/v1/responses` response returns an empty `citations` array with no indication that this is structural (no RAG wired) rather than "no citations found for this query."
- `src/application/conversation.rs:37-48` (`ContextPlanner::deterministic_phase_five_order`): returns a hardcoded ordering array documenting an intended context-assembly order (`protected_instructions`, …, `retrieved_memory`, `retrieved_rag`, `older_history`) that is never actually used to assemble a prompt (its only caller is a unit test) — `prepare_response_conversation` (`:314-380`) only persists the user's message via `self.repo.add_message(...)`; it never loads conversation history, summaries, memories, or RAG chunks into anything sent to the provider.
- No summarization write path exists: grep confirms `conversation_summaries` is never inserted into outside migrations/tests.
- `docs/todo.md:77` area (Phase 5 preamble) already instructs advertising these as primitives "until retrieval, chunking, embeddings, context injection, and citations are wired end to end" — this plan makes that instruction real in the shipped API surface instead of only in an internal TODO file.

### P0-3 — Conversation/memory/RAG surface must be explicitly scoped before public exposure
- Aggregates P0-1 and P0-2. `docs/public-api.md` (22 lines, read during grounding) documents `/v1/responses`-family routes only and contains **zero** mentions of conversations/memories/RAG. `docs/conversation-memory-rag-api.md` (15 lines) lists the routes but makes no honesty statement — it says "OpenAPI includes schemas for these resources" but never says the resources are non-functional as an intelligence layer.
- **Correction path:** rewrite both docs plus the affected `#[utoipa::path]` `description` fields to state the boundary explicitly, so the generated OpenAPI spec itself (not just prose docs) carries the caveat.

### P0-2 — not addressed here (pointer only)
P0-2 is closed by **`plans/02b-idempotency-replay.md`**, which implements genuine replay for `src/http/conversation.rs:665,847,954,984` using the existing `AdminCommandRunner` envelope. 02a's only obligation toward P0-2 is to **not make it worse and not pre-empt it**: keep the parameter, and say honestly in the spec and docs that replay is landing in the immediately-following PR. Any text in this plan that once proposed removing the parameter or returning `501` has been deleted per `CONVENTIONS.md` §0 D1.

---

## Architecture

**Components & ownership boundaries (per `docs/project-structure.md` layering).**
- `src/http/conversation.rs` — HTTP layer: route handlers and `#[utoipa::path]` doc annotations (owns the description text; owns **no parameter removals**).
- `src/application/conversation.rs` — `ConversationService`: owns nothing changed in behavior here except doc comments on `ContextPlanner` and `prepare_response_conversation`; no method signatures change.
- `src/infra/repositories/conversation.rs` — `PgConversationRepository`: SQL (owns the `ingestion_status` write-path fix and the read-path `LEFT JOIN`).
- `src/infra/pg_rows.rs` — row-mapping helpers (owns the new `rag_ingestion_status_from_db`/`_to_db` pair and the `rag_document_record_from_row` addition).
- `src/domain/conversation.rs` — DTOs (owns adding the `ingestion_status` field to `RagDocumentRecord`).
- `src/domain/public.rs`, `src/application/public.rs` — the `citations` schema description and its construction-site comment.
- `docs/*` — documentation layer, no code ownership overlap.

This plan does not cross the Moira/Rig boundary at all — it touches only persistence primitives and API contract text, never AI execution.

**Data flow.** `POST /api/v1/admin/rag-documents/{id}/ingest` → `ingest_rag_document` handler → `ConversationService::ingest_rag_document` → `PgConversationRepository::ingest_rag_document` (transaction: supersede old version, insert new version with **honest** `ingestion_status`, update `current_version_id`, re-select the document row) → `rag_document_record_from_row` (now includes `ingestion_status`) → JSON response. No new data-flow paths are introduced; only the status value and the mapped field change.

**Security boundaries.** No change. Both admin RAG routes and public conversation/memory routes keep their existing `admin_actor`/`public_actor` authentication (`src/http/conversation.rs:25-37`) and authorization scope checks unchanged.

**Database/migration changes.** **None required — verified.** `rag_document_versions.ingestion_status` is `varchar(32) not null default 'pending'` with a CHECK constraint enumerating exactly `('pending', 'downloading', 'parsing', 'chunking', 'embedding', 'indexed', 'failed', 'superseded')` (`migrations/0007_conversations_memory_rag.sql:382-383`). `'pending'` — the honest value this plan writes — is already permitted (it is the column default), so only the Rust-side literal changes.

**API & OpenAPI changes.**
- **No parameter is removed.** The `Idempotency-Key` parameter stays on all four RAG write operations; only its `description` string changes (see Detailed Implementation §5).
- Add/adjust `description` text on `create_conversation`, `create_message`, `create_memory`, `create_rag_collection`, `create_rag_document`, `ingest_rag_document`, `reindex_rag_document`, and the four policy PUT endpoints (`put_conversation_policy`, `put_memory_policy`, `put_retrieval_policy`, `put_embedding_policy`) to state the preview boundary once, consistently.
- `RagDocumentRecord` OpenAPI schema (`ToSchema` derive) gains one new field `ingestion_status: Option<RagIngestionStatus>`. This is an **additive, backward-compatible** schema change.
- `PublicResponse.citations` doc comment / schema description updated to state "always empty; RAG retrieval is not wired into response generation" (no field type change).

**Backward compatibility.** Adding `ingestion_status` is additive JSON — existing clients that don't know the field ignore it; no field is removed or renamed. `RagDocumentIngestRequest` and `RagDocumentCreateRequest` are unchanged. No header behavior changes.

**Deployment implications.** None — no new config flags, no new infra dependency, no migration to run. Safe as a normal rolling deploy (single replica, per current architecture).

**Failure & recovery behavior.** No new failure modes. The `ingest_rag_document` transaction (`src/infra/repositories/conversation.rs:1144-1209`) keeps its existing transaction boundary (`begin()` … `commit()`), row lock (`for update` on the document row), and version-supersession logic — only the literal status value written and the read-path projection change. **Note for the 02b implementer:** 02b converts these methods to run on a caller-supplied connection; 02a must leave the transaction shape recognisably intact so that refactor is mechanical.

---

## Detailed Implementation

### 1. `src/domain/conversation.rs` — honest status vocabulary and DTO field

- Confirm `RagIngestionStatus` (`:130-139`) already has the variants needed: `Pending, Downloading, Parsing, Chunking, Embedding, Indexed, Failed, Superseded`. For "content stored, not yet processed by a real pipeline," the correct honest value is **`Pending`** (work not started) — do **not** introduce a new variant; `Pending` is semantically exact given no chunking/embedding pipeline exists (plan 11 will transition documents through `Downloading → Parsing → Chunking → Embedding → Indexed` when it lands).
- Add a field to `RagDocumentRecord` (`:577-593`, which derives `Debug, Clone, Serialize, Deserialize, ToSchema`):
  ```rust
  pub struct RagDocumentRecord {
      ...
      pub current_version_id: Option<Uuid>,
      pub ingestion_status: Option<RagIngestionStatus>,
      ...
  }
  ```
  `Option` because a document with no versions yet has no `ingestion_status` to report — reflect that honestly as `null` rather than inventing a default. Note that `create_rag_document` **can** create version 1 immediately when the create request carries inline `content` (`src/infra/repositories/conversation.rs:1056-1079`), so `null` means "no version exists," not "freshly created": a content-carrying create must report `"pending"` like any other stored-but-unprocessed version.
- **Do not remove or reorder existing fields** — `RagDocumentRecord` derives `Deserialize`, and 02b relies on round-tripping it through the idempotency ledger.

### 2. `src/infra/repositories/conversation.rs` — stop writing a false status

- In `ingest_rag_document` (`:1138-1211`), the `INSERT INTO rag_document_versions (...)` at `:1177-1197` hardcodes the literal `'indexed'` in `VALUES (..., 'indexed', $9)` (`:1184`). Replace the literal with a **bound parameter** carrying `rag_ingestion_status_to_db(RagIngestionStatus::Pending)` (see §3) rather than hand-writing `'pending'`, so the vocabulary has exactly one source of truth.
- **Same fix in `create_rag_document`** (`:1013-1084`): the inline-content version-1 `INSERT` at `:1058-1074` hardcodes `'indexed'` in its `VALUES` clause (`:1064`) — change it identically.
- The `UPDATE rag_document_versions SET superseded_at = …, ingestion_status = case when ingestion_status = 'indexed' then 'superseded' else ingestion_status end` at `:1166-1176` (supersession of the *previous* version) is a **read** comparison, not a write of `'indexed'`. Since nothing ever reaches `'indexed'` under this plan's scope, this `CASE` becomes a no-op until plan 11 lands a real pipeline. Leave the `CASE` structure in place (harmless, forward-compatible) but do not claim in comments that it does anything today.
- **Add a code comment** directly above each corrected `INSERT`: `// Honest status: no chunking/embedding pipeline exists yet (see plans/11-rag-memory-intelligence.md). Content is stored verbatim; ingestion_status reflects "not yet processed", not "indexed for retrieval".`

### 3. `src/infra/pg_rows.rs` — surface the true value

- Add the missing mapping helpers `rag_ingestion_status_from_db` / `rag_ingestion_status_to_db` next to `rag_collection_status_from_db` (`:1032`) and `rag_document_status_from_db` (`:1072`), covering all eight variants with the `snake_case` strings the migration and serde already use, and following those helpers' exact match-on-string pattern (unknown value ⇒ error, never a silent default).
- In `rag_document_record_from_row` (`:626-644`), add:
  ```rust
  ingestion_status: row
      .try_get::<Option<String>, _>("ingestion_status")?
      .map(rag_ingestion_status_from_db)
      .transpose()?,
  ```
- **This is the one non-trivial part of this plan:** `rag_documents` itself has no `ingestion_status` column (only `rag_document_versions` does, per the versioned-content model), and every read path (`get_rag_document`, `list_rag_documents`, `create_rag_document`, `ingest_rag_document`) builds its row through `rag_document_select` (`src/infra/repositories/conversation.rs:1253-1263`), whose base selects pull from `rag_documents` only. Extend `rag_document_select` to `LEFT JOIN rag_document_versions v ON v.id = document_rows.current_version_id` and project `v.ingestion_status`, so **every** read of a `RagDocumentRecord` reports the same field rather than having it silently `null` depending on which endpoint served it. Grep all `rag_document_select(` call sites to confirm full coverage.
- **Ordering caveat inside the two write transactions:** both `create_rag_document` and `ingest_rag_document` fetch the response row *within* the same transaction. `create_rag_document` fetches it from the `insert into rag_documents … returning *` (`:1035-1055`) **before** the version insert and `current_version_id` update run (`:1056-1079`), so with the `LEFT JOIN` its response would still report `ingestion_status: null` even when a version was just created. Fix by re-selecting the document row via `rag_document_select` after the version insert / `current_version_id` update, mirroring what `ingest_rag_document` already does at `:1203-1208`.

### 4. `src/application/conversation.rs` — documentation only

- No behavioral change to any `ConversationService` method. **02a must not add idempotency wiring here** — `ConversationService` still has zero `idempotency` references at the end of this plan, and 02b's Wave 2 is the only place that changes.
- Add a doc comment above `ContextPlanner` (`:34-49`): `// NOTE: this ordering is a design placeholder for the future context-assembly pipeline (plans/11-rag-memory-intelligence.md). It is not currently consumed by prepare_response_conversation and does not affect what is sent to the provider.`
- Add a doc comment above `prepare_response_conversation` (`:314-380`): `// Persists the user's message for later retrieval by GET endpoints; does not load history, summaries, memories, or RAG content into the prompt sent to the provider. See docs/conversation-memory-rag-api.md for the MVP boundary.`

### 5. `src/http/conversation.rs` — OpenAPI contract corrections (**no parameter removals**)

The four `Idempotency-Key` declarations at `:665` (`create_rag_collection`), `:847` (`create_rag_document`), `:954` (`ingest_rag_document`), `:984` (`reindex_rag_document`) **stay exactly where they are**. Change only their `description` string, and add operation descriptions, as follows.

**(a) Parameter description** — replace `description = "Optional replay key"` on all four with:
```
description = "Optional replay key. Replay is not yet implemented on this route; see plans/02b-idempotency-replay.md."
```

**(b) Operation description — two separate sentences, deliberately.** Write them as two distinct sentences so that 02b can delete exactly one without disturbing the other, and so each has its own test:

- **Sentence A (permanent boundary, survives 02b)** — on `create_rag_collection`, `create_rag_document`, `ingest_rag_document`, `reindex_rag_document`:
  `"Persistence primitive: no retrieval, chunking, or embedding pipeline runs, and stored content is not used to influence model responses. See docs/conversation-memory-rag-api.md."`
- **Sentence B (interim, deleted by 02b)** — appended to the same four operations:
  `"Idempotency-Key is accepted but replay is not implemented yet; retrying can duplicate side effects."`

- Apply **Sentence A only** (shortened) to `create_conversation`, `create_message`, `create_memory` and the four `put_*_policy` admin routes, since all of them configure/gate a pipeline that doesn't run: `"Persistence/configuration primitive; conversation history, memory, and RAG are not yet used to influence model responses."` These routes declare no `Idempotency-Key`, so Sentence B does not apply.
- Keep each description under ~250 characters to stay OpenAPI-idiomatic; the full explanation lives in the docs files and is linked.
- Do not touch security annotations, status codes, or any parameter other than the four description strings above.

### 6. `src/application/public.rs` + `src/domain/public.rs` — citations note

- At the `PublicResponse` construction site (`public_response_from_record`, `:1614-1662`; `citations: Vec::new()` at `:1657`), add: `// Always empty: RAG retrieval is not wired into response generation (see plans/11-rag-memory-intelligence.md).`
- In `src/domain/public.rs` (`pub struct PublicResponse` at `:156`, `pub citations: Vec<PublicCitation>` at `:167`), add/adjust the field's doc comment / `ToSchema` description to state the same, so it is visible in generated OpenAPI, not just Rust source.
- Verify at implementation time whether the SSE path (`map_runtime_event`, `src/application/public.rs:1664`) emits its own citations field needing the same note; if so, add it there too.

### 7. `docs/public-api.md` — add the boundary statement

Add a new section (after the existing route list, before the "Do not send real provider secrets…" paragraph):

```markdown
## MVP boundary: conversations, memory, and RAG

`/api/v1/conversations`, `/api/v1/memories`, and the admin RAG endpoints under
`/api/v1/admin/rag-collections` and `/api/v1/admin/rag-documents` are **persistence
and configuration primitives only** in this release. They store and version content
durably and enforce policy, but:

- No retrieval, chunking, or embedding pipeline runs. `ingestion_status` on a RAG
  document reflects storage, not indexing for retrieval.
- Conversation history, explicit memories, and RAG documents are not loaded into the
  prompt sent to a provider. `POST /v1/responses` always returns `citations: []`.
- No summarization runs; `conversation_summaries` is never populated.
- `Idempotency-Key` is advertised on the RAG create/ingest/reindex routes but its
  replay implementation has not shipped yet: until it does, retrying a create can
  duplicate side effects. Real replay exists today only for `/v1/responses` and the
  admin command routes documented in `docs/idempotency.md`.

Full retrieval/memory intelligence is tracked separately and is not part of this MVP.
```

### 8. `docs/conversation-memory-rag-api.md` — rewrite for honesty

Replace the current 15-line file's closing line ("OpenAPI includes schemas…") with an explicit statement mirroring the `docs/public-api.md` section, plus a short table mapping each route group to its actual current behavior (store / version / policy-gate) versus what it does **not** do (retrieve / inject / summarize / replay). Cross-reference `docs/public-api.md` rather than duplicating the prose.

### 9. `docs/idempotency.md` — close the exclusion gap

`docs/public-api.md`'s new section points readers at `docs/idempotency.md` as the authority on where idempotency is real. That file is accurate about what **is** covered ("non-streaming public response creation and selected admin create and rotate commands", plus the ten-row operation table matching `src/http/mod.rs:534`'s explicit route list), but it **never states that conversation/memory/RAG routes are excluded**. Add one sentence after the operation table:

```markdown
Conversation, memory, and RAG endpoints do **not** replay today. `Idempotency-Key`
is advertised on the RAG create/ingest/reindex routes and is accepted, but no replay
is performed yet, so retrying a create can duplicate side effects. Implementing real
replay for those routes is the next change to this document.
```

Docs-only, no code, no test.

### 10. `docs/document-ingestion.md` — name the new field

Read during grounding and confirmed already honest about scope ("direct-text document metadata and version creation", excludes parsing/crawling/OCR). The one gap: it does not mention `ingestion_status` at all. Add a single sentence naming the new field and its `pending` / `null` semantics so the field is discoverable from the ingestion doc, not only from the API reference.

`docs/retrieval-citations.md` and `docs/conversation-summarization.md` were re-read and are **already honest** ("returns an empty list because retrieval context is not yet injected"; "Automatic summarization execution is not enabled yet"). Leave them untouched rather than churning them.

### 11. `docs/todo.md` — reconciliation

- Annotate the Phase 5 preamble (`docs/todo.md:77` area): `(Descoped for MVP: plans/02a-mvp-boundary-honesty.md makes the current no-op behavior honest in the API contract; full implementation remains tracked here for plans/11-rag-memory-intelligence.md.)`
- Annotate the Phase 2 idempotency-extension line (`:22`, "Extend atomic idempotency and sanitized deterministic-failure replay to the runtime-policy, RAG, conversation, memory…") to point at `plans/02b-idempotency-replay.md` for the RAG/conversation/memory slice.
- Do not delete or renumber existing TODO lines — only annotate.

### 12. Tests — **both layers are mandatory** (`plans/CONVENTIONS.md` §3)

#### 12a. Unit layer — no database, `#[cfg(test)] mod tests` beside the code

| File | New test function | Asserts |
|---|---|---|
| `src/http/mod.rs` (existing `mod tests` at `:213`; helper `parameter_named` at `:653`) | `rag_write_routes_still_declare_the_idempotency_key_parameter` | `parameter_named(op, "Idempotency-Key")` is **`true`** on all four operations (`POST /api/v1/admin/rag-collections`, `POST /api/v1/admin/rag-collections/{collection_id}/documents`, `POST /api/v1/admin/rag-documents/{id}/ingest`, `POST /api/v1/admin/rag-documents/{id}/reindex`). This is the guard that keeps a stale "remove the parameter" instinct (superseded by `CONVENTIONS.md` §0 D1) from being applied. |
| `src/http/mod.rs` | `rag_collection_document_route_keeps_its_collection_id_path_parameter` | the `collection_id` path param on `POST .../{collection_id}/documents` is intact — regression guard on the `params(...)` block |
| `src/http/mod.rs` | `conversation_memory_rag_operations_document_the_mvp_preview_boundary` | every operation listed in §5 carries a non-empty `description` containing **Sentence A** — this is what stops the honesty text from silently regressing once plan 05 freezes the spec. Assert Sentence A only, so 02b can delete Sentence B without touching this test. |
| `src/http/mod.rs` | `rag_write_routes_carry_the_interim_idempotency_disclaimer` | **Sentence B** is present on the four RAG write operations, and the `Idempotency-Key` parameter description names 02b. **This is the test 02b deletes**; naming it explicitly makes the hand-off mechanical. |
| `src/http/mod.rs` | `rag_document_record_schema_exposes_ingestion_status` | `components.schemas.RagDocumentRecord.properties.ingestion_status` exists and `$ref`s / enumerates `RagIngestionStatus` |
| `src/http/mod.rs` | `public_response_schema_documents_always_empty_citations` | `components.schemas.PublicResponse.properties.citations.description` is non-empty and states the structural emptiness |
| `src/infra/pg_rows.rs` (new `#[cfg(test)] mod tests`) | `rag_ingestion_status_round_trips_all_eight_variants` | the new `_to_db`/`_from_db` pair round-trips `Pending, Downloading, Parsing, Chunking, Embedding, Indexed, Failed, Superseded` against the exact `snake_case` strings in the CHECK constraint (`migrations/0007_conversations_memory_rag.sql:382-383`) |
| `src/infra/pg_rows.rs` | `rag_ingestion_status_from_db_rejects_unknown_value` | an out-of-vocabulary DB string errors rather than silently defaulting |
| `src/domain/conversation.rs` (new `#[cfg(test)] mod tests`) | `rag_document_record_serializes_ingestion_status_as_snake_case` | `Some(RagIngestionStatus::Pending)` serializes to `"pending"` |
| `src/domain/conversation.rs` | `rag_document_record_round_trips_through_serde` | serialize → deserialize is lossless including `ingestion_status` — the property 02b's ledger replay depends on |

#### 12b. E2E layer — new file `tests/rag_ingestion_honesty.rs`

Follows the existing harness exactly: `mod support;`, a `LifecycleFixture`-derived fixture, and the router-driving pattern used by `tests/admin_idempotency.rs` (its `post(...)` helper at `:168` builds a request and calls `router.oneshot(...)`) or `MoiraHttpServer::start(state)` + `reqwest` as in `tests/public_authorization.rs`. Inherits the harness's fail-closed rule: `MOIRA_TEST_DATABASE_URL` absent under `CI` ⇒ `panic!` (`tests/support/mod.rs:427-441`); absent locally ⇒ skip with a printed reason.

| Test function | Asserts |
|---|---|
| `ingest_rag_document_reports_pending_over_http_and_in_the_database` | `POST /api/v1/admin/rag-documents/{id}/ingest` → `200`, body `ingestion_status == "pending"`, **and** `SELECT ingestion_status FROM rag_document_versions WHERE id = <current_version_id>` also equals `'pending'`. Both halves required — the API could be honest while the DB audit trail stays false, which is exactly the defect P0-1 describes |
| `create_rag_document_with_inline_content_reports_pending_ingestion_status` | the second `'indexed'` write site (`src/infra/repositories/conversation.rs:1064`) is fixed **and** the in-transaction response-row re-select from §3 works — without the re-select this returns `null` and the test fails, which is exactly the trap it exists to catch |
| `create_rag_document_without_content_reports_null_ingestion_status` | `null`, not `"pending"` — proves `Option` semantics are real, not a default |
| `rag_document_get_and_list_report_the_same_ingestion_status` | `GET /api/v1/admin/rag-documents/{id}` and `GET /api/v1/admin/rag-collections/{collection_id}/documents` agree with the create/ingest response — proves the `LEFT JOIN` covers **every** `rag_document_select(` call site |
| `reindex_supersedes_the_previous_version_without_ever_writing_indexed` | `POST .../reindex` creates version N+1, marks version N `superseded_at`, and **no** row in `rag_document_versions` for this document ever holds `'indexed'` (`SELECT count(*) … WHERE ingestion_status = 'indexed'` is `0`). Also proves the existing supersession behavior did not regress |
| `repeated_ingest_with_the_same_idempotency_key_creates_two_versions_until_02b` | the **honest interim** contract: sending `Idempotency-Key` twice produces two version rows (no replay). This is deliberately an assertion of *current* behavior. **02b replaces this test with its inverse** (`repeated_ingest_with_the_same_key_replays_and_creates_exactly_one_version`) — the test name encodes the hand-off, and 02b's Definition of Done includes deleting it |
| `rag_document_error_responses_carry_catalog_message_keys` | `CONVENTIONS.md` §4 rule 5, second half: request a non-existent RAG document → the `ErrorDetail` envelope carries `code == "rag_document_not_found"`, `message_key == "moira.error.rag_document_not_found"`, and a **non-empty** `message`. **Do not call `moira::i18n::is_known_key` here** — `src/i18n/` is not declared in `src/lib.rs` (finding P0-5) and is therefore not compiled or reachable from `tests/`. Wiring it is 02b Wave 0 / plan 04 Wave 0; this test asserts the wire envelope only, and 02b upgrades it to a catalog assertion |

**No concurrency test in 02a.** Concurrency semantics on these routes are 02b's subject matter. 02a introduces no concurrent code path, so it adds no interleaving test — and therefore no `sleep()` either (`CONVENTIONS.md` §3; finding P2-12).

#### 12c. Not covered by automated tests

`docs/public-api.md`, `docs/conversation-memory-rag-api.md`, `docs/idempotency.md`, `docs/document-ingestion.md`, and `docs/todo.md` changes have no automated assertion (the markdown-drift gate is plan 06's). Verify manually that relative cross-references resolve, and record that check in the PR's Test evidence section.

---

## Multi-Agent Workflow

**Coordinator responsibilities.** Sequence the waves below, confirm each wave's writers have disjoint file sets before dispatch, run `cargo fmt --check` / `cargo clippy` / `cargo test` after each wave, and hold the OpenAPI assertions as the integration gate before merging Wave 3.

### Wave 1 (parallel, disjoint files)
- **Agent A — domain/schema:** `src/domain/conversation.rs` (add `ingestion_status` to `RagDocumentRecord`). No other file.
- **Agent B — docs:** `docs/public-api.md`, `docs/conversation-memory-rag-api.md`, `docs/idempotency.md`, `docs/document-ingestion.md`, `docs/todo.md`. No source files.

Checkpoint: A's new field must compile (even if unpopulated) before Wave 2 begins.

### Wave 2 (after Wave 1 Agent A; internally parallel across disjoint files)
- **Agent C — repository/row-mapping:** `src/infra/repositories/conversation.rs` (status literal fix at both write sites, `LEFT JOIN` in `rag_document_select`, create-path re-select) **and** `src/infra/pg_rows.rs` (status mapping pair + row mapper). Kept as one agent because the SQL projection and the mapper must stay in lockstep.
- **Agent D — HTTP/OpenAPI contract:** `src/http/conversation.rs` (description text only — **no parameter removals**) **and** `src/application/public.rs` (citations comment) **and** `src/domain/public.rs` (`citations` schema description).
- **Agent E — application-layer docs:** `src/application/conversation.rs` (doc comments on `ContextPlanner` and `prepare_response_conversation` only — no logic changes).

C, D, and E touch entirely disjoint files — no conflict risk.

### Wave 3 (after Wave 2 fully merged) — **two test layers, both mandatory**
- **Agent F — unit layer (§12a):** `src/http/mod.rs`, `src/infra/pg_rows.rs`, `src/domain/conversation.rs`. No database required.
- **Agent G — e2e layer (§12b):** the new `tests/rag_ingestion_honesty.rs` only. Requires real PostgreSQL 16 + pgvector via `MOIRA_TEST_DATABASE_URL`.

F and G are file-disjoint and may run in parallel, but **both** must land before the PR opens: `CONVENTIONS.md` §3 makes a single-layer plan unmergeable.

### Wave 4 — read-only reviewer
Re-read the full diff against Findings Addressed and confirm: (1) no `ConversationService` public method signature changed; (2) no route added/removed from `src/http/mod.rs`'s router table; (3) **`Idempotency-Key` still appears exactly four times in `src/http/conversation.rs`** (`grep -c 'Idempotency-Key' src/http/conversation.rs` = 4) — a removal is a hard review failure under `CONVENTIONS.md` §0 D1; (4) no 02b/plan-11 scope crept in (grep the diff for `rag_chunks`, `rag_chunk_embeddings`, `conversation_summaries`, `claim_idempotency`, `AdminCommandRunner` — none should appear).

**Conflict-avoidance strategy.** `src/http/conversation.rs` is owned solely by Agent D across the whole plan; `src/infra/repositories/conversation.rs` solely by Agent C. No two agents in the same wave touch the same file.

---

## Interfaces & Contracts

**Endpoints affected (no route added/removed, no path/method/parameter changes):**

| Method | Path | Change |
|---|---|---|
| POST | `/api/v1/admin/rag-collections` | OpenAPI: description updated (Sentences A+B); `Idempotency-Key` param **kept**, description updated |
| POST | `/api/v1/admin/rag-collections/{collection_id}/documents` | same; response body gains `ingestion_status` (`"pending"` when inline `content` created version 1, else `null`) |
| POST | `/api/v1/admin/rag-documents/{id}/ingest` | same; response body gains `ingestion_status` |
| POST | `/api/v1/admin/rag-documents/{id}/reindex` | same as `/ingest` (delegates to it, `src/http/conversation.rs:993-1000`) |
| GET | `/api/v1/admin/rag-documents/{id}` | response body gains `ingestion_status` |
| GET | `/api/v1/admin/rag-collections/{collection_id}/documents` | list items gain `ingestion_status` |
| POST | `/api/v1/conversations`, `/api/v1/conversations/{id}/messages`, `/api/v1/memories`, the four `put_*_policy` admin routes | OpenAPI description text only |

**Request/response shapes.** `RagDocumentRecord` gains:
```json
{ "id": "doc_…", "object": "rag.document", "…": "…", "ingestion_status": "pending" }
```
Value is one of the existing `RagIngestionStatus` snake_case variants or `null` if no version exists. Under this plan's scope only `pending` and `null` are ever produced; the others become reachable when plan 11 lands.

**Status codes.** Unchanged for all affected routes.

**Headers.** `Idempotency-Key` remains declared on the four RAG write routes and remains **accepted and ignored** at runtime. No `400`/`501` is introduced. This is binding: `CONVENTIONS.md` §0 D1 settled the "remove and reject" alternative — it is **not** an open product question and must not be reintroduced.

**Scopes/authorization rules.** Unchanged — `moira:rag-collections:write`, `moira:rag-documents:write`, `moira:rag-documents:ingest`, `moira:conversations:create`, etc.

**Error codes & i18n message keys** (binding: `plans/CONVENTIONS.md` §4).
- **Zero new error codes.** Verified against the real catalog: `src/i18n/catalog/errors.rs` holds the `moira.error.*` vocabulary, and the RAG/conversation/memory routes this plan touches already emit codes that exist there (`rag_collection_not_found`, `rag_document_not_found`, `rag_document_parse_failed`, `rag_document_type_unsupported`, `conversation_not_found`, `conversation_policy_disabled`, `memory_disabled`, …). `message_key` is derived mechanically as `format!("moira.error.{}", code())` (`src/error.rs:146-148`) into the `ErrorDetail { code, message_key, message, message_args, request_id, details }` envelope (`src/error.rs:52-65`).
- **Zero new notice strings.** The one new response field, `ingestion_status`, is a machine-readable enum value (`RagIngestionStatus`, `#[serde(rename_all = "snake_case")]`) — not human prose — so `CONVENTIONS.md` §4 rule 2 does not require a `moira.notice.*` entry. The MVP-boundary text lives in OpenAPI `description` attributes and markdown, **not** in any response payload.
- **Known limitation, discharged honestly:** `CONVENTIONS.md` §4 rule 5 asks each plan to assert its keys exist in the catalog. `src/i18n/` is currently **orphaned** — `src/lib.rs:3-11` declares no `pub mod i18n;` (finding **P0-5**), so `moira::i18n::is_known_key` is unreachable from `tests/` and the catalog is not compiled at all. 02a adds no keys, so it has nothing to assert; it discharges the *second* half of the rule (live responses carry non-empty `message_key` + `message`) via `rag_document_error_responses_carry_catalog_message_keys`. Module wiring and the first real catalog assertion are **02b Wave 0** (see `plans/02b-idempotency-replay.md`) / plan 04 Wave 0.

**Idempotency behavior.** Unchanged and explicitly untouched by this plan: none of these routes replay today. 02b implements it. 02a must not add `claim_idempotency`/`finalize_idempotency`/`AdminCommandRunner` usage anywhere.

**Transaction boundaries.** Unchanged in scope — `ingest_rag_document`'s existing `pool.begin()…commit()` (`src/infra/repositories/conversation.rs:1144-1209`) and `create_rag_document`'s (`:1021-1083`) keep their current boundaries; only the bound status value, the `LEFT JOIN` projection, and the position of `create_rag_document`'s response-row re-select change.

**Cache invalidation.** Not applicable — RAG documents are not part of `RuntimeConfigCache`.

**Concurrency behavior.** Unchanged — the existing `for update` row lock on `rag_documents` during ingest (`:1145-1147`) is untouched.

**SSE behavior.** Not applicable beyond the `citations` documentation note (§6).

---

## Verification

**Both test layers are required to merge** (`plans/CONVENTIONS.md` §3). Full per-test breakdown in Detailed Implementation §12.

**Layer 1 — unit (`#[cfg(test)] mod tests`, no database).** In `src/http/mod.rs`, `src/infra/pg_rows.rs`, `src/domain/conversation.rs`:
`rag_write_routes_still_declare_the_idempotency_key_parameter`, `rag_collection_document_route_keeps_its_collection_id_path_parameter`, `conversation_memory_rag_operations_document_the_mvp_preview_boundary`, `rag_write_routes_carry_the_interim_idempotency_disclaimer`, `rag_document_record_schema_exposes_ingestion_status`, `public_response_schema_documents_always_empty_citations`, `rag_ingestion_status_round_trips_all_eight_variants`, `rag_ingestion_status_from_db_rejects_unknown_value`, `rag_document_record_serializes_ingestion_status_as_snake_case`, `rag_document_record_round_trips_through_serde`.

**Layer 2 — e2e / integration (real HTTP surface, real PostgreSQL 16 + pgvector).** In the new `tests/rag_ingestion_honesty.rs`, following `tests/support/mod.rs` and imitating `tests/admin_idempotency.rs` / `tests/public_authorization.rs`:
`ingest_rag_document_reports_pending_over_http_and_in_the_database`, `create_rag_document_with_inline_content_reports_pending_ingestion_status`, `create_rag_document_without_content_reports_null_ingestion_status`, `rag_document_get_and_list_report_the_same_ingestion_status`, `reindex_supersedes_the_previous_version_without_ever_writing_indexed`, `repeated_ingest_with_the_same_idempotency_key_creates_two_versions_until_02b`, `rag_document_error_responses_carry_catalog_message_keys`.

**Concurrency discipline.** 02a adds **no** concurrency test (none is warranted — no concurrent code path is introduced) and therefore no `sleep()`-based interleaving. 02b owns the barrier-gated concurrency coverage for these routes (`CONVENTIONS.md` §3; finding P2-12).

**i18n verification** (`CONVENTIONS.md` §4 rule 5). Zero new error codes and zero new notice strings ⇒ no new catalog entry to assert. The live-response half of the rule is discharged by `rag_document_error_responses_carry_catalog_message_keys`. The catalog module itself is orphaned (P0-5) and is wired by 02b Wave 0 / plan 04 Wave 0 — stated here so the gap is tracked, not silently skipped.

- Migration validation: **N/A** — no migration; still run the standard clean-PostgreSQL migration gate to confirm no accidental schema drift.
- Security/secret-leak: not applicable (no secret-bearing fields touched) — run `src/security/masking::tests` unchanged as a regression check.
- Required Rust gates, run verbatim and must pass clean:
  - `cargo fmt --check`
  - `cargo clippy --workspace --all-targets --all-features -- -D warnings`
  - `cargo test --workspace --all-features`
  - clean PostgreSQL migration validation (existing CI migration-contract job)
  - `cargo build --release --locked`

---

## Definition of Done

- [ ] `RagDocumentRecord` (JSON response on every route that returns it) includes `ingestion_status`, verified by an HTTP integration test reading the actual response body — not just the OpenAPI schema.
- [ ] `rag_document_versions.ingestion_status` is never written as `'indexed'` by any reachable code path (grep for the literal `'indexed'` in `src/infra/repositories/conversation.rs` returns zero **write-context** matches; the supersession `CASE` comparison at `:1170` may remain).
- [ ] Generated OpenAPI (`documented_router().into_openapi()`) **still** declares `Idempotency-Key` on all four RAG write operations, verified by `rag_write_routes_still_declare_the_idempotency_key_parameter` — the parameter was **not** removed (`CONVENTIONS.md` §0 D1).
- [ ] Every operation named in §5 carries the Sentence-A boundary description, verified by an automated test rather than manual inspection.
- [ ] `docs/public-api.md`, `docs/conversation-memory-rag-api.md`, `docs/idempotency.md`, and `docs/document-ingestion.md` contain the MVP-boundary statement, and a human reviewer confirms the wording matches actual code behavior (no doc drift).
- [ ] `docs/todo.md` Phase 5 preamble and Phase 2 idempotency-extension line reference 02a / 02b / plan 11 without being deleted.
- [ ] All five required Rust gates pass with zero warnings/failures.
- [ ] No `ConversationService`, `PgConversationRepository`, or `PublicResponse` public method/field was removed or had its signature changed in a way that breaks an existing caller (only additive changes).
- [ ] `src/application/conversation.rs` still contains **zero** references to `idempotency`, `claim_idempotency`, `finalize_idempotency`, or `AdminCommandRunner` — 02a must not pre-empt 02b.
- [ ] The Wave 4 reviewer confirms no 02b/plan-11 scope (idempotency ledger wiring, chunking, embeddings, retrieval, summarization) was implemented under this plan.

### Cross-cutting compliance checklist (`plans/CONVENTIONS.md` §8 — binding)

- [ ] Work performed on branch `plan/02a-mvp-boundary-honesty`, cut from current `main`; PR opened with **all seven** required description sections (Plan link · Findings addressed · Migrations included · Breaking API/OpenAPI changes · Test evidence · Rollback procedure · Deferred follow-ups).
- [ ] All gates in `CONVENTIONS.md` §2 pass: `cargo fmt --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace --all-features`, `cargo build --release --locked`, plus clean PostgreSQL migration validation from an empty database.
- [ ] **Unit tests delivered and passing** — all ten functions named in Verification Layer 1, in `#[cfg(test)] mod tests` beside the code, requiring no database.
- [ ] **E2E tests delivered and passing** — all seven functions named in Verification Layer 2, in `tests/rag_ingestion_honesty.rs`, driving the real HTTP surface against real PostgreSQL 16 + pgvector via `tests/support/mod.rs`, fail-closed under `CI`.
- [ ] No new concurrency test uses `sleep()` — 02a adds no concurrency test at all; the barrier-gated coverage is 02b's.
- [ ] Every new error/notice string has an i18n key + English default in `src/i18n/catalog/errors.rs`/`notices.rs`, mirrored into `docs/i18n-response-catalog.json`, with a test asserting presence. **This plan adds zero new keys**, so the obligation is discharged by `rag_document_error_responses_carry_catalog_message_keys` proving live responses carry a non-empty `message_key` + `message`. The orphaned-catalog gap (P0-5) is explicitly handed to 02b Wave 0 / plan 04 Wave 0 and is not silently ignored.
- [ ] Frontend conventions (`CONVENTIONS.md` §5/§6) — **N/A**, this plan ships no console code.
- [ ] Auth conventions (`CONVENTIONS.md` §7) — **N/A**, no authentication/authorization behavior is touched.
- [ ] No secret-leak, verified by test — `src/security/masking::tests` passes unchanged; this plan introduces no secret-bearing field.
- [ ] Plan lands **before** plan 05's OpenAPI-drift gate freezes the spec (`CONVENTIONS.md` §1 rule 6), and the branch is **not** force-pushed after `plan/02b-idempotency-replay` branches from it (§1 rule 7).
- [ ] **Done means merged.** Every box above is verified by a named, passing test — "implemented" is not "done."

---

## Risks & Rollback

**Security risks.** None introduced — this plan removes a false claim and adds an honest status field; it does not touch authentication, authorization, or credential handling.

**Data-migration risks.** None — no migration. The only data-shape change is the *meaning* of a value already-written rows may hold (`'indexed'`) versus new rows (`'pending'`). **Note for implementers:** RAG documents ingested before this ships will have `ingestion_status = 'indexed'` already persisted (a stale, false value) and will keep reporting `"indexed"` until re-ingested. The one-time backfill `UPDATE rag_document_versions SET ingestion_status = 'pending' WHERE ingestion_status = 'indexed'` is **recommended** — the audit's own framing is that `'indexed'` was always false, so there is no legitimate prior state to preserve. If taken, name it in PR section 3 and note the inverse `UPDATE` in the rollback procedure.

**Compatibility risks.** Adding `ingestion_status` is purely additive. No parameter is removed, so no codegen-based client SDK breaks.

**Interim-honesty risk (the cost of the split).** Between 02a merging and 02b merging, the spec advertises `Idempotency-Key` on four routes that do not replay. This is a **deliberate, time-boxed** consequence of `CONVENTIONS.md` §0 D2, mitigated by: (a) the parameter's own description saying replay is not implemented yet; (b) Sentence B on each operation; (c) the explicit paragraph in `docs/public-api.md` and `docs/idempotency.md`; (d) 02b being stacked directly on this branch so the window is one PR wide. **Mitigation owner:** if 02b slips past plan 05's OpenAPI freeze, the coordinator must re-evaluate — a frozen spec that advertises unimplemented replay would convert a temporary gap into a permanent one.

**Deployment risks.** None beyond a standard rolling deploy; no new config, no new infra.

**Rollback procedure.** `git revert` of the merge commit — no migration to roll back and no data written by this plan that a rollback must reverse, aside from the optional backfill, which is revertible by the inverse `UPDATE`. If 02b has already merged on top, revert 02b first (it depends on this branch's `RagDocumentRecord` shape).

**Deliberately deferred follow-ups (tracked elsewhere, not dropped):**
- Real idempotency replay for the RAG write routes — **`plans/02b-idempotency-replay.md`** (stacked on this branch).
- Full RAG/memory pipeline (chunking, embeddings, retrieval, context injection, citations) — plan 11.
- Wiring `pub mod i18n;` and making the catalog load-bearing (P0-5) — 02b Wave 0 / plan 04 Wave 0; the missing entries and the every-emitted-code test in plans 05/06.
- Automated markdown/OpenAPI drift gating for the honesty text this plan writes — plan 05 (OpenAPI drift) and plan 06 (i18n catalog ↔ JSON mirror drift). Until then, drift is caught only by `conversation_memory_rag_operations_document_the_mvp_preview_boundary` and by review.
- Keyset pagination on the RAG list endpoints (P1-4) — plan 04.
