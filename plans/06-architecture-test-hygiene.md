# Plan 06 — Architecture & Test Hygiene

> **Binding cross-cutting spec:** `plans/CONVENTIONS.md`. Where anything below conflicts with that file, **CONVENTIONS.md wins**. This plan has been re-audited against the real tree and brought into compliance with CONVENTIONS §1 (branch/PR), §2 (gates), §3 (unit **and** e2e), §4 (i18n), §8 (Definition of Done).

> **Rewritten 2026-07-26 (Wave 0) against `0b3301c`** — the commit on `main` that carries merged plans 02a, 02b, 03, 04, 05. See **§0 Rewrite provenance** for what drifted and why. Every line number, count, and file path below was re-derived from the tree at that commit; none was carried over from the previous draft.

---

## 0. Rewrite provenance — what drifted, and why this document changed

The previous draft of this plan was written before plans 02b, 03, 04, and 05 landed. Executing it as written would have wasted a full wave and, in one place, would not have compiled. The corrections:

| # | Previous draft said | Repo at `0b3301c` says | Consequence |
|---|---|---|---|
| 1 | Nothing at all about `actor_fingerprint` unification | `src/application/admin.rs:1666-1682` explicitly hands that work to *this plan* in a doc comment, and three divergent formulas write one unique index | The work would have been dropped silently. Now **Module 16**. |
| 2 | If-Match TOCTOU is "deferred follow-up (c)", 4 JWT-issuer sites | **33** production sites in `src/http/admin.rs`, across **two** services and **two** repositories | Undercounted by 8×. Now **Module 17**, with a recommendation to split it into its own plan — see §17.4. |
| 3 | Module 9: "delete `src/orchestration/resolver.rs`" | `RuntimeConfigCache` is **defined in that file** (`:22-26`, `:57-87`) and is load-bearing for `src/app/state.rs:41,89` and `src/infra/db.rs:13,45,61` | Deleting the file **breaks the build**. Module 9 is rewritten extract-first. |
| 4 | Module 9 treats `src/orchestration/executor.rs` as "the documented Rig-boundary owner" and relocates code *into* it | `executor.rs` is **itself dead** (`execute_chat`/`stream_chat` have zero callers in `src/` or `tests/`); the real Rig boundary per `CLAUDE.md` and `.claude/skills/moira-rig-integration` is `src/orchestration/runtime_factory.rs` | Modules 7 and 9 both retargeted. |
| 5 | Module 9 lists `executor.rs:11` as the only consumer of `normalize_openai_base_url` | `src/orchestration/runtime_factory.rs:26,101,141` is a **live** consumer | The function must survive the deletion; the plan never said so. |
| 6 | Module 10: JSON mirror has 63 entries / 61 unique; 8 error codes missing | **73 entries / 73 unique** (no duplicates); **7 of the 8** keys already exist | ~90% of Module 10 is already done by 02b/04/05. Re-scoped to the true residual. |
| 7 | Module 10 proposes `every_app_error_variant_code_has_a_catalog_entry` | Already exists as `every_error_message_key_resolves_to_a_catalog_entry` (`src/i18n/catalog/mod.rs:336`) | Would have been a duplicate test. Removed. |
| 8 | `src/application/admin.rs` is 1,873 lines; method line numbers `:50`–`:1295` | **2,436 lines**; methods at `:217`–`:1545` | Every line number in Modules 2-5 was wrong. Re-derived. |
| 9 | Repository-trait work is "~45 methods" (implied by "24 public methods" on the largest) | **78** trait-worthy methods across the four repos (public 16, runtime 31, conversation 30, setup 1) | Module 8 was undersized by ~70%. |
| 10 | Module 13 targets `tests/support/mod.rs` (496 lines) as *the* fixture | `tests/support/mod.rs` is **636 lines** and serves **14** suites; `tests/admin_idempotency.rs:28-30` has its **own** independent `SERIAL`+`Fixture`; `tests/execution_policy_if_match.rs:86` has a third | Module 13 would have isolated one of three fixtures and left the rest serialized. |
| 11 | Module 12 sleep sites at `admin_idempotency.rs:977`, `execution_lifecycle.rs:979,1002`, `mock_openai.rs:330,373` | `:987`, `:1600`, `:1623`, `:351`, `:394` | All five line numbers stale; the classification (one unbounded, three bounded) still holds. |
| 12 | "8 in-process spec tests in `src/http/mod.rs`", `mod tests` at `:213` | `mod tests` at `:580`; **30+** spec tests including plan 05's drift gate | The "8 tests pass unmodified" proof is now a much larger, stronger surface. |
| 13 | No mention of `SecretString` hardening | Handed to this plan by plan 05's leak-suite work | Now **Module 0**, commit #1, ahead of the split. |
| 14 | No mention of plan 05's new test suites | `tests/metrics_endpoint.rs`, `tests/openapi_drift.rs` build on `tests/support/mod.rs`; `docs/openapi.json` is now a frozen, drift-gated artifact | New collision-surface section (§Multi-Agent Workflow → "Plan 05 collision surface"). |

Two claims in the previous draft were re-checked and found **still correct**, and are kept: `AdminService` has exactly **46** `pub async fn` methods, and `PageQuery` has exactly **26** fields.

---

## Summary

**Objective.** Pay down structural debt in Moira's admin/orchestration/domain layers and its test harness without changing any externally observable behavior: harden the generated-API-key type so a plaintext leak cannot compile, split the `AdminService` god-object into focused per-context services, stop leaking a Rig type into `src/domain`, add repository traits for the four untested-in-isolation repos, extract the live code out of the dead provider-resolution path and delete the rest, unify the three divergent `actor_fingerprint` formulas that write one unique index, close the residual i18n catalog gap CI cannot currently see, and fix two pieces of doc drift. This plan also **owns the test-hygiene work itself** (P2-12 acknowledgement gates, P2-13 database test isolation/teardown), and **inventories** the 33-site If-Match TOCTOU surface for a dedicated follow-up plan.

**Why ordered here.** Per `plans/01-roadmap-and-dependencies.md` §1.2 and §2, security-critical iterations (03, 07) must stay pure — no refactors mixed in. This iteration is the refactor, isolated on purpose. It is "recommended but not a hard gate" before 07 (`plans/01` §2, `-.recommended.->` edge) because a clean `AdminService`/repository-trait surface makes the identity work in 07 easier to land safely, but 07 does not structurally depend on 06 — 07 only adds a new, additive `AdminIdentityService`/`admin_identities` slice. If schedules force a choice, 07 must not block on 06.

**User-visible outcome.** None on the wire. The HTTP surface, OpenAPI contract, request/response shapes, and database schema are unchanged. The only externally observable artifacts are a smaller circuit-reset blast radius (P2-14), tighter replay isolation on the runtime-admin routes (Module 16 — a *narrowing* of what counts as the same actor, never a widening), and CI catching classes of drift it cannot see today.

**Included scope.** P2-0 (`SecretString` hardening of `GeneratedApiKey`), P2-1 (split `AdminService`), P2-2 (remove `rig_core::completion::Message` from `src/domain`), P2-3 (repository traits for public/runtime/conversation/setup), P2-4 (extract the live items from `src/orchestration/resolver.rs`, then delete the dead path), P2-8 (residual i18n catalog gap + mirror-drift tests), P2-9 (per-endpoint unknown-query-field test), P2-12 (replace ungated `sleep()` interleaving with acknowledgement gates), P2-13 (DB test isolation/teardown), P2-14 (scope `reset_all()` to the changed resource), P2-15 (**new** — unify `actor_fingerprint`), P2-16 (**new** — inventory + harness for the If-Match TOCTOU surface), P3-9 (docs drift).

**Excluded scope.** No new endpoints, no product migrations, no scope/authz changes, no behavior change to routing/credential-resolution semantics, no changes to `AdminService`'s external call signatures. **The If-Match TOCTOU *fix* is explicitly excluded** — this plan delivers only the inventory, the mechanical recipe, and the failing-first regression harness; the 33-handler change is recommended as its own plan **06b** (§17.4). P2-5/P2-6/P2-7/P2-10/P2-11 (routing quality, pool sizing, embedding-dimension policy, container/Helm hardening) are separate P2 findings **not** in this iteration.

---

## Branch & Pull Request (CONVENTIONS §1)

- **Branch:** `plan/06-architecture-test-hygiene`, cut from the **current `main`** (`0b3301c`). Not stacked on any other plan branch.
- **Commits:** Conventional Commits, matching existing history style. Commit #1 is fixed: `fix(security): make GeneratedApiKey.raw_key a SecretString so plaintext cannot be serialized`. Thereafter: `refactor: split AdminService into per-context services`, `refactor: unify the actor fingerprint across admin, runtime-admin, and public`, `test: isolate integration tests per Postgres schema`, `fix: scope circuit reset to the changed resource`, `docs: record src/application and src/i18n in project-structure`.
- The PR is **not opened** until every gate in §2 / Verification passes locally.
- **PR description — required sections:**
  - **Plan link** — `plans/06-architecture-test-hygiene.md`
  - **Findings addressed** — P2-0, P2-1, P2-2, P2-3, P2-4, P2-8, P2-9, P2-12, P2-13, P2-14, P2-15, P2-16 (inventory only), P3-9
  - **Migrations included** — **none** (the only DDL is test-only `CREATE SCHEMA moira_test_*`, which never runs outside `cargo test`)
  - **Breaking API/OpenAPI changes** — **none**; include the `docs/openapi.json` diff showing it is empty (see §"OpenAPI regeneration" below)
  - **Test evidence** — unit + e2e output summary (see Verification)
  - **Rollback procedure** — see Risks & Rollback
  - **Deferred follow-ups** — plan 06b (If-Match TOCTOU, 33 sites), P2-5, P2-6, P2-7, P2-10, P2-11, and the four items listed at the end of Risks & Rollback
- **Done means merged.** Opening the PR is not done. The plan is done when the PR is merged with all gates green and every Definition of Done item objectively verified.
- **Ordering:** this plan changes **no** OpenAPI path, operation, or schema, so it is not subject to the "must land before 05's OpenAPI-drift gate freezes the spec" constraint (`CONVENTIONS §1.6`). It must nonetheless prove the spec is byte-identical (see Verification).
- Never force-push this branch; plan 07 may be developed alongside it.

### OpenAPI regeneration — which modules require it

Plan 05 froze the spec: `docs/openapi.json` (651 KB) is committed and guarded by `src/http/mod.rs`'s `committed_openapi_matches_the_generated_document` (`:1649`) plus `tests/openapi_drift.rs`. **No module in this plan requires regeneration**, and each is expected to leave the spec byte-identical for a specific reason:

| Module | Touches the spec? | Why not |
|---|---|---|
| 0 (`SecretString`) | No | `GeneratedApiKey` is internal; the wire DTO `ApiKeySecretResponse.secret: Option<String>` (`src/domain/admin.rs:497`) is unchanged. |
| 1-6 (`AdminService` split) | No | Handler files and their `#[utoipa::path]` attributes are untouched; only the service module layout moves. |
| 7 (`DomainMessage`) | No | `ExecutionCommand` (`src/domain/runtime.rs:241-257`) derives `Debug, Clone, Serialize, Deserialize` but **not** `ToSchema`, and is not referenced by any registered operation. Do not add `ToSchema`. |
| 8 (repository traits) | No | Infra layer only. |
| 9 (dead resolver path) | No | `src/http/chat.rs` was never registered in `src/http/mod.rs:1-6`, so it contributes nothing to the document. |
| 10 (i18n) | No | The catalog is a runtime registry plus a JSON doc mirror; neither is part of the OpenAPI document. |
| 11 (unknown query field) | No | `PageQuery`'s `#[into_params]` shape is unchanged — only a doc comment and tests are added. |
| 12-13 (tests) | No | Test-only. |
| 14 (`reset_for_resource`) | No | Internal listener behavior. |
| 15 (docs) | No | Markdown only. |
| 16 (`actor_fingerprint`) | No | The fingerprint is never on the wire; it is a stored ledger column and an advisory-lock input. |
| 17 (If-Match **inventory**) | No | No handler signature changes in this plan. |

**Therefore the drift gate must pass untouched.** If it does not, something in this plan changed the contract and that is a finding, not a reason to regenerate. Only if a change is deliberately accepted:

```bash
UPDATE_SNAPSHOTS=1 cargo test --lib http::tests::committed_openapi_matches_the_generated_document
```

If plan 06b (If-Match) is later folded in, re-verify rather than assume: pushing `expected_version` into the service layer does not alter any `#[utoipa::path]` attribute, and `If-Match` is already declared required on all 33 operations (guarded by `every_if_match_operation_declares_the_documented_precondition`, `src/http/mod.rs:1122`), so the spec should still diff empty — but prove it.

---

## Findings Addressed

All evidence below re-verified against `0b3301c` on 2026-07-26.

| ID | Evidence | Current behavior |
|----|----------|-------------------|
| **P2-0** *(new)* | `src/security/api_keys.rs:19-26` — `#[derive(Debug, Clone)] pub struct GeneratedApiKey { pub raw_key: String, … }`. The plaintext key is a bare `String` for its whole lifetime. Consumed at `src/application/admin.rs:1054, 1161, 1265` (`secret: Some(generated.raw_key)`) and `api_keys.rs:98` (a test). Plan 05's `9531b90` shipped an injected probe writing `"raw": generated.raw_key` into `audit_logs.metadata`; two "remove probe artifacts" commits missed it because they only cleaned `tests/`. | Nothing in the type system stops `json!({ "raw": generated.raw_key })`. The only defence is `tests/secret_leak_snapshots.rs`, a runtime test — it caught the real leak, but only after the code was written, compiled, and committed twice. |
| P2-1 | `src/application/admin.rs` — **2,436 lines**, **46** `pub async fn` methods on one `AdminService<'a>` (`grep -c 'pub async fn'` → 46; `create_application:217` … `get_audit_log:1545`) spanning ~9 bounded contexts. `rotate_key:1205`, `revoke_key:1280`, `delete_key:1306` are **table-generic** (each takes `table: &str` selecting `system_api_keys` vs `consumer_api_keys`), which constrains the split (module 5). The file also owns shared, non-per-context surface: `PageRequest` (`:78-104`), `From<&PageQuery> for PageRequest` (`:105`), `actor_fingerprint` (`:1682`), `admin_command_spec` (`:1699`), `success_audit` (`:1719`), and a 316-line `#[cfg(test)] mod tests` (`:2120-2436`) whose 15 tests are almost entirely about **pagination**, not about any one context. | One `impl AdminService` block owns unrelated domains; per-context ownership and testing are impossible without whole-file review. |
| P2-2 | `src/domain/runtime.rs:2` (`use rig_core::completion::Message;`) and `:249` (`pub messages: Vec<Message>` inside `ExecutionCommand`, struct declared `:241-257`). **Wider than the previous draft admitted:** `rig_core` is imported in four places under `src/application/` (`execution.rs` ×4 — `build_completion_request:1790`, `first_text:1952`; `public.rs` ×1) as well as `src/orchestration/{executor,runtime_factory}.rs`. | A `domain` DTO is generic over an upstream crate's execution-primitive type, violating `docs/project-structure.md` ("domain must stay dependency-light") and `CLAUDE.md` ("Rig owns AI execution primitives"). |
| P2-3 | `src/infra/repositories/admin.rs:92-93` is the **only** trait (`#[async_trait] pub trait AdminRepository`, second `#[async_trait]` impl block at `:799`, re-exported from `repositories/mod.rs`). No trait for `PgPublicRepository` (`public.rs:22`), `PgRuntimeRepository` (`runtime.rs:25`), `PgConversationRepository` (`conversation.rs:97`), `PgSetupRepository` (`setup.rs:18`). Trait-worthy public methods, excluding `new()`: **public 16** (`:70`–`:689`), **runtime 31** (`:84`–`:974`), **conversation 30** (`:271`–`:1287`), **setup 1** (`inspect:27`) — **78 total**, not the ~45 previously implied. | Only the admin repo is mockable. Application-layer unit tests for public execution, runtime resolution, conversation, and setup must hit a real Postgres or not exist. **Making trait-based mocking possible is the point of P2-3** — see Verification for the unit tests this unlocks. |
| P2-4 | `src/orchestration/resolver.rs` (422 lines): `RuntimeConfigCache` struct `:22-26` + `impl :57-87`, `CacheEntry :28-32`, `ResolvedProvider :35`, `CredentialCandidate :42`, `resolve_provider:89`, `get_provider:124`, `find_default_provider:145`, `resolve_api_key:159`, `credential_priority:255`, local `credential_aad:272`, `normalize_openai_base_url:283`, `provider_runtime_select_sql:294`, `scope_type_to_aad:323`, `api_key_from_credential_payload:332`, tests `:346`. `grep -rn "resolver::" src/ tests/` → only `orchestration/mod.rs:11` and `orchestration/executor.rs:11`. `src/http/chat.rs` is not declared in `src/http/mod.rs:1-6` (`admin, conversation, health, observability, openapi, public`), so it **is not compiled**. `src/orchestration/executor.rs` (168 lines) exports `execute_chat`/`stream_chat` via `mod.rs:10` and has **zero** callers in `src/` or `tests/` — it is dead too. `ChatCompletionRequest` (`src/domain/models.rs:117`) is referenced only by those two dead files. | A second, legacy provider-resolution implementation sits in the tree with a **divergent credential AAD** (`resolver.rs:272-281`, 3-part `provider:{}:scope:{:?}:owner:{}`) that could not decrypt any live credential — two credential-priority algorithms and two AAD formats to keep straight, one of which is silently wrong. And the file cannot simply be deleted: it also holds `RuntimeConfigCache`, which the live process depends on. |
| P2-8 | **Mostly closed since the previous draft.** `docs/i18n-response-catalog.json` now has **73 entries / 73 unique keys** — the two duplicates are gone (02b/04/05). Seven of the eight previously-missing codes are present in **both** `src/i18n/catalog/errors.rs` and the JSON. `src/i18n/catalog/mod.rs` now has 15 tests including `every_error_message_key_resolves_to_a_catalog_entry` (`:336`), which is the previously-proposed `every_app_error_variant_code_has_a_catalog_entry` under another name. **Residual, verified:** (a) `routing_policy_provider_model_mismatch` (emitted at `src/application/runtime_admin.rs:308`) has **no** catalog entry and **no** JSON entry; (b) **nothing in the test suite reads `docs/i18n-response-catalog.json`** — `grep -rn "i18n-response-catalog" src/ tests/` matches only doc comments, so the Rust↔JSON mirror is entirely unguarded; (c) all four `moira.notice.*` entries still have **zero** production consumers. | One error code ships a `message_key` that resolves to nothing. More importantly, the mirror can drift arbitrarily and CI will not notice — the duplicates that existed before were fixed by hand, not by a gate, and nothing prevents them recurring. |
| P2-9 | `src/domain/admin.rs:31-61` — `#[serde(deny_unknown_fields)]` at `:32`, `#[into_params(parameter_in = Query)]` at `:33`, `pub struct PageQuery {` at `:34`, closing `:61`; **26 fields** (`limit` … `occurred_after`). | `deny_unknown_fields` is applied once, globally, to one struct shared by every admin list/filter endpoint. An endpoint that only honors `status`+`limit` still **accepts** `provider_id` because `PageQuery` defines it for a different endpoint. No test enumerates, per endpoint, which fields are honored vs. silently ignored-but-typed, and no test proves the global rejection works at all. |
| P2-12 | `grep -rn "sleep(" tests/` — **one genuinely ungated sleep**: `tests/admin_idempotency.rs:987` (`sleep(Duration::from_millis(50)).await` after `task.abort()` + `let _ = task.await;` + `blocker.rollback()`, before asserting the application row count is 0). Note `let _ = task.await;` is already present at `:986`, so the cancellation *is* observed — the sleep is now guarding only the rollback's visibility, which makes it a bounded-poll candidate rather than a signal candidate. Bounded poll intervals inside `timeout(...)` loops: `admin_idempotency.rs:1269` (`wait_for_audit_lock`), `execution_lifecycle.rs:1600` (`wait_for_public_cancellation`), `execution_lifecycle.rs:1623` (`wait_for_attempt_status`). Two fully-qualified sleeps in `tests/support/mock_openai.rs:351,394`. | One unbounded sleep is a real latent CI flake. The three bounded poll loops are acceptable but are still time-based where a notification is available. |
| P2-13 | **Three independent fixtures, not one.** (a) `tests/support/mod.rs` — **636 lines**, serving **14** suites; `TEST_SERIAL` declared `:44`, acquired `:130`, held in `_serial` on `LifecycleFixture` (`:118`, `new()` `:128`); isolation relies solely on the `Uuid::now_v7()` suffix at `:148`/`:199`; CI fail-closed correct at `:453-455` (`panic!` when **`CI=true`** and `MOIRA_TEST_DATABASE_URL` is absent — value check per CONVENTIONS §3); **no `impl Drop`, no truncate/rollback/cleanup anywhere in the file**. (b) `tests/admin_idempotency.rs:28-30` — its **own** `static SERIAL: LazyLock<Mutex<()>>` and `struct Fixture`, with its own `db::migrate` call and its own identical CI fail-closed block; it does **not** use `tests/support`. (c) `tests/execution_policy_if_match.rs:86` — a third `struct Fixture`. Separately, `tests/security_foundation.rs` creates and force-drops an entire database per run, and `tests/http_middleware_contract.rs` / `tests/jwks_hardening.rs` call `db::migrate` directly. | Every integration test serializes on one of three process-wide mutexes against one shared physical database with no schema or transaction isolation. Rows from a failed test are never removed and accumulate permanently; suite parallelism is capped at 1 *per mutex*, and the three mutexes do not coordinate with each other, so cross-fixture interference is possible today. |
| P2-14 | `src/infra/db.rs:59-80` (`listen_once`) — **the payload is never parsed**. Every `NOTIFY moira_runtime_config` unconditionally triggers `cache.invalidate_all()` (`:71`), `runtime_handles.invalidate_all()` (`:72`), **and** `circuits.reset_all()` (`:73`). The trigger function (`migrations/0004_admin_api_contract.sql:108-127`) emits `json_build_object('resource_type', tg_table_name, 'resource_id', changed_id::text)` (`:116-119`) — **no `tg_op`**. **26** `create trigger …_runtime_config_notify` statements across migrations 0002–0007, covering **20** distinct tables. `CircuitBreakerRegistry` (`src/orchestration/controls.rs:494`) keys on `(provider_id, model_id)` (`states: Arc<Mutex<HashMap<(Uuid, Uuid), CircuitEntry>>>`, `:495`); `reset_all` (`:606`) clears the whole map. | A conversation-policy write, a RAG-document write, or any of 20 tables' writes discards in-flight circuit-breaker state for **every** provider-model on the instance — throwing away exactly the protection that exists to shield a known-bad upstream. |
| **P2-15** *(new)* | **Three fingerprint formulas write one unique index.** `migrations/0003_security_foundation.sql:360-361` declares `idempotency_records_unique on idempotency_records (idempotency_key_hash, actor_fingerprint, operation)`. Writers: (a) `src/application/admin.rs:1682` `pub(crate) fn actor_fingerprint` — **10 fields**, `serde_json::to_vec` of a tuple `(actor_type, subject, api_key_id, trusted_jwt_issuer_id, internal_application_id, application_id, tenant_id, external_user_id, external_tenant_id, delegated_subject)`; reused verbatim by `src/application/conversation.rs:1303`. (b) `src/application/runtime_admin.rs:744` `fn actor_fingerprint` — **3 fields**, `format!("{:?}:{}:{}", actor_type, subject, api_key_id)`. (c) `src/application/public.rs:1893` `fn public_actor_fingerprint` — **4 fields**, `format!("{:?}:{}:{}:{}", actor_type, subject, api_key_id, application_id)`. All three feed `idempotency_records` (admin via `src/infra/repositories/admin.rs:690`, public via `src/infra/repositories/public.rs:249`). The fingerprint is **also** an input to `advisory_lock_key` (`src/infra/repositories/admin.rs:1934-1938`, called at `:625`), so it partitions the `pg_try_advisory_xact_lock` keyspace too. `src/application/admin.rs:1666-1682` documents the divergence and **explicitly assigns the fix to this plan**. | On the runtime-admin routes (`route.create`, `routing_policy.create`, `agent_profile.create`, `provider_runtime_policy.*`), two actors differing only by trusted-JWT issuer or by tenant produce **identical** fingerprints, so those routes do **not** isolate replay across issuers or tenants: actor A's `Idempotency-Key` can replay actor B's stored response. On the public route the same holds for tenant and delegated-subject. Additionally, the three formulas partition the advisory-lock keyspace differently, so lock contention characteristics differ per route family for no principled reason. |
| **P2-16** *(new)* | **33 If-Match TOCTOU sites**, all in `src/http/admin.rs`, all production (the file has no `#[cfg(test)]` module). The pattern is invariably `ensure_version(service.get_X(&actor, id).await?.version, require_if_match(&headers)?)?;` followed by `service.patch_X(...)` in a **separate** transaction. `ensure_version` is defined at `:92`; `require_if_match` at `:66`. The 33 sites, by owning service: **`AdminService` — 21**: `patch_application:190`, `delete_application:220`, `enable_application:249`, `disable_application:280`, `patch_provider:440`, `delete_provider:470`, `enable_provider:499`, `disable_provider:528`, `patch_provider_model:619`, `delete_provider_model:673`, `enable_provider_model:704`, `disable_provider_model:735`, `patch_credential:840`, `delete_credential:870`, `enable_credential:930`, `disable_credential:961`, `delete_user_credential:1049`, `patch_trusted_jwt_issuer:1452`, `delete_trusted_jwt_issuer:1484`, `enable_trusted_jwt_issuer:1536`, `disable_trusted_jwt_issuer:1567`. **`RuntimeAdminService` — 12**: `patch_route_definition:1715`, `delete_route_definition:1747`, `enable_route_definition:1776`, `disable_route_definition:1807`, `patch_routing_policy:1911`, `delete_routing_policy:1943`, `enable_routing_policy:1972`, `disable_routing_policy:2003`, `patch_agent_profile:2107`, `delete_agent_profile:2139`, `enable_agent_profile:2168`, `disable_agent_profile:2199`. **Plan 04 already fixed exactly one of the original 34**: `rotate_credential:902` reads `let expected_version = require_if_match(&headers)?;` at `:910` and passes it down to `AdminService::rotate_credential`, which threads it into `admin_command_spec(...).with_expected_version(Some(expected_version))` (`src/application/admin.rs:830`) so the check happens **inside** the command transaction. One further site, `put_application_execution_policy:335`, already pushes `Some(require_if_match(&headers)?)` into `PublicExecutionService` and is likewise not a TOCTOU site. | Between the version read and the mutation, a concurrent writer can bump the row's version. The read-then-write pair is not atomic, so a client's `If-Match` precondition can pass against a version that no longer exists by the time the write lands — a lost update. The window is small but real, and is exactly the failure mode `If-Match` exists to prevent. `tests/execution_policy_if_match.rs` demonstrates the correct shape for the one endpoint that has it. |
| P3-9 | `docs/project-structure.md:8-21` omits **two** layers, not one: `src/application/` (the largest — `admin.rs` alone is 2,436 lines, `execution.rs` is 95 KB) and `src/i18n/`. `src/http/chat.rs` (51 lines) is uncompiled dead weight. `docs/todo.md:10` bundles "unregistered chat-route types" with `owner_scope` as legacy-to-remove — but `OwnerScope` lives in `src/domain` and is live/load-bearing in `src/security/crypto.rs` credential envelopes; what is actually dead is `resolver.rs`'s resolution functions and its divergent local `credential_aad`. `docs/todo.md:58` separately (and correctly) says to keep `/v1/chat/completions` unregistered. | Structure doc is stale (missing the two biggest additions since it was written); a genuinely dead file exists uncounted; the todo's framing invites someone to delete live, load-bearing code while "cleaning up." |

---

## Architecture

### Components & ownership (per `docs/project-structure.md`)

- `src/security/api_keys.rs` — `GeneratedApiKey.raw_key` becomes `secrecy::SecretString`. **Verified type-system guarantee:** `secrecy` is pinned at `0.8.0` (`Cargo.lock`) with default features (`default = ["alloc"]`, no `serde`). Even *with* the `serde` feature enabled by any downstream crate, `impl Serialize for Secret<T>` (`secrecy-0.8.0/src/lib.rs:275-277`) requires `T: SerializableSecret`, and `String` does **not** implement that marker — so `SecretString: !Serialize` **unconditionally**, and `json!({"raw": generated.raw_key})` cannot compile. `string.rs:11-12` does provide `impl DebugSecret for String` and `impl CloneableSecret for String`, so `#[derive(Debug, Clone)]` on `GeneratedApiKey` keeps working and `Debug` prints `Secret([REDACTED alloc::string::String])`. The three call sites become an explicit, greppable `generated.raw_key.expose_secret().to_string()`.
- `src/application/` (undocumented today — P3-9 fixes this) becomes a directory of per-context service modules instead of one file:
  - `src/application/admin/mod.rs` — thin re-export facade (keeps `application::AdminService` name and constructor stable for `src/http/admin.rs`, `src/main.rs`, `tests/support/mod.rs`, and `tests/admin_idempotency.rs` call sites)
  - `src/application/admin/applications.rs` — `ApplicationAdminService`
  - `src/application/admin/providers.rs` — `ProviderAdminService` (provider + provider-model)
  - `src/application/admin/credentials.rs` — `CredentialAdminService`
  - `src/application/admin/keys.rs` — `ApiKeyAdminService` (system **and** consumer keys in one service: `rotate_key`/`revoke_key`/`delete_key` are table-generic, so splitting into two files would force duplicating or awkwardly sharing those three)
  - `src/application/admin/jwt_issuers.rs` — `JwtIssuerAdminService`
  - `src/application/admin/audit.rs` — `AuditAdminService`
  - `src/application/admin/shared.rs` — the **non**-per-context surface that today sits in `admin.rs` and must not be scattered: `PageRequest` (`:78-104`) + `From<&PageQuery>` (`:105`), `actor_fingerprint` (`:1682`), `admin_command_spec` (`:1699`), `success_audit` (`:1719`), and request-validation helpers currently inlined across the file. The 15 tests in `admin.rs:2120-2436` are overwhelmingly about pagination and cursor scoping; they move **here**, not into the per-context files.
  - **No new idempotency helper is invented.** The envelope already exists and is already shared: `AdminCommandRunner` / `admin_command_spec` / `AdminCommandMutation` in `src/application/admin_command.rs`, driving `PgAdminCommandTransaction`'s `claim_idempotency` (`src/infra/repositories/admin.rs:~600-705`, advisory lock at `:625-628`), `begin_command_savepoint` (`:707`), `release_command_savepoint` (`:714`), `rollback_command_savepoint` (`:721`), `finalize_idempotency` (`:728`). Every sub-service keeps calling `AdminCommandRunner::new(self.repo.clone()).execute(spec, …)` exactly as `admin.rs` does today.
- `src/domain/runtime.rs` stays domain-owned but drops the Rig dependency; a new `src/domain/message.rs` defines `DomainMessage`, with `From`/`TryFrom` conversions to/from `rig_core::completion::Message` living in **`src/orchestration/runtime_factory.rs`** — the documented Rig boundary per `CLAUDE.md` and `.claude/skills/moira-rig-integration/SKILL.md`. **Not** `executor.rs`, which is dead code being deleted in module 9.
- `src/infra/repositories/{public,runtime,conversation,setup}.rs` each grow a trait (`PublicRepository`, `RuntimeRepository`, `ConversationRepository`, `SetupRepository`) mirroring `AdminRepository` (`admin.rs:92-…`, `#[async_trait]`, methods returning `Result<_, AppError>`); the concrete `Pg*` structs implement them. `src/application/*.rs` consumers switch from the concrete struct to `Arc<dyn Trait + Send + Sync>`.
- `src/orchestration/resolver.rs` is **emptied, not deleted in one step**: `RuntimeConfigCache` + `CacheEntry` move to a new `src/orchestration/runtime_cache.rs`, and `normalize_openai_base_url` moves to a new `src/orchestration/provider_url.rs`. Only then are the remaining dead items and the file removed. `src/orchestration/executor.rs`, `src/http/chat.rs`, and `ChatCompletionRequest`/`ChatMessage` go with it.
- `src/i18n/catalog/mod.rs` gains the mirror-drift and completeness tests it currently lacks; `src/i18n/catalog/errors.rs` and `docs/i18n-response-catalog.json` gain the one missing entry.
- `src/domain/admin.rs` gains a documented note on the `PageQuery` nuance; `src/http/admin.rs` gains per-endpoint unit tests and a new e2e suite covers it at HTTP level.
- All three test fixtures (`tests/support/mod.rs`, `tests/admin_idempotency.rs`, `tests/execution_policy_if_match.rs`) move from "global serial mutex + shared DB" to "one Postgres schema per fixture, dropped by a guard."
- `src/infra/db.rs::listen_once` parses the NOTIFY payload and scopes the circuit reset.
- A single `actor_fingerprint` in `src/application/admin/shared.rs` becomes the only fingerprint formula, with a read-path legacy fallback for in-flight rows.

### Data flow

No data-flow change for the public/admin API. Internally, `src/http/admin.rs` handlers keep calling the same `AdminService` method names on the same struct; the facade delegates to the new sub-service field. **No HTTP handler file changes** in this iteration (except additive `#[cfg(test)]` modules).

### Security boundaries

Unchanged in shape, tightened in two places.

- The split does not change which scope gates which operation — `AuthorizationService::require` calls stay exactly where they are today, inside each relocated method. The transaction envelope (advisory lock via `advisory_lock_key` → savepoint → business logic → release/rollback → finalize) is preserved verbatim because the shared `AdminCommandRunner` is not touched at all.
- **Module 0 tightens** the type of a secret: a plaintext API key can no longer be serialized by accident.
- **Module 16 tightens** replay isolation: after unification, two actors that differ in issuer, tenant, application, or delegated subject can no longer replay each other's runtime-admin or public responses. This is a narrowing of the equivalence class — strictly safer. It is nonetheless a **behavior change** and is called out as such in the PR.

### DB/migration changes

**None.** No product migration is added. Module 16 is handled by a read-path fallback, not by a backfill (see §16.3). For P2-13, the three fixtures gain a test-only helper that runs `CREATE SCHEMA moira_test_<uuidv7>`, sets `search_path`, runs `sqlx::migrate!` against that schema, and drops it on teardown. This is test-only DDL and never runs outside `cargo test`.

### API & OpenAPI changes

**None.** `src/http/mod.rs`'s in-process spec tests (`mod tests` at `:580`; 30+ tests including `generated_openapi_covers_every_registered_route:592`, `public_document_filters_admin_paths_and_keeps_operational_paths:706`, `every_if_match_operation_declares_the_documented_precondition:1122`, `every_local_schema_reference_resolves:1180`, `committed_openapi_matches_the_generated_document:1649`) must pass **unmodified** — that is the structural proof no route changed. See §"OpenAPI regeneration" for the per-module reasoning.

### Backward compatibility

Preserved on the wire. External clients, the OpenAPI spec, and the production DB schema are byte-identical before/after. The one compatibility surface with a documented transition is `idempotency_records` rows written before Module 16 lands — handled by the read-path fallback in §16.3, not by breaking them.

### Deployment implications

None — no migration, no config change, no restart-order concern. Module 16's fallback means a mixed-version fleet is safe: an old replica writes an old fingerprint, a new replica reads it via the fallback.

### Failure & recovery

The primary failure mode is a *regression* introduced during the split (e.g. an `AuthorizationService::require` call dropped when a method moves files). Mitigation: every method move is a mechanical cut-paste-adjust-imports change reviewed against the original `git diff` line-for-line (no "while I'm here" edits), and the full existing suite (`tests/admin_idempotency.rs`'s 9 tests in particular) must pass unmodified after **each** service split, not just at the end. Rollback is a plain `git revert`; there is no data migration to unwind.

---

## Detailed Implementation

### Module 0 — `SecretString` hardening of `GeneratedApiKey` (P2-0) — **commit #1**

**This lands first, before anything else, and alone.** Its three call sites are inside `src/application/admin.rs` at `:1054`, `:1161`, `:1265`; if the `AdminService` split (modules 1-6) lands first, those three lines move files and this change collides with it for no reason.

1. `src/security/api_keys.rs:21` — `pub raw_key: String` → `pub raw_key: secrecy::SecretString`. `:50` — `raw_key,` → `raw_key: SecretString::new(raw_key),`. Keep `#[derive(Debug, Clone)]`: `secrecy-0.8.0/src/string.rs:11-12` implements `DebugSecret` and `CloneableSecret` for `String`, so both derives still resolve, and `Debug` now redacts.
2. `src/security/api_keys.rs:98` (test) — `.verify(&generated.raw_key, …)` → `.verify(generated.raw_key.expose_secret(), …)`.
3. `src/application/admin.rs:1054, 1161, 1265` — `secret: Some(generated.raw_key)` → `secret: Some(generated.raw_key.expose_secret().to_string())`. **Do not** change `ApiKeySecretResponse.secret` (`src/domain/admin.rs:497`) — it is the once-only wire envelope and must stay `Option<String>` so it can serialize exactly once, at exactly one place, visibly.
4. Add `use secrecy::{ExposeSecret, SecretString};` where needed. The repo already uses this idiom at `src/domain/runtime.rs:397`, `src/application/execution.rs:9,1011`, and `src/orchestration/runtime_factory.rs:15,92` — match it.
5. **Prove the guarantee, do not assert it.** Add a `#[cfg(test)]` compile-fail note *and* a positive test in `src/security/api_keys.rs`:
   - `generated_api_key_debug_redacts_the_plaintext` — `format!("{:?}", generated)` must not contain the plaintext and must contain `REDACTED`.
   - `generated_api_key_plaintext_is_only_reachable_through_expose_secret` — round-trips through `expose_secret()` and asserts the hash verifies.
   - During development, temporarily write `serde_json::json!({ "raw": generated.raw_key })` and confirm it **fails to compile** with `SecretString: !Serialize`. Record the exact compiler error in the PR. Revert before commit. *(A `trybuild` case would be better and is welcome, but `trybuild` is not currently a dependency and adding one for this is out of proportion; the recorded transcript is the accepted evidence.)*
6. Gate this commit on its own before proceeding: `cargo fmt --check && cargo clippy --workspace --all-targets --all-features -- -D warnings && cargo test --workspace --all-features`.

### Module 1 — Confirm the shared idempotency envelope (no extraction)

- Read `src/application/admin_command.rs` and `src/application/admin.rs:217-259` (`create_application`, the reference caller) before touching anything. Confirm the pattern is: `authz.require(...)` → `admin_command_spec(ctx, actor, "<op>", json!({…}), &request)?` (optionally `.with_expected_version(...)`) → `AdminCommandRunner::new(self.repo.clone()).execute(spec, |transaction| Box::pin(async move { … AdminCommandMutation::new(record, status, resource_id) }))` → `if !outcome.replayed { self.schedule_runtime_cache_invalidation(); }`.
- **Do not refactor, re-implement, or "generalize" this.** Sub-services call it unchanged. The only permitted change is import paths.
- Record the confirmed pattern in the Wave 0 reference document so every sub-service agent copies the same shape.

### Module 2 — `src/application/admin/applications.rs`

Move verbatim: `create_application` (`:217`), `list_applications` (`:260`), `get_application` (`:276`), `patch_application` (`:285`), `delete_application` (`:317`), `set_application_enabled` (`:338`). Mirror `AdminService`'s existing field shape for construction — do not redesign it.

### Module 3 — `src/application/admin/providers.rs`

Move verbatim: `create_provider` (`:369`), `list_providers` (`:424`), `get_provider` (`:440`), `patch_provider` (`:445`), `delete_provider` (`:474`), `set_provider_enabled` (`:494`), `create_provider_model` (`:523`), `list_provider_models` (`:568`), `patch_provider_model` (`:589`), `get_provider_model` (`:611`), `delete_provider_model` (`:620`), `set_provider_model_enabled` (`:640`). Providers and provider-models stay one service — a provider-model always requires its parent provider and they already share private helpers. Verify the shared-helper boundary before splitting further; do not force an artificial split that duplicates a private helper.

### Module 4 — `src/application/admin/credentials.rs`

Move verbatim: `create_credential` (`:669`), `list_credentials` (`:732`), `list_user_credentials` (`:748`), `get_credential` (`:770`), `patch_credential` (`:781`), `rotate_credential` (`:812`), `validate_credential` (`:885`), `set_credential_enabled` (`:920`), `delete_credential` (`:951`), `delete_user_credential` (`:975`).

`rotate_credential` (`:812`) is the **one method already carrying `expected_version`** into the command envelope (`.with_expected_version(Some(expected_version))` at `:830`, plan 04's work). Move it verbatim including that plumbing — it is the template plan 06b will replicate 33 times, and breaking it here would cost that plan its only worked example.

Note the asymmetry to **preserve, not "fix"**: `create_credential`/`rotate_credential` run inside `AdminCommandRunner`, while `patch_credential` (`:781-811`) does not and calls `self.state.runtime_cache.invalidate_all()` directly. That inconsistency is out of scope here — moving it unchanged is correct; changing it is a behavior change this plan forbids. Record it as a deferred follow-up.

### Module 5 — `src/application/admin/keys.rs`, `jwt_issuers.rs`, `audit.rs`

Re-verify line numbers with `grep -n "pub async fn" src/application/admin.rs` at execution time before moving; module 0 will have already shifted nothing (it edits three lines in place) but the split itself is iterative.

- `keys.rs` (`ApiKeyAdminService`): `create_system_key` (`:1001`), `list_system_keys` (`:1069`), `get_system_key` (`:1085`), `create_consumer_key` (`:1090`), `list_consumer_keys` (`:1176`), `get_consumer_key` (`:1194`), and the table-generic trio `rotate_key` (`:1205`), `revoke_key` (`:1280`), `delete_key` (`:1306`) — one service, so the generic trio is not duplicated. This file owns module 0's three `expose_secret()` call sites once they move here.
- `jwt_issuers.rs` (`JwtIssuerAdminService`): `create_trusted_jwt_issuer` (`:1331`), `list_trusted_jwt_issuers` (`:1370`), `get_trusted_jwt_issuer` (`:1386`), `patch_trusted_jwt_issuer` (`:1395`), `set_trusted_jwt_issuer_enabled` (`:1422`), `refresh_trusted_jwt_issuer` (`:1450`), `delete_trusted_jwt_issuer` (`:1509`).
- `audit.rs` (`AuditAdminService`): `list_audit_logs` (`:1528`), `get_audit_log` (`:1545`).

### Module 6 — `src/application/admin/mod.rs` facade and `shared.rs`

- `src/application/admin/shared.rs` receives, verbatim: `PageRequest` (`:78-104`), `impl From<&PageQuery> for PageRequest` (`:105`), `actor_fingerprint` (`:1682`, still `pub(crate)` — `src/application/conversation.rs:1303` depends on it by path and that path must keep resolving), `admin_command_spec` (`:1699`), `success_audit` (`:1719`), and the **entire** `#[cfg(test)] mod tests` block (`:2120-2436`). Fifteen tests live there; thirteen concern pagination/cursor scoping (`has_more_is_false_when_exactly_limit_rows_are_available:2171` … `every_admin_list_uses_a_distinct_cursor_scope:2320`), and three concern shared validation and the fingerprint (`credential_scope_validation_matches_contract:2344`, `dangerous_custom_headers_are_rejected:2375`, `provider_url_policy_blocks_private_by_default:2386`, `actor_fingerprint_is_shared_by_admin_and_conversation_commands:2394`). **None of them is per-context. Do not distribute them and do not delete any as "redundant."**
- `pub struct AdminService<'a> { applications: …, providers: …, credentials: …, keys: …, jwt_issuers: …, audit: … }`.
- `impl<'a> AdminService<'a>` re-exposes **all 46** original public method names as one-line delegates, e.g. `pub async fn create_application(&self, actor: &Actor, ctx: &RequestContext, req: ApplicationCreateRequest) -> Result<ApplicationRecord, AppError> { self.applications.create_application(actor, ctx, req).await }`. This keeps `src/http/admin.rs`, `src/main.rs`'s `bootstrap_system_key`, `tests/support/mod.rs`, and `tests/admin_idempotency.rs` unmodified.
- Delete `src/application/admin.rs`, replace with the `src/application/admin/` directory; `src/application/mod.rs`'s `mod admin;` and `pub use admin::AdminService;` lines are unchanged.
- **Signature snapshot.** Before the split, capture `AdminService`'s public surface; after, capture it again and diff. Suggested: `cargo doc --no-deps` and diff the rendered method list, or a `rustdoc --output-format json` capture. A hand-written list is not evidence.

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
  Reconcile the field/variant set against what `rig_core::completion::Message` actually exposes **and** against what `ExecutionCommand.messages` consumers read today: `src/application/execution.rs:900` (`is_empty` check), `:1794` (`OneOrMany::many(command.messages.clone())` inside `build_completion_request`), and `:1952-1962` (`first_text`, which pattern-matches `Message::User { content }` / `Message::System { content }` / `Message::Assistant { content, .. }` and reaches into `rig_core::completion::message::UserContent::Text`). Read those call sites before finalizing so no information is lost in conversion. **Consult `.claude/skills/moira-rig-completions/SKILL.md` for the `Message`/`UserContent`/`OneOrMany` construction rules before writing the conversion.**
- `src/domain/runtime.rs:249` changes `pub messages: Vec<Message>` → `Vec<DomainMessage>`; drop `use rig_core::completion::Message;` at `:2`. `ExecutionCommand` (`:241-257`) derives `Debug, Clone, Serialize, Deserialize` but **not** `ToSchema` — do not add it, or the OpenAPI document changes.
- Add the conversion impls in **`src/orchestration/runtime_factory.rs`**, the documented Rig boundary. (The previous draft said `executor.rs`; that file is dead code being deleted in module 9.)
- Update every `ExecutionCommand { … }` construction site to build `DomainMessage`s and convert **at** the orchestration boundary, not before: `src/application/public.rs:958`, `src/application/execution.rs:81`, and `tests/support/mod.rs:314` (`fn command(&self, stream: bool) -> moira::domain::ExecutionCommand`). That last one is a **test-harness** construction site the previous draft never named; it will not compile after the type change.
- **Honest scope limit.** The DoD for P2-2 is `grep -rn "rig_core" src/domain/` returning nothing — achievable and worth doing. It does **not** make `src/application/` Rig-free: `execution.rs` will still import `rig_core` for `CompletionRequest`, `OneOrMany`, and the `first_text` match, and `public.rs` retains one import. Moving `build_completion_request` and `first_text` behind the `RuntimeFactory` seam is the right eventual shape but is a larger change than this plan's no-behavior-change budget allows. **Record it as a deferred follow-up; do not half-do it.**

### Module 8 — Repository traits (P2-3)

For each of `src/infra/repositories/{public.rs, runtime.rs, conversation.rs, setup.rs}`:
- Mirror `AdminRepository` exactly (`src/infra/repositories/admin.rs:92`): `#[async_trait]`, `pub trait X { async fn … -> Result<_, AppError>; }`, `Send + Sync` bounds, impl block annotated `#[async_trait]`.
- Extract a trait with the method set the concrete struct already exposes publicly — **78 methods total**: `PgSetupRepository` **1** (`inspect:27`), `PgPublicRepository` **16** (`:70`–`:689`), `PgConversationRepository` **30** (`:271`–`:1287`), `PgRuntimeRepository` **31** (`:84`–`:974`). Do setup first as the pattern reference; it is one method and proves the whole shape end-to-end in an hour. Then public, then conversation, then runtime.
- **Budget this honestly.** 78 trait methods plus 78 forwarding impl signatures plus four in-memory fakes is the single largest mechanical body of work in this plan — larger than the `AdminService` split. If it cannot land in this iteration, land setup + public (17 methods) and defer runtime + conversation (61) to a follow-up **explicitly**, rather than silently shipping two of four.
- Update consumers to hold `Arc<dyn Trait + Send + Sync>`: `SetupService` (`src/application/setup.rs`), `RuntimeAdminService` (`src/application/runtime_admin.rs`, which holds `PgRuntimeRepository` **and** `PgAdminRepository`), `PublicExecutionService` (`src/application/public.rs`), and the conversation consumer (`src/application/conversation.rs`).
- Add one in-memory fake per trait under `#[cfg(test)]` in the same repository file, sufficient for a first unit test of business logic without Postgres. **Fakes must not embed real credential material** — use synthetic values only.
- Do **not** replace the existing Postgres-backed integration tests; only add the option.
- Re-export the four new traits from `src/infra/repositories/mod.rs` beside `AdminRepository`.

### Module 9 — Extract the live code, then delete the dead resolver path (P2-4)

**Rewritten. The previous draft's step "delete `src/orchestration/resolver.rs`" does not compile:** `RuntimeConfigCache` is defined in that file (`:22-26`, `impl :57-87`) and is constructed at `src/app/state.rs:89`, stored at `:41`, and threaded through `src/infra/db.rs:13,45,61` into the runtime-config listener. It is load-bearing production code.

1. **Re-run the dead-code proof before deleting anything.**
   - `grep -rn "resolver::" src/ tests/` must return only `src/orchestration/mod.rs:11` and `src/orchestration/executor.rs:11`.
   - `grep -rn "mod chat\|http::chat\|chat_completions" src/` must return only `src/http/chat.rs` itself plus the unrelated setting `src/config/settings.rs:265 chat_completions_compat_enabled` (default `false` at `:854`), which is **not** a reference to the handler.
   - `grep -rn "execute_chat\|stream_chat" src/ tests/` must return only `src/orchestration/{mod,executor}.rs` and `src/http/chat.rs`. If it returns anything else, `executor.rs` is not dead and this module must be re-scoped.
   - If any check fails, stop and re-scope.
2. **Extract `RuntimeConfigCache` first.** Move `RuntimeConfigCache` (`:22-26`) and its `impl` (`:57-87`), together with the private `CacheEntry<T>` (`:28-32`) it depends on, into a new `src/orchestration/runtime_cache.rs`. Bodies verbatim. Update `src/orchestration/mod.rs` so `pub use runtime_cache::RuntimeConfigCache;` keeps `src/app/state.rs:12` and `src/infra/db.rs:13` importing from `crate::orchestration` unchanged — **no consumer file should need editing.**
3. **Extract `normalize_openai_base_url` second.** Move it (`:283-292`) and its tests (`resolver.rs:353,357`) into a new `src/orchestration/provider_url.rs`. It has a **live** consumer the previous draft never named: `src/orchestration/runtime_factory.rs:26,101,141`, on the OpenAI and Azure provider paths. Re-export from `mod.rs` so `runtime_factory.rs:26`'s `use crate::orchestration::normalize_openai_base_url;` keeps resolving unchanged.
4. **Delete, do not move, `resolver.rs`'s local `credential_aad` (`:272-281`).** It is a *divergent duplicate* using the legacy 3-part format `provider:{}:scope:{:?}:owner:{}`, referenced only by its own test. The canonical, live AAD is `credential_aad` + `CredentialAadParts` in `src/security/crypto.rs:85-111` (8 fields, `credential_id=…;provider_id=…;credential_type=…;scope_type=…;external_tenant_id=…;application_id=…;external_user_id=…;encryption_version=…`), which `resolver.rs` itself already imports as `envelope_credential_aad`. Deleting the local one removes a landmine that would silently fail to decrypt any live credential.
5. Delete `credential_priority` (`resolver.rs:255-268`) — the live precedence lives in `src/infra/repositories/runtime.rs::resolve_runtime_credential` (`:764`). Confirm no non-test caller first.
6. Delete the remaining resolver items: `ResolvedProvider` (`:35`), `CredentialCandidate` (`:42`), `resolve_provider` (`:89`), `get_provider` (`:124`), `find_default_provider` (`:145`), `resolve_api_key` (`:159`), `provider_runtime_select_sql` (`:294`), `scope_type_to_aad` (`:323`), `api_key_from_credential_payload` (`:332`), and the `#[cfg(test)]` module (`:346`) minus the two `normalize_openai_base_url` tests relocated in step 3. Then delete `src/orchestration/resolver.rs` and its `mod resolver;` / `pub use resolver::{…}` lines in `src/orchestration/mod.rs:3,11-14`.
7. **Delete `src/orchestration/executor.rs` (168 lines) too.** It is dead: `execute_chat`/`stream_chat` are exported at `mod.rs:10` and called from nowhere in `src/` or `tests/`. Its only would-be consumer is `src/http/chat.rs`, which is not compiled. The previous draft treated it as "the documented Rig-boundary owner" and relocated code *into* it — that is backwards; per `CLAUDE.md` and `.claude/skills/moira-rig-integration/SKILL.md` the Rig boundary is `src/orchestration/runtime_factory.rs`, which is the file that actually constructs Rig clients. Remove `mod executor;` and `pub use executor::{execute_chat, stream_chat};` from `mod.rs:2,10`.
   - Note for the reviewer: `orchestration` is `pub mod` in `src/lib.rs:11`, so `dead_code` never fired on any of this. Absence of a warning was not evidence of use.
8. Delete `src/http/chat.rs`. **No `src/http/mod.rs` edit is required** — `mod chat;` was never declared (`mod.rs:1-6` lists `admin, conversation, health, observability, openapi, public`), so the file is not compiled today. State this explicitly in the PR so a reviewer does not go looking for a router change. `docs/todo.md:58` ("Keep `/v1/chat/completions` unregistered") stays true and is unaffected.
9. `ChatCompletionRequest` (`src/domain/models.rs:117`) and its companion `ChatMessage` are referenced only by `chat.rs` and `executor.rs`. Once both are gone, delete them and their `src/domain/mod.rs:45` re-exports. **Grep first**; if anything else references them, leave them and say so.
10. `OwnerScope` lives in `src/domain` and is live in `src/security/crypto.rs` credential envelopes — **untouched**.

### Module 10 — i18n catalog: the residual gap and the missing mirror gate (P2-8, CONVENTIONS §4)

**Re-scoped. Plans 02b, 04, and 05 did roughly 90% of what the previous draft asked for.** Executing it as written would re-remove duplicates that no longer exist, re-add seven keys that already exist, and duplicate an existing test under a new name. Verified current state: `docs/i18n-response-catalog.json` has **73 entries / 73 unique keys**; `src/i18n/catalog/mod.rs` has **15** tests. What remains is one key and the mirror gate.

**10a — Add the one genuinely missing entry.** `routing_policy_provider_model_mismatch` is emitted at `src/application/runtime_admin.rs:308` and has **no** entry in `src/i18n/catalog/errors.rs` and **none** in `docs/i18n-response-catalog.json`. Add to both:

| Key | Produced at | `default_message` |
|---|---|---|
| `moira.error.routing_policy_provider_model_mismatch` | `src/application/runtime_admin.rs:308` | "The routing policy references a provider model that does not belong to the selected provider." |

Verify the suffix against `format!("moira.error.{}", self.code())` (`src/error.rs`) so key and code match **exactly**.

**10b — Do NOT re-add these seven.** `database_unavailable`, `database_error`, `configuration_error`, `upstream_error`, `http_client_error`, `redis_error`, `idempotency_in_progress` are **already present in both** the Rust catalog and the JSON mirror. Assert their presence; do not insert. A duplicate insert would be caught by 10c's `docs_mirror_has_no_duplicate_keys`, but catching it is worse than not causing it.

**10c — Close the real remaining hole: nothing reads the JSON mirror.** `grep -rn "i18n-response-catalog" src/ tests/` matches only doc comments (`src/i18n/catalog/mod.rs:9`, `src/i18n/catalog/README.md:16`, `docs/i18n-response-contract.md:54,63`). The mirror can drift arbitrarily and CI will not notice. The duplicates that existed before were fixed by hand, not by a gate. In `src/i18n/catalog/mod.rs`'s `#[cfg(test)] mod tests`:

- `docs_mirror_matches_rust_catalog` — read `docs/i18n-response-catalog.json` via `std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/docs/i18n-response-catalog.json"))`, parse the `entries` array, and assert set equality of `(key, default_message, description)` against `all_entries()` (`mod.rs:24`). Fail with a diff-friendly message naming keys present on only one side and, for shared keys, the exact differing field.
- `docs_mirror_has_no_duplicate_keys` — assert `entries.len() == unique_keys.len()`. This is the assertion that would have caught the two duplicates that plan 02b removed by hand.
- `every_coded_error_literal_in_src_has_a_catalog_entry` — walk `src/**/*.rs` from `CARGO_MANIFEST_DIR` at test time, regex-match `AppError::(coded|conflict|unprocessable)(…, "<code>")`, and assert every captured code has a `moira.error.<code>` entry. This is the test that would have caught `routing_policy_provider_model_mismatch`. Keep the walker simple and deterministic (sorted file order, skip `target/`).

**10d — Do NOT add `every_app_error_variant_code_has_a_catalog_entry`.** It already exists under the name **`every_error_message_key_resolves_to_a_catalog_entry`** (`src/i18n/catalog/mod.rs:336`, `async fn`, constructs real `AppError` variants including an `unreachable_reqwest_error()` helper at `:306`). Adding a second test with the same assertion under a different name is churn. If the name is felt to be unclear, rename it in a separate, obvious commit — do not duplicate it.

**10e — Notices.** All four `moira.notice.*` entries (`src/i18n/catalog/notices.rs:9,14,19,24`) still have **zero** production consumers; `grep -rn 'moira\.notice' src/` matches only the catalog, its README, and two tests (`mod.rs:59`, `src/domain/i18n.rs:47`). This plan adds **no** notice entries and emits none — it introduces no new user-visible success string. Record the dead-catalog fact in `src/i18n/catalog/mod.rs`'s module doc so the next plan that emits a notice (07) knows it is the first real consumer.

**10f — Injected-failure proof (required, per Definition of Done).** During development, temporarily (a) delete one JSON entry, (b) alter one `default_message` on one side only, and (c) add a `AppError::coded(…, "made_up_code_for_the_proof")` literal in a source file, and confirm `docs_mirror_matches_rust_catalog`, `docs_mirror_has_no_duplicate_keys` (via a deliberate duplicate), and `every_coded_error_literal_in_src_has_a_catalog_entry` each fail loudly and name the offending key. Capture the output. Revert before commit. **A test that has never been seen to fail is not evidence.**

### Module 11 — Per-endpoint unknown-query-field (P2-9)

- Unit (`src/http/admin.rs` `#[cfg(test)] mod tests`, additive — the file has **no** test module today, so this creates one): deserialize a query string containing a genuinely-unknown field (`?not_a_real_field=1`) into `PageQuery` and assert rejection. Name: `page_query_rejects_a_field_absent_from_the_struct`.
- E2E (`tests/admin_query_contract.rs`, new): drive each `GET /api/v1/admin/*` list route over HTTP with `?not_a_real_field=1` and assert `400` plus a well-formed error envelope carrying a non-empty `message_key` **and** `message` (CONVENTIONS §4.5). Name: `each_admin_list_endpoint_rejects_an_unknown_query_field`. A second test, `defined_but_unsupported_page_query_field_is_accepted_and_ignored`, pins the documented nuance so a future change to it is a deliberate, visible decision.
- Add a doc comment above `PageQuery` (`src/domain/admin.rs:31`) recording the P2-9 nuance: `deny_unknown_fields` rejects only fields **absent from the struct**; a field defined on `PageQuery` for a *different* endpoint (e.g. `credential_type` on the applications list) is silently accepted and ignored. All **26** fields are accepted on every list endpoint. Cross-reference finding ID P2-9. **Do not attempt to fix this in 06** — per-endpoint query types are a larger change and would alter `#[into_params]`, hence the OpenAPI document; document it and leave it as future work.

### Module 12 — Replace ungated `sleep()` interleaving (P2-12)

Scoped to what the re-verification actually found. **All five line numbers differ from the previous draft; re-grep at execution time rather than trusting these.**

- **`tests/admin_idempotency.rs:987` — the one real fix.** The surrounding code already does `task.abort(); let _ = task.await;` (`:985-986`) and `blocker.rollback().await` (`:986`), so cancellation *is* observed. The `sleep(Duration::from_millis(50))` at `:987` guards only the rollback becoming visible to the subsequent `count(...)` query. Replace it with a bounded poll of the row count inside `timeout(Duration::from_secs(5), …)` so the assertion is fail-loud rather than a timing guess. If a production-side signal is needed at the exact raced point (e.g. "after advisory lock acquired, before savepoint begins"), add a `#[cfg(test)]`-only probe or a trait test double — **never** an ungated production side effect that exists only for tests.
- **`tests/admin_idempotency.rs:1269`, `tests/execution_lifecycle.rs:1600`, `tests/execution_lifecycle.rs:1623` — reclassified, not rewritten blind.** Each is a 20 ms poll interval inside a `timeout(Duration::from_secs(5), …)`-wrapped loop (`wait_for_audit_lock`, `wait_for_public_cancellation`, `wait_for_attempt_status`), so each already fails loudly rather than flaking silently. Convert to `tokio::sync::Notify`/`oneshot` **only where a real signal exists**; otherwise leave the bounded poll and add a one-line comment citing P2-12 explaining why polling is correct here. Replacing a bounded poll with a hand-rolled signal that is subtly racier is a regression, not a fix.
- `tests/support/mock_openai.rs:351,394` already sit alongside `Semaphore`/`Notify` gating in the same file — bring them onto the existing gates.
- The existing `Barrier`-based concurrency pattern is the house style for firing simultaneous requests and must be reused for any new concurrency test: `tests/admin_idempotency.rs:528,1038,1138`, `tests/execution_policy_if_match.rs:374`, `tests/idempotency_hash_migration.rs:893`, `tests/rag_idempotency_replay.rs:1019`, `tests/jwks_hardening.rs:850`.
- **Anti-flake proof:** run the affected suites in a loop (≥20 iterations) and confirm zero failures. A sleep-based race replaced by a still-racy signal is not done.

### Module 13 — DB test isolation & teardown (P2-13)

**Re-scoped: there are three fixtures, not one.** The previous draft targeted only `tests/support/mod.rs`, which would have left `tests/admin_idempotency.rs` — the suite with the heaviest concurrency and the one this plan's split most needs as a regression baseline — still on a shared database behind its own separate mutex.

1. **`tests/support/mod.rs` (636 lines, 14 consuming suites).** Replace the `TEST_SERIAL` global-mutex serialization (`:44`, acquired `:130`, held in `_serial` on `LifecycleFixture` `:118`) with per-fixture Postgres schema isolation:
   - On `LifecycleFixture::new()` (`:128`), generate `moira_test_<uuidv7>`, `CREATE SCHEMA`, and set `search_path` for that fixture's pool. `PgPoolOptions::after_connect` is the reliable way to apply it to **every** pooled connection, not just the first.
   - Run `sqlx::migrate!("./migrations")` against that schema.
   - Guarantee teardown with a guard that issues `DROP SCHEMA moira_test_<uuidv7> CASCADE` — it **must run even when the test panics**, which is the whole point (there is no `impl Drop` in the file today). Because `Drop` is synchronous, drive the async drop through the fixture's existing shutdown mechanism (`tokio::task::block_in_place` + `Handle::current().block_on`, or an explicit `async fn teardown()` called from every test **plus** a `Drop` fallback that logs loudly if teardown was skipped). Pick one and make it unconditional; do not rely on tests remembering to call teardown.
   - Keep the existing CI fail-closed behavior at `:453-455` exactly as-is — preserve the `env::var("CI").is_ok_and(|value| value.eq_ignore_ascii_case("true"))` **value** check (per CONVENTIONS §3). Do **not** revert it to `env::var_os("CI").is_some()`, and do not let the schema-isolation refactor drop the `panic!` branch.
   - `MoiraHttpServer` (`:81-116`) spawns a real listener per fixture and is used by `tests/metrics_endpoint.rs` and `tests/openapi_drift.rs`; its `shutdown()` (`:109`) is the natural place to hang deterministic teardown off. Do not break it.
2. **`tests/admin_idempotency.rs` (own `SERIAL` at `:28`, own `Fixture` at `:30`, own `db::migrate` at `:69`).** Apply the same treatment. This is the suite the `AdminService` split depends on as its regression baseline, so it must keep passing at every step — do the schema isolation **after** the split has landed and been proven green under the current fixture, not concurrently with it.
3. **`tests/execution_policy_if_match.rs` (own `Fixture` at `:86`).** Same treatment, or an explicit justification in a comment for why it stays as-is.
4. **`LISTEN/NOTIFY` channels are database-wide, not schema-scoped** in Postgres. With 26 triggers across 20 tables emitting on `moira_runtime_config`, concurrent schema-isolated tests **will** see each other's notifications. Handle it explicitly: either skip `spawn_runtime_config_listener` (`src/infra/db.rs:43`, spawned from `src/main.rs:92`) in fixtures that do not exercise invalidation (most do not — no test fixture spawns it today), or embed the fixture's schema name in a test-run id the listener filters on. The new `tests/runtime_config_invalidation.rs` (module 14) is the one suite that must run its listener, so it must tolerate foreign notifications by asserting on its own resource ids only.
5. **`tests/security_foundation.rs`** already creates and force-drops an **entire database** per run — a second, coarser isolation strategy. Reconcile: keep it as-is (it is a migration-contract test, not a fixture user) and note the two strategies coexist deliberately. `tests/http_middleware_contract.rs` and `tests/jwks_hardening.rs` call `db::migrate` directly against the shared database; decide per file whether they need schema isolation and say which and why.
6. Remove `TEST_SERIAL`/`SERIAL` only once nothing depends on them. Grep first and re-check `src/app/state.rs` for process-global mutable state that schema isolation does **not** fix — `ConcurrencyController` (`state.rs:43`), `InMemoryRateLimiter` (`:44`), and `CircuitBreakerRegistry` (`:45`) are per-`AppState` fields constructed in `AppState::new` (`:94-102`), so per-fixture `AppState` construction already isolates them; confirm this before deleting the mutex, and keep serialization only where a genuine residual need exists.

### Module 14 — Scope `reset_all()` to the changed resource (P2-14)

- `src/infra/db.rs:59-80` (`listen_once`) currently ignores `notification.payload()` for control flow, logging it only (`:76`). Parse it as `{ "resource_type": String, "resource_id": String }` — the exact shape emitted by `migrations/0004_admin_api_contract.sql:116-119`.
- Keep `cache.invalidate_all()` (`:71`) and `runtime_handles.invalidate_all()` (`:72`) unconditional — the audit flagged only `circuits.reset_all()` (`:73`) as over-broad, and narrowing the caches is a separate, riskier change.
- Replace `circuits.reset_all()` with `circuits.reset_for_resource(resource_type, resource_id)`. Because `CircuitBreakerRegistry` keys on `(provider_id, model_id)` (`src/orchestration/controls.rs:494-495`), the mapping is:
  - `providers` → remove every entry whose **`provider_id`** equals `resource_id`.
  - `provider_models` → remove every entry whose **`model_id`** equals `resource_id`.
  - `provider_runtime_policies` (`migrations/0005`) → provider-scoped; resolve the owning provider id from the payload's `resource_id` **or**, if that requires a query on the hot path, fall back to the provider-scoped reset only when the id parses as a provider (document whichever is chosen).
  - Every other `resource_type` — the other 17 of the 20 triggered tables (applications, conversations, memory_records, rag_collections, rag_documents, the four `application_*_policies`, route_definitions, routing_policies, agent_profiles, system_api_keys, consumer_api_keys, trusted_jwt_issuers, application_execution_policies) → **no circuit reset at all**.
- **Fail-safe on malformed input:** if the payload does not parse, or `resource_id` is not a `Uuid`, or `resource_type` is unknown, fall back to today's `reset_all()` and log at `warn`. Narrowing must never turn a parse bug into a silently-stale breaker.
- Add `reset_for_resource` to `CircuitBreakerRegistry` alongside the existing `reset_all` (`controls.rs:606`) — keep `reset_all` for legitimate callers such as process startup.
- Note the payload carries **no `tg_op`**, so INSERT/UPDATE/DELETE cannot be distinguished; the scoped reset must be correct for all three. Do not add `tg_op` (that would require a migration, which this plan excludes).
- **Directional safety:** the worst case of an incomplete mapping is a circuit that *should* have reset and did not — strictly more conservative than today, self-healing on the next relevant NOTIFY or breaker timeout. Not a security or correctness regression.

### Module 15 — Docs drift (P3-9)

- `docs/project-structure.md:8-21`: add **two** missing entries to the source-tree listing in their correct alphabetical positions — `application/  per-context admin/runtime/execution business services` and `i18n/  response message-key catalog and default English strings` — and add one sentence each to "Boundaries": `application`'s role (thin orchestration between `http` and `infra`/`orchestration`; owns request-context, idempotency-envelope, and audit wiring) and `i18n`'s (the single registry of `moira.error.*`/`moira.notice.*` keys, mirrored to `docs/i18n-response-catalog.json`). Both are currently undocumented despite `application` being the largest layer.
- Add the two new orchestration modules from module 9 (`runtime_cache.rs`, `provider_url.rs`) to the `orchestration` line's description if the doc enumerates files; otherwise leave the layer description and just remove any mention of a resolver.
- `src/http/chat.rs` deletion (module 9) satisfies the second drift item by construction.
- `docs/todo.md:10`: split the bullet bundling "unregistered chat-route types" with `owner_scope`. Mark the chat-route deletion **done** (module 9), and rewrite the `owner_scope` line to state that `OwnerScope` (`src/domain`) and the credential AAD binding (`src/security/crypto.rs:85-111`) are **live and load-bearing** and must not be removed; what was dead was `src/orchestration/resolver.rs`'s resolution functions and its divergent local `credential_aad`, now deleted. Cite `src/security/crypto.rs`, not `resolver.rs` line numbers, since that file no longer exists once this lands. Leave `docs/todo.md:58` alone — it is correct.
- **Added after module 9 landed (`088f0a4`) — this module's original scope missed two dangling documents.** Both now point at files that no longer exist, and one states a rule that contradicts `CLAUDE.md`:
  - `docs/moira-security-auth-credential-design.md:223,225` list `resolver.rs` and `executor.rs` in a source-tree listing, and `:230` asserts *"Rig usage stays in `src/orchestration/executor.rs` and adjacent execution modules."* That file is deleted, and the rule is wrong regardless: `CLAUDE.md` and the rig skills put the Rig boundary at `src/orchestration/runtime_factory.rs`. Correct the listing and rewrite the placement rule to name `runtime_factory.rs`. Left uncorrected, this document actively instructs the next contributor to reintroduce the anti-pattern module 7 just removed.
  - `plans/11-rag-memory-intelligence.md:208` instructs reusing "the exact credential-resolution path at `resolver.rs:254-268`". That path is deleted. Redirect it to `src/infra/repositories/runtime.rs::resolve_runtime_credential`, where the live 8-tier precedence actually lives. `plans/README.md:89-90` already flags the conflict; this closes it. **Do not otherwise edit plan 11** — it is re-audited on its own turn.

### Module 16 — Unify `actor_fingerprint` (P2-15) — **new; the plan never accepted this handoff**

`src/application/admin.rs:1666-1682` assigns this work to `plans/06-architecture-test-hygiene.md` by name. The previous draft of this plan never mentioned it. Writing it in is the point of this rewrite.

**16.1 — The single formula.** Keep `src/application/admin.rs:1682`'s implementation verbatim as the canonical one; it is the strictest (10 identity fields, `serde_json::to_vec` of a tuple, then `secret_fingerprint`) and it is already shared with `src/application/conversation.rs:1303`. It moves to `src/application/admin/shared.rs` in module 6 and stays `pub(crate)`.

**16.2 — Delete the two divergent copies.**
- `src/application/runtime_admin.rs:744` (3 fields) — delete; call the shared one. Call sites: `:664`, `:725`.
- `src/application/public.rs:1893` `public_actor_fingerprint` (4 fields) — delete; call the shared one. Call site: `:1040`.
- After this, `grep -rn "fn actor_fingerprint\|fn public_actor_fingerprint" src/` must return exactly one line.

**16.3 — In-flight rows: read-path fallback, no migration.** Unifying changes the fingerprint value for existing keys, so a row written by the old formula would no longer be found by the new one — the caller would get a fresh execution instead of a replay. That is not silent corruption (the unique index still holds; the worst case is a duplicated non-idempotent operation on a key issued within the retention window). It is still unacceptable to ship blind.

The precedent is `src/security/idempotency.rs`'s keyed-hash switch, proven by `tests/idempotency_hash_migration.rs`: **the read path accepts both the new and the legacy value; the write path only ever emits the new one.** Apply the same shape:
- Compute both the unified fingerprint and the route-family legacy fingerprint on the **read** path (`get_idempotency_record` / `claim_idempotency` lookup).
- Try the unified value first; on miss, try the legacy value. A hit on the legacy value is a legitimate replay.
- **Always write the unified value.** Never write a legacy fingerprint.
- Gate the legacy read behind a `// TODO(plan-07): remove after <retention window> has elapsed` comment naming the concrete date/window, so it is removable rather than permanent.
- `src/infra/repositories/admin.rs:1934` `advisory_lock_key` takes the fingerprint as an input, so lock partitioning changes with it. That is fine — the lock is a same-key serialization device, not a durable identity — but the legacy-fallback read must not acquire two different locks for what is one logical key. Take the lock on the **unified** fingerprint only, and do the legacy lookup inside it.

**16.4 — Tests.**
- Unit, in `src/application/admin/shared.rs`: `actor_fingerprint_distinguishes_actors_differing_only_by_trusted_jwt_issuer`, `actor_fingerprint_distinguishes_actors_differing_only_by_tenant`, `actor_fingerprint_distinguishes_actors_differing_only_by_delegated_subject`. Each must have been **observed failing** against the old 3-field `runtime_admin` formula before that formula was deleted — that is the proof the bug was real. Capture the transcript.
- Keep and extend `actor_fingerprint_is_shared_by_admin_and_conversation_commands` (`admin.rs:2394`) into `actor_fingerprint_is_shared_by_every_idempotent_command_path`, asserting the admin, conversation, runtime-admin, and public paths all produce the identical value for an identical actor.
- E2E, `tests/actor_fingerprint_unification.rs` (new): `runtime_admin_replay_is_isolated_across_trusted_jwt_issuers` and `runtime_admin_replay_is_isolated_across_tenants` — two actors differing only in issuer (then only in tenant), same `Idempotency-Key`, same operation; assert each gets its **own** resource and its **own** `idempotency_records` row. **Write these first and watch them fail on `main`.** A test that only ever passed proves nothing about a bug that was allegedly there.
- E2E: `a_legacy_fingerprint_row_still_replays_after_unification` — insert a row with the pre-unification fingerprint directly, then replay through HTTP and assert the stored response comes back. Model it on `tests/idempotency_hash_migration.rs`.

**16.5 — Scope guard.** This module changes fingerprint computation only. It does **not** change `runtime_admin.rs`'s two-phase, non-transactional idempotency scheme (`idempotency_replay`/`record_idempotency`) — that remains a deferred follow-up. Unifying the fingerprint makes that scheme *correct about identity* without making it *transactional*; say so plainly in the PR rather than implying the runtime-admin idempotency story is now finished.

### Module 17 — If-Match TOCTOU: inventory, recipe, and failing harness (P2-16) — **inventory only**

**17.1 — What this module delivers.** Not the fix. This module delivers three artifacts so that the fix is a mechanical, reviewable job in its own PR:
1. The verified inventory of all 33 sites (reproduced in the P2-16 finding row above, with line numbers and owning service).
2. The written recipe (§17.3).
3. A **failing-first** e2e harness, `tests/if_match_atomicity.rs`, containing one `#[ignore]`d test per resource family that races a version-bump against a precondition-checked write and asserts the write is rejected. Marked `#[ignore]` with a comment naming plan 06b, so the suite is green but the evidence is committed and runnable with `--ignored`.

**17.2 — Why not fix it here.** Four reasons, in order of weight:

1. **It directly contradicts this plan's central safety control.** Module 6's Definition of Done requires `AdminService`'s public surface — "all 46 method names, **signatures**, return types" — to be unchanged, verified by a before/after signature snapshot. Fixing If-Match requires adding an `expected_version: i64` parameter to roughly 21 of those 46 signatures. You cannot simultaneously prove "signatures unchanged" and change 21 signatures. Folding them together destroys the one mechanical control that makes a 2,436-line refactor safe to review.
2. **It is a behavior change; this plan's premise is that there is none.** Today, under concurrency, a stale `If-Match` can pass and a lost update can land. After the fix, that request gets a `409`. That is observable, it is the point, and it deserves a PR whose reviewers are looking for it.
3. **It touches code this plan requires to be byte-identical.** Each `patch_*`/`delete_*`/`set_*_enabled` repository method needs an `expected_version` predicate in its SQL, i.e. edits throughout `src/infra/repositories/admin.rs` and `runtime.rs`. Module 6's DoD requires `git diff main -- src/infra/repositories/admin.rs` to show no change beyond mechanical import moves.
4. **It crosses two services, two repositories, and two idempotency schemes.** 21 sites go through `AdminService` → `PgAdminRepository` → the transactional `AdminCommandRunner` envelope. 12 go through `RuntimeAdminService` → `PgRuntimeRepository` → the two-phase, non-transactional scheme. The second group cannot be done correctly until Module 16 lands, because a version predicate inside a replay-checked write needs the replay key to isolate actors properly first. That is a dependency, not a coincidence.

**17.3 — The recipe (for plan 06b).** Plan 04 already did exactly one of these and it is the template:

- HTTP handler: replace `ensure_version(service.get_X(&actor, id).await?.version, require_if_match(&headers)?)?;` with `let expected_version = require_if_match(&headers)?;` and pass it to the service call. See `rotate_credential` (`src/http/admin.rs:902-914`), and `put_application_execution_policy` (`:335-353`) for the `Option<i64>` variant.
- Service method: add `expected_version: i64`, thread it into the command spec via `.with_expected_version(Some(expected_version))` (`src/application/admin_command.rs:96`). See `AdminService::rotate_credential` (`src/application/admin.rs:812`, spec at `:830`).
- Repository method: add the version predicate to the `UPDATE`/`DELETE`'s `WHERE` clause so the check and the write are one statement in one transaction; a zero-row result becomes `AppError::conflict("resource_version_conflict", …)` — the **same** code the handler emits today, so the wire contract is unchanged.
- Keep `ensure_version` (`src/http/admin.rs:92`) until the last site is converted, then delete it and prove `grep -rn "ensure_version" src/` is empty.
- Per site, add a concurrency e2e test using the existing `Barrier` house style; `tests/execution_policy_if_match.rs` is the worked example for the whole shape.

**17.4 — Recommendation: split into plan 06b, sequenced 06 → 06b → 07.** 33 handlers, ~33 service signatures across two services, ~33 repository predicates, and 33 concurrency tests is a plan, not a module. Sequencing it after 06 gets it three things it needs: the `AdminService` split (so the 21 admin signatures change in six small focused files instead of one 2,436-line one), Module 16's fingerprint unification (a hard prerequisite for the 12 runtime-admin sites), and Module 13's schema isolation (without which 33 new concurrency tests on a shared serialized database will be slow and flaky). Sequencing it *before* 07 matters because 07 adds an `AdminIdentityService` slice that will otherwise be written against the TOCTOU pattern and inherit it.

**If the decision is instead to fold it into 06**, then Module 6's "signatures unchanged" DoD item must be struck and replaced with an explicit signature-change inventory, and this plan's "no externally observable behavior change" premise must be amended in the Summary. Do not leave both claims standing.

---

## i18n Compliance (CONVENTIONS §4)

- This plan adds **no new error code** and **no new user-visible success string** of its own, so it adds no *new* `moira.error.*` or `moira.notice.*` entry for behavior it introduces.
- It nonetheless carries a mandatory i18n deliverable: module 10 closes the one remaining verified catalog gap (`routing_policy_provider_model_mismatch`) and adds the three tests (`docs_mirror_matches_rust_catalog`, `docs_mirror_has_no_duplicate_keys`, `every_coded_error_literal_in_src_has_a_catalog_entry`) that make CONVENTIONS §4.1/§4.4/§4.5 enforceable for **every later plan**, including 07's new codes. The mirror is currently read by nothing; that is the real hole.
- Module 16's `409 resource_version_conflict` and Module 14's behavior are unchanged codes; no new key.
- `message_args` is untouched by this plan; no handler gains an inline English literal.
- The JSON mirror is updated in the **same PR** as the Rust catalog.

---

## Multi-Agent Workflow

Eighteen modules across `src/security/`, `src/application/`, `src/domain/`, `src/infra/`, `src/orchestration/`, `src/http/`, `src/i18n/`, `tests/`, `docs/`. File ownership is disjoint per agent by construction, with the exceptions called out below.

### Plan 05 collision surface — read before assigning agents

Plan 05 (`3ea8037`) is the most recent thing to land on the files this plan refactors. Specifically:

- **`tests/support/mod.rs` is the highest-risk file in the plan.** Plan 05 modified it (`git log --oneline -- tests/support/mod.rs` → `3ea8037`, `19b98ae`, `ac46108`, `ce99a7a`) and added `MoiraHttpServer` usage on top of it. It now serves **14** suites, two of which (`tests/metrics_endpoint.rs:51`, `tests/openapi_drift.rs:71`) are plan 05's and import `LifecycleFixture`, `MoiraHttpServer`, and `RuntimePolicy` directly. Modules 12 and 13 both edit this file. **One agent owns it, for both modules.**
- **`tests/retention_worker.rs` does *not* use `tests/support`** — it is standalone and was repaired by plan 05 for the `MetricsRegistry::new(service_name, pool)` signature and the deleted `snapshot()`. It is therefore *not* a `tests/support` collision, but it is a separate file no agent in this plan should touch. Keep it out of scope entirely.
- **`tests/supply_chain_policy.rs`** is likewise standalone and out of scope.
- **`docs/openapi.json` is frozen.** Plan 05 committed it and gated it. No module here may regenerate it (see §"OpenAPI regeneration"). Any agent that finds the drift test failing must report it, not run `UPDATE_SNAPSHOTS=1`.
- **`src/http/mod.rs`** grew from ~8 spec tests to 30+, including the drift machinery (`:1464`–`:1751`). No module here edits it, but module 9's deletions and module 7's type change must leave every one of those tests passing unmodified.
- **`deny.toml` / `Cargo.toml`** — module 0 adds no dependency (`secrecy` is already present), so `cargo deny check` should be unaffected. Note `publish = false` in `Cargo.toml` pairs with `deny.toml`'s `[licenses.private] ignore = true`; do not remove either.

### Waves

**Wave 0 (coordinator, sequential, before any agent starts).** This document. Plus: one agent reads `src/application/admin.rs` in full and produces the authoritative 46-method → target-module mapping with current line numbers, the confirmed `AdminCommandRunner` call pattern (module 1), the `AdminRepository` trait shape (module 8 template), and the pre-split `AdminService` signature snapshot (module 6). Output is a shared reference document, not a code change.

**Commit #1 (coordinator or a single agent, sequential, alone).** Module 0 — `SecretString`. Three files, ~6 lines, plus two tests and the compile-failure transcript. Gate it on its own. **Nothing else starts until this is committed**, because its three call sites live in `src/application/admin.rs` and the split will move them.

**Wave 1 (parallel, disjoint files):**
- **Agent A — modules 1-6**, the entire `AdminService` split, sequentially and alone. Every sub-module shares one facade file (`src/application/admin/mod.rs`); splitting this across agents guarantees merge conflicts.
- **Agent B — module 7** (`src/domain/message.rs`, `src/domain/runtime.rs`, `src/orchestration/runtime_factory.rs` conversions, plus the three `ExecutionCommand` construction sites in `src/application/public.rs:958`, `src/application/execution.rs:81`, and `tests/support/mod.rs:314`). **Overlaps Agent F on `tests/support/mod.rs`** — B's edit there is a three-line type change; pre-agree that B lands it first and F rebases onto it.
- **Agent C — module 8** (four repository files + their four `src/application/*.rs` consumers — `setup.rs`, `runtime_admin.rs`, `public.rs`, `conversation.rs`; explicitly **not** `admin.rs`, avoiding Agent A). **Overlaps Agent J on `runtime_admin.rs` and `public.rs`** — see sequencing.
- **Agent D — module 9** (extract `RuntimeConfigCache` → `src/orchestration/runtime_cache.rs` and `normalize_openai_base_url` → `src/orchestration/provider_url.rs`; delete `resolver.rs`, `executor.rs`, `src/http/chat.rs`, `ChatCompletionRequest`/`ChatMessage`; clean `src/orchestration/mod.rs`). **Overlaps Agent B on `src/orchestration/mod.rs`** (B adds nothing there if the conversions live inside `runtime_factory.rs`; confirm at Wave 0) — run D after B lands, or pre-agree non-overlapping insertion points.
- **Agent E — modules 10 + 11** (`src/i18n/catalog/{mod,errors}.rs`, `docs/i18n-response-catalog.json`, `src/domain/admin.rs` doc comment, `src/http/admin.rs` additive test module, new `tests/admin_query_contract.rs`). The `src/http/admin.rs` addition is additive-only and does not collide with anything else in this plan (no module edits that file's handlers).
- **Agent F — modules 12 + 13** (`tests/support/mod.rs`, `tests/support/mock_openai.rs`, `tests/admin_idempotency.rs`, `tests/execution_lifecycle.rs`, `tests/execution_policy_if_match.rs`). Fully disjoint from all production-code agents **except** Agent B's three-line `tests/support/mod.rs:314` change. Module 13's `tests/admin_idempotency.rs` work runs **after** Agent A is green.
- **Agent G — module 14** (`src/infra/db.rs`, `src/orchestration/controls.rs`, new `tests/runtime_config_invalidation.rs`).
- **Agent H — module 15** (docs only). Fully disjoint; can run first. Must run **after** D to describe the final orchestration layout accurately, or must be re-checked after D.
- **Agent I — the e2e regression suite** (`tests/admin_surface_contract.rs`, new file). Must land **after** Agent A, because its whole purpose is to prove the split changed nothing. Golden values are captured on `main` **before** the split and asserted after.
- **Agent J — module 16** (`src/application/runtime_admin.rs`, `src/application/public.rs`, `src/application/conversation.rs`, `src/infra/repositories/admin.rs` read path, new `tests/actor_fingerprint_unification.rs`). **Overlaps Agent A** on the canonical formula's new home (`src/application/admin/shared.rs`) and **Agent C** on `runtime_admin.rs`/`public.rs`. Run J after both. Its failing-first e2e tests can and should be written and run against `main` in parallel with everything else.
- **Agent K — module 17** (inventory + `tests/if_match_atomicity.rs`, all `#[ignore]`d). Read-only against `src/http/admin.rs`; writes one new test file. Fully disjoint, can run first.

**Sequencing constraints.**
- **Commit #1 (module 0) before everything.**
- B before D (`src/orchestration/mod.rs`), and B before F (`tests/support/mod.rs:314`).
- A before J (`shared.rs`), C before J (`runtime_admin.rs`, `public.rs`).
- A before I, with goldens captured pre-split.
- A green before F's `tests/admin_idempotency.rs` schema isolation.
- D before H (docs must describe the final layout).
- **Cross-plan:** plan 07 also edits `src/infra/db.rs::listen_once` (it adds auth-settings cache invalidation). If 06 and 07 are in flight together, module 14 and 07's listener change must be merged deliberately, not blind-rebased. Flag to the coordinator at Wave 0.
- All other agents (E, G, K) run fully parallel with A.

**Checkpoints (read-only reviewer, after each wave and after each sequential merge).** Run the full gate list; report pass/fail; do not edit code. This is the mechanism that catches an `authz.require` call dropped during the split.

---

## Interfaces & Contracts

No new endpoints, no changed request/response shapes, no changed status codes, headers, scopes, or **wire-visible** error codes.

**i18n:** the catalog *content* changes by exactly one added key; no `message_key` that a client sees today changes value. The added key is one that was already being emitted with no catalog backing — adding it is strictly additive from a client's perspective and is what CONVENTIONS §4 requires.

**Idempotency:** the *envelope* is unchanged — `AdminCommandRunner` and the `claim_idempotency`/savepoint/`finalize_idempotency` sequence are not modified; sub-services call the same shared code. The *actor identity* used as part of the ledger key **is** changed by Module 16, narrowing the equivalence class on the runtime-admin and public paths so that issuer/tenant/application/delegated-subject differences are now distinguished. In-flight rows keep replaying via the documented read-path fallback (§16.3).

**Transaction boundaries:** unchanged (advisory lock → savepoint → business logic → release/rollback → finalize, within one `AdminRepository`-held connection, per method). Module 16 changes the *value* fed to `advisory_lock_key`, not the locking protocol.

**Cache invalidation:** `cache.invalidate_all()` / `runtime_handles.invalidate_all()` unchanged. `circuits.reset_all()` becomes `circuits.reset_for_resource(...)` with a `reset_all()` fallback on malformed payloads — the one intentional behavioral narrowing on the listener path, scoped down, never up.

**Concurrency:** `pg_try_advisory_xact_lock` single-winner semantics unchanged; no new lock keys, though Module 16 repartitions the existing keyspace.

**Secrets:** `GeneratedApiKey.raw_key` becomes non-serializable at the type level. The once-only wire envelope `ApiKeySecretResponse.secret` is unchanged — plaintext still crosses the wire exactly once, at exactly one place, now via an explicit `expose_secret()`.

**SSE:** not touched.

---

## Verification (CONVENTIONS §3 — unit **and** e2e are both mandatory)

### Unit tests (new, named)

| File | Test | Proves |
|------|------|--------|
| `src/security/api_keys.rs` | `generated_api_key_debug_redacts_the_plaintext` | P2-0 — `Debug` cannot leak |
| `src/security/api_keys.rs` | `generated_api_key_plaintext_is_only_reachable_through_expose_secret` | P2-0 — the escape hatch is the only path |
| `src/i18n/catalog/mod.rs` | `docs_mirror_matches_rust_catalog` | JSON mirror ≡ `all_entries()` (P2-8) — nothing reads the mirror today |
| `src/i18n/catalog/mod.rs` | `docs_mirror_has_no_duplicate_keys` | would have caught the two duplicates 02b fixed by hand |
| `src/i18n/catalog/mod.rs` | `every_coded_error_literal_in_src_has_a_catalog_entry` | catches `routing_policy_provider_model_mismatch` |
| *(not added)* | ~~`every_app_error_variant_code_has_a_catalog_entry`~~ | **already exists** as `every_error_message_key_resolves_to_a_catalog_entry` (`src/i18n/catalog/mod.rs:336`) — do not duplicate |
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
| `src/orchestration/provider_url.rs` | the two relocated `normalize_openai_base_url` tests | module 9 lost nothing |
| `src/application/admin/shared.rs` | `actor_fingerprint_distinguishes_actors_differing_only_by_trusted_jwt_issuer`, `…_by_tenant`, `…_by_delegated_subject` | P2-15 — each **observed failing** against the old 3-field formula |
| `src/application/admin/shared.rs` | `actor_fingerprint_is_shared_by_every_idempotent_command_path` | P2-15 — one formula, four call paths |
| moved-with-code | the 15 tests in `src/application/admin.rs:2120-2436` move **whole** into `src/application/admin/shared.rs` — none deleted "because it's redundant", none scattered into per-context files | split loses no coverage |

### E2E tests (new, named — real HTTP surface, real PostgreSQL 16 + pgvector)

Following the existing harness (`tests/support/mod.rs`) and the in-process-router pattern from `tests/admin_idempotency.rs` (`moira::build_router(state.clone())` at `:78`, the `post(router, path, key, if_match, body)` helper, `Fixture::post` at `:92`).

| File | Test | Proves |
|------|------|--------|
| `tests/admin_surface_contract.rs` (new) | `applications_crud_contract_is_unchanged_after_service_split` | P2-1 changed no HTTP behavior |
| | `providers_and_provider_models_contract_is_unchanged_after_service_split` | ditto |
| | `credentials_crud_and_rotation_contract_is_unchanged_after_service_split` | ditto, incl. `If-Match` 409 |
| | `system_and_consumer_key_contract_is_unchanged_after_service_split` | ditto, incl. once-only secret envelope — and that module 0 did not change the wire |
| | `trusted_jwt_issuer_contract_is_unchanged_after_service_split` | ditto |
| | `audit_log_contract_is_unchanged_after_service_split` | ditto |
| | `every_admin_mutation_still_writes_exactly_one_audit_row` | no audit call dropped in a move |
| | `every_admin_mutation_still_honours_its_required_scope` | no `authz.require` dropped in a move |
| `tests/admin_query_contract.rs` (new) | `each_admin_list_endpoint_rejects_an_unknown_query_field` | P2-9 at HTTP level, with a non-empty `message_key` + `message` |
| | `defined_but_unsupported_page_query_field_is_accepted_and_ignored` | pins the documented nuance |
| `tests/actor_fingerprint_unification.rs` (new) | `runtime_admin_replay_is_isolated_across_trusted_jwt_issuers` | P2-15 — **must fail on `main`** before the fix |
| | `runtime_admin_replay_is_isolated_across_tenants` | P2-15 — ditto |
| | `a_legacy_fingerprint_row_still_replays_after_unification` | §16.3 fallback |
| `tests/runtime_config_invalidation.rs` (new) | `provider_model_notify_resets_only_that_models_circuit` | P2-14 |
| | `unrelated_table_notify_leaves_all_circuits_intact` | P2-14 — the actual finding |
| | `runtime_cache_still_invalidates_on_every_notify` | narrowing did not over-narrow |
| | `malformed_notify_payload_falls_back_to_full_reset` | fail-safe |
| `tests/test_isolation.rs` (new) | `each_fixture_runs_in_its_own_schema` | P2-13 |
| | `schema_is_dropped_even_when_the_test_panics` | P2-13 — the property that matters |
| | `two_concurrent_fixtures_do_not_observe_each_others_rows` | P2-13, and that `TEST_SERIAL` removal is safe |
| `tests/if_match_atomicity.rs` (new, **all `#[ignore]`d**) | one race test per resource family, each citing plan 06b | P2-16 — the evidence is committed and runnable with `--ignored`, but the suite stays green |

**Regression baseline (must pass unmodified in outcome):** `tests/admin_idempotency.rs` (9 tests), `tests/execution_lifecycle.rs`, `tests/execution_policy_if_match.rs`, `tests/public_authorization.rs`, `tests/http_error_contract.rs`, `tests/http_middleware_contract.rs`, `tests/idempotency_hash_migration.rs`, `tests/jwks_hardening.rs`, `tests/list_pagination.rs`, `tests/rag_idempotency_replay.rs`, `tests/rag_ingestion_honesty.rs`, `tests/security_foundation.rs`, and plan 05's `tests/metrics_endpoint.rs`, `tests/openapi_drift.rs`, `tests/secret_leak_snapshots.rs`, `tests/content_leak_snapshots.rs`, `tests/supply_chain_policy.rs`, `tests/retention_worker.rs`. Their *content* may change per modules 7/12/13; their assertions and pass criteria may not.

### Other verification

- **Compile-failure transcript (module 0):** the recorded `rustc` error proving `serde_json::json!({ "raw": generated.raw_key })` does not compile. Without it, P2-0 is unverified.
- **Failing-first transcripts (module 16):** the two runtime-admin isolation tests failing on `main`, and the three fingerprint unit tests failing against the old 3-field formula.
- **Injected-failure transcripts (module 10):** each of the three new i18n tests observed failing against a deliberate mismatch.
- **Signature snapshot (module 6):** before/after capture of `AdminService`'s 46 public methods, diffing empty.
- **Concurrency/anti-flake:** run the concurrency-bearing suites ≥20 times in a loop and confirm zero failures (module 12). A sleep replaced by a still-racy signal is not done.
- **Migration:** none added. `tests/security_foundation.rs`'s migration-contract test must still pass, confirming the new schema-per-fixture isolation does not collide with its create-and-drop-database strategy.
- **OpenAPI:** every spec test in `src/http/mod.rs` (`mod tests` at `:580`) passes **unmodified**, `tests/openapi_drift.rs` passes, and `git diff main -- docs/openapi.json` is **empty**. This is the structural proof no route or DTO changed. Do not run `UPDATE_SNAPSHOTS=1`.
- **Dead-code proof (module 9):** the three greps in §9.1, captured before deletion.
- **Secret-leak (CONVENTIONS §8):** `tests/secret_leak_snapshots.rs` and `tests/content_leak_snapshots.rs` pass; `src/security/masking::tests` and `src/infra/repositories/setup.rs`'s `SETUP_READINESS_SQL` guard (asserting the query never mentions `encrypted_payload`, `encrypted_data_key`, `key_hash`, `key_prefix`, `masked_secret`, `secret_fingerprint`) still pass. Repository-trait fakes (module 8) must use synthetic values only.
- **Required gates (CONVENTIONS §2, verbatim, after every wave and at the end):**
  ```bash
  export MOIRA_TEST_DATABASE_URL='postgres://postgres:postgres@127.0.0.1:5432/moira'
  cargo fmt --check
  cargo clippy --workspace --all-targets --all-features -- -D warnings
  cargo test --workspace --all-features
  cargo build --release --locked
  cargo deny check
  ```
  plus clean PostgreSQL migration validation (migrations apply from an empty database and the DB-backed suite passes against it). **DB-backed suites reporting *skips* are not passing** — `tests/support/mod.rs:453-455` only fails closed when `CI=true`, so confirm the suites actually ran.

---

## Definition of Done

**Plan-specific**

- [ ] `GeneratedApiKey.raw_key` is `secrecy::SecretString`; `grep -rn "raw_key" src/` shows every read going through `expose_secret()`; the recorded compiler error proving `json!({"raw": …})` does not compile is in the PR.
- [ ] `src/application/admin.rs` no longer exists as a single 2,436-line file; `src/application/admin/` contains the per-context modules plus `shared.rs`; `AdminService`'s public surface (all **46** method names, signatures, return types) is unchanged — verified by a before/after method-signature snapshot, not by inspection.
- [ ] All 15 tests from `src/application/admin.rs:2120-2436` exist in `src/application/admin/shared.rs` and pass; none deleted, none scattered.
- [ ] `AdminCommandRunner` / `admin_command_spec` / the `claim_idempotency`→`finalize_idempotency` sequence are unchanged apart from Module 16's read-path fallback and mechanical import moves (`git diff main -- src/application/admin_command.rs src/infra/repositories/admin.rs` reviewed line-for-line and the exceptions named in the PR).
- [ ] `grep -rn "rig_core" src/domain/` returns nothing. The residual `rig_core` imports under `src/application/` are named in the PR as an explicit deferred follow-up, not silently left.
- [ ] `PublicRepository`, `RuntimeRepository`, `ConversationRepository`, `SetupRepository` traits exist, are `#[async_trait]`, are implemented by their `Pg*` structs, and are re-exported from `src/infra/repositories/mod.rs`; **at least one unit test per trait** exercises a fake without a live Postgres connection, and at least one *application-service* unit test (`src/application/setup.rs`) runs Postgres-free. If any of the four was deferred, the PR says which and why.
- [ ] `src/orchestration/resolver.rs`, `src/orchestration/executor.rs`, and `src/http/chat.rs` do not exist; `RuntimeConfigCache` lives in `src/orchestration/runtime_cache.rs` and `src/app/state.rs`/`src/infra/db.rs` are **unmodified**; `normalize_openai_base_url` lives in `src/orchestration/provider_url.rs` and `src/orchestration/runtime_factory.rs:26,101,141` still resolves it; the divergent local `credential_aad` and `credential_priority` are **deleted**, and `src/security/crypto.rs:85-111` remains the single AAD implementation.
- [ ] `docs/i18n-response-catalog.json` matches the Rust catalog exactly and has no duplicate keys — **asserted by a test**, not by hand; `routing_policy_provider_model_mismatch` exists in both; the three new i18n tests pass, and each was **observed failing** against an injected mismatch before the mismatch was reverted. No duplicate of `every_error_message_key_resolves_to_a_catalog_entry` was added.
- [ ] Every admin list/filter endpoint has an e2e test asserting unknown-query-field rejection with a real unknown field name, and the response carries a non-empty `message_key` **and** `message`; the `PageQuery` doc comment records the P2-9 nuance and its 26-field scope.
- [ ] `tests/admin_idempotency.rs` contains no unbounded `sleep()`; the three bounded poll sites are either converted to signals or annotated with the P2-12 rationale; the affected suites survive ≥20 consecutive runs.
- [ ] All three fixtures (`tests/support/mod.rs`, `tests/admin_idempotency.rs`, `tests/execution_policy_if_match.rs`) isolate each run in its own Postgres schema with teardown guaranteed on panic — verified by `schema_is_dropped_even_when_the_test_panics`, not by inspection. Each `TEST_SERIAL`/`SERIAL` is removed, or its remaining use is justified in a comment naming the specific process-global state it protects. The `CI=true` value-check fail-closed branch survives in all three.
- [ ] `circuits.reset_all()` is no longer called from `listen_once`'s normal path; `reset_for_resource` is, with `resource_type`/`resource_id` parsed from the payload and a documented `reset_all()` fallback on malformed input; `unrelated_table_notify_leaves_all_circuits_intact` passes.
- [ ] Exactly one `actor_fingerprint` formula exists in `src/` (`grep -rn "fn actor_fingerprint\|fn public_actor_fingerprint" src/` returns one line); the read-path legacy fallback is in place with a dated removal TODO; the two runtime-admin isolation e2e tests were **observed failing on `main`** and pass after; `a_legacy_fingerprint_row_still_replays_after_unification` passes.
- [ ] `tests/if_match_atomicity.rs` exists with one `#[ignore]`d race test per resource family, the 33-site inventory is in the PR body, and a recommendation on plan 06b is recorded and answered by the user.
- [ ] `docs/project-structure.md` lists **both** `src/application/` and `src/i18n/`; `docs/todo.md:10`'s chat/`owner_scope` bullet is split, with the `owner_scope` line rewritten to state it is live and load-bearing; `docs/todo.md:58` is untouched.
- [ ] `git diff main -- docs/openapi.json` is empty and `tests/openapi_drift.rs` passes.

**CONVENTIONS §8 compliance checklist**

- [ ] Work performed on branch `plan/06-architecture-test-hygiene`; PR opened with all required description sections (Plan link · Findings addressed · Migrations included · Breaking API/OpenAPI changes · Test evidence · Rollback procedure · Deferred follow-ups).
- [ ] All gates in CONVENTIONS §2 pass, plus `cargo deny check` (Rust set; frontend set not applicable), with real output read — and DB-backed suites confirmed to have **run**, not skipped.
- [ ] **Unit tests** delivered and passing (table above).
- [ ] **E2E tests** delivered and passing at the HTTP level against real PostgreSQL 16 + pgvector (table above).
- [ ] Every new error/notice string has an i18n key + English default in the Rust catalog, mirrored into `docs/i18n-response-catalog.json`, with a test asserting presence. *(This plan adds no new string but closes the last pre-existing gap and adds the enforcing tests.)*
- [ ] Frontend items — **not applicable** (no console code in this plan).
- [ ] Auth-touching items — **not applicable to authn/authz semantics**; Module 0 (secret type) and Module 16 (replay identity) are security-relevant narrowings and are called out explicitly in the PR with their evidence.
- [ ] No secret-leak: verified by `tests/secret_leak_snapshots.rs`, `tests/content_leak_snapshots.rs`, the masking and `SETUP_READINESS_SQL` guard tests, the module-0 compile-failure transcript, and the requirement that all repository fakes use synthetic values.
- [ ] PR **merged** with all gates green — not merely opened.

---

## Risks & Rollback

**Security.** Low, and net positive. Module 0 removes a whole class of leak by construction. Module 16 narrows replay isolation — strictly safer, never wider. The main risk is *accidentally* altering idempotency/audit/authorization sequencing during the split. Mitigated four ways: the "mechanical move, not redesign" constraint; leaving `AdminCommandRunner` and the repository envelope untouched; the before/after signature snapshot; and the two new e2e tests (`every_admin_mutation_still_writes_exactly_one_audit_row`, `every_admin_mutation_still_honours_its_required_scope`) that would catch a dropped call the existing suite might not.

**Module 16 specifically.** Changing the fingerprint changes the identity half of the idempotency ledger key. If the legacy read-path fallback is wrong or missing, in-flight `Idempotency-Key`s stop replaying and a client's retry executes a second time. This is the highest-consequence change in the plan. It is mitigated by following `tests/idempotency_hash_migration.rs`'s proven pattern exactly, by `a_legacy_fingerprint_row_still_replays_after_unification`, and by the fallback being read-only (the write path only ever emits the unified value, so there is no possibility of writing a value that later cannot be found).

**Module 9 specifically.** The previous draft of this module did not compile. The extract-first ordering (steps 2-3 before any deletion) is the mitigation, and step 1's three greps are the gate. If `grep -rn "execute_chat\|stream_chat" src/ tests/` returns anything outside the three dead files, stop — `executor.rs` is not dead and the module must be re-scoped, not forced.

**Data-migration.** None. No schema change ships to production; the only new DDL (`CREATE SCHEMA moira_test_*`) is test-only. Module 16 is handled by read-path fallback, so a mixed-version fleet during rollout is safe in both directions.

**Compatibility.** If the repository-trait extraction (module 8) accidentally changes a SQL query's behavior while "just" adding a trait, that is a regression — guarded by requiring trait method bodies to be a verbatim lift of the existing `pub async fn` bodies, not a rewrite. At 78 methods this is the largest surface for that error in the plan.

**Test-isolation risk.** Schema-per-fixture plus database-wide `LISTEN/NOTIFY` is the sharpest edge in this plan, and it now spans three fixtures rather than one. If cross-schema notification bleed causes flakes, the fallback is to skip the listener in fixtures that do not need it (module 13) rather than to reinstate the global mutex.

**Scope risk.** This plan is now noticeably larger than the previous draft claimed, chiefly because module 8 is 78 methods rather than ~45 and module 16 is new. If it will not fit, the honest reductions, in order of preference: defer module 8's `runtime`+`conversation` traits (61 of the 78 methods); defer module 13's `execution_policy_if_match.rs` fixture. **Do not** reduce by dropping module 0 or module 16 — the first is six lines and closes a proven leak class, and the second is the item `src/application/admin.rs` explicitly assigned to this plan. **Do not** silently narrow: state what was cut, in the PR and in `docs/todo.md`, marked PARTIAL.

**Deployment.** None — no migration step, no config change, no restart-order dependency.

**Rollback procedure.** Each wave lands as its own reviewable commit within the single plan PR; the waves are file-disjoint by construction, so a regression traced to one wave can be `git revert`ed independently. Module 0 is commit #1 and is independently revertable. Post-merge, `git revert` of the merge commit restores `main` exactly — there is no data to unwind and no migration to reverse. The one asterisk is Module 16: reverting after rows have been written with unified fingerprints leaves those rows unfindable by the reverted (per-family) formulas. The blast radius is bounded by `idempotency_records.expires_at`; state this in the PR.

**Deferred follow-ups.** Tracked, not dropped:
- **Plan 06b — If-Match TOCTOU**, 33 sites (§17.4). Recommended sequencing `06 → 06b → 07`.
- **P2-2 residual:** `rig_core` remains imported in `src/application/execution.rs` (`build_completion_request`, `first_text`) and `src/application/public.rs`. Moving those behind the `RuntimeFactory` seam is the right eventual shape.
- **P2-3 residual**, if modules 8's runtime/conversation traits were deferred.
- **`patch_credential`** (`src/application/admin.rs:781-811`) bypasses `AdminCommandRunner` while its sibling mutations use it — an idempotency/atomicity inconsistency to fix deliberately, not incidentally.
- **`runtime_admin.rs`'s idempotency scheme** remains two-phase and non-transactional (`idempotency_replay`/`record_idempotency`) unlike `admin.rs`'s transactional envelope. Module 16 fixes its *identity*, not its *atomicity*.
- **The `moira_runtime_config` NOTIFY payload carries no `tg_op`**, limiting any future listener precision. Adding it requires a migration.
- **The four `moira.notice.*` catalog entries have zero consumers.** Plan 07 is expected to be the first.
- P2-5 (health/circuit state not an input to candidate ranking), P2-6 (connection-pool dev-scale sizing), P2-7 (embedding-dimension policy), P2-10/P2-11 (container/Helm hardening) remain open P2 findings **not** addressed here.
