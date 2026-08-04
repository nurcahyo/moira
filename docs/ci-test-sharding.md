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
- **service-aware packing** (shards without Redis). Only 4 of 48 targets need no
  Postgres and one needs Redis; it would save ~4s and put `ci/test-costs.tsv` on the
  correctness path, because a target that starts using Redis would land on a
  Redis-less shard. Service topology is uniform on purpose.

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
