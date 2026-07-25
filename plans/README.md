# Moira MVP Audit & Iteration Plans

Implementation-ready audit and iteration plans for taking Moira to a controlled MVP and beyond. **Planning only — no source code was changed to produce these documents.**

- **Audited commit:** `ea94eb939fe58864b04fec912daed1a0f0bfcb4b` (branch `codex/atomic-admin-idempotency`), tree-identical to `origin/main` (squash `356eec7`, tree `9261202…`). `git diff HEAD origin/main` is empty.
- **Verified gates at audit time:** `cargo fmt --check` ✅ · `cargo clippy --all-targets -- -D warnings` ✅ · `cargo test --workspace --all-features` ✅ **120/120** (real pgvector PG16 + Redis 7).
- **Method:** 8 parallel read-only specialist audits → reconciled into `00` → roadmap `01` → 10 iteration plans written by 5 parallel writers with disjoint file ownership.

## Execution status

Updated as each plan merges. A plan is **Complete** only when its PR is merged with all
`CONVENTIONS.md` §2 gates green and every Definition of Done box verified by a named, passing test
(§1 rule 5) — "implemented" is not "done".

| Plan | Status | PR | Merge commit |
|------|--------|----|--------------|
| `02a` | ✅ **Complete** | [#10](https://github.com/nurcahyo/moira/pull/10) | `36b05ee` |
| `02b` | ✅ **Complete** | [#24](https://github.com/nurcahyo/moira/pull/24) | `e1c2658` |
| `03` | ✅ **Complete** | [#25](https://github.com/nurcahyo/moira/pull/25) | `19b98ae` |
| `04` | ✅ **Complete** | [#26](https://github.com/nurcahyo/moira/pull/26) | `400ad70` |
| `05` | ⏳ In progress | — | — |
| `06` | ⬜ Not started | — | — |
| `07` | ⬜ Not started | — | — |
| `08` | ⬜ Not started | — | — |
| `09` | ⬜ Not started | — | — |
| `10` | ⬜ Not started | — | — |
| `11` | ⬜ Not started | — | — |

**Login milestone:** an operator can sign in once `07` (Moira-side identity claiming) and `08`
(Next.js console + Google OAuth) are both complete. `02b`–`05` are the backend MVP gates that
precede them; `06` is recommended before `07`.

Open decisions carried out of executed plans live in [`../NEED_CONFIRMATION.md`](../NEED_CONFIRMATION.md);
deferred hardening lives in [`../TODO.md`](../TODO.md).

## Index

| File | Purpose |
|------|---------|
| [`CONVENTIONS.md`](./CONVENTIONS.md) | **Binding cross-cutting rules — read first.** One-branch-one-PR per plan, mandatory unit **+** e2e tests, i18n key + English default, frontend toolchain pins (Next.js 16.2.11 / Node 24 LTS / Bun 1.3.14), Atomic Design layering, and the auth architecture (Better Auth BFF + Moira-owned authorization, config DB-backed). **Where a plan conflicts with this file, this file wins.** |
| [`00-audit-report.md`](./00-audit-report.md) | Severity-ranked findings (P0–P3) with file:line evidence, MVP boundary, `docs/todo.md` reconciliation, positive findings. |
| [`01-roadmap-and-dependencies.md`](./01-roadmap-and-dependencies.md) | Ordering principles, Mermaid dependency graph, and the full Next.js / identity architecture decision. |
| [`02a-mvp-boundary-honesty.md`](./02a-mvp-boundary-honesty.md) | **MVP gate (P0).** Make the API honest: relabel no-op RAG/memory endpoints, document the preview boundary. No migrations — ships first and fast. Closes P0-1, P0-3. |
| [`02b-idempotency-replay.md`](./02b-idempotency-replay.md) | **MVP gate (P0).** Implement real `Idempotency-Key` replay on conversation/memory/RAG routes, reusing the existing admin ledger. Stacked on 02a. Closes P0-2. |
| [`03-security-hardening.md`](./03-security-hardening.md) | **MVP gate (P1).** HMAC+pepper idempotency hash, SSRF-hardened JWKS fetch, production HTTP middleware. |
| [`04-durability-correctness.md`](./04-durability-correctness.md) | **MVP gate (P1).** Real cursor pagination across all ~17 `ListResponse` list endpoints, retention cleanup worker, full execution-deadline coverage, streaming-stall test, `If-Match` on execution-policy PUT. |
| [`05-observability-ci-gates.md`](./05-observability-ci-gates.md) | **MVP gate (P0-4/P1).** Fix `cargo deny`, wire OTel + Prometheus histograms, OpenAPI-drift + secret/content-leak CI gates. |
| [`06-architecture-test-hygiene.md`](./06-architecture-test-hygiene.md) | Near-MVP (P2). Split `AdminService`, remove Rig domain leak, repo traits, delete dead code, test isolation, docs drift. |
| [`07-identity-foundation.md`](./07-identity-foundation.md) | **MVP gate for UI/OAuth (P1).** Moira-native owner/admin claiming: `(issuer,subject)` grants, setup-required detection. Backend-only. |
| [`08-nextjs-console-google-oauth.md`](./08-nextjs-console-google-oauth.md) | MVP gate for UI. Creates the Next.js BFF, setup wizard, and Google OAuth. |
| [`09-generic-oidc-github-invitations.md`](./09-generic-oidc-github-invitations.md) | Post-MVP. Generic OIDC, GitHub, invitations/additional admins, ownership transfer, recovery. |
| [`10-multi-replica-readiness.md`](./10-multi-replica-readiness.md) | Post-MVP. Redis-backed distributed limiter/concurrency/locks, admission/lease, leader election, durable workers. |
| [`11-rag-memory-intelligence.md`](./11-rag-memory-intelligence.md) | Post-MVP. Implements the real RAG/memory pipeline that `02a` honestly relabelled. |

## Reading order

1. [`CONVENTIONS.md`](./CONVENTIONS.md) first — the binding rules and the resolved decisions (§0). 2. `00` for what's wrong and how bad. 3. `01` for the sequence and the identity decision. 4. Iteration plans `02a`→`11` in order.

## MVP gates vs post-MVP

- **Backend controlled MVP (single replica):** `02a` + `02b` + `03` + `04` + `05`.
- **Admin console MVP:** add `07` → `08`.
- **Post-MVP:** `09` (identity extensibility), `10` (multi-replica), `11` (RAG/memory intelligence). `06` is recommended before `07`.

## Coordinator action items (open, must be resolved during execution)

1. **`src/infra/db.rs::listen_once` is edited by BOTH plan 06 (P2-14 scoped circuit reset) and plan 07 (auth-settings invalidation)** — merge deliberately, never blind-rebase.
2. **Catalog entries `database_unavailable` and `idempotency_in_progress` are owned by plan 06 but *required* by plan 07** — if 06 slips, 07 adds them; if both add them, 06's `docs_mirror_has_no_duplicate_keys` test catches the collision.
3. **Plan 07 adds 11 OpenAPI operations** → must land before plan 05's drift gate freezes the spec, or regenerate the snapshot in-PR (Wave 0 check). Same rule as plan 04's breaking `If-Match` change.
4. ~~Four product-input decisions~~ — **RESOLVED 2026-07-25.** All six open decisions are now recorded in [`CONVENTIONS.md`](./CONVENTIONS.md) §0 (D1–D6) and written into the affected plans: real replay instead of `501`; 02a/02b split; deny-by-default domains with **no** bootstrap exemption; `setup/auth-methods` stays authenticated; `email` required on **both** claim paths; histograms via the `metrics` crate + Prometheus exporter. Do not reopen these.
5. **Migration numbers must be assigned centrally at stage entry, never from plan text.** `0009` is
   already consumed by plan 02a's `0009_backfill_false_indexed_ingestion_status.sql`. Plans **04**
   (`0009_list_cursor_indexes`, `0010_retention_indexes`), **07** (`0009_admin_identity_claims`,
   `0010_auth_provider_settings`) and **10** (`0009_multi_replica_readiness`) each independently reserve
   `0009`/`0010`; **09** leaves it `00XX`. Four plans, two numbers, one already taken.
6. **`src/infra/db.rs::listen_once` is a THREE-way conflict, not the two-way one item 1 describes.**
   Plan **10** also adds functions to that file *while requiring lines 43-80 stay byte-for-byte
   unchanged* — which both 06 and 07 violate. Order 06 → 07 → 10, hand-merge each time, and re-baseline
   10's byte-for-byte invariant against the post-07 file rather than against audited `main`.
7. **Catalog duplication is four-way, not the two-way item 2 describes.** `database_unavailable` (+5
   infra codes) is added by **both** 05 and 06; `idempotency_in_progress` is claimed by **02b, 06 and
   10** (02b shipped it). `docs/i18n-response-catalog.json` and `src/i18n/catalog/errors.rs` are written
   by 9 of the 10 plans → nominate ONE i18n owner per stage; every other agent asserts presence rather
   than inserting.
8. **Plan text that is stale on arrival — do not follow it literally:**
   - Plan **10**'s `src/infra/metrics.rs` instructions cannot compile after plan 05. 05 deletes every
     `AtomicU64` field, `MetricsSnapshot` and `snapshot()` per decision D6; 10 still says to add them.
     05 runs first.
   - Plan **11** Sub-Phase B says "reuse the exact credential-resolution path at `resolver.rs:254-268`" —
     plan 06 Module 9 deletes `src/orchestration/resolver.rs` entirely, including `credential_priority`.
   - Plan **06** deletes `src/application/admin.rs` (→ an 8-file `src/application/admin/` directory).
     02b, 04 and 10 all edit that file; anything rebased across 06 must re-land in the correct
     sub-service file with `authz.require` intact.
   - Plan **04** moves missing-`If-Match` from `bad_request` to `if_match_required` on 7+ endpoints,
     breaking any concurrently-written test asserting the old code.
9. **Plans 10 and 11 each defer the worker job bodies to the other.** 10 registers stub handlers for
   `memory-extraction-retry`, `conversation-summarization-retry`, `embedding-retry` and
   `document-ingestion-retry`, reserving the bodies for 11; 11 calls the executor "an infrastructure
   concern this plan does not need to solve". A genuine gap, not a conflict — someone must own it.
10. **Plan 11 Wave 0 is a blocking research spike** — the Rig embedding API is unverified (`rig-core 0.40.0` resolved; `docs/rig-integration.md` has zero embedding references). If no supported provider exposes embeddings, plan 11 must be re-scoped; Moira does not build a second execution engine.

## Verified execution order

Derived from a full file-ownership analysis of every plan, not from caution. Each stage merges before
the next begins.

| Stage | Runs | Why |
|---|---|---|
| 1 | **02b** ∥ console scaffold | disjoint — the console shares no file with the Rust tree |
| 2 | **03** | collides with everything on the settings/state/lib/router spine |
| 3 | **04** | must precede 05 (breaking `If-Match` OpenAPI change) |
| 4 | **05** | freezes the spec — all spec-changing work lands first |
| 5 | **06** | run alone; must precede 07 for the `listen_once` merge |
| 6 | **07** | hand-merge `listen_once` onto 06's version; regenerate `docs/openapi.json` in-PR |
| 7 | **{ 08 ∥ 10 }** | the one genuinely wide stage — 08 is zero-Rust, 10 is zero-console |
| 8 | **11** (or 09) | interchangeable with 9; run 09 first if login matters more than RAG |
| 9 | **09** (or 11) | collides with 07's identity vertical and 11's route table |

**Plans 02b, 03, 04, 05 and 06 may never run concurrently** — they mutually collide on
`src/config/settings.rs`, `src/app/state.rs`, `src/lib.rs` and `src/http/mod.rs`. The only
parallel-safe pairs are `{08,10}`, `{08,11}` and `{08,06}`, each in a separate git worktree.

## Cross-plan dependencies surfaced by writers (honor at execution time)

- `02a` ships first (honesty, no migrations); `02b` stacks on it (replay). `02a`/`02b` and `03` are otherwise independent, but all must precede `05` (OpenAPI-drift gate freezes the spec — so all spec-changing work in `02a`/`02b`/`03`/`04` lands first). `04`'s `If-Match`-required change on execution-policy PUT is a breaking OpenAPI change that must precede `05`.
- `07` modifies `src/security/auth.rs`, which `03` also hardens — `07` must diff against `03`'s post-hardening state.
- `08` and `09` both block on `07`'s frozen contract. Setup-path reconciliation is resolved in `07` under the **`/api/v1/admin/setup/…`** namespace (binding on 08/09): the existing `GET /api/v1/admin/setup/status` (`src/http/admin.rs:32-49`, operator-gated **structural** readiness) stays as-is, and `07` adds two deliberately **separate** identity-claiming endpoints beside it — unauthenticated `GET /api/v1/admin/setup/claim-status` (returns only `{"claimed": bool}`) and `POST /api/v1/admin/setup/claim` (system-key- or setup-token-gated). The distinct `claim-status` segment keeps the two status concepts from ever being conflated; they answer different questions and must not be merged.
- `11` implements the pipeline behind endpoints `02a` relabels — keep `02a`'s honest status wording until `11`'s behavior is verified. `11` does **not** depend on `10`: its cross-tenant vector isolation is enforced at the SQL level per query and holds under today's single replica.
- `10`'s rollout ordering is mandatory: the Helm `replicaCount==1` guard (`charts/moira/templates/_helpers.tpl:16-26`) must never be relaxed before `10`'s Redis-backed controls **and** the Postgres admission lease are live. `10` *adds* Redis pub/sub invalidation alongside the already-working Postgres `LISTEN/NOTIFY` (`src/infra/db.rs:43-80`) — it does not replace it.

## Ground rules applied

Every plan is grounded in the audited code (file:line), reuses existing patterns (Argon2id+pepper hashing, the atomic-idempotency ledger, `If-Match`, `utoipa` OpenAPI generation, Postgres `LISTEN/NOTIFY`), keeps Moira's boundary intact (Moira owns config/identity-claims/credentials/authz/routing/persistence/streaming; Rig owns AI execution), never exposes system keys or decrypted credentials to the browser, and flags decisions that still need product input rather than inventing answers. Decisions the product owner has since made are recorded as binding in [`CONVENTIONS.md`](./CONVENTIONS.md) §0.
