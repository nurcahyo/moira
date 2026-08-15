// @server-only
//
// "Connect a Claude credential" — three acquisition modes, two credential
// shapes, ONE storage chain each:
//
//   Mode A (CLI-assisted)  `lib/claude-cli.ts` mints a token by running the
//                          locally installed `claude` CLI, then this module
//                          stores it exactly like Mode C.
//   Mode B (API key)       an official `sk-ant-…` Anthropic Console API key,
//                          pasted, stored as `api_key` on ITS OWN dedicated
//                          provider row — `connectClaudeApiKey`.
//   Mode C (paste, the     paste the output of `claude setup-token` by hand.
//           original shape)
//
// Modes A and C store the SAME credential shape (`oauth2`, on the dedicated
// "Claude subscription (sidecar)" provider row) through the SAME function,
// `connectClaudeSubscription` — only how the token is OBTAINED differs. Mode
// B is a different credential_type on a DIFFERENT dedicated provider row,
// because an `api_key` credential is the one shape `RuntimeFactory`'s
// `ProviderType::Anthropic` arm actually accepts today
// (`require_credential_type(..., &[CredentialType::ApiKey])`,
// `src/orchestration/runtime_factory.rs:112`) — conflating it with the
// storage-only subscription row would make ONE row simultaneously "never
// resolved by execution" (true of the oauth2 row, see below) and "the thing
// a completion actually authenticates with" (true of an api_key row),
// depending on which credential happened to be active on it.
//
// All three modes go through Moira's EXISTING credential endpoints — no new
// Moira HTTP endpoint for any of them. See `docs/claude-subscription-sidecar.md`
// for the architecture and `plans/12-feature-expansion-brainstorm.md` §1 for
// the decision record, including why no browser OAuth flow exists here:
// Anthropic publishes no third-party OAuth client id, so driving one against
// `claude.ai/oauth/authorize` from this console would mean either
// impersonating Claude Code's own client id (blocked since ~Jan 2026) or
// inventing a client id that was never issued. Mode A sidesteps the whole
// question: the real, already-authenticated `claude` CLI performs the
// authentication, this console only asks it for a token.
//
// ============================================================================
// WHAT THE oauth2 CHAIN (Modes A, C) DOES NOT DO
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
import type { ClaudeCredentialStatusView } from "./llm-view";
import { apiKeyCredentialSecret, ifMatchFor, oauth2CredentialSecret, type MoiraClient } from "./moira-client";
import type {
  ApiKeyCredentialSecret,
  ConsoleApiKeyCredentialCreateRequest,
  ConsoleOAuth2CredentialCreateRequest,
  OAuth2CredentialSecret,
} from "./moira-credential-types";
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
 * Step 1 of every mode's chain, factored out because Modes A/C (the oauth2
 * row) and Mode B (the api_key row) both need it, against two DIFFERENT
 * display names: find-by-display-name, create if absent, repair to `active`
 * if found disabled. See `connectClaudeSubscription`'s original header
 * (preserved on the exported functions below) for why the match is by
 * `provider_type` AND `display_name` rather than reusing whatever
 * `anthropic` row already exists.
 */
async function findOrCreateAnthropicProvider(
  client: MoiraClient,
  displayName: string,
): Promise<ProviderRecord> {
  const providerPage = await client.listProviders({ limit: LIST_PAGE_LIMIT });
  let provider: ProviderRecord | null = findFirstOnPage(
    providerPage,
    (row) =>
      row.provider_type === CLAUDE_SUBSCRIPTION_PROVIDER_TYPE &&
      row.display_name === displayName &&
      row.status !== "deleted",
  );

  if (provider === null) {
    provider = await client.createProvider(
      { provider_type: CLAUDE_SUBSCRIPTION_PROVIDER_TYPE, display_name: displayName },
      // Derived from the fixed display name, so a double-submit replays
      // rather than landing a second row with nothing to disambiguate it by.
      { idempotencyKey: `anthropic-provider:${displayName}` },
    );
  } else if (provider.status !== "active") {
    // Repaired, not merely reused: routing resolves only `active` rows, and a
    // disabled provider silently holding a freshly-rotated credential would
    // misreport readiness.
    provider = await client.enableProvider(provider.id, ifMatchFor(provider));
  }
  return provider;
}

/**
 * Step 2 of every mode's chain: the credential on `provider`, rotated in
 * place if one of `credentialType` already exists there, created if not.
 * Shared by Modes A/C (`oauth2`) and Mode B (`api_key`) — the two credential
 * shapes differ, but "reuse-first, rotate rather than duplicate" does not.
 *
 * `createBody` is built by the CALLER, not by this function, and on purpose:
 * each caller constructs it with a LITERAL `credential_type` (`"oauth2"` or
 * `"api_key"`, never the union), which is what lets TypeScript pick the
 * matching arm of `ConsoleApiKeyCredentialCreateRequest |
 * ConsoleOAuth2CredentialCreateRequest` by contextual typing alone — no cast,
 * and no risk of a cast papering over the two ever coming apart.
 */
async function connectAnthropicCredential(
  client: MoiraClient,
  provider: ProviderRecord,
  args: {
    readonly credentialType: "oauth2" | "api_key";
    readonly createBody: ConsoleApiKeyCredentialCreateRequest | ConsoleOAuth2CredentialCreateRequest;
    readonly secret: OAuth2CredentialSecret | ApiKeyCredentialSecret;
    readonly idempotencyNamespace: string;
  },
): Promise<ClaudeSubscriptionResult> {
  const credentialPage = await client.listProviderCredentials({
    providerId: provider.id,
    limit: LIST_PAGE_LIMIT,
  });
  const existingCredential = findFirstOnPage(
    credentialPage,
    (row) => row.credential_type === args.credentialType && row.status !== "deleted",
  );

  if (existingCredential === null) {
    const created = await client.createProviderCredential(args.createBody, {
      idempotencyKey: `${args.idempotencyNamespace}-credential:${provider.id}:${crypto.randomUUID()}`,
    });
    return { providerId: provider.id, credentialId: created.id, outcome: "created" };
  }

  const rotated = await client.rotateProviderCredential(
    existingCredential.id,
    { secret: args.secret },
    ifMatchFor(existingCredential),
  );
  const active =
    rotated.status === "active"
      ? rotated
      : await client.enableProviderCredential(rotated.id, ifMatchFor(rotated));
  return { providerId: provider.id, credentialId: active.id, outcome: "rotated" };
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
 *
 * Used by BOTH Mode A (CLI-minted) and Mode C (pasted) — the two modes differ
 * only in how `options.accessToken` was obtained, never in how it is stored.
 * That is also what makes "re-acquire" and "rotate" the same action from this
 * function's point of view: a second call with a fresh token rotates the
 * existing row in place rather than creating a second one.
 */
export async function connectClaudeSubscription(
  client: MoiraClient,
  options: ConnectClaudeSubscriptionOptions,
): Promise<ClaudeSubscriptionResult> {
  const provider = await findOrCreateAnthropicProvider(client, CLAUDE_SUBSCRIPTION_PROVIDER_DISPLAY_NAME);
  const secret = oauth2CredentialSecret(options.accessToken);
  return connectAnthropicCredential(client, provider, {
    credentialType: "oauth2",
    secret,
    createBody: {
      // Resolved from the provider record THIS chain created or found —
      // never from a request body.
      provider_id: provider.id,
      credential_type: "oauth2",
      scope: { type: "global" },
      secret,
      display_name: CLAUDE_SUBSCRIPTION_CREDENTIAL_DISPLAY_NAME,
    },
    idempotencyNamespace: "claude-subscription",
  });
}

/* -------------------------------------------------------------------------- */
/* Mode B: an official Anthropic Console API key, stored as `api_key`         */
/* -------------------------------------------------------------------------- */

/**
 * A DIFFERENT dedicated provider row from the subscription one — see this
 * file's header for why the two credential shapes may not share a row.
 */
export const CLAUDE_API_KEY_PROVIDER_DISPLAY_NAME = "Anthropic (API key)" as const;

/** Purely descriptive — never matched on, only sent when creating a row. */
export const CLAUDE_API_KEY_CREDENTIAL_DISPLAY_NAME = "Anthropic API key" as const;

/**
 * Generous: real Anthropic API keys are well under this, but the bound
 * exists to stop an obviously-wrong paste (a whole curl command, a JSON
 * blob) from reaching Moira rather than to pin an exact key length Anthropic
 * has never published as stable.
 */
export const MAX_API_KEY_LENGTH = 512;

/** Every Anthropic Console API key observed in the wild carries this prefix. */
const ANTHROPIC_API_KEY_PREFIX = "sk-ant-";

export type ApiKeyResolution =
  | { readonly ok: true; readonly apiKey: string }
  | { readonly ok: false; readonly messageKey: string };

/**
 * Trim, bound, refuse a control character, and check the `sk-ant-` shape —
 * the same three refusals `resolveSubscriptionToken` applies to a pasted
 * subscription token, plus one: this field's value is meant to authenticate
 * directly against the Messages API, so a value that plainly is not an
 * Anthropic key (an OpenAI `sk-proj-…` key pasted into the wrong field, most
 * plausibly) is refused here rather than stored and left to fail later as an
 * opaque `401` from Moira's own execution path.
 */
export function resolveAnthropicApiKey(raw: unknown): ApiKeyResolution {
  if (typeof raw !== "string" || raw.trim() === "") {
    return { ok: false, messageKey: CONSOLE_MESSAGE_KEYS.claude_api_key_required };
  }
  const trimmed = raw.trim();
  if (trimmed.length > MAX_API_KEY_LENGTH) {
    return { ok: false, messageKey: CONSOLE_MESSAGE_KEYS.claude_api_key_too_long };
  }
  if (CLAUDE_SUBSCRIPTION_CONTROL_CHARACTER_PATTERN.test(trimmed)) {
    return { ok: false, messageKey: CONSOLE_MESSAGE_KEYS.claude_api_key_invalid };
  }
  if (!trimmed.startsWith(ANTHROPIC_API_KEY_PREFIX)) {
    return { ok: false, messageKey: CONSOLE_MESSAGE_KEYS.claude_api_key_wrong_shape };
  }
  return { ok: true, apiKey: trimmed };
}

export interface ConnectClaudeApiKeyOptions {
  /** Already resolved by `resolveAnthropicApiKey`. */
  readonly apiKey: string;
}

/**
 * Store `options.apiKey` as an `api_key` provider credential on the
 * dedicated `"Anthropic (API key)"` row — the same reuse-first,
 * rotate-in-place shape as `connectClaudeSubscription`, against a different
 * row and a different credential type.
 */
export async function connectClaudeApiKey(
  client: MoiraClient,
  options: ConnectClaudeApiKeyOptions,
): Promise<ClaudeSubscriptionResult> {
  const provider = await findOrCreateAnthropicProvider(client, CLAUDE_API_KEY_PROVIDER_DISPLAY_NAME);
  const secret = apiKeyCredentialSecret(options.apiKey);
  return connectAnthropicCredential(client, provider, {
    credentialType: "api_key",
    secret,
    createBody: {
      provider_id: provider.id,
      credential_type: "api_key",
      scope: { type: "global" },
      secret,
      display_name: CLAUDE_API_KEY_CREDENTIAL_DISPLAY_NAME,
    },
    idempotencyNamespace: "claude-api-key",
  });
}

/* -------------------------------------------------------------------------- */
/* Read-only status — "is a credential connected, and until when"             */
/* -------------------------------------------------------------------------- */

/**
 * A READ-ONLY lookup, never a write. Deliberately returns a status rather
 * than throwing on "nothing connected yet" or "list truncated" — both are
 * normal states for a screen rendered before any operator has connected
 * anything, and a page-load status lookup must not turn a normal state into
 * a 500. Never returns `masked_secret` or `secret_fingerprint`: those stay
 * server-side by the same rule `lib/moira-credential-types.ts`'s
 * `CredentialRecord` header states for every other console screen — the
 * `kind`/`status`/`expiresAt` fields answer "is there a row, and is it
 * usable", which is what this screen needs, without carrying a value that
 * could later leak through a render, a log, or a screenshot.
 */
async function loadAnthropicCredentialStatus(
  client: MoiraClient,
  providerDisplayName: string,
  credentialType: "oauth2" | "api_key",
): Promise<ClaudeCredentialStatusView> {
  const providerPage = await client.listProviders({ limit: LIST_PAGE_LIMIT });
  const provider = providerPage.data.find(
    (row) =>
      row.provider_type === CLAUDE_SUBSCRIPTION_PROVIDER_TYPE &&
      row.display_name === providerDisplayName &&
      row.status !== "deleted",
  );
  if (provider === undefined) {
    return providerPage.pagination.has_more
      ? { kind: "unknown", status: null, expiresAt: null }
      : { kind: "not_connected", status: null, expiresAt: null };
  }

  const credentialPage = await client.listProviderCredentials({
    providerId: provider.id,
    limit: LIST_PAGE_LIMIT,
  });
  const credential = credentialPage.data.find(
    (row) => row.credential_type === credentialType && row.status !== "deleted",
  );
  if (credential === undefined) {
    return credentialPage.pagination.has_more
      ? { kind: "unknown", status: null, expiresAt: null }
      : { kind: "not_connected", status: null, expiresAt: null };
  }

  return { kind: "connected", status: credential.status, expiresAt: credential.expires_at ?? null };
}

/** Status of the Mode A/C (oauth2, "Claude subscription (sidecar)") row. */
export function loadClaudeSubscriptionStatus(client: MoiraClient): Promise<ClaudeCredentialStatusView> {
  return loadAnthropicCredentialStatus(
    client,
    CLAUDE_SUBSCRIPTION_PROVIDER_DISPLAY_NAME,
    "oauth2",
  );
}

/** Status of the Mode B (api_key, "Anthropic (API key)") row. */
export function loadClaudeApiKeyStatus(client: MoiraClient): Promise<ClaudeCredentialStatusView> {
  return loadAnthropicCredentialStatus(client, CLAUDE_API_KEY_PROVIDER_DISPLAY_NAME, "api_key");
}
