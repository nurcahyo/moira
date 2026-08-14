// `POST /api/settings/llm/claude-subscription`, driven as a route handler —
// same shape as `tests/unit/api/llm-routes.test.ts`: the session gate is real,
// only the environment/runtime/Moira transport are substituted
// (`setConsoleApiDependenciesForTests`), so a deleted `withConsoleSession(`
// call in the handler fails these tests, not merely the architecture scan.
//
// Per the owner-approved testing policy
// (`plans/12-feature-expansion-brainstorm.md` §1), every assertion here runs
// against a MOCKED Moira client. No real subscription token, no real `claude`
// CLI, no network call — see `scripts/claude-subscription-sidecar-spike.sh`
// for the local-only, human-run counterpart that does use a real one.

import { afterEach, beforeEach, describe, expect, test } from "bun:test";

import { POST as CLAUDE_SUBSCRIPTION_POST } from "@/app/api/settings/llm/claude-subscription/route";
import {
  CLAUDE_SUBSCRIPTION_PROVIDER_DISPLAY_NAME,
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

const PROVIDER_ID = "aaaaaaaa-1111-4111-8111-111111111111";
const CREDENTIAL_ID = "bbbbbbbb-1111-4111-8111-111111111111";
const PROVIDER_CONFIG_ID = "moira-console-idp";

/** Unmistakable, and asserted absent from every response body. */
const TOKEN = "sk-ant-oat01-unmistakable-subscription-token-4f9c2b";

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
    display_name: CLAUDE_SUBSCRIPTION_PROVIDER_DISPLAY_NAME,
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
    credential_type: "oauth2",
    scope: { type: "global" },
    secret_fingerprint: "sha256:fingerprint-that-must-not-cross",
    masked_secret: "oauth2****mask",
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

/** Signed in, verified, inside the allow-list, with a linked account. */
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
  return new Request("https://console.example.com/api/settings/llm/claude-subscription", {
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

describe("POST /api/settings/llm/claude-subscription — the session gate", () => {
  test("no session is a 401, and nothing reaches Moira", async () => {
    install({ session: null });
    const response = await CLAUDE_SUBSCRIPTION_POST(request({ token: TOKEN }));
    expect(response.status).toBe(401);
    expect(errorOf(await json(response))["code"]).toBe("no_session");
    expect(stub.routes()).toEqual([]);
  });

  test("a session outside the allow-list is a 403", async () => {
    install({
      session: {
        session: { id: "session-2", providerId: PROVIDER_CONFIG_ID },
        user: { id: "user-2", email: "outsider@elsewhere.test", emailVerified: true },
      },
    });
    const response = await CLAUDE_SUBSCRIPTION_POST(request({ token: TOKEN }));
    expect(response.status).toBe(403);
    expect(stub.routes()).toEqual([]);
  });

  test("no response on this surface is cacheable", async () => {
    const response = await CLAUDE_SUBSCRIPTION_POST(request({ token: TOKEN }));
    expect(response.headers.get("cache-control")).toBe("no-store");
  });
});

/* -------------------------------------------------------------------------- */
/* Body validation                                                            */
/* -------------------------------------------------------------------------- */

describe("POST /api/settings/llm/claude-subscription — body validation", () => {
  test("a missing body is a keyed 400, and nothing reaches Moira", async () => {
    const response = await CLAUDE_SUBSCRIPTION_POST(request());
    expect(response.status).toBe(400);
    expect(errorOf(await json(response))["message_key"]).toBe(
      CONSOLE_MESSAGE_KEYS.claude_subscription_request_body_invalid,
    );
    expect(stub.routes()).toEqual([]);
  });

  test("an empty token is a keyed 400, and nothing reaches Moira", async () => {
    const response = await CLAUDE_SUBSCRIPTION_POST(request({ token: "" }));
    expect(response.status).toBe(400);
    expect(errorOf(await json(response))["message_key"]).toBe(
      CONSOLE_MESSAGE_KEYS.claude_subscription_token_required,
    );
    expect(stub.routes()).toEqual([]);
  });

  test("a token carrying a newline is a keyed 400, and nothing reaches Moira", async () => {
    const response = await CLAUDE_SUBSCRIPTION_POST(request({ token: `${TOKEN}\nsecond-line` }));
    expect(response.status).toBe(400);
    expect(errorOf(await json(response))["message_key"]).toBe(
      CONSOLE_MESSAGE_KEYS.claude_subscription_token_invalid,
    );
    expect(stub.routes()).toEqual([]);
  });
});

/* -------------------------------------------------------------------------- */
/* The secret goes one way                                                    */
/* -------------------------------------------------------------------------- */

describe("POST /api/settings/llm/claude-subscription — the token goes one way", () => {
  test("first save: creates the provider and credential, and the response carries no secret", async () => {
    const response = await CLAUDE_SUBSCRIPTION_POST(request({ token: TOKEN }));
    expect(response.status).toBe(200);

    // It went to Moira exactly once, as the oauth2 secret's access_token.
    const sent = stub.bodyOf(CREDENTIAL_CREATE) as Record<string, unknown>;
    expect((sent["secret"] as Record<string, unknown>)["access_token"]).toBe(TOKEN);

    // And came back in nothing: not the token, not Moira's own mask or
    // fingerprint for it.
    const body = await json(response);
    const bodyText = JSON.stringify(body);
    expect(bodyText).not.toContain(TOKEN);
    expect(bodyText).not.toContain("sha256:fingerprint-that-must-not-cross");
    expect(bodyText).not.toContain("oauth2****mask");

    expect(body).toEqual({
      provider_id: PROVIDER_ID,
      credential_id: CREDENTIAL_ID,
      outcome: "created",
    });
  });

  test("second save: rotates the existing credential rather than duplicating it", async () => {
    install({
      handlers: handlers({
        [CREDENTIAL_LIST]: () => ({ status: 200, body: page([credentialRecord()]) }),
      }),
    });
    const response = await CLAUDE_SUBSCRIPTION_POST(request({ token: TOKEN }));
    expect(response.status).toBe(200);
    expect(stub.routes()).not.toContain(CREDENTIAL_CREATE);
    expect(stub.routes()).toContain(CREDENTIAL_ROTATE);
    expect((await json(response))["outcome"]).toBe("rotated");
  });
});

/* -------------------------------------------------------------------------- */
/* Refusals surfaced from the chain and from Moira                            */
/* -------------------------------------------------------------------------- */

describe("POST /api/settings/llm/claude-subscription — refusals", () => {
  test("a truncated provider list is a keyed 409, not a 500", async () => {
    install({ handlers: handlers({ [PROVIDER_LIST]: () => ({ status: 200, body: page([], true) }) }) });
    const response = await CLAUDE_SUBSCRIPTION_POST(request({ token: TOKEN }));
    expect(response.status).toBe(409);
    const body = errorOf(await json(response));
    expect(body["code"]).toBe("claude_subscription_failed");
    expect(body["message_key"]).toBe(CONSOLE_MESSAGE_KEYS.claude_subscription_list_truncated);
  });

  test("a Moira refusal (e.g. no admin grant) is rendered by the existing MoiraRequestError path", async () => {
    install({
      handlers: handlers({
        [PROVIDER_LIST]: () => ({ status: 403, body: errorEnvelope("missing_scope") }),
      }),
    });
    const response = await CLAUDE_SUBSCRIPTION_POST(request({ token: TOKEN }));
    expect(response.status).toBe(403);
    expect(errorOf(await json(response))["code"]).toBe("missing_scope");
  });
});
