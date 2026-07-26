# Plan 07 — Identity Foundation: Owner/Admin Claiming + Runtime Auth Settings

> **Binding cross-cutting spec:** `plans/CONVENTIONS.md`. Where anything below conflicts with that file, **CONVENTIONS.md wins**. This plan has been re-audited against the real tree and brought into compliance with CONVENTIONS §1 (branch/PR), §2 (gates), §3 (unit **and** e2e), §4 (i18n), §7.2/§7.3/§7.5 (auth configured in settings at runtime), and §8 (Definition of Done).

---

## §0 — Wave 0: drift against the tree (re-audit 2026-07-26, HEAD `c45257f`)

**Read this section before any other. The body of this plan was written against the pre-03 tree and
plans 03, 04, 05, 06, 06b and 06c have all merged since.** An exhaustive citation-by-citation audit
checked ~62 file:line references: **~14 are true, ~8 are true within a few lines, and ~40 are stale.**
Three would fail to compile, one would cause a silent production regression, and three instruct the
implementer to work around defects that plan 06b already fixed.

The rule from plan 06's Wave 0 applies again: **where §0 and the body disagree, §0 wins.** The body is
left in place because its *design* is still sound — it is the citations, not the intent, that rotted.

### §0.1 Blockers — these break the build or corrupt state

| # | Body says | Reality | Required change |
|---|---|---|---|
| **B1** | Migrations `0009_admin_identity_claims.sql`, `0010_auth_provider_settings.sql`, rollback `0011_…`; "`0008` is the current highest" (`:127`) | `0009`, `0010` **and** `0011` are all taken (`backfill_false_indexed_ingestion_status`, `list_cursor_indexes`, `retention_indexes`). Next free is **`0012`** | Renumber to **`0012`**, **`0013`**, rollback **`0014`** — in all ~14 places (`:16-17`, `:40`, `:71-72`, `:129`, `:207`, `:322`, `:777`, `:993`, `:1044`). The append-only window is now `0001-0011`, not `0001-0008` |
| **B2** | `AdminCommandRunner::new(self.repo.clone())` (`:521`) | Takes **two** args. Every real call site is `AdminCommandRunner::new(self.repo.clone(), command_hasher(self.state))` (`src/application/admin_command.rs:169`) | Add the hasher argument |
| **B3** | Attach `notify_moira_runtime_config_change()` to `auth_provider_settings` (`:273-277`); "the existing mechanism, no new channel" (`:866`) | **Silently resets every provider circuit breaker on every auth-settings write.** `auth_provider_settings` is absent from `CIRCUIT_UNAFFECTED_RESOURCE_TYPES` (`src/infra/db.rs:119-138`), so it falls to the `other =>` arm (`:175-181`) → `CircuitResetScope::All` **plus a `warn!` per write**. The plan mentions circuits nowhere | Add `"auth_provider_settings"` to `CIRCUIT_UNAFFECTED_RESOURCE_TYPES`, **with a test**. See §0.3 |
| **B4** | "same `etag_headers` / `require_if_match` / **`ensure_version` (`:86-95`)** helpers" (`:664`) | `ensure_version` **was deleted** by plan 06b (`498221a`). `src/http/admin.rs:85-90` is now `optional_if_match` | Drop the reference. Use `require_if_match` + pass `expected_version` into the repository, per §0.2 |
| **B5** | "update the two exhaustive `ActorType` matches in `src/security/authz.rs` (`:119`, `:146`)" (`:86`, `:301`, `:505`, `:765`, `:1046`) | **There are no such matches, and never were.** `8039c53` replaced the old `!= ConsumerKey` test with an allow-list constant `ADMIN_IMPLYING_ACTOR_TYPES` (`src/security/authz.rs:129-133`) | **Do nothing in `authz.rs`.** Omission from the allow-list *is* denial. Reviewer item `:765(e)` is unsatisfiable as written and becomes "`SetupToken` is absent from `ADMIN_IMPLYING_ACTOR_TYPES`" — moot under D1 below |
| **B6** | "**Known defect to not copy:** the issuer handlers version-check outside the transaction (`admin.rs:1449-1452`, `:1480-1483`, `:1532-1535`, `:1563-1566`) — a TOCTOU window" (`:666`, `:861`); listed again as a deferred follow-up (`:1056`) | **Fixed by plan 06b.** All 33 sites now pass `expected_version` into the transaction and evaluate it via `lock_and_match_version` under `select … for update` (`src/infra/repositories/admin.rs:2503-2516`) | Delete the warning, the contrast at `:861`, and the deferred item at `:1056`. Copy `lock_and_match_version` + the `*_VERSION_FOR_UPDATE` consts (`:2495-2500`), **not** `rotate_credential`'s inline form |
| **B7** | "Reuse `AdminCommandRunner`/`admin_command_spec`/`AdminCommandMutation` from `src/application/admin_command.rs` verbatim" (`:521`); "follows `src/application/admin.rs`'s `create_credential` (`:483-544`)" (`:534`) | **`src/application/admin.rs` does not exist** — plan 06 split it into `src/application/admin/{mod,shared,applications,providers,credentials,keys,jwt_issuers,audit}.rs`. `admin_command_spec` (`shared.rs:383`), `success_audit` (`:402`) and `command_hasher` (`:163`) are `pub(crate)` in `admin/shared.rs` | Retarget to `src/application/admin/shared.rs` and `src/application/admin/credentials.rs:50-111` |
| **B8** | `AdminCommandMutation::new(record, 201, Some(record.id.to_string()))` used as a value (`:521`, `:536`) | Returns `Result<Self, AppError>` (`src/application/admin_command.rs:136`) | Add `?` |

### §0.2 Scope decisions taken at Wave 0

Both are recorded with their reversal conditions in `plans/reports/EXECUTION-LEDGER.md`.

**D1 — the setup-token credential path is DEFERRED, not implemented.**
This plan's second claim credential (`admin_setup_tokens` table, `ActorType::SetupToken`, the
one-time-token gate on `POST /api/v1/admin/setup/claim`) **is cut from scope.** Plan 08's console
declares `setup_token?: string` in its DTO (`plans/08-…:697`) and **never sends it** — its entire
setup triple uses `X-Moira-System-Key` (`plans/08-…:701-706`). The path was specified and never
exercised.

Consequences: no `admin_setup_tokens` table, no `ActorType::SetupToken` variant, no
`GeneratedSetupTokenResponse`, and the ~8 tests covering that path are not written. The DTO keeps
`setup_token: Option<String>` so 08's generated client still typechecks against the schema, and the
field is documented as reserved-and-rejected rather than silently ignored. **B5 becomes moot** — the
new `ActorType` variant that made the allow-list question urgent no longer exists.

**D2 — the `moira:admin` grant applies on the ADMIN plane only.**
`authenticate_admin` (`src/security/auth.rs:308`) and `authenticate_caller` (`:353`) **both** delegate
to the same `authenticate_trusted_jwt` (`:474`), and `authenticate_caller` returns that actor verbatim
for a bare bearer token (`:386-388`). Injecting the grant inside `authenticate_trusted_jwt` — which is
what the body implies — therefore puts `moira:admin` on the **public execution API**, where combined
with admin implication it satisfies `moira:execution:override-credential`, `override-model` and
`moira:identity:delegate`.

**The grant lookup goes in `authenticate_admin`, applied to the actor *after*
`authenticate_trusted_jwt` returns.** `combine_consumer_and_jwt` (`:941`) already strips `moira:admin`
on the consumer+JWT path, so admin-plane-only is the direction the existing code already goes; a bare
JWT carrying admin onto the public surface would be the one path that disagrees. **Required test:** a
granted identity receives **403** on a public-API scope it does not independently hold.

### §0.3 Additional required work the body does not contain

| Item | Why |
|---|---|
| Add `"auth_provider_settings"` to `CIRCUIT_UNAFFECTED_RESOURCE_TYPES` (`src/infra/db.rs:119-138`) + a test asserting `circuit_reset_scope` returns `Unaffected` for it | B3. Without it every auth-settings write discards breaker state that was earned by observing real failures and cannot be rebuilt |
| **Regenerate `docs/openapi.json` unconditionally**, via `UPDATE_SNAPSHOTS=1 cargo test --lib http::tests::committed_openapi_matches_the_generated_document` | The body treats this as conditional on whether 05 landed (`:46`, `:744`, `:976`). **It landed** (`3ea8037`). Two gates enforce it: `src/http/mod.rs:1649` and `tests/openapi_drift.rs:100`. Adding 10 operations without regenerating fails both |
| Satisfy four spec gates the body predates: `every_if_match_operation_declares_the_documented_precondition` (`src/http/mod.rs:1122`), `atomic_admin_idempotency_contract_is_explicit` (`:907`), `once_only_key_responses_use_the_secret_envelope` (`:884`), `committed_openapi_matches_the_generated_document` (`:1649`) | All 8 new `If-Match` operations and every new idempotent operation must declare their preconditions in the utoipa annotations |
| **Write the missing Module 13.** `:745` and `:754` assign "module 13" (the `AppState` auth-settings cache field, the `db.rs` listener hook, the `settings.rs` TTL) to Agent 6, but Detailed Implementation stops at Module 12 | The DoD item `:1001` and the e2e test `an_auth_settings_write_invalidates_the_cache_via_listen_notify` (`:968`) both depend on a cache that no module specifies |
| Register new routes in **`admin_routes()`** (`src/http/mod.rs:466`), not `documented_router()` (`:274`) | `documented_router` now only merges route groups and applies layers |
| Re-export or mirror `header_string` (`src/security/auth.rs:1086`) | It is a module-private free function; `src/security/mod.rs:9-21` does not export it, so a new `src/http/identity.rs` cannot call it |

### §0.4 Body claims that are simply false

| Body | Reality |
|---|---|
| "`moira.error.database_unavailable` … has **no** catalog entry today (verified). The earlier draft of this plan asserted it already existed. **It does not.**" (`:719`) | **It does** — `src/i18n/catalog/errors.rs:29-32`. The earlier draft was right and the correction was wrong |
| "`idempotency_in_progress` … no catalog entry today" (`:720`) | **Present** — `src/i18n/catalog/errors.rs:49-52` |
| The whole "if 06 has not landed, 07 must add these two entries itself" contingency (`:722`) | Moot. 06 landed; both keys are catalogued. Note also that plan 06c made a missing catalog entry a **compile** error, so this class of gap can no longer reach review |
| "`AuthSettings` currently holds **only** `admin` and `caller` sub-structs" (`:572`) | It also holds `jwks: JwksFetchSettings` (`src/config/settings.rs:100-101`) |
| "`src/http/mod.rs` has **8** spec tests" (`:294`, `:976`) | ~26 |
| `tests/execution_lifecycle.rs` "(14)" (`:971`) | 18 |

### §0.5 Citation staleness by file — assume every line number is wrong until re-checked

| File | Status |
|---|---|
| `src/security/auth.rs` | **12 of 12 cites stale.** The file grew ~270 lines. Real anchors: `ActorType` `:30-39`, `authenticate_admin` `:308`, `authenticate_caller` `:353`, `verify_api_key` `:408`, `authenticate_trusted_jwt` `:474`, `actor_from_trusted_claims` `:826`, `header_string` `:1086` |
| `src/http/mod.rs` | **6 of 6 stale.** `router` `:213`, `documented_router` `:274`, `admin_routes` `:466`, `mod tests` `:580` |
| `src/application/admin.rs` | **File does not exist.** See B7 |
| `src/infra/repositories/admin.rs` | All stale. `AdminRepository` trait `:93`, `rotate_credential` `:475`, `claim_idempotency` `:651`, `begin_command_savepoint` `:738`, `finalize_idempotency` `:759`, `lock_and_match_version` `:2503` |
| `src/infra/db.rs` | All stale. `MIGRATOR` `:20`, `migrate()` `:40`, `spawn_runtime_config_listener` `:47`, `listen_once` `:62` |
| `src/http/admin.rs` | Mostly **true**, except `ensure_version` (B4) and the four TOCTOU sites (B6) |
| `migrations/*` | All internal cites **true**; only the *new* filenames collide (B1) |
| `src/security/authz.rs`, `src/security/crypto.rs`, `src/error.rs`, `src/application/context.rs`, `src/domain/i18n.rs` | Substantially **true** — see the audit for one-or-two-line offsets |

### §0.7 Found during Wave 1 implementation — corrections §0 itself missed

| Finding | Resolution |
|---|---|
| **The unique index at `:361` is invalid SQL.** `on auth_provider_settings (method, coalesce(issuer, ''))` — Postgres rejects a bare `COALESCE` in an index column list; an expression needs its own parentheses | Written as `(method, (coalesce(issuer, '')))`. Verified against a live server |
| `notify_moira_runtime_config_change()` begins at `migrations/0004_admin_api_contract.sql:107`, not `:108` | §0.5 marked all `migrations/*` cites clean; this one was off by one |
| "Verify whether a singleton convention already exists elsewhere in the schema and prefer it" (`:306`) | **None exists.** Grepped `0001`–`0011`. Used the `id boolean primary key default true check (id)` idiom this plan proposes |
| **The plan never says what `granted_by_actor_type`'s CHECK should allow under D1** | `'setup_token'` kept in the allowed set. D1 is *deferred with reversal conditions*, and migrations are append-only — dropping the value would make reversing D1 cost another migration, for no security gain, since Moira controls what it writes to that column. Recorded as a judgement call, not something the plan decided |

Found during Wave 2:

| Finding | Resolution |
|---|---|
| **A latent ordering bug in the implied `governing_policy` query.** Module 10 step 1 says the policy matches "on `issuer`, or via `trusted_jwt_issuer_id`". The natural spelling is `order by (issuer = $1) desc` — but `issuer` is nullable, `null = $1` is `NULL`, and Postgres sorts nulls **first** under `desc`. An issuer-less row would therefore outrank an exact match and the **wrong `allowed_email_domains` would be applied** | Written `is not distinct from`, with a DB-backed test (`an_exact_issuer_match_outranks_a_match_through_the_trusted_issuer_id`). No unit test can see this — it needs a real Postgres sort |
| **Module 9's enable-time completeness check is unreachable as `0013` is written.** `auth_provider_settings_method_shape` is an *unconditional* CHECK, not `enabled`-conditional, so no incomplete row can be stored to later be enabled | Check kept and documented in-code as forward cover if a later migration relaxes the CHECK to allow drafts, rather than silently dropping a named plan requirement |
| **`auth_provider_method_unsupported` (`:937`) is unemittable.** `AuthMethod` is a three-variant enum on a `deny_unknown_fields` DTO, so an unsupported method is a serde rejection (`invalid_request`), never a service-level condition | Not added. A catalog entry with no emitter is worse than the gap |
| **Two files outside Components & ownership had to change** — flagged rather than absorbed, per this plan's own "an unlisted file in the diff is a scope violation to raise" rule | `src/security/authz.rs`: the three `moira:auth-settings:*` scopes. Without them `AuthorizationService::require` returns `AppError::Internal` (500) for an unknown scope, making module 9 **unreachable**, not merely ungated. Scope block only — no `ADMIN_IMPLYING_ACTOR_TYPES` change. `src/application/setup.rs`: `require_setup_actor` widened to `pub(crate)` so module 9 gates on the same function as the existing endpoint rather than a second transcription of the same rule |
| `scope_invalid` is emitted as **422, not the 400 the plan states** (`:690`) | Deliberate: reuses `AuthorizationService::normalize_scopes`, the existing helper, which emits via `AppError::unprocessable` — the same status the analogous system-key and consumer-key creation paths already return. Diverging per-call-site for one code is worse than the documented drift |
| `setup_token_not_supported` is a new code the plan's table does not list | D1 says "rejected with a clear error" without naming one. `invalid_request` would fit the catalog description, but plan 08's console needs to distinguish "you sent a reserved field" from any other schema complaint |

### §0.6 What this plan must NOT do

The four `TODO(post-deploy)` markers in `src/application/runtime_admin.rs` and
`src/application/public.rs` were named `TODO(plan-07)` until `c45257f`. **This plan does not remove
them.** Their precondition is 24 hours after the **deploy** carrying plan 06 Module 16 — not after a
merge — and removing the `legacy_actor_fingerprint` read-fallbacks early makes a client retrying an
idempotent request across the deploy boundary miss its ledger row and execute a second time against
the provider. `plans/06-architecture-test-hygiene.md:377` still refers to them by the old name.

---

## Summary

**Objective.** Give Moira a Moira-native way to grant a **human** admin authority without Moira ever issuing passwords or sessions, and to hold the auth-provider configuration that grant depends on as **runtime, database-backed settings** rather than build-time environment. Concretely: a new `admin_identities` table binding admin scope to a stable `(issuer, subject)` pair from an already-trusted JWT issuer; a `setup_state` singleton for setup-required detection; single-use `admin_setup_tokens`; a new `auth_provider_settings` table holding enabled auth methods with their **non-secret config only** (decision **D7** — the OAuth client secret is owned by the console, never by Moira); the two frozen setup endpoints (`GET /api/v1/admin/setup/claim-status`, `POST /api/v1/admin/setup/claim`); a scope-gated, `If-Match`-versioned, idempotent auth-settings admin surface; and an additive extension to `src/security/auth.rs`'s Actor mapping so a trusted-JWT caller whose `(issuer, subject)` has a grant resolves to admin scope on every subsequent request.

**Why ordered here.** Per `plans/01` §2/§3, this is a **security-critical iteration that must stay pure** — no unrelated refactors (§1.2). It depends on 03 (auth/credential/middleware hardening — JWKS SSRF protection, unkeyed-hash fix, production middleware) and 05 (observability + OpenAPI-drift gate) per the dependency graph (`I03 --> I07`, `I05 --> I07`), and gates 08 (`I07 --> I08`). 06 is "recommended but not required" — this plan is additive (new services, new repositories, new tables) and does not modify the `AdminService` methods 06 splits.

**Why the auth-settings surface is in this plan and not 08.** CONVENTIONS §7.2 is binding: *auth provider configuration is runtime configuration owned by Moira's database*, consistent with how providers, models, routing, and credentials already work (`docs/project-structure.md`: "Runtime provider config belongs in PostgreSQL"). The setup wizard in 08 **writes** that configuration and the console **reads** it at boot and on invalidation — so the storage, encryption, admin API, and cache-invalidation path must exist in Moira **before** 08 starts. Putting it in 08 would mean either a Next.js iteration shipping a Rust migration and a security-critical encryption path, or auth config living in env vars in violation of §7.2. Neither is acceptable. It stays here, backend-only.

**User-visible outcome.** An operator holding the bootstrap system key (existing `bootstrap-system-key` CLI, `src/main.rs`) can (a) configure which auth methods this deployment offers and with what policy — non-secret configuration only; the OAuth client secret is never sent to Moira (**D7**) — and (b) grant a specific human's `(issuer, subject)` Moira admin scope — exactly once per identity, idempotently, and auditably. An unauthenticated caller can check whether setup is still required (a single boolean, no internal detail). After a grant, that human's future trusted-JWT-authenticated requests resolve to `moira:admin` automatically. No password, session cookie, or login page exists in Moira itself.

**Included scope.** P1-11 in full, plus the CONVENTIONS §7.2 runtime-auth-settings requirement:
- migration `0012_admin_identity_claims.sql` — `admin_identities`, `setup_state` (**no `admin_setup_tokens`** — §0.2 D1)
- migration `0013_auth_provider_settings.sql` — `auth_provider_settings` (**non-secret config only**, no secret envelope — decision **D7** — + NOTIFY trigger), **plus the `CIRCUIT_UNAFFECTED_RESOURCE_TYPES` entry that trigger requires** (§0.1 B3)
- `GET /api/v1/admin/setup/claim-status` (unauthenticated) and `POST /api/v1/admin/setup/claim` (**system-key gated only** — the one-time-token path is deferred, §0.2 D1)
- `GET /api/v1/admin/setup/auth-methods` (setup-actor gated) + the seven `/api/v1/admin/auth/providers…` admin routes
- `src/security/auth.rs` Actor-mapping extension, **hooked at `authenticate_admin`** (§0.2 D2); three new `ADMIN_SCOPES` entries. **No `ActorType::SetupToken`** and **no change to `src/security/authz.rs`'s allow-list** (§0.1 B5, §0.2 D1)
- verified-email + **DB-backed, deny-by-default** allowed-domain policy enforced at grant time **with no first-claim exemption and no bootstrap bypass** (resolved decision, 2026-07-25). With D1 there is only one claim path, which removes the token-burn ordering hazard the original two-path design carried
- `email` + `email_verified` **required on the claim path** (resolved decision, 2026-07-25) — which is what makes the domain policy enforceable and puts a human-identifiable attribute on every grant. The DTO keeps `setup_token: Option<String>`, **rejected rather than ignored**, so plan 08's generated client still typechecks
- **regenerating and committing `docs/openapi.json`** (§0.3) — mandatory, not conditional
- `GET /api/v1/admin/setup/auth-methods` **authenticated** (resolved decision **D4**, 2026-07-25) — the console calls it server-side; only `claim-status` is anonymous
- **no OAuth client secret anywhere in Moira** (resolved decision **D7**, 2026-07-25) — `auth_provider_settings` stores non-secret config only; the console owns the secret in its own `console_auth` database. Moira exposes `client_id` as the drift-protection anchor the console fingerprints against
- new `moira.error.*` / `moira.notice.*` catalog entries with tests (CONVENTIONS §4)
- keeping `bootstrap-system-key` as-is, documented as the break-glass root

**Excluded scope.** No Next.js code, no OAuth/OIDC *client* (no authorization-code flow, no token exchange, no PKCE implementation — Moira never runs an OAuth flow, per CONVENTIONS §7.1), no BFF, no session/cookie machinery (all 08). No GitHub provider, no invitation/additional-admin flow, no ownership-transfer flow (all 09). No refactor of `AdminService`, `AdminCommandRunner`, or any existing `src/security/auth.rs` method beyond the additive changes enumerated in Detailed Implementation. **If a reviewer finds this plan's diff touching a file not listed in Detailed Implementation, that is a scope violation to flag, not silently accept.**

---

## Branch & Pull Request (CONVENTIONS §1)

- **Branch:** `plan/07-identity-foundation`, cut from the **current `main`**, after 03 and 05 have merged. If 03 has not merged when work begins, the branch stacks on `plan/03-security-hardening`; in that case the PR description must name the base PR and the branch must be rebased once 03 merges.
- **Commits:** Conventional Commits (`feat: add admin identity claiming`, `feat: store non-secret auth provider settings`, `feat: resolve admin grants for trusted jwt actors`, `test: prove first-login-wins is impossible`, `docs: catalog identity error keys`).
- The PR is **not opened** until every gate in CONVENTIONS §2 / Verification passes locally.
- **PR description — required sections:**
  - **Plan link** — `plans/07-identity-foundation.md`
  - **Findings addressed** — P1-11 (plus CONVENTIONS §7.2 compliance)
  - **Migrations included** — `migrations/0012_admin_identity_claims.sql`, `migrations/0013_auth_provider_settings.sql`
  - **Breaking API/OpenAPI changes** — none against any *shipped* surface; **10 new operations added** (enumerate them), all under `/api/v1/admin/`. Must additionally reproduce **both** ⚠️ callouts from Interfaces & Contracts: (i) **changed by D7** — the client secret is gone from Moira entirely, `POST /api/v1/admin/auth/providers/{id}/rotate-secret` does not exist, and the operation count is **10, not 11**; (ii) `ClaimAdminIdentityRequest.email` is now required (`Option<String>` → `String`), `email_verified` loses its serde default, and `AdminIdentityRecord.email` is now `String`. Both are changes against the shape plans 08/09 were drafted against, which the coordinator must propagate before 08 starts.
  - **Test evidence** — unit + e2e output summary (see Verification)
  - **Rollback procedure** — see Risks & Rollback
  - **Deferred follow-ups** — see Risks & Rollback
- **Done means merged**, with all gates green and every Definition of Done item objectively verified.
- **Ordering (CONVENTIONS §1.6):** this plan **adds OpenAPI operations**. The condition is resolved: **plan 05 has landed** (`3ea8037`) and the spec is frozen, so **this PR must regenerate and commit `docs/openapi.json`** — see Verification and §0.3 for the command. There is no "whichever applies" left to decide.
- Plan 08 stacks on this branch's merged result; never force-push after 08 branches from it.

---

## Findings Addressed

**P1-11 · Identity foundation absent — no owner/admin claiming, no user model** [BE][UI][OAuth] · *Verified* (`plans/00-audit-report.md` P1-11).

- **Evidence:** Exhaustive grep across `migrations/0001-0008` and `src/` confirms no `users` table, no session/cookie store, no OAuth/OIDC client code. The only identity primitives are machine credentials: system keys (`system_api_keys`, `migrations/0003_security_foundation.sql:262-278`, Argon2id+pepper via `src/security/api_keys.rs`), consumer keys (`consumer_api_keys`, `0003:288-317`), and trusted JWT issuers (`trusted_jwt_issuers`, `0003:231-260`; JWKS-validated with a per-issuer algorithm allow-list at `src/security/auth.rs:292-343` `authenticate_trusted_jwt` → `actor_from_trusted_claims` at `:555-628`).
- **Impact:** No way to grant a human admin authority, no setup-required concept beyond the existing *structural* readiness check, and thus no safe basis for an admin console or OAuth login.
- **Correction (this plan):** the `admin_identities`/`setup_state` grant model of `plans/01` §4.4-§4.5, plus the runtime auth-settings storage CONVENTIONS §7.2 requires.
- **A closely related but distinct existing feature this plan must not collide with:** `GET /api/v1/admin/setup/status` already exists (`src/http/admin.rs:32-42` utoipa block, `get_setup_status` handler `:43-49`, backed by `src/application/setup.rs` and `src/infra/repositories/setup.rs`). It answers *"is the provider/routing configuration structurally complete enough to serve a request"* — root system key exists, and an application/route/provider/model/credential/routing-policy chain is wired end to end (`SETUP_READINESS_SQL`, `src/infra/repositories/setup.rs:45-142`). It is gated to system-key/trusted-JWT actors (`require_setup_actor`) plus `moira:setup:read`, and returns granular per-check detail. **This plan's `GET /api/v1/admin/setup/claim-status` is a different, unauthenticated, identity-claiming-specific status living beside it.** The two must not be merged or renamed into each other. The existing endpoint, its `SetupStatusResponse` shape, its `SetupChecks`/`SetupCheckName` domain types, its check ordering, and its auth gating are **untouched** by this plan. Note the guard test at `src/infra/repositories/setup.rs:164-179`, which asserts `SETUP_READINESS_SQL` never mentions `encrypted_payload`, `encrypted_data_key`, `key_hash`, `key_prefix`, `masked_secret`, or `secret_fingerprint` — this plan adds no readiness check and therefore must leave that SQL and that test alone.
- **`docs/todo.md`:** no existing line covers this; P1-11 is tracked as a roadmap iteration, not a todo bullet. No `docs/todo.md` edit is required.

**Path-convention decision (final, binding on 08/09).** `plans/01` §4.5 sketched `GET /api/v1/setup/status`. This plan finalizes the claim routes under the existing **`/api/v1/admin/setup/…`** namespace (`claim-status`, `claim`, `auth-methods`) so the identity surface sits beside the existing structural endpoint rather than minting a second top-level `setup` namespace of permanently ambiguous relationship. The segment `claim-status` (not a reuse of `status`) keeps the concepts distinct at the URL level. **Plans 08/09 bind to the exact paths, shapes, and table names frozen in Interfaces & Contracts below, not to the `plans/01` sketch.**

---

## Architecture

### Components & ownership (per `docs/project-structure.md`)

| Layer | File | Change |
|---|---|---|
| `migrations/` | `0012_admin_identity_claims.sql` (new) | `admin_identities`, `setup_state` — **no `admin_setup_tokens`** (§0.2 D1) |
| | `0013_auth_provider_settings.sql` (new) | `auth_provider_settings` + version-bump trigger + NOTIFY trigger, **and the `CIRCUIT_UNAFFECTED_RESOURCE_TYPES` entry in `src/infra/db.rs` that the NOTIFY trigger requires** (§0.1 B3) |
| `src/domain/` | `identity.rs` (new) | `SetupClaimStatusResponse`, `ClaimAdminIdentityRequest`, `AdminIdentityRecord`, `AdminIdentityStatus` — **no `GeneratedSetupTokenResponse`** (§0.2 D1) |
| | `auth_settings.rs` (new) | `AuthProviderSettingsRecord`, `…CreateRequest`, `…PatchRequest`, `AuthMethod`, `SetupAuthMethodsResponse`, `PublicAuthMethod` |
| | `mod.rs` | two additive `pub use` blocks |
| `src/infra/repositories/` | `identity.rs` (new) | `AdminIdentityRepository` trait + `PgAdminIdentityRepository` — **trait from day one**, so this plan needs no P2-3-style retrofit |
| | `auth_settings.rs` (new) | `AuthProviderSettingsRepository` trait + `PgAuthProviderSettingsRepository` |
| | `mod.rs` | two additive re-exports beside `AdminRepository` (`mod.rs:8`) |
| `src/application/` | `identity.rs` (new) | `AdminIdentityService` |
| | `auth_settings.rs` (new) | `AuthProviderSettingsService` |
| | `mod.rs` | two additive `mod` + `pub use` lines |
| `src/http/` | `identity.rs` (new) | `get_setup_claim_status`, `claim_admin_identity` |
| | `auth_settings.rs` (new) | `get_setup_auth_methods` + seven `/admin/auth/providers…` handlers |
| | `mod.rs` | additive `mod` lines + `.routes(routes!(...))` registrations in `documented_router()` (`:21-210`), and extension of the existing spec tests in `mod tests` (`:213`) |
| `src/security/` | `auth.rs` | **additive only**: `ActorType::SetupToken` variant; `apply_admin_identity_grant`; `verify_system_key_only` |
| | `authz.rs` | **additive only**: three scopes appended to `ADMIN_SCOPES` (`:8-91`); `SetupToken` handled in the two `ActorType` matches (`:119`, `:146`) |
| | `crypto.rs` | **unchanged.** Decision **D7** removes the client secret from Moira, so no new AAD scheme is added. `CredentialAadParts`/`credential_aad` (`:84-111`) keep governing *provider credentials* (the AI-provider API keys) exactly as today |
| | `mod.rs` | additive re-exports |
| `src/app/state.rs` | | additive: `auth_settings_cache` field on `AppState` (`:20-39`) |
| `src/infra/db.rs` | | additive: `listen_once` invalidates the auth-settings cache. **Cross-plan conflict: plan 06 module 14 also edits `listen_once`** — merge deliberately |
| `src/config/settings.rs` | | additive: `AuthSettings.setup_token_ttl_seconds` (`:89-95`) |
| `src/i18n/catalog/` | `errors.rs`, `notices.rs` | new entries (see i18n section) |
| `docs/` | `i18n-response-catalog.json` | mirrored entries |
| | setup runbook (exact file chosen at Wave 0) | operator-facing copy for the **deny-by-default** domain allow-list: configure `allowed_email_domains` **before** the first claim, or every claim is refused 403 (module 10) |
| `src/main.rs` | | **unchanged.** `bootstrap-system-key` remains the break-glass root exactly as-is |

### Data flow

1. **Auth configuration (operator, runtime).** Operator (system key, or an already-granted admin's trusted JWT) calls `POST /api/v1/admin/auth/providers` with a method (`google_oauth` / `generic_oidc` / `jwks`) and its **non-secret** config — issuer, discovery/authorization/token/userinfo/JWKS URLs, `client_id`, requested scopes, `allowed_email_domains`, allowed algorithms, audiences, redirect URIs. **No `client_secret` is accepted, on any operation** (decision **D7**): the OAuth client secret is owned by the console and lives in the console's own `console_auth` database; Moira neither stores nor returns it, and the request DTO has no field for it. The write fires the `moira_runtime_config` NOTIFY trigger, which invalidates the auth-settings cache on every instance via the existing `LISTEN/NOTIFY` path (`src/infra/db.rs:43-80`).
2. **Setup-required check (unauthenticated, safe).** `GET /api/v1/admin/setup/claim-status` → `AdminIdentityService::claim_status()` → reads the single `setup_state` row → returns `{ "claimed": bool }` **only**. No enumeration of who is claimed, no count, no timestamp, no issuer/subject. A boolean is the entire contract, deliberately.
3. **Bootstrap auth discovery (operator/BFF, authenticated).** `GET /api/v1/admin/setup/auth-methods` → the enabled methods' **non-secret** config, so 08's BFF can configure Better Auth server-side. Gated exactly like the existing structural endpoint (`ActorType::SystemKey` or `TrustedJwt`, plus `moira:setup:read`). **Not unauthenticated** — see Security boundaries.
4. **Claim, path A — system-key-gated.** Operator calls `POST /api/v1/admin/setup/claim` with `X-Moira-System-Key` (verified against `system_api_keys` by the existing key path) carrying `moira:admin`, plus a body naming the target `(issuer, subject, email, email_verified)`. The service validates the target issuer resolves to an **active, registered** `trusted_jwt_issuers` row (Moira never accepts a free-text issuer at claim time), enforces verified-email + allowed-domain policy, and inserts the grant inside the existing `AdminCommandRunner` envelope.
5. **Claim, path B — one-time-token-gated.** An operator can hand the *act* of claiming to the human being granted, without handing over the system key, by minting a single-use, short-lived, system-key-derived setup token. It is not a second trust root; it is a delegated, scoped, one-shot credential. The system-key path always works with **no network dependency**, which is what keeps air-gapped operation viable.
6. **Post-grant resolution.** On every future request where `authenticate_trusted_jwt` builds an `Actor` for that `(issuer, subject)`, it additionally looks up `admin_identities` and, if an active grant exists, **unions** the granted scopes onto the actor's scopes. The grant takes effect with no new Moira-specific flow — the human's existing trusted-JWT bearer token simply carries more authority.

### Security boundaries

- **Human → BFF (08):** out of scope entirely. Moira never runs an OAuth flow (CONVENTIONS §7.1).
- **BFF/operator → Moira:** the claim endpoint accepts exactly two credential shapes (system key, one-time setup token) and rejects everything else — **including a bare trusted-JWT bearer token with no prior grant**. This is the structural rejection of "first successful admin JWT wins" that `plans/01` §4.4 requires. It is enforced by an explicit allow-list in the handler (`resolve_claim_credential`), **not** by calling `state.auth.authenticate_admin`, which would happily accept a bearer JWT (`src/security/auth.rs:143-145`).
- **No self-asserted scopes (CONVENTIONS §7.5).** `actor_from_trusted_claims` copies scopes from the JWT verbatim, so an issuer configured with a `scopes_claim` lets its tokens self-assert scopes. For any issuer registered as a **console/BFF** issuer (`auth_provider_settings.trusted_jwt_issuer_id`), this plan **requires `trusted_jwt_issuers.scopes_claim IS NULL`** — validated on write and asserted by a test. Authorization for humans then comes from the `admin_identities` grant table alone, which is the entire point.
- **Moira stores no OAuth client secret at all — decision `D7` (product owner, 2026-07-25), binding.** `auth_provider_settings` holds **non-secret configuration only**. The OAuth client secret is **owned by the console** and stored in the console's own `console_auth` database (the database Better Auth already requires), encrypted at rest, written by the setup wizard, never sent to Moira, never exposed to the browser, never in `NEXT_PUBLIC_*`.
  - **Why the secret is not in Moira, stated once so it is not re-litigated.** Better Auth needs the *plaintext* secret in process to run the OAuth authorization-code exchange. Moira's secret envelope is deliberately **write-only**: `SecretCipher` has no read-back path, and there is no endpoint that returns a decrypted secret. Making the secret readable over HTTP so the console could fetch it would break Moira's load-bearing invariant that **a decrypted secret never crosses a network boundary** — an invariant judged more important than the convenience of a single configuration store. So the secret lives where it is actually used, and Moira keeps the invariant intact.
  - **Consequence for Moira's read surface: there is no secret in the payload at all.** `GET /api/v1/admin/auth/providers`, `GET …/{id}`, and `GET /api/v1/admin/setup/auth-methods` return pure configuration — issuer, URLs, `client_id`, scopes, domains, algorithms, audiences, redirect URIs. Because no secret material exists on the row, these endpoints are **safe to expose to any holder of `moira:auth-settings:read`** (and, for `auth-methods`, `moira:setup:read`) without a secret-redaction argument. The scope gate is still enforced — it is configuration disclosure control, not secret protection. *(This does not relax `GET …/setup/auth-methods`'s authentication requirement one bit: decision `D4` keeps it authenticated because the identity **configuration** is still reconnaissance-worthy. D7 removes the secret; it does not make the configuration public.)*
  - **Unaffected: Moira's provider credentials.** The AI-provider API keys in `provider_credentials` are **not** touched by D7. They remain encrypted in Moira with `SecretCipher` + `credential_aad`, with the `#[serde(skip_serializing)]` + `#[schema(ignore)]` envelope-hiding pattern on `CredentialRecord` (`src/domain/admin.rs:385-422`) and no read-back path. That pattern is correct and stays exactly where it is; D7 simply means `auth_provider_settings` never needed it.
- **Drift protection: Moira is the source of truth for `client_id` (CONVENTIONS §0, "D7 consequences").** Two configuration stores can diverge — a `client_id` changed in Moira while the console still holds the old client's secret would fail the code exchange with an opaque provider error. Moira's side of the mitigation is exactly one obligation: **expose `client_id` on the read path so the console can compare it.** That obligation is already met — `client_id` is a plain, non-secret, always-returned field of both `AuthProviderSettingsRecord` and `PublicAuthMethod` (module 3), so the console can read Moira's current `client_id` from `GET /api/v1/admin/setup/auth-methods` (its server-side boot call) or `GET /api/v1/admin/auth/providers/{id}`, fingerprint it, and compare against the fingerprint it stored beside its own secret. **Stated explicitly so plan 08 can bind to it: no new Moira endpoint, field, header, or fingerprint-computation is required — the existing `client_id` on the existing read endpoints is sufficient.** Moira deliberately does **not** compute or store the console's fingerprint: the fingerprint is a console-side artifact of a console-side secret, and having Moira hold it would put half of a secret-derived artifact back on the wrong side of the boundary. The remaining mitigations — the wizard writing both stores in one step and treating partial success as an operator-resolvable failure, and the e2e test asserting the mismatch produces an actionable keyed error — are **plan 08's** deliverables, not this plan's.
- **Secrets never leave the server.** The setup token is stored only as an Argon2id hash + prefix + fingerprint (mirroring `system_api_keys`) and is returned in plaintext exactly once, at mint time. It is the only secret material this plan writes.
- **`GET /api/v1/admin/setup/auth-methods` is authenticated. DECIDED (product owner, 2026-07-25) — confirmed, not open.** It requires `ActorType::SystemKey | TrustedJwt` **plus** the `moira:setup:read` scope, exactly like the pre-existing structural endpoint. There is no anonymous variant and no anonymous fallback in this plan.
  - **Rationale:** an unauthenticated variant would let anyone who finds a fresh instance enumerate its identity configuration — which IdP, which issuer, which client id, which allowed email domains — a reconnaissance gift on precisely the surface an attacker would target during the setup window, when the deployment is least defended.
  - **Why the setup wizard still functions:** 08's console calls this endpoint **server-side**, from its BFF, using the system key it already holds (CONVENTIONS §6.5: secrets never descend past the page/server boundary). The browser never calls it and never sees the credential. The wizard's *first* call — the one that must work before any credential exists — is `GET /api/v1/admin/setup/claim-status`, which is anonymous by design.
  - **Contrast, deliberately (this is the whole design):** `claim-status` is anonymous **because its entire response is `{"claimed": bool}`** — one bit, which an attacker could infer anyway by observing that the instance is freshly deployed, and which a wizard genuinely needs before it has any credential. `auth-methods` is authenticated **because its response is configuration detail** — issuer, client id, discovery URL, domain policy — that reveals how the deployment authenticates humans. The dividing line is *information content*, not endpoint category: one bit of "is setup done" is free; the identity configuration is not. Anyone proposing to relax `auth-methods` must first explain why that line has moved.
  - **Do not add a public method-names-only variant in this plan.** If 08 ever demonstrates a concrete need for one, it is a separate, separately-reviewed change with its own threat model — not a fallback an implementer may reach for when the server-side call is inconvenient to wire.
- **Identity stability.** The unique key is `(issuer, subject)`, never email (`plans/01` §4.4: "email is mutable and reassignable"). Email is an attribute column only, never part of the uniqueness constraint or the `authenticate_trusted_jwt` lookup key.
- **SSRF.** `discovery_url` and `jwks_url` on `auth_provider_settings` are operator-supplied URLs that Moira may fetch. They **must** go through plan 03's SSRF-hardened fetcher (P1-2 correction). If 03 has not landed, this plan does **not** fetch them at all — it stores and validates scheme/host syntactically only, and the fetch is deferred. Do not hand-roll a second fetcher.

### DB/migration changes

Two new append-only migrations. **`0011` is the current highest** (verified at HEAD `c45257f`): 03/04/05 did land first and did add migrations, which is why the numbers below are `0012` and `0013` rather than the `0009`/`0010` this plan originally reserved. The append-only window is `0001-0011`. If further plans merge before this branch is cut, re-verify and renumber again — and update `plans/08` alongside.

#### `migrations/0012_admin_identity_claims.sql`

Style follows `0003_security_foundation.sql` / `0004_admin_api_contract.sql`: uuid PK `default gen_random_uuid()`, `created_at`/`updated_at timestamptz not null default now()`, `deleted_at timestamptz` where the table supports mutation, `status varchar(32) … check (…)` enums, partial unique indexes `where deleted_at is null`.

```sql
create table if not exists admin_identities (
    id uuid primary key default gen_random_uuid(),
    trusted_jwt_issuer_id uuid not null references trusted_jwt_issuers(id),
    issuer text not null,
    subject varchar(256) not null,
    email varchar(320),
    email_verified boolean not null default false,
    granted_scopes text[] not null default array['moira:admin'],
    granted_by_actor_type varchar(32) not null
        check (granted_by_actor_type in ('system_key', 'setup_token')),
    granted_by_subject varchar(256),
    status varchar(32) not null default 'active'
        check (status in ('active', 'revoked', 'deleted')),
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    revoked_at timestamptz,
    deleted_at timestamptz,
    version bigint not null default 1
);

create unique index if not exists admin_identities_issuer_subject_active_unique
    on admin_identities (issuer, subject)
    where deleted_at is null;

create index if not exists admin_identities_lookup_idx
    on admin_identities (issuer, subject)
    where deleted_at is null and status = 'active';

create table if not exists setup_state (
    id boolean primary key default true check (id),
    claimed boolean not null default false,
    claimed_admin_identity_id uuid references admin_identities(id),
    claimed_at timestamptz,
    updated_at timestamptz not null default now()
);

insert into setup_state (id, claimed) values (true, false)
    on conflict (id) do nothing;

create table if not exists admin_setup_tokens (
    id uuid primary key default gen_random_uuid(),
    token_prefix varchar(64) not null,
    token_hash text not null,
    fingerprint varchar(128) not null,
    pepper_version varchar(64) not null,
    issued_by_subject varchar(256),
    target_issuer text,
    target_subject varchar(256),
    expires_at timestamptz not null,
    consumed_at timestamptz,
    created_at timestamptz not null default now()
);

create unique index if not exists admin_setup_tokens_prefix_unique
    on admin_setup_tokens (token_prefix);

create index if not exists admin_setup_tokens_fingerprint_idx
    on admin_setup_tokens (fingerprint);

drop trigger if exists admin_identities_bump_version on admin_identities;
create trigger admin_identities_bump_version
before update on admin_identities
for each row execute function moira_bump_resource_version();
```

**Reuse, do not redefine, `moira_bump_resource_version()`** — it already exists (`migrations/0004_admin_api_contract.sql:17-24`) and is the function every other versioned table uses (`:41-47`). The earlier draft of this plan defined a bespoke `moira_bump_admin_identity_version()`; that is a needless duplicate and is dropped.

**Design notes** (no open product questions remain in this section; the four decisions this plan once carried are resolved — see module 10, module 11, Security boundaries, and Definition of Done):
- `setup_state` is a **single-row** table; `id boolean primary key default true check (id)` is an explicit singleton idiom. **Verify at execution time** whether a singleton convention already exists elsewhere in the schema and prefer that one for consistency.
- It tracks whether *any* admin identity has **ever** been claimed, independent of `admin_identities.status`, so that revoking the sole admin cannot silently reopen the unauthenticated land-grab window. Re-opening claim-ability after a revocation is a deliberate operator action via the system-key path, not an automatic side effect. This is this plan's reading of `plans/01` §4.5 ("reads whether any admin identity is claimed") and it is the safe one.
- `admin_setup_tokens` mirrors `system_api_keys`' exact column vocabulary (`key_prefix varchar(64)`, `key_hash text`, `fingerprint varchar(128)`, `pepper_version varchar(64)` — verified `0003:262-278`) rather than inventing a new secret-storage shape.
- `trusted_jwt_issuer_id` (FK) proves the grant is bound to a currently-registered issuer row; `issuer` is denormalized beside it so the hot-path lookup in `authenticate_trusted_jwt` needs no join. The denormalization cannot drift because `trusted_jwt_issuers_issuer_active_unique` (`0003:254-256`) makes the string a key.

#### `migrations/0013_auth_provider_settings.sql`

This is the CONVENTIONS §7.2 table: enabled auth methods and their **non-secret** config. **Decision `D7`: there is no secret envelope on this table** — no `encrypted_payload`, `encryption_algorithm`, `encryption_version`, `encrypted_data_key`, `nonce`, `secret_fingerprint`, or `masked_secret` column, and therefore no envelope-completeness CHECK constraint. The OAuth client secret is owned by the console and stored in the console's own `console_auth` database. Every column below is safe to read.

```sql
create table if not exists auth_provider_settings (
    id uuid primary key default gen_random_uuid(),
    method varchar(32) not null
        check (method in ('google_oauth', 'generic_oidc', 'jwks')),
    display_name varchar(256) not null,
    enabled boolean not null default false,

    -- non-secret configuration (CONVENTIONS §7.2)
    issuer text,
    discovery_url text,
    authorization_url text,
    token_url text,
    userinfo_url text,
    jwks_url text,
    client_id text,
    requested_scopes text[] not null default array['openid', 'email', 'profile'],
    allowed_email_domains text[] not null default '{}',
    allowed_algorithms text[] not null default array['RS256'],
    expected_audiences text[] not null default '{}',
    redirect_uris text[] not null default '{}',
    trusted_jwt_issuer_id uuid references trusted_jwt_issuers(id),

    -- NO client-secret columns. Decision D7: the OAuth client secret is owned by
    -- the console and stored in the console's own console_auth database. Moira
    -- never stores it and never returns it. Do not add an envelope here.

    metadata jsonb not null default '{}'::jsonb,
    status varchar(32) not null default 'active'
        check (status in ('active', 'disabled', 'deleted')),
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    deleted_at timestamptz,
    version bigint not null default 1,

    constraint auth_provider_settings_method_shape check (
        (method = 'jwks'
            and jwks_url is not null
            and client_id is null)
        or (method in ('google_oauth', 'generic_oidc')
            and client_id is not null
            and (issuer is not null or discovery_url is not null))
    )
);

create unique index if not exists auth_provider_settings_method_issuer_active_unique
    on auth_provider_settings (method, coalesce(issuer, ''))
    where deleted_at is null;

create index if not exists auth_provider_settings_enabled_idx
    on auth_provider_settings (method)
    where deleted_at is null and status = 'active' and enabled;

create index if not exists auth_provider_settings_cursor_idx
    on auth_provider_settings (created_at desc, id desc)
    where deleted_at is null;

drop trigger if exists auth_provider_settings_bump_version on auth_provider_settings;
create trigger auth_provider_settings_bump_version
before update on auth_provider_settings
for each row execute function moira_bump_resource_version();

drop trigger if exists auth_provider_settings_notify on auth_provider_settings;
create trigger auth_provider_settings_notify
after insert or update or delete on auth_provider_settings
for each row execute function notify_moira_runtime_config_change();
```

**Reuse, do not redefine, `notify_moira_runtime_config_change()`** — it exists (`0002:101`, re-declared `0003:439`, `0004:108`) and emits `json_build_object('resource_type', tg_table_name, 'resource_id', changed_id::text)` (`0004:116-119`) on channel `moira_runtime_config`. Attaching to it is exactly how CONVENTIONS §7.2's "changing auth settings must invalidate the runtime cache through the existing Postgres LISTEN/NOTIFY path" is satisfied — **no new channel and no new mechanism is invented**. Match the trigger-attachment style used at `0004:132-162`.

⚠️ **Attaching the trigger is NOT free, and this plan treats it as free (§0.1 B3). A required Rust
change accompanies this migration.**

`listen_once` (`src/infra/db.rs:62-88`) now routes every notification through
`circuit_reset_scope(payload)` (`:147-183`). That function maps `providers` and `provider_models` to
narrow scopes, maps the 18 tables in `CIRCUIT_UNAFFECTED_RESOURCE_TYPES` (`:119-138`) to
`Unaffected`, and falls through to **`CircuitResetScope::All` plus a `warn!` for anything it has
never heard of** — deliberately, because an unknown table is treated as unknown rather than assumed
harmless (`:116-118`).

`auth_provider_settings` is a table it has never heard of. So without the accompanying change, **every
auth-settings write discards every provider circuit breaker in the process** and logs a warning. That
matters more than it sounds: the two runtime caches are keyed by version and rebuild on the next read
(`:72-74` accepts that cost explicitly), but breaker state is *earned* by observing real failures and
cannot be rebuilt — resetting it sends live traffic back at a provider that was just failing.

**Required with this migration:** add `"auth_provider_settings"` to
`CIRCUIT_UNAFFECTED_RESOURCE_TYPES`, plus a unit test asserting `circuit_reset_scope` returns
`Unaffected` for a payload naming it. Auth settings do not affect provider health, so `Unaffected` is
the honest classification. Note also that both runtime caches *are* invalidated unconditionally on
every notification — that is the existing design, and it is what makes the NOTIFY attachment do the
job CONVENTIONS §7.2 asks for.

**DDL style — verified, no need to re-check:** the existing triggers do use
`after insert or update or delete … for each row` (`migrations/0004_admin_api_contract.sql:132-134`),
which is the form written above.

**Deliberate non-decisions.** No secret-envelope columns and no `key_id` column exist on this table, because no secret is stored on it (**D7**). Envelope key rotation remains a concern of `provider_credentials` alone and stays a deferred follow-up there — see Risks & Rollback. **Do not "restore symmetry" with `provider_credentials` by adding envelope columns here**; the asymmetry is the decision, not an oversight.

### API & OpenAPI changes

**Ten new operations**, all under `/api/v1/admin/` — see Interfaces & Contracts for exact shapes. (It was eleven before decision **D7** removed `POST /api/v1/admin/auth/providers/{id}/rotate-secret`; there is no client secret in Moira to rotate.) All register in `src/http/mod.rs::documented_router()` (`:21-210`), the claim routes adjacent to the existing `.routes(routes!(admin::get_setup_status))` line and the auth-provider routes adjacent to the `trusted_jwt_issuer` block.

Two verified facts this plan depends on:

1. **No router-level auth middleware exists on `/api/v1/admin` today** — every admin handler authenticates itself in its own body via `admin_actor(state, headers)` (`src/http/admin.rs:51-56`, a thin wrapper over `state.auth.authenticate_admin`). An unauthenticated handler under the admin prefix is therefore structurally possible. **However, plan 03 lands first and may introduce path-scoped production middleware (P1-3).** If it does, `GET /api/v1/admin/setup/claim-status` must be registered as an explicit, documented, reviewed exemption from any admin-path auth middleware, and `POST /api/v1/admin/setup/claim` from any bearer-required middleware (its credentials are a system key or a body token, not a bearer JWT). **Wave 0 must check 03's landed state for this and stop if the exemption cannot be made cleanly.**
2. **OpenAPI docs exposure:** when `MOIRA_DOCS__EXPOSE_ADMIN` is false, `public_document` strips every path starting `"/api/v1/admin/"` (`src/http/openapi.rs:156-162`; the authenticated full-doc branch is at `:111-130`). This affects spec visibility only, never routing — `claim-status` still serves unauthenticated traffic regardless. Acceptable: 08's wizard consumes the endpoint, not the public spec. **Note this in the endpoint's doc comment** so nobody "fixes" the spec omission by moving the route out from under `/api/v1/admin/`, which would also move it out of the admin-strip protection for the other nine.

`src/http/mod.rs`'s existing spec tests (`mod tests` at `:213` — 8 tests, notably `generated_openapi_covers_every_registered_route:226`, `every_operation_documents_request_ids_and_protected_operations_document_auth…:422`, `setup_status_contract_is_typed_and_exact:480`, `every_local_schema_reference_resolves:646`) **must be extended, not bypassed**, to cover the ten new routes. This is required, not optional. In particular `every_operation_documents_request_ids_and_protected_operations_document_auth…` will need an explicit, commented allowance for `get_setup_claim_status` being intentionally unauthenticated.

### Backward compatibility

Fully additive. No existing endpoint, DTO, scope, or table changes shape.

- `src/security/auth.rs`: `authenticate_trusted_jwt` gains one DB lookup and a scope union. For every `(issuer, subject)` with **no** grant row — the overwhelming majority of trusted-JWT callers, including every machine-to-machine integration today — behavior is byte-identical: the lookup returns no row, no scopes are added, the `Actor` is exactly what `actor_from_trusted_claims` already produces. **A test asserts this byte-identity explicitly.**
- `ActorType::SetupToken` is a new enum variant. `ActorType` derives `Serialize/Deserialize` with `rename_all = "snake_case"` (`auth.rs:27-37`), so it appears as `"setup_token"` in audit rows and actor JSON. The two exhaustive matches in `src/security/authz.rs` (`:119`, `:146`) must be updated to treat `SetupToken` like `ConsumerKey` — **denied** admin-scope implication. A setup token authorizes exactly one action (calling `POST …/setup/claim`) and grants no standing authority.
- `ADMIN_SCOPES` (`src/security/authz.rs:8-91`) gains three entries. `AuthorizationService::require` returns `AppError::Internal` for an unknown scope (`authz.rs:103-106`), so the scopes **must** be added there or every auth-settings call 500s.

### Deployment implications

Standard migrate-then-deploy. Both migrations must run before the new binary serves traffic (already true of every prior migration per `src/main.rs`'s `ProcessMode::Migrate` / `migrate_on_startup` handling). No new deployment step. No config-flag gate is needed: the new endpoints are net-new paths and the auth-service change is a no-op until a grant exists. But see Definition of Done for the explicit requirement that a fresh deployment reports `claimed: false` **and** rejects any bare-trusted-JWT claim attempt, so the very first request after deploy cannot itself become an unauthorized land-grab.

### Failure & recovery

- A claim that fails partway is atomic by construction — it runs inside the existing `AdminCommandRunner` envelope, whose correctness the audit's positive findings already establish ("single transaction, `pg_try_advisory_xact_lock` single-winner, savepoint-scoped business-failure rollback, `finalize` once-only").
- Lost/compromised system key: unaffected by this plan; the existing `system_api_keys` rotation/revocation story governs recovery and is not weakened.
- Wrongly-granted first admin: a second system-key-gated claim can grant a further admin. The `status`/`revoked_at` columns exist from day one, so an operator can revoke via direct DB access before a dedicated revoke endpoint exists (deliberately deferred — see Risks & Rollback).
- Lost or compromised OAuth client secret: **not a Moira failure mode (D7).** Moira holds no client secret, so there is nothing here to lose, rotate, or corrupt. The operator rotates the secret at the identity provider and updates the console's own `console_auth` store; Moira is involved only if the `client_id` also changes, which is an ordinary `PATCH /api/v1/admin/auth/providers/{id}` — and see the drift-protection contract in Security boundaries for how the console detects a `client_id` that moved out from under its stored secret.
- Corrupted `auth_provider_settings` row: it is plain configuration, so recovery is a `PATCH` or a disable-and-recreate. There is no decryption step on this table and therefore no fail-closed decrypt path to reason about.

---

## Detailed Implementation

### Module 1 — Migrations

`migrations/0012_admin_identity_claims.sql` and `migrations/0013_auth_provider_settings.sql`, exactly as specified above. Append-only (`docs/project-structure.md`) — never edited after merge; any correction is a new migration. Re-verify the highest existing migration number at execution time.

### Module 2 — `src/domain/identity.rs` (new)

```rust
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SetupClaimStatusResponse {
    pub claimed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ClaimAdminIdentityRequest {
    pub issuer: String,
    pub subject: String,
    /// REQUIRED on **both** credential paths (system key and setup token).
    pub email: String,
    /// REQUIRED on both paths; must be `true` or the claim is refused 403.
    pub email_verified: bool,
    #[serde(default = "default_admin_grant_scopes")]
    pub scopes: Vec<String>,
    /// Presented instead of `X-Moira-System-Key`. Never echoed back.
    pub setup_token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AdminIdentityRecord {
    pub id: Uuid,
    pub issuer: String,
    pub subject: String,
    /// Always present — a grant cannot be created without a verified email.
    pub email: String,
    pub email_verified: bool,
    pub granted_scopes: Vec<String>,
    pub status: AdminIdentityStatus,
    pub created_at: DateTime<Utc>,
    pub version: i64,
    /// i18n envelope for the success message (CONVENTIONS §4.2).
    pub notice: ResponseText,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AdminIdentityStatus { Active, Revoked }

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct GeneratedSetupTokenResponse {
    pub token_prefix: String,
    pub fingerprint: String,
    pub expires_at: DateTime<Utc>,
    pub target_issuer: Option<String>,
    pub target_subject: Option<String>,
    /// Returned exactly once, at mint time. Never persisted in plaintext.
    pub setup_token: String,
    pub notice: ResponseText,
}

fn default_admin_grant_scopes() -> Vec<String> { vec!["moira:admin".to_string()] }
```

**`email` / `email_verified` are non-optional — DECIDED (product owner, 2026-07-25), on both paths.** This is a change from the earlier draft, where `email` was `Option<String>` and `email_verified` carried `#[serde(default)]`. Both are now plain required fields. Consequences, stated precisely because plans 08/09 bind to this shape:

- **utoipa marks both `required` in the generated `ClaimAdminIdentityRequest` schema**, because neither is an `Option` and neither has a serde default. This is the machine-readable form of the decision, and it is what 08/09 must generate their client against. See the loud propagation callout in Interfaces & Contracts.
- **Dropping `#[serde(default)]` from `email_verified` is load-bearing, not cosmetic.** With the default, a body that simply omitted the field silently deserialized to `false` and then failed the verified-email check with a *misleading* 403 (`admin_claim_email_not_verified`) — telling the caller their email is unverified when in fact they never sent the field. Without it, an omitted field is a schema violation, which is what it actually is.
- **Missing-field rejection must still carry an i18n key (CONVENTIONS §4).** Making a field required moves the "omitted" case from service validation into the `axum::Json` extractor, and axum's default `JsonRejection` renders a plain-text body with **no** `ErrorResponse` envelope, no `code`, and no `message_key`. Every existing admin handler takes a bare `Json<T>` (`src/http/admin.rs:112,188,333,…`) and therefore already has this gap — it is pre-existing and repo-wide, **not** something this plan may inherit for a brand-new endpoint. Module 11 therefore extracts `Result<Json<ClaimAdminIdentityRequest>, JsonRejection>` and maps the rejection itself; see there for the exact code and status.
- **`AdminIdentityRecord.email` correspondingly becomes `String`** (not `Option<String>`), since no grant can now be created without one, and `admin_identities.email` is populated on every insert. The migration column stays `varchar(320)` **nullable** — do not tighten it to `not null`, because `0009` is append-only and a future revoke/anonymisation path may need to clear it; the *application* invariant is enforced at the service, and a test asserts every row this plan writes has a non-null email.

- `ResponseText` already exists (`src/domain/i18n.rs:12-18`, `{ message_key, message, message_args }`) and is already re-exported (`src/domain/mod.rs:36`) and registered with utoipa (`src/http/openapi.rs:20,40`). **Reuse it; do not invent a parallel notice shape.**
- `deny_unknown_fields` on every request DTO, matching `CredentialCreateRequest`/`TrustedJwtIssuerCreateRequest` (`src/domain/admin.rs:424-452`, `:562-605`).
- Register via a new `pub use identity::{...}` block in `src/domain/mod.rs` (alphabetically ordered, per the file's convention). **Do not fold these into `src/domain/admin.rs`** — that file's `PageQuery`/`SetupStatusResponse` are a different concept (see Findings).

### Module 3 — `src/domain/auth_settings.rs` (new)

**Decision `D7`: these DTOs carry no secret material of any kind.** There is no `client_secret` field on any request, no envelope fields on the record, no `secret_fingerprint`, no `masked_secret` — so, unlike `CredentialRecord` (`src/domain/admin.rs:385-422`), **no `#[serde(skip_serializing)]` / `#[schema(ignore)]` hiding is needed here**, because there is nothing to hide. Every field below is non-secret configuration and is serialized normally. *(`CredentialRecord`'s hiding pattern is still correct and still required for provider credentials — the AI-provider API keys — which D7 does not touch. Do not delete it there; just do not copy it here, where it would only imply a secret exists.)*

```rust
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AuthMethod { GoogleOauth, GenericOidc, Jwks }

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AuthProviderSettingsRecord {
    pub id: Uuid,
    pub method: AuthMethod,
    pub display_name: String,
    pub enabled: bool,
    pub issuer: Option<String>,
    pub discovery_url: Option<String>,
    pub authorization_url: Option<String>,
    pub token_url: Option<String>,
    pub userinfo_url: Option<String>,
    pub jwks_url: Option<String>,
    /// Non-secret. Always returned. This is the value the console fingerprints
    /// and compares against its own stored fingerprint for D7 drift protection.
    pub client_id: Option<String>,
    pub requested_scopes: Vec<String>,
    pub allowed_email_domains: Vec<String>,
    pub allowed_algorithms: Vec<String>,
    pub expected_audiences: Vec<String>,
    pub redirect_uris: Vec<String>,
    pub trusted_jwt_issuer_id: Option<Uuid>,
    pub metadata: Value,
    pub status: ResourceStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub version: i64,
}
```

Plus `AuthProviderSettingsCreateRequest` (same fields minus server-managed ones) and `AuthProviderSettingsPatchRequest` (all `Option<_>`). **Neither carries a `client_secret` field, and there is no `RotateAuthProviderSecretRequest` type** — decision **D7**. Both request DTOs are `deny_unknown_fields`, which means a console still sending `client_secret` is **rejected loudly with a schema error rather than silently accepted and dropped** — that is deliberate, and it is what makes a stale 08 client fail fast instead of believing Moira stored a secret it never stored. Then the bootstrap-read shapes:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SetupAuthMethodsResponse { pub methods: Vec<PublicAuthMethod> }

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PublicAuthMethod {
    pub id: Uuid,
    pub method: AuthMethod,
    pub display_name: String,
    pub issuer: Option<String>,
    pub discovery_url: Option<String>,
    pub authorization_url: Option<String>,
    pub jwks_url: Option<String>,
    /// Non-secret. The D7 drift-protection anchor: the console reads this on
    /// boot and compares its fingerprint against the one stored beside its own
    /// client secret. Sufficient on its own — plan 08 needs nothing more.
    pub client_id: Option<String>,
    pub requested_scopes: Vec<String>,
    pub allowed_email_domains: Vec<String>,
}
```

`PublicAuthMethod` is a **deliberately narrower projection** than the full record — it must never gain a field carrying secret material, and a test asserts that (`public_auth_method_never_exposes_secret_fields`). Under D7 no such field exists on the source record either, so the test is now a *forward* guard against reintroduction rather than a redaction check.

### Module 4 — `src/security/crypto.rs`: **no change** (decision D7)

**This module is intentionally empty.** Earlier drafts added an `AuthProviderSecretAadParts` / `auth_provider_secret_aad` sibling beside `CredentialAadParts` / `credential_aad` (`:84-111`) to bind an auth-provider client secret to its `(id, method, issuer, client_id, encryption_version)`. **Decision `D7` deletes that work entirely**: Moira stores no OAuth client secret, so there is no envelope, no AAD, and nothing to bind. `src/security/crypto.rs` and `src/security/mod.rs` are **untouched by this plan**.

Two things follow that an implementer must not get wrong:

- **`credential_aad` / `CredentialAadParts` stay exactly as they are.** They govern **provider credentials** — the AI-provider API keys in `provider_credentials` — which D7 does not touch and which remain encrypted in Moira with `SecretCipher` + AAD, write-only, with no read-back path. Do not remove, generalise, or "share" them.
- **A whole class of hazard disappears with the AAD.** Because `issuer` and `client_id` were bound into the deleted AAD, changing either via `PATCH` used to invalidate the stored secret, which forced a rebind-or-rotate rule and a `409 auth_provider_secret_rebind_required`. **That hazard no longer exists on the Moira side**: `issuer` and `client_id` are now ordinary mutable configuration fields, freely patchable under the normal `If-Match` rules, with no secret bound to them. The corresponding error code, its i18n entry, and its tests are removed. *(The console still has its own reason to care that `client_id` changed — its stored secret belongs to a specific client — but that is the drift-protection contract in Security boundaries, handled by fingerprint comparison in plan 08, not by an error from Moira.)*

### Module 5 — `src/infra/repositories/identity.rs` (new)

`#[async_trait] pub trait AdminIdentityRepository` + `PgAdminIdentityRepository`, mirroring `AdminRepository`'s shape (`src/infra/repositories/admin.rs:60-234`). Methods:

- `async fn find_active_grant(&self, issuer: &str, subject: &str) -> Result<Option<AdminIdentityGrant>, AppError>` — a **plain `fetch_optional` against the pool**, not a transactional wrapper with advisory locks. This is the hot path of *every* trusted-JWT request (module 7) and must not open a write transaction. Backed by `admin_identities_lookup_idx`.
- `async fn insert_grant(&mut self, …) -> Result<AdminIdentityGrant, AppError>` — transactional; relies on `admin_identities_issuer_subject_active_unique` as the final backstop and maps a unique violation to `AppError::conflict("admin_identity_already_claimed", …)` (`AppError::conflict` verified at `src/error.rs:86-88`).
- `async fn setup_state(&self) -> Result<bool, AppError>` — reads `setup_state.claimed`.
- `async fn mark_setup_claimed(&mut self, admin_identity_id: Uuid) -> Result<(), AppError>` — `update setup_state set claimed = true, claimed_admin_identity_id = $1, claimed_at = now(), updated_at = now() where id = true and claimed = false`. The `and claimed = false` guard makes it self-idempotent.
- `async fn resolve_active_issuer(&mut self, issuer: &str) -> Result<Uuid, AppError>` — looks up `trusted_jwt_issuers` by `issuer` with `status = 'active' and deleted_at is null`; returns `AppError::coded(StatusCode::BAD_REQUEST, "unregistered_trusted_issuer", …)` otherwise. **Must use `AppError::coded`, not `AppError::BadRequest`** — the latter derives code `bad_request` (`src/error.rs:130`), which would lose the specific key 08 needs.
- `async fn insert_setup_token(&mut self, …) -> Result<(), AppError>`
- `async fn consume_setup_token(&mut self, prefix: &str, …) -> Result<ConsumedSetupToken, AppError>` — lookup by `token_prefix` (mirroring `verify_api_key`'s prefix-then-Argon2-verify flow, `src/security/auth.rs:226-290`), then `update admin_setup_tokens set consumed_at = now() where id = $1 and consumed_at is null and expires_at > now() returning …`. The `and consumed_at is null` guard at the **database** level is what makes it genuinely one-time. **Do not reuse `idempotency_records`** — that ledger's whole purpose is replay-safety, the opposite guarantee.

### Module 6 — `src/infra/repositories/auth_settings.rs` (new)

`#[async_trait] pub trait AuthProviderSettingsRepository` + `PgAuthProviderSettingsRepository`. Methods: `create(id, &request)`, `list(limit)`, `get(id)`, `patch(id, expected_version, &request)` (with the `select version … for update` optimistic-lock check taken from `rotate_credential`'s shape at `admin.rs:392-403` — see module 12 on why the check goes *inside* the transaction), `set_status(id, status)`, `set_enabled(id, expected_version, enabled)`, `soft_delete(id, expected_version)`, and `list_enabled_public()` for the bootstrap read.

**There is no `rotate_secret` method and no `EncryptedSecret` parameter anywhere in this repository** — decision **D7**. The INSERT/SELECT column lists contain **only** the non-secret columns of `0010`; there is no envelope mapping to write, which is the reason this repository is materially simpler than the credential repository it was once modelled on.

**There is no `load_secret`-style method, because there is no secret on this table** — and consequently **no decrypt path exists anywhere in this plan**. If 08 needs an OAuth client secret server-side, it reads it from its **own** `console_auth` database, never from Moira. Adding a secret column plus a read-back here would break the invariant D7 exists to preserve; it is not a follow-up, it is a prohibition.

### Module 7 — `src/security/auth.rs` extension (the highest-risk diff)

All `auth.rs` line numbers below were verified against the **pre-03** tree: `authenticate_admin:126-169` (bearer branch `:143-145`), `authenticate_caller:171-224`, `verify_api_key:226-290`, `authenticate_trusted_jwt:292-343` (final expression `actor_from_trusted_claims(&issuer_config, claims)` at `:342`), `actor_from_trusted_claims:555-628`, `header_string:815`, header literals `"x-moira-system-key"` at `:132,178`. **Plan 03 hardens this exact file first — re-locate every cite against the post-03 state before editing (Wave 0's job) and re-confirm 03 did not change these functions' shapes.**

**7a — grant application.** Change `authenticate_trusted_jwt`'s final expression to:
```rust
let actor = actor_from_trusted_claims(&issuer_config, claims)?;
apply_admin_identity_grant(pool, &issuer_config.issuer, actor).await
```
New private `async fn apply_admin_identity_grant(pool: &PgPool, issuer: &str, mut actor: Actor) -> Result<Actor, AppError>`:
- `actor.subject` is already guaranteed `Some` (`actor_from_trusted_claims` errors when the subject claim is missing) and `actor.trusted_jwt_issuer_id` is always set — assert both, do not re-derive.
- **Thread `issuer_config.issuer` through as a parameter** rather than re-querying `trusted_jwt_issuers` by id; the string is already in scope and a second round-trip on every authenticated request is unacceptable.
- Run the module-5 lightweight `find_active_grant(issuer, subject)`.
- If found: `actor.scopes.extend(grant.granted_scopes)` then dedup+sort, **mirroring the existing dedup at `auth.rs:566-567`**. This is a **union**, not a replace, so a trusted issuer that also grants narrower JWT-claimed scopes does not lose them.
- If not found: return `actor` **unchanged** — the byte-identical no-op path.
- ⚠️ **SUPERSEDED BY §0.2 D2 — read that before implementing this bullet.** The grant is applied in
  **`authenticate_admin` (`src/security/auth.rs:308`), to the actor returned by
  `authenticate_trusted_jwt`** — *not* inside `authenticate_trusted_jwt` itself, which is what this
  bullet's "reachable from `authenticate_trusted_jwt` only" phrasing implies.

  The distinction is load-bearing and this bullet gets it wrong. `authenticate_admin` (`:308`) and
  `authenticate_caller` (`:353`) **both** call `authenticate_trusted_jwt` (`:474`), and
  `authenticate_caller` returns its actor verbatim for a bare bearer token (`:386-388`). A grant
  injected at the shared function therefore lands on the **public execution API**, where — combined
  with admin implication — it satisfies `moira:execution:override-credential`, `override-model` and
  `moira:identity:delegate` for any granted human sending `POST /api/v1/responses` with nothing but
  their bearer token.

  This bullet's own reviewer checklist inspects the dev-trust-header branch, which is genuinely
  unreachable (`actor_from_trusted_headers` bypasses `authenticate_trusted_jwt` entirely), and misses
  the branch that matters. Admin-identity grants apply exclusively to the **admin plane**
  (`plans/01` §4.3 Mode A); `combine_consumer_and_jwt` (`:941`) already strips `moira:admin` on the
  consumer+JWT path, so this is the direction the existing code already goes.

**7b — `verify_system_key_only`.** A small new `pub(crate)` method on `AuthService` wrapping the existing private `verify_api_key` (`:226`, called today as `verify_api_key(pool, "system_api_keys", raw_key)` at `:148`) that reads **only** the `x-moira-system-key` header and never a bearer JWT. This is the structural enforcement of "no first-login-wins."

**7c — `ActorType::SetupToken`.** ⚠️ **CUT — see §0.2 D1.** The setup-token credential path is
deferred, so no new `ActorType` variant is added and this sub-module is not implemented.

Two things were wrong with it independently of the cut, recorded so neither is reintroduced if the
setup-token path ever returns:

1. **The "two `ActorType` matches in `src/security/authz.rs` (`:119`, `:146`)" do not exist and never
   did.** The old code tested a single negated equality (`actor.actor_type != ActorType::ConsumerKey`),
   not a match. Commit `8039c53` replaced it with an explicit allow-list, `ADMIN_IMPLYING_ACTOR_TYPES`
   (`src/security/authz.rs:129-133`).
2. **Under the old form, following this instruction would have produced the opposite of its intent.**
   `!= ConsumerKey` grants admin implication to every variant *except* `ConsumerKey`, so a newly added
   `SetupToken` would have inherited **full admin authority** — silently, with no match to make
   non-exhaustive and therefore no compiler warning. That fail-open default is why `8039c53` exists;
   its doc comment names this plan (`src/security/authz.rs:124-125`).

Were the variant added today, the correct action in `authz.rs` would be **none**: absence from the
allow-list *is* denial, which is the safe direction to be wrong in.

**7d — no self-asserted scopes (CONVENTIONS §7.5).** Add a validation, invoked from the auth-settings service (module 9), that any `trusted_jwt_issuers` row linked from `auth_provider_settings.trusted_jwt_issuer_id` has `scopes_claim IS NULL`. Reject with `AppError::coded(StatusCode::BAD_REQUEST, "console_issuer_must_not_assert_scopes", …)`.

### Module 8 — `src/application/identity.rs` (new) — `AdminIdentityService`

- `pub fn new(state: &AppState) -> Result<Self, AppError>` — mirrors `SetupService::new`/`RuntimeAdminService::new` (`src/application/runtime_admin.rs:27-35`: `let pool = state.pool()?.clone();`).
- `pub async fn claim_status(&self) -> Result<SetupClaimStatusResponse, AppError>` — **takes no `Actor` and performs no authz check.** This is the one handler in Moira's admin surface that deliberately has no actor. Document that in a doc comment so a future reviewer does not "fix" it: an unauthenticated setup wizard must be able to ask "do I need to show the claim flow?" *before* any human has credentials to present. Returns `{ claimed }` and **nothing else**.
- `pub async fn claim(&self, ctx: &RequestContext, credential: ClaimCredential, request: ClaimAdminIdentityRequest) -> Result<(AdminIdentityRecord, bool), AppError>`, where `ClaimCredential` is an enum `{ SystemKey(Actor), SetupToken(String) }` constructed by the HTTP handler (module 11), and the `bool` is `replayed` so the handler can map replay → 200 and fresh → 201.

  Order of operations — **validate before entering the transactional envelope**, matching the existing convention, so a policy-rejected request never takes the advisory lock or writes an idempotency record for a request that was never going to succeed:
  1. If `SetupToken(raw)`: consume it (module 5, single-use). If the token was minted with a pre-bound `target_issuer`/`target_subject`, assert the request's `(issuer, subject)` matches **exactly** — a setup token can never be replayed to claim a different identity than the one it was issued for.
  2. If `SystemKey(actor)`: `state.authz.require(&actor, "moira:admin")?`.
  3. Validate every requested scope is a member of `ADMIN_SCOPES` (`src/security/authz.rs:8-91`); reject unknown scope strings with the **existing** `scope_invalid` code (400) — do not mint a new key for a condition the catalog already covers.
  4. Resolve the target issuer (`resolve_active_issuer`) → `unregistered_trusted_issuer` (400) if absent.
  5. Enforce verified-email + allowed-domain policy (module 10).
  6. Enter the envelope: `admin_command_spec(ctx, actor_or_synthetic, "admin_identity.claim", json!({ "issuer": …, "subject": … }), &request)?` → `AdminCommandRunner::new(self.repo.clone(), command_hasher(self.state)).execute(spec, |transaction| Box::pin(async move { … }))`. **Reuse the envelope verbatim** — do not hand-roll the advisory-lock/savepoint/finalize sequence. Inside: `insert_grant`, `mark_setup_claimed`, `insert_audit(success_audit(actor, ctx, "admin_identity.claim", "admin_identity", Some(id), json!({…})))`, then `AdminCommandMutation::new(record, 201, Some(record.id.to_string()))?`.

  ⚠️ **Three corrections to the line above, all of which were compile errors as originally written
  (§0.1 B2, B7, B8):**
  - `AdminCommandRunner::new` takes **two** arguments — the repository *and* an `IdempotencyHasher`
    (`src/application/admin_command.rs:169`). Every real call site passes
    `command_hasher(self.state)`; see `src/application/admin/applications.rs:49-51` for the shape.
  - `AdminCommandMutation::new` returns `Result<Self, AppError>`
    (`src/application/admin_command.rs:136`), so it needs the `?` — it is not a value.
  - **`src/application/admin.rs` no longer exists.** Plan 06 split it into `src/application/admin/`.
    `admin_command_spec` (`admin/shared.rs:383`), `success_audit` (`:402`) and `command_hasher`
    (`:163`) are `pub(crate)` there; `AdminCommandRunner` and `AdminCommandMutation` remain in
    `src/application/admin_command.rs`. The closure receives a `&mut PgAdminCommandTransaction`
    (`src/infra/repositories/admin.rs:34`). The advisory-lock/savepoint/finalize primitives are
    `claim_idempotency` (`:651`), `begin_command_savepoint` (`:738`) and `finalize_idempotency`
    (`:759`) — not the `:559-687` range this plan cites.
  7. On unique violation from `insert_grant`: `admin_identity_already_claimed` (409). This is the DB-level backstop that holds even if the advisory-lock window is somehow raced.

  **Consequence of the resolved decisions (module 10): step 5 now runs identically on both credential paths, and it can deny a system-key claim.** Two things follow that an implementer must get right. First, do **not** add a `matches!(credential, ClaimCredential::SystemKey(_))` short-circuit around step 5 "so bootstrap works" — that is exactly the bypass module 10 rules out. Second, note the ordering interaction on the setup-token path: step 1 consumes the token *before* step 5 can deny the claim, so a policy-denied token claim burns the token. That is the correct trade-off (the DB-level `consumed_at is null` guard is what makes the token genuinely single-use, and weakening it to "consume only on success" would reintroduce replayability), but it makes it operationally important that the operator configures `allowed_email_domains` **before** minting a setup token — call that out in the operator runbook alongside module 10's ordering guidance.

  **Replay semantics note that falls out of using the shared envelope:** on an `Idempotency-Key` replay, the stored response body is returned verbatim. The `notice` field therefore carries the same `moira.notice.admin_identity_claimed` key on both 201 and 200 — the **status code**, not the notice, distinguishes fresh from replayed. Do not add a second "replayed" notice key; it could never be emitted.

  **Audit fidelity:** the `granted_by_actor_type` column records `'system_key'` or `'setup_token'` honestly. For the setup-token path, construct a synthetic `Actor { actor_type: ActorType::SetupToken, subject: Some(token_prefix), .. }` for the command spec's actor fingerprint and the audit row — **never** claim `ActorType::SystemKey` for a token-path claim.

- `pub async fn mint_setup_token(&self, actor: &Actor, ctx: &RequestContext, target: Option<(String, String)>, ttl_seconds: Option<i64>) -> Result<GeneratedSetupTokenResponse, AppError>` — requires `actor.actor_type == ActorType::SystemKey` (an `ActorType` check, mirroring how `require_setup_actor` restricts by type and not only by scope) **and** `moira:admin`. Generates via the existing `ApiKeyHasher` (`src/security/api_keys.rs:41-56`, `generate(namespace: &str)` producing `format!("{namespace}_{}", URL_SAFE_NO_PAD.encode(bytes))` over 32 `OsRng` bytes, with Argon2id+pepper hashing at `:58-64`, prefix `:74-76`, fingerprint `:78-80`) with namespace `"moira_setup"`, stored in `admin_setup_tokens`. **Reuse `ApiKeyHasher`; do not write a second hashing implementation.** TTL defaults to `AuthSettings.setup_token_ttl_seconds` (900).

### Module 9 — `src/application/auth_settings.rs` (new) — `AuthProviderSettingsService`

Follows `src/application/admin.rs`'s `create_credential` shape (`:483-544`) for the transactional envelope — **not** `runtime_admin.rs`'s two-phase non-transactional scheme, because this surface needs version-conditional mutation in one transaction. It does **not** follow `rotate_credential`, because under decision **D7** there is no secret to encrypt or rotate; only the in-transaction `select … for update` version-check technique is borrowed from it.

- `create(actor, ctx, request)` — `authz.require(actor, "moira:auth-settings:write")`; validate method shape (jwks ⇒ `jwks_url` present, no `client_id`; oauth methods ⇒ `client_id` present and one of `issuer`/`discovery_url`); validate `allowed_email_domains` entries are syntactically plausible domains; validate URL schemes are `https` (defer any *fetch* to plan 03's SSRF-hardened fetcher — do not fetch here if 03 has not landed); if `trusted_jwt_issuer_id` is set, enforce module 7d's `scopes_claim IS NULL` rule; run inside `AdminCommandRunner` with `admin_command_spec(ctx, actor, "auth_provider.create", …)`; insert audit; `AdminCommandMutation::new(record, 201, Some(id))`; on success, invalidate the auth-settings cache (the NOTIFY trigger handles cross-instance; the local invalidation mirrors `schedule_runtime_cache_invalidation`'s pattern). **No `client_secret` is accepted; the DTO has no such field and `deny_unknown_fields` rejects one that is sent.**
- `patch(actor, ctx, id, expected_version, request)` — `If-Match` required. **`issuer` and `client_id` are freely patchable** — there is no secret bound to them (**D7** removed the AAD that once made a change invalidating, along with the `auth_provider_secret_rebind_required` 409). The console's stale-secret concern is handled console-side by the `client_id` fingerprint comparison described in Security boundaries.
- `set_enabled(actor, ctx, id, expected_version, enabled)` — enabling a method whose **non-secret** configuration is incomplete (e.g. `generic_oidc` with neither `issuer` nor `discovery_url`, or `jwks` with no `jwks_url`) is rejected with `auth_provider_method_config_incomplete` (400). **Moira cannot and must not check for a client secret here — it does not have one** (D7); whether the console holds a usable secret is the console's precondition, enforced by its own wizard. **Deny-by-default: `enabled` defaults to `false` on create**, so a half-configured method can never be live by accident.
- `delete(actor, ctx, id, expected_version)` — soft delete; `moira:auth-settings:delete`.
- `list(actor, query)` / `get(actor, id)` — `moira:auth-settings:read`.
- `setup_auth_methods(actor)` — **authenticated, decided (see Security boundaries): the method takes an `Actor` and is not callable without one.** `require_setup_actor`-equivalent gating (`ActorType::SystemKey | TrustedJwt`) plus `moira:setup:read`; returns `SetupAuthMethodsResponse` built from `list_enabled_public()`. **Projects to `PublicAuthMethod` explicitly, field by field** — never `..record` spread, so a future field addition cannot silently widen this response. Do not add an anonymous overload, an `Option<Actor>` parameter, or a "public" sibling method; the signature taking a non-optional `Actor` is itself the enforcement, and `setup_auth_methods_requires_a_setup_actor_and_rejects_anonymous` / `setup_auth_methods_rejects_an_unauthenticated_call_with_401` are the tests that hold it.

### Module 10 — Verified-email + allowed-domain policy (DB-backed, deny-by-default)

**Changed from the earlier draft, per CONVENTIONS §7.2.** The allowed-domain policy is **runtime, DB-backed configuration** on `auth_provider_settings.allowed_email_domains` — *not* a `src/config/settings.rs` field and *not* an environment variable. §7.2 names "allowed email domains" explicitly among the non-secret config the settings table stores, and putting it in env would make it impossible for 08's setup wizard to configure. The earlier `MOIRA_AUTH__ADMIN_CLAIM_ALLOWED_EMAIL_DOMAINS` design is withdrawn.

**Two product-owner decisions are RESOLVED here (2026-07-25) and are no longer open:**

- **Deny-by-default is confirmed.** An unconfigured (empty) `allowed_email_domains` **denies every claim**. The operator **MUST** configure at least one allowed domain before the first claim can succeed.
- **`email` + `email_verified` are required on BOTH credential paths** — system key and setup token alike. The earlier "email optional on the system-key path" carve-out is **withdrawn**.

**There is NO first-claim exemption and NO bootstrap bypass — explicitly ruled out.** No "the very first claim is allowed through", no "the system-key path skips the domain check", no "empty allow-list means allow all", no env-var escape hatch, no build-flag. If a future reader finds themselves adding one because a fresh deployment "can't get started", the correct answer is that the operator configures `allowed_email_domains` first — that is the designed setup order, not a defect. A bypass would exist precisely during the setup window, when the deployment is least defended and most attractive, which is the worst possible time to have one; and it would be indistinguishable, from the outside, from the "first-login-wins" land-grab this whole plan exists to make structurally impossible. Any patch reintroducing one is a **security regression to reject in review**, and `claim_is_denied_by_default_when_no_domain_allow_list_is_configured` (Verification) is the test that catches it.

Evaluation, inside `AdminIdentityService::claim`, after issuer resolution and before the transactional envelope. Steps 2-5 run **identically on both credential paths** — the path affects only who is authorised to submit the claim, never which policy is applied to it:

1. **Resolve the governing policy row.** Find the `auth_provider_settings` row governing the target issuer (matched on `auth_provider_settings.issuer = request.issuer`, or via `trusted_jwt_issuer_id` → the resolved issuer row). **If no enabled row governs the issuer, the claim is denied on every path** — `admin_claim_domain_not_allowed` (403). "No governing configuration" is a *stricter* case of "no allowed domains", and it resolves the same way. This is the change that closes the earlier draft's system-key carve-out; a system key authorises you to *submit* a claim, it does not exempt you from *policy*.
2. **Verified email.** `request.email_verified` **must** be `true`, else `admin_claim_email_not_verified` (403). Hard requirement, not configurable, both paths.
3. **Email presence — required on both paths.** `email` is a required field of the DTO (module 2), so a body omitting it never reaches this step; it is rejected by the extractor (module 11) with a coded, catalogued error. This step catches what the type system cannot: a present-but-empty or whitespace-only string, or a value with no `@` and therefore no extractable domain → `admin_claim_email_required` (400). **The system-key path gets no exemption.** Rationale, recorded because it is the reason the earlier carve-out was withdrawn: (a) the deny-by-default domain policy is only a policy if it is enforceable on *every* claim path, and an email-less system-key claim would have had no domain to check — a silent bypass hiding inside a "convenience"; (b) every `admin_identities` grant now carries a human-identifiable audit attribute, so an audit reader can answer "which human holds this grant?" from the grant row alone rather than inferring it from an opaque `(issuer, subject)` pair.
4. **Domain allow-list — deny by default, both paths.** The email's domain must appear in the governing row's `allowed_email_domains`. An **empty** array means **deny all**: there is no "empty means unrestricted" reading. Every claim must match an explicit entry. `plans/01` §4.7 recommends deny-by-default; CONVENTIONS §7.5 makes it binding ("email/domain allow-list is **deny-by-default**") and §8's auth-touching checklist requires it. Document it prominently in the endpoint's OpenAPI description (module 11) **and** in the operator-facing setup documentation (below) so a fresh deployment's first 403 reads as the designed setup order, not as a bug.
5. Domain comparison is case-insensitive, on the substring after the **last** `@`, with no wildcard/subdomain matching in this plan (an exact match on `example.com` does **not** admit `sub.example.com`). Wildcards are a deferred follow-up; silently supporting them would be a policy hole.

#### Operator-facing documentation (deliverable of this module, not optional)

Because a correctly-configured fresh deployment refuses its very first claim until domains are configured, this plan **must** ship operator copy saying so, or the decision will be re-opened as a bug report the first time someone deploys. Three surfaces, all in this PR:

- **`POST /api/v1/admin/setup/claim` OpenAPI description** — state that the domain allow-list is deny-by-default, that an unconfigured or empty `allowed_email_domains` refuses every claim on every credential path, that there is no first-claim exemption, and name the endpoint that fixes it (`POST /api/v1/admin/auth/providers`, then `POST /api/v1/admin/auth/providers/{id}/enable`).
- **`moira.error.admin_claim_domain_not_allowed`'s catalog `description`** — must read as a setup instruction, not just a denial, so the console can surface something actionable. Keep the `default_message` short and user-facing; put the "configure allowed domains first" guidance in `description` (which is documentation for implementers and console authors, per the catalog's own shape).
- **The setup runbook in `docs/`** — document the required ordering explicitly: *(1)* bootstrap the system key (`bootstrap-system-key`, unchanged); *(2)* register the trusted JWT issuer; *(3)* create the `auth_provider_settings` row **including `allowed_email_domains`** and enable it; *(4)* only then `POST …/setup/claim`. Add this beside the existing setup material rather than minting a new top-level document, and add the `docs/` row to Components & ownership when the exact file is chosen at Wave 0.

The named e2e test `claim_is_denied_by_default_when_no_domain_allow_list_is_configured` and its no-governing-row sibling are what make this section verifiable rather than aspirational.

`AuthSettings.setup_token_ttl_seconds` (default 900) is the **one** new `src/config/settings.rs` field this plan adds — it is an infrastructure knob, not auth-method config, so it correctly stays in settings. Env: `MOIRA_AUTH__SETUP_TOKEN_TTL_SECONDS` (prefix/separator verified at `src/config/settings.rs:374-382`: `Environment::with_prefix("MOIRA").prefix_separator("_").separator("__")`; `AuthSettings` at `:89-95` currently holds only `admin` and `caller` sub-structs, so this is the first scalar on it).

### Module 11 — `src/http/identity.rs` (new)

```rust
#[utoipa::path(
    get, path = "/api/v1/admin/setup/claim-status", tag = "admin-setup",
    responses(
        (status = 200, description = "Whether an admin identity has been claimed. Intentionally returns a single boolean and nothing else — no count, timestamp, issuer, or subject — so that an unauthenticated caller learns nothing about the deployment beyond whether the setup wizard should be shown.", body = SetupClaimStatusResponse),
        (status = 503, description = "PostgreSQL is unavailable", body = ErrorResponse)
    )
)]
pub async fn get_setup_claim_status(
    State(state): State<AppState>,
) -> Result<Json<SetupClaimStatusResponse>, AppError> {
    AdminIdentityService::new(&state)?.claim_status().await.map(Json)
}
```
No `security(...)` annotation, no `HeaderMap` parameter, no `Actor` — the shape itself is the documentation.

```rust
#[utoipa::path(
    post, path = "/api/v1/admin/setup/claim", tag = "admin-setup",
    request_body = ClaimAdminIdentityRequest,
    description = "Grants Moira admin scope to a specific (issuer, subject). `email` and `email_verified` are REQUIRED on both credential paths. The email domain allow-list is DENY-BY-DEFAULT: a claim is refused 403 unless an enabled auth-provider configuration governs the target issuer AND its `allowed_email_domains` explicitly contains the email's domain. An unconfigured or empty allow-list denies every claim, including the first one — there is no first-claim exemption and no bootstrap bypass, on either the system-key or the setup-token path. Configure the policy first via POST /api/v1/admin/auth/providers, then POST /api/v1/admin/auth/providers/{id}/enable.",
    responses(
        (status = 201, description = "Admin identity granted", body = AdminIdentityRecord),
        (status = 200, description = "Idempotent replay of a prior successful claim", body = AdminIdentityRecord),
        (status = 400, description = "unregistered_trusted_issuer, scope_invalid, admin_claim_email_required, invalid_request (malformed body)", body = ErrorResponse),
        (status = 401, description = "setup_claim_credential_required, setup_token_invalid, setup_token_expired, setup_token_consumed", body = ErrorResponse),
        (status = 403, description = "admin_claim_email_not_verified, admin_claim_domain_not_allowed, setup_token_target_mismatch", body = ErrorResponse),
        (status = 409, description = "admin_identity_already_claimed, idempotency_conflict, idempotency_in_progress", body = ErrorResponse),
        (status = 422, description = "invalid_request — the body is well-formed JSON but violates the schema, e.g. `email` or `email_verified` omitted", body = ErrorResponse),
        (status = 503, description = "PostgreSQL is unavailable", body = ErrorResponse)
    ),
    params(("Idempotency-Key" = Option<String>, Header, description = "Supply for safe automatic retry of a claim in flight. Without it, retrying an already-succeeded claim returns 409, not a replay.")),
    security(("systemKeyAuth" = []))
)]
pub async fn claim_admin_identity(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<ClaimAdminIdentityRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<AdminIdentityRecord>), AppError> {
    let Json(request) = body.map_err(claim_body_rejection)?;
    let credential = resolve_claim_credential(&state, &headers, &request).await?;
    let ctx = RequestContext::from_headers(&headers);
    let (record, replayed) = AdminIdentityService::new(&state)?
        .claim(&ctx, credential, request)
        .await?;
    Ok((if replayed { StatusCode::OK } else { StatusCode::CREATED }, Json(record)))
}

async fn resolve_claim_credential(
    state: &AppState,
    headers: &HeaderMap,
    request: &ClaimAdminIdentityRequest,
) -> Result<ClaimCredential, AppError> {
    if let Some(raw_key) = header_string(headers, "x-moira-system-key") {
        let actor = state.auth.verify_system_key_only(state.pool()?, &raw_key).await?;
        return Ok(ClaimCredential::SystemKey(actor));
    }
    if let Some(token) = request.setup_token.clone() {
        return Ok(ClaimCredential::SetupToken(token));
    }
    Err(AppError::coded(
        StatusCode::UNAUTHORIZED,
        "setup_claim_credential_required",
        "a system key or one-time setup token is required to claim an admin identity",
    ))
}

/// Keeps a schema-violating body inside Moira's `ErrorResponse` envelope
/// (CONVENTIONS §4) instead of axum's bare plain-text rejection.
fn claim_body_rejection(rejection: JsonRejection) -> AppError {
    AppError::coded(
        rejection.status(),
        "invalid_request",
        "the claim request body is malformed or does not match the required schema",
    )
}
```

- `RequestContext::from_headers` already exists (`src/application/context.rs:15-32`, reading `idempotency-key` at `:30`, `x-request-id` at `:17`, client IP at `:19-28`) — reuse it; do not construct a context inline.
- `header_string` already exists (`src/security/auth.rs:815-820`); expose it or mirror it rather than re-implementing header parsing.
- **The handler must never fall through to `state.auth.authenticate_admin`.** That method accepts a bearer JWT (`auth.rs:143-145`) and is exactly what this endpoint must refuse.
- **Why `Result<Json<…>, JsonRejection>` rather than a bare `Json<…>`** (module 2's consequence): with `email`/`email_verified` now required, an omitted field is rejected by the extractor before the handler body runs. A bare `Json<T>` would emit axum's default plain-text 422 with no `code` and no `message_key`, violating CONVENTIONS §4 on a brand-new endpoint. `claim_body_rejection` maps every `axum::extract::rejection::JsonRejection` variant onto `AppError::coded(rejection.status(), "invalid_request", …)`, which preserves axum's own status distinction (400 for malformed JSON / missing content-type, 422 for well-formed JSON that violates the schema) while guaranteeing the `ErrorResponse` envelope and `moira.error.invalid_request` on both. **`invalid_request` is an existing catalog key** (`src/i18n/catalog/errors.rs:35`, "The request is invalid.") — verified present, so this adds no new key; it is a **reuse**, listed under Reused existing keys in the i18n section.
- **Scope discipline:** apply this rejection mapping to **this handler only**. Every pre-existing admin handler takes a bare `Json<T>` and has the same §4 gap; fixing them all is a repo-wide change and would violate this plan's "pure iteration" constraint (`plans/01` §1.2). Note the pre-existing gap in the PR as a deferred follow-up so it is visible and separately trackable.
- **`resolve_claim_credential` sees a fully-typed request**, so it can rely on `request.email` being a `String`. It must still **not** perform policy evaluation — credential resolution answers "may this caller submit a claim at all", and module 10's policy answers "may this claim succeed". Keeping them separate is what guarantees the policy runs on both paths.
- Register both routes in `documented_router()` adjacent to the existing `.routes(routes!(admin::get_setup_status))` line. Tag both `admin-setup` (matching the existing structural endpoint) with distinct `operationId`s.

### Module 12 — `src/http/auth_settings.rs` (new)

Eight handlers (`get_setup_auth_methods` plus the seven provider operations — **there is no `rotate_secret` handler**, decision **D7**). Follow the trusted-JWT-issuer handlers as the template (`src/http/admin.rs:1335-1537`) — same `security((...))` triple, same `params(PageQuery)` on list, same `etag_headers` (`:58-64`) / `require_if_match` (`:66-83`) helpers, same `Idempotency-Key` documentation on create.

⚠️ **`ensure_version` no longer exists** (§0.1 B4). Plan 06b deleted it; `src/http/admin.rs:85-90` is
now `optional_if_match`. Referencing it is a compile error.

**The "known defect to not copy" is gone — plan 06b fixed it** (§0.1 B6). This plan previously warned
that the issuer handlers version-check as a read-then-compare **outside** the transaction
(`admin.rs:1449-1452`, `:1480-1483`, `:1532-1535`, `:1563-1566`) and instructed the implementer to
diverge from the template. **All 33 admin mutation sites now do the check inside the transaction**, so
the template is the correct pattern to copy rather than the one to avoid, and the "note the divergence
in the PR" and "do not fix the issuer handlers here" instructions are both moot.

The mechanism to copy is `lock_and_match_version` (`src/infra/repositories/admin.rs:2503-2516`) with
the per-table `*_VERSION_FOR_UPDATE` constants (`:2495-2500`) — **not** `rotate_credential`'s inline
form, which this plan cites at a line number (`:392-403`) that is now `create_provider`'s INSERT. The
handler takes `let expected_version = require_if_match(&headers)?;` and passes it into the service,
which passes it into the repository; the repository evaluates it under `select … for update`.

One gate this plan predates: `every_if_match_operation_declares_the_documented_precondition`
(`src/http/mod.rs:1122`). All eight new `If-Match` operations must declare the precondition in their
utoipa annotations or the spec tests fail.

`GET /api/v1/admin/setup/auth-methods` lives in this file and is tagged `admin-setup` (grouping with the other setup endpoints), while the seven `/admin/auth/providers…` operations are tagged `admin-auth-settings`.

**No route, handler, DTO, or utoipa path for `POST /api/v1/admin/auth/providers/{id}/rotate-secret` may appear in this file or in `documented_router()`.** It was removed by decision **D7** and its absence is part of the frozen contract plans 08/09 bind to.

⚠️ **Route registration.** New routes go in **`admin_routes()`** (`src/http/mod.rs:466`), alongside
the existing `.routes(routes!(admin::get_setup_status))` at `:468`. `documented_router()` (`:274`)
now only merges route groups and applies layers; registering directly there — which this plan's
earlier text implies — puts the routes outside the admin body-limit and timeout layers applied at
`:325-331`.

### Module 13 — cache, listener hook, and settings TTL

⚠️ **This module was missing.** The Multi-Agent Workflow section assigns "module 13" to Agent 6 and
the Definition of Done and the e2e test
`an_auth_settings_write_invalidates_the_cache_via_listen_notify` both depend on it, but Detailed
Implementation stopped at Module 12 — so the auth-settings cache that the whole LISTEN/NOTIFY story
rests on was specified nowhere (§0.3). Written in here.

**13a — `AppState` cache field.** Add an auth-settings cache to `AppState`
(`src/app/state.rs:21-52`), following the shape of the existing `RuntimeConfigCache` rather than
inventing a second caching idiom. It is read by `GET /api/v1/admin/setup/auth-methods` and by the
console at boot; it is keyed by nothing (the table is effectively a small set) and rebuilt on the next
read after invalidation.

**13b — listener hook.** `listen_once` (`src/infra/db.rs:62-88`) already calls
`cache.invalidate_all()` and `runtime_handles.invalidate_all()` unconditionally on every
notification. Add the auth-settings cache to that unconditional set. **This is also where §0.1 B3's
`CIRCUIT_UNAFFECTED_RESOURCE_TYPES` entry lands** — the two changes are in the same function's blast
radius and must be reviewed together, because attaching the NOTIFY trigger without the allow-list
entry is the silent breaker-reset regression.

**13c — settings TTL.** Add the TTL to `AuthSettings` (`src/config/settings.rs:92-102`). Note that
struct already holds `jwks: JwksFetchSettings` in addition to `admin` and `caller`, contrary to this
plan's claim that it holds "only" the two (§0.4). Environment binding is
`Environment::with_prefix("MOIRA").prefix_separator("_").separator("__")` (`:553-557`).

**13d — required test.** `circuit_reset_scope` returns `CircuitResetScope::Unaffected` for an
`auth_provider_settings` payload. Without this the B3 regression has no gate and would return the
moment someone adds another settings table.

---

## i18n Compliance (CONVENTIONS §4) — mandatory

Every key below must be added to `src/i18n/catalog/errors.rs` / `notices.rs` **with an English `default_message` and a `description`**, and mirrored into `docs/i18n-response-catalog.json` (shape: `{ "version", "default_locale", "namespace", "entries": [ { "key", "default_message", "description" } ] }`) **in the same PR**.

**Derivation rule, verified:** `message_key = format!("moira.error.{}", self.code())` (`src/error.rs:146-148`), and `code()` returns the literal passed to `AppError::coded`/`conflict`/`unprocessable` for the `Api` variant (`:131`). **Therefore every new condition must be raised with `AppError::coded(status, "<code>", …)` — never `AppError::BadRequest`/`Forbidden`/`NotFound`/`Unauthorized`, which derive the generic codes `bad_request`/`forbidden`/`not_found`/`unauthorized` (`:130,133-135`) and would silently drop the specific key.** This is the single most common way a plan like this ships a key that never appears on the wire; a test asserts each code actually reaches the client.

### New `moira.error.*` entries

| Code | HTTP | Raised by | `default_message` |
|---|---|---|---|
| `unregistered_trusted_issuer` | 400 | `resolve_active_issuer` (module 5) | "The target issuer is not a registered, active trusted JWT issuer." |
| `admin_claim_email_required` | 400 | policy step 3 (module 10) | "An email address is required to claim an admin identity." |
| `admin_claim_email_not_verified` | 403 | policy step 2 | "The email address for this identity is not verified." |
| `admin_claim_domain_not_allowed` | 403 | policy steps 1 **and** 4 | "This email domain is not allowed to claim an admin identity." |
| `admin_identity_already_claimed` | 409 | `insert_grant` unique violation (module 5) | "This identity has already been granted admin access." |
| `setup_claim_credential_required` | 401 | `resolve_claim_credential` (module 11) | "A system key or one-time setup token is required." |
| `setup_token_invalid` | 401 | `consume_setup_token` (module 5) | "The setup token is not valid." |
| `setup_token_expired` | 401 | `consume_setup_token` | "The setup token has expired." |
| `setup_token_consumed` | 401 | `consume_setup_token` | "The setup token has already been used." |
| `setup_token_target_mismatch` | 403 | module 8 step 1 | "The setup token was issued for a different identity." |
| `auth_provider_not_found` | 404 | module 9 | "The auth provider configuration was not found." |
| `duplicate_auth_provider` | 409 | unique-index violation (module 6) | "An auth provider is already configured for this method and issuer." |
| `auth_provider_method_unsupported` | 400 | module 9 validation | "The requested auth method is not supported." |
| `auth_provider_method_config_incomplete` | 400 | module 9 create/enable validation | "The auth provider configuration is incomplete for this method." |
| `auth_provider_url_not_allowed` | 400 | module 9 URL validation / plan 03's SSRF fetcher | "The configured URL is not allowed." |
| `console_issuer_must_not_assert_scopes` | 400 | module 7d | "A console issuer must not map a scopes claim." |

**Removed by decision `D7` — do not add these keys:** `auth_provider_secret_required`, `auth_provider_secret_not_supported`, and `auth_provider_secret_rebind_required`. All three described conditions that can no longer occur, because Moira accepts, stores, and binds no OAuth client secret. `auth_provider_method_config_incomplete` replaces the *non-secret* half of what `auth_provider_secret_required` used to cover (enabling a method whose configuration is structurally incomplete); the secret half has no Moira-side meaning and belongs to the console's own wizard validation.

**Wording notes forced by the resolved decisions:**

- `auth_provider_method_config_incomplete`'s `description` must say what it does **not** cover, or an implementer will re-add a secret check: *"Used when a create or enable request leaves the method's required non-secret configuration incomplete — e.g. `generic_oidc` with neither `issuer` nor `discovery_url`, or `jwks` with no `jwks_url`. Moira never checks for an OAuth client secret: under decision D7 the client secret is owned by the console and Moira does not store it."*
- `admin_claim_email_required`'s `default_message` **must not** mention setup tokens. Email is required on **both** credential paths now; the earlier wording ("…with a setup token") described the withdrawn carve-out and would actively mislead an operator hitting it on the system-key path. Its `description` should say: *"Used when a claim omits an email address, presents an empty one, or presents a value from which no domain can be extracted. Email is required on both the system-key and setup-token paths; there is no exemption."*
- `admin_claim_domain_not_allowed` now covers **two** conditions — no enabled auth-provider configuration governs the target issuer (step 1), and the email's domain is absent from a configured `allowed_email_domains` (step 4). One code for both is deliberate: distinguishing them on the wire would tell an unprivileged caller whether a policy exists, and both have the same operator remedy. Its `description` carries the actionable setup guidance required by module 10: *"Used when the deny-by-default email-domain policy refuses a claim, either because no enabled auth-provider configuration governs the target issuer or because the email's domain is not in its `allowed_email_domains`. An unconfigured or empty allow-list denies every claim on every credential path; there is no first-claim exemption. The operator must create and enable an auth-provider configuration with the intended domains before any claim can succeed."* Keep the `default_message` short and user-facing; the guidance lives in `description`, per the catalog's own field semantics.

### Reused existing keys (no new entry — verified present in the catalog)

`scope_invalid` (400, unknown scope in the claim body), `resource_version_conflict` (409, `If-Match`), `idempotency_conflict` (409), `not_found`, `unauthorized`, `forbidden`, `bad_request`, and **`invalid_request`**.

`invalid_request` is newly *used* by this plan but is **not** a new key — verified present at `src/i18n/catalog/errors.rs:35` (`default_message: "The request is invalid."`, `description: "Used when the request cannot be parsed or violates a basic contract rule."`), which already fits the case exactly. It is emitted by module 11's `claim_body_rejection` at whatever status axum's `JsonRejection` reports (400 for malformed JSON, 422 for a schema-violating body — e.g. `email` or `email_verified` omitted, now that both are required). **Derivation check performed:** `AppError::coded(status, "invalid_request", …)` is the `Api` variant, whose `code()` returns the literal (`src/error.rs:130-131`), and `message_key()` is `format!("moira.error.{}", code())` (`:146-148`) → `moira.error.invalid_request` → resolves. No catalog addition, no `docs/i18n-response-catalog.json` change for this key. The e2e test `claim_without_an_email_is_rejected_with_a_catalogued_error` asserts the key actually reaches the client rather than trusting the derivation.

### Pre-existing catalog gaps this plan depends on

⚠️ **RESOLVED — there is no gap. This whole section is obsolete (§0.4).**

This plan claimed two codes it emits have no catalog entry, and went out of its way to "correct" an
earlier draft that said otherwise. **The earlier draft was right and the correction was wrong.** Both
entries exist:

- `database_unavailable` — `src/i18n/catalog/errors.rs:29-32`. (The error path itself is real: 503
  from `GET /api/v1/admin/setup/claim-status`, `AppError::DatabaseUnavailable`, `src/error.rs:136`.)
- `idempotency_in_progress` — `src/i18n/catalog/errors.rs:49-52`. (409 from `claim_idempotency`,
  which is now at `src/infra/repositories/admin.rs:651`, not `:576,610`.)

The "if 06 has not landed, 07 must add these two entries itself" contingency is therefore moot — 06
landed (`39c5326`).

**The class of bug this section worried about is now gated — but read what the gate actually is,
because an earlier draft of this section overstated it.**

- **Compile error**, via the `const _: () = { … }` block at `src/i18n/catalog/mod.rs:107-121`: only
  `ExecutionFailureClass::ALL`. `code()` is a `const fn`, so a missing entry for an execution-failure
  class is `error[E0080]`.
- **Test failure**, not a build failure, for everything else: codes passed to
  `AppError::coded`/`conflict`/`unprocessable` are covered by
  `every_coded_error_literal_in_src_has_a_catalog_entry`, which walks source literals, and
  `validate_override`'s forwarded codes by `every_validate_override_code_has_a_catalog_entry`. The
  `docs/i18n-response-catalog.json` mirror has its own test.

So a new `moira.error.*` this plan emits without a catalog entry is caught by **`cargo test`, not
`cargo build`**. That is still a gate, and no coverage test needs hand-writing — but an implementer
who trusted the earlier "fails `cargo build`" phrasing could skip the test run and believe a green
build meant a complete catalog. It does not.

### New `moira.notice.*` entries

**Context:** all four existing `moira.notice.*` entries currently have **zero production consumers** (verified: `grep -rn 'moira\.notice' src/` matches only the catalog, its README, and a doc-test). **This plan is the first real consumer of the notice catalog**, so the pattern it sets is the pattern later plans copy. Keep it minimal and honest: emit a notice only where the response actually carries prose the console will show a human.

| Key | Emitted on | `default_message` |
|---|---|---|
| `moira.notice.admin_identity_claimed` | `POST …/setup/claim` 201 **and** 200 (replay returns the stored body verbatim) | "Admin access has been granted to this identity." |
| `moira.notice.setup_token_issued` | `mint_setup_token` | "A one-time setup token was issued. It is shown once and cannot be retrieved again." |

**Deliberately no notice on:** `GET …/claim-status` (the frozen `{ "claimed": bool }` shape carries no prose, and adding a field would break 08/09's binding); all `auth_provider_settings` records (pure configuration data, no prose); `GET …/setup/auth-methods` (pure data). Per CONVENTIONS §4.2, a notice entry is required for every new success *string* — a response with no human-readable string needs none, and inventing one to satisfy a checklist would be noise.

`message_args` is used for structured interpolation only — never pre-formatted English prose (CONVENTIONS §4.3).

---

## Multi-Agent Workflow

**Wave 0 (coordinator, sequential, before any agent starts).** Four checks, all blocking:
**All five were performed on 2026-07-26 against HEAD `c45257f`; the answers are in §0. They are kept
here as the record of what was asked, with each answer recorded inline. Do not re-derive them from
scratch — verify §0 still matches the tree and move on.**

1. ~~Re-read `src/security/auth.rs` fresh and re-locate `authenticate_trusted_jwt`, `actor_from_trusted_claims`, `verify_api_key`, `authenticate_admin`'s bearer branch, and `ActorType`.~~ **Done — §0.5.** All 12 citations to this file were stale. Real anchors: `ActorType` `:30-39`, `authenticate_admin` `:308`, `authenticate_caller` `:353`, `verify_api_key` `:408`, `authenticate_trusted_jwt` `:474`, `actor_from_trusted_claims` `:826`, `header_string` `:1086`. This audit also produced **§0.2 D2** — the grant hooks into `authenticate_admin`, not `authenticate_trusted_jwt`.
2. ~~Determine whether plan 03 introduced path-scoped auth middleware on `/api/v1/admin`.~~ **Done — it did not.** `src/http/mod.rs:325-331` layers only `DefaultBodyLimit::max(policy.admin_body_limit_bytes)`, `timeout` and `body_timeout`. Every handler still self-authenticates via `admin_actor` (`src/http/admin.rs:51-56`), so fact 1 holds and no exemption design is needed.
3. ~~Determine whether plan 05's OpenAPI-drift gate has landed.~~ **Done — it landed** (`3ea8037`). The snapshot **must** be regenerated in this PR; command in §0.3. This is no longer conditional.
4. ~~Re-verify the highest migration number and whether plan 06's `listen_once` change has landed.~~ **Done — `0011` is highest (§0.1 B1), and `listen_once` already carries the circuit-scoping change.** That is not a future rebase concern; it is the current shape, and it is what makes **§0.1 B3** a blocker.
5. Choose the exact `docs/` file that carries module 10's operator setup runbook (prefer extending existing setup material over minting a new top-level document) and record it in Components & ownership. **Blocking**, because the deny-by-default decision ships an intentional first-claim 403 and the copy explaining it is a required deliverable, not a nicety.

**Wave 1 (parallel where genuinely disjoint):**
- **Agent 1 — module 1** (both migration files). Fully disjoint; runs first.
- **Agent 2 — modules 2 + 3 + 5 + 6** (domain DTOs and both repositories, sequentially within one agent). The repositories depend on the exact schema and DTO shapes; splitting this across agents buys nothing and costs a handoff. Includes the additive `src/domain/mod.rs` and `src/infra/repositories/mod.rs` re-export lines.
- **Agent 3 — none.** This agent previously owned module 4 (the `src/security/crypto.rs` AAD extension). Decision **D7** deletes that module, so `src/security/crypto.rs` and `src/security/mod.rs` are untouched and no agent is assigned to them. If a coordinator finds an agent editing `crypto.rs` in this plan's diff, that is a scope violation to flag.
- **Agent 4 — modules 8 + 9 + 10** (both application services, the policy logic, **and module 10's operator setup runbook in `docs/`**). Starts once Agent 2 has agreed interfaces; lands after it. This is the largest single piece. **Module 10's policy logic stays inside the service method it governs** — do not split it to a separate agent, which would mean two agents editing one function. The runbook ships with the policy, by the same agent, so the deny-by-default behavior and the copy explaining it cannot land apart.
- **Agent 5 — modules 11 + 12** (both HTTP files + `src/http/mod.rs` registrations + extending `src/http/mod.rs`'s spec tests). Starts once Agent 4's service signatures are agreed.
- **Agent 6 — module 13** (`src/app/state.rs` auth-settings cache field, `src/infra/db.rs` listener wiring, `src/config/settings.rs` TTL field). Disjoint from all others except the flagged plan-06 overlap on `src/infra/db.rs`.
- **Agent 7 — the i18n deliverable** (`src/i18n/catalog/errors.rs`, `notices.rs`, `docs/i18n-response-catalog.json`). Fully disjoint; can run in parallel from the start. Must land **before** Agent 5, so the handlers' error paths reference keys that already exist.
- **Agent 8 (sequential, last, and its own dedicated security reviewer) — module 7** (`src/security/auth.rs` + `src/security/authz.rs`). This is the highest-risk diff in the plan: a single-file change on the authentication hot path. Land it **last**, after modules 5-6 exist (it calls the lightweight read path), as a small focused diff with its own review pass independent of everything else.

**Checkpoints.** Run the full Verification gate list after **every** module lands — not just per wave. This iteration is small enough that per-module gating is affordable and its security-critical nature makes it appropriate.

**Dedicated read-only reviewer for module 7.** A reviewer who is not the authoring agent re-reads the `auth.rs` diff line-by-line against Architecture → Security boundaries and module 7's constraints, explicitly confirming:
- (a) `apply_admin_identity_grant` is unreachable from `authenticate_admin`'s system-key/consumer-key branches and from `authenticate_caller`'s dev-trust-header branch;
- (b) a bare trusted JWT with no grant row produces an `Actor` **byte-identical** to pre-change behavior;
- (c) there is **no** code path that calls `apply_admin_identity_grant` before signature and claims validation complete — it must be impossible for an unauthenticated caller to probe grant existence;
- (d) the scope merge is a union with dedup, never a replace;
- (e) `ActorType::SetupToken` is denied admin implication in **every** `ActorType` match, and no match was made non-exhaustive to accommodate it.

---

## Interfaces & Contracts

### Frozen contract (binding on plans 08/09)

**Operation count: 10.** (`claim-status`, `claim`, `auth-methods`, plus seven `/api/v1/admin/auth/providers…` operations.) It was 11 before decision **D7** removed `rotate-secret`.

| Item | Final value |
|------|-------------|
| Migrations | `migrations/0012_admin_identity_claims.sql`, `migrations/0013_auth_provider_settings.sql` (re-verify numbering at execution; 0008 was highest at plan time) |
| Tables | `admin_identities` (unique active key `(issuer, subject)`), `setup_state` (singleton row), `admin_setup_tokens`, `auth_provider_settings` |
| **Claim-status endpoint** | `GET /api/v1/admin/setup/claim-status` — **unauthenticated** — 200 `{ "claimed": bool }`. **Shape frozen; no fields may be added.** |
| **Claim endpoint** | `POST /api/v1/admin/setup/claim` — `X-Moira-System-Key` header **or** `setup_token` body field — body `ClaimAdminIdentityRequest`, 201 (fresh) / 200 (replay) `AdminIdentityRecord` |
| **Claim request shape** | `ClaimAdminIdentityRequest { issuer: String (req), subject: String (req), email: String (req), email_verified: bool (req), scopes: Vec<String> (default ["moira:admin"]), setup_token: Option<String> }`, `deny_unknown_fields`. **`email` and `email_verified` are REQUIRED on both credential paths — CHANGED from the earlier `Option<String>` / `#[serde(default)] bool`.** A body omitting either is rejected with the `ErrorResponse` envelope carrying `moira.error.invalid_request` (400 malformed / 422 schema-violating). |
| **Claim response shape** | `AdminIdentityRecord.email` is `String`, not `Option<String>` — a grant cannot exist without an email. |
| **Claim domain policy** | **Deny-by-default, no exemptions.** A claim is refused `403 admin_claim_domain_not_allowed` (`moira.error.admin_claim_domain_not_allowed`) unless an **enabled** `auth_provider_settings` row governs the target issuer **and** its `allowed_email_domains` contains the email's domain. Empty or unconfigured ⇒ deny. Applies to the system-key path and the setup-token path identically. **No first-claim exemption, no bootstrap bypass.** Operators must configure and enable an auth-provider row before the first claim can succeed. |
| **Bootstrap auth read** | `GET /api/v1/admin/setup/auth-methods` — **authenticated, decided and confirmed** — setup-actor gated (`ActorType::SystemKey`\|`TrustedJwt` + `moira:setup:read`) — 200 `SetupAuthMethodsResponse { methods: [PublicAuthMethod] }`. Unauthenticated calls get **401**. The console calls it **server-side** with its system key; the browser never calls it. There is no anonymous variant. |
| **Auth-settings list** | `GET /api/v1/admin/auth/providers` — `moira:auth-settings:read`, `params(PageQuery)` — 200 `[AuthProviderSettingsRecord]` |
| **Auth-settings create** | `POST /api/v1/admin/auth/providers` — `moira:auth-settings:write`, optional `Idempotency-Key` — 201 `AuthProviderSettingsRecord` + `ETag` |
| **Auth-settings get** | `GET /api/v1/admin/auth/providers/{id}` — `moira:auth-settings:read` — 200 + `ETag` |
| **Auth-settings patch** | `PATCH /api/v1/admin/auth/providers/{id}` — `moira:auth-settings:write`, **`If-Match` required** — 200 + `ETag` |
| **Auth-settings delete** | `DELETE /api/v1/admin/auth/providers/{id}` — `moira:auth-settings:delete`, **`If-Match` required** — 204 |
| **Auth-settings enable** | `POST /api/v1/admin/auth/providers/{id}/enable` — `moira:auth-settings:write`, **`If-Match` required** — 200 + `ETag` |
| **Auth-settings disable** | `POST /api/v1/admin/auth/providers/{id}/disable` — `moira:auth-settings:write`, **`If-Match` required** — 200 + `ETag` |
| Structural-status endpoint | `GET /api/v1/admin/setup/status` — **pre-existing, unchanged** (authenticated, granular `SetupStatusResponse`) |
| Trusted-JWT-issuer endpoints | `/api/v1/admin/jwt-issuers…` — **pre-existing, unchanged**. These *are* CONVENTIONS §7.3 mode 3 (bring-your-own JWKS/JWT); no new surface is invented for it. |
| Auth methods | `google_oauth`, `generic_oidc`, `jwks` (CONVENTIONS §7.3 modes 1/2/3) — **unchanged by D7** |
| **OAuth client secret** | **Not in Moira. Anywhere.** (decision **D7**) `auth_provider_settings` has no secret columns; no request DTO has a `client_secret` field (and `deny_unknown_fields` rejects one that is sent); no response ever contains one; `POST …/{id}/rotate-secret` **does not exist**. The console owns the secret in its own `console_auth` database. |
| **`auth_provider_settings` columns (non-secret only)** | `id`, `method`, `display_name`, `enabled`, `issuer`, `discovery_url`, `authorization_url`, `token_url`, `userinfo_url`, `jwks_url`, `client_id`, `requested_scopes`, `allowed_email_domains`, `allowed_algorithms`, `expected_audiences`, `redirect_uris`, `trusted_jwt_issuer_id`, `metadata`, `status`, `created_at`, `updated_at`, `deleted_at`, `version`. **No `encrypted_payload` / `encryption_algorithm` / `encryption_version` / `encrypted_data_key` / `nonce` / `secret_fingerprint` / `masked_secret`.** |
| **D7 drift protection (Moira's obligation)** | Moira is the **source of truth for `client_id`**, and returns it as a plain non-secret field on `GET /api/v1/admin/auth/providers`, `GET …/{id}`, and `GET /api/v1/admin/setup/auth-methods` (`PublicAuthMethod.client_id`). **That is sufficient and complete for the console's fingerprint comparison — plan 08 needs no additional Moira endpoint, field, header, or server-computed fingerprint, and must bind to this.** The console stores a fingerprint of `client_id` beside its own secret, re-reads Moira's `client_id` on load, and raises its own actionable keyed error on mismatch. Moira neither stores nor validates that fingerprint. |
| **Patching `issuer` / `client_id`** | Freely allowed under the normal `If-Match` rules. **No `409 auth_provider_secret_rebind_required`** — that error is deleted with the secret it protected. |
| Default granted scope | `["moira:admin"]` (member of `ADMIN_SCOPES`, `src/security/authz.rs:7-91`) |
| New scopes | `moira:auth-settings:read`, `moira:auth-settings:write`, `moira:auth-settings:delete` — appended to `ADMIN_SCOPES` |
| New `ActorType` | `SetupToken` (serializes as `"setup_token"`); denied admin implication |
| New config | `AuthSettings.setup_token_ttl_seconds` (default 900) — **the only** new settings field; auth-method policy including allowed email domains lives in the **database**, per CONVENTIONS §7.2 |
| New error codes | 16 (see i18n section), each with a `moira.error.<code>` catalog entry. **D7 removed three** (`auth_provider_secret_required`, `auth_provider_secret_not_supported`, `auth_provider_secret_rebind_required`) and added one (`auth_provider_method_config_incomplete`) |
| New notice keys | `moira.notice.admin_identity_claimed`, `moira.notice.setup_token_issued` |

> ### ⚠️ CHANGED BY D7 — the client secret is gone from Moira; plans 08 and 09 must be updated
>
> Product-owner decision **D7** (2026-07-25, CONVENTIONS §0) removes the OAuth client secret from Moira entirely. **This is a simplification of Moira's surface, not an addition.** Paths that remain, methods (`google_oauth|generic_oidc|jwks`), scope names (`moira:auth-settings:{read,write,delete}`), `If-Match` requirements, `Idempotency-Key`, `ETag`, utoipa coverage, and `LISTEN/NOTIFY` invalidation are **all unchanged**.
>
> **What changed:**
> 1. **The frozen contract is now 10 operations, not 11.** `POST /api/v1/admin/auth/providers/{id}/rotate-secret` **no longer exists**. Any 08/09 client, wizard step, or test that calls it must be deleted.
> 2. **No request to Moira may carry a `client_secret`.** `AuthProviderSettingsCreateRequest` and `AuthProviderSettingsPatchRequest` have no such field, and `deny_unknown_fields` means sending one is a **loud schema error, not a silent drop**. A stale 08 client will fail fast — which is the intent.
> 3. **No response from Moira contains secret material, including `secret_fingerprint` and `masked_secret`, which are gone from `AuthProviderSettingsRecord`.** Any 08/09 UI rendering a masked secret from Moira has nothing to render and must be removed.
> 4. **The console now owns the OAuth client secret**, stored encrypted at rest in its own `console_auth` database (which Better Auth already requires), written by the setup wizard, never sent to Moira, never exposed to the browser, never in `NEXT_PUBLIC_*`.
> 5. **`409 auth_provider_secret_rebind_required` is deleted**, along with the "changing issuer/client_id invalidates the secret" hazard. On the Moira side, `issuer` and `client_id` are ordinary patchable configuration.
>
> **Drift protection — plan 08's contract with this plan.** Moira is the **source of truth for `client_id`** and already returns it on every read path (`AuthProviderSettingsRecord.client_id`, `PublicAuthMethod.client_id`). **That is sufficient: 08 needs no new Moira endpoint, field, header, or server-side fingerprint.** Plan 08 must (a) have the wizard write Moira's provider config and the console's secret **in the same step**, treating partial success as an operator-resolvable failure; (b) store a fingerprint of `client_id` beside the secret and compare it against Moira's `client_id` on load, raising a specific actionable keyed error on mismatch instead of letting the OAuth code exchange fail with an opaque provider error; (c) ship an e2e test asserting the mismatch path produces that actionable error. All three are **08 deliverables**; Moira's only obligation is the exposed `client_id`, which is already met.
>
> ### ⚠️ FROZEN-CONTRACT CHANGE — claim DTO shape (decision D5)
>
> Product-owner decision **D5** (2026-07-25) changes one shape that 08/09 already bind to. Paths, methods, method names, and scope names are **unchanged**; only the claim request/response DTO moves.
>
> **`ClaimAdminIdentityRequest.email`: `Option<String>` → `String` (required), and `email_verified` loses `#[serde(default)]` (also required).** Correspondingly `AdminIdentityRecord.email`: `Option<String>` → `String`.
>
> Coordinator action items for 08/09:
> 1. **Plan 08** — the setup wizard must always send `email` and `email_verified` on `POST /api/v1/admin/setup/claim`, on the system-key path too. Any generated TypeScript client must regenerate against the new schema (both fields become non-optional). Any 08 text describing email as optional for operator-driven claims is now wrong.
> 2. **Plan 08/09** — the wizard must guide the operator to **configure and enable an auth-provider row with `allowed_email_domains` before** the first claim, and must render `admin_claim_domain_not_allowed` as an actionable setup step, not a generic failure. A wizard that presents the claim step before the provider step will always produce a 403 on a fresh deployment.
> 3. **Plan 08** — `GET /api/v1/admin/setup/auth-methods` is confirmed authenticated: 08 must call it **server-side** from the BFF with the system key. Any 08 draft that fetches it from the browser, or that assumes an anonymous variant, must be corrected. `GET …/setup/claim-status` remains the only anonymous call.
> 4. **Plan 09** — invitation/additional-admin flows build on the same claim path, so they inherit required-email and deny-by-default domain policy. There is no invitation-based exemption either; an invited admin's email must still be in an allowed domain.
>
> 5. **Plan 08** — the wizard must **stop sending `client_secret` to Moira** and must write it to the console's own `console_auth` store instead (D7 callout above). Remove any rotate-secret call, any masked-secret display sourced from Moira, and any assumption that Moira can hand the secret back.
>
> Everything else in this table is unchanged and remains binding: `GET /api/v1/admin/setup/claim-status`, `POST /api/v1/admin/setup/claim`, `GET /api/v1/admin/setup/auth-methods`, the remaining seven `/api/v1/admin/auth/providers*` operations, methods `google_oauth|generic_oidc|jwks`, scopes `moira:auth-settings:{read,write,delete}`, `If-Match` on every mutation, `Idempotency-Key` on create, `ETag` on every record response, full utoipa coverage, and `LISTEN/NOTIFY` cache invalidation.

**08's setup-wizard flow against this contract:** unauthenticated `GET …/setup/claim-status` → if `claimed: false`, the BFF (holding a server-side Moira system key) calls `GET …/setup/auth-methods` **server-side** to learn how to configure Better Auth → the operator creates and **enables** the `auth_provider_settings` row in Moira, **including `allowed_email_domains`** (without this the next step always 403s) and **including `client_id`**, while the wizard writes the matching **client secret plus a fingerprint of that `client_id` into the console's own `console_auth` database in the same step** (D7; partial success is a failure the operator must resolve) → the BFF drives the human through OAuth → the BFF calls `POST …/setup/claim` with the system key and the human's `(issuer, subject, email, email_verified)`, all four required → after the grant, the human's own trusted-JWT calls reach the *existing* `GET …/setup/status` for structural readiness. The BFF registers its own JWKS endpoint as a `trusted_jwt_issuer` via the **pre-existing** `/api/v1/admin/jwt-issuers` surface, with `scopes_claim` left NULL (CONVENTIONS §7.5).

### `GET /api/v1/admin/setup/claim-status`

- **Auth:** none. No headers required, no `Actor` resolved.
- **200:** `{ "claimed": true | false }`. **No other fields, ever.** Deliberately distinct from the sibling `GET /api/v1/admin/setup/status` (system-key/trusted-JWT gated, returns `SetupStatusResponse` with granular `checks`/`missing` detail about provider/routing configuration). A client calls **this** endpoint first, unauthenticated, to decide whether to show "claim your admin account"; it calls the *existing* structural endpoint only after the human has authenticated and been granted admin.
- **503:** database unavailable (`AppError::DatabaseUnavailable` → `moira.error.database_unavailable` — see the pre-existing-gap note in the i18n section).
- No `Idempotency-Key`, no `If-Match` (read-only GET).

### `POST /api/v1/admin/setup/claim`

- **Auth:** `X-Moira-System-Key` (must carry `moira:admin`, verified against `system_api_keys` by `verify_system_key_only`) **or** `setup_token` in the JSON body (verified against `admin_setup_tokens`, single-use). **A bare `Authorization: Bearer <jwt>` alone is rejected 401 regardless of the JWT's scopes** — the structural "no first-login-wins" enforcement.
- **Headers:** optional `Idempotency-Key`. When present, the claim is replay-safe (identical request + key → identical response, no duplicate grant) via the shared admin envelope. When absent, duplicate *effects* are still impossible because the `(issuer, subject)` unique index rejects a second claim with `409 admin_identity_already_claimed` — but a caller retrying without a key gets 409, not a replay. Documented explicitly in the OpenAPI description.
- **Request body:** `ClaimAdminIdentityRequest`, `deny_unknown_fields`. **`issuer`, `subject`, `email`, and `email_verified` are all required** (decision **D5** — see the frozen-contract change callout above). A body omitting `email` or `email_verified` never reaches the service: the `Json` extractor rejects it and module 11's `claim_body_rejection` returns the standard `ErrorResponse` envelope with `code: "invalid_request"` and `message_key: "moira.error.invalid_request"` at axum's own status (400 malformed JSON, 422 schema violation).
- **Policy, both paths, no exemptions:** `email_verified` must be `true` (else 403 `admin_claim_email_not_verified`); the email must be non-empty and contain an extractable domain (else 400 `admin_claim_email_required`); an enabled `auth_provider_settings` row must govern the target issuer and list the email's domain in `allowed_email_domains` (else 403 `admin_claim_domain_not_allowed`). **Deny-by-default: unconfigured or empty ⇒ deny.** The system-key path is authorised to *submit* a claim; it is not exempt from *policy*. There is no first-claim exemption and no bootstrap bypass.
- **201 Created:** new grant. **200 OK:** idempotent replay. Body shape (`AdminIdentityRecord`, including `notice`) identical in both cases; the status code is what distinguishes them. `AdminIdentityRecord.email` is a required `String`.
- **Status → code mapping:** see the i18n table. Every non-2xx carries the standard `ErrorResponse` envelope with a non-empty `message_key` and `message`.
- **Scopes granted:** defaults to `["moira:admin"]`; overridable in the body to a narrower set drawn from `ADMIN_SCOPES`, with unknown scope strings rejected 400 `scope_invalid`.
- **Transaction boundary:** one transaction per claim: advisory lock → idempotency claim/replay check → savepoint → `insert_grant` → `mark_setup_claimed` → audit insert → release savepoint → finalize idempotency → commit. Policy validation happens **before** the envelope opens. This is the existing `AdminCommandRunner` shape verbatim; **no new transaction shape is invented**.
- **Cache invalidation:** none. `admin_identities` is not provider/routing config and is not part of `RuntimeConfigCache`/`ProviderRuntimeCache`; no NOTIFY trigger is attached to it. (`auth_provider_settings` **does** get one — see below.)
- **Concurrency:** two concurrent claims for the same `(issuer, subject)` → exactly one 201, the other 409 (`admin_identity_already_claimed` from the unique index, or `idempotency_in_progress` from the advisory lock, depending on timing). Never both succeed; never a duplicate row. Different identities proceed independently (different lock keys).
- **SSE:** not applicable.

### `/api/v1/admin/auth/providers…`

- **Auth:** standard admin authentication (`admin_actor`), plus the new `moira:auth-settings:*` scopes.
- **Versioning:** `If-Match` **required** on every mutation except create, checked **inside** the transaction under `select … for update` — which since plan 06b is what every existing admin handler already does, so this matches the house pattern rather than diverging from it (the parenthetical contrast this line used to draw is obsolete; see module 12). Stale version → `409 resource_version_conflict`.
- **Idempotency:** `Idempotency-Key` supported on create, via the shared admin envelope.
- **Secrets: none (decision `D7`).** No operation on this surface accepts a `client_secret`, and no response contains one — there is no `secret_fingerprint` and no `masked_secret` either, because there is no secret to fingerprint or mask. The OAuth client secret is owned by the console and lives in the console's own `console_auth` database. `POST /api/v1/admin/auth/providers/{id}/rotate-secret` **does not exist**.
- **Read exposure:** because the payload contains no secret material at all, these reads are safe for any holder of `moira:auth-settings:read`. The scope gate remains, as configuration-disclosure control rather than secret protection.
- **`client_id` is the D7 drift anchor:** always returned, non-secret, and the value the console fingerprints against its own stored secret. Do not remove it from any read projection.
- **Cache invalidation:** every mutation fires the existing `notify_moira_runtime_config_change()` trigger on channel `moira_runtime_config`, which `listen_once` (`src/infra/db.rs:59-80`) consumes to invalidate the auth-settings cache on **every** instance — CONVENTIONS §7.2's requirement, satisfied through the existing mechanism with no new channel.
- **Deny-by-default:** `enabled` is `false` on create; enabling a method with an incomplete configuration is rejected 400.

---

## Verification (CONVENTIONS §3 — unit **and** e2e are both mandatory)

### Unit tests (new, named)

| File | Test | Proves |
|------|------|--------|
| `src/security/auth.rs` | `admin_identity_grant_unions_and_dedups_scopes` | module 7a union semantics |
| | `actor_without_a_grant_is_byte_identical_to_pre_change` | backward compatibility |
| | `setup_token_actor_is_denied_admin_implication` | module 7c |
| `src/application/identity.rs` | `claim_rejects_unverified_email` | policy step 2 |
| | `claim_rejects_email_domain_absent_from_the_allow_list` | policy step 4 |
| | `claim_denies_every_domain_when_the_allow_list_is_empty` | **deny-by-default**, the single most important policy assertion |
| | `claim_denies_when_no_enabled_auth_provider_governs_the_issuer` | policy step 1 — "unconfigured" denies exactly like "empty" |
| | `claim_rejects_a_scope_outside_admin_scopes` | scope validation |
| | `claim_requires_an_email_on_the_setup_token_path` | policy step 3, token path |
| | `claim_requires_an_email_on_the_system_key_path` | policy step 3, **system-key path — the withdrawn carve-out is gone** (replaces the earlier `claim_allows_a_null_email_on_the_system_key_path`, which asserted the opposite and must be deleted, not adapted) |
| | `claim_rejects_a_blank_or_domainless_email_on_both_paths` | what the type system cannot catch: `""`, whitespace, no `@` |
| | `domain_policy_is_enforced_identically_on_the_system_key_path` | **no bypass** — same fixture, same disallowed domain, denied on both `ClaimCredential` variants |
| | `system_key_credential_grants_no_policy_exemption_on_a_fresh_deployment` | the explicit "no first-claim exemption / no bootstrap bypass" assertion: zero prior grants + unconfigured allow-list + valid system key ⇒ 403 `admin_claim_domain_not_allowed` |
| | `domain_match_is_case_insensitive_and_not_subdomain_wildcarded` | policy step 5 |
| | `claim_rejects_an_issuer_with_no_active_trusted_issuer_row` | module 5 `resolve_active_issuer` |
| `src/domain/identity.rs` | `claim_request_requires_email_and_email_verified` | deserializing a body omitting either field **fails** — pins decision **D5** at the DTO level, where 08/09's generated clients bind |
| `src/http/identity.rs` | `claim_body_rejection_maps_to_the_invalid_request_code` | a `JsonRejection` becomes `AppError::coded(_, "invalid_request", _)`, preserving axum's status and yielding `moira.error.invalid_request` |
| `src/application/auth_settings.rs` | `jwks_method_requires_a_jwks_url_and_rejects_a_client_id` | method-shape validation (non-secret) |
| | `oidc_method_requires_an_issuer_or_discovery_url_before_enabling` | deny-by-default enablement on **non-secret** completeness — Moira has no secret to check (**D7**) |
| | `patch_allows_changing_issuer_and_client_id` | **D7**: with no AAD-bound secret, these are ordinary mutable config; the deleted `auth_provider_secret_rebind_required` path must not come back |
| | `console_issuer_with_a_scopes_claim_is_rejected` | **CONVENTIONS §7.5** |
| `src/domain/auth_settings.rs` | `auth_provider_dtos_have_no_client_secret_field` | **D7** structural guard: neither create nor patch deserializes a `client_secret`, and `deny_unknown_fields` makes sending one an error rather than a silent drop |
| | `public_auth_method_never_exposes_secret_fields` | projection guard (now a forward guard against reintroduction — no secret field exists to redact) |
| | `public_auth_method_exposes_client_id` | **D7 drift-protection contract**: `client_id` is present on the read projection plan 08 fingerprints, so 08 can bind to it |
| `src/infra/repositories/identity.rs` | `fake_admin_identity_repository_supports_claim_unit_tests` | the trait exists from day one (no P2-3 retrofit needed) |
| `src/i18n/catalog/mod.rs` | `identity_error_keys_exist_in_the_catalog`, `identity_notice_keys_exist_in_the_catalog` | **CONVENTIONS §4.5** — all 16 error keys and 2 notice keys present |
| | `no_auth_provider_client_secret_keys_exist_in_the_catalog` | **D7** regression guard: `moira.error.auth_provider_secret_required`, `…_secret_not_supported`, and `…_secret_rebind_required` are absent from the catalog and from `docs/i18n-response-catalog.json` |
| `src/error.rs` | `identity_error_codes_derive_their_documented_message_keys` | the `AppError::coded` vs `AppError::Forbidden` trap is closed |

All service-layer unit tests run against the module-5/6 repository fakes — **no Postgres required**.

### E2E tests (new, named — real HTTP surface, real PostgreSQL 16 + pgvector)

Following `tests/support/mod.rs` and the in-process-router pattern of `tests/admin_idempotency.rs` (`moira::build_router(state.clone())`; the `post(router, path, key, if_match, body)` helper at `:168-212`; the `Arc::new(Barrier::new(n))` + `barrier.wait().await` concurrency pattern at `:518,535`). Concurrency tests use **acknowledgement gates, never `sleep()`** (CONVENTIONS §3; plan 06 P2-12) — if 06 has not landed, implement the gate locally with the existing `Barrier` pattern.

**`tests/identity_claim.rs` (new)**

| Test | Proves |
|---|---|
| `fresh_database_reports_claim_status_false` | Definition of Done |
| `claim_status_is_unauthenticated_and_returns_only_a_boolean` | frozen shape; response has exactly one key |
| `system_key_claim_succeeds_and_flips_claim_status_to_true` | happy path |
| `bare_trusted_jwt_cannot_claim_regardless_of_its_scopes` | **the P1-11 headline test — "first-login-wins is impossible"** |
| `granted_identity_resolves_to_admin_scope_on_its_next_trusted_jwt_request` | module 7a end-to-end |
| `ungranted_subject_on_the_same_issuer_gains_no_scopes` | no scope leak across subjects |
| `claim_is_idempotent_under_an_idempotency_key` | 200 replay, one row |
| `claim_without_an_idempotency_key_conflicts_on_retry` | documented 409-not-replay behavior |
| `concurrent_claims_for_the_same_identity_yield_one_201_and_one_409` | barrier-gated; exactly one row afterward |
| `concurrent_claims_for_different_identities_both_succeed` | lock keys are per-identity |
| `setup_token_claim_succeeds_once_and_is_rejected_on_reuse` | single-use at the DB level |
| `expired_setup_token_is_rejected` | TTL enforcement |
| `setup_token_cannot_claim_an_identity_other_than_its_target` | target binding |
| `claim_with_an_unregistered_issuer_returns_400_unregistered_trusted_issuer` | issuer must be vetted |
| `claim_with_an_unverified_email_returns_403` | policy |
| `claim_with_a_disallowed_domain_returns_403` | policy |
| `claim_is_denied_by_default_when_no_domain_allow_list_is_configured` | **deny-by-default at HTTP level** — a valid system-key claim against an `auth_provider_settings` row whose `allowed_email_domains` is `'{}'` returns **403** with `error.code == "admin_claim_domain_not_allowed"` and `error.message_key == "moira.error.admin_claim_domain_not_allowed"` |
| `claim_is_denied_when_no_auth_provider_configuration_exists_at_all` | the stricter sibling: **zero** `auth_provider_settings` rows, fresh database, valid system key, `claimed: false` ⇒ still 403, same code. **This is the "no bootstrap bypass" test** — it must be impossible to make it pass by adding a first-claim exemption |
| `system_key_path_is_not_exempt_from_the_domain_policy` | the same disallowed-domain body denied on the system-key path exactly as on the setup-token path — the withdrawn carve-out cannot come back |
| `claim_succeeds_once_the_operator_configures_and_enables_the_allowed_domain` | the positive counterpart: the denial is *configuration*, not breakage — configure + enable, then the identical claim returns 201. Without this, the deny tests could be satisfied by an endpoint that never works |
| `claim_without_an_email_is_rejected_with_a_catalogued_error` | decision **D5** at HTTP level: a body omitting `email` returns axum's schema-violation status with the full `ErrorResponse` envelope, `error.code == "invalid_request"`, and a non-empty `message_key`/`message` — **not** axum's bare plain-text rejection |
| `claim_without_email_verified_is_rejected_with_a_catalogued_error` | same, for the field that lost `#[serde(default)]` — proves an omitted flag is a schema error, not a silent `false` |
| `system_key_claim_without_an_email_is_rejected` | the system-key path gets no email exemption either |
| `every_granted_admin_identity_row_carries_a_non_null_email` | audit-attribute guarantee: after every successful claim in this suite, `admin_identities.email` is non-null (the application invariant behind the deliberately-nullable column) |
| `every_claim_error_response_carries_a_nonempty_message_key_and_message` | **CONVENTIONS §4.5** |
| `successful_claim_response_carries_the_admin_identity_claimed_notice` | notice emission |
| `every_claim_attempt_writes_exactly_one_audit_row_with_the_correct_actor_type` | audit fidelity, incl. `setup_token` |
| `existing_setup_status_endpoint_is_unaffected_by_this_plan` | no collision between the two setup concepts |

**`tests/auth_provider_settings.rs` (new)**

| Test | Proves |
|---|---|
| `create_google_oauth_provider_stores_only_non_secret_configuration` | **D7**: the created row's columns are exactly the non-secret set; a direct DB inspection finds no envelope column, because none exists on the table |
| `auth_provider_requests_reject_a_client_secret_field` | **D7**: a create or patch body carrying `client_secret` is rejected by `deny_unknown_fields` with a catalogued `ErrorResponse` — never silently accepted, never silently dropped |
| `no_auth_provider_response_contains_secret_material` | **secret-leak, CONVENTIONS §8** — no response body from any of the seven provider operations or from `…/setup/auth-methods` contains `client_secret`, `secret_fingerprint`, `masked_secret`, or any envelope field |
| `openapi_document_has_no_rotate_secret_operation_and_no_secret_schema_fields` | **D7 at the contract level**: `POST /api/v1/admin/auth/providers/{id}/rotate-secret` is absent from the generated spec, as are `client_secret`/`secret_fingerprint`/`masked_secret` on every auth-settings schema. This is the test 08/09 bind against |
| `rotate_secret_path_returns_404_because_it_does_not_exist` | **D7**: the removed route is genuinely unrouted, not merely undocumented |
| `patch_requires_if_match_and_conflicts_on_a_stale_version` | optimistic concurrency |
| `patching_the_issuer_and_client_id_succeeds_with_a_valid_if_match` | **D7**: the former AAD-rebind 409 is gone; these are ordinary configuration fields now |
| `client_id_is_returned_by_the_read_endpoints_for_drift_comparison` | **D7 drift-protection contract at HTTP level**: `GET …/auth/providers/{id}` and `GET …/setup/auth-methods` both return `client_id`, and it reflects the value most recently written — the exact guarantee plan 08's fingerprint comparison binds to |
| `jwks_method_without_a_jwks_url_returns_400` | method-shape validation (non-secret) |
| `enabling_a_provider_with_incomplete_non_secret_config_returns_400` | deny-by-default enablement, `auth_provider_method_config_incomplete` |
| `a_second_provider_for_the_same_method_and_issuer_returns_409` | unique index |
| `create_is_idempotent_under_an_idempotency_key` | shared envelope |
| `auth_settings_endpoints_require_their_scopes` | authz gating on all seven provider routes |
| `setup_auth_methods_requires_a_setup_actor_and_rejects_anonymous` | the endpoint is authenticated on purpose |
| `setup_auth_methods_rejects_an_unauthenticated_call_with_401` | **decision D4, asserted exactly**: `GET /api/v1/admin/setup/auth-methods` with **no** `X-Moira-System-Key` and **no** `Authorization` header returns **401** with a coded `ErrorResponse` and a non-empty `message_key`. Assert the body contains no `methods` array, no issuer, no client id, and no domain policy — the point is that anonymous reconnaissance yields nothing |
| `setup_auth_methods_rejects_a_setup_actor_missing_the_setup_read_scope` | the scope half of the gate, not just the actor-type half |
| `setup_auth_methods_succeeds_for_a_system_key_actor` | the server-side call 08's BFF makes actually works — the decision does not break the wizard |
| `claim_status_is_anonymous_while_auth_methods_is_not` | pins the deliberate asymmetry in one place: the same unauthenticated client gets **200 `{"claimed": …}`** from `…/setup/claim-status` and **401** from `…/setup/auth-methods`. If a future change makes these agree, this test fails and forces the reasoning to be revisited |
| `setup_auth_methods_projects_only_public_fields` | narrow projection |
| `an_auth_settings_write_invalidates_the_cache_via_listen_notify` | **CONVENTIONS §7.2 cache-invalidation requirement, proven not asserted** |
| `every_auth_settings_error_response_carries_a_nonempty_message_key_and_message` | **CONVENTIONS §4.5** |

**Regression baseline (must pass unmodified):** `tests/admin_idempotency.rs` (9), `tests/execution_lifecycle.rs` (14), `tests/public_authorization.rs`, `tests/http_error_contract.rs`, `tests/security_foundation.rs`.

### Other verification

- **Migration:** both new migrations apply cleanly to a fresh database via `sqlx::migrate!` (`MIGRATOR`, `src/infra/db.rs:20`; `migrate()` `:40-45`); `tests/security_foundation.rs`'s migration-contract job passes with them included, confirming append-only compliance (**no edit to `0001-0011`** — the window grew; see §0.1 B1).
- **OpenAPI:** `src/http/mod.rs`'s spec tests are **extended** to cover all **ten** new operations — presence, status codes, schemas, security annotations, and the documented-and-commented exemption for `get_setup_claim_status` being unauthenticated. They must also assert the **absence** of `POST /api/v1/admin/auth/providers/{id}/rotate-secret` (**D7**). There are ~26 such tests, not 8 (§0.4).

  ⚠️ **The committed snapshot IS regenerated in this PR — this is no longer conditional (§0.3).** Plan
  05 landed (`3ea8037`) and `docs/openapi.json` is frozen and committed. Regenerate with:

  ```
  UPDATE_SNAPSHOTS=1 cargo test --lib http::tests::committed_openapi_matches_the_generated_document
  ```

  and commit the result. **Two** gates enforce it — the unit gate `committed_openapi_matches_the_generated_document`
  (`src/http/mod.rs:1649`) and the e2e gate `served_openapi_document_matches_committed_docs_openapi_json`
  (`tests/openapi_drift.rs:100`) — so adding ten operations without regenerating fails both. Three
  further gates this plan predates must also be satisfied:
  `every_if_match_operation_declares_the_documented_precondition` (`:1122`),
  `atomic_admin_idempotency_contract_is_explicit` (`:907`), and
  `once_only_key_responses_use_the_secret_envelope` (`:884`).
- **Secret-leak:** the setup token is the only secret this plan writes. A test asserts `admin_setup_tokens.token_hash` is an Argon2id hash — not plaintext and not a reversible encoding — and that the minted token appears in exactly one response body (the mint response) and nowhere else, mirroring the existing `system_api_keys` handling and `src/security/masking::tests`. **The OAuth client secret needs no leak test in Moira, because Moira never receives one (D7)**; the corresponding secret-leak coverage for it is plan 08's, against the console's own store and bundle.
- **Required gates (CONVENTIONS §2, verbatim):**
  ```bash
  cargo fmt --check
  cargo clippy --workspace --all-targets --all-features -- -D warnings
  cargo test --workspace --all-features
  cargo build --release --locked
  ```
  plus clean PostgreSQL migration validation and OpenAPI generation/validation as above.

---

## Definition of Done

**Plan-specific**

- [ ] `migrations/0012_admin_identity_claims.sql` and `migrations/0013_auth_provider_settings.sql` exist, are append-only (no prior migration edited), reuse `moira_bump_resource_version()` and `notify_moira_runtime_config_change()` rather than redefining them, and apply cleanly to a fresh database.
- [ ] `GET /api/v1/admin/setup/claim-status` is unauthenticated, returns **only** `{ "claimed": bool }`, and is verified by passing tests to return `false` on a fresh database and `true` after any successful claim.
- [ ] `POST /api/v1/admin/setup/claim` is verified by passing tests to: reject a bare trusted-JWT bearer token unconditionally (401); accept a valid system-key claim; accept a valid one-time setup-token claim and reject a reused/expired/wrong-target one; enforce verified-email and the **deny-by-default** domain policy; be idempotent under `Idempotency-Key` and conflict safely (409, never duplicate) without one.
- [ ] A granted `(issuer, subject)`'s next trusted-JWT request resolves to an `Actor` carrying the granted scopes, verified by a test that performs a real `authenticate_trusted_jwt` before and after a grant and diffs `Actor.scopes`; and an ungranted actor's `Actor` is byte-identical to pre-change.
- [ ] All **seven** `/api/v1/admin/auth/providers…` operations plus `GET /api/v1/admin/setup/auth-methods` exist, are scope-gated, require `If-Match` on every mutation (checked **inside** the transaction), support `Idempotency-Key` on create, and are covered by passing e2e tests. **The frozen contract totals 10 operations.**
- [ ] **RESOLVED DECISION — no OAuth client secret in Moira (product owner `D7`, 2026-07-25).** `auth_provider_settings` has **no** `encrypted_payload` / `encryption_algorithm` / `encryption_version` / `encrypted_data_key` / `nonce` / `secret_fingerprint` / `masked_secret` column and no envelope-completeness CHECK; no request DTO has a `client_secret` field; no response contains secret material; `POST /api/v1/admin/auth/providers/{id}/rotate-secret` does not exist and is absent from the generated OpenAPI document; `src/security/crypto.rs` is **unmodified** (no `auth_provider_secret_aad`); and `auth_provider_secret_rebind_required` exists nowhere in code or catalog. Proven by `create_google_oauth_provider_stores_only_non_secret_configuration`, `auth_provider_requests_reject_a_client_secret_field`, `no_auth_provider_response_contains_secret_material`, `openapi_document_has_no_rotate_secret_operation_and_no_secret_schema_fields`, `rotate_secret_path_returns_404_because_it_does_not_exist`, and `no_auth_provider_client_secret_keys_exist_in_the_catalog`. **This preserves Moira's invariant that a decrypted secret never crosses a network boundary.**
- [ ] **D7 drift protection — Moira's obligation is met and stated.** `client_id` is returned as a non-secret field by `GET /api/v1/admin/auth/providers`, `GET …/{id}`, and `GET /api/v1/admin/setup/auth-methods`, and the plan records **explicitly** that this is sufficient for the console's fingerprint comparison so plan 08 can bind to it with no additional Moira surface. Proven by `public_auth_method_exposes_client_id` and `client_id_is_returned_by_the_read_endpoints_for_drift_comparison`.
- [ ] **Provider credentials are untouched.** `provider_credentials`, `CredentialRecord`'s `#[serde(skip_serializing)]` + `#[schema(ignore)]` envelope hiding, `SecretCipher`, and `credential_aad`/`CredentialAadParts` are **unmodified** by this plan and their existing tests still pass. D7 applies to the OAuth client secret only.
- [ ] **An auth-settings write invalidates the runtime cache on every instance through the existing Postgres `LISTEN/NOTIFY` path — proven by a test, not asserted** (CONVENTIONS §7.2).
- [ ] All three CONVENTIONS §7.3 modes are reachable from settings: `google_oauth` and `generic_oidc` via `auth_provider_settings`; `jwks` via `auth_provider_settings` linked to the **pre-existing** `trusted_jwt_issuers` surface, with **no new trust mechanism invented**.
- [ ] Any `trusted_jwt_issuers` row linked as a console issuer has `scopes_claim IS NULL`, enforced on write and asserted by a test (CONVENTIONS §7.5 — no self-asserted scopes).
- [ ] The allowed-email-domain policy lives in the **database** (`auth_provider_settings.allowed_email_domains`), not in environment variables; `AuthSettings.setup_token_ttl_seconds` is the **only** new settings field.
- [ ] The existing `GET /api/v1/admin/setup/status` endpoint, `SetupStatusResponse`, `SetupChecks`/`SetupCheckName`, `SETUP_READINESS_SQL`, and the secret-column guard test at `src/infra/repositories/setup.rs:164-179` are **unmodified** and their tests still pass.
- [ ] The existing `bootstrap-system-key` CLI is **unmodified** and documented as the break-glass root that the claim flow's system-key path depends on.
- [ ] **No Next.js / OAuth-client / session / cookie code exists in this diff** — a reviewer grep for `next`, `oauth` (outside config field names and doc text), `session`, `cookie` returns nothing new. Moira runs no OAuth flow.
- [ ] **RESOLVED DECISION — email required on both paths (product owner `D5`, 2026-07-25).** `ClaimAdminIdentityRequest.email` is `String` and `email_verified` is a `bool` with no serde default; both are marked `required` in the generated OpenAPI schema; `AdminIdentityRecord.email` is `String`. A claim omitting either field returns the full `ErrorResponse` envelope with `code: "invalid_request"` and `message_key: "moira.error.invalid_request"` (existing catalog key, verified present at `src/i18n/catalog/errors.rs:35`) — **never** axum's bare plain-text rejection. Proven by `claim_request_requires_email_and_email_verified`, `claim_body_rejection_maps_to_the_invalid_request_code`, `claim_without_an_email_is_rejected_with_a_catalogued_error`, `claim_without_email_verified_is_rejected_with_a_catalogued_error`, and `system_key_claim_without_an_email_is_rejected`. Every grant row carries a non-null email, proven by `every_granted_admin_identity_row_carries_a_non_null_email`.
- [ ] **RESOLVED DECISION — domain allow-list is deny-by-default with NO exemptions (product owner `D3`, 2026-07-25).** An unconfigured or empty `allowed_email_domains` denies every claim, on the system-key path and the setup-token path alike; the refusal is `403` with code `admin_claim_domain_not_allowed` and key `moira.error.admin_claim_domain_not_allowed`. **There is no first-claim exemption and no bootstrap bypass in the diff** — a reviewer confirms no `ClaimCredential::SystemKey` short-circuit, no "first grant" special case, no env-var or build-flag escape hatch. Proven by `claim_is_denied_by_default_when_no_domain_allow_list_is_configured`, `claim_is_denied_when_no_auth_provider_configuration_exists_at_all`, `system_key_path_is_not_exempt_from_the_domain_policy`, `system_key_credential_grants_no_policy_exemption_on_a_fresh_deployment`, and `domain_policy_is_enforced_identically_on_the_system_key_path`; and shown to be configuration rather than breakage by `claim_succeeds_once_the_operator_configures_and_enables_the_allowed_domain`.
- [ ] **Operator-facing copy exists so the deny-by-default 403 does not read as a bug** (module 10): the `POST …/setup/claim` OpenAPI description states the policy and names the endpoints that configure it; `moira.error.admin_claim_domain_not_allowed`'s catalog `description` carries the actionable setup guidance; and the `docs/` setup runbook documents the required ordering (bootstrap key → trusted issuer → auth-provider row **with** `allowed_email_domains` → enable → claim). All three land in this PR.
- [ ] **RESOLVED DECISION — `GET /api/v1/admin/setup/auth-methods` stays authenticated (product owner `D4`, 2026-07-25).** It requires `ActorType::SystemKey | TrustedJwt` **plus** `moira:setup:read`; an unauthenticated call returns **401** carrying a coded `ErrorResponse` and leaking no configuration; no anonymous variant, anonymous fallback, or `Option<Actor>` overload exists anywhere in the diff. The console's server-side system-key call still works, so the setup wizard functions. Proven by `setup_auth_methods_rejects_an_unauthenticated_call_with_401`, `setup_auth_methods_rejects_a_setup_actor_missing_the_setup_read_scope`, `setup_auth_methods_succeeds_for_a_system_key_actor`, and `claim_status_is_anonymous_while_auth_methods_is_not` (which pins the deliberate contrast with the anonymous `claim-status`).
- [ ] **Both frozen-contract changes are propagated.** This PR's description reproduces **both** ⚠️ callouts from Interfaces & Contracts — (i) **D7**: the client secret is gone from Moira, `rotate-secret` is removed, the count is **10 operations not 11**, and plan 08 owns the secret plus the `client_id`-fingerprint drift check; (ii) **D5**: `ClaimAdminIdentityRequest.email` `Option<String>` → `String`, `email_verified` loses its serde default, `AdminIdentityRecord.email` → `String` — and the coordinator has confirmed plans 08 and 09 are updated to match. Method names (`google_oauth|generic_oidc|jwks`), scope names (`moira:auth-settings:{read,write,delete}`), and every surviving path are **unchanged**.
- [ ] *(Previously-open item, resolved earlier and recorded here for continuity: whether setup-token minting is HTTP-exposed — it stays an internal service method in this plan, reachable only from a CLI-style path, mirroring `bootstrap-system-key`. A `POST /api/v1/admin/setup/claim-tokens` endpoint can be added later with no migration change, since the table already supports it.)*

**CONVENTIONS §8 compliance checklist**

- [ ] Work performed on branch `plan/07-identity-foundation`; PR opened with all required description sections (Plan link · Findings addressed · Migrations included · Breaking API/OpenAPI changes · Test evidence · Rollback procedure · Deferred follow-ups).
- [ ] All gates in CONVENTIONS §2 pass (Rust set; frontend set not applicable — this plan is backend-only).
- [ ] **Unit tests** delivered and passing (table above), running Postgres-free against repository fakes.
- [ ] **E2E tests** delivered and passing at the HTTP level against a real PostgreSQL 16 + pgvector (`tests/identity_claim.rs`, `tests/auth_provider_settings.rs`).
- [ ] Every new error and notice string has an i18n **key + English default** in the Rust catalog, mirrored into `docs/i18n-response-catalog.json`, with tests asserting presence **and** asserting the key actually reaches the client (the `AppError::coded` requirement).
- [ ] Frontend items — **not applicable** (no console code; 08 owns Next.js 16.2.11 / Node 24 / Bun 1.3.14 / Atomic Design).
- [ ] **Auth-touching:** config is runtime/DB-backed (§7.2); the OAuth client secret is **not stored in Moira at all** and lives encrypted in the console's own `console_auth` DB (§7.2 as amended by **D7**), while provider credentials remain encrypted in Moira with `SecretCipher` + AAD; no scope claim on console-issuer JWTs (§7.5); domain policy is deny-by-default (§7.5); identity binds to `(issuer, subject)`, never email (§7.5).
- [ ] No secret-leak: verified by test — setup token and system key absent from every response body, OpenAPI document, audit metadata, and log line. (No client-secret leak test is needed or possible in Moira: **D7** means Moira never receives one; that coverage belongs to plan 08 against the console's own store and bundle.)
- [ ] PR **merged** with all gates green — not merely opened.

---

## Risks & Rollback

**Security.** This is the highest-risk plan in the roadmap after 03 — it is the one place Moira grants standing admin authority to a human-controlled identity. **Decision `D7` materially reduces its blast radius**: Moira custodies no OAuth client secret, so an entire class of risk (envelope leakage, AAD misbinding, a read-back path being added under pressure, a rotation endpoint mishandling plaintext) simply does not exist here. Remaining risks and mitigations:

1. *The `auth.rs` extension becomes reachable from a non-trusted-JWT path.* Mitigated by module 7's explicit scoping and the dedicated reviewer pass (Multi-Agent Workflow), which checks reachability from all five `ActorType` branches explicitly.
2. *The claim endpoint accepts a bare JWT through a credential-resolution bug.* Mitigated by `resolve_claim_credential`'s narrow explicit allow-list — it never falls through to `authenticate_admin` — plus `bare_trusted_jwt_cannot_claim_regardless_of_its_scopes`.
3. *A setup token is replayable.* Mitigated by the `and consumed_at is null` guard at the **database** level, so even an application-logic bug cannot make it replay-safe by accident.
4. *A console issuer self-asserts scopes and bypasses the grant table.* Mitigated by module 7d's `scopes_claim IS NULL` enforcement and its test — this is CONVENTIONS §7.5's central rule, and it is enforced in Moira rather than trusted to 08.
5. *The OAuth client secret is reintroduced into Moira "for convenience".* This is the most likely regression against **D7**, because a single configuration store genuinely looks tidier to someone who has not read the rationale — and because the codebase already contains a working secret-envelope pattern next door in `provider_credentials`, which makes copying it feel like consistency rather than a decision reversal. Mitigated by: the migration comment that names D7 in the DDL itself; the structural tests `auth_provider_requests_reject_a_client_secret_field`, `openapi_document_has_no_rotate_secret_operation_and_no_secret_schema_fields`, `rotate_secret_path_returns_404_because_it_does_not_exist`, and `no_auth_provider_client_secret_keys_exist_in_the_catalog`, none of which can be made to pass while a secret path exists; and the recorded reason — **Better Auth needs the plaintext in process, and Moira's envelope is write-only, so a single store would require a read-back endpoint that breaks the "a decrypted secret never crosses a network boundary" invariant.** Anyone proposing to re-add it must first explain how the console obtains the plaintext without that read-back.
6. *The two configuration stores drift* — Moira's `client_id` changes while the console still holds the previous client's secret, producing an opaque provider error at code-exchange time. Mitigated on Moira's side by exposing `client_id` on every read path as a stable, non-secret comparison anchor (Security boundaries), and on the console's side by plan 08's mandatory fingerprint comparison, same-step wizard write, and mismatch e2e test. **Moira must not "help" by storing the console's fingerprint** — that would put a secret-derived artifact back on the wrong side of the boundary.
7. *A new error condition ships with an unresolvable `message_key`.* Mitigated by the `AppError::coded` requirement, the catalog-presence tests, and `identity_error_codes_derive_their_documented_message_keys`. This is a real trap — `AppError::Forbidden` derives `forbidden`, not the specific code — and it is exactly the failure mode the audit found eight instances of already.
8. *The unauthenticated `claim-status` route is swallowed or exempted incorrectly by plan 03's middleware.* Mitigated by Wave 0's blocking check, with an explicit instruction to stop and re-scope rather than improvise.
9. *A deny-by-default bypass is reintroduced as a "usability fix".* The most likely regression in this plan, because the symptom (a fresh deployment's first claim returning 403) genuinely looks like a bug to someone who has not read module 10. Mitigated by three things acting together: the operator-facing copy on all three surfaces required by module 10, so the 403 is self-explaining; the explicit "no first-claim exemption / no bootstrap bypass" prohibition recorded in module 10, module 8 step 7's consequence note, the frozen contract, and the Definition of Done; and the tests `claim_is_denied_when_no_auth_provider_configuration_exists_at_all` and `system_key_credential_grants_no_policy_exemption_on_a_fresh_deployment`, which cannot be made to pass by any exemption. A bypass would exist exactly during the setup window — the moment the deployment is least defended — and would be externally indistinguishable from the first-login-wins land-grab this plan exists to prevent.
10. *`GET …/setup/auth-methods` is relaxed to anonymous so the browser can call it directly.* Mitigated by the decision record in Security boundaries (the dividing line is information content: one bit of "is setup done" is free, the identity configuration is not), by `setup_auth_methods_succeeds_for_a_system_key_actor` proving the server-side path works so the wizard has no reason to reach for anonymity, and by `claim_status_is_anonymous_while_auth_methods_is_not`, which fails the moment the two endpoints' auth postures converge.

**Data-migration.** New tables only; no transformation of existing tables; no risk to existing rows. Migration rollback, if ever needed, is a new `0014_drop_identity_and_auth_settings.sql` — **never edit `0009`/`0010` in place once merged.**

**Compatibility.** Fully additive. The one shared-file behavioral change (`auth.rs`) is a byte-identical no-op for every actor without a grant, asserted explicitly. The `ActorType::SetupToken` variant changes the serialized enum's value set; any consumer that exhaustively matched `ActorType` (including in tests and in `authz.rs:119,146`) must be updated — a non-exhaustive match added to accommodate it is a review failure.

**Deployment.** Standard migrate-then-serve. The first deploy after this ships must be smoke-checked (per Deployment implications) to confirm `claimed: false` on an empty DB and a rejected bare-JWT claim attempt — proving the "no land-grab window" property holds from the first moment the code is live.

**Rollback procedure.** Layered, least-destructive first:
- (a) Remove the ten routes from `documented_router()` and redeploy — immediately closes the new HTTP attack surface with no migration rollback, leaving `admin_identities`/`auth_provider_settings` data intact for forensics. Note that `auth_provider_settings` contains **no secret material** (**D7**), so leaving it in place for forensics carries no secret-exposure cost.
- (b) Revert the `src/security/auth.rs` grant-lookup independently (it is one function call) to restore pre-07 JWT-actor resolution even if the tables remain.
- (c) Disable all auth providers via `POST …/disable` — a data-level kill switch requiring no deploy.
- (d) Full rollback (migration `0011` dropping the tables) is available but should be a last resort, since (a)-(c) already neutralize the risk without destroying audit data.

**Deferred follow-ups (explicitly out of scope, not forgotten).** A dedicated `PATCH`/`DELETE` revoke-grant endpoint (the `status`/`revoked_at` columns exist but no route sets them; an operator uses direct DB access until then). Invitation and additional-admin flows (09). Ownership transfer (09). GitHub provider (09). Minting setup tokens over HTTP rather than only via the internal service method. Key-id-based envelope rotation for `provider_credentials` (no `key_id` column today; `auth_provider_settings` is **not** part of this follow-up, since **D7** leaves it with no envelope to rotate). Wildcard/subdomain matching in the email-domain allow-list. Applying module 11's `JsonRejection` → `invalid_request` mapping to the pre-existing admin handlers that take a bare `Json<T>` (e.g. `src/http/admin.rs:1414`; the cited `:112,188,333` are stale, though the class of handler still exists) and therefore still emit axum's uncatalogued plain-text rejection in violation of CONVENTIONS §4 — a pre-existing, repo-wide gap this plan closes only for its own new endpoint, since fixing it everywhere would violate the "pure iteration" constraint.

~~Fixing the pre-existing read-then-compare TOCTOU in the trusted-JWT-issuer handlers.~~ **Already
done — plan 06b (`46c2c74`) made all 33 admin mutation sites atomic and deleted `ensure_version`
(§0.1 B6).** Nothing is deferred here.
