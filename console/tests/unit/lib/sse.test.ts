import { describe, expect, test } from "bun:test";

import { parsePublicSseEnvelope, parseSseFrame, readSseEnvelopes, splitSseFrames } from "@/lib/sse";

describe("splitSseFrames", () => {
  test("splits on the blank-line terminator and returns the trailing partial frame separately", () => {
    const { frames, rest } = splitSseFrames("data: a\n\ndata: b\n\ndata: c");
    expect(frames).toEqual(["data: a", "data: b"]);
    expect(rest).toBe("data: c");
  });

  test("tolerates CRLF line endings", () => {
    const { frames, rest } = splitSseFrames("data: a\r\n\r\ndata: b");
    expect(frames).toEqual(["data: a"]);
    expect(rest).toBe("data: b");
  });

  test("an empty buffer produces no frames and no rest", () => {
    const { frames, rest } = splitSseFrames("");
    expect(frames).toEqual([]);
    expect(rest).toBe("");
  });
});

describe("parseSseFrame", () => {
  test("reads event/id/data lines and ignores a leading space in the value", () => {
    const parsed = parseSseFrame('event: response.output_text.delta\nid: 5\ndata: {"text":"hi"}');
    expect(parsed.event).toBe("response.output_text.delta");
    expect(parsed.id).toBe("5");
    expect(parsed.data).toBe('{"text":"hi"}');
  });

  test("joins multiple data lines with a newline, per the SSE spec", () => {
    const parsed = parseSseFrame("data: line one\ndata: line two");
    expect(parsed.data).toBe("line one\nline two");
  });

  test("ignores comment lines (keep-alives) and blank lines", () => {
    const parsed = parseSseFrame(": heartbeat\n\ndata: payload");
    expect(parsed.data).toBe("payload");
  });

  test("a frame with no data line at all has empty data", () => {
    const parsed = parseSseFrame("event: response.in_progress");
    expect(parsed.data).toBe("");
  });
});

describe("parsePublicSseEnvelope", () => {
  const VALID = JSON.stringify({
    response_id: "resp_1",
    execution_id: "exec_1",
    request_id: "req_1",
    sequence: 3,
    timestamp: "2026-08-15T00:00:00Z",
    type: "response.output_text.delta",
    payload: { text: "hi" },
  });

  test("parses a well-formed envelope", () => {
    const envelope = parsePublicSseEnvelope(VALID);
    expect(envelope).not.toBeNull();
    expect(envelope?.type).toBe("response.output_text.delta");
    expect(envelope?.execution_id).toBe("exec_1");
    expect(envelope?.sequence).toBe(3);
    expect(envelope?.payload).toEqual({ text: "hi" });
  });

  test("defaults a missing payload to null rather than throwing", () => {
    const envelope = parsePublicSseEnvelope(
      JSON.stringify({
        response_id: "resp_1",
        execution_id: "exec_1",
        request_id: "req_1",
        sequence: 1,
        timestamp: "2026-08-15T00:00:00Z",
        type: "response.in_progress",
      }),
    );
    expect(envelope?.payload).toBeNull();
  });

  test("returns null for unparseable JSON", () => {
    expect(parsePublicSseEnvelope("not json")).toBeNull();
  });

  test("returns null when a required field is missing", () => {
    expect(parsePublicSseEnvelope(JSON.stringify({ type: "response.completed" }))).toBeNull();
  });

  test("returns null for a JSON array or primitive, not just malformed text", () => {
    expect(parsePublicSseEnvelope("[]")).toBeNull();
    expect(parsePublicSseEnvelope("42")).toBeNull();
  });
});

function streamOf(text: string): ReadableStream<Uint8Array> {
  const bytes = new TextEncoder().encode(text);
  return new ReadableStream({
    start(controller) {
      controller.enqueue(bytes);
      controller.close();
    },
  });
}

describe("readSseEnvelopes", () => {
  test("yields one envelope per well-formed frame, in order", async () => {
    const frame = (sequence: number, type: string) =>
      `event: ${type}\ndata: ${JSON.stringify({
        response_id: "resp_1",
        execution_id: "exec_1",
        request_id: "req_1",
        sequence,
        timestamp: "2026-08-15T00:00:00Z",
        type,
        payload: {},
      })}\n\n`;

    const text = frame(1, "response.created") + frame(2, "response.completed");
    const envelopes = [];
    for await (const envelope of readSseEnvelopes(streamOf(text))) {
      envelopes.push(envelope);
    }
    expect(envelopes.map((e) => e.type)).toEqual(["response.created", "response.completed"]);
  });

  test("reassembles a frame split across two underlying chunks", async () => {
    const full = JSON.stringify({
      response_id: "resp_1",
      execution_id: "exec_1",
      request_id: "req_1",
      sequence: 1,
      timestamp: "2026-08-15T00:00:00Z",
      type: "response.output_text.delta",
      payload: { text: "hello" },
    });
    const half = Math.floor(full.length / 2);
    const first = `data: ${full.slice(0, half)}`;
    const second = `${full.slice(half)}\n\n`;

    const stream = new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(new TextEncoder().encode(first));
        controller.enqueue(new TextEncoder().encode(second));
        controller.close();
      },
    });

    const envelopes = [];
    for await (const envelope of readSseEnvelopes(stream)) {
      envelopes.push(envelope);
    }
    expect(envelopes).toHaveLength(1);
    expect(envelopes[0]?.payload).toEqual({ text: "hello" });
  });

  test("skips a bare keep-alive comment frame without yielding anything for it", async () => {
    const dataFrame = `data: ${JSON.stringify({
      response_id: "resp_1",
      execution_id: "exec_1",
      request_id: "req_1",
      sequence: 1,
      timestamp: "2026-08-15T00:00:00Z",
      type: "response.completed",
      payload: {},
    })}\n\n`;
    const text = ": heartbeat\n\n" + dataFrame;

    const envelopes = [];
    for await (const envelope of readSseEnvelopes(streamOf(text))) {
      envelopes.push(envelope);
    }
    expect(envelopes).toHaveLength(1);
    expect(envelopes[0]?.type).toBe("response.completed");
  });

  test("stops yielding once the signal is already aborted", async () => {
    const controller = new AbortController();
    controller.abort();
    const text = `data: ${JSON.stringify({
      response_id: "resp_1",
      execution_id: "exec_1",
      request_id: "req_1",
      sequence: 1,
      timestamp: "2026-08-15T00:00:00Z",
      type: "response.completed",
      payload: {},
    })}\n\n`;

    const envelopes = [];
    for await (const envelope of readSseEnvelopes(streamOf(text), controller.signal)) {
      envelopes.push(envelope);
    }
    expect(envelopes).toEqual([]);
  });
});
