#!/usr/bin/env bash
# Deterministic partition of every test target across N shards.
#
#   scripts/ci-shard-plan.sh <idx> <total>          → this shard's unit names, one per line
#   scripts/ci-shard-plan.sh <idx> <total> --cost   → this shard's predicted cost, centiseconds
#
# THE UNIT SET COMES FROM THE FILESYSTEM, NOT FROM A LIST.
# `ls tests/*.rs` plus three pseudo-units (`__lib__`, `__bins__`, `__doc__`) that no
# `ls` can see. Add `tests/new_thing.rs` next week and it is partitioned automatically;
# there is no list to forget to edit. `ci/test-costs.tsv` only supplies *weights* —
# a target missing from it still runs, at DEFAULT.
#
# DETERMINISM IS LOAD-BEARING. Five shards run this independently on five machines and
# their five answers must compose into an exact cover. Three things make that true:
#
#   1. `LC_ALL=C`. A locale-dependent sort lets two runners compute two different
#      partitions from byte-identical input — and the union gate would then red with
#      a cause nobody could reproduce locally.
#   2. Integer centiseconds. No float accumulates differently anywhere.
#   3. A total order on the sort key: cost DESCENDING, then name ASCENDING. Cost alone
#      is not a total order — several targets tie — and bin selection uses strict `<`
#      so a load tie always goes to the lowest bin index.
#
# And non-determinism here is not silent: a gap trips `union-incomplete` in
# `scripts/ci-assert-union.sh`, a double-assignment trips `union-duplicate`.
#
# The algorithm is LPT (longest-processing-time-first): sort descending, place each
# unit on the currently lightest shard. Its worst case is 4/3 of optimal, which is far
# inside the noise here; the real floor is the single largest target, which cannot be
# split under one-process-per-binary.

set -euo pipefail
export LC_ALL=C

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
COSTS="$ROOT/ci/test-costs.tsv"

# DELIBERATELY HIGH (25.00s in centiseconds). An unmeasured new target is assumed
# expensive, so it lands on a light shard rather than piling onto the heaviest one.
# Under-estimating an unknown is how a "balanced" partition acquires a straggler.
DEFAULT=2500

idx="${1:-}"
total="${2:-}"
mode="${3:-names}"

case "$idx" in ''|*[!0-9]*) printf 'usage: %s <idx> <total> [--cost]\n' "$0" >&2; exit 2;; esac
case "$total" in ''|*[!0-9]*|0) printf 'usage: %s <idx> <total> [--cost]\n' "$0" >&2; exit 2;; esac

if [ "$idx" -ge "$total" ]; then
    # SHARD_TOTAL in the workflow drifted above the matrix length, or a matrix entry
    # was added without raising SHARD_TOTAL. Fail closed and loudly rather than run
    # an empty shard that reads as green.
    printf 'FAILED — shard index %s is outside 0..%s\n' "$idx" "$((total - 1))" >&2
    exit 2
fi

units="$(mktemp)"
trap 'rm -f "$units"' EXIT

# tests/support/ is a directory, not a target: `tests/*.rs` does not match it.
( cd "$ROOT" && ls tests/*.rs 2>/dev/null | sed 's#^tests/##; s#\.rs$##' ) > "$units"
printf '__lib__\n__bins__\n__doc__\n' >> "$units"

k=$(wc -l < "$units" | tr -d ' ')
if [ "$total" -gt "$k" ]; then
    printf 'FAILED — %s shards for %s units: at least one shard would be empty\n' "$total" "$k" >&2
    exit 2
fi

want_cost=0
[ "$mode" = "--cost" ] && want_cost=1

awk -v idx="$idx" -v total="$total" -v def="$DEFAULT" -v want_cost="$want_cost" '
    # Pass 1: the cost table. Comments and blanks ignored; a row naming a deleted
    # target is simply never looked up.
    NR == FNR {
        if ($0 ~ /^[[:space:]]*#/ || $0 ~ /^[[:space:]]*$/) next
        # Integer centiseconds, rounded half-up. No float survives into the packer.
        cost[$1] = int($2 * 100 + 0.5)
        next
    }
    { unit[++n] = $1; c[$1] = ($1 in cost) ? cost[$1] : def }
    END {
        # Total order: cost DESC, name ASC. Insertion sort over ~51 items.
        for (i = 1; i <= n; i++) {
            v = unit[i]
            for (j = i - 1; j >= 1; j--) {
                w = unit[j]
                if (c[w] > c[v] || (c[w] == c[v] && w < v)) break
                unit[j + 1] = w
            }
            unit[j + 1] = v
        }
        for (b = 0; b < total; b++) load[b] = 0
        for (i = 1; i <= n; i++) {
            best = 0
            # Strict `<`: a tie goes to the LOWEST index, on every machine.
            for (b = 1; b < total; b++) if (load[b] < load[best]) best = b
            load[best] += c[unit[i]]
            if (best == idx) mine[++m] = unit[i]
        }
        if (want_cost) { print load[idx]; exit }
        for (i = 1; i <= m; i++) print mine[i]
    }
' "$COSTS" - < "$units"
