# What to work on next

A living queue for whoever — person or agent — picks this repository up next. It answers three
questions in order: **what must I read**, **what is actually open**, and **what will bite me**.

Keep it current. An entry that has shipped is deleted, not ticked; a stale queue is worse than no
queue because it is read as authoritative.

Last reconciled against GitHub: **2026-08-17**, after #300, #301, #302 and #304, with #303 in
flight and #307 newly filed (docs landed, code not started).

Two agents are active. **§3 records who holds what** — read it before claiming anything, and update
it in the same commit as your claim.

---

## 1. Read these first, in this order

Do not skip to the work. Every rule in these files is written down because breaking it cost
something here.

| File | Binds |
|---|---|
| `plans/CONVENTIONS.md` | Branch and PR workflow, required gates, testing, i18n, auth architecture. **§1A** governs every merge between `main` and `develop`, in either direction. |
| `.agents/skills/moira-agent-orchestration-safety/SKILL.md` | Multi-agent runs, benchmarks, and how to verify what an agent claims. **§0 — never change repository settings** — applies before anything else. |
| `plans/RUNNER-PROMPT.md` | The per-plan runner protocol: one plan, one branch, one PR. |
| `CLAUDE.md` | Which specialist skill to load for which change. |
| `docs/project-structure.md` | Where code goes. |

`.claude/skills/*/SKILL.md` are eight-line stubs pointing at `.agents/skills/`. **The `.agents/`
copy is canonical** — it is shared with Codex and Antigravity. Edit there; never edit a stub into
a second source of truth.

## 2. The two rules most often broken by accident

**Merge method.** Feature and plan branches squash into `develop`. A merge *between* `main` and
`develop`, in either direction, uses a merge commit (`gh pr merge <N> --merge`) — never squash,
never rebase. PR #102 squashed one and permanently divorced the two branches' ancestry;
`CONVENTIONS.md` §1A keeps the evidence.

**Real CI green is the merge criterion.** `make gates` runs six checks locally; `ci.yml` defines
more than twenty jobs and several have no local equivalent. A green local gate is a precondition,
not a verdict.

---

## 3. Who is working on what — read before claiming anything

More than one agent works this repository at a time, and they are not always the same tool. This
section is the only thing preventing two of them from editing the same file from different
directions. **Update it when you claim or release work**, in the same commit as the claim.

| Claimed by | State | Issue | Files it owns |
|---|---|---|---|
| unknown | **in flight, uncommitted** | #236 flaky key-open test | main tree, branch `fix/flaky-key-open-236`; `src/security/data_keys.rs` held ~146 uncommitted lines on 2026-08-17 |

**Released since the last revision of this table.** Both entries that stood here are done, and the
blocks they imposed elsewhere in this file are lifted:

- **#252 Rig tool loop — CLOSED**, merged as `253b783` (PR #265). Every "do not start while #252 is
  open" warning below is void.
- **#253 OpenAPI spec import — PR #303 MERGED.** The umbrella issue stays open for its remaining
  findings, but no branch is claiming those files.

**Someone works in the main tree** at the repository root, and has held unpushed commits there.
Do not `git checkout`, `git switch`, `git stash` or `git add -A` in the main tree while that is
true — that is exactly how a session here lost 932 lines of work once. Use a worktree. A docs-only
change needs no `target/`, so its worktree is a source checkout and costs nothing.

Only one agent builds or tests at a time (orchestration skill §2). Before running `make gates`,
check whether another agent is mid-build: the volume has hit 100% twice, and each `target/` reaches
12–26 GB.

## 4. Open work, highest value first

### 4.1 Plan-12 review remainder — 15 medium + 15 low

All ten **high**-severity findings are closed. What is left is filed by area:

| Issue | Area | Former overlap with #252 |
|---|---|---|
| #251 | Job dispatcher, provider health read surface | 1 file — `src/config/settings.rs` |
| #255 | Native chatgpt provider behind the ToS opt-in | 6 files — `execution.rs`, `public.rs`, `settings.rs`, `domain/runtime.rs`, `controls.rs`, `runtime_factory.rs` |
| #256 | Relationship graph, metrics, DeepSeek catalog, nextest | 5 files — `execution.rs`, `repositories/public.rs`, `controls.rs`, `ssrf.rs`, `migrations/0031` |

**All three are now unblocked.** #252 closed as `253b783`, so the "wait for #252" instruction that
stood in this table is void. The overlap column is kept only as a record of which files these
issues touch, which is still worth reading before claiming two of them at once.

Those counts come from the file paths each issue names, which is an **upper bound** — an issue
often cites a file as evidence without needing to modify it, which is exactly why #251's apparent
three overlaps were really one. Read the citation's context before trusting the number.

These are umbrella issues: each holds several findings, so an issue staying open after a fix is
normal. Close individual findings by referencing them in the commit message — squash merges compose
the commit from commit messages, so a closing keyword only in the PR body is lost.

Take one area per branch. Do not batch across areas; the review found that mixed branches make the
"which fix did this" question unanswerable at review time.

### 4.2 Claude subscription boundary — #307

The decision is recorded and the documentation has landed; **the code has not**. `docs/claude-
subscription-boundary.md` is the canonical statement and `docs/decisions-taken.md` §9 is the
decision record. Issue #307 specifies the implementation, which mirrors the existing ChatGPT
opt-in gate rather than inventing a mechanism.

The load-bearing part is not the flag, it is the **composition rule**: a `global`-scoped
subscription-backed credential must be *refused* for a tenant-scoped request, not silently used.
That is reachable today — `PgRuntimeRepository::resolve_runtime_credential` ranks `tenant` above
`global` but does not exclude `global`, and a tenant credential that expired or was revoked drops
out of the candidate set entirely, so the platform's row wins by default. Everything else in #307
is a warning; that one is a refusal.

Touches `src/config/settings.rs` (overlaps #251), plus `src/orchestration/runtime_factory.rs`,
`src/infra/repositories/runtime.rs`, `src/application/{admin/providers,runners}.rs`,
`src/domain/{admin,runners}.rs`, `src/i18n/catalog/errors.rs`. **Unblocked** — the #252 hold that
stood here is lifted.

### 4.3 Flaky test — #236

`a_hundred_concurrent_unknown_key_opens_cost_exactly_one_database_load` reds `rust-shard (2)` on
unrelated commits. A flaky test in a required check trains everyone to re-run CI without reading
it, which is how a real failure gets merged.

Touches `src/domain/message.rs` and `src/security/data_keys.rs`.

**Claimed, and being worked right now.** On 2026-08-17 the main tree sat on `fix/flaky-key-open-236`
with ~146 uncommitted lines in `src/security/data_keys.rs`. This is no longer a free task — take
something else, and leave that tree alone.

### 4.4 Held on a human decision — do not start these

- **#71** — the `/setup` wizard branch. The harness blocked autonomous merge three times. Surface
  it; do not retry.
- **#91, #78** — labelled `[decision]`. They need a product answer, not an implementation.

**#283 is no longer held.** It asked for a decision on per-tenant authorization for
credential-scoped writes; that decision is recorded in
`docs/decision-session-identity-and-conversation-scope.md` and implemented by #322. It is now
blocked on commerce-os publishing a JWKS, which is a dependency rather than an unanswered question.

### 4.5 Session, context, and provider work — from the 2026-08-17 architecture review

Two documents came out of it and are the context for thirteen issues. Read the relevant one before
picking any of them up; each issue is deliberately thin because the reasoning lives in the doc.

- `docs/decision-session-identity-and-conversation-scope.md` — four binding decisions on identity,
  conversation scope, and what Moira stores. **The headline is that Moira does not become the
  system of record for conversation content**, and `metadata_only` costs nothing because prompt
  caching turns out to be a wire concern rather than a storage one.
- `docs/provider-plan-grok-kimi-gemini-flash.md` — xAI, Moonshot, and the current Gemini Flash tier.

**Start with #327.** The pricing table blocks #328, #329, #330 and #332 — four issues behind one —
and without prices no caching claim can be proved either way. Note that the obvious schema does not
work: these vendors' prices are **time-dated** and **context-tiered**, so `effective_from` and a
tier bound have to land in the first version, not a later migration.

| Issue | | Note |
|---|---|---|
| #322 | Register commerce-os as a trusted JWT issuer | Blocked on commerce-os publishing a JWKS. Also closes #283. |
| #323 | Measure JWKS cache behaviour under overlapping rotation | Pure investigation, no dependencies. **Blocks the first key rotation** — measure before, not during. |
| #324 | Enforce the conversation-id contract | `title` and `metadata` are the only remaining path by which personal data reaches Moira, and nothing enforces the caller-side rule today. |
| #325 | Propagate erasure to embeddings | Embeddings are unsealed under **every** persistence policy value. |
| #326 | Retention sweeper honour `retention_expires_at` | Written since `0007`, read by nothing. |
| #327 | Pricing table, dated and tiered | **Start here.** |
| #328 | Map `cache_creation_input_tokens` | Without the write count, a 0.1× read and a 1.25× write are indistinguishable. |
| #329 #330 #331 | Gemini Flash catalog, xAI Grok, Moonshot Kimi | rig-core 0.40 already ships all three clients. **Do not bump rig-core** — its model-id constants are stale but `completion_model` takes any string. |
| #332 | Per-provider cache semantics as a capability | The five providers disagree; a router should absorb that, not each caller. |
| #333 | Update the rig-providers skill | Ships last. |
| #336 | Default new applications to `metadata_only` | Breaking for new applications; existing ones untouched. |

**Three security findings are tracked privately** as GitHub security advisories, not as issues, so
that an unfixed exploit chain is not published on a public repository. Two are live today. They are
now the gate for promoting `develop` to `main` — see §5.

---

## 5. Housekeeping state, so you do not rediscover it

**Promotion to `main`.** The gate is no longer #259. `main` should not carry a known cross-tenant
leak, so the gate is the three security advisories in §4.5. #259 stays open on its own merits.
When you do promote, it is a **merge commit** — never a squash. `CLAUDE.md` says why.

**Branches.** 222 local branches were reduced to 27 on 2026-08-16. Of the 135 that appeared to have
unique commits, **113 were already merged** — squash merges make ancestry lie. Establish merged-ness
with `gh pr list --state merged --json headRefName`, never with `git merge-base --is-ancestor`.

**Worktrees.** Seven remain, of which several hold uncommitted work. Each worktree builds its own
`target/`; two reached 26 GB and 20 GB, and the volume hit 100% twice in one session — killing a
gate mid-run with `No space left on device`. Before creating one, ask whether the agents actually
run concurrently: the isolation rule exists for concurrent writers, and the main tree's warm
`target/` is worth a lot.

`git worktree remove` without `--force` refuses anything with modified or untracked files. That
refusal is the safety check, not an obstacle — do not reach for `--force` to get past it.

A worktree with `develop` checked out **blocks `develop` repo-wide**, including `gh`'s post-merge
cleanup, which then reports a merge as failed when it succeeded.

**Test floor.** `TL_TEST_COUNT_MINIMUM` in `scripts/test-log-lib.sh` is at **1756**, measured on the
merged tree with zero skip lines. It moves in the same commit as any test you add or remove, and it
is a floor — it goes up, never down to accommodate a deletion.

---

## 6. Definition of done, for anything here

1. A test that **fails without the fix.** Revert the change and watch it red. A test that passes
   both ways is the single most common finding in this repository's reviews.
2. `MOIRA_TEST_REDIS_URL="redis://127.0.0.1:6379/0" /usr/bin/make gates` — `ALL GATES PASSED`, and
   **zero skip lines**. A bare `cargo test` does not source `.env` and skips the database suites
   silently, proving nothing while reporting green.
3. Real CI green on the PR, per-job.
4. The closing keyword in the **commit message**.
5. English in the repository — issues, PRs, commits, docs, plans. Reports to the user are a separate
   matter and follow their preference.

Report what actually happened. If a step was skipped, say which. If a number is unmeasured, say it
is unmeasured. Three headline figures in this project were published from measurements taken under
contention, and one was wrong by an order of magnitude.
