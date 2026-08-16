# What to work on next

A living queue for whoever — person or agent — picks this repository up next. It answers three
questions in order: **what must I read**, **what is actually open**, and **what will bite me**.

Keep it current. An entry that has shipped is deleted, not ticked; a stale queue is worse than no
queue because it is read as authoritative.

Last reconciled against GitHub: **2026-08-17**, after #300, #301 and #302, with #303 in flight.

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
| Antigravity | in flight, PR #303 | #253 OpenAPI spec import | `src/orchestration/openapi_import.rs`, `src/infra/repositories/agent_platform.rs`, `.github/workflows/ci.yml` |
| Antigravity | claimed, next | #252 Rig tool loop | branch `fix/rig-tool-loop-memory-252`; 16 files including `src/orchestration/skill_tool.rs`, `controls.rs`, `runtime_factory.rs`, `src/application/{context,execution,public}.rs`, `src/domain/{agent_platform,runtime}.rs`, `src/config/settings.rs`, `src/security/ssrf.rs`, `migrations/0031_agent_platform.sql` |

**Antigravity works in the main tree** at the repository root, and has held unpushed commits there.
Do not `git checkout`, `git switch`, `git stash` or `git add -A` in the main tree while that is
true — that is exactly how a session here lost 932 lines of work once. Use a worktree. A docs-only
change needs no `target/`, so its worktree is a source checkout and costs nothing.

Only one agent builds or tests at a time (orchestration skill §2). Before running `make gates`,
check whether another agent is mid-build: the volume has hit 100% twice, and each `target/` reaches
12–26 GB.

## 4. Open work, highest value first

### 4.1 Plan-12 review remainder — 15 medium + 15 low

All ten **high**-severity findings are closed. What is left is filed by area:

| Issue | Area | Overlap with Antigravity's #252 |
|---|---|---|
| #251 | Job dispatcher, provider health read surface | **1 file** — `src/config/settings.rs`. `skill_tool.rs` and `ssrf.rs` appear in the issue only as cited precedent, not as edit targets. |
| #255 | Native chatgpt provider behind the ToS opt-in | **6 files** — `execution.rs`, `public.rs`, `settings.rs`, `domain/runtime.rs`, `controls.rs`, `runtime_factory.rs`. **Do not start while #252 is open.** |
| #256 | Relationship graph, metrics, DeepSeek catalog, nextest | **5 files** — `execution.rs`, `repositories/public.rs`, `controls.rs`, `ssrf.rs`, `migrations/0031`. Wait for #252. |

Those counts come from the file paths each issue names, which is an **upper bound** — an issue
often cites a file as evidence without needing to modify it, which is exactly why #251's apparent
three overlaps are really one. Read the citation's context before trusting the number.

These are umbrella issues: each holds several findings, so an issue staying open after a fix is
normal. Close individual findings by referencing them in the commit message — squash merges compose
the commit from commit messages, so a closing keyword only in the PR body is lost.

Take one area per branch. Do not batch across areas; the review found that mixed branches make the
"which fix did this" question unanswerable at review time.

### 4.2 Flaky test — #236

`a_hundred_concurrent_unknown_key_opens_cost_exactly_one_database_load` reds `rust-shard (2)` on
unrelated commits. A flaky test in a required check trains everyone to re-run CI without reading
it, which is how a real failure gets merged.

Touches `src/domain/message.rs` and `src/security/data_keys.rs` — **no overlap with anything
claimed above**, so it is the safest code task to pick up while #252 is in flight.

### 4.3 Held on a human decision — do not start these

- **#71** — the `/setup` wizard branch. The harness blocked autonomous merge three times. Surface
  it; do not retry.
- **#283, #91, #78** — labelled `[decision]`. They need a product answer, not an implementation.

---

## 5. Housekeeping state, so you do not rediscover it

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
