# CI test sharding

The Rust half of `ci` runs as five jobs instead of one. This document says how the
partition is derived, why the completeness gate is the load-bearing part, and what a
developer has to do when they add a test target. (The short answer to the last one is:
nothing.)

## The jobs

| job | what it does | gate? |
|---|---|---|
| `rust-lint` | `cargo fmt --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings` | via `rust` |
| `rust-shard (0…4)` | one fifth of the test targets each, own Postgres + Redis per shard | via `rust` |
| `rust-migrations` | the migration contract test against a **dedicated** fresh pgvector instance | via `rust` |
| `rust` | aggregator: asserts the three above succeeded, then asserts the union of what ran covers the tree | **yes — require this one** |

`rust` is the check to require in branch protection. Never require
`rust-shard (0)`…`(4)`: matrix check names embed the shard index and change whenever
`SHARD_TOTAL` is re-tuned, which un-gates the branch without anything going red.

## Adding a test target

Create `tests/new_thing.rs`. That is the whole procedure.

`scripts/ci-shard-plan.sh` derives the unit set from `ls tests/*.rs` at run time, so the
new file is partitioned automatically, and `scripts/ci-assert-union.sh` derives the
expected set the same way in its own fresh checkout, so the new file is automatically
required to have run. There is no list to edit and therefore no list to forget.

Optionally add a row to `ci/test-costs.tsv` afterwards for balance. See below for why
that is optional in the strong sense.

## `ci/test-costs.tsv` is a performance hint and can never affect coverage

This file maps target name → measured seconds. It is hand-maintained, which is exactly
the shape this repository has been burned by: HANDOFF §3.4's
`every_triggered_table_has_a_scope` pinned the schema against a hand-retyped list of 21
entries where the schema had 24, and passed. A guard that pins X against a hand-written
copy of X is not a guard.

So the table is kept **off the correctness path** by construction, in three ways:

1. **Coverage never reads it.** The expected set comes from `ls tests/*.rs` plus three
   pseudo-units, in the aggregator's own checkout, every run.
2. **A missing row still runs.** It gets `DEFAULT_COST`, deliberately set **high**
   (25s), so an unmeasured new target is assumed expensive and lands on a light shard.
   Under-estimating an unknown is how a "balanced" partition acquires a straggler.
3. **A stale row naming a deleted target is ignored.** It is looked up, never iterated.

The consequence: a missing or wrong row costs *balance*, never *coverage*. The
aggregator prints a predicted-vs-actual table in the job summary so refreshing the file
is copy-paste — and that table **warns and never fails**, because failing on drift would
drag the hand-maintained list straight back onto the correctness path.

`conversation_content_persistence` is currently unmeasured on purpose: it was added
after the measurement run and rides `DEFAULT_COST`. That is the quarantine working.

Two rows are knowingly **stale-high** as of 2026-08-14: `secret_leak_snapshots` (52.85s)
and `content_leak_snapshots` (24.64s) were measured with `argon2` and `blake2` compiled
at `opt-level = 0`, which the dev/test profile overrides in `Cargo.toml` have since
changed. Locally those two targets dropped by roughly 6.6x and 4.7x; the CI factor is
unmeasured. The table was deliberately **not** hand-patched with local numbers — a local
Apple M2 with Postgres in Docker Desktop disagrees with `ubuntu-latest` in both
directions, and mixing two provenances in one table is worse than one honestly stale
table. Over-estimating is the safe direction (an over-weighted target is packed first
and its shard finishes early), so this costs balance and nothing else. Refresh the file
from the first post-merge CI run's predicted-vs-actual table.

## The three pseudo-units

`ls tests/*.rs` cannot see three real test targets, and a runner that stops running one
of them loses it with nothing going red:

| unit | cargo flag | why it matters |
|---|---|---|
| `__lib__` | `--lib` | holds `generated_openapi_covers_every_registered_route`, the whole-route-table pin, and `committed_openapi_matches_the_generated_document`. Until this work it was counted by **nothing**, locally or in CI. |
| `__bins__` | `--bins` | `src/main.rs` unit tests |
| `__doc__` | `--doc`, **a separate invocation** | `cargo test --test X` does not run doctests, and `--doc` cannot be combined with any other target selector — `cargo test --doc --test foo` fails with *"can't mix --doc with other target selecting options"*. So `scripts/ci-shard-run.sh` runs it as its own command appended to the same log. |

## The test-phase assertions live in one file

`scripts/test-log-lib.sh` holds the log parsing and both assertions, and is sourced by
**both** `scripts/gates.sh` (local) and `scripts/ci-shard-run.sh` /
`scripts/ci-assert-union.sh` (CI). Two copies of a guard is two chances to loosen one.

Two things in there are measured rather than reasoned:

- **ANSI must be stripped before anything is parsed.** Against the captured log of run
  30889929026, `grep -c 'Running tests/'` returns **0** raw and **48** after stripping:
  `CARGO_TERM_COLOR: always` is set job-wide and cargo emits
  `\e[1m\e[32m   Running\e[0m tests/x.rs`, with the reset *between* the two words and
  the leading spaces *inside* an escape. A completeness check ported to CI without this
  step reds every healthy run, and the tempting "fix" — loosening the pattern — is how
  a completeness gate becomes decorative.
- **The skip pattern is `skipping` plus the anchored `MOIRA_TEST_DATABASE_URL is not
  set`.** On that same healthy log, `skipping` occurs **0** times and catches all three
  real emitters, including `"skipping Redis-backed test: MOIRA_TEST_REDIS_URL is not
  set"`, which the previous pattern missed entirely. The **bare** variable name occurs
  **19** times — all GitHub env dumps — so it is deliberately not used.

  **A pattern is only as good as the text reaching the log, and it was not reaching it**
  (issue #77). `libtest` captures a test's output and prints it only when the test
  *fails*, so a skip announced with `eprintln!` from a test that then reports `ok` never
  appeared in the capture at all. Measured on this branch, before the fix: with the
  no-database opt-out in force, `cargo test --test retention_worker` redirected to a file
  held **zero** occurrences of `skipping` while all eight tests reported `ok`. (That command
  is how it was spelled when the measurement was taken; `retention_worker` became a module of
  the `workers` target in #187, so reproducing it today reads
  `cargo test --test workers retention_worker::`.) Every skip
  line in the tree is now written straight to `std::io::stderr()`
  (`tests/support/mod.rs::announce_skip`), which is below `libtest`'s capture, and the
  same measurement now yields **1**. The suites themselves also refuse to run without a
  database by default, so this assertion is a second line of defence rather than the
  only one.

`gates.sh` gained one thing it did not have: it now asserts `__lib__`, `__bins__` and
`__doc__` ran, and it reports a set *diff* naming the missing target rather than a count
comparison that any 48 `Running` lines would satisfy. It runs the same
`cargo test --workspace --all-features` it always did, at the same speed.

## Reproducing a shard locally

```bash
scripts/ci-shard-plan.sh 3 5              # what shard 3 would run
scripts/ci-shard-plan.sh 3 5 --cost       # its predicted cost, centiseconds
scripts/ci-shard-run.sh  3 5              # actually run it; evidence lands in shard-out/
```

`total` may be any value up to the unit count, which is useful for narrowing:
`scripts/ci-shard-run.sh 20 51` runs exactly one unit. `idx >= total`, or more shards
than units, exits 2 rather than running nothing.

## Why the two cache keys must stay separate

`rust-lint` uses `shared-key: rust-lint`; the shards and `rust-migrations` use
`shared-key: rust-test`. This is not tidiness.

`cargo clippy` runs under `RUSTC_WORKSPACE_WRAPPER` and emits metadata-only artifacts;
`cargo test` needs full codegen. They are mutually unusable — the old single-job log
shows 38.3s of "Checking moira" followed by a from-scratch 94s "Compiling moira" under
the identical feature set.

Worse: **GitHub cache keys are immutable and first-writer-wins.** If the ~69s lint job
shared the shards' key it would win the reservation on nearly every run and publish a
`target/` containing only clippy's `.rmeta`. The shards would log "Unable to reserve
cache", save nothing, and every subsequent run would recompile all 405 dependencies from
cold — silently, permanently, green. You would find out by reading a cache log.

Only shard 0 saves (`save-if: ${{ matrix.shard == 0 }}`); `rust-migrations` is a reader
(`save-if: false`). `Swatinem/rust-cache` prunes workspace artifacts before saving, so
what is stored is the shard-independent dependency graph rather than shard 0's own test
binaries. If a dev-dependency is ever reachable only from a target outside shard 0, the
symptom is `Compiling <crate>` lines naming something other than `moira` in shards 1–4
on a warm run; the fix is to add `cargo build --tests --all-features` to shard 0 or move
`save-if` to a shard that builds the union.

## `SHARD_TOTAL` and the matrix must agree

`SHARD_TOTAL` is a workflow-level `env`; the matrix is a literal `[0, 1, 2, 3, 4]`.
GitHub does not let a matrix read `env`, so they are two numbers in one file, kept a few
dozen lines apart with a comment at each site. Both mismatch directions fail closed:

- matrix shorter than `SHARD_TOTAL` → a bucket runs nowhere → `union-incomplete`
- matrix longer → the extra shard exits 2 on `idx >= total`

## Approaches rejected, with the arithmetic

- **build-once, fan-out the binaries.** 47 test binaries at 80–86 MB is ~3.9 GB;
  `Swatinem/rust-cache` prunes workspace artifacts on save so a shared key cannot carry
  them, and upload+download exceeds the ~50s compile it would replace.
- **`cargo-nextest` as the default runner.** `.config/nextest.toml` records a direct
  in-repo measurement that it is ~28% slower on this suite (2m07 vs 1m39). It stays as
  a diagnostic second runner. **Tripwire for anyone who revisits it:** nextest captures
  passing tests' output by default, and the skip line is printed by a test that then
  *passes* — so a textual skip gate would read an empty log and go green while every DB
  suite skipped, a strictly worse version of the failure the gate exists to catch.
  `success-output = "final"` would be mandatory. nextest also does not run doctests.
- **sccache** — dependencies are already a 100% cache hit; only `moira` recompiles, so
  the hit rate would be ~0.
- **cargo-chef** — a Docker build tool; there is no Docker layer here to cache.
- **`CARGO_INCREMENTAL=1`** — incremental state does not survive between runners, and
  the cache action does not carry it.
- **service-aware packing** (shards without Redis). Only a handful of targets need no
  Postgres and one needs Redis; it would save ~4s and put `ci/test-costs.tsv` on the
  correctness path, because a target that starts using Redis would land on a
  Redis-less shard. Service topology is uniform on purpose. The denominator was 48 and is
  50 as of #187; the numerator was not re-measured there, so it is not restated. The
  direction is not in doubt, though: a merged target needs the union of its members'
  services, so consolidating groups can only shrink the saving this option could offer.

## Test-target consolidation: one group measured, as a proof of concept

The 54 targets under `tests/*.rs` compile to 54 separate binaries, each paying its own
process-start and link cost. Merging related targets into one binary was tried on exactly
one group — the five worker/coordination suites (`admin_idempotency` was left out;
`cluster_admission`, `coordination_default_path`, `retention_worker`,
`worker_leader_election`, `worker_queue` went into a single `tests/workers.rs`, cases split
across `tests/workers/*.rs` modules reached via `#[path]`) — deliberately scoped to one
group rather than done across the board, specifically so the result could be measured
before committing to the rest. It has not been repeated on the other groups.

**Measured**, same 45 tests, build time excluded, n=3 per arm on the same M2, with the test
executable invoked **directly** so that `cargo`'s own per-invocation cost is out of the loop:

| | mean wall time | spread |
|---|---|---|
| 5 separate binaries | 11.67s | ±0.13s |
| 1 merged binary | 7.74s | ±0.08s |

**−3.93s, a 33.7% cut on these five suites — and roughly 1% of a 328–431s full suite.**

An earlier pass reported this as 58.4s → 18.6s, a 68% cut. **That figure was wrong and is
retained here only as a warning.** It failed three independent checks: it contradicted its own
per-suite table (58.4s total against a 38.38s sum, twenty seconds unexplained); it disagreed
with this repo's own CI measurements in `ci/test-costs.tsv`, which sum to 11.39s against the
verified 11.56s and put `cluster_admission` at 2.12s rather than the claimed 20.22s, a factor
of 9.5; and it was taken while two concurrent `make gates` process trees were running on the
same machine. **Timings taken on a contended machine are contention, not signal** — this repo
has produced 81s, 118s and 472s for one identical command purely from background load. Interleave
A/B, repeat at least three times, report the spread, and treat overlapping ranges as "no effect".

The mechanism survived that correction even though the magnitude did not, and it is not what
"fewer binaries link faster" would predict: `cargo test --no-run` link time showed **no measurable
difference** between 54 targets and 50 (signs disagreed across repeats). Nor is it process-start
savings — cargo's warm per-invocation cost measures at 0.2–1.2s. The win is that `FIXTURE_BUDGET`
(the 4-permit semaphore in `tests/support/mod.rs` bounding concurrent database fixtures) is applied
**per process**, so five binaries serialise through five separate budgets while one binary lets all
45 tests overlap within a single one. Full end-to-end suite wall-clock impact was **not resolved**:
runs of the *same* configuration varied by up to ~300s, far larger than the ~4s effect, so no
full-suite number is asserted either way.

**Recommendation: do not consolidate the remaining nine groups on these numbers.** ~1% of a
full suite does not pay for restructuring 44 more files and dismantling nine quarantine
boundaries. The link-time premise that originally justified the plan measured at zero effect,
and the runtime payoff is an order of magnitude smaller than first reported. Reopen the question
only if a cheaper lever measures well and points back here.

**The cheaper lever to measure first is `FIXTURE_BUDGET` headroom.**
If most of the group-level win comes from unused fixture-concurrency slack — which the
mechanism above suggests — then raising `CONCURRENT_FIXTURES` (checked against PostgreSQL's
`max_connections`) could capture a similar win across all 50 remaining targets with no file
moves, no lost per-target tripwires in `tests/test_database_isolation.rs`'s
`SHARED_DATABASE_ALLOWLIST`/`SCANNED_ROOTS` guards, and no rework of the mutation scripts
that name targets by file (`scripts/f32-mutate.sh`, `scripts/verify-mutants.sh`,
`scripts/p11f-mutate.sh`). That measurement has not been done. Do it before consolidating
the remaining nine candidate groups, not after — it may make most of the remaining work
unnecessary, or it may confirm the merge is still worth it on top of a wider budget. Either
way, do not read "merging one group won" as "merge everything": the pending groups were not
all safe by the same reasoning (see the per-file risk notes maintained alongside the
consolidated `tests/workers.rs`), and `tests/security_foundation.rs` and
`tests/test_database_sweep.rs` must stay solo regardless — see their own comments.

## What sharding costs

Billed Rust minutes roughly double (10 → 20): five runners each pay ~46s of fixed
overhead and ~39s of shared lib compile, about six minutes of pure duplication. The
repository is public, so that is $0 today; if it ever goes private, revisit the shard
count at that moment. `concurrency: cancel-in-progress` (excluding `main`) pays some of
it back by killing superseded runs.

It also buys five independent chances at an infrastructure flake instead of one — five
image pulls, five health-check waits, five cache restores, five uploads. `fail-fast:
false` means a flaky shard reds the aggregator without truncating its siblings'
evidence. **That, not runner minutes, is the argument against raising the shard count.**
