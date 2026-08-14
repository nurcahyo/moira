// The CLIENT-SAFE view model for `/settings/keys`.
//
// ============================================================================
// WHY IT IS A SEPARATE MODULE FROM `lib/consumer-keys.ts`
// ============================================================================
//
// Same split, same reason, as `lib/llm-view.ts` against `lib/llm-settings.ts`:
// the orchestration module imports `lib/moira-api-key-types.ts`, which is
// server-only, so a `"use client"` organism cannot import it even for a type.
// The shapes the organisms render therefore live here, where the browser may
// reach them, and the server module builds them.
//
// Nothing here names a secret, and that is a property of the SHAPES rather than
// a promise about the code: `fingerprint` and `pepper_version` are absent from
// `ConsumerKeyView`, so a component cannot render them by mistake and a future
// edit cannot widen the projection without editing this file, where the omission
// is stated.
//
// `key_prefix` IS here. It is Moira's own safe projection of the credential —
// enough to tell two keys apart in a list, not enough to authenticate with — and
// it is the only part of a key this console ever displays after the once-only
// modal closes.

import type { KeyStatus, ResourceStatus } from "./types";

/**
 * `POST /api/keys`'s response — the ONE shape in this module that names a
 * plaintext, and the reason it is here rather than in `lib/types.ts`.
 *
 * `lib/types.ts` is on `CLIENT_SAFE_MODULES` and is scanned for secret-shaped
 * fields, with a per-interface exemption list W5-D4 caps at three; that cap is
 * already spent, and its recorded remedy is containment, not a fourth carve-out.
 * Moira's own envelope IS contained (`lib/moira-api-key-types.ts`, server-only).
 * What cannot be contained is this one: the whole feature is that the operator
 * sees the key exactly once, so the value has to reach the browser, and the type
 * describing it has to be nameable from a client component.
 *
 * What contains it instead is the same thing that contains the invitation token:
 * the value lives in ONE component (`OnceOnlySecretModal`), the mount is on
 * `no-secret-props.test.ts`'s rule-(c) list, and `secret-leak.e2e.ts` fails if
 * the value reaches the browser by any other route.
 *
 * `resource` is the NARROWED view, not Moira's `ApiKeyRecord`: the route handler
 * drops `fingerprint` and `pepper_version` before this ever exists.
 */
export interface MintedConsumerKey {
  readonly resource: ConsumerKeyView;
  /** The plaintext, or `null` on an idempotent replay — which is a SUCCESS. */
  readonly secret: string | null;
  readonly secret_retrievable: boolean;
}

/** One consumer key, as the browser may see it. */
export interface ConsumerKeyView {
  readonly id: string;
  readonly display_name: string;
  readonly key_prefix: string;
  readonly scopes: readonly string[];
  readonly status: KeyStatus;
  readonly created_at: string;
  readonly last_used_at: string | null;
  readonly expires_at: string | null;
}

/** One application and the keys hanging off it. */
export interface ApplicationView {
  readonly id: string;
  readonly display_name: string;
  readonly application_slug: string | null;
  readonly status: ResourceStatus;
  readonly keys: readonly ConsumerKeyView[];
}

export interface ConsumerKeysView {
  readonly applications: readonly ApplicationView[];
  /**
   * Keys whose `application_id` matches no listed application — a deleted
   * application, or one past the page limit.
   *
   * Rendered rather than dropped. A key that authenticates against this
   * deployment and appears on no screen is exactly the credential nobody
   * revokes, and filtering it silently would make the list a claim ("these are
   * the keys") it cannot support.
   */
  readonly unattachedKeys: readonly ConsumerKeyView[];
  /** Whether either Moira list reported more pages, so the screen can say so. */
  readonly truncated: boolean;
  /** The scopes the mint form may offer. Curated server-side. */
  readonly offeredScopes: readonly string[];
  /** Pre-checked on the mint form; a key without it can do nothing. */
  readonly defaultScope: string;
}
