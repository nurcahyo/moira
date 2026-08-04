// @server-only
//
// The setup wizard's flow logic: the ordering of Moira writes, the gates between
// wizard steps, and the partial-state handling between them.
//
// Carries the bootstrap system key transitively (through `MoiraClient`) and
// receives a closure over the OAuth client secret, so it is server-only by the
// same build guard as `moira-client.ts` — see the `import "server-only"` below.
//
// ============================================================================
// WHY THE ORDER IS WHAT IT IS. Read before changing anything below.
// ============================================================================
//
// `AuthSettingsService::admission_policy` selects the policy that decides a
// claim in two disjoint stages (wave 4A; it replaced `governing_policy`, which
// ordered a single `issuer = $1 or trusted_jwt_issuer_id = $2` query and let the
// oldest bound row govern every claim — finding F23):
//
//   1. `where ... and trusted_jwt_issuer_id = $2`   <- at most one row, by the
//      partial unique index `auth_provider_settings_one_enabled_per_trusted_issuer`
//   2. only if (1) matched nothing: `where ... and issuer = $1
//      and trusted_jwt_issuer_id is null`  <- the mode-3 bring-your-own-JWKS path
//
// `$1` is the CLAIM BODY's issuer. `$2` is the `trusted_jwt_issuers.id` resolved
// from that same issuer string.
//
// This console claims with the CONSOLE's issuer, while the provider row's
// `issuer` column holds the IDP's issuer (Google, or the generic-OIDC IdP) —
// that column is load-bearing for `validate_method_shape` and for composing the
// OAuth client, and must not be repurposed. So stage 2 can never match a
// console-provisioned row, and stage 1 matches only if the provider row actually
// carries the id of the console's registered trusted JWT issuer.
//
// Therefore, PER PROVIDER:
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
//
// ============================================================================
// ONE TRUSTED ISSUER PER PROVIDER (wave 4B)
// ============================================================================
//
// `runSetupProvisioning` provisions ONE provider and the ONE trusted JWT issuer
// it is bound to. That was already true; what changes in 4B is that the issuer
// string is a per-provider input rather than a deployment constant.
//
//   * the FIRST provider takes `env.bffIssuerUrl` — it is the incumbent, so it
//     keeps that string and Better Auth's `moira-console-idp` automatically,
//     with no alias table and no "which one is legacy" flag;
//   * each ADDITIONAL provider takes `consoleIssuerForSlug(bffIssuerUrl, slug)`,
//     an operator-chosen stable slug under the console's own issuer namespace.
//
// Provisioning a second provider is therefore calling `runSetupProvisioning`
// again with a different `console.issuer` and a different `provider`.
// `ensureTrustedJwtIssuer` is reuse-first and already generalises: it looks the
// issuer string up before creating it, so a re-run adopts rather than colliding.
//
// The partial unique index means each additional provider MUST get its own
// trusted-issuer row: two enabled providers bound to one issuer is refused
// `409 duplicate_enabled_provider_for_issuer` at the enable step.

import "server-only";

import { consoleIssuerForSlug } from "./auth-config";
import { MoiraClient, ifMatchFor, type MoiraOperationName } from "./moira-client";
import { isMoiraRequestError, type MoiraError } from "./errors";
import { CONSOLE_MESSAGE_KEYS } from "./i18n/keys";
import {
  EMPTY_PROVISIONING_STATE,
  provisioningDeficiencies,
  reachableSetupStep,
  type SetupProvisioningState,
  type SetupWizardState,
} from "./setup-steps";
import type { AuthMethod, AuthProviderSettingsRecord, TrustedJwtIssuerRecord } from "./types";

/* -------------------------------------------------------------------------- */
/* Wizard steps and their gates                                               */
/* -------------------------------------------------------------------------- */
//
// The step types and the pure gate functions moved to `lib/setup-steps.ts` so
// the wizard UI (`"use client"` organisms in `modules/setup/**`) can recompute
// `reachableSetupStep` without importing this credential-carrying module. They
// are re-exported here verbatim: this module remains the canonical import for
// every server-side caller, and nothing about their behaviour changed.

export {
  EMPTY_PROVISIONING_STATE,
  SETUP_STEP_ORDER,
  isProvisioningComplete,
  provisioningDeficiencies,
  reachableSetupStep,
} from "./setup-steps";
export type {
  ProvisioningDeficiency,
  SetupProvisioningState,
  SetupStepId,
  SetupWizardState,
} from "./setup-steps";

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
  | "update_auth_provider"
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
  ensure_trusted_jwt_issuer: CONSOLE_MESSAGE_KEYS.trusted_jwt_issuer_registration_failed,
  create_auth_provider: CONSOLE_MESSAGE_KEYS.auth_provider_create_failed,
  update_auth_provider: CONSOLE_MESSAGE_KEYS.auth_provider_update_failed,
  store_console_secret: CONSOLE_MESSAGE_KEYS.auth_provider_secret_write_failed,
  enable_auth_provider: CONSOLE_MESSAGE_KEYS.auth_provider_enable_failed,
};

const SETUP_PARTIAL_STATE_REMEDIES: Readonly<
  Record<SetupProvisioningStepId, SetupPartialStateRemedy>
> = {
  ensure_trusted_jwt_issuer: "retry",
  create_auth_provider: "retry_reuses_trusted_jwt_issuer",
  // The row already exists; a retry replays the same PATCH, which is safe under
  // `If-Match` and cannot mint a second provider row.
  update_auth_provider: "retry",
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
  /**
   * What THIS provider's console-minted tokens carry as `iss`.
   *
   * `env.bffIssuerUrl` for the incumbent, `consoleIssuerForSlug(...)` for every
   * additional provider. Build it with `consoleIssuerConfigFor` rather than by
   * hand: the console derives Better Auth's `providerId` back OUT of this string
   * at sign-in time, and a string it cannot parse gets no sign-in button.
   */
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

export type SetupTraceOutcome = "created" | "reused" | "updated" | "enabled" | "stored";

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
        "must be set as `trusted_jwt_issuer_id` on the create body. Without it `admission_policy` " +
        "matches neither stage and every claim is 403 admin_claim_domain_not_allowed — and from " +
        "wave 4B the console cannot resolve the provider's minted `iss` either, because that is " +
        "the bound trusted issuer's own `issuer` string.",
    );
  }
}

/* -------------------------------------------------------------------------- */
/* Which console issuer a provider is provisioned under (wave 4B)             */
/* -------------------------------------------------------------------------- */

/**
 * The `ConsoleIssuerConfig` for one provider.
 *
 * `slug === null` means the INCUMBENT: the provider bound to the
 * `env.bffIssuerUrl` trusted issuer, which keeps that string and Better Auth's
 * `moira-console-idp` id. There is at most one, it is not marked anywhere, and
 * it does not need to be — "the row bound to the `bffIssuerUrl` issuer" is the
 * definition, and the partial unique index makes it unique.
 *
 * Every other provider gets its own slug, and therefore its own
 * `trusted_jwt_issuers` row, its own `iss`, and its own `admin_identities` grant
 * namespace. They SHARE one `jwks_url` and one ES256 key pair: `trusted_jwt_issuers`
 * has a unique index on `issuer` alone and none on `jwks_url`, and better-auth's
 * `getLatestKey` sorts the `jwks` table globally with no issuer awareness, so
 * per-issuer key pairs are not implementable and are not attempted.
 *
 * THE FOOTGUN THIS AVOIDS. Deriving the issuer from the `auth_provider_settings`
 * row UUID instead would mint a NEW issuer string every time an operator deleted
 * and recreated a provider, silently orphaning every `admin_identities` grant
 * made through it — and wave 4A's `trusted_issuer_has_active_grants` guard would
 * not fire, because the trusted-issuer row is not what changed. Anchoring on the
 * trusted issuer's own `issuer` column puts the identity behind that guard.
 */
export function consoleIssuerConfigFor(
  env: { readonly bffIssuerUrl: string; readonly adminApiAudience: string },
  jwksUrl: string,
  slug: string | null,
  allowedAlgorithms?: readonly string[],
): ConsoleIssuerConfig {
  return {
    // `consoleIssuerForSlug` THROWS on a slug the console could not derive a
    // `providerId` back out of, and that refusal is deliberately here — before
    // any Moira write — because the failure it prevents is one-way: a trusted
    // issuer registered under an unparseable string cannot be deleted once a
    // grant exists under it (`409 trusted_issuer_has_active_grants`), so the
    // provider would be permanently unofferable and its admins permanently
    // unreachable. The validation is NOT restated here: one rule, one spelling.
    issuer: slug === null ? env.bffIssuerUrl : consoleIssuerForSlug(env.bffIssuerUrl, slug),
    jwksUrl,
    audience: env.adminApiAudience,
    ...(allowedAlgorithms === undefined ? {} : { allowedAlgorithms }),
  };
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

/**
 * Build the `PATCH /api/v1/admin/auth/providers/{id}` body for a re-save.
 *
 * The re-save path exists because the domain-refusal remedy tells the operator
 * to "add the domain and save again": replaying the CREATE for that would hit
 * the submission's idempotency key with a changed body (`409
 * idempotency_conflict`), and a fresh create would mint a second row that the
 * partial unique index refuses at enable (`409
 * duplicate_enabled_provider_for_issuer`). So an existing row is PATCHED.
 *
 * Deliberately absent: `enabled` (the client contract forbids patching it —
 * enable stays its own commit point), `trusted_jwt_issuer_id` (the binding is
 * not re-asserted; the read-back check verifies it instead), and `method`
 * (Moira's patch schema has no such field). URL members are omitted rather than
 * sent as null when the form left them empty, so a re-save cannot silently
 * clear an endpoint the row already has.
 */
export function buildProviderPatchBody(provider: AuthProviderConfig) {
  if (provider.allowedEmailDomains.length === 0) {
    throw new SetupOrderingError(
      "allowed_email_domains must be non-empty: an empty list denies every claim, including the " +
        "operator's own first claim, and there is no first-claim exemption",
    );
  }
  const withOptional = (key: string, value: string | null | undefined) =>
    value === null || value === undefined || value === "" ? {} : { [key]: value };

  return {
    display_name: provider.displayName,
    allowed_email_domains: [...provider.allowedEmailDomains],
    requested_scopes: [...(provider.requestedScopes ?? [])],
    redirect_uris: [...(provider.redirectUris ?? [])],
    ...withOptional("issuer", provider.issuer),
    ...withOptional("discovery_url", provider.discoveryUrl),
    ...withOptional("authorization_url", provider.authorizationUrl),
    ...withOptional("token_url", provider.tokenUrl),
    ...withOptional("userinfo_url", provider.userinfoUrl),
    ...withOptional("jwks_url", provider.jwksUrl),
    ...withOptional("client_id", provider.clientId),
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
  //
  // Two shapes, decided by the resume state:
  //
  //   * no `providerId` yet -> CREATE, keyed on the submission's idempotency key
  //     so a same-body retry replays rather than duplicates;
  //   * a `providerId` -> the row EXISTS (a partial attempt, or a completed one
  //     being re-saved after a claim-time domain refusal) -> PATCH it. Replaying
  //     the create here with a CHANGED body would be `409 idempotency_conflict`,
  //     and a fresh create would mint a second row beside it that the partial
  //     unique index then refuses at enable.
  const providerStep: SetupProvisioningStepId =
    state.providerId === null ? "create_auth_provider" : "update_auth_provider";
  let provider: AuthProviderSettingsRecord;
  if (state.providerId === null) {
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
  } else {
    try {
      assertB1Invariant(state.trustedJwtIssuerId);
      // Read the row first for a FRESH `If-Match`: the recorded version may be
      // stale (the enable bumped it, or another tab saved), and a stale
      // precondition would turn every re-save into `409 resource_version_conflict`.
      const current = await client.getAuthProvider(state.providerId);
      provider = await client.patchAuthProvider(
        current.id,
        buildProviderPatchBody(request.provider),
        ifMatchFor(current),
      );
      trace.push({
        step: "update_auth_provider",
        operation: "patchAuthProvider",
        outcome: "updated",
      });
    } catch (error) {
      throw new SetupProvisioningError(
        "update_auth_provider",
        state,
        moiraErrorOf(error),
        "could not update the existing auth provider configuration",
      );
    }
  }

  state = {
    ...state,
    providerId: provider.id,
    providerVersion: provider.version,
    providerTrustedJwtIssuerId: provider.trusted_jwt_issuer_id ?? null,
    providerEnabled: provider.enabled,
    allowedEmailDomainCount: provider.allowed_email_domains.length,
  };

  // Read-back check: if Moira did not persist the binding, stop here rather than
  // enabling a row that can never govern the console's issuer.
  if (state.providerTrustedJwtIssuerId !== state.trustedJwtIssuerId) {
    throw new SetupProvisioningError(
      providerStep,
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
  // safety is If-Match plus enable being naturally idempotent. A row that is
  // ALREADY enabled (the update path re-saving a live provider) is left alone:
  // the commit already happened, and re-committing it would only spend a
  // version bump on nothing.
  if (provider.enabled) {
    return { state, trace };
  }
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
    allowedEmailDomainCount: enabled.allowed_email_domains.length,
  };

  return { state, trace };
}

/* -------------------------------------------------------------------------- */
/* Rehydration — what has ALREADY been provisioned, read back from the source  */
/* -------------------------------------------------------------------------- */

/**
 * Reconstruct the provisioning state for one console issuer from Moira's own
 * records plus the console's secret store.
 *
 * This is what makes the wizard survive anything the browser forgets: the
 * sign-in step is a FULL NAVIGATION to the IdP and back, so `/setup` reloads as
 * a fresh document with empty React state, and a provisioned-but-unclaimed
 * deployment may equally be revisited days later from a different browser. The
 * state is therefore never trusted to the client's memory — `GET /api/setup`
 * derives it here for display, and the claim action derives it AGAIN for its
 * own gate rather than believing whatever the browser echoes back.
 *
 * `hasStoredSecret` is a predicate, not the store, for the same reason
 * `storeClientSecret` is a closure: this module must not be able to reach a
 * secret value, only answer "is one sealed for this row".
 */
export async function deriveProvisioningState(
  client: MoiraClient,
  hasStoredSecret: (providerId: string) => Promise<boolean>,
  consoleIssuer: string,
): Promise<SetupProvisioningState> {
  const issuer = await client.findTrustedJwtIssuerByIssuer(consoleIssuer);
  if (issuer === null || issuer.status === "deleted") return EMPTY_PROVISIONING_STATE;

  const state: SetupProvisioningState = {
    ...EMPTY_PROVISIONING_STATE,
    trustedJwtIssuerId: issuer.id,
    trustedJwtIssuerVersion: issuer.version,
  };

  const provider = await findProviderBoundTo(client, issuer.id);
  if (provider === null) return state;

  return {
    ...state,
    providerId: provider.id,
    providerVersion: provider.version,
    providerTrustedJwtIssuerId: provider.trusted_jwt_issuer_id ?? null,
    providerEnabled: provider.enabled,
    allowedEmailDomainCount: provider.allowed_email_domains.length,
    consoleSecretStored: await hasStoredSecret(provider.id),
  };
}

/**
 * The provider row bound to a trusted issuer, paging the list.
 *
 * The ENABLED row wins when one exists — the partial unique index guarantees at
 * most one — and otherwise the first non-deleted bound row is the resumable
 * partial state a previous attempt left behind.
 */
async function findProviderBoundTo(
  client: MoiraClient,
  trustedJwtIssuerId: string,
): Promise<AuthProviderSettingsRecord | null> {
  let fallback: AuthProviderSettingsRecord | null = null;
  let cursor: string | undefined;
  // Bounded so a paging bug cannot spin forever during setup.
  for (let page = 0; page < 50; page += 1) {
    const response = await client.listAuthProviders(
      cursor === undefined ? { limit: 100 } : { limit: 100, cursor },
    );
    for (const row of response.data) {
      if (row.status === "deleted" || (row.trusted_jwt_issuer_id ?? null) !== trustedJwtIssuerId) {
        continue;
      }
      if (row.enabled) return row;
      fallback ??= row;
    }
    if (!response.pagination.has_more) break;
    const next = response.pagination.next_cursor;
    if (next === null || next === undefined || next === "") break;
    cursor = next;
  }
  return fallback;
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
