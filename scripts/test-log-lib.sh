#!/usr/bin/env bash
# Shared test-log evidence extraction. Sourced by `scripts/gates.sh` (local) and
# `scripts/ci-shard-run.sh` (CI) so the two paths cannot drift.
#
# **Why one file.** The local gate and the CI gate assert the same two properties —
# "every target actually ran" and "no suite skipped" — and before this file they did
# it with two copies of two greps. Two copies of a guard is two chances to loosen one.
#
# Everything here reads a CAPTURED LOG, never cargo's exit status. That is deliberate:
# a redirected `cargo test` capture has been observed to drop whole test binaries while
# cargo still exited 0 (plan 09 wave 2: 34 of 38 targets logged, 779 of 875 tests). The
# log is the thing under suspicion, so the log is the thing that gets asserted against
# an independent source — the filesystem.
#
# Sourcing contract: define nothing, touch no globals other than the `tl_*` namespace.

# ---------------------------------------------------------------------------------
# tl_strip_ansi <in> <out>
#
# THE ANSI TRAP, MEASURED. Against the captured log of run 30889929026:
#
#     grep -c 'Running tests/' 0_rust.txt        →  0
#     grep -c 'Running tests/' (after stripping) → 48
#
# `CARGO_TERM_COLOR: always` is set job-wide and cargo emits the line as
# `\e[1m\e[32m   Running\e[0m tests/x.rs` — the reset lands BETWEEN the two words and
# the leading spaces sit INSIDE an escape. A completeness check ported to CI without
# this step reds every healthy run, and the obvious "fix" — loosening the pattern —
# is precisely how a completeness gate becomes decorative.
#
# We strip rather than set `CARGO_TERM_COLOR=never`, so the Actions UI log stays
# readable and only the machine-read copy is plain.
tl_strip_ansi() {
    sed -e 's/\x1b\[[0-9;]*m//g' -e 's/\x1b\[[0-9;]*[A-Za-z]//g' "$1" > "$2"
}

# ---------------------------------------------------------------------------------
# tl_ran_units <plain-log>
#
# EVIDENCE, NOT INTENT. Emits the unit names the log says actually ran, sorted and
# de-duplicated by the caller as it sees fit. It is extracted from the log and never
# from the packer's output — a partition that claims to cover a target proves nothing
# about whether the target ran.
#
# Four line shapes, all confirmed present in the real log:
#     Running tests/x.rs (target/debug/deps/x-HASH)   → x
#     Running unittests src/lib.rs (…)                → __lib__
#     Running unittests src/main.rs (…)               → __bins__
#     Doc-tests moira                                 → __doc__
# A GitHub job log downloaded with `gh run view --log` prefixes every line with an ISO
# timestamp, a plain `cargo test > file` capture does not. The prefix is stripped
# explicitly — one narrow `sub()` — rather than by loosening the anchors to a bare
# `Running`, because an unanchored pattern would also match the word inside a test's own
# output and quietly inflate the count. This was not theoretical: the first version of
# this function anchored at `^` only and returned ZERO units against the real job log
# while returning the right answer against a local capture.
tl_ran_units() {
    awk '
        {
            line = $0
            sub(/^[0-9][0-9-]*T[0-9:.]+Z[[:space:]]?/, "", line)
        }
        line ~ /^[[:space:]]*Running unittests src\/lib\.rs/  { print "__lib__";  next }
        line ~ /^[[:space:]]*Running unittests src\/main\.rs/ { print "__bins__"; next }
        line ~ /^[[:space:]]*Running tests\// {
            sub(/^[[:space:]]*Running tests\//, "", line)
            sub(/\.rs[[:space:]].*$/, "", line)
            sub(/\.rs$/, "", line)
            print line
            next
        }
        line ~ /^[[:space:]]*Doc-tests / { print "__doc__"; next }
    ' "$1"
}

# ---------------------------------------------------------------------------------
# tl_expected_units <repo-root>
#
# The independent source. `tests/*.rs` is one integration target each; `tests/support/`
# is a directory and is not matched. The three pseudo-units are targets `ls` cannot see
# and that a runner can therefore drop with nothing going red — `__lib__` in particular
# holds `generated_openapi_covers_every_registered_route` and
# `committed_openapi_matches_the_generated_document`, which until now were counted by
# nothing at all.
tl_expected_units() {
    ( cd "$1" && ls tests/*.rs 2>/dev/null | sed 's#^tests/##; s#\.rs$##' )
    printf '__lib__\n__bins__\n__doc__\n'
}

# ---------------------------------------------------------------------------------
# tl_count_skips <plain-log>
#
# A skipped DB suite reports green. That has invalidated a round of results in this
# project before, so the absence of skip lines is asserted rather than assumed.
#
# PATTERN MEASURED against run 30889929026's captured log:
#   `skipping`                       →  0 occurrences on a healthy run, and it catches
#                                       all three real emitters — including
#                                       "skipping Redis-backed test: MOIRA_TEST_REDIS_URL
#                                       is not set" (tests/support/mod.rs), which the
#                                       previous pattern missed entirely.
#   `MOIRA_TEST_DATABASE_URL` (bare) → 19 occurrences, ALL GitHub env dumps. Harmless
#                                       against a cargo-stdout capture, a landmine the
#                                       day anyone points this at a job log. The
#                                       anchored `… is not set` form is the safe one.
#
# `grep -c` exits 1 on zero matches — the GOOD case — hence `|| true`. Getting this
# backwards once made a fully green run look failed.
tl_count_skips() {
    { grep -ci 'skipping\|MOIRA_TEST_DATABASE_URL is not set' "$1" || true; }
}

# ---------------------------------------------------------------------------------
# tl_assert_complete <repo-root> <plain-log> <labels-out>
#
# Diffs expected against ran and appends a failure label per distinct cause to
# <labels-out>. Labels are how `scripts/gates.sh` names which gate broke; collapsing
# them into one boolean is a real regression in the script this project trusts most.
#
# Labels emitted:
#   test:incomplete-log        a target present in the tree never appeared in the log
#   test:missing-lib-target    specifically __lib__/__bins__/__doc__ went missing
#   test:duplicate-target      a target appeared twice (a shard partition overlap)
tl_assert_complete() {
    local root="$1" plain="$2" labels="$3"
    local exp ran ranu rc=0
    exp="$(mktemp)"; ran="$(mktemp)"; ranu="$(mktemp)"

    tl_expected_units "$root" | LC_ALL=C sort > "$exp"
    tl_ran_units "$plain" | LC_ALL=C sort > "$ran"
    LC_ALL=C sort -u "$ran" > "$ranu"

    if ! diff -u "$exp" "$ranu" > /dev/null; then
        printf '   FAILED — the log does not cover the tree. `-` present but never run, `+` ran but absent from tests/:\n'
        diff -u "$exp" "$ranu" | sed 's/^/     /' || true
        # A pseudo-unit going missing has its own label: it is the failure mode a
        # naive runner loses silently, and it should not read as a generic drop.
        if LC_ALL=C comm -23 "$exp" "$ranu" | grep -q '^__'; then
            printf 'test:missing-lib-target\n' >> "$labels"
        fi
        if LC_ALL=C comm -23 "$exp" "$ranu" | grep -qv '^__'; then
            printf 'test:incomplete-log\n' >> "$labels"
        fi
        rc=1
    fi

    if ! diff -q "$ran" "$ranu" > /dev/null; then
        printf '   FAILED — a target ran more than once:\n'
        LC_ALL=C uniq -d "$ran" | sed 's/^/     /'
        printf 'test:duplicate-target\n' >> "$labels"
        rc=1
    fi

    rm -f "$exp" "$ran" "$ranu"
    return "$rc"
}

# ---------------------------------------------------------------------------------
# tl_assert_no_skips <plain-log> <labels-out>
tl_assert_no_skips() {
    local plain="$1" labels="$2" skips
    skips="$(tl_count_skips "$plain")"
    if [ "$skips" -ne 0 ]; then
        printf '   FAILED — %s skip lines: suites did not run\n' "$skips"
        grep -in 'skipping\|MOIRA_TEST_DATABASE_URL is not set' "$plain" | head -20 | sed 's/^/     /'
        printf 'test:skipped-db-suites\n' >> "$labels"
        return 1
    fi
    return 0
}
