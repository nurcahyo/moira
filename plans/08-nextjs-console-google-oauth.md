# Plan 08 — Next.js Admin Console (BFF): Setup Wizard, Better Auth & Google Sign-In

> **Compliance note.** This plan is written against `plans/CONVENTIONS.md` (verified 2026-07-25), which is authoritative and overrides any earlier draft of this file. The three substantive corrections CONVENTIONS forced into this revision are: (1) **Better Auth replaces Auth.js/NextAuth** as the console's identity layer (§7.4) and the hand-rolled JWT-minting/JWKS-exposure code the previous draft specified is **deleted** in favour of the `jwt` plugin; (2) **Atomic Design** (§6) replaces the previous ad-hoc `app/**/components/` layout — every concrete path below has been rewritten; (3) auth is **configured at runtime from Moira's DB-backed settings** (§7.2), not from build-time env, which changes the setup wizard's job and introduces a precisely-stated blocking dependency on plan 07 (see **Blocking dependency D-1**).

## Summary

**Objective.** Ship the first Moira admin console: a Next.js application acting as a Backend-For-Frontend (BFF) in front of Moira. It gives a human operator a browser-based way to (1) complete first-run setup by configuring the auth provider and claiming the initial admin identity, and (2) sign in thereafter to manage providers, credentials, models, routing, and view the audit log — all by calling Moira's *existing* (and plan-07-added) admin APIs. No Moira source in this repository is modified by this plan; it adds a **new**, separately deployed Next.js project plus deployment assets.

**Why ordered here.** Plan 01 §3/§4 makes this dependency explicit: "no Next.js line is written until the identity foundation (07) exists in Moira, because 'first login becomes admin' is unsafe." Iteration 07 adds the unauthenticated `GET /api/v1/admin/setup/claim-status` endpoint, the `admin_identities (issuer, subject)` grant table, and `POST /api/v1/admin/setup/claim`. Iteration 08 is the **first and only** iteration that creates the Next.js project — everything UI/OAuth-shaped before this plan is backend-only (02a, 02b, 03, 05, 07 — plan 02 is split per CONVENTIONS §0 D2 into **02a** honesty and **02b** idempotency replay). This plan binds to plan 07's **Frozen contract** table (`plans/07-identity-foundation.md` § Interfaces & Contracts) verbatim, not to the `plans/01` §4.5 sketch.

**Branch & PR (CONVENTIONS §1).** Branch `plan/08-nextjs-console-google-oauth`, cut from current `main`, stacked on `plan/07-identity-foundation` only if 07 has not merged (PR description must then name the base PR and the branch is rebased once 07 lands). Conventional Commits. One plan = one branch = one PR.

**Identity stack decision (CONVENTIONS §7.4, verified 2026-07-25).** The console uses **Better Auth**, not Auth.js/NextAuth. The Auth.js/NextAuth team joined Better Auth in September 2025; Auth.js is security-patch-only and Better Auth is the recommended choice for new projects. Concretely:

| Requirement | Better Auth mechanism (verified against better-auth.com docs, 2026-07-25) |
|---|---|
| Google sign-in | built-in social provider `socialProviders.google` (supports `clientId`, `clientSecret`, `prompt`, `accessType`, and an `hd` hosted-domain restriction) |
| Custom OAuth / generic OIDC | **`genericOAuth` plugin** (`import { genericOAuth } from "better-auth/plugins"`) — `config: [{ providerId, clientId, clientSecret, discoveryUrl, issuer, requireIssuerValidation, scopes, pkce, ... }]` |
| BFF→Moira short-lived JWT + JWKS | **`jwt` plugin** (`import { jwt } from "better-auth/plugins"`) — asymmetric signing plus a **published JWKS endpoint** whose path is customisable via `jwks.jwksPath` (documented example: `"/.well-known/jwks.json"`). Moira registers that URL as a `trusted_jwt_issuer`. |
| Sessions, CSRF, rate limiting | built in — `session.expiresIn`/`updateAge`, `trustedOrigins` origin validation (CSRF), `rateLimit.{enabled,window,max,storage,customRules}`, `advanced.{useSecureCookies,defaultCookieAttributes,cookies}` |
| Next.js App Router wiring | `toNextJsHandler(auth)` in `app/api/auth/[...all]/route.ts`; `auth.api.getSession({ headers: await headers() })` server-side; `nextCookies()` plugin last in the plugin array |

**What this deletes from the previous draft.** `console/lib/moira-jwt.ts` (hand-rolled RS256 minting) and `console/app/.well-known/jwks.json/route.ts` (hand-rolled JWKS document) are **removed as hand-written code**. Key generation, key storage, key rotation with `kid` overlap, and JWKS publication are library features of the `jwt` plugin (`jwks.keyPairConfig`, `jwks.rotationInterval`, `jwks.gracePeriod`, private key AES-256-GCM-encrypted at rest by default, `disablePrivateKeyEncryption` deliberately left at its secure default). What remains hand-written is a **thin, well-tested claims policy** (`console/lib/moira-token.ts`) that configures `jwt.definePayload` / `jwt.getSubject` / `jwt.issuer` / `jwt.audience` / `jwt.expirationTime` and is asserted by unit tests.

**Honest limitation (CONVENTIONS §7.4).** Better Auth does **not** provide enterprise SAML SSO, and does not act as a SAML SP against an external enterprise IdP. This plan makes **no SAML claim**. Operators who need SAML use **mode 3** — bring-your-own JWT via JWKS: they register their own IdP (or an SSO gateway that fronts SAML and emits OIDC/JWT) directly as a Moira `trusted_jwt_issuer` and skip the console's OAuth path entirely. That path needs no console and is unchanged by this plan.

**Architecture change forced by Better Auth: the console gains a small database.** Better Auth persists `user`, `session`, `account`, and `verification` records, and the `jwt` plugin persists its key material in a `jwks` table. The previous draft's "the console holds zero durable state" claim is therefore **no longer true and is corrected here**. The console gets a dedicated PostgreSQL schema (`console_auth`) — either in the existing Postgres instance or a separate one — managed by Better Auth's own CLI (`npx @better-auth/cli generate` / `migrate`), **never** mixed into Moira's `migrations/` directory. Moira remains the sole system of record for **authorization**; the console DB holds only human-session/account/JWKS rows and never any Moira credential, system key, or provider secret.

**User-visible outcome.** An operator deploys the Moira console container, opens it, is taken to a `/setup` wizard (because `claim-status` reports `claimed: false`), configures the auth provider **in the wizard — written into Moira's settings, not into a `.env` file**, signs in with Google (or a generic-OIDC provider), claims their identity as the first Moira admin, and is thereafter redirected to `/login` → sign-in → an authenticated admin console that reads/writes Moira's provider, credential, model, routing, and audit configuration through server actions calling the real Moira admin HTTP API. The browser never sees a Moira system key, the console's JWT-signing private key, an OAuth client secret, or a decrypted provider credential.

**Included scope.**
- New Next.js **16.2.11** App Router project (`console/`) on **Node 24 LTS** with **Bun 1.3.14** as package manager/script runner/unit-test runner (CONVENTIONS §5).
- **Atomic Design** layering (CONVENTIONS §6): `console/app/**` (pages) → `console/modules/<feature>/**` (organisms) → `console/components/molecules/**` → `console/components/atoms/**`, with a one-way dependency rule enforced by a test.
- Better Auth identity layer: Google social provider, `genericOAuth` plugin for OIDC, `jwt` plugin for the Moira-facing token + JWKS, `nextCookies()` for server-action cookie handling.
- Runtime auth configuration read from Moira's DB-backed auth settings and applied to a lazily-constructed Better Auth instance, with cache invalidation (CONVENTIONS §7.2).
- First-run setup wizard driving Moira's auth-settings write + `claim-status`/`claim` endpoints.
- MVP admin screens (see the flagged product decision below).
- i18n layer rendering Moira `message_key` with fallback to the server-supplied `message`, plus a console-originated key catalog (CONVENTIONS §4).
- `bun test` unit coverage for every lib module, atom, molecule, and organism; Playwright e2e for wizard, sign-in via **local mock OIDC**, sign-out, config round-trip, and an authorization-denial path; axe a11y on every page route; a secret-leak test (CONVENTIONS §3).
- New Dockerfile + Helm chart additions for the console as a second deployable.

**Excluded scope (explicitly deferred to plan 09 or later).**
- Generic-OIDC *hardening* beyond the `genericOAuth` baseline shipped here (multi-provider management UI, strict `requireIssuerValidation` policy surface), GitHub provider — all 09.
- Invitation flows / additional-admin self-service; ownership transfer; recovery beyond system-key break-glass — all 09.
- Enterprise SAML SSO — **not on any roadmap for the console**; mode 3 (bring-your-own JWKS) is the supported path, permanently.
- Conversation/memory/RAG configuration screens (P0-1/P0-3 — persistence primitives, not honest MVP features).
- Multi-replica session affinity (sessions are DB-backed in the console store, so horizontal scaling is already safe; no affinity work needed).

**Product-input decisions this plan flags (confirm before Wave 1):**
1. **MVP admin-screen list.** Proposed: setup wizard, dashboard/readiness, providers, provider models, provider credentials, routes, routing policies, applications, trusted JWT issuers, audit log. Deliberately excluded: system-keys, consumer-keys, agent-profiles, RAG/memory/conversation. **Needs product sign-off** — the exclusion of system-keys/consumer-keys management is a deliberate blast-radius decision, not an oversight.
2. **Signing algorithm.** Better Auth's `jwt` plugin defaults to `EdDSA`/`Ed25519`. This plan pins `jwks.keyPairConfig: { alg: "ES256" }` because Moira's per-issuer `allowed_algorithms` allow-list must accept it and ES256 is unambiguously supported by Moira's JWT verifier. **Wave 0 must verify** whether Moira accepts `EdDSA`; if it does, EdDSA is the better default and this pin should be revisited.
3. **Console database placement.** Same Postgres instance as Moira under a separate `console_auth` schema (simpler ops) vs. a separate database (stronger isolation). Recommend **separate schema, separate DB role with no grants on Moira's tables**; needs ops confirmation.
4. **Mode B (server-held system key) as an ongoing admin path.** Recommended: setup-time only. Running Mode B permanently is an explicit product/ops decision with coarser audit attribution, never a silent default.

---

## Findings Addressed

- **P1-11** (`plans/00-audit-report.md` — "Identity foundation absent — no owner/admin claiming, no user model... no safe basis for a Next.js admin console or OAuth login"): this plan is the console half of the fix; plan 07 is the backend half. Current behavior referenced by P1-11: **no UI/identity exists** — no `users` table, no session store, no OAuth client anywhere in `src/`. Verified by exhaustive grep in the audit (`migrations/0001-0008`, `src/`).
- **P0-3** (conversation/memory/RAG surface must be explicitly scoped before public exposure): the console MVP screen list deliberately **excludes** RAG/memory/conversation configuration UI so the console does not visually imply capabilities plan **02a** (the honesty half of the split — CONVENTIONS §0 D2) is simultaneously marking as preview/non-functional (`ingestion_status`, empty `citations`, no summarization). If plan **02a** has not yet landed the honest-status change when 08 starts, the console must still not build screens for these surfaces. Note also that per **D1** the `Idempotency-Key` parameter **stays** on Moira's conversation/memory/RAG routes (real replay lands in **02b**); no console text may describe it as removed or `501`-rejected.
- **P1-10** (no committed OpenAPI spec): the console's Moira client is hand-typed for MVP and switches to generated types once plan 05's committed-spec gate exists. Recorded as a dependency, not silently assumed.
- **P1-4** (audit-log cursor pagination correctness): the audit-log screen honestly renders "showing latest N" with no "next" control until plan 04 lands the cursor fix, rather than shipping a broken pager.
- `docs/todo.md` — no direct line items reference a Next.js console (confirmed absent); this plan and 07 are additive roadmap items, not corrections to an existing TODO.
- Referenced in `plans/01-roadmap-and-dependencies.md` §4 (identity architecture decision) and §4.5 — this plan implements rows: "Session management & logout" (08), "CSRF, PKCE, state, nonce, redirect validation" (08), "Secure server-side custody of Moira credentials" (08), "Browser vs BFF trust boundary" (08), "Verified email + allowed-email/domain policy... BFF enforcement" (08).

---

## Architecture

### Dependency D-1 — Moira-side auth settings (**RESOLVED: frozen in plan 07**)

CONVENTIONS §7.2 requires auth provider configuration to be **runtime configuration owned by Moira's database**, written by the setup wizard, read by the console at boot and on invalidation, with client secrets encrypted via Moira's existing `SecretCipher` and cache invalidation over the existing Postgres `LISTEN/NOTIFY` path (`src/infra/db.rs:43-80`).

**Status: plan 07 now provides this.** An earlier revision of this plan declared D-1 as a *blocking, unspecified* prerequisite because 07 placed the domain policy in static env config, defined no auth-settings resource, and stated "cache invalidation: none needed." **All three conflicts were resolved in 07's compliance pass**: the env-var domain allow-list (`MOIRA_AUTH__ADMIN_CLAIM_ALLOWED_EMAIL_DOMAINS`) was **withdrawn** in favour of DB-backed policy, migration `0010_auth_provider_settings.sql` adds the `auth_provider_settings` table, and invalidation reuses the existing `LISTEN/NOTIFY` channel. This plan now binds to 07's **frozen** names below — it no longer guesses them.

#### Frozen-contract change adopted (product-owner decisions D3/D4/D5, 2026-07-25)

Plan 07's Interfaces & Contracts section carries a **frozen-contract change callout** that this plan is bound to. Paths, methods (`google_oauth` | `generic_oidc` | `jwks`), and scopes (`moira:auth-settings:{read,write,delete}`) are **unchanged**. Exactly one DTO shape moved, and two policy facts are now load-bearing for this plan's wizard:

| # | Change (07, frozen) | What plan 08 does about it |
|---|---|---|
| **D5** | `ClaimAdminIdentityRequest.email`: `Option<String>` → **`String` (required)**; `email_verified` loses `#[serde(default)]` → **required**; `AdminIdentityRecord.email`: `Option<String>` → **`String`**. Required on **both** credential paths (system-key **and** setup-token). | The wizard sends **`email` and `email_verified` on every claim, including the system-key path**. `lib/types.ts` types both fields as non-optional; when the generated client from P1-10 replaces the hand-written types it **must be regenerated** against the new schema. **There is no optional-email path any more** — any text describing email as optional or omittable is deleted from this plan. |
| **D3** | Email/domain allow-list is **deny-by-default with no exemption and no bootstrap bypass** — 07 explicitly *removed* the system-key carve-out that earlier drafts assumed. Unconfigured or empty ⇒ deny. | **Wizard step order becomes load-bearing**: configure-and-**enable** an auth provider carrying a non-empty `allowed_email_domains` **before** the claim step. On a fresh deployment a claim attempted first **always** returns `403 admin_claim_domain_not_allowed`. The console renders that code as an **actionable setup instruction**, never a generic failure. |
| **D4** | `GET /api/v1/admin/setup/auth-methods` is **authenticated** (`SystemKey` \| `TrustedJwt` + `moira:setup:read`); unauthenticated calls get **401**; **there is no anonymous variant**. | Called **server-side from the BFF with the system key only**. No browser-side fetch, no client component, no route proxying it to the browser. **`GET /api/v1/admin/setup/claim-status` remains the ONLY anonymous Moira call this console makes** — that contrast is stated explicitly wherever either endpoint appears. |

Everything else 08 already bound to is unchanged and remains binding.

**Frozen contract this plan consumes** (source of truth: `plans/07-identity-foundation.md` § Interfaces & Contracts — re-verify at Wave 0, do not re-derive):

| Endpoint | Auth | Notes |
|---|---|---|
| `GET /api/v1/admin/setup/claim-status` | **none — the ONLY anonymous Moira call in this console** | 200 `{ "claimed": bool }`, shape frozen, no fields ever added |
| `GET /api/v1/admin/setup/auth-methods` | SystemKey \| TrustedJwt + `moira:setup:read` | `SetupAuthMethodsResponse { methods: [PublicAuthMethod] }`. **Authenticated** (anti-reconnaissance, D4) — the BFF calls it server-side with the system key, **never** from the browser; unauthenticated ⇒ 401; **no anonymous variant exists**. |
| `POST /api/v1/admin/setup/claim` | `X-Moira-System-Key` **or** `setup_token` in body | `ClaimAdminIdentityRequest { issuer, subject, email, email_verified, scopes?, setup_token? }` — **`email` and `email_verified` required on both paths (D5)**, `deny_unknown_fields`. 201 fresh / 200 replay → `AdminIdentityRecord` with `email: String`. |
| `GET /api/v1/admin/auth/providers` | `moira:auth-settings:read` | list, `params(PageQuery)` |
| `POST /api/v1/admin/auth/providers` | `moira:auth-settings:write` | opt. `Idempotency-Key` → 201 + `ETag` |
| `GET /api/v1/admin/auth/providers/{id}` | `moira:auth-settings:read` | 200 + `ETag` |
| `PATCH /api/v1/admin/auth/providers/{id}` | `moira:auth-settings:write` | **`If-Match` required** |
| `DELETE /api/v1/admin/auth/providers/{id}` | `moira:auth-settings:delete` | **`If-Match` required** → 204 |
| `POST /api/v1/admin/auth/providers/{id}/rotate-secret` | `moira:auth-settings:write` | **`If-Match` required** |
| `POST /api/v1/admin/auth/providers/{id}/{enable,disable}` | `moira:auth-settings:write` | **`If-Match` required** |

- **Table:** `auth_provider_settings` (migration `0010_auth_provider_settings.sql`).
- **Method discriminator — use 07's exact values:** `google_oauth` | `generic_oidc` | `jwks`. *(A previous draft of this plan guessed `google` / `byo_jwks`; those names are wrong and must not appear anywhere in the console.)*
- **New scopes:** `moira:auth-settings:{read,write,delete}` — the console's system-key actor must hold them.
- **Mode 3 (bring-your-own JWKS) is the pre-existing `/api/v1/admin/jwt-issuers` surface** — 07 invented nothing new for it, and the console reuses those endpoints rather than a parallel path.
- **Client secret is write-only.** 07 copies the `CredentialRecord` hiding pattern (`#[serde(skip_serializing)]` + `#[schema(ignore)]`) and exposes **no plaintext read-back endpoint** at all. The console therefore must **not** expect to read the secret back; see the corrected `loadAuthSettings()` contract in Detailed Implementation.
- **Secret rebinding hazard inherited from 07:** the secret's AAD binds `issuer`/`client_id`, so changing either invalidates it and returns a coded `409 auth_provider_secret_rebind_required`. The console's auth-settings form must surface this as a keyed error and prompt a secret re-entry rather than appearing to succeed.

**Wave 0 gate (retained, narrowed).** Plan 08 still does not begin Wave 1 until plan 07 is **merged**, because these endpoints must exist to be called. The interim env-var fallback behind `console/lib/auth-settings.ts::loadAuthSettings()` is retained **only** as a time-boxed escape hatch if the coordinator explicitly authorises starting Wave 1 against an unmerged-but-frozen 07; it **must not ship**, and a Definition-of-Done checkbox asserts the env path is removed and the Moira-backed path is live.

Everything else in this plan binds to 07's frozen contract as written, and needs no 07 change.

### Components & ownership

| Component | Owner | Lives in |
|---|---|---|
| Moira Rust API (unchanged by this plan) | existing team | `src/` (this repo) |
| `admin_identities`, `setup_state`, claim-status/claim endpoints | plan 07 (backend prerequisite, frozen) | `src/`, `migrations/` |
| DB-backed auth settings + `SecretCipher` client-secret storage + NOTIFY invalidation | **plan-07 amendment (D-1)** — not this plan | `src/`, `migrations/` |
| Next.js console (BFF) | this plan | new top-level directory `console/` in this repo |
| Better Auth configuration & runtime factory | this plan | `console/lib/auth.ts`, `console/lib/auth-settings.ts`, `console/lib/moira-token.ts` |
| Better Auth route handler + JWKS publication | this plan (thin wiring only) | `console/app/api/auth/[...all]/route.ts` |
| Console auth schema (`user`/`session`/`account`/`verification`/`jwks`) | this plan | `console/db/` (Better Auth CLI output; **never** `migrations/`) |
| Moira admin API client (BFF↔Moira) | this plan | `console/lib/moira-client.ts` |
| Console container image + Helm release | this plan | `console/Dockerfile`, `charts/moira-console/` |

Moira remains the **only** system of record for admin **authorization**, credentials, and runtime configuration. The console's own database holds human-session state and Better Auth key material only.

### Data flow

```
Browser (operator)
   │  1. GET / (no cookie)
   ▼
Next.js BFF (console) — page layer
   │  2. Server-side: GET Moira /api/v1/admin/setup/claim-status
   │     (plan-07 endpoint, UNAUTHENTICATED — the ONLY anonymous Moira call)
   ▼
Moira API
   │  3. { "claimed": false }   (the entire response — a single boolean, by design)
   ▼
BFF redirects to /setup (claimed == false)
   │  4. Server-side ONLY: GET Moira /api/v1/admin/setup/auth-methods with
   │     X-Moira-System-Key (D4: authenticated, moira:setup:read; 401 when
   │     unauthenticated; NO anonymous variant). The browser never issues this
   │     request and never receives its raw response.
   ▼
   │  5. Wizard step ORDER IS LOAD-BEARING (D3). Auth provider FIRST:
   │     operator enters auth-provider config (client id, discovery URL /
   │     issuer, allowed_email_domains — NON-EMPTY, client secret)
   │  6. Server action POSTs it to Moira's auth-settings endpoint with
   │     X-Moira-System-Key, then ENABLES the row. Moira encrypts the client
   │     secret with SecretCipher. The secret is NEVER stored in the console,
   │     never echoed back, and never re-rendered into the form.
   │     ── Until this step completes, step 10 CANNOT succeed: deny-by-default
   │        with no exemption and no bootstrap bypass means an unconfigured or
   │        empty allow-list denies EVERY claim, system-key path included. ──
   ▼
   │  7. Console invalidates its auth-settings cache and rebuilds the Better
   │     Auth instance from the newly-stored settings (runtime, no redeploy)
   │  8. Operator clicks "Sign in with Google" — Better Auth runs the OAuth
   │     flow (PKCE + state + nonce), verifies email, applies the `hd` /
   │     allowed-domain deny-by-default policy in a databaseHooks.user.create
   │     .before hook
   ▼
   │  9. Server action "Claim admin", two system-key-gated Moira calls, in order:
   ▼
Moira API — POST /api/v1/admin/jwt-issuers   (X-Moira-System-Key)
   │ 10. Registers the console as a trusted_jwt_issuer FIRST — plan 07's claim
   │     endpoint rejects (400 unregistered_trusted_issuer) any issuer not
   │     already a registered, active trusted_jwt_issuers row
   │     (07 module 3 `resolve_issuer_id`). jwks_url = the URL the Better Auth
   │     `jwt` plugin publishes (see Interfaces & Contracts).
   ▼
Moira API — POST /api/v1/admin/setup/claim   (X-Moira-System-Key, Idempotency-Key)
   │ 11. Body ClaimAdminIdentityRequest { issuer, subject, email,
   │     email_verified: true } — email AND email_verified are REQUIRED (D5)
   │     and are sent on EVERY claim, the system-key path included; there is
   │     no optional-email path → grants (issuer, subject) -> moira:admin,
   │     sets setup_state.claimed = true.
   │     If steps 5–6 were skipped: 403 admin_claim_domain_not_allowed, which
   │     the wizard renders as an actionable "add your email domain to the
   │     allow-list" instruction that routes back to the auth-provider step.
   ▼
Better Auth session cookie already set; BFF redirects to /
   │
   ▼
Subsequent admin actions: Browser → BFF (session cookie) → BFF asks the Better
Auth `jwt` plugin for a fresh short-lived token for the session → Moira admin API;
Moira fetches the console's JWKS, verifies, and resolves moira:admin via the
admin_identities grant union in src/security/auth.rs (07 module 4) — never from
any scope claim the console asserts (the console asserts none).
```

The **only** place a Moira system key is used is the setup-time triple (auth-settings write, issuer registration, claim) and, if Mode B is explicitly enabled in an environment, ongoing admin calls (documented fallback, coarser audit).

### Security boundaries — browser vs BFF vs Moira

- **Browser**: holds only the Better Auth session cookie (httpOnly, `Secure`, `SameSite=Lax`, signed via `BETTER_AUTH_SECRET`). Never receives: Moira system keys, the console's JWT-signing private key, Moira-audience JWTs, OAuth client secrets, or decrypted provider credentials. React Server Components and Server Actions run exclusively on the BFF; client components receive only display-safe, already-redacted data. **Per CONVENTIONS §6 rule 5, no secret is ever passed as a prop into an organism, molecule, or atom** — enforced by a test (see Verification).
- **BFF**: holds `BETTER_AUTH_SECRET`, the console DB connection string, and the Moira bootstrap system key (K8s Secret). OAuth client secrets live **in Moira**, encrypted; the console fetches them server-side at auth-instance construction and holds them only in process memory, never on disk, never in a cookie, never in a log. The `jwt` plugin's private key lives in the console DB, AES-256-GCM-encrypted at rest by Better Auth's default (`disablePrivateKeyEncryption` is deliberately **not** set).
- **Moira**: unchanged trust model — it authenticates the console's token exactly as any other `trusted_jwt_issuer` (`src/security/auth.rs::authenticate_trusted_jwt`), enforcing the per-issuer algorithm allow-list (`none`/`HS*` rejected per `docs/jwt-issuer-management.md`), audience/issuer/JWKS validation, and scope-based authorization (`src/security/authz.rs::ADMIN_SCOPE = "moira:admin"`).

### Atomic Design layering (CONVENTIONS §6, mandatory)

| Layer | Location | Responsibility | May import |
|---|---|---|---|
| **Pages** | `console/app/**/{page,layout,route,error,not-found}.tsx`, `app/**/actions.ts` | routing, auth gating, redirects, server-side data fetching, server actions | organisms, molecules, atoms, `lib/` |
| **Organisms (modules)** | `console/modules/<feature>/**` | feature-aware sections; may call the Moira client and server actions | molecules, atoms, `lib/` |
| **Molecules** | `console/components/molecules/**` | composite presentational components | atoms only |
| **Atoms** | `console/components/atoms/**` | primitives | nothing but React + styles |
| **Shared non-UI** | `console/lib/**` | Moira client, auth, i18n, policy, types | other `lib/` modules only |

**Enforced rules:** dependency direction is strictly one-way (pages → organisms → molecules → atoms); atoms and molecules are **feature-agnostic and presentational** — no Moira API calls, no `next/navigation` side effects, no auth logic, data and callbacks arrive via props; organisms own feature logic; pages stay thin; secrets never descend past the page/server boundary; shared non-UI logic lives in `lib/`, never in `components/`. Violations are a **test failure**, not a review opinion — see `console/tests/unit/architecture/layer-dependencies.test.ts`.

### DB/migration changes

**None to Moira's `migrations/` in this plan.** All identity/authorization state lives in Moira/Postgres and is owned by plan 07 (and D-1). The console owns a **separate** schema, generated and migrated by Better Auth's own CLI into `console/db/`, containing `user`, `session`, `account`, `verification`, and the `jwt` plugin's `jwks` table. This schema is deployed by a console-side job, is never referenced by Moira, and its role has no grants on Moira's tables.

### API & OpenAPI changes

**None to `src/http/`, `src/domain/`, or the generated OpenAPI document in this plan.** The console is a pure consumer of:
- `GET /api/v1/admin/setup/claim-status` (plan 07, **unauthenticated**) — returns exactly `{ "claimed": bool }` (`SetupClaimStatusResponse`). The **only** signal the `/setup` wizard branches on, and — stated explicitly for contrast with the row below — **the only anonymous Moira call this console makes anywhere**.
- `GET /api/v1/admin/setup/auth-methods` (plan 07, **authenticated**, D4) — `ActorType::SystemKey | TrustedJwt` + `moira:setup:read` → 200 `SetupAuthMethodsResponse { methods: [PublicAuthMethod] }`; unauthenticated ⇒ **401**. **There is no anonymous variant.** The BFF calls it **server-side with `X-Moira-System-Key`**; no browser-side `fetch`, no client component, and no console route that proxies its response to the browser. Asserted by `console/tests/unit/architecture/no-client-side-auth-methods.test.ts`.
- `GET /api/v1/admin/setup/status` (already implemented — `src/http/admin.rs:33-48`, `SetupStatusResponse`) — *structural* readiness, gated to system-key/trusted-JWT actors, carrying **no** admin-claimed field. Used post-auth for the dashboard readiness panel only. Plan 07 leaves it untouched; so does this plan.
- `POST /api/v1/admin/setup/claim` (plan 07) — auth: `X-Moira-System-Key` header **or** `setup_token` body field; a bare `Authorization: Bearer` JWT is rejected 401 unconditionally. Body: `ClaimAdminIdentityRequest { issuer, subject, email, email_verified, scopes?, setup_token? }` with `deny_unknown_fields` — **`email` (`String`) and `email_verified` (`bool`) are REQUIRED on both credential paths (D5)**; omitting either is rejected with the `ErrorResponse` envelope carrying `moira.error.invalid_request` (400 malformed / 422 schema-violating). Responses: 201 new / 200 idempotent replay (same `Idempotency-Key`) with `AdminIdentityRecord` whose `email` is a required `String`; 400 `unregistered_trusted_issuer`; **403 `admin_claim_domain_not_allowed` (deny-by-default, no exemption, no bootstrap bypass — D3)**; 409 `admin_identity_already_claimed`.
- `POST /api/v1/admin/jwt-issuers` + `GET`/`PATCH`/enable/disable/`refresh-jwks` (already implemented, `src/http/admin.rs`).
- Moira's auth-settings endpoints **once D-1 freezes them**.
- All existing admin CRUD endpoints in `src/http/mod.rs` for the MVP screens.

No new Moira endpoints are invented by this plan.

### Backward compatibility

Fully additive. Moira's existing machine-auth paths (system keys, consumer keys, non-console trusted JWT issuers) are untouched. Operators who never deploy the console continue to manage Moira via system keys / direct API calls exactly as today — and **mode 3** (bring-your-own JWT/JWKS) remains the console-free, OAuth-free, air-gap-compatible path, permanently.

### Deployment implications

- **New container**: `console/Dockerfile`, multi-stage build on **Node 24 LTS** (`node:24-slim`), Bun used for install/build inside the image, non-root user, healthcheck.
- **New Helm chart**: `charts/moira-console/` (sibling to `charts/moira/`) with `deployment.yaml`, `service.yaml`, `ingress.yaml`, `secret.yaml`, `configmap.yaml`, `serviceaccount.yaml`, `migration-job.yaml` (Better Auth schema), `hpa.yaml`.
- **Secrets** (K8s `Secret`, env-injected, never `NEXT_PUBLIC_*`, never in the image): `BETTER_AUTH_SECRET`, `CONSOLE_DATABASE_URL`, `MOIRA_SYSTEM_KEY`. **OAuth client secrets are not console secrets** — they live in Moira, encrypted (D-1).
- **Config** (ConfigMap): `MOIRA_BASE_URL`, `CONSOLE_BASE_URL`, `ALLOWED_CONSOLE_HOSTS`, `MOIRA_ADMIN_API_AUDIENCE`.
- **Network**: console → Moira over cluster-internal service DNS; the console's ingress is separate from Moira's public API ingress (`console.example.com` vs `api.example.com`).
- **Scaling**: sessions are DB-backed, so `replicaCount > 1` is safe with no affinity requirement.

### Failure & recovery

- **Claim attempted before an auth provider with a non-empty `allowed_email_domains` is enabled** (the fresh-deployment default): Moira returns **`403 admin_claim_domain_not_allowed`**. This is **expected behaviour, not a bug** (D3 — deny-by-default, no first-claim exemption, no bootstrap bypass). The wizard must render it as an **actionable setup instruction** — "add your email domain to the allow-list" — with a control that routes the operator back to the auth-provider step, its state preserved. It must never surface as a generic failure, a stack trace, or a "try again" toast. Covered by `console/tests/e2e/setup-wizard-ordering.spec.ts`.
- **Moira unreachable at setup time**: `/setup` shows a retry-with-backoff state; no partial claim is possible — the claim is a single atomic Moira admin command (`src/infra/repositories/admin.rs:560-726`), safe to retry with the same `Idempotency-Key`.
- **Auth settings unreadable from Moira at boot**: the console fails closed — `/login` renders a keyed "auth not configured" state (`console.error.auth_settings_unavailable`) and no sign-in button; it does **not** silently fall back to env.
- **OAuth failure / denied consent**: Better Auth error callback; no session created, no Moira call made.
- **Claim succeeds at Moira but session-cookie write fails**: idempotent retry — the operator re-authenticates, the BFF re-derives the same `(issuer, subject)`, re-attempts with the same idempotency key; Moira's ledger replays the prior success (no duplicate grant).
- **Signing-key rotation**: handled by the `jwt` plugin (`jwks.rotationInterval`, `jwks.gracePeriod`) — both keys are published in JWKS during the grace window, `kid`-differentiated. Moira's JWKS cache (300s TTL, `src/security/auth.rs::jwks`) picks up the new key on expiry; `POST /api/v1/admin/jwt-issuers/{id}/refresh-jwks` forces it immediately.
- **Console pod crash/restart**: sessions survive (DB-backed); no data loss.

---

## Detailed Implementation

### Toolchain pins (CONVENTIONS §5 — all verified 2026-07-25; do not change without re-verification)

| Tool | Pin | Enforcement |
|---|---|---|
| Next.js | **16.2.11** (latest stable, 2026-07-21; **16.3 is canary — do not use**) | exact pin `"next": "16.2.11"` in `console/package.json` |
| Node.js | **24.x Active LTS** (EOL 2028-04-30; Node 26 is *Current*, not LTS until Oct 2026; Node 22 is Maintenance-only) | `console/.nvmrc` → `24`; `package.json` `"engines": { "node": ">=24 <25" }`; `node:24-slim` in the Dockerfile |
| Bun | **1.3.14** (2026-05-13) | package manager + script runner + unit-test runner; `"packageManager": "bun@1.3.14"`; `oven/bun:1.3.14` build stage |
| React | as bundled with Next.js 16.2.11 | **not pinned independently** |
| Playwright | e2e runner | `bunx playwright test`; version pinned exactly in `package.json` |

- `bun install --frozen-lockfile` in CI; `console/bun.lock` is **committed**.
- Every dependency in `console/package.json` uses an **exact** version (no `^`, no `~`).
- Scripts: `bun run lint`, `bun run typecheck`, `bun test`, `bunx playwright test`, `bun run build`.

### Project layout (Atomic Design, mandatory)

```
console/
  app/                                     # ── PAGES (routing, auth gating, server fetching, server actions)
    layout.tsx
    error.tsx
    not-found.tsx
    page.tsx                               # root: redirects to /setup, /login, or /dashboard
    setup/
      layout.tsx
      page.tsx                             # server component; reads claim-status
      actions.ts                           # "use server": saveAuthSettings(), claimAdmin()
    login/
      page.tsx
    api/
      auth/[...all]/route.ts               # toNextJsHandler(await getAuth()) — Better Auth, incl. JWKS
      health/route.ts
    (console)/
      layout.tsx                           # session guard + nav shell
      dashboard/page.tsx
      providers/page.tsx
      providers/[id]/page.tsx
      providers/actions.ts
      provider-models/page.tsx
      provider-models/actions.ts
      credentials/page.tsx
      credentials/actions.ts
      routes/page.tsx
      routes/actions.ts
      routing-policies/page.tsx
      routing-policies/actions.ts
      applications/page.tsx
      applications/actions.ts
      jwt-issuers/page.tsx
      jwt-issuers/actions.ts
      audit-log/page.tsx
      settings/auth/page.tsx               # read + edit auth settings post-setup (secret write-only)
      settings/auth/actions.ts

  modules/                                 # ── ORGANISMS (feature-aware UI modules)
    setup/
      SetupWizard.tsx
      WelcomeStep.tsx
      AuthSettingsStep.tsx                 # writes auth config into Moira
      SignInClaimStep.tsx
      DoneStep.tsx
    auth/
      SignInPanel.tsx                      # renders one button per enabled auth method
      SignOutButton.tsx
    shell/
      ConsoleNav.tsx
      SessionBanner.tsx
    dashboard/
      ReadinessPanel.tsx
    providers/
      ProviderTable.tsx
      ProviderForm.tsx
    providerModels/
      ProviderModelTable.tsx
      ProviderModelForm.tsx
    credentials/
      CredentialTable.tsx
      CredentialForm.tsx                   # never renders a plaintext secret
      CredentialRotatePanel.tsx
    routing/
      RouteTable.tsx
      RouteForm.tsx
      RoutingPolicyTable.tsx
      RoutingPolicyForm.tsx
    applications/
      ApplicationTable.tsx
      ApplicationForm.tsx
    jwtIssuers/
      JwtIssuerTable.tsx                   # console's own row flagged read-only
    audit/
      AuditLogPanel.tsx
    authSettings/
      AuthSettingsForm.tsx

  components/
    molecules/                             # ── MOLECULES (composite, presentational, feature-agnostic)
      FormField.tsx
      TableRow.tsx
      DataTable.tsx
      ConfirmDialog.tsx
      StatusBadgeGroup.tsx
      Pagination.tsx
      EmptyState.tsx
      ErrorBanner.tsx
      Toast.tsx
      OnceOnlySecretModal.tsx
      MaskedValue.tsx
    atoms/                                 # ── ATOMS (primitives)
      Button.tsx
      Input.tsx
      Textarea.tsx
      Select.tsx
      Checkbox.tsx
      Label.tsx
      Badge.tsx
      Spinner.tsx
      Icon.tsx
      Heading.tsx
      Text.tsx
      VisuallyHidden.tsx

  lib/                                     # ── SHARED NON-UI
    auth.ts                                # getAuth(): lazily-built, cached Better Auth instance
    auth-settings.ts                       # loadAuthSettings() from Moira + cache + invalidation
    moira-token.ts                         # jwt-plugin claims policy (issuer/audience/subject/definePayload)
    moira-client.ts                        # typed fetch wrapper for the Moira admin API
    session.ts                             # server-only session helpers
    domain-policy.ts                       # deny-by-default email/domain policy
    errors.ts                              # Moira ErrorResponse → client-safe discriminated union
    types.ts                               # DTOs mirrored from src/domain/*.rs
    env.server.ts                          # server-only env accessor (uses `server-only`)
    i18n/
      index.ts                             # t(key, args, fallback)
      catalog.en.ts                        # console.* keys + English defaults
      moira-keys.ts                        # mirrored moira.error.* / moira.notice.* keys

  db/                                      # Better Auth CLI schema output (NOT Moira migrations/)
  tests/                                   # see Tests below
  middleware.ts
  next.config.ts
  package.json                             # exact pins, engines, packageManager
  bun.lock                                 # committed
  .nvmrc                                   # 24
  tsconfig.json
  playwright.config.ts
  Dockerfile
  README.md
  .env.example                             # server-only vars; none prefixed NEXT_PUBLIC_
```

### `console/lib/auth-settings.ts` — runtime auth configuration (CONVENTIONS §7.2)

- `loadAuthSettings(): Promise<AuthSettings>` — server-only. Calls `GET /api/v1/admin/auth/providers` with `X-Moira-System-Key`, returning the **non-secret** config only. **Per 07's frozen contract the `client_secret` is write-only: accepted on create and `rotate-secret` only, never returned in any response, and no read-back endpoint exists at all** (`AuthProviderSettingsRecord` exposes only `secret_fingerprint` and `masked_secret`). This plan therefore **must not** assume it can read the secret back, and **must not** invent an endpoint that returns it.
  - **Open item, blocking at Wave 0 (coordinator):** Better Auth's `socialProviders.google` / `genericOAuth` need the plaintext client secret in console process memory to perform the OAuth code exchange. With 07 frozen as write-only, the console needs one of: (a) 07 adds a narrowly-scoped, system-key-only, cluster-internal secret-read path in a **follow-up** plan-07 amendment; or (b) the operator supplies the same client secret to the console once as a deployment secret while Moira retains the encrypted copy of record; or (c) the OAuth code exchange is proxied through Moira. **This plan picks none of these unilaterally** — it is a coordinator decision, and Wave 1 does not start without it. Whatever is chosen, the secret never reaches the browser, `NEXT_PUBLIC_*`, or any client bundle.
- In-process cache keyed by the settings `version`, with two invalidation triggers: (a) a short TTL (default 60s) and (b) an explicit `invalidateAuthSettings()` called by the wizard's `saveAuthSettings()` server action immediately after a successful write, so a settings change takes effect **without a redeploy**.
- `AuthSettings` shape consumed by the console: `{ version, methods: { google?: { clientId, clientSecret, hostedDomain? }, genericOidc?: { providerId, clientId, clientSecret, discoveryUrl, issuer } }, allowedEmailDomains: string[], allowedAlgorithms: string[], jwksUrl?: string }`.
- **Deny-by-default, with NO exemption (D3)**: an empty or unconfigured `allowedEmailDomains` means *nobody* may sign in **and no claim can succeed** — **including the first claim and including the system-key path**. Never "empty means allow all," and **never a first-claim exemption or bootstrap bypass**: plan 07 deliberately *removed* the system-key carve-out an earlier draft assumed, so the console must not reintroduce one client-side either. The console's own `domain-policy.ts` gate is defence-in-depth **in front of** Moira's authoritative check, never a substitute for it and never more permissive than it.

### `console/lib/auth.ts` — Better Auth instance (verified API shapes)

```ts
import "server-only";
import { betterAuth } from "better-auth";
import { jwt, genericOAuth } from "better-auth/plugins";
import { nextCookies } from "better-auth/next-js";
```

- **Lazy async factory, not a module-level constant.** `export async function getAuth(): Promise<Auth>` awaits `loadAuthSettings()`, builds `betterAuth({...})`, and caches the instance keyed by the settings `version`; a version change rebuilds it. This is what makes §7.2 ("configured in settings at runtime") mechanically true. Every consumer — the route handler, server components, and server actions — calls `await getAuth()`.
- `database` — the console's PostgreSQL store (`CONSOLE_DATABASE_URL`). Required: Better Auth persists `user`/`session`/`account`/`verification`, and the `jwt` plugin persists `jwks`.
- `baseURL: CONSOLE_BASE_URL`, `basePath: "/api/auth"` (documented default; keep it).
- `secret` from `BETTER_AUTH_SECRET` (32+ bytes, K8s Secret, rotated via Better Auth's versioned `secrets` support).
- `trustedOrigins: [CONSOLE_BASE_URL]` — this is Better Auth's CSRF mechanism (origin validation + Fetch Metadata). **`advanced.disableCSRFCheck` must never be set** (a lint rule and a unit test assert its absence).
- `session: { expiresIn: 28800 /* 8h */, updateAge: 3600, cookieCache: { enabled: true, maxAge: 60 } }`.
- `advanced: { useSecureCookies: true /* prod */, defaultCookieAttributes: { httpOnly: true, secure: true, sameSite: "lax", path: "/" } }`.
- `rateLimit: { enabled: true, window: 10, max: 100, storage: "database", customRules: { "/sign-in/social": { window: 60, max: 10 }, "/oauth2/callback/*": { window: 60, max: 20 } } }`.
- `socialProviders.google` — built from settings: `{ clientId, clientSecret, prompt: "select_account", hd: settings.methods.google.hostedDomain }`. The `hd` option restricts sign-in to a Google Workspace domain and rejects tokens with no `hd` claim when set; it is a **defence-in-depth complement to**, not a replacement for, `lib/domain-policy.ts`.
- `plugins`, in order:
  1. `genericOAuth({ config: [{ providerId: "moira-oidc", clientId, clientSecret, discoveryUrl, issuer, requireIssuerValidation: true, pkce: true, scopes: ["openid", "email", "profile"], mapProfileToUser }] })` — present only when generic-OIDC is enabled in settings. `discoveryUrl` gives OIDC auto-discovery; `issuer` + `requireIssuerValidation: true` gives strict issuer validation (the plugin's default for `requireIssuerValidation` is `false` — **this plan sets it true explicitly**). Callback path is `${baseURL}/api/auth/oauth2/callback/:providerId`, registered exactly (no wildcards) at the IdP.
  2. `jwt({ ... })` — see `moira-token.ts`.
  3. `nextCookies()` — **must be last** so server actions can set cookies.
- `user.additionalFields: { idpIssuer: { type: "string" }, idpSubject: { type: "string" } }` — populated by each provider's `mapProfileToUser` from the IdP's stable `sub`/`iss`. See the subject-binding note below; this is load-bearing.
- `databaseHooks.user.create.before` — the deny-by-default gate. Rejects with `throw new APIError("FORBIDDEN", { message: ... })` (import `APIError` from `"better-auth/api"`) when: `emailVerified !== true`, or the email's domain is not in `settings.allowedEmailDomains`, or `idpSubject` is absent. Returning `false` from a before-hook also aborts; this plan throws `APIError` so the failure carries a message key. Re-validated **again** server-side in `claimAdmin()` — never trust a value checked once at sign-in for a later privileged action.

### `console/lib/moira-token.ts` — the `jwt` plugin claims policy

Configures the `jwt` plugin. This is the **only** hand-written JWT logic remaining; signing, key storage, rotation, and JWKS publication are the plugin's job.

```ts
jwt({
  jwks: {
    jwksPath: "/.well-known/jwks.json",          // documented custom-path option
    keyPairConfig: { alg: "ES256" },             // see product decision 2
    rotationInterval: 60 * 60 * 24 * 30,
    gracePeriod:      60 * 60 * 24 * 30,
    // disablePrivateKeyEncryption intentionally NOT set — the private key
    // stays AES-256-GCM encrypted at rest (Better Auth default).
  },
  jwt: {
    issuer:   env.MOIRA_BFF_ISSUER_URL,          // e.g. https://console.example.com
    audience: env.MOIRA_ADMIN_API_AUDIENCE,      // must be non-empty; see below
    expirationTime: "120s",
    getSubject: (session) => session.user.idpSubject,
    definePayload: ({ user }) => ({}),           // SEE THE SECURITY RULE BELOW
  },
})
```

**Security rule (non-negotiable, CONVENTIONS §7.5 — preserved verbatim from the previous draft because it is the single most important invariant in this plan):**

> **`definePayload` must never emit a `scope` (or `scp`) claim.** Moira's `actor_from_trusted_claims` (`src/security/auth.rs:555-628`) copies scopes **straight out of the JWT's scope claim**. If the console asserted `scope: "moira:admin"`, Moira would honour it for *any* subject the console signs — making the console's own token logic the sole authorization gate and rendering plan 07's `admin_identities` grant table decorative. By emitting **no** scope claim, the actor's scopes come **only** from 07 module 4's grant union: a granted `(issuer, subject)` resolves to `moira:admin`; an ungranted one resolves to zero scopes and every admin call fails 403 at Moira regardless of any console bug. **Moira stays the system of record for authorization; the console only for authentication.** Asserted by `console/tests/unit/lib/moira-token.test.ts` and again end-to-end by `console/tests/e2e/authorization-denial.spec.ts`.

**No `email`/`email_verified` claims either.** Moira's trusted-JWT verification does not read them (verified: `actor_from_trusted_claims` extracts subject/tenant/application/roles/scopes claims only). Email is consumed exactly once, at claim time, from the `POST /api/v1/admin/setup/claim` request **body**, where plan 07 stores it as an `admin_identities` attribute column. Putting PII in every minted token adds leak surface for zero function.

**`getSubject` is load-bearing and must be set explicitly.** The `jwt` plugin's default subject is the Better Auth **`session.user.id`** — a surrogate key local to the *console's* database. Binding Moira's `admin_identities (issuer, subject)` grant to a console-DB-local id would mean a console database restore, re-seed, or migration silently invalidates every admin grant (or, worse, re-points one at a different human). This plan therefore sets `getSubject: (session) => session.user.idpSubject`, where `idpSubject` is the **IdP's stable `sub`** captured at account creation via `mapProfileToUser` — satisfying CONVENTIONS §7.5 ("identity binds to stable `(issuer, subject)` — never to email alone") and matching what plan 07's claim endpoint stores. `console/tests/unit/lib/moira-token.test.ts` asserts the subject is the IdP `sub` and **not** the Better Auth user id.

**Audience must be non-empty.** Moira skips audience validation entirely when a trusted issuer's `expected_audiences` is empty (`validation.validate_aud = false`, `src/security/auth.rs:327-328`). Registering a non-empty audience is therefore **mandatory** for this issuer, and the production-config test asserts it on both sides.

### `console/app/api/auth/[...all]/route.ts` — Better Auth handler (thin wiring)

```ts
import { toNextJsHandler } from "better-auth/next-js";
import { getAuth } from "@/lib/auth";

const handler = async (req: Request) => {
  const { GET, POST } = toNextJsHandler(await getAuth());
  return req.method === "GET" ? GET(req) : POST(req);
};
export { handler as GET, handler as POST };
```

This single catch-all serves sign-in, callbacks, sign-out, the token endpoint, **and the JWKS document** at the `jwks.jwksPath` the plugin is configured with. **No hand-written JWKS route exists.** The concrete JWKS URL that Moira must be registered with is resolved and asserted at Wave 0 by fetching it (the plugin serves JWKS under its base path by default and `jwks.jwksPath` overrides it) — the console registers **the URL it actually serves**, verified by `console/tests/e2e/jwks.spec.ts`, never a hard-coded guess.

### `console/lib/moira-client.ts`

- `import "server-only"` at the top — a **build failure** if a client component imports it.
- Thin typed wrapper over `fetch` for every admin endpoint the MVP screens use; hand-written types mirroring `src/domain/admin.rs` (`ApplicationRecord`, `ProviderRecord`, `ProviderModelRecord`, `CredentialRecord`, `RouteDefinitionRecord`, `RoutingPolicyRecord`, `TrustedJwtIssuerRecord`, `AuditLogRecord`, `SetupStatusResponse`) and `src/domain/identity.rs` (`SetupClaimStatusResponse`, `ClaimAdminIdentityRequest`, `AdminIdentityRecord`, `SetupAuthMethodsResponse`, `PublicAuthMethod`), switching to generated types once P1-10's committed spec exists.
  - **TypeScript shapes bound to 07's changed DTO (D5) — both fields are NON-OPTIONAL:**
    ```ts
    // console/lib/types.ts — mirrors src/domain/identity.rs after 07's frozen-contract change
    export interface ClaimAdminIdentityRequest {
      issuer: string;
      subject: string;
      email: string;          // REQUIRED — was `string | undefined` in the pre-D5 draft
      email_verified: boolean; // REQUIRED — no default, no `?`
      scopes?: string[];       // omitted → Moira defaults to ["moira:admin"]
      setup_token?: string;
    }
    export interface AdminIdentityRecord {
      // …
      email: string;           // REQUIRED — was `string | null`; a grant cannot exist without an email
    }
    ```
    No `?`, no `| null`, and no `| undefined` on `email`/`email_verified` in either shape. TypeScript's own checker is therefore the first line of enforcement: a call site that omits either field fails `bun run typecheck`.
  - **Generated-client note (P1-10):** when the hand-written types are replaced by a client generated from the committed OpenAPI spec, **the client must be regenerated against the post-D5 schema**. A client generated before this change types `email` as optional and would silently permit an omitting call site; regeneration — not hand-patching — is the required action, and the regenerated output must be re-checked against this section.
- Every call passes, as applicable:
  - `Authorization: Bearer <token from the jwt plugin>` (Mode A, default) — obtained per-request, never cached beyond a single request.
  - `X-Moira-System-Key: <bootstrap key>` — **only** for the setup-time triple (auth-settings write, `POST /api/v1/admin/jwt-issuers`, `POST /api/v1/admin/setup/claim`), and for all admin calls **only** when `MOIRA_ADMIN_MODE=system_key` is explicitly set. The pre-claim `GET /api/v1/admin/setup/claim-status` read is unauthenticated.
  - `Idempotency-Key: <server-generated UUIDv4 per logical operation>` on every mutating call — one key per **form submission**, not per HTTP retry, so retries replay rather than duplicate.
  - `If-Match: <resource_version>` on every PATCH/PUT **and on rotate calls** — verified against current source: `require_if_match` is enforced on `rotate_credential` (`src/http/admin.rs`, commit `0688e2e`) as well as the standard PATCH handlers; Moira returns 400 "If-Match header is required" when absent. The client tracks the version from the prior read and never fabricates one.
- Maps `ErrorResponse` into a client-safe discriminated union via `lib/errors.ts` — **never** forwarding the raw envelope across the server/client boundary. `message_key` and `message_args` cross; `message` crosses as the i18n fallback; `details` does not.

### `console/lib/i18n/` — message rendering (CONVENTIONS §4.6)

- `t(key: string, args?: Record<string, string|number>, fallback?: string): string`. Resolution order: **console catalog (`catalog.en.ts`) → server-supplied `message` (the fallback argument) → the key itself**. This is exactly §4.6: "the console renders `message_key` through its own i18n layer and falls back to the server-supplied `message`."
- **Server-originated conditions** (every Moira error and notice): the UI calls `t(err.messageKey, err.messageArgs, err.message)`. **No component may hardcode English copy for a server-originated condition** — `ErrorBanner` and `Toast` accept `{ messageKey, messageArgs, message }`, never a pre-formatted string.
- **Console-originated strings** (labels, button text, wizard copy, validation hints): every one has a `console.*` key with an English default in `catalog.en.ts` (e.g. `console.setup.welcome.title`, `console.error.auth_settings_unavailable`, `console.credentials.reveal_once_warning`). Components call `t("console...")`; **bare literals in JSX are a test failure**.
- `moira-keys.ts` mirrors the Moira key namespaces (`moira.error.*`, `moira.notice.*`) the console renders, kept in sync with `docs/i18n-response-catalog.json`. A test asserts every mirrored key exists in that JSON, so a Moira-side rename surfaces as a console test failure rather than an untranslated string in production.
- **`moira.error.admin_claim_domain_not_allowed` is an ACTIONABLE SETUP INSTRUCTION, not a generic failure (D3).** It is the *expected* response on a fresh deployment when the claim is attempted before an enabled auth provider carries the operator's email domain, so it is the one server-originated condition the wizard treats as guidance. Handling, in order:
  1. It is **not** routed to the generic `ErrorBanner` failure path and **never** rendered as "something went wrong", a stack trace, or a retry toast.
  2. The console catalog supplies an override rendered *alongside* the server `message`: `console.setup.domain_not_allowed.title` ("Add your email domain to the allow-list"), `console.setup.domain_not_allowed.body` (explains deny-by-default: an unconfigured or empty allow-list denies **every** claim, including this first one, and that this is intended behaviour rather than a bug), and `console.setup.domain_not_allowed.action` ("Edit allowed email domains").
  3. The action control returns the operator to `AuthSettingsStep` with its state preserved and the allowed-email-domains field focused; the offending domain is offered as a pre-filled suggestion via `message_args` (structured data — never pre-formatted English prose).
  4. Per §4.6 the server-supplied `message` remains the fallback if the console key is missing, so the operator never sees a bare key. `moira.error.admin_claim_domain_not_allowed` is listed in `moira-keys.ts` and must exist in `docs/i18n-response-catalog.json`.
  
  The same treatment applies wherever the code can surface post-setup (e.g. `/settings/auth`), so the guidance is not wizard-only. Asserted by `console/tests/e2e/setup-wizard-ordering.spec.ts` and `console/tests/unit/modules/setup/SetupWizard.test.tsx`.
- Structure is locale-ready (`catalog.en.ts` is the first of N); shipping additional locales is out of scope for MVP but requires no refactor.

### Setup wizard (`console/app/setup/**` + `console/modules/setup/**`)

#### Step order is load-bearing (D3) — do not reorder

On a **fresh deployment** the `auth_provider_settings` table is empty, so `allowed_email_domains` is unconfigured. Because plan 07's domain policy is **deny-by-default with no first-claim exemption and no bootstrap bypass**, a claim attempted before an auth provider is configured **and enabled** with a non-empty `allowed_email_domains` **always** returns `403 admin_claim_domain_not_allowed` — on the system-key path too. The auth-provider step therefore **must** precede the claim step; the wizard enforces this as navigation state, not as advice.

| # | Screen (organism) | Route/step | Moira call(s) | Auth used | Gate to advance |
|---|---|---|---|---|---|
| 1 | `WelcomeStep` | `/setup` | `GET .../setup/claim-status` | **none — the only anonymous call** | `claimed === false` (else redirect `/login`) |
| — | *(page-layer preload, no screen)* | `/setup` | `GET .../setup/status`, **`GET .../setup/auth-methods`** | `X-Moira-System-Key`, **server-side only (D4)** | system key present, Moira reachable |
| 2 | **`AuthSettingsStep`** | `/setup` | `POST` + `enable` on `/api/v1/admin/auth/providers` | `X-Moira-System-Key` | **provider row saved AND enabled AND `allowed_email_domains` non-empty** — the wizard blocks step 3 until Moira confirms all three |
| 3 | `SignInClaimStep` | `/setup` | Better Auth OAuth (no Moira call) | session | verified email whose domain is in the allow-list |
| 4 | *(claim action)* | `/setup` | `POST .../jwt-issuers`, then `POST .../setup/claim` | `X-Moira-System-Key` + `Idempotency-Key` | 201/200 from claim |
| 5 | `DoneStep` | `/setup` | — | — | → `/dashboard` |

Step 2 **cannot** be skipped, deferred, or reordered behind step 3/4. If an operator reaches the claim with step 2 incomplete — e.g. by deep-linking, by a restored draft, or because the provider row exists but is **disabled** — the resulting `403 admin_claim_domain_not_allowed` is rendered as an **actionable setup instruction** that returns them to step 2 with its state preserved (see the i18n handling below).

1. **`app/setup/page.tsx`** (page layer, server component): calls `moiraClient.getSetupClaimStatus()` — unauthenticated `GET /api/v1/admin/setup/claim-status`, **the only anonymous Moira call in the console**. If `claimed === true`, redirect to `/login`. Then, **server-side only**, calls the authenticated `GET /api/v1/admin/setup/status` **and `GET /api/v1/admin/setup/auth-methods`** with `X-Moira-System-Key` (D4: `moira:setup:read`; unauthenticated ⇒ 401; **no anonymous variant exists**). The `auth-methods` result is reduced to a display-safe view model **on the server** and passed down as props; the raw response never crosses to the browser and no client component ever fetches this path. If the root system key is missing, render a blocking keyed state (`console.setup.backend_not_ready`) — the console **cannot** bootstrap Moira's root system key; that remains the operator CLI step (`bootstrap-system-key`, `src/main.rs`), documented in the wizard copy, never automated. Renders `<SetupWizard />`.
2. **`modules/setup/WelcomeStep.tsx`** — explains what claiming does, that it happens exactly once, **and that the auth provider must be configured first because the domain allow-list denies by default**.
3. **`modules/setup/AuthSettingsStep.tsx`** — the CONVENTIONS §7.2 step, and **a hard prerequisite of the claim (D3)**. Collects: auth method (`google_oauth` / `generic_oidc`), client id, discovery URL or issuer, hosted domain (Google), allowed email domains (**deny-by-default; the form refuses to submit an empty list with an explicit keyed warning, and the copy states that an empty list denies every claim including the operator's own first claim**), and the client secret. Submits to `app/setup/actions.ts::saveAuthSettings()`, which POSTs to Moira's auth-settings endpoint with `X-Moira-System-Key` — **Moira encrypts the client secret with `SecretCipher`** — and then calls the **`enable`** endpoint, because 07 creates rows `enabled: false` and a **disabled** row does not govern the issuer, so the claim would still 403. The secret is write-only: never returned by any read, never re-rendered into the form (the field shows a `MaskedValue` "configured" state on revisit), never passed as a prop into any organism/molecule/atom. On success the action calls `invalidateAuthSettings()` so the next `getAuth()` rebuilds from the new config with no redeploy, and unlocks the sign-in/claim step.
4. **`modules/setup/SignInClaimStep.tsx`** — "Sign in to become the first admin." **Rendered only once step 3 reports an enabled provider with a non-empty allow-list.** Renders one button per enabled method, driving Better Auth's sign-in. Tagged with an intent marker (a signed, short-lived httpOnly cookie set when the step renders) so the post-callback landing routes into the claim action rather than a normal login.
5. **`app/setup/actions.ts::claimAdmin()`** (`"use server"`):
   - Reads the session via `(await getAuth()).api.getSession({ headers: await headers() })`; re-verifies `emailVerified === true` and re-runs `domain-policy.ts` (defence in depth — never more permissive than Moira's authoritative check).
   - **First** registers the console's own issuer: `GET /api/v1/admin/jwt-issuers` pre-check, then `POST /api/v1/admin/jwt-issuers` with `X-Moira-System-Key` — body `{ issuer: MOIRA_BFF_ISSUER_URL, jwks_url: <the URL the jwt plugin actually serves>, expected_audiences: [MOIRA_ADMIN_API_AUDIENCE], allowed_algorithms: ["ES256"], subject_claim: "sub" }` (fields match `TrustedJwtIssuerCreateRequest`, `src/domain/admin.rs:564`, `deny_unknown_fields` — send nothing extra; **`scopes_claim` is deliberately omitted** because the console mints no scope claim). Registration **must precede** the claim: plan 07's claim endpoint rejects an unregistered issuer with 400 `unregistered_trusted_issuer` (07 module 3 `resolve_issuer_id`).
   - **Then** `POST /api/v1/admin/setup/claim` with `X-Moira-System-Key`, body `{ issuer: MOIRA_BFF_ISSUER_URL, subject: <idpSubject>, email: <verified email>, email_verified: true }` (exact `ClaimAdminIdentityRequest`; `scopes` omitted → 07 defaults to `["moira:admin"]`), `Idempotency-Key` derived deterministically from `(issuer, idpSubject)` so a double-submit replays (200) rather than duplicating.
     > **`email` and `email_verified` are sent on EVERY claim, unconditionally, including the system-key path (D5).** Both are required by 07's DTO and there is **no optional-email path** — the action must never construct a body that omits either, and must never branch on credential type to decide whether to include them. Omitting either yields `moira.error.invalid_request` (400/422) from axum's extractor before the service is reached. Asserted by `console/tests/unit/lib/moira-client.test.ts`.
   - **Handles `403 admin_claim_domain_not_allowed` as an actionable setup step, not a failure** — see the i18n handling below; the action returns the operator to `AuthSettingsStep` rather than dead-ending.
   - Redirects to `DoneStep` → `/dashboard` under a normal authenticated session (issuer registered, grant present, JWKS fetchable — Mode A works immediately).
6. **`modules/setup/DoneStep.tsx`** — confirms the grant (rendering `AdminIdentityRecord.email`, a required `String`) and links to the dashboard.

### MVP admin screens

Pages under `app/(console)/` stay thin (fetch + guard + render); all rendering lives in `modules/`.

| Screen | Page | Organism(s) | Moira endpoints |
|---|---|---|---|
| Dashboard | `app/(console)/dashboard/page.tsx` | `modules/dashboard/ReadinessPanel` | `GET .../setup/status` + `GET .../setup/claim-status` |
| Providers | `app/(console)/providers/page.tsx`, `[id]/page.tsx` | `modules/providers/{ProviderTable,ProviderForm}` | `GET/POST /providers`, `GET/PATCH/DELETE .../{id}`, enable/disable |
| Provider models | `app/(console)/provider-models/page.tsx` | `modules/providerModels/*` | `GET/POST /providers/{id}/models`, `GET/PATCH/DELETE /provider-models/{id}` |
| Credentials | `app/(console)/credentials/page.tsx` | `modules/credentials/{CredentialTable,CredentialForm,CredentialRotatePanel}` | `GET/POST /provider-credentials`, rotate/enable/disable |
| Routes | `app/(console)/routes/page.tsx` | `modules/routing/{RouteTable,RouteForm}` | `GET/POST /routes`, `GET/PATCH/DELETE .../{id}` |
| Routing policies | `app/(console)/routing-policies/page.tsx` | `modules/routing/{RoutingPolicyTable,RoutingPolicyForm}` | `GET/POST /routing-policies`, `GET/PATCH/DELETE .../{id}` |
| Applications | `app/(console)/applications/page.tsx` | `modules/applications/*` | `GET/POST /applications`, `GET/PATCH/DELETE .../{id}`, execution-policy |
| Trusted JWT issuers | `app/(console)/jwt-issuers/page.tsx` | `modules/jwtIssuers/JwtIssuerTable` | `GET/POST /jwt-issuers`, `GET/PATCH/DELETE .../{id}`, enable/disable, refresh-jwks |
| Audit log | `app/(console)/audit-log/page.tsx` | `modules/audit/AuditLogPanel` | `GET /audit-events`, `GET .../{id}` |
| Auth settings | `app/(console)/settings/auth/page.tsx` | `modules/authSettings/AuthSettingsForm` | Moira auth-settings endpoints (D-1) |

Notes carried forward: **credentials never render a plaintext secret** — the once-only creation response appears in `OnceOnlySecretModal` ("copy now, will not be shown again"), mirroring Moira's `ApiKeySecretResponse` contract, and the value never becomes a prop on any reusable molecule/atom beyond that modal's own render. The **console's own issuer row** is flagged read-only in `JwtIssuerTable` with a typed-issuer-name confirm guard on disable, so an operator cannot casually disable their own login path. The **audit log** shows "showing latest N, no further pages" until P1-4's cursor fix lands in plan 04, rather than a broken "next" control.

**Explicitly excluded from MVP:** system-keys and consumer-keys management (break-glass/bootstrap mechanisms kept CLI/API-only so the console never becomes a vector for over-privileged self-service key minting), agent-profiles, RAG collections/documents, conversation/memory policy screens (P0-3).

### `console/middleware.ts`

- Enforces `ALLOWED_CONSOLE_HOSTS` (comma-separated exact hostnames, **no wildcards**) against the `Host` header before any auth processing, closing host-header-injection open-redirect risk.
- Sets on every response: `Strict-Transport-Security`, `X-Content-Type-Options: nosniff`, `Content-Security-Policy: frame-ancestors 'none'` (plus `X-Frame-Options: DENY`), `Referrer-Policy: strict-origin-when-cross-origin`, and a `Content-Security-Policy` with no `unsafe-inline` for scripts.
- Redirects unauthenticated requests to `/(console)/**` → `/login`, and redirects everything → `/setup` while `claim-status` reports `claimed: false` (server-side call, briefly cached to avoid a Moira round-trip per request).

### Tests (exact file names)

**Unit — `bun test` (CONVENTIONS §3).**

*lib:*
- `console/tests/unit/lib/auth-settings.test.ts` — Moira fetch shape, version-keyed cache, TTL expiry, `invalidateAuthSettings()` forces a reload, fail-closed when Moira is unreachable, **empty `allowedEmailDomains` denies everyone**.
- `console/tests/unit/lib/auth-config.test.ts` — the object handed to `betterAuth()`: `trustedOrigins` set; `advanced.disableCSRFCheck` **absent**; `useSecureCookies` true in prod; cookie attributes `httpOnly`/`secure`/`sameSite=lax`; `rateLimit.enabled` true; `nextCookies()` is the **last** plugin; `genericOAuth` carries `requireIssuerValidation: true` and `pkce: true`; a settings `version` change yields a rebuilt instance.
- `console/tests/unit/lib/moira-token.test.ts` — **the security invariant suite**: `definePayload` output contains **no `scope` and no `scp`**; no `email`/`email_verified`; `getSubject` returns the IdP `sub` and **not** the Better Auth `user.id`; `issuer`/`audience` non-empty and matching env; `expirationTime` ≤ 120s; `keyPairConfig.alg` is in Moira's registered `allowed_algorithms`; `disablePrivateKeyEncryption` is not set.
- `console/tests/unit/lib/domain-policy.test.ts` — deny-by-default, exact-email match, domain match, case-insensitivity, sub-domain non-match, unicode/IDN normalisation.
- `console/tests/unit/lib/moira-client.test.ts` — `Idempotency-Key` present on every mutation and stable per submission; `If-Match` present on PATCH/PUT **and rotate**; `Authorization` vs `X-Moira-System-Key` selection per mode; the unauthenticated claim-status read sends no credential. **Named tests for the D4/D5 propagation:**
  - `claim_request_always_sends_email_and_email_verified` — every claim body built by the client carries **both** fields, on the **system-key path and the setup-token path alike**; no code path produces a body omitting either.
  - `claim_request_has_no_optional_email_branch` — the builder exposes no flag, overload, or credential-type branch that makes `email` omittable (the pre-D5 optional path is gone, not merely unused).
  - `auth_methods_read_sends_system_key` — `GET .../setup/auth-methods` always attaches `X-Moira-System-Key` and is never issued credential-free (D4: an anonymous call would 401, and there is no anonymous variant to fall back to).
  - `claim_status_is_the_only_anonymous_call` — enumerating every method on the client, exactly one (`getSetupClaimStatus`) sends no credential; every other Moira call attaches either `X-Moira-System-Key` or `Authorization: Bearer`.
- `console/tests/unit/lib/errors.test.ts` — `ErrorResponse` → client-safe union; `details` never crosses the boundary; 401/403 map to the sign-out-and-redirect outcome.
- `console/tests/unit/lib/session.test.ts` — server-only session read, no token ever returned to callers.
- `console/tests/unit/lib/i18n.test.ts` — `t()` resolves catalog first, falls back to the server `message` for an unknown `message_key`, falls back to the key when both are absent, interpolates `message_args` as structured data (never pre-formatted prose).
- `console/tests/unit/lib/i18n-catalog-coverage.test.ts` — every `console.*` key referenced in `app/`, `modules/`, and `components/` exists in `catalog.en.ts` with a non-empty English default; every key mirrored in `moira-keys.ts` exists in `docs/i18n-response-catalog.json`.

*atoms (one per atom, CONVENTIONS §6 rule 6):* `console/tests/unit/atoms/{Button,Input,Textarea,Select,Checkbox,Label,Badge,Spinner,Icon,Heading,Text,VisuallyHidden}.test.tsx` — render, prop pass-through, disabled/loading states, accessible name, keyboard focus.

*molecules (one per molecule):* `console/tests/unit/molecules/{FormField,TableRow,DataTable,ConfirmDialog,StatusBadgeGroup,Pagination,EmptyState,ErrorBanner,Toast,OnceOnlySecretModal,MaskedValue}.test.tsx` — composition, callback wiring, error/empty states. `ErrorBanner`/`Toast` additionally assert they render `messageKey` through `t()` and fall back to `message`. `MaskedValue`/`OnceOnlySecretModal` assert the raw value never appears in a `title`/`aria-label`/`data-*` attribute.

*organisms:* `console/tests/unit/modules/setup/SetupWizard.test.tsx`, `.../setup/AuthSettingsStep.test.tsx` (client-secret field is write-only; empty allowed-domains blocks submit), `.../auth/SignInPanel.test.tsx` (renders exactly the enabled methods; renders the keyed not-configured state when none), `.../providers/ProviderTable.test.tsx`, `.../credentials/CredentialForm.test.tsx` (**never renders a plaintext secret**), `.../audit/AuditLogPanel.test.tsx`, `.../dashboard/ReadinessPanel.test.tsx`, `.../jwtIssuers/JwtIssuerTable.test.tsx` (console's own row read-only).

*architecture:*
- `console/tests/unit/architecture/layer-dependencies.test.ts` — static import-graph scan asserting the one-way rule: atoms import no molecule/organism/`lib`; molecules import only atoms; organisms import no page; nothing under `components/` imports `lib/moira-client` or `lib/auth`; nothing outside `app/`/`modules/` imports `next/navigation`.
- `console/tests/unit/architecture/server-only-guards.test.ts` — `lib/{auth,auth-settings,moira-client,moira-token,session,env.server}.ts` each begin with `import "server-only"`.
- `console/tests/unit/architecture/no-secret-props.test.ts` — no component prop name matches `/(secret|systemKey|privateKey|clientSecret|apiKey|token|password)/i` anywhere under `modules/` or `components/` (CONVENTIONS §6 rule 5, mechanically enforced).
- `console/tests/unit/architecture/no-hardcoded-copy.test.tsx` — JSX text nodes under `modules/` and `components/` are either `t(...)` calls or props; bare English literals fail.

**E2E — Playwright (`bunx playwright test`), against a running console + a real test-fixture Moira + a local mock OIDC provider.**
- `console/tests/fixtures/mock-oidc/server.ts` — the local mock OIDC provider (discovery document, JWKS, authorize/token/userinfo). **Real Google is never used in CI**, per CONVENTIONS §3.
- `console/tests/e2e/setup-wizard.spec.ts` — fresh Moira (bootstrap system key only) → `/setup` → auth-settings step writes config into Moira → mock-OIDC sign-in → claim succeeds → dashboard. Direct API assertions (not through the UI): `claim-status` flips `false`→`true`, and a **scope-free** console token for the claimed `(issuer, subject)` authorizes `GET /api/v1/admin/setup/status` — proving the 07 grant union, not a minted scope, is what authorizes.
- `console/tests/e2e/google-signin.spec.ts` — post-setup sign-in through the mock OIDC provider standing in for Google, including the `hd`/allowed-domain accept and reject cases; asserts session cookie attributes via `page.context().cookies()`.
- `console/tests/e2e/sign-out.spec.ts` — authenticated session → sign out → `/(console)/**` redirects to `/login` → session cookie cleared and the server-side session row is gone.
- `console/tests/e2e/config-round-trip.spec.ts` — create a provider via the UI → assert it appears in the UI list → assert via a direct Moira API read that it exists with the submitted fields → PATCH via the UI → assert a concurrent direct-API patch surfaces an `If-Match` conflict as a keyed toast rather than a silent overwrite.
- `console/tests/e2e/auth-settings-round-trip.spec.ts` — change allowed domains + client id via `/settings/auth` → assert Moira stores them → assert the console applies them **without a restart** → assert the client secret is never returned by any read and never appears in the rendered HTML.
- `console/tests/e2e/authorization-denial.spec.ts` — a second identity signs in successfully (authentication OK) but has **no** `admin_identities` grant; every admin screen and every server action fails with Moira's 403, the UI renders the keyed denial state, and a direct API check confirms no mutation occurred. Explicitly asserts the console cannot self-grant.
- `console/tests/e2e/jwks.spec.ts` — the JWKS URL the console registers with Moira is the URL it actually serves; the document is valid JWKS JSON, contains **public key material only** (no `d` parameter, no PEM private header), and every published key has a `kid`.
- `console/tests/e2e/i18n-message-key.spec.ts` — force a Moira error with a known `message_key`; assert the console renders the catalog string; then force an **unknown** `message_key` and assert it renders the server-supplied `message` verbatim (never a hardcoded English string, never the raw key).
- `console/tests/e2e/a11y.spec.ts` — `@axe-core/playwright` on **every page route**: `/`, `/setup` (each step), `/login`, `/dashboard`, `/providers`, `/providers/[id]`, `/provider-models`, `/credentials`, `/routes`, `/routing-policies`, `/applications`, `/jwt-issuers`, `/audit-log`, `/settings/auth`. Zero critical/serious violations gates CI.
- `console/tests/e2e/secret-leak.spec.ts` — a network/console tap across the full authenticated journey asserting no browser-observed response body, no rendered HTML, and no `console.log` ever contains the system key fixture, the OAuth client secret fixture, a PEM header, or a decrypted provider credential — except the one intentional once-only reveal, which is additionally asserted never to be logged or cached.

**Secret-leak — build-time.**
- `console/tests/secret-leak/bundle-scan.test.ts` — after `bun run build`, scan `.next/static/**/*.js`, `.next/server/**/*.html`, and all SSR-emitted HTML from the e2e run for: the bootstrap system key fixture value, the OAuth client secret fixture value, `-----BEGIN` (any PEM header), and any env var name matching `/^NEXT_PUBLIC_.*?(SECRET|KEY|TOKEN|PASSWORD)/i`. **Asserts the violation set is empty**, not merely "this run's fixture wasn't found," so the gate catches future regressions before a real secret exists to grep for.

### Documentation

- `console/README.md` — deployment, the pinned toolchain, required env vars (each marked server-only; none are public), the Atomic Design layering rules, Mode A vs Mode B, and the "auth is configured in the wizard, not in `.env`" model.
- `docs/admin-console.md` (new, Moira repo root docs) — the exact Moira endpoints the console calls, the `trusted_jwt_issuer` registration shape, the no-scope-claim invariant and why it exists, the deny-by-default domain policy, **and an explicit statement that SAML SSO is not supported and that mode 3 (bring-your-own JWKS) is the path for it**. Links to `docs/jwt-issuer-management.md` and `docs/public-authentication.md`.

### Deployment assets

- `console/Dockerfile`: multi-stage (`deps` → `builder` → `runner`); `oven/bun:1.3.14` for install/build, `node:24-slim` runtime; `bun install --frozen-lockfile`; non-root user; `HEALTHCHECK` hitting `app/api/health/route.ts`, which checks Moira reachability **without leaking Moira response bodies**.
- `charts/moira-console/`: `Chart.yaml`, `values.yaml` (secrets referenced by name not value; `moiraBaseUrl`, `consoleBaseUrl`, `allowedConsoleHosts`, `replicaCount`), `templates/{deployment,service,ingress,secret,configmap,serviceaccount,migration-job,hpa}.yaml` mirroring `charts/moira/templates/`. `migration-job.yaml` runs the Better Auth schema migration against `CONSOLE_DATABASE_URL` as a pre-install/pre-upgrade hook.

---

## Multi-Agent Workflow

### Waves (disjoint file ownership; parallelizable within a wave, sequential across)

**Wave 0 — Coordinator checkpoint (sequential, blocking).**
- Confirm plan 07's shipped contract matches its Frozen-contract table (`GET .../claim-status` → `{ "claimed": bool }`; `POST .../claim` + `ClaimAdminIdentityRequest`/`AdminIdentityRecord`; the issuer-must-be-preregistered guard).
- **Confirm or unblock D-1** (Moira DB-backed auth settings). Wave 1 does not start without D-1 merged or signed off as a frozen RFC.
- **Verify Moira's accepted JWT algorithms** and fix the `keyPairConfig.alg` pin (product decision 2).
- **Resolve the concrete JWKS URL** the Better Auth `jwt` plugin serves under the chosen `jwksPath`, by running it — the registered `jwks_url` is the observed URL, never a guess.
- Confirm the MVP admin-screen list and the console-database placement (product decisions 1 and 3).

**Wave 1 — Scaffolding, toolchain, design system (parallel, disjoint).**
- *Frontend engineer A*: `console/package.json` (exact pins, `engines`, `packageManager`), `bun.lock`, `.nvmrc`, `next.config.ts`, `tsconfig.json`, `playwright.config.ts`, `app/layout.tsx`, `app/error.tsx`, `app/not-found.tsx`.
- *Design-system engineer*: **all** of `console/components/atoms/**` and `console/components/molecules/**` plus their unit tests. Presentational only — this track never touches `lib/` or `modules/`, which is what makes it fully parallel and keeps the layering honest by construction.
- *Backend-integration engineer*: `console/lib/{types,moira-client,errors,env.server}.ts` + tests. Publishes `lib/types.ts` first (~30 min head start) so the other tracks compile against it.
- *i18n engineer*: `console/lib/i18n/**` + `i18n.test.ts`, `i18n-catalog-coverage.test.ts`.
- *Security engineer*: `console/middleware.ts` — **single owner across all waves** (this file is touched again in Wave 2; one owner end-to-end avoids the merge race).

**Wave 2 — Better Auth + runtime settings + setup wizard (parallel, disjoint).**
- *Security/OAuth engineer*: `console/lib/{auth,auth-settings,moira-token,domain-policy,session}.ts`, `console/app/api/auth/[...all]/route.ts`, `console/db/**` (Better Auth CLI schema), `console/app/login/page.tsx`, `console/modules/auth/**`, plus `middleware.ts`'s auth-redirect completion. Owns every security-invariant unit test.
- *Frontend engineer A*: `console/modules/setup/**`, `console/app/setup/{layout,page,actions}.ts(x)`.
- *Frontend engineer B*: `console/app/(console)/layout.tsx`, `console/modules/shell/**`, `console/app/(console)/dashboard/page.tsx`, `console/modules/dashboard/**`.
- No overlap: `lib/` + `app/api/auth/**` (security), `app/setup/**` + `modules/setup/**` (A), `app/(console)/layout.tsx` + `modules/shell,dashboard/**` (B).

**Wave 3 — Admin CRUD screens (parallel, disjoint — one engineer per resource family; each owns both its `app/(console)/<resource>/**` page+actions and its `modules/<feature>/**` organisms).**
- *Frontend engineer A*: `providers/` + `modules/providers/**`; `provider-models/` + `modules/providerModels/**`.
- *Frontend engineer B*: `credentials/` + `modules/credentials/**`; `routes/` + `routing-policies/` + `modules/routing/**`.
- *Frontend engineer C*: `applications/` + `modules/applications/**`; `jwt-issuers/` + `modules/jwtIssuers/**`; `audit-log/` + `modules/audit/**`.
- *Security/OAuth engineer*: `app/(console)/settings/auth/**` + `modules/authSettings/**` (kept with the auth owner because it writes secrets into Moira).
- Zero cross-directory writes — these four run fully in parallel.

**Wave 4 — Deployment, e2e, hardening (parallel, disjoint).**
- *DevOps engineer*: `console/Dockerfile`, `charts/moira-console/**`, CI workflow additions for the §2 frontend gates.
- *Test engineer*: `console/tests/e2e/**` and `console/tests/fixtures/mock-oidc/**`.
- *Security engineer*: `console/tests/secret-leak/**`, `console/tests/unit/architecture/**`, CSP finalization.
- *Docs engineer*: `console/README.md`, `docs/admin-console.md`.

**Read-only reviewers (every wave).** A security reviewer re-reads `lib/auth.ts`, `lib/auth-settings.ts`, `lib/moira-token.ts`, `middleware.ts`, and every `actions.ts` diff for: (a) any `NEXT_PUBLIC_` prefix near a secret-shaped name; (b) any server action missing a session/authorization re-check; (c) any client component importing `moira-client.ts`/`auth.ts` (a build failure via `server-only`, but reviewed anyway); (d) **any change that would introduce a `scope` claim**; (e) any `advanced.disableCSRFCheck`. Findings go back to the owning engineer; this reviewer writes no code.

**Conflict avoidance.** Every wave's file list above has zero intra-wave path overlaps. `middleware.ts` and `lib/types.ts` are the only files touched in more than one wave, and each has a **single designated owner across all waves** (security engineer and backend-integration engineer respectively).

### Pull request (CONVENTIONS §1.4)

One PR against `main` from `plan/08-nextjs-console-google-oauth`, opened only after every §2 gate passes locally, with the required sections: **Plan link** (`plans/08-nextjs-console-google-oauth.md`) · **Findings addressed** (P1-11, P0-3, P1-10, P1-4) · **Migrations included** (none in `migrations/`; console-side Better Auth schema in `console/db/`) · **Breaking API/OpenAPI changes** (none) · **Test evidence** (`bun test` + `bunx playwright test` summaries) · **Rollback procedure** · **Deferred follow-ups**.

---

## Interfaces & Contracts

### BFF↔Moira endpoints and headers

| Call | Headers | Notes |
|---|---|---|
| `GET /api/v1/admin/setup/claim-status` | none (unauthenticated) | plan 07 frozen; response is exactly `{ "claimed": bool }` — the wizard's only branch signal |
| `GET /api/v1/admin/setup/status` | `X-Moira-System-Key` (setup-time) or `Authorization: Bearer` (post-claim) | pre-existing, unchanged by 07 and by this plan; structural readiness only |
| `POST /api/v1/admin/setup/claim` | `X-Moira-System-Key`, `Idempotency-Key` | plan 07 frozen; body `ClaimAdminIdentityRequest`; 201 new / 200 replay / 400 `unregistered_trusted_issuer` / 403 `admin_claim_domain_not_allowed` / 409 `admin_identity_already_claimed`; bare Bearer JWT rejected 401 |
| `POST /api/v1/admin/jwt-issuers` | `X-Moira-System-Key`, `Idempotency-Key` | existing; called once, **before** the claim |
| `GET /api/v1/admin/jwt-issuers` | `X-Moira-System-Key` (setup-time) | existing; already-registered pre-check |
| Moira auth-settings read/write | `X-Moira-System-Key`, `Idempotency-Key` + `If-Match` on write | **D-1 — paths/shapes owned and frozen by plan 07's amendment**; this plan binds to them, does not name them |
| All other admin CRUD | `Authorization: Bearer <jwt-plugin token>`, `Idempotency-Key` (mutations), `If-Match` (PATCH/PUT and rotate) | existing, per `src/http/mod.rs` |

### Exact JWT claims Moira expects from the console-minted token

Per `src/security/auth.rs::authenticate_trusted_jwt` / `actor_from_trusted_claims` and the `trusted_jwt_issuers` row the console self-registers:

- `iss` — exact string match against the registered `issuer` column (`jwt.issuer` = `MOIRA_BFF_ISSUER_URL`).
- `kid` (JOSE header) — **required** by Moira (`authenticate_trusted_jwt` rejects tokens without it) and must match a key in the JWKS the `jwt` plugin publishes. Asserted by `jwks.spec.ts` and by the round-trip in `setup-wizard.spec.ts`.
- `alg` (JOSE header) — must be in the registered `allowed_algorithms`; this plan registers `["ES256"]` and pins `jwks.keyPairConfig: { alg: "ES256" }` to match. `none`/`HS*` are rejected by Moira regardless (`docs/jwt-issuer-management.md`).
- `aud` — must match the registered `expected_audiences`; set to a single value, `MOIRA_ADMIN_API_AUDIENCE`. **Non-empty is mandatory**: Moira skips audience validation entirely when `expected_audiences` is empty (`validation.validate_aud = false`, `src/security/auth.rs:327-328`).
- `sub` — the registered `subject_claim` (default `"sub"`), carrying the **IdP's stable subject** via `jwt.getSubject`, not the Better Auth user id and not email.
- **No `scope`/`scp` claim** — authorization comes exclusively from plan 07's `admin_identities` grant union. Moira reads scope claims when present, so *not* minting one is a security property, asserted by `moira-token.test.ts` and `authorization-denial.spec.ts`.
- **No `email`/`email_verified` claims** — Moira's trusted-JWT path does not consume them; email lives only in the one-time claim request body and the `admin_identities.email` column.
- `iat`/`exp` — `jwt.expirationTime: "120s"`, so `exp - iat ≤ 120`. Moira's registered `clock_skew_seconds` tolerates minor drift.
- `jti` — unique per token. Moira does **not** track `jti` (no replay ledger for trusted JWTs); the short `exp` is the actual replay control. `jti` is for console-side log correlation only, not claimed as a Moira-enforced property.

### Scopes/authz

The claimed `(issuer, subject)` is granted `moira:admin` at claim time (plan 07's default), matching `ADMIN_SCOPE` in `src/security/authz.rs`. MVP does not support scoped-down admin roles from the console — that is 09+.

### Error handling & i18n

- Moira's `ErrorResponse { error: { code, message_key, message, message_args, request_id, details } }` (`src/error.rs:52-65`) is mapped by `lib/errors.ts` into a client-safe discriminated union carrying `{ code, messageKey, message, messageArgs }`. `details` and `request_id` stay server-side (`request_id` is logged, not rendered). The UI renders `t(messageKey, messageArgs, message)` — catalog first, server `message` as fallback (CONVENTIONS §4.6).
- `401`/`403` from Moira on an authenticated console session (stale/revoked grant) → the BFF clears the session and redirects to `/login` with a **keyed** flash (`console.notice.session_revoked`), never a raw error dump.
- `409 idempotency_conflict` / `idempotency_in_progress` on a form submit → keyed message `console.error.change_in_progress`, submit disabled until the in-flight request resolves; **no client-side idempotency-key regeneration on retry-click**, matching Moira's replay semantics.
- Every console-originated string has a `console.*` key with an English default in `catalog.en.ts`.

### Session cookie attributes

Better Auth default session cookie, configured via `advanced.defaultCookieAttributes`: `httpOnly: true`, `secure: true` (prod; `false` only for local `http://localhost`), `sameSite: "lax"`, `path: "/"`, `session.expiresIn: 28800` (8h) with `updateAge: 3600` sliding refresh, signed with `BETTER_AUTH_SECRET` (32+ bytes, K8s Secret, rotated via Better Auth's versioned `secrets` support). Sessions are DB-backed in the console store, so sign-out is a genuine server-side revocation, not merely a cookie clear.

### CSRF / PKCE / state / nonce / redirect validation

- **CSRF** — Better Auth's `trustedOrigins` origin validation plus Fetch Metadata checks. `advanced.disableCSRFCheck` is **never** set (unit-tested). Next.js Server Actions are POST-only and same-origin by framework default; every action additionally re-reads and re-authorizes the session server-side rather than trusting any client-supplied identity.
- **PKCE** — enabled for OIDC (`pkce: true` on the `genericOAuth` config; standard for the social providers). `code_challenge_method: S256`.
- **State / nonce** — handled by Better Auth's OAuth flow; mismatch is a hard failure with no session created.
- **Redirect validation** — each callback URL is registered **exactly** (no wildcards, no path-prefix matching) at the IdP: `/api/auth/callback/google` for the social provider and `/api/auth/oauth2/callback/moira-oidc` for the generic-OIDC provider. `middleware.ts` rejects any request whose `Host` is not in `ALLOWED_CONSOLE_HOSTS` before Better Auth processes it.

### Logout

Better Auth's sign-out deletes the server-side session row **and** clears the cookie. No Moira-side call is needed — Moira never held a session, and outstanding minted tokens expire in ≤120s. `sign-out.spec.ts` asserts both the cookie clear and the session-row deletion.

---

## Verification

### Gates (CONVENTIONS §2 — frontend)

```bash
bun install --frozen-lockfile
bun run lint
bun run typecheck
bun test                # unit
bunx playwright test    # e2e
bun run build
```

### Unit
`console/tests/unit/**` exactly as enumerated in Detailed Implementation § Tests — lib modules, **every atom**, **every molecule**, every organism, and the four architecture guards. Run with `bun test`.

### Browser end-to-end (Playwright)
`console/tests/e2e/**` exactly as enumerated — `setup-wizard.spec.ts`, `google-signin.spec.ts` (local mock OIDC, **never real Google in CI**), `sign-out.spec.ts`, `config-round-trip.spec.ts`, `auth-settings-round-trip.spec.ts`, `authorization-denial.spec.ts`, `jwks.spec.ts`, `i18n-message-key.spec.ts` — against a running console + a real test-fixture Moira instance.

### Accessibility
`console/tests/e2e/a11y.spec.ts` with `@axe-core/playwright` on **every page-level route** (list in Detailed Implementation). Zero critical/serious violations gates CI.

### Secret-leak
`console/tests/secret-leak/bundle-scan.test.ts` (build output + SSR HTML, asserting an **empty** violation set) and `console/tests/e2e/secret-leak.spec.ts` (browser-observed responses, rendered HTML, and console output). Together these prove no Moira system key, admin key, OAuth client secret, JWT private key, or decrypted provider credential reaches any client bundle, HTML payload, or browser-visible response.

### Production-config tests
A CI job builds the console with `NODE_ENV=production` and a minimal-but-complete fixture, boots it, and asserts: security headers present; `/setup` reachable without a session; `/(console)/**` redirects to `/login` without a session; the JWKS endpoint returns valid JWKS JSON with **no private-key material**; the registered `expected_audiences` is non-empty on both sides; `middleware.ts` rejects an unlisted `Host`.

### Helm / Kubernetes validation
`helm lint charts/moira-console` and `helm template charts/moira-console | kubeconform` (mirroring the existing `charts/moira` gate in `.github/workflows/ci.yml`), plus a rendered-manifest assertion that no secret value appears in a `ConfigMap` (only in `Secret`), and that `readOnlyRootFilesystem: true`, `runAsNonRoot: true`, and dropped capabilities match the `charts/moira` hardening baseline.

### Rust gates (regression check only — this plan makes no Rust changes)
```bash
cargo fmt --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo build --release --locked
```
Plus clean PostgreSQL migration validation (unaffected — this plan adds no Moira migrations).

---

## Definition of Done

**CONVENTIONS §8 compliance checklist**
- [ ] Work performed on branch `plan/08-nextjs-console-google-oauth`; PR opened with all required description sections (§1.4).
- [ ] All §2 frontend gates pass: `bun install --frozen-lockfile`, `bun run lint`, `bun run typecheck`, `bun test`, `bunx playwright test`, `bun run build`. Rust gates green as a regression check.
- [ ] **Unit tests** delivered and passing — including one test per **atom** and per **molecule**, and at least one per organism.
- [ ] **E2E tests** delivered and passing — Playwright, covering setup wizard, sign-in via **local mock OIDC**, sign-out, a config round-trip, and an **authorization-denial** path.
- [ ] Every console-originated string has an i18n **key + English default** in `catalog.en.ts`; every Moira key the console renders exists in `docs/i18n-response-catalog.json`; `i18n-catalog-coverage.test.ts` and `no-hardcoded-copy.test.tsx` pass.
- [ ] **Next.js 16.2.11 · Node 24 LTS · Bun 1.3.14** pinned exactly; `bun.lock` committed; `.nvmrc`/`engines` present; **Atomic Design layering respected with the one-way dependency rule**, proven by `layer-dependencies.test.ts`.
- [ ] Auth config is **runtime/DB-backed via Moira** (D-1 live, env fallback **removed**); the OAuth client secret is encrypted server-side by Moira's `SecretCipher` and never returned to the browser; **no `scope` claim in minted JWTs**; domain policy is **deny-by-default**.
- [ ] **No secret-leak**, verified by `bundle-scan.test.ts` (empty violation set) and `secret-leak.spec.ts`.

**Plan-specific**
- [ ] Fresh-instance E2E: the `/setup` wizard writes auth settings into Moira, then claims the first admin via plan 07's `POST /api/v1/admin/setup/claim` (issuer self-registration first), and `claim-status` flips `false`→`true`.
- [ ] A settings change made in `/settings/auth` takes effect **without a redeploy** (`auth-settings-round-trip.spec.ts`).
- [ ] The console's registered `jwks_url` is the URL the Better Auth `jwt` plugin actually serves, and the published document contains public key material only (`jwks.spec.ts`).
- [ ] `getSubject` binds to the **IdP subject**, not the Better Auth `user.id` (`moira-token.test.ts`).
- [ ] All MVP admin screens perform at least one create/read/update round-trip against real Moira admin endpoints in e2e, with `Idempotency-Key` and `If-Match` correctly sent.
- [ ] Accessibility gate clean on **every** page route.
- [ ] `helm lint` + `kubeconform` clean for `charts/moira-console`.
- [ ] `docs/admin-console.md` documents the BFF↔Moira contract, Mode A vs Mode B, deny-by-default domain policy, and states plainly that **SAML SSO is not supported** (mode 3 is the path).
- [ ] No Moira Rust source under `src/` or file under `migrations/` is modified by this plan (verified by `git diff --stat`: the PR touches only `console/`, `charts/moira-console/`, `docs/admin-console.md`, and CI workflow files).

---

## Risks & Rollback

### Security
- **Token custody.** The highest-risk assets are the `jwt` plugin's private key (console DB, AES-256-GCM-encrypted at rest by Better Auth default — `disablePrivateKeyEncryption` deliberately unset) and the bootstrap system key (K8s Secret). The `server-only` import guard makes leaking either into a client bundle a **build failure**, not a review miss; the bundle scan is a CI gate.
- **The no-scope-claim invariant** is the single load-bearing control that keeps Moira the authorization system of record. Risk: a future contributor "helpfully" adds `scope` to `definePayload`. Mitigations: a dedicated unit test, an e2e denial test, and an explicit per-wave reviewer check item.
- **`getSubject` regression.** Reverting to the Better Auth default subject would silently rebind Moira grants to a console-DB-local surrogate id. Mitigated by `moira-token.test.ts` and called out in `docs/admin-console.md`.
- **New secret class: the OAuth client secret now lives in Moira.** This is a deliberate §7.2 consequence. Risk: a D-1 read endpoint that returns the decrypted secret too liberally. Mitigation: D-1 must restrict it to system-key actors over the cluster-internal network, never to a bearer-JWT actor, never over the public ingress — flagged for 07's amendment and re-checked in review.
- **Console database is a new stateful component** (previously "none"). It contains no Moira secrets, but it does contain session and key material. Mitigations: dedicated schema and DB role with no grants on Moira's tables; TLS to Postgres; backup/restore documented; a restore that loses `jwks` is recoverable by re-registering the issuer's JWKS (Moira's `refresh-jwks` endpoint) — and, because subjects are IdP-derived rather than console-DB-derived, **admin grants survive a console DB rebuild**.
- **Redirect / open-redirect.** Exact redirect-URI registration at each IdP, `middleware.ts` host allow-list, `trustedOrigins`. Residual risk: a misconfigured `ALLOWED_CONSOLE_HOSTS` — mitigated by the production-config test asserting an unlisted host is rejected in CI before every release.
- **Land-grab / first-login race** (the failure mode `plans/01` §4.4 calls out): closed by design — the claim endpoint is Moira-side, system-key-gated, and idempotent per `(issuer, subject)`; a second person reaching `/setup` after a successful claim sees "already claimed" and cannot self-grant. `authorization-denial.spec.ts` proves it end-to-end.
- **Full admin for the single claimed identity** is an accepted MVP limitation (matches plan 01's Mode A), not a defect; multi-admin/scoped roles are 09+.

### Compatibility
Fully additive. Moira's machine-auth surfaces are untouched. Undeploying the console has zero effect on Moira's operability — system-key/CLI administration and **mode 3** (bring-your-own JWKS) continue to work exactly as before the console existed.

### Deployment
- **Risk:** deploying the console before plan 07 (or D-1) ships leaves `/setup` broken. Mitigation: the Wave 0 checkpoint blocks on both; the console's health check fails closed with a clear operator-facing keyed error if the expected endpoints 404, rather than serving a blank page.
- **Risk:** the D-1 interim env fallback ships by accident, leaving auth config build-time and §7.2-non-compliant. Mitigation: an explicit Definition-of-Done checkbox requires the env path to be **removed**, plus `auth-settings-round-trip.spec.ts` proves the Moira-backed path is live.
- **Risk:** algorithm mismatch — the `jwt` plugin's `EdDSA` default versus Moira's registered `allowed_algorithms`. Mitigation: the explicit `ES256` pin, the Wave 0 verification, and a unit test asserting the pin matches what the issuer row registers.
- **Risk:** OAuth client misconfiguration blocks all sign-in. Mitigation: `/login` renders a keyed "auth not configured" state (`console.error.auth_settings_unavailable`) rather than a stack trace, and the wizard is reachable to fix it.

### Rollback
Undeploy the `charts/moira-console` release; no Moira-side cleanup is required. The console's own schema can be dropped independently. If plan 07's grant itself must be reverted (wrong admin claimed), that is a plan-07-owned break-glass procedure (system-key re-grant / revoke), not this plan's concern — Moira remains the system of record.

### Deferred follow-ups (explicitly punted, not silently dropped)
- Generic-OIDC hardening beyond the `genericOAuth` baseline, multi-provider management UI, GitHub provider — **09**.
- Invitations / additional-admin self-service, ownership transfer, recovery beyond system-key break-glass — **09**.
- **Enterprise SAML SSO — permanently out of scope for the console.** Better Auth does not provide it; mode 3 (bring-your-own JWT/JWKS behind the customer's own IdP or SSO gateway) is the supported path. This is recorded as a limitation, not a roadmap item.
- Generated Moira client types from the committed OpenAPI spec, once P1-10's gate exists (hand-written types until then).
- Working audit-log cursor pagination, once P1-4 lands in plan 04.
- Additional locales beyond `en` — the i18n layer is structured for it; no catalog beyond English ships in MVP.
- Console-side rate limiting beyond Better Auth's built-in `rateLimit` on the setup/claim flow — low risk given the flow is system-key-gated, flagged for a future hardening pass.
