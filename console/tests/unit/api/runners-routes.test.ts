// `app/api/runners/**`, driven as route handlers — same shape as
// `tests/unit/api/claude-subscription-route.test.ts`: the session gate is
// real, only the environment/runtime/Moira transport are substituted, so a
// deleted `withConsoleSession(` call in any handler fails these tests, not
// merely the architecture scan.
//
// `lib/runners.ts`'s own retry/re-read logic is exercised directly in
// `tests/unit/lib/runners.test.ts`; this file is about the HTTP surface each
// handler puts around it — body validation, status codes, and that a Moira
// refusal (including `409 runner_token_unavailable`) reaches the browser
// unmodified.

import { afterEach, beforeEach, describe, expect, test } from "bun:test";

import { DELETE as RUNNER_DELETE, GET as RUNNER_GET } from "@/app/api/runners/[id]/route";
import { POST as AUTHORIZATION_CODE_POST } from "@/app/api/runners/[id]/authorization-code/route";
import { POST as FINALIZE_POST } from "@/app/api/runners/[id]/finalize/route";
import { GET as RUNNERS_LIST_GET, POST as RUNNERS_PROVISION_POST } from "@/app/api/runners/route";
import type { ResolvedAuthConfig } from "@/lib/auth-config";
import type { ConsoleAuth } from "@/lib/auth";
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

const RUNNER_ID = "aaaaaaaa-1111-4111-8111-111111111111";
const PROVIDER_CONFIG_ID = "moira-console-idp";

function page(rows: readonly unknown[], hasMore = false) {
  return { data: rows, pagination: { has_more: hasMore, next_cursor: null } };
}

function runnerRecord(overrides: Record<string, unknown> = {}) {
  return {
    id: RUNNER_ID,
    label: "claude-1",
    runner_reference: "runner-ref-1",
    state: "provisioning",
    scope: { type: "global" },
    metadata: {},
    created_at: "2026-08-16T00:00:00Z",
    updated_at: "2026-08-16T00:00:00Z",
    version: 1,
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
        findAccountByUserId: async () => [
          { providerId: PROVIDER_CONFIG_ID, accountId: "idp-subject-abc" },
        ],
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
  } = {},
): void {
  stub = createMoiraStub(options.handlers ?? {});
  const auth = authWith("session" in options ? options.session : ADMITTED);
  const runtime: ConsoleRuntime = {
    ok: true,
    auth,
    configs: [CONFIG],
    problems: [],
    stale: false,
  };
  setConsoleApiDependenciesForTests({
    runtime: async () => runtime,
    env: () => ENV,
    clientFor: () =>
      new MoiraClient({ baseUrl: MOIRA_STUB_BASE_URL, systemKey: "sk_test_stub", fetch: stub.fetch }),
  });
}

beforeEach(() => {
  install();
});

afterEach(() => {
  setConsoleApiDependenciesForTests(null);
});

function request(
  url: string,
  init: Omit<RequestInit, "body"> & { readonly body?: unknown } = {},
): Request {
  const { body, ...rest } = init;
  return new Request(url, {
    ...rest,
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

/* -------------------------------------------------------------------------- */
/* GET/POST /api/runners                                                      */
/* -------------------------------------------------------------------------- */

describe("GET /api/runners", () => {
  test("no session is a 401, and nothing reaches Moira", async () => {
    install({ session: null });
    const response = await RUNNERS_LIST_GET(request("https://console.example.com/api/runners"));
    expect(response.status).toBe(401);
    expect(stub.routes()).toEqual([]);
  });

  test("forwards limit and cursor, and answers with cache-control: no-store", async () => {
    install({
      handlers: { "GET /api/v1/admin/runners": () => ({ status: 200, body: page([runnerRecord()]) }) },
    });
    const response = await RUNNERS_LIST_GET(
      request("https://console.example.com/api/runners?limit=10&cursor=abc"),
    );
    expect(response.status).toBe(200);
    expect(response.headers.get("cache-control")).toBe("no-store");
    expect(stub.requests[0]?.url).toContain("limit=10");
    expect(stub.requests[0]?.url).toContain("cursor=abc");
    expect((await json(response))["data"]).toEqual([runnerRecord()]);
  });
});

describe("POST /api/runners", () => {
  test("a missing body is a keyed 400, and nothing reaches Moira", async () => {
    const response = await RUNNERS_PROVISION_POST(request("https://console.example.com/api/runners"));
    expect(response.status).toBe(400);
    expect(errorOf(await json(response))["message_key"]).toBe(
      CONSOLE_MESSAGE_KEYS.runners_request_body_invalid,
    );
    expect(stub.routes()).toEqual([]);
  });

  test("a label outside the runner-service charset is a keyed 400, and nothing reaches Moira", async () => {
    const response = await RUNNERS_PROVISION_POST(
      request("https://console.example.com/api/runners", { method: "POST", body: { label: "Not Valid!" } }),
    );
    expect(response.status).toBe(400);
    expect(errorOf(await json(response))["message_key"]).toBe(
      CONSOLE_MESSAGE_KEYS.runners_provision_label_required,
    );
    expect(stub.routes()).toEqual([]);
  });

  test("a ttl_seconds outside 60..3600 is a keyed 400, and nothing reaches Moira", async () => {
    const response = await RUNNERS_PROVISION_POST(
      request("https://console.example.com/api/runners", {
        method: "POST",
        body: { label: "claude-1", ttl_seconds: 30 },
      }),
    );
    expect(response.status).toBe(400);
    expect(errorOf(await json(response))["message_key"]).toBe(
      CONSOLE_MESSAGE_KEYS.runners_provision_ttl_invalid,
    );
    expect(stub.routes()).toEqual([]);
  });

  test("an unrecognised scope shape is a keyed 400, and nothing reaches Moira", async () => {
    const response = await RUNNERS_PROVISION_POST(
      request("https://console.example.com/api/runners", {
        method: "POST",
        body: { label: "claude-1", scope: { type: "tenant" } },
      }),
    );
    expect(response.status).toBe(400);
    expect(errorOf(await json(response))["message_key"]).toBe(
      CONSOLE_MESSAGE_KEYS.runners_provision_scope_invalid,
    );
    expect(stub.routes()).toEqual([]);
  });

  test("a valid label provisions with a deterministic idempotency key, at 201", async () => {
    install({
      handlers: {
        "POST /api/v1/admin/runners": () => ({ status: 201, body: runnerRecord() }),
      },
    });
    const response = await RUNNERS_PROVISION_POST(
      request("https://console.example.com/api/runners", { method: "POST", body: { label: "claude-1" } }),
    );
    expect(response.status).toBe(201);
    expect(stub.requestsFor("POST /api/v1/admin/runners")[0]?.headers["Idempotency-Key"]).toBe(
      "runner-provision:claude-1",
    );
    expect(await json(response)).toEqual(runnerRecord());
  });

  test("a tenant scope is forwarded to Moira verbatim", async () => {
    install({
      handlers: {
        "POST /api/v1/admin/runners": () => ({
          status: 201,
          body: runnerRecord({ scope: { type: "tenant", external_tenant_id: "acme" } }),
        }),
      },
    });
    await RUNNERS_PROVISION_POST(
      request("https://console.example.com/api/runners", {
        method: "POST",
        body: { label: "claude-acme", scope: { type: "tenant", external_tenant_id: "acme" } },
      }),
    );
    expect(stub.bodyOf("POST /api/v1/admin/runners")).toEqual({
      label: "claude-acme",
      scope: { type: "tenant", external_tenant_id: "acme" },
    });
  });

  test("a Moira refusal (e.g. duplicate label) is rendered by the existing MoiraRequestError path", async () => {
    install({
      handlers: {
        "POST /api/v1/admin/runners": () => ({
          status: 409,
          body: errorEnvelope("duplicate_runner_label"),
        }),
      },
    });
    const response = await RUNNERS_PROVISION_POST(
      request("https://console.example.com/api/runners", { method: "POST", body: { label: "claude-1" } }),
    );
    expect(response.status).toBe(409);
    expect(errorOf(await json(response))["code"]).toBe("duplicate_runner_label");
  });
});

/* -------------------------------------------------------------------------- */
/* GET/DELETE /api/runners/[id]                                               */
/* -------------------------------------------------------------------------- */

function params(id: string): { params: Promise<{ id: string }> } {
  return { params: Promise.resolve({ id }) };
}

describe("GET /api/runners/[id]", () => {
  test("no session is a 401, and nothing reaches Moira", async () => {
    install({ session: null });
    const response = await RUNNER_GET(
      request(`https://console.example.com/api/runners/${RUNNER_ID}`),
      params(RUNNER_ID),
    );
    expect(response.status).toBe(401);
    expect(stub.routes()).toEqual([]);
  });

  test("returns the refreshed record with cache-control: no-store", async () => {
    install({
      handlers: {
        [`GET /api/v1/admin/runners/${RUNNER_ID}`]: () => ({
          status: 200,
          body: runnerRecord({ state: "awaiting_authorization", authorization_url: "https://claude.com/x" }),
        }),
      },
    });
    const response = await RUNNER_GET(
      request(`https://console.example.com/api/runners/${RUNNER_ID}`),
      params(RUNNER_ID),
    );
    expect(response.status).toBe(200);
    expect(response.headers.get("cache-control")).toBe("no-store");
    expect((await json(response))["state"]).toBe("awaiting_authorization");
  });
});

describe("DELETE /api/runners/[id]", () => {
  test("re-reads for a fresh version, deletes, and answers 204", async () => {
    install({
      handlers: {
        [`GET /api/v1/admin/runners/${RUNNER_ID}`]: () => ({ status: 200, body: runnerRecord({ version: 9 }) }),
        [`DELETE /api/v1/admin/runners/${RUNNER_ID}`]: (req) => {
          expect(req.headers["If-Match"]).toBe("9");
          return { status: 204 };
        },
      },
    });
    const response = await RUNNER_DELETE(
      request(`https://console.example.com/api/runners/${RUNNER_ID}`, { method: "DELETE" }),
      params(RUNNER_ID),
    );
    expect(response.status).toBe(204);
  });

  test("a runner already gone is a keyed 404", async () => {
    install({
      handlers: {
        [`GET /api/v1/admin/runners/${RUNNER_ID}`]: () => ({
          status: 404,
          body: errorEnvelope("runner_not_found"),
        }),
      },
    });
    const response = await RUNNER_DELETE(
      request(`https://console.example.com/api/runners/${RUNNER_ID}`, { method: "DELETE" }),
      params(RUNNER_ID),
    );
    expect(response.status).toBe(404);
    expect(errorOf(await json(response))["message_key"]).toBe(
      CONSOLE_MESSAGE_KEYS.runners_delete_not_found,
    );
  });

  test("repeated version conflicts exhaust the retry and answer a keyed 409", async () => {
    install({
      handlers: {
        [`GET /api/v1/admin/runners/${RUNNER_ID}`]: () => ({ status: 200, body: runnerRecord() }),
        [`DELETE /api/v1/admin/runners/${RUNNER_ID}`]: () => ({
          status: 409,
          body: errorEnvelope("resource_version_conflict"),
        }),
      },
    });
    const response = await RUNNER_DELETE(
      request(`https://console.example.com/api/runners/${RUNNER_ID}`, { method: "DELETE" }),
      params(RUNNER_ID),
    );
    expect(response.status).toBe(409);
    expect(errorOf(await json(response))["message_key"]).toBe(
      CONSOLE_MESSAGE_KEYS.runners_delete_conflict_exhausted,
    );
  });
});

/* -------------------------------------------------------------------------- */
/* POST /api/runners/[id]/authorization-code                                  */
/* -------------------------------------------------------------------------- */

describe("POST /api/runners/[id]/authorization-code", () => {
  test("an empty code is a keyed 400, and nothing reaches Moira", async () => {
    const response = await AUTHORIZATION_CODE_POST(
      request(`https://console.example.com/api/runners/${RUNNER_ID}/authorization-code`, {
        method: "POST",
        body: { code: "  " },
      }),
      params(RUNNER_ID),
    );
    expect(response.status).toBe(400);
    expect(errorOf(await json(response))["message_key"]).toBe(
      CONSOLE_MESSAGE_KEYS.runners_authorization_code_required,
    );
    expect(stub.routes()).toEqual([]);
  });

  test("forwards the code and returns the updated record", async () => {
    install({
      handlers: {
        [`POST /api/v1/admin/runners/${RUNNER_ID}/authorization-code`]: () => ({
          status: 200,
          body: runnerRecord({ state: "exchanging" }),
        }),
      },
    });
    const response = await AUTHORIZATION_CODE_POST(
      request(`https://console.example.com/api/runners/${RUNNER_ID}/authorization-code`, {
        method: "POST",
        body: { code: "auth-code-xyz" },
      }),
      params(RUNNER_ID),
    );
    expect(response.status).toBe(200);
    expect(stub.bodyOf(`POST /api/v1/admin/runners/${RUNNER_ID}/authorization-code`)).toEqual({
      code: "auth-code-xyz",
    });
    expect((await json(response))["state"]).toBe("exchanging");
  });
});

/* -------------------------------------------------------------------------- */
/* POST /api/runners/[id]/finalize                                            */
/* -------------------------------------------------------------------------- */

describe("POST /api/runners/[id]/finalize", () => {
  test("a body carrying scope is a keyed 400, and nothing reaches Moira", async () => {
    const response = await FINALIZE_POST(
      request(`https://console.example.com/api/runners/${RUNNER_ID}/finalize`, {
        method: "POST",
        body: { provider_id: "prov-1", scope: { type: "global" } },
      }),
      params(RUNNER_ID),
    );
    expect(response.status).toBe(400);
    expect(errorOf(await json(response))["message_key"]).toBe(
      CONSOLE_MESSAGE_KEYS.runners_finalize_scope_forbidden,
    );
    expect(stub.routes()).toEqual([]);
  });

  test("a missing provider_id is a keyed 400, and nothing reaches Moira", async () => {
    const response = await FINALIZE_POST(
      request(`https://console.example.com/api/runners/${RUNNER_ID}/finalize`, {
        method: "POST",
        body: {},
      }),
      params(RUNNER_ID),
    );
    expect(response.status).toBe(400);
    expect(errorOf(await json(response))["message_key"]).toBe(
      CONSOLE_MESSAGE_KEYS.runners_finalize_provider_id_required,
    );
    expect(stub.routes()).toEqual([]);
  });

  test("finalizes successfully and returns the linked record", async () => {
    install({
      handlers: {
        [`POST /api/v1/admin/runners/${RUNNER_ID}/finalize`]: () => ({
          status: 200,
          body: runnerRecord({ state: "linked", credential_id: "cred-1", provider_id: "prov-1" }),
        }),
      },
    });
    const response = await FINALIZE_POST(
      request(`https://console.example.com/api/runners/${RUNNER_ID}/finalize`, {
        method: "POST",
        body: { provider_id: "prov-1" },
      }),
      params(RUNNER_ID),
    );
    expect(response.status).toBe(200);
    expect(stub.bodyOf(`POST /api/v1/admin/runners/${RUNNER_ID}/finalize`)).toEqual({
      provider_id: "prov-1",
    });
    const body = await json(response);
    expect(body["state"]).toBe("linked");
    // No token-shaped field anywhere in the response.
    expect(JSON.stringify(body)).not.toMatch(/token/i);
  });

  test("409 runner_token_unavailable reaches the browser unmodified — the caller must not retry it", async () => {
    install({
      handlers: {
        [`POST /api/v1/admin/runners/${RUNNER_ID}/finalize`]: () => ({
          status: 409,
          body: errorEnvelope("runner_token_unavailable"),
        }),
      },
    });
    const response = await FINALIZE_POST(
      request(`https://console.example.com/api/runners/${RUNNER_ID}/finalize`, {
        method: "POST",
        body: { provider_id: "prov-1" },
      }),
      params(RUNNER_ID),
    );
    expect(response.status).toBe(409);
    expect(errorOf(await json(response))["code"]).toBe("runner_token_unavailable");
  });
});
