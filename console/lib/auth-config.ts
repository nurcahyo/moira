// @server-only
//
// Compose Better Auth's `genericOAuth` provider config from the two stores D7
// splits the configuration across.
//
// ============================================================================
// THE TWO HALVES
// ============================================================================
//
//   Moira  — `auth_provider_settings`: issuer, discovery/authorization/token/
//            userinfo/JWKS URLs, `client_id`, `requested_scopes`,
//            `allowed_email_domains`, `trusted_jwt_issuer_id`, `enabled`,
//            `version`. NON-SECRET ONLY, by design.
//   Console — the OAuth client SECRET for that `client_id`, sealed at rest.
//
// Neither half is usable alone, and either can move without the other. Every
// function in this file is written on the assumption that they HAVE drifted
// until it has checked.
//
// ============================================================================
// THE CACHE KEY (a §0 correction, and it matters)
// ============================================================================
//
// Plan 08's body proposed `${moiraSettingsVersion}:${maxConsoleSecretUpdatedAt}`.
// There is no deployment-wide settings version: `version` on
// `auth_provider_settings` is PER ROW, bumped by that row's own writes. The
// obvious repair — `max(row.version)` — is also wrong, because a max cannot
// observe a row DELETION: delete the newer of two rows and the max goes *down*
// only if the deleted row was the max, and stays identical otherwise, so a
// deleted provider keeps serving from cache.
//
// `authConfigCacheKey` therefore hashes the full sorted set of `(id, version)`
// pairs. Adding, removing, or bumping any row changes the digest.
import "server-only";

import { createHash } from "node:crypto";

import type { ConsoleSecretStore, ConsoleSecretDrift, SealedClientSecret } from "./console-secrets";
import { classifySecretDrift } from "./console-secrets";
import type { MoiraClient } from "./moira-client";
import type { AuthMethod, AuthProviderSettingsRecord } from "./types";

/* -------------------------------------------------------------------------- */
/* Cache key                                                                  */
/* -------------------------------------------------------------------------- */

/**
 * A digest over every fetched provider row's `(id, version)` plus the newest
 * console-side secret write.
 *
 * Sorted before hashing so list ordering cannot change the key, and each pair is
 * length-prefixed so `("ab", 1)` and `("a", "b1")` cannot collide.
 */
export function authConfigCacheKey(
  rows: readonly Pick<AuthProviderSettingsRecord, "id" | "version">[],
  newestSecretUpdatedAt: string | null,
): string {
  const pairs = rows
    .map((row) => `${row.id.length}:${row.id}:${row.version}`)
    .sort((a, b) => (a < b ? -1 : a > b ? 1 : 0));
  return createHash("sha256")
    .update(`v1\n${pairs.join("\n")}\nsecret=${newestSecretUpdatedAt ?? "none"}`, "utf8")
    .digest("base64url");
}

/* -------------------------------------------------------------------------- */
/* Resolution outcome                                                         */
/* -------------------------------------------------------------------------- */

/** Why sign-in cannot be offered. Each maps to a distinct operator remedy. */
export type AuthConfigProblem =
  /** No `auth_provider_settings` row is enabled. Setup has not been completed. */
  | "no_enabled_provider"
  /** More than one is enabled, so which one governs is not determined here. */
  | "ambiguous_enabled_providers"
  /** The enabled row uses `jwks`, which is not an interactive sign-in method. */
  | "method_not_interactive"
  /** The row is missing a URL the OAuth flow cannot proceed without. */
  | "provider_endpoints_incomplete"
  /** The row carries no `allowed_email_domains`, so every claim would be denied. */
  | "allowed_email_domains_empty"
  /** The row is not bound to the console's trusted JWT issuer (the B1 defect). */
  | "provider_not_bound_to_trusted_jwt_issuer"
  /** D7 drift: Moira has the provider, the console cannot supply its secret. */
  | "console_secret_unavailable";

export const AUTH_CONFIG_PROBLEM_MESSAGE_KEYS: Readonly<Record<AuthConfigProblem, string>> = {
  no_enabled_provider: "console.error.no_enabled_auth_provider",
  ambiguous_enabled_providers: "console.error.ambiguous_enabled_auth_providers",
  method_not_interactive: "console.error.auth_method_not_interactive",
  provider_endpoints_incomplete: "console.error.auth_provider_endpoints_incomplete",
  allowed_email_domains_empty: "console.error.allowed_email_domains_empty",
  provider_not_bound_to_trusted_jwt_issuer: "console.error.provider_not_bound_to_trusted_jwt_issuer",
  console_secret_unavailable: "console.error.oauth_client_secret_missing",
};

/** The provider id Better Auth routes on. One console, one interactive provider. */
export const CONSOLE_OAUTH_PROVIDER_ID = "moira-console-idp";

/**
 * Everything needed to construct the `genericOAuth` plugin, plus the policy the
 * console enforces on top of it.
 */
export interface ResolvedAuthConfig {
  readonly providerId: string;
  readonly method: AuthMethod;
  /** The Moira row this was resolved from. */
  readonly moiraProviderId: string;
  readonly moiraProviderVersion: number;
  /** The IdP's issuer, verbatim from Moira. Never the console's. */
  readonly issuer: string | null;
  readonly discoveryUrl: string | null;
  readonly authorizationUrl: string | null;
  readonly tokenUrl: string | null;
  readonly userInfoUrl: string | null;
  readonly clientId: string;
  /** Plaintext. In process memory only, never persisted, never logged. */
  readonly clientSecret: string;
  readonly scopes: readonly string[];
  /** Lower-cased. Deny-by-default; guaranteed non-empty by resolution. */
  readonly allowedEmailDomains: readonly string[];
  readonly trustedJwtIssuerId: string;
  readonly cacheKey: string;
}

export type AuthConfigResolution =
  | { readonly ok: true; readonly config: ResolvedAuthConfig }
  | {
      readonly ok: false;
      readonly problem: AuthConfigProblem;
      readonly messageKey: string;
      /** Populated only for the D7 drift problems. */
      readonly drift?: ConsoleSecretDrift;
    };

function fail(problem: AuthConfigProblem, drift?: ConsoleSecretDrift): AuthConfigResolution {
  return {
    ok: false,
    problem,
    messageKey: AUTH_CONFIG_PROBLEM_MESSAGE_KEYS[problem],
    ...(drift === undefined ? {} : { drift }),
  };
}

/* -------------------------------------------------------------------------- */
/* Endpoint completeness                                                      */
/* -------------------------------------------------------------------------- */

/**
 * Can an OAuth code flow actually be driven from this row?
 *
 * Either a discovery document (from which Better Auth reads the rest) or the
 * authorization + token pair explicitly. Moira's own `validate_method_shape`
 * enforces something similar server-side, but it is method-specific and this
 * console needs the narrower question answered locally before it offers a sign-in
 * button that cannot work.
 */
export function hasUsableEndpoints(row: AuthProviderSettingsRecord): boolean {
  const discovery = row.discovery_url ?? null;
  if (discovery !== null && discovery !== "") return true;
  const authorization = row.authorization_url ?? null;
  const token = row.token_url ?? null;
  return authorization !== null && authorization !== "" && token !== null && token !== "";
}

/** `jwks` is a bearer-token trust method, not a browser sign-in method. */
export function isInteractiveMethod(method: AuthMethod): boolean {
  return method === "google_oauth" || method === "generic_oidc";
}

/* -------------------------------------------------------------------------- */
/* Resolution                                                                 */
/* -------------------------------------------------------------------------- */

/**
 * Pick the governing provider row and marry it to the console-held secret.
 *
 * `rows` is passed in rather than fetched here so the caller owns the Moira call
 * (and its credential), and so this function stays pure enough to test against
 * the exact shapes `docs/openapi.json` describes.
 */
export function resolveAuthConfig(
  rows: readonly AuthProviderSettingsRecord[],
  sealed: SealedClientSecret | null,
  clientSecret: string | null,
  newestSecretUpdatedAt: string | null,
): AuthConfigResolution {
  const cacheKey = authConfigCacheKey(rows, newestSecretUpdatedAt);
  const enabled = rows.filter((row) => row.enabled && row.status === "active");

  if (enabled.length === 0) return fail("no_enabled_provider");
  if (enabled.length > 1) {
    // Moira permits several enabled rows and picks one by a documented ordering
    // at claim time. The console refuses to guess: an operator who enabled two
    // providers gets a determinate error rather than a sign-in button that
    // silently uses whichever row sorted first.
    return fail("ambiguous_enabled_providers");
  }

  const row = enabled[0];
  if (row === undefined) return fail("no_enabled_provider");

  if (!isInteractiveMethod(row.method)) return fail("method_not_interactive");
  if (!hasUsableEndpoints(row)) return fail("provider_endpoints_incomplete");
  if (row.allowed_email_domains.length === 0) return fail("allowed_email_domains_empty");

  const trustedJwtIssuerId = row.trusted_jwt_issuer_id ?? null;
  if (trustedJwtIssuerId === null || trustedJwtIssuerId === "") {
    // The B1 defect, caught on the read path as well as the write path. A row
    // in this state can be signed into but can never produce a successful claim,
    // and the failure would otherwise land as a 403 on the very last step.
    return fail("provider_not_bound_to_trusted_jwt_issuer");
  }

  const drift = classifySecretDrift(row.client_id, sealed);
  if (drift !== "in_sync" || clientSecret === null || clientSecret === "") {
    return fail("console_secret_unavailable", drift);
  }

  return {
    ok: true,
    config: {
      providerId: CONSOLE_OAUTH_PROVIDER_ID,
      method: row.method,
      moiraProviderId: row.id,
      moiraProviderVersion: row.version,
      issuer: row.issuer ?? null,
      discoveryUrl: row.discovery_url ?? null,
      authorizationUrl: row.authorization_url ?? null,
      tokenUrl: row.token_url ?? null,
      userInfoUrl: row.userinfo_url ?? null,
      // Non-null because `classifySecretDrift` returned `in_sync`, which is
      // unreachable when `client_id` is null or empty.
      clientId: row.client_id ?? "",
      clientSecret,
      scopes: [...row.requested_scopes],
      allowedEmailDomains: row.allowed_email_domains.map((domain) => domain.toLowerCase()),
      trustedJwtIssuerId,
      cacheKey,
    },
  };
}

/**
 * Fetch from Moira and resolve, in one call.
 *
 * The list is bounded rather than fully paged: a deployment with more than 100
 * auth providers is already in the `ambiguous_enabled_providers` failure, and
 * paging to find a second enabled row would only change which error is reported.
 */
export async function loadAuthConfig(
  client: MoiraClient,
  store: ConsoleSecretStore,
): Promise<AuthConfigResolution> {
  const response = await client.listAuthProviders({ limit: 100 });
  const rows = response.data;
  const newestSecretUpdatedAt = await store.newestUpdatedAt();

  const enabled = rows.filter((row) => row.enabled && row.status === "active");
  const candidate = enabled.length === 1 ? enabled[0] : undefined;
  const sealed = candidate === undefined ? null : await store.read(candidate.id);
  const secret = candidate === undefined ? null : await store.reveal(candidate.id);

  return resolveAuthConfig(rows, sealed, secret, newestSecretUpdatedAt);
}

/* -------------------------------------------------------------------------- */
/* The allow-list, enforced console-side too                                  */
/* -------------------------------------------------------------------------- */

/**
 * Is this email address permitted to hold a console session?
 *
 * Moira enforces `allowed_email_domains` at claim time and, after that, enforces
 * authority through `admin_identities`. Neither stops a stranger with a valid
 * IdP account from obtaining a *console* session — they would hold no Moira
 * authority, but they would be inside the console shell, and every error the UI
 * renders would be one more thing they can see. Enforcing the same allow-list at
 * the session boundary keeps the two answers identical.
 *
 * Matching is on the domain half, lower-cased, exact — the same comparison
 * Moira makes. Deliberately NOT a suffix match: `evilexample.com` must not pass
 * an allow-list containing `example.com`.
 */
export function isEmailDomainAllowed(
  email: string,
  allowedDomains: readonly string[],
): boolean {
  if (allowedDomains.length === 0) return false; // deny by default
  const at = email.lastIndexOf("@");
  if (at <= 0 || at === email.length - 1) return false;
  const domain = email.slice(at + 1).toLowerCase();
  return allowedDomains.some((allowed) => allowed.toLowerCase() === domain);
}
