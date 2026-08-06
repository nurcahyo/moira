# Execution ledger

Durable state for the continuous plan-execution loop. **Read this first on every wake** — it, not
recollection, is the source of truth for where the work stands. Update it at the end of every cycle
in which anything changed, and commit it.

Working agreement in force (granted 2026-07-26, **extended 2026-07-30**): full autonomy including
merge, self-paced cadence. Security findings escalate to the user immediately.

## STANDING AUTHORITY — unattended execution (granted 2026-07-30)

**Run to completion without stopping for input.** The user is away. These rules replace asking:

1. **Do not ask questions. Decide, and record the decision.** Where a question would have been
   asked, research the tree first, pick the recommended option, and write it into the affected
   plan's §0 with its reasoning and — this part is not optional — **the condition that would
   reverse it**. A decision recorded that way is reviewable after the fact; a question asked into
   an empty room just stalls.
2. **Parallel vs series is the loop's call.** Rule of thumb established by measurement: agents get
   private `CARGO_TARGET_DIR`s and run parallel when they touch disjoint trees (Rust vs console),
   series when they share files. Cargo takes an *exclusive* target-dir lock, so agents sharing one
   directory serialise no matter how many are spawned.

   **A private target dir is NOT isolation. A private WORKTREE is.** Learned the hard way on
   2026-07-31: plan 11's agent was working in the main checkout, and the coordinator ran
   `git checkout main` in that same tree to commit an unrelated ledger entry. The agent's work then
   committed onto `main` instead of its branch. It behaved correctly — it detected the drift, refused
   to `reset --hard` a branch another writer was active on, and left a recovery recipe. The
   coordinator caused the problem.

   **Rule: any agent that will commit gets its own `git worktree`, not just its own target dir.** The
   target dir prevents *build* contention; only a worktree prevents *branch* contention. And the
   coordinator must never run `git checkout` in a tree an agent is using — do ledger commits from a
   separate worktree, or wait.

   Recovery when it happens anyway: if the stray commit is unpushed, `cherry-pick` it onto the right
   branch and `reset --hard origin/<branch>`. Never force-push to fix this while another agent is
   live.
3. **Merge on green CI.** Five checks green with steps executed → merge: **`rust`**, `supply-chain`,
   `container-and-helm`, `console`, `console-container-and-helm`. A job that ran and failed is real
   and blocks; investigate rather than override. The old infrastructure override is **void** — CI
   works.

   This sentence used to read "all three jobs green" and then named jobs that no longer exist. The
   Rust half is now sharded across `rust-lint`, `rust-shard (0…4)` and `rust-migrations`; **`rust`**
   is the aggregator that `needs:` all three, asserts each one's `result` is `success`, and then
   proves the union of what the shards actually ran equals every target in the tree. It is the only
   Rust check a human or a branch-protection rule should look at.

   **Never gate on `rust-shard (0)`…`(4)` individually.** Matrix check names embed the shard index
   and change every time `SHARD_TOTAL` is re-tuned, which silently un-gates the branch — the same
   shape as every other guard in HANDOFF §3 that went quiet without going red.

   One signal changed meaning with the sharding: `test:incomplete-log`. `cargo test` without
   `--no-fail-fast` stops scheduling targets after the first failure, so incomplete-log used to
   double as a failure symptom. The shards pass `--no-fail-fast` deliberately, which breaks that
   coupling — completeness is now orthogonal to pass/fail, and one bug produces one red instead of
   three. Older entries about that signal predate the change.
4. **PR #23 is no longer HELD** — the user chose to branch plan 08 from it, so it lands as part of
   plan 08 rather than on its own.
5. **OAuth / Google credentials: mock first.** No real Google client id or secret is available and
   none is to be requested. Build against a mock IdP and a mock token issuer, with the seam drawn so
   a real provider drops in later without reshaping the code. Anything genuinely requiring live
   credentials is deferred with an explicit note, never faked into a green test.
6. **Never fake a gate.** A skipped DB suite reporting green, an `--ignore-unfixed` added to silence
   Trivy, a weakened assertion — all forbidden. If something cannot pass honestly, stop that thread,
   record why, and move to the next.
7. **Escalation still applies** for security findings, data-loss risks, and anything that would
   need a credential or a spend decision. Escalate by writing it here and in the PR body, and keep
   working — do not block on it.
8. **Watch the disk every cycle. This is unattended — nobody is going to notice a full disk.**
   Check `df -g .` **and** `du -sh ~/.cargo-targets/*` at the top of each cycle, not just the main
   `target/`. Agents now hold *private* target dirs, so total usage is `main + N × ~2 GB` and grows
   with every agent spawned — the number that mattered historically was never one directory.
   - **Below 60 GB free:** run `scripts/reclaim.sh`, which escalates from the free-to-delete
     `debug/incremental` cache upward and refuses to run while a build is live.
   - **Below 30 GB free:** also delete `~/.cargo-targets/*` for agents that have finished. They
     rebuild in ~2m21s; a stalled overnight run costs far more.
   - **Delete finished agents' target dirs as a matter of routine**, not only under pressure.
   - `cargo clean` is no longer catastrophic — `debug = 1` took a full build from 20 GB to **2.0 GB**
     and a cold rebuild to **2m21s** — but still prefer the graduated script, because `deps` is the
     expensive half and `incremental` is ~45% of the tree and free to drop.
   - **Never delete a target dir while a build is running.** The script refuses; do the same by hand.

**Plan order remaining:** `{08 ∥ 10} → 11 → 09`.

---

## ⚠️ CORRECTION (2026-07-27) — CI IS NO LONGER DOWN. THE OVERRIDE'S PREMISE IS VOID.

**Everything in the section below is stale and must not be relied on.** It is kept because the
merge override was granted on its basis and the record should show what that basis was.

`gh run view 30192737818` on **`main`**, 2026-07-26:

```
rust                success  steps=13
supply-chain        failure  steps=10
container-and-helm  failure  steps=1
```

Jobs **run now**, for 5–6 minutes, with steps executing. The `rust` job — fmt, clippy, tests, build
— **passes on `main`**. The 2-second/0-step signature is gone.

That inverts the override. Its load-bearing condition was *"a job that never started tells you
nothing; a job that ran and failed is real and blocks the merge."* Both failures now **ran**:

1. **`supply-chain` — a real finding, now fixed** (`b011ae3`). The job runs `cargo audit` **and**
   `cargo deny check`. **`cargo audit` was never one of the five local gates**, so "all five gates
   green" never covered half of what this job checks. It was failing on RUSTSEC-2023-0071 (Marvin
   timing sidechannel in `rsa` 0.9.10, severity 5.9, no fixed upgrade). **Inapplicable here** —
   `rsa` is not in the build graph on any target (`cargo tree --invert rsa --target all` prints
   nothing); it is in `Cargo.lock` only because `sqlx` records its unused MySQL backend. That is why
   `cargo deny` reported clean on the same tree. Closed with a documented `.cargo/audit.toml`.
2. **`container-and-helm` — infrastructure rot, not code.** `Unable to resolve action
   aquasecurity/trivy-action@0.28.0` (version does not exist) and `yannh/kubeconform-action`
   (repository not found). Pinned actions that no longer resolve. **Still open** — needs the pins
   updated to versions that exist.

**The gate list is now SIX, not five.** `cargo fmt --check`, `cargo clippy`, `cargo test`,
`cargo build --release --locked`, `cargo deny check`, **`cargo audit`**. Any claim of "all gates
green" that omits `cargo audit` is incomplete — that is exactly how this went unnoticed.

---

## STANDING RISK — GitHub Actions is failing repo-wide *(SUPERSEDED — see the correction above)*

**Status: unresolved. Merging proceeds under an explicit user-granted override; see below.**

**`main` currently has no working CI.** Every plan merged until the Actions billing state is fixed
lands verified by local gates only. That risk accumulates silently with each merge — the longer it
runs, the more likely it hides a real regression that CI would have caught on a different platform,
a clean checkout, or a fresh database. Worth resolving before it does.

The user authorised merging past this red on 2026-07-26. The override and its four required
conditions are specified in `HANDOFF-PROMPT.md` §6.5 — **read them before using it**. The load-bearing
distinction: a job that *ran and failed* is real and blocks the merge; a job that never started
(`steps: 0`) tells you nothing. If you cannot tell which you are looking at, treat it as real.

Each use of the override must be recorded in the cycle log with its run ID and zero-step evidence,
and stated on the PR, so no reader mistakes these merges for CI-verified ones.

Every workflow run on this repository fails **2 seconds after start with 0 steps executed**:

```
supply-chain        failure  started 17:22:22  completed 17:22:24  steps: 0
container-and-helm  failure  started 17:22:22  completed 17:22:24  steps: 0
rust                failure  started 17:22:22  completed 17:22:24  steps: 0
```

`gh run list` shows the same on **`main`** and on `plan/04-durability-correctness` — i.e. this
predates PR #27 and predates plan 05. Plan 04 was merged into an already-red CI.

Zero steps executed means the jobs never began work: this is account/runner infrastructure, not a
compile or test failure. `actions/permissions` reports `enabled: true, allowed_actions: all`, so the
most probable cause is an **Actions spending limit or billing state**, which only the repository
owner can resolve.

**Consequence for the loop:** the merge precondition — *all five gates green on the branch* — cannot
be satisfied through CI while this holds. Merging anyway would mean merging on local runs alone,
which is exactly the "a gate you did not run is not a gate that passed" trap. **Do not merge PR #27
until CI is restored, or the user explicitly authorises merging on local-gate evidence.**

---

## Plan 05 — observability & CI/supply-chain gates

**Status: MERGED to `main` as `3ea8037` (2026-07-26).**

Merged under the infrastructure override, **not CI-verified** — run `30167563082` showed all three
jobs failing with `steps: 0` in three seconds, the same failure present on `main` and
`plan/04-durability-correctness`. All five gates were re-run and verified green on the exact merge
commit `5e7335a`, with DB-backed suites confirmed running rather than skipping (`retention_worker`:
6 passed, 0 skips). Disclosed in a PR comment.

- PR: **#27** — https://github.com/nurcahyo/moira/pull/27 (MERGED)
- Branch: `plan/05-observability-ci-gates`, pushed, 6 commits
  `9531b90` impl · `cb41b6a` E2E layer · `0b79502` leak revert · `370c94b` mask fix ·
  `895c463` execution span · `c40af24` missing DoD tests

**Gates — all five verified locally, 2026-07-26:**

| Gate | Result |
|---|---|
| `cargo fmt --check` | clean |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | clean |
| `cargo test --workspace --all-features` | pass (DB-backed suites confirmed running, not skipping) |
| `cargo build --release --locked` | Finished in 2m58s |
| `cargo deny check` | advisories ok, bans ok, licenses ok, sources ok |

**Manual DoD items, both performed:** live OTLP capture (both `http_request` and `execution_attempt`
spans observed against a real vLLM execution; span-attribute denylist scan clean) and the
supply-chain seeded-violation teeth check. Evidence bundle in the session scratchpad
(`otel-capture-transcript.txt`, `metrics-before-after-diff.md`, `gates.log`).

**Two security defects found and fixed on this branch — neither ever shipped:**
1. `mask_plain_secret` returned the entire plaintext for secrets of ≤4 characters, and the result is
   persisted to `credentials.masked_secret` and served by admin read APIs (`370c94b`).
2. Two QA probes injected into `src/application/admin.rs` wrote system-key and credential plaintext
   into `audit_logs.metadata`, which the admin audit API serialises verbatim (`0b79502`).

**Method note worth carrying forward:** plan 05 was twice declared "complete and green" and was twice
neither. The first miss was the whole E2E layer; the second was 5 DoD tests plus a span that did not
exist, found only by checking **all 49** DoD-named tests instead of a sample.

---

## Plan 06 — architecture & test hygiene

**Status: MERGED to `main` as `39c5326` (2026-07-26), with `06b` `46c2c74` and `06c` `627fe4d`.**
The narrative below is kept as the record of how it was executed; see the cycle log for outcomes.

- `01107bb` — Wave 0: plan rewritten against the real tree. §0 of the plan document carries a
  14-item drift table. Two corrections were load-bearing: Module 9 as written **did not compile**
  (it deleted `resolver.rs`, which defines the load-bearing `RuntimeConfigCache`), and Modules 7/9
  targeted `executor.rs` as the Rig boundary when that file is dead code and the boundary is
  `runtime_factory.rs`. Two omitted items written in as Modules 16 (`actor_fingerprint`) and 17
  (If-Match inventory).
- `6b335a9` — **Module 0**, the gating commit: `GeneratedApiKey.raw_key` is `SecretString`, so
  `json!({"raw": ...})` is a compile error. Proven, not asserted: re-introducing the exact leak line
  from `9531b90` fails with `Secret<String>: serde::Serialize is not satisfied`. The guarantee rests
  on the `SerializableSecret` marker, which `String` never implements — so it survives feature
  unification, which is stronger than the "no serde feature" reasoning first assumed.

**Decision taken: If-Match TOCTOU splits into plan 06b, sequenced `06 → 06b → 07`.** It cannot land
in 06 because Module 6's DoD requires `AdminService`'s 46 signatures provably unchanged while the fix
adds `expected_version` to 21 of them — both claims cannot stand. It must precede 07 or
`AdminIdentityService` inherits the pattern. Module 17 therefore delivers inventory + recipe +
an `#[ignore]`d failing harness only.

**Wave 1 in flight** (3 agents, disjoint files; Modules 8 and 14 deliberately held back because all
agents share one `./target` lock and extra parallelism buys queueing, not speed):
- A — Modules 1–6, the `admin.rs` split. Long pole. Guarded by a before/after signature snapshot.
- B — Module 7, `DomainMessage` + Rig boundary, retargeted to `runtime_factory.rs`.
- E — Modules 10–11, i18n residual + per-endpoint unknown-query-field, with a mandatory
  injected-failure proof (the new gates would otherwise pass on first run and prove nothing).

Full findings in `HANDOFF-PROMPT.md` §5.1. Headlines:

- **The plan document does not contain the two items it was believed to own.** No mention of
  `actor_fingerprint` unification; If-Match TOCTOU appears only as a deferred follow-up naming 4
  sites. Both must be written into the plan explicitly or they will be dropped.
- **If-Match TOCTOU is 33 sites, not ~28** — all production, all `src/http/admin.rs`. Plan 04 fixed
  1 of 34. Likely its own PR.
- **`actor_fingerprint` is genuinely divergent**: three formulas (10-field, 3-field, 4-field) writing
  one `idempotency_records` unique index. Runtime-admin routes fail to isolate replay across JWT
  issuers and tenants. Also feeds `advisory_lock_key`, so unification shifts lock partitioning.
- **Module 9 as written breaks the build** — deletes `resolver.rs`, but `RuntimeConfigCache` lives
  there and `app/state.rs` + `infra/db.rs` depend on it.
- **Module 10 (i18n) is ~90% already done** by plans 02b/04/05.
- **Every line number in Modules 2–5, 12, 13 is stale.**

**First commit when work starts:** `SecretString` on `GeneratedApiKey.raw_key`
(`src/security/api_keys.rs:20-26`) — ~6 lines, 3 files, must land *before* the `admin.rs` split.

---

## OPEN FINDINGS — need a decision, not a mechanical fix

### F28 — inline memory extraction delays the terminal SSE event and consumes a caller concurrency permit

Found while building plan 11 Sub-Phase F (`feat/plan-11-subphase-f`). **Both halves only bite an
application that has turned `automatic_extraction_enabled` on, which defaults to `false`** — so
nothing in the shipped default is affected. Neither is a correctness bug; both are costs that must
not be discovered in production.

**(a) The streaming terminal event is delayed by a full extraction round-trip.**
`record_conversation_assistant` — and therefore `extract_memories` — runs at
`src/application/public.rs`'s streaming arm **before** the `response.completed` SSE event is pushed.
The tokens themselves are unaffected: `EventCollector::streaming` forwards each chunk live during
execution, so the caller already has the whole text. What is delayed is the terminal event and the
stream close. A client that waits for `response.completed` therefore sees the last token, then a
multi-second stall, then completion.

*Why it was not fixed in this wave:* the fix is to emit the terminal event before extracting, and
that arm is the streaming terminal-state machine — the same code that owns the committed-output
override and the cancellation path, and whose branch on `conversation_result` decides which SSE is
emitted. Reordering it hastily at the end of a wave is how HANDOFF §3.4's six toothless guards got
written. **No test pins the current ordering**, deliberately: a test asserting today's order would
go red on the fix, which is the "test pinned the defect" antipattern from §3.4.

**Decide:** either move extraction after the terminal SSE push (its own change, with a test that the
terminal event precedes the second provider call), or move it to the queue once a real
`JobDispatcher` exists — decision D3's reversal condition, which fixes this as a side effect.

**(b) Extraction takes a concurrency permit from the caller's own pool.**
`execute_inner` acquires `state.concurrency.acquire_scoped(provider, …, application_id,
external_user_id)` per attempt, and extraction goes through the same `MoiraExecutionService::execute`
path. So an extraction-enabled application's effective per-provider, per-application and per-user
headroom is **halved** — two permits per caller turn instead of one.

There is **no deadlock**: the caller's own permit is released when `execute_inner` returns, which is
before `record_conversation_assistant` is called. Under saturation the extraction call is simply
refused by `acquire_scoped`, which surfaces as `memory_extraction_runs.failure_class =
'extraction_call_failed'` and does not touch the caller's response — the fail-open policy working as
designed. It is a capacity-planning fact, now documented in `docs/memory-extraction.md`, not a
defect.

### F1 — 23 uncatalogued error codes reach the wire (pre-existing, NOT fixed)

`failure_code()` (`src/application/public.rs:2009`) returns 28 codes that flow into
`AppError::coded(status, failure_code(class), …)`. `AppError::message_key()` renders any code as
`moira.error.<code>`, so all 28 ship to clients as message keys — and **23 have no catalog entry**:
`provider_unavailable`, `provider_timeout`, `credential_expired`, `circuit_open`, `route_not_found`,
`deadline_exceeded`, `capacity_exhausted`, and 16 more. These are the *most common* public execution
failures, so this is the i18n contract failing exactly where it matters most.

The Module 10 walker cannot catch them: the code argument is a runtime expression, not a literal.
Instead the count is pinned — the test asserts exactly 2 runtime-computed code sites exist and names
the gap — so the number cannot grow silently.

**Why it is not fixed:** closing it needs 23 reviewed, operator-safe English strings. That is a
product-copy decision, not a refactor, and inventing them unreviewed would be worse than the gap.
**Decide:** write them in plan 06, or schedule a dedicated i18n pass.

### ~~F2 — unknown query fields are rejected before authentication, and the 400 enumerates field names~~ — **CLOSED** `fix/f2-query-rejection-envelope` (2026-08-01)

**The finding as recorded.** Rejection of an unknown query parameter was axum's `QueryRejection`:
`400 text/plain`, with no `code`, no `message_key`, and no `request_id` —
`normalize_infrastructure_error` (`src/lib.rs`) only rewrote 413 and 504. Because `Query` is the last
extractor and `admin_actor` runs *inside* the handler, **the rejection precedes authentication**, and
axum's message enumerated all 26 `PageQuery` field names to an anonymous caller. Low severity — field
*names*, no data, no credentials, and the endpoints are already known from the public OpenAPI
document — but an unauthenticated response shape that bypassed the error envelope, and it meant
**Module 11's DoD item could not honestly be ticked**.

**Two things the finding got wrong, both in the direction of understating it.**

1. **It was never a `Query`-only defect, and the "observable wire change" framing was backwards.**
   Every axum *extractor* rejection had the same shape and none was covered: `Json`'s 400/415/422,
   `Path`'s 400, `Extension`'s 500 (which names the missing Rust type). More to the point, every one
   of them **already contradicted the published contract**: `docs/openapi.json` documents `4XX` and
   `5XX` on these operations as `application/json` → `ErrorResponse`. The fix does not change the
   documented wire contract; it makes the implementation obey the one that was already committed.
   That is why the OpenAPI snapshot is byte-identical after the fix.
2. **The scope was fixed by inverting the rule, not by extending the list.** A status allow-list has
   to grow every time an extractor is added and nothing fails when it does not.
   `normalize_infrastructure_error` now rewrites **any** client- or server-error response that is not
   already `application/json` — `AppError` is the only thing in Moira that produces an error body, so
   a non-JSON error body is by construction one that bypassed the envelope. 4xx → `invalid_request`,
   5xx → `internal_error`, with 413/504 keeping their existing specific codes.

**DECISION — the rejection stays pre-authentication.** Moving it behind `admin_actor` would mean
either an authentication middleware layer (Moira deliberately authenticates *inside* handlers) or a
custom extractor threaded through all 25 `Query<…>` call sites so the handler can `?` it after
authenticating. Both are large, and neither buys anything now: the response no longer varies with the
caller, the credential, or the query string, so reaching it early reveals nothing that
`docs/openapi.json` does not already publish to anonymous readers. *Reversal condition:* if a
rejection ever becomes able to carry request-specific detail again — a per-endpoint query type with
its own message, a `details` payload, anything that makes the response a function of the input — the
early-exit becomes a disclosure again and it must move behind authentication.

**DECISION — no diagnostic detail, and none logged either.** The rejection text is discarded, not
downgraded to a log line. `redacted_request_span` deliberately drops the query string from the span,
and a rejection body echoes caller-supplied query keys and — for an unknown enum variant —
caller-supplied *values*; writing it to `tracing` would reintroduce exactly the request content that
decision removed. The cost is real: a developer who mistypes a filter now gets "The request is
invalid." and must consult the OpenAPI document. *Reversal condition:* revisit if the rejection ever
moves behind authentication, where a detailed message would go only to an authenticated caller.

**DECISION — 404 and 405 are carved out** (`ROUTER_STATUSES_LEFT_UNWRAPPED`). Both are produced by
the router rather than by a rejection, both carry an empty body, and 405 carries an `Allow` header a
synthesised envelope would drop. *Reversal condition:* stated on the constant, and
`router_produced_404_and_405_keep_their_bodyless_shape` asserts the premise (empty body, `Allow`
present) rather than assuming it — so the carve-out cannot outlive its own justification.

**Guards.** `unknown_query_field_rejection_is_plain_text_and_precedes_authentication` pinned the
defect and was rewritten, not deleted, into
`unknown_query_field_rejection_carries_the_error_envelope_and_enumerates_nothing` — renamed because a
test asserting JSON under a name that says `is_plain_text` is the §3.4 failure mode. Its enumeration
oracle **reads the 26 parameter names out of `docs/openapi.json`** instead of hard-coding them, so
field 27 is covered the day it is added, and it has a vacuity guard on the selector. The
"enumerates nothing" property is asserted twice — no documented parameter name in any *value* of the
envelope, **and** the response does not vary between two different unknown field names — because a
partial fix that echoed only one half of axum's sentence would pass either check alone. Plan 06
module 11's DoD assertions (non-empty `message_key` **and** `message`) were also added to
`each_admin_list_endpoint_rejects_an_unknown_query_field`, across all twelve routes.

**Mutation evidence.** Reverting `normalize_infrastructure_error` to its two-status form and re-running
the suite is recorded below under "F2 mutation".

### F28 — `metrics_endpoint_exposes_db_pool_gauges_reflecting_the_live_pool` races the sqlx pool under load

Found 2026-08-01 during F2's gate run, on a machine that also had another agent's `cargo clippy`
running. The suite failed with `left: 1.0, right: 0.0` at the second `assert_eq!` in
`tests/metrics_endpoint.rs`: after `fixture.pool.acquire()` holds one connection, the test requires
`idle == baseline_idle - 1`, and the scrape still reported `idle == 1` while `total` was unchanged at
`1` — i.e. a held connection and a full idle count in the *same* scrape. **The identical target
passed 9/9 on an immediate re-run with nothing changed.**

The mechanism is that `sqlx::Pool::num_idle()` is documented as *approximate* — it reads the idle
queue's length, and the pool's own maintenance can transiently disagree with `size()`. The test
treats two approximate gauges as exact and as mutually consistent within one scrape, which is a
stronger premise than sqlx offers, and the gap widens under CPU contention.

**Not caused by F2's change**, and that is checkable rather than asserted: `/metrics` answers `200`,
and `normalize_infrastructure_error` returns the response untouched for any status that is not a
client or server error, so no code path F2 touched can execute on this request.

**The fix is not "add a sleep".** Either bound the assertion (`idle <= baseline_idle - 1`, which is
what the property actually is — a held connection cannot be idle), or poll the scrape inside a
`timeout(…)` the way the rest of the suite polls for eventual state. Recorded rather than fixed here
because it is a different file from F2's change and gate runs are serialised; fixing it inside F2's
branch would have cost another full gate cycle to prove.

#### **CLOSED** 2026-08-02, `fix/shared-db-flakes` — and neither the diagnosis nor the suggested fix above was right

*(This is the metrics-gauge F28. `F28` also names an unrelated finding earlier in this file —
inline memory extraction delaying the terminal SSE event. They share only a number.)*

**`num_idle()` is not approximate.** The paragraph above is wrong on the mechanism, and the wrong
mechanism pointed at the wrong fix. `sqlx-core-0.8.6/src/pool/inner.rs` reads:

```rust
pub(super) fn num_idle(&self) -> usize {
    // We don't use `self.idle_conns.len()` as it waits for the internal
    // head and tail pointers to stop changing for a moment before calculating the length…
    // By maintaining our own atomic count, we avoid that issue entirely.
    self.num_idle.load(Ordering::Acquire)
}
```

A dedicated `AtomicUsize` since 0.6, and `size()` is another. Both are exact reads. There is also no
pool maintenance task to "transiently disagree" with them: `spawn_maintenance_tasks` returns
immediately unless `max_lifetime`, `idle_timeout` or `min_connections` is set, and the fixture pool
sets none of them.

**What is actually asynchronous is the return.** `Drop for PoolConnection` *spawns* a task, and
`Floating::return_to_pool` issues `self.raw.ping().await` — a full round-trip to PostgreSQL — before
it re-queues the connection and increments `num_idle`. So between a `drop` and the counter moving
there is a database RTT during which the pool is genuinely still settling, and a scrape landing
inside it reads a low `idle`. That is the failure exactly: the baseline scrape sampled `idle = 1`
while a second connection's return was in flight, the return landed, and the "busy" scrape read
`idle = 1` again instead of `0` — `left: 1.0, right: 0.0`.

**Reproduced deterministically before anything was changed.** Replaying the test's shape back to
back, so that each iteration's `drop` is the only thing between it and the next baseline scrape,
failed **8 times in 10**; the delay that made the committed test pass was incidental — fixture setup
and the first scrape happened to give the in-flight return time to land. Instrumented output, one
failing iteration: `pre(size=2,idle=1) base(total=2,idle=1) post_idle=2 held_idle=1
busy(total=2,idle=1) expected_busy_idle=0`. Note `pre_idle=1` against `post_idle=2`: the return
landed *during* the scrape it was supposed to precede.

**Neither a bound nor a poll was needed.** Two properties of the pool make the numbers exactly
knowable:

* **`acquire` is a barrier.** A connection whose return is still in flight has not yet released its
  semaphore permit, so taking *every* permit cannot return until every pending return has completed.
* **`PoolConnection::return_to_pool().await`** runs that same return eagerly instead of spawning it.

Between them the pool reaches a state with nothing in flight. Measured 20/20 and 15/15 before being
written up. `idle` is now asserted at **`capacity`**, **`capacity - 1`** and **`0`** — three exact
values, no tolerance, no timing — which is *stronger* than the exact assertion it replaced, not
weaker. The suggested `idle <= baseline_idle - 1` bound would have been satisfied by a gauge frozen
at zero.

**Two false claims in the test's own doc comment, both load-bearing** (the F22 pattern — the
investigation found them, they were not reported):

1. *"Nothing else touches this pool."* sqlx does, per above.
2. *"`LifecycleFixture` serialises the suite."* It does not, and has not since fixtures were given
   private databases. `tests/support/mod.rs` says so in as many words at `CONCURRENT_FIXTURES`:
   *"This is **not** an isolation device — the database is."* Four fixtures run concurrently.

A third claim — *"neither `/metrics` nor `/health/live` opens a connection"* — is **true**, is now
load-bearing rather than incidental, and is proven in passing: the test scrapes successfully while
holding all eight permits, which it could not do if the handler needed one.

**Teeth, by injection.** Four mutations of `render_prometheus`, each reverted:

| Mutation | Result |
|---|---|
| `record_db_pool_utilization(8, 8)` — both constant | RED at `capacity - 1`: *"the idle gauge did not follow the live pool"* |
| `(pool.size(), pool.size())` — idle wired to total | RED, `left: 8.0, right: 7.0` |
| sampling deleted, gauges frozen at their `0.0` init | RED at the settled scrape, `left: 0.0, right: 8.0` |
| `total` from `options().get_max_connections()` | **GREEN — a survivor.** See below |

**The fourth is the one worth reading.** Driving the pool to saturation is what bought determinism,
and it also made `pool.size()` and `max_connections` the same number in every observation the test
made — so a `total` gauge sourced from the configured ceiling passed. Verified by running the
committed test against the mutation: `test result: ok`. Fixed by one scrape taken **before** the
pool is touched, where the fixture has used it but not exhausted it. Only `total` is asserted there,
because `size` is precisely the quantity an in-flight return does *not* move.

*Reversal condition:* reopens if the pre-saturation scrape is deleted, or if these gauges are
asserted anywhere against a pool not first brought to a known state.

*Left open, deliberately:* `render_prometheus` does `u32::try_from(pool.num_idle()).unwrap_or(u32::MAX)`.
sqlx's `release()` pushes to the idle queue and releases the permit **before** `num_idle.fetch_add`,
so on a multi-threaded runtime a waiter can `fetch_sub` first and wrap the `AtomicUsize` to
`usize::MAX` — published as `4294967295`. `#[tokio::test]` is current-thread, so no test here can
observe it; **production is multi-threaded**. The one-line guard is to clamp the gauge to `size()`.
Recorded rather than fixed: it is a `src/` behaviour change, and this branch is a test-hygiene
branch.

### F3 — the skill files still describe deleted code, and agents are briefed from them

Module 9 deleted `resolver.rs`, `executor.rs` and `src/http/chat.rs`. Several skill files still
describe them as present, and one asserts the Rig boundary lives in `executor.rs` — the exact
anti-pattern module 7 removed. **These are not documentation; they are the instructions subagents are
given.** A stale skill actively steers the next agent into reintroducing deleted structure, which is
strictly worse than a stale doc a human might skim past.

Module 7's agent corrected `moira-rig-integration` and `moira-rig-completions`. Still stale:
`moira-rig-providers/SKILL.md:312,330,524-525`, `moira-rig-tools/SKILL.md:608`,
`moira-rig-streaming/SKILL.md:12`, `moira-rig-errors-testing/SKILL.md:148,441`, and
`skills/moira-project-structure/SKILL.md` (which also omits `src/application` and `src/i18n`, and
still says orchestration owns credential selection). **Duplicated under both `.claude/skills/` and
`.agents/skills/` — fix both copies.**

### F4 — `invalidate_runtime()` reproduces the over-broad circuit reset from the service side

Module 14 narrowed the LISTEN/NOTIFY path, but `src/application/runtime_admin.rs:634`
(`async fn invalidate_runtime`, declared `:631`) still calls the unconditional reset, and it has
**14 callers** (`:81,143,164,188,230,298,339,365,411,478,501,527,610`) — so every runtime-admin
mutation still discards health state for providers that never changed. The service knows which
resource it just changed, so the mapping to `CircuitResetScope` is direct.

Not fixed in module 14 because module 16 holds that file. **Follow-up commit once module 16 lands.**

### F5 — module 13's new template-database fixture has cross-process contention

`every_non_sse_route_group_is_governed_by_the_non_streaming_timeout`
(`tests/http_middleware_contract.rs`) failed once on a full-workspace run at `tests/support/mod.rs:789`
— `connections to moira_test_template were never released` after a 10s wait. Cargo runs test binaries
concurrently, so the new fixture waits on a *different process*. Passes in isolation and on re-run.

This is a new flake introduced while fixing the old one, which is worth stating plainly rather than
counting module 13 as done. Its owner must widen or serialise that wait. **Do not treat plan 06 as
green until a full-workspace run passes twice consecutively from cold.**

### F6 — OTel exports every recorded span, so `env_filter` is the only thing holding prompts back — **CLOSED** `f31ff59`

**Mechanism.** `src/config/telemetry.rs` now applies an **allow-list of Moira-owned target roots**
(`EXPORTABLE_TARGET_ROOTS = ["moira"]`, matched on target *segments*) to the `tracing`→OTLP bridge
layer itself, in `otel_export_layer`. A global filter can only ever *narrow* what reaches a layer,
so the allow-list sits below the `EnvFilter` and no value of `env_filter` or `RUST_LOG` — including
a bare `trace` — can widen it back open. Same placement, and the same reasoning, as F16's log
suppression. It replaces a denylist (`!target.starts_with("opentelemetry")`), which is the F8 shape:
every future dependency opted in by default.

**Two corrections to the description above, which was written months ago.**

1. **It was not a `debug`/`trace` hazard.** `rig-core` 0.40's completion span is an **`info_span!`**
   on target `rig::completions`, carrying `gen_ai.system_instructions` — the preamble — as a span
   attribute (`providers/openai/completion/mod.rs:1929`, `providers/anthropic/completion.rs:2467`,
   and the gemini/xai/ollama equivalents). A bare `info` filter, which is an ordinary thing for an
   operator to set, was already enough. The bar was one notch lower than recorded.

   Worth knowing precisely, because it is unintuitive: those call sites read
   `if tracing::Span::current().is_disabled() { info_span!(…) } else { Span::current() }`. So when
   Moira's own `execution_attempt` span is live, Rig **reuses it** and its `gen_ai.*` recordings
   no-op against a span that never declared those fields. The exposure window is therefore
   *`rig` enabled while Moira's `DEBUG` spans are not* — i.e. exactly a bare `info`, and exactly the
   configuration an operator reaches for. The guard does not rely on that upstream branch either
   way: it asserts the property, not Rig's current control flow, which can change in any release.
2. **The mitigation deliberately has no `INFO`-and-above carve-out**, unlike F16's. In the log
   pipeline level separates content from diagnostics — the body dump is at `TRACE`, failures at
   `WARN` — so keeping `INFO`+ costs nothing. In the span pipeline that separation does not exist:
   the prompt-bearing span *is* the `INFO` span, so an `INFO` carve-out would export precisely what
   the guard exists to stop. Verified as mutation M3 below. The constraint it was meant to serve is
   still met: the `fmt` layers are untouched, so third-party warnings and errors reach stdout exactly
   as before. Only the collector stops receiving them, and provider outcome reaches it on Moira's own
   `execution_attempt` span.

**Why a layer filter and not the two alternatives.** A `SpanProcessor` keyed on instrumentation
scope cannot work: `tracing-opentelemetry` 0.33 bridges every span through a *single* tracer, so
Rig's span and Moira's share the scope `moira` — asserted, not assumed, in
`every_bridged_span_shares_one_instrumentation_scope`. A `Sampler` never sees the `tracing` target,
only the span name, so it would be a denylist on names like `chat`. And any processor-level filter
sees whole spans only: a third-party *event* is attached to whichever OTel span is current, so a Rig
event fired inside `execution_attempt` rides out on a span the processor has no reason to drop —
also asserted, in the premise test. The layer filter is the only one of the three that governs spans
and events alike, and it acts before attribute values are copied into a span builder.

**Verified by injection, not by reading.** Six mutations, each caught by exactly one test:

| | Mutation | Test that failed |
|---|---|---|
| M1 | delete `.with_filter(...)` from `otel_export_layer` | `no_third_party_span_is_exported_however_permissive_the_filter_is` — `env_filter="trace" exported Rig's completion span: ["chat", "execution_attempt", "http_request"]` |
| M2 | restore the old `opentelemetry`-prefix denylist | same |
| M3 | add F16's `INFO`-and-above carve-out | same |
| M4 | `target.contains(root)` instead of segment match | `target_ownership_is_decided_by_segment_not_by_substring` |
| M5 | drop every span (`false`) | `moiras_own_spans_are_still_exported_with_the_guard_on` — M5 is the mutation that passes points 1 and 2 by destroying the observability plan 05 built |
| M6 | `init()` builds the bridge layer itself, bypassing the seam | `the_otlp_bridge_layer_is_constructed_in_exactly_one_place` |
| M7 | delete F16's `.with(filter_fn(suppresses_provider_payload_logs))` | *nothing* — 598/598 green. See F16: that gap was found here and closed in `8bbda15` |

The tests also assert their own premise — an unguarded bridge layer, reachable only from
`#[cfg(test)]`, *does* export `chat`, its preamble, and a Rig event's payload on Moira's own span —
so a future change that makes the emitting side stop producing the dangerous span turns the suite
red rather than silently vacuous. Everything runs in-process against a recording `SpanExporter`; no
collector, no network.

**Reversal condition.** The allow-list becomes *wrong* the day Moira wants a third party's spans in
its traces — `sqlx`, an HTTP client, a worker crate. That is a legitimate want, and the answer is to
add that target root to `EXPORTABLE_TARGET_ROOTS` after reading what the crate puts on its spans,
not to relax the predicate. It becomes *unnecessary* only if Moira stops exporting spans altogether,
or if every crate in the tree gains a content-free instrumentation guarantee — neither is in
prospect. Unlike F16's filter, this one does not go away when `rig-core` fixes its logging: it is
about every dependency, not about Rig.

**Also fixed in passing:** `docs/otel.md` and `.env.example` advertised port **4317** for an
exporter that speaks OTLP/**HTTP**. Every operator following either would have configured a
collector's gRPC port and got nothing.

### F12 — the shipped container image carries 5 CRITICAL and 31 HIGH CVEs — hardening in progress

Found the first time trivy actually ran (`30220949227`). It had been masked: `container-and-helm`
died at step 1 on a broken action pin, so the scanner never executed. Two earlier pin fixes are what
made this visible.

**`apt-get upgrade` is not the fix.** Status of the sampled CVEs: **8 `affected`, 7 `fix_deferred`,
1 `will_not_fix`, 1 `fixed`.** Debian has not shipped patches for nearly all of them, so the only
real mitigation is removing the packages — 106 in `debian:bookworm-slim`, of which a Rust binary
calls almost none.

**The largest single contributor was self-inflicted.** `curl` was installed for exactly one reason,
the `HEALTHCHECK`, and dragged in ~5 curl/libcurl CVEs including an SSH host-verification bypass and
a TLS downgrade, several `fix_deferred`. Meanwhile `charts/moira/templates/deployment.yaml:42-50`
already defines `readinessProbe` and `livenessProbe` — Kubernetes does the health checking in the
real deployment target, so the Docker `HEALTHCHECK` was redundant. A TLS-bypass CVE class was being
carried to support a probe nothing used.

**User decision (2026-07-27):** harden now, then merge; base becomes
`gcr.io/distroless/cc-debian12:nonroot`. Verified prerequisite: the lockfile has **zero**
`openssl-sys`/`native-tls` entries — TLS is pure `rustls` — so glibc + ca-certificates suffices.

**VERIFIED end to end, 2026-07-27 — 36 CVEs → 0.**

| | Before | After |
|---|---|---|
| CRITICAL + HIGH | 36 (5 / 31) | **0** |
| Image size | ~120 MB | **69.6 MB** |
| Shell | present | **absent** (`exec /bin/sh` → no such file) |
| UID | 10001 | **65532**, matching both manifests |

Not just built — **run**: the container starts, `/health/live` returns `200 {"status":"ok",…}`, and
it runs as 65532. That check was the point. Distroless failures appear at *startup*, not at build,
so a green `docker build` proves nothing about whether the binary can find its dynamic libraries.

Zero, rather than "fewer", because the vulnerable code is *absent* rather than patched — which is
the whole argument for the approach given that Debian had declined or deferred fixes for nearly all
36.

**Two process failures worth carrying forward:**
1. **`docker build … | tail` reports `tail`'s exit status.** A build that died with
   `DeadlineExceeded` pulling base-image metadata was reported as exit 0. Never judge a piped
   command by its exit code; check the log or use `PIPESTATUS`.
2. **`bb7009e` caught a half-done reconciliation reported as complete.** `deployment.yaml` moved to
   65532 while `migration-job.yaml` stayed at 10001 — same image, and distroless has no shell or
   package manager with which to create a second user. It would have failed at deploy time, not in
   CI. When an agent reports "verified", check the specific thing that could fail silently.

### F19 — validating inside the envelope turns invite redemption into an enumeration oracle

**Found by measuring a mutation that nothing caught, then asking what the guard is actually for.**

Moving redeem's validation inside the transactional envelope was **caught by nothing** — 869 passed,
0 failed, byte-identical to baseline. Neither the mechanism §0 claimed (a replayed 403 — impossible,
`is_cacheable_admin_failure` excludes `Forbidden`) nor the correction to it (assert the invite's
`status`) can see the difference: `AdminCommandRunner::execute` rolls back on any non-cacheable
failure, so a denial inside the envelope has its `consume_invite` rolled back too. **Pre-envelope
ordering and transaction rollback defend the same property**, which is why no assertion on that
property distinguishes them.

**What ordering uniquely buys is which failure the caller is told about.** Inside the envelope
`insert_grant` runs first, so a caller whose `(issuer, subject)` already holds a grant receives
`409 admin_identity_already_claimed` — *before* the invite's own constraint has refused them, and
before policy has. A stranger holding a leaked invite token can therefore learn whether an arbitrary
identity already has admin, from a request the invite should have refused outright.

Pinned by `a_refusal_that_belongs_to_the_invite_is_not_pre_empted_by_the_grant_table`, which probes
both checkpoints with an already-granted identity as an explicit premise. Verified failing under the
mutation (`admin_identity_already_claimed` where `invite_email_mismatch` was due) and passing at HEAD.

**The lesson is about how the guard was documented.** Its doc comment credited ordering with
"never reaching the statement that marks the invite consumed" — something rollback would have done
anyway. A guard justified by the wrong property is one refactor away from being removed as
redundant, because *for that property it is*.

### F20 — on any deployment created after `0017`, no admin is ever primary — **FIXED** `a6d2984`

`claim` never sets `is_primary` — the shared `insert_grant` names no such column — and `0017`'s
backfill is a one-shot migration-time `UPDATE` that finds an **empty table** on a greenfield deploy.
So the ownership-transfer endpoint is unreachable by every JWT admin until an operator reaches for
the system-key break-glass `PATCH`.

`0017` says "the setup claimant is primary by default", which is true only for deployments that
already had a claimant when `0017` ran.

**Fixed on `fix/findings-sweep` under decision D-F20** (user, 2026-07-31; written into plan 09 §0.2
with its reversal condition). A single primary, taken at the first grant on a deployment that has
none — system-key claim or redeemed invitation alike. `insert_grant` computes it in SQL under the
existing `moiraown` transaction advisory lock; `0019` adds
`admin_identities_single_active_primary` as the backstop and repairs already-deployed instances;
`set_primary` now *moves* the flag.

**The load-bearing design choice was which of the two mechanisms decides the race.** A partial
unique index alone would make the *loser of two concurrent first grants fail with a 500* — the
index cannot express "you are second, so you are not the owner", only "you are second, so you are
rejected". The advisory lock is what turns that into a demotion; the index is what keeps the
invariant true if a future path forgets the lock. Pinned by
`two_simultaneous_first_grants_produce_exactly_one_owner_and_no_failure`, whose assertion is
deliberately *both* `201`s **and** one owner — a test that accepted a failing racer would pass
against the index-only design.

**Consequence worth knowing:** a deployment's sole admin can no longer be revoked through the API.
They are the owner, revocation clears `is_primary`, and the last-primary guard refuses it. Transfer
first, then revoke. Documented in `docs/admin-identity-claiming.md`.

**`0019`'s repair path was verified against real PostgreSQL, not reasoned about** — it is the one
part of the change no test in the suite can reach, because by the time a test runs the migration has
already applied. Three scenarios on throwaway databases:

| Scenario | Result |
|---|---|
| Three active grants, **two** primary, setup claimant = the *younger* of the two | Collapsed to one, and the **claimant** survived, not the oldest. The `coalesce(id = claimant, false) desc` is load-bearing: `desc` puts NULLs *first* in PostgreSQL, so without the `coalesce` a deployment with no recorded claimant would sort an arbitrary row to the top |
| Zero primaries, a recorded claimant (**the F20 population**) | Claimant promoted, the other grant untouched |
| Re-running the whole migration | Nothing moved; the index creation is `if not exists` |
| `update … set is_primary = true` on a second row afterwards | `ERROR: duplicate key value violates unique constraint "admin_identities_single_active_primary"` — which is also what makes `already_claimed_on_unique_violation`'s constraint-name match real rather than assumed |

### METHOD NOTE — right conclusion, wrong mechanism, and the test that would have proved nothing

Plan 09 §0 argued the redeem validation must sit **outside** the transactional envelope because,
since `Idempotency-Key` is required, a denied redemption would write a ledger row and a later retry
would **replay the stored 403** after the operator widened the allow-list.

**The mechanism is wrong.** `AppError::is_cacheable_admin_failure` (`src/error.rs:209-225`) caches
only 400/404/409/422 and explicitly excludes `Self::Forbidden(_)`. A 403 raised inside the closure
takes the rollback arm and leaves **no** ledger row. There is no replay.

The conclusion still holds — validating outside the envelope is what stops the invite being consumed
and the advisory lock being taken — so the code is right. **But a test written against the stated
mechanism would have passed in both the fixed and the broken arrangement**, because neither produces
a replayed 403. It would have been a green test proving nothing, added specifically to guard the
thing it does not touch.

The ordering test must assert on the **invite row's `status`**, not on a replayed response.

**This is the second time in this run that a mechanism I asserted was wrong in a way that would have
produced a false-confidence test** — the first being F15, where a type was named as safe without
reading its fields. Both were caught by an agent verifying rather than implementing. The pattern:
*a conclusion can be right for a reason that is wrong, and the test follows the reason.*

### F17 — rotating `BETTER_AUTH_SECRET` makes the console publish a JWKS it cannot sign for — **CLOSED** `fix/f17-jwks-rotation`

**A new hazard created by durable storage; the in-memory path did not have it.**

On rotation, `getJwks` serves the **plaintext `publicKey` column**, so the JWKS document is unchanged
and Moira's cached copy stays valid. Meanwhile `signJWT` fails with `Failed to decrypt private key`
— and it does **not** regenerate the pair. The console therefore advertises keys it can no longer
sign with, and every token it mints is rejected. Silently: the JWKS endpoint looks healthy.

**Why it is worth its own entry:** with the memory adapter, a rotation regenerated the pair and the
next process simply published new keys. Making storage durable — which fixes three other problems —
converts a self-healing restart into a silent, persistent outage. That is the shape of hazard worth
looking for whenever ephemeral state is made persistent.

**Blast radius, corrected.** The entry above predates wave 4B. There is now one key pair, one `kid`
and one JWKS URL behind **N** provider issuers, so a single undecryptable row takes every provider's
admin path down at once — as D3's "what A′ does not buy" already predicted in the abstract.

#### The mechanism — option 1, "fail loudly", implemented as an invariant rather than a check

`console/lib/jwks-signable.ts` installs the jwt plugin's `adapter.getJwks` override.
`plugins/jwt/adapter.mjs` routes **both** `getAllKeys` (what the JWKS endpoint publishes) and
`getLatestKey` (what `signJWT` signs with) through that one function, so **published ⊆ signable holds
by construction** — not by two checks that have to agree. A table with rows and none decryptable is
refused with a `503 JWKS_KEY_UNSIGNABLE` and one log line carrying the remedy; an empty table is
still the plugin's ordinary mint-on-first-read.

The predicate is `symmetricDecrypt` against `ctx.context.secretConfig` — literally the two
operations `sign.mjs` performs before `importJWK`, in the same order. A check that *re-derived* the
answer could disagree with the signer, and disagreement in the permissive direction is F17 itself.

**Why not the other two.** *Regenerate on decrypt failure* (option 2) restores self-healing and
preserves the observable property, but it cannot distinguish a deliberate rotation from a **wrong
secret supplied by mistake**, and in the second case it destroys a fully recoverable state by
silently minting a new console identity — while the operator, who has just changed a secret and does
not yet know anything is wrong, is told nothing. Under the refusal, putting the old secret back is a
*complete* recovery: same `kid`, Moira's cache never invalidated, nothing in flight orphaned.
*Make rotation supported* (option 3) is real and cheaper than expected — better-auth 1.6.25 already
ships versioned secrets (`options.secrets` → `SecretConfig`, `$ba$<version>$` envelopes, a
`legacySecret` fallback), which the console does not use — but it closes rotation, **not the
finding**: an operator who rotates without declaring the previous secret still gets the silent
outage. Option 1 is the only one that is unconditional. 1 and 3 compose; 2 is excluded by 1.

**A startup probe was rejected as *the* mechanism** and would only be an addition: it is a
point-in-time sample (a row can stop being decryptable after boot — a second replica writing under a
different secret, a restore from an older backup), Next.js has no hook between "pool exists" and
"first request", and a boot-time database probe turns a transient database blip into a console that
refuses to start.

#### The guard, and what the mutations showed

`console-jwks-stability.test.ts` **was not a working test for this** — it *pinned the defect*. It
asserted that the published document was unchanged by the rotation and that signing raised the
library's decrypt message, which documents F17 and guards nothing: it goes red on the fix, which is
the opposite of what a guard does. Worse, it asked the two questions **separately**, and F17 is the
*conjunction* — a 200 JWKS is unremarkable, a signing failure is unremarkable.

It now asserts the joint property over a real socket: the JWKS is fetched over TLS through the
shipped route handler (`app/api/auth/[...all]/route.ts`, not `auth.handler`) and the minted token is
verified against **that document** with `jose`, by `kid` — Moira's own verification path.

Five mutations, each applied by hand and observed:

| Mutation | Result |
|---|---|
| delete `adapter: signableJwksAdapter(...)` from `lib/auth.ts` | **caught** — "the console PUBLISHED […] and then failed to sign" |
| `throw` → return the empty set (i.e. become option 2) | **caught** by the separately-labelled *mechanism* assertion, not by the property — option 2 satisfies the property |
| predicate always `true` | **caught** |
| `disablePrivateKeyEncryption` flag inverted | **caught** |
| publish **all** rows whenever one is signable | **caught only after the guard was extended** — see below |

**The fifth one is the finding inside the fix.** Asked for "the cheapest edit that breaks the
property while leaving the guard green", there *was* one: the first four tests only ever put the
table in an all-usable or an all-unusable state, against which "publish the usable ones" and "publish
everything as long as one is usable" are indistinguishable. A mixed table is reachable (a restore, a
second replica, `rotationInterval` plus a rotation between two mints). The guard now inserts a decoy
row by **raw SQL** — no API can produce one — stamped newest so `getLatestKey` would reach for it.
Same technique guard G1 had to be rebuilt with, for the same reason.

**Residual, stated rather than hidden:** the predicate stops at decrypt-and-parse. A row that
decrypts to a *structurally invalid* JWK would still be published. Nothing in this system writes one
(better-auth is the only writer), and closing it would mean importing `jose` — a devDependency —
into shipped console code.

#### Reversal condition

Replace the refusal with re-encryption **the moment there is an operator signal that distinguishes a
deliberate rotation from a mistake** — that is, when the console adopts better-auth's versioned
`options.secrets` and an operator can supply the previous secret alongside the new one. At that point
the deliberate case should re-key the stored row with no new `kid` and no outage, and this refusal
should survive as the backstop for the *unexplained* case only. Delete it outright only if
`plugins/jwt/adapter.mjs` stops routing `getLatestKey` through `options.adapter.getJwks`, because the
invariant is structural in exactly that fact — and if that happens the mechanism must be rebuilt, not
merely removed.

**What an operator must still do by hand:** exactly one thing — `delete from "jwks";` when the
rotation was intended. That step is the assertion that a new console signing identity is wanted, and
it is deliberately not automated.

### F18 — the sign-in rate limit was multiplying by replica count

Better Auth's default `rateLimit.storage` is **per-process memory**. With N replicas the effective
sign-in limit was N× the configured value, and the configured value only applied in production at
all (`enabled: isProduction`). Set to `"database"` in plan 09 Wave 1, so the limit is now shared.

Minor while `replicaCount` is pinned at 1, and exactly the kind of thing that stops being minor the
moment someone lifts that pin.

### F16 — ESCALATED: `rig-core` logs the whole completion body, which now carries OTHER tenants' documents

**`rig-core` 0.40 emits the entire completion request body — every message, verbatim — on the
`rig::completions` target at TRACE.** Pre-existing, and until now it exposed only caller-supplied
prompts: bad, but the caller typed it.

**Plan 11 changes the severity class.** The assembled context now also contains retrieved RAG chunk
text and memory content — material the caller never typed and, in a multi-tenant deployment, may
have no right to see in a log stream. An operator raising a log level to debug routing would have
been silently exporting other documents' contents.

This is the same hazard as **F6** (OTel bridges every recorded span, so `env_filter` is the only
barrier) arriving through a second channel. This is its sibling. **F6 is now closed too** (`f31ff59`),
by a filter of the same shape in the same file — with one deliberate difference: no
`INFO`-and-above carve-out, because Rig's prompt-bearing *span* is itself an `info_span!`. See F6.

**Mitigated in plan 11 Wave 2**, and the shape of the mitigation matters: a hard suppression in
`src/config/telemetry.rs` sitting **below** the `EnvFilter`, so it holds however the operator sets
`env_filter` or `RUST_LOG`. Someone who wants `moira=trace` to debug routing must not have to accept
every prompt and every retrieved chunk as the price. `INFO` and above still pass, so upstream
warnings and errors are never hidden — the dropped events are exactly the ones whose *content* is
the payload.

Found by a canary test, not by review.

**Its guard was untested where it mattered — the fifth of these, and it was shipped and trusted.**
Found while closing F6, by injection: deleting `.with(filter_fn(suppresses_provider_payload_logs))`
from `init` left **all 598 library tests green**. F16's three tests all exercise a `suppresses()`
helper *re-implemented inside the test module*, because `filter_fn` receives a `Metadata` that
cannot be constructed outside `tracing`'s macros — so they tested the decision and never the stack
the decision was supposed to be installed in. The reason no test could reach it was structural:
`init` installs a global subscriber, which a test process can set once and never undo. Fixed in
`8bbda15` — stack assembly moved into `build_subscriber`, which `with_default` can install, and
`the_payload_log_suppression_is_wired_into_the_subscriber_stack` asks the installed subscriber via
`tracing::enabled!`. Verified failing under the same deletion.

**The transferable rule:** a predicate test and a wiring test are different tests, and "the filter
function is correct" says nothing about whether anything calls it. Every one of this project's
laundering findings has had a correct predicate.

**Reversal condition:** remove it the moment `rig-core` gains a way to disable or redact
request-body logging at the source, which is where it belongs. Residual risk is documented in
`docs/rag-security.md`. **The proper fix is upstream** — worth raising with the rig-core maintainers.

### F15 — ESCALATED: the console cannot render a sign-in button without already being signed in

**Every read of the auth configuration requires a credential.** Verified against the frozen spec:

```
GET  /api/v1/admin/setup/auth-methods  ->  [{bearerAuth}, {systemKeyAuth}]
GET  /api/v1/admin/setup/claim-status  ->  anonymous  (returns one boolean)
POST /api/v1/admin/setup/claim         ->  [{systemKeyAuth}]
```

`claim-status` is the **only** anonymous admin operation and it carries a single bit. So to learn
which sign-in methods a deployment offers, the console needs a bearer token — which is the JWT it
can only mint *after* a user signs in. **Circular.**

The practical consequence: an operator who removes `MOIRA_SYSTEM_KEY` after setup — which is exactly
what one does with a bootstrap credential — leaves a console that can never render a sign-in button
again. Plan 08 works around it with a configuration snapshot taken while the key is still present.

**This is a consequence of plan 07's decision D4**, which made `auth-methods` authenticated on
information-content grounds: one bit of "is setup done" is free, the identity configuration is not.
That reasoning is sound for the *full* record and wrong for the projection a login screen needs.

**⚠️ THE RECOMMENDED FIX BELOW WAS WRONG, AND IT WAS THE LOAD-BEARING CLAIM. Kept, struck, because
the error is more instructive than the correction.**

> ~~Serve the public projection anonymously. `PublicAuthMethod` is exactly this — method name and
> `client_id`, **no secrets, no policy**.~~

**`PublicAuthMethod` carries `allowed_email_domains`.** That field *is* decision D3 — the
deny-by-default admin-claim policy. Serving it anonymously would have published, to any
unauthenticated caller, the exact set of email domains that can obtain Moira admin: a ready-made
phishing target list, for zero rendering benefit.

It also fails the very test the recommendation used to justify itself. `client_id` is safe *because*
it appears in every OAuth redirect URL a browser sends. `allowed_email_domains` never appears on that
wire at all. And `plans/07-…md` already listed "relaxed to anonymous so the browser can call it
directly" as a **risk to defend against**.

**Actually fixed as:** a *narrower* anonymous endpoint,
`GET /api/v1/admin/setup/sign-in-methods` → `PublicSignInMethod`, which is `PublicAuthMethod` minus
the domain policy and minus `jwks_url`, with `jwks` rows filtered out (they have no
`authorization_url` and would render a button that cannot work). `auth-methods` is **unchanged** and
D4 stands. Stays under `/api/v1/admin/` so it keeps the admin strip, body limit and timeout.

**The admitting rule, written onto the type so a future addition has a test rather than a judgement
call:** *every field must be one the browser already transmits or receives during the sign-in it is
about to start.* **Reversal condition:** if `PublicSignInMethod` is ever widened to a field failing
that rule, the endpoint goes back behind authentication — the anonymity is justified by the
response's *contents*, never by the endpoint's purpose.

**Process note.** The agent was told to verify the premise before relaxing anything, and to stop and
say so if it did not hold. It did not hold; it stopped. **An escalation is not evidence** — this one
named a specific type as safe without reading its fields, and the recommendation would have shipped
a security leak dressed as a usability fix.

**Not fixed in plan 08** because it is a Moira API change, not a console change, and plan 08 is a
console iteration. Scheduled as a follow-up. Until then the snapshot workaround holds, with the
failure mode above documented in the console's own notes.

#### RESOLVED — `fix/f15-anonymous-auth-methods`. The recommendation above was wrong in one detail, and it was the load-bearing one.

**`PublicAuthMethod` does carry policy.** The claim "no secrets, no policy" is false: its tenth field
is `allowed_email_domains` (`src/domain/auth_settings.rs`), which is not rendering data but plan 07
decision **D3** itself — the deny-by-default admin-claim allow-list. Serving it anonymously as
recommended would have published, to any unauthenticated caller, the exact set of email domains that
can obtain Moira admin: a ready-made phishing target list, for zero rendering benefit. Plan 07's own
risk register anticipated this — §Risks item 10 lists *"`GET …/setup/auth-methods` is relaxed to
anonymous"* as a failure mode to defend against.

**So the fix is a narrower new operation, not a relaxed old one.**

```
GET /api/v1/admin/setup/sign-in-methods  ->  anonymous  (PublicSignInMethod)
GET /api/v1/admin/setup/auth-methods     ->  unchanged, [{bearerAuth}, {systemKeyAuth}]
```

`PublicSignInMethod` is `PublicAuthMethod` minus `allowed_email_domains` (policy) and minus
`jwks_url` (machine token verification, not a button), and `jwks` rows are filtered out entirely.
D4 stands unamended for the endpoint it was about.

**The rule that admits a field to the anonymous projection** — apply it to any future addition:
*every field must be one the browser already transmits or receives during the sign-in it is about to
start.* `client_id`, `issuer`, `authorization_url` and `requested_scopes` all appear in the OAuth
authorization URL the browser is redirected to, so an anonymous caller learns nothing it could not
learn by clicking the button. `allowed_email_domains` never appears on that wire, which is precisely
why it fails the rule.

The path stays under `/api/v1/admin/` — moving it out would also move it out of the admin strip, the
admin body limit and the admin timeout (`src/http/identity.rs`).

**Reversal condition:** if `PublicSignInMethod` is ever widened to a field that is *not* visible to
the browser during sign-in, this endpoint must go back behind authentication — the anonymity is
justified by the response's contents, not by the endpoint's purpose. Five gates enforce it, all
mutation-verified against an injected `client_secret` field: two domain unit tests, two E2E tests,
and the OpenAPI drift gate.

**Test rename:** `claim_status_is_anonymous_while_auth_methods_is_not` →
`the_anonymous_setup_surface_is_claim_status_and_sign_in_methods_but_never_auth_methods`. It pinned a
two-way asymmetry that is now three-way. Plan 07's verification table (lines 1258, 1318, 1349) still
names the old identifier; the invariant it described is preserved and strengthened, not dropped.

### F14 — memory dedupe silently stops matching after a pepper rotation — **FIXED** `74262ad`

`memory_records.content_hash` was written with `IdempotencyHasher::hash`, which produces
`"{pepper_version}:{base64url(hmac(...))}"`. `IdempotencyHasher::verify` deliberately accepts
**only the active pepper** (`src/security/idempotency.rs`). So after a pepper rotation every stored
`content_hash` became unmatchable and exact-match memory dedupe would silently stop working.

**The finding's own framing conflated two tables, and the distinction turned out to be decisive.**
The hasher's narrow verify contract is justified in its own module doc by a *retention* argument —
every `idempotency_records` row expires within 24 hours, so old-pepper rows age out on their own.
`memory_records` has no retention: a nullable `valid_until` and a `status` that stays `'active'`
indefinitely. The hasher is correct for its namesake table and was reused for one with a
fundamentally different lifetime. Meanwhile the *same* call in `src/application/conversation.rs`
also writes `conversation_messages.content_hash`, which **is** served to callers
(`ConversationMessageRecord`, and in `docs/openapi.json`) where the memory hash is not.

So plan 11's `chunk_hash` admitting rule was applied **per table**, not to "content_hash" as one
thing:

- `memory_records.content_hash` → unkeyed `request_hash`. Verified against all three clauses: not
  on any caller-visible DTO or OpenAPI schema; never a caller-supplied lookup key
  (`MemoryCreateRequest`/`MemoryPatchRequest`/`MemoryQuery` are all `deny_unknown_fields` and carry
  no hash field — the only caller-supplied key is the `mem_…` public id); and every read is bound
  by `application_id` (`find_memory_authorized`, `list_memories_authorized`,
  `find_memory_candidates`).
- `conversation_messages.content_hash` **stays keyed** — it fails clause (a) outright, and an
  unkeyed digest handed to callers is an offline verifier against content the schema expects to be
  able to hold encrypted (`content_encrypted`).

**Migration `0021` re-hashes** rather than accepting a one-time dedupe reset. The reset would be
nearly free *today* — Sub-Phase F is deferred, so no read path compares this column yet — and that
is exactly why it is the wrong choice: it would leave the column holding two incomparable formats
forever, so the first dedupe reader to ship would silently miss every pre-F14 row. That is F14's
failure mode re-created from a format split, arriving later, with more data and nobody left who
remembers. Only rows with `content_plain` are recomputed; `content_encrypted` has no writer
anywhere in the tree today, and if it ever gains one no SQL migration could recompute those rows.
Rows the migration skips keep their `"v1:"`-prefixed value, which is self-describing: a `:` can
never appear in a base64url content address, so a future reader misses such a row rather than
matching it wrongly.

**Reversal condition** (recorded on `memory_content_hash` in `src/application/conversation.rs` and
in the migration): back to a keyed hash — paired with a re-hash-on-rotation procedure, because the
lifetime problem does not go away — the moment `content_hash` appears on `MemoryRecord` or any
caller-visible DTO, a filter or lookup accepts a caller-supplied hash, or a dedupe query drops the
`application_id` predicate.

**A consequence nobody had recorded, now documented.** Because `conversation_messages.content_hash`
is **both** peppered **and** caller-visible, rotating the idempotency pepper changes the
`content_hash` the API returns **for the same unchanged message**. Not a leak — a contract surprise
on an operational action. It is now stated in the hasher's rotation contract, in the field's
OpenAPI description, and pinned by
`conversation_message_content_hash_stays_peppered_and_changes_on_rotation`.

Guards in `tests/memory_content_hash_rotation.rs`, all three mutation-verified: the rotation proof
builds a second `AppState` on the same database with a different pepper and asserts the dedupe
lookup still matches across it; the negative proof asserts the message hash is still keyed and
*does* change on rotation; and a third executes migration `0021` verbatim against PostgreSQL and
asserts its SQL digest equals Rust's `request_hash`. **The mutation that mattered was not a code
edit at all** — the migration guard only catches a dropped `translate(…, '+/', '-_')` if its
content literal happens to digest to a value using those characters, so an innocent reword of the
string would have left it green through the exact defect it exists to catch. The literal's
suitability is now asserted (`23f8687`).

**Correction to the earlier record:** the finding cited one memory call site. There were **two** —
`create_memory` *and* `patch_memory` — and reverting only the second is the cheapest way back to
F14. Both are covered.

**The dedupe reader this closure was made for now exists** (plan 11 Sub-Phase F,
`feat/plan-11-subphase-f`), so clause (c) has three new call sites and F14's reversal condition has
a new way to be tripped. `find_memory_by_content_hash` compares this exact value across rows;
`find_nearest_memory` and `find_memory_by_key` compare content by other means. All three, plus
`find_memory_candidates`, are now built from **one** `MEMORY_SCOPE_PREDICATE` constant rather than
four copies of the predicate, and `every_memory_read_shares_the_isolation_predicate` asserts on the
**emitted SQL** that each embeds it and binds `m.application_id = $2`. Asserting the SQL rather than
the behaviour is deliberate: a behavioural cross-application test needs two applications whose
contents actually collide, and one written without that collision passes against a query with no
scope at all. Mutation-verified — emptying the application binding in the shared constant turns the
guard red (probe M6, `scripts/p11f-mutate.sh`).

The closure's claim that the reset "would be nearly free today, so it is the wrong choice" is now
settled the right way round: the reader shipped *after* `0021`, so it compares one format only.

### F13 — a duplicate trusted JWT issuer returns 500, not 409 — **FIXED** `a6d2984`

Every other uniqueness conflict in the tree maps to a 409 — `auth_provider_settings` has an
`is_unique_violation` → `duplicate_auth_provider` mapping. `trusted_jwt_issuers` has none, so a
duplicate falls through `AppError::Sqlx` to **500 `database_error`**.

Consequence, found while building plan 08's console: an orphaned-issuer retry path cannot recover by
catching a 409, because the 409 never comes. Plan 08 worked around it by listing-then-adopting
rather than create-then-recover, which is the right client behaviour regardless — but the server
shape is still wrong.

Fixed with `duplicate_trusted_jwt_issuer`: the mapping on **both** `create_trusted_jwt_issuer`
implementations (the pool one and the command-transaction one — only the second is on the live
route, but a mapping that exists on one of two identical inserts is a trap for whoever wires the
other), the catalog entry, the `docs/i18n-response-catalog.json` mirror, the OpenAPI 409, and
`a_second_trusted_jwt_issuer_for_the_same_issuer_returns_409_not_500` beside the
`duplicate_auth_provider` test it was missing — the two are the same condition on two tables, and
the reason the gap survived four plans is that nothing ever put them next to each other.

### F21 — a successful invite redemption cannot be replayed, and the API said it could

Found while writing the `Idempotency-Key` round-trip test the wave-2 sweep asked for.

`POST /api/v1/admin/admin-invites/redeem` documented *"a repeated request with the same key and
body replays the stored response instead of creating a second grant"*. It does not, and it cannot:
`redeem_invite` validates **before** the transactional envelope — deliberately, so a policy refusal
never consumes the invite — and by the time a retry arrives the invite is `consumed`, so
`require_redeemable` refuses it with `409 invite_already_consumed` and `AdminCommandRunner::execute`
is never entered. The `Idempotency-Key` is read and can never matter on the success path.

The outcome is safe either way (a single-use invitation cannot produce a second grant), so this is
a **documentation** defect, not a behavioural one — but it is the kind that gets a client written
against it. Corrected in the route's parameter description and asserted by
`an_idempotent_replay_does_not_count_a_second_invitation_or_redemption`.

**It also corrects the wave-2 leftover it was found under.** "`create_invite`/`redeem_invite`
double-count an idempotent replay" is true of `create_invite` and **unreachable** for
`redeem_invite`'s success path. The replay that *is* reachable there is a **failure** replay: a
refusal raised inside the envelope (`admin_identity_already_claimed` from `insert_grant`) is a
cacheable 409, so the ledger stores it, `consume_invite` never runs, the invite stays pending, and
the retry gets past the pre-envelope check to the stored response — arriving as
`AppError::Replayed`. That is where one refused redemption was being counted once per client retry,
turning a denial-rate alert into a measure of the client's retry policy. Both are fixed; the
success-path guard is kept and documented as currently-unreachable, because the property it leans
on lives in a different function and moving validation inside the envelope — which plan 09 §0
originally proposed — would silently reintroduce it.

### F22 — `api_keys.prefix_length` clamped at 12, which is two random characters for an invite token

`ApiKeyHasher::new` clamped with a bare `.max(12)`, a number that knew nothing about the namespace
it would be prefixing. `moira_inv_` is ten characters, so a configured `prefix_length` of 12 left
**two** random base64url characters: 4096 distinct prefixes, colliding on
`admin_invites_token_prefix_active_unique` (an unmapped unique violation, i.e. a 500) and reducing
the anonymous preview endpoint's documented *"no Argon2 work without a valid prefix"* bound to a
4096-guess search.

The shipped default is 20, so this was configuration-only and never live.

**Decision: refuse at startup, do not raise the clamp.** A clamp makes a misconfiguration
*invisible* — the operator sets one value, the process runs another, and nothing says so. That is
the same reasoning `validated_invite_lifetime` already applies to an invitation's lifetime
("refused rather than clamped: an operator who believes they issued a 30-day invite and silently
received a 3-day one discovers the difference at the worst possible moment"), so this follows an
established rule rather than inventing one. `Settings::validate` now rejects anything below
`MIN_API_KEY_PREFIX_LENGTH`, which is **derived** from the longest entry in a new `KEY_NAMESPACES`
table plus `MIN_RANDOM_PREFIX_CHARS = 8`, never written down. The floor in `ApiKeyHasher::new`
stays, retargeted, as a backstop configuration can no longer reach.

Two gates keep the derivation honest, because the constant is only as good as the namespace list:
a source walker over `src/` for inline `generate("…")` call sites, and — for `ADMIN_INVITE_NAMESPACE`,
which is a `const` and therefore invisible to any walker — a `const` assertion that fails the
**build**. The invite namespace is both the shortest and the only one no walking gate could see,
which is exactly why it was the one that broke.

*Reversal condition:* raising `MIN_RANDOM_PREFIX_CHARS` is free. Lowering it, or removing the
startup check in favour of a clamp, needs an argument about the preview endpoint's cost bound —
that endpoint is anonymous, and the bound is the only thing standing between it and an Argon2
CPU-exhaustion oracle.

### F11 — a retention batch could delete its whole table in one transaction — **FIXED** `9799826`

**`limit $1` bounds one *evaluation* of a sub-query, not the statement.** The retention sweep used
`delete … where id in (select id … order by expires_at limit $1 for update skip locked)` and
believed that capped a batch at `$1` rows. It does not.

PostgreSQL may plan that as a `Nested Loop Semi Join` with the sub-query on the inner side and no
`Materialize`, re-executing it **once per outer row**. Each re-execution's `LockRows` skips rows the
current command has already deleted (`TM_SelfModified`), so it returns a *different* victim every
time and the outer scan deletes that one too. The chain continues until the victims run out.

**Verified independently, not taken on report.** Same temp table, same hostile statistics, same
`for update skip locked`, plan confirmed as `Subquery Scan … loops=43`:

| Form | `limit 1` deleted |
|---|---|
| `where id in (select … limit $1)` | **43 of 43** |
| `with victims as materialized (…)` | **1 of 43** |

**Why it hid:** the trigger is a *statistics* state — bulk delete, autoanalyze, then new traffic
before the next analyze. An idle machine never shows it. It surfaced only as `left: 21`/`left: 22`
against a cap of 20 in `tests/retention_worker.rs`, during unrelated work.

**Production impact.** No unexpired row was ever at risk — every extra victim still comes from a
predicate on `expires_at < now()`. The damage is an unbounded *rate*, not corruption. But the
per-tick cap was fiction: one tick could delete the entire expired set in a single transaction,
holding row locks and accumulating WAL for all of it, which is exactly what batching exists to
prevent. The module's documented bound on how long a user-facing `claim_idempotency` can block was
"how long one batch statement runs" — and that statement had no size limit, so neither did the wait.
On `responses` it is worse: `migrations/0011_retention_indexes.sql` records a single 500-row batch at
**12 735 ms** in RI triggers. Every replica runs its own uncapped sweep.

Fixed with `with victims as materialized (…)` — a CTE is evaluated once into a tuplestore whatever
the planner does. `materialized` is load-bearing: PostgreSQL 12+ inlines un-annotated
single-reference CTEs, and an inlined CTE is a sub-query again. The regression test forces the
hostile join shape rather than depending on a statistics snapshot that will drift, and was checked
non-vacuous against the old SQL (`left: 24, right: 1`).

**Process note.** When this first surfaced I said to leave it — pre-existing, plan 05/06 territory,
out of scope for a plan 07 branch. That was wrong, and the agent pushed back correctly. *Scope is
the wrong lens for "an assertion that should be unreachable just fired twice."*

### F10 — two shared-test-database hazards that can PERMANENTLY wedge a test run

Both currently pass by luck. Found while fixing the plan 07 CI race, and worth separating from it:
that race merely failed intermittently, whereas these two can leave the shared database in a state
where a test fails *forever* until someone deletes a row by hand.

1. **`tests/retention_worker.rs:329,333-334` — pre-existing, plan 05/06 territory, NOT fixed here.**
   `retention::run_once` is a cluster-wide sweep, but the test asserts *exact* equality on the delete
   counter. `sweep_guard` serialises retention tests against each other and nothing else. It seeds
   century-backdated rows with no cleanup path, so a failure leaks rows that sort ahead of the next
   run's under `order by expires_at` — self-poisoning. Deliberately left alone: changing retention
   test semantics is a larger, riskier change than belonged in a regression fix on the plan 07 branch.
   — **CLOSED** 2026-08-02 `fix/shared-db-flakes`; see the sub-entry below.

2. **`tests/http_middleware_contract.rs:469`** — creates a `system_api_keys` row per run and never
   deletes it. Minor; no current assertion depends on the count. — **CLOSED with F26**
   `fix/test-row-leak`. It was the same shape as F26 and took the same one-line fix: the fixture now
   owns a `support::TestDatabase`. Two corrections to the record above, both found by measuring:
   the row is not merely a row but an **`active` API key scoped `moira:admin`**, and **42** had
   accumulated. The test cannot delete it — the response body is the only place the secret ever
   exists, and asserting on that *is* the test — so a disposable database, not a cleanup, is the
   only fix available. Item 1 (`retention_worker.rs`) remains open and is now the **last** suite
   leaking to the shared database; a private clone would scope `retention::run_once` to one database
   and dissolve both its global advisory lock and its exact-equality hazard at once.

#### Item 1 — **CLOSED** 2026-08-02, `fix/shared-db-flakes`. The self-poisoning was measured, not argued

**The leak, reproduced.** A `panic!` injected into `retention_run_respects_the_configured_batch_size`
after its 43 rows are seeded and before any cleanup, against the pre-fix suite:

```
$ psql moira -c "select count(*), min(expires_at)::date from idempotency_records"
 43 | 1926-08-27
```

**The poisoning, reproduced.** The *unmodified* pre-fix suite was then run against that database and
failed on its own:

```
assertion `left == right` failed: the remainder of the backlog must be left for the next tick
  left: 43
 right: 23
```

All 43 of its own rows survived, because the 43 leaked 1926-dated rows sorted ahead under
`order by expires_at` and absorbed the entire 20-row per-tick cap. And it **escalates**: each failed
run leaks 23 more, measured at **43 → 66 → 89** across three consecutive runs, with no code path
anywhere that would ever clean them up. That is the finding's "permanently red suite" demonstrated
end to end rather than predicted.

**The fix, verified the same way.** The suite now uses `support::TestDatabase`. The identical
injected failure leaves **0 rows in the shared database and 0 leftover fixture databases** — the
whole database is discarded in `Drop`, on a dedicated thread with its own runtime, so it runs while
the test unwinds.

**Isolation does not invalidate the suite, and the allowlist's stated reason for keeping it was
wrong.** `SHARED_DATABASE_ALLOWLIST` recorded that `run_once` is "a cluster-wide sweep" and that a
private clone would therefore change retention test semantics. A PostgreSQL connection is scoped to
one database: the sweep is **database**-wide, and it is database-wide on a clone over the identical
code path. No test in the file asserted that a sweep reaches rows it did not itself seed — the
module comment said the *opposite*, that every assertion is scoped by id. What isolation removed was
other suites' rows, which were never the subject. The entry has been deleted, which
`every_allowlist_entry_is_still_load_bearing` forced rather than merely permitted.

**What isolation bought, beyond the leak.** Every `>=` became `==`; whole-table counts were added,
which catch a sweep deleting *more* than it was asked to and which no number of by-id existence
checks can; the cluster-wide advisory lock serialising every retention sweep in every test binary
and every worktree on the machine is gone, and the suite went from serialised to **2.86 s** for
eight tests; and the century backdating — the thing that made a leak permanent — is gone with it.

**Teeth, by injection.** Each mutation reverted after observing the failure:

| Mutation | Guard that fired |
|---|---|
| `RetentionPlan::next_batch_size` ignores the per-tick cap | `retention_run_respects_the_configured_batch_size` — *"a backlog larger than the per-tick cap must be reported as capped, got RetentionOutcome { idempotency_records_deleted: 43, batches_run: 45, hit_per_tick_cap: false }"* |
| `TestDatabase`'s pool pointed at the shared database | `the_fixture_owns_a_disposable_database` — *"the retention suite is sweeping the shared test database `moira`"* |
| stale `retention_worker.rs` entry re-added to the allowlist | `every_allowlist_entry_is_still_load_bearing` — *"is on SHARED_DATABASE_ALLOWLIST but no longer resolves var(…). Delete the entry"* |

**One incidental gap closed.** The suite's own skip message was `skipping retention worker tests:
MOIRA_TEST_DATABASE_URL is not set`, which matches **neither** pattern in `scripts/gates.sh`'s
skip assertion (`skipping database` / `set MOIRA_TEST_DATABASE_URL`). A silent skip here was
invisible to the gate built to catch silent skips. `TestDatabase` prints `skipping database-backed
tests: …`, which matches.

*Reversal condition:* reopens if any suite outside `SHARED_DATABASE_ALLOWLIST` resolves
`MOIRA_TEST_DATABASE_URL`, or if `the_fixture_owns_a_disposable_database` is deleted from any of the
three suites that carry it. *Known limit, unchanged from F27:* the scan is **per-file and textual**,
so the cheapest remaining bypass is a helper added to the allowlisted `support/mod.rs` that hands
out a shared-database pool. Nothing would notice.

**The general lesson.** `migrated_pool()`-style helpers hand every `#[cfg(test)]` module in `src/` the
*same* database, and `cargo test --workspace` runs binaries concurrently against it. Any test writing
a singleton, a globally-unique slot, or a cluster-wide counter is sharing mutable state with every
other test in the tree. The integration suites avoid this with `support::LifecycleFixture`'s cloned
databases; the lib tests have no equivalent, so they need an explicit advisory lock. **A test that
leaks a row on the panic path is worse than a flaky one** — it converts one bad run into a permanently
red suite. **No integration suite writes to the shared database any more**; the `src/**/tests` unit
tests still do, and are the reason the `moira` database must keep existing.

### F7 — the "no `rig_core` under `src/domain/`" rule has no automated gate — **CLOSED** `d7580a6`

Closed by `tests/rig_boundary.rs`. Teeth verified by injection, not assumed. Original finding:

Plan 06 Module 7 verified it by a one-off `grep`, and the coordinator subsequently described it as
enforced by a test. **It is not.** There is no source-scanning test for it — the only source-scanning
tests are `supply_chain_policy.rs`, `security_foundation.rs` and `openapi_drift.rs`, none of which
check imports. The rule currently holds by absence alone, so a future edit reintroduces the leak
silently.

`c546f08` writes it into the skills as a checkable invariant (`grep -rl rig_core src` must return
exactly two paths) rather than claiming a gate that does not exist. **Cheap to close properly:** one
test in the `supply_chain_policy.rs` style. Recommended for plan 07.

### F8 — `authz` fails OPEN on unknown actor types — **CLOSED** `8039c53`

Fixed as described below. Both `has_scope` and `can_grant` now consult an explicit
`ADMIN_IMPLYING_ACTOR_TYPES` allow-list (`DevAdmin`, `SystemKey`, `TrustedJwt`); a variant absent from
that list is denied implication. `admin_implication_is_denied_to_actor_types_not_on_the_allow_list`
pins both directions. One real behaviour tightening: an `Anonymous` actor carrying `moira:admin`
previously received implication and no longer does — nothing depended on it. Original finding:

`AuthorizationService::has_scope` (`src/security/authz.rs:116-120`) and `can_grant` (`:145-148`) do
not match on `ActorType`. They test a single negated equality:

```rust
scopes.contains(required_scope)
    || (actor.actor_type != ActorType::ConsumerKey && scopes.contains(ADMIN_SCOPE))
```

So admin implication is **allow-by-default for every actor type except `ConsumerKey`**. Any new
`ActorType` variant added in future silently inherits full admin implication and grant authority,
and the compiler says nothing — there is no match to make non-exhaustive.

This is not currently exploitable: the existing variants are all intended to have implication. It is
a **fail-open default** in the authorization core, one variant away from becoming a privilege
escalation. Plan 07 proposes adding exactly such a variant (`ActorType::SetupToken`) and its own text
claims it will "update the two `ActorType` matches" — matches that do not exist. Implemented as
written, `SetupToken` would receive full admin authority: the precise opposite of the plan's intent.

**Fix (small, worth doing regardless of plan 07):** convert both to an explicit allow-list —
`matches!(actor.actor_type, ActorType::SystemKey | ActorType::TrustedJwt | ActorType::DevAdmin)` —
so a new variant is denied by default and adding one forces a deliberate decision.

### F9 — a `moira:admin` grant on a trusted JWT would reach the PUBLIC API, unanalysed by plan 07

`authenticate_caller` (`src/security/auth.rs:385-387`) calls `authenticate_trusted_jwt` for a bare
bearer token and returns that actor **verbatim**, `ActorType::TrustedJwt`. The consumer-key+JWT path
(`:375`) is safe — `combine_consumer_and_jwt` intersects scopes and strips `moira:admin` — but the
bare-JWT path does not.

Plan 07 grants `moira:admin` to trusted-JWT identities. Combined with F8's admin implication, a
granted human calling `POST /api/v1/responses` with only their bearer token would satisfy **every**
scope check — including `moira:execution:override-credential`, `override-model`, and
`moira:identity:delegate`.

That may even be intended, but the plan neither states it nor tests it, and its
backward-compatibility claim covers only actors *without* a grant. Its reviewer checklist inspects
the dev-trust-header branch (genuinely unreachable — `actor_from_trusted_headers` bypasses
`authenticate_trusted_jwt` entirely) and misses the branch that matters.

**DECIDED — the grant applies on the admin plane only.** Taken by the loop on 2026-07-26 after the
user declined to arbitrate; recorded here so it is reviewable and reversible rather than implicit.

The grant lookup goes in `authenticate_admin` (`src/security/auth.rs:325-327`), applied to the actor
*after* `authenticate_trusted_jwt` returns — **not inside `authenticate_trusted_jwt`**, which both
planes share. A granted human therefore administers Moira through `/api/v1/admin/*` and calls
`POST /api/v1/responses` with only the scopes their consumer key or JWT already carries.

Rationale: the alternative makes one grant silently confer `moira:execution:override-credential`,
`override-model` and `moira:identity:delegate` on the public surface. `combine_consumer_and_jwt`
(`:941`) already strips `moira:admin` on the consumer+JWT path, so admin-plane-only is the direction
the existing code was already going; letting a bare JWT carry admin onto the public API would be the
one path that disagrees. CONVENTIONS §7.1 keeps the admin and caller planes separate.

Plan 07 must carry a test asserting a granted identity gets 403 on a public-API scope it does not
independently hold. **Reversible:** if the console later needs it, move the call site — but that
should be a deliberate change with its own test, not a default.

---

## DECISIONS TAKEN BY THE LOOP — reviewable, reversible

These were resolved without the user because the loop holds full autonomy and stopping would have
stalled the plan order. Each names what would have to change to reverse it.

### D1 — plan 07 cuts the setup-token credential path (deferred, not deleted)

Plan 07 proposes two credential paths for `POST /api/v1/admin/setup/claim`: `X-Moira-System-Key` and
a `setup_token` body field, the latter introducing `ActorType::SetupToken` plus its own table.

**Plan 08's console never sends `setup_token`.** Verified: `setup_token?: string` appears in its
TypeScript DTO (`plans/08-…:697`) but every call in 08's frozen flow uses the system-key header for
the whole setup triple (`plans/08-…:701-706`). The path is defined and never exercised.

So it is cut from 07's scope: one fewer table, one fewer `ActorType` variant, ~8 fewer tests, and no
new variant to reason about against F8's allow-list. The DTO field stays optional in the schema so
08's generated client still typechecks, and 07 documents the deferral rather than pretending the
feature exists. **Reverse by:** re-adding the module if a bootstrap flow without a system key is
needed — but that is a product decision, not a prerequisite for LOGIN.

### D2 — the `TODO(plan-07)` fingerprint removals stay, and get retargeted

Four markers (`src/application/runtime_admin.rs:793,888`, `src/application/public.rs:1061,1938`) ask
for the `legacy_actor_fingerprint` read-fallbacks to be deleted. Their own stated precondition is
**24h after the *deploy* carrying plan 06 Module 16** — the `expires_at` window on
`idempotency_records` — not 24h after merge. Plan 06 merged 2026-07-26 and there is no evidence of a
production deploy.

Removing them early means a client retrying an idempotent request across the deploy boundary misses
its ledger row and **executes twice**. The fallbacks cost one extra index probe on a miss. They stay;
the markers are retargeted off plan 07 so 07 is not credited with work it must not do.
**Reverse by:** confirming the deploy date, then deleting the fallbacks 24h after it.

### D3 — plan 11 Sub-Phase F runs extraction inline, not on the worker queue

Sub-Phase E's summarization is specified as *enqueued* to keep it off the response path, and the same
latency argument applies to extraction — it is a second completion call per assistant turn, so an
application that enables it pays roughly double per turn.

It is nevertheless **synchronous**, because both alternatives available in this tree are worse.
`WorkerRegistry::run_supervisor` wires `queue::StubJobDispatcher`, so an enqueued extraction would be
claimed and dropped: a feature whose work never runs, behind a flag that says it does, which is
P0-1's exact shape and the thing plan 11 exists to remove. A detached `tokio::spawn` would outlive
the request and its pool guarantees and could not be asserted on without a `sleep`, which
CONVENTIONS §3 forbids and finding P2-12 is about.

The cost is bounded by the flag: `automatic_extraction_enabled` defaults to `false`, so no existing
application pays anything, and the doubling is documented in `docs/memory-extraction.md`.

**Reverse by:** moving the body behind `memory-extraction-retry` the moment a real `JobDispatcher`
replaces the stub. `extract_memories` already takes only ids and reads everything else from the
database, so the move is a call-site change rather than a rewrite.

### D4 — Sub-Phase F reads BOTH consent columns and takes the more restrictive

`application_memory_policies.consent_mode` and
`application_conversation_policies.memory_consent_mode` are independent columns over the same four
values, both defaulting to `'explicit_only'`. Nothing in the schema or the code makes them agree, and
plan 11's Sub-Phase F text names only the first.

Reading either alone is a real defect in **both** directions: read only the memory policy and an
operator who set the conversation policy to `'disabled'` still gets memories extracted; read only the
conversation policy and the reverse. `effective_extraction_status` returns the minimum over the
lattice `refuse < candidate < active`. Mutation-verified in both directions (probes M1 and M2), at
the unit *and* e2e layers — a unit test alone would not have shown the branch was wired.

**Reverse by:** a product decision that one column is authoritative — at which point the other should
be **removed**, not left meaning nothing.

---

## Plan order (forced)

`02b → 03 → 04 → 05 → 06 → 07 → {08 ∥ 10} → 11 → 09`

02a, 02b, 03, 04, 05, 06, 06b, 06c all merged to `main`. **07 is next**, in Wave 0 (plan rewrite).
Next free migration numbers are **`0012` and `0013`** — `0009`, `0010` and `0011` are taken by
`backfill_false_indexed_ingestion_status`, `list_cursor_indexes` and `retention_indexes`, so plan 07's
proposed `0009`/`0010` filenames must be renumbered.

Plan 05 froze the OpenAPI spec: any later route/DTO change must regenerate `docs/openapi.json` via
`UPDATE_SNAPSHOTS=1 cargo test --lib http::tests::committed_openapi_matches_the_generated_document`.

---

## LOOP PERFORMANCE — measured 2026-07-27, supersedes the old `cargo clean` rule

The loop was spending most of its wall-clock on build cost and duplicated verification. Measured,
not estimated:

| Change | Before | After |
|---|---|---|
| `[profile.dev] debug = 1` (`0055c7e`) | `target/` **20 GB**, `deps` 11 GB | **2.0 GB**, `deps` 1.5 GB |
| Full clean rebuild, 405 crates | (avoided — cost minutes) | **2m21s** |
| `cargo test --workspace --all-features`, warm | — | **1m39s**, 622 pass |
| `cargo nextest run --workspace` | — | **2m07s** — *28% SLOWER* |

**`cargo clean` is no longer the enemy, and `CARGO_TARGET_DIR` is no longer forbidden.** Both old
rules were workarounds for artifact bloat that no longer exists:

- **Agents SHOULD now set a private `CARGO_TARGET_DIR`.** Cargo takes an **exclusive lock** on its
  target directory, so agents sharing one cannot compile simultaneously — one builds, the rest
  block. Every "parallel" wave so far was parallel *thinking* and serialised *building*. At 2 GB
  each, three concurrent agents cost ~6 GB. This is the single biggest remaining speedup.
- **Agents should NOT run the full gate set.** Five agents × (full clippy + 622 tests + release
  build) is ~40 minutes per wave re-proving a tree nobody changed. Agents run `cargo check` plus
  their own tests; the coordinator runs all six gates once before the PR. Exceptions: broad `src/`
  changes, and proving a race — where repeated full runs *are* the evidence.
- Prefer `scripts/reclaim.sh` over `cargo clean`: `debug/incremental` is ~45% of the tree and
  free to delete, while `deps` is the expensive half. But at 2m21s to recover, cleaning is now an
  annoyance rather than a lost afternoon.

**nextest was adopted and then demoted.** It is 28% slower here — 621 process spawns each build a
Postgres pool, integration suites clone a template database per test, and the new advisory locks
serialise across processes rather than within one. It is kept as a *secondary* runner because it
forces cross-process contention, which is the case an in-process `Mutex` silently does not cover;
both plan 07 shared-database fixes were re-verified under it. **`retries` is pinned at 0** — the
default would have reported the `setup_state` race as "flaky" and shipped the isolation bug.

Rejected: `[profile.dev.package."*"] opt-level = 3`. It optimises 405 dependencies to speed up test
*runtime*, but this suite is Postgres-I/O-bound, and the cost is repaid on every dependency rebuild.

## STATE AT A GLANCE — update this before every compaction

**Merged to `main` (12 plans):** 02a, 02b, 03, 04, 05, 06, 06b, 06c, **07** (`27b6e0c`),
**10** (`671eadf`), **08** (`f0ecbbc`), **11** (`e898f80`). The last four were CI-verified with every
job running steps.

**Plus the findings sweep — PR #39 MERGED `5206ffd` (2026-07-31), 27 files, +2471/−101.** F20
(single-primary ownership), F13 (duplicate-issuer 409), F21 (replay double-count, closed here and
nobody had noticed), the wave-2 leftovers, and `cargo-mutants` adoption. CI-verified on the exact
merge commit: five jobs, steps executed (`rust` 13, `console` 16, `console-container-and-helm` 14,
`container-and-helm` 13, `supply-chain` 10).

**Migrations: the tree is now at `0024` (F53). Next free is `0025`.** `0016` is a permanent gap.
This line said `0019`/`0020` for four migrations' worth of drift — **derive it with
`ls migrations/ | tail -1` rather than trusting it.**

## ✅ ALL PLAN WORK IS COMPLETE — plan 09 finished 2026-07-31. Only T11 remains, and it is the user's.

**Five PRs merged in one cycle**, each CI-verified with every job running steps:

| PR | What | Merge |
|---|---|---|
| **#39** | findings sweep — F20, F13, F21, `cargo-mutants` | `5206ffd` |
| **#40** | F22 — the non-streaming timeout probe raced a sub-millisecond deadline | `f3a9480` |
| **#41** | wave 4A — deterministic `admission_policy`, `github_oauth` schema, F23/F24/F25/B2 | `c98aeb7` |
| **#42** | wave 4B — per-provider console issuer, N sign-in buttons, GitHub-shaped mock | `da384c8` |
| **#43** | wave 5 — invitations and ownership UI | `820a5a8` |

**The forced plan order `02b → 03 → 04 → 05 → 06 → 07 → {08 ∥ 10} → 11 → 09` is now fully executed.**
~~Migrations end at `0020`~~ — **stale; see "STATE AT A GLANCE" above and derive it from
`migrations/`.** `0016` is a permanent gap. ~~OpenAPI is stable at **151 / 99 / 178**~~ — also
stale; it is **152 operations / 100 paths / 183 schemas**, and F50 added one `RuntimeEventType`
enum member without changing any of the three counts.

**The one piece of plan 09 the loop could not do is T11**, removing the console's
`ambiguous_enabled_providers` guard. It is gated on stage 4A being **deployed**, not merged, and
nothing here can deploy. **It must not be waved through** — until Moira's refusal is running in
production that guard is the only thing in front of F23.

### Findings state after this cycle

**Closed:** F21 (already fixed in #39, unnoticed until an auditor checked), **F23**, **F24**
(structurally, zero `admin_identities` change), **F25**, **F22**, and **B2**.

**F6 is CLOSED** (`f31ff59`) — an allow-list `filter_fn` on the OTLP bridge layer. And closing it
exposed a **fifth** laundering guard: **F16's own mitigation was never wired-tested.** Deleting
`.with(filter_fn(suppresses_provider_payload_logs))` from `init` left **all 598 library tests
green**, because F16's three tests exercised a `suppresses()` helper *re-implemented inside the test
module* — they covered the predicate and never the stack it was meant to be installed in. Fixed in
`8bbda15` by moving stack assembly to `build_subscriber`, which `with_default` can install, and
asking the installed subscriber via `tracing::enabled!`.

**The transferable rule: a predicate test and a wiring test are different tests — and every one of
this project's laundering findings has had a correct predicate.**

**F17 is CLOSED** (`7640829`) — see its entry. And it exposed a **sixth** bad-guard category, distinct
from the five above: `console-jwks-stability.test.ts` was not toothless, it **pinned the defect**. It
asserted the published JWKS was *unchanged* by a rotation and that signing raised the library's
decrypt string — so it went **red on the fix**, which is the opposite of what a guard does. Worse, it
asked the two questions **separately**, and F17 is the *conjunction*: a 200 JWKS is unremarkable on
its own, and a signing failure is unremarkable on its own. Only together are they the outage.

**The rule: when a test documents current behaviour, say so in its name, and never let a conjunction
be asserted as two independent facts.**

**Open, and the queue from here:** ~~the leaked `trusted_jwt_issuers` test rows~~ (**closed as F27**)
and ~~F2 (user-deferred)~~ (**closed** `fix/f2-query-rejection-envelope`, 2026-08-01).
**F6 `f31ff59`, F17 `d8aab3e` and F14 are closed**, and **admin-write/audit non-atomicity is closed as
F26** — but F26 left a
successor: `RuntimeAdminService::record_idempotency` is still non-atomic with its own write at 13
sites, which is a different table and a design change (move `runtime_admin` onto
`AdminCommandRunner`), not a follow-up commit.

**Needs a human — recorded, not implied:** T11's deploy; the **rig-core issue for F16**, which should
go under a person's name; and a **Google credential** if the OAuth mock/live seam ever needs closing —
everything is verified against a real TLS mock IdP with real signed JWTs, and what cannot be proven
without one is Google's own token claims, consent screen and key rotation.

### Plan 09 wave 4, stage 4A — landed on `plan/09-wave4-multi-provider` (2026-07-31)

The decision, both its reversal conditions and corrections C1–C9 to §0.1's blockers are written into
**plan 09 §0.7.7**, which supersedes §0.1's B3/B4/B5/B6/B9 and §0.7.3's W4-D2/W4-D3/W4-D4 without
editing them — the record of what was believed is worth keeping. Read that section, not this entry,
for the reasoning.

**What 4A ships.** `migrations/0020` (the `github_oauth` method + shape CHECK swaps, which must move
together, and the partial unique index `auth_provider_settings_one_enabled_per_trusted_issuer`);
`admission_policy` replacing `governing_policy` with a deterministic two-stage lookup;
`AuthMethod::GithubOauth` through the encoder, the shape validator and the anonymous sign-in
projection; the W4-B2 fix so one undecodable row cannot 500 the anonymous login endpoint during a
rolling deploy; a `trusted_issuer_has_active_grants` guard on the issuer delete/disable paths; and
`checkSession` wired at `jwt.getSubject` — F25's fix.

**What 4A deliberately does NOT ship.** `ambiguous_enabled_providers` stays. The console guard may be
removed only after `0020` and the coded 409 are **deployed**, not merely merged — C5's ordering, and
it is load-bearing. No per-provider issuer, no N buttons, no `provider_id` column.

**No migration contains an unattended `update … set enabled = false`.** An already-ambiguous
deployment fails index creation, the Helm pre-upgrade Job fails, the upgrade aborts and the old pods
keep serving. That is the correct outcome; the operator remedy is in the migration comment. Auto-repair
would be a silent change to who can obtain admin, run with nobody present, that a binary rollback
cannot undo.

**Three lessons worth more than the code.**

1. **A named mutation is a hypothesis, not a result.** G1 was specified as "two bound providers,
   permute `created_at`, restore the old query → the runs disagree". Applied, the test stayed
   **green**: once every provider owns its own trusted issuer the old query and the new one agree,
   because there is nothing left to order. The guard was rewritten with decoy rows and re-verified
   red. Every guard in this wave had its mutation applied by hand and observed; that is the only
   reason this one was caught.
2. **A test harness that reimplements the code under test proves the harness works.**
   `tests/support/console-server.ts` bound `auth.handler` to a socket directly, so every wire-level
   console test bypassed `app/api/auth/[...all]/route.ts` — including the token-endpoint refusal
   those tests exist to exercise. Only the runtime *resolution* is stubbed now.
3. **`auth.api.getSession` mints a token.** better-auth's jwt plugin has an `after` hook on
   `/get-session` that sets `set-auth-jwt`, so `getSubject` runs on a session **read** and a policy
   refusal surfaces as a throw from a getter. Verified in `dist/plugins/jwt/index.mjs`, not assumed.

**Carried into 4B, explicitly:** the T0 spike (can better-auth bind the authenticating
`account.providerId` to the session?) gates everything; `admission_policy`'s stage 2 is now the
legacy/compat path only, because `auth_provider_issuer_shadows_trusted_issuer` steers new
configuration to binding; and one human signing in through two providers will hold **two**
`admin_identities` grants with no column linking them — revocation and `is_primary` become per-grant.


**Remaining: plan 09 only — and it is much larger than it says.** §0 written (`13284f1`) recording
**9 blockers**. Two are structural rather than citation drift:

- **It extends a console UI that plan 08 never built.** Every "reused from plan 08, not
  re-implemented" claim is false — 15 named artefacts do not exist. Waves 2–3 are **greenfield**.
- **It assumes a `console_auth` database that does not exist.** Better Auth is on the in-memory
  adapter under a header labelled "DELIBERATE SCOPE LIMIT"; secrets are an in-memory map; the JWKS
  key pair regenerates every process start; the chart is pinned to one replica. Its "real session
  registry" feature has nothing beneath it.

**And its N-provider premise is a redesign, not an extension.** The console today returns
`ambiguous_enabled_providers` when more than one provider is enabled — deliberately, because "the
console refuses to guess". **Enabling a second provider currently breaks sign-in.**

**Re-sequenced into five waves** (the old Wave 2 ∥ Wave 3 split cannot run — Wave 3's screens have
no foundation to attach to):

1. **Durable console storage** — `console_auth`, Better Auth CLI migrations, a durable
   `ConsoleSecretStore` behind the existing interface, lift `replicaCount: 1`. Non-negotiable:
   secret durability and a stable JWKS stop being optional the moment there is a second provider or
   a second replica.
2. **Moira invite backend** — parallel with 1; one owner end-to-end for the route table.
3. **Console foundations** — the `(console)` group, `middleware.ts`, `/login`, `SignInPanel`, the
   i18n catalog, the architecture guards. **The wave nobody planned for.**
4. **Multi-provider + auth-settings screen** — including removing `ambiguous_enabled_providers`.
5. **Invitations, ownership, sessions.**

**DECISION — session management is cut from plan 09 unless waves 1–4 land comfortably.** It is the
one feature that needs durable storage *and* delivers nothing the invitation flow requires. Shipping
an "active sessions" screen over an in-memory store would be the appearance of a feature.
*Reversal condition:* restore it once durable storage ships and the invitation flow is green.

**Open findings, none blocking a merge:**

| | What | Where it lives |
|---|---|---|
| ~~**F15**~~ | ~~Console cannot render a sign-in button without a credential it can only get by signing in~~ **RESOLVED** — anonymous `GET /api/v1/admin/setup/sign-in-methods`. *Not* by serving `PublicAuthMethod` as recommended: that carries `allowed_email_domains`, the deny-by-default admin-claim policy | `fix/f15-anonymous-auth-methods` |
| **F14** | Memory dedupe silently stops matching after a pepper rotation | plan 11 Sub-Phase F |
| ~~**F13**~~ | ~~Duplicate trusted JWT issuer returns 500, not 409~~ **FIXED** `a6d2984` | `fix/findings-sweep` |
| ~~**F2**~~ | ~~Pre-auth query-field enumeration~~ **CLOSED** — `normalize_infrastructure_error` now envelopes **every** non-JSON 4xx/5xx, not a list of statuses, and discards the rejection text. It was never `Query`-only, and it was violating `docs/openapi.json`'s own `4XX → ErrorResponse` claim, so the snapshot is unchanged. See the F2 section above | `fix/f2-query-rejection-envelope` |
| ~~**F6**~~ | ~~OTel exports every span; `env_filter` is the sole barrier to Rig prompt spans~~ **CLOSED** `f31ff59` — allow-list of Moira-owned targets on the bridge layer, below the `EnvFilter`. The recorded description understated it: Rig's prompt-bearing span is `INFO`, so a bare `info` was already enough | `fix/f6-otel-span-filter` |
| ~~**F26**~~ | ~~Admin write + audit row still non-atomic~~ **FIXED** `3825fb0` — 36 sites, not all of them; 20 were already atomic inside the command envelope. Reachable from an over-long `x-request-id`, not only from a crash. See the F26 section at the end of this file | `fix/admin-audit-atomicity` |
| ~~**F27**~~ | ~~~986 leaked `trusted_jwt_issuers` rows in the shared test DB~~ **CLOSED** `fix/test-row-leak` — the count was **160**, not 986; see F27 below | hygiene |

**Test baseline:** 779 passing on plan 11's branch (744 on `main` after plan 10).

## MUTATION TESTING PAID FOR ITSELF ON ITS FIRST RUN — 2026-07-31

`cargo-mutants` on the findings sweep: **63 mutants, 9 missed, 25 caught, 29 unviable**, 2 hours at
`-j 2`. **All nine survivors were real gaps in code and tests written that same day**, by an agent
that had already been told this project has six laundering findings.

The worst one re-creates the finding the sweep existed to fix, through a different door:

> `set_primary`'s `is_primary && !current.is_primary` → `||` means a `PATCH {"is_primary": false}`
> on a grant that **never owned anything demotes the actual owner**. A `200 OK` that returns the
> deployment to the ownerless state F20 is about — and it walks past the last-primary guard, because
> that guard inspects only the row being written.

**Nothing in 887 tests caught it.** All nine are now killed, each verified by re-applying its
mutation by hand.

**Not wired as a CI gate.** Two hours for a ten-file change is a reviewer's tool, not a PR blocker.
Measurement and reversal condition in `docs/mutation-testing.md`.

**The lesson is about scope, not tooling.** These were fresh, carefully-written tests for
security-critical code, reviewed by an agent primed to look for exactly this. Mutation testing is
not a backstop for careless work — it is the only thing that reliably distinguishes a test that
passes from a test that would fail.

## F21 — the OpenAPI claimed a successful redemption could replay; it cannot

Pre-envelope validation refuses a consumed invite **before** the envelope is entered, so a
successful redemption can never replay. The spec said otherwise. Corrected.

The reachable double-count is a **failure** replay — `admin_identity_already_claimed` →
`AppError::Replayed` — which distorts an operator-facing denial rate rather than an invitation count.
That is the one worth fixing, and it is not the one the sweep brief named.

**CLOSED — already fixed in PR #39, which nobody had noticed.** `redeem_invite`'s `Err` arm carries
`if !matches!(error, AppError::Replayed(_))`, and
`an_idempotent_replay_does_not_count_a_second_invitation_or_redemption` asserts the counter reads
`1.0` after a replayed cacheable 409. **That test works** — it reads `2.0` with the guard removed.

Found by the wave-5 auditor while checking whose wave F21 belonged to. It belongs to **neither**: it
is a wave-2 defect by domain and was closed by the findings sweep. §0.7's W4-D6 would have
**double-implemented it**, which is the cost of routing a finding by "which file is already open"
rather than by checking whether it is still open at all.

## STANDING RISK (2026-07-31) — GitHub stopped firing `pull_request` runs; use `workflow_dispatch`

**`pull_request` events stopped producing workflow runs on this repository** some time after
08:44 UTC on 2026-07-31, while `push` runs on `main` kept working normally throughout.

PR #39 accumulated **zero check-runs** (`/commits/<sha>/check-runs` → `total_count: 0`) across all
three of: a `synchronize` from a real push, a close/reopen, and a fresh empty commit. The PR timeline
records every one of those events, so GitHub received them and produced nothing.

**Everything that could explain it was checked and ruled out:** the workflow is `active`;
`actions/permissions` is `enabled: true, allowed_actions: all`; `ci.yml` is **byte-identical** on
`main` and the branch (`git diff` empty), with no `paths` filter and no blocking job-level `if`; no
commit message carries a skip directive; the repo is public, so Actions minutes are unmetered; and
`pull_request` runs demonstrably worked on this same workflow as recently as 08:44.

**Resolved honestly, not worked around.** `64d44ec` adds `workflow_dispatch:` to `ci.yml`, so the same
six jobs can be run against an arbitrary ref: `gh workflow run ci --ref <branch>`. The dispatch uses
the workflow file **from the target ref**, so the change must exist on the branch too — cherry-picked
as `3093c3c`.

**The alternative was merging PR #39 on local gates alone, and that was refused.** The old
infrastructure override is void because CI works; here CI *does* work, just not through the event that
normally reaches a PR. A dispatched run executes the identical jobs against the identical tree, so it
is real verification rather than an override. **Never merge this PR on local gates because the event
did not arrive** — dispatch instead.

## F23 — ESCALATED: `governing_policy` can enforce the WRONG admin-admission policy, and cannot enforce a per-provider one at all — **QUERY-SIDE CLOSED** by plan 09 wave 4A

> **Status (2026-07-31).** All three reachable shapes are closed in Moira's layer.
> `governing_policy` is **deleted**; `admission_policy` resolves the bound provider first and
> reaches the `issuer = $1` branch only for *unbound* rows, refusing a duplicate set with
> `409 duplicate_enabled_provider_for_issuer` rather than taking the first of it.
> `migrations/0020` makes shape (a) unrepresentable and
> `auth_provider_issuer_shadows_trusted_issuer` refuses to store shape (b). The mitigation this
> finding mandated — a partial unique index plus a coded 409 in `create`/`set_enabled` — shipped
> as specified, and the console's `ambiguous_enabled_providers` guard **stays** until 4A is
> deployed, per this finding's own ordering rule.
>
> **The structural half is NOT closed and is not closable at this layer:** Moira still cannot see
> which upstream IdP authenticated a user. Plan 09 §0.7.7's Option A′ routes around it by giving
> each provider its own console-minted issuer (stage 4B); it does not make Moira an independent
> observer. F24 remains open for the same reason.

**Raised by the wave-4 re-audit as W4-B1; six adversarial verifiers then corrected its scope, raised
its severity, and found the audit had it partly wrong in four ways.** Verified empirically against
the real schema in rolled-back transactions, not argued from reading SQL.

```sql
select id, allowed_email_domains from auth_provider_settings
 where deleted_at is null and status = 'active' and enabled
   and (issuer = $1 or trusted_jwt_issuer_id = $2)
 order by (issuer is not distinct from $1) desc, created_at asc, id asc
 limit 1
```

On a console deployment `$1` is the **console's** issuer while every provider row's `issuer` holds the
**IdP's**, so rows match only through `$2`, tie on the first sort key, and the oldest row bound to
that trusted issuer supplies `allowed_email_domains` for **every** claim and redemption — regardless
of which provider authenticated the user. Three reachable shapes:

- **(a)** ≥2 enabled rows share one `trusted_jwt_issuer_id` and none has `issuer = $1`. They tie;
  `created_at asc` decides. Reproduced independently by three verifiers; flipping `created_at` flips
  the governing policy.
- **(b)** *Not in the audit.* Any enabled row whose own `issuer` equals `$1` outranks the correctly
  linked row **at any age**, and need not be linked at all — plausible for a `jwks` row registered
  against the console's own issuer string.
- **(c)** The intended row is linked to a *different* trusted issuer, never enters the set, and a
  single wrong row is returned.

**Scope correction — the audit was wrong.** It is **not** "the oldest enabled row in the table". An
unlinked row carrying the IdP's issuer returns **0 rows** (verified); rows on different trusted
issuers never compete. It is the oldest enabled row *bound to that one `trusted_jwt_issuer_id`*.

> **RETRACTED — this entry originally accused the audit of misquoting the ORDER BY by omitting
> `id asc`. That was false, and the error was the coordinator's.** §0.7 quotes all three keys
> correctly in at least three places (its drift row 7, W4-B1's prose, and §0.1 B5). The truncation
> was introduced by **the coordinator's own brief to the verifiers**, which quoted the clause as
> `(issuer is not distinct from $1) desc, created_at asc`. Three verifiers correctly noticed the
> missing key and reasonably attributed it to the audit; the coordinator then propagated that into
> this finding. Caught by the wave-5 auditor, which read §0.7's actual text instead of trusting the
> correction it was sent.
>
> `id asc` is real and load-bearing — `created_at` ties exactly for rows inserted in one transaction,
> and `id asc` is what makes the result deterministic. The audit had it right all along.
>
> **The lesson is about verification inputs, not about verification.** A verifier can only falsify
> what it is shown. Quoting a claim *into* a brief is itself a transcription step, and an error there
> is laundered into a confident, multiply-confirmed finding — three independent lenses agreed
> precisely because all three were reading the same corrupted quote. **Point verifiers at the
> artefact, not at a paraphrase of it.**

**Severity: a defect gated behind a fully supported operator action — reachable TODAY, not
wave-4-only.** The audit called it gated by the console's `ambiguous_enabled_providers` refusal.
That is wrong three ways, all confirmed:
1. `governing_policy`'s only callers are `AdminIdentityService::claim` (system-key auth) and
   `redeem_invite` (any registered trusted JWT). **Neither reads any console state.**
2. `consoleRuntime` resolves its snapshot **once per process**; a running console keeps minting
   tokens after a second row is enabled. The file header's promise of per-request refresh is not
   implemented.
3. **Moira has no server-side refusal.** `create`/`set_enabled` do no cross-row check and the only
   unique index is `(method, coalesce(issuer,''))`. Two enabled rows on one trusted issuer insert
   cleanly.

**Blast radius.** The deny-by-default gate is enforced from the wrong row in *both* directions,
silently. The common failure is **availability** — a narrower or empty allow-list 403s every claim
and redemption, and on a deployment that retired its system key that permanently blocks admin
onboarding. It is **not** an unauthenticated path to admin: `claim` still needs the bootstrap system
key and `redeem` still independently enforces the invitation's own email/domain constraint.
**Medium overall; High on the lockout axis.**

**The structural finding, which is bigger than the query bug: Moira cannot see which upstream IdP
authenticated a user.** Confirmed on every surface — the console mints `iss: env.bffIssuerUrl`
unconditionally (one issuer per console, never per provider) and forwards only
`{iss, sub, email, email_verified}`; `trusted_jwt_issuers` has claim-mapping columns for
subject/user/tenant/application/roles/scopes but **no slot a provider identifier could map into**;
`TrustedJwtIdentity` is `{issuer, subject}`; `admin_identities` records no provider column. So
per-provider `allowed_email_domains` is **unenforceable at Moira's layer by construction**. No
ordering can select on information the query never receives — which means "fix the ORDER BY" is the
wrong fix.

**Mitigation wave 4 must ship regardless of which design it picks:** a partial unique index on
`(trusted_jwt_issuer_id) where enabled and status='active' and deleted_at is null`, plus a coded 409
in `create`/`set_enabled`. That turns a silent wrong-policy into a refusal at configure time and
makes the ordering stop being load-bearing. **This is the opposite of plan 09 §0.1 B4's scheduled
"remove the console's ambiguity guard": the guard may only be removed once Moira itself refuses the
ambiguous state.**

## F24 — ESCALATED: two IdPs returning the same `sub` collapse into ONE admin grant

Surfaced by the synthesis, not by any single verifier, and it is worse than F23.

`admin_identities`' uniqueness key is `(issuer, subject)` (`migrations/0012`), where `issuer` is the
**console's** and is therefore identical for every provider. With N providers minting under one
console issuer, two different IdPs returning the same `sub` string map to the **same admin grant**.
GitHub subjects are short numeric strings; a generic-OIDC IdP returning a numeric `sub` collides.

**Consequence:** an identity on provider B can land on an admin grant established for a different
human on provider A. This is a cross-provider identity-confusion hazard, not merely a policy bug.

**Wave 4 must not enable a second provider under one console issuer without resolving this.** It is
the strongest argument for giving each provider its own console-minted issuer and its own
`trusted_jwt_issuers` row, which would make `governing_policy`'s `$2` a real discriminator and close
F23 and F24 together.

## F25 — the console's email-domain enforcement is tested, passing, and DEAD CODE — **CLOSED** by plan 09 wave 4A

> **Status (2026-07-31).** `checkSession` is wired at `jwt.getSubject` in
> `console/lib/auth.ts` — the single function every minted Moira-bound token passes through,
> including every server-side `mintMoiraToken` — plus a keyed 403 on the token route and the
> `(console)` layout gate. Guarded two ways, both mutation-verified: an architecture test
> (`guard-reachability.test.ts`) that fails when the call site is deleted **while every
> assertion in `moira-session.test.ts` stays green** — that divergence *is* the finding — and a
> wire-level test in which a human outside the allow-list completes OAuth and then gets 403 with
> no token. A page-level-only wiring was verified to fail both: the token endpoint returned
> **200 with a working credential** for exactly the session the page would have redirected.

`checkSession` (`console/lib/moira-session.ts`) is the console's session-boundary gate and the only
caller of `isEmailDomainAllowed`. **`checkSession` has no shipped caller.** Verified directly: every
reference outside its own definition is in `console/tests/unit/lib/moira-session.test.ts`,
`console/tests/integration/oauth-flow.test.ts`, or an i18n catalog *description*.

**Not a hole today** — Moira enforces the same policy server-side in `evaluate_claim_policy`, so the
deny-by-default admin-claim gate still holds. The failure is that a user passes console sign-in and
is refused later by Moira.

**Why it is worth a finding.** It is the exact shape §2.3 is about: *a guard nobody has seen fail is
an assumption*, and here the guard is never even reached, while eleven green unit assertions say it
works. It is also load-bearing for wave 4: the design option that moves per-provider enforcement into
the console **must wire this**, not assume it. One verifier asserted the console "already runs the
same allow-list at the session boundary" — the synthesis caught it, and it was verified again here.

## F27 — the leaked-row finding was recorded at 6× its real size, and the leak was the happy path — **CLOSED** `fix/test-row-leak`

**The recorded number was wrong and had been for long enough that nobody rechecked it.** The ledger
and the handoff both said *"~986 leaked `trusted_jwt_issuers` rows"*. Measured: **160**. Not an
estimate — 10 distinct path labels × 16 runs, exactly, with zero rows outside the fixture's shape.
The ten labels are precisely the ten `register_issuer` call sites in `tests/jwks_hardening.rs`
(`scheme`, `ip-range`, `oversized`, `content-type`, `jwk-set`, `slow`, `first-failure`, `retention`,
`oracle`, `warm-cache`). That symmetry is what identified the source beyond argument.

**It was the happy path, not the panic path.** Every run leaked all ten rows whether or not anything
failed: `register_issuer` inserts and nothing anywhere deletes. The panic path leaked the same ten
for the same reason. F10's lesson — *"a test that leaks a row on the panic path is worse than a flaky
one"* — turned out to understate this one, because there was no path on which it did **not** leak.

**Nothing depended on the rows, in either direction — verified, not assumed.** The one test that
counts the table (`jwks_url_resolving_to_a_private_address_is_rejected_at_issuer_registration`,
asserting `0`) scopes by a per-fixture-suffixed `jwks_url`, so deleting the residue cannot satisfy or
violate it. Nothing asserts a uniqueness or a non-empty count that a previous run's rows were quietly
supplying. `trusted_jwt_issuers_issuer_active_unique` is keyed on `issuer`, which carries a
`Uuid::now_v7()` suffix, so no run could ever collide with another.

**The fix is the mechanism, not a cleanup.** Both leaking suites now build on
`support::TestDatabase` — a database cloned from the migrated template per fixture, dropped in
`Drop`. That holds on the panic path because `TestDatabase::drop` does its teardown on a **dedicated
`std::thread` with its own current-thread runtime**: `Drop` is synchronous and `#[tokio::test]`
defaults to a current-thread runtime, so `block_in_place` is unavailable, and this is the only
construction that runs unconditionally while a test unwinds. `tests/support/mod.rs` had already
documented these suites as deliberately excluded from that pattern on the grounds that they "assert
only on rows they created" — true, and beside the point: asserting safely is not the same as leaving
nothing behind.

**Cleanup predicate**, three independent conjuncts, each alone sufficient to rule out real data:
host `idp.invalid` (the `.invalid` reserved TLD, RFC 6761 §6.4 — it can never resolve, so it can
never name a real IdP); a path label from the literal ten above; and a 32-hex-character
`Uuid::now_v7().simple()` suffix. 160 rows matched, 0 rows did not, 0 were referenced by
`admin_identities` or `auth_provider_settings`. No `DROP`, no `TRUNCATE`.

**Left in place deliberately:** 180 `audit_logs` rows (112 `jwks_fetch`, 42 `system_key.create`, 26
`admin_identity.claim`). They are the same residue and are now frozen — both suites write elsewhere —
but `audit_logs` is append-only *by product design*, and deleting from it to tidy a test database is
a precedent worth refusing.

**The guard, and what it does not cover.** Each suite carries
`the_fixture_owns_a_disposable_database`, asserting its pool is on a database that is (a) not the one
`MOIRA_TEST_DATABASE_URL` names, (b) the very database its own `TestDatabase` will drop, and (c)
carries zero pre-existing rows. **(c) alone would be worthless** — "the table is empty" proves nothing
on a database another suite writes to — and is meaningful only because (a) and (b) have established
the database is private and freshly cloned first.

That guard is fixture-scoped, so the cheapest way to reintroduce the finding is to add a *new* suite,
or a new test, that opens its own pool: no fixture-scoped assertion would ever see it. Hence
`tests/test_database_isolation.rs`, which scans every source under `tests/` and fails on any file
outside a documented allowlist that resolves the variable itself. Its allowlist can only shrink
without review — `every_allowlist_entry_is_still_load_bearing` fails on an entry that has stopped
being needed, because a stale exemption pre-authorises whatever file next takes that name. It
assembles its own search pattern at runtime rather than as a literal, so that it is scanned on the
same terms as every other file instead of special-casing itself.

**It has teeth, demonstrated twice.** It caught a real violation before any deliberate mutation: the
first draft of the per-suite guards read `MOIRA_TEST_DATABASE_URL` inline to learn the shared
database's name, and the architecture guard failed them by name. That access now goes through
`support::shared_database_name`.

## F22 — a SECOND flake on `main`, distinct from F5, found because docs pushes run CI

Run `30625512140` on `main` at `653461b` — a **docs-only ledger commit** — failed the `rust` job:

```
every_non_sse_route_group_is_governed_by_the_non_streaming_timeout ... FAILED
tests/http_middleware_contract.rs:644
assertion `left == right` failed: the admin route group is not layered with
RouterPolicy::non_streaming_timeout
  left: 200, right: 504
```

**This is not F5.** F5 is the same *file* but a different mechanism — `connections to
moira_test_template were never released`. Here a request expected to exceed the non-streaming
timeout returned `200` instead of `504`, i.e. the slow path completed before the deadline.

**Why it matters more than one flaky test:** `main` is not reliably green, so "merge on green CI" has
a reliability problem in both directions — a red PR run may be a known flake, and a green one proves
less than it should. Two independent flakes are now known (F5, F22) plus the LISTEN/NOTIFY attach
race just closed. **Do not paper over this by re-running until green** — that is faking a gate by
attrition. Diagnose F22 the way the attach race was diagnosed: make it deterministic first.

**Also worth carrying:** docs-only pushes to `main` run the full CI suite, which is what exposed this.
That is accidental coverage worth keeping rather than optimising away.

## FOUR TOOTHLESS GUARDS IN ONE PLAN — the pattern, and the rule that comes out of it

Plan 09 produced **four** guards that could not detect the defect they were named for. Every one was
found by *running the mutation*; not one was visible by reading the test. Two were specified by a
judged design panel, and two were **already shipped and trusted**.

| | Guard | Why it could not fire |
|---|---|---|
| 4A | G1, policy ordering | migration `0020` made the target state **unrepresentable**, so a fixture of legal rows could not reach it |
| 4B | G9's minted-`sub` assertion | the mutation creates a **fresh account row with the same IdP subject**, so `sub` stays correct while every grant is orphaned |
| 5 | `secret-leak.e2e.ts` | grepped for a literal import path; the real mount is **transitive** (page → organism → modal), so the page's own source never contains the string |
| 5 | route-handler session guard | a **per-file** scan can express "*some* handler is guarded", never "*every* handler" — and the second exported method is exactly where an unguarded endpoint goes |

**The `secret-leak` one is the most alarming.** It was an armed tripwire, named in the plan as the
thing that would stop wave 5 if it mounted the modal — and it was **verified green with the modal
mounted**. A guard that everyone believed was protecting them, that had never been asked to fire.

**The common shape, worth carrying into every future guard:** each was written against the *shape the
author imagined the defect would take* — a direct import, one handler per file, a representable row,
a changed subject — rather than against the property. **Ask of every guard: what is the cheapest
edit that breaks the property while leaving the guard green?** That question found all four; reading
the tests found none.

Two corollaries already earned:

1. **A fix that makes a defect unrepresentable can silently disarm its own guard.** After any
   constraint that narrows what can exist, re-check that the guard's fixture is still *reachable*.
2. **A source-scanning guard must scan the closure, not the file.** Both wave-5 failures are this:
   one needed the transitive import graph, the other needed per-export granularity. Rebuilding the
   second immediately exposed two further extractor bugs (a type annotation's `{` mistaken for a
   body; `export const GET = handle;` having no body at all) — **which only re-mutating revealed.**

## WAVE 5 COMPLETE — PR #43. Plan 09 is finished except T11.

29 tasks; all but three landed. **No Moira change** — OpenAPI unchanged at 151/99/178.

**`/admins` ships per-grant and says so.** F24 means one human across two providers holds two grants
with no linking column, so the screen deliberately does **not** group by email: email is required but
is not the key and is not unique across grants, and grouping on it would manufacture person-level
identity from a non-key attribute — the misattribution W5-B12 warns about, wearing a helpful face.
A test asserts two grants with one email render as two rows. **D3's tertiary reversal condition does
not fire.**

**The a11y gate was vacuous for every route inside `(console)`** — no authenticated Playwright state,
so `page.goto` followed the redirect and audited `/login`. It had been passing while auditing the
wrong page. Now `/login` and `/invite/<fixture>` are genuinely audited behind an explicit
final-URL assertion, and the unaudited set is pinned in both directions.

**W5-D7′ — the a11y gate declares its blind spot instead of failing forever.** §0.8.4 step 24 asked
for a permanently red suite. Rejected: a permanently red merge gate blocks every later change for an
unrelated reason and makes weakening the assertion the path of least resistance — it *loses* the
assertion rather than keeping it. The declared-exemption form fails on the same drift (verified by
two mutations) and merges. *Reversal:* when an authenticated Playwright project exists, delete the
entries and keep the URL assertion.

**Four e2e specs deferred honestly** — `invite-redeem`, `invite-domain-policy`, `ownership-transfer`,
`authorization-denial`. All need a live Moira **and** an authenticated session; with
`MOIRA_API_URL=https://moira.invalid` and no storage state each could only navigate, get redirected,
and assert nothing about the behaviour it names. The denial case is covered at unit level.
*Reversal:* an authenticated storage state with a mock IdP inside the Playwright environment.

**Corrections to §0.8 found while implementing:** `revoke_admin_invite` is `POST …/{id}/revoke` (there
is no `DELETE /admin-invites/{id}`); `delete_admin_identity` returns the record, not 204, so a `void`
typing drops the notice; the ownership `PATCH` reuses `moira.notice.admin_identity_claimed` — there is
no ownership-transfer notice; `patch_admin_identity` also declares `Idempotency-Key`; and
**`SECRET_PROP_PATTERN` matches `token`**, so the invite panel cannot take the token as a prop. An
assertion that `page.content()` omits the token was written, run, and **failed on a correct page** —
Next serialises the dynamic segment into the RSC router state — so the honest property is "never in
visible copy".

## WAVE 4 IS ENGINEERING-COMPLETE — 4A `c98aeb7`, 4B `da384c8`. Only T11 remains, and it is the user's.

**Stage 4B merged** (PR #42, `da384c8`) — per-provider console issuer, N sign-in buttons, a
GitHub-shaped mock. **Console-only**: `git diff main…HEAD -- . ':!console'` was empty, so no Rust, no
migration, no OpenAPI change (still 151 operations / 99 paths / 178 schemas). Five CI jobs green with
steps executed. `ambiguous_enabled_providers` **is still in the tree**, as required.

**The slug decision is better than the one the design specified, and the reason generalises.** The
design said to add an operator-chosen slug column, warning that deriving issuer strings from the
provider row UUID would silently revoke a provider's admins on delete-and-recreate — a footgun 4A's
`trusted_issuer_has_active_grants` guard "does not cover". The implementer instead anchored provider
identity to the **tail of `trusted_jwt_issuers.issuer`** (`${bffIssuerUrl}/idp/<slug>`): a string
already operator-chosen, already uniquely indexed, already what Moira pins `Validation::set_issuer`
to — and **already behind that guard**. Anchoring one level up put provider identity *inside* the
existing protection instead of beside it, at zero schema cost. *Reversal condition, recorded in the
code:* a console issuer outside `${bffIssuerUrl}/idp/*` requires a real slug column with a migration,
uniqueness rule and format CHECK.

**A SECOND toothless guard was caught, in the same design document.** G9's specified assertion list
had three members; **the minted `sub` has no teeth.** Under the provider-id mutation the flow creates
a fresh `account` row carrying the same IdP subject, so `sub` stays correct while every pre-existing
grant is orphaned. Only "`readIdpSubject` still finds the account" and "mints that exact `iss`"
discriminate. Recorded in the test.

Its G9 mutation was also **under-specified** — "apply the derived scheme to the legacy row" has two
spellings at two call sites with different blast radii, one a total outage and one silent mass
revocation. Both were applied.

**The 4A lesson propagated correctly, which is the point of writing it down.** Asked whether
`ambiguous_enabled_providers`'s condition should change to count per-trusted-issuer, the implementer
refused and gave the right reason: `0020` makes two enabled rows on one trusted issuer
**unrepresentable**, so such a guard could never fire — a guard whose premise the schema forbids,
reading as protection for the multi-provider case it silently permits. **It should be deleted when 4A
deploys, not rewritten.** That reasoning now sits at `ambiguityGuard`'s doc comment as a removal
condition plus an explicit "do NOT fix it this way".

**Three more corrections to the design**: `definePayload` performs no account read (what it shares
with `getSubject` is one *provider resolution* over the same session object — the property holds, the
named mechanism does not); `ConsoleRuntime`'s cache key had to start hashing the **trusted-issuer**
rows, or repointing a provider keeps an instance alive minting the previous issuer string, which is
the wrong grant *namespace* rather than a stale label; and GitHub is not a test-harness-only item —
shipped code needed a `getUserInfo` override because better-auth defaults `email_verified` to `false`
without an `id_token`, so a configured GitHub row would otherwise offer a button that can never
complete.

**GitHub's email handling is deliberately strict:** the address comes only from a `primary && verified`
entry in `/user/emails`. `GET /user`'s public profile address is attacker-settable, and implicit
linking would attach it to an existing admin's user row.

## ⚠️ BLOCKED ON THE USER — T11 needs Stage 4A DEPLOYED, and this loop cannot deploy

**Wave 4 cannot be finished by the loop.** Stage 4A merged (`c98aeb7`), but its last task — **T11,
removing the console's `ambiguous_enabled_providers` guard** — is gated on 4A being **deployed**, not
merely merged, and nothing in this loop can deploy.

**Why the gate is real and must not be waved through.** Until Moira's own refusal (`0020`'s partial
unique index and the coded 409) is *running in production*, the console guard is the only thing in
front of F23. Remove it in the same release and any rollout that lands the console before Moira opens
exactly the window 4A exists to close. The correct sequence is **4A in release N, T11 in release
N+1**.

**What the user needs to do:** deploy the release containing `c98aeb7`, then T11 can land. Until then
Stage 4B ships the multi-provider plumbing with the guard still in place — the capability is built and
tested but dormant, which is the honest way to stage a change whose safety depends on release order.

This is recorded rather than worked around. Nothing here fakes the gate.

## THE JUDGED DESIGN PANEL SPECIFIED A TOOTHLESS GUARD, AND ONLY THE MUTATION CAUGHT IT — 2026-07-31

**G1's named mutation left G1 GREEN.** The guard was "policy selection does not depend on row order",
and its prescribed mutation was "restore the old `governing_policy`". It passed.

**Why:** with per-provider trusted issuers, `trusted_jwt_issuer_id = $2` selects exactly one row, so
there is nothing for an `ORDER BY` to arbitrate. **F23 shape (a) is unrepresentable after `0020`** —
the migration that makes the defect impossible also makes a guard built from *legal* rows unable to
reach the defect it is named for. Rebuilt with decoy rows inserted by raw SQL, because the API now
refuses that shape, and re-verified red.

**This is the project's most expensive recurring defect appearing inside the fix for it.** The guard
was specified by a judged panel of three designs and three judge lenses, written by an implementer
primed on seven prior laundering findings, and it would have shipped green — a test named for a
property it could not observe. Nothing but applying the mutation by hand would have found it.

Two general lessons, both worth more than the specific bug:

1. **A fix that makes a defect unrepresentable can silently disarm the test that guards it.** After
   any constraint that narrows what rows can exist, re-check that the guard's fixture is still
   *reachable* — a guard whose premise the schema now forbids asserts nothing.
2. **"Verified by a panel" is not verification.** Three designs, three judges and a synthesis all
   passed this through. The mutation is the only step that touched reality.

## D-W2-1 — recovery invites were deliberately NOT built in wave 2 (recorded 2026-07-31, retroactively)

**This decision was taken in plan 09 wave 2 and recorded nowhere a planner would find it.** Its only
written trace was a comment in `migrations/0017_admin_invites.sql` and two comments in the i18n
catalogues:

> *"a column no code writes is the schema equivalent of a catalog entry with no emitter."*

Wave 2 shipped the invitation backend without `is_recovery` and without
`replaces_admin_identity_id`. There is no column, no DTO field, no route, no service method, no error
code, no notice (`src/i18n/catalog/notices.rs` says "deliberately no `admin_identity_recovered`
notice"), no audit event (`ADMIN_IDENTITY_GRANT_EVENTS` is exactly
`["granted","revoked","ownership_transferred"]`) and no test.

**Why it belongs here rather than only in a migration comment.** It removes a *third of wave 5's
stated scope*. `RecoveryPanel`, `recovery.e2e.ts`, `recovery_invite_gets_no_domain_policy_exemption`
and the `admin_identity_recovered` event are all unbuildable, and a wave-5 implementer reading the
plan body would have discovered that only by trying. The general rule this produces: **a decision
that changes a later wave's scope must land in that wave's §0 and in this ledger, not only where it
was taken.**

## W5-D1 — recovery is CUT from plan 09 wave 5 (taken by the loop, 2026-07-31)

Wave 5 ships **invitations and ownership only**. Per D-W2-1 above, recovery is a Moira backend slice
— one migration (two columns plus a CHECK that `replaces_admin_identity_id` is set iff
`is_recovery`), two DTO changes, an atomic revoke-and-grant swap inside the existing transactional
envelope, a new error code and notice with pinned emitters, an OpenAPI regeneration, and a
mid-transaction failure-injection test — with a thin UI on top. That is the size of wave 2's own
grant-administration slice, and it is not UI work.

Half of what recovery promises is **already achievable with what exists**: "revoke a locked-out
admin's grant, then invite their replacement" is two ordinary operations, and `AdminTable` plus
`InviteAdminForm` expose both. What is genuinely missing is the *atomicity* of the swap, and
atomicity is a backend property. A `RecoveryPanel` performing two independent calls while the plan
promises "never a window where both or neither exist" would be the appearance of a feature.

*Reversal condition:* when a wave takes the Moira change end to end — `is_recovery`,
`replaces_admin_identity_id`, the in-envelope swap, `admin_identity_recovered`, and the
mid-transaction failure-injection test asserting neither half persists without the other. The UI is a
follow-on to that, never its driver.

## W5-D7′ — the a11y walker DECLARES its blind spot instead of failing forever (taken by the loop, 2026-07-31)

Plan 09 §0.8.4 step 24 asked for the final-URL assertion plus a permanently red suite: *"expect `/`
and `/admins` to fail it until an authenticated e2e path exists. Record that failure honestly."*

A permanently red merge gate is not a gate. It blocks every later change for a reason unrelated to
that change, and the pressure to weaken it is then constant — which is how the assertion gets lost
rather than kept.

**Taken instead:** the walker classifies each route as gated or public from its source path;
a **public** route's final pathname is asserted to equal the one requested BEFORE axe runs; a
**gated** route is asserted to redirect to `/login` and is NOT audited; and the unaudited set is
pinned in **both directions** against a declared list. Same information visible, same drift fails,
and it merges.

Mutation-verified: a `redirect("/login")` injected into the public `/invite/[token]` fails the URL
assertion naming both URLs; removing `/admins` from the declared list fails the set assertion.

*Reversal condition:* when an authenticated Playwright project exists — which needs a mock IdP inside
the e2e environment — delete the declared entries and KEEP the URL assertion. The assertion is what
will prove the authenticated run is doing anything.

## THE THIRD TOOTHLESS GUARD, CAUGHT BY ITS OWN MUTATION — 2026-07-31 (plan 09 wave 5)

**`route-handler-session.test.ts`'s named mutation left it GREEN.** The guard is "every route handler
under `app/api/**` re-checks the session itself" — necessary because route groups contribute no
layout there, so a handler inherits no gate. It scanned each `route.ts` for `withConsoleSession(`.

The mutation was to delete the guard from `DELETE` in `app/api/admins/identities/[id]/route.ts`,
which exports **both** `PATCH` and `DELETE`. It passed: the file still contained the call, in the
other handler.

**A file-level scan can express "some handler is guarded", never "every handler is"** — and the
second exported method in a file is exactly where an unguarded mutation endpoint would go.

Rebuilt per exported method, with the body brace-matched. Two extractor bugs surfaced only by
re-running the mutation, not by reading the code:

1. the first `{` after `export async function POST` is inside the **second parameter's** type
   annotation (`{ params: Promise<…> }`), so brace-matching from there ended the "body" before it
   began, and a correctly guarded handler was reported as unguarded;
2. `export const GET = handle;` has no body. Skipping it would have made the alias form a hole in the
   rule, so it is recorded with an empty body — i.e. unguarded — which is the safe direction.

Re-mutated after the fix: both the `DELETE`-only deletion and the alias rewrite fail, naming the
method.

**Third occurrence of the same shape** (after F19's enumeration oracle and G1's decoy-less fixture).
The standing question is now: *after any change to what a guard measures, can its fixture still
represent the defect it is named for?* Here the answer was no for a reason nothing in the code
looked wrong about.

## D3 — wave 4 implements Option A′, staged 4A / 4B (taken by the loop, 2026-07-31)

Decided by a judged panel: three worked designs, three judge lenses (security closure, migration and
operational cost, contract and test surface), one synthesis that re-read the code where judges
disagreed. Claims verified empirically against the live database in rolled-back transactions. Full
document staged at `scratchpad/wave4-decision.md` and folded into plan 09 §0.7.7.

**Option A′ = per-provider console-minted issuer, plus a deterministic two-stage policy lookup
replacing `governing_policy`.** Option C (a provider claim) rejected; Option B (console-side
enforcement) rejected as a whole, one idea taken from it.

**The decisive argument is a constraint, not a preference.** The mandated invariant — a partial unique
index on `(trusted_jwt_issuer_id) where enabled and status='active' and deleted_at is null` — and
multi-provider are **incompatible under one console issuer**: the index refuses the second enabled
provider outright. Per-provider trusted issuers is the only shape in which the invariant wave 4 must
ship and the capability wave 4 exists to deliver can coexist.

**F24 closes with zero `admin_identities` change** — verified live, since
`admin_identities_issuer_subject_active_unique` is on `(issuer, subject)`, so distinct per-provider
issuer strings give distinct grants for the same `sub`.

**F23 shape (b) survives A′ and is AMPLIFIED by it** — an enabled *unbound* row whose own `issuer`
equals a console issuer string outranks the correctly-bound row at any age, and no index on
`(trusted_jwt_issuer_id)` can reach it because that column is NULL. Per-provider issuers multiply the
collidable strings from 1 to N. Reproduced live. Closed only by the two-stage lookup **plus** a new
`auth_provider_issuer_shadows_trusted_issuer` guard — not by the mandated index, which creates cleanly
with the rogue row present.

**Staging is load-bearing.** 4A ships the invariant, the deterministic lookup, `github_oauth` schema
and enum, the B2 fix, a trusted-issuer deletion guard, and wires `checkSession` (F25) — and
**keeps `ambiguous_enabled_providers`**. The console guard may be removed only after 4A is
**deployed**, not merely merged, because until Moira itself refuses the ambiguous state the guard is
the only thing standing in front of it.

### T0 SPIKE — PASSED. 4B's primary reversal condition does not fire.

`spike/w4-t0-provider-session`, `plans/reports/W4-T0-SPIKE.md`. **`context.params.providerId` is
populated when `createSession(user.id)` runs**, so the token minter can know which provider
authenticated the current session. Chain observed in the installed `console/node_modules` (which was
**absent** — the agent ran `bun install --frozen-lockfile` rather than reasoning from published docs,
which is the rule this project learned the hard way): `better-call`'s router puts `params` on the
endpoint input → `dispatch.mjs` wraps the handler in `runWithEndpointContext` → `with-hooks.mjs`
reads it back through `getCurrentAuthContext()` into `hooks.session.create.before`. Confirmed the
value cannot come from the call site: `link-account.mjs::handleOAuthUserInfo` calls
`createSession(user.id)` with no provider argument.

**The strongest evidence was not in the brief: Better Auth ships a plugin that already does this.**
`plugins/last-login-method` resolves `ctx.params?.providerId` from a session `databaseHooks`. So 4B
rests on a supported pattern, not a private-API bet.

**The two-linked-accounts case — the one that matters — was proved on real PostgreSQL**, not the
memory adapter: 1 `user` row, 2 `account` rows with different subjects and the same verified email
(implicitly linked), 2 `session` rows carrying different provider ids. Minting from B's session gives
`iss = …/idp/contractors` **and** `sub = …bbbb`, both naming B, concurrently with A's session minting
A's pair. **Both forbidden heuristics would have put A's subject under B's issuer** — which is F24
reproduced. A single-account fixture passes either way, so this is the assertion that has teeth.

**Four constraints 4B inherits, one of them unanticipated by the decision:**

1. The `session.providerId` column is nullable; pre-4B sessions must **refuse**, never default —
   asserted by nulling a live row and re-minting.
2. The refusal must **throw**. `sign.mjs` spells it `await getSubject(...) ?? session.user.id`, so a
   nullish return silently falls back to the console's own user id — a refusal that returns instead
   of throwing is not a refusal.
3. **NEW — the jwt plugin also mints on `/get-session`** (an `after` hook setting `set-auth-jwt`), so
   a refusal that throws **500s an ordinary session read**. Fix is
   `jwt: { disableSettingJwtHeader: true }`, demonstrated both ways: one spike part asserts the 500,
   the next asserts a clean 200 with `/token` still refusing. **This would have shipped as a broken
   session read discovered in production.**
4. Read `params`, never `path` — `path` is the route *template*, so `path.split("/").pop()` yields
   the literal `":providerId"`.

Migration `0003` (`alter table "session" add column "providerId" text;`) was derived from Better
Auth's own schema compiler rather than hand-written.

**Correction to the decision:** its T0 listed approaches 1–3 as alternatives. **Approaches 1 and 2 are
not alternatives — 1 supplies the value and 2 is what persists it and reads it back; 4B needs both.**
Approach 3 (after-callback stamp) is rejected outright: `create.after` is queued through
`queueAfterTransactionHook`, leaving a window with a NULL provider.

Verification: 10 spike tests green (8 memory, 2 PostgreSQL), `typecheck` and `lint` clean, full
console suite 561 pass / 1 fail where the single failure is the deliberate `CONSOLE_SKIP_DB_TESTS`
canary — **also red on the baseline captured first**, which is how it was shown not to be a
regression. Durable tests used their own `console_auth_t0_spike` database.

### Reversal conditions

**Primary — blocks 4B only. NOT FIRED; T0 passed (above).** If spike T0 had shown Better Auth 1.6.25
cannot make the authenticating
account's `providerId` available to the token minter for the current session, 4B has no honest
implementation. Ship 4A, keep the console guard, defer multi-provider with that named blocker.
**Explicitly forbidden as substitutes:** the most-recently-updated-account heuristic (wrong exactly
when two accounts are linked, which is the case it exists to handle), and disabling implicit account
linking to force 1:1 (a second provider then returns "account not linked" for precisely the humans
multi-provider serves).

**Secondary — reverses A′ entirely.** If a requirement appears that two interactive providers must
share one `trusted_jwt_issuers` row, the invariant and multi-provider cannot both hold, and the
decision reverts to Option C with its full cost: `provider_claim` columns, an `admin_identities` key
rotation that must be spelled `nulls not distinct`, and a self-disarming legacy read-fallback.

**Tertiary — changes 4B's budget, not its shape.** If wave 5's revocation/ownership UI cannot ship
with per-grant rather than per-human semantics, a `person_key` grouping column must land in wave 4.
Verified live: `admin_identities_single_active_primary` is unique on `(is_primary)` **globally**, so
under 4B a human's second grant is never primary and revoking "the" row leaves a live back door.

### What A′ explicitly does NOT buy — recorded so nobody credits wave 4 with it

- **No cryptographic separation between providers.** One ES256 key pair, one JWKS URL, N issuer
  strings. A token signed with `iss = <github issuer>` verifies against the GitHub trusted-issuer row
  whichever IdP actually authenticated the human. **The `iss` selection is a security boundary backed
  by one line of console code and one test.** F17's blast radius grows from one issuer to N. (That
  consequence is now closed on `fix/f17-jwks-rotation`; the *premise* — one key pair behind N issuers
  — is unchanged, so any future key-material hazard still lands on every provider at once.)
- **No person-level identity.** One human across two providers holds **two** grants with no column
  linking them; revocation and `is_primary` are per-grant.
- **Moira still cannot see which upstream IdP authenticated a user.** A′ routes *around* F23's
  structural finding; it does not close it. Moira receives a console assertion pinned to a registered
  issuer, not an independent view.
- **At most one `github_oauth` row per deployment** (one GitHub org per console) and one
  discovery-only OIDC row with a null issuer — both from the untouched
  `auth_provider_settings_method_issuer_active_unique`.
- **No schema/binary version handshake.** B2 is mitigated for one enum only; ~30 `*_from_db` mappers,
  ~21 fallible per-row `collect()` sites and 48 enum-like CHECKs remain exposed. Rollout ordering
  (Moira first) is the only mitigation and goes in the release note.

### F29 — `ExecutionOutcome.structured_output` is always `None`, for every caller — **CLOSED**, see "F29 — CLOSED `fix/f29-structured-output`" below

The **request** side of structured output works: `output_schema` is honoured and
`structured_output_invalid`/`_unsupported` are catalogued. The **response** side does not.
`ExecutionRunOutput.structured_output` is hardcoded `None` at three sites in
`src/application/execution.rs` — on the streaming **and** non-streaming paths — so no caller in the
tree has ever received a parsed structured output.

**No test pins the gap**, which is why it survived. Found while building plan 11 Sub-Phase F, whose
extractor parses `output_text` and prefers `structured_output` when present — so it works today only
because the preference never fires.

Not Sub-Phase F's to close, and recorded rather than fixed in passing: it changes what every caller
of the execution API receives, which deserves its own change and its own tests.

**The fail-hard variant F29 chose *not* to adopt has its own three preconditions, and all three now
hold** — F39 landed; `StructuredOutputInvalid` has a recorded, guarded disposition (in none of
`is_retryable`, `is_fallback_eligible`, `is_circuit_failure`); and `run_extraction` reads
`execution.status`. The last two landed on `fix/f30-consent-columns` (2026-08-03). **The flip is
still deliberately unshipped**; see "F30 CLOSED (partly refuted) · F29's last two preconditions
LANDED" below and the doc comment on `structured_output_from_text`.

### F30 — there are TWO memory-consent columns, and nothing makes them agree — **CLOSED, premise partly REFUTED**, see "F30 CLOSED (partly refuted)" below

`application_memory_policies.consent_mode` and
`application_conversation_policies.memory_consent_mode` are **independent**, both default
`'explicit_only'`, and no constraint or code path reconciles them. Plan 11's body — and the brief
derived from it — name only one.

**Reading either alone is a defect in both directions**: honour only the memory policy and a
conversation-level `explicit_only` is ignored; honour only the conversation policy and the reverse.
Sub-Phase F takes the **stricter of the two** (decision D4).

Worth stating because it is the shape that hides: two columns that agree in every default
deployment, and disagree exactly when an operator has deliberately tightened one of them.

### F31 — the public OpenAPI contract still says citations are always empty, and a test **enforces** that

Found by Sub-Phase E, 2026-08-02. Not Sub-Phase E's to fix, and deliberately left alone.

`docs/openapi.json` documents `PublicResponse.citations` as:

> *"Always an empty array: RAG retrieval is not wired into response generation in this release, so
> no citation is ever produced."*

That has been false since Sub-Phase G. `citations_from_link` (`src/application/public.rs`) returns
`link.context.citations`, which the planner populates from real retrieval provenance, and
`tests/rag_retrieval_end_to_end.rs` asserts it.

**The part that makes this an entry rather than a typo:**
`public_response_schema_documents_always_empty_citations` (`src/http/mod.rs`) asserts the
description **contains** the words `"empty"` and `"not wired"`. It is green today, and it goes red
on the fix. That is HANDOFF §3.4's sixth shape — *a test that pins the defect* — and it is the
second instance of it in this repository.

The reach is a public contract, not an internal comment: every consumer generating a client from
`docs/openapi.json` is told a field it now receives data in is permanently empty.

**Why Sub-Phase E did not fix it.** The correct replacement text is a statement about *when*
citations are populated, which is Sub-Phase G's semantics rather than E's, and getting it wrong
would put a second false statement on the public contract. Fixing it also regenerates
`docs/openapi.json`, which would entangle an unrelated contract change with E's reviewable diff.

**Remedy, in one change:** rewrite the description to state the real condition, rename the test to
say what it now guards, and regenerate the snapshot. Verify the new test fails against the *old*
description — a guard on a description string is exactly the kind that passes by accident.

### F32 — `conversation_content_persistence` is enforced by nothing

Found by Sub-Phase E, 2026-08-02, while deciding whether a summary body may be persisted in
plaintext.

`application_conversation_policies.conversation_content_persistence` is a four-value column
(`none`, `metadata_only`, `plain_content`, `encrypted_content`) with a check constraint and a
default. **No code in `src/` reads it.** `conversation_messages.content_plain` is written
unconditionally by `add_message`, and `*_encrypted` columns exist on three tables with no writer at
all.

So an operator who selects `metadata_only` or `encrypted_content` — which is what a deployment with
a data-residency or PII obligation would select — gets `plain_content` behaviour, silently, with no
error and no signal. The policy reads as enforced because it is validated, versioned, exposed on
`ConversationPolicyRecord`, and settable through the admin API.

Sub-Phase E **deliberately did not become the first consumer** (decision D-E6's neighbour, recorded
in plan 11 §0.1c): honouring it for summaries while the message path ignores it would mean a
conversation stores every turn in plaintext and withholds only the summary derived from them —
an inconsistency dressed as a fix. Sub-Phase F reached the same column from the other side and
guessed the same way: its `turns.is_empty()` branch comments that a conversation persisting no
plaintext has nothing to extract from, a state no configuration can currently produce.

**Remedy is a change of its own:** make `add_message` the enforcement point, decide what
`encrypted_content` actually means (there is no cipher wired to those columns), and only then
extend it to summaries and extraction transcripts.

#### F32 — CLOSED, `fix/f32-content-persistence`. Wired, not removed

**The decision was wire rather than remove**, and the reasoning is worth keeping because the
alternative was defensible. Removing the column is a migration plus an OpenAPI change plus a drift
regeneration, and it is *destructive to any deployment that has already set it* — including exactly
the deployments the column exists for. Wiring it is additive, and the four values name a real
requirement (a data-residency or PII obligation) that Moira has no other way to express.
*Reversal condition:* remove it if a deployment ever needs conversation content withheld at a
granularity this column cannot express — per conversation, per tenant, or per message role —
because at that point the application-wide enum is the wrong shape and keeping it would be a second
policy that disagrees with the first.

**Two corrections to the finding as written above**, both of which the brief drawn from it
inherited:

1. **"a state no configuration can currently produce" was right; the gloss that grew around it was
   not.** The finding was later restated as the policy having an *emergent* effect — an operator
   setting `'none'` getting no extraction because there was no plaintext to extract. That is
   backwards. `content_plain` was bound unconditionally, so `'none'` stored **full plaintext**,
   `turns` was never empty for a policy reason, and extraction ran on a `'none'` application exactly
   as on a `'plain_content'` one. There was no emergent protection to preserve. Verified by
   mutation M1 below, which is literally the pre-fix line and turns four cases red.
2. **Four sites named the policy, not two.** Beyond the extraction `turns.is_empty()` branch and
   `summarization.rs`'s doc comment, `build_summarization_plan` had the *same* false claim in its
   own `turns.is_empty()` branch, and `ConversationSummaryInsert::summary_text` was documented as
   "`None` when the application's persistence policy excludes plaintext" — a prepared seam whose
   only caller always passed `Some`. `run_summarization` even took `policy` and discarded it with
   `let _ = policy;`.

**What was built.** Enforcement at `add_message`, in the existing `for update` lock query via a
`left join` + `coalesce`, so it costs no extra round trip and cannot observe a policy from a
different instant than the row it governs. Chosen over the three application-layer call sites
deliberately: `add_message` is the only path into `conversation_messages`, so a fourth writer
inherits the policy instead of having to remember it — and "having to remember it" is what F32
*was*. Second enforcement point at the summary write, reachable only via a mid-conversation policy
tightening (under a steady `none` the plan refuses first, so a guard there would have been
unreachable — HANDOFF §3.4 corollary 1). `none` vs `metadata_only` differ in the length-revealing
metadata (`content_size_bytes`, `token_count`); two enum values with identical behaviour would have
been the same defect in miniature. `content_hash` is retained under every value — it is an HMAC
under a deployment-held pepper, which F14's own analysis is the argument for.

**`encrypted_content` is refused on write** (`conversation_content_persistence_unsupported`, 422)
and **fails closed** for rows that already hold it. Accepting a value named for encryption while
storing plaintext was F32's sharpest edge: the API itself was doing the misleading.

**Six hand-written mutations, all caught** — committed as `scripts/f32-mutate.sh` so the claim is
re-derivable rather than a paragraph asserting a guard works, which is the artefact that failed six
times in §3.4. Plaintext stored anyway (M1, literally the pre-fix line — four cases red); `none`
collapsing into `metadata_only`; `encrypted_content` failing open; the refusal removed; the
missing-row default flipped; the summary write ignoring the policy. Each mutation asserts its own
anchor text is present first, so the script fails loudly if the code moves instead of silently
mutating nothing and reporting all-caught.

**OpenAPI is one added `description`** on `ConversationContentPersistence` — 152 operations / 100
paths / 181 schemas, unchanged. (The handoff's "151 / 99 / 178" is stale as of `dac7468`.)

### F33 — five `*_encrypted` columns, no cipher, no writer, no reader

Split out of F32 on closure, 2026-08-02, because it outlives it and is the larger half.

`migrations/0007` creates **five** encryption-at-rest columns —
`conversation_messages.content_encrypted`, `conversation_summaries.summary_text_encrypted`,
`memory_records.content_encrypted`, `rag_document_versions.content_encrypted`,
`rag_chunks.chunk_text_encrypted`. **Nothing in `src/` writes or reads any of them**, and no cipher,
key store, or key-rotation path exists anywhere in the tree. The schema advertises a capability the
binary does not have.

F32's fix removes the *acute* harm — no caller can now select `encrypted_content` and be told their
content is encrypted while it is stored in the clear — but the columns remain, and the next reader
of `0007` will reasonably infer that encryption at rest is implemented. Two prior findings in this
project had exactly this shape (a schema or a comment asserting a property no code delivered), so
the inference is not hypothetical.

**This is a scoping question for a human, not an autonomous change.** Envelope encryption touches
key custody, rotation, backup/restore, and — per plan 11's still-open Decision 3 — what keyword
retrieval is even allowed to do over encrypted rows. Doing it badly is worse than not doing it.

The honest interim options are (a) implement envelope encryption and make `encrypted_content`
selectable again, or (b) drop the five columns in a migration and state plainly that Moira relies on
storage-layer encryption (disk/volume) rather than application-layer. **Neither should be picked
without the operator requirement that motivates it.** Until then the state is documented rather than
implied: the enum's OpenAPI description and `docs/conversation-summarization.md` both say the value
does not encrypt.

Plan 11 says to dedupe against existing **active** memories. Retrieval is `active`-only, so scoping
dedupe the same way reads as consistent — and makes an `explicit_only` application accumulate **one
unconfirmed duplicate per turn**, because unconfirmed candidates are never `active` and therefore
never match. Caught by implementing it, not by reading it.

### F28/F29 RE-VERIFICATION — 2026-08-02, twelve agents, six claims, each conclusion challenged

Run before writing any code, because this project's recurring failure is a **finding whose premise is
wrong** (F32's was backwards; F15's recommended fix would have leaked; F2's mechanism was wrong
twice). Six claims investigated independently, each verdict then handed to a skeptic told to break
it. Read-only — no cargo, no gates — so it parallelised without touching the serialisation rule.

**F28's permit half is FALSE.** Two agents, independently, returned REFUTED. `permits` is a local in
the per-candidate retry loop in `MoiraExecutionService::execute_inner`, and `drop(permits)` runs on
every exit — including the cancellation arm, checked specifically for a `?` that could escape with
the permit alive. There is none. `extract_memories` runs after `execute_inner` has fully returned,
so **no permit is held across the second model call**. There is exactly one non-test acquisition
site in the tree.

**F28's SSE half is TRUE**, confirmed twice, and the escape hatch I offered in the brief — "streaming
may take a different route" — is false. Both SSE routes converge on `supervise_public_stream`, whose
body ends with the terminal `send_public_event`; `record_conversation_assistant` is **awaited**
between the last delta and that event, with no timeout wrapper. The `tokio::spawn` does not detach
the latency: the client's stream is fed by `public_rx` and cannot end until `public_tx` drops.

**And the real finding is bigger than F28** — see F34.

**F29 is three-quarters right and wrong in the part that sets the fix's scope.** Genuine defect count
is **2, not 3**: the third literal is in `failed_outcome`, which constructs `ExecutionOutcome` — a
different type — alongside `output_text: None` and a default `UsageSummary`.

> **CORRECTION, same day, by the synthesis pass.** The sentence that stood here — *"a failed
> execution carries no committed output, so `None` there is correct; it has 13 call sites, all
> failure paths"* — **was wrong**, and it was wrong in this ledger for about half an hour before
> anyone caught it. `failed_outcome` is called at `execution.rs:658` from inside
> `match result { Ok(Ok(output)) => … }` — the terminal-persistence-deadline arm — where `output` is
> **live**, and `output.usage.clone()` is read on the surrounding lines to feed `update_attempt` and
> `attempt_summary`. So one of the 13 is not a failure path, and its `None` is inert *only because
> the field is universally `None` today*. **The moment F29 lands it becomes a silent drop site.**
> Recorded as F38.
>
> This is the second time in one day that a *summary* of a finding was worse than the finding — see
> F32. Both times the error entered when a verified detail was compressed into a confident
> generalisation ("all failure paths", "emergent protection"). The tell in both cases was a
> universal quantifier that nobody had actually enumerated.

**The value does not exist to forward.** `rig-core` 0.40's `CompletionResponse` is
`{choice, usage, raw_response, message_id}` and `AssistantContent` is `Text | ToolCall | Reasoning |
Image` — no structured variant. `git log -S structured_output -- src/orchestration/runtime_factory.rs`
returns **zero commits over the whole history**. So F29 is not a plumbing gap; populating the field
means *parsing text as JSON*, and the only question is where.

**`StructuredOutputInvalid` is a JSON type-check wearing a validator's name.** Exactly one emitter,
in `build_completion_request`. `schemars` accepts any `Value::Object` or `Value::Bool`, so
`{"type":"banana"}`, `{"$ref":"http://x"}` and bare `true` all pass and go to the provider. Moira has
**no JSON Schema validation crate in `Cargo.toml` at all**. The name promises validation the code
never performs — the same shape as F32.

### F35–F45 — eleven findings nobody asked for, surfaced by the same twelve agents

Every one came from the "report anything you found that nobody asked about" clause. None was the
question being investigated. That clause has now out-produced the questions themselves twice.

| # | Finding | Severity |
|---|---|---|
| ~~**F35**~~ | ~~**The OpenAI-compat endpoint silently discards `text.format`.**~~ **CLOSED** `fix/f35-compat-text-format` — `json_schema` is honoured, `json_object` is refused (it would have reached the provider as an empty-object schema — that was **F46**, now **CLOSED** `a8937f4`: the native path refuses it too, so the two endpoints agree and F35's original reversal condition is superseded), every other `text` key is refused by a typed DTO. Finding verified correct in every particular; two things it did not mention made the fix sharper. See the F35 section below. | **HIGH** |
| **F36** | **`SummarizationLock` pins a Postgres session advisory lock across a full provider round-trip**, on a per-turn path, over a `pool.acquire().await?.detach()`ed backend. A hung provider holds one backend and one conversation lock for the whole attempt timeout. The lock's own doc comment names this reversal condition — and it has already been met. | **MED-HIGH** |
| **F37** | **Four wasted DB reads per conversation-linked turn when summarization is disabled**, inside the caller's request, between the last SSE delta and the terminal event. Six round-trips, four of them dead, on the default configuration. | **MEDIUM** |
| ~~**F38**~~ | ~~**The terminal-persistence-deadline arm throws away a successful provider result.**~~ **CLOSED** `fix/f38-deadline-usage`. Finding verified correct in every particular. **Decision: the outcome carries all three values (`output_text`, `structured_output`, `usage`) and `status` stays `Failed`**; the never-retry/never-fallback clamp is untouched. Two things the finding did not know sharpened it: `UsageSummary::default()` is all-`None` ("unknown"), not zero, so retention replaces an absence rather than overwriting a claim of no spend; and `terminal_update_from_outcome` runs on the `Failed` branch too, so the zeroing was also writing "no tokens, zero bytes, no hash" onto the `responses` row. **`"output_committed": true` was inaccurate** — a hardcoded literal, false in both available senses on the non-streaming path; now derived. Reversal conditions in the closure section below. | **CLOSED** |
| ~~**F39**~~ | ~~**The structured-output capability gate cannot see Rig's per-provider reality.**~~ **CLOSED** `fix/f39-structured-output-capability`. Both divergences verified true. Resolved **asymmetrically**, because they are not the same problem: DeepSeek is decidable and is now reconciled out of routing by reading Rig's own `SUPPORTS_RESPONSE_FORMAT`; `OpenAiCompatible`/`Local` is **not decidable at admission** and was deliberately left admitted. See the F39 closure section below. | **CLOSED** |
| ~~**F40**~~ | ~~**`GET /v1/responses/{id}` returns an empty `output` array for a completed, persisted response.**~~ **PREMISE REFUTED; two adjacent defects found and CLOSED** on `fix/f40-f47-response-output-and-policy-reads`. No persistence configuration reaches the empty array on a completed response, because **`output_persisted` is never `true`** — all three `ResponseTerminalUpdate` constructors hardcode `false`, the column defaults `false`, nothing in `src/` writes `true`. So `Completed` always took the `OutputUnavailable` branch and `Vec::new()` was reached only by `Queued`/`InProgress`/`Failed`/`Cancelled`, where it is correct. What was wrong: the **reason was a lie for three of the four persistence modes**, and `Completed && output_persisted` fell to `[]` — the inversion that produces F40's exact symptom the day content persistence lands. See the F40 closure section below. | **CLOSED** |
| ~~**F41**~~ | ~~**Skill-tree drift on exactly the guidance F29's implementer needs.**~~ **STRUCK — the inference was wrong.** The `.claude/` copy is a nine-line **pointer file** that says to read the `.agents/` one; all eight `.claude/skills/` entries have that shape. See "F41 is WRONG as recorded" below. | **struck** |
| ~~**F42**~~ | ~~**i18n overclaim.** `moira.error.structured_output_invalid` asserts "or the model's output does not conform to it".~~ **CLOSED** `fix/f42-f45-declared-vs-true` `871889f`, `4551ba3`. **Premise held.** Enumerated: the code has exactly **two** emitters and both reject the *caller's schema* — `validate_response_format` (over `maximum_schema_bytes`) and `build_completion_request` (not a readable `schemars::Schema`, the only construction site of the class). `classify_completion_error` never produces it, so there is no third route in from the Rig boundary. **The near-miss the finding did not mention, and the reason the sentence was plausible enough to survive:** `memory_extraction::FAILURE_STRUCTURED_OUTPUT_INVALID` is the same string for exactly the missing case, but its own doc says it is never returned to a caller — it lands on `memory_extraction_runs.failure_class` and never renders an i18n message. Description corrected and the fail-hard variant deliberately **not** shipped (F29 needs three preconditions; only F39 has landed). `default_message` was wrong in the same direction and is now *"The structured output schema is invalid."* Two guards added, both from asking §3.4's question of the fix. | **closed** |
| ~~**F43**~~ | ~~**`ConcurrencyController::acquire` is dead `pub` API** — every caller is inside `#[cfg(test)]`.~~ **CLOSED** `fix/f42-f45-declared-vs-true` `8729068`. **Half right, and the actionable half was wrong.** 29 call sites, every one test code — but **9 are in `tests/cluster_coordination.rs` and `tests/coordination_default_path.rs`, which are separate crates**, so `pub(crate)` and deletion were never available. "Dead `pub` API" in a `publish = false` service crate means "visible to integration tests", not an external contract. **The hazard was the real finding.** Resolved by removing the *choice* rather than the code: the wrapper is gone, `acquire_scoped` is now `pub` and named `acquire`, and there is exactly one admission function which cannot be called without stating `is_stream`. | **closed** |
| ~~**F44**~~ | ~~**`RuntimeModelHandle::stream` / `RuntimeStreamOutput` are dead `pub` API.**~~ **CLOSED** `fix/f42-f45-declared-vs-true` `f83437c`. **Premise held in every particular**, verified by enumeration: `.stream(` occurs **exactly once** outside `target/` and that occurrence is Rig's own `CompletionModel::stream`; `RuntimeStreamOutput` occurred four times, all of them the method's own definition, return type, construction and re-export. `RuntimeEventSeed` and `next_event` existed only to serve it. 103 lines deleted. **What the finding did not know:** that cluster was the *sole* reason `runtime_factory.rs` imported `RuntimeEventEnvelope`, `RuntimeEventType` and `serde_json::json`; deleting it restored the Rig/runtime-event module boundary. | **closed** |
| ~~**F45**~~ | ~~**`PublicResponseFormat::JsonSchema { name, strict }` — both accepted, both dropped.**~~ **CLOSED** `fix/f42-f45-declared-vs-true` `b516c4e`. **Premise held.** Neither is expressible in `rig-core` 0.40 on any provider. **Resolved asymmetrically, because the two fields are not the same problem: `strict` is refused, `name` is documented.** `strict` became `Option<bool>` and an explicit `false` is now `422 unsupported_request_option` on both endpoints — a stated **public contract change**. `name` cannot be refused (a required field of the variant) and is not smuggled through the schema's `title`. OpenAPI counts verified by hand and unchanged at **152 / 100 / 183**. | **closed** |
| **F48** | **A third `output_schema` drop path, latent, and it is silent even on OpenAI.** `should_apply_response_format` (`openai/completion/mod.rs`) is `output_schema.is_some() && supports_response_format && (tools.is_empty() \|\| history_has_tool_result)`. The third clause drops the schema on **turn 1 of any tool-calling conversation, for every OpenAI-family provider**, and unlike the DeepSeek path it emits **no `warn!` at all** — the warning at the same site fires only when `supports_response_format` is false. Cannot bite today: `build_completion_request` hardcodes `tools: Vec::new()`, so `tools.is_empty()` is always true. It becomes live the moment tool calling is enabled, which `.agents/skills/moira-rig-tools/SKILL.md` contemplates. **The F39 fix does not cover it** — F39 reconciles by provider type, and this drop is per-request. Rig documents the caveat itself: *"a turn-1 answer with no tool call is therefore not schema-constrained; `Native` is 'guaranteed' only once tools have run"* (issue #1928). **GUARDED, still latent and still not fixed** (`fix/f38-deadline-usage`). Premise re-verified: `execution.rs:1907` is the tree's **only** `CompletionRequest` construction and still hardcodes `tools: Vec::new()`, and `public.rs` refuses caller-declared tools outright (`unsupported_tool`, *"client-defined tools are not registered in this phase"*), so tools cannot reach the constructor from any direction. The drop behaviour is Rig's and is deliberately unchanged; the guard is `moiras_request_still_carries_its_schema_onto_rigs_openai_wire_body` in `src/application/execution.rs`, which hands Moira's real request to Rig's real encoder and reds the moment `tools` stops being empty. See F49 for why the obvious-looking integration coverage does not substitute for it. | **MEDIUM, latent — guarded** |
| ~~**F49**~~ | ~~**No integration test in the tree ever builds a request from an agent profile.**~~ **CLOSED** `fix/f49-agent-profile-coverage`. **Premise held, with one correction: the column is `route_definitions.agent_profile_id`, not `routing_policies.agent_profile_id` — `RoutingPolicyRecord` has no such field.** Verified exhaustively: all six typed `RouteDefinitionCreateRequest` sites in `tests/` pass `agent_profile_id: None`, the raw-SQL insert at `tests/public_authorization.rs:693` omits the column, and the seeded `general` route at `migrations/0005_provider_runtime.sql:295` omits it too — so `agent_profile` really was `None` on every end-to-end path. `tests/agent_profile_wire.rs` (6 cases) now attaches a real profile to the fixture's route and asserts on the body that reached the scripted mock. **The branch is correct at the wire**: `preamble` arrives as the leading `system` message, `temperature` and `max_tokens` arrive top-level, caller options win over profile values, and the streaming arm carries the same three. Eight mutations run, each reverted: `preamble: None` (3 red), dropping the `temperature` `or_else` (2 red), dropping the `max_tokens` `or_else` (2 red), inverting both `or_else` orders (**only** the precedence case red — the exact indistinguishability HANDOFF §3.4's seventh entry warns about, and why that case exists), never loading the profile (3 red), **hardcoding the profile's three values into `build_completion_request`** (the no-profile control red — this is the cheapest edit that leaves the primary case green, and the control is the only thing that sees it), a streaming-arm-only rebuild without the profile (**only** the streaming case red, so it is not redundant), and F48's own mutation. **Under F48's mutation only the new `tool_policy` case goes red; the preamble/temperature/max_tokens cases stay green** — they observe a different property, and `tests/structured_output.rs` was re-run under it and stayed green through all seven cases again. **F48's guard is not superseded** and its doc comment now says why. Raised **F50**. | **closed** |
| ~~**F51**~~ | ~~**The runtime-config invalidation channel is wired to per-request data tables, and `apply_invalidation` ignores its own scope for three of the four things it clears.**~~ **CLOSED** `fix/f51-f52-invalidation-scope` `f97d4f1`. **Premise held in full, and every count in it was right** — 24 tables when counted by trigger *function* (`auth_provider_settings`'s trigger really is named `auth_provider_settings_notify`, so a name-based query returns 23), `conversations` and `memory_records` really do fire on every `INSERT`/`UPDATE`/`DELETE`, and the three `invalidate_all()` calls really were unconditional while only the breaker reset was scoped. **Both candidate fixes taken, because they are independent barriers and the brief was right that they are not exclusive.** (1) `invalidation_plan` now returns an `InvalidationPlan { caches, circuits }` from one parse, and `RUNTIME_DATA_RESOURCE_TYPES` names the resource types that clear no cache; the narrowing is one-way, so an unparseable payload, a non-uuid id or an unrecognised `resource_type` still clears everything and resets every breaker. (2) Migration `0022` drops the trigger from both data tables. **Nothing depends on those two tables notifying**, established three ways: `docs/runtime-cache-invalidation.md` enumerates the invalidation-producing resources and lists neither — the schema drifted from the documented design in `0007`, not the other way round; **no cache in the process is keyed by a conversation or a memory record** (there is no `ConversationCache`/`MemoryCache` anywhere in `src/`), and the three the listener clears hold provider configuration, built provider clients and the enabled auth methods; and `src/infra/db.rs` is the **only** listener on the channel, so there is no other subscriber whose behaviour could change. **The most expensive consequence was under-stated by the original entry's own framing**: the standing justification in `apply_invalidation`'s doc comment was that the caches "rebuild on the next read, so re-reading them costs a query" — true of `RuntimeConfigCache` and `AuthProviderSettingsCache`, **false of `ProviderRuntimeCache`**, which holds built Rig clients with their connection pools and is keyed by a tuple that already contains every version number, so the config-write case it was defending never needed the wipe either. **Five mutations, each reverted.** Reverting the scoping reds the integration guard; **honouring the plan for `cache` but not `runtime_handles`/`auth_settings` — the cheapest edit that preserves the defect — reds it on the handles assertion specifically**, which is why the guard seeds a real `RuntimeModelHandle` and observes all three caches rather than the one that is easy to observe; leaving the trigger attached reds the trigger guard (and showed all three of INSERT/UPDATE/DELETE firing, which is why the test does all three); eating the fail-safe on the unknown arm reds the unit guard. **Under mutation 1 the unit guard stayed green and only the integration guard fired** — the correct-predicate/wrong-wiring split of F16's shape, and the reason both exist. *Reversal condition:* re-attaching either trigger requires deleting that table from `RUNTIME_DATA_RESOURCE_TYPES` in the same change, because a table that has become configuration must not stay classified as data — `every_triggered_table_has_a_scope` asserts `plan.caches` for every triggered table and reds if it is. | **closed** |
| ~~**F52**~~ | ~~**Three triggered tables are unclassified … and the guard that exists to prevent exactly this is a retyped list that has already drifted.**~~ **CLOSED** `fix/f51-f52-invalidation-scope` `9f243a6`. **Premise held in full and every specific was verified against the live catalogue**: `pg_trigger` returns exactly 24 tables for `notify_moira_runtime_config_change`, the three `legacy_*` tables are among them, their triggers still carry their *pre-rename* names (`legacy_providers`'s trigger is `providers_runtime_config_notify`) — so a name-based query mis-attributes them as well as missing `auth_provider_settings` — and the test's array really did hold 21 hand-typed names. **The fix is the shape the entry named**: `TRIGGERED_RESOURCE_TYPES` is now a real constant and `tests/runtime_notify_inventory.rs` pins it against `pg_trigger` **in both directions**, counted by trigger *function* and excluding `tgisinternal`, with a `MINIMUM_TRIGGERED_TABLES` floor against the empty-set failure a derived list is most exposed to. **The three legacy tables lose their triggers** (migration `0023`) rather than being classified: the finding's claim that nothing in `src/` reads or writes them is **correct and stronger than stated** — the only references anywhere in the tree are inside `0003_security_foundation.sql` itself, as a one-time backfill source guarded by `to_regclass(…) is not null`. The tables themselves are kept; they are an operator's record of the pre-0003 world and dropping them is irreversible. `legacy_applications` is renamed by the same migration and correctly absent — `0002` only ever attached the trigger to three tables. **Four mutations, each reverted, and the two halves interlock.** Leaving one legacy trigger attached reds both new tests — *the exact original defect, where the old guard was green*. **Attaching the trigger to a brand-new table (`responses`) reds the inventory test — the forward drift the retyped list could never detect, which is the whole property.** Then the cheapest edit: **"fix" that red by adding `responses` to `TRIGGERED_RESOURCE_TYPES` and nothing else — the inventory test goes green and the unit guard goes red**, because it iterates the constant and asserts the scope; and classifying it as `RUNTIME_DATA_RESOURCE_TYPES` instead leaves the scope assertion satisfied and reds the new `plan.caches` assertion. There is no way to satisfy one half without the other. *Reversal condition:* it re-opens if the inventory is ever satisfied by editing the constant alone — i.e. if `every_triggered_table_has_a_scope` stops iterating `TRIGGERED_RESOURCE_TYPES`, or if `tests/runtime_notify_inventory.rs` stops reading `pg_trigger`. | **closed** |
| ~~**F53**~~ | ~~**The same defect class F51 closed, one table over and at admin rate rather than request rate: `rag_documents` and `rag_collections` are content, not configuration, and every write to one wipes every replica's cache.**~~ **CLOSED** `fix/f53-f50-silent-degradation` `31a23a2`. **Premise held in full**, and the gating question it was recorded with has one answer for both tables — reached, as instructed, before choosing the fix. **Neither table's configuration is read through any cache the listener clears, and for `rag_collections` the reason is stronger than the entry expected: it carries no runtime configuration at all.** Its columns are `collection_key`, `display_name`, `description`, `status`, `visibility`, `metadata` and the lifecycle fields; the embedding model, dimension, batch size and timeout the entry guessed might live there live in `application_embedding_policies`, keyed by `application_id`, and `find_document_ingestion_context`/`find_collection_ingestion_context` join the collection **only** to reach `application_id`. That policy table is configuration, keeps its trigger and keeps `caches: true`. The three caches make the answer type-level rather than argumentative: `RuntimeConfigCache` is `HashMap<Uuid, ProviderConfig>`, `AuthProviderSettingsCache` is one `Vec<PublicAuthMethod>`, `ProviderRuntimeCache` is keyed by `RuntimeCacheKey` (provider/model/credential/runtime-policy ids and versions) — none can hold either row, and no `RuntimeCacheKey` field derives from either table. Every read of both, including the `visibility`/`status` predicates that carry tenant isolation, goes to PostgreSQL on the spot, so dropping the trigger cannot make an authorization decision stale either. **So both lose the trigger, and both barriers are taken as under F51**: migration `0024` drops them, and `RUNTIME_DATA_RESOURCE_TYPES` gains both names while `TRIGGERED_RESOURCE_TYPES` loses them. **Five mutations, each reverted.** Honouring the plan for `runtime_cache` only and still wiping `runtime_handles`/`auth_settings` reds the new integration guard **on the handles assertion**, which is why it seeds a real `RuntimeModelHandle`. Reverting the classifier for `rag_documents` alone, and separately for `rag_collections` alone, each red only its own leg — **and every unit test stayed green through both**, so the integration guard is the only thing that sees them. Re-attaching the `rag_documents` trigger reds the trigger guard (showing all three of INSERT/UPDATE/DELETE) *and* the `pg_trigger` inventory; then "fixing" the inventory the lazy way — adding the name back to `TRIGGERED_RESOURCE_TYPES` — turns the inventory green and reds `every_triggered_table_has_a_scope` on its `caches` half, so F52's interlock holds for these tables too. **Two corrections to the entry.** Its claim that `docs/runtime-cache-invalidation.md` "does not list either table" was true when written and **false by the time it was committed**: the F51/F52 commit added *"and the RAG collection and document tables"* to that paragraph while making it match `TRIGGERED_RESOURCE_TYPES`. And "its embedding model, its dimensions" describes columns `rag_collections` does not have. *Reversal condition:* if a collection ever acquires configuration a cache holds, re-create its trigger and delete its name from `RUNTIME_DATA_RESOURCE_TYPES` in the same change. | **closed** |
| **F50** | **A disabled or soft-deleted agent profile silently degrades every execution on its route.** `execution.rs:191` resolves the profile with `get_active_agent_profile`, which filters `status = 'active' and deleted_at is null`. Neither operation clears the route's reference: the FK is `on delete set null` and `soft_delete_agent_profile` only writes `status='deleted', deleted_at=now()`, never a `DELETE`. The lookup returns `Ok(None)` and the match arm treats it identically to "this route has no profile" — **no failure, no `warn!`, no runtime event, no audit row.** Every subsequent request loses its `preamble`, `temperature` and `max_tokens` and reports `succeeded`. A preamble is where guardrails live, so the failure mode is an unguarded model answering production traffic. The agent profile is the **only** runtime reference on this path whose disappearance is silent — an unresolvable route is a `RouteNotFound` failure. **Recorded, not fixed: the fix is a product decision.** Fail-closed is safer but breaks any deployment that disables a profile expecting its routes to keep serving; observable fail-open (`warn!` + runtime event) is cheaper but still serves the unguarded request. **Reversal condition:** decide fail-closed vs observable fail-open; on either decision `documents_current_behaviour_a_disabled_agent_profile_is_silently_ignored` in `tests/agent_profile_wire.rs` is wrong and must be rewritten — it is named `documents_` and not `guards_` because it pins current behaviour and would otherwise hold the defect in place (HANDOFF §3.4). Found by F49's new coverage. **ID allocated against `origin/main` at `779104d`, whose highest was F49.** **→ CLOSED.** The silence was fixed by `fix/f53-f50-silent-degradation` (`da8a936`); the product decision was taken **fail-closed** by the maintainer on 2026-08-06 (issue #79) and implemented by `feat/agent-profile-fail-closed-79`. `documents_current_behaviour_a_dangling_agent_profile_still_serves_the_request` was replaced by a guard asserting the refusal, exactly as its own reversal condition required, and the four observability guards were untouched by it. See both F50 sections below. | **closed** |

### F35 — CLOSED: `text.format` is now honoured for `json_schema`, refused for the rest

`fix/f35-compat-text-format`. The finding was verified before acting and was **correct in every
particular**: `#[serde(deny_unknown_fields)]` really is on `OpenAiResponseCompatRequest`,
`request.text` really had **zero** reads anywhere in the tree (`git log -S` shows the field arrived
in the baseline commit `227d90f` with no intent recorded), and `openai_compat_to_public` really did
hardcode `PublicResponseFormat::Text`.

Two things the finding did not mention, both of which sharpened the fix:

- **`docs/openai-compatibility.md` already stated the contract `text` was violating.** It lists the
  six mapped fields — `text` not among them — and then asserts "Unsupported options are rejected by
  `deny_unknown_fields` or public validation." So this was not an undecided question; it was a
  declared policy with one field quietly exempt from it.
- **`docs/openapi.json` published `"text": {}`** — an accepted field of unconstrained shape. The
  contract was not merely silent about the drop, it advertised the field as supported.

#### The decision: honour the subset that maps cleanly, refuse the rest

Neither of the two options in the brief was taken whole.

| `text` | Result |
|---|---|
| absent, `{}`, or `format.type = "text"` | `PublicResponseFormat::Text` — unchanged behaviour |
| `format.type = "json_schema"` | `PublicResponseFormat::JsonSchema`, carrying `name`, `schema`, `strict` |
| `format.type = "json_object"` | **422 `unsupported_request_option`** |
| any other key — `verbosity`, `format.description`, unknown `format.type` | **422**, from `deny_unknown_fields` on the now-typed DTO |

**Pure rejection was rejected because it would refuse requests Moira already satisfies.** A caller
sending `text.format = {"type": "text"}` is asking for exactly what the endpoint does. Failing that
is cost with no safety return.

**Pure honouring was rejected because of `json_object`, and the brief's unverified lead is why.**
It is now **confirmed, mechanically, on the provider socket**. `PublicResponseFormat::JsonObject`
becomes the output schema `{"type":"object"}`; `rig-core` 0.40's `sanitize_schema` inserts
`properties: {}`, `additionalProperties: false` and `required: []`, and the encoder sends it under
`strict: true`. The resulting schema is satisfied by exactly one document — `{}`. Translating
`json_object` would therefore have replaced F35's silent wrong answer with a different silent wrong
answer. `tests/openai_compat_text_format.rs::documents_native_json_object_reaching_the_provider_as_an_empty_object_schema`
asserts the exact bytes, so this is pinned rather than described.

**The `text` field is now typed rather than `Value`.** That is what makes "any key Moira does not
honour is refused" true by construction instead of by a list someone has to maintain, and it
replaces `"text": {}` in the published contract with the three real shapes. Two schemas were added;
operation and path counts are unchanged.

**This widens F45 rather than fixing it, and that is deliberate.** `name` and `strict` reach
`PublicResponseFormat::JsonSchema` and are then dropped by the native path — `rig-core` derives the
name from the schema's `title` and hardcodes `strict: true`, rewriting the schema to suit (every
declared property becomes required). Overriding that would mean Moira hand-building
`additional_params.response_format` and bypassing `output_schema` entirely, which is the boundary
violation `moira-rig-integration` exists to prevent. Refusing `strict: false` on the compat path
only was considered and dropped: it would make `/v1/responses` stricter than `/api/v1/responses`
for no reason, and OpenAI's own default for `strict` is falsy, so it would refuse the common case.
One contract, one defect, one place to fix it. Both limits are now written down in
`docs/openai-compatibility.md`.

**Known behaviour change, and it is the point.** An honoured `text.format` makes the request a
structured-output request, so it becomes subject to `structured_output_enabled` and the model's
`structured_output` capability. A caller who previously got a 200 and prose may now get a 422. The
endpoint is opt-in (`openai_responses_compat_enabled`, default `false`), which bounds the blast
radius to deployments that chose OpenAI compatibility — the deployments most likely to be sending
`text.format` in the first place.

*Reversal condition — **SUPERSEDED by F46's closure, 2026-08-02**.* As written it said the
`json_object` refusal reverts to a translation the moment the native path stops encoding
`{"type":"json_object"}` as an empty-object schema, i.e. when
`documents_native_json_object_reaching_the_provider_as_an_empty_object_schema` goes red. That test
is now deleted and the native path **also refuses** — so the trigger fired and the correct response
was *not* to revert. Reading the original condition literally would un-refuse the compat path into
a native 422 one layer later. **The live condition is now F46's:** both refusals revert together,
and only when `rig-core` gains a schema-free structured-output mode on a typed `CompletionRequest`
seam for every `ProviderType` Moira routes to. The `json_schema` honouring reverts only if
`PublicResponseFormat` stops
being the native representation of a caller-supplied schema; it does **not** revert on an F45 fix,
which would simply make it more faithful.

*Verification:* four unit assertions watched failing against unfixed code — `text.format.json_schema
must not be discarded, got Text`; `json_object must not be accepted and ignored` printing a whole
`PublicResponseRequest` with `response_format: Text`; `{"input":"hello","text":{"verbosity":"low"}}
must not deserialize into a request Moira silently ignores`. Three mutations killed:
deleting the `json_schema` arm turned the wiring test's provider body into
`{"messages":[…],"model":"test-model","moira":{…}}` with **no `response_format` key at all** —
F35 itself, reproduced on the wire; blinding the `json_object` refusal produced a 200 carrying
`"text":"must-not-be-reached"`; removing `deny_unknown_fields` produced a 200 for
`text: {"verbosity": "low"}`.

### F46 — `response_format: {"type":"json_object"}` constrains the model to the empty object — **CLOSED** `a8937f4`

Confirmed while deciding F35, on the provider socket rather than by reading. **Native path,
`POST /api/v1/responses`** — not the compat endpoint, which now refuses this shape.

*As originally recorded, below; the closure and the one clause it got wrong follow it.*

`PublicResponseFormat::JsonObject` maps to the output schema `{"type":"object"}` in
`prepare_execution`. `rig-core` 0.40's `sanitize_schema` completes any object schema with
`properties: {}` and `additionalProperties: false`, then sets `required` to the (empty) property
key list, and the encoder wraps the result with `strict: true`. What reaches the provider is
`{"type":"object","properties":{},"additionalProperties":false,"required":[]}` — a schema satisfied
only by `{}`. OpenAI's own `json_object` mode means *free-form* JSON, so a caller asking for it gets
the exact opposite of what the name promises, with a 200 and a `succeeded` status.

*ID allocation:* `origin/main`'s ledger topped out at **F45** immediately before this was written
(HANDOFF §3.2). Three concurrent finding branches were live; if F46 collides, this is the
`json_object` one.
### F39 — **CLOSED** `fix/f39-structured-output-capability`. Reconciled at candidate selection by reading Rig's own constant — 2026-08-02

**Both divergences survived verification.** `providers/deepseek.rs` sets
`SUPPORTS_RESPONSE_FORMAT = false`, and `providers/openai/completion/mod.rs` discards
`output_schema` with only a `tracing::warn!` when it is false. `ProviderType::OpenAi`,
`OpenAiCompatible` and `Local` really do share one `build_completion_model` arm, so all three send
`response_format.json_schema` to whatever backend the base URL names.

**The question was "can Moira know at admission time whether a schema will reach the provider?" and
the answer is different for the two halves. That asymmetry is the whole decision.**

- **DeepSeek is decidable.** Rig encodes the answer as a public associated const on a public trait,
  so it is a *compile-time fact* Moira can read.
- **`OpenAiCompatible` / `Local` is not decidable, ever.** Rig does send the schema; whether the
  backend honours it is a property of a binary Moira has never seen. Any admission-time verdict
  there would be a guess presented as a guarantee — which is the disease F39 names, relocated
  rather than cured.

**What changed.** One function, at the one site that already answered "does this candidate have
capability X":

```rust
fn provider_emits_output_schema(provider_type: ProviderType) -> bool {
    use rig_core::providers::openai::OpenAICompatibleProvider;
    match provider_type {
        ProviderType::OpenAi | ProviderType::OpenAiCompatible | ProviderType::Local =>
            <openai::OpenAICompletionsExt as OpenAICompatibleProvider>::SUPPORTS_RESPONSE_FORMAT,
        ProviderType::AzureOpenAi =>
            <azure::AzureExt as OpenAICompatibleProvider>::SUPPORTS_RESPONSE_FORMAT,
        ProviderType::DeepSeek =>
            <deepseek::DeepSeekExt as OpenAICompatibleProvider>::SUPPORTS_RESPONSE_FORMAT,
        ProviderType::Anthropic | ProviderType::Gemini => true,
        ProviderType::Custom => false,
    }
}
```

`capabilities_match` takes `candidate.provider_type` (already on `ModelCandidate`) and returns
false for `structured_output` when that is false. **It only ever subtracts** — a row declaring
`structured_output: false` stays unusable even where Rig would honour it, because that is also an
operator decision.

**Why this is not the "hardcoded table that rots" the brief warned about.** For the four
OpenAI-family provider types nothing is restated: Moira reads Rig's constant, so a `rig-core` bump
that changes a provider's behaviour changes Moira's admission decision **with no edit here**. The
divergence F39 describes is not *representable* for those four, which is strictly stronger than a
table plus a test that notices it rotted. Only `Anthropic` and `Gemini` are restated, because they
do not implement `OpenAICompatibleProvider` and expose no constant — they map `output_schema`
natively (`anthropic/completion.rs` `output_config`, `gemini/completion.rs` `generation_config`).

Rot in the *other* direction — Rig gaining DeepSeek support, silently un-applying the fix — is
caught by `rig_0_40_still_drops_the_schema_for_deepseek_and_sends_it_for_everyone_else`, which
states rig 0.40.0's truth table literally. **A red there is not a defect; it means Rig changed**,
and it says so in the assertion message.

**No contract change, and that is deliberate.** A disqualified candidate falls out of routing and,
if nothing else matches, yields the pre-existing `no_eligible_model` (404). That is **exactly** what
a row honestly declaring `structured_output: false` already produces — so the fix makes a lying row
behave identically to a truthful one. No new failure class, no OpenAPI drift, no catalog entry.

**Options rejected.**

| Rejected | Why |
|---|---|
| Hardcoded `ProviderType → bool` table | Duplicates Rig and rots silently on a bump. Replaced by reading Rig's const, which cannot diverge. |
| Refuse `OpenAiCompatible`/`Local` too | Rig *does* send the schema. Refusing breaks every conforming self-hosted backend (vLLM, llama.cpp, TGI) to guard against a non-conforming one Moira cannot identify. Symmetric treatment would be the wrong fix for the half that is unknowable. |
| Observe Rig's `warn!` at runtime | Requires a subscriber layer string-matching a dependency's log text, and fires *after* admission. F16 already shows Rig's tracing carries tenant data; adding a scraper there is the wrong direction. |
| A dedicated failure class / 422 | More precise-looking, but it special-cases `structured_output` against every other capability — `vision` already yields `no_eligible_model` on the same path. Consistency beat precision. |
| Validate the capability JSON on write | **Deferred, not rejected — see below.** Right idea, wrong blast radius for this change. |

**The deferred half, and why it is deferred rather than done.** Refusing `structured_output: true`
on a DeepSeek row *at write time* would stop the lie being stored at all, and gives the operator
feedback at the only moment they can act on it. It is not done here because it would 422 a
previously-accepted admin request and break any IaC that sets it — the **exact** shape of F32,
which is complete, gated, and deliberately left unmerged as PR #57 pending human sight
(HANDOFF §3.3 item 4). Landing a second one autonomously would be inconsistent with that
precedent. The routing fix covers already-stored rows, which the write-time check would not.

**Reversal condition.** This reverses if any of:
1. `rig-core` gives DeepSeek `SUPPORTS_RESPONSE_FORMAT = true` — the fix un-applies itself
   correctly, and the pinning test reds to force the re-read.
2. Moira gains a way to *verify* a backend honours `json_schema` (a probe, a declared conformance
   field on the provider row, a capability handshake). Then `OpenAiCompatible`/`Local` becomes
   decidable and the asymmetry above is no longer justified.
3. `capabilities_match` stops being the single site answering candidate capability — the
   reconciliation is only sound while there is exactly one such site.

**Three things found that the finding did not mention.**

1. **A third drop path, recorded as F48.** `should_apply_response_format` also requires
   `tools.is_empty() || history_has_tool_result`, so the schema is dropped on turn 1 of any
   tool-calling conversation **on every OpenAI-family provider, with no warning at all**. Latent
   only because `build_completion_request` hardcodes `tools: Vec::new()`. F39's provider-type
   reconciliation does **not** cover it — that drop is per-request.
2. **Anthropic and Gemini are fine, and had to be checked.** Neither goes through the OpenAI arm;
   both map `output_schema` onto their own request shapes. Had either dropped it, the finding's
   scope would have been twice what it claimed.
3. **Moira's OpenAI mock could not serve a DeepSeek provider, and failed misleadingly.** rig's
   DeepSeek response type declares `prompt_cache_hit_tokens` and `prompt_cache_miss_tokens` with
   **no `#[serde(default)]`**. Omitting them makes a perfectly good 200 fail to deserialize, and it
   surfaces as `provider_upstream_error` with the message **"provider request failed with HTTP
   200"** — which reads as an upstream fault rather than a mock gap. Two keys added to
   `tests/support/mock_openai.rs`; no rig `Usage` sets `deny_unknown_fields`, so nothing else moved.
   Anyone writing the next DeepSeek test would have lost the same hour.

**Guards, and the mutation that killed each — every one run, not read.**

| Guard | Mutation | Result |
|---|---|---|
| `a_deepseek_row_claiming_structured_output_is_not_routed_a_structured_request` | remove the reconciliation | red: `left: String("succeeded")`, `right: "failed"` — **the defect verbatim** |
| `a_structured_request_routes_past_deepseek_to_a_provider_that_sends_the_schema` | remove the reconciliation | red: `left: Null`, `right: Object {"a": 1}` |
| `a_deepseek_candidate_cannot_satisfy_structured_output_however_it_is_configured` | `DeepSeek => true` (hardcode instead of reading the const) | red |
| `rig_0_40_still_drops_the_schema_for_deepseek_and_sends_it_for_everyone_else` | same | red: *"rig-core changed: DeepSeek now sends response_format"* |
| `the_reconciliation_subtracts_only_and_touches_no_other_capability` | reconcile **upward** (`return provider_emits_output_schema(..)`) | red: *"an operator's explicit false must survive the reconciliation"* — **and nothing else red**, so the guards discriminate |
| `rig_drops_the_schema_before_the_wire_on_deepseek` | *(none — premise guard)* | green **before and after** the fix, by design |

That last row is the one worth reading twice. It drives a real DeepSeek `CompletionModel` against
the mock with `required_capabilities` deliberately **empty**, so the candidate filter never runs and
the request still reaches the wire — which is the only way the premise stays *observable* after the
fix starts excluding DeepSeek from routing. A guard that could only be checked before its own fix
landed would have been a guard nobody could ever re-run.

**Does this unblock the strict variant of F29? Partly — one of three, and F29's own entry says so.**
Its reversal condition names **all three** of: F39 landed; `StructuredOutputInvalid` given an
explicit retry/fallback disposition (today it is in *neither* `is_retryable` nor
`is_fallback_eligible` nor `is_circuit_failure`); and `run_extraction` reading `execution.status`
rather than inferring failure from `output_text` being `None`. **Item 1 is now true. Items 2 and 3
are untouched, so the lenient parse must stay**, and **F42** — the i18n string that already claims a
non-conformance path exists — becomes true on the same day as those two, not this one.



#### F46 — **CLOSED** `fix/f46-json-object-format` `a8937f4`. Refused, not translated — 2026-08-02

**The mechanism as recorded was correct except for one clause, and that clause was the one that
said it could not be fixed.** Verified against the vendored crate rather than assumed:
`sanitize_schema` (`providers/openai/mod.rs:39`) does complete an object schema with
`properties: {}` and `additionalProperties: false` and then set `required` to the empty property
key list, and `strict: true` is hardcoded at `providers/openai/completion/mod.rs:1839`.

**What the entry got wrong:** it said `json_utils::merge` lets the encoder's `response_format` win
over `additional_params`. `merge(a, b)` does let `b` win — but it is only reached inside
`if let Some(schema) = output_schema && should_apply_response_format` (line 1823), and
`should_apply_response_format` requires `output_schema.is_some()` (line 1818). With **no**
`output_schema`, `additional_params` passes through untouched and lands flattened on the wire
(`#[serde(flatten)]`, line 1582). So a hand-built `additional_params.response_format` *would* have
reached an OpenAI-family provider. The claim "cannot be fixed at Moira's layer" was false; it was
refused for different reasons, below.

**The decision: refuse it. `422 unsupported_request_option`, on the native path, streaming and
non-streaming — following F35, consciously, not departing from it.** The asymmetry the brief
warned about is now gone: both endpoints refuse the same shape for the same reason.

**Why not honour it as free-form JSON**, the option the name argues for:

1. **`rig-core` 0.40 has no representation of free-form JSON.** The string `"json_object"` occurs
   **zero** times in the crate. `CompletionRequest::output_schema` is the only structured-output
   seam and every encoder reads it as a *constraint* — OpenAI family via `sanitize_schema` +
   `strict: true`; Anthropic via `output_config.format = json_schema` (`completion.rs:2363`);
   Gemini only sets `generation_config.response_mime_type` when a schema is present
   (`completion.rs:234`).
2. **It is not expressible for every provider Moira routes to.** Anthropic's Messages API has no
   free-form JSON mode at all. Honouring `json_object` would therefore make the same request
   succeed or fail by *routing outcome* — a worse public contract than a consistent refusal, and
   it would need a new `required_capabilities` entry to hide the difference.
3. **It requires the boundary violation this tree has already refused once.** Doing it means Moira
   hand-building `additional_params.response_format` per provider and bypassing `output_schema` —
   named as the thing `moira-rig-integration` exists to prevent, in this ledger, in the F35 entry,
   about F45. Applying the opposite reasoning here would be the drift the brief warned about.
4. **The "open schema" variant was checked and rejected on evidence.** `sanitize_schema` inserts
   `additionalProperties: false` only when the key is **absent**, so `{"type":"object",
   "additionalProperties":true}` survives as `{"type":"object","properties":{},
   "additionalProperties":true,"required":[]}` — semantically free-form. But `strict: true` is
   hardcoded and OpenAI's strict mode *requires* `additionalProperties: false`, so this trades a
   silent wrong answer for a provider 400 on the flagship provider, and would behave differently
   on self-hosted backends. Rejected.

**Why "it removes a shipped capability" does not apply.** It removes a shipped *defect*. Every
caller who has ever sent `json_object` received a schema satisfied only by `{}`; there is no
working behaviour to preserve and nobody can be depending on one. Nothing in-tree used it either —
memory extraction and summarization pass explicit schemas or none.

**The variant stays in `PublicResponseFormat`.** Removing it would be the actual contract break:
requests would fail deserialization with an uncoded 400 instead of a coded, documented 422 that
names the field and points at `json_schema`. Same shape F35 chose for `OpenAiCompatTextFormat`.

**Two layers, deliberately redundant.** `validate_request` raises the coded 422 (placed *before*
the `structured_output_enabled` check, so the caller is not told to enable a policy that would
never help); `prepare_execution`'s match arm — where the empty-object schema was born — refuses
again and falls back to `None`, never to a schema. `prepare_execution` calls `validate_request`
itself, so there is no entry point that reaches one without the other.

*Reversal condition:* this refusal becomes a translation when `rig-core` gains a **schema-free**
structured-output mode reachable through a typed `CompletionRequest` seam — not through
`additional_params` — for every variant of `ProviderType` Moira routes to. A rig-core release in
which `"json_object"` appears in the OpenAI encoder *and* Anthropic/Gemini gain an equivalent is
the concrete trigger. Partial support is not enough: routing-dependent semantics on a public API
is the failure mode this refusal exists to avoid.

*Verification.* Four mutations run, none read:

| Mutation | Result |
|---|---|
| **M1** — restore both layers to pre-fix (`Some(json!({"type":"object"}))` + delete the refusal) | **RED**, and it is F46 verbatim: `got 200 OK: {…"status":"completed"…"output_text":"{}"…}`. The streaming twin returned `200` with the full SSE sequence, confirming a late refusal there becomes a 200, not a 422 |
| **M4** — the *plausible alternative fix*: map `JsonObject` → `None`, delete the refusal | **RED**. This is the one that matters: no bad schema reaches the provider under M4, so a guard asserting "the empty-object schema is absent from the wire" would have passed while the caller still got a `200` for a request Moira cannot honour. Asserting on the **refusal** rather than on the absence of the bad schema is what gives the guard teeth |
| **M2** — restore the `prepare_execution` translation only | **GREEN**, property held — `validate_request` still refuses |
| **M3** — delete the `validate_request` refusal only | **GREEN**, property held — `prepare_execution` still refuses |

M2/M3 green is the intended relationship, not a toothless guard: neither single edit reintroduces
the defect, and the guard asserts the property, not the implementation.

**Cheapest edit that breaks the property while leaving the guard green — none found inside the
public request path.** The two layers are individually sufficient and `prepare_execution` funnels
through `validate_request`, so no third entry point exists. The guard was also built against the
two ways this repository's guards have gone toothless before: it asserts the **error code**, not
just `422`, because a routing failure raises `422` with `call_count() == 0` for the wrong reason;
and it carries a `json_schema` **control on the same fixture** proving the provider *is* reachable,
so the zero is attributable to the refusal rather than to a broken fixture. The nearest surviving
route to an empty-object schema is **outside** this property and belongs to F45: `POST
/api/v1/admin/runtime/diagnose` takes `ExecutionOptions.output_schema` verbatim, so an admin
sending the literal schema `{"type":"object"}` still gets it closed and `strict`-ed on the wire.
That is rig's strict-mode rewrite of a caller-supplied schema, not a `json_object` mistranslation,
and the endpoint is admin-gated and `false` by default.

*On the provider socket, now:* `json_object` → `calls=0 status=422`. The `json_schema` control on
the same fixture → `calls=1 status=200` carrying
`"response_format":{"type":"json_schema","json_schema":{"name":"caller_title","strict":true,
"schema":{"type":"object","title":"caller_title","properties":{"answer":{"type":"string"}},
"required":["answer"],"additionalProperties":false}}}` — F45 still visible in `name` coming from
the schema's `title` and `strict` being hardcoded, unchanged and out of scope here.

*Guards:* `tests/openai_compat_text_format.rs::native_json_object_is_refused_and_never_reaches_the_provider`
and `::native_json_object_is_refused_before_the_stream_begins` (wiring), plus
`src/application/public.rs::tests::native_json_object_is_refused_and_the_other_two_formats_are_not`
(predicate — it also asserts `Text` and `JsonSchema` still pass, so refusing everything is not a
cheap way to make it green). The F35-era documentation test
`documents_native_json_object_reaching_the_provider_as_an_empty_object_schema` is **deleted**; its
own doc comment predicted exactly this.

*OpenAPI:* regenerated; **counts unchanged**. The brief's frozen figures (151 operations / 99 paths
/ 178 schemas) were **stale** — the tree is at **152 operations / 100 paths / 183 schemas**, which
is also what `src/http/mod.rs:777` asserts. One line added to `docs/openapi.json`: the
`PublicResponseFormat` schema description.
### F42, F43, F44, F45 — **CLOSED** `fix/f42-f45-declared-vs-true` — 2026-08-02

Four LOW findings sharing one shape: *the API declares something it does not do.* Independent
commits. **All four premises survived verification; one had a wrong conclusion (F43).**

#### F45 — `strict` refused, `name` documented. The public contract changed, deliberately. `b516c4e`

**Can Rig express either? No — established against the vendored crate before deciding.**

| Field | Provider | Verdict |
|---|---|---|
| `strict` | OpenAI family | `"strict": true` **hardcoded**, `providers/openai/completion/mod.rs:1838`. Not reachable via `additional_params`: with an `output_schema` present the encoder's object is the `b` argument to `json_utils::merge` and wins |
| `strict` | Anthropic | `OutputConfig { format: JsonSchema { schema } }` — no strictness field exists |
| `strict` | Gemini | `response_mime_type` + `response_json_schema` — no strictness field exists |
| `strict` | DeepSeek | schema dropped before the wire entirely (`SUPPORTS_RESPONSE_FORMAT = false`, F39) |
| `name` | OpenAI family | derived from the **schema's `title`**, falling back to `"response_schema"` (line 1826) |
| `name` | Anthropic, Gemini | no name field of any kind |

**The two fields got different answers, and that asymmetry is the decision.**

**`strict` is refused**, following F46. The silent upgrade is not harmless over-delivery:
`sanitize_schema` promotes **every declared property to `required`**, so a caller's optional
fields come back mandatory, and OpenAI's strict mode rejects schemas outside its supported
subset, so a caller who asked for best-effort can receive a provider error. That is measured, not
argued — `rig_0_40_still_hardcodes_strict_true_and_promotes_optional_properties_to_required`
sends a schema whose `note` property is optional and reads `required: ["answer","note"]` back off
the socket.

**PUBLIC CONTRACT CHANGE, weighed as F32 and F46 were.** `strict` is now `Option<bool>`; omitted
(`null`) and `true` are accepted exactly as before, an explicit `false` is `422
unsupported_request_option` on the native path and the compat path, streaming and non-streaming.
**F35 considered this refusal and rejected it** — *"OpenAI's own default for `strict` is falsy, so
it would refuse the common case"* — and F35 was **right about the field as it then stood**:
`#[serde(default)] strict: bool` made an omitted `strict` and an explicit `false` the same value.
Making the field nullable is what makes refusing available, and it is a **restoration rather than
an invention**: `OpenAiCompatTextFormat` already carried `Option<bool>`, and
`strict.unwrap_or(false)` in `compat_response_format` was destroying the distinction at the one
boundary that still had it. It removes a shipped *defect*, not a capability: nobody can depend on
`strict: false` working, because it never did.

**`name` is documented, not refused, and not honoured.** Refusing is impossible — it is a
*required* field of the variant, so refusing it refuses every request. Honouring means writing it
into the schema's `title`, which is the only thing `rig-core` reads: that mutates caller-supplied
data to pass a value through a field meaning something else — a subtler form of the boundary
violation F46 refused, abusing the typed field rather than bypassing it — and it works on one
provider family only, making the contract's truthfulness depend on routing, **which is F46's
objection #2 verbatim**. Unlike `strict`, `name` has no observable effect on the answer. The
documentation now points callers at the lever that does work: put it in the schema's `title`.

*Out of scope, and stated so it is not mistaken for an oversight:* `POST
/api/v1/admin/runtime/diagnose` takes `ExecutionOptions.output_schema` verbatim and never
constructs a `PublicResponseFormat`, so neither refusal applies there. There is no `strict` field
on that path to drop — an admin supplying a schema directly is not making a claim Moira is
ignoring.

*Reversal conditions.* The `strict` refusal becomes an honouring when `rig-core` exposes
strictness on a typed `CompletionRequest` seam — not `additional_params` — for **every**
`ProviderType` Moira routes to; a release in which the OpenAI encoder reads a strictness input
instead of hardcoding `true` *and* Anthropic and Gemini gain an equivalent is the concrete
trigger. `name` becomes honourable on the same all-providers condition, for a response-format
name. Partial support is not enough for either: routing-dependent semantics on a public API is
the failure mode both refusals exist to avoid. **Both triggers are mechanical**, not a diary
note: `rig_0_40_still_hardcodes_strict_true_and_promotes_optional_properties_to_required` reds
when either fact changes, and its message says so.

*OpenAPI:* regenerated. Counts **verified by hand from the committed document** and unchanged —
**152 operations / 100 paths / 183 schemas**, which is what `src/http/mod.rs:777` asserts. The
only drift was `PublicResponseFormat`'s description, two new property descriptions, and
`strict`'s type becoming `["boolean","null"]` and leaving `required`.

*Six mutations, each reverted, each run:*

| Mutation | Result |
|---|---|
| **M1** — blind both refusal layers | **RED**, F45 verbatim: `got 200 OK` … `"status":"completed"` on a `strict: false` request. All three wiring cases |
| **M2** — remove the `prepare_execution` layer only | **GREEN**, property held — `validate_request` still refuses |
| **M3** — remove the `validate_request` layer only | **GREEN**, property held — `prepare_execution` still refuses |
| **M4** — the *plausible alternative fix*: map `strict: Some(false)` to `output_schema: None` instead of refusing | **RED.** No bad schema reaches the provider under M4, so a guard asserting "the wrong schema is absent from the wire" would have passed while the caller still got a 200 for a request Moira cannot honour. Asserting on the **refusal** is what gives it teeth — F46's lesson, re-earned |
| **M5** — the cheapest edit: `strict.or(Some(false))` at the compat translation, restoring the old collapse of omitted-into-false | **RED, and only the omitted-`strict` CONTROL reds.** Every primary refusal assertion stays green. This is why the controls exist |
| **M6** — refuse the whole `json_schema` variant (the classic cheap green) | **RED** in 5 wiring cases *and* the predicate, which reports `an omitted strict must stay honoured — it is the common case` |

M2/M3 green is the intended relationship, not a toothless guard: neither single edit reintroduces
the defect, and the guards assert the property rather than the implementation.

#### F42 — the string described a path that does not exist; corrected, not widened. `871889f`, `4551ba3`

**Premise held.** Two emitters, both rejecting the *caller's schema*:
`validate_response_format` (over `maximum_schema_bytes`) and `build_completion_request` (not a
readable `schemars::Schema`) — the latter being the **only** construction site of
`ExecutionFailureClass::StructuredOutputInvalid`. `classify_completion_error` never produces the
class, so nothing arrives from the Rig boundary either.

**The near-miss the finding did not mention.** `memory_extraction::FAILURE_STRUCTURED_OUTPUT_INVALID`
is the identical string for exactly the case the description claimed — a model reply that does
not parse. It is *not* a counter-example, because its own doc comment says it is never returned
to a caller: it lands on `memory_extraction_runs.failure_class` and never renders an i18n
message. The corrected description says this explicitly, so the next reader does not "fix" the
wording back by pointing at it.

**The fail-hard variant was deliberately not shipped.** F29's reversal condition needs all three
of: F39 landed (true), `StructuredOutputInvalid` given a retry/fallback disposition (still
false — the mutation output confirms `retryable: false, fallback_eligible: false`), and
`run_extraction` reading `execution.status` (still false).

`default_message` was wrong in the same direction and was changed too: *"The structured output is
invalid."* points at the model on a path that only ever rejects the caller's schema. It is now
*"The structured output schema is invalid."*

*Guards, and the two gaps that asking §3.4's question found:*

1. **The corrected description is a claim about the code, and nothing observed it.**
   `docs_mirror_matches_rust_catalog` proves only that the Rust catalog and the JSON mirror
   agree — they can be wrong together, and were. So a **third emitter** added anywhere (a
   tool-argument validator raising the same code for a non-conforming *reply* is the obvious
   one) falsifies the description with every test green.
   `structured_output_invalid_has_only_the_two_emitters_its_catalog_entry_describes` derives the
   emitter set by walking `src/` and **parsing calls** — not matching mentions, which would
   harvest doc comments — in both spellings the code reaches a client. It is honest about its
   limit: it cannot prove prose true, it makes prose unable to rot silently.
2. **The suite's own header argues that `execute_rig_stream` is a separate path, then does not
   apply that argument to the non-conforming reply.** Adding the fail-hard variant to the
   **streaming arm only** left all seven existing cases green — case 2 sends conforming JSON and
   never reaches the branch, case 4 never streams. Verified by running it.
   `a_stream_whose_reply_is_not_json_leaves_the_field_null_and_still_succeeds` is the missing
   twin, and under that mutation it is the **only** case that reds.

*Mutations:* reverting the mirror description reds `docs_mirror_matches_rust_catalog` naming the
field; renaming the `AppError` code in `public.rs` and adding a third emitter in
`runtime_factory.rs` each red the new inventory test in the correct direction; the fail-hard
variant reds the completion case, and the streaming-only fail-hard reds the new stream case
alone. Both behavioural reds print a message naming **both** catalog files.

*Reversal condition:* the description is widened when the fail-hard variant ships, i.e. when all
three of F29's preconditions hold. The two behavioural guards are the trigger — they red on that
change and their failure messages say which files to edit.

#### F43 — the finding's conclusion was wrong; the hazard it named was real. `8729068`

**"Every caller is inside `#[cfg(test)]`" is true only if `tests/` counts as `#[cfg(test)]`.**
Enumerated: 29 call sites, all test code — 20 in `controls.rs`'s two `#[cfg(test)]` modules and
**9 in `tests/cluster_coordination.rs` and `tests/coordination_default_path.rs`, which are
separate crates and genuinely require a `pub` entry point.** So the implied remedies — make it
private, delete it — were never available. It is also worth stating once for the next
dead-`pub`-API finding: this crate is `publish = false` and is the only crate in the workspace,
so `pub` here means "visible to integration tests", not an external contract.

**The hazard was real and is what got fixed.** `acquire` supplied `is_stream: false` and
`provider_stream_limit = provider_limit` itself, and it was the shorter, more obvious name than
`acquire_scoped` — the one a future streaming caller reaches for, silently taking a *request*
permit and leaving `max_concurrent_streams` unenforced. **The fix removes the choice, not the
code:** `acquire_scoped` is now `pub` and named `acquire`, the wrapper is gone, and there is one
admission function that cannot be called without stating which ceiling is wanted.
`CapacityExhaustion`/`CapacityScope` became `pub` because that entry point returns the type that
*names* the ceiling — strictly more information than the wrapper's `ExecutionFailure`, which had
already discarded it. Production behaviour is unchanged: `execute_attempt` already called
`acquire_scoped` with the real `command.options.stream`.

*The guard already existed and has teeth.* `stream_capacity_is_independent_from_request_capacity`
in `tests/execution_lifecycle.rs` runs `max_concurrent_requests: 2` against
`max_concurrent_streams: 1` — two **distinct** numbers, so the ceilings stay distinguishable,
which is not true of the three other fixtures in that file that set both to 1 and therefore could
never have seen this. **Mutation: pass `false` instead of `command.options.stream` at the one
production call site. It is the only test in the suite that reds**, with `left: Succeeded, right:
Failed` — a second stream admitted against a ceiling of 1, F43's hazard exactly.

*What stops it coming back:* **nothing mechanical, and that is stated in the code.** No lint
catches a new four-argument convenience wrapper — it would have test callers immediately, which
is precisely how the old one survived. What changed is that the obvious name is now taken by the
function that demands the answer, and the wiring is guarded.

#### F44 — deleted; the hazard was divergence, and it is gone by construction. `f83437c`

**Premise held in every particular, verified by enumeration over the whole tree.** `.stream(`
occurs **exactly once** outside `target/`, at `runtime_factory.rs:347`, and that occurrence is
Rig's own `CompletionModel::stream` inside `start_stream_with_model` —
`RuntimeModelHandle::stream` had **zero** callers. `RuntimeStreamOutput` occurred four times: its
definition, its use as that method's return type, its construction inside it, and the re-export.
`RuntimeEventSeed` and `next_event` existed only to serve it. Nothing in `tests/`, `docs/` or the
skills references any of them, and nothing outside the crate can. **103 lines removed.**

**What the finding did not know, and it sharpens the case:** that cluster was the *sole* reason
`runtime_factory.rs` imported `RuntimeEventEnvelope`, `RuntimeEventType` and `serde_json::json`.
With it gone the file compiles without the runtime-event vocabulary at all — Rig primitives in
the factory, runtime events in the application layer, which is the boundary
`moira-rig-integration` describes.

*Which one to keep was not a coin flip, and it was measured.* Deleting all 103 lines left the
build and every suite green — nothing linked to them. A one-line mutation to the **surviving**
`execute_rig_stream` (dropping `text.push_str(&delta)`) reds two integration tests immediately.
The surviving loop is also the one carrying idle timeouts, backpressure, cancellation, TTFT
metrics and `mark_output_committed`; the duplicate had none of them, which is the divergence
hazard stated concretely.

*What stops it coming back:* **nothing mechanical, stated as such in the surviving function's doc
comment.** `dead_code` cannot see it — `pub` items in a library crate are exempt from that lint
whether or not anything calls them, which is exactly how ~95 lines survived. A source-scan
pretending otherwise would be a guard written to have a guard, which is what §3.4 is a list of.

### F29 — **CLOSED** `fix/f29-structured-output`. Gated parse in `execution.rs`, not at the Rig boundary — 2026-08-02

**One parse site, `structured_output_from_text` in `src/application/execution.rs`, called from both
`execute_rig_completion` and `execute_rig_stream`.** No change to `RuntimeCompletionOutput`,
`RuntimeStreamItem`, `ExecutionOutcome`, or `src/orchestration/runtime_factory.rs`. No OpenAPI drift
— `ExecutionOutcome.structured_output` is documented as a bare `{}` and its Rust type is unchanged;
verified by running `openapi_drift` rather than assumed.

**Why not the Rig boundary, which is where the in-tree skill points.**
`.agents/skills/moira-rig-completions/SKILL.md` says *"parse from `RuntimeCompletionOutput.text`"*.
`execute_rig_stream` **never constructs a `RuntimeCompletionOutput`** — it accumulates `text` itself
— so `output_from_response` would have covered the non-streaming path only and forced a second,
divergent implementation for streams, which is the "second response-narrowing site" the same
paragraph forbids. The skill's two instructions are in tension; the one about a second narrowing
site is the one that survives. CLAUDE.md already admits `rig_core` imports in `execution.rs`.

**The gate is the safety property, not an optimisation.** `wants_structured` is
`request.output_schema.is_some()`, captured **before** `request` is moved into
`handle.completion(request)` / `handle.start_stream(request)`. Without it, conversation
summarization corrupts: it sends **no** `output_schema`, `parse_summary` accepts any non-empty
prose, and `summarize_conversation` prefers `structured_output` over `output_text` via
`.map(|value| value.to_string())`. **Measured, not reasoned about** — an ungated build was built and
run on purpose, and stored

```
{"decision":"ship the invoicing rewrite in March","owner":"the user"}
```

where the model had sent the pretty-printed form. `summary_hash` is `request_hash` over the stored
bytes and is documented as a content address, so the row is internally consistent and wrong. The
gate went in only after that red was observed.

**Populate on success; do NOT fail hard.** A non-conforming reply leaves the field `None` and
changes nothing else, against the skill's `StructuredOutputInvalid` advice, for three reasons each
checked against the tree:

1. `StructuredOutputInvalid` is in **neither** `is_retryable` nor `is_fallback_eligible` nor
   `is_circuit_failure` — one non-conforming reply ends the execution with no retry, no fallback.
2. DeepSeek's `SUPPORTS_RESPONSE_FORMAT = false` drops the schema before the wire (**F39**), so
   every structured request on that route would hard-fail where it previously returned 200.
3. `run_extraction` detects failure by `output_text` being `None` and never reads
   `execution.status`, so `an_unparseable_extraction_reply_fails_the_run_and_writes_no_memory`
   would flip from `structured_output_invalid` to `extraction_call_failed`.

**Reversal condition — what makes someone adopt the fail-hard variant.** All three of: **F39**
landed, so the capability gate reflects Rig's per-provider reality instead of a config bool;
`StructuredOutputInvalid` given an explicit retry/fallback disposition (today it silently has
neither); and `run_extraction` reading `execution.status` rather than inferring failure from
`output_text`. Until all three hold, failing loudly trades a silent `None` for an outage on a
provider that was never going to comply. **F42** becomes true on the same day and not before.

**Strict, and deliberately not a scavenger.** `serde_json::from_str` on the trimmed text. Rig's
balanced-brace scan is not copied and **no code fence is stripped**:
`memory_extraction::parse_candidates` owns the one real-world tolerance, on the `output_text` it
already falls back to. Two parsers with two accept-sets over the same bytes is the parser
differential that module's doc comment refuses.

**Guards, and the mutation that killed each.** Every one was run, not read.

| Guard | Mutation | Result |
|---|---|---|
| `a_schema_carrying_completion_returns_the_parsed_structured_output` | unfixed code | RED — `left: Null, right: Object {"a": Number(1)}` |
| `a_schema_carrying_stream_returns_the_parsed_structured_output` | unfixed code; **and** `wants_structured = false` in the stream path only | RED both times, and it is the **only** case that reds on the second — the two paths are independently covered |
| `a_reply_that_is_json_is_not_parsed_when_no_schema_was_requested` | delete the gate | RED |
| `a_summary_that_is_valid_json_is_stored_verbatim` | delete the gate | RED, with the re-serialised body quoted above |
| `structured_output_is_parsed_only_when_a_schema_was_requested` (unit) | delete the gate | RED — and it needs no database, so the gate stays observable if a fixture stops being reachable |

`a_summary_that_is_valid_json_is_stored_verbatim` **passes against unfixed code** and is a
regression guard, not a watched-failing test. It was earned the only honest way: the ungated parse
was implemented first, the case was watched going red, and the gate was added after. The reply in
it is pretty-printed on purpose — a compact JSON reply round-trips through `to_string()` identically
and the guard would pass against the defect.

**[SUPERSEDED 2026-08-02 — F38 is now CLOSED on `fix/f38-deadline-usage`; see the F38 closure
section above. The reasoning below is preserved because it is what deferred the decision, and two
of its premises turned out to be weaker than they read: `UsageSummary::default()` is `None`
("unknown"), not a zero that retention would overwrite, and `terminal_update_from_outcome` already
runs on the `Failed` branch, so the outcome was *already* the source of the response row's usage
and output hash.]**

**F38 is NOT fixed and stays open.** `execution.rs`'s terminal-persistence-deadline arm calls
`failed_outcome` from inside `match result { Ok(Ok(output)) => … }`, where `output` is live. F29
turns its `structured_output: None` into a third silent drop alongside `output.text` and
`output.usage`. **The drop is now commented rather than silent**, naming all three values and F38.
Not fixed here because the arm's own condition is that terminal persistence did *not* complete:
`update_attempt`, `insert_usage_record` and `touch_credential_used` may each have failed to commit,
so promoting the usage onto the outcome asserts a billing fact whose row may be absent, and
promoting `output_text` onto a non-`Succeeded` status changes what every consumer of a failed
execution receives. That is a billing decision; burying it inside a parsing change would hide it.

### F38 — **CLOSED** `fix/f38-deadline-usage`. The outcome keeps what the provider produced — 2026-08-02

**The finding was right, and two facts it did not have made the decision easier than it looked.**

**Decision: `output_text`, `structured_output` and `usage` are all retained on the outcome;
`status` stays `Failed`; the retry/fallback clamp is untouched.** One arm changed, in
`src/application/execution.rs` — the `Err(_)` branch of the terminal-persistence timeout, the only
`failed_outcome` call site reachable from `Ok(Ok(output))`.

**Why the "this asserts a billing fact whose row may be absent" objection does not survive
contact.** Three things, each checked against the tree rather than reasoned about:

1. **`UsageSummary::default()` is all-`None`, not zero.** Every field is `Option<u64>`. The old
   outcome did not claim the execution was free — it claimed the token count was *unknown*, while
   the `attempts` array **in the same serialised document** carried the exact numbers. Retention
   replaces an absence of information with information. The mutation run confirmed the shape:
   `left: None, right: Some(2)`.
2. **The outcome is already the write path for the response row.**
   `terminal_update_from_outcome` in `application/public.rs` runs on the `Failed` branch as well
   as the `Succeeded` one, and copies `usage`, `output_text.len()` and the output hash straight
   onto `responses`. The zeroing was writing *"no tokens, zero bytes, no hash"* for a call the
   provider answered and will invoice. Nothing new is asserted; a wrong assertion is corrected.
3. **No `usage_records` row is created, so nothing is double-counted.** Billing reads
   `usage_records`, which on this arm is empty — the integration test asserts `count(*) = 0`. So
   invoicing still under-counts this execution. The difference is that `responses.usage`
   populated while `usage_records` has no row is now the **detectable signature** of exactly this
   condition, which was previously invisible in every surface at once.

**What it costs.** The deployment still eats the provider cost — this change does not bill anyone
for anything. It buys the ability to *find out*, and to reconcile against the provider invoice.

**Reversal condition — usage.** Re-zero it the day `ExecutionOutcome.usage` or `responses.usage`
becomes an input to customer invoicing rather than a reporting surface. Today the only readers are
`terminal_update_from_outcome` and the runtime diagnostic endpoint, both reporting. Once a billing
job sums the response rows, a `Failed` response carrying usage charges a caller who received an
HTTP error, and the deployment must decide explicitly whether this arm is chargeable instead of
inheriting the answer from a struct literal. **The trigger is concrete: any new reader of
`responses.usage` that is not a reporting or reconciliation surface.**

**Reversal condition — text.** `ConversationService::run_extraction` and `summarize_conversation`
infer "the model answered" from `output_text`/`structured_output` being `Some`, never from
`status`. Retaining the text lets both proceed on a reply that genuinely exists, which is the
correct answer to the question they are asking — the model *did* answer; only Moira's own
bookkeeping failed. If a **delivery** path (rather than an interpretation path) ever treats
`output_text.is_some()` as "show this to the end user" without checking `status`, this arm must
stop carrying text and those two sites must read `status` — which is also the third precondition of
F29's own reversal condition. The public plane is not such a path today: both `create_response` and
the streaming terminal branch dispatch on `outcome.status` first, and the `Failed` arm never
*delivers* the text — it returns `Err(AppError::coded(…))` / a `terminal_failure` SSE, and it does
not call `record_conversation_assistant`, so no conversation message is written from it. It does
**read** `output_text` on that arm, through `terminal_update_from_outcome`, but only to record
`output_text_bytes` and `output_hash` — which is the improvement, not the risk.

**`"output_committed": true` was inaccurate, and the audit entry was the wrong one — not the
outcome.** The brief asked which of the two is wrong. Neither, exactly: the value was a **hardcoded
literal**, never derived from anything, and the module uses the word in two incompatible senses.

- `EventCollector::output_committed` — the module's own definition — flips on the first chunk
  *accepted by the consumer*. On the non-streaming path nothing reaches the caller, so it is
  `false`.
- The `tracing::error!` line two statements above the audit call always said output *may* already
  be committed. It was the honest one.
- The existing integration test asserted `usage_records` count `= 0` in the same body, so the
  write group had not landed either.

So on a non-streaming execution the literal was false in **both** available senses, and the test
shipped in the tree **pinned it** — an instance of HANDOFF §3.4's "a test's own content literal,
not code". It is now read from `EventCollector::output_committed`, and a second key
`terminal_state_persisted: false` records what the arm actually knows.

**The clamp keeps its unconditional `true`, for a third and different reason, now documented.**
`terminal_persistence_deadline_failure()` passes `output_committed = true` into
`attempt_timeout_failure` to force `retryable = false` / `fallback_eligible = false`. That is not a
claim about delivery — it is the fact that *the provider's tokens are already spent*, so a retry
buys a second answer at a second cost whether or not a byte reached the caller. Making that
argument conditional would let a non-streaming execution be retried against a provider that has
already billed for the answer. **`ExecutionFailure` classification is otherwise unchanged**, as the
brief scoped it: nothing here required it, and the three preconditions for strict structured-output
failure still have only one landed.

**Guards, and the mutation that killed each.** Every one was run, not read.

| Guard | Mutation | Result |
|---|---|---|
| `a_terminal_persistence_breach_clamps_retry_and_keeps_the_result_it_could_not_persist` | restore `failed_outcome(...)` | RED — `left: None, right: Some("committed-output")` |
| `a_streamed_terminal_persistence_breach_reports_the_output_the_caller_already_received` | restore `failed_outcome(...)` | RED — `left: None, right: Some("firstsecond")` |
| both | retain the text, re-zero **only** `usage` | RED both — `left: None, right: Some(2)`. The billing half is independently pinned; the text assertion is not shadowing it |
| non-streamed case | restore the hardcoded `"output_committed": true` | RED — `left: Bool(true), right: false` |
| streamed case | *same mutation* | **GREEN, correctly** — the caller really did receive the deltas there. The pair is what proves the value is derived rather than constant; one case asserting `false` everywhere would have been the toothless version |

**Renamed test.** `terminal_persistence_timeout_is_recorded_as_output_committed_not_as_a_plain_failure`
→ `a_terminal_persistence_breach_clamps_retry_and_keeps_the_result_it_could_not_persist`. The old
name asserted the thing F38 found to be false. It is cited in
`plans/04-durability-correctness.md:359`, which is now stale on the name only.

**Cheapest edit that breaks the property while leaving the guards green.** Populate the outcome
from a *different* successful-attempt source than `output` — e.g. re-reading the attempt row —
which would produce the right numbers here and diverge whenever the row did not commit. The
`outcome.usage == attempt.usage` conjunction is asserted as one fact for that reason. The residual
gap is the `responses` row: neither case drives `PublicService`, so
`terminal_update_from_outcome`'s handling of a `Failed` outcome carrying usage is reasoned about
above but not pinned by a test. A public-plane case that forces a terminal-persistence breach would
close it.

**Gates:** `ALL GATES PASSED` — fmt, clippy, **test (1049 passed, 42/42 integration targets logged,
zero DB-skip lines)**, release, deny, audit (the one expected allowed warning, RUSTSEC-2026-0221).

### F41 is WRONG as recorded — there is no skill-tree drift

Found while following the brief that cited it. `.claude/skills/moira-rig-completions/SKILL.md` is
**eight lines**, and its body is:

> Read and follow `../../../.agents/skills/moira-rig-completions/SKILL.md` completely. That
> canonical workflow is shared by Codex and Antigravity and is authoritative for this repository.

So the zero occurrences of "structured" in the `.claude/` copy are not drift — it is a **pointer
file**, and every one of the eight skills under `.claude/skills/` has the same shape. CLAUDE.md
pointing at `.claude/` therefore routes an agent to `.agents/` by design; the authoritative
instruction is exactly one hop away, not in "the tree nobody is told to read".

The finding's underlying observation (a `grep` for "structured" in `.claude/` returns nothing) was
correct; the **inference** from it was not. This is the F32/F29 shape a third time: a verified
detail compressed into a confident generalisation. **F41 should be struck, not fixed.**

### F34 — ESCALATED: summarization is inline too, ungated, and the docstring says otherwise

Found by the F28 re-verification, and **worse than the finding that turned it up**.

`record_assistant_response` awaits `extract_memories` and then, on the very next line,
`maybe_summarize_after_turn` — which builds its own `ExecutionCommand` and makes a second
non-streaming completion call. So the window between the last content delta and the terminal SSE
event can hold **two serial provider round-trips, not one**.

**The docstring above `extract_memories` states that summarization is "specified as *enqueued*".**
It is not; it is inline, on the same path, in the same await chain. Any latency budget derived from
that comment is understated by an entire model call. This is the F31 shape again: prose in the tree
asserting a behaviour the code contradicts.

**Extraction is gated by `automatic_extraction_enabled` (default false). Summarization has no
equivalent gate.** So the half of this cost that nobody opted into is the half that runs by default.

**Reversal condition:** none — this is a defect, not a decision. It closes when summarization is
either gated to match extraction or moved off the response path.

### F33 — ESCALATED: five encryption-at-rest columns exist and nothing writes or reads any of them

`migrations/0007` creates `conversation_messages.content_encrypted`,
`conversation_summaries.summary_text_encrypted`, `memory_records.content_encrypted`,
`rag_document_versions.content_encrypted` and `rag_chunks.chunk_text_encrypted`.

**Nothing in `src/` touches any of them.** The schema says content can be encrypted at rest; no
cipher exists anywhere in the tree.

**Needs a human, not an autonomous change.** Envelope encryption is key custody, key rotation, and
plan 11's still-open Decision 3 — a scoping question, not an implementation gap. Recorded here so
the columns are not mistaken for a partially-built feature by whoever finds them next.

### F32 — a data-protection policy that protected nothing — **CORRECTED, then fixed**

**My own framing of this finding was wrong in the direction that mattered, and the correction is the
finding.** I briefed it as an *unused column* whose effect was "emergent": setting `'none'` yielded
no extraction because there was no plaintext to extract.

**There was no plaintext protection at all.** `add_message` binds `content_plain` unconditionally —
verified on `main`, the write path never mentions the policy. So a deployment setting
`conversation_content_persistence = 'none'` stored **full message plaintext**, and extraction ran
exactly as under `'plain_content'`. An operator configuring PII or data-residency controls received
none, with the API reporting success. Proven by mutation M1, which reintroduces the pre-fix line.

The ledger's *original* F32 wording was right — "a state no configuration can currently produce."
The gloss I added on top of it was not. **A finding's later summary can be worse than its first
draft**, and mine was.

**`encrypted_content` was accepted while nothing encrypts** (see F33). Storing plaintext under a
value literally named for encryption meant the *API itself* was doing the misleading, not merely
failing to act. It is now refused on write (`conversation_content_persistence_unsupported`, 422) and
**fails closed** for rows that already hold it.

**Enforcement sits at `add_message`**, in the existing `for update` lock query — the only path into
`conversation_messages`, so a fourth writer *inherits* the policy rather than having to remember it.
Having to remember it is what F32 was.

**Not auto-merged.** A 422 on a previously-accepted value will break any deployment setting
`encrypted_content` in IaC — loudly, which is the point, but that deserves human sight.

### F37 CLOSED · F34 CLOSED · F36 REFUTED — `fix/f36-summarization-path`, 2026-08-02

Three items on one path — the tail of `record_assistant_response`. Two needed code. The third's
premise did not survive being checked, so it is documented and left open with a condition that can
be measured instead of argued.

**F37 — CLOSED, but three dead reads removed, not four.** Both load-bearing claims hold.
`decide_summarization`'s first line is `if !policy.enabled`, tested **before** the `force`
short-circuit, so `force: true` never bypassed it and still does not; the hoisted guard calls
`summarization_skip_error(SummarizationSkip::Disabled)`, the same constructor `plan_summarization`
reaches through `map_err`, so the 403 / `summarization_disabled` /
`moira.error.summarization_disabled` envelope is byte-identical; and authorization and the archived
check are above the policy read and are untouched.

**The proposed placement was not semantics-preserving, and that is the correction.** Immediately
after the policy read puts the guard *above* `find_conversation_context_anchor`, whose `None` is
this function's `conversation_not_found` 404. `find_conversation_authorized` filters
`public_id = $1 and deleted_at is null and <access>`; the anchor filters
`public_id = $1 and deleted_at is null` — a strict superset — so the two disagree only when a soft
delete lands **between the two statements**. Narrow, but it turns a resource verdict into a policy
one, and no test can pin the difference precisely because no reachable state produces it. The guard
sits one statement lower.

`summarize_conversation_unscoped` goes from **6 database round-trips to 3** on the default
configuration. Counting the whole tail — `extract_memories` pays two policy upserts before its own
flags are read — a conversation-linked turn goes from **8 to 5**.

**The test story, stated rather than dressed up.** There is no watched-failing test for a refactor
that preserves semantics and none is claimed.
`force_does_not_bypass_a_disabled_summarization_policy` passes identically either side and is what
pins the envelope. **No query-count test was added**, deliberately: the two available instruments
are `pg_stat_database`, whose asynchronous flush is exactly F28's flake shape, and an in-process
`tracing` counter, which cannot see these queries at all because the HTTP server and the response
path run on spawned tasks that do not inherit a scoped subscriber and the process holds one global
default. Both would trade a real flake for a signal the existing suite already constrains.

**What was added guards the mutation that actually threatens the change.**
`a_disabled_policy_does_not_pre_empt_the_access_and_archived_checks` asserts both refusals under
`summarization_enabled: false`, the only configuration in which the new guard fires. **Both
mutations were run and watched**: hoisting the guard and its policy read above
`find_conversation_authorized` reds the first assertion (403 where 404 is expected); above the
archived check reds the second (403 where 409 is expected).
`an_archived_conversation_is_refused_with_conversation_archived` re-enables the policy before
archiving, so it stays green through both and could not stand in.

*Reversal condition:* if the anchor's lateral join ever measures as material on the disabled path,
move the guard above it and accept 404 → 403 inside the delete window.

**F34 — CLOSED, with one qualification to the finding's arithmetic.** The docstring above
`extract_memories` said Sub-Phase E's summarization "is specified as *enqueued*" and offered that
as the reason extraction alone doubles a turn. E did not ship enqueued either:
`maybe_summarize_after_turn` is awaited on the next line of `record_assistant_response` and makes
its own completion call. Corrected in the docstring and in `docs/memory-extraction.md`, which now
carry the same table. (`docs/conversation-summarization.md` was already correct — it says inline
under "Known limits" — so the drift was in two places, not three.)

**"A fully-enabled turn issues three provider calls" is true only on the turns that trigger.**
Extraction is per turn; summarization runs only once `minimum_messages_since_summary` **and**
`summary_trigger_tokens` are both crossed, so its amortised cost is one call per
`summary_trigger_tokens` of conversation. What ran every turn regardless was summarization's *read*
path — which is F37. *Reversal condition:* the tables stop being true the moment either feature
moves off the response path; whoever moves one corrects both.

**F36 — REFUTED as stated. Documented, not fixed, and left open with a sharper condition.** The
finding says the lock is held "on a path now reached every turn". The *enclosing function* is
per-turn; `SummarizationLock::try_acquire` is not — it sits below `plan_summarization`'s `?`, so it
is reached only once `decide_summarization` has said yes. That is exactly the rate the lock's own
doc comment argues about, so **its reversal condition has not been met** and nothing about the lock
was changed. Two things are real and are now written down in numbers:

- **Duration is bounded.** `run_summarization` uses `ExecutionOptions::default()`, so `timeout_ms`
  is `None` and `execute` falls back to `runtime.default_execution_timeout_seconds` — 120 s in
  `config/default.toml`, clamped by `maximum_execution_timeout_seconds`. No caller can extend it.
  The finding's "for the entire attempt timeout" reads as unbounded; it is not.
- **Count is bounded by nothing in the tree.** `detach` removes the connection permanently and the
  pool opens a replacement, so a run costs one backend *beyond* `database.max_connections` (10).
  The caller's execution permit is released before `record_assistant_response` runs, so
  `runtime.global_execution_concurrency` does not bound how many turns are inside a run at once —
  only the in-flight request count does.

Three cheaper-looking fixes were considered and each is worse; all three are recorded on the type
so the next reader does not re-derive them. A **pooled** session lock is re-entrant, so the next
checkout would believe it holds someone else's lock. **Dropping the lock** and letting
`conversation_summary_boundary_unique` arbitrate makes the loser pay for a completion and then
receive a unique violation as a 500. A **second, tighter timeout** shortens a duration that is
already bounded and does nothing to the count.

*Reversal condition, sharpened:* replace the advisory lock with a lock-table row and a bounded
lease when concurrent summarization **runs** — not turns — can approach the server's spare backend
headroom, measurable from `moira_summarization_runs_total` against the deployment's
`max_connections`.

**The briefed baseline was wrong.** It stated `main` at **1021** tests as of `7bb5f15`. Measured
directly in this worktree — the four touched files replaced with `git show origin/main:` content,
then `cargo test --workspace --all-features`, 41/41 targets logged and zero skip lines — `main` is
**1027**. This branch is **1028**, exactly one more: the diff adds one `#[tokio::test]` and removes
none. Anyone reconciling a count against 1021 will chase a phantom of six.

**And a gate-log hazard worth adding to HANDOFF §2.2.** The first run here was started before the
edits landed and reported a *plausible* 1027 for this branch. It was wrong: `scripts/gates.sh`
redirects `cargo test` to a `mktemp` file and only the summary line reaches the outer log, so the
outer log's tail sits on `── test` for the whole phase and then shows release-build output — which
reads exactly like the test phase still compiling. Editing sources during that window silently
splits the run: `fmt` and `clippy` had already passed against the *old* tree while `release` built
the new one. **Do not edit sources while `scripts/gates.sh` is running**, and if you did, the run
proves nothing about what you edited.

### F47 — the `get_or_create_*_policy` readers are UPDATEs, and one turn issues three of them

Found while counting F37's round-trips; nobody asked for it. Both
`get_or_create_conversation_policy` and `get_or_create_memory_policy` are

```sql
insert into … (application_id) values ($1)
on conflict (application_id) do update set application_id = excluded.application_id
returning *
```

The `do update` is the usual trick for getting `returning` on the conflict path, and it is a real
heap write: a new row version and a WAL record on **one row per application**, every call.

A conversation-linked turn calls `get_or_create_conversation_policy` **twice** — once in
`extract_memories`, once in `summarize_conversation_unscoped` — and `get_or_create_memory_policy`
once. So an application's two policy rows are rewritten three times per turn on the default
configuration, where every one of those reads ends in an early return. Two consequences:
dead-tuple churn concentrated on two rows, and a row-level lock that briefly serialises concurrent
turns of the *same* application against each other.

Not fixed here. The honest fix is `on conflict do nothing` plus a `select`, or read-then-insert,
and it touches every `get_or_create_*_policy` family rather than the two on this path — a change
about policy reads, not about summarization. *Reversal condition:* it closes when the read path
stops writing.

### F47 — **CLOSED** `fix/f40-f47-response-output-and-policy-reads`. The read path stops writing, and the consequence was three times larger than recorded — 2026-08-02

**The finding was right and understated.** It named dead-tuple churn and a row lock. Measured
against the live schema, those are two of **four** things one `get_or_create_*_policy` call did,
and the two it missed are the expensive ones. Every measurement below was taken with `psql`
against the real tables, not argued from the SQL:

| what a "read" did | evidence |
|---|---|
| wrote a new heap tuple + WAL record | `xmin` `1067657 → 1067658 → 1067659 → 1067660` and `ctid` `(0,1) → (0,2) → (0,3) → (0,4)` over three calls |
| **bumped `version`** — the `ETag` served on `GET …/policy` and demanded back on `If-Match` | `version 1 → 2 → 3 → 4` over the same three calls, via the `<table>_bump_version` trigger; `updated_at` moved each time too |
| **fired `pg_notify('moira_runtime_config', …)`** | `LISTEN` on the channel received **exactly three** notifications from three `do update` calls and **zero** from three `do nothing` calls |
| took a row-level lock | as recorded |

The third is the one that matters. `apply_invalidation` (`src/infra/db.rs`) calls
`cache.invalidate_all()`, `runtime_handles.invalidate_all()` and `auth_settings.invalidate_all()`
on **every** notification, so a conversation-linked turn — three policy reads — wiped every
replica's runtime-config cache and every cached provider client handle **three times per turn**.
The second means an operator's `If-Match` on a policy `PUT` could be invalidated by unrelated
traffic on the same application.

**The family is FIVE, not two, and the count in the finding's own fix sketch was the trap.** Four
live on `PgConversationRepository` (conversation, memory, retrieval, embedding) and all four had
the `do update` spelling. The fifth is
`PgPublicRepository::get_or_create_application_execution_policy`, and it is the interesting one:
it **already read first**, so it never had any of the write amplification above — and its insert
carried **no `on conflict` clause at all**. Two concurrent first requests for a new application
therefore raced, and the loser got
`duplicate key value violates unique constraint "application_execution_policies_pkey"`.
Reproduced directly in Postgres, then reproduced through the repository under a barrier. It sits
on the hot path of every `POST /v1/responses`.

That inverts the brief's warning. It said the row lock being removed "is currently what makes
[the race] impossible" — true for the four writers, and **false for the fifth, which never had
the lock and already had the bug.** Done properly the fix *removes* a correctness bug rather than
trading one for throughput.

**The race is closed by a fresh snapshot, not by a retry.** All five now share
`src/infra/repositories/policy_row.rs`: `select` → `insert … on conflict do nothing returning` →
`select`. Each statement runs on its own pooled connection at `READ COMMITTED`, so the second
`select` takes a **new** snapshot and sees a row a concurrent inserter committed while the
`do nothing` was waiting on its speculative insertion. Verified in Postgres: the losing session
returns `INSERT 0 0` and the following `select` returns the row. Steady state is now **one
`SELECT`** — cheaper than what it replaced as well as silent. The bounded `ATTEMPTS` loop exists
only so a row deleted underneath the caller ends in a coded error rather than a spin.

**Guard: `tests/policy_reads_do_not_write.rs`, five cases, and it asserts on the write.** A guard
that checked "the policy comes back" passes in both arrangements — the returned value was never
wrong. So the assertions are `xmin`, `version`, and the **absence of a runtime-config
notification**, the last one closed with a sentinel `pg_notify` rather than a timeout so it is an
acknowledgement gate and not a delay. The silence is checked against a control on the same
listener — a genuine `put_*_policy` must still be announced — because an assertion that nothing
arrived is worthless if nothing *can* arrive, which is F16's shape.

**Six mutations run, each reverted:**

| mutation | result |
|---|---|
| conversation policy back to `do update` | red — three notifications, naming that table |
| **memory** policy back to `do update`, conversation left fixed | red — three notifications, naming *that* table. Per-member coverage is real, and reverting one member is the cheapest edit |
| execution policy back to `select` + bare `insert` | red — `a concurrent first touch of application_execution_policies failed: database error` |
| delete the insert entirely, select only | red ×3, loudly |
| `ATTEMPTS = 1` — no re-select after a conflicting insert | **red on the concurrency case only.** This is the "cannot return `None` for a row that exists" mutation, and it proves the second select is load-bearing |
| `select … for update` | **`a_policy_read_is_not_a_write` stayed GREEN** |

The last one is the §3.4 answer and the reason there are five cases and not three. `for update`
restores the serialisation half of F47 while writing nothing — no tuple version, no `version`
bump, no notification — so every write assertion held. That is the cheapest edit that breaks the
property and satisfies the guard, and it now has its own case:
`a_policy_read_does_not_wait_for_a_row_lock` holds an exclusive lock in an uncommitted
transaction and requires the read not to wait. **Found by asking the question and then running
the answer**, which is the only step that has ever worked here.

**The known state that collapsed two variables, and the case that separates them.** Pinning every
row to "already exists" makes the signatures exactly comparable — and makes *read-then-insert*
and *select-only* indistinguishable, because no policy is ever missing.
`a_first_touch_creates_the_row_and_says_so` separates them, and
`concurrent_first_touches_all_succeed` separates both from the bare-insert spelling that actually
shipped. A fifth case pins `POLICY_TABLES` against `information_schema`, because every other
assertion iterates a hardcoded list of five and a sixth `application_*_policies` table would be
watched by nothing.

*Reversal condition:* F47 reopens if any `get_or_create_*_policy` performs a heap write, takes a
row lock, or emits a `moira_runtime_config` notification on the path where the row already
exists — all three of which `tests/policy_reads_do_not_write.rs` now observes directly. It also
reopens if a new `application_*_policies` table is added without being added to `POLICY_TABLES`,
which the schema-pinned fifth case reds on.

**Raised F51** while measuring the notification half: the channel is attached to `conversations`
and `memory_records` as well as the configuration tables, and `apply_invalidation` ignores
`circuit_reset_scope` for three of the four things it clears. F47's fix does not touch that.

### F40 — **CLOSED** `fix/f40-f47-response-output-and-policy-reads`. Premise refuted; the reason it gave was the real defect — 2026-08-02

**The premise does not hold, and establishing that was most of the work.** F40 says
`GET /v1/responses/{id}` returns an empty `output` array "for a completed, persisted response".
That state is unreachable. `output_persisted` is written by exactly three constructors —
`terminal_update_from_outcome`, `failure_update`, and the stream-start failure arm — and **all
three hardcode `false`**; the column defaults `false`; nothing anywhere in `src/` writes `true`.
`docs/response-persistence.md` already said so. The old condition was
`Completed && !output_persisted`, so `Completed` *always* produced `OutputUnavailable`, and
`Vec::new()` was reached only by `Queued`, `InProgress`, `Failed` and `Cancelled`.

**Is `[]` right for those four?** Yes, and it is left alone. They have genuinely produced no
output, `status` already says which, and converting them would be a public-shape change with
nothing behind it. `only_completed_responses_carry_an_explanation` pins that, so the fix cannot
drift into "always explain".

**Is the output retrievable anywhere?** No — and this is the question the product call turned on.
There is **no column that stores response output text**. `responses.output_summary` holds
`{persistence_mode, output_text_bytes, output_hash}` — a length and a *peppered* hash, not the
text. The one surviving copy is `conversation_messages.content_plain`, written by
`record_assistant_response`, and only for conversation-linked responses. **Serving it from
`get_response` was considered and rejected**: that endpoint authorises `moira:responses:read`,
while conversation content is governed by `moira:conversations:read` plus the conversation
policy's `conversation_content_persistence` (which F32 shows is enforced by nothing, on a fix
still unmerged in PR #57). Widening an authorisation boundary to improve an explanatory string is
the wrong trade, and it is the same reasoning `citations_from_link` already gives for not
re-resolving `context_plans` on this path.

**So: not retrievable ⟹ say so, and say it accurately. Two defects, both real, both fixed.**

1. **The reason was a lie for three of the four persistence modes.**
   `reason: "metadata_only_persistence"` was a literal, emitted whatever the application had
   configured. It is correct for the default and false for `none`, `plain_content` and
   `encrypted_content` — worse than no explanation, because it names a cause the operator did not
   choose and sends them to change a setting that is not the reason. Now derived from the
   `persistence_mode` recorded in `output_summary` **at completion time**: `none` →
   `persistence_disabled`, `plain_content`/`encrypted_content` →
   `content_persistence_not_implemented` (nothing in the tree honours either; see also F33's five
   unwritten encryption columns), everything else → the previous literal.
2. **`Completed && output_persisted` fell through to `[]`** — the more the row claimed to have
   persisted, the less the endpoint returned. Unreachable today and now named
   `persisted_output_not_loaded` rather than left silent, so whoever implements content
   persistence meets a string that says what happened instead of the empty array F40 reported.
   This is the F48 shape: latent, made loud, behaviour on every reachable path unchanged.

**The invariant is now stateable: a completed response never carries an empty `output`.** Either
the text, or a reason. That matters because a completed response *can* legitimately have no
content — a model returning an empty string — and it is served as `OutputText { text: "" }`,
never as `[]` and never as `output_unavailable`. Pinned by
`an_empty_model_reply_is_output_text_not_output_unavailable`, because if `Succeeded` could arrive
with `output_text: None` the whole distinction would collapse.

**Public shape: unchanged. Value: changed, narrowly, and deliberately.** `reason` is
`{"type": "string"}` with no `enum` in `docs/openapi.json`, so no schema moved — the committed
snapshot is byte-identical and the counts hold at **152 operations / 100 paths / 183 schemas**
(re-derived from the document, not copied from a brief). The *value* changes only for
applications not on the default `metadata_only`, which are exactly the ones being told something
false today. **This is a much weaker break than F32 or F46** — F32 refused a previously-accepted
input value with a 422 and was held for human sight; F46 refused a previously-accepted request
shape. This changes an explanatory string on a field documented as unconstrained, and the default
deployment sees no change at all. Recorded in the PR body; not held.

**Guards, at two levels, deliberately.** Five unit cases in `src/application/public.rs` pin the
whole mapping matrix — all four modes with a distinctness assertion, the `output_persisted` case,
every non-completed status, the empty-completion case, and the unreadable-mode fallback. Three
integration cases in `tests/response_output_honesty.rs` drive a real `POST` against a scripted
mock and read the real `GET` body.

**Why both, and the mutation that proves it was not redundant.** This is F49's lesson applied
before the fact: *"it asserts on the real wire" is not "it reaches the code you changed"*, and
its converse. The fix reads `output_summary.persistence_mode`; the unit cases build that field
themselves, so they cannot tell whether `terminal_update_from_outcome` writes it or whether
`find_response_authorized` selects it. Mutation **M10** removed `persistence_mode` from the
terminal update — **all 21 unit cases stayed green while two integration cases went red.** Unit
coverage alone would have been exactly the F49 trap.

**Four mutations run, each reverted:**

| mutation | result |
|---|---|
| re-hardcode the reason to `"metadata_only_persistence"` | red at both levels; the integration failure prints the whole response body |
| restore `&& !record.output_persisted` | red at both levels — the integration case reports `got []`, F40's exact symptom |
| explain every status, not only `Completed` | red — `Queued must return an empty output array` |
| **drop `persistence_mode` from the terminal update** | **unit green ×21, integration red ×2** — the wiring is genuinely under test |

*Reversal condition:* F40 reopens the moment anything writes `output_persisted = true`. At that
point `persisted_output_not_loaded` stops being a latent-state marker and becomes a real bug
report about `get_response`, which must then load and return the stored body —
`a_row_claiming_persisted_output_is_not_served_as_an_empty_array` is the case to rewrite, and it
flips the column by hand precisely so it is already sitting on that state. It also reopens if a
fifth `ResponsePersistenceMode` variant is added without an arm in `output_unavailable_reason`,
which falls back to the default answer silently.

## USER DECISIONS — 2026-07-31, taken interactively

1. **Findings before waves 4–5.** F20, F13, F17 and the Wave 2 leftovers first. F20 is the reason:
   Wave 5 is meant to build the ownership UI, and ownership is currently unreachable on any
   greenfield deployment.
2. **Ownership is a SINGLE primary, set at claim time.** The setup claimant becomes primary
   automatically, so a fresh deployment has one without operator intervention — that is the direct
   F20 fix. Transfer moves the flag; the last-primary guard prevents clearing it.
   *Reversal condition:* if a deployment ever needs several people able to manage admins
   independently, this becomes a set rather than a flag, and the last-primary guard becomes a
   last-any-primary guard. That is a schema change, not a config toggle.
3. **`cargo-mutants` on code a PR touches**, not the whole tree — a full run over 400+ crates is too
   slow to gate on. Rationale: hand-written mutations found **6 of 6** cases where a test passed
   against broken code, including one nothing caught, which is how F19's enumeration oracle
   surfaced. Reading a test does not tell you whether it works.
4. **Ban `file.rs:123` citations in plans**; cite symbol names, which do not rot. Measured staleness
   across five re-audited plans: **40%, 45%, 65%, 70%, 85%** — every one needed a rewrite before it
   could be implemented.
   **Keep the re-audit step regardless.** It is what caught plan 08's wizard being unable to ever
   succeed, plan 11 contradicting a committed test suite, and plan 09 extending a UI that did not
   exist. No citation format would have caught any of those — the ban removes drift *volume*, not
   the danger in it.

## WHAT THE FINDINGS-SWEEP BRIEF GOT WRONG — 2026-07-31

Recorded because the brief was assembled from *these* finding descriptions, so the errors are in
the ledger's own text and will be inherited by the next brief drawn from it.

1. **"`begin_admin_command` takes no advisory lock on any path, so `redeem_invite`'s doc clause
   describes something that does not exist."** The clause described the right *behaviour* and named
   the wrong *function*. `begin_admin_command` indeed takes none — but
   `PgAdminCommandTransaction::claim_idempotency`, which `AdminCommandRunner::execute` calls
   immediately after, takes a per-key `pg_try_advisory_xact_lock`. So a pre-envelope refusal really
   does skip an advisory lock, **whenever the request carries an `Idempotency-Key`**; without one
   the clause is vacuous rather than false. The comment was rewritten to name the real lock and its
   condition, not deleted.
2. **"`create_invite`/`redeem_invite` … an idempotent replay double-counts one invitation."** True
   for `create_invite`. **Unreachable** for `redeem_invite`'s success path, because the pre-envelope
   check refuses the consumed invite before the envelope is entered — see F21. The reachable
   double-count on that path is a *failure* replay, which the brief did not mention and which is the
   one that distorts an operator-facing denial-rate metric.

Both errors share a shape worth naming: **a conclusion that is right about the system and wrong
about the mechanism.** That is the same shape as the METHOD NOTE above (plan 09 §0's replayed-403
argument) and as F15's "a type was named as safe without reading its fields". Three occurrences now.
The practical rule: when a brief asserts a mechanism, verify the mechanism before writing the test,
because *the test follows the reason* — and a test written to a wrong reason passes in both the
fixed and the broken arrangement.

## COMPACTION DISCIPLINE — added 2026-07-31

This run is unattended and long, so context *will* be summarised. **The rule: this file, the plan
§0 sections, and git are the source of truth — never conversation memory.**

A checkpoint is safe to compact at only when **all** of these hold:

1. Working tree clean, everything committed **and pushed**.
2. **This ledger reflects reality** — merges recorded, findings recorded, in-flight work named with
   its branch. Checked on 2026-07-31 and it was *not*: two merged plans were missing. Verify, do not
   assume.
3. Every decision made since the last checkpoint is written into the affected plan's §0 **with its
   reversal condition**, not just into a commit message.
4. Running agents are named above with what they are doing, so a fresh context can pick up their
   notifications without knowing why they were spawned.

If any of those is false, **make it true first** — that is cheap, and re-deriving lost state is not.
The "State at a glance" block above exists precisely so a compacted context can resume from one read.

## Cycle log

### Cycle 17 — 2026-08-03 — F54 closed; the queue is empty; a marker-carrying commit shipped and was caught

`main` at `027af93`. **Eleven PRs merged across cycles 14–17.** Nothing actionable remains.

**F54 closed on a corrected premise — and the correction came from my own summary, not the finding.**
The cycle-16 paragraph said the extraction failure class was *"lost from `memory_extraction_runs`"*.
It was not: that column has existed since `0007` and has carried the class since F29's third
precondition. **F54's own entry said so; my one-line summary of it said the opposite, in the same
commit** — and the summary is what the next brief inherited, which made its first proposed fix
already-shipped work. Corrected in `b1bb9af`.

The real gap was *correlation*, closed by `0025`'s `execution_id` column. Not a FK, deliberately —
`execution_id` is no table's primary key, and a FK to `responses` would fail on exactly the
deployments that persist least. The argument against the cheapest option was not "documenting is
bad": **the schema had already answered this twice in the same migration** (`context_plans` and
`retrieval_runs` both carry bare indexed `execution_id`), so documenting the string convention would
have made `memory_extraction_runs` the only run table correlating differently.

**The `stricter_of` gap recorded as "bounded, not fixed" turned out closable** — `permissiveness()`
maps two consent modes to the same value deliberately, so a tying pair exists.

#### ⚠️ A thirteenth form of "exit codes lie", committed by this loop

**A conflict-resolution script failed its assertion, and `git add; git commit` — chained with `;`
rather than `&&` — staged, committed and pushed the file with `<<<<<<<` still in it.** The commit
output looked entirely normal.

Form 3's shape applied to a merge: *a failed step followed by a succeeding one reads as success.*
Caught by grepping the pushed branch, fixed in `8ab8cac`, recorded as HANDOFF §2.2 form 13.
**Two habits close it:** chain with `&&`, and make the resolver assert *zero markers remain* **before
it writes** — a wrong line number must not be able to produce a half-resolved file.

#### Two more corrections to briefs I wrote

- **"the shared DB is at `0024`"** — true and misleading. `moira` is only the *origin*; each test
  clones a migrated **template** which was at 25. `select max(version)` against `moira` reads 24 and
  is **not** evidence a migration failed to apply. Nearly caused a real green to be read as a lie.
- **"summarization has the same shape"** (in F54's own entry) — false. `conversation_summaries` has
  no `status`, no `failure_class` and no run row; a failed summarization writes nothing but a metric
  counter. Raised as **F55**, deliberately unfixed: a new table and a design question, not a column.

#### The record across this run

**Four findings were refuted where they aimed and real somewhere else** — F40, F43, F30, F53 — and
F53's evidence had been destroyed by the commit that raised it. **Thirteen guards** have now been
found that could not fire, several already shipped and trusted. The question that found nearly all of
them: *what is the cheapest edit that breaks the property while leaving the guard green?*


### Cycle 17 — 2026-08-03 — F54 closed on a corrected premise; F30's recorded gap closed

Branch `fix/f54-extraction-correlation`, three commits, one gate run — **ALL GATES PASSED**, 1102
tests, the one expected allowed `RUSTSEC-2026-0221`.

| item | verdict |
|---|---|
| **F54** | **CLOSED, premise partly REFUTED.** Migration `0025` adds `memory_extraction_runs.execution_id` |
| **F30's recorded `stricter_of` gap** | **CLOSED — it was closable, not unclosable.** The tying pair exists in the enum |

#### F54's brief was wrong in the same direction twice, and the ledger's own entry was righter

The cycle-16 summary above says *"the extraction failure class is lost from
`memory_extraction_runs`"*, and the brief that carried it repeated that. **It is not lost.**
`memory_extraction_runs.failure_class` has existed since `0007…:312` and has carried the
*execution's own* class since F29's third precondition — which the F54 entry two sections down
states correctly (*"The run row now records the execution's failure class"*). The one-line summary
drifted from the entry it was summarising, in the direction of restating the finding it had just
narrowed.

**The consequence is that the brief's first candidate fix — "persist the failure class on
`memory_extraction_runs`" — was already shipped**, and had it been taken it would have duplicated
a column onto itself. The second candidate ("persist the execution/response id as a real column,
making the correlation a foreign key") was also wrong in its detail: `response_id` is **already**
a real FK on that table and is **already taken** — it references the *triggering turn's* response,
which is a different execution from the extraction's own. *Read the entry, not the summary of the
entry; and check whether a proposed column already exists before costing it.*

#### What the fix is, and why the cheapest option was not right

`execution_id uuid` plus an index, written when the run row is **opened**.

**Not a foreign key, and this is not a compromise.** There is nothing to reference: `execution_id`
is not the primary key of any table — it is `unique` on `responses` and a plain indexed column on
`execution_attempts`. A FK to `responses(execution_id)` would additionally fail *exactly* on the
deployments that persist least, since a `responses` row exists only when `persistence_mode` says
so while `execution_attempts` and `audit_logs` are written regardless.

**The argument that beat "document the convention" is not that documenting is cheap-and-bad; it is
that the schema had already answered this question twice, in the same migration.**
`context_plans.execution_id` and `retrieval_runs.execution_id` (`0007…:448`, `0007…:467`) are both
bare indexed uuids on run tables solving this exact problem. Extraction was the odd one out.
Documenting the string convention would have made `memory_extraction_runs` the only run table in
the schema that correlates differently — and the convention is a `varchar(128)` with no format
constraint, so the documented join would still have been `like 'memory-extraction-%'` with a uuid
parsed out of it. There was also a **fourth** option the brief did not list and which is genuinely
the cheapest: put the id in the existing `metadata` jsonb and skip the migration entirely. It
loses the index and the type, on a column whose only consumer is an operator writing SQL.

**Written at open, not at completion.** `insert_memory_extraction_run`'s doc comment celebrates
that a run dying mid-call leaves a `'running'` row rather than no row. That row is the one an
operator most needs to correlate, and it never reaches `complete_memory_extraction_run` — so
recording the id at completion would have left it null precisely there.

**The barrier is the type.** `MemoryExtractionRunInsert.execution_id` is a required `Uuid`, not an
`Option`, so no writer can open a run row without naming the execution it is about to run. The
column is nullable only for rows predating `0025`.

**Reversal condition:** if executions ever get a table of their own with `execution_id` as its
primary key, this column becomes a real foreign key to it in the same change. Until then the type
on the insert struct is what holds the invariant, and it must not be relaxed to `Option` to make a
new call site compile.

#### The correlation guard, and the naive version that would have shipped green

`a_failed_extraction_run_names_the_execution_that_failed`. It does **not** assert the column is
populated; it resolves it against `execution_attempts`, which the *execution kernel* wrote.

| mutation | observed |
|---|---|
| `run_extraction` mints its own id again, ignoring the one the run row was opened with | **red** — *"the run row must name the execution that failed, not a uuid minted somewhere else"*. The column is non-null and indexed and names nothing |
| the same mutation, against a guard weakened to `assert!(execution_id.is_some())` | **GREEN.** Run deliberately, and it is the whole reason the guard joins |
| `insert_memory_extraction_run` binds `None` | **red** — *"a run row must name the execution it ran, even when that execution failed"*. Only this test reds; the other 22 stay green, so nothing else in the tree covered this |
| the run row records the **caller's** execution id (the realistic confusion, since `response_id` on the same row points at that turn) | **red** on the same `assert_eq!` — and see below |

**The fixture always produces two executions** — the caller's turn and the extraction — so an id
that merely resolves to *some* attempt row proves nothing. The caller's succeeded and the
extraction failed, which is what makes them tellable apart.

**What running the fourth mutation found, and it is about the guard rather than the code.** The
guard also carried an explicit `assert_ne!(run_execution_id, callers.execution_id)`. That
assertion **can never be the one that fires**: with the two ids already asserted distinct, it is
implied by the `assert_eq!` above it. It was removed in its own commit. *An arm no test can
execute is a promise, not a guard* — §3.4's rule, applied to a guard written by an author who had
just read it, which is the third time that sequence has happened here.

**Cheapest edit that breaks the property while leaving the guard green:** none found. Writing the
id at completion instead of at open is not cheap — it requires moving the field from the insert
struct to the outcome struct, because the insert takes a non-optional `Uuid` and the outcome has
no such field. That is a redesign, not an edit.

#### F30's recorded gap — CLOSABLE, and now closed

The gap was *"swapping `stricter_of`'s arguments at the `memory_behavior` call site survives its
guard"*. The brief asked whether a pair of **different** values that **tie** exists in the enum,
and said that if none did the gap would be unclosable by construction and should be left recorded.

**Such a pair exists.** `permissiveness()` maps `ApplicationManaged` and
`AutomaticWithUserControls` both to `2` — deliberately, and documented as such: they *"differ in
who asserted consent, not in how much is permitted"*. So the gap is closable, and
`the_reported_memory_behavior_resolves_the_consent_tie_toward_the_memory_policy` closes it with
both argument orders of that pair.

| mutation | observed |
|---|---|
| `stricter_of(memory, conversation)` at `effective_memory_behavior` — **the recorded gap verbatim** | **red**, and **only** the new test — *"conversation=AutomaticWithUserControls memory=ApplicationManaged: … left: "automatic_with_user_controls", right: "application_managed""*. 23 other cases green, which confirms this was genuinely the surviving edit |
| `effective_memory_behavior` reports the memory column alone (F30's shipped defect) | **red**, and **only the sibling** `…_is_the_stricter_of_the_two_consent_columns`. The new tie test stays **green** |

**The two cases are exact complements, verified in both directions rather than asserted.** The tie
test is *blind* to a memory-column-only read — because on a tie the memory column **is** the
answer — and the sibling is blind to the swap. Neither is a superset of the other, and the doc
comment on the new case says so explicitly so that neither gets retired for the other later.

**The value is bounded and the test states the bound in its own doc:** both tied modes permit the
same thing, so this can only change *which of two equally-permissive labels is reported*, never a
consent outcome. It is worth pinning because resolving the tie the other way would silently change
the value reported to deployments where nothing is wrong.

*Swapping the arguments at the **other** call site (`effective_extraction_status`) remains inert
and is not a gap: both tied modes map to the same `MemoryStatus`, which
`the_combined_consent_decision_is_symmetric` already pins.*

#### A correction to the F54 entry's own remedy

It said *"add `execution_id` … (and the summarization run equivalent)"*. **There is no
summarization run equivalent.** `conversation_summaries` has no `status`, no `failure_class` and
no run row at all — a summarization that fails writes **nothing** to the database beyond a metric.
That is a different and larger gap than F54's, and it was not expanded into here. Recorded as
**F55** rather than silently folded in.

#### What remains

Unchanged by this cycle, and none of it autonomous: **F50**'s fail-closed product decision, the
**structured-output fail-hard flip**, **F33**'s envelope-encryption scoping, **PR #57** (F32), the
**rig-core issue** in `docs/upstream/`, and the **T11 deploy**. Newly raised and also not
autonomous: **F55**.

### Cycle 16 — 2026-08-03 — the queue is empty; what is left needs a human

**Ten PRs merged across cycles 15–16.** `main` at `cddb2a5`. **Every finding that an autonomous loop
can honestly close is closed.**

| PR | Findings | Merge |
|---|---|---|
| #66 | F53, F50 made observable | `8d983aa` |
| #67 | F30 (partly refuted), both structured-output preconditions, F54 raised | `cddb2a5` |

#### Two more findings were refuted where they aimed and real somewhere else

- **F53's own evidence was destroyed by the commit that raised it.** It argued
  `docs/runtime-cache-invalidation.md` "lists neither table" — true when drafted, **false when
  committed**, because `e16cb0c` added the RAG tables to that paragraph. *A finding that cites a
  document its own commit edits cannot be re-verified later by reading that document.* It also named
  columns (`embedding model`, `dimensions`) that `rag_collections` does not have.
- **F30 is refuted as an extraction defect** — "takes the stricter" *is* implemented. But **its own
  predicted third reader had already arrived**: `conversation_select` reads the memory column alone,
  in SQL, so `GET /api/v1/conversations` reported `application_managed` while extraction refused
  under a conversation policy of `disabled`.

#### The rules earned this cycle

1. **A single point of truth is only a barrier if it is reachable from every layer that needs the
   answer, in the type that layer needs.** F30's rule was already in one place — an application-layer
   function over `Option<MemoryStatus>` that a SQL query could neither call nor use.
2. **A guard that iterates a constant cannot see a name being *removed* from it.** Set-membership
   guards are one-directional by construction; deleting `rag_documents` from the derived inventory
   left every unit test green.
3. **An arm no test can execute is a promise, not a guard.** Why `run_extraction` records the
   execution's own failure class rather than special-casing an unreachable variant.
4. **Checking a claim can change the fix.** Precondition 3's "the only signal" was overstated — the
   class survives in `audit_logs` and `execution_attempts`. The narrower truth (lost from the
   operator-facing `memory_extraction_runs`) is now **F54**.

#### Decisions taken, each with what would reverse it

- **F50: observability shipped, product decision untouched.** Silence is a defect under *either*
  answer; fail-closed and observable fail-open differ only in whether the request is *also* refused.
  **Recommendation if the decision is taken: a per-route opt-in, not a global mode** — a disabled
  profile is a configuration state an operator chose, and refusing turns one admin toggle into a
  route-wide outage. Fail-closed is defensible only if the preamble is a *security control* rather
  than a behaviour default, and nothing in the tree frames it that way.
- **`StructuredOutputInvalid` stays out of all three dispositions**, now recorded and guarded rather
  than true by omission. Fallback was **nearly yes** and flipped to no because **F39 already removed
  the real case at routing time**, and because a class carries one disposition while this class has
  two emitters — admitting it lets one malformed caller schema walk the whole fallback chain.
- **The fail-hard flip is NOT shipped.** Its reversal condition now holds in full, so it is a choice
  rather than a wait — but it turns a silent `None` into a terminal 422 for every caller whose model
  returns prose, and it *must* turn F42's emitter guard red (that red is the interlock working).
  Landing it inside enabling work would have meant a reviewer approving a blast-radius decision they
  did not come for. What it must do is written at `structured_output_from_text`.


> **CORRECTION (cycle 17).** This paragraph originally described F54 as *"the extraction failure
> class is lost from `memory_extraction_runs`"*. **That was wrong, and it contradicted F54's own
> entry in the same commit.** `memory_extraction_runs.failure_class` has existed since `0007` and has
> carried the execution's class since F29's third precondition. The real gap was *correlation* — the
> run could not be tied to its execution except through an unenforced `request_id` string convention.
> Closed by `0025`'s `execution_id` column.
>
> **A one-line summary of a finding can contradict the finding, in the same commit, and the summary
> is what the next brief inherits.** The implementing agent caught it by reading the entry rather than
> the summary — the same failure mode as F53, whose evidence its own commit had destroyed.

#### What remains, and none of it is autonomous work

~~**F54** (a failed extraction cannot be correlated to its execution except through an unenforced
`request_id` string convention) and one **bounded,
recorded gap** — swapping `stricter_of`'s arguments survives its guard, and can only change which of
two equally-permissive labels is *reported*, never a consent outcome.~~

**Both closed on `fix/f54-extraction-correlation` (2026-08-03) — and the first clause of that
sentence was wrong when written.** The failure class is **not** lost from `memory_extraction_runs`;
that column has existed since `0007` and this cycle's own F54 entry says so. What F54 is actually
about is the missing **`execution_id`**, which migration `0025` adds. The `stricter_of` gap turned
out to be **closable** — `ApplicationManaged` and `AutomaticWithUserControls` are two distinct
values that tie — so it is closed rather than still recorded. See cycle 17.

**Needing a human:** PR **#57** (F32's 422 breaks IaC setting `encrypted_content`); **F33**'s
encryption scoping; the **rig-core issue** drafted in `docs/upstream/`; the **T11 deploy**; **F50**'s
fail-closed call; and the **fail-hard flip**.


### Cycle 15 — 2026-08-02/03 — the findings queue emptied: eight PRs, three refutations

**Every finding on the inherited queue is closed.** `main` moved `eb9b988 → 20efdfa`.

| PR | Findings | Merge |
|---|---|---|
| #59 | F39 — structured-output capability | `655494a` — **the stopped peer's**, adopted |
| #58 | F46 — refuse `json_object` | `c938d5c` |
| #60 | F28, F10 item 1 | `71b7dba` — **my stopped agent's**, recovered and re-gated |
| #61 | F38, F48 guard, F49 raised | `779104d` |
| #62 | F49, F50 raised | `324d1b4` |
| #63 | F47 confirmed, **F40 refuted**, F51/F52 raised | `d295f9e` |
| #64 | F51, F52, F53 raised | `e16cb0c` |
| #65 | F42, F43 **refuted**, F44, F45 | `20efdfa` |

**Three findings were refuted rather than fixed, and each refutation was worth more than a fix
would have been:**

- **F40** — no configuration reaches the empty array at all; `output_persisted` is never `true`
  anywhere, so `Completed` *always* took the `OutputUnavailable` branch. Going to check turned up two
  real defects next door: a hardcoded reason literal that was false for three of four modes, and a
  fall-through to `[]` where the row claimed *more* persistence.
- **F43** — "every caller is inside `#[cfg(test)]`" is true only if `tests/` counts. **9 of 29 callers
  are in a separate crate**, so private/deleted were never available. **`pub` in this
  `publish = false` single-crate workspace means "visible to integration tests", not "external
  contract"** — that reframes every dead-`pub` finding.
- **F41, F36** were refuted in the prior cycle; the pattern is now established enough to expect it.

**Two findings were understated by their own entries:**

- **F47** — the family is **five**, not two, and the cost was not dead tuples. Each "read" fired
  `pg_notify`, so a conversation-linked turn wiped every replica's caches **three times**. Measured:
  3 notifications from 3 `do update` calls, 0 from 3 `do nothing`.
- **F51** — its standing defence was that caches "rebuild on the next read." True of two of three;
  **false of `ProviderRuntimeCache`, which holds built Rig clients with their connection pools** —
  and it is keyed by a tuple already containing every version number, so even the config-write case
  that defence was written for never needed the wipe.

#### The rules earned this cycle

1. **A derived inventory is only a guard if something else consumes it.** (F52) Deriving a list from
   `pg_trigger` stops it drifting from the schema; it does not stop someone editing the list to make
   the test pass. Closed only because a second guard consumes the same constant.
2. **A barrier must be inert with respect to the property it brackets.** `drain_listener` emits a
   `provider_models` payload — configuration — so it cleared the very caches under observation.
3. **Observe the most expensive thing a fix protects, not the easiest to construct.**
4. **Two CI runs can exist on one commit.** A `workflow_dispatch` alongside the automatic
   `pull_request` event makes `check-runs` report a job as *both* `completed/success` and
   `in_progress`. Form 12 warns about the previous *commit*; this is its sibling on the *same* commit.
   **Select the run by `event == "pull_request"`, and do not fire a redundant dispatch.**
5. **`pgrep` for a live build is too coarse to gate a reclaim on.** The build that blocked one was the
   Moira **server** (`cargo run` in the main checkout on `./target`), unrelated to every
   `~/.cargo-targets/*`. Resolve with `lsof -a -p <pid> -d cwd -Fn` and the process's
   `CARGO_TARGET_DIR` before acting.

#### On recovering stopped work

Two sessions stopped mid-flight this cycle and both left work worth finishing. The recovered branch
carried five commits *including its own ledger closures*, which reads like completion — **but no gates
log existed anywhere.** Gates were re-run from scratch before it was PR'd. *"It committed, so gates
must have passed"* is exactly the inference §2.2 exists to prevent.

#### What remains, and it is short

~~**F50** and **F53** are open and both deliberately unfixed~~ — **both were taken on
`fix/f53-f50-silent-degradation` (2026-08-03).** F53 is **CLOSED**: the question it was gated on was
answered first and neither RAG table's configuration is read through any cache, so both lose the
trigger. F50's **silence is fixed** — `warn!`, runtime event, audit row — while the fail-closed vs
fail-open **product decision is deliberately still open**, because observability is the part both
answers share rather than half of one of them. ~~The two remaining strict-structured-output
preconditions are unshipped by design.~~ — **both landed on `fix/f30-consent-columns`
(2026-08-03); the flip itself is still deliberately unshipped, and its reversal condition now
holds.** **F30** closed on the same branch, its premise partly refuted. **PR #57 (F32) is still
held for human sight.**

**The things still needing a human on the findings queue are therefore F50's product decision** and
**the structured-output fail-hard flip** — now unblocked rather than blocked, which makes it a
choice about blast radius rather than a wait — plus the pre-existing F33 (envelope encryption
scoping), the newly raised F54 (extraction runs carry no `execution_id`), and F32's held PR.


### Cycle 15 — 2026-08-02 — `fix/f40-f47-response-output-and-policy-reads`: one refutation, one understatement, two new findings

Two independent findings closed on one branch, separate commits, one gate run.

| finding | verdict |
|---|---|
| **F40** | **premise REFUTED** — the state it described is unreachable. Two adjacent defects in the same function were real and are fixed |
| **F47** | **confirmed and understated by a factor of two** — it named two consequences and there were four, including a cluster-wide cache wipe |
| **F51** | raised — the invalidation channel is attached to per-request data tables and `apply_invalidation` ignores its own scope for three of four targets |
| **F52** | raised — three triggered tables are unclassified, and the shipped guard that exists to catch that retypes its inventory instead of deriving it |

## F30 CLOSED (partly refuted) · F29's last two preconditions LANDED — `fix/f30-consent-columns`, 2026-08-03

Two independent pieces, separate commits (`673f9c0`, `4824838`, `ff7f5e2`), one gate run.

| finding | verdict |
|---|---|
| **F30** | **premise partly REFUTED, and confirmed at a site it did not name.** "No code path reconciles them" was true when written and false by the time it was read: `effective_extraction_status` has taken the stricter of the two since Sub-Phase F, tested with the columns disagreeing in both directions. **But the reader F30 predicted had already arrived** — `ConversationRecord.memory_behavior` was one of the two columns, computed in SQL |
| **F29 preconditions** | both remaining ones **landed**. The fail-hard flip is deliberately **not** taken; the reversal condition now holds and the flip is a separate, reviewable change |
| **F54** | raised — a failed extraction cannot be correlated to its execution except through an unenforced `request_id` string convention. **CLOSED on `fix/f54-extraction-correlation` (2026-08-03)** by migration `0025`'s `memory_extraction_runs.execution_id`; the "failure class is lost" half of how it was later summarised is **refuted** — that column has existed since `0007` |

### F30 — the finding was right about the shape and wrong about the site

The entry said *"no constraint or code path reconciles them"*. **One does**, and has since plan 11
Sub-Phase F: `effective_extraction_status` takes the stricter of the two, and
`explicit_only_on_the_conversation_policy_alone_still_withholds_the_memory` and
`disabled_consent_calls_no_extractor_and_writes_no_run_row` already drove the columns *apart* in
both directions. As an extraction defect F30 is **refuted**.

**Its own predicted failure mode had happened anyway.** `conversation_select` emitted
`coalesce(mp.consent_mode, 'explicit_only') as memory_behavior`, and that value is returned to
every caller of `GET /api/v1/conversations` and `GET /api/v1/conversations/{id}`. An application
with `memory_consent_mode = 'disabled'` on the conversation policy and
`consent_mode = 'application_managed'` on the memory policy was **told `application_managed`
while extraction was refusing**. Exactly the shape the entry named — two columns that agree in
every default deployment and disagree only where an operator deliberately tightened one — and
every test in the tree set them to the same value, which is why it shipped.

**The reason it happened in SQL is the whole lesson, and it is a sharpening of "reconcile in one
place".** The combining rule *was* in one place: an application-layer function over
`Option<MemoryStatus>`. A query cannot call an application-layer function, and the value
`memory_behavior` needed was a *mode*, not a status — so the second reader could not reuse the
rule even if its author had wanted to, and wrote its own in six words. **"One place in code" is
only a barrier if it is reachable from every layer that needs the answer, in the type that layer
needs.** The rule now lives on `MemoryConsentMode` as `stricter_of`, which is why
`src/infra/pg_rows.rs` can apply it and `conversation_select` can go back to selecting two raw
columns and deciding nothing.

Three barriers, in decreasing strength:

1. **There is no consent decision in SQL any more.** The query hands both columns up.
2. **`status_for_consent_mode` is private and no longer re-exported.** It turned *one* column into
   a decision, which made it the autocomplete answer for anyone holding one. The only exported
   entry point takes both.
3. **Two guards on the data layer**, because 1 and 2 constrain the code that exists and F30 is
   about the code that does not yet.

`create_memory` still reads the memory column alone. That is deliberate — a manual memory is
`user_application`-scoped and carries no conversation id, so the conversation policy is not
describing it — and it is now labelled as a decision in code and in `docs/memory-consent.md`
rather than sitting there looking like the defect.

**The mutation that mattered**, and it is the one the brief named: a guard that sets both columns
to the same value cannot see a reader consulting one of them.

| mutation | observed |
|---|---|
| `effective_memory_behavior` reports the memory column alone (the shipped defect, restored) | `the_reported_memory_behavior_is_the_stricter_of_the_two_consent_columns` **red** — *"conversation=Disabled memory=ApplicationManaged: the value reported to callers must be the one enforced; left: "application_managed", right: "disabled""*. Every other consent test in the file stayed green, which is precisely how this shipped |
| a new single-column read added to `conversation_select` (a fourth reader appearing) | both data-layer guards **red**; the count guard named the file and the direction — `("src/infra/repositories/conversation.rs", 8, 7)` against an expected `(7, 7)` |
| the conversation-policy read **removed** from `conversation_select` | both **red**; the count guard went `(6, 6)` against `(7, 7)`. This is the direction a membership guard cannot see — §3.4's thirteenth shape — and is why the table is compared as a whole rather than asserted upward |
| `stricter_of` ignores its conversation argument (the rule neutered, SQL untouched) | four unit guards **red**, including `the_combined_consent_decision_is_symmetric`, which was already in the tree and is the one no single-column implementation can pass. The two data-layer guards stayed **green**, correctly — they guard the shape of the query, not the rule |

**Cheapest edit that breaks the property while leaving the guards green** — one found, bounded and
recorded rather than fixed: **swapping `stricter_of`'s arguments** at the `memory_behavior` call
site. The function is symmetric except on the tie between the two equally-permissive modes, and the
integration case that exercises a tie uses two *agreeing* values, so nothing goes red. The blast
radius is which of `application_managed` / `automatic_with_user_controls` is reported when the two
columns hold one each — a label difference between two modes that permit the same thing, never a
consent difference. ~~Closing it would need a tie case with the columns disagreeing, which is worth
adding the next time this file is opened.~~

**CLOSED on `fix/f54-extraction-correlation` (2026-08-03).** The tie case with the columns
disagreeing exists, because `ApplicationManaged` and `AutomaticWithUserControls` are two *distinct*
values that both rank `2`.
`the_reported_memory_behavior_resolves_the_consent_tie_toward_the_memory_policy` drives both
argument orders of that pair; the swap reds it and **only** it, with 23 other cases green. It is
deliberately **not** a superset of `…_is_the_stricter_of_the_two_consent_columns` — on a tie the
memory column *is* the answer, so the new case is blind to the single-column read F30 was about,
and the sibling is blind to the swap. Verified in both directions by running both mutations.

### F29's preconditions — the disposition question had a different answer than expected

Precondition 1 asked for a retry/fallback disposition for `StructuredOutputInvalid`. The answer is
**stay out of all three sets** — the same *behaviour* it had by omission, now a recorded decision —
but the reasoning is not the same in the three directions, and one of them nearly went the other
way.

- **Retry: no.** The two live emitters reject the *caller's schema* before the model is called, and
  an unreadable schema is unreadable on the second attempt. For the reply case the flip would add,
  Moira pins `temperature: Some(0.0)` on its own schema-carrying calls, so a resample is
  bit-identical; and a retry budget spent on a chatty model is an attempt not available to the
  transport failure retries exist for.
- **Fallback: nearly yes.** The real argument was DeepSeek — a provider that *structurally cannot*
  send a schema will never comply, and the next one might. **F39 answered that at routing time**, so
  a model that cannot carry a schema is no longer selected for a schema-carrying request. What is
  left is a caller schema that fails everywhere, or a model declining, which is a quality question
  the fallback chain is not scoped for.
- **The decisive constraint, and it is worth stating on its own: a class carries exactly one
  disposition, and this class has two emitters.** `is_fallback_eligible` cannot tell "the model
  replied badly" from "the caller sent a 2 MB schema". Admitting it would let one malformed schema
  walk the entire fallback chain on every request — caller-triggered amplification against every
  provider the route lists.
- **Circuit: no, and least arguable.** Breaker entries are per `(provider, model)` and refuse
  traffic for *every* caller. A request-shaped failure that can open one is a denial of service
  wearing a health check's clothes.

**The claim in precondition 3 was overstated, and checking it changed the fix.** The doc comment
said reclassifying would lose *"the only signal"* distinguishing "the model did not comply" from
"the call did not happen". It is not the only one: `audit_execution` writes an `audit_logs` row with
`metadata.failure_class`, and `complete_failed_attempt` writes `execution_attempts.failure_class`.
What is true is narrower and still bad — the signal is lost from `memory_extraction_runs`, the
operator-facing record for extraction, and recovering it needs an undocumented string-format join
(**F54**).

**The fix is deliberately general rather than one arm for `StructuredOutputInvalid`.** A special
case for that class would be **unreachable by any test in this tree**: extraction builds its own
always-readable schema and never crosses `validate_response_format`, so neither live emitter is on
that path. An arm no test can execute is a promise, not a guard. `run_extraction` now records the
execution's own failure class, which *is* reachable — and the proof is that
`a_failed_extraction_call_leaves_the_response_untouched` went red on a real provider 500 and now
asserts `provider_unavailable` where it asserted `extraction_call_failed`.

**The consequence worth keeping:** a non-conforming reply is recorded as `structured_output_invalid`
**before and after the flip** — today by `parse_candidates` refusing prose, afterwards by the
execution. The signal precondition 3 was protecting survives the flip instead of being traded for it.

**Mutations.**

| mutation | observed |
|---|---|
| `StructuredOutputInvalid` added to `is_fallback_eligible` | both disposition guards **red**; the table one printed *"StructuredOutputInvalid: (retryable, fallback_eligible, circuit_failure) changed … left: (false, true, false), right: (false, false, false)"* |
| `ProviderTimeout` **removed** from `is_retryable` | the single-variant guard **green**, the table guard **red** — §3.4's thirteenth shape, demonstrated on the guard written to survive it |
| the `run_extraction` call site reverted to the constant, leaving the helper and its unit tests intact | unit guards **green**, integration guard **red**. This is F49's lesson in one line, and it is *why* the general form was chosen: had the fix special-cased `StructuredOutputInvalid`, this mutation would have had no red at all |

**Cheapest edit that breaks the property while leaving the guards green:** for the disposition,
adding a **third emitter** of `StructuredOutputInvalid` on a model-output path without revisiting
the decision. Not a new gap — F42's
`structured_output_invalid_has_only_the_two_emitters_its_catalog_entry_describes` is the interlock,
and the disposition guard's body now points at it so a red there is read as "re-open this decision"
rather than "fix the count".

### Why the flip is still not shipped

The reversal condition holds in full: F39 landed, the disposition is recorded and guarded, and
`run_extraction` reads `execution.status`. **It is still not taken here, and that is the point.**
The flip turns a silent `None` into a terminal 422 for every caller whose model returns prose, on a
class that by design neither retries nor falls back — a blast-radius decision that deserves its own
diff and its own review rather than arriving inside the work that unblocked it.

What it must still do is written down at `structured_output_from_text`: widen the catalog
description, expect F42's emitter guard to go **red** (that red is the interlock working), re-read
the disposition with three emitters in view, and replace the two `tests/structured_output.rs` cases
that pin the current behaviour on both run paths.

## F58 CLOSED — `fix/openapi-summarization-claim-94`, 2026-08-05 (issue #94)

### F58 — the spec told seven operations that summarization does not exist, one hour after it shipped

`POST /api/v1/conversations/{id}/summarize` landed in `dac7468` (plan 11 Sub-Phase E) on
2026-08-02 at 05:21. `270df5e` — **F31, the fix whose entire subject was "stop the spec telling
callers that retrieval is unwired"** — landed at 06:19 the same morning and wrote

> Conversation summarization is not implemented yet.

into the shared operation description carried by seven operations: `POST /api/v1/conversations`,
`POST /api/v1/conversations/{id}/messages`, `POST /api/v1/memories`, and the four
`/api/v1/admin/applications/{application_id}/*-policy` PUTs. F31 replaced one false claim with a
narrower one. The four policy PUTs are again the sharp end: `summarization_enabled`,
`summary_trigger_tokens`, `minimum_messages_since_summary`, `summary_target_tokens` and
`history_strategy` are all written through the conversation-policy PUT whose own description said
the feature they configure does not exist.

**Checked for the nuance that would have made it true, and there is none.** The endpoint exists and
is registered on the caller plane; `summarize_conversation` writes an immutable version behind a
per-conversation advisory lock; `maybe_summarize_after_turn` runs the automatic path after the
assistant message is persisted; `assemble_context` injects the active summary for every
`history_strategy` except `recent_messages`, and the column's default is `summary_plus_recent`.
`tests/conversation_summarization.rs` (1 605 lines) covers both entry points end to end. The only
true part of the neighbourhood is that `summarization_enabled` defaults to `false` — a default, not
an absence, and the corrected sentence says so.

**Fixed** by replacing the sentence in all seven `#[utoipa::path]` descriptions with what the
feature actually does, including the gate and the injection condition, and by regenerating
`docs/openapi.json`. **No operation was added or removed and the operation count did not move**;
the snapshot diff is seven description strings.

**The regression guard is the test that used to hold the falsehood in place.**
`conversation_memory_rag_operations_document_where_stored_content_is_used` already pins the
sentence verbatim on all eleven operations and already carries `INERT_PRIMITIVE_CLAIMS`, the
forbidden-phrase family from the same defect one sub-phase earlier. `"summarization is not
implemented"` is now the seventh member, so the sentence cannot return by any wording that contains
it.

**Siblings checked and deliberately left alone** (every hedging description in the generated
document was read against the code):

| Claim | Verdict |
|---|---|
| `ClaimAdminIdentityRequest.setup_token` — "deferred … refused with a clear, keyed error" | **still true** — `setup_token_not_supported` is raised in both `src/application/identity.rs` and `src/http/identity.rs` |
| `DELETE /api/v1/admin/admin-identities/{id}` — "plan 07's explicitly deferred revoke endpoint" | **still true and honest** — it describes shipped soft-revoke behaviour |
| The four RAG-write `Idempotency-Key` descriptions | **already fixed by 02b** — real replay, and two tests forbid "not implemented" there |
| `POST /v1/responses` — "`json_object` is refused", "`verbosity` is refused rather than ignored" | **still true** (F35) |
| `SENTENCE_A_RAG_WRITE` on the four RAG writes | **still true** — chunking, embedding, indexing and citations all run |
| "retrieval stays off until those policies enable it" | **still true** — the three retrieval flags default `false`; kept verbatim |

The prose docs the descriptions point at carried the same lie and were corrected with them:
`docs/conversation-memory-rag-api.md` had a section titled *"The one thing that still does not
run"* asserting `conversation_summaries` has no writer. `docs/public-api.md` was already correct,
which is the clearest evidence that this was a stale sentence rather than a disagreement about
behaviour.


### F54 — a failed extraction cannot be correlated to its execution

Found while discharging F29's third precondition. `memory_extraction_runs` has **no
`execution_id` column**. The run row now records the execution's failure class, which is the
question an operator asks first — but the follow-up ("show me that execution: which provider,
which model, which attempts, what the sanitised provider message was") has no join key.

The only correlation that exists is a **string convention**: `run_extraction` sets
`request_id = format!("memory-extraction-{run_id}")`, and `audit_execution` writes that into
`audit_logs.request_id`. Nothing enforces the format, no test asserts it, and no doc names it, so
an operator can only find the execution by knowing to `like 'memory-extraction-%'` and parsing a
uuid out of a string. `summarization` has the same shape.

**Not fixed here**: it is a migration plus a writer plus an admin-surface question about whether
the id is exposed, in a change whose subject was consent columns and failure labels. **Remedy in
one change:** add `execution_id uuid` to `memory_extraction_runs` (and the summarization run
equivalent), write it from the `ExecutionCommand` that is already in scope, and pin the
correlation with a test that reads the execution back through it — at which point the `request_id`
convention becomes a convenience rather than the only route.

**CLOSED on `fix/f54-extraction-correlation` (2026-08-03), migration `0025`.** Three corrections
to the remedy as written above, all found while taking it:

1. **The admin-surface question does not arise.** `memory_extraction_runs` is exposed on no route
   and appears nowhere in `docs/openapi.json`; it is a SQL-only operator record. So there is no
   DTO and no contract change — but it does mean the column has to be *queryable*, which rules
   out the genuinely cheapest option of stuffing the id into the existing `metadata` jsonb.
2. **Write it from the run-row insert, not "from the `ExecutionCommand` that is already in
   scope".** The command is built *after* the row is opened, so taking the id from it means
   recording at completion — and the row that never completes is the one
   `insert_memory_extraction_run` exists to leave behind. The id is now minted in
   `extract_memories` and handed *to* the command instead.
3. **There is no summarization run equivalent** — see F55.

### F56 — a reasoning model's chain-of-thought is stored as the conversation summary

**Measured against a real provider, not inferred.** The user's vLLM moved to
`https://local-llm.motrait.com` (`Qwen/Qwen3-4B`, OpenAI-compatible, no key) on 2026-08-04, which
made this testable for the first time. Every number below came off that wire.

Moira's **actual** summarization call — `SUMMARIZATION_INSTRUCTION`, the real transcript shape, **no
`output_schema`**, temperature 0 — returned:

```
<think>
Okay, let's see. The user wants to ship the invoicing rewrite before the March board meeting…
</think>

The user aims to complete the invoicing rewrite ahead of the March board meeting but faces…
```

**808 of 1282 bytes — 63% — is chain-of-thought**, and `parse_summary` stores all of it.
`parse_summary` trims, strips a **code fence**, then checks empty and size. There were **zero** code
fences. Nothing in `src/` handles a reasoning block; `grep -rn "think" src/` returns nothing.

**The prompt already tries to prevent this and fails.** `SUMMARIZATION_INSTRUCTION` says *"Return
only the summary text — no preamble, no headings, no JSON, no code fences."* A `<think>` block is a
preamble. The instruction is not a control.

**Why this compounds rather than merely wastes space.** The stored text is fed back as
`PRIOR_SUMMARY_LABEL` on the next run, so the model reasons about its own previous reasoning. It
also counts against `MAXIMUM_SUMMARY_BYTES` and the target-token budget, so the *actual* summary is
squeezed by the reasoning that precedes it.

**Moira has already decided that reasoning is not output — it just cannot enforce it here.**
`text_from_choice` (`src/orchestration/runtime_factory.rs`) drops `AssistantContent::Reasoning` in
its `_ => None` arm, and the streaming path filters `StreamedAssistantContent::Reasoning` and
`ReasoningDelta` by name. That intent is implemented for the case where the **server** separates
reasoning. vLLM without `--reasoning-parser` does not: the block arrives as ordinary `Text`, and
Moira cannot tell it apart.

**Not reachable through extraction**, which sends `extraction_output_schema()` — guided decoding
suppressed the block entirely in test 2 below. **Summarization is the exposed path** precisely
because F29's parse is gated on a schema summarization never sends.

**Severity is deployment-shaped:** invisible on OpenAI/Anthropic, unavoidable on a self-hosted
reasoning model — which is exactly what `OpenAiCompatible`/`Local` exists to serve, and what F39
deliberately left admitted as undecidable.

**Open question, deliberately not decided here:** strip, or refuse. Stripping is a heuristic on
prose, and `memory_extraction.rs` documents a deliberate refusal to hunt JSON inside prose — the
same shape. Refusing costs reasoning-model deployments their summaries entirely. **The one thing
that is not defensible is the current behaviour: storing it silently.**

#### The same session settled two other questions by measurement

- **F39's undecidable half is decidable for this backend: vLLM complies.** `json_schema` +
  `strict: true` returned exactly `{"name":"Dr. Elara Voss","age":42}` — clean, and with the
  `<think>` block suppressed by guided decoding. Leaving `Local` admitted was right.
- **F46 is confirmed on a real provider, not just by reading Rig's source.** Sending the exact shape
  Moira's `json_object` compiled to — `{"type":"object"}` sanitised to `properties: {}`,
  `additionalProperties: false`, `required: []`, `strict: true` — the model returned literally
  `{}`. The refusal shipped in `a8937f4` is now backed by a measurement, not an inference.
- Tool calling works on this endpoint (`finish_reason: tool_calls`), so **F48 becomes live-testable**
  the day `build_completion_request` stops hardcoding `tools: Vec::new()`.

## F56 REPRODUCED · CLOSED as F57 — `fix/f57-reasoning-in-summaries`, 2026-08-04

**F56 reproduced against the live endpoint before anything was built on it**, on a second machine
two days later, with a different transcript. Same shape, different magnitude: **1 298 of 2 419
bytes — 53.7 %** of the stored summary was chain-of-thought, against F56's 63 %. The share is
transcript-dependent and should not be quoted as a constant; what reproduces is that *the majority
of the stored summary is not the summary*.

Three things the wire showed that reading the code could not:

1. **The `reasoning` field is present on the response and is `null`.** vLLM without
   `--reasoning-parser` emits `message.reasoning: null` while `content` carries the block. So
   Moira's existing intent — `text_from_choice` dropping `AssistantContent::Reasoning`, the
   streaming path filtering `ReasoningDelta` — is not merely unenforced, it is *addressed to a
   field the server left empty*. That makes the operator-side fix exact and nameable rather than
   speculative, and it is why the warning names `--reasoning-parser`.
2. **The endpoint sits behind a WAF that 403s on `User-Agent: Python-urllib/*`** while accepting
   an empty or absent UA. Nothing to do with Moira — but it cost the first measurement attempt, and
   a future agent probing this endpoint should know that a 403 there is not an auth failure.
3. `chat_template_kwargs: {"enable_thinking": false}` **works** — 830 bytes, zero tags. It is a
   real fix living on the request, and it is rejected below for a stated reason rather than
   overlooked.

### The decision: announce it, store it unchanged. Removal was rejected on a measurement

`parse_summary` now returns `ValidatedSummary { text, inline_reasoning }`, where
`inline_reasoning` is `text.starts_with("<think>")` — anchored at offset 0, no search, no
terminator, **and `text` is byte-identical either way**. The condition is announced three ways with
three consumers, the split `announce_dangling_agent_profile` established for F50: a `warn!` naming
`--reasoning-parser`, the new label-free counter
`moira_summarization_inline_reasoning_total`, and an `inline_reasoning` boolean on the existing
`conversation.summary.created` audit row.

**Stripping the block (option a) was not rejected on principle. It was rejected because the
terminator is not identifiable, and the live model demonstrated that on the first attempt.** A
transcript that merely *discusses* reasoning tags — a support conversation about this very defect,
which is the population most likely to be running a reasoning model — returned one `<think>` and
**ten** `</think>`: the model quoted the tag while reasoning, and again in the summary because the
user had asked for the markers verbatim. The real terminator was the **fifth of ten**.

| removal rule | result on that reply |
|---|---|
| cut at the first `</think>` | **985 bytes of chain-of-thought left in the stored summary** |
| cut at the last `</think>` | **2 173 bytes of legitimate summary destroyed**, 220 left |
| cut only a *well-formed* leading block | correct here, and **inert** on the truncation case below |

And the worst case has no terminator at all. At `max_tokens: 120` the reply came back
`finish_reason: length`, 613 bytes, one `<think>` and **zero** `</think>` — 100 % reasoning, 0 %
summary. A rule that strips only a well-formed block does nothing precisely where the damage is
total, and truncation is reachable: `AgentProfileRecord::max_tokens` reaches this path (F49).

Detection had the opposite result over the same five replies — anchored at offset 0 it was correct
on **all five**, including the truncated one and the `enable_thinking: false` control. **The
condition is decidable; its extent is not.** That asymmetry is the entire decision, and it is why
the fix stores a `bool` rather than a shorter string.

### Every option rejected, with why

- **(b) refuse with a new `FAILURE_SUMMARY_*` class** — rejected **by reasoning already in the
  tree**, which is why it is worth stating rather than re-deriving. `parse_summary`'s own doc
  comment explains that a refused summary writes no row, so `covers_through_sequence` does not
  advance, so the backlog that triggered the run re-triggers it **every turn, forever**. That
  argument was written about a conversation containing the word `bearer `; here it is strictly
  worse, because "the model emits inline reasoning" is a **permanent property of the deployment**,
  not an incident. Refusing would convert a fat summary into an unbounded per-turn provider bill on
  exactly the deployments the feature is meant to serve. **Its reversal condition is already
  written at that function and is F55** — a `conversation_summarization_runs` table that can record
  a refusal and back it off. F55 has **not** landed; the brief for this work assumed it had.
- **(d) per-provider or per-model configuration** — rejected because it buys nothing. The action a
  declaration would authorise is still *removal*, and removal is undecidable regardless of how
  confident the operator is that their model reasons. Configuration that cannot change what the
  code does is sprawl.
- **(e) send `chat_template_kwargs: {"enable_thinking": false}`** — measured to work, and still
  rejected. It is a vLLM transport extension carrying a *Qwen chat-template* kwarg; OpenAI rejects
  unknown body fields, and `OpenAiCompatible`/`Local` is by construction the arm where Moira cannot
  know what is behind it — the same undecidability F39 recorded and deliberately left admitted.
  Moira would be guessing the backend in order to avoid guessing the prose. It is the **operator's**
  knob, so it is named in the metric description instead.
- **A well-argued no-change** was available and was not taken, because the measurement moved the
  question. F56 recorded the one thing not defensible as *storing it silently*; the silence is
  removable without any heuristic, and that is all that shipped.

### What is deliberately unchanged

The stored summary is still 53 % scratchpad, and it still counts against `MAXIMUM_SUMMARY_BYTES`,
`budget_tokens` and the target-token budget. **Compounding was measured and the brief's account of
it needs one correction:** feeding run 1's contaminated summary back as `PRIOR_SUMMARY_LABEL`
raised the prompt from 347 to 894 tokens and produced a 3 617-byte reply — but run 2's *output* did
not quote run 1's reasoning. It read it as material and rewrote it. So the compounding is in
**bytes and tokens**, not in semantic contamination of the summary text. That is still a real cost
and still not fixed here; it is fixed by `--reasoning-parser`, which is now discoverable.

### Reversal condition

**Remove the block instead of reporting it only if a *non-positional* separation becomes
available** — the provider separating it into its own field, or a declared response contract that
makes the boundary explicit. **A better search over the prose is not that**, and any proposed
search must first be run against the ten-terminator reply, which is committed as
`REASONING_SUMMARY_BODY` in `tests/conversation_summarization.rs` for exactly that purpose.

Separately, **the refusal option reopens the day `conversation_summarization_runs` exists** (F55).
At that point a refused summary can be recorded and backed off, the retry loop stops being the cost
of refusing, and "refuse a reply that is mostly scratchpad" becomes a live choice rather than a
foreclosed one.

### Guards, and the mutations run against them

Six cases: four pure (`src/application/summarization.rs`), two DB-backed
(`tests/conversation_summarization.rs`), plus a metrics unit test and an **opt-in, skipped-by-default**
live suite (`tests/live_reasoning_model.rs`, gated on `MOIRA_LIVE_REASONING_BASE_URL`; CI has no
route to the endpoint and a gate that depends on someone's LAN is a gate that lies).

**Watched failing first.** With `begins_with_inline_reasoning` returning a constant `false` — which
*is* the shipped defect, expressed compilably — the suite reported
`FAILED. 25 passed; 3 failed` and `FAILED. 18 passed; 2 failed`, the three positive unit cases and
the integration case reding on `inline_reasoning: false` / `left: 0.0, right: 1.0`. **Both negative
cases stayed green**, which is what makes it the defect rather than an inverted build. Restoring the
one-line body gave `28 passed; 0 failed`, `25 passed; 0 failed` (metrics) and `20 passed; 0 failed`.

Four mutations run, and **the interesting result is that no single case catches more than two of
them** — which is the point of splitting detection from storage:

| mutation | measured result |
|---|---|
| M1 `begins_with_inline_reasoning` → `false` (**the shipped defect**) | 3 unit + 1 integration red; both controls green |
| M2 `starts_with` → `contains` | `27 passed; 1 failed` — **only** `a_summary_that_merely_discusses_reasoning_tags_is_not_flagged` |
| M3 cut the text at the terminator (**option (a), implemented**) | `26 passed; 2 failed` — only the byte-identity assertions |
| M4 delete the `record_summarization_inline_reasoning()` call | `28 passed` in the lib, **`19 passed; 1 failed`** in the integration suite |

**M3's green case is the argument, sitting in the suite.** Implementing option (a) reds the two
cases whose replies *have* a terminator and leaves `an_unterminated_reasoning_block_is_flagged_too`
**passing** — because a well-formed-block strip does nothing to a block that never closes. The
measurement that ruled stripping out is therefore not only recorded in prose; the test suite
demonstrates the blind spot on every run.

**M4 is HANDOFF §3.4's thirteenth-entry shape in miniature:** all 28 pure tests stay green because
the pure layer cannot see whether anything is wired to it. Only the DB-backed case names the
counter, and it is the only thing standing between this fix and the "seeded but never emitted"
failure this repository has shipped five times.

The `contains` mutation is the one worth naming. It is invisible in review, leaves every other case
green, and would fire on any conversation *about* reasoning models — so its false positives land on
precisely the population whose true positives matter. It is caught by a case whose input is the
live model's own reply, not by an invented one.

The `contains` mutation is the one worth naming. It is invisible in review, leaves every other case
green, and would fire on any conversation *about* reasoning models — so its false positives land on
precisely the population whose true positives matter. It is caught by a case whose input is the
live model's own reply, not by an invented one.

The control (`an_ordinary_summary_announces_nothing`) is not padding: without it,
`inline_reasoning: true` unconditionally is a passing implementation of the entire feature. That is
F45's lesson from HANDOFF §3.4, applied before it was needed rather than after.

**One assertion was written wrong and caught before it could lie.** The integration case first
walked the *whole* `audit_logs` table asserting the phrase `"invoicing rewrite in March"` was
absent — and `USER_TURN` in that same fixture is
`"we agreed to ship the invoicing rewrite in March"`. It would have failed for a reason having
nothing to do with F57, or worse, passed only because nothing happened to log the turn. It is now
scoped to the summary row and to markers unique to the reply, and the general property is left to
`no_summary_or_transcript_text_reaches_the_audit_log`, which already owns it — HANDOFF §3.4's
"a guard that duplicates another guard is not a second guard".

### Gate runner — NOT `scripts/gates.sh`, and why

This host could not execute **any** freshly compiled binary while this work was done: a
`chrome_crashpad_handler` in a FATAL crash-loop had `syspolicyd` pegged at ~99 % CPU, so every
code-signing assessment queued forever. A 16 KB hello-world hung; cargo sat on eight build scripts
with `0:00.00` CPU each and no `rustc` at all. Diagnosis and escape hatch are written up in
HANDOFF §2.2d, which is the durable half of this entry.

`cargo fmt --check` was run on the host (rustfmt compiles nothing, so it is unaffected). Everything
else ran inside `rust:1.97-trixie` against the same Postgres over `host.docker.internal`. **That is
not `scripts/gates.sh`**, so its log-completeness assertion and its skipped-DB-suite check did not
run, and no `ALL GATES PASSED` marker exists for this branch. Said plainly rather than left to be
inferred — the completeness check was instead done by hand: **all 47 files in `tests/` appear as a
`Running tests/…` line**, none dropped.

**All six gates ran and are green**, in three passes because the VM's 32 GB disk could not hold the
debug and release trees at once:

| gate | result |
|---|---|
| `fmt --check` | **PASS** (host *and* container) |
| `clippy --workspace --all-targets --all-features -- -D warnings` | **PASS** |
| `test --workspace --all-features --no-fail-fast` | **1111 passed, 1 failed** — 50 result lines over 47 suites; the one failure is the flake below |
| `build --release --locked` | **PASS** — `Finished \`release\` profile [optimized] in 15m 05s`, rc 0 |
| `deny check` | **PASS** — `advisories ok, bans ok, licenses ok, sources ok` |
| `audit` | **PASS** — `warning: 1 allowed warning found`, RUSTSEC-2026-0221, the expected one |

**A form-1 trap was walked into while running the supply-chain pair, and the content marker is what
caught it.** The script ended each gate with `cargo … | tail -20; echo "EXIT=$?"`, which reports
**`tail`'s** status — both printed `EXIT=0` while `cargo audit` had in fact printed
`error: no such command: audit`, having never installed. Exactly §2.2's first form, in a script
written by someone who had just read §2.2. The only reason it was not recorded as a pass is that
`deny`'s own `advisories ok, bans ok, licenses ok, sources ok` line is a *content* marker and
`audit` had no such line. It was then installed properly and run **unpiped**, so `AUDIT_RC=0` is its
own status. **The argument for content markers over exit codes, earned again.**

Two flakes were observed, both timing-sensitive, both unrelated to this change, and both verified by
re-running in isolation:

- `a_concurrent_summarization_is_answered_with_202_and_retry_after` — the one HANDOFF §2.2a already
  names. 502 instead of 200 on two runs; **3/3 green** when re-run alone.
- `context_planner_boundary`'s two `responses request timed out` failures under the parallel run;
  **6/6 green** when that suite is run alone. Timeouts, not assertions, in a suite this change does
  not touch.

Both are the container's bind-mount latency on a host whose spare core is being eaten by the
`syspolicyd` spin described above.

### F55 — a failed summarization leaves no operator-facing record at all

Raised while closing F54, whose remedy assumed a "summarization run equivalent" of
`memory_extraction_runs` and whose entry says *"`summarization` has the same shape"*. **It does
not, and its shape is worse.**

Extraction has a run table: opened before the call, carrying `status`, `failure_class`,
`started_at`/`completed_at`, counts, and now `execution_id`. Summarization has
`conversation_summaries`, which is a table of **successful outputs** — no `status`, no
`failure_class`, no `started_at`. `run_summarization` returns a `Result`; on the automatic path
(`maybe_summarize_after_turn`) the error is swallowed so it cannot become the caller's problem,
exactly as extraction's is. The difference is that extraction writes the reason to a row and
summarization writes **nothing**. The only trace a failed summarization leaves anywhere is the
`record_summarization_run(false)` metric counter — an aggregate with no conversation id, no
failure class, and no execution to chase.

So the operator question F54 was about — *"this conversation's summary is stale; why?"* — is not
merely hard to answer here, it is unanswerable from the database. And the F54 fix does not reach
it: there is no row to add a column to.

**Not fixed here** deliberately. It is a new table, not a column, and "should a failed
summarization be durable at all" is a design question with a real cost on the response path —
summarization already makes a second provider call per triggering turn, and adding an
insert-before-call plus an update-after doubles its database writes. That is a different
conversation from F54's, which was a one-column correlation fix on a table that already existed.

**Remedy if taken:** mirror extraction exactly — a `conversation_summarization_runs` table opened
in `'running'` before the completion call, carrying `conversation_id`, `execution_id`,
`failure_class` and the covered-sequence boundary, completed on both paths. The shape is already
proven one module over, which is most of the argument for doing it that way rather than inventing
a second one.

## F53 CLOSED · F50 OBSERVABLE — `fix/f53-f50-silent-degradation`, 2026-08-03

Two findings, separate commits (`31a23a2`, `da8a936`), one gate run.

| finding | verdict |
|---|---|
| **F53** | **confirmed in full.** The gating question — is a RAG collection's own configuration read through any cache the listener clears? — has the **same answer for both tables, and it is no.** Both lose the trigger, exactly as `conversations` and `memory_records` did |
| **F50** | **confirmed in full.** The silence is fixed — `warn!`, runtime event, audit row. **The fail-closed/fail-open decision is deliberately NOT taken and is still open** |

### F53 — the question decided the fix, and the answer was stronger than "no"

The entry's own hypothesis was that `rag_collections` might carry an embedding model and a
dimension, which a cached read could then serve stale. **It carries neither. It carries no
runtime configuration at all** — `collection_key`, `display_name`, `description`, `status`,
`visibility`, `metadata` and lifecycle columns, and nothing else. Every embedding value is in
`application_embedding_policies`, keyed by `application_id`; `find_collection_ingestion_context`
and `find_document_ingestion_context` join the collection **only** to obtain that id. So the
table that looked like the plausible configuration candidate turned out to be the clearer of
the two.

**The general answer is type-level rather than argumentative,** which is why it is worth
writing down once: `apply_invalidation` clears exactly three caches, and all three are closed
types. `RuntimeConfigCache` is `HashMap<Uuid, ProviderConfig>`. `AuthProviderSettingsCache` is
one `Vec<PublicAuthMethod>`. `ProviderRuntimeCache` is keyed by `RuntimeCacheKey`, whose seven
fields are provider, model, credential and runtime-policy ids and versions. **No RAG row can be
in any of them and no key field derives from either table**, so the question "is it read through
a cache?" cannot be answered yes for anything outside those three shapes. That argument is
reusable for the next table anyone asks about.

A second reason the answer is safe: the collection's `visibility` and `status` carry the
tenant-isolation predicate, and they are evaluated in the retrieval SQL on every query rather
than read from anywhere cached — so removing the notification cannot leave an authorization
decision stale, which was the only way "content" could have been the wrong classification.

**Corrections to the F53 entry, both worth recording because briefs here inherit each other's
errors:**

1. *"`docs/runtime-cache-invalidation.md` does not list either table as an invalidation-producing
   resource, exactly as it did not list the two F51 removed."* **True when written, false by the
   time it was committed.** The F51/F52 commit (`e16cb0c`) rewrote that paragraph to match
   `TRIGGERED_RESOURCE_TYPES` and in doing so *added* "and the RAG collection and document
   tables". The finding's supporting evidence was destroyed by the commit that raised it. Checked
   with `git show e16cb0c^:docs/…` rather than trusting either version.
2. *"its embedding model, its dimensions"* — columns that do not exist on that table.

**Five mutations, each reverted.**

| mutation | result |
|---|---|
| honour `plan.caches` for `runtime_cache` only; always clear `runtime_handles` and `auth_settings` | **red** on the handles assertion in both the F51 and F53 integration guards — *"dropped every built provider client handle, connection pools included"*. This is why the guard seeds a real `RuntimeModelHandle` rather than the trivially-seedable config sentinel |
| move `rag_documents` back to `CIRCUIT_UNAFFECTED_RESOURCE_TYPES` | **red** on the `rag_documents` leg only — **and all eight `infra::db` unit tests stayed green**, because they iterate the constant the name was removed from. The integration guard is the only thing that sees this |
| the same for `rag_collections` | **red** on the `rag_collections` leg only. The two legs are independent, which is the point of asserting per resource type |
| re-attach the `rag_documents` trigger (a scratch `0025`) | **red** on the trigger guard, showing all three of INSERT/UPDATE/DELETE, **and** red on `the_notify_trigger_inventory_is_exactly_the_classified_set` naming the table |
| then the **lazy repair**: add `rag_documents` back to `TRIGGERED_RESOURCE_TYPES` and stop | inventory test **green**, `every_triggered_table_has_a_scope` **red** on its `caches` half. F52's interlock holds for these tables without any new machinery |

**What the second mutation adds to §3.4, and it is a sharpening rather than a new rule.** F52's
lesson is *a derived inventory is only a guard if something else consumes it*. The corollary
this run produced: **a guard that iterates a constant cannot see a name being removed from that
constant.** `only_configuration_changes_invalidate_the_configuration_caches` is a real guard and
it went green under the edit that reintroduced half the defect, because the edit deleted the
name it would have iterated. Set-membership guards are one-directional by construction; the
behavioural guard that names the table literally is what closes the other direction, and both
are needed.

### F50 — the silence is fixed; the product decision is untouched and still open

**The coordinator's decision, implemented as given: observability only.** A `warn!`, a
`RuntimeEventType::AgentProfileUnavailable` runtime event and an `agent_profile.unavailable`
audit row. **The request's behaviour is byte-for-byte what it was** — `succeeded`, no failure,
no preamble, no temperature, no max_tokens.

**The rationale, recorded with its reversal condition.** Silence is a defect under *either*
product answer. Fail-closed and observable fail-open both require the operator to be told; they
differ only in whether the request is *also* refused. Shipping observability is therefore not a
partial implementation of one option — it is the part both options share. **Reversal condition:**
when the fail-closed/fail-open decision is taken, this changes from "observe and proceed" to
"observe and refuse" *or* stays as it is. **The observability itself is not revisited.**

**"No profile" and "the profile vanished" are cleanly distinguishable, and the distinction is
free.** `route.agent_profile_id` is read *before* the lookup: `None` is "this route never had
one" — the normal case, and what every other fixture in the tree produces — and only
`Some(id)` with an unresolved lookup announces. No inference from the lookup's own result is
needed. The one way the two could have collapsed is a **hard** delete, which the FK's
`on delete set null` would turn into `None`; `soft_delete_agent_profile` writes
`status = 'deleted', deleted_at = now()` and never issues a `DELETE`, so it cannot happen, and
`guards_a_soft_deleted_agent_profile_is_announced_too` asserts the reference survives before it
asserts anything else.

**The event is deliberately not on the public SSE contract.** `map_runtime_event` returns `None`
for it. Its payload names a route id, a route key and an agent profile id — admin-plane shape a
caller can do nothing with — and the operator's channels are the diagnostic endpoint (which
returns every envelope verbatim), the log and the audit row. `docs/openapi.json` gains exactly
one enum member and nothing else; 152 / 100 / 183 unchanged.

**`documents_current_behaviour_a_disabled_agent_profile_is_silently_ignored` is gone**, because
its name stopped being true: the condition *is* observed now, it is simply not refused. It is
replaced by four cases that keep the `documents_`/`guards_` line honest — three guards that
survive the product decision, and one `documents_` case that the decision makes wrong. **Merging
them would have made the observability guard go red on a fail-closed fix**, which is precisely
the pinned-defect shape §3.4 records twice.

**Six mutations, each reverted.**

| mutation | result |
|---|---|
| drop the audit row, keep the event and the `warn!` | **red** on the disable and soft-delete guards |
| drop the runtime event, keep the audit row and the `warn!` | **red** on all three announcement guards |
| announce whenever `agent_profile_id` is `Some`, rather than when the lookup fails | **red** on the *within-test* active-profile control only — **`guards_a_route_with_no_agent_profile_announces_nothing` stayed green**, because that route never enters the arm. This is the edit that justifies having both controls |
| empty payload (`json!({})`) | **red** on `agent_profile_id`; the ids are asserted, not just the event's presence |
| `AuditResult::Success` instead of `Failed` | **red** — the disable guard asserts the row's `result`, not only its action |
| map the event onto the public SSE contract | **red** on the new unit guard, which carries its own liveness control (`route_selected` must still map) |

**The one gap, chosen rather than missed: nothing asserts the `warn!`.** That is deliberate —
a log assertion is weak, and capturing `tracing` output requires a process-global subscriber that
would fight every other test in the binary. Deleting the `warn!` alone leaves every guard green.
The two signals that *are* guarded are the structured one and the durable one, which is the right
two of the three.

**A hole closed that nothing asked for, from §3.4's twelfth entry.** `tests/agent_profile_wire.rs`
justifies its streaming case with *"both arms share the one call today, but they are separate
arms"*. That justification applies to **every** property the suite tests, and F50's observability
had no streaming twin. `get_active_agent_profile` has exactly one call site in `src/`, so the
case cannot fail today for a reason the disable case would not also catch — it exists so the cell
is filled before something moves the lookup. Adding it is the whole cost of not repeating F49.

## F51 CLOSED · F52 CLOSED — `fix/f51-f52-invalidation-scope`, 2026-08-02

Two defects in one channel, separate commits (`f97d4f1`, `9f243a6`), one gate run.

| finding | verdict |
|---|---|
| **F51** | **confirmed in full — every count in the brief was right**, including the 24-vs-23 trigger count and which table's trigger is named differently. Both candidate fixes taken; they are independent barriers and neither is load-bearing |
| **F52** | **confirmed in full, and the fix shape the entry named was the right one.** The three legacy tables lose their triggers rather than being classified — nothing outside `0003` itself has ever named them |
| **F53** | raised — `rag_documents`/`rag_collections` are the same class as F51 at admin rate, deliberately left |

**What this pair adds to §3.4, and it is a new shape: a fix can be *sound* and still leave the
guard for it toothless, because the guard observes the cheapest-to-observe consequence rather
than the most expensive one.** F51 clears three caches. Two rebuild from a query;
`ProviderRuntimeCache` holds built Rig clients with their connection pools and is the reason the
finding was worth fixing. A guard that watched `runtime_cache` — the one with a trivially
seedable sentinel already sitting in the test file — passes against an edit that honours the
plan for that cache and keeps wiping the other two. **Verified by running it.** The rule:
*when a fix protects several things, seed and observe the one that is most expensive to lose,
not the one that is easiest to construct.*

**A second, smaller lesson, and it cost twenty minutes: the barrier a test uses can be the thing
that breaks it.** `drain_listener` in `tests/runtime_config_invalidation.rs` establishes ordering
by emitting a `provider_models` notification — which is *configuration*, so it clears the very
caches a cache-survival assertion is about. The first version of the guard failed for that
reason and not because the fix was wrong. It now brackets on the `moira_runtime_invalidations_total`
counter, which `apply_invalidation` increments **after** doing its work and on every notification
including the ones that now clear nothing. *A barrier must be inert with respect to the property
under test, and an ordering barrier borrowed from a neighbouring test usually is not.*

**F52's own verification is the part worth copying.** The question "what is the cheapest edit
that breaks the property while leaving my guard green?" had an answer here that a reading would
not have produced: attach the trigger to a new table, watch the inventory test red, then *fix
that red the lazy way* — add the name to `TRIGGERED_RESOURCE_TYPES` and stop. The inventory test
goes green. The unit guard reds, because it iterates that same constant and asserts the scope.
**Two halves that can only be satisfied together is the property a derived inventory needs**, and
it is not automatic: a derived list that nothing else consumes is just a different list.

**The pattern across both closures: the finding was right about the system and wrong about the
mechanism, in opposite directions.** F40 described a symptom that cannot occur and missed the two
defects sitting in the branch it pointed at. F47 described a mechanism correctly and
under-counted its effects. Neither could have been implemented from its one-liner — which is the
third and fourth occurrence of the shape already named in "WHAT THE FINDINGS-SWEEP BRIEF GOT
WRONG".

**Everything load-bearing was measured, not argued.** `xmin`/`ctid` advance, `version` advance,
`LISTEN` receiving exactly three notifications from three `do update` reads and zero from three
`do nothing` reads, and the duplicate-key error from two racing first-touches — all taken with
`psql` against the live schema before a line of Rust changed. The two claims that would have been
easiest to get wrong by reasoning (does `on conflict do nothing` still write? does the follow-up
`select` see a concurrently committed row?) are exactly the two that were checked directly.

**Corrections to the brief this cycle worked from**, recorded because briefs here inherit each
other's errors:

1. **"The row-level lock you are removing is currently what makes [the insert/select race]
   impossible."** True of the four `do update` members, and **inverted** for the fifth:
   `get_or_create_application_execution_policy` never had that lock and already had the race.
   Done properly the fix *removes* a correctness bug.
2. **"Count the family members rather than assuming there are two."** Good instruction, and the
   answer is five — but the interesting part is not the count, it is that the fifth member had a
   *different* bug, so "apply the same fix to all of them" would have been wrong if the fix had
   been "read-then-insert" as the finding suggested. That spelling is what the fifth already did.
3. The OpenAPI counts in the brief (**152 / 100 / 183**) were **correct** — re-derived from
   `docs/openapi.json` and unchanged by this branch. `plans/reports/HANDOFF.md` still said
   151/99/178 and has been corrected.

**One guard of my own failed before merge, and it is now HANDOFF §3.4's tenth.** F47's first
guard asserted on the write three separate ways and stayed green under `select … for update` —
because `do update` was removing *two* coupled things and all three assertions observed only one
of them. Found by running the answer to "what is the cheapest edit that breaks the property while
leaving my guard green", after having already asked it and judged the guard sound. Same sequence
as the seventh.

### Cycle 14 — 2026-08-02 — recovery cycle: three merges, two of them other people's work

**A peer session and one of my own agents both stopped mid-flight. Neither left the tree broken, and
both left work worth finishing rather than redoing.**

| PR | What | Merge | Provenance |
|---|---|---|---|
| **#59** | F39 — structured-output capability reconciled against what Rig will actually send | `655494a` | **the stopped peer's**, adopted and finished |
| **#58** | F46 — refuse `json_object` rather than send a schema only `{}` satisfies | `c938d5c` | mine |
| **#60** | F28 + F10 item 1 — bound the pool-gauge assertion; retention suite off the shared DB | `71b7dba` | **my stopped agent's**, recovered and re-verified |

**`SHARED_DATABASE_ALLOWLIST` is down to two entries** — `support/mod.rs` (owns the mechanism) and
`security_foundation.rs` (must apply migrations to a real database to assert the migration contract).
**Cross-run coupling through the shared test database is gone.**

#### The recovered agent had committed but never gated — and the distinction mattered

`fix/shared-db-flakes` carried five commits including its own ledger closures, which reads like
finished work. **There was no gates log anywhere in the scratchpad.** "It committed, so gates must
have passed" is precisely the inference §2.2 exists to prevent, so gates were re-run from scratch:
`ALL GATES PASSED`, 1046 tests. Only then was it PR'd.

#### A hazard one level below form 12: TWO runs on the SAME commit

Form 12 warns that `statusCheckRollup` can report the *previous commit's* verdict. This cycle
produced its sibling. A `workflow_dispatch` I triggered and the automatic `pull_request` event both
ran on the **same head SHA**, so `check-runs` returned `rust: completed/success` **and**
`rust: in_progress` simultaneously — both true, for different runs.

Keying on the head SHA is **not sufficient**. Select the run by `event == "pull_request"` (the one
that actually gates the PR) and read *its* jobs:

```bash
gh api "repos/{owner}/{repo}/actions/runs?per_page=15" \
  --jq --arg s "$SHA" '[.workflow_runs[] | select(.head_sha==$s and .event=="pull_request")][0].id'
```

**Corollary: do not fire a redundant `workflow_dispatch` when the `pull_request` event will run
anyway.** It buys nothing and creates an ambiguous signal at the moment you are deciding to merge.

#### `pgrep` for a live build is too coarse to gate a reclaim on

Disk hit 40 GB and `pgrep -f 'cargo|rustc'` said a build was live — which would normally abort any
reclaim, per the rule against deleting a target directory you did not create.

**It was the Moira server itself**, `cargo run` from the main checkout using `./target`, unrelated to
every `~/.cargo-targets/*` directory. Resolve the ambiguity before acting on it:

```bash
lsof -a -p <pid> -d cwd -Fn      # which tree is it in?
ps -Eww -p <pid> | tr ' ' '\n' | grep CARGO_TARGET_DIR   # which target dir?
```

22 GB reclaimed from two finished target dirs (one the stopped peer's, whose PR had merged), and a
further 13 GB later — 40 GB → 63 GB — with `./target` and the running server untouched.

#### Corrections to the briefing this cycle worked from

- **F46 was listed as a user-only item needing "a rig-core change or a public contract break".** It
  was already fixed and open as #58 — and the approach taken *was* the contract break, a 422 chosen
  to match F35's precedent, with a reversal condition.
- **The OpenAPI counts in circulation were stale**: the tree asserts **152 operations / 100 paths /
  183 schemas**, not 151/99/178.
- **F46's own recorded mechanism contained a false clause — the one implying it could not be fixed.**
  `json_utils::merge` is only reached inside a branch requiring `output_schema.is_some()`, so with no
  schema `additional_params` passes through untouched and *would* have reached an OpenAI-family
  provider. It was refused on principle, not impossibility.


### Cycle 11 — 2026-07-31 — findings sweep (`fix/findings-sweep`, `a6d2984`)

F20, F13 and the wave-2 leftovers, plus `cargo-mutants` adopted. Details in each finding's section
above and in plan 09 §0.2 (decision D-F20). Four things worth carrying forward:

1. **Fixing F20 falsified four existing tests' premises, and that was the useful signal.**
   `create_preview_redeem_grants_admin_and_consumes_the_invite`,
   `clearing_the_only_primary_is_refused_with_the_last_primary_conflict`,
   `a_non_primary_admin_cannot_promote_itself_to_primary` and
   `the_grant_administration_conflicts_are_pinned_to_their_paths` each asserted "a redeemed grant is
   not primary" or built on a deployment with no owner. Every one was rewritten to state the *new*
   premise explicitly — the non-primary test now creates an owner first, precisely so the 403 it
   asserts is still due. Adjusting an assertion until it goes green would have left four tests whose
   stated premise no longer described the system.
2. **Two of the brief's four premises were wrong in detail** (see the section below). The pattern
   from cycle 10 held: every wave that was asked to check its brief, found something.
3. **A gate log named a different worktree's path.** One `scripts/gates.sh` run wrote
   `Checking moira v0.1.0 (…/scratchpad/f15)` into a log produced from `…/scratchpad/fsweep`, and
   the same run lost every `^test result` line, which made `gates.sh` exit 1 with no diagnostic at
   all (`grep` finds nothing → `pipefail` → `set -e`). Re-run under `bash scripts/gates.sh` with
   `PWD`/`command -v cargo`/`CARGO_TARGET_DIR` echoed into the log, it was correct and green. **Echo
   provenance into every gate log**: without those three lines the anomaly was indistinguishable
   from a real failure, and it is a seventh form of the "exit codes lie here" problem.
4. **`cargo-mutants` is adopted scoped, not tree-wide** — `scripts/mutants.sh` diffs against the
   merge base and passes `--in-diff`. Deliberately **not** wired as a blocking CI gate; see
   `docs/mutation-testing.md` for the measured reason and the reversal condition.

   **First run: `63 mutants tested in 2h: 9 missed, 25 caught, 29 unviable`.** All nine survivors
   were real gaps in code written the same day, in tests written the same day to cover exactly
   that code. The worst was `set_primary`'s `is_primary && !current.is_primary` → `||`: a
   `PATCH {"is_primary": false}` on a grant that never owned anything would demote **the actual
   owner**, through a `200 OK`, around the last-primary guard — which inspects only the row being
   written. That is the ownerless state F20 describes, re-created by the fix for F20, and nothing
   in a 887-test suite noticed.

   Every survivor shared one shape: **the test exercised only the side of the boundary the code was
   written for.** A floor tested at floor−1 and at floor+1 but never at the floor; a membership
   test with no non-member; an error mapper only ever handed the error it maps. All nine are now
   killed, verified by re-applying each mutation by hand and watching the named test fail
   (`scratchpad/verify-mutants.sh`), because re-running the tool costs two hours and answers the
   same question.
### Cycle 11 (continued) — 2026-07-31 — PR #39 blocked by a pre-existing LISTEN/NOTIFY race; waves 4–5 re-audited

*The section above is the findings-sweep implementation's own record, written on the branch. This one
is the coordination cycle that carried it to merge. Both were authored as "Cycle 11" concurrently and
are kept whole rather than reconciled into a single narrative — they describe different work.*

**PR #39 did NOT merge on arrival — its `rust` job ran and failed.** Run `30617393166`: four of five
jobs green (`supply-chain`, `container-and-helm`, `console`, `console-container-and-helm`); `rust`
exited 101 on a single test.

```
test an_auth_settings_write_invalidates_the_cache_via_listen_notify ... FAILED
tests/auth_provider_settings.rs:914 — ... (CONVENTIONS §7.2): Elapsed(())
test result: FAILED. 19 passed; 1 failed
```

**Not PR #39's defect, and not overridden.** `git diff main...origin/fix/findings-sweep` on that file
is **+144/-0** — the branch only *added* the new F13 test and never touched the failing one, which is
pre-existing on `main`. The merge is still blocked: a job that ran and failed is real.

**Diagnosed as a listener-attach race in the test, not a product defect.**
`spawn_runtime_config_listener` (`src/infra/db.rs`) spawns a task that only *then* calls
`PgListener::connect_with` and `listener.listen("moira_runtime_config")`. The test spawns it and
proceeds straight to the write. Postgres delivers `NOTIFY` only to sessions **already** listening at
commit time, so if the listener has not attached when the `UPDATE` commits, that notification is lost
forever and the 10s poll spins to timeout. The intervening HTTP read normally covers the gap; under
CI load it did not. In production the listener attaches at boot and lives forever, so the lost-window
is a startup artefact with no cache populated yet to invalidate.

The fix must **establish the missing precondition, not weaken the assertion** — the test's own comment
already asks for an acknowledgement gate rather than a fixed sleep (CONVENTIONS §3). A longer timeout
or a `sleep` would hide the race, and both are forbidden here.

**Worktree hygiene:** six merged worktrees pruned (`f15`, `p08`, `p09a`, `p09b`, `p09c`, `p11`).
Disk 76 GB free — above the 60 GB threshold, no reclaim needed. `~/.cargo-targets` holds only
`moira-fsweep` (13 GB).

**The race is CLOSED — `4ea484b` on `fix/findings-sweep`. The diagnosis held, and was made
deterministic before it was fixed.**

Decisive experiment: move the HTTP read to *before* the spawn, removing its incidental delay and
changing nothing else. The test then failed **3 of 3** with the byte-identical panic and line from
CI. That settles the mechanism — `spawn_runtime_config_listener` returns its `JoinHandle` before the
task has run `PgListener::connect_with` + `listen(…)`, so the racing write's notification is **lost,
not late**, and no timeout length recovers it. Measured attach latency on a warm, unloaded machine:
**59 ms** — the size of the window CI lost.

Fixed with `wait_for_listener_attached`, an acknowledgement gate on `pg_stat_activity` polled at
10 ms inside the existing `WAIT`. **No sleep, no lengthened timeout.** The assertion is byte-for-byte
unchanged — the cache must still go `Some` → `None` from a raw `update auth_provider_settings` with
no service involvement — so this establishes a precondition rather than weakening a gate. The
reordering (populate → spawn → gate → write) is kept permanently, which makes the test *stronger*
than the original: the gate is now the only thing between spawn and write, so it cannot be masked by
incidental latency again.

**The agent improved the gate I specified, and the correction is the interesting part.** My predicate
was `query ilike 'listen%'`. That can match a `LISTEN` still *executing* — uncommitted, and therefore
not yet able to receive anything. The shipped gate adds `state = 'idle'`, because a backend reports
idle only after the statement committed, which is the exact point delivery becomes guaranteed; and
`strpos(query, $1) > 0` bound to the channel name, so it proves the *right* channel is attached
rather than any `LISTEN`. Both were verified against sqlx's `PgListener::listen` rather than assumed.

**Teeth verified by three injections, all fatal, each reporting accurately:**

| Injection | Result |
|---|---|
| dropped the `auth_provider_settings_notify` trigger (`migrations/0013`) | FAILED at the assertion, correct message |
| removed `targets.auth_settings.invalidate_all()` from `apply_invalidation` | FAILED at the assertion, correct message |
| listener never spawned | FAILED at the **gate**, naming the channel |

That third row is why the `pg_stat_activity` gate beat the probe-notification fallback I offered: a
probe gate would consult the same cache the test asserts on, so a broken `apply_invalidation` would
surface as a *gate timeout with a misleading message* instead of the real assertion failing. The
catalog observation is orthogonal to the mechanism under test, so each failure mode reports itself
honestly. Injections were made after the commit; tree verified clean afterwards.

**Gates: all six green, 891 passed.** Five consecutive `auth_provider_settings` runs, 20 passed each,
**zero skipped DB suites** in every run and in the gates log.

**`auth_provider_settings.rs` was the only ungated listener test.** `tests/runtime_config_invalidation.rs`
already had `drain_listener`, whose comment states the same property this exercise proved; and
`tests/coordination_default_path.rs` only asserts `!listener.is_finished()`, so it never needed one.
No other site to fix.

**§2.2's zsh `noclobber` hazard (form 5) recurred, in a new disguise.** The first attempt at the five
consecutive runs reported `FAIL` five times with **empty test results** — `> $(mktemp)` on an
already-existing file was refused and `cargo` never ran. `>|` fixed it. Same root cause as form 5,
different surface: a redirect to an existing file is a silent way to not run a command at all.

**Wave 4 re-audited — `85b093d` on `plan/09-wave4-multi-provider`, §0.7, 383 lines. Drift ~70%**
(31 of 44 falsifiable wave-4 claims wrong or materially incomplete; 12 hold, 2 partly). That lands
squarely in the 40/45/65/70/85% band every other re-audited plan measured.

**W4-B1 — ESCALATED, and it is the finding that matters.** `governing_policy`
(`src/infra/repositories/auth_settings.rs`) selects
`where … (issuer = $1 or trusted_jwt_issuer_id = $2) order by (issuer is not distinct from $1) desc,
created_at asc, id asc limit 1`. On a console deployment `$1` is the **console's** issuer while each
provider row's `issuer` column holds the **IdP's** — `src/application/identity.rs` says so in a
comment — so rows match only through `trusted_jwt_issuer_id`. With several providers registered
against one console, several rows share that id, they tie on the first sort key, and **the oldest
enabled row's `allowed_email_domains` governs every claim and redemption regardless of which
provider actually authenticated the user.** Reachable both ways: a permissive first provider silently
widens a restrictive second; a restrictive first silently denies a correct second.

This is plan 08's B1 signature again — *the plan states a per-provider allowed-domain policy and no
named test exercises it.* `ambiguous_enabled_providers` is the only thing holding it back, and wave
4's stated purpose is to remove that guard. **If wave 4 is descoped, the guard removal is descoped
with it.**

The audit's stronger structural claim, which decides what wave 4 can honestly promise: **Moira cannot
see which upstream IdP authenticated a user** — the JWT it receives is the console's. If that holds,
per-provider domain policy is not merely buggy but unenforceable at Moira's layer, and enforcement
has to sit in the console.

**Under adversarial verification before any code is written** (run `wf_f1c8b6c2-5b7`): three
independent lenses on B1 — SQL semantics against a live rolled-back transaction, the shipped console
data model, and reachability on today's code — plus one each on B2/B3/B4. Recorded here as *claimed*,
not as established. The method note from cycle 9 applies: an agent correction is not self-proving,
and one of them was itself half-wrong.

Other blockers claimed: **B2** an unknown provider-kind row makes `auth_method_from_db`'s catch-all
500 the anonymous login endpoint for *every* provider (migrate-then-roll makes it reachable);
**B3** changing the console providerId scheme orphans the shipped secret — the AEAD AAD binds it, so
it cannot decrypt rather than merely miss; **B4** the TS `AuthMethod` union has no drift gate against
the spec enum. **B5** verified empirically in a rolled-back transaction: the unique index refuses two
discovery-only OIDC providers.

**Brief corrections from the audit** (this is why every brief ends with the question): next free
migration is **`0019` on `main` today** — `0020` only after PR #39 merges, since that branch carries
`0019_single_primary_admin.sql`; `0016` is a permanent gap. Wave 3 never shipped `middleware.ts` —
the session gate is the `(console)` route-group layout. `PublicSignInMethod` already carries what a
GitHub button needs; only `provider_id` is missing. F21 is a **wave-2 backend** defect by domain,
reassigned to wave 4 only because another wave-4 task already opens that file.

**Wave 5 re-audited — §0.8 on `plan/09-wave5-invitations-ui`. Drift ~83% (43 of 52)** — the highest
measured in this project, beating plan 11's 85% only narrowly. 9 hold, 6 partly, 1 unestablished,
**36 wrong**. Roughly half is "already shipped, differently" (waves 2–3 built more than predicted);
half is "specified against something that does not exist".

**Wave 5's headline: it is not three UI features.** As written it is two UI features and one
**backend** feature disguised as a third.

- **Recovery has no backend at all.** Wave 2 took an undocumented decision (D-W2-1) to omit
  `is_recovery` / `replaces_admin_identity_id`, **recorded only in a migration comment and two
  catalog comments** — not in this ledger, not in the plan. So `RecoveryPanel`, `recovery.e2e.ts`,
  `recovery_invite_gets_no_domain_policy_exemption` and `admin_identity_recovered` are all
  unbuildable. **Cut.** *This is the finding to learn from:* a decision recorded only in code
  comments silently removed a third of a later wave's scope, and nothing surfaced it until an
  auditor went looking. The standing authority requires decisions in a plan's §0 **with a reversal
  condition** precisely so this cannot happen.
- **`redeem` cannot be registered under any existing `MoiraCredentialRequirement`.** Its spec
  security is `bearerAuth` alone. `admin` fails the contract test *and* `#buildHeaders`' `admin` arm
  **prefers the system key**, which would send the bootstrap credential on an invitee's redemption.
  `none` 401s. Needs a fourth variant (`bearer_only`).
- **Two DTO fields fail a shipped guard**: `SECRET_DTO_FIELD_PATTERN` matches `token`, so
  `AdminInvitePreviewRequest.token` / `AdminInviteRedeemRequest.token` red `server-only-guards.test.ts`.
- **Mounting `OnceOnlySecretModal` reds `secret-leak.e2e.ts` by design** — an armed tripwire that
  names this wave.
- **The a11y gate is vacuous for every route inside `(console)`.** There is no authenticated
  Playwright state, so `page.goto` follows the redirect and audits `/login` instead. Already true of
  `/` today — so the gate has been passing while auditing the wrong page.

**Session management stays cut.** Durable storage shipped in wave 1, which satisfies half the
recorded reversal condition — but the other half is what wave 5 builds, and three independent reasons
stand against it: the plan's session scope silently includes an operator-editable lifetime policy
**persisted in Moira** (a frozen-contract change); `bun test`/`next dev` default to `memoryAdapter`,
so unit tests would exercise a store the feature does not use; and its coverage lands behind the same
a11y silence as every gated route. `DELETE /admin-identities/{id}` already revokes *authorization*,
which is strictly stronger than revoking a session. The auditor **could not read Better Auth
1.6.25's `listSessions`/`revokeSession` surface** (`node_modules` absent) and recorded that as
unverified rather than assuming it — correctly.

**In flight / done this cycle:**

| Agent | Branch | Doing |
|---|---|---|
| A | `fix/findings-sweep` | **done** — attach race closed `4ea484b`; CI dispatched as run `30628522675` |
| B | `plan/09-wave4-multi-provider` | **done** — §0.7 committed `85b093d` |
| C | `plan/09-wave5-invitations-ui` | **done** — §0.8 committed |
| verify | read-only | **done** — `wf_f1c8b6c2-5b7`, six verifiers; raised F23/F24/F25 |
| design | read-only | `wf_05ebcb68-1b2` — three wave-4 designs, three judge lenses, one decision |

### Cycle 10 — 2026-07-29 → 07-31 — plans 10 and 08 MERGED, plan 11 started

**Plan 10 merged** `671eadf` — cluster admission lease, leader election, durable worker queue,
Redis behind a flag. All three CI jobs green. 744 tests. Locks mutation-tested by neutering them
and watching 7 fail.

**Plan 08 merged** `f0ecbbc` — Next.js console, mock-first OAuth, B1-correct wizard, distroless
image. **Five** CI jobs green (it adds `console` and `console-container-and-helm`). PR #23 closed
as superseded; its scaffold shipped inside #33.

**Plan 11** — §0 written (`85a3c08`), Wave 1 landed (`a641fa2`): real ingestion pipeline, 779 tests.

**Findings raised:** F13, F14, F15 (see the table above). F15 is the one worth a human's attention.

**Three things worth carrying forward:**

1. **A private `CARGO_TARGET_DIR` is not isolation.** The coordinator ran `git checkout` in a tree an
   agent was working in and its commit landed on `main`. Rule now: any agent that commits gets its
   own **worktree**. Recorded in the standing authority.
2. **Mutation testing keeps finding laundering.** Plan 11 Wave 1 faked an `'indexed'` status on the
   create path; the status assertion **passed**, because the supersession `CASE` rewrote the fake
   value. Only a row-level assertion caught it. A test that checks the field the code also writes is
   not a test. The same trap is live for citations — counting them proves nothing about whether they
   correspond to the chunks retrieved.
3. **Briefs get the shape right and the specifics wrong.** Across four waves the agents corrected the
   coordinator ~20 times, several load-bearing: `distroless/nodejs24-debian12` does *not* pass Trivy
   (only debian13 reaches 0/0); the §0 Redis-payload guidance would have caused the exact defect it
   warned against; "four `pending` pins" was really ten; a third RAG ingestion entry point was missed
   entirely. **Expect the brief to be wrong in detail and ask agents to say so** — every wave that was
   asked, found something.

### Cycle 9 — 2026-07-27 — plan 07 MERGED; plans 08 and 10 re-audited

**Plan 07 merged as `27b6e0c` — the first CI-verified merge in this sequence.** Run `30257760315`:
`rust` success (13 steps), `supply-chain` success (10), `container-and-helm` success (13). Every
prior plan landed under the infrastructure override on local gates alone. Evidence posted on PR #31.

**The override's premise was stale, and the outage was concealing real defects.** Repairing three
broken action pins is what made the rest visible — while `container-and-helm` died at step 1, trivy
never ran. Found and fixed as a direct consequence: F11 (retention could delete a whole table),
F12 (36 container CVEs), the missing `cargo audit` gate, and two shared-database isolation bugs.

**Plans 08 and 10 re-audited in parallel** — the first wave to genuinely run in parallel, now that
`debug = 1` made per-agent `CARGO_TARGET_DIR` affordable.

**Plan 10** (`d788ad6`, `7aa1780`): ~45 of ~70 citations stale, 10 blockers. Its Redis breaker
registry would not compile (plan 04 added a fifth method); its 3-call invalidation sequence is now
four and would leave plan 07's auth cache stale on replicas while reintroducing the unconditional
breaker reset; two i18n keys it insists are missing already exist; the metrics module it describes
was rewritten by plan 05. **The retention worker's own module header is addressed to plan 10** —
"there is no leader election in Moira today (that is plan 10)" — while plan 10 still lists retention
as out of scope. Three deployment gaps that only surface in a cluster: workers are **off** in the
shipped chart, `pod_name` has no downward-API env var, and a rolling update can deadlock against the
lease ceiling (the grace period clears it by luck, not design).

**Plan 08** (`b4ef754`): five blockers, one of them fatal to the whole design.

**B1 — the wizard as specified could never succeed.** `governing_policy` matches the provider row on
`issuer = $1 or trusted_jwt_issuer_id = $2` where `$1` is the *claim body's* issuer. Plan 08 writes
the row for the IdP, sends the console's issuer in the claim, and never sets `trusted_jwt_issuer_id`
— it is absent from the plan's field list entirely. Neither branch matches → `policy = None` →
**403 on every run**, including the plan's own happy-path e2e. The console would have been built
end-to-end and failed at the last step, every time.

The tell: plan 08 states an invariant — the console issuer must not assert scopes — that Moira
enforces *only* when `trusted_jwt_issuer_id` is supplied. Since the plan never supplies it, **it
never exercises its own stated invariant.** That is the signature of a design written against an API
sketch rather than the shipped one.

**Method note.** Both audits were told to be exhaustive rather than representative, and both found
that the consequential items were not the obvious ones. Agents corrected me on five points in plan
08 and three in plan 10; I verified each disagreement myself rather than taking either side. One
agent "correction" was itself half-wrong — it removed a real advisory-lock key from a
collision-avoidance list on the grounds that production does not take it, when the risk is that
*tests* do, against the same database (`7aa1780`).

### Cycle 8 — 2026-07-27 — plan 07 implemented

Branch `plan/07-identity-foundation`, four waves. **All five gates green: 622 passed, 0 failed,
0 skipped**, run twice consecutively from cold (F5 template-database flake did not reappear).
OpenAPI 131 → 141 operations, snapshot regenerated and committed; zero `rotate-secret` references.

| Commit | Wave |
|---|---|
| `d291e47` | 1 — migrations `0012`/`0013`, domain DTOs |
| `621b498` | B3 fix — `auth_provider_settings` keeps provider breakers |
| `bf5a744` | 2 — repositories, services, deny-by-default domain policy |
| `d5f5aa8` | 3 — D2 auth wiring, HTTP surface, auth-settings cache, OpenAPI regen |
| `9718273` `113c08b` | 4 — the named-test coverage gap |

**D2 verified directly, not on report.** `apply_admin_identity_grant` is called at exactly one site,
`src/security/auth.rs:334`, inside `authenticate_admin`'s bearer branch. The two `authenticate_caller`
paths (`:383`, `:394`) receive the ungranted actor. `git diff` on `src/security/authz.rs` is empty.
`a_granted_identity_gains_admin_scope_only_on_the_admin_plane` pins both directions and asserts the
403's message names the missing scope, so it cannot be confused with the application-binding 403.

**Three real defects the plan would have shipped**, none of which the citation audit found — they
needed an implementer and a live database:

1. **`order by (issuer = $1) desc` applies the wrong policy.** `issuer` is nullable, `null = $1` is
   `NULL`, and Postgres sorts nulls **first** under `desc` — so an issuer-less row outranks an exact
   match and the wrong `allowed_email_domains` governs a grant. Fixed with `is not distinct from`.
   No unit test can see this; it needs a real sort.
2. **The plan's unique index is invalid SQL** — a bare `COALESCE` in an index column list.
3. **`auth_provider_settings` would have reset every provider circuit breaker on every write** (B3),
   found by the audit but only closable outside Wave 1's file set. Fixed in `621b498`, teeth verified.

**Named-test audit: 80 extracted, 80 verified**, deliberately not sampled — plan 05's two false
"complete and green" calls both came from checking a subset and generalising. 21 tests written; 5 are
unwritable because D1 cut the setup-token path, which the plan's own D1 text predicts. Five of the
highest-risk new tests had their teeth checked by injection.

**A false claim of mine was caught and corrected** (`c3f6c09`): §0.4 said plan 06c made *any* missing
catalog entry a `cargo build` failure. The `const` block covers `ExecutionFailureClass::ALL` only;
everything else is a source-walking **test**. Still gated, but a green build is not proof of a
complete catalog. §0.4's correction was also narrower than its phrasing implied — a third code,
`setup_claim_credential_required`, was missing and Wave 3 found it.

**Process note:** two agents wrote commit messages to the same scratchpad `msg.txt` and one commit
briefly carried the other's message. Content was never affected — `git commit --only -- <paths>`
scoped correctly throughout. **Use inline `-m`, or a per-agent path.** Add this to the shared-index
lesson: the scratchpad is shared state too.

### Cycle 7 — 2026-07-26 → 07-27

- **F8 CLOSED** `8039c53` — admin implication is an allow-list; `Anonymous` + `moira:admin` no
  longer receives it. **F9 DECIDED** — admin plane only, hooked at `authenticate_admin`.
- **F7 CLOSED** `d7580a6` — `tests/rig_boundary.rs`. The rule was described as test-enforced and was
  not; there was no import-scanning test in the tree at all. Checked in both directions (a stale
  allow-list entry fails too), with vacuity guards. **Teeth verified**: injecting `use rig_core::…`
  into `src/domain/runtime.rs` fired both assertions naming the right file, then reverted.
- **D2 done** `c45257f` — the four `TODO(plan-07)` markers are now `TODO(post-deploy)`. Plan 07 is
  next in the order, so "plan 07 owns this" was about to cash out as a removal ~a day early.
- **Plan 07 Wave 0 rewrite** `0ee1419` — §0 drift table, 303 insertions. **8 blockers**: 3 would not
  compile (`AdminCommandRunner::new` arity, `AdminCommandMutation::new` returns `Result`,
  `src/application/admin.rs` no longer exists), 1 is a silent prod regression (attaching NOTIFY to
  `auth_provider_settings` resets **every** provider circuit breaker on every write, because
  `circuit_reset_scope` treats an unknown table as unknown rather than harmless), 1 is a migration
  collision (`0009`/`0010`/`0011` all taken → `0012`/`0013`/`0014`), and 3 instruct working around
  defects 06b already fixed. **Module 13 was missing entirely** — assigned to an agent, depended on
  by the DoD and an e2e test, specified nowhere.

**Method note.** The audit checked ~62 citations one at a time rather than sampling: ~40 were stale.
A sample of ten would have found the file-level rot and missed B3, which is the only one that would
have reached production. Plan 06's Wave 0 found 14 problems the same way. **Assume a plan written
before the preceding plan merged is wrong about the tree, and budget a cycle to prove where.**

### Cycle 4-6 — 2026-07-26

- **Plan 06 MERGED** `39c5326` — 12 modules. Found and fixed a real cross-issuer response disclosure
  (Module 16), a latent credential-decryption bug (Module 9), and a demonstrated test flake (Module 13,
  suites ~3x faster). The plan document itself was wrong in 14 places and one module did not compile;
  rewriting it first cost one cycle and saved a wasted wave.
- **Plan 06b MERGED** `46c2c74` — all 33 If-Match sites atomic, `ensure_version` deleted. Both failure
  modes verified by reintroducing them, including the 404-vs-409 regression the plan predicted.
  One accepted behaviour change: removing the pre-read also removed an incidental `:read` scope
  requirement on writes. Write scope is enforced throughout; user decided to merge as-is.
- **Plan 06c MERGED** `627fe4d` — closes F1. A missing catalog entry is now a COMPILE error.
  The new gate immediately found a second gap nobody had scoped: `validate_override` forwards its
  code through a parameter, so four more codes reached clients uncatalogued while every gate was green.
- **F3 closed** `c546f08` — skill files retargeted at the real tree.

**Process lessons worth keeping:**
1. **Commit before running injection/teeth tests.** `git checkout --` discards uncommitted work, not
   just the injection. This cost work three times in one task. The fix is not "revert more carefully"
   — it is to have nothing uncommitted at risk.
2. **Every new gate needs a vacuity guard.** `every_validate_override_code_has_a_catalog_entry`
   asserts it finds >=5 codes; that assertion caught two parser bugs that would otherwise have left
   the test passing while checking nothing.
3. **Parallel agents share one git index.** Disjoint files are not enough — use worktree isolation or
   `git commit --only -- <paths>`.


### Cycle 1 — 2026-07-26
- Opened PR #27 with the seven required sections and the full evidence bundle.
- Discovered the repo-wide CI outage above. Did **not** merge: the merge precondition is unmet, and
  the failure is infrastructure the loop cannot repair.
- Re-audited plan 06 (read-only); recorded findings here and in `HANDOFF-PROMPT.md` §5.1.
- **Next action:** plan 06 Wave 0 — rewrite the plan against the repo (correct stale line numbers,
  fix the Module 9 build break, cut the satisfied i18n scope, and add the two omitted items). This
  needs no CI, so it proceeds while PR #27 waits.
- **Blocked on user:** GitHub Actions billing/runner state.

### Cycle 2 — 2026-07-26
- User granted the infrastructure-failure merge override; encoded in `HANDOFF-PROMPT.md` §6.5 with
  four required conditions, deliberately narrow so it can never cover a job that actually ran.
- Verified all four conditions on PR #27, including re-running all five gates on the exact merge
  commit `5e7335a` rather than reusing the earlier `c40af24` results.
- **Merged plan 05** → `main` at `3ea8037`. Verified the artifacts landed and the `mask_plain_secret`
  fix is present on `main`.
- `cargo clean` after merge, per the standing rule: reclaimed 15.9 GiB, 148 Gi free.
- **Next action:** plan 06 Wave 0 plan rewrite, then the `SecretString` commit, then the exec waves.

## F26 — "Admin write + audit row still non-atomic" was **36 sites, not all of them**, and reachable from a header — **FIXED** `3825fb0`

The one-liner had been carried unowned and unanalysed for many cycles. It was **true**, and in one
respect **worse** than recorded; in another it was **broader** than the code. Both halves matter,
because the entry as written invited a sweep of every admin mutation, and 20 of them never had the
defect.

### What was actually true, per path

| | Sites | Audit row written | Could diverge? |
|---|---|---|---|
| Inside `AdminCommandRunner::execute` | **20** | `PgAdminCommandTransaction::insert_audit`, in the command transaction | **No** — already atomic |
| `PATCH` / `DELETE` / `enable`-`disable`, plus `validate`, `revoke`, `refresh` | **23** | `shared::audit_success` → `PgAdminRepository::insert_audit` → `pool.acquire()` | Yes |
| Every `RuntimeAdminService` mutation | **13** | `RuntimeAdminService::audit_success` → same second connection | Yes |

**36 divergent sites**, counted three ways that agreed: 36 `audit_success` call sites, 36 write
methods needing the parameter, and 36 `commit_with_audit` calls after the fix (20 `AdminRepository`
+ 13 `RuntimeRepository` + 3 `AuthProviderSettingsRepository`). Not estimated.

The 20 safe ones are the *creates* and the identity mutations — `claim`, `create_invite`,
`revoke_invite`, `redeem_invite`, `set_identity_primary`, `revoke_identity`, and every
`create_*`/`rotate_*`. They write `success_audit(…)` through the command transaction, so a failed
audit `INSERT` returns `Err`, the runner rolls back, and neither row survives. **A path that cannot
diverge is not a finding**, and the ledger line did not say which was which.

### The reachable direction, and the failure that reaches it

Only **write commits, audit row does not**. The audit was written strictly after the write's own
transaction had committed, so the phantom direction (audit without write) was never possible on
these paths. `audit_denied` writes exactly such a standalone row *by design* — it records a refusal,
there is no write for it to be atomic with, and its failure is deliberately swallowed so the admin's
response cannot vary with database state. It is untouched.

**It was not only a crash window.** `RequestContext::from_headers` (`src/application/context.rs:17`)
takes `x-request-id` from the caller **verbatim and unbounded**; `audit_logs.request_id` is
`varchar(128)` (`0003:322`). A 129-character request id fails the audit `INSERT` — and nothing else —
with SQLSTATE `22001`. One ordinary HTTP request, deterministic, no fault injection: the admin change
committed, the caller got a `500`, and `audit_logs` held nothing. That is the whole finding,
executable.

### What changed

Every write method on `AdminRepository`, `RuntimeRepository` and `AuthProviderSettingsRepository`
now takes an `AuditLogInsert` and ends with **`commit_with_audit(tx, audit)`**, which inserts the
audit row on the transaction's own connection and then commits *that* transaction. Eight writes that
had no transaction at all (`mark_credential_validated`, `revoke_key`, `soft_delete_key`,
`touch_trusted_jwt_issuer`, and the four runtime `create`/`put` methods) were given one.

`commit_with_audit` takes the transaction **by value** on purpose. A required `audit` parameter only
forces the row to be *carried*; it does not stop a later edit from moving the insert below
`tx.commit()`, which is exactly the pre-fix arrangement. Consuming the transaction removes the
sequence there is to reorder.

`shared::audit_success` is **deleted**, so a `Success` audit row for an admin mutation can no longer
be produced outside the write. The version check still runs first, under the row lock, so a `409` or
`404` writes nothing — unchanged.

Idempotency is untouched: `AdminCommandRunner::execute` still rolls back on any non-cacheable
failure and `is_cacheable_admin_failure` still excludes `Forbidden` (F19). A `22001` from the audit
insert is `AppError::Sqlx` → `500` → non-cacheable → full rollback.

### Forced-failure proof — `tests/admin_audit_atomicity.rs`, five tests

Not an argument, and not a happy-path observation. The audit `INSERT` is *made to fail* inside a real
transaction against a real Postgres, through the installed HTTP stack, and both rows are then
asserted absent. Each test states its premise (`500`, i.e. the injection fired) before asserting the
property, because "no audit row" is also satisfied by a `PATCH` that never ran.

**Verified failing under mutation, twice, and reverted.** By *reproducing* the `cdb2f46`
arrangement, not by checking `cdb2f46` out — at `cdb2f46` these tests do not compile, because the
write methods took no `audit` argument. That distinction is the honest one and is written into the
test's own module docs.

* `patch_application`'s `commit_with_audit` replaced by `tx.commit()` + a second `pool.acquire()`:

  ```text
  the write must be rolled back with its audit row: at cdb2f46 the UPDATE commits and the audit
  INSERT does not, which is the finding
    left: "audit-suppressed"
   right: "Lifecycle 019fb91151147c128c5c8dc0c507cd6e"
  ```

* the same mutation applied to all 13 `PgRuntimeRepository` sites:

  ```text
  a runtime-admin write must be rolled back with its audit row
    left: "route-audit-suppressed"
   right: "Lifecycle route 019fb913ffe57c228570280070e7dbeb"

  the INSERT and its audit row must be rolled back together
    left: 1
   right: 0
  ```

`the_same_write_without_the_injection_commits_both_rows` is the vacuity guard: the identical `PATCH`
with a normal request id must return `200` **and** leave exactly one audit row, so the first test
cannot be green because the endpoint is broken.
`a_create_inside_the_command_envelope_was_already_atomic` passed before the fix as well — that is its
job, pinning the 20 sites that were never part of the finding.

### The cheapest edit that breaks the property and leaves the guard green

**Making the same one-line change at any of the 32 sites no test names** — `patch_credential`,
`soft_delete_provider`, `set_trusted_jwt_issuer_status`, any of them.

Measured, not asserted. `PgAdminRepository::patch_credential` was reverted to `tx.commit()` plus a
second `pool.acquire()` and `scripts/gates.sh --fast` run against it: **`ok — 906 passed`,
`ALL GATES PASSED`.** Nothing in the tree notices. Two of the three gate-detectable properties this
project has been burned on are therefore *not* what defends the other 32 sites.

The guard covers four sites across the two repository traits and both body shapes; it does not, and
economically cannot, cover 36 endpoints. What covers the rest is structural and should be judged as
such: the `AuditLogInsert` parameter is required, `audit_success` no longer exists, and
`commit_with_audit` consumes the transaction — so reintroducing the divergence means hand-writing a
`commit` **and** a second `pool.acquire()`, which is a visible act rather than a moved line. Anyone
extending this guard should add sites, not replace the mechanism.

### Reversal condition

Reverse if a repository write must commit while its audit row does *not* — the only sound reason
being an audit row that has to survive the write's rollback, as `audit_denied`'s does. That is a
different row (`AuditResult::Denied`) on a different path, so it is not a reason to revert this. If
it is ever reversed, `tests/admin_audit_atomicity.rs` must be deleted in the same commit with the
reason written down, not left `#[ignore]`d.

### Three things found alongside it, recorded and NOT fixed

1. **`RuntimeAdminService::record_idempotency` is still non-atomic with its own write**, at all 13
   sites, on a third connection after the write and audit have committed. A failure between them
   leaves the mutation done and no ledger row, so a retry carrying the same `Idempotency-Key`
   executes a second time. The 20 envelope paths do not have this: `finalize_idempotency` runs
   inside the command transaction. **This is a separate finding about a separate table**, and
   closing it means moving `runtime_admin` onto `AdminCommandRunner`, which is a design change.
2. **`x-request-id` is unbounded into a `varchar(128)` column.** After this fix it fails closed —
   `500`, nothing written — but a client can still turn any audited admin write into a `500` with
   one header. It arguably belongs in the `400` family. Left alone deliberately: it is the lever
   `tests/admin_audit_atomicity.rs` injects through, and bounding it silently would turn those
   tests green-for-the-wrong-reason. **If it is ever bounded, the tests fail rather than pass** (the
   `500` premise is the first assertion) — replace the lever with a `raise`ing trigger in the
   fixture's private database, do not delete the tests.
3. **`audit_logs.actor_type` carries two spellings.** `runtime_admin` writes `format!("{:?}", …)`
   (`"SystemKey"`); `admin::shared` lowercases it (`"systemkey"`). Deployed rows carry both.
   Preserved exactly, and now documented at `runtime_audit`: unifying them is a data change, not a
   refactor, and does not belong in an atomicity fix.


## THE SHARED TEST DATABASE CANNOT HOLD TWO MIGRATION SETS AT ONCE — it reds a gate run that has nothing wrong with it — 2026-08-01

`tests/support/mod.rs::sweep_leaked_databases` drops **every** `moira_test_template_*` that is not
the calling process's own. The template's name is a SHA-256 over the migration set
(`template_database`, `tests/support/mod.rs:785`), so two worktrees whose migrations differ by a
single file have different template names — and each one's `prepare_template` destroys the other's.

That is worse than a wasted rebuild. `prepare_template` runs **once** per process, under the
*exclusive* advisory lock. Every fixture afterwards takes only the **shared** lock, and only for the
duration of its own `create database … template …`. Nothing holds the template alive *between* the
prepare and the last fixture, so a neighbouring worktree that sweeps inside that window leaves the
running suite cloning from a database that no longer exists:

```text
clone the migrated template database: Database(PgDatabaseError { severity: Error, code: "3D000",
message: "template database \"moira_test_template_9a2ad893dabfee7b\" does not exist" … })
```

Observed on `fix/admin-audit-atomicity`, whose `migrations/` is byte-identical to `main`: three
`tests/secret_leak_snapshots.rs` tests failed this way and `scripts/gates.sh` reported
`FAILED: test`. The immediate re-run was green, with no code change between them.

**Two things to carry forward.**

1. **A red gate run here is not automatically a code failure.** Read the panic before concluding
   anything. `3D000` naming a template is a neighbouring worktree, not a regression — and it is
   otherwise indistinguishable from one, which is precisely the failure mode this ledger keeps
   warning about.
2. **The gate's `log holds N of M integration targets` line fires as a *consequence* of any test
   failure**, because `cargo test` stops scheduling further targets after the first one fails. It is
   a second symptom of one problem, not a second problem. Do not go looking for a truncated capture.

**Mitigated, not cured, on `fix/admin-audit-atomicity`.** `TestDatabase::create_with_max_connections`
now goes through `clone_template`, which on `3D000` rebuilds the template under the exclusive lock
and retries the clone **once**. Retrying once and no more is deliberate: a second `3D000` means a
neighbour is sweeping faster than a template can be built, which is a real environmental problem and
should fail loudly rather than spin. The sweep's semantics are untouched, so a genuine schema drift
still surfaces — the rebuild runs the same `MIGRATOR`.

The cure, when someone takes it, is for the sweep to spare templates other than its own. A template
with no client backends is exactly what an idle-between-fixtures neighbour looks like, so "no client
backend" cannot distinguish a leak from a live run; that check needs a different signal (an age, or
an owning-run marker) before it can safely drop a template it does not recognise. Sweeping
`moira_test_building_*` and aged fixture clones stays correct as it is.


## AN UNMERGED MIGRATION IN ONE WORKTREE BLOCKS EVERY OTHER WORKTREE'S LIB TESTS — including `main`'s — 2026-08-01

Worse than the template sweep above, and a different mechanism. **Integration** suites clone a private
database from a template built out of the *calling* tree's migrations, so they are isolated. **Lib
unit tests are not**: `src/**/tests` run `MIGRATOR.run` against `MOIRA_TEST_DATABASE_URL` — the shared
`moira` database — directly.

`fix/f14-memory-content-hash`, in a sibling worktree, carries
`migrations/0021_memory_content_hash_is_content_addressed.sql` and its test run applied it. The
shared database's `_sqlx_migrations` now holds version 21. Every tree that does **not** have that
file — this branch, and `origin/main` itself, which is still at `0020` — now fails every
database-backed lib unit test with:

```text
migrate: Internal("run migrations: migration 21 was previously applied but is missing in the
resolved migrations")
```

`cargo test` fails on the lib target and stops, so `scripts/gates.sh` reports both `test` and
`test:incomplete-log` with **0 of 36** integration targets logged — the whole suite never runs.

**This is not repairable from the affected side.** Deleting the row would break the branch that owns
it, and the standing instruction on the shared database is not to destroy state other agents depend
on. It clears when `0021` merges to `main`, or when the shared database is rebuilt by someone who
owns both.

**The rule that follows: an unmerged migration must not be applied to the shared test database.** A
branch that adds one needs its own database (`MOIRA_TEST_DATABASE_URL` pointing somewhere private) for
as long as it is unmerged, because the cost of getting this wrong is not borne by the branch that adds
the migration — it is borne by every *other* agent, silently, in a failure whose message names a
migration they have never heard of. The template-clone path already gets this right; the lib unit
tests are the hole.

**Verified this way on `fix/admin-audit-atomicity`:** the six gates run green end to end against a
byte-identical database that simply has not had `0021` applied to it, and red against the shared
`moira` on the same commit, with nothing else changed.

```text
── fmt
   ok
── clippy
   ok
── test
   ok — 906 passed
── release
   ok
── deny
   ok
── audit
   ok

ALL GATES PASSED
```

`MOIRA_TEST_DATABASE_URL=postgres://postgres:postgres@127.0.0.1:5432/moira_atomicity` — created for
this run, migrated from `0001` by the suite itself, and named outside `moira_test_%` so the leak
sweep leaves it alone. `scripts/gates.sh`'s own two integrity assertions are what make that summary
trustworthy: neither the `36 of 36 integration targets` check nor the zero-skip check fired, so no
suite was silently absent and no DB-backed suite skipped.

**Three further ways a gate run died here, all environmental, none from the code under test** —
recorded because each was individually indistinguishable from a regression:

| Symptom | Cause |
|---|---|
| `3D000 template … does not exist` | neighbouring worktree's leak sweep (see the section above) |
| `57P01 terminating connection due to administrator command` | same neighbour, different statement |
| `gates exit=137` | SIGKILL. Two full Rust test suites on one machine; ~56 MB free at the time |

Exit 137 is worth its own line: it is not a test failure at all, and `scripts/gates.sh` cannot
distinguish it from one. **Two agents must not run `cargo test --workspace` against this machine
simultaneously** — not for the database's sake, for the RAM's.


## THE TEST DATABASE INFRASTRUCTURE, CURED RATHER THAN MITIGATED — issue #77 — 2026-08-05

Three properties the two sections above described as open. Each is now closed by a mechanism, and
each has a test that fails without it.

### 1. A missing database is a failure, not a skip

`database_origin` used to print one line and return `None`, and every database-backed test then
returned early **and reported success**. `scripts/gates.sh` asserts zero skip lines, which was the
only guard — and it turns out it was not a guard at all.

**`libtest` was eating the evidence.** A test's output is captured and printed only when the test
*fails*, so a skip announced with `eprintln!` from a test that then reports `ok` never reached the
log the gate greps. Measured on this branch, before the fix: with the opt-out in force,
`cargo test --test retention_worker` redirected to a file held **zero** occurrences of `skipping`
while all eight tests reported `ok`. Every skip line now goes to `std::io::stderr()` directly
(`tests/support/mod.rs::announce_skip`), below the capture, and the same measurement yields **1**.

The suites themselves now refuse. `MOIRA_TEST_ALLOW_NO_DATABASE=1` is the single, deliberately
long-named opt-out, ignored when `CI=true`. With the variable unset,
`cargo test --test retention_worker` reports `0 passed; 8 failed`.

### 2. The template sweep spares a neighbour and still reclaims a leak

The cure the section above named — "an age, or an owning-run marker" — is a `COMMENT ON DATABASE`
carrying `moira-test-template last-used=<epoch>`, refreshed by every test binary's
`prepare_template` and read back through `shobj_description`. `template_sweep_verdict` then reads:
recently claimed → **spare**; claim older than an hour → **drop**; no marker this harness
recognises → **spare and stamp**, so a template built before markers existed becomes reclaimable
one grace period later instead of never. A `cargo test --workspace` run re-stamps its template
dozens of times over a few minutes, which is one to two orders of magnitude inside the grace.

`tests/test_database_sweep.rs` asserts **both** halves against a real cluster, holding the same
exclusive advisory lock `prepare_template` sweeps under. With the old rule restored, its two
sparing cases fail and its two reclaiming cases still pass — which is the point: sparing everything
would have traded the flake for an unbounded disk leak.

### 3. An unmerged migration can no longer poison anything

Not "better reported" — unreachable. The library's `#[cfg(test)]` modules no longer connect to
`MOIRA_TEST_DATABASE_URL` at all. `src/test_support.rs` creates **one private database per test
process** (`moira_test_<epoch>_<uuid>`, the same name grammar a fixture clone uses, so the existing
age-bounded sweep reclaims it with no new rule), migrates it once, and hands every test a pool onto
it. The URL is now read only for its host, port and credentials.

The two advisory locks that guard the singleton rows — `SetupStateLock`,
`ISSUERLESS_GENERIC_OIDC_LOCK_KEY` — stay exactly as they were. They now serialise the threads of
one process instead of every checkout on the machine, which is strictly less contention for the same
guarantee, and their comments say so.

The diagnostic half is kept anyway, because the next person to point `MOIRA_TEST_DATABASE_URL` at a
colleague's database will meet the same `sqlx` message: `MigrateError::VersionMissing` is translated
into text naming the database, the cause and the remedy. It is proven by
`the_missing_migration_explanation_names_the_cause_and_the_remedy`, which fabricates the condition
on a real database rather than mocking it — the assumption most likely to rot is that `sqlx` reports
this situation as `VersionMissing` at all.

### 4. A migration-contract path for a restricted CI

`tests/security_foundation.rs` needed `CREATEDB`, with no alternative. `MOIRA_TEST_MIGRATION_DATABASE_URL`
now names a pre-provisioned **empty** database that the suite migrates and mutates in place, over the
identical `run_migration_contract` code path. It is a second variable rather than an overload
because that database is deliberately damaged (`alter table responses drop column updated_at`), and
the suite refuses to start if its public schema is not empty.

### What did not change, and one thing to watch

The `3D000` retry in `clone_template` stays. It is now a second line of defence rather than the
mitigation, and a `3D000` from here on genuinely warrants suspicion rather than a shrug.

**A transitional window exists while other checkouts are still on the old code.** A neighbour
running the pre-#77 sweep still drops every foreign template on sight. Nothing on this branch can
prevent that; `clone_template`'s single retry is what covers it until the change is everywhere.

---

## F50 CLOSED — fail-closed, `feat/agent-profile-fail-closed-79`, 2026-08-06

**The product decision F50 was waiting on has been taken by the maintainer: fail-closed.** A route
naming an agent profile the runtime cannot use refuses the request. Issue #79.

| Finding | Verdict |
| --- | --- |
| **F50** | **closed.** The silence was fixed in `8d983aa`; the request is now refused as well. Nothing about the observability changed, which is what splitting it out in the first place was for |

### What shipped

`get_active_agent_profile` is **gone**, not bypassed. It filtered
`status = 'active' and deleted_at is null` and answered `Ok(None)` for two different conditions, so
no caller of it could have distinguished them. Its replacement,
`find_agent_profile_reference`, selects `where id = $1` and nothing else;
`domain::AgentProfileResolution::classify` is the only thing that interprets the row. That the old
method no longer exists is deliberate: the cheapest way to un-fix this would have been to call the
lenient lookup again, and it is not there to call.

Two failure classes, because the two conditions have different remedies:

| Condition | Class | HTTP | Remedy |
| --- | --- | --- | --- |
| `status = 'disabled'` | `AgentProfileDisabled` | `409` | re-enable it |
| no row / `deleted_at` set / `status = 'deleted'` | `AgentProfileNotFound` | `404` | create one and repoint the route |

`404` joins the existing `route_not_found` / `model_not_found` / `credential_not_found` arm — the
same shape of failure, a reference on the resolution chain that does not resolve, and the caller
names none of them either. `409` is chosen against three alternatives and the reasons are recorded
in `failure_http_status`: `404` would deny a row an operator can see on the admin plane, `503`
would promise that waiting helps, and `502` — where `CredentialDisabled` sits today, via the
wildcard arm — would blame a provider that was never contacted. `CredentialDisabled` was left
alone; it is the same wart on a different resource and not this issue's to move.

The refusal happens at the single resolution site in `execute_inner`, **before** model selection,
credential decryption, the circuit breaker and any attempt row. Both arms inherit it, because they
diverge after it.

### The test that had to change, and it is the one that said so

`documents_current_behaviour_a_dangling_agent_profile_still_serves_the_request` asserted
`succeeded`, no failure, and a wire body with no preamble. It carried its own reversal condition —
*"this case is wrong under the fail-closed answer"* — and it is now replaced by a guard asserting
the opposite. This is the `documents_`/`guards_` split doing exactly what §3.4 asks of it: the four
observability guards next to it did **not** go red, because they were deliberately written to
survive either answer.

`guards_the_streaming_arm_announces_a_dangling_agent_profile_too` also changed, and its comment
records why: it asserted `"stream": true` on the body that reached the provider, as proof it had
not silently taken the non-streaming path. Under fail-closed no body reaches the provider at all,
so that evidence cannot exist. The replacement premise is a control stream that succeeds first —
without it, "no provider request" is also what a broken fixture produces.

### Mutation answers

Each new case was checked against the cheapest edit that would remove the refusal:

* **Return `None` instead of refusing** (restore the fail-open arm) — all four refusal cases red.
* **Re-add `and status = 'active'` to the SQL** — the disabled cases go `404` instead of `409`; the
  `409` case red. This is the mutation the missing/disabled *pair* exists for: with only one of the
  two statuses guarded, moving the filter back into SQL is invisible.
* **Classify `Disabled` as `Active`** — the disabled cases serve the request; red at status.
* **Swap the two classes** — both public cases red, in both directions.
* **Move the refusal after the provider call** — the provider-request counter assertions red.
* **Drop the announcement, keep the refusal** — the four F50 observability guards red.

Every refusal case's "nothing was executed against a provider" assertion is paired with a
*successful* control request on the same fixture that moves the provider's request counter first.
Without it, "zero provider requests" is satisfied by a fixture that could never have made one, and
by a Moira that refuses everything.

### Elsewhere

`ExecutionFailureClass::ALL` is 30. The `expected_disposition` table in `orchestration/controls.rs`
gains both classes as `(false, false, false)` — deployment configuration is identical on the next
attempt, identical at the next provider, and says nothing about anyone's health. The i18n catalog
and its `docs/` mirror gain both codes; the `const` gate would not have compiled otherwise.
`docs/openapi.json` gains two enum values and **no operations**: still 152 / 100 paths / 183
schemas. `docs/release-notes.md` is new — the maintainer asked for the change to be written into
release notes, and there was no such file; it carries the SQL an operator runs *before* upgrading
to find the routes this will start refusing.

No migration. `failure_class` is `varchar(128)` with no check constraint in all four tables that
hold one.
