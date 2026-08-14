# Skipping the heavy CI jobs on documentation-only changes

A documentation pull request used to wait about six minutes. Measured on develop run
`31832283949`:

| job | seconds |
|---|---|
| `container-and-helm` | 353 |
| `console` | 177 |
| `rust-shard (0)` | 154 |
| `rotation-gate` | 99 |
| `console-container-and-helm` | 92 |
| `rust-migrations` | 78 |
| `rust-lint` | 60 |
| `sast` | 35 |
| `supply-chain` | 19 |
| `secret-scan` | 9 |
| `rust` (aggregator) | 9 |

Almost all of the critical path was `container-and-helm` building a Docker image from a
tree whose only change was a markdown file — a file that `.dockerignore` does not even
copy into the build context.

A documentation-only change now runs `changes` and `secret-scan`, plus six gate jobs
that take seconds each.

## The problem that makes this non-obvious

Seven checks are required on `develop`:

```
rust  secret-scan  sast  supply-chain  container-and-helm  console  rotation-gate
```

**A required status check that reports `skipped` does not report success, and GitHub
will not merge the pull request.** So the obvious implementation — putting
`if: <not docs>` on `container-and-helm` — does not make documentation PRs fast. It
makes them permanently unmergeable, which is strictly worse than the six minutes, and
the only escape from it is the one thing this repository will not do: `--admin`.

## The shape that works

The heavy work moves into a job with a **new** name. The **required** name becomes a
seconds-long job that runs unconditionally and reports on what its dependency did.

This is not a new idea here. `rust` has been exactly that shape since the shards landed,
and `docs/ci-test-sharding.md` already says to require the aggregator and never the jobs
beneath it. The same pattern now covers five more checks:

| required check (branch protection) | job that does the work |
|---|---|
| `rust` | `rust-lint`, `rust-shard`, `rust-migrations` |
| `rotation-gate` | `rotation-gate-run` |
| `supply-chain` | `supply-chain-scan` |
| `sast` | `sast-scan` |
| `container-and-helm` | `container-and-helm-build` |
| `console` | `console-checks` |
| `secret-scan` | itself — never skipped, see below |

**The required-checks list does not change.** Never require a `-run`, `-scan`, `-build`
or `-checks` job: those are the jobs that legitimately skip, and requiring one
reintroduces the deadlock this design exists to avoid.

Two pieces are load-bearing together and neither works alone:

- `if: always()` on the gate, so it runs when its dependency failed. A bare `success()`
  would evaluate false and the check would never report.
- an explicit per-result assertion, because `if: always()` with no assertion reports
  green over a red dependency.

`scripts/ci-required-gate.sh` is that assertion, in one copy for all six gates:

```
success                       -> pass
skipped, and docs_only=true   -> pass, and the log says which paths bought the skip
anything else                 -> FAIL
```

`anything else` includes `skipped` with **no** docs-only verdict — a botched `if:` — and
it includes the case where the `changes` job itself did not succeed, because then the
verdict carries no authority at all.

## The allowlist

`scripts/ci-docs-only.sh` holds it, with the evidence for each entry next to it. It is
an **allowlist of paths proven inert**, never a denylist of paths known heavy. Under an
allowlist a new directory runs the full suite: slow and correct. Under a denylist it is
skipped: fast, wrong, green, and silent.

"Inert" means one mechanical thing: **no job in `ci.yml` can produce a different result
because this file changed.** Not "looks like documentation".

| entry | why it is inert |
|---|---|
| `*.md` (any depth) | Nothing compiles, embeds, imports or parses a `.md` here — no `include_str!` names one, the console imports none, none of the five semgrep rulesets has a markdown analyzer, and the release image copies only the binary and `config/default.toml` out of the builder. |
| `LICENSE`, `LICENSE.*`, `NOTICE`, `COPYING` | Legal text, read by no job. |
| `.agents/**` | Agent instruction tree. Nothing under `.github/`, `scripts/`, `Makefile`, `ci/`, `src/`, `tests/` or `console/` reads it. |
| `.claude/**` | Claude Code harness config. Same check, same result. |
| `skills/**` | The third agent-skill tree. Same check, same result. |
| `plans/**` | Planning documents. Same check, same result. |

### `docs/**` is deliberately **not** on the list

This is the interesting part. `docs/` looks like the obvious entry and it is wrong — two
files in it are live test fixtures, not prose:

- `docs/openapi.json` is read by `tests/openapi_drift.rs`
  (`COMMITTED_OPENAPI_PATH`), `tests/auth_provider_settings.rs`,
  `tests/admin_query_contract.rs`, the committed-document pin in `src/http/mod.rs`, and
  `console/tests/contract/openapi-contract.test.ts`.
- `docs/i18n-response-catalog.json` is read by `src/i18n/catalog/mod.rs`
  (`DOCS_MIRROR_PATH`) and `console/tests/unit/lib/moira-keys.test.ts`.

Allowlisting `docs/**` would let a hand-edit of the committed OpenAPI document — the
exact drift `tests/openapi_drift.rs` exists to catch — merge without that test ever
running, on both the Rust and the console side. The entry is therefore `*.md`, which
reaches every prose file under `docs/` and neither of those two.

`scripts/ci-docs-only-test.sh` pins it by derivation rather than by list: it walks
`git ls-files docs/` and asserts inert ⇔ the name ends in `.md`. A future widening to
`docs/**` turns that case red, and a future non-markdown file added under `docs/` is
covered the day it lands.

### Explicitly not inert

These need no rule — they are simply absent from the allowlist and therefore run
everything. They are named so a reader looking for them finds them ruled out in writing:

```
Dockerfile   console/Dockerfile   charts/**   .github/**   Cargo.toml   Cargo.lock
deny.toml    migrations/**        src/**      tests/**     console/**   scripts/**
config/**    ci/**                deploy/**   docs/*.json  Makefile
docker-compose.yml   .gitleaks.toml   .dockerignore   .env.example
```

`scripts/**` includes this mechanism itself: a change to the filter runs the full suite.

### Renaming a job orphans its build cache

`Swatinem/rust-cache` derives its key from `${{ github.job }}` unless given an explicit
`shared-key`. Renaming `supply-chain` to `supply-chain-scan` therefore pointed it at a
key nothing had ever written, and `cargo install cargo-audit cargo-deny --locked` went
from 19s to 302s — measured on demonstration run `31834960269`. Nothing failed and
nothing warned; the job was simply sixteen times slower.

`supply-chain-scan` now pins `shared-key: supply-chain`, which is the key the job had
before the rename. The other renamed jobs were unaffected and were checked, not assumed:
`rotation-gate-run` already pinned `shared-key: rust-test`, `console-checks` caches
through `setup-bun` on the lockfile hash, and `sast-scan` and `container-and-helm-build`
use no `rust-cache` at all. Their durations across the demonstration runs match the
develop baseline.

**If you rename a job in this file, check what its cache key was derived from.**

## Fail closed, always towards running more

Every uncertainty resolves to `docs_only=false`:

- an unrecognised event, or `workflow_dispatch` (whose whole purpose is to force real CI);
- a pull request whose base is `main`, or a push to `main` — a release's green run is
  evidence about the released tree and is never skipped;
- a brand-new branch or a force push, where GitHub's `before` SHA is all zeros;
- a shallow clone in which one end of the range is missing;
- an empty change set, which is evidence the range was wrong, not evidence of prose;
- any failure of `git diff`.

The workflow conditions are written `!= 'true'` rather than `== 'false'` for the same
reason: if `changes` published no output at all, the empty string is not `'true'` and
the job runs.

A documentation file can never mask a code file. The verdict is an AND over every path
in the change set, not a majority and not a heuristic — one non-inert path is enough.
Renames are diffed with `--no-renames` so both the old and the new name are classified;
`README.md` → `build.rs` runs the full suite.

## Why `secret-scan` is never skipped

It is 9 seconds, and it sets the floor for a documentation run's wall clock. That is the
right thing to spend it on.

Every other job here is a function of the tree, and the docs-only skip is sound exactly
because a markdown file cannot move any input those jobs read. `secret-scan`'s input is
different: it is the commit's own content and the history leading to it, and a
documentation commit changes that by definition. A markdown file is a perfectly good
place to leak a credential — an example `curl` in a README with a real key in the
header, a pasted log in a runbook, a config block in a migration guide. Those are the
ordinary contents of documentation, not hypothetical shapes.

The cost of missing one is also asymmetric in a way no other check here is: this
repository is public, so a credential that reaches a branch is disclosed the moment it
is pushed. A `rotation-gate` regression can be fixed on the next commit; a disclosed key
cannot be un-disclosed by a later CI run.

So this job is what keeps a fast documentation PR from being a cheaper way to get a
secret into a public repository than a code PR.

## What the skip genuinely defers, disclosed

Three of the skipped jobs are not purely functions of the tree — they also read an
external database that moves on its own:

- `supply-chain-scan` — RustSec advisories, via `cargo audit`;
- `container-and-helm-build` — Trivy's vulnerability database;
- `sast-scan` — semgrep's registry rulesets.

Skipping them on a documentation PR defers the "is there a **new** finding against an
unchanged tree" question to the next non-documentation push. The clean fix is a
scheduled run of those three. That is a separate change and is deliberately not smuggled
in here.

## The self-test

`scripts/ci-docs-only-test.sh` drives the classifier and the gate through 49 cases built
from real git objects, and it runs as a **step of the `changes` job on every run of the
workflow** — including the docs-only runs it is gating. It costs about a second. If it
goes red, so does every gate, because `changes` is then not a job whose verdict anyone
may act on.

A path filter fails in the one direction nobody notices: it stops running a job on a
change that needed it, the run goes green in forty seconds, and the missing coverage is
invisible because nothing red ever appeared. Reading an allowlist cannot detect that;
only driving it can. Same argument `scripts/branch-invariant-test.sh` makes for itself.

The three cases that matter most:

- **mixed** — a markdown file and a Rust file in one change set must classify as not
  docs-only. A documentation file masking a code file is what a wrong filter actually
  produces.
- **`docs/*.json`** — derived from the real tree, so it keeps holding as the tree changes.
- **unexplained skip** — a `skipped` dependency with no docs-only verdict must turn the
  gate red. That is the whole difference between this design and an `if:` on a required
  check.

## Locally

```bash
/usr/bin/make docs-only        # is HEAD vs origin/develop a docs-only change?
/usr/bin/make test-docs-only   # drive the filter and the gate through every case
```

<!-- CI demonstration for #230, case 1: a markdown-only change. This branch is throwaway. -->
