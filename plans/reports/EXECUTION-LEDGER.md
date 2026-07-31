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

**Plus the findings sweep — PR #39 MERGED `5206ffd` (2026-07-31), 27 files, +2471/−101.** F20
(single-primary ownership), F13 (duplicate-issuer 409), F21 (replay double-count, closed here and
nobody had noticed), the wave-2 leftovers, and `cargo-mutants` adoption. CI-verified on the exact
merge commit: five jobs, steps executed (`rust` 13, `console` 16, `console-container-and-helm` 14,
`container-and-helm` 13, `supply-chain` 10).

**Migrations: `main` is now at `0019`. Next free is `0020`.** `0016` is a permanent gap.

**Plan 09 is the only plan work left, and BOTH remaining waves are now re-audited:**

| Wave | §0 | Drift | State |
|---|---|---|---|
| 4 — multi-provider | §0.7 + **§0.7.7** (the decision), `plan/09-wave4-multi-provider` | ~70% | decision taken (Option A′, staged 4A/4B); **stage 4A implemented and gated** — see below |
| 5 — invitations + ownership UI | §0.8 on `plan/09-wave5-invitations-ui` | ~83% | audited; recovery **cut** (no backend), sessions **stay cut** |

**Open findings after this merge:** F24, F22, plus the carried-over F14, F17, ~~F6~~ (closed
`f31ff59`), F2,
admin-write/audit non-atomicity, and the leaked `trusted_jwt_issuers` test rows. **F21 is CLOSED.**
**F23 is closed in Moira's layer** and **F25 is CLOSED** — both by wave 4A, below.

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
| **F2** | Pre-auth query-field enumeration | user deferred |
| ~~**F6**~~ | ~~OTel exports every span; `env_filter` is the sole barrier to Rig prompt spans~~ **CLOSED** `f31ff59` — allow-list of Moira-owned targets on the bridge layer, below the `EnvFilter`. The recorded description understated it: Rig's prompt-bearing span is `INFO`, so a bare `info` was already enough | `fix/f6-otel-span-filter` |
| — | Admin write + audit row still non-atomic | unscheduled |
| — | ~986 leaked `trusted_jwt_issuers` rows in the shared test DB | hygiene |

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
  by one line of console code and one test.** F17's blast radius grows from one issuer to N.
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
