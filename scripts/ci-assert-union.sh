#!/usr/bin/env bash
# The completeness gate. Proves that the union of what the shards ACTUALLY RAN equals
# every test target present in the tree — nothing dropped, nothing run twice, nothing
# skipped.
#
#   scripts/ci-assert-union.sh <manifests-dir>
#
# `<manifests-dir>` is where `actions/download-artifact` put `rust-shard-manifest-*`,
# one directory per shard, each holding the `shard-out/` contents.
#
# THE EXPECTED SET COMES FROM THIS JOB'S OWN FRESH CHECKOUT.
# `ls tests/*.rs` plus the three pseudo-units, re-derived every run. Never from
# `ci/test-costs.tsv`, never from the shard scripts, never a hardcoded number.
# Hardcoded counters in this repo have gone stale twice, and HANDOFF §3.4 catalogues
# what happens when a guard is pinned against a hand-written copy of the thing it
# guards: `every_triggered_table_has_a_scope` compared the schema to a retyped list of
# 21 entries where the schema had 24, and passed.
#
# A MISSING MANIFEST FAILS CLOSED. A shard that never started, or was cancelled, or
# whose upload failed, contributes no `ran.txt`, so its targets are absent from the
# union and assertion 2 reds. There is no path where absent evidence reads as green.
#
# Adding `tests/new_thing.rs` next week needs no edit here: it is expected because it
# is on disk, and it is assigned because `ci-shard-plan.sh` reads the same disk.

set -euo pipefail
export LC_ALL=C

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
# shellcheck source=scripts/test-log-lib.sh
. "$ROOT/scripts/test-log-lib.sh"

dir="${1:?usage: ci-assert-union.sh <manifests-dir>}"
[ -d "$dir" ] || { printf 'FAILED — no manifest directory at %s\n' "$dir" >&2; exit 1; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

expected="$work/expected"
actual="$work/actual"
uniq_actual="$work/uniq"

tl_expected_units "$ROOT" | sort > "$expected"

: > "$actual"
manifests=0
while IFS= read -r f; do
    manifests=$((manifests + 1))
    cat "$f" >> "$actual"
done < <(find "$dir" -name ran.txt -type f | sort)
sort -o "$actual" "$actual"
sort -u "$actual" > "$uniq_actual"

printf 'manifests: %s\n' "$manifests"
printf 'expected units: %s\n' "$(wc -l < "$expected" | tr -d ' ')"
printf 'ran units:      %s (%s distinct)\n' \
    "$(wc -l < "$actual" | tr -d ' ')" "$(wc -l < "$uniq_actual" | tr -d ' ')"

rc=0

# ── 1. union-incomplete — THE DISQUALIFYING FAILURE.
# A test target present in the tree that no shard ran. This is the one thing a sharded
# runner can get wrong and still exit 0 everywhere else.
if ! diff -q "$expected" "$uniq_actual" > /dev/null; then
    printf 'FAILED — union-incomplete: the shards did not cover the tree.\n'
    printf '  `-` is present in tests/ but ran nowhere; `+` ran but is not in the tree.\n'
    diff -u "$expected" "$uniq_actual" | sed 's/^/  /' || true
    rc=1
fi

# ── 2. union-duplicate — a target assigned to two shards.
# Not a correctness bug on its own, but it means the packer is non-deterministic
# across runners, and the very next non-determinism is a gap rather than an overlap.
if ! diff -q "$actual" "$uniq_actual" > /dev/null; then
    printf 'FAILED — union-duplicate: a target ran on more than one shard.\n'
    uniq -d "$actual" | sed 's/^/  /'
    rc=1
fi

# ── 3. zero skips across the whole union.
# A skipped DB suite reports green; that has invalidated a round of results here
# before. `CI=true` makes the fixtures panic rather than skip (plans/CONVENTIONS.md
# §3) — this is the second line, not the first.
total_skips=0
while IFS= read -r f; do
    n="$(tr -d ' \n' < "$f")"
    case "$n" in ''|*[!0-9]*) n=0 ;; esac
    total_skips=$((total_skips + n))
done < <(find "$dir" -name skips.txt -type f | sort)
if [ "$total_skips" -ne 0 ]; then
    printf 'FAILED — %s skip lines across the shards: suites did not run.\n' "$total_skips"
    grep -rin 'skipping\|MOIRA_TEST_DATABASE_URL is not set' "$dir" --include=plain.log \
        | head -20 | sed 's/^/  /' || true
    rc=1
fi

# ── 4. Predicted-vs-actual cost table. WARN ONLY, NEVER FAILS.
# `ci/test-costs.tsv` is hand-maintained, which is the exact shape this repo has been
# burned by. It is defensible only because it is quarantined from correctness — so
# failing on drift would drag it straight back onto the correctness path. This table
# exists to make refreshing it copy-paste, and for no other reason.
summary="${GITHUB_STEP_SUMMARY:-/dev/stdout}"
{
    printf '\n### Shard balance (advisory — never fails the run)\n\n'
    printf '| shard | units | predicted | actual |\n|---|---|---|---|\n'
} >> "$summary"
while IFS= read -r f; do
    d="$(dirname "$f")"
    name="$(basename "$d")"
    p="$(cut -f1 "$f")"; a="$(cut -f2 "$f")"
    u=0; [ -f "$d/ran.txt" ] && u="$(wc -l < "$d/ran.txt" | tr -d ' ')"
    printf '| %s | %s | %s.%02ss | %ss |\n' "$name" "$u" "$((p / 100))" "$((p % 100))" "$((a / 100))" >> "$summary"
done < <(find "$dir" -name cost.txt -type f | sort)

if [ "$rc" -eq 0 ]; then
    printf 'UNION COMPLETE — %s targets, 0 duplicates, 0 skips\n' "$(wc -l < "$expected" | tr -d ' ')"
fi
exit "$rc"
