// `GET /api/playground/providers/{id}/models`, driven as a route handler —
// the model picker's dependent second level.

import { afterEach, beforeEach, describe, expect, test } from "bun:test";

import { GET as PLAYGROUND_PROVIDER_MODELS_GET } from "@/app/api/playground/providers/[id]/models/route";
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

const PROVIDER_ID = "aaaaaaaa-1111-4111-8111-111111111111";
const LIST_MODELS = `GET /api/v1/admin/providers/${PROVIDER_ID}/models`;
const PROVIDER_CONFIG_ID = "moira-console-idp";

function page(rows: readonly unknown[]) {
  return { data: rows, pagination: { has_more: false, next_cursor: null } };
}

function modelRecord(overrides: Record<string, unknown> = {}) {
  return {
    id: "bbbbbbbb-1111-4111-8111-111111111111",
    provider_id: PROVIDER_ID,
    model_key: "gpt-test",
    display_name: "GPT Test",
    capabilities: {},
    status: "active",
    created_at: "2026-08-15T00:00:00Z",
    updated_at: "2026-08-15T00:00:00Z",
    version: 1,
    ...overrides,
  };
}

function handlers(overrides: Record<string, StubHandler> = {}): Record<string, StubHandler> {
  return {
    [LIST_MODELS]: () => ({ status: 200, body: page([modelRecord()]) }),
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

function invoke(): Promise<Response> {
  return PLAYGROUND_PROVIDER_MODELS_GET(
    new Request(`https://console.example.com/api/playground/providers/${PROVIDER_ID}/models`),
    { params: Promise.resolve({ id: PROVIDER_ID }) },
  );
}

async function json(response: Response): Promise<Record<string, unknown>> {
  return (await response.json()) as Record<string, unknown>;
}

describe("GET /api/playground/providers/{id}/models — the session gate", () => {
  test("no session is a 401, and nothing reaches Moira", async () => {
    install({ session: null });
    const response = await invoke();
    expect(response.status).toBe(401);
    expect(stub.routes()).toEqual([]);
  });
});

describe("GET /api/playground/providers/{id}/models — the happy path", () => {
  test("returns the model list verbatim, uncached", async () => {
    const response = await invoke();
    expect(response.status).toBe(200);
    expect(response.headers.get("cache-control")).toBe("no-store");
    const body = await json(response);
    expect((body["data"] as unknown[]).map((row) => (row as Record<string, unknown>)["model_key"])).toEqual([
      "gpt-test",
    ]);
  });
});

describe("GET /api/playground/providers/{id}/models — a Moira refusal renders as the keyed envelope", () => {
  test("a not-found provider id is a keyed 404", async () => {
    install({ handlers: handlers({ [LIST_MODELS]: () => ({ status: 404, body: errorEnvelope("not_found") }) }) });
    const response = await invoke();
    expect(response.status).toBe(404);
    expect(((await json(response))["error"] as Record<string, unknown>)["code"]).toBe("not_found");
  });
});
