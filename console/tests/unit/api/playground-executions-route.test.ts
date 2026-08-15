// `GET /api/playground/executions/{executionId}`, driven as a route handler.

import { afterEach, beforeEach, describe, expect, test } from "bun:test";

import { GET as PLAYGROUND_EXECUTION_GET } from "@/app/api/playground/executions/[executionId]/route";
import type { ConsoleAuth } from "@/lib/auth";
import type { ResolvedAuthConfig } from "@/lib/auth-config";
import type { ConsoleRuntime } from "@/lib/auth-runtime";
import { setConsoleApiDependenciesForTests } from "@/lib/console-api";
import { readConsoleEnv, type ConsoleEnv } from "@/lib/env";
import { MoiraClient } from "@/lib/moira-client";
import {
  createMoiraStub,
  errorEnvelope,
  MOIRA_STUB_BASE_URL,
  type MoiraStub,
  type StubHandler,
} from "../../support/moira-stub";

const EXECUTION_ID = "exec_11111111-1111-4111-8111-111111111111";
const GET_EXECUTION = `GET /api/v1/executions/${EXECUTION_ID}`;
const PROVIDER_CONFIG_ID = "moira-console-idp";

function executionSummary(overrides: Record<string, unknown> = {}) {
  return {
    execution_id: EXECUTION_ID,
    response_id: "resp_1",
    request_id: "req_1",
    status: "completed",
    attempt_count: 2,
    usage: { input_tokens: 10, output_tokens: 4, total_tokens: 14 },
    latency_ms: 842,
    route: { id: "aaaaaaaa-1111-4111-8111-111111111111", key: "general" },
    model: { id: "bbbbbbbb-1111-4111-8111-111111111111", provider: "open_ai", key: "gpt-test" },
    ...overrides,
  };
}

function handlers(overrides: Record<string, StubHandler> = {}): Record<string, StubHandler> {
  return {
    [GET_EXECUTION]: () => ({ status: 200, body: executionSummary() }),
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
    clientFor: () => new MoiraClient({ baseUrl: MOIRA_STUB_BASE_URL, systemKey: "sk_test_stub", fetch: stub.fetch }),
  });
}

beforeEach(() => {
  install();
});

afterEach(() => {
  setConsoleApiDependenciesForTests(null);
});

function request(): Request {
  return new Request(`https://console.example.com/api/playground/executions/${EXECUTION_ID}`);
}

function invoke(): Promise<Response> {
  return PLAYGROUND_EXECUTION_GET(request(), { params: Promise.resolve({ executionId: EXECUTION_ID }) });
}

async function json(response: Response): Promise<Record<string, unknown>> {
  return (await response.json()) as Record<string, unknown>;
}

describe("GET /api/playground/executions/{executionId} — the session gate", () => {
  test("no session is a 401, and nothing reaches Moira", async () => {
    install({ session: null });
    const response = await invoke();
    expect(response.status).toBe(401);
    expect(stub.routes()).toEqual([]);
  });
});

describe("GET /api/playground/executions/{executionId} — the happy path", () => {
  test("returns the execution summary verbatim, uncached", async () => {
    const response = await invoke();
    expect(response.status).toBe(200);
    expect(response.headers.get("cache-control")).toBe("no-store");
    const body = await json(response);
    expect(body["attempt_count"]).toBe(2);
    expect(body["latency_ms"]).toBe(842);
  });
});

describe("GET /api/playground/executions/{executionId} — a Moira refusal renders as the keyed envelope", () => {
  test("a not-found execution id is a keyed 404", async () => {
    install({ handlers: handlers({ [GET_EXECUTION]: () => ({ status: 404, body: errorEnvelope("not_found") }) }) });
    const response = await invoke();
    expect(response.status).toBe(404);
    expect(((await json(response))["error"] as Record<string, unknown>)["code"]).toBe("not_found");
  });
});
