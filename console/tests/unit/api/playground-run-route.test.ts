// `POST /api/playground/run`, driven as a route handler — same shape as
// `tests/unit/api/claude-subscription-route.test.ts`: the session gate is
// real, only the environment/runtime/Moira transport are substituted.

import { afterEach, beforeEach, describe, expect, test } from "bun:test";

import { POST as PLAYGROUND_RUN_POST } from "@/app/api/playground/run/route";
import type { ConsoleAuth } from "@/lib/auth";
import type { ResolvedAuthConfig } from "@/lib/auth-config";
import type { ConsoleRuntime } from "@/lib/auth-runtime";
import { setConsoleApiDependenciesForTests } from "@/lib/console-api";
import { readConsoleEnv, type ConsoleEnv } from "@/lib/env";
import { CONSOLE_MESSAGE_KEYS } from "@/lib/i18n/keys";
import { MoiraClient } from "@/lib/moira-client";
import {
  createMoiraStub,
  errorEnvelope,
  MOIRA_STUB_BASE_URL,
  type MoiraStub,
  type StubHandler,
} from "../../support/moira-stub";

const RESPONSES_CREATE = "POST /api/v1/responses";
const PROVIDER_CONFIG_ID = "moira-console-idp";

/** Unmistakable, and asserted absent from every response body. */
const SYSTEM_KEY = "sk_test_unmistakable_playground_system_key_9f3c";

function publicResponse(overrides: Record<string, unknown> = {}) {
  return {
    id: "resp_1",
    object: "response",
    created_at: "2026-08-15T00:00:00Z",
    status: "completed",
    execution_id: "exec_1",
    request_id: "req_1",
    output: [{ type: "message", role: "assistant", content: [{ type: "output_text", text: "Hello!" }] }],
    citations: [],
    usage: { input_tokens: 10, output_tokens: 4, total_tokens: 14 },
    metadata: {},
    output_persisted: false,
    ...overrides,
  };
}

function handlers(overrides: Record<string, StubHandler> = {}): Record<string, StubHandler> {
  return {
    [RESPONSES_CREATE]: () => ({ status: 200, body: publicResponse() }),
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

function request(body?: unknown): Request {
  return new Request("https://console.example.com/api/playground/run", {
    method: "POST",
    ...(body === undefined
      ? {}
      : { headers: { "content-type": "application/json" }, body: JSON.stringify(body) }),
  });
}

async function json(response: Response): Promise<Record<string, unknown>> {
  return (await response.json()) as Record<string, unknown>;
}

function errorOf(body: Record<string, unknown>): Record<string, unknown> {
  return body["error"] as Record<string, unknown>;
}

describe("POST /api/playground/run — the session gate", () => {
  test("no session is a 401, and nothing reaches Moira", async () => {
    install({ session: null });
    const response = await PLAYGROUND_RUN_POST(request({ prompt: "hello" }));
    expect(response.status).toBe(401);
    expect(stub.routes()).toEqual([]);
  });

  test("no response on this surface is cacheable", async () => {
    const response = await PLAYGROUND_RUN_POST(request({ prompt: "hello" }));
    expect(response.headers.get("cache-control")).toBe("no-store");
  });
});

describe("POST /api/playground/run — body validation", () => {
  test("a missing body is a keyed 400, and nothing reaches Moira", async () => {
    const response = await PLAYGROUND_RUN_POST(request());
    expect(response.status).toBe(400);
    expect(errorOf(await json(response))["message_key"]).toBe(
      CONSOLE_MESSAGE_KEYS.playground_request_body_invalid,
    );
    expect(stub.routes()).toEqual([]);
  });

  test("a blank prompt is a keyed 400, and nothing reaches Moira", async () => {
    const response = await PLAYGROUND_RUN_POST(request({ prompt: "   " }));
    expect(response.status).toBe(400);
    expect(errorOf(await json(response))["message_key"]).toBe(CONSOLE_MESSAGE_KEYS.playground_prompt_required);
    expect(stub.routes()).toEqual([]);
  });
});

describe("POST /api/playground/run — the happy path", () => {
  test("forwards the prompt wrapped as a user input_text message, and returns the response verbatim", async () => {
    const response = await PLAYGROUND_RUN_POST(
      request({ prompt: "hello", route: "general", temperature: 0.5 }),
    );
    expect(response.status).toBe(200);
    expect(stub.routes()).toEqual([RESPONSES_CREATE]);
    expect(stub.bodyOf(RESPONSES_CREATE)).toEqual({
      input: [{ role: "user", content: [{ type: "input_text", text: "hello" }] }],
      route: "general",
      provider: null,
      model: null,
      temperature: 0.5,
      max_output_tokens: null,
    });
    expect((await json(response))["execution_id"]).toBe("exec_1");
  });

  test("the response body never carries the console's system key", async () => {
    const response = await PLAYGROUND_RUN_POST(request({ prompt: "hello" }));
    const bodyText = JSON.stringify(await json(response));
    expect(bodyText).not.toContain(SYSTEM_KEY);
    expect(response.headers.get("x-moira-system-key")).toBeNull();
  });
});

describe("POST /api/playground/run — a Moira refusal renders as the keyed envelope, not a raw body", () => {
  test("a route-override refusal (403) is rendered with its own code and message_key", async () => {
    install({
      handlers: handlers({
        [RESPONSES_CREATE]: () => ({ status: 403, body: errorEnvelope("route_forbidden") }),
      }),
    });
    const response = await PLAYGROUND_RUN_POST(request({ prompt: "hello", route: "general" }));
    expect(response.status).toBe(403);
    const error = errorOf(await json(response));
    expect(error["code"]).toBe("route_forbidden");
    // The client-safe `MoiraError` shape `moiraErrorBody` produces, not Moira's
    // raw envelope — the key lives at `error.text.messageKey`, camelCase and
    // nested, not `error.message_key`.
    expect((error["text"] as Record<string, unknown>)["messageKey"]).toBe("moira.error.route_forbidden");
  });

  test("a provider 502 is rendered with its own status and code, not a 500 with a stack", async () => {
    install({
      handlers: handlers({
        [RESPONSES_CREATE]: () => ({ status: 502, body: errorEnvelope("provider_upstream_error") }),
      }),
    });
    const response = await PLAYGROUND_RUN_POST(request({ prompt: "hello" }));
    expect(response.status).toBe(502);
    expect(errorOf(await json(response))["code"]).toBe("provider_upstream_error");
  });
});
