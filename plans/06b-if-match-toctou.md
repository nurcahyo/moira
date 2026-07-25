# Plan 06b — Close the `If-Match` TOCTOU on the admin write surface

> **Binding cross-cutting spec:** `plans/CONVENTIONS.md`. Where anything below conflicts with that file, **CONVENTIONS.md wins**.

> **Status: inventory + recipe + failing harness only.** This document is the deliverable of
> **plan 06 module 17**. Module 17 delivers *evidence*, not the fix. The fix is this plan, and it
> has not started.

> **Derived 2026-07-26 from the tree at `ec00a32`** (branch `plan/06-architecture-test-hygiene`,
> after the `src/application/admin.rs` split into `src/application/admin/`). Every line number,
> count, and method name below was re-derived at that commit. Plan 06 §0 row 2 was written against
> `0b3301c` and its *counts* still hold; two of its structural claims do not — see §6.

---

## 1. The defect

`src/http/admin.rs` validates `If-Match` like this, at 33 sites:

```rust
let service = AdminService::new(&state)?;
ensure_version(
    service.get_application(&actor, id).await?.version,   // read  — one pooled connection
    require_if_match(&headers)?,
)?;
let record = service.patch_application(&actor, &ctx, id, request).await?;  // write — another
```

`ensure_version` (`src/http/admin.rs:92`) compares two `i64`s in Rust. The version it compares
came from a *separate* statement on a *separate* pooled connection, and nothing carries the
caller's expectation into the `UPDATE`. `PgAdminRepository::patch_application`
(`src/infra/repositories/admin.rs:866`) is an unconditional
`update applications … where id = $1 and deleted_at is null`.

Consequence: the precondition rejects only versions that were **already stale when the request
arrived**. Two writers that each read the same currently-valid version both pass `ensure_version`
and both write. The first writer's values are overwritten, the version advances twice, and both
callers receive `200 OK`. Nothing anywhere reports a conflict.

This is a genuine lost update, not a theoretical one. `tests/if_match_toctou_harness.rs` reproduces
it end to end through the real HTTP surface; see §5 for the observed transcript.

### Severity

Reachable by any authenticated admin caller on 33 of the 35 versioned admin mutations. It is a
correctness bug rather than a privilege bug — an attacker gains no access they did not already
have — but every admin console built on these endpoints will silently drop concurrent edits, and
`If-Match` is the only concurrency control the API offers.

---

## 2. Verified inventory

**35 versioned mutation sites in `src/http/admin.rs`. 2 already correct. 33 remaining.**

Reproduce the count:

```bash
grep -c '^    ensure_version($' src/http/admin.rs     # → 33
grep -n 'require_if_match' src/http/admin.rs          # → 33 + 2 fixed + 2 helper definitions
```

Of the 33: **21** reach the database through `AdminService`, **12** through `RuntimeAdminService`.
This confirms plan 06 §0 row 2 and the 33-of-35 split exactly.

### 2.1 Already correct — the two worked examples

| line | route | handler | service | repository write | shape |
|---|---|---|---|---|---|
| `src/http/admin.rs:902` | `POST /api/v1/admin/provider-credentials/{id}/rotate` | `rotate_credential` | `AdminService::rotate_credential` (`src/application/admin/credentials.rs:194`) | `PgAdminCommandTransaction::rotate_credential` (`src/infra/repositories/admin.rs:444`) | `select … for update` + compare, inside the command transaction |
| `src/http/admin.rs:335` | `PUT /api/v1/admin/applications/{id}/execution-policy` | `put_application_execution_policy` | `PublicExecutionService::put_application_execution_policy` (`src/application/public.rs:838`) | `PgPublicRepository::put_application_execution_policy` (`src/infra/repositories/public.rs:247`) | explicit `tx` + `select … for update` + `where … and version = $22` |

### 2.2 The 21 `AdminService` sites

`AdminService` facade methods live in `src/application/admin/mod.rs`; each delegates to a
per-family service. The `enable`/`disable` handler pairs share one service method
(`set_*_enabled`), so 21 call sites map to **16 distinct service methods** and **16 distinct
repository methods**.

| # | `admin.rs` line | route | handler | `AdminService` method (`admin/mod.rs`) | delegate | repository method needing the predicate |
|---|---|---|---|---|---|---|
| 1 | 199 | `PATCH /api/v1/admin/applications/{id}` | `patch_application` | `:120` | `admin/applications.rs:105` | `AdminRepository::patch_application` — trait `admin.rs:105`, impl `:866` |
| 2 | 228 | `DELETE /api/v1/admin/applications/{id}` | `delete_application` | `:132` | `admin/applications.rs:138` | `soft_delete_application` — trait `:115`, impl `:919` |
| 3 | 257 | `POST /api/v1/admin/applications/{id}/enable` | `enable_application` | `:141` | `admin/applications.rs:160` | `set_application_status` — trait `:110`, impl `:895` |
| 4 | 288 | `POST /api/v1/admin/applications/{id}/disable` | `disable_application` | `:141` | `admin/applications.rs:160` | `set_application_status` — same |
| 5 | 449 | `PATCH /api/v1/admin/providers/{id}` | `patch_provider` | `:174` | `admin/providers.rs:123` | `patch_provider` — trait `:129`, impl `:995` |
| 6 | 478 | `DELETE /api/v1/admin/providers/{id}` | `delete_provider` | `:184` | `admin/providers.rs:155` | `soft_delete_provider` — trait `:137`, impl `:1050` |
| 7 | 507 | `POST /api/v1/admin/providers/{id}/enable` | `enable_provider` | `:193` | `admin/providers.rs:176` | `set_provider_status` — trait `:135`, impl `:1026` |
| 8 | 536 | `POST /api/v1/admin/providers/{id}/disable` | `disable_provider` | `:193` | `admin/providers.rs:176` | `set_provider_status` — same |
| 9 | 628 | `PATCH /api/v1/admin/provider-models/{id}` | `patch_provider_model` | `:228` | `admin/providers.rs:272` | `patch_provider_model` — trait `:151`, impl `:1112` |
| 10 | 681 | `DELETE /api/v1/admin/provider-models/{id}` | `delete_provider_model` | `:248` | `admin/providers.rs:304` | `soft_delete_provider_model` — trait `:162`, impl `:1177` |
| 11 | 712 | `POST /api/v1/admin/provider-models/{id}/enable` | `enable_provider_model` | `:259` | `admin/providers.rs:325` | `set_provider_model_status` — trait `:157`, impl `:1153` |
| 12 | 743 | `POST /api/v1/admin/provider-models/{id}/disable` | `disable_provider_model` | `:259` | `admin/providers.rs:325` | `set_provider_model_status` — same |
| 13 | 849 | `PATCH /api/v1/admin/provider-credentials/{id}` | `patch_credential` | `:309` | `admin/credentials.rs:162` | `patch_credential` — trait `:185`, impl `:1309` |
| 14 | 878 | `DELETE /api/v1/admin/provider-credentials/{id}` | `delete_credential` | `:355` | `admin/credentials.rs:335` | `soft_delete_credential` — trait `:203`, impl `:1429` |
| 15 | 938 | `POST /api/v1/admin/provider-credentials/{id}/enable` | `enable_credential` | `:343` | `admin/credentials.rs:303` | `set_credential_status` — trait `:197`, impl `:1382` |
| 16 | 969 | `POST /api/v1/admin/provider-credentials/{id}/disable` | `disable_credential` | `:343` | `admin/credentials.rs:303` | `set_credential_status` — same |
| 17 | 1057 | `DELETE /api/v1/admin/users/{external_user_id}/provider-credentials/{id}` | `delete_user_credential` | `:364` | `admin/credentials.rs:360` | `soft_delete_user_credential` — trait `:204`, impl `:1439` |
| 18 | 1461 | `PATCH /api/v1/admin/jwt-issuers/{id}` | `patch_trusted_jwt_issuer` | `:483` | `admin/jwt_issuers.rs:102` | `patch_trusted_jwt_issuer` — trait `:263`, impl `:1709` |
| 19 | 1492 | `DELETE /api/v1/admin/jwt-issuers/{id}` | `delete_trusted_jwt_issuer` | `:518` | `admin/jwt_issuers.rs:220` | `soft_delete_trusted_jwt_issuer` — trait `:274`, impl `:1806` |
| 20 | 1544 | `POST /api/v1/admin/jwt-issuers/{id}/enable` | `enable_trusted_jwt_issuer` | `:495` | `admin/jwt_issuers.rs:130` | `set_trusted_jwt_issuer_status` — trait `:268`, impl `:1759` |
| 21 | 1575 | `POST /api/v1/admin/jwt-issuers/{id}/disable` | `disable_trusted_jwt_issuer` | `:495` | `admin/jwt_issuers.rs:130` | `set_trusted_jwt_issuer_status` — same |

Entities touched: `applications`, `providers`, `provider_models`, `provider_credentials`,
`trusted_jwt_issuers`. All five carry a `version` column with a `BEFORE UPDATE` bump trigger
(`migrations/0004_admin_api_contract.sql:21`, triggers from `:26`).

`AdminRepository` has exactly **one** implementor (`PgAdminRepository`,
`src/infra/repositories/admin.rs:800`), so each of the 16 methods costs one trait signature and
one impl.

### 2.3 The 12 `RuntimeAdminService` sites

21 call sites' worth of handler shape, but only **9 distinct service methods** and **9 distinct
repository methods**.

| # | `admin.rs` line | route | handler | `RuntimeAdminService` method (`src/application/runtime_admin.rs`) | repository method needing the predicate |
|---|---|---|---|---|---|
| 22 | 1724 | `PATCH /api/v1/admin/routes/{id}` | `patch_route_definition` | `:125` | `RuntimeRepository::patch_route_definition` — trait `runtime.rs:125`, Pg `:338`, InMemory `:1582` |
| 23 | 1755 | `DELETE /api/v1/admin/routes/{id}` | `delete_route_definition` | `:156` | `soft_delete_route_definition` — trait `:137`, Pg `:397`, InMemory `:1598` |
| 24 | 1784 | `POST /api/v1/admin/routes/{id}/enable` | `enable_route_definition` | `:176` | `set_route_definition_status` — trait `:131`, Pg `:373`, InMemory `:1590` |
| 25 | 1815 | `POST /api/v1/admin/routes/{id}/disable` | `disable_route_definition` | `:176` | `set_route_definition_status` — same |
| 26 | 1920 | `PATCH /api/v1/admin/routing-policies/{id}` | `patch_routing_policy` | `:278` | `patch_routing_policy` — trait `:160`, Pg `:477`, InMemory `:1630` |
| 27 | 1951 | `DELETE /api/v1/admin/routing-policies/{id}` | `delete_routing_policy` | `:329` | `soft_delete_routing_policy` — trait `:172`, Pg `:566`, InMemory `:1646` |
| 28 | 1980 | `POST /api/v1/admin/routing-policies/{id}/enable` | `enable_routing_policy` | `:351` | `set_routing_policy_status` — trait `:166`, Pg `:538`, InMemory `:1638` |
| 29 | 2011 | `POST /api/v1/admin/routing-policies/{id}/disable` | `disable_routing_policy` | `:351` | `set_routing_policy_status` — same |
| 30 | 2116 | `PATCH /api/v1/admin/agent-profiles/{id}` | `patch_agent_profile` | `:456` | `patch_agent_profile` — trait `:194`, Pg `:645`, InMemory `:1677` |
| 31 | 2147 | `DELETE /api/v1/admin/agent-profiles/{id}` | `delete_agent_profile` | `:491` | `soft_delete_agent_profile` — trait `:206`, Pg `:708`, InMemory `:1693` |
| 32 | 2176 | `POST /api/v1/admin/agent-profiles/{id}/enable` | `enable_agent_profile` | `:513` | `set_agent_profile_status` — trait `:200`, Pg `:683`, InMemory `:1685` |
| 33 | 2207 | `POST /api/v1/admin/agent-profiles/{id}/disable` | `disable_agent_profile` | `:513` | `set_agent_profile_status` — same |

Entities: `route_definitions`, `routing_policies`, `agent_profiles`.

`RuntimeRepository` has **two** implementors (`PgRuntimeRepository` `:260`,
`InMemoryRuntimeRepository` `:1508`), so each of the 9 methods costs one trait signature and
**two** impls. The in-memory implementor must reject a version mismatch with the same
`resource_version_conflict` error or the unit layer will not exercise the new branch.

### 2.4 Totals for sizing plan 06b

| | `AdminService` | `RuntimeAdminService` | total |
|---|---|---|---|
| HTTP handler edits (`src/http/admin.rs`) | 21 | 12 | **33** |
| distinct service signatures gaining `expected_version: i64` | 16 | 9 | **25** |
| distinct repository trait signatures | 16 | 9 | **25** |
| repository implementations | 16 | 18 | **34** |
| new e2e concurrency tests (one per resource family) | 5 | 3 | **8** |

The "8 tests" figure is the *minimum*. One race test per resource family proves the SQL-level
check on each table; per-verb coverage (patch / delete / enable / disable) would be 33 and is the
alternative if reviewers want site-level evidence.

---

## 3. The recipe

Written from the two sites that are already correct. Follow it literally; it is intended to be
mechanical.

### 3.1 Handler (`src/http/admin.rs`)

Replace

```rust
    let service = AdminService::new(&state)?;
    ensure_version(
        service.get_application(&actor, id).await?.version,
        require_if_match(&headers)?,
    )?;
    let record = service.patch_application(&actor, &ctx, id, request).await?;
```

with

```rust
    let expected_version = require_if_match(&headers)?;
    let record = AdminService::new(&state)?
        .patch_application(&actor, &ctx, id, expected_version, request)
        .await?;
```

This is exactly `rotate_credential` (`src/http/admin.rs:902-915`). The pre-read disappears; the
handler no longer decides anything.

Keep `ensure_version` (`:92`) until the last site is converted, then delete it and prove
`grep -rn 'ensure_version' src/` returns nothing.

### 3.2 Service

Add `expected_version: i64` to the facade method in `src/application/admin/mod.rs` and to the
delegate in the per-family file, and pass it straight through to the repository. For methods that
already build an `AdminCommandSpec` — only `rotate_credential` among the versioned mutations —
also thread it into the replay key with
`.with_expected_version(Some(expected_version))` (`src/application/admin_command.rs:96`), so a
replayed command cannot be satisfied by a differently-versioned earlier one.

For `RuntimeAdminService` the same edit applies at `src/application/runtime_admin.rs`.

### 3.3 Repository — the part that actually fixes it

The comparison must happen **inside the same transaction as the write**. Two accepted shapes,
both already in the tree:

**Shape A — lock then compare** (`PgAdminCommandTransaction::rotate_credential`,
`src/infra/repositories/admin.rs:444-497`). Preferred when the method already runs inside a
transaction:

```rust
let current_version = sqlx::query_scalar::<_, i64>(
    "select version from applications where id = $1 and deleted_at is null for update",
)
.bind(id)
.fetch_optional(self.connection())
.await?
.ok_or_else(|| AppError::NotFound(format!("application {id}")))?;
if expected_version.is_some_and(|expected| expected != current_version) {
    return Err(AppError::conflict(
        "resource_version_conflict",
        "resource version does not match If-Match",
    ));
}
// … the existing UPDATE, unchanged …
```

**Shape B — lock, compare, *and* predicate**
(`PgPublicRepository::put_application_execution_policy`,
`src/infra/repositories/public.rs:257-353`). Preferred when the method currently runs on the pool
with no transaction, which is the case for **all 25** methods in §2.2 and §2.3:

```rust
let mut tx = self.pool.begin().await?;
let current_version = sqlx::query_scalar::<_, i64>(
    "select version from applications where id = $1 and deleted_at is null for update",
)
.bind(id)
.fetch_optional(&mut *tx)
.await?
.ok_or_else(|| AppError::NotFound(format!("application {id}")))?;
if expected_version.is_some_and(|expected| expected != current_version) {
    return Err(AppError::conflict(
        "resource_version_conflict",
        "resource version does not match If-Match",
    ));
}
let row = sqlx::query(
    r#"update applications set … where id = $1 and deleted_at is null and version = $N returning …"#,
)
… .fetch_optional(&mut *tx).await?
  .ok_or_else(|| AppError::conflict(
        "resource_version_conflict",
        "resource version does not match If-Match",
   ))?;
let record = application_record_from_row(&row)?;
tx.commit().await?;
Ok(record)
```

The `for update` row lock is what closes the window; the `and version = $N` predicate is belt and
braces and makes the SQL self-documenting.

**Do not** take the tempting one-line shortcut of appending `and version = $N` to the existing
`UPDATE` and leaving everything else alone. Every one of these methods currently ends in

```rust
.ok_or_else(|| AppError::NotFound(format!("application {id}")))?
```

so a version mismatch would surface as **`404`, not `409`** — a silent wire-contract change, and
one that leaks "this row does not exist" for a row that does. If the shortcut is used anyway, the
zero-row branch must first probe existence and only then choose between `NotFound` and `conflict`.

### 3.4 Error contract — unchanged, deliberately

The conflict is the *same* `AppError::conflict("resource_version_conflict", …)` the handler emits
today via `ensure_version`, so:

- status stays `409`;
- `code` / `message_key` stay `resource_version_conflict` / `moira.error.resource_version_conflict`
  — already in the catalog, so plan 06b adds **no** i18n key (CONVENTIONS §4);
- `docs/openapi.json` must diff **empty**; no route, DTO, parameter, or status changes. Verify,
  do not assume:
  `cargo test --lib http::tests::committed_openapi_matches_the_generated_document`.

The single observable behaviour change is the one that is the point: two racing writers now
produce one `200` and one `409` instead of two `200`s.

### 3.5 Per-site test

Use the `tokio::sync::Barrier` house style. `tests/execution_policy_if_match.rs` is the complete
worked example (four sequential precondition tests plus one race), and
`tests/if_match_toctou_harness.rs` is the same race pre-written for two of the unfixed sites. No
`sleep()` — CONVENTIONS §3.

---

## 4. Sequencing and dependencies

**`06 → 06b → 07`.** Both edges are load-bearing:

- **After 06.** Plan 06 module 6's Definition of Done requires `AdminService`'s 46 public
  signatures to be *provably unchanged*, verified by a before/after snapshot. This plan changes 16
  of them. The two claims cannot both stand in one PR, and folding them together destroys the one
  mechanical control that makes plan 06's 2,436-line refactor reviewable. Plan 06 also splits
  `admin.rs` into six per-family files, so the 16 service edits land in small focused diffs
  instead of one enormous one.
- **Before 07.** Plan 07 adds an `AdminIdentityService` slice modelled on `AdminService`. Written
  against the current pattern it inherits the TOCTOU, and the inventory grows.
- **Module 16 (`actor_fingerprint` unification) before the 12 runtime-admin sites.** Those routes
  currently collide two actors that differ only by trusted-JWT issuer or tenant, and the
  fingerprint also feeds `advisory_lock_key` (`src/infra/repositories/admin.rs:1934`). A version
  predicate inside a replay-checked write needs the replay key to isolate actors correctly first.
- **Module 13 (per-fixture databases) before the concurrency tests.** Already landed at `ec00a32`:
  every `LifecycleFixture` owns a private migrated database, so raced writers contend only with
  each other and PostgreSQL advisory locks do not span suites.

Suggested wave split for 06b, by disjoint files:

1. `applications` + `providers` + `provider_models` (`admin/applications.rs`, `admin/providers.rs`, `admin.rs` repo)
2. `provider_credentials` + `jwt_issuers` (`admin/credentials.rs`, `admin/jwt_issuers.rs`)
3. `routes` + `routing_policies` + `agent_profiles` (`runtime_admin.rs`, `repositories/runtime.rs`)
4. `src/http/admin.rs` handler sweep + delete `ensure_version` — **last, single agent**, because
   all 33 edits are in one file.

---

## 5. The failing harness — committed evidence

`tests/if_match_toctou_harness.rs`, two `#[ignore]`d tests, one per owning service:

| test | site proved |
|---|---|
| `concurrent_application_patches_with_the_same_version_must_yield_one_success_and_one_409` | `patch_application` — `AdminService` → `PgAdminRepository` |
| `concurrent_route_patches_with_the_same_version_must_yield_one_success_and_one_409` | `patch_route_definition` — `RuntimeAdminService` → `PgRuntimeRepository` |

Each reads the current version over HTTP, forms two `PATCH`es carrying that *same, valid*
`If-Match`, releases them through a `tokio::sync::Barrier` on a multi-threaded runtime, and
asserts `(successes, conflicts) == (1, 1)` plus that the surviving row is the winner's at exactly
`version + 1`.

Run:

```bash
export MOIRA_TEST_DATABASE_URL='postgres://postgres:postgres@127.0.0.1:5432/moira'
cargo test --test if_match_toctou_harness -- --ignored --nocapture
```

Observed at `ec00a32` — both tests fail, both with `(2, 0)`:

```text
thread 'concurrent_route_patches_with_the_same_version_must_yield_one_success_and_one_409' panicked at
tests/if_match_toctou_harness.rs:242:5:
assertion `left == right` failed: two writers holding the same valid If-Match must resolve to
exactly one 200 and one 409 `resource_version_conflict`. …
Observed: [
  "display_name=\"toctou-route-a\" status=200 OK body={… \"display_name\":\"toctou-route-a\", … \"version\":3}",
  "display_name=\"toctou-route-b\" status=200 OK body={… \"display_name\":\"toctou-route-b\", … \"version\":2}",
]
  left: (2, 0)
 right: (1, 1)

thread 'concurrent_application_patches_with_the_same_version_must_yield_one_success_and_one_409' panicked at
tests/if_match_toctou_harness.rs:242:5:
… Observed: [
  "display_name=\"toctou-application-a\" status=200 OK body={… \"version\":3}",
  "display_name=\"toctou-application-b\" status=200 OK body={… \"version\":2}",
]
  left: (2, 0)
 right: (1, 1)

test result: FAILED. 0 passed; 2 failed
```

Read the two response bodies: both writers were told `200`, the version went `1 → 2 → 3` off a
single `If-Match: 1`, and only one `display_name` survives. That is the lost update, observed.

**Plan 06b's definition of done includes deleting both `#[ignore]` attributes and this suite
passing unmodified.** Do not weaken the assertions.

---

## 6. Corrections to plan 06 module 17

Module 17 was written against `0b3301c`. Its counts survive re-derivation at `ec00a32`; two
structural claims do not.

| plan 06 §17 says | verified at `ec00a32` | why it matters |
|---|---|---|
| "33 sites … 21 `AdminService`, 12 `RuntimeAdminService`; two already fixed" | **Confirmed exactly.** | — |
| §17.2(4): "21 sites go through `AdminService` → `PgAdminRepository` → the transactional `AdminCommandRunner` envelope" | **None of the 16 mutation methods behind those 21 sites uses `AdminCommandRunner`.** Only the *create* methods and `rotate_credential` do. All 16 are bare `self.repo.<method>()` calls on the pool, followed by a separate `audit_success` — no transaction at all. | The fix is *larger* than "add a predicate": 25 repository methods must acquire an explicit `tx` (recipe shape B), and the write plus its audit row are currently not atomic either. |
| §17.2(1): the fix "adds an `expected_version: i64` parameter to roughly **21** of those 46 signatures" | **16** distinct `AdminService` signatures (21 *call sites*; `enable`/`disable` pairs share `set_*_enabled`). Plus **9** on `RuntimeAdminService`. | Sizing. The conclusion — that it contradicts module 6's "46 signatures unchanged" DoD — is unaffected. |
| §17.3: "add the version predicate to the `UPDATE`/`DELETE`'s `WHERE` clause … a zero-row result becomes `AppError::conflict`" | Correct as far as it goes, but every one of these methods currently maps zero rows to `AppError::NotFound`. Taken literally the recipe turns a stale `If-Match` into a **404**. | See §3.3. This is the single most likely way to get 06b wrong.
| §17.1: harness file named `tests/if_match_atomicity.rs` | Delivered as **`tests/if_match_toctou_harness.rs`**. | Naming only; noted so a grep for the plan's name does not come back empty. |

---

## 7. Definition of Done for plan 06b

- [ ] All 33 handlers in `src/http/admin.rs` pass `expected_version` down; no handler performs a pre-read for version purposes.
- [ ] `grep -rn 'ensure_version' src/` returns nothing, and the helper is deleted.
- [ ] 25 service signatures and 25 repository trait signatures carry `expected_version`; all 34 implementations (including `InMemoryRuntimeRepository`) enforce it.
- [ ] Every enforcing repository method compares the version inside the same transaction as the write, via recipe shape A or B — no in-Rust comparison against a separately-fetched version survives.
- [ ] A stale `If-Match` still yields `409 resource_version_conflict`; a missing one still yields `400 if_match_required`; a genuinely absent row still yields `404`. Pinned by tests, not by inspection.
- [ ] `tests/if_match_toctou_harness.rs` passes with both `#[ignore]` attributes removed and no assertion changed.
- [ ] ≥ 8 e2e race tests, one per resource family, all `Barrier`-gated; no `sleep()` anywhere (CONVENTIONS §3).
- [ ] `docs/openapi.json` diffs empty, proven by running the drift gate.
- [ ] No new i18n key (CONVENTIONS §4) — verified, not assumed.
- [ ] Gates green on the merged commit: `cargo fmt --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace --all-features` (DB suites **ran**, not skipped), `cargo build --release --locked`, `cargo deny check`.
