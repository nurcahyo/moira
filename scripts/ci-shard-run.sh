#!/usr/bin/env bash
# Run one shard of the Rust test suite and leave behind the evidence the `rust`
# aggregator needs to prove the union covered everything.
#
#   scripts/ci-shard-run.sh <idx> <total>
#
# Writes `shard-out/` (uploaded as an artifact):
#   assigned.txt  what the packer said this shard should run   (INTENT)
#   ran.txt       what the log says this shard actually ran    (EVIDENCE)
#   skips.txt     count of skip lines
#   cost.txt      predicted centiseconds / actual centiseconds (job summary only)
#   cargo.log     raw cargo output, colour intact
#   plain.log     the same, ANSI stripped — the only thing ever parsed
#
# INVOKED AS A SCRIPT FILE, NOT AN INLINE `cargo` LINE. HANDOFF form 9: a PreToolUse
# hook rewrites an outer `cargo` command and replaces its output with a one-line
# summary even when the command redirects. Cargo inside a script file is immune. This
# is not hypothetical — it was hit while verifying the `--doc` behaviour below.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/test-log-lib.sh
. "$ROOT/scripts/test-log-lib.sh"

idx="${1:?usage: ci-shard-run.sh <idx> <total>}"
total="${2:?usage: ci-shard-run.sh <idx> <total>}"

out="$ROOT/shard-out"
rm -rf "$out"
mkdir -p "$out"

# Materialised to a file rather than read through a pipe or a process substitution,
# so a non-zero exit from the packer (index out of range, more shards than units)
# aborts here under `set -e` instead of arriving as an empty list.
"$ROOT/scripts/ci-shard-plan.sh" "$idx" "$total" > "$out/plan.txt"
units=()
while IFS= read -r line; do
    [ -n "$line" ] && units+=("$line")
done < "$out/plan.txt"
if [ "${#units[@]}" -eq 0 ]; then
    # The packer refuses `total > unit count`, so an empty shard means the unit set
    # itself came back empty — a broken checkout, not a light shard. Fail closed.
    printf 'FAILED — shard %s/%s was assigned no units\n' "$idx" "$total" >&2
    exit 1
fi

args=(); want_doc=0
for u in "${units[@]}"; do
  case "$u" in
    __lib__)  args+=(--lib)  ;;
    __bins__) args+=(--bins) ;;
    # `cargo test --test X` does NOT run doctests, and --doc cannot be combined with
    # any other target selector (verified: "can't mix --doc with other target
    # selecting options"). So it is a SEPARATE invocation appended to the same log.
    # Without this pseudo-unit the doctest target is covered by no shard and vanishes
    # with nothing red — the exact silent-target-loss this repo has measured.
    __doc__)  want_doc=1 ;;
    *)        args+=(--test "$u") ;;
  esac
done
printf '%s\n' "${units[@]}" | LC_ALL=C sort > "$out/assigned.txt"

printf '── shard %s/%s — %s units\n' "$idx" "$total" "${#units[@]}"
sed 's/^/     /' "$out/assigned.txt"

started=$(date +%s)
status=0
cd "$ROOT"
# CAPTURE TO A FILE, NEVER PIPE. `cmd | tail` reports tail's status (HANDOFF form 1):
# a build that died pulling base metadata once looked like exit 0 for exactly this.
#
# `--no-fail-fast` is REQUIRED, not a convenience. Without it cargo stops scheduling
# later targets after the first failure, so one real bug would trip pass/fail AND the
# per-shard completeness check AND the union gate — three reds, one cause. With it,
# completeness becomes orthogonal to pass/fail, which is the only way the union gate
# means what it says. (This changes what `test:incomplete-log` signals: it no longer
# doubles as a failure symptom. Old ledger entries about that signal predate the
# change.)
if [ "${#args[@]}" -gt 0 ]; then
  cargo test --no-fail-fast --all-features "${args[@]}" > "$out/cargo.log" 2>&1 || status=$?
else
  : > "$out/cargo.log"
fi
if [ "$want_doc" -eq 1 ]; then
  cargo test --no-fail-fast --all-features --doc >> "$out/cargo.log" 2>&1 || status=$?
fi
elapsed=$(( $(date +%s) - started ))

tl_strip_ansi "$out/cargo.log" "$out/plain.log"
tl_ran_units "$out/plain.log" | LC_ALL=C sort > "$out/ran.txt"
tl_count_skips "$out/plain.log" > "$out/skips.txt"

predicted="$("$ROOT/scripts/ci-shard-plan.sh" "$idx" "$total" --cost)"
printf '%s\t%s\n' "$predicted" "$((elapsed * 100))" > "$out/cost.txt"

if [ "$status" -ne 0 ]; then
    printf '   FAILED — cargo exited %s\n' "$status"
    tail -80 "$out/plain.log"
fi

# Belt-and-braces ahead of the union gate: this shard ran what it was told to. The
# union gate catches a target no shard was assigned; this catches a target this shard
# was assigned and then did not run.
if ! diff -u "$out/assigned.txt" "$out/ran.txt"; then
    printf '   FAILED — shard-incomplete: assigned and ran disagree\n'
    status=1
fi

printf '   %s units, %s skip lines, %ss wall (predicted %s.%02ss of execution)\n' \
    "${#units[@]}" "$(cat "$out/skips.txt")" "$elapsed" "$((predicted / 100))" "$((predicted % 100))"

exit "$status"
