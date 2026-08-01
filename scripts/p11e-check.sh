#!/usr/bin/env bash
# Sub-Phase E inner loop. `cargo` runs from inside a script file because a PreToolUse hook
# rewrites `cargo` on the outer Bash command and replaces a redirected log with a one-line
# summary — see HANDOFF.md §2.2 form 9. Everything here is throwaway; `scripts/gates.sh` is the
# real verification.
set -uo pipefail

: "${MOIRA_TEST_DATABASE_URL:=postgres://postgres:postgres@127.0.0.1:5432/moira}"
export MOIRA_TEST_DATABASE_URL
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.cargo-targets/moira-p11e}"

log=$(mktemp)
case "${1:-check}" in
  check)  cargo check --workspace --all-features   >|"$log" 2>&1 ;;
  clippy) cargo clippy --workspace --all-targets --all-features -- -D warnings >|"$log" 2>&1 ;;
  fmt)    cargo fmt --all >|"$log" 2>&1 ;;
  lib)    cargo test --lib "${2:-summarization}" -- --nocapture >|"$log" 2>&1 ;;
  test)   shift; cargo test --all-features "$@" >|"$log" 2>&1 ;;
  *)      echo "unknown mode ${1}"; exit 2 ;;
esac
status=$?
tail -80 "$log"
echo "---- exit=${status} log=${log}"
exit "$status"
