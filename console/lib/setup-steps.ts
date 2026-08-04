// The setup wizard's step gates, extracted CLIENT-SAFE from `lib/setup-flow.ts`.
//
// ============================================================================
// WHY THIS IS ITS OWN MODULE AND NOT PART OF `setup-flow.ts`
// ============================================================================
//
// `lib/setup-flow.ts` carries `import "server-only"`: it reaches `MoiraClient`
// and (through it) the bootstrap system key, so no `"use client"` file may
// import it — `tests/unit/architecture/server-only-guards.test.ts` and
// `layer-dependencies.test.ts` both enforce that.
//
// The wizard UI (`modules/setup/**`) still must navigate by
// `reachableSetupStep(state)` rather than by free-form local state — plan 08's
// setup-wizard item states that rule — and it recomputes the reachable step in
// the browser after every BFF response. So the PURE half lives here: types and
// functions over plain data, no credential, no import at all. `setup-flow.ts`
// re-exports everything below, so its existing importers (the BFF route,
// `setup-window.ts`, the B1 regression suite) are unchanged.
//
// Nothing in this file may ever read `process.env`, import a server module, or
// name a credential — it is bundled for the browser.

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
 *
 * Every field here is DISPLAY-SAFE BY CONSTRUCTION, because this state crosses
 * to the browser: `GET /api/setup` rehydrates it so the wizard survives the
 * OAuth round trip (the sign-in step is a full navigation away from `/setup`
 * and back) and any later revisit of a provisioned-but-unclaimed deployment.
 * That is why the allow-list is a COUNT and not the list — decision D4 keeps
 * `allowed_email_domains`, the deny-by-default admin-claim policy, off the
 * anonymous setup surface, and the gate below only ever needed non-emptiness.
 */
export interface SetupProvisioningState {
  readonly trustedJwtIssuerId: string | null;
  readonly trustedJwtIssuerVersion: number | null;
  readonly providerId: string | null;
  readonly providerVersion: number | null;
  /** As returned by Moira on the created/enabled row. */
  readonly providerTrustedJwtIssuerId: string | null;
  readonly providerEnabled: boolean;
  /** How many domains the row's allow-list carries. Never the domains (D4). */
  readonly allowedEmailDomainCount: number;
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
  allowedEmailDomainCount: 0,
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
 * trusted JWT issuer, or `admission_policy` never selects it — and from wave 4B
 * the binding is also where the console reads this provider's minted `iss`.
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
  if (state.allowedEmailDomainCount <= 0) missing.push("allowed_email_domains_empty");
  if (!state.consoleSecretStored) missing.push("console_secret_not_stored");
  return missing;
}

/**
 * Narrow an untrusted JSON payload back to `SetupProvisioningState`.
 *
 * Lenient on purpose (missing members become their empty value): the callers
 * are the wizard organisms reading their OWN BFF's responses, where a field the
 * console cannot read must degrade to "not confirmed" rather than crash a step.
 * The SERVER-side counterpart — `parseProvisioningState` in `setup-window.ts` —
 * REFUSES instead, because there the payload is a caller-supplied `resume`.
 */
export function narrowProvisioningState(value: unknown): SetupProvisioningState | null {
  if (typeof value !== "object" || value === null || Array.isArray(value)) return null;
  const record = value as Record<string, unknown>;
  const nullableString = (member: unknown): string | null =>
    typeof member === "string" ? member : null;
  const nullableNumber = (member: unknown): number | null =>
    typeof member === "number" && Number.isFinite(member) ? member : null;
  const count = record["allowedEmailDomainCount"];
  return {
    trustedJwtIssuerId: nullableString(record["trustedJwtIssuerId"]),
    trustedJwtIssuerVersion: nullableNumber(record["trustedJwtIssuerVersion"]),
    providerId: nullableString(record["providerId"]),
    providerVersion: nullableNumber(record["providerVersion"]),
    providerTrustedJwtIssuerId: nullableString(record["providerTrustedJwtIssuerId"]),
    providerEnabled: record["providerEnabled"] === true,
    allowedEmailDomainCount:
      typeof count === "number" && Number.isFinite(count) && count > 0 ? count : 0,
    consoleSecretStored: record["consoleSecretStored"] === true,
  };
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
