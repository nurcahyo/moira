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
//
// ============================================================================
// REUSE-FIRST MEANS REUSE **AND REPAIR** — 'active' IS THE ONLY STATUS ROUTING
// ACCEPTS
// ============================================================================
//
// A match is looked up as "not deleted" and then brought to `active` if it is
// not there already. Both halves are load-bearing and for opposite reasons:
//
//   THE WIDE MATCH is what stops the chain creating a duplicate. Disable writes
//   `status = 'disabled'` and leaves `deleted_at` null, so a disabled model row
//   still occupies `provider_models_provider_model_key_active_unique` — a
//   partial index on `deleted_at is null`. A narrow match would try to create a
//   second one, and the unique violation has no mapping for that table, so the
//   operator gets an opaque 500.
//
//   THE REPAIR is what makes the report true. `runtime.rs` joins
//   `provider_models`, `routing_policies` and `provider_credentials` on
//   `status = 'active'`, so a chain that "reused" three disabled rows and
//   announced "the provider, its models, a credential row and routing all
//   exist" would be describing a deployment no prompt can reach. A repaired step
//   reports the outcome `enabled`, which is neither `created` nor `reused`.
//
// This is also what makes the DELETE-means-disable decision honest: disable is
// only reversible if something reverses it.

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
 * The deadline elapsed, or the connection failed, while the body was arriving.
 *
 * A distinct type so `discoverModels` can say WHY it is answering
 * `llm_discovery_unreachable` in one place instead of inferring it from a bare
 * `catch`.
 */
export class DiscoveryBodyError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "DiscoveryBodyError";
  }
}

/**
 * `work`, but abandoned the moment `signal` aborts.
 *
 * ============================================================================
 * THE SIGNAL HAS TO BE OBSERVED HERE, NOT ONLY HANDED TO `fetch`
 * ============================================================================
 *
 * Passing `signal` to `fetch` aborts the body stream on a real runtime — but
 * only on a real runtime. Every fake `fetch` in the test suite hands back a
 * `Response` it constructed itself, whose body stream has no relationship to the
 * controller at all, so a deadline that existed only inside `fetch` would be
 * untestable: the one hang a test could write is the one hang it already
 * covers. Racing each `read()` against the signal makes the bound a property of
 * THIS function, observable with an ordinary stalled `ReadableStream`.
 */
function untilAborted<T>(work: Promise<T>, signal: AbortSignal | undefined): Promise<T> {
  if (signal === undefined) return work;
  if (signal.aborted) {
    return Promise.reject(new DiscoveryBodyError("the discovery deadline had already elapsed"));
  }
  return new Promise<T>((resolve, reject) => {
    const onAbort = (): void => {
      reject(new DiscoveryBodyError("the discovery deadline elapsed while the body was arriving"));
    };
    signal.addEventListener("abort", onAbort, { once: true });
    // `then` before `finally` so the settled work always has a handler attached:
    // once the race has been lost these rejections are ignored rather than
    // becoming unhandled ones.
    void work.then(resolve, reject).finally(() => {
      signal.removeEventListener("abort", onAbort);
    });
  });
}

/** Close a body this console will not read, without waiting on the peer. */
function discardBody(response: Response): void {
  void response.body?.cancel().catch(() => undefined);
}

/**
 * Read at most `limit` bytes of a response body, for at most as long as `signal`
 * stays unaborted.
 *
 * ============================================================================
 * BOTH BOUNDS, OR NEITHER IS A BOUND
 * ============================================================================
 *
 * Streamed rather than `await response.text()`, because `text()` is unbounded:
 * an endpoint that answers with an endless body would be read until the process
 * ran out of memory. The reader is cancelled as soon as the cap is passed, which
 * also closes the socket.
 *
 * The SIZE cap alone is not enough, and that is the defect this parameter
 * exists for. `fetch` resolves when the RESPONSE HEADERS arrive, so a probe
 * whose only deadline is around the fetch is bounded on connect-and-headers and
 * on nothing else. An endpoint that answers `200` in ten milliseconds and then
 * writes one byte every thirty seconds never reaches the byte cap, never
 * reports `done`, and holds a request handler and a socket open for as long as
 * it likes — undici's `bodyTimeout` is a 300 s INACTIVITY timer, which such a
 * trickle resets forever. So the deadline is passed in and observed on every
 * read.
 *
 * Exported so both bounds can be exercised without a socket.
 */
export async function readBounded(
  response: Response,
  limit: number,
  signal?: AbortSignal,
): Promise<string | null> {
  const declared = response.headers.get("content-length");
  if (declared !== null && Number(declared) > limit) {
    discardBody(response);
    return null;
  }

  const body = response.body;
  if (body === null) {
    const text = await untilAborted(response.text(), signal);
    return text.length > limit ? null : text;
  }

  const reader = body.getReader();
  const chunks: Uint8Array[] = [];
  let total = 0;
  try {
    for (;;) {
      const { done, value } = await untilAborted(reader.read(), signal);
      if (done) break;
      if (value === undefined) continue;
      total += value.byteLength;
      if (total > limit) {
        await untilAborted(reader.cancel(), signal).catch(() => undefined);
        return null;
      }
      chunks.push(value);
    }
  } catch (error) {
    // A stalled or broken body must not be left holding the socket. Not awaited:
    // waiting on a peer that has already stopped answering is the state being
    // escaped from.
    void reader.cancel().catch(() => undefined);
    throw error;
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
    const key = narrowModelKey((entry as { id?: unknown }).id);
    if (key === null) return null;
    if (!keys.includes(key)) keys.push(key);
    if (keys.length > DISCOVERY_MAX_MODELS) return null;
  }
  return keys;
}

/**
 * One model id, narrowed to what may become a `provider_models.model_key`.
 *
 * ============================================================================
 * THE SAME BOUNDS ON BOTH WAYS IN, BECAUSE THEY REACH THE SAME COLUMN
 * ============================================================================
 *
 * A model id enters a provider row by two doors: the discovery listing an
 * endpoint advertised, and the `model_keys` array the browser posts back with
 * `action: "connect"`. Narrowing only the first is narrowing neither — the
 * second is a plain request body and nothing obliges it to carry what discovery
 * offered. Whatever a caller puts there travels into a provider row, into every
 * list response, into a rendered table, and onto the wire of every completion
 * built from that row.
 *
 * So the cap and the control-character refusal live here, once, and both doors
 * call it.
 */
export function narrowModelKey(raw: unknown): string | null {
  if (typeof raw !== "string") return null;
  const trimmed = raw.trim();
  if (trimmed === "") return null;
  if (trimmed.length > DISCOVERY_MAX_MODEL_KEY_LENGTH) return null;
  // A control character in a model id would travel into a provider row, a URL
  // and a rendered table. There is no legitimate one. Written as escapes rather
  // than as raw bytes so the range stays readable in a diff.
  //
  // Issue #117: this branch was indented four spaces deeper than its block, which
  // read as a guard nested inside the length check above it. It is not nested; it
  // is the fourth of four flat refusals. `package.json` exposes `format` only as
  // `prettier --write`, with no check variant in any gate, so nothing caught it.
  if (/[\u0000-\u001f\u007f]/.test(trimmed)) return null;
  return trimmed;
}

/**
 * A non-empty list of model ids from an untrusted array, or `null`.
 *
 * [`narrowModelKey`] applied to the `connect` stage's `model_keys`, with the
 * same de-duplication and the same 200-entry ceiling discovery applies. Empty is
 * `null` rather than `[]`: a chain asked to register a provider with no model
 * produces one routing never selects, which is `llm_model_required` and not a
 * silent success.
 */
export function narrowModelKeys(value: unknown): readonly string[] | null {
  if (!Array.isArray(value) || value.length === 0) return null;
  const keys: string[] = [];
  for (const entry of value) {
    const key = narrowModelKey(entry);
    if (key === null) return null;
    if (!keys.includes(key)) keys.push(key);
    if (keys.length > DISCOVERY_MAX_MODELS) return null;
  }
  return keys;
}

/* -------------------------------------------------------------------------- */
/* Authorization for the one effect that never reaches Moira                  */
/* -------------------------------------------------------------------------- */

/**
 * Refuse the discovery probe unless the operator holds an `admin_identities`
 * grant.
 *
 * ============================================================================
 * THE CONSOLE SESSION GATE IS NOT AN ADMIN CHECK, AND THIS IS THE ONE HANDLER
 * THAT NEEDED IT TO BE
 * ============================================================================
 *
 * `withConsoleSession` runs `checkSession`, which admits any session with a
 * verified address inside `allowed_email_domains` and an IdP subject. It reads
 * no grant, and `lib/console-api.ts` says so in as many words: *"It is not the
 * security control. Moira is."* That reasoning is sound for every other handler
 * on this surface, because every one of their effects IS a Moira admin call that
 * Moira authorizes itself — a signed-in employee with no grant gets a 403 from
 * Moira and nothing happens.
 *
 * `POST /api/llm/connect-vllm {action: "discover"}` is the exception. Its whole
 * effect is an outbound GET issued from inside the deployment's network to a
 * host named in the request body, and it returns before touching `client` at
 * all — so no Moira authorization is ever consulted, and the session gate is the
 * entire authorization. That makes it a host and port oracle over the internal
 * network for anyone whose IdP address happens to be in the allow-list: the
 * three refusal keys separate the outcomes cleanly (`unreachable` = nothing
 * there, `refused` = something is listening and speaks HTTP, `invalid_response`
 * = it speaks HTTP and answers 2xx), and up to 200 bounded ids of the response
 * come back reflected as `models`.
 *
 * ============================================================================
 * WHY A READ, AND WHY THIS READ
 * ============================================================================
 *
 * Every path under `/api/v1/admin/*` goes through `authenticate_admin`, which is
 * the ONLY authenticator that applies the `admin_identities` grant union
 * (`src/security/auth.rs`). So any admin-plane call proves the grant, and the
 * cheapest honest one is the list this operator is about to act on: it reads a
 * single page-one row, writes nothing, and needs no authority the `connect`
 * stage would not need a moment later.
 *
 * It THROWS rather than returning a verdict, deliberately. Moira's own 403
 * travels as a `MoiraRequestError` and `withConsoleSession` already renders that
 * as the keyed, client-safe union with a remedy — re-deriving the refusal here
 * would be a second spelling of a rule Moira owns.
 *
 * Narrowing the URL is not a substitute for this and never was: reaching the
 * operator's own LAN is the feature, so there is no private-address denylist to
 * hide behind. The question is only who may ask.
 */
export async function requireAdminGrant(client: MoiraClient): Promise<void> {
  await client.listProviders({ limit: 1 });
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
 *
 * NO PATH THROWS, AND THE BODY IS PART OF "NO PATH". The first version of this
 * function wrapped the fetch and left `readBounded` bare, which was a promise it
 * only kept until the headers: a reset, a TLS shutdown or a premature close
 * mid-body rejected out of here, out of the handler callback, past
 * `withConsoleSession`'s catch — which rethrows anything that is not a
 * `MoiraRequestError` — and rendered as a Next 500 with a stack. A half-open
 * network on the operator's own LAN is the stated normal case for this screen;
 * it must not be a console error page.
 *
 * ============================================================================
 * THE DEADLINE COVERS THE WHOLE EXCHANGE, NOT THE HEADERS
 * ============================================================================
 *
 * The abort stays armed until the body has been read, and `clearTimeout` is in
 * the outer `finally` for that reason alone. Clearing it in the fetch's own
 * `finally` disarmed the only bound the body ever had — see `readBounded`.
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

  try {
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
    }

    if (!response.ok) {
      discardBody(response);
      return { ok: false, messageKey: CONSOLE_MESSAGE_KEYS.llm_discovery_refused };
    }

    let text: string | null;
    try {
      text = await readBounded(response, DISCOVERY_MAX_BYTES, controller.signal);
    } catch {
      // The deadline elapsed mid-body, or the connection failed after the
      // headers. Both are "the endpoint did not answer", which is the same thing
      // a refused connection means, so they carry the same key.
      return { ok: false, messageKey: CONSOLE_MESSAGE_KEYS.llm_discovery_unreachable };
    }
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
  } finally {
    clearTimeout(timer);
  }
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
 *
 * IT CARRIES THE PROGRESS IT INTERRUPTED. `LlmProvisioningError` exists to tell
 * the operator what already exists — that is the whole reason it is not a bare
 * `Error` — and a refusal raised at step five with `state: emptyConnectState()`
 * throws away exactly the fact the report was built to carry. The caller passes
 * what it has written so far; only a call site with nothing written yet takes
 * the default.
 */
export function findOnPage<T>(
  page: ListResponse<T>,
  match: (row: T) => boolean,
  step: ConnectStepName,
  progress: {
    readonly state: ConnectState;
    readonly trace: readonly LlmConnectStepView[];
  } = { state: emptyConnectState(), trace: [] },
): T | null {
  const hit = page.data.find(match);
  if (hit !== undefined) return hit;
  if (page.pagination.has_more) {
    throw new LlmProvisioningError({
      step,
      messageKey: CONSOLE_MESSAGE_KEYS.llm_list_truncated,
      state: progress.state,
      trace: [...progress.trace],
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
        { state: { ...state, providerModelIds: [...modelIds] }, trace },
      );
      if (existing !== null) {
        // REUSED **AND REPAIRED**. A disabled row still occupies
        // `provider_models_provider_model_key_active_unique` — the partial index
        // is on `deleted_at is null`, which disable leaves null — so creating a
        // second one is a unique violation Moira has no mapping for, and an
        // opaque 500 for the operator. Matching it and turning it back on is
        // both the only way through and the undo this screen otherwise lacks.
        const repaired =
          existing.status === "active"
            ? existing
            : await client.enableProviderModel(existing.id, ifMatchFor(existing));
        modelIds.push(repaired.id);
        trace.push({
          step: "provider_model",
          outcome: existing.status === "active" ? "reused" : "enabled",
          detail: modelKey,
        });
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
      { state, trace },
    );
    if (existing !== null) {
      // Repaired for the same reason the model row is: routing resolves a
      // credential whose status is `active` (`runtime.rs`), so a disabled row
      // answers `credential_not_found` exactly as a missing one does — and this
      // step exists because that error names neither.
      const repaired =
        existing.status === "active"
          ? existing
          : await client.enableProviderCredential(existing.id, ifMatchFor(existing));
      state = { ...state, keyRowId: repaired.id };
      trace.push({
        step: "provider_credential",
        outcome: existing.status === "active" ? "reused" : "enabled",
        detail: repaired.id,
      });
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
        { state: { ...state, routingPolicyIds: [...policyIds] }, trace },
      );
      if (existing !== null) {
        // The last of the three. `runtime.rs` joins `routing_policies` on
        // `rp.status = 'active'`, so a disabled policy is a policy that cannot
        // be created again for the same triple and cannot be selected either.
        const repaired =
          existing.status === "active"
            ? existing
            : await client.enableRoutingPolicy(existing.id, ifMatchFor(existing));
        policyIds.push(repaired.id);
        trace.push({
          step: "routing_policy",
          outcome: existing.status === "active" ? "reused" : "enabled",
          detail: repaired.id,
        });
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
