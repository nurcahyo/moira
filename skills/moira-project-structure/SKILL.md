---
name: moira-project-structure
description: Use when working in the Moira Rust/Axum codebase and deciding where to place modules, route handlers, domain types, PostgreSQL access, security code, orchestration logic, migrations, tests, or documentation. Trigger for structural refactors, new APIs, provider/runtime-config changes, credential handling, and agent handoffs for Claude or Codex.
---

# Moira Project Structure

## Core Rule

Keep boundaries boring and explicit. Moira is an orchestration service over Rig, not a provider SDK; place code by responsibility, not by endpoint name alone.

## Module Layout

- `src/app`: application state and process-wide composition.
- `src/config`: static process configuration and telemetry setup.
- `src/domain`: serde API/domain types with minimal dependencies.
- `src/http`: Axum route registration and request/response handlers.
- `src/infra`: external infrastructure adapters such as PostgreSQL pools, migrations, listeners, and SQL row mapping.
- `src/orchestration`: provider resolution, credential selection, runtime cache, and Rig/OpenAI-compatible execution.
- `src/security`: authentication, caller identity extraction, and secret encryption.
- `src/error.rs`: application error type and HTTP error response mapping.
- `migrations`: append-only SQL migrations.
- `docs`: human and agent-facing architecture/runbook material.

## Placement Rules

- Put new request/response structs in `src/domain` unless they are private query extractors for one handler.
- Put Axum extractors, headers, status codes, and route-specific glue in `src/http`.
- Put SQLx row decoding and database enum string conversion in `src/infra/pg_rows.rs`.
- Put direct SQL queries near the subsystem that owns the behavior; move repeated queries into an infra repository module only after duplication is real.
- Put provider selection, base URL normalization, credential priority, and runtime cache behavior in `src/orchestration`.
- Put AES/JWT/JWKS/security-sensitive code in `src/security`; never log or return plaintext secrets.
- Keep Rig usage at the orchestration execution boundary. Do not create a parallel LLM abstraction.
- Keep migrations append-only. Do not edit committed migrations unless the user explicitly asks for a pre-release rewrite.

## Validation

After structural changes run:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

If database behavior changes, also validate migrations with the local pgvector Postgres container when Docker is available.
