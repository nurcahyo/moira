#!/usr/bin/env bash
# Seed a freshly-migrated Moira with the minimum a prompt needs to execute.
#
# A migrated database is not a runnable one. Migration 0005 seeds the `general`
# route and nothing else, so `POST /api/v1/responses` on a clean install fails
# twice before it can succeed, and the two failures look nothing alike:
#
#   404 credential_not_found  "no eligible provider credential"
#       — a provider credential row is REQUIRED even when the provider needs no
#         secret. A keyless local vLLM still needs one; the value is never sent.
#   403 route_override_forbidden
#       — sending `"route": "general"` in the body is an *override* and needs its
#         own authorisation. Omit it and default routing picks the same route.
#
# This script creates: provider -> provider_model -> credential -> routing_policy,
# all bound to the `general` route. Re-running reuses whatever already matches.
#
# Configuration (all optional):
#   MOIRA_SEED_BASE_URL   OpenAI-compatible base, default http://127.0.0.1:8000/v1
#   MOIRA_SEED_MODEL      model id; default is the first one the box advertises
#   MOIRA_SEED_NAME       provider display name, default "Local OpenAI-compatible"
#
# Requires MOIRA_SYSTEM_KEY in the environment (`make bootstrap-key` writes it to
# `.env`, and every make recipe sources `.env`).
set -euo pipefail
cd "$(dirname "$0")/.."

BASE_URL="${MOIRA_SEED_BASE_URL:-http://127.0.0.1:8000/v1}"
NAME="${MOIRA_SEED_NAME:-Local OpenAI-compatible}"
PORT="${MOIRA_SERVER__PORT:-8080}"
HOST="${MOIRA_SERVER__HOST:-127.0.0.1}"
B="http://$HOST:$PORT"

if [ -z "${MOIRA_SYSTEM_KEY:-}" ]; then
    printf 'seed: MOIRA_SYSTEM_KEY is not set. Run `make bootstrap-key`.\n' >&2
    exit 1
fi
H=(-H "X-Moira-System-Key: $MOIRA_SYSTEM_KEY" -H 'Content-Type: application/json')

# Pull the id out of a create response. Create returns the resource itself; only
# the key-minting endpoints wrap it in `{resource, secret}`.
id_of() { python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])'; }

# Find one row in a `{data: [...], pagination: {...}}` list response by field.
find_by() {
    python3 -c 'import json,sys
field, want = sys.argv[1], sys.argv[2]
rows = json.load(sys.stdin)["data"]
hit = [r for r in rows if r.get(field) == want]
print(hit[0]["id"] if hit else "")' "$1" "$2"
}

step() { printf '\n\033[1m%s\033[0m\n' "$*"; }

MODEL="${MOIRA_SEED_MODEL:-}"
if [ -z "$MODEL" ]; then
    step "0. discover a model from $BASE_URL"
    MODEL=$(curl -fsS -m 10 "$BASE_URL/models" 2>/dev/null \
        | python3 -c 'import json,sys
try:
    d = json.load(sys.stdin)["data"]
except Exception:
    sys.exit(0)
print(d[0]["id"] if d else "")' || true)
    if [ -z "$MODEL" ]; then
        printf '   %s/models advertised nothing. Set MOIRA_SEED_MODEL and re-run.\n' "$BASE_URL" >&2
        exit 1
    fi
    printf '   %s\n' "$MODEL"
fi

step "1. provider  ($BASE_URL)"
PROVIDER_ID=$(curl -fsS "${H[@]}" "$B/api/v1/admin/providers" | find_by display_name "$NAME")
if [ -n "$PROVIDER_ID" ]; then
    printf '   reuse  %s\n' "$PROVIDER_ID"
else
    PROVIDER_ID=$(curl -fsS -X POST "${H[@]}" "$B/api/v1/admin/providers" \
        -d "$(python3 -c 'import json,sys; print(json.dumps({
            "display_name": sys.argv[1],
            "provider_type": "open_ai_compatible",
            "base_url": sys.argv[2]}))' "$NAME" "$BASE_URL")" | id_of)
    printf '   create %s\n' "$PROVIDER_ID"
fi

step "2. provider model  ($MODEL)"
MODEL_ID=$(curl -fsS "${H[@]}" "$B/api/v1/admin/providers/$PROVIDER_ID/models" | find_by model_key "$MODEL")
if [ -n "$MODEL_ID" ]; then
    printf '   reuse  %s\n' "$MODEL_ID"
else
    # `capabilities` carries #[serde(default)] and defaults to Value::Null, so
    # omitting it stores jsonb null rather than "no constraints". A request that
    # asks for a named capability then fails `no_eligible_model` — a message that
    # says nothing about the row being under-specified. Send it explicitly.
    MODEL_ID=$(curl -fsS -X POST "${H[@]}" "$B/api/v1/admin/providers/$PROVIDER_ID/models" \
        -d "$(python3 -c 'import json,sys; print(json.dumps({
            "model_key": sys.argv[1],
            "capabilities": {"text": True, "streaming": True, "tools": True}}))' "$MODEL")" | id_of)
    printf '   create %s\n' "$MODEL_ID"
fi

step "3. credential"
# Required even for a keyless endpoint: routing rejects the request with
# `credential_not_found` before it ever reaches the provider. The value below is
# stored encrypted and, for an OpenAI-compatible box that ignores Authorization,
# never used for anything.
CRED_ID=$(curl -fsS "${H[@]}" "$B/api/v1/admin/provider-credentials" | find_by provider_id "$PROVIDER_ID")
if [ -n "$CRED_ID" ]; then
    printf '   reuse  %s\n' "$CRED_ID"
else
    CRED_ID=$(curl -fsS -X POST "${H[@]}" "$B/api/v1/admin/provider-credentials" \
        -d "$(python3 -c 'import json,sys; print(json.dumps({
            "provider_id": sys.argv[1],
            "credential_type": "api_key",
            "scope": {"type": "global"},
            "secret": {"api_key": "unused-by-a-keyless-endpoint"}}))' "$PROVIDER_ID")" | id_of)
    printf '   create %s\n' "$CRED_ID"
fi

step "4. route  (migration 0005 seeds this one)"
ROUTE_ID=$(curl -fsS "${H[@]}" "$B/api/v1/admin/routes" | find_by route_key general)
if [ -z "$ROUTE_ID" ]; then
    printf '   no `general` route — the database is not the one 0005 migrated\n' >&2
    exit 1
fi
printf '   %s\n' "$ROUTE_ID"

step "5. routing policy"
POLICY_ID=$(curl -fsS "${H[@]}" "$B/api/v1/admin/routing-policies" | python3 -c 'import json,sys
route, provider = sys.argv[1], sys.argv[2]
rows = json.load(sys.stdin)["data"]
hit = [r for r in rows if r.get("route_id") == route and r.get("provider_id") == provider]
print(hit[0]["id"] if hit else "")' "$ROUTE_ID" "$PROVIDER_ID")
if [ -n "$POLICY_ID" ]; then
    printf '   reuse  %s\n' "$POLICY_ID"
else
    POLICY_ID=$(curl -fsS -X POST "${H[@]}" "$B/api/v1/admin/routing-policies" \
        -d "$(python3 -c 'import json,sys; print(json.dumps({
            "route_id": sys.argv[1],
            "provider_id": sys.argv[2],
            "provider_model_id": sys.argv[3],
            "priority": 100,
            "weight": 1}))' "$ROUTE_ID" "$PROVIDER_ID" "$MODEL_ID")" | id_of)
    printf '   create %s\n' "$POLICY_ID"
fi

step "6. application"
# `GET /api/v1/admin/setup/status` reports `application: missing` and
# `executable_path: missing` until one exists — even though an admin-authenticated
# execution already succeeds. The application is what the *consumer* path hangs
# off, and that path is the one a real client uses.
APP_ID=$(curl -fsS "${H[@]}" "$B/api/v1/admin/applications" | find_by application_slug local-test)
if [ -n "$APP_ID" ]; then
    printf '   reuse  %s\n' "$APP_ID"
else
    APP_ID=$(curl -fsS -X POST "${H[@]}" "$B/api/v1/admin/applications" \
        -d '{"display_name": "Local Test App", "application_slug": "local-test"}' | id_of)
    printf '   create %s\n' "$APP_ID"
fi

step "7. consumer key"
# Minted only when `.env` does not already carry one: the secret is printed once,
# so re-minting on every seed would leave a trail of unusable active keys.
if [ -n "${MOIRA_CONSUMER_KEY:-}" ]; then
    printf '   reuse  the MOIRA_CONSUMER_KEY already in .env\n'
elif [ ! -f .env ]; then
    printf '   skip   no .env to store the secret in\n'
else
    SECRET=$(curl -fsS -X POST "${H[@]}" "$B/api/v1/admin/consumer-keys" \
        -d "$(python3 -c 'import json,sys; print(json.dumps({
            "application_id": sys.argv[1],
            "display_name": "Local Test Key",
            "scopes": ["moira:responses:create"]}))' "$APP_ID")" \
        | python3 -c 'import json,sys; print(json.load(sys.stdin)["secret"])')
    printf '\n# Consumer key for the local test application, minted by `make seed`.\n# A real client sends this as X-Consumer-Key; the system key is for admin calls.\nMOIRA_CONSUMER_KEY=%s\n' "$SECRET" >>.env
    chmod 600 .env
    printf '   create and stored in .env as MOIRA_CONSUMER_KEY (not printed)\n'
fi

step "setup status"
curl -fsS "${H[@]}" "$B/api/v1/admin/setup/status" | python3 -c '
import json, sys
d = json.load(sys.stdin)
print("   status: %s" % d["status"])
missing = d.get("missing") or []
if missing:
    print("   missing: %s" % ", ".join(missing))'

printf '\nSeeded. `make smoke` executes a prompt through it.\n'
