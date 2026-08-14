// @server-only
//
// "Connect Claude subscription" — store a long-lived Claude subscription token
// (e.g. `claude setup-token` output) as an `oauth2` provider credential,
// through Moira's EXISTING `POST /api/v1/admin/provider-credentials`. No new
// Moira HTTP endpoint. See `docs/claude-subscription-sidecar.md` for the
// architecture this is one piece of, and
// `plans/12-feature-expansion-brainstorm.md` §1 for the decision record.
//
// ============================================================================
// WHAT THIS DOES NOT DO
// ============================================================================
//
// It does not create a `provider_models` row or a `routing_policies` row, and
// it does not talk to the sidecar or to Anthropic at all. The row this writes
// is storage only: nothing in Moira's execution path resolves it today, and
// `RuntimeFactory`'s `ProviderType::Anthropic` arm still gates on
// `require_credential_type(..., &[CredentialType::ApiKey])`
// (`src/orchestration/runtime_factory.rs:112`), so an `oauth2` credential on
// that provider type fails closed with a config error if anything ever tried
// to resolve it — it does not silently misroute. The row exists so the two
// future consumers named in the plan (the `oauth-token-refresh` worker,
// blocked on issue #90, and a possible native-runner execution backend) have
// something to read once they exist.
//
// The sidecar itself is registered as an ordinary `open_ai_compatible`
// provider through the EXISTING "Connect a local endpoint" shortcut
// (`lib/llm-settings.ts`) — that is a separate provider row with its own
// (placeholder) credential, untouched by this module.
//
// ============================================================================
// WHY A DEDICATED PROVIDER ROW, MATCHED BY DISPLAY NAME
// ============================================================================
//
// `plans/12-feature-expansion-brainstorm.md` §1 is explicit that Anthropic
// needs no new `provider_type`: it stays `"anthropic"`, discriminated by
// `credential_type` (`oauth2` here vs `api_key` for a direct, sanctioned
// Anthropic API-key provider some deployments may already have). Reusing
// WHICHEVER `anthropic` provider happens to already exist would risk attaching
// a subscription token to a provider row an operator set up for something
// else entirely. Anthropic provider rows carry no `base_url` to disambiguate
// by (unlike the vLLM shortcut's `base_url` match), so this module matches on
// a fixed, non-operator-editable display name instead — the same "list, then
// match, then create-if-absent" shape `runConnectChain` uses, applied to one
// row instead of four.
//
// ============================================================================
// WHY ROTATE RATHER THAN REUSE-AND-LEAVE-ALONE
// ============================================================================
//
// The vLLM shortcut's credential step reuses an existing row untouched — the
// value never changes, so there is nothing to update. A subscription token is
// different: `claude setup-token` can be re-run to produce a fresh one (the
// old one may have been revoked, or the operator is rotating it on purpose),
// and the whole point of this screen is to let that happen without leaving a
// stale row behind or creating a second one Moira's schema does not forbid.
// So an existing `oauth2` row on the matched provider is ROTATED in place
// (`POST .../rotate`, keeping the row id and its version bumped), never
// duplicated.

import "server-only";

import { CONSOLE_MESSAGE_KEYS } from "./i18n/keys";
import { ifMatchFor, oauth2CredentialSecret, type MoiraClient } from "./moira-client";
import type { ProviderRecord } from "./types";

/** Same rationale as `lib/llm-settings.ts`'s `LIST_PAGE_LIMIT`. */
export const LIST_PAGE_LIMIT = 200;

/**
 * Anthropic needs no new `provider_type` — see this module's header. Exported
 * so the BFF route and its tests can refer to the same constant rather than
 * repeating the string.
 */
export const CLAUDE_SUBSCRIPTION_PROVIDER_TYPE = "anthropic" as const;

/**
 * A fixed, non-operator-editable name. It is what this module matches an
 * existing row by, standing in for the `base_url` the vLLM shortcut matches on
 * — Anthropic provider rows carry none.
 */
export const CLAUDE_SUBSCRIPTION_PROVIDER_DISPLAY_NAME = "Claude subscription (sidecar)" as const;

/** Purely descriptive — never matched on, only sent when creating a row. */
export const CLAUDE_SUBSCRIPTION_CREDENTIAL_DISPLAY_NAME = "Claude subscription token" as const;

/** The longest token this console will accept, before it ever reaches Moira. */
export const MAX_SUBSCRIPTION_TOKEN_LENGTH = 4096;

/* -------------------------------------------------------------------------- */
/* Narrowing the pasted token                                                 */
/* -------------------------------------------------------------------------- */

export type SubscriptionTokenResolution =
  | { readonly ok: true; readonly token: string }
  | { readonly ok: false; readonly messageKey: string };

/**
 * Matches a C0 control byte or DEL. Built from character codes rather than
 * written as a bracket-escape regex literal, so the pattern is legible instead
 * of embedding a raw control byte in this source file the way one earlier
 * draft of this function did — see `lib/setup-flow.ts`'s own documented NUL
 * byte for what that costs a text-based scanner (`i18n-catalog-coverage.test.ts`
 * names it: a NUL byte makes `grep`/`rg` classify a file as binary and skip it
 * silently).
 */
const CLAUDE_SUBSCRIPTION_CONTROL_CHARACTER_PATTERN = new RegExp(
  `[${String.fromCharCode(0)}-${String.fromCharCode(0x1f)}${String.fromCharCode(0x7f)}]`,
);

/**
 * Trim, bound, and refuse a control character — the same three refusals
 * `lib/llm-settings.ts`'s `narrowModelKey` applies to a model id, applied here
 * to a pasted token. A token carrying a control character (most likely a
 * newline from a copy that grabbed more than one line) would travel into an
 * encrypted column and every subsequent `Authorization` header built from it;
 * there is no legitimate one.
 */
export function resolveSubscriptionToken(raw: unknown): SubscriptionTokenResolution {
  if (typeof raw !== "string" || raw.trim() === "") {
    return { ok: false, messageKey: CONSOLE_MESSAGE_KEYS.claude_subscription_token_required };
  }
  const trimmed = raw.trim();
  if (trimmed.length > MAX_SUBSCRIPTION_TOKEN_LENGTH) {
    return { ok: false, messageKey: CONSOLE_MESSAGE_KEYS.claude_subscription_token_too_long };
  }
  if (CLAUDE_SUBSCRIPTION_CONTROL_CHARACTER_PATTERN.test(trimmed)) {
    return { ok: false, messageKey: CONSOLE_MESSAGE_KEYS.claude_subscription_token_invalid };
  }
  return { ok: true, token: trimmed };
}

/* -------------------------------------------------------------------------- */
/* The chain                                                                  */
/* -------------------------------------------------------------------------- */

export type ClaudeSubscriptionOutcome = "created" | "rotated";

export interface ClaudeSubscriptionResult {
  readonly providerId: string;
  readonly credentialId: string;
  readonly outcome: ClaudeSubscriptionOutcome;
}

/**
 * A chain step failed in a way that is not a Moira refusal — today, only the
 * "list was truncated and no match was found" case. Kept separate from
 * `MoiraRequestError` (which `withConsoleSession` already renders keyed) the
 * same way `LlmProvisioningError` is in `lib/llm-settings.ts`.
 */
export class ClaudeSubscriptionError extends Error {
  readonly messageKey: string;

  constructor(messageKey: string) {
    super(`claude subscription connect failed: ${messageKey}`);
    this.name = "ClaudeSubscriptionError";
    this.messageKey = messageKey;
  }
}

export function isClaudeSubscriptionError(value: unknown): value is ClaudeSubscriptionError {
  return value instanceof ClaudeSubscriptionError;
}

/**
 * Find one row on a single page, refusing when the page was truncated and no
 * match was found — "not on this page" is not "does not exist"; see
 * `lib/llm-settings.ts`'s `findOnPage` for the fuller rationale. A local copy
 * rather than a shared import: that function's signature carries
 * `llm-settings.ts`'s own `ConnectStepName`/`ConnectState` trace shape, which
 * this simpler, two-step chain has no use for.
 */
function findFirstOnPage<T>(
  page: { readonly data: readonly T[]; readonly pagination: { readonly has_more: boolean } },
  match: (row: T) => boolean,
): T | null {
  const hit = page.data.find(match);
  if (hit !== undefined) return hit;
  if (page.pagination.has_more) {
    throw new ClaudeSubscriptionError(CONSOLE_MESSAGE_KEYS.claude_subscription_list_truncated);
  }
  return null;
}

export interface ConnectClaudeSubscriptionOptions {
  /** Already resolved by `resolveSubscriptionToken`. */
  readonly accessToken: string;
}

/**
 * Store `options.accessToken` as an `oauth2` provider credential, through
 * Moira's existing credential-create (and, on a second run, rotate) endpoints.
 *
 * Two steps, both reuse-first:
 *
 *   1. the dedicated `anthropic` provider row (find by display name, create if
 *      absent, repair to `active` if found disabled);
 *   2. the `oauth2` credential on it (rotate in place if one already exists,
 *      create if not).
 */
export async function connectClaudeSubscription(
  client: MoiraClient,
  options: ConnectClaudeSubscriptionOptions,
): Promise<ClaudeSubscriptionResult> {
  /* --- 1. the dedicated Anthropic provider row ---------------------------- */
  const providerPage = await client.listProviders({ limit: LIST_PAGE_LIMIT });
  let provider: ProviderRecord | null = findFirstOnPage(
    providerPage,
    (row) =>
      row.provider_type === CLAUDE_SUBSCRIPTION_PROVIDER_TYPE &&
      row.display_name === CLAUDE_SUBSCRIPTION_PROVIDER_DISPLAY_NAME &&
      row.status !== "deleted",
  );

  if (provider === null) {
    provider = await client.createProvider(
      {
        provider_type: CLAUDE_SUBSCRIPTION_PROVIDER_TYPE,
        display_name: CLAUDE_SUBSCRIPTION_PROVIDER_DISPLAY_NAME,
      },
      // Derived from the fixed display name, so a double-submit replays
      // rather than landing a second row with nothing to disambiguate it by.
      { idempotencyKey: `claude-subscription-provider:${CLAUDE_SUBSCRIPTION_PROVIDER_DISPLAY_NAME}` },
    );
  } else if (provider.status !== "active") {
    // Repaired, not merely reused: routing (were this ever wired to routing)
    // resolves only `active` rows, and a disabled provider silently holding a
    // freshly-rotated token would misreport readiness.
    provider = await client.enableProvider(provider.id, ifMatchFor(provider));
  }

  /* --- 2. the oauth2 credential, created or rotated in place -------------- */
  const credentialPage = await client.listProviderCredentials({
    providerId: provider.id,
    limit: LIST_PAGE_LIMIT,
  });
  const existingCredential = findFirstOnPage(
    credentialPage,
    (row) => row.credential_type === "oauth2" && row.status !== "deleted",
  );

  if (existingCredential === null) {
    const created = await client.createProviderCredential(
      {
        // Resolved from the provider record THIS function created or found —
        // never from a request body.
        provider_id: provider.id,
        credential_type: "oauth2",
        scope: { type: "global" },
        secret: oauth2CredentialSecret(options.accessToken),
        display_name: CLAUDE_SUBSCRIPTION_CREDENTIAL_DISPLAY_NAME,
      },
      { idempotencyKey: `claude-subscription-credential:${provider.id}:${crypto.randomUUID()}` },
    );
    return { providerId: provider.id, credentialId: created.id, outcome: "created" };
  }

  const rotated = await client.rotateProviderCredential(
    existingCredential.id,
    { secret: oauth2CredentialSecret(options.accessToken) },
    ifMatchFor(existingCredential),
  );
  const active =
    rotated.status === "active"
      ? rotated
      : await client.enableProviderCredential(rotated.id, ifMatchFor(rotated));
  return { providerId: provider.id, credentialId: active.id, outcome: "rotated" };
}
