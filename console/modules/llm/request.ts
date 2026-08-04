// One place that knows the two error shapes this console's BFF answers with.
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
// mapping happens server-side. Extracted here so all four organisms agree.

import { CONSOLE_MESSAGE_KEYS } from "@/lib/i18n";
import type { JsonValue } from "@/lib/types";

/** What a caller needs in order to render a refusal. */
export interface LlmFailure {
  readonly messageKey: string;
  /** Moira's already-interpolated prose, when it supplied any. */
  readonly message: string | undefined;
  readonly messageArgs: JsonValue | undefined;
  /** The chain step that failed, when the refusal named one. */
  readonly step: string | null;
  /** What was already written, when the refusal reported it. */
  readonly detail: Record<string, unknown> | null;
}

export type LlmResult<T> =
  | { readonly ok: true; readonly data: T }
  | { readonly ok: false; readonly failure: LlmFailure };

function asRecord(value: unknown): Record<string, unknown> | null {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : null;
}

/** Read whichever of the two shapes arrived. */
export function readFailure(body: unknown): LlmFailure {
  const envelope = asRecord(body);
  const error = envelope === null ? null : asRecord(envelope["error"]);
  if (error === null) {
    return {
      messageKey: CONSOLE_MESSAGE_KEYS.llm_request_failed,
      message: undefined,
      messageArgs: undefined,
      step: null,
      detail: null,
    };
  }

  const text = asRecord(error["text"]);
  const messageKey =
    typeof text?.["messageKey"] === "string"
      ? (text["messageKey"] as string)
      : typeof error["message_key"] === "string"
        ? (error["message_key"] as string)
        : CONSOLE_MESSAGE_KEYS.llm_request_failed;

  return {
    messageKey,
    message: typeof text?.["message"] === "string" ? (text["message"] as string) : undefined,
    messageArgs: (text?.["messageArgs"] as JsonValue | undefined) ?? undefined,
    step: typeof error["step"] === "string" ? (error["step"] as string) : null,
    detail: error,
  };
}

/**
 * Send one BFF request.
 *
 * A thrown `fetch` — offline, navigation away — is a keyed failure like any
 * other rather than an unhandled rejection: this runs in a browser and the
 * alternative is a screen that stops responding with nothing written on it.
 */
export async function sendLlmRequest<T>(
  url: string,
  init: RequestInit,
  fetchImpl?: typeof fetch,
): Promise<LlmResult<T>> {
  const send = fetchImpl ?? globalThis.fetch;
  let response: Response;
  try {
    response = await send(url, init);
  } catch {
    return {
      ok: false,
      failure: {
        messageKey: CONSOLE_MESSAGE_KEYS.llm_request_failed,
        message: undefined,
        messageArgs: undefined,
        step: null,
        detail: null,
      },
    };
  }

  let body: unknown;
  try {
    body = await response.json();
  } catch {
    body = undefined;
  }

  if (!response.ok) return { ok: false, failure: readFailure(body) };
  return { ok: true, data: body as T };
}

/** `POST` with a JSON body. */
export function postJson<T>(
  url: string,
  body: unknown,
  fetchImpl?: typeof fetch,
): Promise<LlmResult<T>> {
  return sendLlmRequest<T>(
    url,
    { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) },
    fetchImpl,
  );
}

/** `DELETE`, which on this surface means "disable". */
export function sendDelete<T>(url: string, fetchImpl?: typeof fetch): Promise<LlmResult<T>> {
  return sendLlmRequest<T>(url, { method: "DELETE" }, fetchImpl);
}
