#!/usr/bin/env bash
# Classify a change set as "documentation only" — the input to the heavy-job skip in
# .github/workflows/ci.yml.
#
# Prints a verdict, writes `docs_only=true|false` to $GITHUB_OUTPUT, and exits 0 in
# every case a caller can act on. The verdict is consumed by `scripts/ci-required-gate.sh`,
# which is what actually lets a required check report success over a skipped dependency.
#
#     scripts/ci-docs-only.sh                      # reads the event from the GITHUB_* environment
#     scripts/ci-docs-only.sh A B                  # classify the diff between two revisions, locally
#     scripts/ci-docs-only.sh --classify-path P    # print true/false for ONE path, no git
#
# The third form exists so `scripts/ci-docs-only-test.sh` can interrogate the allowlist
# directly — in particular to walk the real `docs/` tree and assert inert ⇔ markdown —
# without re-implementing the patterns in the test, which would only pin a copy against
# a copy.
#
# ── THE ALLOWLIST IS THE WHOLE SAFETY ARGUMENT ───────────────────────────────────────
#
# This is an ALLOWLIST of paths proven inert, never a denylist of paths known heavy.
# The difference is what happens to a path nobody thought about: under an allowlist a
# new directory runs the full suite (slow, correct), under a denylist it is skipped
# (fast, and wrong in a way that is green and silent). A mis-scoped filter does not fail
# — it stops running a job on a change that needed it, which is the same class of defect
# `scripts/ci-assert-union.sh` exists to catch and is just as invisible.
#
# "Inert" here has one meaning and it is mechanical: **no job in ci.yml can produce a
# different result because this file changed.** Not "looks like documentation". Each
# entry below carries the check that was actually run to establish it.
#
# ── `docs/**` IS NOT ON THE LIST, AND THAT IS THE INTERESTING PART ───────────────────
#
# `docs/` looks like the obvious entry and it is wrong. Two files in it are live test
# fixtures, not prose:
#
#   docs/openapi.json              read by tests/openapi_drift.rs (COMMITTED_OPENAPI_PATH),
#                                  tests/auth_provider_settings.rs, tests/admin_query_contract.rs,
#                                  src/http/mod.rs's committed-document pin, and
#                                  console/tests/contract/openapi-contract.test.ts
#   docs/i18n-response-catalog.json  read by src/i18n/catalog/mod.rs (DOCS_MIRROR_PATH) and
#                                  console/tests/unit/lib/moira-keys.test.ts
#
# Allowlisting `docs/**` would let a hand-edit of the committed OpenAPI document — the
# exact drift `tests/openapi_drift.rs` exists to catch — merge without that test ever
# running, on both the Rust and the console side. So the entry is `docs/*.md`, which
# reaches every prose file under `docs/` and neither of those two.
# `scripts/ci-docs-only-test.sh` pins this: it walks `git ls-files docs/` and asserts
# inert ⇔ the name ends in `.md`, so a future widening to `docs/**` goes red instead of
# going quiet.
#
# ── EXPLICITLY NOT INERT ─────────────────────────────────────────────────────────────
#
# These are not on the allowlist and therefore run everything — INCLUDING any markdown
# inside them. That last clause is not free: it is enforced by the anchoring in
# `path_is_inert` below, it was not true of the first draft of this file, and
# `scripts/ci-docs-only-test.sh` now walks each of these trees and asserts that nothing
# in them is inert, so the code and this list cannot drift apart again.
#
# They are named because a reader looking for them should find them ruled out in writing:
#
#   Dockerfile, console/Dockerfile   the image `container-and-helm` builds and Trivy scans
#   charts/**                        helm lint / template / kubeconform. Helm renders
#                                    EVERY file under `templates/`, markdown included,
#                                    and neither chart has a `.helmignore`.
#   .github/**                       this workflow, and semgrep's p/github-actions rules
#   Cargo.toml, Cargo.lock           every Rust job; `cargo audit` / `cargo deny` read them
#   deny.toml                        `cargo deny`, and tests/supply_chain_policy.rs include_str!s it
#   migrations/**                    rust-migrations, and several tests include_str! a migration
#   src/**, tests/**                 the suite itself
#   console/**                       the `console` job in full
#   scripts/**                       the CI scripts, INCLUDING this one and its self-test
#   config/**, ci/**, deploy/**      runtime config, cost table, deployment manifests
#   docs/*.json                      see above
#   Makefile, docker-compose.yml, .gitleaks.toml, .dockerignore, .env.example
#
# ── FAIL CLOSED, ALWAYS TOWARDS RUNNING MORE ─────────────────────────────────────────
#
# Every uncertainty resolves to `docs_only=false`: an unknown event type, a diff range
# that cannot be computed, a force-pushed or brand-new branch whose `before` SHA is gone,
# an empty change set, `main` on either end. "Could not tell" and "not docs" produce the
# same behaviour on purpose, because the cost of being wrong is six minutes in one
# direction and an unrun gate in the other.

set -uo pipefail
export LC_ALL=C

# ── The allowlist ────────────────────────────────────────────────────────────────────
#
# EVERY ENTRY IS ANCHORED AT A KNOWN LOCATION. There is no bare `*.md` arm, and the
# reason is a defect that review caught in the first draft of this file.
#
# Bash `case` globs are string globs, not pathname globs: `*` crosses `/`. So a bare
# `*.md` arm does not mean "a markdown file", it means "any path anywhere whose name
# ends in .md" — and it silently punched a hole straight through four of the trees the
# header above declares NOT INERT. Concretely, `charts/moira/templates/README.md`
# classified as inert. Helm renders every file under `templates/` as a manifest (the
# only exemptions are `_`-prefixed partials and `NOTES.txt`, and neither chart has a
# `.helmignore`), so that path is fed to `helm lint`, `helm template` and kubeconform —
# and a README documenting the templates is exactly the file most likely to contain a
# `{{ .Values.* }}` expression that Go's template engine will execute and die on. The
# job that would have caught it is `container-and-helm-build`, and the classifier had
# just decided not to run it. Same shape, dormant rather than live, for
# `src/i18n/catalog/README.md`, `console/README.md`, `console/modules/README.md` and
# `deploy/observability/README.md`, all of which are tracked today.
#
# The prose said one thing and the code did another, and the code wins. So the code now
# says it: markdown is inert **at the repository root, under `docs/`, and inside the
# four agent/planning trees**, and nowhere else. A markdown file anywhere else runs the
# full suite. That is a real allowlist — a new top-level directory's prose is slow and
# correct until someone adds an entry here, rather than fast and wrong on the day it
# lands.
path_is_inert() {
    local path="$1"

    # Not a plain repository-relative path — an absolute path, a traversal, or empty.
    # `git diff --name-only` cannot emit these, so reaching here means the caller is
    # something other than this script's own diff. Fail closed rather than guess.
    case "$path" in
        '' | /* | ../* | */../* | */..) return 1 ;;
    esac

    # ── Whole-tree entries ───────────────────────────────────────────────────────────
    #
    # The three agent-instruction trees and the planning tree. Grepped across .github/,
    # scripts/, Makefile, ci/, src/, tests/ and console/: nothing reads any of them, so
    # no job's result can move. They are whole-tree rather than markdown-only because
    # each carries a little non-markdown harness config (`.claude/settings.json`, the
    # per-skill `agents/openai.yaml` files) that is just as unread by CI as the prose
    # beside it.
    case "$path" in
        .agents/* | .claude/* | skills/* | plans/*) return 0 ;;
    esac

    # ── Prose under docs/, and ONLY the markdown in it ───────────────────────────────
    #
    # `docs/*.md` is anchored at `docs/`, and the `*` crossing `/` is wanted here: it
    # reaches `docs/adr/0007-foo.md` and cannot reach outside the directory. The
    # explicit `docs/*` arm below it is what keeps `docs/openapi.json` and
    # `docs/i18n-response-catalog.json` — live test fixtures, see the header — out, and
    # says so in code rather than relying on the fall-through.
    case "$path" in
        docs/*.md) return 0 ;;
        docs/*) return 1 ;;
    esac

    # ── Repository root only ─────────────────────────────────────────────────────────
    #
    # Anything with a directory component has now had its one chance above. This is the
    # arm that replaces the old bare `*.md`, and the `*/*` test is the anchor: it is
    # what stops `charts/moira/templates/README.md` from ever reaching the `.md` case.
    case "$path" in
        */*) return 1 ;;
    esac

    case "$path" in
        # Root markdown: README.md, AGENTS.md, CLAUDE.md, TODO.md, NEED_CONFIRMATION.md.
        # Nothing compiles, embeds, imports or parses a `.md` file in this repository:
        # no `include_str!`/`include_bytes!` names one (checked across src/, tests/ and
        # build.rs), the console imports none, and none of the five semgrep rulesets in
        # the `sast` job (p/rust, p/typescript, p/javascript, p/github-actions,
        # p/dockerfile) has a markdown analyzer.
        *.md) return 0 ;;

        # Legal and notice text. Read by no job; enumerated rather than globbed, because
        # `LICENSE.*` would have crossed `/` the same way `*.md` did. (None exist today;
        # listed so that adding one later is not a six-minute event. The `.md` spellings
        # are deliberately absent — the arm above already covers them, and repeating
        # them here is a shellcheck SC2222 dead pattern.)
        LICENSE | LICENSE.txt | NOTICE | NOTICE.txt | COPYING) return 0 ;;
    esac

    return 1
}

# Single-path interrogation, for the self-test. Answered before anything touches git, so
# it works in any directory and cannot be confused by the repository's state.
if [ "${1:-}" = "--classify-path" ]; then
    if [ "$#" -ne 2 ]; then
        printf 'usage: %s --classify-path <path>\n' "$0" >&2
        exit 2
    fi
    if path_is_inert "$2"; then printf 'true\n'; else printf 'false\n'; fi
    exit 0
fi

say() { printf '%s\n' "$*"; }

# Two outputs, deliberately separate. stdout is the log; $GITHUB_OUTPUT is the contract
# with ci.yml. Writing the output file is the LAST thing that happens, so a script that
# dies half way through publishes nothing and the consuming `if:` sees an empty string,
# which `!= 'true'` reads as "run everything".
emit() {
    local verdict="$1" reason="$2"
    say ""
    say "verdict: docs_only=${verdict} — ${reason}"
    if [ -n "${GITHUB_OUTPUT:-}" ]; then
        printf 'docs_only=%s\n' "$verdict" >>"$GITHUB_OUTPUT"
    fi
    if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
        {
            printf '### docs-only classifier\n\n'
            printf '`docs_only=%s` — %s\n\n' "$verdict" "$reason"
            if [ -n "${CHANGED_FILES:-}" ]; then
                printf '<details><summary>%s changed path(s)</summary>\n\n```\n%s\n```\n\n</details>\n' \
                    "$(printf '%s\n' "$CHANGED_FILES" | wc -l | tr -d ' ')" "$CHANGED_FILES"
            fi
        } >>"$GITHUB_STEP_SUMMARY"
    fi
    exit 0
}

# ── Resolve the diff range for this event ────────────────────────────────────────────

BASE=""
HEAD=""

if [ "$#" -eq 2 ]; then
    # Local / self-test invocation. No event, two explicit revisions.
    BASE="$1"
    HEAD="$2"
    say "range: explicit ${BASE}..${HEAD}"
else
    case "${GITHUB_EVENT_NAME:-}" in
        pull_request | pull_request_target)
            # A promotion PR into `main` is never classified as docs-only. The green run
            # on a release is the evidence that the released tree was built and scanned,
            # and a promotion carries every commit since the last one anyway, so this
            # costs nothing real and removes the one case where a skip would degrade a
            # release record.
            if [ "${PR_BASE_REF:-}" = "main" ]; then
                emit false "pull request targets main; release evidence is never skipped"
            fi
            BASE="${PR_BASE_SHA:-}"
            HEAD="${PR_HEAD_SHA:-}"
            say "range: pull request ${BASE}...${HEAD} (three-dot, against the merge base)"
            ;;
        push)
            # Same reasoning as above, from the other side.
            if [ "${GITHUB_REF_NAME:-}" = "main" ]; then
                emit false "push to main; release evidence is never skipped"
            fi
            BASE="${PUSH_BEFORE_SHA:-}"
            HEAD="${PUSH_AFTER_SHA:-}"
            # All-zeros is what GitHub sends for the first push to a new branch. There is
            # no previous state to diff against, so there is nothing to prove inert.
            case "$BASE" in
                '' | 0000000000000000000000000000000000000000)
                    emit false "push has no usable before-SHA (new branch or force push)"
                    ;;
            esac
            say "range: push ${BASE}..${HEAD}"
            ;;
        workflow_dispatch)
            # The manual trigger exists precisely to get real CI onto a ref when the
            # event never arrived. Answering it with a skipped suite would defeat it.
            emit false "workflow_dispatch always runs the full suite"
            ;;
        *)
            emit false "unrecognised event '${GITHUB_EVENT_NAME:-<unset>}'"
            ;;
    esac
fi

if [ -z "$BASE" ] || [ -z "$HEAD" ]; then
    emit false "could not determine both ends of the diff range"
fi

# Both objects must actually be present. A shallow clone is the common way this goes
# wrong, and it must not be answered with a confident "docs only".
for rev in "$BASE" "$HEAD"; do
    if ! git cat-file -e "${rev}^{commit}" 2>/dev/null; then
        emit false "revision ${rev} is not present in this checkout (shallow clone?)"
    fi
done

# ── Compute the change set ───────────────────────────────────────────────────────────
#
# `--no-renames` on purpose: with rename detection on, `git diff --name-only` reports a
# rename as the destination path alone, so `README.md` -> `build.rs` would be classified
# on `build.rs` only by luck of which side is printed. With it off, both the deleted and
# the added path are listed and BOTH must be inert for the change to count as docs-only.
#
# Three-dot for pull requests (changes the branch introduced, not changes the base made
# since the fork point); two-dot for a push, where `before..after` is exactly the push.
if [ "${GITHUB_EVENT_NAME:-}" = "pull_request" ] || [ "${GITHUB_EVENT_NAME:-}" = "pull_request_target" ] || [ "$#" -eq 2 ]; then
    RANGE_OP="..."
else
    RANGE_OP=".."
fi

if ! CHANGED_FILES="$(git diff --name-only --no-renames "${BASE}${RANGE_OP}${HEAD}" 2>&1)"; then
    say "git diff failed:"
    say "$CHANGED_FILES"
    CHANGED_FILES=""
    emit false "git diff over the range failed"
fi

if [ -z "$CHANGED_FILES" ]; then
    # An empty diff is not evidence of a documentation change; it is evidence that the
    # range was wrong. Merge commits and re-runs both land here.
    emit false "the range produced no changed paths"
fi

# ── Classify ─────────────────────────────────────────────────────────────────────────
#
# Every path is printed with its verdict, so the log of a skipped run says exactly which
# files bought the skip. A wrong filter is then visible by reading, not only by failing.

not_inert=0
say ""
say "changed paths:"
while IFS= read -r path; do
    [ -n "$path" ] || continue
    if path_is_inert "$path"; then
        say "  inert      ${path}"
    else
        say "  NOT inert  ${path}"
        not_inert=$((not_inert + 1))
    fi
done <<EOF
$CHANGED_FILES
EOF

if [ "$not_inert" -gt 0 ]; then
    # One non-inert path is enough. A documentation file can never mask a code file: the
    # verdict is an AND over every path, not a majority or a heuristic.
    emit false "${not_inert} path(s) outside the inert allowlist"
fi

emit true "every changed path is on the inert allowlist"
