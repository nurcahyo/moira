# What to work on next

A living queue for whoever — person or agent — picks this repository up next. It answers three
questions in order: **what must I read**, **what is actually open**, and **what will bite me**.

Keep it current. An entry that has shipped is deleted, not ticked; a stale queue is worse than no
queue because it is read as authoritative.

Last reconciled against GitHub: **2026-08-16**, after #291, #294, #296 and #297.

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

## 3. Open work, highest value first

### 3.1 Plan-12 review remainder — 15 medium + 15 low

All ten **high**-severity findings are closed. What is left is filed by area:

| Issue | Area |
|---|---|
| #251 | Job dispatcher, provider health read surface |
| #252 | Rig tool loop, `HttpSkillTool` |
| #253 | OpenAPI spec import, `skill_http_executors` |
| #255 | Native chatgpt provider behind the ToS opt-in |
| #256 | Relationship graph, metrics, DeepSeek catalog, nextest |

These are umbrella issues: each holds several findings, so an issue staying open after a fix is
normal. Close individual findings by referencing them in the commit message — squash merges compose
the commit from commit messages, so a closing keyword only in the PR body is lost.

Take one area per branch. Do not batch across areas; the review found that mixed branches make the
"which fix did this" question unanswerable at review time.

### 3.2 Docker layer caching in CI — approved, not started

Approved as a separate PR from the docs-only path filtering, which has already shipped.

Two constraints established before any work begins:

- **Measure the current cache first.** The GitHub Actions cache is a 10 GB LRU pool *per
  repository*, and this repo is already near it. A new cache that evicts the Rust build cache is a
  net loss, and it will look like a win in the PR that adds it.
- **Use a GHCR registry cache, not `type=gha`, and not `mode=min`.** Registry cache does not
  consume the Actions pool. `mode=min` discards intermediate layers, which is most of the benefit
  for a cargo-chef build.

State the measured before/after with `n >= 3` interleaved runs. Benchmarks in this repository have
been wrong by 10x from contention alone — see the orchestration skill §5.

### 3.4 Flaky test — #236

`a_hundred_concurrent_unknown_key_opens_cost_exactly_one_database_load` reds `rust-shard (2)` on
unrelated commits. A flaky test in a required check trains everyone to re-run CI without reading
it, which is how a real failure gets merged.

### 3.5 Held on a human decision — do not start these

- **#71** — the `/setup` wizard branch. The harness blocked autonomous merge three times. Surface
  it; do not retry.
- **#283, #91, #78** — labelled `[decision]`. They need a product answer, not an implementation.

---

## 4. Housekeeping state, so you do not rediscover it

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

## 5. Definition of done, for anything here

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
