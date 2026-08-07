#!/usr/bin/env bash
# The keyring-rotation gate — issue #138, and `docs/decision-encryption-at-rest.md` §12.
#
# ============================================================================================
# Why a gate of its own, when `rust-shard` already runs `--lib`
# ============================================================================================
#
# It does, and that is not the property this file exists to hold. §12 names the largest risk to
# the whole envelope-encryption plan and it is **not a missing test — it is a skipped one**:
# this repository's recorded landmine is that a wrong or absent `MOIRA_TEST_DATABASE_URL` makes
# database-backed tests skip while reporting green, and the rotation tests are precisely the
# ones that would then never run. Nobody discovers a broken master-key rotation at a convenient
# moment. A check never wired to the thing it checks is the `accept_legacy_hashes` incident
# (#125) that motivated the no-feature-flag decision in the first place.
#
# The Rust half of the fix is `test_support::rotation_gate_database`, which returns a database
# or panics — so a rotation test without one **fails**, and there is no early-return form for
# one to be written with. This script is the other half: it asserts that a non-zero number of
# those tests **actually executed**, because a suite that fails when it cannot run is still not
# a suite that ran. Deleting the whole test module would leave `rust-shard` perfectly green.
#
# ============================================================================================
# The count is checked against the SOURCE, not against a number written here
# ============================================================================================
#
# `scripts/test-log-lib.sh` established the rule: assert the log against an INDEPENDENT source
# rather than against a constant somebody has to remember to bump. So the expected count is the
# number of `#[test]` / `#[tokio::test]` attributes in the rotation module, read from the file
# at run time. That catches, in one assertion:
#
#   * a test deleted                      (source count and executed count both drop — but see
#                                          MINIMUM below, which is what catches deletion)
#   * a test renamed out of the filter    (source count stays, executed count drops)
#   * a test marked `#[ignore]`           (executed count drops; `ignored` is also asserted 0)
#   * the module renamed or moved         (the filter matches nothing at all)
#
# MINIMUM exists because the source-derived count alone cannot notice the module being emptied:
# zero attributes and zero executed tests agree perfectly. It is the one number in this file
# that is written down, it is a floor rather than an equality, and it goes UP when the suite
# grows — never down to accommodate a deletion.
MINIMUM=17

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SOURCE="$ROOT/src/security/keyring_admin/tests.rs"
FILTER="security::keyring_admin::"

: "${MOIRA_TEST_DATABASE_URL:=postgres://postgres:postgres@127.0.0.1:5432/moira}"
export MOIRA_TEST_DATABASE_URL

# Unset, never merely ignored. The Rust side already refuses to honour it for this suite, and
# this makes the intent visible in the one place an operator reads when the gate goes red.
unset MOIRA_TEST_ALLOW_NO_DATABASE || true

if [ ! -f "$SOURCE" ]; then
    printf 'FAILED — rotation-gate: %s does not exist. The rotation suite was moved or\n' "$SOURCE" >&2
    printf '         deleted; this gate must be repointed deliberately, not quietly dropped.\n' >&2
    exit 1
fi

expected=$(grep -cE '^[[:space:]]*#\[(tokio::)?test' "$SOURCE" || true)
printf '── keyring-rotation gate\n'
printf '   source declares %s test(s) in %s\n' "$expected" "${SOURCE#"$ROOT"/}"

# CAPTURE TO A FILE, NEVER PIPE — `cmd | grep` reports grep's status, and this project has
# twice judged a pipeline by its last command. The exit status recorded here is cargo's.
log=$(mktemp)
status=0
cd "$ROOT"
cargo test --lib --all-features "$FILTER" > "$log" 2>&1 || status=$?

# ANSI is stripped before anything is parsed: under `CARGO_TERM_COLOR: always` the raw log
# matches `test result` differently from a local redirect. Measured in `scripts/gates.sh`.
plain=$(mktemp)
sed -E 's/\x1b\[[0-9;]*[A-Za-z]//g' "$log" > "$plain"

if [ "$status" -ne 0 ]; then
    printf '   FAILED — cargo exited %s\n' "$status"
    tail -60 "$plain"
    rm -f "$log" "$plain"
    exit 1
fi

# One `test result:` line, from the one target this runs. More than one means the invocation
# grew a second target and every count below is being read off the wrong line.
results=$(grep -c '^test result:' "$plain" || true)
if [ "$results" -ne 1 ]; then
    printf '   FAILED — expected exactly one `test result:` line, found %s\n' "$results"
    cat "$plain"
    rm -f "$log" "$plain"
    exit 1
fi

line=$(grep '^test result:' "$plain")
passed=$(printf '%s' "$line" | sed -E 's/.* ([0-9]+) passed.*/\1/')
failed=$(printf '%s' "$line" | sed -E 's/.* ([0-9]+) failed.*/\1/')
ignored=$(printf '%s' "$line" | sed -E 's/.* ([0-9]+) ignored.*/\1/')
printf '   %s\n' "$line"

rc=0
if [ "$passed" -lt "$MINIMUM" ]; then
    printf '   FAILED — rotation-gate: only %s rotation test(s) executed, floor is %s.\n' \
        "$passed" "$MINIMUM" >&2
    printf '            A green run that executed no rotation tests is the exact failure this\n' >&2
    printf '            gate exists to prevent. See docs/decision-encryption-at-rest.md §12.\n' >&2
    rc=1
fi
if [ "$passed" -ne "$expected" ]; then
    printf '   FAILED — rotation-gate: %s test(s) are declared in the source but %s executed.\n' \
        "$expected" "$passed" >&2
    printf '            A rotation test that the filter %s no longer matches is a test that\n' \
        "$FILTER" >&2
    printf '            never runs, and nothing else in CI would notice.\n' >&2
    rc=1
fi
if [ "$ignored" -ne 0 ]; then
    printf '   FAILED — rotation-gate: %s rotation test(s) are #[ignore]d.\n' "$ignored" >&2
    rc=1
fi
if [ "$failed" -ne 0 ]; then
    printf '   FAILED — rotation-gate: %s rotation test(s) failed.\n' "$failed" >&2
    rc=1
fi

rm -f "$log" "$plain"
if [ "$rc" -eq 0 ]; then
    printf '   ok — %s rotation test(s) executed against a real database\n' "$passed"
fi
exit "$rc"
