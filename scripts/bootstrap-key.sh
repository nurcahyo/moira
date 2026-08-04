#!/usr/bin/env bash
# Mint a root system key and persist it into both env files.
#
# **Why this is a script and not just `cargo run -- bootstrap-system-key`.** The
# plaintext secret is printed exactly once — only its Argon2 hash is stored — so a
# secret that scrolls past in a terminal is gone. Rows already in `system_api_keys`
# are not a reason to skip minting: none of them can be recovered.
#
# It lands in two files because two different things need it:
#   .env                 every make recipe sources this, and README's admin-docs
#                        example already reads $MOIRA_SYSTEM_KEY.
#   console/.env.local   the console's bootstrap credential. With no snapshot and
#                        no key, `/login` renders "Sign-in is not available" —
#                        console/lib/auth-runtime.ts:229 reports that deadlock as
#                        itself rather than as an opaque 401 from Moira.
#
# The secret is never echoed. Both files stay 0600 and both are gitignored.
set -euo pipefail
cd "$(dirname "$0")/.."

MINTED=$(mktemp)
trap 'rm -f "$MINTED"' EXIT

# Tracing writes its startup WARN to stdout alongside the JSON, so the payload
# starts at the first `{` rather than at byte 0.
cargo run --quiet -- bootstrap-system-key >"$MINTED"

SECRET=$(python3 -c '
import json, sys
raw = open(sys.argv[1]).read()
i = raw.find("{")
if i < 0:
    sys.exit("bootstrap: no JSON in the CLI output")
d = json.loads(raw[i:])
secret = d["secret"]
if not secret.startswith("moira_sys_"):
    sys.exit("bootstrap: unexpected secret shape")
print(secret)
print(d["resource"]["key_prefix"], file=sys.stderr)' "$MINTED")

persist() {
    local file="$1" note="$2"
    [ -f "$file" ] || { printf '   skip   %s (missing — run `make env`)\n' "$file"; return; }
    # `.keytmp`, not `.bak`: `make env-force` leaves its backup under `.bak` and
    # that file still holds live key material.
    if grep -q '^MOIRA_SYSTEM_KEY=' "$file"; then
        sed -i.keytmp '/^MOIRA_SYSTEM_KEY=/d' "$file" && rm -f "$file.keytmp"
    fi
    printf '\n# %s\nMOIRA_SYSTEM_KEY=%s\n' "$note" "$SECRET" >>"$file"
    chmod 600 "$file"
    printf '   write  %s\n' "$file"
}

printf 'moira system key\n'
persist .env \
    'Root system key from `make bootstrap-key`. Printed once; only its hash is stored.'
persist console/.env.local \
    'Bootstrap credential the console fetches its sign-in configuration with.'

printf '\nMinted and stored. It was not printed — read it back with:\n'
printf '  set -a; . ./.env; set +a; echo "$MOIRA_SYSTEM_KEY"\n'
