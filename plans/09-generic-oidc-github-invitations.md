# Plan 09 — Generic OIDC Hardening, GitHub Sign-In, Invitations & Additional Admins (Post-MVP)

> **Compliance note.** Written against `plans/CONVENTIONS.md` (verified 2026-07-25), which is authoritative and overrides any earlier draft of this file. The corrections CONVENTIONS forced into this revision are: (1) the console's identity layer is **Better Auth**, not Auth.js/NextAuth — plan 08 already ships Google *and* a generic-OIDC baseline via the **`genericOAuth` plugin**, so this plan's OIDC work is **hardening and operator-facing management**, not "add generic OIDC for the first time"; (2) **Atomic Design** file paths (§6) replace the previous `app/**/components/` layout; (3) auth config is **runtime, DB-backed in Moira** (§7.2), so this plan's multi-provider work writes into Moira's auth settings rather than into env vars; (4) **product-owner decision D7** (CONVENTIONS §0 and its "D7 consequences (binding)" subsection) — **each provider's OAuth client secret is owned by the console and stored in the console's own database; Moira never stores one and never returns one** — which reshapes this plan's multi-provider model into a **per-provider dual write with a per-provider drift check** and moves secret rotation entirely into the console; (5) the pinned toolchain (§5), mandatory unit+e2e+a11y+secret-leak testing (§3), i18n (§4), and the branch/PR/DoD rules (§1, §2, §8) apply here exactly as they do to 08.
>
> **D7 as it applies to N providers.** Plan 08 establishes the model for one provider; this plan generalises it without changing it. For **every** provider — Google, GitHub, and each generic-OIDC entry — the **non-secret config lives in Moira's `auth_provider_settings`** (issuer, discovery/authorization/token/userinfo/JWKS URLs, `client_id`, scopes, `allowed_email_domains`, `hosted_domain`, `required_org`, algorithms, audiences, redirect URIs, `enabled`, `version`) and the **client secret lives encrypted at rest in the console's own `console_auth.authProviderSecret` table**, one row per provider, each with **its own `client_id` fingerprint**. Consequences that are binding here: (a) **no Moira read-back** — the rejected option stays rejected, per-provider and in aggregate; (b) **no `rotate-secret` endpoint exists** — rotation is a console operation, per provider; (c) Moira's auth-provider read endpoints carry **no secret material at all**, so a multi-provider list response is entirely non-secret; (d) the **per-provider dual write and per-provider drift check** are mandatory, since N providers mean N independent ways for the two stores to diverge. Every passage in this file that previously said "secrets live in Moira, encrypted with `SecretCipher`" was a pre-D7 artefact and has been rewritten.
>
> ⚠️ **The compliance note above is itself stale in two places.** Plan 08 shipped **no** `socialProviders.google` and **no** second provider entry — exactly one `genericOAuth` config entry exists, and `google_oauth` is only a *method value* routed through that same plugin. And the `console_auth.authProviderSecret` table does not exist: the console's secret store is an **in-memory `Map`**. See **§0** below, which wins on conflict.

---

## §0 — Wave 0: drift against the tree (audit 2026-07-31, HEAD `0b79502`)

**Read this section before any other.** The body of this plan was written against a tree in which
plans 07 and 08 had both *fully* shipped. Plan 07 shipped. **Plan 08 shipped only its lib layer, its
atoms, two molecules and its harnesses** — no organisms, no console route group, no durable database.
An audit re-checked every structural claim in this file against the working tree.

The rule from plans 06 and 07 applies again: **where §0 and the body disagree, §0 wins.** The body is
left in place because most of its *design* is still sound. What rotted here is not line numbers — it
is the **premises**. Three of this plan's four headline features are specified as extensions of code
that was never written, and one of them (session management) cannot exist at all against the store
the console actually has.

**The one-paragraph version.** Waves 2 and 3 are **greenfield, not extension**. Durable console
storage is a **prerequisite this plan owns**, not an assumption it inherits. `moira:admins:manage`
cannot be an explicit, never-implied scope without changing the authorization core, so ownership
becomes **row state**. `admin-invites/{preview,redeem}` stay **under** the admin prefix. Migrations
start at **`0017`**. Finding **F15** is a hard prerequisite and is fixed elsewhere, before this plan
implements.

### §0.1 Blockers — these make the plan-as-written unimplementable or ineffective

| # | Body says | Reality (verified at HEAD) | Required change |
|---|---|---|---|
| **B1** | `moira:admins:manage` is "deliberately checked as an **explicit** scope (**not** implied by `moira:admin`)" (`:504`, `:596`, `:612`) | **There is no per-scope opt-out anywhere in Moira.** `AuthorizationService::has_scope` (`src/security/authz.rs:148-152`) is `scopes.contains(required) \|\| (admin_scope_implies_all(actor) && scopes.contains(ADMIN_SCOPE))`, and `ADMIN_IMPLYING_ACTOR_TYPES` (`:138-142`) contains `TrustedJwt`. `ClaimAdminIdentityRequest.scopes` defaults to `["moira:admin"]` (`src/domain/identity.rs:58-59,100`), `admin_identities.granted_scopes` defaults to `array['moira:admin']` (`migrations/0012:34`), and the grant is unioned onto the trusted-JWT actor (`src/security/auth.rs:334, 920`). **Every admin Moira can currently grant already satisfies `moira:admins:manage`.** The ownership model, the last-primary guard and `authorization-denial.e2e.ts`'s "holds `moira:admin` but not `moira:admins:manage`" case are all decorative as specified | See **D1** below. Ownership becomes **row state on `admin_identities`**, not a scope |
| **B2** | Fifteen console artefacts are "reused from plan 08, not re-implemented" | **`console/modules/README.md` says "Nothing lives here yet."** There is no `(console)` route group, no `/login`, no `/settings/auth`, no wizard UI, no `middleware.ts`. `console/app/` contains only `layout.tsx`, `page.tsx`, `api/auth/[...all]/route.ts`, `api/health/route.ts`. **None of these exist:** `lib/provider-secrets.ts`, `OnceOnlySecretModal`, `SignInPanel`, `ProviderSecretRotatePanel`, `ProviderDriftBanner`, `lib/i18n/catalog.en.ts`, `lib/i18n/moira-keys.ts`, `layer-dependencies.test.ts`, `no-secret-props.test.ts`, `no-hardcoded-copy.test.tsx`, `i18n-catalog-coverage.test.ts`, `console/tests/secret-leak/bundle-scan.test.ts`, `docs/admin-console.md`. Shipped instead: `lib/{auth,auth-config,auth-runtime,console-secrets,env,errors,moira-client,moira-keys,moira-session,setup-flow,types}.ts`, five atoms (`Badge,Button,Input,Label,Spinner`), two molecules (`FormField`, `StatusBadgeGroup`), `architecture.test.ts` + `tests/unit/architecture/server-only-{guards,import}.test.ts`, `e2e/{a11y,secret-leak,smoke}.e2e.ts`, and `docs/console-architecture.md` | **Re-scope Waves 2 and 3 as greenfield.** Every "reused from plan 08" claim in this file is false and must be re-read as "built here". See §0.4 |
| **B3** | "Better Auth **DB-backed sessions** (plan 08 gave the console its own `console_auth` schema), which makes the 'active sessions' screen and true remote sign-out **genuinely implementable**" (`:21`, `:32`, `:179`, `:328`) | **The console is on `memoryAdapter`.** `console/lib/auth.ts:59,203` under a `DELIBERATE SCOPE LIMIT` header (`:162-176`) that states the jwt plugin's ES256 key pair "is regenerated on every process start", making the path "single-replica and restart-sensitive". `console-secrets.ts:198-220` is an `InMemoryConsoleSecretStore` backed by a `Map` under the same header. `charts/moira-console/values.yaml:55` pins `replicaCount: 1` and disables the PDB and HPA for that reason. **There is no `console_auth` database, no `session` table to list or revoke, and no `authProviderSecret` table** | See **D2** below. Durable console storage becomes **Wave 1** of this plan, or session management is cut |
| **B4** | Multiple simultaneous enabled providers, N sign-in buttons, "a drifted GitHub entry must not take generic-OIDC sign-in down with it" (`:19`, `:246`, `:258-260`) | **The shipped console refuses to run with more than one enabled provider.** `console/lib/auth-config.ts:191-198` returns `fail("ambiguous_enabled_providers")` when `enabled.length > 1`, deliberately: *"Moira permits several enabled rows and picks one by a documented ordering at claim time. The console refuses to guess."* There is one `CONSOLE_OAUTH_PROVIDER_ID = "moira-console-idp"` constant (`:98`), one `genericOAuth` config entry (`lib/auth.ts:231-251`), and `readIdpSubject` filters accounts by that single `providerId`. **Enabling a second provider today breaks console sign-in outright** | Multi-provider is a **redesign of a shipped, deliberate safety decision**, not an extension. This plan owns removing `ambiguous_enabled_providers`, minting per-provider `providerId`s, and re-deriving the callback-URL and `readIdpSubject` contracts. Budget it as such |
| **B5** | `governing_policy` "matches on `issuer`, or via `trusted_jwt_issuer_id`" is satisfied by the redeem body's issuer | Verified `src/infra/repositories/auth_settings.rs:365-388`: `where … and (issuer = $1 or trusted_jwt_issuer_id = $2) order by (issuer is not distinct from $1) desc, created_at asc, id asc limit 1`. `$1` is the **caller's** issuer (the console); the provider row's `issuer` column holds the **IdP's**. So the row matches **only** via `trusted_jwt_issuer_id`. **`trusted_jwt_issuer_id` appears nowhere in this plan** — every redemption would 403 forever | The redeem path must resolve the console's `trusted_jwt_issuer_id` exactly as `claim` does (`src/application/identity.rs:137-147` → `resolve_active_issuer`) and pass it. **Mitigating, and a correction to the audit that raised this:** plan 08 *did* ship the read-side guard — `auth-config.ts:207-213` fails `provider_not_bound_to_trusted_jwt_issuer` for an unbound row, naming the defect in-code. The gap is in **this plan's Moira surface**, not in the console |
| **B6** | GitHub is stored in `auth_provider_settings`; the multi-provider migration "adds **non-secret columns only**" and is conditional on D-1's shape (`:69`, `:71`, `:175`, `:448`) | `migrations/0013:23-24` — `check (method in ('google_oauth','generic_oidc','jwks'))`. GitHub OAuth is **not OIDC** and cannot be `generic_oidc`: `auth_provider_settings_method_shape` (`:57-64`) requires `issuer is not null or discovery_url is not null` for that method. There is **no `provider_id` column and no `required_org` column**; `metadata jsonb` is the only home and this plan never names it. Worse, `auth_provider_settings_method_issuer_active_unique` is on `(method, (coalesce(issuer,'')))` — so **at most one row per method can have a null issuer**, which is exactly GitHub's shape | The migration is **unconditional** and must **drop and re-add two CHECK constraints** (`method`, `auth_provider_settings_method_shape`) plus the unique index. That is not "non-secret columns only" — say so, and decide explicitly whether GitHub gets a `github_oauth` method value or a `provider_id` key |
| **B7** | `POST /api/v1/admin-invites/{preview,redeem}` sit **outside** `/api/v1/admin/*`, "per 07's non-admin-credential path precedent" (`:187`, `:490`) | **07's precedent is the opposite, and it is written into the code as a prohibition.** `src/http/identity.rs:12-20`: *"Do **not** 'fix' the spec omission by moving the path out from under the admin prefix — that would also move it out of the admin-strip protection covering the other nine operations this plan adds, and out of the admin body-limit and timeout layers."* The anonymous `claim-status` stays **under** the prefix for exactly that reason (`src/http/mod.rs:471-482`). `openapi::public_document` strips only `/api/v1/admin/` (`src/http/openapi.rs:156-162`), so both operations would render into the anonymous `/openapi.json`; and `preview` would become a **second** unauthenticated operation needing an entry in the hardcoded allow-list at `src/http/mod.rs:829-844`, whose comment demands the mover *"first explain why that line has moved"* | Move both under `/api/v1/admin/admin-invites/{preview,redeem}`. They stay token-authenticated; the prefix is about layers and spec visibility, not about scope gating |
| **B8** | The `admin_invites` table uses "the exact secret-storage column set 07's `admin_setup_tokens` uses" (`:173`) | **`admin_setup_tokens` was cut by decision D1.** `migrations/0012:7-8` says so in a comment. The vocabulary claim is also wrong: `system_api_keys` uses `key_prefix varchar(64)` / `key_hash text` (`migrations/0003:265-266`), not `token_prefix`/`token_hash`. **"D1" appears nowhere in this plan**, and the Wave-0 checkpoint re-confirms D3, D5 and D7 but neither D1 nor D2 | Cite `system_api_keys` directly (`0003:262-278`) and keep this plan's own `token_*` names as a deliberate rename. **This is a citation blocker, not a design one** — the invite token is a genuinely new credential and the Argon2id+pepper design is correct |
| **B9** | `/invite/[token]` renders "a provider-agnostic set of sign-in buttons (one per enabled provider)" to an unauthenticated invitee (`:304`) | **Finding F15** (`plans/reports/EXECUTION-LEDGER.md:406-437`, ESCALATED): every read of auth configuration requires a credential; `claim-status` is the only anonymous admin operation and it carries one bit. The invitee is unauthenticated *by construction*. Plan 08's snapshot workaround is stale in exactly the case this plan creates — **a provider added through this plan's new screen is invisible to the invite page until someone signs in with the old configuration** | **F15 is BLOCKING for plan 09 and is fixed separately, before this plan implements** (serve the existing `PublicAuthMethod` projection anonymously — `src/domain/auth_settings.rs`). Recorded here as a hard prerequisite, not as work this plan performs |

### §0.2 Scope decisions taken at Wave 0

Both carry reversal conditions and belong in `plans/reports/EXECUTION-LEDGER.md`.

**D1 — ownership becomes ROW STATE, not a capability. Do not add a never-implied-scope mechanism.**
"Who is the owner" is *identity state*, not a capability grant, and scopes are the wrong primitive
for it. A never-implied set would change the **authorization core**, which plan 07 §0.7 deliberately
confined to "scope block only", and would create two competing notions of authority in a file whose
whole design is one allow-list. A column on `admin_identities` (e.g. `is_primary boolean not null
default false`) checked directly in the handler is simpler, and it **also makes the last-primary
guard writable as a query** — which under the scope design it was not, because "who carries the
scope" answers *"everyone, by implication"*.

Consequences: the backfill is a column write, not a `granted_scopes` append; `AdminTable`'s "primary"
badge reads the column; `PATCH /api/v1/admin/admin-identities/{id}` toggles it; `moira:admins:manage`
remains an ordinary scope gating **who may toggle it**, and is allowed to be implied by `moira:admin`
like every other scope. `admin_identities` therefore **does** gain a column — the body's "gains **no**
new column" claim (`:174`, `:230`, `:622`) is amended.

**Reversal condition:** revisit only if a genuine capability must be withheld from `moira:admin`
holders. At that point the authorization core is the right place, and it is one deliberate change
with its own tests — not a side effect of an invitation feature.

`moira:admins:{invite,read}` stay as ordinary scopes. Both are genuinely absent from `ADMIN_SCOPES`
(`src/security/authz.rs:8-100`) and correctly named against the `moira:jwt-issuers:{read,write,delete}`
and `moira:auth-settings:{read,write,delete}` precedents.

**D2 — this plan OWNS durable console storage, as Wave 1. It is not an assumption.**
It is a hard prerequisite for three separate features (session listing/revocation, secret durability,
a stable JWKS) and for `replicaCount > 1`. `console/lib/auth.ts:172-175` states the reversal
precisely: *"supply a Kysely dialect over the `console_auth` database here and the restriction
disappears; no other code changes."* `console-secrets.ts:214` says the same for the secret store.
Scope: the `console_auth` database, a durable Better Auth adapter, Better Auth CLI migrations in
`console/db/`, a durable `ConsoleSecretStore` implementation behind the **existing** interface, chart
values for the connection, and lifting `replicaCount: 1`.

**If that makes this plan too large, cut session management instead** — the invitation flow does not
need it, and shipping an "active sessions" screen against an in-memory store would be the *appearance*
of a feature. Durable storage still ships, because secret durability and a stable JWKS are not
optional once a second provider or a second replica exists.

**Reversal condition:** none. Without durable storage those features cannot exist.

### §0.3 Two designs plan 08 tried and REJECTED — do not re-do them

| Design this plan proposes | Why plan 08 rejected it |
|---|---|
| The cache key `` `${moiraSettingsVersion}:${maxConsoleSecretUpdatedAt}` `` (`:261`) | **There is no deployment-wide settings version**, and a `max()` cannot observe a row **deletion**. `console/lib/auth-config.ts:25-33` records both reasons. **Shipped:** `authConfigCacheKey` — a base64url digest over the **sorted `(id, version)` set** plus the newest secret write (`:54-63`). Use it unchanged; it already generalises to N rows |
| `mapProfileToUser` populating `idpIssuer`/`idpSubject` additional fields (`:263`) | **Proven broken *and* unsafe.** `console/lib/auth.ts:40-48`: Better Auth filters the mapped profile against the user schema before `createOAuthUser`, so an `input: false` field is silently dropped (observed: the row came back with only name/email/emailVerified/createdAt/updatedAt/id). Setting `input: true` makes it survive — **and settable through `update-user`, letting a signed-in operator rewrite their own `sub` and mint a token for someone else's grant.** **Shipped:** `readIdpSubject` reads `account.accountId` (`:146-157`), which has neither problem |

### §0.4 Shipped vs. open — read this before planning against plan 08

**This is the most consequential section in §0.** Plan 08 is *merged*; it is not *complete*.

**Shipped and safe to build on (the lib layer + primitives):**
`console/lib/`: `auth.ts` (one `genericOAuth` entry, `jwt` plugin, `readIdpSubject`,
`getConsoleAuth`/`resetConsoleAuth` memoisation), `auth-config.ts` (`resolveAuthConfig`,
`loadAuthConfig`, `authConfigCacheKey`, `isEmailDomainAllowed`, `AuthConfigProblem` +
`AUTH_CONFIG_PROBLEM_MESSAGE_KEYS`), `auth-runtime.ts`, `console-secrets.ts`
(`sealClientSecret`/`openClientSecret`/`SealedClientSecret`/`ConsoleSecretStore`/`classifySecretDrift`
+ `CONSOLE_SECRET_DRIFT_MESSAGE_KEYS`), `env.ts`, `errors.ts`, `moira-client.ts`, `moira-keys.ts`,
`moira-session.ts`, `setup-flow.ts`, `types.ts`. Atoms `Badge,Button,Input,Label,Spinner`; molecules
`FormField,StatusBadgeGroup`. Harnesses: `tests/support/{mock-idp,moira-stub,console-server,browser-agent,fixture-tls,…}.ts`,
`e2e/support/{console-env,leak-tap,paths,routes,secrets}.ts`. Guards: `architecture.test.ts`,
`tests/unit/architecture/server-only-{guards,import}.test.ts`, `tests/contract/openapi-contract.test.ts`,
`tests/integration/oauth-flow.test.ts`. Docs: `docs/console-architecture.md`.

**NOT shipped — Waves 2 and 3 are greenfield:**
every organism (`console/modules/` holds `.gitkeep` and a README saying so); every page beyond
`app/page.tsx`; the `(console)` route group; `/login`; `/settings/auth`; the setup wizard;
`middleware.ts`; the console i18n catalog (`lib/i18n/`); the layer-dependency, no-secret-props,
no-hardcoded-copy and i18n-coverage architecture tests; `docs/admin-console.md`; `console/db/`; the
`console_auth` database; any durable store.

**Renames the body must absorb:** `lib/provider-secrets.ts` → **`lib/console-secrets.ts`**;
`putProviderSecret`/`getProviderSecret`/`deleteProviderSecret`/`listConfiguredProviderIds` →
**`ConsoleSecretStore` + `sealClientSecret`/`openClientSecret`**; `loadAuthSettings()` →
**`loadAuthConfig()`/`resolveAuthConfig()`**; `invalidateAuthSettings()` → **`resetConsoleAuth()`**;
`console_auth.authProviderSecret` → **does not exist**; `docs/admin-console.md` →
**`docs/console-architecture.md`**; `console/tests/secret-leak/bundle-scan.test.ts` →
**`console/e2e/secret-leak.e2e.ts`** (plus `e2e/support/leak-tap.ts`).

**Sequencing that follows from B2/B3/B4.** Wave 1 becomes *durable console storage* (D2). The Moira
invite backend moves to Wave 2 (still a single owner end-to-end, for `src/http/mod.rs`'s shared route
table). Wave 3 becomes *console foundations* — the `(console)` group, `middleware.ts`, `/login`,
`SignInPanel`, `OnceOnlySecretModal`, the i18n catalog and the four architecture guards — because
every screen this plan wants depends on them and none exists. Multi-provider (B4) and the auth-settings
screen follow in Wave 4; invitations, ownership and sessions in Wave 5. **Waves 2 and 3 as currently
written cannot run in parallel from Wave 0**, because Wave 3's console work has no foundation to
attach to until Wave 3-new lands.

### §0.5 Additional required corrections the body does not contain

| Item | Why |
|---|---|
| **Migrations start at `0017`.** The body says "sequential after 07's `0009`" (`:171`, `:351`) — 07 shipped `0012`/`0013`. `0014_cluster_replica_leases.sql` and `0015_worker_jobs.sql` have since landed, and plan 11 §0 A1 reserves **`0016`** | Fixed inline. Re-verify at implementation time anyway |
| **Decision D2 (plan 07 §0.2): the grant is applied only in `authenticate_admin`, never in `authenticate_trusted_jwt`** (`src/security/auth.rs:326-335`, and the `apply_admin_identity_grant` doc comment at `:875-892` explains why moving it is a privilege-escalation bug) | This plan's central rationale — *"07's `src/security/auth.rs` extension unions `granted_scopes` onto the trusted-JWT actor's scopes **on every request**"* (`:43`) — is **false**. It matters for the redeem handler, which must state which authenticator it uses. Redeem needs `(iss, sub)` proof from a token carrying **zero** grants, so it must **not** route through `authenticate_admin`'s grant path by accident |
| **`setup/claim` already grants N admins.** `AdminIdentityService::claim` (`src/application/identity.rs:102-206`) has **no `setup_claimed` precondition**; `mark_setup_claimed` is `update … where id and claimed = false` (`src/infra/repositories/identity.rs:226-229`), a no-op on the second grant. `admin_identity_already_claimed` comes from the `(issuer, subject)` **unique index** (`:238-251`), not from a singleton gate | The premise "after plan 08, Moira/the console support **exactly one** admin identity" (`:59`, and `:9`/`:11`'s "single-admin claim flow") is wrong. **The real gap is a non-system-key path to a grant, not a second admin.** Narrowed inline |
| **e2e filenames and path.** The body names 13 `*.spec.ts` files under `console/tests/e2e/` (`:397-425`, `:549`). The convention is **`*.e2e.ts` under `console/e2e/`** — `playwright.config.ts:63-64` is `testDir: "./e2e"`, `testMatch: "**/*.e2e.ts"` | Deliberate: Bun's default test matcher picks up `*.spec.*`, so a Playwright `.spec.ts` anywhere under `console/` would be collected by **both** runners and **red the `bun test` gate**. Fixed inline |
| **Register routes in `admin_routes()`** (`src/http/mod.rs:468`), not `documented_router()` (the body says `documented_router()` at `:355`) | `documented_router` only merges route groups and applies layers. A route registered on it directly sits outside the admin body limit and timeout — stated in-code at `src/http/mod.rs:471-481` |
| **OpenAPI regeneration is mandatory, not conditional.** The body treats the drift gate as something to land *before* (`:477`, `:628`). Plan 05 landed; two gates are live (`src/http/mod.rs:1706`, `tests/openapi_drift.rs`) | Regenerate with `UPDATE_SNAPSHOTS=1 cargo test --lib http::tests::committed_openapi_matches_the_generated_document`. **Four spec gates apply and this plan names none:** `every_if_match_operation_declares_the_documented_precondition` (`:1179`), `atomic_admin_idempotency_contract_is_explicit` (`:958`), `once_only_key_responses_use_the_secret_envelope` (`:935`), and the unauthenticated-operation allow-list inside `every_operation_documents_request_ids_and_protected_operations_document_auth` (`:829-844`). The once-only gate binds `AdminInviteSecretResponse`; the If-Match gate binds `PATCH /admin-identities/{id}` |
| **The redeem 403 must not consume the invite — so it must validate OUTSIDE the transactional envelope.** The body puts the allow-list check inside the atomic redeem (`:137-151`, `:190`) while also promising the invite survives | `src/application/identity.rs:99-101` establishes the precedent verbatim: *"Every validation below runs **before** the transactional envelope, so a policy-rejected request never takes the advisory lock and never writes an idempotency record for a request that was never going to succeed."* Since `Idempotency-Key` is **required** on redeem, validating inside the envelope means a retry after the operator widens the allow-list **replays the stored 403** — breaking `allow_list_widened_then_original_invite_redeems`, this plan's own ordering test |
| **`admin_identity_not_found` / `admin_identity_already_revoked` have no *specified* emitter** (`:202`) | **A correction to the audit that raised this.** These are *not* the `auth_provider_method_unsupported` case plan 07 rejected — that code was **structurally unemittable** (serde rejects an unknown method before the service runs). Per-resource `*_not_found` codes are the house convention (`auth_provider_not_found`, `credential_not_found`, `route_not_found`), and a 404 on `PATCH`/`DELETE /admin-identities/{id}` is an obvious emitter. The real defect is that this plan **never pins either code to a path and status**, and names neither in any test. `admin_identity_already_revoked` is the genuinely doubtful one: a repeat soft-revoke under a fresh `Idempotency-Key` is the only path to it, and if that is specified to return `200` instead, the code has no emitter and must be dropped. **Pin both, or drop them** |
| **Metrics: this plan proposes none**, on a surface that grants full `moira:admin` | `src/infra/metrics.rs` is rich and live (`moira_http_requests_total`, `moira_provider_outcome_total`, `moira_worker_jobs_*`, …). Add at minimum an invite-outcome counter labelled by a **bounded denial-reason enum** (`expired`/`consumed`/`revoked`/`email_mismatch`/`domain_mismatch`/`domain_not_allowed`/`not_found`) and a grant/revoke counter. **Invitee email or domain is not a safe label** — unbounded cardinality and PII |
| **`docs/admin-console.md` does not exist**; `docs/console-architecture.md` does. `docs/admin-identity-claiming.md` (plan 07's runbook) is the file the invitation runbook should sit beside | Retarget every `docs/admin-console.md` reference (`:429`, `:598`, `:600`) |

### §0.6 Citation staleness by area — assume every reference is wrong until re-checked

| Area | Status |
|---|---|
| `console/**` | **~70% stale, and structurally so.** Fifteen named artefacts do not exist (B2); five more are renamed (§0.4). Verify against `find console -type f` before citing anything |
| `src/security/authz.rs` | `has_scope` `:148`, `ADMIN_IMPLYING_ACTOR_TYPES` `:138`, `normalize_scopes` `:154`, `can_grant` `:181`, `is_known_scope` `:186`. `moira:admins:*` genuinely absent |
| `src/security/auth.rs` | `authenticate_admin` `:309`, `authenticate_caller` `:361`, `authenticate_trusted_jwt` `:499`, `apply_admin_identity_grant` `:901`, `union_granted_scopes` `:934` |
| `src/application/identity.rs` | `claim` `:102`, the pre-envelope rule `:99-101`, `evaluate_claim_policy` ~`:225` |
| `src/infra/repositories/{identity,auth_settings}.rs` | `insert_grant` `:191`, `mark_setup_claimed` `:220`, `already_claimed_on_unique_violation` `:243`; `governing_policy` `:365` |
| `src/http/{identity,mod,openapi}.rs` | prefix prohibition `identity.rs:12-20`; `admin_routes` `mod.rs:468`; unauthenticated allow-list `mod.rs:829-844`; `public_document` `openapi.rs:156-162` |
| `migrations/*` | `0012` (identity claims, no `admin_setup_tokens`), `0013` (auth provider settings), `0014`, `0015` all shipped. Next free is **`0017`** |
| `plans/reports/EXECUTION-LEDGER.md` | F15 at `:406`; the follow-up table at `:715` |

---

## §0.7 — Wave 4 (multi-provider): drift against the tree (audit 2026-07-31, `main` at `1f5e3f7`)

**Scope of this section.** Waves 1–3 are merged; §0.1–§0.6 above describe the tree as it was
*before* them and remain the record of what those waves changed. This section audits **wave 4 only**
— multi-provider sign-in, GitHub storage, and the auth-settings screen — against the tree as it
actually stands now. Where §0.7 and any earlier §0 subsection disagree about the present tree,
**§0.7 wins**; where §0.7 and the body disagree, §0.7 wins.

**Measured drift: ~70%** — 31 of 44 discrete, checkable wave-4 claims are wrong or materially
incomplete (12 hold, 2 hold only in part). Counted by extracting every falsifiable assertion in the
wave-4 surface of the body (the D-1 dependency block, the multi-provider/GitHub/auth-settings-screen
implementation sections, their named tests, the migration and deployment notes) and checking each
against the tree, a live database, or a shipped test.

**The one-paragraph version.** Wave 4 is not one redesign but **two**, and the second is not in the
plan at all. Removing `ambiguous_enabled_providers` is real and roughly as described. But Moira's
own `governing_policy` resolves **exactly one** provider row per claim — so the moment a second
provider is enabled, the *oldest* enabled row's `allowed_email_domains` silently governs everyone,
including users who signed in through a different provider. The console guard is the only thing
that has been keeping that unreachable. **Removing the guard without fixing the query converts a
console-side refusal into a silent, deployment-wide policy substitution.** Separately, a fourth
`AuthMethod` variant is safe in Rust (three compile-time stops) and **unguarded in TypeScript**
(zero), and one `github_oauth` row reaching the database before the Rust enum knows the value turns
the anonymous login endpoint into a 500 for **every** provider.

### §0.7.1 Drift table — wave-4 claims that no longer match the tree

Grouped by area. "Body" means the wave-4 prose in this file unless another section is named.

#### Moira data model

| # | Body says | Reality (verified at `1f5e3f7`) |
|---|---|---|
| 1 | The auth-settings table supports N rows "keyed by `provider_id`" | **There is no `provider_id` column.** Rows are keyed by `id uuid`; the only uniqueness is `auth_provider_settings_method_issuer_active_unique` on `(method, coalesce(issuer,''))`. Every `provider_id` in the Moira tree belongs to the *AI-provider* side and is unrelated |
| 2 | Each row carries `hosted_domain` | **No such column, anywhere in the tree.** Zero hits in SQL, Rust, TS or the committed spec |
| 3 | Each row carries `required_org` (GitHub only) | **No such column, anywhere.** It exists only as prose in this plan |
| 4 | GitHub's non-secret config is stored in `auth_provider_settings` | **Two CHECK constraints reject it, and the second is not the one §0.1 B6 emphasises.** Verified empirically against the live database: the unnamed inline method CHECK (Postgres auto-names it `auth_provider_settings_method_check`) rejects `github_oauth` first; with *only* that widened, `auth_provider_settings_method_shape` still rejects the row, because GitHub has neither `issuer` nor `discovery_url`. Both must be dropped and re-added |
| 5 | The multi-provider migration "adds **non-secret columns only**" | **False on both halves.** No column is required to hold N rows — the table is already N-row. What is required is constraint surgery. Columns enter only through this section's decisions (`provider_id`) |
| 6 | §0.1 **B6**: the migration must also drop the unique index, because GitHub's null issuer means "at most one row per method can have a null issuer" | **Right conclusion, wrong reason — and the real defect is worse.** A single null-issuer GitHub row is fine (one GitHub.com integration is the normal case). The index actually bites on **two `generic_oidc` providers configured by `discovery_url` with no `issuer`**: both collapse to `('generic_oidc','')`. Verified live — the second insert fails `duplicate key … Key (method, COALESCE(issuer, ''::text))=(generic_oidc, )`. That is a mainstream multi-provider configuration, refused with an opaque 409 `duplicate_auth_provider` |
| 7 | Moira's claim/redeem policy is resolved **per provider** ("per-provider allowed-domain policy") | **`governing_policy` ends in `limit 1`.** It orders by `(issuer is not distinct from $1) desc, created_at asc, id asc`. In a real deployment `$1` is the *console's* issuer while every provider row's `issuer` holds the *IdP's*, so no row matches on issuer and the tiebreak is `created_at` — **the oldest enabled row governs every claim and every redemption, whichever provider the user actually used.** See W4-B1 |

#### Console

| # | Body says | Reality |
|---|---|---|
| 8 | `loadAuthSettings()` is extended to return an array | Named `loadAuthConfig` / `resolveAuthConfig` (already recorded in §0.4). The *shape* claim is the real work: `ResolvedAuthConfig` is a single flat object — one `providerId`, one `clientId`, one `clientSecret`, one `allowedEmailDomains`, one `trustedJwtIssuerId` |
| 9 | `genericOAuth` accepts a `config` **array**, so N providers need no extra plumbing | **TRUE**, and better than claimed: the shipped call already passes an array — with exactly one element spread from the single `ResolvedAuthConfig`. The chokepoint is entirely upstream |
| 10 | Secrets resolve via `getProviderSecret({moiraProviderId, providerId, moiraClientId})`, which fingerprints before decrypting | **The interface is `ConsoleSecretStore` — `put`/`read`/`reveal`/`remove`/`newestUpdatedAt`.** Drift is a separate `classifySecretDrift` (constant-time compare) returning `in_sync` / `console_secret_missing` / `client_id_mismatch` / `moira_client_id_missing`. There is deliberately **no `list()`/enumerate** — "an interface that cannot enumerate secrets is one an accidental debug endpoint cannot dump" |
| 11 | The cache key is `` `${moiraSettingsVersion}:${maxConsoleSecretUpdatedAt}` `` | Already rejected in §0.3, and the shipped replacement **needs no wave-4 change**: `authConfigCacheKey` digests the sorted `(id, version)` set over **all** fetched rows plus the newest secret write. It is already N-row-shaped. Do not touch it |
| 12 | `lib/provider-secrets.ts` is reused verbatim | The modules are `lib/console-secrets.ts` (envelope + interface + in-memory impl) and `lib/console-secrets-postgres.ts` (the durable impl over `console_provider_secret`). The *intent* — one and only one place that encrypts — is shipped and is guarded |
| 13 | `requireIssuerValidation: true` is pinned on every entry ("the plugin's own default is `false`") | **The option does not appear anywhere in `console/`.** It was never set. `pkce: true` *is* set unconditionally. Pinning `requireIssuerValidation` is new work, not a per-entry propagation |
| 14 | `mapProfileToUser` populates `idpIssuer`/`idpSubject` additional fields "plan 08 introduced" | Recorded as **proven broken and unsafe** in §0.3 and still true: the shipped mechanism is `readIdpSubject`, reading `account.accountId` filtered by `providerId`. Its doc comment already anticipates wave 4 — "the `providerId` filter matters because a future second provider would otherwise make the answer depend on row order" |
| 15 | Callback URLs are covered because "`middleware.ts`'s host allow-list is unchanged" | **There is no `middleware.ts`.** §0.4 scheduled one for wave 3; wave 3 did not ship it. The session gate is the `(console)` **route-group layout** (`hasConsoleSession()` → `redirect("/login")`). Any wave-4 page placed inside `(console)` inherits that gate for free; there is no host allow-list to inherit |
| 16 | The `databaseHooks.user.create.before` gate "from plan 08" is extended to resolve per-provider domains | **`databaseHooks` appears nowhere in `console/`.** That gate was never built. Domain enforcement lives in `lib/moira-session.ts`, which calls `isEmailDomainAllowed(email, config.allowedEmailDomains)` against the **single** resolved config |
| 17 | GitHub arrives via Better Auth's built-in `socialProviders.github` | **`socialProviders` has zero occurrences in `console/`**; `emailAndPassword` is explicitly disabled with "An admin console has exactly one way in: the operator's IdP." This is greenfield, not an extension |
| 18 | The auth-settings screen "extends plan 08's" | **No settings screen exists.** No path under `console/` matches `settings`; no `modules/authSettings/`; no provider CRUD UI at all. `SignInPanel`'s own header states the position: a provider picker "is not 'not built yet' — it is wrong in this wave" |
| 19 | `ProviderSecretRotatePanel` and `ProviderDriftBanner` are "reused per provider row, not re-implemented" | **Neither component exists.** `console/modules/` holds exactly `secrets/OnceOnlySecretModal.tsx` and `signIn/SignInPanel.tsx` |
| 20 | `OnceOnlySecretModal` and `SignInPanel` are reused from plan 08 | **TRUE** — both shipped in wave 3, under `console/modules/` |
| 21 | "**No new i18n keys are needed for D7 in this plan**" — the named `console.error.auth_provider_*` and `console.authSettings.*` keys are reused verbatim | **Three of the named keys exist** (`auth_provider_create_failed`, `auth_provider_secret_write_failed`, `auth_provider_enable_failed`). **These do not:** `auth_provider_client_id_mismatch`, `auth_provider_secret_undecryptable`, `console.notice.orphaned_provider_secret`, `console.authSettings.rotate_secret.*`, `console.authSettings.secret_configured`. Drift is currently keyed through `CONSOLE_SECRET_DRIFT_MESSAGE_KEYS`. **And the coverage gate is bidirectional** — a key added without a shipped emitter fails `i18n-catalog-coverage.test.ts`, so keys and UI must land together |

#### Migration, contract and deployment

| # | Body says | Reality |
|---|---|---|
| 22 | The multi-provider extension rides in `migrations/0017_admin_invites.sql` | **`0017` and `0018` are shipped.** `0016` was reserved by plan 11 and never used — the sequence is permanently non-contiguous, which `sqlx::migrate!("./migrations")` tolerates. Next free **on `main` today is `0019`**; PR #39 (`fix/findings-sweep`, in flight) already carries `0019_single_primary_admin.sql`, so after it merges the next free is **`0020`**. HANDOFF's "`0020`" is right only once #39 lands. **Re-verify with `git ls-files migrations/` at branch time** |
| 23 | Wave 0 must confirm "the operation count still **10**" | **Two different numbers, neither of which is 10 in the sense used.** The auth-provider family is **7** operations (4 paths); the committed document is **151** operations over **99** paths and **178** schemas. "10" was plan 07's identity-surface figure. The console pins the 7 in `openapi-contract.test.ts` |
| 24 | Adding GitHub is a storage question | It is also an **enum** question the body never asks. `AuthMethod` is enumerated in `docs/openapi.json` as exactly `["google_oauth","generic_oidc","jwks"]`, and the TS mirror in `console/lib/types.ts` is hand-written with **no test and no compile-time link to the spec** |
| 25 | `charts/moira-console/values.yaml` needs no new secret because "provider secrets live in Moira, encrypted" | **Contradicts D7** — they live in the *console's* database. The chart statement is right for the wrong reason. Separately `replicaCount: 1` **is still pinned**, because the auth-config snapshot is per-process; N providers multiply that divergence rather than introduce it |
| 26 | Named test `auth-settings-multi.test.ts` covers `lib/auth-settings.ts` | No such module. The unit under test is `lib/auth-config.ts` (+ `lib/auth-runtime.ts`) |
| 27 | Named test `only_provider_secrets_module_encrypts_or_fingerprints` | Right idea, wrong module name, and **already enforced**: `server-only-guards.test.ts` pins `console_provider_secret` to `lib/console-secrets-postgres.ts` alone and pins the connection string to exactly three named files |
| 28 | Named test `there_is_no_rotate_secret_call_for_any_provider` is new work | **Already shipped and gated three ways**: the tree-wide `rotate-secret` literal ban in `server-only-guards.test.ts`, the no-`/rotate.*secret/i`-key rule in `i18n-catalog-coverage.test.ts`, and Moira-side `the_rotate_secret_path_is_genuinely_unrouted_not_merely_undocumented` |
| 29 | New file `console/tests/unit/architecture/bundle-scan.test.ts` | Superseded — `console/e2e/secret-leak.e2e.ts` scans `.next/static/**` and sourcemaps as well as rendered HTML/RSC. Wave 3 closed the `E2E_SKIP_BUILD=1` hole it would have duplicated |

**Holds as written (do not re-litigate):** the `genericOAuth` config array (#9); `OnceOnlySecretModal`/`SignInPanel` reuse (#20); D7 itself — Moira stores no client secret, returns none, and has no `rotate-secret` endpoint, all three gated; `console_provider_secret` being N-row by construction (one row per provider, `provider_id` primary key); per-provider fail-closed resolution as a *design*; deny-by-default domain policy with no invitation exemption; the Atomic-Design placement rules; `*.e2e.ts` under `console/e2e/`; `pkce: true`; the Better Auth callback pattern `${baseURL}/api/auth/oauth2/callback/:providerId`; and mandatory OpenAPI regeneration.

### §0.7.2 Blockers, ranked

**W4-B1 — Removing `ambiguous_enabled_providers` without fixing `governing_policy` ships a silent policy substitution. (Severity: highest — security, silent, and created by this wave.)**

`governing_policy` selects `limit 1`. Its ordering prefers an exact `issuer` match, but on any console-mediated deployment the caller's issuer is the *console's* while each provider row's `issuer` is the *IdP's* — so nothing matches on issuer and rows tie, leaving `created_at asc, id asc`. With one enabled provider that is harmless. With two it means **the oldest enabled row's `allowed_email_domains` governs every claim and every invite redemption**, regardless of which provider the identity actually authenticated through.

Consequences, both reachable: a permissive first provider silently widens a restrictive second one (a domain allowed for Google is accepted for a GitHub identity that no rule ever admitted); and a restrictive first provider silently denies a correctly-configured second one. Neither surfaces an error naming the cause.

This is the plan-08 B1 signature: **the plan states an invariant — "per-provider allowed-domain policy" — that nothing in it exercises.** No named wave-4 test asserts that a redemption through provider B is judged by B's allow-list. The console's `ambiguous_enabled_providers` refusal is the only reason this has never fired, and wave 4's central task is to remove it. **Fix the query in the same wave that removes the guard, or do neither.**

**W4-B2 — `auth_method_from_db` turns one unknown row into a 500 on the anonymous login endpoint. (Severity: high — availability, deploy-ordering, silent to the compiler.)**

`auth_method_to_db` is an exhaustive match (the compiler stops you). Its inverse `auth_method_from_db` ends in a catch-all `_ => Err(AppError::Internal(...))`. `record_from_row` and `list_enabled_public` both call it **per row**, so a single `github_oauth` row that exists in the database while the binary does not know the value fails **the whole list** — including the unauthenticated `GET /api/v1/admin/setup/sign-in-methods`, i.e. the login screen goes to 500 for every provider, not just GitHub.

The reachable path is ordinary: `charts/moira/templates/migration-job.yaml` runs migrations as a Helm hook **before** pods roll, so during any rolling deploy old replicas serve against the new schema. If the widened CHECK lands and any row is created before every replica is new, old replicas 500. The existing negative test does not catch the gap — it asserts `auth_method_from_db("github").is_err()`, using the string `"github"`, not `"github_oauth"`.

**W4-B3 — Changing the console's `providerId` scheme orphans the shipped secret and locks out every existing admin. (Severity: high — data, irreversible without operator action.)**

Today there is one hardcoded `CONSOLE_OAUTH_PROVIDER_ID = "moira-console-idp"`. Two durable stores are keyed on it:

* `console_provider_secret.provider_id` is the **primary key**, and the AEAD's AAD is
  `` `moira-console/v${SECRET_ENVELOPE_VERSION}/${providerId}/${clientId}` `` — so a renamed
  providerId does not merely miss the row, it **cannot decrypt** it. The operator must re-enter the
  client secret.
* Better Auth's `account.providerId` records which provider each linked account came from, and
  `readIdpSubject` filters on it. A renamed providerId makes `readIdpSubject` throw
  `MissingIdpSubjectError` for **every existing admin**, so no Moira-bound token can be minted —
  and `account.accountId` is precisely the IdP subject Moira's `admin_identities` grant is keyed on.

Any wave-4 providerId scheme must therefore either preserve `moira-console-idp` for the pre-existing row or ship a console-DB data migration that re-seals every secret and rewrites `account.providerId`. See **W4-D2**, which chooses the former.

**W4-B4 — The TypeScript `AuthMethod` union is the one unguarded seam in the whole contract. (Severity: medium-high — silent, and mis-reports its own cause.)**

`console/lib/types.ts` hand-mirrors the spec enum. `console/tests/contract/openapi-contract.test.ts` re-derives required/optional **key sets and operation paths** and never inspects `enum`, so **nothing** links the union to `docs/openapi.json`. Worse, `isInteractiveMethod` is an allow-list of literals (`method === "google_oauth" || method === "generic_oidc"`), not a `switch`, so TypeScript raises no exhaustiveness error: a `github_oauth` row resolves to `fail("method_not_interactive")`, whose shipped catalog message tells the operator **"The enabled row's `method` is `jwks`"** — a false diagnosis of a provider they just configured.

**W4-B5 — The `(method, coalesce(issuer,''))` unique index refuses two discovery-only OIDC providers. (Severity: medium — mainstream configuration, opaque error.)**

Verified live. Two `generic_oidc` rows configured by `discovery_url` alone both key to `('generic_oidc','')` and the second is refused. `map_constraint_violation` renders that as `409 duplicate_auth_provider`, which is true but unhelpful — the rows are not duplicates in any sense the operator recognises. Distinct `issuer` values avoid it, so the failure is configuration-dependent and will look intermittent.

**W4-B6 — The shared test database has one global null-issuer slot per method, and only one of them is locked. (Severity: medium — CI flake, and it leaks a permanently poisoning row.)**

`ISSUERLESS_GENERIC_OIDC_LOCK_KEY` / `IssuerlessSlotLock::acquire` exist precisely because `('generic_oidc','')` is a single global slot on the shared database; the lock serialises tests that need it and deletes the row. `('github_oauth','')` is a **second** such slot with **no** lock. Any wave-4 test inserting an issuer-less GitHub row will collide across parallel runs, and a leaked row then feeds W4-B2. This is the same class as the ledger's "~986 leaked `trusted_jwt_issuers` rows".

**W4-B7 — Wave-4 UI cannot enumerate configured secrets through the shipped interface. (Severity: low-medium — design constraint, not a defect.)**

`ProviderList` wants a "secret configured" badge per row. `ConsoleSecretStore` deliberately has no `list()`. The resolution is to drive per-provider `read()` calls from Moira's row list (which the console already fetches) rather than to widen the interface — widening it would discard the stated rationale and is the kind of change that later reads as an oversight. Record the choice where the interface is defined.

### §0.7.3 Decisions taken for wave 4, each with its reversal condition

Taken under the standing "decide rather than ask" authority. Each belongs in
`plans/reports/EXECUTION-LEDGER.md` alongside the wave-1–3 decisions.

**W4-D1 — GitHub becomes a fourth `AuthMethod` variant, `github_oauth`. It does not become a `provider_id`-keyed generic row.**
`AuthMethod` is already the discriminator in the SQL CHECK, the shape validator, the DB encoder, the
sign-in projection filter and the committed spec. Adding a variant lights up **three compile-time
stops** — `PublicSignInMethod::from_enabled_method`, `validate_method_shape`, `auth_method_to_db` —
each of which forces an explicit GitHub decision at exactly the right place. A `provider_id`-keyed
"generic OAuth" row would introduce a second discriminator alongside `method` and leave all three
matches untouched, which is the same as having no forcing function at all. GitHub's shape branch is
`client_id is not null and authorization_url is not null and token_url is not null` — not the OIDC
issuer/discovery rule.
*Reversal condition:* if a deployment must configure more than one non-OIDC OAuth provider that is
not GitHub (GitLab, Bitbucket, a bespoke OAuth2 IdP), stop adding variants and introduce a generic
`oauth2` method whose per-provider identity is carried by `provider_id`. One extra variant is
cheaper than a second discriminator; three would not be.

**W4-D2 — Add a `provider_id` slug column to `auth_provider_settings`, unique among live rows, and backfill the existing enabled row to `'moira-console-idp'`.**
This is the only option that resolves **W4-B3** without a console-DB data migration. The backfill
makes the pre-existing row's Better Auth `providerId` unchanged, so `console_provider_secret` still
decrypts (the AAD is preserved) and every `account.providerId` still matches `readIdpSubject`. It
also gives operators stable, readable callback URLs (`/api/auth/oauth2/callback/github`) that
survive deleting and recreating a row — which the row `id` uuid would not, since the callback must
be registered **exactly** at the IdP. The slug is immutable after creation; renaming one is
equivalent to deleting and recreating the provider, and must be documented as such.
*Reversal condition:* if Better Auth ever routes on something other than a caller-supplied
`providerId`, or a slug collides with one of its reserved route segments, fall back to the row `id`
uuid **plus** a console-DB migration that re-seals every `console_provider_secret` row under the new
AAD and rewrites `account.providerId` in the same transaction. That migration is the cost this
decision exists to avoid; do not pay it accidentally.

**W4-D3 — `provider_id` is added to `PublicSignInMethod`; `required_org` is not, and neither is anything else.**
Applying F15's admitting rule explicitly, as the brief requires: *every field in the anonymous
projection must be one the browser already transmits or receives during the sign-in it is about to
start.* `provider_id` **passes** — it is literally a path segment of the callback URL the browser is
about to visit and a field of the sign-in POST body, so it discloses nothing a click would not.
`required_org` **fails** — it is membership policy, never on the wire to the browser, and naming a
company's GitHub org to anonymous callers is the same class of disclosure as
`allowed_email_domains`, which is exactly why `sign-in-methods` exists instead of `auth-methods`.
This deliberately breaks `the_anonymous_projection_drops_the_domain_policy_and_the_jwks_url`, which
asserts an **exact key set**; that break is the gate working. Update it *with the rule written into
the assertion*, so the next person adding a field meets the argument rather than the list.
*Reversal condition:* if the console can render N buttons without knowing each provider's routing
key — for example if Better Auth gains a lookup by opaque row id — drop the field again. The
anonymous surface should shrink whenever it can.

**W4-D4 — `governing_policy` becomes the union of `allowed_email_domains` across all enabled rows bound to the caller's trusted issuer. Moira is the deployment-wide backstop; the console keeps per-provider enforcement.**
Moira **cannot** observe which upstream IdP a console-minted token came from — the token's issuer is
the console's for every provider — so a genuinely per-provider decision is not available to it
without a new console-asserted claim. The console *can*: it already knows the provider from
`account.providerId`, and already enforces domains in `lib/moira-session.ts`. So the honest split is
console = per-provider gate, Moira = deployment-wide backstop, and the union is the correct backstop
semantics. With one enabled provider the union is identical to today's behaviour, so no existing
deployment changes. Deny-by-default is preserved exactly: zero enabled rows, or rows whose lists are
all empty, still deny everyone.
*Reversal condition:* if an operator needs different allow-lists per provider enforced **at Moira**
— for instance contractors admitted through GitHub only, under an org check the console cannot be
trusted to apply alone — add an explicit `provider_id` to `ClaimAdminIdentityRequest` and
`AdminInviteRedeemRequest` and resolve policy from that row. That is a frozen-DTO change and a new
trust assertion (the console would be asserting *which* policy governs it), so it needs its own
decision, not a silent widening of this one.

**W4-D5 — `required_org` is deferred out of wave 4 entirely. GitHub ships with verified-primary-email hardening only.**
The org check is optional by the plan's own product recommendation, and wave 4 is already two
redesigns. Deferring it removes a column, three DTO changes, a spec regeneration and a live-GitHub
dependency from the critical path, and loses nothing that blocks GitHub sign-in. The
verified-primary-email lookup **is** in scope, because without it GitHub cannot satisfy the uniform
verified-email requirement the grant path depends on (`profile.email` may be null, unverified, or a
`noreply` address).
*Reversal condition:* when org scoping is wanted, add a typed `required_org text` column — **not**
`metadata`. `metadata` is opaque round-trip storage that nothing reads, and it is excluded from
`list_enabled_public` and from both public projections, so policy parked there is invisible to every
read path that matters and is validated by nothing.

**W4-D6 — F21 is closed in wave 4, not wave 5.**
F21 belongs to the invitation domain, which is wave 5's, so by ownership it is wave 5's. It is
reassigned to wave 4 on cost grounds: wave 4 already opens `src/application/identity.rs` for
**W4-D4**'s policy change, and the fix is a guard in one `match` arm plus one test. The defect: after
the transactional envelope, the `Err` arm calls `record_admin_invite_denied(denial_reason(&error))`
**unconditionally**, including when the error is `AppError::Replayed`. A cacheable failure (409
`admin_identity_already_claimed` is 400/404/409/422) writes an idempotency record, so every retry
under the same `Idempotency-Key` replays it and increments the denial counter again — inflating an
operator's denial rate by the client's retry count. The success arm is unaffected and needs no
guard: pre-envelope validation refuses a consumed invite before the envelope is entered, so a
*successful* redemption can never replay. Skip the metric when the error is a replay; the original
attempt already counted it.
*Reversal condition:* if wave 4 ships without touching `src/application/identity.rs` (i.e. **W4-D4**
is descoped), F21 returns to wave 5 rather than being carried alone.

### §0.7.4 Ordered task list for the wave-4 implementation

Sized for one agent. **Moira first** — the console cannot render a provider it cannot store, and the
Rust enum must reach every replica before any `github_oauth` row can exist (W4-B2).

**Phase 1 — Moira schema and domain (one migration, one commit)**

1. Re-verify the migration number: `git ls-files migrations/`. `0019` if PR #39 has not merged,
   `0020` if it has. **Two migrations with one number is a hard failure** — do not take HANDOFF's
   number on trust.
2. Write the migration: drop the auto-named `auth_provider_settings_method_check` and re-add it with
   `github_oauth`; drop and re-add `auth_provider_settings_method_shape` with the GitHub branch
   (`client_id not null and authorization_url not null and token_url not null`); add
   `provider_id text` with a unique index over live rows; backfill `'moira-console-idp'` onto the
   existing enabled row; re-key the issuer uniqueness to
   `(method, coalesce(issuer, discovery_url, ''))` so W4-B5's two discovery-only OIDC rows are
   admitted while genuine duplicates are still refused.
3. `src/domain/auth_settings.rs`: add `AuthMethod::GithubOauth`; add `provider_id` to
   `AuthProviderSettingsRecord`, both request DTOs, `PublicAuthMethod` and `PublicSignInMethod`.
   Follow the three compile errors this produces (`from_enabled_method`, `validate_method_shape`,
   `auth_method_to_db`) and give GitHub an explicit answer at each.
4. **Fix `auth_method_from_db` (W4-B2)** — the compiler will not point at it. Decide deliberately
   between a hard error and a skip-with-`warn!`; a skip keeps the login screen up for other
   providers during a mixed-version roll, which is the failure this blocker is about.
5. Update the tests that pass vacuously: `auth_method_round_trips_through_the_database_encoding`
   (its negative literal is `"github"`, not `"github_oauth"`),
   `oauth_methods_need_a_client_id_and_an_issuer_or_discovery_url` (an array literal, not a match),
   `the_anonymous_projection_excludes_jwks_rows`, and the console's
   `only google_oauth and generic_oidc are interactive`.
6. Add the sibling advisory-lock key beside `ISSUERLESS_GENERIC_OIDC_LOCK_KEY` for
   `('github_oauth','')` **before** writing any test that inserts an issuer-less GitHub row (W4-B6).

**Phase 2 — Moira policy (W4-B1, the security fix)**

7. Change `governing_policy` to union `allowed_email_domains` across all enabled, active,
   non-deleted rows matching `issuer = $1 or trusted_jwt_issuer_id = $2` (per W4-D4). Keep
   deny-by-default: no matching row, or an empty union, still denies.
8. Test it against a real database with **two** enabled providers carrying **different** allow-lists,
   asserting a domain in either is admitted and a domain in neither is refused — and assert the
   premise (both rows are enabled and bound) so the test cannot pass vacuously. Mutation-test it:
   revert to `limit 1` and confirm the test fails.
9. Close **F21** (W4-D6): skip `record_admin_invite_denied` when the error is `AppError::Replayed`;
   test that a retried failing redemption increments the counter exactly once.

**Phase 3 — Contract**

10. Regenerate:
    `UPDATE_SNAPSHOTS=1 cargo test --lib http::tests::committed_openapi_matches_the_generated_document`.
    Never hand-edit `docs/openapi.json` — the drift gate compares bytes.
11. Expect **no operation-count change**: wave 4 adds no route, so `assert_eq!(operation_count, 151)`
    and the 99-path `BTreeSet` both stand. If either moves, a route was added by accident — find it.
    Schema churn (the `AuthMethod` enum, the new `provider_id` fields) is expected.
12. Re-run `tests/openapi_drift.rs` and the console's `openapi-contract.test.ts`; the auth-provider
    family must still be **7** operations.

**Phase 4 — Console multi-provider (the `ambiguous_enabled_providers` removal)**

13. `console/lib/types.ts`: widen the `AuthMethod` union **and** add `provider_id` to the affected
    DTOs and their `*_CONTRACT` descriptors (every `*_CONTRACT` must appear in `SCHEMA_CONTRACTS`).
14. **Add the missing guard (W4-B4)**: a contract test asserting the TS union equals
    `docs/openapi.json`'s `components.schemas.AuthMethod.enum`. This seam is currently unguarded in
    both directions; without it, step 13 can be half-done and nothing objects.
15. `console/lib/auth-config.ts`: turn `ResolvedAuthConfig` into a per-provider record and
    `resolveAuthConfig` into a per-provider resolution returning resolved providers **plus**
    per-provider problems; delete the `enabled.length > 1` refusal and its comment; make
    `loadAuthConfig` read one secret per enabled row. Add `github_oauth` to `isInteractiveMethod`.
    **Leave `authConfigCacheKey` alone** — it is already correct.
16. Keep resolution **fail-closed per provider**: a provider whose secret is missing,
    fingerprint-mismatched or undecryptable is omitted from the array with its keyed condition
    attached; the others are constructed normally.
17. `console/lib/auth.ts`: map N resolved providers into the existing `genericOAuth({config: [...]})`
    array; set `requireIssuerValidation: true` per entry (it is absent today — W4-#13); keep
    `pkce: true`. **The single-instance memoisation stays correct** — one Better Auth instance holds
    N configs and `cacheKey` already spans all rows. Resolve the `getConsoleAuth`/`resetConsoleAuth`
    dead-code duplication against `lib/auth-runtime.ts` while here, and fix `SignInPanel`'s header,
    which cites the dead symbol.
18. Mirror the option set into `console/db/schema.ts` (its single-element placeholder array feeds the
    migration drift test).
19. `SignInPanel`: render N buttons from the anonymous `getSetupSignInMethods()`, keyed by
    `provider_id`. Delete the "a provider picker is wrong in this wave" header comment — it is the
    thing being changed, and leaving it would make the next reader think the change was accidental.
20. `lib/moira-session.ts`: enforce the domain list of the provider the session actually came from
    (`account.providerId` → that provider's config), not a single global list.

**Phase 5 — GitHub**

21. `console/lib/github.ts` (`// @server-only` first line **and** `import "server-only";` — the
    derived-credential guard requires both): verified-primary-email lookup against `/user/emails`,
    rejecting null / unverified / `noreply`-only. Build against a **mock GitHub**; no live
    credential, per the standing constraint.
22. Wire GitHub through the same generic path where possible. It is not OIDC, so it needs
    `authorization_url`/`token_url` rather than discovery — which is exactly the shape branch added
    in step 2.

**Phase 6 — Auth-settings screen**

23. `console/app/(console)/settings/auth/{page.tsx,actions.ts}` — inside `(console)`, so it inherits
    the session gate; it is picked up automatically by the a11y route walker and the secret-leak
    scanner, and must answer **< 400 on a cold, unconfigured console**.
24. Organisms under `console/modules/authSettings/` — they may import only `lib/errors.ts`,
    `lib/types.ts`, `lib/moira-keys.ts`, `lib/i18n/**` and atoms. No `fetch` in atoms or molecules.
25. Per-provider dual write (Moira create/patch → console secret `put` → Moira enable as the commit
    point), with partial states rendered per row and never as success. Drive the "secret configured"
    badge from per-provider `read()` over Moira's row list — **do not add `list()` to
    `ConsoleSecretStore`** (W4-B7); record that where the interface is defined.
26. i18n: add the missing keys **with their emitters in the same commit** — the coverage gate is
    bidirectional and an orphan key fails it. Do not add a key matching `/rotate.*secret/i`.

**Phase 7 — Verification**

27. `scripts/gates.sh` (six gates; asserts zero silently-skipped DB suites), with
    `MOIRA_TEST_DATABASE_URL='postgres://postgres:postgres@127.0.0.1:5432/moira'` **exactly**.
28. Console: `bun install --frozen-lockfile && bun run typecheck && bun run lint && bun test &&
    bun run build && bun run e2e`.
29. `scripts/mutants.sh` over the touched Rust — mandatory for the `governing_policy` union and the
    F21 guard. Both are new guards, and a guard nobody has seen fail is an assumption.
30. e2e: two OIDC providers plus GitHub configured from the screen; three buttons render; disabling
    one removes its button with no redeploy; a drifted provider is excluded while the others keep
    working; and **a redemption through provider B is judged by B's allow-list** — the assertion
    W4-B1 exists for.

### §0.7.5 Escalations — written down, not blocking

1. **SECURITY — W4-B1 is a silent authorization-policy substitution** and is the single item on this
   list that can reach production as a wrong *grant* rather than a wrong error. It is latent today
   only because a console-side guard makes it unreachable, and wave 4's stated purpose is to remove
   that guard. If wave 4 is descoped, **descope the guard removal with it**; shipping multi-provider
   sign-in against a single-provider policy resolver is worse than shipping neither.
2. **SECURITY (lower) — `required_org` must never enter `PublicSignInMethod`** (W4-D3). It is
   membership policy and belongs to the same class as `allowed_email_domains`, whose anonymous
   exposure F15 explicitly rejected. Recorded here because the body proposes carrying it per provider
   without saying on which surface.
3. **AVAILABILITY — the migration/binary ordering in W4-B2 is a property of the Helm chart**, not of
   this wave: `migration-job.yaml` runs as a hook before pods roll, so mixed-version windows are
   normal. Wave 4 should choose the tolerant `auth_method_from_db` behaviour; the general question of
   forward-compatible enum reads across a rolling deploy is **unscheduled** and affects every
   DB-encoded enum in the tree, not just this one.
4. **HYGIENE — W4-B6 adds a second unguarded global slot** on the shared test database, the same
   class as the ~986 leaked `trusted_jwt_issuers` rows already recorded. Add the lock before the
   first GitHub test, not after the first flake.
5. **NO LIVE CREDENTIAL IS REQUESTED OR NEEDED.** GitHub is built against a mock exactly as Google
   is; the seam is `console/lib/github.ts`. What cannot be proven without a real GitHub OAuth app —
   GitHub's own token claims, its consent screen, and `/user/emails` behaviour on edge-case accounts
   — is **deferred and recorded**, never faked into a green test.
6. **`replicaCount: 1` remains pinned** and wave 4 does not lift it. The per-process auth-config
   snapshot means two pods can hold different provider sets; with N providers the symptom broadens
   from "some sign-ins get `invalid_client`" to "some providers are missing on some pods". Lifting it
   needs the snapshot to become shared, which is out of scope here.

### §0.7.6 Corrections to the wave-4 brief itself

Recorded because the brief asked, and because two of these would have shaped the work wrongly:

* **"Migrations: next free is `0020`"** — `0020` only after PR #39 merges; on `main` at this audit it
  is `0019`. `0016` is a permanent gap. Verify at branch time.
* **"Wave 3 shipped `middleware.ts`"** (implied by §0.4's wave-3 scope) — it did not. The session
  gate is the `(console)` layout. There is no host allow-list for callback URLs to inherit.
* **"The migration is unconditional, dropping and re-adding two CHECK constraints"** — correct, and
  now verified empirically rather than by reading. But §0.1 B6's *reason* for also touching the
  unique index is wrong: the real defect is two discovery-only OIDC providers, not GitHub's null
  issuer.
* **"`PublicSignInMethod` would have to carry something for a GitHub button"** — it already carries
  everything GitHub needs (`authorization_url`, `client_id`, `requested_scopes`, with `issuer` and
  `discovery_url` simply null). The only addition is `provider_id`, and that is required by
  *multi-provider routing*, not by GitHub.
* **"F21 lives in plan 09"** — true, but it is neither wave's by domain; it is a wave-2 backend
  defect. Reassigned to wave 4 on cost grounds only (W4-D6).
* **The brief's framing that wave 4 is "a redesign of a shipped safety decision"** is right, and
  understated: the shipped decision is a *console-side* refusal standing in front of a *Moira-side*
  defect nobody has recorded. Removing it is not one redesign but two.

---

## §0.8 — Wave 5 (invitations + ownership UI): drift against the tree (audit 2026-07-31, `main` at `0b7792b`, PR #39 read at `4ea484b`)

**Scope of this section.** Waves 1–3 are merged and §0.7 audits wave 4. This section audits **wave 5
only** — the invitation UI, the ownership-transfer / recovery UI, and session management — against
the tree as it stands now **and against the tree as it will stand after PR #39 (`fix/findings-sweep`)
merges**. Where §0.8 and any earlier §0 subsection disagree about the present tree, **§0.8 wins**;
where §0.8 and the body disagree, §0.8 wins. §0.8 does not restate wave-4 findings; where a wave-4
finding also binds wave 5 it is cited, not repeated.

**Measured drift: ~83%** — 43 of 52 discrete, checkable wave-5 claims are wrong or materially
incomplete (9 hold, 6 hold only in part, 1 could not be established). Counted the same way §0.7
counted wave 4: by extracting every falsifiable assertion in the wave-5 surface of the body — the
invitation-UI, session-management and ownership-transfer/recovery-UI implementation sections; the two
data-flow blocks; the Atomic-Design placement rows for wave-5 files; the console-side i18n table; the
wave-5 rows of the API and Interfaces tables; the Scopes/authz section; the named wave-5 Rust,
console-unit and e2e tests; and the wave-5 Definition-of-Done and product-decision items — then
checking each against the working tree, the committed `docs/openapi.json`, a shipped test, or PR #39's
diff. Roughly half the drift is *"already shipped, in a different shape"* (waves 2 and 3 built more
than this plan predicted, and named it differently); the other half is *"specified against something
that does not exist"*.

**The one-paragraph version.** Wave 5 as written is three features. One of them —
**invitations** — is genuinely a UI wave over a backend that is complete, better than specified, and
already has eleven catalogued error codes and a sixteen-test integration suite. The second —
**ownership** — is also a UI wave, but only after PR #39, which is what makes any admin primary at all
and which changes transfer from two calls into one. The third — **recovery** — has **no backend
whatsoever**: wave 2 deliberately omitted `is_recovery` and `replaces_admin_identity_id` under an
unrecorded decision (D-W2-1), so `RecoveryPanel`, `recovery.e2e.ts` and the `admin_identity_recovered`
audit event are specified over a migration nobody has written. Separately, four shipped console gates
— the secret-leak modal tripwire, the dynamic-route coverage guard, the secret-shaped-DTO rule and the
`"use client"` containment rule — will each go **red on the first commit of this wave**, by design;
none is named in the plan. And the a11y gate is currently **vacuous for every route inside
`(console)`**, which is the wave's entire surface.

### §0.8.1 Drift table — wave-5 claims that no longer match the tree

Grouped by area. "Body" means the wave-5 prose in this file unless another section is named.

#### The Moira surface the wave-5 UI binds to

| # | Body says | Reality (verified at `0b7792b`, and at `4ea484b` where marked **#39**) |
|---|---|---|
| 1 | `POST /api/v1/admin-invites/{preview,redeem}` sit **outside** the `/api/v1/admin/*` prefix | Both are `POST /api/v1/admin/admin-invites/{preview,redeem}`, registered in `admin_routes()`. §0.1 **B7** said so; the body's Interfaces & Contracts table still says otherwise. `preview` is on the unauthenticated allow-list inside `every_operation_documents_request_ids_and_protected_operations_document_auth`, with the explanation that comment demands |
| 2 | Create body is `{ email_or_domain_type: "email"\|"domain", value, expires_in_seconds, is_recovery?, replaces_admin_identity_id? }` | `AdminInviteCreateRequest { constraint: AdminInviteConstraint, value: String, expires_in_seconds: u32 }`, `deny_unknown_fields`. The field is `constraint`, not `email_or_domain_type`, and **neither recovery field exists** |
| 3 | `preview` returns "the inviter's display email with the local part masked, e.g. `j***@example.com`" | `AdminInvitePreviewResponse { constraint, value, expires_at }` and nothing else, pinned by `the_anonymous_preview_response_carries_only_constraint_and_expiry`. Masked inviter attribution was **considered and declined** in the DTO's own doc comment, with a reversal condition ("if product wants inviter attribution, it arrives with its own masking function and its own leak test") |
| 4 | `PATCH /api/v1/admin/admin-identities/{id}` grants/revokes `moira:admins:manage` inside the target's `granted_scopes` | `AdminIdentityPatchRequest { is_primary: bool }` — one required field, `deny_unknown_fields`, pinned by `the_ownership_patch_request_has_exactly_one_field`. `granted_scopes` is never written by this path |
| 5 | The caller scope for `PATCH`/`DELETE /admin-identities/{id}` is `moira:admins:manage` | The gate is `AdminIdentityService::require_primary_actor` — **row state, not a scope**. `is_known_scope("moira:admins:manage")` is asserted **false** in `src/security/authz.rs`. Only `moira:admins:{read,invite}` were added to `ADMIN_SCOPES` |
| 6 | `moira:admins:manage` "is deliberately checked as an **explicit** scope (**not** implied by `moira:admin`)" and "must be implemented **and tested** as an explicit check" (Scopes/authz, Risks, DoD) | Settled against, in code, by §0.2 **D1**. The body still asserts it in four places and the DoD still requires it. `moira.error.admin_identity_not_primary`'s catalogue description states the reason in one sentence: *"a scope could not express 'not every admin'"* |
| 7 | `AdminTable` renders a "primary" badge for rows whose `granted_scopes` include `moira:admins:manage` | Read `AdminIdentityRecord.is_primary`. `console/lib/types.ts` already carries the field, in `ADMIN_IDENTITY_RECORD_CONTRACT.required`, with an in-file warning not to render it as a capability the *signed-in* user has |
| 8 | Recovery invites exist: `is_recovery: true`, `replaces_admin_identity_id`, an atomic revoke-and-grant swap in one transaction | **Nothing exists.** No column, no DTO field, no route, no service method, no error code, no notice, no test. Wave 2 recorded **decision D-W2-1** — "a column no code writes is the schema equivalent of a catalog entry with no emitter" — in `migrations/0017_admin_invites.sql` and in both i18n catalogues. See **W5-B1** |
| 9 | Recovery-invite creation is gated on `moira:admins:manage`, "since recovery is a higher-privilege action" | Neither the scope nor the feature exists |
| 10 | Recovery is "audited as a distinct `admin_identity_recovered` event" | `src/i18n/catalog/notices.rs` says "deliberately no `admin_identity_recovered` notice". `ADMIN_IDENTITY_GRANT_EVENTS` is exactly `["granted","revoked","ownership_transferred"]` |
| 11 | Ten new Moira error codes are wave-5 work (`invite_expired`, `invite_already_consumed`, `invite_revoked`, `invite_email_mismatch`, `invite_domain_mismatch`, `invite_not_found`, `admin_identity_last_primary`, `admin_identity_not_found`, `admin_identity_already_revoked`, `admin_invite_expiry_too_long`) | **All ten shipped in wave 2**, each with a pinned emitter and status. Wave 5 adds none. The plan also never names the eleventh, `admin_identity_not_primary`, which is the one the ownership UI must actually render |
| 12 | Four new notices are wave-5 work | Three shipped (`admin_invite_created`, `admin_invite_redeemed`, `admin_identity_revoked`); the fourth (`admin_identity_recovered`) is deliberately absent |
| 13 | Wave 5 owns `migrations/0017_admin_invites.sql` | `0017` and `0018` shipped in wave 2; **#39** adds `0019_single_primary_admin.sql`. `0016` is a permanent gap. **Wave 5 as scoped needs no migration at all** |
| 14 | "`admin_identities` gains **no** new column" | It gained `is_primary boolean not null default false` in `0017`, with a partial index and a two-step one-shot backfill |
| 15 | This plan "changes the OpenAPI surface and must land **before** plan 05's gate freezes the spec" | The gate is live and wave 2 already regenerated the snapshot. Wave 5 as scoped adds **no route**, so `assert_eq!(operation_count, 151)` and the 99-path `BTreeSet` must both be **unchanged**. If either moves, a route was added by accident |
| 16 | "the operation count still **10**", re-verified at Wave 0 | Three different numbers, none of them 10 in that sense: the auth-provider family is **7** operations; the invite/identity family is **9**; the committed document is **151** operations over **99** paths. Already recorded as §0.7 #23 and repeated here because the wave-5 Wave-0 checklist and the Interfaces table both still say 10 |
| 17 | Ownership transfer is "two sequential calls, each with its own `Idempotency-Key` and `If-Match`" — promote the target, then demote the actor | **#39**: `set_primary` calls `demote_active_primaries_other_than` inside the same transaction, and `admin_identities_single_active_primary` refuses a second owner outright. **Transfer is ONE `PATCH`.** The second call would demote the operator just promoted, or 409 on a stale `If-Match`. Before #39 it is two calls — so this claim is wrong in one direction today and wrong in the other after #39. See **W5-B8** |
| 18 | "revoking the last admin leaves system-key break-glass as the re-entry path" | **#39**: `revoke_grant` clears `is_primary`, and the last-primary guard refuses that, so **a deployment's sole admin cannot be revoked through the API at all** — `revoking_the_owner_is_refused_by_the_last_primary_guard`. The repository doc comment states it as a consequence of D-F20, not an oversight. The UI must say so, not surface a bare 409 |

#### Console foundations wave 5 builds on

| # | Body says | Reality |
|---|---|---|
| 19 | `middleware.ts`'s host allow-list covers the new callback URLs and is unchanged | **There is no `middleware.ts` anywhere in the repository.** The session gate is `hasConsoleSession()` in the `(console)` route-group layout, which redirects to `/login` and fails **closed** on a Moira outage. Confirms §0.7 #15 — wave 3 never shipped one, and there is no host allow-list to inherit |
| 20 | The invite link is displayed in "plan 08's existing `console/components/molecules/OnceOnlySecretModal.tsx`" | It is `console/modules/secrets/OnceOnlySecretModal.tsx` — an **organism**, not a molecule, shipped in **wave 3**, not plan 08. Its props are already invite-shaped (`secret: string \| null`, `resource: AdminInviteRecord`, `notice: ResponseText`, `inviteBaseUrl`), and `secret === null` is the normal idempotent-replay case, not a failure |
| 21 | `SignInPanel` is "reused from plan 08" | **Holds in part** — the organism exists and is reusable, but it shipped in wave 3 at `console/modules/signIn/SignInPanel.tsx`, and it takes a fully-resolved `SignInPanelState` prop rather than fetching. A public invite page must resolve that state server-side itself |
| 22 | `layer-dependencies.test.ts` is "plan 08's" and "covers these files automatically" | **Holds in part.** The file exists — shipped in **wave 3** — and does cover new files automatically. But it is one of **five** guards, four of which the plan never names: `console/architecture.test.ts`, `tests/unit/architecture/{layer-dependencies,no-secret-props,server-only-guards,server-only-import}.test.ts`, plus `tests/unit/lib/no-hardcoded-copy.test.tsx`. Their combined rules are what actually shape this wave — see **W5-B3**, **W5-B7**, **W5-B10** |
| 23 | `console/tests/unit/architecture/bundle-scan.test.ts` is new work owned by this plan | Superseded. `console/e2e/secret-leak.e2e.ts` already scans `.next/**` build output, rendered HTML, RSC flight data, console output and page errors, with a vacuity guard and an armed once-only needle. Already §0.7 #29; repeated because the wave-5 test list and the Verification section both still name the file |
| 24 | `docs/admin-console.md` (created in plan 08) gains new sections | It does not exist. `docs/console-architecture.md` and `docs/console-storage.md` do; `docs/admin-identity-claiming.md` is the runbook the invitation runbook should sit beside |
| 25 | `lib/provider-secrets.ts` is reused verbatim; `console_auth.authProviderSecret` is already N-row | Renamed and re-homed in wave 1: `lib/console-secrets.ts` + `lib/console-secrets-postgres.ts` over table `console_provider_secret`. Already §0.4; still asserted in the wave-5 security-boundary and DoD sections |
| 26 | `loadAuthSettings()` / `invalidateAuthSettings()` | `loadAuthConfig()` / `resolveAuthConfig()` / `resetConsoleAuth()`. Already §0.4 |
| 27 | The console can already call the invite/identity endpoints | **One of nine.** `MOIRA_OPERATIONS` registers `createAdminInvite` only. `listAdminInvites`, `getAdminInvite`, `revokeAdminInvite`, `previewAdminInvite`, `redeemAdminInvite`, `listAdminIdentities`, `patchAdminIdentity` and `deleteAdminIdentity` are all unregistered, and every one needs a registry entry that `openapi-contract.test.ts` re-derives from the spec |
| 28 | The console types cover the invite/identity DTOs | **Holds in part.** `AdminInviteConstraint`, `AdminInviteStatus`, `AdminInviteRecord`, `AdminInviteCreateRequest`, `AdminInviteSecretResponse` and `AdminIdentityRecord` all shipped in wave 3 with their `*_CONTRACT` descriptors. **Four are missing:** `AdminInvitePreviewRequest`, `AdminInvitePreviewResponse`, `AdminInviteRedeemRequest`, `AdminIdentityPatchRequest` — each needing an interface, a descriptor, an `assertKeyContract<ExactKeys<…>>` line and a `SCHEMA_CONTRACTS` row |
| 29 | §0.1 **B9**: `/invite/[token]` cannot render provider-agnostic sign-in buttons because every read of auth configuration needs a credential (finding F15) | **No longer true, and this is a correction to §0.1.** F15 was resolved by the anonymous `GET /api/v1/admin/setup/sign-in-methods` returning `PublicSignInMethod`. `MOIRA_OPERATIONS.getSetupSignInMethods` has `credential: "none"`. Its registry comment states the limit precisely: the projection is enough to **render** a button and not enough to **resolve** the configuration behind one — resolution still needs `allowed_email_domains` and `trusted_jwt_issuer_id`, which the server already holds. So `/invite/[token]` is unblocked |
| 30 | The accept-invite intent is carried "in a signed, short-lived httpOnly cookie … mirroring 08's claim-intent pattern" | **There is no claim-intent pattern to mirror.** No cookie is set anywhere in `console/`; `lib/setup-flow.ts` implements provisioning but nothing under `app/` imports it, and there is no `/setup` route. This is greenfield design, not propagation |
| 31 | New molecules `ExpiryPicker,CopyableLink,ScopeChipList,DangerConfirmDialog` and atoms `Tooltip,Avatar,Divider` | **Holds in part.** None exists — but two are already solved and one is obsolete. `CopyButton` (atom, wave 3) deliberately takes an element **id**, never the value, which is exactly what keeps the token in one place; `CopyableLink` taking a link that contains the token would re-open that. `Dialog` (atom, wave 3) is a native `<dialog>` with platform focus containment, which is what `DangerConfirmDialog` should compose rather than re-implement. `ScopeChipList` is obsolete: ownership is `is_primary`, not a scope. See **W5-D6** |
| 32 | Pages carry `actions.ts` server actions (`app/(console)/admins/actions.ts`, `app/invite/[token]/actions.ts`) | **There is not one `actions.ts` in `console/`.** `nextCookies()` is deliberately absent from the Better Auth plugin list and there is no module-scope `auth` object to import; `SignInPanel` posts to the mounted route handler with `fetch` instead, and its header says why. The mutation transport for this wave is an **undecided design**, not an inherited one. See **W5-B7** and **W5-D5** |
| 33 | The console i18n keys this wave needs exist or are reused ("no new i18n keys are needed for D7") | The catalogue has **45 keys** across `console.{error,a11y,meta,page,signIn,action,secret}`. There is **no** `console.admins.*`, `console.invite.*`, `console.sessions.*` or `console.authSettings.*` namespace. Every key the wave-5 tables name is new — and each must be added to **both** `lib/i18n/keys.ts` and `lib/i18n/catalog.en.ts` (the catalogue is typed `Record<ConsoleMessageKey, CatalogEntry>`, so a mismatch is a `tsc` failure before any test runs) |
| 34 | "`i18n-catalog-coverage.test.ts` and `no-hardcoded-copy.test.tsx` extend to cover this plan's new files automatically; both must stay green" | **Holds** — both walk the tree from a derived root set, so `modules/admins/` and `app/(console)/admins/` are picked up with no edit. Two teeth the body never mentions: the coverage gate is **bidirectional** (a catalogue key nothing emits fails, and `tests/`/`e2e/`/`lib/i18n/**` are excluded from the emitter scan, so a key used only by its own test does not count), and **no two keys may share the same English message** |
| 35 | "`@axe-core/playwright` on **every new page route** … zero critical/serious violations gates CI" | **Holds mechanically and is vacuous in practice.** `discoverPageRoutes` finds new routes with no edit, and the tags are `wcag2a/2aa/21a/21aa` with `critical`+`serious` blocking. But there is no authenticated Playwright state, so `page.goto("/admins")` follows the layout's redirect and audits `/login`. That is already true of `/` today. See **W5-B6** |
| 36 | `authorization-denial.e2e.ts` and `i18n-message-key.e2e.ts` are "extended from 08" | Neither exists. `console/e2e/` holds exactly `a11y.e2e.ts`, `secret-leak.e2e.ts` and `smoke.e2e.ts` plus five support modules. Both are **new files** |

#### Session management

| # | Body says | Reality |
|---|---|---|
| 37 | Better Auth DB-backed sessions came from plan 08, which "gave the console its own `console_auth` schema" | Plan 08 shipped `memoryAdapter`. Durable storage shipped in **wave 1**. §0.1 **B3** recorded the old state; this is its resolution, not a confirmation of the body |
| 38 | A real `session` table with created-at, last-updated, IP and user-agent exists | **Holds now.** `console/db/migrations/0001_better_auth_core.sql` creates `session(id, expiresAt, token unique, createdAt, updatedAt, ipAddress, userAgent, userId)` with `session_userId_idx`. `CONSOLE_DATABASE_URL` is **required in production**; the `memoryAdapter` fallback is reachable only outside production, which is where `bun test` and `next dev` run |
| 39 | "revoke one session, or revoke all others" is available and genuine | **Could not be established.** `console/node_modules` is not installed in this worktree, so Better Auth 1.6.25's `listSessions` / `revokeSession` / `revokeOtherSessions` surface was not read. Recorded as unverified rather than assumed — this is exactly the class of claim §0.7 and the ledger's method note say to check before building on |
| 40 | "Session lifetime/idle policy (`session.expiresIn`, `session.updateAge`) becomes an operator-editable auth setting persisted in Moira, applied at runtime" | No such field exists on `auth_provider_settings`, in any DTO, or in the committed spec. This is a **frozen-contract change** — a migration, DTO fields, a spec regeneration and a schema delta — hidden inside a sentence about a UI screen |
| 41 | The "best-effort" caveat is removed from `docs/admin-console.md` | The file does not exist (see #24) |

#### Tests, decisions and findings

| # | Body says | Reality |
|---|---|---|
| 42 | The nine named Rust redeem tests are wave-5 deliverables, in a new `tests/admin_invite_lifecycle.rs` | The suite shipped in **wave 2** with sixteen tests, and **#39** adds ~786 lines more. Wave 5 writes no Rust test for the invite path |
| 43 | Each of those nine named behaviours is covered | **Holds in part: seven of nine.** All nine are absent *by exact name*; seven have a semantically stronger equivalent (e.g. `a_policy_denied_redemption_leaves_the_invite_pending_and_the_same_link_still_works`, which asserts the invite **row's** `status` rather than a replayed response — the correction the ledger's METHOD NOTE demands). `redeem_denies_when_governing_provider_is_disabled` has **no equivalent on the redeem path**. `recovery_invite_gets_no_domain_policy_exemption` is unbuildable (#8) |
| 44 | New console unit tests are needed for the new lib modules, organisms, molecules and atoms | **Holds.** Note the shipped standard: every organism test asserts rendered copy against `CONSOLE_CATALOG[key].message`, **never** an English literal, because "a literal assertion passes whether or not `t()` was ever called". Also note two shipped components with **no** unit test — `CopyButton` and `Dialog` — which wave 5 will lean on |
| 45 | Product decision 1 — single primary vs. multiple `moira:admins:manage` holders — is open and "blocking before Wave 3" | **Resolved**, twice over: by the user on 2026-07-31 ("ownership is a SINGLE primary, set at claim time") and by **#39**'s decision **D-F20**, which is already written into §0.2 on that branch. The framing was also wrong — it is not a scope |
| 46 | Product decision 2 — default invite expiry, recommend ≤72h enforced as a server-side hard cap | **Holds and is already shipped:** `MAX_INVITE_EXPIRY_SECONDS = 72 * 60 * 60`, refused rather than clamped (`admin_invite_expiry_too_long`, 422). The plan never mentions the **floor**, `MIN_INVITE_EXPIRY_SECONDS = 60`, which `ExpiryPicker` must respect |
| 47 | Product decision 4 — which new admin screens are MVP-of-this-plan — needs product sign-off | **Holds as open.** Taken here as **W5-D9** under the standing decide-rather-than-ask authority |
| 48 | Invite tokens are Argon2id+pepper hashed, single-use, time-capped, covered by `tests/admin_invite_lifecycle.rs` | **Holds**, shipped. `ApiKeyHasher` under namespace `moira_inv`, prefix-lookup-then-verify, `select … for update` single-winner, and a concurrent-redemption test |
| 49 | Redeem validates outside the transactional envelope, so a denial does not consume the invite | **Holds**, shipped, and is load-bearing for finding **F19**: the ordering's real purpose is that `insert_grant` must not pre-empt the invite's own refusal, or a stranger with a leaked token learns whether an arbitrary identity already holds admin |
| 50 | The redeem token carries no `scope`/`scp` claim and binds `sub` to the IdP subject | **Holds.** Redeem routes through `AuthService::verify_trusted_jwt_identity`, not `authenticate_admin`, and its spec `security` is `bearerAuth` alone |
| 51 | D3 — an invite is never an exemption from `allowed_email_domains`, identically for routine and recovery invites | **Holds** for routine invites, shipped and tested. The recovery half is vacuous (#8) |
| 52 | **F21** is plan 09's to fix — and §0.7's **W4-D6** reassigns it to wave 4 on cost grounds | **Both are wrong. F21 is already fixed, in PR #39, with a test.** `redeem_invite`'s `Err` arm on `fix/findings-sweep` reads `if !matches!(error, AppError::Replayed(_)) { … record_admin_invite_denied(…) }`, and `an_idempotent_replay_does_not_count_a_second_invitation_or_redemption` drives a cacheable 409 (`admin_identity_already_claimed`) twice under one `Idempotency-Key` and asserts `invite_outcome(…, "other") == 1.0` after the replay. That assertion would read `2.0` with the guard removed, so it is a test that works. **Neither wave 4 nor wave 5 may implement it** |

**Holds as written (do not re-litigate):** the invite/ownership data-flow *shape* (create → share out of band → preview → sign in → redeem); D3 and D5 inheritance with no invitation carve-out; the token in request bodies only, never a query string; once-only display; the separation of `invite_*_mismatch` from `admin_claim_domain_not_allowed` on remedy grounds; `InviteAdminForm`'s pre-submit allow-list gate as the correct fix for the stranding case (never a policy carve-out); `*.e2e.ts` under `console/e2e/`; Atomic-Design placement with organisms in `modules/`; no console-sent invite emails; and "the console's client-side capability check is UI gating only — Moira is the authority".

### §0.8.2 Blockers, ranked

**W5-B1 — the recovery UI is specified over a backend that does not exist, and building it is a Moira wave, not a UI wave. (Severity: highest — it is a third of the stated scope, and every artefact named for it is unbuildable.)**

Wave 2 took **decision D-W2-1** and omitted `is_recovery` and `replaces_admin_identity_id` deliberately: *"a column no code writes is the schema equivalent of a catalog entry with no emitter."* There is no route, no DTO field, no service method, no error code, no notice, no audit event and no test. `RecoveryPanel`, `recovery.e2e.ts`, `recovery_invite_gets_no_domain_policy_exemption` and the `admin_identity_recovered` event therefore cannot be written at all.

Doing it properly costs: one migration (two columns plus a CHECK that `replaces_admin_identity_id` is set iff `is_recovery`), two DTO changes, an atomic revoke-and-grant swap inside the existing envelope, a new error code and notice with pinned emitters, an OpenAPI regeneration, and a mid-transaction failure-injection test. That is the same size as wave 2's own grant-administration slice. It is not UI work and must not be smuggled into a UI wave. See **W5-D1**.

*The D-W2-1 decision itself is an escalation:* it lives only in a migration comment and two catalogue comments. It is not in `plans/reports/EXECUTION-LEDGER.md` and was not in this plan's §0 until now. A decision that removes a third of a later wave's scope must be findable from the plan, not from `git log`.

**W5-B2 — `redeem` cannot be registered in `MOIRA_OPERATIONS` under any credential requirement that exists. (Severity: high — nothing in the redemption path works until this is designed.)**

`MoiraCredentialRequirement` is `"none" | "system_key_only" | "admin"`, and `openapi-contract.test.ts` asserts a branch per value: `none` ⇒ the operation declares **no** `security`; `system_key_only` ⇒ exactly `["systemKeyAuth"]`; anything else ⇒ the scheme list must **contain both** `systemKeyAuth` and `bearerAuth`. `redeem_admin_invite`'s committed security is `[{ "bearerAuth": [] }]` alone — deliberately, so no token-asserted scope and no bootstrap credential can reach a path that mints a grant.

So `credential: "admin"` fails the contract test, and `credential: "none"` sends no `Authorization` header and 401s. Worse, `#buildHeaders`'s `admin` arm prefers the **system key when one is present**, which on the redeem path would send the console's bootstrap credential on an invitee's request. A fourth variant is required, with its own contract-test branch and its own refusal when a system key is supplied. See **W5-D3**.

**W5-B3 — the two DTOs the redeem path needs fail a shipped architecture guard on the field name `token`. (Severity: high — reds `bun test` on the first commit, and the obvious fix weakens a real rule.)**

`server-only-guards.test.ts` asserts that **no** Moira DTO in `lib/types.ts` declares a field matching `/(secret|masked|fingerprint|token|password|api_?key|private_?key|credential)/i`. Exactly one interface is exempt — `AdminInviteSecretResponse`, with its member set pinned to `["notice","resource","secret","secret_retrievable"]` — and exactly two field names are exempt: `token_url` and `setup_token`. **Every exemption is checked in reverse**, so an exemption that carves out nothing is itself a failure.

`AdminInvitePreviewRequest.token` and `AdminInviteRedeemRequest.token` both match. Widening `SECRET_DTO_FIELD_PATTERN` would silently un-guard `secret`, `password` and `api_key` across every DTO. See **W5-D4**.

**W5-B4 — mounting `OnceOnlySecretModal` on a page fails `secret-leak.e2e.ts`, by design. (Severity: high — deliberate, documented, and unscheduled.)**

`no shipped route mounts OnceOnlySecretModal yet — and this fails when one does` walks `app/**` for the literal `modules/secrets/OnceOnlySecretModal` and asserts the result is `[]`. Its comment names this wave: *"The moment a page does mount it — plan 09's `/invite/[token]`, or an admin surface that creates one — this assertion FAILS, and whoever wrote that page has to arrange for the fixture token to flow through it. Without this, that author would inherit a gate that reads as covering the render and does not."*

The fixture is already seeded: `ONCE_ONLY_SECRET_FIXTURE = "moira-invite-token-fixture-8c41ab07f2de9536"` is unconditionally in the needle set, and its length / whitespace / slash constraints are separately asserted so it cannot be silently dropped by `isUsableNeedle`. Wave 5 must replace the negative assertion with a positive one that renders the modal with that token on a real route and confirms it appears in **zero** captured response bodies, RSC payloads, build outputs and console messages — except the one intentional reveal.

**W5-B5 — `/invite/[token]` is the repository's first dynamic route and reds the coverage guard until a fixture is registered. (Severity: medium-high — a one-line fix nobody has scheduled, with a design consequence.)**

`DYNAMIC_ROUTE_FIXTURES` in `e2e/support/routes.ts` is empty, and `a11y.e2e.ts` asserts `uncoveredRoutes(routes)` is `[]` — a dynamic route with no fixture is a **failing** condition, not a skip. Adding an entry is trivial; the consequence is not. The a11y test asserts `response.status() < 400`, so the fixture token's page must **render**, which means an invalid or expired token must produce an accessible error state rather than a 404. That is the right design; it should be chosen, not discovered.

**W5-B6 — the a11y gate is vacuous for every route inside `(console)`, which is this wave's entire surface. (Severity: medium-high — a green gate that audits the wrong page, and the exact signature the handoff warns about.)**

There is no authenticated Playwright storage state anywhere in `console/e2e/`. The `(console)` layout redirects an unauthenticated visitor to `/login`, and `page.goto` follows redirects, so `no critical or serious axe violations on /` **currently audits `/login`**. The e2e environment makes this certain, not merely likely: `CONSOLE_PUBLIC_ORIGIN=https://console.e2e.invalid`, an unreachable DSN, and `smoke.e2e.ts` positively asserting the root route contacts no external origin — so `consoleRuntime()` always fails and the gate always redirects.

Every screen this wave adds under `(console)` inherits that silence, and so would any `authorization-denial.e2e.ts` written the same way. This is the "leak suite passing a deliberately injected leak under `E2E_SKIP_BUILD=1`" pattern, one wave later. See **W5-D7**.

**W5-B7 — a client organism cannot reach the Moira client, and there is no server-action precedent to follow. (Severity: medium-high — an architecture decision the plan assumes was already made.)**

`layer-dependencies.test.ts` rule 5 forbids any `"use client"` module from importing a credential-carrying module, where that set is **derived** (reads `process.env`, imports `pg`, sends `X-Moira-System-Key` or `Authorization`, constructs an AEAD, handles a `clientSecret`, calls `betterAuth(`) and closed transitively over value imports. `lib/moira-client.ts` is in it by name. A client organism may import only `lib/errors.ts`, `lib/types.ts`, `lib/moira-keys.ts` and `lib/i18n/**`.

So every mutation — create invite, revoke invite, transfer, revoke grant, redeem — needs a server-side transport. The plan says `actions.ts`; **there is no `actions.ts` in the repository**, `nextCookies()` is deliberately excluded from the Better Auth plugin list, and the one shipped interactive organism posts to a route handler instead. Choosing server actions means adopting `nextCookies()` — a change to the auth instance, in a wave that should not be touching it. See **W5-D5**.

**W5-B8 — transfer is one call after #39 and two before it, and the sole-admin row can no longer be revoked. (Severity: medium — ships a wrong action or a bare 409.)**

After #39, `set_primary` demotes every other active primary in the same transaction and `admin_identities_single_active_primary` refuses a second owner; the plan's second "demote the actor" call would demote the person just promoted, or 409 on a version it no longer holds. And because `revoke_grant` clears `is_primary`, revoking the owner is refused — so on a fresh deployment with one admin, `DELETE /admin-identities/{id}` is unavailable for that row. `AdminTable` must render that as a stated rule with the remedy ("transfer ownership first"), not as a failed request.

**W5-B9 — the `authorization-denial` case the plan names cannot be constructed. (Severity: medium — a test that would pass by never exercising anything.)**

"an identity holding `moira:admin` but **not** `moira:admins:manage` can view `/admins` but cannot transfer or revoke" describes a scope that does not exist and, per §0.1 **B1**, could not be withheld if it did. The constructible case is an **active grant with `is_primary = false`**, which meets `require_primary_actor` and receives `403 moira.error.admin_identity_not_primary`. Assert that code, not the absence of a scope.

**W5-B10 — new copy must land with its emitter, in two files, and may not reuse an existing English string. (Severity: medium — three separate ways to be red for reasons the plan never states.)**

A key in `lib/i18n/keys.ts` without an entry in `catalog.en.ts` is a **typecheck** failure. An entry whose key nothing emits fails `i18n-catalog-coverage.test.ts`'s orphan check — and `tests/`, `e2e/` and `lib/i18n/**` are excluded from the emitter scan, so a key referenced only by its own test does not count as an emitter. Two keys sharing one English message fail. No key may match `/rotate.*secret/i`. `no-hardcoded-copy.test.tsx` additionally forbids literal `aria-label`, `title`, `placeholder`, `alt` and default-parameter copy, so every accessible name in every new component is a catalogue key too.

One trap for any new scanner this wave writes: `lib/setup-flow.ts` contains a literal NUL byte, which makes `grep`/`rg` classify it as binary and skip it silently. Read files with `readFileSync(path, "utf8")`, as the shipped scanners do.

### §0.8.3 Decisions taken for wave 5, each with its reversal condition

Taken under the standing "decide rather than ask" authority. Each belongs in
`plans/reports/EXECUTION-LEDGER.md` alongside the wave-1–4 decisions.

**W5-D1 — recovery is CUT from wave 5. Wave 5 ships invitations and ownership only.**
Per **W5-B1**, recovery is a Moira backend slice (migration, DTOs, atomic swap, code, notice, spec
regeneration, failure-injection test) with a thin UI on top, and wave 2 removed it deliberately. Half
of it — "revoke a locked-out admin's grant, then invite their replacement" — is **already achievable
with what exists**, as two ordinary operations, and `AdminTable` plus `InviteAdminForm` expose both.
What is genuinely missing is only the *atomicity* of the swap, and atomicity is a backend property.
Building a `RecoveryPanel` that performs two independent calls while the plan promises "never a window
where both or neither exist" would be the appearance of a feature — the same failure mode the session
decision was taken to avoid.
*Reversal condition:* when a wave takes the Moira change end to end — `is_recovery`,
`replaces_admin_identity_id`, the in-envelope swap, `admin_identity_recovered`, and the
mid-transaction failure-injection test asserting neither half persists without the other. The UI is a
follow-on to that, never its driver.

**W5-D2 — session management STAYS CUT, and the reversal condition is not yet met.**
The recorded reversal condition has two halves: *durable storage ships* and *the invitation flow is
green*. The first is now met — wave 1 (PR #36) shipped `console_auth`, the durable Better Auth
adapter and the durable secret store. The second is what wave 5 builds, so it cannot be met at wave-5
planning time. Three further reasons, each independent: the plan's session scope silently includes an
operator-editable lifetime/idle policy **persisted in Moira** (#40), which is a frozen-contract change
and must stay cut regardless; `bun test` and `next dev` default to `memoryAdapter`, so a sessions
screen's unit tests would exercise a store the shipped feature does not use; and per **W5-B6** its
a11y and e2e coverage would land behind the same silence as every other gated route. Finally, the
capability it competes with already exists and is stronger: `DELETE /admin-identities/{id}` revokes
*authorization*, where a session revocation only ends *authentication*.
*Reversal condition:* restore it as its own small wave when all three hold — (a) the invitation flow is
green on `main`, (b) the a11y gate is non-vacuous for routes inside `(console)`, and (c) Better Auth
1.6.25's `listSessions`/`revokeSession`/`revokeOtherSessions` surface has been read and confirmed —
drift-table row #39 records that it was **not** verified here, and building a revocation screen on an
unread API is how the last four findings in this project started. The Moira-persisted lifetime policy
(#40) stays out of scope even then, as its own decision.

**W5-D3 — add a fourth `MoiraCredentialRequirement`, `"bearer_only"`, rather than registering redeem as `admin`.**
It carries the invariant that matters: this operation accepts the invitee's freshly-minted, grantless
JWT and **must not** accept the console's bootstrap system key, because a system-key redemption would
be the console granting admin to an identity of its own choosing. `#buildHeaders` must throw
`MoiraClientContractError` if a system key is supplied, mirroring `system_key_only`'s refusal of a
bearer. `openapi-contract.test.ts` gains a branch asserting the declared scheme list is exactly
`["bearerAuth"]`.
*Reversal condition:* if the committed spec ever adds `systemKeyAuth` to `redeem_admin_invite`, this
variant collapses back into `admin` — but that spec change is itself the decision, and it needs its
own argument about why the console should be able to self-grant.

**W5-D4 — carve out the two redeem/preview DTOs by name in `server-only-guards.test.ts`; do not widen `SECRET_DTO_FIELD_PATTERN`.**
Add `AdminInvitePreviewRequest` and `AdminInviteRedeemRequest` to the interface exemption list with
their member sets **pinned**, exactly as `AdminInviteSecretResponse` is, and keep the reverse check
that every exemption still carves something out. Widening the pattern to drop `token` would un-guard
every future DTO. Pin the member sets so a later field cannot ride in on the exemption.
*Reversal condition:* if a third DTO needs `token`, stop exempting and introduce a typed
`InviteToken` newtype whose name does not match the pattern — at three exemptions the list has become
the thing it was guarding against.

**W5-D5 — mutations go through route handlers under `app/api/**`, not server actions.**
It is the shipped precedent (`SignInPanel` → `POST /api/auth/sign-in/oauth2`), it keeps
`nextCookies()` out of the Better Auth plugin list in a wave that should not be touching the auth
instance, and it keeps the credential graph on the server side of a boundary the architecture guards
already understand. Route handlers under `app/api/**` sit outside every route group, so they are also
outside the `(console)` session gate — each one must therefore re-check the session itself, which is
explicit rather than inherited.
*Reversal condition:* if a wave adopts `nextCookies()` deliberately, with its own tests for cookie
propagation and CSRF, server actions become available and this collapses to a style preference.

**W5-D6 — of the seven new molecules and atoms the plan names, build two: `ExpiryPicker` and `DangerConfirmDialog`. Drop `CopyableLink`, `ScopeChipList`, `Tooltip`, `Avatar` and `Divider`.**
`CopyableLink` is actively harmful: `CopyButton` already takes an element **id** rather than a value,
which is the design that keeps the token in one expression and one DOM node, and a prop named `link`
carrying a token would also brush against `no-secret-props`. `ScopeChipList` is obsolete — ownership
is `is_primary`, and `granted_scopes` is `["moira:admin"]` on every grant this plan creates.
`Tooltip`, `Avatar` and `Divider` are decoration with no stated requirement; a tooltip in particular
is an accessibility liability that the console's "every state has an ARIA counterpart" standard would
have to earn. `DangerConfirmDialog` composes the shipped `Dialog` atom (native `<dialog>`,
platform focus containment, no backdrop-click dismissal) rather than re-implementing focus management.
`ExpiryPicker` must respect **both** documented bounds: `MIN_INVITE_EXPIRY_SECONDS = 60` and
`MAX_INVITE_EXPIRY_SECONDS = 259200`.
*Reversal condition:* each returns the moment a shipped screen needs it and a test can name the
behaviour it adds. "The plan listed it" is not that.

**W5-D7 — make the a11y walker assert it audited the route it asked for; do not ship an authenticated e2e storage state in this wave.**
Add `expect(new URL(page.url()).pathname).toBe(route.pattern)` (or the fixture URL for a dynamic
route) beside the existing `< 400` assertion. That converts **W5-B6**'s silence into a visible
failure for `/`, `/admins` and every future gated route — which is the honest state, and is
information the next wave needs. Building an authenticated storage state requires a mock IdP inside
the Playwright environment, which the e2e harness does not have today (`tests/support/mock-idp.ts`
serves the unit and integration suites, not `console/e2e/`); doing it properly is its own piece of
work and doing it badly would produce exactly the green-but-empty gate this decision exists to expose.
*Reversal condition:* when an authenticated Playwright project exists, keep the URL assertion **and**
add the authenticated run — the assertion is what proves the authenticated run is doing anything.

**W5-D8 — wave 5 makes no Moira change. `operation_count` stays 151 and the path `BTreeSet` stays at 99.**
Every Moira capability this wave needs shipped in wave 2, and D-F20 shipped in #39. The correct
outcome of `UPDATE_SNAPSHOTS=1 cargo test --lib http::tests::committed_openapi_matches_the_generated_document`
in this wave is **no diff at all**; if `docs/openapi.json` moves, something was added by accident and
must be found rather than regenerated over. The four spec gates
(`every_if_match_operation_declares_the_documented_precondition`,
`atomic_admin_idempotency_contract_is_explicit`, `once_only_key_responses_use_the_secret_envelope`,
and the unauthenticated allow-list) already cover this surface and must stay untouched.
*Reversal condition:* recovery (**W5-D1**) or the Moira-persisted session policy (#40). Both are
separate waves with their own regeneration.

**W5-D9 — the MVP screen set is `/admins` and `/invite/[token]`. `/settings/sessions` is cut; `/settings/auth` belongs to wave 4.**
This resolves the body's open product decision 4. `/admins` carries the admin list, the invite form,
the invite list, ownership transfer and grant revocation; `/invite/[token]` is the public redemption
page. Wave 5 also owns the `(console)` chrome the layout deferred — its header says so in as many
words: *"navigation, a header and a sign-out control arrive with the surfaces they navigate to."*
Without navigation, `/admins` is reachable only by typing the URL.
*Reversal condition:* none needed for the cut screens — they return with **W5-D1** / **W5-D2**. If
product wants a different first screen, that is a scope call, not a correction.

**W5-D10 — wave 5 sequences AFTER PR #39 merges, and is not implemented against `main` as it stands.**
Three of this wave's findings invert depending on #39: transfer is one call or two (**W5-B8**), the
sole admin is revocable or not (#18), and ownership is reachable by a JWT admin or only by
break-glass (F20). Implementing the ownership UI against pre-#39 `main` means writing the two-call
transfer and then deleting it. The migration number follows the same rule: wave 5 needs none, but if
one is ever added it is `0020` after #39 and `0021` if wave 4 has also landed — **re-verify with
`git ls-files migrations/` at branch time; two migrations with one number is a hard failure.**
*Reversal condition:* if #39 is abandoned rather than merged, F20 reopens, F21 reopens with it, and
wave 5's ownership half must be re-planned from the top — it would be building a transfer UI for a
flag nothing sets.

### §0.8.4 Ordered task list for the wave-5 implementation

Sized for one agent. **Contract and guards first** — four shipped gates go red on the first commit,
and finding that out after the screens are written is the expensive order.

**Phase 0 — preconditions (do not start without these)**

1. Confirm PR #39 is merged (**W5-D10**). Verify on `main`: `migrations/0019_single_primary_admin.sql`
   exists; `set_primary` contains `demote_active_primaries_other_than`; `redeem_invite`'s `Err` arm
   contains `!matches!(error, AppError::Replayed(_))`.
2. Re-verify the migration inventory with `git ls-files migrations/` even though this wave adds none.
3. Record **D-W2-1** (recovery deliberately unbuilt) and **W5-D1** in
   `plans/reports/EXECUTION-LEDGER.md`. It currently exists only in a migration comment.

**Phase 1 — the console↔Moira contract (no UI yet, and every gate stays green)**

4. `console/lib/types.ts`: add `AdminInvitePreviewRequest`, `AdminInvitePreviewResponse`,
   `AdminInviteRedeemRequest`, `AdminIdentityPatchRequest` — each with its interface, its
   `*_CONTRACT` descriptor, its `assertKeyContract<ExactKeys<…>>` line, and a row in
   `SCHEMA_CONTRACTS` (a descriptor missing from that array is checked by nothing).
5. **Before step 4 can pass**, apply **W5-D4** to `server-only-guards.test.ts`: exempt the two `token`
   DTOs by name with pinned member sets, keeping the reverse check.
6. `console/lib/moira-client.ts`: add the eight missing `MOIRA_OPERATIONS` entries, transcribed from
   `docs/openapi.json` and **not guessed** — `preview` is `credential: "none"`, `redeem` is the new
   `"bearer_only"` (**W5-D3**), the rest are `"admin"`; `Idempotency-Key` is declared on create,
   revoke, redeem and delete and **not** on the reads; `If-Match` is required on `patch` and forbidden
   everywhere else in this family.
7. Add the `"bearer_only"` arm to `#buildHeaders` (throw if a system key is supplied) and its branch
   in `openapi-contract.test.ts` (declared schemes are exactly `["bearerAuth"]`). Verify the shipped
   `"of the ten operations the console binds to, exactly two declare Idempotency-Key"` test is
   unaffected — its `boundNames` list is hardcoded and excludes this family.
8. Client methods: `listAdminInvites`, `getAdminInvite`, `revokeAdminInvite`, `previewAdminInvite`,
   `redeemAdminInvite`, `listAdminIdentities`, `patchAdminIdentity`, `deleteAdminIdentity`. Use
   `ifMatchFor(record)` for the patch. Remember `AdminInviteSecretResponse.secret === null` on replay
   is the **normal** case, not an error.
9. `console/lib/moira-keys.ts`: mirror the eleven wave-2 codes this wave renders —
   `invite_not_found`, `invite_expired`, `invite_already_consumed`, `invite_revoked`,
   `invite_email_mismatch`, `invite_domain_mismatch`, `admin_invite_expiry_too_long`,
   `admin_identity_not_found`, `admin_identity_already_revoked`, `admin_identity_last_primary`,
   `admin_identity_not_primary` — plus the notices `admin_invite_created`, `admin_invite_redeemed`,
   `admin_identity_revoked`. Give each an entry in `MOIRA_CODE_REMEDIES` (`errors.test.ts` requires
   one) and check `moira-keys.test.ts` still finds every mirrored key in
   `docs/i18n-response-catalog.json`.
10. `console/lib/invites.ts` (`// @server-only` **first line** and `import "server-only";` — the
    derived-credential guard requires both): token handling, and the assertions the plan names
    (the raw token is never logged, never serialised into a client payload, never placed in a query
    string).

**Phase 2 — i18n, before the components that emit it (they must land together)**

11. Add every new key to **both** `lib/i18n/keys.ts` and `lib/i18n/catalog.en.ts`. Namespaces:
    `console.admins.*` and `console.invite.*`. Minimum set implied by the screens —
    page and section titles; the invite form's labels, hints and the two pre-submit refusals
    (`invite_domain_not_in_allow_list`, `no_enabled_provider`); the invite list's status and
    `expired` copy; the admin table's `primary` badge, transfer and revoke controls and their
    confirmations; the "the owner cannot be revoked — transfer first" explanation (**W5-B8**);
    the invitee-facing `domain_not_allowed` title/body/action; and an `aria-label` for every list,
    dialog and live region, because `no-hardcoded-copy` forbids literal ones.
12. Land keys and emitters in the **same commit** (orphan keys fail), give every entry a distinct
    English message (duplicates fail), and add no key matching `/rotate.*secret/i`.

**Phase 3 — organisms and the chrome**

13. `(console)` chrome: navigation, header and sign-out, in `app/(console)/layout.tsx`. Without it
    `/admins` is unreachable except by typing the URL, and the layout's own header schedules it here.
14. `console/modules/admins/{AdminTable,InviteAdminForm,TransferPrimaryPanel}.tsx`. `AdminTable`
    reads `is_primary` (**not** `granted_scopes`), disables revoke on the owner row with the keyed
    explanation, and renders transfer as **one** `PATCH` (**W5-B8**).
15. `console/modules/invite/InviteAcceptPanel.tsx`. Renders `admin_claim_domain_not_allowed` as an
    actionable instruction — never the generic error banner — and never conflates it with
    `invite_email_mismatch`/`invite_domain_mismatch`.
16. Molecules per **W5-D6**: `ExpiryPicker` (honouring the 60 s floor and the 72 h cap) and
    `DangerConfirmDialog` (composing the shipped `Dialog`). No `CopyableLink`, no `ScopeChipList`.
17. Follow the shipped a11y standard rather than importing one: `role="alert"` for errors,
    `role="status"` + `aria-live="polite"` for async updates with the region **present before** it is
    populated, `aria-busy` on loading controls, `aria-invalid` + `aria-describedby` wired through
    `FormField`, decoration `aria-hidden`, and every accessible name from the catalogue. Unit-test
    rendered copy against `CONSOLE_CATALOG[key].message`, never an English literal.

**Phase 4 — routes and transport**

18. `app/(console)/admins/page.tsx` — thin: guard, fetch, render.
19. Route handlers under `app/api/**` for every mutation (**W5-D5**), each re-checking the session
    itself because `app/api/**` sits outside the `(console)` gate.
20. `app/invite/[token]/page.tsx` — public, outside `(console)`. Server-side: `preview` with the
    token in the **body**, then `getSetupSignInMethods()` (anonymous — #29) to render `SignInPanel`.
    An invalid or expired token must render an accessible error **page**, not a 404 (**W5-B5**).
21. `InviteAdminForm`'s pre-submit gate reads the enabled providers' `allowed_email_domains` through
    `getSetupAuthMethods()` (credential `admin` — the inviting admin's bearer token). Blocks
    submission for an uncovered domain and disables entirely when no provider is enabled. UI gating
    only: Moira's redeem-time check remains the authority and the console is never more permissive.

**Phase 5 — the four gates that go red, closed deliberately**

22. `e2e/support/routes.ts`: register `"/invite/[token]"` in `DYNAMIC_ROUTE_FIXTURES` (**W5-B5**).
23. `secret-leak.e2e.ts`: replace `no shipped route mounts OnceOnlySecretModal yet` with a positive
    assertion that renders the modal with `ONCE_ONLY_SECRET_FIXTURE` on a real route and confirms it
    appears in **zero** captured bodies, RSC payloads, build outputs and console messages, with an
    **empty** violation set (**W5-B4**).
24. `a11y.e2e.ts`: add the final-URL assertion from **W5-D7**, and expect `/` and `/admins` to fail
    it until an authenticated e2e path exists. Record that failure honestly; do not weaken the
    assertion to make it pass.
25. Confirm `layer-dependencies`, `no-secret-props`, `server-only-{guards,import}`,
    `no-hardcoded-copy` and `i18n-catalog-coverage` are green with the new files.

**Phase 6 — e2e and verification**

26. `console/e2e/invite-redeem.e2e.ts`, `invite-negative.e2e.ts`, `invite-domain-policy.e2e.ts`,
    `ownership-transfer.e2e.ts`, and **new** `authorization-denial.e2e.ts` / `i18n-message-key.e2e.ts`
    (#36 — neither exists to extend). The denial case is a non-primary grant meeting
    `admin_identity_not_primary`, not a missing scope (**W5-B9**). No `recovery.e2e.ts`, no
    `sessions.e2e.ts`.
27. `bun install --frozen-lockfile && bun run typecheck && bun run lint && bun test && bun run build && bun run e2e`.
28. Rust: `scripts/gates.sh` with
    `MOIRA_TEST_DATABASE_URL='postgres://postgres:postgres@127.0.0.1:5432/moira'` **exactly** —
    unchanged code, but the gate proves it. Expect `operation_count == 151` and no
    `docs/openapi.json` diff (**W5-D8**).
29. Docs: extend `docs/console-architecture.md` (**not** `docs/admin-console.md`) with the invitation
    flow, ownership transfer, and the two consequences of D-F20 an operator will meet — a sole admin
    cannot be revoked, and transfer moves the flag. Put the invitation runbook beside
    `docs/admin-identity-claiming.md`.

### §0.8.5 Escalations — written down, not blocking

1. **TEST INTEGRITY — the a11y gate audits `/login` for every route inside `(console)`** (**W5-B6**).
   Already true for `/` on `main`. It is not a wave-5 regression, but wave 5 is what makes it
   material, and any `authorization-denial.e2e.ts` written without noticing would be green and empty.
   **W5-D7** makes it visible rather than fixing it, deliberately.
2. **SECURITY — nothing welds the `moira.*` keys the console actually renders to
   `MIRRORED_MOIRA_KEYS`.** `t(notice.message_key, …)` and `t(state.messageKey, …)` pass arbitrary
   server-supplied keys straight through; the mirror is hand-maintained and is imported by **no**
   application module, only by two tests. Wave 5 renders eleven new Moira codes and three notices. The
   failure mode is mild (an operator sees the server's English instead of the catalogue's) but the
   gap is structural: it is a forward-only check over a hand-written list. A source scanner that
   collects `moira.error.*` / `moira.notice.*` emissions the way `i18n-scan.ts` collects
   `console.*` ones would close it. Unscheduled.
3. **PRIVACY (low) — the invite list is a directory of pending invitees.** `AdminInviteRecord.value`
   is the invited email address or domain and `consumed_subject` the redeemer's IdP subject; both are
   returned to any `moira:admins:read` holder. That is the right audience, and it is worth stating
   rather than discovering: `/admins` renders personal data, so its copy and any future export need
   to be thought of that way.
4. **PROCESS — decision D-W2-1 was recorded only in a migration comment and two catalogue comments.**
   It removes a third of wave 5's scope, and nothing in the plan or the ledger says so. Fixed here;
   the general lesson is that a decision which changes a *later wave's* scope must land in that
   wave's §0, not only where it was taken.
5. **F21 is fixed in PR #39** and **§0.7's W4-D6 is wrong** (#52). If wave 4 has already started, tell
   it to skip that task. If #39 is abandoned, F21 reopens together with F20 and F13.
6. **NO LIVE CREDENTIAL IS REQUESTED OR NEEDED.** Wave 5 touches no OAuth provider configuration; the
   invitee signs in through whatever wave 3 and wave 4 already resolve, against the TLS mock IdP.
   Nothing in this wave is deferred for want of a credential.
7. **`replicaCount: 1` remains pinned** and wave 5 does not lift it. The reason is the per-process
   auth-config snapshot in `auth-runtime.ts`, not storage — the chart's own note says the database
   half of the work is done. Unchanged by this wave.
8. **The `/invite/[token]` page is unauthenticated and takes a secret in a URL path.** Wave 2 already
   bounded the server side (prefix lookup before any Argon2 work, so it is not a CPU-exhaustion
   oracle; identical `invite_not_found` for a wrong prefix and a wrong hash, so it is not a guessing
   oracle). The console side must not undo that: the token is exchanged server-side on first load and
   must never reach client-visible state, a `Referer`, or an analytics call. There is no analytics in
   `console/` today and `smoke.e2e.ts` asserts the root route contacts no foreign origin — extend
   that assertion to `/invite/[token]`.

### §0.8.6 Which findings depend on PR #39 merging

**Depend on #39 — false or differently-true if it does not merge:**

* **#17 / W5-B8** — transfer is one `PATCH`. On `main` today `set_primary` does **not** demote the
  incumbent, so transfer really is two calls there. Implementing against pre-#39 `main` means writing
  the two-call form and then deleting it.
* **#18** — the sole admin is non-revocable. Introduced by #39's `revoke_grant`/last-primary
  interaction plus `admin_identities_single_active_primary`. On `main` it is not true.
* **#52 / F21** — the failure-replay double-count is fixed **only** on `fix/findings-sweep`. Without
  #39 it is still live and returns to whichever wave takes it.
* **W5-D10** and Phase 0 step 1 exist entirely because of this dependency.
* Indirectly, the whole **ownership half of wave 5**: F20 means no admin is primary on any deployment
  created after `0017`, so before #39 a transfer UI would be a control no JWT admin can ever use.
  `insert_grant` setting `is_primary` on the first grant is #39's change, not `main`'s.

**Do NOT depend on #39 — true either way:**

* Every claim about the invite backend's shape (#1–#3, #11–#13, #48–#51) — wave 2.
* Every console finding (#19–#36) and blockers **W5-B2** through **W5-B7**, **W5-B9**, **W5-B10**.
* **W5-B1** (recovery has no backend) — D-W2-1 is wave 2's, and #39 does not touch it.
* #14 (`is_primary` exists), #15/#16 (the spec is frozen at 151/99), #37/#38/#40 (session storage and
  the absent lifetime policy), #45–#47 (product decisions).

**One merge-mechanics note.** PR #39 edits this same file, inserting decision **D-F20** into §0.2.
§0.8 appends after §0.7.6, so the two changes do not overlap and should merge without conflict — but
verify rather than assume, and if a conflict appears, **keep both**: D-F20 is the decision and §0.8 is
the audit that depends on it.

---

**Objective.** Extend the Moira admin console (shipped in plan 08 as a Better Auth BFF with Google sign-in and a working `genericOAuth` baseline) with **operator-facing provider extensibility** and **multi-admin lifecycle**: hardened generic-OIDC support managed from the console rather than from environment variables, a GitHub sign-in option, an invitation flow so an existing admin can grant a new `(issuer, subject)` admin identity **without touching Moira's bootstrap system key** — the actual gap, since Moira already grants N admins *via that key* (§0.5) — refined session management, and an ownership-transfer / account-recovery story that goes beyond the system-key break-glass that plans 07/08 already provide.

**Why ordered here.** Explicitly **post-MVP** per `plans/01-roadmap-and-dependencies.md` §2 (row 09) and §4.6 ("Identity features that remain post-MVP: GitHub provider, invitations/additional-admin flows, ownership transfer, account recovery beyond system-key break-glass"). It depends on plan 08 existing — but **only its lib layer, atoms, two molecules and harnesses actually shipped** (§0.1 B2, §0.4). `lib/moira-client.ts`, `lib/auth.ts`, `lib/auth-config.ts`, `lib/console-secrets.ts` and `lib/setup-flow.ts` are genuinely **extended, not rebuilt**; **every screen, organism and console i18n artefact this plan names is greenfield** — and on plan 07's identity foundation (the `admin_identities (issuer, subject)` model this plan's invitation flow grants into). **Nothing in this plan is required to ship a working MVP console** — 08 alone is a complete, safe, single-admin console.

**Branch & PR (CONVENTIONS §1).** Branch `plan/09-generic-oidc-github-invitations`, cut from current `main` (or stacked on `plan/08-nextjs-console-google-oauth` if 08 has not merged, in which case the PR description names the base PR and the branch is rebased once 08 lands). Conventional Commits. One plan = one branch = one PR.

**What changed versus the previous draft of this plan.** The previous draft said "add Auth.js's built-in generic `OIDCProvider`" and "add Auth.js's built-in `GitHubProvider`." Both are superseded: plan 08 already configures Better Auth's **`genericOAuth` plugin** (`config: [{ providerId, clientId, clientSecret, discoveryUrl, issuer, requireIssuerValidation, pkce, scopes, mapProfileToUser }]`, verified against better-auth.com 2026-07-25), so generic OIDC is a *baseline capability* from 08 onward. This plan's OIDC contribution is therefore narrower and more honest:

| Previously claimed for 09 | Actual 09 scope after the Better Auth migration |
|---|---|
| "Add generic OIDC provider" | **Already in 08.** 09 adds: multiple simultaneous OIDC providers (`genericOAuth` accepts a `config` **array**), an operator-facing management screen writing into Moira's auth settings, a strict-mode policy surface for `requireIssuerValidation`/`pkce`/`scopes`, discovery-document health checks, and per-provider allowed-domain policy. |
| "Add GitHub via Auth.js `GitHubProvider`" | Better Auth **built-in `socialProviders.github`**, with the same server-side verified-email hardening and optional org-membership check as before. |
| "Auth.js session model" | Better Auth **DB-backed sessions** (plan 08 gave the console its own `console_auth` schema), which makes the "active sessions" screen and true remote sign-out **genuinely implementable** rather than best-effort. This is a real capability upgrade the Auth.js-era draft could not offer. |

**Honest limitation (CONVENTIONS §7.4), restated.** Better Auth does **not** provide enterprise SAML SSO and does not act as a SAML SP. This plan adds **no SAML support** and must not be read as doing so. Customers needing SAML use **mode 3** — they front SAML with their own IdP or SSO gateway that emits OIDC/JWT, and register that issuer's JWKS directly as a Moira `trusted_jwt_issuer`, bypassing the console entirely. That path is unchanged by this plan and needs no console at all.

**User-visible outcome.** An operator can (a) configure one or more standards-compliant OIDC providers, or GitHub, **from the console's auth-settings screen** — each provider's non-secret config persisted in Moira and its client secret stored **encrypted in the console's own database (D7)**, both written in the same step, applied at runtime with no redeploy — and **rotate any provider's client secret from that same screen**, since Moira has no `rotate-secret` endpoint, (b) as an already-admin user, invite a colleague by email or domain, who then signs in with any configured provider and is automatically granted admin scope bound to their invite, (c) transfer "ownership" (the ability to manage other admins) from one admin to another without a system-key operation, (d) see and revoke active console sessions for real, and (e) recover access via a documented, audited recovery path when at least one other admin remains.

**Included scope.**
- Multi-provider generic-OIDC management: N simultaneous `genericOAuth` entries, operator-managed via **Moira's auth settings for non-secret config plus the console's own encrypted store for each client secret (D7)**, with `requireIssuerValidation: true` and `pkce: true` enforced as non-overridable policy.
- **Per-provider D7 mechanics**: one `authProviderSecret` row per provider, each with its own `client_id` fingerprint; a per-provider same-step dual write with partial success treated as an operator-resolvable failure; a per-provider drift check surfacing a specific, actionable keyed error; and **console-side secret rotation per provider**, since Moira's `rotate-secret` endpoint does not exist.
- GitHub sign-in via Better Auth's built-in `socialProviders.github`, with GitHub's weaker email/org guarantees explicitly compensated for — its client secret console-owned like every other provider's.
- Invitation flow: an existing admin creates a scoped invite token (email- or domain-bound, time-limited, single-use); the invitee redeems it during sign-in with any configured provider; Moira grants `(issuer, subject)` → `moira:admin` via a new Moira admin-invite endpoint family. **Invitations inherit plan 07's frozen contract in full: `email`/`email_verified` are required on redemption (D5), and the deny-by-default domain allow-list applies with no invitation-based and no recovery-based exemption (D3) — an invite is a scoping token, never a policy bypass.**
- Real session management: list and revoke Better Auth sessions (DB-backed), configurable lifetime/idle policy.
- Ownership transfer via the `moira:admins:manage` **scope** (see the design decision below).
- Admin-to-admin account recovery, so system-key break-glass becomes a last resort rather than the only resort.
- Full CONVENTIONS compliance for everything added: Atomic Design paths, pinned toolchain, `bun test` unit coverage for every new atom/molecule/organism, Playwright e2e, axe on every new page route, secret-leak coverage, i18n keys for every new string (Rust **and** console).

**Excluded scope.**
- Enterprise SAML SSO — permanently out of scope (see above).
- Any Moira-side password/session storage (still rejected per plan 01 §4.2 option 5 — every provider remains BFF-mediated; Moira still only ever sees short-lived minted JWTs or the invite-grant call).
- Fine-grained per-resource admin roles ("providers-only admin") — flagged as a possible future iteration, not built here.
- Console-sent invite emails (no SMTP integration) — the inviting admin copies the link and shares it out of band.

**Design decision preserved from the previous draft (correct, keep it).** The "primary/ownership" designation is expressed as a **scope, `moira:admins:manage`, inside plan 07's existing `admin_identities.granted_scopes text[]` column** — *not* as a new boolean column. Rationale: 07's `src/security/auth.rs` extension unions `granted_scopes` onto the trusted-JWT actor's scopes on every request, so a scope stored there is **enforced automatically by the existing authz path**, whereas a boolean column would be invisible to `Actor.scopes` without further `auth.rs` changes. This keeps 09 additive to 07's schema (no column change, only a one-row backfill).

**Product-input decisions this plan flags (confirm at Wave 0):**
1. **Single primary vs. multiple `moira:admins:manage` holders** — changes the transfer action materially (revoke-from-self vs. leave-both-granted). **Blocking before Wave 3.**
2. **Default invite expiry** — recommend ≤72h, enforced server-side as a hard cap, not just a UI default.
3. **GitHub org-membership check: required-on or optional-on** for deployments that configure GitHub — recommend optional (not every self-host operator uses a GitHub org).
4. **Which new admin screens are MVP-of-this-plan** — proposed: `/admins` (list + invite + transfer + recovery), `/invite/[token]` (public, token-gated), `/settings/auth` provider management (extending 08's screen), `/settings/sessions`. Needs product sign-off.

---

## Findings Addressed

- **P1-11** (identity foundation): plan 07 built the `(issuer, subject)` grant primitive and single-admin claim; plan 08 consumed it for one admin. This plan is the **identity extensibility** layer named in `plans/01-roadmap-and-dependencies.md` §2 row 09 and §4.5's rows "Invitation / additional admins" (09) and "Account recovery & ownership transfer" (09).
- **P1-1** (unkeyed hash weakness): this plan's new secret type — the invite token — is designed correctly from the start, hashed with the existing `ApiKeyHasher` (Argon2id + pepper) rather than a bare SHA-256. Explicitly a non-regression, and a named reviewer check.
- **P0-3** (conversation/memory/RAG scoping): unaffected; plan 08's excluded-screen list remains excluded here.
- **P1-4 / P1-10**: unchanged dependencies inherited from 08 (audit-log cursor pagination, generated OpenAPI client types).
- **Current behavior this plan changes (narrowed — §0.5).** Moira already supports **N** admin identities: `AdminIdentityService::claim` has **no `setup_claimed` precondition**, and `409 admin_identity_already_claimed` comes from the `(issuer, subject)` unique index, not from a singleton gate. So an operator who still holds the bootstrap system key can grant a second, third and fourth admin today. **The real gap is a non-system-key path to a grant** — an existing admin cannot onboard a colleague without the break-glass credential, which means the credential can never be retired. Alongside it: no in-console admin-management surface at all, no session revocation surface, and no recovery short of break-glass. Those are the gaps this plan closes.

---

## Architecture

### Dependency on plan 08's D-1 (Moira DB-backed auth settings)

Plan 08 declares **D-1**: Moira must own a DB-backed auth-settings resource (**non-secret config only — D7**) in a migration-backed table, with admin CRUD endpoints and `LISTEN/NOTIFY` invalidation, owned by a plan-07 amendment. **This plan assumes D-1 is live and extends its data shape** to hold *multiple* providers rather than one:

- The Moira auth-settings table must support N rows keyed by `provider_id` (`google`, `github`, and one per generic-OIDC provider), each with its own `client_id`, `discovery_url`/`issuer`, `allowed_email_domains text[]`, `hosted_domain`, `required_org` (GitHub only), requested scopes, algorithms, audiences, redirect URIs, and `enabled`. **There is no `client_secret` column and no encrypted-envelope columns (D7)** — those were deleted from 07's spec, and this plan must not reintroduce them for the multi-provider case.
- **The console's `authProviderSecret` table (plan 08, D7) is already N-row by construction**: it is keyed by `moiraProviderId` and unique on it, so N providers need **no console schema change at all**. Plan 08's `putProviderSecret` / `getProviderSecret` / `deleteProviderSecret` / `listConfiguredProviderIds` are used as-is, once per provider. This plan adds **no new console table** and **no second encryption mechanism** — reusing `lib/provider-secrets.ts` verbatim is a hard requirement, not a preference, so that there is exactly one place where a client secret is encrypted, decrypted, or fingerprinted.
- If D-1 landed with a single-row shape, extending it to N rows is a **new forward migration owned by this plan** (append-only; never an edit to a merged migration), and this plan's Wave 1 owns it. That migration adds **non-secret columns only**.
- **Wave 0 must confirm** D-1's shipped shape before Wave 2 begins, **and re-verify D7 conformance for the multi-provider surface**: no `client_secret` field on any auth-provider request or response DTO, no `rotate-secret` endpoint, no secret material (plaintext, fingerprint, mask, or `has_secret` flag) in a list or detail response, and the operation count still **10**. This plan does **not** invent auth-settings endpoint paths; it binds to whatever 07/D-1 froze as D7 leaves it.

### Inherited from plan 07's frozen contract — invitations get NO exemption (D3/D5)

Plan 07's Interfaces & Contracts section carries a **frozen-contract change** (product-owner decisions D3/D4/D5, recorded 2026-07-25). Plan 08 propagates it for the first-admin claim; **this plan inherits it verbatim for invitations and every additional-admin path**. Paths, methods (`google_oauth` | `generic_oidc` | `jwks`), and scopes (`moira:auth-settings:{read,write,delete}`) are unchanged.

| # | Inherited rule | What it means for invitations |
|---|---|---|
| **D5** | `email` and `email_verified` are **required**, non-optional, on every identity-granting path. `AdminIdentityRecord.email` is `String`, not `Option<String>` — **a grant cannot exist without an email**. | `POST /api/v1/admin-invites/redeem` **always** carries `email: String` and `email_verified: bool` in its body, BFF-asserted from the just-verified session. There is **no optional-email path** and no branch that makes either field conditional — redemption creates an `admin_identities` grant, and that grant's `email` column is non-nullable. Shapes match `ClaimAdminIdentityRequest`'s post-D5 form exactly. |
| **D3** | Email/domain allow-list is **deny-by-default with no exemption and no bootstrap bypass**. An enabled `auth_provider_settings` row must govern the target issuer **and** list the email's domain in `allowed_email_domains`; unconfigured or empty ⇒ deny. | **An invitation does NOT bypass the allow-list.** Possession of a valid, unexpired, constraint-matching invite token is **necessary but not sufficient**: the invitee's verified email domain must *also* be in an enabled provider's `allowed_email_domains`, or redemption is refused `403 admin_claim_domain_not_allowed` exactly as a first claim would be. There is **no invitation-based exemption, no "the inviter vouched for them" carve-out, and no recovery-flow carve-out** — recovery invites are held to the identical policy. |

**Consequence — configuration order is load-bearing here too.** An admin can create an invite whose email/domain constraint is satisfiable but whose domain is not in any enabled provider's allow-list; the invitee will then authenticate successfully and still be denied at redemption. This plan's UI must prevent and explain that, not merely surface the 403 — see the invitation UI and i18n sections. This is **expected behaviour, not a bug**.

**Two independent constraints, never conflated.** The invite's own `email_constraint`/`domain_constraint` (this plan) and the provider's `allowed_email_domains` (07's policy) are checked separately and **both** must pass. A mismatch on the first yields `invite_email_mismatch`/`invite_domain_mismatch`; a mismatch on the second yields `admin_claim_domain_not_allowed`. The UI must not collapse the two into one message, because their remedies differ (reissue the invite vs. widen the provider allow-list).

### Components & ownership

| Component | Owner | Lives in |
|---|---|---|
| Moira admin-invite endpoint family (new) | this plan (Moira-side change) | `src/http/identity.rs`, `src/application/identity.rs`, `src/infra/repositories/identity.rs`, `src/domain/identity.rs`, `src/security/authz.rs`, `migrations/` |
| Moira auth-settings multi-provider extension (if needed) | this plan | `migrations/`, `src/domain/`, `src/application/`, `src/http/` (whichever module D-1 established) |
| New i18n catalog entries (errors + notices) | this plan | `src/i18n/catalog/errors.rs`, `src/i18n/catalog/notices.rs`, `docs/i18n-response-catalog.json` |
| Console: multi-provider auth settings + GitHub | this plan | `console/lib/auth.ts` (extended), `console/lib/auth-settings.ts` (extended), `console/modules/authSettings/**`, `console/app/(console)/settings/auth/**` |
| Console: invitation UI + redemption | this plan | `console/modules/admins/**`, `console/modules/invite/**`, `console/app/(console)/admins/**`, `console/app/invite/[token]/**` |
| Console: session management | this plan | `console/modules/sessions/**`, `console/app/(console)/settings/sessions/**` |
| Console: ownership transfer / recovery | this plan | `console/modules/admins/**` (shared with invitations) |

This is the **first plan in the identity/console line that touches Moira source** since plan 07 — plan 08 was console-only. The Moira-side change is additive and narrowly scoped: one new admin-invite endpoint family reusing the `admin_identities` table and grant machinery plan 07 built, not a new identity model.

### Data flow — invitation

```
Admin A (already authenticated in console)
   │ 1. "Invite admin" form: email or domain, expiry, optional scope note
   ▼
Console server action (app/(console)/admins/actions.ts)
   │    → Moira POST /api/v1/admin/admin-invites
   │      Authorization: Bearer <Admin A's jwt-plugin token>  (no scope claim;
   │      authorization resolves from admin_identities.granted_scopes)
   │      requires scope moira:admins:invite
   ▼
Moira: creates an admin_invites row (Argon2id+pepper token hash,
       email/domain constraint, expires_at, created_by = Admin A's
       (issuer, subject), status = pending)
   │ 2. Returns { invite_id, token } — token shown ONCE, mirroring Moira's
   │    existing ApiKeySecretResponse pattern
   ▼
Console renders the link in OnceOnlySecretModal (molecule from plan 08)
   │ 3. Admin A shares it out of band; the console sends no email
   ▼
Invitee opens /invite/<token>
   │ 4. Page (server component) POSTs Moira /api/v1/admin-invites/preview
   │    (token in BODY, never a query string) → non-sensitive descriptive
   │    fields only (masked inviter email, expiry, constraint pattern)
   │ 5. Invitee signs in with any configured provider (Better Auth)
   ▼
Console server action → Moira POST /api/v1/admin-invites/redeem
   │   token in body  +  Authorization: Bearer <invitee's freshly minted
   │   jwt-plugin token — iss = console, sub = their IdP subject, NO scope
   │   claim; identity proof only>
   │   Body also carries { email, email_verified } asserted by the BFF from
   │   the just-verified Better Auth session — BOTH REQUIRED, non-optional,
   │   matching 07's post-D5 ClaimAdminIdentityRequest shape. Sent on every
   │   redemption; there is no optional-email path.
   ▼
Moira: validates token not expired/consumed, validates the invite's own
       email/domain CONSTRAINT against the BFF-asserted verified email,
       AND SEPARATELY enforces 07's deny-by-default provider allow-list
       (an enabled auth_provider_settings row must govern the issuer and
       list the email's domain) — an invite grants NO exemption from it —
       then grants (issuer = console, subject = invitee_sub) -> moira:admin,
       marks the invite consumed — all in one transaction under an advisory
       lock on the token hash (single winner)
   │
   │   If the domain is not in an enabled provider's allowed_email_domains:
   │   403 admin_claim_domain_not_allowed. The invitee is authenticated but
   │   NOT granted, the invite is NOT consumed (remains redeemable once the
   │   operator widens the allow-list), and the console renders the
   │   actionable "ask an admin to add your email domain to the allow-list"
   │   instruction rather than a generic failure.
   ▼
Console: the invitee is now a fully authenticated console admin
```

### Data flow — ownership transfer / recovery

- **"Primary" designation** — the `moira:admins:manage` scope inside `admin_identities.granted_scopes` (see Summary for why this beats a boolean column). Backfilled onto the row referenced by `setup_state.claimed_admin_identity_id` (07's singleton records exactly which identity claimed setup), preserving "the claimant is primary by default" without a new claim. If that row is revoked/absent, backfill the sole `status = 'active'` row instead; zero active rows = nothing to backfill (system-key break-glass still works). Transferable by an existing holder via `PATCH /api/v1/admin/admin-identities/{id}` with `If-Match` against `admin_identities.version` (07's migration ships the column and bump trigger) plus the standard `Idempotency-Key`.
- **Recovery** — a `moira:admins:manage` holder issues an invite scoped to replace a specific locked-out `(issuer, subject)` (`is_recovery: true`, `replaces_admin_identity_id`). On redemption Moira revokes the old grant and creates the new one **in the same transaction** (atomic swap — never a window where both or neither exist), audited as a distinct `admin_identity_recovered` event so the audit log separates routine onboarding from recovery.
- **If no admin holds `moira:admins:manage`** — the only path left is system-key break-glass (unchanged from 07/08). This plan documents that as the deliberate last resort and does **not** try to eliminate it; a system-key-less recovery would reintroduce exactly the "first login wins" unsafety `plans/01` §4.4 rejects.

### Security boundaries — browser vs BFF vs Moira

Unchanged from plan 08's model, extended to more providers and one more privileged action:
- **Browser** — never sees invite-token validation logic, another admin's session tokens, or any OAuth client secret. Per CONVENTIONS §6 rule 5, **no secret is passed as a prop into any organism, molecule, or atom**; the invite token appears exactly once, in `OnceOnlySecretModal`, and never as a prop on a reusable component beyond that modal's own render.
- **BFF** — gains no new secret *class* and no new secret *mechanism*: GitHub's and each generic-OIDC provider's **client secret is console-owned and stored encrypted at rest in `console_auth.authProviderSecret` (D7)**, one row per provider, written by the auth-settings screen, decrypted server-side at auth-instance construction, held only in process memory. **No client secret is ever sent to Moira, and Moira never returns one.** This plan adds providers, not a new storage or encryption path — `lib/provider-secrets.ts` from plan 08 is reused verbatim under the same `CONSOLE_SECRET_ENCRYPTION_KEY`. The invite-token value is never logged and never retained in client-visible state after the `/invite/[token]` page's first server-side exchange.
- **Moira** — the new admin-invite endpoints follow the same authentication/authorization model as every other admin endpoint. `preview`/`redeem` are **token-authenticated, not scope-authenticated**; the token is hashed at rest with the existing `ApiKeyHasher` (Argon2id + pepper, `src/security/api_keys.rs`) under a new namespace. **No new auth primitive is invented.**

### DB/migration changes

New Moira migration **`migrations/0017_admin_invites.sql`** (append-only; §0.5 — `0012`/`0013` are plan 07's, `0014`/`0015` shipped since, and plan 11 reserves `0016`; re-verify by listing `migrations/` at implementation time):

- **`admin_invites`** — `id uuid pk`, `token_hash text`, `token_prefix varchar(64)`, `fingerprint varchar(128)`, `pepper_version varchar(64)` (the exact secret-storage column set 07's `admin_setup_tokens` uses, which itself mirrors `system_api_keys`), `email_constraint text null`, `domain_constraint text null` (exactly one set, CHECK-enforced), `is_recovery boolean not null default false`, `replaces_admin_identity_id uuid null references admin_identities(id)`, `created_by_issuer text`, `created_by_subject text`, `status varchar(32)` (`pending`/`consumed`/`revoked`/`expired`, CHECK-constrained), `expires_at timestamptz`, `consumed_at timestamptz null`, `consumed_issuer text null`, `consumed_subject varchar(256) null`, `created_at`, `updated_at`, `deleted_at null` — following `0009`'s conventions exactly.
- **`admin_identities`** gains **no new column**. The only touch is the `moira:admins:manage` backfill described in Data flow.
- **Auth-settings multi-provider extension**, if D-1 shipped a single-provider shape (see the D-1 dependency above).

All in **append-only** migration files (07's convention: never edit a merged migration; corrections are new migrations), validated by the existing migration-contract CI job.

**Console-side.** Plan 08 already gave the console a `console_auth` schema (Better Auth `user`/`session`/`account`/`verification`, the `jwt` plugin's `jwks` table, and — per **D7** — `authProviderSecret`, keyed uniquely on `moiraProviderId` and therefore **already N-row**). This plan adds **no console schema of its own** — N providers mean N rows in the existing table, managed by Better Auth's own CLI in `console/db/`, never by Moira's `migrations/` — but does newly *use* the `session` table: because Better Auth sessions are DB-backed, the "active sessions" screen is a **real** session registry with **real** revocation — a genuine capability upgrade over the previous Auth.js-era draft, which could only offer an audit-log-derived approximation. This plan states that plainly and removes the old "best-effort" caveat.

### API & OpenAPI changes

New Moira endpoints (this plan's only new Moira surface):
- `POST /api/v1/admin/admin-invites` — create. Scope `moira:admins:invite`. Body `{ email_or_domain_type: "email"|"domain", value: string, expires_in_seconds: u32, is_recovery?: bool, replaces_admin_identity_id?: Uuid }`. Response `AdminInviteSecretResponse` (once-only token, same envelope pattern as `ApiKeySecretResponse`). `Idempotency-Key` required.
- `GET /api/v1/admin/admin-invites`, `GET .../{id}` — list/inspect; no token value returned after creation. Scope `moira:admins:read`.
- `POST /api/v1/admin/admin-invites/{id}/revoke` — scope `moira:admins:invite`. `Idempotency-Key` required.
- `POST /api/v1/admin-invites/preview` — **token-authenticated, not scope-authenticated**; the path sits **outside** the `/api/v1/admin/*` scope-gated prefix because it is credentialed by a one-time invite token rather than an admin scope. POST (not GET) with the raw token in the **body only**, so it cannot leak into access logs or referer chains. Returns only non-sensitive descriptive fields (inviter's display email with the local part masked, e.g. `j***@example.com`; expiry; constraint pattern).
- `POST /api/v1/admin-invites/redeem` — token in body **+** `Authorization: Bearer <invitee's freshly minted, scope-claim-free JWT>`, binding redemption to a concrete `(issuer, subject)` in one atomic call. `Idempotency-Key` required; single winner via an advisory lock on the token hash.
  - **Body shape, bound to 07's post-D5 contract:** `AdminInviteRedeemRequest { token: String (req), email: String (req), email_verified: bool (req) }`, `deny_unknown_fields`. **`email` and `email_verified` are non-optional** — no `Option<String>`, no `#[serde(default)]` — mirroring `ClaimAdminIdentityRequest` exactly, because redemption creates the same `admin_identities` grant whose `email` column is non-nullable (`AdminIdentityRecord.email: String`). A body omitting either is rejected with the standard `ErrorResponse` envelope carrying `moira.error.invalid_request` (400 malformed / 422 schema-violating), before the service is reached.
  - **Policy enforced on redeem, no invitation exemption (D3):** `email_verified` must be `true`; the email must be non-empty with an extractable domain; the invite's own `email_constraint`/`domain_constraint` must match; **and** an **enabled** `auth_provider_settings` row must govern the issuer and list the email's domain in `allowed_email_domains` — else `403 admin_claim_domain_not_allowed`. **Deny-by-default: unconfigured or empty ⇒ deny.** Possession of a valid token authorises the caller to *submit* a redemption; it does not exempt them from *policy*. This is enforced identically for routine and **recovery** invites. On this rejection the invite is **not** consumed, so it stays redeemable after an operator widens the allow-list.
  - The two constraint families are checked and reported **separately** (`invite_*_mismatch` vs. `admin_claim_domain_not_allowed`) because their remedies differ; they are never collapsed into one code or one message.
- `PATCH /api/v1/admin/admin-identities/{id}` — grant/revoke `moira:admins:manage` in the target's `granted_scopes` (ownership transfer). Caller scope `moira:admins:manage`. `If-Match` against `admin_identities.version` **and** `Idempotency-Key` required.
- `GET /api/v1/admin/admin-identities` — list current grants (the `AdminIdentityRecord` shape 07 already defines). Scope `moira:admins:read`.
- `DELETE /api/v1/admin/admin-identities/{id}` — **soft** revoke (`status = 'revoked'`, `revoked_at`), never a hard row delete. This is 07's explicitly-deferred revoke endpoint landing here. Scope `moira:admins:manage`. `Idempotency-Key` required. Per 07's design, revocation does **not** reset `setup_state.claimed` — setup-required is a one-way transition; revoking the last admin leaves system-key break-glass as the re-entry path.

All new endpoints get `#[utoipa::path]` annotations and OpenAPI coverage following `.claude/skills/moira-openapi/SKILL.md`; `src/http/mod.rs::tests::generated_openapi_covers_every_registered_route` must be updated — required, not optional.

### i18n (CONVENTIONS §4) — Moira side

Every new error code gets a `moira.error.<code>` entry in `src/i18n/catalog/errors.rs` with an English `default_message` and a `description`; every new success/notice string gets a `moira.notice.*` entry in `src/i18n/catalog/notices.rs`. `message_args` carries interpolation values as **structured data**, never pre-formatted English prose. `docs/i18n-response-catalog.json` is updated in the same PR. New codes:

`invite_expired`, `invite_already_consumed`, `invite_revoked`, `invite_email_mismatch`, `invite_domain_mismatch`, `invite_not_found`, `admin_identity_last_primary`, `admin_identity_not_found`, `admin_identity_already_revoked`, `admin_invite_expiry_too_long`.

Notices: `moira.notice.admin_invite_created`, `moira.notice.admin_invite_redeemed`, `moira.notice.admin_identity_revoked`, `moira.notice.admin_identity_recovered`.

### i18n — console side

Every new console-originated string gets a `console.*` key with an English default in `console/lib/i18n/catalog.en.ts` (from plan 08). Every new Moira key the console renders is mirrored into `console/lib/i18n/moira-keys.ts`. The console renders `t(messageKey, messageArgs, message)` — catalog first, **server-supplied `message` as fallback** — and **never hardcodes English copy for a server-originated condition** (CONVENTIONS §4.6). Plan 08's `i18n-catalog-coverage.test.ts` and `no-hardcoded-copy.test.tsx` extend to cover this plan's new files automatically; both must stay green.

**`moira.error.admin_claim_domain_not_allowed` on the invite path is an ACTIONABLE instruction, not a generic failure (D3).** Plan 08 established this treatment for the setup wizard; this plan extends it to the invitation and recovery surfaces, with audience-appropriate copy. New console keys:

| Key | Surface | English default (intent) |
|---|---|---|
| `console.admins.invite_domain_not_in_allow_list` | `InviteAdminForm` (pre-submit gate) | "This domain isn't in any enabled provider's allowed email domains, so the invite would be refused at redemption. Add it in Settings → Auth first." |
| `console.admins.no_enabled_provider` | `InviteAdminForm` (disabled state) | "No auth provider is enabled yet. Configure and enable one before inviting admins." |
| `console.invite.domain_not_allowed.title` | `InviteAcceptPanel` (invitee-facing) | "Your email domain isn't allowed yet" |
| `console.invite.domain_not_allowed.body` | `InviteAcceptPanel` | Explains that the invite is still valid and unconsumed, that an admin must add the domain to the allow-list in Settings → Auth, and that the link will work afterwards — **not** that the invite failed. |
| `console.invite.domain_not_allowed.action` | `InviteAcceptPanel` | "Retry" (re-attempts redemption once the operator has widened the list) |

Rules for this rendering, mirroring plan 08's:
1. It is **never** routed to the generic `ErrorBanner` failure path and never shown as "something went wrong", a stack trace, or a raw envelope.
2. It is **never conflated** with `invite_email_mismatch` / `invite_domain_mismatch`, which mean the *invite's own* constraint failed and whose remedy is a reissued invite — a different message with a different action.
3. The offending domain travels via `message_args` as **structured data**, never pre-formatted English prose.
4. The server-supplied `message` remains the §4.6 fallback, so a missing console key never surfaces a bare key.
5. `moira.error.admin_claim_domain_not_allowed` is listed in `moira-keys.ts` and must exist in `docs/i18n-response-catalog.json` (it is 07's key — this plan renders it, it does not redefine it).

### Backward compatibility

- Plan 08's single-admin console continues to work unmodified if this plan's new providers/invite UI are simply not configured: with only Google enabled in Moira's auth settings, `getAuth()` builds exactly the plan-08 instance.
- The `moira:admins:manage` backfill is backward-compatible (a scope append on at most one existing row) — no existing 07/08 admin loses access, and `admin_identities`' schema is unchanged.
- Existing Moira admin API consumers are unaffected; every new endpoint is additive.
- **Mode 3** (bring-your-own JWT/JWKS, no console, air-gap-friendly) is untouched.

### Deployment implications

- No new container or chart — this plan extends `console/` and `charts/moira-console/` from plan 08. **New provider client secrets are not new chart values and not new deployment secrets**: they are entered in the console's auth-settings screen and stored **encrypted in the console's own database (D7)**, under the **same** `CONSOLE_SECRET_ENCRYPTION_KEY` plan 08 already provisions — so adding GitHub or an Nth OIDC provider requires **no new `Secret` key, no chart value, and no redeploy**. The only chart change is any new non-secret toggle.
- Moira's chart gains one new migration, handled by the existing `migration-job.yaml` Helm hook.
- Toolchain pins are inherited unchanged from plan 08 (§5): Next.js **16.2.11**, Node **24.x Active LTS**, Bun **1.3.14**, Playwright for e2e, `bun install --frozen-lockfile`, committed `bun.lock`, exact pins in `package.json`, `.nvmrc`/`engines` for Node. **This plan does not bump any of them** — a bump is a separate, separately-verified decision.

### Failure & recovery

- **Invite token leaked/guessed** — Argon2id+pepper hashed at rest (unusable from a DB-only compromise; explicitly *not* the plain-SHA-256 pattern P1-1 flags elsewhere), time-limited (`expires_at`, hard server-side cap), single-use (`status` → `consumed` atomically under the same advisory-lock single-winner pattern as Moira's other atomic admin commands).
- **Last `moira:admins:manage` holder revokes their own privilege** — a Moira-side guard rejects any `PATCH`/revoke that would leave zero **active** admins carrying `moira:admins:manage`, returning `admin_identity_last_primary` (403/409). If no precedent guard exists in 07's system-key deletion path, this plan **establishes** the pattern and flags adopting it there as a follow-up (not implemented here).
- **GitHub/OIDC IdP outage** — does not affect other providers or already-authenticated sessions; only new sign-ins via the affected provider fail, rendering a keyed error state.
- **Discovery document unreachable** — the auth-settings screen shows a keyed per-provider health state; a provider whose discovery URL fails validation cannot be saved as `enabled`.
- **Per-provider two-store drift (D7)** — with N providers there are N independent ways for Moira's `client_id` and the console's stored secret to diverge. Handled **per provider**, exactly as plan 08 handles the single case: `loadAuthSettings()` compares `fingerprint(moiraRow.client_id)` against that provider's `clientIdFingerprint` on every load; on mismatch **that provider alone** is excluded with `console.error.auth_provider_client_id_mismatch`, its sign-in button is not rendered, and the OAuth exchange is never attempted. **Every other configured provider keeps working** — a drifted GitHub entry must not take generic-OIDC sign-in down with it. Missing (`auth_provider_secret_missing`) and undecryptable (`auth_provider_secret_undecryptable`) remain distinct conditions, reported per provider.
- **Per-provider partial dual write (D7)** — a provider whose Moira config saved but whose console secret write failed is left **disabled** and shown as an incomplete row in `ProviderList` with `console.error.auth_provider_secret_write_failed` plus Retry/Discard, never as an enabled provider and never as a success. With N providers this is a **per-row** state, so one incomplete provider never blocks configuring or using the others.
- **All providers unresolvable at once** — if every configured provider fails its D7 check, `/login` falls back to the existing keyed `console.error.auth_settings_unavailable` state with no sign-in buttons, exactly as in plan 08. It never constructs a provider with an empty or guessed secret.
- **Invitee's domain not in any enabled provider's allow-list** — redemption is refused `403 admin_claim_domain_not_allowed` (D3: an invite grants **no** exemption). **Expected behaviour, not a bug.** The invite is **not** consumed, so it remains redeemable; the invitee sees the actionable `console.invite.domain_not_allowed.*` instruction; the inviting admin is prevented from creating such an invite in the first place by `InviteAdminForm`'s pre-submit gate. Recovery invites behave identically — there is no recovery carve-out.
- **All providers disabled while invites are outstanding** — every outstanding invite becomes temporarily unredeemable (deny-by-default with nothing enabled denies everyone). Invites are **not** auto-revoked; re-enabling a provider whose allow-list covers the invitee restores redeemability within the invite's original expiry. The `/admins` invite list surfaces this as a keyed per-invite "blocked by policy" state rather than silently showing them as `pending`.

---

## Detailed Implementation

### Console: multi-provider generic OIDC (`console/lib/auth.ts` + `console/lib/auth-settings.ts`, extended)

- `loadAuthSettings()` (plan 08) is extended to return **an array** of provider configs rather than at most one OIDC entry. `getAuth()` maps them into a single `genericOAuth({ config: [...] })` call — the plugin accepts a **`config` array**, so N providers need no extra plumbing.
- **It composes the same two stores, per provider (D7).** For each Moira `auth_provider_settings` row it pairs the **non-secret config from Moira** with the **client secret from the console's own `authProviderSecret` row**, resolved by `getProviderSecret({ moiraProviderId: row.id, providerId, moiraClientId: row.client_id })`, which performs that provider's `client_id` fingerprint check before decrypting. **Moira returns no secret material for any provider** — no `client_secret`, no `secret_fingerprint`, no `masked_secret`, no `has_secret` flag — and this plan must not read, type, expect, or invent an endpoint that returns one. **The rejected Moira read-back option stays rejected, for every provider.**
- **Resolution is per provider and fail-closed per provider.** A provider whose secret is missing, fingerprint-mismatched, or undecryptable is **omitted from the `config` array** with its keyed condition attached for the UI; the remaining providers are constructed normally. `unresolvedProviders` (ids and condition keys only — never secret material) is what the settings screen and `/login` render.
- **The cache key spans both stores**, as in plan 08: `` `${moiraSettingsVersion}:${maxConsoleSecretUpdatedAt}` ``, so rotating any single provider's secret — an operation that touches Moira not at all — still rebuilds the Better Auth instance with no redeploy.
- **`lib/provider-secrets.ts` is reused verbatim.** This plan adds no second encryption path, no per-provider key, and no alternative fingerprint scheme. One module encrypts, decrypts, and fingerprints every client secret the console holds; a unit test asserts no other module performs a `createCipheriv`/`createHmac` on secret material.
- Per-provider policy is **enforced, not configurable**: `requireIssuerValidation: true` (the plugin's own default is `false` — this plan pins it true for every entry), `pkce: true`, `scopes: ["openid", "email", "profile"]` as the floor, and `mapProfileToUser` populating the `idpIssuer`/`idpSubject` additional fields plan 08 introduced (so the Moira-facing `sub` remains the **IdP's** stable subject, never the Better Auth `user.id`).
- Callback URLs follow the plugin's documented pattern `${baseURL}/api/auth/oauth2/callback/:providerId` and are registered **exactly** (no wildcards) at each IdP; `middleware.ts`'s host allow-list is unchanged and already covers them.
- The `databaseHooks.user.create.before` gate from plan 08 is extended to resolve the **per-provider** allowed-domain list, still **deny-by-default** (empty list denies everyone), throwing `APIError("FORBIDDEN", …)` with a message key on rejection. The check is provider-agnostic by construction; the new work is verifying it fires for every provider, not writing new logic per provider.

### Console: GitHub sign-in (`console/lib/auth.ts`, extended)

- Better Auth's built-in `socialProviders.github` (`{ clientId, clientSecret, scope: ["user:email", "read:org"?] }`), where **`clientId` and the non-secret config come from Moira and `clientSecret` comes from the console's own encrypted store (D7)**, already fingerprint-verified. If GitHub's secret is missing, mismatched, or undecryptable the provider is **omitted entirely** rather than constructed with an empty value, and only GitHub sign-in is affected.
- **GitHub-specific hardening** (per `plans/01` §4.2's note that GitHub has "weaker org/email-domain policy"): request `user:email` explicitly and, server-side in the `databaseHooks.user.create.before` hook, call GitHub's `/user/emails` to find the **verified primary email** — GitHub's profile email can be null, unverified, or a `noreply` address. Reject sign-in if no verified email is obtainable, closing the gap where `profile.email` alone cannot satisfy the uniform verified-email requirement of `plans/01` §4.3. Optional per-provider `required_org` setting: additionally check `GET /orgs/{org}/members/{username}` server-side and reject non-members — a GitHub-specific substitute for the `hd` hosted-domain check Google natively provides (plan 08 uses Google's `hd` option for that).
- All GitHub API calls are server-side only, from `console/lib/github.ts` (`import "server-only"`), never from a component.

### Console: auth-settings management screen (extends plan 08's)

- Page: `console/app/(console)/settings/auth/page.tsx`, actions `console/app/(console)/settings/auth/actions.ts`.
- Organisms: `console/modules/authSettings/{AuthSettingsForm,ProviderList,ProviderEditor,DiscoveryHealthPanel}.tsx`, plus plan 08's `ProviderSecretRotatePanel` and `ProviderDriftBanner` **reused per provider row, not re-implemented**.

#### D7 per-provider mechanics on this screen (mandatory)

**(a) Per-provider same-step dual write; partial success is an operator-resolvable failure.** Saving *any* provider — new or edited — performs both writes in one step, in plan 08's order, with `enable` as the commit point:

| # | Write | Note |
|---|---|---|
| 1 | Moira `POST`/`PATCH /api/v1/admin/auth/providers[/{id}]` (`X-Moira-System-Key`, `Idempotency-Key`, `If-Match` on edit) — **non-secret config only** | supplies the `id` the console's secret row is keyed by; rows are created **disabled** |
| 2 | `putProviderSecret({ moiraProviderId, providerId, clientId, clientSecret })` — console DB, one transaction | idempotent upsert, so retry is always safe |
| 3 | Moira `POST .../{id}/enable` (`If-Match`) | **the commit point** — a provider is never enabled without its secret present |

Partial states are handled **per provider row**, so one incomplete provider never blocks the others: step-2 failure → `console.error.auth_provider_secret_write_failed` on that row with **Retry** (re-runs step 2 against the same `moiraProviderId`) and **Discard** (`DELETE` the Moira row with `If-Match`, then `deleteProviderSecret`); step-3 failure → `console.error.auth_provider_enable_failed`, retried with a fresh `If-Match` and **no secret re-entry**. Neither state is ever rendered as a success, and neither leaves an **enabled** provider without a secret. `ProviderList` renders each row's completeness state (`configured & enabled` / `configured & disabled` / `config saved, secret missing` / `drifted` / `orphaned`) so the two stores are always reconcilable by eye.

**(b) Per-provider `client_id` fingerprint drift check.** Each `authProviderSecret` row carries its own `clientIdFingerprint`, compared against that provider's Moira `client_id` on every load. A mismatch produces `console.error.auth_provider_client_id_mismatch` **naming the affected provider**, excludes only that provider, hides only its sign-in button, and **prevents its OAuth exchange from being attempted at all** — so the operator never debugs an opaque `invalid_client` from GitHub or a self-hosted IdP. `ProviderDriftBanner` renders `client_id_mismatch`, `secret_missing`, and `secret_undecryptable` as **three distinct actionable states**, per provider, never collapsed and never routed to the generic `ErrorBanner`. Orphaned console secrets (no matching Moira provider — likelier here, since providers are added and removed routinely) are listed with `console.notice.orphaned_provider_secret` and a delete control.

**(c) Rotation is a console concern, per provider (D7 — Moira has no `rotate-secret` endpoint).** Each `ProviderList` row exposes **Rotate client secret** (`console.authSettings.rotate_secret.action`) opening `ProviderSecretRotatePanel` for that provider. The current secret is never displayed, pre-filled, or fetched — the row shows only `console.authSettings.secret_configured`, derived from the *existence* of the console row. `rotateProviderSecret(moiraProviderId, newSecret)` calls `putProviderSecret` with the **unchanged** `client_id`, re-encrypting under a fresh nonce; **it issues zero Moira requests**, needs no `If-Match`, cannot conflict with a concurrent config edit, and cannot fail on Moira availability. `invalidateAuthSettings()` then rebuilds the instance — **no redeploy** — and `console.notice.auth_provider_secret_rotated` confirms it. Changing a provider's `client_id` *and* secret together is the dual write of (a) again, with the same partial-failure treatment, because the fingerprint must be rewritten in lockstep. **Nothing in this plan may call, type, document, or generate a client for `POST /api/v1/admin/auth/providers/{id}/rotate-secret` — it does not exist.**

Client secrets remain **write-only in the UI**: never returned by any read (Moira has none to return, and the console never sends its own back to the browser), never re-rendered into a form, never a prop on any organism/molecule/atom. A successful save or rotation calls `invalidateAuthSettings()` so the next `getAuth()` rebuilds **with no redeploy**.

**No new i18n keys are needed for D7 in this plan** — plan 08's `console.error.auth_provider_{client_id_mismatch,secret_missing,secret_undecryptable,secret_write_failed,enable_failed}`, `console.notice.{auth_provider_secret_rotated,orphaned_provider_secret}`, and `console.authSettings.{rotate_secret.action,rotate_secret.body,secret_configured}` all carry a `{provider}` interpolation slot and are reused verbatim, with the provider name passed through `message_args` as **structured data, never pre-formatted English prose** (CONVENTIONS §4.3). `i18n-catalog-coverage.test.ts` and `no-hardcoded-copy.test.tsx` must stay green over the new files.

### Console: invitation UI

- Page `console/app/(console)/admins/page.tsx` (thin: guard + fetch + render), actions `console/app/(console)/admins/actions.ts`.
- Organisms `console/modules/admins/{AdminTable,InviteAdminForm,TransferPrimaryPanel,RecoveryPanel}.tsx`. `AdminTable` renders a "primary" badge for rows whose `granted_scopes` include `moira:admins:manage`.
- `createInvite(formData)` calls `POST /api/v1/admin/admin-invites`. The acting session's capability is checked client-side **for UI gating only**; Moira's own scope enforcement is the authority — never trust the client-side check alone.
- The invite link is displayed in plan 08's existing `console/components/molecules/OnceOnlySecretModal.tsx` — **reused, not re-implemented**, since 08 is merged by the time 09 runs.
- Public page `console/app/invite/[token]/page.tsx` (no session required) + organism `console/modules/invite/InviteAcceptPanel.tsx`: the server component calls Moira's `preview` endpoint with the URL token and renders "You've been invited…" plus a provider-agnostic set of sign-in buttons (one per enabled provider, driven by the same `SignInPanel` organism plan 08 shipped). An accept-invite intent is carried in a **signed, short-lived httpOnly cookie** set when the page loads (mirroring 08's claim-intent pattern), and the post-sign-in server action calls `redeem`. The raw token is exchanged server-side on first load and never retained in client-visible state.
- **Explicitly not built:** the console sends no invite emails. Flagged as a candidate enhancement, not silently assumed.

#### Order is load-bearing for invitations too (D3) — allow-list before invite

Exactly as plan 08's wizard must configure an enabled auth provider **before** the first claim, this plan's invite flow depends on the same deny-by-default policy and must order itself accordingly. **An invite is not a bypass**: an invitee whose verified email domain is absent from every enabled provider's `allowed_email_domains` will authenticate successfully and still be refused `403 admin_claim_domain_not_allowed` at redemption.

| # | Step | Screen | Gate |
|---|---|---|---|
| 1 | Configure/enable the provider(s) that will govern the invitee's issuer, **with the invitee's domain in `allowed_email_domains`** | `/settings/auth` (`ProviderList`/`ProviderEditor`) | at least one **enabled** provider whose allow-list covers the intended domain |
| 2 | Create the invite (email- or domain-constrained, expiry) | `/admins` (`InviteAdminForm`) | **step 1 satisfied for the value being invited** |
| 3 | Invitee opens the link, previews, signs in | `/invite/[token]` (`InviteAcceptPanel`) | valid, unexpired, unconsumed token |
| 4 | Redeem → grant | server action → `POST /api/v1/admin-invites/redeem` | invite constraint **and** provider allow-list both pass |

**`InviteAdminForm` enforces step 1 at creation time, not only at redemption time.** Before submitting, it resolves the current enabled providers' `allowed_email_domains` (server-side, via the existing auth-settings read) and:
- If the invited email's domain — or, for a domain-constrained invite, the domain itself — is **not** covered by any enabled provider, the form **blocks submission** and renders a keyed, actionable warning (`console.admins.invite_domain_not_in_allow_list`) offering a direct link to `/settings/auth` to widen the allow-list. This prevents minting an invite that is guaranteed to fail at redemption, which would otherwise strand the invitee after a successful sign-in.
- If **no** provider is enabled at all (the deny-everything default), the invite form is disabled entirely with `console.admins.no_enabled_provider`.
- The client-side check is **UI gating only** — Moira's redeem-time enforcement remains the authority, and the console's check is never more permissive than it.

**`InviteAcceptPanel` renders `admin_claim_domain_not_allowed` as an actionable instruction, not a generic failure** — see the i18n section below. The same treatment applies to `RecoveryPanel`: recovery invites are held to the identical policy and get the identical rendering.

### Console: session management (now a real registry, not best-effort)

- Page `console/app/(console)/settings/sessions/page.tsx`; organism `console/modules/sessions/SessionTable.tsx`.
- Because plan 08's Better Auth instance is **DB-backed**, this screen lists the actual `session` rows for the current user (created-at, last-updated, IP/user-agent as recorded by Better Auth) and offers **real revocation**: revoke one session, or revoke all others. This replaces the previous draft's audit-log-derived approximation, and the previous "session management is best-effort" caveat is **removed from the docs** because it is no longer true.
- Separately, a `moira:admins:manage` holder can "revoke this admin's grant entirely" via `DELETE /api/v1/admin/admin-identities/{id}`. The UI copy is explicit that this revokes the **Moira identity grant** (authorization), which is a different and stronger action than ending a console session (authentication) — both are now available and the distinction is stated, not blurred.
- Session lifetime/idle policy (`session.expiresIn`, `session.updateAge`) becomes an operator-editable auth setting persisted in Moira, applied at runtime.

### Console: ownership transfer / recovery UI

- On `AdminTable`, a `moira:admins:manage` holder sees "Make primary" per other row → `PATCH /api/v1/admin/admin-identities/{id}` granting the scope to the target and, **if the single-primary product decision is taken**, revoking it from the actor in the same server action (two sequential calls, each with its own `Idempotency-Key` and `If-Match`); under "multiple primaries allowed," both stay granted.
- "Start recovery for a locked-out admin": a holder selects a non-self admin row, creates a **recovery-flagged** invite (`is_recovery: true`, `replaces_admin_identity_id`) bound to that admin's known recovery email; on redemption Moira performs the atomic swap and logs `admin_identity_recovered` distinctly.

### Atomic Design placement (CONVENTIONS §6) — every new file

| Layer | New files |
|---|---|
| **Pages** | `console/app/(console)/admins/{page.tsx,actions.ts}` · `console/app/(console)/settings/sessions/{page.tsx,actions.ts}` · `console/app/(console)/settings/auth/{page.tsx,actions.ts}` (extended) · `console/app/invite/[token]/{page.tsx,actions.ts}` |
| **Organisms** | `console/modules/admins/{AdminTable,InviteAdminForm,TransferPrimaryPanel,RecoveryPanel}.tsx` · `console/modules/invite/InviteAcceptPanel.tsx` · `console/modules/sessions/SessionTable.tsx` · `console/modules/authSettings/{ProviderList,ProviderEditor,DiscoveryHealthPanel}.tsx` |
| **Molecules** | `console/components/molecules/{ExpiryPicker,CopyableLink,ScopeChipList,DangerConfirmDialog}.tsx` — presentational and **feature-agnostic**; they receive data and callbacks via props, make no Moira calls, and contain no auth logic |
| **Atoms** | `console/components/atoms/{Tooltip,Avatar,Divider}.tsx` — primitives only |
| **Shared non-UI** | `console/lib/{github.ts,invites.ts}` — `import "server-only"`; never in `components/` |

The one-way dependency rule (pages → organisms → molecules → atoms) is enforced by plan 08's `console/tests/unit/architecture/layer-dependencies.test.ts`, which covers these files automatically and must stay green. `OnceOnlySecretModal` and `SignInPanel` are **reused from plan 08**, not duplicated.

### Moira-side (Rust) changes

- `migrations/0017_admin_invites.sql` (append-only; §0.5) — `admin_invites` table, the `admin_identities.is_primary` column and its backfill (**§0.2 D1** — row state, not a scope backfill), and the **unconditional** auth-settings multi-provider extension, which must drop and re-add two CHECK constraints and the method/issuer unique index (**§0.1 B6**).
- `src/domain/identity.rs` (extending the file plan 07 created — **not** `src/domain/admin.rs`, keeping 07's identity/admin domain-type separation): `AdminInviteCreateRequest`, `AdminInviteRecord`, `AdminInviteSecretResponse` (once-only, mirrors `ApiKeySecretResponse`), `AdminInvitePreviewRequest`/`AdminInvitePreviewResponse`, `AdminInviteRedeemRequest`, `AdminIdentityPatchRequest`. `AdminIdentityRecord` **already exists** from 07 module 2 — reuse it, do not redefine. All request DTOs use `deny_unknown_fields`, matching the existing convention.
- `src/application/identity.rs` — extend 07's `AdminIdentityService`: `create_admin_invite`, `list_admin_invites`, `revoke_admin_invite`, `preview_admin_invite` (token-keyed lookup, no scope check, token-hash verification via `ApiKeyHasher` with a new namespace, e.g. `moira_invite`, reusing the hasher-with-namespace pattern 07 module 6 established), `redeem_admin_invite` (atomic: validate token, validate the body-carried verified email against the constraint, insert the `admin_identities` grant, consume the invite — one Postgres transaction under the same advisory-lock envelope 07's claim uses), `list_admin_identities`, `patch_admin_identity` (scope toggle with the last-primary guard), `revoke_admin_identity` (soft revoke — 07's explicitly-deferred revoke endpoint landing here).
- `src/infra/repositories/identity.rs` — extend 07's `AdminIdentityRepository` trait + `PgAdminIdentityRepository` with invite methods, reusing the existing atomic-idempotency / `pg_try_advisory_xact_lock` pattern the audit calls "genuinely correct and DB-backed."
- `src/http/identity.rs` — handlers with `#[utoipa::path]` for every endpoint above, wired into `src/http/mod.rs::documented_router()` and the route-coverage test's expected-path set.
- `src/security/authz.rs` — new scope constants `moira:admins:invite`, `moira:admins:read`, `moira:admins:manage` (following the `moira:jwt-issuers:{read,write,delete}` naming convention), **added to `ADMIN_SCOPES`** so admin authorization recognises them and 07's claim-endpoint scope validation ("every requested scope must be a member of `ADMIN_SCOPES`") accepts grants carrying them.
- `src/i18n/catalog/{errors,notices}.rs` + `docs/i18n-response-catalog.json` — the entries listed in Architecture § i18n.

### Tests (exact file names)

**Rust unit** (`#[cfg(test)] mod tests` beside the code, no database):
- `src/application/identity.rs` — invite-token validation (expiry, consumption, email match, domain match, case/IDN normalisation), the **last-primary guard**, expiry hard-cap enforcement, scope-constant membership in `ADMIN_SCOPES`.

**Rust e2e / integration** (real PostgreSQL 16 + pgvector, following `tests/support/mod.rs`):
- `tests/admin_invite_lifecycle.rs` — create/preview/redeem/revoke happy paths; expired, consumed, revoked, wrong-email, wrong-domain redemption rejections; **concurrent-redemption single-winner** test (two simultaneous redeems on one token → exactly one succeeds), using an **acknowledgement gate, never `sleep()`** (CONVENTIONS §3 / finding P2-12); last-primary guard rejection; atomic recovery-swap test including a mid-transaction failure injection asserting neither the revoke nor the grant persists if the other fails; redeem with a JWT from an issuer that is **not** the console's registered issuer → rejected; redeem with a JWT carrying a self-asserted `scope` claim → the claim is **not** honoured (Moira's authorization still comes from `granted_scopes`). **Plus the D3/D5 inheritance tests, named:**
  - `redeem_requires_email_and_email_verified` — a redeem body omitting `email`, or omitting `email_verified`, is rejected with `moira.error.invalid_request` (400/422) before the service runs; neither field has a serde default.
  - `redeem_rejects_unverified_email` — `email_verified: false` is refused, never silently coerced.
  - `redeem_denies_domain_outside_provider_allow_list` — a **valid, unexpired, constraint-matching** invite whose invitee's domain is absent from every enabled provider's `allowed_email_domains` is refused **`403 admin_claim_domain_not_allowed`** — proving an invite grants no exemption.
  - `redeem_denies_when_no_provider_is_enabled` — with zero enabled providers (deny-by-default), every redemption is refused regardless of token validity.
  - `redeem_denies_when_governing_provider_is_disabled` — a provider whose allow-list covers the domain but which is **disabled** does not govern the issuer, so redemption is still refused.
  - `recovery_invite_gets_no_domain_policy_exemption` — the same denial applies with `is_recovery: true`, proving there is no recovery carve-out.
  - `denied_redemption_does_not_consume_the_invite` — after an `admin_claim_domain_not_allowed` rejection the invite is still `pending` and succeeds once the allow-list is widened, in the same test.
  - `invite_constraint_and_domain_policy_are_distinct_codes` — an invite-constraint mismatch yields `invite_email_mismatch`/`invite_domain_mismatch` while an allow-list miss yields `admin_claim_domain_not_allowed`; the two are never conflated.
  - `granted_identity_always_has_an_email` — every `admin_identities` row created by redemption has a non-null `email` (D5: `AdminIdentityRecord.email` is `String`).
- `tests/http_error_contract.rs` (extended) — every new error code returns a non-empty `message_key` **and** `message`, and every new key exists in the catalog (CONVENTIONS §4.5).
- `src/http/mod.rs` — route-coverage and atomic-idempotency-contract tests extended to include every new path with its `Idempotency-Key`/`If-Match` expectations.
- DB-dependent tests fail closed in CI (`panic!` when **`CI=true`** and `MOIRA_TEST_DATABASE_URL` is absent (value check per `CONVENTIONS.md` §3 — never `var_os("CI").is_some()`)) — the existing pattern.

**Console unit** (`bun test`):
- `console/tests/unit/lib/github.test.ts` — verified-primary-email extraction across null / unverified / noreply-only / multiple-emails cases; org-membership check pass and fail; every call is server-side.
- `console/tests/unit/lib/invites.test.ts` — the raw invite token is never logged, never serialised into a client payload, and never placed in a URL query string. **Plus the D5 propagation, named:** `redeem_request_always_sends_email_and_email_verified` (every redeem body carries both fields, unconditionally, with no branch making either optional) and `redeem_request_types_email_as_required` (the TypeScript shape has no `?`, no `| null`, no `| undefined` on either field, so an omitting call site fails `bun run typecheck`).
- `console/tests/unit/lib/auth-settings-multi.test.ts` — N providers map to a single `genericOAuth({ config: [...] })`; `requireIssuerValidation: true` and `pkce: true` are forced on **every** entry; a provider missing a discovery URL cannot be enabled; per-provider allowed-domain lists are **deny-by-default**. **Plus the D7 multi-provider tests, named:**
  - `each_provider_composes_moira_config_with_its_own_console_secret` — N Moira rows pair with N `authProviderSecret` rows by `moiraProviderId`; no cross-wiring.
  - `no_provider_reads_secret_material_from_moira` — no code path types, requests, or consumes a `client_secret`, `secret_fingerprint`, `masked_secret`, or `has_secret` from any Moira auth-provider response, for any provider.
  - `drifted_provider_is_excluded_and_others_still_resolve` — one provider's fingerprint mismatch excludes **only** that provider; the rest are constructed normally.
  - `missing_mismatched_and_undecryptable_are_three_distinct_per_provider_conditions` — never collapsed into one key.
  - `secret_rotation_for_one_provider_invalidates_the_composite_cache` — a rotation touching Moira not at all still rebuilds the instance.
  - `all_providers_unresolvable_falls_back_to_auth_settings_unavailable` — fail-closed with no sign-in buttons; never a provider built with an empty secret.
  - `there_is_no_rotate_secret_call_for_any_provider` — no method, path constant, type, or string `rotate-secret` exists anywhere under `console/`.
  - `only_provider_secrets_module_encrypts_or_fingerprints` — no module other than `lib/provider-secrets.ts` performs `createCipheriv`/`createHmac` on secret material; this plan introduces no second encryption path.
- `console/tests/unit/lib/moira-token.test.ts` (extended from 08) — the invitee's redeem-time token still carries **no `scope`/`scp` claim** and binds `sub` to the IdP subject, not the Better Auth `user.id`.
- Organisms: `console/tests/unit/modules/admins/{AdminTable,InviteAdminForm,TransferPrimaryPanel,RecoveryPanel}.test.tsx`, `console/tests/unit/modules/invite/InviteAcceptPanel.test.tsx`, `console/tests/unit/modules/sessions/SessionTable.test.tsx`, `console/tests/unit/modules/authSettings/{ProviderList,ProviderEditor,DiscoveryHealthPanel}.test.tsx`.
- Molecules (one per new molecule): `console/tests/unit/molecules/{ExpiryPicker,CopyableLink,ScopeChipList,DangerConfirmDialog}.test.tsx`.
- Atoms (one per new atom): `console/tests/unit/atoms/{Tooltip,Avatar,Divider}.test.tsx`.
- Architecture guards from plan 08 (`layer-dependencies`, `server-only-guards`, `no-secret-props`, `no-hardcoded-copy`, `i18n-catalog-coverage`) must remain green with the new files included — no new test file needed, but their passing is a Definition-of-Done item.

**Console e2e** (Playwright, **`console/e2e/`, named `*.e2e.ts`** — `playwright.config.ts:63-64` is `testDir: "./e2e"`, `testMatch: "**/*.e2e.ts"`, and the suffix is deliberate: Bun's default matcher collects `*.spec.*`, so a Playwright `.spec.ts` anywhere under `console/` reds the `bun test` gate. Local mock OIDC + a mock GitHub OAuth/API stub — **never real GitHub or Google in CI**):
- `invite-redeem.e2e.ts` — admin A creates an invite; a **fresh browser context** redeems it as invitee B via mock OIDC; both appear as distinct admins in `GET /api/v1/admin/admin-identities` (asserted by a direct API call, not only through the UI).
- `invite-negative.e2e.ts` — mismatched email, mismatched domain, expired token, double-redeem (`invite_already_consumed`), and a concurrent double-redeem race asserting exactly one winner end-to-end.
- `invite-domain-policy.e2e.ts` — **the D3 inheritance + ordering spec for invitations.** Named tests:
  - `invite_does_not_bypass_the_provider_allow_list` — a valid, unexpired, constraint-matching invite for a domain absent from every enabled provider's `allowed_email_domains` is refused at redemption with `403 admin_claim_domain_not_allowed`; a direct API check confirms **no** `admin_identities` grant was created.
  - `denied_invitee_sees_actionable_instruction_not_a_failure` — the `/invite/[token]` page renders the `console.invite.domain_not_allowed.*` instruction (invite still valid, ask an admin to widen the allow-list) and **not** the generic error banner, stack trace, or raw envelope.
  - `invite_form_blocks_creation_for_a_domain_outside_the_allow_list` — `InviteAdminForm` refuses to submit and shows `console.admins.invite_domain_not_in_allow_list` with a working link to `/settings/auth`, so the stranding case cannot be created through the UI.
  - `invite_form_is_disabled_when_no_provider_is_enabled` — with zero enabled providers the invite form is disabled with `console.admins.no_enabled_provider`.
  - `allow_list_widened_then_original_invite_redeems` — the **ordering** assertion: after the operator adds the domain in `/settings/auth` (no redeploy), the *same, previously-denied* invite redeems successfully and the invitee appears in `GET /api/v1/admin/admin-identities` — proving the denial did not consume it and that configure-allow-list-before-redeem is the correct order.
  - `recovery_invite_is_held_to_the_same_policy` — a recovery-flagged invite for an out-of-allow-list domain is denied identically; no recovery carve-out exists.
  - `redeem_always_sends_email_and_email_verified` — a request tap over the whole flow shows every redeem body carried both fields (D5).
- `github-signin.e2e.ts` — mock GitHub returning (a) no verified email → rejected, (b) a verified primary email → accepted, (c) non-member of `required_org` when configured → rejected.
- `ownership-transfer.e2e.ts` — A transfers primary to B; A can no longer manage admins; B can; the audit log (queried via Moira's API) shows the correct event.
- `recovery.e2e.ts` — B recovers a simulated locked-out A into a new identity C; A's old grant is gone; the audit log shows `admin_identity_recovered` as a distinct event type.
- `sessions.e2e.ts` — two concurrent sessions for one admin; revoking one from `/settings/sessions` ends **that** session (the other survives); "revoke all others" leaves only the current one. **Real revocation, asserted against the DB-backed session store.**
- `multi-provider.e2e.ts` — configure two OIDC providers plus GitHub from `/settings/auth`; all three sign-in buttons render; each callback route responds; disabling one removes its button **without a redeploy**. Additionally asserts, via a direct API read, that **every** Moira auth-provider response is free of secret material (D7).
- `auth-secret-drift-multi.e2e.ts` — **the D7 per-provider drift spec** (plan 08's `auth-secret-drift.e2e.ts` generalised to N providers). Named tests:
  - `client_id_changed_in_moira_for_one_provider_surfaces_an_actionable_named_mismatch` — patch one provider's `client_id` directly against Moira's API; the console renders `console.error.auth_provider_client_id_mismatch` **naming that provider**, with the remedy.
  - `drift_in_one_provider_does_not_disable_the_others` — the other two providers' sign-in buttons still render and sign-in still succeeds through them.
  - `mismatch_never_reaches_that_providers_token_endpoint` — a network tap records **zero** outbound requests to the drifted IdP's token endpoint; the failure is caught before the exchange, never as an opaque provider error.
  - `re_entering_the_secret_for_the_new_client_id_clears_the_mismatch` — rotation through `ProviderSecretRotatePanel` rewrites ciphertext and fingerprint; sign-in works again with no redeploy.
  - `missing_secret_for_a_newly_added_provider_is_its_own_distinct_error` — a Moira provider row with no console secret yields `auth_provider_secret_missing`, not the mismatch key.
  - `per_provider_partial_write_leaves_only_that_provider_disabled` — with the console-DB write forced to fail for one provider, that row shows `auth_provider_secret_write_failed` with Retry/Discard and is confirmed **disabled** by a direct Moira read, while every other provider stays enabled and usable.
  - `rotating_one_provider_secret_issues_no_moira_request` — a recording proxy confirms zero Moira traffic for a secret-only rotation.
- `authorization-denial.e2e.ts` (extended from 08) — a signed-in identity with **no** grant is denied on every new admin screen and server action; an identity holding `moira:admin` but **not** `moira:admins:manage` can view `/admins` but cannot transfer or revoke.
- `a11y.e2e.ts` (extended) — `@axe-core/playwright` on **every new page route**: `/admins`, `/invite/[token]`, `/settings/sessions`, `/settings/auth`. Zero critical/serious violations gates CI.
- `i18n-message-key.e2e.ts` (extended) — force `invite_expired` and `admin_identity_last_primary`; assert the console renders the catalog string; then force an **unknown** `message_key` and assert the server-supplied `message` renders verbatim.
- `secret-leak.e2e.ts` (extended) — no invite token (beyond the single intentional once-only reveal), **no GitHub or OIDC client secret for any provider**, and no console encryption key ever appears in a browser-observed response body, rendered HTML, RSC payload, or `console.log`. **Plus the D7 test, named:** `no_provider_client_secret_appears_in_any_request_to_moira` — the recording proxy in front of the fixture Moira captures every request across configuring three providers, rotating each one's secret, changing one's `client_id`, and the full invite/transfer/recovery journey; **every** provider's client-secret fixture must appear in **zero** of them, with an **empty** violation set.
- `console/tests/unit/architecture/bundle-scan.test.ts` (**new — §0.4: plan 08 shipped no bundle-scan file**) — the build output and SSR HTML contain no invite-token fixture, **no client-secret fixture for any of the configured providers**, no `CONSOLE_SECRET_ENCRYPTION_KEY` fixture, no PEM header, and no `NEXT_PUBLIC_*` name matching `/(SECRET|KEY|TOKEN|PASSWORD)/i`; the violation set must be **empty**.

### Documentation

- `docs/admin-console.md` (created in plan 08) — new sections: multi-provider OIDC configuration, GitHub configuration + verified-email and org-membership behavior, the invitation flow, ownership transfer, the recovery flow, and **real** session management (the old "best-effort" caveat is **removed**, since DB-backed sessions make revocation genuine). Plan 08's **D7 section is extended to the multi-provider case**: one console-owned encrypted secret per provider, each with its own `client_id` fingerprint; the per-provider drift states and what each keyed error means operationally; the per-provider console-side rotation procedure; and a restated note that **Moira never stores or returns a client secret and has no `rotate-secret` endpoint**, so adding a provider adds no deployment secret and no chart value. The existing statement that **SAML SSO is not supported** (mode 3 is the path) is retained and restated here.
- `docs/jwt-issuer-management.md` — **not modified**: the console's own trusted-JWT-issuer registration from plan 08 is unchanged; only the human-facing OAuth providers multiply, not Moira's JWT trust model.
- `docs/admin-invitations.md` (or a section of `docs/admin-console.md` — implementer's choice at the OpenAPI-skill pass) documenting the new endpoint family per `.claude/skills/moira-openapi/SKILL.md`.
- `docs/i18n-response-catalog.json` — updated with every new error and notice key in the same PR.

### Deployment assets

- `charts/moira-console/values.yaml` — no new **secret** values (provider secrets live in Moira, encrypted). Any new non-secret toggle is added optionally; the chart must render correctly with only plan 08's values set.
- `charts/moira/templates/migration-job.yaml` — unchanged mechanism; it already runs all pending migrations.

---

## Multi-Agent Workflow

### Waves

**Wave 0 — Coordinator checkpoint (sequential, blocking).**
- Confirm plan 08's console is merged/stable and plan 07's `admin_identities` shape is final (this plan backfills a scope into `granted_scopes` and relies on the `version` trigger for `If-Match`; a moving target would force a migration rewrite).
- **Re-confirm plan 07's frozen-contract change (D3/D5) against shipped code**, since this plan's redeem path mirrors it: `AdminIdentityRecord.email` is a non-nullable `String`; `ClaimAdminIdentityRequest.email`/`email_verified` are required with no serde default (the redeem DTO must match); and a claim/redeem for a domain outside every **enabled** provider's `allowed_email_domains` is refused `403 admin_claim_domain_not_allowed` with **no exemption** — confirming there is no carve-out for this plan's invitations to inherit or imitate. Any divergence is an escalation, not a local workaround.
- **Confirm D-1's shipped auth-settings shape** and decide whether the multi-provider extension migration is needed. **That migration adds non-secret columns only.**
- **Re-verify D7 conformance for the multi-provider surface** (CONVENTIONS §0 D7 — a conformance check, not a decision): no `client_secret` or encrypted-envelope column on `auth_provider_settings`; no `client_secret`/`secret_fingerprint`/`masked_secret`/`has_secret` field on any auth-provider request or response DTO, in list **or** detail; `POST /api/v1/admin/auth/providers/{id}/rotate-secret` absent from the OpenAPI document; `auth_provider_secret_rebind_required` absent from Moira's catalog and `docs/i18n-response-catalog.json`; the operation count still **10**. Also confirm plan 08 shipped `console_auth.authProviderSecret` and `lib/provider-secrets.ts` — this plan **reuses them and adds no second secret store or encryption path**. Any divergence is an escalation; **adding a Moira read-back endpoint is not an available remedy**, it was considered and rejected.
- Resolve product decision 1 (**single vs. multiple primaries**) — blocking before Wave 3, since it changes the transfer server action materially. Resolve decisions 2–4 as well.
- Re-verify the toolchain pins are still §5's values; do **not** bump anything in this plan.

**Wave 1 — Moira-side identity-invite backend (single owner, internally sequential; fully independent of console work).**
- *Backend/Rust engineer*: the migration, `src/domain/identity.rs` DTOs, `src/infra/repositories/identity.rs`, `src/application/identity.rs`, `src/http/identity.rs` + route wiring, `src/security/authz.rs` scope constants, `src/i18n/catalog/{errors,notices}.rs` + `docs/i18n-response-catalog.json`, `tests/admin_invite_lifecycle.rs`, and the OpenAPI/route-coverage test updates. **Single owner for the whole vertical slice** — `src/http/mod.rs`'s route table and its expected-path `BTreeSet` are single, shared, order-sensitive collections.
- *Read-only security reviewer*: re-checks that invite-token hashing reuses `ApiKeyHasher` (not a fresh `sha256` — a direct regression risk against P1-1) and that the atomic-swap transaction boundaries are correct.

**Wave 2 — Console provider extensibility (parallel with Wave 1; depends on Wave 0 only).**
- *Security/OAuth engineer*: `console/lib/{auth,auth-settings,github}.ts` extensions, `console/modules/authSettings/**`, `console/app/(console)/settings/auth/**`, plus every security-invariant unit test **and the whole per-provider D7 surface** (dual write, per-provider fingerprint drift check, per-provider rotation). Reuses plan 08's `lib/provider-secrets.ts` **unchanged** — extending it is allowed only for multi-row convenience helpers, never with a second encryption or fingerprint scheme. No shared files with Wave 1 (console vs. Rust).

**Wave 3 — Console invitation & admin-management UI (after Wave 1 ships its endpoints and Wave 0's primary-model decision; parallel internally by directory).**
- *Frontend engineer A*: `console/app/(console)/admins/**` + `console/modules/admins/**`.
- *Frontend engineer B*: `console/app/invite/[token]/**` + `console/modules/invite/**` (public route, separate directory — no overlap with A).
- *Frontend engineer C*: `console/app/(console)/settings/sessions/**` + `console/modules/sessions/**`.
- *Design-system engineer*: the new molecules and atoms (`ExpiryPicker`, `CopyableLink`, `ScopeChipList`, `DangerConfirmDialog`, `Tooltip`, `Avatar`, `Divider`) **plus their unit tests** — presentational only, touching neither `lib/` nor `modules/`, which keeps this track fully parallel and the layering honest by construction.

**Wave 4 — Integration, hardening, deployment (parallel, disjoint).**
- *Test engineer*: the full e2e suite additions and the mock GitHub stub; `tests/admin_invite_lifecycle.rs` finalisation in coordination with Wave 1's owner (**read access only** to that Rust test file — writes go through the Wave 1 owner).
- *DevOps engineer*: `charts/moira-console/**` value additions and a rollout-order check of `charts/moira/templates/migration-job.yaml` against the new migration (no edit expected).
- *Security reviewer*: final pass on token redaction, the last-primary guard, and confirmation that the invite flow cannot escalate a non-invited identity (negative tests: redeem with a mismatched email; redeem with a JWT from a non-registered issuer; redeem with a self-asserted `scope` claim). **Plus the D3/D5 inheritance checks: (i) no code path treats a valid invite as an exemption from the `allowed_email_domains` policy — including the recovery path; (ii) no `Option`/`#[serde(default)]`/`?` reintroduces optional `email`/`email_verified` on the redeem DTO or its TypeScript mirror; (iii) the invite-constraint and provider-allow-list checks remain distinct codes. Plus the D7 checks: (iv) no request to Moira carries any provider's client secret and no type expects Moira to return secret material; (v) no reference to a `rotate-secret` endpoint exists anywhere; (vi) every provider's save is a same-step dual write that never advances on partial success, and every provider's load performs its own `client_id` fingerprint check; (vii) no second encryption or fingerprint path was introduced alongside `lib/provider-secrets.ts`.**

### Conflict avoidance
- The Rust vertical slice (Wave 1) has **one owner end-to-end** because `src/http/mod.rs`'s route table and its expected-path set are shared, order-sensitive collections — the same discipline the branch history already shows (`feat: make admin commands atomic`, `fix: harden admin idempotency isolation` as sequential, focused commits).
- Console waves are disjoint by directory (`admins/`, `invite/`, `settings/sessions/`, `settings/auth/`, `components/`) with zero cross-writes, mirroring plan 08's Wave 3 pattern.

### Pull request (CONVENTIONS §1.4)

One PR against `main` from `plan/09-generic-oidc-github-invitations`, opened only after every §2 gate (Rust **and** frontend) passes locally, with the required sections: **Plan link** (`plans/09-generic-oidc-github-invitations.md`) · **Findings addressed** (P1-11, P1-1) · **Migrations included** (filenames) · **Breaking API/OpenAPI changes** (none — all additive) · **Test evidence** (`cargo test`, `bun test`, `bunx playwright test` summaries) · **Rollback procedure** · **Deferred follow-ups**. Because this plan changes the OpenAPI surface, it must land **before** plan 05's OpenAPI-drift gate freezes the spec (CONVENTIONS §1.6), or coordinate a spec-snapshot regeneration in the same PR.

---

## Interfaces & Contracts

### BFF↔Moira endpoints and headers (new, additive to plan 08's set)

| Call | Auth | Idempotency / concurrency |
|---|---|---|
| `POST /api/v1/admin/admin-invites` | `Authorization: Bearer` (`moira:admins:invite`) | `Idempotency-Key` required |
| `GET /api/v1/admin/admin-invites`, `GET .../{id}` | `Authorization: Bearer` (`moira:admins:read`) | n/a (read) |
| `POST /api/v1/admin/admin-invites/{id}/revoke` | `Authorization: Bearer` (`moira:admins:invite`) | `Idempotency-Key` required |
| `POST /api/v1/admin-invites/preview` (outside `/admin/`, per 07's non-admin-credential path precedent) | token in **body** (no bearer) | n/a (read; rate-limited server-side to blunt token guessing, reusing plan 03's middleware stack once P1-3 exists) |
| `POST /api/v1/admin-invites/redeem` | token in body **+** `Authorization: Bearer` (invitee's freshly minted, **scope-claim-free** JWT) | `Idempotency-Key` required; single winner via advisory lock on the token hash |
| `GET /api/v1/admin/admin-identities` | `Authorization: Bearer` (`moira:admins:read`) | n/a (read) |
| `PATCH /api/v1/admin/admin-identities/{id}` | `Authorization: Bearer` (`moira:admins:manage`) | `If-Match` **and** `Idempotency-Key` required |
| `DELETE /api/v1/admin/admin-identities/{id}` | `Authorization: Bearer` (`moira:admins:manage`) | `Idempotency-Key` required |
| Moira auth-settings read/write (multi-provider, **10 operations**) | `X-Moira-System-Key` (console boot) / `Authorization: Bearer` (`moira:admin`) for the settings screen | `Idempotency-Key` + `If-Match` on write. **D7: non-secret config only** — no request carries a client secret and no response carries secret material for any provider. **`rotate-secret` does not exist**; rotation is console-side, per provider. |
| Console-owned per-provider client secret (**not a Moira call**) | none — a console-DB write via plan 08's `lib/provider-secrets.ts` | **D7.** One encrypted `authProviderSecret` row per provider, keyed by `moiraProviderId`, each with its own `client_id` fingerprint, written in the same step as that provider's Moira config write. Listed here so the contract table shows the whole configuration write, not just its Moira half. |

### JWT claims for the redeem call

Identical to plan 08's minted token (`iss`, `sub`, `aud`, `iat`/`exp` ≤ 120s, `jti`) and, per 08's corrected contract, **no `scope`/`scp` claim and no email claims**. The invitee's freshly-authenticated Better Auth session produces this token from the `jwt` plugin exactly as any post-claim admin session does; since the invitee has no `admin_identities` grant yet, 07's grant union yields **zero scopes** — the token proves `(iss, sub)` and nothing more, which is precisely what redemption needs. The invitee's verified `email`/`email_verified` travel in the redeem request **body** (BFF-asserted from the session), mirroring 07's claim endpoint — and, per **D5**, **both are required and non-optional there, exactly as in the post-change `ClaimAdminIdentityRequest`**. The BFF sends both on **every** redemption with no conditional branch; a grant cannot exist without an email (`AdminIdentityRecord.email: String`). `sub` is the **IdP's stable subject** (plan 08's `jwt.getSubject` → `idpSubject`), never the Better Auth `user.id`. **No new claim shape is introduced** — invitation is "the claim flow, generalized to N times with a scoping token," not a parallel identity mechanism — and because it *is* the claim flow, it inherits **D3's deny-by-default domain policy with no invitation-based exemption**.

### Scopes/authz

- **Interaction with the full-admin scope.** Plans 07/08 grant `moira:admin` (`ADMIN_SCOPE`), which Moira's admin authorization already treats as satisfying admin endpoints regardless of granular scope — so existing admins can invite/manage without a re-grant, matching how `moira:jwt-issuers:*` granular scopes coexist with `moira:admin` today. **Except `moira:admins:manage`**, which is deliberately checked as an **explicit** scope (**not** implied by `moira:admin`), because it is precisely the primary/ownership distinction *between* admins who all hold `moira:admin`. This carve-out deviates from the implied-by-full-admin default and must be implemented **and tested** as an explicit check.
- `moira:admins:invite` — create/revoke invites.
- `moira:admins:read` — list admin identities and invites.
- `moira:admins:manage` — patch (primary-scope toggle), revoke admin identities, and create **recovery** invites. Recovery-invite creation is gated on `moira:admins:manage`, not merely `moira:admins:invite`, since recovery is a higher-privilege action than routine onboarding.
- Redemption itself requires **no** admin scope on the invitee's JWT — authorization for redemption is possession of a valid token plus a matching verified email/domain, exactly analogous to the plan-07 claim being system-key-gated rather than scope-gated. **Possession authorises submission, not policy exemption:** the token gets the invitee past *authentication of the request*, after which 07's deny-by-default `allowed_email_domains` check applies in full (D3). There is **no invitation-based exemption and no recovery-invite exemption.**

### Error handling & i18n

Same `ErrorResponse` envelope and console-side mapping as plan 08 (`lib/errors.ts` → `{ code, messageKey, message, messageArgs }`; `details`/`request_id` stay server-side). New error codes and notice keys are listed in Architecture § i18n; each has a catalog entry with an English default plus a `docs/i18n-response-catalog.json` mirror, asserted by an extended `tests/http_error_contract.rs`. The console renders `t(messageKey, messageArgs, message)` and **never hardcodes English copy for a server-originated condition**.

### Session cookie attributes, CSRF/PKCE/state/nonce/redirect validation, logout

Unchanged from plan 08 — Better Auth provides these uniformly per provider (`trustedOrigins` origin validation for CSRF, `advanced.disableCSRFCheck` never set, PKCE forced on every `genericOAuth` entry, state/nonce handled by the flow, `advanced.defaultCookieAttributes` for httpOnly/Secure/SameSite). The only new redirect surface is each new provider's callback URL — `/api/auth/callback/github` for the social provider and `/api/auth/oauth2/callback/<providerId>` for each generic-OIDC entry — each registered **exactly** (no wildcards) at its IdP and covered by `middleware.ts`'s unchanged host allow-list. Logout gains a real multi-session dimension: revoking a session deletes its DB row, so revocation is immediate and genuine.

---

## Verification

### Gates (CONVENTIONS §2)

**Rust**
```bash
cargo fmt --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo build --release --locked
```
Plus clean PostgreSQL migration validation from an **empty** database, and specifically: the `moira:admins:manage` backfill verified against a database seeded with plan 07/08 claim state, **including** the empty-database and revoked-claimant edge cases.

**Frontend**
```bash
bun install --frozen-lockfile
bun run lint
bun run typecheck
bun test                # unit
bunx playwright test    # e2e
bun run build
```

### Unit
- Rust: in-module `#[cfg(test)] mod tests` in `src/application/identity.rs` as enumerated in Detailed Implementation § Tests.
- Console: `bun test` over `console/tests/unit/**` — `lib/{github,invites,auth-settings-multi,moira-token}.test.ts`, one test per **new atom**, one per **new molecule**, and one per **new organism**, plus plan 08's architecture guards remaining green.

### E2E
- Rust/HTTP: `tests/admin_invite_lifecycle.rs` against a real PostgreSQL 16 + pgvector, using acknowledgement gates (never `sleep()`) for the concurrency cases, failing closed in CI when `MOIRA_TEST_DATABASE_URL` is absent.
- Browser: `console/e2e/{invite-redeem,invite-negative,invite-domain-policy,auth-secret-drift-multi,github-signin,ownership-transfer,recovery,sessions,multi-provider,authorization-denial,i18n-message-key,secret-leak,a11y}.e2e.ts` (§0.5 — `console/e2e/`, `*.e2e.ts`; `secret-leak.e2e.ts` and `a11y.e2e.ts` are **extended**, not new) (`invite-domain-policy.e2e.ts` is the D3 inheritance + ordering spec; **`auth-secret-drift-multi.e2e.ts` is the D7 per-provider drift spec**) against a running console + a real test-fixture Moira **behind a recording proxy** (so the no-secret-to-Moira assertions can inspect every outbound request), with a **local mock OIDC provider and a mock GitHub OAuth/API stub** — never real GitHub or Google in CI.

### Accessibility
`@axe-core/playwright` on **every new page-level route** (`/admins`, `/invite/[token]`, `/settings/sessions`, `/settings/auth`) plus plan 08's existing routes. Zero critical/serious violations gates CI.

### Secret-leak
Extended `console/e2e/secret-leak.e2e.ts` (browser-observed responses, rendered HTML, console output, via `e2e/support/leak-tap.ts`) covering the invite token and the GitHub/OIDC client secrets. §0.4: **`console/tests/secret-leak/bundle-scan.test.ts` does not exist** — plan 08 never shipped a separate bundle-scan file, so a build-output + SSR-HTML scan with an **empty** violation set is **new work owned by this plan**, placed under `console/tests/unit/architecture/`.

### Production-config tests
Boot with only Google configured (plan 08's baseline) → invite/OIDC/GitHub UI elements are gracefully absent, not broken. Boot with all providers configured → every sign-in button renders and every callback route responds. Boot with an unreachable discovery URL → that provider cannot be enabled and the screen shows a keyed health state. **Boot with one provider's console secret deleted and another's `client_id` drifted (D7)** → those two providers are excluded with their own distinct keyed states, every other provider still signs in, and no OAuth exchange is attempted for either broken one. **Boot without `CONSOLE_SECRET_ENCRYPTION_KEY`** → the console fails closed at startup with `console.error.secret_encryption_key_missing`, exactly as in plan 08.

### Helm / Kubernetes validation
`helm lint` and `helm template … | kubeconform` on `charts/moira-console` with the new optional values both set and unset (two template runs), asserting no secret value renders into a `ConfigMap` in either case **and that no provider client secret is a chart value at all (D7 — every provider's secret is entered in the console and stored in the console's own database, so adding a provider adds no `Secret` key)**.

---

## Definition of Done

**CONVENTIONS §8 compliance checklist**
- [ ] Work performed on branch `plan/09-generic-oidc-github-invitations`; PR opened with all required description sections (§1.4).
- [ ] All §2 gates pass — Rust (`fmt`, `clippy`, `test`, `build --release --locked`, clean migration validation) **and** frontend (`bun install --frozen-lockfile`, `bun run lint`, `bun run typecheck`, `bun test`, `bunx playwright test`, `bun run build`).
- [ ] **Unit tests** delivered and passing — Rust in-module tests, plus one console test per **new atom**, per **new molecule**, and per **new organism**.
- [ ] **E2E tests** delivered and passing — HTTP-level `tests/admin_invite_lifecycle.rs` for Rust, and Playwright for the console (invite/redeem, GitHub sign-in, ownership transfer, recovery, sessions, multi-provider, authorization denial).
- [ ] Every new error/notice string has an i18n **key + English default** in the Rust catalog, mirrored into `docs/i18n-response-catalog.json`, with `tests/http_error_contract.rs` asserting presence; every new console string has a `console.*` key with an English default and `i18n-catalog-coverage.test.ts` / `no-hardcoded-copy.test.tsx` stay green.
- [ ] Toolchain pins unchanged and still §5-compliant (**Next.js 16.2.11 · Node 24 LTS · Bun 1.3.14**); `bun.lock` committed; **Atomic Design layering respected with the one-way dependency rule**, proven by `layer-dependencies.test.ts` covering every new file.
- [ ] Auth config for every provider is **runtime/DB-backed**: **non-secret config in Moira, each client secret console-owned and encrypted at rest (D7)**, composed per provider behind `loadAuthSettings()` and never returned to the browser; **no `scope` claim in any minted JWT**; per-provider domain policy is **deny-by-default**.
- [ ] **No secret-leak**, verified by the extended bundle scan (empty violation set) and the extended e2e secret-leak spec — including `no_provider_client_secret_appears_in_any_request_to_moira`.

**D7 conformance — per-provider console-owned secrets, no residual read-back assumption**
- [ ] **No text, type, client method, path constant, or doc in this plan assumes Moira stores or returns a client secret** for any provider. `no_provider_reads_secret_material_from_moira` passes. **No Moira read-back endpoint is proposed, called, or planned** — that option was rejected by D7.
- [ ] **Reuse, not reinvention.** Every provider's secret is stored in plan 08's `console_auth.authProviderSecret` via plan 08's `lib/provider-secrets.ts`, one row per provider keyed by `moiraProviderId`, under the same `CONSOLE_SECRET_ENCRYPTION_KEY`. **No new console table, no second encryption path, no alternative fingerprint scheme** — `only_provider_secrets_module_encrypts_or_fingerprints` passes. The table remains managed by Better Auth's CLI in `console/db/`, never in Moira's `migrations/`.
- [ ] **Per-provider dual write.** Saving any provider writes Moira's non-secret config and the console's secret in the **same step**, ordered Moira → console secret → `enable` (the commit point). Partial success is an **operator-resolvable failure** on that row with Retry/Discard, never a success, never an enabled provider without a secret, and never a blocker for the other providers. `per_provider_partial_write_leaves_only_that_provider_disabled` passes.
- [ ] **Per-provider drift check.** Each provider's `client_id` fingerprint is compared against Moira's `client_id` on every load; a mismatch yields the **specific, actionable, provider-named** `console.error.auth_provider_client_id_mismatch`, excludes only that provider, and **prevents its OAuth exchange from being attempted**. `client_id_changed_in_moira_for_one_provider_surfaces_an_actionable_named_mismatch`, `drift_in_one_provider_does_not_disable_the_others`, and `mismatch_never_reaches_that_providers_token_endpoint` pass. Missing / mismatched / undecryptable stay **three distinct** conditions.
- [ ] **Rotation is console-side, per provider.** Every provider's secret is rotatable from `/settings/auth` with **zero Moira requests** for a secret-only rotation and no redeploy. **Nothing anywhere under `console/` references `POST /api/v1/admin/auth/providers/{id}/rotate-secret`** — `there_is_no_rotate_secret_call_for_any_provider` and `rotating_one_provider_secret_issues_no_moira_request` pass.
- [ ] **The Moira auth-provider surface is still 10 operations carrying no secret material**, re-verified at Wave 0 against the shipped multi-provider shape and recorded in the PR.

**Plan-07 frozen-contract inheritance (D3/D5) — no residual mismatch**
- [ ] **D5 — required email on the invite path.** `AdminInviteRedeemRequest.email: String` and `email_verified: bool` are **required and non-optional** in the Rust DTO (no `Option`, no `#[serde(default)]`) and in the console's TypeScript shape (no `?`/`| null`/`| undefined`), matching 07's post-change `ClaimAdminIdentityRequest`. The BFF sends both on **every** redemption with no conditional branch. `redeem_requires_email_and_email_verified`, `granted_identity_always_has_an_email`, and `redeem_request_always_sends_email_and_email_verified` pass.
- [ ] **D3 — no invitation exemption.** An invite does **not** bypass the allow-list: a valid, constraint-matching invite whose domain is outside every enabled provider's `allowed_email_domains` is refused `403 admin_claim_domain_not_allowed`, no grant is created, and the invite is **not** consumed. Recovery invites are held to the identical policy. `invite_does_not_bypass_the_provider_allow_list`, `recovery_invite_gets_no_domain_policy_exemption`, `redeem_denies_when_no_provider_is_enabled`, and `denied_redemption_does_not_consume_the_invite` pass.
- [ ] **D3 — ordering + actionable error.** `InviteAdminForm` blocks creation of an invite that would be refused at redemption; `InviteAcceptPanel` renders `moira.error.admin_claim_domain_not_allowed` as an **actionable instruction**, never a generic failure, and never conflated with `invite_email_mismatch`/`invite_domain_mismatch`. `invite-domain-policy.e2e.ts` passes, including `allow_list_widened_then_original_invite_redeems`.

**Plan-specific**
- [ ] Multiple generic-OIDC providers **and** GitHub are configurable from the console; with none configured, the console behaves exactly as plan 08 shipped it (regression-tested).
- [ ] `requireIssuerValidation: true` and `pkce: true` are forced on **every** `genericOAuth` entry and cannot be disabled from the settings UI.
- [ ] GitHub sign-in enforces verified email **server-side** (not by trusting `profile.email`); the optional org-membership check works when configured.
- [ ] An existing admin can create an invite, share the link, and a second identity (any provider) redeems it and appears as a distinct admin in `GET /api/v1/admin/admin-identities`.
- [ ] Invite tokens are **Argon2id+pepper** hashed at rest (`ApiKeyHasher` reuse verified, **not** a new hashing scheme), single-use, and time-limited with a server-enforced hard cap, all covered by `tests/admin_invite_lifecycle.rs`.
- [ ] Ownership transfer via `PATCH /api/v1/admin/admin-identities/{id}` works, `moira:admins:manage` is checked **explicitly** (not implied by `moira:admin`), and the "cannot zero out the last primary" guard is enforced and tested.
- [ ] Recovery performs an **atomic** revoke-and-grant swap, audited distinctly as `admin_identity_recovered`.
- [ ] Session management performs **real** revocation against the DB-backed Better Auth session store, and `docs/admin-console.md` no longer describes it as best-effort.
- [ ] System-key break-glass (unchanged from 07/08) and **mode 3** (bring-your-own JWKS) remain available and untouched.
- [ ] `docs/admin-console.md` restates that **SAML SSO is not supported** and that mode 3 is the path.
- [ ] `src/http/mod.rs`'s route-coverage and atomic-idempotency-contract tests include every new endpoint.
- [ ] Accessibility gate clean on every new page route; `helm lint` + `kubeconform` clean for `charts/moira-console`.

---

## Risks & Rollback

### Security
- **Invite-token blast radius** — an invite grants full `moira:admin` on redemption, so a leaked, still-valid, constraint-matching link is equivalent to handing out admin access. Mitigations: short default expiry (recommend ≤72h, enforced server-side as a **hard cap**, not just a UI default), single-use, Argon2id+pepper hashed at rest, email/domain-bound (never "anyone with the link"), token in request **bodies only** (never a query string, so it cannot land in access logs or referer chains), and once-only display minimising the window it is visible anywhere.
- **GitHub's weaker identity guarantees** — compensated for by the server-side verified-primary-email lookup and the optional org-membership check, rather than trusting the OAuth profile at face value. This is the direct answer to `plans/01` §4.2's flagged GitHub weakness.
- **The no-scope-claim invariant** (inherited from plan 08) remains the load-bearing control keeping Moira the authorization system of record. This plan adds a redeem-time token minted for an identity with **zero** grants — the strongest possible demonstration that the console cannot self-grant. Asserted in both the Rust and the console suites.
- **`moira:admins:manage` must be an explicit check** — if a future refactor lets `moira:admin` imply it, every admin silently becomes a primary and ownership transfer becomes meaningless. Mitigated by a dedicated test and a named reviewer check item.
- **Recovery-flow abuse** — a compromised `moira:admins:manage` identity could "recover" (silently replace) every other admin. Mitigation: the atomic swap is fully audited with a distinct event type, and the Definition of Done requires it to be independently testable. This plan adds **no technical control beyond audit visibility** for that scenario (a compromised primary is a fundamentally hard problem at this layer); system-key break-glass remains the ultimate override to strip a compromised primary's scope or revoke the grant outright, bypassing the console.
- **Reintroducing an invitation-based exemption (D3 regression).** The most likely well-intentioned regression in this plan: a contributor "fixes" the denied-invitee experience by letting a valid invite bypass the `allowed_email_domains` check — reasoning that an existing admin already vouched for the invitee. That would recreate exactly the bypass plan 07 deliberately removed, and would make the deny-by-default policy unenforceable for every admin after the first. The correct fix is the **pre-submit gate in `InviteAdminForm`** plus the actionable invitee-facing instruction, never a policy carve-out. Mitigations: `invite_does_not_bypass_the_provider_allow_list` and `recovery_invite_gets_no_domain_policy_exemption` in `tests/admin_invite_lifecycle.rs`, `invite-domain-policy.e2e.ts`, and a named reviewer check item.
- **Optional-email regression (D5).** Equally: a contributor makes `email` optional on the redeem DTO to simplify a call site, reintroducing grants with no human-identifiable audit attribute and making the domain policy unenforceable on that path. Mitigated by `redeem_requires_email_and_email_verified`, `granted_identity_always_has_an_email`, the non-nullable column, and the reviewer check.
- **Last-primary lockout** — guarded server-side and negative-tested; residual bug risk is mitigated by system-key break-glass remaining a working fallback regardless (this plan never removes it).
- **New provider secrets are console-owned (D7)** — GitHub's and each generic-OIDC provider's client secret joins the console's encrypted-at-rest set, **not Moira's**. There is therefore **no auth-settings read endpoint that could return them too liberally**: Moira has nothing to return, which removes an entire class of over-exposure risk rather than mitigating it. Residual risk is concentration — the console DB now holds N client secrets plus the `jwt` private key. Mitigations inherited unchanged from plan 08: AES-256-GCM at rest under a dedicated key with AAD binding, a DB role with no grants on Moira's tables, TLS to Postgres, plaintext only in process memory, `server-only` guards, the bundle scan, and the outbound-request tap. Re-checked in this plan's review.
- **Drift multiplies with providers (D7's accepted cost, N times).** Each additional provider is another independent way for Moira's `client_id` and the console's stored secret to diverge, and the failure mode without protection is an opaque `invalid_client` from that IdP. Mitigations are **per provider and mandatory**: the same-step dual write, the per-provider fingerprint comparison on every load, and `auth-secret-drift-multi.e2e.ts`. The blast radius is deliberately bounded — a drifted provider is excluded alone and never takes the others down. The likeliest regression is a contributor optimising the per-provider check into a single aggregate check, or skipping it for "simple" providers like GitHub; mitigated by the named unit and e2e tests and reviewer check item (vi).
- **Reintroducing a Moira read-back path (D7 regression), multi-provider flavour.** The temptation is stronger here than in plan 08 — "N secrets in two places is worse than N secrets in one" — but the reasoning is the same and the answer is unchanged: exposing a decrypted secret over a network boundary breaks the invariant all of Moira's credential handling rests on, and D7 **rejected** that option explicitly. Mitigations: `no_provider_reads_secret_material_from_moira`, `there_is_no_rotate_secret_call_for_any_provider`, and reviewer check items (iv)/(v).

### Compatibility
Additive to Moira's schema (one new table plus a one-row scope backfill; no column changes) and to the console (opt-in providers and screens). No existing 07/08 behavior changes when the new settings are left unconfigured.

### Deployment
- **Migration ordering** — `migrations/0017_admin_invites.sql` (§0.5: `0015` is the highest shipped, plan 11 reserves `0016`). Re-verify by listing `migrations/` before cutting the branch; if another plan lands first, renumber.
- **D-1 shape drift** — if D-1 shipped a single-provider auth-settings shape, Wave 1 must add the multi-provider extension migration. Caught at the Wave 0 checkpoint.
- **Chart drift** — `charts/moira-console` gains only optional non-secret values; an upgrade that does not set them is safe (providers simply stay disabled), verified by the "boot with only Google configured" production-config test.
- **OpenAPI-drift gate ordering** — this plan changes the OpenAPI surface and must land before plan 05's gate freezes the spec, or regenerate the committed snapshot in the same PR (CONVENTIONS §1.6).

### Rollback
- **Console** — disable the new providers in Moira's auth settings; no data loss, existing admins unaffected, no redeploy needed (settings are runtime).
- **Moira** — the migration is additive-only (new table plus a one-row scope backfill), so a rollback that simply stops using the new endpoints leaves the schema harmlessly present. A full schema rollback is a **new forward migration** dropping `admin_invites` — migrations are append-only per `docs/project-structure.md` and 07's convention; never a down-migration, never an edit to a merged file.

### Deferred follow-ups (explicitly punted, not silently dropped)
- Console-sent invite emails (SMTP/email-provider integration) — copy-link-and-share-manually is this plan's behavior.
- Fine-grained / scoped-down admin roles (non-full-admin console users).
- Adopting the last-primary-style guard in 07's system-key deletion path (this plan establishes the pattern; applying it there is a follow-up).
- Additional i18n locales beyond English — the catalogs on both sides are structured for it; no second locale ships here.
- Generated Moira client types from the committed OpenAPI spec, once P1-10's gate exists.
- **Enterprise SAML SSO — permanently out of scope.** Better Auth does not provide it; mode 3 (bring-your-own JWT/JWKS behind the customer's own IdP or SSO gateway) is the supported path. Recorded as a limitation, not a roadmap item.

### Decisions still requiring product input (carried forward / new)
- **Single primary vs. multiple `moira:admins:manage` holders** — blocking at Wave 0; the transfer action's behavior differs materially (revoke-from-self vs. leave-both-granted).
- **Default invite expiry** — recommend ≤72h; final value is a product/ops call.
- **GitHub org-membership checking: required-on or optional-on** — recommend optional (not every self-host operator uses a GitHub org).
- **Which new admin screens are MVP-of-this-plan** — proposed set in the Summary; needs product sign-off.
