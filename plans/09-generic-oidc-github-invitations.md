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

## Summary

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
