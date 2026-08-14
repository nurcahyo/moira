#!/usr/bin/env bash
# A REAL R1 and a REAL R2 against the local database, through the real binary.
#
# ============================================================================================
# Why this target exists at all
# ============================================================================================
#
# `docs/decision-encryption-at-rest.md` §12 asks for it in one line, and the reason is worth
# more than a line: **rotation code is usually broken the first time it is needed.** It is run
# once every few years, by a tired person, under pressure, on the one path nothing exercises.
# The rotation suite proves the functions work; this proves the *binary* works — argument
# parsing, the process mode, settings loading, custody construction, the output an operator
# actually reads. A `cargo test` cannot fail on a mistyped verb name in `src/main.rs`.
#
# So: `make rotate-keys` performs a data-key rotation and a master-key rotation on the local
# database, end to end, and refuses to report success unless the database actually changed.
#
# ============================================================================================
# What it does, and what it does NOT do
# ============================================================================================
#
# R1 (`add` then `promote`) is permanent and harmless: it mints a new content data key and
# makes it active. Rows already written stay under the previous key and stay readable forever
# — that is what `retiring` means. Your local database ends up with one more keyring row than
# it started with, and nothing else changes.
#
# R2 (`rewrap`) is performed onto a **throwaway master key generated here**, and then rewrapped
# straight back onto the one your `.env` configures. So the keyring ends the run wrapped
# exactly as it began, and no edit to `.env` is needed or made. Both directions are real
# rewraps against the real database; the second is not a rollback, it is a second rotation.
#
# **It never touches a `*_encrypted` column.** Neither verb does. That is the property being
# demonstrated as much as the rotation itself.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

if [ -z "${MOIRA_CONTENT_ENCRYPTION__KEYS:-}" ] || [ -z "${MOIRA_CONTENT_ENCRYPTION__ACTIVE_KEY_ID:-}" ]; then
    printf 'FAILED — MOIRA_CONTENT_ENCRYPTION__KEYS and MOIRA_CONTENT_ENCRYPTION__ACTIVE_KEY_ID\n' >&2
    printf '         must be set. Run `make env` to generate a .env, then `make rotate-keys`\n' >&2
    printf '         (the Makefile sources .env for you).\n' >&2
    exit 1
fi
CONFIGURED_ACTIVE="$MOIRA_CONTENT_ENCRYPTION__ACTIVE_KEY_ID"

# A throwaway master key, real and random, generated per run. It exists for the length of this
# script and is never written anywhere.
SCRATCH_ID="rotate-keys-scratch"
SCRATCH_KEY="$(head -c 32 /dev/urandom | base64)"

moira() {
    # `--quiet` so the operator sees the verb's own output rather than cargo's. Built once by
    # the caller (`make rotate-keys` depends on `build`), so this is not a compile per verb.
    cargo run --quiet -- keyring "$@"
}

# The keyring's shape, as a stable string, so "the database actually changed" is asserted
# rather than assumed. Reads through the CLI, not through psql: if `status` is wrong, this
# script must not be able to paper over it with a query of its own.
keyring_digest() {
    # `|| true` on the grep, and it is not cosmetic. `set -o pipefail` is on, and an empty
    # keyring — which is what a fresh local database has — makes `grep` exit 1 for the *good*
    # outcome. That is the same defect `scripts/gates.sh` documents in its header, where
    # `cargo test … | grep -c skipping` returned 1 for zero matches and made a green run look
    # failed. Here it made this script exit before it rotated anything.
    moira status \
        | { grep -E '^v[0-9]+ |^  state|^  master key id' || true; } \
        | shasum | cut -d' ' -f1
}

step() { printf '\n\033[1m── %s\033[0m\n' "$1"; }
fail() { printf '\nFAILED — %s\n' "$1" >&2; exit 1; }

step "before"
moira status

# A fresh local database has an empty keyring, and an R1 against an empty keyring has nothing
# to demote — so it would exercise the promotion but not the *rotation*. Seed one first, so
# the R1 below always goes through `active -> retiring` and this target means the same thing
# on a new checkout as on a database that has been running for months.
if ! moira status | grep -qE '^v[0-9]+ '; then
    step "the keyring is empty — minting a first key so the R1 below is a real rotation"
    SEED="$(moira add | sed -n 's/^minted data key \(.*\)$/\1/p')"
    [ -n "$SEED" ] || fail "\`keyring add\` did not name the key it minted"
    moira promote "$SEED" >/dev/null
fi
BEFORE="$(keyring_digest)"

# ------------------------------------------------------------------------------------------
# R1 — data-key rotation. No config change, no restart, nothing re-encrypted.
# ------------------------------------------------------------------------------------------
step "R1: mint a data key (moira keyring add)"
ADDED="$(moira add | tee /dev/stderr | sed -n 's/^minted data key \(.*\)$/\1/p')"
[ -n "$ADDED" ] || fail "\`keyring add\` did not name the key it minted"

step "R1: promote it (moira keyring promote $ADDED)"
moira promote "$ADDED" | tee /dev/stderr | grep -q '^demoted ' \
    || fail "the promotion demoted nothing; R1 did not rotate anything"

AFTER_R1="$(keyring_digest)"
[ "$BEFORE" != "$AFTER_R1" ] || fail "R1 reported success and changed nothing in the keyring"
moira status | grep -qE "^v[0-9]+ $ADDED" || fail "the promoted key is not in \`keyring status\`"

# ------------------------------------------------------------------------------------------
# R2 — master-key rotation. Step 1 is the config change; steps 2 and 4 are the rewraps.
# ------------------------------------------------------------------------------------------
step "R2: rewrap onto a throwaway master key (moira keyring rewrap --to $SCRATCH_ID)"
# Exactly the R2 step-2 configuration: BOTH master keys held, ACTIVE_KEY_ID deliberately left
# alone. That combination is why `rewrap` has to be able to wrap under a key that is not the
# active one.
MOIRA_CONTENT_ENCRYPTION__KEYS="$MOIRA_CONTENT_ENCRYPTION__KEYS,$SCRATCH_ID:$SCRATCH_KEY" \
    moira rewrap --to "$SCRATCH_ID"

AFTER_R2="$(keyring_digest)"
[ "$AFTER_R1" != "$AFTER_R2" ] || fail "R2 reported success and changed no master_key_id"
MOIRA_CONTENT_ENCRYPTION__KEYS="$MOIRA_CONTENT_ENCRYPTION__KEYS,$SCRATCH_ID:$SCRATCH_KEY" \
    moira status | grep -q "$SCRATCH_ID" \
    || fail "no keyring row names the master key R2 claims to have rewrapped onto"

# The ordering guard, demonstrated rather than described: with the scratch key dropped from
# KEYS — an operator who skipped step 2 and went straight to step 4 — the keyring can no
# longer be opened, and the *next* thing that touches it must refuse.
step "R2: the ordering guard (the scratch master key is now absent)"
# The rewrap alone, with nothing before it in the `if`: a `cmd_a && cmd_b` here would report
# "refused" whenever cmd_a failed, without cmd_b ever having run — the guard-that-never-fired
# shape this repository keeps cataloguing.
guard_output="$(moira rewrap --to "$CONFIGURED_ACTIVE" 2>&1)" && guard_status=0 || guard_status=$?
[ "$guard_status" -ne 0 ] || fail "a rewrap succeeded while the keyring named a master key \
this process does not hold; the ordering guard is not enforced"
printf '%s\n' "$guard_output" | grep -q "$SCRATCH_ID" \
    || fail "the ordering refusal does not name the master key that is missing"
printf '   refused, as it must, naming %s: dropping a master key before re-wrapping is\n' "$SCRATCH_ID"
printf '   caught here rather than becoming an unreadable keyring at the next restart.\n'

step "R2: rewrap back onto $CONFIGURED_ACTIVE, so .env needs no edit"
MOIRA_CONTENT_ENCRYPTION__KEYS="$MOIRA_CONTENT_ENCRYPTION__KEYS,$SCRATCH_ID:$SCRATCH_KEY" \
    moira rewrap --to "$CONFIGURED_ACTIVE"

step "after"
moira status
AFTER="$(keyring_digest)"
[ "$AFTER" = "$AFTER_R1" ] || fail "the keyring did not come back to the configured master key"

printf '\n\033[1mR1 and R2 both completed against the local database.\033[0m\n'
printf '  * data key %s was minted and promoted; the key it replaced is now\n' "$ADDED"
printf '    `retiring`, and rows written under it stay readable forever.\n'
printf '  * the keyring was re-wrapped onto a second master key and back again.\n'
printf '  * NOT ONE ROW of any *_encrypted column was read or written by either.\n'
