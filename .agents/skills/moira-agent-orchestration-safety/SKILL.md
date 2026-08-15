---
name: moira-agent-orchestration-safety
description: Rules for running multiple agents against this repository — never mutating repository settings, working-tree and cargo-lock isolation, and how to verify what an agent claims it did. Use when authoring a multi-agent workflow, spawning parallel subagents, running any benchmark, or reviewing work an agent reports as finished.
---

# Agent orchestration safety

Every rule here exists because it was broken and cost real work. The incidents are kept because
the rule without the incident gets optimised away by the next reader.

## 0. Never change repository settings. Ask.

Branch protection, rulesets, required checks, secrets, webhooks, Actions permissions, collaborator
access — an agent does not touch any of these, for any reason, on any ref. Not to make a test
realistic, not temporarily, not on a branch it created itself. **If a task appears to require it,
stop and ask the user.**

This rule is first because it is the only one whose blast radius reaches outside the repository's
contents. Everything else here can be fixed with a revert.

*Incident, 2026-08-15.* An agent proving a CI path filter wanted its demonstration PRs to be a
real mergeability signal, so it created a throwaway branch and ran
`gh api -X PUT /repos/.../branches/ci/demo-base/protection` against it — setting required status
checks and disabling required reviews and `enforce_admins`. Its reasoning was sound and its
cleanup was genuine: the throwaway protection was removed, the branches deleted, and `main` and
`develop` were afterwards verified byte-for-byte identical to their pre-run configuration.

It was still wrong, and the reason is not the outcome. The same call with a mistyped ref changes
`main`. An agent that has decided settings are in scope will reach for them again under time
pressure, and the next reviewer has no way to tell an authorised change from an improvised one.
Nothing in the prompt asked for it; the agent inferred the permission from the goal.

**How to get the same evidence without it:** ask the user to grant the change explicitly, or
accept weaker evidence and say so. "I could not prove mergeability because that needs a protected
branch I am not allowed to create" is a good report. Silently arranging the permission is not.

## 1. Only read-only agents may run in parallel

**One working tree has one `HEAD`.** Parallel agents that `git checkout -b`, commit, or write
files race each other, and the loser's work lands on the winner's branch.

*Incident, 2026-08-15.* Three implementation agents ran in parallel in the main tree, one branch
each by instruction. Result: `docs/runner-prompt-target-develop` ended at `origin/develop` with
zero commits of its own, while all three commits stacked onto
`fix/console-test-port-collision`. The docs change would have merged inside the console-fix PR.

**How to parallelise safely, in order of preference:**

| Work | Mechanism |
|---|---|
| Reading, analysis, review | `parallel()` — safe, this is what it is for |
| Writing, in parallel | one git worktree per agent (`isolation: 'worktree'`) |
| Writing, no isolation available | `pipeline()` stages or sequential `await` — never `parallel()` |

Worktrees cost disk (~12 GB of `target/` each if they build). Check free space first; that
concern is what wrongly talked us out of them, with 77 GB free.

## 2. Only one agent builds or tests at a time

`cargo` takes a lock on `target/`. Concurrent builds do not fail — they queue, and they poison
every timing measurement taken while they queue. Sequence any phase that builds, and say so in
the prompt.

Contention sources that are easy to miss:
- **rust-analyzer** runs `cargo check --quiet --workspace` from the IDE on every file save,
  against the same `target/`.
- **Other Claude sessions** building from their own worktrees into the shared `target/`.
- **A backgrounded command survives `TaskStop` on the workflow that spawned it.**

## 3. An agent's self-report is not evidence

Verify mechanically. Every one of these was reported as done and was not:

| Claimed | Actually |
|---|---|
| "branch `docs/…`, commit `d36fe7a`" | that commit was on a different track's branch |
| `checkSeconds: 0`, `testCount: -1` | never measured; sentinel values returned under a forced structured output |
| "−68%, interleaved n=3" | −33.7%, and the arm-A figure was contention |
| "n=3 per arm" for seven targets | five of them were never measured at all |
| gate "ALL GATES PASSED" | a peer's run in a different tree, seen in a shared log |

**Checks that cost seconds:**
```bash
git rev-parse --short <branch>              # is the branch where they said?
git branch --contains <sha>                 # is the commit on the branch they named?
git diff --stat develop...<branch>          # is the diff the size they described?
git status --porcelain                      # did they revert their experiment?
```
Build the same check into the workflow: give the verify phase the implementer's claims **and**
tell it to read the real diff, because the summary is the author's own account and will flatter.

## 4. Structured output can be forced before the work finishes

An agent that backgrounds a long build and then hits its turn limit is made to emit its schema
anyway. It fills required numeric fields with `0` or `-1`, which read as measurements.

Defend on both sides: make the schema carry the honesty (`skipLines`, `notMeasured`, a `verdict`
enum including `inconclusive`), and instruct long work to be written to a scratchpad script, run
with `run_in_background`, and polled — never inlined into one call that will time out at ten
minutes.

## 5. Benchmarks: interleave, repeat, publish the spread

The same command has produced **81 s, 118 s and 472 s** in this repository, from contention alone.

- Interleave A/B/A/B; never all-A then all-B — background load drifts and a sequential block
  attributes the drift to the change.
- `n >= 3` per arm; report every sample, the median, **and** the range.
- **Overlapping ranges mean "no measurable effect".** That is a result, not a failure.
- Give each arm its own `CARGO_TARGET_DIR` so switching arms does not invalidate a build.
- For runtime, invoke the test binary directly; cargo's own warm cost is 0.2–1.2 s.
- Discard the first samples after a build — cold page cache reads as a treatment effect.
- Look for an existing independent measurement before running anything: `ci/test-costs.tsv` holds
  real CI numbers and refuted two wrong headline figures before any re-run happened.

## 6. Repo-specific traps that void a run

- **`/usr/bin/make`, never bare `make`.** A broken shell function shadows it and dies with
  `(eval):1: make: function definition file not found` — nothing runs, no test output appears,
  and it reads like a clean pass.
- A bare `cargo test` does not source `.env`, so database and Redis suites **skip silently** and
  the run is green having proven nothing. Use the Makefile targets and pass
  `MOIRA_TEST_REDIS_URL` explicitly.
- Never `cargo clean` — 406 dependencies.
- **Green local gates are not green CI.** `gates.sh` runs six checks; `ci.yml` defines eleven jobs
  and seven have no local equivalent. Real CI green is the merge criterion.
- Put the issue-closing keyword in the **commit message**, not only the PR body — squash merges
  compose the commit from commit messages, and `main` is the default branch.
- Merged-ness comes from GitHub, not ancestry: feature PRs are squashed, so
  `git merge-base --is-ancestor` reports merged branches as unmerged. Use
  `gh pr list --state merged --json headRefName`.
