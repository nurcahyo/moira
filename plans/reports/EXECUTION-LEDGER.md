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
3. **Merge on green CI.** All three jobs green with steps executed → merge. A job that ran and
   failed is real and blocks; investigate rather than override. The old infrastructure override is
   **void** — CI works.
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
   - **Below 60 GB free:** run `scratchpad/reclaim.sh`, which escalates from the free-to-delete
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

### F2 — unknown query fields are rejected before authentication, and the 400 enumerates field names

Rejection of an unknown query parameter is axum's `QueryRejection`: `400 text/plain`, with no `code`,
no `message_key`, and no `request_id` — `normalize_infrastructure_error` (`src/lib.rs:165`) only
rewrites 413 and 504. Because `Query` is the last extractor and `admin_actor` runs *inside* the
handler, **the rejection precedes authentication**, and axum's message enumerates all 26 `PageQuery`
field names to an anonymous caller.

Low severity — field *names*, no data, no credentials, and the endpoints are already known from the
public OpenAPI document. But it is an unauthenticated response shape that bypasses the error
envelope, and it means **Module 11's DoD item cannot honestly be ticked**.

Not fixed here because the fix lives in `src/lib.rs` and changes an observable wire response, which
plan 06 excludes by premise. `unknown_query_field_rejection_is_plain_text_and_precedes_authentication`
pins the shipped shape and goes red the day someone fixes it.
**Decide:** fold the envelope fix into plan 06b (which already carries wire changes), or accept it.

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

### F6 — OTel exports every recorded span, so `env_filter` is the only thing holding prompts back

Plan 05 wired a ~590-line OTel pipeline that bridges **every recorded span** to OTLP. Rig emits spans
carrying `gen_ai.system_instructions` and related fields. So with `otel_enabled=true`, the log filter
is the sole barrier between prompt content and a remote collector: a bare `debug` or `trace` level —
rather than target-scoped `moira=debug` — would start exporting Rig's spans, prompts included.

**Not currently a leak.** The shipped default is `otel_enabled=false`, and plan 05's live capture
reviewed every exported attribute against a denylist and found nothing (prompt text, credentials,
response bodies — zero hits). This is a *configuration* hazard, not a present defect: it needs an
operator to enable OTel and widen the filter.

Mitigated for agents in `c546f08` — the errors-testing skill now documents target-scoped `moira=debug`
as the way to get spans and keeps bare levels forbidden, so an agent chasing an empty trace stream
does not reach for the dangerous knob. **Worth considering for a later plan:** a filter guard that
refuses to export third-party spans regardless of level, so the safety does not rest on operator
discipline.

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

### F17 — rotating `BETTER_AUTH_SECRET` makes the console publish a JWKS it cannot sign for

**A new hazard created by durable storage; the in-memory path did not have it.**

On rotation, `getJwks` serves the **plaintext `publicKey` column**, so the JWKS document is unchanged
and Moira's cached copy stays valid. Meanwhile `signJWT` fails with `Failed to decrypt private key`
— and it does **not** regenerate the pair. The console therefore advertises keys it can no longer
sign with, and every token it mints is rejected. Silently: the JWKS endpoint looks healthy.

Verified rather than reasoned about, in `console-jwks-stability.test.ts`. Runbook in
`docs/console-storage.md`.

**Why it is worth its own entry:** with the memory adapter, a rotation regenerated the pair and the
next process simply published new keys. Making storage durable — which fixes three other problems —
converts a self-healing restart into a silent, persistent outage. That is the shape of hazard worth
looking for whenever ephemeral state is made persistent.

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
barrier) arriving through a second channel. F6 remains open; this is its sibling.

**Mitigated in plan 11 Wave 2**, and the shape of the mitigation matters: a hard suppression in
`src/config/telemetry.rs` sitting **below** the `EnvFilter`, so it holds however the operator sets
`env_filter` or `RUST_LOG`. Someone who wants `moira=trace` to debug routing must not have to accept
every prompt and every retrieved chunk as the price. `INFO` and above still pass, so upstream
warnings and errors are never hidden — the dropped events are exactly the ones whose *content* is
the payload.

Found by a canary test, not by review.

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

### F14 — memory dedupe silently stops matching after a pepper rotation

`memory_records.content_hash` is written with `IdempotencyHasher::hash`
(`src/application/conversation.rs:580`), which produces `"{pepper_version}:{base64url(hmac(...))}"`.
`IdempotencyHasher::verify` deliberately accepts **only the active pepper**
(`src/security/idempotency.rs:24-40`).

So after a pepper rotation, every stored `content_hash` becomes unmatchable and exact-match memory
dedupe silently stops working — duplicates accumulate with no error and no log line. Not data loss,
and not urgent, but it fails quietly, which is the worst failure mode for a dedupe mechanism.

Found while deciding plan 11's `chunk_hash` question. It is now an explicit Sub-Phase F obligation
in that plan rather than a production surprise, but the *existing* `memory_records` behaviour is
unchanged and still has this property.

**Decide when plan 11 reaches Sub-Phase F:** either re-hash on rotation, accept the duplicate window
and document it, or move `content_hash` to the unkeyed `request_hash` on the same reasoning plan 11
used for `chunk_hash` — peppering exists to protect digests of request bodies that carry provider
API keys, and memory content is not that.

### F13 — a duplicate trusted JWT issuer returns 500, not 409

Every other uniqueness conflict in the tree maps to a 409 — `auth_provider_settings` has an
`is_unique_violation` → `duplicate_auth_provider` mapping. `trusted_jwt_issuers` has none, so a
duplicate falls through `AppError::Sqlx` to **500 `database_error`**.

Consequence, found while building plan 08's console: an orphaned-issuer retry path cannot recover by
catching a 409, because the 409 never comes. Plan 08 worked around it by listing-then-adopting
rather than create-then-recover, which is the right client behaviour regardless — but the server
shape is still wrong. Plan 03 territory; small, clearly correct, not yet scheduled.

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

2. **`tests/http_middleware_contract.rs:469`** — creates a `system_api_keys` row per run and never
   deletes it. Minor; no current assertion depends on the count.

**The general lesson.** `migrated_pool()`-style helpers hand every `#[cfg(test)]` module in `src/` the
*same* database, and `cargo test --workspace` runs binaries concurrently against it. Any test writing
a singleton, a globally-unique slot, or a cluster-wide counter is sharing mutable state with every
other test in the tree. The integration suites avoid this with `support::LifecycleFixture`'s cloned
databases; the lib tests have no equivalent, so they need an explicit advisory lock. **A test that
leaks a row on the panic path is worse than a flaky one** — it converts one bad run into a permanently
red suite.

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
- Prefer `scratchpad/reclaim.sh` over `cargo clean`: `debug/incremental` is ~45% of the tree and
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
| **F13** | Duplicate trusted JWT issuer returns 500, not 409 | plan 03 territory |
| **F2** | Pre-auth query-field enumeration | user deferred |
| **F6** | OTel exports every span; `env_filter` is the sole barrier to Rig prompt spans | unscheduled |
| — | Admin write + audit row still non-atomic | unscheduled |
| — | ~986 leaked `trusted_jwt_issuers` rows in the shared test DB | hygiene |

**Test baseline:** 779 passing on plan 11's branch (744 on `main` after plan 10).

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
