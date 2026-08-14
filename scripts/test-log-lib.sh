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
# THE WRITTEN FLOOR.
#
# It exists for the same reason `scripts/rotation-gate.sh:41` writes one, and the reason is
# not redundancy: a source-derived count CANNOT notice a whole `mod` line being dropped from
# a group root, because the attribute count and the executed count fall together and agree
# with each other perfectly. Only a number written down before the fall catches that.
#
# It is a floor. It goes UP when the suite grows. It NEVER comes down to accommodate a
# deletion — a deletion is the event this is here to make visible, and editing this line to
# make a red run green is the one change that turns the guard back into decoration.
#
# Measured 2026-08-14 by `/usr/bin/make gates`, twice: **1352** at c79264a, pre-consolidation,
# on the tree at 54 test targets, and **1353** on this branch, which adds exactly one test
# (`every_group_member_is_declared_by_its_root`). The floor below is the second number, not
# the first. Leaving it at 1352 after the suite grew to 1353 would have left one test of
# slack — enough for a later change to delete a test and still pass, which is precisely the
# event this constant exists to make visible, and it would have quietly falsified the
# "set at the exact measured count" claim on `tl_assert_test_count` below.
#
# The `.config/nextest.toml` header carried "622 passed" for months and was wrong by ~730;
# both numbers here were taken from a run, not copied from a comment.
TL_TEST_COUNT_MINIMUM=1353

# ---------------------------------------------------------------------------------
# tl_declared_tests <repo-root>
#
# The independent source for the count, in the shape `scripts/rotation-gate.sh:62` already
# uses: derived from SOURCE, never a constant somebody has to remember to bump.
#
# WHY THIS EXISTS. `tl_assert_complete` above asserts a set of TARGET NAMES, and both sides
# of its diff are re-derived at run time — so consolidating N test binaries into one turns
# nothing red there. It simply shrinks the diff, and every remaining line still matches.
# What it loses is the NAMED tripwire per target: before consolidation, deleting
# `tests/worker_queue.rs` is named by `-worker_queue` in that diff. After, dropping
# `mod worker_queue;` from `tests/workers.rs` leaves the file on disk, removes its tests from
# the build, and `tl_assert_complete` still diffs clean. `scripts/gates.sh` already computed
# the pass count and only printed it; this is what makes it load-bearing.
#
# It counts attributes in the files ON DISK rather than walking the module graph, and that
# choice is the whole point: an orphaned member still contributes its `#[tokio::test]`
# attributes to `declared` while contributing zero to `passed`, so the two disagree and the
# gate reds. A module-graph-derived expectation would drop in lockstep with the orphan.
#
# The glob is quoted because an unquoted `--include=*.rs` is expanded by zsh before grep
# sees it and fails with "no matches found".
tl_declared_tests() {
    (
        cd "$1" || return 1
        { grep -rhE '^[[:space:]]*#\[(tokio::)?test' --include='*.rs' tests src || true; } \
            | wc -l | tr -d ' '
    )
}

# ---------------------------------------------------------------------------------
# tl_assert_test_count <repo-root> <plain-log> <labels-out>
#
# Every target can be present in the log and still have lost the tests inside it. This is
# the assertion for that.
#
# WHAT IT CATCHES, stated precisely, because a guard described loosely gets trusted loosely.
# The floor is set at the exact measured count, so ANY net drop reds it — one test, not just
# a whole suite. Verified by fabricating logs one and ten below the floor; both emit the label.
#
# That claim is only true while the constant tracks the measurement, and it briefly was not:
# this branch grew the suite to 1353 while the floor still read 1352, leaving exactly one test
# of slack. Caught in review, not by the gate — the gate cannot detect its own floor drifting
# below the truth. **When you add or remove a test, move `TL_TEST_COUNT_MINIMUM` in the same
# commit**, or this paragraph becomes a lie the next reader will trust.
#
# WHAT IT DOES NOT CATCH. It is a count, not an identity. Deleting three tests while adding
# three leaves it perfectly green, and it can never name *which* test went missing. That
# residual is the real price of consolidating targets and no edit to this file recovers it:
# the property `tl_assert_complete` can see (a binary ran) is coarser than the property that
# matters (the tests inside it still exist), and 54 named tripwires is a finer instrument
# than one number. `every_group_member_is_declared_by_its_root` in
# `tests/test_database_isolation.rs` buys back the naming for the one accident this layout
# actually invents; nothing buys back the rest.
#
# Two independent comparisons, because they fail on different accidents:
#   passed < declared   an attribute exists on disk that never ran (orphaned module,
#                       a target dropped from the runner, a cfg that stopped matching)
#   passed < MINIMUM    the attribute was deleted too, so `declared` fell with it
#
# Label emitted: test:count-below-floor
tl_assert_test_count() {
    local root="$1" plain="$2" labels="$3" declared passed
    declared="$(tl_declared_tests "$root")"
    passed=$(grep -E '^test result' "$plain" | awk '{p+=$4} END {print p+0}')
    if [ "$passed" -lt "$declared" ] || [ "$passed" -lt "$TL_TEST_COUNT_MINIMUM" ]; then
        printf '   FAILED — %s tests passed; source declares %s, written floor is %s.\n' \
            "$passed" "$declared" "$TL_TEST_COUNT_MINIMUM"
        printf '            Every target can be present in the log and still have lost its tests.\n'
        printf '            Look first for a `mod <member>;` line missing from a group root while\n'
        printf '            `tests/<group>/<member>.rs` is still on disk — cargo never compiles it,\n'
        printf '            so nothing else in this repository goes red.\n'
        printf 'test:count-below-floor\n' >> "$labels"
        return 1
    fi
    printf '   ok — %s passed (source declares %s, floor %s)\n' \
        "$passed" "$declared" "$TL_TEST_COUNT_MINIMUM"
    return 0
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
