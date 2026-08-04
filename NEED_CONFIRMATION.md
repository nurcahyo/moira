# Needs human confirmation

Intake for decisions taken during plan execution that a human should double-check: ambiguous plan
wording, product choices not covered by `plans/CONVENTIONS.md` §0 D1–D7, security trade-offs made
unilaterally, and conflicts auto-resolved by a runner. Format is defined in
[`plans/RUNNER-PROMPT.md`](./plans/RUNNER-PROMPT.md) §10: `## <topic>` / **Context** / **What I
did** / **What I need confirmed**.

Ongoing work items live in [`TODO.md`](./TODO.md).

## Open decisions

**Eight, all from plans 02a and 02b, all still unanswered.**

The full record — the recommendation, the evidence that it was executed, and the reversal condition
for each — is in [`docs/decisions-taken.md`](./docs/decisions-taken.md). That document is an
*evidence* record, not an approval: at the time of writing, issue
[#96](https://github.com/nurcahyo/moira/issues/96) (which asks for the sign-off) is open,
unassigned and has no comments, so there is no written answer to point at.

This file therefore stays non-empty. Answer the items on issue #96, then move them across and empty
this section — not before.

Seven of the eight are **already shipped and running unapproved**, which is what makes them worth a
signature rather than a shrug:

| # | What needs confirming | Why it is not cosmetic | Detail |
|---|-----------------------|------------------------|--------|
| 1 | Backfill migration `0009` shipped under a plan that advertised no migrations | It **rewrites existing production audit values** (`ingestion_status` `'indexed'` → `'pending'`), and its inverse is lossy | [§1](./docs/decisions-taken.md#1-backfill-migration-0009-under-a-plan-advertising-no-migrations) |
| 2 | Plan 02a verified on PostgreSQL 18.3, not the pinned 16 | Accepts CI-on-PG16 as the only PG16 evidence; the per-PR check-runs could not be verified | [§2 and 6](./docs/decisions-taken.md#2-and-6-local-verification-ran-on-postgresql-183-not-the-pinned-postgresql-16) |
| 3 | Inline-content create now returns `version: 2` / `ETag: "2"` instead of `1` | **Breaking change to a shipped response/`ETag` contract**, already pinned by a test | [§3](./docs/decisions-taken.md#3-create-with-inline-content-returns-version-2-and-etag-2-instead-of-1) |
| 4 | Fixing the 02a/02b `Idempotency-Key` grep guard by counting declarations | The only item **not executed**; plan text is still stale. Owned by issue [#82](https://github.com/nurcahyo/moira/issues/82) | [§4](./docs/decisions-taken.md#4-plan-02a02bs-idempotency-key-grep-guard--not-executed-still-open) |
| 5 | Deferring the `actor_fingerprint` unification out of 02b into plan 06 | The deferred gap was a real replay-isolation weakness; plan 06 has since closed it, leaving a post-deploy cleanup with no owner | [§5](./docs/decisions-taken.md#5-deferring-the-actor_fingerprint-unification-out-of-plan-02b) |
| 6 | Plan 02b verified on PostgreSQL 18.3, not the pinned 16 | Same as #2 | [§2 and 6](./docs/decisions-taken.md#2-and-6-local-verification-ran-on-postgresql-183-not-the-pinned-postgresql-16) |
| 7a | Plan §8 said *replace* a docs line; the implementation *appended* | Interpretation call only; the retained line is still true | [§7a](./docs/decisions-taken.md#7a-plan-8-said-replace-a-docs-line-the-implementation-appended) |
| 7b | The Sentence-A test asserted a stricter property than the plan's wording | Interpretation call only; superseded by F31 on its own merits | [§7b](./docs/decisions-taken.md#7b-the-sentence-a-test-asserts-a-stricter-property-than-the-plans-literal-wording) |

**What each one needs.** A yes/no per row, recorded in writing on issue #96. A blanket "proceed on
the recommendations" is a valid answer and covers all eight — but it has to be *written down by a
human*, because rows 1 and 3 change data and a public contract respectively, and no runner should
be able to grant itself that approval by inference.

**When answered.** Record the answer and a permalink to the comment in the "Confirmed?" column of
`docs/decisions-taken.md`, then delete this section's table and leave "None." here.

---

Add new items above under "Open decisions", in the `plans/RUNNER-PROMPT.md` §10 format. When one is
answered, move it to `docs/decisions-taken.md` with its evidence and reversal condition instead of
deleting it.
