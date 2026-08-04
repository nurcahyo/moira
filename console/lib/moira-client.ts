// @server-only
//
// Typed client for the Moira admin endpoints the setup wizard needs.
//
// This module carries the bootstrap system key. It must never be imported from a
// client component. Two independent mechanisms enforce that:
//
//   * `import "server-only"` below — a build-time guard. Next.js compiles server
//     code with the `react-server` export condition, which resolves that package
//     to an empty module; a browser bundle resolves the `default` condition,
//     which is a bare `throw`, and `next build` fails.
//   * `tests/unit/architecture/server-only-guards.test.ts` — a static scan that
//     catches what the build guard cannot: an import that is server-legal but
//     architecturally wrong.
//
// DESIGN NOTE — the operation registry is the point.
//
// Every request goes through `MOIRA_OPERATIONS`, a table transcribed from
// `docs/openapi.json`. It records, per operation, which credential the spec
// declares, whether `Idempotency-Key` is declared, and whether `If-Match` is
// required. `request()` reads only that table; no method sets a header directly.
// Consequences that would otherwise be review opinions become mechanical:
//
//   * `Idempotency-Key` is attached only where the spec declares it. Of the ten
//     operations the console binds to, exactly TWO declare it — `POST /setup/claim`
//     and `POST /auth/providers`. `enable` does not, so the wizard's commit step
//     gets its retry safety from `If-Match` plus `enable` being naturally
//     idempotent, not from a key. Passing a key to an operation that does not
//     declare one is a thrown contract error, not a silently ignored header.
//   * Which calls are credential-free is read off the table, never assumed.
//     There are THREE as of plan 09 wave 3, and the number is deliberately not
//     written into any rule here:
//       - `GET  /api/v1/admin/setup/claim-status`     (wave 0)
//       - `POST /api/v1/admin/admin-invites/preview`  (wave 2 — shipped with no
//         `security` block at all, which nothing in this console noticed)
//       - `GET  /api/v1/admin/setup/sign-in-methods`  (finding F15's fix)
//     An earlier version of this note asserted "claim-status is the only
//     credential-free call". It was already false in the tree when it was read,
//     and a note that has to be edited every time Moira adds an anonymous
//     operation is a note that will be wrong again. The registry below is the
//     answer; `tests/contract/openapi-contract.test.ts` re-derives it from the
//     committed spec on every run.
//
// `tests/contract/openapi-contract.test.ts` re-derives the whole table from the
// committed spec on every run.

import "server-only";

import { MoiraRequestError, toMoiraError, toTransportError } from "./errors";
import type {
  ApiKeyCredentialSecret,
  ConsoleApiKeyCredentialCreateRequest,
  CredentialRecord,
  RotateCredentialRequest,
} from "./moira-credential-types";
import type {
  AdminIdentityPatchRequest,
  AdminIdentityRecord,
  AdminInviteCreateRequest,
  AdminInvitePreviewRequest,
  AdminInvitePreviewResponse,
  AdminInviteRecord,
  AdminInviteRedeemRequest,
  AdminInviteSecretResponse,
  AuthProviderSettingsRecord,
  ConsoleAuthProviderCreateRequest,
  ConsoleClaimAdminIdentityRequest,
  ConsoleTrustedJwtIssuerCreateRequest,
  ConsoleProviderModelCreateRequest,
  ListResponse,
  ProviderCreateRequest,
  ProviderModelRecord,
  ProviderPatchRequest,
  ProviderRecord,
  RouteDefinitionRecord,
  RoutingPolicyCreateRequest,
  RoutingPolicyPatchRequest,
  RoutingPolicyRecord,
  SetupAuthMethodsResponse,
  SetupClaimStatusResponse,
  SetupSignInMethodsResponse,
  TrustedJwtIssuerRecord,
} from "./types";

/* -------------------------------------------------------------------------- */
/* Operation registry                                                         */
/* -------------------------------------------------------------------------- */

export type HttpMethod = "GET" | "POST" | "PATCH" | "DELETE";

/**
 * Which credential the spec's `security` block declares for an operation.
 *
 * `system_key_only` is `[{ systemKeyAuth: [] }]` and nothing else — the claim
 * endpoint. A bearer JWT is refused there even if it verifies.
 * `admin` is the usual `[bearerAuth, systemKeyAuth, consumerKeyAuth]` triple.
 * `bearer_only` is `[{ bearerAuth: [] }]` and nothing else — see below.
 * `none` is an absent `security` block.
 *
 * ============================================================================
 * WHY `bearer_only` EXISTS AND WHY `admin` COULD NOT BE USED (plan 09 W5-D3)
 * ============================================================================
 *
 * `redeem_admin_invite`'s committed security is `[{ "bearerAuth": [] }]` alone.
 * Registering it as `admin` fails `openapi-contract.test.ts` outright (that
 * branch asserts the scheme list CONTAINS `systemKeyAuth`), and — the part that
 * matters — `#buildHeaders`' `admin` arm PREFERS the system key when one is
 * present. On the redemption path that would send the console's bootstrap
 * credential on an invitee's request, which is the console granting Moira admin
 * to an identity of its own choosing.
 *
 * `none` is not an option either: redemption is what proves the invitee's
 * `(issuer, subject)`, so a request with no `Authorization` header is a 401.
 *
 * So this variant carries the invariant rather than describing a shape: it
 * REQUIRES a bearer token and REFUSES a system key, throwing
 * `MoiraClientContractError` if one is configured — the mirror image of
 * `system_key_only`'s refusal of a bearer.
 */
export type MoiraCredentialRequirement = "none" | "system_key_only" | "admin" | "bearer_only";

export interface MoiraOperation {
  /** `operationId` in `docs/openapi.json`. */
  readonly id: string;
  readonly method: HttpMethod;
  /** The spec's path template, `{...}` placeholders intact. */
  readonly path: string;
  readonly credential: MoiraCredentialRequirement;
  /** The spec declares an `Idempotency-Key` header parameter. */
  readonly declaresIdempotencyKey: boolean;
  /** The spec declares `If-Match` as a REQUIRED header parameter. */
  readonly requiresIfMatch: boolean;
}

function op<T extends MoiraOperation>(operation: T): T {
  return operation;
}

export const MOIRA_OPERATIONS = {
  getSetupClaimStatus: op({
    id: "get_setup_claim_status",
    method: "GET",
    path: "/api/v1/admin/setup/claim-status",
    credential: "none",
    declaresIdempotencyKey: false,
    requiresIfMatch: false,
  }),
  getSetupAuthMethods: op({
    id: "get_setup_auth_methods",
    method: "GET",
    path: "/api/v1/admin/setup/auth-methods",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: false,
  }),
  /**
   * The ANONYMOUS sign-in projection (finding F15's fix, plan 09 wave 1).
   *
   * `PublicSignInMethod` is deliberately `PublicAuthMethod` MINUS
   * `allowed_email_domains` (that is plan 07 decision D3 — the deny-by-default
   * admin-claim policy, and publishing it anonymously would hand any caller the
   * list of domains that can obtain Moira admin) and minus `jwks_url`.
   *
   * Consequence the console must respect: it is enough to RENDER a sign-in
   * button and NOT enough to RESOLVE the configuration behind one.
   * `resolveAuthConfigs` refuses a row without `allowed_email_domains` or
   * `trusted_jwt_issuer_id`, and neither is in this projection.
   */
  getSetupSignInMethods: op({
    id: "get_setup_sign_in_methods",
    method: "GET",
    path: "/api/v1/admin/setup/sign-in-methods",
    credential: "none",
    declaresIdempotencyKey: false,
    requiresIfMatch: false,
  }),
  claimAdminIdentity: op({
    id: "claim_admin_identity",
    method: "POST",
    path: "/api/v1/admin/setup/claim",
    credential: "system_key_only",
    declaresIdempotencyKey: true,
    requiresIfMatch: false,
  }),

  /**
   * `POST /api/v1/admin/admin-invites` — the once-only token mint.
   *
   * `declaresIdempotencyKey: true` and `requiresIfMatch: false` are read OFF THE
   * SPEC, not guessed: the operation declares an optional `Idempotency-Key`
   * header parameter and no `If-Match` at all.
   * `tests/contract/openapi-contract.test.ts:195-206` re-derives both.
   *
   * The idempotent-replay behaviour is what makes the key matter here: a replay
   * returns the SANITIZED record with `secret: null`, not the token again. That
   * is not an error — see `AdminInviteSecretResponse` in `lib/types.ts`.
   */
  createAdminInvite: op({
    id: "create_admin_invite",
    method: "POST",
    path: "/api/v1/admin/admin-invites",
    credential: "admin",
    declaresIdempotencyKey: true,
    requiresIfMatch: false,
  }),

  listAdminInvites: op({
    id: "list_admin_invites",
    method: "GET",
    path: "/api/v1/admin/admin-invites",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: false,
  }),
  getAdminInvite: op({
    id: "get_admin_invite",
    method: "GET",
    path: "/api/v1/admin/admin-invites/{id}",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: false,
  }),
  /**
   * `POST .../admin-invites/{id}/revoke` — a POST to a sub-resource, not a
   * `DELETE`. Transcribed, not guessed: `DELETE /admin-invites/{id}` is not in
   * the spec at all, and the registry is what makes the difference a compile-time
   * fact rather than a 404 in front of an operator.
   *
   * No `If-Match`. Revocation is idempotent in the direction that matters (a
   * second revoke of a revoked invite is `409 invite_revoked`, not a silent
   * overwrite), so the spec declares an optional `Idempotency-Key` and no
   * version precondition.
   */
  revokeAdminInvite: op({
    id: "revoke_admin_invite",
    method: "POST",
    path: "/api/v1/admin/admin-invites/{id}/revoke",
    credential: "admin",
    declaresIdempotencyKey: true,
    requiresIfMatch: false,
  }),
  /**
   * `POST .../admin-invites/preview` — ANONYMOUS, and the token goes in the BODY.
   *
   * `credential: "none"` is read off the spec: the operation declares no
   * `security` block at all, and it is on the unauthenticated allow-list inside
   * `every_operation_documents_request_ids_and_protected_operations_document_auth`
   * with the explanation that comment demands.
   *
   * The invitee has no session yet when this runs — that is the whole point of
   * the endpoint — so anything that required a credential here would make the
   * invitation page unrenderable.
   */
  previewAdminInvite: op({
    id: "preview_admin_invite",
    method: "POST",
    path: "/api/v1/admin/admin-invites/preview",
    credential: "none",
    declaresIdempotencyKey: false,
    requiresIfMatch: false,
  }),
  /**
   * `POST .../admin-invites/redeem` — the ONLY `bearer_only` operation.
   *
   * See `MoiraCredentialRequirement`. The invitee's freshly minted, grantless
   * JWT is the credential; the console's bootstrap system key must never be
   * sent here, and `#buildHeaders` throws rather than silently preferring it.
   */
  redeemAdminInvite: op({
    id: "redeem_admin_invite",
    method: "POST",
    path: "/api/v1/admin/admin-invites/redeem",
    credential: "bearer_only",
    declaresIdempotencyKey: true,
    requiresIfMatch: false,
  }),

  /* --- admin identities (the ownership surface) --------------------------- */

  listAdminIdentities: op({
    id: "list_admin_identities",
    method: "GET",
    path: "/api/v1/admin/admin-identities",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: false,
  }),
  /**
   * `PATCH .../admin-identities/{id}` — ownership transfer, in ONE call.
   *
   * `declaresIdempotencyKey: true` AND `requiresIfMatch: true`. Plan 09 §0.8.4
   * step 6 describes this family as "`Idempotency-Key` on create, revoke, redeem
   * and delete" — the committed spec ALSO declares it here, so the audit's list
   * is incomplete and the registry follows the spec.
   *
   * `If-Match` is required and is the reason transfer cannot be two calls:
   * `set_primary` demotes every other active primary inside the same
   * transaction, so a follow-up "demote the actor" call would either demote the
   * person just promoted or 409 on a version the actor no longer holds.
   */
  patchAdminIdentity: op({
    id: "patch_admin_identity",
    method: "PATCH",
    path: "/api/v1/admin/admin-identities/{id}",
    credential: "admin",
    declaresIdempotencyKey: true,
    requiresIfMatch: true,
  }),
  /**
   * `DELETE .../admin-identities/{id}` — soft revoke. NO `If-Match`.
   *
   * Asserted rather than assumed, because the neighbouring `PATCH` requires one:
   * `every_if_match_operation_declares_the_documented_precondition` covers the
   * spec side and the contract test re-derives this flag on every run. Passing an
   * `ifMatch` here throws.
   */
  deleteAdminIdentity: op({
    id: "delete_admin_identity",
    method: "DELETE",
    path: "/api/v1/admin/admin-identities/{id}",
    credential: "admin",
    declaresIdempotencyKey: true,
    requiresIfMatch: false,
  }),

  listTrustedJwtIssuers: op({
    id: "list_trusted_jwt_issuers",
    method: "GET",
    path: "/api/v1/admin/jwt-issuers",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: false,
  }),
  createTrustedJwtIssuer: op({
    id: "create_trusted_jwt_issuer",
    method: "POST",
    path: "/api/v1/admin/jwt-issuers",
    credential: "admin",
    declaresIdempotencyKey: true,
    requiresIfMatch: false,
  }),
  enableTrustedJwtIssuer: op({
    id: "enable_trusted_jwt_issuer",
    method: "POST",
    path: "/api/v1/admin/jwt-issuers/{id}/enable",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: true,
  }),

  // The auth-provider surface: SEVEN operations, not ten. (Ten is the total
  // including the three setup operations above.)
  listAuthProviders: op({
    id: "list_auth_providers",
    method: "GET",
    path: "/api/v1/admin/auth/providers",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: false,
  }),
  createAuthProvider: op({
    id: "create_auth_provider",
    method: "POST",
    path: "/api/v1/admin/auth/providers",
    credential: "admin",
    declaresIdempotencyKey: true,
    requiresIfMatch: false,
  }),
  getAuthProvider: op({
    id: "get_auth_provider",
    method: "GET",
    path: "/api/v1/admin/auth/providers/{id}",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: false,
  }),
  patchAuthProvider: op({
    id: "patch_auth_provider",
    method: "PATCH",
    path: "/api/v1/admin/auth/providers/{id}",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: true,
  }),
  deleteAuthProvider: op({
    id: "delete_auth_provider",
    method: "DELETE",
    path: "/api/v1/admin/auth/providers/{id}",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: true,
  }),
  enableAuthProvider: op({
    id: "enable_auth_provider",
    method: "POST",
    path: "/api/v1/admin/auth/providers/{id}/enable",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: true,
  }),
  disableAuthProvider: op({
    id: "disable_auth_provider",
    method: "POST",
    path: "/api/v1/admin/auth/providers/{id}/disable",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: true,
  }),

  /* ======================================================================== */
  /* LLM runtime configuration (issue #73)                                    */
  /* ======================================================================== */
  //
  // ORDINARY ADMINISTRATION, NOT SETUP. Every operation below declares the usual
  // `[bearerAuth, systemKeyAuth, consumerKeyAuth]` triple, i.e. `credential:
  // "admin"` — none of them is anonymous, and none of them belongs on the
  // pre-admin bootstrap path the way `claim-status` does. Any console route
  // handler that reaches them therefore sits behind `withConsoleSession`, the
  // same gate `app/api/admins/**` uses.
  //
  // The three flags on each entry are transcribed from `docs/openapi.json` and
  // re-derived by `tests/contract/openapi-contract.test.ts` on every run. Two
  // shapes recur and neither is guessable:
  //
  //   create  optional `Idempotency-Key`, no `If-Match`
  //   mutate  required `If-Match`, no `Idempotency-Key`
  //
  // `rotate_credential` is the only operation that declares BOTH, and
  // `create_provider_model` is the only create that also takes a path parameter.

  listProviders: op({
    id: "list_providers",
    method: "GET",
    path: "/api/v1/admin/providers",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: false,
  }),
  createProvider: op({
    id: "create_provider",
    method: "POST",
    path: "/api/v1/admin/providers",
    credential: "admin",
    declaresIdempotencyKey: true,
    requiresIfMatch: false,
  }),
  getProvider: op({
    id: "get_provider",
    method: "GET",
    path: "/api/v1/admin/providers/{id}",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: false,
  }),
  /**
   * `PATCH /api/v1/admin/providers/{id}`. `If-Match` required, no key.
   *
   * `provider_type` IS NOT IN `ProviderPatchRequest`, and with
   * `additionalProperties: false` that makes it a flat `400` rather than an
   * error naming an immutable field — see `assertLlmProviderPatchIsSafe`.
   */
  patchProvider: op({
    id: "patch_provider",
    method: "PATCH",
    path: "/api/v1/admin/providers/{id}",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: true,
  }),
  enableProvider: op({
    id: "enable_provider",
    method: "POST",
    path: "/api/v1/admin/providers/{id}/enable",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: true,
  }),
  disableProvider: op({
    id: "disable_provider",
    method: "POST",
    path: "/api/v1/admin/providers/{id}/disable",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: true,
  }),

  /**
   * Models are NESTED under their provider for list and create, and FLAT for
   * enable and disable. Transcribed, not guessed: the write paths are
   * `/api/v1/admin/provider-models/{id}/…`, so a caller holding only a model id
   * needs no provider id to disable it — and a caller holding only a provider id
   * cannot enumerate models any other way.
   */
  listProviderModels: op({
    id: "list_provider_models",
    method: "GET",
    path: "/api/v1/admin/providers/{provider_id}/models",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: false,
  }),
  createProviderModel: op({
    id: "create_provider_model",
    method: "POST",
    path: "/api/v1/admin/providers/{provider_id}/models",
    credential: "admin",
    declaresIdempotencyKey: true,
    requiresIfMatch: false,
  }),
  enableProviderModel: op({
    id: "enable_provider_model",
    method: "POST",
    path: "/api/v1/admin/provider-models/{id}/enable",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: true,
  }),
  disableProviderModel: op({
    id: "disable_provider_model",
    method: "POST",
    path: "/api/v1/admin/provider-models/{id}/disable",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: true,
  }),

  listProviderCredentials: op({
    id: "list_credentials",
    method: "GET",
    path: "/api/v1/admin/provider-credentials",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: false,
  }),
  createProviderCredential: op({
    id: "create_credential",
    method: "POST",
    path: "/api/v1/admin/provider-credentials",
    credential: "admin",
    declaresIdempotencyKey: true,
    requiresIfMatch: false,
  }),
  enableProviderCredential: op({
    id: "enable_credential",
    method: "POST",
    path: "/api/v1/admin/provider-credentials/{id}/enable",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: true,
  }),
  disableProviderCredential: op({
    id: "disable_credential",
    method: "POST",
    path: "/api/v1/admin/provider-credentials/{id}/disable",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: true,
  }),
  /**
   * `POST .../provider-credentials/{id}/rotate` — the ONLY operation in this
   * registry declaring a REQUIRED `If-Match` and an optional `Idempotency-Key`
   * together, which is why it is asserted rather than assumed.
   *
   * Both are load-bearing and for different failures: the precondition stops a
   * rotation from landing on a row somebody has just disabled, and the key stops
   * a retried request from installing a second replacement key that the first
   * response never told the operator about.
   */
  rotateProviderCredential: op({
    id: "rotate_credential",
    method: "POST",
    path: "/api/v1/admin/provider-credentials/{id}/rotate",
    credential: "admin",
    declaresIdempotencyKey: true,
    requiresIfMatch: true,
  }),

  /**
   * Routes are READ-ONLY here, and the omission is the decision.
   *
   * `POST /api/v1/admin/routes` exists and is deliberately unregistered:
   * migration `0005` seeds the `general` route, and the create operation
   * documents no `409` for a duplicate `route_key`. A console that offered
   * "create route" would let an operator land a second `general`, after which
   * routing has two candidate definitions and no documented rule for choosing.
   * Reading the seeded row and binding policies to it is the whole of what the
   * settings page needs.
   */
  listRoutes: op({
    id: "list_route_definitions",
    method: "GET",
    path: "/api/v1/admin/routes",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: false,
  }),
  getRoute: op({
    id: "get_route_definition",
    method: "GET",
    path: "/api/v1/admin/routes/{id}",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: false,
  }),

  listRoutingPolicies: op({
    id: "list_routing_policies",
    method: "GET",
    path: "/api/v1/admin/routing-policies",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: false,
  }),
  createRoutingPolicy: op({
    id: "create_routing_policy",
    method: "POST",
    path: "/api/v1/admin/routing-policies",
    credential: "admin",
    declaresIdempotencyKey: true,
    requiresIfMatch: false,
  }),
  patchRoutingPolicy: op({
    id: "patch_routing_policy",
    method: "PATCH",
    path: "/api/v1/admin/routing-policies/{id}",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: true,
  }),
  enableRoutingPolicy: op({
    id: "enable_routing_policy",
    method: "POST",
    path: "/api/v1/admin/routing-policies/{id}/enable",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: true,
  }),
  disableRoutingPolicy: op({
    id: "disable_routing_policy",
    method: "POST",
    path: "/api/v1/admin/routing-policies/{id}/disable",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: true,
  }),
} as const;

export type MoiraOperationName = keyof typeof MOIRA_OPERATIONS;

/** The seven operations that make up the auth-provider surface. */
export const AUTH_PROVIDER_OPERATION_NAMES = [
  "listAuthProviders",
  "createAuthProvider",
  "getAuthProvider",
  "patchAuthProvider",
  "deleteAuthProvider",
  "enableAuthProvider",
  "disableAuthProvider",
] as const satisfies readonly MoiraOperationName[];

/**
 * The LLM runtime-configuration surface: the operations an LLM settings page may
 * reach, named so a test can assert properties of the SET rather than of each
 * entry — "all of them require a credential", "the routes family is read-only".
 *
 * `POST /api/v1/admin/routes`, `DELETE` on every family, and
 * `PUT .../runtime-policy` are all real operations that are deliberately absent.
 * The last is worth naming because it is the one mutation in Moira's whole admin
 * surface with an OPTIONAL `If-Match`, and a registry that carried it would
 * invite "If-Match is required on writes" to be read as a rule with no
 * exceptions.
 */
export const LLM_CONFIG_OPERATION_NAMES = [
  "listProviders",
  "createProvider",
  "getProvider",
  "patchProvider",
  "enableProvider",
  "disableProvider",
  "listProviderModels",
  "createProviderModel",
  "enableProviderModel",
  "disableProviderModel",
  "listProviderCredentials",
  "createProviderCredential",
  "enableProviderCredential",
  "disableProviderCredential",
  "rotateProviderCredential",
  "listRoutes",
  "getRoute",
  "listRoutingPolicies",
  "createRoutingPolicy",
  "patchRoutingPolicy",
  "enableRoutingPolicy",
  "disableRoutingPolicy",
] as const satisfies readonly MoiraOperationName[];

/* -------------------------------------------------------------------------- */
/* Contract errors — the console built a request it is forbidden to build      */
/* -------------------------------------------------------------------------- */

export class MoiraClientContractError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "MoiraClientContractError";
  }
}

/**
 * `POST /api/v1/admin/setup/claim` guard.
 *
 * - `scopes: []` is NOT the same as omitting `scopes`. Omitted yields
 *   `["moira:admin"]`; an explicit empty array creates a grant with zero scopes —
 *   a silent, permanent, un-revocable-by-retry no-op admin. The field must be
 *   absent, not empty.
 * - `setup_token` is rejected with `400 setup_token_not_supported`, not ignored.
 *   The console never sends it.
 * - `email` and `email_verified` are required with no defaults and no
 *   credential-type branch that makes them omittable.
 */
export function assertClaimRequestIsSafe(body: Record<string, unknown>): void {
  if ("scopes" in body) {
    throw new MoiraClientContractError(
      "claim body must omit `scopes` entirely — `scopes: []` creates a grant with zero scopes, " +
        "and a non-empty bad scope is 422 scope_invalid",
    );
  }
  if ("setup_token" in body) {
    throw new MoiraClientContractError(
      "claim body must omit `setup_token` — it is reserved and rejected with 400 setup_token_not_supported",
    );
  }
  if (typeof body["email"] !== "string" || body["email"].length === 0) {
    throw new MoiraClientContractError("claim body requires a non-empty `email`");
  }
  if (typeof body["email_verified"] !== "boolean") {
    throw new MoiraClientContractError("claim body requires a boolean `email_verified`");
  }
}

/**
 * `POST /api/v1/admin/auth/providers` guard.
 *
 * - `enabled` must be ABSENT, not `false`. It is a plain writable boolean in
 *   Moira; "the row is created disabled" is this console's convention, and this
 *   is where the convention is enforced. Only `enableAuthProvider` may enable a row.
 * - `trusted_jwt_issuer_id` must be present and non-empty. Without it,
 *   `admission_policy` matches neither its bound stage (`trusted_jwt_issuer_id
 *   = $2`) nor its unbound one (`issuer = $1`, which is the claim body's issuer —
 *   the console's, not the IdP's), so `policy = None` and every claim is
 *   `403 admin_claim_domain_not_allowed`. From wave 4B it is also what the
 *   console's minted `iss` is read from.
 * - `display_name` is required by the schema; omitting it is a 400.
 */
export function assertProviderCreateIsSafe(body: Record<string, unknown>): void {
  if ("enabled" in body) {
    throw new MoiraClientContractError(
      "provider create body must not contain `enabled` at all — not even `enabled: false`. " +
        "Use enableAuthProvider() as the commit point.",
    );
  }
  const issuerId = body["trusted_jwt_issuer_id"];
  if (typeof issuerId !== "string" || issuerId.length === 0) {
    throw new MoiraClientContractError(
      "provider create body must carry a non-empty `trusted_jwt_issuer_id` — without it the row " +
        "can never govern the console's issuer and every claim is 403 admin_claim_domain_not_allowed",
    );
  }
  if (typeof body["display_name"] !== "string" || body["display_name"].length === 0) {
    throw new MoiraClientContractError(
      "provider create body requires a non-empty `display_name` (schema-required; omitting it is a 400)",
    );
  }
}

/**
 * `POST /api/v1/admin/jwt-issuers` guard.
 *
 * A console-linked issuer must leave `scopes_claim` unset, or a provider row
 * bound to it is refused `400 console_issuer_must_not_assert_scopes` — tokens
 * that self-assert scopes would displace `admin_identities` as the source of
 * human authorization.
 */
export function assertTrustedIssuerCreateIsSafe(body: Record<string, unknown>): void {
  if ("scopes_claim" in body && body["scopes_claim"] != null) {
    throw new MoiraClientContractError(
      "the console's trusted JWT issuer must not declare `scopes_claim` — " +
        "authorization comes from the admin_identities grant, never from a self-asserted claim",
    );
  }
  if ("claim_mapping" in body && body["claim_mapping"] != null) {
    throw new MoiraClientContractError(
      "the console's trusted JWT issuer must not declare `claim_mapping` — it can carry a scopes mapping",
    );
  }
}

/* -------------------------------------------------------------------------- */
/* LLM runtime-configuration guards (issue #73)                                */
/* -------------------------------------------------------------------------- */

/** `ProviderType` values, as a runtime set. Mirrors `#/components/schemas/ProviderType`. */
const PROVIDER_TYPES: readonly string[] = [
  "open_ai_compatible",
  "open_ai",
  "anthropic",
  "gemini",
  "deep_seek",
  "azure_open_ai",
  "local",
  "custom",
];

/** Provider types whose whole purpose is a caller-supplied endpoint. */
const BASE_URL_REQUIRED_PROVIDER_TYPES: readonly string[] = ["open_ai_compatible", "local"];

/**
 * `POST /api/v1/admin/providers` guard.
 *
 * - `provider_type` must be one of the eight enum values. It is also IMMUTABLE:
 *   `ProviderPatchRequest` does not declare it, so a wrong value cannot be
 *   corrected — only replaced by a new provider and a re-pointed policy.
 * - `display_name` is schema-required; omitting it is a 400.
 * - `open_ai_compatible` and `local` REQUIRE a non-empty `base_url`. This is the
 *   one check here that is not a restatement of the schema, and it is the
 *   dangerous case: the compatible arm with no base URL does not fail, it falls
 *   back to the vendor default, so an operator who meant to point at a machine on
 *   their own network sends prompts to a third party instead. Silent, correct-
 *   looking, and only visible in someone else's logs.
 */
export function assertLlmProviderCreateIsSafe(body: Record<string, unknown>): void {
  const providerType = body["provider_type"];
  if (typeof providerType !== "string" || !PROVIDER_TYPES.includes(providerType)) {
    throw new MoiraClientContractError(
      "provider create body requires a `provider_type` from the documented enum; it is also " +
        "immutable (absent from ProviderPatchRequest), so a wrong value cannot be patched later",
    );
  }
  if (typeof body["display_name"] !== "string" || body["display_name"].length === 0) {
    throw new MoiraClientContractError(
      "provider create body requires a non-empty `display_name` (schema-required; omitting it is a 400)",
    );
  }
  if (BASE_URL_REQUIRED_PROVIDER_TYPES.includes(providerType)) {
    const baseUrl = body["base_url"];
    if (typeof baseUrl !== "string" || baseUrl.length === 0) {
      throw new MoiraClientContractError(
        `provider_type \`${providerType}\` requires a non-empty \`base_url\`: without one the ` +
          "provider silently falls back to the vendor's public API, so a run the operator " +
          "believed was local leaves their network",
      );
    }
  }
}

/**
 * `PATCH /api/v1/admin/providers/{id}` guard.
 *
 * `provider_type` is refused here rather than sent. `ProviderPatchRequest` is
 * `additionalProperties: false` and does not declare the field, so Moira answers
 * a flat `400` that names nothing — the operator would be shown a generic
 * validation failure for a request that is not invalid but IMPOSSIBLE.
 */
export function assertLlmProviderPatchIsSafe(body: Record<string, unknown>): void {
  if ("provider_type" in body) {
    throw new MoiraClientContractError(
      "`provider_type` is immutable: it is absent from ProviderPatchRequest, and with " +
        "additionalProperties:false the request is a 400 that names no field. Create a new " +
        "provider and re-point the routing policy instead.",
    );
  }
}

/**
 * `POST /api/v1/admin/providers/{provider_id}/models` guard.
 *
 * `capabilities` IS OPTIONAL IN THE SCHEMA AND MUST NOT BE OMITTED — the single
 * most expensive omission on this surface. Absent, it is stored as SQL `null`;
 * routing's capability filter then matches the row against nothing and the first
 * completion fails `no_eligible_model`, an error that names neither the model nor
 * the missing column. `null` is refused for the same reason as absence.
 *
 * `model_key` must be non-empty: it is what the provider is actually asked for,
 * and an empty one produces a 404 from the provider rather than a 400 from Moira.
 */
export function assertProviderModelCreateIsSafe(body: Record<string, unknown>): void {
  if (typeof body["model_key"] !== "string" || body["model_key"].length === 0) {
    throw new MoiraClientContractError(
      "provider model create body requires a non-empty `model_key` (schema-required)",
    );
  }
  if (!("capabilities" in body) || body["capabilities"] === null) {
    throw new MoiraClientContractError(
      "provider model create body must send `capabilities` explicitly: an omitted or null value " +
        "is stored as null, matches no capability filter, and surfaces later as an opaque " +
        "`no_eligible_model` that names neither the model nor the missing field",
    );
  }
}

/**
 * Build the `api_key` arm of `CredentialSecret` — the ONLY sanctioned way to
 * construct one.
 *
 * ============================================================================
 * `endpoint` MUST BE ABSENT, NOT NULL (the untagged-union trap)
 * ============================================================================
 *
 * `CredentialSecret` is `#[serde(untagged)]` and two of its arms start with a
 * required `api_key`: `{ api_key }` and `{ api_key, endpoint? }`. A body of
 * `{ "api_key": "…", "endpoint": null }` satisfies BOTH, so `oneOf` matches
 * twice and the request is refused — with a schema-level rejection that names no
 * field. Sending `endpoint: null` "to be explicit" is therefore the easiest way
 * to make this endpoint permanently unusable, and it is why this returns a fresh
 * object literal rather than spreading anything a caller hands in.
 *
 * ============================================================================
 * AN EMPTY KEY IS REFUSED EVEN FOR A KEYLESS ENDPOINT
 * ============================================================================
 *
 * A local vLLM ignores the `Authorization` header entirely, which makes "leave
 * the key blank" look reasonable. It is not: routing resolves a credential ROW
 * before it builds any request, and a provider with no active credential fails
 * `credential_not_found` — an error that reads as "your key is wrong" when the
 * truth is "there is no key at all". Any non-empty placeholder works; nothing
 * does not.
 */
export function apiKeyCredentialSecret(apiKey: string): ApiKeyCredentialSecret {
  if (typeof apiKey !== "string" || apiKey.length === 0) {
    throw new MoiraClientContractError(
      "the credential secret requires a non-empty `api_key`, including against a keyless " +
        "endpoint: routing resolves a credential row before it builds a request, and a provider " +
        "without one fails `credential_not_found`",
    );
  }
  return { api_key: apiKey };
}

/**
 * `POST /api/v1/admin/provider-credentials` guard.
 *
 * The two properties that are not restatements of the schema:
 *
 *   1. `credential_type: "api_key"` REQUIRES the secret to be EXACTLY
 *      `{ api_key }`. An `endpoint` key — present, even as `null` — makes the
 *      untagged union ambiguous with the azure arm; see
 *      `apiKeyCredentialSecret`.
 *   2. `provider_id` must be a non-empty string that the CALLER has already
 *      resolved and verified server-side. Nothing here can check that, and
 *      saying so is the point: a `provider_id` taken straight from a request
 *      body lets whoever sent it attach a credential to a provider they were
 *      never shown.
 */
export function assertCredentialCreateIsSafe(body: Record<string, unknown>): void {
  if (typeof body["provider_id"] !== "string" || body["provider_id"].length === 0) {
    throw new MoiraClientContractError(
      "credential create body requires a non-empty `provider_id`, resolved and verified " +
        "server-side — never taken from a request body unchecked",
    );
  }
  const scope = body["scope"];
  if (typeof scope !== "object" || scope === null || typeof (scope as { type?: unknown }).type !== "string") {
    throw new MoiraClientContractError(
      "credential create body requires a `scope` carrying a `type` discriminator " +
        "(global | tenant | application | user)",
    );
  }
  const credentialType = body["credential_type"];
  const secret = body["secret"];
  if (typeof secret !== "object" || secret === null) {
    throw new MoiraClientContractError("credential create body requires a `secret` object");
  }
  if (credentialType === "api_key") {
    const keys = Object.keys(secret as Record<string, unknown>).sort();
    if (keys.length !== 1 || keys[0] !== "api_key") {
      throw new MoiraClientContractError(
        "an `api_key` credential secret must be exactly `{ api_key }`: CredentialSecret is " +
          "serde-untagged and `{ api_key, endpoint }` — including `endpoint: null` — matches the " +
          "azure arm as well, so the request is refused as ambiguous with no field named. " +
          `Received keys: ${keys.join(", ")}`,
      );
    }
    const apiKey = (secret as { api_key?: unknown }).api_key;
    if (typeof apiKey !== "string" || apiKey.length === 0) {
      throw new MoiraClientContractError(
        "the credential secret requires a non-empty `api_key`, including against a keyless " +
          "endpoint: a provider with no credential row fails `credential_not_found`",
      );
    }
  }
}

/**
 * `POST .../provider-credentials/{id}/rotate` guard — the same secret rules as
 * create, applied to the replacement key.
 *
 * Written as its own function rather than reusing the create guard: the rotate
 * body has ONE field and carries no `credential_type`, so the arm is inferred
 * from the secret's own shape. A body with `endpoint` alongside `api_key` is
 * legitimate here only when the row it rotates is an azure credential, and the
 * caller — which read that row to obtain the `If-Match` version — is the only
 * party that knows.
 */
export function assertCredentialRotateIsSafe(body: Record<string, unknown>): void {
  const secret = body["secret"];
  if (typeof secret !== "object" || secret === null) {
    throw new MoiraClientContractError("rotate body requires a `secret` object");
  }
  const apiKey = (secret as { api_key?: unknown }).api_key;
  if (apiKey !== undefined && (typeof apiKey !== "string" || apiKey.length === 0)) {
    throw new MoiraClientContractError(
      "rotate body requires a non-empty `api_key` when the secret carries one",
    );
  }
  if ("endpoint" in secret && (secret as { endpoint?: unknown }).endpoint === null) {
    throw new MoiraClientContractError(
      "`endpoint: null` makes the untagged CredentialSecret ambiguous between the api_key and " +
        "azure arms — omit the key entirely rather than sending null",
    );
  }
}

/**
 * `POST /api/v1/admin/routing-policies` guard.
 *
 * All three foreign keys are schema-required, and all three decide WHICH
 * provider live traffic reaches. They must be resolved server-side before this
 * body is built — the caller's obligation, restated on `createRoutingPolicy`,
 * because no client-side check can distinguish a verified id from one that
 * arrived in a request body.
 *
 * The operation documents NO 409, so two identical policies on one route are
 * both stored and both eligible. Deduplicate by listing first.
 */
export function assertRoutingPolicyCreateIsSafe(body: Record<string, unknown>): void {
  for (const field of ["route_id", "provider_id", "provider_model_id"] as const) {
    const value = body[field];
    if (typeof value !== "string" || value.length === 0) {
      throw new MoiraClientContractError(
        `routing policy create body requires a non-empty \`${field}\`, resolved and verified ` +
          "server-side: it selects which provider live traffic reaches",
      );
    }
  }
}

/* -------------------------------------------------------------------------- */
/* Client                                                                     */
/* -------------------------------------------------------------------------- */

export interface MoiraClientOptions {
  /** e.g. `https://moira.internal`. Trailing slashes are trimmed. */
  readonly baseUrl: string;
  /** The bootstrap system key. Required for every `system_key_only` operation. */
  readonly systemKey?: string | undefined;
  /** Resolves the console's minted admin JWT. Unused by the wizard. */
  readonly bearerToken?: (() => string | Promise<string>) | undefined;
  /** Injectable for tests. */
  readonly fetch?: typeof fetch | undefined;
  /** Per-request correlation id, sent as `X-Request-Id`. */
  readonly requestId?: (() => string) | undefined;
}

interface RequestOptions {
  readonly pathParams?: Readonly<Record<string, string>>;
  readonly query?: Readonly<Record<string, string | number | undefined>>;
  readonly body?: unknown;
  /**
   * Only permitted on operations whose `declaresIdempotencyKey` is true.
   * Supplying it elsewhere throws — the console does not send headers the spec
   * does not declare, even though they would be ignored at runtime.
   */
  readonly idempotencyKey?: string | undefined;
  /** Required on operations whose `requiresIfMatch` is true. */
  readonly ifMatch?: string | undefined;
}

/** What a request actually sent. Returned alongside results so flows can trace. */
export interface MoiraRequestRecord {
  readonly operation: MoiraOperationName;
  readonly method: HttpMethod;
  readonly url: string;
  readonly headerNames: readonly string[];
}

export class MoiraClient {
  readonly #baseUrl: string;
  readonly #systemKey: string | undefined;
  readonly #bearerToken: (() => string | Promise<string>) | undefined;
  readonly #fetch: typeof fetch;
  readonly #requestId: (() => string) | undefined;

  constructor(options: MoiraClientOptions) {
    this.#baseUrl = options.baseUrl.replace(/\/+$/, "");
    this.#systemKey = options.systemKey;
    this.#bearerToken = options.bearerToken;
    this.#fetch = options.fetch ?? globalThis.fetch;
    this.#requestId = options.requestId;
  }

  /* ---------------------------------------------------------------------- */
  /* Setup surface                                                          */
  /* ---------------------------------------------------------------------- */

  /**
   * `GET /api/v1/admin/setup/claim-status`. THE ONLY anonymous Moira call in
   * this console. One boolean is the whole contract.
   */
  async getSetupClaimStatus(): Promise<SetupClaimStatusResponse> {
    return this.#request<SetupClaimStatusResponse>("getSetupClaimStatus", {});
  }

  /**
   * `GET /api/v1/admin/setup/auth-methods`. Authenticated on purpose — the
   * response is identity configuration. Called server-side only; the raw
   * response never crosses to the browser.
   */
  async getSetupAuthMethods(): Promise<SetupAuthMethodsResponse> {
    return this.#request<SetupAuthMethodsResponse>("getSetupAuthMethods", {});
  }

  /**
   * `GET /api/v1/admin/setup/sign-in-methods`. ANONYMOUS.
   *
   * Enough to render a button, not enough to resolve the configuration behind
   * it — see the registry entry. `/login` uses it for the provider's
   * `display_name` only, and decides whether to render a button at all from
   * `consoleRuntime()`.
   */
  async getSetupSignInMethods(): Promise<SetupSignInMethodsResponse> {
    return this.#request<SetupSignInMethodsResponse>("getSetupSignInMethods", {});
  }

  /**
   * `POST /api/v1/admin/setup/claim`. System key only — a bearer JWT is refused
   * even if it verifies (`401 setup_claim_credential_required`).
   *
   * `idempotencyKey` should be derived deterministically from `(issuer, subject)`
   * so a double-submit replays with 200 rather than conflicting with 409.
   */
  async claimAdminIdentity(
    body: ConsoleClaimAdminIdentityRequest,
    options: { readonly idempotencyKey?: string } = {},
  ): Promise<AdminIdentityRecord> {
    assertClaimRequestIsSafe(body as unknown as Record<string, unknown>);
    return this.#request<AdminIdentityRecord>("claimAdminIdentity", {
      body,
      idempotencyKey: options.idempotencyKey,
    });
  }

  /* ---------------------------------------------------------------------- */
  /* Trusted JWT issuers                                                    */
  /* ---------------------------------------------------------------------- */

  async listTrustedJwtIssuers(
    options: { readonly limit?: number; readonly cursor?: string } = {},
  ): Promise<ListResponse<TrustedJwtIssuerRecord>> {
    return this.#request<ListResponse<TrustedJwtIssuerRecord>>("listTrustedJwtIssuers", {
      query: { limit: options.limit, cursor: options.cursor },
    });
  }

  /**
   * Exact-match lookup by `issuer`, paging the list.
   *
   * Deliberately not a `?search=` call: `search`'s matching semantics are not
   * part of this console's contract, and an issuer lookup that silently matches
   * a prefix would bind the provider row to the wrong issuer. Exact string
   * comparison here mirrors `resolve_active_issuer`'s own exact match.
   */
  async findTrustedJwtIssuerByIssuer(issuer: string): Promise<TrustedJwtIssuerRecord | null> {
    let cursor: string | undefined;
    // Bounded so a paging bug cannot spin forever during setup.
    for (let page = 0; page < 50; page += 1) {
      const response: ListResponse<TrustedJwtIssuerRecord> = await this.listTrustedJwtIssuers(
        cursor === undefined ? { limit: 100 } : { limit: 100, cursor },
      );
      const match = response.data.find((row) => row.issuer === issuer);
      if (match !== undefined) return match;
      if (!response.pagination.has_more) return null;
      const next = response.pagination.next_cursor;
      if (next === null || next === undefined || next === "") return null;
      cursor = next;
    }
    return null;
  }

  async createTrustedJwtIssuer(
    body: ConsoleTrustedJwtIssuerCreateRequest,
    options: { readonly idempotencyKey?: string } = {},
  ): Promise<TrustedJwtIssuerRecord> {
    assertTrustedIssuerCreateIsSafe(body as unknown as Record<string, unknown>);
    return this.#request<TrustedJwtIssuerRecord>("createTrustedJwtIssuer", {
      body,
      idempotencyKey: options.idempotencyKey,
    });
  }

  /** `POST .../jwt-issuers/{id}/enable`. `If-Match` required; no `Idempotency-Key`. */
  async enableTrustedJwtIssuer(id: string, ifMatch: string): Promise<TrustedJwtIssuerRecord> {
    return this.#request<TrustedJwtIssuerRecord>("enableTrustedJwtIssuer", {
      pathParams: { id },
      ifMatch,
    });
  }

  /* ---------------------------------------------------------------------- */
  /* Auth providers                                                         */
  /* ---------------------------------------------------------------------- */

  async listAuthProviders(
    options: { readonly limit?: number; readonly cursor?: string } = {},
  ): Promise<ListResponse<AuthProviderSettingsRecord>> {
    return this.#request<ListResponse<AuthProviderSettingsRecord>>("listAuthProviders", {
      query: { limit: options.limit, cursor: options.cursor },
    });
  }

  /**
   * `POST /api/v1/admin/auth/providers`.
   *
   * The body type forbids `enabled` and requires `trusted_jwt_issuer_id`; the
   * runtime guard re-checks both for callers that reached here through `any`.
   */
  async createAuthProvider(
    body: ConsoleAuthProviderCreateRequest,
    options: { readonly idempotencyKey?: string } = {},
  ): Promise<AuthProviderSettingsRecord> {
    assertProviderCreateIsSafe(body as unknown as Record<string, unknown>);
    return this.#request<AuthProviderSettingsRecord>("createAuthProvider", {
      body,
      idempotencyKey: options.idempotencyKey,
    });
  }

  async getAuthProvider(id: string): Promise<AuthProviderSettingsRecord> {
    return this.#request<AuthProviderSettingsRecord>("getAuthProvider", { pathParams: { id } });
  }

  async patchAuthProvider(
    id: string,
    body: Readonly<Record<string, unknown>>,
    ifMatch: string,
  ): Promise<AuthProviderSettingsRecord> {
    if ("enabled" in body) {
      throw new MoiraClientContractError(
        "use enableAuthProvider()/disableAuthProvider() — `enabled` is not patched directly",
      );
    }
    return this.#request<AuthProviderSettingsRecord>("patchAuthProvider", {
      pathParams: { id },
      body,
      ifMatch,
    });
  }

  async deleteAuthProvider(id: string, ifMatch: string): Promise<void> {
    await this.#request<void>("deleteAuthProvider", { pathParams: { id }, ifMatch });
  }

  /**
   * `POST .../auth/providers/{id}/enable` — the dual write's commit point.
   *
   * Carries `If-Match` and NO `Idempotency-Key`: the spec does not declare one on
   * this operation. Retry safety is `If-Match` plus `enable` being naturally
   * idempotent, and that is the whole of it.
   */
  async enableAuthProvider(id: string, ifMatch: string): Promise<AuthProviderSettingsRecord> {
    return this.#request<AuthProviderSettingsRecord>("enableAuthProvider", {
      pathParams: { id },
      ifMatch,
    });
  }

  async disableAuthProvider(id: string, ifMatch: string): Promise<AuthProviderSettingsRecord> {
    return this.#request<AuthProviderSettingsRecord>("disableAuthProvider", {
      pathParams: { id },
      ifMatch,
    });
  }

  /* ---------------------------------------------------------------------- */
  /* Admin invitations                                                      */
  /* ---------------------------------------------------------------------- */

  /**
   * `POST /api/v1/admin/admin-invites` — mint a once-only invitation token.
   *
   * The response is the ONLY time the raw token exists outside Moira's hash.
   * Note what this method does NOT do: it does not log the response, does not
   * pass it through `lib/errors.ts`, and does not cache it. `#request` returns a
   * 2xx body raw — `toMoiraError` is called only under `if (!response.ok)` — so
   * there is nothing between the JSON parse and whatever the caller does next.
   *
   * `idempotencyKey` should be derived from the invite's own identity
   * `(constraint, value)`. A replay returns `secret: null` with the sanitized
   * record, which is the correct and expected outcome, not a failure.
   */
  async createAdminInvite(
    body: AdminInviteCreateRequest,
    options: { readonly idempotencyKey?: string } = {},
  ): Promise<AdminInviteSecretResponse> {
    return this.#request<AdminInviteSecretResponse>("createAdminInvite", {
      body,
      idempotencyKey: options.idempotencyKey,
    });
  }

  async listAdminInvites(
    options: { readonly limit?: number; readonly cursor?: string; readonly status?: string } = {},
  ): Promise<ListResponse<AdminInviteRecord>> {
    return this.#request<ListResponse<AdminInviteRecord>>("listAdminInvites", {
      query: { limit: options.limit, cursor: options.cursor, status: options.status },
    });
  }

  async getAdminInvite(id: string): Promise<AdminInviteRecord> {
    return this.#request<AdminInviteRecord>("getAdminInvite", { pathParams: { id } });
  }

  /** `POST .../admin-invites/{id}/revoke`. No `If-Match` — see the registry entry. */
  async revokeAdminInvite(
    id: string,
    options: { readonly idempotencyKey?: string } = {},
  ): Promise<AdminInviteRecord> {
    return this.#request<AdminInviteRecord>("revokeAdminInvite", {
      pathParams: { id },
      idempotencyKey: options.idempotencyKey,
    });
  }

  /**
   * `POST .../admin-invites/preview`. Anonymous; the token travels in the BODY.
   *
   * The response carries `constraint`, `value` and `expires_at` and nothing
   * else — no inviter, no invite id, no policy. Note what this method does NOT
   * return: the token it was given. Nothing here echoes it.
   */
  async previewAdminInvite(token: string): Promise<AdminInvitePreviewResponse> {
    const body: AdminInvitePreviewRequest = { token };
    return this.#request<AdminInvitePreviewResponse>("previewAdminInvite", { body });
  }

  /**
   * `POST .../admin-invites/redeem`. The invitee's own bearer token, never the
   * system key — the client throws if one is configured.
   *
   * `email`/`email_verified` are BFF-asserted from the just-verified session by
   * the caller; this method does not read them from anywhere else, and there is
   * no branch that makes either omittable.
   */
  async redeemAdminInvite(
    body: AdminInviteRedeemRequest,
    options: { readonly idempotencyKey?: string } = {},
  ): Promise<AdminIdentityRecord> {
    return this.#request<AdminIdentityRecord>("redeemAdminInvite", {
      body,
      idempotencyKey: options.idempotencyKey,
    });
  }

  /* ---------------------------------------------------------------------- */
  /* Admin identities — the ownership surface                               */
  /* ---------------------------------------------------------------------- */

  async listAdminIdentities(
    options: { readonly limit?: number; readonly cursor?: string; readonly status?: string } = {},
  ): Promise<ListResponse<AdminIdentityRecord>> {
    return this.#request<ListResponse<AdminIdentityRecord>>("listAdminIdentities", {
      query: { limit: options.limit, cursor: options.cursor, status: options.status },
    });
  }

  /**
   * `PATCH .../admin-identities/{id}` — ownership transfer, in ONE call.
   *
   * Use `ifMatchFor(record)` rather than a fabricated version: the precondition
   * is what stops a transfer racing a concurrent revocation of the same row.
   */
  async patchAdminIdentity(
    id: string,
    body: AdminIdentityPatchRequest,
    ifMatch: string,
    options: { readonly idempotencyKey?: string } = {},
  ): Promise<AdminIdentityRecord> {
    return this.#request<AdminIdentityRecord>("patchAdminIdentity", {
      pathParams: { id },
      body,
      ifMatch,
      idempotencyKey: options.idempotencyKey,
    });
  }

  /**
   * `DELETE .../admin-identities/{id}` — soft revoke. Returns the revoked record.
   *
   * NOT a 204: the response is the `AdminIdentityRecord` in its revoked state,
   * with the `admin_identity_revoked` notice. Typing it as `void` would drop the
   * one string the operator is meant to be shown.
   *
   * THE SOLE ADMIN CANNOT BE REVOKED. `revoke_grant` clears `is_primary`, and the
   * last-primary guard refuses that — so on a deployment with one admin this is a
   * `409 admin_identity_last_primary` for that row, permanently. That is a stated
   * consequence of decision D-F20, not an outage: the caller must render it as a
   * rule with a remedy ("transfer ownership first"), never as a failed request.
   */
  async deleteAdminIdentity(
    id: string,
    options: { readonly idempotencyKey?: string } = {},
  ): Promise<AdminIdentityRecord> {
    return this.#request<AdminIdentityRecord>("deleteAdminIdentity", {
      pathParams: { id },
      idempotencyKey: options.idempotencyKey,
    });
  }

  /* ---------------------------------------------------------------------- */
  /* LLM providers (issue #73)                                              */
  /* ---------------------------------------------------------------------- */

  async listProviders(
    options: {
      readonly limit?: number;
      readonly cursor?: string;
      readonly status?: string;
      readonly search?: string;
    } = {},
  ): Promise<ListResponse<ProviderRecord>> {
    return this.#request<ListResponse<ProviderRecord>>("listProviders", {
      query: {
        limit: options.limit,
        cursor: options.cursor,
        status: options.status,
        search: options.search,
      },
    });
  }

  /**
   * `POST /api/v1/admin/providers`.
   *
   * `idempotencyKey` should be derived from the provider's own identity — its
   * `(provider_type, base_url)` pair — so a double-submit replays instead of
   * landing two providers that routing then has to choose between. The operation
   * documents a 409, unlike routes and routing policies.
   */
  async createProvider(
    body: ProviderCreateRequest,
    options: { readonly idempotencyKey?: string } = {},
  ): Promise<ProviderRecord> {
    assertLlmProviderCreateIsSafe(body as unknown as Record<string, unknown>);
    return this.#request<ProviderRecord>("createProvider", {
      body,
      idempotencyKey: options.idempotencyKey,
    });
  }

  async getProvider(id: string): Promise<ProviderRecord> {
    return this.#request<ProviderRecord>("getProvider", { pathParams: { id } });
  }

  /** `PATCH .../providers/{id}`. `If-Match` required; `provider_type` refused. */
  async patchProvider(
    id: string,
    body: ProviderPatchRequest,
    ifMatch: string,
  ): Promise<ProviderRecord> {
    assertLlmProviderPatchIsSafe(body as unknown as Record<string, unknown>);
    return this.#request<ProviderRecord>("patchProvider", {
      pathParams: { id },
      body,
      ifMatch,
    });
  }

  /**
   * `POST .../providers/{id}/enable` — the commit point for a provider.
   *
   * `If-Match` and NO `Idempotency-Key`: the spec declares no key here, exactly
   * as on the auth-provider surface, so retry safety is the precondition plus
   * `enable` being naturally idempotent. Use `ifMatchFor(record)` — a fabricated
   * version defeats the only thing stopping this from racing a concurrent patch.
   */
  async enableProvider(id: string, ifMatch: string): Promise<ProviderRecord> {
    return this.#request<ProviderRecord>("enableProvider", { pathParams: { id }, ifMatch });
  }

  async disableProvider(id: string, ifMatch: string): Promise<ProviderRecord> {
    return this.#request<ProviderRecord>("disableProvider", { pathParams: { id }, ifMatch });
  }

  /* ---------------------------------------------------------------------- */
  /* Provider models                                                        */
  /* ---------------------------------------------------------------------- */

  /** `GET .../providers/{provider_id}/models` — nested under the provider. */
  async listProviderModels(
    providerId: string,
    options: { readonly limit?: number; readonly cursor?: string; readonly status?: string } = {},
  ): Promise<ListResponse<ProviderModelRecord>> {
    return this.#request<ListResponse<ProviderModelRecord>>("listProviderModels", {
      pathParams: { provider_id: providerId },
      query: { limit: options.limit, cursor: options.cursor, status: options.status },
    });
  }

  /**
   * `POST .../providers/{provider_id}/models`.
   *
   * The body type requires `capabilities`; the runtime guard re-checks it for
   * callers that reached here through `any`. Omitting it is the
   * `no_eligible_model` defect described on `assertProviderModelCreateIsSafe`.
   */
  async createProviderModel(
    providerId: string,
    body: ConsoleProviderModelCreateRequest,
    options: { readonly idempotencyKey?: string } = {},
  ): Promise<ProviderModelRecord> {
    assertProviderModelCreateIsSafe(body as unknown as Record<string, unknown>);
    return this.#request<ProviderModelRecord>("createProviderModel", {
      pathParams: { provider_id: providerId },
      body,
      idempotencyKey: options.idempotencyKey,
    });
  }

  /** `POST /api/v1/admin/provider-models/{id}/enable` — FLAT, not nested. */
  async enableProviderModel(id: string, ifMatch: string): Promise<ProviderModelRecord> {
    return this.#request<ProviderModelRecord>("enableProviderModel", {
      pathParams: { id },
      ifMatch,
    });
  }

  async disableProviderModel(id: string, ifMatch: string): Promise<ProviderModelRecord> {
    return this.#request<ProviderModelRecord>("disableProviderModel", {
      pathParams: { id },
      ifMatch,
    });
  }

  /* ---------------------------------------------------------------------- */
  /* Provider credentials — WRITE-ONLY SECRETS                              */
  /* ---------------------------------------------------------------------- */
  //
  // Nothing on this surface returns a raw key: Moira answers with
  // `CredentialRecord`, whose `masked_secret` is a redaction and whose
  // `secret_fingerprint` is a hash. The rule that matters is on the way IN — the
  // plaintext exists on this server for the duration of one request and must not
  // be logged, echoed into an error, cached, or reflected into a form's default
  // value. `#request` never logs a body, and these methods add nothing that
  // would.
  //
  // The DTOs live in `lib/moira-credential-types.ts`, which is server-only, so a
  // client component cannot even name the shapes.

  /**
   * `GET /api/v1/admin/provider-credentials`, optionally filtered by provider.
   *
   * `provider_id` is a documented query parameter on this operation — filtering
   * server-side rather than listing everything and matching in the console keeps
   * other providers' credential rows out of this process entirely.
   */
  async listProviderCredentials(
    options: {
      readonly providerId?: string;
      readonly limit?: number;
      readonly cursor?: string;
      readonly status?: string;
    } = {},
  ): Promise<ListResponse<CredentialRecord>> {
    return this.#request<ListResponse<CredentialRecord>>("listProviderCredentials", {
      query: {
        provider_id: options.providerId,
        limit: options.limit,
        cursor: options.cursor,
        status: options.status,
      },
    });
  }

  /**
   * `POST /api/v1/admin/provider-credentials`.
   *
   * CALLER'S OBLIGATION: `body.provider_id` must be an identifier the caller
   * resolved and verified on the server — never one lifted from a request body.
   * A privileged write whose target is chosen by the browser is a privileged
   * write the browser controls, and no check inside this client can tell a
   * verified id from an unverified one.
   *
   * Build `body.secret` with `apiKeyCredentialSecret()`. Assembling the object
   * by hand is how `endpoint: null` gets in, which makes the untagged union
   * ambiguous and the request permanently refused.
   */
  async createProviderCredential(
    body: ConsoleApiKeyCredentialCreateRequest,
    options: { readonly idempotencyKey?: string } = {},
  ): Promise<CredentialRecord> {
    assertCredentialCreateIsSafe(body as unknown as Record<string, unknown>);
    return this.#request<CredentialRecord>("createProviderCredential", {
      body,
      idempotencyKey: options.idempotencyKey,
    });
  }

  async enableProviderCredential(id: string, ifMatch: string): Promise<CredentialRecord> {
    return this.#request<CredentialRecord>("enableProviderCredential", {
      pathParams: { id },
      ifMatch,
    });
  }

  async disableProviderCredential(id: string, ifMatch: string): Promise<CredentialRecord> {
    return this.#request<CredentialRecord>("disableProviderCredential", {
      pathParams: { id },
      ifMatch,
    });
  }

  /**
   * `POST .../provider-credentials/{id}/rotate` — replace the stored key in
   * place, keeping the row id and every policy bound to it.
   *
   * The only operation here that carries BOTH a required `If-Match` and an
   * optional `Idempotency-Key`, and both are used: the precondition refuses a
   * rotation onto a row that changed under the operator, and the key stops a
   * retry from installing a second replacement whose value nothing reported.
   */
  async rotateProviderCredential(
    id: string,
    body: RotateCredentialRequest,
    ifMatch: string,
    options: { readonly idempotencyKey?: string } = {},
  ): Promise<CredentialRecord> {
    assertCredentialRotateIsSafe(body as unknown as Record<string, unknown>);
    return this.#request<CredentialRecord>("rotateProviderCredential", {
      pathParams: { id },
      body,
      ifMatch,
      idempotencyKey: options.idempotencyKey,
    });
  }

  /* ---------------------------------------------------------------------- */
  /* Routes — READ ONLY                                                     */
  /* ---------------------------------------------------------------------- */

  async listRoutes(
    options: { readonly limit?: number; readonly cursor?: string; readonly status?: string } = {},
  ): Promise<ListResponse<RouteDefinitionRecord>> {
    return this.#request<ListResponse<RouteDefinitionRecord>>("listRoutes", {
      query: { limit: options.limit, cursor: options.cursor, status: options.status },
    });
  }

  async getRoute(id: string): Promise<RouteDefinitionRecord> {
    return this.#request<RouteDefinitionRecord>("getRoute", { pathParams: { id } });
  }

  /**
   * Exact-match lookup by `route_key`, paging the list.
   *
   * The same shape as `findTrustedJwtIssuerByIssuer`, and for the same reason:
   * `?search=` has matching semantics that are not part of this console's
   * contract, and binding a routing policy to a prefix-matched route would look
   * like it worked. The `general` route is seeded by migration `0005`, so this
   * returning `null` means the deployment is not migrated — not that the console
   * should create one.
   */
  async findRouteByKey(routeKey: string): Promise<RouteDefinitionRecord | null> {
    let cursor: string | undefined;
    // Bounded so a paging bug cannot spin forever behind a settings page.
    for (let page = 0; page < 50; page += 1) {
      const response: ListResponse<RouteDefinitionRecord> = await this.listRoutes(
        cursor === undefined ? { limit: 100 } : { limit: 100, cursor },
      );
      const match = response.data.find((row) => row.route_key === routeKey);
      if (match !== undefined) return match;
      if (!response.pagination.has_more) return null;
      const next = response.pagination.next_cursor;
      if (next === null || next === undefined || next === "") return null;
      cursor = next;
    }
    return null;
  }

  /* ---------------------------------------------------------------------- */
  /* Routing policies                                                       */
  /* ---------------------------------------------------------------------- */

  async listRoutingPolicies(
    options: { readonly limit?: number; readonly cursor?: string; readonly status?: string } = {},
  ): Promise<ListResponse<RoutingPolicyRecord>> {
    return this.#request<ListResponse<RoutingPolicyRecord>>("listRoutingPolicies", {
      query: { limit: options.limit, cursor: options.cursor, status: options.status },
    });
  }

  /**
   * `POST /api/v1/admin/routing-policies`.
   *
   * CALLER'S OBLIGATION, as on `createProviderCredential`: `route_id`,
   * `provider_id` and `provider_model_id` must all be server-resolved. Together
   * they decide which provider live traffic reaches.
   *
   * NO DOCUMENTED 409. Two identical policies on one route are both stored and
   * both eligible, so dedupe by listing first — there is no uniqueness
   * constraint to lean on and no error to catch.
   */
  async createRoutingPolicy(
    body: RoutingPolicyCreateRequest,
    options: { readonly idempotencyKey?: string } = {},
  ): Promise<RoutingPolicyRecord> {
    assertRoutingPolicyCreateIsSafe(body as unknown as Record<string, unknown>);
    return this.#request<RoutingPolicyRecord>("createRoutingPolicy", {
      body,
      idempotencyKey: options.idempotencyKey,
    });
  }

  async patchRoutingPolicy(
    id: string,
    body: RoutingPolicyPatchRequest,
    ifMatch: string,
  ): Promise<RoutingPolicyRecord> {
    return this.#request<RoutingPolicyRecord>("patchRoutingPolicy", {
      pathParams: { id },
      body,
      ifMatch,
    });
  }

  async enableRoutingPolicy(id: string, ifMatch: string): Promise<RoutingPolicyRecord> {
    return this.#request<RoutingPolicyRecord>("enableRoutingPolicy", {
      pathParams: { id },
      ifMatch,
    });
  }

  async disableRoutingPolicy(id: string, ifMatch: string): Promise<RoutingPolicyRecord> {
    return this.#request<RoutingPolicyRecord>("disableRoutingPolicy", {
      pathParams: { id },
      ifMatch,
    });
  }

  /* ---------------------------------------------------------------------- */
  /* Transport                                                              */
  /* ---------------------------------------------------------------------- */

  async #request<T>(name: MoiraOperationName, options: RequestOptions): Promise<T> {
    const operation: MoiraOperation = MOIRA_OPERATIONS[name];
    const url = this.#buildUrl(operation, options);
    const headers = await this.#buildHeaders(operation, options);

    let response: Response;
    try {
      response = await this.#fetch(url, {
        method: operation.method,
        headers,
        ...(options.body === undefined ? {} : { body: JSON.stringify(options.body) }),
      });
    } catch (cause) {
      throw new MoiraRequestError(toTransportError(cause));
    }

    if (!response.ok) {
      let parsed: unknown;
      try {
        parsed = await response.json();
      } catch {
        parsed = undefined;
      }
      throw new MoiraRequestError(toMoiraError(response.status, parsed));
    }

    if (response.status === 204) return undefined as T;
    return (await response.json()) as T;
  }

  #buildUrl(operation: MoiraOperation, options: RequestOptions): string {
    let path = operation.path;
    const params = options.pathParams ?? {};
    for (const [key, value] of Object.entries(params)) {
      const token = `{${key}}`;
      if (!path.includes(token)) {
        throw new MoiraClientContractError(
          `operation ${operation.id} has no path parameter ${token}`,
        );
      }
      path = path.replace(token, encodeURIComponent(value));
    }
    if (/\{[^}]+\}/.test(path)) {
      throw new MoiraClientContractError(`unsubstituted path parameter in ${path}`);
    }

    const search = new URLSearchParams();
    for (const [key, value] of Object.entries(options.query ?? {})) {
      if (value === undefined) continue;
      search.set(key, String(value));
    }
    const suffix = search.size > 0 ? `?${search.toString()}` : "";
    return `${this.#baseUrl}${path}${suffix}`;
  }

  async #buildHeaders(
    operation: MoiraOperation,
    options: RequestOptions,
  ): Promise<Record<string, string>> {
    const headers: Record<string, string> = {};

    if (options.body !== undefined) headers["Content-Type"] = "application/json";
    headers["Accept"] = "application/json";

    // --- credential, straight from the registry ---------------------------
    switch (operation.credential) {
      case "none":
        // Deliberately nothing. `claim-status` is anonymous by contract.
        break;
      case "system_key_only": {
        if (this.#systemKey === undefined || this.#systemKey === "") {
          throw new MoiraClientContractError(
            `${operation.id} requires the bootstrap system key (X-Moira-System-Key); ` +
              "no bearer token is accepted on this operation",
          );
        }
        headers["X-Moira-System-Key"] = this.#systemKey;
        break;
      }
      case "admin": {
        if (this.#systemKey !== undefined && this.#systemKey !== "") {
          headers["X-Moira-System-Key"] = this.#systemKey;
        } else if (this.#bearerToken !== undefined) {
          headers["Authorization"] = `Bearer ${await this.#bearerToken()}`;
        } else {
          throw new MoiraClientContractError(
            `${operation.id} requires a credential: configure systemKey or bearerToken`,
          );
        }
        break;
      }
      case "bearer_only": {
        // THE REFUSAL IS THE POINT, and it is checked BEFORE the bearer is
        // resolved so that a misconfigured client cannot mint a token on the way
        // to being rejected.
        //
        // The `admin` arm above prefers the system key when one is present. If
        // redemption went through that arm on a client that carries the
        // bootstrap key — which `moiraClientForSetup` does — the console would
        // present its own break-glass credential on a request that mints an
        // `admin_identities` grant for whichever `(issuer, subject)` the body
        // implies. That is not a leak of the key; it is the console granting
        // admin to an identity of its own choosing, and no server-side check can
        // distinguish it from a legitimate operator action.
        if (this.#systemKey !== undefined && this.#systemKey !== "") {
          throw new MoiraClientContractError(
            `${operation.id} declares bearerAuth alone and must NEVER carry the bootstrap ` +
              "system key: a system-key redemption would be the console granting admin to an " +
              "identity of its own choosing. Build the client from the invitee's session " +
              "(moiraClientForSession), not from moiraClientForSetup.",
          );
        }
        if (this.#bearerToken === undefined) {
          throw new MoiraClientContractError(
            `${operation.id} requires the caller's own bearer token; there is no system-key ` +
              "fallback on this operation",
          );
        }
        headers["Authorization"] = `Bearer ${await this.#bearerToken()}`;
        break;
      }
    }

    // --- Idempotency-Key: only where the spec declares it -----------------
    if (options.idempotencyKey !== undefined) {
      if (!operation.declaresIdempotencyKey) {
        throw new MoiraClientContractError(
          `${operation.id} does not declare an Idempotency-Key parameter; ` +
            "retry safety there comes from If-Match and natural idempotence",
        );
      }
      headers["Idempotency-Key"] = options.idempotencyKey;
    }

    // --- If-Match: required where the spec says required ------------------
    if (operation.requiresIfMatch) {
      if (options.ifMatch === undefined || options.ifMatch === "") {
        throw new MoiraClientContractError(
          `${operation.id} requires If-Match; read the resource first rather than fabricating a version`,
        );
      }
      headers["If-Match"] = options.ifMatch;
    } else if (options.ifMatch !== undefined) {
      throw new MoiraClientContractError(`${operation.id} does not declare an If-Match parameter`);
    }

    if (this.#requestId !== undefined) headers["X-Request-Id"] = this.#requestId();

    return headers;
  }
}

/** `If-Match` value for a record's current version. */
export function ifMatchFor(record: { readonly version: number }): string {
  return String(record.version);
}
