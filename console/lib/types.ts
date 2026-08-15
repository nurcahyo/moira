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
 * `true` only when `TShape`'s required keys are exactly `TRequired` and its
 * optional keys are exactly `TOptional`.
 *
 * Exported so `lib/moira-credential-types.ts` — the SERVER-ONLY home of the
 * credential DTOs, which may not live here (see the note on
 * `SCHEMA_CONTRACTS`) — welds its interfaces to its descriptors with the same
 * machinery. Two copies of this would be two places for it to be subtly wrong.
 */
export type ExactKeysOf<TShape, TRequired extends string, TOptional extends string> = ExactKeys<
  TShape,
  TRequired,
  TOptional
>;

/**
 * Compile-time assertion. Instantiating it with anything but `true` — which
 * `ExactKeys` produces as `never` on any mismatch — is a type error.
 * The runtime body is deliberately empty.
 */
export const assertKeyContract = <T extends true>(_ok?: T): void => {};

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

/**
 * `#/components/schemas/AuthMethod`
 *
 * `github_oauth` arrived with `migrations/0020` (plan 09 wave 4A) and is listed here because
 * the union's job is to describe what Moira can put on the wire — a narrower union would be
 * a type that lies about the response.
 *
 * It is deliberately **not** added to `isInteractiveMethod` in this wave. The console mints
 * one `iss` for every provider, so offering a second sign-in button would mean two providers
 * sharing one console issuer — which is what finding F24 rules out. Per-provider issuers are
 * stage 4B; until they ship, a `github_oauth` row is storable in Moira and not offerable
 * here. W4-B4 (the diagnosis `method_not_interactive` gives is wrong for it) is 4B's, with
 * the N-button rendering it belongs to.
 */
export type AuthMethod = "google_oauth" | "generic_oidc" | "jwks" | "github_oauth";

/** `#/components/schemas/ResourceStatus` */
export type ResourceStatus = "active" | "disabled" | "deleted";

/**
 * `#/components/schemas/KeyStatus`.
 *
 * Client-safe, unlike the rest of the consumer-key family: it is four words, it
 * names no secret, and `/settings/keys` renders it. The shapes that carry a key
 * or a hash of one live in `lib/moira-api-key-types.ts`, which the browser
 * cannot load at all.
 */
export type KeyStatus = "active" | "revoked" | "expired" | "deleted";

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

/**
 * `#/components/schemas/PublicSignInMethod` — the ANONYMOUS projection.
 *
 * Strictly narrower than `PublicAuthMethod`, and the two omissions are the
 * point. `allowed_email_domains` is absent because it is plan 07 decision D3 —
 * the deny-by-default admin-claim policy — and publishing it anonymously would
 * hand any caller the exact list of email domains that can obtain Moira admin.
 * `jwks_url` is absent because it is machine token-verification configuration.
 *
 * The rule the spec states for this schema: every field here is something the
 * browser already transmits or receives while signing in. Adding a field that
 * fails that rule publishes it to the internet.
 *
 * CONSEQUENCE FOR THE CONSOLE: this is enough to RENDER a sign-in button and not
 * enough to RESOLVE the configuration behind one — `resolveAuthConfigs` refuses a
 * row with no `allowed_email_domains` and no `trusted_jwt_issuer_id`, and
 * neither is here.
 */
export interface PublicSignInMethod {
  id: string;
  method: AuthMethod;
  display_name: string;
  requested_scopes: string[];
  authorization_url?: string | null;
  /** Non-secret, and specifically not confidential: it appears in every OAuth
   *  redirect URL a browser sends. Moira stores no `client_secret` at all (D7). */
  client_id?: string | null;
  discovery_url?: string | null;
  issuer?: string | null;
}

export const PUBLIC_SIGN_IN_METHOD_CONTRACT = {
  schema: "PublicSignInMethod",
  required: ["id", "method", "display_name", "requested_scopes"],
  optional: ["authorization_url", "client_id", "discovery_url", "issuer"],
} as const satisfies SchemaContract;

assertKeyContract<
  ExactKeys<
    PublicSignInMethod,
    (typeof PUBLIC_SIGN_IN_METHOD_CONTRACT)["required"][number],
    (typeof PUBLIC_SIGN_IN_METHOD_CONTRACT)["optional"][number]
  >
>();

/** `#/components/schemas/SetupSignInMethodsResponse` */
export interface SetupSignInMethodsResponse {
  methods: PublicSignInMethod[];
}

export const SETUP_SIGN_IN_METHODS_RESPONSE_CONTRACT = {
  schema: "SetupSignInMethodsResponse",
  required: ["methods"],
  optional: [],
} as const satisfies SchemaContract;

assertKeyContract<
  ExactKeys<
    SetupSignInMethodsResponse,
    (typeof SETUP_SIGN_IN_METHODS_RESPONSE_CONTRACT)["required"][number],
    (typeof SETUP_SIGN_IN_METHODS_RESPONSE_CONTRACT)["optional"][number]
  >
>();

/* -------------------------------------------------------------------------- */
/* Admin invitations (plan 09 wave 2, Moira side)                             */
/* -------------------------------------------------------------------------- */

/** `#/components/schemas/AdminInviteConstraint` — mutually exclusive. */
export type AdminInviteConstraint = "email" | "domain";

/**
 * `#/components/schemas/AdminInviteStatus`.
 *
 * There is NO `expired` value: nothing sweeps for it. Expiry is derived at read
 * time — see `AdminInviteRecord.expired`.
 */
export type AdminInviteStatus = "pending" | "consumed" | "revoked";

/**
 * `#/components/schemas/AdminInviteRecord` — the invite as it is safe to return
 * AFTER creation: no token, no hash, no prefix.
 */
export interface AdminInviteRecord {
  id: string;
  constraint: AdminInviteConstraint;
  /** The email address or bare domain the invite is bound to. Never a token. */
  value: string;
  status: AdminInviteStatus;
  /** Derived, not stored. A `pending` invite past `expires_at` reads `true`. */
  expired: boolean;
  expires_at: string;
  created_at: string;
  version: number;
  consumed_at?: string | null;
  consumed_subject?: string | null;
  created_by_subject?: string | null;
}

export const ADMIN_INVITE_RECORD_CONTRACT = {
  schema: "AdminInviteRecord",
  required: [
    "id",
    "constraint",
    "value",
    "status",
    "expired",
    "expires_at",
    "created_at",
    "version",
  ],
  optional: ["consumed_at", "consumed_subject", "created_by_subject"],
} as const satisfies SchemaContract;

assertKeyContract<
  ExactKeys<
    AdminInviteRecord,
    (typeof ADMIN_INVITE_RECORD_CONTRACT)["required"][number],
    (typeof ADMIN_INVITE_RECORD_CONTRACT)["optional"][number]
  >
>();

/**
 * `#/components/schemas/AdminInviteCreateRequest`.
 *
 * `additionalProperties: false`. All three fields are required — there is no
 * "anyone with the link" invite, because an unbound invite would make a leaked
 * URL equivalent to handing out admin. `expires_in_seconds` is clamped
 * server-side as a HARD CAP: a client that asks for a year is refused, not
 * quietly honoured.
 */
export interface AdminInviteCreateRequest {
  constraint: AdminInviteConstraint;
  value: string;
  expires_in_seconds: number;
}

export const ADMIN_INVITE_CREATE_REQUEST_CONTRACT = {
  schema: "AdminInviteCreateRequest",
  required: ["constraint", "value", "expires_in_seconds"],
  optional: [],
} as const satisfies SchemaContract;

assertKeyContract<
  ExactKeys<
    AdminInviteCreateRequest,
    (typeof ADMIN_INVITE_CREATE_REQUEST_CONTRACT)["required"][number],
    (typeof ADMIN_INVITE_CREATE_REQUEST_CONTRACT)["optional"][number]
  >
>();

/**
 * `#/components/schemas/AdminInviteSecretResponse` — the once-only envelope.
 *
 * ============================================================================
 * IT IS *NOT* FIELD-FOR-FIELD `ApiKeySecretResponse`, WHATEVER THE DOC SAYS
 * ============================================================================
 *
 * Moira's own doc comment (`src/domain/identity.rs:169-174`) claims this shape
 * is "field-for-field the shape of `ApiKeySecretResponse` — `resource`,
 * `secret`, `secret_retrievable`". Verified against `docs/openapi.json`, it is
 * not: this schema carries a FOURTH REQUIRED field, `notice: ResponseText`,
 * which `ApiKeySecretResponse` does not have (`ApiKeySecretResponse.required`
 * is `["resource", "secret_retrievable"]`).
 *
 * A modal typed against the latter compiles cleanly and silently drops the
 * notice — the one string in the response that is meant to be rendered to the
 * operator through `t()`.
 *
 * ============================================================================
 * `secret === null` IS THE NORMAL CASE, NOT AN ERROR
 * ============================================================================
 *
 * `secret` is `{"type": ["string", "null"]}` and is NOT required. It carries the
 * raw token exactly once, at creation, and is `None` on an idempotent replay,
 * where the stored replay body is the sanitized record. A UI that treats `null`
 * as a failure reports a successful, correct operation as broken.
 *
 * NOTHING REDACTS THIS. `lib/errors.ts` never sees it: `moira-client.ts` calls
 * `toMoiraError` only under `if (!response.ok)`, and a 201 body is returned raw.
 */
export interface AdminInviteSecretResponse {
  resource: AdminInviteRecord;
  secret_retrievable: boolean;
  /** i18n envelope for the success message. Render through `t()`, never as
   *  hardcoded English. Absent from `ApiKeySecretResponse` — see above. */
  notice: ResponseText;
  /** The raw token. Present exactly once; `null` on an idempotent replay. */
  secret?: string | null;
}

export const ADMIN_INVITE_SECRET_RESPONSE_CONTRACT = {
  schema: "AdminInviteSecretResponse",
  required: ["resource", "secret_retrievable", "notice"],
  optional: ["secret"],
} as const satisfies SchemaContract;

assertKeyContract<
  ExactKeys<
    AdminInviteSecretResponse,
    (typeof ADMIN_INVITE_SECRET_RESPONSE_CONTRACT)["required"][number],
    (typeof ADMIN_INVITE_SECRET_RESPONSE_CONTRACT)["optional"][number]
  >
>();

/**
 * `#/components/schemas/AdminInvitePreviewRequest` — body of
 * `POST /api/v1/admin/admin-invites/preview`.
 *
 * `POST` with the token in the **body**, never a `GET` with it in the path or
 * the query string, so it cannot land in an access log, a proxy log, or a
 * `Referer` chain. `lib/invites.ts` is the only module allowed to construct it.
 *
 * The field name `token` matches `SECRET_DTO_FIELD_PATTERN` in
 * `tests/unit/architecture/server-only-guards.test.ts`. It is exempt BY NAME
 * there with its member set pinned (decision W5-D4) — the pattern is not
 * widened, because widening it would un-guard `secret`, `password` and
 * `api_key` on every other DTO.
 */
export interface AdminInvitePreviewRequest {
  token: string;
}

export const ADMIN_INVITE_PREVIEW_REQUEST_CONTRACT = {
  schema: "AdminInvitePreviewRequest",
  required: ["token"],
  optional: [],
} as const satisfies SchemaContract;

assertKeyContract<
  ExactKeys<
    AdminInvitePreviewRequest,
    (typeof ADMIN_INVITE_PREVIEW_REQUEST_CONTRACT)["required"][number],
    (typeof ADMIN_INVITE_PREVIEW_REQUEST_CONTRACT)["optional"][number]
  >
>();

/**
 * `#/components/schemas/AdminInvitePreviewResponse` — what an **unauthenticated**
 * invitee is told about an invite they hold the token for.
 *
 * THREE FIELDS, AND THE ABSENCES ARE THE DESIGN. No inviter identity, no invite
 * id, no deployment detail, no policy. Plan 09's body proposes "the inviter's
 * display email with the local part masked, e.g. `j***@example.com`" — that is
 * NOT built, and the schema's own doc comment records the reversal condition:
 * *"if product wants inviter attribution, it arrives with its own masking
 * function and its own leak test."*
 * `the_anonymous_preview_response_carries_only_constraint_and_expiry` pins it
 * server-side.
 *
 * There is deliberately no `expired` flag here (unlike `AdminInviteRecord`): an
 * anonymous caller learns expiry from `expires_at` and nothing else, and a
 * server-computed boolean would be one more bit of state to keep consistent for
 * a reader who can subtract two timestamps.
 */
export interface AdminInvitePreviewResponse {
  constraint: AdminInviteConstraint;
  /** The email address or bare domain the invite is bound to. Never a token. */
  value: string;
  expires_at: string;
}

export const ADMIN_INVITE_PREVIEW_RESPONSE_CONTRACT = {
  schema: "AdminInvitePreviewResponse",
  required: ["constraint", "value", "expires_at"],
  optional: [],
} as const satisfies SchemaContract;

assertKeyContract<
  ExactKeys<
    AdminInvitePreviewResponse,
    (typeof ADMIN_INVITE_PREVIEW_RESPONSE_CONTRACT)["required"][number],
    (typeof ADMIN_INVITE_PREVIEW_RESPONSE_CONTRACT)["optional"][number]
  >
>();

/**
 * `#/components/schemas/AdminInviteRedeemRequest` — body of
 * `POST /api/v1/admin/admin-invites/redeem`.
 *
 * Bound to plan 07's post-D5 `ClaimAdminIdentityRequest` shape: `email` and
 * `email_verified` are **required and non-optional** — no `Option`, no
 * `#[serde(default)]` — because redemption creates the same `admin_identities`
 * grant, and a grant with no human-identifiable attribute makes the domain
 * policy unenforceable on that path.
 *
 * BOTH ARE BFF-ASSERTED FROM THE JUST-VERIFIED SESSION, never from client input.
 * There is no `issuer` and no `subject` field: the operation's declared security
 * is `bearerAuth` **alone**, and Moira takes `(issuer, subject)` from the token
 * it verified — which is why this is the only console operation whose credential
 * requirement is `"bearer_only"` and why sending the system key here would be the
 * console granting admin to an identity of its own choosing.
 */
export interface AdminInviteRedeemRequest {
  token: string;
  email: string;
  email_verified: boolean;
}

export const ADMIN_INVITE_REDEEM_REQUEST_CONTRACT = {
  schema: "AdminInviteRedeemRequest",
  required: ["token", "email", "email_verified"],
  optional: [],
} as const satisfies SchemaContract;

assertKeyContract<
  ExactKeys<
    AdminInviteRedeemRequest,
    (typeof ADMIN_INVITE_REDEEM_REQUEST_CONTRACT)["required"][number],
    (typeof ADMIN_INVITE_REDEEM_REQUEST_CONTRACT)["optional"][number]
  >
>();

/**
 * `#/components/schemas/AdminIdentityPatchRequest` — the ownership transfer body
 * of `PATCH /api/v1/admin/admin-identities/{id}`.
 *
 * ONE FIELD, and it is not `granted_scopes`. Plan 09's body says this endpoint
 * "grants/revokes `moira:admins:manage` inside the target's `granted_scopes`";
 * it does not, and `granted_scopes` is never written by this path.
 * `the_ownership_patch_request_has_exactly_one_field` pins that server-side.
 *
 * `is_primary` is REQUIRED rather than optional: a `PATCH` that changes nothing
 * is a request whose `If-Match` precondition and `Idempotency-Key` ledger entry
 * describe a no-op.
 *
 * ONE CALL, NOT TWO (plan 09 §0.8 W5-B8). `set_primary` calls
 * `demote_active_primaries_other_than` inside the same transaction and
 * `admin_identities_single_active_primary` refuses a second owner outright, so
 * the plan body's "promote the target, then demote the actor" pair would demote
 * the person just promoted, or 409 on a version the actor no longer holds.
 */
export interface AdminIdentityPatchRequest {
  is_primary: boolean;
}

export const ADMIN_IDENTITY_PATCH_REQUEST_CONTRACT = {
  schema: "AdminIdentityPatchRequest",
  required: ["is_primary"],
  optional: [],
} as const satisfies SchemaContract;

assertKeyContract<
  ExactKeys<
    AdminIdentityPatchRequest,
    (typeof ADMIN_IDENTITY_PATCH_REQUEST_CONTRACT)["required"][number],
    (typeof ADMIN_IDENTITY_PATCH_REQUEST_CONTRACT)["optional"][number]
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
   * `admission_policy` resolves the governing row with
   * `where ... and trusted_jwt_issuer_id = $2` and only falls back to
   * `where ... and issuer = $1 and trusted_jwt_issuer_id is null` — the mode-3
   * bring-your-own-JWKS path. The console's claim names the *console's* issuer
   * while this row's `issuer` names the *IdP*, so without this field neither
   * stage matches, `policy = None`, and every claim is
   * `403 admin_claim_domain_not_allowed`, forever.
   *
   * From wave 4B it is also where the console READS this provider's minted
   * `iss`: the bound row's `issuer` string is what the token carries.
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
/* LLM runtime configuration — providers, models, routes, routing policies     */
/* -------------------------------------------------------------------------- */
//
// ============================================================================
// WHAT IS HERE AND WHAT IS DELIBERATELY NOT (issue #73)
// ============================================================================
//
// The LLM settings surface has five resource families. FOUR of them are here;
// the fifth — provider credentials — is in `lib/moira-credential-types.ts`,
// which carries `import "server-only"`.
//
// The split is not organisational. This module is asserted CLIENT-SAFE by
// `tests/unit/architecture/server-only-guards.test.ts` (`CLIENT_SAFE_MODULES`),
// which means a `"use client"` component may import it and Next may bundle it
// for the browser. `CredentialCreateRequest.secret` carries a raw API key and
// `CredentialRecord` carries `masked_secret` and `secret_fingerprint`; a shape
// that models any of those must not be in a module the browser can load, and
// the same test's `no Moira DTO in lib/types.ts declares a secret-shaped field`
// rule says so mechanically.
//
// That rule's exemption list is capped at THREE by decision W5-D4, and it is
// full. The recorded remedy at the cap is "introduce a newtype instead, not a
// fourth carve-out" — see `ApiKeyCredentialSecret` in the credential module.
// Adding a fourth exemption here would have been the cheaper edit and the wrong
// one: it would relax the guard on the console's whole DTO surface in exchange
// for one file's convenience.

/**
 * `#/components/schemas/ProviderType`.
 *
 * `open_ai_compatible` is the vLLM/Ollama/LM-Studio arm and is the one the
 * local-runtime path uses. It is NOT interchangeable with `open_ai`: the latter
 * ignores nothing but still defaults its base URL to OpenAI's own API, so an
 * `open_ai` row created without a `base_url` silently sends the operator's
 * prompts to api.openai.com. `assertLlmProviderCreateIsSafe` refuses the
 * compatible arm without a base URL for exactly that reason.
 */
export type ProviderType =
  | "open_ai_compatible"
  | "open_ai"
  | "anthropic"
  | "gemini"
  | "deep_seek"
  | "azure_open_ai"
  | "local"
  | "custom";

/**
 * `#/components/schemas/ProviderCreateRequest`. `additionalProperties: false`.
 *
 * NOTE WHAT IS ABSENT: there is no `enabled` field at all, unlike
 * `AuthProviderSettingsCreateRequest`. A provider row's lifecycle is moved only
 * by `POST .../enable` and `POST .../disable`, both of which require `If-Match`,
 * so the "created disabled" convention this console enforces by hand on the auth
 * surface is a property of the schema here and needs no type-level removal.
 */
export interface ProviderCreateRequest {
  provider_type: ProviderType;
  display_name: string;
  base_url?: string | null;
  metadata?: JsonValue;
}

export const PROVIDER_CREATE_REQUEST_CONTRACT = {
  schema: "ProviderCreateRequest",
  required: ["provider_type", "display_name"],
  optional: ["base_url", "metadata"],
} as const satisfies SchemaContract;

assertKeyContract<
  ExactKeys<
    ProviderCreateRequest,
    (typeof PROVIDER_CREATE_REQUEST_CONTRACT)["required"][number],
    (typeof PROVIDER_CREATE_REQUEST_CONTRACT)["optional"][number]
  >
>();

/**
 * `#/components/schemas/ProviderPatchRequest`. `additionalProperties: false`.
 *
 * `provider_type` IS IMMUTABLE, and the evidence is its absence from this
 * schema rather than a documented 4xx: `additionalProperties: false` makes a
 * patch body carrying it a flat `400`, with no error code that says "immutable".
 * `assertLlmProviderPatchIsSafe` refuses it in the console so the operator is
 * told the rule instead of being shown a generic validation failure.
 */
export interface ProviderPatchRequest {
  base_url?: string | null;
  display_name?: string | null;
  metadata?: JsonValue;
}

export const PROVIDER_PATCH_REQUEST_CONTRACT = {
  schema: "ProviderPatchRequest",
  required: [],
  optional: ["base_url", "display_name", "metadata"],
} as const satisfies SchemaContract;

assertKeyContract<
  ExactKeys<
    ProviderPatchRequest,
    (typeof PROVIDER_PATCH_REQUEST_CONTRACT)["required"][number],
    (typeof PROVIDER_PATCH_REQUEST_CONTRACT)["optional"][number]
  >
>();

/** `#/components/schemas/ProviderRecord` */
export interface ProviderRecord {
  id: string;
  provider_type: ProviderType;
  display_name: string;
  status: ResourceStatus;
  metadata: JsonValue;
  created_at: string;
  updated_at: string;
  version: number;
  base_url?: string | null;
  deleted_at?: string | null;
}

export const PROVIDER_RECORD_CONTRACT = {
  schema: "ProviderRecord",
  required: [
    "id",
    "provider_type",
    "display_name",
    "status",
    "metadata",
    "created_at",
    "updated_at",
    "version",
  ],
  optional: ["base_url", "deleted_at"],
} as const satisfies SchemaContract;

assertKeyContract<
  ExactKeys<
    ProviderRecord,
    (typeof PROVIDER_RECORD_CONTRACT)["required"][number],
    (typeof PROVIDER_RECORD_CONTRACT)["optional"][number]
  >
>();

/**
 * `#/components/schemas/ProviderModelCreateRequest`. `additionalProperties: false`.
 *
 * `capabilities` IS OPTIONAL IN THE SCHEMA AND MUST NOT BE OMITTED. An absent
 * `capabilities` is stored as SQL `null`, and routing's capability filter then
 * matches the row against nothing — the request fails with an opaque
 * `no_eligible_model` that names neither the model nor the missing field. The
 * console never constructs this type directly; it constructs
 * `ConsoleProviderModelCreateRequest` below, where the field is required.
 */
export interface ProviderModelCreateRequest {
  model_key: string;
  capabilities?: JsonValue;
  display_name?: string | null;
}

export const PROVIDER_MODEL_CREATE_REQUEST_CONTRACT = {
  schema: "ProviderModelCreateRequest",
  required: ["model_key"],
  optional: ["capabilities", "display_name"],
} as const satisfies SchemaContract;

assertKeyContract<
  ExactKeys<
    ProviderModelCreateRequest,
    (typeof PROVIDER_MODEL_CREATE_REQUEST_CONTRACT)["required"][number],
    (typeof PROVIDER_MODEL_CREATE_REQUEST_CONTRACT)["optional"][number]
  >
>();

/**
 * The only model-create body this console may construct: `capabilities` is
 * promoted from optional to REQUIRED, so the field cannot be forgotten.
 *
 * ============================================================================
 * WHAT THE TYPE DOES **NOT** GUARANTEE (issue #113)
 * ============================================================================
 *
 * This comment used to claim the promotion made the `no_eligible_model` defect
 * "unrepresentable". It does not, and reading it that way is how a runtime check
 * gets skipped as redundant. `JsonValue` INCLUDES `null`, so
 * `{ model_key, capabilities: null }` satisfies this type exactly — and `null` is
 * stored as SQL `null`, which is the same defect the required key was meant to
 * prevent. TypeScript is also erased: a body that arrived over HTTP, or through
 * an `any`, is not checked by anything here at all.
 *
 * So the division of labour is: the REQUIRED KEY makes omission a compile error,
 * and `assertProviderModelCreateIsSafe` in `lib/moira-client.ts` refuses BOTH
 * omission and `null` at runtime, on every call. Neither is redundant, and
 * `tests/unit/lib/moira-client.test.ts` pins that the guard is still invoked.
 *
 * Narrowing `capabilities` to exclude `null` was considered and rejected: it
 * would make this type say something `#/components/schemas/ProviderModelCreateRequest`
 * does not, and the value is a free-form JSON document whose shape Moira owns.
 */
export type ConsoleProviderModelCreateRequest = Omit<ProviderModelCreateRequest, "capabilities"> & {
  capabilities: JsonValue;
};

/**
 * `#/components/schemas/ProviderModelRecord`.
 *
 * `status` is a PLAIN STRING here, not `ResourceStatus`. Transcribed, not
 * assumed: the neighbouring `ProviderRecord.status` is `$ref`'d to the enum and
 * this one is `{"type": "string"}`, so narrowing it would be a type that lies
 * about what Moira can put on the wire.
 */
export interface ProviderModelRecord {
  id: string;
  provider_id: string;
  model_key: string;
  capabilities: JsonValue;
  status: string;
  created_at: string;
  updated_at: string;
  version: number;
  deleted_at?: string | null;
  display_name?: string | null;
}

export const PROVIDER_MODEL_RECORD_CONTRACT = {
  schema: "ProviderModelRecord",
  required: [
    "id",
    "provider_id",
    "model_key",
    "capabilities",
    "status",
    "created_at",
    "updated_at",
    "version",
  ],
  optional: ["deleted_at", "display_name"],
} as const satisfies SchemaContract;

assertKeyContract<
  ExactKeys<
    ProviderModelRecord,
    (typeof PROVIDER_MODEL_RECORD_CONTRACT)["required"][number],
    (typeof PROVIDER_MODEL_RECORD_CONTRACT)["optional"][number]
  >
>();

/** `#/components/schemas/RouteSelectionStrategy` */
export type RouteSelectionStrategy = "explicit" | "rules" | "default";

/**
 * `#/components/schemas/RouteDefinitionRecord` — READ-ONLY in this console.
 *
 * There is no create, patch or delete route operation in the registry, and that
 * is deliberate rather than incomplete: migration `0005` seeds the `general`
 * route, `POST /api/v1/admin/routes` documents no 409 for a duplicate
 * `route_key`, and a console that creates a second `general` produces two rows
 * that routing must choose between with no rule that says which. The console
 * reads the seeded route and binds policies to it.
 */
export interface RouteDefinitionRecord {
  id: string;
  route_key: string;
  display_name: string;
  status: ResourceStatus;
  selection_strategy: RouteSelectionStrategy;
  metadata: JsonValue;
  created_at: string;
  updated_at: string;
  version: number;
  agent_profile_id?: string | null;
  deleted_at?: string | null;
  description?: string | null;
}

export const ROUTE_DEFINITION_RECORD_CONTRACT = {
  schema: "RouteDefinitionRecord",
  required: [
    "id",
    "route_key",
    "display_name",
    "status",
    "selection_strategy",
    "metadata",
    "created_at",
    "updated_at",
    "version",
  ],
  optional: ["agent_profile_id", "deleted_at", "description"],
} as const satisfies SchemaContract;

assertKeyContract<
  ExactKeys<
    RouteDefinitionRecord,
    (typeof ROUTE_DEFINITION_RECORD_CONTRACT)["required"][number],
    (typeof ROUTE_DEFINITION_RECORD_CONTRACT)["optional"][number]
  >
>();

/**
 * `#/components/schemas/RoutingPolicyCreateRequest`. `additionalProperties: false`.
 *
 * The three required fields are all FOREIGN KEYS, and none of them may be taken
 * from a browser-supplied body without being resolved and re-checked on the
 * server first — a policy pointing at a provider the operator never chose is a
 * privileged write choosing its own target. `assertRoutingPolicyCreateIsSafe`
 * enforces the shape; resolving the ids is the caller's obligation, stated on
 * `createRoutingPolicy`.
 *
 * `POST /routing-policies` documents NO 409. Two identical policies on one route
 * are both stored and both eligible, so the caller deduplicates by listing first
 * — there is no server-side uniqueness to lean on.
 */
export interface RoutingPolicyCreateRequest {
  route_id: string;
  provider_id: string;
  provider_model_id: string;
  application_id?: string | null;
  cost_weight?: number;
  external_tenant_id?: string | null;
  latency_weight?: number;
  maximum_cost_per_request?: number | null;
  maximum_input_tokens?: number | null;
  maximum_output_tokens?: number | null;
  metadata?: JsonValue;
  priority?: number;
  privacy_class?: string | null;
  quality_weight?: number;
  required_capabilities?: string[];
  retry_policy?: JsonValue;
  timeout_ms?: number | null;
  weight?: number;
}

export const ROUTING_POLICY_CREATE_REQUEST_CONTRACT = {
  schema: "RoutingPolicyCreateRequest",
  required: ["route_id", "provider_id", "provider_model_id"],
  optional: [
    "application_id",
    "cost_weight",
    "external_tenant_id",
    "latency_weight",
    "maximum_cost_per_request",
    "maximum_input_tokens",
    "maximum_output_tokens",
    "metadata",
    "priority",
    "privacy_class",
    "quality_weight",
    "required_capabilities",
    "retry_policy",
    "timeout_ms",
    "weight",
  ],
} as const satisfies SchemaContract;

assertKeyContract<
  ExactKeys<
    RoutingPolicyCreateRequest,
    (typeof ROUTING_POLICY_CREATE_REQUEST_CONTRACT)["required"][number],
    (typeof ROUTING_POLICY_CREATE_REQUEST_CONTRACT)["optional"][number]
  >
>();

/**
 * `#/components/schemas/RoutingPolicyPatchRequest` — EVERY field optional, and
 * the three foreign keys are patchable (they are required on create and
 * nullable here).
 *
 * A patch that repoints `provider_id` moves live traffic to a different
 * provider, so the same "resolve the id server-side" obligation applies to it as
 * to create.
 */
export interface RoutingPolicyPatchRequest {
  application_id?: string | null;
  cost_weight?: number | null;
  external_tenant_id?: string | null;
  latency_weight?: number | null;
  maximum_cost_per_request?: number | null;
  maximum_input_tokens?: number | null;
  maximum_output_tokens?: number | null;
  metadata?: JsonValue;
  priority?: number | null;
  privacy_class?: string | null;
  provider_id?: string | null;
  provider_model_id?: string | null;
  quality_weight?: number | null;
  required_capabilities?: string[] | null;
  retry_policy?: JsonValue;
  route_id?: string | null;
  timeout_ms?: number | null;
  weight?: number | null;
}

export const ROUTING_POLICY_PATCH_REQUEST_CONTRACT = {
  schema: "RoutingPolicyPatchRequest",
  required: [],
  optional: [
    "application_id",
    "cost_weight",
    "external_tenant_id",
    "latency_weight",
    "maximum_cost_per_request",
    "maximum_input_tokens",
    "maximum_output_tokens",
    "metadata",
    "priority",
    "privacy_class",
    "provider_id",
    "provider_model_id",
    "quality_weight",
    "required_capabilities",
    "retry_policy",
    "route_id",
    "timeout_ms",
    "weight",
  ],
} as const satisfies SchemaContract;

assertKeyContract<
  ExactKeys<
    RoutingPolicyPatchRequest,
    (typeof ROUTING_POLICY_PATCH_REQUEST_CONTRACT)["required"][number],
    (typeof ROUTING_POLICY_PATCH_REQUEST_CONTRACT)["optional"][number]
  >
>();

/** `#/components/schemas/RoutingPolicyRecord` */
export interface RoutingPolicyRecord {
  id: string;
  route_id: string;
  provider_id: string;
  provider_model_id: string;
  priority: number;
  weight: number;
  cost_weight: number;
  latency_weight: number;
  quality_weight: number;
  required_capabilities: string[];
  retry_policy: JsonValue;
  status: ResourceStatus;
  metadata: JsonValue;
  created_at: string;
  updated_at: string;
  version: number;
  application_id?: string | null;
  deleted_at?: string | null;
  external_tenant_id?: string | null;
  maximum_cost_per_request?: number | null;
  maximum_input_tokens?: number | null;
  maximum_output_tokens?: number | null;
  privacy_class?: string | null;
  timeout_ms?: number | null;
}

export const ROUTING_POLICY_RECORD_CONTRACT = {
  schema: "RoutingPolicyRecord",
  required: [
    "id",
    "route_id",
    "provider_id",
    "provider_model_id",
    "priority",
    "weight",
    "cost_weight",
    "latency_weight",
    "quality_weight",
    "required_capabilities",
    "retry_policy",
    "status",
    "metadata",
    "created_at",
    "updated_at",
    "version",
  ],
  optional: [
    "application_id",
    "deleted_at",
    "external_tenant_id",
    "maximum_cost_per_request",
    "maximum_input_tokens",
    "maximum_output_tokens",
    "privacy_class",
    "timeout_ms",
  ],
} as const satisfies SchemaContract;

assertKeyContract<
  ExactKeys<
    RoutingPolicyRecord,
    (typeof ROUTING_POLICY_RECORD_CONTRACT)["required"][number],
    (typeof ROUTING_POLICY_RECORD_CONTRACT)["optional"][number]
  >
>();

/* -------------------------------------------------------------------------- */
/* Applications and consumer keys (issue #180)                                */
/* -------------------------------------------------------------------------- */

/**
 * `#/components/schemas/ApplicationRecord`.
 *
 * An application is what a consumer key HANGS OFF: `ConsumerKeyCreateRequest`
 * requires an `application_id`, so a console that mints keys has to be able to
 * create and list these too. It is not an optional nicety of the keys screen —
 * it is a precondition of it.
 */
export interface ApplicationRecord {
  id: string;
  display_name: string;
  status: ResourceStatus;
  metadata: JsonValue;
  created_at: string;
  updated_at: string;
  version: number;
  application_slug?: string | null;
  external_application_id?: string | null;
  deleted_at?: string | null;
}

export const APPLICATION_RECORD_CONTRACT = {
  schema: "ApplicationRecord",
  required: ["id", "display_name", "status", "metadata", "created_at", "updated_at", "version"],
  optional: ["application_slug", "external_application_id", "deleted_at"],
} as const satisfies SchemaContract;

assertKeyContract<
  ExactKeys<
    ApplicationRecord,
    (typeof APPLICATION_RECORD_CONTRACT)["required"][number],
    (typeof APPLICATION_RECORD_CONTRACT)["optional"][number]
  >
>();

/** `#/components/schemas/ApplicationCreateRequest`. `additionalProperties: false`. */
export interface ApplicationCreateRequest {
  display_name: string;
  application_slug?: string | null;
  external_application_id?: string | null;
  metadata?: JsonValue;
}

export const APPLICATION_CREATE_REQUEST_CONTRACT = {
  schema: "ApplicationCreateRequest",
  required: ["display_name"],
  optional: ["application_slug", "external_application_id", "metadata"],
} as const satisfies SchemaContract;

assertKeyContract<
  ExactKeys<
    ApplicationCreateRequest,
    (typeof APPLICATION_CREATE_REQUEST_CONTRACT)["required"][number],
    (typeof APPLICATION_CREATE_REQUEST_CONTRACT)["optional"][number]
  >
>();

/* -------------------------------------------------------------------------- */
/* Skills (plan 12 §5) — tool/guard registry, HTTP executors, OpenAPI import  */
/* -------------------------------------------------------------------------- */

/** `#/components/schemas/SkillKind`. */
export type SkillKind = "tool" | "guard";

/** `#/components/schemas/SkillStatus`. `draft` -> `enabled`/`disabled`. */
export type SkillStatus = "draft" | "enabled" | "disabled";

/**
 * `#/components/schemas/HttpMethod` as it applies to `skill_http_executors`.
 *
 * Named `SkillExecutorMethod` here rather than `HttpMethod` — `lib/moira-client.ts`
 * already exports a `HttpMethod` for the four verbs `MoiraOperation.method` uses
 * (`GET | POST | PATCH | DELETE`, no `PUT`), and reusing the name for a
 * differently-membered wire enum is exactly the kind of drift this file's own
 * contract machinery exists to catch elsewhere.
 */
export type SkillExecutorMethod = "GET" | "POST" | "PUT" | "PATCH" | "DELETE";

/** `#/components/schemas/SkillRecord`. */
export interface SkillRecord {
  id: string;
  skill_key: string;
  display_name: string;
  kind: SkillKind;
  params_schema: JsonValue;
  tags: string[];
  status: SkillStatus;
  metadata: JsonValue;
  created_at: string;
  updated_at: string;
  version: number;
  description?: string | null;
  deleted_at?: string | null;
}

export const SKILL_RECORD_CONTRACT = {
  schema: "SkillRecord",
  required: [
    "id",
    "skill_key",
    "display_name",
    "kind",
    "params_schema",
    "tags",
    "status",
    "metadata",
    "created_at",
    "updated_at",
    "version",
  ],
  optional: ["description", "deleted_at"],
} as const satisfies SchemaContract;

assertKeyContract<
  ExactKeys<
    SkillRecord,
    (typeof SKILL_RECORD_CONTRACT)["required"][number],
    (typeof SKILL_RECORD_CONTRACT)["optional"][number]
  >
>();

/** `#/components/schemas/SkillCreateRequest`. `additionalProperties: false`. */
export interface SkillCreateRequest {
  skill_key: string;
  display_name: string;
  kind: SkillKind;
  description?: string | null;
  metadata?: JsonValue;
  params_schema?: JsonValue;
  tags?: string[];
}

export const SKILL_CREATE_REQUEST_CONTRACT = {
  schema: "SkillCreateRequest",
  required: ["skill_key", "display_name", "kind"],
  optional: ["description", "metadata", "params_schema", "tags"],
} as const satisfies SchemaContract;

assertKeyContract<
  ExactKeys<
    SkillCreateRequest,
    (typeof SKILL_CREATE_REQUEST_CONTRACT)["required"][number],
    (typeof SKILL_CREATE_REQUEST_CONTRACT)["optional"][number]
  >
>();

/**
 * `#/components/schemas/SkillPatchRequest`. `additionalProperties: false`.
 *
 * `kind` and `status` are absent — `kind` is immutable after creation and
 * `status` moves only through enable/disable, mirroring `ProviderPatchRequest`'s
 * treatment of `provider_type`.
 */
export interface SkillPatchRequest {
  description?: string | null;
  display_name?: string | null;
  metadata?: JsonValue;
  params_schema?: JsonValue;
  tags?: string[] | null;
}

export const SKILL_PATCH_REQUEST_CONTRACT = {
  schema: "SkillPatchRequest",
  required: [],
  optional: ["description", "display_name", "metadata", "params_schema", "tags"],
} as const satisfies SchemaContract;

assertKeyContract<
  ExactKeys<
    SkillPatchRequest,
    (typeof SKILL_PATCH_REQUEST_CONTRACT)["required"][number],
    (typeof SKILL_PATCH_REQUEST_CONTRACT)["optional"][number]
  >
>();

/** `#/components/schemas/SkillBulkEnableRequest`. No `If-Match` — a multi-row operation with no single row version. */
export interface SkillBulkEnableRequest {
  skill_ids: string[];
}

export const SKILL_BULK_ENABLE_REQUEST_CONTRACT = {
  schema: "SkillBulkEnableRequest",
  required: ["skill_ids"],
  optional: [],
} as const satisfies SchemaContract;

assertKeyContract<
  ExactKeys<
    SkillBulkEnableRequest,
    (typeof SKILL_BULK_ENABLE_REQUEST_CONTRACT)["required"][number],
    (typeof SKILL_BULK_ENABLE_REQUEST_CONTRACT)["optional"][number]
  >
>();

/** `#/components/schemas/SkillBulkEnableResponse`. */
export interface SkillBulkEnableResponse {
  data: SkillRecord[];
}

export const SKILL_BULK_ENABLE_RESPONSE_CONTRACT = {
  schema: "SkillBulkEnableResponse",
  required: ["data"],
  optional: [],
} as const satisfies SchemaContract;

assertKeyContract<
  ExactKeys<
    SkillBulkEnableResponse,
    (typeof SKILL_BULK_ENABLE_RESPONSE_CONTRACT)["required"][number],
    (typeof SKILL_BULK_ENABLE_RESPONSE_CONTRACT)["optional"][number]
  >
>();

/**
 * `#/components/schemas/SkillImportRequest` — the raw OpenAPI 3.x document to
 * import (plan 12 §5). Parsing happens server-side in Moira; the console never
 * inspects `document` beyond forwarding it as JSON.
 */
export interface SkillImportRequest {
  document: JsonValue;
}

export const SKILL_IMPORT_REQUEST_CONTRACT = {
  schema: "SkillImportRequest",
  required: ["document"],
  optional: [],
} as const satisfies SchemaContract;

assertKeyContract<
  ExactKeys<
    SkillImportRequest,
    (typeof SKILL_IMPORT_REQUEST_CONTRACT)["required"][number],
    (typeof SKILL_IMPORT_REQUEST_CONTRACT)["optional"][number]
  >
>();

/**
 * `#/components/schemas/SkillImportResponse` — one row per imported operation
 * (a draft `skills` row plus its `skill_http_executors` row), capped at 300
 * operations server-side (plan 12 §5 decision 23).
 */
export interface SkillImportResponse {
  imported_count: number;
  skills: SkillRecord[];
  executors: SkillHttpExecutorRecord[];
}

export const SKILL_IMPORT_RESPONSE_CONTRACT = {
  schema: "SkillImportResponse",
  required: ["imported_count", "skills", "executors"],
  optional: [],
} as const satisfies SchemaContract;

assertKeyContract<
  ExactKeys<
    SkillImportResponse,
    (typeof SKILL_IMPORT_RESPONSE_CONTRACT)["required"][number],
    (typeof SKILL_IMPORT_RESPONSE_CONTRACT)["optional"][number]
  >
>();

/**
 * `#/components/schemas/SkillHttpExecutorRecord`.
 *
 * NO `version` FIELD. `skill_http_executors` has no `version`/`deleted_at`
 * columns — optimistic concurrency on this surface is given only to the three
 * named registries (`skills`, `eval_suites`, `agent_flows`); this row is a
 * versionless 1:1 child of a `skills` row. `updated_at` is this resource's
 * `If-Match` basis instead, and the header it round-trips through is a QUOTED
 * RFC 3339 timestamp rather than an integer — see `skillExecutorIfMatchFor` in
 * `lib/moira-client.ts`.
 */
export interface SkillHttpExecutorRecord {
  skill_id: string;
  method: SkillExecutorMethod;
  url_template: string;
  allowed_host: string;
  header_template: JsonValue;
  timeout_ms: number;
  created_at: string;
  updated_at: string;
  credential_id?: string | null;
  response_schema?: JsonValue;
}

export const SKILL_HTTP_EXECUTOR_RECORD_CONTRACT = {
  schema: "SkillHttpExecutorRecord",
  required: [
    "skill_id",
    "method",
    "url_template",
    "allowed_host",
    "header_template",
    "timeout_ms",
    "created_at",
    "updated_at",
  ],
  optional: ["credential_id", "response_schema"],
} as const satisfies SchemaContract;

assertKeyContract<
  ExactKeys<
    SkillHttpExecutorRecord,
    (typeof SKILL_HTTP_EXECUTOR_RECORD_CONTRACT)["required"][number],
    (typeof SKILL_HTTP_EXECUTOR_RECORD_CONTRACT)["optional"][number]
  >
>();

/**
 * `#/components/schemas/SkillHttpExecutorPatchRequest`. `additionalProperties: false`.
 *
 * `allowed_host` is absent — see `SkillHttpExecutorRecord.allowed_host`'s own
 * note. Changing `url_template` re-derives and re-validates the allowed host
 * server-side rather than taking a client-supplied value.
 */
export interface SkillHttpExecutorPatchRequest {
  credential_id?: string | null;
  header_template?: JsonValue;
  method?: SkillExecutorMethod | null;
  response_schema?: JsonValue;
  timeout_ms?: number | null;
  url_template?: string | null;
}

export const SKILL_HTTP_EXECUTOR_PATCH_REQUEST_CONTRACT = {
  schema: "SkillHttpExecutorPatchRequest",
  required: [],
  optional: ["credential_id", "header_template", "method", "response_schema", "timeout_ms", "url_template"],
} as const satisfies SchemaContract;

assertKeyContract<
  ExactKeys<
    SkillHttpExecutorPatchRequest,
    (typeof SKILL_HTTP_EXECUTOR_PATCH_REQUEST_CONTRACT)["required"][number],
    (typeof SKILL_HTTP_EXECUTOR_PATCH_REQUEST_CONTRACT)["optional"][number]
  >
>();

/* -------------------------------------------------------------------------- */
/* Provider health (issue #83) — read-only rolling reachability window       */
/* -------------------------------------------------------------------------- */

/**
 * `#/components/schemas/ProviderHealthStatus`. `unknown` is a provider with no
 * snapshot in the rolling window at all — never probed, or not probed recently
 * enough to still be in window — and is distinct from `unhealthy`.
 */
export type ProviderHealthStatus = "healthy" | "degraded" | "unhealthy" | "unknown";

/** `#/components/schemas/ProviderHealthEntry` — one provider's rolling health window. */
export interface ProviderHealthEntry {
  provider_id: string;
  provider_type: ProviderType;
  display_name: string;
  status: ProviderHealthStatus;
  probes_total: number;
  probes_successful: number;
  average_latency_ms?: number | null;
  last_failure_at?: string | null;
  last_probe_at?: string | null;
  last_success_at?: string | null;
}

export const PROVIDER_HEALTH_ENTRY_CONTRACT = {
  schema: "ProviderHealthEntry",
  required: ["provider_id", "provider_type", "display_name", "status", "probes_total", "probes_successful"],
  optional: ["average_latency_ms", "last_failure_at", "last_probe_at", "last_success_at"],
} as const satisfies SchemaContract;

assertKeyContract<
  ExactKeys<
    ProviderHealthEntry,
    (typeof PROVIDER_HEALTH_ENTRY_CONTRACT)["required"][number],
    (typeof PROVIDER_HEALTH_ENTRY_CONTRACT)["optional"][number]
  >
>();

/** `#/components/schemas/ProviderHealthResponse` — `GET /api/v1/admin/providers/health`'s whole body. */
export interface ProviderHealthResponse {
  providers: ProviderHealthEntry[];
}

export const PROVIDER_HEALTH_RESPONSE_CONTRACT = {
  schema: "ProviderHealthResponse",
  required: ["providers"],
  optional: [],
} as const satisfies SchemaContract;

assertKeyContract<
  ExactKeys<
    ProviderHealthResponse,
    (typeof PROVIDER_HEALTH_RESPONSE_CONTRACT)["required"][number],
    (typeof PROVIDER_HEALTH_RESPONSE_CONTRACT)["optional"][number]
  >
>();

/* -------------------------------------------------------------------------- */
/* Eval suites (plan 12 §3) — offline/online grading                         */
/* -------------------------------------------------------------------------- */

/** `#/components/schemas/GradingKind`. `llm_judge` is deliberately absent (decision 14). */
export type GradingKind = "exact_match" | "contains" | "schema_valid";

/** `#/components/schemas/EvalRunStatus`. */
export type EvalRunStatus = "pending" | "running" | "completed" | "failed";

/** `#/components/schemas/EvalTriggerKind`. */
export type EvalTriggerKind = "offline_manual" | "offline_ci" | "online_sampled";

/** `#/components/schemas/EvalSuiteRecord`. */
export interface EvalSuiteRecord {
  id: string;
  suite_key: string;
  display_name: string;
  status: ResourceStatus;
  metadata: JsonValue;
  created_at: string;
  updated_at: string;
  version: number;
  description?: string | null;
  deleted_at?: string | null;
}

export const EVAL_SUITE_RECORD_CONTRACT = {
  schema: "EvalSuiteRecord",
  required: ["id", "suite_key", "display_name", "status", "metadata", "created_at", "updated_at", "version"],
  optional: ["description", "deleted_at"],
} as const satisfies SchemaContract;

assertKeyContract<
  ExactKeys<
    EvalSuiteRecord,
    (typeof EVAL_SUITE_RECORD_CONTRACT)["required"][number],
    (typeof EVAL_SUITE_RECORD_CONTRACT)["optional"][number]
  >
>();

/** `#/components/schemas/EvalSuiteCreateRequest`. `additionalProperties: false`. */
export interface EvalSuiteCreateRequest {
  suite_key: string;
  display_name: string;
  description?: string | null;
  metadata?: JsonValue;
}

export const EVAL_SUITE_CREATE_REQUEST_CONTRACT = {
  schema: "EvalSuiteCreateRequest",
  required: ["suite_key", "display_name"],
  optional: ["description", "metadata"],
} as const satisfies SchemaContract;

assertKeyContract<
  ExactKeys<
    EvalSuiteCreateRequest,
    (typeof EVAL_SUITE_CREATE_REQUEST_CONTRACT)["required"][number],
    (typeof EVAL_SUITE_CREATE_REQUEST_CONTRACT)["optional"][number]
  >
>();

/** `#/components/schemas/EvalSuitePatchRequest`. `additionalProperties: false`. */
export interface EvalSuitePatchRequest {
  description?: string | null;
  display_name?: string | null;
  metadata?: JsonValue;
}

export const EVAL_SUITE_PATCH_REQUEST_CONTRACT = {
  schema: "EvalSuitePatchRequest",
  required: [],
  optional: ["description", "display_name", "metadata"],
} as const satisfies SchemaContract;

assertKeyContract<
  ExactKeys<
    EvalSuitePatchRequest,
    (typeof EVAL_SUITE_PATCH_REQUEST_CONTRACT)["required"][number],
    (typeof EVAL_SUITE_PATCH_REQUEST_CONTRACT)["optional"][number]
  >
>();

/** `#/components/schemas/EvalCaseRecord`. No `version` — cases have no PATCH surface (migration header). */
export interface EvalCaseRecord {
  id: string;
  suite_id: string;
  input: JsonValue;
  expected: JsonValue;
  grading_kind: GradingKind;
  metadata: JsonValue;
  created_at: string;
}

export const EVAL_CASE_RECORD_CONTRACT = {
  schema: "EvalCaseRecord",
  required: ["id", "suite_id", "input", "expected", "grading_kind", "metadata", "created_at"],
  optional: [],
} as const satisfies SchemaContract;

assertKeyContract<
  ExactKeys<
    EvalCaseRecord,
    (typeof EVAL_CASE_RECORD_CONTRACT)["required"][number],
    (typeof EVAL_CASE_RECORD_CONTRACT)["optional"][number]
  >
>();

/** `#/components/schemas/EvalCaseCreateRequest`. `additionalProperties: false`. */
export interface EvalCaseCreateRequest {
  input: JsonValue;
  expected: JsonValue;
  grading_kind: GradingKind;
  metadata?: JsonValue;
}

export const EVAL_CASE_CREATE_REQUEST_CONTRACT = {
  schema: "EvalCaseCreateRequest",
  required: ["input", "expected", "grading_kind"],
  optional: ["metadata"],
} as const satisfies SchemaContract;

assertKeyContract<
  ExactKeys<
    EvalCaseCreateRequest,
    (typeof EVAL_CASE_CREATE_REQUEST_CONTRACT)["required"][number],
    (typeof EVAL_CASE_CREATE_REQUEST_CONTRACT)["optional"][number]
  >
>();

/** `#/components/schemas/EvalRunRecord`. Read-only — produced by the eval runner. */
export interface EvalRunRecord {
  id: string;
  trigger_kind: EvalTriggerKind;
  status: EvalRunStatus;
  results: JsonValue;
  metadata: JsonValue;
  created_at: string;
  agent_profile_id?: string | null;
  completed_at?: string | null;
  execution_id?: string | null;
  score?: number | null;
  suite_id?: string | null;
}

export const EVAL_RUN_RECORD_CONTRACT = {
  schema: "EvalRunRecord",
  required: ["id", "trigger_kind", "status", "results", "metadata", "created_at"],
  optional: ["agent_profile_id", "completed_at", "execution_id", "score", "suite_id"],
} as const satisfies SchemaContract;

assertKeyContract<
  ExactKeys<
    EvalRunRecord,
    (typeof EVAL_RUN_RECORD_CONTRACT)["required"][number],
    (typeof EVAL_RUN_RECORD_CONTRACT)["optional"][number]
  >
>();

/* -------------------------------------------------------------------------- */
/* Agent flows (plan 12 §6) — ordered, sequential-only MVP                   */
/* -------------------------------------------------------------------------- */

/** `#/components/schemas/FlowRunStatus`. */
export type FlowRunStatus = "running" | "completed" | "failed" | "cancelled";

/** `#/components/schemas/FlowStepOnFailure`. */
export type FlowStepOnFailure = "abort" | "continue";

/** `#/components/schemas/AgentFlowStepRecord`. */
export interface AgentFlowStepRecord {
  id: string;
  flow_id: string;
  step_key: string;
  step_order: number;
  agent_profile_id: string;
  on_failure: FlowStepOnFailure;
  input_mapping: JsonValue;
  metadata: JsonValue;
  created_at: string;
}

export const AGENT_FLOW_STEP_RECORD_CONTRACT = {
  schema: "AgentFlowStepRecord",
  required: [
    "id",
    "flow_id",
    "step_key",
    "step_order",
    "agent_profile_id",
    "on_failure",
    "input_mapping",
    "metadata",
    "created_at",
  ],
  optional: [],
} as const satisfies SchemaContract;

assertKeyContract<
  ExactKeys<
    AgentFlowStepRecord,
    (typeof AGENT_FLOW_STEP_RECORD_CONTRACT)["required"][number],
    (typeof AGENT_FLOW_STEP_RECORD_CONTRACT)["optional"][number]
  >
>();

/** `#/components/schemas/AgentFlowStepCreateRequest`. `additionalProperties: false`. */
export interface AgentFlowStepCreateRequest {
  step_key: string;
  step_order: number;
  agent_profile_id: string;
  input_mapping?: JsonValue;
  metadata?: JsonValue;
  on_failure?: FlowStepOnFailure;
}

export const AGENT_FLOW_STEP_CREATE_REQUEST_CONTRACT = {
  schema: "AgentFlowStepCreateRequest",
  required: ["step_key", "step_order", "agent_profile_id"],
  optional: ["input_mapping", "metadata", "on_failure"],
} as const satisfies SchemaContract;

assertKeyContract<
  ExactKeys<
    AgentFlowStepCreateRequest,
    (typeof AGENT_FLOW_STEP_CREATE_REQUEST_CONTRACT)["required"][number],
    (typeof AGENT_FLOW_STEP_CREATE_REQUEST_CONTRACT)["optional"][number]
  >
>();

/**
 * `#/components/schemas/AgentFlowRecord`.
 *
 * A flow's steps are managed as an ordered array INSIDE this record rather than
 * through a separate sub-resource (decision 13: simplest contract, matches the
 * sequential-only MVP) — `create`/`patch` accept the whole ordered list and this
 * record echoes it back.
 */
export interface AgentFlowRecord {
  id: string;
  flow_key: string;
  display_name: string;
  status: ResourceStatus;
  metadata: JsonValue;
  created_at: string;
  updated_at: string;
  version: number;
  steps: AgentFlowStepRecord[];
  description?: string | null;
  deleted_at?: string | null;
}

export const AGENT_FLOW_RECORD_CONTRACT = {
  schema: "AgentFlowRecord",
  required: [
    "id",
    "flow_key",
    "display_name",
    "status",
    "metadata",
    "created_at",
    "updated_at",
    "version",
    "steps",
  ],
  optional: ["description", "deleted_at"],
} as const satisfies SchemaContract;

assertKeyContract<
  ExactKeys<
    AgentFlowRecord,
    (typeof AGENT_FLOW_RECORD_CONTRACT)["required"][number],
    (typeof AGENT_FLOW_RECORD_CONTRACT)["optional"][number]
  >
>();

/** `#/components/schemas/AgentFlowCreateRequest`. `additionalProperties: false`. */
export interface AgentFlowCreateRequest {
  flow_key: string;
  display_name: string;
  description?: string | null;
  metadata?: JsonValue;
  steps?: AgentFlowStepCreateRequest[];
}

export const AGENT_FLOW_CREATE_REQUEST_CONTRACT = {
  schema: "AgentFlowCreateRequest",
  required: ["flow_key", "display_name"],
  optional: ["description", "metadata", "steps"],
} as const satisfies SchemaContract;

assertKeyContract<
  ExactKeys<
    AgentFlowCreateRequest,
    (typeof AGENT_FLOW_CREATE_REQUEST_CONTRACT)["required"][number],
    (typeof AGENT_FLOW_CREATE_REQUEST_CONTRACT)["optional"][number]
  >
>();

/**
 * `#/components/schemas/AgentFlowPatchRequest`. `additionalProperties: false`.
 *
 * `steps: Some(...)` atomically REPLACES the whole ordered list; omitted leaves
 * the existing steps untouched — the same coalesce convention every other patch
 * request in this file follows for its scalar fields.
 */
export interface AgentFlowPatchRequest {
  description?: string | null;
  display_name?: string | null;
  metadata?: JsonValue;
  steps?: AgentFlowStepCreateRequest[] | null;
}

export const AGENT_FLOW_PATCH_REQUEST_CONTRACT = {
  schema: "AgentFlowPatchRequest",
  required: [],
  optional: ["description", "display_name", "metadata", "steps"],
} as const satisfies SchemaContract;

assertKeyContract<
  ExactKeys<
    AgentFlowPatchRequest,
    (typeof AGENT_FLOW_PATCH_REQUEST_CONTRACT)["required"][number],
    (typeof AGENT_FLOW_PATCH_REQUEST_CONTRACT)["optional"][number]
  >
>();

/** `#/components/schemas/AgentFlowRunRecord`. Read-only — produced by the flow orchestrator. */
export interface AgentFlowRunRecord {
  id: string;
  flow_id: string;
  status: FlowRunStatus;
  metadata: JsonValue;
  created_at: string;
  completed_at?: string | null;
}

export const AGENT_FLOW_RUN_RECORD_CONTRACT = {
  schema: "AgentFlowRunRecord",
  required: ["id", "flow_id", "status", "metadata", "created_at"],
  optional: ["completed_at"],
} as const satisfies SchemaContract;

assertKeyContract<
  ExactKeys<
    AgentFlowRunRecord,
    (typeof AGENT_FLOW_RUN_RECORD_CONTRACT)["required"][number],
    (typeof AGENT_FLOW_RUN_RECORD_CONTRACT)["optional"][number]
  >
>();

/**
 * `#/components/schemas/AgentProfileRecord` — MINIMAL, for the flow step
 * builder's "which agent profile does this step run" picker. The console owns
 * no create/edit surface for agent profiles; this is read-only.
 */
export interface AgentProfileRecord {
  id: string;
  profile_key: string;
  display_name: string;
  tool_policy: JsonValue;
  context_policy: JsonValue;
  memory_policy: JsonValue;
  status: ResourceStatus;
  metadata: JsonValue;
  created_at: string;
  updated_at: string;
  version: number;
  preamble?: string | null;
  temperature?: number | null;
  max_tokens?: number | null;
  deleted_at?: string | null;
}

export const AGENT_PROFILE_RECORD_CONTRACT = {
  schema: "AgentProfileRecord",
  required: [
    "id",
    "profile_key",
    "display_name",
    "tool_policy",
    "context_policy",
    "memory_policy",
    "status",
    "metadata",
    "created_at",
    "updated_at",
    "version",
  ],
  optional: ["preamble", "temperature", "max_tokens", "deleted_at"],
} as const satisfies SchemaContract;

assertKeyContract<
  ExactKeys<
    AgentProfileRecord,
    (typeof AGENT_PROFILE_RECORD_CONTRACT)["required"][number],
    (typeof AGENT_PROFILE_RECORD_CONTRACT)["optional"][number]
  >
>();

/* -------------------------------------------------------------------------- */
/* Relationship graph (plan 12 §4, issue #234) — derived, read-only          */
/* -------------------------------------------------------------------------- */

/** `#/components/schemas/GraphNodeType` */
export type GraphNodeType =
  "agent" | "skill" | "eval_suite" | "flow" | "provider" | "model" | "memory_scope";

/** `#/components/schemas/GraphEdgeKind` */
export type GraphEdgeKind =
  | "agent_uses_skill"
  | "agent_uses_eval_suite"
  | "agent_reads_memory_scope"
  | "flow_contains_agent"
  | "agent_routes_to_model";

/**
 * `#/components/schemas/GraphNode`. `id` is `"<type>:<key>"` (e.g. `"agent:0199…"`), composite
 * so an edge's `from`/`to` never collides across node types. `status` is `null` for the
 * synthetic `memory_scope` node type, which has no row of its own to carry one.
 */
export interface GraphNode {
  id: string;
  type: GraphNodeType;
  label: string;
  status?: string | null;
}

/** `#/components/schemas/GraphEdge`. `from`/`to` are `GraphNode.id` values. */
export interface GraphEdge {
  from: string;
  to: string;
  kind: GraphEdgeKind;
}

export const GRAPH_NODE_CONTRACT = {
  schema: "GraphNode",
  required: ["id", "type", "label"],
  optional: ["status"],
} as const satisfies SchemaContract;

assertKeyContract<
  ExactKeys<
    GraphNode,
    (typeof GRAPH_NODE_CONTRACT)["required"][number],
    (typeof GRAPH_NODE_CONTRACT)["optional"][number]
  >
>();

export const GRAPH_EDGE_CONTRACT = {
  schema: "GraphEdge",
  required: ["from", "to", "kind"],
  optional: [],
} as const satisfies SchemaContract;

assertKeyContract<
  ExactKeys<
    GraphEdge,
    (typeof GRAPH_EDGE_CONTRACT)["required"][number],
    (typeof GRAPH_EDGE_CONTRACT)["optional"][number]
  >
>();

/** `#/components/schemas/GraphResponse` — `GET /api/v1/admin/graph`'s whole body. */
export interface GraphResponse {
  nodes: GraphNode[];
  edges: GraphEdge[];
  generated_at: string;
}

export const GRAPH_RESPONSE_CONTRACT = {
  schema: "GraphResponse",
  required: ["nodes", "edges", "generated_at"],
  optional: [],
} as const satisfies SchemaContract;

assertKeyContract<
  ExactKeys<
    GraphResponse,
    (typeof GRAPH_RESPONSE_CONTRACT)["required"][number],
    (typeof GRAPH_RESPONSE_CONTRACT)["optional"][number]
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

/**
 * Every schema descriptor, for the contract test to iterate.
 *
 * HAND-MAINTAINED, AND THAT IS A HAZARD WITH A GUARD. A `*_CONTRACT` declared
 * above but missing from this array is checked by NOTHING — the interface still
 * type-checks against its own descriptor, and the claim that "the contract test
 * re-derives everything from the spec" quietly stops being true for that one
 * DTO. `tests/contract/openapi-contract.test.ts` source-scans this file for
 * `export const *_CONTRACT` and asserts every one appears here, with a count.
 *
 * IT IS NOT THE WHOLE SET. The credential descriptors live in
 * `lib/moira-credential-types.ts` and are registered in that module's own
 * `CREDENTIAL_SCHEMA_CONTRACTS`; the contract test scans BOTH files with the
 * same completeness rule and shape-checks the concatenation. A descriptor that
 * reaches neither array is checked by nothing, which is the hazard the scan
 * exists for — moving DTOs to a second module must not create a second way to
 * fall out of the gate.
 */
export const SCHEMA_CONTRACTS: readonly SchemaContract[] = [
  RESPONSE_TEXT_CONTRACT,
  PAGINATION_CONTRACT,
  SETUP_CLAIM_STATUS_RESPONSE_CONTRACT,
  PUBLIC_AUTH_METHOD_CONTRACT,
  SETUP_AUTH_METHODS_RESPONSE_CONTRACT,
  PUBLIC_SIGN_IN_METHOD_CONTRACT,
  SETUP_SIGN_IN_METHODS_RESPONSE_CONTRACT,
  CLAIM_ADMIN_IDENTITY_REQUEST_CONTRACT,
  ADMIN_IDENTITY_RECORD_CONTRACT,
  ADMIN_INVITE_RECORD_CONTRACT,
  ADMIN_INVITE_CREATE_REQUEST_CONTRACT,
  ADMIN_INVITE_SECRET_RESPONSE_CONTRACT,
  ADMIN_INVITE_PREVIEW_REQUEST_CONTRACT,
  ADMIN_INVITE_PREVIEW_RESPONSE_CONTRACT,
  ADMIN_INVITE_REDEEM_REQUEST_CONTRACT,
  ADMIN_IDENTITY_PATCH_REQUEST_CONTRACT,
  AUTH_PROVIDER_SETTINGS_CREATE_REQUEST_CONTRACT,
  AUTH_PROVIDER_SETTINGS_RECORD_CONTRACT,
  TRUSTED_JWT_ISSUER_CREATE_REQUEST_CONTRACT,
  TRUSTED_JWT_ISSUER_RECORD_CONTRACT,
  PROVIDER_CREATE_REQUEST_CONTRACT,
  PROVIDER_PATCH_REQUEST_CONTRACT,
  PROVIDER_RECORD_CONTRACT,
  PROVIDER_MODEL_CREATE_REQUEST_CONTRACT,
  PROVIDER_MODEL_RECORD_CONTRACT,
  ROUTE_DEFINITION_RECORD_CONTRACT,
  ROUTING_POLICY_CREATE_REQUEST_CONTRACT,
  ROUTING_POLICY_PATCH_REQUEST_CONTRACT,
  ROUTING_POLICY_RECORD_CONTRACT,
  APPLICATION_RECORD_CONTRACT,
  APPLICATION_CREATE_REQUEST_CONTRACT,
  SKILL_RECORD_CONTRACT,
  SKILL_CREATE_REQUEST_CONTRACT,
  SKILL_PATCH_REQUEST_CONTRACT,
  SKILL_BULK_ENABLE_REQUEST_CONTRACT,
  SKILL_BULK_ENABLE_RESPONSE_CONTRACT,
  SKILL_IMPORT_REQUEST_CONTRACT,
  SKILL_IMPORT_RESPONSE_CONTRACT,
  SKILL_HTTP_EXECUTOR_RECORD_CONTRACT,
  SKILL_HTTP_EXECUTOR_PATCH_REQUEST_CONTRACT,
  PROVIDER_HEALTH_ENTRY_CONTRACT,
  PROVIDER_HEALTH_RESPONSE_CONTRACT,
  EVAL_SUITE_RECORD_CONTRACT,
  EVAL_SUITE_CREATE_REQUEST_CONTRACT,
  EVAL_SUITE_PATCH_REQUEST_CONTRACT,
  EVAL_CASE_RECORD_CONTRACT,
  EVAL_CASE_CREATE_REQUEST_CONTRACT,
  EVAL_RUN_RECORD_CONTRACT,
  AGENT_FLOW_STEP_RECORD_CONTRACT,
  AGENT_FLOW_STEP_CREATE_REQUEST_CONTRACT,
  AGENT_FLOW_RECORD_CONTRACT,
  AGENT_FLOW_CREATE_REQUEST_CONTRACT,
  AGENT_FLOW_PATCH_REQUEST_CONTRACT,
  AGENT_FLOW_RUN_RECORD_CONTRACT,
  AGENT_PROFILE_RECORD_CONTRACT,
  GRAPH_NODE_CONTRACT,
  GRAPH_EDGE_CONTRACT,
  GRAPH_RESPONSE_CONTRACT,
  ERROR_DETAIL_CONTRACT,
  ERROR_RESPONSE_CONTRACT,
];
