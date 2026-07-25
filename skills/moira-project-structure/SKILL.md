---
name: moira-project-structure
description: Use when working in the Moira Rust/Axum codebase and deciding where to place modules, route handlers, domain types, PostgreSQL access, security code, orchestration logic, migrations, tests, or documentation. Trigger for structural refactors, new APIs, provider/runtime-config changes, credential handling, and agent handoffs for Claude or Codex.
---

# Moira Project Structure

## Core Rule

Keep boundaries boring and explicit. Moira is an orchestration service over Rig, not a provider SDK; place code by responsibility, not by endpoint name alone.

## Module Layout

- `src/app`: application state (`AppState`) and process-wide composition.
- `src/config`: static process configuration (`settings.rs`) and telemetry/OTel setup (`telemetry.rs`).
- `src/domain`: serde API/domain types with minimal dependencies. **No `rig_core` import anywhere under `src/domain`** — it speaks `DomainMessage` (`src/domain/message.rs`) and Moira envelopes only.
- `src/http`: Axum route registration and request/response handlers (`admin.rs`, `public.rs`, `conversation.rs`, `health.rs`, `observability.rs`, `openapi.rs`).
- `src/application`: use-case services that sit between `src/http` and `src/infra`/`src/orchestration` — `public.rs` (public execution pipeline), `execution.rs` (attempt loop, retry/fallback, credential pre-flight, `build_completion_request`), `conversation.rs`, `runtime_admin.rs`, `setup.rs`, `context.rs` (`RequestContext`), `admin_command.rs` (idempotency/audit envelope), and the `admin/` directory.
  - `src/application/admin/` is a directory, not a file: `mod.rs` is the `AdminService` facade that forwards to per-context services in `applications.rs`, `providers.rs`, `credentials.rs`, `keys.rs`, `jwt_issuers.rs`, `audit.rs`, with `shared.rs` holding pagination, validators, cursor scopes, and the idempotency/audit helpers. Add a new admin context as a new module plus forwarding methods on the facade; do not re-grow `mod.rs`.
- `src/infra`: external infrastructure adapters — PostgreSQL pool and migrations (`db.rs`), SQL row mapping (`pg_rows.rs`), Redis (`redis.rs`), Prometheus metrics (`metrics.rs`), background workers (`workers.rs`, `workers/retention.rs`), and `repositories/`.
  - `src/infra/repositories/` owns five traits, each with a Postgres impl and (under `cfg(test)`) an in-memory fake: `AdminRepository` (`admin.rs`), `PublicRepository` (`public.rs`), `RuntimeRepository` (`runtime.rs`), `ConversationRepository` (`conversation.rs`), `SetupRepository` (`setup.rs`).
- `src/orchestration`: the Rig execution boundary and the controls around it — `runtime_factory.rs` (`RuntimeFactory`, `RuntimeModelHandle`, `classify_completion_error`), `controls.rs` (`ProviderRuntimeCache`, circuit breakers, permits, rate limiter, retry/fallback predicates), `runtime_cache.rs` (`RuntimeConfigCache`), `provider_url.rs` (`normalize_openai_base_url`).
- `src/security`: authentication, caller identity extraction, authorization, API-key hashing, idempotency hashing, masking, SSRF/JWKS hardening, and secret encryption.
- `src/i18n`: the response message catalog (`catalog/errors.rs`, `catalog/notices.rs`) backing the i18n response contract. The key type lives in `src/domain/i18n.rs`; nothing in `src/http`, `src/application`, or `src/orchestration` invents message text of its own.
- `src/error.rs`: application error type and HTTP error response mapping.
- `migrations`: append-only SQL migrations.
- `docs`: human and agent-facing architecture/runbook material.

## Placement Rules

- Put new request/response structs in `src/domain` unless they are private query extractors for one handler.
- Put Axum extractors, headers, status codes, and route-specific glue in `src/http`. Handlers stay thin: authorize, deserialize, delegate to a service in `src/application`, map the result.
- Put use-case logic — orchestrating repositories, idempotency/audit envelopes, retry and fallback policy — in `src/application`, not in `src/http` and not in `src/orchestration`.
- Put SQLx row decoding and database enum string conversion in `src/infra/pg_rows.rs`.
- Put persistence behind the repository trait that owns the table; add a method to an existing trait in `src/infra/repositories/` rather than issuing SQL from `src/application` or `src/http`.
- Put provider client construction, base URL normalization, runtime/handle caching, and circuit-breaker behavior in `src/orchestration`.
- **Credential resolution does not live in `src/orchestration`.** Precedence and selection are implemented once, in `resolve_runtime_credential` (`src/infra/repositories/runtime.rs`); the executable-credential-type gate is mirrored between `supported_credential_types` (`src/application/execution.rs`) and `require_credential_type` (`src/orchestration/runtime_factory.rs`), and a unit test pins the two together. Orchestration receives an already-`ResolvedCredential` and calls `expose_secret()` exactly once, at the provider builder.
- Put AES/JWT/JWKS/security-sensitive code in `src/security`; never log or return plaintext secrets. The canonical credential AAD is `credential_aad` / `CredentialAadParts` in `src/security/crypto.rs` — never hand-assemble an AAD string at a call site.
- Put user-facing message text in the `src/i18n` catalog keyed by a `src/domain/i18n.rs` key; do not inline literals in handlers or services.
- Keep Rig usage at the orchestration execution boundary. `rig_core` is imported by exactly two files — `src/orchestration/runtime_factory.rs` and `src/application/execution.rs`. Adding a third is a design change; `src/domain`, `src/http`, `src/infra`, `src/security`, `src/config`, and `src/app` must import none. Do not create a parallel LLM abstraction.
- Keep migrations append-only. Do not edit committed migrations unless the user explicitly asks for a pre-release rewrite.

## Validation

After structural changes run:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

If database behavior changes, also validate migrations with the local pgvector Postgres container when Docker is available.
