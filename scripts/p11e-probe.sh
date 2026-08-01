#!/usr/bin/env bash
# One hand-written mutation probe, with the harness verified on every run.
#
# Sub-Phase F's mutation driver reported a false SURVIVED because a probe ran against
# **unmutated** code. Three assertions here make that impossible:
#
#   1. the Python edit asserts the search text occurs exactly once, so a silent no-op edit
#      (the usual cause) aborts instead of producing a verdict;
#   2. `git diff` must be non-empty *and* must contain the expected changed token, checked
#      before the test command runs;
#   3. the restore is verified by content, not by exit status, so the next probe cannot
#      start from a mutated tree.
#
# Usage: p11e-probe.sh <id> <file> <python-repr-old> <python-repr-new> <cargo-test-args...>
set -uo pipefail

ID="$1"; FILE="$2"; OLD="$3"; NEW="$4"; shift 4

export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.cargo-targets/moira-p11e}"
export MOIRA_TEST_DATABASE_URL="${MOIRA_TEST_DATABASE_URL:-postgres://postgres:postgres@127.0.0.1:5432/moira}"

BAK="/tmp/p11e-probe-$ID.bak"
cp "$FILE" "$BAK"

restore() {
    command cp -f "$BAK" "$FILE"
    if ! python3 -c "
import sys
a=open('$FILE','rb').read(); b=open('$BAK','rb').read()
sys.exit(0 if a==b else 1)
"; then
        printf '%s: RESTORE FAILED — tree is dirty, stop and fix by hand\n' "$ID" >&2
        exit 99
    fi
}
trap restore EXIT

# 1. Apply, asserting the edit is real.
if ! python3 - "$FILE" "$OLD" "$NEW" <<'PY'
import sys
path, old, new = sys.argv[1], sys.argv[2], sys.argv[3]
s = open(path).read()
n = s.count(old)
if n != 1:
    print(f"REFUSED: search text occurs {n} times, expected exactly 1", file=sys.stderr)
    sys.exit(1)
open(path, "w").write(s.replace(old, new))
PY
then
    printf '%s: MUTATION NOT APPLIED — no verdict recorded\n' "$ID" >&2
    exit 2
fi

# 2. Prove the working tree really differs before believing any test result.
if git diff --quiet -- "$FILE"; then
    printf '%s: git diff is EMPTY after mutating — refusing to report a verdict\n' "$ID" >&2
    exit 3
fi
printf '── %s  mutation applied:\n' "$ID"
git diff -U0 -- "$FILE" | grep -E '^[+-][^+-]' | head -8 | sed 's/^/     /'

# 3. Run the named tests.
log=$(mktemp)
cargo test "$@" >|"$log" 2>&1
status=$?
if [ "$status" -eq 0 ]; then
    printf '   SURVIVED — no test noticed\n'
else
    printf '   caught by:\n'
    awk '/^failures:$/{f=1;next} f&&/^    /{print "     "$1} /^test result/{f=0}' "$log" | sort -u | head -6
fi
rm -f "$log"
exit "$status"
