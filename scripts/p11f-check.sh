#!/usr/bin/env bash
# Run cargo from inside a script file so the `rtk` PreToolUse hook cannot rewrite the command
# and replace a redirected log with a one-line summary (HANDOFF §2.2 form 9).
set -uo pipefail
export CARGO_TARGET_DIR="$HOME/.cargo-targets/moira-p11f"
: "${MOIRA_TEST_DATABASE_URL:=postgres://postgres:postgres@127.0.0.1:5432/moira}"
export MOIRA_TEST_DATABASE_URL
"$@"
status=$?
printf '\n[p11f-check exit %s]\n' "$status"
exit "$status"
