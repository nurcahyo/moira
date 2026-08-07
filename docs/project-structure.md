# Moira Project Structure

Moira uses a small layered Rust layout. The goal is to make new features easy to place without turning the service into a framework.

## Source Layout

```text
src/
  app/             AppState and process composition
  application/     per-context admin/runtime/execution business services
  config/          static config loading and telemetry setup
  domain/          serde API/domain types
  http/            Axum route registration and handlers
  i18n/            response message-key catalog and default English strings
  infra/           Postgres pools, migrations, listeners, SQL row mapping
  orchestration/   runtime and provider-handle caches, execution controls, Rig execution
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
- `application` is thin orchestration between `http` and `infra`/`orchestration`. It is the largest layer and owns request-context handling, the idempotency envelope, and audit wiring; it holds no SQL and no provider protocol.
- `i18n` is the single registry of `moira.error.*` and `moira.notice.*` keys with their default English strings, mirrored into `docs/i18n-response-catalog.json`. A user-visible string added anywhere else is a bug.
- `infra` owns external persistence and database decoding, including enum string conversion.
- `orchestration` owns Moira runtime behavior: the runtime-config and provider-handle caches (`runtime_cache.rs`, `controls.rs`), provider base-URL normalisation (`provider_url.rs`), concurrency, rate limiting and circuit breaking, and the Rig boundary in `runtime_factory.rs`. Credential resolution is not here — it lives in `src/infra/repositories/runtime.rs`.
- `security` owns trust and secret handling. Plaintext credentials should only exist in short-lived local variables. It holds no `PgPool` with **one deliberate exception, the content keyring**, which is two modules:
  - `security/data_keys.rs` reads it. Its rows *are* sealed key material, and its invariant — unwrapped exactly once, at boot, under a custody backend, or the process refuses to start — is a security rule rather than a storage one. Splitting the loader into `infra` would leave neither layer able to enforce it. It touches exactly one table, `content_data_keys`.
  - `security/keyring_admin.rs` changes it: the `moira keyring` verbs (`docs/decision-encryption-at-rest.md` §9). It lives beside the loader rather than in `infra` because their failure postures are opposites and only make sense together — the loader refuses to start on anything it cannot fully open, and these verbs must keep working on exactly that keyring, since `abandon` is reachable *only* after the loader has already refused. It is the one module in `src/security` that reads the five content tables, and it does so through migration `0027`'s `content_envelope_data_key_id()` — reference counts for the retirement audit, and `reseal`'s compare-and-swap. Its SQL identifiers come from the `AadProfile` registry, never from literals, so a sixth sealed column is counted by construction.

  Argument parsing and operator output for those verbs are `src/app/keyring_cli.rs`; nothing in `src/security` formats a CLI message.
- `config` owns static infrastructure config. Runtime provider config belongs in PostgreSQL.

## Feature Placement

- New admin endpoint: add handler in `src/http/admin.rs`, service method in the matching `src/application/admin/` context module, types in `src/domain`, SQL row mapping in `src/infra/pg_rows.rs` if needed.
- New execution endpoint: add handler in `src/http`, service behavior in `src/application`, runtime behavior in `src/orchestration`, and keep provider-specific calls behind `src/orchestration/runtime_factory.rs`.
- New credential behavior: add resolution/priority policy in `src/infra/repositories/runtime.rs` (`resolve_runtime_credential`), encryption and AAD binding in `src/security/crypto.rs`, schema changes in a new migration.
- New user-visible error or notice string: add the key and English default in `src/i18n/catalog/`, and mirror it into `docs/i18n-response-catalog.json`.
- New database table: add an append-only migration and place row decoding in `src/infra`.
- New operational guidance: add or update docs under `docs/`, then link from `README.md` if it is user-facing.

## Agent Guidance

Codex and Claude should read `skills/moira-project-structure/SKILL.md` before structural refactors or broad feature additions. `AGENTS.md` and `CLAUDE.md` point to the same canonical skill so rules stay in one place.
