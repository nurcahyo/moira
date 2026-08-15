#!/usr/bin/env bash
# Bring up the local Keycloak dev IdP (issue #260): generate a self-signed TLS
# cert if one is not already present, start the `dev-idp` compose profile, and
# block until the realm's discovery document actually answers — not until the
# container reports "started", which happens well before the realm import and
# the internal HTTPS listener are ready.
#
# **Why a script and not a `make` one-liner.** Cert generation needs a presence
# check and readiness needs a poll loop; GNU Make 3.81 (what macOS ships) has no
# clean way to express either without moving them here — the same reason
# `scripts/dev-env.sh` exists instead of a Make recipe.
#
# **Why the cert must exist before `docker compose up`.** The compose service
# bind-mounts `deploy/keycloak/tls.crt` and `tls.key` as individual files. A
# bind mount of a file that does not yet exist on the host creates an empty
# DIRECTORY at that path inside the container instead of erroring, which is a
# silent way to make Keycloak's HTTPS listener fail to start. So the cert is
# generated (or confirmed present) strictly before the container starts.
#
# Usage: scripts/keycloak-dev.sh [--down]
#   (no args)  generate the cert if absent, start the dev-idp profile, wait
#              for the realm to answer, print the seeded logins
#   --down     stop the dev-idp profile (the generated cert is left in place)
#
# Never commits key material: both files are covered by .gitignore, and this
# script never prints their contents.

set -euo pipefail
cd "$(dirname "$0")/.."

CERT_DIR=deploy/keycloak
CERT_FILE="$CERT_DIR/tls.crt"
KEY_FILE="$CERT_DIR/tls.key"
REALM_URL="https://127.0.0.1:8443/realms/moira/.well-known/openid-configuration"
# First boot imports the realm and stands up the HTTPS listener; 2-4 minutes is
# typical on a laptop. Polled at 1s, so this is a ceiling, not a wait.
MAX_ATTEMPTS=300

if [ "${1:-}" = "--down" ]; then
    printf 'compose  stopping the dev-idp profile (cert kept)\n'
    docker compose --profile dev-idp down
    exit 0
fi

if [ -f "$CERT_FILE" ] && [ -f "$KEY_FILE" ]; then
    printf 'cert     present  %s\n' "$CERT_FILE"
else
    printf 'cert     generating a self-signed cert for localhost/127.0.0.1 (dev only, not committed)\n'
    mkdir -p "$CERT_DIR"
    openssl req -x509 -newkey rsa:2048 -nodes -days 3650 \
        -keyout "$KEY_FILE" -out "$CERT_FILE" \
        -subj "/CN=localhost/O=Moira local dev" \
        -addext "subjectAltName=DNS:localhost,IP:127.0.0.1" \
        >/dev/null 2>&1
    chmod 600 "$KEY_FILE"
    printf 'cert     written  %s / %s\n' "$CERT_FILE" "$KEY_FILE"
fi

printf 'compose  starting the dev-idp profile (quay.io/keycloak/keycloak:26.4)\n'
docker compose --profile dev-idp up -d

printf 'realm    waiting for %s\n' "$REALM_URL"
for ((attempt = 1; attempt <= MAX_ATTEMPTS; attempt += 1)); do
    # `-k`: the cert above is self-signed and this loop is asking "has Keycloak
    # finished booting", not validating trust — the browser and the console do
    # that separately, and docs/local-testing.md says what each of them needs.
    code=$(curl -sk -o /dev/null -w '%{http_code}' -m 5 "$REALM_URL" 2>/dev/null || true)
    if [ "$code" = "200" ]; then
        printf 'realm    ready (%ds)\n\n' "$attempt"
        printf 'Keycloak:       https://127.0.0.1:8443  (admin console at /admin, admin/admin)\n'
        printf 'Realm:          moira\n'
        printf 'Client:         moira-console / moira-local-dev-secret (published dev value)\n'
        printf 'Users:          owner/owner (owner@moira.local), operator/operator (operator@moira.local)\n'
        printf '\nSee docs/local-testing.md for the provisioning call and how to sign in.\n'
        exit 0
    fi
    if [ $((attempt % 15)) -eq 0 ]; then
        printf '         still waiting (%ds elapsed, last status %s)\n' "$attempt" "${code:-none}"
    fi
    sleep 1
done

printf 'realm    FAILED — %s did not return 200 within %ds\n' "$REALM_URL" "$MAX_ATTEMPTS" >&2
printf '         check `docker compose logs keycloak`\n' >&2
exit 1
