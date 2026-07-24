# Iteration 11 — RAG & Memory Intelligence

Post-MVP. Addresses **P3-8** and closes the real implementation work behind **P0-1**'s MVP descope (plan 02 made the API *honest*; this plan makes the underlying capability *real*). Companion source: `00-audit-report.md`, `01-roadmap-and-dependencies.md`, `docs/todo.md` Phase 5.

---

## Summary

**Objective.** Implement the actual retrieval-augmented generation and memory intelligence pipeline that today is persisted-but-inert: document chunking, Rig-backed embeddings for memory and RAG, semantic/keyword/hybrid retrieval with strict tenant isolation, a real context planner (replacing the `ContextPlanner` stub), conversation summarization, automatic memory extraction, and response citation population from real provenance.

**Why ordered here.** Plan 02 (MVP boundary honesty) already made the conversation/memory/RAG endpoints **truthful** — they are documented and behave as persistence/configuration primitives, and `ingestion_status`/citations no longer overclaim. This plan is the large, genuinely-post-MVP body of work that plan 02 explicitly deferred (`00-audit-report.md` P0-1 correction: "(b) implement the pipeline (large, post-MVP)... MVP path = (a)"). It depends only on `I02` (the honest API contract this plan will fill in) per the roadmap graph; it does **not** depend on multi-replica readiness (`I10`) for correctness, only benefits from it at scale (`I10 -.enables scaled.-> I11`).

**User-visible outcome.** `POST /v1/responses` calls against a conversation with memory/RAG enabled actually inject relevant history, summaries, memories, and retrieved document chunks into the model's context, and the response's `citations` field is populated with real provenance instead of always being empty. RAG document ingestion actually chunks and embeds content instead of only storing raw text with a hardcoded `'indexed'` status. Conversations past the configured message/token budget get real, versioned summaries instead of unbounded raw history.

**Included scope.** Document chunking (paragraph/Markdown/token-window strategies, UTF-8-safe, deterministic hashes). Rig embedding integration for both memory and RAG paths (batch limits, model/dimension versioning, cancellation, supersession). Semantic (vector), keyword, and hybrid retrieval with per-application/tenant/user isolation, score thresholds, and provenance recording. A real `ContextPlanner` (bounded history + latest summary + memory candidates + RAG chunks, safely injected, persisted to `context_plans`). Conversation summarization (immutable versions, singleflight, manual + policy-triggered). Automatic memory extraction (structured output, confidence/type/sensitivity validation, consent enforcement, dedupe/contradiction handling). Response citation population from `retrieval_runs`/`context_plans` provenance. Finishing direct-text RAG ingestion end-to-end (chunks + chunk embeddings, not metadata only).

**Excluded scope.** Remote-URL RAG ingestion with SSRF hardening (`docs/todo.md:73`) — flagged as a follow-up sub-phase, not required for the core pipeline to be real; direct-text ingestion is sufficient to prove the pipeline end-to-end. Conversation export packaging and deletion-propagation for derived artifacts (`docs/todo.md:79`) — a smaller, separable follow-up. Multi-replica distributed worker execution of the retry/backoff workers this plan's job bodies plug into — that plumbing is plan 10's scope; this plan only needs the job **bodies** to exist and be safely idempotent/re-runnable, wherever they are invoked from (single-process supervisor today, distributed queue after plan 10). Rig's `Agent`/tool-calling path (P3-6) — out of scope; this plan uses direct completion only, matching current execution architecture. Custom/pluggable embedding providers beyond what Rig's official embedding client surfaces support today.

---

## Branch & Pull Request

Binding: `plans/CONVENTIONS.md` §1. Where anything below conflicts with CONVENTIONS.md, **CONVENTIONS.md wins**.

**Branch of record:** `plan/11-rag-memory-intelligence` — branched from the **current `main`** (§1.1). This plan is **post-MVP**.

**Shape: a STACKED SERIES of PRs, not one PR.** This is the deliberate exception to the usual one-plan-one-PR default, and the reason is concrete: plan 11 spans eight sub-phases (A–H) across seven internal waves, touches the highest-security-sensitivity code in the repository (the prompt-injection boundary and the cross-tenant vector-isolation SQL), and would otherwise produce a single unreviewable diff. CONVENTIONS.md §1.1 explicitly contemplates stacking; §1.4's description requirements and §1.5's "done means merged" apply **to every PR in the stack**, not just the last one.

`plan/11-rag-memory-intelligence` is the **integration branch**. Nothing is committed to it directly — it only receives merges from sub-phase branches, and it is what finally opens a PR against `main`.

| PR | Branch | Base branch / base PR | Sub-phase (Wave) | Independently reviewable deliverable |
|---|---|---|---|---|
| **11.0** | `plan/11-w0-embedding-spike` | `plan/11-rag-memory-intelligence` | Wave 0 | **Docs only.** Verified Rig embedding API surface + pgvector query-plan findings written into `docs/rig-integration.md` and `docs/pgvector-benchmarks.md`. No `src/` changes. Must merge first — everything downstream depends on it not guessing. |
| **11.A** | `plan/11-a-chunking` | 11.0 | A (Wave 1) | `src/orchestration/chunking.rs` + unit tests. Pure logic, no DB — fully self-contained. |
| **11.B** | `plan/11-b-embeddings` | 11.0 | B (Wave 1) | `src/orchestration/embedding.rs` + unit tests. **Sibling of 11.A, not stacked on it** — both branch from 11.0 and are reviewed in parallel. |
| **11.AB** | `plan/11-ingestion` | merge of 11.A **and** 11.B | A/B (Wave 2) | Ingestion pipeline: chunks + embeddings + `rag_ingestion_runs` + honest `ingestion_status` progression. First PR with an e2e proof of P0-1's root cause being fixed. |
| **11.C** | `plan/11-c-retrieval` | 11.AB | C (Wave 3) | `src/orchestration/retrieval.rs` + repository methods. **Carries the cross-tenant isolation e2e suite — this PR does not merge without it.** |
| **11.D** | `plan/11-d-context-planner` | 11.C | D (Wave 4) | Real `ContextPlanner`. **Mandatory read-only security review before merge** (prompt-injection boundary). |
| **11.E** | `plan/11-e-summarization` | 11.D | E (Wave 5) | Summarization + `POST /v1/conversations/{id}/summarize`. |
| **11.F** | `plan/11-f-memory-extraction` | 11.E | F (Wave 5) | Automatic memory extraction. **Sequenced after 11.E rather than parallel**, because both edit `src/application/conversation.rs` (the plan's highest-conflict file). |
| **11.G** | `plan/11-g-citations` | 11.F | G (Wave 6) | Real citation population from provenance. |
| **11.H** | `plan/11-h-idempotency-audit` | 11.G | H (Wave 7) | `If-Match`/idempotency consistency pass. |
| **11.final** | `plan/11-rag-memory-intelligence` | `main` | — | Integration PR. Opens only after 11.H merges into the integration branch. |

**Stacking rules (all binding):**
1. Every sub-phase PR description **must name its base PR** by number and link (§1.1) — e.g. "Base PR: #11.C `plan/11-c-retrieval`". A PR with no named base, or one claiming `main` as base when it is stacked, is rejected in review.
2. Every sub-phase branch is **rebased once its base merges** (§1.1), then re-runs the full §2 gate set before its own review resumes.
3. **Never force-push a branch another PR is stacked on** (§1.7). Because this stack is nine deep, this is the single highest-risk process rule in the plan: a force-push to 11.C silently corrupts 11.D through 11.H. If history must be rewritten, close and re-open the downstream PRs rather than force-pushing under them.
4. **Every PR in the stack independently satisfies §2** — `cargo fmt --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace --all-features`, `cargo build --release --locked`, clean migration application. A stack member that only compiles when combined with a later member is not a valid stack member.
5. **Every PR in the stack independently delivers unit *and* e2e tests** for what it adds (§3). No PR defers "tests come later in the stack."
6. Every PR carries all seven §1.4 description sections: **Plan link** (`plans/11-rag-memory-intelligence.md`) · **Findings addressed** (`P0-1` root cause, `P3-8`, plus `P2-7` for the embedding-dimension resolution in 11.B) · **Migrations included** (expected "none" for every PR — see § DB/migration changes; only the embedding-dimension Open Decision could produce one) · **Breaking API/OpenAPI changes** · **Test evidence** · **Rollback procedure** · **Deferred follow-ups**.
7. **Done means merged** (§1.5). Neither an individual sub-phase nor the plan is done at PR-open; the plan is done when **11.final** merges with all gates green and every Definition-of-Done bullet objectively verified by a named, passing test.

**OpenAPI ordering (§1.6).** This plan **does** change the OpenAPI surface — it adds `POST /v1/conversations/{id}/summarize` and two `moira:diagnostics:read` endpoints (§ API & OpenAPI changes). Per §1.6, OpenAPI-changing plans must land before plan 05's drift gate freezes the spec. Given plan 11 is last in the roadmap, the realistic reading is the inverse: **plan 05's drift gate will already be in force**, so PRs 11.E and 11.D/11.G must regenerate and commit the spec in the same PR as the route change, and the `moira-openapi` skill must be invoked per the repo's own `CLAUDE.md`. Treat spec drift as a merge blocker, not a follow-up.

**Why not one PR per wave instead of per sub-phase:** waves 1 and 5 each contain two agents working disjoint files. Wave 1's two outputs (A, B) are genuinely independent and get sibling PRs. Wave 5's two (E, F) are *not* independent — they collide on `src/application/conversation.rs` — so they are serialized into stacked PRs rather than sibling ones. The table above already encodes this; do not re-parallelize 11.E/11.F.

---

## Findings Addressed

- **P0-1** (root cause, already descoped honestly by plan 02; this plan does the deferred implementation): `src/infra/repositories/conversation.rs:1138-1210` `ingest_rag_document` stores `content_plain` into `rag_document_versions` and hardcodes `ingestion_status = 'indexed'` (see line 1184: `values ($1, $2, $3, $4, $5, $6, $7, $8, 'indexed', $9)`) without ever writing `rag_chunks` or `rag_chunk_embeddings`. `src/application/public.rs:1657` `citations: Vec::new()` unconditionally. `src/application/conversation.rs:37-49` `ContextPlanner::deterministic_phase_five_order()` returns a hardcoded `[&str; 8]` array and is never used to actually assemble a prompt; `prepare_response_conversation` (`:314-381`) only stores the user's message as a `conversation_messages` row and returns a `ConversationExecutionLink` — it never loads prior history, a summary, memory candidates, or RAG chunks into the execution request.
- **P3-8**: full RAG/memory intelligence is Phase 5 of `docs/todo.md` — see the complete TODO list below, each mapped to a sub-phase in this plan.
- **Verified absence** (per audit): summarization (`conversation_summaries` is defined in migration 0007 but never written by any code — verified via grep across `src/application/conversation.rs`, `src/infra/repositories/conversation.rs`), memory extraction, memory embeddings, and semantic retrieval have **no implementing code** anywhere in the repository today.
- **`docs/todo.md` Phase 5 (lines 62-82), verbatim mapping to this plan's sub-phases**:
  - `:64` context planner → Sub-Phase D.
  - `:65` safe context injection (no untrusted text as system instructions) → Sub-Phase D.
  - `:66` live summarization, immutable versions, singleflight → Sub-Phase E.
  - `:67` automatic memory extraction, validation, consent, dedupe/contradiction → Sub-Phase F.
  - `:68` memory embeddings via Rig, batch/versioning/cancellation/supersession → Sub-Phase B.
  - `:69` semantic memory retrieval, isolation, thresholds, usage counters → Sub-Phase C.
  - `:70` retrieval service combining memory+RAG, `retrieval_runs`, diagnostic scopes → Sub-Phase C.
  - `:71` chunking strategies, UTF-8 safety, deterministic hashes → Sub-Phase A.
  - `:72` finish direct-text ingestion (chunks + embeddings) → Sub-Phase A/B.
  - `:73` remote URL ingestion with SSRF hardening → **excluded**, flagged follow-up.
  - `:74` Rig embedding integration + document API/version assumption → Sub-Phase B (open item, see below).
  - `:75` RAG vector/keyword/hybrid retrieval, diversity, provenance → Sub-Phase C.
  - `:76` populate `citations` from provenance → Sub-Phase G.
  - `:77` (already done by plan 02 — honest MVP scoping).
  - `:78` tokenizer-aware context budgeting, `context_length_exceeded` → Sub-Phase D.
  - `:79` export/deletion propagation → **excluded**, flagged follow-up.
  - `:80` `If-Match`/idempotency consistency for Phase 5 endpoints → Sub-Phase H (cross-cutting, verify against plan 04's patterns).
  - `:81` full route/security/concurrency tests incl. cross-tenant vector isolation, no prompt/content leakage → Verification section, all sub-phases.
  - `:82` pgvector HNSW query-plan/benchmark docs → Verification section.
- **Embedding-dimension risk** (P2-7, `00-audit-report.md`): `migrations/0007_conversations_memory_rag.sql:288,421` hardcode `embedding vector(1536)` in both `memory_embeddings` and `rag_chunk_embeddings`. `application_embedding_policies.embedding_dimension` (`migrations/0007…:103`) is a nullable, policy-level *declared* dimension with **no enforcement** tying it to the fixed `1536` column type today. Sub-Phase B must resolve this (see Detailed Implementation).

---

## Architecture

### Components & ownership (per `docs/project-structure.md`)

| Component | Module | Notes |
|---|---|---|
| Chunking strategies (paragraph/Markdown/token-window) | `src/orchestration/chunking.rs` (new) | Pure text-transform logic; belongs in `orchestration` (Moira behavior), not `infra` (no DB access) or `domain` (not a DTO). |
| Embedding client wrapper (Rig) | `src/orchestration/embedding.rs` (new) | Provider-facing execution behavior stays in `orchestration`, mirroring how completion execution already lives there — keeps Rig-specific calls behind a Moira-owned boundary per `docs/project-structure.md`'s "keep provider-specific calls behind Rig-compatible boundaries" guidance. |
| `ContextPlanner` (real implementation) | `src/application/conversation.rs` (replaces the stub at 34-49) | Already the home of `ConversationService`; the planner is an application-layer orchestration of `orchestration`/`infra` primitives, consistent with existing placement. |
| Retrieval service (vector/keyword/hybrid) | `src/orchestration/retrieval.rs` (new) | Runtime behavior; queries `infra` repositories, returns ranked candidates to `application`. |
| Summarization service | `src/application/conversation.rs` (new methods on `ConversationService`) | Reuses the existing execution kernel per `docs/todo.md:66` — calls into `src/application/execution.rs`'s existing completion path, not a new engine. |
| Memory extraction service | `src/application/conversation.rs` (new methods) or a new `src/application/memory_extraction.rs` if the file grows too large (defer file-split decision to implementation; `AdminService`'s 1,873-line god-object is a known anti-pattern per P2-1 — do not repeat it here) | Structured-output completion call + validation, same reuse-execution-kernel pattern. |
| Repository additions (chunks, embeddings, summaries, extraction runs, retrieval runs, context plans) | `src/infra/repositories/conversation.rs` (extend `PgConversationRepository`) | `infra` already owns this repository; no new repository file needed — matches existing single-repo-per-bounded-context pattern (P2-3 notes this repo currently lacks a trait; plan 06 addresses trait coverage — this plan should add a trait if plan 06 has already landed, otherwise stay concrete-only to avoid conflicting with plan 06's separate refactor). |
| Row mapping for new/extended tables | `src/infra/pg_rows.rs` | Per `docs/project-structure.md` feature-placement guidance. |
| Domain DTOs (citation provenance detail, chunking strategy enum, retrieval diagnostics) | `src/domain/conversation.rs` (extends existing `PublicCitation` at 258-267 and related types) | `domain` stays dependency-light per boundaries. |

### Data flow

**Ingestion (RAG document → retrievable chunks):**
1. `ConversationService::ingest_rag_document` (`src/application/conversation.rs:947-979`, currently calls `self.repo.ingest_rag_document` which only writes `rag_document_versions`) is extended: after the existing version-row insert (`src/infra/repositories/conversation.rs:1138-1211`), a new step calls the Sub-Phase A chunker on `content_plain`, inserts `rag_chunks` rows (one per chunk, deterministic `chunk_hash`), then calls the Sub-Phase B embedding client in batches to populate `rag_chunk_embeddings`, and finally flips `rag_document_versions.ingestion_status` from `'chunking'`→`'embedding'`→`'indexed'` (states already defined in the migration 0007 check constraint at `rag_document_versions.ingestion_status`, line 382-383) as each step completes — so a partially-completed ingestion is honestly observable via status, not silently marked `'indexed'` early as today.
2. A `rag_ingestion_runs` row (migration 0007:431-444, already defined, never written) is created at the start and updated with `chunk_count`/`embedded_chunk_count`/`status` as ingestion proceeds — this table exists purely for this plan to finally populate.

**Retrieval (query → ranked memory + RAG candidates):**
1. `ContextPlanner` (Sub-Phase D), given a conversation + the current user turn, calls the Sub-Phase C retrieval service twice — once scoped to `memory_records`/`memory_embeddings`, once to `rag_chunks`/`rag_chunk_embeddings` — each call strictly filtered by `application_id` + (`external_tenant_id`/`external_user_id`) matching the acting `Actor`, mirroring the exact isolation predicate already proven correct for conversations/memories in `conversation_access`/`can_read_all` (`src/application/conversation.rs:1040-1065`).
2. Each retrieval call records one `retrieval_runs` row (migration 0007:467-486, already defined) capturing `memory_candidate_count`/`memory_returned_count`/`chunk_candidate_count`/`chunk_returned_count`/`latency_ms`/`status` — this is the provenance source for both diagnostics and (indirectly, via the chunks/memories it returned) citations.
3. `ContextPlanner` assembles the final prompt content in the fixed, safety-ordered sequence already declared (and now actually enforced, not just returned as a constant) by `deterministic_phase_five_order()` (`protected_instructions`, `current_input`, `tool_state`, `recent_messages`, `conversation_summary`, `retrieved_memory`, `retrieved_rag`, `older_history`) and writes one `context_plans` row (migration 0007:446-465, already defined) recording exactly which message/summary/memory/chunk IDs were included and why anything was excluded (`excluded_counts`, `truncation_reason`).
4. The assembled context is handed to `src/application/execution.rs`'s existing completion path as additional `rig_core::completion::Message` history entries — **critically, retrieved memory/RAG text is always injected as conversation-role content (e.g. a clearly-delimited context/user-adjacent message), never concatenated into or treated as a system/developer instruction**, per `docs/todo.md:65`'s explicit safety requirement. This is the single most important security property of Sub-Phase D and must be unit-tested with adversarial retrieved content (e.g. a RAG chunk containing "ignore previous instructions...").

**Summarization:** triggered either manually (new admin/public-scoped endpoint, TBD in Interfaces) or policy-driven when `application_conversation_policies.summarization_enabled` and the conversation's un-summarized token count exceeds `summary_trigger_tokens` (migration 0007:13-15, already defined policy fields). A singleflight lock (reuse the same `pg_try_advisory_xact_lock` pattern already proven at `src/infra/repositories/admin.rs:567` for idempotency, keyed on `conversation_id`) prevents two concurrent summarization runs for the same conversation. The summarization call reuses `src/application/execution.rs`'s existing completion kernel (no second execution engine, per the Moira/Rig boundary) and writes an immutable `conversation_summaries` row (migration 0007:194-210) with `summary_version` incremented and the prior active summary's `superseded_at` set — never mutated in place.

**Memory extraction:** triggered after a response completes (hook point: `ConversationService::record_assistant_response`, `src/application/conversation.rs:383-421`, currently only persists the assistant message) when `application_memory_policies.automatic_extraction_enabled` is true. Calls the completion kernel with a structured-output request (JSON schema constraining `memory_type`/`confidence`/`sensitivity`/`content`), validates each candidate against `allowed_memory_types`/`allowed_sensitivity_levels`/`minimum_extraction_confidence` (migration 0007:42,46,50-51, already defined policy fields), deduplicates against existing active memories (via `content_hash` exact-match plus a semantic-similarity near-duplicate check using the Sub-Phase B embedding), and either inserts a new `memory_records` row (`status='candidate'` or `'active'` depending on `consent_mode`) or marks contradiction via `contradicts_memory_id`/`resolution_status` (both already-defined columns, migration 0007:254-255). A `memory_extraction_runs` row (migration 0007:298-314, already defined) records the run.

### Security boundaries — cross-tenant vector isolation

This is the highest-risk property in this plan and must be enforced at the SQL layer, not only the application layer. It is a per-query property of bound parameters — it must hold identically under today's single-replica deployment and owes nothing to plan 10's distributed controls (no replica-count, Redis, or lease mechanism participates in it):

- Every vector query (`memory_embeddings`, `rag_chunk_embeddings`) **must** join through to a table carrying `application_id` (`memory_records.application_id`, `rag_chunks.collection_id → rag_collections.application_id`) and filter on it in the *same query* as the `<->`/cosine-distance ORDER BY — never filter application/tenant scope in application code after fetching top-K unscoped results (that would leak cross-tenant nearest-neighbor information even if results are discarded before returning to the caller, since HNSW result ordering itself could be inferred/timed).
- `external_tenant_id`/`external_user_id` scoping follows the exact same `coalesce(external_tenant_id, '')`/`coalesce(external_user_id, '')` pattern already used in the migration 0007 indexes (`memory_records_scope_cursor_idx`, migration 0007:270-281) — reuse this predicate shape in the retrieval SQL so the query planner can use the existing indexes (extend them with a vector-similarity-compatible index if `EXPLAIN ANALYZE` in Verification shows the existing HNSW partial index isn't selected under the added equality filters — HNSW indexes in pgvector do not support pre-filtering natively pre-PG16/pgvector versions with iterative scan; verify the deployed pgvector version's filtered-HNSW support before assuming the naive `WHERE application_id = $1 AND ... ORDER BY embedding <=> $2` plan is efficient, and document the actual measured plan).
- `RagCollectionRecord.visibility` (`'application' | 'tenant' | 'restricted'`, migration 0007:330-331) must be enforced as an additional filter — a `'restricted'` collection is never a retrieval candidate unless the policy's `allowed_collection_ids` (migration 0007:76) explicitly includes it for the acting application.
- A dedicated **cross-tenant isolation integration test** (Verification section) creates two applications with semantically near-identical memory/RAG content and asserts application A's retrieval never returns application B's rows, even when B's content scores higher by raw cosine similarity than any of A's own content — this is the test that actually proves the SQL-level filter (a purely application-level post-filter would pass a weaker version of this test but fail a timing/inference-based variant, which is why the SQL-level requirement above is non-negotiable).

### DB/migration changes

**No new tables required** — migration 0007 already defines every table this plan needs (`memory_embeddings`, `rag_chunks`, `rag_chunk_embeddings`, `conversation_summaries`, `memory_extraction_runs`, `rag_ingestion_runs`, `context_plans`, `retrieval_runs`, `application_embedding_policies`). This plan is primarily an application/orchestration-layer implementation against an already-correct schema.

One **additive** migration is likely still needed, scoped narrowly:
- Resolving the embedding-dimension hardcode (P2-7): if the product decision (see Open Decisions) is to support more than one embedding dimension across applications, `memory_embeddings.embedding`/`rag_chunk_embeddings.embedding` need either (a) a per-dimension-bucket set of columns/tables (ugly, avoid), or (b) confirmation that Moira standardizes on exactly one embedding model/dimension (1536, matching the existing column type — e.g. `text-embedding-3-small`) for the MVP-of-this-feature, with `application_embedding_policies.embedding_dimension` used only for **validation** (reject a configured embedding model whose dimension ≠ 1536) rather than as a variable schema driver. **Recommendation**: option (b) for this iteration — it requires no migration at all, only an application-layer validation check in `put_embedding_policy` (`src/application/conversation.rs:721-747`) rejecting a mismatched `embedding_dimension`. Flag as an explicit open decision below; only write a migration if the decision is (a).
- If retrieval query plans (Verification) show the existing HNSW partial indexes (migration 0007:294-296, 427-429) don't compose well with the added application/tenant equality filters, a follow-up migration adding a composite btree+HNSW or a partial index per common `(application_id)` value may be needed — defer to actual `EXPLAIN ANALYZE` evidence gathered during Sub-Phase C implementation, do not pre-guess the index shape.

Sketch of the retrieval query shape Sub-Phase C is expected to issue against `memory_embeddings` (the `rag_chunk_embeddings` query mirrors this via `rag_chunks`/`rag_collections` joins for `application_id`/visibility filtering):

```sql
select m.id, m.public_id, m.memory_type, m.content_plain, m.importance, m.confidence,
       (e.embedding <=> $1) as distance
from memory_embeddings e
join memory_records m on m.id = e.memory_id
where e.superseded_at is null
  and m.deleted_at is null
  and m.status = 'active'
  and m.application_id = $2
  and coalesce(m.external_tenant_id, '') = coalesce($3, '')
  and coalesce(m.external_user_id, '') = coalesce($4, '')
order by e.embedding <=> $1
limit $5;
```

This query's `where` clause is the non-negotiable cross-tenant isolation boundary described above — `application_id`/`external_tenant_id`/`external_user_id` are bound parameters evaluated in the same query as the ORDER BY, never as a post-fetch filter. Wave 0's spike must confirm via `EXPLAIN ANALYZE` whether the planner uses `memory_embeddings_active_hnsw_idx` (migration 0007:294-296) directly under this combined filter, or whether pgvector's iterative-scan / a supplementary btree index on `(application_id, external_tenant_id, external_user_id)` over `memory_records` is needed for the planner to avoid a large HNSW scan followed by discard.

### API & OpenAPI changes

- `POST /v1/conversations/{id}/summarize` (new, admin+public-scope-gated manual trigger) — request: `{}` (no body needed, or optional `{"force": bool}` to bypass the trigger-token threshold); response: `ConversationSummaryRecord` (new DTO mirroring `conversation_summaries` columns). `202 Accepted` if a summarization is already in flight (singleflight lock held) with `Retry-After`, `200 OK` with the new/existing active summary otherwise.
- Existing `RagDocumentIngestRequest`/ingestion endpoint (`src/http/conversation.rs`, `RagDocumentIngestRequest` at `src/domain/conversation.rs:611-617`) — no shape change, but the **response** `RagDocumentRecord`'s underlying `rag_document_versions.ingestion_status` now genuinely progresses through `pending→chunking→embedding→indexed` (or `→failed`) instead of jumping straight to `indexed`; document this behavior change in the OpenAPI description (per `moira-openapi` skill guidance — must be run when this ships).
- `PublicResponse.citations` (`src/domain/public.rs:167`, `PublicCitation` at `src/domain/conversation.rs:258-267`) — no shape change to `PublicCitation` itself (already has `id`, `citation_type`, `document_id`, `memory_id`, `title`, `section` — sufficient for both RAG-chunk and memory provenance), only its population changes from `Vec::new()` (`src/application/public.rs:1657`) to real entries derived from the `context_plans`/`retrieval_runs` rows written during the request.
- New diagnostic-scope-gated endpoint (per `docs/todo.md:70`'s "diagnostic metadata only through diagnostic scopes"): `GET /v1/admin/conversations/{id}/context-plans/{execution_id}` and `GET /v1/admin/conversations/{id}/retrieval-runs` for operators debugging why a particular retrieval did/didn't surface expected content — gated behind a new `moira:diagnostics:read` scope, never exposed to ordinary callers (protects retrieval internals/scoring from being reverse-engineered by end users).
- `context_length_exceeded` error (`docs/todo.md:78`) — new `AppError` variant / `message_key`, returned as `422` (matching the existing `AppError::unprocessable` pattern used throughout `src/application/conversation.rs`) when the context planner cannot fit required content (protected instructions + current input + minimum viable recent-history) within `application_conversation_policies.maximum_history_tokens` even after excluding all optional content (summary, memory, RAG) — never silently truncate required content.

### Backward compatibility

- All existing conversation/memory/RAG endpoints keep their current request/response shapes; this plan changes *behavior* (ingestion actually indexes, citations actually populate, responses actually get better context) without breaking existing clients. The one explicit behavior change requiring a documentation update is `ingestion_status` progression (no longer instantaneous `'indexed'`).
- Conversations/memories/documents created before this plan ships have no chunks/embeddings/summaries — they remain valid, retrieval simply returns no candidates for them until a new ingestion or a backfill job runs (a backfill job is a reasonable Sub-Phase A/B follow-up but is not required for Definition of Done; flag as optional).
- `ContextPlanner::deterministic_phase_five_order()`'s existing unit test (`src/application/conversation.rs:1217-1224`, `context_planner_order_keeps_required_content_first`) continues to pass unchanged — the real planner must still expose (or be tested against) the same ordering guarantee, just now actually enforce it end-to-end rather than only returning the constant.

### Deployment implications

- Requires at least one configured, working embedding-capable provider/model per application that enables memory/RAG embeddings (`application_embedding_policies.embedding_provider_id`/`embedding_model_id`, migration 0007:101-102, already nullable/optional columns) — document that embedding-enabled applications need this configured before enabling `memory_embeddings_enabled`/`rag_embeddings_enabled`.
- No new external infra dependency (pgvector is already provisioned per migration 0007's HNSW indexes) — this plan is pure application code against existing infrastructure.
- Increased Postgres write volume (chunks, embeddings, summaries, extraction/retrieval runs, context plans) — flag for operators sizing storage/IOPS; no schema change needed to accommodate, but this plan's Verification should note observed row-growth rates from a representative test corpus.

### Worked example — a `/v1/responses` call that actually uses retrieval

Request: an application with `memory_retrieval_enabled=true` and `rag_retrieval_enabled=true` calls `POST /v1/responses` with `conversation.id` set to an existing conversation and a new user turn asking about a topic covered by a previously-ingested RAG document and a stored preference memory.

1. `ConversationService::prepare_response_conversation` (`src/application/conversation.rs:314-381`) persists the user turn as today, then (new in this plan) hands off to `ContextPlanner::plan(...)`.
2. `ContextPlanner` embeds the user turn's text via Sub-Phase B, calls `RetrievalService::retrieve_memories` and `retrieve_rag_chunks` (Sub-Phase C) scoped to the caller's `application_id`/`external_tenant_id`/`external_user_id`, receiving e.g. 1 memory candidate above `minimum_memory_score` and 2 RAG chunks above `minimum_chunk_score`.
3. `ContextPlanner` loads the last 8 recent messages (bounded by `maximum_recent_messages`) plus the latest `conversation_summaries` row if the conversation is long enough to have one, assembles the final message list in `deterministic_phase_five_order()`, and writes a `context_plans` row recording `included_memory_ids: [mem_id]`, `included_chunk_ids: [chunk_id_1, chunk_id_2]`, `included_summary_id`, `included_message_ids`, `truncation_reason: null`.
4. `src/application/execution.rs`'s existing completion path runs with the assembled context; the retrieved memory/chunk text is injected as clearly-delimited, non-instruction-role content per Sub-Phase D's safety requirement.
5. After completion, `public_response_from_record` (`src/application/public.rs:1614-1662`, Sub-Phase G) reads the `context_plans` row written in step 3, resolves `mem_id`/`chunk_id_1`/`chunk_id_2` to `PublicCitation` entries (`citation_type: "memory"` and `"rag_chunk"` respectively, with `title`/`section` populated from the source records), and returns them in `PublicResponse.citations` instead of the current unconditional `Vec::new()`.

A caller inspecting the response can now see *why* the model said what it said, and an operator with `moira:diagnostics:read` can pull the same `context_plans` row via the new diagnostic endpoint to debug a surprising or missing retrieval.

### Failure & recovery

- **Embedding provider call fails during ingestion**: `rag_document_versions.ingestion_status` moves to `'failed'` (already a valid check-constraint value, migration 0007:383) with the failure recorded in the corresponding `rag_ingestion_runs.failure_class`; the document remains queryable/re-ingestible (a new `ingest_rag_document` call creates a new version per the existing supersession logic at `src/infra/repositories/conversation.rs:1166-1176`).
- **Embedding provider call fails during memory extraction**: `memory_extraction_runs.status='failed'` with `failure_class`; no partial/inconsistent memory rows are left `active` — a candidate that fails validation or extraction is simply not inserted (extraction is all-or-nothing per candidate, not partially committed).
- **Retrieval query fails or times out**: per `application_embedding_policies.failure_behavior` (migration 0007:109, already defined, default `'continue_without_semantic_retrieval'`) — the context planner proceeds without memory/RAG content rather than failing the whole `/v1/responses` call; this must be the default and must be tested explicitly (a broken vector index must never take down the execution path).
- **Summarization singleflight lock holder crashes**: the `pg_try_advisory_xact_lock` is transaction-scoped (mirroring the proven idempotency pattern) so it releases automatically on connection/transaction termination — no stale-lock cleanup needed, matching the existing admin-idempotency design's correctness argument.
- **Context length still exceeded after dropping all optional content**: `context_length_exceeded` (422) is returned to the caller rather than silently truncating `current_input` or `protected_instructions` — this is a hard safety invariant, not a best-effort behavior.

---

## Detailed Implementation

### Sub-Phase A — Document chunking
- New `src/orchestration/chunking.rs`: `pub enum ChunkStrategy { Paragraph, Markdown, TokenWindow { window_tokens: usize, overlap_tokens: usize } }`, `pub fn chunk(content: &str, strategy: ChunkStrategy, max_chunk_chars: usize) -> Vec<ChunkCandidate>` where `ChunkCandidate { text: String, start_offset: usize, end_offset: usize, section_title: Option<String>, chunk_index: i32 }`. Must operate on `char`/grapheme boundaries, never split a UTF-8 multi-byte sequence (use `str::char_indices`, never raw byte slicing) — this is a correctness requirement, not a nice-to-have, given arbitrary user-supplied Markdown/text content.
- `chunk_hash` = deterministic hash (reuse `crate::security::request_hash` / `request_hash()` already used identically for `content_hash` elsewhere, e.g. `src/application/conversation.rs:286,352,395,455`) over the exact chunk text bytes, so identical content re-ingested produces identical `rag_chunks.chunk_hash` values (migration 0007:402, already a column) — enables future dedup/no-op-reingestion detection even though full dedup logic is not required for this plan's Definition of Done.
- Enforce `rag_chunks` count/size limits per document (new config, e.g. `Settings.rag.max_chunks_per_document`, `Settings.rag.max_chunk_chars`) — reject (`422 rag_document_too_large`) rather than silently truncating a document that would produce an unreasonable chunk count.
- `src/infra/repositories/conversation.rs`: extend `ingest_rag_document` (1138-1211) to, within the same transaction (`tx` already opened at 1144), insert chunk rows via a new `insert_rag_chunks(&mut tx, document_version_id, collection_id, chunks: &[ChunkCandidate]) -> Result<Vec<Uuid>, AppError>` batch insert.

### Sub-Phase B — Embeddings (Rig integration)
- New `src/orchestration/embedding.rs`: a thin wrapper analogous to how `src/orchestration/runtime_factory.rs` (referenced by `RuntimeModelHandle` in `controls.rs:20`) wraps completion — construct an embedding-capable Rig client from the resolved provider/model/credential (reuse the exact same credential-resolution path already used for completion, `resolver.rs:254-268` per the audit's "Credential precedence" positive finding), call Rig's embedding API in batches bounded by `application_embedding_policies.batch_size` (migration 0007:104, default 32), and return `Vec<EmbeddingResult { input_index: usize, vector: Vec<f32>, model_dimension: usize }>`.
- **Open technical item — THE plan's single largest technical unknown. Must be resolved by the Wave 0 / PR 11.0 spike before any Sub-Phase B implementation, and must not be guessed or fabricated.**
  **Re-verified during the CONVENTIONS re-audit and still fully open:**
  - `Cargo.toml:22` declares `rig-core = "0.40"` — a **caret range, not an exact pin**. `Cargo.lock` currently resolves it to **`rig-core 0.40.0`**. The spike must record the resolved version it verified against, because a `cargo update` inside the range can move the surface underneath this plan.
  - `docs/rig-integration.md` is **32 lines with a single `#` heading and zero occurrences of `embed`/`Embed`** — it documents only the completion/streaming surface. The claim "zero embedding usage documented" is **confirmed true**, not stale.
  - The provider list lives under the header "Supported in Phase 3:" at **`docs/rig-integration.md:16`**, with the bullets at **`:18-24`** (an earlier draft cited `16-24` for the bullets). The file also has "Configured but not executable: `custom`" at `:26-28` and "Partially supported" at `:30-32`.
  The spike must (a) confirm the exact Rig embedding trait/struct names and call shape (`rig_core::embeddings` / `EmbeddingsBuilder` / `EmbeddingModel` **or whatever they actually are** — these names are candidates to check, **not** an API this plan asserts exists) against the resolved `rig-core` version; (b) confirm which of Moira's supported provider clients (`openai`, `anthropic`, `gemini`, `deepseek`, `azure_openai`, `openai_compatible`/`local`, per `docs/rig-integration.md:18-24`) actually expose an embedding model in that version — historically only `openai`-family clients expose embeddings in comparable SDKs; **Anthropic/Gemini/DeepSeek embedding support is explicitly not assumed**; and (c) update `docs/rig-integration.md` with a new "Embeddings" section documenting exactly what is verified, mirroring the existing "Supported in Phase 3" / "Configured but not executable" / "Partially supported" structure.
  **If the spike finds that no supported provider exposes embeddings in the resolved Rig version, that is a legitimate outcome and must be reported as such** — it would make Sub-Phase B (and therefore C, D, G) blocked on an upstream capability, and the plan must be re-scoped rather than a second execution engine being introduced to work around it. Moira's boundary holds: **Rig owns AI execution and embedding primitives; Moira does not build its own.**
  PR 11.0 is docs-only and merges before any `src/` work in this plan begins.
- Batch cancellation: wrap each batch call in the same `execution_deadline`-bounded pattern plan 04 establishes for other unbounded-await phases (`src/application/execution.rs:132,327` pattern) — an embedding batch must respect a caller-visible deadline, not run unbounded.
- Model/dimension versioning: `memory_embeddings.embedding_version`/`rag_chunk_embeddings.embedding_version` (migration 0007:286,419, already `integer not null default 1`) increments whenever the configured `embedding_model_id` changes for an application; old-version rows get `superseded_at` set (already a column, migration 0007:290,423) rather than deleted, so retrieval queries always filter `where superseded_at is null` (already the pattern baked into the existing HNSW partial indexes, migration 0007:294-296,427-429 `where superseded_at is null and embedding is not null`).
- Dimension validation: reject (at `put_embedding_policy`, `src/application/conversation.rs:721-747`) any `embedding_dimension` that does not equal `1536` (the fixed column type) unless/until the Open Decision above chooses the multi-dimension-schema path.

### Sub-Phase C — Retrieval service
- New `src/orchestration/retrieval.rs`: `pub struct RetrievalService` with `retrieve_memories(...)` and `retrieve_rag_chunks(...)`, each accepting the acting application/tenant/user scope, the query embedding (from Sub-Phase B on the current user turn's text), and the effective `application_retrieval_policies` row (migration 0007:70-96, already defined — `semantic_weight`/`keyword_weight`/`recency_weight`/`importance_weight` for hybrid scoring, `minimum_memory_score`/`minimum_chunk_score` thresholds, `maximum_chunks_per_document` diversity control, `diversity_enabled`).
- SQL: vector similarity via `embedding <=> $query_vector` (cosine distance, matching `vector_cosine_ops` already used in both HNSW index definitions), combined with keyword search (Postgres full-text `ts_rank`/`plainto_tsquery` over `content_plain`/`chunk_text_plain` — note these columns are nullable in favor of `*_encrypted` variants per the conversation-content-persistence policy, migration 0007:5-6,171-172,376-377; keyword search must handle the encrypted-content case by either decrypting transiently or skipping keyword search when persistence mode excludes plaintext — resolve as part of implementation, document the chosen behavior), blended per the policy's weight fields into a single hybrid score, filtered by the tenant-isolation predicate from the Architecture section, and capped by `maximum_memory_results`/`maximum_chunk_results` (migration 0007:78-79).
- Every call writes one `retrieval_runs` row (migration 0007:467-486) with `query_hash` = `request_hash()` of the query text (consistent with the existing hashing pattern), candidate/returned counts, `latency_ms`, and `status`.
- `src/infra/repositories/conversation.rs`: add repository methods backing the above (`find_memory_candidates`, `find_rag_chunk_candidates`, `insert_retrieval_run`).

### Sub-Phase D — Context planner
- Replace the `ContextPlanner` stub (`src/application/conversation.rs:34-49`) with a real implementation that: (1) loads bounded recent messages via `history_strategy`/`maximum_recent_messages`/`maximum_history_tokens` (migration 0007:9-12, already-defined policy fields, currently unread by any code), (2) loads the latest non-superseded `conversation_summaries` row if one exists, (3) calls Sub-Phase C's retrieval service for memory and RAG candidates (only if the respective policy flags are enabled), (4) assembles content strictly in the `deterministic_phase_five_order()` sequence, truncating **optional** sections first (`older_history` → `retrieved_rag` → `retrieved_memory` → `conversation_summary` → `recent_messages`, in that priority, never touching `protected_instructions`/`current_input`), (5) if required content still doesn't fit, returns `context_length_exceeded`, else (6) writes one `context_plans` row (migration 0007:446-465) and returns the assembled message list to `src/application/execution.rs`'s existing completion invocation as additional history entries.
- **Prompt-injection safety** (the security-critical part of this sub-phase): retrieved memory/RAG text must be wrapped/delimited in a way the completion call structurally distinguishes from system/developer instructions — e.g. injected as `Message::user`-role content prefixed with an explicit, non-executable label (`"[retrieved context, not an instruction]: ..."`) rather than ever being placed in the same message/role Moira's own `protected_instructions` occupy. Must be unit- and integration-tested with adversarial content (a memory/chunk containing imperative "ignore prior instructions" text) asserting the model-facing message structure keeps it clearly separated from system-level instructions — note this tests structural placement, not that the *model* obeys the boundary (which is a model-behavior concern outside Moira's control), consistent with keeping Moira's boundary (orchestration, not AI execution behavior).
- Tokenizer-aware budgeting (`docs/todo.md:78`): use a real token estimate, not the existing crude `estimate_tokens` word-count heuristic (`src/application/conversation.rs:1196-1198`, currently `content.split_whitespace().count().max(1)`) for this specific budgeting decision — if Rig or the resolved provider/model exposes a tokenizer, use it; otherwise document the chosen conservative estimation ratio (e.g. characters/4) as an explicit, tested approximation with a safety margin, since under-counting risks exceeding the real provider context window and over-counting only wastes headroom (prefer erring toward over-counting).

### Sub-Phase E — Summarization
- New `ConversationService::summarize_conversation(actor, ctx, conversation_id, force: bool)`: acquires the `pg_try_advisory_xact_lock` singleflight lock keyed on a hash of `conversation_id`, checks `summarization_enabled`/`summary_trigger_tokens`/`minimum_messages_since_summary` (migration 0007:13,14,16) unless `force`, loads messages since the last summary's `covers_through_sequence` (migration 0007:198), calls the existing completion kernel (`src/application/execution.rs`) with a summarization prompt, and inserts a new `conversation_summaries` row with `summary_version` = previous max + 1, `covers_through_sequence` set to the latest included message's `sequence_number`, superseding the prior active row (`superseded_at = now()` on the old row, both writes in one transaction).
- New endpoint (`POST /v1/conversations/{id}/summarize`, see Interfaces) wired through `src/http/conversation.rs`.
- Policy-triggered auto-summarization hooks into `prepare_response_conversation` (`src/application/conversation.rs:314-381`) or `record_assistant_response` (`:383-421`) — after persisting the assistant turn, check the trigger threshold and enqueue (not synchronously run, to avoid adding summarization latency to the response path) a summarization job. Wherever the job actually executes (in-process supervisor today, plan 10's durable queue later) is an infrastructure concern this plan does not need to solve — the job **body** (the `summarize_conversation` method above) must simply be safely callable from either.

### Sub-Phase F — Automatic memory extraction
- New `ConversationService::extract_memories(actor, ctx, conversation_id, response_id)`, invoked from `record_assistant_response` (`:383-421`) when `automatic_extraction_enabled` and `consent_mode != Disabled`.
- Structured-output completion call (JSON-schema-constrained via whatever mechanism `src/application/execution.rs`'s existing completion path already supports for structured output — if none exists yet, this sub-phase must add minimal structured-output support to the completion call path, scoped narrowly to what extraction needs, not a general tool-calling framework, keeping the Rig-execution/tool-path (P3-6) explicitly out of scope).
- Validate each candidate: `memory_type` ∈ `allowed_memory_types` (migration 0007:42-45), `sensitivity` ∈ `allowed_sensitivity_levels` (migration 0007:46), `confidence >= minimum_extraction_confidence` (migration 0007:50-51) — reject candidates failing any check (recorded as `rejected_count` on the `memory_extraction_runs` row, migration 0007:307).
- Dedupe: exact match via `content_hash`; near-duplicate via embedding cosine similarity above a configurable threshold against existing active memories in the same scope — on near-duplicate, either skip or update `last_confirmed_at`/`use_count` on the existing record rather than inserting a duplicate.
- Contradiction: if a new candidate's content semantically conflicts with an existing active memory (initial implementation may use a simple heuristic — e.g. same `memory_key` with different `content_hash` — full semantic contradiction detection can be a documented follow-up refinement, not a Definition-of-Done blocker), set `contradicts_memory_id`/`resolution_status` (migration 0007:254-255) rather than silently overwriting.
- Consent enforcement: `consent_mode = 'explicit_only'` means extraction produces `status='candidate'` rows requiring separate user/caller confirmation (via the existing `patch_memory` endpoint, `src/application/conversation.rs:528-565`) before becoming `'active'`; `'automatic_with_user_controls'`/`'application_managed'` may insert directly as `'active'` per policy — this mapping must be explicit and tested per mode.

### Sub-Phase G — Citations
- `src/application/public.rs:1614-1662` `public_response_from_record` (and its caller) gains access to the `context_plans`/`retrieval_runs` rows written during the request's execution (threaded through from the `ContextPlanner` output already computed earlier in the same request, not re-queried) and maps included `memory_ids`/`chunk_ids` (migration 0007:456-457) to `PublicCitation` entries: `citation_type` = `"memory"` or `"rag_chunk"`, `document_id`/`memory_id` populated from the source record, `title`/`section` populated from `rag_documents.title`/`rag_chunks.section_title` where available. `PublicCitation` (`src/domain/conversation.rs:258-267`) requires no shape change.
- Per `docs/todo.md:76`, exact character/token spans are **not** fabricated — `PublicCitation` has no span fields today and this plan does not add them; citations reference the source chunk/memory by ID/title/section only, honestly reflecting what provenance is actually tracked.

### Sub-Phase H — Cross-cutting: idempotency & optimistic concurrency
- Audit every Phase-5 create/update/ingest endpoint (`create_conversation`, `patch_conversation`, `create_message`, `create_memory`, `patch_memory`, `create_rag_collection`, `patch_rag_collection`, `create_rag_document`, `ingest_rag_document`, plus the two new endpoints this plan adds) against plan 04's established `If-Match`/idempotency patterns (once plan 04 has landed — if this plan lands first, apply the same pattern plan 04 uses for other mutations, don't invent a third pattern). This sub-phase is bookkeeping/consistency, not new design.

### i18n catalog additions (CONVENTIONS.md §4 — binding)

Every user-visible response this plan produces must carry a stable `message_key` **and** a default English message. The derivation rule is `format!("moira.error.{}", code())` (`src/error.rs:146-148`, verified), so **the catalog key suffix must exactly equal the `code` string passed to `AppError::coded`**. Entries live in `src/i18n/catalog/errors.rs` (`RESPONSE_ERROR_CATALOG`) and `src/i18n/catalog/notices.rs` (`RESPONSE_NOTICE_CATALOG`), and must be mirrored into `docs/i18n-response-catalog.json` **in the same PR** (§4.4 — hand-synced today; drift is a review failure until plan 06 adds the drift test). `message_args` carries interpolation values as structured data — never pre-formatted English prose (§4.3).

**Verified state of the real catalog at audit time** (61 unique keys; Rust catalog and JSON mirror confirmed in sync). Several keys this plan needs **already exist and must be reused, not duplicated**:

| Key | Status | Sub-phase | Notes |
|---|---|---|---|
| `moira.error.rag_document_type_unsupported` | **Already exists** (`errors.rs:219-223`) | A | This *is* the "unsupported content type" key. Reuse verbatim — do not add a second variant. |
| `moira.error.rag_document_parse_failed` | **Already exists** (`errors.rs:214-218`) | A | Covers parse/decode failures during ingestion. Reuse for the parse case only; see `rag_ingestion_failed` below for the distinct embedding/pipeline failure case. |
| `moira.error.memory_consent_required` | **Already exists** (`errors.rs:159-163`) | F | Reuse for `consent_mode` enforcement. |
| `moira.error.memory_sensitivity_forbidden` | **Already exists** (`errors.rs:174-178`) | F | Reuse for `allowed_sensitivity_levels` violations. |
| `moira.error.memory_disabled` | **Already exists** (`errors.rs:164-168`) | F | Reuse when memory policy disables the operation. |
| `moira.error.structured_output_invalid` / `..._unsupported` | **Already exist** (`errors.rs:249-258`) | F | Reuse for the extraction call's structured-output failures — this is exactly the vocabulary Sub-Phase F needs; do not invent an extraction-specific variant. |
| `moira.error.conversation_archived` | **Already exists** (`errors.rs:84-88`) | E | Reuse for the summarize endpoint's `409`. |
| `moira.error.rag_collection_not_found` / `rag_document_not_found` / `memory_not_found` / `conversation_not_found` / `conversation_forbidden` | **Already exist** | A/C/E/F | Reuse throughout. |
| `moira.error.forbidden` | **Already exists** (`errors.rs:19-23`) | C | Reuse for `moira:diagnostics:read` scope denial — a new scope does **not** need a new error key. |
| **`moira.error.context_length_exceeded`** | **MISSING — add** | D | `default_message`: "The request context exceeds the available budget." `description`: "Used when required context cannot fit within the configured history token budget even after excluding all optional content." **Naming trap (verified):** the catalog already contains a *different, similarly-named* key `moira.error.context_required_content_too_large` (`errors.rs:79-83`, "Required content is too large to process."). These are **not** the same condition and must not be conflated — the existing key is about a single oversized required item; the new one is about the assembled budget. If Sub-Phase D concludes they are in fact the same condition, **reuse the existing key and add nothing** — but that must be an explicit, recorded decision, not an accident. |
| **`moira.error.retrieval_unavailable`** | **MISSING — add** | C/D | `default_message`: "Retrieval is required for this request but is currently unavailable." `description`: "Used when retrieval is configured as required and the retrieval or embedding backend cannot serve the query." **Only emitted when the application's `failure_behavior` is *not* the default `'continue_without_semantic_retrieval'`** (migration 0007:109) — under the default, retrieval failure degrades silently and returns `200`, so this key must never fire there. Both branches need a named test. |
| **`moira.error.embedding_dimension_mismatch`** | **MISSING — add** | B | `default_message`: "The configured embedding dimension does not match the supported dimension." `description`: "Used when a configured embedding model's dimension differs from the dimension the vector store supports." Emitted by `put_embedding_policy` (`src/application/conversation.rs:721-747`) rejecting an `embedding_dimension` ≠ 1536, and by the embedding client if a provider returns an unexpected vector length at runtime. Carry the expected and actual dimensions in `message_args` as **numbers**, not prose (§4.3). |
| **`moira.error.rag_document_too_large`** | **MISSING — add** | A | `default_message`: "The document is too large to ingest." `description`: "Used when a document exceeds the configured chunk-count or chunk-size limits." This is the code Sub-Phase A already names for its `422`; it does not exist yet. Carry limit/actual as structured `message_args`. |
| **`moira.error.rag_ingestion_failed`** | **MISSING — add** | A/B | `default_message`: "Document ingestion failed." `description`: "Used when chunking or embedding fails during document ingestion." Distinct from `rag_document_parse_failed` (which is parse-specific) — this covers the embedding-provider and pipeline failure paths that drive `ingestion_status='failed'`. |
| **`moira.error.embedding_provider_unavailable`** | **MISSING — add** | B | `default_message`: "The embedding provider is unavailable." `description`: "Used when the configured embedding provider cannot be reached or times out." Needed because the embedding path is a *new* provider-call surface; the existing `upstream_unavailable`/`upstream_timeout` keys describe completion upstreams and reusing them would make embedding failures indistinguishable from completion failures in client telemetry. If Sub-Phase B's spike concludes the existing upstream keys are sufficient, **reuse them and record that decision** rather than adding this key. |
| **`moira.notice.summarization_in_progress`** | **MISSING — add** | E | `default_message`: "A summarization is already in progress for this conversation." `description`: "Used for the 202 response when a summarization singleflight lock is already held." Required by §4.2: the summarize endpoint's `202 Accepted` must not inline an English literal. |

**Honest non-additions** (do not invent keys for these):
- The `200 OK` summarize response returns a `ConversationSummaryRecord` DTO with no human-readable prose field, so it needs **no** notice key. Only the `202` branch does.
- `context_plans` / `retrieval_runs` diagnostic responses are raw data DTOs behind `moira:diagnostics:read`; they carry no prose and need no notice key.
- Individual rejected extraction candidates are counted into `memory_extraction_runs.rejected_count` (migration 0007:307) — they are not user-visible responses and get no keys.

**Required i18n tests** (§4.5, exemplar `tests/http_error_contract.rs`): assert every key this plan adds resolves through `crate::i18n::catalog::is_known_key` / `default_message_for_key` (`src/i18n/catalog/mod.rs:30-38`, already implemented), and assert the corresponding HTTP responses carry a **non-empty `message_key` and non-empty `message`**. Named tests are in § Verification. **Each stacked PR adds the keys its own sub-phase emits, plus their tests** — no PR defers catalog work to a later stack member.

---

## Multi-Agent Workflow

Given the size (8 sub-phases, explicitly acknowledged as spanning multiple future iterations), this plan is executed as **its own sequence of internal waves**, not a single flat parallel burst — several sub-phases have hard data dependencies on earlier ones.

**Each wave below maps one-to-one onto a pull request in the stacked series defined in § Branch & Pull Request** (Wave 0 → PR 11.0, Wave 1 → PRs 11.A and 11.B, Wave 2 → 11.AB, Wave 3 → 11.C, Wave 4 → 11.D, Wave 5 → 11.E then 11.F, Wave 6 → 11.G, Wave 7 → 11.H). A wave is not complete when its agent stops editing — it is complete when **its PR merges with all CONVENTIONS.md §2 gates green and its own unit + e2e tests passing**. Do not start a dependent wave against unmerged work unless the coordinator explicitly accepts the rebase cost.

**Wave 0 — Technical spike (sequential, blocking, single agent).** Resolve the Rig embedding API open item (Sub-Phase B's "Open technical item") against the actually-pinned `rig-core` version, and confirm/refute the pgvector filtered-HNSW query-plan assumption (Architecture § cross-tenant isolation) with a real `EXPLAIN ANALYZE` against a seeded dataset. Both are prerequisites that would otherwise cause every downstream wave to guess wrong. Output: an update to `docs/rig-integration.md`'s new Embeddings section, and a short note confirming the retrieval index strategy.

**Wave 1 — Parallel, disjoint (2 agents), depends on Wave 0.**
1. **Chunking agent** — owns `src/orchestration/chunking.rs` (new file) plus its unit tests. No DB, no other module dependencies — fully independent.
2. **Embedding client agent** — owns `src/orchestration/embedding.rs` (new file), informed by Wave 0's spike output. Depends on Wave 0 only, not on the chunking agent.

**Wave 2 — Sequential, single owner, depends on Wave 1 (both).**
- **Ingestion pipeline agent** — owns `src/infra/repositories/conversation.rs`'s `ingest_rag_document` extension (chunks + embeddings + `rag_ingestion_runs`) and the corresponding `ConversationService::ingest_rag_document` wiring in `src/application/conversation.rs`. Must run after Wave 1 because it calls both new modules — this is the integration point, kept single-owner to avoid two agents editing the same repository file simultaneously.

**Wave 3 — Parallel, disjoint (2 agents), depends on Wave 2 (retrieval needs embedded content to test against; but the retrieval agent can write the SQL/service against Wave 0's plan-verified index strategy without waiting for a full ingestion pipeline — coordinator judgment call: if Wave 2 is slow, Wave 3's retrieval agent can start against seeded test fixtures directly, only truly blocking on Wave 2 for end-to-end integration tests, not for writing the service itself).**
1. **Retrieval service agent** — owns `src/orchestration/retrieval.rs` (new file) and its repository methods in `src/infra/repositories/conversation.rs` (coordinate with Wave 2's agent if still active — prefer sequencing Wave 2 fully before Wave 3 starts on the same repository file to avoid merge conflicts, unless the coordinator explicitly splits the repository file into logical sections each agent owns).
2. **Domain/citation-shape agent** — owns `src/domain/conversation.rs`/`src/domain/public.rs` additions needed for citation provenance detail and any new request/response DTOs (summarize endpoint, diagnostic endpoints). Fully disjoint from the retrieval agent's files.

**Wave 4 — Sequential, single owner, depends on Wave 3.**
- **Context planner agent** — owns the `ContextPlanner` replacement in `src/application/conversation.rs` and its integration into `src/application/execution.rs`'s prompt assembly. This is the highest-security-sensitivity file in the whole plan (prompt-injection safety boundary) — kept single-owner deliberately, with a **mandatory read-only security reviewer** pass before merge (see below).

**Wave 5 — Parallel, disjoint (2 agents), depends on Wave 4.**
1. **Summarization agent** — owns the new summarization methods/endpoint (Sub-Phase E): `src/application/conversation.rs` (new methods, coordinate section ownership with citation/context-planner edits already merged), `src/http/conversation.rs` (new route), `src/domain/conversation.rs` (new `ConversationSummaryRecord` DTO if not already added in Wave 3).
2. **Memory extraction agent** — owns Sub-Phase F: `src/application/conversation.rs` (new methods), hook into `record_assistant_response`. Disjoint endpoint/route surface from the summarization agent, but both touch `src/application/conversation.rs` — coordinator should sequence these two if the file-diff risk is judged too high, or have each agent work in a clearly separate method range and rely on Edit tool's exact-match requirement to catch true conflicts.

**Wave 6 — Sequential, single owner, depends on Wave 5.**
- **Citation-population agent** — owns Sub-Phase G: wiring `context_plans`/`retrieval_runs` provenance through to `src/application/public.rs:1614-1662`. Small, focused, last because it depends on everything upstream actually producing the provenance data.

**Wave 7 — Cross-cutting, single agent, can run any time after Wave 2 in parallel with later waves.**
- **Idempotency/If-Match audit agent** (Sub-Phase H) — read-only-until-fix: audits existing and newly-added Phase-5 endpoints against plan 04's patterns, files small targeted diffs. Explicitly does not touch the same files mid-edit as other active waves — best scheduled after Wave 6 to avoid churn, but is logically independent.

**Checkpoints.** Run `cargo fmt --check` + `cargo clippy --workspace --all-targets --all-features -- -D warnings` + `cargo test --workspace --all-features` after every wave. Given the data dependencies, do not start a wave until the prior blocking wave's checkpoint is green.

**Read-only reviewers.** Mandatory dedicated security-review pass (no edit tools) after Wave 4 specifically for the prompt-injection safety boundary, and again after Wave 6 for citation/provenance correctness (does a citation ever leak cross-tenant content, does it ever fabricate an unsupported span). A second read-only reviewer after Wave 2 checks cross-tenant vector isolation SQL specifically (the Architecture section's non-negotiable SQL-level filter requirement).

**Conflict avoidance.** `src/application/conversation.rs` is the highest-conflict-risk file (touched in Waves 2, 4, 5, 6) — the coordinator should either strictly sequence waves that touch it (safest, matches the plan above) or, if parallelizing Wave 5's two agents, have them coordinate on non-overlapping line ranges before starting and re-diff before merge.

---

## Interfaces & Contracts

- **`POST /v1/conversations/{id}/summarize`** — scopes: `moira:conversations:write` (reuses existing scope, no new scope needed since this is a conversation-owner action). Request body: `{"force": bool}` (optional, default `false`). Responses: `200 OK` with `ConversationSummaryRecord` body (new DTO: `id, conversation_id, summary_version, covers_through_sequence, summary_text (present only if persistence policy allows plaintext), token_count, created_at, superseded_at: null`); `202 Accepted` with `Retry-After` header if another summarization is already in flight for this conversation; `409 conversation_archived` if the conversation is archived (mirrors the existing check at `src/application/conversation.rs:343-349`); `422 context_length_exceeded` if even the minimal summarization input can't be assembled (rare, but must be a defined error path, not a panic).
- **`GET /v1/admin/conversations/{id}/context-plans/{execution_id}`** and **`GET /v1/admin/conversations/{id}/retrieval-runs`** — new scope `moira:diagnostics:read`, admin-only (system key / dev admin), never granted to consumer keys or ordinary trusted-JWT callers. `200 OK` with the raw `context_plans`/`retrieval_runs` row contents (new diagnostic DTOs) including truncation reasons and candidate/returned counts — this is the one place internal retrieval scoring/diagnostics are intentionally exposed, strictly gated.
- **Idempotency**: the new summarize endpoint and any Sub-Phase F/extraction-triggering surface follow whatever consistent pattern plan 04 establishes (Sub-Phase H) — `Idempotency-Key` header support only if plan 04's pattern is already implemented and proven; otherwise these new endpoints must **not** advertise `Idempotency-Key` in their OpenAPI spec until it's genuinely implemented, repeating P0-2's mistake being explicitly the thing to avoid.
- **Transaction boundaries**: chunk+embedding insertion is one transaction per document version (Sub-Phase A/B, matching the existing `ingest_rag_document` transaction shape at `src/infra/repositories/conversation.rs:1144-1209`); summarization's supersede-old/insert-new is one transaction; memory extraction's per-candidate validate/dedupe/insert is one transaction per candidate (not one giant transaction per extraction run, to avoid a single bad candidate rolling back an otherwise-valid batch — the `memory_extraction_runs` row itself tracks aggregate `accepted_count`/`rejected_count` across the run outside any single candidate's transaction).
- **Cache invalidation**: none of this plan's writes affect `RuntimeConfigCache` (that cache is for provider/model/routing/policy config, not conversation/memory/RAG content) — no `invalidate_all()` calls needed for this plan's new write paths, only for the existing policy PUT endpoints already covered.
- **Concurrency behavior**: summarization singleflight via `pg_try_advisory_xact_lock` keyed per-conversation (non-blocking `pg_try_*`, matching the existing idempotency pattern's non-blocking-then-retry shape at `src/infra/repositories/admin.rs:567-581`, adapted to return `202`+`Retry-After` instead of retry-looping, since summarization is not a client-retried idempotent write in the same sense).
- **SSE**: retrieval/context-planning happens before the SSE stream starts (as part of the existing `prepare_execution`/`prepare_response_conversation` pre-stream phase, `src/application/public.rs:125-147`) — no new SSE event types are required for this plan's core scope, though a future refinement could emit a `response.context.planned` diagnostic SSE event; not required for Definition of Done.
- **Citation/provenance shape**: `PublicCitation { id, citation_type: "memory"|"rag_chunk", document_id: Option<String>, memory_id: Option<String>, title: Option<String>, section: Option<String> }` — unchanged schema (`src/domain/conversation.rs:258-267`), populated for real. No span/offset fields are added (per Sub-Phase G's explicit no-fabrication rule) — flag as an open product question below if exact-span citations become a future requirement.
- **`context_length_exceeded` behavior**: `422` with `error.code = "context_length_exceeded"` and therefore — per the **verified** derivation rule `format!("moira.error.{}", code())` (`src/error.rs:146-148`) — `message_key = "moira.error.context_length_exceeded"`, **not** the bare `"context_length_exceeded"` an earlier draft of this plan stated. The `moira.error.` prefix is not optional; it is what the catalog is keyed on. The response includes, in the existing envelope's `details` field (`ErrorDetail` at `src/error.rs:57-65`), which required section could not fit (e.g. `"reason": "current_input_exceeds_budget"` vs `"reason": "no_summary_or_history_capacity"`) so callers/operators can diagnose without needing diagnostic-scope access. Any numeric budget values go in `message_args` as numbers, never as pre-formatted English (§4.3). See § i18n catalog additions for the naming trap against the pre-existing `moira.error.context_required_content_too_large`.

---

## Verification

**Binding rule (CONVENTIONS.md §3): both a unit layer and an e2e layer are mandatory. A plan with only one layer is incomplete and must not be merged — and this applies to every PR in the stack, not only the last one.** "E2E" means the behavior is exercised through its real external surface (HTTP) against a **real PostgreSQL 16 + pgvector**, following the existing harness in `tests/support/mod.rs`. Exemplars to imitate: `tests/admin_idempotency.rs`, `tests/execution_lifecycle.rs`, `tests/public_authorization.rs`, `tests/http_error_contract.rs`.

**Environment facts (verified):** CI runs `pgvector/pgvector:pg16` (`.github/workflows/ci.yml:13-25`) and `migrations/0001_extensions.sql:2` does `create extension if not exists vector`, so pgvector is genuinely available to tests. **No conversation/memory/RAG test file exists today** — `tests/` contains only `admin_idempotency.rs`, `execution_lifecycle.rs`, `http_error_contract.rs`, `public_authorization.rs`, `security_foundation.rs`, plus `support/`. Every e2e file below is net-new, and `tests/support/mod.rs` (496 lines) needs new fixture helpers (seeded collections/documents/memories, a deterministic stub embedding provider) before the first e2e can be written.

**Fail-closed rule:** DB-dependent tests must `panic!` when **`CI=true`** and `MOIRA_TEST_DATABASE_URL` is absent (value check per `CONVENTIONS.md` §3 — never `var_os("CI").is_some()`) — the existing verified pattern at `tests/support/mod.rs:430-441`. Never silently skip in CI.

**Deterministic embeddings:** e2e tests must not call a real embedding provider. Use a stub embedding model that maps fixture text to **fixed, hand-chosen 1536-dimension vectors** so cosine distances are exactly predictable — this is what makes the "other tenant scores higher" isolation assertion provable rather than probabilistic. The stub belongs beside the existing `tests/support/mock_openai.rs`.

**Concurrency rule (CONVENTIONS.md §3, finding P2-12): interleaving tests use acknowledgement gates (`Barrier`, `oneshot`, `Notify`), never `sleep()`.** This applies directly to the summarization-singleflight test.

### Unit layer (colocated `#[cfg(test)] mod tests`, no database)

- **Chunking** — `src/orchestration/chunking.rs`:
  - `paragraph_strategy_splits_on_blank_lines`
  - `markdown_strategy_respects_section_headers`
  - `markdown_strategy_populates_section_title_from_nearest_heading`
  - `token_window_strategy_overlap_is_deterministic`
  - `token_window_strategy_overlap_never_exceeds_window`
  - `chunking_never_splits_a_utf8_multibyte_sequence` — feed multi-byte content (CJK, emoji, combining marks) at every offset near a boundary; assert every emitted chunk is valid UTF-8 and the concatenation round-trips. This is the correctness requirement, not a nice-to-have.
  - `chunking_preserves_grapheme_clusters_across_boundaries`
  - `chunk_offsets_are_byte_exact_and_non_overlapping_for_paragraph_strategy`
  - `chunk_hash_stable_across_repeated_runs` — identical input → identical `chunk_hash` (migration 0007:402), using the verified `crate::security::request_hash` (`src/security/masking.rs:10`, re-exported at `src/security/mod.rs:14`).
  - `chunk_hash_differs_for_differing_content`
  - `chunking_rejects_document_exceeding_max_chunk_count` — asserts the `rag_document_too_large` code.
  - `chunking_rejects_chunk_exceeding_max_chunk_chars`
  - `chunking_empty_and_whitespace_only_input_produces_no_chunks_not_a_panic`
- **Context-budget arithmetic** — `src/application/conversation.rs` (extend the existing `mod tests`, which already holds the verified `context_planner_order_keeps_required_content_first` at `:1217-1224`):
  - `budget_drops_optional_sections_in_documented_priority_order` — `older_history` → `retrieved_rag` → `retrieved_memory` → `conversation_summary` → `recent_messages`, in that exact order.
  - `budget_never_drops_protected_instructions_or_current_input`
  - `budget_returns_context_length_exceeded_when_required_content_alone_overflows`
  - `budget_accounts_for_every_included_section_exactly_once` (no double-counting, no omission)
  - `token_estimate_errs_toward_over_counting` — pins the Sub-Phase D safety-margin decision; under-counting risks a real provider rejection, over-counting only wastes headroom.
  - `token_estimate_is_monotonic_in_input_length`
  - `budget_arithmetic_is_stable_at_exact_boundary` (content sized to exactly `maximum_history_tokens` is included, one token more is not)
- **Retrieval scoring / threshold logic** — `src/orchestration/retrieval.rs` (pure scoring functions, factored out of the SQL):
  - `hybrid_score_blends_weights_per_policy` (against the verified `semantic_weight`/`keyword_weight`/`recency_weight`/`importance_weight` fields, migration 0007:70-96)
  - `hybrid_score_with_zero_keyword_weight_equals_pure_semantic_score`
  - `candidates_below_minimum_memory_score_are_excluded`
  - `candidates_below_minimum_chunk_score_are_excluded`
  - `threshold_is_exclusive_at_exactly_the_minimum` (pin the boundary either way — do not leave `>` vs `>=` unspecified)
  - `results_are_capped_at_maximum_memory_results_and_maximum_chunk_results` (migration 0007:78-79)
  - `diversity_cap_limits_chunks_per_document` (`maximum_chunks_per_document`)
  - `cosine_distance_to_score_conversion_is_monotonic_decreasing`
- **Citation / provenance mapping** — `src/application/public.rs` or `src/domain/conversation.rs` (pure mapping, no DB):
  - `context_plan_memory_ids_map_to_memory_citations` (`citation_type == "memory"`)
  - `context_plan_chunk_ids_map_to_rag_chunk_citations` (`citation_type == "rag_chunk"`)
  - `citation_mapping_preserves_order_and_deduplicates_repeated_ids`
  - `citation_mapping_omits_span_fields_because_public_citation_has_none` — pins the Sub-Phase G no-fabrication rule against the verified `PublicCitation` shape (`src/domain/conversation.rs:258-267`: `id`, `citation_type` (serialized as `type`), `document_id`, `memory_id`, `title`, `section` — **no span/offset fields**).
  - `empty_context_plan_maps_to_empty_citations_not_null`
- **Embedding-dimension validation** — `src/orchestration/embedding.rs`:
  - `dimension_mismatch_is_rejected_with_embedding_dimension_mismatch`
  - `batch_splitting_respects_configured_batch_size` (migration 0007:104, default 32)
  - `batch_splitting_preserves_input_index_mapping` (a reordered provider response must not silently misalign vectors to chunks — this would be a *silent* correctness bug, so it needs its own test)

### E2E layer (`tests/`, real PostgreSQL 16 + pgvector, driven through HTTP)

- **`tests/rag_ingestion_pipeline.rs`** (new, PR 11.AB) — ingestion → chunks → embeddings, end to end:
  - `direct_text_ingestion_produces_chunks_and_embeddings` — asserts real `rag_chunks` **and** `rag_chunk_embeddings` rows exist by querying the DB, not by trusting the HTTP body. This is the direct e2e refutation of P0-1.
  - `ingestion_status_progresses_pending_chunking_embedding_indexed` — proves the verified hardcoded `'indexed'` literal at `src/infra/repositories/conversation.rs:1184` is gone.
  - `ingestion_failure_leaves_status_failed_not_indexed`
  - `ingestion_failure_records_failure_class_on_rag_ingestion_runs`
  - `reingesting_supersedes_prior_version_chunks`
  - `oversized_document_returns_rag_document_too_large_with_message_key`
  - `unsupported_content_type_returns_rag_document_type_unsupported_with_message_key`
- **`tests/rag_retrieval_end_to_end.rs`** (new, PR 11.C/11.G) — the full chain the plan exists to deliver:
  - `ingest_then_respond_populates_citations_from_real_provenance` — the headline e2e: ingest a document, issue `POST /v1/responses` against a conversation with retrieval enabled, assert `citations` is **non-empty** and its IDs match the `context_plans` row's `included_chunk_ids`.
  - `retrieved_chunks_appear_in_the_context_plan_row`
  - `retrieval_returns_no_candidates_for_content_ingested_before_the_pipeline_existed` (backward-compatibility claim)
  - `retrieval_below_threshold_yields_empty_citations_not_missing_field`
- **`tests/retrieval_cross_tenant_isolation.rs`** (new, PR 11.C) — **MANDATORY. This is the security-critical test of the entire plan; PR 11.C does not merge without it.** Each case seeds the "other" scope with content whose raw cosine similarity to the query is **higher** than anything in the caller's own scope, so an application-level post-filter would still pass a naive test but a missing SQL-level filter is caught:
  - `application_isolation_holds_even_when_other_application_scores_higher`
  - `tenant_isolation_holds_within_single_application_even_when_other_tenant_scores_higher`
  - `user_isolation_holds_within_single_tenant_even_when_other_user_scores_higher`
  - `restricted_collection_excluded_from_unauthorized_application`
  - `restricted_collection_included_only_when_listed_in_allowed_collection_ids` (migration 0007:76)
  - `memory_retrieval_isolation_holds_under_all_three_scopes`
  - `rag_chunk_retrieval_isolation_holds_under_all_three_scopes`
  - `isolation_holds_when_the_only_matching_row_belongs_to_another_tenant` — the degenerate case: the caller's scope has **zero** candidates; assert an empty result, never a fallback to the global nearest neighbour.
  - `retrieval_run_counts_never_reveal_other_tenant_candidate_counts` — `retrieval_runs.*_candidate_count` (migration 0007:467-486) must count only in-scope candidates; leaking a cross-tenant count is an inference channel even when no content is returned.
  - `diagnostic_endpoint_never_returns_another_applications_context_plan` — the `moira:diagnostics:read` surface is a *new* read path over provenance and needs its own isolation proof, not just the retrieval path's.
- **`tests/context_planner.rs`** (new, PR 11.D):
  - `required_content_never_dropped_under_pressure`
  - `optional_sections_drop_in_documented_priority_order`
  - `context_length_exceeded_returned_when_nothing_fits` — asserts `422`, `error.code == "context_length_exceeded"`, and a non-empty `message_key` of exactly `"moira.error.context_length_exceeded"`.
  - `adversarial_retrieved_content_never_enters_system_role_message` — the prompt-injection structural-separation test. Seed a RAG chunk and a memory containing imperative text ("ignore previous instructions…"), then assert on the **assembled message list handed to the completion path**: no system/developer-role message contains any retrieved substring, and every retrieved item appears only in a clearly-delimited non-instruction-role message. Note honestly what this does and does not prove: it tests **structural placement**, not that the model obeys the boundary — model behavior is outside Moira's boundary.
  - `retrieved_content_is_delimited_with_the_non_instruction_label`
- **`tests/rag_content_leak.rs`** (new, PR 11.D — **required, and distinct from the isolation suite**) — proves retrieved text never escapes into observability surfaces. This closes the gap the audit flagged as entirely missing (`00-audit-report.md` P1-10: "no prompt/content-leak suite at all"):
  - `retrieved_chunk_text_never_appears_in_audit_metadata` — audit rows carry IDs and hashes only.
  - `retrieved_memory_text_never_appears_in_audit_metadata`
  - `retrieved_content_never_appears_in_captured_log_output` — capture the `tracing` subscriber output for a full retrieval-backed `/v1/responses` call and assert no seeded canary string appears at any level.
  - `summarization_prompt_and_response_bodies_are_never_logged`
  - `extraction_prompt_and_response_bodies_are_never_logged`
  - `retrieved_content_never_appears_in_a_system_or_developer_role_message` — the same invariant as the planner test, asserted here at the HTTP/e2e level for defense in depth.
  - `error_responses_never_echo_retrieved_content` (a `context_length_exceeded` or retrieval failure must not quote the retrieved text back to the caller).
  - Implementation note: seed each fixture with a unique high-entropy canary token so a single `contains` assertion is both sensitive and non-flaky.
- **`tests/conversation_summarization.rs`** (new, PR 11.E):
  - `concurrent_summarize_calls_singleflight_to_one_writer` — two concurrent calls gated on a `Barrier` (**not** `sleep`); assert exactly one `200` and one `202`.
  - `in_flight_summarize_returns_202_with_retry_after_and_notice_key` — asserts the new `moira.notice.summarization_in_progress` key and a non-empty message.
  - `summary_versions_are_immutable_and_monotonic`
  - `superseded_summary_retains_its_row_and_gains_superseded_at`
  - `policy_triggered_summarization_respects_trigger_tokens`
  - `summarize_on_archived_conversation_returns_conversation_archived`
- **`tests/memory_extraction.rs`** (new, PR 11.F):
  - `extraction_rejects_below_confidence_threshold`
  - `extraction_rejects_disallowed_memory_type`
  - `extraction_rejects_disallowed_sensitivity_level`
  - `extraction_dedupes_exact_content_hash`
  - `extraction_dedupes_near_duplicate_embedding`
  - `extraction_respects_consent_mode_candidate_vs_active` — one case per `consent_mode` value, per the Sub-Phase F requirement that the mapping be explicit and tested per mode.
  - `extraction_marks_contradiction_instead_of_overwriting`
  - `rejected_candidates_are_counted_on_memory_extraction_runs`
  - `extraction_never_writes_memories_outside_the_acting_application_scope`
- **`tests/embedding_provider_chaos.rs`** (new, PR 11.B/11.C):
  - `ingestion_marks_failed_on_embedding_timeout`
  - `ingestion_never_leaves_a_phantom_indexed_status_after_failure`
  - `retrieval_continues_without_semantic_results_on_embedding_failure` — the **default** `failure_behavior='continue_without_semantic_retrieval'` branch (migration 0007:109): assert `200` with empty citations, never a failed execution. "A broken vector index must never take down the execution path."
  - `retrieval_returns_retrieval_unavailable_when_failure_behavior_is_strict` — the non-default branch; asserts the new key. Both branches must be pinned.
  - `embedding_batch_respects_the_execution_deadline`
  - `embedding_provider_unreachable_surfaces_embedding_provider_unavailable`
- **`tests/http_error_contract.rs`** (extend the existing exemplar) — §4.5 i18n presence assertions:
  - `new_rag_memory_error_keys_exist_in_catalog` — `is_known_key` (`src/i18n/catalog/mod.rs:30-32`) for `moira.error.context_length_exceeded`, `moira.error.retrieval_unavailable`, `moira.error.embedding_dimension_mismatch`, `moira.error.rag_document_too_large`, `moira.error.rag_ingestion_failed`, `moira.error.embedding_provider_unavailable`, and `moira.notice.summarization_in_progress`.
  - `reused_rag_memory_keys_still_resolve` — `moira.error.rag_document_type_unsupported`, `rag_document_parse_failed`, `memory_consent_required`, `memory_sensitivity_forbidden`, `structured_output_invalid`, `conversation_archived`.
  - `i18n_json_mirror_matches_rust_catalog_for_new_keys` — manual-sync guard per §4.4 until plan 06's drift test lands.

### Other required verification

- **Concurrency/cancellation**: embedding batch calls respect the execution-deadline bound (Sub-Phase B); a cancelled/interrupted ingestion run leaves `rag_document_versions.ingestion_status='failed'` (or a consistent in-progress state, never a phantom `'indexed'`) and `rag_ingestion_runs` reflects the true outcome.
- **Migration**: no new tables required per current design (Architecture § DB/migration changes) — if the embedding-dimension Open Decision produces a migration, it must clean-apply against the full existing chain.
- **OpenAPI validation**: new endpoints (`summarize`, diagnostic context-plan/retrieval-run routes) pass the existing route-coverage tests (`src/http/mod.rs:23-165` pattern) and the CI OpenAPI-drift gate (plan 05, assumed landed by this point per the roadmap's `I02 --> I11` dependency — if plan 05 hasn't landed, this plan's new routes must still be manually verified against the committed spec).
- **`tests/response_citations.rs`** (new, PR 11.G) — the citation surface gets its own e2e file in addition to the round-trip cases above: `citations_populated_from_context_plan_provenance`, `citations_never_include_cross_tenant_source`, `citations_omit_span_fields_when_unsupported`, `citations_resolve_title_and_section_from_source_records`.
- **Chaos**: Redis/multi-replica chaos is **not** this plan's scope (plan 10). This plan's equivalent is **embedding-provider-failure** chaos — see `tests/embedding_provider_chaos.rs` above.
- **Benchmark/query-plan docs for pgvector HNSW**: required deliverable, not optional — `EXPLAIN ANALYZE` output for the actual retrieval queries (Sub-Phase C) at a realistic seeded row count (e.g. 100k memory embeddings, 500k RAG chunk embeddings across multiple applications) with the actual configured dimension (1536) and representative application/tenant/user filters, documented in a new `docs/pgvector-benchmarks.md` (or appended to an existing ops doc) confirming the HNSW index is actually selected by the planner under the combined filter+similarity query shape used in production, not silently falling back to a sequential scan. Landed by **PR 11.0** (initial finding) and updated by **PR 11.C** (shipped query shape).
- **Required Rust gates (CONVENTIONS.md §2, verbatim — run at every wave checkpoint and before *each* stacked PR opens):**
  - `cargo fmt --check`
  - `cargo clippy --workspace --all-targets --all-features -- -D warnings`
  - `cargo test --workspace --all-features`
  - `cargo build --release --locked`
  - clean PostgreSQL migration validation (migrations apply from an empty database)

---

## Definition of Done

- A RAG document ingested via `POST /v1/rag/collections/{id}/documents/{doc_id}/ingest` with direct text content produces real `rag_chunks` and `rag_chunk_embeddings` rows (verified by querying the DB after ingestion, not only by inspecting the HTTP response), and `ingestion_status` genuinely reflects pipeline progress, never prematurely `'indexed'`.
- A `/v1/responses` call against a conversation with `memory_retrieval_enabled`/`rag_retrieval_enabled` on an application with seeded, relevant memories/documents demonstrably includes that content in the assembled prompt (verified via a `context_plans` row inspection, not only by eyeballing model output) and the response's `citations` array is non-empty and points at the correct source IDs.
- A conversation exceeding `summary_trigger_tokens` gets an automatic, versioned summary; two concurrent manual summarize calls for the same conversation never produce two `summary_version`s covering overlapping sequence ranges (singleflight proven under real concurrency, not just code-reading).
- Automatic memory extraction on a response with `automatic_extraction_enabled` produces validated, policy-compliant `memory_records` rows honoring `consent_mode`, and a duplicate/near-duplicate extraction does not create a second row.
- Cross-tenant vector isolation integration tests (Verification section) pass, including the "other tenant scores higher by raw similarity" adversarial case.
- `context_length_exceeded` is returned (never a silent truncation of required content or an unhandled panic) when content genuinely cannot fit.
- `docs/rig-integration.md` has a real, verified "Embeddings" section (not a TODO) documenting the exact Rig embedding API and provider coverage actually implemented.
- `docs/pgvector-benchmarks.md` (or equivalent) documents a real `EXPLAIN ANALYZE`-verified query plan for the shipped retrieval queries at realistic scale.
- All Verification-section gates pass, including cross-tenant isolation, prompt-injection structural-separation, and embedding-provider-failure chaos tests.
- `docs/todo.md` Phase 5 items this plan covers are marked done or precisely rewritten to reflect what shipped vs. what remains excluded (remote-URL ingestion, export/deletion propagation).

### CONVENTIONS.md §8 compliance checklist

**This checklist applies TWICE: once per stacked sub-phase PR (scoped to what that PR delivers), and once to the final integration PR (scoped to the whole plan).** A sub-phase PR that ticks fewer boxes because "a later PR covers it" is not mergeable.

- [ ] Work performed on the plan's own branch **`plan/11-rag-memory-intelligence`** (or one of its stacked sub-phase branches per § Branch & Pull Request); PR opened with all seven required description sections, **and every stacked PR names its base PR** (§1.1).
- [ ] All gates in CONVENTIONS.md §2 pass **independently for this PR**: `cargo fmt --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace --all-features`, `cargo build --release --locked`, plus clean migration application from an empty database. (Frontend gates: **N/A** — no console code.)
- [ ] **Unit tests** delivered and passing for this PR's scope — chunking (paragraph/Markdown/token-window, UTF-8 boundary safety, deterministic chunk hashes), context-budget arithmetic, retrieval scoring/threshold logic, and citation/provenance mapping, all named in § Verification.
- [ ] **E2E tests** delivered and passing against a **real PostgreSQL 16 + pgvector**, driven through HTTP: ingestion→chunks→embeddings→retrieval→citations end to end. (Playwright: **N/A** — no console surface.)
- [ ] **Cross-tenant / application / tenant / user vector-isolation e2e tests pass**, including the adversarial "the other scope scores higher by raw cosine similarity" case and the zero-in-scope-candidates degenerate case. **This is the security-critical gate of the whole plan — PR 11.C does not merge without `tests/retrieval_cross_tenant_isolation.rs` green.**
- [ ] **Prompt/content-leak e2e tests pass** (`tests/rag_content_leak.rs`): retrieved memory/RAG text never appears in logs or audit metadata, and is never injected into a system/developer-role message. Canary-token based, per § Verification.
- [ ] Isolation is enforced **in SQL, in the same query as the similarity ORDER BY** — not as an application-layer post-filter over unscoped top-K results. Verified by the security reviewer reading the shipped SQL, in addition to the tests.
- [ ] **No new `sleep()`-based interleaving** in any concurrency test; acknowledgement gates used in the summarization-singleflight test (finding P2-12).
- [ ] DB-dependent tests **fail closed in CI** — `panic!` when **`CI=true`** and `MOIRA_TEST_DATABASE_URL` is absent (value check per `CONVENTIONS.md` §3 — never `var_os("CI").is_some()`), matching the verified pattern at `tests/support/mod.rs:430-441`.
- [ ] Every new error/notice string has an i18n **key + English `default_message` + `description`** in the Rust catalog, **mirrored into `docs/i18n-response-catalog.json` in the same PR**, with a test asserting presence and non-empty `message_key` + `message` on responses: `moira.error.context_length_exceeded`, `retrieval_unavailable`, `embedding_dimension_mismatch`, `rag_document_too_large`, `rag_ingestion_failed`, `embedding_provider_unavailable`, `moira.notice.summarization_in_progress`. Verified already-existing keys are **reused, not duplicated**: `rag_document_type_unsupported`, `rag_document_parse_failed`, `memory_consent_required`, `memory_sensitivity_forbidden`, `memory_disabled`, `structured_output_invalid`/`_unsupported`, `conversation_archived`, `forbidden`, and the `*_not_found` family.
- [ ] Every new error `code` string exactly matches its catalog key suffix, per the verified derivation rule `format!("moira.error.{}", code())` (`src/error.rs:146-148`) — in particular `message_key` is `"moira.error.context_length_exceeded"`, **not** the bare `"context_length_exceeded"`.
- [ ] **OpenAPI regenerated and committed in the same PR** as any route/DTO/status-code change, with the `moira-openapi` skill invoked per the repo's `CLAUDE.md`; the plan-05 drift gate passes (§1.6).
- [ ] Frontend toolchain / Atomic Design items (§8 bullet 6): **N/A** — no console code.
- [ ] Auth-touching items (§8 bullet 7): **N/A for auth configuration**, but the new `moira:diagnostics:read` scope is an **authorization** change — it must be admin-only, never granted to consumer keys or ordinary trusted-JWT callers, and must be **deny-by-default**, proven by a named negative test.
- [ ] **No secret-leak: verified by test** — and in this plan, extended to **no content-leak**: neither secrets nor retrieved user content reach logs, audit metadata, error bodies, or another tenant's diagnostic query.
- [ ] Every finding claimed closed (`P0-1` root cause, `P3-8`, `P2-7`) is backed by a **named, passing test** — "implemented" is not "done" (§3). Specifically, P0-1 is closed only by `direct_text_ingestion_produces_chunks_and_embeddings` plus `ingest_then_respond_populates_citations_from_real_provenance`, not by code inspection.
- [ ] **Every PR in the stack merged** with all gates green, ending with the **11.final** integration PR (§1.5) — neither a sub-phase nor the plan is done at PR-open.

---

## Risks & Rollback

- **Security**: prompt injection via retrieved content is the single highest-severity risk in this plan — mitigated by the structural message-role separation in Sub-Phase D, but this is a defense-in-depth measure, not a guarantee the underlying model resists instruction-like retrieved text; document this residual risk explicitly in operator-facing docs (retrieval-augmented content should still be treated as untrusted by application-layer prompt design, Moira only guarantees it is not placed in Moira's own system/developer instruction slot). Cross-tenant vector leakage is the second highest — mitigated by SQL-level filtering proven in tests, not application-level-only filtering.
- **Data-migration**: minimal risk — this plan is additive-only against an already-existing, already-migrated schema (migration 0007); the only conditional migration (embedding-dimension handling) is narrow and reversible.
- **Compatibility**: existing conversations/memories/documents without chunks/embeddings/summaries continue to function (retrieval simply returns no candidates) — no breaking change for pre-existing data; a backfill job is optional, not required.
- **Deployment**: requires an embedding-capable provider configured per application wanting semantic features — document as a prerequisite; applications that leave `memory_embeddings_enabled`/`rag_embeddings_enabled` off are entirely unaffected by this plan (`failure_behavior` default ensures execution never breaks even if embedding is misconfigured).
- **Rollback procedure**: because this plan's endpoints already exist today (as honest no-op-ish primitives per plan 02) and this plan only fills in behavior behind them, a rollback is a code revert to the plan-02 state — no schema rollback needed (new rows in already-existing tables simply stop being written; existing rows are harmless if the code reverts). If a specific sub-phase proves problematic in production (e.g. memory extraction producing low-quality memories), it can be disabled per-application via `application_memory_policies.automatic_extraction_enabled=false` without any code change — policy flags are the primary operational kill-switch for every sub-phase (`conversations_enabled`, `summarization_enabled`, `memory_enabled`/`automatic_extraction_enabled`/`automatic_retrieval_enabled`, `rag_enabled`, all already-defined migration 0007 columns).
- **Deferred follow-ups**: remote-URL RAG ingestion with SSRF hardening (`docs/todo.md:73`); conversation export/deletion-propagation for derived artifacts (`docs/todo.md:79`); exact-span citations (would require a schema addition, currently out of scope); backfill job for pre-existing content; refined semantic contradiction detection beyond the initial heuristic (Sub-Phase F); full structured-output/tool-calling framework generalization beyond what extraction narrowly needs (kept explicitly separate from the still-out-of-scope Rig Agent/tool path, P3-6).

---

## Rollout Sequencing Relative to Plan 02's Honest-API Boundary

Plan 02 made `ingestion_status`, `citations`, and the conversation/memory/RAG endpoints truthful by documenting them as persistence/configuration primitives (`00-audit-report.md` P0-1 correction path (a)). This plan does not need to, and must not, "undo" that documentation as a first step — the correct rollout order is:

1. Land each sub-phase (A through H) behind the existing policy flags, which already default to `false`/disabled (`application_conversation_policies.summarization_enabled`, `application_memory_policies.enabled`/`automatic_extraction_enabled`, `application_retrieval_policies.enabled`, `application_embedding_policies.memory_embeddings_enabled`/`rag_embeddings_enabled` — all already-defined migration 0007 columns defaulting to `false`). No existing application's behavior changes until an operator explicitly opts an application in.
2. Validate each sub-phase against a pilot application with the relevant policy flags enabled, using the Verification suite's cross-tenant isolation and prompt-injection tests as the go/no-go gate — these are not optional smoke tests, they are the primary risk this plan introduces.
3. Only once a sub-phase's behavior is proven does the OpenAPI/docs description move from plan 02's "persistence primitive, retrieval not yet wired" language to describing the real capability (per `docs/todo.md:77`, which this plan is what finally allows removing) — this documentation update should happen sub-phase by sub-phase, not as one big-bang rewrite at the end, since ingestion/chunking (Sub-Phase A/B) can honestly graduate before summarization (Sub-Phase E) or extraction (Sub-Phase F) do if they land on different timelines.
4. The `moira-openapi` skill must be invoked for every endpoint/DTO change in this plan, per the project's own `CLAUDE.md` instruction — this is not optional tooling, it is a repository-mandated gate for any HTTP route or DTO change.

---

## Open Product & Technical Decisions

1. **Rig embedding API surface** (Sub-Phase B) — must be confirmed against the resolved `rig-core = "0.40"` (`Cargo.toml:22`; exact patch version from `Cargo.lock`) before implementation; this plan's Wave 0 is a hard blocking spike specifically because `docs/rig-integration.md` currently documents zero embedding-API usage.
2. **Embedding dimension strategy** — standardize on a single 1536-dimension model across all applications (recommended, needs no migration) vs. support multiple dimensions via schema changes (larger scope, needs product justification for why multiple embedding models/dimensions must coexist).
3. **Keyword search over encrypted-content conversations** — when `conversation_content_persistence` excludes plaintext (`'none'`/`'metadata_only'`/`'encrypted_content'`, migration 0007:5-6), how should hybrid retrieval's keyword component behave? Options: skip keyword search (semantic-only) for such conversations, or transiently decrypt for the search operation only (higher complexity, must not log/cache decrypted content). Needs a decision before Sub-Phase C's keyword-search implementation.
4. **Contradiction-detection sophistication** (Sub-Phase F) — the plan recommends a simple `memory_key`-equality heuristic for the initial implementation; whether more sophisticated semantic contradiction detection is required for Definition of Done or acceptable as a documented follow-up is a product call given the scope size already acknowledged.
5. **Exact-span citations** — whether future product requirements need character/token-offset-precise citations (would require extending `PublicCitation` and likely `rag_chunks`' existing `start_offset`/`end_offset` columns, migration 0007:404-405, which already exist but aren't currently surfaced) — flagged as a natural, low-effort follow-up given the columns already exist, but explicitly not required for this plan's Definition of Done.
6. **`context_length_exceeded` vs the pre-existing `context_required_content_too_large`** — the catalog already contains `moira.error.context_required_content_too_large` (`src/i18n/catalog/errors.rs:79-83`). Sub-Phase D must explicitly decide whether the new budget-overflow condition is genuinely distinct (add the new key) or the same condition under a different name (reuse the existing key, add nothing). Do not let this be decided by accident. See § i18n catalog additions.
7. **`embedding_provider_unavailable` vs reusing `upstream_unavailable`/`upstream_timeout`** — Sub-Phase B must decide whether embedding-provider failures need their own vocabulary or should reuse the existing upstream keys. The plan leans toward a dedicated key (so embedding failures are distinguishable from completion failures in client telemetry), but either choice is acceptable if recorded.
8. **Strict-retrieval `failure_behavior`** — `moira.error.retrieval_unavailable` only fires when an application configures a non-default `failure_behavior` (migration 0007:109; default is `'continue_without_semantic_retrieval'`). Confirm with product that a strict/required-retrieval mode is actually wanted. If it is not, drop the key and test the default branch only — do not ship an unreachable error code.

---

## Re-audit corrections applied (verified against source at audit commit)

The following citations in earlier drafts were off and have been corrected in place. Everything not listed here was **re-verified as accurate**.

- **`create_rag_document`** spans `src/infra/repositories/conversation.rs:1013-1083` (line 1084 is blank; `list_rag_documents` begins at 1085) — earlier text said `1013-1084`. The hardcoded `'indexed'` literal at `:1064` is **confirmed**.
- **`deterministic_phase_five_order`** spans `src/application/conversation.rs:37-48` (fn at 37, array 38-47, closing brace 48) — earlier text said `37-47` / `37-49`. The enclosing `ContextPlanner` struct at `:34` + `impl` at `:36-49` is **confirmed**.
- **Migration 0007 notify triggers** span `528-566`, not `528-567` — the file is 566 lines.
- **`conversation_content_persistence`** appears literally only at `migrations/0007…:5-6`. The previously-cited `:171-172` and `:376-377` are the `content_encrypted`/`content_plain` **column pairs** (in `conversation_messages` and `rag_document_versions`) that *implement* that policy — accurate as an implementation reference, but the policy column name is not at those lines. Open Decision 3 (keyword search over encrypted content) depends on this distinction, so it is worth being precise about.
- **`docs/rig-integration.md`** provider bullets are at `:18-24` under the header at `:16` — earlier text cited `16-24` for the bullets.
- **`RagDocumentIngestRequest`** — the `pub struct` is at `src/domain/conversation.rs:611` with its derive at `:609`; the cited `611-617` is correct for the struct itself.

**Confirmed still accurate** (spot-checked, no drift): `ingest_rag_document` at `src/infra/repositories/conversation.rs:1138-1211` with the `'indexed'` literal at `:1184`, tx opened `:1144`, supersession `:1166-1176`, commit `:1209`; `citations: Vec::new()` at `src/application/public.rs:1657` and `public_response_from_record` at `:1614-1662`; `prepare_response_conversation` `:314-381`, `record_assistant_response` `:383-421`, `patch_memory` `:528-565`, `ingest_rag_document` service method `:947-979`, archived check `:343-349`, `estimate_tokens` `:1196-1198`, the existing unit test at `:1217-1224`, all seven `request_hash` call sites (`286,352,395,455,546,876,967`), and all four policy `invalidate_all()` lines (`615,654,695,736`); `PublicCitation` at `src/domain/conversation.rs:258-267` with **no span fields**; `PublicResponse.citations` at `src/domain/public.rs:167`; all four `Idempotency-Key` declarations at `src/http/conversation.rs:665,847,954,984`; `src/error.rs:53-65` envelope; `runtime_factory.rs` exists and `controls.rs:20` imports `RuntimeModelHandle`; and **every** cited line in `migrations/0007_conversations_memory_rag.sql` other than the notify-trigger range above.

### Missing work identified during the re-audit (now folded into the plan)

1. **No test infrastructure exists for this plan's entire domain.** `tests/` has no conversation/memory/RAG file at all, and `tests/support/mod.rs` has no fixtures for collections, documents, memories, or embeddings. Harness work is a **prerequisite deliverable of PR 11.AB**, not an assumed given. Earlier drafts listed test *files* without acknowledging the fixture gap.
2. **No deterministic embedding stub was specified.** Without one, the cross-tenant isolation assertion "the other tenant scores higher by raw similarity" cannot be made deterministic. Now specified in § Verification.
3. **The prompt/content-leak suite was under-specified.** It was one sub-bullet of a "Security" line; the audit flags it as *entirely absent* from the repo (`00-audit-report.md` P1-10). It is now a named e2e file (`tests/rag_content_leak.rs`) with canary-token methodology, owned by PR 11.D.
4. **Retrieval-run counts as an inference channel** was not covered. `retrieval_runs.*_candidate_count` must count only in-scope candidates; a cross-tenant count leaks information even when no content is returned. Now a named isolation test.
5. **The diagnostic endpoints had no isolation test.** `moira:diagnostics:read` is a new read path over provenance data and needs its own cross-application negative test plus a deny-by-default scope test — not just coverage of the retrieval path.
6. **The zero-in-scope-candidates degenerate case** was not covered: when the caller's scope has no candidates at all, retrieval must return empty, never fall back to a global nearest neighbour. Now a named isolation test.
7. **i18n was entirely absent from this plan.** No key list, no catalog check, and the one `message_key` the plan did state was written without the mandatory `moira.error.` prefix. Now a full § i18n catalog additions section, with the six already-existing reusable keys identified so they are not duplicated.
8. **Branch/PR workflow was entirely absent.** Now specified as a nine-PR stack with explicit base-PR and no-force-push rules.
