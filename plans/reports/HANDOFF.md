# Handoff — Moira plan execution

**Point a fresh agent at this file.** It is written to be the *only* thing that needs reading before
work resumes. Read it, then read `plans/reports/EXECUTION-LEDGER.md` — the ledger is the source of
truth for state; this file is the source of truth for *how to work here*.

Written 2026-07-31, `main` at `d709ed7`. **Not finished.** What remains is in §3.

---

## 1. The working agreement

Unattended, full autonomy including merge. The user is away for long stretches.

- **Decide rather than ask.** Where you would ask a question, research the tree, pick the best
  option, implement it, and record the decision **with the condition that would reverse it** in the
  affected plan's §0. A decision recorded that way is reviewable after the fact; a question asked
  into an empty room just stalls.
- **Escalate by writing, not by blocking.** Security findings, data-loss risks and anything needing
  a credential or a spend decision go into the ledger and the PR body — then keep working.
- **Merge on green CI.** All jobs green *with steps executed*. A job that ran and failed is real and
  blocks; investigate rather than override.
- **Never fake a gate.** No `--ignore-unfixed` to quiet Trivy, no weakened assertion, no DB suite
  skipping silently. If something cannot pass honestly, stop that thread, write down why, and
  continue with the rest.
- **OAuth stays mock-first.** No Google credential exists and none is to be requested. Build against
  the TLS mock IdP; defer anything genuinely needing live credentials with an explicit note.

## 2. The five rules that cost the most to learn

Each of these was learned by losing time to it. They are not style preferences.

### 2.1 A private `CARGO_TARGET_DIR` is not isolation. A private **worktree** is.

Cargo takes an *exclusive* lock on its target directory, so agents sharing one serialise no matter
how many you spawn. But a private target dir does **not** stop a `git checkout` in a shared tree
landing an agent's commit on the wrong branch — that happened twice.

**Any agent that will commit gets its own `git worktree`.** The coordinator must never run
`git checkout` in a tree an agent is using.

### 2.2 Exit codes lie here, in seven observed forms

1. `cmd | tail` reports `tail`'s status — hid a genuinely failed `docker build`
2. `grep -c` returns 1 on **zero** matches — made a fully green gate run look failed
3. `script; echo $?` reports `echo`'s status
4. A redirected `cargo test` **dropped whole test binaries** while exiting 0 (875 / 779 / 861 on one
   tree, all green)
5. zsh `noclobber` silently no-op'd a re-run, and the file read was another worktree's
6. `bun run x 2>&1 | tail` — same as (1), different runtime
7. A gate log naming a different worktree's path, losing every `test result` line

**Redirect to a file, capture `$?` immediately, then read the file.** Use `scripts/gates.sh`, which
handles this and asserts log completeness against `ls tests/*.rs`.

### 2.3 A test that passes is not a test that works

**Seven findings** where an assertion passed against broken code: a faked `'indexed'` status
laundered by a supersession `CASE`; an isolation filter moved to Rust returning the *right rows*; a
leak suite passing a deliberately injected leak under `E2E_SKIP_BUILD=1`; a metrics assertion
matching nothing because of a global label; five metric labels seeded but never emitted; a mutation
**nothing caught** (which is how F19's enumeration oracle surfaced); and nine `cargo-mutants`
survivors in code written that same day.

- **Mutation-test every new guard.** Break the thing it guards, confirm the test fails, revert.
  A guard nobody has seen fail is an assumption.
- **Every walker needs a vacuity guard.** Assert it found a plausible minimum; a walker that finds
  nothing asserts nothing and passes.
- **Every property test needs a premise assertion.** A cross-tenant isolation suite here was vacuous
  because the second application had no embedding policy, so its corpus was never indexed.
- `cargo-mutants` is adopted, scoped to touched code (`scripts/mutants.sh`, `docs/mutation-testing.md`).
  **Not** a CI gate — 2 hours for a ten-file change.

### 2.4 Re-audit every plan against the tree before writing code

Measured staleness in the five re-audited plans: **40%, 45%, 65%, 70%, 85%**. Every one needed a §0
drift section before it could be implemented. §0 wins wherever it disagrees with the body.

**And the expensive failures were never citation drift:**
- Plan 08's setup wizard **could never have succeeded** — it wrote the provider row for the IdP,
  sent the console's issuer in the claim, and never set `trusted_jwt_issuer_id`, so every run 403'd.
  It also stated an invariant it never exercised.
- Plan 11 required ingestion to reach `'indexed'` while a committed 28 KB suite asserted no row may
  **ever** hold `'indexed'`. It claimed no such test file existed; there were 31.
- Plan 09 extends a console UI that does not exist — **15 named artefacts absent** — and assumed a
  database that was never built.

Line-number citations are now banned in plans (cite symbols). That removes drift *volume*, not the
danger. **Keep re-auditing.**

### 2.5 Your brief will be wrong in detail — say so in it

Agents corrected the coordinator **~25 times** across this run, several load-bearing:
`distroless/nodejs24-debian12` does **not** pass Trivy (only debian13 reaches 0/0); §0's Redis-payload
guidance would have caused the exact defect it warned against; "four `pending` pins" was really ten;
a third RAG ingestion entry point was missed entirely; `PublicAuthMethod` was named as safe to serve
anonymously **while carrying the admin-claim domain policy**.

Twice a mechanism I asserted was wrong in a way that would have produced a **false-confidence test**.
End every brief with *"report anything in this brief you found wrong"* — every wave that was asked,
found something.

## 3. What remains

### 3.1 In flight

- **PR #39** — findings sweep (F20 single-primary ownership, F13 409, `cargo-mutants` adoption).
  891 tests, six gates green. **Awaiting CI. Merge when green.**

### 3.2 Plan 09, waves 4–5 (the only plan work left)

Read `plans/09-generic-oidc-github-invitations.md` §0 first — 9 blockers, re-sequenced into five
waves, waves 1–3 merged.

- **Wave 4 — multi-provider.** Its central task is **removing `ambiguous_enabled_providers`**.
  `auth-config.ts` currently refuses to guess when more than one provider is enabled, *deliberately*
  — so **enabling a second provider today breaks sign-in**. This is a redesign of a shipped safety
  decision, not an extension. Also needs GitHub storage: `auth_provider_settings`'s CHECK admits only
  `google_oauth`/`generic_oidc`/`jwks`, and GitHub is not OIDC (no issuer, no discovery). The
  migration is **unconditional** and must drop and re-add two CHECK constraints.
- **Wave 5 — invitations + ownership UI.** Session management is **cut** (user decision) unless
  waves 1–4 land comfortably; it needs durable storage *and* delivers nothing the invitation flow
  requires.

### 3.3 Open findings

| | What | Where it lives |
|---|---|---|
| **F14** | Memory dedupe silently stops matching after a pepper rotation | plan 11 Sub-Phase F |
| **F16** | `rig-core` logs the whole completion body — now carrying other tenants' retrieved documents. Mitigated below the `EnvFilter`; **proper fix is upstream** | needs an issue filed against rig-core, by a human |
| **F17** | Rotating `BETTER_AUTH_SECRET` makes the console publish a JWKS it cannot sign for — endpoint healthy, every token rejected | runbook in `docs/console-storage.md` |
| **F21** | A *failure* replay double-counts, distorting an operator's denial rate | plan 09 |
| **F2** | Pre-auth query-field enumeration | user deferred |
| **F6** | OTel bridges every recorded span; `env_filter` is the sole barrier to Rig prompt spans | unscheduled |
| — | Admin write + audit row still non-atomic | unscheduled |
| — | ~986 leaked `trusted_jwt_issuers` rows in the shared test DB | hygiene |

**Plan 11 Sub-Phases E (summarization) and F (memory extraction) are deferred**, stated in its PR
rather than implied. F14 belongs to F.

### 3.4 Two things only the user can do

1. **File the rig-core issue** for F16. Draftable, but it should go under a human's name.
2. **Supply a Google credential** if the OAuth mock/live seam ever needs closing. Everything is
   verified against a real TLS mock IdP with real signed JWTs — what cannot be proven without a
   credential is Google's own token claims, consent screen and key rotation. Recorded, not implied.

## 4. Mechanics

- **Test DB:** `MOIRA_TEST_DATABASE_URL='postgres://postgres:postgres@127.0.0.1:5432/moira'` — exactly
  this. A wrong URL makes DB tests skip **silently** and report green; that invalidated a round of
  results here once. Console DB: `CONSOLE_TEST_DATABASE_URL=…/console_auth_test`.
- **Gates:** `scripts/gates.sh` (six gates; asserts zero skipped DB suites and log completeness).
  Console: `bun install --frozen-lockfile && bun run typecheck && bun run lint && bun test && bun run build && bun run e2e`.
- **Disk — check every cycle.** `df -g .` **and** `du -sh ~/.cargo-targets/*`; usage is
  `main + N × ~2 GB` and grows with every agent. Below 60 GB free run `scratchpad/reclaim.sh`; below
  30 GB also delete finished agents' target dirs. **Delete them routinely, not only under pressure.**
  `debug = 1` took a full build from 20 GB to 2 GB and a cold rebuild to 2m21s.
- **Migrations** are append-only; next free number is **`0020`**.
- **OpenAPI** is frozen: regenerate with
  `UPDATE_SNAPSHOTS=1 cargo test --lib http::tests::committed_openapi_matches_the_generated_document`.
  Two gates enforce it, plus a hardcoded route list *and an exact operation count* in
  `generated_openapi_covers_every_registered_route`.
- **Commits:** `git commit --only -- <paths>`, inline `-m`, never bare `git commit` — the index is
  shared. Commit **incrementally**; two stalls left real work uncommitted.
- **Stale worktrees** from this run can be pruned: `git worktree list`, then
  `git worktree remove <path>` for any whose branch is merged.

## 5. Compaction

This file plus the ledger must be enough to resume from one read. Before compacting, verify:
working tree clean and pushed; the ledger's "State at a glance" reflects reality (it did **not**,
once — two merged plans were missing); every decision since the last checkpoint is in a plan's §0
with its reversal condition; and running agents are named with their branch.

If any is false, **make it true first**. That is cheap; re-deriving lost state is not.
