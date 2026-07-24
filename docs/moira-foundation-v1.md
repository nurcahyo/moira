# Moira Foundation V1

Moira is an API-first orchestration layer over provider execution primitives. Phase 1 stores runtime provider configuration and encrypted provider credentials in PostgreSQL, exposes scoped admin/security APIs, and establishes authentication, authorization, audit, and idempotency foundations.

## Runtime Configuration

Static process configuration comes from `config/default.toml`, optional `config/local.toml`, and `MOIRA__` environment variables. Applications, provider endpoints, provider models, encrypted credentials, system keys, consumer keys, trusted JWT issuers, audit logs, and idempotency records live in PostgreSQL and are managed through `/api/v1/admin`.

The initial local vLLM target is still useful for development, but Phase 1 blocks HTTP and private provider URLs by default. Enable both opt-ins only for trusted local development:

```text
provider_type = openai_compatible
base_url = http://192.168.1.13:8000
model_key = Qwen/Qwen3-4B
MOIRA_PROVIDER_SECURITY__ALLOW_HTTP_PROVIDER_URLS=true
MOIRA_PROVIDER_SECURITY__ALLOW_PRIVATE_PROVIDER_URLS=true
```

## Credential Resolution

Credential resolution is founded on stored encrypted credentials only. Raw provider-key request overrides are rejected.

When execution phases use the resolver, credentials are selected in this order:

1. Explicit authorized stored credential ID
2. User + application + tenant
3. User + application
4. User + tenant
5. User
6. Application + tenant
7. Application
8. Tenant
9. Global

Credentials are encrypted with AES-256-GCM using `MOIRA_SECRETS__MASTER_KEY_BASE64`. The authenticated associated data binds credential id, provider id, credential type, scope type, identity bindings, and encryption version.

## APIs

Admin routes are under `/api/v1/admin` and require a trusted bearer JWT, system API key, or allowed consumer API key plus endpoint-specific scopes. `moira:admin` implies administrative scopes for bearer/system actors only; consumer keys never receive that implication.

Phase 1 routes include:

```text
GET    /health/live
GET    /health/ready
GET    /openapi.json
GET    /docs
POST   /api/v1/admin/applications
POST   /api/v1/admin/providers
POST   /api/v1/admin/providers/{provider_id}/models
POST   /api/v1/admin/provider-credentials
POST   /api/v1/admin/system-keys
POST   /api/v1/admin/consumer-keys
POST   /api/v1/admin/jwt-issuers
GET    /api/v1/admin/audit-events
```

System and consumer API keys return raw key material only on create or rotate. Provider credentials never return plaintext.

## Local Run

```bash
docker compose up -d postgres
export MOIRA_DATABASE__URL=postgres://postgres:postgres@localhost:5432/moira
export MOIRA_DATABASE__REQUIRE=true
export MOIRA_SECRETS__MASTER_KEY_BASE64="$(openssl rand -base64 32)"
export MOIRA_API_KEYS__PEPPER_BASE64="$(openssl rand -base64 32)"
cargo run
```

Create a provider in local dev mode when admin auth is disabled:

```bash
curl -X POST http://127.0.0.1:8080/api/v1/admin/providers \
  -H 'content-type: application/json' \
  -d '{
    "display_name": "local-vllm-qwen3",
    "provider_type": "openai_compatible",
    "base_url": "http://192.168.1.13:8000",
    "metadata": {}
  }'
```

## Verification

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Migration validation is env-gated for local Postgres:

```bash
export MOIRA_TEST_DATABASE_URL=postgres://postgres:postgres@localhost:5432/moira
cargo test security_foundation_migration_creates_contract_tables_when_configured
```
