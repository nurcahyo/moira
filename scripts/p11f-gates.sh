#!/usr/bin/env bash
# Thin wrapper so `scripts/gates.sh` runs with this worktree's private target dir and the
# canonical test database URL, from inside a script file (HANDOFF §2.2 form 9).
set -uo pipefail
cd "$(dirname "$0")/.."
export CARGO_TARGET_DIR="$HOME/.cargo-targets/moira-p11f"
export MOIRA_TEST_DATABASE_URL='postgres://postgres:postgres@127.0.0.1:5432/moira'
bash scripts/gates.sh "$@"
status=$?
printf '\n[gates exit %s]\n' "$status"
exit "$status"
