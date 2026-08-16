// One place that knows the two error shapes `app/api/runners/**` answers with.
// The same split `modules/llm/request.ts` documents in full — see that file's
// header for why there are exactly two — with one addition: `code`, because
// `RunnerDetail` has to tell `409 runner_token_unavailable` (unrecoverable —
// say so plainly, never offer a retry) apart from every other refusal (an
// ordinary retryable failure).

import { CONSOLE_MESSAGE_KEYS } from "@/lib/i18n";
import type { JsonValue } from "@/lib/types";

export interface RunnerFailure {
  readonly messageKey: string;
  /** Moira's already-interpolated prose, when it supplied any. */
  readonly message: string | undefined;
  readonly messageArgs: JsonValue | undefined;
  /** The Moira or console-invented error code, when the refusal carried one. */
  readonly code: string | null;
}

export type RunnerResult<T> =
  | { readonly ok: true; readonly data: T }
  | { readonly ok: false; readonly failure: RunnerFailure };

function asRecord(value: unknown): Record<string, unknown> | null {
  return typeof value === "object" && value !== null && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : null;
}

const GENERIC_FAILURE: RunnerFailure = {
  messageKey: CONSOLE_MESSAGE_KEYS.runners_request_failed,
  message: undefined,
  messageArgs: undefined,
  code: null,
};

/** Read whichever of the two shapes arrived. */
export function readRunnerFailure(body: unknown): RunnerFailure {
  const envelope = asRecord(body);
  const error = envelope === null ? null : asRecord(envelope["error"]);
  if (error === null) return GENERIC_FAILURE;

  const text = asRecord(error["text"]);
  const messageKey =
    typeof text?.["messageKey"] === "string"
      ? (text["messageKey"] as string)
      : typeof error["message_key"] === "string"
        ? (error["message_key"] as string)
        : CONSOLE_MESSAGE_KEYS.runners_request_failed;

  return {
    messageKey,
    message: typeof text?.["message"] === "string" ? (text["message"] as string) : undefined,
    messageArgs: (text?.["messageArgs"] as JsonValue | undefined) ?? undefined,
    code: typeof error["code"] === "string" ? (error["code"] as string) : null,
  };
}

/**
 * Send one BFF request.
 *
 * A thrown `fetch` — offline, navigation away — is a keyed failure like any
 * other rather than an unhandled rejection: this runs in a browser and the
 * alternative is a screen that stops responding with nothing written on it.
 */
export async function sendRunnerRequest<T>(
  url: string,
  init: RequestInit,
  fetchImpl?: typeof fetch,
): Promise<RunnerResult<T>> {
  const send = fetchImpl ?? globalThis.fetch;
  let response: Response;
  try {
    response = await send(url, init);
  } catch {
    return { ok: false, failure: GENERIC_FAILURE };
  }

  if (response.status === 204) return { ok: true, data: undefined as T };

  let body: unknown;
  try {
    body = await response.json();
  } catch {
    body = undefined;
  }

  if (!response.ok) return { ok: false, failure: readRunnerFailure(body) };
  return { ok: true, data: body as T };
}

/** `POST` with a JSON body. */
export function postRunnerJson<T>(
  url: string,
  body: unknown,
  fetchImpl?: typeof fetch,
): Promise<RunnerResult<T>> {
  return sendRunnerRequest<T>(
    url,
    { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) },
    fetchImpl,
  );
}

/** `DELETE`, no body. */
export function sendRunnerDelete<T>(url: string, fetchImpl?: typeof fetch): Promise<RunnerResult<T>> {
  return sendRunnerRequest<T>(url, { method: "DELETE" }, fetchImpl);
}
