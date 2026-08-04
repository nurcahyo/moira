// @server-only
//
// LLM runtime configuration: reading it, and writing it in the ONE order that
// works.
//
// ============================================================================
// THE PROVISIONING ORDER IS NOT A MATTER OF TASTE
// ============================================================================
//
// `scripts/seed-local.sh` is the only proven-working sequence in this repository
// and its header names the two failures a wrong order produces. Both are
// reproducible on a freshly migrated database and neither error message points
// at the missing row:
//
//   404 credential_not_found  "no eligible provider credential" — routing
//       resolves a credential ROW before it builds any request, so a provider
//       with no credential fails even when the endpoint needs no key at all. A
//       keyless local vLLM still needs the row; the value is never sent.
//
//   403 route_override_forbidden — sending `"route": "general"` in a completion
//       body is an OVERRIDE and needs its own authorisation. Omit it and default
//       routing picks the same route. Nothing in this module ever sends a route
//       as an override; it binds a POLICY to the seeded route instead, which is
//       what makes default routing choose it.
//
// So: provider -> provider model -> provider credential -> (enable) -> routing
// policy bound to the existing `general` route.
//
// ============================================================================
// WHERE THIS DEPARTS FROM THE SEED SCRIPT, AND WHY
// ============================================================================
//
// The seed script has no `enable` step, because `POST /api/v1/admin/providers`
// creates an ACTIVE row — `ProviderCreateRequest` has no `enabled` field at all.
// Issue #74 lists `enableProvider` as step 4. Both are right about different
// states, so the step is CONDITIONAL: it runs only when the provider's status is
// not already active. On a fresh install it is skipped and the sequence is
// byte-for-byte the seed script's; on a re-run after an operator disabled a
// provider it is the step that brings it back. A wrong endpoint is the most
// likely first mistake on this screen, disable is how it gets undone, and
// re-connecting afterwards has to work.
//
// ============================================================================
// EVERY STEP IS REUSE-FIRST, AND TRUNCATION IS REFUSED RATHER THAN GUESSED
// ============================================================================
//
// `POST /routing-policies` documents NO 409 and neither does `POST /providers`
// for a duplicate `base_url`, so nothing server-side stops a second identical
// row. Deduplication is therefore this module's job, done by listing first — and
// "not on this page" is not the same as "does not exist". `findOnPage` refuses
// when the list is truncated and no match was found, exactly as the seed
// script's `find_by` does, because guessing wrong creates the duplicate the
// dedupe existed to prevent.

import "server-only";

import { CONSOLE_MESSAGE_KEYS } from "./i18n/keys";
import {
  GENERAL_ROUTE_KEY,
  type LlmConnectStepView,
  type LlmKeyRowView,
  type LlmModelView,
  type LlmPolicyView,
  type LlmProviderView,
  type LlmSettingsView,
} from "./llm-view";
import { apiKeyCredentialSecret, ifMatchFor, type MoiraClient } from "./moira-client";
import type {
  ListResponse,
  ProviderModelRecord,
  ProviderRecord,
  RouteDefinitionRecord,
  RoutingPolicyRecord,
} from "./types";

/* -------------------------------------------------------------------------- */
/* Bounds                                                                     */
/* -------------------------------------------------------------------------- */

/**
 * `PageQuery::limit` is `self.limit.unwrap_or(50).clamp(1, 200)`. Ask for the
 * clamp maximum so one page covers any plausible deployment, and let
 * `findOnPage` refuse rather than guess when it does not.
 */
export const LIST_PAGE_LIMIT = 200;

/** How long the BFF will wait for an operator's own endpoint to answer. */
export const DISCOVERY_TIMEOUT_MS = 5_000;

/**
 * How much of that answer it will read.
 *
 * A hostile or merely broken endpoint that streams forever would otherwise pin a
 * request handler until the process dies. 256 KiB is far more than any
 * `/v1/models` document and small enough to be irrelevant to memory.
 */
export const DISCOVERY_MAX_BYTES = 256 * 1024;

/** The most model ids a single discovery may offer. */
export const DISCOVERY_MAX_MODELS = 200;

/** The longest model id this console will accept from an endpoint. */
export const DISCOVERY_MAX_MODEL_KEY_LENGTH = 256;

/**
 * The capability set every model row this console creates declares.
 *
 * EXPLICIT, ALWAYS. `capabilities` is optional in the schema and an omitted
 * value is stored as SQL `null`, which matches no capability filter — the first
 * completion then fails `no_eligible_model`, an error naming neither the model
 * nor the missing column. `assertProviderModelCreateIsSafe` refuses the omission;
 * this is the value it is refused in favour of.
 */
export const DEFAULT_MODEL_CAPABILITIES = { text: true, streaming: true, tools: true } as const;

/**
 * The stored value for a keyless endpoint's required credential row.
 *
 * Generated server-side, never entered by an operator and never rendered. A
 * local vLLM ignores `Authorization` entirely, so this is only ever read by the
 * row-existence check that decides `credential_not_found`. It must be non-empty:
 * `apiKeyCredentialSecret` refuses an empty string for exactly that reason.
 */
export function generatedPlaceholderKey(): string {
  return `moira-console-placeholder-${crypto.randomUUID()}`;
}

/* -------------------------------------------------------------------------- */
/* Base URL handling                                                          */
/* -------------------------------------------------------------------------- */

export type BaseUrlResolution =
  | { readonly ok: true; readonly baseUrl: string }
  | { readonly ok: false; readonly messageKey: string };

/**
 * Canonicalise an operator-typed address into an OpenAI-compatible base URL.
 *
 * ============================================================================
 * THE `/v1` SUFFIX IS ADDED HERE OR IT IS ADDED NOWHERE
 * ============================================================================
 *
 * `scripts/seed-local.sh` defaults to `http://127.0.0.1:8000/v1` and probes
 * `$BASE_URL/models`, i.e. the base a provider row stores ALREADY INCLUDES
 * `/v1`. An operator typing `https://local-llm.motrait.com` means the same
 * server; a provider row created from the bare origin sends completions to
 * `/chat/completions` instead of `/v1/chat/completions` and 404s at request
 * time, long after this screen said it worked.
 *
 * So the suffix is appended when absent, and — the part that matters — the
 * canonical value is what BOTH the discovery probe and the stored provider row
 * use. "Discovery worked" and "the provider works" must not be two different
 * URLs.
 *
 * ============================================================================
 * WHAT IS REFUSED, AND WHY EACH REFUSAL IS NOT MERELY TIDINESS
 * ============================================================================
 *
 * This URL is fetched BY THE SERVER, on an authenticated operator's say-so. That
 * is the feature — the browser must never be asked to reach the operator's own
 * network — and it is also the reason the input is narrowed before it is used:
 *
 *   scheme      `http:`/`https:` only. Without this, `file:`, `data:` and
 *               whatever else the runtime's fetch supports become reachable.
 *   credentials a URL carrying `user:password@` would put a secret into a
 *               provider row, into every subsequent list response, and onto this
 *               screen.
 *   query/hash  dropped. They are not part of a base URL and would be carried
 *               into every completion request built from it.
 */
export function canonicalOpenAiBaseUrl(raw: unknown): BaseUrlResolution {
  if (typeof raw !== "string" || raw.trim() === "") {
    return { ok: false, messageKey: CONSOLE_MESSAGE_KEYS.llm_base_url_required };
  }
  let parsed: URL;
  try {
    parsed = new URL(raw.trim());
  } catch {
    return { ok: false, messageKey: CONSOLE_MESSAGE_KEYS.llm_base_url_invalid };
  }
  if (parsed.protocol !== "https:" && parsed.protocol !== "http:") {
    return { ok: false, messageKey: CONSOLE_MESSAGE_KEYS.llm_base_url_scheme_unsupported };
  }
  if (parsed.username !== "" || parsed.password !== "") {
    return { ok: false, messageKey: CONSOLE_MESSAGE_KEYS.llm_base_url_userinfo_rejected };
  }
  const path = parsed.pathname.replace(/\/+$/, "");
  const withVersion = path === "" || !/\/v\d+$/.test(path) ? `${path}/v1` : path;
  return { ok: true, baseUrl: `${parsed.origin}${withVersion}` };
}

/* -------------------------------------------------------------------------- */
/* Discovery — the outbound call, made from the BFF and from nowhere else     */
/* -------------------------------------------------------------------------- */

export type DiscoveryOutcome =
  | { readonly ok: true; readonly baseUrl: string; readonly models: readonly string[] }
  | { readonly ok: false; readonly messageKey: string };

/**
 * Read at most `limit` bytes of a response body.
 *
 * Streamed rather than `await response.text()`, because `text()` is unbounded:
 * an endpoint that answers with an endless body would be read until the process
 * ran out of memory. The reader is cancelled as soon as the cap is passed, which
 * also closes the socket.
 */
async function readBounded(response: Response, limit: number): Promise<string | null> {
  const declared = response.headers.get("content-length");
  if (declared !== null && Number(declared) > limit) return null;

  const body = response.body;
  if (body === null) {
    const text = await response.text();
    return text.length > limit ? null : text;
  }

  const reader = body.getReader();
  const chunks: Uint8Array[] = [];
  let total = 0;
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      if (value === undefined) continue;
      total += value.byteLength;
      if (total > limit) {
        await reader.cancel();
        return null;
      }
      chunks.push(value);
    }
  } finally {
    reader.releaseLock();
  }

  const joined = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) {
    joined.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return new TextDecoder().decode(joined);
}

/**
 * Validate an OpenAI-compatible `GET /v1/models` body.
 *
 * NOTHING IS TRUSTED. The endpoint is on the operator's network and this console
 * has no relationship with it beyond having been given its address, so the
 * response is treated as arbitrary input: the envelope must be an object, `data`
 * must be an array, and every entry must be an object whose `id` is a non-empty,
 * bounded, single-line string. Anything else yields `null` and the caller reports
 * a keyed failure rather than rendering whatever arrived.
 *
 * Exported so the validation can be tested without a socket — and so that
 * deleting it fails a test rather than silently widening what reaches the screen.
 */
export function modelKeysFromDiscoveryBody(body: unknown): readonly string[] | null {
  if (typeof body !== "object" || body === null || Array.isArray(body)) return null;
  const data = (body as { data?: unknown }).data;
  if (!Array.isArray(data)) return null;

  const keys: string[] = [];
  for (const entry of data) {
    if (typeof entry !== "object" || entry === null || Array.isArray(entry)) return null;
    const id = (entry as { id?: unknown }).id;
    if (typeof id !== "string") return null;
    const trimmed = id.trim();
    if (trimmed === "") return null;
    if (trimmed.length > DISCOVERY_MAX_MODEL_KEY_LENGTH) return null;
    // A control character in a model id would travel into a provider row, a URL
    // and a rendered table. There is no legitimate one. Written as escapes
    // rather than as raw bytes so the range stays readable in a diff.
    if (/[\u0000-\u001f\u007f]/.test(trimmed)) return null;
    if (!keys.includes(trimmed)) keys.push(trimmed);
    if (keys.length > DISCOVERY_MAX_MODELS) return null;
  }
  return keys;
}

/**
 * Ask an OpenAI-compatible endpoint what it serves.
 *
 * ============================================================================
 * THIS CALL LEAVES THE BFF, NEVER THE BROWSER
 * ============================================================================
 *
 * The endpoint lives on the operator's own network. A browser fetch would need
 * that network to be reachable from wherever the operator happens to be sitting,
 * would need CORS the endpoint has no reason to send, and would move the decision
 * about which host to contact into a place this console does not control. So the
 * request is made here, from the same process that holds the Moira credential,
 * behind `withConsoleSession`.
 *
 * ============================================================================
 * AN UNREACHABLE ENDPOINT IS AN ORDINARY MESSAGE, NOT A CRASH
 * ============================================================================
 *
 * A laptop with the tunnel down is the normal case, not an exceptional one. Every
 * failure below — DNS, TLS, timeout, an HTML error page from a proxy, a body too
 * large to read, a shape that is not `{data: [{id}]}` — comes back as
 * `{ ok: false, messageKey }` and the page still renders.
 */
export async function discoverModels(
  rawBaseUrl: unknown,
  options: { readonly fetchImpl?: typeof fetch; readonly timeoutMs?: number } = {},
): Promise<DiscoveryOutcome> {
  const resolved = canonicalOpenAiBaseUrl(rawBaseUrl);
  if (!resolved.ok) return resolved;

  const send = options.fetchImpl ?? globalThis.fetch;
  const timeoutMs = options.timeoutMs ?? DISCOVERY_TIMEOUT_MS;
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);

  let response: Response;
  try {
    response = await send(`${resolved.baseUrl}/models`, {
      method: "GET",
      headers: { accept: "application/json" },
      signal: controller.signal,
      // No credential of any kind. This console has none for the operator's own
      // endpoint, and `redirect: "error"` keeps a redirect from moving the
      // request to a host the operator never named.
      redirect: "error",
    });
  } catch {
    return { ok: false, messageKey: CONSOLE_MESSAGE_KEYS.llm_discovery_unreachable };
  } finally {
    clearTimeout(timer);
  }

  if (!response.ok) {
    return { ok: false, messageKey: CONSOLE_MESSAGE_KEYS.llm_discovery_refused };
  }

  const text = await readBounded(response, DISCOVERY_MAX_BYTES);
  if (text === null) {
    return { ok: false, messageKey: CONSOLE_MESSAGE_KEYS.llm_discovery_response_too_large };
  }

  let parsed: unknown;
  try {
    parsed = JSON.parse(text);
  } catch {
    return { ok: false, messageKey: CONSOLE_MESSAGE_KEYS.llm_discovery_invalid_response };
  }

  const models = modelKeysFromDiscoveryBody(parsed);
  if (models === null || models.length === 0) {
    return { ok: false, messageKey: CONSOLE_MESSAGE_KEYS.llm_discovery_invalid_response };
  }
  return { ok: true, baseUrl: resolved.baseUrl, models };
}

/* -------------------------------------------------------------------------- */
/* Listing helpers                                                            */
/* -------------------------------------------------------------------------- */

/**
 * Find one row on a single page, refusing when the page was truncated.
 *
 * The seed script's `find_by`, transcribed: an empty result means "absent,
 * create it", and that reading is only sound when the whole list was seen. If it
 * was not, this throws rather than creating a duplicate that routing then has to
 * choose between with no rule for choosing.
 */
export function findOnPage<T>(
  page: ListResponse<T>,
  match: (row: T) => boolean,
  step: ConnectStepName,
): T | null {
  const hit = page.data.find(match);
  if (hit !== undefined) return hit;
  if (page.pagination.has_more) {
    throw new LlmProvisioningError({
      step,
      messageKey: CONSOLE_MESSAGE_KEYS.llm_list_truncated,
      state: emptyConnectState(),
      trace: [],
    });
  }
  return null;
}

/* -------------------------------------------------------------------------- */
/* Reading the screen                                                         */
/* -------------------------------------------------------------------------- */

function modelView(record: ProviderModelRecord): LlmModelView {
  return {
    id: record.id,
    modelKey: record.model_key,
    status: record.status,
    version: record.version,
    capabilities: record.capabilities,
  };
}

function policyView(
  record: RoutingPolicyRecord,
  routeKeys: ReadonlyMap<string, string>,
): LlmPolicyView {
  return {
    id: record.id,
    routeId: record.route_id,
    routeKey: routeKeys.get(record.route_id) ?? null,
    providerModelId: record.provider_model_id,
    priority: record.priority,
    weight: record.weight,
    status: record.status,
    version: record.version,
  };
}

/**
 * Everything `/settings/llm` renders.
 *
 * The credential list is read with `provider_id` as a SERVER-SIDE QUERY
 * PARAMETER rather than fetched whole and filtered here, so rows belonging to
 * other providers never enter this process — and the projection to
 * `LlmKeyRowView` drops `masked_secret` and `secret_fingerprint` before anything
 * crosses to the browser.
 */
export async function loadLlmSettings(client: MoiraClient): Promise<LlmSettingsView> {
  const [providerPage, routePage, policyPage] = await Promise.all([
    client.listProviders({ limit: LIST_PAGE_LIMIT }),
    client.listRoutes({ limit: LIST_PAGE_LIMIT }),
    client.listRoutingPolicies({ limit: LIST_PAGE_LIMIT }),
  ]);

  const routeKeys = new Map<string, string>(
    routePage.data.map((route: RouteDefinitionRecord) => [route.id, route.route_key]),
  );
  const generalRoute = routePage.data.find((route) => route.route_key === GENERAL_ROUTE_KEY);

  const providers: LlmProviderView[] = [];
  for (const provider of providerPage.data) {
    const [models, keyRows] = await Promise.all([
      client.listProviderModels(provider.id, { limit: LIST_PAGE_LIMIT }),
      client.listProviderCredentials({ providerId: provider.id, limit: LIST_PAGE_LIMIT }),
    ]);
    providers.push({
      id: provider.id,
      displayName: provider.display_name,
      providerType: provider.provider_type,
      baseUrl: provider.base_url ?? null,
      status: provider.status,
      version: provider.version,
      models: models.data.map(modelView),
      keyRows: keyRows.data.map(
        (row): LlmKeyRowView => ({
          id: row.id,
          kind: row.credential_type,
          status: row.status,
          version: row.version,
        }),
      ),
      policies: policyPage.data
        .filter((policy) => policy.provider_id === provider.id)
        .map((policy) => policyView(policy, routeKeys)),
    });
  }

  return { providers, generalRouteId: generalRoute?.id ?? null };
}

/* -------------------------------------------------------------------------- */
/* Writing — the chain                                                        */
/* -------------------------------------------------------------------------- */

export type ConnectStepName =
  | "provider"
  | "provider_model"
  | "provider_credential"
  | "provider_enable"
  | "routing_policy";

/** What has been written so far. Returned on success AND on failure. */
export interface ConnectState {
  readonly providerId: string | null;
  readonly providerModelIds: readonly string[];
  readonly keyRowId: string | null;
  readonly providerEnabled: boolean;
  readonly routingPolicyIds: readonly string[];
}

export function emptyConnectState(): ConnectState {
  return {
    providerId: null,
    providerModelIds: [],
    keyRowId: null,
    providerEnabled: false,
    routingPolicyIds: [],
  };
}

/**
 * A chain that stopped part-way.
 *
 * CARRIES THE STATE. "If step three fails, the operator must be told what was
 * already written" is the whole reason this is not a bare `Error`: a provider and
 * a model exist, the operator cannot see them on a screen that failed to load,
 * and clicking again without that knowledge is how duplicates get created.
 */
export class LlmProvisioningError extends Error {
  readonly step: ConnectStepName;
  readonly messageKey: string;
  readonly state: ConnectState;
  readonly trace: readonly LlmConnectStepView[];

  constructor(options: {
    readonly step: ConnectStepName;
    readonly messageKey: string;
    readonly state: ConnectState;
    readonly trace: readonly LlmConnectStepView[];
  }) {
    super(`llm provisioning failed at ${options.step}`);
    this.name = "LlmProvisioningError";
    this.step = options.step;
    this.messageKey = options.messageKey;
    this.state = options.state;
    this.trace = options.trace;
  }
}

export function isLlmProvisioningError(value: unknown): value is LlmProvisioningError {
  return value instanceof LlmProvisioningError;
}

export interface ConnectChainOptions {
  /** Already canonicalised by `canonicalOpenAiBaseUrl`. */
  readonly baseUrl: string;
  readonly displayName: string;
  readonly modelKeys: readonly string[];
  /** Injected by tests so the placeholder is deterministic. */
  readonly placeholderKey?: string;
}

export interface ConnectChainResult {
  readonly providerId: string;
  readonly state: ConnectState;
  readonly trace: readonly LlmConnectStepView[];
}

/**
 * Run the whole chain, reusing whatever already matches.
 *
 * Every step is idempotent by LOOKUP rather than by an idempotency key alone:
 * the key protects a double-submit of one request, and what this needs to
 * survive is a SECOND CLICK after a partial failure, minutes later, from a
 * different request. Those are different problems and only the lookup solves the
 * second one.
 */
export async function runConnectChain(
  client: MoiraClient,
  options: ConnectChainOptions,
): Promise<ConnectChainResult> {
  const trace: LlmConnectStepView[] = [];
  let state = emptyConnectState();

  const fail = (step: ConnectStepName, messageKey: string): never => {
    throw new LlmProvisioningError({ step, messageKey, state, trace: [...trace] });
  };

  /* --- 1. provider ------------------------------------------------------- */
  let provider: ProviderRecord;
  try {
    const page = await client.listProviders({ limit: LIST_PAGE_LIMIT });
    const existing = findOnPage(
      page,
      (row) => row.base_url === options.baseUrl && row.status !== "deleted",
      "provider",
    );
    if (existing !== null) {
      provider = existing;
      trace.push({ step: "provider", outcome: "reused", detail: existing.id });
    } else {
      provider = await client.createProvider(
        {
          provider_type: "open_ai_compatible",
          display_name: options.displayName,
          base_url: options.baseUrl,
        },
        { idempotencyKey: `llm-provider:${options.baseUrl}` },
      );
      trace.push({ step: "provider", outcome: "created", detail: provider.id });
    }
  } catch (error) {
    if (isLlmProvisioningError(error)) throw error;
    throw rethrowAsStep(error, "provider", state, trace);
  }
  state = { ...state, providerId: provider.id };

  /* --- 2. provider models ------------------------------------------------ */
  const modelIds: string[] = [];
  let firstModelId: string | null = null;
  try {
    const page = await client.listProviderModels(provider.id, { limit: LIST_PAGE_LIMIT });
    for (const modelKey of options.modelKeys) {
      const existing = findOnPage(
        page,
        (row) => row.model_key === modelKey && row.status !== "deleted",
        "provider_model",
      );
      if (existing !== null) {
        modelIds.push(existing.id);
        trace.push({ step: "provider_model", outcome: "reused", detail: modelKey });
        continue;
      }
      const created = await client.createProviderModel(
        provider.id,
        // EXPLICIT capabilities. See `DEFAULT_MODEL_CAPABILITIES`.
        { model_key: modelKey, capabilities: DEFAULT_MODEL_CAPABILITIES },
        { idempotencyKey: `llm-model:${provider.id}:${modelKey}` },
      );
      modelIds.push(created.id);
      trace.push({ step: "provider_model", outcome: "created", detail: modelKey });
    }
  } catch (error) {
    if (isLlmProvisioningError(error)) throw error;
    state = { ...state, providerModelIds: modelIds };
    throw rethrowAsStep(error, "provider_model", state, trace);
  }
  state = { ...state, providerModelIds: modelIds };
  firstModelId = modelIds[0] ?? null;
  if (firstModelId === null) fail("provider_model", CONSOLE_MESSAGE_KEYS.llm_model_required);

  /* --- 3. provider credential ------------------------------------------- */
  //
  // REQUIRED EVEN THOUGH THE ENDPOINT IS KEYLESS. Without the row, routing
  // answers `404 credential_not_found` before it builds any request — an error
  // that reads as "your key is wrong" when the truth is "there is no key at all".
  try {
    const page = await client.listProviderCredentials({
      providerId: provider.id,
      limit: LIST_PAGE_LIMIT,
    });
    const existing = findOnPage(
      page,
      (row) => row.provider_id === provider.id && row.status !== "deleted",
      "provider_credential",
    );
    if (existing !== null) {
      state = { ...state, keyRowId: existing.id };
      trace.push({ step: "provider_credential", outcome: "reused", detail: existing.id });
    } else {
      const created = await client.createProviderCredential(
        {
          // Resolved from the provider record THIS function created or found —
          // never from a request body.
          provider_id: provider.id,
          credential_type: "api_key",
          scope: { type: "global" },
          // `endpoint` must be ABSENT, not null: the untagged union would match
          // the azure arm as well and the request would be refused with no field
          // named. The constructor is the only sanctioned way to build this.
          secret: apiKeyCredentialSecret(options.placeholderKey ?? generatedPlaceholderKey()),
        },
        { idempotencyKey: `llm-credential:${provider.id}` },
      );
      state = { ...state, keyRowId: created.id };
      trace.push({ step: "provider_credential", outcome: "created", detail: created.id });
    }
  } catch (error) {
    if (isLlmProvisioningError(error)) throw error;
    throw rethrowAsStep(error, "provider_credential", state, trace);
  }

  /* --- 4. enable, only when it is not already active --------------------- */
  try {
    if (provider.status === "active") {
      state = { ...state, providerEnabled: true };
      trace.push({ step: "provider_enable", outcome: "skipped", detail: null });
    } else {
      const enabled = await client.enableProvider(provider.id, ifMatchFor(provider));
      provider = enabled;
      state = { ...state, providerEnabled: true };
      trace.push({ step: "provider_enable", outcome: "created", detail: provider.id });
    }
  } catch (error) {
    throw rethrowAsStep(error, "provider_enable", state, trace);
  }

  /* --- 5. routing policy on the SEEDED `general` route ------------------- */
  try {
    const route = await client.findRouteByKey(GENERAL_ROUTE_KEY);
    if (route === null) {
      // Not created. `POST /api/v1/admin/routes` documents no 409 for a duplicate
      // `route_key`, so a console that created one could land a second `general`
      // and leave routing with two candidates and no rule for choosing.
      fail("routing_policy", CONSOLE_MESSAGE_KEYS.llm_general_route_missing);
    }
    const routeId = route!.id;
    const page = await client.listRoutingPolicies({ limit: LIST_PAGE_LIMIT });
    const policyIds: string[] = [];
    for (const providerModelId of modelIds) {
      const existing = findOnPage(
        page,
        (row) =>
          row.route_id === routeId &&
          row.provider_id === provider.id &&
          row.provider_model_id === providerModelId &&
          row.status !== "deleted",
        "routing_policy",
      );
      if (existing !== null) {
        policyIds.push(existing.id);
        trace.push({ step: "routing_policy", outcome: "reused", detail: existing.id });
        continue;
      }
      const created = await client.createRoutingPolicy(
        {
          route_id: routeId,
          provider_id: provider.id,
          provider_model_id: providerModelId,
          priority: 100,
          weight: 1,
        },
        { idempotencyKey: `llm-policy:${routeId}:${provider.id}:${providerModelId}` },
      );
      policyIds.push(created.id);
      trace.push({ step: "routing_policy", outcome: "created", detail: created.id });
    }
    state = { ...state, routingPolicyIds: policyIds };
  } catch (error) {
    if (isLlmProvisioningError(error)) throw error;
    throw rethrowAsStep(error, "routing_policy", state, trace);
  }

  return { providerId: provider.id, state, trace: [...trace] };
}

/**
 * Re-throw a non-provisioning failure with the step it happened at attached.
 *
 * A `MoiraRequestError` keeps travelling as itself — `withConsoleSession`
 * already renders those as the client-safe union with a remedy — but the STATE
 * is what would otherwise be lost, so it is attached by wrapping instead.
 */
function rethrowAsStep(
  error: unknown,
  step: ConnectStepName,
  state: ConnectState,
  trace: readonly LlmConnectStepView[],
): never {
  const wrapped = new LlmProvisioningError({
    step,
    messageKey: CONSOLE_MESSAGE_KEYS.llm_connect_step_failed,
    state,
    trace: [...trace],
  });
  wrapped.cause = error;
  throw wrapped;
}
