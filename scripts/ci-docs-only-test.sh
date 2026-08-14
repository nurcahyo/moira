#!/usr/bin/env bash
# Self-test for the docs-only skip: scripts/ci-docs-only.sh and scripts/ci-required-gate.sh.
#
#     scripts/ci-docs-only-test.sh          # or: make test-docs-only
#
# Exits 0 if every case behaved as specified, 1 otherwise.
#
# ── WHY A SELF-TEST AND NOT A CODE REVIEW ────────────────────────────────────────────
#
# A path filter fails in the one direction nobody notices: it stops running a job on a
# change that needed it, the run goes green in forty seconds, and the missing coverage is
# invisible because nothing red ever appeared. Reading the allowlist cannot detect that;
# only driving it can. This is the same argument `scripts/branch-invariant-test.sh` makes
# in its own header — a guard that has never been observed to fail is not a guard — and
# the same reason it is built out of real git objects rather than mocked diffs.
#
# It runs as a step of the `changes` job in ci.yml, so it executes on EVERY run of this
# workflow including the docs-only ones it is gating. It costs about a second.
#
# ── THE CASES THAT MATTER MOST ───────────────────────────────────────────────────────
#
#   mixed        a markdown file and a Rust file in one change set must classify as
#                NOT docs-only. A documentation file masking a code file is the failure
#                mode a wrong filter actually produces.
#   docs/*.json  docs/openapi.json and docs/i18n-response-catalog.json are test fixtures
#                that live under docs/. They must never be inert. The `docs/` inventory
#                case below derives this from the real tree rather than from this list,
#                so it keeps holding as the tree changes.
#   unexplained  a `skipped` dependency with no docs-only verdict must turn the gate RED.
#                That is the whole difference between this design and simply putting an
#                `if:` on a required check.
#   .md in a     markdown inside a tree that is NOT on the allowlist must NOT be inert.
#   heavy tree   The first draft of the classifier had a bare `*.md` arm, and because
#                bash `case` globs cross `/` it classified
#                `charts/moira/templates/README.md` as inert — a file helm renders as a
#                manifest, whose linting job the verdict then skipped. The `not inert`
#                inventory below walks every such tree in the real repository and
#                asserts nothing in it is inert, so the classifier cannot drift away
#                from its own documentation again.

set -uo pipefail
export LC_ALL=C

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
CLASSIFIER="${SCRIPT_DIR}/ci-docs-only.sh"
GATE="${SCRIPT_DIR}/ci-required-gate.sh"

pass=0
fail=0

ok() {
    printf '  ok    %s\n' "$1"
    pass=$((pass + 1))
}
bad() {
    printf '  FAIL  %s\n' "$1"
    printf '        %s\n' "$2"
    fail=$((fail + 1))
}

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
SCRATCH="${WORK}/repo"
OUTFILE="${WORK}/github_output"
LOGFILE="${WORK}/log"

# ── A scratch repository with the shape of the real one ──────────────────────────────
#
# Real git objects, real `git diff`, the real script. The point of building a tree rather
# than feeding the classifier a list of strings is that the diff-range handling — three
# dot versus two dot, --no-renames, deletions — is part of what can be wrong, and a
# string-list test would declare all of it correct by construction.
mkdir -p "$SCRATCH"
(
    cd "$SCRATCH" || exit 1
    git init --quiet --initial-branch=develop .
    git config user.email ci@example.invalid
    git config user.name "ci self-test"
    mkdir -p docs plans .agents/skills/x/agents .claude skills/y/agents \
        src/i18n tests migrations charts/moira/templates console \
        .github/workflows scripts config
    printf 'readme\n' >README.md
    printf 'prose\n' >docs/security.md
    printf '{}\n' >docs/openapi.json
    printf '{}\n' >docs/i18n-response-catalog.json
    printf 'plan\n' >plans/01.md
    printf 'skill\n' >.agents/skills/x/SKILL.md
    printf 'agents:\n' >.agents/skills/x/agents/openai.yaml
    printf '{}\n' >.claude/settings.json
    printf 'skill\n' >skills/y/SKILL.md
    printf 'fn main() {}\n' >src/main.rs
    printf 'fn t() {}\n' >tests/t.rs
    printf 'select 1;\n' >migrations/0001_x.sql
    printf 'replicas: 1\n' >charts/moira/values.yaml
    # Markdown inside trees that are NOT on the allowlist. These mirror files that
    # really exist in the repository and are what the anchoring has to hold against.
    printf 'chart docs\n' >charts/moira/templates/README.md
    printf 'module docs\n' >src/i18n/README.md
    printf 'console docs\n' >console/README.md
    printf '{}\n' >console/package.json
    printf 'name: ci\n' >.github/workflows/ci.yml
    printf 'FROM scratch\n' >Dockerfile
    printf '[package]\n' >Cargo.toml
    printf 'lock\n' >Cargo.lock
    printf 'deny\n' >deny.toml
    printf 'echo\n' >scripts/ci-docs-only.sh
    printf 'x = 1\n' >config/default.toml
    git add -A
    git commit --quiet -m base
) || {
    printf 'could not build the scratch repository\n' >&2
    exit 1
}

BASE="$(git -C "$SCRATCH" rev-parse HEAD)"

# Applies a change, commits it, and returns the new SHA on stdout. The scratch branch is
# reset back to BASE first so every case is independent.
commit_change() {
    local msg="$1"
    shift
    git -C "$SCRATCH" reset --hard --quiet "$BASE"
    git -C "$SCRATCH" clean -fdq
    (cd "$SCRATCH" && "$@") || return 1
    git -C "$SCRATCH" add -A
    git -C "$SCRATCH" commit --quiet -m "$msg"
    git -C "$SCRATCH" rev-parse HEAD
}

# Runs the real classifier over an explicit range and echoes the verdict it wrote to
# $GITHUB_OUTPUT — the value ci.yml actually consumes, not the prose on stdout.
classify_range() {
    : >"$OUTFILE"
    (
        cd "$SCRATCH" || exit 1
        env -u GITHUB_EVENT_NAME -u GITHUB_REF_NAME -u GITHUB_STEP_SUMMARY \
            GITHUB_OUTPUT="$OUTFILE" \
            "$CLASSIFIER" "$1" "$2"
    ) >"$LOGFILE" 2>&1
    sed -n 's/^docs_only=//p' "$OUTFILE" | tail -n 1
}

expect_range() {
    local label="$1" want="$2" head="$3"
    local got
    got="$(classify_range "$BASE" "$head")"
    if [ "$got" = "$want" ]; then
        ok "$label"
    else
        bad "$label" "expected docs_only=${want}, got '${got}' — log: $(tail -n 1 "$LOGFILE")"
    fi
}

printf '\nchange sets — the allowlist\n'

expect_range "markdown at the root is inert" true \
    "$(commit_change md-root sh -c 'printf "changed\n" >> README.md')"

expect_range "markdown under docs/ is inert" true \
    "$(commit_change md-docs sh -c 'printf "more\n" >> docs/security.md')"

expect_range "a new markdown file is inert" true \
    "$(commit_change md-new sh -c 'printf "new\n" > docs/brand-new.md')"

expect_range "deleting a markdown file is inert" true \
    "$(commit_change md-delete sh -c 'rm docs/security.md')"

expect_range "several markdown files at once are inert" true \
    "$(commit_change md-many sh -c 'printf "a\n" >> README.md; printf "b\n" >> plans/01.md; printf "c\n" >> .agents/skills/x/SKILL.md')"

expect_range "plans/ is inert" true \
    "$(commit_change plans sh -c 'printf "p\n" >> plans/01.md')"

expect_range ".claude/ harness config is inert" true \
    "$(commit_change claude sh -c 'printf "{\"a\":1}\n" > .claude/settings.json')"

expect_range ".agents/ skill config is inert" true \
    "$(commit_change agents sh -c 'printf "model: x\n" >> .agents/skills/x/agents/openai.yaml')"

expect_range "skills/ is inert" true \
    "$(commit_change skills sh -c 'printf "s\n" >> skills/y/SKILL.md')"

expect_range "a LICENSE file is inert" true \
    "$(commit_change license sh -c 'printf "MIT\n" > LICENSE')"

printf '\nchange sets — everything else runs the full suite\n'

expect_range "src/ is not inert" false \
    "$(commit_change src sh -c 'printf "// x\n" >> src/main.rs')"

expect_range "tests/ is not inert" false \
    "$(commit_change tests sh -c 'printf "// x\n" >> tests/t.rs')"

expect_range "MIXED: markdown plus Rust is NOT docs-only" false \
    "$(commit_change mixed sh -c 'printf "doc\n" >> README.md; printf "// code\n" >> src/main.rs')"

expect_range "MIXED: many markdown files cannot outvote one Rust file" false \
    "$(commit_change mixed-many sh -c 'printf "a\n" >> README.md; printf "b\n" >> plans/01.md; printf "c\n" >> docs/security.md; printf "// code\n" >> src/main.rs')"

expect_range "docs/openapi.json is NOT inert (it is a test fixture)" false \
    "$(commit_change openapi sh -c 'printf "{\"a\":1}\n" > docs/openapi.json')"

expect_range "docs/i18n-response-catalog.json is NOT inert" false \
    "$(commit_change i18n sh -c 'printf "{\"a\":1}\n" > docs/i18n-response-catalog.json')"

expect_range "Dockerfile is not inert" false \
    "$(commit_change dockerfile sh -c 'printf "USER 1\n" >> Dockerfile')"

expect_range "charts/ is not inert" false \
    "$(commit_change charts sh -c 'printf "x: 1\n" >> charts/moira/values.yaml')"

expect_range ".github/ is not inert" false \
    "$(commit_change gh sh -c 'printf "# x\n" >> .github/workflows/ci.yml')"

expect_range "Cargo.toml is not inert" false \
    "$(commit_change cargotoml sh -c 'printf "# x\n" >> Cargo.toml')"

expect_range "Cargo.lock is not inert" false \
    "$(commit_change cargolock sh -c 'printf "x\n" >> Cargo.lock')"

expect_range "deny.toml is not inert" false \
    "$(commit_change deny sh -c 'printf "x\n" >> deny.toml')"

expect_range "migrations/ is not inert" false \
    "$(commit_change migrations sh -c 'printf "select 2;\n" >> migrations/0001_x.sql')"

expect_range "console/ is not inert" false \
    "$(commit_change console sh -c 'printf "{\"a\":1}\n" > console/package.json')"

expect_range "scripts/ is not inert — including this mechanism itself" false \
    "$(commit_change scripts sh -c 'printf "# x\n" >> scripts/ci-docs-only.sh')"

expect_range "config/ is not inert" false \
    "$(commit_change config sh -c 'printf "y = 2\n" >> config/default.toml')"

expect_range "RENAME: README.md -> build.rs is not inert" false \
    "$(commit_change rename git mv README.md build.rs)"

# The regression cases, over a real diff rather than --classify-path. A bare `*.md` arm
# passes every other test in this file and fails these three.
expect_range "markdown under charts/templates/ is NOT docs-only (helm renders it)" false \
    "$(commit_change chart-md sh -c 'printf "more\n" >> charts/moira/templates/README.md')"

expect_range "markdown under src/ is NOT docs-only" false \
    "$(commit_change src-md sh -c 'printf "more\n" >> src/i18n/README.md')"

expect_range "markdown under console/ is NOT docs-only" false \
    "$(commit_change console-md sh -c 'printf "more\n" >> console/README.md')"

expect_range "a chart README cannot ride along with real docs" false \
    "$(commit_change chart-md-mixed sh -c 'printf "a\n" >> README.md; printf "b\n" >> docs/security.md; printf "c\n" >> charts/moira/templates/README.md')"

printf '\nranges that cannot be classified fail closed\n'

got="$(classify_range "$BASE" "$BASE")"
if [ "$got" = "false" ] && grep -q 'no changed paths' "$LOGFILE"; then
    ok "an empty diff is not docs-only"
else
    bad "an empty diff is not docs-only" "got '${got}': $(tail -n 1 "$LOGFILE")"
fi

got="$(classify_range "$BASE" 0000000000000000000000000000000000000000)"
if [ "$got" = "false" ] && grep -q 'not present in this checkout' "$LOGFILE"; then
    ok "a missing revision is not docs-only (shallow-clone guard)"
else
    bad "a missing revision is not docs-only" "got '${got}': $(tail -n 1 "$LOGFILE")"
fi

printf '\nevent handling\n'

# Same scratch history, driven through the environment ci.yml actually sets.
run_event() {
    : >"$OUTFILE"
    (
        cd "$SCRATCH" || exit 1
        env -u GITHUB_STEP_SUMMARY GITHUB_OUTPUT="$OUTFILE" "$@" "$CLASSIFIER"
    ) >"$LOGFILE" 2>&1
    sed -n 's/^docs_only=//p' "$OUTFILE" | tail -n 1
}

expect_event() {
    local label="$1" want="$2" needle="$3"
    shift 3
    local got
    got="$(run_event "$@")"
    if [ "$got" = "$want" ] && grep -q "$needle" "$LOGFILE"; then
        ok "$label"
    else
        bad "$label" "expected ${want} matching '${needle}', got '${got}': $(tail -n 2 "$LOGFILE" | tr '\n' ' ')"
    fi
}

MD_HEAD="$(commit_change md-event sh -c 'printf "changed\n" >> README.md')"
CODE_HEAD="$(commit_change code-event sh -c 'printf "// x\n" >> src/main.rs')"

expect_event "pull_request into develop, markdown only -> docs-only" true "inert allowlist" \
    GITHUB_EVENT_NAME=pull_request PR_BASE_REF=develop PR_BASE_SHA="$BASE" PR_HEAD_SHA="$MD_HEAD"

expect_event "pull_request into develop, code -> full suite" false "outside the inert allowlist" \
    GITHUB_EVENT_NAME=pull_request PR_BASE_REF=develop PR_BASE_SHA="$BASE" PR_HEAD_SHA="$CODE_HEAD"

expect_event "pull_request into main is never docs-only" false "targets main" \
    GITHUB_EVENT_NAME=pull_request PR_BASE_REF=main PR_BASE_SHA="$BASE" PR_HEAD_SHA="$MD_HEAD"

expect_event "push to develop, markdown only -> docs-only" true "inert allowlist" \
    GITHUB_EVENT_NAME=push GITHUB_REF_NAME=develop PUSH_BEFORE_SHA="$BASE" PUSH_AFTER_SHA="$MD_HEAD"

expect_event "push to main is never docs-only" false "push to main" \
    GITHUB_EVENT_NAME=push GITHUB_REF_NAME=main PUSH_BEFORE_SHA="$BASE" PUSH_AFTER_SHA="$MD_HEAD"

expect_event "a new branch (all-zero before-SHA) is not docs-only" false "no usable before-SHA" \
    GITHUB_EVENT_NAME=push GITHUB_REF_NAME=topic \
    PUSH_BEFORE_SHA=0000000000000000000000000000000000000000 PUSH_AFTER_SHA="$MD_HEAD"

expect_event "workflow_dispatch always runs the full suite" false "workflow_dispatch" \
    GITHUB_EVENT_NAME=workflow_dispatch

expect_event "an unrecognised event is not docs-only" false "unrecognised event" \
    GITHUB_EVENT_NAME=schedule

printf '\nthe real docs/ tree — inert if and only if it is markdown\n'

# DERIVED, not listed. This is what keeps `docs/openapi.json` out of the allowlist as the
# tree changes: any future non-markdown file added under docs/ is covered the day it
# lands, and any future widening of the allowlist to `docs/**` turns this red.
docs_bad=0
while IFS= read -r path; do
    [ -n "$path" ] || continue
    want=false
    case "$path" in *.md) want=true ;; esac
    verdict="$("$CLASSIFIER" --classify-path "$path")"
    if [ "$verdict" != "$want" ]; then
        printf '  FAIL  docs/ inventory: %s classified inert=%s, expected %s\n' "$path" "$verdict" "$want"
        docs_bad=$((docs_bad + 1))
    fi
done < <(git -C "$REPO_ROOT" ls-files docs/)
if [ "$docs_bad" -eq 0 ]; then
    ok "every tracked file under docs/ is inert exactly when it is markdown"
else
    fail=$((fail + 1))
fi

printf '\nthe real not-inert trees — NOTHING in them is inert, markdown included\n'

# The mirror of the docs/ inventory, and the case that would have caught the bare `*.md`
# arm. Every tree named here is on the "EXPLICITLY NOT INERT" list in the classifier's
# header; this walks the real repository and asserts the code agrees with that list, so
# prose and behaviour cannot diverge the way they did in the first draft.
#
# Four tracked README.md files live in these trees today —
# src/i18n/catalog/README.md, console/README.md, console/modules/README.md and
# deploy/observability/README.md — so this is not a hypothetical guard; it fails on the
# tree as it stands if the anchoring is ever removed.
heavy_bad=0
heavy_checked=0
while IFS= read -r path; do
    [ -n "$path" ] || continue
    heavy_checked=$((heavy_checked + 1))
    verdict="$("$CLASSIFIER" --classify-path "$path")"
    if [ "$verdict" != "false" ]; then
        printf '  FAIL  not-inert inventory: %s classified inert=true\n' "$path"
        heavy_bad=$((heavy_bad + 1))
    fi
done < <(git -C "$REPO_ROOT" ls-files \
    charts/ .github/ src/ tests/ console/ scripts/ migrations/ config/ ci/ deploy/)
if [ "$heavy_checked" -eq 0 ]; then
    bad "not-inert inventory listed no files" "the ls-files pathspec matched nothing"
elif [ "$heavy_bad" -eq 0 ]; then
    ok "none of the ${heavy_checked} tracked files in the heavy trees is inert"
else
    fail=$((fail + 1))
fi

printf '\nanchoring — a bash case glob crosses "/", so every entry must be pinned\n'

expect_path() {
    local label="$1" want="$2" path="$3" got
    got="$("$CLASSIFIER" --classify-path "$path")"
    if [ "$got" = "$want" ]; then
        ok "$label"
    else
        bad "$label" "expected inert=${want} for '${path}', got ${got}"
    fi
}

# The live one: helm renders every file under templates/, there is no .helmignore, and a
# README documenting the templates is the likeliest place for a `{{ .Values.* }}`.
expect_path "charts/*/templates/README.md is NOT inert" false charts/moira/templates/README.md
expect_path "charts/*/README.md is NOT inert" false charts/moira/README.md
expect_path "src/**/README.md is NOT inert" false src/i18n/catalog/README.md
expect_path "console/**/README.md is NOT inert" false console/modules/README.md
expect_path "deploy/**/README.md is NOT inert" false deploy/observability/README.md
expect_path ".github/**/*.md is NOT inert" false .github/PULL_REQUEST_TEMPLATE.md
expect_path "migrations/**/*.md is NOT inert" false migrations/README.md
expect_path "scripts/**/*.md is NOT inert" false scripts/README.md
expect_path "tests/**/*.md is NOT inert" false tests/fixtures/golden.md
expect_path "a brand-new top-level tree's markdown is NOT inert" false guides/getting-started.md

# ...and the entries that must keep working, so the anchoring is not simply "deny all".
expect_path "root README.md is inert" true README.md
expect_path "root CLAUDE.md is inert" true CLAUDE.md
expect_path "docs/*.md is inert" true docs/ci-docs-only.md
expect_path "docs/**/*.md at depth is inert" true docs/adr/0007-thing.md
expect_path "docs/openapi.json is NOT inert" false docs/openapi.json
expect_path "plans/ is inert whole-tree" true plans/CONVENTIONS.md
expect_path ".claude/settings.json is inert whole-tree" true .claude/settings.json
expect_path ".agents/ is inert whole-tree" true .agents/skills/x/agents/openai.yaml

# `LICENSE.*` would have crossed `/` exactly as `*.md` did.
expect_path "LICENSE is inert" true LICENSE
expect_path "LICENSE.md is inert" true LICENSE.md
expect_path "LICENSE.d/build.rs is NOT inert" false LICENSE.d/build.rs

# Not producible by `git diff --name-only`; fail closed rather than guess.
expect_path "an absolute path is NOT inert" false /etc/passwd
expect_path "a traversal is NOT inert" false docs/../src/main.rs
expect_path "an empty path is NOT inert" false ""

printf '\nthe required-check gate\n'

expect_gate() {
    local label="$1" want_rc="$2"
    shift 2
    local rc=0
    "$@" >"$LOGFILE" 2>&1 || rc=$?
    if [ "$rc" -eq "$want_rc" ]; then
        ok "$label"
    else
        bad "$label" "expected exit ${want_rc}, got ${rc}: $(tail -n 2 "$LOGFILE" | tr '\n' ' ')"
    fi
}

expect_gate "a job that ran and passed is green" 0 \
    env CHANGES_RESULT=success DOCS_ONLY=false "$GATE" work=success

expect_gate "a job skipped under a docs-only verdict is green" 0 \
    env CHANGES_RESULT=success DOCS_ONLY=true "$GATE" work=skipped

expect_gate "a job skipped WITHOUT a docs-only verdict is RED" 1 \
    env CHANGES_RESULT=success DOCS_ONLY=false "$GATE" work=skipped

expect_gate "an empty docs-only verdict is RED, not permissive" 1 \
    env CHANGES_RESULT=success DOCS_ONLY= "$GATE" work=skipped

expect_gate "a failed job is RED even under a docs-only verdict" 1 \
    env CHANGES_RESULT=success DOCS_ONLY=true "$GATE" work=failure

expect_gate "a cancelled job is RED" 1 \
    env CHANGES_RESULT=success DOCS_ONLY=true "$GATE" work=cancelled

expect_gate "a broken classifier makes every gate RED" 1 \
    env CHANGES_RESULT=failure DOCS_ONLY=true "$GATE" work=skipped

expect_gate "a skipped classifier makes every gate RED" 1 \
    env CHANGES_RESULT=skipped DOCS_ONLY=true "$GATE" work=skipped

expect_gate "a gate with no dependencies to check is RED" 1 \
    env CHANGES_RESULT=success DOCS_ONLY=true "$GATE"

expect_gate "several dependencies, all accounted for, is green" 0 \
    env CHANGES_RESULT=success DOCS_ONLY=true "$GATE" a=skipped b=skipped c=skipped

expect_gate "several dependencies, one red, is RED" 1 \
    env CHANGES_RESULT=success DOCS_ONLY=false "$GATE" a=success b=failure c=success

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
