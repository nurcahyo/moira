# Plan 09 — Generic OIDC Hardening, GitHub Sign-In, Invitations & Additional Admins (Post-MVP)

> **Compliance note.** Written against `plans/CONVENTIONS.md` (verified 2026-07-25), which is authoritative and overrides any earlier draft of this file. The corrections CONVENTIONS forced into this revision are: (1) the console's identity layer is **Better Auth**, not Auth.js/NextAuth — plan 08 already ships Google *and* a generic-OIDC baseline via the **`genericOAuth` plugin**, so this plan's OIDC work is **hardening and operator-facing management**, not "add generic OIDC for the first time"; (2) **Atomic Design** file paths (§6) replace the previous `app/**/components/` layout; (3) auth config is **runtime, DB-backed in Moira** (§7.2), so this plan's multi-provider work writes into Moira's auth settings rather than into env vars; (4) the pinned toolchain (§5), mandatory unit+e2e+a11y+secret-leak testing (§3), i18n (§4), and the branch/PR/DoD rules (§1, §2, §8) apply here exactly as they do to 08.

## Summary

**Objective.** Extend the Moira admin console (shipped in plan 08 as a Better Auth BFF with Google sign-in and a working `genericOAuth` baseline) with **operator-facing provider extensibility** and **multi-admin lifecycle**: hardened generic-OIDC support managed from the console rather than from environment variables, a GitHub sign-in option, an invitation flow so an existing admin can grant a new `(issuer, subject)` admin identity without touching Moira's bootstrap system key, refined session management, and an ownership-transfer / account-recovery story that goes beyond the system-key break-glass that plans 07/08 already provide.

**Why ordered here.** Explicitly **post-MVP** per `plans/01-roadmap-and-dependencies.md` §2 (row 09) and §4.6 ("Identity features that remain post-MVP: GitHub provider, invitations/additional-admin flows, ownership transfer, account recovery beyond system-key break-glass"). It depends on plan 08 existing (the Next.js project, Better Auth configuration, `lib/moira-client.ts`, the `jwt`-plugin token path, the Atomic Design layering, and the single-admin claim flow are all **extended, not rebuilt**) and on plan 07's identity foundation (the `admin_identities (issuer, subject)` model this plan's invitation flow grants into). **Nothing in this plan is required to ship a working MVP console** — 08 alone is a complete, safe, single-admin console.

**Branch & PR (CONVENTIONS §1).** Branch `plan/09-generic-oidc-github-invitations`, cut from current `main` (or stacked on `plan/08-nextjs-console-google-oauth` if 08 has not merged, in which case the PR description names the base PR and the branch is rebased once 08 lands). Conventional Commits. One plan = one branch = one PR.

**What changed versus the previous draft of this plan.** The previous draft said "add Auth.js's built-in generic `OIDCProvider`" and "add Auth.js's built-in `GitHubProvider`." Both are superseded: plan 08 already configures Better Auth's **`genericOAuth` plugin** (`config: [{ providerId, clientId, clientSecret, discoveryUrl, issuer, requireIssuerValidation, pkce, scopes, mapProfileToUser }]`, verified against better-auth.com 2026-07-25), so generic OIDC is a *baseline capability* from 08 onward. This plan's OIDC contribution is therefore narrower and more honest:

| Previously claimed for 09 | Actual 09 scope after the Better Auth migration |
|---|---|
| "Add generic OIDC provider" | **Already in 08.** 09 adds: multiple simultaneous OIDC providers (`genericOAuth` accepts a `config` **array**), an operator-facing management screen writing into Moira's auth settings, a strict-mode policy surface for `requireIssuerValidation`/`pkce`/`scopes`, discovery-document health checks, and per-provider allowed-domain policy. |
| "Add GitHub via Auth.js `GitHubProvider`" | Better Auth **built-in `socialProviders.github`**, with the same server-side verified-email hardening and optional org-membership check as before. |
| "Auth.js session model" | Better Auth **DB-backed sessions** (plan 08 gave the console its own `console_auth` schema), which makes the "active sessions" screen and true remote sign-out **genuinely implementable** rather than best-effort. This is a real capability upgrade the Auth.js-era draft could not offer. |

**Honest limitation (CONVENTIONS §7.4), restated.** Better Auth does **not** provide enterprise SAML SSO and does not act as a SAML SP. This plan adds **no SAML support** and must not be read as doing so. Customers needing SAML use **mode 3** — they front SAML with their own IdP or SSO gateway that emits OIDC/JWT, and register that issuer's JWKS directly as a Moira `trusted_jwt_issuer`, bypassing the console entirely. That path is unchanged by this plan and needs no console at all.

**User-visible outcome.** An operator can (a) configure one or more standards-compliant OIDC providers, or GitHub, **from the console's auth-settings screen** (persisted in Moira, secrets encrypted with `SecretCipher`, applied at runtime with no redeploy), (b) as an already-admin user, invite a colleague by email or domain, who then signs in with any configured provider and is automatically granted admin scope bound to their invite, (c) transfer "ownership" (the ability to manage other admins) from one admin to another without a system-key operation, (d) see and revoke active console sessions for real, and (e) recover access via a documented, audited recovery path when at least one other admin remains.

**Included scope.**
- Multi-provider generic-OIDC management: N simultaneous `genericOAuth` entries, operator-managed via Moira's auth settings, with `requireIssuerValidation: true` and `pkce: true` enforced as non-overridable policy.
- GitHub sign-in via Better Auth's built-in `socialProviders.github`, with GitHub's weaker email/org guarantees explicitly compensated for.
- Invitation flow: an existing admin creates a scoped invite token (email- or domain-bound, time-limited, single-use); the invitee redeems it during sign-in with any configured provider; Moira grants `(issuer, subject)` → `moira:admin` via a new Moira admin-invite endpoint family.
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
- **Current behavior this plan changes:** after plan 08, Moira/the console support **exactly one** admin identity, with no in-console way to add a second admin, no real session revocation surface, and no recovery short of system-key break-glass. That is the gap this plan closes.

---

## Architecture

### Dependency on plan 08's D-1 (Moira DB-backed auth settings)

Plan 08 declares **D-1**: Moira must own a DB-backed auth-settings resource (non-secret config in a migration-backed table, client secrets encrypted with `SecretCipher`, admin CRUD endpoints, `LISTEN/NOTIFY` invalidation), owned by a plan-07 amendment. **This plan assumes D-1 is live and extends its data shape** to hold *multiple* providers rather than one:

- The auth-settings table must support N rows / N entries keyed by `provider_id` (`google`, `github`, and one per generic-OIDC provider), each with its own `client_id`, encrypted `client_secret`, `discovery_url`/`issuer`, `allowed_email_domains text[]`, `hosted_domain`, `required_org` (GitHub only), and `enabled`.
- If D-1 landed with a single-row shape, extending it to N rows is a **new forward migration owned by this plan** (append-only; never an edit to a merged migration), and this plan's Wave 1 owns it.
- **Wave 0 must confirm** D-1's shipped shape before Wave 2 begins. This plan does **not** invent auth-settings endpoint paths; it binds to whatever 07/D-1 froze.

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
   │   the just-verified Better Auth session
   ▼
Moira: validates token not expired/consumed, validates the email/domain
       constraint against the BFF-asserted verified email, grants
       (issuer = console, subject = invitee_sub) -> moira:admin,
       marks the invite consumed — all in one transaction under an advisory
       lock on the token hash (single winner)
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
- **BFF** — gains no new persistent secret class: GitHub and generic-OIDC **client secrets live in Moira, encrypted with `SecretCipher`** (CONVENTIONS §7.2), fetched server-side at auth-instance construction and held only in process memory. The invite-token value is never logged and never retained in client-visible state after the `/invite/[token]` page's first server-side exchange.
- **Moira** — the new admin-invite endpoints follow the same authentication/authorization model as every other admin endpoint. `preview`/`redeem` are **token-authenticated, not scope-authenticated**; the token is hashed at rest with the existing `ApiKeyHasher` (Argon2id + pepper, `src/security/api_keys.rs`) under a new namespace. **No new auth primitive is invented.**

### DB/migration changes

New Moira migration (append-only, sequential after plan 07's `0009_admin_identity_claims.sql` and after whatever D-1 shipped — exact number fixed at implementation time):

- **`admin_invites`** — `id uuid pk`, `token_hash text`, `token_prefix varchar(64)`, `fingerprint varchar(128)`, `pepper_version varchar(64)` (the exact secret-storage column set 07's `admin_setup_tokens` uses, which itself mirrors `system_api_keys`), `email_constraint text null`, `domain_constraint text null` (exactly one set, CHECK-enforced), `is_recovery boolean not null default false`, `replaces_admin_identity_id uuid null references admin_identities(id)`, `created_by_issuer text`, `created_by_subject text`, `status varchar(32)` (`pending`/`consumed`/`revoked`/`expired`, CHECK-constrained), `expires_at timestamptz`, `consumed_at timestamptz null`, `consumed_issuer text null`, `consumed_subject varchar(256) null`, `created_at`, `updated_at`, `deleted_at null` — following `0009`'s conventions exactly.
- **`admin_identities`** gains **no new column**. The only touch is the `moira:admins:manage` backfill described in Data flow.
- **Auth-settings multi-provider extension**, if D-1 shipped a single-provider shape (see the D-1 dependency above).

All in **append-only** migration files (07's convention: never edit a merged migration; corrections are new migrations), validated by the existing migration-contract CI job.

**Console-side.** Plan 08 already gave the console a `console_auth` schema (Better Auth `user`/`session`/`account`/`verification` + the `jwt` plugin's `jwks` table). This plan adds **no console schema of its own** but does newly *use* the `session` table: because Better Auth sessions are DB-backed, the "active sessions" screen is a **real** session registry with **real** revocation — a genuine capability upgrade over the previous Auth.js-era draft, which could only offer an audit-log-derived approximation. This plan states that plainly and removes the old "best-effort" caveat.

### API & OpenAPI changes

New Moira endpoints (this plan's only new Moira surface):
- `POST /api/v1/admin/admin-invites` — create. Scope `moira:admins:invite`. Body `{ email_or_domain_type: "email"|"domain", value: string, expires_in_seconds: u32, is_recovery?: bool, replaces_admin_identity_id?: Uuid }`. Response `AdminInviteSecretResponse` (once-only token, same envelope pattern as `ApiKeySecretResponse`). `Idempotency-Key` required.
- `GET /api/v1/admin/admin-invites`, `GET .../{id}` — list/inspect; no token value returned after creation. Scope `moira:admins:read`.
- `POST /api/v1/admin/admin-invites/{id}/revoke` — scope `moira:admins:invite`. `Idempotency-Key` required.
- `POST /api/v1/admin-invites/preview` — **token-authenticated, not scope-authenticated**; the path sits **outside** the `/api/v1/admin/*` scope-gated prefix because it is credentialed by a one-time invite token rather than an admin scope. POST (not GET) with the raw token in the **body only**, so it cannot leak into access logs or referer chains. Returns only non-sensitive descriptive fields (inviter's display email with the local part masked, e.g. `j***@example.com`; expiry; constraint pattern).
- `POST /api/v1/admin-invites/redeem` — token in body **+** `Authorization: Bearer <invitee's freshly minted, scope-claim-free JWT>`, binding redemption to a concrete `(issuer, subject)` in one atomic call. `Idempotency-Key` required; single winner via an advisory lock on the token hash.
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

### Backward compatibility

- Plan 08's single-admin console continues to work unmodified if this plan's new providers/invite UI are simply not configured: with only Google enabled in Moira's auth settings, `getAuth()` builds exactly the plan-08 instance.
- The `moira:admins:manage` backfill is backward-compatible (a scope append on at most one existing row) — no existing 07/08 admin loses access, and `admin_identities`' schema is unchanged.
- Existing Moira admin API consumers are unaffected; every new endpoint is additive.
- **Mode 3** (bring-your-own JWT/JWKS, no console, air-gap-friendly) is untouched.

### Deployment implications

- No new container or chart — this plan extends `console/` and `charts/moira-console/` from plan 08. **New provider credentials are not new chart values**: they are entered in the console and stored encrypted in Moira (CONVENTIONS §7.2). The only chart change is any new non-secret toggle.
- Moira's chart gains one new migration, handled by the existing `migration-job.yaml` Helm hook.
- Toolchain pins are inherited unchanged from plan 08 (§5): Next.js **16.2.11**, Node **24.x Active LTS**, Bun **1.3.14**, Playwright for e2e, `bun install --frozen-lockfile`, committed `bun.lock`, exact pins in `package.json`, `.nvmrc`/`engines` for Node. **This plan does not bump any of them** — a bump is a separate, separately-verified decision.

### Failure & recovery

- **Invite token leaked/guessed** — Argon2id+pepper hashed at rest (unusable from a DB-only compromise; explicitly *not* the plain-SHA-256 pattern P1-1 flags elsewhere), time-limited (`expires_at`, hard server-side cap), single-use (`status` → `consumed` atomically under the same advisory-lock single-winner pattern as Moira's other atomic admin commands).
- **Last `moira:admins:manage` holder revokes their own privilege** — a Moira-side guard rejects any `PATCH`/revoke that would leave zero **active** admins carrying `moira:admins:manage`, returning `admin_identity_last_primary` (403/409). If no precedent guard exists in 07's system-key deletion path, this plan **establishes** the pattern and flags adopting it there as a follow-up (not implemented here).
- **GitHub/OIDC IdP outage** — does not affect other providers or already-authenticated sessions; only new sign-ins via the affected provider fail, rendering a keyed error state.
- **Discovery document unreachable** — the auth-settings screen shows a keyed per-provider health state; a provider whose discovery URL fails validation cannot be saved as `enabled`.

---

## Detailed Implementation

### Console: multi-provider generic OIDC (`console/lib/auth.ts` + `console/lib/auth-settings.ts`, extended)

- `loadAuthSettings()` (plan 08) is extended to return **an array** of provider configs rather than at most one OIDC entry. `getAuth()` maps them into a single `genericOAuth({ config: [...] })` call — the plugin accepts a **`config` array**, so N providers need no extra plumbing.
- Per-provider policy is **enforced, not configurable**: `requireIssuerValidation: true` (the plugin's own default is `false` — this plan pins it true for every entry), `pkce: true`, `scopes: ["openid", "email", "profile"]` as the floor, and `mapProfileToUser` populating the `idpIssuer`/`idpSubject` additional fields plan 08 introduced (so the Moira-facing `sub` remains the **IdP's** stable subject, never the Better Auth `user.id`).
- Callback URLs follow the plugin's documented pattern `${baseURL}/api/auth/oauth2/callback/:providerId` and are registered **exactly** (no wildcards) at each IdP; `middleware.ts`'s host allow-list is unchanged and already covers them.
- The `databaseHooks.user.create.before` gate from plan 08 is extended to resolve the **per-provider** allowed-domain list, still **deny-by-default** (empty list denies everyone), throwing `APIError("FORBIDDEN", …)` with a message key on rejection. The check is provider-agnostic by construction; the new work is verifying it fires for every provider, not writing new logic per provider.

### Console: GitHub sign-in (`console/lib/auth.ts`, extended)

- Better Auth's built-in `socialProviders.github` (`{ clientId, clientSecret, scope: ["user:email", "read:org"?] }`), built from Moira's auth settings.
- **GitHub-specific hardening** (per `plans/01` §4.2's note that GitHub has "weaker org/email-domain policy"): request `user:email` explicitly and, server-side in the `databaseHooks.user.create.before` hook, call GitHub's `/user/emails` to find the **verified primary email** — GitHub's profile email can be null, unverified, or a `noreply` address. Reject sign-in if no verified email is obtainable, closing the gap where `profile.email` alone cannot satisfy the uniform verified-email requirement of `plans/01` §4.3. Optional per-provider `required_org` setting: additionally check `GET /orgs/{org}/members/{username}` server-side and reject non-members — a GitHub-specific substitute for the `hd` hosted-domain check Google natively provides (plan 08 uses Google's `hd` option for that).
- All GitHub API calls are server-side only, from `console/lib/github.ts` (`import "server-only"`), never from a component.

### Console: auth-settings management screen (extends plan 08's)

- Page: `console/app/(console)/settings/auth/page.tsx`, actions `console/app/(console)/settings/auth/actions.ts`.
- Organisms: `console/modules/authSettings/{AuthSettingsForm,ProviderList,ProviderEditor,DiscoveryHealthPanel}.tsx`.
- Client secrets are **write-only**: submitted to Moira (encrypted with `SecretCipher`), never returned by a read, never re-rendered into the form — the field shows a `MaskedValue` "configured" state on revisit. A successful save calls `invalidateAuthSettings()` so the next `getAuth()` rebuilds from the new config **with no redeploy**.

### Console: invitation UI

- Page `console/app/(console)/admins/page.tsx` (thin: guard + fetch + render), actions `console/app/(console)/admins/actions.ts`.
- Organisms `console/modules/admins/{AdminTable,InviteAdminForm,TransferPrimaryPanel,RecoveryPanel}.tsx`. `AdminTable` renders a "primary" badge for rows whose `granted_scopes` include `moira:admins:manage`.
- `createInvite(formData)` calls `POST /api/v1/admin/admin-invites`. The acting session's capability is checked client-side **for UI gating only**; Moira's own scope enforcement is the authority — never trust the client-side check alone.
- The invite link is displayed in plan 08's existing `console/components/molecules/OnceOnlySecretModal.tsx` — **reused, not re-implemented**, since 08 is merged by the time 09 runs.
- Public page `console/app/invite/[token]/page.tsx` (no session required) + organism `console/modules/invite/InviteAcceptPanel.tsx`: the server component calls Moira's `preview` endpoint with the URL token and renders "You've been invited…" plus a provider-agnostic set of sign-in buttons (one per enabled provider, driven by the same `SignInPanel` organism plan 08 shipped). An accept-invite intent is carried in a **signed, short-lived httpOnly cookie** set when the page loads (mirroring 08's claim-intent pattern), and the post-sign-in server action calls `redeem`. The raw token is exchanged server-side on first load and never retained in client-visible state.
- **Explicitly not built:** the console sends no invite emails. Flagged as a candidate enhancement, not silently assumed.

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

- `migrations/00XX_admin_invites.sql` (append-only, sequential after 07's `0009` and D-1's migration) — `admin_invites` table, the `moira:admins:manage` backfill, and (if required) the auth-settings multi-provider extension.
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
- `tests/admin_invite_lifecycle.rs` — create/preview/redeem/revoke happy paths; expired, consumed, revoked, wrong-email, wrong-domain redemption rejections; **concurrent-redemption single-winner** test (two simultaneous redeems on one token → exactly one succeeds), using an **acknowledgement gate, never `sleep()`** (CONVENTIONS §3 / finding P2-12); last-primary guard rejection; atomic recovery-swap test including a mid-transaction failure injection asserting neither the revoke nor the grant persists if the other fails; redeem with a JWT from an issuer that is **not** the console's registered issuer → rejected; redeem with a JWT carrying a self-asserted `scope` claim → the claim is **not** honoured (Moira's authorization still comes from `granted_scopes`).
- `tests/http_error_contract.rs` (extended) — every new error code returns a non-empty `message_key` **and** `message`, and every new key exists in the catalog (CONVENTIONS §4.5).
- `src/http/mod.rs` — route-coverage and atomic-idempotency-contract tests extended to include every new path with its `Idempotency-Key`/`If-Match` expectations.
- DB-dependent tests fail closed in CI (`panic!` when `CI` is set and `MOIRA_TEST_DATABASE_URL` is absent) — the existing pattern.

**Console unit** (`bun test`):
- `console/tests/unit/lib/github.test.ts` — verified-primary-email extraction across null / unverified / noreply-only / multiple-emails cases; org-membership check pass and fail; every call is server-side.
- `console/tests/unit/lib/invites.test.ts` — the raw invite token is never logged, never serialised into a client payload, and never placed in a URL query string.
- `console/tests/unit/lib/auth-settings-multi.test.ts` — N providers map to a single `genericOAuth({ config: [...] })`; `requireIssuerValidation: true` and `pkce: true` are forced on **every** entry; a provider missing a discovery URL cannot be enabled; per-provider allowed-domain lists are **deny-by-default**.
- `console/tests/unit/lib/moira-token.test.ts` (extended from 08) — the invitee's redeem-time token still carries **no `scope`/`scp` claim** and binds `sub` to the IdP subject, not the Better Auth `user.id`.
- Organisms: `console/tests/unit/modules/admins/{AdminTable,InviteAdminForm,TransferPrimaryPanel,RecoveryPanel}.test.tsx`, `console/tests/unit/modules/invite/InviteAcceptPanel.test.tsx`, `console/tests/unit/modules/sessions/SessionTable.test.tsx`, `console/tests/unit/modules/authSettings/{ProviderList,ProviderEditor,DiscoveryHealthPanel}.test.tsx`.
- Molecules (one per new molecule): `console/tests/unit/molecules/{ExpiryPicker,CopyableLink,ScopeChipList,DangerConfirmDialog}.test.tsx`.
- Atoms (one per new atom): `console/tests/unit/atoms/{Tooltip,Avatar,Divider}.test.tsx`.
- Architecture guards from plan 08 (`layer-dependencies`, `server-only-guards`, `no-secret-props`, `no-hardcoded-copy`, `i18n-catalog-coverage`) must remain green with the new files included — no new test file needed, but their passing is a Definition-of-Done item.

**Console e2e** (Playwright, `console/tests/e2e/`, local mock OIDC + a mock GitHub OAuth/API stub — **never real GitHub or Google in CI**):
- `invite-redeem.spec.ts` — admin A creates an invite; a **fresh browser context** redeems it as invitee B via mock OIDC; both appear as distinct admins in `GET /api/v1/admin/admin-identities` (asserted by a direct API call, not only through the UI).
- `invite-negative.spec.ts` — mismatched email, mismatched domain, expired token, double-redeem (`invite_already_consumed`), and a concurrent double-redeem race asserting exactly one winner end-to-end.
- `github-signin.spec.ts` — mock GitHub returning (a) no verified email → rejected, (b) a verified primary email → accepted, (c) non-member of `required_org` when configured → rejected.
- `ownership-transfer.spec.ts` — A transfers primary to B; A can no longer manage admins; B can; the audit log (queried via Moira's API) shows the correct event.
- `recovery.spec.ts` — B recovers a simulated locked-out A into a new identity C; A's old grant is gone; the audit log shows `admin_identity_recovered` as a distinct event type.
- `sessions.spec.ts` — two concurrent sessions for one admin; revoking one from `/settings/sessions` ends **that** session (the other survives); "revoke all others" leaves only the current one. **Real revocation, asserted against the DB-backed session store.**
- `multi-provider.spec.ts` — configure two OIDC providers plus GitHub from `/settings/auth`; all three sign-in buttons render; each callback route responds; disabling one removes its button **without a redeploy**.
- `authorization-denial.spec.ts` (extended from 08) — a signed-in identity with **no** grant is denied on every new admin screen and server action; an identity holding `moira:admin` but **not** `moira:admins:manage` can view `/admins` but cannot transfer or revoke.
- `a11y.spec.ts` (extended) — `@axe-core/playwright` on **every new page route**: `/admins`, `/invite/[token]`, `/settings/sessions`, `/settings/auth`. Zero critical/serious violations gates CI.
- `i18n-message-key.spec.ts` (extended) — force `invite_expired` and `admin_identity_last_primary`; assert the console renders the catalog string; then force an **unknown** `message_key` and assert the server-supplied `message` renders verbatim.
- `secret-leak.spec.ts` (extended) — no invite token (beyond the single intentional once-only reveal), no GitHub/OIDC client secret, ever appears in a browser-observed response body, rendered HTML, or `console.log`.
- `console/tests/secret-leak/bundle-scan.test.ts` (extended from 08) — the build output and SSR HTML contain no invite-token fixture, no client-secret fixture, no PEM header, and no `NEXT_PUBLIC_*` name matching `/(SECRET|KEY|TOKEN|PASSWORD)/i`; the violation set must be **empty**.

### Documentation

- `docs/admin-console.md` (created in plan 08) — new sections: multi-provider OIDC configuration, GitHub configuration + verified-email and org-membership behavior, the invitation flow, ownership transfer, the recovery flow, and **real** session management (the old "best-effort" caveat is **removed**, since DB-backed sessions make revocation genuine). The existing statement that **SAML SSO is not supported** (mode 3 is the path) is retained and restated here.
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
- **Confirm D-1's shipped auth-settings shape** and decide whether the multi-provider extension migration is needed.
- Resolve product decision 1 (**single vs. multiple primaries**) — blocking before Wave 3, since it changes the transfer server action materially. Resolve decisions 2–4 as well.
- Re-verify the toolchain pins are still §5's values; do **not** bump anything in this plan.

**Wave 1 — Moira-side identity-invite backend (single owner, internally sequential; fully independent of console work).**
- *Backend/Rust engineer*: the migration, `src/domain/identity.rs` DTOs, `src/infra/repositories/identity.rs`, `src/application/identity.rs`, `src/http/identity.rs` + route wiring, `src/security/authz.rs` scope constants, `src/i18n/catalog/{errors,notices}.rs` + `docs/i18n-response-catalog.json`, `tests/admin_invite_lifecycle.rs`, and the OpenAPI/route-coverage test updates. **Single owner for the whole vertical slice** — `src/http/mod.rs`'s route table and its expected-path `BTreeSet` are single, shared, order-sensitive collections.
- *Read-only security reviewer*: re-checks that invite-token hashing reuses `ApiKeyHasher` (not a fresh `sha256` — a direct regression risk against P1-1) and that the atomic-swap transaction boundaries are correct.

**Wave 2 — Console provider extensibility (parallel with Wave 1; depends on Wave 0 only).**
- *Security/OAuth engineer*: `console/lib/{auth,auth-settings,github}.ts` extensions, `console/modules/authSettings/**`, `console/app/(console)/settings/auth/**`, plus every security-invariant unit test. No shared files with Wave 1 (console vs. Rust).

**Wave 3 — Console invitation & admin-management UI (after Wave 1 ships its endpoints and Wave 0's primary-model decision; parallel internally by directory).**
- *Frontend engineer A*: `console/app/(console)/admins/**` + `console/modules/admins/**`.
- *Frontend engineer B*: `console/app/invite/[token]/**` + `console/modules/invite/**` (public route, separate directory — no overlap with A).
- *Frontend engineer C*: `console/app/(console)/settings/sessions/**` + `console/modules/sessions/**`.
- *Design-system engineer*: the new molecules and atoms (`ExpiryPicker`, `CopyableLink`, `ScopeChipList`, `DangerConfirmDialog`, `Tooltip`, `Avatar`, `Divider`) **plus their unit tests** — presentational only, touching neither `lib/` nor `modules/`, which keeps this track fully parallel and the layering honest by construction.

**Wave 4 — Integration, hardening, deployment (parallel, disjoint).**
- *Test engineer*: the full e2e suite additions and the mock GitHub stub; `tests/admin_invite_lifecycle.rs` finalisation in coordination with Wave 1's owner (**read access only** to that Rust test file — writes go through the Wave 1 owner).
- *DevOps engineer*: `charts/moira-console/**` value additions and a rollout-order check of `charts/moira/templates/migration-job.yaml` against the new migration (no edit expected).
- *Security reviewer*: final pass on token redaction, the last-primary guard, and confirmation that the invite flow cannot escalate a non-invited identity (negative tests: redeem with a mismatched email; redeem with a JWT from a non-registered issuer; redeem with a self-asserted `scope` claim).

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
| Moira auth-settings read/write (multi-provider) | `X-Moira-System-Key` (console boot) / `Authorization: Bearer` (`moira:admin`) for the settings screen | `Idempotency-Key` + `If-Match` on write |

### JWT claims for the redeem call

Identical to plan 08's minted token (`iss`, `sub`, `aud`, `iat`/`exp` ≤ 120s, `jti`) and, per 08's corrected contract, **no `scope`/`scp` claim and no email claims**. The invitee's freshly-authenticated Better Auth session produces this token from the `jwt` plugin exactly as any post-claim admin session does; since the invitee has no `admin_identities` grant yet, 07's grant union yields **zero scopes** — the token proves `(iss, sub)` and nothing more, which is precisely what redemption needs. The invitee's verified `email`/`email_verified` travel in the redeem request **body** (BFF-asserted from the session), mirroring 07's claim endpoint. `sub` is the **IdP's stable subject** (plan 08's `jwt.getSubject` → `idpSubject`), never the Better Auth `user.id`. **No new claim shape is introduced** — invitation is "the claim flow, generalized to N times with a scoping token," not a parallel identity mechanism.

### Scopes/authz

- **Interaction with the full-admin scope.** Plans 07/08 grant `moira:admin` (`ADMIN_SCOPE`), which Moira's admin authorization already treats as satisfying admin endpoints regardless of granular scope — so existing admins can invite/manage without a re-grant, matching how `moira:jwt-issuers:*` granular scopes coexist with `moira:admin` today. **Except `moira:admins:manage`**, which is deliberately checked as an **explicit** scope (**not** implied by `moira:admin`), because it is precisely the primary/ownership distinction *between* admins who all hold `moira:admin`. This carve-out deviates from the implied-by-full-admin default and must be implemented **and tested** as an explicit check.
- `moira:admins:invite` — create/revoke invites.
- `moira:admins:read` — list admin identities and invites.
- `moira:admins:manage` — patch (primary-scope toggle), revoke admin identities, and create **recovery** invites. Recovery-invite creation is gated on `moira:admins:manage`, not merely `moira:admins:invite`, since recovery is a higher-privilege action than routine onboarding.
- Redemption itself requires **no** admin scope on the invitee's JWT — authorization for redemption is possession of a valid token plus a matching verified email/domain, exactly analogous to the plan-07 claim being system-key-gated rather than scope-gated.

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
- Browser: `console/tests/e2e/{invite-redeem,invite-negative,github-signin,ownership-transfer,recovery,sessions,multi-provider,authorization-denial,i18n-message-key,secret-leak,a11y}.spec.ts` against a running console + a real test-fixture Moira, with a **local mock OIDC provider and a mock GitHub OAuth/API stub** — never real GitHub or Google in CI.

### Accessibility
`@axe-core/playwright` on **every new page-level route** (`/admins`, `/invite/[token]`, `/settings/sessions`, `/settings/auth`) plus plan 08's existing routes. Zero critical/serious violations gates CI.

### Secret-leak
Extended `console/tests/secret-leak/bundle-scan.test.ts` (build output + SSR HTML, **empty** violation set) and `console/tests/e2e/secret-leak.spec.ts` (browser-observed responses, rendered HTML, console output) covering the invite token and the GitHub/OIDC client secrets.

### Production-config tests
Boot with only Google configured (plan 08's baseline) → invite/OIDC/GitHub UI elements are gracefully absent, not broken. Boot with all providers configured → every sign-in button renders and every callback route responds. Boot with an unreachable discovery URL → that provider cannot be enabled and the screen shows a keyed health state.

### Helm / Kubernetes validation
`helm lint` and `helm template … | kubeconform` on `charts/moira-console` with the new optional values both set and unset (two template runs), asserting no secret value renders into a `ConfigMap` in either case.

---

## Definition of Done

**CONVENTIONS §8 compliance checklist**
- [ ] Work performed on branch `plan/09-generic-oidc-github-invitations`; PR opened with all required description sections (§1.4).
- [ ] All §2 gates pass — Rust (`fmt`, `clippy`, `test`, `build --release --locked`, clean migration validation) **and** frontend (`bun install --frozen-lockfile`, `bun run lint`, `bun run typecheck`, `bun test`, `bunx playwright test`, `bun run build`).
- [ ] **Unit tests** delivered and passing — Rust in-module tests, plus one console test per **new atom**, per **new molecule**, and per **new organism**.
- [ ] **E2E tests** delivered and passing — HTTP-level `tests/admin_invite_lifecycle.rs` for Rust, and Playwright for the console (invite/redeem, GitHub sign-in, ownership transfer, recovery, sessions, multi-provider, authorization denial).
- [ ] Every new error/notice string has an i18n **key + English default** in the Rust catalog, mirrored into `docs/i18n-response-catalog.json`, with `tests/http_error_contract.rs` asserting presence; every new console string has a `console.*` key with an English default and `i18n-catalog-coverage.test.ts` / `no-hardcoded-copy.test.tsx` stay green.
- [ ] Toolchain pins unchanged and still §5-compliant (**Next.js 16.2.11 · Node 24 LTS · Bun 1.3.14**); `bun.lock` committed; **Atomic Design layering respected with the one-way dependency rule**, proven by `layer-dependencies.test.ts` covering every new file.
- [ ] Auth config for every provider is **runtime/DB-backed in Moira**, client secrets **encrypted with `SecretCipher`** and never returned to the browser; **no `scope` claim in any minted JWT**; per-provider domain policy is **deny-by-default**.
- [ ] **No secret-leak**, verified by the extended bundle scan (empty violation set) and the extended e2e secret-leak spec.

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
- **Last-primary lockout** — guarded server-side and negative-tested; residual bug risk is mitigated by system-key break-glass remaining a working fallback regardless (this plan never removes it).
- **New provider secrets in Moira** — GitHub/OIDC client secrets join the encrypted-at-rest set. Risk: an auth-settings read endpoint that returns them too liberally. Mitigation: inherited from plan 08's D-1 constraint (system-key actors over the cluster-internal network only, never a bearer-JWT actor, never over the public ingress), re-checked in this plan's review.

### Compatibility
Additive to Moira's schema (one new table plus a one-row scope backfill; no column changes) and to the console (opt-in providers and screens). No existing 07/08 behavior changes when the new settings are left unconfigured.

### Deployment
- **Migration ordering** — this plan's migration must land after plan 07's final migration number **and** after D-1's; coordinate the exact filename/number at implementation time rather than hardcoding it here (07's and D-1's final numbers are not knowable while this plan is written).
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
