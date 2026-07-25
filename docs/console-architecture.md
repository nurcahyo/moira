# Console Architecture

> **Status: none of this is implemented yet.** The console currently
> exists only as a workspace scaffold — toolchain, TypeScript/lint/format
> config, the Atomic Design directory structure, test harnesses, and a
> Dockerfile. There is no auth flow, no setup wizard, and no Moira client.
> This document describes the *target* relationship between the console and
> Moira, as fixed by `plans/CONVENTIONS.md` §7 and elaborated in
> `plans/07-identity-foundation.md` / `plans/08-nextjs-console-google-oauth.md`,
> so that scaffold-phase work (this document included) doesn't drift from
> the design those plans will implement.

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
URLs, client id, allowed email domains, allowed algorithms) is intended to
live in Moira's own database, written by an eventual setup wizard and read
by the console at boot and on invalidation — consistent with how Moira
already treats other runtime configuration (providers, models, routing,
credentials) as database-owned rather than baked into a build. It is not
implemented in the scaffold.

## Client secret custody (design decision D7)

The OAuth client secret is planned to be owned by the **console**, stored
encrypted in the console's own database — Moira is not planned to store it
or ever return it. This preserves Moira's existing invariant that a
decrypted secret never crosses a network boundary, while still letting the
console perform the OAuth code exchange, which requires the plaintext
secret in-process. See `plans/CONVENTIONS.md` §0 (decision D7) for the full
rationale and consequences; none of it is built yet.

## Sessions and transport (target, not yet built)

- Sessions: httpOnly + Secure + SameSite cookies, managed by the console.
- PKCE, `state`, and `nonce` are mandatory on every OAuth flow, with an
  exact redirect-URI allow-list.
- Verified email is required; the email/domain allow-list Moira enforces is
  deny-by-default.
- Any JWT the console mints for calls into Moira is short-lived.

## What exists today

Only the workspace scaffold described in `console/README.md`: pinned
toolchain, TypeScript/lint/format configuration, the empty Atomic Design
directory skeleton with a small number of trivial example primitives, unit
and e2e test harnesses, and a Dockerfile. No route in the console calls
Moira. No auth flow exists. This document will be revised as
`plans/07-identity-foundation.md` and `plans/08-nextjs-console-google-oauth.md`
land real behavior.
