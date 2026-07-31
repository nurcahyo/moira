// TypeScript mirrors of the Moira admin DTOs this console binds to.
//
// GROUND TRUTH is `docs/openapi.json` — the committed spec — and nothing else.
// Neither plan 08's body nor its §0 audit is authoritative here; where they
// disagree with the spec, the spec wins. `tests/contract/openapi-contract.test.ts`
// re-derives every `*_CONTRACT` descriptor below from that file on every run, so
// a Moira-side schema change surfaces as a console test failure rather than as a
// 400 in production.
//
// The descriptors and the interfaces are welded together at *compile* time by the
// `assertKeyContract` calls at the bottom of each block: the descriptor cannot
// drift from the interface without `bun run typecheck` failing, and the
// interface cannot drift from the spec without the contract test failing. Both
// halves are required — either alone is silently defeatable.

/* -------------------------------------------------------------------------- */
/* Compile-time contract machinery                                            */
/* -------------------------------------------------------------------------- */

/** Keys of `T` that are NOT optional (`?`). */
type RequiredKeys<T> = {
  [K in keyof T]-?: Record<string, never> extends Pick<T, K> ? never : K;
}[keyof T];

/** Keys of `T` that ARE optional (`?`). */
type OptionalKeys<T> = {
  [K in keyof T]-?: Record<string, never> extends Pick<T, K> ? K : never;
}[keyof T];

type Extends<A, B> = [A] extends [B] ? true : false;

/** `true` only when `A` and `B` are the same union of string literals. */
type SameSet<A, B> =
  Extends<A, B> extends true ? (Extends<B, A> extends true ? true : never) : never;

/**
 * `true` only when `TShape`'s required keys are exactly `TRequired` and its
 * optional keys are exactly `TOptional`.
 */
type ExactKeys<TShape, TRequired extends string, TOptional extends string> =
  SameSet<RequiredKeys<TShape>, TRequired> extends true
    ? SameSet<OptionalKeys<TShape>, TOptional> extends true
      ? true
      : never
    : never;

/**
 * Compile-time assertion. Instantiating it with anything but `true` — which
 * `ExactKeys` produces as `never` on any mismatch — is a type error.
 * The runtime body is deliberately empty.
 */
const assertKeyContract = <T extends true>(_ok?: T): void => {};

/** The shape every `*_CONTRACT` descriptor takes. */
export interface SchemaContract {
  /** `#/components/schemas/<schema>` in `docs/openapi.json`. */
  readonly schema: string;
  readonly required: readonly string[];
  readonly optional: readonly string[];
}

/* -------------------------------------------------------------------------- */
/* Primitives                                                                 */
/* -------------------------------------------------------------------------- */

export type JsonValue =
  null | boolean | number | string | readonly JsonValue[] | { readonly [key: string]: JsonValue };

/** `#/components/schemas/AuthMethod` */
export type AuthMethod = "google_oauth" | "generic_oidc" | "jwks";

/** `#/components/schemas/ResourceStatus` */
export type ResourceStatus = "active" | "disabled" | "deleted";

/** `#/components/schemas/AdminIdentityStatus` */
export type AdminIdentityStatus = "active" | "revoked";

/**
 * `#/components/schemas/ResponseText` — Moira's i18n envelope.
 *
 * Every server-originated string reaches the operator through one of these,
 * rendered `t(message_key, message_args, message)`: catalog first, server
 * `message` as the fallback. Components never receive pre-formatted prose.
 */
export interface ResponseText {
  message_key: string;
  message: string;
  message_args?: JsonValue;
}

export const RESPONSE_TEXT_CONTRACT = {
  schema: "ResponseText",
  required: ["message_key", "message"],
  optional: ["message_args"],
} as const satisfies SchemaContract;

assertKeyContract<
  ExactKeys<
    ResponseText,
    (typeof RESPONSE_TEXT_CONTRACT)["required"][number],
    (typeof RESPONSE_TEXT_CONTRACT)["optional"][number]
  >
>();

/** `#/components/schemas/Pagination` */
export interface Pagination {
  has_more: boolean;
  next_cursor?: string | null;
}

export const PAGINATION_CONTRACT = {
  schema: "Pagination",
  required: ["has_more"],
  optional: ["next_cursor"],
} as const satisfies SchemaContract;

assertKeyContract<
  ExactKeys<
    Pagination,
    (typeof PAGINATION_CONTRACT)["required"][number],
    (typeof PAGINATION_CONTRACT)["optional"][number]
  >
>();

/** `#/components/schemas/ListResponse_*` — every admin list shares this envelope. */
export interface ListResponse<T> {
  data: T[];
  pagination: Pagination;
}

/* -------------------------------------------------------------------------- */
/* Setup / identity surface                                                   */
/* -------------------------------------------------------------------------- */

/** `#/components/schemas/SetupClaimStatusResponse` — one bit, deliberately. */
export interface SetupClaimStatusResponse {
  claimed: boolean;
}

export const SETUP_CLAIM_STATUS_RESPONSE_CONTRACT = {
  schema: "SetupClaimStatusResponse",
  required: ["claimed"],
  optional: [],
} as const satisfies SchemaContract;

assertKeyContract<
  ExactKeys<
    SetupClaimStatusResponse,
    (typeof SETUP_CLAIM_STATUS_RESPONSE_CONTRACT)["required"][number],
    (typeof SETUP_CLAIM_STATUS_RESPONSE_CONTRACT)["optional"][number]
  >
>();

/** `#/components/schemas/PublicAuthMethod` — the narrowed bootstrap projection. */
export interface PublicAuthMethod {
  id: string;
  method: AuthMethod;
  display_name: string;
  requested_scopes: string[];
  allowed_email_domains: string[];
  authorization_url?: string | null;
  client_id?: string | null;
  discovery_url?: string | null;
  issuer?: string | null;
  jwks_url?: string | null;
}

export const PUBLIC_AUTH_METHOD_CONTRACT = {
  schema: "PublicAuthMethod",
  required: ["id", "method", "display_name", "requested_scopes", "allowed_email_domains"],
  optional: ["authorization_url", "client_id", "discovery_url", "issuer", "jwks_url"],
} as const satisfies SchemaContract;

assertKeyContract<
  ExactKeys<
    PublicAuthMethod,
    (typeof PUBLIC_AUTH_METHOD_CONTRACT)["required"][number],
    (typeof PUBLIC_AUTH_METHOD_CONTRACT)["optional"][number]
  >
>();

/** `#/components/schemas/SetupAuthMethodsResponse` */
export interface SetupAuthMethodsResponse {
  methods: PublicAuthMethod[];
}

export const SETUP_AUTH_METHODS_RESPONSE_CONTRACT = {
  schema: "SetupAuthMethodsResponse",
  required: ["methods"],
  optional: [],
} as const satisfies SchemaContract;

assertKeyContract<
  ExactKeys<
    SetupAuthMethodsResponse,
    (typeof SETUP_AUTH_METHODS_RESPONSE_CONTRACT)["required"][number],
    (typeof SETUP_AUTH_METHODS_RESPONSE_CONTRACT)["optional"][number]
  >
>();

/**
 * `#/components/schemas/ClaimAdminIdentityRequest` — body of
 * `POST /api/v1/admin/setup/claim`.
 *
 * `additionalProperties: false` in the spec (`deny_unknown_fields`): sending a
 * field not listed here is a hard 400, never a silent drop.
 */
export interface ClaimAdminIdentityRequest {
  /**
   * Must resolve to an **active, registered** `trusted_jwt_issuers` row. For
   * this console that is the *console's own* issuer, not the IdP's — see
   * `setup-flow.ts` and the B1 note there.
   */
  issuer: string;
  /** The IdP's stable subject. With `issuer`, the grant's uniqueness key. */
  subject: string;
  /** REQUIRED `String` in Moira. Not optional, not nullable, no default. */
  email: string;
  /**
   * REQUIRED `bool` with **no** `#[serde(default)]`. Omitting it is a schema
   * violation, not a `false`. Must be `true` or the claim is refused
   * `403 admin_claim_email_not_verified`.
   */
  email_verified: boolean;
  /**
   * OMIT THIS FIELD. `#[serde(default = "default_admin_grant_scopes")]` means an
   * *omitted* `scopes` yields `["moira:admin"]`, but an explicitly sent `[]`
   * normalises to an empty vector and creates a grant with **zero scopes** — a
   * silent, permanent, un-revocable-by-retry no-op admin. A non-empty bad scope
   * is `422 scope_invalid` (not 400). The client never populates this; see
   * `assertClaimRequestIsSafe`.
   */
  scopes?: string[];
  /**
   * RESERVED AND REJECTED — never silently ignored.
   *
   * The one-time setup-token credential path is deferred (plan 07 §0.2 D1).
   * `POST /api/v1/admin/setup/claim` declares `security: [{ systemKeyAuth: [] }]`
   * and nothing else, and a populated `setup_token` is refused **twice** — at the
   * handler and again in the service — with `400 setup_token_not_supported`.
   *
   * The field survives in this interface only so the shape matches the committed
   * schema. The client never populates it.
   */
  setup_token?: string | null;
}

export const CLAIM_ADMIN_IDENTITY_REQUEST_CONTRACT = {
  schema: "ClaimAdminIdentityRequest",
  required: ["issuer", "subject", "email", "email_verified"],
  optional: ["scopes", "setup_token"],
} as const satisfies SchemaContract;

assertKeyContract<
  ExactKeys<
    ClaimAdminIdentityRequest,
    (typeof CLAIM_ADMIN_IDENTITY_REQUEST_CONTRACT)["required"][number],
    (typeof CLAIM_ADMIN_IDENTITY_REQUEST_CONTRACT)["optional"][number]
  >
>();

/**
 * The only claim body this console is allowed to construct: the two
 * never-send fields are removed at the type level, so a call site cannot
 * populate them even by accident.
 */
export type ConsoleClaimAdminIdentityRequest = Omit<
  ClaimAdminIdentityRequest,
  "scopes" | "setup_token"
>;

/**
 * `#/components/schemas/AdminIdentityRecord` — the claim response.
 *
 * `notice` and `version` are **required** and are not optional here. `notice`
 * is a `ResponseText` and must be rendered through the i18n helper, never as a
 * hardcoded English success string.
 */
export interface AdminIdentityRecord {
  id: string;
  issuer: string;
  subject: string;
  email: string;
  email_verified: boolean;
  granted_scopes: string[];
  status: AdminIdentityStatus;
  created_at: string;
  version: number;
  notice: ResponseText;
  /**
   * Ownership, as **row state rather than a scope** (plan 09 wave 2).
   *
   * `moira:admins:manage` was specified as a scope `moira:admin` must not imply, and that
   * is unimplementable: `AuthorizationService::has_scope` has no per-scope opt-out, and every
   * admin identity is granted `moira:admin`, so such a scope would have been satisfied by
   * everyone and the ownership model built on it would have been decorative.
   *
   * **Do not render this as a capability the signed-in user has.** It is a property of the
   * row, and the transfer endpoint checks it server-side; a console that treats it as a
   * permission will disagree with Moira the moment a non-primary admin looks at the screen.
   */
  is_primary: boolean;
}

export const ADMIN_IDENTITY_RECORD_CONTRACT = {
  schema: "AdminIdentityRecord",
  required: [
    "id",
    "issuer",
    "subject",
    "email",
    "email_verified",
    "granted_scopes",
    "status",
    "created_at",
    "version",
    "notice",
    "is_primary",
  ],
  optional: [],
} as const satisfies SchemaContract;

assertKeyContract<
  ExactKeys<
    AdminIdentityRecord,
    (typeof ADMIN_IDENTITY_RECORD_CONTRACT)["required"][number],
    (typeof ADMIN_IDENTITY_RECORD_CONTRACT)["optional"][number]
  >
>();

/* -------------------------------------------------------------------------- */
/* Auth provider settings surface (SEVEN operations, not ten)                 */
/* -------------------------------------------------------------------------- */

/**
 * `#/components/schemas/AuthProviderSettingsCreateRequest`.
 *
 * `display_name` is **required** — omitting it is a 400. `additionalProperties`
 * is `false`, so unknown fields are rejected rather than dropped.
 *
 * `enabled` is a plain **writable** boolean. "The row is created disabled" is a
 * console convention enforced by `ConsoleAuthProviderCreateRequest` below, not a
 * Moira guarantee: a create body sending `enabled: true` lands an enabled
 * provider immediately.
 */
export interface AuthProviderSettingsCreateRequest {
  method: AuthMethod;
  display_name: string;
  allowed_algorithms?: string[];
  allowed_email_domains?: string[];
  authorization_url?: string | null;
  client_id?: string | null;
  discovery_url?: string | null;
  enabled?: boolean;
  expected_audiences?: string[];
  issuer?: string | null;
  jwks_url?: string | null;
  metadata?: JsonValue;
  redirect_uris?: string[];
  requested_scopes?: string[];
  token_url?: string | null;
  /**
   * THE B1 FIELD. Binds this provider row to a registered trusted JWT issuer.
   * `governing_policy` selects the policy with
   * `where ... and (issuer = $1 or trusted_jwt_issuer_id = $2)` where `$1` is the
   * *claim body's* issuer. The console's claim names the *console's* issuer while
   * this row's `issuer` names the *IdP* — so without this field neither branch
   * matches, `policy = None`, and every claim is `403
   * admin_claim_domain_not_allowed`, forever.
   */
  trusted_jwt_issuer_id?: string | null;
  userinfo_url?: string | null;
}

export const AUTH_PROVIDER_SETTINGS_CREATE_REQUEST_CONTRACT = {
  schema: "AuthProviderSettingsCreateRequest",
  required: ["method", "display_name"],
  optional: [
    "allowed_algorithms",
    "allowed_email_domains",
    "authorization_url",
    "client_id",
    "discovery_url",
    "enabled",
    "expected_audiences",
    "issuer",
    "jwks_url",
    "metadata",
    "redirect_uris",
    "requested_scopes",
    "token_url",
    "trusted_jwt_issuer_id",
    "userinfo_url",
  ],
} as const satisfies SchemaContract;

assertKeyContract<
  ExactKeys<
    AuthProviderSettingsCreateRequest,
    (typeof AUTH_PROVIDER_SETTINGS_CREATE_REQUEST_CONTRACT)["required"][number],
    (typeof AUTH_PROVIDER_SETTINGS_CREATE_REQUEST_CONTRACT)["optional"][number]
  >
>();

/**
 * The only provider-create body this console may construct.
 *
 * `enabled` is removed at the type level and `trusted_jwt_issuer_id` is promoted
 * to **required and non-nullable**. Between them, the two edits make the two
 * defects that matter unrepresentable: an enabled-on-create provider, and a
 * provider row that can never govern the console's issuer.
 */
export type ConsoleAuthProviderCreateRequest = Omit<
  AuthProviderSettingsCreateRequest,
  "enabled" | "trusted_jwt_issuer_id"
> & {
  trusted_jwt_issuer_id: string;
};

/** `#/components/schemas/AuthProviderSettingsRecord` */
export interface AuthProviderSettingsRecord {
  id: string;
  method: AuthMethod;
  display_name: string;
  enabled: boolean;
  requested_scopes: string[];
  allowed_email_domains: string[];
  allowed_algorithms: string[];
  expected_audiences: string[];
  redirect_uris: string[];
  metadata: JsonValue;
  status: ResourceStatus;
  created_at: string;
  updated_at: string;
  version: number;
  authorization_url?: string | null;
  client_id?: string | null;
  discovery_url?: string | null;
  issuer?: string | null;
  jwks_url?: string | null;
  token_url?: string | null;
  trusted_jwt_issuer_id?: string | null;
  userinfo_url?: string | null;
}

export const AUTH_PROVIDER_SETTINGS_RECORD_CONTRACT = {
  schema: "AuthProviderSettingsRecord",
  required: [
    "id",
    "method",
    "display_name",
    "enabled",
    "requested_scopes",
    "allowed_email_domains",
    "allowed_algorithms",
    "expected_audiences",
    "redirect_uris",
    "metadata",
    "status",
    "created_at",
    "updated_at",
    "version",
  ],
  optional: [
    "authorization_url",
    "client_id",
    "discovery_url",
    "issuer",
    "jwks_url",
    "token_url",
    "trusted_jwt_issuer_id",
    "userinfo_url",
  ],
} as const satisfies SchemaContract;

assertKeyContract<
  ExactKeys<
    AuthProviderSettingsRecord,
    (typeof AUTH_PROVIDER_SETTINGS_RECORD_CONTRACT)["required"][number],
    (typeof AUTH_PROVIDER_SETTINGS_RECORD_CONTRACT)["optional"][number]
  >
>();

/* -------------------------------------------------------------------------- */
/* Trusted JWT issuers                                                        */
/* -------------------------------------------------------------------------- */

/** `#/components/schemas/JwtClaimMapping` */
export interface JwtClaimMapping {
  application_id?: string | null;
  delegated_tenant?: string | null;
  delegated_user?: string | null;
  roles?: string | null;
  scopes?: string | null;
  subject?: string | null;
  tenant_id?: string | null;
  user_id?: string | null;
}

/**
 * `#/components/schemas/TrustedJwtIssuerCreateRequest`.
 *
 * `scopes_claim` must stay unset for a console-linked issuer: a provider row
 * bound to an issuer that *does* assert scopes is refused
 * `400 console_issuer_must_not_assert_scopes`, because such tokens could
 * self-assert authority and `admin_identities` would stop being the sole source
 * of human authorization.
 */
export interface TrustedJwtIssuerCreateRequest {
  issuer: string;
  jwks_url: string;
  allow_delegation?: boolean;
  allowed_algorithms?: string[];
  application_id_claim?: string | null;
  claim_mapping?: JwtClaimMapping | null;
  clock_skew_seconds?: number;
  delegated_tenant_claim?: string | null;
  delegated_user_claim?: string | null;
  expected_audiences?: string[];
  roles_claim?: string | null;
  scopes_claim?: string | null;
  subject_claim?: string;
  tenant_id_claim?: string | null;
  user_id_claim?: string | null;
}

export const TRUSTED_JWT_ISSUER_CREATE_REQUEST_CONTRACT = {
  schema: "TrustedJwtIssuerCreateRequest",
  required: ["issuer", "jwks_url"],
  optional: [
    "allow_delegation",
    "allowed_algorithms",
    "application_id_claim",
    "claim_mapping",
    "clock_skew_seconds",
    "delegated_tenant_claim",
    "delegated_user_claim",
    "expected_audiences",
    "roles_claim",
    "scopes_claim",
    "subject_claim",
    "tenant_id_claim",
    "user_id_claim",
  ],
} as const satisfies SchemaContract;

assertKeyContract<
  ExactKeys<
    TrustedJwtIssuerCreateRequest,
    (typeof TRUSTED_JWT_ISSUER_CREATE_REQUEST_CONTRACT)["required"][number],
    (typeof TRUSTED_JWT_ISSUER_CREATE_REQUEST_CONTRACT)["optional"][number]
  >
>();

/**
 * The only trusted-JWT-issuer create body this console may construct:
 * `scopes_claim` and `claim_mapping` are removed at the type level so the
 * console can never register an issuer whose tokens self-assert scopes.
 */
export type ConsoleTrustedJwtIssuerCreateRequest = Omit<
  TrustedJwtIssuerCreateRequest,
  "scopes_claim" | "claim_mapping"
>;

/** `#/components/schemas/TrustedJwtIssuerRecord` */
export interface TrustedJwtIssuerRecord {
  id: string;
  issuer: string;
  jwks_url: string;
  expected_audiences: string[];
  allowed_algorithms: string[];
  subject_claim: string;
  clock_skew_seconds: number;
  allow_delegation: boolean;
  status: ResourceStatus;
  created_at: string;
  updated_at: string;
  version: number;
  application_id_claim?: string | null;
  delegated_tenant_claim?: string | null;
  delegated_user_claim?: string | null;
  deleted_at?: string | null;
  roles_claim?: string | null;
  scopes_claim?: string | null;
  tenant_id_claim?: string | null;
  user_id_claim?: string | null;
}

export const TRUSTED_JWT_ISSUER_RECORD_CONTRACT = {
  schema: "TrustedJwtIssuerRecord",
  required: [
    "id",
    "issuer",
    "jwks_url",
    "expected_audiences",
    "allowed_algorithms",
    "subject_claim",
    "clock_skew_seconds",
    "allow_delegation",
    "status",
    "created_at",
    "updated_at",
    "version",
  ],
  optional: [
    "application_id_claim",
    "delegated_tenant_claim",
    "delegated_user_claim",
    "deleted_at",
    "roles_claim",
    "scopes_claim",
    "tenant_id_claim",
    "user_id_claim",
  ],
} as const satisfies SchemaContract;

assertKeyContract<
  ExactKeys<
    TrustedJwtIssuerRecord,
    (typeof TRUSTED_JWT_ISSUER_RECORD_CONTRACT)["required"][number],
    (typeof TRUSTED_JWT_ISSUER_RECORD_CONTRACT)["optional"][number]
  >
>();

/* -------------------------------------------------------------------------- */
/* Error envelope (server-side shape — never crosses to the browser)          */
/* -------------------------------------------------------------------------- */

/**
 * `#/components/schemas/ErrorDetail`. **Server-side only.** `details` and
 * `request_id` must not cross the server/client boundary; `lib/errors.ts` is the
 * only module allowed to read this shape, and it produces a narrower
 * client-safe union.
 */
export interface MoiraErrorDetail {
  code: string;
  message_key: string;
  message: string;
  message_args: JsonValue;
  request_id: string;
  details?: JsonValue;
}

export const ERROR_DETAIL_CONTRACT = {
  schema: "ErrorDetail",
  required: ["code", "message_key", "message", "message_args", "request_id"],
  optional: ["details"],
} as const satisfies SchemaContract;

assertKeyContract<
  ExactKeys<
    MoiraErrorDetail,
    (typeof ERROR_DETAIL_CONTRACT)["required"][number],
    (typeof ERROR_DETAIL_CONTRACT)["optional"][number]
  >
>();

/** `#/components/schemas/ErrorResponse`. **Server-side only.** */
export interface MoiraErrorResponse {
  error: MoiraErrorDetail;
}

export const ERROR_RESPONSE_CONTRACT = {
  schema: "ErrorResponse",
  required: ["error"],
  optional: [],
} as const satisfies SchemaContract;

assertKeyContract<
  ExactKeys<
    MoiraErrorResponse,
    (typeof ERROR_RESPONSE_CONTRACT)["required"][number],
    (typeof ERROR_RESPONSE_CONTRACT)["optional"][number]
  >
>();

/** Every schema descriptor, for the contract test to iterate. */
export const SCHEMA_CONTRACTS: readonly SchemaContract[] = [
  RESPONSE_TEXT_CONTRACT,
  PAGINATION_CONTRACT,
  SETUP_CLAIM_STATUS_RESPONSE_CONTRACT,
  PUBLIC_AUTH_METHOD_CONTRACT,
  SETUP_AUTH_METHODS_RESPONSE_CONTRACT,
  CLAIM_ADMIN_IDENTITY_REQUEST_CONTRACT,
  ADMIN_IDENTITY_RECORD_CONTRACT,
  AUTH_PROVIDER_SETTINGS_CREATE_REQUEST_CONTRACT,
  AUTH_PROVIDER_SETTINGS_RECORD_CONTRACT,
  TRUSTED_JWT_ISSUER_CREATE_REQUEST_CONTRACT,
  TRUSTED_JWT_ISSUER_RECORD_CONTRACT,
  ERROR_DETAIL_CONTRACT,
  ERROR_RESPONSE_CONTRACT,
];
