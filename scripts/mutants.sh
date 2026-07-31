#!/usr/bin/env bash
# Mutation-test the code this branch touches. See `docs/mutation-testing.md`.
#
# **Scoped on purpose.** `cargo mutants` with no filter walks every function in `src/`, and
# each surviving mutant costs a full workspace build plus a full test run. On this tree
# that is hours. The adopted scope is the diff against the merge base, which is the code a
# reviewer is being asked to trust — and it is what makes this affordable enough to
# actually run.
#
# Usage:
#   scripts/mutants.sh                 # mutants in the diff vs. origin/main's merge base
#   scripts/mutants.sh --base <ref>    # ... vs. another ref
#   scripts/mutants.sh --list          # just list them, run nothing
#   scripts/mutants.sh -- <extra args> # everything after `--` goes to cargo mutants
#
# Exit status is cargo-mutants': non-zero if any mutant survived, was unviable in a way it
# could not classify, or timed out. Read `mutants.out/outcomes.json` for the detail;
# `mutants.out/` is gitignored.

set -euo pipefail

BASE="origin/main"
LIST=0
EXTRA=()

while [ $# -gt 0 ]; do
    case "$1" in
        --base) BASE="$2"; shift 2 ;;
        --list) LIST=1; shift ;;
        --) shift; EXTRA=("$@"); break ;;
        *) printf 'unknown argument: %s\n' "$1" >&2; exit 2 ;;
    esac
done

# The DB-backed suites are where this technique has found everything it has found. A run
# with them silently skipping would report survivors as "caught by nothing" when the truth
# is "not tested at all", which is the exact false signal `scripts/gates.sh` exists to
# prevent — so refuse rather than warn.
: "${MOIRA_TEST_DATABASE_URL:=postgres://postgres:postgres@127.0.0.1:5432/moira}"
export MOIRA_TEST_DATABASE_URL
if ! psql "$MOIRA_TEST_DATABASE_URL" -c 'select 1' >/dev/null 2>&1; then
    printf 'MOIRA_TEST_DATABASE_URL is not reachable (%s).\n' "$MOIRA_TEST_DATABASE_URL" >&2
    printf 'Mutation testing without the DB suites reports survivors that were never tested.\n' >&2
    exit 1
fi

if ! command -v cargo-mutants >/dev/null 2>&1; then
    printf 'cargo-mutants is not installed. Install it with:\n\n    cargo install cargo-mutants --locked\n' >&2
    exit 1
fi

# The merge base, not the tip: diffing against the tip of `main` would present every commit
# that landed on main since branching as "code this change touched".
if ! merge_base=$(git merge-base HEAD "$BASE" 2>/dev/null); then
    printf 'cannot find a merge base with %s — pass --base <ref>\n' "$BASE" >&2
    exit 1
fi

diff_file=$(mktemp)
trap 'rm -f "$diff_file"' EXIT
git diff "$merge_base" -- 'src/**/*.rs' 'src/*.rs' >"$diff_file"
if [ ! -s "$diff_file" ]; then
    printf 'no changes under src/ against %s — nothing to mutate\n' "$merge_base"
    exit 0
fi

printf '── mutating src/ changes against %s\n' "$merge_base"
git diff --stat "$merge_base" -- 'src/**/*.rs' 'src/*.rs' | tail -1

if [ "$LIST" -eq 1 ]; then
    exec cargo mutants --in-diff "$diff_file" --list "${EXTRA[@]+"${EXTRA[@]}"}"
fi

# `--baseline skip` is NOT used: cargo-mutants runs the unmutated tree first, and that run
# is the only thing standing between "the suite catches this mutant" and "the suite was
# already red". Keeping it costs one test run and buys the whole result its meaning.
exec cargo mutants --in-diff "$diff_file" "${EXTRA[@]+"${EXTRA[@]}"}"
