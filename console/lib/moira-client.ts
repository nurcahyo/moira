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
  ApiKeyRecord,
  ApiKeySecretResponse,
  ConsumerKeyCreateRequest,
} from "./moira-api-key-types";
import type {
  ApiKeyCredentialSecret,
  ConsoleApiKeyCredentialCreateRequest,
  ConsoleOAuth2CredentialCreateRequest,
  CredentialRecord,
  OAuth2CredentialSecret,
  RotateCredentialRequest,
} from "./moira-credential-types";
import type {
  AdminIdentityPatchRequest,
  AgentFlowCreateRequest,
  AgentFlowPatchRequest,
  AgentFlowRecord,
  AgentFlowRunRecord,
  AgentProfileRecord,
  ApplicationCreateRequest,
  ApplicationRecord,
  AdminIdentityRecord,
  AdminInviteCreateRequest,
  AdminInvitePreviewRequest,
  AdminInvitePreviewResponse,
  AdminInviteRecord,
  AdminInviteRedeemRequest,
  AdminInviteSecretResponse,
  AuthProviderSettingsRecord,
  ClaudeRunnerAuthorizationCodeRequest,
  ClaudeRunnerFinalizeRequest,
  ClaudeRunnerProvisionRequest,
  ClaudeRunnerRecord,
  ConsoleAuthProviderCreateRequest,
  ConsoleClaimAdminIdentityRequest,
  ConsoleTrustedJwtIssuerCreateRequest,
  ConsoleProviderModelCreateRequest,
  DiagnosticExecutionRequest,
  DiagnosticExecutionResponse,
  EvalCaseCreateRequest,
  EvalCaseRecord,
  EvalRunRecord,
  EvalSuiteCreateRequest,
  EvalSuitePatchRequest,
  EvalSuiteRecord,
  GraphResponse,
  ListResponse,
  ProviderCreateRequest,
  ProviderHealthResponse,
  ProviderModelRecord,
  ProviderPatchRequest,
  ProviderRecord,
  PublicExecutionSummary,
  PublicResponse,
  PublicResponseRequest,
  RouteDefinitionRecord,
  RoutingPolicyCreateRequest,
  RoutingPolicyPatchRequest,
  RoutingPolicyRecord,
  SetupAuthMethodsResponse,
  SetupClaimStatusResponse,
  SetupSignInMethodsResponse,
  SkillBulkEnableRequest,
  SkillBulkEnableResponse,
  SkillCreateRequest,
  SkillHttpExecutorPatchRequest,
  SkillHttpExecutorRecord,
  SkillImportRequest,
  SkillImportResponse,
  SkillPatchRequest,
  SkillRecord,
  TrustedJwtIssuerRecord,
} from "./types";
import { CLAUDE_RUNNER_LABEL_PATTERN } from "./types";

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

  /**
   * Applications and consumer keys (issue #180) — the credential an APPLICATION
   * presents to Moira, as opposed to the credential Moira presents to a provider.
   *
   * Two families, not one, and not by choice: `ConsumerKeyCreateRequest` requires
   * an `application_id`, so a console that mints keys must be able to create and
   * list applications too.
   *
   * THE HEADER SHAPES ARE NOT THE PROVIDER FAMILIES' SHAPES. Transcribed from
   * `docs/openapi.json`, where they differ in ways worth naming because guessing
   * by analogy gets each one wrong:
   *
   *   `revoke_consumer_key`   POST, and declares NEITHER `If-Match` NOR
   *                           `Idempotency-Key` — unlike every provider-family
   *                           disable, which requires `If-Match`. Sending one is
   *                           an unknown header, not a stricter request.
   *   `create_consumer_key`   optional `Idempotency-Key`, no `If-Match`.
   *   `create_application`    same.
   *
   * `delete_*` and `rotate_consumer_key` are deliberately UNREGISTERED. Deletion
   * of a key with live traffic is not an operation an operator should reach
   * through two clicks — revoke is the reversible-in-consequence one and is what
   * the screen offers; rotation is a real need with a real design question (the
   * overlap window, `revoke_previous_immediately`) that this issue does not
   * settle, and a half-built rotate is worse than none.
   */
  listApplications: op({
    id: "list_applications",
    method: "GET",
    path: "/api/v1/admin/applications",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: false,
  }),
  createApplication: op({
    id: "create_application",
    method: "POST",
    path: "/api/v1/admin/applications",
    credential: "admin",
    declaresIdempotencyKey: true,
    requiresIfMatch: false,
  }),
  listConsumerKeys: op({
    id: "list_consumer_keys",
    method: "GET",
    path: "/api/v1/admin/consumer-keys",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: false,
  }),
  createConsumerKey: op({
    id: "create_consumer_key",
    method: "POST",
    path: "/api/v1/admin/consumer-keys",
    credential: "admin",
    declaresIdempotencyKey: true,
    requiresIfMatch: false,
  }),
  revokeConsumerKey: op({
    id: "revoke_consumer_key",
    method: "POST",
    path: "/api/v1/admin/consumer-keys/{id}/revoke",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: false,
  }),
  /**
   * `GET /api/v1/admin/graph` (plan 12 §4, issue #234) — the derived, read-only
   * relationship graph over the agent-platform and provider/model registries.
   * No `Idempotency-Key`, no `If-Match`: nothing here ever writes.
   */
  getGraph: op({
    id: "get_graph",
    method: "GET",
    path: "/api/v1/admin/graph",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: false,
  }),

  /* ---------------------------------------------------------------------- */
  /* Skills (plan 12 §5) — tool/guard registry, HTTP executors             */
  /* ---------------------------------------------------------------------- */

  listSkills: op({
    id: "list_skills",
    method: "GET",
    path: "/api/v1/admin/skills",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: false,
  }),
  createSkill: op({
    id: "create_skill",
    method: "POST",
    path: "/api/v1/admin/skills",
    credential: "admin",
    declaresIdempotencyKey: true,
    requiresIfMatch: false,
  }),
  getSkill: op({
    id: "get_skill",
    method: "GET",
    path: "/api/v1/admin/skills/{id}",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: false,
  }),
  patchSkill: op({
    id: "patch_skill",
    method: "PATCH",
    path: "/api/v1/admin/skills/{id}",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: true,
  }),
  deleteSkill: op({
    id: "delete_skill",
    method: "DELETE",
    path: "/api/v1/admin/skills/{id}",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: true,
  }),
  enableSkill: op({
    id: "enable_skill",
    method: "POST",
    path: "/api/v1/admin/skills/{id}/enable",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: true,
  }),
  disableSkill: op({
    id: "disable_skill",
    method: "POST",
    path: "/api/v1/admin/skills/{id}/disable",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: true,
  }),
  /**
   * `POST /api/v1/admin/skills/bulk-enable` (plan 12 §5 decision 22) — the
   * reason a large imported spec does not become hundreds of clicks. No
   * `If-Match`: it is a multi-row operation with no single row version.
   */
  bulkEnableSkills: op({
    id: "bulk_enable_skills",
    method: "POST",
    path: "/api/v1/admin/skills/bulk-enable",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: false,
  }),
  /**
   * `POST /api/v1/admin/skills/import` — parses an OpenAPI 3.x document,
   * SSRF-validates the server URL, and creates one `draft` skill plus one HTTP
   * executor per operation, capped at 300 (plan 12 §5 decision 23). Creates
   * disabled rows only; `enableSkill`/`bulkEnableSkills` is the review step.
   */
  importSkills: op({
    id: "import_skills",
    method: "POST",
    path: "/api/v1/admin/skills/import",
    credential: "admin",
    declaresIdempotencyKey: true,
    requiresIfMatch: false,
  }),
  listSkillExecutors: op({
    id: "list_skill_executors",
    method: "GET",
    path: "/api/v1/admin/skill-executors",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: false,
  }),
  /**
   * `GET .../skills/{id}/executor` — `If-Match` on this family is the QUOTED
   * RFC 3339 `updated_at`, not an integer version. See
   * `SkillHttpExecutorRecord`'s doc comment and `skillExecutorIfMatchFor` below.
   */
  getSkillExecutor: op({
    id: "get_skill_executor",
    method: "GET",
    path: "/api/v1/admin/skills/{id}/executor",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: false,
  }),
  patchSkillExecutor: op({
    id: "patch_skill_executor",
    method: "PATCH",
    path: "/api/v1/admin/skills/{id}/executor",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: true,
  }),
  deleteSkillExecutor: op({
    id: "delete_skill_executor",
    method: "DELETE",
    path: "/api/v1/admin/skills/{id}/executor",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: true,
  }),

  /* ---------------------------------------------------------------------- */
  /* Provider health (issue #83) — read-only                               */
  /* ---------------------------------------------------------------------- */

  getProviderHealth: op({
    id: "get_provider_health",
    method: "GET",
    path: "/api/v1/admin/providers/health",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: false,
  }),

  /* ---------------------------------------------------------------------- */
  /* Eval suites (plan 12 §3)                                               */
  /* ---------------------------------------------------------------------- */

  listEvalSuites: op({
    id: "list_eval_suites",
    method: "GET",
    path: "/api/v1/admin/eval-suites",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: false,
  }),
  createEvalSuite: op({
    id: "create_eval_suite",
    method: "POST",
    path: "/api/v1/admin/eval-suites",
    credential: "admin",
    declaresIdempotencyKey: true,
    requiresIfMatch: false,
  }),
  getEvalSuite: op({
    id: "get_eval_suite",
    method: "GET",
    path: "/api/v1/admin/eval-suites/{id}",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: false,
  }),
  patchEvalSuite: op({
    id: "patch_eval_suite",
    method: "PATCH",
    path: "/api/v1/admin/eval-suites/{id}",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: true,
  }),
  /** Soft-deletes the suite (`status: "deleted"`) — there is no restore endpoint. */
  deleteEvalSuite: op({
    id: "delete_eval_suite",
    method: "DELETE",
    path: "/api/v1/admin/eval-suites/{id}",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: true,
  }),
  listEvalCases: op({
    id: "list_eval_cases",
    method: "GET",
    path: "/api/v1/admin/eval-suites/{id}/cases",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: false,
  }),
  createEvalCase: op({
    id: "create_eval_case",
    method: "POST",
    path: "/api/v1/admin/eval-suites/{id}/cases",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: false,
  }),
  /** No `If-Match` — `EvalCaseRecord` carries no `version`. */
  deleteEvalCase: op({
    id: "delete_eval_case",
    method: "DELETE",
    path: "/api/v1/admin/eval-suites/{id}/cases/{case_id}",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: false,
  }),
  listEvalRuns: op({
    id: "list_eval_runs",
    method: "GET",
    path: "/api/v1/admin/eval-suites/{id}/runs",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: false,
  }),

  /* ---------------------------------------------------------------------- */
  /* Flows (plan 12 §6) — sequential-only MVP                               */
  /* ---------------------------------------------------------------------- */

  listFlows: op({
    id: "list_flows",
    method: "GET",
    path: "/api/v1/admin/flows",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: false,
  }),
  createFlow: op({
    id: "create_flow",
    method: "POST",
    path: "/api/v1/admin/flows",
    credential: "admin",
    declaresIdempotencyKey: true,
    requiresIfMatch: false,
  }),
  getFlow: op({
    id: "get_flow",
    method: "GET",
    path: "/api/v1/admin/flows/{id}",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: false,
  }),
  patchFlow: op({
    id: "patch_flow",
    method: "PATCH",
    path: "/api/v1/admin/flows/{id}",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: true,
  }),
  /** Soft-deletes the flow (`status: "deleted"`) — there is no restore endpoint. */
  deleteFlow: op({
    id: "delete_flow",
    method: "DELETE",
    path: "/api/v1/admin/flows/{id}",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: true,
  }),
  listFlowRuns: op({
    id: "list_flow_runs",
    method: "GET",
    path: "/api/v1/admin/flows/{id}/runs",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: false,
  }),

  /**
   * `GET /api/v1/admin/agent-profiles` — read-only, for the flow step builder's
   * "which agent profile" picker. The console owns no create/edit surface for
   * agent profiles; only the list operation is registered.
   */
  listAgentProfiles: op({
    id: "list_agent_profiles",
    method: "GET",
    path: "/api/v1/admin/agent-profiles",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: false,
  }),

  /* ---------------------------------------------------------------------- */
  /* The playground (issue #261) — the real execution path                  */
  /* ---------------------------------------------------------------------- */

  /**
   * `POST /api/v1/responses` — the non-streaming fallback. Declares an
   * OPTIONAL `Idempotency-Key`; the playground does not send one (a replay of
   * an identical prompt should run again, not silently return the first
   * answer), which is legal — the header is declared, not required.
   */
  createResponse: op({
    id: "create_response",
    method: "POST",
    path: "/api/v1/responses",
    credential: "admin",
    declaresIdempotencyKey: true,
    requiresIfMatch: false,
  }),
  /**
   * `POST /api/v1/responses/stream` — `text/event-stream`. Registered so
   * `#buildUrl`/`#buildHeaders` stay the single source of truth for the
   * playground's outbound request too, but it is called through
   * `streamResponse()` below rather than through `#request<T>`: that helper
   * always does `await response.json()`, which would consume the stream body
   * before a single byte reached the browser. Declares NO `Idempotency-Key`
   * (the spec explicitly rejects one on this operation).
   */
  streamResponse: op({
    id: "stream_response",
    method: "POST",
    path: "/api/v1/responses/stream",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: false,
  }),
  /**
   * `GET /api/v1/executions/{execution_id}` — the baseline routing-transparency
   * read after a run: `attempt_count`, `latency_ms`, the route/model that
   * served, usage. No extra scope beyond the execution itself.
   */
  getExecution: op({
    id: "get_execution",
    method: "GET",
    path: "/api/v1/executions/{execution_id}",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: false,
  }),
  /**
   * `POST /api/v1/admin/runtime/diagnose` — disabled by default
   * (`runtime.diagnostic_endpoint_enabled = false`, a 404 when off) and gated
   * on `moira:runtime:diagnose` beyond that. The only committed endpoint that
   * returns per-candidate rank/score/selection-reason and raw tool-call
   * events — see `DiagnosticExecutionResponse` in `lib/types.ts`.
   */
  diagnoseRuntime: op({
    id: "diagnose_runtime",
    method: "POST",
    path: "/api/v1/admin/runtime/diagnose",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: false,
  }),

  /**
   * Containerised Claude runners (issue #275, workstream R3 of #272) —
   * `/api/v1/admin/runners*`. PR #282's frozen contract, transcribed from the
   * generated `docs/openapi.json` on that branch, not guessed from the shape
   * of the other admin families:
   *
   *   `provisionRunner`   optional `Idempotency-Key`, no `If-Match`.
   *   `listRunners`       neither.
   *   `getRunner`         neither — but it WRITES (the runner service refresh),
   *                       so its ETag advances on every call. Never cache one
   *                       from here and reuse it for `deleteRunner`.
   *   `submitRunnerAuthorizationCode`
   *                       NEITHER header, unlike `provisionRunner` next to it —
   *                       confirmed against the spec rather than assumed by
   *                       family resemblance.
   *   `finalizeRunner`    NEITHER header either, for the same reason: the spec
   *                       declares no `Idempotency-Key` parameter on this
   *                       operation, despite `src/http/runners.rs`'s own doc
   *                       comment describing one — the generated spec is ground
   *                       truth here, not the handler's prose.
   *   `deleteRunner`      `If-Match` REQUIRED, no key — the one operation on
   *                       this surface that needs a precondition, because it is
   *                       the one that destroys the runner's container.
   */
  provisionRunner: op({
    id: "provision_runner",
    method: "POST",
    path: "/api/v1/admin/runners",
    credential: "admin",
    declaresIdempotencyKey: true,
    requiresIfMatch: false,
  }),
  listRunners: op({
    id: "list_runners",
    method: "GET",
    path: "/api/v1/admin/runners",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: false,
  }),
  getRunner: op({
    id: "get_runner",
    method: "GET",
    path: "/api/v1/admin/runners/{id}",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: false,
  }),
  submitRunnerAuthorizationCode: op({
    id: "submit_runner_authorization_code",
    method: "POST",
    path: "/api/v1/admin/runners/{id}/authorization-code",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: false,
  }),
  finalizeRunner: op({
    id: "finalize_runner",
    method: "POST",
    path: "/api/v1/admin/runners/{id}/finalize",
    credential: "admin",
    declaresIdempotencyKey: false,
    requiresIfMatch: false,
  }),
  deleteRunner: op({
    id: "delete_runner",
    method: "DELETE",
    path: "/api/v1/admin/runners/{id}",
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
 * The collection segment — the fourth path segment of `/api/v1/admin/<here>` —
 * of every family that makes up LLM runtime configuration.
 *
 * Compared as a WHOLE SEGMENT rather than as a string prefix. `providers` and
 * `provider-models` are different families whose names share a prefix, and
 * `/api/v1/admin/auth/providers` is a different surface entirely that a naive
 * `startsWith` on `/api/v1/admin/provider` would swallow.
 */
const LLM_CONFIG_COLLECTIONS: ReadonlySet<string> = new Set([
  "providers",
  "provider-models",
  "provider-credentials",
  "routes",
  "routing-policies",
]);

/** The collection segment of a spec path, or `""` if it has none. */
function collectionSegmentOf(path: string): string {
  // ["", "api", "v1", "admin", "<collection>", …]
  const segments = path.split("/");
  if (segments[1] !== "api" || segments[2] !== "v1" || segments[3] !== "admin") return "";
  return segments[4] ?? "";
}

/**
 * The LLM runtime-configuration surface: the operations an LLM settings page may
 * reach, named so a test can assert properties of the SET rather than of each
 * entry — "all of them require a credential", "the routes family is read-only".
 *
 * ============================================================================
 * DERIVED FROM THE REGISTRY, NOT TRANSCRIBED FROM IT (issue #113)
 * ============================================================================
 *
 * This was a hand-maintained array of twenty-two names. It was exact when it was
 * written and nothing made it stay exact: registering a twenty-third operation
 * under one of these families left the list silently short, and every set-level
 * assertion built on it — "every LLM operation requires a credential", in this
 * file AND in `tests/contract/openapi-contract.test.ts` — then passed by not
 * looking at the new entry. A list that shrinks its own coverage without failing
 * is worse than no list.
 *
 * Deriving it removes the drift instead of detecting it: membership is now a
 * consequence of the path an operation is registered at, so a new operation on
 * one of these families joins the set the moment it exists and inherits every
 * assertion.
 *
 * WHAT REMAINS DELIBERATE, AND WHERE IT IS NOW PINNED. The absences were the
 * hand-written list's real content, and they are absences from `MOIRA_OPERATIONS`
 * itself — not from this array — so deriving preserves every one of them:
 *
 *   `POST /api/v1/admin/routes`     unregistered; pinned by the "routes family is
 *                                   READ-ONLY" assertions in the unit and
 *                                   contract suites, which count GET/GET.
 *   `DELETE` on every family        unregistered; the console disables instead.
 *   `PUT .../runtime-policy`        unregistered, and worth naming: it is the one
 *                                   mutation in Moira's whole admin surface with
 *                                   an OPTIONAL `If-Match`, which `requiresIfMatch`
 *                                   cannot express. Pinned by name in
 *                                   `openapi-contract.test.ts`.
 *
 * The type is `readonly MoiraOperationName[]` rather than a literal tuple: a
 * derived value has no literal type, and the names were never used as literals.
 */
export const LLM_CONFIG_OPERATION_NAMES: readonly MoiraOperationName[] = (
  Object.keys(MOIRA_OPERATIONS) as MoiraOperationName[]
).filter((name) => LLM_CONFIG_COLLECTIONS.has(collectionSegmentOf(MOIRA_OPERATIONS[name].path)));

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
 * Build the `oauth2` arm of `CredentialSecret` — the ONLY sanctioned way to
 * construct one, for the same reason `apiKeyCredentialSecret` is: a fresh
 * object literal, never a spread of caller input, so nothing extra rides along.
 *
 * Unlike `apiKeyCredentialSecret`, there is no untagged-union ambiguity to
 * defend against here — see `OAuth2CredentialSecret`'s header in
 * `lib/moira-credential-types.ts`. `refresh_token`/`token_type`/`expires_at`
 * are accepted but always omitted by every shipped call site
 * (`lib/claude-subscription.ts` stores a long-lived setup token, not a
 * refresh-token pair), so this returns exactly `{ access_token }` when only
 * that is supplied.
 */
export function oauth2CredentialSecret(accessToken: string): OAuth2CredentialSecret {
  if (typeof accessToken !== "string" || accessToken.length === 0) {
    throw new MoiraClientContractError("the credential secret requires a non-empty `access_token`");
  }
  return { access_token: accessToken };
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
  if (
    typeof scope !== "object" ||
    scope === null ||
    typeof (scope as { type?: unknown }).type !== "string"
  ) {
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
  } else if (credentialType === "oauth2") {
    // No ambiguity to defend against here — `access_token` is the only
    // required field on this arm and no other CredentialSecret variant shares
    // it (see `OAuth2CredentialSecret`'s header). The check is therefore just:
    // no unknown key, and a non-empty `access_token`.
    const ALLOWED = new Set(["access_token", "refresh_token", "token_type", "expires_at"]);
    const unknown = Object.keys(secret as Record<string, unknown>).filter(
      (key) => !ALLOWED.has(key),
    );
    if (unknown.length > 0) {
      throw new MoiraClientContractError(
        `an \`oauth2\` credential secret carries unknown key(s): ${unknown.join(", ")}`,
      );
    }
    const accessToken = (secret as { access_token?: unknown }).access_token;
    if (typeof accessToken !== "string" || accessToken.length === 0) {
      throw new MoiraClientContractError(
        "the credential secret requires a non-empty `access_token`",
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
  const accessToken = (secret as { access_token?: unknown }).access_token;
  if (accessToken !== undefined && (typeof accessToken !== "string" || accessToken.length === 0)) {
    throw new MoiraClientContractError(
      "rotate body requires a non-empty `access_token` when the secret carries one",
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
/* Claude runners (issue #275/#272 workstream R3)                            */
/* -------------------------------------------------------------------------- */

/**
 * `POST /api/v1/admin/runners` guard.
 *
 * `label` is re-validated here rather than trusted to Moira's own `422
 * runner_label_invalid`, for the same reason `assertLlmProviderCreateIsSafe`
 * re-checks `base_url`: the value ends up in a container name
 * (`src/domain/runners.rs`'s own doc comment on the field), and a bad label
 * caught here is a named rule instead of a relayed error a request round trip
 * away.
 */
export function assertRunnerProvisionRequestIsSafe(body: Record<string, unknown>): void {
  const label = body["label"];
  if (typeof label !== "string" || !CLAUDE_RUNNER_LABEL_PATTERN.test(label)) {
    throw new MoiraClientContractError(
      "runner provision body requires a `label` matching [a-z0-9-]{1,64} — the value becomes a " +
        "container name on the runner service",
    );
  }
}

/**
 * `POST /api/v1/admin/runners/{id}/finalize` guard.
 *
 * `scope` MUST BE ABSENT. `ClaudeRunnerFinalizeRequest` cannot even express it
 * (see that type's header), so this only catches a caller that built the body
 * by hand or reached here through an `any` — but it is the one field on this
 * surface where "Moira will refuse it anyway" is not a reason to skip the
 * console-side check: the scope is sealed into the credential's AAD at
 * provisioning time, and a value sent here silently disagreeing with the one
 * already displayed would be exactly the kind of drift this console exists to
 * prevent, even though `deny_unknown_fields` turns it into a loud 400 rather
 * than a silent acceptance.
 */
export function assertRunnerFinalizeRequestIsSafe(body: Record<string, unknown>): void {
  if ("scope" in body) {
    throw new MoiraClientContractError(
      "runner finalize body must not carry `scope`: it is fixed at provisioning time and sealed " +
        "into the credential's AAD, so a value sent here could contradict the one already stored " +
        "— Moira's own deny_unknown_fields refuses it too, but the console must not build it",
    );
  }
  if (typeof body["provider_id"] !== "string" || body["provider_id"].length === 0) {
    throw new MoiraClientContractError(
      "runner finalize body requires a non-empty `provider_id`, resolved from a provider the " +
        "operator selected — it decides which provider row the runner's token becomes a " +
        "credential for",
    );
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
   * Build `body.secret` with `apiKeyCredentialSecret()` or, for the `oauth2`
   * arm, `oauth2CredentialSecret()`. Assembling the object by hand is how
   * `endpoint: null` gets in, which makes the untagged union ambiguous and the
   * request permanently refused.
   */
  async createProviderCredential(
    body: ConsoleApiKeyCredentialCreateRequest | ConsoleOAuth2CredentialCreateRequest,
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
  /* Applications and consumer keys (issue #180)                            */
  /* ---------------------------------------------------------------------- */

  async listApplications(
    options: { readonly limit?: number; readonly cursor?: string; readonly status?: string } = {},
  ): Promise<ListResponse<ApplicationRecord>> {
    return this.#request<ListResponse<ApplicationRecord>>("listApplications", {
      query: { limit: options.limit, cursor: options.cursor, status: options.status },
    });
  }

  /**
   * `POST /api/v1/admin/applications`.
   *
   * `idempotencyKey` should be derived from the application's own identity — its
   * slug, or failing that its display name — so a double-submit replays instead
   * of landing two applications an operator then has to tell apart by their
   * creation timestamps.
   */
  async createApplication(
    body: ApplicationCreateRequest,
    options: { readonly idempotencyKey?: string } = {},
  ): Promise<ApplicationRecord> {
    return this.#request<ApplicationRecord>("createApplication", {
      body,
      idempotencyKey: options.idempotencyKey,
    });
  }

  async listConsumerKeys(
    options: {
      readonly limit?: number;
      readonly cursor?: string;
      readonly status?: string;
      readonly application_id?: string;
    } = {},
  ): Promise<ListResponse<ApiKeyRecord>> {
    return this.#request<ListResponse<ApiKeyRecord>>("listConsumerKeys", {
      query: {
        limit: options.limit,
        cursor: options.cursor,
        status: options.status,
        application_id: options.application_id,
      },
    });
  }

  /**
   * `POST /api/v1/admin/consumer-keys` — **THE ONE CALL IN THIS CLIENT THAT
   * RETURNS A PLAINTEXT CONSUMER KEY.**
   *
   * The response is `ApiKeySecretResponse` and it is returned RAW: `#request`
   * runs `toMoiraError` only on a non-ok response, so nothing sanitises a 201
   * body. Every caller is therefore handling a live credential, and the rule
   * that follows from it is the same one the invitation mint carries — do not
   * log it, do not widen it into a wrapper, and hand it to exactly one component.
   *
   * `secret` may be absent on an idempotent replay. That is a SUCCESS; see the
   * note on the type.
   */
  async createConsumerKey(
    body: ConsumerKeyCreateRequest,
    options: { readonly idempotencyKey?: string } = {},
  ): Promise<ApiKeySecretResponse> {
    return this.#request<ApiKeySecretResponse>("createConsumerKey", {
      body,
      idempotencyKey: options.idempotencyKey,
    });
  }

  /**
   * `POST /api/v1/admin/consumer-keys/{id}/revoke`.
   *
   * NO `If-Match`, unlike every provider-family disable — the operation declares
   * none, and `#buildHeaders` would send an unknown header rather than a
   * stricter request. Confirmed against the committed spec, not inferred from
   * the neighbouring families.
   */
  async revokeConsumerKey(id: string): Promise<ApiKeyRecord> {
    return this.#request<ApiKeyRecord>("revokeConsumerKey", { pathParams: { id } });
  }

  /* ---------------------------------------------------------------------- */
  /* Relationship graph (plan 12 §4, issue #234)                            */
  /* ---------------------------------------------------------------------- */

  /** `GET /api/v1/admin/graph` — the whole derived graph, assembled fresh on every call. */
  async getGraph(): Promise<GraphResponse> {
    return this.#request<GraphResponse>("getGraph", {});
  }

  /* ---------------------------------------------------------------------- */
  /* Skills (plan 12 §5)                                                    */
  /* ---------------------------------------------------------------------- */

  async listSkills(
    options: {
      readonly limit?: number;
      readonly cursor?: string | undefined;
      readonly status?: string | undefined;
      readonly search?: string | undefined;
    } = {},
  ): Promise<ListResponse<SkillRecord>> {
    return this.#request<ListResponse<SkillRecord>>("listSkills", {
      query: {
        limit: options.limit,
        cursor: options.cursor,
        status: options.status,
        search: options.search,
      },
    });
  }

  async createSkill(
    body: SkillCreateRequest,
    options: { readonly idempotencyKey?: string } = {},
  ): Promise<SkillRecord> {
    return this.#request<SkillRecord>("createSkill", {
      body,
      idempotencyKey: options.idempotencyKey,
    });
  }

  async getSkill(id: string): Promise<SkillRecord> {
    return this.#request<SkillRecord>("getSkill", { pathParams: { id } });
  }

  async patchSkill(id: string, body: SkillPatchRequest, ifMatch: string): Promise<SkillRecord> {
    return this.#request<SkillRecord>("patchSkill", { pathParams: { id }, body, ifMatch });
  }

  async deleteSkill(id: string, ifMatch: string): Promise<void> {
    await this.#request<void>("deleteSkill", { pathParams: { id }, ifMatch });
  }

  async enableSkill(id: string, ifMatch: string): Promise<SkillRecord> {
    return this.#request<SkillRecord>("enableSkill", { pathParams: { id }, ifMatch });
  }

  async disableSkill(id: string, ifMatch: string): Promise<SkillRecord> {
    return this.#request<SkillRecord>("disableSkill", { pathParams: { id }, ifMatch });
  }

  /** `POST /api/v1/admin/skills/bulk-enable`. No `If-Match` — see the registry note. */
  async bulkEnableSkills(skillIds: readonly string[]): Promise<SkillBulkEnableResponse> {
    const body: SkillBulkEnableRequest = { skill_ids: [...skillIds] };
    return this.#request<SkillBulkEnableResponse>("bulkEnableSkills", { body });
  }

  /**
   * `POST /api/v1/admin/skills/import` — the OpenAPI import pipeline (plan 12
   * §5). `idempotencyKey` should be derived from the document's own identity so
   * a double-submit replays rather than importing the same spec twice.
   */
  async importSkills(
    document: unknown,
    options: { readonly idempotencyKey?: string } = {},
  ): Promise<SkillImportResponse> {
    const body: SkillImportRequest = { document: document as SkillImportRequest["document"] };
    return this.#request<SkillImportResponse>("importSkills", {
      body,
      idempotencyKey: options.idempotencyKey,
    });
  }

  async listSkillExecutors(
    options: { readonly limit?: number; readonly cursor?: string | undefined } = {},
  ): Promise<ListResponse<SkillHttpExecutorRecord>> {
    return this.#request<ListResponse<SkillHttpExecutorRecord>>("listSkillExecutors", {
      query: { limit: options.limit, cursor: options.cursor },
    });
  }

  async getSkillExecutor(skillId: string): Promise<SkillHttpExecutorRecord> {
    return this.#request<SkillHttpExecutorRecord>("getSkillExecutor", {
      pathParams: { id: skillId },
    });
  }

  /** `ifMatch` is the QUOTED `updated_at` — build it with `skillExecutorIfMatchFor`. */
  async patchSkillExecutor(
    skillId: string,
    body: SkillHttpExecutorPatchRequest,
    ifMatch: string,
  ): Promise<SkillHttpExecutorRecord> {
    return this.#request<SkillHttpExecutorRecord>("patchSkillExecutor", {
      pathParams: { id: skillId },
      body,
      ifMatch,
    });
  }

  async deleteSkillExecutor(skillId: string, ifMatch: string): Promise<void> {
    await this.#request<void>("deleteSkillExecutor", { pathParams: { id: skillId }, ifMatch });
  }

  /* ---------------------------------------------------------------------- */
  /* Provider health (issue #83)                                            */
  /* ---------------------------------------------------------------------- */

  /** `GET /api/v1/admin/providers/health` — the rolling reachability window for every enabled provider. */
  async getProviderHealth(): Promise<ProviderHealthResponse> {
    return this.#request<ProviderHealthResponse>("getProviderHealth", {});
  }

  /* ---------------------------------------------------------------------- */
  /* Eval suites (plan 12 §3)                                               */
  /* ---------------------------------------------------------------------- */

  async listEvalSuites(
    options: {
      readonly limit?: number;
      readonly cursor?: string | undefined;
      readonly status?: string | undefined;
      readonly search?: string | undefined;
    } = {},
  ): Promise<ListResponse<EvalSuiteRecord>> {
    return this.#request<ListResponse<EvalSuiteRecord>>("listEvalSuites", {
      query: {
        limit: options.limit,
        cursor: options.cursor,
        status: options.status,
        search: options.search,
      },
    });
  }

  async createEvalSuite(
    body: EvalSuiteCreateRequest,
    options: { readonly idempotencyKey?: string } = {},
  ): Promise<EvalSuiteRecord> {
    return this.#request<EvalSuiteRecord>("createEvalSuite", {
      body,
      idempotencyKey: options.idempotencyKey,
    });
  }

  async getEvalSuite(id: string): Promise<EvalSuiteRecord> {
    return this.#request<EvalSuiteRecord>("getEvalSuite", { pathParams: { id } });
  }

  async patchEvalSuite(
    id: string,
    body: EvalSuitePatchRequest,
    ifMatch: string,
  ): Promise<EvalSuiteRecord> {
    return this.#request<EvalSuiteRecord>("patchEvalSuite", { pathParams: { id }, body, ifMatch });
  }

  /** Soft-deletes the suite. There is no restore operation on this surface. */
  async deleteEvalSuite(id: string, ifMatch: string): Promise<void> {
    await this.#request<void>("deleteEvalSuite", { pathParams: { id }, ifMatch });
  }

  async listEvalCases(
    suiteId: string,
    options: { readonly limit?: number; readonly cursor?: string | undefined } = {},
  ): Promise<ListResponse<EvalCaseRecord>> {
    return this.#request<ListResponse<EvalCaseRecord>>("listEvalCases", {
      pathParams: { id: suiteId },
      query: { limit: options.limit, cursor: options.cursor },
    });
  }

  async createEvalCase(suiteId: string, body: EvalCaseCreateRequest): Promise<EvalCaseRecord> {
    return this.#request<EvalCaseRecord>("createEvalCase", { pathParams: { id: suiteId }, body });
  }

  /** No `If-Match` — `EvalCaseRecord` carries no `version`. */
  async deleteEvalCase(suiteId: string, caseId: string): Promise<void> {
    await this.#request<void>("deleteEvalCase", { pathParams: { id: suiteId, case_id: caseId } });
  }

  async listEvalRuns(
    suiteId: string,
    options: { readonly limit?: number; readonly cursor?: string | undefined } = {},
  ): Promise<ListResponse<EvalRunRecord>> {
    return this.#request<ListResponse<EvalRunRecord>>("listEvalRuns", {
      pathParams: { id: suiteId },
      query: { limit: options.limit, cursor: options.cursor },
    });
  }

  /* ---------------------------------------------------------------------- */
  /* Flows (plan 12 §6)                                                     */
  /* ---------------------------------------------------------------------- */

  async listFlows(
    options: {
      readonly limit?: number;
      readonly cursor?: string | undefined;
      readonly status?: string | undefined;
      readonly search?: string | undefined;
    } = {},
  ): Promise<ListResponse<AgentFlowRecord>> {
    return this.#request<ListResponse<AgentFlowRecord>>("listFlows", {
      query: {
        limit: options.limit,
        cursor: options.cursor,
        status: options.status,
        search: options.search,
      },
    });
  }

  async createFlow(
    body: AgentFlowCreateRequest,
    options: { readonly idempotencyKey?: string } = {},
  ): Promise<AgentFlowRecord> {
    return this.#request<AgentFlowRecord>("createFlow", {
      body,
      idempotencyKey: options.idempotencyKey,
    });
  }

  async getFlow(id: string): Promise<AgentFlowRecord> {
    return this.#request<AgentFlowRecord>("getFlow", { pathParams: { id } });
  }

  async patchFlow(
    id: string,
    body: AgentFlowPatchRequest,
    ifMatch: string,
  ): Promise<AgentFlowRecord> {
    return this.#request<AgentFlowRecord>("patchFlow", { pathParams: { id }, body, ifMatch });
  }

  /** Soft-deletes the flow. There is no restore operation on this surface. */
  async deleteFlow(id: string, ifMatch: string): Promise<void> {
    await this.#request<void>("deleteFlow", { pathParams: { id }, ifMatch });
  }

  async listFlowRuns(
    id: string,
    options: { readonly limit?: number; readonly cursor?: string | undefined } = {},
  ): Promise<ListResponse<AgentFlowRunRecord>> {
    return this.#request<ListResponse<AgentFlowRunRecord>>("listFlowRuns", {
      pathParams: { id },
      query: { limit: options.limit, cursor: options.cursor },
    });
  }

  /** `GET /api/v1/admin/agent-profiles` — read-only, for the flow step builder's picker. */
  async listAgentProfiles(
    options: { readonly limit?: number; readonly cursor?: string; readonly status?: string } = {},
  ): Promise<ListResponse<AgentProfileRecord>> {
    return this.#request<ListResponse<AgentProfileRecord>>("listAgentProfiles", {
      query: { limit: options.limit, cursor: options.cursor, status: options.status },
    });
  }

  /* ---------------------------------------------------------------------- */
  /* The playground (issue #261) — the real execution path                  */
  /* ---------------------------------------------------------------------- */

  /** `POST /api/v1/responses` — the non-streaming fallback toggle. */
  async createResponse(body: PublicResponseRequest): Promise<PublicResponse> {
    return this.#request<PublicResponse>("createResponse", { body });
  }

  /**
   * `POST /api/v1/responses/stream` — returns the RAW upstream `Response`
   * rather than a parsed value. `#request<T>` always calls `response.json()`,
   * which would read the stream to completion before a single SSE frame
   * reached the browser; this method builds the url/headers through the same
   * private helpers every other operation uses and calls `#fetch` directly
   * instead.
   *
   * The caller owns the returned `Response`: a non-`ok` one carries a JSON
   * error body exactly like every other operation (`toMoiraError` still
   * applies — see `app/api/playground/stream/route.ts`), and an `ok` one has
   * `.body` as the live `text/event-stream` to pipe straight through to the
   * browser.
   *
   * `options.signal` is forwarded to the outbound fetch so the BFF route
   * handler can cancel the upstream Moira execution the instant the browser
   * aborts its own request to the console — see the stop button in
   * `modules/playground/PlaygroundScreen.tsx`.
   */
  async streamResponse(
    body: PublicResponseRequest,
    options: { readonly signal?: AbortSignal } = {},
  ): Promise<Response> {
    const operation = MOIRA_OPERATIONS.streamResponse;
    const url = this.#buildUrl(operation, {});
    const headers = await this.#buildHeaders(operation, { body });
    headers["Accept"] = "text/event-stream";
    try {
      return await this.#fetch(url, {
        method: operation.method,
        headers,
        body: JSON.stringify(body),
        ...(options.signal === undefined ? {} : { signal: options.signal }),
      });
    } catch (cause) {
      throw new MoiraRequestError(toTransportError(cause));
    }
  }

  /**
   * `GET /api/v1/executions/{execution_id}` — the baseline routing-transparency
   * follow-up after a run, reachable with no scope beyond the execution
   * itself. `executionId` is `PublicResponse.execution_id`/`PublicSseEnvelope.execution_id`
   * verbatim, including its `exec_` prefix.
   */
  async getExecution(executionId: string): Promise<PublicExecutionSummary> {
    return this.#request<PublicExecutionSummary>("getExecution", {
      pathParams: { execution_id: executionId },
    });
  }

  /**
   * `POST /api/v1/admin/runtime/diagnose` — a 404
   * (`runtime.diagnostic_endpoint_enabled` off on this deployment) or a 403
   * (missing `moira:runtime:diagnose`, or, when `body.options.priority` /
   * `.complexity_hint` is set, missing `moira:execution:override-priority` /
   * `-complexity-hint`) both surface through the usual `MoiraRequestError`
   * path, so `app/api/playground/diagnose/route.ts` renders either the same
   * way as any other Moira refusal — the keyed envelope, not a special case.
   */
  async diagnoseRuntime(body: DiagnosticExecutionRequest): Promise<DiagnosticExecutionResponse> {
    return this.#request<DiagnosticExecutionResponse>("diagnoseRuntime", { body });
  }

  /* ---------------------------------------------------------------------- */
  /* Claude runners (issue #275/#272 workstream R3)                        */
  /* ---------------------------------------------------------------------- */
  //
  // The token never appears on this surface. Every method below returns
  // `ClaudeRunnerRecord` (no token-shaped field — see its header in
  // `lib/types.ts`), `ListResponse<ClaudeRunnerRecord>`, or nothing.

  /**
   * `POST /api/v1/admin/runners`.
   *
   * `idempotencyKey` should be derived from `label`: Moira itself refuses a
   * duplicate label with `409 duplicate_runner_label`, and a deterministic key
   * makes a double-submit of the SAME provisioning attempt replay instead of
   * racing that refusal.
   */
  async provisionRunner(
    body: ClaudeRunnerProvisionRequest,
    options: { readonly idempotencyKey?: string } = {},
  ): Promise<ClaudeRunnerRecord> {
    assertRunnerProvisionRequestIsSafe(body as unknown as Record<string, unknown>);
    return this.#request<ClaudeRunnerRecord>("provisionRunner", {
      body,
      idempotencyKey: options.idempotencyKey,
    });
  }

  async listRunners(
    options: { readonly limit?: number; readonly cursor?: string } = {},
  ): Promise<ListResponse<ClaudeRunnerRecord>> {
    return this.#request<ListResponse<ClaudeRunnerRecord>>("listRunners", {
      query: { limit: options.limit, cursor: options.cursor },
    });
  }

  /**
   * `GET /api/v1/admin/runners/{id}`.
   *
   * THIS CALL WRITES. A runner that is not yet in a terminal state is
   * refreshed from the runner service first — that is what surfaces
   * `authorization_url` — so the returned `version` (and therefore any `ETag`
   * built from it) ADVANCES on every poll. Never hold onto a version read from
   * here and hand it to `deleteRunner` later; re-read immediately before
   * deleting instead. See `lib/runners.ts`'s `deleteRunnerSafely`.
   */
  async getRunner(id: string): Promise<ClaudeRunnerRecord> {
    return this.#request<ClaudeRunnerRecord>("getRunner", { pathParams: { id } });
  }

  /**
   * `POST /api/v1/admin/runners/{id}/authorization-code`.
   *
   * The code is a single-use OAuth authorization code, not a token. It is
   * forwarded to the runner service and dropped — never stored, logged, or
   * echoed back by anything in this client.
   */
  async submitRunnerAuthorizationCode(
    id: string,
    body: ClaudeRunnerAuthorizationCodeRequest,
  ): Promise<ClaudeRunnerRecord> {
    return this.#request<ClaudeRunnerRecord>("submitRunnerAuthorizationCode", {
      pathParams: { id },
      body,
    });
  }

  /**
   * `POST /api/v1/admin/runners/{id}/finalize`.
   *
   * `409 runner_token_unavailable` means this runner is a write-off: the token
   * read is one-shot, so a row still at `ready` after this fails can never
   * yield it again. The caller must render that plainly and offer
   * delete-and-reprovision — not a retry button that cannot work. See
   * `modules/runners/RunnerDetail.tsx`.
   */
  async finalizeRunner(id: string, body: ClaudeRunnerFinalizeRequest): Promise<ClaudeRunnerRecord> {
    assertRunnerFinalizeRequestIsSafe(body as unknown as Record<string, unknown>);
    return this.#request<ClaudeRunnerRecord>("finalizeRunner", { pathParams: { id }, body });
  }

  /**
   * `DELETE /api/v1/admin/runners/{id}`. `If-Match` REQUIRED.
   *
   * Removes the runner's container upstream AND soft-deletes Moira's mirror
   * row; any credential the runner already produced is deliberately left in
   * place. Use `ifMatchFor(record)` from a version read IMMEDIATELY before
   * calling this — see `getRunner`'s header for why a poll's version is not
   * safe to reuse here.
   */
  async deleteRunner(id: string, ifMatch: string): Promise<void> {
    await this.#request<void>("deleteRunner", { pathParams: { id }, ifMatch });
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

/**
 * `If-Match` value for a `skill_http_executors` row.
 *
 * That family has no `version` column — see `SkillHttpExecutorRecord`'s doc
 * comment — so its precondition is the QUOTED `updated_at` timestamp instead,
 * matching the shape `executor_etag_headers` writes on the response `ETag`
 * (`src/http/agent_platform.rs`). Building this by hand elsewhere (or reusing
 * `ifMatchFor`, which has no `updated_at` to read) would send an unquoted or
 * stale value the server rejects.
 */
export function skillExecutorIfMatchFor(record: { readonly updated_at: string }): string {
  return `"${record.updated_at}"`;
}
