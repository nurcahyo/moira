# Handoff — Moira plan execution

**Point a fresh agent at this file.** It is written to be the *only* thing that needs reading before
work resumes. Read it, then read `plans/reports/EXECUTION-LEDGER.md` — the ledger is the source of
truth for state; this file is the source of truth for *how to work here*.

Written 2026-07-31. **All plan work is complete** — the forced order `02b → … → 09` is fully
executed. What remains is a short findings queue and three things only the user can do, both in §3.

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

## 2. The rules that cost the most to learn

Each of these was learned by losing time to it. They are not style preferences.

### 2.1 A private `CARGO_TARGET_DIR` is not isolation. A private **worktree** is.

Cargo takes an *exclusive* lock on its target directory, so agents sharing one serialise no matter
how many you spawn. But a private target dir does **not** stop a `git checkout` in a shared tree
landing an agent's commit on the wrong branch — that happened twice.

**Any agent that will commit gets its own `git worktree`.** The coordinator must never run
`git checkout` in a tree an agent is using.

### 2.2 Exit codes lie here, in TEN observed forms — and form 4's cause is now known

1. `cmd | tail` reports `tail`'s status — hid a genuinely failed `docker build`
2. `grep -c` returns 1 on **zero** matches — made a fully green gate run look failed
3. `script; echo $?` reports `echo`'s status
4. A redirected `cargo test` **dropped whole test binaries** while exiting 0 (875 / 779 / 861 on one
   tree, all green) — **see form 9; this was almost certainly the same cause**
5. zsh `noclobber` silently no-op'd a re-run, and the file read was another worktree's.
   **Recurred 2026-07-31**: `> $(mktemp)` on an existing file was refused, `cargo` never ran, and
   five runs reported `FAIL` with empty results. Use `>|`
6. `bun run x 2>&1 | tail` — same as (1), different runtime
7. A gate log naming a different worktree's path, losing every `test result` line
8. `git commit --only -- <paths> -m "…"` puts the message **after** `--`, so git reads it as a
   pathspec and aborts — and a chained `git push` then prints `Everything up-to-date`. The pair reads
   as success. **`-m` must come before the `--`**
9. **THE WORST ONE. A `PreToolUse` hook rewrites your `cargo` command and replaces the log with a
   one-line summary — even when you redirect to a file.** `~/.claude/hooks/rtk-rewrite.sh` runs
   `rtk rewrite` on every Bash command. Verified directly:

   ```
   $ rtk rewrite 'cargo test --workspace --all-features > /tmp/x.log 2>&1'
   rtk cargo test --workspace --all-features > /tmp/x.log 2>&1     # exit 0 → rewritten
   $ rtk rewrite 'bash scripts/gates.sh'
   (nothing)                                                        # exit 1 → untouched
   ```

   The redirect survives; the **command** is replaced. So the file receives
   `cargo test: 2 passed (1 suite, 1.39s)` instead of every `test result:` line and all
   `--nocapture` output. An agent measuring anything this way silently measures nothing, and
   "redirect to a file, then read the file" — the rule directly above — **does not save you**.

   **`cargo` invoked from inside a script file is immune**, because the hook only sees the outer
   command. That is why `scripts/gates.sh` has never been bitten by this, and it is the reason to
   keep using it rather than hand-rolling.
10. `pgrep -fc` is **not valid on macOS** — it prints usage to stderr and yields `0`, so a
    concurrency check reports "no peers" during a busy run

**Redirect to a file, capture `$?` immediately, then read the file — and run cargo from inside a
script.** Use `scripts/gates.sh`, which handles all of this and asserts log completeness against
`ls tests/*.rs`.

### 2.2b `scripts/gates.sh` CANNOT run concurrently with another gates run

Found 2026-08-01, after two runs sat **wedged for 40+ minutes**.

`sweep_leaked_databases` (`tests/support/mod.rs`) drops **any** `moira_test_template_*` other than its
own digest. Two runs with different migration sets therefore sweep each other in a loop, producing
`template database "…" does not exist` mid-run. Worse, it can **hard-deadlock**: one process holding
shared template locks for live fixtures blocks another's exclusive request, while its own next
fixture queues behind that exclusive request. Clear it with `pg_terminate_backend` on the idle lock
holders.

**Consequence for the loop: serialise gate runs.** Parallel agents are fine while they are reading,
designing, or editing — but only one may be in `scripts/gates.sh` at a time. Stagger them, or give an
agent its own database.

Three related traps from the same session:

- **`ps aux | grep` gives false negatives here — use `pgrep`.** An agent had *three* of its own gate
  runs stacked without seeing them.
- **Never edit a migration after any gate run** — `migration N was previously applied but has been
  modified`. Remedy: `delete from _sqlx_migrations where version = N` on the shared DB.
- **The shared `moira` database is migrated by unit tests** in `src/**/tests` that connect to
  `MOIRA_TEST_DATABASE_URL` directly. So a *merged* migration leaves every branch **without** it
  failing its shared-DB unit tests with `VersionMissing` until it rebases. Same one-line remedy.
- **`sqlx::migrate!` is a proc macro** and does not reliably invalidate cargo's fingerprint when
  migration files change, so a test binary can embed a stale migration digest. `cargo clean -p moira`.

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

### 3.1 ALL PLAN WORK IS COMPLETE (2026-07-31)

The forced order `02b → 03 → 04 → 05 → 06 → 07 → {08 ∥ 10} → 11 → 09` is **fully executed**. Six PRs
merged in the final cycle, each CI-verified with every job running steps:

| PR | What | Merge |
|---|---|---|
| #39 | findings sweep — F20, F13, F21, `cargo-mutants` | `5206ffd` |
| #40 | F22 — a timeout probe racing a sub-millisecond deadline | `f3a9480` |
| #41 | wave 4A — deterministic `admission_policy`, F23/F24/F25/B2 | `c98aeb7` |
| #42 | wave 4B — per-provider console issuer, N sign-in buttons | `da384c8` |
| #43 | wave 5 — invitations and ownership UI | `820a5a8` |
| #44 | F6 — allow-list on the OTLP bridge | `f2c24a8` |

Migrations end at **`0020`** (next free `0021`; `0016` is a permanent gap). OpenAPI is stable at
**151 operations / 99 paths / 178 schemas**.

### 3.2 Open findings

| | What | State |
|---|---|---|
| **F16** | `rig-core` logs the whole completion body, now carrying other tenants' retrieved documents. Mitigated below the `EnvFilter` — **and that mitigation's own wiring test was missing until `8bbda15`** | **proper fix is upstream; needs an issue filed by a human** |
| **F2** | Pre-auth query-field enumeration | user deferred |
| ~~**F27**~~ | ~~Leaked `trusted_jwt_issuers` rows in the shared test DB~~ **CLOSED** `fix/test-row-leak`. **The recorded count was wrong**: it said ~986; the measurement was **160** — exactly ten rows (the ten `register_issuer` call sites in `tests/jwks_hardening.rs`) × sixteen runs, and it leaked them on the **happy** path, not only on a panic. `tests/http_middleware_contract.rs` was the same shape (F10 item 2) with **42** *active* `moira:admin` API keys. Both now use `support::TestDatabase`, whose `Drop` discards the whole database including while unwinding. Residue deleted by predicate; `audit_logs` residue (180 rows) left in place deliberately | hygiene |

*Reversal condition for F27:* it reopens if any test source outside `SHARED_DATABASE_ALLOWLIST` in
`tests/test_database_isolation.rs` resolves `MOIRA_TEST_DATABASE_URL` itself, or if either suite's
`the_fixture_owns_a_disposable_database` is deleted or weakened. The three allowlisted files
(`support/mod.rs`, `security_foundation.rs`, `retention_worker.rs`) are *not* covered — see F10 item 1,
which a private clone would also fix and which remains open.

**Finding IDs are being allocated concurrently and have collided three times.** `F22` names *two*
unrelated findings (`api_keys.prefix_length`, and the second `main` flake); `F21` has two entries; and
this work was written up as `F26` before `#47` merged claiming that number for admin-write/audit
atomicity — hence `F27`. **Check `origin/main`'s ledger for the highest ID immediately before
writing one down, not at the start of the task.**

**Closed in the final cycle:** F6, F13, F14, F17, F20, F21, F22, F23, F24, F25, **F26**, B2 —
**nine PRs, #39–#47**, each CI-verified with every job running steps.

Two closures corrected the finding that named them, which is the reason to re-derive rather than
implement from a one-liner:

- **F14's own suggested fix would have caused a leak.** It proposed moving `content_hash` to an
  unkeyed digest. `IdempotencyHasher::hash` feeds **four** tables, and
  `conversation_messages.content_hash` is **served to callers** in two OpenAPI schemas — unkeying it
  would let anyone holding the hash test candidate plaintexts offline. Applied **per table** instead:
  memory's becomes a content address, the message hash **stays peppered**, and a test pins that.
- **F26's one-liner named the wrong scope and understated reach.** Of the sites, **20 were already
  atomic** inside the command envelope and were never part of it; **36** were genuinely divergent.
  And it was reachable from a **request header**, not only a crash: `x-request-id` is taken verbatim
  and unbounded while `audit_logs.request_id` is `varchar(128)`, so a 129-character id fails the
  audit `INSERT` and nothing else — the change commits, the caller gets a 500, and the audit log is
  empty.

**Plan 11 Sub-Phases E (summarization) and F (memory extraction) remain deferred**, stated in its PR
rather than implied. F14 was Sub-Phase F's inherited obligation and is now **closed ahead of it**
(`74262ad`) — `memory_records.content_hash` is a content address, so the dedupe F will write does
not have to carry a rotation caveat. F's remaining work is unchanged otherwise.

### 3.3 Three things only the user can do

1. **Deploy the release containing `c98aeb7`, then land T11** — removing the console's
   `ambiguous_enabled_providers` guard. **Do not wave this through.** It is gated on stage 4A being
   *deployed*, not merged: until Moira's own refusal (`0020`'s partial unique index and coded 409) is
   running in production, that console guard is the only thing in front of **F23**. A rollout that
   lands the console before Moira reopens exactly the window 4A closed. Correct order is 4A in
   release N, T11 in release N+1.
2. **File the rig-core issue** for F16. Draftable, but it should go under a human's name.
3. **Supply a Google credential** if the OAuth mock/live seam ever needs closing. Everything is
   verified against a real TLS mock IdP with real signed JWTs — what cannot be proven without a
   credential is Google's own token claims, consent screen and key rotation. Recorded, not implied.
   The same now applies to **GitHub**, added in wave 4B and exercised only against a purpose-built
   mock (no discovery document, no `id_token`, `/user` + `/user/emails`).

### 3.4 Six guards that failed — five toothless, one that pinned the defect

**Read this before writing any guard.** Plan 09 produced **six**, every one found by *running the
mutation* and none by reading the test. **Two were already shipped and trusted.**

| Guard | Why it could not fire |
|---|---|
| wave 4A, policy ordering | migration `0020` made the target state **unrepresentable**, so a fixture of legal rows could not reach it |
| wave 4B, G9's minted-`sub` | the mutation creates a fresh account row with the **same** IdP subject — `sub` stays correct while every grant is orphaned |
| wave 5, `secret-leak.e2e.ts` | grepped for a **literal import path**; the real mount is transitive (page → organism → modal). **Verified green with the modal mounted** — an armed tripwire that could not fire |
| wave 5, route session guard | a **per-file** scan says "*some* handler is guarded", never "*every* handler" — and the second exported method is exactly where an unguarded endpoint goes |
| **F16's own mitigation** | its tests exercised a predicate **re-implemented in the test module**. Deleting the filter from `init` left **all 598 tests green** |

**A sixth, and it is a different failure — the test PINNED the defect.**
`console-jwks-stability.test.ts` asserted the published JWKS was *unchanged* by a secret rotation and
that signing raised the library's decrypt string. It went **red on the fix**, which is the opposite of
what a guard does. Worse, it asked the two questions **separately**, and F17 is the *conjunction*: a
200 JWKS is unremarkable alone, a signing failure is unremarkable alone — only together are they the
outage.

**Two more rules from that one:** when a test documents current behaviour rather than guarding a
property, **say so in its name**; and **never let a conjunction be asserted as two independent
facts.**

**The common shape:** each was written against the *shape the author imagined the defect would take*
— a direct import, one handler per file, a representable row, a changed subject, a correct predicate
— rather than against the property.

**Ask of every guard: what is the cheapest edit that breaks the property while leaving the guard
green?** That question found all five; reading the tests found none.

Three corollaries earned the hard way:

1. **A fix that makes a defect unrepresentable can silently disarm its own guard.** After any
   constraint that narrows what can exist, re-check the guard's fixture is still *reachable*.
2. **A source-scanning guard must scan the closure, not the file** — the transitive import graph, and
   per-export granularity.
3. **A predicate test and a wiring test are different tests.** Every laundering finding here has had
   a *correct predicate*.

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
- **Commits:** `git commit -m "…" --only -- <paths>`, never bare `git commit` — the index is
  shared. Commit **incrementally**; two stalls left real work uncommitted.
  **`-m` must come BEFORE the `--`.** Everything after `--` is a pathspec, so
  `git commit --only -- <paths> -m "…"` fails with *"did not match any file(s) known to git"* — and
  if you chained `&& git push`, the push prints **`Everything up-to-date`**. A failed commit followed
  by a reassuring push message is the eighth form of the §2.2 hazard: read the commit's own output,
  not the pair.
- **Stale worktrees** from this run can be pruned: `git worktree list`, then
  `git worktree remove <path>` for any whose branch is merged.

## 5. Compaction

This file plus the ledger must be enough to resume from one read. Before compacting, verify:
working tree clean and pushed; the ledger's "State at a glance" reflects reality (it did **not**,
once — two merged plans were missing); every decision since the last checkpoint is in a plan's §0
with its reversal condition; and running agents are named with their branch.

If any is false, **make it true first**. That is cheap; re-deriving lost state is not.
