// Shared client-safe transport for organisms that call THIS CONSOLE'S OWN route
// handlers (never Moira directly).
//
// ============================================================================
// WHY THIS IS SHARED RATHER THAN ONE COPY PER FEATURE
// ============================================================================
//
// `modules/llm/request.ts` established this shape first (two refusal envelopes,
// a `fetchImpl` seam for tests, a `LlmResult<T>` discriminated union) and every
// feature added after it — skills, provider health, eval suites, flows — needs
// exactly the same shape with a different fallback message key. Four copies of
// the same ~130 lines is four places for the parsing to drift; this file is the
// one place, parameterised over the fallback key each caller already owns in
// its own i18n namespace.
//
// It is CLIENT-SAFE by construction: no `process.env`, no `pg`, no credential
// header, no AEAD, no `clientSecret` — none of the shapes
// `tests/unit/architecture/layer-dependencies.test.ts` derives the
// credential-module set from. It imports nothing but `@/lib/types` (itself
// asserted client-safe) and belongs in `lib/` under CONVENTIONS.md §6 rule 7
// ("shared, non-UI logic").
//
// ============================================================================
// TWO SHAPES, BECAUSE TWO DIFFERENT PARTIES REFUSED
// ============================================================================
//
//   { error: { code, message_key, … } }  the CONSOLE refused — a body it could
//                                        not read, a nested id that does not
//                                        belong to its parent, a partial chain.
//   { error: MoiraError }                MOIRA refused, narrowed by
//                                        `lib/errors.ts` and re-shaped by
//                                        `moiraErrorBody`. Its copy lives at
//                                        `error.text`, and it already carries a
//                                        `remedy` derived once on the server.
//
// A component that only understood one of them would render the other as a
// generic failure and throw away the remedy — which is the whole reason the
// mapping happens server-side.

import type { JsonValue } from "@/lib/types";

/** What a caller needs in order to render a refusal. */
export interface ConsoleApiFailure {
  readonly messageKey: string;
  /** Moira's already-interpolated prose, when it supplied any. */
  readonly message: string | undefined;
  readonly messageArgs: JsonValue | undefined;
  /** The chain step that failed, when the refusal named one. */
  readonly step: string | null;
  /** What was already written, when the refusal reported it. */
  readonly detail: Record<string, unknown> | null;
  /**
   * The HTTP status, or `0` for a transport failure that never reached a
   * response at all. Present so a caller can distinguish a NORMAL 404 (e.g. "this
   * skill has no HTTP executor yet") from a genuine failure, without re-deriving
   * that distinction from `detail`'s shape.
   */
  readonly status: number;
}

export type ConsoleApiResult<T> =
  | { readonly ok: true; readonly data: T }
  | { readonly ok: false; readonly failure: ConsoleApiFailure };

function asRecord(value: unknown): Record<string, unknown> | null {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : null;
}

/** Read whichever of the two shapes arrived. `fallbackKey` is this caller's own default. */
export function readConsoleApiFailure(
  body: unknown,
  fallbackKey: string,
  status: number,
): ConsoleApiFailure {
  const envelope = asRecord(body);
  const error = envelope === null ? null : asRecord(envelope["error"]);
  if (error === null) {
    return {
      messageKey: fallbackKey,
      message: undefined,
      messageArgs: undefined,
      step: null,
      detail: null,
      status,
    };
  }

  const text = asRecord(error["text"]);
  const messageKey =
    typeof text?.["messageKey"] === "string"
      ? (text["messageKey"] as string)
      : typeof error["message_key"] === "string"
        ? (error["message_key"] as string)
        : fallbackKey;

  return {
    messageKey,
    message: typeof text?.["message"] === "string" ? (text["message"] as string) : undefined,
    messageArgs: (text?.["messageArgs"] as JsonValue | undefined) ?? undefined,
    step: typeof error["step"] === "string" ? (error["step"] as string) : null,
    detail: error,
    status,
  };
}

/**
 * Send one BFF request.
 *
 * A thrown `fetch` — offline, navigation away — is a keyed failure like any
 * other rather than an unhandled rejection: this runs in a browser and the
 * alternative is a screen that stops responding with nothing written on it.
 */
export async function sendConsoleApiRequest<T>(
  url: string,
  init: RequestInit,
  fallbackKey: string,
  fetchImpl?: typeof fetch,
): Promise<ConsoleApiResult<T>> {
  const send = fetchImpl ?? globalThis.fetch;
  let response: Response;
  try {
    response = await send(url, init);
  } catch {
    return {
      ok: false,
      failure: {
        messageKey: fallbackKey,
        message: undefined,
        messageArgs: undefined,
        step: null,
        detail: null,
        status: 0,
      },
    };
  }

  let body: unknown;
  try {
    body = await response.json();
  } catch {
    body = undefined;
  }

  if (!response.ok) {
    return { ok: false, failure: readConsoleApiFailure(body, fallbackKey, response.status) };
  }
  return { ok: true, data: body as T };
}

/** `POST` with a JSON body. */
export function postJson<T>(
  url: string,
  body: unknown,
  fallbackKey: string,
  fetchImpl?: typeof fetch,
): Promise<ConsoleApiResult<T>> {
  return sendConsoleApiRequest<T>(
    url,
    { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) },
    fallbackKey,
    fetchImpl,
  );
}

/** `PATCH` with a JSON body. */
export function patchJson<T>(
  url: string,
  body: unknown,
  fallbackKey: string,
  fetchImpl?: typeof fetch,
): Promise<ConsoleApiResult<T>> {
  return sendConsoleApiRequest<T>(
    url,
    { method: "PATCH", headers: { "content-type": "application/json" }, body: JSON.stringify(body) },
    fallbackKey,
    fetchImpl,
  );
}

/** `DELETE`, with no body. */
export function sendDelete<T>(
  url: string,
  fallbackKey: string,
  fetchImpl?: typeof fetch,
): Promise<ConsoleApiResult<T>> {
  return sendConsoleApiRequest<T>(url, { method: "DELETE" }, fallbackKey, fetchImpl);
}

/** A plain `GET`, for a client-side reload of one resource (e.g. an on-demand executor fetch). */
export function getJson<T>(
  url: string,
  fallbackKey: string,
  fetchImpl?: typeof fetch,
): Promise<ConsoleApiResult<T>> {
  return sendConsoleApiRequest<T>(url, { method: "GET" }, fallbackKey, fetchImpl);
}
