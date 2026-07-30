# Moira Console

The Moira admin console — a Next.js BFF (backend-for-frontend) that will
eventually handle operator authentication and administration UI for Moira.

**This is scaffold only.** See [Status](#status) below.

## Toolchain (pinned — see `plans/CONVENTIONS.md` §5)

| Tool | Version |
|------|---------|
| Next.js | `16.2.11` (App Router) |
| Node.js | `24.x` Active LTS (`.nvmrc` pins `24.18.0`) |
| Bun | `1.3.14` — package manager, script runner, unit-test runner |
| Playwright | e2e runner (`bunx playwright test` / `bun run e2e`) |

React is not pinned independently — it tracks whatever Next.js 16.2.11 bundles.

## Running

```bash
# from console/
bun install --frozen-lockfile   # installs pinned deps from the committed bun.lock

bun run dev                     # dev server (next dev)
bun run build                   # production build (next build)
bun run start                   # serve the production build (next start)

bun run lint                    # eslint . (flat config: eslint-config-next + eslint-config-prettier)
bun run typecheck               # tsc --noEmit
bun run format                  # prettier --write .

bun test                        # unit tests (bun:test) — includes the layering-enforcement
                                 # test at architecture.test.ts

bun run e2e                     # Playwright end-to-end tests

bun run db:migrate              # apply console/db/migrations to $CONSOLE_DATABASE_URL
bun run db:check                # report pending migrations, exit 1, change nothing
bun run db:generate             # emit the DDL better-auth wants and the database lacks
```

All of the above run against Node 24.x (`.nvmrc`) and Bun 1.3.14
(`packageManager` in `package.json`). CI must use `bun install
--frozen-lockfile` — never a bare `bun install` that could silently drift
the lockfile.

## The console's own database

`CONSOLE_DATABASE_URL` names a PostgreSQL database belonging to the console —
**never Moira's**. It holds Better Auth's session tables, the `jwt` plugin's
ES256 key pair, Better Auth's rate-limit counters, and the sealed OAuth client
secret. `console/db/` owns its schema; `docs/console-storage.md` is the
operator runbook, including the two key-rotation procedures.

It is **required under `NODE_ENV=production`** and optional elsewhere. Omitting
it selects the ephemeral path (Better Auth's `memoryAdapter` +
`InMemoryConsoleSecretStore`), which is what lets `bun test` and `next dev` run
with no database at all.

The database-backed tests are **not** skipped when no URL is configured — they
default to `postgres://postgres:postgres@127.0.0.1:5432/console_auth_test`
(creating the database if needed) and fail loudly if PostgreSQL is unreachable.
`CONSOLE_TEST_DATABASE_URL` overrides it. `CONSOLE_SKIP_DB_TESTS=1` disables
them and **reds** `tests/integration/console-db-availability.test.ts` on
purpose: a suite that silently disables itself is worse than one that is red.

## Atomic Design layering (mandatory — `plans/CONVENTIONS.md` §6)

The console UI is organized into four layers with a **strict one-way
dependency rule**: pages → organisms → molecules → atoms. Each layer may
depend only on itself and the layers to its right in that chain; nothing
may import back to its left.

| Layer | Meaning | Location |
|-------|---------|----------|
| **Pages** | Next.js routes: routing, auth gating, server-side data fetching. Kept thin — real logic lives in organisms. | `console/app/**/page.tsx`, `layout.tsx`, `route.ts` |
| **Organisms** | Feature-aware modules that own a slice of a page (e.g. a setup wizard, a provider table). May call server actions and the Moira client. | `console/modules/<feature>/` |
| **Molecules** | Composite, presentational components built from atoms (e.g. a labeled form field, a confirm dialog). | `console/components/molecules/` |
| **Atoms** | Primitive, presentational components (e.g. a button, an input, a badge). | `console/components/atoms/` |

Shared non-UI logic (Moira client, auth helpers, formatting utilities) lives
in `console/lib/` — never inside `components/`.

**The rule, precisely:**
- An atom must never import a molecule, an organism (`modules/**`), or a page (`app/**`).
- A molecule must never import an organism or a page.
- Atoms and molecules are presentational and feature-agnostic: no Moira/API
  calls, no `next/navigation` side effects, no auth logic. They receive
  everything through props.
- Organisms own feature logic and may call the Moira client and server
  actions; they compose molecules and atoms.
- Pages own routing, auth gating, and server-side data fetching, then
  delegate rendering to organisms.

This is enforced by a static test, not just convention: **`bun test
architecture.test.ts`** scans `components/atoms/**` and
`components/molecules/**` and fails the build if any file in those
directories imports upward/laterally (molecules/modules/app), imports
`next/navigation`, imports an auth module, or performs a network call
(`fetch`, `axios`). Run it as part of `bun test`.

## Secrets never descend past the page/server boundary

A system key, admin key, or decrypted credential must never be passed as a
prop into an organism, molecule, or atom — those layers render client-side.
Anything secret is read and used only on the server (pages, route handlers,
server actions) and stays there. Nothing secret may ever appear in
`NEXT_PUBLIC_*` environment variables or in any client-bundled code.

## Status

This is a **workspace scaffold only**: toolchain, TypeScript/lint/format
config, the Atomic Design directory structure with a small number of
genuinely trivial example primitives, test harnesses (Bun unit, Playwright
e2e, axe accessibility), a Dockerfile, and this documentation.

**Not yet implemented** — all of the following arrive in
[`plans/08-nextjs-console-google-oauth.md`](../plans/08-nextjs-console-google-oauth.md),
after `plans/07-identity-foundation.md` lands Moira's identity contract:

- Better Auth, Google OAuth, or any OAuth/OIDC flow
- The setup wizard
- Any Moira API client or call to a Moira endpoint
- JWT minting or JWKS
- Sessions or login pages

See [`docs/console-architecture.md`](../docs/console-architecture.md) for
how the console will relate to Moira once those pieces exist.
