#!/usr/bin/env bash
# The reporter that lets a REQUIRED check stay green over a legitimately skipped job.
#
#     CHANGES_RESULT=success DOCS_ONLY=true \
#         scripts/ci-required-gate.sh container-and-helm-build=skipped
#
# ── WHY THIS EXISTS AT ALL ───────────────────────────────────────────────────────────
#
# A required status check that reports `skipped` does not report success, and GitHub will
# not merge the pull request. So the obvious implementation of "skip the heavy jobs on a
# docs PR" — an `if:` on `container-and-helm` — does not make docs PRs fast, it makes them
# unmergeable forever. That is strictly worse than the six minutes it was trying to save,
# and the only way out of it is an administrative override of the required checks, which
# is the bypass issue #221 exists to stop anyone normalising.
#
# The shape that works is the one `rust` has used since the shards landed: a cheap job
# that ALWAYS runs, named exactly what branch protection requires, which inspects the
# results of the jobs that did the work. This script is that inspection, extracted so
# there is exactly one copy of it. Two copies of a guard is two chances to loosen one —
# the same reason `scripts/test-log-lib.sh` is sourced by both the local and the CI path.
#
# ── THE THREE-STATE RULE ─────────────────────────────────────────────────────────────
#
#   success                          -> pass. The work ran.
#   skipped, and DOCS_ONLY=true      -> pass. The work was skipped for a reason this
#                                      workflow can name, over a change set every path of
#                                      which `scripts/ci-docs-only.sh` proved inert.
#   anything else                    -> FAIL. failure, cancelled, timed_out, and — the
#                                      one that matters — `skipped` WITHOUT a docs-only
#                                      verdict, which is what a botched `if:` produces.
#
# `if: always()` in the workflow is what makes the job run when a dependency failed;
# without the explicit per-result check below, `success()` would evaluate false and a
# bare `if: always()` job with no assertion would report green over a red dependency.
# Both halves are load-bearing and neither works alone.
#
# ── AND THE CLASSIFIER ITSELF IS CHECKED ─────────────────────────────────────────────
#
# CHANGES_RESULT is the result of the `changes` job. If the classifier did not run, or
# ran and failed, then DOCS_ONLY carries no authority and every gate here fails closed.
# Without this, a crashed classifier would leave DOCS_ONLY empty, every heavy job would
# be skipped by its `if:`, and every gate would be asked to bless a skip it could not
# account for. It would still fail on the three-state rule above — but it would fail
# saying "unexplained skip" rather than "the classifier is broken", and this repository
# has paid for misattributed red checks before.

set -uo pipefail
export LC_ALL=C

fail=0
skipped_for_docs=0
ran=0

CHANGES_RESULT="${CHANGES_RESULT:-}"
DOCS_ONLY="${DOCS_ONLY:-}"

printf 'docs-only verdict: %s (changes job: %s)\n\n' \
    "${DOCS_ONLY:-<unset>}" "${CHANGES_RESULT:-<unset>}"

if [ "$CHANGES_RESULT" != "success" ]; then
    printf 'FAILED — the docs-only classifier (`changes`) reported %s, not success.\n' \
        "${CHANGES_RESULT:-<unset>}"
    printf '         No skip can be justified without it. Fix that job first.\n'
    exit 1
fi

if [ "$#" -eq 0 ]; then
    printf 'FAILED — no `job=result` pairs were passed. A gate with nothing to check is\n'
    printf '         a green tick that means nothing.\n'
    exit 1
fi

for pair in "$@"; do
    name=${pair%%=*}
    result=${pair#*=}
    if [ -z "$name" ] || [ "$name" = "$pair" ]; then
        printf 'FAILED — malformed argument %q; expected job=result.\n' "$pair"
        fail=1
        continue
    fi
    case "$result" in
        success)
            printf 'ok       %-28s ran and passed\n' "$name"
            ran=$((ran + 1))
            ;;
        skipped)
            if [ "$DOCS_ONLY" = "true" ]; then
                printf 'ok       %-28s skipped — every changed path is on the inert allowlist\n' "$name"
                skipped_for_docs=$((skipped_for_docs + 1))
            else
                printf 'FAILED   %-28s skipped, but the change set is NOT docs-only.\n' "$name"
                printf '         An unexplained skip is not a pass. See scripts/ci-docs-only.sh.\n'
                fail=1
            fi
            ;;
        *)
            printf 'FAILED   %-28s %s\n' "$name" "$result"
            fail=1
            ;;
    esac
done

printf '\n'
if [ "$fail" -ne 0 ]; then
    printf 'gate: RED\n'
    exit 1
fi

if [ "$ran" -eq 0 ] && [ "$skipped_for_docs" -gt 0 ]; then
    printf 'gate: GREEN by documented skip — %d job(s) did not run because this change\n' \
        "$skipped_for_docs"
    printf '      touches only paths proven inert (see the `changes` job log for the list).\n'
    printf '      Nothing was bypassed: the work these jobs do has no input that moved.\n'
else
    printf 'gate: GREEN — %d job(s) ran and passed, %d documented skip(s)\n' \
        "$ran" "$skipped_for_docs"
fi
exit 0
