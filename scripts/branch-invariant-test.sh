#!/usr/bin/env bash
# Self-test for scripts/branch-invariant.sh.
#
# Builds each state out of REAL git objects in a scratch repository and runs the real
# script against them. It does not re-implement the comparisons and it does not assert
# that the logic reads correctly — a guard that has never been observed to fail is not a
# guard, and this repository has a documented history of exactly that (CONVENTIONS §1A:
# "a gate you did not run is not a gate that passed").
#
# The scratch repo evolves through the states in the order they occur in real life:
# steady state -> promotion -> develop moves on -> hotfix on main.
#
#   scripts/branch-invariant-test.sh          # or: make test-branch-invariant
#
# Exits 0 if every case behaved as specified, 1 otherwise.

set -uo pipefail
export LC_ALL=C

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GUARD="${SCRIPT_DIR}/branch-invariant.sh"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

failures=0
pass() { printf '  PASS  %s\n' "$1"; }
fail() { printf '  FAIL  %s\n' "$1"; failures=$((failures + 1)); }

# Throwaway ref names, deliberately NOT main/develop: the script must be comparing the
# refs it is given, not names it hardcoded.
M=refs/heads/scratch-release
D=refs/heads/scratch-integration

run_guard() {
  ( cd "$WORK/repo" && GUARD_FETCH=0 GUARD_MAIN_REF="$M" GUARD_DEVELOP_REF="$D" \
      GITHUB_ACTIONS='' GITHUB_STEP_SUMMARY='' bash "$GUARD" 2>&1 )
}

# expect <label> <expected-exit> <expected-verdict-substring> [required-output-substring]
expect() {
  local label="$1" want_code="$2" want_verdict="$3" want_text="${4:-}"
  local out code
  out="$(run_guard)"
  code=$?

  local ok=1
  [ "$code" = "$want_code" ] || { ok=0; printf '        exit %s, wanted %s\n' "$code" "$want_code"; }
  grep -qF "$want_verdict" <<<"$out" || { ok=0; printf '        no %q in output\n' "$want_verdict"; }
  if [ -n "$want_text" ] && ! grep -qF "$want_text" <<<"$out"; then
    ok=0; printf '        no %q in output\n' "$want_text"
  fi

  if [ "$ok" = 1 ]; then
    pass "$label  (exit ${code}, ${want_verdict})"
  else
    fail "$label"
    printf '        ---- actual output ----\n'
    sed 's/^/        /' <<<"$out"
    printf '        -----------------------\n'
  fi
}

# ── scratch repository ──────────────────────────────────────────────────────────────
mkdir -p "$WORK/repo"
cd "$WORK/repo" || exit 2
git init --quiet -b scratch-release .
git config user.email test@example.invalid
git config user.name "branch-invariant test"
git config commit.gpgsign false

commit() { # commit <file> <content> <message>
  printf '%s\n' "$2" >"$1"
  git add "$1"
  git commit --quiet -m "$3"
}

echo "branch-invariant.sh — behavioural self-test"
echo

# ── STATE 1: the invariant holds ────────────────────────────────────────────────────
# release = A, integration = A + B. Ancestry holds; the guard must be silent.
commit app.txt v1 "A: shared base"
git branch scratch-integration
git switch --quiet scratch-integration
commit feature.txt f1 "B: feature lands on the integration branch"
git switch --quiet scratch-release

expect "state 1  ancestor holds -> ok, silent" 0 "verdict=ok" "ok: main ⊆ develop"

# ── STATE 2: promotion merge commit, byte-identical trees ───────────────────────────
# The release branch merges the integration branch with --no-ff, exactly as
# `gh pr merge --merge` does. Ancestry breaks immediately; no content is stranded.
git merge --quiet --no-ff -m "release: promote integration to release" scratch-integration

if git merge-base --is-ancestor "$M" "$D"; then
  fail "state 2 setup: ancestry did NOT break — the fixture is wrong"
elif ! git diff --quiet "$M" "$D"; then
  fail "state 2 setup: trees are not identical — the fixture is wrong"
else
  expect "state 2  promotion merge, identical trees -> warn, repair 4a" \
    0 "verdict=warn repair=4a" "git push origin origin/main:develop"
fi

# ── STATE 3: the integration branch moves on ────────────────────────────────────────
# The ordinary steady state a few hours after any release. Trees now differ, but the
# release branch introduced nothing since the merge base. THIS IS THE CASE A NAIVE
# `git diff main develop` GUARD WOULD FAIL ON, and it must not fail here.
git switch --quiet scratch-integration
commit feature2.txt f2 "C: more work lands after the promotion"
git switch --quiet scratch-release

if git diff --quiet "$M" "$D"; then
  fail "state 3 setup: trees are still identical — the fixture is wrong"
else
  expect "state 3  develop merely moved on -> warn, NOT fail" \
    0 "verdict=warn repair=4b" "no release will revert anything"
fi

# ── STATE 4: a hotfix strands content on the release branch ─────────────────────────
# The only case with production consequences, and the only one that goes red.
commit hotfix.txt "the fix" "H: hotfix lands directly on the release branch"

expect "state 4  content stranded on main -> FAIL" \
  1 "verdict=fail repair=4b" "H: hotfix lands directly on the release branch"

# The failure must name the stranded file, not just the commit — whoever reads it
# should not have to re-derive what is at risk.
out="$(run_guard)"
if grep -qF "hotfix.txt" <<<"$out"; then
  pass "state 4  names the stranded file"
else
  fail "state 4  does not name the stranded file"
fi

# ── STATE 3b: the hotfix is cherry-picked onto the integration branch ───────────────
# Ancestry is still broken and the release branch still "introduced content since the
# merge base", so states 1-3 do not fire. But the fix is already on the integration
# branch, so no release can revert it and the guard MUST NOT go red. This is the false
# positive a naive state-4 test produces.
git switch --quiet scratch-integration
git cherry-pick --quiet scratch-release >/dev/null 2>&1 || {
  fail "state 3b setup: cherry-pick failed — the fixture is wrong"
}
git switch --quiet scratch-release

if git merge-base --is-ancestor "$M" "$D"; then
  fail "state 3b setup: ancestry was restored — the fixture is wrong"
else
  expect "state 3b  hotfix already cherry-picked onto develop -> warn, NOT fail" \
    0 "verdict=warn repair=4b" "already contains main's changes"
fi

# ── STATE 3b(ii): cherry-picked, AND develop then edits the same file ───────────────
# The realistic continuation: having taken the hotfix, the integration branch carries on
# with unrelated work. Both branches now hold hotfix.txt AND the trees differ, so the
# state-2 and state-3 tests are both silent and only the merge decides. Nothing is
# stranded and the guard must stay quiet.
git switch --quiet scratch-integration
commit feature3.txt f3 "I: unrelated work continues after the cherry-pick"
git switch --quiet scratch-release

if git diff --quiet "$M" "$D"; then
  fail "state 3b(ii) setup: trees identical — the fixture is wrong"
else
  expect "state 3b(ii)  cherry-picked, then develop moves on -> warn, NOT fail" \
    0 "verdict=warn repair=4b" "already contains main's changes"
fi

# ── LIMITATION, asserted rather than described ──────────────────────────────────────
# When develop's own edit REWRITES the hotfix's lines instead of leaving them alone, the
# in-memory merge conflicts and the guard cannot tell whether the fix survived. It goes
# red and says the merge conflicts. That is the safe direction — a conflicting reverse
# sync needs a human regardless — but it IS a false alarm about "stranded content", and
# it is pinned here so nobody discovers it in production and assumes the guard is broken.
# Isolated in its own repository so a deliberately conflicted state cannot leak into the
# fixtures above.
(
  iso="$WORK/iso"
  mkdir -p "$iso" && cd "$iso" || exit 2
  git init --quiet -b scratch-release .
  git config user.email test@example.invalid
  git config user.name "branch-invariant test"
  git config commit.gpgsign false
  printf 'v1\n' >app.txt && git add app.txt && git commit --quiet -m "A: base"
  git branch scratch-integration
  printf 'the fix\n' >hotfix.txt && git add hotfix.txt && git commit --quiet -m "H: hotfix on release"
  git switch --quiet scratch-integration
  printf 'the fix, rewritten differently\n' >hotfix.txt && git add hotfix.txt
  git commit --quiet -m "I: integration rewrites the same lines"
  git switch --quiet scratch-release
  out="$(GUARD_FETCH=0 GUARD_MAIN_REF="$M" GUARD_DEVELOP_REF="$D" \
         GITHUB_ACTIONS='' GITHUB_STEP_SUMMARY='' bash "$GUARD" 2>&1)"
  code=$?
  if [ "$code" = 1 ] && grep -qF "conflict textually" <<<"$out"; then
    printf '  PASS  %s\n' "limitation: divergent same-line edit -> fail, and says it is a conflict"
  else
    printf '  FAIL  %s\n' "limitation: divergent same-line edit: exit ${code}, wanted 1 naming the conflict"
    sed 's/^/        /' <<<"$out"
    exit 1
  fi
) || failures=$((failures + 1))

# ── STATE 5: repaired by a reverse-sync merge commit ────────────────────────────────
# CONVENTIONS §1A step 4b. The guard must go quiet again, which is what makes it
# actionable rather than permanently red.
git switch --quiet scratch-integration
git merge --quiet --no-ff -m "sync: release into integration" scratch-release
git switch --quiet scratch-release

expect "state 5  after §1A step 4b repair -> ok again" 0 "verdict=ok" "ok: main ⊆ develop"

# ── guard-cannot-run: a missing ref must not read as a passing invariant ────────────
out="$( cd "$WORK/repo" && GUARD_FETCH=0 GUARD_MAIN_REF=refs/heads/nope GUARD_DEVELOP_REF="$D" \
        GITHUB_ACTIONS='' GITHUB_STEP_SUMMARY='' bash "$GUARD" 2>&1 )"
code=$?
if [ "$code" = 2 ] && grep -qF "verdict=error" <<<"$out"; then
  pass "missing ref -> exit 2 (guard broken), distinct from both ok and fail"
else
  fail "missing ref: exit ${code}, wanted 2 with verdict=error"
  sed 's/^/        /' <<<"$out"
fi

echo
if [ "$failures" = 0 ]; then
  echo "all cases behaved as specified"
  exit 0
fi
echo "${failures} case(s) misbehaved"
exit 1
