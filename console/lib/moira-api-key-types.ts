// @server-only
//
// The consumer-key DTOs. SEPARATE FROM `lib/types.ts`, FOR THE SAME REASON THE
// PROVIDER-CREDENTIAL ONES ARE.
//
// ============================================================================
// WHY THESE THREE SHAPES MAY NOT LIVE WITH THE OTHER DTOs
// ============================================================================
//
// `lib/types.ts` is named in `CLIENT_SAFE_MODULES`
// (`tests/unit/architecture/server-only-guards.test.ts`), which is an assertion
// with teeth: a `"use client"` component may import it and Next may compile it
// into a browser bundle. That file therefore carries the rule
// `no Moira DTO in lib/types.ts declares a secret-shaped field`, and this family
// trips it twice:
//
//   ApiKeyRecord           `fingerprint`  — a stable hash of the live key
//   ApiKeySecretResponse   `secret`       — THE PLAINTEXT, exactly once
//
// The same two routes were open as for the credential family (issue #73), and
// the same one is taken: a module the browser cannot load at all, rather than a
// fourth entry in `EXEMPT_DTO_INTERFACES` — a list capped at three by decision
// W5-D4, whose own reversal condition says the fix at four is a different shape,
// not another carve-out.
//
// ============================================================================
// WHY A SECOND SERVER-ONLY MODULE AND NOT MORE OF `moira-credential-types.ts`
// ============================================================================
//
// That module's header names what it is: "The provider-credential DTOs" — the
// key Moira presents to an LLM provider. These are the opposite direction: the
// key an APPLICATION presents to Moira. Both are secrets and neither may reach
// the browser, but they are different surfaces with different lifecycles, and a
// module whose stated subject is one of them is the wrong home for the other.
//
// The split costs one entry in `CONTRACT_MODULES`
// (`tests/contract/openapi-contract.test.ts`) and nothing else: containment is
// derived from the `import "server-only"` marker below, so the existing rules —
// no `"use client"` file and nothing under `components/**` may import a
// contained module — cover this path with no new list to maintain.
//
// ============================================================================
// WHAT THIS MODULE MUST NEVER GROW
// ============================================================================
//
// A renderer, a formatter, or a "show the last four characters" helper. The one
// field on this surface that may be shown to an operator is `key_prefix` —
// Moira's own safe projection, which identifies a row without being usable — and
// it must reach the browser as a field of a projection a route handler CHOSE to
// build, never because a component could import this file and reach for it.
//
// GROUND TRUTH is `docs/openapi.json`. `tests/contract/openapi-contract.test.ts`
// re-derives `API_KEY_SCHEMA_CONTRACTS` from the committed spec on every run,
// with the same completeness scan it applies to the other two modules.

import "server-only";

import { assertKeyContract, type ExactKeysOf, type KeyStatus, type SchemaContract } from "./types";

/**
 * `#/components/schemas/ApiKeyRecord` — the sanitized key row.
 *
 * `key_prefix` identifies the row to a human and is not usable on its own.
 * `fingerprint` is a hash of the live secret: an identifier, and one that must
 * not be rendered — it buys an operator nothing and puts a stable cryptographic
 * value on a screen. `lib/consumer-keys.ts` drops it when it projects for the
 * browser, which is why the projection exists at all.
 */
export interface ApiKeyRecord {
  id: string;
  display_name: string;
  key_prefix: string;
  fingerprint: string;
  pepper_version: string;
  scopes: string[];
  status: KeyStatus;
  created_at: string;
  updated_at: string;
  application_id?: string | null;
  expires_at?: string | null;
  last_used_at?: string | null;
  revoked_at?: string | null;
}

export const API_KEY_RECORD_CONTRACT = {
  schema: "ApiKeyRecord",
  required: [
    "id",
    "display_name",
    "key_prefix",
    "fingerprint",
    "pepper_version",
    "scopes",
    "status",
    "created_at",
    "updated_at",
  ],
  optional: ["application_id", "expires_at", "last_used_at", "revoked_at"],
} as const satisfies SchemaContract;

assertKeyContract<
  ExactKeysOf<
    ApiKeyRecord,
    (typeof API_KEY_RECORD_CONTRACT)["required"][number],
    (typeof API_KEY_RECORD_CONTRACT)["optional"][number]
  >
>();

/**
 * `#/components/schemas/ApiKeySecretResponse` — THE ONLY SHAPE CARRYING A
 * PLAINTEXT CONSUMER KEY.
 *
 * `secret` is optional AND nullable, and null is a NORMAL, SUCCESSFUL outcome:
 * on an idempotent replay Moira returns the stored sanitized record with
 * `secret_retrievable: false` and no raw key. A UI that reads that as a failure
 * reports a correct operation as broken, on the retry path — which is precisely
 * where an operator already suspects something went wrong. The same trap is
 * documented on `AdminInviteSecretResponse`, and it is the same trap.
 *
 * Unlike the invite envelope this one carries NO `notice`, so the console
 * supplies catalogued copy of its own rather than fabricating a Moira-shaped
 * `ResponseText` that Moira never sent.
 */
export interface ApiKeySecretResponse {
  resource: ApiKeyRecord;
  secret_retrievable: boolean;
  secret?: string | null;
}

export const API_KEY_SECRET_RESPONSE_CONTRACT = {
  schema: "ApiKeySecretResponse",
  required: ["resource", "secret_retrievable"],
  optional: ["secret"],
} as const satisfies SchemaContract;

assertKeyContract<
  ExactKeysOf<
    ApiKeySecretResponse,
    (typeof API_KEY_SECRET_RESPONSE_CONTRACT)["required"][number],
    (typeof API_KEY_SECRET_RESPONSE_CONTRACT)["optional"][number]
  >
>();

/**
 * `#/components/schemas/ConsumerKeyCreateRequest`. `additionalProperties: false`.
 *
 * `application_id` is REQUIRED by the schema, which is the whole reason the keys
 * screen has to manage applications too: there is no such thing as a consumer
 * key that belongs to no application.
 */
export interface ConsumerKeyCreateRequest {
  application_id: string;
  display_name: string;
  scopes: string[];
  expires_at?: string | null;
}

export const CONSUMER_KEY_CREATE_REQUEST_CONTRACT = {
  schema: "ConsumerKeyCreateRequest",
  required: ["application_id", "display_name", "scopes"],
  optional: ["expires_at"],
} as const satisfies SchemaContract;

assertKeyContract<
  ExactKeysOf<
    ConsumerKeyCreateRequest,
    (typeof CONSUMER_KEY_CREATE_REQUEST_CONTRACT)["required"][number],
    (typeof CONSUMER_KEY_CREATE_REQUEST_CONTRACT)["optional"][number]
  >
>();

/**
 * Every descriptor this module declares, for the contract test to iterate.
 *
 * Hand-maintained, with the same hazard and the same guard as its two siblings:
 * a `*_CONTRACT` declared above and missing here is checked by nothing, and
 * `tests/contract/openapi-contract.test.ts` source-scans this file to prove the
 * two lists agree.
 */
export const API_KEY_SCHEMA_CONTRACTS: readonly SchemaContract[] = [
  API_KEY_RECORD_CONTRACT,
  API_KEY_SECRET_RESPONSE_CONTRACT,
  CONSUMER_KEY_CREATE_REQUEST_CONTRACT,
];
