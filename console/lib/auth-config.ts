// @server-only
//
// Compose Better Auth's `genericOAuth` provider configs from the two stores D7
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
// pairs. Adding, removing, or bumping any row changes the digest. Wave 4B adds
// the TRUSTED ISSUER rows to the same digest, because the console's minted `iss`
// is now read from them: an operator who repoints a provider at a different
// trusted issuer changes which issuer string the tokens carry, and a digest that
// could not see that would keep minting the old one.
//
// ============================================================================
// WAVE 4B — WHERE THE CONSOLE'S MINTED `iss` COMES FROM
// ============================================================================
//
// Before 4B every token carried `env.bffIssuerUrl`, one issuer per console.
// That is finding F24: `admin_identities`' key is `(issuer, subject)` and the
// issuer half was constant, so two IdPs returning the same `sub` string mapped
// to the SAME admin grant.
//
// From 4B the minted `iss` is the `issuer` string of the `trusted_jwt_issuers`
// row the provider is bound to, READ FROM MOIRA. It is never derived from the
// provider row's own id:
//
//   * the row UUID changes when an operator deletes and recreates a provider,
//     which would mint a new issuer string and silently revoke every admin
//     granted under the old one. `trusted_issuer_has_active_grants` (wave 4A)
//     guards the trusted-issuer row against exactly that; nothing guards the
//     provider row;
//   * `trusted_jwt_issuers.issuer` is operator-chosen, carries a unique index,
//     and is the string Moira already pins `Validation::set_issuer` to. It is
//     the stable identifier this derivation needs, one level up from the
//     provider row.
//
// See `consoleProviderIdFor` for the second half: Better Auth's `providerId`,
// which is derived from the same string and is equally load-bearing.
import "server-only";

import { createHash } from "node:crypto";

import type { ConsoleSecretStore, ConsoleSecretDrift, SealedClientSecret } from "./console-secrets";
import { classifySecretDrift } from "./console-secrets";
import { CONSOLE_MESSAGE_KEYS } from "./i18n/keys";
import type { MoiraClient } from "./moira-client";
import type { AuthMethod, AuthProviderSettingsRecord, TrustedJwtIssuerRecord } from "./types";

/* -------------------------------------------------------------------------- */
/* Cache key                                                                  */
/* -------------------------------------------------------------------------- */

/**
 * A digest over every fetched provider row's and trusted-issuer row's
 * `(id, version)` plus the newest console-side secret write.
 *
 * Sorted before hashing so list ordering cannot change the key, and each pair is
 * length-prefixed so `("ab", 1)` and `("a", "b1")` cannot collide.
 */
export function authConfigCacheKey(
  rows: readonly Pick<AuthProviderSettingsRecord, "id" | "version">[],
  issuers: readonly Pick<TrustedJwtIssuerRecord, "id" | "version">[],
  newestSecretUpdatedAt: string | null,
): string {
  const pair = (row: { id: string; version: number }) => `${row.id.length}:${row.id}:${row.version}`;
  const sort = (values: string[]) => values.sort((a, b) => (a < b ? -1 : a > b ? 1 : 0));
  return createHash("sha256")
    .update(
      `v2\nproviders\n${sort(rows.map(pair)).join("\n")}\n` +
        `issuers\n${sort(issuers.map(pair)).join("\n")}\n` +
        `secret=${newestSecretUpdatedAt ?? "none"}`,
      "utf8",
    )
    .digest("base64url");
}

/* -------------------------------------------------------------------------- */
/* Resolution outcome                                                         */
/* -------------------------------------------------------------------------- */

/** Why sign-in cannot be offered. Each maps to a distinct operator remedy. */
export type AuthConfigProblem =
  /** No `auth_provider_settings` row is enabled. Setup has not been completed. */
  | "no_enabled_provider"
  /** More than one is enabled. STILL REFUSED — see `ambiguityGuard` below. */
  | "ambiguous_enabled_providers"
  /** The enabled row uses `jwks`, which is not an interactive sign-in method. */
  | "method_not_interactive"
  /** The row is missing a URL the OAuth flow cannot proceed without. */
  | "provider_endpoints_incomplete"
  /** The row carries no `allowed_email_domains`, so every claim would be denied. */
  | "allowed_email_domains_empty"
  /** The row is not bound to the console's trusted JWT issuer (the B1 defect). */
  | "provider_not_bound_to_trusted_jwt_issuer"
  /**
   * The bound `trusted_jwt_issuers` row cannot supply a console issuer string.
   *
   * Two shapes, one remedy — the operator has to fix the binding:
   *   (a) the id names no active trusted-issuer row this console can read;
   *   (b) the row exists, but its `issuer` string is outside the namespace this
   *       console owns, so no stable Better Auth `providerId` can be derived
   *       from it. See `consoleProviderIdFor`.
   */
  | "trusted_jwt_issuer_not_resolvable"
  /** D7 drift: Moira has the provider, the console cannot supply its secret. */
  | "console_secret_unavailable";

export const AUTH_CONFIG_PROBLEM_MESSAGE_KEYS: Readonly<Record<AuthConfigProblem, string>> = {
  no_enabled_provider: CONSOLE_MESSAGE_KEYS.no_enabled_auth_provider,
  ambiguous_enabled_providers: CONSOLE_MESSAGE_KEYS.ambiguous_enabled_auth_providers,
  method_not_interactive: CONSOLE_MESSAGE_KEYS.auth_method_not_interactive,
  provider_endpoints_incomplete: CONSOLE_MESSAGE_KEYS.auth_provider_endpoints_incomplete,
  allowed_email_domains_empty: CONSOLE_MESSAGE_KEYS.allowed_email_domains_empty,
  provider_not_bound_to_trusted_jwt_issuer:
    CONSOLE_MESSAGE_KEYS.provider_not_bound_to_trusted_jwt_issuer,
  trusted_jwt_issuer_not_resolvable: CONSOLE_MESSAGE_KEYS.trusted_jwt_issuer_not_resolvable,
  console_secret_unavailable: CONSOLE_MESSAGE_KEYS.oauth_client_secret_missing,
};

/* -------------------------------------------------------------------------- */
/* Provider identity — the two strings that must never be regenerated         */
/* -------------------------------------------------------------------------- */

/**
 * The incumbent provider's Better Auth `providerId`.
 *
 * FROZEN. `account.providerId` holds this literal for every admin who has ever
 * signed into this console, `readIdpSubject` filters on it, and it is the last
 * path segment of the redirect URI registered at the IdP. Changing it orphans
 * every account row AND invalidates a URI an operator registered by hand — a
 * total sign-in outage that no console-side code can repair.
 *
 * The incumbent is defined as "the provider bound to the `env.bffIssuerUrl`
 * trusted issuer", so it keeps both this id and that issuer string
 * automatically. There is no alias table and no `is_legacy` flag.
 */
export const CONSOLE_OAUTH_PROVIDER_ID = "moira-console-idp";

/**
 * Where a non-incumbent console issuer string lives, relative to `bffIssuerUrl`.
 *
 * `https://console.example/idp/<slug>`. The path segment is fixed so that the
 * slug is unambiguous in both directions: derive an issuer from a slug, and
 * recover the slug from an issuer, without a second stored column.
 */
export const CONSOLE_ISSUER_PATH_PREFIX = "/idp/";

/**
 * A provider slug: lower-case, hyphen-separated, no leading or trailing hyphen.
 *
 * Deliberately narrow. The slug ends up in a URL path segment (the callback
 * route is `/oauth2/callback/:providerId`) and in `account.providerId`, so a
 * value needing escaping in either place would be a value that could not be
 * changed later.
 */
export const PROVIDER_SLUG_PATTERN = /^[a-z0-9](?:[a-z0-9-]{0,30}[a-z0-9])?$/;

export function isProviderSlug(slug: string): boolean {
  return PROVIDER_SLUG_PATTERN.test(slug);
}

/**
 * The console issuer string an additional provider should be registered under.
 *
 * ============================================================================
 * WHY A SLUG AND NOT THE MOIRA ROW UUID
 * ============================================================================
 *
 * The wave-4 design mandated "an operator-chosen stable slug, not the row UUID",
 * because with a UUID an operator who deletes and recreates a provider mints a
 * NEW issuer string and silently revokes every admin granted under the old one —
 * and `trusted_issuer_has_active_grants` does not cover it, because the trusted
 * issuer row is not what changed.
 *
 * That slug is NOT a new column on `auth_provider_settings`. It is the tail of
 * `trusted_jwt_issuers.issuer`, which is already operator-chosen, already
 * uniquely indexed, already the string Moira pins verification to, and already
 * protected from deletion-while-granted by wave 4A's guard. A second column
 * would have to be kept consistent with it, and the two could disagree.
 *
 * REVERSAL CONDITION: if a deployment ever needs a console issuer string outside
 * `${bffIssuerUrl}${CONSOLE_ISSUER_PATH_PREFIX}*` — a downstream consumer that
 * keys on a fixed issuer, or an operator contract fixing the string — then the
 * slug must become an explicit column on `auth_provider_settings`, with a
 * migration, a uniqueness rule, and a format CHECK. Until then, refusing to
 * derive a provider id from an unrecognised issuer (see `consoleProviderIdFor`)
 * is what keeps the derivation total and injective.
 */
export function consoleIssuerForSlug(bffIssuerUrl: string, slug: string): string {
  if (!isProviderSlug(slug)) {
    throw new Error(
      `"${slug}" is not a usable provider slug (${String(PROVIDER_SLUG_PATTERN)}). It becomes a ` +
        "URL path segment in the OAuth redirect URI and a value in `account.providerId`, " +
        "neither of which can be changed after the first sign-in.",
    );
  }
  return `${trimTrailingSlash(bffIssuerUrl)}${CONSOLE_ISSUER_PATH_PREFIX}${slug}`;
}

function trimTrailingSlash(value: string): string {
  return value.endsWith("/") ? value.slice(0, -1) : value;
}

/**
 * Better Auth's `providerId` for a console issuer string, or `null`.
 *
 * `null` is a refusal, not a default. A console issuer this function does not
 * recognise gets no sign-in button, because the alternative — inventing an id —
 * would produce a value that changes the next time the derivation is touched,
 * and `account.providerId` cannot be migrated once a human has signed in.
 */
export function consoleProviderIdFor(bffIssuerUrl: string, consoleIssuer: string): string | null {
  const base = trimTrailingSlash(bffIssuerUrl);
  if (trimTrailingSlash(consoleIssuer) === base) return CONSOLE_OAUTH_PROVIDER_ID;
  const prefix = `${base}${CONSOLE_ISSUER_PATH_PREFIX}`;
  if (!consoleIssuer.startsWith(prefix)) return null;
  const slug = consoleIssuer.slice(prefix.length);
  if (!isProviderSlug(slug)) return null;
  return `${CONSOLE_OAUTH_PROVIDER_ID}-${slug}`;
}

/* -------------------------------------------------------------------------- */
/* One resolved provider                                                      */
/* -------------------------------------------------------------------------- */

/**
 * Everything needed to construct one `genericOAuth` entry, plus the policy the
 * console enforces on top of it.
 */
export interface ResolvedAuthConfig {
  /** Better Auth's `providerId`, derived from `consoleIssuer`. */
  readonly providerId: string;
  /**
   * The CONSOLE's issuer for this provider — the `iss` its tokens carry, and the
   * `trusted_jwt_issuers.issuer` string Moira pins them to. Never the IdP's.
   */
  readonly consoleIssuer: string;
  readonly trustedJwtIssuerId: string;
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
}

/** One provider row that could not be resolved, and why. */
export interface AuthConfigProviderProblem {
  /** The `auth_provider_settings` row id. */
  readonly moiraProviderId: string;
  readonly problem: AuthConfigProblem;
  readonly messageKey: string;
  /** Populated only for the D7 drift problems. */
  readonly drift?: ConsoleSecretDrift;
}

export type AuthConfigsResolution =
  | {
      readonly ok: true;
      /** Every provider a human can actually sign in through. Never empty. */
      readonly configs: readonly ResolvedAuthConfig[];
      /**
       * Enabled rows that did NOT resolve.
       *
       * Carried alongside rather than promoted to a failure: a drifted GitHub
       * row must not take OIDC sign-in down. Before 4B a single failing row was
       * the whole resolution, because there was only ever one.
       */
      readonly problems: readonly AuthConfigProviderProblem[];
      readonly cacheKey: string;
    }
  | {
      readonly ok: false;
      readonly problem: AuthConfigProblem;
      readonly messageKey: string;
      readonly drift?: ConsoleSecretDrift;
      readonly cacheKey: string;
    };

function failure(
  cacheKey: string,
  problem: AuthConfigProblem,
  drift?: ConsoleSecretDrift,
): AuthConfigsResolution {
  return {
    ok: false,
    problem,
    messageKey: AUTH_CONFIG_PROBLEM_MESSAGE_KEYS[problem],
    ...(drift === undefined ? {} : { drift }),
    cacheKey,
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
 *
 * `github_oauth` can only ever take the second branch: GitHub publishes no
 * discovery document, and `migrations/0020` requires `discovery_url is null` on
 * that method precisely so nothing can pretend otherwise.
 */
export function hasUsableEndpoints(row: AuthProviderSettingsRecord): boolean {
  const discovery = row.discovery_url ?? null;
  if (discovery !== null && discovery !== "") return true;
  const authorization = row.authorization_url ?? null;
  const token = row.token_url ?? null;
  if (authorization === null || authorization === "" || token === null || token === "") return false;
  // GitHub has no `id_token`, so the profile can only come from userinfo. Better
  // Auth's `getUserInfo` returns `null` without one and the callback fails with
  // `user_info_is_missing` — several redirects in, rather than here.
  if (row.method === "github_oauth") {
    const userinfo = row.userinfo_url ?? null;
    if (userinfo === null || userinfo === "") return false;
  }
  return true;
}

/** `jwks` is a bearer-token trust method, not a browser sign-in method. */
export function isInteractiveMethod(method: AuthMethod): boolean {
  return method === "google_oauth" || method === "generic_oidc" || method === "github_oauth";
}

/* -------------------------------------------------------------------------- */
/* Resolution                                                                 */
/* -------------------------------------------------------------------------- */

/** What `resolveAuthConfigs` needs, so the caller owns every fetch. */
export interface AuthConfigsInput {
  readonly rows: readonly AuthProviderSettingsRecord[];
  readonly trustedIssuers: readonly TrustedJwtIssuerRecord[];
  /** `env.bffIssuerUrl` — the incumbent's issuer, and the derivation root. */
  readonly bffIssuerUrl: string;
  /** Sealed record and plaintext per Moira provider row id. */
  readonly sealed: ReadonlyMap<string, SealedClientSecret | null>;
  readonly secrets: ReadonlyMap<string, string | null>;
  readonly newestSecretUpdatedAt: string | null;
}

/**
 * Resolve EVERY enabled provider row independently.
 *
 * ============================================================================
 * WHY THIS RETURNS N AND THE AMBIGUITY GUARD IS A SEPARATE STEP
 * ============================================================================
 *
 * Wave 4B builds the multi-provider capability; it does NOT switch it on. The
 * `ambiguous_enabled_providers` refusal stays until wave 4A's server-side
 * invariant (the partial unique index and its coded 409) is **deployed**, not
 * merely merged — until then the console guard is the only thing standing in
 * front of the ambiguous state, and removing it reopens the window 4A closed.
 *
 * So the two are split: this function is N-capable and per-provider, and
 * `ambiguityGuard` below is the deployment gate applied on top of it by
 * `loadAuthConfigs`. Removing the guard later is deleting one call, not
 * rewriting a resolution.
 */
export function resolveAuthConfigs(input: AuthConfigsInput): AuthConfigsResolution {
  const cacheKey = authConfigCacheKey(
    input.rows,
    input.trustedIssuers,
    input.newestSecretUpdatedAt,
  );
  const enabled = input.rows.filter((row) => row.enabled && row.status === "active");
  if (enabled.length === 0) return failure(cacheKey, "no_enabled_provider");

  const issuersById = new Map(
    input.trustedIssuers
      .filter((issuer) => issuer.status === "active")
      .map((issuer) => [issuer.id, issuer]),
  );

  const configs: ResolvedAuthConfig[] = [];
  const problems: AuthConfigProviderProblem[] = [];

  // Sorted by row id so the button order, the reported problem order, and the
  // failure a caller surfaces are all stable across list pagination order.
  for (const row of [...enabled].sort((a, b) => (a.id < b.id ? -1 : a.id > b.id ? 1 : 0))) {
    const resolved = resolveOne(row, issuersById, input);
    if ("problem" in resolved) problems.push(resolved);
    else configs.push(resolved);
  }

  if (configs.length === 0) {
    // Nothing resolvable. Surface the FIRST row's problem rather than a generic
    // key: on the overwhelmingly common single-provider deployment that is the
    // same message this file emitted before 4B, with the same remedy.
    const first = problems[0];
    if (first === undefined) return failure(cacheKey, "no_enabled_provider");
    return failure(cacheKey, first.problem, first.drift);
  }

  return { ok: true, configs, problems, cacheKey };
}

function resolveOne(
  row: AuthProviderSettingsRecord,
  issuersById: ReadonlyMap<string, TrustedJwtIssuerRecord>,
  input: AuthConfigsInput,
): ResolvedAuthConfig | AuthConfigProviderProblem {
  const problem = (
    kind: AuthConfigProblem,
    drift?: ConsoleSecretDrift,
  ): AuthConfigProviderProblem => ({
    moiraProviderId: row.id,
    problem: kind,
    messageKey: AUTH_CONFIG_PROBLEM_MESSAGE_KEYS[kind],
    ...(drift === undefined ? {} : { drift }),
  });

  if (!isInteractiveMethod(row.method)) return problem("method_not_interactive");
  if (!hasUsableEndpoints(row)) return problem("provider_endpoints_incomplete");
  if (row.allowed_email_domains.length === 0) return problem("allowed_email_domains_empty");

  const trustedJwtIssuerId = row.trusted_jwt_issuer_id ?? null;
  if (trustedJwtIssuerId === null || trustedJwtIssuerId === "") {
    // The B1 defect, caught on the read path as well as the write path. A row
    // in this state can be signed into but can never produce a successful claim,
    // and the failure would otherwise land as a 403 on the very last step.
    return problem("provider_not_bound_to_trusted_jwt_issuer");
  }

  // ------------------------------------------------------------------------
  // The 4B step: the console's own issuer for THIS provider, read from Moira.
  // ------------------------------------------------------------------------
  const trustedIssuer = issuersById.get(trustedJwtIssuerId);
  if (trustedIssuer === undefined) return problem("trusted_jwt_issuer_not_resolvable");
  const providerId = consoleProviderIdFor(input.bffIssuerUrl, trustedIssuer.issuer);
  if (providerId === null) return problem("trusted_jwt_issuer_not_resolvable");

  const drift = classifySecretDrift(row.client_id, input.sealed.get(row.id) ?? null);
  const clientSecret = input.secrets.get(row.id) ?? null;
  if (drift !== "in_sync" || clientSecret === null || clientSecret === "") {
    return problem("console_secret_unavailable", drift);
  }

  return {
    providerId,
    consoleIssuer: trustedIssuer.issuer,
    trustedJwtIssuerId,
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
  };
}

/**
 * The console's ambiguity refusal — **deliberately still standing** (T11).
 *
 * Moira permitted several enabled rows and picked one by a documented ordering
 * at claim time; the console refused to guess. Wave 4A replaced that ordering
 * with a deterministic two-stage lookup and a partial unique index, which is
 * what makes this guard redundant — but only on a deployment that has actually
 * RUN that migration. A merged migration is not a deployed one, and the console
 * cannot tell the difference, so the refusal stays and its removal is a separate
 * change made when 4A is known to be live.
 *
 * REMOVAL CONDITION, stated so it is not re-derived: delete this function and
 * its single call site in `loadAuthConfigs` once wave 4A is deployed. Do NOT
 * "fix" it to count per trusted issuer instead of globally — `migrations/0020`
 * makes two enabled rows on one trusted issuer unrepresentable, so a
 * per-trusted-issuer version could never fire, and a guard that cannot fire is
 * the F25 shape with extra steps.
 *
 * WHAT THIS GUARD IS FOR, NOW THAT THE WRITE SIDE ALSO ENFORCES IT.
 * `provisioningAdmissionFor` (`app/api/setup/route.ts`) refuses a provisioning
 * run that would take the deployment-wide enabled count above one, counted with
 * the predicate on the line below, so the wizard can no longer produce the state
 * this function answers. It stays anyway, and is not redundant: the enabled
 * count is Moira's, not the console's, and Moira's admin API enables rows
 * without asking the console anything. This is what keeps a state the console
 * cannot render from being rendered wrongly; the write-side refusal is what
 * keeps an operator from walking into it through the UI.
 */
export function ambiguityGuard(
  enabledCount: number,
  resolution: AuthConfigsResolution,
): AuthConfigsResolution {
  if (enabledCount <= 1) return resolution;
  return failure(resolution.cacheKey, "ambiguous_enabled_providers");
}

/**
 * Fetch from Moira and resolve, in one call.
 *
 * The lists are bounded rather than fully paged. 100 providers is far past the
 * point where a console has a usable sign-in page, and the trusted-issuer list
 * only has to contain the rows the enabled providers are bound to.
 */
export async function loadAuthConfigs(
  client: MoiraClient,
  store: ConsoleSecretStore,
  bffIssuerUrl: string,
): Promise<AuthConfigsResolution> {
  const [providers, issuers] = await Promise.all([
    client.listAuthProviders({ limit: 100 }),
    client.listTrustedJwtIssuers({ limit: 100 }),
  ]);
  const rows = providers.data;
  const newestSecretUpdatedAt = await store.newestUpdatedAt();

  const enabled = rows.filter((row) => row.enabled && row.status === "active");
  const sealed = new Map<string, SealedClientSecret | null>();
  const secrets = new Map<string, string | null>();
  for (const row of enabled) {
    sealed.set(row.id, await store.read(row.id));
    secrets.set(row.id, await store.reveal(row.id));
  }

  const resolution = resolveAuthConfigs({
    rows,
    trustedIssuers: issuers.data,
    bffIssuerUrl,
    sealed,
    secrets,
    newestSecretUpdatedAt,
  });
  return ambiguityGuard(enabled.length, resolution);
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
 *
 * From 4B the list is PER PROVIDER, because each provider owns its own trusted
 * issuer row and therefore its own `admission_policy` lookup. The caller must
 * pass the list of the provider that authenticated the session, not "the"
 * console's list — there is no longer one.
 */
export function isEmailDomainAllowed(email: string, allowedDomains: readonly string[]): boolean {
  if (allowedDomains.length === 0) return false; // deny by default
  const at = email.lastIndexOf("@");
  if (at <= 0 || at === email.length - 1) return false;
  const domain = email.slice(at + 1).toLowerCase();
  return allowedDomains.some((allowed) => allowed.toLowerCase() === domain);
}
