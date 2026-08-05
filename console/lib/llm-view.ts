// The LLM settings screen's view model — CLIENT-SAFE BY CONSTRUCTION.
//
// ============================================================================
// WHY THIS MODULE EXISTS AT ALL
// ============================================================================
//
// The organisms under `modules/llm/**` are `"use client"`, so they may not
// import `lib/moira-client.ts` or `lib/moira-credential-types.ts` — both are in
// `containedModulePaths()` and `layer-dependencies.test.ts` fails on the import.
// They still need to NAME the shapes a route handler sends them, so the shapes
// live here, in a module that imports nothing credential-carrying and declares
// no secret.
//
// ============================================================================
// WHAT A PROVIDER CREDENTIAL LOOKS LIKE FROM THE BROWSER
// ============================================================================
//
// A row id, a status, and a type label. NOT `masked_secret`, and NOT
// `secret_fingerprint`.
//
// Moira's `CredentialRecord` carries both, and neither is the raw key — the
// first is Moira's own redaction, the second a hash. They are still withheld
// here, deliberately and against issue #74's own wording, because
// `secret_fingerprint` is a STABLE IDENTIFIER FOR A SECRET VALUE: equal
// fingerprints across two deployments say the same key is installed in both.
// Nothing on this screen needs that fact, and a value the browser never receives
// is one no later render, log or screenshot can leak. The screen's real question
// is "is there a credential row at all", which is a boolean, and that question is
// the one that decides whether a completion fails `credential_not_found`.
//
// The name `keyRows` rather than `credentials` is not squeamishness: the
// rendering-layer guard (`tests/unit/architecture/no-secret-props.test.ts`)
// matches `/credential/i` on every `*Props` member in `modules/**`, so a prop
// carrying that word fails the build for the whole family. Naming the projection
// after what it holds — rows, not secrets — keeps the guard meaningful instead of
// making it something to be exempted from.

import type { JsonValue } from "./types";

/* -------------------------------------------------------------------------- */
/* Constants the browser is allowed to know                                   */
/* -------------------------------------------------------------------------- */

/**
 * The address the "Connect local vLLM" shortcut is pre-filled with.
 *
 * A CONSTANT, not catalog copy. `tests/unit/lib/i18n-catalog-coverage.test.ts`
 * fails any message containing a URL, and it is right to: a message that names a
 * deployment's address stops being translatable and starts being configuration.
 * This is rendered as a FIELD VALUE the operator can overwrite.
 */
export const LOCAL_VLLM_BASE_URL = "https://local-llm.motrait.com";

/**
 * The route key every routing policy on this screen binds to.
 *
 * Seeded by migration `0005`. The console never creates it — see
 * `MOIRA_OPERATIONS`'s note on why `POST /api/v1/admin/routes` is unregistered —
 * so its absence is a configuration error with its own keyed message.
 */
export const GENERAL_ROUTE_KEY = "general";

/**
 * The console's own BFF paths. Never Moira's.
 *
 * THERE IS NO TOP-LEVEL `/api/llm/routing`. An earlier draft of this constant
 * carried one, together with a `routingEndpoint()` helper that built
 * `/api/llm/routing/{id}` — a path no route module under `app/api/llm/**`
 * implements, and which Next answers with its 404 page rather than with a keyed
 * refusal. Routing lives NESTED under the provider that owns it
 * (`/api/llm/providers/{id}/routing[/{policyId}]`) because the ownership check
 * that stops a request body choosing which policy a privileged call touches is
 * only possible when the provider is in the path. Both the dead constant and the
 * dead helper are gone; `providerEndpoint` builds every path on this surface.
 */
export const LLM_ENDPOINTS = {
  providers: "/api/llm/providers",
  connectVllm: "/api/llm/connect-vllm",
} as const;

/** `/api/llm/providers/{id}` and the collections nested under it. */
export function providerEndpoint(providerId: string, suffix = ""): string {
  return `${LLM_ENDPOINTS.providers}/${encodeURIComponent(providerId)}${suffix}`;
}

/* -------------------------------------------------------------------------- */
/* The projection                                                             */
/* -------------------------------------------------------------------------- */

/** One `provider_models` row, as the browser sees it. */
export interface LlmModelView {
  readonly id: string;
  readonly modelKey: string;
  readonly status: string;
  readonly version: number;
  /** Moira's `capabilities`, which is what routing filters on. */
  readonly capabilities: JsonValue;
}

/**
 * One `provider_credentials` row, MINUS every secret-shaped field.
 *
 * `kind` is the `credential_type` discriminator (`api_key`, `azure_open_ai`, …),
 * carried so the screen can say WHAT sort of row exists without saying anything
 * about its value.
 */
export interface LlmKeyRowView {
  readonly id: string;
  readonly kind: string;
  readonly status: string;
  readonly version: number;
}

/** One `routing_policies` row pointing at this provider. */
export interface LlmPolicyView {
  readonly id: string;
  readonly routeId: string;
  readonly routeKey: string | null;
  readonly providerModelId: string;
  readonly priority: number;
  readonly weight: number;
  readonly status: string;
  readonly version: number;
}

/** A provider and everything hanging off it. */
export interface LlmProviderView {
  readonly id: string;
  readonly displayName: string;
  readonly providerType: string;
  readonly baseUrl: string | null;
  readonly status: string;
  readonly version: number;
  readonly models: readonly LlmModelView[];
  readonly keyRows: readonly LlmKeyRowView[];
  readonly policies: readonly LlmPolicyView[];
}

/** The whole screen. */
export interface LlmSettingsView {
  readonly providers: readonly LlmProviderView[];
  /** `null` when migration `0005`'s `general` route is absent. */
  readonly generalRouteId: string | null;
}

/* -------------------------------------------------------------------------- */
/* Chain readiness — "what exists and what is still missing"                  */
/* -------------------------------------------------------------------------- */

/**
 * The four things a prompt needs before `POST /api/v1/responses` can succeed.
 *
 * Named after the provisioning order rather than after the screen's layout,
 * because that order is the thing an operator has to get right: a provider with
 * a model and no credential row fails `credential_not_found`, and a provider
 * with all three and no routing policy is never selected at all. Both failures
 * are reported by Moira in terms that name none of this.
 */
export interface LlmChainReadiness {
  readonly hasModel: boolean;
  readonly hasKeyRow: boolean;
  readonly isEnabled: boolean;
  readonly hasPolicy: boolean;
}

/**
 * A row routing will actually consider.
 *
 * ============================================================================
 * `active`, NOT "not deleted" — MOIRA'S JOIN IS THE SPECIFICATION
 * ============================================================================
 *
 * `provider_models.status` cannot be `deleted` at all: migration
 * `0003_security_foundation.sql` constrains it to
 * `('active','disabled','deprecated')`, and disable writes `'disabled'` with
 * `deleted_at` left null. So a `status !== "deleted"` test degrades to "a row
 * exists" and admits both of the states routing refuses.
 *
 * Routing's own predicate is the one this has to agree with:
 * `src/infra/repositories/runtime.rs` joins `provider_models` on
 * `pm.status = 'active'`, `routing_policies` on `rp.status = 'active'` and
 * resolves a credential the same way. A screen that called a provider READY on a
 * disabled model would render "a prompt can reach this provider" directly under
 * a "Disabled" badge, and the first prompt would then fail with a routing error
 * naming neither the model nor its status.
 */
export function isRoutable(status: string): boolean {
  return status === "active";
}

export function chainReadiness(provider: LlmProviderView): LlmChainReadiness {
  return {
    hasModel: provider.models.some((model) => isRoutable(model.status)),
    hasKeyRow: provider.keyRows.some((row) => isRoutable(row.status)),
    isEnabled: isRoutable(provider.status),
    hasPolicy: provider.policies.some((policy) => isRoutable(policy.status)),
  };
}

/** Is the chain complete? `true` only when every step above is satisfied. */
export function isChainComplete(readiness: LlmChainReadiness): boolean {
  return readiness.hasModel && readiness.hasKeyRow && readiness.isEnabled && readiness.hasPolicy;
}

/* -------------------------------------------------------------------------- */
/* Discovery                                                                  */
/* -------------------------------------------------------------------------- */

/**
 * What `POST /api/llm/connect-vllm {action: "discover"}` answers with.
 *
 * The base URL comes BACK from the server rather than being remembered by the
 * browser, because the server canonicalises it (see `canonicalOpenAiBaseUrl`) and
 * the value that gets stored on the provider row must be the value that was
 * actually probed — otherwise "discovery worked" and "the provider works" are
 * two different endpoints.
 */
export interface LlmDiscoveryView {
  readonly baseUrl: string;
  readonly models: readonly string[];
}

/**
 * What one step of the chain did.
 *
 * `enabled` is its own outcome and not a flavour of `reused`. A step that found
 * a DISABLED row and turned it back on did not reuse anything routing would have
 * accepted — the deployment was unroutable a moment earlier and the operator has
 * to be able to tell the two apart, because "reused" on a disabled row is
 * exactly the report that made the shortcut announce success on a deployment no
 * prompt could reach.
 */
export type LlmConnectOutcome = "created" | "reused" | "enabled" | "skipped";

/** One step of the connect chain, as reported back to the browser. */
export interface LlmConnectStepView {
  readonly step: string;
  readonly outcome: LlmConnectOutcome;
  readonly detail: string | null;
}

/** What `POST /api/llm/connect-vllm {action: "connect"}` answers with. */
export interface LlmConnectResultView {
  readonly providerId: string;
  readonly trace: readonly LlmConnectStepView[];
}
