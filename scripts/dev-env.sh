#!/usr/bin/env bash
# Generate the local development environment files.
#
# **Why a script and not a Make recipe.** macOS ships GNU Make 3.81, which has no
# `.ONESHELL` and no `.SHELLFLAGS`: every recipe line is its own shell, so a
# heredoc has to be spliced together with backslash continuations. That is a bad
# place to keep the one file in the repo that holds generated key material.
#
# **Why the binary needs this at all.** `Settings::load` reads `config/default.toml`,
# then `config/local.toml`, then the process environment — it does NOT read a
# `.env` file, because nothing in the dependency tree does dotenv loading. The
# Makefile sources this file into the environment before exec'ing cargo; running
# `cargo run` by hand without sourcing it first gets the defaults instead, which
# is a database-less, insecure-dev-key process that looks like it started fine.
#
# Usage:  scripts/dev-env.sh [--force] [--rotate-keys]
#   --force        regenerate, moving any existing file aside to <file>.bak
#   --rotate-keys  ALSO mint new key material. Destroys access to everything the
#                  current keys sealed — see the note on `carry` below.
#
# Never prints a generated secret. Both files are gitignored.

set -euo pipefail

cd "$(dirname "$0")/.."

FORCE=0
ROTATE=0
for arg in "$@"; do
    case "$arg" in
        --force) FORCE=1 ;;
        --rotate-keys) ROTATE=1 ;;
        *) printf 'dev-env: unknown argument %s\n' "$arg" >&2; exit 2 ;;
    esac
done

# Postgres and Redis are addressed as 127.0.0.1, never `localhost`. On a machine
# where `localhost` resolves to ::1 first, a container published on the IPv4
# address only is unreachable under a name that looks correct in every log line.
PG=postgres://postgres:postgres@127.0.0.1:5432

# `openssl rand -base64 32` is exactly 32 decoded bytes, which is what
# `SecretSettings::master_key_bytes` requires — it rejects any other length
# rather than truncating or padding.
key32() { openssl rand -base64 32; }

# Better Auth signs sessions with this and wants more entropy than a 32-byte key;
# it is not length-checked the way `master_key_bytes` is.
key48() { openssl rand -base64 48; }

# Reuse the key material an existing file already holds.
#
# **What goes wrong without this.** `--force` rewrites `.env`, and every secret in
# it is generated. But `MOIRA_SECRETS__MASTER_KEY_BASE64` is what every
# `provider_credentials` row was sealed with, `MOIRA_API_KEYS__PEPPER_BASE64` is
# what every live API key hash was peppered with, and `BETTER_AUTH_SECRET`
# encrypts the console's stored ES256 signing key. Minting new ones does not
# fail, and does not warn: the old rows simply stop being readable, at use time,
# one endpoint at a time. The `.bak` file is the only copy left.
#
# So `--force` means "rewrite the layout", not "rotate the secrets". Rotation is
# `--rotate-keys`, which is a separate, deliberate act.
carry() {
    local name="$1" file="${2:-.env}" existing=""
    if [ "$ROTATE" -eq 0 ] && [ -f "$file" ]; then
        existing=$(sed -n "s/^$name=//p" "$file" | tail -n 1 | sed 's/^"//; s/"$//')
    fi
    if [ -n "$existing" ]; then printf '%s' "$existing"; else "$3"; fi
}

MASTER_KEY=$(carry MOIRA_SECRETS__MASTER_KEY_BASE64 .env key32)
API_PEPPER=$(carry MOIRA_API_KEYS__PEPPER_BASE64 .env key32)
IDEM_PEPPER=$(carry MOIRA_IDEMPOTENCY__PEPPER_BASE64 .env key32)
AUTH_SECRET=$(carry BETTER_AUTH_SECRET console/.env.local key48)
CONSOLE_SEAL=$(carry CONSOLE_SECRET_ENCRYPTION_KEY console/.env.local key32)

# The API port, resolved ONCE and used by both generated files.
#
# It has to be one value because the failure it prevents is silent. Writing
# `MOIRA_SERVER__PORT` into `.env` and a different `MOIRA_API_URL` into
# `console/.env.local` produces two processes that both start cleanly and never
# speak: the console's every call goes to whatever else is on that port.
#
# And it has to be *chosen*, not fixed at `config/default.toml`'s 8080, because on
# a developer machine 8080 is routinely some other project's dev server. A port
# that answers is worse than one that is closed — `make health` gets a 404 from a
# stranger's app and reports this service down, and the console proxies admin
# calls into it.
pick_port() {
    local p
    for p in "$@"; do
        command -v lsof >/dev/null 2>&1 || { printf '%s' "$p"; return 0; }
        lsof -nP -iTCP:"$p" -sTCP:LISTEN >/dev/null 2>&1 || { printf '%s' "$p"; return 0; }
    done
    printf 'dev-env: no free port among %s — set MOIRA_PORT to one you know is free\n' "$*" >&2
    return 1
}
#
# Precedence: an explicit `MOIRA_PORT`, then whatever `.env` already says, and
# only then a scan for a free one.
#
# The existing value wins even under `--force`, for two reasons. Regenerating one
# file and not the other is the ordinary case — `make env` after deleting
# `console/.env.local` — and it is exactly when the two drift apart. And a scan
# run while the service is up finds its own port occupied and helpfully moves it,
# which relocates a running deployment out from under every client that already
# knows where it lives. `--force` rewrites the layout; it does not move the port.
PORT="${MOIRA_PORT:-}"
if [ -z "$PORT" ] && [ -f .env ]; then
    PORT=$(sed -n 's/^MOIRA_SERVER__PORT=//p' .env | tail -n 1)
fi
[ -n "$PORT" ] || PORT=$(pick_port 8080 8100 8101 8102 8103)

write() {
    local path="$1"
    if [ -e "$path" ] && [ "$FORCE" -eq 0 ]; then
        printf '   skip   %s (exists — `make env-force` to regenerate)\n' "$path"
        return 1
    fi
    if [ -e "$path" ]; then
        mv "$path" "$path.bak"
        printf '   backup %s -> %s.bak\n' "$path" "$path"
    fi
    cat >"$path"
    chmod 600 "$path"
    printf '   write  %s\n' "$path"
    return 0
}

printf 'moira local environment\n'
printf '   port   %s (API and console agree on this one)\n' "$PORT"

# ---------------------------------------------------------------------------
# .env — the Rust service
# ---------------------------------------------------------------------------
write .env <<EOF || true
# Generated by scripts/dev-env.sh. Local development only — never commit.
# Overrides config/default.toml. Nested TOML keys use a double underscore.

MOIRA_DEPLOYMENT__ENVIRONMENT=development
MOIRA_SERVER__HOST=127.0.0.1
MOIRA_SERVER__PORT=$PORT

MOIRA_DATABASE__URL=$PG/moira
MOIRA_DATABASE__REQUIRE=true
MOIRA_DATABASE__MIGRATE_ON_STARTUP=true

# Real 32-byte keys rather than the built-in dev sentinels, so the insecure-dev
# escape hatches stay off and this environment fails the same way production
# would if a key were missing.
MOIRA_SECRETS__MASTER_KEY_BASE64=$MASTER_KEY
MOIRA_SECRETS__KEY_ID=local-dev
MOIRA_SECRETS__ALLOW_INSECURE_DEV_KEY=false

MOIRA_API_KEYS__PEPPER_BASE64=$API_PEPPER
MOIRA_API_KEYS__PEPPER_VERSION=local-dev
MOIRA_API_KEYS__ALLOW_INSECURE_DEV_PEPPER=false
MOIRA_API_KEYS__PREFIX_LENGTH=20

# A separate pepper from the API-key one, keying the idempotency ledger's hashes.
# Omitting it is not a no-op: startup logs "insecure_idempotency_pepper_fallback"
# and the ledger is keyed with a constant every reader of this repo knows.
MOIRA_IDEMPOTENCY__PEPPER_BASE64=$IDEM_PEPPER
MOIRA_IDEMPOTENCY__PEPPER_VERSION=local-dev
MOIRA_IDEMPOTENCY__ALLOW_INSECURE_DEV_PEPPER=false

# Admin bearer auth is off on a cold start: the console is what mints those JWTs
# and it is not running yet. Admin calls authenticate with the system key from
# \`make bootstrap-key\` (header: X-Moira-System-Key) until the console is wired.
MOIRA_AUTH__ADMIN__ENABLED=false
MOIRA_AUTH__CALLER__ENABLED=false
MOIRA_AUTH__CALLER__DEV_TRUST_HEADERS=false

# The console publishes its JWKS on http://localhost:3000, and Moira's
# jwt-issuer path runs a full SSRF check that rejects loopback and plain http.
# Registering the local console as a trusted issuer needs this; production
# validation refuses it outright.
MOIRA_AUTH__JWKS__ALLOW_INSECURE_DEV_URLS=true

MOIRA_CORS__ALLOWED_ORIGINS=http://localhost:3000
MOIRA_DOCS__EXPOSE_ADMIN=false

# Both are needed to register a provider that lives on a private-network http://
# URL — a LAN vLLM box, say. Neither is accepted in production.
MOIRA_PROVIDER_SECURITY__ALLOW_PRIVATE_PROVIDER_URLS=true
MOIRA_PROVIDER_SECURITY__ALLOW_HTTP_PROVIDER_URLS=true

MOIRA_REDIS__ENABLED=false
MOIRA_REDIS__URL=redis://127.0.0.1:6379/0
MOIRA_WORKERS__ENABLED=false

MOIRA_TELEMETRY__PROMETHEUS_ENABLED=true
MOIRA_TELEMETRY__METRICS_PATH=/metrics
MOIRA_TELEMETRY__OTEL_ENABLED=false

# Read by the DB-backed test suites. Pointing this at the wrong database does not
# fail — those suites skip silently and the run still reports green.
MOIRA_TEST_DATABASE_URL=$PG/moira
EOF

# ---------------------------------------------------------------------------
# console/.env.local — the Next.js BFF
# ---------------------------------------------------------------------------
# Next.js loads .env.local itself, so the console needs no equivalent of the
# sourcing the Makefile does for cargo.
write console/.env.local <<EOF || true
# Generated by scripts/dev-env.sh. Local development only — never commit.
# console/lib/env.ts validates every one of these at boot and names what is wrong.

MOIRA_API_URL=http://127.0.0.1:$PORT
CONSOLE_PUBLIC_ORIGIN=http://localhost:3000
MOIRA_ADMIN_API_AUDIENCE=moira-admin

# Signs sessions AND encrypts the better-auth ES256 private key at rest. Changing
# it against a durable CONSOLE_DATABASE_URL orphans the stored signing key — see
# docs/console-storage.md before rotating.
BETTER_AUTH_SECRET=$AUTH_SECRET

# Exactly 32 decoded bytes. Seals the console-owned OAuth client secret.
CONSOLE_SECRET_ENCRYPTION_KEY=$CONSOLE_SEAL

# Permits the http:// loopback origins above. Refused under NODE_ENV=production.
CONSOLE_ALLOW_INSECURE_URLS=true

# The console's OWN database — never Moira's; the two keep independent migration
# ledgers. Create and migrate it with \`make console-db\`. Commented out means the
# ephemeral path: in-memory sessions and a signing key that does not survive a
# restart, which is fine for a first look and wrong for anything else.
# CONSOLE_DATABASE_URL=$PG/moira_console
EOF

printf '\nNext: make up && make migrate && make run\n'
