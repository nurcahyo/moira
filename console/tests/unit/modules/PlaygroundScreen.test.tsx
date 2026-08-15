import { describe, expect, test } from "bun:test";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { CONSOLE_MESSAGE_KEYS, t } from "@/lib/i18n";
import { PlaygroundScreen } from "@/modules/playground/PlaygroundScreen";

function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
}

function sseFrame(sequence: number, type: string, payload: Record<string, unknown>): string {
  const envelope = {
    response_id: "resp_1",
    execution_id: "exec_1",
    request_id: "req_1",
    sequence,
    timestamp: "2026-08-15T00:00:00Z",
    type,
    payload,
  };
  return `event: ${type}\nid: ${sequence}\ndata: ${JSON.stringify(envelope)}\n\n`;
}

function sseResponse(text: string): Response {
  const stream = new ReadableStream<Uint8Array>({
    start(controller) {
      controller.enqueue(new TextEncoder().encode(text));
      controller.close();
    },
  });
  return new Response(stream, { status: 200, headers: { "content-type": "text/event-stream" } });
}

const EXECUTION_SUMMARY = {
  execution_id: "exec_1",
  response_id: "resp_1",
  request_id: "req_1",
  status: "completed",
  attempt_count: 1,
  usage: { input_tokens: 5, output_tokens: 2, total_tokens: 7 },
  latency_ms: 123,
  route: { id: "aaaaaaaa-1111-4111-8111-111111111111", key: "general" },
  model: { id: "bbbbbbbb-1111-4111-8111-111111111111", provider: "open_ai", key: "gpt-test" },
};

async function typePrompt(text: string): Promise<void> {
  const textarea = screen.getByRole("textbox", { name: t(CONSOLE_MESSAGE_KEYS.playground_prompt_label) });
  await userEvent.type(textarea, text);
}

async function clickSend(): Promise<void> {
  await userEvent.click(screen.getByRole("button", { name: t(CONSOLE_MESSAGE_KEYS.playground_send) }));
}

describe("PlaygroundScreen — renders", () => {
  test("shows the prompt input, the send button and an empty response panel", () => {
    render(<PlaygroundScreen routes={[]} agentProfiles={[]} providers={[]} fetchImpl={(() => {
      throw new Error("unexpected fetch");
    }) as unknown as typeof fetch} />);
    expect(
      screen.getByRole("textbox", { name: t(CONSOLE_MESSAGE_KEYS.playground_prompt_label) }),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: t(CONSOLE_MESSAGE_KEYS.playground_send) })).toBeInTheDocument();
    expect(screen.getByText(t(CONSOLE_MESSAGE_KEYS.playground_response_empty))).toBeInTheDocument();
    expect(screen.getByText(t(CONSOLE_MESSAGE_KEYS.playground_routing_summary_unavailable))).toBeInTheDocument();
    expect(screen.getByText(t(CONSOLE_MESSAGE_KEYS.playground_tools_unavailable_note))).toBeInTheDocument();
  });

  test("the Send button is disabled until a prompt is entered", async () => {
    render(<PlaygroundScreen routes={[]} agentProfiles={[]} providers={[]} fetchImpl={(() => {
      throw new Error("unexpected fetch");
    }) as unknown as typeof fetch} />);
    expect(screen.getByRole("button", { name: t(CONSOLE_MESSAGE_KEYS.playground_send) })).toBeDisabled();
    await typePrompt("hi");
    expect(screen.getByRole("button", { name: t(CONSOLE_MESSAGE_KEYS.playground_send) })).toBeEnabled();
  });
});

describe("PlaygroundScreen — sends and streams into view (the default, stream=true)", () => {
  test("tokens accumulate into the response panel, then the routing summary loads", async () => {
    const text =
      sseFrame(1, "response.output_text.delta", { text: "Hel" }) +
      sseFrame(2, "response.output_text.delta", { text: "lo" }) +
      sseFrame(3, "response.completed", { status: "completed", usage: {} });

    const fetchImpl = (async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url.endsWith("/api/playground/stream")) return sseResponse(text);
      if (url.includes("/api/playground/executions/")) return jsonResponse(EXECUTION_SUMMARY);
      throw new Error(`unexpected fetch: ${url}`);
    }) as unknown as typeof fetch;

    render(<PlaygroundScreen routes={[]} agentProfiles={[]} providers={[]} fetchImpl={fetchImpl} />);
    await typePrompt("hi");
    await clickSend();

    await waitFor(() => expect(screen.getByText("Hello")).toBeInTheDocument());
    await waitFor(() =>
      expect(screen.getByText(t(CONSOLE_MESSAGE_KEYS.playground_routing_attempt_count_label))).toBeInTheDocument(),
    );
    expect(screen.getByText("general")).toBeInTheDocument();
    expect(screen.getByText("gpt-test")).toBeInTheDocument();
  });

  test("a failed terminal event renders the keyed refusal, not a raw provider body", async () => {
    const text = sseFrame(1, "response.failed", {
      status: "failed",
      error: { code: "route_forbidden", message: "route override is not authorized", request_id: "req_1" },
    });
    const fetchImpl = (async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url.endsWith("/api/playground/stream")) return sseResponse(text);
      throw new Error(`unexpected fetch: ${url}`);
    }) as unknown as typeof fetch;

    render(<PlaygroundScreen routes={[]} agentProfiles={[]} providers={[]} fetchImpl={fetchImpl} />);
    await typePrompt("hi");
    await clickSend();

    await waitFor(() => expect(screen.getByRole("alert")).toHaveTextContent("route override is not authorized"));
  });
});

describe("PlaygroundScreen — the Stop button actually aborts the request", () => {
  test("clicking Stop aborts the in-flight stream's AbortSignal", async () => {
    let capturedSignal: AbortSignal | undefined;
    let streamController: ReadableStreamDefaultController<Uint8Array> | undefined;

    const fetchImpl = (async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input);
      if (!url.endsWith("/api/playground/stream")) throw new Error(`unexpected fetch: ${url}`);
      capturedSignal = init?.signal as AbortSignal | undefined;
      const stream = new ReadableStream<Uint8Array>({
        start(controller) {
          streamController = controller;
          controller.enqueue(
            new TextEncoder().encode(sseFrame(1, "response.created", { status: "in_progress" })),
          );
          // Deliberately never closes on its own — a real in-flight stream.
        },
      });
      capturedSignal?.addEventListener("abort", () => {
        try {
          streamController?.error(new DOMException("Aborted", "AbortError"));
        } catch {
          // already closed/errored
        }
      });
      return new Response(stream, { status: 200, headers: { "content-type": "text/event-stream" } });
    }) as unknown as typeof fetch;

    render(<PlaygroundScreen routes={[]} agentProfiles={[]} providers={[]} fetchImpl={fetchImpl} />);
    await typePrompt("hi");
    await clickSend();

    const stopButton = await screen.findByRole("button", { name: t(CONSOLE_MESSAGE_KEYS.playground_stop) });
    await userEvent.click(stopButton);

    await waitFor(() => expect(capturedSignal?.aborted).toBe(true));
    await waitFor(() =>
      expect(screen.getByRole("status")).toHaveTextContent(t(CONSOLE_MESSAGE_KEYS.playground_status_cancelled)),
    );
  });
});

describe("PlaygroundScreen — non-streaming fallback and diagnostics mode", () => {
  test("turning off Stream sends the non-streaming request instead", async () => {
    const fetchImpl = (async (input: RequestInfo | URL) => {
      const url = String(input);
      if (url.endsWith("/api/playground/run")) {
        return jsonResponse({
          id: "resp_1",
          object: "response",
          created_at: "2026-08-15T00:00:00Z",
          status: "completed",
          execution_id: "exec_1",
          request_id: "req_1",
          output: [{ type: "message", role: "assistant", content: [{ type: "output_text", text: "Non-streamed" }] }],
          citations: [],
          usage: {},
          metadata: {},
          output_persisted: false,
        });
      }
      if (url.includes("/api/playground/executions/")) return jsonResponse(EXECUTION_SUMMARY);
      throw new Error(`unexpected fetch: ${url}`);
    }) as unknown as typeof fetch;

    render(<PlaygroundScreen routes={[]} agentProfiles={[]} providers={[]} fetchImpl={fetchImpl} />);

    await userEvent.click(screen.getByText(t(CONSOLE_MESSAGE_KEYS.playground_controls_heading)));
    await userEvent.click(screen.getByLabelText(t(CONSOLE_MESSAGE_KEYS.playground_field_stream_toggle_label)));

    await typePrompt("hi");
    await clickSend();

    await waitFor(() => expect(screen.getByText("Non-streamed")).toBeInTheDocument());
  });

  test("Detailed diagnostics renders tool calls and candidate ranking instead of the public-path note", async () => {
    const fetchImpl = (async (input: RequestInfo | URL) => {
      const url = String(input);
      if (!url.endsWith("/api/playground/diagnose")) throw new Error(`unexpected fetch: ${url}`);
      return jsonResponse({
        outcome: {
          request_id: "req_1",
          execution_id: "11111111-1111-4111-8111-111111111111",
          status: "succeeded",
          usage: {},
          attempts: [],
          output_text: "Diagnosed!",
        },
        events: [
          {
            request_id: "req_1",
            execution_id: "11111111-1111-4111-8111-111111111111",
            sequence: 0,
            timestamp: "2026-08-15T00:00:00Z",
            event_type: "tool_call_started",
            payload: { internal_call_id: "c1", name: "lookup_weather", arguments: '{"city":"Paris"}' },
          },
          {
            request_id: "req_1",
            execution_id: "11111111-1111-4111-8111-111111111111",
            sequence: 1,
            timestamp: "2026-08-15T00:00:00Z",
            event_type: "tool_result",
            payload: { attempt_id: "a1", tool_name: "lookup_weather", outcome: "success", failure_kind: null },
          },
        ],
      });
    }) as unknown as typeof fetch;

    render(<PlaygroundScreen routes={[]} agentProfiles={[]} providers={[]} fetchImpl={fetchImpl} />);

    await userEvent.click(screen.getByText(t(CONSOLE_MESSAGE_KEYS.playground_controls_heading)));
    await userEvent.click(
      screen.getByLabelText(t(CONSOLE_MESSAGE_KEYS.playground_field_diagnostics_toggle_label)),
    );

    await typePrompt("hi");
    await clickSend();

    await waitFor(() => expect(screen.getByText("Diagnosed!")).toBeInTheDocument());
    expect(screen.getByText("lookup_weather")).toBeInTheDocument();
    expect(screen.queryByText(t(CONSOLE_MESSAGE_KEYS.playground_tools_unavailable_note))).not.toBeInTheDocument();
  });
});
