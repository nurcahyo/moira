# Plan 04 — QA report (durability & correctness)

Merged as [#26](https://github.com/nurcahyo/moira/pull/26), commit `400ad70`.

Fourteen implementation agents, four adversarial lenses (pagination correctness, security, test
integrity, conventions), then five remediation agents.

## Verdict

The branch was **handed off red** — 3 failing tests — and three of its five headline findings had
mutation-surviving coverage. All were fixed and re-verified before merge.

## The defect that made it red was a coordination error, not an implementation error

All nine admin list handlers called `list_*(&actor, query.limit())`. An
`impl From<i64> for PageRequest` hardcoding `cursor: None` let that compile silently, so `PageQuery.cursor`
was discarded before the service ever saw it.

This made the endpoints **worse than before the plan**. Previously they returned `next_cursor: null,
has_more: false` — inert but honest. After, they minted a real signed `next_cursor` and `has_more: true`
that the handler then ignored on the way back in, so a conforming client re-receives page 1 forever.
`GET /api/v1/admin/audit-events` was the worst case: audit rows written in one transaction share
`occurred_at` exactly, so a compliance export silently loops over the same rows.

**Cause.** The coordinator scoped the admin-pagination specialist away from `src/http/admin.rs` on the
reasoning that cursor decoding happens in the application layer — but that layer still needs the HTTP
layer to *pass the raw cursor through*, and the file had been assigned to an agent scoped only to
If-Match. The implementer followed the brief exactly and documented the gap honestly in the `From<i64>`
"migration bridge" doc comment rather than concealing it.

**Fix.** All nine handlers wired, and `impl From<i64> for PageRequest` deleted so the compiler enforces
the wiring rather than a comment asking future authors to remember.

A tenth endpoint was also unreachable: `GET /api/v1/admin/rag-collections/{id}/documents` had a hardcoded
limit of 50 and no `Query` extractor, yet still ran `paginate()` — so a collection with >50 documents
advertised `has_more: true` and a `next_cursor` the route had **no parameter to accept**. Now wired, and
verified by walking 65 seeded documents to completion.

## The security finding: a proven lost update

The now-required `If-Match` did not prevent one. The service read the version on a pooled connection,
compared it in Rust, then issued a *separate unconditional* upsert with no `where version = $n` and no
row lock. Two writers holding the same currently-valid `If-Match`, released through a barrier:

```
starting version = 1
writer -> 200 OK version=2 rpm=31
writer -> 200 OK version=3 rpm=97
RESULT: successes=2 conflicts=0   (expected 1 and 1)
```

Reproduced 5/5 runs.

**Fix.** One transaction, `select … for update` so the comparison happens *inside* the row lock, plus —
belt and braces, since the lock alone is easy to regress — an `and version = $n` guard mapping zero
affected rows onto the existing `409 resource_version_conflict` envelope. Follows `rotate_credential`'s
established pattern rather than inventing a third one.

**Mutation evidence, and a correction worth recording.** Removing *only* the version guard left the test
passing — which initially looked like weak coverage. Reading the code showed the two mechanisms are
genuinely independent, and the row lock alone is sufficient, so that survival was correct. Removing
**both** reproduced the original defect exactly:

```
assertion `left == right` failed: two writers holding the same valid If-Match must resolve to
exactly one success and one conflict
  left: (2, 0)   right: (1, 1)
```

The lesson: a surviving mutation is a hypothesis to investigate, not automatically a finding. The wrong
conclusion here would have been to weaken a correct implementation.

The plan's DoD names that concurrency test. It had been **silently substituted** with an easier
malformed-header test, and the file header stated "no concurrency" as a design choice — honest, but not
what the DoD required. Restored to the barrier-gated version.

## Three mutations that survived the entire workspace

| Mutation | Before | After |
|---|---|---|
| Gut `bounded_phase` — the sole deadline enforcement point | 275 lib + all e2e green | **killed**, 2 DoD-named tests |
| `if false && retention_configured` — disable supervisor dispatch | whole suite green | **killed** by a new test |
| Revert `If-Match` to `Option<i64>` in the generated OpenAPI | 275 lib + 4/4 if_match green | **killed**, plus a 36-operation inventory pin |

The third mattered disproportionately: **plan 05 freezes `docs/openapi.json` from this generator**, so a
wrong spec frozen now would be expensive to unwind.

The retention one is notable because the *logic* was well tested — an over-broad delete predicate was
killed by "an unexpired idempotency record must survive the sweep", and removing `SKIP LOCKED` was
killed too. What was untested was whether the worker ever **ran**: every test called `run_once` directly
and nothing referenced `WorkerSupervisor` at all.

## A documented behaviour that was backwards in both halves

The mid-sweep comment claimed a row bumped during pagination "can be returned twice… or missed entirely
(if it moves behind the cursor)". `updated_at` is set by a trigger to `now()`, so it only ever
**increases**. Under `updated_at desc` with `(updated_at, id) < cursor`:

- an already-returned row sits *above* the cursor; bumping pushes it further above → it can **never**
  repeat. Duplicates are impossible.
- an unreached row sits *below*; bumping moves it *above* the cursor → silently **skipped**.

Measured against the real database: bumping an unreached row made the sweep see 3 of 4 rows; bumping an
already-returned row produced a byte-identical page with no duplicate. The stated consequence — "callers
must reconcile by `id`" — was therefore also wrong; pages *are* disjoint, and what callers need is a
**completeness check, not a de-duplication pass**. Left uncorrected this would have sent client
reconciliation logic in precisely the wrong direction.

One nuance was added rather than glossed: `now()` is *transaction-start* time, so a write transaction
that began before a page was served and commits after it can land below the cursor and be returned
twice. That is the single bounded source of duplicates, and it is now documented.

## Test-suite hygiene

A false claim was made true rather than deleted: `tests/list_pagination.rs` said seeded rows are cleaned
up, while measurement showed `+20 applications, +20 routes, +187 audit_logs` per run. The fixture now
tracks and deletes every seeded id — audit rows first, since `application_id` is `on delete set null`
and they would otherwise survive detached. After: `+0 / +0 / +0` across 10 runs.

Cross-binary flakiness was reproduced and fixed: `purge_previous_runs` deleted rows belonging to
*concurrently running* test processes. Two concurrent binaries produced moving failure sets
(`7 passed; 5 failed`, then `8 passed; 4 failed`). Purge predicates are now bounded to rows untouched for
ten minutes. Final: 10/10 consecutive green, 6/6 across concurrent rounds.

## Gates

All five green, raw, captured to file and read back: fmt clean; clippy `--workspace --all-targets
--all-features -D warnings` clean; **391 tests, 0 failed**; release build clean; migrations 0001→0011
applied to a fresh database with 0010/0011 idempotent on re-run.

Migration numbers were assigned **centrally** — the plan text reserves `0009`/`0010`, but `0009` was
already consumed by plan 02a. Assigned `0010` and `0011`.

**DoD roster: 50/50 named tests present, 0 absent, 0 weaker substitutes.**

## Deferred

Five items in `TODO.md`. The widest: **the lost update is closed for exactly one endpoint.** Roughly 33
other `ensure_version(…)` call sites in `src/http/admin.rs` use the identical check-then-act shape with
no SQL guard and no row lock. Plan 04 owns only the execution-policy PUT, so the scoping is correct — but
the *class* of defect is not gone, only one instance. Assigned to plan 06.

## Tooling

`rtk` was again caught fabricating output: one command returned invented `jwks_*` failures where the raw
file showed `12 passed`, which would have produced a false NOT-MERGE-READY verdict. Every number in this
report was captured with `/bin/sh -c '… > file'` and read back.
