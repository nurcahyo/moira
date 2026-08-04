# Console Architecture

> **Status: partially implemented.** The Moira client, the typed DTO layer, the
> error boundary, the auth runtime, the console's own database, the sign-in
> surface and the once-only secret modal all exist. The setup wizard's UI, the
> admin-management surfaces and `/invite/[token]` do not. Each section below says
> which it is; sections with no marker describe shipped behaviour.

## The console is a separate deployable

The console (`console/`) is a Next.js application with its **own
Dockerfile**, its own dependency lockfile, and its own deploy lifecycle. It
is not part of the Rust `moira` binary or its build. It will run as a
distinct service in front of Moira's public/admin HTTP API.

## The console is a BFF (backend-for-frontend)

The console's server side (route handlers, server components, server
actions) is the only part of the system permitted to hold a Moira system
key, an admin key, or any decrypted credential. The browser talks only to
the console's own server; the console's server talks to Moira. A secret
must never cross into a client component, a `NEXT_PUBLIC_*` variable, or
any payload sent to the browser (`plans/CONVENTIONS.md` §6 rule 5, §7.5).

## The trust split: authentication in the console, authorization in Moira

This is the load-bearing boundary and it must not blur:

- **Authentication — "who is this human?"** happens in the console BFF.
  The console will run the OAuth/OIDC flow (Google, generic OIDC, or accept
  a bring-your-own JWT), verify the identity, and hold the resulting
  session. Moira never runs an OAuth flow and never stores passwords or
  sessions.
- **Authorization — "what may this identity do in Moira?"** happens in
  Moira, which is the **system of record**. Moira decides authorization
  from its own tables: `trusted_jwt_issuers` (which issuers it trusts) and
  `admin_identities` grants keyed on the stable pair `(issuer, subject)`,
  carrying scopes. A JWT the console mints must not carry a self-asserted
  `scope` claim — Moira copies scopes only from its own grant table, never
  from the token, so authorization cannot be bypassed by a token claim.

Concretely: the console proves *who* signed in; Moira alone decides *what*
that identity is allowed to do. Identity binds to `(issuer, subject)`, never
to email alone.

## The three identity modes Moira's trust model supports

1. **Google OAuth** — the default first-party option, via the console.
2. **Custom OAuth / generic OIDC** — any provider reachable via OIDC
   discovery, via the console.
3. **Bring-your-own JWT via JWKS** — an operator registers a trusted JWT
   issuer (JWKS URL, allowed algorithms, audience) directly with Moira.
   This path needs **no console and no OAuth at all**, which is what keeps
   air-gapped and machine-to-machine deployments working without the
   console in the loop.

## Configuration is runtime, not build-time

Auth provider configuration (issuer, discovery/authorization/token/userinfo
URLs, client id, allowed email domains, allowed algorithms) lives in Moira's own
database and is read by the console at runtime — consistent with how Moira
already treats providers, models, routing and credentials as database-owned
rather than baked into a build. An operator changes the IdP with an API call and
no redeploy.

That is why the Better Auth instance is built by a FACTORY rather than at module
scope: `getConsoleAuth` memoises exactly one instance per configuration digest,
and the digest hashes the full sorted set of provider `(id, version)` pairs plus
the newest console-side secret write. A `max(version)` would have been the
obvious digest and is wrong — a max cannot observe a row DELETION, so a deleted
provider would keep serving from cache.

There is a bootstrap consequence with no clean answer, recorded in
`lib/auth-runtime.ts`: reading that configuration needs a credential, and the
credential is what signing in produces. While `MOIRA_SYSTEM_KEY` is present the
console reads live and snapshots; once it is gone it serves sign-in from the
snapshot and refreshes on any request that carries an operator credential. The
snapshot is per process, which is why `charts/moira-console` still pins
`replicaCount: 1`.

**The sign-in screen reads that configuration without a credential.** "Read
by the console at boot" must not be taken to mean the console always holds a
Moira credential when it needs to render a login page — it does not, and
requiring one is circular: the credential is what signing in produces. An
operator who removes `MOIRA_SYSTEM_KEY` after setup, and a visitor landing on
a public invitation-acceptance page, both arrive with nothing. Moira
therefore serves the login-screen list anonymously at
`GET /api/v1/admin/setup/sign-in-methods`, narrowed to fields the browser
already sees during the OAuth flow it is about to start.

That endpoint is **not** the full configuration read. The wider
`GET /api/v1/admin/setup/auth-methods` stays authenticated and remains a
server-side call, because it carries `allowed_email_domains` — the
deny-by-default admin-claim policy, which must not be published to an
anonymous caller. See `docs/admin-identity-claiming.md`.

## The console's own database (implemented)

The console has a PostgreSQL database of its own, `CONSOLE_DATABASE_URL`, holding
Better Auth's session tables, the `jwt` plugin's ES256 key pair, Better Auth's
rate-limit counters, and the sealed OAuth client secret. It is separate from
Moira's database and has its own migrations under `console/db/migrations/`.

**See `docs/console-storage.md`** for the table inventory, the migration
commands, and the rotation runbooks for the two keys the database depends on —
in particular the non-obvious one: rotating `BETTER_AUTH_SECRET` against a
durable database leaves the console publishing a JWKS it can no longer sign for.

## Client secret custody (design decision D7)

The OAuth client secret is owned by the **console**, sealed with AES-256-GCM in
the console's own database — Moira does not store it and has no endpoint that
returns it. This preserves Moira's invariant that a decrypted secret never
crosses a network boundary, while still letting the console perform the OAuth
code exchange, which needs the plaintext in-process.

The unavoidable cost is two stores that can DRIFT: Moira holds `client_id`, the
console holds the secret sealed *against* that `client_id` as the AEAD's
additional data. `classifySecretDrift` names three distinct disagreements —
the console has no secret, the console's secret is bound to a different
`client_id`, or the Moira row carries no `client_id` at all — and `/login`
renders the specific one rather than collapsing them, because "re-enter the
secret" and "fix the provider row in Moira" are instructions to different
people.

There is no rotate-secret endpoint and there must never be one; rotation is a
console `put()`. `tests/unit/architecture/server-only-guards.test.ts` scans the
whole tree for that endpoint's name as a bare literal, comments included.

## The setup window: do not expose an unclaimed console to an untrusted network

**While a deployment is unclaimed, the setup window is open by design, and the
first party to complete setup becomes its owner.** The console must therefore not
be reachable from an untrusted network until the first admin has been claimed.
Bind it to a private network, an operator VPN, or a bastion for that interval,
and open it up afterwards.

The window is open exactly while both of these hold, and both are re-read from
Moira on every request rather than cached:

1. the console holds a bootstrap system key (`MOIRA_SYSTEM_KEY`), and
2. Moira answers `claimed: false` on `GET /api/v1/admin/setup/claim-status`.

Removing the system key after setup closes the window permanently — `/api/setup`
then answers `404`, and that is why the post-setup runbook tells operators to
remove it.

### What is protected inside the window

- **A row bound to another console's trusted issuer cannot be touched at all.**
  Which provider row a privileged write may target is derived server-side from
  Moira's own records — this console's trusted issuer, then the provider row
  bound to it — and is never named by the caller. A request that names a
  different row is refused with nothing written.
- **An enabled provider cannot be re-pointed without a session established
  through it.** An enabled row is a live authenticator, so rewriting its client
  id or its issuer/discovery/token/userinfo/JWKS URLs would repoint sign-in at
  another identity provider. Re-saving one requires a valid console session whose
  authenticating provider is that same row; otherwise the request is refused and
  nothing is written.
- **A disabled row may still be completed without a session**, deliberately: it
  authenticates nobody, and requiring a session would make an interrupted first
  run unresumable.
- **The OAuth client secret never crosses to Moira**, and the setup responses
  publish counts and presence rather than the allow-list or the endpoint URLs.

### What is NOT protected, and cannot be

- **An unprovisioned, unclaimed deployment is claimable by whoever finishes setup
  first.** There is no admin yet to authorise the first admin, and no session to
  require, so reachability is the only thing that decides it. This is inherent to
  first-run bootstrap and is not something the console can close in code — it is
  closed by not publishing the console until the claim is done.
- **Anyone who can reach the window can read the setup view model** — whether the
  deployment is claimed, which sign-in methods exist, how many domains are
  allow-listed. It is narrowed, but it is not secret.

### The one consequence operators meet

Because an enabled provider needs a session to be re-saved, a provider that was
enabled with a credential nobody can actually sign in with (a mistyped client
secret, say) cannot be corrected from the wizard: there is no session to be had
through a broken provider. Two ways out, both intended:

- provision again under a different provider slug — the create path is still
  open, and each slug gets its own trusted issuer; or
- correct the row directly against Moira's admin API with the bootstrap system
  key you already hold. The console declines to be an unauthenticated proxy for
  that write; it does not deny you the write.

## Routes, layouts, and where the auth boundary sits

```
app/layout.tsx                    root layout, <html lang="en">, generateMetadata()
├── app/(console)/                the AUTHENTICATED group — session gate in its layout
│   ├── layout.tsx                the gate, plus the chrome (nav + sign-out)
│   ├── page.tsx                  `/`
│   └── admins/page.tsx           `/admins` — grants, invitations, ownership
├── app/login/page.tsx            `/login` — deliberately OUTSIDE the group
├── app/invite/[token]/page.tsx   PUBLIC redemption page — also outside the group
└── app/api/**                    route handlers — outside every group by construction
    ├── admins/invites/…          create / withdraw an invitation
    ├── admins/identities/[id]    transfer ownership (PATCH) / revoke a grant (DELETE)
    └── invite/[token]/redeem     the invitee's own redemption
```

Three properties, each of which breaks something if changed:

- **The root layout stays at `app/layout.tsx`.** `app/api/**` sits outside every
  route group and Next requires a root layout for the whole segment tree; a tree
  whose only layout lives inside a group leaves those segments with no
  `<html>`/`<body>` ancestor.
- **`/login` is a sibling of the group, not a member.** `lib/errors.ts` maps
  three conditions to a redirect there — `isSessionExpired` (remedy
  `reauthenticate`), and the `already_complete` remedy on `409
  admin_identity_already_claimed`. Inside an auth-gated layout every one of them
  becomes a redirect loop.
- **The gate fails closed onto `/login`, including on a configuration problem.**
  `consoleRuntime()` returns `{ ok: false }` for "no provider is enabled yet" and
  throws for a Moira outage. Neither is a session, and rendering the console
  shell over a backend that cannot authorise anyone shows an operator a
  working-looking console.

`/login` must answer HTTP < 400 on a cold, unconfigured deployment: "no provider
is enabled yet" is the normal first-run state and renders as a 200 body carrying
a keyed message. `console/e2e/a11y.e2e.ts` fails the gate on any status >= 400
for every discovered page-level route, and route-group segments are already
stripped by the route discovery, so a new group needs no e2e edit. A DYNAMIC
route does — it needs a `DYNAMIC_ROUTE_FIXTURES` entry or the coverage guard
fails.

## The sign-in surface

`modules/signIn/SignInPanel.tsx` is an ORGANISM, not a molecule: the layering
test forbids `components/atoms/**` and `components/molecules/**` from calling
`fetch(`, from importing `next/navigation`, and from importing any specifier
matching `/(^|[/-])auth([/-]|$)|better-auth|next-auth/i`. It is also the first
`"use client"` file in the repository.

**At most one button, by construction.** `resolveAuthConfig` returns
`ambiguous_enabled_providers` when more than one provider is enabled, and
`loadAuthConfig` will not even read a secret in that case. A provider picker is
not "unbuilt" — it is wrong until the ownership question behind multi-provider is
decided.

**The refusal states are resolved server-side.** The anonymous
`GET /api/v1/admin/setup/sign-in-methods` projection is enough to RENDER a button
and not enough to RESOLVE the configuration behind one: it omits
`allowed_email_domains` and `trusted_jwt_issuer_id`, and `resolveAuthConfig`
refuses a row without either. A page that renders a button from that endpoint
alone shows a button that 503s on click — `app/api/auth/[...all]/route.ts`
answers 503 with a `message_key` and no English on exactly the deployment the
anonymous endpoint exists for. So `/login` asks `consoleRuntime()` whether a
sign-in can actually be resolved, and the panel renders a button only then.

Sign-in is a `fetch` to the mounted route handler, not a server action:
`nextCookies()` is deliberately absent from the Better Auth plugin list (with it
installed, sign-in returned no `Set-Cookie` at all and the callback failed
`state_security_mismatch`), and there is no module-scope `auth` object to import.

## Copy: the console has its own i18n catalog

`console/lib/i18n/` holds every console-originated string.

- `keys.ts` owns the keys and is CLIENT-SAFE — no `import "server-only"`. Five of
  the six modules that emit a `console.*` key carry that directive, so deriving
  the key union from them would drag the credential graph into the browser
  bundle. Each emitter imports from here and keeps its own exported name.
- `catalog.en.ts` declares `Record<ConsoleMessageKey, CatalogEntry>`, so a
  missing entry is a **type error** at `bun run typecheck`. This is the
  TypeScript spelling of the `const` gate at `src/i18n/catalog/mod.rs:107-121`,
  and it exists because a source-text walker only sees literal arguments — which
  is how 23 of 28 execution-failure classes shipped as bare keys on the Rust
  side.
- `index.ts` resolves **catalog → supplied fallback → the key itself**, the same
  order as `catalog_message` in `src/lib.rs:120-124`. Placeholders are
  interpolated into CATALOG messages only; a server-supplied `message` has
  already been interpolated by Moira and is rendered verbatim.

`docs/i18n-response-catalog.json` receives **no** console entries. It is
generated from the Rust catalog and compared field-for-field in both directions;
a console entry fails that gate. `lib/moira-keys.ts` mirrors Moira's KEYS only
and stays English-free.

## The guards, and what each one is guarding against

| Guard | Property | Why it is not obvious |
|---|---|---|
| `tests/unit/lib/i18n-catalog-coverage.test.ts` | every key is emitted, every emitted key is catalogued | a key with no emitter reads as coverage; the scanner must not use `grep`, which skips `lib/setup-flow.ts` as binary (it contains a NUL byte) |
| `tests/unit/lib/no-hardcoded-copy.test.tsx` | components render no English | a regex that stops matching reports zero violations — hence a positive-control fixture and floors |
| `tests/unit/architecture/layer-dependencies.test.ts` | credential reachability, `modules/ ↛ app/` | the credential set is DERIVED from credential shape, never from the `server-only` marker |
| `tests/unit/architecture/no-secret-props.test.ts` | no secret as a prop on a rendering layer | vacuous by construction on the day it lands; a positive control is what makes it real |
| `tests/contract/openapi-contract.test.ts` | DTOs and the operation registry match `docs/openapi.json` | `SCHEMA_CONTRACTS` is hand-maintained, so its completeness is itself source-scanned |
| `console/e2e/secret-leak.e2e.ts` | nothing secret reaches the browser | the environment harvester cannot see a runtime-minted token, so the suite owns one needle explicitly |

The credential-module set is derived rather than listed. Deriving it from
`import "server-only"` would be self-defeating — delete the marker, the module
leaves the set, every consumer still agrees, and the guard evaporates silently.
So the derivation runs on credential SHAPE (reads `process.env`, imports `pg`,
sends a credential header, constructs an AEAD, handles a `clientSecret`) plus the
transitive closure over value imports, and the marker is the thing ASSERTED.

`db/**` is in every reachability scan and deliberately carries no marker: those
are plain `bun run` scripts and `server-only`'s default export is a bare throw,
so the import would break `bun run db:migrate`. Its containment is asserted
directly instead — nothing under `lib/ app/ components/ modules/` imports it.

## Once-only secrets

`AdminInviteSecretResponse.secret` is the raw invitation token, returned exactly
once at creation and `null` on an idempotent replay. **`null` is the normal
case**, not an error: a UI that treats it as a failure reports a correct
operation as broken, on the retry path.

Three things are worth knowing before touching this surface:

1. **Nothing redacts it.** A secret-bearing response never passes through
   `lib/errors.ts`: `toMoiraError` is called only under `if (!response.ok)`, and
   a 201 body is returned raw. There is nothing between the JSON parse and a
   React prop.
2. **The token exists in exactly one place.** `OnceOnlySecretModal` is the single
   file allow-listed by `no-secret-props`, and `CopyButton` takes an element
   `id` rather than a value — so copying does not create a second holder. The
   invite link is composed inside the modal from an `inviteBaseUrl` prop for the
   same reason: a caller-built link would be a second string containing the
   token, held by a file that is allow-listed by nothing.
3. **The envelope is not `ApiKeySecretResponse`.** Moira's own doc comment says
   it is field-for-field identical; it is not. It carries a fourth required
   field, `notice: ResponseText`. A modal typed against the other shape compiles
   and silently drops the one string meant to be rendered to the operator.

## Sessions and transport

- Sessions: httpOnly + `SameSite=lax` cookies, `Secure` unless
  `CONSOLE_ALLOW_INSECURE_URLS` is set — which is itself a hard boot failure
  under `NODE_ENV=production`. Session lifetime is 8 hours; the Moira-bound JWT
  is 5 minutes.
- PKCE is unconditional. The console is a confidential client so it is not
  strictly required, but it costs nothing and closes authorization-code
  interception through a compromised redirect.
- There is no password path, ever: `emailAndPassword` is disabled. An admin
  console has exactly one way in, and a password would be a second, weaker one
  that the deployment's own auth policy never sees.
- Verified email is required, and the email/domain allow-list is enforced twice
  — by Moira at claim time, and by the console at the session boundary. The
  duplication is deliberate: without the second check a stranger with a valid IdP
  account holds a console session they can do nothing with, which reads to them
  as a broken console rather than a denied identity.
- Better Auth's rate limiter uses `storage: "database"`, so the limit is shared
  across replicas rather than multiplying by the pod count.

## Invitations and ownership

The operator-facing runbook is [docs/admin-invitations.md](admin-invitations.md); this section is
the console's side of it.

### The flow, end to end

1. An admin opens `/admins` and creates an invitation, bound to **one email
   address** or to **one email domain**, with a lifetime between 60 seconds and
   72 hours. There is no "anyone with the link" invitation: an unbound one would
   make a leaked URL equivalent to handing out admin.
2. Moira returns the raw token **exactly once**. `OnceOnlySecretModal` shows it
   and the link built from it; every later read of the record returns a shape
   with no token field at all. On an idempotent replay the token is `null` and
   the modal says so — that is the normal retry outcome, not a failure.
3. The admin shares the link **out of band**. The console sends no email.
4. The invitee opens `/invite/<token>`. The page exchanges the token
   **server-side** through the anonymous preview endpoint and renders what Moira
   is willing to tell an unauthenticated holder: the constraint, its value, and
   the expiry. Nothing about the inviter, the deployment, or the policy.
5. They sign in with whatever provider this deployment has configured, then
   accept. The console POSTs to its own route handler, which redeems with the
   **invitee's own** bearer token.

### Two consequences of single-primary ownership an operator will meet

Ownership is `admin_identities.is_primary` — **row state, not a scope**. A
`moira:admins:manage` scope was specified and is unimplementable: every admin
identity is granted `moira:admin`, and a `moira:admin`-holding trusted-JWT actor
is granted every scope by implication with no per-scope opt-out, so such a scope
would have been satisfied by everyone.

- **Transfer is one request, and it moves the flag.** `PATCH
  /admin-identities/{id}` with `{ "is_primary": true }` demotes every other
  active primary in the same transaction. There is no second call to make, and
  after it the operator who performed the transfer is no longer the owner.
- **A deployment's sole admin cannot be revoked through the API at all.**
  Revoking a grant clears `is_primary`, and the last-primary guard refuses that.
  The console renders this as a stated rule beside a disabled control with its
  remedy — *transfer ownership first* — rather than as a failed request.

### What is per-grant, and why the screen says so

`admin_identities` is keyed on `(issuer, subject)` where `issuer` is the
**console's**, so from the multi-provider wave one human signing in through two
providers holds **two grants with no column linking them**. Revocation is
per-grant, and `is_primary` is globally unique, so that human is the owner
through at most one of them.

`/admins` states this rather than inventing a grouping: a table that merged the
two rows would be claiming a person-level identity the data model does not have,
and an operator acting on it would revoke "the" row and leave the other live.

### The invitation form's pre-submit check is a HINT above one provider

It reads the enabled providers' `allowed_email_domains` and:

- **blocks** when no provider is enabled — nobody could sign in at all;
- **blocks** an uncovered domain when exactly one provider is enabled, where the
  union it computes provably equals the row Moira will resolve;
- **warns, and says so, above one provider**, because redemption applies exactly
  one provider row and the projection the console can read carries neither the
  trusted-issuer binding nor the ordering inputs that decide which. A block there
  would refuse invitations that would in fact redeem.

Either way it is UI gating only. Moira's redeem-time check is the authority, and
a policy-denied redemption does **not** consume the invitation — so the same link
works once the allow-list widens.

### Domain policy is never waived for an invitation

Plan 07 decision D3 applies unchanged: an invitation is a **scoping token, never
a policy exemption**. An invitee at a domain outside `allowed_email_domains` is
refused even holding a valid link, on both the console's session boundary and
Moira's. The invitation page renders that as an actionable instruction — add the
domain, then use this link again — and never conflates it with
`invite_email_mismatch` / `invite_domain_mismatch`, whose remedy is a new
invitation instead.

### The token in the URL

`/invite/[token]` carries a secret in a URL path. That is deliberate — it is the
only shape a shareable link can take — and it is bounded on both sides:

- Moira does a prefix lookup before any Argon2 work, so the endpoint is not a
  CPU-exhaustion oracle, and returns an identical `invite_not_found` for a wrong
  prefix and a wrong hash, so it is not a guessing oracle;
- the console exchanges the token server-side on first load, never renders it
  into visible copy, and contacts no third-party origin from that page — a single
  external request would put the full URL into a `Referer` header.

An unusable invitation renders as a **page** with a keyed explanation, not a 404:
it is a condition the holder needs explained, and the a11y walker requires every
discovered route to answer below 400.

### `/admins` renders personal data

The invitation list is a directory of who was invited, and it is returned to any
holder of `moira:admins:read`. That is the right audience; the screen says so in
its own copy so that future exports and retention are thought about that way.

## What is not built yet

**Account recovery.** There is no `is_recovery` column, no
`replaces_admin_identity_id`, no atomic revoke-and-grant swap and no
`admin_identity_recovered` event — omitted deliberately (decision D-W2-1: *"a
column no code writes is the schema equivalent of a catalog entry with no
emitter"*). Half of what it promises is already achievable as two ordinary
operations, revoke then invite; what is missing is the *atomicity* of the swap,
and atomicity is a backend property.

**Session management.** No active-sessions screen. `DELETE /admin-identities/{id}`
already revokes *authorization*, which is strictly stronger than ending an
*authentication*.

**An authenticated end-to-end path.** There is no authenticated Playwright
storage state, so no e2e in this repository can drive a signed-in screen. The
a11y gate asserts that gated routes redirect to `/login` and does **not** audit
them, with the unaudited set pinned in both directions — see
`console/e2e/a11y.e2e.ts`. Building the authenticated project needs a mock IdP
inside the Playwright environment; until then `/admins` is covered by unit tests
and by Moira's own integration suite, and that limit is written down rather than
papered over.
