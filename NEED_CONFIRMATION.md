# Needs human confirmation

Decisions taken during plan execution that a human should double-check: ambiguous plan wording,
product choices not covered by `plans/CONVENTIONS.md` §0 D1–D7, security trade-offs made
unilaterally, and conflicts auto-resolved by a runner.

Ongoing work items live in [`TODO.md`](./TODO.md).

## Recommended answers (proceeding on these unless overridden)

Each open question below has a recommendation. Execution continues on these defaults rather than
blocking; overturning any of them is cheap and localised, and the reversal cost is noted.

| # | Question | Recommendation | If you disagree |
|---|----------|----------------|-----------------|
| 1 | Backfill migration shipped under a plan advertising none | **Keep it.** The audit's position is that `'indexed'` was never true, so there is no legitimate prior state; without it the API keeps serving the exact false value P0-1 targets. | Revert `0009` alone; no schema change to undo. |
| 2 | Verified on PostgreSQL 18.3, not the pinned 16 | **Accept, with CI on PG16 as the authoritative gate.** No version-specific SQL is introduced; only long-settled DML/CHECK/PK-join/trigger behaviour is exercised. | Install Docker and re-run; the suite is unchanged. |
| 3 | `ETag`/`version` now `2` on inline-content create | **Keep it.** The old `"1"` was stale against the committed row — an immediate `If-Match` round-trip would have spuriously conflicted. It is a correction, now pinned by test. | Re-select before the trigger fires; loses `ingestion_status` in the create response. |
| 4 | Plan 02a's Wave 4 grep guard is unsatisfiable | **Fix the plan text** to `grep -c '("Idempotency-Key"'`. Plan 02b's Wave 5 repeats the same broken command and will fail a compliant tree. | None — the current text cannot be satisfied by any implementation. |

Question 4 is the only one with a live downstream effect: it is corrected in plan 02b's execution
rather than left to fail that plan's reviewer.

| # | Question | Recommendation | If you disagree |
|---|----------|----------------|-----------------|
| 5 | Plan 02b's DoD requires `actor_fingerprint` to have exactly one definition, but its Excluded scope forbids touching the second one | **Correct the DoD wording, do not touch `runtime_admin.rs`.** The box as written is unsatisfiable within the plan's own scope; plan 06 owns the unification. | Pull the `runtime_admin.rs` unification into 02b — a wider security change with its own test surface. |
| 6 | Plan 02b verified on PostgreSQL 18.3, not the pinned 16 | **Accept, same rationale as question 2.** 02b adds no migration and no version-sensitive SQL; it exercises advisory locks, savepoints, `select … for update`, a unique index and a plpgsql trigger, all long-settled. | Install Docker and re-run; the suite is unchanged. |

---

## Plan 02b's Definition of Done contains a box its own Excluded scope makes impossible to tick

**Context.** Plan 02b's DoD and its Wave 5 check 3 both require that `grep -rn 'fn actor_fingerprint' src/`
return exactly one hit — "no divergent copy". It returns two:
`src/application/admin.rs:1375` (10 identity fields) and `src/application/runtime_admin.rs:702`
(`actor_type`, `subject`, `api_key_id` only). The same plan's Excluded scope says "No change to
`runtime_admin.rs`'s idempotency scheme". The reviewer is therefore asked to certify something the plan
forbids fixing. All four QA lenses flagged this independently.

**Why it is not cosmetic.** The `runtime_admin` copy writes into the *same* `idempotency_records` table.
A lens proved with a standalone binary linking the real crate that it fails to isolate issuer,
application or tenant — three actors differing only in those fields all hashed to `oBmwFVvCAAAh`. Two
`TrustedJwt` actors sharing a `sub` across different registered issuers can replay each other's
runtime-policy responses. This is **pre-existing on `main`**; 02b neither introduces nor widens it, and
02b's own routes are unaffected because `operation` is part of the unique index and the `rag.*`
operations are disjoint from the runtime-policy ones.

**What I did.** Left `runtime_admin.rs` untouched, and corrected the doc comment 02b had added at
`src/application/admin.rs:1369` — it read "The single definition of the admin actor fingerprint in the
crate", which is false and would have misled the next reader. It now names the second copy and points at
plan 06. Recorded the unification in [`TODO.md`](./TODO.md). The DoD box is reported as **not met, by
design**, rather than ticked.

**What I need confirmed.** That deferring the unification to plan 06 is right, rather than pulling it
into 02b — and that the plan text itself should be corrected so plan 06's reviewer does not hit the same
contradiction.

---

## Plan 02b was verified on PostgreSQL 18.3, not the pinned PostgreSQL 16

Same root cause as the 02a entry below: Docker is unavailable on this machine. Re-recorded here because
02b's evidence is separate. 02b adds **no** migration and no version-sensitive SQL; what it exercises is
`pg_try_advisory_xact_lock`, `savepoint`/`rollback to savepoint`, `select … for update`, a unique index
on `(idempotency_key_hash, actor_fingerprint, operation)`, `on conflict do nothing` and a plpgsql
`before` trigger — all long-settled with no semantic change between 16 and 18. Three independent lenses
assessed the deviation as immaterial for this diff. The one plausible 16-vs-18 difference is
lock-contention *timing* in the concurrency test, and that test asserts row-count invariants rather than
a timing outcome. CI on PG16 remains the authoritative gate.

---

## Plan 02a shipped a migration, though the plan advertises "no migrations"

**Context.** Plan 02a's headline is that it carries no migration and therefore ships fast, and its
PR template pre-fills "Migrations included — none". But fixing only the write paths leaves every
`rag_document_versions` row already on disk carrying the hardcoded `'indexed'` status — the exact
false value finding P0-1 exists to remove — and the API now surfaces that value with no way for a
caller to distinguish a legacy row from a genuinely indexed document. The new sentence in
`docs/document-ingestion.md` would have been false for all pre-existing data. The plan's own
Risks & Rollback section calls the backfill **recommended** and says "if taken, name it in PR
section 3", so it is sanctioned rather than invented.

**What I did.** Added `migrations/0009_backfill_false_indexed_ingestion_status.sql`, a data-only
`update rag_document_versions set ingestion_status = 'pending' where ingestion_status = 'indexed'`.
Rows already `'superseded'` are deliberately left alone, since that value describes being replaced
by a newer version, which genuinely happened. Verified against a seeded database: both `'indexed'`
rows became `'pending'` while `'superseded'` and `'failed'` were untouched. Named in the PR, and the
migration header documents that the inverse `UPDATE` is only approximately reversible — after this
ships, honest `'pending'` rows are indistinguishable from rows it touched, so rolling back the code
and leaving the data honest is preferable.

**What I need confirmed.** That shipping a data migration under 02a is acceptable rather than
deferring it, and that resetting legacy `'indexed'` rows is the wanted behaviour — this rewrites
existing audit-trail values, and the argument for it is precisely that those values were never true.

---

## Local verification used PostgreSQL 18.3, not the pinned PostgreSQL 16

**Context.** `plans/CONVENTIONS.md` §3 requires e2e tests to run against real PostgreSQL 16 +
pgvector, which `docker-compose.yml` provides via `pgvector/pgvector:pg16`. Docker is not installed
on this machine (no `docker` binary on PATH; the daemon socket is unreachable), so that image could
not be started.

**What I did.** Ran the full e2e layer against the native PostgreSQL **18.3** with pgvector 0.8.5,
in a dedicated `moira_test` database. All nine migrations apply cleanly from an empty database on
it. The independent compliance review assessed the gap as not materially weakening the evidence:
the assertions exercise plain DML, a `varchar` CHECK constraint, a primary-key `LEFT JOIN` and a
`BEFORE UPDATE` trigger, all long-settled behaviour with no semantic difference between 16 and 18,
and no version-specific SQL is introduced. Recorded as a follow-up in `TODO.md`.

**What I need confirmed.** That merging on PG18-verified evidence is acceptable with the CI run on
PG16 as the authoritative gate — or whether the merge should have waited for a PG16 environment.

---

## Plan 02a's Wave 4 review guard is self-contradictory

**Context.** The plan's Multi-Agent Workflow section requires the Wave 4 reviewer to confirm
`grep -c 'Idempotency-Key' src/http/conversation.rs` returns **4**, treating any other value as a
hard review failure under decision D1. But the same plan's §5(b) mandates "Sentence B", whose text
literally contains the string `Idempotency-Key`, appended to those same four operations. Following
§5(b) makes the grep return **8**. The two requirements cannot both be satisfied.

**What I did.** Kept the mandated Sentence B and substituted a guard that preserves the check's
actual intent — counting parameter *declarations* rather than string occurrences:
`grep -c '("Idempotency-Key" = Option<String>, Header' src/http/conversation.rs` returns exactly 4.
Three independent reviews confirmed the parameter survives on all four routes in the **generated**
OpenAPI, which is the property D1 cares about. Repo-wide the declaration count is unchanged at 21.

**What I need confirmed.** That the plan text should be corrected before plan 02b's reviewer runs
the stated command literally and fails a compliant tree. I did not edit the plan file, since the
plans are owned elsewhere.

---

## Create-with-inline-content now returns `version: 2` and `ETag: "2"` instead of `1`

**Context.** The plan requires `create_rag_document` to re-select the document row after the version
insert, so the response reports the freshly created version's `ingestion_status` instead of `null`.
That re-select necessarily reads the row *after* `update rag_documents set current_version_id = …`,
which fires the `rag_documents_bump_version` BEFORE UPDATE trigger. The handler derives its ETag
from that value, so `POST /api/v1/admin/rag-collections/{collection_id}/documents` with inline
`content` now returns `version: 2` and `ETag: "2"` where it previously returned `1`.

**What I did.** Kept the behaviour — it is arguably a correction, since the old ETag was stale
against the committed row and an immediate `If-Match` round-trip would have spuriously conflicted —
and pinned it with an assertion so it cannot drift unnoticed. Disclosed in the PR's breaking-changes
section. Note this sits in tension with the plan's claim that the change is purely additive with
"no header behavior changes". No RAG route reads `If-Match` today, so the blast radius is the body
and header *value* only.

**What I need confirmed.** That the corrected-but-changed ETag is wanted, rather than preserving the
old `version: 1` response by re-selecting before the trigger fires.

---

## Two minor deviations from the plan's literal instructions

**Context and what I did.**

1. Plan §8 says to **replace** `docs/conversation-memory-rag-api.md`'s closing line ("OpenAPI
   includes schemas…"). The implementation **appended** a new section and left that line intact.
   The retained line is not false, so the outcome satisfies the intent.
2. Plan §12a specifies a test asserting "Sentence A" on every operation, but §5 defines two
   different Sentence A variants — a full form for the four RAG write routes and a shortened form
   for the other seven. The test therefore asserts the phrase common to both
   (`used to influence model responses`) across all eleven, **plus** the exact per-group sentence.
   That is strictly stronger than the plan's wording, but it is an interpretation.

**What I need confirmed.** That both readings are acceptable; neither changes behaviour.
