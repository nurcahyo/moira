// The `/api/llm/**` route handlers, driven as route handlers.
//
// ============================================================================
// WHY THESE IMPORT THE HANDLERS AND NOT THE LIBRARY BEHIND THEM
// ============================================================================
//
// `tests/unit/lib/llm-settings.test.ts` already proves the ORDER of the writes
// against a stub. What it cannot observe is whether anything calls
// `runConnectChain` at all, or whether a session was checked before it did —
// and that is finding F25's exact shape: `checkSession` shipped with eleven
// green unit assertions and no shipped caller.
//
// So every test below calls an exported HTTP handler.
//
// ============================================================================
// THE GATE IS REAL, NOT STUBBED
// ============================================================================
//
// `setConsoleApiDependenciesForTests` substitutes the environment, the runtime
// resolution and the Moira transport — three things a unit test cannot supply —
// and DELIBERATELY NOT `consoleSessionCheck`. The auth object handed in answers
// `getSession` from a fixture, and everything after that is the shipped code:
// `resolveSessionProviderIn` finds the config the session was stamped with,
// `readIdpSubject` reads the linked account, and `checkSession` applies the
// email-verification and domain rules.
//
// THE MUTATION THIS BUYS: delete `withConsoleSession(` from any handler and the
// "no session" test for it goes red, because the handler then acts on a request
// that carries no session at all.

import { afterEach, beforeEach, describe, expect, test } from "bun:test";

import { POST as CONNECT } from "@/app/api/llm/connect-vllm/route";
import { GET as PROVIDERS_GET, POST as PROVIDERS_POST } from "@/app/api/llm/providers/route";
import {
  DELETE as PROVIDER_DELETE,
  PATCH as PROVIDER_PATCH,
} from "@/app/api/llm/providers/[id]/route";
import { POST as MODEL_POST } from "@/app/api/llm/providers/[id]/models/route";
import { DELETE as MODEL_DELETE } from "@/app/api/llm/providers/[id]/models/[modelId]/route";
import { POST as CREDENTIAL_POST } from "@/app/api/llm/providers/[id]/credentials/route";
import { DELETE as CREDENTIAL_DELETE } from "@/app/api/llm/providers/[id]/credentials/[credentialId]/route";
import { POST as ROUTING_POST } from "@/app/api/llm/providers/[id]/routing/route";
import { DELETE as ROUTING_DELETE } from "@/app/api/llm/providers/[id]/routing/[policyId]/route";

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

const PROVIDER_ID = "11111111-1111-4111-8111-111111111111";
const MODEL_ID = "22222222-2222-4222-8222-222222222222";
const CREDENTIAL_ID = "33333333-3333-4333-8333-333333333333";
const ROUTE_ID = "44444444-4444-4444-8444-444444444444";
const POLICY_ID = "55555555-5555-4555-8555-555555555555";
const OTHER_PROVIDER_ID = "66666666-6666-4666-8666-666666666666";

const PROVIDER_CONFIG_ID = "moira-console-idp";
const BASE_URL = "https://local-llm.example.test/v1";
const MODEL_KEY = "Qwen3-4B";

/** Unmistakable, and asserted absent from every response body. */
const OPERATOR_KEY = "operator-supplied-provider-key-9f31ab77";

const PROVIDER_GET = `GET /api/v1/admin/providers/${PROVIDER_ID}`;
const PROVIDER_LIST = "GET /api/v1/admin/providers";
const PROVIDER_CREATE = "POST /api/v1/admin/providers";
const PROVIDER_PATCH_ROUTE = `PATCH /api/v1/admin/providers/${PROVIDER_ID}`;
const PROVIDER_DISABLE = `POST /api/v1/admin/providers/${PROVIDER_ID}/disable`;
const MODEL_LIST = `GET /api/v1/admin/providers/${PROVIDER_ID}/models`;
const MODEL_CREATE = `POST /api/v1/admin/providers/${PROVIDER_ID}/models`;
const MODEL_DISABLE = `POST /api/v1/admin/provider-models/${MODEL_ID}/disable`;
const CREDENTIAL_LIST = "GET /api/v1/admin/provider-credentials";
const CREDENTIAL_CREATE = "POST /api/v1/admin/provider-credentials";
const CREDENTIAL_DISABLE = `POST /api/v1/admin/provider-credentials/${CREDENTIAL_ID}/disable`;
const ROUTE_LIST = "GET /api/v1/admin/routes";
const POLICY_LIST = "GET /api/v1/admin/routing-policies";
const POLICY_CREATE = "POST /api/v1/admin/routing-policies";
const POLICY_DISABLE = `POST /api/v1/admin/routing-policies/${POLICY_ID}/disable`;

const EMPTY_PAGE = { data: [], pagination: { has_more: false, next_cursor: null } };

function page(rows: readonly unknown[]) {
  return { data: rows, pagination: { has_more: false, next_cursor: null } };
}

function providerRecord(overrides: Record<string, unknown> = {}) {
  return {
    id: PROVIDER_ID,
    provider_type: "open_ai_compatible",
    display_name: "Local endpoint",
    status: "active",
    metadata: {},
    created_at: "2026-08-04T00:00:00Z",
    updated_at: "2026-08-04T00:00:00Z",
    version: 7,
    base_url: BASE_URL,
    ...overrides,
  };
}

function modelRecord(overrides: Record<string, unknown> = {}) {
  return {
    id: MODEL_ID,
    provider_id: PROVIDER_ID,
    model_key: MODEL_KEY,
    capabilities: { text: true, streaming: true, tools: true },
    status: "active",
    created_at: "2026-08-04T00:00:00Z",
    updated_at: "2026-08-04T00:00:00Z",
    version: 3,
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
    masked_secret: "sk-****mask",
    status: "active",
    priority: 0,
    metadata: {},
    created_at: "2026-08-04T00:00:00Z",
    updated_at: "2026-08-04T00:00:00Z",
    version: 4,
    ...overrides,
  };
}

function routeRecord(overrides: Record<string, unknown> = {}) {
  return {
    id: ROUTE_ID,
    route_key: "general",
    display_name: "General",
    status: "active",
    selection_strategy: "default",
    metadata: {},
    created_at: "2026-08-04T00:00:00Z",
    updated_at: "2026-08-04T00:00:00Z",
    version: 1,
    ...overrides,
  };
}

function policyRecord(overrides: Record<string, unknown> = {}) {
  return {
    id: POLICY_ID,
    route_id: ROUTE_ID,
    provider_id: PROVIDER_ID,
    provider_model_id: MODEL_ID,
    priority: 100,
    weight: 1,
    cost_weight: 1,
    latency_weight: 1,
    quality_weight: 1,
    required_capabilities: [],
    retry_policy: null,
    status: "active",
    metadata: {},
    created_at: "2026-08-04T00:00:00Z",
    updated_at: "2026-08-04T00:00:00Z",
    version: 5,
    ...overrides,
  };
}

function handlers(overrides: Record<string, StubHandler> = {}): Record<string, StubHandler> {
  return {
    [PROVIDER_GET]: () => ({ status: 200, body: providerRecord() }),
    [PROVIDER_LIST]: () => ({ status: 200, body: EMPTY_PAGE }),
    [PROVIDER_CREATE]: () => ({ status: 201, body: providerRecord() }),
    [PROVIDER_PATCH_ROUTE]: () => ({ status: 200, body: providerRecord({ version: 8 }) }),
    [PROVIDER_DISABLE]: () => ({ status: 200, body: providerRecord({ status: "disabled" }) }),
    [MODEL_LIST]: () => ({ status: 200, body: page([modelRecord()]) }),
    [MODEL_CREATE]: () => ({ status: 201, body: modelRecord() }),
    [MODEL_DISABLE]: () => ({ status: 200, body: modelRecord({ status: "disabled" }) }),
    [CREDENTIAL_LIST]: () => ({ status: 200, body: page([credentialRecord()]) }),
    [CREDENTIAL_CREATE]: () => ({ status: 201, body: credentialRecord() }),
    [CREDENTIAL_DISABLE]: () => ({ status: 200, body: credentialRecord({ status: "disabled" }) }),
    [ROUTE_LIST]: () => ({ status: 200, body: page([routeRecord()]) }),
    [POLICY_LIST]: () => ({ status: 200, body: page([policyRecord()]) }),
    [POLICY_CREATE]: () => ({ status: 201, body: policyRecord() }),
    [POLICY_DISABLE]: () => ({ status: 200, body: policyRecord({ status: "disabled" }) }),
    ...overrides,
  };
}

/* -------------------------------------------------------------------------- */
/* The session the gate will actually check                                   */
/* -------------------------------------------------------------------------- */

/** Signed in, verified, inside the allow-list, with a linked account. */
const ADMITTED = {
  session: { id: "session-1", providerId: PROVIDER_CONFIG_ID },
  user: { id: "user-1", email: "ops@example.com", emailVerified: true },
};

const CONFIG = {
  providerId: PROVIDER_CONFIG_ID,
  allowedEmailDomains: ["example.com"],
} as unknown as ResolvedAuthConfig;

/**
 * A Better Auth stand-in.
 *
 * Only `getSession` and `$context` are supplied, because those are the only two
 * things `consoleSessionCheck` reaches for — and everything it does WITH them is
 * the shipped implementation. `session: null` therefore produces a genuine
 * "no session" refusal rather than a fabricated one.
 */
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

function request(method: string, body?: unknown): Request {
  return new Request("https://console.example.com/api/llm/anything", {
    method,
    ...(body === undefined
      ? {}
      : { headers: { "content-type": "application/json" }, body: JSON.stringify(body) }),
  });
}

function params<T extends Record<string, string>>(value: T): { params: Promise<T> } {
  return { params: Promise.resolve(value) };
}

async function json(response: Response): Promise<Record<string, unknown>> {
  return (await response.json()) as Record<string, unknown>;
}

function errorOf(body: Record<string, unknown>): Record<string, unknown> {
  return body["error"] as Record<string, unknown>;
}

/* -------------------------------------------------------------------------- */
/* AC3 — every handler is behind the gate                                     */
/* -------------------------------------------------------------------------- */

/** Every exported handler, with the arguments Next would give it. */
const EVERY_HANDLER: ReadonlyArray<{ name: string; call: () => Promise<Response> }> = [
  { name: "GET /api/llm/providers", call: () => PROVIDERS_GET(request("GET")) },
  {
    name: "POST /api/llm/providers",
    call: () => PROVIDERS_POST(request("POST", { display_name: "x", base_url: BASE_URL })),
  },
  {
    name: "PATCH /api/llm/providers/[id]",
    call: () => PROVIDER_PATCH(request("PATCH", { display_name: "x" }), params({ id: PROVIDER_ID })),
  },
  {
    name: "DELETE /api/llm/providers/[id]",
    call: () => PROVIDER_DELETE(request("DELETE"), params({ id: PROVIDER_ID })),
  },
  {
    name: "POST /api/llm/providers/[id]/models",
    call: () => MODEL_POST(request("POST", { model_key: MODEL_KEY }), params({ id: PROVIDER_ID })),
  },
  {
    name: "DELETE /api/llm/providers/[id]/models/[modelId]",
    call: () => MODEL_DELETE(request("DELETE"), params({ id: PROVIDER_ID, modelId: MODEL_ID })),
  },
  {
    name: "POST /api/llm/providers/[id]/credentials",
    call: () => CREDENTIAL_POST(request("POST", { api_key: "" }), params({ id: PROVIDER_ID })),
  },
  {
    name: "DELETE /api/llm/providers/[id]/credentials/[credentialId]",
    call: () =>
      CREDENTIAL_DELETE(request("DELETE"), params({ id: PROVIDER_ID, credentialId: CREDENTIAL_ID })),
  },
  {
    name: "POST /api/llm/providers/[id]/routing",
    call: () =>
      ROUTING_POST(request("POST", { provider_model_id: MODEL_ID }), params({ id: PROVIDER_ID })),
  },
  {
    name: "DELETE /api/llm/providers/[id]/routing/[policyId]",
    call: () =>
      ROUTING_DELETE(request("DELETE"), params({ id: PROVIDER_ID, policyId: POLICY_ID })),
  },
  {
    name: "POST /api/llm/connect-vllm",
    call: () => CONNECT(request("POST", { action: "discover", base_url: BASE_URL })),
  },
];

describe("AC3 — nothing on this surface is reachable without a console session", () => {
  test("the handler list is the whole surface, so the loop below is not scanning air", () => {
    expect(EVERY_HANDLER.length, "eleven exported handlers under app/api/llm/**").toBe(11);
  });

  for (const handler of EVERY_HANDLER) {
    test(`${handler.name} answers 401 and writes NOTHING with no session`, async () => {
      install({ session: null });
      const response = await handler.call();
      expect(response.status).toBe(401);
      expect(errorOf(await json(response))["code"]).toBe("no_session");
      // THE ASSERTION THAT MAKES THE MUTATION VISIBLE: not merely refused —
      // refused before a single Moira request left the process.
      expect(stub.routes()).toEqual([]);
    });
  }

  test("a session that may not act is a 403, not a 401", async () => {
    // Same cookie, different verdict: the address is outside the allow-list, so
    // `checkSession` refuses. If the gate were deleted this would be a 200.
    install({
      session: {
        session: { id: "session-2", providerId: PROVIDER_CONFIG_ID },
        user: { id: "user-2", email: "outsider@elsewhere.test", emailVerified: true },
      },
    });
    const response = await PROVIDERS_GET(request("GET"));
    expect(response.status).toBe(403);
    expect(errorOf(await json(response))["code"]).toBe("email_domain_not_allowed");
    expect(stub.routes()).toEqual([]);
  });

  test("an unverified address is a 403 as well", async () => {
    install({
      session: {
        session: { id: "session-3", providerId: PROVIDER_CONFIG_ID },
        user: { id: "user-3", email: "ops@example.com", emailVerified: false },
      },
    });
    expect((await PROVIDERS_GET(request("GET"))).status).toBe(403);
  });

  test("an unresolvable runtime is a 503 — a backend outage is not a sign-out", async () => {
    install({
      runtime: {
        ok: false,
        resolution: {
          ok: false,
          problem: "no_enabled_provider",
          messageKey: CONSOLE_MESSAGE_KEYS.auth_config_unavailable,
          cacheKey: "unavailable",
        },
      },
    });
    const response = await PROVIDERS_GET(request("GET"));
    expect(response.status).toBe(503);
    expect(stub.routes()).toEqual([]);
  });

  test("no response on this surface is cacheable", async () => {
    // The discovery probe is stubbed for the whole loop. Without it the shortcut
    // handler would make a REAL outbound connection to the fixture address —
    // harmless here, but a unit suite that opens sockets is one that behaves
    // differently on a machine with a wildcard DNS resolver.
    const offline = (async () => {
      throw new TypeError("fetch failed");
    }) as unknown as typeof fetch;

    await withDiscovery(offline, async () => {
      for (const handler of EVERY_HANDLER) {
        install();
        const response = await handler.call();
        expect(response.headers.get("cache-control"), handler.name).toBe("no-store");
      }
    });
  });
});

/* -------------------------------------------------------------------------- */
/* Reading                                                                    */
/* -------------------------------------------------------------------------- */

describe("GET /api/llm/providers", () => {
  test("it publishes the projection and never a credential's mask or fingerprint", async () => {
    install({ handlers: handlers({ [PROVIDER_LIST]: () => ({ status: 200, body: page([providerRecord()]) }) }) });
    const response = await PROVIDERS_GET(request("GET"));
    expect(response.status).toBe(200);
    const serialised = JSON.stringify(await json(response));
    expect(serialised).not.toContain("sha256:fingerprint-that-must-not-cross");
    expect(serialised).not.toContain("sk-****mask");
    expect(serialised).toContain(PROVIDER_ID);
  });
});

/* -------------------------------------------------------------------------- */
/* Writing — every id is resolved server-side                                 */
/* -------------------------------------------------------------------------- */

describe("a privileged write never lets the request choose its target", () => {
  test("the provider is READ before it is patched, and the version comes from that read", async () => {
    const response = await PROVIDER_PATCH(
      request("PATCH", { base_url: "https://elsewhere.example.test" }),
      params({ id: PROVIDER_ID }),
    );
    expect(response.status).toBe(200);
    expect(stub.routes()).toEqual([PROVIDER_GET, PROVIDER_PATCH_ROUTE]);
    // 7 is the version the READ returned. A fabricated one defeats the only
    // thing stopping this from racing a concurrent write.
    expect(stub.requestsFor(PROVIDER_PATCH_ROUTE)[0]?.headers["If-Match"]).toBe("7");
    // Canonicalised, so what is stored is what a probe would have reached.
    expect((stub.bodyOf(PROVIDER_PATCH_ROUTE) as Record<string, unknown>)["base_url"]).toBe(
      "https://elsewhere.example.test/v1",
    );
    // `provider_type` is immutable and is never sent.
    expect("provider_type" in (stub.bodyOf(PROVIDER_PATCH_ROUTE) as object)).toBe(false);
  });

  test("a model that belongs to another provider is refused with the console's own 404", async () => {
    // Moira's disable is FLAT — it would act on this id happily. The nested path
    // is what makes the ownership check possible, and this is the check.
    const response = await MODEL_DELETE(
      request("DELETE"),
      params({ id: PROVIDER_ID, modelId: "a-model-of-someone-elses" }),
    );
    expect(response.status).toBe(404);
    expect(errorOf(await json(response))["message_key"]).toBe(
      CONSOLE_MESSAGE_KEYS.llm_model_not_found,
    );
    expect(stub.routes()).not.toContain(MODEL_DISABLE);
  });

  test("a credential row that belongs to another provider is refused the same way", async () => {
    install({
      handlers: handlers({
        [CREDENTIAL_LIST]: () => ({
          status: 200,
          body: page([credentialRecord({ provider_id: OTHER_PROVIDER_ID })]),
        }),
      }),
    });
    const response = await CREDENTIAL_DELETE(
      request("DELETE"),
      params({ id: PROVIDER_ID, credentialId: CREDENTIAL_ID }),
    );
    expect(response.status).toBe(404);
    expect(errorOf(await json(response))["message_key"]).toBe(
      CONSOLE_MESSAGE_KEYS.llm_key_row_not_found,
    );
    expect(stub.routes()).not.toContain(CREDENTIAL_DISABLE);
  });

  test("a routing policy that points at another provider is refused", async () => {
    install({
      handlers: handlers({
        [POLICY_LIST]: () => ({
          status: 200,
          body: page([policyRecord({ provider_id: OTHER_PROVIDER_ID })]),
        }),
      }),
    });
    const response = await ROUTING_DELETE(
      request("DELETE"),
      params({ id: PROVIDER_ID, policyId: POLICY_ID }),
    );
    expect(response.status).toBe(404);
    expect(errorOf(await json(response))["message_key"]).toBe(
      CONSOLE_MESSAGE_KEYS.llm_policy_not_found,
    );
    expect(stub.routes()).not.toContain(POLICY_DISABLE);
  });

  test("disable really does disable, with the version it read", async () => {
    const response = await PROVIDER_DELETE(request("DELETE"), params({ id: PROVIDER_ID }));
    expect(response.status).toBe(200);
    expect(stub.routes()).toEqual([PROVIDER_GET, PROVIDER_DISABLE]);
    expect(stub.requestsFor(PROVIDER_DISABLE)[0]?.headers["If-Match"]).toBe("7");
  });
});

describe("POST /api/llm/providers/[id]/routing", () => {
  test("the route is resolved from `route_key` and is never taken from the body", async () => {
    install({ handlers: handlers({ [POLICY_LIST]: () => ({ status: 200, body: EMPTY_PAGE }) }) });
    const response = await ROUTING_POST(
      // A body that TRIES to choose the route. It must have no effect.
      request("POST", { provider_model_id: MODEL_ID, route_id: "a-route-nobody-showed-me" }),
      params({ id: PROVIDER_ID }),
    );
    expect(response.status).toBe(201);
    const body = stub.bodyOf(POLICY_CREATE) as Record<string, unknown>;
    expect(body["route_id"]).toBe(ROUTE_ID);
    expect(body["provider_id"]).toBe(PROVIDER_ID);
    expect(body["provider_model_id"]).toBe(MODEL_ID);
    expect(JSON.stringify(stub.requests)).not.toContain("a-route-nobody-showed-me");
    expect(stub.routes()).not.toContain("POST /api/v1/admin/routes");
  });

  test("an existing identical policy is returned rather than duplicated", async () => {
    // `POST /routing-policies` documents no 409, so a second click would land a
    // second eligible row with nothing to catch it.
    const response = await ROUTING_POST(
      request("POST", { provider_model_id: MODEL_ID }),
      params({ id: PROVIDER_ID }),
    );
    expect(response.status).toBe(200);
    expect((await json(response))["id"]).toBe(POLICY_ID);
    expect(stub.routes()).not.toContain(POLICY_CREATE);
  });

  test("a model the provider does not own cannot be routed to", async () => {
    const response = await ROUTING_POST(
      request("POST", { provider_model_id: "someone-elses-model" }),
      params({ id: PROVIDER_ID }),
    );
    expect(response.status).toBe(404);
    expect(stub.routes()).not.toContain(POLICY_CREATE);
  });

  test("a missing `general` route is a keyed refusal, and no route is created", async () => {
    install({ handlers: handlers({ [ROUTE_LIST]: () => ({ status: 200, body: EMPTY_PAGE }) }) });
    const response = await ROUTING_POST(
      request("POST", { provider_model_id: MODEL_ID }),
      params({ id: PROVIDER_ID }),
    );
    expect(response.status).toBe(400);
    expect(errorOf(await json(response))["message_key"]).toBe(
      CONSOLE_MESSAGE_KEYS.llm_general_route_missing,
    );
    expect(stub.routes()).not.toContain("POST /api/v1/admin/routes");
  });
});

/* -------------------------------------------------------------------------- */
/* The credential handler — the secret goes one way                           */
/* -------------------------------------------------------------------------- */

describe("POST /api/llm/providers/[id]/credentials", () => {
  test("a blank field becomes a SERVER-GENERATED placeholder, never an empty key", async () => {
    const response = await CREDENTIAL_POST(request("POST", { api_key: "" }), params({ id: PROVIDER_ID }));
    expect(response.status).toBe(201);
    const sent = stub.bodyOf(CREDENTIAL_CREATE) as Record<string, unknown>;
    const secret = sent["secret"] as Record<string, unknown>;
    // Exactly one key, and it is non-empty. `endpoint`, even as null, makes the
    // untagged union ambiguous and the request is refused naming no field.
    expect(Object.keys(secret)).toEqual(["api_key"]);
    expect(String(secret["api_key"]).length).toBeGreaterThan(0);
    expect(sent["provider_id"]).toBe(PROVIDER_ID);
  });

  test("AC2 — the credential body never carries an `endpoint` key", async () => {
    await CREDENTIAL_POST(request("POST", { api_key: OPERATOR_KEY }), params({ id: PROVIDER_ID }));
    const secret = (stub.bodyOf(CREDENTIAL_CREATE) as Record<string, unknown>)["secret"];
    expect("endpoint" in (secret as object)).toBe(false);
  });

  test("the operator's key reaches Moira and NOTHING else", async () => {
    const response = await CREDENTIAL_POST(
      request("POST", { api_key: OPERATOR_KEY }),
      params({ id: PROVIDER_ID }),
    );
    // It went to Moira exactly once.
    expect(JSON.stringify(stub.bodyOf(CREDENTIAL_CREATE))).toContain(OPERATOR_KEY);
    // And came back in nothing. Nor did Moira's own mask or fingerprint.
    const body = JSON.stringify(await json(response));
    expect(body).not.toContain(OPERATOR_KEY);
    expect(body).not.toContain("sha256:fingerprint-that-must-not-cross");
    expect(body).not.toContain("sk-****mask");
    expect(await json(await CREDENTIAL_POST(request("POST", { api_key: "" }), params({ id: PROVIDER_ID })))).toEqual({
      id: CREDENTIAL_ID,
      kind: "api_key",
      status: "active",
      version: 4,
    });
  });
});

/* -------------------------------------------------------------------------- */
/* AC1 — the shortcut                                                         */
/* -------------------------------------------------------------------------- */

/** Swap the global `fetch` for the discovery probe only. */
function withDiscovery<T>(impl: typeof fetch, body: () => Promise<T>): Promise<T> {
  const original = globalThis.fetch;
  globalThis.fetch = impl;
  return body().finally(() => {
    globalThis.fetch = original;
  });
}

describe("POST /api/llm/connect-vllm", () => {
  test("discover asks the endpoint from the SERVER and offers what it advertised", async () => {
    const seen: string[] = [];
    const probe = (async (url: RequestInfo | URL) => {
      seen.push(String(url));
      return new Response(JSON.stringify({ data: [{ id: MODEL_KEY }, { id: "other" }] }), {
        status: 200,
      });
    }) as unknown as typeof fetch;

    const response = await withDiscovery(probe, () =>
      CONNECT(request("POST", { action: "discover", base_url: "https://local-llm.example.test" })),
    );
    expect(response.status).toBe(200);
    expect(await json(response)).toEqual({ base_url: BASE_URL, models: [MODEL_KEY, "other"] });
    expect(seen).toEqual([`${BASE_URL}/models`]);
    // Discovery writes nothing.
    expect(stub.routes()).toEqual([]);
  });

  test("AN UNREACHABLE ENDPOINT IS A KEYED 502, NOT A CRASH", async () => {
    const dead = (async () => {
      throw new TypeError("fetch failed");
    }) as unknown as typeof fetch;
    const response = await withDiscovery(dead, () =>
      CONNECT(request("POST", { action: "discover", base_url: BASE_URL })),
    );
    expect(response.status).toBe(502);
    expect(errorOf(await json(response))["message_key"]).toBe(
      CONSOLE_MESSAGE_KEYS.llm_discovery_unreachable,
    );
  });

  test("A RESPONSE THAT IS NOT A MODEL LISTING IS NEVER OFFERED", async () => {
    // THE MUTATION THIS BUYS: delete the validation from `discoverModels` and
    // this goes red, because the object below is then handed straight on.
    const hostile = (async () =>
      new Response(JSON.stringify({ data: [{ id: { nested: "not a string" } }] }), {
        status: 200,
      })) as unknown as typeof fetch;
    const response = await withDiscovery(hostile, () =>
      CONNECT(request("POST", { action: "discover", base_url: BASE_URL })),
    );
    expect(response.status).toBe(502);
    const body = await json(response);
    expect(errorOf(body)["message_key"]).toBe(CONSOLE_MESSAGE_KEYS.llm_discovery_invalid_response);
    // Nothing from the unvalidated response is echoed.
    expect(JSON.stringify(body)).not.toContain("not a string");
  });

  test("connect runs the chain in the seed script's order", async () => {
    install({
      handlers: handlers({
        [MODEL_LIST]: () => ({ status: 200, body: EMPTY_PAGE }),
        [CREDENTIAL_LIST]: () => ({ status: 200, body: EMPTY_PAGE }),
        [POLICY_LIST]: () => ({ status: 200, body: EMPTY_PAGE }),
      }),
    });
    const response = await CONNECT(
      request("POST", { action: "connect", base_url: BASE_URL, model_keys: [MODEL_KEY] }),
    );
    expect(response.status).toBe(201);
    expect(stub.routes()).toEqual([
      PROVIDER_LIST,
      PROVIDER_CREATE,
      MODEL_LIST,
      MODEL_CREATE,
      CREDENTIAL_LIST,
      CREDENTIAL_CREATE,
      ROUTE_LIST,
      POLICY_LIST,
      POLICY_CREATE,
    ]);
    const body = await json(response);
    expect(body["provider_id"]).toBe(PROVIDER_ID);
    expect((body["trace"] as unknown[]).length).toBe(5);
  });

  test("a chain that fails at step three reports WHAT WAS ALREADY WRITTEN", async () => {
    install({
      handlers: handlers({
        [MODEL_LIST]: () => ({ status: 200, body: EMPTY_PAGE }),
        [CREDENTIAL_LIST]: () => ({ status: 200, body: EMPTY_PAGE }),
        [CREDENTIAL_CREATE]: () => ({ status: 503, body: errorEnvelope("database_unavailable") }),
      }),
    });
    const response = await CONNECT(
      request("POST", { action: "connect", base_url: BASE_URL, model_keys: [MODEL_KEY] }),
    );
    expect(response.status).toBe(409);
    const error = errorOf(await json(response));
    expect(error["step"]).toBe("provider_credential");
    expect(error["message_key"]).toBe(CONSOLE_MESSAGE_KEYS.llm_connect_step_failed);
    // The provider and the model exist. Saying so is what turns a second click
    // into an informed decision.
    expect((error["state"] as Record<string, unknown>)["providerId"]).toBe(PROVIDER_ID);
    expect((error["state"] as Record<string, unknown>)["providerModelIds"]).toEqual([MODEL_ID]);
    expect((error["trace"] as unknown[]).length).toBe(2);
    // Moira's `request_id` and `details` do not cross the boundary.
    const serialised = JSON.stringify(error);
    expect(serialised).not.toContain("req_stub_0001");
    expect(serialised).not.toContain("must not cross the boundary");
  });

  test("a missing `general` route stops the chain with an actionable key", async () => {
    install({
      handlers: handlers({
        [MODEL_LIST]: () => ({ status: 200, body: EMPTY_PAGE }),
        [CREDENTIAL_LIST]: () => ({ status: 200, body: EMPTY_PAGE }),
        [ROUTE_LIST]: () => ({ status: 200, body: EMPTY_PAGE }),
      }),
    });
    const response = await CONNECT(
      request("POST", { action: "connect", base_url: BASE_URL, model_keys: [MODEL_KEY] }),
    );
    expect(response.status).toBe(409);
    expect(errorOf(await json(response))["message_key"]).toBe(
      CONSOLE_MESSAGE_KEYS.llm_general_route_missing,
    );
  });

  test("connect with no model selected is refused before anything is written", async () => {
    const response = await CONNECT(
      request("POST", { action: "connect", base_url: BASE_URL, model_keys: [] }),
    );
    expect(response.status).toBe(400);
    expect(errorOf(await json(response))["message_key"]).toBe(
      CONSOLE_MESSAGE_KEYS.llm_model_required,
    );
    expect(stub.routes()).toEqual([]);
  });

  test("an unknown action and an unreadable body are keyed 400s that write nothing", async () => {
    expect(
      errorOf(await json(await CONNECT(request("POST", { action: "delete-everything" }))))[
        "message_key"
      ],
    ).toBe(CONSOLE_MESSAGE_KEYS.llm_action_unknown);

    const malformed = new Request("https://console.example.com/api/llm/connect-vllm", {
      method: "POST",
      body: "not json",
    });
    expect((await CONNECT(malformed)).status).toBe(400);
    expect(stub.routes()).toEqual([]);
  });
});

/* -------------------------------------------------------------------------- */
/* Input the console refuses before any request leaves                        */
/* -------------------------------------------------------------------------- */

describe("provider input is validated before the first Moira write", () => {
  const refusals: ReadonlyArray<{ what: string; body: unknown; key: string }> = [
    {
      what: "a blank display name",
      body: { display_name: "  ", base_url: BASE_URL },
      key: CONSOLE_MESSAGE_KEYS.llm_display_name_required,
    },
    {
      what: "a missing endpoint",
      body: { display_name: "x" },
      key: CONSOLE_MESSAGE_KEYS.llm_base_url_required,
    },
    {
      what: "an endpoint carrying sign-in details",
      body: { display_name: "x", base_url: "https://user:pass@local-llm.example.test" },
      key: CONSOLE_MESSAGE_KEYS.llm_base_url_userinfo_rejected,
    },
    {
      what: "a scheme the console will not fetch",
      body: { display_name: "x", base_url: "file:///etc/passwd" },
      key: CONSOLE_MESSAGE_KEYS.llm_base_url_scheme_unsupported,
    },
  ];

  for (const refusal of refusals) {
    test(`${refusal.what} is a keyed 400 with nothing written`, async () => {
      install();
      const response = await PROVIDERS_POST(request("POST", refusal.body));
      expect(response.status).toBe(400);
      expect(errorOf(await json(response))["message_key"]).toBe(refusal.key);
      expect(stub.routes()).toEqual([]);
    });
  }

  test("a created provider always carries an explicit type and a canonical endpoint", async () => {
    install();
    const response = await PROVIDERS_POST(
      request("POST", { display_name: "Local endpoint", base_url: "https://local-llm.example.test" }),
    );
    expect(response.status).toBe(201);
    expect(stub.bodyOf(PROVIDER_CREATE)).toEqual({
      provider_type: "open_ai_compatible",
      display_name: "Local endpoint",
      base_url: BASE_URL,
    });
  });

  test("a model create sends `capabilities` explicitly", async () => {
    install();
    await MODEL_POST(request("POST", { model_key: MODEL_KEY }), params({ id: PROVIDER_ID }));
    expect((stub.bodyOf(MODEL_CREATE) as Record<string, unknown>)["capabilities"]).toEqual({
      text: true,
      streaming: true,
      tools: true,
    });
  });

  test("a blank model identifier is refused before the provider is even read", async () => {
    install();
    const response = await MODEL_POST(request("POST", { model_key: " " }), params({ id: PROVIDER_ID }));
    expect(response.status).toBe(400);
    expect(errorOf(await json(response))["message_key"]).toBe(
      CONSOLE_MESSAGE_KEYS.llm_model_key_required,
    );
    expect(stub.routes()).toEqual([]);
  });
});
