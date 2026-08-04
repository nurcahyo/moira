# Decisions taken by plan runners — evidence record, **not yet confirmed by a human**

Record of the decisions that plan runners took unilaterally while executing plans 02a and 02b, each
of which the runner flagged for a human in the repo-root `NEED_CONFIRMATION.md`. This document
collects, per decision, the recommendation the runner made, the evidence that it was in fact
executed in the tree, and the condition under which it should be reversed.

> **Confirmation status: none of the decisions below has been signed off.**
>
> Issue [#96](https://github.com/nurcahyo/moira/issues/96) asks for exactly that sign-off. At the
> time of writing it is **open, unassigned, and carries no comments** — so there is no written
> maintainer approval to cite, and this document does not claim one. Verified with:
>
> ```
> $ gh issue view 96 -R nurcahyo/moira --json comments,state,assignees,closedAt
> {"assignees":[],"closedAt":null,"comments":[],"state":"OPEN"}   # 2026-08-04T18:19:09Z
> ```
>
> The "Runner recommendation" column below is therefore **a runner's proposal awaiting
> confirmation**, not an approved answer. Two of these decisions are not cosmetic — decision 1
> rewrites existing production audit values via migration `0009`, and decision 3 is a breaking
> response/`ETag` contract change — and both are already shipped. That is precisely why they need a
> human signature rather than an inferred one.
>
> Until a maintainer records an answer on issue #96, the decisions stay open in
> [`NEED_CONFIRMATION.md`](../NEED_CONFIRMATION.md). When they are answered, update the
> "Confirmed?" column here with a permalink to the comment that answered them, and only then empty
> `NEED_CONFIRMATION.md`.

**Verification.** Every claim below was re-checked against the tree at commit `40acb6e`
(`origin/develop`, 2026-08-04) before it was written here. Where a claim could not be verified from
the tree, this document says so instead of asserting it. Line numbers are from that commit and will
drift; the identifiers and file paths are the durable part.

**Why this file lives in `docs/` and not `plans/reports/`.** Issue #96's acceptance criterion
suggests `plans/reports/` as the archive destination. That directory holds point-in-time artefacts
of a single plan run (`02a-…-qa.md`, `EXECUTION-LEDGER.md`); this record spans two plans, is
expected to be updated as decisions are confirmed or reversed, and is referenced from `TODO.md` and
`plans/RUNNER-PROMPT.md` §10 as the standing destination for answered decisions. It is a deviation
from the issue text and is called out here rather than passed over; moving it is a `git mv` plus
four link updates if the maintainer prefers the stated location.

Open items that still need doing live in [`TODO.md`](../TODO.md). New decisions still go to
[`NEED_CONFIRMATION.md`](../NEED_CONFIRMATION.md) in the format `plans/RUNNER-PROMPT.md` §10
specifies.

## Summary

"Executed?" is a fact about the tree and is verifiable from the evidence in each section.
"Confirmed?" is a fact about a human signature and is **"not yet"** for every row — see the
confirmation-status note above. The two are independent: everything except decision 4 is already
live in `main` *without* having been approved, which is the risk this record exists to make visible.

| # | Decision | Runner recommendation | Executed? | Confirmed? |
|---|----------|-----------------------|-----------|------------|
| 1 | Backfill migration `0009` shipped under a plan advertising no migrations | Keep it | Yes | Not yet — **rewrites existing audit values** |
| 2 | Plan 02a verified on PostgreSQL 18.3, not the pinned 16 | Accept, CI on PG16 is the authoritative gate | Yes | Not yet |
| 3 | Inline-content create now returns `version: 2` / `ETag: "2"` | Keep it, pinned by test | Yes | Not yet — **breaking contract change** |
| 4 | Plan 02a/02b's `Idempotency-Key` grep guard contradicted the same plan's own §5(b) | Fix the plan text | **No — still open**, owned by issue #82 | Not yet |
| 5 | Plan 02b's DoD requires one `actor_fingerprint`, its Excluded scope forbids the fix | Defer the unification to plan 06 | Yes, and plan 06 then did the unification | Not yet |
| 6 | Plan 02b verified on PostgreSQL 18.3, not the pinned 16 | Accept, same rationale as #2 | Yes | Not yet |
| 7a | Plan §8 said *replace* a docs line; the implementation *appended* | Accept | Yes | Not yet |
| 7b | Sentence-A test asserts a stricter property than the plan's literal wording | Accept | Yes | Not yet |

---

## 1. Backfill migration `0009` under a plan advertising "no migrations"

**Question.** Plan 02a's headline is that it carries no migration. Fixing only the write paths would
have left every `rag_document_versions` row already on disk carrying the hardcoded `'indexed'`
status — the exact false value finding P0-1 exists to remove. Should a data migration ship anyway,
and is rewriting existing audit-trail values wanted?

**Runner recommendation (not yet confirmed).** Keep it. The audit's position is that `'indexed'`
was never true for those rows — `rag_chunks` and `rag_chunk_embeddings` had no writer — so there is
no legitimate prior state being destroyed. Without the backfill the API keeps serving the false
value with no way for a caller to tell a legacy row from a genuinely indexed one.

This is the decision with the largest blast radius if the recommendation is wrong: `0009` has
already run, so a maintainer who disagrees is choosing between the reversal below and living with
rewritten history. It should not be treated as settled until it is answered on issue #96.

**Evidence it was executed.**

- `migrations/0009_backfill_false_indexed_ingestion_status.sql` exists and is exactly the data-only
  statement described: `update rag_document_versions set ingestion_status = 'pending' where
  ingestion_status = 'indexed';`.
- Rows already `'superseded'` are left alone, as promised — the `where` clause names `'indexed'`
  only, and the migration header states the reasoning.
- The header also documents that the inverse `UPDATE` is only approximately reversible.
- Shipped in commit `36b05ee` ("plan 02a: MVP boundary honesty & API truth-in-advertising", PR #10).

**Reversal condition.** If someone later decides the pre-existing `'indexed'` values should have been
preserved: there is no schema change to undo, so reverting means reverting `0009` alone. Note the
migration's own warning — running the inverse `update … set ingestion_status = 'indexed' where
ingestion_status = 'pending'` is lossy, because after `0009` shipped, honest `'pending'` rows are
indistinguishable from the rows it touched. Reverting the code and leaving the data honest is the
safer direction.

---

## 2 and 6. Local verification ran on PostgreSQL 18.3, not the pinned PostgreSQL 16

**Question.** `plans/CONVENTIONS.md` §3 requires e2e tests against PostgreSQL 16 + pgvector, which
`docker-compose.yml` provides. Docker was unavailable on the dev machine, so plans 02a and 02b were
both verified locally against native PostgreSQL 18.3 with pgvector 0.8.5. Is merging on PG18-verified
evidence acceptable with CI on PG16 as the authoritative gate?

**Runner recommendation (not yet confirmed).** Accept. Neither plan introduces version-specific
SQL: 02a exercises plain DML, a `varchar` CHECK, a primary-key `LEFT JOIN` and a
`BEFORE UPDATE` trigger; 02b exercises
`pg_try_advisory_xact_lock`, savepoints, `select … for update`, a unique index, `on conflict do
nothing` and a plpgsql `BEFORE` trigger. All long-settled between 16 and 18. CI on PG16 is the gate
that actually decides.

**Evidence it was executed.**

- `.github/workflows/ci.yml:21` runs the `rust` job against the service image
  `pgvector/pgvector:pg16`.
- The same job sets `MOIRA_TEST_DATABASE_URL` (`:44`) — the variable the DB-backed suites require in
  order to run rather than skip — and runs `cargo test --workspace --all-features` (`:56`), plus a
  separate "Verify migrations against clean pgvector PostgreSQL" step (`:57-60`) against a second
  database on the same PG16 service.
- The `ci.yml` workflow has completed successfully on `main` since plans 02a (`36b05ee`) and 02b
  (`e1c2658`) merged — the most recent completed runs on `main` at the time of writing (2026-08-04)
  concluded `success`. So the PG16 gate has in fact run against a tree containing both plans.

**What could not be verified.** That the specific 02a/02b PR check-runs were green on PG16 at merge
time. The repository has a documented period where GitHub stopped producing `pull_request` runs
(see the comment at `.github/workflows/ci.yml:8-13`), so what is verifiable here is the post-merge
`main` runs, not the per-PR ones. `TODO.md` still carries "Re-run the 02a gates against PostgreSQL
16 in CI" as an open item; that item is arguably satisfied by the post-merge runs, but closing it is
left to whoever owns the `TODO.md` sync (issue #82) rather than asserted here.

**Reversal condition.** If PG16-specific evidence is required after all: install Docker, bring up
`docker-compose.yml`, and re-run the suites unchanged. Nothing in either plan needs to be modified
for that — only the evidence would be re-collected. Two related items already sit in `TODO.md`: the
02a gate re-run, and "Re-verify the pagination index choices on PostgreSQL 16" from plan 04, whose
`EXPLAIN` numbers have the same provenance.

---

## 3. Create-with-inline-content returns `version: 2` and `ETag: "2"` instead of `1`

**Question.** Plan 02a required `create_rag_document` to re-select the document row after the
version insert so the response reports the new version's real `ingestion_status`. That re-select
reads the row after `update rag_documents set current_version_id = …`, which fires the
`rag_documents_bump_version` BEFORE UPDATE trigger — so a create with inline `content` now reports
version `2`, and the ETag derived from it becomes `"2"`. Is the corrected-but-changed value wanted,
or should the old `1` be preserved by re-selecting before the trigger fires?

**Runner recommendation (not yet confirmed).** Keep it. The old `"1"` was stale against the
committed row; an immediate `If-Match` round-trip using it would have spuriously conflicted. It is a
correction, not a regression — and it is pinned by a test so it cannot drift back unnoticed.

This one is a breaking change to a shipped response contract, made without a human signature. It is
recorded here as a proposal, not as an approved deviation.

**Evidence it was executed.**

- `tests/rag_ingestion_honesty.rs:596-609` asserts `created.field("version") == json!(2)` for a
  content-carrying create, with a comment recording exactly why the value changed.
- The ETag follows from the same number rather than being computed separately:
  `src/http/conversation.rs:954` returns `etag_headers(record.version)` from the create handler.
  The assertion therefore pins the ETag transitively. There is no direct `ETag: "2"` assertion in
  `tests/rag_ingestion_honesty.rs` — the pin is on `version`.
- The change was disclosed in the 02a PR's breaking-changes section and in
  `plans/reports/02a-mvp-boundary-honesty-qa.md:59`.

**Reversal condition.** Re-select the document row *before* the `current_version_id` update fires
the trigger. The cost of doing so is the thing 02a was fixing: the create response loses the freshly
created version's `ingestion_status`. Blast radius of the current behaviour is the response body and
the header *value* only, because no RAG route reads `If-Match` today — check that this is still true
before treating a reversal as cheap.

---

## 4. Plan 02a/02b's `Idempotency-Key` grep guard — **NOT EXECUTED, still open**

**Question.** Plan 02a's Wave 4 review guard requires `grep -c 'Idempotency-Key'
src/http/conversation.rs` to return `4`, treating anything else as a hard review failure under
`CONVENTIONS.md` §0 D1. But the same plan's §5(b) mandated appending "Sentence B", whose text
literally contains the string `Idempotency-Key`, to those same four operations — which makes the
grep return `8`. Both requirements could not hold at once. Plan 02b repeats the same command.

**Runner recommendation (not yet confirmed).** Fix the plan text to count parameter *declarations*
rather than string occurrences. The runner substituted the declaration-scoped guard at review time
and did **not** edit the plan files, since the plans are owned elsewhere.

**Status: the plan text was never fixed.** Verified at `40acb6e`:

- `plans/02a-mvp-boundary-honesty.md:297` still reads ``grep -c 'Idempotency-Key'
  src/http/conversation.rs`` = 4.
- `plans/02b-idempotency-replay.md:316` and `:396` repeat it.

**What did change, and why the urgency dropped.** Plan 02b implemented real replay and replaced
02a's interim "Sentence B" with a truthful parameter description that does not contain a second
occurrence of the header name. On the current tree both counts now agree:

- `grep -c 'Idempotency-Key' src/http/conversation.rs` → `4`
- `grep -c '("Idempotency-Key" = Option<String>, Header' src/http/conversation.rs` → `4`

So the guard as written is satisfiable today. The contradiction was real but time-bound to the
window between 02a and 02b. The plan text is stale rather than actively breaking a reviewer.

**A second guard in the same list has the same defect — in its command, not its requirement.**
`plans/02b-idempotency-replay.md:396` check (3) reads: "`actor_fingerprint` has exactly one
definition in the crate (`grep -rn 'fn actor_fingerprint' src/`)". The *requirement* is satisfied
today and a reviewer reading it plainly would pass the tree — there is exactly one definition
(`src/application/admin/shared.rs:344`). It is the *suggested command* that misreports: it returns
**5** lines at `40acb6e`, because `grep 'fn actor_fingerprint'` also matches the four test functions
named `fn actor_fingerprint_…` in the same file.

So this is a bad hint attached to a good check, not an impossible check — a reviewer who runs the
parenthetical literally and treats `5 != 1` as a failure will be misled, which is worth fixing, but
the guard is not unsatisfiable. Whoever fixes the 02a/02b plan text should narrow this command in
the same pass (`grep -rn 'pub(crate) fn actor_fingerprint' src/` returns 1).

**Owner.** Issue [#82](https://github.com/nurcahyo/moira/issues/82) — "[docs] Sync stale documents:
TODO.md, docs/todo.md, docs/scaling.md, plan 02a/02b text". Not duplicated into a new issue, and
deliberately not fixed here: issue #96's scope is sign-off and archiving, not editing plan text.

**Reversal condition.** None on the decision itself. Guard (1) was genuinely self-contradictory at
the time it was written — the plan mandated text that made its own count fail — and guard (3)'s
requirement is fine while its suggested command over-counts. Neither is a judgement call that could
sensibly be reversed; what remains is execution, under issue #82.

---

## 5. Deferring the `actor_fingerprint` unification out of plan 02b

**Question.** Plan 02b's DoD and Wave 5 check both required `actor_fingerprint` to have exactly one
definition in the crate, while the same plan's Excluded scope said "No change to `runtime_admin.rs`".
The reviewer was asked to certify something the plan forbade fixing. Defer the unification to plan
06, or pull it into 02b?

**Runner recommendation (not yet confirmed).** Correct the DoD wording and leave `runtime_admin.rs`
alone in 02b; plan 06 owns the unification. The divergence was pre-existing on `main` — a 3-field
fingerprint in
`runtime_admin.rs` writing into the *same* `idempotency_records` table as the 10-field one in
`admin.rs`, so two `TrustedJwt` actors sharing a `sub` across different registered issuers could
replay each other's runtime-policy responses. 02b neither introduced nor widened it, and 02b's own
routes were unaffected because `operation` is part of the unique index and the `rag.*` operations
are disjoint from the runtime-policy ones.

**Evidence it was executed — and that plan 06 then finished the job.**

- There is exactly one definition of the formula in the crate today:
  `src/application/admin/shared.rs:344`, `pub(crate) fn actor_fingerprint(actor: &Actor) -> String`,
  over all 10 identity fields, with a doc comment (`:303-343`) that explains why each field must
  discriminate and states that the two weaker copies "are gone".
- `src/application/runtime_admin.rs` no longer defines its own formula; it imports the shared one
  (`src/application/runtime_admin.rs:14`, `application::{RequestContext, admin::actor_fingerprint}`)
  with a comment at `:11-13` recording that the 3-field copy was removed.
- Unification commit: `27fe021` "refactor: unify the actor fingerprint across admin, runtime-admin,
  and public".
- What survives of the old copies is read-only: `legacy_actor_fingerprint`
  (`src/application/runtime_admin.rs:1042`) and `legacy_public_actor_fingerprint`
  (`src/application/public.rs`), consulted in a fallback sweep so pre-deploy ledger rows stay
  replayable, never written back — `record_idempotency` writes `actor_fingerprint(actor)`
  unconditionally. The order is load-bearing: the current fingerprint is tried first, so a
  post-deploy row always wins (`src/application/runtime_admin.rs:933-936`).

**Outstanding follow-up (operational, not a decision).** `src/application/runtime_admin.rs:938-948`
carries a `TODO(post-deploy)`: delete `legacy_actor_fingerprint` and the second half of the fallback
sweep once every ledger row written before plan 06 shipped has expired. `idempotency_records.
expires_at` is set 24h ahead, so the earliest safe removal is deploy-date + 1 day. The comment is
explicit that this is gated on a **deploy**, not on a merge, and is deliberately owned by no plan.
`src/application/public.rs:1060` carries the matching TODO for its two legacy probes. Nothing in
`TODO.md` currently schedules either; that needs an owner and a date.

**Reversal condition.** If deferring turns out to have been wrong, the fix is not to re-open 02b —
plan 06 already landed the unification, so there is nothing left to pull forward. The reversible
part is the *removal* of the legacy fallback above: removing it before every pre-plan-06 ledger row
has expired means a client retrying an idempotent request across the deploy boundary misses its
ledger row and executes a second time. If in doubt, wait longer.

---

## 7a. Plan §8 said *replace* a docs line; the implementation *appended*

**Question.** Plan 02a §8 said to replace `docs/conversation-memory-rag-api.md`'s closing line
("OpenAPI includes schemas…"). The implementation appended a new section and left that line intact.
Acceptable?

**Runner recommendation (not yet confirmed).** Accept. The retained line is not false, so the
outcome satisfies the plan's intent — the reader gets the new information without losing accurate
information.

**Evidence it was executed.** The line survives at `docs/conversation-memory-rag-api.md:14`
("OpenAPI includes schemas for these resources and omits embeddings, extraction prompts, protected
instructions, and parser internals."), and the sections added after it are still there. Note the
document has since been substantially rewritten by plan 11 and by commit `270df5e` (F31), so it is
the retained *line* that is verifiable today, not 02a's original section layout.

**Reversal condition.** Delete the line if it ever stops being true — for example if the OpenAPI
document starts exposing embeddings or extraction prompts. That is a one-line docs edit, not a
reversal of anything structural.

---

## 7b. The Sentence-A test asserts a stricter property than the plan's literal wording

**Question.** Plan 02a §12a specified a test asserting "Sentence A" on every one of eleven
operations, but §5 defined two different Sentence A variants — a full form for the four RAG write
routes and a shortened form for the other seven. The implementation asserted the phrase common to
both (`used to influence model responses`) across all eleven, *plus* the exact per-group sentence.
Stronger than the wording, but an interpretation. Acceptable?

**Runner recommendation (not yet confirmed).** Accept. Neither reading changes behaviour, and the
stricter one catches more.

**Evidence it was executed — and that the shape survived a later inversion.** The per-group
structure is still there in `src/http/mod.rs`: `SENTENCE_A_RAG_WRITE` (`:1487`) and
`SENTENCE_A_SHORT` (`:1488`) are asserted verbatim against their own operation groups by
`conversation_memory_rag_operations_document_where_stored_content_is_used` (`:1623`), on top of
group-independent requirements that every description name `POST /api/v1/responses` and point at
`docs/conversation-memory-rag-api.md`.

The common-phrase half of the original assertion is **gone, deliberately**. Plan 11 wired chunking,
embedding, retrieval, context injection and citations, which made 02a's "these routes are inert"
phrasing false on eleven shipped operations at once — and the original test was holding that
falsehood in place. The test was inverted (commit `270df5e`, F31): the invariant is now
`INERT_PRIMITIVE_CLAIMS` (`src/http/mod.rs:1497-1504`), a six-entry list of claims a description
must **not** contain.

Precisely: the bare phrase `used to influence model responses` is *not itself* an entry in that
array — it only ever read naturally inside a negation, so what became forbidden are the two negated
forms that carry it, `"not yet used to influence model responses"` (`:1498`) and
`"is not used to influence model responses"` (`:1499`), alongside four other inert-primitive
phrasings. The test's own doc comment (`:1608-1622`) records the inversion and says the same thing.

So the stricter interpretation was taken, and the part of it that later became wrong was corrected
rather than preserved. Of the eight, this is the one whose confirmation matters least: the shape it
proposed has already been superseded by F31 on its own merits.

**Reversal condition.** None meaningful. The stricter per-group assertion is what a future plan
would keep; if a plan wants a weaker check it can relax `check()` in that test, but doing so would
re-open the door F31 closed.

---

## Still open

### Awaiting a human answer

All eight decisions above. They remain listed in [`NEED_CONFIRMATION.md`](../NEED_CONFIRMATION.md)
and issue [#96](https://github.com/nurcahyo/moira/issues/96) is the place to answer them. Answering
is cheap — the evidence is assembled here — but it has not happened, so nothing above may be cited
as approved.

### Awaiting an owner, not an answer

1. **Decision 4** — the 02a/02b plan text still contains the string-counting grep guard, and 02b's
   `fn actor_fingerprint` guard still suggests a command that over-counts. Owned by issue #82.
2. **Post-deploy removal of the legacy fingerprint fallbacks** (from decision 5) — four
   `TODO(post-deploy)` markers: `src/application/runtime_admin.rs:938` and `:1039`,
   `src/application/public.rs:1060` and `:2239`. Gated on a production deploy plus 24 hours.
   Issue #96 explicitly asked for this to be scheduled; it is now carried as an item in
   [`TODO.md`](../TODO.md) with its trigger condition written down, which is as far as a docs change
   can take it. It still needs a named owner and a date, and neither can be invented here.
