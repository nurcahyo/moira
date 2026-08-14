#!/usr/bin/env bash
# Wave-0 spike: prove a local Claude-subscription sidecar works end to end.
#
# LOCAL-ONLY. NEVER RUN IN CI. NEVER COMMIT A TOKEN.
#
# See docs/claude-subscription-sidecar.md for the full runbook this script is
# part of, and plans/12-feature-expansion-brainstorm.md §1 for the decision
# record ("Testing policy for this workstream") that authorizes running this
# against a real subscription session on a human's own machine.
#
# What this does, in two independent stages:
#
#   1. ALWAYS: smoke-test a running sidecar directly — GET /v1/models, then a
#      trivial POST /v1/chat/completions — the same two calls Moira's own
#      OpenAI-compatible discovery and completion paths make. Requires only
#      SIDECAR_BASE_URL.
#
#   2. OPTIONAL, only if MOIRA_SYSTEM_KEY is set: also prove the credential
#      storage half — POST the token from CLAUDE_SUBSCRIPTION_TOKEN to Moira's
#      EXISTING provider-credentials endpoint as credential_type "oauth2",
#      exactly what the console's "Connect Claude subscription" panel does.
#      This does NOT wire the credential into execution (see the doc — it is
#      storage-only today), so it does not attempt a completion through it.
#
# Neither stage sends a real prompt to Anthropic through Moira's OWN Anthropic
# credential path — that path is unchanged and still api_key-only. The
# completion in stage 1 goes straight to the sidecar, which is the thing
# actually holding the authenticated subscription session.
set -uo pipefail
cd "$(dirname "$0")/.."

if [ "${CI:-}" = "true" ]; then
    printf 'claude-subscription-sidecar-spike: refusing to run with CI=true.\n' >&2
    printf 'This script exercises a real local sidecar and a real subscription\n' >&2
    printf 'session. It has no place in an automated pipeline. See\n' >&2
    printf 'docs/claude-subscription-sidecar.md for what CI covers instead.\n' >&2
    exit 1
fi

pass=0
fail=0
ok()  { printf '  \033[32mok\033[0m    %s\n' "$*"; pass=$((pass + 1)); }
bad() { printf '  \033[31mFAIL\033[0m  %s\n' "$*"; fail=$((fail + 1)); }

SIDECAR_BASE_URL="${SIDECAR_BASE_URL:-}"
if [ -z "$SIDECAR_BASE_URL" ]; then
    printf 'claude-subscription-sidecar-spike: SIDECAR_BASE_URL is not set.\n' >&2
    printf 'Point it at your running sidecar, e.g.:\n' >&2
    printf '  SIDECAR_BASE_URL=http://127.0.0.1:8317/v1 %s\n' "$0" >&2
    exit 1
fi
SIDECAR_BASE_URL="${SIDECAR_BASE_URL%/}"

printf '\033[1mclaude-subscription-sidecar-spike — %s\033[0m\n\n' "$SIDECAR_BASE_URL"

# --- stage 1: the sidecar itself, directly ---------------------------------
#
# Never logs the response body in full: a real sidecar's /v1/models or
# /v1/chat/completions body could in principle echo back something the
# operator would not want in a terminal scrollback shared later. Only shape
# and a bounded excerpt are printed.

models_body=$(curl -s -m 10 "$SIDECAR_BASE_URL/models" 2>/dev/null)
case "$models_body" in
    *'"data"'*'"id"'*) ok "sidecar GET /models — reports a model list" ;;
    "")                bad "sidecar GET /models — nothing answered at $SIDECAR_BASE_URL"; printf '\nIs the sidecar running?\n'; exit 1 ;;
    *)                 bad "sidecar GET /models — unexpected shape: ${models_body:0:120}" ;;
esac

model_id=$(printf '%s' "$models_body" | python3 -c '
import json, sys
try:
    d = json.load(sys.stdin)
    ids = [row.get("id") for row in d.get("data", []) if isinstance(row, dict) and row.get("id")]
    print(ids[0] if ids else "")
except Exception:
    print("")' 2>/dev/null)

if [ -z "$model_id" ]; then
    bad "sidecar GET /models — could not extract a model id to complete against"
else
    completion_body=$(curl -s -m 60 -X POST \
        -H 'Content-Type: application/json' \
        "$SIDECAR_BASE_URL/chat/completions" \
        -d "$(python3 -c 'import json,sys; print(json.dumps({
            "model": sys.argv[1],
            "messages": [{"role": "user", "content": "Reply with exactly: SIDECAR SPIKE OK"}],
            "max_tokens": 32,
            "temperature": 0,
        }))' "$model_id")" 2>/dev/null)
    verdict=$(printf '%s' "$completion_body" | python3 -c '
import json, sys
try:
    d = json.load(sys.stdin)
except Exception:
    print("BAD unparseable response"); raise SystemExit
if "error" in d:
    print("BAD %s" % json.dumps(d["error"])[:160]); raise SystemExit
choices = d.get("choices") or []
text = ""
if choices:
    text = (choices[0].get("message") or {}).get("content", "")
usage = d.get("usage") or {}
if not text:
    print("BAD completion has no message content")
else:
    print("OK  %r — usage=%s" % (text[:60], usage))' 2>/dev/null)
    case "$verdict" in
        OK*)  ok "sidecar POST /chat/completions ($model_id) — ${verdict#OK  }" ;;
        *)    bad "sidecar POST /chat/completions ($model_id) — ${verdict#BAD }" ;;
    esac
fi

# --- stage 2 (optional): store the subscription token as an oauth2 credential
#
# Mirrors exactly what app/api/settings/llm/claude-subscription/route.ts does
# server-side: find-or-create the dedicated `anthropic` provider row, then
# create-or-rotate an `oauth2` credential on it. Skipped entirely unless both
# MOIRA_SYSTEM_KEY and CLAUDE_SUBSCRIPTION_TOKEN are set, so a plain sidecar
# smoke test never needs a Moira instance running at all.
if [ -n "${MOIRA_SYSTEM_KEY:-}" ] && [ -n "${CLAUDE_SUBSCRIPTION_TOKEN:-}" ]; then
    PORT="${MOIRA_SERVER__PORT:-8080}"
    HOST="${MOIRA_SERVER__HOST:-127.0.0.1}"
    B="http://$HOST:$PORT"
    H=(-H "X-Moira-System-Key: $MOIRA_SYSTEM_KEY" -H 'Content-Type: application/json')
    NAME='Claude subscription (sidecar)'
    PAGE="limit=200"

    id_of() { python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])'; }
    find_by() {
        # $1 = list body, $2 = python expression over one row `r` (a dict)
        python3 -c '
import json, sys
d = json.loads(sys.argv[1])
for r in d.get("data", []):
    if eval(sys.argv[2], {}, {"r": r}):
        print(r["id"]); raise SystemExit
print("")' "$1" "$2"
    }

    providers=$(curl -s -m 10 "${H[@]}" "$B/api/v1/admin/providers?$PAGE")
    provider_id=$(find_by "$providers" 'r.get("provider_type") == "anthropic" and r.get("display_name") == "'"$NAME"'" and r.get("status") != "deleted"')
    if [ -z "$provider_id" ]; then
        provider_id=$(curl -s -m 10 -X POST "${H[@]}" "$B/api/v1/admin/providers" \
            -d "$(python3 -c 'import json,sys; print(json.dumps({"provider_type": "anthropic", "display_name": sys.argv[1]}))' "$NAME")" \
            | id_of)
        [ -n "$provider_id" ] && ok "Moira provider — created ($provider_id)" || bad "Moira provider — create failed"
    else
        ok "Moira provider — reused ($provider_id)"
    fi

    if [ -n "$provider_id" ]; then
        creds=$(curl -s -m 10 "${H[@]}" "$B/api/v1/admin/provider-credentials?provider_id=$provider_id&$PAGE")
        credential_id=$(find_by "$creds" 'r.get("credential_type") == "oauth2" and r.get("status") != "deleted"')
        if [ -z "$credential_id" ]; then
            result=$(curl -s -m 10 -X POST "${H[@]}" "$B/api/v1/admin/provider-credentials" \
                -d "$(python3 -c 'import json,sys; print(json.dumps({
                    "provider_id": sys.argv[1],
                    "credential_type": "oauth2",
                    "scope": {"type": "global"},
                    "secret": {"access_token": sys.argv[2]},
                    "display_name": "Claude subscription token",
                }))' "$provider_id" "$CLAUDE_SUBSCRIPTION_TOKEN")")
            credential_id=$(printf '%s' "$result" | id_of 2>/dev/null || true)
            [ -n "$credential_id" ] && ok "Moira oauth2 credential — created ($credential_id)" || bad "Moira oauth2 credential — create failed"
        else
            version=$(python3 -c 'import json,sys; d=json.loads(sys.argv[1]); [print(r["version"]) for r in d.get("data", []) if r.get("id") == sys.argv[2]]' "$creds" "$credential_id")
            result=$(curl -s -m 10 -X POST "${H[@]}" -H "If-Match: \"$version\"" \
                "$B/api/v1/admin/provider-credentials/$credential_id/rotate" \
                -d "$(python3 -c 'import json,sys; print(json.dumps({"secret": {"access_token": sys.argv[1]}}))' "$CLAUDE_SUBSCRIPTION_TOKEN")")
            rotated_id=$(printf '%s' "$result" | id_of 2>/dev/null || true)
            [ -n "$rotated_id" ] && ok "Moira oauth2 credential — rotated in place ($rotated_id)" || bad "Moira oauth2 credential — rotate failed"
        fi
    fi

    printf '\nNote: this credential is storage-only as of this change — it is not\n'
    printf 'wired to any provider_models/routing_policies row, so nothing above\n'
    printf 'routes a completion through it. See docs/claude-subscription-sidecar.md.\n'
else
    printf '\n(skipping the Moira credential-storage stage — set MOIRA_SYSTEM_KEY and\n'
    printf ' CLAUDE_SUBSCRIPTION_TOKEN to also exercise it)\n'
fi

printf '\n\033[1m%d passed, %d failed\033[0m\n' "$pass" "$fail"
[ "$fail" -eq 0 ]
