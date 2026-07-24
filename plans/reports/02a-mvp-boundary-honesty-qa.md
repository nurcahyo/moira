# Plan 02a — MVP Boundary Honesty — QA Report

- **Plan:** [`plans/02a-mvp-boundary-honesty.md`](../02a-mvp-boundary-honesty.md)
- **Branch:** `plan/02a-mvp-boundary-honesty`, cut from `main` @ `ac46108`
- **Findings closed:** **P0-1**, **P0-3** (`plans/00-audit-report.md`). **P0-2 is deliberately not addressed** — it is closed by [`plans/02b-idempotency-replay.md`](../02b-idempotency-replay.md) per CONVENTIONS.md §0 decisions D1/D2.
- **Commits:** `60b2efd`, `f206b62`, `cd10096`, then review fixes `9677e8a`, `8dca73f`, `193308d`.

---

## 1. Files changed

| File | Change |
|---|---|
| `src/domain/conversation.rs` | `RagDocumentRecord.ingestion_status: Option<RagIngestionStatus>` + 2 unit tests |
| `src/infra/repositories/conversation.rs` | Both `'indexed'` write sites bound to `Pending`; `LEFT JOIN` + `ingestion_status` projection; conditional create-path re-select; outer `ORDER BY`; 1 unit test |
| `src/infra/pg_rows.rs` | `rag_ingestion_status_from_db` / `_to_db`; mapper populates the new field; 2 unit tests |
| `src/http/conversation.rs` | Boundary descriptions on 11 operations; 4 `Idempotency-Key` parameter descriptions |
| `src/http/mod.rs` | 6 OpenAPI contract unit tests |
| `src/domain/public.rs` | `citations` schema description |
| `src/application/public.rs` | `citations` construction-site comment |
| `src/application/conversation.rs` | 2 doc comments (`ContextPlanner`, `prepare_response_conversation`) |
| `migrations/0009_backfill_false_indexed_ingestion_status.sql` | **New** — resets legacy `'indexed'` rows |
| `tests/rag_ingestion_honesty.rs` | **New** — 8 e2e tests |
| `docs/public-api.md`, `docs/conversation-memory-rag-api.md`, `docs/idempotency.md`, `docs/document-ingestion.md`, `docs/todo.md` | Boundary statements; `todo.md` annotated only |
| `TODO.md`, `NEED_CONFIRMATION.md` | **New** — bookkeeping |

---

## 2. Gate output

Run verbatim on the final tree with `MOIRA_TEST_DATABASE_URL` set.

| Gate | Result |
|---|---|
| `cargo fmt --check` | **PASS** |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | **PASS**, zero warnings |
| `cargo test --workspace --all-features` | **PASS — 139 passed, 0 failed** |
| `cargo build --release --locked` | **PASS** |
| Clean-database migration validation | **PASS** — all 9 migrations apply from an empty database |

Per-suite: lib 103 · `admin_idempotency` 9 · `execution_lifecycle` 14 · `http_error_contract` 1 · `public_authorization` 3 · `rag_ingestion_honesty` 8 · `security_foundation` 1.

**Proof the e2e layer was not silently skipped:** no `skipping` line in the transcript, and
`rag_ingestion_honesty` took 0.66s against the ~0.01s early-return signature; `admin_idempotency`
(8.11s) and `execution_lifecycle` (5.48s) independently confirm real database work. This matters
because with the database absent these suites report `ok` while proving nothing — see `TODO.md`.

---

## 3. QA lenses, findings and resolution

Every lens was instructed to assume the implementation was wrong until proven otherwise.

### Correctness — 5 findings, all confirmed

| Finding | Resolution |
|---|---|
| **P1 — the new `LEFT JOIN` destroyed `list_rag_documents` newest-first ordering.** The inner `order by` only selects which rows survive the CTE `limit`; the outer select does not inherit it, and the planner may drive results from a seq scan of `rag_document_versions`. Reproduced with 5003 documents: the six most recently re-ingested were emitted **last**. | **FIXED** (`9677e8a`) — outer `ORDER BY` restated. Guarded by `rag_document_select_orders_the_outer_result`, asserting the clause in the emitted SQL. |
| **P2 — undeclared `version`/`ETag` change** on inline-content create (`1` → `2`, from re-selecting after the version-bump trigger). | **KEPT AND PINNED** (`193308d`) — it corrects a stale ETag. Asserted in test 2; disclosed in the PR and `NEED_CONFIRMATION.md`. |
| **P3 — new false claim in the boundary table:** conversation/message routes listed as not replaying `Idempotency-Key`, implying they accept it. They never declare it. | **FIXED** (`8dca73f`) — row corrected. |
| **P3 — superseded versions keep `'pending'` forever**; `RagIngestionStatus::Superseded` unreachable. | **ACCEPTED, PINNED AND TRACKED** — plan-predicted. Asserted in test 5; `TODO.md` hands it to plan 11. |
| **INFO — worktree diverged from HEAD mid-review** (the concurrent mutation agent). | No action — correctly identified as another agent's transient state; that lens reviewed `git show HEAD`. |

Verified clean by this lens: placeholder renumbering character-by-character at both inserts; no column shadowing; join cardinality ≤1; re-select inside the inline-content branch and within the transaction; zero `'indexed'` write-context matches; all plan prohibitions honoured.

### Security — no exploitable defect; 1 honesty gap actioned

| Finding | Resolution |
|---|---|
| **Pre-existing rows still report `"indexed"`** — no backfill, so legacy data kept the exact false value P0-1 targets, and the new `document-ingestion.md` sentence was false for it. | **FIXED** (`8dca73f`) — migration `0009` + corrected doc sentence. Backfill behaviour proven on seeded data. |
| Superseded status under-reporting, **untested**. | **FIXED** (`193308d`) — see above. |
| New e2e tests carry no authn/authz assertions. | **SCOPED OUT, TRACKED** — this lens itself proved auth is byte-identical across the diff (126 `security(...)` annotations, 209 guard call sites), so it is a pre-existing gap, not one 02a introduces. → `TODO.md`. |
| `rag_document_select(inner: &str)` has no compile-time literal guarantee. | **TRACKED** — all five call sites verified literal today. → `TODO.md`. |

Proven clean: no SQL injection (status is a bound parameter; all `inner` arguments are literals; hostile binds rejected by the CHECK and length constraints); no cross-tenant widening (`rag_document_versions.id` is the PK, so the LEFT JOIN cannot add rows); zero secret/URL/credential hits across the whole diff; no new network, file or SSRF surface; measured performance on 21k rows (list 4.7 ms, get 0.15 ms, re-select 0.19 ms) with no new lock — `create_rag_document` takes no `for update`, and `ingest_rag_document` gained no queries.

### Test integrity — 21 mutations applied, **0 survivors**

Every one of the then-17 tests is killed by at least one deliberate implementation break, including
both traps specifically probed: null-versus-absent (`HttpResult::field()` uses `object.get()`, not
`Value::Index`, so a missing key cannot masquerade as `null`) and symmetric-but-wrong status mapping
(the round-trip test asserts literal snake_case strings against the CHECK vocabulary). Fail-closed
behaviour was verified by execution: `CI=true` with the URL unset panics; `CI=false`/`0`/empty skip.
No `sleep()`, no `Barrier`; determinism confirmed over 8 runs including `--test-threads=8`.

| Finding | Resolution |
|---|---|
| **`reindex_..._without_ever_writing_indexed` is weaker than its name** — its `count = 0` check is laundered by the supersession `CASE`, which would rewrite a regressed `'indexed'` to `'superseded'`. | **FIXED** (`193308d`) — now asserts `versions[0].ingestion_status` directly. |
| **Skip-trap:** the whole e2e layer reports `ok` in 0.01s on a database-less machine, so the literal §2 gate is green while proving nothing. | **TRACKED** (systemic, inherited) → `TODO.md`; PR evidence shows the DB-backed run. |
| `citations` schema test matches lowercased substrings rather than verbatim constants. | **TRACKED** → `TODO.md`. Still catches deletion. |

### Conventions compliance — PASS

All CONVENTIONS §8 boxes verified or justified-N/A; all ten plan Definition-of-Done boxes VERIFIED
against named passing tests. The "zero new error codes / zero new notices" claim was independently
confirmed true (the one new `AppError::Internal` maps to the pre-existing `internal_error`, which is
present in both the Rust catalog and the JSON mirror, so no catalog change was required).
`docs/todo.md` confirmed annotate-only: 129 lines before and after, only lines 22 and 77 touched,
old text a strict prefix of new, TODO count 94 → 94. `conversation_summaries` appearing in
`docs/public-api.md` is the plan's own mandated negative sentence, not scope creep.

| Finding | Resolution |
|---|---|
| **Doc over-claim:** all four policy groups credited with gating behaviour, but retrieval and embedding policies are pure CRUD — nothing reads them. | **FIXED** (`8dca73f`) — split into two rows so each claim is true. |
| Plan's Wave 4 grep guard unsatisfiable (independently confirmed). | **SUBSTITUTED** — declaration-scoped grep returns 4. → `NEED_CONFIRMATION.md`. |
| Plan §8 said *replace* a docs line; implementation *appended*. | **ACCEPTED** — retained line is not false. → `NEED_CONFIRMATION.md`. |
| Six admin routes still advertise unimplemented replay. | **TRACKED** → `TODO.md`. |
| No PR at review time. | Resolved by opening the PR. |

---

## 4. Test evidence — named passing tests

**Unit (11).** `rag_write_routes_still_declare_the_idempotency_key_parameter` ·
`rag_collection_document_route_keeps_its_collection_id_path_parameter` ·
`conversation_memory_rag_operations_document_the_mvp_preview_boundary` ·
`rag_write_routes_carry_the_interim_idempotency_disclaimer` ·
`rag_document_record_schema_exposes_ingestion_status` ·
`public_response_schema_documents_always_empty_citations` ·
`rag_ingestion_status_round_trips_all_eight_variants` ·
`rag_ingestion_status_from_db_rejects_unknown_value` ·
`rag_document_record_serializes_ingestion_status_as_snake_case` ·
`rag_document_record_round_trips_through_serde` ·
**`rag_document_select_orders_the_outer_result`** (added in review).

**E2E (8), real HTTP surface against real PostgreSQL + pgvector.**
`ingest_rag_document_reports_pending_over_http_and_in_the_database` ·
`create_rag_document_with_inline_content_reports_pending_ingestion_status` ·
`create_rag_document_without_content_reports_null_ingestion_status` ·
`rag_document_get_and_list_report_the_same_ingestion_status` ·
`reindex_supersedes_the_previous_version_without_ever_writing_indexed` ·
`repeated_ingest_with_the_same_idempotency_key_creates_two_versions_until_02b` ·
`rag_document_error_responses_carry_catalog_message_keys` ·
**`list_rag_documents_stays_newest_first_after_reingestion`** (added in review).

Manual check required by plan §12c: every relative cross-reference in the four changed docs resolves
to a real file, and the `#mvp-boundary-conversations-memory-and-rag` anchor matches its heading.

---

## 5. Remaining risk

1. **PostgreSQL 18.3, not the pinned 16** — Docker unavailable locally. Assessed as low: only
   long-settled DML/CHECK/PK-join/trigger behaviour is exercised and no version-specific SQL is
   introduced. CI on PG16 is the authoritative gate. → `NEED_CONFIRMATION.md`.
2. **A migration shipped under a plan advertising none** — sanctioned by the plan's Risks section,
   but it rewrites existing audit-trail values. → `NEED_CONFIRMATION.md`.
3. **ETag/`version` change on inline-content create** — a correction, but a visible contract change
   in tension with the plan's "purely additive" claim. Pinned by test. → `NEED_CONFIRMATION.md`.
4. **Interim dishonesty window** — `Idempotency-Key` is advertised and ignored on four RAG routes
   until 02b merges. Deliberate per D2, mitigated by the parameter description, Sentence B, and the
   docs. If 02b slips past plan 05's OpenAPI freeze, a temporary gap becomes permanent.
5. **The e2e skip-trap** — the §2 gate cannot by itself prove the database layer ran.
6. **No authn/authz assertion** on the RAG routes in this suite (pre-existing).

---

## 6. Subagent model assignment

The `Agent` tool exposes a `model` parameter but **no** reasoning-effort knob, so depth was expressed
through model tier plus explicit instructions (CONVENTIONS-style "pair with effort *where the tool
supports it*").

| Agent | Model | Why |
|---|---|---|
| A — domain field | `sonnet` | Single-file additive schema edit; precision needed, reasoning depth not. |
| B — docs (5 files) | `sonnet` | Prose breadth with judgment about claim accuracy; no deep reasoning. |
| C — repository + row mapping | `opus` | The plan's own "one non-trivial part": join projection, transaction-ordering trap, placeholder renumbering — being wrong writes bad data silently. |
| D — OpenAPI contract text | `sonnet` | Mechanical breadth over 11 operations; risk is omission, not difficulty. |
| E — application doc comments | `haiku` | Two comments. Cheapest tier, but instructed to verify each claim true before writing. |
| F — unit tests (10) | `sonnet` | Test authoring at breadth against exact known strings. |
| G — e2e tests (7) | `opus` | Harness integration plus DB-level assertions — highest risk of tests that pass while proving nothing. It mutation-tested its own assertions unprompted. |
| QA — correctness | `opus` | Found the P1 ordering regression that all 17 tests missed. |
| QA — security | `opus` | Injection/tenancy/authz analysis where being wrong is expensive. |
| QA — test integrity | `opus` | Ran 21 mutations with restore-verification; the most expensive and most valuable lens. |
| QA — conventions | `opus` | Strict box-by-box gate against binding rules. |

Cheap tiers on mechanical stages paid for four `opus` reviewers. That trade found a P1 correctness
defect, one medium honesty gap, and two false documentation claims — none of which the passing test
suite detected on its own.
