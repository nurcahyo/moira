// @server-only
//
// The bridge between "there is a console session" and "there is a Moira-bound
// credential".
//
// ============================================================================
// THE PLANE RESTRICTION THIS DEPENDS ON (plan 07 decision D2 — state it, do not
// inherit it silently)
// ============================================================================
//
// `authenticate_admin` and `authenticate_caller` both delegate to the same
// `authenticate_trusted_jwt`, but ONLY `authenticate_admin` applies the
// `admin_identities` grant union (`src/security/auth.rs:334`);
// `authenticate_caller` returns the trusted-JWT actor verbatim. So the token
// this module mints carries `moira:admin` on `/api/v1/admin/*` AND NOWHERE ELSE.
// Presented to the public execution API it resolves to exactly whatever the JWT
// independently claims — which, for this console's issuer, is nothing, because
// the console registers its trusted JWT issuer WITHOUT a `scopes_claim`.
//
// That is a security property the console relies on, not a coincidence. This
// console makes zero non-admin API calls; `assertAdminPlanePath` below keeps it
// that way, so nobody adds one under the impression that the same token confers
// the same authority there.
import "server-only";

import { MoiraClient } from "./moira-client";
import type { ConsoleEnv } from "./env";
import { isEmailDomainAllowed, type ResolvedAuthConfig } from "./auth-config";
import { CONSOLE_MESSAGE_KEYS } from "./i18n/keys";

/* -------------------------------------------------------------------------- */
/* The session as this module needs it                                        */
/* -------------------------------------------------------------------------- */

/**
 * The narrow projection of a Better Auth session this module consumes.
 *
 * Structural on purpose: it keeps this module testable without constructing a
 * whole Better Auth instance, and it makes the set of session fields that
 * influence a Moira credential explicit and small.
 */
export interface ConsoleSessionIdentity {
  readonly email: string;
  readonly emailVerified: boolean;
  /** The IdP's stable subject — the `sub` the console mints. */
  readonly idpSubject: string;
}

/** Why a session may not be exchanged for a Moira credential. */
export type SessionRejection =
  /** No session at all. */
  | "no_session"
  /** The IdP did not verify the address. Moira refuses the claim anyway (403). */
  | "email_not_verified"
  /** The address is outside `allowed_email_domains`. */
  | "email_domain_not_allowed"
  /** The IdP supplied no `sub`, so no `account.accountId` was recorded. */
  | "idp_subject_missing"
  /**
   * The session carries no authenticating provider, or one this configuration
   * does not contain (wave 4B). Every session live at deploy time is in the
   * first state, because the column is nullable.
   */
  | "session_provider_unknown";

export const SESSION_REJECTION_MESSAGE_KEYS: Readonly<Record<SessionRejection, string>> = {
  no_session: CONSOLE_MESSAGE_KEYS.session_required,
  email_not_verified: CONSOLE_MESSAGE_KEYS.email_not_verified,
  email_domain_not_allowed: CONSOLE_MESSAGE_KEYS.email_domain_not_allowed,
  idp_subject_missing: CONSOLE_MESSAGE_KEYS.idp_subject_missing,
  session_provider_unknown: CONSOLE_MESSAGE_KEYS.session_provider_unknown,
};

export type SessionCheck =
  | {
      readonly ok: true;
      readonly identity: ConsoleSessionIdentity;
      /**
       * The CONSOLE ISSUER of the provider this session was established
       * through — `ResolvedAuthConfig.consoleIssuer`, never a caller's string.
       *
       * Carried on the verdict because a caller that wants to act in an
       * `admin_identities` NAMESPACE has to be able to prove the session
       * belongs to it, and only the resolution knows which one that is. The
       * setup window's claim step is the first such caller: its namespace comes
       * from a request-body slug, and without this it had nothing to check that
       * slug against — signing in through provider A and posting provider B's
       * slug would have granted admin in B's namespace.
       */
      readonly consoleIssuer: string;
    }
  | { readonly ok: false; readonly rejection: SessionRejection; readonly messageKey: string };

function reject(rejection: SessionRejection): SessionCheck {
  return { ok: false, rejection, messageKey: SESSION_REJECTION_MESSAGE_KEYS[rejection] };
}

/**
 * A refusal as a value, for a caller that learned the verdict by catching it.
 *
 * The jwt plugin mints a token on `/get-session` as well as on `/token`
 * (`better-auth/dist/plugins/jwt/index.mjs`: `matcher: context.path === "/get-session"`,
 * then `ctx.setHeader("set-auth-jwt", jwt)`), so `jwt.getSubject` runs on a *read* too and
 * its refusal arrives as a thrown [`SessionNotAdmissibleError`]. That is fail-closed and
 * correct — but a caller holding the verdict as an exception needs to put it back into the
 * shape every other caller uses, and must not re-derive it, or two spellings of the same
 * rule appear.
 */
export function rejectedSession(rejection: SessionRejection): SessionCheck {
  return reject(rejection);
}

/**
 * Decide whether a session may act against Moira.
 *
 * Runs the SAME allow-list Moira applies at claim time. Duplicating the check is
 * deliberate: Moira's copy governs the claim and the grant, this one governs the
 * console session, and a deployment where the two disagree is a deployment where
 * somebody can hold a console session they can do nothing with — which reads to
 * them as a broken console rather than as a denied identity.
 */
export function checkSession(
  session: Partial<ConsoleSessionIdentity> | null | undefined,
  config: Pick<ResolvedAuthConfig, "allowedEmailDomains" | "consoleIssuer">,
): SessionCheck {
  if (session === null || session === undefined) return reject("no_session");
  const { email, emailVerified, idpSubject } = session;
  if (typeof email !== "string" || email === "") return reject("no_session");
  if (emailVerified !== true) return reject("email_not_verified");
  if (!isEmailDomainAllowed(email, config.allowedEmailDomains)) {
    return reject("email_domain_not_allowed");
  }
  if (typeof idpSubject !== "string" || idpSubject === "") return reject("idp_subject_missing");
  return {
    ok: true,
    identity: { email, emailVerified: true, idpSubject },
    // Taken from the configuration that AUTHENTICATED this session, never from
    // anything the caller sent — see the field's own note on `SessionCheck`.
    consoleIssuer: config.consoleIssuer,
  };
}

/**
 * A session that exists but may not be exchanged for a Moira-bound credential.
 *
 * Carries the machine-readable `rejection` and its i18n key so the surfaces that catch it —
 * the token route and the console layout — render a named condition rather than "something
 * went wrong". Never the email address: this error reaches a log, and the log is not the
 * place to accumulate the addresses of people who failed the allow-list.
 */
export class SessionNotAdmissibleError extends Error {
  readonly rejection: SessionRejection;
  readonly messageKey: string;

  constructor(rejection: SessionRejection, messageKey: string) {
    super(
      `the console session is not admissible (${rejection}). Refusing to mint a Moira-bound ` +
        "JWT: Moira would refuse the claim anyway, and a token issued here would be a " +
        "credential the holder can do nothing with.",
    );
    this.name = "SessionNotAdmissibleError";
    this.rejection = rejection;
    this.messageKey = messageKey;
  }
}

/**
 * [`checkSession`], as a guard rather than a value — **finding F25's fix**.
 *
 * # Why this exists at all
 *
 * `checkSession` shipped with eleven green unit assertions and **no shipped caller**. Every
 * reference outside its own definition was a test or an i18n catalog *description*. It is
 * the console's session-boundary gate and the only caller of `isEmailDomainAllowed`, and it
 * had never once run against a real session. That is the exact shape of the project's
 * standing rule — *a guard nobody has seen fail is an assumption* — with the additional
 * twist that this one was never even reached.
 *
 * # Where it is wired, and why not only on the page
 *
 * The call site that matters is `jwt.getSubject` in `lib/auth.ts`: it is the single
 * function every minted token passes through, it already fails closed by throwing, and a
 * token is the only thing a console session can be turned into that Moira will look at.
 * Wiring the check into a page layout instead would let the *page* redirect while the
 * `/api/auth/token` endpoint kept minting — the browser would be refused and a `curl` with
 * the same cookie would not. The layout gate exists too, for the UX half (a human refused
 * at sign-in rather than by a bare 403 several screens later), and it delegates here.
 *
 * # This is not a security control
 *
 * Moira remains the enforcer: `evaluate_claim_policy` applies the same `allowed_email_domains`
 * server-side on the claim and the redemption, and it is the copy that governs the grant.
 * This one governs the console session. A deployment where the two disagree is one where
 * somebody holds a console session they can do nothing with — which reads to them as a
 * broken console rather than as a denied identity.
 */
export function assertAdmissibleSession(
  session: Partial<ConsoleSessionIdentity> | null | undefined,
  config: Pick<ResolvedAuthConfig, "allowedEmailDomains" | "consoleIssuer">,
): ConsoleSessionIdentity {
  const check = checkSession(session, config);
  if (!check.ok) throw new SessionNotAdmissibleError(check.rejection, check.messageKey);
  return check.identity;
}

/* -------------------------------------------------------------------------- */
/* Plane restriction                                                          */
/* -------------------------------------------------------------------------- */

export const MOIRA_ADMIN_PLANE_PREFIX = "/api/v1/admin/";

/** A console call outside the admin plane is a bug — see the header note. */
export class MoiraPlaneViolationError extends Error {
  constructor(path: string) {
    super(
      `${path} is outside Moira's admin plane. The console's JWT only carries moira:admin on ` +
        `${MOIRA_ADMIN_PLANE_PREFIX}* — authenticate_caller does not apply the admin_identities ` +
        "grant, so the same token confers no authority here.",
    );
    this.name = "MoiraPlaneViolationError";
  }
}

export function assertAdminPlanePath(path: string): void {
  if (!path.startsWith(MOIRA_ADMIN_PLANE_PREFIX)) throw new MoiraPlaneViolationError(path);
}

/* -------------------------------------------------------------------------- */
/* Token minting                                                              */
/* -------------------------------------------------------------------------- */

/**
 * The Better Auth surface this module needs, structurally.
 *
 * Avoids importing `lib/auth.ts` — which would drag the whole plugin graph into
 * every module that merely wants a Moira client — and keeps the dependency
 * one-directional.
 */
export interface TokenMinter {
  readonly api: {
    getToken(options: { headers: Headers }): Promise<{ token: string }>;
  };
}

/**
 * Mint the short-lived, Moira-bound JWT for the current request.
 *
 * The token is never cached across requests and never written anywhere. It lives
 * for `MOIRA_JWT_LIFETIME` and is re-minted per request; caching it would trade
 * a signature computation for a credential sitting in memory past its use.
 */
export async function mintMoiraToken(auth: TokenMinter, headers: Headers): Promise<string> {
  const { token } = await auth.api.getToken({ headers });
  if (typeof token !== "string" || token === "") {
    throw new Error("the JWT plugin returned no token for an authenticated session");
  }
  return token;
}

/* -------------------------------------------------------------------------- */
/* Client construction                                                        */
/* -------------------------------------------------------------------------- */

/** Optional per-call overrides. `fetch` is the same test seam `MoiraClient` exposes. */
export interface MoiraClientOverrides {
  /** Per-request correlation id, sent as `X-Request-Id`. */
  readonly requestId?: (() => string) | undefined;
  /** Injectable transport. Tests only; shipped call sites omit it. */
  readonly fetch?: typeof fetch | undefined;
}

/**
 * A `MoiraClient` that authenticates as the signed-in operator.
 *
 * NOTE the omission: no `systemKey`. `MoiraClient` prefers the system key over
 * the bearer token when both are present, so passing it here would silently
 * authenticate every admin call as the bootstrap key instead of as the human —
 * defeating the audit trail and making the `admin_identities` grant untested in
 * practice. The system key belongs to the setup wizard alone.
 */
export function moiraClientForSession(
  env: ConsoleEnv,
  auth: TokenMinter,
  headers: Headers,
  options: MoiraClientOverrides = {},
): MoiraClient {
  return new MoiraClient({
    baseUrl: env.moiraBaseUrl,
    bearerToken: () => mintMoiraToken(auth, headers),
    ...(options.requestId === undefined ? {} : { requestId: options.requestId }),
    ...(options.fetch === undefined ? {} : { fetch: options.fetch }),
  });
}

/**
 * A `MoiraClient` carrying the bootstrap system key.
 *
 * Setup only. Throws when the key is absent, rather than falling back to an
 * unauthenticated client that would fail later with a less useful error.
 */
export function moiraClientForSetup(
  env: ConsoleEnv,
  options: MoiraClientOverrides = {},
): MoiraClient {
  if (env.moiraSystemKey === undefined) {
    throw new Error(
      "MOIRA_SYSTEM_KEY is not set. The setup wizard needs the bootstrap system key; " +
        "POST /api/v1/admin/setup/claim declares systemKeyAuth and nothing else, so a bearer " +
        "token is refused there even if it verifies (401 setup_claim_credential_required).",
    );
  }
  return new MoiraClient({
    baseUrl: env.moiraBaseUrl,
    systemKey: env.moiraSystemKey,
    ...(options.requestId === undefined ? {} : { requestId: options.requestId }),
    ...(options.fetch === undefined ? {} : { fetch: options.fetch }),
  });
}
