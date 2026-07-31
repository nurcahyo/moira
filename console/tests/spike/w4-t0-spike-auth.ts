// ============================================================================
// SPIKE — plan 09 wave 4, task T0. THIS IS NOT SHIPPED CODE.
// ============================================================================
//
// It exists to answer exactly one question, by observation rather than by
// reading the library:
//
//   Can the authenticating account's `providerId` be made available to the
//   token minter for the CURRENT session, in better-auth 1.6.25 as vendored
//   here?
//
// The answer decides whether plan 09 wave 4 Stage 4B (multi-provider sign-in
// with a per-provider minted `iss`) has an honest implementation at all. If it
// does not, the console would mint a token whose `iss` names the wrong
// provider — silently reproducing finding F24, while looking correct.
//
// WHAT THIS FILE IS. A deliberate near-copy of `lib/auth.ts`'s
// `createConsoleAuth`, differing only in the parts 4B would change:
//
//   1. N `genericOAuth` entries instead of one.
//   2. `session.additionalFields.providerId` — a nullable session column.
//   3. `databaseHooks.session.create.before` stamping that column from the
//      endpoint context's route parameter.
//   4. `jwt.definePayload` returning a per-provider `iss`, and `jwt.getSubject`
//      reading the account for the SAME provider, so the pair cannot disagree.
//
// It is a copy rather than an edit of `lib/auth.ts` on purpose: the spike must
// not change shipped behaviour on this branch, and 4B should lift the
// mechanism deliberately rather than inherit it by merge.
//
// WHY A ROUTE PARAMETER IS THE RIGHT SOURCE, and not a guess. better-auth's own
// shipped `lastLoginMethod` plugin does the same thing, in
// `node_modules/better-auth/dist/plugins/last-login-method/index.mjs`:
//
//     if (path.startsWith("/callback/") || path.startsWith("/oauth2/callback/"))
//       return ctx.params?.id || ctx.params?.providerId || path.split("/").pop();
//
// read from a `databaseHooks.session.create.after(session, context)`. So the
// mechanism is in-library precedent, not an invention. What the library's own
// use does NOT establish — and what this spike had to observe — is that the
// parameter is populated at the moment `createSession(user.id)` runs inside
// `dist/oauth2/link-account.mjs`, which passes no provider of its own.

import { betterAuth } from "better-auth";
import { genericOAuth } from "better-auth/plugins/generic-oauth";
import { jwt } from "better-auth/plugins/jwt";
import { memoryAdapter } from "better-auth/adapters/memory";

import {
  createConsoleMemoryDatabase,
  MOIRA_JWT_ALGORITHM,
  MOIRA_JWT_LIFETIME,
  readIdpSubject,
  type ConsoleAuthContext,
  type ConsoleAuthDatabase,
} from "@/lib/auth";
import { AUTH_BASE_PATH, AUTH_JWKS_PATH, type ConsoleEnv } from "@/lib/env";

/* -------------------------------------------------------------------------- */
/* What 4B would carry per provider                                           */
/* -------------------------------------------------------------------------- */

/**
 * One interactive provider, as 4B's `resolveAuthConfigs` would produce it.
 *
 * `consoleIssuer` is the value 4B derives per T7: the `issuer` string of the
 * `trusted_jwt_issuers` row this provider is bound to. It is the console's own
 * issuer, never the IdP's, and it is what makes `admin_identities`'
 * `(issuer, subject)` key distinct per provider — the F24 closure.
 */
export interface SpikeProvider {
  /** Better Auth's `providerId`, and the `account.providerId` column value. */
  readonly providerId: string;
  /** The per-provider console issuer this provider's tokens must carry. */
  readonly consoleIssuer: string;
  readonly clientId: string;
  readonly clientSecret: string;
  readonly discoveryUrl: string;
  readonly scopes: readonly string[];
}

/** The session column this spike adds. Nullable, and never settable by input. */
export const SESSION_PROVIDER_FIELD = "providerId" as const;

/**
 * A session that predates the column, or one created outside an OAuth callback,
 * carries no provider. Minting from it would have to guess, so it refuses.
 *
 * This is the same failure shape as `MissingIdpSubjectError` in `lib/auth.ts`,
 * and for the same reason: a token minted on a guess verifies against the
 * console's JWKS and then authorises whoever the guess named.
 */
export class UnknownSessionProviderError extends Error {
  constructor(sessionId: string) {
    super(
      `console session ${sessionId} carries no authenticating providerId. ` +
        "Refusing to mint a Moira-bound JWT: the `iss` claim selects which " +
        "trusted_jwt_issuers row — and therefore which admin_identities grant " +
        "namespace — the token is redeemed against, so defaulting it would " +
        "authorise the session against a provider that did not authenticate it.",
    );
    this.name = "UnknownSessionProviderError";
  }
}

/** No configured provider matches the one stamped on the session. */
export class UnresolvableSessionProviderError extends Error {
  constructor(providerId: string) {
    super(
      `console session names provider ${providerId}, which is not in the resolved ` +
        "configuration. Refusing to mint rather than fall back to another provider's issuer.",
    );
    this.name = "UnresolvableSessionProviderError";
  }
}

/* -------------------------------------------------------------------------- */
/* Reading the stamp back                                                     */
/* -------------------------------------------------------------------------- */

/** The slice of Better Auth's session payload this spike reads. */
interface SessionWithProvider {
  readonly session: { readonly id: string } & Record<string, unknown>;
  readonly user: { readonly id: string; readonly email: string; readonly emailVerified: boolean };
}

/**
 * The provider that authenticated this session, or a refusal.
 *
 * Deliberately NOT "the account that was updated most recently" and NOT "the
 * first account on the user". Both are guesses, and both are wrong in exactly
 * the case multi-provider exists to serve: one human, two linked accounts.
 */
export function sessionProviderId(session: SessionWithProvider): string {
  const stamped = session.session[SESSION_PROVIDER_FIELD];
  if (typeof stamped !== "string" || stamped === "") {
    throw new UnknownSessionProviderError(session.session.id);
  }
  return stamped;
}

/* -------------------------------------------------------------------------- */
/* The spike instance                                                         */
/* -------------------------------------------------------------------------- */

export interface SpikeAuthDeps {
  readonly env: ConsoleEnv;
  readonly providers: readonly SpikeProvider[];
  readonly database?: ConsoleAuthDatabase;
  /**
   * Turn the stamping hook off, without changing anything else.
   *
   * This is the mutation that must turn the spike's assertions red: with the
   * hook gone the session carries no provider, and the guard table's G8/G10
   * both hinge on the mint refusing rather than silently falling back to
   * `options.jwt.issuer`.
   */
  readonly disableProviderStamp?: boolean;
  /**
   * Stop the jwt plugin from minting a token into every `/get-session` reply.
   *
   * Discovered by this spike rather than assumed: `dist/plugins/jwt/index.mjs`
   * registers `hooks.after` with `matcher: context.path === "/get-session"`,
   * whose handler calls `getJwtToken` and sets a `set-auth-jwt` response
   * header. So `definePayload`/`getSubject` are on the `/get-session` path too,
   * and a refusal that throws there turns an ordinary session read into a 500.
   * Part 3 and part 4 below demonstrate both sides of that.
   */
  readonly disableSettingJwtHeader?: boolean;
  /**
   * Every `(path, params)` pair the session-create hook was handed.
   *
   * This is the raw observation the whole spike turns on. It is recorded rather
   * than asserted inside the hook so a failure reports what was actually seen.
   */
  readonly observations?: SessionCreateObservation[];
}

export interface SessionCreateObservation {
  /** `context.path` — the route TEMPLATE, not the concrete request path. */
  readonly path: string | null;
  /** `context.params`, verbatim. */
  readonly params: Record<string, unknown> | null;
  /** Whether a `GenericEndpointContext` was available at all. */
  readonly hadContext: boolean;
}

export function createSpikeAuth(deps: SpikeAuthDeps) {
  const { env, providers } = deps;
  const observations = deps.observations;

  const byProviderId = new Map(providers.map((provider) => [provider.providerId, provider]));

  /** The per-provider console issuer, or a refusal. Never a default. */
  function consoleIssuerFor(providerId: string): string {
    const provider = byProviderId.get(providerId);
    if (provider === undefined) throw new UnresolvableSessionProviderError(providerId);
    return provider.consoleIssuer;
  }

  let resolveContext: (value: PromiseLike<ConsoleAuthContext>) => void = () => {};
  const context = new Promise<ConsoleAuthContext>((resolve) => {
    resolveContext = resolve;
  });

  const auth = betterAuth({
    appName: "Moira Console (T0 spike)",
    baseURL: env.consoleOrigin,
    basePath: AUTH_BASE_PATH,
    secret: env.betterAuthSecret,
    database: deps.database ?? memoryAdapter(createConsoleMemoryDatabase()),
    emailAndPassword: { enabled: false },
    trustedOrigins: [env.consoleOrigin],

    session: {
      expiresIn: 60 * 60 * 8,
      updateAge: 60 * 15,
      // ------------------------------------------------------------------
      // THE COLUMN.
      // ------------------------------------------------------------------
      // `required: false` because every session that predates 4B has no value,
      // and a required column would make the migration refuse them.
      //
      // `input: false` because this must never be settable through an API
      // surface. Verified that it is still writable by the internal hook:
      // `@better-auth/core/dist/db/adapter/factory.mjs::transformInput` copies
      // schema fields straight through and does not consult `input`, while
      // `better-auth/dist/db/schema.mjs::parseInputData` — the function that
      // DOES enforce `input: false`, by throwing `<key> is not allowed to be
      // set` — is only reached from request-shaped data. So `input: false`
      // closes the "signed-in operator rewrites their own provider, and
      // therefore their own `iss`" path without closing the stamp.
      additionalFields: {
        [SESSION_PROVIDER_FIELD]: {
          type: "string",
          required: false,
          input: false,
        },
      },
    },

    rateLimit: { storage: "database" },

    advanced: {
      useSecureCookies: !env.allowInsecureUrls,
      defaultCookieAttributes: { httpOnly: true, sameSite: "lax" },
    },

    // ----------------------------------------------------------------------
    // THE MECHANISM.
    // ----------------------------------------------------------------------
    // `dist/oauth2/link-account.mjs::handleOAuthUserInfo` ends with a bare
    // `createSession(user.id)` — no provider argument, and `createSession`'s
    // own `override` parameter is left undefined. So the provider cannot come
    // from the call. It comes from the ambient endpoint context instead:
    //
    //   dist/api/dispatch.mjs::dispatchAuthEndpoint
    //     → runWithEndpointContext(internalContext, …)   [AsyncLocalStorage]
    //   dist/db/with-hooks.mjs::createWithHooks
    //     → const context = await getCurrentAuthContext()
    //     → hooks.session.create.before(data, context)
    //
    // and `internalContext` is the router's input, which better-call populated
    // with `params` from the matched route `/oauth2/callback/:providerId`
    // (`better-call/dist/router.mjs`, `params: route.params ? … : {}`).
    databaseHooks: deps.disableProviderStamp
      ? undefined
      : {
          session: {
            create: {
              async before(data, ctx) {
                const params = (ctx?.params ?? null) as Record<string, unknown> | null;
                observations?.push({
                  path: typeof ctx?.path === "string" ? ctx.path : null,
                  params,
                  hadContext: ctx !== null && ctx !== undefined,
                });

                const providerId = params?.[SESSION_PROVIDER_FIELD];
                // No stamp for a session created outside an OAuth callback.
                // Leaving the column null is correct: `sessionProviderId` then
                // refuses to mint, which is the safe failure.
                if (typeof providerId !== "string" || providerId === "") return;

                return { data: { ...data, [SESSION_PROVIDER_FIELD]: providerId } };
              },
            },
          },
        },

    plugins: [
      genericOAuth({
        config: providers.map((provider) => ({
          providerId: provider.providerId,
          clientId: provider.clientId,
          clientSecret: provider.clientSecret,
          discoveryUrl: provider.discoveryUrl,
          scopes: [...provider.scopes],
          pkce: true,
        })),
      }),

      jwt({
        jwks: {
          keyPairConfig: { alg: MOIRA_JWT_ALGORITHM },
          jwksPath: AUTH_JWKS_PATH,
        },
        ...(deps.disableSettingJwtHeader === true ? { disableSettingJwtHeader: true } : {}),
        jwt: {
          // ----------------------------------------------------------------
          // A SENTINEL, ON PURPOSE.
          // ----------------------------------------------------------------
          // `dist/plugins/jwt/sign.mjs` reads `const iss = payload.iss;` and
          // signs `.setIssuer(iss ?? defaultIss)`, where `defaultIss` is this
          // option. So this value is reached ONLY if `definePayload` failed to
          // supply an issuer — which is precisely the G8 mutation. Setting it
          // to something no `trusted_jwt_issuers` row will ever hold means the
          // mutation surfaces as a visibly wrong token rather than as a
          // plausible legacy one.
          //
          // 4B ships `env.bffIssuerUrl` here instead, because the incumbent
          // provider legitimately mints that string (T7: the incumbent is the
          // row bound to the `env.bffIssuerUrl` trusted issuer).
          issuer: "https://spike.invalid/never-mint-this",
          audience: env.adminApiAudience,
          expirationTime: MOIRA_JWT_LIFETIME,

          getSubject: async (session) => {
            // ONE resolution, shared with `definePayload` below, so the minted
            // `iss` and `sub` cannot name different providers (guard G10).
            const providerId = sessionProviderId(session as unknown as SessionWithProvider);
            // Throws rather than returns null on purpose: `getJwtToken` spells
            // this `await getSubject(...) ?? session.user.id`, so a nullish
            // return silently falls back to the console's own user id.
            return readIdpSubject(await context, providerId, session.user.id);
          },

          definePayload: (session) => {
            const providerId = sessionProviderId(session as unknown as SessionWithProvider);
            return {
              iss: consoleIssuerFor(providerId),
              email: session.user.email,
              email_verified: session.user.emailVerified,
            };
          },
        },
      }),
    ],
  });

  resolveContext((auth as unknown as { $context: Promise<ConsoleAuthContext> }).$context);
  return auth;
}

export type SpikeAuth = ReturnType<typeof createSpikeAuth>;
