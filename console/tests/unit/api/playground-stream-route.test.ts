// `POST /api/playground/stream`, driven as a route handler. Same session-gate
// shape as `playground-run-route.test.ts`; this file's own job is the part
// that route does not have: the raw SSE passthrough, cancellation wiring, and
// a non-OK upstream response reshaped into the keyed envelope rather than
// piped through as a stream.

import { afterEach, beforeEach, describe, expect, test } from "bun:test";

import { POST as PLAYGROUND_STREAM_POST } from "@/app/api/playground/stream/route";
import type { ConsoleAuth } from "@/lib/auth";
import type { ResolvedAuthConfig } from "@/lib/auth-config";
import type { ConsoleRuntime } from "@/lib/auth-runtime";
import { setConsoleApiDependenciesForTests } from "@/lib/console-api";
import { readConsoleEnv, type ConsoleEnv } from "@/lib/env";
import { MoiraClient } from "@/lib/moira-client";
import { readSseEnvelopes } from "@/lib/sse";
import {
  createMoiraStub,
  errorEnvelope,
  MOIRA_STUB_BASE_URL,
  type MoiraStub,
  type StubHandler,
} from "../../support/moira-stub";

const RESPONSES_STREAM = "POST /api/v1/responses/stream";
const PROVIDER_CONFIG_ID = "moira-console-idp";

/** Unmistakable, and asserted absent from every response body and header. */
const SYSTEM_KEY = "sk_test_unmistakable_playground_system_key_9f3c";

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

const HAPPY_PATH_SSE =
  sseFrame(1, "response.created", { status: "in_progress" }) +
  sseFrame(2, "response.output_text.delta", { text: "Hi" }) +
  sseFrame(3, "response.completed", { status: "completed", usage: {} });

function handlers(overrides: Record<string, StubHandler> = {}): Record<string, StubHandler> {
  return {
    [RESPONSES_STREAM]: () => ({ status: 200, bodyText: HAPPY_PATH_SSE }),
    ...overrides,
  };
}

const ADMITTED = {
  session: { id: "session-1", providerId: PROVIDER_CONFIG_ID },
  user: { id: "user-1", email: "ops@example.com", emailVerified: true },
};

const CONFIG = {
  providerId: PROVIDER_CONFIG_ID,
  allowedEmailDomains: ["example.com"],
} as unknown as ResolvedAuthConfig;

function authWith(session: unknown): ConsoleAuth {
  return {
    api: { getSession: async () => session },
    $context: Promise.resolve({
      internalAdapter: {
        findAccountByUserId: async () => [{ providerId: PROVIDER_CONFIG_ID, accountId: "idp-subject-abc" }],
      },
    }),
  } as unknown as ConsoleAuth;
}

const ENV: ConsoleEnv = readConsoleEnv({
  NODE_ENV: "test",
  MOIRA_API_URL: MOIRA_STUB_BASE_URL,
  CONSOLE_PUBLIC_ORIGIN: "https://console.example.com",
  MOIRA_ADMIN_API_AUDIENCE: "moira-admin-audience",
  BETTER_AUTH_SECRET: "a-secret-that-is-at-least-32-characters",
  CONSOLE_SECRET_ENCRYPTION_KEY: Buffer.alloc(32, 7).toString("base64"),
});

let stub: MoiraStub;

function install(
  options: {
    readonly handlers?: Record<string, StubHandler>;
    readonly session?: unknown;
    readonly runtime?: ConsoleRuntime;
  } = {},
): void {
  stub = createMoiraStub(options.handlers ?? handlers());
  const auth = authWith("session" in options ? options.session : ADMITTED);
  const runtime: ConsoleRuntime = options.runtime ?? {
    ok: true,
    auth,
    configs: [CONFIG],
    problems: [],
    stale: false,
  };
  setConsoleApiDependenciesForTests({
    runtime: async () => runtime,
    env: () => ENV,
    clientFor: () => new MoiraClient({ baseUrl: MOIRA_STUB_BASE_URL, systemKey: SYSTEM_KEY, fetch: stub.fetch }),
  });
}

beforeEach(() => {
  install();
});

afterEach(() => {
  setConsoleApiDependenciesForTests(null);
});

function request(body: unknown, init: { readonly signal?: AbortSignal } = {}): Request {
  return new Request("https://console.example.com/api/playground/stream", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
    ...(init.signal === undefined ? {} : { signal: init.signal }),
  });
}

async function json(response: Response): Promise<Record<string, unknown>> {
  return (await response.json()) as Record<string, unknown>;
}

function errorOf(body: Record<string, unknown>): Record<string, unknown> {
  return body["error"] as Record<string, unknown>;
}

describe("POST /api/playground/stream — the session gate", () => {
  test("no session is a 401, and nothing reaches Moira", async () => {
    install({ session: null });
    const response = await PLAYGROUND_STREAM_POST(request({ prompt: "hello" }));
    expect(response.status).toBe(401);
    expect(stub.routes()).toEqual([]);
  });
});

describe("POST /api/playground/stream — body validation", () => {
  test("a blank prompt is a keyed 400, and nothing reaches Moira", async () => {
    const response = await PLAYGROUND_STREAM_POST(request({ prompt: "" }));
    expect(response.status).toBe(400);
    expect(stub.routes()).toEqual([]);
  });
});

describe("POST /api/playground/stream — the streaming happy path", () => {
  test("pipes the upstream SSE body through verbatim, parseable by the same client-side reader", async () => {
    const response = await PLAYGROUND_STREAM_POST(request({ prompt: "hello" }));
    expect(response.status).toBe(200);
    expect(response.headers.get("content-type")).toBe("text/event-stream");
    expect(response.headers.get("cache-control")).toBe("no-store");

    const envelopes = [];
    for await (const envelope of readSseEnvelopes(response.body!)) {
      envelopes.push(envelope);
    }
    expect(envelopes.map((e) => e.type)).toEqual([
      "response.created",
      "response.output_text.delta",
      "response.completed",
    ]);
    expect(envelopes[1]?.payload).toEqual({ text: "Hi" });
  });

  test("forwards the prompt as a user input_text message, same shape as the non-streaming route", async () => {
    await PLAYGROUND_STREAM_POST(request({ prompt: "hello", model: "gpt-test" }));
    expect(stub.bodyOf(RESPONSES_STREAM)).toEqual({
      input: [{ role: "user", content: [{ type: "input_text", text: "hello" }] }],
      route: null,
      provider: null,
      model: "gpt-test",
      temperature: null,
      max_output_tokens: null,
    });
  });

  test("the response never carries the console's system key, in body or headers", async () => {
    const response = await PLAYGROUND_STREAM_POST(request({ prompt: "hello" }));
    const bodyText = await response.text();
    expect(bodyText).not.toContain(SYSTEM_KEY);
    expect(response.headers.get("x-moira-system-key")).toBeNull();
  });
});

describe("POST /api/playground/stream — cancellation", () => {
  test("the browser's own AbortSignal is forwarded, unchanged, to the outbound Moira request", async () => {
    const controller = new AbortController();
    await PLAYGROUND_STREAM_POST(request({ prompt: "hello" }, { signal: controller.signal }));
    const sent = stub.requestsFor(RESPONSES_STREAM)[0];
    expect(sent?.signal).toBe(controller.signal);
  });
});

describe("POST /api/playground/stream — a non-OK upstream is reshaped into the keyed envelope, never piped through", () => {
  test("a scope refusal (403) is rendered as JSON with its own code and message_key, not as an SSE body", async () => {
    install({
      handlers: handlers({
        [RESPONSES_STREAM]: () => ({ status: 403, body: errorEnvelope("route_forbidden") }),
      }),
    });
    const response = await PLAYGROUND_STREAM_POST(request({ prompt: "hello", route: "general" }));
    expect(response.status).toBe(403);
    expect(response.headers.get("content-type")).not.toBe("text/event-stream");
    const error = errorOf(await json(response));
    expect(error["code"]).toBe("route_forbidden");
    // The client-safe `MoiraError` shape `moiraErrorBody` produces — the key
    // lives at `error.text.messageKey`, not `error.message_key`.
    expect((error["text"] as Record<string, unknown>)["messageKey"]).toBe("moira.error.route_forbidden");
  });

  test("a provider 502 is rendered with its own status and code, not a 500 with a stack", async () => {
    install({
      handlers: handlers({
        [RESPONSES_STREAM]: () => ({ status: 502, body: errorEnvelope("provider_upstream_error") }),
      }),
    });
    const response = await PLAYGROUND_STREAM_POST(request({ prompt: "hello" }));
    expect(response.status).toBe(502);
    expect(errorOf(await json(response))["code"]).toBe("provider_upstream_error");
  });
});
