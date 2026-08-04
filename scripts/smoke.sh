#!/usr/bin/env bash
# End-to-end smoke test against a running local Moira.
#
# Every check below asserts on a value, not on an exit code. `curl` without `-f`
# exits 0 on a 500, and a DB-backed path can answer 200 having done nothing — so
# the last check is a real completion with real token accounting, which is the
# only one of these that cannot pass while the thing underneath is broken.
#
# `-e` is deliberately absent — every other script here sets it. This one tallies
# failures and reports all of them, so an early `bad` must not abort the run; the
# tally is what sets the exit status at the end.
set -uo pipefail
cd "$(dirname "$0")/.."

PORT="${MOIRA_SERVER__PORT:-8080}"
HOST="${MOIRA_SERVER__HOST:-127.0.0.1}"
B="http://$HOST:$PORT"

pass=0
fail=0
ok()   { printf '  \033[32mok\033[0m    %s\n' "$*"; pass=$((pass + 1)); }
bad()  { printf '  \033[31mFAIL\033[0m  %s\n' "$*"; fail=$((fail + 1)); }

printf '\033[1mmoira smoke — %s\033[0m\n\n' "$B"

# --- liveness -------------------------------------------------------------
body=$(curl -s -m 5 "$B/health/live" 2>/dev/null)
case "$body" in
    *'"status":"ok"'*) ok "health/live" ;;
    "")                bad "health/live — nothing answering on $B"; printf '\nIs the API running? `make run`\n'; exit 1 ;;
    *)                 bad "health/live — answered, but not with Moira's body: ${body:0:120}" ;;
esac

# Readiness is the one that fails when Postgres is gone; liveness would still pass.
body=$(curl -s -m 5 "$B/health/ready" 2>/dev/null)
case "$body" in
    *'"database":"ready"'*) ok "health/ready — database ready" ;;
    *)                      bad "health/ready — ${body:0:160}" ;;
esac

# --- contract -------------------------------------------------------------
# Counting operations rather than checking for a 200: the document is generated
# from the annotated handlers, so a wrong count means routes moved.
counts=$(curl -s -m 15 "$B/openapi.json" 2>/dev/null | python3 -c '
import json, sys
try:
    d = json.load(sys.stdin)
except Exception:
    print("0 0 0"); raise SystemExit
methods = ("get", "post", "put", "patch", "delete", "head", "options")
ops = sum(1 for p in d["paths"].values() for m in p if m in methods)
print(len(d["paths"]), ops, len(d.get("components", {}).get("schemas", {})))' 2>/dev/null)
set -- $counts
if [ "${1:-0}" -gt 0 ]; then
    ok "openapi.json — $1 paths, $2 operations, $3 schemas"
    # Pinned against the operation count, because that is the number the repo
    # already asserts (`src/http/mod.rs`, 100 paths / 152 operations). Keep the
    # two in step: if that test changes, change this.
    [ "$2" = 152 ] || bad "operation count is $2, expected 152 — the contract moved"
else
    bad "openapi.json did not parse"
fi

# --- deployment readiness -------------------------------------------------
# Moira diagnoses itself here, and it is stricter than a green health probe:
# `ready` means every one of database, root system key, application, route,
# provider, model, credential, routing policy and executable path is present.
if [ -n "${MOIRA_SYSTEM_KEY:-}" ]; then
    verdict=$(curl -s -m 10 -H "X-Moira-System-Key: $MOIRA_SYSTEM_KEY" "$B/api/v1/admin/setup/status" 2>/dev/null \
        | python3 -c '
import json, sys
try:
    d = json.load(sys.stdin)
except Exception:
    print("BAD unparseable"); raise SystemExit
if "error" in d:
    print("BAD %s" % d["error"].get("code")); raise SystemExit
missing = d.get("missing") or []
# "run make seed" is only the right advice when rows are actually absent. Saying
# it after an auth failure sends you to re-provision a database that is fine.
print(("OK  " + d.get("status", "?")) if not missing else ("SEED missing: " + ", ".join(missing)))' 2>/dev/null)
    case "$verdict" in
        OK*)   ok  "setup/status — ${verdict#OK  }" ;;
        SEED*) bad "setup/status — ${verdict#SEED } (run \`make seed\`)" ;;
        *)     bad "setup/status — ${verdict#BAD }" ;;
    esac
fi

# --- execution ------------------------------------------------------------
# The real test. Needs MOIRA_SYSTEM_KEY: an anonymous caller has no scopes, so
# this returns 403 `missing required scope moira:responses:create` without it.
if [ -z "${MOIRA_SYSTEM_KEY:-}" ]; then
    bad "execution — MOIRA_SYSTEM_KEY not set; run \`make bootstrap-key\`"
else
    # No "route" field on purpose: sending one is an override and needs its own
    # authorisation (403 route_override_forbidden). Default routing picks the
    # same route the seed script bound.
    out=$(curl -s -m 180 -X POST \
        -H 'Content-Type: application/json' \
        -H "X-Moira-System-Key: $MOIRA_SYSTEM_KEY" \
        "$B/api/v1/responses" \
        -d '{"input":[{"role":"user","content":[{"type":"input_text","text":"Reply with exactly: MOIRA LOCAL OK"}]}],"max_output_tokens":64,"temperature":0}' 2>/dev/null)
    verdict=$(printf '%s' "$out" | python3 -c '
import json, sys
try:
    d = json.load(sys.stdin)
except Exception:
    print("BAD unparseable response"); raise SystemExit
if "error" in d:
    e = d["error"]
    print("BAD %s — %s" % (e.get("code"), e.get("message")))
    raise SystemExit
status = d.get("status")
tokens = (d.get("usage") or {}).get("total_tokens")
model = (d.get("model") or {}).get("key")
text = ""
for item in d.get("output", []):
    for part in item.get("content", []):
        text += part.get("text", "")
if status != "completed":
    print("BAD status=%s" % status)
elif not tokens:
    # A completion that reports no tokens has not been through a provider.
    print("BAD completed but usage.total_tokens is %r" % tokens)
else:
    print("OK  %s — %s tokens — %r" % (model, tokens, text[:60]))' 2>/dev/null)
    case "$verdict" in
        OK*)  ok "execution — ${verdict#OK  }" ;;
        *)    bad "execution — ${verdict#BAD }" ;;
    esac
fi

# The consumer path is the one a real client takes. It is a separate check because
# it can fail on its own: the key carries its own scopes and its own application,
# and an admin key passing proves nothing about it.
if [ -n "${MOIRA_CONSUMER_KEY:-}" ]; then
    out=$(curl -s -m 180 -X POST \
        -H 'Content-Type: application/json' \
        -H "X-Consumer-Key: $MOIRA_CONSUMER_KEY" \
        "$B/api/v1/responses" \
        -d '{"input":[{"role":"user","content":[{"type":"input_text","text":"Say OK"}]}],"max_output_tokens":32,"temperature":0}' 2>/dev/null)
    verdict=$(printf '%s' "$out" | python3 -c '
import json, sys
try:
    d = json.load(sys.stdin)
except Exception:
    print("BAD unparseable"); raise SystemExit
if "error" in d:
    print("BAD %s — %s" % (d["error"].get("code"), d["error"].get("message"))); raise SystemExit
tokens = (d.get("usage") or {}).get("total_tokens")
print("OK  %s tokens" % tokens if d.get("status") == "completed" and tokens else "BAD status=%s tokens=%r" % (d.get("status"), tokens))' 2>/dev/null)
    case "$verdict" in
        OK*) ok "execution as consumer — ${verdict#OK  }" ;;
        *)   bad "execution as consumer — ${verdict#BAD }" ;;
    esac
fi

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[ "$fail" -eq 0 ] || exit 1
printf 'SMOKE PASSED\n'
