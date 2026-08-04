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
// factory, and `lib/auth-runtime.ts` memoises exactly one instance per configuration
// digest (`AuthConfigsResolution.cacheKey`, which hashes the full set of provider
// and trusted-issuer `(id, version)` pairs plus the newest console secret write).
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
// ============================================================================
// WAVE 4B — N PROVIDERS, N ISSUERS, ONE KEY PAIR
// ============================================================================
//
// The reversal condition recorded in the header above ("if the console ever
// supports linking several IdP accounts to one console operator, `sub` must
// become a console-owned stable id") has FIRED, and the resolution is the
// opposite of what it proposed. `sub` stays the IdP's subject; what changes is
// that `iss` stops being constant.
//
// One human signing in through two providers holds TWO linked `account` rows
// with the same verified email and different subjects, and therefore TWO
// `admin_identities` grants — distinguished by `iss`, which is now the `issuer`
// of the `trusted_jwt_issuers` row each provider is bound to. That is finding
// F24's closure: `admin_identities`' key is `(issuer, subject)`, so two IdPs
// returning the same `sub` string no longer collapse onto one grant.
//
// It buys NO cryptographic separation. There is one ES256 key pair, one `kid`
// and one JWKS URL; a token this console signs with `iss = <github issuer>`
// verifies against the GitHub trusted-issuer row whichever IdP actually
// authenticated the human. **The `iss` selection is the whole security
// boundary**, it is `resolveSessionProvider` below, and guard G8 is what proves
// it names the right provider. "The token verifies against the JWKS" is true in
// the broken arrangement too.
//
// ============================================================================
// HOW THE SESSION KNOWS WHICH PROVIDER AUTHENTICATED IT
// ============================================================================
//
// `dist/oauth2/link-account.mjs::handleOAuthUserInfo` ends with a bare
// `createSession(user.id)` — no provider argument, and `createSession`'s own
// `override` parameter is left undefined. So the provider cannot come from the
// call. It comes from the ambient endpoint context instead:
//
//   dist/api/dispatch.mjs::dispatchAuthEndpoint
//     -> runWithEndpointContext(internalContext, ...)      [AsyncLocalStorage]
//   dist/db/with-hooks.mjs::createWithHooks
//     -> const context = await getCurrentAuthContext()
//     -> hooks.session.create.before(data, context)
//
// and `internalContext` is the router's input, which better-call populated with
// `params` from the matched route `/oauth2/callback/:providerId`. Better Auth's
// own `lastLoginMethod` plugin reads the same parameter from the same hook, so
// this is in-library precedent rather than a private-API bet. Established by
// observation in `plans/reports/W4-T0-SPIKE.md`, not from documentation.
import "server-only";

import { betterAuth } from "better-auth";
import { genericOAuth } from "better-auth/plugins/generic-oauth";
import { jwt, type JwtOptions } from "better-auth/plugins/jwt";
import { memoryAdapter } from "better-auth/adapters/memory";

import { AUTH_BASE_PATH, AUTH_JWKS_PATH, type ConsoleEnv } from "./env";
import type { ResolvedAuthConfig } from "./auth-config";
import { signableJwksAdapter } from "./jwks-signable";
import {
  assertAdmissibleSession,
  checkSession,
  rejectedSession,
  SessionNotAdmissibleError,
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

/**
 * The jwt plugin's `jwks` options, hoisted to a named constant.
 *
 * ============================================================================
 * WHY THIS IS NOT INLINE (finding F17)
 * ============================================================================
 *
 * `signableJwksAdapter` — the mechanism that stops the console publishing a key
 * it cannot sign with — has to know whether the private half is encrypted,
 * because `plugins/jwt/sign.mjs` decides that from
 * `!options?.jwks?.disablePrivateKeyEncryption` and a check that disagreed with
 * the signer in the PERMISSIVE direction would be F17 wearing a guard's name.
 *
 * So the flag is read from the same object the plugin is configured with,
 * rather than restated. `disablePrivateKeyEncryption` is deliberately absent
 * here (encryption ON) and turning it on would flow through to the check on the
 * same commit.
 */
const JWKS_OPTIONS = {
  keyPairConfig: { alg: MOIRA_JWT_ALGORITHM },
  jwksPath: AUTH_JWKS_PATH,
} as const satisfies JwtOptions["jwks"];

/**
 * The session column that records which provider authenticated the session.
 *
 * Nullable, and never settable through an API surface — see the `session`
 * options below for why both halves matter.
 */
export const SESSION_PROVIDER_FIELD = "providerId" as const;

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
 * The IdP's stable subject for a console user, on ONE provider.
 *
 * `account.accountId` is Better Auth's own record of the provider's `sub`. The
 * `providerId` filter was defensive before 4B and is load-bearing now: one human
 * can hold two linked accounts with different subjects, and the caller must pass
 * the provider that authenticated THIS session — never "the first account",
 * never "the most recently updated one". Both of those name provider A while
 * `iss` names provider B, which is a token that verifies, names a real human,
 * and resolves the wrong grant.
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

/* -------------------------------------------------------------------------- */
/* Which provider authenticated this session                                  */
/* -------------------------------------------------------------------------- */

/**
 * The session carries no authenticating provider.
 *
 * Every session created before the `providerId` column shipped is in this state,
 * because `console/db/migrations/0003` adds it nullable — Better Auth's own
 * schema compiler chose that, and a NOT NULL column would have refused the
 * sessions live at deploy time.
 *
 * It THROWS rather than returning a default, and that is not stylistic:
 * `dist/plugins/jwt/sign.mjs` spells the subject
 * `await getSubject(...) ?? ctx.context.session.user.id`, so a `getSubject` that
 * returns null or undefined silently falls back to the console's OWN user id —
 * a token that verifies against the console's JWKS and matches no grant. A
 * refusal that returns is not a refusal.
 */
export class UnknownSessionProviderError extends Error {
  constructor(sessionId: string) {
    super(
      `console session ${sessionId} carries no authenticating providerId. ` +
        "Refusing to mint a Moira-bound JWT: the `iss` claim selects which trusted_jwt_issuers " +
        "row — and therefore which admin_identities grant namespace — the token is redeemed " +
        "against, so defaulting it would authorise the session against a provider that did not " +
        "authenticate it.",
    );
    this.name = "UnknownSessionProviderError";
  }
}

/** The stamped provider is not in the resolved configuration. */
export class UnresolvableSessionProviderError extends Error {
  constructor(providerId: string) {
    super(
      `console session names provider ${providerId}, which is not in the resolved configuration. ` +
        "Refusing to mint rather than fall back to another provider's issuer: an operator who " +
        "disabled a provider must not have their live sessions silently re-pointed at a " +
        "different admin_identities grant namespace.",
    );
    this.name = "UnresolvableSessionProviderError";
  }
}

/** The slice of Better Auth's session payload the provider lookup reads. */
interface SessionWithProvider {
  readonly session: { readonly id: string } & Record<string, unknown>;
  readonly user: { readonly id: string; readonly email: string; readonly emailVerified: boolean };
}

/** The stamped provider id, or a refusal. Never a guess. */
export function sessionProviderId(session: SessionWithProvider): string {
  const stamped = session.session[SESSION_PROVIDER_FIELD];
  if (typeof stamped !== "string" || stamped === "") {
    throw new UnknownSessionProviderError(session.session.id);
  }
  return stamped;
}

/* -------------------------------------------------------------------------- */
/* GitHub's non-OIDC profile                                                  */
/* -------------------------------------------------------------------------- */

/**
 * What Better Auth's `getUserInfo` override has to return.
 *
 * Structurally the library's `OAuth2UserInfo` (`@better-auth/core/oauth2`),
 * declared here rather than imported so the console's build does not depend on
 * a deep subpath of a transitive package. The one narrowing that matters is
 * `id: string`: `OAuth2UserInfo` permits `string | number` and GitHub's is a
 * number, so stringifying it ONCE — in `githubProfile`, not wherever it is next
 * compared — is what keeps `account.accountId` and Moira's `subject` the same
 * text. Two spellings of one subject are two different grants.
 */
interface OAuthProfile {
  readonly id: string;
  readonly email?: string | undefined;
  readonly emailVerified: boolean;
  readonly name: string;
  readonly image?: string | undefined;
}

interface GithubEmail {
  readonly email?: unknown;
  readonly primary?: unknown;
  readonly verified?: unknown;
}

/**
 * GitHub's profile, assembled from the two endpoints it actually has.
 *
 * ============================================================================
 * WHY THIS OVERRIDE EXISTS AT ALL
 * ============================================================================
 *
 * `generic-oauth/routes.mjs::getUserInfo` reads the `id_token` when there is
 * one and otherwise `GET`s `userInfoUrl` and takes `profile.email_verified ??
 * false`. GitHub issues no `id_token` and `GET /user` has no `email_verified`
 * field at all, so without this override every GitHub sign-in produces
 * `emailVerified: false` and is refused at the session boundary — a provider
 * that is configured, offers a button, and can never complete.
 *
 * ============================================================================
 * AND WHY AN UNVERIFIED ADDRESS IS DROPPED RATHER THAN PASSED THROUGH
 * ============================================================================
 *
 * `GET /user`'s `email` is the account's PUBLIC profile email, which GitHub does
 * not require to be verified and which a user can set to anything. Passing it on
 * would let an attacker who controls a GitHub account claim any address —
 * including one an existing console admin already holds, which Better Auth's
 * implicit account linking would then link onto that admin's user row. So the
 * address comes from `GET /user/emails` and only when that entry is BOTH
 * `primary` and `verified`; anything else yields a profile with no email, which
 * Better Auth refuses with `email_is_missing` before any row is written.
 *
 * The unverified addresses are dropped rather than carried, because the refusal
 * path logs the profile object.
 *
 * UNVERIFIED AGAINST THE REAL PROVIDER. No GitHub client id or secret exists for
 * this project and none is requested. This is exercised against
 * `tests/support/mock-github.ts`, which serves GitHub's shapes — no discovery
 * document, no `id_token`, `/user` plus `/user/emails` — and nothing here
 * pretends that is the same as having driven github.com.
 */
export async function githubProfile(
  accessToken: string,
  userInfoUrl: string,
  fetchImpl: typeof fetch = fetch,
): Promise<OAuthProfile | null> {
  const headers = {
    authorization: `Bearer ${accessToken}`,
    accept: "application/vnd.github+json",
    "user-agent": "moira-console",
  };

  const userResponse = await fetchImpl(userInfoUrl, { headers });
  if (!userResponse.ok) return null;
  const user = (await userResponse.json()) as {
    id?: unknown;
    login?: unknown;
    name?: unknown;
    avatar_url?: unknown;
  };
  // GitHub's `id` is a NUMBER. `account.accountId` is text and Moira's `subject`
  // is text, so it is stringified once, here, rather than wherever it is next
  // compared — two spellings of the same subject are two different grants.
  const id = typeof user.id === "number" ? String(user.id) : typeof user.id === "string" ? user.id : "";
  if (id === "") return null;
  const login = typeof user.login === "string" ? user.login : "";
  const name = typeof user.name === "string" && user.name !== "" ? user.name : login;
  if (name === "") return null;

  // `/user/emails` needs the `user:email` scope. Without it GitHub answers 403,
  // and the sign-in refuses rather than falling back to the public profile
  // address — see the header.
  const emailsResponse = await fetchImpl(`${trimTrailingSlash(userInfoUrl)}/emails`, { headers });
  const verifiedPrimary = emailsResponse.ok
    ? ((await emailsResponse.json()) as GithubEmail[]).find(
        (entry) =>
          entry.primary === true && entry.verified === true && typeof entry.email === "string",
      )
    : undefined;

  const email = verifiedPrimary === undefined ? undefined : String(verifiedPrimary.email);
  return {
    id,
    ...(email === undefined ? {} : { email }),
    emailVerified: email !== undefined,
    name,
    ...(typeof user.avatar_url === "string" ? { image: user.avatar_url } : {}),
  };
}

function trimTrailingSlash(value: string): string {
  return value.endsWith("/") ? value.slice(0, -1) : value;
}

/* -------------------------------------------------------------------------- */
/* The instance                                                               */
/* -------------------------------------------------------------------------- */

export interface ConsoleAuthDeps {
  readonly env: ConsoleEnv;
  /**
   * Every provider a human may sign in through, resolved from Moira plus the
   * console's own secret store. Never empty — `resolveAuthConfigs` reports a
   * refusal instead of an empty list.
   */
  readonly configs: readonly ResolvedAuthConfig[];
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
 * Build a Better Auth instance for N resolved provider configurations.
 *
 * Everything provider-shaped comes from `deps.configs`, which was resolved from
 * Moira plus the console's own secret store. Nothing here reads `process.env`.
 */
export function createConsoleAuth(deps: ConsoleAuthDeps) {
  const { env, configs } = deps;
  if (configs.length === 0) {
    throw new Error(
      "createConsoleAuth was given no providers. A Better Auth instance with an empty " +
        "`genericOAuth` config offers no sign-in and answers 400 provider_config_not_found on " +
        "every attempt; `resolveAuthConfigs` reports a keyed refusal instead, and the caller " +
        "must surface that rather than build an instance out of nothing.",
    );
  }

  /**
   * The configuration that authenticated this session, or a refusal.
   *
   * The SAME function `consoleSessionCheck` uses, deliberately: the minter and
   * the token route must not be able to disagree about which provider a cookie
   * belongs to, and two spellings of "which provider" is how they would.
   */
  const resolveSessionProvider = (session: SessionWithProvider): ResolvedAuthConfig =>
    resolveSessionProviderIn(configs, session);

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

      // --------------------------------------------------------------------
      // THE COLUMN (wave 4B). `console/db/migrations/0003`.
      // --------------------------------------------------------------------
      //
      // `required: false` because every session that predates 4B has no value,
      // and a required column would make the migration refuse them.
      //
      // `input: false` because this must never be settable through an API
      // surface: it selects the minted `iss`, so a settable field would let a
      // signed-in operator re-point their own token at another provider's
      // `admin_identities` namespace. Verified that it does NOT block the
      // internal write — `@better-auth/core`'s `transformInput` copies schema
      // fields straight through and never consults `input`, while
      // `dist/db/schema.mjs::parseInputData`, the function that enforces
      // `input: false` by throwing `<key> is not allowed to be set`, is only
      // reached from request-shaped data.
      additionalFields: {
        [SESSION_PROVIDER_FIELD]: { type: "string", required: false, input: false },
      },
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

    // ------------------------------------------------------------------------
    // THE STAMP (wave 4B).
    // ------------------------------------------------------------------------
    //
    // `create.before`, not `create.after`: the `after` hook is queued through
    // `queueAfterTransactionHook`, which would leave a window in which a session
    // exists with a NULL provider and the minter would refuse a session that is
    // about to become valid. `before` writes the column in the same insert.
    //
    // `ctx.params`, NEVER `ctx.path`. `dispatchAuthEndpoint` sets
    // `path: endpoint.path` — the route TEMPLATE — so `path.split("/").pop()`
    // yields the literal `":providerId"`. Better Auth's own `lastLoginMethod`
    // plugin has that fallback; this console must not copy it.
    databaseHooks: {
      session: {
        create: {
          async before(data, ctx) {
            const params = (ctx?.params ?? null) as Record<string, unknown> | null;
            const providerId = params?.[SESSION_PROVIDER_FIELD];
            // A session created outside an OAuth callback keeps a NULL column,
            // and `sessionProviderId` then refuses to mint from it. That is the
            // safe failure: there is nothing to guess from.
            if (typeof providerId !== "string" || providerId === "") return;
            return { data: { ...data, [SESSION_PROVIDER_FIELD]: providerId } };
          },
        },
      },
    },

    plugins: [
      genericOAuth({
        config: configs.map((config) => ({
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
          // GitHub is not OIDC: no discovery document, no `id_token`, and no
          // `email_verified` anywhere in `GET /user`. See `githubProfile`.
          ...(config.method === "github_oauth" && config.userInfoUrl !== null
            ? {
                getUserInfo: async (tokens: { readonly accessToken?: string | undefined }) =>
                  githubProfile(tokens.accessToken ?? "", config.userInfoUrl ?? ""),
              }
            : {}),
        })),
      }),

      jwt({
        jwks: JWKS_OPTIONS,

        // --------------------------------------------------------------------
        // FINDING F17 — published keys ⊆ signable keys, by construction.
        // --------------------------------------------------------------------
        //
        // `plugins/jwt/adapter.mjs` routes BOTH `getAllKeys` (what the JWKS
        // endpoint publishes) and `getLatestKey` (what `signJWT` signs with)
        // through this ONE override. Filtering here is therefore not two checks
        // that must agree — it is a single read that the document and the
        // signer share, so "the console advertises a key it cannot sign with"
        // has no arrangement in which it is true.
        //
        // Delete this line and the console goes back to publishing an unchanged,
        // 200-ing JWKS after a `BETTER_AUTH_SECRET` rotation while every token
        // it mints is rejected. `tests/integration/console-jwks-stability.test.ts`
        // is what notices.
        adapter: signableJwksAdapter(JWKS_OPTIONS),

        // --------------------------------------------------------------------
        // WHY THIS FLAG IS SET, AND WHAT BREAKS WITHOUT IT
        // --------------------------------------------------------------------
        //
        // `dist/plugins/jwt/index.mjs` registers `hooks.after` with
        // `matcher: context.path === "/get-session"`, whose handler calls
        // `getJwtToken` and sets a `set-auth-jwt` response header. So the minter
        // — and every refusal inside it — runs on an ordinary session READ.
        //
        // From 4B a session that predates the `providerId` column must refuse to
        // mint, and the refusal must throw (see `UnknownSessionProviderError`).
        // Without this flag that throw turns every page load of an un-upgraded
        // session into a 500. Demonstrated both ways in the T0 spike.
        //
        // Nothing in this console consumes `set-auth-jwt`: the browser never
        // holds a Moira credential, and every server-side caller goes through
        // `mintMoiraToken` -> `GET /api/auth/token`. So the header is pure cost.
        disableSettingJwtHeader: true,

        jwt: {
          // --------------------------------------------------------------
          // THE FALLBACK, NOT THE VALUE.
          // --------------------------------------------------------------
          //
          // `dist/plugins/jwt/sign.mjs` reads `const iss = payload.iss;` and
          // signs `.setIssuer(iss ?? defaultIss)`, so `definePayload`'s `iss`
          // wins and this is only reached if it failed to supply one. It is
          // `env.bffIssuerUrl` because the INCUMBENT provider legitimately mints
          // that string — it is the one bound to the `env.bffIssuerUrl` trusted
          // issuer — so an accidental fallback on a single-provider deployment
          // is indistinguishable from correct behaviour. That is precisely why
          // guard G8 asserts on `iss` and not on "the token verifies".
          issuer: env.bffIssuerUrl,
          audience: env.adminApiAudience,
          expirationTime: MOIRA_JWT_LIFETIME,

          getSubject: async (session) => {
            // ONE resolution, shared with `definePayload` below, so the minted
            // `iss` and `sub` cannot name different providers (guard G10). Both
            // read the SAME session object through the same function; neither
            // re-derives the provider and neither takes "the first account".
            const config = resolveSessionProvider(session as unknown as SessionWithProvider);
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
            // The allow-list is the AUTHENTICATING provider's, not "the"
            // console's: from 4B each provider owns its own trusted-issuer row
            // and therefore its own `admission_policy` lookup in Moira, so
            // enforcing another provider's list here would disagree with the
            // server that decides the claim.
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

          definePayload: (session) => {
            const config = resolveSessionProvider(session as unknown as SessionWithProvider);
            return {
              // THE SECURITY BOUNDARY (F24). This selects which
              // `trusted_jwt_issuers` row Moira verifies against and therefore
              // which `admin_identities` grant namespace the token resolves in.
              // It is the `issuer` string of the row THIS provider is bound to,
              // read from Moira — never derived from a provider row id, which
              // changes when an operator recreates a provider.
              iss: config.consoleIssuer,
              // Non-authoritative, and Moira treats them as such: authority comes
              // from the `admin_identities` grant keyed on (iss, sub), never from
              // a self-asserted claim. These are here so an audit log entry on
              // Moira's side can name a human without a second lookup.
              email: session.user.email,
              email_verified: session.user.emailVerified,
            };
          },
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
 * Run [`checkSession`] against the session on this request, resolving the
 * authenticating provider and the IdP subject the same way `getSubject` does.
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
 * instead of as a `MissingIdpSubjectError` the route would have to special-case. The two
 * provider refusals are folded the same way, onto `session_provider_unknown` — a session
 * that predates the column is an ordinary "sign in again", not a fault.
 */
export async function consoleSessionCheck(
  auth: ConsoleAuth,
  configs: readonly ResolvedAuthConfig[],
  headers: Headers,
): Promise<SessionCheck> {
  let session: Awaited<ReturnType<typeof auth.api.getSession>>;
  try {
    session = await auth.api.getSession({ headers });
  } catch (error) {
    // ----------------------------------------------------------------------
    // `getSession` USED TO MINT, and the catch stays. Verified in 1.6.25.
    // ----------------------------------------------------------------------
    //
    // `dist/plugins/jwt/index.mjs` registers an `after` hook whose matcher is
    // `context.path === "/get-session"`, and it calls `getJwtToken` to populate a
    // `set-auth-jwt` response header. `disableSettingJwtHeader: true` in
    // `createConsoleAuth` turns that off — it has to, or a refusal that throws would
    // 500 an ordinary session read — but this catch is kept deliberately: it costs
    // nothing, and without it a future change that clears the flag would turn every
    // refusal on this path into a 500 with no local explanation.
    // The row is carried back too: `checkSession` named it on the refusal, and
    // a caller that reads the verdict here must see the same value one that
    // read it as a return would have.
    if (error instanceof SessionNotAdmissibleError) {
      return rejectedSession(error.rejection, error.resolvedProviderId);
    }
    if (error instanceof MissingIdpSubjectError) return rejectedSession("idp_subject_missing");
    if (
      error instanceof UnknownSessionProviderError ||
      error instanceof UnresolvableSessionProviderError
    ) {
      return rejectedSession("session_provider_unknown");
    }
    throw error;
  }
  // `rejectedSession`, not `checkSession(null, …)`: with no session there is no
  // authenticating configuration to answer from, and passing an invented one
  // just to satisfy the parameter would be inventing the very field
  // `SessionCheck.consoleIssuer` exists to make trustworthy. The verdict is the
  // same one `checkSession` returns for a null session.
  if (session === null || session === undefined) return rejectedSession("no_session");

  let config: ResolvedAuthConfig;
  try {
    config = resolveSessionProviderIn(configs, session as unknown as SessionWithProvider);
  } catch (error) {
    if (
      error instanceof UnknownSessionProviderError ||
      error instanceof UnresolvableSessionProviderError
    ) {
      return rejectedSession("session_provider_unknown");
    }
    throw error;
  }

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

/**
 * The configuration that authenticated a session, for callers outside the
 * instance.
 *
 * Deliberately the same two refusals `createConsoleAuth`'s private copy raises,
 * from the same `sessionProviderId`, so the token route and the minter cannot
 * disagree about which provider a cookie belongs to.
 */
export function resolveSessionProviderIn(
  configs: readonly ResolvedAuthConfig[],
  session: SessionWithProvider,
): ResolvedAuthConfig {
  const providerId = sessionProviderId(session);
  const config = configs.find((candidate) => candidate.providerId === providerId);
  if (config === undefined) throw new UnresolvableSessionProviderError(providerId);
  return config;
}

/* -------------------------------------------------------------------------- */
/* Memoisation lives in `auth-runtime.ts`, not here                           */
/* -------------------------------------------------------------------------- */

// `getConsoleAuth`/`resetConsoleAuth` used to memoise an instance in this module
// as well, keyed on `ResolvedAuthConfig.cacheKey`. They had no caller: the
// shipped path is `lib/auth-runtime.ts`, which holds the snapshot, the secret
// store and its own `cachedAuth` keyed on the same digest. Two caches of the
// same thing, one of them unreachable, is how a rotated client secret survives
// in memory after the other cache dropped it — so this one is deleted rather
// than ported to the 4B shape.
