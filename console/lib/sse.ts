// Pure, hand-rolled SSE-frame parsing for the playground (issue #261).
//
// Browsers' `EventSource` cannot issue a `POST` or carry an `Authorization`
// header, and `POST /api/playground/stream` needs both (the console session
// cookie rides along automatically, but the request body — the prompt and
// every control — has to be a POST body). So the playground reads
// `fetch(...).body` directly as a `ReadableStream<Uint8Array>` and parses the
// standard SSE wire format itself, rather than reaching for `EventSource`.
//
// Deliberately pure and dependency-free: every function here takes strings or
// streams in and returns data out, with no `fetch`, no DOM, no React. That is
// what makes `readSseEnvelopes` testable against a synthetic
// `ReadableStream` in a unit test without a browser — see
// `tests/unit/lib/sse.test.ts`.

import type { JsonValue, PublicSseEnvelope } from "./types";

/** One parsed SSE frame's named fields. Unknown field names and comment lines (`:...`) are ignored. */
export interface ParsedSseFrame {
  readonly event: string | undefined;
  readonly id: string | undefined;
  /** Every `data:` line's value, joined with `\n` — the SSE spec's own multi-line rule. */
  readonly data: string;
}

/**
 * Split raw text on the blank-line frame terminator (`\n\n`), tolerating
 * `\r\n` line endings. Returns every COMPLETE frame plus whatever trailing
 * text has not yet seen its terminator, so the caller can prepend it to the
 * next chunk rather than losing a frame split across two reads.
 */
export function splitSseFrames(buffer: string): { readonly frames: string[]; readonly rest: string } {
  const normalized = buffer.replace(/\r\n/g, "\n");
  const parts = normalized.split("\n\n");
  const rest = parts.pop() ?? "";
  return { frames: parts, rest };
}

/** Parse one frame's `event:`/`id:`/`data:` lines. */
export function parseSseFrame(frame: string): ParsedSseFrame {
  let event: string | undefined;
  let id: string | undefined;
  const dataLines: string[] = [];
  for (const line of frame.split("\n")) {
    if (line === "" || line.startsWith(":")) continue; // blank or a comment (keep-alive)
    const colon = line.indexOf(":");
    const field = colon === -1 ? line : line.slice(0, colon);
    const rawValue = colon === -1 ? "" : line.slice(colon + 1);
    const value = rawValue.startsWith(" ") ? rawValue.slice(1) : rawValue;
    if (field === "event") event = value;
    else if (field === "id") id = value;
    else if (field === "data") dataLines.push(value);
  }
  return { event, id, data: dataLines.join("\n") };
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** Parse a frame's `data:` payload as a `PublicSseEnvelope`. `null` on anything that does not match the shape. */
export function parsePublicSseEnvelope(data: string): PublicSseEnvelope | null {
  let parsed: unknown;
  try {
    parsed = JSON.parse(data);
  } catch {
    return null;
  }
  if (!isRecord(parsed)) return null;
  if (typeof parsed["type"] !== "string") return null;
  if (typeof parsed["execution_id"] !== "string") return null;
  if (typeof parsed["response_id"] !== "string") return null;
  if (typeof parsed["request_id"] !== "string") return null;
  if (typeof parsed["sequence"] !== "number") return null;
  if (typeof parsed["timestamp"] !== "string") return null;
  return {
    response_id: parsed["response_id"],
    execution_id: parsed["execution_id"],
    request_id: parsed["request_id"],
    sequence: parsed["sequence"],
    timestamp: parsed["timestamp"],
    type: parsed["type"],
    payload: (parsed["payload"] as JsonValue | undefined) ?? null,
  };
}

/**
 * Consume a `text/event-stream` body, yielding one `PublicSseEnvelope` per
 * frame whose `data:` parses. A frame with no `data:` line at all (a bare
 * keep-alive comment) is silently skipped, not yielded as an error.
 *
 * Reads until the stream ends (the server's own terminal event) or `signal`
 * aborts, whichever comes first. Always releases the reader's lock on the
 * way out, so an aborted or errored stream does not leak it.
 */
export async function* readSseEnvelopes(
  body: ReadableStream<Uint8Array>,
  signal?: AbortSignal,
): AsyncGenerator<PublicSseEnvelope, void, void> {
  const reader = body.getReader();
  const decoder = new TextDecoder();
  let buffer = "";
  try {
    while (true) {
      if (signal?.aborted === true) return;
      const { done, value } = await reader.read();
      if (done) break;
      buffer += decoder.decode(value, { stream: true });
      const { frames, rest } = splitSseFrames(buffer);
      buffer = rest;
      for (const frame of frames) {
        const parsed = parseSseFrame(frame);
        if (parsed.data === "") continue;
        const envelope = parsePublicSseEnvelope(parsed.data);
        if (envelope !== null) yield envelope;
      }
    }
  } finally {
    reader.releaseLock();
  }
}
