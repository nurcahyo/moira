// @server-only
//
// The Better Auth instance: the console's session layer and its token issuer.
//
// ============================================================================
// WHY THE INSTANCE IS BUILT AT RUNTIME, NOT AT MODULE LOAD
// ============================================================================
//
// Almost every Better Auth example constructs `betterAuth({...})` once, at module
// scope, from build-time environment variables. This console cannot: per §7.2 the
// auth provider's configuration lives in Moira's DB-backed settings, so an
// operator changes the IdP, the client id, the scopes, or the allowed email
// domains with an API call and no redeploy. `createConsoleAuth` is therefore a
// factory, and `getConsoleAuth` memoises exactly one instance per configuration
// digest (`ResolvedAuthConfig.cacheKey`, which hashes the full set of provider
// `(id, version)` pairs plus the newest console secret write).
//
// ============================================================================
// WHAT `sub` IS, AND WHY IT IS NOT THE BETTER AUTH USER ID
// ============================================================================
//
// Moira's grant key is `(issuer, subject)` in `admin_identities`, where `issuer`
// is the CONSOLE's issuer — so `subject` must equal the `sub` claim of the JWT
// this console mints, or the grant that the setup wizard created authorises
// nobody.
//
// Better Auth's default `sub` is `session.user.id`: a value this console
// generates and stores in its OWN database. Using it would mean the grant is
// keyed to a row in the console's database rather than to a human — and losing
// that database (a restore from an older backup; a restart, on the ephemeral
// development path) would orphan every admin grant. There is no recovery from
// that, because a second claim is refused `409 admin_identity_already_claimed`.
// Durable storage reduces how often that can happen; it does not change the
// argument, which is why `sub` still comes from the IdP.
//
// So `sub` is the IDP's stable subject, read from `account.accountId` — the
// column Better Auth itself populates from the provider's `sub` on every OAuth
// sign-in. It is stable across console database loss by construction, because
// the console never mints it.
//
// A `user.additionalFields` entry fed by `mapProfileToUser` was tried first and
// does NOT work: Better Auth filters the mapped profile against the user schema
// before `createOAuthUser`, so an `input: false` field is dropped and the user
// row comes back without it (observed: the row held only name/email/
// emailVerified/createdAt/updatedAt/id, while `account.accountId` held the
// subject correctly). Setting `input: true` would make it survive — and would
// also make it settable through `update-user`, letting a signed-in operator
// rewrite their own `sub` and mint a token for somebody else's grant. Reading
// the account row has neither problem.
//
// REVERSAL CONDITION: if the console ever supports linking several IdP accounts
// to one console operator, `sub` must become a console-owned stable id, and the
// claim body in `setup-flow.ts` must send that same id. The two must change
// together; changing either alone silently breaks authorisation.
import "server-only";

import { betterAuth } from "better-auth";
import { genericOAuth } from "better-auth/plugins/generic-oauth";
import { jwt } from "better-auth/plugins/jwt";
import { memoryAdapter } from "better-auth/adapters/memory";


import { AUTH_BASE_PATH, AUTH_JWKS_PATH, type ConsoleEnv } from "./env";
import type { ResolvedAuthConfig } from "./auth-config";
import {
  assertAdmissibleSession,
  checkSession,
  type ConsoleSessionIdentity,
  type SessionCheck,
} from "./moira-session";

/** How long a minted Moira-bound JWT lives. */
export const MOIRA_JWT_LIFETIME = "5m";

/**
 * The algorithm pin.
 *
 * Moira accepts EdDSA as well (`src/security/auth.rs` maps it through to
 * `jsonwebtoken::Algorithm::EdDSA`, so plan 08's "Wave 0 must verify whether
 * Moira accepts EdDSA" is answered: it does). ES256 is nonetheless the pin,
 * because it is the more widely interoperable of the two and because the
 * console registers its issuer with an explicit `allowed_algorithms: ["ES256"]`
 * either way. The two must agree — `setup-flow.ts`'s `ConsoleIssuerConfig`
 * defaults to `["ES256"]` for the same reason.
 */
export const MOIRA_JWT_ALGORITHM = "ES256" as const;

/** Better Auth's `database` option, passed through so tests can substitute one. */
export type ConsoleAuthDatabase = Parameters<typeof betterAuth>[0]["database"];

/**
 * The tables the console's plugin set requires.
 *
 * `memoryAdapter` does NOT create tables on demand — it looks the model up in
 * the object it was handed and throws `Model <name> not found` if it is absent.
 * Passing `{}` therefore produces a runtime failure on the first account write,
 * several redirects into the OAuth flow, reported as a database error.
 *
 *   user, session, account   Better Auth core
 *   verification             the OAuth `state`/PKCE record
 *   jwks                     the jwt plugin's ES256 key pair
 *   rateLimit                required by `rateLimit.storage: "database"` below
 *
 * A durable adapter runs migrations instead and needs none of this; this
 * function exists solely because the in-memory path has no migration step.
 * Adding a plugin or an option that introduces a table means adding it here as
 * well, or the ephemeral path fails on first use with a database error several
 * redirects into the OAuth flow.
 */
export function createConsoleMemoryDatabase(): Record<string, unknown[]> {
  return { user: [], session: [], account: [], verification: [], jwks: [], rateLimit: [] };
}

/* -------------------------------------------------------------------------- */
/* Reading the IdP subject back                                               */
/* -------------------------------------------------------------------------- */

/** A linked provider account, as Better Auth stores it. */
interface LinkedAccount {
  readonly accountId: string;
  readonly providerId: string;
}

/**
 * The slice of Better Auth's `$context` this module uses.
 *
 * Structural rather than imported: `AuthContext` is a large internal type whose
 * shape is not part of Better Auth's public contract, and depending on all of it
 * would make a minor upgrade a compile error for no benefit.
 */
export interface ConsoleAuthContext {
  readonly internalAdapter: {
    findAccountByUserId(userId: string): Promise<LinkedAccount[]>;
  };
}

/** No usable `account.accountId` — minting would key a grant to nothing. */
export class MissingIdpSubjectError extends Error {
  constructor(userId: string, providerId: string) {
    super(
      `no linked ${providerId} account carries a subject for console user ${userId}. ` +
        "Refusing to mint a Moira-bound JWT: a token whose `sub` is not the IdP's subject " +
        "verifies against the console's JWKS and then matches no admin_identities grant, " +
        "which surfaces as a bare 403 from Moira with nothing locally to explain it.",
    );
    this.name = "MissingIdpSubjectError";
  }
}

/**
 * The IdP's stable subject for a console user.
 *
 * `account.accountId` is Better Auth's own record of the provider's `sub`; the
 * `providerId` filter matters because a future second provider would otherwise
 * make the answer depend on row order.
 */
export async function readIdpSubject(
  context: ConsoleAuthContext,
  providerId: string,
  userId: string,
): Promise<string> {
  const accounts = await context.internalAdapter.findAccountByUserId(userId);
  const account = accounts.find((candidate) => candidate.providerId === providerId);
  if (account === undefined || typeof account.accountId !== "string" || account.accountId === "") {
    throw new MissingIdpSubjectError(userId, providerId);
  }
  return account.accountId;
}

export interface ConsoleAuthDeps {
  readonly env: ConsoleEnv;
  readonly config: ResolvedAuthConfig;
  /**
   * The durable store, or omitted for Better Auth's in-memory adapter.
   *
   * `lib/auth-runtime.ts` supplies the console's own `pg.Pool` whenever
   * `CONSOLE_DATABASE_URL` is set, which `lib/env.ts` makes mandatory under
   * `NODE_ENV=production`. Better Auth's `createKyselyAdapter` recognises a
   * `pg.Pool` by its `connect` method and wraps it in Kysely's
   * `PostgresDialect`; no dialect has to be constructed here.
   *
   * WHY IT MATTERS THAT THIS IS DURABLE. The jwt plugin stores its ES256 key
   * pair in this database. With the memory adapter the key pair is regenerated
   * on every process start, so the JWKS document Moira fetched a minute ago
   * stops verifying newly minted tokens until Moira re-fetches — a restart
   * becomes an outage of unbounded length, and two replicas publish two
   * different JWKS documents. `tests/integration/console-jwks-stability.test.ts`
   * demonstrates both halves across genuinely separate processes.
   *
   * The in-memory path remains, and is the default for `bun test` and
   * `next dev`: a durable store that cannot be substituted would make every
   * test that touches auth need a database.
   */
  readonly database?: ConsoleAuthDatabase;
}

export type ConsoleAuth = ReturnType<typeof createConsoleAuth>;

/**
 * Build a Better Auth instance for one resolved provider configuration.
 *
 * Everything provider-shaped comes from `deps.config`, which was resolved from
 * Moira plus the console's own secret store. Nothing here reads `process.env`.
 */
export function createConsoleAuth(deps: ConsoleAuthDeps) {
  const { env, config } = deps;

  // Resolved lazily and only from inside `getSubject`, which cannot run before
  // `betterAuth()` has returned. The alternative — passing the adapter in
  // separately — would let the instance and the adapter it queries drift apart.
  let resolveContext: (value: PromiseLike<ConsoleAuthContext>) => void = () => {};
  const context = new Promise<ConsoleAuthContext>((resolve) => {
    resolveContext = resolve;
  });

  const auth = betterAuth({
    appName: "Moira Console",
    baseURL: env.consoleOrigin,
    basePath: AUTH_BASE_PATH,
    secret: env.betterAuthSecret,
    database: deps.database ?? memoryAdapter(createConsoleMemoryDatabase()),

    // An admin console has exactly one way in: the operator's IdP. A password
    // path would be a second, weaker one that the deployment's own auth policy
    // never sees.
    emailAndPassword: { enabled: false },

    trustedOrigins: [env.consoleOrigin],

    session: {
      // Short, because the console holds admin authority over Moira. The Moira-
      // bound JWT is shorter still (`MOIRA_JWT_LIFETIME`); this is the outer
      // bound on how long a stolen cookie is useful.
      expiresIn: 60 * 60 * 8,
      updateAge: 60 * 15,
    },

    rateLimit: {
      // Better Auth's default storage is `"memory"` (verified in
      // `@better-auth/core`'s `create-context.mjs`: `storage:
      // options.rateLimit?.storage || (secondaryStorage ? ... : "memory")`),
      // and `enabled` defaults to `isProduction`. So the shipped default is a
      // rate limiter that IS on in production and IS per process — which means
      // the effective limit on the sign-in endpoints multiplies by the replica
      // count, and a limit that depends on how many pods happen to be running
      // is not the limit anyone configured.
      //
      // `"database"` puts the counters in the `rateLimit` table, shared by every
      // replica. It costs one row per key per window. On the memory adapter the
      // same table exists as an array — see `createConsoleMemoryDatabase`.
      storage: "database",
    },

    advanced: {
      // `false` only for a local http fixture; `env.allowInsecureUrls` is a hard
      // failure under NODE_ENV=production, so this cannot be false there.
      useSecureCookies: !env.allowInsecureUrls,
      defaultCookieAttributes: {
        httpOnly: true,
        sameSite: "lax",
      },
    },

    plugins: [
      genericOAuth({
        config: [
          {
            providerId: config.providerId,
            clientId: config.clientId,
            clientSecret: config.clientSecret,
            ...(config.discoveryUrl === null ? {} : { discoveryUrl: config.discoveryUrl }),
            ...(config.issuer === null ? {} : { issuer: config.issuer }),
            ...(config.authorizationUrl === null
              ? {}
              : { authorizationUrl: config.authorizationUrl }),
            ...(config.tokenUrl === null ? {} : { tokenUrl: config.tokenUrl }),
            ...(config.userInfoUrl === null ? {} : { userInfoUrl: config.userInfoUrl }),
            scopes: [...config.scopes],
            // PKCE unconditionally. The console is a confidential client, so
            // PKCE is not strictly required — but it costs nothing and it closes
            // authorization-code interception through a compromised redirect.
            pkce: true,
          },
        ],
      }),

      jwt({
        jwks: {
          keyPairConfig: { alg: MOIRA_JWT_ALGORITHM },
          jwksPath: AUTH_JWKS_PATH,
        },
        jwt: {
          issuer: env.bffIssuerUrl,
          audience: env.adminApiAudience,
          expirationTime: MOIRA_JWT_LIFETIME,
          getSubject: async (session) => {
            // Refusing to mint is the safe failure. Better Auth's default here
            // is `session.user.id`, which would produce a token that verifies
            // against the console's JWKS and then matches no `admin_identities`
            // grant — a 403 from Moira with nothing locally to explain it.
            const idpSubject = await readIdpSubject(
              await context,
              config.providerId,
              session.user.id,
            );
            // ----------------------------------------------------------------
            // FINDING F25 — the credential boundary, and the ONLY place the
            // console's own allow-list is load-bearing.
            // ----------------------------------------------------------------
            //
            // `checkSession` shipped in wave 3 with eleven green unit
            // assertions and no shipped caller at all. This is the call site.
            //
            // It is HERE and not in a page because this function is what every
            // minted token passes through — the browser's `GET /api/auth/token`
            // and every server-side `mintMoiraToken` alike. A page-level gate
            // would redirect the browser while the token endpoint kept minting
            // for the same cookie, which is a redirect, not a gate.
            //
            // `verify.mjs`/`sign.mjs` give no way to signal a policy refusal, so
            // this throws — the same shape `MissingIdpSubjectError` already uses
            // a line above, and the reason both fail closed. The route handler
            // turns the throw into a keyed 403 so the caller sees a named
            // condition rather than a 500.
            assertAdmissibleSession(
              {
                email: session.user.email,
                emailVerified: session.user.emailVerified,
                idpSubject,
              },
              config,
            );
            return idpSubject;
          },
          definePayload: (session) => ({
            // Non-authoritative, and Moira treats them as such: authority comes
            // from the `admin_identities` grant keyed on (iss, sub), never from
            // a self-asserted claim. These are here so an audit log entry on
            // Moira's side can name a human without a second lookup.
            email: session.user.email,
            email_verified: session.user.emailVerified,
          }),
        },
      }),

      // ----------------------------------------------------------------
      // `nextCookies()` is DELIBERATELY ABSENT. Read before adding it back.
      // ----------------------------------------------------------------
      //
      // That plugin exists for Next.js SERVER ACTIONS: it intercepts the
      // cookies Better Auth wants to set and pushes them into Next's own
      // cookie store, because a server action's return value is not an HTTP
      // response and there is nowhere else to put them.
      //
      // This console mounts Better Auth as a catch-all ROUTE HANDLER
      // (`app/api/auth/[...all]/route.ts`), where the handler's own `Response`
      // carries `Set-Cookie` directly. With `nextCookies()` installed, the
      // interception happens anyway and the headers never reach that response:
      // verified empirically — the sign-in reply came back with NO `Set-Cookie`
      // at all, and the OAuth callback then failed
      // `state_security_mismatch / State not persisted correctly`, which reads
      // as a CSRF-protection bug rather than as a missing cookie.
      //
      // REVERSAL CONDITION: add it back only alongside a server action that
      // calls `auth.api.*` to MUTATE the session (sign-in, sign-out, link).
      // Read-only calls such as `auth.api.getToken` do not need it.
    ],
  });

  resolveContext((auth as unknown as { $context: Promise<ConsoleAuthContext> }).$context);
  return auth;
}

/* -------------------------------------------------------------------------- */
/* The request-level session check                                            */
/* -------------------------------------------------------------------------- */

/**
 * Run [`checkSession`] against the session on this request, resolving the IdP subject the
 * same way `getSubject` does.
 *
 * # One resolution, two surfaces
 *
 * The token route needs the *rejection* so it can answer with a keyed 403; `getSubject`
 * needs the *subject* and throws. Both must reach the same verdict for the same cookie, so
 * neither re-implements the rule: this reads the session and delegates, `getSubject`
 * delegates, and `checkSession` is the only thing that decides.
 *
 * A missing `account` row is folded into the check rather than rethrown, so
 * `idp_subject_missing` reaches the caller as an ordinary rejection with its own key
 * instead of as a `MissingIdpSubjectError` the route would have to special-case.
 */
export async function consoleSessionCheck(
  auth: ConsoleAuth,
  config: ResolvedAuthConfig,
  headers: Headers,
): Promise<SessionCheck> {
  const session = await auth.api.getSession({ headers });
  if (session === null || session === undefined) return checkSession(null, config);

  const context = await (auth as unknown as { $context: Promise<ConsoleAuthContext> }).$context;
  let idpSubject: string | undefined;
  try {
    idpSubject = await readIdpSubject(context, config.providerId, session.user.id);
  } catch (error) {
    if (!(error instanceof MissingIdpSubjectError)) throw error;
  }

  const identity: Partial<ConsoleSessionIdentity> = {
    email: session.user.email,
    emailVerified: session.user.emailVerified,
    ...(idpSubject === undefined ? {} : { idpSubject }),
  };
  return checkSession(identity, config);
}

/* -------------------------------------------------------------------------- */
/* Per-configuration memoisation                                              */
/* -------------------------------------------------------------------------- */

let instance: { readonly cacheKey: string; readonly auth: ConsoleAuth } | undefined;

/**
 * The instance for this configuration, built once.
 *
 * Keyed on `config.cacheKey` so a provider change (or a secret rotation)
 * produces a new instance rather than silently serving the old client id. Only
 * one instance is retained: a console has one interactive provider, and holding
 * a map keyed by digest would keep a rotated client secret alive in memory after
 * it was replaced.
 */
export function getConsoleAuth(deps: ConsoleAuthDeps): ConsoleAuth {
  if (instance !== undefined && instance.cacheKey === deps.config.cacheKey) {
    return instance.auth;
  }
  const auth = createConsoleAuth(deps);
  instance = { cacheKey: deps.config.cacheKey, auth };
  return auth;
}

/** Test seam, and the operator-facing "reload configuration" path. */
export function resetConsoleAuth(): void {
  instance = undefined;
}
