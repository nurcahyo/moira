// `POST /api/settings/llm/claude-api-key` — Mode B, driven as a route
// handler. Same shape as `claude-subscription-route.test.ts`: the session
// gate is real, only the environment/runtime/Moira transport are substituted.

import { afterEach, beforeEach, describe, expect, test } from "bun:test";

import { POST as API_KEY_POST } from "@/app/api/settings/llm/claude-api-key/route";
import {
  CLAUDE_API_KEY_PROVIDER_DISPLAY_NAME,
  CLAUDE_SUBSCRIPTION_PROVIDER_TYPE,
} from "@/lib/claude-subscription";
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

/* -------------------------------------------------------------------------- */
/* Fixtures                                                                   */
/* -------------------------------------------------------------------------- */

const PROVIDER_ID = "cccccccc-1111-4111-8111-111111111111";
const CREDENTIAL_ID = "dddddddd-1111-4111-8111-111111111111";
const PROVIDER_CONFIG_ID = "moira-console-idp";

/** Unmistakable, and asserted absent from every response body. */
const API_KEY = "sk-ant-unmistakable-console-api-key-4f9c2b";

const PROVIDER_LIST = "GET /api/v1/admin/providers";
const PROVIDER_CREATE = "POST /api/v1/admin/providers";
const CREDENTIAL_LIST = "GET /api/v1/admin/provider-credentials";
const CREDENTIAL_CREATE = "POST /api/v1/admin/provider-credentials";
const CREDENTIAL_ROTATE = `POST /api/v1/admin/provider-credentials/${CREDENTIAL_ID}/rotate`;

function page(rows: readonly unknown[], hasMore = false) {
  return { data: rows, pagination: { has_more: hasMore, next_cursor: null } };
}

function providerRecord(overrides: Record<string, unknown> = {}) {
  return {
    id: PROVIDER_ID,
    provider_type: CLAUDE_SUBSCRIPTION_PROVIDER_TYPE,
    display_name: CLAUDE_API_KEY_PROVIDER_DISPLAY_NAME,
    status: "active",
    metadata: {},
    created_at: "2026-08-14T00:00:00Z",
    updated_at: "2026-08-14T00:00:00Z",
    version: 1,
    ...overrides,
  };
}

function credentialRecord(overrides: Record<string, unknown> = {}) {
  return {
    id: CREDENTIAL_ID,
    provider_id: PROVIDER_ID,
    credential_type: "api_key",
    scope: { type: "global" },
    secret_fingerprint: "sha256:fingerprint-that-must-not-cross",
    masked_secret: "api_key****mask",
    status: "active",
    priority: 0,
    metadata: {},
    created_at: "2026-08-14T00:00:00Z",
    updated_at: "2026-08-14T00:00:00Z",
    version: 1,
    ...overrides,
  };
}

function handlers(overrides: Record<string, StubHandler> = {}): Record<string, StubHandler> {
  return {
    [PROVIDER_LIST]: () => ({ status: 200, body: page([]) }),
    [PROVIDER_CREATE]: () => ({ status: 201, body: providerRecord() }),
    [CREDENTIAL_LIST]: () => ({ status: 200, body: page([]) }),
    [CREDENTIAL_CREATE]: () => ({ status: 201, body: credentialRecord() }),
    [CREDENTIAL_ROTATE]: () => ({ status: 200, body: credentialRecord({ version: 2 }) }),
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
    clientFor: () =>
      new MoiraClient({
        baseUrl: MOIRA_STUB_BASE_URL,
        systemKey: "sk_test_stub",
        fetch: stub.fetch,
      }),
  });
}

beforeEach(() => {
  install();
});

afterEach(() => {
  setConsoleApiDependenciesForTests(null);
});

function request(body?: unknown): Request {
  return new Request("https://console.example.com/api/settings/llm/claude-api-key", {
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

/* -------------------------------------------------------------------------- */
/* The session gate                                                           */
/* -------------------------------------------------------------------------- */

describe("POST /api/settings/llm/claude-api-key — the session gate", () => {
  test("no session is a 401, and nothing reaches Moira", async () => {
    install({ session: null });
    const response = await API_KEY_POST(request({ api_key: API_KEY }));
    expect(response.status).toBe(401);
    expect(errorOf(await json(response))["code"]).toBe("no_session");
    expect(stub.routes()).toEqual([]);
  });

  test("no response on this surface is cacheable", async () => {
    const response = await API_KEY_POST(request({ api_key: API_KEY }));
    expect(response.headers.get("cache-control")).toBe("no-store");
  });
});

/* -------------------------------------------------------------------------- */
/* Body validation                                                            */
/* -------------------------------------------------------------------------- */

describe("POST /api/settings/llm/claude-api-key — body validation", () => {
  test("a missing body is a keyed 400, and nothing reaches Moira", async () => {
    const response = await API_KEY_POST(request());
    expect(response.status).toBe(400);
    expect(errorOf(await json(response))["message_key"]).toBe(
      CONSOLE_MESSAGE_KEYS.claude_api_key_request_body_invalid,
    );
    expect(stub.routes()).toEqual([]);
  });

  test("an empty key is a keyed 400, and nothing reaches Moira", async () => {
    const response = await API_KEY_POST(request({ api_key: "" }));
    expect(response.status).toBe(400);
    expect(errorOf(await json(response))["message_key"]).toBe(
      CONSOLE_MESSAGE_KEYS.claude_api_key_required,
    );
    expect(stub.routes()).toEqual([]);
  });

  test("a key with the wrong prefix is a keyed 400, and nothing reaches Moira", async () => {
    const response = await API_KEY_POST(request({ api_key: "sk-proj-not-anthropic" }));
    expect(response.status).toBe(400);
    expect(errorOf(await json(response))["message_key"]).toBe(
      CONSOLE_MESSAGE_KEYS.claude_api_key_wrong_shape,
    );
    expect(stub.routes()).toEqual([]);
  });

  test("a key carrying a newline is a keyed 400, and nothing reaches Moira", async () => {
    const response = await API_KEY_POST(request({ api_key: `${API_KEY}\nsecond-line` }));
    expect(response.status).toBe(400);
    expect(errorOf(await json(response))["message_key"]).toBe(CONSOLE_MESSAGE_KEYS.claude_api_key_invalid);
    expect(stub.routes()).toEqual([]);
  });
});

/* -------------------------------------------------------------------------- */
/* The key goes one way                                                      */
/* -------------------------------------------------------------------------- */

describe("POST /api/settings/llm/claude-api-key — the key goes one way", () => {
  test("first save: creates the DEDICATED api_key provider and credential, and the response carries no secret", async () => {
    const response = await API_KEY_POST(request({ api_key: API_KEY }));
    expect(response.status).toBe(200);

    const providerSent = stub.bodyOf(PROVIDER_CREATE) as Record<string, unknown>;
    expect(providerSent["display_name"]).toBe(CLAUDE_API_KEY_PROVIDER_DISPLAY_NAME);

    const sent = stub.bodyOf(CREDENTIAL_CREATE) as Record<string, unknown>;
    expect(sent["credential_type"]).toBe("api_key");
    expect((sent["secret"] as Record<string, unknown>)["api_key"]).toBe(API_KEY);

    const body = await json(response);
    const bodyText = JSON.stringify(body);
    expect(bodyText).not.toContain(API_KEY);
    expect(bodyText).not.toContain("sha256:fingerprint-that-must-not-cross");
    expect(bodyText).not.toContain("api_key****mask");
    expect(body).toEqual({ provider_id: PROVIDER_ID, credential_id: CREDENTIAL_ID, outcome: "created" });
  });

  test("second save: rotates the existing credential rather than duplicating it", async () => {
    install({
      handlers: handlers({ [CREDENTIAL_LIST]: () => ({ status: 200, body: page([credentialRecord()]) }) }),
    });
    const response = await API_KEY_POST(request({ api_key: API_KEY }));
    expect(response.status).toBe(200);
    expect(stub.routes()).not.toContain(CREDENTIAL_CREATE);
    expect(stub.routes()).toContain(CREDENTIAL_ROTATE);
    expect((await json(response))["outcome"]).toBe("rotated");
  });

  test("an oauth2 credential on the same provider is ignored; an api_key one is created alongside it", async () => {
    install({
      handlers: handlers({
        [CREDENTIAL_LIST]: () => ({
          status: 200,
          body: page([credentialRecord({ id: "eeeeeeee-0000-4000-8000-000000000000", credential_type: "oauth2" })]),
        }),
      }),
    });
    const response = await API_KEY_POST(request({ api_key: API_KEY }));
    expect(response.status).toBe(200);
    expect(stub.routes()).toContain(CREDENTIAL_CREATE);
    expect(stub.routes()).not.toContain(CREDENTIAL_ROTATE);
  });
});

/* -------------------------------------------------------------------------- */
/* Refusals surfaced from the chain and from Moira                            */
/* -------------------------------------------------------------------------- */

describe("POST /api/settings/llm/claude-api-key — refusals", () => {
  test("a truncated provider list is a keyed 409, not a 500", async () => {
    install({ handlers: handlers({ [PROVIDER_LIST]: () => ({ status: 200, body: page([], true) }) }) });
    const response = await API_KEY_POST(request({ api_key: API_KEY }));
    expect(response.status).toBe(409);
    const body = errorOf(await json(response));
    expect(body["code"]).toBe("claude_api_key_failed");
    expect(body["message_key"]).toBe(CONSOLE_MESSAGE_KEYS.claude_subscription_list_truncated);
  });

  test("a Moira refusal (e.g. no admin grant) is rendered by the existing MoiraRequestError path", async () => {
    install({
      handlers: handlers({ [PROVIDER_LIST]: () => ({ status: 403, body: errorEnvelope("missing_scope") }) }),
    });
    const response = await API_KEY_POST(request({ api_key: API_KEY }));
    expect(response.status).toBe(403);
    expect(errorOf(await json(response))["code"]).toBe("missing_scope");
  });
});
