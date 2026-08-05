#!/usr/bin/env bash
# The six gates, in one place, with correct exit semantics.
#
# **Why this exists.** Two separate failures in this project came from judging a pipeline by its
# last command. `docker build … | tail` reported `tail`'s status, so a build that died pulling base
# metadata looked like exit 0. And `cargo test … | grep -c skipping` returned 1 for *zero* matches —
# the good outcome — making a fully green run look failed. `set -o pipefail` plus explicit status
# capture removes both.
#
# It also removes variance: "run the gates" now means the same six commands with the same database
# URL for every agent, rather than whatever each one reconstructed from a brief.
#
# Usage:  scripts/gates.sh [--fast]
#   --fast  skips the release build and the supply-chain pair (dev inner loop only, NOT a merge gate)

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# The two test-phase assertions below (completeness, no-skips) live in one file shared
# with CI's `scripts/ci-shard-run.sh` and `scripts/ci-assert-union.sh`, so the local
# gate and the CI gate cannot drift. Two copies of a guard is two chances to loosen one.
# shellcheck source=scripts/test-log-lib.sh
. "$ROOT/scripts/test-log-lib.sh"

: "${MOIRA_TEST_DATABASE_URL:=postgres://postgres:postgres@127.0.0.1:5432/moira}"
export MOIRA_TEST_DATABASE_URL

FAST=0
[ "${1:-}" = "--fast" ] && FAST=1

failures=()
run() {
    local name="$1"; shift
    printf '── %s\n' "$name"
    if "$@"; then
        printf '   ok\n'
    else
        printf '   FAILED\n'
        failures+=("$name")
    fi
}

run "fmt"    cargo fmt --check
run "clippy" cargo clippy --workspace --all-targets --all-features -- -D warnings

# Tests: capture to a file rather than piping, so the exit status is cargo's and the log stays
# available for the skip check below.
log=$(mktemp)
printf '── test\n'
if cargo test --workspace --all-features >"$log" 2>&1; then
    passed=$(grep -E '^test result' "$log" | awk '{p+=$4} END {print p+0}')
    printf '   ok — %s passed\n' "$passed"
else
    printf '   FAILED\n'
    tail -40 "$log"
    failures+=("test")
fi

# **The log itself can lie.** Every assertion below reads `$log` rather than cargo's exit status,
# and a redirected `cargo test` capture has been observed to drop whole test binaries — "Running
# tests/x.rs", its per-test lines and its `test result:` summary, all absent — while cargo still
# exited 0. Measured on plan 09 wave 2: one capture of a 38-target, 875-test tree logged 34 targets
# and 779 tests, and one run of this script logged 861. The run was green either way, but the
# reported count was wrong, and — the part that matters — the skip assertion below would have been
# reading a log with the skipping suite's output missing from it.
#
# So the log's completeness is asserted against the filesystem, which is an independent source:
# every `tests/*.rs` is one integration target (`tests/support/` is a directory and is not matched).
#
# THREE THINGS CHANGED HERE, all strengthening:
#
#   1. The parsing moved into `scripts/test-log-lib.sh`, shared verbatim with the CI shards. The
#      local gate and the CI gate now assert the same property with the same code.
#   2. It is a set diff, not a count. `logged_targets -ne expected_targets` was satisfied by any
#      48 `Running` lines — including 47 real ones plus a duplicate. The diff NAMES the target.
#   3. `__lib__`, `__bins__` and `__doc__` are now asserted. The lib binary holds
#      `generated_openapi_covers_every_registered_route`, the whole-route-table pin and
#      `committed_openapi_matches_the_generated_document`, and until now it was counted by
#      nothing: a runner change that stopped running it would have gone green.
#
# ANSI is stripped first. Locally cargo emits none into a redirect, so this is a no-op — but the
# same function runs in CI under `CARGO_TERM_COLOR: always`, where the raw log matches
# `Running tests/` ZERO times (the reset lands between the two words). Measured, not guessed.
plain=$(mktemp)
labels=$(mktemp)
tl_strip_ansi "$log" "$plain"
tl_assert_complete "$ROOT" "$plain" "$labels" || true
tl_assert_no_skips "$plain" "$labels" || true
while IFS= read -r label; do
    [ -n "$label" ] && failures+=("$label")
done < "$labels"
rm -f "$log" "$plain" "$labels"

if [ "$FAST" -eq 0 ]; then
    run "release"     cargo build --release --locked
    run "deny"        cargo deny check
    run "audit"       cargo audit
else
    printf '── release/deny/audit skipped (--fast) — NOT sufficient for a merge\n'
fi

if [ ${#failures[@]} -eq 0 ]; then
    printf '\nALL GATES PASSED\n'
    exit 0
fi
printf '\nFAILED: %s\n' "${failures[*]}"
exit 1
