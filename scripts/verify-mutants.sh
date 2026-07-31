#!/usr/bin/env bash
# Re-apply each of the nine mutants cargo-mutants reported as MISSED and confirm the tests
# added afterwards now kill them. Reverts the tree after every case, including on failure.
set -uo pipefail

cd /private/tmp/claude-501/-Users-nalhide-Project-motrait-moira/1b00aa10-d2be-4e06-870b-1192d9096979/scratchpad/fsweep
export CARGO_TARGET_DIR="$HOME/.cargo-targets/moira-fsweep"
export MOIRA_TEST_DATABASE_URL="postgres://postgres:postgres@127.0.0.1:5432/moira"

revert() { git checkout -- src/ >/dev/null 2>&1; }
trap revert EXIT

results=()

# $1 label  $2 file  $3 old  $4 new  $5.. cargo test args
check() {
    local label="$1" file="$2" old="$3" new="$4"; shift 4
    revert
    if ! grep -qF -- "$old" "$file"; then
        results+=("NO-MATCH  $label (the source no longer contains the mutated text)")
        return
    fi
    python3 - "$file" "$old" "$new" <<'PY'
import sys
path, old, new = sys.argv[1], sys.argv[2], sys.argv[3]
text = open(path).read()
assert text.count(old) == 1, f"{path}: {text.count(old)} occurrences of the mutated text"
open(path, "w").write(text.replace(old, new))
PY
    if [ $? -ne 0 ]; then
        results+=("AMBIGUOUS $label")
        revert
        return
    fi
    local log; log=$(mktemp)
    if cargo test "$@" >"$log" 2>&1; then
        results+=("SURVIVED  $label  <-- still not caught")
    else
        results+=("KILLED    $label")
    fi
    rm -f "$log"
    revert
}

check "identity.rs:784 set_identity_primary && -> ||" \
  src/application/identity.rs \
  'if !outcome.replayed && outcome.response.is_primary {' \
  'if !outcome.replayed || outcome.response.is_primary {' \
  --test admin_invite_lifecycle --all-features \
  transferring_ownership_moves_the_flag_rather_than_adding_a_second_owner

check "identity.rs:836 revoke_identity delete !" \
  src/application/identity.rs \
  '        // Same rule as every other counter on this path: a replay returns the stored
        // response of the revocation that already happened, and is not a second one.
        if !outcome.replayed {' \
  '        // Same rule as every other counter on this path: a replay returns the stored
        // response of the revocation that already happened, and is not a second one.
        if outcome.replayed {' \
  --test admin_invite_lifecycle --all-features a_replayed_revocation_does_not_count_a_second_one

check "settings.rs:668 < -> <=" \
  src/config/settings.rs \
  'if self.api_keys.prefix_length < MIN_API_KEY_PREFIX_LENGTH {' \
  'if self.api_keys.prefix_length <= MIN_API_KEY_PREFIX_LENGTH {' \
  --lib --all-features the_api_key_prefix_floor_is_the_point

check "api_keys.rs:59 is_registered_key_namespace -> true" \
  src/security/api_keys.rs \
  'pub const fn is_registered_key_namespace(namespace: &str) -> bool {
    let mut index = 0;' \
  'pub const fn is_registered_key_namespace(namespace: &str) -> bool {
    if true { return true; }
    let mut index = 0;' \
  --lib --all-features an_unregistered_namespace_is_not_registered

check "api_keys.rs:70 const_str_eq -> true" \
  src/security/api_keys.rs \
  'const fn const_str_eq(left: &str, right: &str) -> bool {
    let left = left.as_bytes();' \
  'const fn const_str_eq(left: &str, right: &str) -> bool {
    if true { return true; }
    let left = left.as_bytes();' \
  --lib --all-features an_unregistered_namespace_is_not_registered

check "api_keys.rs:76 const_str_eq < -> ==" \
  src/security/api_keys.rs \
  '    let mut index = 0;
    while index < left.len() {
        if left[index] != right[index] {' \
  '    let mut index = 0;
    while index == left.len() {
        if left[index] != right[index] {' \
  --lib --all-features an_unregistered_namespace_is_not_registered

check "admin.rs:2498 duplicate-issuer guard -> true" \
  src/infra/repositories/admin.rs \
  'sqlx::Error::Database(database) if database.is_unique_violation() => AppError::conflict(
            "duplicate_trusted_jwt_issuer",' \
  'sqlx::Error::Database(database) if true || database.is_unique_violation() => AppError::conflict(
            "duplicate_trusted_jwt_issuer",' \
  --test auth_provider_settings --all-features a_non_unique_database_failure_is_not_reported_as_a_duplicate

check "identity.rs:514 set_primary && -> ||" \
  src/infra/repositories/identity.rs \
  'if is_primary && !current.is_primary {' \
  'if is_primary || !current.is_primary {' \
  --test admin_invite_lifecycle --all-features \
  transferring_ownership_moves_the_flag_rather_than_adding_a_second_owner

check "identity.rs:933 already_claimed && -> ||" \
  src/infra/repositories/identity.rs \
  '            database.is_unique_violation()
                && database.constraint() != Some(SINGLE_ACTIVE_PRIMARY_INDEX)' \
  '            database.is_unique_violation()
                || database.constraint() != Some(SINGLE_ACTIVE_PRIMARY_INDEX)' \
  --test identity_claim --all-features a_non_unique_database_failure_is_not_reported_as_already_claimed

printf '\n=== VERIFICATION ===\n'
for line in "${results[@]}"; do printf '%s\n' "$line"; done
