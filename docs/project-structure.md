# Moira Project Structure

Moira uses a small layered Rust layout. The goal is to make new features easy to place without turning the service into a framework.

## Source Layout

```text
src/
  app/             AppState and process composition
  config/          static config loading and telemetry setup
  domain/          serde API/domain types
  http/            Axum route registration and handlers
  infra/           Postgres pools, migrations, listeners, SQL row mapping
  orchestration/   provider resolution, runtime cache, Rig execution
  security/        JWT/JWKS auth, caller identity, encryption
  error.rs         shared app error and HTTP error mapping
migrations/        append-only SQL migrations
config/            default process config
docs/              runbooks and architecture notes
skills/            repo-local agent skills
```

## Boundaries

- `domain` must stay dependency-light. It owns public structs and enums, not SQLx or Axum behavior.
- `http` owns request extraction, response shaping, and route grouping.
- `infra` owns external persistence and database decoding, including enum string conversion.
- `orchestration` owns Moira behavior: provider lookup, credential priority, runtime cache, and execution.
- `security` owns trust and secret handling. Plaintext credentials should only exist in short-lived local variables.
- `config` owns static infrastructure config. Runtime provider config belongs in PostgreSQL.

## Feature Placement

- New admin endpoint: add handler in `src/http/admin.rs`, types in `src/domain`, SQL row mapping in `src/infra/pg_rows.rs` if needed.
- New execution endpoint: add handler in `src/http`, orchestration behavior in `src/orchestration`, and keep provider-specific calls behind Rig-compatible boundaries.
- New credential behavior: add policy in `src/orchestration/resolver.rs`, encryption/auth in `src/security`, schema changes in a new migration.
- New database table: add an append-only migration and place row decoding in `src/infra`.
- New operational guidance: add or update docs under `docs/`, then link from `README.md` if it is user-facing.

## Agent Guidance

Codex and Claude should read `skills/moira-project-structure/SKILL.md` before structural refactors or broad feature additions. `AGENTS.md` and `CLAUDE.md` point to the same canonical skill so rules stay in one place.
