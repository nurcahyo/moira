# Moira

Moira is a Rust/Axum AI orchestration platform scaffold built around the official Rig ecosystem. Moira owns runtime configuration, identity claims, credentials, routing decisions, audit, and streaming boundaries; Rig owns provider execution primitives.

Phase 1 establishes the security foundation, Phase 2 adds the administrative runtime-configuration API, Phase 3 adds the internal provider runtime/model-routing/Rig execution kernel, Phase 4 adds the public Responses API, Phase 5 adds the conversation, memory, and RAG foundation, and Phase 6 adds the production hardening foundation:

```text
PostgreSQL schema
UUIDv7 internal IDs
opaque external identity IDs
encrypted provider credentials
Argon2id system and consumer API keys
database-backed trusted JWT issuers
deny-by-default authorization scopes
append-only audit logs
idempotency records
generated OpenAPI 3.1 for all registered API operations
Phase 2 admin APIs for applications, providers, provider models, credentials, keys, JWT issuers, and audit events
Phase 3 admin APIs for routes, routing policies, agent profiles, provider runtime policies, and disabled-by-default runtime diagnostics
Internal execution attempts, usage records, runtime events, concurrency limits, circuit breakers, retries, and fallback
Public `/api/v1/responses`, `/api/v1/responses/stream`, execution, usage, model, route, and capability APIs
Optional `/v1/responses` compatibility adapter, disabled by default
Application execution policies for public runtime limits and overrides
Conversation, message, memory, policy, RAG collection/document, context-plan, retrieval-run, and pgvector embedding schema
Public conversation and explicit memory APIs with policy-controlled response conversation attachment
Admin conversation, memory, retrieval, embedding policy, and RAG collection/document APIs
Optional Redis readiness, worker supervisor scaffold, Prometheus metrics, production Dockerfile, Kubernetes manifests, Helm chart, and CI quality gates
```

Admin APIs live under `/api/v1/admin`. The generated OpenAPI document is served from `/openapi.json`, with Scalar at `/docs`.

## Prerequisites

- Rust 1.97 or newer
- Docker and Docker Compose for local PostgreSQL/pgvector and optional Redis
- OpenSSL for the local secret-generation examples

## Local Service Setup

Start PostgreSQL:

```bash
docker compose up -d postgres
```

Start Redis only when testing distributed runtime-cache invalidation, workers, or Redis readiness:

```bash
docker compose up -d redis
export MOIRA_REDIS__ENABLED=true
export MOIRA_REDIS__URL=redis://localhost:6379/0
```

## Configuration Setup

Moira loads `config/default.toml`, then optional `config/local.toml`, then `MOIRA__` environment variables. For local development, exporting env vars keeps secrets out of the repository:

```bash
export MOIRA_DATABASE__URL=postgres://postgres:postgres@localhost:5432/moira
export MOIRA_DATABASE__REQUIRE=true
export MOIRA_SECRETS__MASTER_KEY_BASE64="$(openssl rand -base64 32)"
export MOIRA_SECRETS__KEY_ID=local-dev
export MOIRA_API_KEYS__PEPPER_BASE64="$(openssl rand -base64 32)"
export MOIRA_API_KEYS__PEPPER_VERSION=local-dev
```

`.env.example` lists the supported local environment variables for shells, containers, and process managers. The Moira binary does not load `.env` automatically; if you use one, export it before running the process:

```bash
set -a
source .env
set +a
```

For trusted local provider endpoints such as vLLM on localhost or a private LAN address, opt in explicitly:

```bash
export MOIRA_PROVIDER_SECURITY__ALLOW_HTTP_PROVIDER_URLS=true
export MOIRA_PROVIDER_SECURITY__ALLOW_PRIVATE_PROVIDER_URLS=true
```

Do not enable those provider URL relaxations in production.

## Run Locally

Run the API:

```bash
cargo run
```

Development applies migrations during startup by default. Production requires
`MOIRA_DATABASE__MIGRATE_ON_STARTUP=false`; run `cargo run -- migrate` as a
controlled release step before starting the API. The default listener is
`http://127.0.0.1:8080`.

Smoke-check the process:

```bash
curl http://127.0.0.1:8080/health/live
curl http://127.0.0.1:8080/health/ready
curl http://127.0.0.1:8080/openapi.json
```

## API Documentation

Open `http://127.0.0.1:8080/docs` for the interactive Scalar reference, or fetch the OpenAPI 3.1 document directly:

```bash
curl http://127.0.0.1:8080/openapi.json
```

Moira generates the contract from the same annotated handlers registered through `utoipa_axum::OpenApiRouter`. The document covers all 130 operations across health, readiness, Prometheus metrics, documentation, native and streaming responses, execution and usage history, discovery, OpenAI compatibility, conversations, messages, memory, policies, RAG, and administration.

The generated operations include:

- exact path and query parameters
- JSON request and response schemas
- success and typed error responses
- `ETag`, `If-Match`, and `Idempotency-Key` where supported
- the global `X-Request-Id` request parameter and response header
- JSON, SSE, Prometheus, and HTML content types
- bearer JWT, `X-Moira-System-Key`, and `X-Consumer-Key` security alternatives
- once-only API-key secret envelopes without exposing stored hashes or encrypted credential material
- keyed user-facing API messages with English fallbacks for curl, Postman, and UI translation layers

Admin paths are filtered from the public document by default. To request the complete document, enable admin exposure and authenticate with a caller authorized for `moira:admin`:

```bash
export MOIRA_DOCS__EXPOSE_ADMIN=true
curl \
  -H "X-Moira-System-Key: $MOIRA_SYSTEM_KEY" \
  http://127.0.0.1:8080/openapi.json
```

When adding or changing an endpoint, follow `.agents/skills/moira-openapi/SKILL.md` for Codex and Antigravity or `.claude/skills/moira-openapi/SKILL.md` for Claude Code. Contract tests enforce complete route and method coverage, unique operation IDs, resolvable schemas, public admin filtering, security schemes, statuses, parameters, headers, and content types.

See [docs/openapi.md](docs/openapi.md) for the contract maintenance rules.

## Administrative Setup

Minimal administrative setup:

```text
1. Apply migrations
2. Create the initial system key with `cargo run -- bootstrap-system-key`
3. Create an application
4. Create a provider
5. Create a provider model
6. Add an encrypted provider credential
7. Create a route definition and routing policy
8. Optionally tune provider runtime policy
9. Tune application execution policy for public response limits
10. Configure a trusted JWT issuer if bearer auth is needed
11. Create a consumer key for application-scoped public callers
12. Test the internal execution kernel with `cargo run -- execute-test -- --prompt "Hello" --route general`
13. Call `POST /api/v1/responses` with `X-Consumer-Key` or delegated bearer auth
14. Optionally enable conversation policy and attach `conversation` in response requests
15. Optionally enable memory policy before using explicit `/api/v1/memories`
```

Run `cargo run -- migrate` before production bootstrap or deployment. Development
commands may migrate on startup when `database.migrate_on_startup` is enabled.
The bootstrap command prints the raw system key once; store it in your secret
manager and send it as `X-Moira-System-Key` for admin API calls.

## Runtime Verification

After configuring providers, models, credentials, routes, and routing policies, test the internal execution kernel:

```bash
cargo run -- execute-test -- --prompt "Hello" --route general
```

Run the standard quality checks before handoff:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Validate database migrations against the local pgvector Postgres container when database behavior changes:

```bash
export MOIRA_TEST_DATABASE_URL=postgres://postgres:postgres@localhost:5432/moira
cargo test security_foundation_migration_creates_contract_tables_when_configured
```

Mutation-test the code a change touches — a passing test proves it ran, not that it would have failed had the code been wrong:

```bash
cargo install cargo-mutants --locked
scripts/mutants.sh            # mutants in this branch's src/ diff against origin/main
```

Scoped to the diff on purpose, and deliberately not a CI gate. See [docs/mutation-testing.md](docs/mutation-testing.md) for how to read a surviving mutant and the condition under which it would become one.

See [docs/conversations.md](docs/conversations.md), [docs/conversation-api.md](docs/conversation-api.md), [docs/conversation-persistence.md](docs/conversation-persistence.md), [docs/context-planning.md](docs/context-planning.md), [docs/conversation-summarization.md](docs/conversation-summarization.md), [docs/memory-architecture.md](docs/memory-architecture.md), [docs/memory-policy.md](docs/memory-policy.md), [docs/memory-consent.md](docs/memory-consent.md), [docs/memory-extraction.md](docs/memory-extraction.md), [docs/memory-retrieval.md](docs/memory-retrieval.md), [docs/memory-correction-and-deletion.md](docs/memory-correction-and-deletion.md), [docs/rag-architecture.md](docs/rag-architecture.md), [docs/rag-collections.md](docs/rag-collections.md), [docs/document-ingestion.md](docs/document-ingestion.md), [docs/document-chunking.md](docs/document-chunking.md), [docs/embeddings.md](docs/embeddings.md), [docs/pgvector.md](docs/pgvector.md), [docs/retrieval-ranking.md](docs/retrieval-ranking.md), [docs/retrieval-citations.md](docs/retrieval-citations.md), [docs/rag-security.md](docs/rag-security.md), [docs/conversation-memory-rag-api.md](docs/conversation-memory-rag-api.md), [docs/data-retention-and-deletion.md](docs/data-retention-and-deletion.md), [docs/public-api.md](docs/public-api.md), [docs/responses-api.md](docs/responses-api.md), [docs/streaming-api.md](docs/streaming-api.md), [docs/public-authentication.md](docs/public-authentication.md), [docs/public-authorization.md](docs/public-authorization.md), [docs/idempotency.md](docs/idempotency.md), [docs/response-persistence.md](docs/response-persistence.md), [docs/execution-and-usage-api.md](docs/execution-and-usage-api.md), [docs/model-and-route-discovery.md](docs/model-and-route-discovery.md), [docs/openai-compatibility.md](docs/openai-compatibility.md), [docs/admin-api.md](docs/admin-api.md), [docs/application-management.md](docs/application-management.md), [docs/provider-management.md](docs/provider-management.md), [docs/provider-credential-management.md](docs/provider-credential-management.md), [docs/jwt-issuer-management.md](docs/jwt-issuer-management.md), [docs/admin-identity-claiming.md](docs/admin-identity-claiming.md), [docs/system-and-consumer-keys.md](docs/system-and-consumer-keys.md), [docs/audit-api.md](docs/audit-api.md), [docs/runtime-architecture.md](docs/runtime-architecture.md), [docs/rig-integration.md](docs/rig-integration.md), [docs/task-routing.md](docs/task-routing.md), [docs/model-routing.md](docs/model-routing.md), [docs/credential-resolution-runtime.md](docs/credential-resolution-runtime.md), [docs/provider-runtime-factory.md](docs/provider-runtime-factory.md), [docs/provider-pools.md](docs/provider-pools.md), [docs/concurrency-and-backpressure.md](docs/concurrency-and-backpressure.md), [docs/retry-and-fallback.md](docs/retry-and-fallback.md), [docs/circuit-breakers.md](docs/circuit-breakers.md), [docs/runtime-events.md](docs/runtime-events.md), [docs/execution-attempts-and-usage.md](docs/execution-attempts-and-usage.md), [docs/runtime-diagnostics.md](docs/runtime-diagnostics.md), [docs/runtime-cache-invalidation.md](docs/runtime-cache-invalidation.md), [docs/deployment.md](docs/deployment.md), [docs/kubernetes.md](docs/kubernetes.md), [docs/redis.md](docs/redis.md), [docs/otel.md](docs/otel.md), [docs/prometheus.md](docs/prometheus.md), [docs/grafana.md](docs/grafana.md), [docs/production-checklist.md](docs/production-checklist.md), [docs/security.md](docs/security.md), [docs/disaster-recovery.md](docs/disaster-recovery.md), [docs/scaling.md](docs/scaling.md), [docs/load-testing.md](docs/load-testing.md), [docs/chaos-testing.md](docs/chaos-testing.md), [docs/enterprise-operations.md](docs/enterprise-operations.md), [docs/todo.md](docs/todo.md), and [docs/openapi.md](docs/openapi.md). See [docs/project-structure.md](docs/project-structure.md) for module boundaries and agent guidance.

For the response localization contract, see [docs/i18n-response-contract.md](docs/i18n-response-contract.md) and the runtime registry at [src/i18n/catalog/](src/i18n/catalog/). The directory index lives in [src/i18n/catalog/mod.rs](src/i18n/catalog/mod.rs), with error translations in [src/i18n/catalog/errors.rs](src/i18n/catalog/errors.rs) and notice translations in [src/i18n/catalog/notices.rs](src/i18n/catalog/notices.rs). The docs copy lives at [docs/i18n-response-catalog.json](docs/i18n-response-catalog.json).
