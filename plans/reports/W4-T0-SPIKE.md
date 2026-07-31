# W4-T0 — can the authenticating `providerId` reach the token minter?

**Status: ANSWERED — YES, with constraints. Stage 4B is unblocked.**

Branch `spike/w4-t0-provider-session`, from `origin/main` at `05cf6e8`.
Spike code: `console/tests/spike/`. 10 tests, all green.

---

## 1. The question

Can the authenticating account's `providerId` be made available to the token minter for the
**current session**, in Better Auth 1.6.25 as vendored in this repo — either by stamping it onto the
session at creation, or by reading it inside `definePayload` / `getSubject`?

If no, plan 09 wave 4 **Stage 4B** has no honest implementation: the console would mint a token whose
`iss` names the wrong provider, silently reproducing **F24** while looking correct.

## 2. The answer

**Yes.** Approach 1 from the decision's ordered list works — `databaseHooks.session.create.before(data, context)`
reading `context.params.providerId` — and it needs approach 2 alongside it (`session.additionalFields`
plus a console migration `0003`) for the value to persist and be read back. Approach 3 (an
after-callback stamp) was not needed and is not recommended; see §7.

`console/node_modules` was absent at branch time. It was installed (`bun install --frozen-lockfile`,
414 packages) and the answer comes from reading that tree and driving flows against it, not from
published documentation.

## 3. The observation that settled it

The gate was whether `context.params.providerId` is populated at the moment `createSession(user.id)`
runs. It is. The chain, by file and symbol, all under `console/node_modules/`:

| # | File | Symbol | What it establishes |
|---|------|--------|---------------------|
| 1 | `better-call/dist/router.mjs` | router match | `params: route.params ? … : {}` is put on the endpoint input |
| 2 | `better-auth/dist/api/dispatch.mjs` | `dispatchAuthEndpoint` | wraps the handler in `runWithEndpointContext(internalContext, …)`, where `internalContext = {...input}` — so `params` is in AsyncLocalStorage |
| 3 | `@better-auth/core/dist/context/endpoint-context.mjs` | `getCurrentAuthContext` | reads that same store back |
| 4 | `better-auth/dist/db/with-hooks.mjs` | `createWithHooks` | `const context = await getCurrentAuthContext().catch(() => null)` then `hooks[model].create.before(actualData, context)`, merging a returned `{data:{…}}` |
| 5 | `better-auth/dist/db/internal-adapter.mjs:162` | `createSession` | routes to `createWithHooks(data, "session", …)` |
| 6 | `better-auth/dist/oauth2/link-account.mjs:134` | `handleOAuthUserInfo` | `const session = await c.context.internalAdapter.createSession(user.id);` — no provider argument, `override` left undefined. This is why the value has to come from the ambient context |
| 7 | `better-auth/dist/plugins/generic-oauth/routes.mjs:123,143` | `oAuth2Callback` | route is `/oauth2/callback/:providerId`; the handler itself reads `ctx.params?.providerId` |

**In-library precedent, found rather than assumed.** Better Auth's own shipped
`better-auth/dist/plugins/last-login-method/index.mjs` does exactly this, from a session
`databaseHooks`:

```js
if (path.startsWith("/callback/") || path.startsWith("/oauth2/callback/"))
  return ctx.params?.id || ctx.params?.providerId || path.split("/").pop();
```

So the mechanism is a supported pattern, not a private-API bet.

**Observed, not just read.** `console/tests/spike/w4-t0-provider-session.spike.test.ts`, test *"the
endpoint context at session-create time carries the callback's providerId"*, drives a real
authorization-code flow through a real TLS mock IdP and asserts on what the hook was actually handed:

- `hadContext === true` — a `GenericEndpointContext` was available (step 4 tolerates `null`);
- `path === "/oauth2/callback/:providerId"`;
- `params.providerId === "moira-console-idp-contractors"`.

Note the second one: `dispatchAuthEndpoint` sets `path: endpoint.path`, the route **template**. So
`path.split("/").pop()` yields the literal `":providerId"`. **4B must read `params`, never parse
`path`.**

## 4. The two-linked-accounts result — the case with teeth

A single-account fixture passes whether or not the mechanism works, which the decision's guard table
flags as the toothless version to reject. The load-bearing test is **G10**, run twice — on the memory
adapter and on real PostgreSQL.

Setup: two mock IdPs return the **same verified email** `dual@corp.test` with **different subjects**.
Signing in through A then B makes Better Auth link implicitly (`accountLinking.enabled` defaults on,
`disableImplicitLinking` unset, `requireLocalEmailVerified` defaults true, both sides verified).

Asserted in SQL against `console_auth_t0_spike`
(`console/tests/spike/w4-t0-durable.spike.test.ts`):

- `select id from "user"` → **1 row**. One human.
- `select "providerId","accountId" from "account"` → **2 rows**, `moira-console-idp` / `moira-console-idp-contractors`, subjects `…durable-aaaa` / `…durable-bbbb`.
- `select "providerId" from "session" order by "createdAt"` → `['moira-console-idp', 'moira-console-idp-contractors']`. The two sessions disagree, correctly.
- Minting from **B's** session: `iss = https://console.test/idp/contractors`, `sub = corp-idp-subject-durable-bbbb`. **Both halves name B.**
- Minting from **A's** session, same user row, concurrently: `iss = https://console.test/idp/corp`, `sub = corp-idp-subject-durable-aaaa`.

A mechanism that returned "the first account" or "the most recently updated account" would put A's
subject under B's `iss` — a token that verifies, names a real human, and resolves the wrong grant.
The spike asserts `sub !== SUB_A` explicitly for exactly that.

This also makes concrete the consequence wave 4 does **not** ship: one human holds **two** distinct
`(iss, sub)` pairs simultaneously, therefore two `admin_identities` grants, with no column linking
them.

## 5. The mechanism Stage 4B should use

Three parts, all in `console/tests/spike/w4-t0-spike-auth.ts` (`createSpikeAuth`), lift-ready:

**(a) The column.** In `lib/auth.ts` *and* `db/schema.ts`:

```ts
session: {
  additionalFields: {
    providerId: { type: "string", required: false, input: false },
  },
},
```

`required: false` because sessions live at deploy time have no value. `input: false` so no API
surface can set it — verified it does **not** block the internal write:
`@better-auth/core/dist/db/adapter/factory.mjs::transformInput` copies schema fields straight through
and never consults `input`, while `better-auth/dist/db/schema.mjs::parseInputData` — the function that
enforces `input: false` by throwing `<key> is not allowed to be set` — is only reached from
request-shaped data. The durable test writes the column with `input: false` set, so this is observed.

**(b) The stamp.**

```ts
databaseHooks: {
  session: { create: { async before(data, ctx) {
    const providerId = ctx?.params?.providerId;
    if (typeof providerId !== "string" || providerId === "") return; // leave NULL
    return { data: { ...data, providerId } };
  } } },
},
```

**(c) The mint.** `definePayload` returns `{ iss, email, email_verified }` and `getSubject` reads the
account for the **same** resolved provider, so `iss` and `sub` cannot disagree.
`better-auth/dist/plugins/jwt/sign.mjs` spells the issuer `.setIssuer(iss ?? defaultIss)`, so a
`definePayload`-supplied `iss` wins and `options.jwt.issuer` is only the fallback.

**Migration `0003`.** Not hand-written — derived from better-auth's own compiler at the pinned
version by `console/tests/spike/w4-t0-derive-0003.ts` (`getMigrations().compileMigrations()` against a
throwaway database already carrying `0001`/`0002`):

```sql
alter table "session" add column "providerId" text;
```

`{ pending: 1, toBeCreated: [], toBeAdded: ["session"] }`. Nullable, by better-auth's own choice.

## 6. Constraints 4B inherits

1. **The column is nullable, and pre-4B sessions WILL reach the minter.** They must **refuse**, never
   default. Asserted on PostgreSQL by nulling a live session's column and re-minting: non-200, no
   token.
2. **Refuse by throwing, not by returning nullish.** `sign.mjs::getJwtToken` spells it
   `sub: await getSubject(...) ?? ctx.context.session.user.id`. A `getSubject` that returns
   `null`/`undefined` silently falls back to the console's **own** user id — a token that verifies and
   matches no grant.
3. **NEW — `/get-session` is on the minting path.** `better-auth/dist/plugins/jwt/index.mjs` registers
   `hooks.after` with `matcher: context.path === "/get-session"`, whose handler calls `getJwtToken`
   and sets a `set-auth-jwt` response header. So a refusal that throws turns an ordinary session read
   into a **500**, not just a failed `/token`. Demonstrated both ways in the spike: part 3 asserts the
   500; part 4 sets **`jwt: { disableSettingJwtHeader: true }`** and gets a clean 200 session read with
   `/token` still refusing. **4B should set that flag** — the console has no client-side consumer of
   `set-auth-jwt`, and without it every page load of an un-upgraded session 500s. This was not
   anticipated by the decision and is the spike's main incidental finding.
4. **Read `params`, never `path`** — `path` is the route template (§3).
5. **One key pair, one `kid`, N issuer strings.** The spike asserts both tokens share a `kid`. The
   `iss` selection is the whole security boundary, exactly as §6 of the decision says. "The token
   verifies against the JWKS" is true in the broken arrangement too, so G8 must assert on `iss`.
6. **Provider ids are load-bearing.** `readIdpSubject` filters `account.providerId === config.providerId`;
   the incumbent must stay `"moira-console-idp"`. Unchanged by this spike, restated because the
   mechanism now depends on that string matching the route parameter as well as the account column.

## 7. What was ruled out, and how

- **The most-recently-updated-account heuristic** — forbidden by the decision, and not used. §4 shows
  why it is wrong: with two linked accounts it can name A while `iss` names B.
- **Disabling implicit account linking to force 1:1** — forbidden, and not used. Linking is left at
  its defaults and is *exercised*: §4's fixture depends on it working.
- **Approach 3, an after-callback stamp** — not needed and worse. `create.after` is queued through
  `queueAfterTransactionHook` and would leave a window in which a session exists with a NULL provider,
  turning a correctness property into a race. `create.before` writes it in the same insert.
- **`createSession`'s own `override` parameter** — exists
  (`internal-adapter.mjs:162 createSession(userId, dontRememberMe, override, overrideAll)`) but
  `handleOAuthUserInfo` passes only `user.id`, so it is unreachable without patching the vendored
  library. Rejected.
- **`user.additionalFields` via `mapProfileToUser`** — already ruled out in `lib/auth.ts`'s header for
  a different field, for reasons that apply unchanged here. Not retried.

## 8. Reproducing

```bash
cd console && bun install --frozen-lockfile
bun test tests/spike/                      # 10 tests: 8 memory-adapter, 2 PostgreSQL
bun run tests/spike/w4-t0-derive-0003.ts   # re-derive the 0003 DDL
```

The durable tests use their **own** database, `console_auth_t0_spike`, created on demand — never
`console_auth_test` (shared) and never `moira`. Full console suite with the spike present:
**561 pass / 1 fail**, the one failure being the deliberate `CONSOLE_SKIP_DB_TESTS` canary that is
also red on the baseline. `bun run typecheck` and `bun run lint` are clean.

## 9. Corrections to the brief

- The brief called the hypothesis "current evidence suggests it will be". It holds, and the citation
  chain in §3 is now observation rather than inference — plus one thing the brief did not have: the
  library ships a plugin that already uses this exact mechanism.
- The brief's ordered approach list treats 1 and 2 as alternatives. They are **not** — approach 1
  supplies the value and approach 2 is required for it to persist and be read back. 4B needs both.
- Nothing in the brief anticipated constraint §6.3 (`/get-session` mints, so refusal-by-throw breaks
  session reads). It is cheap to handle but must be handled deliberately.
