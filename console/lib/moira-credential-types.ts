// @server-only
//
// The provider-credential DTOs. SEPARATE FROM `lib/types.ts` ON PURPOSE.
//
// ============================================================================
// WHY THESE THREE SHAPES MAY NOT LIVE WITH THE OTHER DTOs
// ============================================================================
//
// `lib/types.ts` is named in `CLIENT_SAFE_MODULES`
// (`tests/unit/architecture/server-only-guards.test.ts`). That is an assertion
// with teeth: a `"use client"` component may import it, and Next may compile it
// into a browser bundle. The same file carries the rule
// `no Moira DTO in lib/types.ts declares a secret-shaped field`, whose pattern is
//
//   /(secret|masked|fingerprint|token|password|api_?key|private_?key|credential)/i
//
// and the credential family trips it five times over:
//
//   CredentialCreateRequest   `secret`  — the RAW api key, on its way to Moira
//   CredentialRecord          `masked_secret`, `secret_fingerprint`
//   RotateCredentialRequest   `secret`  — the replacement key
//   ApiKeyCredentialSecret    `api_key`
//
// There were two ways to land them.
//
//   REJECTED — a fourth entry in `EXEMPT_DTO_INTERFACES`. That list is CAPPED AT
//   THREE by decision W5-D4, with the reversal condition recorded in the test
//   itself: "at four, the fix is a newtype whose name does not match the
//   pattern, not a fourth carve-out". Taking the exemption would have relaxed
//   the guard across the console's entire DTO surface to accommodate one file.
//
//   TAKEN — move the family into a module the browser cannot load at all. The
//   `import "server-only"` below is a build-time guard (Next resolves the
//   package's `default` condition to a bare `throw`, so a browser bundle fails
//   `next build`), and declaring the marker also puts this path in
//   `containedModulePaths()`, so the existing "no `use client` file and nothing
//   under `components/**` imports a contained module" rules cover it with no new
//   list to maintain.
//
// This is strictly stronger than the exemption would have been: the exemption
// would have permitted a raw secret to be MODELLED in a browser-reachable
// module. Here the type is unnameable from the client.
//
// ============================================================================
// WHAT THIS MODULE MUST NEVER GROW
// ============================================================================
//
// A renderer. No formatting helper, no "display the last four characters", no
// React anything. `masked_secret` is Moira's own safe projection and is the only
// value on this surface that may be shown to an operator, and it must reach the
// browser as a field of a response a route handler CHOSE to forward — never
// because a component could import this file and reach for it.
//
// GROUND TRUTH is `docs/openapi.json`. `tests/contract/openapi-contract.test.ts`
// re-derives `CREDENTIAL_SCHEMA_CONTRACTS` from the committed spec on every run,
// with the same completeness scan it applies to `lib/types.ts`.

import "server-only";

import { assertKeyContract, type ExactKeysOf, type JsonValue, type SchemaContract } from "./types";

/* -------------------------------------------------------------------------- */
/* Enumerations                                                               */
/* -------------------------------------------------------------------------- */

/** `#/components/schemas/CredentialType` */
export type CredentialType =
  | "api_key"
  | "oauth2"
  | "bearer_token"
  | "basic_auth"
  | "custom_headers"
  | "azure_open_ai"
  | "service_account";

/**
 * `#/components/schemas/CredentialStatus`.
 *
 * Five values, and it is NOT `ResourceStatus`: `expired` and `validation_failed`
 * exist here and nowhere else. A UI that renders this through the `ResourceStatus`
 * vocabulary shows a credential Moira has stopped trusting as merely "active".
 */
export type CredentialStatus = "active" | "disabled" | "expired" | "deleted" | "validation_failed";

/* -------------------------------------------------------------------------- */
/* The untagged unions — where the wire format is ambiguous                   */
/* -------------------------------------------------------------------------- */

/**
 * `#/components/schemas/CredentialScope` — `oneOf`, discriminated by `type`.
 *
 * Modelled as a discriminated union rather than a loose object so a `tenant`
 * scope missing its `external_tenant_id` does not compile. There is no
 * `*_CONTRACT` for it: the spec node is a bare `oneOf` with no `properties` and
 * no `required`, so the descriptor machinery — which compares those two lists —
 * has nothing to compare against and would assert `required: []` about a schema
 * that has four mandatory-field variants.
 */
export type CredentialScope =
  | { readonly type: "global" }
  | { readonly type: "tenant"; readonly external_tenant_id: string }
  | {
      readonly type: "application";
      readonly application_id: string;
      readonly external_tenant_id?: string | null;
    }
  | {
      readonly type: "user";
      readonly external_user_id: string;
      readonly application_id?: string | null;
      readonly external_tenant_id?: string | null;
    };

/**
 * The `api_key` arm of `#/components/schemas/CredentialSecret`, AS A NEWTYPE.
 *
 * ============================================================================
 * WHY THIS IS A TYPE OF ITS OWN AND WHY `endpoint` IS NOT ON IT
 * ============================================================================
 *
 * `CredentialSecret` is `#[serde(untagged)]`, and two of its seven variants
 * begin with a required `api_key`:
 *
 *   variant 1  { api_key }                 the plain API key
 *   variant 6  { api_key, endpoint? }      the Azure OpenAI key
 *
 * A body of `{ "api_key": "…", "endpoint": null }` satisfies BOTH — variant 1
 * because the extra member is not forbidden, variant 6 because `endpoint` is
 * nullable — so `oneOf` matches twice and the request is refused. The failure is
 * a schema-level rejection with no field named in it, which is why sending
 * `endpoint: null` "to be explicit" is the single easiest way to make this
 * surface unusable.
 *
 * The rule is therefore not "set endpoint to null"; it is `endpoint` MUST BE
 * ABSENT. A caller cannot express the broken body through this type, and
 * `apiKeyCredentialSecret()` in `lib/moira-client.ts` builds the object literal
 * so no call site assembles one by spreading.
 *
 * `api_key` MUST ALSO BE NON-EMPTY, including against a keyless endpoint. A
 * local vLLM ignores the header entirely, but routing resolves a credential ROW
 * before it ever builds a request, and a provider with no active credential
 * fails `credential_not_found` — which reads as "the key is wrong" when the
 * truth is "there is no key at all". The constructor refuses an empty string.
 */
export interface ApiKeyCredentialSecret {
  api_key: string;
}

/**
 * The `azure_open_ai` arm. `endpoint` is OPTIONAL AND MEANINGFUL here — this is
 * the variant that owns the field, and it is what disambiguates the body above.
 */
export interface AzureCredentialSecret {
  api_key: string;
  endpoint?: string | null;
}

/**
 * The `oauth2` arm — added for the "Connect Claude subscription" flow
 * (`lib/claude-subscription.ts`), which stores a long-lived subscription token
 * (e.g. `claude setup-token` output) through this endpoint.
 *
 * ============================================================================
 * NO UNTAGGED-UNION HAZARD HERE, UNLIKE THE api_key/azure PAIR ABOVE
 * ============================================================================
 *
 * `access_token` is the only REQUIRED member, and no other `CredentialSecret`
 * variant shares it — `ApiKeyCredentialSecret`/`AzureCredentialSecret` require
 * `api_key`, `BearerToken` requires `bearer_token`, `BasicAuth` requires both
 * `username` and `password`, `CustomHeaders` requires `headers`,
 * `ServiceAccount` requires `payload`. A body of exactly `{ access_token }`
 * therefore matches this arm and no other, so `endpoint: null`'s trap (see
 * `ApiKeyCredentialSecret`'s header) simply does not apply to this shape.
 *
 * `refresh_token`/`token_type`/`expires_at` are modelled because the wire
 * schema carries them, but `oauth2CredentialSecret()` in `lib/moira-client.ts`
 * never sets them: `claude setup-token` prints a single long-lived token, not a
 * refresh-token pair, and the `oauth-token-refresh` worker that would consume a
 * refresh token does not exist yet (issue #90 — see
 * `plans/12-feature-expansion-brainstorm.md` §1).
 */
export interface OAuth2CredentialSecret {
  access_token: string;
  refresh_token?: string | null;
  token_type?: string | null;
  expires_at?: string | null;
}

/**
 * `#/components/schemas/CredentialSecret`, restricted to the arms this console
 * builds.
 *
 * The other four (`bearer_token`, `basic_auth`, `custom_headers`,
 * `service_account`) are real on the wire and deliberately unmodelled: each is a
 * distinct credential ceremony with its own storage and rotation story, and a
 * union arm nothing constructs is a shape the guards must still reason about.
 * They arrive with the flow that needs them. `oauth2` used to be in this list;
 * it left when the Claude-subscription flow needed it.
 */
export type ConsoleCredentialSecret =
  | ApiKeyCredentialSecret
  | AzureCredentialSecret
  | OAuth2CredentialSecret;

/* -------------------------------------------------------------------------- */
/* Request and record shapes                                                  */
/* -------------------------------------------------------------------------- */

/**
 * `#/components/schemas/CredentialCreateRequest`. `additionalProperties: false`.
 *
 * `provider_id` is a foreign key supplied by the caller. It must be resolved and
 * verified SERVER-SIDE before this body is built — a browser that can name the
 * provider a credential is attached to can attach its own key to somebody
 * else's provider. `createProviderCredential` states the obligation; nothing in
 * this type can enforce it, which is why it is written down where the call is.
 */
export interface CredentialCreateRequest {
  provider_id: string;
  credential_type: CredentialType;
  scope: CredentialScope;
  secret: ConsoleCredentialSecret;
  display_name?: string | null;
  expires_at?: string | null;
  metadata?: JsonValue;
  priority?: number;
}

export const CREDENTIAL_CREATE_REQUEST_CONTRACT = {
  schema: "CredentialCreateRequest",
  required: ["provider_id", "credential_type", "scope", "secret"],
  optional: ["display_name", "expires_at", "metadata", "priority"],
} as const satisfies SchemaContract;

assertKeyContract<
  ExactKeysOf<
    CredentialCreateRequest,
    (typeof CREDENTIAL_CREATE_REQUEST_CONTRACT)["required"][number],
    (typeof CREDENTIAL_CREATE_REQUEST_CONTRACT)["optional"][number]
  >
>();

/**
 * The plain API key at global scope, with both halves of the ambiguity above
 * closed at the type level. The generic per-provider "add a credential" flow
 * (`app/api/llm/providers/[id]/credentials/route.ts`) builds this.
 */
export type ConsoleApiKeyCredentialCreateRequest = Omit<
  CredentialCreateRequest,
  "credential_type" | "secret"
> & {
  credential_type: "api_key";
  secret: ApiKeyCredentialSecret;
};

/**
 * The `oauth2` credential-create body. `lib/claude-subscription.ts` builds
 * this — see `OAuth2CredentialSecret`'s header for why it needs no ambiguity
 * guard the way the api_key arm does.
 */
export type ConsoleOAuth2CredentialCreateRequest = Omit<
  CredentialCreateRequest,
  "credential_type" | "secret"
> & {
  credential_type: "oauth2";
  secret: OAuth2CredentialSecret;
};

/**
 * `#/components/schemas/CredentialRecord` — the response, AND THE REASON THIS
 * MODULE IS SERVER-ONLY.
 *
 * `masked_secret` and `secret_fingerprint` are both REQUIRED. Neither is the raw
 * key: the first is Moira's own redaction and the second is a hash. They are
 * still the two fields this console must be deliberate about, because
 * `secret_fingerprint` is a stable identifier for a secret value — equal
 * fingerprints across two deployments say the same key is installed in both —
 * and that is a fact a browser has no reason to hold.
 *
 * A route handler that forwards a credential record to the browser must project
 * it, not spread it.
 */
export interface CredentialRecord {
  id: string;
  provider_id: string;
  credential_type: CredentialType;
  scope: CredentialScope;
  secret_fingerprint: string;
  masked_secret: string;
  status: CredentialStatus;
  priority: number;
  metadata: JsonValue;
  created_at: string;
  updated_at: string;
  version: number;
  deleted_at?: string | null;
  display_name?: string | null;
  expires_at?: string | null;
  last_used_at?: string | null;
  last_validated_at?: string | null;
}

export const CREDENTIAL_RECORD_CONTRACT = {
  schema: "CredentialRecord",
  required: [
    "id",
    "provider_id",
    "credential_type",
    "scope",
    "secret_fingerprint",
    "masked_secret",
    "status",
    "priority",
    "metadata",
    "created_at",
    "updated_at",
    "version",
  ],
  optional: [
    "deleted_at",
    "display_name",
    "expires_at",
    "last_used_at",
    "last_validated_at",
  ],
} as const satisfies SchemaContract;

assertKeyContract<
  ExactKeysOf<
    CredentialRecord,
    (typeof CREDENTIAL_RECORD_CONTRACT)["required"][number],
    (typeof CREDENTIAL_RECORD_CONTRACT)["optional"][number]
  >
>();

/**
 * `#/components/schemas/RotateCredentialRequest` — ONE field.
 *
 * Rotation replaces the stored secret in place, keeping the row id, its scope
 * and every policy bound to it. It is not a create-then-delete, and the
 * operation is the one mutation on this surface that declares BOTH `If-Match`
 * (required) and `Idempotency-Key` (optional) — the precondition stops a
 * rotation racing a disable, and the key stops a retried rotation installing a
 * second new key.
 */
export interface RotateCredentialRequest {
  secret: ConsoleCredentialSecret;
}

export const ROTATE_CREDENTIAL_REQUEST_CONTRACT = {
  schema: "RotateCredentialRequest",
  required: ["secret"],
  optional: [],
} as const satisfies SchemaContract;

assertKeyContract<
  ExactKeysOf<
    RotateCredentialRequest,
    (typeof ROTATE_CREDENTIAL_REQUEST_CONTRACT)["required"][number],
    (typeof ROTATE_CREDENTIAL_REQUEST_CONTRACT)["optional"][number]
  >
>();

/**
 * The credential descriptors, for the contract test to iterate.
 *
 * Same hand-maintained hazard as `SCHEMA_CONTRACTS`, guarded the same way:
 * `tests/contract/openapi-contract.test.ts` source-scans THIS file for
 * `export const *_CONTRACT` and asserts every one appears here, then shape-checks
 * the concatenation of both arrays against `docs/openapi.json`.
 */
export const CREDENTIAL_SCHEMA_CONTRACTS: readonly SchemaContract[] = [
  CREDENTIAL_CREATE_REQUEST_CONTRACT,
  CREDENTIAL_RECORD_CONTRACT,
  ROTATE_CREDENTIAL_REQUEST_CONTRACT,
];
