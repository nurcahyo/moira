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
expected_targets=$(ls tests/*.rs 2>/dev/null | wc -l | tr -d ' ')
logged_targets=$(grep -c '^ *Running tests/' "$log" || true)
if [ "$logged_targets" -ne "$expected_targets" ]; then
    printf '   FAILED — log holds %s of %s integration targets: the capture is incomplete, so the\n' \
        "$logged_targets" "$expected_targets"
    printf '            count and the skip assertion below are both reading a partial log\n'
    failures+=("test:incomplete-log")
fi

# A skipped DB suite reports green. That has invalidated a round of results in this project before,
# so the absence of skip lines is asserted rather than assumed. `grep -c` exits 1 on zero matches,
# which is the *good* case — hence `|| true`.
skips=$(grep -ci 'skipping database\|set MOIRA_TEST_DATABASE_URL' "$log" || true)
if [ "$skips" -ne 0 ]; then
    printf '   FAILED — %s skip lines: DB-backed suites did not run\n' "$skips"
    failures+=("test:skipped-db-suites")
fi
rm -f "$log"

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
