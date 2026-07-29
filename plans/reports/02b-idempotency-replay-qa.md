# Plan 02b — QA report (idempotency replay)

Merged as [#24](https://github.com/nurcahyo/moira/pull/24), commit `e1c2658`.

Four adversarial lenses reviewed the implementation before merge: correctness, security,
test-integrity, and conventions compliance. Eleven agents total, ~4.5M tokens.

## Verdict

The implementation was **substantively correct**. The *evidence* for two of its Definition-of-Done
boxes was not — both were provably unfalsifiable, and both were fixed and re-verified before merge.

## What was proven correct, and how

The transaction seam — the plan's own "single highest-risk defect" — was confirmed by PostgreSQL
row-level transaction ids rather than by reading code. After a keyed ingest on a fresh database:

```
ledger row   xmin = 33961
version row  xmin = 33962
audit row    xmin = 33962
```

The apparent split is exactly savepoint sub-transaction id allocation; the reviewer reproduced the
identical N/N+1 pattern in isolation (`begin; insert A; savepoint sp; insert B; release sp; commit`).
That positively confirms the intended structure: **claim before the savepoint, mutation and audit
inside it, one top-level commit.**

Also verified: authorization runs *before* the ledger claim on all three methods, so a rejected request
never occupies a key; the ledger is scoped by `(idempotency_key_hash, actor_fingerprint, operation)`,
checked in DDL and against the live catalog; the repository extraction introduced zero SQL drift (every
literal and `.bind()` diffed against `main` — identical); and `/ingest` ↔ `/reindex` share one operation
identity without cross-document aliasing, verified by computing real hashes through the shipped code.

## The two surviving mutations

Nine of eleven mutations were killed by the suite as written. Two survived:

| | Mutation | Result |
|---|---|---|
| M5 | Move the audit write outside the transaction — the exact pre-02b non-atomic behaviour this plan claims to fix | **13/13 passed** |
| M6 | Corrupt the stored replay status (201→503, 200→418) | **13/13 + 8/8 passed** |

**M5 is the important one.** The plan's DoD said audit atomicity was *"proven by
`rolled_back_ingest_leaves_no_audit_row_and_no_partial_version`"*. That test installed a trigger that
raised *inside* the audit insert — so zero audit rows existed regardless of which connection wrote them.
The property was real and correctly implemented; the test simply could not observe it.

**Fix.** The reviewer proposed a `before update on idempotency_records` trigger. That was rejected
because it retains database-global DDL, which was a separate finding. Instead the test now compares
PostgreSQL's `xmin` system column: an audit row and the row its mutation produced share a transaction id
only if one transaction wrote both. No DDL, no locks, nothing to leak, and it observes the committed
success path. Mutation-verified at all three sites (`44404` vs `44403`, etc.).

**M6 fix.** Handlers hardcode 201/201/200 and discard `outcome.status`, so the ledger's stored status was
never observable. Now asserted directly against `idempotency_records.response_status`; kills the mutation
three ways.

## Security findings

**A test installed a trigger on `audit_logs` and dropped it only on the happy path.** The security lens
hit a real `40P01` deadlock **at the cleanup statement** — the drop never ran, leaving a plpgsql trigger
firing on every audit insert in the database. `CREATE`/`DROP TRIGGER` also takes `AccessExclusiveLock` on
the table, blocking every audit write database-wide while installed. The test now scripts its failure
through an over-length `x-request-id` against the existing `varchar(128)` column: no DDL, nothing to leak.

**Flaky isolation tests had a real cause.** Two tests used *fixed* actor subjects, and `actor_fingerprint`
derives from the subject and sits in both the ledger unique index and the advisory-lock key — so
concurrent test binaries genuinely collided, producing a different body replaying `200` instead of
conflicting. Now fixture-suffixed. Verified: 10 serial runs and **26 concurrent binary runs across 13
rounds**, zero failures, zero deadlocks, zero leaked triggers.

## One DoD box reported NOT MET, by design

All four lenses independently found the same false statement. Plan 02b added a comment reading *"The
single definition of the admin actor fingerprint in the crate."* There are two — and the second, in
`runtime_admin.rs`, writes to the **same ledger table** while omitting issuer, application and tenant.
Proven with a standalone binary linking the real crate:

```
base          admin=T5QUEfaSkuLr  runtime=oBmwFVvCAAAh
other_issuer  admin=vmwVQAs2qWzR  runtime=oBmwFVvCAAAh   <- does not isolate issuer
other_tenant  admin=V6o0XSPi2-J9  runtime=oBmwFVvCAAAh   <- does not isolate tenant
```

The plan's DoD requires exactly one definition while its own Excluded scope forbids touching the second,
so the box was never satisfiable within this plan. Pre-existing on `main`; 02b's routes are unaffected
because `operation` is in the unique index and the `rag.*` operations are disjoint. The comment was
corrected to warn rather than mislead, the unification recorded in `TODO.md` for plan 06, and the
contradiction recorded in `NEED_CONFIRMATION.md`.

## Gates

All five green, run raw. `cargo test --workspace --all-features`: 164 passed, 0 failed. Per-suite wall
clock recorded, since a `0.00s` e2e suite means it silently skipped.

## Deviation

Verified on PostgreSQL 18.3, not the pinned 16 — Docker is unavailable on the dev machine. 02b adds no
migration and no version-sensitive SQL; three lenses independently assessed the gap as immaterial for
this diff. CI on PG16 remains the authoritative gate.
