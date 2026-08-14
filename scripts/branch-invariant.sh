#!/usr/bin/env bash
# The `main ⊆ develop` guard — CONVENTIONS §1A, "Recommended: a CI guard".
#
#     main ⊆ develop        everything released is present in the integration branch
#
# `develop ⊆ main` is NOT an invariant and is never checked here. `develop` being ahead
# of `main` is the normal, healthy state of an integration branch; reporting it would
# turn this guard into the permanent alarm §1A exists to prevent.
#
# WHAT IS ACTUALLY AT RISK. If `main` holds content `develop` lacks — a hotfix — the
# next release cut from `develop` carries a tree that never had the fix and silently
# reverts it. No test fails and no conflict is raised. That is the only outcome worth
# going red for, and it is the only one this script exits non-zero on.
#
# THE FOUR STATES, and why "main is not an ancestor of develop" is not by itself a
# problem. Ancestry breaks the instant a promotion merge commit lands on `main`, which
# happens on every release, so a guard keyed on ancestry alone would be red after every
# promotion and would be ignored within a month.
#
#   1.  ancestor holds                       -> ok    exit 0, silent
#   2.  broken, trees byte-identical         -> warn  exit 0, repair 4a (fast-forward)
#   3.  broken, main introduced nothing      -> warn  exit 0, repair 4b (merge commit)
#   3b. broken, but develop already has it   -> warn  exit 0, repair 4b (merge commit)
#   4.  broken, content stranded on main     -> FAIL  exit 1, repair 4b (merge commit)
#
# State 3b exists because "main changed a file since the merge base" does not imply the
# change is missing from `develop`: a hotfix is routinely cherry-picked onto `develop`
# while the branches stay unmerged. Going red there would accuse the repository of a
# silent revert that cannot happen. The test is whether merging `main` into `develop`
# would move develop's tree at all — see the block itself.
#
# WHY STATE 3 IS A WARNING AND NOT A FAILURE — this is the whole design, do not
# "simplify" it away. The obvious test for state 4 is `git diff main develop`, and it is
# wrong: that diff is non-empty whenever `develop` has merely moved on after a
# promotion, which is the ordinary state of the repository a few hours after every
# release (§1A, row 3 of the table: "Normal: develop is ahead of the release. Action:
# nothing."). Keying the failure on it would fire on the normal state and the guard
# would be muted. The question "is real content stranded on `main`?" is answered by
# comparing `main` against the MERGE BASE — what `main` introduced since the two
# branches parted — which is exactly what §1A specifies.
#
# State 2 is checked before state 3 because byte-identical trees prove nothing can be
# reverted regardless of what the merge-base diff says: `develop`'s tree already
# contains everything `main`'s tree does. It differs from state 3 only in which repair
# is mechanically possible.
#
# Usage:
#   scripts/branch-invariant.sh
#
# Environment:
#   GUARD_MAIN_REF      ref for the release branch      (default: origin/main)
#   GUARD_DEVELOP_REF   ref for the integration branch  (default: origin/develop)
#   GUARD_FETCH         1 to fetch first, 0 to skip     (default: 1)
#
# The three variables exist so `scripts/branch-invariant-test.sh` can drive this script
# against throwaway refs in a scratch repository and observe all four states for real,
# rather than asserting that the logic reads correctly. A guard that has never been seen
# to fail is not a guard.
#
# Exit codes:
#   0  invariant holds, or is broken in a way that strands no content (ok / warn)
#   1  content is stranded on the release branch (fail)
#   2  the guard itself could not run — a ref is missing, git failed
#
# Costs seconds: no cargo, no containers, no network beyond one shallow-ish ref fetch.

set -uo pipefail
export LC_ALL=C

MAIN_REF="${GUARD_MAIN_REF:-origin/main}"
DEV_REF="${GUARD_DEVELOP_REF:-origin/develop}"
GUARD_FETCH="${GUARD_FETCH:-1}"

# `::error::`/`::warning::` are GitHub Actions annotations; outside Actions they are
# noise, so degrade to plain prefixes. Same text either way.
in_actions() { [ "${GITHUB_ACTIONS:-}" = "true" ]; }
err()  { if in_actions; then echo "::error::$*"; else echo "ERROR: $*"; fi; }
warn() { if in_actions; then echo "::warning::$*"; else echo "WARNING: $*"; fi; }

# Mirrored into the job summary when running in Actions so the verdict survives past
# log retention and is readable without expanding the step.
summary() {
  echo "$*"
  if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then echo "$*" >>"$GITHUB_STEP_SUMMARY"; fi
}

if [ "$GUARD_FETCH" = "1" ]; then
  # Explicit refspecs rather than a bare `git fetch origin main develop`: this updates
  # the remote-tracking refs the comparisons below name, without depending on how
  # `remote.origin.fetch` happens to be configured on the runner.
  if ! git fetch --no-tags --quiet origin \
      "+refs/heads/main:refs/remotes/origin/main" \
      "+refs/heads/develop:refs/remotes/origin/develop"; then
    err "branch-invariant: could not fetch main and develop from origin."
    echo "verdict=error"
    exit 2
  fi
fi

for ref in "$MAIN_REF" "$DEV_REF"; do
  if ! git rev-parse --verify --quiet "${ref}^{commit}" >/dev/null; then
    err "branch-invariant: ref '${ref}' does not resolve. The guard cannot run."
    echo "Checkout needs full history — actions/checkout with fetch-depth: 0."
    echo "verdict=error"
    exit 2
  fi
done

main_sha="$(git rev-parse --short "$MAIN_REF")"
dev_sha="$(git rev-parse --short "$DEV_REF")"

# ── State 1: the invariant holds ────────────────────────────────────────────────────
if git merge-base --is-ancestor "$MAIN_REF" "$DEV_REF"; then
  echo "ok: main ⊆ develop  (${MAIN_REF} ${main_sha} is an ancestor of ${DEV_REF} ${dev_sha})"
  echo "verdict=ok"
  exit 0
fi

base="$(git merge-base "$MAIN_REF" "$DEV_REF")" || {
  err "branch-invariant: ${MAIN_REF} and ${DEV_REF} share no merge base."
  echo "verdict=error"
  exit 2
}

# ── State 2: ancestry broken, but the two trees are byte-identical ──────────────────
# `develop`'s tree already contains everything `main`'s does, so nothing can be
# reverted. This is the fresh-promotion artifact: the merge commit on `main` has the
# same tree as the `develop` head it promoted.
if git diff --quiet "$MAIN_REF" "$DEV_REF"; then
  warn "main is not an ancestor of develop, but the two trees are identical — nothing is at risk."
  summary "branch-invariant: WARN — left-over promotion merge commit (${MAIN_REF} ${main_sha}, ${DEV_REF} ${dev_sha})."
  summary "No content is stranded; the branches differ only by the merge commit itself."
  summary "Repair — CONVENTIONS §1A step 4a, fast-forward develop onto main:"
  summary "    git push origin origin/main:develop"
  summary "If that push is refused, fall through to step 4b (reverse sync by merge commit, never squash)."
  echo "verdict=warn repair=4a"
  exit 0
fi

# ── State 3: ancestry broken, but `main` introduced nothing since the merge base ────
# The trees differ only because `develop` moved on after the promotion. §1A row 3:
# normal, nothing to do. Warn rather than fail — failing here is the mistake that
# would make this guard fire on the repository's ordinary steady state.
if git diff --quiet "$base" "$MAIN_REF"; then
  warn "main is not an ancestor of develop; develop has simply moved on. No content is stranded."
  summary "branch-invariant: WARN — develop is ahead of main (${MAIN_REF} ${main_sha}, ${DEV_REF} ${dev_sha})."
  summary "main introduced nothing since the merge base, so no release will revert anything."
  summary "This is the ordinary state after a promotion. Ancestry is still broken, so the"
  summary "repair is worth doing when convenient — CONVENTIONS §1A step 4b (merge commit, never squash):"
  summary "    git switch -c sync/main-into-develop origin/develop && git merge --no-ff origin/main"
  echo "verdict=warn repair=4b"
  exit 0
fi

# ── State 3b: main introduced content, but develop already has its EFFECT ───────────
# `main` changed something since the merge base, so the state-3 test above did not fire.
# That is still not sufficient to go red: the hotfix may already be present on `develop`
# by another route — a cherry-pick, or an equivalent fix committed directly there. The
# question this guard actually has to answer is not "did main change anything?" but
# "would a release cut from develop revert anything?", and that is answered by merging
# main into develop and asking whether develop's tree would move at all.
#
# `git merge-tree --write-tree` computes that merge in memory, touching no worktree and
# no index. If the resulting tree is develop's existing tree, main contributes nothing
# and nothing can be reverted. This is sharper than comparing file NAMES: develop having
# cherry-picked the hotfix and then appended to the same file still resolves to develop's
# tree, where a `diff --name-only` intersection would call that stranded content.
#
# Where it cannot decide, it fails safe. If a later edit on develop rewrites the
# hotfix's own lines, the in-memory merge conflicts, this test does not fire, and the
# guard goes red — correctly enough, since a conflicting reverse sync needs a human
# regardless, and the summary says the conflict is why.
# Exit status distinguishes the three outcomes: 0 clean merge, 1 conflict, anything
# else (128) means this git predates `--write-tree` and the probe itself is unusable.
merged_tree="$(git merge-tree --write-tree "$DEV_REF" "$MAIN_REF" 2>/dev/null)"
mt_status=$?
case "$mt_status" in
  0) : ;;                              # clean merge; $merged_tree is the result
  1) merged_tree=""; mt_note="conflict" ;;
  *) merged_tree=""; mt_note="unsupported" ;;
esac
mt_note="${mt_note:-clean}"

if [ -n "$merged_tree" ] && [ "$merged_tree" = "$(git rev-parse "${DEV_REF}^{tree}")" ]; then
  warn "main is not an ancestor of develop, but develop already contains main's changes. No content is stranded."
  summary "branch-invariant: WARN — main's content is already on develop (${MAIN_REF} ${main_sha}, ${DEV_REF} ${dev_sha})."
  summary "main introduced content since the merge base (${base:0:7}), but merging main into develop"
  summary "would not change develop's tree — the fix is already there by cherry-pick or an equivalent"
  summary "commit. A release cut from develop reverts nothing, so this is not a failure."
  summary "Ancestry is still broken, so the repair is worth doing when convenient —"
  summary "CONVENTIONS §1A step 4b (merge commit, never squash):"
  summary "    git switch -c sync/main-into-develop origin/develop && git merge --no-ff origin/main"
  echo "verdict=warn repair=4b"
  exit 0
fi

# ── State 4: content is stranded on `main` — the hotfix case ────────────────────────
err "main holds content develop lacks. A release cut from develop would silently revert it."
summary "branch-invariant: FAIL — content stranded on main (${MAIN_REF} ${main_sha}, ${DEV_REF} ${dev_sha})."
summary ""
summary "Merging main into develop WOULD change develop's tree, so develop is genuinely missing"
summary "content that main has. Nothing will fail on its own: the next promotion carries a tree"
summary "that never had this change, and it disappears from production silently."
summary ""
if [ "$mt_note" = "conflict" ]; then
  summary "(main and develop also conflict textually — the merge below needs manual resolution.)"
  summary ""
elif [ "$mt_note" = "unsupported" ]; then
  summary "(This git has no 'merge-tree --write-tree', so the guard could not check whether"
  summary " develop already contains main's changes. Treating any content on main as stranded,"
  summary " which is the safe direction but may be a false alarm. Upgrade git to 2.38+.)"
  summary ""
fi
summary "Commits on main that develop lacks:"
git --no-pager log --oneline "${DEV_REF}..${MAIN_REF}" | while IFS= read -r line; do
  summary "    ${line}"
done
summary ""
if [ -n "$merged_tree" ]; then
  # Exactly what develop would gain by the repair — narrower and more truthful than
  # "files main changed since the merge base", which also lists files develop already
  # matched.
  summary "Files develop is missing main's version of:"
  git --no-pager diff --name-only "$DEV_REF" "$merged_tree" | while IFS= read -r line; do
    summary "    ${line}"
  done
else
  summary "Files main changed since the merge base:"
  git --no-pager diff --name-only "$base" "$MAIN_REF" | while IFS= read -r line; do
    summary "    ${line}"
  done
fi
summary ""
summary "Repair — CONVENTIONS §1A step 4b, reverse sync by MERGE COMMIT (never squash, never rebase):"
summary "    git switch -c sync/main-into-develop origin/develop"
summary "    git merge --no-ff origin/main"
summary "    git push -u origin sync/main-into-develop"
summary "    gh pr create --base develop --head sync/main-into-develop \\"
summary "      --title 'sync: main into develop' --body 'Restores main ⊆ develop per CONVENTIONS.md §1A.'"
summary "    gh pr merge <N> --merge"
summary ""
summary "Resolve conflicts toward KEEPING BOTH changes — the hotfix's effect and develop's newer work."
echo "verdict=fail repair=4b"
exit 1
