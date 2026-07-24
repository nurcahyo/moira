# Needs human confirmation

Decisions taken during plan execution that a human should double-check: ambiguous plan wording,
product choices not covered by `plans/CONVENTIONS.md` §0 D1–D7, security trade-offs made
unilaterally, and conflicts auto-resolved by a runner.

Ongoing work items live in [`TODO.md`](./TODO.md).

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
