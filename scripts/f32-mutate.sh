#!/usr/bin/env bash
# Hand-written mutations for `conversation_content_persistence` — finding F32.
#
# # Why this is a committed script rather than a paragraph in a report
#
# HANDOFF §3.4 lists six guards that were green against broken code, "every one found by *running
# the mutation* and none by reading the test". A prose claim that a guard works is exactly the
# artefact that failed six times. This script re-derives the claim on demand.
#
# Each mutation is the cheapest edit that breaks the property while leaving the code compiling —
# the question §3.4 says found all five toothless guards. Every one is applied, tested, and
# reverted; the script exits non-zero if any mutation leaves the suite green.
#
# Not a CI gate: it rewrites tracked files and needs a database. Run it when touching the policy.
#
#   scripts/f32-mutate.sh
#
set -uo pipefail

ROOT=$(git rev-parse --show-toplevel) || exit 1
cd "$ROOT" || exit 1
: "${MOIRA_TEST_DATABASE_URL:=postgres://postgres:postgres@127.0.0.1:5432/moira}"
export MOIRA_TEST_DATABASE_URL

LOGDIR=$(mktemp -d)
REPO=src/infra/repositories/conversation.rs
APP=src/application/conversation.rs
DOM=src/domain/conversation.rs

restore() { git checkout -- "$REPO" "$APP" "$DOM" 2>/dev/null; }
trap restore EXIT

# A "revert" that discarded real work would be worse than no mutation testing at all.
if [ -n "$(git status --porcelain -- "$REPO" "$APP" "$DOM")" ]; then
    echo "REFUSING: uncommitted changes in the files this script rewrites."
    echo "Commit or stash them first — the revert step would discard them."
    exit 2
fi

survivors=()

# apply <python-heredoc-on-stdin>; then verify <label> <cargo test args...>
verify() {
    local label="$1"; shift
    local log="$LOGDIR/$label.log"
    cargo test "$@" >"$log" 2>&1
    if [ $? -eq 0 ]; then
        echo "SURVIVED  $label — the suite is green against broken code. Log: $log"
        survivors+=("$label")
    else
        echo "CAUGHT    $label"
        grep -E "^test .* FAILED" "$log" | head -3 | sed 's/^/            /'
    fi
    restore
}

echo "== M1: add_message stores plaintext whatever the policy says (the pre-fix line) =="
python3 - <<'PY'
p = 'src/infra/repositories/conversation.rs'
s = open(p).read()
old = """        let content_plain = insert
            .content_plain
            .as_deref()
            .filter(|_| persistence.persists_plaintext());"""
assert old in s, "M1 anchor not found — the code moved; update this script"
open(p, 'w').write(s.replace(old, "        let content_plain = insert.content_plain.as_deref();"))
PY
verify M1-plaintext-anyway --test conversation_content_persistence

echo "== M2: 'none' keeps the length metadata, collapsing it into 'metadata_only' =="
python3 - <<'PY'
p = 'src/infra/repositories/conversation.rs'
s = open(p).read()
old = """        let (content_size_bytes, token_count) = if persistence.persists_content_metadata() {
            (insert.content_size_bytes, insert.token_count)
        } else {
            (0, None)
        };"""
assert old in s, "M2 anchor not found — the code moved; update this script"
new = "        let (content_size_bytes, token_count) = (insert.content_size_bytes, insert.token_count);"
open(p, 'w').write(s.replace(old, new))
PY
verify M2-none-keeps-length --test conversation_content_persistence

echo "== M3: 'encrypted_content' fails OPEN and persists plaintext =="
python3 - <<'PY'
p = 'src/domain/conversation.rs'
s = open(p).read()
old = """    pub const fn persists_plaintext(self) -> bool {
        matches!(self, Self::PlainContent)
    }"""
assert old in s, "M3 anchor not found — the code moved; update this script"
new = """    pub const fn persists_plaintext(self) -> bool {
        matches!(self, Self::PlainContent | Self::EncryptedContent)
    }"""
open(p, 'w').write(s.replace(old, new))
PY
verify M3-encrypted-fails-open --test conversation_content_persistence

echo "== M4: the admin API accepts 'encrypted_content' again =="
python3 - <<'PY'
p = 'src/domain/conversation.rs'
s = open(p).read()
old = """    pub const fn is_enforceable(self) -> bool {
        !matches!(self, Self::EncryptedContent)
    }"""
assert old in s, "M4 anchor not found — the code moved; update this script"
open(p, 'w').write(s.replace(old, """    pub const fn is_enforceable(self) -> bool {
        true
    }"""))
PY
verify M4-refusal-removed --test conversation_content_persistence

echo "== M5: a missing policy row withholds instead of taking the column default =="
python3 - <<'PY'
p = 'src/infra/repositories/conversation.rs'
s = open(p).read()
old = """coalesce(p.conversation_content_persistence, 'plain_content')
                       as content_persistence"""
assert old in s, "M5 anchor not found — the code moved; update this script"
new = """coalesce(p.conversation_content_persistence, 'none')
                       as content_persistence"""
open(p, 'w').write(s.replace(old, new))
PY
verify M5-default-flipped --test conversation_content_persistence

echo "== M6: the summary write ignores the policy =="
python3 - <<'PY'
p = 'src/application/conversation.rs'
s = open(p).read()
old = """        let summary_text = policy
            .conversation_content_persistence
            .persists_plaintext()
            .then_some(summary.text.as_str());"""
assert old in s, "M6 anchor not found — the code moved; update this script"
open(p, 'w').write(s.replace(old, "        let summary_text = Some(summary.text.as_str());"))
PY
verify M6-summary-plaintext-anyway --test conversation_summarization

echo
if [ ${#survivors[@]} -eq 0 ]; then
    echo "ALL MUTATIONS CAUGHT"
    exit 0
fi
echo "SURVIVORS (a guard that cannot fail is an assumption): ${survivors[*]}"
exit 1
