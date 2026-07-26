# Execution ledger

Durable state for the continuous plan-execution loop. **Read this first on every wake** — it, not
recollection, is the source of truth for where the work stands. Update it at the end of every cycle
in which anything changed, and commit it.

Working agreement in force (granted 2026-07-26): full autonomy including merge, self-paced cadence.
Console draft PR **#23 stays HELD**. Security findings escalate to the user immediately.

---

## STANDING RISK — GitHub Actions is failing repo-wide

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

## Cycle log

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
