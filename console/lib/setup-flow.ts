// @server-only
//
// The setup wizard's flow logic: the ordering of Moira writes, the gates between
// wizard steps, and the partial-state handling between them.
//
// ============================================================================
// WHY THE ORDER IS WHAT IT IS. Read before changing anything below.
// ============================================================================
//
// `AuthSettingsService::governing_policy` selects the policy that decides a
// claim with:
//
//     select id, allowed_email_domains from auth_provider_settings
//      where deleted_at is null and status = 'active' and enabled
//        and (issuer = $1 or trusted_jwt_issuer_id = $2)
//
// `$1` is the CLAIM BODY's issuer. `$2` is the `trusted_jwt_issuers.id` resolved
// from that same issuer string. There is no third branch.
//
// This console claims with the CONSOLE's issuer, while the provider row's
// `issuer` column holds the IDP's issuer (Google, or the generic-OIDC IdP) —
// that column is load-bearing for `validate_method_shape` and for composing the
// OAuth client, and must not be repurposed. So `issuer = $1` can never match.
// The only branch that can match is `trusted_jwt_issuer_id = $2`, and it matches
// only if the provider row actually carries the id of the console's registered
// trusted JWT issuer.
//
// Therefore:
//
//   0. bootstrap the system key            (operator CLI — not automatable here)
//   1. POST /api/v1/admin/jwt-issuers      <- FIRST Moira write of the wizard
//   2. POST /api/v1/admin/auth/providers   <- carrying trusted_jwt_issuer_id
//   3. store the OAuth client secret in the console's own store
//   4. POST /api/v1/admin/auth/providers/{id}/enable   <- the commit point
//   5. POST /api/v1/admin/setup/claim
//
// REORDERING ALONE IS NOT ENOUGH. Moving the jwt-issuers call earlier without
// setting `trusted_jwt_issuer_id` on the provider row changes nothing: the
// policy lookup still returns None and the claim still fails
// `403 admin_claim_domain_not_allowed`. It fails as a 403 rather than
// `400 unregistered_trusted_issuer` precisely BECAUSE the issuer is registered —
// `resolve_active_issuer` is a hard pre-check that succeeds, and then the policy
// lookup finds nothing. A green "issuer registered" step is not evidence of
// anything.
//
// `assertB1Invariant` below is what makes that non-negotiable, and
// `tests/unit/lib/setup-flow.test.ts` is the regression test.

import { MoiraClient, ifMatchFor, type MoiraOperationName } from "./moira-client";
import { isMoiraRequestError, type MoiraError } from "./errors";
import type { AuthMethod, AuthProviderSettingsRecord, TrustedJwtIssuerRecord } from "./types";

/* -------------------------------------------------------------------------- */
/* Wizard steps and their gates                                               */
/* -------------------------------------------------------------------------- */

export type SetupStepId = "welcome" | "auth_settings" | "sign_in" | "claim" | "done";

/** Wizard order. `claim` is last, and unreachable without `auth_settings`. */
export const SETUP_STEP_ORDER = [
  "welcome",
  "auth_settings",
  "sign_in",
  "claim",
  "done",
] as const satisfies readonly SetupStepId[];

/**
 * Everything the wizard knows about what has actually been written.
 *
 * Deliberately records `providerTrustedJwtIssuerId` READ BACK FROM MOIRA's
 * response rather than what the console intended to send. The gate compares the
 * two: a provider row that came back without the binding does not advance the
 * wizard, whatever the request body said.
 */
export interface SetupProvisioningState {
  readonly trustedJwtIssuerId: string | null;
  readonly trustedJwtIssuerVersion: number | null;
  readonly providerId: string | null;
  readonly providerVersion: number | null;
  /** As returned by Moira on the created/enabled row. */
  readonly providerTrustedJwtIssuerId: string | null;
  readonly providerEnabled: boolean;
  readonly allowedEmailDomains: readonly string[];
  /** The console-side OAuth client secret write (D7) succeeded. */
  readonly consoleSecretStored: boolean;
}

export const EMPTY_PROVISIONING_STATE: SetupProvisioningState = {
  trustedJwtIssuerId: null,
  trustedJwtIssuerVersion: null,
  providerId: null,
  providerVersion: null,
  providerTrustedJwtIssuerId: null,
  providerEnabled: false,
  allowedEmailDomains: [],
  consoleSecretStored: false,
};

/** Why the auth-settings step is not complete. Empty means it is. */
export type ProvisioningDeficiency =
  | "trusted_jwt_issuer_not_registered"
  | "provider_not_created"
  | "provider_not_bound_to_trusted_jwt_issuer"
  | "provider_not_enabled"
  | "allowed_email_domains_empty"
  | "console_secret_not_stored";

/**
 * The five-condition advance gate, plus the D7 secret condition.
 *
 * The third condition is the one plan 08's body never had: it is not enough for
 * the provider row to exist and be enabled — it must be BOUND to the console's
 * trusted JWT issuer, or `governing_policy` never selects it.
 */
export function provisioningDeficiencies(
  state: SetupProvisioningState,
): readonly ProvisioningDeficiency[] {
  const missing: ProvisioningDeficiency[] = [];
  if (state.trustedJwtIssuerId === null) missing.push("trusted_jwt_issuer_not_registered");
  if (state.providerId === null) missing.push("provider_not_created");
  if (
    state.trustedJwtIssuerId === null ||
    state.providerTrustedJwtIssuerId !== state.trustedJwtIssuerId
  ) {
    missing.push("provider_not_bound_to_trusted_jwt_issuer");
  }
  if (!state.providerEnabled) missing.push("provider_not_enabled");
  if (state.allowedEmailDomains.length === 0) missing.push("allowed_email_domains_empty");
  if (!state.consoleSecretStored) missing.push("console_secret_not_stored");
  return missing;
}

/** True only when every gate condition holds. */
export function isProvisioningComplete(state: SetupProvisioningState): boolean {
  return provisioningDeficiencies(state).length === 0;
}

export interface SetupWizardState {
  /** `GET /setup/claim-status`. `true` means setup is over. */
  readonly claimed: boolean;
  readonly provisioning: SetupProvisioningState;
  /** A verified IdP session exists whose email domain is in the allow-list. */
  readonly signedInWithAllowedIdentity: boolean;
  /** The claim returned 200/201. */
  readonly claimSucceeded: boolean;
}

/**
 * The furthest step the operator may reach.
 *
 * Navigation state, not advice: `claim` is unreachable while the provider is not
 * committed, so an operator cannot deep-link into a request that is guaranteed
 * to 403.
 */
export function reachableSetupStep(state: SetupWizardState): SetupStepId {
  if (state.claimSucceeded) return "done";
  if (state.claimed) return "done";
  if (!isProvisioningComplete(state.provisioning)) {
    // `welcome` is informational; the operator lands on `auth_settings` because
    // that is where the only outstanding work is.
    return "auth_settings";
  }
  if (!state.signedInWithAllowedIdentity) return "sign_in";
  return "claim";
}

/** Guard used by the claim action itself, not only by navigation. */
export function assertClaimStepIsReachable(state: SetupWizardState): void {
  const step = reachableSetupStep(state);
  if (step !== "claim") {
    throw new SetupOrderingError(
      `the claim step is unreachable (furthest reachable step: ${step}); ` +
        `outstanding: ${provisioningDeficiencies(state.provisioning).join(", ") || "none"}`,
    );
  }
}

/* -------------------------------------------------------------------------- */
/* Errors                                                                     */
/* -------------------------------------------------------------------------- */

/** A step ran out of order, or a required binding was absent. Always a bug. */
export class SetupOrderingError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "SetupOrderingError";
  }
}

export type SetupProvisioningStepId =
  | "ensure_trusted_jwt_issuer"
  | "create_auth_provider"
  | "store_console_secret"
  | "enable_auth_provider";

/**
 * What the operator can do about a partial write, per failing step.
 *
 * `retry_reuses_trusted_jwt_issuer` is the §0 partial state: the issuer was
 * created but the provider was not. The issuer row is inert on its own, but a
 * naive retry that re-POSTs it hits the `trusted_jwt_issuers_issuer_active_unique`
 * index — which is NOT mapped to a 409, so the operator would see an opaque
 * `500 database_error`. `ensureTrustedJwtIssuer` is reuse-first for exactly this
 * reason, and the recorded state makes the reuse explicit rather than incidental.
 */
export type SetupPartialStateRemedy =
  | "retry"
  | "retry_reuses_trusted_jwt_issuer"
  | "retry_or_discard_provider"
  | "retry_enable_no_secret_re_entry";

export const SETUP_PARTIAL_STATE_MESSAGE_KEYS: Readonly<Record<SetupProvisioningStepId, string>> = {
  ensure_trusted_jwt_issuer: "console.error.trusted_jwt_issuer_registration_failed",
  create_auth_provider: "console.error.auth_provider_create_failed",
  store_console_secret: "console.error.auth_provider_secret_write_failed",
  enable_auth_provider: "console.error.auth_provider_enable_failed",
};

const SETUP_PARTIAL_STATE_REMEDIES: Readonly<
  Record<SetupProvisioningStepId, SetupPartialStateRemedy>
> = {
  ensure_trusted_jwt_issuer: "retry",
  create_auth_provider: "retry_reuses_trusted_jwt_issuer",
  store_console_secret: "retry_or_discard_provider",
  enable_auth_provider: "retry_enable_no_secret_re_entry",
};

/**
 * A step failed. Carries the state written so far so the caller can resume
 * instead of restarting — restarting is what produces the orphan-issuer
 * uniqueness collision.
 */
export class SetupProvisioningError extends Error {
  readonly step: SetupProvisioningStepId;
  readonly state: SetupProvisioningState;
  readonly remedy: SetupPartialStateRemedy;
  readonly messageKey: string;
  readonly moiraError: MoiraError | null;

  constructor(
    step: SetupProvisioningStepId,
    state: SetupProvisioningState,
    moiraError: MoiraError | null,
    message: string,
  ) {
    super(message);
    this.name = "SetupProvisioningError";
    this.step = step;
    this.state = state;
    this.remedy = SETUP_PARTIAL_STATE_REMEDIES[step];
    this.messageKey = SETUP_PARTIAL_STATE_MESSAGE_KEYS[step];
    this.moiraError = moiraError;
  }
}

/* -------------------------------------------------------------------------- */
/* Provisioning request                                                       */
/* -------------------------------------------------------------------------- */

export interface ConsoleIssuerConfig {
  /** `MOIRA_BFF_ISSUER_URL` — what the console's own tokens carry as `iss`. */
  readonly issuer: string;
  /** The JWKS URL the console ACTUALLY serves, never a hard-coded guess. */
  readonly jwksUrl: string;
  /** `MOIRA_ADMIN_API_AUDIENCE`. Must be non-empty: an empty
   *  `expected_audiences` makes Moira skip audience validation entirely. */
  readonly audience: string;
  /** Defaults to `["ES256"]`. */
  readonly allowedAlgorithms?: readonly string[];
}

export interface AuthProviderConfig {
  readonly method: AuthMethod;
  /** Schema-required. Omitting it is a 400. */
  readonly displayName: string;
  /** The IDP's issuer. NOT the console's — see the header note. */
  readonly issuer?: string | null;
  readonly discoveryUrl?: string | null;
  readonly authorizationUrl?: string | null;
  readonly tokenUrl?: string | null;
  readonly userinfoUrl?: string | null;
  readonly jwksUrl?: string | null;
  readonly clientId?: string | null;
  /** Deny-by-default. An empty list refuses every claim, including the first. */
  readonly allowedEmailDomains: readonly string[];
  readonly requestedScopes?: readonly string[];
  readonly redirectUris?: readonly string[];
  readonly expectedAudiences?: readonly string[];
  readonly allowedAlgorithms?: readonly string[];
}

export interface SetupProvisioningRequest {
  readonly console: ConsoleIssuerConfig;
  readonly provider: AuthProviderConfig;
  /**
   * Writes the OAuth client secret into the CONSOLE's own store (D7).
   *
   * A pre-bound closure on purpose: this module never receives, holds, or names
   * the secret, so no code path here can put it in a Moira request body.
   */
  readonly storeClientSecret: (providerId: string, clientId: string | null) => Promise<void>;
  /** Stable per form submission, so a retry replays rather than duplicates. */
  readonly idempotencyKeys: {
    readonly trustedJwtIssuer: string;
    readonly authProvider: string;
  };
  /** State from a previous partial attempt, so a retry resumes. */
  readonly resume?: SetupProvisioningState;
}

/* -------------------------------------------------------------------------- */
/* Trace — what actually happened, in order                                   */
/* -------------------------------------------------------------------------- */

export type SetupTraceOutcome = "created" | "reused" | "enabled" | "stored";

export interface SetupTraceEntry {
  readonly step: SetupProvisioningStepId;
  readonly operation: MoiraOperationName | "console_secret_store";
  readonly outcome: SetupTraceOutcome;
}

export interface SetupProvisioningResult {
  readonly state: SetupProvisioningState;
  readonly trace: readonly SetupTraceEntry[];
}

/* -------------------------------------------------------------------------- */
/* The B1 invariant                                                           */
/* -------------------------------------------------------------------------- */

/**
 * Refuse to build a provider-create body without a registered trusted JWT issuer
 * id to bind it to.
 *
 * This is the check that makes "just reorder the calls" insufficient. A future
 * edit that moves `POST /jwt-issuers` back below `POST /auth/providers` cannot
 * merely produce a subtly-broken deployment — it throws here, in-process, before
 * a single request goes out.
 */
export function assertB1Invariant(trustedJwtIssuerId: string | null): asserts trustedJwtIssuerId {
  if (trustedJwtIssuerId === null || trustedJwtIssuerId === "") {
    throw new SetupOrderingError(
      "the trusted JWT issuer must be registered BEFORE the auth provider is created, and its id " +
        "must be set as `trusted_jwt_issuer_id` on the create body. Without it `governing_policy` " +
        "matches neither branch and every claim is 403 admin_claim_domain_not_allowed.",
    );
  }
}

/* -------------------------------------------------------------------------- */
/* Step 1 — the trusted JWT issuer, reuse-first                               */
/* -------------------------------------------------------------------------- */

interface EnsuredIssuer {
  readonly record: TrustedJwtIssuerRecord;
  readonly outcome: SetupTraceOutcome;
}

/**
 * Register (or adopt) the console's own trusted JWT issuer.
 *
 * Reuse-first, because a re-POST of an existing issuer hits a unique index that
 * Moira does not map to a 409 — the caller would see `500 database_error` and
 * have nothing to act on. Looking first turns the orphan-issuer partial state
 * into a no-op instead of a dead end.
 */
export async function ensureTrustedJwtIssuer(
  client: MoiraClient,
  config: ConsoleIssuerConfig,
  idempotencyKey: string,
): Promise<EnsuredIssuer> {
  const existing = await client.findTrustedJwtIssuerByIssuer(config.issuer);
  if (existing !== null) {
    return adoptExistingIssuer(client, existing);
  }

  const body = {
    issuer: config.issuer,
    jwks_url: config.jwksUrl,
    expected_audiences: [config.audience],
    allowed_algorithms: [...(config.allowedAlgorithms ?? ["ES256"])],
    subject_claim: "sub",
    // `scopes_claim` is deliberately absent — see assertTrustedIssuerCreateIsSafe.
  };

  try {
    const created = await client.createTrustedJwtIssuer(body, { idempotencyKey });
    return { record: created, outcome: "created" };
  } catch (error) {
    // A concurrent wizard run, or the unmapped unique-violation-as-500. Look
    // again before giving up: if the row now exists, adopting it is correct and
    // the failure was cosmetic.
    if (!isMoiraRequestError(error)) throw error;
    const raced = await client.findTrustedJwtIssuerByIssuer(config.issuer);
    if (raced === null) throw error;
    return adoptExistingIssuer(client, raced);
  }
}

async function adoptExistingIssuer(
  client: MoiraClient,
  existing: TrustedJwtIssuerRecord,
): Promise<EnsuredIssuer> {
  if (existing.scopes_claim != null) {
    throw new SetupOrderingError(
      `the registered trusted JWT issuer ${existing.issuer} declares scopes_claim; binding a ` +
        "provider to it is refused 400 console_issuer_must_not_assert_scopes. Clear the claim " +
        "mapping before continuing.",
    );
  }
  if (existing.status === "deleted") {
    throw new SetupOrderingError(
      `the trusted JWT issuer ${existing.issuer} is deleted and cannot be reused`,
    );
  }
  if (existing.status === "disabled") {
    // A disabled issuer fails `resolve_active_issuer` at claim time with
    // 400 unregistered_trusted_issuer, so adopting it as-is would defer the
    // failure to the last step.
    const enabled = await client.enableTrustedJwtIssuer(existing.id, ifMatchFor(existing));
    return { record: enabled, outcome: "enabled" };
  }
  return { record: existing, outcome: "reused" };
}

/* -------------------------------------------------------------------------- */
/* Step 2 — the provider create body                                          */
/* -------------------------------------------------------------------------- */

/**
 * Build the `POST /api/v1/admin/auth/providers` body.
 *
 * Never sets `enabled` — the key is absent, not `false`. Always sets
 * `trusted_jwt_issuer_id`. `issuer` stays the IdP's.
 */
export function buildProviderCreateBody(
  provider: AuthProviderConfig,
  trustedJwtIssuerId: string | null,
) {
  assertB1Invariant(trustedJwtIssuerId);
  if (provider.allowedEmailDomains.length === 0) {
    throw new SetupOrderingError(
      "allowed_email_domains must be non-empty: an empty list denies every claim, including the " +
        "operator's own first claim, and there is no first-claim exemption",
    );
  }

  return {
    method: provider.method,
    display_name: provider.displayName,
    trusted_jwt_issuer_id: trustedJwtIssuerId,
    allowed_email_domains: [...provider.allowedEmailDomains],
    issuer: provider.issuer ?? null,
    discovery_url: provider.discoveryUrl ?? null,
    authorization_url: provider.authorizationUrl ?? null,
    token_url: provider.tokenUrl ?? null,
    userinfo_url: provider.userinfoUrl ?? null,
    jwks_url: provider.jwksUrl ?? null,
    client_id: provider.clientId ?? null,
    requested_scopes: [...(provider.requestedScopes ?? [])],
    redirect_uris: [...(provider.redirectUris ?? [])],
    expected_audiences: [...(provider.expectedAudiences ?? [])],
    allowed_algorithms: [...(provider.allowedAlgorithms ?? [])],
  };
}

/* -------------------------------------------------------------------------- */
/* The runner                                                                 */
/* -------------------------------------------------------------------------- */

function moiraErrorOf(error: unknown): MoiraError | null {
  return isMoiraRequestError(error) ? error.moiraError : null;
}

/**
 * Run the whole provisioning sequence in its required order.
 *
 * Returns the state and an ordered trace. Throws `SetupProvisioningError`
 * carrying the state written so far, so the caller resumes rather than restarts.
 */
export async function runSetupProvisioning(
  client: MoiraClient,
  request: SetupProvisioningRequest,
): Promise<SetupProvisioningResult> {
  const trace: SetupTraceEntry[] = [];
  let state: SetupProvisioningState = request.resume ?? EMPTY_PROVISIONING_STATE;

  // ---- 1. trusted JWT issuer FIRST ---------------------------------------
  let issuer: TrustedJwtIssuerRecord;
  try {
    const ensured = await ensureTrustedJwtIssuer(
      client,
      request.console,
      request.idempotencyKeys.trustedJwtIssuer,
    );
    issuer = ensured.record;
    trace.push({
      step: "ensure_trusted_jwt_issuer",
      operation: ensured.outcome === "created" ? "createTrustedJwtIssuer" : "listTrustedJwtIssuers",
      outcome: ensured.outcome,
    });
  } catch (error) {
    throw new SetupProvisioningError(
      "ensure_trusted_jwt_issuer",
      state,
      moiraErrorOf(error),
      "could not register the console's trusted JWT issuer",
    );
  }
  state = {
    ...state,
    trustedJwtIssuerId: issuer.id,
    trustedJwtIssuerVersion: issuer.version,
  };

  // ---- 2. the auth provider, BOUND to that issuer -------------------------
  let provider: AuthProviderSettingsRecord;
  try {
    const body = buildProviderCreateBody(request.provider, state.trustedJwtIssuerId);
    provider = await client.createAuthProvider(body, {
      idempotencyKey: request.idempotencyKeys.authProvider,
    });
    trace.push({
      step: "create_auth_provider",
      operation: "createAuthProvider",
      outcome: "created",
    });
  } catch (error) {
    // The orphan trusted issuer is inert, but `state` records it so the retry
    // adopts it rather than re-POSTing into the unique index.
    throw new SetupProvisioningError(
      "create_auth_provider",
      state,
      moiraErrorOf(error),
      "could not create the auth provider configuration",
    );
  }

  state = {
    ...state,
    providerId: provider.id,
    providerVersion: provider.version,
    providerTrustedJwtIssuerId: provider.trusted_jwt_issuer_id ?? null,
    providerEnabled: provider.enabled,
    allowedEmailDomains: [...provider.allowed_email_domains],
  };

  // Read-back check: if Moira did not persist the binding, stop here rather than
  // enabling a row that can never govern the console's issuer.
  if (state.providerTrustedJwtIssuerId !== state.trustedJwtIssuerId) {
    throw new SetupProvisioningError(
      "create_auth_provider",
      state,
      null,
      "Moira returned a provider row without the trusted_jwt_issuer_id binding; " +
        "enabling it would produce a permanent 403 admin_claim_domain_not_allowed",
    );
  }

  // ---- 3. the console-side secret (never sent to Moira) -------------------
  try {
    await request.storeClientSecret(provider.id, provider.client_id ?? null);
    trace.push({
      step: "store_console_secret",
      operation: "console_secret_store",
      outcome: "stored",
    });
  } catch (error) {
    throw new SetupProvisioningError(
      "store_console_secret",
      state,
      moiraErrorOf(error),
      "the provider configuration was saved in Moira, but its client secret could not be stored " +
        "in this console; the provider has been left disabled",
    );
  }
  state = { ...state, consoleSecretStored: true };

  // ---- 4. enable: the commit point ---------------------------------------
  // No Idempotency-Key: the spec does not declare one on this operation. Retry
  // safety is If-Match plus enable being naturally idempotent.
  let enabled: AuthProviderSettingsRecord;
  try {
    enabled = await client.enableAuthProvider(provider.id, ifMatchFor(provider));
    trace.push({
      step: "enable_auth_provider",
      operation: "enableAuthProvider",
      outcome: "enabled",
    });
  } catch (error) {
    throw new SetupProvisioningError(
      "enable_auth_provider",
      state,
      moiraErrorOf(error),
      "the client secret was stored, but the provider could not be enabled in Moira",
    );
  }

  state = {
    ...state,
    providerVersion: enabled.version,
    providerTrustedJwtIssuerId: enabled.trusted_jwt_issuer_id ?? null,
    providerEnabled: enabled.enabled,
    allowedEmailDomains: [...enabled.allowed_email_domains],
  };

  return { state, trace };
}

/* -------------------------------------------------------------------------- */
/* Step 5 — the claim                                                         */
/* -------------------------------------------------------------------------- */

export interface ClaimAdminIdentityInput {
  /** The CONSOLE's issuer — the same string registered in step 1. */
  readonly consoleIssuer: string;
  /** The IdP's stable subject for the signed-in human. */
  readonly subject: string;
  readonly email: string;
  readonly emailVerified: boolean;
}

/**
 * Deterministic `Idempotency-Key` for a claim, derived from `(issuer, subject)`.
 *
 * Deterministic so a double-submit replays with 200 rather than conflicting with
 * 409. Formatted as a v5-shaped UUID; the value is a key, not a secret, and the
 * inputs are already public identifiers.
 */
export async function claimIdempotencyKey(issuer: string, subject: string): Promise<string> {
  const encoded = new TextEncoder().encode(`moira-console/claim/${issuer} ${subject}`);
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", encoded));
  const bytes = digest.slice(0, 16);
  // RFC 4122 version 5 / variant 10xx bits.
  bytes[6] = ((bytes[6] ?? 0) & 0x0f) | 0x50;
  bytes[8] = ((bytes[8] ?? 0) & 0x3f) | 0x80;
  const hex = Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
  return [
    hex.slice(0, 8),
    hex.slice(8, 12),
    hex.slice(12, 16),
    hex.slice(16, 20),
    hex.slice(20, 32),
  ].join("-");
}

/**
 * `POST /api/v1/admin/setup/claim`, gated on the provisioning being complete.
 *
 * The body carries `issuer`, `subject`, `email` and `email_verified` and nothing
 * else. `scopes` is omitted so Moira applies `["moira:admin"]`; `setup_token` is
 * omitted because populating it is a hard 400.
 */
export async function claimAdminIdentity(
  client: MoiraClient,
  wizard: SetupWizardState,
  input: ClaimAdminIdentityInput,
) {
  assertClaimStepIsReachable(wizard);
  if (!input.emailVerified) {
    throw new SetupOrderingError(
      "email_verified must be true; Moira refuses an unverified address with " +
        "403 admin_claim_email_not_verified",
    );
  }
  const idempotencyKey = await claimIdempotencyKey(input.consoleIssuer, input.subject);
  return client.claimAdminIdentity(
    {
      issuer: input.consoleIssuer,
      subject: input.subject,
      email: input.email,
      email_verified: input.emailVerified,
    },
    { idempotencyKey },
  );
}
