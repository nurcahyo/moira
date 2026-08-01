#!/usr/bin/env bash
# Hand-run mutation probes for plan 11 Sub-Phase F.
#
# Each probe breaks one security-critical property and asserts the suite goes RED. A probe that
# leaves the suite green is a guard that cannot fire — HANDOFF §3.4's six-of-six failure mode.
#
# **The harness fails loudly when a mutation does not apply.** The first version embedded the
# anchors in heredocs, one anchor's escaping was wrong, the assert raised into a driver with no
# `set -e`, and the probe ran against unmutated code and reported `SURVIVED`. Anchors now live
# in `scripts/p11f-mut.py`, which exits 2 on a miss, and this driver treats that as a hard stop.
#
# Runs cargo from inside a script file so the `rtk` PreToolUse hook cannot rewrite it.
set -uo pipefail
cd "$(dirname "$0")/.."
export CARGO_TARGET_DIR="$HOME/.cargo-targets/moira-p11f"
: "${MOIRA_TEST_DATABASE_URL:=postgres://postgres:postgres@127.0.0.1:5432/moira}"
export MOIRA_TEST_DATABASE_URL

SOURCES=(
    src/application/conversation.rs
    src/application/memory_extraction.rs
    src/infra/repositories/conversation.rs
)

restore() { git checkout -- "${SOURCES[@]}"; }
trap restore EXIT

if ! git diff --quiet -- "${SOURCES[@]}"; then
    printf 'refusing to run: the mutated sources have uncommitted changes\n' >&2
    exit 1
fi

survivors=0

# probe <id> <description> <target> [filter]
probe() {
    local id="$1" description="$2" target="$3" filter="${4:-}"
    printf '\n══ %s  %s\n' "$id" "$description"
    if ! python3 scripts/p11f-mut.py "$id"; then
        printf 'HARNESS FAILURE — the mutation was not applied. This probe proves nothing.\n'
        survivors=$((survivors + 1))
        restore
        return
    fi
    # Prove the mutation is really in the tree before believing anything the run says.
    if git diff --quiet -- "${SOURCES[@]}"; then
        printf 'HARNESS FAILURE — no diff after applying %s.\n' "$id"
        survivors=$((survivors + 1))
        restore
        return
    fi
    local out status
    if [ "$target" = "lib" ]; then
        out=$(cargo test --lib --all-features -- $filter 2>&1)
    else
        out=$(cargo test --test "$target" --all-features -- --test-threads=4 $filter 2>&1)
    fi
    status=$?
    if [ "$status" -eq 0 ]; then
        printf 'SURVIVED — the mutation is green. The guard cannot fire.\n'
        survivors=$((survivors + 1))
    else
        printf 'CAUGHT by:\n'
        printf '%s\n' "$out" | grep -E '^test .* FAILED$' | sed 's/^test /  /; s/ \.\.\. FAILED$//' | sort -u
    fi
    printf '%s\n' "$out" | grep -E '^test result' | sed 's/^/  /'
    restore
}

probe M1  "consent ignored — status_for_consent_mode always Active"        lib  memory_extraction
probe M1  "consent ignored — the same mutation, at the e2e layer"          memory_extraction
probe M2  "the conversation policy's consent column is ignored"            lib  memory_extraction
probe M2  "the conversation policy's consent column is ignored — e2e"      memory_extraction
probe M3  "near-duplicate check always says 'not a duplicate'"             memory_extraction
probe M4  "near-duplicate boundary flipped from <= to <"                   lib  memory_extraction
probe M5  "dedupe stops seeing unconfirmed candidate rows"                 memory_extraction
probe M6  "the shared isolation predicate drops application_id"            lib  isolation_predicate
probe M7  "the policy confidence floor is not applied"                     memory_extraction
probe M8  "the transcript becomes a System message"                        lib  memory_extraction
probe M8  "the transcript becomes a System message — e2e"                  memory_extraction
probe M9  "contradictions are never recorded"                              memory_extraction
probe M10 "exact content-address dedupe never matches"                     memory_extraction
probe M11 "secret-shaped extracted content is not refused"                 lib  memory_extraction
# Deliberately NOT the lib target. The cap is enforced in the per-candidate loop, which needs a
# database — pointing this probe at the unit layer is how it was first reported as a survivor.
probe M12 "the per-run candidate cap is effectively removed"               memory_extraction

printf '\n%s survivor(s).\n' "$survivors"
[ "$survivors" -eq 0 ]
