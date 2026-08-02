# Handoff — Moira plan execution

**Point a fresh agent at this file.** It is written to be the *only* thing that needs reading before
work resumes. Read it, then read `plans/reports/EXECUTION-LEDGER.md` — the ledger is the source of
truth for state; this file is the source of truth for *how to work here*.

Written 2026-07-31. **All plan work is complete** — the forced order `02b → … → 09` is fully
executed. What remains is a short findings queue and **four** things only the user can do, both in §3.
Updated 2026-08-02: F29, F31, F35 and F37 closed; F36 and F41 **refuted rather than fixed**.

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

### 2.2 Exit codes lie here, in THIRTEEN observed forms — and form 4's cause is now known

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
11. **`gates.sh`'s own test phase looks like it is still compiling when it is not.** The script
    redirects `cargo test` to a `mktemp` file, so the outer log sits on `── test` for the whole
    phase and then shows *release-build* output — visually indistinguishable from a test phase
    still running. **Editing sources during that window silently splits the run:** `fmt` and
    `clippy` pass against the old tree while `release` builds the new one, and the reported test
    count belongs to neither. Found 2026-08-02 when a run reported a plausible number that was not
    that branch's. **Do not edit sources while `gates.sh` is running**; if you did, the run proves
    nothing about what you edited. Corollary: `grep "Running tests/"` on the *outer* log returns
    zero by design — the completeness assertion reads the temp log, and `ALL GATES PASSED` is the
    only trustworthy summary of it.
12. **`gh pr view --json statusCheckRollup` can report the PREVIOUS commit's verdict.** Right after
    a push it returned five `SUCCESS` checks for a commit that had just been superseded; the real
    checks for the new head were all pending. Found 2026-08-02, one command before an unwarranted
    merge. **Key the wait on the head SHA:** `gh pr view --json headRefOid -q .headRefOid`, then
    `gh api repos/{owner}/{repo}/commits/$SHA/check-runs`. Also note `.conclusion//"pending"` does
    **not** substitute for an empty string — queued checks carry `""`, not `null`, so a
    `grep -q pending` loop exits immediately on the first poll.

13. **A conflict-resolution script that fails, followed by `git add; git commit`, commits the
    markers.** Done 2026-08-03: a resolve script hit an assertion, and because the next commands were
    chained with `;` rather than `&&`, the unresolved file was staged, committed and **pushed** with
    `<<<<<<<` still in it. The commit output looked entirely normal. This is form 3's shape applied to
    a merge: *a failed step followed by a succeeding one reads as success.*
    **Two habits close it:** chain with `&&`, and make the resolver itself assert
    `zero markers remain` **before it writes**, so a wrong line number cannot silently produce a
    half-resolved file. Verifying afterwards with `grep -c '^<<<<<<<'` costs one command — and note
    it exits **1** on zero matches (form 2), so read the printed count, not `$?`.

**Redirect to a file, capture `$?` immediately, then read the file — and run cargo from inside a
script.** Use `scripts/gates.sh`, which handles all of this and asserts log completeness against
`ls tests/*.rs`.

**And never pipe the gate runner.** `scripts/gates.sh | tail -25` yields `tail`'s exit code (form 1
applied to the very tool built to defeat form 1). Redirect to a file instead. The content marker
`ALL GATES PASSED` is emitted only when the failures array is empty, so it is a sounder signal than
`$?` in every case where the two could disagree.

### 2.2a You may not be the only Claude session on this repo

**Discovered 2026-08-02 the expensive way.** A coordinator briefed its agent "you are the only agent
running gates". That was false: a *second Claude Code session* was running its own agents on the same
checkout. Load average peaked at **96**, and two gate runs failed on timing-sensitive concurrency
tests (`a_concurrent_summarization…`, `concurrent_key_create…`) with no relationship to the change
under test. A coordinator cannot see another session's agents in its own context — it can only see
their artifacts.

**Check for peers before claiming exclusivity, and before deleting anything:**

```bash
git worktree list                     # worktrees under a DIFFERENT session id are not yours
pgrep -f 'scripts/gates\.sh'          # a live gate run, whoever owns it
gh pr list --state open               # PRs you did not open
uptime                                # load >20 means you are not alone
```

**Never delete a target directory you did not create.** `scripts/reclaim.sh` L1 walks
`$TARGET_ROOT/*/debug/incremental` and would drop a *peer's* cache mid-build. Under contention,
reclaim only your own, and prefer waiting.

**`pgrep -fc 'cargo-targets/moira-<name>'` is a FALSE NEGATIVE.** `CARGO_TARGET_DIR` is an
*environment variable* — it never appears in a cargo process's argv, so this reports "clear" while a
peer's gate run is live. It is form 10's cousin: a peer check that always passes. Key on
`pgrep -f 'scripts/gates\.sh$'` or on a recorded PID.

### 2.2c Form 9 is broader than documented: the hook rewrites `grep` and `tail` too

The `rtk` `PreToolUse` hook does not only rewrite `cargo`. Observed 2026-08-02: a `grep` over the
ledger returned rtk's *summary* (`3496 matches in 589F`) instead of the matching lines, and a
`tail -3` was rewritten into `/usr/bin/read`.

So a search can silently return a digest of the answer rather than the answer — and a coordinator
reading that digest may conclude a symbol is absent when it is present, or miss the second of two
call sites. **Wrap `grep`/`tail` in a script file whenever the exact output matters**, the same way
`cargo` already must be. The immunity rule is unchanged: the hook only sees the outer command.

### 2.2b `scripts/gates.sh` CANNOT run concurrently with another gates run

Found 2026-08-01, after two runs sat **wedged for 40+ minutes**.

`sweep_leaked_databases` (`tests/support/mod.rs`) drops **any** `moira_test_template_*` other than its
own digest. Two runs with different migration sets therefore sweep each other in a loop, producing
`template database "…" does not exist` mid-run. Worse, it can **hard-deadlock**: one process holding
shared template locks for live fixtures blocks another's exclusive request, while its own next
fixture queues behind that exclusive request. Clear it with `pg_terminate_backend` on the idle lock
holders.

**The memory signal to watch is SWAP, not `vm_stat` free.** Measured 2026-08-02 with three
concurrent `cargo test --workspace` runs: `vm_stat` reported **13 MB free** while
`memory_pressure` said **34% free** — the free page list is not the constraint on macOS. What was
genuinely nearly exhausted was swap: **15.5 GB used of 16 GB, 903 MB left**. An earlier note here
recorded "~56 MB free" as the OOM threshold; that used the misleading metric. Check
`sysctl vm.swapusage` and `memory_pressure`, and treat swap above ~90% as the stop signal.

**Consequence for the loop: serialise gate runs.** Parallel agents are fine while they are reading,
designing, or editing — but only one may be in `scripts/gates.sh` at a time. Stagger them, or give an
agent its own database.

Three related traps from the same session:

- **`ps aux | grep` gives false negatives here — use `pgrep`.** An agent had *three* of its own gate
  runs stacked without seeing them.
- **…but `pgrep -f "gates.sh"` matches the waiting shell's OWN command line.** A wait loop spelled
  `until ! pgrep -f "gates.sh"; do sleep 20; done` contains the string `gates.sh`, so it matches
  itself and **blocks forever**. Four such loops were found spinning on 2026-08-01, long after the
  runs they were waiting on had finished. Anchor the pattern — `pgrep -f 'scripts/gates.sh$'` — or
  match the cargo process instead. This is the self-referential cousin of the whole §2.2 family: the
  check that reports on itself.
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

## 2.6 SUBAGENT SPAWNING WAS BROKEN ON 2026-08-01 — check before assuming a task is at fault

**Four consecutive agents died at their first action**, while the coordinator's own Bash, git and
`gh` calls kept working normally throughout. Symptoms:

| Attempt | Died doing | Failure |
|---|---|---|
| 1 | "study the Sub-Phase F implementation … in parallel" | `Connection closed mid-response` |
| 2 | "read the key files in parallel" | `Connection closed mid-response` |
| 3 | "read the main file and locate the definitions in parallel" | stalled, watchdog did not recover |
| 4 | "read the handoff rules first" — **sequentially** | stalled, watchdog did not recover |

**A hypothesis was tested and disproved.** The first three all announced *parallel* file reads, so
attempt 4 was briefed to read strictly one file at a time. It stalled anyway, on a single read. So
parallel tool calls are **not** the cause; record that so nobody re-tests it.

**Nothing was lost in any attempt** — none had written anything. That is the one thing that went
right, and only because the worktree was created before the agent was spawned.

**What to do when this recurs:**

1. **Verify it is the subagent layer, not the task.** Run a Bash and a `gh` call yourself. If those
   work while agents die at their first action, the task is not at fault and re-briefing it will not
   help.
2. **Stop after the second failure, not the fourth.** Three of these four were spent on the same
   task, and the fourth on a hypothesis that turned out wrong. The information was already in hand
   after two.
3. **Do not lower the bar to get something through** — a smaller brief that succeeds against a
   broken layer proves nothing, and a partial implementation of a security-adjacent wave is worse
   than none.
4. Record it here, stop the loop, and let a later session retry. The work is durable in git and this
   file; a burned cycle is not.

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
**152 operations / 100 paths / 183 schemas** — corrected 2026-08-02, re-derived from
`docs/openapi.json` itself. This line said 151/99/178 for two cycles after it stopped being true;
the ledger's cycle-14 entry already carried the correction and this one did not, which is how the
wrong numbers kept reaching briefs. **Derive them, do not copy them.**

### 3.2 Open findings

| | What | State |
|---|---|---|
| **F16** | `rig-core` logs the whole completion body, now carrying other tenants' retrieved documents. Mitigated below the `EnvFilter` — **and that mitigation's own wiring test was missing until `8bbda15`** | **proper fix is upstream; needs an issue filed by a human** |
| ~~**F2**~~ | ~~Pre-auth query-field enumeration~~ **CLOSED** `fix/f2-query-rejection-envelope`. Two corrections to the finding: it was never `Query`-only (every extractor rejection had the shape — `Json`'s 400/415/422, `Path`'s 400, `Extension`'s 500), and it was not an "observable wire change" — `docs/openapi.json` already documented `4XX`/`5XX` on these operations as `ErrorResponse`, so the fix makes the implementation obey a contract that was already committed and the snapshot is byte-identical. Scoped by **inverting the rule**: any non-JSON 4xx/5xx is envelope-wrapped, because `AppError` is the only producer of an error body. Rejection stays pre-auth, decided and recorded | closed |
| ~~**F28** (metrics gauge)~~ | ~~`metrics_endpoint_exposes_db_pool_gauges_reflecting_the_live_pool` treats sqlx's `num_idle()`/`size()` as exact and mutually consistent within one scrape~~ **CLOSED** `fix/shared-db-flakes`. **The diagnosis in the finding was wrong.** `num_idle()` is not approximate — since sqlx 0.6 it is a dedicated `AtomicUsize`, and 0.8.6's `size()` is another. What is asynchronous is the **return**: `Drop for PoolConnection` *spawns* a task, and that task issues a `ping()` round-trip **before** it re-queues the connection and increments `num_idle`. So a scrape landing in that window reads a pool that is still settling. Reproduced **8/10** by replaying the shape with the incidental delay removed. No bound and no poll were needed: `acquire` on every permit is a barrier (an in-flight return has not released its permit) and `PoolConnection::return_to_pool().await` runs the return eagerly, so `idle` is now pinned **exactly** at `capacity`, `capacity - 1` and `0` — strictly stronger than what it replaced. Also corrected two false claims in the test's own doc comment | closed |
| ~~**F10 item 1**~~ | ~~`tests/retention_worker.rs` asserts an exact delete count against a cluster-wide sweep, and seeds century-backdated rows with no cleanup path~~ **CLOSED** `fix/shared-db-flakes`. Both halves measured rather than argued: an injected failure leaked **43** rows dated **1926-08-27**, after which the *unmodified* suite failed `left: 43, right: 23` and leaked 23 more per run — 43 → 66 → 89 across three runs, permanently. Now on `support::TestDatabase`; the same injected failure leaves **0 rows and 0 databases**. The sweep is **database**-wide, not cluster-wide, so isolation does not change what is tested — it made the counts exactly assertable, and `>=` became `==` throughout. `SHARED_DATABASE_ALLOWLIST` is down to two entries | closed |
| ~~**F27**~~ | ~~Leaked `trusted_jwt_issuers` rows in the shared test DB~~ **CLOSED** `fix/test-row-leak`. **The recorded count was wrong**: it said ~986; the measurement was **160** — exactly ten rows (the ten `register_issuer` call sites in `tests/jwks_hardening.rs`) × sixteen runs, and it leaked them on the **happy** path, not only on a panic. `tests/http_middleware_contract.rs` was the same shape (F10 item 2) with **42** *active* `moira:admin` API keys. Both now use `support::TestDatabase`, whose `Drop` discards the whole database including while unwinding. Residue deleted by predicate; `audit_logs` residue (180 rows) left in place deliberately | hygiene |

**A concurrent branch `fix/test-row-leak-2` exists on origin** — a *reclaim* approach to the same
finding (delete the rows), superseded by `cac20ff` because reclaiming leaves the suite writing to the
shared database, so the next run leaks again. It was **left in place rather than deleted**: it is
another writer's work, and this loop does not destroy branches it did not create.

*Reversal condition for F27 and F10 item 1:* they reopen if any test source outside
`SHARED_DATABASE_ALLOWLIST` in `tests/test_database_isolation.rs` resolves
`MOIRA_TEST_DATABASE_URL` itself, or if any of the **three** suites'
`the_fixture_owns_a_disposable_database` tests is deleted or weakened. **No integration suite
writes to the shared database any more.** The allowlist is down to two entries — `support/mod.rs`,
which owns the mechanism, and `security_foundation.rs`, which must migrate a database built from
nothing — and both are read-scoped or self-cleaning. The shared `moira` database is still created
and migrated by the unit tests under `src/**/tests`, which is why it must keep existing.

*Reversal condition for F28 (metrics gauge):* it reopens if the pool gauges are asserted anywhere
against a value sampled from a pool that is not first brought to a known state, or if
`metrics_endpoint_exposes_db_pool_gauges_reflecting_the_live_pool` loses its **pre-saturation**
scrape. That scrape looks redundant and is not: every other observation saturates the pool, so
`pool.size()` and `max_connections` are the same number in all of them, and sourcing the `total`
gauge from the configured ceiling left the whole test green until it was added. **That survivor was
found by running the mutation — the test read fine.**

*Known limit, deliberately left:* `render_prometheus` does `u32::try_from(pool.num_idle())`, and
sqlx's `release()` re-queues the connection and releases its permit **before** incrementing
`num_idle`, so on a multi-threaded runtime a waiter can decrement first and wrap the `AtomicUsize`
to `usize::MAX` — which the `unwrap_or(u32::MAX)` then publishes as `4294967295`. `#[tokio::test]`
is current-thread, so no test here can observe it; production is multi-threaded. Not fixed, not
forgotten: the one-line guard is to clamp the gauge to `size()`.

**Finding IDs are being allocated concurrently and have collided three times.** `F22` names *two*
unrelated findings (`api_keys.prefix_length`, and the second `main` flake); `F21` has two entries; and
this work was written up as `F26` before `#47` merged claiming that number for admin-write/audit
atomicity — hence `F27`. **Check `origin/main`'s ledger for the highest ID immediately before
writing one down, not at the start of the task.**

**Closed in the final cycle:** F6, F13, F14, F17, F20, F21, F22, F23, F24, F25, **F26**, B2 —
**ten PRs, #39–#49**, each CI-verified with every job running steps.

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

### 3.2b THE LEDGER IS AHEAD OF THIS TABLE — read it, not just §3.2 (2026-08-02)

The table above was written when the highest finding was F28. **The ledger now runs to `F54`.**
`plans/reports/EXECUTION-LEDGER.md` is authoritative; §3.2 is a summary that has already gone stale
once. Current state of everything above F28:

| | What | State |
|---|---|---|
| **F33** | Five encryption-at-rest columns exist in `migrations/0007` and **nothing in `src/` writes or reads any of them** | **ESCALATED, human-only.** Envelope encryption is key custody, key rotation and plan 11's open Decision 3 — a scoping question, not an implementation gap. **Do not "finish" it autonomously** |
| **F32** | `conversation_content_persistence` protected nothing — `'none'` still stored full plaintext | fixed on `fix/f32-content-persistence`, **PR #57, deliberately NOT auto-merged**: the 422 on the previously-accepted `encrypted_content` breaks any IaC that sets it. Loudly, which is the point — but that deserves human sight |
| ~~**F46**~~ | ~~`response_format: {"type":"json_object"}` reaches the provider as a schema satisfied only by `{}`~~ | **CLOSED** `c938d5c` (#58) — **refused** with `422 unsupported_request_option` on both endpoints, matching F35's precedent on the compat path. Its recorded mechanism contained a false clause: `json_utils::merge` is only reached inside a branch requiring `output_schema.is_some()`, so a hand-built `additional_params` payload *would* have reached an OpenAI-family provider. It was refused on principle, not impossibility |
| ~~**F47**~~ | ~~`get_or_create_*_policy` are `insert … on conflict do update`, i.e. **reads that write**~~ | **CLOSED** `fix/f40-f47-response-output-and-policy-reads`. Finding right and **understated**: a "read" also **bumped `version`** (the `If-Match` ETag) and fired `pg_notify('moira_runtime_config')`, which makes every replica drop its runtime-config *and provider-handle* caches — three times per conversation-linked turn. Family is **five**, not two; the fifth (`get_or_create_application_execution_policy`) already read first and so had **no** write amplification — and no `on conflict` clause either, so it raced and returned a duplicate-key error on the hot path of every `POST /v1/responses`. The brief's warning that the row lock "makes that race impossible" was **inverted** for that member. Six mutations; the `select … for update` one left the first guard green and earned a fifth case. Raised **F51** |
| **F40** | ~~`GET /v1/responses/{id}` returns an empty `output` for a completed, persisted response~~ | **PREMISE REFUTED, two adjacent defects CLOSED**, same branch. `output_persisted` is never `true` anywhere, so `Completed` always explained itself and `[]` was reached only by non-completed statuses, where it is right. Real defects: the reason was the literal `"metadata_only_persistence"` for **all four** persistence modes, and `Completed && output_persisted` fell to `[]`. Public shape unchanged (`reason` is an unconstrained string; snapshot byte-identical) |
| ~~**F51**~~ | ~~The `moira_runtime_config` channel is attached to **`conversations` and `memory_records`**, and `apply_invalidation` calls three `invalidate_all()`s **unconditionally**~~ | **CLOSED** `fix/f51-f52-invalidation-scope`. **Premise held in full — every count in it was right**, including 24 triggers when counted by *function* and which table's trigger is named differently. **Both fixes taken**: `invalidation_plan` returns `{caches, circuits}` and narrows one-way (unknown payloads still clear everything), and migration `0022` drops both triggers. **Nothing depended on them notifying** — `docs/runtime-cache-invalidation.md` never listed them, no cache in the process is keyed by a conversation or memory record, and `db.rs` is the only listener. The doc comment's standing defence ("re-reading costs a query") was **true of two caches and false of the third**: `ProviderRuntimeCache` holds built Rig clients with connection pools. Five mutations; **the cheapest edit — honour the plan for `runtime_cache` only — reds only the handles assertion.** Raised **F53** |
| ~~**F52**~~ | ~~**A shipped, trusted guard whose list is retyped rather than pinned, and it has already drifted.**~~ | **CLOSED** `fix/f51-f52-invalidation-scope`. Premise held in full; `pg_trigger` returns exactly 24 and the three `legacy_*` tables still carry their **pre-rename trigger names**, so a name-based query mis-attributes as well as misses. `TRIGGERED_RESOURCE_TYPES` is now pinned against `pg_trigger` **both directions**, counted by trigger function, floored by `MINIMUM_TRIGGERED_TABLES`. The three legacy tables **lose their triggers** (`0023`) rather than being classified — the only references in the whole tree are inside `0003` itself, as a backfill source. Four mutations; **attaching the trigger to a new table reds the inventory test — the forward drift the retyped list could never detect — and "fixing" that red by adding the name to the constant alone reds the unit guard instead.** |
| ~~**F53**~~ | ~~**F51's class, one table over and at admin rate:** `rag_documents` and `rag_collections` are content, not configuration, both carry the notify trigger and both are `caches: true`~~ | **CLOSED** `fix/f53-f50-silent-degradation`. **The gating question was answered before the fix was chosen, and both tables have the same answer: no.** `rag_collections` carries no runtime configuration at all — the embedding model and dimension the entry guessed at live in `application_embedding_policies`, and the collection is joined only to reach `application_id`. The three caches are closed types (`HashMap<Uuid, ProviderConfig>`, one `Vec<PublicAuthMethod>`, and a map keyed by `RuntimeCacheKey`'s seven provider/model/credential/policy fields), so no RAG row can be in any of them. Both lose the trigger (`0024`) **and** move into `RUNTIME_DATA_RESOURCE_TYPES`. Five mutations; the two that matter — reverting the classifier for **one** table — red only the integration guard and **leave every unit test green**, because a guard that iterates a constant cannot see a name removed from it |
| ~~**F30**~~ | ~~`application_memory_policies.consent_mode` and `application_conversation_policies.memory_consent_mode` are independent, both default `'explicit_only'`, and nothing reconciles them~~ | **CLOSED, premise partly REFUTED** `fix/f30-consent-columns`. *Extraction* has reconciled them since Sub-Phase F and is tested with the columns **disagreeing** in both directions — as an extraction defect this is refuted. **But the reader it predicted had already arrived:** `ConversationRecord.memory_behavior` was `coalesce(mp.consent_mode, …)` **in SQL**, so `GET /api/v1/conversations` reported `application_managed` while extraction refused under a conversation policy of `disabled`. The lesson is a sharpening of *reconcile in one place*: the rule was in one place, in the **application layer**, and a query could not call it — it now lives on `MemoryConsentMode::stricter_of` so `pg_rows` can, and `conversation_select` decides nothing. `status_for_consent_mode` is private so no single-column answer has a ready-made caller. Four mutations; the one that matters restores the shipped defect and reds **only** the guard whose columns disagree |
| **F48** | **A third `output_schema` drop path, and it is silent even on OpenAI.** Rig also requires `tools.is_empty() \|\| history_has_tool_result`, so the schema is dropped on turn 1 of any tool-calling conversation with **no `warn!` at all** — the warning at that site fires only for the DeepSeek case. Latent only because `build_completion_request` hardcodes `tools: Vec::new()`; it goes live the day tool calling is enabled. **F39's fix does not cover it** — that reconciles per provider type, this drop is per request | **latent, now GUARDED, behaviour deliberately unchanged.** Premise re-verified: that constructor is the tree's only `CompletionRequest`, and `public.rs` refuses caller-declared tools outright. `moiras_request_still_carries_its_schema_onto_rigs_openai_wire_body` reds on the precondition |
| **F49** | **No integration test ever built a request from an agent profile** — every fixture left `agent_profile_id` NULL, so `preamble`/`temperature`/`max_tokens`/`tool_policy` were unverified at the wire | **CLOSED** `fix/f49-agent-profile-coverage`. Premise held; the column is on `route_definitions`, not `routing_policies`. `tests/agent_profile_wire.rs` now builds from a real profile and reads the mock's body. **The branch is correct at the wire.** Eight mutations run. Under F48's mutation only the new `tool_policy` case reds — **F48's guard is not superseded.** Raised F50 |
| **F50** | **A disabled or soft-deleted agent profile silently degrades every execution on its route** — `get_active_agent_profile` filters on `status='active'`, neither disable nor soft-delete clears the route's FK, and `Ok(None)` is treated as "no profile": preamble, temperature and max_tokens vanish, and the run reports `succeeded` | **OBSERVABLE; the product decision is STILL OPEN.** `fix/f53-f50-silent-degradation` ships a `warn!`, a `RuntimeEventType::AgentProfileUnavailable` runtime event and an `agent_profile.unavailable` audit row. **The request's behaviour is unchanged** — fail-closed vs fail-open is **not** decided here, because silence is a defect under either answer and the observability is the part they share, not half of one. "No profile" and "profile vanished" are distinguished at the call site by reading `route.agent_profile_id` *before* the lookup. *Reversal condition:* the decision makes this "observe and refuse" or leaves it as is; **the observability is not revisited.** The `documents_`-named case is now scoped to the fail-open behaviour alone, so the three observability guards survive either answer |
| **F38** | Terminal-persistence-deadline arm discarded a successful provider result | **CLOSED** `fix/f38-deadline-usage` — all three values retained, `"output_committed"` was a hardcoded literal and is now derived. Reversal conditions in the ledger |
| ~~**F45**~~ | ~~`PublicResponseFormat::JsonSchema { name, strict }` — both accepted, both dropped~~ | **CLOSED** `fix/f42-f45-declared-vs-true`. Premise held; **neither field is expressible in rig-core 0.40 on any provider** (`strict: true` hardcoded and unreachable via `additional_params`; `name` derived from the schema's `title`; Anthropic and Gemini have neither field). Resolved **asymmetrically**: `strict` **refused**, `name` **documented**. **PUBLIC CONTRACT CHANGE** — `strict` is now `Option<bool>` and an explicit `false` is `422` on both endpoints; omitted and `true` are unchanged. That is what makes refusing available at all, and F35 was right to decline it while the field was a defaulting `bool`. `name` cannot be refused (required field) and is not smuggled through `title`. OpenAPI counts **hand-verified, unchanged: 152 / 100 / 183** |
| ~~**F42**~~ | ~~`moira.error.structured_output_invalid` asserts a model-output-non-conformance path that does not exist~~ | **CLOSED** same branch. Premise held: two emitters, both rejecting the *caller's schema*. The near-miss that made it plausible — `memory_extraction::FAILURE_STRUCTURED_OUTPUT_INVALID`, the same string for the missing case — is never returned to a caller, and the description now says so. Fail-hard variant deliberately **not** shipped; F29 still needs two of its three preconditions |
| ~~**F43**~~ | ~~`ConcurrencyController::acquire` is dead `pub` API; every caller is inside `#[cfg(test)]`~~ | **CLOSED** same branch. **Conclusion refuted, hazard confirmed.** 9 of the 29 callers are in `tests/`, a separate crate, so private/deleted were never available — `pub` in this `publish = false` single-crate workspace means "visible to integration tests", not an external contract. Fixed by removing the *choice*: one `pub acquire`, `is_stream` mandatory, wrapper gone |
| ~~**F44**~~ | ~~`RuntimeModelHandle::stream` / `RuntimeStreamOutput` are dead `pub` API~~ | **CLOSED** same branch. Premise held exactly; **103 lines deleted**. Not in the finding: that cluster was the sole reason `runtime_factory.rs` imported the runtime-event vocabulary, so deleting it restored the Rig/application boundary |
| **F35, F37, F34, F29, F39, F46** | — | CLOSED |
| **F36, F41** | — | REFUTED / wrong as recorded |

**F39 closed 2026-08-02** (`fix/f39-structured-output-capability`). Both divergences verified true,
and fixed **asymmetrically because they are not the same problem**: DeepSeek is decidable — Moira
now reads Rig's own `SUPPORTS_RESPONSE_FORMAT` associated const rather than restating it, so a
`rig-core` bump cannot silently rot the answer — while `OpenAiCompatible`/`Local` is **undecidable
at admission** (Rig does send the schema; whether a self-hosted backend honours it is unknowable)
and is deliberately still admitted. Unblocked **one of the three** preconditions F29's reversal
condition names — see the ledger's F39 section. **The other two landed on
`fix/f30-consent-columns` (2026-08-03), so the reversal condition now holds in full and the flip is
a choice rather than a wait.** `StructuredOutputInvalid`'s disposition is *stay out of all three*
sets, recorded at each function and guarded bidirectionally; `run_extraction` reads
`execution.status` and records the execution's own failure class. **The lenient parse still stays**
— the flip turns a silent `None` into a terminal 422 on a class that neither retries nor falls
back, and that blast radius deserves its own diff. What it must do is written at
`structured_output_from_text`.

**Finding IDs are allocated concurrently and have collided three times.** `F22` and `F28` each name
**two** unrelated findings; `F21` has two entries; F27 was written as F26 until #47 claimed it.
**Read `origin/main`'s ledger for the highest ID immediately before writing one down — not at the
start of your task.**

### 3.3 Things only the user can do

1. **Deploy the release containing `c98aeb7`, then land T11** — removing the console's
   `ambiguous_enabled_providers` guard. **Do not wave this through.** It is gated on stage 4A being
   *deployed*, not merged: until Moira's own refusal (`0020`'s partial unique index and coded 409) is
   running in production, that console guard is the only thing in front of **F23**. A rollout that
   lands the console before Moira reopens exactly the window 4A closed. Correct order is 4A in
   release N, T11 in release N+1.
2. **File the rig-core issue** for F16. Draftable, but it should go under a human's name.
3. **Decide F33's scope** — the five encryption-at-rest columns. Nothing in the tree encrypts, and
   envelope encryption is key custody plus key rotation plus plan 11's open Decision 3. Recorded so
   the columns are not mistaken for a partially-built feature by whoever finds them next.
4. **Review PR #57 (F32)** — it is complete and gated but deliberately unmerged, because refusing the
   previously-accepted `encrypted_content` with a 422 will break any deployment setting it in IaC.
5. **Supply a Google credential** if the OAuth mock/live seam ever needs closing. Everything is
   verified against a real TLS mock IdP with real signed JWTs — what cannot be proven without a
   credential is Google's own token claims, consent screen and key rotation. Recorded, not implied.
   The same now applies to **GitHub**, added in wave 4B and exercised only against a purpose-built
   mock (no discovery document, no `id_token`, `/user` + `/user/emails`).

### 3.4 Thirteen guards that failed — eleven toothless, two that pinned the defect

**Read this before writing any guard.** Plan 09 produced **six**, every one found by *running the
mutation* and none by reading the test. **Two were already shipped and trusted.** A seventh followed
on 2026-08-02, in a *replacement* guard written by an agent who had read this section first. A
**tenth** followed later the same day, in a *brand-new* guard written by an agent who had read this
section first and had already asked its question — see the end of this section.

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

**A seventh, and it is the one to be least comfortable about: the fix for a toothless assertion was
itself toothless, in a new place.** F28's replacement drives the connection pool to saturation to
make its numbers exact. That works — and it means `pool.size()` and the configured
`max_connections` are *the same number in every observation the test makes*. Sourcing the `total`
gauge from `options().get_max_connections()` instead of the live size therefore left the whole test
green; verified by running it. One scrape taken **before** the pool is saturated closes it.

**The lesson is narrower than "test more".** The technique that bought determinism — pin the system
to a known extreme — is the same technique that collapsed two distinct quantities into one. *Any
time a guard reaches a known state to make an assertion exact, ask which variables that state has
just made indistinguishable from each other.* Determinism and discrimination pull in opposite
directions, and this one was noticed only because the mutation was run anyway.

**An eighth and a ninth, from `fix/f38-deadline-usage` — both already shipped and trusted.** That
brings the shipped-and-trusted count to **four**, and an eleventh below takes it to **five**.

| Guard | Why it could not fire |
|---|---|
| all seven cases in `tests/structured_output.rs` | they read `response_format` off the body that actually reached a mock provider, which looks unassailable — but every fixture in the tree left `route_definitions.agent_profile_id` NULL (**not** `routing_policies`, which has no such column — the original wording of this row and of F48's doc comment were both wrong, corrected under F49). The realistic way to enable tool calling is to read `AgentProfileRecord::tool_policy`, and **that mutation left all seven green**. Recorded as **F49**: no end-to-end test had ever built a request from an agent profile, so `preamble`, `temperature` and `max_tokens` were equally unpinned. **F49 is now CLOSED** — `tests/agent_profile_wire.rs` builds from a real profile — and the seven cases here are *still* green under that mutation. Closing the fixture hole did not arm them, and was never going to: they send no `output_schema` on the profile-carrying path, so they cannot see the drop |
| `terminal_persistence_timeout_is_recorded_as_output_committed_not_as_a_plain_failure` | **PINNED the defect**, the second of that kind. It asserted `"output_committed": true` in both the event and the audit row on a **non-streaming** execution — and asserted `usage_records` count `= 0` a few lines away, which is the proof nothing was committed in either sense. The literal in the *test* was the thing keeping the wrong literal in the *code* |

The lesson from the first: **"it asserts on the real wire" is not the same as "it reaches the code
you changed."** Before trusting an end-to-end guard, check that its fixture populates the input
your edit reads. The lesson from the second is §3.4's existing one, now twice-earned: a content
literal in a test is code, and it can pin a defect just as firmly as an assertion can guard one.

**A tenth, from `fix/f40-f47-response-output-and-policy-reads` — caught before merge, and a shape
not yet on this list: the fix removed two coupled things and the guard observed only one.**

F47's `on conflict do update` was both **a write** and **a row lock**. The guard asserted on the
write, three ways — `xmin`, `version`, and the absence of a `moira_runtime_config` notification —
which felt exhaustive because those are three independent observations of the same event. They
are three observations of *one* of the two things being removed. Adding `for update` to the
replacement `select` restores the serialisation with **no** new tuple version, **no** `version`
bump and **no** notification, and left every one of those assertions green. Verified by running
it, after the question had already been asked and the guard judged sound — the same sequence as
the seventh.

**The rule this adds: when a fix removes a construct, enumerate everything that construct was
doing, and check the guard covers each one separately.** Three assertions on one consequence are
one assertion. `do update` was doing two jobs; the guard needed a case per job, and
`a_policy_read_does_not_wait_for_a_row_lock` is the second.

A corollary that also came out of that suite, and is cheap everywhere: **an assertion that
nothing happened is worthless unless something *can* happen.** The no-notification case now
performs a real write on the same listener immediately afterwards and requires *that* to be
announced. Without it the assertion would have held equally against a database with the triggers
dropped or a listener on the wrong channel — F16's shape, in a new place.

**An eleventh, found the same day and the worst of the shipped-and-trusted set, because it is a
guard whose entire job is drift detection and it had already drifted. Recorded as F52, and
since FIXED on `fix/f51-f52-invalidation-scope` — see the end of this entry for what the fix
had to prove, which is more than "derive the list".**

`every_triggered_table_has_a_scope` in `src/infra/db.rs` proves every table wired to
`moira_runtime_config` classifies to something other than `CircuitResetScope::All` — an
unclassified table means a write to it discards every provider's earned breaker health on every
replica. Its doc comment says: *"the list is pinned here against the trigger list in
`migrations/`."*

**It is not pinned. It is retyped** — 21 table names as a Rust array literal, checked against a
schema that has **24**. The three it omits are `legacy_providers`, `legacy_routing_policies` and
`legacy_provider_credentials`, created by migration `0003`'s
`alter table providers rename to legacy_providers`, which **carries the trigger with it**. All
three fall to the classifier's `other =>` arm. The guard passes, and has always passed, because
it is comparing the classifier to a copy of the classifier.

**The rule: a guard that pins X against a hand-written list of X is not a guard, it is a
duplicate.** The inventory must come from an independent source — `pg_trigger`,
`information_schema`, `read_dir`, the generated document — or the guard only proves the author
typed the same thing twice. This project already gets it right in three places
(`suites_opening_the_shared_database` scans the filesystem, `if_match_inventory` reads the
generated OpenAPI, and `MINIMUM_SUITES_SCANNED` floors the scan against an empty set), and
`tests/policy_reads_do_not_write.rs` was given the same treatment on this branch for exactly this
reason — `POLICY_TABLES` is pinned against `information_schema`, so a sixth policy table reds it.

Note the shape it shares with the seventh and tenth: **all three were sound at the moment they
were written and were falsified by a later change elsewhere** — a migration that renamed a table,
a fix that removed two coupled behaviours. Guards rot in the direction of passing.

**What fixing it taught, and it is not "derive the list".** `TRIGGERED_RESOURCE_TYPES` is now
pinned against `pg_trigger` in both directions. That alone is *still* not a guard, because there
is an obvious lazy repair for the red it produces: someone attaches a trigger to a new table, the
inventory test fails naming that table, and they add the name to the constant. Set equality is
restored, the test is green, and **the table is still unclassified** — the original defect,
reintroduced by the fix's own error message. What closes it is that a *second* guard iterates the
same constant and asserts each name classifies: adding the name satisfies the first and reds the
second, and classifying it wrongly (as per-request data, so `caches: false`) reds a third
assertion. **A derived inventory is only a guard if something else consumes it.** Verified by
running all three edits, and the lazy-repair one was not predicted — it was found by asking the
§3.4 question of the finished fix.

**Two other things worth carrying, both from F51 on the same branch.**

*When a fix protects several things, seed and observe the most expensive one, not the easiest
to construct.* `apply_invalidation` clears three caches. Two rebuild from a query; the third
holds built provider clients and their connection pools, and is the reason the finding mattered.
The test file already contained a ready-made sentinel for the cheap one — so the guard that
wrote itself would have watched exactly the wrong cache, and stayed green against an edit that
honoured the plan for it and kept wiping the other two. That edit was run; it reds only the
handles assertion.

*A barrier must be inert with respect to the property under test.* The ordering barrier already
in `tests/runtime_config_invalidation.rs` establishes "the listener has caught up" by emitting a
`provider_models` notification — which is configuration, and therefore clears the caches. Reusing
it to bracket a cache-*survival* assertion made the guard fail for a reason that had nothing to do
with the code. It now brackets on the invalidation counter, which moves on every notification
including the ones that clear nothing.

**A twelfth, from `fix/f42-f45-declared-vs-true`, and its shape is new to this list: the suite
stated the right principle in its own header and then applied it to half its cases.**

`tests/structured_output.rs` opens by arguing that `execute_rig_stream` is *"a genuinely separate
code path, and a fix applied at the Rig boundary would cover only case 1"*. That argument is
correct, and it produced case 2 — the streaming twin of the **conforming** reply. It was never
applied to the **non-conforming** one. So the cheapest edit that falsifies F42's corrected catalog
entry — add the fail-hard variant to the streaming arm only — left **all seven** existing cases
green: case 2 sends conforming JSON and never reaches the branch, and case 4 never streams.
Verified by running it; the new twin is then the only case that reds.

**The rule this adds: when a suite justifies a case by "this is a separate path", that
justification applies to every property the suite tests on the other path, not just the one that
prompted it.** A per-path pairing is a matrix, and this one had a hole in it that the header's own
reasoning would have filled. It is cheap to check — list the properties, list the paths, look for
the empty cell — and nothing else in the suite could have found it.

**A thirteenth, from `fix/f53-f50-silent-degradation`, and it is the sharpening F52 needed: a
guard that iterates a constant cannot see a name being REMOVED from that constant.**

F52's rule is *a derived inventory is only a guard if something else consumes it*, and the
consumer it produced — `only_configuration_changes_invalidate_the_configuration_caches` — loops
over `RUNTIME_DATA_RESOURCE_TYPES` asserting each name classifies to `caches: false`. That is a
real guard against a name being *added* wrongly. It is **structurally blind** to a name being
taken out: the cheapest edit that reintroduces half of F53 is to move `rag_documents` back to
`CIRCUIT_UNAFFECTED_RESOURCE_TYPES`, and **all eight `infra::db` unit tests stayed green** through
it, because the loop no longer had that name to iterate. Only the integration guard, which names
the table literally, reds. Verified by running it, for each of the two tables separately.

**The rule: set-membership guards are one-directional by construction.** If a constant's contents
are the property, something must also assert the *behaviour* of each specific member by name —
otherwise the guard covers additions and silently permits deletions, which is the direction a
regression actually travels. This is the same shape as the tenth entry (one construct, two jobs,
guard covers one) applied to a list rather than a statement.

**A second thing from the same branch, about `documents_` versus `guards_` when a fix is
deliberately partial.** F50's coordinator decision was "ship the observability, do not take the
product decision". The existing case was named
`documents_current_behaviour_a_disabled_agent_profile_is_silently_ignored`, and shipping made its
name false without making it fail. The temptation is to widen it into one case asserting both
"it is announced" and "it still succeeds". **Do not** — the first survives whichever way the
product decision goes and the second does not, so a merged case would go red on a correct
fail-closed fix. Split by *lifetime*: what the pending decision cannot change is a `guards_`, what
it will change is a `documents_` with the reversal condition on it. A `documents_` case is a
liability with an expiry date, and the expiry date belongs in its name's scope.

*Two things from the same branch that are not new failures but are worth carrying.* The F43 fix
was guarded by a test that **already existed and did have teeth** —
`stream_capacity_is_independent_from_request_capacity` reds alone when the streaming flag stops
being wired — and it works precisely because it sets `max_concurrent_requests: 2` against
`max_concurrent_streams: 1`. **Three other fixtures in the same file set both to 1** and could
never have distinguished the two ceilings: the seventh entry's lesson (a state reached for
determinism can collapse two quantities into one) sitting in the tree in triplicate, with one
counter-example that happens to be the one that matters. And for F45, the *only* thing that caught
the cheapest edit — collapsing an omitted `strict` back into an explicit `false` at the compat
translation — was a **control** case asserting that the omitted spelling still succeeds. Every
primary refusal assertion stayed green through it. Controls are not padding.

**A sharpening about *barriers*, from F30 — it is not a fourteenth guard, it is the thing guards
are supposed to be protecting.** The standing advice for "two inputs that must be read together" is
*reconcile in one place in code that every reader must go through*. F30 had that: the
stricter-of-two-consent-columns rule lived in exactly one function. A second reader appeared anyway,
in **SQL**, and got it wrong — because the one place was an application-layer function over a type
(`Option<MemoryStatus>`) that the layer needing the answer could neither call nor use. **A single
point of truth is only a barrier if it is reachable from every layer that needs the answer, in the
type that layer needs.** Otherwise the next reader is not being disciplined; it is being locked out,
and it will reimplement.

Two corollaries, both cheap:

- **Ask which layers need the answer before choosing where the rule lives.** F30's fix was to move
  the rule *down* to the domain type, which let `src/infra` apply it and let the query go back to
  selecting raw columns and deciding nothing. Moving it up would have had the same failure.
- **Delete the convenient wrong answer.** `status_for_consent_mode` turned *one* of the two columns
  into a decision and was `pub`. Making it private is not tidying — it is removing the autocomplete
  that any future single-column reader would have reached for first.

---

**The original six — five toothless, one that pinned the defect**

**The common shape:** each was written against the *shape the author imagined the defect would take*
— a direct import, one handler per file, a representable row, a changed subject, a correct
predicate, a saturated pool — rather than against the property.

**Ask of every guard: what is the cheapest edit that breaks the property while leaving the guard
green?** That question found all six; reading the tests found none. **Asking it is not enough — the
seventh was found by *running* the answer**, after the same author had already asked the question
and judged the guard sound.

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
  `main + N × ~2 GB` and grows with every agent. Below 60 GB free run `scripts/reclaim.sh`; below
  30 GB also delete finished agents' target dirs. **Delete them routinely, not only under pressure.**
  `debug = 1` took a full build from 20 GB to 2 GB and a cold rebuild to 2m21s.
- **Migrations** are append-only; next free number is **`0025`**. This line said `0020` while the
  tree was at `0023` — **derive it (`ls migrations/ | tail -1`) instead of trusting it**, exactly as
  the brief for F53 instructed. `0016` is a permanent gap.
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
