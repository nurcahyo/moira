// `POST /api/playground/diagnose`, driven as a route handler. Same
// session-gate shape as `playground-run-route.test.ts`; this file's own job
// is the authorization-gated refusal shapes issue #261 asked to be rendered
// cleanly: a missing `moira:runtime:diagnose` scope (403) and a deployment
// with the endpoint disabled (404) — both must reach the operator as the
// keyed envelope, not a special-cased message and not a raw 500.

import { afterEach, beforeEach, describe, expect, test } from "bun:test";

import { POST as PLAYGROUND_DIAGNOSE_POST } from "@/app/api/playground/diagnose/route";
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

const RUNTIME_DIAGNOSE = "POST /api/v1/admin/runtime/diagnose";
const PROVIDER_CONFIG_ID = "moira-console-idp";

function diagnosticResponse(overrides: Record<string, unknown> = {}) {
  return {
    outcome: {
      request_id: "req_1",
      execution_id: "11111111-1111-4111-8111-111111111111",
      status: "succeeded",
      usage: { input_tokens: 10, output_tokens: 4, total_tokens: 14 },
      attempts: [],
      output_text: "Hello!",
    },
    events: [
      {
        request_id: "req_1",
        execution_id: "11111111-1111-4111-8111-111111111111",
        sequence: 0,
        timestamp: "2026-08-15T00:00:00Z",
        event_type: "candidate_ranked",
        payload: { candidates: [] },
      },
    ],
    ...overrides,
  };
}

function handlers(overrides: Record<string, StubHandler> = {}): Record<string, StubHandler> {
  return {
    [RUNTIME_DIAGNOSE]: () => ({ status: 200, body: diagnosticResponse() }),
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

function request(body?: unknown): Request {
  return new Request("https://console.example.com/api/playground/diagnose", {
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

describe("POST /api/playground/diagnose — the session gate", () => {
  test("no session is a 401, and nothing reaches Moira", async () => {
    install({ session: null });
    const response = await PLAYGROUND_DIAGNOSE_POST(request({ prompt: "hello" }));
    expect(response.status).toBe(401);
    expect(stub.routes()).toEqual([]);
  });
});

describe("POST /api/playground/diagnose — body validation", () => {
  test("a blank prompt is a keyed 400, and nothing reaches Moira", async () => {
    const response = await PLAYGROUND_DIAGNOSE_POST(request({ prompt: "" }));
    expect(response.status).toBe(400);
    expect(errorOf(await json(response))["message_key"]).toBe(CONSOLE_MESSAGE_KEYS.playground_prompt_required);
    expect(stub.routes()).toEqual([]);
  });
});

describe("POST /api/playground/diagnose — the happy path", () => {
  test("forwards prompt/route/provider/model/priority/complexity_hint, and returns the outcome+events verbatim", async () => {
    const response = await PLAYGROUND_DIAGNOSE_POST(
      request({
        prompt: "hello",
        route: "general",
        provider_id: "aaaaaaaa-1111-4111-8111-111111111111",
        provider_model_id: "bbbbbbbb-1111-4111-8111-111111111111",
        priority: 10,
        complexity_hint: "standard",
      }),
    );
    expect(response.status).toBe(200);
    expect(stub.bodyOf(RUNTIME_DIAGNOSE)).toEqual({
      prompt: "hello",
      route: "general",
      provider_id: "aaaaaaaa-1111-4111-8111-111111111111",
      provider_model_id: "bbbbbbbb-1111-4111-8111-111111111111",
      stream: false,
      options: { temperature: null, max_tokens: null, priority: 10, complexity_hint: "standard" },
    });
    const body = await json(response);
    expect((body["outcome"] as Record<string, unknown>)["output_text"]).toBe("Hello!");
    expect((body["events"] as unknown[])).toHaveLength(1);
  });
});

describe("POST /api/playground/diagnose — the two authorization-gated refusals issue #261 asked for", () => {
  test("missing moira:runtime:diagnose scope (403) renders the keyed envelope, not a raw provider body", async () => {
    install({
      handlers: handlers({
        [RUNTIME_DIAGNOSE]: () => ({ status: 403, body: errorEnvelope("forbidden") }),
      }),
    });
    const response = await PLAYGROUND_DIAGNOSE_POST(request({ prompt: "hello" }));
    expect(response.status).toBe(403);
    const error = errorOf(await json(response));
    expect(error["code"]).toBe("forbidden");
    // The client-safe `MoiraError` shape `moiraErrorBody` produces — the key
    // lives at `error.text.messageKey`, not `error.message_key`.
    expect((error["text"] as Record<string, unknown>)["messageKey"]).toBe("moira.error.forbidden");
  });

  test("the endpoint disabled on this deployment (404) renders the keyed envelope, not a 500", async () => {
    install({
      handlers: handlers({
        [RUNTIME_DIAGNOSE]: () => ({ status: 404, body: errorEnvelope("not_found") }),
      }),
    });
    const response = await PLAYGROUND_DIAGNOSE_POST(request({ prompt: "hello" }));
    expect(response.status).toBe(404);
    expect(errorOf(await json(response))["code"]).toBe("not_found");
  });

  test("priority set without moira:execution:override-priority is a 403 naming model_forbidden, not a silently-ignored value", async () => {
    install({
      handlers: handlers({
        [RUNTIME_DIAGNOSE]: () => ({ status: 403, body: errorEnvelope("model_forbidden") }),
      }),
    });
    const response = await PLAYGROUND_DIAGNOSE_POST(request({ prompt: "hello", priority: 999 }));
    expect(response.status).toBe(403);
    expect(errorOf(await json(response))["code"]).toBe("model_forbidden");
  });
});
