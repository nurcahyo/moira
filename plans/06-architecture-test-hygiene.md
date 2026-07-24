# Plan 06 — Architecture & Test Hygiene

> **Binding cross-cutting spec:** `plans/CONVENTIONS.md`. Where anything below conflicts with that file, **CONVENTIONS.md wins**. This plan has been re-audited against the real tree and brought into compliance with CONVENTIONS §1 (branch/PR), §2 (gates), §3 (unit **and** e2e), §4 (i18n), §8 (Definition of Done).

## Summary

**Objective.** Pay down structural debt in Moira's admin/orchestration/domain layers and its test harness without changing any externally observable behavior: split the `AdminService` god-object into focused per-context services, stop leaking a Rig type into `src/domain`, add repository traits for the four untested-in-isolation repos, delete a dead provider-resolution path, close the i18n catalog gaps CI cannot currently see, and fix two pieces of doc drift. This plan also **owns the test-hygiene work itself** (P2-12 acknowledgement gates, P2-13 database test isolation/teardown).

**Why ordered here.** Per `plans/01-roadmap-and-dependencies.md` §1.2 and §2, security-critical iterations (03, 07) must stay pure — no refactors mixed in. This iteration is the refactor, isolated on purpose. It is "recommended but not a hard gate" before 07 (`plans/01` §2, `-.recommended.->` edge) because a clean `AdminService`/repository-trait surface makes the identity work in 07 easier to land safely, but 07 does not structurally depend on 06 — 07 only adds a new, additive `AdminIdentityService`/`admin_identities` slice. If schedules force a choice, 07 must not block on 06.

**User-visible outcome.** None on the wire. The HTTP surface, OpenAPI contract, request/response shapes, and database schema are unchanged. The only externally observable artifacts are a smaller circuit-reset blast radius (P2-14) and CI catching classes of drift it cannot see today (i18n catalog completeness and mirror drift, per-endpoint unknown-query-field).

**Included scope.** P2-1 (split `AdminService`), P2-2 (remove `rig_core::completion::Message` from `src/domain`), P2-3 (repository traits for public/runtime/conversation/setup), P2-4 (delete the dead `src/orchestration/resolver.rs` path), P2-8 (i18n catalog drift **and completeness** tests), P2-9 (per-endpoint unknown-query-field test), P2-12 (replace ungated `sleep()` interleaving with acknowledgement gates), P2-13 (DB test isolation/teardown), P2-14 (scope `reset_all()` to the changed resource), P3-9 (docs drift).

**Excluded scope.** No new endpoints, no product migrations, no scope/authz changes, no behavior change to routing/credential-resolution semantics, no changes to `AdminService`'s external call signatures. P2-5/P2-6/P2-7/P2-10/P2-11 (routing quality, pool sizing, embedding-dimension policy, container/Helm hardening) are separate P2 findings **not** in this iteration.

---

## Branch & Pull Request (CONVENTIONS §1)

- **Branch:** `plan/06-architecture-test-hygiene`, cut from the **current `main`**. Not stacked on any other plan branch.
- **Commits:** Conventional Commits, matching existing history style (`refactor: split AdminService into per-context services`, `test: isolate integration tests per Postgres schema`, `fix: scope circuit reset to the changed resource`, `docs: record src/application in project-structure`).
- The PR is **not opened** until every gate in §2 / Verification passes locally.
- **PR description — required sections:**
  - **Plan link** — `plans/06-architecture-test-hygiene.md`
  - **Findings addressed** — P2-1, P2-2, P2-3, P2-4, P2-8, P2-9, P2-12, P2-13, P2-14, P3-9
  - **Migrations included** — **none** (the only DDL is test-only `CREATE SCHEMA moira_test_*`, which never runs outside `cargo test`)
  - **Breaking API/OpenAPI changes** — **none**; include the generated-spec diff showing it is empty
  - **Test evidence** — unit + e2e output summary (see Verification)
  - **Rollback procedure** — see Risks & Rollback
  - **Deferred follow-ups** — P2-5, P2-6, P2-7, P2-10, P2-11
- **Done means merged.** Opening the PR is not done. The plan is done when the PR is merged with all gates green and every Definition of Done item objectively verified.
- **Ordering:** this plan changes **no** OpenAPI path, operation, or schema, so it is not subject to the "must land before 05's OpenAPI-drift gate freezes the spec" constraint (`CONVENTIONS §1.6`). It must nonetheless prove the spec is byte-identical (see Verification).
- Never force-push this branch; plan 07 may be developed alongside it.

---

## Findings Addressed

| ID | Evidence (re-verified against the current tree) | Current behavior |
|----|--------------------------------------------------|-------------------|
| P2-1 | `src/application/admin.rs` — **1,873 lines**, **46** `pub async fn` methods on one `AdminService` (verified: `grep -c 'pub async fn' src/application/admin.rs` → 46; `create_application:50` … `get_audit_log:1295`) spanning ~9 bounded contexts. The audit report's "48 public methods" is off by two — **46 is the verified count**. `rotate_key:999`, `revoke_key:1074`, `delete_key:1100` are **table-generic** (each takes `table: &str` selecting `system_api_keys` vs `consumer_api_keys`), which constrains the split (module 5). | One `impl AdminService` block owns unrelated domains; per-context ownership and testing are impossible without whole-file review. |
| P2-2 | `src/domain/runtime.rs:2` (`use rig_core::completion::Message;`) and `:249` (`pub messages: Vec<Message>` inside `ExecutionCommand`, struct declared `:241-257`) | A `domain` DTO is generic over an upstream crate's execution-primitive type, violating `docs/project-structure.md` ("domain must stay dependency-light") and `CLAUDE.md` ("Rig owns AI execution primitives"). |
| P2-3 | `src/infra/repositories/admin.rs:60-61` is the **only** trait (`#[async_trait] pub trait AdminRepository`, impl at `:728-729`, re-exported at `repositories/mod.rs:8`). No trait in `public.rs:22 PgPublicRepository`, `runtime.rs:25 PgRuntimeRepository`, `conversation.rs:33 PgConversationRepository`, `setup.rs:18 PgSetupRepository`. `async_trait` is imported only in `admin.rs:3`. | Only the admin repo is mockable. Application-layer unit tests for public execution, runtime resolution, conversation, and setup must hit a real Postgres or not exist. **Making trait-based mocking possible is the point of P2-3** — see Verification for the unit tests this unlocks. |
| P2-4 | `src/orchestration/resolver.rs`: `resolve_provider:89`, `get_provider:124`, `find_default_provider:145`, `resolve_api_key:159`, `credential_priority:255`. Its only production consumer would be `src/http/chat.rs:15 chat_completions` — and **`mod chat;` is not declared in `src/http/mod.rs` at all** (verified: `mod.rs:1-6` declares only `admin, conversation, health, observability, openapi, public`), so `src/http/chat.rs` **is not compiled**. The live routing path is `src/application/execution.rs` via `src/orchestration/executor.rs` and `src/domain/runtime.rs::ResolvedProviderConfiguration`. | A second, legacy provider-resolution implementation sits in the tree with a **divergent credential AAD** (`resolver.rs:272-281`, 3-part `provider:{}:scope:{:?}:owner:{}`) that could not decrypt any live credential — two credential-priority algorithms and two AAD formats to keep straight, one of which is silently wrong. |
| P2-8 | `src/i18n/catalog/mod.rs:9` is a **comment**, not an assertion. Three verified gaps, all invisible to CI today: **(a)** `docs/i18n-response-catalog.json` has **63 entries but only 61 unique keys** — `moira.error.idempotency_conflict` and `moira.error.rate_limited` each appear **twice**. **(b)** Eight error `code()` values produced by real code have **no catalog entry at all**: `database_unavailable`, `database_error`, `configuration_error`, `upstream_error`, `http_client_error`, `redis_error` (all from `src/error.rs:128-144`), `idempotency_in_progress` (`src/infra/repositories/admin.rs:576,610`), `routing_policy_provider_model_mismatch` (`src/application/runtime_admin.rs`). **(c)** All four `moira.notice.*` entries have **zero production consumers** (`grep -rn 'moira\.notice' src/` matches only the catalog, its README, and a doc-test). | `message_key` is derived as `format!("moira.error.{}", code())` (`src/error.rs:146-148`), so those eight codes ship a `message_key` that **resolves to nothing** in the catalog — a client i18n layer has no default string to fall back to, which is exactly the failure CONVENTIONS §4 forbids. Nothing in the test suite reads the JSON mirror, so the duplicates and the gaps are invisible. |
| P2-9 | `src/domain/admin.rs:31-34` — `#[serde(deny_unknown_fields)]` at `:32`, `#[into_params(parameter_in = Query)]` at `:33`, `pub struct PageQuery {` at `:34`, closing `:61`; **26 fields** (`limit` … `occurred_after`). The audit's "27" is off by one. | `deny_unknown_fields` is applied once, globally, to one struct shared by every admin list/filter endpoint. An endpoint that only honors `status`+`limit` still **accepts** `provider_id` because `PageQuery` defines it for a different endpoint. No test enumerates, per endpoint, which fields are honored vs. silently ignored-but-typed, and no test proves the global rejection works at all. |
| P2-12 | **Corrected from the audit.** `grep -n "tokio::time::sleep" tests/*.rs` returns **nothing** — the sleeps are imported unqualified (`use tokio::time::{sleep, timeout}`: `execution_lifecycle.rs:17`, `admin_idempotency.rs:22`). Re-verified sites: **`tests/admin_idempotency.rs:977` is the only genuinely ungated sleep** (`sleep(Duration::from_millis(50)).await` after `task.abort()` + `blocker.rollback()`, before asserting the application row count is 0 — a pure race with no bound). `admin_idempotency.rs:1259`, `execution_lifecycle.rs:979`, and `execution_lifecycle.rs:1002` are **poll intervals inside `timeout(...)`-wrapped loops** (`wait_for_audit_lock`, cancelled-status poll, `wait_for_attempt_status`) — bounded and fail-loud, not the flake class the finding names. Two further fully-qualified sleeps live in `tests/support/mock_openai.rs:330,373`. | One unbounded sleep is a real latent CI flake (a fast runner can assert before the rollback lands; a slow one can still lose the race). The three bounded poll loops are acceptable but are still time-based where a notification is available. |
| P2-13 | `tests/support/mod.rs` — **496 lines**. `TEST_SERIAL` declared `:40-41` (`static TEST_SERIAL: LazyLock<Arc<Mutex<()>>>`), acquired at `:126` (`TEST_SERIAL.clone().lock_owned().await`) and held in the `_serial: OwnedMutexGuard<()>` field (`:115`, assigned `:174`). `LifecycleFixture` struct `:114`, `new()` `:125`. **No `impl Drop` and no truncate/rollback/cleanup exists anywhere in the file** (verified: zero hits for `impl Drop|truncate|delete from|drop table|cleanup|teardown`). Isolation relies solely on the `Uuid::now_v7()` suffix at `:139`. CI fail-closed already correct at `:430-440` (`panic!` when **`CI=true`** and `MOIRA_TEST_DATABASE_URL` is absent — value check per `CONVENTIONS.md` §3). | Every integration test serializes on one process-wide mutex against one shared physical database with no schema or transaction isolation. Rows from a failed test are never removed and accumulate permanently; suite parallelism is capped at 1. |
| P2-14 | `src/infra/db.rs:59-80` (`listen_once`) — **the payload is never parsed**. Every `NOTIFY moira_runtime_config` unconditionally triggers `cache.invalidate_all()`, `runtime_handles.invalidate_all()`, **and** `circuits.reset_all()` (`:71-73`). The trigger function (`migrations/0004_admin_api_contract.sql:108-127`) emits `json_build_object('resource_type', tg_table_name, 'resource_id', changed_id::text)` (`:116-119`) — `resource_type` and `resource_id` only, **no `tg_op`**. That trigger is attached to **~26 tables** across migrations 0002–0007. `CircuitBreakerRegistry` (`src/orchestration/controls.rs:494`) keys on `(provider_id, model_id)` (`states: Arc<Mutex<HashMap<(Uuid, Uuid), CircuitEntry>>>`, `:495`); `reset_all` (`:606-608`) is `self.states.lock().await.clear()`. | A conversation-policy write, a RAG-document write, or any of ~26 tables' writes discards in-flight circuit-breaker state (including `policy_version` bookkeeping) for **every** provider-model on the instance — throwing away exactly the protection that exists to shield a known-bad upstream. |
| P3-9 | `docs/project-structure.md:8-21` omits `src/application/`, the largest layer (`admin.rs` alone is 1,873 lines). `src/http/chat.rs` (51 lines) is uncompiled dead weight. `docs/todo.md`'s Phase 1 bullet bundles "unregistered chat-route types" with `owner_scope` as legacy-to-remove — but `OwnerScope` lives in `src/domain` and is live/load-bearing in `src/security/crypto.rs` credential envelopes; what is actually dead is `resolver.rs`'s resolution functions and its divergent local `credential_aad`. | Structure doc is stale (missing the biggest layer); a genuinely dead file exists uncounted; the todo's framing invites someone to delete live, load-bearing code while "cleaning up." |

---

## Architecture

### Components & ownership (per `docs/project-structure.md`)

- `src/application/` (undocumented today — P3-9 fixes this) becomes a directory of per-context service modules instead of one file:
  - `src/application/admin/mod.rs` — thin re-export facade (keeps `application::AdminService` name and constructor stable for `src/http/admin.rs`, `src/main.rs`, and `tests/support/mod.rs` call sites)
  - `src/application/admin/applications.rs` — `ApplicationAdminService`
  - `src/application/admin/providers.rs` — `ProviderAdminService` (provider + provider-model)
  - `src/application/admin/credentials.rs` — `CredentialAdminService`
  - `src/application/admin/keys.rs` — `ApiKeyAdminService` (system **and** consumer keys in one service: `rotate_key`/`revoke_key`/`delete_key` are table-generic, so splitting into two files would force duplicating or awkwardly sharing those three)
  - `src/application/admin/jwt_issuers.rs` — `JwtIssuerAdminService`
  - `src/application/admin/audit.rs` — `AuditAdminService`
  - `src/application/admin/validation.rs` — shared request-validation helpers currently inlined across `admin.rs`
  - **No new idempotency helper is invented.** The envelope already exists and is already shared: `AdminCommandRunner` / `admin_command_spec` / `AdminCommandMutation` in `src/application/admin_command.rs`, driving `PgAdminCommandTransaction`'s `claim_idempotency` (`src/infra/repositories/admin.rs:559-634`), `begin_command_savepoint` (`:636-641`), `release_command_savepoint` (`:643-648`), `rollback_command_savepoint` (`:650-655`), `finalize_idempotency` (`:657-687`). Every sub-service keeps calling `AdminCommandRunner::new(self.repo.clone()).execute(spec, …)` exactly as `admin.rs` does today. **This supersedes the earlier draft's "extract `run_idempotent_command`" module** — that helper would have been a re-implementation of code that is already correct and already shared, and the audit's positive finding ("atomic admin idempotency is genuinely correct") is not something to re-derive.
- `src/domain/runtime.rs` stays domain-owned but drops the Rig dependency; a new `src/domain/message.rs` defines `DomainMessage`, with `From`/`TryFrom` conversions to/from `rig_core::completion::Message` living in `src/orchestration/` (the documented Rig-boundary owner).
- `src/infra/repositories/{public,runtime,conversation,setup}.rs` each grow a trait (`PublicRepository`, `RuntimeRepository`, `ConversationRepository`, `SetupRepository`) mirroring `AdminRepository` (`admin.rs:60-234`, `#[async_trait]`, methods returning `Result<_, AppError>`); the concrete `Pg*` structs implement them. `src/application/*.rs` consumers switch from the concrete struct to `Arc<dyn Trait + Send + Sync>`.
- `src/orchestration/resolver.rs` and `src/http/chat.rs` are **deleted outright** (not `#[cfg(test)]`-gated). `ResolvedProvider` (`resolver.rs:35`) and `normalize_openai_base_url` (`resolver.rs:283`) are the **only** items `src/orchestration/executor.rs:11` imports, so they relocate; everything else in the file goes (module 9).
- `src/i18n/catalog/mod.rs` gains drift **and completeness** tests (module 10); `src/i18n/catalog/errors.rs` and `notices.rs` gain the missing entries; `docs/i18n-response-catalog.json` loses its two duplicates and gains the missing entries.
- `src/domain/admin.rs` gains a documented note on the `PageQuery` nuance; `src/http/admin.rs` gains per-endpoint unit tests and a new e2e suite covers it at HTTP level.
- `tests/support/mod.rs` isolation changes from "one global serial mutex + shared DB" to "one Postgres schema per fixture, dropped by a `Drop` guard."
- `src/infra/db.rs::listen_once` parses the NOTIFY payload and scopes the circuit reset.

### Data flow

No data-flow change for the public/admin API. Internally, `src/http/admin.rs` handlers keep calling the same `AdminService` method names on the same struct; the facade delegates to the new sub-service field. **No HTTP handler file changes** in this iteration (except additive `#[cfg(test)]` modules).

### Security boundaries

Unchanged. The split does not change which scope gates which operation — `AuthorizationService::require` calls stay exactly where they are today, inside each relocated method. The transaction envelope (advisory lock via `advisory_lock_key`, `src/infra/repositories/admin.rs:1801-1810` → savepoint → business logic → release/rollback → finalize) is preserved verbatim because the shared `AdminCommandRunner` is not touched at all.

### DB/migration changes

**None.** No product migration is added. For P2-13, `tests/support/mod.rs` gains a test-only helper that runs `CREATE SCHEMA moira_test_<uuidv7>`, sets `search_path`, runs `sqlx::migrate!` against that schema, and drops it on teardown. This is test-only DDL and never runs outside `cargo test`.

### API & OpenAPI changes

**None.** `src/http/mod.rs`'s 8 in-process spec tests (`mod tests` at `:213`; `generated_openapi_covers_every_registered_route:226`, `public_document_filters_admin_paths_and_keeps_operational_paths:339`, `generated_openapi_contains_security_content_types_and_parameters:363`, `every_operation_documents_request_ids_and_protected_operations_document_auth…:422`, `setup_status_contract_is_typed_and_exact:480`, `once_only_key_responses_use_the_secret_envelope:513`, `atomic_admin_idempotency_contract_is_explicit:535`, `every_local_schema_reference_resolves:646`) must pass **unmodified** — that is the structural proof no route changed.

### Backward compatibility

Fully preserved. External clients, the OpenAPI spec, and the production DB schema are byte-identical before/after.

### Deployment implications

None — no migration, no config change, no restart-order concern.

### Failure & recovery

The primary failure mode is a *regression* introduced during the split (e.g. an `AuthorizationService::require` call dropped when a method moves files). Mitigation: every method move is a mechanical cut-paste-adjust-imports change reviewed against the original `git diff` line-for-line (no "while I'm here" edits), and the full existing suite (`tests/admin_idempotency.rs`'s 9 tests in particular) must pass unmodified after **each** service split, not just at the end. Rollback is a plain `git revert`; there is no data migration to unwind.

---

## Detailed Implementation

### Module 1 — Confirm the shared idempotency envelope (no extraction)

- Read `src/application/admin_command.rs` and `src/application/admin.rs:50-92` (`create_application`, the reference caller) before touching anything. Confirm the pattern is: `authz.require(...)` → `admin_command_spec(ctx, actor, "<op>", json!({…}), &request)?` (optionally `.with_expected_version(...)`) → `AdminCommandRunner::new(self.repo.clone()).execute(spec, |transaction| Box::pin(async move { … AdminCommandMutation::new(record, status, resource_id) }))` → `if !outcome.replayed { self.schedule_runtime_cache_invalidation(); }`.
- **Do not refactor, re-implement, or "generalize" this.** Sub-services call it unchanged. The only permitted change is import paths.
- Record the confirmed pattern in the Wave 0 reference document so every sub-service agent copies the same shape.

### Module 2 — `src/application/admin/applications.rs`

Move verbatim: `create_application` (`:50`), `list_applications` (`:93`), `get_application` (`:105`), `patch_application` (`:114`), `delete_application` (`:146`), `set_application_enabled` (`:167`). Mirror `AdminService`'s existing field shape for construction — do not redesign it.

### Module 3 — `src/application/admin/providers.rs`

Move verbatim: `create_provider` (`:198`), `list_providers` (`:253`), `get_provider` (`:262`), `patch_provider` (`:267`), `delete_provider` (`:296`), `set_provider_enabled` (`:316`), `create_provider_model` (`:345`), `list_provider_models` (`:390`), `patch_provider_model` (`:403`), `get_provider_model` (`:425`), `delete_provider_model` (`:434`), `set_provider_model_enabled` (`:454`). Providers and provider-models stay one service — a provider-model always requires its parent provider and they already share private helpers. Verify the shared-helper boundary before splitting further; do not force an artificial split that duplicates a private helper.

### Module 4 — `src/application/admin/credentials.rs`

Move verbatim: `create_credential` (`:483`), `list_credentials` (`:546`), `list_user_credentials` (`:558`), `get_credential` (`:572`), `patch_credential` (`:583`), `rotate_credential` (`:614`), `validate_credential` (`:687`), `set_credential_enabled` (`:722`), `delete_credential` (`:753`), `delete_user_credential` (`:777`).

Note the asymmetry to **preserve, not "fix"**: `create_credential`/`rotate_credential` run inside `AdminCommandRunner`, while `patch_credential` (`:583-612`) does not and calls `self.state.runtime_cache.invalidate_all()` directly. That inconsistency is out of scope here — moving it unchanged is correct; changing it is a behavior change this plan forbids. Record it as a deferred follow-up.

### Module 5 — `src/application/admin/keys.rs`, `jwt_issuers.rs`, `audit.rs`

Re-verify line numbers with `grep -n "pub async fn" src/application/admin.rs` at execution time before moving.

- `keys.rs` (`ApiKeyAdminService`): `create_system_key` (`:803`), `list_system_keys` (`:871`), `get_system_key` (`:883`), `create_consumer_key` (`:888`), `list_consumer_keys` (`:974`), `get_consumer_key` (`:988`), and the table-generic trio `rotate_key` (`:999`), `revoke_key` (`:1074`), `delete_key` (`:1100`) — one service, so the generic trio is not duplicated.
- `jwt_issuers.rs` (`JwtIssuerAdminService`): `create_trusted_jwt_issuer` (`:1125`), `list_trusted_jwt_issuers` (`:1161`), `get_trusted_jwt_issuer` (`:1173`), `patch_trusted_jwt_issuer` (`:1182`), `set_trusted_jwt_issuer_enabled` (`:1209`), `refresh_trusted_jwt_issuer` (`:1237`), `delete_trusted_jwt_issuer` (`:1264`).
- `audit.rs` (`AuditAdminService`): `list_audit_logs` (`:1283`), `get_audit_log` (`:1295`).

### Module 6 — `src/application/admin/mod.rs` facade

- `pub struct AdminService<'a> { applications: …, providers: …, credentials: …, keys: …, jwt_issuers: …, audit: … }`.
- `impl<'a> AdminService<'a>` re-exposes **all 46** original public method names as one-line delegates, e.g. `pub async fn create_application(&self, actor: &Actor, ctx: &RequestContext, req: ApplicationCreateRequest) -> Result<ApplicationRecord, AppError> { self.applications.create_application(actor, ctx, req).await }`. This keeps `src/http/admin.rs`, `src/main.rs`'s `bootstrap_system_key` (`AdminService::new(&state)?.create_system_key(...)`), and `tests/support/mod.rs`'s `AdminService::new(&state)?.create_application(...)` unmodified.
- Delete `src/application/admin.rs`, replace with the `src/application/admin/` directory; `src/application/mod.rs`'s `mod admin;` and `pub use admin::AdminService;` lines are unchanged.

### Module 7 — `src/domain/message.rs` + Rig boundary (P2-2)

- Add `src/domain/message.rs`:
  ```rust
  #[derive(Debug, Clone, Serialize, Deserialize)]
  pub struct DomainMessage {
      pub role: DomainMessageRole,
      pub content: String,
  }
  #[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
  #[serde(rename_all = "snake_case")]
  pub enum DomainMessageRole { User, Assistant, System, Tool }
  ```
  Reconcile the field/variant set against what `rig_core::completion::Message` actually exposes **and** against what `ExecutionCommand.messages` consumers in `src/orchestration/executor.rs` and `src/application/execution.rs` read today. Read those call sites before finalizing so no information is lost in conversion.
- `src/domain/runtime.rs:249` changes `pub messages: Vec<Message>` → `Vec<DomainMessage>`; drop `use rig_core::completion::Message;` at `:2`. Note `ExecutionCommand` (`:241-257`) derives `Debug, Clone, Serialize, Deserialize` but **not** `ToSchema` — do not add it.
- Add the conversion impls in `src/orchestration/executor.rs` (the file that already imports from `resolver` and is the documented Rig-boundary owner).
- Update every `ExecutionCommand { … }` construction site (grep across `src/application/`, `src/orchestration/`) to build `DomainMessage`s and convert **at** the orchestration boundary, not before.

### Module 8 — Repository traits (P2-3)

For each of `src/infra/repositories/{public.rs, runtime.rs, conversation.rs, setup.rs}`:
- Mirror `AdminRepository` exactly (`src/infra/repositories/admin.rs:60-234`): `#[async_trait]`, `pub trait X { async fn … -> Result<_, AppError>; }`, `Send + Sync` bounds, impl block annotated `#[async_trait]`.
- Extract a trait with the method set the concrete struct already exposes publicly. `PgSetupRepository` is the smallest (`inspect` at `setup.rs:27-42`) and is the right one to do first as the pattern reference; `PgRuntimeRepository` is the largest (24 public methods, `runtime.rs:84-985`).
- Update consumers to hold `Arc<dyn Trait + Send + Sync>`: `SetupService` (`src/application/setup.rs`, currently `repo: PgSetupRepository`), `RuntimeAdminService` (`src/application/runtime_admin.rs:21-35`, holds `PgRuntimeRepository` **and** `PgAdminRepository`), the public-execution consumer, and the conversation consumer.
- Add one in-memory fake per trait under `#[cfg(test)]` in the same repository file, sufficient for a first unit test of business logic without Postgres. **Fakes must not embed real credential material** — use synthetic values only.
- Do **not** replace the existing Postgres-backed integration tests; only add the option.
- Re-export the four new traits from `src/infra/repositories/mod.rs` beside `AdminRepository` (`mod.rs:8`).

### Module 9 — Delete the dead resolver path (P2-4)

**Corrected from the earlier draft, which wrongly said `credential_aad` must be preserved from `resolver.rs`.**

1. Re-run the dead-code proof before deleting: `grep -rn "resolver::" src/` must return only `src/orchestration/mod.rs:11` (the re-export block) and `src/orchestration/executor.rs:11`; `grep -rn "mod chat\|http::chat\|chat_completions" src/` must return only `src/http/chat.rs:15` itself (plus the unrelated setting `src/config/settings.rs:166 chat_completions_compat_enabled`, which is **not** a reference to the handler). If either check fails, stop and re-scope.
2. **Relocate first, delete second.** `src/orchestration/executor.rs:11` imports exactly two items: `ResolvedProvider` (`resolver.rs:35`) and `normalize_openai_base_url` (`resolver.rs:283-292`). Move both into `src/orchestration/executor.rs` (or a small `src/orchestration/provider_url.rs` if `executor.rs` is already crowded), keeping their bodies verbatim, and update `src/orchestration/mod.rs:11-13`'s `pub use` block accordingly.
3. **Delete, do not move, `resolver.rs`'s local `credential_aad` (`:272-281`).** It is a *divergent duplicate* using the legacy 3-part format `provider:{}:scope:{:?}:owner:{}`, referenced only by its own test at `:420`. The canonical, live AAD is `credential_aad` + `CredentialAadParts` in `src/security/crypto.rs:96-111` (8 fields), which `resolver.rs:18` itself already imports as `envelope_credential_aad` and uses at `:239`. Deleting the local one removes a landmine that would silently fail to decrypt any live credential.
4. Also delete `credential_priority` (`resolver.rs:255-268`) — the live precedence lives in `src/infra/repositories/runtime.rs`'s `resolve_runtime_credential` SQL (`:775-868`). Confirm no non-test caller first.
5. Delete `src/http/chat.rs`. **No `src/http/mod.rs` edit is required** — `mod chat;` was never declared, so the file is not compiled today. State this explicitly in the PR so a reviewer does not go looking for a router change.
6. Delete `src/orchestration/resolver.rs` and its `src/orchestration/mod.rs` re-exports.
7. `ChatCompletionRequest` in `src/domain`: grep first. Delete only if nothing else references it. The mandate is to remove the *dead code path*, not necessarily every adjacent DTO.
8. `OwnerScope` lives in `src/domain` and is live in `src/security/crypto.rs` credential envelopes — **untouched**.

### Module 10 — i18n catalog: drift **and** completeness (P2-8, CONVENTIONS §4)

This module is this plan's i18n deliverable. It fixes real, verified defects, not just adds a guard.

**10a — Fix the mirror.** `docs/i18n-response-catalog.json` (shape: `{ "version", "default_locale", "namespace", "entries": [ { "key", "default_message", "description" } ] }`) currently has **63 entries / 61 unique keys**. Remove the duplicate `moira.error.idempotency_conflict` and `moira.error.rate_limited` entries.

**10b — Add the eight missing error entries.** These `code()` values are produced by real code but have **no** catalog entry, so their derived `moira.error.<code>` key resolves to nothing. Add each to `src/i18n/catalog/errors.rs` **and** the JSON mirror, with an English `default_message` and a `description`:

| Code (→ key `moira.error.<code>`) | Produced at | Suggested `default_message` |
|---|---|---|
| `database_unavailable` | `src/error.rs:136` (`AppError::DatabaseUnavailable`, 503) | "The service is not connected to its database." |
| `database_error` | `src/error.rs:139` (`AppError::Sqlx`) | "A database error occurred." |
| `configuration_error` | `src/error.rs:138` (`AppError::Config`) | "The server configuration is invalid." |
| `upstream_error` | `src/error.rs:137` (`AppError::Upstream`) | "The upstream provider failed." |
| `http_client_error` | `src/error.rs:140` (`AppError::Reqwest`) | "An outbound HTTP request failed." |
| `redis_error` | `src/error.rs:141` (`AppError::Redis`) | "A Redis error occurred." |
| `idempotency_in_progress` ⚠️ | `src/infra/repositories/admin.rs:576,610` (409) | "An identical request is already being processed. Retry shortly." — **⚠️ Ownership: plan 02b adds this key and lands first** (roadmap order `02a → 02b → 03 → 04 → … → 06`). **Do not re-add it**; on rebase, assert it is present rather than inserting it. A duplicate insert is caught by this plan's own `docs_mirror_has_no_duplicate_keys` test. |
| `routing_policy_provider_model_mismatch` | `src/application/runtime_admin.rs` | "The routing policy references a provider model that does not belong to the selected provider." |

Verify each suffix against `format!("moira.error.{}", self.code())` (`src/error.rs:146-148`) so key and code match **exactly**.

**10c — Notices.** All four `moira.notice.*` entries currently have **zero production consumers**. This plan adds **no** notice entries and emits none — it introduces no new user-visible success string (it is a pure refactor). Record the dead-catalog fact in `src/i18n/catalog/mod.rs`'s module doc so the next plan that emits a notice (07) knows it is the first real consumer, and add `notice_catalog_is_documented_as_currently_unconsumed` only if a reviewer wants the fact test-asserted rather than commented.

**10d — The tests.** In `src/i18n/catalog/mod.rs`'s `#[cfg(test)] mod tests` (which already holds `response_catalog_keys_are_unique:45` and `default_messages_can_be_resolved:53`):

- `docs_mirror_matches_rust_catalog` — read `docs/i18n-response-catalog.json` via `std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/docs/i18n-response-catalog.json"))`, parse the `entries` array, and assert set equality of `(key, default_message, description)` against `all_entries()`. Fail with a diff-friendly message naming keys present on only one side and, for shared keys, the exact differing field.
- `docs_mirror_has_no_duplicate_keys` — assert `entries.len() == unique_keys.len()`; this is the assertion that would have caught the two duplicates.
- `every_app_error_variant_code_has_a_catalog_entry` — in `src/error.rs`'s test module, construct one `AppError` per variant and assert `is_known_key(&error.error_response(None).error.message_key)`. This is the test that would have caught the six `src/error.rs` gaps.
- `every_coded_error_literal_in_src_has_a_catalog_entry` — walk `src/**/*.rs` from `CARGO_MANIFEST_DIR` at test time, regex-match `AppError::(coded|conflict|unprocessable)(…, "<code>")`, and assert every captured code has a `moira.error.<code>` entry. This is the test that would have caught `idempotency_in_progress` and `routing_policy_provider_model_mismatch`. Keep the walker simple and deterministic (sorted file order, skip `target/`).
- **Injected-failure proof (required, per Definition of Done):** during development, temporarily delete one JSON entry and one catalog entry and confirm each test fails loudly; revert before commit. A test that has never been seen to fail is not evidence.

### Module 11 — Per-endpoint unknown-query-field (P2-9)

- Unit (`src/http/admin.rs` `#[cfg(test)] mod tests`, additive): for each admin list/filter endpoint, deserialize a query string containing a genuinely-unknown field (`?not_a_real_field=1`) into `PageQuery` and assert rejection. Name: `page_query_rejects_a_field_absent_from_the_struct`.
- E2E (`tests/admin_query_contract.rs`, new): drive each `GET /api/v1/admin/*` list route over HTTP with `?not_a_real_field=1` and assert `400` plus a well-formed error envelope carrying a non-empty `message_key` **and** `message` (CONVENTIONS §4.5). Name: `each_admin_list_endpoint_rejects_an_unknown_query_field`. A second test, `defined_but_unsupported_page_query_field_is_accepted_and_ignored`, pins the documented nuance so a future change to it is a deliberate, visible decision.
- Add a doc comment above `PageQuery` (`src/domain/admin.rs:31`) recording the P2-9 nuance: `deny_unknown_fields` rejects only fields **absent from the struct**; a field defined on `PageQuery` for a *different* endpoint (e.g. `credential_type` on the applications list) is silently accepted and ignored. Cross-reference finding ID P2-9. **Do not attempt to fix this in 06** — per-endpoint query types are a larger change; document it and leave it as future work.

### Module 12 — Replace ungated `sleep()` interleaving (P2-12)

Scoped to what the re-verification actually found:

- **`tests/admin_idempotency.rs:977` — the one real fix.** Replace `sleep(Duration::from_millis(50)).await` (which follows `task.abort()` + `blocker.rollback()` and precedes the "application row count is 0" assertion) with an explicit acknowledgement. Preferred: `await` the aborted `JoinHandle` so cancellation is observed rather than guessed, then poll the row count inside `timeout(Duration::from_secs(5), …)` so the assertion is bounded and fail-loud. If a production-side signal is needed at the exact raced point (e.g. "after advisory lock acquired, before savepoint begins"), add a `#[cfg(test)]`-only probe or a trait test double — **never** an ungated production side effect that exists only for tests.
- **`tests/admin_idempotency.rs:1259`, `tests/execution_lifecycle.rs:979`, `tests/execution_lifecycle.rs:1002` — reclassified, not rewritten blind.** Each is a poll interval inside a `timeout(...)`-wrapped loop (`wait_for_audit_lock`, the cancelled-status poll, `wait_for_attempt_status`), so each already fails loudly rather than flaking silently. Convert to `tokio::sync::Notify`/`oneshot` **only where a real signal exists**; otherwise leave the bounded poll and add a one-line comment citing P2-12 explaining why polling is correct here. Replacing a bounded poll with a hand-rolled signal that is subtly racier is a regression, not a fix.
- `tests/support/mock_openai.rs:330,373` already sit alongside `Semaphore`/`Notify` gating in the same file (`:26,42-55,123,423`) — bring them onto the existing gates.
- The existing `Barrier`-based concurrency pattern (`tests/admin_idempotency.rs:518,535,1028` — `Arc::new(Barrier::new(3))` then `barrier.wait().await`) is the house style for firing simultaneous requests and must be reused for any new concurrency test.
- **Anti-flake proof:** run the affected suites in a loop (≥20 iterations) and confirm zero failures. A sleep-based race replaced by a still-racy signal is not done.

### Module 13 — DB test isolation & teardown (P2-13)

- `tests/support/mod.rs`: replace the `TEST_SERIAL` global-mutex serialization (`:40-41`, acquired `:126`, held in `_serial` `:115`/`:174`) with per-fixture Postgres schema isolation:
  1. On `LifecycleFixture::new()` (`:125`), generate `moira_test_<uuidv7>`, `CREATE SCHEMA`, and set `search_path` for that fixture's pool (`PgPoolOptions::after_connect` is the reliable way to apply it to **every** pooled connection, not just the first).
  2. Run `sqlx::migrate!("./migrations")` against that schema.
  3. Guarantee teardown with a `Drop` guard that issues `DROP SCHEMA moira_test_<uuidv7> CASCADE` — **`Drop` must run even when the test panics**, which is the whole point (there is no `impl Drop` in the file today at all). Because `Drop` is synchronous, drive the async drop through the fixture's existing shutdown mechanism (a `tokio::runtime::Handle::block_in_place` / `Handle::current().spawn` + join, or an explicit `async fn teardown()` called from every test **plus** a `Drop` fallback that logs loudly if teardown was skipped). Pick one and make it unconditional; do not rely on tests remembering to call teardown.
  4. Keep the existing CI fail-closed behavior at `:430-440` exactly as-is — i.e. preserve the `env::var("CI").is_ok_and(|v| v.eq_ignore_ascii_case("true"))` value check (per `CONVENTIONS.md` §3). Do **not** revert it to `env::var_os("CI").is_some()`, and do not let the schema-isolation refactor drop the `panic!` branch.
- `LISTEN/NOTIFY` channels are **database-wide, not schema-scoped** in Postgres. With ~26 tables carrying the `moira_runtime_config` trigger, concurrent schema-isolated tests **will** see each other's notifications. Handle it explicitly: either skip `spawn_runtime_config_listener` in fixtures that do not exercise invalidation (most do not), or embed the fixture's schema name in a test-run id the listener filters on. The new `tests/runtime_config_invalidation.rs` (module 14) is the one suite that must run its listener, so it must tolerate foreign notifications by asserting on its own resource ids only.
- `tests/security_foundation.rs` already creates and force-drops an **entire database** per run (`connect_test_database:251`, `database_url_with_name:267`) — a second, coarser isolation strategy. Reconcile: keep it as-is (it is a migration-contract test, not a fixture user) and note the two strategies coexist deliberately.
- Remove `TEST_SERIAL` only once nothing depends on it. Grep first and re-check `src/app/state.rs` for process-global mutable state that schema isolation does **not** fix — `InMemoryRateLimiter` (`state.rs:35`), `ConcurrencyController` (`:34`), and `CircuitBreakerRegistry` (`:36`) are per-`AppState`, so per-fixture `AppState` construction already isolates them; confirm this before deleting the mutex, and keep serialization only where a genuine residual need exists.

### Module 14 — Scope `reset_all()` to the changed resource (P2-14)

- `src/infra/db.rs:59-80` (`listen_once`) currently ignores `notification.payload()` entirely. Parse it as `{ "resource_type": String, "resource_id": String }` (the exact shape emitted by `migrations/0004_admin_api_contract.sql:116-119`).
- Keep `cache.invalidate_all()` and `runtime_handles.invalidate_all()` unconditional — the audit flagged only `circuits.reset_all()` as over-broad, and narrowing the caches is a separate, riskier change.
- Replace `circuits.reset_all()` with `circuits.reset_for_resource(resource_type, resource_id)`. Because `CircuitBreakerRegistry` keys on `(provider_id, model_id)` (`src/orchestration/controls.rs:494-495`), the mapping is:
  - `providers` → remove every entry whose **`provider_id`** equals `resource_id`.
  - `provider_models` → remove every entry whose **`model_id`** equals `resource_id`.
  - `provider_runtime_policies` (`migrations/0005`) → provider-scoped; resolve the owning provider id from the payload's `resource_id` **or**, if that requires a query on the hot path, fall back to the provider-scoped reset only when the id parses as a provider (document whichever is chosen).
  - Every other `resource_type` (applications, conversations, RAG tables, keys, issuers, …) → **no circuit reset at all**.
- **Fail-safe on malformed input:** if the payload does not parse, or `resource_id` is not a `Uuid`, or `resource_type` is unknown, fall back to today's `reset_all()` and log at `warn`. Narrowing must never turn a parse bug into a silently-stale breaker.
- Add `reset_for_resource` to `CircuitBreakerRegistry` alongside the existing `reset_all` (`controls.rs:606-608`) — keep `reset_all` for legitimate callers such as process startup.
- Note the payload carries **no `tg_op`**, so INSERT/UPDATE/DELETE cannot be distinguished; the scoped reset must be correct for all three. Do not add `tg_op` (that would require a migration, which this plan excludes).
- **Directional safety:** the worst case of an incomplete mapping is a circuit that *should* have reset and did not — strictly more conservative than today, self-healing on the next relevant NOTIFY or breaker timeout. Not a security or correctness regression.

### Module 15 — Docs drift (P3-9)

- `docs/project-structure.md:8-21`: add `application/  per-context admin/runtime/execution business services` to the source-tree listing in the correct position, and add one sentence to "Boundaries" describing `application`'s role (thin orchestration between `http` and `infra`/`orchestration`; owns request-context, idempotency-envelope, and audit wiring) — it is currently undocumented despite being the largest layer.
- `src/http/chat.rs` deletion (module 9) satisfies the second drift item by construction.
- `docs/todo.md`: locate the Phase 1 bullet bundling "unregistered chat-route types" with `owner_scope`. Split it: mark the chat-route deletion **done** (module 9), and rewrite the `owner_scope` line to state that `OwnerScope` (`src/domain`) and the credential AAD binding (`src/security/crypto.rs:84-111`) are **live and load-bearing** and must not be removed; what was dead was `src/orchestration/resolver.rs`'s resolution functions and its divergent local `credential_aad` (`:272-281`), now deleted. Cite `src/security/crypto.rs`, not `resolver.rs` line numbers, since that file no longer exists once this lands.

---

## i18n Compliance (CONVENTIONS §4)

- This plan adds **no new error code** and **no new user-visible success string**, so it adds no *new* `moira.error.*` or `moira.notice.*` entry for behavior of its own.
- It nonetheless carries a mandatory i18n deliverable: module 10 closes **eight** verified catalog gaps and **two** duplicate JSON entries, and adds the tests (`docs_mirror_matches_rust_catalog`, `docs_mirror_has_no_duplicate_keys`, `every_app_error_variant_code_has_a_catalog_entry`, `every_coded_error_literal_in_src_has_a_catalog_entry`) that make CONVENTIONS §4.1/§4.4/§4.5 enforceable for **every later plan**, including 07's new codes.
- `message_args` is untouched by this plan; no handler gains an inline English literal.
- The JSON mirror is updated in the **same PR** as the Rust catalog.

---

## Multi-Agent Workflow

Fifteen modules across `src/application/`, `src/domain/`, `src/infra/`, `src/orchestration/`, `src/http/`, `src/i18n/`, `tests/`, `docs/`. File ownership is disjoint per agent by construction.

**Wave 0 (coordinator, sequential, before any agent starts).** One agent reads `src/application/admin.rs` in full and produces the authoritative 46-method → target-module mapping with current line numbers, plus the confirmed `AdminCommandRunner` call pattern (module 1) and the `AdminRepository` trait shape (module 8 template). Output is a shared reference document, not a code change.

**Wave 1 (parallel, disjoint files):**
- **Agent A — modules 1-6**, the entire `AdminService` split, sequentially and alone. Every sub-module shares one facade file (`src/application/admin/mod.rs`); splitting this across agents guarantees merge conflicts.
- **Agent B — module 7** (`src/domain/message.rs`, `src/domain/runtime.rs`, `src/orchestration/executor.rs` conversions).
- **Agent C — module 8** (four repository files + their four `src/application/*.rs` consumers — `setup.rs`, `runtime_admin.rs`, the public and conversation consumers; explicitly **not** `admin.rs`, avoiding Agent A).
- **Agent D — module 9** (delete `src/http/chat.rs`, `src/orchestration/resolver.rs`; relocate `ResolvedProvider`/`normalize_openai_base_url` into `src/orchestration/executor.rs`; clean `src/orchestration/mod.rs:11-13`). **Overlaps Agent B on `executor.rs`** — run D after B lands, or pre-agree non-overlapping insertion points. Flag to the coordinator.
- **Agent E — modules 10 + 11** (`src/i18n/catalog/{mod,errors,notices}.rs`, `docs/i18n-response-catalog.json`, `src/error.rs` test module, `src/domain/admin.rs` doc comment, `src/http/admin.rs` additive test module, new `tests/admin_query_contract.rs`). The `src/http/admin.rs` addition should land after Agent A to avoid rebasing across a file move.
- **Agent F — modules 12 + 13** (`tests/admin_idempotency.rs`, `tests/execution_lifecycle.rs`, `tests/support/mod.rs`, `tests/support/mock_openai.rs`). Fully disjoint from all production-code agents.
- **Agent G — module 14** (`src/infra/db.rs`, `src/orchestration/controls.rs`, new `tests/runtime_config_invalidation.rs`).
- **Agent H — module 15** (docs only). Fully disjoint; can run first.
- **Agent I — the e2e regression suite** (`tests/admin_surface_contract.rs`, new file). Must land **after** Agent A, because its whole purpose is to prove the split changed nothing. Golden values are captured on `main` **before** the split and asserted after.

**Sequencing constraints.**
- D after B (`executor.rs`).
- E's `src/http/admin.rs` edit after A.
- I after A, with goldens captured pre-split.
- **Cross-plan:** plan 07 also edits `src/infra/db.rs::listen_once` (it adds auth-settings cache invalidation). If 06 and 07 are in flight together, module 14 and 07's listener change must be merged deliberately, not blind-rebased. Flag to the coordinator at Wave 0.
- All other agents (B, C, F, G, H) run fully parallel with A.

**Checkpoints (read-only reviewer, after each wave and after each sequential merge).** Run the full gate list; report pass/fail; do not edit code. This is the mechanism that catches an `authz.require` call dropped during the split.

---

## Interfaces & Contracts

No new endpoints, no changed request/response shapes, no changed status codes, headers, scopes, or **wire-visible** error codes.

**i18n:** the catalog *content* changes (eight additions, two JSON de-duplications), but no `message_key` that a client sees today changes value. The eight added keys are ones that were already being emitted with no catalog backing — adding them is strictly additive from a client's perspective and is what CONVENTIONS §4 requires.

**Idempotency:** unchanged. `AdminCommandRunner` and the `claim_idempotency`/savepoint/`finalize_idempotency` sequence are not modified at all — sub-services call the same shared code.

**Transaction boundaries:** unchanged (advisory lock → savepoint → business logic → release/rollback → finalize, within one `AdminRepository`-held connection, per method).

**Cache invalidation:** `cache.invalidate_all()` / `runtime_handles.invalidate_all()` unchanged. `circuits.reset_all()` becomes `circuits.reset_for_resource(...)` with a `reset_all()` fallback on malformed payloads — the one intentional behavioral narrowing, scoped down, never up.

**Concurrency:** `pg_try_advisory_xact_lock` single-winner semantics unchanged; no new lock keys.

**SSE:** not touched.

---

## Verification (CONVENTIONS §3 — unit **and** e2e are both mandatory)

### Unit tests (new, named)

| File | Test | Proves |
|------|------|--------|
| `src/i18n/catalog/mod.rs` | `docs_mirror_matches_rust_catalog` | JSON mirror ≡ `all_entries()` (P2-8) |
| `src/i18n/catalog/mod.rs` | `docs_mirror_has_no_duplicate_keys` | catches the two verified duplicates |
| `src/i18n/catalog/mod.rs` | `every_coded_error_literal_in_src_has_a_catalog_entry` | catches `idempotency_in_progress`, `routing_policy_provider_model_mismatch` |
| `src/error.rs` | `every_app_error_variant_code_has_a_catalog_entry` | catches the six `src/error.rs` gaps |
| `src/domain/message.rs` | `domain_message_round_trips_through_rig_message` | P2-2 conversion is lossless |
| `src/domain/message.rs` | `domain_message_serde_uses_snake_case_roles` | wire shape pinned |
| `src/infra/repositories/setup.rs` | `setup_repository_trait_is_object_safe`, `fake_setup_repository_reports_incomplete_configuration` | P2-3 trait mocking works without Postgres |
| `src/infra/repositories/public.rs` | `fake_public_repository_supports_execution_service_unit_test` | ditto |
| `src/infra/repositories/runtime.rs` | `fake_runtime_repository_supports_candidate_selection_unit_test` | ditto |
| `src/infra/repositories/conversation.rs` | `fake_conversation_repository_supports_policy_unit_test` | ditto |
| `src/application/setup.rs` | `status_is_setup_required_without_a_root_system_key`, `status_is_ready_when_an_executable_path_exists` | first Postgres-free unit test of an application service — the payoff of P2-3 |
| `src/orchestration/controls.rs` | `reset_for_resource_clears_only_the_named_providers_entries`, `reset_for_resource_clears_only_the_named_models_entries`, `reset_for_resource_ignores_unrelated_resource_types`, `reset_all_still_clears_everything` | P2-14 mapping |
| `src/infra/db.rs` | `notify_payload_parses_resource_type_and_id`, `malformed_notify_payload_falls_back_to_reset_all` | P2-14 fail-safe |
| `src/http/admin.rs` | `page_query_rejects_a_field_absent_from_the_struct` | P2-9 global rejection actually works |
| moved-with-code | every `#[cfg(test)] mod tests` that lived in `src/application/admin.rs` moves **with** its methods into the new sub-service file — none deleted "because it's redundant" | split loses no coverage |

### E2E tests (new, named — real HTTP surface, real PostgreSQL 16 + pgvector)

Following the existing harness (`tests/support/mod.rs`) and the in-process-router pattern from `tests/admin_idempotency.rs` (`moira::build_router(state.clone())`, the `post(router, path, key, if_match, body)` helper at `:168-212`, `request_with_id` at `:1212-1245`).

| File | Test | Proves |
|------|------|--------|
| `tests/admin_surface_contract.rs` (new) | `applications_crud_contract_is_unchanged_after_service_split` | P2-1 changed no HTTP behavior |
| | `providers_and_provider_models_contract_is_unchanged_after_service_split` | ditto |
| | `credentials_crud_and_rotation_contract_is_unchanged_after_service_split` | ditto, incl. `If-Match` 409 |
| | `system_and_consumer_key_contract_is_unchanged_after_service_split` | ditto, incl. once-only secret envelope |
| | `trusted_jwt_issuer_contract_is_unchanged_after_service_split` | ditto |
| | `audit_log_contract_is_unchanged_after_service_split` | ditto |
| | `every_admin_mutation_still_writes_exactly_one_audit_row` | no audit call dropped in a move |
| | `every_admin_mutation_still_honours_its_required_scope` | no `authz.require` dropped in a move |
| `tests/admin_query_contract.rs` (new) | `each_admin_list_endpoint_rejects_an_unknown_query_field` | P2-9 at HTTP level, with a non-empty `message_key` + `message` |
| | `defined_but_unsupported_page_query_field_is_accepted_and_ignored` | pins the documented nuance |
| `tests/runtime_config_invalidation.rs` (new) | `provider_model_notify_resets_only_that_models_circuit` | P2-14 |
| | `unrelated_table_notify_leaves_all_circuits_intact` | P2-14 — the actual finding |
| | `runtime_cache_still_invalidates_on_every_notify` | narrowing did not over-narrow |
| | `malformed_notify_payload_falls_back_to_full_reset` | fail-safe |
| `tests/test_isolation.rs` (new) | `each_fixture_runs_in_its_own_schema` | P2-13 |
| | `schema_is_dropped_even_when_the_test_panics` | P2-13 — the property that matters |
| | `two_concurrent_fixtures_do_not_observe_each_others_rows` | P2-13, and that `TEST_SERIAL` removal is safe |

**Regression baseline (must pass unmodified in outcome):** `tests/admin_idempotency.rs` (9 tests — `all_ten_admin_operation_identities_replay_the_same_resource`, `concurrent_same_key_create_has_one_resource_audit_and_ledger`, `command_hash_covers_request_path_and_version_while_scope_covers_actor…`, `trusted_actor_fingerprint_isolates_issuer_and_application_identity`, `expired_record_is_reclaimed_and_deterministic_failure_is_replayed`, `advisory_lock_timeout_is_transient_and_does_not_mutate_or_cache`, `audit_failure_and_request_cancellation_roll_back_mutation_and_ledger…`, `credential_rotation_enforces_if_match_atomically`, `concurrent_key_create_returns_plaintext_to_exactly_one_caller`), `tests/execution_lifecycle.rs` (14 tests), `tests/public_authorization.rs`, `tests/http_error_contract.rs`, `tests/security_foundation.rs`. Their *content* may change per modules 12/13; their assertions and pass criteria may not.

### Other verification

- **Concurrency/anti-flake:** run the concurrency-bearing suites ≥20 times in a loop and confirm zero failures (module 12). A sleep replaced by a still-racy signal is not done.
- **Migration:** none added. `tests/security_foundation.rs`'s migration-contract test must still pass, confirming the new schema-per-fixture isolation does not collide with its create-and-drop-database strategy.
- **OpenAPI:** all 8 in-process spec tests in `src/http/mod.rs` pass **unmodified**, and the generated spec diffs empty against `main`. This is the structural proof no route or DTO changed.
- **Secret-leak (CONVENTIONS §8):** no new secret-bearing surface. `src/security/masking::tests` and `src/infra/repositories/setup.rs:164-179` (the guard asserting `SETUP_READINESS_SQL` never mentions `encrypted_payload`, `encrypted_data_key`, `key_hash`, `key_prefix`, `masked_secret`, `secret_fingerprint`) must still pass. Repository-trait fakes (module 8) must use synthetic values only.
- **Required gates (CONVENTIONS §2, verbatim, after every wave and at the end):**
  ```bash
  cargo fmt --check
  cargo clippy --workspace --all-targets --all-features -- -D warnings
  cargo test --workspace --all-features
  cargo build --release --locked
  ```
  plus clean PostgreSQL migration validation (migrations apply from an empty database and the DB-backed suite passes against it).

---

## Definition of Done

**Plan-specific**

- [ ] `src/application/admin.rs` no longer exists as a single 1,873-line file; `src/application/admin/` contains the per-context modules; `AdminService`'s public surface (all **46** method names, signatures, return types) is unchanged — verified by a method-signature snapshot taken before and after.
- [ ] `AdminCommandRunner` / `admin_command_spec` / the `claim_idempotency`→`finalize_idempotency` sequence are **byte-identical** to `main` (`git diff main -- src/application/admin_command.rs src/infra/repositories/admin.rs` shows no change to those paths beyond mechanical import moves).
- [ ] `grep -rn "rig_core" src/domain/` returns nothing.
- [ ] `PublicRepository`, `RuntimeRepository`, `ConversationRepository`, `SetupRepository` traits exist, are `#[async_trait]`, are implemented by their `Pg*` structs, and are re-exported from `src/infra/repositories/mod.rs`; **at least one unit test per trait** exercises a fake without a live Postgres connection, and at least one *application-service* unit test (`src/application/setup.rs`) runs Postgres-free.
- [ ] `src/orchestration/resolver.rs` and `src/http/chat.rs` do not exist; `ResolvedProvider` and `normalize_openai_base_url` are relocated and still used by `src/orchestration/executor.rs`; the divergent local `credential_aad` and `credential_priority` are **deleted**, and `src/security/crypto.rs:96-111` remains the single AAD implementation.
- [ ] `docs/i18n-response-catalog.json` has no duplicate keys and matches the Rust catalog exactly; the eight missing error entries exist in **both**; all four new i18n tests pass, and each was **observed failing** against an injected mismatch before the mismatch was reverted.
- [ ] Every admin list/filter endpoint has an e2e test asserting unknown-query-field rejection with a real unknown field name, and the response carries a non-empty `message_key` **and** `message`; the `PageQuery` doc comment records the P2-9 nuance.
- [ ] `tests/admin_idempotency.rs:977` no longer contains an unbounded `sleep()`; the three bounded poll sites are either converted to signals or annotated with the P2-12 rationale; the affected suites survive ≥20 consecutive runs.
- [ ] `tests/support/mod.rs` isolates each fixture in its own Postgres schema with teardown guaranteed on panic — verified by `schema_is_dropped_even_when_the_test_panics`, not by inspection. `TEST_SERIAL` is removed, or its remaining use is justified in a comment naming the specific process-global state it protects.
- [ ] `circuits.reset_all()` is no longer called from `listen_once`'s normal path; `reset_for_resource` is, with `resource_type`/`resource_id` parsed from the payload and a documented `reset_all()` fallback on malformed input; `unrelated_table_notify_leaves_all_circuits_intact` passes.
- [ ] `docs/project-structure.md` lists `src/application/`; `docs/todo.md`'s chat/`owner_scope` bullet is split, with the `owner_scope` line rewritten to state it is live and load-bearing.

**CONVENTIONS §8 compliance checklist**

- [ ] Work performed on branch `plan/06-architecture-test-hygiene`; PR opened with all required description sections (Plan link · Findings addressed · Migrations included · Breaking API/OpenAPI changes · Test evidence · Rollback procedure · Deferred follow-ups).
- [ ] All gates in CONVENTIONS §2 pass (Rust set; frontend set not applicable).
- [ ] **Unit tests** delivered and passing (table above).
- [ ] **E2E tests** delivered and passing at the HTTP level against real PostgreSQL 16 + pgvector (table above).
- [ ] Every new error/notice string has an i18n key + English default in the Rust catalog, mirrored into `docs/i18n-response-catalog.json`, with a test asserting presence. *(This plan adds no new string but closes eight pre-existing gaps and adds the enforcing tests.)*
- [ ] Frontend items — **not applicable** (no console code in this plan).
- [ ] Auth-touching items — **not applicable** (no auth code path changes semantics).
- [ ] No secret-leak: verified by the existing masking and `SETUP_READINESS_SQL` guard tests, plus the requirement that all repository fakes use synthetic values.
- [ ] PR **merged** with all gates green — not merely opened.

---

## Risks & Rollback

**Security.** Low — no security-relevant code path changes semantics. The main risk is *accidentally* altering idempotency/audit/authorization sequencing during the split. Mitigated three ways: the "mechanical move, not redesign" constraint; leaving `AdminCommandRunner` and the repository envelope completely untouched; and the two new e2e tests (`every_admin_mutation_still_writes_exactly_one_audit_row`, `every_admin_mutation_still_honours_its_required_scope`) that would catch a dropped call the existing suite might not.

**Data-migration.** None. No schema change ships to production; the only new DDL (`CREATE SCHEMA moira_test_*`) is test-only.

**Compatibility.** If the repository-trait extraction (module 8) accidentally changes a SQL query's behavior while "just" adding a trait, that is a regression — guarded by requiring trait method bodies to be a verbatim lift of the existing `pub async fn` bodies, not a rewrite.

**Test-isolation risk (new).** Schema-per-fixture plus database-wide `LISTEN/NOTIFY` is the sharpest edge in this plan. If cross-schema notification bleed causes flakes, the fallback is to skip the listener in fixtures that do not need it (module 13) rather than to reinstate the global mutex.

**Deployment.** None — no migration step, no config change, no restart-order dependency.

**Rollback procedure.** Each wave lands as its own reviewable commit within the single plan PR; the waves are file-disjoint by construction, so a regression traced to one wave can be `git revert`ed independently. Post-merge, `git revert` the merge commit restores `main` exactly — there is no data to unwind and no migration to reverse.

**Deferred follow-ups.** P2-5 (health/circuit state not an input to candidate ranking), P2-6 (connection-pool dev-scale sizing), P2-7 (embedding-dimension policy), P2-10/P2-11 (container/Helm hardening) remain open P2 findings **not** addressed here. Newly recorded during this re-audit: (a) `patch_credential` (`src/application/admin.rs:583-612`) bypasses `AdminCommandRunner` while its sibling mutations use it — an idempotency/atomicity inconsistency to fix deliberately, not incidentally; (b) `runtime_admin.rs` uses a two-phase, non-transactional idempotency scheme (`idempotency_replay`/`record_idempotency`, `:621-657`) unlike `admin.rs`'s transactional envelope, and supports no `If-Match` at all; (c) the trusted-JWT-issuer `PATCH`/`DELETE`/`enable`/`disable` handlers perform a read-then-compare version check **outside** the repository transaction (`src/http/admin.rs:1449-1452,1480-1483,1532-1535,1563-1566`), a TOCTOU window; (d) the `moira_runtime_config` NOTIFY payload carries no `tg_op`, limiting any future listener precision. All four are real, out of scope here, and must be tracked rather than silently dropped.
